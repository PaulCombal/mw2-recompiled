// The container a zone comes in.
//
// A PC fastfile (the 2009 game's, version 276, and IW4x's) is a 21-byte header, then one
// zlib stream; IW4x scrambles every byte after inflating. A 360 one (zone/src/fastfile.rs
// reads it for the shader pass) lists the pictures it streams from the console's image
// paks, then holds the zlib stream. A converted map is written as an unsigned 360 file.

use crate::deflate;
use crate::error::{Error, Result};
use crate::num::{load, store};
use crate::paths::{file_name, utf8};
use crate::tree::PakEntry;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const PC_VERSION: [u8; 4] = [0x14, 0x01, 0x00, 0x00]; // 276, little-endian
const PC_HEADER: usize = 21; // magic, version, a flag byte, a timestamp
const LARGEST_ZONE: usize = 1 << 30; // (the stock files' largest zone is a few hundred MB)

#[derive(Default)]
pub struct Fastfile {
    pub pc: bool,
    pub iw4x: bool,
    pub zone: Vec<u8>,
    /// A 360 file's pak table: (pak, start, end) of each streamed picture's levels, four a
    /// picture, in the order the zone has them.
    pub table: Vec<PakEntry>,
}

fn starts(b: &[u8], magic: &[u8]) -> bool {
    b.len() >= magic.len() && &b[..magic.len()] == magic
}

// A signed file's stream: past an 8 KB signature header, an 8 KB block of hashes before
// each 2 MB of it.
fn unsigned(body: &[u8]) -> Vec<u8> {
    if !starts(body, b"IWffs100") {
        return body.to_vec();
    }
    let n = body.len();
    let mut stream = Vec::new();
    let mut at = 0x2000;
    while at < n {
        at += 0x2000;
        if at >= n {
            break;
        }
        let take = (n - at).min(0x200000);
        stream.extend_from_slice(&body[at..at + take]);
        at += 0x200000;
    }
    stream
}

fn inflate(stream: &[u8], name: &str) -> Result<Vec<u8>> {
    mw2_install::inflate::unzlib(stream, LARGEST_ZONE).ok_or_else(|| {
        Error::new(format!("{name}: its zone does not decompress, or inflates past {} MB", LARGEST_ZONE >> 20))
    })
}

// IW4x scrambles every byte after inflating (FastFiles::ReadXFileStub).
fn unscramble(data: &mut [u8]) {
    let mut last = 0u8;
    for b in data {
        let mut v = *b ^ last;
        v = v.rotate_left(4);
        v ^= 0xFF;
        v = v.rotate_left(2);
        *b = v;
        last = v;
    }
}

pub fn is_pc_fastfile(path: &Path) -> bool {
    let mut head = [0u8; 12];
    match File::open(path) {
        Ok(mut f) => f.read_exact(&mut head).is_ok() && head[8..12] == PC_VERSION,
        Err(_) => false,
    }
}

/// An Error when the file is neither platform's fastfile.
pub fn read_fastfile(path: &Path) -> Result<Fastfile> {
    let raw = std::fs::read(path).map_err(|_| Error::new(format!("cannot open {}", utf8(path))))?;
    let name = file_name(path);
    let mut ff = Fastfile::default();
    if raw.len() >= 12 && raw[8..12] == PC_VERSION {
        ff.pc = true;
        ff.iw4x = starts(&raw, b"IW4x\x03\0\0\0");
        if !ff.iw4x && !starts(&raw, b"IWffu100") && !starts(&raw, b"IWff0100") {
            return Err(Error::new(format!("{name} is not a PC fastfile of the game")));
        }
        if raw.len() < PC_HEADER {
            return Err(Error::new(format!("{name} is cut short")));
        }
        ff.zone = inflate(&unsigned(&raw[PC_HEADER..]), &name)?;
        if ff.iw4x {
            // And reads one pad byte before the zone's header (FastFiles::ReadXFileHeader).
            unscramble(&mut ff.zone);
            if !ff.zone.is_empty() {
                ff.zone.remove(0);
            }
        }
        return Ok(ff);
    }
    if !starts(&raw, b"IWffu100") && !starts(&raw, b"IWff0100") {
        return Err(Error::new(format!("{name} is not a fastfile of the game")));
    }
    if raw.len() < 0x1D {
        return Err(Error::new(format!("{name} is cut short")));
    }
    let count = load(&raw[0x19..], 4, true) as usize;
    let body = 0x1D + count * 12 + 8;
    if body > raw.len() {
        return Err(Error::new(format!("{name} is cut short")));
    }
    for k in 0..count {
        let e = &raw[0x1D + 12 * k..];
        ff.table.push(PakEntry {
            pak: load(e, 4, true) as u32,
            start: load(&e[4..], 4, true) as u32,
            end: load(&e[8..], 4, true) as u32,
        });
    }
    ff.zone = inflate(&unsigned(&raw[body..]), &name)?;
    Ok(ff)
}

/// An unsigned 360 file of `zone` (deflated at zlib's level 6) with the pak table given.
pub fn xbox_fastfile(zone: &[u8], table: &[PakEntry]) -> Vec<u8> {
    // A stock file's header: magic, version 0x10D, flags, build time; then the pak table
    // and the file's size twice, the second counting the pictures read from
    // imagefile1.pak while it loads.
    const HEAD: [u8; 25] = [
        0x49, 0x57, 0x66, 0x66, 0x75, 0x31, 0x30, 0x30, 0x00, 0x00, 0x01, 0x0d, 0x01, 0x01, 0xca, 0x3e, 0xc0, 0x38, 0xc2,
        0xe4, 0xa0, 0x00, 0x00, 0x00, 0x01,
    ];
    let stream = deflate::zlib(zone, 6);
    let mut out = HEAD.to_vec();
    let put = |out: &mut Vec<u8>, v: u32| {
        let mut b = [0u8; 4];
        store(&mut b, 4, u64::from(v), true);
        out.extend_from_slice(&b);
    };
    put(&mut out, table.len() as u32);
    let mut extra = 0u32;
    for e in table {
        put(&mut out, e.pak);
        put(&mut out, e.start);
        put(&mut out, e.end);
        if e.pak == 1 {
            extra = extra.wrapping_add(e.end.wrapping_sub(e.start));
        }
    }
    let total = (out.len() + 8 + stream.len()) as u32;
    put(&mut out, total);
    put(&mut out, total.wrapping_add(extra));
    out.extend_from_slice(&stream);
    out
}
