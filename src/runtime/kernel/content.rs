// Saved games.
//
// The title's save system is two steps. Memcard_* asks XContentCreate to mount a content
// package as a lettered drive -- "save0:" -- and then opens "save0:\savegame.svg" with
// the ordinary file API and writes it like any other file. Reporting no device is what
// puts the save-device prompt in front of the player before every level.
//
// A package on the console is an STFS container, a signed archive with the save inside
// it. Nothing here writes that format: a package is a directory under `saves/`, and
// mounting it is just an entry in the file layer's device table. The title never looks
// inside the container -- it only asks XAM to mount one and then reads and writes files
// through the drive letter -- so a directory is enough for it to save, load and delete.

use super::misc::ERROR_SUCCESS;
use super::{complete_overlapped, files, stack_argument};
use crate::env;
use crate::guest::{Memory, Register, guest_hook, kernel_import};
use crate::log::{c_string, log_kernel};
use crate::title;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const ERROR_FILE_NOT_FOUND: u32 = 0x0002;
const ERROR_PATH_NOT_FOUND: u32 = 0x0003;
const ERROR_INVALID_PARAMETER: u32 = 0x0057;
const ERROR_ALREADY_EXISTS: u32 = 0x00B7;
pub const ERROR_DEVICE_NOT_CONNECTED: u32 = 0x048F;

/// Where the saves and the profiles are: beside the runtime, not inside the game root,
/// which is the disc.
pub fn save_root() -> PathBuf {
    PathBuf::from("saves")
}

/// The one storage device this runtime offers. The title takes the id from the device
/// selector, hands it back in every content call and stores it in the package it
/// creates, so the selector and the content layer have to agree.
pub const SAVE_DEVICE_ID: u32 = 1;

// XCONTENT_DATA, as the title builds it: the device it chose, what kind of content this
// is, a display name for the dashboard and the name on disk.
const CONTENT_DEVICE_ID: u32 = 0;
const CONTENT_TYPE: u32 = 4;
const CONTENT_FILE_NAME: u32 = 264;
const CONTENT_FILE_MAX: usize = 42;

// XCONTENTFLAG_*: the low nibble of the flags is a create disposition, the same idea as
// NT's.
const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const TRUNCATE: u32 = 5;

fn guest_string(memory: Memory, address: u32, limit: usize) -> Vec<u8> {
    if address == 0 { Vec::new() } else { memory.string(address, limit) }
}

// The name goes on the host filesystem, so it may only be a name.
fn safe_name(name: &[u8]) -> Vec<u8> {
    let out: Vec<u8> = name
        .iter()
        .map(|&byte| if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') { byte } else { b'_' })
        .collect();
    let dots = out.iter().take_while(|&&byte| byte == b'.').count();
    out[dots..].to_vec()
}

fn package_of(name: &[u8]) -> PathBuf {
    save_root().join(Path::new(std::str::from_utf8(name).unwrap_or_default()))
}

fn path_text(path: &Path) -> std::ffi::CString {
    c_string(path.as_os_str().as_encoded_bytes())
}

// Memcard_InitializeSystem keeps the device each controller saved to in a table of one
// word per controller, and everything else asks that table first: a zero there means
// "nobody has chosen yet", which is the state the save-device prompt exists to get out
// of. On the console the choice is remembered in the player's profile, so it is made once
// ever; here the table starts empty on every launch, so the prompt would come back on
// every launch. There is only one device to choose, so choose it.
const CONTROLLERS: u32 = 4;

fn choose_device_for_player() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| !env::flag(c"MW2_NO_AUTO_SAVE_DEVICE"));
    *ON
}

// Memcard_InitializeSystem. The table is clear when it returns, so this is the moment to
// answer the question the prompt would ask.
guest_hook!(T_Memcard_InitializeSystem, |ctx, memory| {
    title::T_Memcard_InitializeSystem.original(ctx, memory);
    let table = title::T_DATA_DeviceTable;
    if !choose_device_for_player() || table == 0 {
        return;
    }
    for i in 0..CONTROLLERS {
        let chosen = table + i * 4;
        if memory.u32_at(chosen) == 0 {
            memory.store_u32(chosen, SAVE_DEVICE_ID);
        }
    }
    log_kernel!("content: the hard disk is the save device for every controller");
});

// Which storage devices exist. The title asks before it offers to save, and a "no" here
// is the save-device prompt.
kernel_import!(XamContentGetDeviceState, |ctx, memory| {
    let result = if ctx.r3.as_u32() == SAVE_DEVICE_ID { ERROR_SUCCESS } else { ERROR_DEVICE_NOT_CONNECTED };
    complete_overlapped(memory, ctx.r4.as_u32(), result);
    ctx.r3.u64 = u64::from(result);
});

// XDEVICE_DATA { id, type, totalBytes, freeBytes, friendlyName[28] }. The title compares
// the free space with the size it is about to write and refuses to save if it does not
// fit, so the numbers have to be real enough to pass.
kernel_import!(XamContentGetDeviceData, |ctx, memory| {
    if ctx.r3.as_u32() != SAVE_DEVICE_ID {
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    let data = ctx.r4.as_u32();
    if data != 0 {
        const TOTAL: u64 = 16 * 1024 * 1024 * 1024;
        const FREE: u64 = 8 * 1024 * 1024 * 1024;
        memory.zero(data, 80);
        memory.store_u32(data, SAVE_DEVICE_ID);
        memory.store_u32(data + 4, 1); // XCONTENTDEVICETYPE_HDD
        memory.store_u64(data + 8, TOTAL);
        memory.store_u64(data + 16, FREE);
        for (i, &letter) in (0u32..).zip(b"Hard Drive") {
            memory.store_u16(data + 24 + i * 2, u16::from(letter)); // 28 wide characters
        }
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// Mount a package as a drive. The disposition in the low nibble of the flags says whether
// it may be created, and the caller is told which of the two happened so it knows whether
// there is a save in there to read.
kernel_import!(XamContentCreateEx, |ctx, memory| {
    let (root_name, content_data, flags) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32());
    let (disposition_out, license_out) = (ctx.r7.as_u32(), ctx.r8.as_u32());
    let overlapped = stack_argument(ctx, memory, 8);

    let drive = guest_string(memory, root_name, 32);
    let file = safe_name(&guest_string(memory, content_data.wrapping_add(CONTENT_FILE_NAME), CONTENT_FILE_MAX));
    if drive.is_empty() || file.is_empty() || content_data == 0 {
        complete_overlapped(memory, overlapped, ERROR_INVALID_PARAMETER);
        ctx.r3.u64 = u64::from(ERROR_INVALID_PARAMETER);
        return;
    }

    let content_type = memory.u32_at(content_data + CONTENT_TYPE);
    let device = memory.u32_at(content_data + CONTENT_DEVICE_ID);
    let package = package_of(&file);
    let existed = package.is_dir();

    // "Create always" and "truncate" both mean the package the title gets is a new empty
    // one. Keeping the old contents instead is not a smaller version of that: the title
    // saves by creating its file with CREATE_NEW, so a leftover file from the last save
    // collides and the save fails.
    let disposition = flags & 0xF;
    let clear = disposition == CREATE_ALWAYS || disposition == TRUNCATE;

    let mut result = match disposition {
        CREATE_NEW if existed => ERROR_ALREADY_EXISTS,
        OPEN_EXISTING | TRUNCATE if !existed => ERROR_PATH_NOT_FOUND,
        _ => ERROR_SUCCESS,
    };
    if result == ERROR_SUCCESS && clear && existed {
        let _ = fs::remove_dir_all(&package);
    }
    if result == ERROR_SUCCESS && fs::create_dir_all(&package).is_err() && !package.is_dir() {
        result = ERROR_PATH_NOT_FOUND;
    }

    let drive_text = c_string(drive.as_slice());
    if result == ERROR_SUCCESS {
        files::mount(&drive, package.clone());
        log_kernel!(
            "content: mounted %s: on %s (device %u, type %u, %s)",
            drive_text.as_c_str(),
            path_text(&package).as_c_str(),
            device,
            content_type,
            if !existed {
                c"new"
            } else if clear {
                c"emptied"
            } else {
                c"existing"
            }
        );
    } else if result == ERROR_PATH_NOT_FOUND && !existed {
        // Asked to open a package nobody has saved yet: the first look for a new player's
        // stats, or for a save before the first.
        log_kernel!(
            "content: there is no %s yet (%s: asked to open it)",
            path_text(&package).as_c_str(),
            drive_text.as_c_str()
        );
    } else {
        log_kernel!(
            "content: refused %s: on %s -- error %u",
            drive_text.as_c_str(),
            path_text(&package).as_c_str(),
            result
        );
    }

    // XCONTENT_NEW / XCONTENT_EXISTING, and a licence mask of nothing, which is what a
    // package made on this console rather than bought carries.
    if disposition_out != 0 {
        memory.store_u32(disposition_out, if existed { 2 } else { 1 });
    }
    if license_out != 0 {
        memory.store_u32(license_out, 0);
    }
    complete_overlapped(memory, overlapped, result);
    ctx.r3.u64 = u64::from(result);
});

kernel_import!(XamContentClose, |ctx, memory| {
    let drive = guest_string(memory, ctx.r3.as_u32(), 32);
    if !drive.is_empty() {
        files::unmount(&drive);
    }
    complete_overlapped(memory, ctx.r4.as_u32(), ERROR_SUCCESS);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XamContentDelete, |ctx, memory| {
    let content_data = ctx.r4.as_u32();
    let file = safe_name(&guest_string(memory, content_data.wrapping_add(CONTENT_FILE_NAME), CONTENT_FILE_MAX));
    let mut result = ERROR_FILE_NOT_FOUND;
    if !file.is_empty() {
        let package = package_of(&file);
        if package.is_dir() {
            result = if fs::remove_dir_all(&package).is_ok() { ERROR_SUCCESS } else { ERROR_PATH_NOT_FOUND };
            if result == ERROR_SUCCESS {
                log_kernel!("content: deleted %s", path_text(&package).as_c_str());
            }
        }
    }
    complete_overlapped(memory, ctx.r5.as_u32(), result);
    ctx.r3.u64 = u64::from(result);
});
