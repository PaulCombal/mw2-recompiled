// A baseline snapshot from the host holds at most 1024 entities; refuse one
// that holds more before it writes past the array it is decoded into.
//
// A snapshot whose snapFlags carry 0x8 is a baseline: CL_DeltaEntity
// (sub_8218C4A8) decodes each of its entities into a 1024-entry array of
// 256-byte entity states (0x82561504) instead of the parse-entities ring, at
// index count, the word just before the array, and counts up for every entity
// not removed. CL_ParseSnapshot zeroes the count when the flag is set; nothing
// compares it with 1024. The loop of CL_ParsePacketEntities (sub_8218C848),
// CL_DeltaEntity's only caller, ends at entity number 2047, on an overflowed
// message or past its end, and the entity index can jump by 9 bits or restart
// at an absolute 11-bit value, so the number of entities is bounded only by the
// message: an unchanged entity costs 3 bits, and 64 KB carries ~170 000 of them,
// writing host-chosen entity states over the title's .data past the array.
//
// The bound is the host's: the server builds a baseline snapshot from at most
// 1024 entities (NUM_NODELTA_ENTITIES, sub_822C6180, "Hit
// NUM_NODELTA_ENTITIES[%d] in baseline snapshot - ignoring the rest."). So a
// baseline snapshot with a 1025th entity cannot have come from a host, and it is
// refused the way the title refuses a malformed one: the message is marked
// overflowed, CL_ParsePacketEntities' loop stops at the overflow, and
// CL_ParseSnapshot drops a snapshot whose message overflowed (it is never made
// valid, stored as the baseline or copied into the frames).
//
// Two checks, one per side of the array:
//   - the write: CL_DeltaEntity refuses to decode into the baseline array once
//     its count is 1024;
//   - the read: a stored baseline frame is read back, as the old frame, from the
//     same array up to the frame's own entity count, which also counts the old
//     frame's unchanged entities copied into it, not only the decoded ones. So
//     after CL_ParsePacketEntities a baseline frame with more than 1024 entities
//     is refused too.
//
// MP only: the campaign's snapshot parser is other code (no twin of these
// functions in default.xex).

use crate::guest::{Memory, Register, guest_hook};
use crate::log::{Doubling, log_warn};
use crate::title::{T_CL_DeltaEntity, T_CL_ParsePacketEntities, T_DATA_BaselineEntityCount};

const MAX_BASELINE_ENTITIES: u32 = 1024; // NUM_NODELTA_ENTITIES
const BASELINE_FLAG: u32 = 0x8;
// clSnapshot_t fields, as the two functions read them
const SNAP_FLAGS: u32 = 12676;
const NUM_ENTITIES: u32 = 12700;
const OVERFLOWED: u32 = 0; // msg_t

static REFUSED: Doubling = Doubling::new();

fn baseline(memory: Memory, frame: u32) -> bool {
    memory.u32_at(frame.wrapping_add(SNAP_FLAGS)) & BASELINE_FLAG != 0
}

fn refuse(memory: Memory, msg: u32, entities: u32) {
    memory.store_u32(msg.wrapping_add(OVERFLOWED), 1);
    if let Some(count) = REFUSED.count() {
        log_warn!(
            "online: dropped a baseline snapshot with %u+ entities, more than the %u a host sends (%u so far)",
            entities,
            MAX_BASELINE_ENTITIES,
            count
        );
    }
}

// CL_DeltaEntity(cl = r3, msg = r4, time = r5, frame = r6, number = r7, old = r8)
guest_hook!(T_CL_DeltaEntity, |ctx, memory| {
    let count = memory.u32_at(T_DATA_BaselineEntityCount);
    if baseline(memory, ctx.r6.as_u32()) && count >= MAX_BASELINE_ENTITIES {
        refuse(memory, ctx.r4.as_u32(), count.wrapping_add(1));
        return; // the caller ignores the result
    }
    T_CL_DeltaEntity.original(ctx, memory);
});

// CL_ParsePacketEntities(cl = r3, msg = r4, time = r5, old = r6, frame = r7)
guest_hook!(T_CL_ParsePacketEntities, |ctx, memory| {
    let msg = ctx.r4.as_u32();
    let frame = ctx.r7.as_u32();
    T_CL_ParsePacketEntities.original(ctx, memory);
    let entities = memory.u32_at(frame.wrapping_add(NUM_ENTITIES));
    if baseline(memory, frame) && entities > MAX_BASELINE_ENTITIES && memory.u32_at(msg.wrapping_add(OVERFLOWED)) == 0 {
        refuse(memory, msg, entities);
    }
});
