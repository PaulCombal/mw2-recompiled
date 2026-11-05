// The reason a server disconnects a client with is used as a print format.
//
// The server drops a client with the server command 'w' and a reason, normally
// a localisation key (EXE_SERVERQUIT, EXE_PLAYERKICKED...). The client passes
// it to sub_82184918, which handles the keys it knows and treats any other
// reason, and any beginning with EXE_PLAYERKICKED, alike:
//
//     text = UI_ReplaceReason(Localize("EXE_SERVERDISCONNECTREASON"), Localize(reason))
//     Com_Error(2, text)
//
// The localiser hands an unknown key back inside its result, so the reason ends
// up in the text, and the text is Com_Error's format. A stock server cannot put
// a '%' in it: it stores every server command through sub_8228F9D0, which turns
// '%' into '.'. But the client reads server commands without such a filter
// (the connectionless reader has one), so a host running a modified game can
// have whatever '%' directives it puts in a reason interpreted on the client,
// with no arguments behind them. On a console '%n' stops the title (the
// C library refuses it and its invalid-parameter path ends in a trap) and '%s'
// reads through whatever pointer follows; here the trap is not emitted and every
// guest read succeeds, so the message comes out garbled.
//
// Those two paths are done here instead, with the same calls in the same order,
// and the text is raised through "%s". Every other reason goes to the original
// function untouched. The keys are compared with the title's own I_stricmp and
// I_strnicmp, in the original's order.
//
// Multiplayer only: the campaign's disconnect handler raises literal messages.

use crate::guest::{GuestFunction, Memory, PPCContext, Register, guest_hook, preserving};
use crate::hooks::com_error::{Level, com_error};
use crate::title::*;

// A frame of our own for the calls: Com_Error is variadic and spills its
// register arguments into its caller's frame.
const FRAME: u32 = 0x80;

fn call(ctx: &mut PPCContext, memory: Memory, function: GuestFunction, r3: u32, r4: u32, r5: u32) -> u32 {
    ctx.r3.u64 = u64::from(r3);
    ctx.r4.u64 = u64::from(r4);
    ctx.r5.u64 = u64::from(r5);
    function.call(ctx, memory);
    ctx.r3.as_u32()
}

// sub_82184918(reason = r3)
guest_hook!(T_CL_DisconnectReason, |ctx, memory| {
    let reason = ctx.r3.as_u32();

    // Comes back only for a reason the original handles. The rest ends in Com_Error.
    preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(FRAME));
        let same = |ctx: &mut PPCContext, key| call(ctx, memory, T_I_stricmp, key, reason, 0) == 0;

        // The keys the original handles before either generic path, in its order.
        if same(ctx, T_STR_EXE_MATCHENDED)
            || same(ctx, T_STR_EXE_SERVERQUIT)
            || same(ctx, T_STR_EXE_MIGRATIONABORTED)
            || same(ctx, T_STR_EXE_MIGRATION_IN_PROGRESS)
            || same(ctx, T_STR_hostquit)
        {
            return;
        }
        let kicked = call(ctx, memory, T_I_strnicmp, T_STR_EXE_PLAYERKICKED, reason, 16) == 0;
        // Past the kicked prefix, two more keys the original handles itself; it
        // compares the second after the party call below, which it then makes too.
        if !kicked && (same(ctx, T_STR_roundsdone) || same(ctx, T_STR_XBOXLIVE_SIGNINCHANGED)) {
            return;
        }

        let party = T_DATA_PartyForDisconnect;
        if kicked {
            call(ctx, memory, T_Party_StopParty, party, 0, 0);
        } else {
            let session = memory.u32_at(memory.u32_at(party).wrapping_add(1440));
            call(ctx, memory, T_Party_OnDisconnect, party, session, 0);
        }

        let localized = call(ctx, memory, T_SEH_LocalizeTextMessage, reason, 0, 0);
        let message = call(ctx, memory, T_SEH_LocalizeTextMessage, T_STR_EXE_SERVERDISCONNECTREASON, 0, 0);
        let text = call(ctx, memory, T_UI_ReplaceReason, message, localized, 0);
        ctx.r5.u64 = u64::from(text);
        com_error(ctx, memory, Level::Disconnect, T_STR_PercentS)
    });
    T_CL_DisconnectReason.original(ctx, memory);
});
