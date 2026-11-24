// A saved game's script objects and code positions must fit the VM they restore,
// and its entity pair list the list it is restored into.
//
// A saved game state (here, the one a server streams to the next host in a host
// migration; migration_state_size.rs) carries the script VM's objects by
// number. Restoring it, sub_822A0AC8 reads the object count (a u16 stored at
// 0x835E58CA, +66 of the script save state at 0x835E5888) and then, for objects
// 1..count, makes sure each has a parent variable, writing its id into the
// number -> variable table at 0x835F78E8: 0x12000 bytes, 36864 u16 entries,
// cleared just before (sub_82300378). References to an object are a signed
// 16-bit number from the file, looked up in that table by sub_822A06E0 (the
// number read by its caller, sub_8223EDB0) and by sub_822A0148 (read by itself,
// or replayed from a 16-entry ring of recent numbers): a 0 entry gets a new
// parent variable stored into the table, and the variable's reference count is
// raised (sub_822A43E0).
//
// Neither is bounded. The count runs to 65535, and its loop writes ids past the
// table's end, into the script VM's globals after it. A reference number is
// tested only against 0: a negative one indexes up to 64 KiB before the table,
// storing a variable id there or raising the count of whatever variable it
// finds; and the ring keeps the number as a u16, so the same number replayed
// (0x8000..0xFFFF, +1) indexes past the table's end.
//
// One more file value reaches the same table unchecked. The object restore fixes
// up an object-reference field (type 8 of sub_82266058) by handing its raw u16
// to sub_822A0740, which — unlike sub_822A06E0, whose number is sign-extended
// 16-bit and so already under 32768 once negatives are refused — masks to the
// full u16 and tests it only against 0. A field value of 36864..65535 reads
// table[number] up to 57 KiB past the table (sub_822A0740 at 822A0788), and that
// garbage u16 then drives a reference-count write 16 bytes per unit past the
// object-record array (sub_822A43E0). The u16 sub_822A0AC8 reads after the count
// (+68) indexes nothing in the restore, and every save sets it from the count
// before reading the table with it (sub_822A1F18, then sub_822A19D0).
//
// Each object's type comes from the file too. Its first byte (sub_822A07B0) is
// either a tag 1..5 in the low three bits, for the types the writer
// (sub_822A16D0) gives one, or the type shifted left by 3; the restore stores
// that type in the object's 5-bit type field unchecked. The script VM has 25
// types (the name table at 0x824445B0 ends at "removed thread", 24), and names a
// type through that table when it reports an object it cannot use as a field
// object (sub_822A4AE0, sub_822AB368): a type of 25..31 reads past it and prints
// whatever the next words point at.
//
// Code positions come from the file as an offset into the script program
// buffer, which runs from 0x835E58E0's word to 0x835E58E4's. A value of type
// codepos or function (sub_822A03D8 at 822A04E0) and a thread's resume position
// (sub_822A02E0 at 822A0338) are restored as start + offset, unchecked, and the
// VM fetches its opcodes from that address when it runs or resumes the thread
// (sub_822AE8F8, lbz from the position at 822AEB10, nothing compared): any
// offset makes it execute whatever guest memory holds as script.
//
// The numbers a writer gives are 1..count, and the table is the space they live
// in: a count above its last entry, or a reference outside it, is a file no
// title wrote, and so is a type the title does not have. A writer saves a code
// position as its distance from the start (sub_822A0D40, sub_822A0218), and
// G_InitGame, loading the scripts for a migrated state, refuses one whose
// program size differs from this host's before the restore (sub_822A5ED8
// against the header, 82233DFC), so a position outside the buffer is no
// title's either: the title's own test for one, sub_8229DE68, is
// start <= position < end. All are refused, before they
// are used, with the error the save reader raises for data it cannot read
// (Com_Error(ERR_DROP, "Trying to read corrupted file"), sub_82311F50). Refusing
// negative numbers where they are read also keeps the ring's numbers in
// 1..32767. The reads that sub_822A0148, sub_822A0AC8, sub_822A07B0,
// sub_822A03D8 and sub_822A02E0 do themselves are recognised by their return
// address in the reader (sub_82312000), the one hook that sees the value before
// it is used.
//
// The entity restore (sub_8223DFF0) reads one more count unchecked: that of a
// list of entity pairs in the game state at 0x82FF2F08, a word at +11996 (read
// returning to 8223E11C), then count 12-byte records into the list at +5852,
// whose next field is at +8924; sub_822332E0 copies count records from there to
// +8924 before it clears the count. The list is 256 records: the one function
// that adds to it (sub_8225BD88) adds none when the count is 256, and the writer
// (sub_8223E5A8) saves the count and that many records as they are. A count
// above 256 is a file no title wrote, and both the read and the copy run past
// the list. It is refused like the others.
//
// MP-only: the campaign's own save loader is not reached from the network.

use crate::guest::{Register, guest_hook};
use crate::hooks::com_error::corrupted;
use crate::log::log_error;
use crate::title::{
    T_DATA_ScrProgramEnd, T_DATA_ScrProgramStart, T_MemFile_Read, T_RET_G_ReadEntityPairCount, T_RET_Scr_ReadCodePos,
    T_RET_Scr_ReadObjectCount, T_RET_Scr_ReadObjectRef, T_RET_Scr_ReadObjectType, T_RET_Scr_ReadResumePos,
    T_Scr_ResolveObjectRef, T_Scr_SavedObject,
};

const TABLE_ENTRIES: u32 = 36864; // 0x12000 bytes of u16
const VARIABLE_TYPES: u32 = 25; // the VM's types, 0..24
const ENTITY_PAIRS: u32 = 256; // sub_8225BD88's limit

// (file = r3, bytes = r4, destination = r5)
guest_hook!(T_MemFile_Read, |ctx, memory| {
    let from = ctx.lr as u32;
    let bytes = ctx.r4.as_u32();
    let destination = ctx.r5.as_u32();
    T_MemFile_Read.original(ctx, memory);
    #[allow(non_upper_case_globals)] // the table's names
    match (bytes, from) {
        (1, T_RET_Scr_ReadObjectType) => {
            let byte = u32::from(memory.u8_at(destination));
            let tag = byte & 7;
            if (tag == 0 || tag > 5) && (byte >> 3) >= VARIABLE_TYPES {
                log_error!("save file: script object type %u, past the VM's last, %u", byte >> 3, VARIABLE_TYPES - 1);
                corrupted(ctx, memory);
            }
        }
        (4, T_RET_Scr_ReadCodePos | T_RET_Scr_ReadResumePos) => {
            let start = memory.u32_at(T_DATA_ScrProgramStart);
            let end = memory.u32_at(T_DATA_ScrProgramEnd);
            let offset = memory.u32_at(destination);
            let position = start.wrapping_add(offset); // as the restore adds it
            if position < start || position >= end {
                log_error!(
                    "save file: script code position %#x, outside the %u-byte program",
                    offset,
                    end.wrapping_sub(start)
                );
                corrupted(ctx, memory);
            }
        }
        (4, T_RET_G_ReadEntityPairCount) => {
            let count = memory.u32_at(destination);
            if count > ENTITY_PAIRS {
                log_error!("save file: %u entity pairs, more than the list's %u", count, ENTITY_PAIRS);
                corrupted(ctx, memory);
            }
        }
        (2, T_RET_Scr_ReadObjectRef) => {
            let number = i32::from(memory.u16_at(destination) as i16);
            if number < 0 {
                log_error!("save file: script object number %d", number);
                corrupted(ctx, memory);
            }
        }
        (2, T_RET_Scr_ReadObjectCount) => {
            let count = u32::from(memory.u16_at(destination));
            if count >= TABLE_ENTRIES {
                log_error!("save file: %u script objects, more than the table's %u", count, TABLE_ENTRIES - 1);
                corrupted(ctx, memory);
            }
        }
        _ => {}
    }
});

// (number = r3, masked to a u16) -> remapped variable. The object-ref field this
// resolves is a raw u16, so refusing negatives where the numbers are read does
// not cover it: a value at or past the table's end reads past it, and the entry
// found drives a write past the object-record array. Refused.
guest_hook!(T_Scr_ResolveObjectRef, |ctx, memory| {
    let number = ctx.r3.as_u32() & 0xFFFF;
    if number >= TABLE_ENTRIES {
        log_error!("save file: script object reference %u, more than the table's %u", number, TABLE_ENTRIES - 1);
        corrupted(ctx, memory);
    }
    T_Scr_ResolveObjectRef.original(ctx, memory);
});

// (number = r3, sign-extended by the caller) -> parent variable
guest_hook!(T_Scr_SavedObject, |ctx, memory| {
    if ctx.r3.as_i32() < 0 {
        log_error!("save file: script object number %d", ctx.r3.as_i32());
        corrupted(ctx, memory);
    }
    T_Scr_SavedObject.original(ctx, memory);
});
