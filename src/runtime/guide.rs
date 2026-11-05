// The Guide button's screen for the first controller, as the console's guide opened
// over any game: who is playing, the time, what the title says the player is doing
// (presence.rs), the network and the friends on it, then Resume, Invite friends
// (friends.rs) and Quit to desktop. The keyboard's Home key is that button. The first
// controller drives it; the others wait until it shuts. At the other controllers the
// button opens the sign-in screen (signin.rs), as before.

use crate::log::log_info;
use crate::runtime::online::{self, FriendList, Service};
use crate::runtime::screen::{self, Font, Image, Sheet};
use crate::runtime::{bridge, friends, platform, presence, signin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

const USERS: usize = 4;
const UP: u16 = 0x0001;
const DOWN: u16 = 0x0002;
const BACK: u16 = 0x0020;
const A: u16 = 0x1000;
const B: u16 = 0x2000;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Resume,
    Invite,
    Quit,
}
const ITEMS: [Item; 3] = [Item::Resume, Item::Invite, Item::Quit];
const COUNT: i32 = ITEMS.len() as i32;

struct State {
    focus: i32,
    /// Quit chosen once; A again quits
    confirming: bool,
    // What the picture shows that changes while it is open.
    clock: Vec<u8>,
    presence: Vec<u8>,
    network: Vec<u8>,
    friends: FriendList,
    held: [u16; USERS],
    swallow: [bool; USERS],
}

static STATE: Mutex<State> = Mutex::new(State {
    focus: 0,
    confirming: false,
    clock: Vec::new(),
    presence: Vec::new(),
    network: Vec::new(),
    friends: FriendList { version: 0, playing: Vec::new(), online: 0, offline: 0 },
    held: [0; USERS],
    swallow: [false; USERS],
});
static OPEN: AtomicBool = AtomicBool::new(false);
static VERSION: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl State {
    fn open(&mut self) {
        if !screen::claim(screen::Which::Guide) {
            return;
        }
        OPEN.store(true, Relaxed);
        self.focus = 0;
        self.confirming = false;
        VERSION.fetch_add(1, Relaxed);
        log_info!("guide: opens");
    }

    // `next` is the screen that takes over, if any.
    fn shut(&mut self, next: screen::Which) {
        OPEN.store(false, Relaxed);
        for user in 0..USERS {
            self.swallow[user] = self.held[user] != 0;
        }
        VERSION.fetch_add(1, Relaxed);
        if next == screen::Which::None {
            screen::release(screen::Which::Guide);
        } else {
            screen::pass(screen::Which::Guide, next);
        }
    }

    fn choose(&mut self) {
        match ITEMS[self.focus as usize] {
            Item::Resume => self.shut(screen::Which::None),
            Item::Invite => {
                self.shut(screen::Which::Friends);
                friends::ask();
            }
            Item::Quit => {
                if !self.confirming {
                    self.confirming = true;
                    VERSION.fetch_add(1, Relaxed);
                    return;
                }
                log_info!("guide: the player quits");
                bridge::request_exit(c"the player quit from the guide");
            }
        }
    }

    // "Steam: 2 friends playing, 5 online", or the network alone.
    fn network(&mut self, service: Option<&dyn Service>) -> Vec<u8> {
        let Some(name) = online::name() else { return b"No network: system link on this PC".to_vec() };
        match name.to_bytes() {
            b"lan" => return b"LAN".to_vec(),
            b"server" => return b"Server".to_vec(),
            _ => {}
        }
        if !service.is_some_and(|service| service.friends(&mut self.friends)) || self.friends.version == 0 {
            return b"Steam".to_vec();
        }
        let playing = self.friends.playing.len();
        let plural = if playing == 1 { "" } else { "s" };
        format!("Steam: {playing} friend{plural} playing, {} online", self.friends.online).into_bytes()
    }
}

// Keeps a picture's text current, and says whether it changed.
fn refresh(held: &mut Vec<u8>, now: Vec<u8>) {
    if *held == now {
        return;
    }
    *held = now;
    VERSION.fetch_add(1, Relaxed);
}

fn clock() -> Vec<u8> {
    let (hour, minute) = platform::local_hour_minute();
    format!("{hour:02}:{minute:02}").into_bytes()
}

/// What a controller holds, at every read of it, as signin::input takes it. True when
/// the title is not to see it.
pub fn input(user: u32, buttons: u16) -> bool {
    let user = user as usize;
    if user >= USERS {
        return false;
    }
    let mut state = state();
    let pressed = buttons & !state.held[user];
    state.held[user] = buttons;
    if !OPEN.load(Relaxed) {
        // Not while what shut a screen is still held: Back and Start pressed one
        // after the other would shut it with Back and open it again.
        if user == 0 && pressed & signin::GUIDE != 0 && !state.swallow[user] {
            state.open();
            if OPEN.load(Relaxed) {
                return true;
            }
        }
        if state.swallow[user] {
            state.swallow[user] = buttons != 0;
        }
        return state.swallow[user];
    }
    // The first controller's; the others wait.
    if user != 0 {
        return true;
    }
    if pressed & (DOWN | UP) != 0 {
        state.focus = (state.focus + if pressed & DOWN != 0 { 1 } else { COUNT - 1 }) % COUNT;
        state.confirming = false;
        VERSION.fetch_add(1, Relaxed);
    }
    if pressed & signin::GUIDE != 0 {
        state.shut(screen::Which::None);
    } else if pressed & (B | BACK) != 0 {
        if state.confirming {
            state.confirming = false;
            VERSION.fetch_add(1, Relaxed);
        } else {
            state.shut(screen::Which::None);
        }
    } else if pressed & A != 0 {
        state.choose();
    }
    true
}

/// The screen, for a window of this height. False while it is shut.
pub fn picture(window_height: u32, image: &mut Image) -> bool {
    if !OPEN.load(Relaxed) {
        return false;
    }
    let mut state = state();
    if !OPEN.load(Relaxed) {
        return false;
    }
    let state = &mut *state;
    refresh(&mut state.clock, clock());
    refresh(&mut state.presence, presence::text(0));
    let network = state.network(online::get());
    refresh(&mut state.network, network);

    // A 450 by 300 sheet, under half the window's height.
    let height = 200u32.max(window_height.wrapping_mul(46) / 100) & !1;
    let version = VERSION.load(Relaxed);
    if image.version == version && image.height == height {
        return true;
    }
    let mut sheet = Sheet::new(image, height, 450.0, 300.0, version);
    let name = online::get().map(|service| service.local_name()).filter(|name| !name.is_empty());
    sheet.frame(&name.unwrap_or_else(|| signin::first().name));
    let clock_width = sheet.width(Font::Entry, &state.clock);
    sheet.text(Font::Entry, 422.0 - clock_width, 24.0, screen::DIM, &state.clock);

    let presence: &[u8] = if state.presence.is_empty() { b"Modern Warfare 2: Recompiled" } else { &state.presence };
    sheet.text(Font::Small, 29.0, 66.0, screen::WHITE, presence);
    sheet.text(Font::Small, 29.0, 90.0, screen::DIM, &state.network);
    sheet.fill(28.0, 124.0, 422.0, 125.0, screen::EDGE);

    for (i, &item) in ITEMS.iter().enumerate() {
        let top = 140.0 + i as f32 * 36.0;
        let chosen = i as i32 == state.focus;
        if chosen {
            sheet.fill(2.0, top, 448.0, top + 34.0, screen::BAR);
            sheet.fill(2.0, top, 8.0, top + 34.0, screen::AMBER);
        }
        let label: &[u8] = match item {
            Item::Resume => b"Resume",
            Item::Invite => b"Invite friends",
            Item::Quit if state.confirming => b"Quit to desktop? A again to quit",
            Item::Quit => b"Quit to desktop",
        };
        sheet.text(Font::Entry, 28.0, top + 2.0, if chosen { screen::WHITE } else { screen::DIM }, label);
    }
    sheet.buttons(272.0, &[("A", "Select"), ("B", "Back")]);
    screen::dump("guide", image);
    true
}
