// iw4x: a client can freeze the host with a bogus reliable acknowledge.
//
// The server tracks, per connected client, two counters in the client struct:
// reliableSequence (client+0x20E50), the last reliable command the server queued
// for that client, and reliableAcknowledge (client+0x20E54), the last one the
// client says it received. SV_ExecuteClientMessage stores reliableAcknowledge
// straight off the wire before validating it, and its one range check is signed:
// `reliableSequence - reliableAcknowledge < 128`. A client that sends
// reliableAcknowledge = INT_MAX makes that difference hugely negative, so it
// passes the check, and the bogus value is kept.
//
// Three per-client loops then run `for (i = reliableAcknowledge + 1; i <=
// reliableSequence; i++)`:
//   - SV_UpdateServerCommandsToClient (sub_822C8D68), the ordinary resend of
//     queued server commands, on the regular send path;
//   - SV_ResendReliableCommands (sub_822C8E78), its twin for when that message
//     overflows;
//   - SV_DumpPendingReliableCommands (sub_822C8F50), which Com_Printfs every
//     still-pending command.
// The command index is masked into a 128-entry ring, so none of them read out of
// bounds -- but with reliableAcknowledge = INT_MAX the loop counter starts at
// INT_MIN and runs ~2^31 times, and only the second has an early exit, so the
// server spins and freezes.
//
// The engine keeps the difference in [0, 128]: it drops a client only once it
// reaches 129 (sub_822C1608: `subf r8,ack,seq; cmpwi r8,129; bne <no drop>`), so
// a slow-but-legitimate client can legitimately sit at 128 pending commands. So
// before each of the three loops runs, reset reliableAcknowledge to
// reliableSequence whenever the difference is outside [0, 128] -- the same thing
// the engine's own guard does when it rejects a bad acknowledge, and a no-op for
// any client within the range the engine itself allows. The subtraction is done
// on unsigned values: an attacker's acknowledge is near INT_MIN/INT_MAX, where a
// signed subtraction would be undefined overflow.
//
// Multiplayer only: the campaign image has no twin of these functions.

use crate::guest::{Memory, Register, guest_hook};
use crate::log::log_warn;
use crate::title::{T_SV_DumpPendingReliableCommands, T_SV_ResendReliableCommands, T_SV_UpdateServerCommandsToClient};
use std::sync::Once;

const RELIABLE_SEQUENCE: u32 = 0x20E50; // client->reliableSequence
const RELIABLE_ACKNOWLEDGE: u32 = 0x20E54; // client->reliableAcknowledge

static LOGGED: Once = Once::new();

fn sanitize_reliable_acknowledge(memory: Memory, client: u32) {
    let sequence = memory.u32_at(client.wrapping_add(RELIABLE_SEQUENCE));
    let acknowledge = memory.u32_at(client.wrapping_add(RELIABLE_ACKNOWLEDGE));
    // Unsigned difference: a value in [0, 128] is left alone; anything larger --
    // including a negative difference (ack ahead of seq), which wraps to a huge
    // unsigned value -- is reset.
    if sequence.wrapping_sub(acknowledge) <= 128 {
        return;
    }
    memory.store_u32(client.wrapping_add(RELIABLE_ACKNOWLEDGE), sequence);
    LOGGED.call_once(|| {
        log_warn!("online: a client acknowledged a command the server never sent; its acknowledge was reset")
    });
}

// Each takes the client in r3.
guest_hook!(T_SV_UpdateServerCommandsToClient, |ctx, memory| {
    sanitize_reliable_acknowledge(memory, ctx.r3.as_u32());
    T_SV_UpdateServerCommandsToClient.original(ctx, memory);
});

guest_hook!(T_SV_ResendReliableCommands, |ctx, memory| {
    sanitize_reliable_acknowledge(memory, ctx.r3.as_u32());
    T_SV_ResendReliableCommands.original(ctx, memory);
});

guest_hook!(T_SV_DumpPendingReliableCommands, |ctx, memory| {
    sanitize_reliable_acknowledge(memory, ctx.r3.as_u32());
    T_SV_DumpPendingReliableCommands.original(ctx, memory);
});
