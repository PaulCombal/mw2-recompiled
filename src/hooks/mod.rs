// The title's functions replaced from Rust. Each module is the C++ file of its
// name; those stay for the C++ build, and tools/hook_diff.py compares each pair.
//
// A hook is safe code. It can hold no Vec, Box or lock guard across a call into
// the guest: a longjmp there would skip the frame, and its destructor with it.
// Nor can it call the guest inside a Once or a LazyLock: skipped, it stays in
// progress, and the next thread to reach it waits for ever.

#[cfg(T_CL_RunActiveAction)]
mod active_action_exec;
#[cfg(all(T_CL_PacketEvent, T_CL_ConnectionlessPacket, T_NET_CompareAdr))]
mod client_packet_sender;
#[cfg(any(T_GlassState_Parse, T_G_LoadMigratedGame, T_CL_DisconnectReason, T_MemFile_Read))]
mod com_error;
#[cfg(T_CG_SetClientDvarFromServer)]
mod command_dvars_from_host;
#[cfg(all(T_CoopFrame_ReliableCommand, T_CoopFrame_UserCmds))]
mod coop_frame_client;
#[cfg(all(T_Coop_ReceiveFrames, T_Coop_ReadFrameHeader, T_MSG_ReadByte, T_MSG_ReadShort))]
mod coop_frame_size;
#[cfg(all(
    T_CoopLaunch,
    T_CoopAccept,
    T_CoopHeartbeat,
    T_MSG_ReadByte,
    T_MSG_ReadBit,
    T_MSG_ReadString,
    T_MSG_ReadLong
))]
mod coop_slot_index;
#[cfg(all(
    T_UI_LoadArenas,
    T_UI_ParseInfos,
    T_LargeLocal_Begin,
    T_LargeLocal_GetBuf,
    T_LargeLocal_End,
    T_DATA_UI_ArenaInfos,
    T_DB_FindXAssetHeader,
    T_SEH_StringEd_GetString,
    T_Com_LoadUiFastFile,
    T_DB_LoadXAssets,
    T_DATA_DB_ZoneCount
))]
mod custom_map_list;
#[cfg(all(mw2_owns_d3d_hooks, T_D3D_Present, T_D3D_ArenaWait))]
mod d3d_hooks;
#[cfg(T_CL_DisconnectReason)]
mod disconnect_reason_format;
mod engine_log;
#[cfg(all(T_CG_ViewFov, T_CG_CullZoom))]
mod field_of_view;
#[cfg(all(T_GlassState_Parse, T_MSG_ReadBit, T_MSG_ReadBits, T_STR_BadGlassPieceIndex, T_Com_Error))]
mod glass_piece_count;
#[cfg(all(T_MSG_ReadDeltaHudElems, T_MSG_ReadDeltaField, T_MSG_ReadBit, T_MSG_ReadBits))]
mod hud_elem_field_index;
#[cfg(all(T_MSG_ReadBitsCompress, T_Huffman_DecodeSymbol))]
mod huffman_overflow;
#[cfg(all(mw2_owns_image_move, T_Image_FlushMove))]
mod image_move;
#[cfg(T_PartyHost_HandleJoinPartyRequest)]
mod joinparty_overflow;
#[cfg(T_DATA_MapNameDvar)]
mod map_name_command;
#[cfg(all(T_G_LoadMigratedGame, T_MemFile_BeginSegment, T_Com_Error, T_DATA_MigrationHeader, T_STR_CorruptedFile))]
mod migration_state_size;
#[cfg(all(T_ProtocolVersion, T_NET_OutOfBandPrint))]
mod network_version;
#[cfg(all(T_Session_Create, T_Session_Modify))]
mod party_slot_count;
#[cfg(all(T_Playlist_SetMapEntry, T_Playlist_UnplayHistory))]
mod playlist_map_index;
#[cfg(all(feature = "diagnostics", T_Job_WaitPredicate))]
mod predicate_waits;
#[cfg(T_CL_HandleRelayPacket)]
mod relay_reflection;
#[cfg(all(T_R_VideoSizes, T_R_SetVidConfig, T_R_DrawStretchPic, T_UI_EvaluateStatement, T_DATA_VidConfig))]
mod render_size;
#[cfg(all(
    T_MemFile_Read,
    T_Scr_SavedObject,
    T_Scr_ResolveObjectRef,
    T_RET_Scr_ReadObjectRef,
    T_RET_Scr_ReadObjectCount,
    T_RET_Scr_ReadObjectType,
    T_RET_Scr_ReadCodePos,
    T_RET_Scr_ReadResumePos,
    T_DATA_ScrProgramStart,
    T_DATA_ScrProgramEnd,
    T_RET_G_ReadEntityPairCount,
    T_Com_Error,
    T_STR_CorruptedFile
))]
mod script_save_ids;
#[cfg(all(
    T_MSG_ReadDeltaEntity,
    T_MSG_ReadDeltaPlayerState,
    T_MSG_ReadDeltaFields,
    T_MSG_ReadFieldGap,
    T_MSG_EntityTypeFields,
    T_MSG_ReadDeltaField,
    T_MSG_ReadDeltaHudElems,
    T_MSG_ReadBit,
    T_MSG_ReadBits,
    T_MSG_Discard,
    T_MSG_ReadByte,
    T_MSG_ReadShort,
    T_CL_FindSentOrigin,
    T_BG_GetEntityTypeName,
    T_Com_Printf,
    T_Com_PrintMessage
))]
mod snapshot_delta_field_index;
#[cfg(all(T_CL_DeltaEntity, T_CL_ParsePacketEntities))]
mod snapshot_entity_count;
#[cfg(all(T_SV_CheckCommandTime, T_va, T_Com_PrintMessage))]
mod sv_command_time_format;
#[cfg(all(T_SV_UpdateServerCommandsToClient, T_SV_ResendReliableCommands, T_SV_DumpPendingReliableCommands))]
mod sv_reliable_freeze;
#[cfg(T_Dvar_RegisterInt)]
mod texture_filtering;
#[cfg(all(T_UI_ReplaceDirective, T_UI_LookupDirective, T_I_strncpyz, T_Com_PrintMessage))]
mod ui_directive_overflow;
#[cfg(T_DB_StartStream)]
mod unsigned_fastfiles;

/// Recordings D3D9 could not have walked to the end of, in the GPU's end-of-run report
/// (runtime/gpu/gpu.rs).
#[cfg(all(feature = "diagnostics", feature = "rust-renderer"))]
pub fn report_d3d_hooks() {
    #[cfg(all(mw2_owns_d3d_hooks, T_D3D_Present, T_D3D_ArenaWait))]
    d3d_hooks::report();
}

/// The predicate waits' end-of-run report (runtime/run.rs).
#[cfg(feature = "diagnostics")]
pub fn report_predicate_waits() {
    #[cfg(T_Job_WaitPredicate)]
    predicate_waits::mw2_engine_report_predicate_waits();
}
