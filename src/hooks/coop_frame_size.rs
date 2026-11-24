// A co-op frame the host sends must lie inside the packet that carries it.
//
// The client takes the host's frames in sub_8233BAF0: a byte, the number of
// frames (1..20), and for each frame a short, its length, followed by the
// frame itself, whose first bytes are its header (sub_8233A4C0: three longs and
// a byte). A frame the client keeps is copied whole, from the packet at the
// position after its length, into one of twenty 4096-byte frame buffers
// (MSG_WriteData, at 8233BD0C), and the next frame is read from that position
// plus the length (8233BDA0). The destination is bounded -- MSG_WriteData
// refuses more than 4096 -- but nothing compares the length with what is left
// of the packet, so the copy reads up to 4 KB past the packet's end: past the
// datagram into the rest of the receive buffer, and past the buffer's 16 KB
// into whatever follows it. Those bytes then run as the frame's commands.
//
// The host writes each frame as its length, the frame's own cursize, followed
// by exactly that many bytes (sub_8233AE80, 8233B0F8..8233B10C). A host's
// packet holds less than a length says only when the packet filled up:
// MSG_WriteShort and MSG_WriteData check for room and not for the overflow
// flag, and the sender's loop never looks at it, so a frame that does not fit
// leaves its length without its bytes, and a later, smaller frame that does fit
// still follows. The client then reads that frame's header out of the next
// frame and, should it match, copies past the packet's end -- a host can cause
// the over-read without meaning to.
//
// So the packet is cut, not dropped: at the first frame whose length runs past
// the end of the packet, its cursize (+20) is set to where that frame's length
// starts. sub_8233BAF0 then takes the frames before it, which are whole, and
// stops at the next length, as it does at the end of any truncated packet;
// what the host meant to send past that point was never in the packet.
//
// The packet is walked with the title's own MSG_ReadByte, MSG_ReadShort and
// header reader, stopping where sub_8233BAF0 stops (a length of 0 or less, a
// read past the end, a header that does not read); the message's read position
// (+28) and overflow flag (+0), the only fields those routines write, are put
// back afterwards. Whether sub_8233BAF0 then keeps or skips a frame (another
// server, an old frame, a full ring) does not change where its bytes are, so
// the walk does not need to know. When sub_8233BAF0 ignores the packet without
// reading it (the byte at 0x83A74289, 8233BB04), the walk is skipped too.
//
// Campaign only: the multiplayer has no co-op frames.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::{Doubling, log_warn};
use crate::title::{T_Coop_ReadFrameHeader, T_Coop_ReceiveFrames, T_MSG_ReadByte, T_MSG_ReadShort};

const MAX_FRAMES: i32 = 20; // sub_8233BAF0's own bound
const OVERFLOWED: u32 = 0; // msg_t
const CUR_SIZE: u32 = 20;
const READ_COUNT: u32 = 28;
const IGNORE_FRAMES: u32 = 0x83A74289; // sub_8233BAF0 returns at once when set

// A frame of our own below the caller's, with room for the header's four results.
const OUT: u32 = 0x80;
const FRAME: u32 = 0x100;

struct Overrun {
    at: u32, // the position of the frame's length
    length: i32,
    left: i32,
}

// The frames' lengths, read as sub_8233BAF0 reads them, with the message's read
// state put back afterwards: the first frame whose header reads but whose length
// runs past the end of the packet.
fn first_overrun(ctx: &mut PPCContext, memory: Memory, msg: u32) -> Option<Overrun> {
    preserving(ctx, |ctx| {
        let overflowed = memory.u32_at(msg.wrapping_add(OVERFLOWED));
        let read_count = memory.u32_at(msg.wrapping_add(READ_COUNT));
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(FRAME));
        let out = ctx.r1.as_u32().wrapping_add(OUT);

        let mut overrun = None;
        ctx.r3.u64 = u64::from(msg);
        T_MSG_ReadByte.call(ctx, memory);
        let frames = ctx.r3.as_i32();
        for _ in 0..if frames <= MAX_FRAMES { frames } else { 0 } {
            let length_at = memory.u32_at(msg.wrapping_add(READ_COUNT));
            ctx.r3.u64 = u64::from(msg);
            T_MSG_ReadShort.call(ctx, memory);
            let length = ctx.r3.as_i32();
            if length <= 0 || memory.u32_at(msg.wrapping_add(OVERFLOWED)) != 0 {
                break;
            }
            let start = memory.u32_at(msg.wrapping_add(READ_COUNT)) as i32;
            ctx.r3.u64 = u64::from(msg);
            ctx.r4.u64 = u64::from(out);
            ctx.r5.u64 = u64::from(out.wrapping_add(4));
            ctx.r6.u64 = u64::from(out.wrapping_add(8));
            ctx.r7.u64 = u64::from(out.wrapping_add(12));
            T_Coop_ReadFrameHeader.call(ctx, memory);
            if ctx.r3.as_u32() & 0xFF == 0 {
                break;
            }
            let left = (memory.u32_at(msg.wrapping_add(CUR_SIZE)) as i32).wrapping_sub(start);
            if length > left {
                overrun = Some(Overrun { at: length_at, length, left });
                break;
            }
            memory.store_u32(msg.wrapping_add(READ_COUNT), start.wrapping_add(length) as u32);
        }

        memory.store_u32(msg.wrapping_add(OVERFLOWED), overflowed);
        memory.store_u32(msg.wrapping_add(READ_COUNT), read_count);
        overrun
    })
}

static CUT: Doubling = Doubling::new();

// (msg = r3), the host's frames after the type and direction bytes
guest_hook!(T_Coop_ReceiveFrames, |ctx, memory| {
    let msg = ctx.r3.as_u32();
    if memory.u8_at(IGNORE_FRAMES) == 0
        && let Some(overrun) = first_overrun(ctx, memory, msg)
        && overrun.at != 0
    {
        memory.store_u32(msg.wrapping_add(CUR_SIZE), overrun.at);
        if let Some(count) = CUT.count() {
            log_warn!(
                "online: cut co-op frames from the host before a %d-byte frame with %d bytes left in the packet (%u so far)",
                overrun.length,
                overrun.left,
                count
            );
        }
    }
    T_Coop_ReceiveFrames.original(ctx, memory);
});
