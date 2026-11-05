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
//   - a player who chose to join the stranger's game, as on a local network;
//   - a player listed for Find Game or in a Find Game game: anyone, since any of
//     a game's players can be its next host.
// Everything else from a stranger is dropped as it arrives, so a player who
// stops hosting is closed to the strangers it played with. Their sessions are
// not closed, since a host's map change ends its game and starts the next one
// at once. Xbox LIVE parties and private matches stay among friends; Find
// Game is shared as System Link is, with its own listings.
#include "service.h"
#include "../env.h"
#include "../log.h"
#include "../signin.h"
#include "../version.h"
#include "playlists_data.h"

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
                      ResultCount = 10, LobbyByIndex = 12, InviteToLobby = 16, SetJoinable = 34, SetOwner = 36;
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
    // The games Find Game can join: lobbies of their own with this key, again
    // with the release's value, whose "host" is the SteamID hosting and whose
    // "game" is what it lists, in one value so that a search reads a whole one.
    constexpr const char* kGameKey = "mw2game";
    constexpr int kGamesAsked = 200;
    // A search opens a link to each host it found, on a port no title uses, and
    // takes the link's ping as the round trip. A host with no link by then
    // cannot be reached, and is left out.
    constexpr uint16_t kProbePort = 65535;
    constexpr auto kProbeWithin = std::chrono::seconds(4);
    // A list Steam has not given by then is taken as empty, as a list that never
    // comes would hold up every later one.
    constexpr auto kListWithin = std::chrono::seconds(10);
    // What a listing may say. It is anyone's words, read under the lock the
    // title's sends take.
    constexpr size_t kGameMostText = 1024;
    constexpr auto kHuntsEvery = std::chrono::milliseconds(100);
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
    // crashes in its first call. MW2_STEAM_ROOT keeps the one running from it.
    struct Running
    {
        int pid;
        std::filesystem::path root;
    };

    std::vector<Running> RunningSteams()
    {
        std::vector<Running> found;
        std::error_code ec;
        const char* named = env::Text("MW2_STEAM_ROOT");
        const auto wanted = named ? std::filesystem::canonical(named, ec) : std::filesystem::path();
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
                if (!ec && (!named || root == wanted)) found.push_back({ std::stoi(name), root });
                break;
            }
        }
        return found;
    }
#endif

    // Where the player's Steam keeps its client library: the install the
    // running Steam runs from, else the first of the usual install, the snap
    // and the Flatpak. On Windows, the folder the registry says Steam is
    // installed in. MW2_STEAM_ROOT names the install instead.
    std::string FindClient()
    {
#ifdef _WIN32
        std::filesystem::path root;
        if (const char* named = env::Text("MW2_STEAM_ROOT")) root = named;
        else
        {
            wchar_t folder[MAX_PATH];
            DWORD bytes = sizeof(folder);
            if (RegGetValueW(HKEY_CURRENT_USER, L"Software\\Valve\\Steam", L"SteamPath", RRF_RT_REG_SZ, nullptr,
                             folder, &bytes) != ERROR_SUCCESS)
                return {};
            root = folder;
        }
        std::error_code ec;
        const auto path = root / "steamclient64.dll";
        if (!std::filesystem::exists(path, ec)) return {};
        const auto utf8 = path.u8string();   // what platform::OpenLibrary reads
        return std::string(utf8.begin(), utf8.end());
#else
        for (const Running& steam : RunningSteams())
        {
            std::error_code ec;
            const auto path = steam.root / "linux64" / "steamclient.so";
            if (std::filesystem::exists(path, ec)) return path.string();
        }
        if (const char* named = env::Text("MW2_STEAM_ROOT"))
        {
            LOGE("online: no Steam is running from %s", named);
            return {};
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

    class Steam final : public online::Backend
    {
    public:
        ~Steam() override
        {
            stopping_ = true;
            if (thread_.joinable()) thread_.join();
            if (lobby_) Call<void>(matchmaking_, matchmaking::LeaveLobby, lobby_.load());
            if (listing_) Call<void>(matchmaking_, matchmaking::LeaveLobby, listing_);
            if (gameListing_) Call<void>(matchmaking_, matchmaking::LeaveLobby, gameListing_);
            if (client_ && pipe_)
            {
                if (user_) Call<void>(client_, client::ReleaseUser, pipe_, user_);
                Call<bool>(client_, client::ReleaseSteamPipe, pipe_);
            }
        }

        bool Start() override
        {
            const std::string path = FindClient();
            if (path.empty()) { LOGE("online: no Steam install found (MW2_STEAM_ROOT names one)"); return false; }
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
                if ((searchedHosts_.contains(peer) || foundHosts_.contains(peer)) && !strangers_.contains(peer))
                    contacted_.insert(peer);
                // Sending opens the session with no request to take: a game's
                // other players are remembered here, to be closed to afterwards.
                if (playing_) strangers_.insert(peer);
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
                    // The set stays as it is: it is how this player closes to them
                    // afterwards, and a link that dropped can come back.
                    for (const uint64_t stranger : strangers_)
                        if (SessionOpen(stranger)) to.push_back(stranger);
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
                const bool from = message->size >= 2 && message->peer.type == 16;
                const bool whole = from && Reaches(message->peer.steamId, port);
                if (from && !whole && port != kProbePort)
                {
                    std::lock_guard g(lock_);
                    if (droppedFrom_.insert(message->peer.steamId).second)
                        LOGI("online: dropping what %016llx sends to port %u", (unsigned long long)message->peer.steamId, port);
                }
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

        // The lobby carries its owner's game, so it goes with the party. Steam
        // tells every member, the new owner included (OwnerChanged).
        void HostMoved(uint64_t peer) override
        {
            // A party Find Game made is not this lobby's.
            if (!InLobby(peer)) return;
            std::lock_guard g(lock_);
            if (!owner_ || !lobby_ || peer == self_) return;
            const bool moved = Call<bool>(matchmaking_, matchmaking::SetOwner, lobby_.load(), peer);
            LOGI("online: lobby %016llx goes to %016llx%s", (unsigned long long)lobby_.load(),
                 (unsigned long long)peer, moved ? "" : ", which Steam refused");
        }

        bool Matchmaking() override { return true; }

        void List(const online::SessionListing* listing) override
        {
            std::lock_guard g(lock_);
            game_.clear();
            if (listing)
            {
                char number[32];
                for (uint8_t byte : listing->session)
                {
                    std::snprintf(number, sizeof number, "%02x", byte);
                    game_ += number;
                }
                std::snprintf(number, sizeof number, "/%u/%u/%u/%u/", listing->openPublic, listing->openPrivate,
                              listing->filledPublic, listing->filledPrivate);
                game_ += number;
                for (const online::SessionNumber& one : listing->numbers)
                {
                    std::snprintf(number, sizeof number, "%x=%x,", one.id, one.value);
                    game_ += number;
                }
            }
            PublishGameLocked();
        }

        uint32_t Search(const online::SessionSearch& search) override
        {
            std::lock_guard g(lock_);
            // A search the title cancelled is never asked for again.
            const auto now = std::chrono::steady_clock::now();
            std::erase_if(hunts_, [&](const auto& entry) { return now - entry.second.created >= 2 * kListWithin; });
            const uint32_t id = ++lastHunt_;
            Hunt& hunt = hunts_[id];
            hunt.search = search;
            hunt.created = now;
            StartHuntLocked();
            return id;
        }

        bool SearchResults(uint32_t search, std::vector<online::SessionFound>& out) override
        {
            std::lock_guard g(lock_);
            const auto it = hunts_.find(search);
            if (it == hunts_.end()) return true;
            if (!it->second.done) return false;
            out = std::move(it->second.found);
            hunts_.erase(it);
            return true;
        }

        bool TitleFile(const std::string& name, std::vector<uint8_t>& out) override
        {
            // playlists.info on the disc, playlists.patch2 after the title update.
            if (name.rfind("playlists.", 0) != 0) return false;
            out.assign(kPlaylists, kPlaylists + kPlaylistsSize);
            return true;
        }

        void InGame(bool playing) override
        {
            std::lock_guard g(lock_);
            if (playing == playing_) return;
            playing_ = playing;
            LOGI("online: %s", playing ? "in a Find Game game" : "no longer in a Find Game game");
            if (playing) return;
            for (const uint64_t host : foundHosts_) contacted_.erase(host);
            // A friend it played with is a friend again.
            std::erase_if(strangers_, [&](uint64_t peer) { return InLobby(peer); });
            std::erase_if(foundHosts_, [&](uint64_t peer) { return InLobby(peer); });
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
                    if (!gameListing_) PublishGameLocked();
                }
                if (const auto now = std::chrono::steady_clock::now(); now - huntsWatched_ >= kHuntsEvery)
                {
                    huntsWatched_ = now;
                    WatchHunts(now);
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
                    else if (done.call == gameListingCall_) GameListed(done.call);
                    else if (done.call == huntCall_) GamesFound(done.call);
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
                if (update.lobby != lobby_ || update.member != update.lobby) break;
                OwnerChanged();
                Follow();
                break;
            }
            case kSessionRequest:
            {
                // From someone in this player's lobby, or from anyone while this
                // player hosts a system-link game, is listed for Find Game or is
                // in a Find Game game: Spacewar is everyone's. A session this
                // player opened needs no request.
                const auto& request = *reinterpret_cast<const SessionRequest*>(message.param);
                if (request.remote.type != 16) break;
                const uint64_t who = request.remote.steamId;
                const bool member = InLobby(who);
                bool take = member;
                if (!take)
                {
                    std::lock_guard g(lock_);
                    take = !advert_.empty() || !game_.empty() || playing_;
                    if (take) strangers_.insert(who);
                }
                if (take) Call<bool>(messages_, messages::Accept, &request.remote);
                LOGI("online: %s asks for a link: %s", Who(who).c_str(),
                     member ? "in the lobby, taken" : take ? "taken, for a game here"
                                                           : "refused, being neither in the lobby nor at a game here");
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

        // A player who hands the lobby on was in its game already, and is not
        // sent back to it.
        void OwnerChanged()
        {
            const bool mine = CallSteamId(matchmaking_, matchmaking::Owner, lobby_.load()) == self_;
            std::lock_guard g(lock_);
            if (mine == owner_) return;
            LOGI("online: %s lobby %016llx", mine ? "now owns" : "no longer owns", (unsigned long long)lobby_.load());
            owner_ = mine;
            followed_ = !mine;
            PublishLocked();
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

        // The Find Game listing, made the first time this player hosts a game
        // it lists and kept for the run, searchable while there is one.
        void PublishGameLocked()
        {
            const bool listed = !game_.empty();
            if (!gameListing_)
            {
                if (listed && !gameListingCall_)
                    gameListingCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::CreateLobby, kInvisible, kListingMembers);
                return;
            }
            if (listed && game_ != gameShown_ &&
                Call<bool>(matchmaking_, matchmaking::SetLobbyData, gameListing_, "game", game_.c_str()))
                gameShown_ = game_;
            if (Call<bool>(matchmaking_, matchmaking::SetJoinable, gameListing_, listed) && listed != gameListed_)
                LOGI("online: %s", listed ? "this game is listed for Find Game" : "this game is no longer listed for Find Game");
            gameListed_ = listed;
        }

        void GameListed(SteamAPICall call)
        {
            gameListingCall_ = 0;
            LobbyCreated created{};
            bool failed = false;
            if (!getCallResult_(pipe_, call, &created, sizeof created, kLobbyCreated, &failed) || failed ||
                created.result != kResultOK)
            {
                LOGW("online: Steam would not list this game for Find Game (result %d)", created.result);
                return;
            }
            gameListing_ = created.lobby;
            const std::string host = std::to_string(self_);
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, gameListing_, kGameKey, ListingVersion().c_str());
            Call<bool>(matchmaking_, matchmaking::SetLobbyData, gameListing_, "host", host.c_str());
            PublishGameLocked();
        }

        // Steam answers one list at a time, and a request's filters hold for it
        // alone.
        void StartHuntLocked()
        {
            if (searchCall_ || huntCall_) return;
            for (auto& [id, hunt] : hunts_)
            {
                if (hunt.asked) continue;
                hunt.asked = true;
                huntAsked_ = id;
                Call<void>(matchmaking_, matchmaking::AddStringFilter, kGameKey, ListingVersion().c_str(), kEqual);
                Call<void>(matchmaking_, matchmaking::AddDistanceFilter, kWorldwide);
                Call<void>(matchmaking_, matchmaking::ResultCount, kGamesAsked);
                huntCall_ = Call<SteamAPICall>(matchmaking_, matchmaking::RequestLobbyList);
                if (!huntCall_) hunt.done = true;
                return;
            }
        }

        // A listing is anyone's words: what does not read as one is passed over,
        // and what it says about its host is only where the title will ask.
        static bool ReadGame(const char* text, online::SessionFound& found, std::map<uint32_t, uint32_t>& numbers)
        {
            if (!text || std::strlen(text) > kGameMostText) return false;
            const char* slash = std::strchr(text, '/');
            if (!slash || !ReadSession(std::string(text, slash).c_str(), found.session)) return false;
            int used = 0;
            if (std::sscanf(slash, "/%u/%u/%u/%u/%n", &found.openPublic, &found.openPrivate, &found.filledPublic,
                            &found.filledPrivate, &used) != 4 || !used)
                return false;
            const uint32_t most = kMaxMembers;
            if (found.openPublic > most || found.openPrivate > most || found.filledPublic > most || found.filledPrivate > most)
                return false;
            uint32_t id, value;
            for (const char* at = slash + used; *at; at += used)
            {
                used = 0;
                if (std::sscanf(at, "%x=%x,%n", &id, &value, &used) != 2 || !used) return false;
                numbers[id] = value;
            }
            return true;
        }

        void GamesFound(SteamAPICall call)
        {
            huntCall_ = 0;
            const auto it = hunts_.find(huntAsked_);
            LobbyMatchList list{};
            bool failed = false;
            const bool answered = getCallResult_(pipe_, call, &list, sizeof list, kLobbyMatchList, &failed) && !failed;
            if (it == hunts_.end()) return;
            Hunt& hunt = it->second;
            if (!answered) hunt.done = true;
            for (uint32_t i = 0; answered && i < list.matching && hunt.found.size() < hunt.search.most; i++)
            {
                const uint64_t lobby = CallSteamId(matchmaking_, matchmaking::LobbyByIndex, int(i));
                const char* host = Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby, "host");
                online::SessionFound found;
                found.host = host ? std::strtoull(host, nullptr, 10) : 0;
                std::map<uint32_t, uint32_t> numbers;
                if (!found.host || found.host == self_ ||
                    !ReadGame(Call<const char*>(matchmaking_, matchmaking::GetLobbyData, lobby, "game"), found, numbers) ||
                    found.openPublic < hunt.search.room)
                    continue;
                const bool fits = std::all_of(hunt.search.wants.begin(), hunt.search.wants.end(), [&](const online::SessionWant& want) {
                    const auto number = numbers.find(want.id);
                    return number != numbers.end() && (want.atLeast ? number->second >= want.value : number->second == want.value);
                });
                const bool again = std::any_of(hunt.found.begin(), hunt.found.end(),
                                               [&](const online::SessionFound& other) { return other.host == found.host; });
                if (!fits || again) continue;
                hunt.found.push_back(found);
                foundHosts_.insert(found.host);
                SendTo(found.host, 0, kProbePort, "", 1);
            }
            LOGI("online: Find Game: %u listed on Steam, %zu fit", list.matching, hunt.found.size());
            hunt.answered = true;
            hunt.probing = std::chrono::steady_clock::now();
            StartHuntLocked();
        }

        // A search ends when every host it found has a link, or the time is up.
        void WatchHunts(std::chrono::steady_clock::time_point now)
        {
            for (online::Datagram probe; Receive(kProbePort, probe);) {}
            std::lock_guard g(lock_);
            if (searchCall_ && now - searched_ >= kListWithin) searchCall_ = 0;
            StartHuntLocked();
            for (auto& [id, hunt] : hunts_)
            {
                if (hunt.done) continue;
                if (!hunt.answered)
                {
                    if (now - hunt.created < kListWithin) continue;
                    if (hunt.asked && id == huntAsked_) huntCall_ = 0;
                    hunt.done = true;
                    continue;
                }
                std::map<uint64_t, uint32_t> lags;
                for (const online::SessionFound& found : hunt.found)
                {
                    const Identity identity = SteamIdentity(found.host);
                    LinkStatus status{};
                    if (Call<int>(messages_, messages::SessionInfo, &identity, nullptr, &status) == kConnected &&
                        status.ping >= 0)
                        lags[found.host] = uint32_t(status.ping);
                }
                if (lags.size() < hunt.found.size() && now - hunt.probing < kProbeWithin) continue;
                std::erase_if(hunt.found, [&](online::SessionFound& found) {
                    const auto lag = lags.find(found.host);
                    if (lag == lags.end())
                    {
                        LOGI("online: Find Game: no link to %016llx in time", (unsigned long long)found.host);
                        return true;
                    }
                    found.lag = lag->second;
                    return false;
                });
                std::stable_sort(hunt.found.begin(), hunt.found.end(),
                                 [](const online::SessionFound& a, const online::SessionFound& b) { return a.lag < b.lag; });
                if (hunt.found.size() > hunt.search.most) hunt.found.resize(hunt.search.most);
                hunt.done = true;
            }
        }

        // Asks Steam for the hosts listed, unless it is already asked or was
        // asked a moment ago.
        void SearchLocked()
        {
            const auto now = std::chrono::steady_clock::now();
            if (searchCall_ || huntCall_ || now - searched_ < kSearchEvery) return;
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
            StartHuntLocked();
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
        // session only to someone this player accepted, or sent to first. While
        // it takes anyone's link it takes anyone: two players who search can
        // have found each other, and the first's session is the second's.
        bool Reaches(uint64_t peer, uint16_t port)
        {
            {
                std::lock_guard g(lock_);
                if (contacted_.contains(peer) || !advert_.empty() || !game_.empty() || playing_) return true;
                const bool system = searchedHosts_.contains(peer);
                if (!system && !foundHosts_.contains(peer) && !strangers_.contains(peer))
                    return true;   // a lobby member, or one that was
                if (system && searchPorts_.contains(port)) return true;
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
                         // -1 until Steam has a measure.
                         ((status.qualityLocal >= 0 && status.qualityLocal < 0.9f) ||
                          (status.qualityRemote >= 0 && status.qualityRemote < 0.9f) ||
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
        std::set<uint64_t> strangers_;          // whose sessions this player accepted, or sent to in a Find Game game
        std::set<uint64_t> contacted_;          // hosts it found that it sent to on its own

        std::string game_;                      // what this player lists for Find Game
        std::string gameShown_;                 // what the listing says
        uint64_t gameListing_ = 0;              // its entry, kept for the run
        bool gameListed_ = false;               // and whether it shows
        SteamAPICall gameListingCall_ = 0, huntCall_ = 0;
        struct Hunt
        {
            online::SessionSearch search;
            std::vector<online::SessionFound> found;
            std::chrono::steady_clock::time_point created{}, probing{};
            bool asked = false, answered = false, done = false;
        };
        std::map<uint32_t, Hunt> hunts_;        // Find Game's searches
        uint32_t lastHunt_ = 0, huntAsked_ = 0;
        std::chrono::steady_clock::time_point huntsWatched_{};   // the callback thread's
        std::set<uint64_t> foundHosts_;         // every host Find Game found
        std::set<uint64_t> droppedFrom_;        // told about once each
        bool playing_ = false;                  // as InGame says
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

std::unique_ptr<online::Backend> online::MakeSteam() { return std::make_unique<Steam>(); }

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
