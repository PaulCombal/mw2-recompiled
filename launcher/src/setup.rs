// Installing the game (launcher/setup.h says what for): the files from the player's disc
// into game/ beside the launcher, and title update 6 applied to the two executables,
// which is the version the game executables are built from. A build of the disc
// version (mw2_version_tu0) installs the disc as it is.
//
// The window and the terminal mode run it through setup::Run (setup.cpp), on a thread of
// their own; this reports its progress there, through the mw2l_* calls, to the
// setup::Progress the window draws, and hands its error back as text. The disc, the
// package and the executables are read with the install crate (install/), as the game
// and cargo xtask read them. What a player reads is word for word what the C++ install
// said.
#![deny(unsafe_code)]

#[cfg(not(mw2_version_tu0))]
use crate::download::download;
use mw2_install::xdvdfs::Disc;
use mw2_install::{crypto, xex};
#[cfg(not(mw2_version_tu0))]
use mw2_install::{patch, stfs};
#[cfg(not(mw2_version_tu0))]
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const GAME_FOLDER: &str = "game";
const TITLE_ID: u32 = 0x41560817;

// The disc's two executables, and what the update makes of them, by the SHA-256 of
// the image each decrypts to, which build.rs reads from mw2/. The game executables run
// only the code they were recompiled from, so another version is refused here rather
// than there. Every region's disc has that code; only the signed headers differ.
struct Title {
    xex: &'static str,
    // The update's patch for it, which a disc-version build does not apply.
    #[cfg_attr(mw2_version_tu0, allow(dead_code))]
    patch: &'static str,
    disc: &'static str,
    installed: &'static str,
}

const TITLES: [Title; 2] = [
    Title {
        xex: "default.xex",
        patch: "default.xexp",
        disc: env!("MW2_DISC_SHA256_SP"),
        installed: env!("MW2_XEX_SHA256_SP"),
    },
    Title {
        xex: "default_mp.xex",
        patch: "default_mp.xexp",
        disc: env!("MW2_DISC_SHA256_MP"),
        installed: env!("MW2_XEX_SHA256_MP"),
    },
];

/// True in a build that installs an update at all.
const USES_UPDATE: bool = cfg!(not(mw2_version_tu0));

// The discs whose update is known, by the media id in their executables' headers. Each
// language's update is its own package, for its disc alone: the same patches to the
// code, and fastfiles in its language, which the title refuses beside a disc in another
// ("Language mismatch on download content").
#[cfg(not(mw2_version_tu0))]
struct KnownDisc {
    media_id: u32,
    name: &'static str,
    // The package's name, which it keeps beside the launcher too.
    update_name: &'static str,
    update_url: &'static str,
    // The fastfiles the update adds, which go beside the disc's, by their SHA-256.
    added: [(&'static str, &'static str); 3],
}

// Title update 6, from the CoD Xenon project's archive of title updates (its
// maintainer's suggestion) -- for now from a fork of it that adds the other languages'
// (codxenon/xbox360-title-updates#1) -- at a commit of our choosing so a file cannot
// change under its checksum. The path in the archive is URL-encoded.
#[cfg(not(mw2_version_tu0))]
macro_rules! update_url {
    ($path:literal) => {
        concat!(
            "https://raw.githubusercontent.com/PaulCombal/xbox360-title-updates/",
            "b053668bfd9d3b59314f04a348004c186f6bf29b/games/",
            $path
        )
    };
}

#[cfg(not(mw2_version_tu0))]
const DISCS: [KnownDisc; 2] = [
    // The disc the build is made from: its update's files are in mw2/update, which
    // build.rs hashes.
    KnownDisc {
        media_id: 0x2C8C0267,
        name: "USA/Europe",
        update_name: "TU_10LC20N_0000018000000.00000000001GA",
        update_url: update_url!(
            "Call%20of%20Duty%20-%20Modern%20Warfare%202%20%28USA%2C%20Europe%29/TU6/TU_10LC20N_0000018000000.00000000001GA"
        ),
        added: [
            ("patch_mp.ff", env!("MW2_UPDATE_SHA256_PATCH_MP")),
            ("dlc1_ui_mp.ff", env!("MW2_UPDATE_SHA256_DLC1_UI_MP")),
            ("dlc2_ui_mp.ff", env!("MW2_UPDATE_SHA256_DLC2_UI_MP")),
        ],
    },
    KnownDisc {
        media_id: 0x11D15B16,
        name: "Spanish",
        update_name: "TU_10LC20N_000001O000000.00000000001GE",
        update_url: update_url!(
            "Call%20of%20Duty%20-%20Modern%20Warfare%202%20%28Spain%29/TU6/TU_10LC20N_000001O000000.00000000001GE"
        ),
        added: [
            ("patch_mp.ff", "ecf3e1c4aa8aad35d3ab6b974a6794def2cc2c5b015710738af79a8506971e4a"),
            ("dlc1_ui_mp.ff", "35d619573063714eb4e4c114bd945d85769d85ca3f02a931ebaf0fb395a5da63"),
            ("dlc2_ui_mp.ff", "0120e9001fc82ef774c76e6559bf05ff573a078e2b16f75b777dd806d623f41a"),
        ],
    },
];

// The disc of the last install, by its place in DISCS, whose update a player may have
// to fetch: the first until an install has read one.
#[cfg(not(mw2_version_tu0))]
static LAST_DISC: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Where the update of the last install's disc is downloaded from; empty in a build
/// that installs none.
fn update_url() -> &'static str {
    #[cfg(not(mw2_version_tu0))]
    return DISCS[LAST_DISC.load(std::sync::atomic::Ordering::Relaxed).min(DISCS.len() - 1)].update_url;
    #[cfg(mw2_version_tu0)]
    return "";
}

// setup::State and setup::Result, by their places in setup.h's enums.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    NotInstalled,
    NeedsUpdate,
    Installed,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Done,
    Cancelled,
    Failed,
    NoUpdate,
}

// Why an install stopped: what Run returns but Done, and what the player reads.
enum Stop {
    Cancelled,
    Failed(String),
    // The update could not be downloaded: the player supplies the file.
    #[cfg_attr(mw2_version_tu0, allow(dead_code))]
    NoUpdate(String),
}

use Stop::{Cancelled, Failed};

// setup::Progress, which the C++ owns.
#[repr(C)]
pub struct Progress {
    _opaque: [u8; 0],
}

// The install's handle on it, which every call below is handed with; the self-update
// (update.rs) reports through it too.
#[derive(Clone, Copy)]
pub(crate) struct Shown(pub(crate) *mut Progress);

// ---- the C side ------------------------------------------------------------------
// Each call is setup.cpp's, which takes the Progress's lock for the fields it changes.

#[allow(unsafe_code)]
mod c {
    use super::Progress;
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn mw2l_setup_begin(progress: *mut Progress, steps: c_int);
        pub fn mw2l_setup_step(progress: *mut Progress, step: c_int, title: *const c_char, length: usize);
        pub fn mw2l_setup_detail(progress: *mut Progress, text: *const c_char, length: usize);
        pub fn mw2l_setup_total(progress: *mut Progress, total: u64);
        pub fn mw2l_setup_add(progress: *mut Progress, done: u64);
        pub fn mw2l_setup_cancelled(progress: *mut Progress) -> bool;
        // A std::string the C++ handed over, set to the text.
        pub fn mw2l_setup_text(to: *mut c_void, text: *const c_char, length: usize);
    }
}

#[allow(unsafe_code)]
impl Shown {
    // SAFETY, each: the Progress setup::Run was handed, alive until it returns.
    pub(crate) fn begin(self, steps: i32) {
        unsafe { c::mw2l_setup_begin(self.0, steps) }
    }
    pub(crate) fn step(self, step: i32, title: &str) {
        unsafe { c::mw2l_setup_step(self.0, step, title.as_ptr().cast(), title.len()) }
    }
    pub(crate) fn detail(self, text: &str) {
        unsafe { c::mw2l_setup_detail(self.0, text.as_ptr().cast(), text.len()) }
    }
    fn total(self, total: u64) {
        unsafe { c::mw2l_setup_total(self.0, total) }
    }
    fn add(self, done: u64) {
        unsafe { c::mw2l_setup_add(self.0, done) }
    }
    pub(crate) fn cancelled(self) -> bool {
        unsafe { c::mw2l_setup_cancelled(self.0) }
    }
}

// The steps, numbered as they are taken.
struct Steps {
    shown: Shown,
    taken: i32,
}

impl Steps {
    fn next(&mut self, title: &str) {
        self.taken += 1;
        self.shown.step(self.taken, title);
    }
}

// ---- the install -----------------------------------------------------------------

fn sha256(bytes: &[u8]) -> String {
    crypto::sha256_hex(bytes)
}

// Of the image an executable decrypts to; none for what is not one.
fn image_sha256(xex: &[u8]) -> Option<String> {
    xex::image(xex).ok().map(|image| sha256(&image))
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

// The system's words for an error, without the number Rust adds: what the C++'s
// std::error_code::message() said.
pub(crate) fn message(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(at) if text.ends_with(')') => text[..at].to_string(),
        _ => text,
    }
}

// The path with ".part" after its name.
fn partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

// Through a temporary name, so a file present is a file written whole.
fn write_file(to: &Path, bytes: &[u8]) -> Result<(), Stop> {
    let part = partial(to);
    if fs::write(&part, bytes).and_then(|()| fs::rename(&part, to)).is_err() {
        return Err(Failed(format!("Could not write {} (disk full?).", absolute(to).display())));
    }
    Ok(())
}

// What to tell a player whose executable is not the disc's.
fn wrong_disc(name: &str, file: &[u8]) -> String {
    let Some(info) = xex::info(file) else { return format!("{name} is not an Xbox 360 executable.") };
    if info.title_id != TITLE_ID {
        return format!("This is not Modern Warfare 2 (title {:08X}).", info.title_id);
    }
    format!(
        "This is Modern Warfare 2, but not the version this build was made from (executable version {}, disc \
         {:08X}). Use an image of the USA/Europe or the Spanish disc, version 1.0.557.",
        info.version, info.media_id
    )
}

// The fastfiles, the image archives and the Bink movies.
fn is_game_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".ff", ".pak", ".bik"].iter().any(|end| lower.ends_with(end))
}

// Copies the disc's game files into game/. What is already there, whole, is kept: an
// install cut short resumes.
fn copy_game(disc: &mut Disc, from: &Path, shown: Shown) -> Result<(), Stop> {
    let folder = Path::new(GAME_FOLDER);
    let copy: Vec<_> = disc
        .files()
        .iter()
        .filter(|file| is_game_file(&file.name))
        .filter(|file| fs::metadata(folder.join(&file.name)).ok().is_none_or(|kept| !kept.is_file() || kept.len() != file.size))
        .cloned()
        .collect();
    let needed: u64 = copy.iter().map(|file| file.size).sum();
    shown.total(needed);
    // Where the free space cannot be told, the copy is tried.
    if let Some(free) = space::available(folder)
        && free < needed
    {
        return Err(Failed(format!(
            "Not enough disk space: {:.1} GB needed, {:.1} GB free.",
            needed as f64 / 1e9,
            free as f64 / 1e9
        )));
    }

    let mut buffer = vec![0u8; 8 << 20];
    for file in &copy {
        shown.detail(&file.name);
        let to = folder.join(&file.name);
        let part = partial(&to);
        let Ok(mut out) = fs::File::create(&part) else {
            return Err(Failed(format!("Cannot write {}", absolute(&part).display())));
        };
        let mut at = 0u64;
        while at < file.size {
            if shown.cancelled() {
                drop(out);
                let _ = fs::remove_file(&part);
                return Err(Cancelled);
            }
            // At most the buffer, which fits a usize.
            let take = (file.size - at).min(buffer.len() as u64) as usize;
            if disc.read(file, at, &mut buffer[..take]).is_err() {
                return Err(Failed(format!("Could not read {} from {}.", file.name, from.display())));
            }
            if out.write_all(&buffer[..take]).is_err() {
                return Err(Failed(format!("Could not write {} (disk full?).", absolute(&part).display())));
            }
            at += take as u64;
            shown.add(take as u64);
        }
        drop(out);
        if let Err(error) = fs::rename(&part, &to) {
            return Err(Failed(format!("Cannot rename {}: {}", part.display(), message(&error))));
        }
    }
    Ok(())
}

// The files of a title update's package, by name, as the update's folder would hold
// them; why not, for what is not such a package.
#[cfg(not(mw2_version_tu0))]
fn package_files(data: Vec<u8>) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let package = stfs::Package::parse(data)?;
    let mut files = BTreeMap::new();
    for file in package.files() {
        files.insert(file.name.clone(), package.read(file)?);
    }
    Ok(files)
}

// The update's files, by name: from the package the player chose or a folder holding
// them, from a package left beside the launcher, or downloaded.
#[cfg(not(mw2_version_tu0))]
fn get_update(disc: &KnownDisc, given: &Path) -> Result<BTreeMap<String, Vec<u8>>, Stop> {
    if given.is_dir() {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(given).into_iter().flatten().flatten() {
            if fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file()) {
                let name = entry.file_name().to_string_lossy().into_owned();
                files.insert(name, fs::read(entry.path()).unwrap_or_default());
            }
        }
        return Ok(files);
    }
    if !given.as_os_str().is_empty() {
        let Ok(data) = fs::read(given) else { return Err(Failed(format!("Cannot read {}.", given.display()))) };
        let name = given.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        return package_files(data).map_err(|why| Failed(format!("{name} cannot be used: {why}.")));
    }
    if let Some(files) = fs::read(disc.update_name).ok().and_then(|data| package_files(data).ok()) {
        return Ok(files);
    }

    // Downloaded to a temporary name beside the launcher, read, and removed.
    let part = partial(Path::new(disc.update_name));
    let got = download(disc.update_url, &part)
        .and_then(|()| fs::read(&part).map_err(|e| message(&e)))
        .and_then(package_files);
    let _ = fs::remove_file(&part);
    got.map_err(|why| Stop::NoUpdate(format!("Title update 6 could not be downloaded: {why}.")))
}

/// What game/ holds, by the executables in it.
fn detect() -> State {
    // The window asks before its first frame, and each executable's image takes about
    // half a second to decrypt and hash: the two are hashed side by side.

    // None for an executable missing; then none for one that is not an executable.
    fn hash(title: &Title) -> Option<Option<String>> {
        fs::read(Path::new(GAME_FOLDER).join(title.xex)).ok().map(|file| image_sha256(&file))
    }
    let hashes = std::thread::scope(|scope| {
        // A thread that cannot be started leaves its executable to this one.
        let other = std::thread::Builder::new().spawn_scoped(scope, || hash(&TITLES[1])).ok();
        let first = hash(&TITLES[0]);
        let second = match other.map(|thread| thread.join()) {
            Some(Ok(second)) => second,
            _ => hash(&TITLES[1]),
        };
        [first, second]
    });
    // Each executable is the installed one, or the disc's and waiting for the update,
    // or something else.
    let (mut installed, mut known) = (true, true);
    for (title, hash) in TITLES.iter().zip(hashes) {
        let Some(hash) = hash else { return State::NotInstalled };
        let is_installed = hash.as_deref() == Some(title.installed);
        installed &= is_installed;
        known &= is_installed || hash.as_deref() == Some(title.disc);
    }
    if installed {
        State::Installed
    } else if known {
        State::NeedsUpdate
    } else {
        State::NotInstalled
    }
}

/// Installs from `disc` (an image or an extracted folder), or brings the install already
/// in game/ up to date when `disc` is empty. `update` is the title update's package or a
/// folder with its files; empty finds or downloads it.
fn run(disc_path: &Path, update: &Path, shown: Shown) -> Result<(), Stop> {
    let copies = !disc_path.as_os_str().is_empty();
    shown.begin(if copies { 3 } else { 1 } + if USES_UPDATE { 2 } else { 0 });
    let mut steps = Steps { shown, taken: 0 };
    let folder = Path::new(GAME_FOLDER);
    if let Err(error) = fs::create_dir_all(folder) {
        return Err(Failed(format!("Cannot create {}: {}", absolute(folder).display(), message(&error))));
    }

    // The disc's executables: from the disc, or already in game/ when only the update
    // is to be applied.
    let mut source = None;
    if copies {
        steps.next("Checking the disc");
        source = Some(Disc::open(disc_path).map_err(Failed)?);
    }
    let mut executables: [Vec<u8>; 2] = Default::default();
    let mut updated = [false; 2];
    for (i, title) in TITLES.iter().enumerate() {
        let read = match &mut source {
            Some(disc) => disc.find(title.xex).and_then(|file| disc.read_all(&file).ok()),
            None => fs::read(folder.join(title.xex)).ok(),
        };
        let Some(file) = read else {
            let place = if copies { disc_path.to_path_buf() } else { absolute(folder) };
            return Err(Failed(format!(
                "There is no {} in {}: it is not a Modern Warfare 2 disc.",
                title.xex,
                place.display()
            )));
        };
        let hash = image_sha256(&file);
        // An update cut short between the two leaves one of them done already.
        updated[i] = !copies && hash.as_deref() == Some(title.installed);
        if !updated[i] && hash.as_deref() != Some(title.disc) {
            return Err(Failed(wrong_disc(title.xex, &file)));
        }
        executables[i] = file;
    }

    #[cfg(not(mw2_version_tu0))]
    let (known, files) = {
        // Which disc, for its update: an executable the update was applied to keeps the
        // disc's media id.
        let infos = [xex::info(&executables[0]), xex::info(&executables[1])];
        let known = match infos {
            [Some(first), Some(second)] if first.media_id == second.media_id => {
                DISCS.iter().position(|disc| disc.media_id == first.media_id)
            }
            _ => None,
        };
        let Some(known) = known else {
            return Err(Failed(format!(
                "This disc ({:08X}) has the right game, but its title update 6 is not one this launcher knows. Use \
                 an image of the USA/Europe or the Spanish disc.",
                infos[0].map_or(0, |info| info.media_id)
            )));
        };
        LAST_DISC.store(known, std::sync::atomic::Ordering::Relaxed);
        let known = &DISCS[known];

        // The update, and the executables it makes, before anything is copied: what can
        // fail fails in the first seconds.
        steps.next("Getting title update 6");
        let files = get_update(known, update)?;
        for (name, hash) in known.added {
            if sha256(files.get(name).map_or(&[][..], Vec::as_slice)) != hash {
                return Err(Failed(format!(
                    "This is not the {} disc's title update 6: its {name} is missing or another version's or \
                     language's.",
                    known.name
                )));
            }
        }

        steps.next("Updating the executables");
        for (i, title) in TITLES.iter().enumerate() {
            if updated[i] {
                continue;
            }
            let patched = match files.get(title.patch) {
                Some(patch) if !patch.is_empty() => patch::apply(&executables[i], patch).ok(),
                _ => None,
            };
            match patched {
                Some(patched) if image_sha256(&patched).as_deref() == Some(title.installed) => {
                    executables[i] = patched
                }
                _ => {
                    return Err(Failed(format!(
                        "This is not title update 6: its {} does not make the expected {}.",
                        title.patch, title.xex
                    )));
                }
            }
        }
        (known, files)
    };
    #[cfg(mw2_version_tu0)]
    let _ = update;
    if shown.cancelled() {
        return Err(Cancelled);
    }

    if let Some(disc) = &mut source {
        steps.next("Copying the game files");
        copy_game(disc, disc_path, shown)?;
    }

    // The executables go last: both present, and the right ones, is what an install
    // that finished looks like.
    steps.next("Finishing");
    #[cfg(not(mw2_version_tu0))]
    for (name, _) in known.added {
        write_file(&folder.join(name), files.get(name).map_or(&[][..], Vec::as_slice))?;
    }
    for (title, executable) in TITLES.iter().zip(&executables) {
        write_file(&folder.join(title.xex), executable)?;
    }
    Ok(())
}

// ---- free disk space, as std::filesystem::space told it ----------------------------

#[allow(unsafe_code)]
mod space {
    use std::path::Path;

    // glibc's struct statvfs on a 64-bit system: every count a 64-bit word.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[repr(C)]
    #[derive(Default)]
    struct StatVfs {
        block_size: u64,
        fragment_size: u64,
        blocks: u64,
        blocks_free: u64,
        blocks_available: u64,
        files: u64,
        files_free: u64,
        files_available: u64,
        id: u64,
        flags: u64,
        name_max: u64,
        spare: [i32; 6],
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    pub fn available(folder: &Path) -> Option<u64> {
        use std::ffi::{CString, c_char, c_int};
        use std::os::unix::ffi::OsStrExt;
        unsafe extern "C" {
            fn statvfs(path: *const c_char, out: *mut StatVfs) -> c_int;
        }
        let path = CString::new(folder.as_os_str().as_bytes()).ok()?;
        let mut out = StatVfs::default();
        // SAFETY: a C string, and a statvfs to fill.
        if unsafe { statvfs(path.as_ptr(), &mut out) } != 0 {
            return None;
        }
        // The blocks an unprivileged user may still write, as libstdc++ counts them.
        Some(out.blocks_available.saturating_mul(out.fragment_size))
    }

    #[cfg(windows)]
    pub fn available(folder: &Path) -> Option<u64> {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(path: *const u16, caller: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        }
        let path: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
        let (mut caller, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: a NUL-terminated wide string, and three counts to fill.
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut caller, &mut total, &mut free) } == 0 {
            return None;
        }
        // What this user may write, quotas counted.
        Some(caller)
    }

    #[cfg(not(any(windows, all(target_os = "linux", target_pointer_width = "64"))))]
    pub fn available(_folder: &Path) -> Option<u64> {
        None
    }
}

// ---- what setup.cpp calls ----------------------------------------------------------
// Text crosses as bytes and a length, paths as UTF-8, as setup.cpp makes them
// (install::Utf8): on Linux a path's own bytes, on Windows the whole name, where the
// code page would lose what it cannot spell.

#[allow(unsafe_code)]
pub(crate) mod exports {
    use super::*;

    // SAFETY: `data` points at `length` bytes, alive for the call, or `length` is 0.
    pub(crate) unsafe fn bytes<'a>(data: *const u8, length: usize) -> &'a [u8] {
        if length == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, length) } }
    }

    fn path_of(bytes: &[u8]) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
        }
        #[cfg(not(unix))]
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }

    // The self-update's text crosses the same way (update.rs).
    pub(crate) fn say(to: *mut c_void, text: &str) {
        // SAFETY: the C++'s std::string, alive across the call that handed it over.
        unsafe { c::mw2l_setup_text(to, text.as_ptr().cast::<c_char>(), text.len()) }
    }

    /// setup::Detect: a setup::State.
    #[unsafe(no_mangle)]
    pub extern "C" fn mw2_setup_detect() -> c_int {
        detect() as c_int
    }

    /// setup::Run: a setup::Result, and `error`, a std::string, set for Failed and
    /// NoUpdate.
    ///
    /// # Safety
    /// `disc` and `update` point at their lengths' bytes (none when 0), `progress` at
    /// the C++'s setup::Progress and `error` at a std::string, all alive until it
    /// returns.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mw2_setup_run(
        disc: *const u8,
        disc_length: usize,
        update: *const u8,
        update_length: usize,
        progress: *mut Progress,
        error: *mut c_void,
    ) -> c_int {
        let (disc, update) = unsafe { (path_of(bytes(disc, disc_length)), path_of(bytes(update, update_length))) };
        let outcome = match run(&disc, &update, Shown(progress)) {
            Ok(()) => Outcome::Done,
            Err(Cancelled) => Outcome::Cancelled,
            Err(Failed(text)) => {
                say(error, &text);
                Outcome::Failed
            }
            Err(Stop::NoUpdate(text)) => {
                say(error, &text);
                Outcome::NoUpdate
            }
        };
        outcome as c_int
    }

    /// setup::UpdateUrl: the address's bytes, which live as long as the program, and
    /// their number in `length`.
    ///
    /// # Safety
    /// `length` points at a size_t.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn mw2_setup_update_url(length: *mut usize) -> *const c_char {
        let url = update_url();
        unsafe { *length = url.len() };
        url.as_ptr().cast()
    }

    /// setup::UsesUpdate.
    #[unsafe(no_mangle)]
    pub extern "C" fn mw2_setup_uses_update() -> bool {
        USES_UPDATE
    }
}
