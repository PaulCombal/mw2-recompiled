// FFmpeg's bit reader (libavcodec/get_bits.h) as this build of it is: big-endian, the
// checked reader, which never moves past eight bits beyond the end of what it reads, and
// reads the bytes after that end as they are. Its reads see up to eight bytes past the
// index, so a buffer has that much more behind the bits it holds; a byte outside the slice
// reads as zero.
//
// Copyright (c) 2004 Michael Niedermayer
// Part of a port of FFmpeg, LGPL 2.1 or later: see NOTICE.md in this folder.

use super::vlc::VlcElem;

pub struct BitReader<'a> {
    buffer: &'a [u8],
    index: u32,
    size_in_bits: i32,
    size_plus8: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(buffer: &'a [u8], size_in_bits: i32) -> Self {
        Self { buffer, index: 0, size_in_bits, size_plus8: (size_in_bits + 8) as u32 }
    }

    #[inline(always)]
    fn byte(&self, at: usize) -> u8 {
        self.buffer.get(at).copied().unwrap_or(0)
    }

    #[inline(always)]
    fn rb32(&self, at: usize) -> u32 {
        match self.buffer.get(at..at + 4) {
            Some(word) => u32::from_be_bytes(word.try_into().expect("four bytes")),
            None => u32::from_be_bytes([self.byte(at), self.byte(at + 1), self.byte(at + 2), self.byte(at + 3)]),
        }
    }

    fn rb64(&self, at: usize) -> u64 {
        (u64::from(self.rb32(at)) << 32) | u64::from(self.rb32(at + 4))
    }

    // UPDATE_CACHE: the 32 bits from the index on, the first in the top bit.
    #[inline(always)]
    fn cache(&self) -> u32 {
        self.rb32((self.index >> 3) as usize) << (self.index & 7)
    }

    // SKIP_COUNTER, in the reader's unsigned arithmetic: a negative count goes back.
    #[inline(always)]
    fn advance(&mut self, bits: i32) {
        self.index = self.size_plus8.min(self.index.wrapping_add(bits as u32));
    }

    #[inline(always)]
    pub fn count(&self) -> i32 {
        self.index as i32
    }

    pub fn left(&self) -> i32 {
        self.size_in_bits - self.index as i32
    }

    /// 1 to 25 bits, without moving.
    #[inline(always)]
    pub fn show(&self, bits: u32) -> u32 {
        self.cache() >> (32 - bits)
    }

    /// 1 to 25 bits.
    #[inline(always)]
    pub fn get(&mut self, bits: u32) -> u32 {
        let value = self.show(bits);
        self.advance(bits as i32);
        value
    }

    /// 0 to 25 bits (get_bitsz).
    pub fn get_z(&mut self, bits: u32) -> u32 {
        if bits == 0 { 0 } else { self.get(bits) }
    }

    /// 1 to 25 bits, sign-extended.
    pub fn get_signed(&mut self, bits: u32) -> i32 {
        let value = (self.cache() as i32) >> (32 - bits);
        self.advance(bits as i32);
        value
    }

    #[inline(always)]
    pub fn get1(&mut self) -> u32 {
        let byte = self.byte((self.index >> 3) as usize);
        let bit = (byte << (self.index & 7)) >> 7;
        if self.index < self.size_plus8 {
            self.index += 1;
        }
        u32::from(bit)
    }

    /// 0 to 32 bits.
    pub fn get_long(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        if bits <= 25 {
            return self.get(bits);
        }
        let cache = ((self.rb64((self.index >> 3) as usize) << (self.index & 7)) >> 32) as u32;
        let value = cache >> (32 - bits);
        self.advance(bits as i32);
        value
    }

    /// skip_bits, of up to 25 bits.
    pub fn skip(&mut self, bits: u32) {
        self.advance(bits as i32);
    }

    /// skip_bits_long: kept between the start and eight bits past the end.
    pub fn skip_long(&mut self, bits: i32) {
        let index = self.index as i32;
        self.index = (index + bits.clamp(-index, self.size_plus8 as i32 - index)) as u32;
    }

    /// get_vlc2: the symbol, or -1 for a code the table does not have.
    #[inline(always)]
    pub fn vlc(&mut self, table: &[VlcElem], bits: u32, max_depth: u32) -> i32 {
        let mut entry = table[self.show(bits) as usize];
        let mut code = i32::from(entry.sym);
        let mut length = i32::from(entry.len);
        if max_depth > 1 && length < 0 {
            self.advance(bits as i32);
            let nb_bits = (-length) as u32;
            entry = table[(self.show(nb_bits) as i32 + code) as usize];
            code = i32::from(entry.sym);
            length = i32::from(entry.len);
            if max_depth > 2 && length < 0 {
                self.advance(nb_bits as i32);
                let nb_bits = (-length) as u32;
                entry = table[(self.show(nb_bits) as i32 + code) as usize];
                code = i32::from(entry.sym);
                length = i32::from(entry.len);
            }
        }
        self.advance(length);
        code
    }
}
