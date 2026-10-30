#pragma once
// The online service the title's network calls are served by: who the player
// is, and how datagrams reach other players.
//
// Nothing here is Xbox-shaped. The kernel side (kernel/net.cpp, kernel/xam.cpp)
// keeps everything the console's API is made of -- XNADDRs, session keys, the
// title's sockets and ports -- and asks this only for what a service does. A
// peer is a 64-bit id the backend chooses (an address, a SteamID, an account
// number); the kernel never looks inside it.
//
// Every build has every backend, and the player chooses one as the game starts
// (MW2_NETWORK, the launcher's NETWORK): none, the default, lan, server or
// steam, which is lan when Steam is not running. A backend is a function that
// makes it, listed in service.cpp. With none, there is no service, and the
// title keeps its offline system link on this machine.
#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace online
{
    struct Datagram
    {
        uint64_t from = 0;
        uint16_t fromPort = 0, toPort = 0;   // the title's own port numbers
        std::vector<uint8_t> bytes;
    };

    // A session a friend can join, opaque to the service: the console's
    // XSESSION_INFO (session id, host address, key), 60 bytes.
    constexpr size_t kSessionInfoSize = 60;
    struct Joinable
    {
        uint8_t session[kSessionInfoSize]{};
    };

    struct Invite
    {
        uint64_t inviter = 0;   // the host's Account()
        uint8_t session[kSessionInfoSize]{};
        bool fromInvite = false;   // an invitation, not the player choosing to join
    };

    // A session listed for matchmaking. Its numbers are the title's contexts and
    // properties, which a search compares; the service compares them without
    // knowing what they mean.
    struct SessionNumber
    {
        uint32_t id = 0, value = 0;
    };
    struct SessionListing
    {
        uint8_t session[kSessionInfoSize]{};
        uint32_t openPublic = 0, openPrivate = 0, filledPublic = 0, filledPrivate = 0;
        std::vector<SessionNumber> numbers;
    };
    struct SessionWant
    {
        uint32_t id = 0, value = 0;
        bool atLeast = false;   // no less than `value`, not equal to it
    };
    struct SessionSearch
    {
        uint32_t room = 0;      // open public slots a session must have
        uint32_t most = 0;
        std::vector<SessionWant> wants;
    };
    struct SessionFound
    {
        uint64_t host = 0;      // the service's id for the player listing it
        uint8_t session[kSessionInfoSize]{};
        uint32_t openPublic = 0, openPrivate = 0, filledPublic = 0, filledPrivate = 0;
        uint32_t lag = 0;       // the round trip between the two players, in ms
    };

    // A friend running this game, as the friends screen shows them.
    struct Friend
    {
        uint64_t id = 0;
        std::string name;
        bool withYou = false;          // already in this player's party or game
        std::string elsewhere;         // why they cannot join: "On another release"; empty if they can
        std::vector<uint8_t> avatar;   // RGBA, avatarSide pixels square; may be empty
        uint32_t avatarSide = 0;
    };
    struct FriendList
    {
        uint64_t version = 0;          // 0: not built yet
        std::vector<Friend> playing;   // by name
        int online = 0, offline = 0;   // the other friends, not running it
    };

    class Service
    {
    public:
        virtual ~Service() = default;

        // Logs in. False leaves the title offline, as if no backend were built.
        virtual bool Start() = 0;

        // Where other players reach this one: unique among everyone it can
        // reach, and stable for the run.
        virtual uint64_t LocalId() = 0;
        // Who the player is, the same every run: the title keeps the player's
        // multiplayer rank and settings under it. The profile at the first
        // controller (signin.h), and unique among everyone in a match.
        virtual uint64_t Account() = 0;
        // The name the service has for the player, or empty: he then goes by
        // his profile's.
        // The title shows the first 15 characters.
        virtual std::string LocalName() = 0;

        // Unreliable and unordered, like the UDP the title thinks it has. Ports
        // are the title's: a datagram sent to port 1000 is received on 1000.
        virtual bool Send(uint64_t peer, uint16_t fromPort, uint16_t toPort,
                          const void* bytes, size_t size) = 0;
        // To everyone who could join this player's games: the title finds
        // system-link games by broadcasting for them. Never to this player.
        virtual bool Broadcast(uint16_t fromPort, uint16_t toPort,
                               const void* bytes, size_t size) = 0;
        // The next datagram that arrived for `port`, without waiting.
        virtual bool Receive(uint16_t port, Datagram& out) = 0;

        // Optional: invitations. A service that can bring a friend to this
        // player's game is told how to join it -- the console line the title's
        // own server browser types, or "" when there is nothing to join -- and
        // hands such a line back when this player accepts an invitation.
        virtual void Advertise(const std::string& connect) { (void)connect; }
        virtual bool NextJoin(std::string& connect) { (void)connect; return false; }
        // Invites everyone this player could invite, for a service without
        // friends to pick from.
        virtual void InviteFriends() {}

        // Optional: friends to pick from, for the friends screen (friends.h).
        // False when the service has none. The list is copied into `list` only
        // when its version is not the one `list` holds; asking keeps it fresh
        // for a while, and a service builds it off the caller's thread.
        virtual bool Friends(FriendList& list) { (void)list; return false; }
        // Invites one of them to this player's party or game.
        virtual bool InviteFriend(uint64_t id) { (void)id; return false; }

        // Optional: Xbox LIVE parties and private matches. A service that says
        // yes here has the player signed in to Live, which opens the title's
        // Xbox LIVE menu; one that says no leaves it refusing, as offline.
        virtual bool Lobbies() { return false; }
        // What a friend joins to be in this player's party or lobby: a session
        // the kernel describes in bytes the service only stores and hands back.
        // Null when there is nothing to join.
        virtual void SetJoinable(const Joinable* joinable) { (void)joinable; }
        // The next invitation this player accepted, or friend they chose to
        // join, without waiting. Polled once a frame on the title's main thread.
        virtual bool NextInvite(Invite& out) { (void)out; return false; }

        // Optional: matchmaking. Without it a search finds nothing, and the
        // title lists a lobby nobody will see and waits in it.
        virtual bool Matchmaking() { return false; }
        // What this player lists now; null when nothing.
        virtual void List(const SessionListing* listing) { (void)listing; }
        // Starts a search, and gives what the results are asked by.
        virtual uint32_t Search(const SessionSearch& search) { (void)search; return 0; }
        // Its results once it has finished, without waiting. A search that
        // gets no answer finishes empty, as one that found nothing.
        virtual bool SearchResults(uint32_t search, std::vector<SessionFound>& out)
        {
            (void)search; (void)out;
            return false;
        }

        // Optional: a file of the title's the service holds for everyone, so
        // that all its players have the same bytes.
        virtual bool TitleFile(const std::string& name, std::vector<uint8_t>& out)
        {
            (void)name; (void)out;
            return false;
        }
    };

    // One per backend, in its file.
    std::unique_ptr<Service> MakeSteam();
    std::unique_ptr<Service> MakeLan();
    std::unique_ptr<Service> MakeServer();

    // Before any thread exists, with steam chosen. A Steam the Flatpak runs is
    // in a process namespace of its own, and tells its clients apart by process
    // id, so a client outside it has its connection dropped mid-call. With such a Steam
    // running, this process joins its namespaces and a child goes on as the
    // game (true); the parent waits for it and returns false with its exit
    // status. True and nothing done everywhere else.
    bool EnterServiceNamespace(int& exitCode);
    bool EnterSteamNamespace(int& exitCode);   // steam.cpp's

    // The service, started on first use: the first backend built in that
    // starts. Null when there is none or none could start.
    Service* Get();
    // Its backend's name, "steam", "lan" or "server"; null without one.
    const char* Name();

    // Once a frame on the title's main thread: types an accepted invitation's
    // console line, or hands an accepted Live invitation to the title.
    void Frame();

    // The service can carry Xbox LIVE lobbies.
    bool Live();
}
