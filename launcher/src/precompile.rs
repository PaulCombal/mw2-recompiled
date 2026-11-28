// Every shader of the game compiled before the game first runs.
//
// On the console a shader is ready the moment the title loads it. Here each has to be
// translated and compiled for the host's GPU, and one the game meets first in play is
// compiled there and then -- on a cold driver cache, tens of milliseconds of a frame
// each, hundreds in a level. The shaders are all in the game's files, so the launcher
// reads them out (the mw2-zone crate, zone/), builds each the way the game would (the shaders crate,
// shaders/src/shader_build.rs) into the pipeline cache the game starts with, and the
// game finds every one there.
//
// It is not optional: the launcher runs it after an install and whenever the marker it
// leaves (shaders/src/marker.rs) no longer fits -- another version, driver, graphics
// card, resolution or game file -- before it starts the game, and a game started on its
// own starts the launcher for it.
//
// The window and the terminal mode run it on a thread of their own (window.rs,
// terminal.rs), and it reports its progress to the Progress they draw or print. The
// texts a player reads are the same as the C++ pass's were, word for word.

use crate::{cnum, settings};
use ash::vk;
use mw2_shaders::{files, marker, pipeline, shader_build};
use mw2_zone as zone;
use mw2_zone::vertex_binding::BindTables;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

const GAME_FOLDER: &str = "game";

/// How the pass is going, for the window to draw and the terminal mode to print.
#[derive(Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    /// The marker has been looked at: reading or compiling now.
    pub checked: AtomicBool,
    /// What no longer fits; set before `checked`.
    pub why: Mutex<Vec<u8>>,
    pub files: AtomicU32,
    pub files_read: AtomicU32,
    pub shaders: AtomicU32,
    pub compiled: AtomicU32,
}

// Which of Progress's counters.
#[derive(Clone, Copy)]
enum Count {
    Files,
    FilesRead,
    Shaders,
    Compiled,
}

unsafe extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
}

/// The shaders crate's log (mw2_shaders::set_log), written as the launcher's C++ wrote
/// its lines (runtime/log.h): with MW2_LOG_TIME's stamp, to stderr, and not at all in a
/// build whose CMake folder has MW2_LOGGING off (build.rs).
pub fn log(line: &[u8]) {
    #[cfg(not(mw2_no_logging))]
    {
        use std::io::Write;
        let mut text = stamp().into_bytes();
        // As the C++ wrote it, a NUL in a device's name ends no line.
        text.extend(line.iter().copied().filter(|&byte| byte != 0));
        text.push(b'\n');
        let _ = std::io::stderr().lock().write_all(&text);
    }
    #[cfg(mw2_no_logging)]
    let _ = line;
}

/// MW2_LOG_TIME (set, and not "0") starts every line with the seconds since the first
/// stamped one, "%9.3f ".
#[cfg(not(mw2_no_logging))]
fn stamp() -> String {
    use std::sync::{LazyLock, OnceLock};
    static ON: LazyLock<bool> = LazyLock::new(|| variable(c"MW2_LOG_TIME").is_some_and(|value| value != c"0"));
    static START: OnceLock<Instant> = OnceLock::new();
    if !*ON {
        return String::new();
    }
    format!("{:9.3} ", START.get_or_init(Instant::now).elapsed().as_secs_f64())
}

// The pass's handle on the progress.
#[derive(Clone, Copy)]
struct Shown<'a>(&'a Progress);

impl<'a> Shown<'a> {
    fn cancelled(self) -> bool {
        self.0.cancel.load(Ordering::SeqCst)
    }
    fn cancel(self) {
        self.0.cancel.store(true, Ordering::SeqCst);
    }
    fn counter(self, which: Count) -> &'a AtomicU32 {
        match which {
            Count::Files => &self.0.files,
            Count::FilesRead => &self.0.files_read,
            Count::Shaders => &self.0.shaders,
            Count::Compiled => &self.0.compiled,
        }
    }
    fn set(self, which: Count, value: u32) {
        self.counter(which).store(value, Ordering::SeqCst);
    }
    fn add(self, which: Count) {
        self.counter(which).fetch_add(1, Ordering::SeqCst);
    }
    // What no longer fits, then that the marker has been looked at.
    fn checked(self, why: &[u8]) {
        *self.0.why.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = why.to_vec();
        self.0.checked.store(true, Ordering::SeqCst);
    }
}

/// What a pass came to.
#[derive(Clone, Default)]
pub struct Pass {
    pub ok: bool,
    pub cancelled: bool,
    /// Nothing to do: the marker fits.
    pub current: bool,
    /// Why not, when not.
    pub error: Vec<u8>,
    /// Why it ran: what no longer fit.
    pub why: Vec<u8>,
    /// The GPU it compiled for, by name, and by its id (shaders/src/pipeline.rs, device_id).
    pub device: Vec<u8>,
    pub device_id: Vec<u8>,
    /// The device compiles shaders only with a draw's state: none could be. Also on a
    /// marker that fits, which says so by its 0 shaders (marker::compiled).
    pub no_libraries: bool,
    /// Its maker, model and driver: which driver to offer (driver.rs).
    pub card: crate::driver::Card,
    /// Distinct shaders in the game.
    pub shaders: u32,
    /// The translator cannot make (the game cannot draw them either), and why, how many.
    pub untranslated: u32,
    pub reasons: BTreeMap<Vec<u8>, u32>,
    /// Translated, and the driver would not compile.
    pub refused: u32,
    /// Already in the cache of an earlier pass for the same build and device.
    pub from_cache: u32,
    pub read_seconds: f64,
    pub compile_seconds: f64,
    pub cache_bytes: u64,
}

/// Brings the compiled shaders up to date: checks the marker and, unless it fits (or
/// `always`), reads game/ and builds every shader in it, on every core. `device` (a
/// device_id) is the one the game draws on when it named one; else the one the marker
/// names, if the machine still has it; else the one the game's window would pick. Blocks
/// until done. Run from the folder the launcher is in.
pub fn compile(device: &[u8], always: bool, progress: &Progress) -> Pass {
    match run(device, always, Shown(progress)) {
        Ok(pass) => pass,
        // The threads it works on could not be started.
        Err(what) => Pass { error: what, ..Pass::default() },
    }
}

// The C library's getenv: null when unset, "" when set empty.
fn variable(name: &CStr) -> Option<&'static CStr> {
    // SAFETY: a C string; nothing changes the environment while the launcher runs.
    let value = unsafe { getenv(name.as_ptr()) };
    (!value.is_null()).then(|| unsafe { CStr::from_ptr(value) })
}

// MW2_SCALE as the game takes it: from its environment first, which a game that starts
// the launcher passes on, then from .env (the settings main loaded) -- strtol's number
// (a long: 32 bits on Windows), clamped to 1..=the most.
fn asked_scale(name: &CStr, setting: fn() -> i32, most: i32) -> u32 {
    match variable(name) {
        Some(set) => cnum::strtol(set.to_bytes(), 10).0.clamp(1, i64::from(most)) as u32,
        None => setting() as u32,
    }
}

fn asked_custom_maps() -> bool {
    match variable(c"MW2_CUSTOM_MAPS") {
        Some(set) => !set.is_empty() && set != c"0",
        None => settings::custom_maps(),
    }
}

// The launcher's setting goes as far as the crate's scale.
const _: () = assert!(shader_build::MAX_SHADOW_SCALE as i32 == settings::MAX_SHADOW_SCALE);

// Whether the game draws anything larger than the title's sizes on this device, which
// is what its shaders depend on.
fn scaled(instance: &ash::Instance, physical: vk::PhysicalDevice) -> bool {
    let scale = asked_scale(c"MW2_SCALE", settings::scale, settings::MAX_SCALE);
    let shadow = asked_scale(c"MW2_SHADOW_SCALE", settings::shadow_scale, settings::MAX_SHADOW_SCALE);
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
    card: crate::driver::Card,
    fingerprint: Vec<u8>,
    /// Which of the two caches and markers: shader_build::cache_file's.
    scaled: bool,
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
    let id = if asked.is_empty() { marker::last_device() } else { asked.to_vec() };
    // SAFETY: the live instance.
    let mut physical = if id.is_empty() { None } else { unsafe { pipeline::choose_device(&instance, &id, None) } };
    if physical.is_none() && !asked.is_empty() {
        *error = b"the graphics card the game draws on is not there any more".to_vec();
    }
    if physical.is_none() && asked.is_empty() {
        physical = unsafe { pipeline::choose_device(&instance, b"", None) };
    }
    let target = match physical {
        Some(physical) => {
            let scaled = scaled(&instance, physical);
            Some(Target {
                // SAFETY: a physical device of the live instance.
                device: unsafe { pipeline::device_id(&instance, physical) }.into_bytes(),
                card: unsafe { card(&instance, physical) },
                fingerprint: unsafe {
                    marker::fingerprint(&instance, physical, scaled, Path::new(GAME_FOLDER), asked_custom_maps())
                },
                scaled,
            })
        }
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

// The device's name, PCI ids and driver.
//
// # Safety
// A physical device of `instance`.
unsafe fn card(instance: &ash::Instance, physical: vk::PhysicalDevice) -> crate::driver::Card {
    let mut properties = vk::PhysicalDeviceProperties::default();
    // SAFETY: the caller's.
    unsafe { (instance.fp_v1_0().get_physical_device_properties)(physical, &mut properties) };
    let mut driver = vk::PhysicalDeviceDriverProperties::default();
    if properties.api_version >= vk::API_VERSION_1_2 {
        let mut more = vk::PhysicalDeviceProperties2 { p_next: (&raw mut driver).cast(), ..Default::default() };
        // SAFETY: the caller's; a chain on this stack.
        unsafe { (instance.fp_v1_1().get_physical_device_properties2)(physical, &mut more) };
    }
    crate::driver::Card {
        // SAFETY: Vulkan ends the name with a 0 within the array.
        name: unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }.to_bytes().to_vec(),
        vendor: properties.vendor_id,
        pci: properties.device_id,
        driver: driver.driver_id,
    }
}

// A shader of the game, by what it is.
struct Shader {
    pixel: bool,
    code: Vec<u32>,
}

type Shaders = BTreeMap<(bool, u64), Shader>;
// try_emplace: the first of a name stays.
fn keep(all: &mut Shaders, found: Vec<zone::shaders::Shader>) {
    for shader in found {
        all.entry((shader.pixel, shader.hash)).or_insert(Shader { pixel: shader.pixel, code: shader.code });
    }
}

// An executable's shaders and the tables it binds vertex shaders with.
fn executable_shaders(path: &Path, multiplayer: bool) -> zone::Result<(Vec<zone::shaders::Shader>, BindTables)> {
    let name = zone::fastfile::name(path);
    let xex = fs::read(path).map_err(|_| zone::Error::new(format!("cannot open {name}")))?;
    let image = mw2_install::xex::image(&xex).map_err(|error| zone::Error::new(format!("{name}: {error}")))?;
    let tables = BindTables::read(&image, multiplayer)?;
    Ok((zone::shaders::executable_shaders(&image, multiplayer, &tables)?, tables))
}

fn fastfile_shaders(path: &Path, tables: &BindTables) -> zone::Result<Vec<zone::shaders::Shader>> {
    Ok(zone::shaders::shaders(&zone::fastfile::read(path)?, tables)?.0)
}

// `work` on `count` threads of the launcher's (spawn::thread_builder), until each
// returns; why, when one could not be started, the pass then stopped (`progress`
// cancelled) so the ones that were started end at their next shader or file.
fn on_threads<F: Fn() + Sync>(count: u32, progress: Shown<'_>, work: &F) -> std::result::Result<(), Vec<u8>> {
    std::thread::scope(|scope| {
        let mut failed = Ok(());
        for _ in 0..count {
            if let Err(error) = crate::spawn::thread_builder().spawn_scoped(scope, work) {
                failed = Err(error.to_string().into_bytes());
                progress.cancel();
                break;
            }
        }
        failed
    })
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

fn run(asked: &[u8], always: bool, progress: Shown<'_>) -> std::result::Result<Pass, Vec<u8>> {
    let mut result = Pass::default();
    let mut error = Vec::new();
    let Some(target) = find_target(asked, &mut error) else {
        result.error = error;
        return Ok(result);
    };
    result.device_id = target.device.clone();
    result.card = target.card.clone();
    let mut why: &'static str = "";
    let scaled = target.scaled;
    let cache_file = shader_build::cache_file(scaled);
    if marker::compiled(&target.fingerprint, scaled, Some(&mut why)) && !always {
        result.ok = true;
        result.current = true;
        result.shaders = marker::read_marker(scaled).shaders;
        result.no_libraries = result.shaders == 0;
        result.device = target.card.name.clone();
        return Ok(result);
    }
    result.why = why.as_bytes().to_vec();
    if always {
        result.why = b"Asked for.".to_vec();
    }
    progress.checked(&result.why);

    // A pass that does not finish leaves no marker. The cache of the last one stays for
    // this one to start from, when it was for the same code on the same device and
    // driver at the same scale: only the shaders of files that changed are compiled
    // then. Anything else in it is of no use, and would only grow the file.
    let before = marker::read_marker(scaled);
    marker::remove_marker(scaled);
    if !before.found || !marker::same_target(&before.fingerprint, &target.fingerprint) {
        // std::filesystem::remove, its error ignored.
        files::remove_file(&CString::new(cache_file).unwrap());
    }
    // The other scale's stays for a player who goes back to it, while it is for the same
    // code, device and driver; else it is only room taken.
    let other = marker::read_marker(!scaled);
    if !other.found || !marker::same_device(&other.fingerprint, &target.fingerprint) {
        marker::remove_marker(!scaled);
        files::remove_file(&CString::new(shader_build::cache_file(!scaled)).unwrap());
    }

    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let cancelled = |result: &mut Pass| {
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
    if let Some(folder) = Path::new(cache_file).parent() {
        let _ = fs::create_dir_all(folder);
    }
    pipeline::set_cache_path(Some(cache_file.as_bytes()));
    pipeline::set_device(&target.device);
    // SAFETY: a headless device of its own.
    if !unsafe { pipeline::initialise(None) } {
        result.error = b"The graphics card could not be started to compile the shaders on.".to_vec();
        return Ok(result);
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
        if let Err(message) = marker::write_marker(&target.fingerprint, scaled, 0) {
            result.error = message;
            return Ok(result);
        }
        result.ok = true;
        return Ok(result);
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
    let mut tables: Option<BindTables> = None;
    for multiplayer in [false, true] {
        let path = Path::new(GAME_FOLDER).join(if multiplayer { "default_mp.xex" } else { "default.xex" });
        let read = match executable_shaders(&path, multiplayer) {
            Ok((found, read)) => {
                keep(&mut all, found);
                read
            }
            Err(message) => {
                result.error = message.0.into_bytes();
                return Ok(result);
            }
        };
        if tables.as_ref().is_some_and(|tables| *tables != read) {
            result.error = b"the two executables bind vertex shaders with different tables".to_vec();
            return Ok(result);
        }
        tables = Some(read);
        progress.add(Count::FilesRead);
    }
    let Some(tables) = tables else { return Ok(result) };
    let failure: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    {
        let next = AtomicUsize::new(0);
        let shared = Mutex::new(&mut all);
        on_threads(cores.min(4), progress, &|| {
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
                            failure.extend_from_slice(message.0.as_bytes());
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
        return Ok(result);
    }
    if cancelled(&mut result) {
        return Ok(result);
    }
    result.shaders = all.len() as u32;
    progress.set(Count::Shaders, result.shaders);

    // SAFETY: the instance and physical device of the live pipeline.
    let instance = unsafe { pipeline::instance_table(pipeline::instance()) };
    let depth = unsafe { shader_build::depth_format(&instance, physical) };

    // ---- each built as the game builds it, on every core -------------------------
    let compile_from = Instant::now();
    let work: Vec<&Shader> = all.values().collect();
    let next = AtomicUsize::new(0);
    let (untranslated, refused, from_cache) = (AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0));
    let reasons: Mutex<BTreeMap<Vec<u8>, u32>> = Mutex::new(BTreeMap::new());
    on_threads(cores, progress, &|| {
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
        return Ok(result);
    }
    // std::filesystem::file_size: a regular file's size, else an error, and -1.
    match fs::metadata(cache_file) {
        Ok(metadata) if metadata.is_file() => result.cache_bytes = metadata.len(),
        _ => {
            result.cache_bytes = u64::MAX;
            result.error = format!("The compiled shaders could not be written to {cache_file}.").into_bytes();
            return Ok(result);
        }
    }
    if let Err(message) = marker::write_marker(&target.fingerprint, scaled, result.shaders) {
        result.error = message;
        return Ok(result);
    }
    result.ok = true;
    Ok(result)
}
