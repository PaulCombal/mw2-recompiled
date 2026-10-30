#pragma once

// Made from relay/src/protocol.rs by cbindgen: BLESS=1 cargo test. Do not edit.

#include <cstddef>
#include <cstdint>

namespace relay {

constexpr static const uint32_t MAGIC = 1297560146;

/// Raised whenever a packet's meaning changes: a copy and a relay of
/// different protocols refuse each other.
constexpr static const uint16_t PROTOCOL = 2;

constexpr static const uint16_t DEFAULT_PORT = 3074;

/// How often the game says Alive; the relay only forgets.
constexpr static const uint32_t ALIVE_EVERY_SECONDS = 1;

constexpr static const uint32_t FORGET_AFTER_SECONDS = 10;

/// How often the game says its listing again.
constexpr static const uint32_t ADVERTISE_EVERY_SECONDS = 2;

constexpr static const size_t SESSION_SIZE = 60;

constexpr static const size_t MOST_ATTRIBUTES = 32;

constexpr static const size_t FOUND_PER_PAGE = 12;

/// what the title asks for
constexpr static const size_t MOST_RESULTS = 50;

constexpr static const size_t CHUNK_BYTES = 1024;

/// what the title has room for
constexpr static const size_t LARGEST_FILE = (128 * 1024);

/// The largest datagram either side sends.
constexpr static const size_t LARGEST = ((64 * 1024) - 64);

/// Ids are 48 bits: the game makes the Ethernet address the title tells
/// machines apart by from the low 48 bits of a player's id.
constexpr static const uint64_t ID_MASK = 281474976710655;

enum Kind : uint8_t {
  /// copy: Hello follows; `peer` is the id it had, or 0; `token` the Challenge's, or 0
  Kind_Hello = 1,
  /// relay: `peer` is the copy's id
  Kind_Welcome = 2,
  /// relay: why, as text, follows
  Kind_Refused = 3,
  /// copy
  Kind_Alive = 4,
  /// copy: `peer` is who for; the bytes follow
  Kind_To = 5,
  /// copy: the bytes follow
  Kind_All = 6,
  /// relay: `peer` is who from; the bytes follow
  Kind_From = 7,
  /// relay: say Hello again
  Kind_Unknown = 8,
  /// copy: it leaves
  Kind_Bye = 9,
  /// relay: `token` is what every packet of this copy carries from now on
  Kind_Challenge = 10,
  /// copy: an Advert, and its Attributes
  Kind_Advertise = 11,
  /// copy: a Search, and its Wants
  Kind_Search = 12,
  /// relay: a ResultsPage, and its Found
  Kind_Results = 13,
  /// copy: `peer` is anything
  Kind_Ping = 14,
  /// relay: `peer` is the Ping's
  Kind_Pong = 15,
  /// copy: a FileAsk
  Kind_FileAsk = 16,
  /// relay: a FileChunk, and the bytes
  Kind_FileChunk = 17,
};

struct Header {
  uint32_t magic;
  uint8_t kind;
  uint8_t pad[3];
  uint64_t peer;
  /// the copy's, on everything it sends after the Challenge
  uint64_t token;
};

struct Hello {
  uint16_t protocol;
  uint8_t pad[2];
  /// NUL-padded
  uint8_t password[64];
};

/// A number a session is known by, which a Search compares.
struct Attribute {
  uint32_t id;
  uint32_t value;
};

struct Advert {
  /// higher on each one, so a late packet undoes nothing
  uint32_t sequence;
  /// the copy's way to the relay, in ms
  uint16_t rtt;
  /// 0: the listing is withdrawn, and the rest means nothing
  uint8_t open;
  uint8_t attributes;
  uint8_t session[SESSION_SIZE];
  uint32_t open_public;
  uint32_t open_private;
  uint32_t filled_public;
  uint32_t filled_private;
};

/// What a listing must have: an attribute of this id, equal to `value`, or
/// with at_least, no less than it.
struct Want {
  uint32_t id;
  uint32_t value;
  uint8_t at_least;
  uint8_t pad[3];
};

struct Search {
  /// the asker's number for it, in its ResultsPages
  uint32_t search;
  /// the open public slots a listing must have
  uint32_t room;
  /// how many listings it can take
  uint16_t most;
  uint16_t wants;
};

struct Found {
  /// the id of the copy that listed it, stamped by the relay
  uint64_t host;
  uint8_t session[SESSION_SIZE];
  uint32_t open_public;
  uint32_t open_private;
  uint32_t filled_public;
  uint32_t filled_private;
  /// the host's way to the relay
  uint16_t rtt;
  uint8_t pad[2];
};

struct ResultsPage {
  uint32_t search;
  /// listings in the whole answer
  uint16_t total;
  /// the first of this page's, in it
  uint16_t first;
  /// on this page
  uint16_t count;
  uint8_t pad[2];
};

struct FileAsk {
  uint32_t offset;
};

struct FileChunk {
  /// the file's size; 0 when the relay has none
  uint32_t total;
  uint32_t offset;
};

}  // namespace relay

namespace relay
{
    inline uint16_t Big16(uint16_t value) { return __builtin_bswap16(value); }
    inline uint32_t Big32(uint32_t value) { return __builtin_bswap32(value); }
    inline uint64_t Big64(uint64_t value) { return __builtin_bswap64(value); }

    static_assert(sizeof(Header) == 24);
    static_assert(sizeof(Hello) == 68);
    static_assert(sizeof(Advert) == 84);
    static_assert(sizeof(Want) == 12);
    static_assert(sizeof(Search) == 12);
    static_assert(sizeof(Found) == 88);
    static_assert(sizeof(ResultsPage) == 12);
}
