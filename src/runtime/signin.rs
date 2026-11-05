// Who plays at each controller, and the screen that chooses it for the second to the
// fourth.
//
// A player is a profile of this machine: a number the title knows him by, under which
// his rank is kept (docs/saves.md), and a name. They are in saves/profiles.txt -- a
// line each, twelve hexadecimal digits, a space, the name -- with two lines more:
// `first`, the profile at the first controller, and `machine`, the machine the
// numbers were made on. The first controller's profile is made the first time the
// game runs, named after the login, and is the same every run until the launcher
// names another. On another machine, where a copied game folder ends up, every
// profile gets a new number and what was kept under the old one is renamed: two
// machines never play as one player, which the title answers by dropping the host.
//
// The console signs a profile in at each controller through its own sign-in screen,
// which a title asks for (XamShowSigninUI) and never draws. This is that screen: the
// profiles of this machine (saves/profiles.txt), a new one, or nobody. The console's
// also signs a guest in beside a player on Live, which this title has no use for: its
// split screen is offline. Nothing is remembered between runs: every controller but
// the first starts signed out, and the first's is not chosen on this screen.
//
// The screen is drawn here into a picture the presenter lays over the frame, and is
// driven by the controller that asked for it, through the title's own reads of the
// controllers: while it is open the title sees none pressed.

use crate::env;
use crate::log::{c_string, log_error, log_info};
use crate::runtime::kernel::{self, content};
use crate::runtime::platform;
use crate::runtime::screen::{self, Font, Image, Sheet};
use std::ffi::CStr;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

pub const USERS: u32 = 4;

#[derive(Clone, Default)]
pub struct Player {
    pub signed_in: bool,
    /// the profile's, 48 bits: its offline XUID's
    pub id: u64,
    pub name: Vec<u8>,
}

const UP: u16 = 0x0001;
const DOWN: u16 = 0x0002;
const START: u16 = 0x0010;
const A: u16 = 0x1000;
const B: u16 = 0x2000;
const X: u16 = 0x4000;
/// What a controller holds, at every read of it (XINPUT_GAMEPAD buttons, a stick
/// pushed up or down counted as the d-pad, and this). The Guide button opens the
/// screen for its controller, as it opens the console's.
pub const GUIDE: u16 = 0x0400;
const XN_SYS_SIGNINCHANGED: u32 = 0x0000_000A;

#[derive(Clone)]
struct Profile {
    id: u64,
    name: Vec<u8>,
}

#[derive(Clone)]
enum What {
    Existing(Profile),
    New,
    SignOut,
}

struct Item {
    what: What,
    label: Vec<u8>,
}

struct State {
    players: [Player; USERS as usize],
    first: Player,
    // The screen.
    asking: u32,
    items: Vec<Item>,
    focus: i32,
    // The controllers, as last read.
    held: [u16; USERS as usize],
    swallow: [bool; USERS as usize],
    pressed_last: u32,
}

static STATE: Mutex<State> = Mutex::new(State {
    players: [const { Player { signed_in: false, id: 0, name: Vec::new() } }; USERS as usize],
    first: Player { signed_in: false, id: 0, name: Vec::new() },
    asking: 0,
    items: Vec::new(),
    focus: 0,
    held: [0; USERS as usize],
    swallow: [false; USERS as usize],
    pressed_last: 0,
});
static OPEN: AtomicBool = AtomicBool::new(false);
static VERSION: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn file() -> PathBuf {
    content::save_root().join("profiles.txt")
}

#[cfg(windows)]
const NEWLINE: &[u8] = b"\r\n";
#[cfg(not(windows))]
const NEWLINE: &[u8] = b"\n";

/// The file: a line per profile, twelve hexadecimal digits, a space, the name;
/// `first <the first controller's>`; `machine <where it was written>`.
#[derive(Default)]
struct Book {
    profiles: Vec<Profile>,
    first: u64,
    machine: u64,
}

fn hexadecimal(text: &[u8]) -> (u64, bool) {
    let text = c_string(text);
    let (value, rest) = env::parse_number(&text, 16);
    (value, rest.is_empty())
}

fn read() -> Book {
    let mut book = Book::default();
    let text = std::fs::read(file()).unwrap_or_default();
    let mut lines: Vec<&[u8]> = text.split(|&byte| byte == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    for mut line in lines {
        while let [rest @ .., b'\r' | b' '] = line {
            line = rest;
        }
        if let Some(rest) = line.strip_prefix(b"first ") {
            book.first = hexadecimal(rest).0;
            continue;
        }
        if let Some(rest) = line.strip_prefix(b"machine ") {
            book.machine = hexadecimal(rest).0;
            continue;
        }
        if line.len() < 14 || line[12] != b' ' {
            continue;
        }
        let (id, whole) = hexadecimal(&line[..12]);
        if id == 0 || !whole {
            continue;
        }
        // a gamertag's longest
        book.profiles.push(Profile { id, name: line[13..line.len().min(28)].to_vec() });
    }
    book
}

fn profiles() -> Vec<Profile> {
    read().profiles
}

fn line_of(profile: &Profile) -> Vec<u8> {
    let mut line = format!("{:012x} ", profile.id).into_bytes();
    line.extend_from_slice(&c_string(profile.name.clone()).into_bytes());
    line.extend_from_slice(NEWLINE);
    line
}

fn write(book: &Book) {
    let _ = std::fs::create_dir_all(content::save_root());
    let mut text = format!("machine {:016x}", book.machine).into_bytes();
    text.extend_from_slice(NEWLINE);
    text.extend_from_slice(format!("first {:012x}", book.first).as_bytes());
    text.extend_from_slice(NEWLINE);
    for profile in &book.profiles {
        text.extend_from_slice(&line_of(profile));
    }
    if std::fs::write(file(), text).is_err() {
        log_error!("signin: cannot write %s", c_string(file().to_string_lossy().as_bytes()).as_c_str());
    }
}

/// A number no profile of the book has.
fn new_id(book: &Book) -> u64 {
    loop {
        let id = platform::random() & 0xFFFF_FFFF_FFFF;
        if id >= 0x10000 && !book.profiles.iter().any(|profile| profile.id == id) {
            return id;
        }
    }
}

fn fnv(text: &[u8]) -> u64 {
    // FNV-1a
    text.iter()
        .fold(0xCBF2_9CE4_8422_2325u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3))
}

/// This machine among others, as its system names it.
fn machine() -> u64 {
    let id = platform::machine_id();
    let hash = if id.is_empty() { 0 } else { fnv(&id) };
    if hash != 0 { hash } else { 1 }
}

fn login_name() -> Option<&'static CStr> {
    if let Some(name) = env::text(c"MW2_NAME") {
        return Some(name);
    }
    #[cfg(windows)]
    let login = c"USERNAME";
    #[cfg(not(windows))]
    let login = c"USER";
    env::text(login)
}

impl State {
    /// The profile at the first controller, read or made once a run.
    fn first(&mut self) -> &Player {
        if self.first.signed_in {
            return &self.first;
        }
        let mut book = read();
        let machine = machine();
        let mut changed = book.machine != machine;
        // Written on another machine: these numbers are that machine's players.
        if book.machine != 0 && book.machine != machine {
            for at in 0..book.profiles.len() {
                let was = book.profiles[at].id;
                book.profiles[at].id = new_id(&book);
                if book.first == was {
                    book.first = book.profiles[at].id;
                }
                kernel::xam::move_player_data(was, book.profiles[at].id);
            }
            log_info!(
                "signin: the profiles were another machine's; %llu have new numbers here",
                book.profiles.len() as u64
            );
        }
        book.machine = machine;
        let first = match book.profiles.iter().position(|profile| profile.id == book.first) {
            Some(first) => first,
            None => {
                let name = login_name().map_or(&b""[..], CStr::to_bytes);
                let made = Profile {
                    id: new_id(&book),
                    name: if name.is_empty() { b"Player 1".to_vec() } else { name[..name.len().min(15)].to_vec() },
                };
                book.first = made.id;
                log_info!("signin: a profile for the first controller, %s", c_string(made.name.clone()).as_c_str());
                book.profiles.insert(0, made);
                changed = true;
                0
            }
        };
        if changed {
            write(&book);
        }
        let first = &book.profiles[first];
        self.first = Player { signed_in: true, id: first.id, name: first.name.clone() };
        &self.first
    }

    fn mask(&self) -> u32 {
        (1..USERS).filter(|&user| self.players[user as usize].signed_in).fold(1, |mask, user| mask | 1 << user)
    }

    fn shut(&mut self) {
        OPEN.store(false, Relaxed);
        self.items.clear();
        // Whatever is held now chose something here, not in the title.
        for user in 0..USERS as usize {
            self.swallow[user] = self.held[user] != 0;
        }
        VERSION.fetch_add(1, Relaxed);
        screen::release(screen::Which::Signin);
    }

    fn choose(&mut self, at: usize) {
        let chosen = match &self.items[at].what {
            What::Existing(profile) => Player { signed_in: true, id: profile.id, name: profile.name.clone() },
            What::New => {
                let made = new_profile();
                Player { signed_in: true, id: made.id, name: made.name }
            }
            What::SignOut => Player::default(),
        };
        log_info!(
            "signin: controller %u %s%s",
            self.asking,
            if chosen.signed_in { c"signs in as " } else { c"signs out" },
            c_string(chosen.name.clone()).as_c_str()
        );
        self.players[self.asking as usize] = chosen;
        self.shut();
        kernel::xam::post_notification(XN_SYS_SIGNINCHANGED, self.mask());
    }

    fn open(&mut self, user: u32) {
        // The first controller's player is the online service's.
        if OPEN.load(Relaxed) || user == 0 || user >= USERS || !screen::claim(screen::Which::Signin) {
            return;
        }
        self.asking = user;
        let mut items = Vec::new();
        // The file as it was before the first player is read, which may renumber it.
        for profile in profiles() {
            let taken = profile.id == self.first().id
                || self.players.iter().any(|other| other.signed_in && other.id == profile.id);
            if !taken {
                items.push(Item { label: profile.name.clone(), what: What::Existing(profile) });
            }
        }
        items.push(Item { what: What::New, label: b"New profile".to_vec() });
        if self.players[user as usize].signed_in {
            items.push(Item { what: What::SignOut, label: b"Sign out".to_vec() });
        }
        self.items = items;
        self.focus = 0;
        OPEN.store(true, Relaxed);
        VERSION.fetch_add(1, Relaxed);
        log_info!("signin: the screen opens for controller %u, %llu entries", self.asking, self.items.len() as u64);
    }
}

/// "Player 2", or the first number after it nobody has.
fn new_profile() -> Profile {
    let profiles = profiles();
    let name = (2..)
        .map(|number| format!("Player {number}").into_bytes())
        .find(|name| !profiles.iter().any(|profile| profile.name == *name))
        .unwrap();
    let made = Profile { id: new_id(&Book { profiles, ..Book::default() }), name };

    let _ = std::fs::create_dir_all(content::save_root());
    let appended = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file())
        .and_then(|mut out| out.write_all(&line_of(&made)));
    if appended.is_err() {
        log_error!("signin: cannot write %s", c_string(file().to_string_lossy().as_bytes()).as_c_str());
    }
    made
}

/// The player at controller 1, 2 or 3 (the second to the fourth).
pub fn at(user: u32) -> Player {
    if user == 0 || user >= USERS {
        return Player::default();
    }
    state().players[user as usize].clone()
}

/// The player at the first controller. Always signed in.
pub fn first() -> Player {
    state().first().clone()
}

/// A bit per signed-in controller, the first's always set.
pub fn mask() -> u32 {
    state().mask()
}

/// The title asks for the screen: it opens for the controller pressed last.
pub fn ask() {
    let mut state = state();
    let user = state.pressed_last;
    state.open(user);
}

/// What a controller holds, at every read of it. True when the title is not to see it.
pub fn input(user: u32, buttons: u16) -> bool {
    if user >= USERS {
        return false;
    }
    let mut state = state();
    let at = user as usize;
    let pressed = buttons & !state.held[at];
    state.held[at] = buttons;
    if !OPEN.load(Relaxed) {
        if pressed & (A | X | START) != 0 {
            state.pressed_last = user;
        }
        // The console's own button opens the screen whatever the title shows; not
        // while what shut a screen is still held, which Back and Start pressed one
        // after the other would otherwise be.
        if pressed & GUIDE != 0 && !state.swallow[at] {
            state.open(user);
            if OPEN.load(Relaxed) {
                return true;
            }
        }
        if state.swallow[at] {
            state.swallow[at] = buttons != 0;
        }
        return state.swallow[at];
    }
    if user == state.asking {
        let count = state.items.len() as i32;
        if pressed & DOWN != 0 {
            state.focus = (state.focus + 1) % count;
            VERSION.fetch_add(1, Relaxed);
        }
        if pressed & UP != 0 {
            state.focus = (state.focus + count - 1) % count;
            VERSION.fetch_add(1, Relaxed);
        }
        if pressed & A != 0 {
            let focus = state.focus as usize;
            state.choose(focus);
        } else if pressed & B != 0 {
            state.shut();
        }
    }
    true
}

/// The screen, for a window of this height. False while it is shut.
pub fn picture(window_height: u32, image: &mut Image) -> bool {
    if !OPEN.load(Relaxed) {
        return false;
    }
    let state = state();
    if !OPEN.load(Relaxed) {
        return false;
    }

    // A 450 by 360 sheet, a little over half the window's height.
    let height = 240u32.max(window_height.wrapping_mul(56) / 100) & !1;
    let version = VERSION.load(Relaxed);
    if image.version == version && image.height == height {
        return true;
    }
    let mut sheet = Sheet::new(image, height, 450.0, 360.0, version);
    sheet.frame(b"SIGN IN");
    let current = &state.players[state.asking as usize];
    let mut who = format!("Controller {}", state.asking + 1).into_bytes();
    if current.signed_in {
        who.extend_from_slice(b", signed in as ");
        who.extend_from_slice(&current.name);
    }
    sheet.text(Font::Small, 29.0, 64.0, screen::DIM, &who);

    // Six entries are seen at once; the list follows the one chosen.
    const SEEN: i32 = 6;
    let count = state.items.len() as i32;
    let first = (state.focus - SEEN + 1).clamp(0, 0.max(count - SEEN));
    for i in first..count.min(first + SEEN) {
        let top = 104.0 + (i - first) as f32 * 36.0;
        let chosen = i == state.focus;
        if chosen {
            sheet.fill(2.0, top, 448.0, top + 34.0, screen::BAR);
            sheet.fill(2.0, top, 8.0, top + 34.0, screen::AMBER);
        }
        let colour = if chosen { screen::WHITE } else { screen::DIM };
        sheet.text(Font::Entry, 28.0, top + 2.0, colour, &state.items[i as usize].label);
    }
    if first > 0 {
        sheet.text(Font::Small, 410.0, 84.0, screen::DIM, b"...");
    }
    if first + SEEN < count {
        sheet.text(Font::Small, 410.0, 318.0, screen::DIM, b"...");
    }
    sheet.buttons(326.0, &[("A", "Select"), ("B", "Back")]);
    screen::dump("signin", image);
    true
}
