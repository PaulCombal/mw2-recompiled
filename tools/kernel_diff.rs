// What tools/kernel_diff.cpp calls of the Rust runtime that nothing else exports: the
// online service both sides' XAM and network talk to, which is the rig's own in
// kernel_diff.cpp, and the calls the rest of the runtime makes of XAM.

use crate::runtime::kernel::xam;
use crate::runtime::online::{self, Datagram, Invite, SESSION_INFO_SIZE, Service, SessionFound, SessionListing};
use crate::runtime::online::{SessionSearch, Started};
use std::ffi::{c_char, c_void};

#[repr(C)]
struct Want {
    id: u32,
    value: u32,
    at_least: bool,
}

#[repr(C)]
struct Found {
    host: u64,
    session: [u8; SESSION_INFO_SIZE],
    open_public: u32,
    open_private: u32,
    filled_public: u32,
    filled_private: u32,
    lag: u32,
}

type Sink = unsafe extern "C" fn(to: *mut c_void, bytes: *const c_char, size: usize);

unsafe extern "C" {
    fn rig_service_local_id() -> u64;
    fn rig_service_account() -> u64;
    fn rig_service_local_name(to: *mut c_void, sink: Sink);
    fn rig_service_send(peer: u64, from_port: u16, to_port: u16, bytes: *const u8, size: usize) -> bool;
    fn rig_service_broadcast(from_port: u16, to_port: u16, bytes: *const u8, size: usize) -> bool;
    fn rig_service_receive(port: u16, from: *mut u64, from_port: *mut u16, to: *mut c_void, sink: Sink) -> bool;
    fn rig_service_advertise(line: *const u8, size: usize);
    fn rig_service_lobbies() -> bool;
    fn rig_service_set_joinable(session: *const u8);
    fn rig_service_host_moved(peer: u64);
    fn rig_service_next_invite(inviter: *mut u64, session: *mut u8, from_invite: *mut bool) -> bool;
    fn rig_service_matchmaking() -> bool;
    fn rig_service_list(session: *const u8, counts: *const u32, numbers: *const u32, count: usize);
    fn rig_service_in_game(playing: bool);
    fn rig_service_search(room: u32, most: u32, wants: *const Want, count: usize) -> u32;
    fn rig_service_search_results(search: u32, out: *mut Found, room: usize) -> isize;
    fn rig_service_title_file(name: *const c_char, to: *mut c_void, sink: Sink) -> bool;
}

unsafe extern "C" fn assign(to: *mut c_void, bytes: *const c_char, size: usize) {
    let to = unsafe { &mut *to.cast::<Vec<u8>>() };
    to.clear();
    if size != 0 {
        to.extend_from_slice(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), size) });
    }
}

struct Rig;

impl Service for Rig {
    fn start(&mut self) -> bool {
        true
    }

    fn local_id(&self) -> u64 {
        unsafe { rig_service_local_id() }
    }

    fn account(&self) -> u64 {
        unsafe { rig_service_account() }
    }

    fn local_name(&self) -> Vec<u8> {
        let mut name = Vec::new();
        unsafe { rig_service_local_name((&raw mut name).cast(), assign) };
        name
    }

    fn send(&self, peer: u64, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        unsafe { rig_service_send(peer, from_port, to_port, bytes.as_ptr(), bytes.len()) }
    }

    fn broadcast(&self, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        unsafe { rig_service_broadcast(from_port, to_port, bytes.as_ptr(), bytes.len()) }
    }

    fn receive(&self, port: u16, out: &mut Datagram) -> bool {
        let got = unsafe {
            rig_service_receive(port, &mut out.from, &mut out.from_port, (&raw mut out.bytes).cast(), assign)
        };
        out.to_port = port;
        got
    }

    fn advertise(&self, connect: &[u8]) {
        unsafe { rig_service_advertise(connect.as_ptr(), connect.len()) }
    }

    fn lobbies(&self) -> bool {
        unsafe { rig_service_lobbies() }
    }

    fn set_joinable(&self, joinable: Option<&online::Session>) {
        unsafe { rig_service_set_joinable(joinable.map_or(std::ptr::null(), |session| session.as_ptr())) }
    }

    fn host_moved(&self, peer: u64) {
        unsafe { rig_service_host_moved(peer) }
    }

    fn next_invite(&self) -> Option<Invite> {
        let mut invite = Invite { inviter: 0, session: [0; SESSION_INFO_SIZE], from_invite: false };
        unsafe { rig_service_next_invite(&mut invite.inviter, invite.session.as_mut_ptr(), &mut invite.from_invite) }
            .then_some(invite)
    }

    fn matchmaking(&self) -> bool {
        unsafe { rig_service_matchmaking() }
    }

    fn list(&self, listing: Option<&SessionListing>) {
        let Some(listing) = listing else {
            return unsafe { rig_service_list(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0) };
        };
        let counts = [listing.open_public, listing.open_private, listing.filled_public, listing.filled_private];
        let numbers: Vec<u32> = listing.numbers.iter().flat_map(|number| [number.id, number.value]).collect();
        unsafe { rig_service_list(listing.session.as_ptr(), counts.as_ptr(), numbers.as_ptr(), listing.numbers.len()) }
    }

    fn in_game(&self, playing: bool) {
        unsafe { rig_service_in_game(playing) }
    }

    fn search(&self, search: &SessionSearch) -> u32 {
        let wants: Vec<Want> =
            search.wants.iter().map(|want| Want { id: want.id, value: want.value, at_least: want.at_least }).collect();
        unsafe { rig_service_search(search.room, search.most, wants.as_ptr(), wants.len()) }
    }

    fn search_results(&self, search: u32, out: &mut Vec<SessionFound>) -> bool {
        let mut found: Vec<Found> = (0..16)
            .map(|_| Found {
                host: 0,
                session: [0; SESSION_INFO_SIZE],
                open_public: 0,
                open_private: 0,
                filled_public: 0,
                filled_private: 0,
                lag: 0,
            })
            .collect();
        let count = unsafe { rig_service_search_results(search, found.as_mut_ptr(), found.len()) };
        if count < 0 {
            return false;
        }
        found.truncate(count as usize);
        *out = found
            .into_iter()
            .map(|one| SessionFound {
                host: one.host,
                session: one.session,
                open_public: one.open_public,
                open_private: one.open_private,
                filled_public: one.filled_public,
                filled_private: one.filled_private,
                lag: one.lag,
            })
            .collect();
        true
    }

    fn title_file(&self, name: &[u8], out: &mut Vec<u8>) -> bool {
        let name = crate::log::c_string(name);
        unsafe { rig_service_title_file(name.as_ptr(), (out as *mut Vec<u8>).cast(), assign) }
    }
}

/// The seed's service, before anything asks for one: none, or the rig's.
#[unsafe(no_mangle)]
extern "C" fn rig_online_choose(any: bool) {
    let started = any.then(|| Started { service: Box::new(Rig), name: c"rig" });
    let _ = online::SERVICE.set(started);
    // What the service says once, said before either side runs.
    online::get();
    online::live();
}

#[unsafe(no_mangle)]
extern "C" fn rig_online_frame() {
    online::frame();
}

#[unsafe(no_mangle)]
extern "C" fn rig_xam_post_notification(id: u32, param: u32) {
    xam::post_notification(id, param);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn rig_xam_accept_invite(inviter: u64, session: *const u8, from_invite: bool) {
    xam::accept_invite(inviter, unsafe { &*session.cast::<online::Session>() }, from_invite);
}

#[unsafe(no_mangle)]
extern "C" fn rig_xam_move_player_data(from: u64, to: u64) {
    xam::move_player_data(from, to);
}
