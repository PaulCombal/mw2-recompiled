# The relay server

A relay is what players choose with NETWORK SERVER in the launcher. Everyone
connected to the same relay sees each other's System Link games and can join
each other's private matches, wherever they are. The relay plays no match
itself: whoever hosts a match plays in it, as on the console, and the match
ends when the host leaves.

Every packet goes through the relay. Players connect out to it, so only the
relay's machine needs an open port, and no player learns another's address.

## Running one

The relay is one small program, `mw2-relay`, for Linux. It listens on UDP port
3074 unless told otherwise. Its version must be the players' release: a relay
and a game of different protocols refuse each other, and the game's log says
so.

**With Docker**, from the image each release publishes:

    docker run -d --restart unless-stopped -p 3074:3074/udp \
      -e MW2_RELAY_PASSWORD=<password> ghcr.io/paulcombal/mw2-relay:<release>

The `/udp` matters: without it Docker forwards TCP, which the relay does not
use. The image runs as an unprivileged user and holds the binary alone.

With Compose:

    services:
      mw2-relay:
        image: ghcr.io/paulcombal/mw2-relay:<release>
        restart: unless-stopped
        ports: ["3074:3074/udp"]
        environment:
          MW2_RELAY_PASSWORD: <password>

To build the image yourself: `docker build -t mw2-relay relay/`.

**Without Docker**, take `mw2-relay-<release>-linux-x86_64` from the release
(a static binary that needs no library), and run it:

    ./mw2-relay --port 3074 --password <password> [--max 64] [--per-address 16]

`--max` is how many players it takes at once, `--per-address` how many of them
from one IP address: players behind one router share theirs, so a LAN party
on a relay needs as many as it has players. The password can come from the
environment instead (`MW2_RELAY_PASSWORD`), which keeps it out of the process
list. Built from source, it is the `mw2-relay` target of the CMake build, or
`g++ -std=c++20 -O2 -static -o mw2-relay relay/relay.cpp`.

**The firewall** lets in UDP on the relay's port, from anywhere. Players need
nothing open.

**The players** set, under NETWORK in the launcher: NETWORK SERVER, SERVER to
the relay's address (`relay.example.com`, or `relay.example.com:<port>` for
another port than 3074), and PASSWORD.

The relay logs a line, with the time in UTC, for each player who joins,
leaves, goes quiet, or is refused, and why.

## What it costs

The relay forwards the game's traffic, a few kilobytes a second per player in
a match, so a small virtual server carries several matches. Each packet
travels player, relay, player: a relay close to its players keeps the lag low.

## What players on a relay should know

Anyone with the password sees every game hosted on the relay and can join it.
The relay's owner can see all the traffic. Keep the password to the people the
relay is for.

## How it works

The relay is `relay/relay.cpp`, and its protocol `relay/protocol.h`, shared
with the game's side, `runtime/online/server.cpp` ([multiplayer.md](multiplayer.md#lan-and-server)).

Every packet is a 24-byte header -- the magic `MW2R`, a kind, a peer id and
a token -- then what the kind carries, big-endian:

| kind | from | carries |
|---|---|---|
| Hello | game | the protocol, the password; the id the copy had, or 0; the Challenge's token, or 0 |
| Challenge | relay | the token every later packet of this copy carries |
| Welcome | relay | the copy's id |
| Refused | relay | why, as text: wrong password, another protocol, full |
| Alive | game | nothing; sent when the copy has said nothing for a second |
| To | game | a peer's id and the bytes for it |
| All | game | bytes for everyone else on the relay |
| From | relay | the sender's id, which the relay stamps, and the bytes |
| Unknown | relay | nothing: the relay does not know this copy, which says Hello again |
| Bye | game | nothing: the copy is leaving |

UDP lets anyone write any sender address on a packet, so the relay keeps
nothing for a Hello until the address has shown it receives what is sent to
it: the first Hello gets a Challenge with a token, SipHash-2-4 of the address
and port under a key the relay draws at each start, and only a Hello carrying
that token is taken in. Every later packet must carry it too; one that does
not is dropped, so nobody can send, leave, or take a slot as another address.
To an address it has not checked, the relay sends nothing larger than what it
got from there, so it cannot be used to multiply traffic at someone else. A
relay restarted with a new key answers the old tokens with Unknown, and the
copies are challenged again.

A copy is known by the address and port its packets come from. The relay keeps
only the copies it took in. The relay
forgets one it has not heard from for ten seconds; the game's Alive keeps a
copy known while it loads a map. A copy that says Hello again asks for the id
it had, and gets it back unless someone else has it, so after a restart of
the relay every copy is back, under its own id, within a second or so. Ids are 48 bits, because the game makes
the Ethernet address the title tells machines apart by from them.

`kProtocol` is raised whenever a packet's meaning changes.
