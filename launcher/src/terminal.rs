// The launcher's command line: the window, or one of the terminal modes.
//
//     mw2-launcher                                  the window
//     mw2-launcher --install [<disc>] [--update <package or folder>]
//                                                   the install, in a terminal
//     mw2-launcher --upgrade                        a newer release, in a terminal
//     mw2-launcher --compile-shaders                every shader of the game, in a terminal
//     mw2-launcher --add-map <files>                a custom map (maps.rs), in a terminal
//     mw2-launcher --play campaign|multiplayer [--device <id>]
//                                                   the window, which compiles the shaders
//                                                   if they need it and starts that game
//
// With no <disc>, --install brings the install already in game/ up to date. The game
// starts the launcher with --play when the shaders compiled for it no longer fit
// (src/runtime/install/), naming the device it draws on.
#![allow(unsafe_code)]

use crate::boundary::{path_of, utf8};
use crate::play::TAKE_OVER;
use crate::precompile;
use crate::settings;
use crate::setup::{self, Outcome, Progress};
use crate::single;
use crate::update;
use crate::window::{self, Action};
use mw2_launcher::{Check, Release};
use sdl3_sys::everything as sdl;
use std::ffi::{CStr, OsString, c_char, c_int};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// `bytes` padded with spaces to `width` bytes, as printf's %-<width>s pads.
fn padded(bytes: &[u8], width: usize) -> Vec<u8> {
    let mut text = bytes.to_vec();
    if text.len() < width {
        text.resize(width, b' ');
    }
    text
}

fn out(text: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text);
    let _ = stdout.flush();
}

fn err(text: &[u8]) {
    let _ = std::io::stderr().lock().write_all(text);
}

// A line as snprintf writes one into a buffer of `room` bytes: what does not fit is cut.
fn fitted(mut line: Vec<u8>, room: usize) -> Vec<u8> {
    line.truncate(room - 1);
    line
}

fn install_in_terminal(disc: PathBuf, update: PathBuf) -> i32 {
    let mut shown: Vec<u8> = Vec::new();
    let (result, error) = window::run_install(disc, update, |job| {
        let progress = job.progress().state();
        let line = fitted(
            [
                format!("\r  {}/{}  ", progress.step, progress.steps).into_bytes(),
                padded(&progress.title, 28),
                b" ".to_vec(),
                padded(window::amount(progress.done, progress.total).as_bytes(), 18),
                b" ".to_vec(),
                padded(&progress.detail, 28),
            ]
            .concat(),
            200,
        );
        if line != shown {
            out(&line);
            shown = line;
        }
    });
    if !shown.is_empty() {
        out(b"\n");
    }
    if result != Outcome::Done {
        err(&[b"Not installed: ".as_slice(), &error, b"\n"].concat());
        if result == Outcome::NoUpdate {
            err(format!("Download it from\n  {}\nand give it with --update <file>.\n", setup::update_address())
                .as_bytes());
        }
        return 1;
    }
    out(&[b"Installed into ".as_slice(), &utf8(&absolute(Path::new(setup::GAME_FOLDER))), b".\n"].concat());
    0
}

// Adds a map from a terminal: one made for the PC game is converted, its progress shown
// as it goes.
fn add_map_in_terminal(files: Vec<PathBuf>) -> i32 {
    let mut shown: Vec<u8> = Vec::new();
    let (result, said) = window::run_add_map(files, |job| {
        let progress = job.progress().state();
        if progress.steps == 0 {
            return;
        }
        let line = fitted(
            [
                format!("  {}/{}  ", progress.step, progress.steps).into_bytes(),
                padded(&progress.title, 30),
                b" ".to_vec(),
                progress.detail.clone(),
            ]
            .concat(),
            240,
        );
        if line != shown {
            out(&[&line[..], b"\n"].concat());
            shown = line;
        }
    });
    let text = [&said[..], b"\n"].concat();
    if result == Outcome::Done {
        out(&text);
        0
    } else {
        err(&text);
        1
    }
}

// Looks for a newer version and installs it, from a terminal.
fn upgrade_in_terminal() -> i32 {
    let current = mw2_launcher::current();
    if current.is_empty() {
        err(b"This build was not made as a release, so there is no version to compare.\n");
        return 1;
    }
    let mut release = Release::default();
    match update::look_for(&mut release) {
        Ok(Check::UpToDate) => {
            out(format!("This is the newest version, {current}.\n").as_bytes());
            return 0;
        }
        Ok(Check::Failed) => {
            err(b"\n");
            return 1;
        }
        Err(error) => {
            err(format!("{error}\n").as_bytes());
            return 1;
        }
        Ok(Check::Newer) => {}
    }
    out(format!("Version {} is out; this is {current}. Installing it...\n", release.version).as_bytes());
    let progress = Progress::default();
    let (result, error) = update::install_release(&release, &progress);
    if result != Outcome::Done {
        err(format!("Not updated: {error}\n").as_bytes());
        return 1;
    }
    out(format!("Updated to {}.\n", release.version).as_bytes());
    0
}

// Compiles every shader of the game, from a terminal.
fn compile_in_terminal() -> i32 {
    let progress = std::sync::Arc::new(precompile::Progress::default());
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let result = {
        let pass = {
            let (progress, done) = (progress.clone(), done.clone());
            crate::spawn::thread(move || {
                let result = precompile::compile(b"", true, &progress);
                done.store(true, Ordering::SeqCst);
                result
            })
        };
        let mut shown = u32::MAX;
        while !done.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
            let total = progress.shaders.load(Ordering::SeqCst);
            let now = 100u32.wrapping_mul(progress.compiled.load(Ordering::SeqCst)).checked_div(total).unwrap_or(0);
            if total != 0 && now != shown {
                shown = now;
                out(format!("compiling shaders: {now}%\n").as_bytes());
            }
        }
        // A panic aborts the launcher (panic = "abort"): the pass always returns.
        pass.join().unwrap_or_else(|_| unreachable!())
    };
    if !result.ok {
        err(&[b"Shaders not compiled: ".as_slice(), &result.error, b"\n"].concat());
        return 1;
    }
    if result.no_libraries {
        out(&[
            &result.device[..],
            b" compiles shaders only with the state of a draw, so the game compiles them as it meets them\n",
        ]
        .concat());
        return 0;
    }
    out(&[
        format!("{} shaders compiled for ", result.shaders).as_bytes(),
        &result.device,
        format!(
            " in {:.1} s (reading the game's files {:.1} s), {} of them already in the cache; {} the translator \
             cannot make, {} the driver refused; a {:.0} MB cache\n",
            result.compile_seconds,
            result.read_seconds,
            result.from_cache,
            result.untranslated,
            result.refused,
            result.cache_bytes as f64 / 1e6
        )
        .as_bytes(),
    ]
    .concat());
    for (reason, count) in &result.reasons {
        out(&[format!("  {count}: ").as_bytes(), reason, b"\n"].concat());
    }
    0
}

// A path of the command line made whole against the folder the terminal is in: an
// absolute one unchanged, else the folder and the path; an empty one is none on Linux,
// the folder on Windows.
fn absolute(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() && cfg!(not(windows)) {
        return PathBuf::new();
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map(|folder| folder.join(path)).unwrap_or_default()
}

// The command line as paths. Windows hands main() the arguments in the system's code
// page, which loses what it cannot spell; the wide command line has them whole.
#[cfg(windows)]
fn arguments(_argc: c_int, _argv: *mut *mut c_char) -> Vec<OsString> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(line: *const u16, count: *mut c_int) -> *mut *mut u16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    let mut list = Vec::new();
    // SAFETY: the process's command line, split by the system into NUL-terminated wide
    // strings, freed once read.
    unsafe {
        let mut count = 0;
        let wide = CommandLineToArgvW(GetCommandLineW(), &mut count);
        if !wide.is_null() {
            for i in 1..usize::try_from(count).unwrap_or(0) {
                let text = *wide.add(i);
                let length = (0..).take_while(|&n| *text.add(n) != 0).count();
                list.push(OsString::from_wide(std::slice::from_raw_parts(text, length)));
            }
            LocalFree(wide.cast());
        }
    }
    list
}

#[cfg(not(windows))]
fn arguments(argc: c_int, argv: *mut *mut c_char) -> Vec<OsString> {
    use std::os::unix::ffi::OsStringExt;
    // SAFETY: the C runtime's arguments, as it handed them.
    (1..usize::try_from(argc).unwrap_or(0))
        .map(|i| OsString::from_vec(unsafe { CStr::from_ptr(*argv.add(i)) }.to_bytes().to_vec()))
        .collect()
}

// A player's build has no console of its own; one it was started from gets what the
// terminal mode prints, and the compile pass's log: the standard handles Rust writes to.
#[cfg(windows)]
fn attach_console() {
    use std::ffi::c_void;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
    }
    // SAFETY: the system's calls, with a NUL-terminated wide name.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$".encode_utf16().chain([0]).collect();
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if handle as isize != -1 {
                SetStdHandle(which, handle);
            }
        }
    }
}

/// The launcher's main, with the C runtime's own arguments.
pub fn run(argc: c_int, argv: *mut *mut c_char) -> i32 {
    let arguments = arguments(argc, argv);
    #[cfg(windows)]
    if !arguments.is_empty() {
        attach_console();
    }

    // The paths on the command line are the terminal's; everything a player's copy keeps
    // -- game/, saves/ -- is beside the launcher.
    let is = |i: usize, text: &str| arguments.get(i).is_some_and(|a| a.as_encoded_bytes() == text.as_bytes());
    let (mut disc, mut update) = (PathBuf::new(), PathBuf::new());
    let upgrade = arguments.len() == 1 && is(0, "--upgrade");
    let compile = arguments.len() == 1 && is(0, "--compile-shaders");
    let add_map = arguments.len() >= 2 && is(0, "--add-map");
    let mut map_files = Vec::new();
    if add_map {
        map_files = arguments[1..].iter().map(|a| absolute(Path::new(a))).collect();
    }
    // --play <title> [--device <id>]: from the game, whose shaders need compiling.
    let mut play = Action::None;
    let mut device = Vec::new();
    if arguments.len() >= 2 && is(0, "--play") {
        play = if is(1, "campaign") {
            Action::PlayCampaign
        } else if is(1, "multiplayer") {
            Action::PlayMultiplayer
        } else {
            Action::None
        };
        if arguments.len() == 4 && is(2, "--device") {
            device = utf8(Path::new(&arguments[3]));
        } else if arguments.len() != 2 {
            play = Action::None;
        }
    }
    let mut usage =
        !arguments.is_empty() && !is(0, "--install") && !upgrade && !compile && !add_map && play == Action::None;
    let mut i = 1;
    while i < arguments.len() && !usage && play == Action::None && !add_map {
        let text = utf8(Path::new(&arguments[i]));
        if text == b"--update" && i + 1 < arguments.len() {
            i += 1;
            update = absolute(Path::new(&arguments[i]));
        } else if text.first() != Some(&b'-') && disc.as_os_str().is_empty() {
            disc = absolute(Path::new(&arguments[i]));
        } else {
            usage = true;
        }
        i += 1;
    }
    // SAFETY: SDL's own string, or null.
    unsafe {
        let base = sdl::SDL_GetBasePath();
        if !base.is_null() {
            let _ = std::env::set_current_dir(path_of(CStr::from_ptr(base).to_bytes()));
        }
    }

    // What the last update left behind goes, now that the launcher it replaced has ended.
    let (updated, former) = update::finish_last();
    settings::load();
    // A release built with one online service updated to this one, which has them all:
    // the network stays the one that release had.
    if !former.is_empty() && settings::current_network() == settings::Network::None {
        settings::set_network(if former == "steam" { settings::Network::Steam } else { settings::Network::Lan });
    }
    if !arguments.is_empty() && play == Action::None {
        if usage {
            // SAFETY: the C runtime's first argument, the program as it was started.
            let me = unsafe { CStr::from_ptr(*argv) }.to_bytes();
            let mut text = Vec::new();
            for (i, line) in [
                " --install [<disc image or extracted disc folder>] [--update <package or folder>]\n",
                " --upgrade\n",
                " --compile-shaders\n",
                " --add-map <one of a map's files>...\n",
                " --play campaign|multiplayer [--device <id>]\n",
            ]
            .iter()
            .enumerate()
            {
                text.extend_from_slice(if i == 0 { b"usage: " } else { b"       " });
                text.extend_from_slice(me);
                text.extend_from_slice(line.as_bytes());
            }
            err(&text);
            return 2;
        }
        if upgrade {
            return upgrade_in_terminal();
        }
        if compile {
            return compile_in_terminal();
        }
        if add_map {
            return add_map_in_terminal(map_files);
        }
        return install_in_terminal(disc, update);
    }
    // One window a copy (single.rs): a second start brings the first up and ends. The one
    // an update started waits for the old one to end; one the game started for its
    // shaders does that work whatever else is open.
    let take_over = std::env::var_os(TAKE_OVER).is_some();
    let name = std::ffi::CString::new(TAKE_OVER).unwrap();
    // SAFETY: a C string; nothing else runs yet that reads the environment.
    unsafe { sdl::SDL_unsetenv_unsafe(name.as_ptr()) }; // not for what this one starts
    if !single::system::claim(take_over) && play == Action::None && !take_over {
        single::system::wake();
        return 0;
    }
    window::run(updated, play, device)
}
