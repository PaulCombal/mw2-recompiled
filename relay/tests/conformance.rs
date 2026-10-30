//! The relay as the game sees it: the real program on a real socket, spoken to
//! as a copy would. The packet layouts are written out here on purpose, so that
//! a change to src/protocol.rs that the game would feel fails here.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAGIC: u32 = 0x4D573252;
const PROTOCOL: u16 = 2;
const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const REFUSED: u8 = 3;
const ALIVE: u8 = 4;
const TO: u8 = 5;
const ALL: u8 = 6;
const FROM: u8 = 7;
const UNKNOWN: u8 = 8;
const BYE: u8 = 9;
const CHALLENGE: u8 = 10;
const ADVERTISE: u8 = 11;
const SEARCH: u8 = 12;
const RESULTS: u8 = 13;
const PING: u8 = 14;
const PONG: u8 = 15;
const FILE_ASK: u8 = 16;
const FILE_CHUNK: u8 = 17;

const ROOM: u32 = 0x1000001A;
const WAIT_A_SEARCH: Duration = Duration::from_millis(950);

static NEXT_PORT: AtomicU16 = AtomicU16::new(0);

struct Relay {
    child: Child,
    port: u16,
    status: u16,
}

impl Relay {
    fn start(arguments: &[&str]) -> Relay {
        // Below the range clients' own ports come from: a port merely found
        // free can go to another test's client before the relay binds it.
        let base = 20000 + (std::process::id() % 240) as u16 * 50;
        let place = NEXT_PORT.fetch_add(2, Ordering::Relaxed);
        let (port, status) = (base + place, base + place + 1);
        let child = Command::new(env!("CARGO_BIN_EXE_mw2-relay"))
            .args(["--port", &port.to_string(), "--status-port", &status.to_string()])
            .args(arguments)
            .env_remove("MW2_RELAY_PASSWORD")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let relay = Relay { child, port, status };
        let end = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", status)).is_err() {
            assert!(Instant::now() < end, "the relay did not start");
            thread::sleep(Duration::from_millis(10));
        }
        relay
    }

    fn status(&self) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.status)).unwrap();
        let mut text = String::new();
        stream.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        stream.read_to_string(&mut text).unwrap();
        text
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone)]
struct Packet {
    kind: u8,
    peer: u64,
    token: u64,
    body: Vec<u8>,
}

fn header(kind: u8, peer: u64, token: u64) -> Vec<u8> {
    let mut bytes = MAGIC.to_be_bytes().to_vec();
    bytes.extend([kind, 0, 0, 0]);
    bytes.extend(peer.to_be_bytes());
    bytes.extend(token.to_be_bytes());
    bytes
}

fn hello_body(protocol: u16, password: &[u8]) -> Vec<u8> {
    let mut body = protocol.to_be_bytes().to_vec();
    body.extend([0, 0]);
    let mut field = [0; 64];
    field[..password.len()].copy_from_slice(password);
    body.extend(field);
    body
}

struct Client {
    socket: Arc<UdpSocket>,
    token: u64,
    id: u64,
    sequence: u32,
    search: u32,
    quiet: Arc<AtomicBool>,
}

impl Client {
    fn socket(relay: &Relay) -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(("127.0.0.1", relay.port)).unwrap();
        socket
    }

    fn try_join(relay: &Relay, password: &[u8], protocol: u16) -> Result<Client, String> {
        let socket = Arc::new(Client::socket(relay));
        let mut client = Client {
            socket,
            token: 0,
            id: 0,
            sequence: 0,
            search: 0,
            quiet: Arc::new(AtomicBool::new(false)),
        };
        for _ in 0..20 {
            client.say(HELLO, 0, &hello_body(protocol, password));
            match client.next(Duration::from_millis(300)) {
                Some(Packet { kind: CHALLENGE, token, .. }) => client.token = token,
                Some(Packet { kind: WELCOME, peer, .. }) => {
                    client.id = peer;
                    let (socket, quiet, token) = (client.socket.clone(), client.quiet.clone(), client.token);
                    thread::spawn(move || {
                        while !quiet.load(Ordering::Relaxed) {
                            let _ = socket.send(&header(ALIVE, 0, token));
                            thread::sleep(Duration::from_secs(1));
                        }
                    });
                    return Ok(client);
                }
                Some(Packet { kind: REFUSED, body, .. }) => return Err(String::from_utf8(body).unwrap()),
                _ => {}
            }
        }
        Err("no answer".into())
    }

    fn join(relay: &Relay) -> Client {
        Client::try_join(relay, b"", PROTOCOL).unwrap()
    }

    fn say(&self, kind: u8, peer: u64, body: &[u8]) {
        let mut packet = header(kind, peer, self.token);
        packet.extend(body);
        self.socket.send(&packet).unwrap();
    }

    fn next(&self, within: Duration) -> Option<Packet> {
        self.socket.set_read_timeout(Some(within.max(Duration::from_millis(1)))).unwrap();
        let mut bytes = [0; 70000];
        let size = self.socket.recv(&mut bytes).ok()?;
        assert!(size >= 24);
        assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().unwrap()), MAGIC);
        Some(Packet {
            kind: bytes[4],
            peer: u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
            token: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
            body: bytes[24..size].to_vec(),
        })
    }

    fn expect(&self, kind: u8, within: Duration) -> Option<Packet> {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(packet) = self.next(end - Instant::now())
                && packet.kind == kind
            {
                return Some(packet);
            }
        }
        None
    }

    fn advert(&mut self, session: u8, open: u32, numbers: &[(u32, u32)], rtt: u16) {
        self.sequence += 1;
        self.advert_numbered(self.sequence, session, open, numbers, rtt, 1);
    }

    fn advert_numbered(&self, sequence: u32, session: u8, open: u32, numbers: &[(u32, u32)], rtt: u16, is_open: u8) {
        let mut body = sequence.to_be_bytes().to_vec();
        body.extend(rtt.to_be_bytes());
        body.extend([is_open, numbers.len() as u8]);
        body.extend([session; 60]);
        for slots in [open, 0, 1, 0] {
            body.extend(slots.to_be_bytes());
        }
        for (id, value) in numbers {
            body.extend(id.to_be_bytes());
            body.extend(value.to_be_bytes());
        }
        self.say(ADVERTISE, 0, &body);
    }

    /// The listings found, as (host, first byte of the session, open, rtt);
    /// None without an answer.
    fn find(&mut self, room: u32, wants: &[(u32, u32, u8)], most: u16) -> Option<Vec<(u64, u8, u32, u16)>> {
        self.search += 1;
        let mut body = self.search.to_be_bytes().to_vec();
        body.extend(room.to_be_bytes());
        body.extend(most.to_be_bytes());
        body.extend((wants.len() as u16).to_be_bytes());
        for (id, value, at_least) in wants {
            body.extend(id.to_be_bytes());
            body.extend(value.to_be_bytes());
            body.extend([*at_least, 0, 0, 0]);
        }
        self.say(SEARCH, 0, &body);
        let end = Instant::now() + Duration::from_secs(1);
        let (mut found, mut total) = (std::collections::BTreeMap::new(), None);
        while Instant::now() < end && total.is_none_or(|total| found.len() < total) {
            let Some(packet) = self.next(end - Instant::now()) else { continue };
            if packet.kind != RESULTS {
                continue;
            }
            assert!(packet.body.len() + 24 <= 1200, "a page must fit a datagram nothing splits");
            let page = &packet.body;
            if u32::from_be_bytes(page[0..4].try_into().unwrap()) != self.search {
                continue;
            }
            total = Some(u16::from_be_bytes(page[4..6].try_into().unwrap()) as usize);
            let first = u16::from_be_bytes(page[6..8].try_into().unwrap()) as usize;
            let count = u16::from_be_bytes(page[8..10].try_into().unwrap()) as usize;
            assert_eq!(page.len(), 12 + count * 88);
            for i in 0..count {
                let one = &page[12 + i * 88..12 + (i + 1) * 88];
                found.insert(
                    first + i,
                    (
                        u64::from_be_bytes(one[0..8].try_into().unwrap()),
                        one[8],
                        u32::from_be_bytes(one[68..72].try_into().unwrap()),
                        u16::from_be_bytes(one[84..86].try_into().unwrap()),
                    ),
                );
            }
        }
        total.map(|_| found.into_values().collect())
    }

    fn leave(&self) {
        self.quiet.store(true, Ordering::Relaxed);
        self.say(BYE, 0, &[]);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.quiet.store(true, Ordering::Relaxed);
    }
}

fn pause() {
    thread::sleep(Duration::from_millis(100));
}

const NUMBERS: [(u32, u32); 6] =
    [(0x800B, 1), (0x800A, 1), (6, 1), (0x10000014, 136), (0x10000017, 1), (ROOM, 8)];

fn wants() -> Vec<(u32, u32, u8)> {
    NUMBERS.iter().map(|&(id, value)| if id == ROOM { (id, 2, 1) } else { (id, value, 0) }).collect()
}

#[test]
fn nothing_is_kept_for_a_hello_without_its_token() {
    let relay = Relay::start(&[]);
    let socket = Client::socket(&relay);
    let mut hello = header(HELLO, 0, 0);
    hello.extend(hello_body(PROTOCOL, b""));
    socket.send(&hello).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut answer = [0; 2000];
    let size = socket.recv(&mut answer).unwrap();
    assert_eq!(answer[4], CHALLENGE);
    assert!(size <= hello.len(), "nothing larger than what came from an address not checked");
    assert!(relay.status().contains("\"copies\":[]"));
}

#[test]
fn a_hello_of_the_wrong_size_gets_nothing() {
    let relay = Relay::start(&[]);
    let socket = Client::socket(&relay);
    let mut hello = header(HELLO, 0, 0);
    hello.extend([0; 10]);
    socket.send(&hello).unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    assert!(socket.recv(&mut [0; 2000]).is_err());
}

#[test]
fn refuses_with_a_reason() {
    let relay = Relay::start(&["--password", "secret", "--per-address", "2"]);
    assert_eq!(Client::try_join(&relay, b"", PROTOCOL).err().unwrap(), "a password is needed");
    assert_eq!(Client::try_join(&relay, b"nope", PROTOCOL).err().unwrap(), "wrong password");
    let other = Client::try_join(&relay, b"secret", 1).err().unwrap();
    assert!(other.contains("this relay speaks protocol 2, the game 1"), "{other}");
    let _a = Client::try_join(&relay, b"secret", PROTOCOL).unwrap();
    let _b = Client::try_join(&relay, b"secret", PROTOCOL).unwrap();
    assert_eq!(Client::try_join(&relay, b"secret", PROTOCOL).err().unwrap(), "too many players from this address");
}

#[test]
fn says_when_it_is_full() {
    let relay = Relay::start(&["--max", "1"]);
    let _a = Client::join(&relay);
    assert_eq!(Client::try_join(&relay, b"", PROTOCOL).err().unwrap(), "the relay is full");
}

#[test]
fn hands_packets_on_and_stamps_the_sender() {
    let relay = Relay::start(&[]);
    let (a, b, c) = (Client::join(&relay), Client::join(&relay), Client::join(&relay));
    assert_eq!(a.id >> 48, 0);
    a.say(TO, b.id, b"to b");
    let got = b.expect(FROM, Duration::from_secs(1)).unwrap();
    assert_eq!((got.peer, got.body.as_slice()), (a.id, &b"to b"[..]));
    a.say(ALL, 0, b"to all");
    for other in [&b, &c] {
        let got = other.expect(FROM, Duration::from_secs(1)).unwrap();
        assert_eq!((got.peer, got.body.as_slice()), (a.id, &b"to all"[..]));
    }
    assert!(a.expect(FROM, Duration::from_millis(200)).is_none(), "All is not for the sender");
    a.say(TO, 0x1234, b"to nobody");
}

#[test]
fn a_wrong_token_is_dropped() {
    let relay = Relay::start(&[]);
    let (mut a, b) = (Client::join(&relay), Client::join(&relay));
    let token = a.token;
    a.token ^= 1;
    a.say(TO, b.id, b"forged");
    a.say(BYE, 0, &[]);
    a.token = token;
    assert!(b.expect(FROM, Duration::from_millis(300)).is_none());
    a.say(TO, b.id, b"still here");
    assert!(b.expect(FROM, Duration::from_secs(1)).is_some());
}

#[test]
fn a_stranger_is_told_to_say_hello() {
    let relay = Relay::start(&[]);
    let socket = Client::socket(&relay);
    socket.send(&header(ALIVE, 0, 0)).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut answer = [0; 100];
    socket.recv(&mut answer).unwrap();
    assert_eq!(answer[4], UNKNOWN);
    socket.send(&header(BYE, 0, 0)).unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    assert!(socket.recv(&mut answer).is_err(), "a Bye from nobody gets nothing");
}

#[test]
fn a_copy_gets_its_id_back() {
    let relay = Relay::start(&[]);
    let a = Client::join(&relay);
    let again = Client { socket: Arc::new(Client::socket(&relay)), token: 0, id: 0, sequence: 0, search: 0, quiet: Arc::new(AtomicBool::new(false)) };
    a.leave();
    pause();
    let mut token = 0;
    for _ in 0..5 {
        let mut hello = header(HELLO, a.id, token);
        hello.extend(hello_body(PROTOCOL, b""));
        again.socket.send(&hello).unwrap();
        match again.next(Duration::from_millis(300)).unwrap() {
            Packet { kind: CHALLENGE, token: given, .. } => token = given,
            Packet { kind: WELCOME, peer, .. } => {
                assert_eq!(peer, a.id);
                return;
            }
            _ => {}
        }
    }
    panic!("no Welcome");
}

#[test]
fn ping_comes_back_with_its_number() {
    let relay = Relay::start(&[]);
    let a = Client::join(&relay);
    a.say(PING, 1234, &[]);
    assert_eq!(a.expect(PONG, Duration::from_secs(1)).unwrap().peer, 1234);
}

#[test]
fn finds_what_fits() {
    let relay = Relay::start(&[]);
    let (mut a, mut b) = (Client::join(&relay), Client::join(&relay));
    assert_eq!(b.find(1, &[], 50), Some(vec![]), "nothing listed still answers");
    thread::sleep(WAIT_A_SEARCH);
    a.advert(0xA1, 8, &NUMBERS, 30);
    pause();
    assert_eq!(b.find(2, &wants(), 50), Some(vec![(a.id, 0xA1, 8, 30)]));
    assert_eq!(a.find(2, &wants(), 50), Some(vec![]), "a copy never finds itself");
    thread::sleep(WAIT_A_SEARCH);
    for wanted in [(0x10000014, 137, 0), (6, 2, 0), (ROOM, 9, 1), (0x7777, 1, 0)] {
        assert_eq!(b.find(1, &[wanted], 50), Some(vec![]));
        thread::sleep(WAIT_A_SEARCH);
    }
    assert_eq!(b.find(9, &[], 50), Some(vec![]), "no open slots");
    assert!(relay.status().contains("\"refusedBy\":{\"0x00007777\":1}"), "{}", relay.status());
}

#[test]
fn one_search_a_second() {
    let relay = Relay::start(&[]);
    let mut a = Client::join(&relay);
    assert!(a.find(1, &[], 50).is_some());
    let started = Instant::now();
    a.search += 1;
    a.say(SEARCH, 0, &[&a.search.to_be_bytes()[..], &[0; 8]].concat());
    assert!(a.expect(RESULTS, Duration::from_millis(300)).is_none());
    thread::sleep(WAIT_A_SEARCH.saturating_sub(started.elapsed()));
    assert!(a.find(1, &[], 50).is_some());
}

#[test]
fn a_late_packet_undoes_nothing() {
    let relay = Relay::start(&[]);
    let (mut a, mut b) = (Client::join(&relay), Client::join(&relay));
    a.advert(0xA1, 8, &[], 1);
    a.advert_numbered(a.sequence - 1, 0xA2, 8, &[], 1, 1);
    pause();
    assert_eq!(b.find(0, &[], 50).unwrap()[0].1, 0xA1);
}

#[test]
fn a_listing_goes_with_a_withdrawal_or_its_copy() {
    let relay = Relay::start(&[]);
    let (mut a, mut b, mut c) = (Client::join(&relay), Client::join(&relay), Client::join(&relay));
    a.advert(0xA1, 8, &[], 70);
    c.advert(0xC1, 8, &[], 10);
    pause();
    let hosts = |found: Vec<(u64, u8, u32, u16)>| found.into_iter().map(|one| one.0).collect::<Vec<_>>();
    assert_eq!(hosts(b.find(0, &[], 50).unwrap()), [c.id, a.id], "nearest first");
    thread::sleep(WAIT_A_SEARCH);
    a.sequence += 1;
    a.advert_numbered(a.sequence, 0, 0, &[], 0, 0);
    pause();
    assert_eq!(hosts(b.find(0, &[], 50).unwrap()), [c.id]);
    thread::sleep(WAIT_A_SEARCH);
    c.leave();
    pause();
    assert_eq!(b.find(0, &[], 50), Some(vec![]));
}

#[test]
fn bad_listings_are_dropped() {
    let relay = Relay::start(&[]);
    let (mut a, mut b) = (Client::join(&relay), Client::join(&relay));
    a.advert(0xA1, 8, &[], 1);
    let too_many: Vec<(u32, u32)> = (0..33).map(|i| (i, i)).collect();
    a.advert(0xA2, 8, &too_many, 1);
    let mut cut = 99u32.to_be_bytes().to_vec();
    cut.extend([0, 0, 1, 2]);
    cut.extend([0xA3; 60]);
    a.say(ADVERTISE, 0, &cut);
    let mut short = 100u32.to_be_bytes().to_vec();
    short.extend([0; 10]);
    a.say(ADVERTISE, 0, &short);
    pause();
    assert_eq!(b.find(0, &[], 50).unwrap()[0].1, 0xA1);
}

#[test]
fn pages_a_long_answer_and_takes_no_more_than_asked() {
    let relay = Relay::start(&[]);
    let mut b = Client::join(&relay);
    let mut crowd: Vec<Client> = (0..14).map(|_| Client::join(&relay)).collect();
    for (i, copy) in crowd.iter_mut().enumerate() {
        copy.advert(i as u8 + 1, 8, &[], i as u16 + 1);
    }
    thread::sleep(Duration::from_millis(200));
    let found = b.find(0, &[], 50).unwrap();
    assert_eq!(found.iter().map(|one| one.3).collect::<Vec<_>>(), (1..=14).collect::<Vec<_>>());
    thread::sleep(WAIT_A_SEARCH);
    assert_eq!(b.find(0, &[], 3).unwrap().len(), 3);
}

#[test]
fn a_copy_gone_quiet_is_forgotten_with_its_listing() {
    let relay = Relay::start(&[]);
    let (mut a, mut b) = (Client::join(&relay), Client::join(&relay));
    a.advert(0xA1, 8, &[], 1);
    pause();
    a.quiet.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_secs(12));
    assert_eq!(b.find(0, &[], 50), Some(vec![]));
    a.say(PING, 1, &[]);
    assert!(a.expect(UNKNOWN, Duration::from_secs(1)).is_some(), "a forgotten copy is told so");
}

#[test]
fn gives_the_playlists_with_chunks_lost() {
    let file: Vec<u8> = (0..130000u32).map(|i| (i % 251) as u8).collect();
    let dir = std::env::temp_dir().join(format!("mw2-relay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("playlists");
    std::fs::write(&path, &file).unwrap();
    let relay = Relay::start(&["--playlists", path.to_str().unwrap()]);
    let a = Client::join(&relay);
    let (mut held, mut asked): (Vec<u8>, u32) = (Vec::new(), 0);
    while held.len() < file.len() {
        asked += 1;
        a.say(FILE_ASK, 0, &(held.len() as u32).to_be_bytes());
        let Some(chunk) = a.expect(FILE_CHUNK, Duration::from_millis(300)) else { continue };
        if asked % 3 == 0 {
            continue; // lost on the way
        }
        assert_eq!(u32::from_be_bytes(chunk.body[0..4].try_into().unwrap()) as usize, file.len());
        assert_eq!(u32::from_be_bytes(chunk.body[4..8].try_into().unwrap()) as usize, held.len());
        assert!(chunk.body.len() - 8 <= 1024);
        held.extend(&chunk.body[8..]);
    }
    assert!(held == file);
    a.say(FILE_ASK, 0, &(file.len() as u32 + 1).to_be_bytes());
    assert!(a.expect(FILE_CHUNK, Duration::from_millis(200)).is_none(), "past the end gets nothing");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn says_it_has_no_playlists() {
    let relay = Relay::start(&[]);
    let a = Client::join(&relay);
    a.say(FILE_ASK, 0, &0u32.to_be_bytes());
    let chunk = a.expect(FILE_CHUNK, Duration::from_secs(1)).unwrap();
    assert_eq!(chunk.body, [0; 8]);
}

#[test]
fn random_packets_stop_nothing() {
    let relay = Relay::start(&[]);
    let (attacker, a, mut b) = (Client::join(&relay), Client::join(&relay), Client::join(&relay));
    let stranger = Client::socket(&relay);
    let mut state = 0x2545F4914F6CDD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..30000u64 {
        let size = (next() % 300) as usize;
        let mut body: Vec<u8> = (0..size).map(|_| next() as u8).collect();
        if round % 2 == 0 {
            // Real headers of every kind, whose bodies are nonsense.
            let kind = 1 + (next() % 20) as u8;
            let mut packet = header(kind, next() % 4, attacker.token);
            packet.append(&mut body);
            attacker.socket.send(&packet).unwrap();
        } else {
            let mut packet = header((next() % 256) as u8, next(), next());
            packet.truncate((next() % 25) as usize);
            packet.append(&mut body);
            let _ = stranger.send(&packet);
        }
        if round % 2000 == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
    thread::sleep(Duration::from_millis(300));
    while b.next(Duration::from_millis(1)).is_some() {}
    a.say(TO, b.id, b"after");
    assert!(b.expect(FROM, Duration::from_secs(2)).is_some(), "the relay no longer hands packets on");
    thread::sleep(WAIT_A_SEARCH);
    assert!(b.find(0, &[], 50).is_some());
    assert!(relay.status().starts_with("HTTP/1.0 200 OK"));
}

#[test]
fn stops_on_sigterm_and_logs_to_a_file() {
    let dir = std::env::temp_dir().join(format!("mw2-relay-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("relay.log");
    let mut relay = Relay::start(&["--log-file", path.to_str().unwrap()]);
    let a = Client::join(&relay);
    a.leave();
    pause();
    Command::new("kill").arg(relay.child.id().to_string()).status().unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    while relay.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "still running");
        thread::sleep(Duration::from_millis(20));
    }
    let log = std::fs::read_to_string(&path).unwrap();
    assert!(log.contains("joined ("), "{log}");
    assert!(log.contains("left ("), "{log}");
    assert!(log.contains("stopped"), "{log}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn refuses_bad_options() {
    let run = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mw2-relay"))
            .args(arguments)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .code()
            .unwrap()
    };
    assert_eq!(run(&["--port"]), 2);
    assert_eq!(run(&["--port", "0"]), 2);
    assert_eq!(run(&["--port", "70000"]), 2);
    assert_eq!(run(&["--max", "0"]), 2);
    assert_eq!(run(&["--what", "1"]), 2);
    assert_eq!(run(&["--password", &"x".repeat(64)]), 1);
    assert_eq!(run(&["--playlists", "/nowhere/at/all"]), 1);
}

#[test]
fn the_status_page_is_json() {
    let relay = Relay::start(&[]);
    let mut a = Client::join(&relay);
    a.advert(0xA1, 8, &NUMBERS, 5);
    pause();
    let text = relay.status();
    let body = text.split("\r\n\r\n").nth(1).unwrap();
    let status: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(status["protocol"], 2);
    assert_eq!(status["copies"][0]["id"], format!("{:012x}", a.id));
    assert_eq!(status["sessions"][0]["openPublic"], 8);
    assert_eq!(status["sessions"][0]["attributes"]["0x1000001a"], 8);
    let _: SocketAddr = status["copies"][0]["address"].as_str().unwrap().parse().unwrap();
}
