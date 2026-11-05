// The online service the title's network calls are served by: who the player is,
// and how datagrams reach other players.
//
// Nothing here is Xbox-shaped. The kernel side (kernel/net.cpp, kernel/xam.cpp) keeps
// everything the console's API is made of -- XNADDRs, session keys, the title's
// sockets and ports -- and asks this only for what a service does. A peer is a 64-bit
// id the backend chooses (an address, a SteamID, an account number); the kernel never
// looks inside it.
//
// Every build has every backend, and the player chooses one as the game starts
// (MW2_NETWORK, the launcher's NETWORK): none, the default, lan, server or steam,
// which is lan when Steam is not running. A backend is a function that makes it,
// listed below. With none, there is no service, and the title keeps its offline
// system link on this machine.

use crate::env;
use crate::log::{log_info, log_warn};
use crate::runtime::{console, kernel};
use std::ffi::CStr;
use std::sync::OnceLock;

#[deny(unsafe_code)]
mod lan;
#[deny(unsafe_code)]
mod players;
#[deny(unsafe_code)]
pub(crate) mod relay_wire;
#[deny(unsafe_code)]
mod server;
pub mod steam;
mod udp;

/// The console's XSESSION_INFO (session id, host address, key), 60 bytes: a session
/// a friend can join, opaque to the service.
pub const SESSION_INFO_SIZE: usize = 60;
pub type Session = [u8; SESSION_INFO_SIZE];

#[derive(Default)]
pub struct Datagram {
    pub from: u64,
    /// the title's own port numbers
    pub from_port: u16,
    pub to_port: u16,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct Invite {
    /// the host's account()
    pub inviter: u64,
    pub session: Session,
    /// an invitation, not the player choosing to join
    pub from_invite: bool,
}

/// A session listed for matchmaking. Its numbers are the title's contexts and
/// properties, which a search compares; the service compares them without knowing
/// what they mean.
#[derive(Clone, Copy, Default)]
pub struct SessionNumber {
    pub id: u32,
    pub value: u32,
}

#[derive(Clone)]
pub struct SessionListing {
    pub session: Session,
    pub open_public: u32,
    pub open_private: u32,
    pub filled_public: u32,
    pub filled_private: u32,
    pub numbers: Vec<SessionNumber>,
}

#[derive(Clone, Copy, Default)]
pub struct SessionWant {
    pub id: u32,
    pub value: u32,
    /// no less than `value`, not equal to it
    pub at_least: bool,
}

#[derive(Clone, Default)]
pub struct SessionSearch {
    /// open public slots a session must have
    pub room: u32,
    pub most: u32,
    pub wants: Vec<SessionWant>,
}

#[derive(Clone, Copy)]
pub struct SessionFound {
    /// the service's id for the player listing it
    pub host: u64,
    pub session: Session,
    pub open_public: u32,
    pub open_private: u32,
    pub filled_public: u32,
    pub filled_private: u32,
    /// the round trip between the two players, in ms
    pub lag: u32,
}

impl Default for SessionFound {
    fn default() -> Self {
        Self {
            host: 0,
            session: [0; SESSION_INFO_SIZE],
            open_public: 0,
            open_private: 0,
            filled_public: 0,
            filled_private: 0,
            lag: 0,
        }
    }
}

/// A friend running this game, as the friends screen shows them.
#[derive(Clone, Default, PartialEq)]
pub struct Friend {
    pub id: u64,
    pub name: Vec<u8>,
    /// already in this player's party or game
    pub with_you: bool,
    /// why they cannot join: "On another release"; empty if they can
    pub elsewhere: &'static str,
    /// RGBA, avatar_side pixels square; may be empty
    pub avatar: Vec<u8>,
    pub avatar_side: u32,
}

#[derive(Clone, Default)]
pub struct FriendList {
    /// 0: not built yet
    pub version: u64,
    /// by name
    pub playing: Vec<Friend>,
    /// the other friends, not running it
    pub online: i32,
    pub offline: i32,
}

pub trait Service: Send + Sync {
    /// Logs in. False leaves the title offline, as if no backend were built.
    fn start(&mut self) -> bool;

    /// Where other players reach this one: unique among everyone it can reach, and
    /// stable for the run.
    fn local_id(&self) -> u64;
    /// Who the player is, the same every run: the title keeps the player's
    /// multiplayer rank and settings under it. The profile at the first controller
    /// (signin), and unique among everyone in a match.
    fn account(&self) -> u64;
    /// The name the service has for the player, or empty: he then goes by his
    /// profile's. The title shows the first 15 characters.
    fn local_name(&self) -> Vec<u8>;

    /// Unreliable and unordered, like the UDP the title thinks it has. Ports are the
    /// title's: a datagram sent to port 1000 is received on 1000.
    fn send(&self, peer: u64, from_port: u16, to_port: u16, bytes: &[u8]) -> bool;
    /// To everyone who could join this player's games: the title finds system-link
    /// games by broadcasting for them. Never to this player.
    fn broadcast(&self, from_port: u16, to_port: u16, bytes: &[u8]) -> bool;
    /// The next datagram that arrived for `port`, without waiting.
    fn receive(&self, port: u16, out: &mut Datagram) -> bool;

    /// Optional: invitations. A service that can bring a friend to this player's
    /// game is told how to join it -- the console line the title's own server browser
    /// types, or "" when there is nothing to join -- and hands such a line back when
    /// this player accepts an invitation.
    fn advertise(&self, _connect: &[u8]) {}
    fn next_join(&self) -> Option<Vec<u8>> {
        None
    }
    /// Invites everyone this player could invite, for a service without friends to
    /// pick from.
    fn invite_friends(&self) {}

    /// Optional: friends to pick from, for the friends screen. False when the service
    /// has none. The list is copied into `list` only when its version is not the one
    /// `list` holds; asking keeps it fresh for a while, and a service builds it off
    /// the caller's thread.
    fn friends(&self, _list: &mut FriendList) -> bool {
        false
    }
    /// Invites one of them to this player's party or game.
    fn invite_friend(&self, _id: u64) -> bool {
        false
    }

    /// Optional: Xbox LIVE parties and private matches. A service that says yes here
    /// has the player signed in to Live, which opens the title's Xbox LIVE menu; one
    /// that says no leaves it refusing, as offline.
    fn lobbies(&self) -> bool {
        false
    }
    /// What a friend joins to be in this player's party or lobby: a session the
    /// kernel describes in bytes the service only stores and hands back. None when
    /// there is nothing to join.
    fn set_joinable(&self, _joinable: Option<&Session>) {}
    /// The party this player is in has moved to that player.
    fn host_moved(&self, _peer: u64) {}
    /// The next invitation this player accepted, or friend they chose to join,
    /// without waiting. Polled once a frame on the title's main thread.
    fn next_invite(&self) -> Option<Invite> {
        None
    }

    /// Optional: matchmaking. Without it a search finds nothing, and the title lists a
    /// lobby nobody will see and waits in it.
    fn matchmaking(&self) -> bool {
        false
    }
    /// What this player lists now; None when nothing.
    fn list(&self, _listing: Option<&SessionListing>) {}
    /// Whether this player is in a Find Game game: its players reach each other as
    /// well as their host, and any can be the next host.
    fn in_game(&self, _playing: bool) {}
    /// Starts a search, and gives what the results are asked by.
    fn search(&self, _search: &SessionSearch) -> u32 {
        0
    }
    /// Its results once it has finished, without waiting. A search that gets no
    /// answer finishes empty, as one that found nothing.
    fn search_results(&self, _search: u32, _out: &mut Vec<SessionFound>) -> bool {
        false
    }

    /// Optional: a file of the title's the service holds for everyone, so that all
    /// its players have the same bytes.
    fn title_file(&self, _name: &[u8], _out: &mut Vec<u8>) -> bool {
        false
    }
}

// A backend and those tried after it when it does not start.
type Make = fn() -> Box<dyn Service>;
const LAN: &[(&CStr, Make)] = &[(c"lan", lan::make)];
const SERVER: &[(&CStr, Make)] = &[(c"server", server::make)];
const STEAM: &[(&CStr, Make)] = &[(c"steam", steam::make), (c"lan", lan::make)];
const NONE: &[(&CStr, Make)] = &[];

// MW2_NETWORK: what the player chose; none when unset or unknown.
fn chosen() -> &'static [(&'static CStr, Make)] {
    let Some(network) = env::text(c"MW2_NETWORK") else { return NONE };
    match network.to_bytes() {
        b"none" => NONE,
        b"lan" => LAN,
        b"server" => SERVER,
        b"steam" => STEAM,
        _ => {
            log_warn!("online: MW2_NETWORK=%s is none of none, lan, server, steam; playing without a network", network);
            NONE
        }
    }
}

/// Before any thread exists, with steam chosen. A Steam the Flatpak runs is in a
/// process namespace of its own, and tells its clients apart by process id, so a
/// client outside it has its connection dropped mid-call. With such a Steam running,
/// this process joins its namespaces and a child goes on as the game (None); the
/// parent waits for it and has its exit status. None and nothing done everywhere
/// else.
pub fn enter_service_namespace() -> Option<i32> {
    if !std::ptr::eq(chosen(), STEAM) {
        return None;
    }
    steam::enter_namespace()
}

pub(crate) struct Started {
    pub(crate) service: Box<dyn Service>,
    pub(crate) name: &'static CStr,
}

// tools/runtime_diff.rs sets it to a service of its own before anything asks.
pub(crate) static SERVICE: OnceLock<Option<Started>> = OnceLock::new();

fn started() -> Option<&'static Started> {
    SERVICE
        .get_or_init(|| {
            let chosen = chosen();
            for &(name, make) in chosen {
                let mut service = make();
                if service.start() {
                    log_info!("online: the service is %s", name);
                    return Some(Started { service, name });
                }
                log_warn!("online: %s did not start", name);
            }
            if chosen.is_empty() {
                log_info!("online: no network chosen; system link stays on this machine");
            } else {
                log_warn!("online: no service started; the title stays offline");
            }
            None
        })
        .as_ref()
}

/// The service, started on first use: the first backend built in that starts. None
/// when there is none or none could start.
pub fn get() -> Option<&'static dyn Service> {
    started().map(|started| &*started.service)
}

/// Its backend's name, "steam", "lan" or "server"; None without one.
pub fn name() -> Option<&'static CStr> {
    started().map(|started| started.name)
}

/// The service can carry Xbox LIVE lobbies.
pub fn live() -> bool {
    static LIVE: OnceLock<bool> = OnceLock::new();
    *LIVE.get_or_init(|| get().is_some_and(|service| service.lobbies()))
}

// What the System Link menu runs when the player opens it, before its server list
// types the host's connect line. Connect loads the player's stats if they are not
// loaded, but only once it is connecting, which the title refuses.
//
// A player signed in to Live has the Live stats loaded from the start, and the title
// asks such a player which stats to play system link with. The answer here is the
// Live ones: they are there already, and the rank is then the same in a private match
// and in system link.
const SYSTEM_LINK_MENU: [&CStr; 6] = [
    c"xblive_rankedmatch 0",
    c"xblive_privatematch 0",
    c"exec default_systemlink.cfg",
    c"exec default_720p.cfg",
    c"exec dvar_defaults.cfg",
    c"xrequiresignin",
];
const USE_LIVE_STATS: &CStr = c"set useonlinestats 1";

/// Whether a line a service hands over is the one a host advertises (kernel's
/// AdvertiseHosted): "connect", the XNADDR, key and session id in hex, the two slot
/// counts, the nonce in hex, a space between each and nothing else. The line goes to
/// the title's console, which runs whatever follows a ';' or a newline as a command of
/// its own, and on Steam it is lobby data the lobby's owner wrote: anything of another
/// shape is refused.
pub fn is_connect_line(line: &[u8]) -> bool {
    // hex digits wanted per token; 0 is a decimal number of up to ten digits
    const TOKENS: [usize; 6] = [72, 32, 16, 0, 0, 16];
    let Some(mut rest) = line.strip_prefix(b"connect") else { return false };
    for hex in TOKENS {
        let Some(token) = rest.strip_prefix(b" ") else { return false };
        let digits = token.iter().position(|&c| c == b' ').unwrap_or(token.len());
        let fits = |c: &u8| c.is_ascii_digit() || (hex != 0 && c.is_ascii_hexdigit());
        if !token[..digits].iter().all(fits) {
            return false;
        }
        if if hex != 0 { digits != hex } else { !(1..=10).contains(&digits) } {
            return false;
        }
        rest = &token[digits..];
    }
    rest.is_empty()
}

/// Once a frame on the title's main thread: types an accepted invitation's console
/// line, or hands an accepted Live invitation to the title.
pub fn frame() {
    let Some(service) = get() else { return };
    kernel::xam::finish_searches();
    if let Some(invite) = service.next_invite() {
        log_info!(
            "online: joining a friend's Xbox LIVE %s",
            if invite.from_invite { c"invitation" } else { c"session" }
        );
        kernel::xam::accept_invite(invite.inviter, &invite.session, invite.from_invite);
    }
    let Some(connect) = service.next_join() else { return };
    if !is_connect_line(&connect) {
        log_warn!("online: a friend's game was advertised with a line that is not a connect command; not joining it");
        return;
    }
    log_info!("online: joining a friend's game");
    if live() {
        console::run_now(USE_LIVE_STATS);
    }
    for command in SYSTEM_LINK_MENU {
        console::run_now(command);
    }
    console::run_now(&crate::log::c_string(connect));
}
