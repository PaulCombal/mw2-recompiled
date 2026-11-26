// The programs beside the launcher: starting one -- the campaign, the multiplayer, or the
// launcher an update installed -- and whether the campaign or the multiplayer is running,
// since the profile is theirs to write while they are, and they write it when they end.
#![deny(unsafe_code)]

use crate::spawn;
use std::path::Path;

#[cfg(windows)]
pub const CAMPAIGN: &str = "mw2-sp.exe";
#[cfg(windows)]
pub const MULTIPLAYER: &str = "mw2-mp.exe";
#[cfg(windows)]
pub const LAUNCHER: &str = "mw2-launcher.exe";
#[cfg(not(windows))]
pub const CAMPAIGN: &str = "mw2-sp";
#[cfg(not(windows))]
pub const MULTIPLAYER: &str = "mw2-mp";
#[cfg(not(windows))]
pub const LAUNCHER: &str = "mw2-launcher";

/// The launcher an update starts is told so: the old one is still there for a moment, and
/// holds the window (single.rs).
pub const TAKE_OVER: &str = "MW2_LAUNCHER_TAKES_OVER";

/// Starts a program beside the launcher and leaves it running. A game is told the device
/// its shaders were compiled for (`shaders_for`), so that it does not start the launcher
/// again for them (src/runtime/install/).
pub fn start(program: &str, shaders_for: &[u8]) -> Result<(), String> {
    let mut set: Vec<(&str, &[u8])> = Vec::new();
    if !shaders_for.is_empty() {
        set.push(("MW2_SHADERS_COMPILED_FOR", shaders_for));
    }
    if program == LAUNCHER {
        set.push((TAKE_OVER, b"1"));
    }
    spawn::start(Path::new(program), &set)
}

/// Whether the campaign or the multiplayer is running: on Linux, a process whose program
/// is the file beside the launcher; on Windows, one of their names.
#[cfg(not(windows))]
pub fn game_running() -> bool {
    use std::fs;
    use std::path::PathBuf;
    // std::filesystem::weakly_canonical: the part that is there resolved, the rest after it.
    fn weakly_canonical(path: &Path) -> PathBuf {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if let Ok(resolved) = fs::canonicalize(&path) {
            return resolved;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => weakly_canonical(parent).join(name),
            _ => path,
        }
    }
    let programs = [weakly_canonical(Path::new(CAMPAIGN)), weakly_canonical(Path::new(MULTIPLAYER))];
    let Ok(processes) = fs::read_dir("/proc") else { return false };
    processes.flatten().any(|process| {
        let name = process.file_name();
        let name = name.as_encoded_bytes();
        !name.is_empty()
            && name.iter().all(u8::is_ascii_digit)
            && fs::read_link(process.path().join("exe")).is_ok_and(|program| programs.contains(&program))
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn game_running() -> bool {
    use std::ffi::c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    // PROCESSENTRY32W.
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        process: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }

    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut Entry) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut Entry) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    // SAFETY: a snapshot of the processes, walked and closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: Entry = std::mem::zeroed();
        entry.size = size_of::<Entry>() as u32;
        let mut running = false;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more && !running {
            let length = entry.exe.iter().position(|&unit| unit == 0).unwrap_or(entry.exe.len());
            let name = String::from_utf16_lossy(&entry.exe[..length]);
            running = name.eq_ignore_ascii_case(CAMPAIGN) || name.eq_ignore_ascii_case(MULTIPLAYER);
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        running
    }
}
