// LZX decompression: libmspack's lzxd.c, with the bit reader (readbits.h) and the
// Huffman decoder (readhuff.h) it builds on, in Rust. A title update's executable patch
// is LZX: each of its records is a stream that rebuilds a run of the executable, and
// an executable stored with "normal" compression is one stream as a whole. It follows
// the C line by line -- the same state, the same order of reads, the same checks -- so
// that it makes the same bytes and fails where libmspack fails; where the C would read
// or write outside its buffers on a damaged stream, this returns the error libmspack
// returns for a damaged stream instead.
//
// The C reads its input and writes its output through an mspack_system. The one
// libmspack is used with here is the in-memory one of XenonRecomp's patcher, so the
// stream reads from a slice and writes into one: a read gives what is left, up to what
// was asked, and a write stores what fits.
//
// (C) 2003-2023 Stuart Caie (lzxd.c, lzx.h, readbits.h, readhuff.h; libmspack commit
// 3059077). The LZX method was created by Jonathan Forbes and Tomi Poutanen, adapted by
// Microsoft Corporation. make_decode_table() was originally coded by David Tritscher.
//
// This file is part of a port of libmspack and, like it, is free software; you can
// redistribute it and/or modify it under the terms of the GNU Lesser General Public
// License (LGPL) version 2.1 as published by the Free Software Foundation. It is
// distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
// even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See
// the GNU Lesser General Public License for more details (LICENSE.LGPL-2.1 and
// NOTICE.md, beside src/).

// Some constants defined by the LZX specification.
const MIN_MATCH: i32 = 2;
const MAX_MATCH: i32 = 257;
const NUM_CHARS: u32 = 256;
const BLOCKTYPE_INVALID: u8 = 0; // also block types 4-7
const BLOCKTYPE_VERBATIM: u8 = 1;
const BLOCKTYPE_ALIGNED: u8 = 2;
const BLOCKTYPE_UNCOMPRESSED: u8 = 3;
const PRETREE_NUM_ELEMENTS: u32 = 20;
const ALIGNED_NUM_ELEMENTS: u32 = 8; // aligned offset tree's elements
const NUM_PRIMARY_LENGTHS: i32 = 7; // missing from the specification
const NUM_SECONDARY_LENGTHS: u32 = 249; // length tree's elements

// The Huffman trees: their symbols, and how many bits of a code one lookup decodes.
const PRETREE_MAXSYMBOLS: u32 = PRETREE_NUM_ELEMENTS;
const PRETREE_TABLEBITS: u32 = 6;
const MAINTREE_MAXSYMBOLS: u32 = NUM_CHARS + 290 * 8;
const MAINTREE_TABLEBITS: u32 = 12;
const LENGTH_MAXSYMBOLS: u32 = NUM_SECONDARY_LENGTHS + 1;
const LENGTH_TABLEBITS: u32 = 12;
const ALIGNED_MAXSYMBOLS: u32 = ALIGNED_NUM_ELEMENTS;
const ALIGNED_TABLEBITS: u32 = 7;
// Reading code lengths may run past the last one; the tables have this much room.
const LENTABLE_SAFETY: u32 = 64;

/// The size of a frame: the output comes out a frame at a time.
pub const FRAME_SIZE: u32 = 32768;

// readhuff.h's: no code is longer.
const HUFF_MAXBITS: u32 = 16;
// The width of the bit buffer, an unsigned int.
const BITBUF_WIDTH: u32 = 32;

/// libmspack's error codes, those its LZX decoder returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// MSPACK_ERR_ARGS: a call made wrongly.
    Args,
    /// MSPACK_ERR_READ: the input ran out.
    Read,
    /// MSPACK_ERR_WRITE: the output had no room.
    Write,
    /// MSPACK_ERR_DECRUNCH: the stream is damaged.
    Decrunch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Error::Args => "LZX: bad arguments",
            Error::Read => "LZX: the stream is cut short",
            Error::Write => "LZX: more output than room for it",
            Error::Decrunch => "LZX: the stream is damaged",
        })
    }
}

type Result<T = ()> = std::result::Result<T, Error>;

// LZX uses 'position slots' to represent match offsets. For every match, a small
// 'position slot' number and a small offset from that slot are encoded instead of one
// large offset.
//
// The number of slots is decided by how many are needed to encode the largest offset
// for a given window size. This is easy when the gap between slots is less than 128Kb,
// it's a linear relationship. But when extra_bits reaches its limit of 17 (because LZX
// can only ensure reading 17 bits of data at a time), we can only jump 128Kb at a time
// and have to start using more and more position slots as each window size doubles.
//
// POSITION_BASE is an index to the position slot bases; EXTRA_BITS states how many bits
// of offset-from-base data is needed. They are calculated as follows:
//   extra_bits[i] = 0 where i < 4
//   extra_bits[i] = floor(i/2)-1 where i >= 4 && i < 36
//   extra_bits[i] = 17 where i >= 36
//   position_base[0] = 0
//   position_base[i] = position_base[i-1] + (1 << extra_bits[i-1])
const POSITION_SLOTS: [u32; 11] = [30, 32, 34, 36, 38, 42, 50, 66, 98, 162, 290];
const EXTRA_BITS: [u8; 36] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15,
    16, 16,
];
// The table the C spells out, computed by the rule above.
const POSITION_BASE: [u32; 290] = {
    let mut base = [0u32; 290];
    let mut i = 1;
    while i < 290 {
        let extra = if i - 1 < 36 { EXTRA_BITS[i - 1] as u32 } else { 17 };
        base[i] = base[i - 1] + (1 << extra);
        i += 1;
    }
    base
};

// A Huffman tree: its code lengths and the table made from them (readhuff.h's
// HUFF_LEN, HUFF_TABLE, TABLEBITS and MAXSYMBOLS for one table name).
struct Tree {
    tablebits: u32,
    maxsymbols: u32,
    len: Vec<u8>,
    table: Vec<u16>,
    // lzxd.c's LENGTH_empty, for the one tree that may be empty.
    empty: bool,
}

impl Tree {
    fn new(maxsymbols: u32, tablebits: u32) -> Tree {
        Tree {
            tablebits,
            maxsymbols,
            len: vec![0; (maxsymbols + LENTABLE_SAFETY) as usize],
            table: vec![0; ((1 << tablebits) + maxsymbols * 2) as usize],
            empty: false,
        }
    }

    // BUILD_TABLE.
    fn build(&mut self) -> Result {
        if make_decode_table(self.maxsymbols, self.tablebits, &self.len, &mut self.table) {
            return Err(Error::Decrunch);
        }
        Ok(())
    }

    // BUILD_TABLE_MAYBE_EMPTY: a tree with no codes at all is allowed, as many CAB
    // files have a length tree that is empty because their block has no matches; it
    // is then not decoded with.
    fn build_maybe_empty(&mut self) -> Result {
        self.empty = false;
        if make_decode_table(self.maxsymbols, self.tablebits, &self.len, &mut self.table) {
            if self.len[..self.maxsymbols as usize].iter().any(|&len| len > 0) {
                return Err(Error::Decrunch);
            }
            self.empty = true;
        }
        Ok(())
    }
}

// make_decode_table(nsyms, nbits, length[], table[]), for bits read from the MSB:
// builds a fast Huffman decoding table from a canonical Huffman code lengths table.
// Symbols with a code length of nbits or less are decoded in one lookup; longer codes
// walk a tree stored after the direct entries. True on an error: lengths that overrun
// the table or leave it incomplete. An entry the C would write outside the table, which
// only lengths that are wrong anyway can ask for, is the same error.
fn make_decode_table(nsyms: u32, nbits: u32, length: &[u8], table: &mut [u16]) -> bool {
    make_decode_table_checked(nsyms, nbits, length, table).unwrap_or(true)
}

fn make_decode_table_checked(nsyms: u32, nbits: u32, length: &[u8], table: &mut [u16]) -> Option<bool> {
    let mut pos: u32 = 0; // the current position in the decode table
    let mut table_mask: u32 = 1 << nbits;
    let mut bit_mask: u32 = table_mask >> 1; // don't do 0 length codes

    // Fill entries for codes short enough for a direct mapping.
    for bit_num in 1..=nbits {
        for sym in 0..nsyms {
            if *length.get(sym as usize)? as u32 != bit_num {
                continue;
            }
            let leaf = pos;
            pos += bit_mask;
            if pos > table_mask {
                return Some(true); // table overrun
            }
            // Fill all possible lookups of this symbol with the symbol itself.
            table.get_mut(leaf as usize..(leaf + bit_mask) as usize)?.fill(sym as u16);
        }
        bit_mask >>= 1;
    }

    // Exit with success if the table is now complete.
    if pos == table_mask {
        return Some(false);
    }

    // Mark all remaining table entries as unused.
    for sym in pos..table_mask {
        *table.get_mut(sym as usize)? = 0xFFFF;
    }

    // next_symbol = base of allocation for long codes
    let mut next_symbol: u32 = if (table_mask >> 1) < nsyms { nsyms } else { table_mask >> 1 };

    // Give ourselves room for codes to grow by up to 16 more bits: codes now start at
    // bit nbits+16 and end at (nbits+16-codelength).
    pos <<= 16;
    table_mask <<= 16;
    bit_mask = 1 << 15;

    for bit_num in nbits + 1..=HUFF_MAXBITS {
        for sym in 0..nsyms {
            if *length.get(sym as usize)? as u32 != bit_num {
                continue;
            }
            if pos >= table_mask {
                return Some(true); // table overflow
            }
            let mut leaf = pos >> 16;
            for fill in 0..bit_num - nbits {
                // If this path hasn't been taken yet, 'allocate' two entries. The C
                // keeps next_symbol in an unsigned short.
                if *table.get(leaf as usize)? == 0xFFFF {
                    *table.get_mut((next_symbol << 1) as usize)? = 0xFFFF;
                    *table.get_mut(((next_symbol << 1) + 1) as usize)? = 0xFFFF;
                    *table.get_mut(leaf as usize)? = next_symbol as u16;
                    next_symbol = (next_symbol + 1) & 0xFFFF;
                }
                // Follow the path and select either left or right for the next bit.
                leaf = (*table.get(leaf as usize)? as u32) << 1;
                if (pos >> (15 - fill)) & 1 != 0 {
                    leaf += 1;
                }
            }
            *table.get_mut(leaf as usize)? = sym as u16;
            pos += bit_mask;
        }
        bit_mask >>= 1;
    }

    // Full table?
    Some(pos != table_mask)
}

// readbits.h's state, for bits read from the MSB, with READ_BYTES as lzxd.c defines it:
// two bytes at a time, little-endian. The C keeps these in locals between STORE_BITS
// and RESTORE_BITS; here they stay in the stream, which comes to the same.
struct Bits<'a> {
    // The input file: what is left of it is read into inbuf as needed.
    input: &'a [u8],
    input_at: usize,
    inbuf: Vec<u8>,
    i_ptr: usize,
    i_end: usize,
    bit_buffer: u32,
    bits_left: i32,
    input_end: bool,
}

impl Bits<'_> {
    // read_input(): the next bufferful. At the end of the input the reader may want
    // bits nobody uses, so two zero bytes are made up once; after those, reading on is
    // an error.
    fn read_input(&mut self) -> Result {
        let read = self.inbuf.len().min(self.input.len() - self.input_at);
        self.inbuf[..read].copy_from_slice(&self.input[self.input_at..self.input_at + read]);
        self.input_at += read;
        let mut read = read;
        if read == 0 {
            if self.input_end {
                return Err(Error::Read);
            }
            read = 2;
            self.inbuf[0] = 0;
            self.inbuf[1] = 0;
            self.input_end = true;
        }
        self.i_ptr = 0;
        self.i_end = read;
        Ok(())
    }

    // READ_IF_NEEDED
    fn read_if_needed(&mut self) -> Result {
        if self.i_ptr >= self.i_end {
            self.read_input()?;
        }
        Ok(())
    }

    // READ_IF_NEEDED; *i_ptr++
    fn next_byte(&mut self) -> Result<u8> {
        self.read_if_needed()?;
        let byte = self.inbuf[self.i_ptr];
        self.i_ptr += 1;
        Ok(byte)
    }

    // READ_BYTES and INJECT_BITS
    fn read_bytes(&mut self) -> Result {
        let b0 = self.next_byte()? as u32;
        let b1 = self.next_byte()? as u32;
        // bits_left is at most 16 here, as no more than 17 bits are ever ensured.
        let shift = BITBUF_WIDTH as i32 - 16 - self.bits_left;
        self.bit_buffer |= ((b1 << 8) | b0).checked_shl(shift as u32).unwrap_or(0);
        self.bits_left += 16;
        Ok(())
    }

    // ENSURE_BITS
    fn ensure(&mut self, nbits: i32) -> Result {
        while self.bits_left < nbits {
            self.read_bytes()?;
        }
        Ok(())
    }

    // PEEK_BITS: nbits is 1 to 17.
    fn peek(&self, nbits: u32) -> u32 {
        self.bit_buffer.checked_shr(BITBUF_WIDTH - nbits).unwrap_or(0)
    }

    // REMOVE_BITS
    fn remove(&mut self, nbits: u32) {
        self.bit_buffer = self.bit_buffer.checked_shl(nbits).unwrap_or(0);
        self.bits_left -= nbits as i32;
    }

    // READ_BITS
    fn read(&mut self, nbits: u32) -> Result<u32> {
        self.ensure(nbits as i32)?;
        let value = self.peek(nbits);
        self.remove(nbits);
        Ok(value)
    }

    // READ_HUFFSYM: a table built without an error has no entry that leads nowhere, so
    // a lookup outside it is the C's HUFF_ERROR too.
    fn read_huffsym(&mut self, tree: &Tree) -> Result<u32> {
        self.ensure(HUFF_MAXBITS as i32)?;
        let mut sym = *tree.table.get(self.peek(tree.tablebits) as usize).ok_or(Error::Decrunch)? as u32;
        if sym >= tree.maxsymbols {
            // HUFF_TRAVERSE
            let mut idx: u32 = 1 << (BITBUF_WIDTH - tree.tablebits);
            loop {
                idx >>= 1;
                if idx == 0 {
                    return Err(Error::Decrunch);
                }
                let next = (sym << 1) | (self.bit_buffer & idx != 0) as u32;
                sym = *tree.table.get(next as usize).ok_or(Error::Decrunch)? as u32;
                if sym < tree.maxsymbols {
                    break;
                }
            }
        }
        let len = *tree.len.get(sym as usize).ok_or(Error::Decrunch)?;
        self.remove(len as u32);
        Ok(sym)
    }
}

// Where the frame being written out lies: the window, or the copy of it the Intel E8
// transform was undone in (the C's o_ptr points into one or the other).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Out {
    E8,
    Window,
}

/// An LZX stream being decompressed (struct lzxd_stream).
pub struct Lzxd<'a> {
    output: &'a mut [u8],
    output_at: usize,

    offset: i64, // number of bytes actually output
    length: i64, // overall decompressed length of the stream

    /// The decoding window. A caller that gives the stream reference data without
    /// lzxd_set_reference_data() -- XenonRecomp's patcher does -- writes it here.
    pub window: Vec<u8>,
    window_size: u32,
    /// LZX DELTA reference data size: how far before the start of the output a match
    /// may reach, into what was put at the end of the window.
    pub ref_data_size: u32,
    num_offsets: u32,    // number of match_offset entries in the table
    window_posn: u32,    // decompression offset within the window
    frame_posn: u32,     // current frame offset within the window
    frame: u32,          // the number of 32kb frames processed
    reset_interval: u32, // which frame do we reset the compressor?

    r0: u32, // for the LRU offset system
    r1: u32,
    r2: u32,
    block_length: u32,    // uncompressed length of this LZX block
    block_remaining: u32, // uncompressed bytes still left to decode

    intel_filesize: i32, // magic header value used for the transform

    intel_started: bool, // has intel E8 decoding started?
    block_type: u8,      // type of the current block
    header_read: bool,   // have we started decoding at all yet?
    is_delta: bool,      // does the stream follow the LZX DELTA spec?

    error: Option<Error>,

    bits: Bits<'a>,
    o_buf: Out,
    o_ptr: usize,
    o_end: usize,

    pretree: Tree,
    maintree: Tree,
    length_tree: Tree,
    aligned: Tree,

    // Used purely for doing the Intel E8 transform.
    e8_buf: Vec<u8>,
}

impl<'a> Lzxd<'a> {
    /// lzxd_init(): a stream reading `input` and writing into `output`. window_bits is
    /// 15 to 21 for regular LZX, 17 to 25 for LZX DELTA; reset_interval the frames
    /// between resets of the bitstream, 0 for never; output_length the whole output's
    /// length, which the Intel E8 transform needs, or 0 for unknown. None for arguments
    /// out of range.
    pub fn new(
        input: &'a [u8],
        output: &'a mut [u8],
        window_bits: i32,
        reset_interval: i32,
        input_buffer_size: i32,
        output_length: i64,
        is_delta: bool,
    ) -> Option<Lzxd<'a>> {
        // LZX DELTA window sizes are between 2^17 (128KiB) and 2^25 (32MiB), regular
        // LZX windows between 2^15 (32KiB) and 2^21 (2MiB).
        if is_delta {
            if !(17..=25).contains(&window_bits) {
                return None;
            }
        } else if !(15..=21).contains(&window_bits) {
            return None;
        }
        if reset_interval < 0 || output_length < 0 {
            return None;
        }
        // Round the input buffer size up to a multiple of two.
        let input_buffer_size = input_buffer_size.checked_add(1)? & -2;
        if input_buffer_size < 2 {
            return None;
        }
        let window_size = 1u32 << window_bits;
        let mut lzx = Lzxd {
            output,
            output_at: 0,
            offset: 0,
            length: output_length,
            window: vec![0; window_size as usize],
            window_size,
            ref_data_size: 0,
            num_offsets: POSITION_SLOTS[(window_bits - 15) as usize] << 3,
            window_posn: 0,
            frame_posn: 0,
            frame: 0,
            reset_interval: reset_interval as u32,
            r0: 0,
            r1: 0,
            r2: 0,
            block_length: 0,
            block_remaining: 0,
            intel_filesize: 0,
            intel_started: false,
            block_type: BLOCKTYPE_INVALID,
            header_read: false,
            is_delta,
            error: None,
            // INIT_BITS
            bits: Bits {
                input,
                input_at: 0,
                inbuf: vec![0; input_buffer_size as usize],
                i_ptr: 0,
                i_end: 0,
                bit_buffer: 0,
                bits_left: 0,
                input_end: false,
            },
            o_buf: Out::E8,
            o_ptr: 0,
            o_end: 0,
            pretree: Tree::new(PRETREE_MAXSYMBOLS, PRETREE_TABLEBITS),
            maintree: Tree::new(MAINTREE_MAXSYMBOLS, MAINTREE_TABLEBITS),
            length_tree: Tree::new(LENGTH_MAXSYMBOLS, LENGTH_TABLEBITS),
            aligned: Tree::new(ALIGNED_MAXSYMBOLS, ALIGNED_TABLEBITS),
            e8_buf: vec![0; FRAME_SIZE as usize],
        };
        lzx.reset_state();
        Some(lzx)
    }

    fn reset_state(&mut self) {
        self.r0 = 1;
        self.r1 = 1;
        self.r2 = 1;
        self.header_read = false;
        self.block_remaining = 0;
        self.block_type = BLOCKTYPE_INVALID;
        // Initialise the tables to 0, because deltas will be applied to them.
        self.maintree.len[..MAINTREE_MAXSYMBOLS as usize].fill(0);
        self.length_tree.len[..LENGTH_MAXSYMBOLS as usize].fill(0);
    }

    /// lzxd_set_reference_data(): LZX DELTA reference data, put at the end of the
    /// window for matches to reach back into. Before the first decompress() only.
    pub fn set_reference_data(&mut self, data: &[u8]) -> Result {
        if !self.is_delta {
            // Only LZX DELTA streams support reference data.
            return Err(Error::Args);
        }
        if self.offset != 0 {
            // Too late to set reference data after decoding starts.
            return Err(Error::Args);
        }
        if data.len() > self.window_size as usize {
            // The reference is longer than the window.
            return Err(Error::Args);
        }
        let start = self.window_size as usize - data.len();
        self.window.get_mut(start..start + data.len()).ok_or(Error::Args)?.copy_from_slice(data);
        self.ref_data_size = data.len() as u32;
        Ok(())
    }

    /// lzxd_set_output_length(): see output_length in new().
    pub fn set_output_length(&mut self, out_bytes: i64) {
        if out_bytes > 0 {
            self.length = out_bytes;
        }
    }

    // The output file's write(): what fits of it, and how much that was.
    fn write(&mut self, from: Out, at: usize, count: usize) -> usize {
        let source = match from {
            Out::E8 => &self.e8_buf,
            Out::Window => &self.window,
        };
        let total = count.min(self.output.len() - self.output_at);
        let Some(bytes) = source.get(at..at + total) else { return 0 };
        self.output[self.output_at..self.output_at + total].copy_from_slice(bytes);
        self.output_at += total;
        total
    }

    // lzxd_read_lens(): code lengths for symbols first to last of a tree, stored in
    // their own special LZX way: as deltas, coded with a pretree read first.
    fn read_lens(bits: &mut Bits, pretree: &mut Tree, lens: &mut [u8], first: u32, last: u32) -> Result {
        // Read lengths for the pretree (20 symbols, lengths stored in fixed 4 bits).
        for x in 0..20 {
            pretree.len[x] = bits.read(4)? as u8;
        }
        pretree.build()?;

        let mut x = first as usize;
        while x < last as usize {
            let z = bits.read_huffsym(pretree)? as i32;
            // lens[x++] = value: the runs may go past last, into the room the tables
            // have for it.
            let set = |lens: &mut [u8], x: &mut usize, value: u8| -> Result {
                *lens.get_mut(*x).ok_or(Error::Decrunch)? = value;
                *x += 1;
                Ok(())
            };
            if z == 17 {
                // code = 17, run of ([read 4 bits]+4) zeros
                let y = bits.read(4)? + 4;
                for _ in 0..y {
                    set(lens, &mut x, 0)?;
                }
            } else if z == 18 {
                // code = 18, run of ([read 5 bits]+20) zeros
                let y = bits.read(5)? + 20;
                for _ in 0..y {
                    set(lens, &mut x, 0)?;
                }
            } else if z == 19 {
                // code = 19, run of ([read 1 bit]+4) [read huffman symbol]
                let y = bits.read(1)? + 4;
                let z = bits.read_huffsym(pretree)? as i32;
                let mut z = *lens.get(x).ok_or(Error::Decrunch)? as i32 - z;
                if z < 0 {
                    z += 17;
                }
                for _ in 0..y {
                    set(lens, &mut x, z as u8)?;
                }
            } else {
                // code = 0 to 16, delta current length entry
                let mut z = *lens.get(x).ok_or(Error::Decrunch)? as i32 - z;
                if z < 0 {
                    z += 17;
                }
                set(lens, &mut x, z as u8)?;
            }
        }
        Ok(())
    }

    /// lzxd_decompress(): the next out_bytes of the output. Bytes decoded past them
    /// are kept for the next call. After an error, the stream gives that error again.
    pub fn decompress(&mut self, out_bytes: i64) -> Result {
        // easy answers
        if out_bytes < 0 {
            return Err(Error::Args);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = self.decompress_frames(out_bytes);
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }

    fn decompress_frames(&mut self, mut out_bytes: i64) -> Result {
        // Flush out any stored-up bytes before we begin.
        let mut i = (self.o_end - self.o_ptr) as i64;
        if i > out_bytes {
            i = out_bytes;
        }
        if i != 0 {
            if self.write(self.o_buf, self.o_ptr, i as usize) != i as usize {
                return Err(Error::Write);
            }
            self.o_ptr += i as usize;
            self.offset += i;
            out_bytes -= i;
        }
        if out_bytes == 0 {
            return Ok(());
        }

        // Restore local state.
        let mut window_posn = self.window_posn;
        let mut r0 = self.r0;
        let mut r1 = self.r1;
        let mut r2 = self.r2;

        let end_frame = ((self.offset + out_bytes) / FRAME_SIZE as i64) as u32 + 1;

        while self.frame < end_frame {
            // Have we reached the reset interval (if there is one)?
            if self.reset_interval != 0 && self.frame.is_multiple_of(self.reset_interval) {
                // Bytes left of a block here are a file format error; libmspack warns
                // once and extracts what it can, and so does this, without the warning.
                // Re-read the Intel header and reset the Huffman lengths.
                self.reset_state();
                r0 = self.r0;
                r1 = self.r1;
                r2 = self.r2;
            }

            // The LZX DELTA format has a chunk size, not present in the LZX format.
            if self.is_delta {
                self.bits.ensure(16)?;
                self.bits.remove(16);
            }

            // Read the header if necessary: one bit; if it is 1, the Intel file size
            // (32 bits) follows, else it is 0.
            if !self.header_read {
                let mut j = 0;
                let mut i = self.bits.read(1)?;
                if i != 0 {
                    i = self.bits.read(16)?;
                    j = self.bits.read(16)?;
                }
                self.intel_filesize = ((i << 16) | j) as i32;
                self.header_read = true;
            }

            // The size of the frame: all frames are 32k except the final frame, which
            // is 32kb or less. This can only be calculated when the length is known.
            let mut frame_size = FRAME_SIZE;
            if self.length != 0 && (self.length - self.offset) < frame_size as i64 {
                frame_size = (self.length - self.offset) as u32;
            }

            // Decode until one more frame is available.
            let mut bytes_todo = self.frame_posn.wrapping_add(frame_size).wrapping_sub(window_posn) as i32;
            while bytes_todo > 0 {
                // Initialise a new block, if one is needed.
                if self.block_remaining == 0 {
                    // Realign if the previous block was an odd-sized uncompressed block.
                    if self.block_type == BLOCKTYPE_UNCOMPRESSED && self.block_length & 1 != 0 {
                        self.bits.read_if_needed()?;
                        self.bits.i_ptr += 1;
                    }

                    // Read the block type (3 bits) and the block length (24 bits).
                    self.block_type = self.bits.read(3)? as u8;
                    let i = self.bits.read(16)?;
                    let j = self.bits.read(8)?;
                    self.block_length = (i << 8) | j;
                    self.block_remaining = self.block_length;

                    // Read the individual block headers.
                    match self.block_type {
                        BLOCKTYPE_ALIGNED | BLOCKTYPE_VERBATIM => {
                            if self.block_type == BLOCKTYPE_ALIGNED {
                                // Read the lengths of and build the aligned Huffman
                                // decoding tree; the rest of the aligned header is the
                                // same as verbatim.
                                for i in 0..8 {
                                    self.aligned.len[i] = self.bits.read(3)? as u8;
                                }
                                self.aligned.build()?;
                            }
                            // Read the lengths of and build the main Huffman decoding
                            // tree.
                            Self::read_lens(&mut self.bits, &mut self.pretree, &mut self.maintree.len, 0, 256)?;
                            Self::read_lens(
                                &mut self.bits,
                                &mut self.pretree,
                                &mut self.maintree.len,
                                256,
                                NUM_CHARS + self.num_offsets,
                            )?;
                            self.maintree.build()?;
                            // If the literal 0xE8 is anywhere in the block...
                            if self.maintree.len[0xE8] != 0 {
                                self.intel_started = true;
                            }
                            // Read the lengths of and build the lengths Huffman
                            // decoding tree.
                            Self::read_lens(
                                &mut self.bits,
                                &mut self.pretree,
                                &mut self.length_tree.len,
                                0,
                                NUM_SECONDARY_LENGTHS,
                            )?;
                            self.length_tree.build_maybe_empty()?;
                        }

                        BLOCKTYPE_UNCOMPRESSED => {
                            // Because we can't assume otherwise.
                            self.intel_started = true;

                            // Read 1-16 (not 0-15) bits to align to bytes.
                            if self.bits.bits_left == 0 {
                                self.bits.ensure(16)?;
                            }
                            self.bits.bits_left = 0;
                            self.bits.bit_buffer = 0;

                            // Read 12 bytes of stored R0 / R1 / R2 values.
                            let mut buf = [0u8; 12];
                            for byte in &mut buf {
                                *byte = self.bits.next_byte()?;
                            }
                            r0 = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                            r1 = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
                            r2 = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
                        }

                        _ => return Err(Error::Decrunch), // bad block type
                    }
                }

                // Decode more of the block: run = min(what's available, what's needed).
                let mut this_run = self.block_remaining as i32;
                if this_run > bytes_todo {
                    this_run = bytes_todo;
                }

                // Assume we decode exactly this_run bytes, for now.
                bytes_todo -= this_run;
                self.block_remaining -= this_run as u32;

                // Decode at least this_run bytes.
                match self.block_type {
                    BLOCKTYPE_ALIGNED | BLOCKTYPE_VERBATIM => {
                        while this_run > 0 {
                            let mut main_element = self.bits.read_huffsym(&self.maintree)? as i32;
                            if main_element < NUM_CHARS as i32 {
                                // literal: 0 to NUM_CHARS-1
                                *self.window.get_mut(window_posn as usize).ok_or(Error::Decrunch)? = main_element as u8;
                                window_posn += 1;
                                this_run -= 1;
                                continue;
                            }

                            // match: NUM_CHARS + ((slot<<3) | length_header (3 bits))
                            main_element -= NUM_CHARS as i32;

                            // Get the match length.
                            let mut match_length = main_element & NUM_PRIMARY_LENGTHS;
                            if match_length == NUM_PRIMARY_LENGTHS {
                                if self.length_tree.empty {
                                    // A LENGTH symbol is needed but the tree is empty.
                                    return Err(Error::Decrunch);
                                }
                                let length_footer = self.bits.read_huffsym(&self.length_tree)? as i32;
                                match_length += length_footer;
                            }
                            match_length += MIN_MATCH;

                            // Get the match offset.
                            let mut match_offset = (main_element >> 3) as u32;
                            match match_offset {
                                0 => match_offset = r0,
                                1 => {
                                    match_offset = r1;
                                    r1 = r0;
                                    r0 = match_offset;
                                }
                                2 => {
                                    match_offset = r2;
                                    r2 = r0;
                                    r0 = match_offset;
                                }
                                slot => {
                                    let extra = if slot >= 36 { 17 } else { EXTRA_BITS[slot as usize] as u32 };
                                    match_offset =
                                        POSITION_BASE.get(slot as usize).ok_or(Error::Decrunch)?.wrapping_sub(2);
                                    if extra >= 3 && self.block_type == BLOCKTYPE_ALIGNED {
                                        if extra > 3 {
                                            let verbatim_bits = self.bits.read(extra - 3)?; // 1-14 bits
                                            match_offset = match_offset.wrapping_add(verbatim_bits << 3);
                                        }
                                        let aligned_bits = self.bits.read_huffsym(&self.aligned)?;
                                        match_offset = match_offset.wrapping_add(aligned_bits);
                                    } else if extra != 0 {
                                        let verbatim_bits = self.bits.read(extra)?; // 1-17 bits
                                        match_offset = match_offset.wrapping_add(verbatim_bits);
                                    }
                                    // Update the repeated offset LRU queue.
                                    r2 = r1;
                                    r1 = r0;
                                    r0 = match_offset;
                                }
                            }

                            // LZX DELTA uses the max match length to signal an even
                            // longer match.
                            if match_length == MAX_MATCH && self.is_delta {
                                let mut extra_len;
                                self.bits.ensure(3)?; // 4 entry huffman tree
                                if self.bits.peek(1) == 0 {
                                    self.bits.remove(1); // '0' -> 8 extra length bits
                                    extra_len = self.bits.read(8)?;
                                } else if self.bits.peek(2) == 2 {
                                    self.bits.remove(2); // '10' -> 10 extra length bits + 0x100
                                    extra_len = self.bits.read(10)?;
                                    extra_len += 0x100;
                                } else if self.bits.peek(3) == 6 {
                                    self.bits.remove(3); // '110' -> 12 extra length bits + 0x500
                                    extra_len = self.bits.read(12)?;
                                    extra_len += 0x500;
                                } else {
                                    self.bits.remove(3); // '111' -> 15 extra length bits
                                    extra_len = self.bits.read(15)?;
                                }
                                match_length += extra_len as i32;
                            }

                            if window_posn as u64 + match_length as u64 > self.window_size as u64 {
                                // The match ran over the window wrap.
                                return Err(Error::Decrunch);
                            }

                            // Copy the match.
                            let mut rundest = window_posn as usize;
                            let mut i = match_length as usize;
                            // Does the match offset wrap the window?
                            if match_offset > window_posn {
                                if match_offset as i64 > self.offset
                                    && (match_offset - window_posn) > self.ref_data_size
                                {
                                    // The match offset is beyond the LZX stream.
                                    return Err(Error::Decrunch);
                                }
                                // j = length from the match offset to the end of the window
                                let j = (match_offset - window_posn) as usize;
                                if j > self.window_size as usize {
                                    // The match offset is beyond the window's boundaries.
                                    return Err(Error::Decrunch);
                                }
                                let mut runsrc = self.window_size as usize - j;
                                if j < i {
                                    // If the match goes over the window edge, do two
                                    // copy runs.
                                    i -= j;
                                    copy_forward(&mut self.window, runsrc, rundest, j)?;
                                    rundest += j;
                                    runsrc = 0;
                                }
                                copy_forward(&mut self.window, runsrc, rundest, i)?;
                            } else {
                                let runsrc = rundest - match_offset as usize;
                                copy_forward(&mut self.window, runsrc, rundest, i)?;
                            }

                            this_run -= match_length;
                            window_posn += match_length as u32;
                        }
                    }

                    BLOCKTYPE_UNCOMPRESSED => {
                        // As this_run is limited not to wrap a frame, this also means
                        // it won't wrap the window (as the window is a multiple of 32k).
                        let mut rundest = window_posn as usize;
                        window_posn = window_posn.wrapping_add(this_run as u32);
                        while this_run > 0 {
                            let mut i = (self.bits.i_end - self.bits.i_ptr) as i32;
                            if i == 0 {
                                self.bits.read_if_needed()?;
                            } else {
                                if i > this_run {
                                    i = this_run;
                                }
                                let (from, count) = (self.bits.i_ptr, i as usize);
                                self.window
                                    .get_mut(rundest..rundest + count)
                                    .ok_or(Error::Decrunch)?
                                    .copy_from_slice(&self.bits.inbuf[from..from + count]);
                                rundest += count;
                                self.bits.i_ptr += count;
                                this_run -= i;
                            }
                        }
                    }

                    _ => return Err(Error::Decrunch), // might as well
                }

                // Did the final match overrun our desired this_run length?
                if this_run < 0 {
                    if this_run.unsigned_abs() > self.block_remaining {
                        // The overrun went past the end of the block.
                        return Err(Error::Decrunch);
                    }
                    self.block_remaining -= this_run.unsigned_abs();
                }
            }

            // Streams don't extend over frame boundaries.
            if window_posn.wrapping_sub(self.frame_posn) != frame_size {
                // Decoded beyond the output frame's limits.
                return Err(Error::Decrunch);
            }

            // Re-align the input bitstream.
            if self.bits.bits_left > 0 {
                self.bits.ensure(16)?;
            }
            if self.bits.bits_left & 15 != 0 {
                self.bits.remove((self.bits.bits_left & 15) as u32);
            }

            // Check that we've used all of the previous frame first.
            if self.o_ptr != self.o_end {
                return Err(Error::Decrunch);
            }

            let frame_start = self.frame_posn as usize;
            let frame_bytes = self.window.get(frame_start..frame_start + frame_size as usize).ok_or(Error::Decrunch)?;
            // Does this Intel block _really_ need decoding?
            if self.intel_started && self.intel_filesize != 0 && self.frame < 32768 && frame_size > 10 {
                let mut data = 0usize;
                let dataend = frame_size as usize - 10;
                let mut curpos = self.offset as i32;
                let filesize = self.intel_filesize;

                // Copy the E8 block to the E8 buffer and tweak it if needed.
                self.o_buf = Out::E8;
                self.o_ptr = 0;
                self.e8_buf[..frame_size as usize].copy_from_slice(frame_bytes);
                let e8 = &mut self.e8_buf;

                while data < dataend {
                    let byte = e8[data];
                    data += 1;
                    if byte != 0xE8 {
                        curpos = curpos.wrapping_add(1);
                        continue;
                    }
                    let abs_off = i32::from_le_bytes([e8[data], e8[data + 1], e8[data + 2], e8[data + 3]]);
                    if abs_off >= curpos.wrapping_neg() && abs_off < filesize {
                        let rel_off =
                            if abs_off >= 0 { abs_off.wrapping_sub(curpos) } else { abs_off.wrapping_add(filesize) };
                        e8[data..data + 4].copy_from_slice(&rel_off.to_le_bytes());
                    }
                    data += 4;
                    curpos = curpos.wrapping_add(5);
                }
            } else {
                self.o_buf = Out::Window;
                self.o_ptr = frame_start;
            }
            self.o_end = self.o_ptr + frame_size as usize;

            // Write a frame.
            let i = if out_bytes < frame_size as i64 { out_bytes as usize } else { frame_size as usize };
            if self.write(self.o_buf, self.o_ptr, i) != i {
                return Err(Error::Write);
            }
            self.o_ptr += i;
            self.offset += i as i64;
            out_bytes -= i as i64;

            // Advance the frame's start position.
            self.frame_posn += frame_size;
            self.frame += 1;

            // Wrap the window and frame position pointers.
            if window_posn == self.window_size {
                window_posn = 0;
            }
            if self.frame_posn == self.window_size {
                self.frame_posn = 0;
            }
        }

        if out_bytes != 0 {
            // Bytes left to output.
            return Err(Error::Decrunch);
        }

        // Store local state.
        self.window_posn = window_posn;
        self.r0 = r0;
        self.r1 = r1;
        self.r2 = r2;
        Ok(())
    }
}

// The C's byte-by-byte copy within the window, from `from` to `to`: a match may overlap
// what it writes, and repeats it then, as a forward copy does.
fn copy_forward(window: &mut [u8], from: usize, to: usize, count: usize) -> Result {
    if from.max(to).checked_add(count).is_none_or(|end| end > window.len()) {
        return Err(Error::Decrunch);
    }
    for k in 0..count {
        window[to + k] = window[from + k];
    }
    Ok(())
}
