//! What the game (runtime/online/server.cpp) and the relay say to each other
//! over UDP. Every packet is a Header, then what its kind carries; every number
//! is big-endian.
//!
//! A copy says Hello, and the relay answers Challenge with a token made from
//! the copy's address and a secret of the relay's, keeping nothing. The copy
//! says Hello again with that token, the password and the protocol it speaks,
//! and only then does the relay take it in: Welcome with the copy's id, or
//! Refused with why. A packet with a forged sender address never gets that far,
//! since its Challenge goes to the forged address. Every packet the copy sends
//! after carries the token, and the relay drops one that does not, so nobody
//! can send as somebody else.
//!
//! The copy then sends To one other copy or to All of them, and the relay hands
//! each on as From, with the sender's id: the relay is the only one that knows
//! where anyone is. A copy says Alive once a second when it has sent nothing
//! else; the relay forgets a copy it has not heard from for
//! FORGET_AFTER_SECONDS, or that says Bye, and answers anything from a copy it
//! does not know with Unknown, which the copy answers with Hello again, asking
//! for the id it had. To an address it has not checked, the relay sends
//! nothing larger than what came from there.
//!
//! The relay also keeps the sessions its copies list for matchmaking: a copy
//! says Advertise, again every ADVERTISE_EVERY_SECONDS, and the relay forgets
//! the listing with the copy, or on a later Advertise that is not open. Search
//! asks for the listings that fit, and Results answers in pages that fit a
//! datagram the Internet will not split. To the relay a session is 60 bytes it
//! hands on and numbers it compares; it knows no more of what they mean. Ping
//! and Pong measure the way to the relay, and FileAsk and FileChunk fetch the
//! playlists every copy on a relay must share.
//!
//! The game reads this file as protocol.h. Its structs have no holes, and a
//! number in them is a Be16, Be32 or Be64, which cannot be read without
//! saying which order it is in.

use zerocopy::byteorder::{BigEndian, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub type Be16 = U16<BigEndian>;
pub type Be32 = U32<BigEndian>;
pub type Be64 = U64<BigEndian>;

pub const MAGIC: u32 = 0x4D573252; // "MW2R"
/// Raised whenever a packet's meaning changes: a copy and a relay of
/// different protocols refuse each other.
pub const PROTOCOL: u16 = 2;
pub const DEFAULT_PORT: u16 = 3074;
/// How often the game says Alive; the relay only forgets.
#[allow(dead_code)]
pub const ALIVE_EVERY_SECONDS: u32 = 1;
pub const FORGET_AFTER_SECONDS: u32 = 10;
/// How often the game says its listing again.
#[allow(dead_code)]
pub const ADVERTISE_EVERY_SECONDS: u32 = 2;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// copy: Hello follows; `peer` is the id it had, or 0; `token` the Challenge's, or 0
    Hello = 1,
    /// relay: `peer` is the copy's id
    Welcome = 2,
    /// relay: why, as text, follows
    Refused = 3,
    /// copy
    Alive = 4,
    /// copy: `peer` is who for; the bytes follow
    To = 5,
    /// copy: the bytes follow
    All = 6,
    /// relay: `peer` is who from; the bytes follow
    From = 7,
    /// relay: say Hello again
    Unknown = 8,
    /// copy: it leaves
    Bye = 9,
    /// relay: `token` is what every packet of this copy carries from now on
    Challenge = 10,
    /// copy: an Advert, and its Attributes
    Advertise = 11,
    /// copy: a Search, and its Wants
    Search = 12,
    /// relay: a ResultsPage, and its Found
    Results = 13,
    /// copy: `peer` is anything
    Ping = 14,
    /// relay: `peer` is the Ping's
    Pong = 15,
    /// copy: a FileAsk
    FileAsk = 16,
    /// relay: a FileChunk, and the bytes
    FileChunk = 17,
}

impl Kind {
    /// What a packet says it is; a kind this relay does not know is None.
    pub fn from_wire(value: u8) -> Option<Kind> {
        use Kind::*;
        Some(match value {
            1 => Hello,
            2 => Welcome,
            3 => Refused,
            4 => Alive,
            5 => To,
            6 => All,
            7 => From,
            8 => Unknown,
            9 => Bye,
            10 => Challenge,
            11 => Advertise,
            12 => Search,
            13 => Results,
            14 => Ping,
            15 => Pong,
            16 => FileAsk,
            17 => FileChunk,
            _ => return None,
        })
    }
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Header {
    pub magic: Be32,
    pub kind: u8,
    pub pad: [u8; 3],
    pub peer: Be64,
    /// the copy's, on everything it sends after the Challenge
    pub token: Be64,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Hello {
    pub protocol: Be16,
    pub pad: [u8; 2],
    /// NUL-padded
    pub password: [u8; 64],
}

/// A number a session is known by, which a Search compares.
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Attribute {
    pub id: Be32,
    pub value: Be32,
}

pub const SESSION_SIZE: usize = 60;
pub const MOST_ATTRIBUTES: usize = 32;

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Advert {
    /// higher on each one, so a late packet undoes nothing
    pub sequence: Be32,
    /// the copy's way to the relay, in ms
    pub rtt: Be16,
    /// 0: the listing is withdrawn, and the rest means nothing
    pub open: u8,
    pub attributes: u8,
    pub session: [u8; SESSION_SIZE],
    pub open_public: Be32,
    pub open_private: Be32,
    pub filled_public: Be32,
    pub filled_private: Be32,
}

/// What a listing must have: an attribute of this id, equal to `value`, or
/// with at_least, no less than it.
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Want {
    pub id: Be32,
    pub value: Be32,
    pub at_least: u8,
    pub pad: [u8; 3],
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Search {
    /// the asker's number for it, in its ResultsPages
    pub search: Be32,
    /// the open public slots a listing must have
    pub room: Be32,
    /// how many listings it can take
    pub most: Be16,
    pub wants: Be16,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct Found {
    /// the id of the copy that listed it, stamped by the relay
    pub host: Be64,
    pub session: [u8; SESSION_SIZE],
    pub open_public: Be32,
    pub open_private: Be32,
    pub filled_public: Be32,
    pub filled_private: Be32,
    /// the host's way to the relay
    pub rtt: Be16,
    pub pad: [u8; 2],
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct ResultsPage {
    pub search: Be32,
    /// listings in the whole answer
    pub total: Be16,
    /// the first of this page's, in it
    pub first: Be16,
    /// on this page
    pub count: Be16,
    pub pad: [u8; 2],
}

pub const FOUND_PER_PAGE: usize = 12;
/// what the title asks for
pub const MOST_RESULTS: usize = 50;

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct FileAsk {
    pub offset: Be32,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
pub struct FileChunk {
    /// the file's size; 0 when the relay has none
    pub total: Be32,
    pub offset: Be32,
}

pub const CHUNK_BYTES: usize = 1024;
/// what the title has room for
pub const LARGEST_FILE: usize = 128 * 1024;

/// The largest datagram either side sends.
pub const LARGEST: usize = 64 * 1024 - 64;

/// Ids are 48 bits: the game makes the Ethernet address the title tells
/// machines apart by from the low 48 bits of a player's id.
pub const ID_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const _: () = {
    use core::mem::size_of;
    assert!(size_of::<Header>() == 24);
    assert!(size_of::<Hello>() == 68);
    assert!(size_of::<Advert>() == 84);
    assert!(size_of::<Want>() == 12);
    assert!(size_of::<Search>() == 12);
    assert!(size_of::<Found>() == 88);
    assert!(size_of::<ResultsPage>() == 12);
};
