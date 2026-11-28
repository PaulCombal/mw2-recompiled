// Custom maps: maps converted from the PC game for the 360's multiplayer, which the maps
// screen lists, adds, puts in the game or takes out of it, and removes.
//
// A map is three files, <name>.ff, <name>_load.ff and <name>.arena (what the menus show
// of it, install/src/arena.rs), and a fourth when it has a picture in the game's menus,
// <name>_preview.ff. A map made for the PC game is converted when it is added (the map
// converter, mapport/), its picture made of its loading screen. The ones in the game
// folder are in the game: the multiplayer lists them in its BONUS MAPS menu, which has
// room for IN_GAME (src/hooks/custom_map_list.rs). The others wait in maps/ beside the
// launcher, which also keeps each map's picture, <name>.bmp, made from the PC map's .iwd
// when it was added, and the log of its conversion, <name>.log. Where the PC game is,
// whose pictures and sounds a converted map takes, is kept in .env (settings.rs).
//
// Names and messages are bytes, as a player's file names are: a path's own bytes on
// Linux, its name in UTF-8 on Windows (boundary.rs). The converter's text is the same
// bytes, a char each (mw2_mapport::text).
#![allow(unsafe_code)]

use crate::boundary::{path_of, utf8};
use crate::settings;
use crate::setup::{self, Progress};
use mw2_install::arena;
use mw2_mapport::port::{self, PcMap, PortOptions};
use mw2_mapport::porter::Log;
use mw2_mapport::zip::Zip;
use mw2_mapport::{Error, fastfile, images, text};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How many custom maps the game has room for (src/hooks/custom_map_list.rs).
pub const IN_GAME: usize = 8;

#[derive(Clone, Default)]
pub struct Map {
    pub name: Vec<u8>,     // the files' name, mp_toujane_v2
    pub longname: Vec<u8>, // as the arena gives it, or the name
    pub description: Vec<u8>,
    pub in_game: bool,
    pub picture: PathBuf, // empty when there is none
}

fn game_folder() -> PathBuf {
    PathBuf::from("game")
}

// Beside the launcher: the maps not in the game, and every picture.
fn store() -> PathBuf {
    PathBuf::from("maps")
}

// The picture in the pane is 416x234 at the design's size.
const PICTURE_WIDTH: usize = 416;
const PICTURE_HEIGHT: usize = 234;

// `name` and `suffix` as a file in `folder`.
fn named(folder: &Path, name: &[u8], suffix: &str) -> PathBuf {
    folder.join(path_of(&[name, suffix.as_bytes()].concat()))
}

// The arena's stem: the name the files share.
fn stem(file: &Path) -> Vec<u8> {
    file.file_stem().map(|s| utf8(Path::new(s))).unwrap_or_default()
}

fn file_name(file: &Path) -> Vec<u8> {
    file.file_name().map(|s| utf8(Path::new(s))).unwrap_or_default()
}

// At most `most` bytes of a file; none when it can't be read.
fn read_head(path: &Path, most: usize) -> Vec<u8> {
    use std::io::Read;
    let mut bytes = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(most as u64).read_to_end(&mut bytes);
    }
    bytes
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

// ---- the picture: a .iwd is a zip of the PC game's .iwi images --------------------------

// The .iwd's picture of that name (as the game looks it up, whatever the case or the
// slashes), decoded to RGBA, the largest level: (width, height, pixels).
fn read_picture(iwd: &Zip, image: &[u8]) -> Result<Option<(i32, i32, Vec<u8>)>, Error> {
    let wanted = text::of(&[b"images/", image, b".iwi"].concat()).to_ascii_lowercase();
    for (k, name) in iwd.names().iter().enumerate() {
        if name.replace('\\', "/").to_ascii_lowercase() != wanted {
            continue;
        }
        let pic = images::read_iwi(&iwd.read(k)?)?;
        if pic.cube || pic.mips.is_empty() {
            return Ok(None);
        }
        let rgba = images::decode_pixels(&pic.mips[0], pic.width, pic.height, pic.format)?;
        return Ok(Some((pic.width, pic.height, rgba)));
    }
    Ok(None)
}

// The map's picture from its .iwd, the size the pane shows it, as the game stretches a
// loading screen over the screen. Its mapimage when the .iwd has it, else its loading
// screen.
fn make_picture(iwd: &Path, map: &arena::Map, name: &[u8], to: &Path) -> bool {
    let found = (|| -> Result<Option<(i32, i32, Vec<u8>)>, Error> {
        let zip = Zip::open(iwd)?;
        let images = [map.mapimage.clone(), [b"loadscreen_", name].concat(), [&b"loadscreen_"[..], &map.map].concat()];
        for image in images.iter().filter(|i| !i.is_empty()) {
            if let Some(found) = read_picture(&zip, image)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    })();
    let Ok(Some((width, height, pixels))) = found else { return false };
    if width < 1 || height < 1 || pixels.len() < width as usize * height as usize * 4 {
        return false;
    }
    let (width, height) = (width as usize, height as usize);
    let mut shown = vec![0u8; PICTURE_WIDTH * PICTURE_HEIGHT * 4];
    for y in 0..PICTURE_HEIGHT {
        for x in 0..PICTURE_WIDTH {
            let from = ((y * height / PICTURE_HEIGHT) * width + x * width / PICTURE_WIDTH) * 4;
            let p = &mut shown[(y * PICTURE_WIDTH + x) * 4..][..4];
            p[..3].copy_from_slice(&pixels[from..from + 3]);
            p[3] = 255;
        }
    }
    save_bmp(&shown, to)
}

// RGBA pixels of the pane's picture saved as a .bmp, as SDL saves one.
fn save_bmp(pixels: &[u8], to: &Path) -> bool {
    use sdl3_sys::everything as sdl;
    let Ok(path) = std::ffi::CString::new(utf8(to)) else { return false };
    // SAFETY: a surface over `pixels`, alive across its use here, then destroyed; the path a
    // C string.
    unsafe {
        let surface = sdl::SDL_CreateSurfaceFrom(
            PICTURE_WIDTH as i32,
            PICTURE_HEIGHT as i32,
            sdl::SDL_PIXELFORMAT_RGBA32,
            pixels.as_ptr() as *mut std::ffi::c_void,
            (PICTURE_WIDTH * 4) as i32,
        );
        if surface.is_null() {
            return false;
        }
        let saved = sdl::SDL_SaveBMP(surface, path.as_ptr());
        sdl::SDL_DestroySurface(surface);
        saved
    }
}

// ---- the files ------------------------------------------------------------------------

// A fastfile the 360's multiplayer loads: unsigned, as nobody but the game's makers can
// sign one, and of the version of the game's own; else why not.
fn console_fastfile(file: &Path) -> Result<(), Vec<u8>> {
    let head = read_head(file, 12);
    let own = read_head(&game_folder().join("common_mp.ff"), 12);
    let name = file_name(file);
    if head.len() < 12 {
        return Err([&name[..], b" could not be read."].concat());
    }
    if &head[..8] != b"IWffu100" {
        return Err([&name[..], b" is not a map file of the game."].concat());
    }
    if own.len() == 12 && head[8..12] != own[8..12] {
        return Err([&name[..], b" was made for another version of the game."].concat());
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> bool {
    if fs::rename(from, to).is_ok() {
        return true;
    }
    // Another drive: a copy, then the original goes.
    if fs::copy(from, to).is_err() {
        return false;
    }
    let _ = fs::remove_file(from);
    true
}

// Whether two paths are one file.
fn equivalent(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// A map's files in the game. The last, the menus' picture of the map, a map can be
// without: one converted before maps came with it, or one made for the 360.
const SUFFIXES: [&str; 4] = [".ff", "_load.ff", ".arena", "_preview.ff"];
const PREVIEW: &str = "_preview.ff";

// ---- converting a map made for the PC game ----------------------------------------------

fn pc_fastfile(file: &Path) -> bool {
    fastfile::is_pc_fastfile(file)
}

// The game's own files, which a conversion takes what the map needs from: the game
// folder's fastfiles, but for the custom maps installed there.
fn stock_files() -> Vec<PathBuf> {
    let game = game_folder();
    let mut custom = BTreeSet::new();
    for arena in arena::installed(&game) {
        for suffix in [".ff", "_load.ff", PREVIEW] {
            custom.insert(named(&game, &stem(&arena), suffix));
        }
    }
    let mut stock: Vec<PathBuf> = fs::read_dir(&game)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "ff") && !custom.contains(p))
                .collect()
        })
        .unwrap_or_default();
    stock.sort();
    stock
}

// ---- the PC game: the pictures and sounds a map takes from it ----------------------------

/// Whether a folder is the PC game's: its main/ (or the folder itself, its main folder
/// chosen) has the game's first .iwd.
fn holds_pc_game(folder: &Path) -> bool {
    is_file(&folder.join("main").join("iw_00.iwd")) || is_file(&folder.join("iw_00.iwd"))
}

#[cfg(windows)]
fn registry_folder(root: isize, key: &str, value: &str) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut std::ffi::c_void,
            bytes: *mut u32,
        ) -> i32;
    }
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const MAX_PATH: usize = 260;
    let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain([0]).collect::<Vec<u16>>();
    let (key, value) = (wide(key), wide(value));
    let mut folder = [0u16; MAX_PATH];
    let mut bytes = (MAX_PATH * 2) as u32;
    // SAFETY: the names zero-ended, the buffer and its size in bytes.
    let found = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            folder.as_mut_ptr().cast(),
            &mut bytes,
        )
    } == 0;
    if !found {
        return None;
    }
    let length = folder.iter().position(|&c| c == 0).unwrap_or(folder.len());
    Some(PathBuf::from(OsString::from_wide(&folder[..length])))
}

// The places Steam is installed in: on Linux, the usual ones, the snap's and the
// Flatpak's; on Windows, where the registry says, else the usual one. Each once.
fn steam_folders() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
        const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
        for (root, key, value) in [
            (HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath"),
            (HKEY_LOCAL_MACHINE, "SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath"),
            (HKEY_LOCAL_MACHINE, "SOFTWARE\\Valve\\Steam", "InstallPath"),
        ] {
            candidates.extend(registry_folder(root, key, value));
        }
        if let Some(programs) = std::env::var_os("ProgramFiles(x86)") {
            candidates.push(PathBuf::from(programs).join("Steam"));
        }
    }
    #[cfg(not(windows))]
    {
        // (Started from a snap, HOME is the snap's and the player's own is SNAP_REAL_HOME.)
        for variable in ["HOME", "SNAP_REAL_HOME"] {
            if let Some(home) = std::env::var_os(variable) {
                for place in [
                    ".local/share/Steam",
                    ".steam/steam",
                    ".steam/root",
                    ".steam/debian-installation",
                    "snap/steam/common/.local/share/Steam",
                    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
                ] {
                    candidates.push(PathBuf::from(&home).join(place));
                }
            }
        }
    }
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for folder in candidates {
        if folder.is_dir() && seen.insert(fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone())) {
            found.push(folder);
        }
    }
    found
}

// The values of `key` in a Steam .vdf file ("key"  "value", a pair a line; \\ and \"
// stand for the character after the backslash).
fn vdf_values(file: &Path, key: &[u8]) -> Vec<Vec<u8>> {
    let mut values = Vec::new();
    let Ok(text) = fs::read(file) else { return values };
    for line in text.split(|&c| c == b'\n') {
        let mut quoted: Vec<Vec<u8>> = Vec::new();
        let mut i = 0;
        while i < line.len() {
            if line[i] != b'"' {
                i += 1;
                continue;
            }
            let mut value = Vec::new();
            i += 1;
            while i < line.len() && line[i] != b'"' {
                if line[i] == b'\\' && i + 1 < line.len() {
                    i += 1;
                }
                value.push(line[i]);
                i += 1;
            }
            quoted.push(value);
            i += 1;
        }
        if quoted.len() == 2 && quoted[0] == key {
            values.push(quoted.pop().unwrap());
        }
    }
    values
}

// Steam's apps of the PC game: the campaign's and the multiplayer's, which share one
// folder.
const PC_GAME_APPS: [&str; 2] = ["10190", "10180"];
const PC_GAME_FOLDER: &str = "Call of Duty Modern Warfare 2";

// The PC game in a Steam library: the folder its app manifest names, when the game is
// there.
fn pc_game_in(library: &Path) -> Option<PathBuf> {
    let mut folders: Vec<Vec<u8>> = Vec::new();
    for app in PC_GAME_APPS {
        let manifest = library.join("steamapps").join(format!("appmanifest_{app}.acf"));
        folders.extend(vdf_values(&manifest, b"installdir"));
    }
    folders.push(PC_GAME_FOLDER.as_bytes().to_vec());
    folders
        .into_iter()
        .map(|dir| library.join("steamapps").join("common").join(path_of(&dir)))
        .find(|game| holds_pc_game(game))
}

// Steam's copy of the PC game: the first of Steam's installations and libraries that has
// it installed.
fn find_pc_game() -> Option<PathBuf> {
    for steam in steam_folders() {
        let mut libraries = vec![steam.clone()];
        for path in vdf_values(&steam.join("steamapps").join("libraryfolders.vdf"), b"path") {
            libraries.push(path_of(&path));
        }
        if let Some(game) = libraries.iter().find_map(|library| pc_game_in(library)) {
            return Some(game);
        }
    }
    None
}

// What a log line of the conversion says to a player: without its indent and its
// "note: ".
fn shown(line: &str) -> &str {
    let text = line.trim_start_matches(' ');
    text.strip_prefix("note: ").unwrap_or(text)
}

// Converts a PC map into `work`: <name>.ff and <name>_load.ff for the 360. Its log goes to
// maps/<name>.log; the notes are what it warned of (pictures missing, shader sets taken
// in place of others, ...), else why it could not. `game`: the PC game's folder, or empty.
fn convert(
    map: &PcMap,
    work: &Path,
    name: &[u8],
    game: &Path,
    progress: Option<&Progress>,
) -> Result<Vec<String>, Vec<u8>> {
    let _ = fs::remove_dir_all(work);
    let _ = fs::create_dir_all(work);
    let log_path = named(&store(), name, ".log");
    let log_file = Rc::new(RefCell::new(fs::File::create(&log_path).ok()));
    if let Some(progress) = progress {
        let title: &[u8] = if map.load.is_some() { b"Converting the loading screen" } else { b"Converting the map" };
        let mut state = progress.state();
        state.steps = if map.load.is_some() { 2 } else { 1 };
        state.step = 1;
        state.title = title.to_vec();
        state.detail.clear();
        state.done = 0;
        state.total = 0;
    }
    let step = Cell::new(0);
    let first_map_line = text::of(&[&b"converting "[..], &file_name(&map.ff)].concat());
    let has_load = map.load.is_some();
    let log: Log = {
        let log_file = log_file.clone();
        // A Progress outlives the conversion (the caller's): a pointer to it, as the log
        // must not borrow.
        let progress = progress.map(|p| p as *const Progress);
        Rc::new(move |line: &str| {
            if let Some(file) = log_file.borrow_mut().as_mut() {
                let _ = file.write_all(&[text::bytes(line), b"\n".to_vec()].concat());
            }
            let Some(progress) = progress else { return Ok(()) };
            // SAFETY: the caller's Progress, alive while the conversion runs.
            let progress = unsafe { &*progress };
            if progress.cancelled() {
                return Err(Error::Cancelled);
            }
            let mut state = progress.state();
            // The loading screen done (or left out), the map is next.
            if has_load && step.get() == 0 && line == first_map_line {
                step.set(1);
                state.step = 2;
                state.title = b"Converting the map".to_vec();
            }
            state.detail = text::bytes(shown(line));
            Ok(())
        })
    };
    let ported = (|| -> mw2_mapport::Result<Vec<String>> {
        let mut options = PortOptions::default();
        if !game.as_os_str().is_empty() {
            options.game_iwds = port::game_iwds(game);
            let folder = text::of(&utf8(game));
            log(&if options.game_iwds.is_empty() {
                format!("no .iwd files in {folder}, the PC game's folder")
            } else {
                format!("the PC game's pictures and sounds: {folder}")
            })?;
        }
        Ok(port::port_map(map, work, &stock_files(), &log, &options)?.warnings)
    })();
    let notes = match ported {
        Ok(notes) => notes,
        Err(Error::Cancelled) => return Err(b"it was stopped.".to_vec()),
        Err(e) => {
            let what = text::bytes(e.text());
            if let Some(file) = log_file.borrow_mut().as_mut() {
                let _ = file.write_all(&[&what[..], b"\n"].concat());
            }
            return Err([&b"it could not be converted: "[..], &what, b"."].concat());
        }
    };
    // (The converter names what it writes after the .ff.)
    if !is_file(&named(work, &stem(&map.ff), "_load.ff")) {
        return Err([&b"its loading screen could not be converted ("[..], &utf8(&log_path), b" says why)."].concat());
    }
    Ok(notes)
}

fn collect(folder: &Path, in_game: bool, into: &mut Vec<Map>) {
    for file in arena::installed(folder) {
        let mut map = Map { name: stem(&file), in_game, ..Map::default() };
        let described = arena::maps(&arena::read(&file));
        if let Some(first) = described.first() {
            map.longname = first.longname.clone();
            map.description = first.description.clone();
        }
        if map.longname.is_empty() {
            map.longname = map.name.clone();
        }
        let picture = named(&store(), &map.name, ".bmp");
        if is_file(&picture) {
            map.picture = picture;
        }
        into.push(map);
    }
}

/// Every installed map, the game's first, each in the order of its name.
pub fn list() -> Vec<Map> {
    let mut list = Vec::new();
    collect(&game_folder(), true, &mut list);
    collect(&store(), false, &mut list);
    list
}

pub fn in_game(list: &[Map]) -> usize {
    list.iter().filter(|map| map.in_game).count()
}

// How a map's files are named after it (_load.ff and _preview.ff before .ff: they end
// the same).
const MAP_FILES: [&str; 5] = ["_load.ff", "_preview.ff", ".ff", ".arena", ".iwd"];

fn lower(bytes: &[u8]) -> Vec<u8> {
    bytes.to_ascii_lowercase()
}

// The chosen files and, beside each, the other files of its map: a map's files share its
// name (mp_x.ff, mp_x_load.ff, mp_x.arena, mp_x.iwd), so choosing one of them is enough.
// Names match whatever their case.
fn with_its_files(chosen: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = chosen.to_vec();
    let parent = |file: &Path| file.parent().map(Path::to_path_buf).unwrap_or_default();
    // (folder, file name in lower case)
    let key = |file: &Path| [utf8(&parent(file)), vec![0], lower(&file_name(file))].concat();
    let mut have: BTreeSet<Vec<u8>> = chosen.iter().map(|f| key(f)).collect();
    // (folder, map)
    let mut looked: BTreeSet<(PathBuf, Vec<u8>)> = BTreeSet::new();
    for file in chosen {
        let name = lower(&file_name(file));
        let Some(ending) = MAP_FILES.iter().find(|e| name.len() > e.len() && name.ends_with(e.as_bytes())) else {
            continue;
        };
        let map = name[..name.len() - ending.len()].to_vec();
        let folder = match file.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if !looked.insert((folder.clone(), map.clone())) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let other = lower(&utf8(Path::new(&entry.file_name())));
            let ours = MAP_FILES.iter().any(|e| other == [&map[..], e.as_bytes()].concat());
            let path = parent(file).join(entry.file_name());
            if ours && entry.path().is_file() && have.insert(key(&path)) {
                out.push(path);
            }
        }
    }
    out
}

/// Whether adding `files` (and their maps' other files) converts a map made for the PC
/// game.
pub fn converts(files: &[PathBuf]) -> bool {
    with_its_files(files).iter().any(|f| {
        f.extension().is_some_and(|x| lower(&utf8(Path::new(x))) == b"ff") && pc_fastfile(f)
    })
}

/// How adding maps went.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Added {
    All,
    NotAll,
}

/// Adds the maps among `files`: each .arena with its two fastfiles, and an .iwd; a file
/// chosen brings its map's others from its folder (they share the map's name). Fastfiles
/// of the 360 are copied as they are; the PC game's are converted for the 360 first, with
/// the .iwd's pictures, which takes a while: `progress` follows it and its cancel stops
/// it; `pc_game` is the PC game's folder (pc_game()) or empty; a build that installs no
/// title update converts none. The .iwd also gives the map its picture here. The maps go
/// into the game folder while the game has room, else into maps/; the chosen files stay as
/// they are. `said` is set to what came of it.
pub fn add(chosen: &[PathBuf], said: &mut Vec<u8>, progress: Option<&Progress>, pc_game: &Path) -> Added {
    let files = with_its_files(chosen);
    // The map files by their name, lower case.
    let mut by_name: BTreeMap<Vec<u8>, PathBuf> = BTreeMap::new();
    for file in files {
        by_name.insert(lower(&file_name(&file)), file);
    }
    let find = |name: &[u8]| by_name.get(&lower(name)).cloned();
    let arenas: Vec<PathBuf> =
        by_name.iter().filter(|(name, _)| name.ends_with(b".arena")).map(|(_, f)| f.clone()).collect();
    if arenas.is_empty() {
        *said = b"No <map>.arena was found with the files chosen: a map is its <map>.ff, <map>_load.ff, <map>.arena and <map>.iwd, in one folder, as they came with it.".to_vec();
        return Added::NotAll;
    }
    let _ = fs::create_dir_all(store());
    let mut installed = list();
    let (mut added, mut waiting, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    let mut notes: Vec<Vec<u8>> = Vec::new();
    let mut stopped = false;
    let several = arenas.len() > 1;
    let prefix = |name: &[u8]| if several { [name, b": "].concat() } else { Vec::new() };
    for arena_file in &arenas {
        let name = stem(arena_file);
        let ff = find(&[&name[..], b".ff"].concat());
        let load = find(&[&name[..], b"_load.ff"].concat());
        // The picture's, and a PC map's pictures: an .iwd of the same name, or the only one
        // chosen.
        let mut iwd = find(&[&name[..], b".iwd"].concat());
        if iwd.is_none() {
            let all: Vec<&PathBuf> = by_name.iter().filter(|(n, _)| n.ends_with(b".iwd")).map(|(_, f)| f).collect();
            if all.len() == 1 {
                iwd = Some(all[0].clone());
            }
        }
        let mut why: Vec<u8> = Vec::new();
        // (an empty one: the map has none)
        let mut from: [PathBuf; 4] = Default::default();
        let work = store().join(".converting");
        match (&ff, &load) {
            (Some(ff), Some(load)) if pc_fastfile(ff) || pc_fastfile(load) => {
                // Made for the PC game: converted for the 360 first.
                if !pc_fastfile(ff) || !pc_fastfile(load) {
                    why = b"one of its fastfiles is the PC game's and the other the Xbox 360's: choose the two that came together.".to_vec();
                } else if !setup::USES_UPDATE {
                    why = b"it is made for the PC game, and only a build for title update 6 converts maps for the Xbox 360.".to_vec();
                } else {
                    let map = PcMap {
                        ff: ff.clone(),
                        load: Some(load.clone()),
                        iwd: iwd.clone(),
                        arena: Some(arena_file.clone()),
                    };
                    match convert(&map, &work, &name, pc_game, progress) {
                        Ok(own) => {
                            let s = stem(ff);
                            from[0] = named(&work, &s, ".ff");
                            from[1] = named(&work, &s, "_load.ff");
                            let preview = named(&work, &s, PREVIEW);
                            if is_file(&preview) {
                                from[3] = preview;
                            }
                            for n in own {
                                notes.push([prefix(&name), text::bytes(&n)].concat());
                            }
                        }
                        Err(w) => why = w,
                    }
                }
                stopped = progress.is_some_and(Progress::cancelled);
            }
            (Some(ff), Some(load)) => match console_fastfile(ff).and_then(|()| console_fastfile(load)) {
                Ok(()) => {
                    from[0] = ff.clone();
                    from[1] = load.clone();
                    // Its menu picture, which the game loads with its menus at every start:
                    // only one it can load.
                    if let Some(preview) = find(&[&name[..], PREVIEW.as_bytes()].concat()) {
                        match console_fastfile(&preview) {
                            Ok(()) => from[3] = preview,
                            Err(not_loadable) => notes.push(
                                [prefix(&name), b"its menu picture is left out: ".to_vec(), not_loadable].concat(),
                            ),
                        }
                    }
                }
                Err(w) => why = w,
            },
            _ => {
                why = [&b"its "[..], &name, if ff.is_none() { b".ff" } else { b"_load.ff" }, b" isn't beside its .arena."]
                    .concat()
            }
        }
        if !why.is_empty() || stopped {
            let reason: &[u8] = if why.is_empty() { b"it was stopped." } else { &why };
            failed.extend([&b"\n"[..], &name, b": ", reason].concat());
            let _ = fs::remove_dir_all(&work);
            if stopped {
                break;
            }
            continue;
        }
        from[2] = arena_file.clone();

        // In the game while there is room; a map added again stays where it was.
        let in_game_now = match installed.iter().find(|m| m.name == name) {
            Some(was) => was.in_game,
            None => in_game(&installed) < IN_GAME,
        };
        let folder = if in_game_now { game_folder() } else { store() };
        let mut copy_error = None;
        for (i, suffix) in SUFFIXES.iter().enumerate() {
            let to = named(&folder, &name, suffix);
            if from[i].as_os_str().is_empty() {
                let _ = fs::remove_file(&to); // (one an earlier copy of the map came with)
                continue;
            }
            if equivalent(&from[i], &to) {
                continue;
            }
            // (what was there is replaced, a link too, not written through)
            let _ = fs::remove_file(&to);
            if let Err(e) = fs::copy(&from[i], &to) {
                // Without its menu picture the map still plays.
                if *suffix != PREVIEW {
                    copy_error = Some(e);
                    break;
                }
            }
        }
        let _ = fs::remove_dir_all(&work);
        if let Some(e) = copy_error {
            failed.extend(
                [&b"\n"[..], &name, b": its files could not be copied into ", &utf8(&folder), b" (", setup::message(&e).as_bytes(), b")."]
                    .concat(),
            );
            continue;
        }

        let described = arena::maps(&arena::read(arena_file));
        if let Some(iwd) = &iwd {
            let map = described.first().cloned().unwrap_or_default();
            make_picture(iwd, &map, &name, &named(&store(), &name, ".bmp"));
        }
        let shown = match described.first() {
            Some(first) if !first.longname.is_empty() => first.longname.clone(),
            _ => name.clone(),
        };
        let into = if in_game_now { &mut added } else { &mut waiting };
        if !into.is_empty() {
            into.extend_from_slice(b", ");
        }
        into.extend(shown);
        installed = list();
    }
    said.clear();
    if !added.is_empty() {
        said.extend([&b"Added, and in the game: "[..], &added, b"."].concat());
    }
    if !waiting.is_empty() {
        if !said.is_empty() {
            said.extend_from_slice(b"\n\n");
        }
        said.extend(
            [
                &b"Added: "[..],
                &waiting,
                format!(
                    ". The game already has {IN_GAME} custom maps, so to play these, take another out of the game first."
                )
                .as_bytes(),
            ]
            .concat(),
        );
    }
    if !notes.is_empty() {
        // What the conversion noted, the first few: the rest are in the log.
        said.extend_from_slice(b"\n\nThe conversion noted:");
        for note in notes.iter().take(3) {
            said.extend([&b"\n- "[..], note].concat());
        }
        if notes.len() > 3 {
            said.extend(format!("\n- and {} more, in the log in the maps folder.", notes.len() - 3).as_bytes());
        }
    }
    if !failed.is_empty() {
        let head: &[u8] = if said.is_empty() { b"No map was added." } else { b"\n\nNot added:" };
        said.extend([head, &failed].concat());
    }
    if failed.is_empty() { Added::All } else { Added::NotAll }
}

/// Puts a map in the game or takes it out; the game has room for IN_GAME. `said` is set
/// to what came of it, when anything did.
pub fn set_in_game(map: &Map, on: bool, said: &mut Vec<u8>) -> bool {
    if map.in_game == on {
        return true;
    }
    if on && in_game(&list()) >= IN_GAME {
        *said = format!("The game has room for {IN_GAME} custom maps, and has them. Take another out of the game first.")
            .into_bytes();
        return false;
    }
    let _ = fs::create_dir_all(store());
    let (from, to) = if on { (store(), game_folder()) } else { (game_folder(), store()) };
    for suffix in SUFFIXES {
        let source = named(&from, &map.name, suffix);
        if suffix == PREVIEW && !source.exists() {
            continue;
        }
        if !move_file(&source, &named(&to, &map.name, suffix)) {
            *said = [&map.longname[..], b" could not be moved: ", &utf8(&source), b"."].concat();
            return false;
        }
    }
    *said = if on {
        [&map.longname[..], b" is in the game: in a private match, GAME SETUP, the map, then BONUS MAPS."].concat()
    } else {
        [&map.longname[..], b" is out of the game. Its files wait in the maps folder beside the launcher."].concat()
    };
    true
}

/// Deletes the launcher's copy, in the game folder or maps/, its picture and the log of
/// its conversion.
pub fn remove(map: &Map, said: &mut Vec<u8>) -> bool {
    let folder = if map.in_game { game_folder() } else { store() };
    for suffix in SUFFIXES {
        let _ = fs::remove_file(named(&folder, &map.name, suffix));
    }
    let _ = fs::remove_file(named(&store(), &map.name, ".bmp"));
    let _ = fs::remove_file(named(&store(), &map.name, ".log"));
    *said = [&map.longname[..], b" is removed."].concat();
    true
}

// Steam's folders are searched once a run: the menus ask every frame.
static SEARCHED: AtomicBool = AtomicBool::new(false);

/// The PC game's folder, whose .iwd files hold the pictures and sounds the PC game's own
/// maps share: a map made for the PC may use them without carrying them, and its
/// conversion takes them from there. It is MW2_PC_GAME (from the environment, else .env);
/// when .env has no such line, Steam's copy, searched for once a run and kept there.
/// Empty when there is none (an empty MW2_PC_GAME= line says so for good). For the
/// window's own thread: it may write .env.
pub fn pc_game() -> PathBuf {
    if let Some(set) = std::env::var_os("MW2_PC_GAME") {
        return PathBuf::from(set);
    }
    if let Some(kept) = settings::pc_game() {
        return path_of(&kept);
    }
    if SEARCHED.swap(true, Ordering::Relaxed) {
        return PathBuf::new();
    }
    let found = find_pc_game().map(|f| utf8(&f)).unwrap_or_default();
    if !found.is_empty() {
        settings::set_pc_game(&found);
    }
    if found.is_empty() { PathBuf::new() } else { path_of(&found) }
}

/// Keeps `folder` as the PC game's, when it has the game's .iwd files; `said` is set to
/// what came of it.
pub fn set_pc_game(folder: &Path, said: &mut Vec<u8>) -> bool {
    let text = utf8(folder);
    if !holds_pc_game(folder) {
        *said = [
            &text[..],
            b" isn't the PC game's folder: it has no main",
            if MAIN_SEPARATOR == '\\' { b"\\" } else { b"/" },
            b"iw_00.iwd. Choose the folder the PC game is installed in, the one with main in it.",
        ]
        .concat();
        return false;
    }
    settings::set_pc_game(&text);
    *said = [&b"Maps added from now on take the pictures and sounds they lack from the PC game in "[..], &text, b"."]
        .concat();
    true
}
