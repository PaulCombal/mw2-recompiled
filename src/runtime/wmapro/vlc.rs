// FFmpeg's VLC tables (libavcodec/vlc.c): built from code lengths the way
// ff_vlc_init_from_lengths builds them, so that an invalid code reads as the same symbol,
// -1, over the same number of bits.
//
// Copyright (c) 2000, 2001 Fabrice Bellard
// Copyright (c) 2002-2004 Michael Niedermayer
// Copyright (c) 2010 Loren Merritt
// Part of a port of FFmpeg, LGPL 2.1 or later: see NOTICE.md in this folder.

#[derive(Clone, Copy, Default)]
pub struct VlcElem {
    pub sym: i16,
    pub len: i16,
}

#[derive(Clone, Copy)]
struct Code {
    bits: i32,
    symbol: i16,
    code: u32,
}

/// A table of `nb_bits` bits a lookup, with its subtables after it. `lengths` and `symbols`
/// go together; a length of 0 is no code, a negative one a gap in the code space.
pub fn from_lengths(nb_bits: i32, lengths: &[i8], symbols: &[u16], offset: i32) -> Vec<VlcElem> {
    let mut codes = Vec::with_capacity(lengths.len());
    let mut code: u64 = 0;
    let len_max = 32.min(3 * nb_bits);
    for (i, &length) in lengths.iter().enumerate() {
        let length = i32::from(length);
        let len = match length {
            0 => continue,
            l if l > 0 => {
                let symbol = i32::from(symbols[i]) + offset;
                codes.push(Code { bits: l, symbol: symbol as i16, code: code as u32 });
                l
            }
            l => -l,
        };
        assert!(len <= len_max && code & ((1u64 << (32 - len)) - 1) == 0, "an invalid VLC");
        code += 1u64 << (32 - len);
        assert!(code <= u64::from(u32::MAX) + 1, "an overdetermined VLC");
    }
    let mut table = Vec::new();
    build(&mut table, nb_bits, &mut codes);
    table
}

// build_table: returns where this table starts.
fn build(table: &mut Vec<VlcElem>, table_nb_bits: i32, codes: &mut [Code]) -> usize {
    let table_size = 1usize << table_nb_bits;
    let table_index = table.len();
    table.resize(table_index + table_size, VlcElem::default());
    let mut i = 0;
    while i < codes.len() {
        let n = codes[i].bits;
        let code = codes[i].code;
        let symbol = codes[i].symbol;
        if n <= table_nb_bits {
            let j = (code >> (32 - table_nb_bits)) as usize;
            let nb = 1usize << (table_nb_bits - n);
            for k in 0..nb {
                let entry = &mut table[table_index + j + k];
                assert!(
                    (entry.len == 0 && entry.sym == 0) || (i32::from(entry.len) == n && entry.sym == symbol),
                    "incorrect VLC codes"
                );
                *entry = VlcElem { sym: symbol, len: n as i16 };
            }
            i += 1;
        } else {
            let n = n - table_nb_bits;
            let code_prefix = code >> (32 - table_nb_bits);
            let mut subtable_bits = n;
            codes[i].bits = n;
            codes[i].code = code << table_nb_bits;
            let mut k = i + 1;
            while k < codes.len() {
                let n = codes[k].bits - table_nb_bits;
                if n <= 0 {
                    break;
                }
                let code = codes[k].code;
                if code >> (32 - table_nb_bits) != code_prefix {
                    break;
                }
                codes[k].bits = n;
                codes[k].code = code << table_nb_bits;
                subtable_bits = subtable_bits.max(n);
                k += 1;
            }
            let subtable_bits = subtable_bits.min(table_nb_bits);
            let j = table_index + code_prefix as usize;
            table[j].len = -subtable_bits as i16;
            let index = build(table, subtable_bits, &mut codes[i..k]);
            // The subtable's place, from the start of the whole table.
            table[j].sym = index as i16;
            i = k;
        }
    }
    for entry in &mut table[table_index..table_index + table_size] {
        if entry.len == 0 {
            entry.sym = -1;
        }
    }
    table_index
}
