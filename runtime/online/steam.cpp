// Steam, through the Steam client the player already runs.
//
// libsteam_api is a thin layer over steamclient.so, the library every Steam
// install carries: it opens it, asks it for versioned interfaces, and calls
// them. This does the same with nothing of Valve's built in or shipped. The
// interfaces are C++ objects, called here through their vtables by slot; the
// slots and the structures below are those of the Steamworks SDK's headers
// (the versions named with each), and Steam keeps every version it ever
// shipped, so a build keeps working as the client updates.
//
// The title runs as Spacewar, app 480, which every Steam account can run
// (MW2_STEAM_APPID for another). Players are SteamIDs and datagrams are
// ISteamNetworkingMessages, which punches through NAT or relays through Valve.
//
// Each player sits in a friends-only lobby of their own from the start. The
// lobby is the network the title's broadcasts reach, so a game one member
// hosts shows up in every other member's System Link list. F6, or the Xbox
// LIVE lobby's "Invite friends", opens the friends screen (friends.h): the
// friends running this game, whom Steam tells apart from Spacewar's own
// players by a rich presence key of ours; a friend who accepts an invitation
// leaves their lobby for this one. The host's lobby carries what joins
// its game: the Xbox LIVE party or private match it is in ("xsession", which
// the friend's copy takes as an accepted Live invitation), or else the console
// line of the system-link game it hosts ("connect", which the friend's copy
// types).
//
// System Link is shared with every Steam player, as a local network is shared
// with everyone on it. A player hosting a system-link game is listed, in an
// invisible lobby of its own that searches return; a player whose title looks
// for games (a broadcast) sends it to the friends' lobby and to every host
// listed, worldwide. What a stranger can reach:
//   - a host, while it hosts: its networking accepts their sessions, and its
//     broadcasts -- the answers to a search -- reach them;
//   - a player who searched, on the ports its searches went to: anyone can
//     list a game, so a listed host is a stranger like any other;
//   - a player who chose to join the stranger's game, as on a local network.
// Everything else from a stranger is dropped as it arrives, so a player who
// stops hosting is closed to the strangers it played with. Their sessions are
// not closed, since a host's map change ends its game and starts the next one
// at once. Every Xbox LIVE party and private match stays among friends.
#include "service.h"
#include "../env.h"
#include "../log.h"
#include "../signin.h"
#include "../version.h"

#include <algorithm>
#include <cctype>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <filesystem>
#include <map>
#include <fstream>
#include <mutex>
#include <set>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include "../platform.h"
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <cerrno>
#include <csignal>
#include <fcntl.h>
#include <sched.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

namespace
{
    using HSteamPipe = int32_t;
    using HSteamUser = int32_t;
    using SteamAPICall = uint64_t;

    // One virtual call. A CSteamID travels as the uint64 it holds: the class is
    // trivially copyable, so the ABI passes and returns it in a register.
    template <typename R, typename... A>
    R Call(void* object, int slot, A... args)
    {
        return (*reinterpret_cast<R (***)(void*, A...)>(object))[slot](object, args...);
    }

    // A call that returns a CSteamID. The Windows client is built with MSVC,
    // whose member functions return any class through a hidden pointer after
    // `this`, however small; elsewhere it comes back in a register.
    template <typename... A>
    uint64_t CallSteamId(void* object, int slot, A... args)
    {
#ifdef _WIN32
        uint64_t id = 0;
        (*reinterpret_cast<uint64_t* (***)(void*, uint64_t*, A...)>(object))[slot](object, &id, args...);
        return id;
#else
        return Call<uint64_t>(object, slot, args...);
#endif
    }

    void SetEnvironment(const char* name, const char* value)
    {
#ifdef _WIN32
        // Both copies: the C library's, which getenv reads, and the process's
        // own, which the Steam client writes and SDL reads.
        _putenv_s(name, value ? value : "");
        SetEnvironmentVariableA(name, value);
#else
        if (value) setenv(name, value, 1);
        else unsetenv(name);
#endif
    }

    // Slots, from steam_api.json and the headers of the SDK version named.
    namespace client     // SteamClient023
    {
        constexpr int CreateSteamPipe = 0, ReleaseSteamPipe = 1, ConnectToGlobalUser = 2,
                      ReleaseUser = 4, GetUser = 5, GetFriends = 8, GetUtils = 9, GetMatchmaking = 10,
                      GetGeneric = 12;
    }
    namespace user { constexpr int LoggedOn = 1, GetSteamID = 2; }   // SteamUser023
    namespace friends    // SteamFriends018
    {
        constexpr int GetPersonaName = 0, Count = 2, ByIndex = 3, PersonaState = 5, Name = 6,
                      GamePlayed = 7, MediumAvatar = 34, SetRichPresence = 41, RichPresence = 43;
    }
    namespace utils { constexpr int ImageSize = 5, ImageRGBA = 6; }   // SteamUtils011
    namespace matchmaking   // SteamMatchMaking009
    {
        constexpr int CreateLobby = 13, JoinLobby = 14, LeaveLobby = 15, MemberCount = 17,
                      Member = 18, GetLobbyData = 19, SetLobbyData = 20, Owner = 35,
                      RequestLobbyList = 4, AddStringFilter = 5, AddDistanceFilter = 9,
                      LobbyByIndex = 12, InviteToLobby = 16, SetJoinable = 34;
    }
    namespace messages   // SteamNetworkingMessages002
    {
        constexpr int Send = 0, Receive = 1, Accept = 2, SessionInfo = 5;
    }

    // Callback ids: the interface's base plus the structure's offset.
    constexpr int kCallCompleted = 703;          // SteamAPICallCompleted_t
    constexpr int kLobbyEnter = 504;             // LobbyEnter_t
    constexpr int kLobbyDataUpdate = 505;        // LobbyDataUpdate_t
    constexpr int kLobbyCreated = 513;           // LobbyCreated_t
    constexpr int kLobbyMatchList = 510;         // LobbyMatchList_t
    constexpr int kLobbyJoinRequested = 333;     // GameLobbyJoinRequested_t
    constexpr int kPresenceJoinRequested = 337;  // GameRichPresenceJoinRequested_t
    constexpr int kSessionRequest = 1251;        // SteamNetworkingMessagesSessionRequest_t

    constexpr int kResultOK = 1;                 // EResult
    constexpr uint32_t kEnterSuccess = 1;        // EChatRoomEnterResponse
    constexpr int kFriendsOnly = 1, kInvisible = 3;   // ELobbyType
    constexpr int kMaxMembers = 18;              // the title's own sv_maxclients
    constexpr int kEqual = 0;                    // ELobbyComparison
    constexpr int kWorldwide = 3;                // ELobbyDistanceFilter
    constexpr int kImmediate = 4;                // EFriendFlags: friends, not requests or groups
    constexpr int kOffline = 0;                  // EPersonaState

    // The public list of system-link games: lobbies with this key, whose value
    // is the release's (version::Network()), so that only copies that can play
    // together see each other. A listing's "host" is the
    // SteamID that hosts. Nobody joins a listing, but a search never returns a
    // full lobby, so it takes Steam's most members: a stranger cannot hide it
    // by filling it.
    constexpr const char* kListingKey = "mw2recomp";
    constexpr int kListingMembers = 250;
    // The disc version and the update do not play together either.
#ifdef MW2_VERSION_TU0
    std::string ListingVersion() { return std::to_string(version::Network()) + "-tu0"; }
#else
    std::string ListingVersion() { return std::to_string(version::Network()); }
#endif
    // The friends screen's list is built this often while it is asked for,
    // and for this long after.
    constexpr auto kFriendsEvery = std::chrono::seconds(1);
    constexpr auto kFriendsWatched = std::chrono::seconds(3);
    // What a copy says it plays, in its rich presence under kListingKey: the
    // executable, whose invitations the other cannot take, and the release.
#ifdef MW2_TITLE_MP
    constexpr const char* kTitle = "mp";
#else
    constexpr const char* kTitle = "sp";
#endif
    std::string Playing() { return std::string(kTitle) + ":" + ListingVersion(); }
    // Why a friend who plays `playing` cannot join this copy, or "".
    std::string Elsewhere(const std::string& playing)
    {
        const size_t colon = playing.find(':');
        const std::string title = playing.substr(0, colon);
        if (colon == std::string::npos || (title != "mp" && title != "sp")) return "On another release";
        if (title != kTitle) return title == "mp" ? "In multiplayer" : "In the campaign or Spec Ops";
        return playing.substr(colon + 1) == ListingVersion() ? "" : "On another release";
    }
    // A listing Steam would not make is asked for again this often.
    constexpr auto kListAgainEvery = std::chrono::seconds(5);
    // The title broadcasts a search every few seconds while its list is open;
    // Steam is asked for the list at most this often.
    constexpr auto kSearchEvery = std::chrono::seconds(2);
    // The links to other players are looked at this often; one that goes
    // bad is told about at most this often.
    constexpr auto kLinksEvery = std::chrono::seconds(1);
    constexpr auto kPoorLinkEvery = std::chrono::seconds(5);
    // Unreliable, sent at once, and a session that broke is started again
    // rather than refused.
    constexpr int kSendFlags = 0 | 1 | 32;

    // SteamNetworkingIdentity, packed to 1 byte as the SDK declares it.
#pragma pack(push, 1)
    struct Identity
    {
        int32_t type;        // ESteamNetworkingIdentityType, 16 = SteamID
        int32_t size;
        union
        {
            uint64_t steamId;
            uint8_t raw[128];
        };
    };
#pragma pack(pop)
    static_assert(sizeof(Identity) == 136);

    Identity SteamIdentity(uint64_t steamId)
    {
        Identity identity{};
        identity.type = 16;
        identity.size = 8;
        identity.steamId = steamId;
        return identity;
    }

    // SteamNetConnectionInfo_t and SteamNetConnectionRealTimeStatus_t, laid
    // out alike packed to 4 and to 8.
    struct LinkInfo
    {
        Identity remote;
        int64_t userData;
        uint32_t listenSocket;
        uint8_t address[18];   // SteamNetworkingIPAddr, packed to 1
        uint16_t pad;
        uint32_t popRemote, popRelay;
        int32_t state, endReason;
        char endDebug[128];
        char description[128];
        int32_t flags;
        uint32_t reserved[63];
    };
    struct LinkStatus
    {
        int32_t state, ping;
        float qualityLocal, qualityRemote;   // the share of packets that arrive, here and there
        float outPackets, outBytes, inPackets, inBytes;
        int32_t sendRate, pendingUnreliable, pendingReliable, sentUnacked;
        int64_t queueMicroseconds;
        uint32_t reserved[16];
    };
    static_assert(offsetof(LinkInfo, state) == 176 && offsetof(LinkInfo, endDebug) == 184 &&
                  sizeof(LinkInfo) == 696);
    static_assert(offsetof(LinkStatus, queueMicroseconds) == 48 && sizeof(LinkStatus) == 120);
    constexpr int kConnected = 3, kClosedByPeer = 4, kProblem = 5;   // ESteamNetworkingConnectionState

    // The callback structures, packed as the SDK packs them: to 8 on Windows,
    // to 4 elsewhere. Of the ones read here only LobbyCreated's lobby moves.
#ifdef _WIN32
#pragma pack(push, 8)
#else
#pragma pack(push, 4)
#endif
    struct Message   // SteamNetworkingMessage_t
    {
        void* data;
        int32_t size;
        uint32_t connection;
        Identity peer;
        int64_t connectionUserData;
        int64_t received;
        int64_t number;
        void (*freeData)(Message*);
        void (*release)(Message*);
        int32_t channel;
        int32_t flags;
        int64_t userData;
        uint16_t lane;
        uint16_t pad;
    };
    struct CallbackMsg { HSteamUser user; int32_t id; uint8_t* param; int32_t size; };
    struct CallCompleted { SteamAPICall call; int32_t id; uint32_t size; };
    struct LobbyCreated { int32_t result; uint64_t lobby; };
    struct LobbyMatchList { uint32_t matching; };
    struct LobbyEnter { uint64_t lobby; uint32_t permissions; bool locked; uint32_t response; };
    struct LobbyDataUpdate { uint64_t lobby, member; uint8_t success; };
    struct LobbyJoinRequested { uint64_t lobby, friendId; };
    struct PresenceJoinRequested { uint64_t friendId; char connect[256]; };
    struct SessionRequest { Identity remote; };
    struct FriendGameInfo { uint64_t game; uint32_t ip; uint16_t port, queryPort; uint64_t lobby; };
#pragma pack(pop)
    static_assert(offsetof(Message, peer) == 16 && offsetof(Message, release) == 184 &&
                  offsetof(Message, channel) == 192);
    static_assert(offsetof(CallbackMsg, param) == 8);
    static_assert(offsetof(FriendGameInfo, lobby) == 16 && sizeof(FriendGameInfo) == 24);
#ifdef _WIN32
    static_assert(sizeof(Message) == 216 && sizeof(CallbackMsg) == 24 && sizeof(CallCompleted) == 16 &&
                  offsetof(LobbyCreated, lobby) == 8 && sizeof(LobbyEnter) == 24 &&
                  sizeof(LobbyJoinRequested) == 16 && sizeof(PresenceJoinRequested) == 264);
#else
    static_assert(sizeof(Message) == 212 && sizeof(CallbackMsg) == 20 && sizeof(CallCompleted) == 16 &&
                  offsetof(LobbyCreated, lobby) == 4 && sizeof(LobbyEnter) == 20 &&
                  sizeof(LobbyJoinRequested) == 16 && sizeof(PresenceJoinRequested) == 264);
#endif

    using CreateInterfaceFn = void* (*)(const char*, int*);
    using GetCallbackFn = bool (*)(HSteamPipe, CallbackMsg*);
    using FreeLastCallbackFn = void (*)(HSteamPipe);
    using GetCallResultFn = bool (*)(HSteamPipe, SteamAPICall, void*, int, int, bool*);

#ifndef _WIN32
    // The Steam clients running: processes named steam, each with the install
    // it runs from, two folders above the steamclient.so it has mapped. A
    // client of another install than the running one's half connects, then
    // crashes in its first call.
    struct Running
    {
        int pid;
        std::filesystem::path root;
    };

    std::vector<Running> RunningSteams()
    {
        std::vector<Running> found;
        std::error_code ec;
        for (const auto& entry : std::filesystem::directory_iterator("/proc", ec))
        {
            const std::string name = entry.path().filename().string();
            if (name.empty() || name.find_first_not_of("0123456789") != std::string::npos) continue;
            std::ifstream comm(entry.path() / "comm");
            std::string command;
            if (!std::getline(comm, command) || command != "steam") continue;
            std::ifstream maps(entry.path() / "maps");
            for (std::string line; std::getline(maps, line);)
            {
                const size_t slash = line.find('/');
                if (slash == std::string::npos) continue;
                std::string mapped = line.substr(slash);
                constexpr std::string_view kDeleted = " (deleted)";
                if (mapped.ends_with(kDeleted)) mapped.resize(mapped.size() - kDeleted.size());
                const std::filesystem::path library(mapped);
                if (library.filename() != "steamclient.so") continue;
                const auto root = std::filesystem::canonical(library.parent_path().parent_path(), ec);
                if (!ec) found.push_back({ std::stoi(name), root });
                break;
            }
        }
        return found;
    }
#endif

    // Where the player's Steam keeps its client library: MW2_STEAMCLIENT, else
    // the install the running Steam runs from, else the first of the usual
    // install, the snap and the Flatpak. On Windows, the folder the registry
    // says Steam is installed in.
    std::string FindClient()
    {
        if (const char* path = env::Text("MW2_STEAMCLIENT")) return path;
#ifdef _WIN32
        wchar_t folder[MAX_PATH];
        DWORD bytes = sizeof(folder);
        if (RegGetValueW(HKEY_CURRENT_USER, L"Software\\Valve\\Steam", L"SteamPath", RRF_RT_REG_SZ, nullptr,
                         folder, &bytes) != ERROR_SUCCESS)
            return {};
        std::error_code ec;
        const auto path = std::filesystem::path(folder) / "steamclient64.dll";
        return std::filesystem::exists(path, ec) ? path.string() : std::string();
#else
        for (const Running& steam : RunningSteams())
        {
            std::error_code ec;
            const auto path = steam.root / "linux64" / "steamclient.so";
            if (std::filesystem::exists(path, ec)) return path.string();
        }
        const char* home = env::Text("HOME");
        if (!home) return {};
        const char* candidates[] = {
            "/.steam/sdk64/steamclient.so",
            "/.local/share/Steam/linux64/steamclient.so",
            "/.steam/steam/linux64/steamclient.so",
            "/snap/steam/common/.local/share/Steam/linux64/steamclient.so",
            "/.var/app/com.valvesoftware.Steam/.local/share/Steam/linux64/steamclient.so",
        };
        for (const char* candidate : candidates)
        {
            std::error_code ec;
            const std::string path = std::string(home) + candidate;
            if (std::filesystem::exists(path, ec)) return path;
        }
        return {};
#endif
    }

    // The SDL_ variables in the environment, to put back as they were.
    struct SdlSettings
    {
        std::vector<std::pair<std::string, std::string>> values;

        static SdlSettings Now()
        {
            SdlSettings settings;
            auto keep = [&](const std::string& text) {
                const size_t equals = text.find('=');
                if (text.rfind("SDL_", 0) == 0 && equals != std::string::npos)
                    settings.values.emplace_back(text.substr(0, equals), text.substr(equals + 1));
            };
#ifdef _WIN32
            // The process's own block, not `environ`: the Steam client sets its
            // variables with SetEnvironmentVariable, which the C library's
            // copy never hears of -- and SDL reads this one.
            if (char* block = GetEnvironmentStringsA())
            {
                for (const char* entry = block; *entry; entry += std::strlen(entry) + 1) keep(entry);
                FreeEnvironmentStringsA(block);
            }
#else
            for (char** entry = environ; *entry; entry++) keep(*entry);
#endif
            return settings;
        }

        void Restore() const
        {
            for (const auto& [name, value] : Now().values)
            {
                const auto was = std::find_if(values.begin(), values.end(),
                                              [&](const auto& v) { return v.first == name; });
                if (was == values.end())
                {
                    LOGI("online: the Steam client set %s; this was not started by Steam, so it"
                         " is taken back", name.c_str());
                    SetEnvironment(name.c_str(), nullptr);
                }
                else if (was->second != value)
                    SetEnvironment(name.c_str(), was->second.c_str());
            }
        }
    };

    class Steam final : public online::Service
    {
    public:
        ~Steam() override
        {
            stopping_ = true;
            if (thread_.joinable()) thread_.join();
            if (lobby_) Call<void>(matchmaking_, matchmaking::LeaveLobby, lobby_.load());
            if (listing_) Call<void>(matchmaking_, matchmaking::LeaveLobby, listing_);
            if (client_ && pipe_)
            {
                if (user_) Call<void>(client_, client::ReleaseUser, pipe_, user_);
                Call<bool>(client_, client::ReleaseSteamPipe, pipe_);
            }
        }

        bool Start() override
        {
            const std::string path = FindClient();
            if (path.empty()) { LOGE("online: no Steam install found (MW2_STEAMCLIENT names one)"); return false; }
            // The client reads which game this is from the environment, as it does
            // for a game libsteam_api started, and reads it when it is loaded: set
            // later, its networking asserts and has no interface to give.
            const std::string app = env::Text("MW2_STEAM_APPID") ? env::Text("MW2_STEAM_APPID") : "480";
            SetEnvironment("SteamAppId", app.c_str());
            SetEnvironment("SteamGameId", app.c_str());
            const SdlSettings launched = SdlSettings::Now();
            library_ = platform::OpenLibrary(path.c_str());
            if (!library_) { LOGE("online: could not load %s", path.c_str()); return false; }
            auto create = reinterpret_cast<CreateInterfaceFn>(platform::Symbol(library_, "CreateInterface"));
            getCallback_ = reinterpret_cast<GetCallbackFn>(platform::Symbol(library_, "Steam_BGetCallback"));
            freeLastCallback_ = reinterpret_cast<FreeLastCallbackFn>(platform::Symbol(library_, "Steam_FreeLastCallback"));
            getCallResult_ = reinterpret_cast<GetCallResultFn>(platform::Symbol(library_, "Steam_GetAPICallResult"));
            if (!create || !getCallback_ || !freeLastCallback_ || !getCallResult_)
            {
                LOGE("online: %s lacks the exports a client has", path.c_str());
                return false;
            }

            int result = 1;
            client_ = create("SteamClient023", &result);
            if (!client_) { LOGE("online: the Steam client refused its interface"); return false; }
            pipe_ = Call<HSteamPipe>(client_, client::CreateSteamPipe);
            user_ = pipe_ ? Call<HSteamUser>(client_, client::ConnectToGlobalUser, pipe_) : 0;
            if (!user_) { LOGE("online: Steam is not running, or nobody is logged in"); return false; }

            steamUser_ = Call<void*>(client_, client::GetUser, user_, pipe_, "SteamUser023");
            friends_ = Call<void*>(client_, client::GetFriends, user_, pipe_, "SteamFriends018");
            utils_ = Call<void*>(client_, client::GetUtils, pipe_, "SteamUtils011");
            matchmaking_ = Call<void*>(client_, client::GetMatchmaking, user_, pipe_, "SteamMatchMaking009");
            messages_ = Call<void*>(client_, client::GetGeneric, user_, pipe_, "SteamNetworkingMessages002");
            if (!steamUser_ || !friends_ || !utils_ || !matchmaking_ || !messages_)
            {
                LOGE("online: the Steam client lacks an interface this needs (user %p, friends %p, utils %p,"
                     " matchmaking %p, networking messages %p)", steamUser_, friends_, utils_, matchmaking_,
                     messages_);
                return false;
            }
            // The client tells SDL to pass over every physical controller, as it
            // does in a game Steam started -- where Steam Input stands a virtual
            // one in for them. Nothing does here unless Steam started this, and
            // then it said so before the client loaded; what the client adds on
            // top would leave the title with no controller at all.
            launched.Restore();

            app_ = std::strtoull(app.c_str(), nullptr, 10);
            self_ = CallSteamId(steamUser_, user::GetSteamID);
            name_ = Call<const char*>(friends_, friends::GetPersonaName);
            LOGI("online: steam as \"%s\" (%016llx), app %s%s", name_.c_str(),
                 (unsigned long long)self_, app.c_str(),
                 Call<bool>(steamUser_, user::LoggedOn) ? "" : ", offline");

            // What tells this game's players from Spacewar's in a friend list:
            // Steam shares rich presence among friends running the same app.
            Call<bool>(friends_, friends::SetRichPresence, kListingKey, Playing().c_str());
            createCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::CreateLobby, kFriendsOnly, kMaxMembers);
            thread_ = std::thread([this] { Run(); });
            return true;
        }

        uint64_t LocalId() override { return self_; }
        // The profile at the first controller; until there were profiles it
        // was the Steam account, and the rank is still under that.
        uint64_t Account() override { return signin::First().id; }
        std::string LocalName() override { return name_; }

        // A listed host this player sends to on its own -- the host of a game
        // it joins -- may answer anywhere. A stranger who came to this
        // player's game is answered, but stays a stranger.
        bool Send(uint64_t peer, uint16_t fromPort, uint16_t toPort,
                  const void* bytes, size_t size) override
        {
            {
                std::lock_guard g(lock_);
                if (searchedHosts_.contains(peer) && !strangers_.contains(peer)) contacted_.insert(peer);
            }
            return SendTo(peer, fromPort, toPort, bytes, size);
        }

        bool SendTo(uint64_t peer, uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size)
        {
            // The sender's port ahead of the title's bytes; the channel is the port
            // it is for.
            thread_local std::vector<uint8_t> packet;
            packet.resize(2 + size);
            packet[0] = uint8_t(fromPort >> 8);
            packet[1] = uint8_t(fromPort);
            std::memcpy(packet.data() + 2, bytes, size);
            const Identity to = SteamIdentity(peer);
            return Call<int>(messages_, messages::Send, &to, packet.data(), uint32_t(packet.size()),
                             kSendFlags, int(toPort)) == kResultOK;
        }

        // To the friends' lobby and, from a host, to the strangers it talks to
        // or, from a player who hosts nothing, to every host listed. The title
        // broadcasts only to look for system-link games and to answer such a
        // search.
        bool Broadcast(uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size) override
        {
            std::vector<uint64_t> to;
            {
                std::lock_guard g(lock_);
                if (advert_.empty())
                {
                    SearchLocked();
                    to = hosts_;
                    searchedHosts_.insert(hosts_.begin(), hosts_.end());
                    if (!hosts_.empty()) searchPorts_.insert(toPort);
                }
                else
                {
                    std::erase_if(strangers_, [&](uint64_t stranger) { return !SessionOpen(stranger); });
                    to.insert(to.end(), strangers_.begin(), strangers_.end());
                }
            }
            if (const uint64_t lobby = lobby_)
            {
                const int count = Call<int>(matchmaking_, matchmaking::MemberCount, lobby);
                for (int i = 0; i < count; i++) to.push_back(CallSteamId(matchmaking_, matchmaking::Member, lobby, i));
            }
            std::sort(to.begin(), to.end());
            to.erase(std::unique(to.begin(), to.end()), to.end());
            for (const uint64_t peer : to)
                if (peer != self_) SendTo(peer, fromPort, toPort, bytes, size);
            return true;
        }

        bool Receive(uint16_t port, online::Datagram& out) override
        {
            for (;;)
            {
                Message* message = nullptr;
                if (Call<int>(messages_, messages::Receive, int(port), &message, 1) < 1 || !message)
                    return false;
                const auto* data = static_cast<const uint8_t*>(message->data);
                const bool whole = message->size >= 2 && message->peer.type == 16 &&
                                   Reaches(message->peer.steamId, port);
                if (whole)
                {
                    out.from = message->peer.steamId;
                    out.fromPort = uint16_t((data[0] << 8) | data[1]);
                    out.toPort = port;
                    out.bytes.assign(data + 2, data + message->size);
                }
                message->release(message);
                if (whole) return true;
            }
        }

        void Advertise(const std::string& connect) override
        {
            std::lock_guard g(lock_);
            advert_ = connect;
            PublishLocked();
            ListLocked();
        }

        bool NextJoin(std::string& connect) override
        {
            std::lock_guard g(lock_);
            if (joins_.empty()) return false;
            connect = std::move(joins_.front());
            joins_.pop_front();
            return true;
        }

        bool Lobbies() override { return true; }

        void SetJoinable(const online::Joinable* joinable) override
        {
            std::lock_guard g(lock_);
            session_.clear();
            if (joinable)
            {
                char pair[3];
                for (uint8_t byte : joinable->session)
                {
                    std::snprintf(pair, sizeof pair, "%02x", byte);
                    session_ += pair;
                }
            }
            PublishLocked();
        }

        bool NextInvite(online::Invite& out) override
        {
            std::lock_guard g(lock_);
            if (invites_.empty()) return false;
            out = invites_.front();
            invites_.pop_front();
            return true;
        }

        bool Friends(online::FriendList& list) override
        {
            friendsWanted_ = std::chrono::steady_clock::now().time_since_epoch().count();
            std::lock_guard g(friendsLock_);
            if (list.version != friendList_.version) list = friendList_;
            return true;
        }

        // To this player's lobby, which carries their party or game: Steam's
        // own invitation, shown in the friend's chat, which their copy hears
        // of when they accept.
        bool InviteFriend(uint64_t id) override
        {
            const uint64_t lobby = lobby_;
            if (!lobby) { LOGW("online: no lobby yet to invite anyone to"); return false; }
            const bool sent = Call<bool>(matchmaking_, matchmaking::InviteToLobby, lobby, id);
            LOGI("online: invites %016llx%s", (unsigned long long)id, sent ? "" : ", which Steam refused");
            return sent;
        }

    private:
        // Callbacks, read off the pipe the way libsteam_api's manual dispatch
        // does. Between frames is soon enough for anything here.
        void Run()
        {
            while (!stopping_)
            {
                CallbackMsg message;
                while (getCallback_(pipe_, &message))
                {
                    Handle(message);
                    freeLastCallback_(pipe_);
                }
                const auto wanted = std::chrono::steady_clock::time_point(
                    std::chrono::steady_clock::duration(friendsWanted_.load()));
                if (const auto now = std::chrono::steady_clock::now();
                    now - wanted < kFriendsWatched && now - friendsBuilt_ >= kFriendsEvery)
                {
                    friendsBuilt_ = now;
                    BuildFriends();
                }
                if (const auto now = std::chrono::steady_clock::now(); now - listAsked_ >= kListAgainEvery)
                {
                    listAsked_ = now;
                    std::lock_guard g(lock_);
                    if (!listing_) ListLocked();
                }
                if (const auto now = std::chrono::steady_clock::now(); now - linksWatched_ >= kLinksEvery)
                {
                    linksWatched_ = now;
                    WatchLinks(now);
                }
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            }
        }

        void Handle(const CallbackMsg& message)
        {
            switch (message.id)
            {
            case kCallCompleted:
            {
                const auto& done = *reinterpret_cast<const CallCompleted*>(message.param);
                bool failed = false;
                if (done.call == createCall_)
                {
                    LobbyCreated created{};
                    if (getCallResult_(pipe_, done.call, &created, sizeof created, kLobbyCreated, &failed) &&
                        !failed && created.result == kResultOK)
                        Entered(created.lobby);
                    else
                        LOGW("online: Steam would not make a lobby (result %d)", created.result);
                }
                else if (done.call == joinCall_)
                {
                    LobbyEnter entered{};
                    if (getCallResult_(pipe_, done.call, &entered, sizeof entered, kLobbyEnter, &failed) &&
                        !failed && entered.response == kEnterSuccess)
                        Entered(entered.lobby);
                    else
                        LOGW("online: could not enter the friend's lobby (response %u)", entered.response);
                }
                else
                {
                    // Asked for on the title's thread, under the lock, so a call
                    // is known here by the time it completes.
                    std::lock_guard g(lock_);
                    if (done.call == listingCall_) Listed(done.call);
                    else if (done.call == searchCall_) Found(done.call);
                }
                break;
            }
            case kLobbyJoinRequested:
                Join(reinterpret_cast<const LobbyJoinRequested*>(message.param)->lobby);
                break;
            case kPresenceJoinRequested:
            {
                // "+connect_lobby <id>", as the rich presence below puts it.
                unsigned long long lobby = 0;
                const auto* request = reinterpret_cast<const PresenceJoinRequested*>(message.param);
                if (std::sscanf(request->connect, "+connect_lobby %llu", &lobby) == 1) Join(lobby);
                break;
            }
            case kLobbyDataUpdate:
            {
                const auto& update = *reinterpret_cast<const LobbyDataUpdate*>(message.param);
                if (update.lobby == lobby_ && update.member == update.lobby) Follow();
                break;
            }
            case kSessionRequest:
            {
                // From someone in this player's lobby, or from anyone while this
                // player hosts a system-link game: Spacewar is everyone's. A
                // session this player opened, to a host it found, needs no
                // request.
                const auto& request = *reinterpret_cast<const SessionRequest*>(message.param);
                if (request.remote.type != 16) break;
                const uint64_t who = request.remote.steamId;
                const bool member = InLobby(who);
                bool take = member;
                if (!take)
                {
                    std::lock_guard g(lock_);
                    take = !advert_.empty();
                    if (take) strangers_.insert(who);
                }
                if (take) Call<bool>(messages_, messages::Accept, &request.remote);
                LOGI("online: %s asks for a link: %s", Who(who).c_str(),
                     member ? "in the lobby, taken" : take ? "taken, as this player hosts a game"
                                                           : "refused, being neither in the lobby nor at a game hosted here");
                break;
            }
            default:
                break;
            }
        }

        void Join(uint64_t lobby)
        {
            if (!lobby) return;
            if (lobby == lobby_)
            {
                // Already in it, with the host's party or game followed once:
                // since then the host may have started another, or this player
                // left it. Taking the invitation again follows the host again.
                LOGI("online: invited again to lobby %016llx; following its owner", (unsigned long long)lobby);
                {
                    std::lock_guard g(lock_);
                    followed_ = false;
                }
                Follow();
                return;
            }
            LOGI("online: invited; entering lobby %016llx", (unsigned long long)lobby);
            if (const uint64_t old = lobby_.exchange(0)) Call<void>(matchmaking_, matchmaking::LeaveLobby, old);
            joinCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::JoinLobby, lobby);
        }

        void Entered(uint64_t lobby)
        {
            const uint64_t owner = CallSteamId(matchmaking_, matchmaking::Owner, lobby);
            const bool mine = owner == self_;
            {
                std::lock_guard g(lock_);
                lobby_ = lobby;
                owner_ = mine;
                followed_ = false;
                PublishLocked();
            }
            // Friends can join from their list, and an invitation that starts the
            // game says which lobby.
            char connect[64];
            std::snprintf(connect, sizeof connect, "+connect_lobby %llu", (unsigned long long)lobby);
            Call<bool>(friends_, friends::SetRichPresence, "connect", connect);
            LOGI("online: in lobby %016llx%s", (unsigned long long)lobby, mine ? ", its owner" : "");
            Follow();
        }

        // The owner's game, once per lobby entered: a friend who accepted an
        // invitation lands in it, and is not pulled back in each time the host
        // starts another -- only when invited again (Join).
        void Follow()
        {
            std::lock_guard g(lock_);
            if (owner_ || followed_ || !lobby_) return;
            online::Invite invite;
            if (ReadSession(Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby_.load(), "xsession"),
                            invite.session))
            {
                // The host's account, which is his profile and not his Steam
                // id; a host of a version before profiles says none, and his
                // was the Steam id.
                const char* account = Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby_.load(), "account");
                invite.inviter = account && *account ? std::strtoull(account, nullptr, 16)
                                                     : CallSteamId(matchmaking_, matchmaking::Owner, lobby_.load());
                invite.fromInvite = true;
                invites_.push_back(invite);
                followed_ = true;
                return;
            }
            const char* connect = Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby_.load(), "connect");
            if (!connect || !*connect) return;
            joins_.push_back(connect);
            followed_ = true;
        }

        static bool ReadSession(const char* hex, uint8_t* session)
        {
            if (!hex || std::strlen(hex) != 2 * online::kSessionInfoSize) return false;
            for (size_t i = 0; i < online::kSessionInfoSize; i++)
            {
                unsigned byte;
                if (std::sscanf(hex + 2 * i, "%2x", &byte) != 1) return false;
                session[i] = uint8_t(byte);
            }
            return true;
        }

        void PublishLocked()
        {
            if (!owner_ || !lobby_) return;
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, lobby_.load(), "connect", advert_.c_str());
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, lobby_.load(), "xsession", session_.c_str());
            char account[20];
            std::snprintf(account, sizeof account, "%012llx", (unsigned long long)Account());
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, lobby_.load(), "account", account);
        }

        // The public list. A listing is made the first time this player hosts
        // a system-link game and kept for the run, searchable only while it
        // hosts one.
        void ListLocked()
        {
            const bool hosting = !advert_.empty();
            if (listing_)
            {
                if (Call<bool>(matchmaking_, matchmaking::SetJoinable, listing_, hosting) && hosting != listed_)
                    LOGI("online: %s", hosting ? "this game is listed for every Steam player"
                                               : "this game is no longer listed");
                listed_ = hosting;
            }
            else if (hosting && !listingCall_)
                listingCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::CreateLobby, kInvisible, kListingMembers);
        }

        void Listed(SteamAPICall call)
        {
            listingCall_ = 0;
            LobbyCreated created{};
            bool failed = false;
            if (!getCallResult_(pipe_, call, &created, sizeof created, kLobbyCreated, &failed) || failed ||
                created.result != kResultOK)
            {
                LOGW("online: Steam would not list this game (result %d)", created.result);
                return;
            }
            listing_ = created.lobby;
            const std::string host = std::to_string(self_);
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, listing_, kListingKey, ListingVersion().c_str());
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, listing_, "host", host.c_str());
            ListLocked();
        }

        // Asks Steam for the hosts listed, unless it is already asked or was
        // asked a moment ago.
        void SearchLocked()
        {
            const auto now = std::chrono::steady_clock::now();
            if (searchCall_ || now - searched_ < kSearchEvery) return;
            searched_ = now;
            // The filters hold for the next request only.
            Call<void>(matchmaking_, matchmaking::AddStringFilter, kListingKey, ListingVersion().c_str(), kEqual);
            Call<void>(matchmaking_, matchmaking::AddDistanceFilter, kWorldwide);
            searchCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::RequestLobbyList);
        }

        void Found(SteamAPICall call)
        {
            searchCall_ = 0;
            LobbyMatchList list{};
            bool failed = false;
            if (!getCallResult_(pipe_, call, &list, sizeof list, kLobbyMatchList, &failed) || failed) return;
            std::vector<uint64_t> found;
            for (uint32_t i = 0; i < list.matching; i++)
            {
                const uint64_t lobby = CallSteamId(matchmaking_, matchmaking::LobbyByIndex, int(i));
                const char* host = Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby, "host");
                const uint64_t id = host ? std::strtoull(host, nullptr, 10) : 0;
                if (id && id != self_) found.push_back(id);
            }
            if (found != hosts_) LOGI("online: %zu system-link game(s) listed on Steam", found.size());
            hosts_ = std::move(found);
        }

        // The friends running this game, by the key Start sets, and how many
        // others there are. Steam has all of it at hand: nothing here waits on
        // the network.
        void BuildFriends()
        {
            online::FriendList list;
            const uint64_t lobby = lobby_;
            const int count = Call<int>(friends_, friends::Count, kImmediate);
            for (int i = 0; i < count; i++)
            {
                const uint64_t id = CallSteamId(friends_, friends::ByIndex, i, kImmediate);
                FriendGameInfo game{};
                // Copied at once: a string the client returns may be held in
                // a buffer its next call reuses.
                std::string marker;
                if (Call<bool>(friends_, friends::GamePlayed, id, &game) && game.game == app_)
                    if (const char* value = Call<const char*>(friends_, friends::RichPresence, id, kListingKey))
                        marker = value;
                if (marker.empty())
                {
                    (Call<int>(friends_, friends::PersonaState, id) == kOffline ? list.offline : list.online)++;
                    continue;
                }
                online::Friend& one = list.playing.emplace_back();
                one.id = id;
                const char* name = Call<const char*>(friends_, friends::Name, id);
                one.name = name ? name : "";
                one.withYou = lobby && game.lobby == lobby;
                one.elsewhere = Elsewhere(marker);
                const Avatar& avatar = AvatarOf(id);
                one.avatar = avatar.rgba;
                one.avatarSide = avatar.side;
            }
            auto lower = [](std::string text) {
                for (char& c : text) c = char(std::tolower(uint8_t(c)));
                return text;
            };
            std::stable_sort(list.playing.begin(), list.playing.end(),
                             [&](const online::Friend& a, const online::Friend& b) { return lower(a.name) < lower(b.name); });

            auto same = [](const online::FriendList& a, const online::FriendList& b) {
                return a.online == b.online && a.offline == b.offline &&
                       std::equal(a.playing.begin(), a.playing.end(), b.playing.begin(), b.playing.end(),
                                  [](const online::Friend& x, const online::Friend& y) {
                                      return x.id == y.id && x.name == y.name && x.withYou == y.withYou &&
                                             x.elsewhere == y.elsewhere && x.avatar == y.avatar;
                                  });
            };
            std::lock_guard g(friendsLock_);
            if (friendList_.version && same(list, friendList_)) return;
            list.version = friendList_.version + 1;
            friendList_ = std::move(list);
        }

        // A friend's 64-pixel picture, read again only when Steam has another.
        // Steam answers -1 while it is still fetching one.
        struct Avatar { int image = 0; std::vector<uint8_t> rgba; uint32_t side = 0; };
        const Avatar& AvatarOf(uint64_t id)
        {
            Avatar& avatar = avatars_[id];
            const int image = Call<int>(friends_, friends::MediumAvatar, id);
            if (image <= 0 || image == avatar.image) return avatar;
            uint32_t width = 0, height = 0;
            if (!Call<bool>(utils_, utils::ImageSize, image, &width, &height) || width != height || !width ||
                width > 256)
                return avatar;
            std::vector<uint8_t> rgba(size_t(width) * height * 4);
            if (!Call<bool>(utils_, utils::ImageRGBA, image, rgba.data(), int(rgba.size()))) return avatar;
            avatar = { image, std::move(rgba), width };
            return avatar;
        }

        bool SessionOpen(uint64_t peer)
        {
            const Identity identity = SteamIdentity(peer);
            const int state = Call<int>(messages_, messages::SessionInfo, &identity, nullptr, nullptr);
            return state >= 1 && state <= 3;   // connecting, finding a route, connected
        }

        // Whether a datagram from this peer reaches the title. Steam opens a
        // session only to someone this player accepted -- a lobby member, or
        // a stranger while hosting -- or sent to first.
        bool Reaches(uint64_t peer, uint16_t port)
        {
            {
                std::lock_guard g(lock_);
                if (contacted_.contains(peer)) return true;
                const bool stranger = strangers_.contains(peer), listed = searchedHosts_.contains(peer);
                if (!stranger && !listed) return true;   // a lobby member, or one that was
                if (stranger && !advert_.empty()) return true;
                if (listed && searchPorts_.contains(port)) return true;
            }
            return InLobby(peer);
        }

        bool InLobby(uint64_t player)
        {
            const uint64_t lobby = lobby_;
            if (!lobby) return false;
            const int count = Call<int>(matchmaking_, matchmaking::MemberCount, lobby);
            for (int i = 0; i < count; i++)
                if (CallSteamId(matchmaking_, matchmaking::Member, lobby, i) == player) return true;
            return false;
        }

        std::string Who(uint64_t id)
        {
            const char* name = Call<const char*>(friends_, friends::Name, id);
            char text[160];
            std::snprintf(text, sizeof text, "%s (%016llx)", name && *name ? name : "?", (unsigned long long)id);
            return text;
        }

        // The links to everyone this player talks to through Steam: the
        // lobby's, the strangers at its game, the hosts it went to. The log
        // says when one connects, when one goes bad and why one ends.
        void WatchLinks(std::chrono::steady_clock::time_point now)
        {
            std::set<uint64_t> peers;
            {
                std::lock_guard g(lock_);
                peers.insert(strangers_.begin(), strangers_.end());
                peers.insert(contacted_.begin(), contacted_.end());
            }
            if (const uint64_t lobby = lobby_)
            {
                const int count = Call<int>(matchmaking_, matchmaking::MemberCount, lobby);
                for (int i = 0; i < count; i++) peers.insert(CallSteamId(matchmaking_, matchmaking::Member, lobby, i));
            }
            for (const auto& [peer, link] : links_) peers.insert(peer);
            peers.erase(self_);
            for (const uint64_t peer : peers)
            {
                const Identity identity = SteamIdentity(peer);
                LinkInfo info{};
                LinkStatus status{};
                const int state = Call<int>(messages_, messages::SessionInfo, &identity, &info, &status);
                Link& link = links_[peer];
                info.endDebug[sizeof info.endDebug - 1] = 0;
                if (state != link.state)
                {
                    if (state == kConnected)
                        LOGI("online: link to %s: connected, %d ms, %.0f%% of packets arrive", Who(peer).c_str(),
                             status.ping, 100.0 * status.qualityLocal);
                    else if (state == kClosedByPeer || state == kProblem)
                        LOGI("online: link to %s: %s (reason %d: %s)", Who(peer).c_str(),
                             state == kClosedByPeer ? "closed by them" : "lost", info.endReason, info.endDebug);
                    else
                        LOGI("online: link to %s: %s", Who(peer).c_str(),
                             state == 1 ? "connecting" : state == 2 ? "finding a route" : "none");
                    link.state = state;
                }
                else if (state == kConnected && now - link.told >= kPoorLinkEvery &&
                         (status.qualityLocal < 0.9f || status.qualityRemote < 0.9f ||
                          status.queueMicroseconds >= 100000))
                {
                    link.told = now;
                    LOGI("online: link to %s is poor: %d ms, %.0f%% of packets arrive here and %.0f%% there, "
                         "%.0f ms queued to send, %.1f KB/s out of %.1f allowed, %.1f KB/s in", Who(peer).c_str(),
                         status.ping, 100.0 * status.qualityLocal, 100.0 * status.qualityRemote,
                         double(status.queueMicroseconds) / 1000.0, status.outBytes / 1024.0,
                         status.sendRate / 1024.0, status.inBytes / 1024.0);
                }
            }
            std::erase_if(links_, [](const auto& entry) { return entry.second.state <= 0; });
        }

        void* library_ = nullptr;
        void* client_ = nullptr;
        void* steamUser_ = nullptr;
        void* friends_ = nullptr;
        void* utils_ = nullptr;
        void* matchmaking_ = nullptr;
        void* messages_ = nullptr;
        GetCallbackFn getCallback_ = nullptr;
        FreeLastCallbackFn freeLastCallback_ = nullptr;
        GetCallResultFn getCallResult_ = nullptr;
        HSteamPipe pipe_ = 0;
        HSteamUser user_ = 0;
        uint64_t self_ = 0;
        uint64_t app_ = 0;
        std::string name_;

        std::thread thread_;
        std::atomic<bool> stopping_{ false };
        std::atomic<SteamAPICall> createCall_{ 0 }, joinCall_{ 0 };
        std::atomic<uint64_t> lobby_{ 0 };

        std::mutex lock_;
        bool owner_ = false, followed_ = false;
        std::string advert_;
        std::string session_;   // what joins this player's Live party or lobby, in hex
        uint64_t listing_ = 0;  // this player's entry in the public list
        bool listed_ = false;   // and whether it shows
        SteamAPICall listingCall_ = 0, searchCall_ = 0;
        std::chrono::steady_clock::time_point searched_{};
        std::vector<uint64_t> hosts_;           // the hosts listed when Steam was last asked
        std::set<uint64_t> searchedHosts_;      // every listed host a search went to
        std::set<uint16_t> searchPorts_;        // and the ports it went to
        std::set<uint64_t> strangers_;          // not in the lobby, whose sessions this player accepted hosting
        std::set<uint64_t> contacted_;          // listed hosts this player sent to on its own
        std::chrono::steady_clock::time_point listAsked_{};   // the callback thread's
        // The links WatchLinks follows, on the callback thread.
        struct Link
        {
            int state = 0;
            std::chrono::steady_clock::time_point told{};   // when it was last said to be poor
        };
        std::map<uint64_t, Link> links_;
        std::chrono::steady_clock::time_point linksWatched_{};
        std::deque<std::string> joins_;
        std::deque<online::Invite> invites_;

        // The friends screen's list, built on the callback thread.
        std::mutex friendsLock_;
        online::FriendList friendList_;
        std::atomic<std::chrono::steady_clock::rep> friendsWanted_{ 0 };   // when it was last asked for
        std::chrono::steady_clock::time_point friendsBuilt_{};
        std::map<uint64_t, Avatar> avatars_;
    };
}

std::unique_ptr<online::Service> online::MakeSteam() { return std::make_unique<Steam>(); }

#ifdef _WIN32
bool online::EnterSteamNamespace(int& exitCode)
{
    (void)exitCode;
    return true;
}
#else
namespace
{
    pid_t g_game = 0;

    // The parent hands a request to end on to the game. An interrupt from the
    // terminal reaches the game itself, as it reaches every process of the
    // terminal's job.
    void Forward(int signal) { if (g_game > 0) kill(g_game, signal); }

    // The Flatpak's namespaces are owned by a user namespace the player's own
    // uid made, so joining needs no privilege. Both at once through a pidfd
    // (Linux 5.8), which some kernels require; else the user namespace, which
    // grants the right to join the process one, then that.
    bool Join(int pid)
    {
        const int pidfd = int(syscall(SYS_pidfd_open, pid, 0));
        if (pidfd >= 0)
        {
            const bool joined = setns(pidfd, CLONE_NEWUSER | CLONE_NEWPID) == 0;
            close(pidfd);
            if (joined) return true;
        }
        const std::string at = "/proc/" + std::to_string(pid) + "/ns/";
        const int user = open((at + "user").c_str(), O_RDONLY | O_CLOEXEC);
        const int process = open((at + "pid").c_str(), O_RDONLY | O_CLOEXEC);
        const bool joined = user >= 0 && process >= 0 && setns(user, CLONE_NEWUSER) == 0 &&
                            setns(process, CLONE_NEWPID) == 0;
        if (user >= 0) close(user);
        if (process >= 0) close(process);
        return joined;
    }
}

bool online::EnterSteamNamespace(int& exitCode)
{
    std::error_code ec;
    const auto own = std::filesystem::read_symlink("/proc/self/ns/pid", ec);
    if (ec) return true;
    for (const Running& steam : RunningSteams())
    {
        const auto its = std::filesystem::read_symlink("/proc/" + std::to_string(steam.pid) + "/ns/pid", ec);
        if (ec || its == own) continue;
        if (!Join(steam.pid))
        {
            LOGW("online: could not join the namespace of Steam %d (%s), which runs from %s: it may drop "
                 "the game's connection",
                 steam.pid, std::strerror(errno), steam.root.string().c_str());
            return true;
        }
        // A process namespace is entered by the children made after joining it.
        std::fflush(nullptr);
        const pid_t child = fork();
        if (child < 0)
        {
            LOGW("online: could not start the game in Steam's namespace (%s)", std::strerror(errno));
            return true;
        }
        if (child == 0)
        {
            // Ends with the parent, however that ends.
            prctl(PR_SET_PDEATHSIG, SIGKILL);
            LOGI("online: in the namespace of Steam %d, which runs from %s", steam.pid, steam.root.string().c_str());
            return true;
        }
        g_game = child;
        std::signal(SIGINT, SIG_IGN);
        std::signal(SIGTERM, Forward);
        std::signal(SIGHUP, Forward);
        int status = 0;
        while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
        exitCode = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
        return false;
    }
    return true;
}
#endif
