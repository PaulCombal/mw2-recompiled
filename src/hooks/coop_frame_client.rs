// A co-op frame's commands that act for a client must come after one that
// says which client.
//
// The host relays every co-op frame: the inputs of both players, which each
// machine then runs in the same order (sub_8233A538, on the host and the
// client alike). A frame is a run of commands, each a 3-bit type:
//
//   3  select a client: a 1-bit slot, which must be 0 or 1 and in use
//   1  the selected client's usercmds (sub_8233A190)
//   2  the selected client's reliable command (sub_82339FA8, through
//      sub_8233A090, which reads the type)
//   4  end
//
// The selected client starts out null (sub_8233A538, 8233A6A8) and only a 3
// sets it, so a 1 or a 2 that comes first runs with a null client: the
// reliable command reads and writes the client's fields at guest 0x1218 and
// up, a 1024-byte Com_sprintf among them, and both hand a client number
// computed from the null pointer (361817) to code that indexes the entities
// and clients with it, unbounded. On the console the first access faults; in
// this recomp guest page 0 is memory like any other, and the writes land.
//
// A host's frames always select first: it builds each player's part of the
// frame as a 3 with the sender's slot followed by that player's own commands
// (sub_8233AA50, 8233ABC8). So a 1 or a 2 with no client selected cannot have
// come from a host, and the frame is refused the way the title refuses a
// malformed one: the message is marked overflowed and the loop in
// sub_8233A538 stops at it, as it does at the end of a message.
//
// Campaign only: the multiplayer has no co-op frames.

use crate::guest::{Memory, Register, guest_hook};
use crate::log::{Doubling, log_warn};
use crate::title::{T_CoopFrame_ReliableCommand, T_CoopFrame_UserCmds};
use std::ffi::CStr;

const OVERFLOWED: u32 = 0; // msg_t

static REFUSED: Doubling = Doubling::new();

fn refuse(memory: Memory, msg: u32, what: &CStr) {
    memory.store_u32(msg.wrapping_add(OVERFLOWED), 1);
    if let Some(count) = REFUSED.count() {
        log_warn!(
            "online: dropped a co-op frame whose %s came before any client was selected (%u so far)",
            what,
            count
        );
    }
}

// (client = r3, msg = r4) -> nonzero to go on reading the frame
guest_hook!(T_CoopFrame_ReliableCommand, |ctx, memory| {
    if ctx.r3.as_u32() == 0 {
        refuse(memory, ctx.r4.as_u32(), c"reliable command");
        ctx.r3.u64 = 0;
        return;
    }
    T_CoopFrame_ReliableCommand.original(ctx, memory);
});

// (client = r3, msg = r4)
guest_hook!(T_CoopFrame_UserCmds, |ctx, memory| {
    if ctx.r3.as_u32() == 0 {
        refuse(memory, ctx.r4.as_u32(), c"usercmds");
        return;
    }
    T_CoopFrame_UserCmds.original(ctx, memory);
});
