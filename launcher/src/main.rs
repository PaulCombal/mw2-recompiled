// The launcher's entry, and the launcher: its window (window.rs, drawn by ui.rs and
// preview.rs with the draw list of draw.rs, atlas.rs and present.rs, read through
// input.rs) and its terminal modes (terminal.rs); the compile pass (precompile.rs), on the
// crate the game builds its shaders with, so both build every shader the same way, and on
// the zone reader (zone/, the mw2-zone crate), which reads the game's shaders out; the
// install (setup.rs, with its download, download.rs), on the crate the game and cargo
// xtask read the disc and the update with; the self-update (update.rs, which reads a
// release's description with the package's library, release.rs); the settings
// (settings.rs), the one window a copy (single.rs), the profile screen's work
// (profile.rs), the bug report (report.rs, scrub.rs), the menu sounds (sound.rs,
// fastfile.rs), on the game's XMA decoder, starting the game (play.rs, spawn.rs) and the
// custom maps (maps.rs), on the map converter (mapport/, the mw2-mapport crate).
//
// `main` is the C runtime's: the arguments it hands over
// (Windows' in the code page, which terminal.rs reads the wide command line instead of),
// and nothing of Rust's start-up -- no SIGPIPE ignored, which the game the launcher
// starts would inherit, no file descriptors opened in place of closed ones.
#![no_main]
// A player's build opens no console window; a terminal it was started from gets what
// the terminal mode prints (terminal.rs). The diagnostics build keeps its own console.
#![cfg_attr(not(feature = "diagnostics"), windows_subsystem = "windows")]

mod atlas;
mod boundary;
mod clock;
mod cnum;
mod download;
mod driver;
mod draw;
mod fastfile;
mod input;
mod maps;
mod play;
mod playerdata_layout;
mod precompile;
mod present;
mod preview;
mod profile;
mod report;
mod scrub;
mod settings;
mod setup;
mod single;
mod sound;
mod spawn;
mod terminal;
mod ui;
mod update;
mod window;

use std::ffi::{c_char, c_int};

#[unsafe(no_mangle)]
extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    mw2_shaders::set_log(precompile::log);
    terminal::run(argc, argv)
}
