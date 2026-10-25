#pragma once
// What the game (runtime/online/server.cpp) and the relay (relay.cpp) say to
// each other over UDP. Every packet is a Header, then what its kind carries;
// every number is big-endian.
//
// A copy says Hello, and the relay answers Challenge with a token made from
// the copy's address and a secret of the relay's, keeping nothing. The copy
// says Hello again with that token, the password and the protocol it speaks,
// and only then does the relay take it in: Welcome with the copy's id, or
// Refused with why. A packet with a forged sender address never gets that far,
// since its Challenge goes to the forged address. Every packet the copy sends
// after carries the token, and the relay drops one that does not, so nobody
// can send as somebody else.
//
// The copy then sends To one other copy or to All of them, and the relay hands
// each on as From, with the sender's id: the relay is the only one that knows
// where anyone is. A copy says Alive once a second when it has sent nothing
// else; the relay forgets a copy it has not heard from for
// kForgetAfterSeconds, or that says Bye, and answers anything from a copy it
// does not know with Unknown, which the copy answers with Hello again, asking
// for the id it had. To an address it has not checked, the relay sends
// nothing larger than what came from there.
#include <cstddef>
#include <cstdint>

namespace relay
{
    constexpr uint32_t kMagic = 0x4D573252;   // "MW2R"
    // Raised whenever a packet's meaning changes: a copy and a relay of
    // different protocols refuse each other.
    constexpr uint16_t kProtocol = 1;
    constexpr uint16_t kDefaultPort = 3074;
    constexpr int kAliveEverySeconds = 1;
    constexpr int kForgetAfterSeconds = 10;

    enum Kind : uint8_t
    {
        kHello = 1,       // copy: Hello follows; `peer` is the id it had, or 0; `token` the Challenge's, or 0
        kWelcome = 2,     // relay: `peer` is the copy's id
        kRefused = 3,     // relay: why, as text, follows
        kAlive = 4,       // copy
        kTo = 5,          // copy: `peer` is who for; the bytes follow
        kAll = 6,         // copy: the bytes follow
        kFrom = 7,        // relay: `peer` is who from; the bytes follow
        kUnknown = 8,     // relay: say Hello again
        kBye = 9,         // copy: it leaves
        kChallenge = 10,  // relay: `token` is what every packet of this copy carries from now on
    };

    struct Header
    {
        uint32_t magic;
        uint8_t kind;
        uint8_t pad[3];
        uint64_t peer;
        uint64_t token;   // the copy's, on everything it sends after the Challenge
    };
    static_assert(sizeof(Header) == 24);

    struct Hello
    {
        uint16_t protocol;
        uint8_t pad[2];
        char password[64];   // NUL-padded
    };
    static_assert(sizeof(Hello) == 68);

    // The largest datagram either side sends.
    constexpr size_t kLargest = 64 * 1024 - 64;

    // Ids are 48 bits: the game makes the Ethernet address the title tells
    // machines apart by from the low 48 bits of a player's id.
    constexpr uint64_t kIdMask = (uint64_t(1) << 48) - 1;

    inline uint16_t Big16(uint16_t value) { return __builtin_bswap16(value); }
    inline uint32_t Big32(uint32_t value) { return __builtin_bswap32(value); }
    inline uint64_t Big64(uint64_t value) { return __builtin_bswap64(value); }
}
