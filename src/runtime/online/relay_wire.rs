// What the game and the relay say to each other, as relay/src/protocol.rs has it:
// every packet is a Header, then what its kind carries, every number big-endian at
// the place named here. The relay's `cargo test` reads this file and fails when a
// number, a size or a place here is not protocol.rs's. It uses nothing, so that the
// relay can build it as it is.
#![allow(dead_code)]

pub const MAGIC: u32 = 0x4D57_3252; // "MW2R"
pub const PROTOCOL: u16 = 2;
pub const DEFAULT_PORT: u16 = 3074;
pub const ALIVE_EVERY_SECONDS: u64 = 1;
pub const ADVERTISE_EVERY_SECONDS: u64 = 2;

// Kinds.
pub const HELLO: u8 = 1;
pub const WELCOME: u8 = 2;
pub const REFUSED: u8 = 3;
pub const ALIVE: u8 = 4;
pub const TO: u8 = 5;
pub const ALL: u8 = 6;
pub const FROM: u8 = 7;
pub const UNKNOWN: u8 = 8;
pub const BYE: u8 = 9;
pub const CHALLENGE: u8 = 10;
pub const ADVERTISE: u8 = 11;
pub const SEARCH: u8 = 12;
pub const RESULTS: u8 = 13;
pub const PING: u8 = 14;
pub const PONG: u8 = 15;
pub const FILE_ASK: u8 = 16;
pub const FILE_CHUNK: u8 = 17;

/// Header: magic, kind, peer, token.
pub const HEADER: usize = 24;
pub const HEADER_MAGIC: usize = 0;
pub const HEADER_KIND: usize = 4;
pub const HEADER_PEER: usize = 8;
pub const HEADER_TOKEN: usize = 16;

/// Hello: the protocol, the password NUL-padded.
pub const HELLO_SIZE: usize = 68;
pub const HELLO_PROTOCOL: usize = 0;
pub const HELLO_PASSWORD: usize = 4;
pub const PASSWORD: usize = 64;

/// Attribute: id, value.
pub const ATTRIBUTE: usize = 8;
pub const ATTRIBUTE_ID: usize = 0;
pub const ATTRIBUTE_VALUE: usize = 4;
pub const MOST_ATTRIBUTES: usize = 32;

pub const SESSION_SIZE: usize = 60;

/// Advert, its Attributes after it.
pub const ADVERT: usize = 84;
pub const ADVERT_SEQUENCE: usize = 0;
pub const ADVERT_RTT: usize = 4;
pub const ADVERT_OPEN: usize = 6;
pub const ADVERT_ATTRIBUTES: usize = 7;
pub const ADVERT_SESSION: usize = 8;
pub const ADVERT_OPEN_PUBLIC: usize = 68;
pub const ADVERT_OPEN_PRIVATE: usize = 72;
pub const ADVERT_FILLED_PUBLIC: usize = 76;
pub const ADVERT_FILLED_PRIVATE: usize = 80;

/// Want: id, value, at least.
pub const WANT: usize = 12;
pub const WANT_ID: usize = 0;
pub const WANT_VALUE: usize = 4;
pub const WANT_AT_LEAST: usize = 8;

/// Search, its Wants after it.
pub const SEARCH_SIZE: usize = 12;
pub const SEARCH_ID: usize = 0;
pub const SEARCH_ROOM: usize = 4;
pub const SEARCH_MOST: usize = 8;
pub const SEARCH_WANTS: usize = 10;

/// Found.
pub const FOUND: usize = 88;
pub const FOUND_HOST: usize = 0;
pub const FOUND_SESSION: usize = 8;
pub const FOUND_OPEN_PUBLIC: usize = 68;
pub const FOUND_OPEN_PRIVATE: usize = 72;
pub const FOUND_FILLED_PUBLIC: usize = 76;
pub const FOUND_FILLED_PRIVATE: usize = 80;
pub const FOUND_RTT: usize = 84;

/// ResultsPage, its Founds after it.
pub const RESULTS_PAGE: usize = 12;
pub const RESULTS_SEARCH: usize = 0;
pub const RESULTS_TOTAL: usize = 4;
pub const RESULTS_FIRST: usize = 6;
pub const RESULTS_COUNT: usize = 8;
pub const FOUND_PER_PAGE: usize = 12;
pub const MOST_RESULTS: usize = 50;

/// FileAsk: the offset.
pub const FILE_ASK_SIZE: usize = 4;
/// FileChunk: the total and the offset, the bytes after them.
pub const FILE_CHUNK_SIZE: usize = 8;
pub const FILE_CHUNK_TOTAL: usize = 0;
pub const FILE_CHUNK_OFFSET: usize = 4;
pub const LARGEST_FILE: usize = 128 * 1024;

/// The largest datagram either side sends.
pub const LARGEST: usize = 64 * 1024 - 64;
/// Ids are 48 bits.
pub const ID_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

pub fn header(kind: u8, peer: u64, token: u64) -> [u8; HEADER] {
    let mut header = [0u8; HEADER];
    put32(&mut header, HEADER_MAGIC, MAGIC);
    header[HEADER_KIND] = kind;
    header[HEADER_PEER..HEADER_PEER + 8].copy_from_slice(&peer.to_be_bytes());
    header[HEADER_TOKEN..HEADER_TOKEN + 8].copy_from_slice(&token.to_be_bytes());
    header
}

pub fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

pub fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

pub fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_be_bytes(value)
}

pub fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

pub fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// A packet's header, read: its kind, peer and token.
pub struct Header {
    pub kind: u8,
    pub peer: u64,
    pub token: u64,
}

impl Header {
    pub fn read(bytes: &[u8]) -> Header {
        Header { kind: bytes[HEADER_KIND], peer: u64_at(bytes, HEADER_PEER), token: u64_at(bytes, HEADER_TOKEN) }
    }
}
