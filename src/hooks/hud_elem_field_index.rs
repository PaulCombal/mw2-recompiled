// A host's snapshot chooses which hud element fields change, by index into the
// 42-entry hud field table; refuse an index the table does not have.
//
// MSG_ReadDeltaPlayerState (sub_8228D0D8) reads the player state's two hud
// element arrays (31 elements each, 168 bytes apart) with
// MSG_ReadDeltaHudElems (sub_8228CF10). Per element it reads lastChanged in 6
// bits (0..63), and then, until the index reaches lastChanged-1, a gap (a bit
// for 1, else 3-bit groups where 7 means "7 more") that it adds to the previous
// index. For each index it takes the descriptor table + index*16, copies the
// fields it skipped and decodes the field with MSG_ReadDeltaField
// (sub_8228BFC0), which stores the value at to + descriptor->offset. Nothing
// compares lastChanged or an index with the table's 42 entries -- the loop
// after them, which copies the unchanged tail, does. An index of 42..62 takes
// its descriptor from the .rdata past the table, whose offset word (31322 for
// 42) puts a host-chosen 32-bit value ~26 KB past the player state; a gap can
// also run the index past lastChanged-1 to any value the message pays for. And
// once the message has been read past its end MSG_ReadBits returns -1, the gap
// comes out 0, and the first field's index is -1: the word before the table
// becomes the offset.
//
// The rule is the host's encoder (sub_82291AE8): lastChanged is one past the
// last of the 42 fields that differ, and it writes every changed field below
// it, so every index it sends is past the previous one and at most
// lastChanged-1, which is at most 41. A message that breaks that cannot have
// come from a host. It is refused the way the title refuses a malformed message:
// the message is marked overflowed, so CL_ParseSnapshot drops the snapshot,
// and nothing more is decoded into the elements.
//
// The function is reimplemented whole, because the check sits inside its
// loop: the same reads, in the same order, with the title's own MSG_ReadBit,
// MSG_ReadBits and MSG_ReadDeltaField, and the same copies; only the check is
// new.
//
// MP only: no twin in default.xex.

use crate::guest::{Memory, PPCContext, Register, guest_hook};
use crate::log::{Doubling, log_warn};
use crate::title::{
    T_DATA_HudElemFieldCount, T_DATA_HudElemFields, T_MSG_ReadBit, T_MSG_ReadBits, T_MSG_ReadDeltaField,
};

const ELEMENT_SIZE: u32 = 168;
const FIELD_SIZE: u32 = 16; // {name, offset, bits, flags}
const FIELD_OFFSET: u32 = 4;
const OVERFLOWED: u32 = 0; // msg_t

// The title's routines; each returns its value in r3.
fn read_bits(ctx: &mut PPCContext, memory: Memory, msg: u32, bits: u32) -> i32 {
    ctx.r3.u64 = u64::from(msg);
    ctx.r4.u64 = u64::from(bits);
    T_MSG_ReadBits.call(ctx, memory);
    ctx.r3.as_i32()
}

fn read_bit(ctx: &mut PPCContext, memory: Memory, msg: u32) -> i32 {
    ctx.r3.u64 = u64::from(msg);
    T_MSG_ReadBit.call(ctx, memory);
    ctx.r3.as_i32()
}

fn field_descriptor(field: i32) -> u32 {
    T_DATA_HudElemFields.wrapping_add((field as u32).wrapping_mul(FIELD_SIZE))
}

// A field the message leaves unchanged keeps the old element's value.
fn copy_field(memory: Memory, from: u32, to: u32, field: i32) {
    let offset = memory.u32_at(field_descriptor(field).wrapping_add(FIELD_OFFSET));
    memory.store_u32(to.wrapping_add(offset), memory.u32_at(from.wrapping_add(offset)));
}

static REFUSED: Doubling = Doubling::new();

fn refuse(memory: Memory, msg: u32, last_changed: i32, field: i32) {
    memory.store_u32(msg.wrapping_add(OVERFLOWED), 1);
    if let Some(count) = REFUSED.count() {
        log_warn!(
            "online: dropped a snapshot whose hud element field %d (of %d changed, %u fields) no host sends (%u so far)",
            field,
            last_changed,
            memory.u32_at(T_DATA_HudElemFieldCount),
            count
        );
    }
}

// MSG_ReadDeltaHudElems(msg = r3, time = r4, from = r5, to = r6, count = r7)
guest_hook!(T_MSG_ReadDeltaHudElems, |ctx, memory| {
    let msg = ctx.r3.as_u32();
    let time = ctx.r4.as_u32();
    let from = ctx.r5.as_u32();
    let to = ctx.r6.as_u32();
    let capacity = ctx.r7.as_i32();
    let fields = memory.u32_at(T_DATA_HudElemFieldCount) as i32;

    let elements = read_bits(ctx, memory, msg, 5);
    for e in 0..elements {
        let from_element = from.wrapping_add((e as u32).wrapping_mul(ELEMENT_SIZE));
        let to_element = to.wrapping_add((e as u32).wrapping_mul(ELEMENT_SIZE));

        let last_changed = read_bits(ctx, memory, msg, 6);
        let mut field = -1i32;
        // The original's tests: unsigned against 0, then lastChanged-1 > -1.
        if last_changed != 0 && last_changed.wrapping_sub(1) > -1 {
            if last_changed > fields {
                refuse(memory, msg, last_changed, last_changed.wrapping_sub(1));
                return;
            }
            let last = last_changed.wrapping_sub(1);
            loop {
                let gap = if read_bit(ctx, memory, msg) != 0 {
                    1
                } else {
                    let mut sevens = 1i32;
                    let mut bits = read_bits(ctx, memory, msg, 3);
                    while bits == 7 {
                        sevens = sevens.wrapping_add(7);
                        bits = read_bits(ctx, memory, msg, 3);
                    }
                    bits.wrapping_add(sevens)
                };
                let previous = field;
                field = previous.wrapping_add(gap);
                if field <= previous || field > last {
                    refuse(memory, msg, last_changed, field);
                    return;
                }
                for skipped in previous.wrapping_add(1)..field {
                    copy_field(memory, from_element, to_element, skipped);
                }

                ctx.r3.u64 = u64::from(msg);
                ctx.r4.u64 = u64::from(time);
                ctx.r5.u64 = u64::from(from_element);
                ctx.r6.u64 = u64::from(to_element);
                ctx.r7.u64 = u64::from(field_descriptor(field));
                ctx.r8.u64 = 0;
                ctx.r9.u64 = 0;
                T_MSG_ReadDeltaField.call(ctx, memory);
                if field >= last {
                    break;
                }
            }
        }
        for unchanged in field.wrapping_add(1)..fields {
            copy_field(memory, from_element, to_element, unchanged);
        }
    }

    // The elements past the message's clear until the first already clear one
    // (type, the first word, 0). As in the original, an overflowed count (-1)
    // starts one element early.
    for e in elements..capacity {
        let element = to.wrapping_add((e as u32).wrapping_mul(ELEMENT_SIZE));
        if memory.u32_at(element) == 0 {
            break;
        }
        memory.zero(element, ELEMENT_SIZE);
    }
});
