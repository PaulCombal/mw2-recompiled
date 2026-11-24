// The screen that invites friends. On the console the title asks for the system's
// friend list (XamShowFriendsUI) and never draws one; this is that list, over the
// frame as the sign-in screen is (signin.rs): the friends the online service sees
// running this game, with their pictures, then how many others it does not. A invites
// the one chosen, B goes back.
//
// A service with no friends to pick from (online::Service::friends) gets a message
// instead, and invites everyone it can as it did before: LAN's.

use crate::log::log_info;
use crate::runtime::online::{self, FriendList, Service};
use crate::runtime::screen::{self, Font, Image, Sheet};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

const USERS: usize = 4;
const UP: u16 = 0x0001;
const DOWN: u16 = 0x0002;
const BACK: u16 = 0x0020;
const GUIDE: u16 = 0x0400;
const A: u16 = 0x1000;
const B: u16 = 0x2000;

struct State {
    /// with friends to pick from; else the message
    service: Option<&'static dyn Service>,
    message: Vec<&'static str>,
    list: FriendList,
    /// since the screen opened
    invited: BTreeSet<u64>,
    focus: i32,
    // The controllers, as last read.
    held: [u16; USERS],
    swallow: [bool; USERS],
}

static STATE: Mutex<State> = Mutex::new(State {
    service: None,
    message: Vec::new(),
    list: FriendList { version: 0, playing: Vec::new(), online: 0, offline: 0 },
    invited: BTreeSet::new(),
    focus: 0,
    held: [0; USERS],
    swallow: [false; USERS],
});
static OPEN: AtomicBool = AtomicBool::new(false);
static VERSION: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl State {
    fn shut(&mut self) {
        OPEN.store(false, Relaxed);
        // Whatever is held now chose something here, not in the title.
        for user in 0..USERS {
            self.swallow[user] = self.held[user] != 0;
        }
        VERSION.fetch_add(1, Relaxed);
        screen::release(screen::Which::Friends);
    }

    fn chosen(&self) -> u64 {
        self.list.playing.get(self.focus as usize).map_or(0, |one| one.id)
    }

    // A new list keeps the friend chosen chosen, wherever they moved to.
    fn refocus(&mut self, chosen: u64) {
        self.focus = match self.list.playing.iter().position(|one| one.id == chosen) {
            Some(at) => at as i32,
            None => self.focus.clamp(0, 0.max(self.list.playing.len() as i32 - 1)),
        };
        VERSION.fetch_add(1, Relaxed);
    }

    fn invite(&mut self) {
        let Some(service) = self.service else { return };
        let Some(chosen) = self.list.playing.get(self.focus as usize) else { return };
        if chosen.with_you || !chosen.elsewhere.is_empty() || self.invited.contains(&chosen.id) {
            return;
        }
        let id = chosen.id;
        if service.invite_friend(id) {
            self.invited.insert(id);
        }
        VERSION.fetch_add(1, Relaxed);
    }
}

/// Opens the screen, from the title's lobby or from F6.
pub fn ask() {
    let service = online::get();
    let mut state = state();
    if OPEN.load(Relaxed) || !screen::claim(screen::Which::Friends) {
        return;
    }
    state.list = FriendList::default();
    state.invited.clear();
    state.focus = 0;
    state.message.clear();
    let mut list = FriendList::default();
    state.service = service.filter(|service| service.friends(&mut list));
    state.list = list;
    if state.service.is_none() {
        state.message = match service {
            Some(service) => {
                service.invite_friends();
                vec![
                    "Everyone playing on your network",
                    "was invited.",
                    "",
                    "Choosing which friends to invite",
                    "needs STEAM in the launcher's NETWORK.",
                ]
            }
            None => vec!["Inviting friends needs STEAM", "in the launcher's NETWORK."],
        };
    }
    OPEN.store(true, Relaxed);
    VERSION.fetch_add(1, Relaxed);
    log_info!(
        "friends: the screen opens%s",
        if state.service.is_some() { c"" } else { c", with a message: this service has no friends to pick" }
    );
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
        if state.swallow[user] {
            state.swallow[user] = buttons != 0;
        }
        return state.swallow[user];
    }
    let count = state.list.playing.len() as i32;
    if state.service.is_some() && count != 0 {
        if pressed & DOWN != 0 {
            state.focus = (state.focus + 1) % count;
            VERSION.fetch_add(1, Relaxed);
        }
        if pressed & UP != 0 {
            state.focus = (state.focus + count - 1) % count;
            VERSION.fetch_add(1, Relaxed);
        }
    }
    if pressed & (B | BACK | GUIDE) != 0 {
        state.shut();
    } else if pressed & A != 0 {
        if state.service.is_some() {
            state.invite();
        } else {
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
    let mut state = state();
    if !OPEN.load(Relaxed) {
        return false;
    }
    // Asked once a frame, which keeps the service's list fresh while it shows.
    if let Some(service) = state.service {
        let (had, chosen) = (state.list.version, state.chosen());
        service.friends(&mut state.list);
        if state.list.version != had {
            state.refocus(chosen);
        }
    }

    // A 450 by 400 sheet, a little under two thirds of the window's height.
    let height = 260u32.max(window_height.wrapping_mul(62) / 100) & !1;
    let version = VERSION.load(Relaxed);
    if image.version == version && image.height == height {
        return true;
    }
    let mut sheet = Sheet::new(image, height, 450.0, 400.0, version);
    sheet.frame(b"INVITE FRIENDS");

    if state.service.is_none() {
        for (i, line) in state.message.iter().enumerate() {
            sheet.text(Font::Small, 29.0, 80.0 + i as f32 * 26.0, screen::WHITE, line.as_bytes());
        }
        sheet.buttons(374.0, &[("B", "Back")]);
        screen::dump("friends", image);
        return true;
    }

    sheet.text(Font::Small, 29.0, 64.0, screen::DIM, b"Friends playing Modern Warfare 2: Recompiled");
    let count = state.list.playing.len() as i32;
    if state.list.version == 0 {
        sheet.text(Font::Entry, 28.0, 104.0, screen::DIM, b"Looking for your friends...");
    } else if count == 0 {
        sheet.text(Font::Entry, 28.0, 104.0, screen::DIM, b"None of your friends is playing.");
    }

    // Five friends are seen at once; the list follows the one chosen.
    const SEEN: i32 = 5;
    const ROW: f32 = 50.0;
    let first = (state.focus - SEEN + 1).clamp(0, 0.max(count - SEEN));
    for i in first..count.min(first + SEEN) {
        let one = &state.list.playing[i as usize];
        let top = 96.0 + (i - first) as f32 * ROW;
        let chosen = i == state.focus;
        if chosen {
            sheet.fill(2.0, top, 448.0, top + ROW - 2.0, screen::BAR);
            sheet.fill(2.0, top, 8.0, top + ROW - 2.0, screen::AMBER);
        }
        if one.avatar_side != 0 {
            sheet.picture(24.0, top + 4.0, 40.0, &one.avatar, one.avatar_side);
        } else {
            sheet.fill(24.0, top + 4.0, 64.0, top + 44.0, screen::EDGE);
        }
        sheet.text(Font::Entry, 76.0, top - 1.0, if chosen { screen::WHITE } else { screen::DIM }, &one.name);
        let invited = state.invited.contains(&one.id);
        let status = if one.with_you {
            b"In your game".to_vec()
        } else if !one.elsewhere.is_empty() {
            format!("{}: cannot join", one.elsewhere).into_bytes()
        } else if invited {
            b"Invited".to_vec()
        } else {
            Vec::new()
        };
        let colour = if one.with_you || invited { screen::AMBER } else { screen::DIM };
        sheet.text(Font::Small, 77.0, top + 26.0, colour, &status);
    }
    if first > 0 {
        sheet.text(Font::Small, 410.0, 74.0, screen::DIM, b"...");
    }
    if first + SEEN < count {
        sheet.text(Font::Small, 410.0, 340.0, screen::DIM, b"...");
    }

    if state.list.version != 0 {
        let line = format!("Not playing: {} online, {} offline", state.list.online, state.list.offline);
        sheet.text(Font::Small, 29.0, 350.0, screen::DIM, line.as_bytes());
    }
    sheet.buttons(374.0, &[("A", "Invite"), ("B", "Back")]);
    screen::dump("friends", image);
    true
}
