// mw2-relay: the server players choose with the launcher's NETWORK SERVER.
// Everyone connected to the same relay sees each other's System Link games and
// Xbox LIVE parties, and plays them through it: the relay hands every packet on
// (protocol.h), so the players need no open port and never learn each other's
// address. It runs no match itself; whoever hosts one plays in it.
//
//   mw2-relay [--port N] [--password TEXT] [--max N] [--per-address N]
//
// --port is the UDP port it listens on (3074), --password what a copy must
// give to join (MW2_RELAY_PASSWORD in the environment, for a container), --max
// how many copies it takes at once (64), --per-address how many of them from
// one IP address (16: players behind one router share theirs). It keeps
// nothing but the copies it took in, each forgotten when it leaves or goes
// quiet. It logs a line for each copy that joins and leaves, and stops on
// SIGINT or SIGTERM.
#include "protocol.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <map>
#include <random>
#include <string>
#include <vector>

namespace
{
    using Clock = std::chrono::steady_clock;

    volatile std::sig_atomic_t g_stop = 0;
    void Stop(int) { g_stop = 1; }

    void Log(const char* format, ...) __attribute__((format(printf, 1, 2)));
    void Log(const char* format, ...)
    {
        char when[32];
        const std::time_t now = std::time(nullptr);
        std::strftime(when, sizeof when, "%Y-%m-%d %H:%M:%S", std::gmtime(&now));
        std::printf("%s ", when);
        va_list args;
        va_start(args, format);
        std::vprintf(format, args);
        va_end(args);
        std::printf("\n");
        std::fflush(stdout);
    }

    // An address and port as one number, to look copies up by.
    uint64_t Key(const sockaddr_in& address)
    {
        return (uint64_t(ntohl(address.sin_addr.s_addr)) << 16) | ntohs(address.sin_port);
    }

    std::string Text(const sockaddr_in& address)
    {
        char text[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &address.sin_addr, text, sizeof text);
        return std::string(text) + ":" + std::to_string(ntohs(address.sin_port));
    }

    struct Copy
    {
        sockaddr_in address{};
        uint64_t id = 0, token = 0;
        Clock::time_point heard{};
    };

    uint64_t Rotate(uint64_t x, int bits) { return (x << bits) | (x >> (64 - bits)); }

    // SipHash-2-4 of one 64-bit word: what makes a copy's token from its
    // address, under a key nobody outside the relay knows.
    uint64_t SipHash(const uint64_t key[2], uint64_t word)
    {
        uint64_t v0 = 0x736F6D6570736575ull ^ key[0], v1 = 0x646F72616E646F6Dull ^ key[1];
        uint64_t v2 = 0x6C7967656E657261ull ^ key[0], v3 = 0x7465646279746573ull ^ key[1];
        auto round = [&] {
            v0 += v1; v1 = Rotate(v1, 13); v1 ^= v0; v0 = Rotate(v0, 32);
            v2 += v3; v3 = Rotate(v3, 16); v3 ^= v2;
            v0 += v3; v3 = Rotate(v3, 21); v3 ^= v0;
            v2 += v1; v1 = Rotate(v1, 17); v1 ^= v2; v2 = Rotate(v2, 32);
        };
        const uint64_t last = uint64_t(8) << 56;   // the message's length, 8 bytes, and no tail
        v3 ^= word; round(); round(); v0 ^= word;
        v3 ^= last; round(); round(); v0 ^= last;
        v2 ^= 0xFF; round(); round(); round(); round();
        return v0 ^ v1 ^ v2 ^ v3;
    }

    class Relay
    {
    public:
        Relay(int socket, std::string password, size_t most, size_t perAddress)
            : socket_(socket), password_(std::move(password)), most_(most), perAddress_(perAddress)
        {
            std::random_device device;
            for (uint64_t& part : key_) part = (uint64_t(device()) << 32) | device();
            random_.seed((uint64_t(device()) << 32) | device());
        }

        void Heard(const uint8_t* packet, size_t size, const sockaddr_in& from)
        {
            relay::Header header;
            if (size < sizeof header) return;
            std::memcpy(&header, packet, sizeof header);
            if (relay::Big32(header.magic) != relay::kMagic) return;
            const uint8_t* body = packet + sizeof header;
            const size_t carried = size - sizeof header;
            const uint64_t token = relay::Big64(header.token);

            if (header.kind == relay::kHello)
            {
                Hello(from, relay::Big64(header.peer) & relay::kIdMask, token, body, carried);
                return;
            }
            const auto known = copies_.find(Key(from));
            if (known == copies_.end())
            {
                // A copy this relay forgot, or never knew: it says Hello again.
                if (header.kind != relay::kBye) Say(from, relay::kUnknown, 0, nullptr, 0);
                return;
            }
            Copy& copy = known->second;
            // Somebody else, sending as this copy's address.
            if (token != copy.token) return;
            copy.heard = Clock::now();
            switch (header.kind)
            {
            case relay::kAlive:
                break;
            case relay::kTo:
            {
                const auto to = ids_.find(relay::Big64(header.peer));
                if (to != ids_.end()) Say(copies_.at(to->second).address, relay::kFrom, copy.id, body, carried);
                break;
            }
            case relay::kAll:
                for (const auto& [key, other] : copies_)
                    if (other.id != copy.id) Say(other.address, relay::kFrom, copy.id, body, carried);
                break;
            case relay::kBye:
                Log("%012llx left (%s), %zu here", static_cast<unsigned long long>(copy.id), Text(from).c_str(),
                    copies_.size() - 1);
                Forget(known);
                break;
            default:
                break;
            }
        }

        // Every copy not heard from for kForgetAfterSeconds: its game ended
        // without a Bye, or its connection did.
        void Sweep()
        {
            const auto now = Clock::now();
            for (auto it = copies_.begin(); it != copies_.end();)
            {
                if (now - it->second.heard < std::chrono::seconds(relay::kForgetAfterSeconds)) { ++it; continue; }
                Log("%012llx went quiet (%s), %zu here", static_cast<unsigned long long>(it->second.id),
                    Text(it->second.address).c_str(), copies_.size() - 1);
                it = Forget(it);
            }
        }

    private:
        uint64_t TokenOf(const sockaddr_in& address) const
        {
            const uint64_t token = SipHash(key_, Key(address));
            return token ? token : 1;   // 0 is no token
        }

        void Hello(const sockaddr_in& from, uint64_t had, uint64_t token, const uint8_t* body, size_t size)
        {
            relay::Hello hello;
            // Nothing is kept and nothing larger than the Hello goes back
            // until the address has shown it receives what is sent to it.
            if (size != sizeof hello) return;
            const uint64_t expected = TokenOf(from);
            if (token != expected)
            {
                Say(from, relay::kChallenge, 0, nullptr, 0, expected);
                return;
            }
            std::memcpy(&hello, body, sizeof hello);
            const uint16_t protocol = relay::Big16(hello.protocol);
            if (protocol != relay::kProtocol)
            {
                Refuse(from, "this relay speaks protocol " + std::to_string(relay::kProtocol) + ", the game " +
                                 std::to_string(protocol) + ": they come from different releases");
                return;
            }
            const std::string password(hello.password, strnlen(hello.password, sizeof hello.password));
            if (password != password_) { Refuse(from, password.empty() ? "a password is needed" : "wrong password"); return; }

            const uint64_t key = Key(from);
            if (const auto known = copies_.find(key); known != copies_.end())
            {
                // Its Welcome was lost; the same answer again.
                known->second.heard = Clock::now();
                Say(from, relay::kWelcome, known->second.id, nullptr, 0);
                return;
            }
            if (copies_.size() >= most_) { Refuse(from, "the relay is full"); return; }
            size_t fromThere = 0;
            for (const auto& [other, copy] : copies_)
                fromThere += copy.address.sin_addr.s_addr == from.sin_addr.s_addr;
            if (fromThere >= perAddress_) { Refuse(from, "too many players from this address"); return; }
            // The id it had, if nobody has it now; a new one otherwise.
            uint64_t id = had && !ids_.count(had) ? had : 0;
            while (!id || ids_.count(id)) id = random_() & relay::kIdMask;
            Copy& copy = copies_[key];
            copy.address = from;
            copy.id = id;
            copy.token = expected;
            copy.heard = Clock::now();
            ids_[id] = key;
            Log("%012llx joined (%s), %zu here", static_cast<unsigned long long>(id), Text(from).c_str(), copies_.size());
            Say(from, relay::kWelcome, id, nullptr, 0);
        }

        void Refuse(const sockaddr_in& to, const std::string& why)
        {
            Log("refused %s: %s", Text(to).c_str(), why.c_str());
            Say(to, relay::kRefused, 0, why.data(), why.size());
        }

        std::map<uint64_t, Copy>::iterator Forget(std::map<uint64_t, Copy>::iterator it)
        {
            ids_.erase(it->second.id);
            return copies_.erase(it);
        }

        void Say(const sockaddr_in& to, relay::Kind kind, uint64_t peer, const void* bytes, size_t size, uint64_t token = 0)
        {
            if (size > relay::kLargest - sizeof(relay::Header)) return;
            packet_.resize(sizeof(relay::Header) + size);
            const relay::Header header{ relay::Big32(relay::kMagic), kind, {}, relay::Big64(peer), relay::Big64(token) };
            std::memcpy(packet_.data(), &header, sizeof header);
            if (size) std::memcpy(packet_.data() + sizeof header, bytes, size);
            ::sendto(socket_, packet_.data(), packet_.size(), 0, reinterpret_cast<const sockaddr*>(&to), sizeof to);
        }

        int socket_;
        std::string password_;
        size_t most_, perAddress_;
        uint64_t key_[2]{};                 // makes the tokens; new at every start
        std::mt19937_64 random_;
        std::map<uint64_t, Copy> copies_;   // by Key(address); nothing else is kept
        std::map<uint64_t, uint64_t> ids_;  // id -> Key(address)
        std::vector<uint8_t> packet_;
    };

    [[noreturn]] void Usage()
    {
        std::fprintf(stderr, "usage: mw2-relay [--port N] [--password TEXT] [--max N] [--per-address N]\n");
        std::exit(2);
    }
}

int main(int argc, char** argv)
{
    long port = relay::kDefaultPort, most = 64, perAddress = 16;
    std::string password;
    if (const char* fromEnvironment = std::getenv("MW2_RELAY_PASSWORD")) password = fromEnvironment;
    for (int i = 1; i < argc; i++)
    {
        const std::string option = argv[i];
        if (i + 1 >= argc) Usage();
        const char* value = argv[++i];
        if (option == "--port") port = std::strtol(value, nullptr, 10);
        else if (option == "--password") password = value;
        else if (option == "--max") most = std::strtol(value, nullptr, 10);
        else if (option == "--per-address") perAddress = std::strtol(value, nullptr, 10);
        else Usage();
    }
    if (port < 1 || port > 65535 || most < 1 || perAddress < 1) Usage();
    if (password.size() >= sizeof(relay::Hello::password))
    {
        std::fprintf(stderr, "mw2-relay: the password is longer than %zu characters\n", sizeof(relay::Hello::password) - 1);
        return 2;
    }

    const int s = ::socket(AF_INET, SOCK_DGRAM, 0);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_ANY);
    address.sin_port = htons(uint16_t(port));
    if (s < 0 || ::bind(s, reinterpret_cast<sockaddr*>(&address), sizeof address) != 0)
    {
        std::fprintf(stderr, "mw2-relay: cannot listen on UDP port %ld: %s\n", port, std::strerror(errno));
        return 1;
    }
    std::signal(SIGINT, Stop);
    std::signal(SIGTERM, Stop);
    Log("mw2-relay, protocol %u, on UDP port %ld, %s, at most %ld copies, %ld from one address", relay::kProtocol,
        port, password.empty() ? "no password" : "with a password", most, perAddress);

    Relay relay(s, password, size_t(most), size_t(perAddress));
    std::vector<uint8_t> packet(relay::kLargest);
    auto swept = Clock::now();
    while (!g_stop)
    {
        pollfd waiting{ s, POLLIN, 0 };
        if (::poll(&waiting, 1, 1000) > 0)
        {
            sockaddr_in from{};
            socklen_t length = sizeof from;
            const ssize_t got = ::recvfrom(s, packet.data(), packet.size(), 0, reinterpret_cast<sockaddr*>(&from), &length);
            if (got > 0) relay.Heard(packet.data(), size_t(got), from);
        }
        if (Clock::now() - swept >= std::chrono::seconds(1))
        {
            relay.Sweep();
            swept = Clock::now();
        }
    }
    Log("stopped");
    ::close(s);
    return 0;
}
