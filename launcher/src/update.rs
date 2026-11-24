// A newer release of these programs (launcher/update.h says what for): looked for when
// the player asks, and put in place of the running ones.
//
// GitHub describes the newest published release at a fixed address: its tag, and each
// archive's name, size, SHA-256 and address, which release.rs reads and compares with
// this build's (the launcher's library target, so that its tests run). The archive for
// this build's system is downloaded (download.rs), checked, unpacked with the system's
// tar, and its files swapped in; the ones they replace wait in update/ until the next
// start removes it, since a running program can be moved aside but not deleted. Starting the new launcher,
// and its waiting for this one to end (MW2_LAUNCHER_TAKES_OVER), are the window's
// (main.cpp, single.cpp).
//
// The window and the terminal mode call it through update.h's functions (update.cpp),
// the install from a thread of its own, reporting to the same setup::Progress an install
// does (setup.rs's Shown); what a player reads is word for word what the C++ said. All of
// it works in the launcher's folder, which main.cpp makes the current one first.
#![deny(unsafe_code)]

use crate::download::download;
use crate::setup::{Outcome, Shown, message};
use mw2_install::crypto::Sha256;
use mw2_launcher::{CURRENT, Check, EXTENSION, KIND, Release, judge, kind, read_description};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

// GitHub's description of the newest published release: its tag, and for each archive
// its name, size, SHA-256 and address.
const NEWEST: &str = "https://api.github.com/repos/PaulCombal/mw2-recompiled/releases/latest";
const FOLDER: &str = "update";

// Holds the version, for the next start to say.
fn done_file() -> PathBuf {
    Path::new(FOLDER).join("installed")
}

// MW2_UPDATE_URL names another description of the same form, a mirror's or a test's file.
fn newest_url() -> String {
    std::env::var("MW2_UPDATE_URL").ok().filter(|url| !url.is_empty()).unwrap_or_else(|| NEWEST.to_string())
}

// The release the description gives, none when it could not be had.
fn look() -> (Option<Release>, Result<Check, String>) {
    let _ = fs::create_dir_all(FOLDER);
    let file = Path::new(FOLDER).join("newest.json");
    if let Err(why) = download(&newest_url(), &file) {
        return (None, Err(format!("The newest version could not be looked up: {why}.")));
    }
    // Not UTF-8 is no description: the tag found in it is then none.
    let json = fs::read(&file).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
    let _ = fs::remove_dir_all(FOLDER);
    let release = read_description(&json);
    let check = judge(&release);
    (Some(release), check)
}

fn sha256_of(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut block) {
            Ok(0) => return Some(hash.finish_hex()),
            Ok(read) => hash.update(&block[..read]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

// Whether this program has this standard stream to hand on. A Windows program opened
// from the desktop has none, and one it would pass on that is not there fails the start
// (spawn.cpp's Has, which the C++ started tar through): those are left unconnected
// instead. Elsewhere a stream is always handed on.
#[cfg(windows)]
#[allow(unsafe_code)]
fn has_stream(which: u32) -> bool {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetFileType(handle: *mut c_void) -> u32;
        fn SetLastError(code: u32);
        fn GetLastError() -> u32;
    }
    const FILE_TYPE_UNKNOWN: u32 = 0;
    // SAFETY: plain calls on this process's own standard handles; GetFileType only
    // reads the handle it is given, which may be any value.
    unsafe {
        let handle = GetStdHandle(which);
        if handle.is_null() || handle as isize == -1 {
            return false;
        }
        SetLastError(0);
        GetFileType(handle) != FILE_TYPE_UNKNOWN || GetLastError() == 0
    }
}

fn stream(which: u32) -> std::process::Stdio {
    #[cfg(windows)]
    if !has_stream(which) {
        return std::process::Stdio::null();
    }
    let _ = which;
    std::process::Stdio::inherit()
}

// Windows 10 and later have tar as every other system does, and theirs reads a .zip.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    use std::process::Command;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE: (DWORD)-10, -11, -12.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(stream(-10i32 as u32))
        .stdout(stream(-11i32 as u32))
        .stderr(stream(-12i32 as u32))
        .status()
        .map_err(|_| "The update could not be unpacked: there is no tar program on this system.".to_string())?;
    // As SDL's process wait reports one a signal ended: the signal's number, negated.
    #[cfg(unix)]
    let code = {
        use std::os::unix::process::ExitStatusExt;
        status.code().or(status.signal().map(|signal| -signal)).unwrap_or(-1)
    };
    #[cfg(not(unix))]
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        return Err(format!("The update could not be unpacked (tar's exit code {code})."));
    }
    Ok(())
}

// Why an update stopped: what Install returns but Done, and what the player reads.
enum Stop {
    Cancelled,
    Failed(String),
}

fn install(release: &Release, shown: Shown) -> Result<(), Stop> {
    let _ = fs::remove_dir_all(FOLDER);
    let folder = Path::new(FOLDER);
    let (fresh, old, archive) = (folder.join("new"), folder.join("old"), folder.join(&release.archive));
    if let Err(e) = fs::create_dir_all(&fresh).and_then(|()| fs::create_dir_all(&old)) {
        return Err(Stop::Failed(format!("Cannot write beside the launcher: {}", message(&e))));
    }
    // What a stop leaves: nothing.
    let result = swap_in(release, shown, &fresh, &old, &archive);
    if result.is_err() {
        let _ = fs::remove_dir_all(FOLDER);
    }
    result
}

fn swap_in(release: &Release, shown: Shown, fresh: &Path, old: &Path, archive: &Path) -> Result<(), Stop> {
    shown.begin(3);
    shown.step(1, "Downloading the new version");
    shown.detail(&format!("{:.0} MB", release.size as f64 / 1e6));
    if let Err(why) = download(&release.url, archive) {
        return Err(Stop::Failed(format!("The new version could not be downloaded: {why}.")));
    }
    if shown.cancelled() {
        return Err(Stop::Cancelled);
    }

    shown.step(2, "Checking it");
    if sha256_of(archive).is_none_or(|sha256| sha256 != release.sha256) {
        return Err(Stop::Failed("The download is not the file the release describes. Try again.".into()));
    }
    unpack(archive, fresh).map_err(Stop::Failed)?;
    // The archive holds one folder, with the files in it.
    let nothing = || Stop::Failed("The update holds nothing to install.".into());
    let folder = fs::read_dir(fresh)
        .ok()
        .and_then(|entries| entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).last())
        .ok_or_else(nothing)?;
    if shown.cancelled() {
        return Err(Stop::Cancelled);
    }

    // Past here nothing is asked: a swap is quick, and half of one is no place to stop.
    shown.step(3, "Installing");
    // The name, and whether one was there before.
    let mut swapped: Vec<(PathBuf, bool)> = Vec::new();
    let undo = |swapped: &[(PathBuf, bool)]| {
        for (name, there) in swapped.iter().rev() {
            let _ = fs::remove_file(name);
            if *there {
                let _ = fs::rename(old.join(name), name);
            }
        }
    };
    let entries = fs::read_dir(&folder).map(|entries| entries.flatten().collect::<Vec<_>>()).unwrap_or_default();
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = PathBuf::from(entry.file_name());
        let there = name.exists();
        let mut failed = if there { fs::rename(&name, old.join(&name)).err() } else { None };
        if failed.is_none() {
            failed = fs::rename(&path, &name).err();
            if failed.is_some() && there {
                let _ = fs::rename(old.join(&name), &name);
            }
        }
        if let Some(e) = failed {
            undo(&swapped);
            return Err(Stop::Failed(format!("Cannot replace {}: {}", name.to_string_lossy(), message(&e))));
        }
        swapped.push((name, there));
    }
    if swapped.is_empty() {
        return Err(nothing());
    }
    let _ = fs::write(done_file(), &release.version);
    Ok(())
}

// What the last update left goes: the version it installed when this is the first start
// since, and the online service of the release it was installed by, from the archive
// that launcher took: "steam" or "lan" until v0.7.x, which were built with one each, and
// empty since.
fn finish() -> (String, String) {
    // The first word of the file, as the C++ read it.
    let version = fs::read(done_file())
        .map(|bytes| String::from_utf8_lossy(&bytes).split_whitespace().next().unwrap_or("").to_string())
        .unwrap_or_default();
    let mut former = String::new();
    if let Ok(entries) = fs::read_dir(FOLDER) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            for service in ["steam", "lan"] {
                let end = format!("-{}-{service}{EXTENSION}", kind());
                if name.len() > end.len() && name.ends_with(&end) {
                    former = service.to_string();
                }
            }
        }
    }
    // The launcher this one replaced may still be closing, and its file cannot go
    // before it has: the next start tries again.
    let _ = fs::remove_dir_all(FOLDER);
    (version, former)
}

// ---- what update.cpp calls -------------------------------------------------------
// As setup.rs's: text crosses as bytes and a length, and comes back into the C++'s
// std::strings through mw2l_setup_text.

#[allow(unsafe_code)]
mod exports {
    use super::*;
    use crate::setup::Progress;
    use crate::setup::exports::{bytes, say};
    use std::ffi::{c_char, c_int, c_void};

    /// update::Current: a NUL-terminated string that lives as long as the program.
    #[unsafe(no_mangle)]
    pub extern "C" fn mw2_update_current() -> *const c_char {
        CURRENT.as_ptr().cast()
    }

    /// update::Kind: a NUL-terminated string that lives as long as the program.
    #[unsafe(no_mangle)]
    pub extern "C" fn mw2_update_kind() -> *const c_char {
        KIND.as_ptr().cast()
    }

    /// update::Look: an update::Check, the release's facts in the four std::strings
    /// and `size`, and `error` set for Failed. As the C++ did, a description that could
    /// not be had changes none of them, and one without this system's archive only the
    /// version.
    ///
    /// # Safety
    /// Every pointer is the C++'s update::Release's field or std::string, alive until
    /// it returns.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mw2_update_look(
        version: *mut c_void,
        archive: *mut c_void,
        sha256: *mut c_void,
        url: *mut c_void,
        size: *mut u64,
        error: *mut c_void,
    ) -> c_int {
        let (release, check) = look();
        if let Some(release) = release {
            say(version, &release.version);
            // A matched archive's name is never empty.
            if !release.archive.is_empty() {
                say(archive, &release.archive);
                say(sha256, &release.sha256);
                say(url, &release.url);
                unsafe { *size = release.size };
            }
        }
        match check {
            Ok(check) => check as c_int,
            Err(text) => {
                say(error, &text);
                Check::Failed as c_int
            }
        }
    }

    /// update::Install: a setup::Result, and `error`, a std::string, set for Failed.
    ///
    /// # Safety
    /// The texts point at their lengths' bytes (none when 0), `progress` at the C++'s
    /// setup::Progress and `error` at a std::string, all alive until it returns.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mw2_update_install(
        version: *const u8,
        version_length: usize,
        archive: *const u8,
        archive_length: usize,
        sha256: *const u8,
        sha256_length: usize,
        url: *const u8,
        url_length: usize,
        size: u64,
        progress: *mut Progress,
        error: *mut c_void,
    ) -> c_int {
        let text = |data, length| unsafe { String::from_utf8_lossy(bytes(data, length)).into_owned() };
        let release = Release {
            version: text(version, version_length),
            archive: text(archive, archive_length),
            sha256: text(sha256, sha256_length),
            url: text(url, url_length),
            size,
        };
        let outcome = match super::install(&release, Shown(progress)) {
            Ok(()) => Outcome::Done,
            Err(Stop::Cancelled) => Outcome::Cancelled,
            Err(Stop::Failed(text)) => {
                say(error, &text);
                Outcome::Failed
            }
        };
        outcome as c_int
    }

    /// update::Finish: the version the last update installed, or empty, and `former`.
    ///
    /// # Safety
    /// `version` and `former` point at std::strings alive until it returns.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mw2_update_finish(version: *mut c_void, former: *mut c_void) {
        let (installed, service) = finish();
        say(version, &installed);
        say(former, &service);
    }
}
