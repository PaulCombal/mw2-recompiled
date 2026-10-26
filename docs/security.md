# Security fixes

The game's network code is the 2009 original. The runtime closes the known
holes in it: published CVEs, the bugs iw4x fixed in the PC version of the same
engine, and the ones found in this port. Each fix sits in its own file. A fix
in `runtime/` hooks the title function that misreads its input, and its header
comment describes the defect and the rule it applies.

Where a message breaks the game's own limits, the fix refuses it the way the
game refuses a malformed message (the message is marked overflowed and the
game drops it), or the way the sender's own code would never have written it.
Valid traffic passes untouched.

The sender columns:

- **anyone**: any machine that can reach the game. On the `lan` service that is
  the whole local network; on `steam`, the members of your lobby, any Steam
  player while you host a System Link game, and any game listed in System Link
  while you look for games there (on the search's ports).
- **client**: a player connected to a game you host.
- **host**: the host of a game or party you joined.
- **partner**: the other player in a campaign co-op game.

## Multiplayer

| fixed | sender | file |
|---|---|---|
| CVE-2019-20893: a party join request's subparty count overflows a stack array | anyone | `runtime/joinparty_overflow.cpp` |
| CVE-2018-10718, and the same bug on the client: Huffman decompression of a game packet writes past its buffer | client, host | `runtime/huffman_overflow.cpp` |
| The relay packet forwards any packet to another party member without checking who sent it | anyone | `runtime/relay_reflection.cpp` |
| A forged reliable-command acknowledge freezes the host | client | `runtime/sv_reliable_freeze.cpp` |
| A party session's slot count from the network bounds a walk over the 18 member records | anyone | `runtime/party_slot_count.cpp` |
| The client takes game packets, connect replies and host-migration messages from any address, not only its server | anyone | `runtime/client_packet_sender.cpp` |
| The client runs the `activeAction` dvar, which the host can set, as console commands | host | `runtime/active_action_exec.cpp` |
| The host can set dvars the client later runs as console commands | host | `runtime/command_dvars_from_host.cpp` |
| A map name from the network is pasted into a `map` command, so text after a command separator runs as more commands | host | `runtime/map_name_command.cpp` |
| A player's name is used as a print format on the server | client | `runtime/sv_command_time_format.cpp` |
| A server's disconnect reason is used as a print format on the client | host | `runtime/disconnect_reason_format.cpp` |
| A playlist map index and the party's play history index the playlist table without a bound | host | `runtime/playlist_map_index.cpp` |
| A snapshot's hud element field index writes outside the player state | host | `runtime/hud_elem_field_index.cpp` |
| A baseline snapshot with more than 1024 entities writes past the baseline array | host | `runtime/snapshot_entity_count.cpp` |
| A snapshot's entity, player-state and objective field indexes write outside their structures | host | `runtime/snapshot_delta_field_index.cpp` |
| The game state streamed during host migration is read outside its buffer | host | `runtime/migration_state_size.cpp` |
| Restoring a migrated game state takes script object numbers, script code positions and entity pairs without a bound | host | `runtime/script_save_ids.cpp` |
| UI directive expansion overflows a stack buffer | host | `runtime/ui_directive_overflow.cpp` |

## Campaign and Special Ops co-op

| fixed | sender | file |
|---|---|---|
| The co-op launch, accept and lobby heartbeat messages index the two player slots and the lobby's member records without a bound, and a launch or heartbeat without its terminator never ends | partner | `runtime/coop_slot_index.cpp` |
| The co-op launch's map name is pasted into a `map` command | partner | `runtime/map_name_command.cpp` |
| A co-op frame's commands run for a null client when no client was selected first | partner | `runtime/coop_frame_client.cpp` |
| A co-op frame's length is not compared with the packet, and the copy reads past its end | partner | `runtime/coop_frame_size.cpp` |
| The glass state's piece count is not compared with its 1024-piece table (the message is built locally; hardening) | none | `runtime/glass_piece_count.cpp` |
| UI directive expansion overflows a stack buffer | partner | `runtime/ui_directive_overflow.cpp` |

## Both titles, outside the game code

| fixed | sender | file |
|---|---|---|
| An integer divide by zero, or the one signed divide that overflows, ends the process: the recompiler emitted the host's divide, which traps, where the console's does not. Every guest divide now gives 0 in those cases, as Xenia's does | anyone who can choose a divisor | `patches/xenonrecomp-mw2.patch` (`PPCDivide` in `XenonUtils/ppc_context.h`) |
| The Steam lobby's `connect` line goes to the console, so text after a command separator runs as console commands. Only a line of the exact shape a host advertises is accepted | lobby owner | `runtime/online/service.cpp` (`IsConnectLine`) |
| A guest path with `..` in it reaches files above the game folder or the save package, and a map name a host picks is a guest path (it names a fastfile). Paths are read the object manager's way, as Xenia does: `..` takes back the name before it and never climbs above the device; a component the host would read as a drive is not found | host, partner | `runtime/kernel/files.cpp` (`Canonical`) |
| An indirect call to an address outside the code section reads past the function table, into memory the guest writes, and jumps to the host address it finds there. Every indirect call is bounded to the code section; outside it the address is reported and the process ends, as for any call with no recompiled function | anyone who can corrupt a guest function pointer | `tools/genshared.py` (`PPC_CALL_INDIRECT_FUNC`) |
| A disc image's file names are used as names in the game folder, so `..\x.ff` or a drive in a name writes outside it. A name holding a separator or a colon is skipped, and the root directory's size is compared with the image before it is read | whoever made the disc image | `launcher/disc.cpp` (`IsFileName`) |

## Which builds carry them

The default build (title update 6) carries every fix. The disc-version build
(`VERSION=tu0`, [building.md](building.md#the-disc-version)) carries the
multiplayer joinParty fix and the fixes outside the game code. Its address
table (`runtime/title.h`) has no entries for the other hooks, so they compile
out.

A hook compiles only when its title's address table (`runtime/title.h`) has the
entries it names, so each file builds into the titles it applies to and is
empty in the others.
