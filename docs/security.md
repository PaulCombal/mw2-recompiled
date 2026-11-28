# Security fixes

The game's network code is the 2009 original. The runtime closes the known
holes in it: published CVEs, the bugs iw4x fixed in the PC version of the same
engine, and the ones found in this port. Each fix sits in its own file. A fix
in `src/hooks/` hooks the title function that misreads its input, and its header
comment describes the defect and the rule it applies.

Where a message breaks the game's own limits, the fix refuses it the way the
game refuses a malformed message (the message is marked overflowed and the
game drops it), or the way the sender's own code would never have written it.
Valid traffic passes untouched.

The sender columns:

- **anyone**: any machine that can reach the game. On the `lan` service that is
  the whole local network; on `steam`, the members of your lobby and those who
  were, while their link lasts; any Steam player while you host a System Link
  game, and from opening Find Game until about 40 seconds after you leave its
  game; any game listed in System Link once you have looked for games there (on
  the search's ports, for the rest of the run); and a System Link host you chose
  to join, for the rest of the run. On `server`, everyone on the relay:
  that is who Find Game puts you in a match with.
- **client**: a player connected to a game you host.
- **host**: the host of a game or party you joined.
- **partner**: the other player in a campaign co-op game.

## Multiplayer

| fixed | sender | file |
|---|---|---|
| CVE-2019-20893: a party join request's subparty count overflows a stack array | anyone | `src/hooks/joinparty_overflow.rs` |
| CVE-2018-10718, and the same bug on the client: Huffman decompression of a game packet writes past its buffer | client, host | `src/hooks/huffman_overflow.rs` |
| The relay packet forwards any packet to another party member without checking who sent it | anyone | `src/hooks/relay_reflection.rs` |
| A forged reliable-command acknowledge freezes the host | client | `src/hooks/sv_reliable_freeze.rs` |
| A party session's slot count from the network bounds a walk over the 18 member records | anyone | `src/hooks/party_slot_count.rs` |
| The client takes game packets, connect replies and host-migration messages from any address, not only its server | anyone | `src/hooks/client_packet_sender.rs` |
| The client runs the `activeAction` dvar, which the host can set, as console commands | host | `src/hooks/active_action_exec.rs` |
| The host can set dvars the client later runs as console commands | host | `src/hooks/command_dvars_from_host.rs` |
| A map name from the network is pasted into a `map` command, so text after a command separator runs as more commands | host | `src/hooks/map_name_command.rs` |
| A player's name is used as a print format on the server | client | `src/hooks/sv_command_time_format.rs` |
| A server's disconnect reason is used as a print format on the client | host | `src/hooks/disconnect_reason_format.rs` |
| A playlist map index and the party's play history index the playlist table without a bound | host | `src/hooks/playlist_map_index.rs` |
| A snapshot's hud element field index writes outside the player state | host | `src/hooks/hud_elem_field_index.rs` |
| A baseline snapshot with more than 1024 entities writes past the baseline array | host | `src/hooks/snapshot_entity_count.rs` |
| A snapshot's entity, player-state and objective field indexes write outside their structures | host | `src/hooks/snapshot_delta_field_index.rs` |
| The game state streamed during host migration is read outside its buffer | host | `src/hooks/migration_state_size.rs` |
| Restoring a migrated game state takes script object numbers, script code positions and entity pairs without a bound | host | `src/hooks/script_save_ids.rs` |
| UI directive expansion overflows a stack buffer | host | `src/hooks/ui_directive_overflow.rs` |

## Campaign and Special Ops co-op

| fixed | sender | file |
|---|---|---|
| The co-op launch, accept and lobby heartbeat messages index the two player slots and the lobby's member records without a bound, and a launch or heartbeat without its terminator never ends | partner | `src/hooks/coop_slot_index.rs` |
| The co-op launch's map name is pasted into a `map` command | partner | `src/hooks/map_name_command.rs` |
| A co-op frame's commands run for a null client when no client was selected first | partner | `src/hooks/coop_frame_client.rs` |
| A co-op frame's length is not compared with the packet, and the copy reads past its end | partner | `src/hooks/coop_frame_size.rs` |
| The glass state's piece count is not compared with its 1024-piece table (the message is built locally; hardening) | none | `src/hooks/glass_piece_count.rs` |
| UI directive expansion overflows a stack buffer | partner | `src/hooks/ui_directive_overflow.rs` |

## Both titles, outside the game code

| fixed | sender | file |
|---|---|---|
| An integer divide by zero, or the one signed divide that overflows, would end the process: the host's divide traps where the console's does not. Every guest divide gives 0 in those cases, as Xenia's does | anyone who can choose a divisor | `XenonRecomp-rs` (`ppc_divide_*` in `XenonUtils/rust/ppc_rt.rs`) |
| The Steam lobby's `connect` line goes to the console, so text after a command separator runs as console commands. Only a line of the exact shape a host advertises is accepted | lobby owner | `src/runtime/online/mod.rs` (`is_connect_line`) |
| A guest path with `..` in it reaches files above the game folder or the save package, and a map name a host picks is a guest path (it names a fastfile). Paths are read the object manager's way, as Xenia does: `..` takes back the name before it and never climbs above the device; a component the host would read as a drive is not found | host, partner | `src/runtime/kernel/files.rs` (`canonical`) |
| An indirect call to an address outside the code section reads past the function table, into memory the guest writes, and jumps to the host address it finds there. Every indirect call is bounded to the code section; outside it the address is reported and the process ends, as for any call with no recompiled function | anyone who can corrupt a guest function pointer | `config/mw2_hooks.rs` (`ppc_call_indirect`), `src/runtime/crash.rs` (`MW2ReportBadIndirectCall`) |
| A disc image's file names are used as names in the game folder, so `..\x.ff` or a drive in a name writes outside it. A name holding a separator or a colon is skipped, and the root directory's size is compared with the image before it is read | whoever made the disc image | `install/src/xdvdfs.rs` (`is_file_name`) |

## Which builds carry them

The default build (title update 6) carries every fix. The disc-version build
(`--tu0`, [building.md](building.md#the-disc-version)) carries the
multiplayer joinParty fix and the fixes outside the game code.

A hook compiles only when its title's address table (`runtime/title.h`) has the
entries it names, so each file builds into the titles it applies to and is
empty in the others; the disc version's table has no entries for the other
hooks.
