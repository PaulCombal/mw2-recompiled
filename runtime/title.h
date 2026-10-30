#pragma once
// The guest addresses the runtime names, for each of the disc's two executables.
//
// The runtime hooks the title's own functions by their address -- D3D9's
// present, the fastfile loader's reads, the job system's waits -- and reads a
// few of its globals. Those addresses belong to one executable: default.xex
// (the campaign, the default) and default_mp.xex (the multiplayer) are the same
// engine built twice, and every function sits somewhere else in each. This
// table is the only place the runtime says which.
//
// A function entry is the bare hex address, without 0x, so the macros below can
// paste it onto sub_ / __imp__sub_ / 0x. A data entry is an ordinary constant,
// zero when the multiplayer's is not known: the code that reads it checks.
// A function the multiplayer has not been located in has no entry, and the
// hook that names it is compiled out under #ifdef.
//
// The multiplayer addresses were found with tools/find_in_title.py. Comments
// elsewhere in the runtime cite the disc's single-player addresses, which the
// update moved by eight bytes at most.
//
// The build selects the title: -DMW2_TITLE=mp defines MW2_TITLE_MP.
//
// It also selects the version. The executables are the ones title update 6
// makes of the disc's; -DMW2_VERSION=tu0 defines MW2_VERSION_TU0 for the
// disc's own, whose blocks come last and carry no comments: each entry is the
// same function or datum as the update's, found by its instructions (the
// functions) or by what the same instructions of the same function form (the
// data). The multiplayer's client state is 0x200 smaller on the disc; the
// fields read here have the same offsets.

#if defined(MW2_TITLE_MP) && !defined(MW2_VERSION_TU0)

#define MW2_TITLE_NAME  "multiplayer"
#define T_ENTRY_POINT   0x823A90C8u          // the XEX header's ENTRY_POINT

// D3D9 (statically linked; the same library build in both executables)
#define T_D3D_Present          820DFD10
#define T_D3D_ArenaWait        820E1E70
#define T_D3D_ReplayRecording  820E4848
#define T_D3D_InitPixelShader  820D7268   // a pixel shader object made over a loaded program; see shader_preload.cpp
#define T_D3D_PatchVertexFetches 820D5220 // rewrites a vertex shader's fetches for a declaration (Bind and two more)

// the job system
#define T_Job_WaitPredicate    823F0C80

// the engine
#define T_Com_Error            82281FA0
#define T_Com_Printf           8227F448
#define T_Com_DPrintf          8227F540
#define T_Com_PrintError       8227F5E0
#define T_Com_PrintWarning     8227F708
#define T_Cbuf_AddText         82275C60
#define T_Memcard_InitializeSystem 8233D890
#define T_Image_FlushMove      823DE738
// the view's angle, and the zoom the distance culls take from it (field_of_view.cpp)
#define T_CG_ViewFov           8215B9A8
#define T_CG_CullZoom          8215BC28
// Dvar_RegisterInt(name, default, min, max, flags, description): the anisotropy
// the textures are filtered with at least, r_texFilterAnisoMin (texture_filtering.cpp)
#define T_Dvar_RegisterInt     8230EE48
// the sizes the title draws at, and the picture's shape (render_size.cpp).
// R_VideoSizes(config) fills { bool wide, hiDef; float refresh; scene w, h;
// frame w, h; samples } from the video mode, the sizes its own: 1024x600 and
// 1280x720 in high definition. R_SetVidConfig(config) makes the vidConfig of
// it: scene w, h, frame w, h, ..., +28 the picture's aspect (16/9 when wide,
// else 4/3), +32 and +36 a scene's and a frame's pixel's (h * aspect / w).
// R_DrawStretchPic(x, y, w, h, s0, t0, s1, t1, colour, material) queues a 2D
// picture.
#define T_R_VideoSizes         823C3DD0
#define T_R_SetVidConfig       823C35C8
#define T_R_DrawStretchPic     823C6DB0
#define T_UI_EvaluateStatement 822D72F0   // a menu expression's value (localClientNum, statement)
#define T_DATA_VidConfig          0x83B3A928u
// gfxRenderTargets: 20 bytes a render target { image, colour surface, depth
// surface, width, height }, in the order of the R_RENDERTARGET_ names; 11 and
// 12 are the sun's and the spots' shadow maps (shadow_resolution.cpp).
#define T_DATA_RenderTargets      0x83D8DA80u
// network hardening against the known IW4 vulnerabilities.
// PartyHost_HandleJoinPartyRequest is the "joinParty" OOB handler whose
// subparty count is unbounded -- CVE-2019-20893 (joinparty_overflow.cpp) --
// and atoi is the C library's, which that handler parses the count with.
#define T_PartyHost_HandleJoinPartyRequest 821A6950
#define T_atoi                 8241B1F8
// MSG_ReadBitsCompress is the shared Huffman decompressor; it emits one byte per
// decoded symbol with no bound on the output (CVE-2018-10718 and its client-side
// twin). Huffman_DecodeSymbol decodes one symbol (tree at T_DATA_HuffmanTree+8).
// SV_ParseClientMessage and CL_ParseServerMessage are its only two callers; they
// decompress into a 2048-byte stack buffer and a 64 KB hunk buffer respectively
// (huffman_overflow.cpp).
#define T_MSG_ReadBitsCompress  822897C0
#define T_Huffman_DecodeSymbol  82285F18
#define T_SV_ParseClientMessage 822BCB18
#define T_CL_ParseServerMessage 8218DF10
// UI_ReplaceDirective expands "[{name}]" directives in a HUD/menu string into a
// looked-up value; its directive-name accumulator is a 256-byte stack buffer
// with no bound (ui_directive_overflow.cpp). LookupDirective resolves a name to
// a value, I_strncpyz is the bounded string copy it emits with, and
// Com_PrintMessage is the warning printer it uses for a malformed directive.
#define T_UI_ReplaceDirective   822F0230
#define T_UI_LookupDirective    822F0140
#define T_I_strncpyz            823159B0
#define T_Com_PrintMessage      8227F5E0
// CL_HandleRelayPacket is the "relay" OOB handler that forwards a packet to a
// named client with no sender check -- an unauthenticated reflector that
// relay_reflection.cpp disables. With relay disabled, CL_RouteToClient (the
// party mesh's next-hop lookup, the forwarder's only caller) is hooked so a
// send never picks an intermediary that would now drop it; Session_LocalClientNum
// is the local client number it derives, which the hook reads to spot that case.
// The campaign has no equivalent handler.
#define T_CL_HandleRelayPacket  82184B88
#define T_CL_RouteToClient      82191BC0
#define T_Session_LocalClientNum 8233C4B0
// A client controls its reliableAcknowledge (the last server command it says it
// received); the server stores it off the wire with only a signed range check, so
// INT_MAX slips through and makes the per-client resend loops iterate ~2^31 times
// -> the server freezes (sv_reliable_freeze.cpp). These three are those loops:
// UpdateServerCommandsToClient is the ordinary server-command resend,
// ResendReliableCommands its twin for a message that overflowed, and
// DumpPendingReliableCommands the one that prints every pending command. The
// campaign image has no twin of them.
#define T_SV_UpdateServerCommandsToClient 822C8D68
#define T_SV_ResendReliableCommands       822C8E78
#define T_SV_DumpPendingReliableCommands  822C8F50
// SV_CheckCommandTime refuses a client command whose time is more than 20
// seconds ahead of the server's, and prints the refusal with the client's name
// in the format (sv_command_time_format.cpp). va is the title's formatter into
// a rotating buffer, which that message is built with.
#define T_SV_CheckCommandTime   822BC778
#define T_va                    823160A8
// The client's handler for the reason a server disconnects it with (the server
// command 'w', and the connectionless "disconnect"). A reason it does not know
// is localised, put into EXE_SERVERDISCONNECTREASON and raised with Com_Error
// as the format (disconnect_reason_format.cpp). The rest are what it calls on
// that path: I_stricmp/I_strnicmp, the localiser, the substitution (which ends
// in va("%s")), and the two party calls before it.
#define T_CL_DisconnectReason   82184918
#define T_I_stricmp             82315B70
#define T_I_strnicmp            823159F8
#define T_SEH_LocalizeTextMessage 822CBFC8
#define T_UI_ReplaceReason      822E1598
#define T_Party_StopParty       8219EB00   // "Calling Party_StopParty() for partyId %i"
#define T_Party_OnDisconnect    82198698   // unnamed: (party, the word at party->[0]+1440)
// Sets ui_mapname and ui_gametype from one map entry of the current playlist,
// by the entry's index; the party host's "go" and party-state messages hand it
// an index unbounded (playlist_map_index.cpp).
#define T_Playlist_SetMapEntry  822E6870
// Takes one off the play count of each playlist entry a party's history names;
// a member's history is the host's party state, unbounded (playlist_map_index.cpp)
#define T_Playlist_UnplayHistory 822E67A0
// The only two functions that store a party session's public/private slot
// counts (session+556 / +552), which bound the party's member-index walk; a
// remote peer chooses them, so party_slot_count.cpp clamps them to the 18 slots
// the party has. Create takes them in r8/r9, Modify in r6/r7.
#define T_Session_Create        823918D8
#define T_Session_Modify        82391B18
// CL_RunActiveAction passes the activeAction dvar's value to Cbuf_AddText when
// it is not empty, then sets it back to "" with Dvar_SetString. A host can set
// any client dvar by name with the set-dvar server command, and activeAction is
// registered with flags 0, so the host of a game a player joins could run
// console commands on that player's copy. active_action_exec.cpp clears
// the dvar before the reader looks at it, so it never runs (iw4x parity). The
// campaign has no twin.
#define T_CL_RunActiveAction    82171790
#define T_Dvar_SetString        8230F7D8
// The client's set-dvar server command (the client side of setClientDvar), which
// sets any dvar by name with source 0, past its flags. command_dvars_from_host.cpp
// refuses there the dvars the client runs as commands: com_errorResolveCommand
// (the resolveError menu script; iw4x parity) and nextmap (after a cinematic). The
// campaign's handler accepts one name only.
#define T_CG_SetClientDvarFromServer 82154738
// The party frame and the menu script runner ("StartServer") queue a "map"
// line with ui_mapname pasted in unquoted; partystate and the set-dvar command
// write ui_mapname from the network. map_name_command.cpp cuts the name where
// the command buffer would end the command.
#define T_Party_Frame           8219FBB8
#define T_UI_RunMenuScript      822E2090
// CL_PacketEvent hands a connected client's sequenced packets to the netchan,
// and the out-of-band dispatcher runs the connection handshake and the host
// migration messages, neither checking who sent them; client_packet_sender.cpp
// drops the ones not from the server, with the title's NET_CompareAdr. The
// campaign's packet handler is bound to the sender already.
#define T_CL_PacketEvent        82189A88
#define T_CL_ConnectionlessPacket 82188DD0
#define T_NET_CompareAdr        8228E5B0
// The protocol number every version check compares, and the out-of-band print
// that carries SV_DirectConnect's refusal; network_version.cpp makes the number
// the release's and the refusal name it.
#define T_ProtocolVersion       8230A3D8
#define T_NET_OutOfBandPrint    8228EE28
// A migrated game state's restore takes the file length from the save header
// the server sent, and the segment opener the segment sizes from the file, with
// neither compared with the buffer; migration_state_size.cpp refuses both with
// the reader's own "Trying to read corrupted file".
#define T_G_LoadMigratedGame    8223F360
#define T_MemFile_BeginSegment  82312360
#define T_DATA_MigrationHeader  0x83050600u   // the save header mhead fills, sub_822664A8(0)
#define T_STR_CorruptedFile     0x820562A4u   // "Trying to read corrupted file"
// A saved game's script object count and object numbers index a 36864-entry
// table unchecked; script_save_ids.cpp refuses the ones outside it. The reader,
// the lookups the entity restore uses (sub_822A06E0, and sub_822A0740 for a u16
// object-ref field), and the return addresses of the reads sub_822A0148 (a
// reference), sub_822A0AC8 (the object count) and sub_822A07B0 (an object's
// type) do themselves. Its code positions, read by sub_822A03D8 (a value) and
// sub_822A02E0 (a thread's resume position), must fall inside the script
// program buffer, whose bounds the two words below hold.
#define T_MemFile_Read          82312000
#define T_Scr_SavedObject       822A06E0
#define T_Scr_ResolveObjectRef  822A0740
#define T_RET_Scr_ReadObjectRef   0x822A019Cu
#define T_RET_Scr_ReadObjectCount 0x822A0B18u
#define T_RET_Scr_ReadObjectType  0x822A07E8u
#define T_RET_Scr_ReadCodePos     0x822A04F0u
#define T_RET_Scr_ReadResumePos   0x822A0338u
#define T_DATA_ScrProgramStart    0x835E58E0u
#define T_DATA_ScrProgramEnd      0x835E58E4u
// The entity restore (sub_8223DFF0) reads the count of a 256-entry list of
// entity pairs, then that many 12-byte records, unchecked; the same hook refuses
// a count above the list's 256.
#define T_RET_G_ReadEntityPairCount 0x8223E11Cu
// A baseline snapshot's entities are decoded by CL_DeltaEntity into a
// 1024-entry array indexed by a count nothing caps; snapshot_entity_count.cpp
// refuses the snapshot at the host's own limit, in CL_DeltaEntity and at the end
// of CL_ParsePacketEntities, its only caller.
#define T_CL_DeltaEntity        8218C4A8
#define T_CL_ParsePacketEntities 8218C848
// MSG_ReadDeltaHudElems takes each hud element's changed-field indexes from the
// message without bounding them by the 42-entry field table, and the descriptor
// read past it gives the store's offset; hud_elem_field_index.cpp replaces it.
// MSG_ReadDeltaField decodes one field into place; MSG_ReadBit/Bits read the
// counts and the index gaps.
#define T_MSG_ReadDeltaHudElems 8228CF10
#define T_MSG_ReadDeltaField    8228BFC0
#define T_MSG_ReadBit           82136C80
#define T_MSG_ReadBits          82136D48
// The snapshot's other delta-field readers take their changed-field indexes the
// same way, unbounded by their field tables, and copy and decode by them;
// snapshot_delta_field_index.cpp replaces the three: MSG_ReadDeltaEntity, the
// player state's MSG_ReadDeltaPlayerState and MSG_ReadDeltaFields (the generic
// reader under MSG_ReadDeltaStruct and the objectives). The rest are what they
// call: the gap reader (msg, width), the entity type's {fields, count} lookup,
// MSG_Discard (marks the message overflowed), the byte/short readers, the
// predicted-origin lookup (unnamed; "Unable to find the origin we sent") and the
// entity type's name for the cl_shownet print.
#define T_MSG_ReadDeltaEntity   8228CA90
#define T_MSG_ReadDeltaPlayerState 8228D0D8
#define T_MSG_ReadDeltaFields   8228C7D8
#define T_MSG_ReadFieldGap      8228C718
#define T_MSG_EntityTypeFields  8228F9A8
#define T_MSG_Discard           82289500
#define T_MSG_ReadByte          82289CD8
#define T_MSG_ReadShort         82289D60
#define T_CL_FindSentOrigin     8218C390
#define T_BG_GetEntityTypeName  82100290
// the fastfile loader's signature gate and the stream start it guards (unsigned_fastfiles.cpp)
#define T_DB_StartStream       821B0958
#define T_DB_AuthLoadInit      821AFA10
// UI_LoadArenas reads mp/basemaps.arena into a LargeLocal buffer and parses it
// with UI_ParseInfos(text, room, infos) into T_DATA_UI_ArenaInfos, which
// UI_UpdateArenas (822D8C00) makes the menus' map list of (custom_map_list.cpp)
#define T_UI_LoadArenas        822D89F8
#define T_UI_ParseInfos        822D8798
#define T_LargeLocal_Begin     822FE458   // (handle, size)
#define T_LargeLocal_GetBuf    822FE540   // (handle) -> the buffer
#define T_LargeLocal_End       822FE528   // (handle)
#define T_DATA_UI_ArenaInfos   0x83695D20u   // 64 info strings, +256 how many
#define T_DB_FindXAssetHeader  821E25B0   // (type, name) -> the asset; a menu is type 24
#define T_SEH_StringEd_GetString 822CBF88 // (key) -> its translation, the localize asset's (type 25); 0 when none
// Com_LoadUiFastFile loads the menus' zones (dlc2_ui_mp, dlc1_ui_mp, ui_mp, and common_mp when asked) in one call of
// DB_LoadXAssets(zones, count, sync), XZoneInfo { name, allocFlags, freeFlags } (custom_map_list.cpp)
#define T_Com_LoadUiFastFile   82280C78
#define T_DB_LoadXAssets       821E1C60
#define T_DATA_DB_ZoneCount    0x82710574u   // the zones loaded; the loader stops the game at 32
// not located in the multiplayer: T_DB_MissingAsset

// data
#define T_DATA_DB_Load            0x82678600u   // the loader's state: +0 the file being read (its name at +8), +52 its stream
#define T_DATA_TimeStampBundlePtr 0x820007B4u   // the KeTimeStampBundle import record
#define T_DATA_DebugMonitorPtr    0x820007F4u   // the KeDebugMonitorData import record
#define T_DATA_DeviceTable        0u
// The per-local-client tokenized console/OOB command args (a Cmd_Argv read):
// +0 the local client index, +68 the per-client argc array, +100 the per-client
// argv pointer array (joinparty_overflow.cpp).
#define T_DATA_CmdArgs            0x830587C8u
#define T_DATA_HuffmanTree        0x8327D168u   // the message Huffman struct; tree pointer at +8
// the activeAction dvar pointer, and the "" literal CL_RunActiveAction clears it
// with (active_action_exec.cpp)
#define T_DATA_ActiveActionDvar   0x825089B8u
#define T_DATA_EmptyString        0x8207D720u
// the ui_mapname dvar pointer (map_name_command.cpp)
#define T_DATA_MapNameDvar        0x8369DE38u
// Per local client (client_packet_sender.cpp): its connection state, and a
// pointer to the client connections, each holding the server address the
// handshake talks to and, in the game netchan, the address it was set up with.
#define T_DATA_ClientConnState    0x825A6460u
#define T_CONN_STATE_STRIDE       3216u
#define T_DATA_ClientConnections  0x82508990u
#define T_CONNECTION_STRIDE       0x00051568u
#define T_CONNECTION_SERVER_ADR   16u
#define T_CONNECTION_NETCHAN_ADR  0x00040158u
// the baseline snapshot's entity count, before its 1024-entry array
// (snapshot_entity_count.cpp)
#define T_DATA_BaselineEntityCount 0x82561500u
// the hud element field table ({name, offset, bits, flags}, 16 bytes each) and
// its length, 42 (hud_elem_field_index.cpp)
#define T_DATA_HudElemFields      0x82044320u
#define T_DATA_HudElemFieldCount  0x820449F4u
// The delta readers' field tables, back to back in .rdata, and their lengths
// (snapshot_delta_field_index.cpp): the entities' (62, the length of every
// entity type's table), the player state's (200) and an objective's (6); the
// eType field every entity table starts with; cl_shownet and the dvar behind
// the changed-entity print; the local clients (27904 bytes each) the origin
// lookup takes; and the readers' messages.
#define T_DATA_EntityFieldCount   0x820449C0u
#define T_DATA_PlayerStateFields  0x82043640u
#define T_DATA_PlayerStateFieldCount 0x820449ECu
#define T_DATA_ObjectiveFields    0x820442C0u
#define T_DATA_ObjectiveFieldCount 0x820449F0u
#define T_DATA_ETypeField         0x8203F960u
#define T_DATA_ShownetDvar        0x825A8B84u
#define T_DATA_EntityPrintDvar    0x83281D84u
#define T_DATA_LocalClients       0x825A8B6Cu   // pointer to the local clients
#define T_LOCAL_CLIENT_STRIDE     27904u
#define T_STR_DeltaFieldsLastChanged 0x8203D70Cu  // "Got lastChanged field of %i, but there are only %i fields\n"
#define T_STR_EntityLastChanged   0x8203D778u   // "Last changed field was %i, but there are only %i fields\n"
#define T_STR_EntityRemove        0x8203D748u   // "%3i: #%-3i remove\n"
#define T_STR_EntityNumber        0x8203D7B4u   // "%3i: #%-3i "
#define T_STR_EntityChanged       0x8203D75Cu   // "%3i: changed ent, eType %s\n"
#define T_STR_PlayerState         0x8203D80Cu   // "%3i: playerstate "
#define T_STR_PercentSSpace       0x8203A3CCu   // "%s "
#define T_STR_PS_STATS            0x8203D7C0u   // "PS_STATS"
#define T_STR_OriginNotFound      0x8203D7CCu   // "Unable to find the origin we sent, delta is not going to work\n"
// the two warning format strings UI_ReplaceDirective prints for a malformed
// directive (ui_directive_overflow.cpp)
#define T_STR_DirectiveEmpty      0x820534E8u   // "Directive empty in string '%s'"
#define T_STR_DirectiveNoEnd      0x820534B4u   // "No end token to match begin token in string '%s'"
// the server's time in milliseconds, the refused command's message and a bare
// "%s" to print it through (sv_command_time_format.cpp)
#define T_DATA_ServerTime         0x8367A010u
#define T_STR_InvalidCommandTime  0x8204A2E8u   // "Invalid command time %i from client %s, current server time is %i\n"
#define T_STR_PercentS            0x8201184Cu   // "%s"
// The disconnect reasons CL_DisconnectReason knows, the message key it puts an
// unknown one into, and the party the two party calls on its path take
// (disconnect_reason_format.cpp)
#define T_STR_EXE_MATCHENDED      0x82019508u
#define T_STR_EXE_SERVERQUIT      0x820194F8u
#define T_STR_EXE_MIGRATIONABORTED 0x820194E0u
#define T_STR_EXE_MIGRATION_IN_PROGRESS 0x820194C4u
#define T_STR_hostquit            0x820194B8u
#define T_STR_EXE_PLAYERKICKED    0x820194A4u
#define T_STR_roundsdone          0x8201947Cu
#define T_STR_XBOXLIVE_SIGNINCHANGED 0x8201940Cu
#define T_STR_EXE_SERVERDISCONNECTREASON 0x82019488u
#define T_DATA_PartyForDisconnect 0x825B5C18u
// A playlist row holds at most this many map entries: the parser's limit, and
// the space the row gives them (playlist_map_index.cpp)
#define T_PLAYLIST_MAP_ENTRIES    210
// Where the player stands, as the title's own `viewpos` command reads it
// (sub_82123638). That command indexes an array of client states; with one local
// player the index is zero, so the pointer to the array is the state. `Valid` is
// the field it checks before printing, and the offsets are its own.
#define T_DATA_ClientStates       0x824C5B64u   // pointer to the client states
#define T_CLIENT_STRIDE           0x000FDE00u
#define T_CLIENT_VALID            13260u
#define T_CLIENT_ORIGIN           437312u       // x, y, z
#define T_CLIENT_ANGLES           453512u       // pitch, yaw, roll

// The title's SPA, its XEX resource named after the title id, as the image
// holds it: the presence strings (presence.cpp).
#define T_DATA_Spa                0x83DA0000u
#define T_DATA_SpaSize            644933u

#elif !defined(MW2_VERSION_TU0)

#define MW2_TITLE_NAME  "campaign"
#define T_ENTRY_POINT   0x82370940u          // the XEX header's ENTRY_POINT

// D3D9
#define T_D3D_Present          820C3390
#define T_D3D_ArenaWait        820B9800
#define T_D3D_ReplayRecording  820C6130
#define T_D3D_InitPixelShader  820B8B58   // a pixel shader object made over a loaded program; see shader_preload.cpp
#define T_D3D_PatchVertexFetches 820B6B10 // rewrites a vertex shader's fetches for a declaration (Bind and two more)

// the job system
#define T_Job_WaitPredicate    823B7840

// the engine
#define T_Com_Error            822830F0
#define T_Com_Printf           82280908
#define T_Cbuf_AddText         8227CF20
#define T_DB_MissingAsset      82172340
#define T_Memcard_InitializeSystem 8230DF90
#define T_Image_FlushMove      823A52F8
#define T_CG_ViewFov           8210FCD8
#define T_CG_CullZoom          8210FF80
#define T_Dvar_RegisterInt     822E1620
// the sizes the title draws at, and the picture's shape (render_size.cpp)
#define T_R_VideoSizes         8238E5D0
#define T_R_SetVidConfig       8238DDC8
#define T_R_DrawStretchPic     823915B0
#define T_UI_EvaluateStatement 822BF4B8
#define T_DATA_VidConfig          0x83C911A8u
#define T_DATA_RenderTargets      0x83E49800u
// network hardening -- the campaign (co-op / Spec Ops) is network-exposed too.
// UI_ReplaceDirective is instruction-identical to the multiplayer's and has the
// same unbounded directive-name buffer (ui_directive_overflow.cpp). The
// joinParty and Huffman overflows are absent from this executable, so no
// entries for them here; this one is present in both.
#define T_UI_ReplaceDirective   822CDAA0
#define T_UI_LookupDirective    822CD9B0
#define T_I_strncpyz            822E7EA0
#define T_Com_PrintMessage      82280B10
// coopLaunch and coopAccept index the two co-op client slots with packet bytes
// (coop_slot_index.cpp); campaign only, the multiplayer has neither.
#define T_CoopLaunch            82339048
#define T_CoopAccept            82339220
// The lobby heartbeat the co-op host sends its partner (hb, from the second
// handler table, 0x82010AC8) indexes the lobby's two member records with a packet
// byte, in a loop that ends only on a 2 (coop_slot_index.cpp).
#define T_CoopHeartbeat         8213A570
#define T_MSG_ReadByte          822881B8
#define T_MSG_ReadString        822884A0
#define T_MSG_ReadLong          82288290
// The co-op map load, called at the end of coopLaunch, queues "map %s\n" with
// coop_mapName, which coopLaunch has just set from the packet: map_name_command.cpp
// cuts the name where the command buffer would end the command.
#define T_Coop_LoadMap          82338CB0
#define T_Dvar_SetString        822E1FB0
// A co-op frame's commands that act for the client its last client-select
// chose; before one, that client is null (coop_frame_client.cpp).
#define T_CoopFrame_ReliableCommand 82339FA8
#define T_CoopFrame_UserCmds        8233A190
// The client's receipt of the host's co-op frames copies each frame from the
// packet without comparing its length with what the packet holds
// (coop_frame_size.cpp).
#define T_Coop_ReceiveFrames        8233BAF0
#define T_Coop_ReadFrameHeader      8233A4C0
#define T_MSG_ReadShort             82288218
// The glass state's piece count is not compared with the 1024-piece table
// (glass_piece_count.cpp).
#define T_GlassState_Parse          820F1D50
#define T_MSG_ReadBit               820F1718
#define T_MSG_ReadBits              820F17B8
#define T_STR_BadGlassPieceIndex    0x82003ACCu   // "Bad glass piece index (%i) in glass state"

// data
#define T_DATA_TimeStampBundlePtr 0x82000780u   // the KeTimeStampBundle import record
#define T_DATA_DebugMonitorPtr    0x820007F8u   // the KeDebugMonitorData import record
#define T_DATA_DeviceTable        0x83A53020u   // Memcard's chosen device per controller
#define T_STR_DirectiveEmpty      0x8203D6CCu   // "Directive empty in string '%s'"
#define T_STR_DirectiveNoEnd      0x8203D698u   // "No end token to match begin token in string '%s'"
// The same from the campaign's `viewpos` (sub_820E1D40), where the states are
// the array itself and not a pointer to it.
#define T_DATA_ClientStates       0x8244C2C0u
// the coop_mapName dvar pointer (map_name_command.cpp)
#define T_DATA_MapNameDvar        0x83A884D0u
#define T_CLIENT_STATES_ARE_HERE  1
#define T_CLIENT_STRIDE           0x0002F1E4u
#define T_CLIENT_VALID            32u
#define T_CLIENT_ORIGIN           133188u       // x, y, z
#define T_CLIENT_ANGLES           149388u       // pitch, yaw, roll

// The title's SPA, its XEX resource named after the title id, as the image
// holds it: the presence strings (presence.cpp).
#define T_DATA_Spa                0x83E60000u
#define T_DATA_SpaSize            644933u

#elif defined(MW2_TITLE_MP)

#define MW2_TITLE_NAME  "multiplayer, disc version"
#define T_ENTRY_POINT   0x823A7FE0u
#define T_D3D_Present          820DFD10
#define T_D3D_ArenaWait        820E1E70
#define T_D3D_ReplayRecording  820E4848
#define T_D3D_InitPixelShader  820D7268
#define T_D3D_PatchVertexFetches 820D5220
#define T_Job_WaitPredicate    823EFBA0
#define T_Com_Error            82281758
#define T_Com_Printf           8227EC30
#define T_Cbuf_AddText         82275470
#define T_Memcard_InitializeSystem 8233CAF0
#define T_Image_FlushMove      823DD658
#define T_CG_ViewFov           8215B9D0
#define T_CG_CullZoom          8215BC50
#define T_Dvar_RegisterInt     8230E2F8
#define T_DATA_RenderTargets      0x83D33D80u
#define T_PartyHost_HandleJoinPartyRequest 821A65C0
#define T_ProtocolVersion      823098A0
#define T_NET_OutOfBandPrint   8228E5E0
#define T_atoi                 8241A118
#define T_DATA_TimeStampBundlePtr 0x820007B4u
#define T_DATA_DebugMonitorPtr    0x820007F4u
#define T_DATA_DeviceTable        0u
#define T_DATA_CmdArgs            0x83002548u
#define T_DATA_ClientStates       0x824C3C24u
#define T_CLIENT_STRIDE           0x000FDC00u
#define T_CLIENT_VALID            13260u
#define T_CLIENT_ORIGIN           437312u
#define T_CLIENT_ANGLES           453512u

// The title's SPA, its XEX resource named after the title id, as the image
// holds it: the presence strings (presence.cpp).
#define T_DATA_Spa                0x83D40000u
#define T_DATA_SpaSize            625298u

#else

#define MW2_TITLE_NAME  "campaign, disc version"
#define T_ENTRY_POINT   0x82370938u
#define T_D3D_Present          820C3390
#define T_D3D_ArenaWait        820B9800
#define T_D3D_ReplayRecording  820C6130
#define T_D3D_InitPixelShader  820B8B58
#define T_D3D_PatchVertexFetches 820B6B10
#define T_Job_WaitPredicate    823B7840
#define T_Com_Error            822830E8
#define T_Com_Printf           82280900
#define T_Cbuf_AddText         8227CF18
#define T_DB_MissingAsset      82172340
#define T_Memcard_InitializeSystem 8230DF88
#define T_Image_FlushMove      823A52F8
#define T_CG_ViewFov           8210FCD8
#define T_CG_CullZoom          8210FF80
#define T_Dvar_RegisterInt     822E1618
#define T_DATA_RenderTargets      0x83E49800u
#define T_DATA_TimeStampBundlePtr 0x82000780u
#define T_DATA_DebugMonitorPtr    0x820007F8u
#define T_DATA_DeviceTable        0x83A53020u
#define T_DATA_ClientStates       0u
#define T_CLIENT_STRIDE           0u
#define T_CLIENT_VALID            0u
#define T_CLIENT_ORIGIN           0u
#define T_CLIENT_ANGLES           0u

// The title's SPA, its XEX resource named after the title id, as the image
// holds it: the presence strings (presence.cpp).
#define T_DATA_Spa                0x83E60000u
#define T_DATA_SpaSize            625298u

#endif

// The macros that turn a table entry into the symbols the recompiled code uses.
// MW2_CAT expands its arguments before pasting, so GUEST_FUNC(T_D3D_Present)
// is sub_820C3390 in the campaign build and sub_820DFD10 in the multiplayer's.
#define MW2_CAT_(a, b) a##b
#define MW2_CAT(a, b) MW2_CAT_(a, b)
#define MW2_STR_(x) #x
#define MW2_STR(x) MW2_STR_(x)
#define GUEST_FUNC(name) MW2_CAT(sub_, name)          // the recompiled function
#define GUEST_ORIG(name) MW2_CAT(__imp__sub_, name)   // the original, once the function is hooked
#define GUEST_ADDR(name) MW2_CAT(0x, name)            // its guest address
#define GUEST_NAME(name) "sub_" MW2_STR(name)         // its name, for a log line
// Declares the original and opens the hook's definition: GUEST_HOOK(T_x) { ... }
#define GUEST_HOOK(name) PPC_FUNC_IMPL(GUEST_ORIG(name)); PPC_FUNC(GUEST_FUNC(name))
