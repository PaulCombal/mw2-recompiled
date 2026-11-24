// Whether every shader of the game is compiled for this computer.
//
// The launcher's compile pass builds every shader in the game's files into the pipeline
// cache, then writes this marker: what the cache was made for. The cache holds good while
// all of it still holds -- the same shader code (this build's), the same device, driver
// and cache format, the same resolution scale (it is in the translated code), and the
// same files of the game. Anything else and the driver would compile shaders as the game
// meets them, which is the stutter the pass is there to remove, so the launcher runs the
// pass again before the game starts, and a game started without the launcher starts it.
//
// A fingerprint is compared whole, so the launcher's pass and the game write and read it
// through the same code here.
#![allow(clippy::missing_safety_doc)]

use crate::host::files;
use crate::pipeline;
use crate::shader_build::{BUILD_ID, CACHE_FILE};
use ash::vk;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

/// Beside the cache, shaders/pipelines.bin (shader_build.rs).
pub const MARKER_FILE: &str = "shaders/compiled.txt";

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// std::getline's lines: split at '\n', none after a last '\n'.
fn lines(text: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = text.split(|&byte| byte == b'\n').collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines
}

fn key(line: &[u8]) -> &[u8] {
    &line[..line.iter().position(|&byte| byte == b' ').unwrap_or(line.len())]
}

// The fastfiles of the disc and of title update 6, by name.
const OWN_FASTFILES: &[&str] = &[
    "af_caves",
    "af_chase",
    "airport",
    "arcadia",
    "boneyard",
    "cliffhanger",
    "co_hunted",
    "code_post_gfx",
    "code_post_gfx_mp",
    "common",
    "common_mp",
    "contingency",
    "dc_whitehouse",
    "dcburning",
    "dcemp",
    "dlc1_ui_mp",
    "dlc2_ui_mp",
    "ending",
    "estate",
    "favela",
    "favela_escape",
    "gulag",
    "invasion",
    "iw4_credits",
    "mp_afghan",
    "mp_afghan_load",
    "mp_boneyard",
    "mp_boneyard_load",
    "mp_brecourt",
    "mp_brecourt_load",
    "mp_checkpoint",
    "mp_checkpoint_load",
    "mp_derail",
    "mp_derail_load",
    "mp_estate",
    "mp_estate_load",
    "mp_favela",
    "mp_favela_load",
    "mp_highrise",
    "mp_highrise_load",
    "mp_invasion",
    "mp_invasion_load",
    "mp_nightshift",
    "mp_nightshift_load",
    "mp_quarry",
    "mp_quarry_load",
    "mp_rundown",
    "mp_rundown_load",
    "mp_rust",
    "mp_rust_load",
    "mp_subbase",
    "mp_subbase_load",
    "mp_terminal",
    "mp_terminal_load",
    "mp_underpass",
    "mp_underpass_load",
    "oilrig",
    "patch_mp",
    "roadkill",
    "so_ac130_co_hunted",
    "so_assault_oilrig",
    "so_bridge",
    "so_chopper_invasion",
    "so_crossing_so_bridge",
    "so_defense_invasion",
    "so_defuse_favela_escape",
    "so_demo_so_bridge",
    "so_download_arcadia",
    "so_escape_airport",
    "so_forest_contingency",
    "so_ghillies",
    "so_hidden_so_ghillies",
    "so_intel_boneyard",
    "so_juggernauts_favela",
    "so_killspree_favela",
    "so_killspree_invasion",
    "so_killspree_trainer",
    "so_rooftop_contingency",
    "so_sabotage_cliffhanger",
    "so_showers_gulag",
    "so_snowrace1_cliffhanger",
    "so_snowrace2_cliffhanger",
    "so_takeover_estate",
    "so_takeover_oilrig",
    "trainer",
    "ui",
    "ui_mp",
];

fn own_fastfile(file: &Path) -> bool {
    let stem = utf8(Path::new(file.file_stem().unwrap_or_default()));
    OWN_FASTFILES.iter().any(|name| name.as_bytes() == stem.as_slice())
}

// The order std::sort gives paths of one folder: their names' native characters
// compared one by one (bytes here, UTF-16 units on Windows).
fn native_order(path: &Path) -> Vec<u32> {
    let name = path.file_name().unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().iter().map(|&byte| u32::from(byte)).collect()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        name.encode_wide().map(u32::from).collect()
    }
}

/// The files the pass reads its shaders from: the fastfiles in `game` and the two
/// executables, which carry a few of their own. The fastfiles are the game's own, from
/// the disc and the update, unless `custom_maps` (MW2_CUSTOM_MAPS): then every one, a
/// custom map's included. Without it the multiplayer loads no other, and the pass does
/// not read one either.
pub fn game_files(game: &Path, custom_maps: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(game) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "ff") && (custom_maps || own_fastfile(&path)) {
                files.push(path);
            }
        }
    }
    files.sort_by_cached_key(|path| native_order(path));
    for executable in ["default.xex", "default_mp.xex"] {
        files.push(game.join(executable));
    }
    files
}

// libstdc++'s file_clock counts from 2174-01-01: the seconds from the Unix epoch to it.
#[cfg(unix)]
const FILE_CLOCK_EPOCH: i128 = 6_437_664_000;

// A file's last write in microseconds of std::filesystem::file_time_type's epoch, as
// duration_cast gives them (toward zero).
fn written(metadata: &fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let nanoseconds =
            (i128::from(metadata.mtime()) - FILE_CLOCK_EPOCH) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
        (nanoseconds / 1000) as i64
    }
    #[cfg(windows)]
    {
        // libc++ on Windows: the Unix epoch, from the FILETIME's 100 ns.
        use std::os::windows::fs::MetadataExt;
        let ticks = i128::from(metadata.last_write_time()) - 116_444_736_000_000_000;
        (ticks * 100 / 1000) as i64
    }
}

/// What a cache made now would be good for: a line a fact, as the marker keeps them.
/// `scaled` is whether either scale is past 1 as the device draws them (shader_build.rs),
/// which is what the shaders depend on, `custom_maps` as game_files.
///
/// # Safety
/// `physical` is a physical device of `instance`.
pub unsafe fn fingerprint(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    scaled: bool,
    game: &Path,
    custom_maps: bool,
) -> Vec<u8> {
    let mut text = format!("build {BUILD_ID}\n").into_bytes();
    let mut p = vk::PhysicalDeviceProperties::default();
    // SAFETY: the caller's.
    unsafe { (instance.fp_v1_0().get_physical_device_properties)(physical, &mut p) };
    let name = unsafe { CStr::from_ptr(p.device_name.as_ptr()) };
    text.extend_from_slice(format!("device-id {}\n", unsafe { pipeline::device_id(instance, physical) }).as_bytes());
    text.extend_from_slice(format!("device {:04x}:{:04x} ", p.vendor_id, p.device_id).as_bytes());
    text.extend_from_slice(name.to_bytes());
    text.extend_from_slice(format!("\ndriver {:08x}\n", p.driver_version).as_bytes());
    text.extend_from_slice(format!("cache-format {}\n", hex(&p.pipeline_cache_uuid)).as_bytes());
    text.extend_from_slice(format!("scaled {}\n", if scaled { "yes" } else { "no" }).as_bytes());
    // A file another of the same name and size replaced has another time.
    for file in game_files(game, custom_maps) {
        let Ok(metadata) = fs::metadata(&file) else { continue };
        if !metadata.is_file() {
            continue;
        }
        text.extend_from_slice(b"file ");
        text.extend_from_slice(&utf8(Path::new(file.file_name().unwrap_or_default())));
        text.extend_from_slice(format!(" {} {}\n", metadata.len(), written(&metadata)).as_bytes());
    }
    text
}

#[derive(Default)]
pub struct Marker {
    pub found: bool,
    pub fingerprint: Vec<u8>,
    /// The device-id line's device_id: which device it was made on.
    pub device: Vec<u8>,
    /// How many the pass compiled: none on a device without pipeline libraries.
    pub shaders: u32,
}

// strtoul's number, as the C library reads it (unsigned long: 32 bits on Windows).
fn strtoul(text: &[u8]) -> u32 {
    unsafe extern "C" {
        fn strtoul(text: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char, base: i32) -> std::ffi::c_ulong;
    }
    let end = text.iter().position(|&byte| byte == 0).unwrap_or(text.len());
    let text = CString::new(&text[..end]).unwrap_or_default();
    // SAFETY: a C string.
    unsafe { strtoul(text.as_ptr(), std::ptr::null_mut(), 10) as u32 }
}

pub fn read_marker() -> Marker {
    let mut marker = Marker::default();
    let Ok(data) = fs::read(MARKER_FILE) else { return marker };
    for line in lines(&data) {
        if let Some(count) = line.strip_prefix(b"shaders ") {
            marker.shaders = strtoul(count);
            marker.found = true; // the last line: a marker without it is not whole
            break;
        }
        if let Some(device) = line.strip_prefix(b"device-id ") {
            marker.device = device.to_vec();
        }
        marker.fingerprint.extend_from_slice(line);
        marker.fingerprint.push(b'\n');
    }
    marker
}

/// Whether the cache is there and the marker says it was made for exactly
/// `fingerprint`; if not, in `why`, what changed, in a player's words.
pub fn compiled(fingerprint: &[u8], why: Option<&mut &'static str>) -> bool {
    let say = |text: &'static str| {
        if let Some(why) = why {
            *why = text;
        }
        false
    };
    let marker = read_marker();
    if !marker.found {
        return say("The game's shaders have not been compiled for this computer yet.");
    }
    // A device that compiles shaders only with a draw's state had none compiled.
    if marker.shaders != 0 && !fs::metadata(CACHE_FILE).is_ok_and(|cache| cache.is_file() && cache.len() != 0) {
        return say("The compiled shaders are missing.");
    }
    if marker.fingerprint == fingerprint {
        return true;
    }
    // The first fact that differs.
    let (was, is) = (lines(&marker.fingerprint), lines(fingerprint));
    for i in 0..was.len().max(is.len()) {
        let a: &[u8] = was.get(i).copied().unwrap_or_default();
        let b: &[u8] = is.get(i).copied().unwrap_or_default();
        if a == b {
            continue;
        }
        return match key(if a.is_empty() { b } else { a }) {
            b"build" => say("This version of the game draws with other shaders than the last one."),
            b"device-id" | b"device" => say("The game now draws on another graphics card."),
            b"driver" | b"cache-format" => say("The graphics driver has changed."),
            b"scaled" => say("The resolution has changed, and the shaders are made for it."),
            _ => say("The game's files have changed."),
        };
    }
    say("The compiled shaders are for something else.")
}

/// The pass's last step, once the cache is in place. remove_marker is its first: a pass
/// that does not finish leaves no marker behind it.
pub fn write_marker(fingerprint: &[u8], shaders: u32) -> Result<(), Vec<u8>> {
    let partial = format!("{MARKER_FILE}.partial");
    let mut text = fingerprint.to_vec();
    text.extend_from_slice(format!("shaders {shaders}\n").as_bytes());
    if fs::write(&partial, &text).is_err() {
        return Err(b"the list of compiled shaders could not be written".to_vec());
    }
    let from = CString::new(partial).unwrap();
    let to = CString::new(MARKER_FILE).unwrap();
    files::rename_file(&from, &to).map_err(|message| {
        let mut error = b"the list of compiled shaders could not be put in place: ".to_vec();
        error.extend_from_slice(&message);
        error
    })
}

/// std::filesystem::remove: the file, or an empty folder of its name.
pub fn remove_marker() {
    files::remove_file(&CString::new(MARKER_FILE).unwrap());
}

/// The lines of two fingerprints that say what the code was compiled for and on -- all
/// but the game's files -- are the same: a cache made for one holds shaders the other
/// can use.
pub fn same_target(a: &[u8], b: &[u8]) -> bool {
    let target = |text: &[u8]| {
        let mut kept = Vec::new();
        for line in lines(text) {
            if key(line) != b"file" {
                kept.extend_from_slice(line);
                kept.push(b'\n');
            }
        }
        kept
    };
    target(a) == target(b)
}
