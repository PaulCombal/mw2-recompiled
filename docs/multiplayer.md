# Multiplayer

The disc carries two programs. `default.xex` is the campaign and special ops;
`default_mp.xex` is the multiplayer, a separate build of the same engine with its
own fastfiles, menus and maps. The runtime is the same for both; what differs is
the recompiled tree it is linked against and the table of guest addresses it
hooks.

    TITLE=mp ./build.sh
    MW2_CONSOLE="8:map mp_afghan" ./build-mp/mw2 mw2/default_mp.pe mw2/game

## One runtime, two titles

| | campaign | multiplayer |
|---|---|---|
| executable | `mw2/default.xex` | `mw2/default_mp.xex` |
| flat image | `mw2/default.pe` | `mw2/default_mp.pe` |
| recompiler config | `config/MW2.toml` | `config/MW2MP.toml` |
| switch tables | `config/mw2_switch_tables.toml` | `config/mw2mp_switch_tables.toml` |
| generated C++ | `ppc/` | `ppc_mp/` |
| build directory | `build/` (`build-release/`) | `build-mp/` (`build-mp-release/`) |

Both are the executables title update 6 makes of the disc's; the disc's own
are a build option with names of their own
([building.md](building.md#the-disc-version)).

`build.sh` takes `TITLE=sp` (the default) or `TITLE=mp`. CMake takes
`-DMW2_TITLE=sp|mp`, which selects the recompiled tree and, for `mp`, defines
`MW2_TITLE_MP`. The Python tools make the same choice through the `MW2_TITLE`
environment variable (`tools/title.py`), which reads the PE section table from
the image because the two executables lay their sections out differently.

`config/MW2MP.toml` has its own `[main]` block: the multiplayer's addresses of
the register save/restore helpers and of `setjmp`/`longjmp`, and its own list
of jump-table functions (`tools/fixbounds.py`).

### `runtime/title.h`

Every function in the two images sits at a different address. `runtime/title.h`
is the only place the runtime names guest addresses; everything else refers to
`T_*` entries through macros:

| macro | expands to |
|---|---|
| `GUEST_FUNC(T_x)` | the recompiled function, `sub_<addr>` |
| `GUEST_ORIG(T_x)` | the original once hooked, `__imp__sub_<addr>` |
| `GUEST_HOOK(T_x) { ... }` | declares the original and defines the hook |
| `GUEST_ADDR(T_x)`, `GUEST_NAME(T_x)` | the address as a number, and as a log string |

A function entry is bare hex so it can be pasted onto `sub_`. A data entry
(`T_DATA_*`) is an ordinary constant and is zero where the address is not known
for a title; code that reads one checks for zero. A function not located in one
title has no entry there, and the hook naming it is compiled out under `#ifdef`
(`T_DB_MissingAsset` in the multiplayer). The client-state offsets used by the
walk autopilot are known only for the multiplayer, since the campaign accepts
`setviewpos`; the save-device table is known only for the campaign.

### `tools/find_in_title.py`

Locates the other executable's copy of a function by code, not by name. The two
images are one engine linked twice, so a function's instructions match except
for what the linker filled in: branch displacements and the 16-bit halves of
absolute addresses. Masking those out of the first N words gives a signature;
a unique match with the same `.pdata` size is the same function.

    tools/find_in_title.py 820C3390 8227CF18     # campaign -> multiplayer
    tools/find_in_title.py --table               # every T_ entry in title.h
    tools/find_in_title.py --reverse 823FF900    # multiplayer -> campaign

Several matches mean the first words are a common prologue; `--words` uses more.
A unique match of a different size needs reading before it is trusted.

## What the multiplayer needs from the runtime

**Every fastfile.** The campaign loads the level it is told to; the multiplayer
chooses its own map and zones. A zone whose file is missing makes the title
report a dirty disc and fail, so `build.sh` and the launcher extract every
`.ff` and `.pak` on the disc (about 5.9 GB), and the campaign's Bink movies
beside them ([gameplay.md](gameplay.md#movies)); the multiplayer plays none.

**A network link.** The multiplayer refuses to start any match without one, and
a system-link match is how it reaches a map. `XNetGetEthernetLinkStatus`
(`runtime/kernel/net.cpp`) reports an active 100 Mb/s full-duplex link in the
multiplayer build and no link in the campaign build; `MW2_NET_LINK=0|1`
overrides either.

**Sessions through XAM.** The title's session, matchmaking and statistics calls
reach the kernel as XAM messages through `XMsgStartIORequest`, not as imports of
their own. `XSessionCreate` is app `0xFB`, message `0x000B0010`, with a 28-byte
buffer `{ session, flags, public slots, private slots, xuid, session info out,
nonce out }`. `runtime/kernel/xam.cpp` fills the 60-byte `XSESSION_INFO`: an
8-byte session id, the 36-byte `XNADDR` from `XNetGetTitleXnAddr`, and a 16-byte
key. It must be filled: the title reads the session back, and an all-zero id is
treated as a Live error (`Com_Error(XBOXLIVE_LIVEERROR)`). A session id's top
bits mark its kind: clear for system link, `0x80` for a Live session. A session
asking for Live features (presence, stats, matchmaking, arbitration) is refused
with `X_ONLINE_E_SESSION_NOT_LOGGED_ON` unless the online service carries
lobbies, and the title then takes its offline path.

**Occlusion queries.** The renderer issues them (the sun glare test) and a job
worker polls for the result, so `EVENT_WRITE_ZPD` must always write its
sample-count record. See [rendering.md](rendering.md).

**No `setviewpos`.** The multiplayer refuses it, so a spot is reached by walking
there (`MW2_WALK_TO`, `MW2_WALK_PATH`; [switches.md](switches.md)).

## The online service

What the title's network calls reach is a backend the player chooses as the
game starts, declared in `runtime/online/service.h`. The kernel side (`kernel/net.cpp`,
`kernel/xam.cpp`) keeps everything Xbox-shaped -- `XNADDR`s, session keys,
sockets, ports -- and asks the service only for what a service does:

| call | purpose |
|---|---|
| `Start()` | log in; false leaves the title offline |
| `LocalId()` | where other players reach this one; unique and stable for the run |
| `Account()` | who the player is, stable across runs; the XUIDs are built from it |
| `LocalName()` | the name other players see (the title shows 15 characters) |
| `Send`, `Broadcast`, `Receive` | unreliable datagrams, addressed by the title's own port numbers; broadcast reaches everyone who could join, never self |
| `Advertise`, `NextJoin` | optional: system-link invitations as the title's `connect` console line |
| `Friends`, `InviteFriend`, `InviteFriends` | optional: the friends screen's list and one invitation; or, without a list, everyone invited at once |
| `Lobbies`, `SetJoinable`, `NextInvite` | optional: Xbox LIVE parties and private matches |

    MW2_NETWORK=none     # default: system link on this machine
    MW2_NETWORK=lan      # plain UDP between machines
    MW2_NETWORK=server   # through a relay server (MW2_SERVER)
    MW2_NETWORK=steam    # through the player's Steam client, or lan without it

The launcher's NETWORK screen writes it, with the server's address and
password, to `.env`. Every build has every backend; a backend is a function
that makes it (`online::MakeLan`), and `online::Get()` tries the chosen one's
list in `service.cpp` and keeps the first whose `Start()` succeeds: `steam` is
Steam and then lan, so a player whose Steam is not running plays over the local
network instead, as another player (below). The log names the service. Only
steam joins the Flatpak Steam's namespace at the start
(`EnterServiceNamespace`).

With `none` there is no service, and the title's sockets are host UDP sockets, enough
for system link on one machine. Only one copy can have the title's ports then;
a second one's binds fail, which the title takes for a fatal error and restarts
on, so `NetDll_bind` leaves its sockets unbound and it plays without a network.

The player's rank is the profile's, whichever service he plays through
([gameplay.md](gameplay.md#signing-in)): the offline stats under his offline
XUID and, signed in to Live, the Live stats under his online one
([saves.md](saves.md)).

With a service, the title's sockets are the service's and nothing is
bound on the host, so several copies can run on one machine. A peer's 64-bit id
travels inside its `XNADDR` (in `abOnline`, behind the tag `MW2O`), and
`XNetXnAddrToInAddr` hands the title a stand-in IPv4 address in `10.0.0.0/8`
for it. Datagrams to a stand-in address or to broadcast go to the service;
datagrams to this machine's own address come straight back.

`online::Frame()` runs once a frame on the title's main thread (from
`XamInputGetState`). It hands an accepted Live invitation to the kernel, or, for
a system-link join, types the commands the System Link menu would run followed
by the host's `connect` line.

A system-link host logs the line another machine joins with:
`online: hosting; another machine joins with "connect <xnaddr> <key> <id>
<public> <private> <nonce>"` -- the same command the title's server browser
issues after a search. `MW2_TRACE_ONLINE=1` logs every datagram through the
service (diagnostic builds).

### LAN and server

The two are one service, `runtime/online/players.cpp`, over two transports
(`transport.h`): a transport opens, says where this copy is reached and which
copy on the machine it is (its slot), sends to one peer or to all, and hands
over what arrived. The service frames the title's datagrams, keeps who is
there, and carries the Xbox LIVE lobbies, so both have the same System Link
list and private matches.

`runtime/online/lan.cpp`: plain UDP on the local network. Each copy sends from a
port of its own, the first free one above the shared port; broadcasts go to the
shared port, `MW2_LAN_PORT` (3074). A peer id is the IPv4 address in the top
bits and the port in the low 16. The log names the network connections found
and the one announced.

`runtime/online/server.cpp`: a relay server (`relay/`, [relay.md](relay.md)) at
`MW2_SERVER`, with `MW2_SERVER_PASSWORD`. The copy says Hello, answers the
relay's challenge with the token it was given, which every later packet
carries, and gets its id, 48 bits; one that has none within five seconds does not start,
and the title stays offline. "To all" is everyone on the relay, and every
packet goes through it, so nobody needs an open port and nobody learns another
player's address. A thread says Alive every second, so a copy stays known while
the title is loading a map and not polling; a relay that forgot it answers
Unknown, and the copy says Hello again with the id it had. Its own port is
picked as lan's is, for the slot.

The account is the profile at the first controller
([gameplay.md](gameplay.md#signing-in)) plus the copy's slot, so a second
copy on one machine is a second player; `MW2_NAME` is another name for him than
the profile's. The log says so when another machine has the same account.

Every copy announces who it is and what can be joined once a second to
everyone the transport reaches. `MW2_LAN_JOIN=<name>` joins that player's party or lobby as soon as
it is seen, as choosing a friend and "Join Session" does on the console. The
lobby's "Invite friends" invites every player seen; a copy started with
`MW2_LAN_ACCEPT=1` accepts.

### Steam

`runtime/online/steam.cpp` talks to the Steam client the player already runs.
It loads the client library from the install -- on Linux `MW2_STEAMCLIENT`,
else the install the running `steam` process has its `steamclient.so` mapped
from, else the first of the usual location, the snap and the Flatpak;
`steamclient64.dll` from the registry's `SteamPath` on Windows -- and calls its
versioned interfaces through their vtables, as `libsteam_api` does. Nothing of
Valve's is built in or shipped; the Steamworks SDK is a reference for slots and
structures only. Another install's library than the running Steam's connects
halfway, then fails its first calls (`IClientUtils::GetSteamRealm returned
failure code 12`) and crashes.

**The Flatpak's Steam** runs in a process namespace of its own and tells its
clients apart by process id: a client outside it has its connection dropped. So
when the running Steam is in another process namespace, the game joins it
first thing in `main`, before any thread exists
(`online::EnterServiceNamespace`): `setns` through a pidfd into its user and
process namespaces together (the user namespace is the player's own, so no
privilege is needed), or one after the other on kernels that refuse both at
once. A process namespace takes in only children made after joining, so the
game forks: the child goes on as the game, and the parent waits, hands it
SIGTERM and SIGHUP, and exits with its status. The mount and network
namespaces stay the host's.

The game runs as Spacewar, app 480 (`MW2_STEAM_APPID`). The app id is put in the
environment before the library loads, because the client reads it at load time
and its networking has no interface to give otherwise. Environment variables the
client adds for SDL are taken back, since they would hide every physical
controller from a game Steam did not start.

- Players are SteamIDs; datagrams are `ISteamNetworkingMessages`, with the
  channel carrying the title's port.
- Every player starts in a friends-only lobby of their own. The lobby is what
  the title's broadcasts reach, so a game a member hosts shows in every other
  member's System Link list. Networking sessions are accepted only from lobby
  members, since Spacewar is shared by everyone, except by a host (below).
- System Link is also shared with every Steam player. A player hosting a
  system-link game is listed: an invisible lobby of its own (`mw2recomp` = the
  release's number (Versions, below), `host` = its SteamID; 250 members, since a search never
  returns a full lobby). It is listed while the title's hosted session is on,
  from its creation (SV_SpawnServer) or XSessionStart to its XSessionEnd or
  XSessionDelete -- a map change ends one and creates the next. A broadcast
  from a player who hosts nothing also goes to every host listed, worldwide;
  Steam is asked for the list at most every 2 s (a listing Steam would not
  make is asked for again every 5 s). The title broadcasts only to search for
  system-link games and to answer a search. What a stranger reaches:
  - a host, while it hosts: it accepts anyone's session, and its broadcasts
    reach the strangers whose sessions are open;
  - a player who searched, on the ports its searches went to, from a host it
    found. Anyone can make a listing, with any `host`, so a listed host is a
    stranger like any other; a stranger can also join a listing and, once its
    host leaves, keep it listed under the host's id (there is no kick);
  - a player who chose to join a listed host's game (sent to it on its own).
  Everything else from a stranger is dropped as it arrives (`Reaches`), so a
  player who stops hosting is closed to the strangers it played with. Their
  sessions are not closed: a host's map change ends its game and starts the
  next at once. Xbox LIVE parties and private matches stay among friends.
- The advertised game is the newest hosted system-link session that is on;
  `kernel/xam.cpp` (`PublishHosted`) chooses it and tells the service under one
  lock, so threads ending one session and creating another cannot reorder it.
- F6, or the Xbox LIVE lobby's "Invite friends", opens the friends screen
  (`friends.h`), drawn over the frame as the sign-in screen is, over a blurred
  and darkened copy of the game (`gpu/vulkan/backdrop.h`): the friends
  running this game, with their avatars, then how many others are online and
  offline. A friend counts as running it when Steam says they play app 480 and
  their rich presence has `mw2recomp`, which every copy sets to its executable
  and release (`mp:<release>`, `sp:<release>`); the rest of Spacewar's players
  have no such key. A friend in the other executable, or on another release,
  is listed but cannot be invited: the campaign's Spec Ops invitations and the
  multiplayer's do not carry into each other. Steam's overlay would need
  Steam to have started the game. The list is built on the callback thread,
  once a second while the screen asks for it, and A sends Steam's lobby
  invitation (`InviteUserToLobby`), which the friend accepts in Steam's chat; a
  friend can also be invited from Steam's own friend list. Accepting moves the
  friend into the host's lobby. With another service the screen says that
  choosing friends needs Steam; LAN still invites everyone on the network.
- The host's lobby data carries what joins its game: `xsession`, the Xbox LIVE
  party or private match the host is in, which the friend's copy takes as an
  accepted Live invitation; otherwise `connect`, the host's system-link join
  line, which the friend's copy types. Either is acted on once per lobby
  entered, and again when the friend accepts another invitation to the lobby
  they are in: the host may have started another party since, or the friend
  left it.
- Each copy logs the Steam links to the players it talks to: a request for one
  and whether it was taken, a link connecting, connected (with its ping), and
  ending (with Steam's reason), and, at most every 5 s, a link that loses
  packets or queues what it sends.
- Steam cannot launch the game from an invitation (it would start Spacewar), so
  the friend must already be running it.

## Versions

Only copies of the same release play together, by the title's own check
(`runtime/network_version.cpp`). Its protocol number (`sub_8230A3D8`, 136; 128
on the disc) carries the console's online-update version in the high half;
that update never runs here, so the high half carries the release instead:
`version::Network()`, 15 bits of a hash of the release tag (`MW2_RELEASE_TAG`),
or of `dev` for a build without one. The low half stays the title's.

- System Link leaves a game of another protocol out of the list
  (CL_ServerInfoPacket).
- A host refuses a client of another protocol (SV_DirectConnect) with
  `EXE_SERVER_IS_DIFFERENT_VER`, "Server is a different version: &&1". The
  title fills in "1.4", every copy's build number; the runtime puts the host's
  release there.
- A party host refuses a member of another protocol
  (PartyHost_HandleJoinPartyRequest); the member sees "Unable to join game
  session", the one message the title has for any refused party join.
- The Steam listing's `mw2recomp` value is the same number, with `-tu0` on the
  disc version, so a search finds only games it can join.

The campaign's co-op has no version check.

## Xbox LIVE private matches

A service whose `Lobbies()` is true (lan and steam) has the player signed in to
Xbox LIVE, which opens PLAY ONLINE: the party, PRIVATE MATCH, the lobby, START
GAME. No Live service is involved; `runtime/kernel/xam.cpp` answers what the
title asks:

| | signed in to Live | offline (`none`) |
|---|---|---|
| sign-in state | 2, Live-enabled | 1, local |
| XUID | online `0x0009` over the account's low 48 bits; offline `0xE0...` over the same bits | offline `0xE0...` |
| privileges | all granted | all denied |

Signed in, every new notification listener for the Live area hears
`XN_LIVE_CONNECTIONCHANGED` (connection established), since the connection came
up before the title was listening. `XOnlineGetNatType` reports an open NAT.

**Sessions** are kept by session object. The newest one this machine hosts with
presence and invitations enabled -- the party's in the Xbox LIVE menu, the
private match's in its lobby -- is the joinable session; its 60-byte
`XSESSION_INFO` is handed to the service (`SetJoinable`) whenever sessions
change. The host logs the console line that joins it:
`online: joinable; another machine joins with "xpartyjoin <xnaddr> <key> <id>"`.

**Invitations** the service returns (`NextInvite`) become the console's accepted
invitation: `XN_LIVE_INVITE_ACCEPTED` is posted, then `XInviteGetAcceptedInfo`
returns it. One accepted before the title listens waits for its first Live
listener.

**Live storage** is on this machine under `saves/online/` (see
[saves.md](saves.md)). The title names files by server path
(`//title.<id>/t:<id>/<file>`, `//tuser.<id>/u:<xuid>/<id>/<file>`):

| directory | contents |
|---|---|
| `title/` | the title's own files -- message of the day, `playlists.info`. Nothing publishes them, so each reads as empty |
| `user/<xuid>/` | what the title uploads: the player's multiplayer rank and stats (`mpdata`), starting at level 1 |

Enumerations of Live title servers and downloadable content end at once, empty.

When a Live-signed-in player joins a system-link game, the title asks which
stats to use; the automatic join answers Live (`set useonlinestats 1`), so the
rank is the same in private matches and system link. With `none`, the Live menu
refuses as signed out.

## Switches

| switch | effect |
|---|---|
| `TITLE=sp\|mp` | `build.sh`: which executable to build |
| `MW2_NETWORK=none\|lan\|server\|steam` | which online service; the launcher's NETWORK |
| `MW2_SERVER=<host>[:<port>]` | server: the relay's address |
| `MW2_SERVER_PASSWORD` | server: the relay's password |
| `MW2_NET_LINK=0\|1` | Ethernet link reported down or up (default: up in the multiplayer, down in the campaign) |
| `MW2_NAME` | lan and server: the player's name |
| `MW2_LAN_PORT` | lan: the shared broadcast port (3074) |
| `MW2_LAN_JOIN=<name>` | lan: join that player's party or lobby when seen |
| `MW2_LAN_ACCEPT=1` | lan: accept invitations |
| `MW2_STEAMCLIENT=<path>` | steam: the client library to load |
| `MW2_STEAM_APPID` | steam: the app id to run as (480) |
| `MW2_TRACE_ONLINE=1` | log every datagram (diagnostic builds) |

The full list is in [switches.md](switches.md).
