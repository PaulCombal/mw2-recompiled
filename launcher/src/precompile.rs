// Every shader of the game compiled before the game first runs (launcher/precompile.h says
// why): the marker checked, and unless it fits, every shader of game/ read out by the
// launcher's zone reader and built as the game builds it, into the pipeline cache the
// game starts with.
//
// The window and the terminal mode run it through precompile::Run (precompile.cpp), on
// a thread of their own; this calls back there for the game's shaders, the settings,
// the threads it works on, and to report its progress and its result. The texts a
// player reads are the same as the C++ pass's were, word for word.

use ash::vk;
use mw2_shaders::{files, marker, pipeline, shader_build};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

const GAME_FOLDER: &str = "game";

// precompile::Progress and precompile::Result, which the C++ owns.
#[repr(C)]
pub struct Progress {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct Outcome {
    _opaque: [u8; 0],
}

// Which of Progress's counters (precompile.cpp's Count).
#[repr(u32)]
#[derive(Clone, Copy)]
enum Count {
    Files,
    FilesRead,
    Shaders,
    Compiled,
}

#[repr(C)]
struct Text {
    data: *const c_char,
    length: usize,
}

impl Text {
    fn of(bytes: &[u8]) -> Text {
        Text { data: bytes.as_ptr().cast(), length: bytes.len() }
    }
}

// precompile.cpp's Summary: precompile::Result but its reasons.
#[repr(C)]
struct Summary {
    ok: bool,
    cancelled: bool,
    current: bool,
    no_libraries: bool,
    shaders: u32,
    untranslated: u32,
    refused: u32,
    from_cache: u32,
    read_seconds: f64,
    compile_seconds: f64,
    cache_bytes: u64,
    error: Text,
    why: Text,
    device: Text,
    device_id: Text,
}

type ShaderSink = extern "C" fn(context: *mut c_void, pixel: bool, hash: u64, code: *const u32, count: usize);
type TextSink = extern "C" fn(context: *mut c_void, text: *const c_char, length: usize);

unsafe extern "C" {
    fn mw2l_scale() -> c_int;
    fn mw2l_shadow_scale() -> c_int;
    fn mw2l_custom_maps() -> bool;
    fn mw2l_max_scale() -> c_int;
    fn mw2l_max_shadow_scale() -> c_int;
    fn mw2l_bind_tables_size() -> usize;
    fn mw2l_executable_shaders(
        path: *const c_char,
        length: usize,
        multiplayer: bool,
        tables: *mut c_void,
        sink: ShaderSink,
        context: *mut c_void,
        error: TextSink,
        error_context: *mut c_void,
    ) -> bool;
    fn mw2l_fastfile_shaders(
        path: *const c_char,
        length: usize,
        tables: *const c_void,
        sink: ShaderSink,
        context: *mut c_void,
        error: TextSink,
        error_context: *mut c_void,
    ) -> bool;
    fn mw2l_cores() -> u32;
    fn mw2l_threads(count: u32, work: extern "C" fn(*mut c_void), context: *mut c_void) -> bool;
    fn mw2l_log(line: *const c_char);
    fn mw2l_progress_cancelled(progress: *mut Progress) -> bool;
    fn mw2l_progress_cancel(progress: *mut Progress);
    fn mw2l_progress_set(progress: *mut Progress, which: Count, value: u32);
    fn mw2l_progress_add(progress: *mut Progress, which: Count);
    fn mw2l_progress_checked(progress: *mut Progress, why: *const c_char, length: usize) -> bool;
    fn mw2l_result(result: *mut Outcome, summary: *const Summary) -> bool;
    fn mw2l_result_reason(result: *mut Outcome, text: *const c_char, length: usize, count: u32) -> bool;

    fn getenv(name: *const c_char) -> *const c_char;
    fn strtol(text: *const c_char, end: *mut *mut c_char, base: c_int) -> c_long;
}

/// The shaders crate's log (mw2_shaders::set_log), written as the launcher's C++ writes
/// its lines: with MW2_LOG_TIME's stamp, to the C library's stderr.
pub fn log(line: &[u8]) {
    // As printf's %s, a NUL in a device's name ends no line.
    let mut text: Vec<u8> = line.iter().copied().filter(|&byte| byte != 0).collect();
    text.push(0);
    // SAFETY: a C string.
    unsafe { mw2l_log(text.as_ptr().cast()) };
}

// The C++ pass's handle on the window's progress.
#[derive(Clone, Copy)]
struct Shown(*mut Progress);
// SAFETY: every call on it is precompile.cpp's, on its atomics.
unsafe impl Send for Shown {}
unsafe impl Sync for Shown {}

impl Shown {
    fn cancelled(self) -> bool {
        // SAFETY: the Progress precompile::Run was handed, alive across the pass.
        unsafe { mw2l_progress_cancelled(self.0) }
    }
    fn cancel(self) {
        unsafe { mw2l_progress_cancel(self.0) }
    }
    fn set(self, which: Count, value: u32) {
        unsafe { mw2l_progress_set(self.0, which, value) }
    }
    fn add(self, which: Count) {
        unsafe { mw2l_progress_add(self.0, which) }
    }
    fn checked(self, why: &[u8]) -> Option<()> {
        unsafe { mw2l_progress_checked(self.0, why.as_ptr().cast(), why.len()) }.then_some(())
    }
}

// precompile::Result as the pass makes it.
#[derive(Default)]
struct Result {
    ok: bool,
    cancelled: bool,
    current: bool,
    error: Vec<u8>,
    why: Vec<u8>,
    device: Vec<u8>,
    device_id: Vec<u8>,
    no_libraries: bool,
    shaders: u32,
    untranslated: u32,
    reasons: BTreeMap<Vec<u8>, u32>,
    refused: u32,
    from_cache: u32,
    read_seconds: f64,
    compile_seconds: f64,
    cache_bytes: u64,
}

/// precompile::Run's work: `device` the device_id the game draws on, or empty.
///
/// A call into the C++ that fails here (None below) holds an exception the C++ pass
/// would have let out of precompile::Run at that point: the pass returns at once, as
/// its unwinding would have, and Run throws it.
///
/// # Safety
/// `device` points at `length` bytes, `progress` and `result` at the C++'s Progress and
/// Result, all alive until it returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mw2_precompile_run(
    device: *const u8,
    length: usize,
    always: bool,
    progress: *mut Progress,
    result: *mut Outcome,
) {
    let asked = if length == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(device, length) } };
    let Some(r) = run(asked, always, Shown(progress)) else { return };
    let summary = Summary {
        ok: r.ok,
        cancelled: r.cancelled,
        current: r.current,
        no_libraries: r.no_libraries,
        shaders: r.shaders,
        untranslated: r.untranslated,
        refused: r.refused,
        from_cache: r.from_cache,
        read_seconds: r.read_seconds,
        compile_seconds: r.compile_seconds,
        cache_bytes: r.cache_bytes,
        error: Text::of(&r.error),
        why: Text::of(&r.why),
        device: Text::of(&r.device),
        device_id: Text::of(&r.device_id),
    };
    // SAFETY: the caller's Result; the texts live across the calls.
    unsafe {
        if !mw2l_result(result, &summary) {
            return;
        }
        for (text, &count) in &r.reasons {
            if !mw2l_result_reason(result, text.as_ptr().cast(), text.len(), count) {
                return;
            }
        }
    }
}

// The C library's getenv: null when unset, "" when set empty.
fn variable(name: &CStr) -> Option<&'static CStr> {
    // SAFETY: a C string; nothing changes the environment while the launcher runs.
    let value = unsafe { getenv(name.as_ptr()) };
    (!value.is_null()).then(|| unsafe { CStr::from_ptr(value) })
}

// MW2_SCALE as the game takes it: from its environment first, which a game that starts
// the launcher passes on, then from .env -- strtol's number (a long: 32 bits on
// Windows), clamped to 1..=the most.
fn asked_scale(name: &CStr, setting: unsafe extern "C" fn() -> c_int, most: unsafe extern "C" fn() -> c_int) -> u32 {
    // SAFETY: a C string; the settings main loaded.
    unsafe {
        match variable(name) {
            Some(set) => strtol(set.as_ptr(), std::ptr::null_mut(), 10).clamp(1, most() as c_long) as u32,
            None => setting() as u32,
        }
    }
}

fn asked_custom_maps() -> bool {
    match variable(c"MW2_CUSTOM_MAPS") {
        Some(set) => !set.is_empty() && set != c"0",
        None => unsafe { mw2l_custom_maps() },
    }
}

// The launcher's setting goes as far as the crate's scale: precompile.cpp holds
// settings::kMaxShadowScale to the same 4.
const _: () = assert!(shader_build::MAX_SHADOW_SCALE == 4);

// Whether the game draws anything larger than the title's sizes on this device, which
// is what its shaders depend on.
fn scaled(instance: &ash::Instance, physical: vk::PhysicalDevice) -> bool {
    let scale = asked_scale(c"MW2_SCALE", mw2l_scale, mw2l_max_scale);
    let shadow = asked_scale(c"MW2_SHADOW_SCALE", mw2l_shadow_scale, mw2l_max_shadow_scale);
    // SAFETY: a physical device of `instance`.
    unsafe {
        shader_build::shadow_scale(instance, physical, shadow, shader_build::scale(instance, physical, scale)) > 1
    }
}

// A path's text, as std::filesystem::path::u8string gives it.
fn utf8(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

fn name(path: &Path) -> Vec<u8> {
    utf8(Path::new(path.file_name().unwrap_or_default()))
}

// The device to compile for, and the marker a pass on it would leave now: from an
// instance of its own, without bringing a device up.
struct Target {
    device: Vec<u8>,
    fingerprint: Vec<u8>,
}

fn find_target(asked: &[u8], error: &mut Vec<u8>) -> Option<Target> {
    let app = vk::ApplicationInfo {
        p_application_name: c"mw2recomp".as_ptr(),
        api_version: vk::API_VERSION_1_2,
        ..Default::default()
    };
    let info = vk::InstanceCreateInfo { p_application_info: &app, ..Default::default() };
    let mut handle = vk::Instance::null();
    // SAFETY: the infos on this stack.
    let made = unsafe { (pipeline::entry().fp_v1_0().create_instance)(&info, std::ptr::null(), &mut handle) };
    if made != vk::Result::SUCCESS {
        *error = b"Vulkan is not available on this computer".to_vec();
        return None;
    }
    // SAFETY: the instance just made, destroyed below.
    let instance = unsafe { pipeline::instance_table(handle) };
    // The device the game named; else the one the last pass was for, which a game
    // drawing elsewhere than the window's own choice named then.
    let id = if asked.is_empty() { marker::read_marker().device } else { asked.to_vec() };
    // SAFETY: the live instance.
    let mut physical = if id.is_empty() { None } else { unsafe { pipeline::choose_device(&instance, &id, None) } };
    if physical.is_none() && !asked.is_empty() {
        *error = b"the graphics card the game draws on is not there any more".to_vec();
    }
    if physical.is_none() && asked.is_empty() {
        physical = unsafe { pipeline::choose_device(&instance, b"", None) };
    }
    let target = match physical {
        Some(physical) => Some(Target {
            // SAFETY: a physical device of the live instance.
            device: unsafe { pipeline::device_id(&instance, physical) }.into_bytes(),
            fingerprint: unsafe {
                marker::fingerprint(
                    &instance,
                    physical,
                    scaled(&instance, physical),
                    Path::new(GAME_FOLDER),
                    asked_custom_maps(),
                )
            },
        }),
        None => {
            if error.is_empty() {
                *error = b"no graphics card here can draw the game".to_vec();
            }
            None
        }
    };
    // SAFETY: nothing made from it is alive.
    unsafe { (instance.fp_v1_0().destroy_instance)(handle, std::ptr::null()) };
    target
}

// A shader of the game, by what it is.
struct Shader {
    pixel: bool,
    code: Vec<u32>,
}

type Shaders = BTreeMap<(bool, u64), Shader>;
// A file's shaders as the zone reader hands them: pixel, name, code.
type Found = Vec<(bool, u64, Vec<u32>)>;

extern "C" fn into_list(context: *mut c_void, pixel: bool, hash: u64, code: *const u32, count: usize) {
    // SAFETY: the list the caller handed as `context`, and `count` words.
    let list = unsafe { &mut *context.cast::<Found>() };
    let code = if count == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(code, count) }.to_vec() };
    list.push((pixel, hash, code));
}

extern "C" fn into_text(context: *mut c_void, text: *const c_char, length: usize) {
    // SAFETY: the text the caller handed as `context`, and `length` bytes.
    let into = unsafe { &mut *context.cast::<Vec<u8>>() };
    if length != 0 {
        into.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) });
    }
}

// try_emplace: the first of a name stays.
fn keep(all: &mut Shaders, found: Found) {
    for (pixel, hash, code) in found {
        all.entry((pixel, hash)).or_insert(Shader { pixel, code });
    }
}

// An executable's shaders and the tables it binds vertex shaders with.
fn executable_shaders(path: &Path, multiplayer: bool, tables: &mut [u8]) -> std::result::Result<Found, Vec<u8>> {
    let text = utf8(path);
    let mut found = Vec::new();
    let mut error = Vec::new();
    // SAFETY: the path's bytes, `tables` of mw2l_bind_tables_size bytes, and the sinks'
    // contexts on this stack.
    let ok = unsafe {
        mw2l_executable_shaders(
            text.as_ptr().cast(),
            text.len(),
            multiplayer,
            tables.as_mut_ptr().cast(),
            into_list,
            (&raw mut found).cast(),
            into_text,
            (&raw mut error).cast(),
        )
    };
    if ok { Ok(found) } else { Err(error) }
}

fn fastfile_shaders(path: &Path, tables: &[u8]) -> std::result::Result<Found, Vec<u8>> {
    let text = utf8(path);
    let mut found = Vec::new();
    let mut error = Vec::new();
    // SAFETY: as executable_shaders.
    let ok = unsafe {
        mw2l_fastfile_shaders(
            text.as_ptr().cast(),
            text.len(),
            tables.as_ptr().cast(),
            into_list,
            (&raw mut found).cast(),
            into_text,
            (&raw mut error).cast(),
        )
    };
    if ok { Ok(found) } else { Err(error) }
}

// `work` on `count` of the C++ library's threads, as the C++ pass started its own;
// None when they could not be started.
fn on_threads<F: Fn() + Sync>(count: u32, work: &F) -> Option<()> {
    extern "C" fn trampoline<F: Fn() + Sync>(context: *mut c_void) {
        // SAFETY: the closure on_threads lends, alive until every thread is joined.
        let work = unsafe { &*context.cast::<F>() };
        work();
    }
    // SAFETY: the closure outlives the call, which joins every thread it starts.
    unsafe { mw2l_threads(count, trampoline::<F>, (work as *const F).cast_mut().cast()) }.then_some(())
}

// Shuts the device down on every return past its initialise, as the C++ pass's Down.
struct Down {
    on: bool,
}
impl Drop for Down {
    fn drop(&mut self) {
        if self.on {
            // SAFETY: nothing made with the device is alive past the pass.
            unsafe { pipeline::shutdown() };
        }
    }
}

fn run(asked: &[u8], always: bool, progress: Shown) -> Option<Result> {
    let mut result = Result::default();
    let mut error = Vec::new();
    let Some(target) = find_target(asked, &mut error) else {
        result.error = error;
        return Some(result);
    };
    result.device_id = target.device.clone();
    let mut why: &'static str = "";
    if marker::compiled(&target.fingerprint, Some(&mut why)) && !always {
        result.ok = true;
        result.current = true;
        result.shaders = marker::read_marker().shaders;
        return Some(result);
    }
    result.why = why.as_bytes().to_vec();
    if always {
        result.why = b"Asked for.".to_vec();
    }
    progress.checked(&result.why)?;

    // A pass that does not finish leaves no marker. The cache of the last one stays for
    // this one to start from, when it was for the same code on the same device and
    // driver at the same scale: only the shaders of files that changed are compiled
    // then. Anything else in it is of no use, and would only grow the file.
    let before = marker::read_marker();
    marker::remove_marker();
    if !before.found || !marker::same_target(&before.fingerprint, &target.fingerprint) {
        // std::filesystem::remove, its error ignored.
        files::remove_file(&CString::new(shader_build::CACHE_FILE).unwrap());
    }

    // SAFETY: the C++ library's count.
    let cores = unsafe { mw2l_cores() }.max(1);
    let cancelled = |result: &mut Result| {
        if !progress.cancelled() {
            return false;
        }
        result.cancelled = true;
        result.error = b"The shaders were not compiled: stopped.".to_vec();
        true
    };

    // ---- the device the game draws on, with its pipeline cache -------------------
    // First, so a device that cannot compile a shader on its own is known before the
    // game's files are read for nothing.
    if let Some(folder) = Path::new(shader_build::CACHE_FILE).parent() {
        let _ = fs::create_dir_all(folder);
    }
    pipeline::set_cache_path(Some(shader_build::CACHE_FILE.as_bytes()));
    pipeline::set_device(&target.device);
    // SAFETY: a headless device of its own.
    if !unsafe { pipeline::initialise(None) } {
        result.error = b"The graphics card could not be started to compile the shaders on.".to_vec();
        return Some(result);
    }
    result.device = pipeline::device_name().into_bytes();
    let physical = pipeline::physical_device();
    if !pipeline::pipeline_libraries() {
        // Without pipeline libraries a shader is compiled only as part of a whole
        // pipeline, which needs the state of a draw to make: the game compiles them as
        // it meets them, and there is nothing to do here.
        // SAFETY: nothing is made with the device yet.
        unsafe { pipeline::shutdown() };
        result.no_libraries = true;
        if let Err(message) = marker::write_marker(&target.fingerprint, 0) {
            result.error = message;
            return Some(result);
        }
        result.ok = true;
        return Some(result);
    }
    // From here every return leaves the device to the next pass's initialise.
    let mut down = Down { on: true };

    // ---- every shader of the game, once ------------------------------------------
    // A level's zone is up to 160 MB once inflated, so only a few are read at a time.
    let read_from = Instant::now();
    let files: Vec<PathBuf> = marker::game_files(Path::new(GAME_FOLDER), asked_custom_maps())
        .into_iter()
        .filter(|file| file.extension().is_some_and(|extension| extension == "ff"))
        .collect();
    progress.set(Count::Files, files.len() as u32 + 2);
    let mut all = Shaders::new();
    // The two executables: the shaders they carry, and the tables vertex shaders are
    // bound with, which must agree.
    // SAFETY: the C++ library's size.
    let size = unsafe { mw2l_bind_tables_size() };
    let mut tables = vec![0u8; size];
    let mut first = true;
    for multiplayer in [false, true] {
        let path = Path::new(GAME_FOLDER).join(if multiplayer { "default_mp.xex" } else { "default.xex" });
        let mut read = vec![0u8; size];
        match executable_shaders(&path, multiplayer, &mut read) {
            Ok(found) => keep(&mut all, found),
            Err(message) => {
                result.error = message;
                return Some(result);
            }
        }
        if !first && tables != read {
            result.error = b"the two executables bind vertex shaders with different tables".to_vec();
            return Some(result);
        }
        tables = read;
        first = false;
        progress.add(Count::FilesRead);
    }
    let failure: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    {
        let next = AtomicUsize::new(0);
        let shared = Mutex::new(&mut all);
        on_threads(cores.min(4), &|| {
            while !progress.cancelled() {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                match fastfile_shaders(&files[i], &tables) {
                    Ok(found) => keep(&mut shared.lock().unwrap(), found),
                    Err(message) => {
                        // A file of the game the pass cannot read is one whose shaders it
                        // cannot compile: the pass fails, naming it.
                        let _guard = shared.lock().unwrap();
                        let mut failure = failure.lock().unwrap();
                        if failure.is_empty() {
                            *failure = name(&files[i]);
                            failure.extend_from_slice(b": ");
                            failure.extend_from_slice(&message);
                        }
                        progress.cancel();
                    }
                }
                progress.add(Count::FilesRead);
            }
        })?;
    }
    result.read_seconds = read_from.elapsed().as_secs_f64();
    let failure = failure.into_inner().unwrap();
    if !failure.is_empty() {
        result.error = b"A file of the game could not be read, so its shaders cannot be compiled: ".to_vec();
        result.error.extend_from_slice(&failure);
        return Some(result);
    }
    if cancelled(&mut result) {
        return Some(result);
    }
    result.shaders = all.len() as u32;
    progress.set(Count::Shaders, result.shaders);

    // SAFETY: the instance and physical device of the live pipeline.
    let instance = unsafe { pipeline::instance_table(pipeline::instance()) };
    let depth = unsafe { shader_build::depth_format(&instance, physical) };
    let scaled = scaled(&instance, physical);

    // ---- each built as the game builds it, on every core -------------------------
    let compile_from = Instant::now();
    let work: Vec<&Shader> = all.values().collect();
    let next = AtomicUsize::new(0);
    let (untranslated, refused, from_cache) = (AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0));
    let reasons: Mutex<BTreeMap<Vec<u8>, u32>> = Mutex::new(BTreeMap::new());
    on_threads(cores, &|| {
        while !progress.cancelled() {
            let i = next.fetch_add(1, Ordering::SeqCst);
            if i >= work.len() {
                break;
            }
            let shader = work[i];
            let mut built = shader_build::build(shader.pixel, &shader.code, scaled, depth);
            if !built.translation.ok {
                untranslated.fetch_add(1, Ordering::SeqCst);
                *reasons.lock().unwrap().entry(built.translation.error.as_bytes().to_vec()).or_insert(0) += 1;
            } else if built.module == vk::ShaderModule::null() || built.library == vk::Pipeline::null() {
                refused.fetch_add(1, Ordering::SeqCst);
            } else if built.from_cache == 1 {
                from_cache.fetch_add(1, Ordering::SeqCst);
            }
            // SAFETY: nothing is being made from them.
            unsafe { shader_build::destroy(&mut built) };
            progress.add(Count::Compiled);
        }
    })?;
    result.compile_seconds = compile_from.elapsed().as_secs_f64();
    result.untranslated = untranslated.into_inner();
    result.refused = refused.into_inner();
    result.from_cache = from_cache.into_inner();
    result.reasons = reasons.into_inner().unwrap();

    down.on = false;
    // Writes the cache, if it got this far: what is compiled is kept.
    // SAFETY: every shader built is destroyed.
    unsafe { pipeline::shutdown() };
    if cancelled(&mut result) {
        return Some(result);
    }
    // std::filesystem::file_size: a regular file's size, else an error, and -1.
    match fs::metadata(shader_build::CACHE_FILE) {
        Ok(metadata) if metadata.is_file() => result.cache_bytes = metadata.len(),
        _ => {
            result.cache_bytes = u64::MAX;
            result.error =
                format!("The compiled shaders could not be written to {}.", shader_build::CACHE_FILE).into_bytes();
            return Some(result);
        }
    }
    if let Err(message) = marker::write_marker(&target.fingerprint, result.shaders) {
        result.error = message;
        return Some(result);
    }
    result.ok = true;
    Some(result)
}
