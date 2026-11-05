// What the player set in the launcher (launcher/settings.cpp) or wrote by hand,
// kept in the file .env as a line each of NAME=value. Any MW2_ switch can be there
// (docs/switches.md).

use crate::env;
use std::ffi::CString;
use std::fs;
use std::path::Path;

// What c_str() would give: the C library stops at the first zero.
fn c_text(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Puts the file's switches in the environment, where the rest of the runtime reads
/// them. One already set there is left as it is, so a switch given on the command
/// line still decides. Called once, before anything reads a switch. Returns what it
/// set, for the log, with the value of any *PASSWORD* switch hidden.
pub fn load(path: &Path) -> Vec<u8> {
    let mut set = Vec::new();
    let Ok(file) = fs::read(path) else { return set };
    for mut line in file.split(|&byte| byte == b'\n') {
        while let [rest @ .., b'\r' | b' '] = line {
            line = rest;
        }
        // The runtime's own switches, not a way to set any variable at all.
        let Some(equals) = line.iter().position(|&byte| byte == b'=') else { continue };
        if !line.starts_with(b"MW2_") {
            continue;
        }
        let (name, value) = (&line[..equals], &line[equals + 1..]);
        let c_name = c_text(name);
        if env::is_set(&c_name) {
            continue;
        }
        env::set(&c_name, &c_text(value));
        if !set.is_empty() {
            set.push(b' ');
        }
        // A password stays out of the log, which a bug report publishes.
        if name.windows(8).any(|part| part == b"PASSWORD") {
            set.extend_from_slice(name);
            set.extend_from_slice(b"=(hidden)");
        } else {
            set.extend_from_slice(line);
        }
    }
    set
}
