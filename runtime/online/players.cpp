// The service over a transport (transport.h): the players it reaches, with no
// account anywhere. lan and server are this, on the local network's UDP and on
// a relay server.
//
// Who the player is, to the title, is the profile at the first controller
// (signin.h), plus the transport's slot, so a second copy on one machine is a
// second player. MW2_NAME is another name for him than the profile's.
//
// Xbox LIVE lobbies: every copy says who it is and what can be joined, once a
// second, to everyone the transport reaches. MW2_LAN_JOIN=<name> joins that
// player's party or lobby as soon as it is seen, as choosing a friend and
// "Join Session" does on the console. The lobby's "Invite friends" invites
// every player seen; a copy started with MW2_LAN_ACCEPT=1 accepts.
#include "transport.h"
#include "../env.h"
#include "../log.h"
#include "../signin.h"

#include <chrono>
#include <cstdio>
#include <cstring>
#include <deque>
#include <map>
#include <mutex>
#include <random>
#include <string>
#include <vector>

namespace
{
    // What travels is big-endian; every host this runs on is little-endian.
    uint64_t BigEndian64(uint64_t value) { return __builtin_bswap64(value); }
    uint32_t BigEndian32(uint32_t value) { return __builtin_bswap32(value); }
    uint16_t BigEndian16(uint16_t value) { return __builtin_bswap16(value); }

    // Magic, the sender's instance, its port and the port it is for.
    struct Frame
    {
        uint32_t magic;
        uint32_t instance;
        uint16_t fromPort, toPort;
    };
    constexpr uint32_t kMagic = 0x4D57324C;   // "MW2L"

    // Who a copy is and what can be joined: to everyone as presence, or to one
    // player as an invitation.
    struct Presence
    {
        uint32_t magic;
        uint32_t instance;
        uint8_t kind;          // kAnnounce or kInvite
        uint8_t joinable;      // `session` holds something
        uint8_t pad[2];
        uint64_t account;      // big-endian
        char name[32];
        uint8_t session[online::kSessionInfoSize];
    };
    constexpr uint32_t kPresenceMagic = 0x4D573250;   // "MW2P"
    constexpr uint8_t kAnnounce = 0, kInvite = 1;
    constexpr auto kAnnounceEvery = std::chrono::seconds(1);

    struct Peer
    {
        uint64_t id = 0, account = 0;
        bool joinable = false;
        uint8_t session[online::kSessionInfoSize]{};
    };
    constexpr size_t kLargest = 64 * 1024;
    constexpr size_t kQueued = 512;

    class Players final : public online::Service
    {
    public:
        explicit Players(std::unique_ptr<online::Transport> transport) : transport_(std::move(transport)) {}

        bool Start() override
        {
            if (!transport_->Open()) return false;
            instance_ = std::random_device{}();
            const signin::Player player = signin::First();
            account_ = player.id;
            const char* name = env::Text("MW2_NAME");
            name_ = name ? name : player.name;
            if (const char* join = env::Text("MW2_LAN_JOIN")) join_ = join;
            accept_ = env::Flag("MW2_LAN_ACCEPT");
            LOGI("online: %s as \"%s\"", transport_->Name(), name_.c_str());
            return true;
        }

        uint64_t LocalId() override { return transport_->LocalId(); }
        // The profile, and which copy on this machine this is.
        uint64_t Account() override { return account_ + transport_->Slot(); }
        std::string LocalName() override { return name_; }

        bool Send(uint64_t peer, uint16_t fromPort, uint16_t toPort,
                  const void* bytes, size_t size) override
        {
            const std::vector<uint8_t>& packet = Framed(fromPort, toPort, bytes, size);
            return !packet.empty() && transport_->SendTo(peer, packet.data(), packet.size());
        }

        bool Broadcast(uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size) override
        {
            const std::vector<uint8_t>& packet = Framed(fromPort, toPort, bytes, size);
            return !packet.empty() && transport_->SendAll(packet.data(), packet.size());
        }

        bool Receive(uint16_t port, online::Datagram& out) override
        {
            std::lock_guard g(lock_);
            Drain();
            auto it = waiting_.find(port);
            if (it == waiting_.end() || it->second.empty()) return false;
            out = std::move(it->second.front());
            it->second.pop_front();
            return true;
        }

        bool Lobbies() override { return true; }

        void SetJoinable(const online::Joinable* joinable) override
        {
            std::lock_guard g(lock_);
            hasJoinable_ = joinable != nullptr;
            if (joinable) joinable_ = *joinable;
            AnnounceLocked();
        }

        bool NextInvite(online::Invite& out) override
        {
            std::lock_guard g(lock_);
            Drain();
            const auto now = std::chrono::steady_clock::now();
            if (now - announced_ >= kAnnounceEvery) AnnounceLocked();
            if (!join_.empty())
            {
                auto it = peers_.find(join_);
                if (it != peers_.end() && it->second.joinable)
                {
                    LOGI("online: %s joins %s", transport_->Name(), join_.c_str());
                    invites_.push_back(InviteFrom(it->second, false));
                    join_.clear();
                }
            }
            if (invites_.empty()) return false;
            out = invites_.front();
            invites_.pop_front();
            return true;
        }

        bool Matchmaking() override { return transport_->Matchmaking(); }
        void List(const online::SessionListing* listing) override { transport_->List(listing); }
        uint32_t Search(const online::SessionSearch& search) override { return transport_->Search(search); }
        // Results arrive with everything else the transport holds.
        bool SearchResults(uint32_t search, std::vector<online::SessionFound>& out) override
        {
            {
                std::lock_guard g(lock_);
                Drain();
            }
            return transport_->SearchResults(search, out);
        }
        bool TitleFile(const std::string& name, std::vector<uint8_t>& out) override
        {
            return transport_->TitleFile(name, out);
        }

        void InviteFriends() override
        {
            std::lock_guard g(lock_);
            if (!hasJoinable_) { LOGW("online: nothing to invite anyone to yet"); return; }
            const Presence invite = PresenceLocked(kInvite);
            for (const auto& [name, peer] : peers_)
            {
                LOGI("online: %s invites %s", transport_->Name(), name.c_str());
                transport_->SendTo(peer.id, &invite, sizeof invite);
            }
        }

    private:
        static online::Invite InviteFrom(const Peer& peer, bool fromInvite)
        {
            online::Invite invite;
            invite.inviter = peer.account;
            std::memcpy(invite.session, peer.session, sizeof invite.session);
            invite.fromInvite = fromInvite;
            return invite;
        }

        // The title's datagram behind a frame; empty when it is too large.
        const std::vector<uint8_t>& Framed(uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size)
        {
            thread_local std::vector<uint8_t> packet;
            packet.clear();
            if (size > kLargest - sizeof(Frame)) return packet;
            packet.resize(sizeof(Frame) + size);
            const Frame frame{ BigEndian32(kMagic), instance_, BigEndian16(fromPort), BigEndian16(toPort) };
            std::memcpy(packet.data(), &frame, sizeof frame);
            std::memcpy(packet.data() + sizeof frame, bytes, size);
            return packet;
        }

        Presence PresenceLocked(uint8_t kind)
        {
            Presence presence{};
            presence.magic = BigEndian32(kPresenceMagic);
            presence.instance = instance_;
            presence.kind = kind;
            presence.joinable = hasJoinable_;
            presence.account = BigEndian64(Account());
            std::snprintf(presence.name, sizeof presence.name, "%s", name_.c_str());
            if (hasJoinable_) std::memcpy(presence.session, joinable_.session, sizeof presence.session);
            return presence;
        }

        void AnnounceLocked()
        {
            announced_ = std::chrono::steady_clock::now();
            const Presence presence = PresenceLocked(kAnnounce);
            transport_->SendAll(&presence, sizeof presence);
        }

        void Heard(uint64_t from, const Presence& presence)
        {
            if (presence.instance == instance_) return;
            const std::string name(presence.name, strnlen(presence.name, sizeof presence.name));
            Peer& peer = peers_[name];
            peer.id = from;
            peer.account = BigEndian64(presence.account);
            if (peer.account == Account() && !warned_)
            {
                warned_ = true;
                LOGE("online: %s on this %s is the same player as this one; on one of the two machines,"
                     " take the `machine` line out of saves/profiles.txt", name.c_str(),
                     transport_->Name() == std::string("lan") ? "network" : "server");
            }
            peer.joinable = presence.joinable;
            std::memcpy(peer.session, presence.session, sizeof peer.session);
            if (presence.kind != kInvite || !peer.joinable) return;
            if (accept_)
            {
                LOGI("online: %s accepts %s's invitation", transport_->Name(), name.c_str());
                invites_.push_back(InviteFrom(peer, true));
            }
            else
                LOGI("online: %s invites you (MW2_LAN_ACCEPT=1 accepts)", name.c_str());
        }

        // Everything the transport holds, sorted by the port it is for. A
        // broadcast can come back to the copy that sent it; the instance tells
        // it apart.
        void Drain()
        {
            thread_local std::vector<uint8_t> packet(kLargest);
            for (;;)
            {
                uint64_t from = 0;
                const long got = transport_->Next(packet.data(), packet.size(), from);
                if (got < 0) return;
                uint32_t magic = 0;
                if (size_t(got) >= sizeof magic) std::memcpy(&magic, packet.data(), sizeof magic);
                if (BigEndian32(magic) == kPresenceMagic && size_t(got) == sizeof(Presence))
                {
                    Presence presence;
                    std::memcpy(&presence, packet.data(), sizeof presence);
                    Heard(from, presence);
                    continue;
                }
                Frame frame;
                if (size_t(got) < sizeof frame) continue;
                std::memcpy(&frame, packet.data(), sizeof frame);
                if (BigEndian32(frame.magic) != kMagic || frame.instance == instance_) continue;
                online::Datagram datagram;
                datagram.from = from;
                datagram.fromPort = BigEndian16(frame.fromPort);
                datagram.toPort = BigEndian16(frame.toPort);
                datagram.bytes.assign(packet.begin() + sizeof frame, packet.begin() + got);
                // A port nobody reads fills up and drops, as a socket's buffer does.
                auto& queue = waiting_[datagram.toPort];
                if (queue.size() < kQueued) queue.push_back(std::move(datagram));
            }
        }

        std::unique_ptr<online::Transport> transport_;
        uint32_t instance_ = 0;
        std::string name_;
        uint64_t account_ = 0;
        bool warned_ = false;                  // told once that another machine has this identity
        std::mutex lock_;
        std::map<uint16_t, std::deque<online::Datagram>> waiting_;

        std::map<std::string, Peer> peers_;   // by name
        bool hasJoinable_ = false;
        online::Joinable joinable_;
        std::chrono::steady_clock::time_point announced_{};
        std::string join_;                     // MW2_LAN_JOIN, until joined
        bool accept_ = false;                  // MW2_LAN_ACCEPT
        std::deque<online::Invite> invites_;
    };
}

std::unique_ptr<online::Service> online::MakePlayers(std::unique_ptr<Transport> transport)
{
    return std::make_unique<Players>(std::move(transport));
}
