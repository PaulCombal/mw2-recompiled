// Players on a relay server (relay/): everyone who connects to the same one, wherever
// they are. MW2_SERVER is its address, host or host:port (3074 when no port is
// given), and MW2_SERVER_PASSWORD the password it asks for, if any. The relay hands
// every packet on, so nobody's address reaches the others, and only the relay's owner
// opens a port: every copy connects out to it.
//
// The relay challenges a copy's first Hello with a token, and gives it its id when it
// says Hello again with it (relay/src/protocol.rs, relay_wire.rs); a copy that cannot get one in
// ANSWER_WITHIN does not start, and the title stays offline. A thread says Alive once
// a second, so a copy the title is not polling -- while a map loads -- stays known.
// Each copy has a port of its own, the first free one above MW2_LAN_PORT (3074), and
// how far above is its slot, as on lan.
//
// The relay also keeps what copies list for matchmaking (relay/src/directory.rs). A
// listing is said again every few seconds, so one lost on the way, or a relay that
// restarted, costs nothing, and a withdrawal is said a few times. The way to the relay
// is measured once in a while: every packet of a match goes through it, so the round
// trip between two players is the sum of their two ways. The playlists are fetched
// once, before the title can ask for them.

use super::players::{self, Transport};
use super::relay_wire as wire;
use super::{SESSION_INFO_SIZE, Service, SessionFound, SessionListing, SessionSearch, udp};
use crate::env;
use crate::log::{c_string, log_error, log_info, log_warn};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ANSWER_WITHIN: Duration = Duration::from_secs(5);
const HELLO_EVERY: Duration = Duration::from_millis(500);
const PING_EVERY: Duration = Duration::from_secs(2);
const ASK_AGAIN_AFTER: Duration = Duration::from_millis(250);
const SEARCH_AGAIN_AFTER: Duration = Duration::from_secs(1);
const SEARCH_WITHIN: Duration = Duration::from_secs(3);
const WITHDRAWALS: i32 = 3;

struct PendingSearch {
    packet: Vec<u8>,
    started: Instant,
    sent: Instant,
    /// None until the first page
    total: Option<usize>,
    /// by place in the answer
    found: BTreeMap<u32, SessionFound>,
}

struct Matching {
    /// the Advert and its Attributes; empty when nothing is listed
    listing: Vec<u8>,
    /// times left to say it was withdrawn
    withdrawals: i32,
    sequence: u32,
    /// when it was last said
    listed: Option<Instant>,
    search_id: u32,
    searches: BTreeMap<u32, PendingSearch>,
    /// the last round trips to the relay, in ms
    ways: [u16; 8],
    way_next: usize,
}

struct Link {
    where_: Vec<u8>,
    relay: SocketAddrV4,
    socket: UdpSocket,
    id: u64,
    /// when this copy last sent the relay anything, in ms since `epoch`
    said: AtomicU64,
    /// the relay's Challenge, on everything sent after it
    token: AtomicU64,
    epoch: Instant,
    password: Vec<u8>,
    matching: Mutex<Matching>,
    stopping: Mutex<bool>,
    stop: Condvar,
}

struct Server {
    link: Option<Arc<Link>>,
    slot: u32,
    /// set before the title can ask
    playlists: Vec<u8>,
    alive: Option<JoinHandle<()>>,
}

pub fn make() -> Box<dyn Service> {
    players::make(Box::new(Server { link: None, slot: 0, playlists: Vec::new(), alive: None }))
}

fn printable(bytes: &[u8]) -> std::ffi::CString {
    c_string(bytes.iter().map(|&c| if (b' '..=b'~').contains(&c) { c } else { b'?' }).collect::<Vec<_>>())
}

impl Link {
    fn where_(&self) -> std::ffi::CString {
        c_string(self.where_.clone())
    }

    fn matching(&self) -> MutexGuard<'_, Matching> {
        self.matching.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn milliseconds(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn say(&self, kind: u8, peer: u64, bytes: &[u8]) -> bool {
        if bytes.len() > wire::LARGEST - wire::HEADER {
            return false;
        }
        let mut packet = Vec::with_capacity(wire::HEADER + bytes.len());
        packet.extend_from_slice(&wire::header(kind, peer, self.token.load(Relaxed)));
        packet.extend_from_slice(bytes);
        self.said.store(self.milliseconds(), Relaxed);
        self.socket.send_to(&packet, self.relay).is_ok()
    }

    fn say_hello(&self, had: u64) {
        let mut hello = [0u8; wire::HELLO_SIZE];
        wire::put16(&mut hello, wire::HELLO_PROTOCOL, wire::PROTOCOL);
        hello[wire::HELLO_PASSWORD..wire::HELLO_PASSWORD + self.password.len()].copy_from_slice(&self.password);
        self.say(wire::HELLO, had, &hello);
    }

    /// The next packet from the relay, its header taken off: the size of what follows
    /// it, or None when nothing is waiting. Anything that is not the relay's is
    /// dropped.
    fn relay_packet(&self, packet: &mut [u8]) -> Option<(usize, wire::Header)> {
        thread_local! {
            static RAW: RefCell<Vec<u8>> = RefCell::new(vec![0u8; wire::LARGEST]);
        }
        RAW.with_borrow_mut(|raw| self.relay_packet_into(raw, packet))
    }

    fn relay_packet_into(&self, raw: &mut [u8], packet: &mut [u8]) -> Option<(usize, wire::Header)> {
        loop {
            let (got, from) = self.socket.recv_from(raw).ok()?;
            if from != SocketAddr::V4(self.relay) || got < wire::HEADER {
                continue;
            }
            if wire::u32_at(raw, wire::HEADER_MAGIC) != wire::MAGIC {
                continue;
            }
            let header = wire::Header::read(raw);
            let carried = got - wire::HEADER;
            packet[..carried].copy_from_slice(&raw[wire::HEADER..got]);
            return Some((carried, header));
        }
    }

    /// The shortest of the last few, which is the way itself and not what happened to
    /// be queueing on it.
    fn way_to_relay(matching: &Matching) -> u32 {
        matching.ways.iter().filter(|&&way| way != 0).min().map_or(0, |&way| u32::from(way))
    }

    fn heard_pong(&self, sent: u64) {
        let took = self.milliseconds().wrapping_sub(sent);
        if took > 60000 {
            return;
        }
        let mut matching = self.matching();
        let at = matching.way_next % matching.ways.len();
        matching.way_next += 1;
        matching.ways[at] = took.max(1) as u16;
    }

    fn heard_results(&self, packet: &[u8]) {
        if packet.len() < wire::RESULTS_PAGE {
            return;
        }
        let total = usize::from(wire::u16_at(packet, wire::RESULTS_TOTAL));
        let first = usize::from(wire::u16_at(packet, wire::RESULTS_FIRST));
        let count = usize::from(wire::u16_at(packet, wire::RESULTS_COUNT));
        if count > wire::FOUND_PER_PAGE
            || total > wire::MOST_RESULTS
            || first + count > total
            || packet.len() != wire::RESULTS_PAGE + count * wire::FOUND
        {
            return;
        }
        let mut matching = self.matching();
        let Some(search) = matching.searches.get_mut(&wire::u32_at(packet, wire::RESULTS_SEARCH)) else { return };
        search.total = Some(total);
        for i in 0..count {
            let wire = &packet[wire::RESULTS_PAGE + i * wire::FOUND..][..wire::FOUND];
            let found = SessionFound {
                host: wire::u64_at(wire, wire::FOUND_HOST) & wire::ID_MASK,
                session: wire[wire::FOUND_SESSION..wire::FOUND_SESSION + SESSION_INFO_SIZE].try_into().unwrap(),
                open_public: wire::u32_at(wire, wire::FOUND_OPEN_PUBLIC),
                open_private: wire::u32_at(wire, wire::FOUND_OPEN_PRIVATE),
                filled_public: wire::u32_at(wire, wire::FOUND_FILLED_PUBLIC),
                filled_private: wire::u32_at(wire, wire::FOUND_FILLED_PRIVATE),
                lag: u32::from(wire::u16_at(wire, wire::FOUND_RTT)),
            };
            search.found.insert((first + i) as u32, found);
        }
    }

    /// The listing as it stands, under a new number; or, after a withdrawal, that
    /// again.
    fn say_listing(&self, matching: &mut Matching) {
        matching.listed = Some(Instant::now());
        if matching.listing.is_empty() {
            if matching.withdrawals == 0 {
                return;
            }
            matching.withdrawals -= 1;
            let mut closed = [0u8; wire::ADVERT];
            matching.sequence = matching.sequence.wrapping_add(1);
            wire::put32(&mut closed, wire::ADVERT_SEQUENCE, matching.sequence);
            self.say(wire::ADVERTISE, 0, &closed);
            return;
        }
        matching.sequence = matching.sequence.wrapping_add(1);
        let (sequence, way) = (matching.sequence, Self::way_to_relay(matching));
        let mut listing = matching.listing.clone();
        wire::put32(&mut listing, wire::ADVERT_SEQUENCE, sequence);
        wire::put16(&mut listing, wire::ADVERT_RTT, way as u16);
        self.say(wire::ADVERTISE, 0, &listing);
    }

    // Alive whenever a second went by with nothing said, and what must be said again
    // whether or not the title is polling.
    fn keep_alive(&self) {
        let every = Duration::from_secs(wire::ALIVE_EVERY_SECONDS);
        let mut pinged: Option<Instant> = None;
        let mut stopping = self.stopping.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            stopping = self.stop.wait_timeout(stopping, every / 4).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            if *stopping {
                return;
            }
            let now = self.milliseconds();
            if now.saturating_sub(self.said.load(Relaxed)) >= every.as_millis() as u64 {
                self.say(wire::ALIVE, self.id, &[]);
            }
            if pinged.is_none_or(|pinged| pinged.elapsed() >= PING_EVERY) {
                pinged = Some(Instant::now());
                self.say(wire::PING, self.milliseconds(), &[]);
            }
            let mut matching = self.matching();
            if matching
                .listed
                .is_none_or(|listed| listed.elapsed() >= Duration::from_secs(wire::ADVERTISE_EVERY_SECONDS))
            {
                self.say_listing(&mut matching);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let Some(link) = &self.link else { return };
        if let Some(alive) = self.alive.take() {
            *link.stopping.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            link.stop.notify_all();
            let _ = alive.join();
        }
        if link.id != 0 {
            link.say(wire::BYE, link.id, &[]);
        }
    }
}

impl Server {
    fn link(&self) -> &Link {
        self.link.as_ref().unwrap()
    }

    // Every copy on a relay must have the same playlists: the title compares their
    // version when it joins a party. A relay without any, or one that stops answering,
    // leaves the title with an empty file, and Find Game with nothing to offer.
    fn fetch_playlists(link: &Link) -> Vec<u8> {
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut asked: Option<Instant> = None;
        let mut total = 0usize;
        let mut file = Vec::new();
        let mut packet = vec![0u8; wire::LARGEST];
        while Instant::now() < deadline {
            if asked.is_none_or(|asked| asked.elapsed() >= ASK_AGAIN_AFTER) {
                link.say(wire::FILE_ASK, 0, &(file.len() as u32).to_be_bytes());
                asked = Some(Instant::now());
            }
            let Some((got, header)) = link.relay_packet(&mut packet) else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            if header.kind != wire::FILE_CHUNK || got < wire::FILE_CHUNK_SIZE {
                continue;
            }
            total = wire::u32_at(&packet, wire::FILE_CHUNK_TOTAL) as usize;
            if total == 0 {
                log_warn!("online: server %s has no playlists", link.where_().as_c_str());
                return Vec::new();
            }
            if total > wire::LARGEST_FILE || wire::u32_at(&packet, wire::FILE_CHUNK_OFFSET) as usize != file.len() {
                continue;
            }
            file.extend_from_slice(&packet[wire::FILE_CHUNK_SIZE..got]);
            if file.len() > total {
                break;
            }
            if file.len() == total {
                log_info!("online: server %s gave its playlists, %llu bytes", link.where_().as_c_str(), total as u64);
                return file;
            }
            asked = None;
        }
        log_warn!(
            "online: server %s did not give its playlists (%llu of %llu bytes)",
            link.where_().as_c_str(),
            file.len() as u64,
            total as u64
        );
        Vec::new()
    }
}

impl Transport for Server {
    fn open(&mut self) -> bool {
        if !udp::startup() {
            log_error!("online: no Winsock");
            return false;
        }
        let Some(where_) = env::text(c"MW2_SERVER") else {
            log_error!("online: server chosen, but no server set (MW2_SERVER)");
            return false;
        };
        let password = env::text(c"MW2_SERVER_PASSWORD").map_or(Vec::new(), |password| password.to_bytes().to_vec());
        if password.len() >= wire::PASSWORD {
            log_error!("online: the server password is longer than %llu characters", (wire::PASSWORD - 1) as u64);
            return false;
        }
        // MW2_SERVER's host and port, the first IPv4 address of the host.
        let text = where_.to_bytes();
        let (host, port) = match text.iter().rposition(|&c| c == b':') {
            Some(colon) => (c_string(&text[..colon]), c_string(&text[colon + 1..])),
            None => (where_.to_owned(), c_string(wire::DEFAULT_PORT.to_string())),
        };
        let Some(relay) = udp::resolve(&host, &port) else {
            log_error!("online: server %s: no such address", where_);
            return false;
        };
        let Some((socket, slot)) = udp::open_own_socket(env::number(c"MW2_LAN_PORT", 3074) as u16) else {
            log_error!("online: server could not open a socket");
            return false;
        };
        self.slot = slot;
        let mut link = Link {
            where_: text.to_vec(),
            relay,
            socket,
            id: 0,
            said: AtomicU64::new(0),
            token: AtomicU64::new(0),
            epoch: Instant::now(),
            password,
            matching: Mutex::new(Matching {
                listing: Vec::new(),
                withdrawals: 0,
                sequence: 0,
                listed: None,
                search_id: 0,
                searches: BTreeMap::new(),
                ways: [0; 8],
                way_next: 0,
            }),
            stopping: Mutex::new(false),
            stop: Condvar::new(),
        };

        log_info!(
            "online: server %s (%s:%u), asking to join",
            where_,
            c_string(relay.ip().to_string()).as_c_str(),
            u32::from(relay.port())
        );
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut hello: Option<Instant> = None;
        let mut packet = vec![0u8; wire::LARGEST];
        while Instant::now() < deadline {
            if hello.is_none_or(|hello| hello.elapsed() >= HELLO_EVERY) {
                link.say_hello(0);
                hello = Some(Instant::now());
            }
            match link.relay_packet(&mut packet) {
                Some((_, header)) if header.kind == wire::CHALLENGE => {
                    link.token.store(header.token, Relaxed);
                    link.say_hello(0);
                    hello = Some(Instant::now());
                }
                Some((got, header)) if header.kind == wire::REFUSED => {
                    log_error!("online: server %s refused this copy: %s", where_, printable(&packet[..got]).as_c_str());
                    return false;
                }
                Some((_, header)) if header.kind == wire::WELCOME => {
                    link.id = header.peer & wire::ID_MASK;
                    break;
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        if link.id == 0 {
            log_error!("online: server %s did not answer within %llu seconds", where_, ANSWER_WITHIN.as_secs());
            return false;
        }
        log_info!("online: server %s welcomes this copy as %012llx", where_, link.id);
        self.playlists = Self::fetch_playlists(&link);
        let link = Arc::new(link);
        let alive = Arc::clone(&link);
        self.alive = Some(std::thread::spawn(move || alive.keep_alive()));
        self.link = Some(link);
        true
    }

    fn name(&self) -> &'static CStr {
        c"server"
    }

    fn local_id(&self) -> u64 {
        self.link.as_ref().map_or(0, |link| link.id)
    }

    fn slot(&self) -> u32 {
        self.slot
    }

    fn send_to(&self, peer: u64, bytes: &[u8]) -> bool {
        self.link().say(wire::TO, peer, bytes)
    }

    fn send_all(&self, bytes: &[u8]) -> bool {
        self.link().say(wire::ALL, 0, bytes)
    }

    fn matchmaking(&self) -> bool {
        true
    }

    fn list(&self, listing: Option<&SessionListing>) {
        let link = self.link();
        let mut matching = link.matching();
        match listing {
            Some(listing) => {
                let count = listing.numbers.len().min(wire::MOST_ATTRIBUTES);
                let mut bytes = vec![0u8; wire::ADVERT + count * wire::ATTRIBUTE];
                bytes[wire::ADVERT_OPEN] = 1;
                bytes[wire::ADVERT_ATTRIBUTES] = count as u8;
                bytes[wire::ADVERT_SESSION..wire::ADVERT_SESSION + SESSION_INFO_SIZE].copy_from_slice(&listing.session);
                wire::put32(&mut bytes, wire::ADVERT_OPEN_PUBLIC, listing.open_public);
                wire::put32(&mut bytes, wire::ADVERT_OPEN_PRIVATE, listing.open_private);
                wire::put32(&mut bytes, wire::ADVERT_FILLED_PUBLIC, listing.filled_public);
                wire::put32(&mut bytes, wire::ADVERT_FILLED_PRIVATE, listing.filled_private);
                for (i, number) in listing.numbers.iter().take(count).enumerate() {
                    wire::put32(&mut bytes, wire::ADVERT + i * wire::ATTRIBUTE + wire::ATTRIBUTE_ID, number.id);
                    wire::put32(&mut bytes, wire::ADVERT + i * wire::ATTRIBUTE + wire::ATTRIBUTE_VALUE, number.value);
                }
                if bytes == matching.listing {
                    return;
                }
                if matching.listing.is_empty() {
                    log_info!("online: listing a session for matchmaking");
                }
                matching.listing = bytes;
                matching.withdrawals = 0;
            }
            None => {
                if matching.listing.is_empty() {
                    return;
                }
                log_info!("online: no longer listing a session");
                matching.listing.clear();
                matching.withdrawals = WITHDRAWALS;
            }
        }
        link.say_listing(&mut matching);
    }

    fn search(&self, search: &SessionSearch) -> u32 {
        let link = self.link();
        let mut matching = link.matching();
        // A search the title cancelled is never asked for again.
        let now = Instant::now();
        matching.searches.retain(|_, pending| now - pending.started < 2 * SEARCH_WITHIN);

        matching.search_id = matching.search_id.wrapping_add(1);
        let id = matching.search_id;
        let count = search.wants.len().min(wire::MOST_ATTRIBUTES);
        let mut packet = vec![0u8; wire::SEARCH_SIZE + count * wire::WANT];
        wire::put32(&mut packet, wire::SEARCH_ID, id);
        wire::put32(&mut packet, wire::SEARCH_ROOM, search.room);
        wire::put16(&mut packet, wire::SEARCH_MOST, (search.most as usize).min(wire::MOST_RESULTS) as u16);
        wire::put16(&mut packet, wire::SEARCH_WANTS, count as u16);
        for (i, want) in search.wants.iter().take(count).enumerate() {
            let at = wire::SEARCH_SIZE + i * wire::WANT;
            wire::put32(&mut packet, at + wire::WANT_ID, want.id);
            wire::put32(&mut packet, at + wire::WANT_VALUE, want.value);
            packet[at + wire::WANT_AT_LEAST] = u8::from(want.at_least);
        }
        link.say(wire::SEARCH, 0, &packet);
        matching
            .searches
            .insert(id, PendingSearch { packet, started: now, sent: now, total: None, found: BTreeMap::new() });
        id
    }

    fn search_results(&self, search: u32, out: &mut Vec<SessionFound>) -> bool {
        let link = self.link();
        let mut matching = link.matching();
        let way = Link::way_to_relay(&matching);
        let Some(pending) = matching.searches.get_mut(&search) else { return true };
        let now = Instant::now();
        let complete = pending.total.is_some_and(|total| pending.found.len() == total);
        if !complete && now - pending.started < SEARCH_WITHIN {
            if now - pending.sent >= SEARCH_AGAIN_AFTER {
                pending.sent = now;
                link.say(wire::SEARCH, 0, &pending.packet);
            }
            return false;
        }
        if !complete {
            log_warn!("online: server %s did not finish answering a search", link.where_().as_c_str());
        }
        out.clear();
        out.extend(pending.found.values().map(|found| SessionFound { lag: found.lag.wrapping_add(way), ..*found }));
        matching.searches.remove(&search);
        true
    }

    fn title_file(&self, name: &[u8], out: &mut Vec<u8>) -> bool {
        // playlists.info on the disc, playlists.patch2 after the title update.
        if !name.starts_with(b"playlists.") || self.playlists.is_empty() {
            return false;
        }
        *out = self.playlists.clone();
        true
    }

    fn next(&self, bytes: &mut [u8]) -> Option<(usize, u64)> {
        thread_local! {
            static PACKET: RefCell<Vec<u8>> = RefCell::new(vec![0u8; wire::LARGEST]);
        }
        PACKET.with_borrow_mut(|packet| self.next_into(packet, bytes))
    }
}

impl Server {
    fn next_into(&self, packet: &mut [u8], bytes: &mut [u8]) -> Option<(usize, u64)> {
        let link = self.link();
        loop {
            let (got, header) = link.relay_packet(packet)?;
            match header.kind {
                wire::FROM => {
                    let carried = got.min(bytes.len());
                    bytes[..carried].copy_from_slice(&packet[..carried]);
                    return Some((carried, header.peer));
                }
                wire::RESULTS => link.heard_results(&packet[..got]),
                wire::PONG => link.heard_pong(header.peer),
                wire::UNKNOWN => {
                    // The relay restarted, or forgot this copy.
                    log_warn!("online: server %s no longer knows this copy; joining again", link.where_().as_c_str());
                    link.say_hello(link.id);
                }
                wire::CHALLENGE => {
                    link.token.store(header.token, Relaxed);
                    link.say_hello(link.id);
                }
                wire::WELCOME => {
                    if header.peer & wire::ID_MASK != link.id {
                        log_error!(
                            "online: server %s took this copy back under another id: the players it knew no longer reach it",
                            link.where_().as_c_str()
                        );
                    }
                }
                wire::REFUSED => {
                    log_error!(
                        "online: server %s refused this copy: %s",
                        link.where_().as_c_str(),
                        printable(&packet[..got]).as_c_str()
                    );
                }
                _ => {}
            }
        }
    }
}
