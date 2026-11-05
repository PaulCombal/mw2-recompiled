// A co-op partner's packet indexes the two co-op client slots without a bound.
//
// The campaign's connectionless dispatcher (sub_8233BEB0) hands five co-op
// commands to handlers from a {name, handler} table (0x8204A188, dispatched by
// sub_823399C8). Nothing there compares the sender's address; the only gate is
// that co-op is on (the byte at 0x825A71A0). Two of the handlers take indexes
// straight from the packet:
//
// coopLaunch (sub_82339048), sent by the host (sub_82338DB0):
//   byte    the co-op player count, stored at 0x83A742A8 and then the mtctr
//           count of a loop that writes a slot header (stw/stb/sth constants)
//           into the client slots at 0x825B2428 + i*9780. There are two
//           slots (every other walk stops at +19576); a count up to 255 writes
//           up to 253 headers past them, ~2.4 MB across the title's .data.
//   string, long, string, long
//   pairs   (index, value) until a byte of 2. The value is stored as a word at
//           0x82662FF8 + 52 + index*56 (sub_82139688) when a byte read at
//           obj + 112 + index*48 (sub_8235AA50) is set -- a read as unbounded
//           as the write, so an index up to 255 writes a packet-chosen word up
//           to 14 KB past the table. And the loop ends only on a 2: past the
//           end of the message MSG_ReadByte returns -1 without advancing, so a
//           packet without the 2 spins the network thread forever.
//   byte
//
// coopAccept (sub_82339220), sent by the host's coopConnect (sub_823395C8):
//   byte    the slot the host gave this client: mulli 9780, then a word 2 at
//           slot+0 and the controller at slot+8 -- 0..255 slots, or -1.
//   byte    the co-op player count, stored at 0x824181C8.
//
// Both counts end up in 0x824181C8: coopLaunch's is copied there at map start
// (sub_821FC8B0, from sub_8233D8A8), coopAccept's directly. Most of that
// global's ~140 readers compare it with a slot number, but some use it as a
// bound. sub_8233D8A8 loops to it over two 5764-byte records (0x83B18B88) and
// hands each one whose first word is 4 to sub_821E7040, which zeroes 45016
// bytes at clients + index*45016. Five functions (sub_820D8BD8 and its
// neighbours) loop to it, through sub_820D8158 when co-op is off, over 8460-byte
// per-player records, copying a record-held length*44. And sub_82338A88
// (via sub_82338CB0, at the end of coopLaunch) divides by the number of
// occupied slots: a count of 0 leaves them empty, and the recompiled divw is a
// host division, which traps on 0 where the console's does not.
//
// What a host sends is fixed by the host's own code: the slot comes from
// sub_823382C0 / sub_82339550, which only return 0 or 1; the player count from
// sub_820D8158 just after coopConnect has written the joiner's address into a
// slot (an address type is never 0), or a constant 1 (sub_82132CC8), so 1..2;
// the pairs' index from a loop over 0 and 1, then the 2 that ends them. So a
// packet whose slot is not 0 or 1, whose count is not 1 or 2, whose pairs carry
// another index, or which ends before the 2 cannot have come from a host, and
// it is dropped before the handler sees it. Everything else reaches the
// handler untouched.
//
// The lobby heartbeat repeats coopLaunch's pair loop. Names the table above
// does not hold go to a second one (sub_82139E28, 0x82010AC8); its hb
// (sub_8213A570) is sent by the host (sub_8213AB40) to each member:
//   string, string
//   records (index, byte, bit, string, long) until an index of 2. For an index
//           other than our own member, the byte and the bit are stored at
//           0x82662FF8 + 52 / 56 + index*56 in the lobby's two member records,
//           and the index picks a session member (xuid at +120 + index*48) for
//           the profile setters. An index up to 255 writes up to 14 KB past the
//           records, into the local controllers' profile records among other
//           things; and, as in coopLaunch, a packet without the 2 never ends.
// The host writes a record for each occupied member, 0 and 1, then the 2, so a
// heartbeat with another index, or that ends before the 2, is dropped the same
// way.
//
// The bytes are read with the title's own MSG_ReadByte / MSG_ReadBit /
// MSG_ReadString / MSG_ReadLong, in the order the handler reads them, so the
// check sees exactly what the handler would; the message's read position (+28),
// bit position (+32) and overflow flag (+0), the only fields those routines
// write, are put back afterwards.
//
// Campaign only: none of these functions has a multiplayer twin.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::log_warn;
use crate::title::{
    T_CoopAccept, T_CoopHeartbeat, T_CoopLaunch, T_MSG_ReadBit, T_MSG_ReadByte, T_MSG_ReadLong, T_MSG_ReadString,
};
use std::ffi::CStr;
use std::sync::Once;

const SLOTS: i32 = 2; // co-op client slots
const PAIRS_END: i32 = 2; // the byte that ends coopLaunch's pairs and hb's records
const LAUNCH_STRING: u32 = 256; // coopLaunch's strings
const HEARTBEAT_STRING: u32 = 1024; // hb's
const OVERFLOWED: u32 = 0; // msg_t fields the read routines write
const READ_COUNT: u32 = 28;
const BIT: u32 = 32;

// A frame of our own below the caller's, with room for the longest string read.
const STRING_AT: u32 = 0x80;
const FRAME: u32 = STRING_AT + HEARTBEAT_STRING;

// Reads a message with the title's routines.
struct Peek<'a> {
    ctx: &'a mut PPCContext,
    memory: Memory,
    msg: u32,
}

impl Peek<'_> {
    fn byte(&mut self) -> i32 {
        self.ctx.r3.u64 = u64::from(self.msg);
        T_MSG_ReadByte.call(self.ctx, self.memory);
        self.ctx.r3.as_i32()
    }

    fn bit(&mut self) {
        self.ctx.r3.u64 = u64::from(self.msg);
        T_MSG_ReadBit.call(self.ctx, self.memory);
    }

    fn long(&mut self) {
        self.ctx.r3.u64 = u64::from(self.msg);
        T_MSG_ReadLong.call(self.ctx, self.memory);
    }

    fn string(&mut self, size: u32) {
        self.ctx.r3.u64 = u64::from(self.msg);
        self.ctx.r4.u64 = u64::from(self.ctx.r1.as_u32().wrapping_add(STRING_AT));
        self.ctx.r5.u64 = u64::from(size);
        T_MSG_ReadString.call(self.ctx, self.memory);
    }
}

// Runs `read` and puts the message's read state and the registers back. Not a
// Drop: the title's routines can longjmp, and nothing here needs to outlive that.
fn peeking(ctx: &mut PPCContext, memory: Memory, msg: u32, read: impl FnOnce(&mut Peek) -> bool) -> bool {
    preserving(ctx, |ctx| {
        let overflowed = memory.u32_at(msg.wrapping_add(OVERFLOWED));
        let read_count = memory.u32_at(msg.wrapping_add(READ_COUNT));
        let bit = memory.u32_at(msg.wrapping_add(BIT));
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(FRAME));

        let valid = read(&mut Peek { ctx, memory, msg });

        memory.store_u32(msg.wrapping_add(OVERFLOWED), overflowed);
        memory.store_u32(msg.wrapping_add(READ_COUNT), read_count);
        memory.store_u32(msg.wrapping_add(BIT), bit);
        valid
    })
}

fn is_count(count: i32) -> bool {
    (1..=SLOTS).contains(&count)
}

fn is_slot(slot: i32) -> bool {
    (0..SLOTS).contains(&slot)
}

// coopLaunch, read as sub_82339048 reads it.
fn launch_is_valid(peek: &mut Peek) -> bool {
    if !is_count(peek.byte()) {
        return false;
    }
    peek.string(LAUNCH_STRING);
    peek.long();
    peek.string(LAUNCH_STRING);
    peek.long();
    let mut index = peek.byte();
    while index != PAIRS_END {
        // -1 is the end of the message: the handler would never leave.
        if !is_slot(index) {
            return false;
        }
        peek.byte();
        index = peek.byte();
    }
    true
}

// coopAccept's first two bytes, read as sub_82339220 reads them.
fn accept_is_valid(peek: &mut Peek) -> bool {
    let slot = peek.byte();
    let count = peek.byte();
    is_slot(slot) && is_count(count)
}

// hb, read as sub_8213A570 reads it.
fn heartbeat_is_valid(peek: &mut Peek) -> bool {
    peek.string(HEARTBEAT_STRING);
    peek.string(HEARTBEAT_STRING);
    let mut index = peek.byte();
    while index != PAIRS_END {
        if !is_slot(index) {
            return false;
        }
        peek.byte();
        peek.bit();
        peek.string(HEARTBEAT_STRING);
        peek.long();
        index = peek.byte();
    }
    true
}

static LOGGED: Once = Once::new();

fn log_dropped(what: &CStr) {
    // once: whoever sends one can send thousands
    LOGGED.call_once(|| {
        log_warn!("online: dropped a %s whose co-op slot, player count or member index is out of range", what);
    });
}

// coopLaunch(source = r3, address = r4:r5, msg = r6)
guest_hook!(T_CoopLaunch, |ctx, memory| {
    if !peeking(ctx, memory, ctx.r6.as_u32(), launch_is_valid) {
        log_dropped(c"coopLaunch");
        return;
    }
    T_CoopLaunch.original(ctx, memory);
});

// coopAccept(source = r3, address = r4:r5, msg = r6)
guest_hook!(T_CoopAccept, |ctx, memory| {
    if !peeking(ctx, memory, ctx.r6.as_u32(), accept_is_valid) {
        log_dropped(c"coopAccept");
        return;
    }
    T_CoopAccept.original(ctx, memory);
});

// hb(source = r3, address = r4:r5, msg = r6)
guest_hook!(T_CoopHeartbeat, |ctx, memory| {
    if !peeking(ctx, memory, ctx.r6.as_u32(), heartbeat_is_valid) {
        log_dropped(c"co-op heartbeat");
        return;
    }
    T_CoopHeartbeat.original(ctx, memory);
});
