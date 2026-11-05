// A map name a remote peer chose is pasted unquoted into a "map" command line:
// cut it where the command buffer would end the command, so nothing after it
// runs as a second command.
//
// Three title functions queue a line built from a map-name dvar's current
// string (dvar+12) with Cbuf_AddText:
//   multiplayer  the party frame (sub_8219FBB8), on the party host once a start
//                is pending:          va("map %s 1 %i\n", ui_mapname, 0)
//   multiplayer  the menu script runner (sub_822E2090), verb "StartServer"
//                (systemlink_startmatch, uiscript_startserver):
//                                     va("map %s\n", ui_mapname)
//   campaign     the co-op map load (sub_82338CB0), called at the end of
//                coopLaunch:          va("map %s\n", coop_mapName)
// The dvars are written from the network without any character filter:
// ui_mapname by a party host's partystate (PartyClient_HandlePartyState ->
// sub_82198D80, up to 1024 bytes; the reader only maps byte 0x92 to ') and by
// the set-dvar server command; coop_mapName by the co-op host's coopLaunch
// (sub_82339048, 256 bytes, MSG_ReadString copying every byte). The party
// start (sub_821A1FB8) picks no map; it launches whatever ui_mapname holds.
// So a co-op host can run any console command on its partner as the partner
// loads the map, and a party host's text would run on a member that later
// starts the match as host.
//
// The rule is Cbuf_Execute's own (its line splitter sub_82277C38): a command
// ends at '\n' or '\r', and at ';' when an even number of '"' come before it.
// The text ahead of the name ("map ") holds no quote, so applying the rule to
// the name alone finds the same place. A map name is a fastfile name and holds
// none of these, so a stock name is never changed. The cut keeps the first
// command the original would have run, plus the line's own trailing arguments.
//
// The hooks run ahead of each original and, only when the name holds such an
// end, set the dvar to the text before it with the title's Dvar_SetString (dvar
// strings are interned and shared, so they are never written in place). The
// new text goes just below the guest stack pointer, with the call made at a
// stack pointer pushed past it, as console.cpp does; Dvar_SetString copies it.
// No call ahead of the read in any of the three reaches a network handler that
// could write the dvar again.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::log_warn;
use crate::title::*;
use std::sync::Once;

static LOGGED: Once = Once::new();

// Where Cbuf_Execute would end the command that starts with the name.
fn command_end(memory: Memory, name: u32) -> Option<u32> {
    let mut quoted = false;
    for i in 0..=u32::MAX {
        let c = memory.u8_at(name.wrapping_add(i));
        match c {
            0 => return None,
            b'"' => quoted = !quoted,
            b';' if !quoted => return Some(i),
            b'\n' | b'\r' => return Some(i),
            _ => {}
        }
    }
    None
}

fn cut_map_name(ctx: &mut PPCContext, memory: Memory) {
    let Some(dvar) = memory.pointer_at(T_DATA_MapNameDvar) else { return };
    let Some(name) = memory.pointer_at(dvar.wrapping_add(12)) else { return };
    let Some(end) = command_end(memory, name) else { return };

    let sp = ctx.r1.as_u32();
    if sp < 0x2000 {
        return;
    }
    LOGGED.call_once(|| log_warn!("online: cut a map name received from the network at a command separator"));

    let scratch = sp.wrapping_sub(0x100).wrapping_sub(end).wrapping_sub(1) & !0xF;
    memory.copy(scratch, name, end);
    memory.store_u8(scratch.wrapping_add(end), 0);

    preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(scratch.wrapping_sub(0x100) & !0xF);
        ctx.r3.u64 = u64::from(dvar);
        ctx.r4.u64 = u64::from(scratch);
        T_Dvar_SetString.call(ctx, memory);
    });
}

#[cfg(T_Party_Frame)]
guest_hook!(T_Party_Frame, |ctx, memory| {
    cut_map_name(ctx, memory);
    T_Party_Frame.original(ctx, memory);
});

#[cfg(T_UI_RunMenuScript)]
guest_hook!(T_UI_RunMenuScript, |ctx, memory| {
    cut_map_name(ctx, memory);
    T_UI_RunMenuScript.original(ctx, memory);
});

#[cfg(T_Coop_LoadMap)]
guest_hook!(T_Coop_LoadMap, |ctx, memory| {
    cut_map_name(ctx, memory);
    T_Coop_LoadMap.original(ctx, memory);
});
