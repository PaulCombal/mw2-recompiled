// CVE-2019-20893 -- PartyHost_HandleJoinPartyRequest.
//
// The "joinParty" out-of-band packet carries, as its sixth token (argv[5]), a
// count of subparties. The handler (sub_821A6950) checks only that the count is
// >= 1, then loops that many times, each iteration storing one attacker-supplied
// int into a fixed array on its own 272-byte stack frame (base r1+80). A count
// past the array -- anything over the engine's 18-client ceiling -- runs the
// loop off the frame, across the saved registers and the return address: a
// remote, pre-authentication stack smash on whoever is hosting a party or lobby.
// Reportedly found on the Xbox 360 first; iw4x clamps the count to 18.
//
// The packet reaches this handler with no sender authentication (the party
// dispatcher does not check who sent it), so on the lan backend anyone on the
// network can send it, and on steam any member of the host's lobby.
//
// No legitimate joinParty ever names more than 18 subparties -- there are at
// most 18 clients -- so the hook reads the same count the handler is about to
// parse, argv[5] of the local client's command args, and drops the packet when
// it exceeds 18 before the original runs. A count in [1, 18] passes through
// untouched; a count below 1 is left to the handler's own rejection. Dropping an
// out-of-range request is how the engine itself treats a bad count here (it
// already rejects "< 1 subparties"), and for every packet a real client sends it
// is indistinguishable from iw4x's clamp.
//
// The count is parsed by the title's own atoi, the call the handler makes, so
// the hook and the handler cannot disagree on a token: that atoi saturates at
// 0x7FFFFFFF where a 64-bit host's strtol, cut to 32 bits, turns 4294967297
// into 1.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::log_warn;
use crate::title::{T_DATA_CmdArgs, T_PartyHost_HandleJoinPartyRequest, T_atoi};
use std::sync::Once;

const MAX_CLIENTS: i32 = 18; // the engine's sv_maxclients ceiling

static LOGGED: Once = Once::new();

// argv[5] of the current local client's tokenized command, parsed the way the
// handler does: args[+0] is the local client index; args[+68 + i*4] is client i's
// argc; args[+100 + i*4] is the pointer to client i's argv array.
fn subparty_count(ctx: &mut PPCContext, memory: Memory) -> i32 {
    let client = memory.u32_at(T_DATA_CmdArgs).wrapping_mul(4);
    let argc = memory.u32_at(T_DATA_CmdArgs.wrapping_add(68).wrapping_add(client));
    if argc <= 5 {
        return 0; // no argv[5]; the handler reads "" -> 0
    }
    let argv = memory.u32_at(T_DATA_CmdArgs.wrapping_add(100).wrapping_add(client));

    // The hook runs at the handler's entry, so the stack below r1 is free for the
    // call, as it is for the handler's own.
    preserving(ctx, |ctx| {
        ctx.r3.u64 = u64::from(memory.u32_at(argv.wrapping_add(5 * 4)));
        T_atoi.call(ctx, memory);
        ctx.r3.as_i32()
    })
}

guest_hook!(T_PartyHost_HandleJoinPartyRequest, |ctx, memory| {
    let count = subparty_count(ctx, memory);
    if count > MAX_CLIENTS {
        // once: whoever sends one can send thousands
        LOGGED.call_once(|| {
            log_warn!(
                "online: dropped a joinParty naming %d subparties (max %d) -- CVE-2019-20893",
                count,
                MAX_CLIENTS
            );
        });
        return;
    }
    T_PartyHost_HandleJoinPartyRequest.original(ctx, memory);
});
