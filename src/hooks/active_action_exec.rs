// iw4x: a host can run console commands on a joining client through the
// activeAction dvar -- never run it.
//
// CL_RunActiveAction (sub_82171790) is run once from the client's connection
// state machine (sub_821718B0) as the client goes active. After its other
// set-up work it reads the activeAction dvar (pointer at 0x825089B8, current
// string at dvar+12) and, if the string is not empty, passes it to Cbuf_AddText
// with a fixed follow-up line, then sets the dvar back to "" with
// Dvar_SetString. The host sets that value: the client's set-dvar server
// command (sub_82154738, from CG_ServerCommand) hands every name but three to
// Dvar_SetFromStringByName with no name filter -- the client side of the
// script's setClientDvar. So the host of a game a player joins (a mod, or a
// crafted server command) can have any console command run on that player's
// copy.
//
// The fix is at the reader, not the setter: activeAction is registered with
// flags 0, so no flag test on the set-dvar path could tell it apart from the
// dvars a host legitimately sets. iw4x does the same -- it turns the reader's
// "not empty" branch into an unconditional skip (Set<BYTE>(0x5A1D43, 0xEB)),
// so the value is never executed. Whether the set-dvar command should refuse
// other names -- or honour write-protect flags for this source -- is a
// separate question, not answered here.
//
// The recompiled function cannot be branch-patched, so the hook clears the dvar
// before the original runs: the original then finds it empty and skips the
// execute-and-clear block, and everything else it does is unchanged. The clear
// goes through the title's own Dvar_SetString with the "" literal the original
// itself clears with, rather than writing a 0 into the string: dvar strings are
// interned script strings (CopyStringInternal -> SL_GetString_), shared by
// every user of the same text, so truncating one in place would blank the
// others too. No call the original makes ahead of the read reaches the
// set-dvar handler or CG_ServerCommand (direct calls, by the call graph), so the
// host cannot set the value again between the clear and the read.
// The one difference from the original: when the function takes its early exit
// (a client-state bit it tests first), the dvar is cleared now rather than left
// for a later call -- which only discards a value that would never be run.
// MP-only: the campaign has no twin.

use crate::guest::{guest_hook, preserving};
use crate::log::log_warn;
use crate::title::{T_CL_RunActiveAction, T_DATA_ActiveActionDvar, T_DATA_EmptyString, T_Dvar_SetString};
use std::sync::Once;

// "Zero when not known" in the table: here that stops the build.
const _: () = assert!(T_DATA_ActiveActionDvar != 0 && T_DATA_EmptyString != 0);

static LOGGED: Once = Once::new();

guest_hook!(T_CL_RunActiveAction, |ctx, memory| {
    // Guard both pointers rather than lean on a null read returning 0 here.
    if let Some(dvar) = memory.pointer_at(T_DATA_ActiveActionDvar)
        && let Some(value) = memory.pointer_at(dvar.wrapping_add(12))
        && memory.u8_at(value) != 0
    {
        // Log once -- activeAction registers empty, so a value here is
        // normally the host's (an attack or a mod).
        LOGGED.call_once(|| log_warn!("online: not running activeAction set by the host (activeAction is disabled)"));

        preserving(ctx, |ctx| {
            ctx.r3.u64 = u64::from(dvar);
            ctx.r4.u64 = u64::from(T_DATA_EmptyString);
            T_Dvar_SetString.call(ctx, memory);
        });
    }
    T_CL_RunActiveAction.original(ctx, memory);
});
