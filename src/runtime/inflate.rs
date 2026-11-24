// gunzip without zlib: RFC 1952's gzip member around RFC 1951's deflate, read the way zlib's
// inflate reads it with a 32 KB window and the gzip wrapper only (inflateInit2 with
// 16 + MAX_WBITS), so that it takes and refuses what zlib did: the header's method and
// reserved flags, its optional fields and CRC, the dynamic tables' limits and zlib's rule
// for incomplete codes, distances before the start, and the trailer's CRC and length. The
// title's XLAST (presence.rs) is the one stream it reads.

/// The `size` bytes a gzip stream holds, or None if it does not hold exactly those.
pub fn gunzip(packed: &[u8], size: usize) -> Option<Vec<u8>> {
    let mut input = Bits { data: packed, at: 0, bit: 0 };
    let header_end = gzip_header(packed)?;
    input.at = header_end;
    // Deflate expands at most 1032 times: no more room than the input could fill.
    let mut out = Vec::with_capacity(size.min(packed.len().saturating_mul(1032)));
    inflate(&mut input, &mut out, size)?;
    // The trailer, from the next whole byte: the data's CRC-32, then its length mod 2^32.
    input.align();
    let crc = input.le32()?;
    let length = input.le32()?;
    (crc == crc32(0, &out) && length == out.len() as u32 && out.len() == size).then_some(out)
}

// The header's length, if zlib takes it.
fn gzip_header(data: &[u8]) -> Option<usize> {
    let byte = |at: usize| data.get(at).copied();
    if byte(0)? != 0x1F || byte(1)? != 0x8B || byte(2)? != 8 {
        return None;
    }
    let flags = byte(3)?;
    if flags & 0xE0 != 0 {
        return None; // reserved flags
    }
    let mut at = 10; // the time, extra flags and system
    if data.len() < at {
        return None;
    }
    if flags & 0x04 != 0 {
        let extra = usize::from(byte(at)?) | usize::from(byte(at + 1)?) << 8;
        at += 2 + extra;
        if data.len() < at {
            return None;
        }
    }
    for flag in [0x08, 0x10] {
        // The name, then the comment: each up to its zero.
        if flags & flag != 0 {
            at += data.get(at..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 0x02 != 0 {
        let stored = u32::from(byte(at)?) | u32::from(byte(at + 1)?) << 8;
        if stored != crc32(0, &data[..at]) & 0xFFFF {
            return None;
        }
        at += 2;
    }
    Some(at)
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
}

impl Bits<'_> {
    fn get(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for i in 0..count {
            let byte = *self.data.get(self.at)?;
            value |= u32::from((byte >> self.bit) & 1) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.at += 1;
            }
        }
        Some(value)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.at += 1;
        }
    }

    fn le32(&mut self) -> Option<u32> {
        let bytes = self.data.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
}

// A canonical Huffman code: how many codes of each length, and the symbols in code order.
struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

#[derive(PartialEq)]
enum Kind {
    CodeLengths,
    LiteralLengths,
    Distances,
}

impl Huffman {
    // zlib's inflate_table: no lengths at all is taken (and fails when a code is read), an
    // over-subscribed set never, an incomplete one only for one code of one bit outside the
    // code-length code.
    fn new(lengths: &[u8], kind: Kind) -> Option<Self> {
        let mut count = [0u16; 16];
        for &length in lengths {
            count[usize::from(length)] += 1;
        }
        let max = (1..16).rev().find(|&length| count[length] != 0).unwrap_or(0);
        let mut symbol = Vec::with_capacity(lengths.len());
        if max != 0 {
            let mut left: i32 = 1;
            for &codes in &count[1..] {
                left <<= 1;
                left -= i32::from(codes);
                if left < 0 {
                    return None;
                }
            }
            if left > 0 && (kind == Kind::CodeLengths || max != 1) {
                return None;
            }
            for length in 1..16 {
                for (s, &l) in lengths.iter().enumerate() {
                    if usize::from(l) == length {
                        symbol.push(s as u16);
                    }
                }
            }
        }
        count[0] = 0;
        Some(Self { count, symbol })
    }

    // A symbol, a bit at a time from the code's first bit; None for a code it lacks.
    fn decode(&self, input: &mut Bits) -> Option<usize> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for length in 1..16 {
            code |= input.get(1)? as i32;
            let count = i32::from(self.count[length]);
            if code - count < first {
                return self.symbol.get((index + code - first) as usize).map(|&s| usize::from(s));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LENGTH_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const WINDOW: usize = 1 << 15;

// The blocks, to the last; more than `size` bytes is a failure, as zlib runs out of room.
fn inflate(input: &mut Bits, out: &mut Vec<u8>, size: usize) -> Option<()> {
    loop {
        let last = input.get(1)?;
        match input.get(2)? {
            0 => stored(input, out, size)?,
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literals = Huffman::new(&lengths, Kind::LiteralLengths)?;
                // All 32 codes, as zlib's table has them: 30 and 31 fail when read.
                let distances = Huffman::new(&[5u8; 32], Kind::Distances)?;
                codes(input, out, size, &literals, &distances)?;
            }
            2 => {
                let (literals, distances) = dynamic(input)?;
                codes(input, out, size, &literals, &distances)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(());
        }
    }
}

fn stored(input: &mut Bits, out: &mut Vec<u8>, size: usize) -> Option<()> {
    input.align();
    let bytes = input.data.get(input.at..input.at + 4)?;
    let (length, inverse) =
        (usize::from(u16::from_le_bytes([bytes[0], bytes[1]])), u16::from_le_bytes([bytes[2], bytes[3]]));
    if length as u16 != !inverse {
        return None;
    }
    input.at += 4;
    let block = input.data.get(input.at..input.at + length)?;
    if out.len() + length > size {
        return None;
    }
    out.extend_from_slice(block);
    input.at += length;
    Some(())
}

// A dynamic block's two codes, as zlib checks them.
fn dynamic(input: &mut Bits) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let literal_count = input.get(5)? as usize + 257;
    let distance_count = input.get(5)? as usize + 1;
    let code_count = input.get(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return None;
    }
    let mut code_lengths = [0u8; 19];
    for &at in &ORDER[..code_count] {
        code_lengths[at] = input.get(3)? as u8;
    }
    let lengths_code = Huffman::new(&code_lengths, Kind::CodeLengths)?;

    let mut lengths = [0u8; 286 + 30];
    let total = literal_count + distance_count;
    let mut at = 0;
    while at < total {
        let symbol = lengths_code.decode(input)?;
        if symbol < 16 {
            lengths[at] = symbol as u8;
            at += 1;
            continue;
        }
        let (value, repeat) = match symbol {
            16 => (*lengths.get(at.checked_sub(1)?)?, 3 + input.get(2)? as usize),
            17 => (0, 3 + input.get(3)? as usize),
            _ => (0, 11 + input.get(7)? as usize),
        };
        if at + repeat > total {
            return None;
        }
        lengths[at..at + repeat].fill(value);
        at += repeat;
    }
    if lengths[256] == 0 {
        return None; // no end of block
    }
    let literals = Huffman::new(&lengths[..literal_count], Kind::LiteralLengths)?;
    let distances = Huffman::new(&lengths[literal_count..total], Kind::Distances)?;
    Some((literals, distances))
}

fn codes(input: &mut Bits, out: &mut Vec<u8>, size: usize, literals: &Huffman, distances: &Huffman) -> Option<()> {
    loop {
        let symbol = literals.decode(input)?;
        match symbol {
            0..=255 => {
                if out.len() == size {
                    return None;
                }
                out.push(symbol as u8);
            }
            256 => return Some(()),
            _ => {
                let at = symbol - 257;
                if at >= 29 {
                    return None; // 286 and 287, which the fixed code has
                }
                let length = usize::from(LENGTH_BASE[at]) + input.get(u32::from(LENGTH_EXTRA[at]))? as usize;
                let code = distances.decode(input)?;
                if code >= 30 {
                    return None;
                }
                let distance = usize::from(DISTANCE_BASE[code]) + input.get(u32::from(DISTANCE_EXTRA[code]))? as usize;
                if distance > out.len() || distance > WINDOW {
                    return None;
                }
                if out.len() + length > size {
                    return None;
                }
                let from = out.len() - distance;
                for i in 0..length {
                    let byte = out[from + i];
                    out.push(byte);
                }
            }
        }
    }
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &byte in data {
        c = CRC_TABLE[((c ^ u32::from(byte)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}
