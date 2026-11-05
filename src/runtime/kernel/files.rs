// File I/O. Guest paths look like "game:\\zone\\common.ff" or "\\Device\\Cdrom0\\...",
// and the console filesystem is case-insensitive, so paths are mapped onto the game root
// and resolved case-insensitively.
//
// The game root is the disc and is read-only. The one writable place is a mounted
// content package -- "save0:" and friends, put there by the content layer in content.rs
// -- so a path whose device names a mount resolves under that mount instead, and may be
// created, written, truncated and deleted.

use super::misc::{STATUS_END_OF_FILE, STATUS_OBJECT_NAME_NOT_FOUND};
use super::objects::{self, Kind, Object};
use super::{STATUS_INVALID_PARAMETER, STATUS_SUCCESS, queue_user_apc, stack_argument};
use crate::env;
use crate::guest::{Memory, Register, kernel_import};
use crate::log::{c_string, log_info, log_kernel};
use crate::runtime::platform;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};

const STATUS_UNSUCCESSFUL: u32 = 0xC000_0001;
const STATUS_NOT_IMPLEMENTED: u32 = 0xC000_0002;
const STATUS_OBJECT_NAME_COLLISION: u32 = 0xC000_0035;
const STATUS_PENDING: u32 = 0x0000_0103;

static GAME_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// The disc, which every device not mounted names.
pub fn set_game_root(root: PathBuf) {
    let _ = GAME_ROOT.set(root);
}

pub fn game_root() -> PathBuf {
    GAME_ROOT.get().cloned().unwrap_or_default()
}

struct File {
    path: PathBuf,
    is_directory: bool,
    writable: bool,
    open: Mutex<Open>,
}

#[derive(Default)]
struct Open {
    handle: Option<fs::File>,
    size: u64,
    delete_on_close: bool,
}

impl File {
    fn open(&self) -> std::sync::MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn name(&self) -> std::ffi::CString {
        c_string(self.path.file_name().unwrap_or_default().as_encoded_bytes())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let open = self.open.get_mut().unwrap_or_else(PoisonError::into_inner);
        drop(open.handle.take());
        // NT has no delete call of its own: a caller marks the file and the deletion
        // happens when the last handle goes.
        if open.delete_on_close {
            let _ = if self.path.is_dir() { fs::remove_dir_all(&self.path) } else { fs::remove_file(&self.path) };
        }
    }
}

// Positions past 2 GB, and a negative one refused as fseeko refuses it.
fn seek(handle: &mut fs::File, offset: i64) {
    if offset >= 0 {
        let _ = handle.seek(SeekFrom::Start(offset as u64));
    }
}

fn tell(handle: &mut fs::File) -> u64 {
    handle.stream_position().unwrap_or(u64::MAX)
}

// As fread does: as much as there is, up to `buffer`'s length.
fn read_fully(handle: &mut fs::File, buffer: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buffer.len() {
        match handle.read(&mut buffer[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    got
}

fn write_fully(handle: &mut fs::File, bytes: &[u8]) -> usize {
    let mut put = 0;
    while put < bytes.len() {
        match handle.write(&bytes[put..]) {
            Ok(0) => break,
            Ok(n) => put += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    put
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(u64::MAX, |metadata| metadata.len())
}

// OBJECT_ATTRIBUTES { rootDirectory, name, attributes } naming an ANSI_STRING { length,
// maximum length, pointer }.
fn object_name(memory: Memory, attributes: u32) -> Vec<u8> {
    if attributes == 0 {
        return Vec::new();
    }
    let string = memory.u32_at(attributes + 4);
    if string == 0 {
        return Vec::new();
    }
    let (pointer, length) = (memory.u32_at(string + 4), memory.u16_at(string));
    if pointer == 0 || length == 0 {
        return Vec::new();
    }
    memory.bytes(pointer, usize::from(length))
}

// A path below a device, as the object manager reads it (Xenia's canonicalize_path):
// "." names the folder it is in, and ".." takes back the name before it, so no path
// climbs above its device's root, here the game folder or a save package. A guest path
// can carry text from the network -- the map a host picks names a fastfile -- so this is
// what keeps it inside. Leading and doubled separators go too.
fn canonical(path: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for component in path.split(|&byte| byte == b'\\' || byte == b'/') {
        match component {
            b".." => {
                parts.pop();
            }
            b"" | b"." => {}
            name => parts.push(name),
        }
    }
    parts.join(&b'\\')
}

fn components(relative: &[u8]) -> impl Iterator<Item = &[u8]> {
    relative.split(|&byte| byte == b'\\' || byte == b'/')
}

fn host_name(component: &[u8]) -> PathBuf {
    platform::path(&c_string(component))
}

// Whether the host reads a component as one name below the folder it is joined to. On
// Windows a drive ("C:") replaces the whole path it is appended to, and a guest name cut
// at a NUL ("..\0x") can come out as ".." after canonical() let it through.
fn is_host_name(component: &Path) -> bool {
    let mut parts = component.components();
    matches!((parts.next(), parts.next()), (Some(Component::Normal(_)), None))
}

// One path component at a time.
fn resolve_insensitive(root: &Path, relative: &[u8]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for name in components(relative) {
        if name.is_empty() || name == b"." {
            continue;
        }
        let host = host_name(name);
        if !is_host_name(&host) {
            return None;
        }
        let direct = current.join(&host);
        if direct.exists() {
            current = direct;
            continue;
        }
        if !current.is_dir() {
            return None;
        }
        let found = fs::read_dir(&current).ok()?.flatten().map(|entry| entry.path()).find(|have| {
            let have = have.file_name().unwrap_or_default().as_encoded_bytes();
            have.len() == name.len() && have.eq_ignore_ascii_case(name)
        });
        current = found?;
    }
    Some(current)
}

// "game:\\zone\\common.ff"        -> device "game",   path "zone\\common.ff"
// "\\Device\\Cdrom0\\zone\\x.ff" -> device "cdrom0", path "zone\\x.ff"
fn split_device(guest_path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut device = Vec::new();
    let mut rest = guest_path;
    if let Some(colon) = guest_path.iter().position(|&byte| byte == b':') {
        device = guest_path[..colon].to_ascii_lowercase();
        rest = &guest_path[colon + 1..];
    } else if guest_path.starts_with(b"\\Device\\") || guest_path.starts_with(b"\\device\\") {
        let slash = guest_path[8..].iter().position(|&byte| byte == b'\\').map(|at| at + 8);
        device = guest_path[8..slash.unwrap_or(guest_path.len())].to_ascii_lowercase();
        rest = slash.map_or(&b""[..], |slash| &guest_path[slash + 1..]);
    }
    (device, canonical(rest))
}

// The mount table the content layer fills in. Everything not in it is the disc, which is
// read-only.
static MOUNTS: Mutex<BTreeMap<Vec<u8>, PathBuf>> = Mutex::new(BTreeMap::new());

pub fn mount(name: &[u8], root: PathBuf) {
    MOUNTS.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_ascii_lowercase(), root);
}

pub fn unmount(name: &[u8]) {
    MOUNTS.lock().unwrap_or_else(PoisonError::into_inner).remove(&name.to_ascii_lowercase());
}

// The device's root, and whether it may be written.
fn device_root(device: &[u8]) -> (PathBuf, bool) {
    match MOUNTS.lock().unwrap_or_else(PoisonError::into_inner).get(device) {
        Some(root) => (root.clone(), true),
        None => (game_root(), false),
    }
}

// Where a name that is not there yet would go. Only ever used under a mount, so there is
// no case-insensitive match to make: this name is the one being created. None when a
// component is not a name the host can create.
fn host_path(root: &Path, relative: &[u8]) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for component in components(relative).filter(|component| !component.is_empty()) {
        let host = host_name(component);
        if !is_host_name(&host) {
            return None;
        }
        out.push(host);
    }
    Some(out)
}

static MISSING: Mutex<BTreeMap<Vec<u8>, u64>> = Mutex::new(BTreeMap::new());

fn note_missing(guest_path: &[u8]) {
    if !cfg!(feature = "diagnostics") {
        return;
    }
    // An empty name means the open carried no ANSI_STRING at all, which is a different
    // bug from a genuinely absent file.
    let key = if guest_path.is_empty() { &b"<no name in OBJECT_ATTRIBUTES>"[..] } else { guest_path };
    let mut missing = MISSING.lock().unwrap_or_else(PoisonError::into_inner);
    let count = missing.entry(key.to_vec()).or_insert(0);
    *count += 1;
    if *count == 1 {
        log_kernel!("file not found: %s", c_string(key).as_c_str());
    }
}

/// The end-of-run report of them.
pub fn report_missing_files() {
    if !cfg!(feature = "diagnostics") {
        return;
    }
    let missing = MISSING.lock().unwrap_or_else(PoisonError::into_inner);
    if missing.is_empty() {
        return;
    }
    log_info!("files the guest asked for and did not get (%llu distinct):", missing.len() as u64);
    for (n, (path, count)) in (1..).zip(missing.iter()) {
        log_info!("  %8llu  %s", *count, c_string(path.as_slice()).as_c_str());
        if n >= 40 {
            log_info!("  ... and %llu more", (missing.len() - n) as u64);
            break;
        }
    }
}

fn trace_files() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| cfg!(feature = "diagnostics") && env::flag(c"MW2_TRACE_FILES"));
    *ON
}

fn path_text(path: &Path) -> std::ffi::CString {
    c_string(path.as_os_str().as_encoded_bytes())
}

// NT create dispositions, and what the caller is told happened.
const FILE_SUPERSEDE: u32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;
const FILE_SUPERSEDED: u32 = 0;
const FILE_OPENED: u32 = 1;
const FILE_CREATED: u32 = 2;
const FILE_OVERWRITTEN: u32 = 3;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

fn wants_to_write(desired_access: u32) -> bool {
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const FILE_WRITE_DATA: u32 = 0x0002;
    const FILE_APPEND_DATA: u32 = 0x0004;
    desired_access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA) != 0
}

// The handle and what the caller is told happened, or the status that stopped it.
fn open_file(guest_path: &[u8], desired_access: u32, disposition: u32, options: u32) -> Result<(u32, u32), u32> {
    let (device, relative) = split_device(guest_path);
    let (root, writable) = device_root(&device);

    let found = if relative.is_empty() { None } else { resolve_insensitive(&root, &relative) };
    let existed = found.is_some();
    let resolved = match found {
        Some(_) if disposition == FILE_CREATE => return Err(STATUS_OBJECT_NAME_COLLISION),
        Some(resolved) => resolved,
        // A name that is not there is only an error until something is allowed to make
        // it: the disc cannot be written to, and a disposition that only opens does not
        // create.
        None => match host_path(&root, &relative) {
            Some(resolved)
                if !relative.is_empty() && writable && disposition != FILE_OPEN && disposition != FILE_OVERWRITE =>
            {
                resolved
            }
            _ => {
                note_missing(guest_path);
                return Err(STATUS_OBJECT_NAME_NOT_FOUND);
            }
        },
    };

    let (file, information) = if options & FILE_DIRECTORY_FILE != 0 || (existed && resolved.is_dir()) {
        // As create_directories, which makes nothing of a folder already there.
        if !existed && (resolved.exists() || fs::create_dir_all(&resolved).is_err()) {
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        }
        let file = File { path: resolved, is_directory: true, writable: false, open: Mutex::default() };
        (file, if existed { FILE_OPENED } else { FILE_CREATED })
    } else {
        // Truncating dispositions and a file that is not there yet both mean starting
        // from empty; anything else keeps what is on disk. Only ever under a mount: the
        // disc is read-only, and a disposition asking to overwrite one of its files must
        // not be taken at its word.
        let truncate = writable
            && (!existed
                || disposition == FILE_SUPERSEDE
                || disposition == FILE_OVERWRITE
                || disposition == FILE_OVERWRITE_IF);
        let file_writable = writable && (truncate || wants_to_write(desired_access));
        if !existed && let Some(parent) = resolved.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let handle =
            fs::OpenOptions::new().read(true).write(file_writable).create(truncate).truncate(truncate).open(&resolved);
        let Ok(handle) = handle else {
            note_missing(guest_path);
            return Err(STATUS_OBJECT_NAME_NOT_FOUND);
        };
        let size = if truncate { 0 } else { file_size(&resolved) };
        let information = if !existed {
            FILE_CREATED
        } else if truncate {
            if disposition == FILE_SUPERSEDE { FILE_SUPERSEDED } else { FILE_OVERWRITTEN }
        } else {
            FILE_OPENED
        };
        let open = Mutex::new(Open { handle: Some(handle), size, delete_on_close: false });
        (File { path: resolved, is_directory: false, writable: file_writable, open }, information)
    };
    if trace_files() {
        log_kernel!(
            "open %s -> %s (%s)",
            c_string(guest_path).as_c_str(),
            path_text(&file.path).as_c_str(),
            if file.writable { c"writable" } else { c"read-only" }
        );
    }
    let handle = objects::lock().insert(Object::new(Kind::Other(Box::new(file))));
    Ok((handle, information))
}

// The file a handle names, if it names one.
fn file_of(handle: u32) -> Option<Arc<Object>> {
    let object = objects::lock().lookup(handle).cloned()?;
    matches!(&object.kind, Kind::Other(other) if other.is::<File>()).then_some(object)
}

fn as_file(object: &Object) -> &File {
    match &object.kind {
        Kind::Other(other) => other.downcast_ref::<File>().expect("file_of checked it"),
        _ => unreachable!("file_of checked it"),
    }
}

// IO_STATUS_BLOCK: status, information.
fn complete(memory: Memory, status_block: u32, status: u32, information: u32) {
    if status_block != 0 {
        memory.store_u32(status_block, status);
        memory.store_u32(status_block + 4, information);
    }
}

fn opened(ctx: &mut crate::guest::PPCContext, memory: Memory, result: Result<(u32, u32), u32>) {
    let (handle_out, status_block) = (ctx.r3.as_u32(), ctx.r6.as_u32());
    let status = match result {
        Ok((handle, information)) => {
            if handle_out != 0 {
                memory.store_u32(handle_out, handle);
            }
            complete(memory, status_block, STATUS_SUCCESS, information);
            STATUS_SUCCESS
        }
        Err(status) => {
            complete(memory, status_block, status, 0);
            status
        }
    };
    ctx.r3.u64 = u64::from(status);
}

kernel_import!(NtCreateFile, |ctx, memory| {
    let name = object_name(memory, ctx.r5.as_u32());
    let options = stack_argument(ctx, memory, 8); // CreateOptions
    let result = open_file(&name, ctx.r4.as_u32(), ctx.r10.as_u32(), options);
    opened(ctx, memory, result);
});

kernel_import!(NtOpenFile, |ctx, memory| {
    let name = object_name(memory, ctx.r5.as_u32());
    let result = open_file(&name, ctx.r4.as_u32(), FILE_OPEN, ctx.r8.as_u32());
    opened(ctx, memory, result);
});

thread_local! {
    // Reads go into a buffer of our own and are copied across, not read straight into
    // guest memory: a page the GPU's shadow watches is read-only, and the kernel fails a
    // read into it where a copy faults and carries on.
    static BOUNCE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn offset_at(memory: Memory, pointer: u32) -> i64 {
    if pointer != 0 { memory.u64_at(pointer) as i64 } else { -1 }
}

kernel_import!(NtReadFile, |ctx, memory| {
    let status_block = ctx.r7.as_u32();
    let (buffer, length) = (ctx.r8.as_u32(), ctx.r9.as_u32());
    let object = file_of(ctx.r3.as_u32()).filter(|object| as_file(object).open().handle.is_some());
    let Some(object) = object.filter(|_| buffer != 0) else {
        complete(memory, status_block, STATUS_INVALID_PARAMETER, 0);
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    };
    let offset = offset_at(memory, ctx.r10.as_u32());
    let apc_routine = ctx.r5.as_u32() & !3;

    // Deferred to APC-delivery time when the caller asked for a completion routine, so
    // the buffer is filled at the moment the title is waiting for it rather than while it
    // is still decompressing out of the other half of its double buffer.
    let transfer = move || {
        let file = as_file(&object);
        let mut open = file.open();
        let Some(handle) = open.handle.as_mut() else { return };
        seek(handle, offset);
        let got = BOUNCE.with_borrow_mut(|bounce| {
            if bounce.len() < length as usize {
                bounce.resize(length as usize, 0);
            }
            let got = read_fully(handle, &mut bounce[..length as usize]);
            memory.store_bytes(buffer, &bounce[..got]);
            got
        });
        drop(open);
        let status = if got == 0 && length != 0 { STATUS_END_OF_FILE } else { STATUS_SUCCESS };
        complete(memory, status_block, status, got as u32);
        if trace_files() {
            log_kernel!(
                "read %s: %u bytes at %lld -> %llu into %08X (%s)",
                file.name().as_c_str(),
                length,
                offset,
                got as u64,
                buffer,
                if status != STATUS_SUCCESS { c"eof" } else { c"ok" }
            );
        }
    };

    if apc_routine == 0 {
        transfer();
        ctx.r3.u64 = u64::from(if status_block != 0 { memory.u32_at(status_block) } else { STATUS_SUCCESS });
        return;
    }
    // The outcome lands in the status block when the APC runs. Bit 0 of the routine is
    // NT's APC-mode flag.
    complete(memory, status_block, STATUS_PENDING, 0);
    queue_user_apc(apc_routine, ctx.r6.as_u32(), status_block, Some(Box::new(transfer)));
    ctx.r3.u64 = u64::from(STATUS_PENDING);
});

kernel_import!(NtWriteFile, |ctx, memory| {
    let status_block = ctx.r7.as_u32();
    let (buffer, length) = (ctx.r8.as_u32(), ctx.r9.as_u32());
    let object = file_of(ctx.r3.as_u32());
    let file = object.as_deref().map(as_file);
    // A write to anything but a mounted package is a write to the disc -- the title's own
    // logs go there. Accept and discard those.
    let Some(file) = file.filter(|file| file.writable && buffer != 0) else {
        complete(memory, status_block, STATUS_SUCCESS, length);
        ctx.r3.u64 = u64::from(STATUS_SUCCESS);
        return;
    };
    let mut open = file.open();
    let mut size = open.size;
    let Some(handle) = open.handle.as_mut() else {
        drop(open);
        complete(memory, status_block, STATUS_SUCCESS, length);
        ctx.r3.u64 = u64::from(STATUS_SUCCESS);
        return;
    };
    let offset = offset_at(memory, ctx.r10.as_u32());
    seek(handle, offset);
    let put = write_fully(handle, &memory.bytes(buffer, length as usize));
    size = size.max(tell(handle));
    open.size = size;
    drop(open);

    let status = if put == length as usize { STATUS_SUCCESS } else { STATUS_UNSUCCESSFUL };
    complete(memory, status_block, status, put as u32);
    if trace_files() {
        log_kernel!("write %s: %u bytes at %lld -> %llu", file.name().as_c_str(), length, offset, put as u64);
    }
    ctx.r3.u64 = u64::from(status);
});

kernel_import!(NtQueryInformationFile, |ctx, memory| {
    const FILE_STANDARD_INFORMATION: u32 = 5;
    const FILE_POSITION_INFORMATION: u32 = 14;
    const FILE_NETWORK_OPEN_INFORMATION: u32 = 34;
    let (status_block, buffer, class) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r7.as_u32());
    let Some(object) = file_of(ctx.r3.as_u32()) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    };
    let file = as_file(&object);
    let mut open = file.open();
    let size = open.size;
    if buffer != 0 {
        match class {
            FILE_STANDARD_INFORMATION => {
                memory.store_u64(buffer, size); // AllocationSize
                memory.store_u64(buffer + 8, size); // EndOfFile
                memory.store_u32(buffer + 16, 1); // NumberOfLinks
                memory.store_u32(buffer + 20, if file.is_directory { 0x0100_0000 } else { 0 }); // DeletePending / Directory
            }
            FILE_POSITION_INFORMATION => {
                memory.store_u64(buffer, open.handle.as_mut().map_or(0, tell));
            }
            FILE_NETWORK_OPEN_INFORMATION => {
                memory.zero(buffer, 32); // the times
                memory.store_u64(buffer + 32, size); // AllocationSize
                memory.store_u64(buffer + 40, size); // EndOfFile
            }
            _ => {}
        }
    }
    drop(open);
    complete(memory, status_block, STATUS_SUCCESS, 0);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtQueryFullAttributesFile, |ctx, memory| {
    let name = object_name(memory, ctx.r3.as_u32());
    let (device, relative) = split_device(&name);
    let (root, _) = device_root(&device);
    let resolved = if relative.is_empty() { None } else { resolve_insensitive(&root, &relative) };
    let Some(resolved) = resolved else {
        note_missing(&name);
        ctx.r3.u64 = u64::from(STATUS_OBJECT_NAME_NOT_FOUND);
        return;
    };
    let directory = resolved.is_dir();
    let size = if directory { 0 } else { file_size(&resolved) };
    let attributes = ctx.r4.as_u32();
    if attributes != 0 {
        memory.zero(attributes, 32);
        memory.store_u64(attributes + 32, size);
        memory.store_u64(attributes + 40, size);
        memory.store_u32(attributes + 48, if directory { 0x10 } else { 0x80 }); // DIRECTORY / NORMAL
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtSetInformationFile, |ctx, memory| {
    const FILE_DISPOSITION_INFORMATION: u32 = 13;
    const FILE_POSITION_INFORMATION: u32 = 14;
    const FILE_END_OF_FILE_INFORMATION: u32 = 20;
    let (status_block, buffer, class) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r7.as_u32());
    if let Some(object) = file_of(ctx.r3.as_u32()).filter(|_| buffer != 0) {
        let file = as_file(&object);
        let mut open = file.open();
        match (class, open.handle.as_mut()) {
            (FILE_POSITION_INFORMATION, Some(handle)) => seek(handle, memory.u64_at(buffer) as i64),
            (FILE_END_OF_FILE_INFORMATION, Some(handle)) if file.writable => {
                let end = memory.u64_at(buffer);
                if handle.set_len(end).is_ok() {
                    open.size = end;
                }
            }
            // A single byte: non-zero means delete when the last handle closes.
            (FILE_DISPOSITION_INFORMATION, _) => open.delete_on_close = file.writable && memory.u8_at(buffer) != 0,
            _ => {}
        }
    }
    complete(memory, status_block, STATUS_SUCCESS, 0);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// Every write is on the disk when it returns.
kernel_import!(NtFlushBuffersFile, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtQueryVolumeInformationFile, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtReadFileScatter, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_NOT_IMPLEMENTED);
});
