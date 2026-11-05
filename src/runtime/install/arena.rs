// A custom map's .arena: what the multiplayer's menus show of it, in the PC game's
// format, { key "value" ... } a map. The game lists the maps in it
// (src/hooks/custom_map_list.rs); the launcher's MAPS screen reads them with
// runtime/install/arena.h, which this is the same as.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub struct Map {
    pub map: Vec<u8>,
    pub longname: Vec<u8>,
    pub description: Vec<u8>,
    pub mapimage: Vec<u8>,
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

// `stem` and then `ending`, beside `path`.
pub fn beside(path: &Path, stem: &std::ffi::OsStr, ending: &str) -> PathBuf {
    let mut name = stem.to_os_string();
    name.push(ending);
    path.with_file_name(name)
}

/// The .arena files in `game` whose map is installed: <name>.ff and <name>_load.ff
/// beside it, sorted.
pub fn installed(game: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(game) else { return Vec::new() };
    let mut arenas: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let (Some(extension), Some(stem)) = (path.extension(), path.file_stem()) else { return false };
            extension == "arena" && is_file(&beside(path, stem, ".ff")) && is_file(&beside(path, stem, "_load.ff"))
        })
        .collect();
    arenas.sort();
    arenas
}

/// The file's bytes, none when it cannot be read.
pub fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

/// The maps an arena's text describes.
pub fn maps(text: &[u8]) -> Vec<Map> {
    let mut maps = Vec::new();
    let mut map = Map::default();
    let mut key: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        if c == b'{' {
            map = Map::default();
            key.clear();
        } else if c == b'}' {
            if !map.map.is_empty() {
                maps.push(map.clone());
            }
        } else if c == b'"' {
            let Some(length) = text[i + 1..].iter().position(|&byte| byte == b'"') else { break };
            let end = i + 1 + length;
            let value = text[i + 1..end].to_vec();
            i = end;
            if key.is_empty() {
                key = value;
            } else {
                match key.as_slice() {
                    b"map" => map.map = value,
                    b"longname" => map.longname = value,
                    b"description" => map.description = value,
                    b"mapimage" => map.mapimage = value,
                    _ => {}
                }
                key.clear();
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut end = i;
            while end < text.len() && (text[end].is_ascii_alphanumeric() || text[end] == b'_') {
                end += 1;
            }
            key = text[i..end].to_vec();
            i = end - 1;
        }
        i += 1;
    }
    maps
}
