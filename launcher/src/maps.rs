// Custom maps: maps converted from the PC game for the 360's multiplayer, which the maps
// screen lists, adds, puts in the game or takes out of it, and removes. The work on the
// files, and the conversion of a map made for the PC game, are the map converter's front
// (maps.cpp, C++ with the converter, mapport/); these call it. Where the PC game is, whose
// pictures and sounds a converted map takes, is kept here, in .env (settings.rs).
#![allow(unsafe_code)]

use crate::boundary::{bytes, path_of, utf8};
use crate::settings;
use crate::setup::{self, Progress};
use std::ffi::{c_char, c_void};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
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

type TextSink = extern "C" fn(context: *mut c_void, text: *const c_char, length: usize);
type MapSink = extern "C" fn(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    longname: *const c_char,
    longname_length: usize,
    description: *const c_char,
    description_length: usize,
    in_game: bool,
    picture: *const c_char,
    picture_length: usize,
);

unsafe extern "C" {
    fn mw2l_maps_list(sink: MapSink, context: *mut c_void);
    fn mw2l_maps_converts(files: *const c_char, length: usize) -> bool;
    fn mw2l_maps_add(
        files: *const c_char,
        length: usize,
        progress: *const c_void,
        pc_game: *const c_char,
        pc_game_length: usize,
        uses_update: bool,
        said: TextSink,
        said_context: *mut c_void,
    ) -> i32;
    fn mw2l_maps_set_in_game(
        name: *const c_char,
        name_length: usize,
        longname: *const c_char,
        longname_length: usize,
        in_game: bool,
        on: bool,
        said: TextSink,
        said_context: *mut c_void,
    ) -> bool;
    fn mw2l_maps_remove(
        name: *const c_char,
        name_length: usize,
        longname: *const c_char,
        longname_length: usize,
        in_game: bool,
        said: TextSink,
        said_context: *mut c_void,
    ) -> bool;
    fn mw2l_maps_find_pc_game(found: TextSink, context: *mut c_void);
    fn mw2l_maps_holds_pc_game(folder: *const c_char, length: usize) -> bool;
}

// `context`, a Vec<u8>, set to the text: what a call said replaces what was there, as the
// std::string the C++ was handed did. (precompile.rs's append_text adds to it.)
extern "C" fn set_text(context: *mut c_void, text: *const c_char, length: usize) {
    // SAFETY: the Vec the caller handed as `context`, and `length` bytes.
    let into = unsafe { &mut *context.cast::<Vec<u8>>() };
    *into = unsafe { bytes(text, length) }.to_vec();
}

#[allow(clippy::too_many_arguments)]
extern "C" fn into_list(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    longname: *const c_char,
    longname_length: usize,
    description: *const c_char,
    description_length: usize,
    in_game: bool,
    picture: *const c_char,
    picture_length: usize,
) {
    // SAFETY: the list the caller handed as `context`, and each text its length of bytes.
    unsafe {
        let list = &mut *context.cast::<Vec<Map>>();
        let picture = bytes(picture, picture_length);
        list.push(Map {
            name: bytes(name, name_length).to_vec(),
            longname: bytes(longname, longname_length).to_vec(),
            description: bytes(description, description_length).to_vec(),
            in_game,
            picture: if picture.is_empty() { PathBuf::new() } else { path_of(picture) },
        });
    }
}

// Paths as UTF-8, each ended by a zero byte.
fn path_list(files: &[PathBuf]) -> Vec<u8> {
    let mut list = Vec::new();
    for file in files {
        list.extend_from_slice(&utf8(file));
        list.push(0);
    }
    list
}

/// Every installed map, the game's first, each in the order of its name.
pub fn list() -> Vec<Map> {
    let mut list: Vec<Map> = Vec::new();
    // SAFETY: the list on this stack, alive across the call.
    unsafe { mw2l_maps_list(into_list, (&raw mut list).cast()) };
    list
}

pub fn in_game(list: &[Map]) -> usize {
    list.iter().filter(|map| map.in_game).count()
}

/// Whether adding `files` (and their maps' other files) converts a map made for the PC
/// game.
pub fn converts(files: &[PathBuf]) -> bool {
    let list = path_list(files);
    // SAFETY: the list's bytes.
    unsafe { mw2l_maps_converts(list.as_ptr().cast(), list.len()) }
}

/// How adding maps went.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Added {
    All,
    NotAll,
    // A step threw what was said, as the window's job caught it.
    Threw,
}

/// Adds the maps among `files` (maps.cpp's maps::Add says how): `progress` follows a
/// conversion and its cancel stops it; `pc_game` is the PC game's folder (pc_game()) or
/// empty. `said` is set to what came of it.
pub fn add(files: &[PathBuf], said: &mut Vec<u8>, progress: Option<&Progress>, pc_game: &Path) -> Added {
    let list = path_list(files);
    let game = utf8(pc_game);
    let progress = progress.map_or(std::ptr::null(), |p| (p as *const Progress).cast::<c_void>());
    // SAFETY: the texts' bytes, the Progress alive across the call (the caller's), and
    // `said` on the caller's stack.
    let added = unsafe {
        mw2l_maps_add(
            list.as_ptr().cast(),
            list.len(),
            progress,
            game.as_ptr().cast(),
            game.len(),
            setup::USES_UPDATE,
            set_text,
            (said as *mut Vec<u8>).cast(),
        )
    };
    match added {
        1 => Added::All,
        0 => Added::NotAll,
        _ => Added::Threw,
    }
}

/// Puts a map in the game or takes it out; the game has room for IN_GAME. `said` is set
/// to what came of it, when anything did.
pub fn set_in_game(map: &Map, on: bool, said: &mut Vec<u8>) -> bool {
    // SAFETY: the map's texts, and `said` on the caller's stack.
    unsafe {
        mw2l_maps_set_in_game(
            map.name.as_ptr().cast(),
            map.name.len(),
            map.longname.as_ptr().cast(),
            map.longname.len(),
            map.in_game,
            on,
            set_text,
            (said as *mut Vec<u8>).cast(),
        )
    }
}

/// Deletes the launcher's copy, in the game folder or maps/, its picture and the log of
/// its conversion.
pub fn remove(map: &Map, said: &mut Vec<u8>) -> bool {
    // SAFETY: as set_in_game.
    unsafe {
        mw2l_maps_remove(
            map.name.as_ptr().cast(),
            map.name.len(),
            map.longname.as_ptr().cast(),
            map.longname.len(),
            map.in_game,
            set_text,
            (said as *mut Vec<u8>).cast(),
        )
    }
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
    let mut found = Vec::new();
    // SAFETY: the text on this stack.
    unsafe { mw2l_maps_find_pc_game(set_text, (&raw mut found).cast()) };
    if !found.is_empty() {
        settings::set_pc_game(&found);
    }
    if found.is_empty() { PathBuf::new() } else { path_of(&found) }
}

/// Keeps `folder` as the PC game's, when it has the game's .iwd files; `said` is set to
/// what came of it.
pub fn set_pc_game(folder: &Path, said: &mut Vec<u8>) -> bool {
    let text = utf8(folder);
    // SAFETY: the path's bytes. (maps.cpp's test, the one Steam's copy is found by.)
    if !unsafe { mw2l_maps_holds_pc_game(text.as_ptr().cast(), text.len()) } {
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
