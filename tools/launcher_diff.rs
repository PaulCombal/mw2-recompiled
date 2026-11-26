// tools/launcher_diff.py's program: the launcher's Rust held to its C++ as it was before
// (launcher_diff.cpp, linked in), the two run side by side on the same input, every answer
// and every file written compared by the script. One side runs per process, since each
// keeps its state in statics:
//
//   launcher_diff settings cpp|rust <script>   in a folder with a .env
//   launcher_diff profile cpp|rust <script>    in a folder with a saves/
//   launcher_diff scrub cpp|rust <log>         in the folder the log names, with HOME set
//   launcher_diff sound cpp|rust <fastfile> <name>...
//
// A script is a line an operation; what each prints is the same for both sides by
// construction, as one driver here calls either.

#![allow(dead_code, unused_imports)]

#[path = "../launcher/src/boundary.rs"]
mod boundary;
#[path = "../launcher/src/clock.rs"]
mod clock;
#[path = "../launcher/src/cnum.rs"]
mod cnum;
#[path = "../launcher/src/fastfile.rs"]
mod fastfile;
#[path = "../launcher/src/playerdata_layout.rs"]
mod playerdata_layout;
#[path = "../launcher/src/profile.rs"]
mod profile;
#[path = "../launcher/src/scrub.rs"]
mod scrub;
#[path = "../launcher/src/settings.rs"]
mod settings;

// What profile.rs takes from setup.rs, as it is there.
mod setup {
    pub fn message(error: &std::io::Error) -> String {
        let text = error.to_string();
        match text.rfind(" (os error ") {
            Some(at) if text.ends_with(')') => text[..at].to_string(),
            _ => text,
        }
    }
}

use std::ffi::{CString, c_char, c_int, c_void};
use std::io::Write;
use std::path::PathBuf;

// A std::string the C++ fills, and what it holds.
#[repr(C)]
struct CppString {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn launcher_diff_string_new() -> *mut c_void;
    fn launcher_diff_string_free(text: *mut c_void);
    fn launcher_diff_string_data(text: *mut c_void, length: *mut usize) -> *const u8;

    fn old_settings_load();
    fn old_settings_get(which: c_int) -> c_int;
    fn old_settings_text(which: c_int, out: *mut c_void) -> bool;
    fn old_settings_set(which: c_int, value: c_int, other: c_int);
    fn old_settings_set_text(which: c_int, text: *const c_char, length: usize);

    fn old_profile_list(sink: PlayerSink, context: *mut c_void) -> usize;
    fn old_profile_rename(index: usize, name: *const c_char, length: usize, error: *mut c_void) -> bool;
    fn old_profile_play_as(index: usize, error: *mut c_void) -> bool;
    fn old_profile_set_rank(index: usize, level: c_int, prestige: c_int, error: *mut c_void) -> bool;
    fn old_profile_unlock(index: usize, error: *mut c_void) -> bool;
    fn old_profile_campaign(missions: *mut c_int, stars: *mut c_int) -> bool;
    fn old_profile_unlock_missions(error: *mut c_void) -> bool;
    fn old_profile_all_stars(error: *mut c_void) -> bool;
    fn old_profile_max(which: c_int) -> c_int;

    fn old_scrub(path: *const c_char, out: *mut c_void);
    fn old_first_line(text: *const c_char, length: usize, marker: *const c_char, out: *mut c_void);
    fn old_performance(text: *const c_char, length: usize, out: *mut c_void);
    fn old_multiplayer(text: *const c_char, length: usize) -> bool;
    fn old_encoded(text: *const c_char, length: usize, out: *mut c_void);
    fn old_file_address(path: *const c_char, length: usize, out: *mut c_void);

    fn old_find(path: *const c_char, name: *const c_char, at: *mut usize, size: *mut usize, rate: *mut c_int) -> c_int;
}

// A std::string to hand the C++, read back after.
struct Text(*mut c_void);

impl Text {
    fn new() -> Text {
        Text(unsafe { launcher_diff_string_new() })
    }
    fn bytes(&self) -> Vec<u8> {
        let mut length = 0;
        let data = unsafe { launcher_diff_string_data(self.0, &mut length) };
        unsafe { std::slice::from_raw_parts(data, length) }.to_vec()
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        unsafe { launcher_diff_string_free(self.0) }
    }
}

fn show(bytes: &[u8]) -> String {
    bytes.iter().flat_map(|&b| std::ascii::escape_default(b)).map(char::from).collect()
}

// Words of a script line: separated by spaces, \xNN for any byte, \s for a space.
fn unescape(word: &str) -> Vec<u8> {
    let bytes = word.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x') {
            out.push(u8::from_str_radix(&word[i + 2..i + 4], 16).unwrap());
            i += 4;
        } else if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b's') {
            out.push(b' ');
            i += 2;
        } else if word == "-" {
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

// ---- settings ---------------------------------------------------------------------------

trait SettingsSide {
    fn load(&self);
    fn get(&self, which: i32) -> i32;
    fn text(&self, which: i32) -> Option<Vec<u8>>;
    fn set(&self, which: i32, value: i32, other: i32);
    fn set_text(&self, which: i32, value: &[u8]);
}

struct OldSettings;
impl SettingsSide for OldSettings {
    fn load(&self) {
        unsafe { old_settings_load() }
    }
    fn get(&self, which: i32) -> i32 {
        unsafe { old_settings_get(which) }
    }
    fn text(&self, which: i32) -> Option<Vec<u8>> {
        let out = Text::new();
        unsafe { old_settings_text(which, out.0) }.then(|| out.bytes())
    }
    fn set(&self, which: i32, value: i32, other: i32) {
        unsafe { old_settings_set(which, value, other) }
    }
    fn set_text(&self, which: i32, value: &[u8]) {
        unsafe { old_settings_set_text(which, value.as_ptr().cast(), value.len()) }
    }
}

struct NewSettings;
impl SettingsSide for NewSettings {
    fn load(&self) {
        settings::load();
    }
    fn get(&self, which: i32) -> i32 {
        use settings::*;
        match which {
            0 => fullscreen() as i32,
            1 => scale(),
            2 => aspect_ratio() as i32,
            3 => resolution_row() as i32,
            4 => has_fps_limit() as i32,
            5 => fps_limit(),
            6 => fov(),
            7 => msaa(),
            8 => shadow_scale(),
            9 => anisotropy(),
            10 => stretch() as i32,
            11 => custom_maps() as i32,
            12 => sounds() as i32,
            13 => current_network() as i32,
            14 => current_resolution().width as i32,
            15 => current_resolution().height as i32,
            16 => current_resolution().scale,
            _ => -1,
        }
    }
    fn text(&self, which: i32) -> Option<Vec<u8>> {
        match which {
            0 => settings::pc_game(),
            1 => Some(settings::server()),
            _ => Some(settings::server_password()),
        }
    }
    fn set(&self, which: i32, value: i32, other: i32) {
        use settings::*;
        match which {
            0 => set_fullscreen(value != 0),
            1 => set_resolution(Ratio::of(value), other),
            2 => next_aspect_ratio(),
            3 => set_fps_limit(value),
            4 => set_fov(value),
            5 => set_msaa(value),
            6 => set_shadow_scale(value),
            7 => set_anisotropy(value),
            8 => set_stretch(value != 0),
            9 => set_custom_maps(value != 0),
            10 => set_network(match value {
                1 => Network::Lan,
                2 => Network::Server,
                3 => Network::Steam,
                _ => Network::None,
            }),
            _ => {}
        }
    }
    fn set_text(&self, which: i32, value: &[u8]) {
        match which {
            0 => settings::set_pc_game(value),
            1 => settings::set_server(value),
            _ => settings::set_server_password(value),
        }
    }
}

fn run_settings(side: &dyn SettingsSide, script: &str, out: &mut impl Write) {
    for line in script.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        match words[0] {
            "load" => side.load(),
            "get" => {
                let numbers: Vec<String> = (0..17).map(|which| side.get(which).to_string()).collect();
                let texts: Vec<String> =
                    (0..3).map(|which| side.text(which).map_or("<none>".to_string(), |text| show(&text))).collect();
                writeln!(out, "{} | {}", numbers.join(" "), texts.join(" | ")).unwrap();
            }
            "set" => side.set(words[1].parse().unwrap(), words[2].parse().unwrap(), words[3].parse().unwrap()),
            "text" => side.set_text(words[1].parse().unwrap(), &unescape(words.get(2).copied().unwrap_or(""))),
            other => panic!("settings: {other}?"),
        }
    }
}

// ---- profile ----------------------------------------------------------------------------

type PlayerSink = extern "C" fn(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    id: u64,
    file: *const c_char,
    file_length: usize,
    offline: bool,
    first: bool,
    level: c_int,
    prestige: c_int,
    played: *const c_char,
    played_length: usize,
    also: c_int,
);

fn line_of(
    name: &[u8],
    id: u64,
    file: &[u8],
    offline: bool,
    first: bool,
    level: i32,
    prestige: i32,
    played: &[u8],
    also: i32,
) -> String {
    format!(
        "{} {id:x} {} offline={offline} first={first} level={level} prestige={prestige} played={} also={also}",
        show(name),
        show(file),
        show(played)
    )
}

extern "C" fn old_player(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    id: u64,
    file: *const c_char,
    file_length: usize,
    offline: bool,
    first: bool,
    level: c_int,
    prestige: c_int,
    played: *const c_char,
    played_length: usize,
    also: c_int,
) {
    let lines = unsafe { &mut *context.cast::<Vec<String>>() };
    let text = |p: *const c_char, n: usize| unsafe { boundary::bytes(p, n) }.to_vec();
    lines.push(line_of(
        &text(name, name_length),
        id,
        &text(file, file_length),
        offline,
        first,
        level,
        prestige,
        &text(played, played_length),
        also,
    ));
}

trait ProfileSide {
    fn list(&mut self) -> Vec<String>;
    fn rename(&self, index: usize, name: &[u8]) -> Result<(), Vec<u8>>;
    fn play_as(&self, index: usize) -> Result<(), Vec<u8>>;
    fn set_rank(&self, index: usize, level: i32, prestige: i32) -> Result<(), Vec<u8>>;
    fn unlock(&self, index: usize) -> Result<(), Vec<u8>>;
    fn campaign(&self) -> Option<(i32, i32)>;
    fn unlock_missions(&self) -> Result<(), Vec<u8>>;
    fn all_stars(&self) -> Result<(), Vec<u8>>;
    fn max(&self, which: i32) -> i32;
}

fn old_result(call: impl FnOnce(*mut c_void) -> bool) -> Result<(), Vec<u8>> {
    let error = Text::new();
    if call(error.0) { Ok(()) } else { Err(error.bytes()) }
}

struct OldProfile;
impl ProfileSide for OldProfile {
    fn list(&mut self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        unsafe { old_profile_list(old_player, (&raw mut lines).cast()) };
        lines
    }
    fn rename(&self, index: usize, name: &[u8]) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_rename(index, name.as_ptr().cast(), name.len(), error) })
    }
    fn play_as(&self, index: usize) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_play_as(index, error) })
    }
    fn set_rank(&self, index: usize, level: i32, prestige: i32) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_set_rank(index, level, prestige, error) })
    }
    fn unlock(&self, index: usize) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_unlock(index, error) })
    }
    fn campaign(&self) -> Option<(i32, i32)> {
        let (mut missions, mut stars) = (0, 0);
        unsafe { old_profile_campaign(&mut missions, &mut stars) }.then_some((missions, stars))
    }
    fn unlock_missions(&self) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_unlock_missions(error) })
    }
    fn all_stars(&self) -> Result<(), Vec<u8>> {
        old_result(|error| unsafe { old_profile_all_stars(error) })
    }
    fn max(&self, which: i32) -> i32 {
        unsafe { old_profile_max(which) }
    }
}

struct NewProfile(Vec<profile::Player>);
impl NewProfile {
    fn files(&self, index: usize) -> Vec<PathBuf> {
        let player = &self.0[index];
        std::iter::once(player.file.clone()).chain(player.also.iter().map(|other| other.file.clone())).collect()
    }
}
impl ProfileSide for NewProfile {
    fn list(&mut self) -> Vec<String> {
        self.0 = profile::players();
        let mut lines = Vec::new();
        for p in &self.0 {
            let file = boundary::utf8(&p.file);
            lines.push(line_of(
                &p.name,
                p.id,
                &file,
                p.offline,
                p.first,
                p.level,
                p.prestige,
                p.played.as_bytes(),
                p.also.len() as i32,
            ));
            for o in &p.also {
                lines.push(line_of(
                    b"",
                    0,
                    &boundary::utf8(&o.file),
                    o.offline,
                    false,
                    o.level,
                    o.prestige,
                    o.played.as_bytes(),
                    -1,
                ));
            }
        }
        lines
    }
    fn rename(&self, index: usize, name: &[u8]) -> Result<(), Vec<u8>> {
        profile::rename(self.0[index].id, name).map_err(String::into_bytes)
    }
    fn play_as(&self, index: usize) -> Result<(), Vec<u8>> {
        profile::play_as(self.0[index].id).map_err(String::into_bytes)
    }
    fn set_rank(&self, index: usize, level: i32, prestige: i32) -> Result<(), Vec<u8>> {
        profile::set_rank(&self.files(index), level, prestige).map_err(String::into_bytes)
    }
    fn unlock(&self, index: usize) -> Result<(), Vec<u8>> {
        profile::unlock_everything(&self.files(index)).map_err(String::into_bytes)
    }
    fn campaign(&self) -> Option<(i32, i32)> {
        profile::read_campaign()
    }
    fn unlock_missions(&self) -> Result<(), Vec<u8>> {
        profile::unlock_missions().map_err(String::into_bytes)
    }
    fn all_stars(&self) -> Result<(), Vec<u8>> {
        profile::all_stars().map_err(String::into_bytes)
    }
    fn max(&self, which: i32) -> i32 {
        if which == 0 { profile::MAX_LEVEL } else { profile::MAX_PRESTIGE }
    }
}

fn said(result: Result<(), Vec<u8>>) -> String {
    match result {
        Ok(()) => "ok".into(),
        Err(error) => format!("error {}", show(&error)),
    }
}

// After a write, every file under saves/ a time of its own from its name: a write's times
// are the clock's, whose ticks two writes of one operation can share, and the order of
// files of one time is the sort's own.
fn settle(folder: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            settle(&path);
        } else {
            let name = boundary::utf8(&path);
            let hash =
                name.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + hash % (400 * 86400));
            let _ = std::fs::File::options().write(true).open(&path).and_then(|file| file.set_modified(when));
        }
    }
}

fn run_profile(side: &mut dyn ProfileSide, script: &str, out: &mut impl Write) {
    let mut count = 0usize;
    for line in script.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        // An operation on a player picks one of the last listing's, by a number taken
        // modulo their count, so a script fits either side's list.
        let pick = |word: &str| word.parse::<usize>().unwrap() % count.max(1);
        let result = match words[0] {
            "list" => {
                let lines = side.list();
                count = lines.iter().filter(|line| !line.ends_with("also=-1")).count();
                format!("{} players\n{}", count, lines.join("\n"))
            }
            "rename" if count > 0 => said(side.rename(pick(words[1]), &unescape(words.get(2).copied().unwrap_or("")))),
            "play" if count > 0 => said(side.play_as(pick(words[1]))),
            "rank" if count > 0 => {
                said(side.set_rank(pick(words[1]), words[2].parse().unwrap(), words[3].parse().unwrap()))
            }
            "unlock" if count > 0 => said(side.unlock(pick(words[1]))),
            "rename" | "play" | "rank" | "unlock" => "no players".into(),
            "campaign" => format!("{:?}", side.campaign()),
            "missions" => said(side.unlock_missions()),
            "stars" => said(side.all_stars()),
            "max" => format!("{} {}", side.max(0), side.max(1)),
            other => panic!("profile: {other}?"),
        };
        writeln!(out, "{line} -> {result}").unwrap();
        if matches!(words[0], "rename" | "play" | "rank" | "unlock" | "missions" | "stars") {
            settle(std::path::Path::new("saves"));
        }
    }
}

// ---- the report's scrubbing -------------------------------------------------------------

fn run_scrub(cpp: bool, log_path: &str, out: &mut impl Write) {
    let raw = std::fs::read(log_path).unwrap();
    let scrubbed = if cpp {
        let text = Text::new();
        let path = CString::new(log_path).unwrap();
        unsafe { old_scrub(path.as_ptr(), text.0) };
        text.bytes()
    } else {
        // report.rs's folders(), on Linux.
        let here = boundary::utf8(&std::env::current_dir().unwrap());
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|home| home.as_encoded_bytes().to_vec())
            .unwrap_or_default();
        let spell = |folder: Vec<u8>| if folder.is_empty() { vec![] } else { vec![folder.clone(), folder] };
        scrub::scrub(&raw, &scrub::Folders { here: spell(here), home: spell(home) })
    };
    out.write_all(b"---- scrubbed\n").unwrap();
    out.write_all(&scrubbed).unwrap();
    for input in [&raw, &scrubbed] {
        for marker in ["] report: graphics ", "] online: ", "as \""] {
            let found = if cpp {
                let text = Text::new();
                let marker = CString::new(marker).unwrap();
                unsafe { old_first_line(input.as_ptr().cast(), input.len(), marker.as_ptr(), text.0) };
                text.bytes()
            } else {
                scrub::first_line(input, marker.as_bytes()).to_vec()
            };
            writeln!(out, "---- first line after {marker:?}: {}", show(&found)).unwrap();
        }
        let performance = if cpp {
            let text = Text::new();
            unsafe { old_performance(input.as_ptr().cast(), input.len(), text.0) };
            text.bytes()
        } else {
            scrub::performance(input)
        };
        writeln!(out, "---- performance\n{}", show(&performance)).unwrap();
        let multiplayer = if cpp {
            unsafe { old_multiplayer(input.as_ptr().cast(), input.len()) }
        } else {
            let find = |what: &[u8]| input.windows(what.len()).position(|window| window == what);
            find(b"runtime (multiplayer").unwrap_or(usize::MAX) < find(b"\n").unwrap_or(usize::MAX)
        };
        writeln!(out, "---- multiplayer {multiplayer}").unwrap();
    }
    // Every line as a page's text and as a folder's address: a path's, so without a zero
    // byte, which the C++ let through raw (strchr finds the terminator) and no path has.
    for line in raw.split(|&b| b == b'\n').take(200) {
        let path: Vec<u8> = line.iter().copied().filter(|&b| b != 0).collect();
        let (encoded, address) = if cpp {
            let (a, b) = (Text::new(), Text::new());
            unsafe {
                old_encoded(line.as_ptr().cast(), line.len(), a.0);
                old_file_address(path.as_ptr().cast(), path.len(), b.0);
            }
            (a.bytes(), b.bytes())
        } else {
            (scrub::encoded(line).into_bytes(), scrub::file_address(&path).into_bytes())
        };
        writeln!(out, "{} {}", show(&encoded), show(&address)).unwrap();
    }
}

// ---- the menu sounds --------------------------------------------------------------------

fn run_sound(cpp: bool, path: &str, names: &[String], out: &mut impl Write) {
    for name in names {
        let found = if cpp {
            let (mut at, mut size, mut rate) = (0usize, 0usize, 0);
            let (path, name) = (CString::new(path).unwrap(), CString::new(name.as_str()).unwrap());
            match unsafe { old_find(path.as_ptr(), name.as_ptr(), &mut at, &mut size, &mut rate) } {
                0 => format!("{at} {size} {rate}"),
                -1 => "no sound".into(),
                _ => "no zone".into(),
            }
        } else {
            match fastfile::zone(&std::fs::read(path).unwrap_or_default()) {
                None => "no zone".into(),
                Some(zone) => match fastfile::find(&zone, name) {
                    Some(sound) => format!("{} {} {}", sound.at, sound.size, sound.rate),
                    None => "no sound".into(),
                },
            }
        };
        writeln!(out, "{name}: {found}").unwrap();
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let cpp = match arguments[2].as_str() {
        "cpp" => true,
        "rust" => false,
        side => panic!("cpp or rust, not {side}"),
    };
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match arguments[1].as_str() {
        "settings" => {
            let script = std::fs::read_to_string(&arguments[3]).unwrap();
            run_settings(if cpp { &OldSettings } else { &NewSettings }, &script, &mut out);
        }
        "profile" => {
            let script = std::fs::read_to_string(&arguments[3]).unwrap();
            if cpp {
                run_profile(&mut OldProfile, &script, &mut out)
            } else {
                run_profile(&mut NewProfile(Vec::new()), &script, &mut out)
            }
        }
        "scrub" => run_scrub(cpp, &arguments[3], &mut out),
        "sound" => run_sound(cpp, &arguments[3], &arguments[4..], &mut out),
        module => panic!("{module}?"),
    }
}
