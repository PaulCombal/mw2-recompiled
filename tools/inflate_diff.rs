// tools/inflate_diff.py's program: src/runtime/inflate.rs's gunzip against zlib's inflate as
// the game called it before (inflateInit2 with 16 + MAX_WBITS, one Z_FINISH call, the whole
// stream and exactly `size` bytes out), on a corpus of streams and on damaged copies of
// them. Both must refuse the same streams and give the same bytes for the rest.
//
//   usage: inflate_diff <corpus> <seeds>
//
// The corpus is entries of a little-endian u32 size, a u32 length and that many bytes.

#![allow(dead_code)]

#[path = "../src/runtime/inflate.rs"]
mod inflate;

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::mem::{size_of, zeroed};

const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_FINISH: c_int = 4;
const MAX_WBITS: c_int = 15;

#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: *mut c_void,
    zfree: *mut c_void,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

unsafe extern "C" {
    fn inflateInit2_(stream: *mut ZStream, bits: c_int, version: *const c_char, size: c_int) -> c_int;
    fn inflate(stream: *mut ZStream, flush: c_int) -> c_int;
    fn inflateEnd(stream: *mut ZStream) -> c_int;
}

// The game's call of zlib, as src/runtime/zlib.rs made it.
fn zlib_gunzip(packed: &[u8], size: usize) -> Option<Vec<u8>> {
    let mut plain = vec![0u8; size];
    let mut stream: ZStream = unsafe { zeroed() };
    if unsafe { inflateInit2_(&mut stream, 16 + MAX_WBITS, c"1.3.1".as_ptr(), size_of::<ZStream>() as c_int) } != Z_OK {
        return None;
    }
    stream.next_in = packed.as_ptr();
    stream.avail_in = packed.len() as c_uint;
    stream.next_out = plain.as_mut_ptr();
    stream.avail_out = size as c_uint;
    let result = unsafe { inflate(&mut stream, Z_FINISH) };
    let whole = stream.total_out as usize == size;
    unsafe { inflateEnd(&mut stream) };
    (result == Z_STREAM_END && whole).then_some(plain)
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// A damaged copy: bits flipped where it matters most, cut short, run on, or asked for a
// different size.
fn damage(random: &mut Random, packed: &[u8], size: usize) -> (Vec<u8>, usize) {
    let mut bytes = packed.to_vec();
    let mut size = size;
    for _ in 0..1 + random.below(3) {
        match random.below(8) {
            0..=2 if !bytes.is_empty() => {
                // anywhere, but mostly in the first block's header and tables
                let at =
                    if random.below(2) == 0 { random.below(bytes.len().min(64)) } else { random.below(bytes.len()) };
                bytes[at] ^= 1 << random.below(8);
            }
            3 if !bytes.is_empty() => {
                let at = random.below(bytes.len());
                bytes[at] = random.next() as u8;
            }
            4 => bytes.truncate(random.below(bytes.len() + 1)),
            5 => bytes.extend((0..random.below(16)).map(|_| random.next() as u8)),
            6 => size = size.wrapping_add(random.below(3)).wrapping_sub(1).min(1 << 24),
            _ if bytes.len() > 18 => {
                // the trailer, or the header's flags
                let at = if random.below(2) == 0 { bytes.len() - 1 - random.below(8) } else { 3 };
                bytes[at] ^= 1 << random.below(8);
            }
            _ => {}
        }
    }
    (bytes, size)
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let corpus = std::fs::read(&arguments[1]).expect("the corpus");
    let seeds: u64 = arguments[2].parse().expect("a number of seeds");
    let mut entries = Vec::new();
    let mut at = 0;
    while at + 8 <= corpus.len() {
        let size = u32::from_le_bytes(corpus[at..at + 4].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(corpus[at + 4..at + 8].try_into().unwrap()) as usize;
        entries.push((size, corpus[at + 8..at + 8 + length].to_vec()));
        at += 8 + length;
    }
    let (mut same, mut accepted) = (0u64, 0u64);
    let mut check = |packed: &[u8], size: usize, what: &str| {
        let (ours, theirs) = (inflate::gunzip(packed, size), zlib_gunzip(packed, size));
        if ours != theirs {
            println!(
                "DIFFERENT: {what}: {} bytes asked for {size}: the port {}, zlib {}",
                packed.len(),
                ours.as_ref().map_or("refuses".to_string(), |o| format!("gives {} bytes", o.len())),
                theirs.as_ref().map_or("refuses".to_string(), |o| format!("gives {} bytes", o.len()))
            );
            let path = format!("{}.different", arguments[1]);
            let mut file = (size as u32).to_le_bytes().to_vec();
            file.extend((packed.len() as u32).to_le_bytes());
            file.extend(packed);
            std::fs::write(&path, file).expect("the failing stream");
            println!("written to {path}");
            std::process::exit(1);
        }
        same += 1;
        accepted += u64::from(ours.is_some());
    };
    for (index, (size, packed)) in entries.iter().enumerate() {
        check(packed, *size, &format!("entry {index}"));
        for seed in 0..seeds {
            let mut random = Random(seed.wrapping_mul(1_000_003).wrapping_add(index as u64));
            let (bytes, asked) = damage(&mut random, packed, *size);
            check(&bytes, asked, &format!("entry {index} seed {seed}"));
        }
    }
    println!("all the same: {} streams, {} entries, {same} cases, {accepted} taken", entries.len(), entries.len());
}
