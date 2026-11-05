// How a player starts the game: with no arguments. The executable works from its own
// folder, where the launcher (launcher/) installed the game's files into game/. With
// no install there, or another version's, it starts the launcher instead.
//
// A run given an image and a game folder on the command line is a development run and
// skips all of this.

// The custom maps' (src/hooks/custom_map_list.rs), which only the update's multiplayer has.
#[cfg_attr(not(T_UI_LoadArenas), allow(dead_code))]
pub mod arena;
pub mod crypto;
#[allow(unsafe_code)]
pub mod spawn;
pub mod xex;

use crate::env;
use crate::log::{log_error, log_info, log_warn};
use crate::runtime::platform;
use spawn::c_path;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

// The executable this build was recompiled from, by its SHA-256, which CMake reads
// from mw2/ at configure time. Any other -- another region, another version -- has
// other code at other addresses, so it is not run.
const SHA256: &str = env!("MW2_XEX_SHA256");
#[cfg(mw2_title_mp)]
const XEX: &str = "default_mp.xex";
#[cfg(not(mw2_title_mp))]
const XEX: &str = "default.xex";
const GAME_FOLDER: &str = "game";
#[cfg(windows)]
const LAUNCHER: &str = "mw2-launcher.exe";
#[cfg(not(windows))]
const LAUNCHER: &str = "mw2-launcher";
const LAUNCH_DATA_VARIABLE: &CStr = c"MW2_LAUNCH_DATA";

static PLAYER_START: AtomicBool = AtomicBool::new(false);
static NEXT_TITLE: Mutex<Option<CString>> = Mutex::new(None);

fn c_text(text: impl Into<Vec<u8>>) -> CString {
    CString::new(text).unwrap_or_default()
}

/// The folder a start runs in: the executable's for a player's start, which prepare
/// moves to, and the current directory (empty) otherwise.
pub fn folder(argc: i32) -> PathBuf {
    if argc == 1 { executable_folder() } else { PathBuf::new() }
}

fn executable_folder() -> PathBuf {
    std::env::current_exe().ok().and_then(|path| Some(path.parent()?.to_path_buf())).unwrap_or_default()
}

fn show_error(text: &str) {
    let text = c_text(text);
    log_error!("%s", text.as_c_str());
    platform::print_line(&text);
    spawn::message_box(&text);
}

// The launcher beside this executable, left running on its own.
fn start_launcher(arguments: &[&CStr]) -> bool {
    let Ok(launcher) = std::path::absolute(LAUNCHER) else { return false };
    if !launcher.is_file() {
        return false;
    }
    let path = c_path(&launcher);
    let mut all = vec![path.as_c_str()];
    all.extend_from_slice(arguments);
    match spawn::start(&all, None) {
        Ok(()) => true,
        Err(error) => {
            log_error!("cannot start %s: %s", path.as_c_str(), c_text(error).as_c_str());
            false
        }
    }
}

// The program of each of the title's executables, beside this one.
fn program_for(xex: &[u8]) -> Option<&'static str> {
    match xex {
        #[cfg(windows)]
        b"default.xex" => Some("mw2-sp.exe"),
        #[cfg(windows)]
        b"default_mp.xex" => Some("mw2-mp.exe"),
        #[cfg(not(windows))]
        b"default.xex" => Some("mw2-sp"),
        #[cfg(not(windows))]
        b"default_mp.xex" => Some("mw2-mp"),
        _ => None,
    }
}

enum Installed {
    Yes(Vec<u8>),
    No,
    Unreadable(String),
}

// The title's image from game/, if the executable there is the one this build was
// made from.
fn load_installed() -> Installed {
    let path = Path::new(GAME_FOLDER).join(XEX);
    let file = fs::read(&path).unwrap_or_default();
    if file.is_empty() || crypto::sha256_hex(&file) != SHA256 {
        return Installed::No;
    }
    match xex::image(&file) {
        Ok(image) => Installed::Yes(image),
        Err(error) => Installed::Unreadable(format!("{}: {error}", path.display())),
    }
}

/// Loads the installed title. None when the run should end, with `exit_code` saying
/// how: the launcher was started, or a failure was shown.
pub fn prepare(exit_code: &mut i32) -> Option<Vec<u8>> {
    *exit_code = 1;
    PLAYER_START.store(true, Ordering::Relaxed);
    // Everything a player's copy keeps -- game/, saves/ -- is beside the executable.
    let folder = executable_folder();
    if !folder.as_os_str().is_empty() {
        let _ = std::env::set_current_dir(folder);
    }

    match load_installed() {
        Installed::Yes(image) => return Some(image),
        Installed::Unreadable(error) => {
            show_error(&error);
            return None;
        }
        Installed::No => {}
    }
    // Not installed, or installed for another version of this build: both are the
    // launcher's to put right.
    if start_launcher(&[]) {
        *exit_code = 0;
        return None;
    }
    show_error(&format!("The game is not installed here, and {LAUNCHER} is not beside this program to install it."));
    None
}

/// A development run's image: a flat PE, or an XEX decrypted on the way in.
pub fn load_image_file(path: &Path) -> Option<Vec<u8>> {
    let shown = c_path(path);
    let Ok(file) = fs::read(path) else {
        log_error!("cannot open image %s", shown.as_c_str());
        return None;
    };
    if !file.starts_with(b"XEX2") {
        return Some(file);
    }
    match xex::image(&file) {
        Ok(image) => Some(image),
        Err(error) => {
            log_error!("%s: %s", shown.as_c_str(), c_text(error).as_c_str());
            None
        }
    }
}

fn hex_digit(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|value| value as u8)
}

// Two hex digits a byte, from the title that left it: nothing at all if anything else
// is there.
fn parse_launch_data(hex: &[u8]) -> Vec<u8> {
    hex.chunks_exact(2)
        .map(|pair| Some(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?))
        .collect::<Option<_>>()
        .unwrap_or_default()
}

fn launch_data_held() -> MutexGuard<'static, Option<Vec<u8>>> {
    static DATA: Mutex<Option<Vec<u8>>> = Mutex::new(None);
    let mut held = DATA.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    held.get_or_insert_with(|| {
        env::text(LAUNCH_DATA_VARIABLE).map_or_else(Vec::new, |hex| parse_launch_data(hex.to_bytes()))
    });
    held
}

/// The data one executable of the title leaves for the next (the console's
/// XamLoaderSetLaunchData): the campaign and the multiplayer are two programs here as
/// there, and it crosses from one to the other in the environment. Empty when nothing
/// left any.
pub fn launch_data() -> Vec<u8> {
    launch_data_held().clone().unwrap_or_default()
}

pub fn set_launch_data(data: &[u8]) {
    *launch_data_held() = Some(data.to_vec());
}

/// Starts the launcher to compile every shader of the game for `device`
/// (vk::pipeline::DeviceId) and then start this title, when the shaders the launcher
/// compiled do not fit this run, for the reason `why`. False when that is no help: the
/// launcher is not there, or it has just compiled for this device and they still do not
/// fit -- the run goes on then, after saying so.
pub fn compile_shaders(device: &CStr, why: &CStr) -> bool {
    let shown = why.to_string_lossy();
    // The launcher names the device it compiled for in the game it starts after a pass
    // (launcher/main.cpp).
    if env::text(c"MW2_SHADERS_COMPILED_FOR") == Some(device) {
        show_error(&format!(
            "The shaders the launcher has just compiled do not fit this game: {shown}\n\nThe game starts anyway, and \
             compiles shaders as it meets them, which stutters. Please report this (REPORT A BUG in the launcher)."
        ));
        return false;
    }
    log_info!("shaders: %s The launcher compiles them for device %s, then starts the game.", why, device);
    let title = if cfg!(mw2_title_mp) { c"multiplayer" } else { c"campaign" };
    if start_launcher(&[c"--play", title, c"--device", device]) {
        return true;
    }
    show_error(&format!(
        "{shown}\n\n{LAUNCHER} is not beside this program to compile them. The game starts anyway, and compiles shaders \
         as it meets them, which stutters."
    ));
    false
}

/// What the title asked to be started in its place, by the executable's name on the
/// disc ("default_mp.xex").
pub fn set_next_title(xex: &CStr) {
    *NEXT_TITLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(xex.to_owned());
}

/// The start itself, made once this run has let go of the window and the sound.
pub fn start_next_title() {
    let next = NEXT_TITLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    let Some(next) = next.filter(|next| !next.is_empty()) else { return };
    let Some(program) = program_for(next.to_bytes()) else {
        log_error!("the title asked for %s, which is not one of its executables", next.as_c_str());
        return;
    };
    // A development run was given its image and game folder by hand, and nothing says
    // where the other title's are.
    if !PLAYER_START.load(Ordering::Relaxed) {
        log_warn!("the title asked for %s: start that build yourself, a development run does not", next.as_c_str());
        return;
    }
    let file = std::path::absolute(program).unwrap_or_default();
    if !file.is_file() {
        show_error(&format!("{program} is not beside this program."));
        return;
    }
    let hex: String = launch_data().iter().map(|byte| format!("{byte:02x}")).collect();
    let hex = c_text(hex);
    let data = (!hex.is_empty()).then_some(hex.as_c_str());
    // It writes on in this run's log, where there is one (run.rs).
    let changes = [(LAUNCH_DATA_VARIABLE, data), (c"MW2_LOG_APPEND", Some(c"1"))];
    let path = c_path(&file);
    match spawn::start(&[&path], Some(&changes)) {
        Ok(()) => log_info!("started %s", path.as_c_str()),
        Err(error) => show_error(&format!("cannot start {}: {error}", path.to_string_lossy())),
    }
}
