// Steam, through the Steam client the player already runs (client.rs).
//
// The title runs as Spacewar, app 480, which every Steam account can run
// (MW2_STEAM_APPID for another). Players are SteamIDs and datagrams are
// ISteamNetworkingMessages, which punches through NAT or relays through Valve.
//
// Each player sits in a friends-only lobby of their own from the start. The lobby is
// the network the title's broadcasts reach, so a game one member hosts shows up in
// every other member's System Link list. F6, or the Xbox LIVE lobby's "Invite
// friends", opens the friends screen: the friends running this game, whom Steam tells
// apart from Spacewar's own players by a rich presence key of ours; a friend who
// accepts an invitation leaves their lobby for this one. The host's lobby carries what
// joins its game: the Xbox LIVE party or private match it is in ("xsession", which the
// friend's copy takes as an accepted Live invitation), or else the console line of the
// system-link game it hosts ("connect", which the friend's copy types).
//
// System Link is shared with every Steam player, as a local network is shared with
// everyone on it. A player hosting a system-link game is listed, in an invisible lobby
// of its own that searches return; a player whose title looks for games (a broadcast)
// sends it to the friends' lobby and to every host listed, worldwide. What a stranger
// can reach:
//   - a host, while it hosts: its networking accepts their sessions, and its
//     broadcasts -- the answers to a search -- reach them;
//   - a player who searched, on the ports its searches went to: anyone can list a
//     game, so a listed host is a stranger like any other;
//   - a player who chose to join the stranger's game, as on a local network;
//   - a player listed for Find Game or in a Find Game game: anyone, since any of a
//     game's players can be its next host.
// Everything else from a stranger is dropped as it arrives, so a player who stops
// hosting is closed to the strangers it played with. Their sessions are not closed,
// since a host's map change ends its game and starts the next one at once. Xbox LIVE
// parties and private matches stay among friends; Find Game is shared as System Link
// is, with its own listings.

#![deny(unsafe_code)]

// The C++ objects of the Steam client, unsafe by nature.
#[allow(unsafe_code)]
mod client;

use super::{Datagram, FriendList, Invite, Service, Session, SessionFound, SessionListing, SessionSearch};
use crate::env;
use crate::log::{c_string, log_info, log_warn};
use crate::runtime::{signin, version};
use client::{Call, Callback, Client, LinkStatus};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub use client::enter_namespace;

const RESULT_OK: i32 = 1; // EResult
const ENTER_SUCCESS: u32 = 1; // EChatRoomEnterResponse
const FRIENDS_ONLY: i32 = 1; // ELobbyType
const INVISIBLE: i32 = 3;
const MAX_MEMBERS: i32 = 18; // the title's own sv_maxclients
const EQUAL: i32 = 0; // ELobbyComparison
const WORLDWIDE: i32 = 3; // ELobbyDistanceFilter
const IMMEDIATE: i32 = 4; // EFriendFlags: friends, not requests or groups
const OFFLINE: i32 = 0; // EPersonaState
// ESteamNetworkingConnectionState
const CONNECTED: i32 = 3;
const CLOSED_BY_PEER: i32 = 4;
const PROBLEM: i32 = 5;

// The public list of system-link games: lobbies with this key, whose value is the
// release's (version::NETWORK), so that only copies that can play together see each
// other. A listing's "host" is the SteamID that hosts. Nobody joins a listing, but a
// search never returns a full lobby, so it takes Steam's most members: a stranger
// cannot hide it by filling it.
const LISTING_KEY: &CStr = c"mw2recomp";
const LISTING_MEMBERS: i32 = 250;
// The disc version and the update do not play together either.
fn listing_version() -> CString {
    let tu0 = if cfg!(mw2_version_tu0) { "-tu0" } else { "" };
    c_string(format!("{}{tu0}", version::NETWORK))
}
// The friends screen's list is built this often while it is asked for, and for this
// long after.
const FRIENDS_EVERY: Duration = Duration::from_secs(1);
const FRIENDS_WATCHED: Duration = Duration::from_secs(3);
// What a copy says it plays, in its rich presence under LISTING_KEY: the executable,
// whose invitations the other cannot take, and the release.
const TITLE: &str = if cfg!(mw2_title_mp) { "mp" } else { "sp" };
fn playing() -> CString {
    c_string([TITLE.as_bytes(), b":", listing_version().as_bytes()].concat())
}
/// Why a friend who plays `playing` cannot join this copy, or "".
pub(crate) fn elsewhere(playing: &[u8]) -> &'static str {
    let colon = playing.iter().position(|&c| c == b':');
    let title = &playing[..colon.unwrap_or(playing.len())];
    if colon.is_none() || (title != b"mp" && title != b"sp") {
        return "On another release";
    }
    if title != TITLE.as_bytes() {
        return if title == b"mp" { "In multiplayer" } else { "In the campaign or Spec Ops" };
    }
    if playing[colon.unwrap() + 1..] == *listing_version().as_bytes() { "" } else { "On another release" }
}
// The games Find Game can join: lobbies of their own with this key, again with the
// release's value, whose "host" is the SteamID hosting and whose "game" is what it
// lists, in one value so that a search reads a whole one.
const GAME_KEY: &CStr = c"mw2game";
const GAMES_ASKED: i32 = 200;
// A search opens a link to each host it found, on a port no title uses, and takes the
// link's ping as the round trip. A host with no link by then cannot be reached, and is
// left out.
const PROBE_PORT: u16 = 65535;
const PROBE_WITHIN: Duration = Duration::from_secs(4);
// A list Steam has not given by then is taken as empty, as a list that never comes
// would hold up every later one.
const LIST_WITHIN: Duration = Duration::from_secs(10);
// What a listing may say. It is anyone's words, read under the lock the title's sends
// take.
const GAME_MOST_TEXT: usize = 1024;
const HUNTS_EVERY: Duration = Duration::from_millis(100);
// A listing Steam would not make is asked for again this often.
const LIST_AGAIN_EVERY: Duration = Duration::from_secs(5);
// The title broadcasts a search every few seconds while its list is open; Steam is
// asked for the list at most this often.
const SEARCH_EVERY: Duration = Duration::from_secs(2);
// The links to other players are looked at this often; one that goes bad is told
// about at most this often.
const LINKS_EVERY: Duration = Duration::from_secs(1);
const POOR_LINK_EVERY: Duration = Duration::from_secs(5);
// Unreliable, sent at once, and a session that broke is started again rather than
// refused.
const SEND_FLAGS: i32 = 1 | 32;

/// The playlists Find Game offers where no relay hands them out: the relay's own
/// file, so a release has one list (build.rs names it).
const PLAYLISTS: &[u8] = include_bytes!(env!("MW2_PLAYLISTS"));

fn since(then: Option<Instant>, now: Instant) -> Duration {
    then.map_or(Duration::MAX, |then| now.saturating_duration_since(then))
}

fn hex(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|byte| format!("{byte:02x}").into_bytes()).collect()
}

/// A session's 120 hexadecimal digits, read as the C++ read them.
pub(crate) fn read_session(hex: Option<&[u8]>, session: &mut Session) -> bool {
    let Some(hex) = hex.filter(|hex| hex.len() == 2 * session.len()) else { return false };
    let text = c_string(hex);
    let text = text.as_bytes_with_nul();
    for (i, byte) in session.iter_mut().enumerate() {
        let Ok(rest) = CStr::from_bytes_until_nul(&text[(2 * i).min(text.len() - 1)..]) else { return false };
        match env::scan_hex_pair(rest) {
            Some(value) => *byte = value as u8,
            None => return false,
        }
    }
    true
}

/// A Find Game listing is anyone's words: what does not read as one is passed over,
/// and what it says about its host is only where the title will ask.
pub(crate) fn read_game(text: Option<&[u8]>, found: &mut SessionFound, numbers: &mut BTreeMap<u32, u32>) -> bool {
    let Some(text) = text.filter(|text| text.len() <= GAME_MOST_TEXT) else { return false };
    let Some(slash) = text.iter().position(|&c| c == b'/') else { return false };
    if !read_session(Some(&text[..slash]), &mut found.session) {
        return false;
    }
    let Some(([open_public, open_private, filled_public, filled_private], used)) =
        env::scan_four_counts(&c_string(&text[slash..]))
    else {
        return false;
    };
    (found.open_public, found.open_private, found.filled_public, found.filled_private) =
        (open_public, open_private, filled_public, filled_private);
    let most = MAX_MEMBERS as u32;
    if [open_public, open_private, filled_public, filled_private].iter().any(|&count| count > most) {
        return false;
    }
    let mut at = slash + used;
    while at < text.len() {
        let Some((id, value, used)) = env::scan_hex_assignment(&c_string(&text[at..])) else { return false };
        numbers.insert(id, value);
        at += used;
    }
    true
}

/// What a player lists for Find Game: the session in hex, its four slot counts and
/// its numbers, in one value so that a search reads a whole one (read_game).
pub(crate) fn game_text(listing: &SessionListing) -> Vec<u8> {
    let mut game = hex(&listing.session);
    game.extend_from_slice(
        format!(
            "/{}/{}/{}/{}/",
            listing.open_public, listing.open_private, listing.filled_public, listing.filled_private
        )
        .as_bytes(),
    );
    for one in &listing.numbers {
        game.extend_from_slice(format!("{:x}={:x},", one.id, one.value).as_bytes());
    }
    game
}

struct Hunt {
    search: SessionSearch,
    found: Vec<SessionFound>,
    created: Instant,
    probing: Option<Instant>,
    asked: bool,
    answered: bool,
    done: bool,
}

#[derive(Default)]
struct State {
    owner: bool,
    followed: bool,
    advert: Vec<u8>,
    /// what joins this player's Live party or lobby, in hex
    session: Vec<u8>,
    /// this player's entry in the public list
    listing: u64,
    /// and whether it shows
    listed: bool,
    listing_call: Call,
    search_call: Call,
    searched: Option<Instant>,
    /// the hosts listed when Steam was last asked
    hosts: Vec<u64>,
    /// every listed host a search went to
    searched_hosts: BTreeSet<u64>,
    /// and the ports it went to
    search_ports: BTreeSet<u16>,
    /// whose sessions this player accepted, or sent to in a Find Game game
    strangers: BTreeSet<u64>,
    /// hosts it found that it sent to on its own
    contacted: BTreeSet<u64>,

    /// what this player lists for Find Game
    game: Vec<u8>,
    /// what the listing says
    game_shown: Vec<u8>,
    /// its entry, kept for the run
    game_listing: u64,
    /// and whether it shows
    game_listed: bool,
    game_listing_call: Call,
    hunt_call: Call,
    /// Find Game's searches
    hunts: BTreeMap<u32, Hunt>,
    last_hunt: u32,
    hunt_asked: u32,
    /// every host Find Game found
    found_hosts: BTreeSet<u64>,
    /// told about once each
    dropped_from: BTreeSet<u64>,
    /// as in_game says
    playing: bool,
    joins: VecDeque<Vec<u8>>,
    invites: VecDeque<Invite>,
}

/// A friend's 64-pixel picture, read again only when Steam has another.
#[derive(Default)]
struct Avatar {
    image: i32,
    rgba: Vec<u8>,
    side: u32,
}

/// A link WatchLinks follows.
#[derive(Default)]
struct Link {
    state: i32,
    /// when it was last said to be poor
    told: Option<Instant>,
}

/// What only the callback thread touches.
#[derive(Default)]
struct Watching {
    friends_built: Option<Instant>,
    list_asked: Option<Instant>,
    hunts_watched: Option<Instant>,
    links_watched: Option<Instant>,
    links: BTreeMap<u64, Link>,
    avatars: BTreeMap<u64, Avatar>,
}

struct Inner {
    client: Client,
    self_id: u64,
    app: u64,
    name: Vec<u8>,
    epoch: Instant,

    stopping: AtomicBool,
    create_call: AtomicU64,
    join_call: AtomicU64,
    lobby: AtomicU64,

    lock: Mutex<State>,
    watching: Mutex<Watching>,
    // The friends screen's list, built on the callback thread.
    friend_list: Mutex<FriendList>,
    /// when it was last asked for, in ms after `epoch` and one more; 0 never
    friends_wanted: AtomicU64,
}

struct Steam {
    inner: Option<Arc<Inner>>,
}

pub fn make() -> Box<dyn Service> {
    Box::new(Steam { inner: None })
}

fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        locked(&self.lock)
    }

    fn account(&self) -> u64 {
        signin::first().id
    }

    fn send_to(&self, peer: u64, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        // The sender's port ahead of the title's bytes; the channel is the port it is
        // for.
        let mut packet = Vec::with_capacity(2 + bytes.len());
        packet.extend_from_slice(&from_port.to_be_bytes());
        packet.extend_from_slice(bytes);
        self.client.send(peer, &packet, SEND_FLAGS, i32::from(to_port)) == RESULT_OK
    }

    fn receive(&self, port: u16, out: &mut Datagram) -> bool {
        loop {
            let Some((peer, bytes)) = self.client.receive(i32::from(port)) else { return false };
            let from = (bytes.len() >= 2).then(|| peer.steam_id()).flatten();
            let whole = from.is_some_and(|from| self.reaches(from, port));
            if let Some(from) = from
                && !whole
                && port != PROBE_PORT
                && self.state().dropped_from.insert(from)
            {
                log_info!("online: dropping what %016llx sends to port %u", from, u32::from(port));
            }
            if let (true, Some(from)) = (whole, from) {
                *out = Datagram {
                    from,
                    from_port: u16::from_be_bytes([bytes[0], bytes[1]]),
                    to_port: port,
                    bytes: bytes[2..].to_vec(),
                };
                return true;
            }
        }
    }

    // Callbacks, read off the pipe the way libsteam_api's manual dispatch does.
    // Between frames is soon enough for anything here.
    fn run(&self) {
        while !self.stopping.load(Relaxed) {
            self.client.callbacks(|callback| self.handle(callback));
            let mut watching = locked(&self.watching);
            let now = Instant::now();
            let wanted = self.friends_wanted.load(Relaxed);
            let wanted = (wanted != 0).then(|| self.epoch + Duration::from_millis(wanted - 1));
            if since(wanted, now) < FRIENDS_WATCHED && since(watching.friends_built, now) >= FRIENDS_EVERY {
                watching.friends_built = Some(now);
                self.build_friends(&mut watching);
            }
            let now = Instant::now();
            if since(watching.list_asked, now) >= LIST_AGAIN_EVERY {
                watching.list_asked = Some(now);
                let mut state = self.state();
                if state.listing == 0 {
                    self.list_locked(&mut state);
                }
                if state.game_listing == 0 {
                    self.publish_game_locked(&mut state);
                }
            }
            let now = Instant::now();
            if since(watching.hunts_watched, now) >= HUNTS_EVERY {
                watching.hunts_watched = Some(now);
                self.watch_hunts(now);
            }
            let now = Instant::now();
            if since(watching.links_watched, now) >= LINKS_EVERY {
                watching.links_watched = Some(now);
                self.watch_links(&mut watching, now);
            }
            drop(watching);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn handle(&self, callback: Callback) {
        match callback {
            Callback::CallCompleted(call) => {
                if call == self.create_call.load(Relaxed) {
                    let (answered, created) = self.client.lobby_created(call);
                    if answered && created.result == RESULT_OK {
                        self.entered(created.lobby);
                    } else {
                        log_warn!("online: Steam would not make a lobby (result %d)", created.result);
                    }
                } else if call == self.join_call.load(Relaxed) {
                    let (answered, entered) = self.client.lobby_entered(call);
                    if answered && entered.response == ENTER_SUCCESS {
                        self.entered(entered.lobby);
                    } else {
                        log_warn!("online: could not enter the friend's lobby (response %u)", entered.response);
                    }
                } else {
                    // Asked for on the title's thread, under the lock, so a call is
                    // known here by the time it completes.
                    let mut state = self.state();
                    if call == state.listing_call {
                        self.listed(&mut state, call);
                    } else if call == state.search_call {
                        self.found(&mut state, call);
                    } else if call == state.game_listing_call {
                        self.game_listed(&mut state, call);
                    } else if call == state.hunt_call {
                        self.games_found(&mut state, call);
                    }
                }
            }
            Callback::LobbyJoinRequested(lobby) => self.join(lobby),
            Callback::PresenceJoinRequested(connect) => {
                // "+connect_lobby <id>", as the rich presence below puts it.
                if let Some(lobby) = env::scan_connect_lobby(&c_string(connect)) {
                    self.join(lobby);
                }
            }
            Callback::LobbyDataUpdate { lobby, member } => {
                if lobby == self.lobby.load(Relaxed) && member == lobby {
                    self.owner_changed();
                    self.follow();
                }
            }
            Callback::SessionRequest(remote) => {
                // From someone in this player's lobby, or from anyone while this player
                // hosts a system-link game, is listed for Find Game or is in a Find
                // Game game: Spacewar is everyone's. A session this player opened needs
                // no request.
                let Some(who) = remote.steam_id() else { return };
                let member = self.in_lobby(who);
                let mut take = member;
                if !take {
                    let mut state = self.state();
                    take = !state.advert.is_empty() || !state.game.is_empty() || state.playing;
                    if take {
                        state.strangers.insert(who);
                    }
                }
                if take {
                    self.client.accept(&remote);
                }
                log_info!(
                    "online: %s asks for a link: %s",
                    self.who(who).as_c_str(),
                    if member {
                        c"in the lobby, taken"
                    } else if take {
                        c"taken, for a game here"
                    } else {
                        c"refused, being neither in the lobby nor at a game here"
                    }
                );
            }
            Callback::Other => {}
        }
    }

    fn join(&self, lobby: u64) {
        if lobby == 0 {
            return;
        }
        if lobby == self.lobby.load(Relaxed) {
            // Already in it, with the host's party or game followed once: since then
            // the host may have started another, or this player left it. Taking the
            // invitation again follows the host again.
            log_info!("online: invited again to lobby %016llx; following its owner", lobby);
            self.state().followed = false;
            self.follow();
            return;
        }
        log_info!("online: invited; entering lobby %016llx", lobby);
        let old = self.lobby.swap(0, Relaxed);
        if old != 0 {
            self.client.leave_lobby(old);
        }
        self.join_call.store(self.client.join_lobby(lobby), Relaxed);
    }

    fn entered(&self, lobby: u64) {
        let mine = self.client.owner(lobby) == self.self_id;
        {
            let mut state = self.state();
            self.lobby.store(lobby, Relaxed);
            state.owner = mine;
            state.followed = false;
            self.publish_locked(&state);
        }
        // Friends can join from their list, and an invitation that starts the game
        // says which lobby.
        self.client.set_rich_presence(c"connect", &c_string(format!("+connect_lobby {lobby}")));
        log_info!("online: in lobby %016llx%s", lobby, if mine { c", its owner" } else { c"" });
        self.follow();
    }

    // A player who hands the lobby on was in its game already, and is not sent back
    // to it.
    fn owner_changed(&self) {
        let lobby = self.lobby.load(Relaxed);
        let mine = self.client.owner(lobby) == self.self_id;
        let mut state = self.state();
        if mine == state.owner {
            return;
        }
        log_info!(
            "online: %s lobby %016llx",
            if mine { c"now owns" } else { c"no longer owns" },
            self.lobby.load(Relaxed)
        );
        state.owner = mine;
        state.followed = !mine;
        self.publish_locked(&state);
    }

    // The owner's game, once per lobby entered: a friend who accepted an invitation
    // lands in it, and is not pulled back in each time the host starts another --
    // only when invited again (join).
    fn follow(&self) {
        let mut state = self.state();
        let lobby = self.lobby.load(Relaxed);
        if state.owner || state.followed || lobby == 0 {
            return;
        }
        let mut session = [0u8; super::SESSION_INFO_SIZE];
        if read_session(self.client.lobby_data(lobby, c"xsession").as_deref(), &mut session) {
            // The host's account, which is his profile and not his Steam id; a host
            // of a version before profiles says none, and his was the Steam id.
            let inviter = match self.client.lobby_data(lobby, c"account").filter(|account| !account.is_empty()) {
                Some(account) => env::parse_number(&c_string(account), 16).0,
                None => self.client.owner(lobby),
            };
            state.invites.push_back(Invite { inviter, session, from_invite: true });
            state.followed = true;
            return;
        }
        let Some(connect) = self.client.lobby_data(lobby, c"connect").filter(|connect| !connect.is_empty()) else {
            return;
        };
        state.joins.push_back(connect);
        state.followed = true;
    }

    fn publish_locked(&self, state: &State) {
        let lobby = self.lobby.load(Relaxed);
        if !state.owner || lobby == 0 {
            return;
        }
        self.client.set_lobby_data(lobby, c"connect", &c_string(state.advert.clone()));
        self.client.set_lobby_data(lobby, c"xsession", &c_string(state.session.clone()));
        self.client.set_lobby_data(lobby, c"account", &c_string(format!("{:012x}", self.account())));
    }

    // The public list. A listing is made the first time this player hosts a
    // system-link game and kept for the run, searchable only while it hosts one.
    fn list_locked(&self, state: &mut State) {
        let hosting = !state.advert.is_empty();
        if state.listing != 0 {
            if self.client.set_joinable(state.listing, hosting) && hosting != state.listed {
                log_info!(
                    "online: %s",
                    if hosting {
                        c"this game is listed for every Steam player"
                    } else {
                        c"this game is no longer listed"
                    }
                );
            }
            state.listed = hosting;
        } else if hosting && state.listing_call == 0 {
            state.listing_call = self.client.create_lobby(INVISIBLE, LISTING_MEMBERS);
        }
    }

    fn listed(&self, state: &mut State, call: Call) {
        state.listing_call = 0;
        let (answered, created) = self.client.lobby_created(call);
        if !answered || created.result != RESULT_OK {
            log_warn!("online: Steam would not list this game (result %d)", created.result);
            return;
        }
        state.listing = created.lobby;
        let host = c_string(self.self_id.to_string());
        self.client.set_lobby_data(state.listing, LISTING_KEY, &listing_version());
        self.client.set_lobby_data(state.listing, c"host", &host);
        self.list_locked(state);
    }

    // The Find Game listing, made the first time this player hosts a game it lists
    // and kept for the run, searchable while there is one.
    fn publish_game_locked(&self, state: &mut State) {
        let listed = !state.game.is_empty();
        if state.game_listing == 0 {
            if listed && state.game_listing_call == 0 {
                state.game_listing_call = self.client.create_lobby(INVISIBLE, LISTING_MEMBERS);
            }
            return;
        }
        if listed
            && state.game != state.game_shown
            && self.client.set_lobby_data(state.game_listing, c"game", &c_string(state.game.clone()))
        {
            state.game_shown = state.game.clone();
        }
        if self.client.set_joinable(state.game_listing, listed) && listed != state.game_listed {
            log_info!(
                "online: %s",
                if listed {
                    c"this game is listed for Find Game"
                } else {
                    c"this game is no longer listed for Find Game"
                }
            );
        }
        state.game_listed = listed;
    }

    fn game_listed(&self, state: &mut State, call: Call) {
        state.game_listing_call = 0;
        let (answered, created) = self.client.lobby_created(call);
        if !answered || created.result != RESULT_OK {
            log_warn!("online: Steam would not list this game for Find Game (result %d)", created.result);
            return;
        }
        state.game_listing = created.lobby;
        let host = c_string(self.self_id.to_string());
        self.client.set_lobby_data(state.game_listing, GAME_KEY, &listing_version());
        self.client.set_lobby_data(state.game_listing, c"host", &host);
        self.publish_game_locked(state);
    }

    // Steam answers one list at a time, and a request's filters hold for it alone.
    fn start_hunt_locked(&self, state: &mut State) {
        if state.search_call != 0 || state.hunt_call != 0 {
            return;
        }
        let Some((&id, hunt)) = state.hunts.iter_mut().find(|(_, hunt)| !hunt.asked) else { return };
        hunt.asked = true;
        self.client.add_string_filter(GAME_KEY, &listing_version(), EQUAL);
        self.client.add_distance_filter(WORLDWIDE);
        self.client.result_count(GAMES_ASKED);
        let call = self.client.request_lobby_list();
        if call == 0 {
            hunt.done = true;
        }
        state.hunt_asked = id;
        state.hunt_call = call;
    }

    fn host_of(&self, lobby: u64) -> u64 {
        self.client.lobby_data(lobby, c"host").map_or(0, |host| env::parse_number(&c_string(host), 10).0)
    }

    fn games_found(&self, state: &mut State, call: Call) {
        state.hunt_call = 0;
        let (answered, matching) = self.client.lobby_list(call);
        let asked = state.hunt_asked;
        let State { hunts, found_hosts, .. } = &mut *state;
        let Some(hunt) = hunts.get_mut(&asked) else { return };
        if !answered {
            hunt.done = true;
        }
        let mut i = 0;
        while answered && i < matching && hunt.found.len() < hunt.search.most as usize {
            let lobby = self.client.lobby_by_index(i as i32);
            i += 1;
            let mut found = SessionFound { host: self.host_of(lobby), ..SessionFound::default() };
            let mut numbers = BTreeMap::new();
            if found.host == 0
                || found.host == self.self_id
                || !read_game(self.client.lobby_data(lobby, c"game").as_deref(), &mut found, &mut numbers)
                || found.open_public < hunt.search.room
            {
                continue;
            }
            let fits = hunt.search.wants.iter().all(|want| {
                numbers
                    .get(&want.id)
                    .is_some_and(|&number| if want.at_least { number >= want.value } else { number == want.value })
            });
            let again = hunt.found.iter().any(|other| other.host == found.host);
            if !fits || again {
                continue;
            }
            hunt.found.push(found);
            found_hosts.insert(found.host);
            self.send_to(found.host, 0, PROBE_PORT, &[0]);
        }
        log_info!("online: Find Game: %u listed on Steam, %llu fit", matching, hunt.found.len() as u64);
        hunt.answered = true;
        hunt.probing = Some(Instant::now());
        self.start_hunt_locked(state);
    }

    // A search ends when every host it found has a link, or the time is up.
    fn watch_hunts(&self, now: Instant) {
        let mut probe = Datagram::default();
        while self.receive(PROBE_PORT, &mut probe) {}
        let mut state = self.state();
        if state.search_call != 0 && since(state.searched, now) >= LIST_WITHIN {
            state.search_call = 0;
        }
        self.start_hunt_locked(&mut state);
        let State { hunts, hunt_call, hunt_asked, .. } = &mut *state;
        for (&id, hunt) in hunts.iter_mut() {
            if hunt.done {
                continue;
            }
            if !hunt.answered {
                if now.saturating_duration_since(hunt.created) < LIST_WITHIN {
                    continue;
                }
                if hunt.asked && id == *hunt_asked {
                    *hunt_call = 0;
                }
                hunt.done = true;
                continue;
            }
            let mut lags = BTreeMap::new();
            for found in &hunt.found {
                let mut status = LinkStatus::default();
                if self.client.session(found.host, Some(&mut status)) == CONNECTED && status.ping >= 0 {
                    lags.insert(found.host, status.ping as u32);
                }
            }
            if lags.len() < hunt.found.len() && since(hunt.probing, now) < PROBE_WITHIN {
                continue;
            }
            hunt.found.retain_mut(|found| match lags.get(&found.host) {
                None => {
                    log_info!("online: Find Game: no link to %016llx in time", found.host);
                    false
                }
                Some(&lag) => {
                    found.lag = lag;
                    true
                }
            });
            hunt.found.sort_by_key(|found| found.lag);
            hunt.found.truncate(hunt.search.most as usize);
            hunt.done = true;
        }
    }

    // Asks Steam for the hosts listed, unless it is already asked or was asked a
    // moment ago.
    fn search_locked(&self, state: &mut State) {
        let now = Instant::now();
        if state.search_call != 0 || state.hunt_call != 0 || since(state.searched, now) < SEARCH_EVERY {
            return;
        }
        state.searched = Some(now);
        // The filters hold for the next request only.
        self.client.add_string_filter(LISTING_KEY, &listing_version(), EQUAL);
        self.client.add_distance_filter(WORLDWIDE);
        state.search_call = self.client.request_lobby_list();
    }

    fn found(&self, state: &mut State, call: Call) {
        state.search_call = 0;
        let (answered, matching) = self.client.lobby_list(call);
        if !answered {
            return;
        }
        let mut found = Vec::new();
        for i in 0..matching {
            let id = self.host_of(self.client.lobby_by_index(i as i32));
            if id != 0 && id != self.self_id {
                found.push(id);
            }
        }
        if found != state.hosts {
            log_info!("online: %llu system-link game(s) listed on Steam", found.len() as u64);
        }
        state.hosts = found;
        self.start_hunt_locked(state);
    }

    // The friends running this game, by the key Start sets, and how many others there
    // are. Steam has all of it at hand: nothing here waits on the network.
    fn build_friends(&self, watching: &mut Watching) {
        let mut list = FriendList::default();
        let lobby = self.lobby.load(Relaxed);
        let count = self.client.friend_count(IMMEDIATE);
        for i in 0..count {
            let id = self.client.friend_by_index(i, IMMEDIATE);
            let played = self.client.game_played(id);
            let marker = match played {
                Some((game, _)) if game == self.app => self.client.rich_presence(id, LISTING_KEY).unwrap_or_default(),
                _ => Vec::new(),
            };
            if marker.is_empty() {
                if self.client.persona_state(id) == OFFLINE {
                    list.offline += 1;
                } else {
                    list.online += 1;
                }
                continue;
            }
            let avatar = self.avatar_of(watching, id);
            list.playing.push(super::Friend {
                id,
                name: self.client.friend_name(id).unwrap_or_default(),
                with_you: lobby != 0 && played.is_some_and(|(_, theirs)| theirs == lobby),
                elsewhere: elsewhere(&marker),
                avatar: avatar.rgba.clone(),
                avatar_side: avatar.side,
            });
        }
        list.playing.sort_by_key(|one| one.name.to_ascii_lowercase());

        let mut held = locked(&self.friend_list);
        let same = held.online == list.online && held.offline == list.offline && held.playing == list.playing;
        if held.version != 0 && same {
            return;
        }
        list.version = held.version + 1;
        *held = list;
    }

    // Steam answers -1 while it is still fetching one.
    fn avatar_of<'a>(&self, watching: &'a mut Watching, id: u64) -> &'a Avatar {
        let avatar = watching.avatars.entry(id).or_default();
        let image = self.client.medium_avatar(id);
        if image <= 0 || image == avatar.image {
            return avatar;
        }
        let Some((width, height)) = self.client.image_size(image) else { return avatar };
        if width != height || width == 0 || width > 256 {
            return avatar;
        }
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        if !self.client.image_rgba(image, &mut rgba) {
            return avatar;
        }
        *avatar = Avatar { image, rgba, side: width };
        avatar
    }

    fn session_open(&self, peer: u64) -> bool {
        // connecting, finding a route, connected
        (1..=3).contains(&self.client.session(peer, None))
    }

    // Whether a datagram from this peer reaches the title. Steam opens a session only
    // to someone this player accepted, or sent to first. While it takes anyone's link
    // it takes anyone: two players who search can have found each other, and the
    // first's session is the second's.
    fn reaches(&self, peer: u64, port: u16) -> bool {
        {
            let state = self.state();
            if state.contacted.contains(&peer) || !state.advert.is_empty() || !state.game.is_empty() || state.playing {
                return true;
            }
            let system = state.searched_hosts.contains(&peer);
            if !system && !state.found_hosts.contains(&peer) && !state.strangers.contains(&peer) {
                return true; // a lobby member, or one that was
            }
            if system && state.search_ports.contains(&port) {
                return true;
            }
        }
        self.in_lobby(peer)
    }

    fn members(&self) -> Vec<u64> {
        let lobby = self.lobby.load(Relaxed);
        if lobby == 0 {
            return Vec::new();
        }
        (0..self.client.member_count(lobby)).map(|i| self.client.member(lobby, i)).collect()
    }

    fn in_lobby(&self, player: u64) -> bool {
        let lobby = self.lobby.load(Relaxed);
        lobby != 0 && (0..self.client.member_count(lobby)).any(|i| self.client.member(lobby, i) == player)
    }

    fn who(&self, id: u64) -> CString {
        let name = self.client.friend_name(id).filter(|name| !name.is_empty()).unwrap_or_else(|| b"?".to_vec());
        let mut text = name;
        text.extend_from_slice(format!(" ({id:016x})").as_bytes());
        // what the C++'s 160 characters held
        text.truncate(159);
        c_string(text)
    }

    // The links to everyone this player talks to through Steam: the lobby's, the
    // strangers at its game, the hosts it went to. The log says when one connects,
    // when one goes bad and why one ends.
    fn watch_links(&self, watching: &mut Watching, now: Instant) {
        let mut peers = BTreeSet::new();
        {
            let state = self.state();
            peers.extend(state.strangers.iter().copied());
            peers.extend(state.contacted.iter().copied());
        }
        peers.extend(self.members());
        peers.extend(watching.links.keys().copied());
        peers.remove(&self.self_id);
        for peer in peers {
            let mut status = LinkStatus::default();
            let state = self.client.session(peer, Some(&mut status));
            let link = watching.links.entry(peer).or_default();
            if state != link.state {
                let who = self.who(peer);
                if state == CONNECTED {
                    log_info!(
                        "online: link to %s: connected, %d ms, %.0f%% of packets arrive",
                        who.as_c_str(),
                        status.ping,
                        100.0 * f64::from(status.quality_local)
                    );
                } else if state == CLOSED_BY_PEER || state == PROBLEM {
                    log_info!(
                        "online: link to %s: %s (reason %d: %s)",
                        who.as_c_str(),
                        if state == CLOSED_BY_PEER { c"closed by them" } else { c"lost" },
                        status.end_reason,
                        c_string(status.end_debug.clone()).as_c_str()
                    );
                } else {
                    log_info!(
                        "online: link to %s: %s",
                        who.as_c_str(),
                        match state {
                            1 => c"connecting",
                            2 => c"finding a route",
                            _ => c"none",
                        }
                    );
                }
                link.state = state;
            } else if state == CONNECTED
                && since(link.told, now) >= POOR_LINK_EVERY
                // -1 until Steam has a measure.
                && ((status.quality_local >= 0.0 && status.quality_local < 0.9)
                    || (status.quality_remote >= 0.0 && status.quality_remote < 0.9)
                    || status.queue_microseconds >= 100_000)
            {
                link.told = Some(now);
                log_info!(
                    "online: link to %s is poor: %d ms, %.0f%% of packets arrive here and %.0f%% there, %.0f ms queued to send, %.1f KB/s out of %.1f allowed, %.1f KB/s in",
                    self.who(peer).as_c_str(),
                    status.ping,
                    100.0 * f64::from(status.quality_local),
                    100.0 * f64::from(status.quality_remote),
                    status.queue_microseconds as f64 / 1000.0,
                    f64::from(status.out_bytes) / 1024.0,
                    f64::from(status.send_rate) / 1024.0,
                    f64::from(status.in_bytes) / 1024.0
                );
            }
        }
        watching.links.retain(|_, link| link.state > 0);
    }
}

impl Steam {
    fn inner(&self) -> &Inner {
        self.inner.as_ref().unwrap()
    }
}

impl Service for Steam {
    fn start(&mut self) -> bool {
        let app = env::text(c"MW2_STEAM_APPID").unwrap_or(c"480").to_owned();
        let Some(client) = Client::open(&app) else { return false };
        let self_id = client.steam_id();
        let name = client.persona_name();
        log_info!(
            "online: steam as \"%s\" (%016llx), app %s%s",
            c_string(name.clone()).as_c_str(),
            self_id,
            app.as_c_str(),
            if client.logged_on() { c"" } else { c", offline" }
        );

        // What tells this game's players from Spacewar's in a friend list: Steam
        // shares rich presence among friends running the same app.
        client.set_rich_presence(LISTING_KEY, &playing());
        let create_call = client.create_lobby(FRIENDS_ONLY, MAX_MEMBERS);
        let inner = Arc::new(Inner {
            app: env::parse_number(&app, 10).0,
            client,
            self_id,
            name,
            epoch: Instant::now(),
            stopping: AtomicBool::new(false),
            create_call: AtomicU64::new(create_call),
            join_call: AtomicU64::new(0),
            lobby: AtomicU64::new(0),
            lock: Mutex::new(State::default()),
            watching: Mutex::new(Watching::default()),
            friend_list: Mutex::new(FriendList::default()),
            friends_wanted: AtomicU64::new(0),
        });
        let thread = Arc::clone(&inner);
        std::thread::spawn(move || thread.run());
        self.inner = Some(inner);
        true
    }

    fn local_id(&self) -> u64 {
        self.inner().self_id
    }

    // The profile at the first controller; until there were profiles it was the Steam
    // account, and the rank is still under that.
    fn account(&self) -> u64 {
        self.inner().account()
    }

    fn local_name(&self) -> Vec<u8> {
        self.inner().name.clone()
    }

    // A listed host this player sends to on its own -- the host of a game it joins --
    // may answer anywhere. A stranger who came to this player's game is answered, but
    // stays a stranger.
    fn send(&self, peer: u64, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        let inner = self.inner();
        {
            let mut state = inner.state();
            if (state.searched_hosts.contains(&peer) || state.found_hosts.contains(&peer))
                && !state.strangers.contains(&peer)
            {
                state.contacted.insert(peer);
            }
            // Sending opens the session with no request to take: a game's other
            // players are remembered here, to be closed to afterwards.
            if state.playing {
                state.strangers.insert(peer);
            }
        }
        inner.send_to(peer, from_port, to_port, bytes)
    }

    // To the friends' lobby and, from a host, to the strangers it talks to or, from a
    // player who hosts nothing, to every host listed. The title broadcasts only to
    // look for system-link games and to answer such a search.
    fn broadcast(&self, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        let inner = self.inner();
        let mut to = Vec::new();
        {
            let mut state = inner.state();
            if state.advert.is_empty() {
                inner.search_locked(&mut state);
                to = state.hosts.clone();
                let hosts = state.hosts.clone();
                state.searched_hosts.extend(hosts.iter().copied());
                if !hosts.is_empty() {
                    state.search_ports.insert(to_port);
                }
            } else {
                // The set stays as it is: it is how this player closes to them
                // afterwards, and a link that dropped can come back.
                to.extend(state.strangers.iter().copied().filter(|&stranger| inner.session_open(stranger)));
            }
        }
        to.extend(inner.members());
        to.sort_unstable();
        to.dedup();
        for peer in to {
            if peer != inner.self_id {
                inner.send_to(peer, from_port, to_port, bytes);
            }
        }
        true
    }

    fn receive(&self, port: u16, out: &mut Datagram) -> bool {
        self.inner().receive(port, out)
    }

    fn advertise(&self, connect: &[u8]) {
        let inner = self.inner();
        let mut state = inner.state();
        state.advert = connect.to_vec();
        inner.publish_locked(&state);
        inner.list_locked(&mut state);
    }

    fn next_join(&self) -> Option<Vec<u8>> {
        self.inner().state().joins.pop_front()
    }

    fn lobbies(&self) -> bool {
        true
    }

    fn set_joinable(&self, joinable: Option<&Session>) {
        let inner = self.inner();
        let mut state = inner.state();
        state.session = joinable.map_or(Vec::new(), |session| hex(session));
        inner.publish_locked(&state);
    }

    // The lobby carries its owner's game, so it goes with the party. Steam tells every
    // member, the new owner included (owner_changed).
    fn host_moved(&self, peer: u64) {
        let inner = self.inner();
        // A party Find Game made is not this lobby's.
        if !inner.in_lobby(peer) {
            return;
        }
        let state = inner.state();
        let lobby = inner.lobby.load(Relaxed);
        if !state.owner || lobby == 0 || peer == inner.self_id {
            return;
        }
        let moved = inner.client.set_owner(lobby, peer);
        log_info!(
            "online: lobby %016llx goes to %016llx%s",
            lobby,
            peer,
            if moved { c"" } else { c", which Steam refused" }
        );
    }

    fn matchmaking(&self) -> bool {
        true
    }

    fn list(&self, listing: Option<&SessionListing>) {
        let inner = self.inner();
        let mut state = inner.state();
        state.game = listing.map_or(Vec::new(), game_text);
        inner.publish_game_locked(&mut state);
    }

    fn search(&self, search: &SessionSearch) -> u32 {
        let inner = self.inner();
        let mut state = inner.state();
        // A search the title cancelled is never asked for again.
        let now = Instant::now();
        state.hunts.retain(|_, hunt| now.saturating_duration_since(hunt.created) < 2 * LIST_WITHIN);
        state.last_hunt = state.last_hunt.wrapping_add(1);
        let id = state.last_hunt;
        state.hunts.insert(
            id,
            Hunt {
                search: search.clone(),
                found: Vec::new(),
                created: now,
                probing: None,
                asked: false,
                answered: false,
                done: false,
            },
        );
        inner.start_hunt_locked(&mut state);
        id
    }

    fn search_results(&self, search: u32, out: &mut Vec<SessionFound>) -> bool {
        let mut state = self.inner().state();
        let Some(hunt) = state.hunts.get_mut(&search) else { return true };
        if !hunt.done {
            return false;
        }
        *out = std::mem::take(&mut hunt.found);
        state.hunts.remove(&search);
        true
    }

    fn title_file(&self, name: &[u8], out: &mut Vec<u8>) -> bool {
        // playlists.info on the disc, playlists.patch2 after the title update.
        if !name.starts_with(b"playlists.") {
            return false;
        }
        *out = PLAYLISTS.to_vec();
        true
    }

    fn in_game(&self, playing: bool) {
        let inner = self.inner();
        let mut state = inner.state();
        if playing == state.playing {
            return;
        }
        state.playing = playing;
        log_info!("online: %s", if playing { c"in a Find Game game" } else { c"no longer in a Find Game game" });
        if playing {
            return;
        }
        let State { found_hosts, contacted, strangers, .. } = &mut *state;
        for host in found_hosts.iter() {
            contacted.remove(host);
        }
        // A friend it played with is a friend again.
        strangers.retain(|&peer| !inner.in_lobby(peer));
        found_hosts.retain(|&peer| !inner.in_lobby(peer));
    }

    fn next_invite(&self) -> Option<Invite> {
        self.inner().state().invites.pop_front()
    }

    fn friends(&self, list: &mut FriendList) -> bool {
        let inner = self.inner();
        inner.friends_wanted.store(inner.epoch.elapsed().as_millis() as u64 + 1, Relaxed);
        let held = locked(&inner.friend_list);
        if list.version != held.version {
            *list = held.clone();
        }
        true
    }

    // To this player's lobby, which carries their party or game: Steam's own
    // invitation, shown in the friend's chat, which their copy hears of when they
    // accept.
    fn invite_friend(&self, id: u64) -> bool {
        let inner = self.inner();
        let lobby = inner.lobby.load(Relaxed);
        if lobby == 0 {
            log_warn!("online: no lobby yet to invite anyone to");
            return false;
        }
        let sent = inner.client.invite_to_lobby(lobby, id);
        log_info!("online: invites %016llx%s", id, if sent { c"" } else { c", which Steam refused" });
        sent
    }
}
