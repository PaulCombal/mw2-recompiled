// The glass state's piece count must fit the 1024-piece table it fills.
//
// The glass state (sub_820F1D50) fills a table of 1024 5-byte pieces
// (0x82437EB0, 5120 bytes; sub_820F1FD8 clears it) in one of two forms, chosen
// by its second bit:
//
//   1  pieces by index: an 11-bit index, refused above 1024 with the title's
//      own Com_Error (ERR_FATAL, "Bad glass piece index (%i) in glass state")
//      and ending the list at 1024
//   0  a run of pieces from 0: an 11-bit count, then that many pieces in order
//
// The run's count is never compared with 1024, so a count up to 2047 writes
// pieces past the table, over the fields after it (+5124..+5132) and into the
// dvar pointers that follow (0x824392C0, sub_820F1FF8).
//
// No packet carries this message: the client parses a glass state the same
// process has just written from its own server's state (sub_82340BD0 ->
// sub_82340668, whose glass part is sub_821F4210, which writes no more pieces
// than there are). So this is hardening, not a remote fix: a run longer than
// the table is refused the way the title refuses the other form, with the same
// error, for the index the run would reach.
//
// The three bits are read with the title's own MSG_ReadBit / MSG_ReadBits, and
// the message's read state (+0 overflow, +28 byte, +32 bit), the only fields
// they write, is put back before the parse runs.
//
// Campaign only: the multiplayer's glass state is other code.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::hooks::com_error::{Level, com_error};
use crate::log::log_error;
use crate::title::{T_GlassState_Parse, T_MSG_ReadBit, T_MSG_ReadBits, T_STR_BadGlassPieceIndex};

const PIECES: u32 = 1024;
const COUNT_BITS: u64 = 11;
const OVERFLOWED: u32 = 0; // msg_t fields the bit readers write
const READ_COUNT: u32 = 28;
const BIT: u32 = 32;

// The run's count, or 0 when the message holds the other form, nothing, or ran
// out before the count was read whole: the run's loop then writes no piece
// (820F1E34 stops on the overflow flag).
fn run_length(ctx: &mut PPCContext, memory: Memory, msg: u32) -> u32 {
    preserving(ctx, |ctx| {
        let overflowed = memory.u32_at(msg.wrapping_add(OVERFLOWED));
        let read_count = memory.u32_at(msg.wrapping_add(READ_COUNT));
        let bit = memory.u32_at(msg.wrapping_add(BIT));
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(0x80));

        let mut count = 0;
        ctx.r3.u64 = u64::from(msg);
        T_MSG_ReadBit.call(ctx, memory);
        if ctx.r3.as_u32() != 0 {
            ctx.r3.u64 = u64::from(msg);
            T_MSG_ReadBit.call(ctx, memory);
            if ctx.r3.as_u32() == 0 {
                ctx.r3.u64 = u64::from(msg);
                ctx.r4.u64 = COUNT_BITS;
                T_MSG_ReadBits.call(ctx, memory);
                count = ctx.r3.as_u32();
            }
        }
        if memory.u32_at(msg.wrapping_add(OVERFLOWED)) != 0 {
            count = 0;
        }

        memory.store_u32(msg.wrapping_add(OVERFLOWED), overflowed);
        memory.store_u32(msg.wrapping_add(READ_COUNT), read_count);
        memory.store_u32(msg.wrapping_add(BIT), bit);
        count
    })
}

// (msg = r3)
guest_hook!(T_GlassState_Parse, |ctx, memory| {
    let count = run_length(ctx, memory, ctx.r3.as_u32());
    if count > PIECES {
        log_error!("glass state: a run of %u pieces, more than the table's %u", count, PIECES);
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(0x80));
        ctx.r5.u64 = u64::from(count - 1); // the last index the run would write
        com_error(ctx, memory, Level::Fatal, T_STR_BadGlassPieceIndex); // as for a bad index
    }
    T_GlassState_Parse.original(ctx, memory);
});
