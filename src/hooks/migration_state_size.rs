// A migrated game state is read only from inside its buffer.
//
// In-game host migration: the server streams its game state, a compressed
// save file, to the client it picked as the next host. mhead (sub_8218BCA0)
// takes a 1152-byte save header into the static header (0x83050600, what
// sub_822664A8(0) returns) and points the migration buffer at 0x830FB150,
// 0x180000 bytes, which ends at the buffer's own descriptor (0x8327B150,
// sub_82289370); mdata (sub_8218BA58) fills it in 1200-byte chunks, each kept
// inside the 0x180000. When the client becomes the host, G_InitGame
// (sub_82233A68) restores the state through sub_8223F360, which opens the buffer
// as a file (sub_823125C0) whose length is the header's word at +1116, and the
// header check (sub_8223F2B0) first checksums that many bytes from the buffer
// (sub_82265000). Nothing compares that length with the buffer: a server can
// name any length, and the checksum and the reads run past the buffer.
//
// Inside the file the state is in segments, each a size word (the word itself
// included) and size-4 bytes of compressed data. Opening segment n
// (sub_82312360) adds up the first n size words from offset 0 and hands the
// inflater the n-th segment's data and size-4 as its input. No size word is
// compared with the file's length (+4) either, so a size word can put the
// inflater's input anywhere in guest memory.
//
// The server writes the state into a buffer of the same 0x180000 (sub_822C4DB0
// sets the same one up before saving into it), so a length above it, or a
// segment that runs past the length, is a file no server wrote. Both are
// refused with the error the file's own reader raises for data it cannot read
// (sub_82311F50 at 82311FE0: Com_Error(ERR_DROP, "Trying to read corrupted
// file")), before the read. The segment check is in the segment opener, so it
// holds for every compressed file the title reads this way; a well-formed file
// passes it untouched.
//
// Reads only: the console would fault on an unmapped address, this build reads
// zeros. MP-only: the campaign has no host migration.

use crate::guest::{Register, guest_hook};
use crate::hooks::com_error::corrupted;
use crate::log::log_error;
use crate::title::{T_DATA_MigrationHeader, T_G_LoadMigratedGame, T_MemFile_BeginSegment};

const BUFFER_SIZE: u32 = 0x180000; // the migration buffer, as mhead and the server set it
const HEADER_LENGTH: u32 = 1116; // the header's file length
const FILE_DATA: u32 = 0; // file object: data, length, read mode, segment
const FILE_LENGTH: u32 = 4;
const FILE_WRITING: u32 = 29;

guest_hook!(T_G_LoadMigratedGame, |ctx, memory| {
    let length = memory.u32_at(T_DATA_MigrationHeader.wrapping_add(HEADER_LENGTH));
    if length > BUFFER_SIZE {
        log_error!("host migration: the game state names %u bytes, more than its %u-byte buffer", length, BUFFER_SIZE);
        corrupted(ctx, memory);
    }
    T_G_LoadMigratedGame.original(ctx, memory);
});

// (file = r3, segment = r4; a negative segment closes the file)
guest_hook!(T_MemFile_BeginSegment, |ctx, memory| {
    let file = ctx.r3.as_u32();
    let segment = ctx.r4.as_i32();
    if segment >= 0 && memory.u8_at(file.wrapping_add(FILE_WRITING)) == 0 {
        let data = memory.u32_at(file.wrapping_add(FILE_DATA));
        let length = u64::from(memory.u32_at(file.wrapping_add(FILE_LENGTH)));
        let mut offset = 0u64;
        for i in 0..=segment {
            let size =
                if offset + 4 <= length { u64::from(memory.u32_at(data.wrapping_add(offset as u32))) } else { 0 };
            if size < 4 || offset + size > length {
                log_error!(
                    "save file: segment %d of %d runs past the file's %llu bytes (at %llu, size word %llu)",
                    i,
                    segment,
                    length,
                    offset,
                    size
                );
                corrupted(ctx, memory);
            }
            offset += size;
        }
    }
    T_MemFile_BeginSegment.original(ctx, memory);
});
