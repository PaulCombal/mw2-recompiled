// Players on a relay server (relay/): everyone who connects to the same one,
// wherever they are. MW2_SERVER is its address, host or host:port (3074 when no
// port is given), and MW2_SERVER_PASSWORD the password it asks for, if any.
// The relay hands every packet on, so nobody's address reaches the others, and
// only the relay's owner opens a port: every copy connects out to it.
//
// The relay challenges a copy's first Hello with a token, and gives it its id
// when it says Hello again with it (relay/protocol.h); a copy that cannot get
// one in kAnswerWithin does not start, and the title stays offline. A thread says Alive once a second, so a copy the title is not
// polling -- while a map loads -- stays known. Each copy has a port of its own,
// the first free one above MW2_LAN_PORT (3074), and how far above is its slot,
// as on lan.
#include "transport.h"
#include "udp.h"
#include "../env.h"
#include "../log.h"
#include "../../relay/protocol.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace
{
    using udp::Socket;
    using udp::kNoSocket;
    using Clock = std::chrono::steady_clock;

    constexpr auto kAnswerWithin = std::chrono::seconds(5);
    constexpr auto kHelloEvery = std::chrono::milliseconds(500);

    class Server final : public online::Transport
    {
    public:
        ~Server() override
        {
            if (alive_.joinable())
            {
                { std::lock_guard g(stopLock_); stopping_ = true; }
                stop_.notify_all();
                alive_.join();
            }
            if (own_ != kNoSocket)
            {
                if (id_) Say(relay::kBye, id_, nullptr, 0);
                udp::CloseSocket(own_);
            }
        }

        bool Open() override
        {
            if (!udp::Startup()) { LOGE("online: no Winsock"); return false; }
            const char* where = env::Text("MW2_SERVER");
            if (!where || !*where) { LOGE("online: server chosen, but no server set (MW2_SERVER)"); return false; }
            where_ = where;
            if (const char* password = env::Text("MW2_SERVER_PASSWORD")) password_ = password;
            if (password_.size() >= sizeof(relay::Hello::password))
            {
                LOGE("online: the server password is longer than %zu characters", sizeof(relay::Hello::password) - 1);
                return false;
            }
            if (!Resolve()) return false;
            own_ = udp::OpenOwnSocket(uint16_t(env::Number("MW2_LAN_PORT", 3074)), slot_);
            if (own_ == kNoSocket) { LOGE("online: server could not open a socket"); return false; }

            char text[INET_ADDRSTRLEN];
            inet_ntop(AF_INET, &relay_.sin_addr, text, sizeof text);
            LOGI("online: server %s (%s:%u), asking to join", where_.c_str(), text, ntohs(relay_.sin_port));
            const auto deadline = Clock::now() + kAnswerWithin;
            auto hello = Clock::time_point{};
            std::vector<uint8_t> packet(relay::kLargest);
            while (Clock::now() < deadline)
            {
                if (Clock::now() - hello >= kHelloEvery)
                {
                    SayHello(0);
                    hello = Clock::now();
                }
                relay::Header header;
                const long got = FromRelay(packet, header);
                if (got >= 0 && header.kind == relay::kChallenge)
                {
                    token_.store(relay::Big64(header.token), std::memory_order_relaxed);
                    SayHello(0);
                    hello = Clock::now();
                    continue;
                }
                if (got >= 0 && header.kind == relay::kRefused)
                {
                    LOGE("online: server %s refused this copy: %s", where_.c_str(), Reason(packet, got).c_str());
                    return false;
                }
                if (got >= 0 && header.kind == relay::kWelcome)
                {
                    id_ = relay::Big64(header.peer) & relay::kIdMask;
                    break;
                }
                if (got < 0) std::this_thread::sleep_for(std::chrono::milliseconds(10));
            }
            if (!id_)
            {
                LOGE("online: server %s did not answer within %lld seconds", where_.c_str(),
                     static_cast<long long>(std::chrono::duration_cast<std::chrono::seconds>(kAnswerWithin).count()));
                return false;
            }
            LOGI("online: server %s welcomes this copy as %012llx", where_.c_str(), static_cast<unsigned long long>(id_));
            alive_ = std::thread([this] { KeepAlive(); });
            return true;
        }

        const char* Name() const override { return "server"; }
        uint64_t LocalId() const override { return id_; }
        uint32_t Slot() const override { return slot_; }

        bool SendTo(uint64_t peer, const void* bytes, size_t size) override
        {
            return Say(relay::kTo, peer, bytes, size);
        }

        bool SendAll(const void* bytes, size_t size) override
        {
            return Say(relay::kAll, 0, bytes, size);
        }

        long Next(void* bytes, size_t size, uint64_t& from) override
        {
            thread_local std::vector<uint8_t> packet(relay::kLargest);
            for (;;)
            {
                relay::Header header;
                const long got = FromRelay(packet, header);
                if (got < 0) return -1;
                switch (header.kind)
                {
                case relay::kFrom:
                {
                    const size_t carried = std::min(size_t(got), size);
                    std::memcpy(bytes, packet.data(), carried);
                    from = relay::Big64(header.peer);
                    return long(carried);
                }
                case relay::kUnknown:
                    // The relay restarted, or forgot this copy.
                    LOGW("online: server %s no longer knows this copy; joining again", where_.c_str());
                    SayHello(id_);
                    break;
                case relay::kChallenge:
                    token_.store(relay::Big64(header.token), std::memory_order_relaxed);
                    SayHello(id_);
                    break;
                case relay::kWelcome:
                    if ((relay::Big64(header.peer) & relay::kIdMask) != id_)
                        LOGE("online: server %s took this copy back under another id: the players it knew no "
                             "longer reach it", where_.c_str());
                    break;
                case relay::kRefused:
                    LOGE("online: server %s refused this copy: %s", where_.c_str(), Reason(packet, got).c_str());
                    break;
                default:
                    break;
                }
            }
        }

    private:
        // MW2_SERVER's host and port, the first IPv4 address of the host.
        bool Resolve()
        {
            std::string host = where_, port = std::to_string(relay::kDefaultPort);
            if (const size_t colon = where_.rfind(':'); colon != std::string::npos)
            {
                host = where_.substr(0, colon);
                port = where_.substr(colon + 1);
            }
            addrinfo hints{};
            hints.ai_family = AF_INET;
            hints.ai_socktype = SOCK_DGRAM;
            addrinfo* found = nullptr;
            if (getaddrinfo(host.c_str(), port.c_str(), &hints, &found) != 0 || !found)
            {
                LOGE("online: server %s: no such address", where_.c_str());
                return false;
            }
            std::memcpy(&relay_, found->ai_addr, sizeof relay_);
            freeaddrinfo(found);
            return true;
        }

        bool Say(relay::Kind kind, uint64_t peer, const void* bytes, size_t size)
        {
            if (size > relay::kLargest - sizeof(relay::Header)) return false;
            thread_local std::vector<uint8_t> packet;
            packet.resize(sizeof(relay::Header) + size);
            const relay::Header header{ relay::Big32(relay::kMagic), kind, {}, relay::Big64(peer),
                                        relay::Big64(token_.load(std::memory_order_relaxed)) };
            std::memcpy(packet.data(), &header, sizeof header);
            if (size) std::memcpy(packet.data() + sizeof header, bytes, size);
            said_.store(Clock::now().time_since_epoch().count(), std::memory_order_relaxed);
            return udp::SendDatagram(own_, packet.data(), packet.size(), relay_);
        }

        void SayHello(uint64_t had)
        {
            relay::Hello hello{};
            hello.protocol = relay::Big16(relay::kProtocol);
            std::memcpy(hello.password, password_.data(), password_.size());
            Say(relay::kHello, had, &hello, sizeof hello);
        }

        // The next packet from the relay, its header taken off: the size of
        // what follows it, or -1 when nothing is waiting. Anything that is not
        // the relay's is dropped.
        long FromRelay(std::vector<uint8_t>& packet, relay::Header& header)
        {
            thread_local std::vector<uint8_t> raw(relay::kLargest);
            for (;;)
            {
                sockaddr_in from{};
                const long got = udp::ReceiveDatagram(own_, raw.data(), raw.size(), from);
                if (got < 0) return -1;
                if (from.sin_addr.s_addr != relay_.sin_addr.s_addr || from.sin_port != relay_.sin_port) continue;
                if (size_t(got) < sizeof header) continue;
                std::memcpy(&header, raw.data(), sizeof header);
                if (relay::Big32(header.magic) != relay::kMagic) continue;
                const size_t carried = size_t(got) - sizeof header;
                std::memcpy(packet.data(), raw.data() + sizeof header, carried);
                return long(carried);
            }
        }

        static std::string Reason(const std::vector<uint8_t>& packet, long size)
        {
            std::string text(reinterpret_cast<const char*>(packet.data()), size_t(size));
            for (char& c : text)
                if (c < ' ' || c > '~') c = '?';
            return text;
        }

        // Alive whenever a second went by with nothing said.
        void KeepAlive()
        {
            std::unique_lock g(stopLock_);
            const auto every = std::chrono::seconds(relay::kAliveEverySeconds);
            while (!stop_.wait_for(g, every / 4, [this] { return stopping_; }))
            {
                const Clock::time_point said{ Clock::duration(said_.load(std::memory_order_relaxed)) };
                if (Clock::now() - said >= every) Say(relay::kAlive, id_, nullptr, 0);
            }
        }

        std::string where_, password_;
        sockaddr_in relay_{};
        Socket own_ = kNoSocket;
        uint32_t slot_ = 0;
        uint64_t id_ = 0;
        std::atomic<Clock::rep> said_{ 0 };   // when this copy last sent the relay anything
        std::atomic<uint64_t> token_{ 0 };    // the relay's Challenge, on everything sent after it
        std::thread alive_;
        std::mutex stopLock_;
        std::condition_variable stop_;
        bool stopping_ = false;
    };
}

std::unique_ptr<online::Transport> online::MakeServerTransport() { return std::make_unique<Server>(); }
std::unique_ptr<online::Service> online::MakeServer() { return MakePlayers(MakeServerTransport()); }
