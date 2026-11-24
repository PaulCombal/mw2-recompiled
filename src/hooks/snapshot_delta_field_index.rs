// A host's snapshot chooses which fields of an entity, of the player state and
// of an objective change, by index into each one's field table; refuse an index
// the table does not have.
//
// Three readers decode a snapshot's deltas the same way: MSG_ReadDeltaEntity
// (sub_8228CA90), MSG_ReadDeltaPlayerState (sub_8228D0D8) and
// MSG_ReadDeltaFields (sub_8228C7D8; under MSG_ReadDeltaStruct, and the
// player state's objectives). Each reads the last changed field, then gaps
// (sub_8228C718: a unary run, or groups of the width it is given where the
// all-ones group means "that many more") that it adds to the previous index.
// For each index it takes the descriptor {name, offset, bits, flags} at
// table + index*16, copies the fields it skipped from the old state and decodes
// the field with MSG_ReadDeltaField (sub_8228BFC0), which stores the value at
// to + descriptor->offset. The last changed field is compared with the table's
// length, the indexes are not:
//   - MSG_ReadDeltaFields stops only when an index equals the last changed
//     field, so an index past it decodes and copies on until the sum wraps;
//   - MSG_ReadDeltaPlayerState stops at the first index not below it, so it
//     decodes that one, whatever it is;
//   - MSG_ReadDeltaEntity stops at an index past it, but decodes the first
//     index unchecked (the one after the eType too, when the eType changed).
// And every reader accepts a last changed field one past its table's last.
// The tables lie back to back, so an index past one takes the next table's
// descriptor, or the .rdata after, and the skipped-field copy and the decode
// write a host-chosen value at a host-chosen offset from the state. Once the
// message has been read past its end the gap comes out 0: an index repeats.
//
// The rule is the host's encoders (sub_82290E60, sub_82291CC0, sub_82290BC0):
// the last changed field is the last of the table's fields that differ, every
// changed field is written in order and none past the last. So every index is
// past the previous one, at most the last changed field and a field of the
// table. A message that breaks that cannot have come from a host. It is refused
// the way the title refuses a malformed message, before anything is copied or
// decoded by the index: MSG_Discard marks the message overflowed, so
// CL_ParseSnapshot drops the snapshot, and the reader stops. For the same
// reason an entity whose eType is negative is refused: the eType is a byte on
// the wire, and MSG_ReadByte gives -1 only once the message is read past its
// end; the type's table lookup clamps from above only.
//
// The functions are reimplemented whole, because the check sits inside their
// loops: the same reads, in the same order, with the title's own routines, the
// same copies and the same cl_shownet prints; only the checks are new. Two
// liberties: the copies of a whole state are host memmoves, and the origin the
// player state falls back on is copied as words, not through the FPU (the same
// but for a signalling NaN). The player state's slot loop also scans the slots
// for the one it decoded, unless a dvar is set, and uses nothing it finds; the
// scan only reads, and is left out. MSG_ReadDeltaPlayerState calls the other
// two readers it uses, MSG_ReadDeltaFields and MSG_ReadDeltaHudElems
// (hud_elem_field_index.rs), through their hooks.
//
// MP only: no twin in default.xex.

use crate::guest::{GuestFunction, Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::{Doubling, log_warn};
use crate::title::{
    T_BG_GetEntityTypeName, T_CL_FindSentOrigin, T_Com_PrintMessage, T_Com_Printf, T_DATA_ETypeField,
    T_DATA_EntityFieldCount, T_DATA_EntityPrintDvar, T_DATA_LocalClients, T_DATA_ObjectiveFieldCount,
    T_DATA_ObjectiveFields, T_DATA_PlayerStateFieldCount, T_DATA_PlayerStateFields, T_DATA_ShownetDvar,
    T_LOCAL_CLIENT_STRIDE, T_MSG_Discard, T_MSG_EntityTypeFields, T_MSG_ReadBit, T_MSG_ReadBits, T_MSG_ReadByte,
    T_MSG_ReadDeltaField, T_MSG_ReadDeltaFields, T_MSG_ReadDeltaHudElems, T_MSG_ReadFieldGap, T_MSG_ReadShort,
    T_STR_DeltaFieldsLastChanged, T_STR_EntityChanged, T_STR_EntityLastChanged, T_STR_EntityNumber, T_STR_EntityRemove,
    T_STR_OriginNotFound, T_STR_PS_STATS, T_STR_PercentSSpace, T_STR_PlayerState,
};
use std::ffi::CStr;

const FIELD_SIZE: u32 = 16; // {name, offset, bits, flags}
const FIELD_OFFSET: u32 = 4;
const FIELD_BITS: u32 = 8;
const FIELD_FLAGS: u32 = 12; // a byte
const READ_COUNT: u32 = 28; // msg_t, printed by cl_shownet
const DVAR_VALUE: u32 = 12;
const ENTITY_STATE_SIZE: u32 = 256;
const PLAYER_STATE_SIZE: u32 = 12672;
const CHANNEL: u32 = 25; // the prints' console channel

// Frames of our own for the calls, the originals' sizes: the prints are
// variadic and spill their register arguments into their caller's frame, and
// the player state's frame holds the empty old state.
const FIELDS_FRAME: u32 = 208;
const ENTITY_FRAME: u32 = 192;
const PLAYER_STATE_FRAME: u32 = 12896;
const EMPTY_PLAYER_STATE: u32 = 96; // in that frame

// r3 to r10, with what `arguments` does not give as 0.
fn call(ctx: &mut PPCContext, memory: Memory, function: GuestFunction, arguments: &[u32]) -> u32 {
    let mut arguments = arguments.iter().copied();
    let mut next = || u64::from(arguments.next().unwrap_or(0));
    ctx.r3.u64 = next();
    ctx.r4.u64 = next();
    ctx.r5.u64 = next();
    ctx.r6.u64 = next();
    ctx.r7.u64 = next();
    ctx.r8.u64 = next();
    ctx.r9.u64 = next();
    ctx.r10.u64 = next();
    function.call(ctx, memory);
    ctx.r3.as_u32()
}

fn read_bit(ctx: &mut PPCContext, memory: Memory, msg: u32) -> i32 {
    call(ctx, memory, T_MSG_ReadBit, &[msg]) as i32
}

fn read_bits(ctx: &mut PPCContext, memory: Memory, msg: u32, bits: u32) -> i32 {
    call(ctx, memory, T_MSG_ReadBits, &[msg, bits]) as i32
}

fn read_gap(ctx: &mut PPCContext, memory: Memory, msg: u32, width: u32) -> i32 {
    call(ctx, memory, T_MSG_ReadFieldGap, &[msg, width]) as i32
}

// What MSG_ReadDeltaField decodes from and into.
#[derive(Copy, Clone)]
struct Delta {
    msg: u32,
    time: u32,
    from: u32,
    to: u32,
}

fn read_delta_field(ctx: &mut PPCContext, memory: Memory, delta: Delta, field: u32, print: u32, from_zero: bool) {
    let Delta { msg, time, from, to } = delta;
    call(ctx, memory, T_MSG_ReadDeltaField, &[msg, time, from, to, field, print, u32::from(from_zero)]);
}

// The bits that hold 0..n: what 32 - cntlzw(n) gives.
fn bits_for(n: u32) -> u32 {
    32 - n.leading_zeros()
}

// A bit read as the readers turn it into a flag (neg/andc/rlwinm): set for 1,
// clear for 0 and for the -1 of a message read past its end.
fn set(bit: i32) -> bool {
    bit > 0
}

fn field(table: u32, index: i32) -> u32 {
    table.wrapping_add((index as u32).wrapping_mul(FIELD_SIZE))
}

fn field_offset(memory: Memory, field: u32) -> u32 {
    memory.u32_at(field.wrapping_add(FIELD_OFFSET))
}

fn field_flags(memory: Memory, field: u32) -> u8 {
    memory.u8_at(field.wrapping_add(FIELD_FLAGS))
}

// A field the message leaves unchanged keeps the old state's value.
fn copy_field(memory: Memory, from: u32, to: u32, field: u32) {
    let offset = field_offset(memory, field);
    memory.store_u32(to.wrapping_add(offset), memory.u32_at(from.wrapping_add(offset)));
}

fn copy_fields(memory: Memory, from: u32, to: u32, table: u32, first: i32, end: i32) {
    for i in first..end {
        copy_field(memory, from, to, field(table, i));
    }
}

// cl_shownet's value, or nothing when the dvar is not registered.
fn shownet(memory: Memory) -> Option<i32> {
    memory.pointer_at(T_DATA_ShownetDvar).map(|dvar| memory.u32_at(dvar.wrapping_add(DVAR_VALUE)) as i32)
}

static REFUSED: Doubling = Doubling::new();

fn refuse(ctx: &mut PPCContext, memory: Memory, msg: u32, what: &CStr, index: i32, last: i32, fields: i32) {
    call(ctx, memory, T_MSG_Discard, &[msg]);
    if let Some(count) = REFUSED.count() {
        log_warn!(
            "online: dropped a snapshot whose %s field %d (last changed %d, %d fields) no host sends (%u so far)",
            what,
            index,
            last,
            fields,
            count
        );
    }
}

// The rule the host's encoders keep: an index past the previous one, at most
// the last changed field, and a field of the table.
fn sent(index: i32, previous: i32, last: i32, fields: i32) -> bool {
    index > previous && index <= last && index < fields
}

// MSG_ReadDeltaFields(msg = r3, time = r4, from = r5, to = r6, count = r7,
//                     fields = r8, gap width = r9, compare = r10, a byte)
guest_hook!(T_MSG_ReadDeltaFields, |ctx, memory| {
    let delta = Delta { msg: ctx.r3.as_u32(), time: ctx.r4.as_u32(), from: ctx.r5.as_u32(), to: ctx.r6.as_u32() };
    let Delta { msg, from, to, .. } = delta;
    let count = ctx.r7.as_i32();
    let table = ctx.r8.as_u32();
    let width = ctx.r9.as_u32();
    let compare = ctx.r10.as_u32() & 0xFF != 0;
    preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(FIELDS_FRAME));

        // With compare, the first field tells whether the old state is the same
        // one: if it differs, the fields decode against zero.
        let first_offset = if compare { field_offset(memory, table) } else { 0 };

        if read_bit(ctx, memory, msg) == 0 {
            copy_fields(memory, from, to, table, 0, count);
            return;
        }

        let last = read_bits(ctx, memory, msg, bits_for(count as u32));
        if last > count {
            call(ctx, memory, T_MSG_Discard, &[msg]);
            call(ctx, memory, T_Com_PrintMessage, &[CHANNEL, T_STR_DeltaFieldsLastChanged, last as u32, count as u32]);
            return;
        }

        let mut same = true;
        if last != -1 {
            let mut index = -1i32;
            loop {
                let previous = index;
                index = previous.wrapping_add(read_gap(ctx, memory, msg, width));
                if !sent(index, previous, last, count) {
                    refuse(ctx, memory, msg, c"struct", index, last, count);
                    return;
                }
                copy_fields(memory, from, to, table, previous.wrapping_add(1), index);
                if compare && index > 0 {
                    same =
                        memory.u32_at(from.wrapping_add(first_offset)) == memory.u32_at(to.wrapping_add(first_offset));
                }
                read_delta_field(ctx, memory, delta, field(table, index), 0, !same);
                if index == last {
                    break;
                }
            }
        }

        // The fields of flags 2 among those before the last decode again.
        for i in 0..last {
            let descriptor = field(table, i);
            if field_flags(memory, descriptor) == 2 {
                read_delta_field(ctx, memory, delta, descriptor, 0, !same);
            }
        }
        copy_fields(memory, from, to, table, last.wrapping_add(1), count);
    });
});

// MSG_ReadDeltaEntity(msg = r3, time = r4, from = r5, to = r6, number = r7,
//                     gap width = r8) -> 1 if the entity is removed
guest_hook!(T_MSG_ReadDeltaEntity, |ctx, memory| {
    let delta = Delta { msg: ctx.r3.as_u32(), time: ctx.r4.as_u32(), from: ctx.r5.as_u32(), to: ctx.r6.as_u32() };
    let number = ctx.r7.as_u32();
    let width = ctx.r8.as_u32();
    let removed = preserving(ctx, |ctx| read_delta_entity(ctx, memory, delta, number, width));
    ctx.r3.u64 = u64::from(removed);
});

fn read_delta_entity(ctx: &mut PPCContext, memory: Memory, delta: Delta, number: u32, width: u32) -> u32 {
    let Delta { msg, from, to, .. } = delta;
    ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(ENTITY_FRAME));
    let shows = || shownet(memory).is_some_and(|value| value >= 2 || value == -1);
    let read_count = || memory.u32_at(msg.wrapping_add(READ_COUNT));

    if read_bit(ctx, memory, msg) == 1 {
        if shows() {
            call(ctx, memory, T_Com_Printf, &[CHANNEL, T_STR_EntityRemove, read_count(), number]);
        }
        return 1;
    }
    if read_bit(ctx, memory, msg) == 0 {
        memory.copy(to, from, ENTITY_STATE_SIZE);
        return 0;
    }

    // When set, the fields of two float kinds (bits -83..-78) decode against
    // zero.
    let zero_kinds = read_bit(ctx, memory, msg) != 0;
    let last_changed = read_bits(ctx, memory, msg, bits_for(memory.u32_at(T_DATA_EntityFieldCount))) as u32;
    if last_changed > memory.u32_at(T_DATA_EntityFieldCount) {
        call(ctx, memory, T_MSG_Discard, &[msg]);
        return 0;
    }
    if shows() {
        call(ctx, memory, T_Com_Printf, &[CHANNEL, T_STR_EntityNumber, read_count(), memory.u32_at(to)]);
    }
    memory.store_u32(to, number);

    // The eType decodes first, from the field every type's table starts with:
    // the type picks the table.
    let etype_offset = field_offset(memory, T_DATA_ETypeField);
    let mut previous = -1i32;
    let mut index = read_gap(ctx, memory, msg, width).wrapping_sub(1);
    if index > 0 {
        memory.store_u32(to.wrapping_add(etype_offset), memory.u32_at(from.wrapping_add(etype_offset)));
    } else {
        read_delta_field(ctx, memory, delta, T_DATA_ETypeField, 0, false);
        previous = 0;
        if last_changed != 0 {
            index = read_gap(ctx, memory, msg, width);
        }
    }

    let etype = memory.u32_at(to.wrapping_add(etype_offset)) as i32;
    if etype < 0 {
        refuse(ctx, memory, msg, c"entity type", etype, last_changed as i32, 0);
        return 0;
    }
    let kind = call(ctx, memory, T_MSG_EntityTypeFields, &[etype as u32]);
    let table = memory.u32_at(kind);
    let fields = memory.u32_at(kind.wrapping_add(4));
    if last_changed > fields {
        call(ctx, memory, T_Com_Printf, &[14, T_STR_EntityLastChanged, last_changed, fields]);
        memory.store_u32(msg, 1); // overflowed
        return 0;
    }
    let last = last_changed as i32;

    // Whether a field decodes against zero: always when the old state is another
    // entity's (its second word differs), and for the two float kinds when asked.
    let from_zero = |descriptor: u32| {
        if memory.u32_at(from.wrapping_add(4)) != memory.u32_at(to.wrapping_add(4)) {
            return true;
        }
        let bits = memory.u32_at(descriptor.wrapping_add(FIELD_BITS)) as i32;
        zero_kinds && (-83..=-78).contains(&bits)
    };

    if previous != last {
        loop {
            if !sent(index, previous, last, fields as i32) {
                refuse(ctx, memory, msg, c"entity", index, last, fields as i32);
                return 0;
            }
            copy_fields(memory, from, to, table, previous.wrapping_add(1), index);
            let descriptor = field(table, index);
            read_delta_field(ctx, memory, delta, descriptor, 0, from_zero(descriptor));
            previous = index;
            if index == last {
                break;
            }
            index = index.wrapping_add(read_gap(ctx, memory, msg, width));
        }
    }

    // The fields of flags 2 among those before the last decode again.
    for i in 0..last {
        let descriptor = field(table, i);
        if field_flags(memory, descriptor) == 2 {
            read_delta_field(ctx, memory, delta, descriptor, 0, from_zero(descriptor));
        }
    }
    let count = memory.u32_at(T_DATA_EntityFieldCount) as i32;
    copy_fields(memory, from, to, table, previous.wrapping_add(1), count);

    let print = memory.u32_at(T_DATA_EntityPrintDvar);
    if memory.u8_at(print.wrapping_add(DVAR_VALUE)) != 0 {
        let etype = memory.u32_at(to.wrapping_add(etype_offset));
        let name = call(ctx, memory, T_BG_GetEntityTypeName, &[etype]);
        call(ctx, memory, T_Com_Printf, &[CHANNEL, T_STR_EntityChanged, number, name]);
    }
    0
}

// MSG_ReadDeltaPlayerState(localClient = r3, msg = r4, time = r5, from = r6,
//                          to = r7, predicted = r8, a byte)
guest_hook!(T_MSG_ReadDeltaPlayerState, |ctx, memory| {
    let local_client = ctx.r3.as_u32();
    let msg = ctx.r4.as_u32();
    let time = ctx.r5.as_u32();
    let old = ctx.r6.as_u32();
    let to = ctx.r7.as_u32();
    let predicted = ctx.r8.as_u32() & 0xFF != 0;
    preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(PLAYER_STATE_FRAME));

        let mut from = old;
        if from == 0 {
            from = ctx.r1.as_u32().wrapping_add(EMPTY_PLAYER_STATE);
            memory.zero(from, PLAYER_STATE_SIZE);
        }
        memory.copy(to, from, PLAYER_STATE_SIZE);
        let delta = Delta { msg, time, from, to };

        let mut print = 0;
        if shownet(memory).is_some_and(|value| value >= 2 || value == -2) {
            let read_count = memory.u32_at(msg.wrapping_add(READ_COUNT));
            call(ctx, memory, T_Com_Printf, &[CHANNEL, T_STR_PlayerState, read_count]);
            print = 1;
        }

        // Clear, the origin is the one the client sent, looked up below; set, the
        // fields of flags 3 of a predicted state decode against zero.
        let own_origin = set(read_bit(ctx, memory, msg));
        let count = memory.u32_at(T_DATA_PlayerStateFieldCount) as i32;
        let table = T_DATA_PlayerStateFields;
        // One past the last changed field, 0 for none.
        let mut last_changed = read_bits(ctx, memory, msg, bits_for(count.wrapping_add(1) as u32));
        if last_changed > count.wrapping_add(1) {
            call(ctx, memory, T_MSG_Discard, &[msg]);
            return;
        }

        let from_zero = |descriptor: u32| predicted && own_origin && field_flags(memory, descriptor) == 3;

        let mut previous = -1i32;
        if last_changed > 0 {
            last_changed -= 1;
            let last = last_changed;
            loop {
                let index = previous.wrapping_add(read_gap(ctx, memory, msg, 4));
                if !sent(index, previous, last, count) {
                    refuse(ctx, memory, msg, c"player state", index, last, count);
                    return;
                }
                // The skipped fields of flags 2 are left as they are.
                for i in previous.wrapping_add(1)..index {
                    let descriptor = field(table, i);
                    if field_flags(memory, descriptor) != 2 {
                        copy_field(memory, from, to, descriptor);
                    }
                }
                let descriptor = field(table, index);
                read_delta_field(ctx, memory, delta, descriptor, print, from_zero(descriptor));
                previous = index;
                if index >= last {
                    break;
                }
            }
        }
        copy_fields(memory, from, to, table, previous.wrapping_add(1), count);

        // The fields of flags 2 among those before the last decode again.
        for i in 0..last_changed {
            let descriptor = field(table, i);
            if field_flags(memory, descriptor) == 2 {
                read_delta_field(ctx, memory, delta, descriptor, print, from_zero(descriptor));
            }
        }

        // The origin sent with the command it was predicted for; without it, the
        // old state's.
        if !own_origin {
            let client =
                memory.u32_at(T_DATA_LocalClients).wrapping_add(local_client.wrapping_mul(T_LOCAL_CLIENT_STRIDE));
            let sent_at = [
                client,
                memory.u32_at(to),
                to.wrapping_add(28),
                to.wrapping_add(40),
                to.wrapping_add(24),
                to.wrapping_add(172),
            ];
            if call(ctx, memory, T_CL_FindSentOrigin, &sent_at) == 0 {
                call(ctx, memory, T_Com_PrintMessage, &[CHANNEL, T_STR_OriginNotFound]);
                for offset in (28..52).step_by(4) {
                    memory.store_u32(to.wrapping_add(offset), memory.u32_at(from.wrapping_add(offset)));
                }
                memory.store_u32(to.wrapping_add(24), memory.u32_at(from.wrapping_add(24)));
                memory.store_u32(to.wrapping_add(172), memory.u32_at(from.wrapping_add(172)));
            }
        }

        // Stats: four, by a 4-bit mask.
        if read_bit(ctx, memory, msg) != 0 {
            if shownet(memory) == Some(4) {
                call(ctx, memory, T_Com_Printf, &[CHANNEL, T_STR_PercentSSpace, T_STR_PS_STATS]);
            }
            let mask = read_bits(ctx, memory, msg, 4) as u32;
            for (bit, offset, reader) in [
                (1, 336, T_MSG_ReadShort),
                (2, 340, T_MSG_ReadShort),
                (4, 344, T_MSG_ReadShort),
                (8, 348, T_MSG_ReadByte),
            ] {
                if mask & bit != 0 {
                    let value = call(ctx, memory, reader, &[msg]);
                    memory.store_u32(to.wrapping_add(offset), value);
                }
            }
        }

        // Ammo: 12-byte entries by a 4-bit index, an 11-bit word and two optional
        // bytes.
        if read_bit(ctx, memory, msg) != 0 {
            loop {
                let index = read_bits(ctx, memory, msg, 4) as u32;
                let entry = to.wrapping_add(848).wrapping_add(index.wrapping_mul(12));
                let word = read_bits(ctx, memory, msg, 11) as u32;
                memory.store_u32(entry, word);
                for k in 1..=2 {
                    if read_bit(ctx, memory, msg) != 0 {
                        let byte = read_bits(ctx, memory, msg, 8) as u32;
                        memory.store_u32(entry.wrapping_add(k * 4), byte);
                    }
                }
                if read_bit(ctx, memory, msg) == 0 {
                    break;
                }
            }
        }

        // The 15 slots: a word at +544 and five bytes at +604 each.
        if read_bit(ctx, memory, msg) != 0 {
            for slot in 0..15 {
                if read_bit(ctx, memory, msg) == 0 {
                    continue;
                }
                let word = to.wrapping_add(544).wrapping_add(slot * 4);
                let bytes = to.wrapping_add(604).wrapping_add(slot * 5);
                if read_bit(ctx, memory, msg) != 0 {
                    let value = read_bits(ctx, memory, msg, 11) as u32;
                    memory.store_u32(word, value);
                }
                for at in [0, 1, 3, 4] {
                    let flag = set(read_bit(ctx, memory, msg));
                    memory.store_u8(bytes.wrapping_add(at), u8::from(flag));
                }
                if read_bit(ctx, memory, msg) != 0 {
                    let value = call(ctx, memory, T_MSG_ReadByte, &[msg]) as u8;
                    memory.store_u8(bytes.wrapping_add(2), value);
                }
            }
        }

        // Weapons: two words, by a 4-bit index.
        if read_bit(ctx, memory, msg) != 0 {
            loop {
                let weapon = read_bits(ctx, memory, msg, 4) as u32;
                let word = read_bits(ctx, memory, msg, 11) as u32;
                memory.store_u32(to.wrapping_add(weapon.wrapping_add(91).wrapping_mul(8)), word);
                let word = read_bits(ctx, memory, msg, 10) as u32;
                memory.store_u32(to.wrapping_add(weapon.wrapping_mul(8)).wrapping_add(732), word);
                if read_bit(ctx, memory, msg) == 0 {
                    break;
                }
            }
        }

        // The 32 objectives, 28 bytes each: a 3-bit state, then their fields.
        if read_bit(ctx, memory, msg) != 0 {
            for i in 0..32 {
                let objective = 1208 + i * 28;
                let state = read_bits(ctx, memory, msg, 3) as u32;
                memory.store_u32(to.wrapping_add(objective), state);
                let fields = memory.u32_at(T_DATA_ObjectiveFieldCount);
                let arguments = [
                    msg,
                    time,
                    from.wrapping_add(objective),
                    to.wrapping_add(objective),
                    fields,
                    T_DATA_ObjectiveFields,
                    1,
                    0,
                ];
                call(ctx, memory, T_MSG_ReadDeltaFields, &arguments);
            }
        }

        // The two hud element arrays, 31 elements each.
        if read_bit(ctx, memory, msg) != 0 {
            for offset in [7328, 2120] {
                let arguments = [msg, time, from.wrapping_add(offset), to.wrapping_add(offset), 31];
                call(ctx, memory, T_MSG_ReadDeltaHudElems, &arguments);
            }
        }
    });
});
