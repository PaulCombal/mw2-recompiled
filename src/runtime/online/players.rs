// The service over a transport: the players it reaches, with no account anywhere.
// lan and server are this, on the local network's UDP and on a relay server, so both
// have the same System Link list and the same Xbox LIVE parties and private matches.
//
// Who the player is, to the title, is the profile at the first controller (signin),
// plus the transport's slot, so a second copy on one machine is a second player.
// MW2_NAME is another name for him than the profile's.
//
// Xbox LIVE lobbies: every copy says who it is and what can be joined, once a second,
// to everyone the transport reaches. MW2_LAN_JOIN=<name> joins that player's party or
// lobby as soon as it is seen, as choosing a friend and "Join Session" does on the
// console. The lobby's "Invite friends" invites every player seen; a copy started
// with MW2_LAN_ACCEPT=1 accepts.

use super::{Datagram, Invite, SESSION_INFO_SIZE, Service, Session, SessionFound, SessionListing, SessionSearch};
use crate::env;
use crate::log::{c_string, log_error, log_info, log_warn};
use crate::runtime::{platform, signin};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How the players a service reaches exchange packets: the local network's UDP
/// (lan.rs) or a relay server everyone connects to (server.rs).
pub trait Transport: Send + Sync {
    /// Opens it, and logs why when it cannot.
    fn open(&mut self) -> bool;
    /// "lan", "server": for the log.
    fn name(&self) -> &'static CStr;
    /// Where the others reach this copy, stable for the run.
    fn local_id(&self) -> u64;
    /// Which copy on this machine this is: 0 for the first to start, so a second copy
    /// is a second player.
    fn slot(&self) -> u32;

    fn send_to(&self, peer: u64, bytes: &[u8]) -> bool;
    /// To everyone it reaches; it may come back to this copy too.
    fn send_all(&self, bytes: &[u8]) -> bool;
    /// The next packet that arrived and who from, without waiting: its size, or None
    /// when nothing is waiting.
    fn next(&self, bytes: &mut [u8]) -> Option<(usize, u64)>;

    // Optional, as on Service: what only a server everyone shares can do.
    fn matchmaking(&self) -> bool {
        false
    }
    fn list(&self, _listing: Option<&SessionListing>) {}
    fn search(&self, _search: &SessionSearch) -> u32 {
        0
    }
    fn search_results(&self, _search: u32, _out: &mut Vec<SessionFound>) -> bool {
        false
    }
    fn title_file(&self, _name: &[u8], _out: &mut Vec<u8>) -> bool {
        false
    }
}

// Magic, the sender's instance, its port and the port it is for. What travels is
// big-endian, the instance aside, which only its sender reads.
const FRAME: usize = 12;
const MAGIC: u32 = 0x4D57_324C; // "MW2L"

// Who a copy is and what can be joined: to everyone as presence, or to one player as
// an invitation. As the C++ struct had it, padding and all: magic, instance, kind,
// joinable, six bytes of nothing, the account, the name, NUL-padded, the session and
// four bytes of nothing.
const PRESENCE: usize = 120;
const NAME: usize = 32;
const ACCOUNT_AT: usize = 16;
const NAME_AT: usize = 24;
const SESSION_AT: usize = NAME_AT + NAME;
const PRESENCE_MAGIC: u32 = 0x4D57_3250; // "MW2P"
const ANNOUNCE: u8 = 0;
const INVITE: u8 = 1;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(1);

struct Presence {
    instance: [u8; 4],
    kind: u8,
    joinable: bool,
    account: u64,
    name: [u8; NAME],
    session: Session,
}

impl Presence {
    fn bytes(&self) -> [u8; PRESENCE] {
        let mut bytes = [0u8; PRESENCE];
        bytes[0..4].copy_from_slice(&PRESENCE_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.instance);
        bytes[8] = self.kind;
        bytes[9] = u8::from(self.joinable);
        bytes[ACCOUNT_AT..ACCOUNT_AT + 8].copy_from_slice(&self.account.to_be_bytes());
        bytes[NAME_AT..NAME_AT + NAME].copy_from_slice(&self.name);
        bytes[SESSION_AT..SESSION_AT + SESSION_INFO_SIZE].copy_from_slice(&self.session);
        bytes
    }

    fn read(bytes: &[u8; PRESENCE]) -> Self {
        Presence {
            instance: bytes[4..8].try_into().unwrap(),
            kind: bytes[8],
            joinable: bytes[9] != 0,
            account: u64::from_be_bytes(bytes[ACCOUNT_AT..ACCOUNT_AT + 8].try_into().unwrap()),
            name: bytes[NAME_AT..NAME_AT + NAME].try_into().unwrap(),
            session: bytes[SESSION_AT..SESSION_AT + SESSION_INFO_SIZE].try_into().unwrap(),
        }
    }
}

#[derive(Clone)]
struct Peer {
    id: u64,
    account: u64,
    joinable: bool,
    session: Session,
}

const LARGEST: usize = 64 * 1024;
const QUEUED: usize = 512;

struct Shared {
    /// told once that another machine has this identity
    warned: bool,
    waiting: BTreeMap<u16, VecDeque<Datagram>>,
    /// by name
    peers: BTreeMap<Vec<u8>, Peer>,
    joinable: Option<Session>,
    announced: Option<Instant>,
    /// MW2_LAN_JOIN, until joined
    join: Option<Vec<u8>>,
    invites: VecDeque<Invite>,
    packet: Vec<u8>,
}

struct Players {
    transport: Box<dyn Transport>,
    instance: [u8; 4],
    name: Vec<u8>,
    account: u64,
    /// MW2_LAN_ACCEPT
    accept: bool,
    shared: Mutex<Shared>,
}

pub fn make(transport: Box<dyn Transport>) -> Box<dyn Service> {
    Box::new(Players {
        transport,
        instance: [0; 4],
        name: Vec::new(),
        account: 0,
        accept: false,
        shared: Mutex::new(Shared {
            warned: false,
            waiting: BTreeMap::new(),
            peers: BTreeMap::new(),
            joinable: None,
            announced: None,
            join: None,
            invites: VecDeque::new(),
            packet: Vec::new(),
        }),
    })
}

/// The title's datagram behind a frame; None when it is too large.
fn framed(instance: [u8; 4], from_port: u16, to_port: u16, bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > LARGEST - FRAME {
        return None;
    }
    let mut packet = Vec::with_capacity(FRAME + bytes.len());
    packet.extend_from_slice(&MAGIC.to_be_bytes());
    packet.extend_from_slice(&instance);
    packet.extend_from_slice(&from_port.to_be_bytes());
    packet.extend_from_slice(&to_port.to_be_bytes());
    packet.extend_from_slice(bytes);
    Some(packet)
}

impl Players {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn presence(&self, shared: &Shared, kind: u8) -> Presence {
        let mut name = [0u8; NAME];
        // snprintf's: up to the first zero, and room for one.
        let text = c_string(self.name.clone());
        let length = text.as_bytes().len().min(NAME - 1);
        name[..length].copy_from_slice(&text.as_bytes()[..length]);
        Presence {
            instance: self.instance,
            kind,
            joinable: shared.joinable.is_some(),
            account: self.account(),
            name,
            session: shared.joinable.unwrap_or([0; SESSION_INFO_SIZE]),
        }
    }

    fn announce(&self, shared: &mut Shared) {
        shared.announced = Some(Instant::now());
        self.transport.send_all(&self.presence(shared, ANNOUNCE).bytes());
    }

    fn invite_from(peer: &Peer, from_invite: bool) -> Invite {
        Invite { inviter: peer.account, session: peer.session, from_invite }
    }

    fn heard(&self, shared: &mut Shared, from: u64, presence: &Presence) {
        if presence.instance == self.instance {
            return;
        }
        let length = presence.name.iter().position(|&byte| byte == 0).unwrap_or(NAME);
        let name = presence.name[..length].to_vec();
        let account = self.account();
        let peer = shared.peers.entry(name.clone()).or_insert(Peer {
            id: 0,
            account: 0,
            joinable: false,
            session: [0; SESSION_INFO_SIZE],
        });
        peer.id = from;
        peer.account = presence.account;
        let same = peer.account == account;
        peer.joinable = presence.joinable;
        peer.session = presence.session;
        let peer = peer.clone();
        if same && !shared.warned {
            shared.warned = true;
            log_error!(
                "online: %s on this %s is the same player as this one; on one of the two machines, take the `machine` line out of saves/profiles.txt",
                c_string(name.clone()).as_c_str(),
                if self.transport.name() == c"lan" { c"network" } else { c"server" }
            );
        }
        if presence.kind != INVITE || !peer.joinable {
            return;
        }
        if self.accept {
            log_info!("online: %s accepts %s's invitation", self.transport.name(), c_string(name).as_c_str());
            shared.invites.push_back(Self::invite_from(&peer, true));
        } else {
            log_info!("online: %s invites you (MW2_LAN_ACCEPT=1 accepts)", c_string(name).as_c_str());
        }
    }

    // Everything the transport holds, sorted by the port it is for. A broadcast can
    // come back to the copy that sent it; the instance tells it apart.
    fn drain(&self, shared: &mut Shared) {
        let mut packet = std::mem::take(&mut shared.packet);
        packet.resize(LARGEST, 0);
        while let Some((got, from)) = self.transport.next(&mut packet) {
            let got = &packet[..got];
            if got.len() == PRESENCE && got[..4] == PRESENCE_MAGIC.to_be_bytes() {
                self.heard(shared, from, &Presence::read(got.try_into().unwrap()));
                continue;
            }
            if got.len() < FRAME || got[..4] != MAGIC.to_be_bytes() || got[4..8] == self.instance {
                continue;
            }
            let datagram = Datagram {
                from,
                from_port: u16::from_be_bytes([got[8], got[9]]),
                to_port: u16::from_be_bytes([got[10], got[11]]),
                bytes: got[FRAME..].to_vec(),
            };
            // A port nobody reads fills up and drops, as a socket's buffer does.
            let queue = shared.waiting.entry(datagram.to_port).or_default();
            if queue.len() < QUEUED {
                queue.push_back(datagram);
            }
        }
        shared.packet = packet;
    }
}

impl Service for Players {
    fn start(&mut self) -> bool {
        if !self.transport.open() {
            return false;
        }
        self.instance = (platform::random() as u32).to_ne_bytes();
        let player = signin::first();
        self.account = player.id;
        self.name = env::text(c"MW2_NAME").map_or(player.name, |name| name.to_bytes().to_vec());
        self.shared().join = env::text(c"MW2_LAN_JOIN").map(|join| join.to_bytes().to_vec());
        self.accept = env::flag(c"MW2_LAN_ACCEPT");
        log_info!("online: %s as \"%s\"", self.transport.name(), c_string(self.name.clone()).as_c_str());
        true
    }

    fn local_id(&self) -> u64 {
        self.transport.local_id()
    }

    // The profile, and which copy on this machine this is.
    fn account(&self) -> u64 {
        self.account.wrapping_add(u64::from(self.transport.slot()))
    }

    fn local_name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn send(&self, peer: u64, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        framed(self.instance, from_port, to_port, bytes).is_some_and(|packet| self.transport.send_to(peer, &packet))
    }

    fn broadcast(&self, from_port: u16, to_port: u16, bytes: &[u8]) -> bool {
        framed(self.instance, from_port, to_port, bytes).is_some_and(|packet| self.transport.send_all(&packet))
    }

    fn receive(&self, port: u16, out: &mut Datagram) -> bool {
        let mut shared = self.shared();
        self.drain(&mut shared);
        match shared.waiting.get_mut(&port).and_then(VecDeque::pop_front) {
            Some(datagram) => {
                *out = datagram;
                true
            }
            None => false,
        }
    }

    fn lobbies(&self) -> bool {
        true
    }

    fn set_joinable(&self, joinable: Option<&Session>) {
        let mut shared = self.shared();
        shared.joinable = joinable.copied();
        self.announce(&mut shared);
    }

    fn next_invite(&self) -> Option<Invite> {
        let mut shared = self.shared();
        self.drain(&mut shared);
        if shared.announced.is_none_or(|announced| announced.elapsed() >= ANNOUNCE_EVERY) {
            self.announce(&mut shared);
        }
        if let Some(join) = shared.join.clone()
            && let Some(peer) = shared.peers.get(&join).filter(|peer| peer.joinable).cloned()
        {
            log_info!("online: %s joins %s", self.transport.name(), c_string(join).as_c_str());
            shared.invites.push_back(Self::invite_from(&peer, false));
            shared.join = None;
        }
        shared.invites.pop_front()
    }

    fn matchmaking(&self) -> bool {
        self.transport.matchmaking()
    }

    fn list(&self, listing: Option<&SessionListing>) {
        self.transport.list(listing)
    }

    fn search(&self, search: &SessionSearch) -> u32 {
        self.transport.search(search)
    }

    // Results arrive with everything else the transport holds.
    fn search_results(&self, search: u32, out: &mut Vec<SessionFound>) -> bool {
        {
            let mut shared = self.shared();
            self.drain(&mut shared);
        }
        self.transport.search_results(search, out)
    }

    fn title_file(&self, name: &[u8], out: &mut Vec<u8>) -> bool {
        self.transport.title_file(name, out)
    }

    fn invite_friends(&self) {
        let shared = self.shared();
        if shared.joinable.is_none() {
            log_warn!("online: nothing to invite anyone to yet");
            return;
        }
        let invite = self.presence(&shared, INVITE).bytes();
        for (name, peer) in &shared.peers {
            log_info!("online: %s invites %s", self.transport.name(), c_string(name.clone()).as_c_str());
            self.transport.send_to(peer.id, &invite);
        }
    }
}
