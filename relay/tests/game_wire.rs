//! The game's side of the protocol, src/runtime/online/relay_wire.rs in the game,
//! must be what src/protocol.rs says: every number, kind, size and place.

#[allow(dead_code)]
#[path = "../src/protocol.rs"]
mod protocol;

#[path = "../../src/runtime/online/relay_wire.rs"]
mod game;

use core::mem::{offset_of, size_of};
use protocol::*;

#[test]
fn numbers() {
    assert_eq!(game::MAGIC, MAGIC);
    assert_eq!(game::PROTOCOL, PROTOCOL);
    assert_eq!(game::DEFAULT_PORT, DEFAULT_PORT);
    assert_eq!(game::ALIVE_EVERY_SECONDS, u64::from(ALIVE_EVERY_SECONDS));
    assert_eq!(game::ADVERTISE_EVERY_SECONDS, u64::from(ADVERTISE_EVERY_SECONDS));
    assert_eq!(game::SESSION_SIZE, SESSION_SIZE);
    assert_eq!(game::MOST_ATTRIBUTES, MOST_ATTRIBUTES);
    assert_eq!(game::FOUND_PER_PAGE, FOUND_PER_PAGE);
    assert_eq!(game::MOST_RESULTS, MOST_RESULTS);
    assert_eq!(game::LARGEST_FILE, LARGEST_FILE);
    assert_eq!(game::LARGEST, LARGEST);
    assert_eq!(game::ID_MASK, ID_MASK);
}

#[test]
fn kinds() {
    let kinds = [
        (game::HELLO, Kind::Hello),
        (game::WELCOME, Kind::Welcome),
        (game::REFUSED, Kind::Refused),
        (game::ALIVE, Kind::Alive),
        (game::TO, Kind::To),
        (game::ALL, Kind::All),
        (game::FROM, Kind::From),
        (game::UNKNOWN, Kind::Unknown),
        (game::BYE, Kind::Bye),
        (game::CHALLENGE, Kind::Challenge),
        (game::ADVERTISE, Kind::Advertise),
        (game::SEARCH, Kind::Search),
        (game::RESULTS, Kind::Results),
        (game::PING, Kind::Ping),
        (game::PONG, Kind::Pong),
        (game::FILE_ASK, Kind::FileAsk),
        (game::FILE_CHUNK, Kind::FileChunk),
    ];
    for (number, kind) in kinds {
        assert_eq!(number, kind as u8, "{kind:?}");
        assert_eq!(Kind::from_wire(number), Some(kind));
    }
    // Every kind the relay knows is one the game names.
    let known = (0..=u8::MAX).filter(|&number| Kind::from_wire(number).is_some()).count();
    assert_eq!(known, kinds.len());
}

#[test]
fn places() {
    assert_eq!(game::HEADER, size_of::<Header>());
    assert_eq!(game::HEADER_MAGIC, offset_of!(Header, magic));
    assert_eq!(game::HEADER_KIND, offset_of!(Header, kind));
    assert_eq!(game::HEADER_PEER, offset_of!(Header, peer));
    assert_eq!(game::HEADER_TOKEN, offset_of!(Header, token));

    assert_eq!(game::HELLO_SIZE, size_of::<Hello>());
    assert_eq!(game::HELLO_PROTOCOL, offset_of!(Hello, protocol));
    assert_eq!(game::HELLO_PASSWORD, offset_of!(Hello, password));
    assert_eq!(game::PASSWORD, size_of::<Hello>() - offset_of!(Hello, password));

    assert_eq!(game::ATTRIBUTE, size_of::<Attribute>());
    assert_eq!(game::ATTRIBUTE_ID, offset_of!(Attribute, id));
    assert_eq!(game::ATTRIBUTE_VALUE, offset_of!(Attribute, value));

    assert_eq!(game::ADVERT, size_of::<Advert>());
    assert_eq!(game::ADVERT_SEQUENCE, offset_of!(Advert, sequence));
    assert_eq!(game::ADVERT_RTT, offset_of!(Advert, rtt));
    assert_eq!(game::ADVERT_OPEN, offset_of!(Advert, open));
    assert_eq!(game::ADVERT_ATTRIBUTES, offset_of!(Advert, attributes));
    assert_eq!(game::ADVERT_SESSION, offset_of!(Advert, session));
    assert_eq!(game::ADVERT_OPEN_PUBLIC, offset_of!(Advert, open_public));
    assert_eq!(game::ADVERT_OPEN_PRIVATE, offset_of!(Advert, open_private));
    assert_eq!(game::ADVERT_FILLED_PUBLIC, offset_of!(Advert, filled_public));
    assert_eq!(game::ADVERT_FILLED_PRIVATE, offset_of!(Advert, filled_private));

    assert_eq!(game::WANT, size_of::<Want>());
    assert_eq!(game::WANT_ID, offset_of!(Want, id));
    assert_eq!(game::WANT_VALUE, offset_of!(Want, value));
    assert_eq!(game::WANT_AT_LEAST, offset_of!(Want, at_least));

    assert_eq!(game::SEARCH_SIZE, size_of::<Search>());
    assert_eq!(game::SEARCH_ID, offset_of!(Search, search));
    assert_eq!(game::SEARCH_ROOM, offset_of!(Search, room));
    assert_eq!(game::SEARCH_MOST, offset_of!(Search, most));
    assert_eq!(game::SEARCH_WANTS, offset_of!(Search, wants));

    assert_eq!(game::FOUND, size_of::<Found>());
    assert_eq!(game::FOUND_HOST, offset_of!(Found, host));
    assert_eq!(game::FOUND_SESSION, offset_of!(Found, session));
    assert_eq!(game::FOUND_OPEN_PUBLIC, offset_of!(Found, open_public));
    assert_eq!(game::FOUND_OPEN_PRIVATE, offset_of!(Found, open_private));
    assert_eq!(game::FOUND_FILLED_PUBLIC, offset_of!(Found, filled_public));
    assert_eq!(game::FOUND_FILLED_PRIVATE, offset_of!(Found, filled_private));
    assert_eq!(game::FOUND_RTT, offset_of!(Found, rtt));

    assert_eq!(game::RESULTS_PAGE, size_of::<ResultsPage>());
    assert_eq!(game::RESULTS_SEARCH, offset_of!(ResultsPage, search));
    assert_eq!(game::RESULTS_TOTAL, offset_of!(ResultsPage, total));
    assert_eq!(game::RESULTS_FIRST, offset_of!(ResultsPage, first));
    assert_eq!(game::RESULTS_COUNT, offset_of!(ResultsPage, count));

    assert_eq!(game::FILE_ASK_SIZE, size_of::<FileAsk>());
    assert_eq!(game::FILE_CHUNK_SIZE, size_of::<FileChunk>());
    assert_eq!(game::FILE_CHUNK_TOTAL, offset_of!(FileChunk, total));
    assert_eq!(game::FILE_CHUNK_OFFSET, offset_of!(FileChunk, offset));
}

/// What the game writes, the relay reads as written: a header, through zerocopy.
#[test]
fn header_reads_back() {
    use zerocopy::FromBytes;
    let bytes = game::header(game::SEARCH, 0x0102_0304_0506_0708, 0x1112_1314_1516_1718);
    let header = Header::read_from_bytes(&bytes[..]).unwrap();
    assert_eq!(header.magic.get(), MAGIC);
    assert_eq!(Kind::from_wire(header.kind), Some(Kind::Search));
    assert_eq!(header.peer.get(), 0x0102_0304_0506_0708);
    assert_eq!(header.token.get(), 0x1112_1314_1516_1718);
    let read = game::Header::read(&bytes);
    assert_eq!((read.kind, read.peer, read.token), (game::SEARCH, 0x0102_0304_0506_0708, 0x1112_1314_1516_1718));
}
