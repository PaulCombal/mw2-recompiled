// A bug report: one run of the game with its log kept, and what a reader needs to know
// about the machine.
//
// The game is started with MW2_REPORT=1 and MW2_LOG_FILE (src/runtime/report.rs), so its
// log names the graphics driver and ends with how the run performed. When it has ended,
// the log -- with what says who the player is taken out (scrub.rs), since an issue is
// public -- goes into one text file under reports/ with the system's description on top.
// GitHub's new-issue page is opened with that description filled in; the launcher has no
// account to file the issue with, and a page cannot be handed a file, so the player drags
// the file in and submits.
#![deny(unsafe_code)]

use crate::boundary::utf8;
use crate::scrub::{self, Folders};
use crate::{clock, setup, spawn};
use std::fs;
use std::path::{Path, PathBuf};

const FOLDER: &str = "reports";
const LOG: &str = "reports/run.log";
const NEW_ISSUE: &str = "https://github.com/PaulCombal/mw2-recompiled/issues/new";

/// Starts `program`, beside the launcher, with its log kept; `shaders_for` is the device
/// its shaders were compiled for (play.rs).
pub fn start(program: &str, shaders_for: &[u8]) -> Result<(), String> {
    let _ = fs::create_dir_all(FOLDER);
    let _ = fs::remove_file(LOG);
    let log = utf8(&std::path::absolute(LOG).unwrap_or_else(|_| PathBuf::from(LOG)));
    let mut set: Vec<(&str, &[u8])> = vec![("MW2_REPORT", b"1"), ("MW2_LOG_FILE", &log)];
    if !shaders_for.is_empty() {
        set.push(("MW2_SHADERS_COMPILED_FOR", shaders_for));
    }
    spawn::start(Path::new(program), &set).map_err(|error| format!("{program} could not be started: {error}"))
}

/// Whether a run was recorded and no report made of it: the launcher was ended with the
/// game, as Steam's "Exit game" does on a Steam Deck.
pub fn pending() -> bool {
    Path::new(LOG).is_file()
}

/// Forgets that run.
pub fn discard() {
    let _ = fs::remove_file(LOG);
}

/// The report, and the new-issue page's address, filled in.
pub struct Made {
    pub file: PathBuf,
    pub page: String,
}

// A folder both ways a log can spell it: with the system's separators, and with forward
// slashes. Nothing for no folder.
fn spellings(folder: &[u8]) -> Vec<Vec<u8>> {
    if folder.is_empty() {
        return Vec::new();
    }
    let native = if cfg!(windows) {
        folder.iter().map(|&b| if b == b'/' { b'\\' } else { b }).collect()
    } else {
        folder.to_vec()
    };
    let generic = if cfg!(windows) {
        folder.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect()
    } else {
        folder.to_vec()
    };
    vec![native, generic]
}

fn folders() -> Folders {
    let here = std::env::current_dir().map(|here| utf8(&here)).unwrap_or_default();
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| home.as_encoded_bytes().to_vec())
        .unwrap_or_default();
    Folders { here: spellings(&here), home: spellings(&home) }
}

/// After the game has ended.
pub fn make() -> Result<Made, String> {
    let raw =
        fs::read(LOG).map_err(|_| "The game left no log, so there is nothing to report. Did it start?".to_string())?;
    let log = scrub::scrub(&raw, &folders());

    let find = |what: &[u8]| log.windows(what.len()).position(|window| window == what);
    // The log's first line says which game wrote it.
    let what = if find(b"runtime (multiplayer").unwrap_or(usize::MAX) < find(b"\n").unwrap_or(usize::MAX) {
        "Multiplayer"
    } else {
        "Campaign"
    };
    let fault = find(b"[E] ----").is_some();
    let mut version = match mw2_launcher::current() {
        "" => "a development build".to_string(),
        current => current.to_string(),
    };
    if !mw2_launcher::kind().is_empty() {
        version += &format!(" {}", mw2_launcher::kind());
    }
    let mut graphics = scrub::first_line(&log, b"] report: graphics ");
    if graphics.is_empty() {
        graphics = b"unknown (the game did not get as far as the graphics device)";
    }

    // Bytes: what comes out of the log is as the log has it.
    let mut system = Vec::new();
    let mut line = |text: String| system.extend_from_slice(text.as_bytes());
    line(format!("- Version: {version}{}\n", if setup::USES_UPDATE { ", title update 6" } else { ", disc version" }));
    line(format!("- Game: {what}{}\n", if fault { ", the run ended on a fault" } else { ", the run ended normally" }));
    line(format!("- System: {}\n", machine::system()));
    line(format!("- Processor: {}\n", machine::processor()));
    line(format!("- Memory: {} GB\n", (machine::memory() + 512) / 1024));
    #[cfg(windows)]
    line(format!("- Memory Integrity: {}\n", machine::memory_integrity()));
    line(format!("- Power: {}\n", machine::power()));
    system.extend_from_slice(b"- Graphics: ");
    system.extend_from_slice(graphics);
    system.push(b'\n');
    system.extend_from_slice(format!("- Display: {}\n", machine::displays()).as_bytes());
    let performance = scrub::performance(&log);

    let stamp = clock::local(clock::now()).map(|local| clock::stamp(&local)).unwrap_or_default();
    let name = format!("mw2-report-{stamp}.txt");
    let relative = Path::new(FOLDER).join(&name);
    let file = std::path::absolute(&relative).unwrap_or(relative);
    let mut text = b"Modern Warfare 2 recompiled: bug report\n\n".to_vec();
    text.extend_from_slice(&system);
    text.extend_from_slice(&performance);
    text.extend_from_slice(b"\n---- log ----\n");
    text.extend_from_slice(&log);
    fs::write(&file, text)
        .map_err(|_| format!("The report could not be written to {}.", String::from_utf8_lossy(&utf8(&file))))?;
    // The log as the game wrote it says who the player is.
    let _ = fs::remove_file(LOG);

    // The body given here takes the place of the repository's own text for a new issue
    // (.github/ISSUE_TEMPLATE/issue.md), which sends people to this.
    let mut body = format!(
        "**What happened?**\n\n<!-- Describe the problem here: what you did, what you expected, what you saw. -->\n\n\n\
         **The report file**\n\n<!-- Drag {name} into this box. The launcher opened the folder it is in. -->\n\n\n\
         **System** (filled in by the launcher)\n\n"
    )
    .into_bytes();
    body.extend_from_slice(&system);
    body.extend_from_slice(&performance);
    let title = format!("{what}{}", if fault { ": crash " } else { ": " });
    let page = format!(
        "{NEW_ISSUE}?template=issue.md&title={}&body={}",
        scrub::encoded(title.as_bytes()),
        scrub::encoded(&body)
    );
    Ok(Made { file, page })
}

/// Shows the page and the folder holding the file. False when the browser could not be
/// opened.
#[allow(unsafe_code)]
pub fn open(made: &Made) -> bool {
    use sdl3_sys::everything as sdl;
    use std::ffi::CString;
    let folder = utf8(made.file.parent().unwrap_or(Path::new("")));
    let generic: Vec<u8> = folder.iter().map(|&b| if cfg!(windows) && b == b'\\' { b'/' } else { b }).collect();
    let folder = CString::new(scrub::file_address(&generic)).unwrap_or_default();
    let page = CString::new(made.page.as_str()).unwrap_or_default();
    // SAFETY: C strings.
    unsafe {
        sdl::SDL_OpenURL(folder.as_ptr());
        sdl::SDL_OpenURL(page.as_ptr())
    }
}

// What the report says of the machine.
#[allow(unsafe_code)]
mod machine {
    use sdl3_sys::everything as sdl;

    /// The system's memory in MB, as SDL counts it.
    pub fn memory() -> i32 {
        // SAFETY: a plain call.
        unsafe { sdl::SDL_GetSystemRAM() }
    }

    /// A laptop on its battery runs its processor slower.
    pub fn power() -> String {
        let mut percent = -1;
        // SAFETY: an int to fill.
        let state = unsafe { sdl::SDL_GetPowerInfo(std::ptr::null_mut(), &mut percent) };
        if state == sdl::SDL_POWERSTATE_ON_BATTERY {
            if percent >= 0 { format!("on battery ({percent}%)") } else { "on battery".into() }
        } else if state == sdl::SDL_POWERSTATE_NO_BATTERY {
            "no battery".into()
        } else if state == sdl::SDL_POWERSTATE_CHARGING || state == sdl::SDL_POWERSTATE_CHARGED {
            "plugged in".into()
        } else {
            "unknown".into()
        }
    }

    pub fn displays() -> String {
        let mut all = String::new();
        let mut count = 0;
        // SAFETY: SDL's list, read and freed; each mode SDL's own.
        unsafe {
            let displays = sdl::SDL_GetDisplays(&mut count);
            if !displays.is_null() {
                for i in 0..count.max(0) as usize {
                    let mode = sdl::SDL_GetCurrentDisplayMode(*displays.add(i));
                    if let Some(mode) = mode.as_ref() {
                        let comma = if all.is_empty() { "" } else { ", " };
                        all += &format!("{comma}{}x{} at {:.0} Hz", mode.w, mode.h, mode.refresh_rate);
                    }
                }
                sdl::SDL_free(displays.cast());
            }
        }
        if all.is_empty() { "unknown".into() } else { all }
    }

    fn threads() -> i32 {
        // SAFETY: a plain call.
        unsafe { sdl::SDL_GetNumLogicalCPUCores() }
    }

    fn named(mut name: String) -> String {
        name.truncate(name.trim_end_matches(' ').len());
        if name.is_empty() {
            name = "unknown".into();
        }
        format!("{name}, {} threads", threads())
    }

    #[cfg(not(windows))]
    pub fn system() -> String {
        use std::ffi::{CStr, c_char, c_int};
        // struct utsname, Linux's: six fields of 65 bytes.
        #[repr(C)]
        struct Utsname {
            fields: [[c_char; 65]; 6],
        }
        unsafe extern "C" {
            fn uname(name: *mut Utsname) -> c_int;
        }
        let mut name = String::from("Linux");
        let release = std::fs::read("/etc/os-release").unwrap_or_default();
        for line in release.split(|&byte| byte == b'\n') {
            if let Some(pretty) = line.strip_prefix(b"PRETTY_NAME=") {
                name = String::from_utf8_lossy(pretty).replace('"', "");
            }
        }
        let mut kernel = Utsname { fields: [[0; 65]; 6] };
        // SAFETY: a struct utsname to fill; its fields end in a zero.
        if unsafe { uname(&mut kernel) } == 0 {
            let release = unsafe { CStr::from_ptr(kernel.fields[2].as_ptr()) };
            name += &format!(", kernel {}", release.to_string_lossy());
        }
        let desktop = std::env::var_os("XDG_CURRENT_DESKTOP").map(|value| value.to_string_lossy().into_owned());
        let session = std::env::var_os("XDG_SESSION_TYPE").map(|value| value.to_string_lossy().into_owned());
        if desktop.is_some() || session.is_some() {
            let on = if desktop.is_some() && session.is_some() { " on " } else { "" };
            name += &format!(", {}{on}{}", desktop.unwrap_or_default(), session.unwrap_or_default());
        }
        name
    }

    #[cfg(not(windows))]
    pub fn processor() -> String {
        let cpu = std::fs::read("/proc/cpuinfo").unwrap_or_default();
        let name = cpu
            .split(|&byte| byte == b'\n')
            .find(|line| line.starts_with(b"model name") && line.contains(&b':'))
            .map(|line| {
                let colon = line.iter().position(|&byte| byte == b':').unwrap_or(0);
                String::from_utf8_lossy(line.get(colon + 2..).unwrap_or_default()).into_owned()
            })
            .unwrap_or_default();
        named(name)
    }

    #[cfg(windows)]
    mod windows {
        use std::ffi::c_void;

        pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
        pub const RRF_RT_REG_SZ: u32 = 0x2;
        pub const RRF_RT_REG_DWORD: u32 = 0x18;
        pub const ERROR_SUCCESS: i32 = 0;

        unsafe extern "system" {
            pub fn RegGetValueW(
                key: isize,
                subkey: *const u16,
                value: *const u16,
                flags: u32,
                kind: *mut u32,
                data: *mut c_void,
                size: *mut u32,
            ) -> i32;
            pub fn GetModuleHandleW(name: *const u16) -> *mut c_void;
            pub fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        }

        pub fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(Some(0)).collect()
        }
    }

    #[cfg(windows)]
    fn registry(key: &str, value: &str) -> String {
        use windows::*;
        let mut text = [0u16; 256];
        let mut size = size_of_val(&text) as u32;
        let (key, value) = (wide(key), wide(value));
        // SAFETY: wide strings and a buffer of `size` bytes.
        let found = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                text.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if found != ERROR_SUCCESS {
            return String::new();
        }
        let length = text.iter().position(|&unit| unit == 0).unwrap_or(text.len());
        String::from_utf16_lossy(&text[..length])
    }

    #[cfg(windows)]
    pub fn system() -> String {
        let key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
        let mut name = registry(key, "ProductName");
        let build = registry(key, "CurrentBuild");
        // Windows 11 still calls itself 10 there; the build number tells them apart.
        if crate::cnum::atoi(build.as_bytes()) >= 22000 {
            name = name.replace("Windows 10", "Windows 11");
        }
        format!("{name} {} (build {build})", registry(key, "DisplayVersion"))
    }

    #[cfg(windows)]
    pub fn processor() -> String {
        named(registry("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0", "ProcessorNameString"))
    }

    /// Windows' Memory Integrity runs the kernel under the hypervisor, which makes every
    /// change of a page's protection dearer, and the game's memory watch makes thousands a
    /// second. The system says whether it is running; the setting is the fallback.
    #[cfg(windows)]
    pub fn memory_integrity() -> String {
        use std::ffi::c_void;
        use windows::*;
        #[repr(C)]
        struct CodeIntegrity {
            length: u32,
            options: u32,
        }
        const CODE_INTEGRITY_INFORMATION: u32 = 103; // SystemCodeIntegrityInformation
        const HYPERVISOR_ENFORCED: u32 = 0x400; // CODEINTEGRITY_OPTION_HVCI_KMCI_ENABLED
        type Query = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;
        // SAFETY: ntdll's export, called as it is declared.
        unsafe {
            let query = GetProcAddress(
                GetModuleHandleW(wide("ntdll.dll").as_ptr()),
                c"NtQuerySystemInformation".as_ptr().cast(),
            );
            if !query.is_null() {
                let query: Query = std::mem::transmute(query);
                let mut information = CodeIntegrity { length: size_of::<CodeIntegrity>() as u32, options: 0 };
                if query(
                    CODE_INTEGRITY_INFORMATION,
                    (&raw mut information).cast(),
                    size_of::<CodeIntegrity>() as u32,
                    std::ptr::null_mut(),
                ) >= 0
                {
                    return if information.options & HYPERVISOR_ENFORCED != 0 { "on" } else { "off" }.into();
                }
            }
            let (mut on, mut size) = (0u32, 4u32);
            let key =
                wide("SYSTEM\\CurrentControlSet\\Control\\DeviceGuard\\Scenarios\\HypervisorEnforcedCodeIntegrity");
            if RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                wide("Enabled").as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut on).cast(),
                &mut size,
            ) != ERROR_SUCCESS
            {
                return "off (not set)".into();
            }
            if on != 0 { "on (set; the system did not say whether it runs)" } else { "off" }.into()
        }
    }
}
