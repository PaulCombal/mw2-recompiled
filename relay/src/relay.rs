//! Who is connected, and what each packet does.

use crate::directory::{Directory, Listing, Tally, Wanted};
use crate::log::log;
use crate::protocol::*;
use serde_json::{Map, Value, json};
use siphasher::sip::SipHasher24;
use std::collections::{BTreeMap, VecDeque};
use std::hash::Hasher;
use std::net::{SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zerocopy::{FromBytes, IntoBytes};

/// A search walks every listing: once a second is more than the title asks for.
const SEARCH_EVERY: Duration = Duration::from_millis(900);
const ASKED_KEPT: usize = 32;

struct GameCopy {
    address: SocketAddrV4,
    id: u64,
    token: u64,
    heard: Instant,
    searched: Option<Instant>,
}

/// One search, as the status page shows it.
struct Asked {
    when: u64,
    asker: u64,
    room: u32,
    wants: Vec<Wanted>,
    tally: Tally,
    found: usize,
}

pub struct Limits {
    pub password: String,
    pub most: usize,
    pub per_address: usize,
}

pub struct Relay {
    socket: UdpSocket,
    limits: Limits,
    keys: (u64, u64), // make the tokens; new at every start
    copies: BTreeMap<SocketAddrV4, GameCopy>, // nothing else is kept
    ids: BTreeMap<u64, SocketAddrV4>,
    playlists: Vec<u8>,
    directory: Directory,
    asked: VecDeque<Asked>, // the newest searches, for the status page
    packet: Vec<u8>,
}

/// A number from the system's own source of randomness. Failing is failing to
/// start: tokens from a guessable key protect nothing.
fn random() -> u64 {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).expect("the system gives no random numbers");
    u64::from_ne_bytes(bytes)
}

fn id_text(id: u64) -> String {
    format!("{id:012x}")
}

fn hexadecimal(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn number(id: u32) -> String {
    format!("0x{id:08x}")
}

fn seconds_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

impl Relay {
    pub fn new(socket: UdpSocket, limits: Limits, playlists: Vec<u8>) -> Relay {
        Relay {
            socket,
            limits,
            keys: (random(), random()),
            copies: BTreeMap::new(),
            ids: BTreeMap::new(),
            playlists,
            directory: Directory::default(),
            asked: VecDeque::new(),
            packet: Vec::new(),
        }
    }

    pub fn heard(&mut self, packet: &[u8], from: SocketAddrV4) {
        let Ok((header, body)) = Header::read_from_prefix(packet) else { return };
        if header.magic.get() != MAGIC {
            return;
        }
        let kind = Kind::from_wire(header.kind);
        let token = header.token.get();
        if kind == Some(Kind::Hello) {
            self.hello(from, header.peer.get() & ID_MASK, token, body);
            return;
        }
        let Some(copy) = self.copies.get_mut(&from) else {
            // A copy this relay forgot, or never knew: it says Hello again.
            if kind != Some(Kind::Bye) {
                self.say(from, Kind::Unknown, 0, &[], 0);
            }
            return;
        };
        // Somebody else, sending as this copy's address.
        if token != copy.token {
            return;
        }
        copy.heard = Instant::now();
        let id = copy.id;
        match kind {
            Some(Kind::To) => {
                if let Some(&to) = self.ids.get(&header.peer.get()) {
                    self.say(to, Kind::From, id, body, 0);
                }
            }
            Some(Kind::All) => {
                let others: Vec<SocketAddrV4> =
                    self.copies.values().filter(|other| other.id != id).map(|other| other.address).collect();
                for to in others {
                    self.say(to, Kind::From, id, body, 0);
                }
            }
            Some(Kind::Advertise) => self.advertise(id, body),
            Some(Kind::Search) => self.search(from, id, body),
            Some(Kind::Ping) => self.say(from, Kind::Pong, header.peer.get(), &[], 0),
            Some(Kind::FileAsk) => self.send_chunk(from, body),
            Some(Kind::Bye) => {
                log!("{} left ({from}), {} here", id_text(id), self.copies.len() - 1);
                self.forget(from);
            }
            _ => {}
        }
    }

    /// Every copy not heard from for FORGET_AFTER_SECONDS: its game ended
    /// without a Bye, or its connection did.
    pub fn sweep(&mut self) {
        let now = Instant::now();
        let after = Duration::from_secs(FORGET_AFTER_SECONDS.into());
        let quiet: Vec<SocketAddrV4> = self
            .copies
            .values()
            .filter(|copy| now.duration_since(copy.heard) >= after)
            .map(|copy| copy.address)
            .collect();
        for address in quiet {
            if let Some(copy) = self.copies.get(&address) {
                log!("{} went quiet ({address}), {} here", id_text(copy.id), self.copies.len() - 1);
            }
            self.forget(address);
        }
    }

    /// What it holds right now, as JSON.
    pub fn status(&self) -> String {
        let copies: Vec<Value> = self
            .copies
            .values()
            .map(|copy| json!({ "id": id_text(copy.id), "address": copy.address.to_string() }))
            .collect();
        let sessions: Vec<Value> = self
            .directory
            .listings()
            .iter()
            .map(|(host, listing)| {
                let attributes: Map<String, Value> =
                    listing.attributes.iter().map(|&(id, value)| (number(id), value.into())).collect();
                json!({
                    "host": id_text(*host),
                    "session": hexadecimal(&listing.session[..8]),
                    "rtt": listing.rtt,
                    "openPublic": listing.open_public,
                    "openPrivate": listing.open_private,
                    "filledPublic": listing.filled_public,
                    "filledPrivate": listing.filled_private,
                    "attributes": attributes,
                })
            })
            .collect();
        let searches: Vec<Value> = self
            .asked
            .iter()
            .map(|asked| {
                let wants: Map<String, Value> = asked
                    .wants
                    .iter()
                    .map(|want| {
                        let compare = if want.at_least { ">=" } else { "==" };
                        (number(want.id), format!("{compare}{}", want.value).into())
                    })
                    .collect();
                let refused: Map<String, Value> =
                    asked.tally.refused_by.iter().map(|(&id, &count)| (number(id), count.into())).collect();
                json!({
                    "when": asked.when,
                    "asker": id_text(asked.asker),
                    "room": asked.room,
                    "wants": wants,
                    "listed": asked.tally.listed,
                    "full": asked.tally.full,
                    "refusedBy": refused,
                    "found": asked.found,
                })
            })
            .collect();
        let status = json!({
            "protocol": PROTOCOL,
            "playlistBytes": self.playlists.len(),
            "copies": copies,
            "sessions": sessions,
            "searches": searches,
        });
        format!("{status}\n")
    }

    fn token_of(&self, address: SocketAddrV4) -> u64 {
        let word = (u64::from(u32::from(*address.ip())) << 16) | u64::from(address.port());
        let mut hasher = SipHasher24::new_with_keys(self.keys.0, self.keys.1);
        hasher.write_u64(word);
        match hasher.finish() {
            0 => 1, // 0 is no token
            token => token,
        }
    }

    fn hello(&mut self, from: SocketAddrV4, had: u64, token: u64, body: &[u8]) {
        // Nothing is kept and nothing larger than the Hello goes back until
        // the address has shown it receives what is sent to it.
        let Ok(hello) = Hello::read_from_bytes(body) else { return };
        let expected = self.token_of(from);
        if token != expected {
            self.say(from, Kind::Challenge, 0, &[], expected);
            return;
        }
        let protocol = hello.protocol.get();
        if protocol != PROTOCOL {
            self.refuse(
                from,
                &format!("this relay speaks protocol {PROTOCOL}, the game {protocol}: they come from different releases"),
            );
            return;
        }
        let end = hello.password.iter().position(|&byte| byte == 0).unwrap_or(hello.password.len());
        let password = &hello.password[..end];
        if password != self.limits.password.as_bytes() {
            let why = if password.is_empty() { "a password is needed" } else { "wrong password" };
            self.refuse(from, why);
            return;
        }
        if let Some(known) = self.copies.get_mut(&from) {
            // Its Welcome was lost; the same answer again.
            known.heard = Instant::now();
            let id = known.id;
            self.say(from, Kind::Welcome, id, &[], 0);
            return;
        }
        if self.copies.len() >= self.limits.most {
            self.refuse(from, "the relay is full");
            return;
        }
        let from_there = self.copies.values().filter(|copy| copy.address.ip() == from.ip()).count();
        if from_there >= self.limits.per_address {
            self.refuse(from, "too many players from this address");
            return;
        }
        // The id it had, if nobody has it now; a new one otherwise.
        let mut id = if self.ids.contains_key(&had) { 0 } else { had };
        while id == 0 || self.ids.contains_key(&id) {
            id = random() & ID_MASK;
        }
        self.copies.insert(
            from,
            GameCopy { address: from, id, token: expected, heard: Instant::now(), searched: None },
        );
        self.ids.insert(id, from);
        log!("{} joined ({from}), {} here", id_text(id), self.copies.len());
        self.say(from, Kind::Welcome, id, &[], 0);
    }

    fn refuse(&mut self, to: SocketAddrV4, why: &str) {
        log!("refused {to}: {why}");
        self.say(to, Kind::Refused, 0, why.as_bytes(), 0);
    }

    fn forget(&mut self, address: SocketAddrV4) {
        if let Some(copy) = self.copies.remove(&address) {
            self.directory.forget(copy.id);
            self.ids.remove(&copy.id);
        }
    }

    fn advertise(&mut self, id: u64, body: &[u8]) {
        let Ok((advert, rest)) = Advert::read_from_prefix(body) else { return };
        let sequence = advert.sequence.get();
        if advert.open == 0 {
            if self.directory.withdraw(id, sequence) {
                log!("{} withdrew its session", id_text(id));
            }
            return;
        }
        let count = usize::from(advert.attributes);
        if count > MOST_ATTRIBUTES {
            return;
        }
        let Ok(attributes) = <[Attribute]>::ref_from_bytes_with_elems(rest, count) else { return };
        let listing = Listing {
            rtt: advert.rtt.get(),
            session: advert.session,
            open_public: advert.open_public.get(),
            open_private: advert.open_private.get(),
            filled_public: advert.filled_public.get(),
            filled_private: advert.filled_private.get(),
            attributes: attributes.iter().map(|attribute| (attribute.id.get(), attribute.value.get())).collect(),
        };
        let known = self.directory.holds(id);
        let open = listing.open_public;
        if self.directory.put(id, sequence, listing) && !known {
            log!("{} listed session {}, {open} public slots open", id_text(id), hexadecimal(&advert.session[..8]));
        }
    }

    fn search(&mut self, from: SocketAddrV4, id: u64, body: &[u8]) {
        let Ok((search, rest)) = Search::read_from_prefix(body) else { return };
        let count = usize::from(search.wants.get());
        if count > MOST_ATTRIBUTES {
            return;
        }
        let Ok(wants) = <[Want]>::ref_from_bytes_with_elems(rest, count) else { return };
        let now = Instant::now();
        let Some(copy) = self.copies.get_mut(&from) else { return };
        if copy.searched.is_some_and(|searched| now.duration_since(searched) < SEARCH_EVERY) {
            return;
        }
        copy.searched = Some(now);

        let wants: Vec<Wanted> = wants
            .iter()
            .map(|want| Wanted { id: want.id.get(), value: want.value.get(), at_least: want.at_least != 0 })
            .collect();
        let room = search.room.get();
        let most = usize::from(search.most.get()).min(MOST_RESULTS);
        let mut tally = Tally::default();
        let found: Vec<(u64, Listing)> = self
            .directory
            .search(id, room, &wants, most, &mut tally)
            .into_iter()
            .map(|(host, listing)| (host, listing.clone()))
            .collect();

        let refused: String =
            tally.refused_by.iter().map(|(&key, count)| format!(" {}x{count}", number(key))).collect();
        log!(
            "{} searched: {} listed, {} full, {} found{}{refused}",
            id_text(id),
            tally.listed,
            tally.full,
            found.len(),
            if refused.is_empty() { "" } else { ", refused by" }
        );
        self.asked.push_back(Asked { when: seconds_now(), asker: id, room, wants, tally, found: found.len() });
        if self.asked.len() > ASKED_KEPT {
            self.asked.pop_front();
        }

        // Nothing fits still answers, or the asker would wait out its timeout
        // for each failed search.
        let pages: Vec<&[(u64, Listing)]> =
            if found.is_empty() { vec![&[]] } else { found.chunks(FOUND_PER_PAGE).collect() };
        let mut first = 0;
        for page in pages {
            let header = ResultsPage {
                search: search.search,
                total: Be16::new(found.len() as u16),
                first: Be16::new(first as u16),
                count: Be16::new(page.len() as u16),
                pad: [0; 2],
            };
            let mut bytes = header.as_bytes().to_vec();
            for (host, listing) in page {
                let one = Found {
                    host: Be64::new(*host),
                    session: listing.session,
                    open_public: Be32::new(listing.open_public),
                    open_private: Be32::new(listing.open_private),
                    filled_public: Be32::new(listing.filled_public),
                    filled_private: Be32::new(listing.filled_private),
                    rtt: Be16::new(listing.rtt),
                    pad: [0; 2],
                };
                bytes.extend_from_slice(one.as_bytes());
            }
            self.say(from, Kind::Results, 0, &bytes, 0);
            first += page.len();
        }
    }

    fn send_chunk(&mut self, to: SocketAddrV4, body: &[u8]) {
        let Ok(ask) = FileAsk::read_from_bytes(body) else { return };
        let offset = ask.offset.get() as usize;
        let Some(left) = self.playlists.get(offset..) else { return };
        let bytes = &left[..left.len().min(CHUNK_BYTES)];
        let header = FileChunk { total: Be32::new(self.playlists.len() as u32), offset: ask.offset };
        let mut chunk = header.as_bytes().to_vec();
        chunk.extend_from_slice(bytes);
        self.say(to, Kind::FileChunk, 0, &chunk, 0);
    }

    fn say(&mut self, to: SocketAddrV4, kind: Kind, peer: u64, bytes: &[u8], token: u64) {
        if bytes.len() > LARGEST - size_of::<Header>() {
            return;
        }
        let header = Header {
            magic: Be32::new(MAGIC),
            kind: kind as u8,
            pad: [0; 3],
            peer: Be64::new(peer),
            token: Be64::new(token),
        };
        self.packet.clear();
        self.packet.extend_from_slice(header.as_bytes());
        self.packet.extend_from_slice(bytes);
        // A datagram that cannot go is one lost on the way.
        let _ = self.socket.send_to(&self.packet, to);
    }
}
