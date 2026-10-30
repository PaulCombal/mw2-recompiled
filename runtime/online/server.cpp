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
//
// The relay also keeps what copies list for matchmaking (relay/src/directory.rs).
// A listing is said again every few seconds, so one lost on the way, or a relay
// that restarted, costs nothing, and a withdrawal is said a few times. The way
// to the relay is measured once in a while: every packet of a match goes through
// it, so the round trip between two players is the sum of their two ways. The
// playlists are fetched once, before the title can ask for them.
#include "transport.h"
#include "udp.h"
#include "../env.h"
#include "../log.h"
#include "../../relay/protocol.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <map>
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
    constexpr auto kPingEvery = std::chrono::seconds(2);
    constexpr auto kAskAgainAfter = std::chrono::milliseconds(250);
    constexpr auto kSearchAgainAfter = std::chrono::seconds(1);
    constexpr auto kSearchWithin = std::chrono::seconds(3);
    constexpr int kWithdrawals = 3;

    struct PendingSearch
    {
        std::vector<uint8_t> packet;
        Clock::time_point started{}, sent{};
        int total = -1;   // -1 until the first page
        std::map<uint32_t, online::SessionFound> found;   // by place in the answer
    };

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
                if (id_) Say(relay::Kind_Bye, id_, nullptr, 0);
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
            std::vector<uint8_t> packet(relay::LARGEST);
            while (Clock::now() < deadline)
            {
                if (Clock::now() - hello >= kHelloEvery)
                {
                    SayHello(0);
                    hello = Clock::now();
                }
                relay::Header header;
                const long got = FromRelay(packet, header);
                if (got >= 0 && header.kind == relay::Kind_Challenge)
                {
                    token_.store(relay::Big64(header.token), std::memory_order_relaxed);
                    SayHello(0);
                    hello = Clock::now();
                    continue;
                }
                if (got >= 0 && header.kind == relay::Kind_Refused)
                {
                    LOGE("online: server %s refused this copy: %s", where_.c_str(), Reason(packet, got).c_str());
                    return false;
                }
                if (got >= 0 && header.kind == relay::Kind_Welcome)
                {
                    id_ = relay::Big64(header.peer) & relay::ID_MASK;
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
            FetchPlaylists();
            alive_ = std::thread([this] { KeepAlive(); });
            return true;
        }

        const char* Name() const override { return "server"; }
        uint64_t LocalId() const override { return id_; }
        uint32_t Slot() const override { return slot_; }

        bool SendTo(uint64_t peer, const void* bytes, size_t size) override
        {
            return Say(relay::Kind_To, peer, bytes, size);
        }

        bool SendAll(const void* bytes, size_t size) override
        {
            return Say(relay::Kind_All, 0, bytes, size);
        }

        bool Matchmaking() override { return true; }

        void List(const online::SessionListing* listing) override
        {
            std::lock_guard g(matchLock_);
            if (listing)
            {
                relay::Advert advert{};
                advert.open = 1;
                std::memcpy(advert.session, listing->session, sizeof advert.session);
                advert.open_public = relay::Big32(listing->openPublic);
                advert.open_private = relay::Big32(listing->openPrivate);
                advert.filled_public = relay::Big32(listing->filledPublic);
                advert.filled_private = relay::Big32(listing->filledPrivate);
                const size_t count = std::min(listing->numbers.size(), relay::MOST_ATTRIBUTES);
                advert.attributes = uint8_t(count);
                std::vector<uint8_t> bytes(sizeof advert + count * sizeof(relay::Attribute));
                std::memcpy(bytes.data(), &advert, sizeof advert);
                for (size_t i = 0; i < count; i++)
                {
                    const relay::Attribute attribute{ relay::Big32(listing->numbers[i].id),
                                                      relay::Big32(listing->numbers[i].value) };
                    std::memcpy(bytes.data() + sizeof advert + i * sizeof attribute, &attribute, sizeof attribute);
                }
                if (bytes == listing_) return;
                if (listing_.empty()) LOGI("online: listing a session for matchmaking");
                listing_ = std::move(bytes);
                withdrawals_ = 0;
            }
            else
            {
                if (listing_.empty()) return;
                LOGI("online: no longer listing a session");
                listing_.clear();
                withdrawals_ = kWithdrawals;
            }
            SayListingLocked();
        }

        uint32_t Search(const online::SessionSearch& search) override
        {
            std::lock_guard g(matchLock_);
            // A search the title cancelled is never asked for again.
            const auto now = Clock::now();
            for (auto it = searches_.begin(); it != searches_.end();)
                it = now - it->second.started >= 2 * kSearchWithin ? searches_.erase(it) : std::next(it);

            const uint32_t id = ++searchId_;
            const size_t count = std::min(search.wants.size(), relay::MOST_ATTRIBUTES);
            const relay::Search head{ relay::Big32(id), relay::Big32(search.room),
                                      relay::Big16(uint16_t(std::min<size_t>(search.most, relay::MOST_RESULTS))),
                                      relay::Big16(uint16_t(count)) };
            PendingSearch pending;
            pending.packet.resize(sizeof head + count * sizeof(relay::Want));
            std::memcpy(pending.packet.data(), &head, sizeof head);
            for (size_t i = 0; i < count; i++)
            {
                const online::SessionWant& want = search.wants[i];
                const relay::Want wire{ relay::Big32(want.id), relay::Big32(want.value), uint8_t(want.atLeast), {} };
                std::memcpy(pending.packet.data() + sizeof head + i * sizeof wire, &wire, sizeof wire);
            }
            pending.started = pending.sent = now;
            Say(relay::Kind_Search, 0, pending.packet.data(), pending.packet.size());
            searches_[id] = std::move(pending);
            return id;
        }

        bool SearchResults(uint32_t search, std::vector<online::SessionFound>& out) override
        {
            std::lock_guard g(matchLock_);
            const auto it = searches_.find(search);
            if (it == searches_.end()) return true;
            PendingSearch& pending = it->second;
            const auto now = Clock::now();
            const bool complete = pending.total >= 0 && pending.found.size() == size_t(pending.total);
            if (!complete && now - pending.started < kSearchWithin)
            {
                if (now - pending.sent >= kSearchAgainAfter)
                {
                    pending.sent = now;
                    Say(relay::Kind_Search, 0, pending.packet.data(), pending.packet.size());
                }
                return false;
            }
            if (!complete) LOGW("online: server %s did not finish answering a search", where_.c_str());
            out.clear();
            const uint32_t way = WayToRelayLocked();
            for (const auto& [place, found] : pending.found)
            {
                out.push_back(found);
                out.back().lag += way;
            }
            searches_.erase(it);
            return true;
        }

        bool TitleFile(const std::string& name, std::vector<uint8_t>& out) override
        {
            // playlists.info on the disc, playlists.patch2 after the title update.
            if (name.rfind("playlists.", 0) != 0 || playlists_.empty()) return false;
            out = playlists_;
            return true;
        }

        long Next(void* bytes, size_t size, uint64_t& from) override
        {
            thread_local std::vector<uint8_t> packet(relay::LARGEST);
            for (;;)
            {
                relay::Header header;
                const long got = FromRelay(packet, header);
                if (got < 0) return -1;
                switch (header.kind)
                {
                case relay::Kind_From:
                {
                    const size_t carried = std::min(size_t(got), size);
                    std::memcpy(bytes, packet.data(), carried);
                    from = relay::Big64(header.peer);
                    return long(carried);
                }
                case relay::Kind_Results:
                    HeardResults(packet, size_t(got));
                    break;
                case relay::Kind_Pong:
                    HeardPong(relay::Big64(header.peer));
                    break;
                case relay::Kind_Unknown:
                    // The relay restarted, or forgot this copy.
                    LOGW("online: server %s no longer knows this copy; joining again", where_.c_str());
                    SayHello(id_);
                    break;
                case relay::Kind_Challenge:
                    token_.store(relay::Big64(header.token), std::memory_order_relaxed);
                    SayHello(id_);
                    break;
                case relay::Kind_Welcome:
                    if ((relay::Big64(header.peer) & relay::ID_MASK) != id_)
                        LOGE("online: server %s took this copy back under another id: the players it knew no "
                             "longer reach it", where_.c_str());
                    break;
                case relay::Kind_Refused:
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
            std::string host = where_, port = std::to_string(relay::DEFAULT_PORT);
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
            if (size > relay::LARGEST - sizeof(relay::Header)) return false;
            thread_local std::vector<uint8_t> packet;
            packet.resize(sizeof(relay::Header) + size);
            const relay::Header header{ relay::Big32(relay::MAGIC), kind, {}, relay::Big64(peer),
                                        relay::Big64(token_.load(std::memory_order_relaxed)) };
            std::memcpy(packet.data(), &header, sizeof header);
            if (size) std::memcpy(packet.data() + sizeof header, bytes, size);
            said_.store(Clock::now().time_since_epoch().count(), std::memory_order_relaxed);
            return udp::SendDatagram(own_, packet.data(), packet.size(), relay_);
        }

        void SayHello(uint64_t had)
        {
            relay::Hello hello{};
            hello.protocol = relay::Big16(relay::PROTOCOL);
            std::memcpy(hello.password, password_.data(), password_.size());
            Say(relay::Kind_Hello, had, &hello, sizeof hello);
        }

        // The next packet from the relay, its header taken off: the size of
        // what follows it, or -1 when nothing is waiting. Anything that is not
        // the relay's is dropped.
        long FromRelay(std::vector<uint8_t>& packet, relay::Header& header)
        {
            thread_local std::vector<uint8_t> raw(relay::LARGEST);
            for (;;)
            {
                sockaddr_in from{};
                const long got = udp::ReceiveDatagram(own_, raw.data(), raw.size(), from);
                if (got < 0) return -1;
                if (from.sin_addr.s_addr != relay_.sin_addr.s_addr || from.sin_port != relay_.sin_port) continue;
                if (size_t(got) < sizeof header) continue;
                std::memcpy(&header, raw.data(), sizeof header);
                if (relay::Big32(header.magic) != relay::MAGIC) continue;
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

        // Every copy on a relay must have the same playlists: the title
        // compares their version when it joins a party. A relay without any,
        // or one that stops answering, leaves the title with an empty file, and
        // Find Game with nothing to offer.
        void FetchPlaylists()
        {
            const auto deadline = Clock::now() + kAnswerWithin;
            auto asked = Clock::time_point{};
            size_t total = 0;
            std::vector<uint8_t> file;
            std::vector<uint8_t> packet(relay::LARGEST);
            while (Clock::now() < deadline)
            {
                if (Clock::now() - asked >= kAskAgainAfter)
                {
                    const relay::FileAsk ask{ relay::Big32(uint32_t(file.size())) };
                    Say(relay::Kind_FileAsk, 0, &ask, sizeof ask);
                    asked = Clock::now();
                }
                relay::Header header;
                const long got = FromRelay(packet, header);
                if (got < 0) { std::this_thread::sleep_for(std::chrono::milliseconds(2)); continue; }
                relay::FileChunk chunk;
                if (header.kind != relay::Kind_FileChunk || size_t(got) < sizeof chunk) continue;
                std::memcpy(&chunk, packet.data(), sizeof chunk);
                total = relay::Big32(chunk.total);
                if (!total) { LOGW("online: server %s has no playlists", where_.c_str()); return; }
                if (total > relay::LARGEST_FILE || relay::Big32(chunk.offset) != file.size()) continue;
                file.insert(file.end(), packet.begin() + sizeof chunk, packet.begin() + got);
                if (file.size() > total) break;
                if (file.size() == total)
                {
                    LOGI("online: server %s gave its playlists, %zu bytes", where_.c_str(), total);
                    playlists_ = std::move(file);
                    return;
                }
                asked = Clock::time_point{};
            }
            LOGW("online: server %s did not give its playlists (%zu of %zu bytes)", where_.c_str(), file.size(), total);
        }

        // The shortest of the last few, which is the way itself and not what
        // happened to be queueing on it.
        uint32_t WayToRelayLocked() const
        {
            uint32_t way = 0;
            for (const uint16_t sample : ways_)
                if (sample && (!way || sample < way)) way = sample;
            return way;
        }

        void HeardPong(uint64_t sent)
        {
            const auto now = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now().time_since_epoch());
            const uint64_t took = uint64_t(now.count()) - sent;
            if (took > 60000) return;
            std::lock_guard g(matchLock_);
            ways_[wayNext_++ % ways_.size()] = uint16_t(std::max<uint64_t>(took, 1));
        }

        void HeardResults(const std::vector<uint8_t>& packet, size_t size)
        {
            relay::ResultsPage page;
            if (size < sizeof page) return;
            std::memcpy(&page, packet.data(), sizeof page);
            const size_t total = relay::Big16(page.total), first = relay::Big16(page.first), count = relay::Big16(page.count);
            if (count > relay::FOUND_PER_PAGE || total > relay::MOST_RESULTS || first + count > total ||
                size != sizeof page + count * sizeof(relay::Found))
                return;
            std::lock_guard g(matchLock_);
            const auto it = searches_.find(relay::Big32(page.search));
            if (it == searches_.end()) return;
            it->second.total = int(total);
            for (size_t i = 0; i < count; i++)
            {
                relay::Found wire;
                std::memcpy(&wire, packet.data() + sizeof page + i * sizeof wire, sizeof wire);
                online::SessionFound found;
                found.host = relay::Big64(wire.host) & relay::ID_MASK;
                std::memcpy(found.session, wire.session, sizeof found.session);
                found.openPublic = relay::Big32(wire.open_public);
                found.openPrivate = relay::Big32(wire.open_private);
                found.filledPublic = relay::Big32(wire.filled_public);
                found.filledPrivate = relay::Big32(wire.filled_private);
                found.lag = relay::Big16(wire.rtt);
                it->second.found[uint32_t(first + i)] = found;
            }
        }

        // The listing as it stands, under a new number; or, after a withdrawal,
        // that again.
        void SayListingLocked()
        {
            listed_ = Clock::now();
            if (listing_.empty())
            {
                if (!withdrawals_) return;
                withdrawals_--;
                relay::Advert closed{};
                closed.sequence = relay::Big32(++sequence_);
                Say(relay::Kind_Advertise, 0, &closed, sizeof closed);
                return;
            }
            relay::Advert advert;
            std::memcpy(&advert, listing_.data(), sizeof advert);
            advert.sequence = relay::Big32(++sequence_);
            advert.rtt = relay::Big16(uint16_t(WayToRelayLocked()));
            std::memcpy(listing_.data(), &advert, sizeof advert);
            Say(relay::Kind_Advertise, 0, listing_.data(), listing_.size());
            advert.sequence = 0;
            advert.rtt = 0;
            std::memcpy(listing_.data(), &advert, sizeof advert);
        }

        // Alive whenever a second went by with nothing said, and what must be
        // said again whether or not the title is polling.
        void KeepAlive()
        {
            std::unique_lock g(stopLock_);
            const auto every = std::chrono::seconds(relay::ALIVE_EVERY_SECONDS);
            auto pinged = Clock::time_point{};
            while (!stop_.wait_for(g, every / 4, [this] { return stopping_; }))
            {
                const auto now = Clock::now();
                const Clock::time_point said{ Clock::duration(said_.load(std::memory_order_relaxed)) };
                if (now - said >= every) Say(relay::Kind_Alive, id_, nullptr, 0);
                if (now - pinged >= kPingEvery)
                {
                    pinged = now;
                    const auto stamp = std::chrono::duration_cast<std::chrono::milliseconds>(now.time_since_epoch());
                    Say(relay::Kind_Ping, uint64_t(stamp.count()), nullptr, 0);
                }
                std::lock_guard match(matchLock_);
                if (now - listed_ >= std::chrono::seconds(relay::ADVERTISE_EVERY_SECONDS)) SayListingLocked();
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

        std::vector<uint8_t> playlists_;         // set before the title can ask

        std::mutex matchLock_;                   // everything below
        std::vector<uint8_t> listing_;           // the Advert and its Attributes; empty when nothing is listed
        int withdrawals_ = 0;                    // times left to say it was withdrawn
        uint32_t sequence_ = 0;
        Clock::time_point listed_{};             // when it was last said
        uint32_t searchId_ = 0;
        std::map<uint32_t, PendingSearch> searches_;
        std::array<uint16_t, 8> ways_{};         // the last round trips to the relay, in ms
        size_t wayNext_ = 0;
    };
}

std::unique_ptr<online::Transport> online::MakeServerTransport() { return std::make_unique<Server>(); }
std::unique_ptr<online::Service> online::MakeServer() { return MakePlayers(MakeServerTransport()); }
