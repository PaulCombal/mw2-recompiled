// An XEX2 as the disc carries it, or as a title update leaves it, turned into the
// memory image the recompiled code was made from: decrypted with the retail key and
// its basic compression undone, where it has either -- what cargo xtask writes as
// default.pe, made at every launch instead of kept on disk.

use super::crypto::{self, Key};
use std::collections::BTreeMap;

// Every retail XEX's session key is encrypted with this one.
const RETAIL_KEY: Key =
    [0x20, 0xB1, 0x85, 0xA5, 0x9D, 0x28, 0xFD, 0xC3, 0x40, 0x58, 0x3F, 0xBB, 0x08, 0x96, 0xBF, 0x91];
const FILE_FORMAT_INFO: u32 = 0x000003FF;

// Zero past the end of the file: a damaged header says so by what it adds up to.
fn be32(file: &[u8], at: usize) -> u32 {
    file.get(at..at + 4).map_or(0, |bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn be16(file: &[u8], at: usize) -> u16 {
    file.get(at..at + 2).map_or(0, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
}

// The optional headers, by key: a value, or the offset of the data.
fn headers(file: &[u8]) -> Option<BTreeMap<u32, u32>> {
    if file.len() < 0x18 || !file.starts_with(b"XEX2") {
        return None;
    }
    let count = be32(file, 0x14) as usize;
    if 0x18 + count * 8 > file.len() {
        return None;
    }
    Some((0..count).map(|i| (be32(file, 0x18 + i * 8), be32(file, 0x18 + i * 8 + 4))).collect())
}

/// An error says why, for anything but encryption none/normal and compression
/// none/basic -- all the disc and a patched executable use.
pub fn image(file: &[u8]) -> Result<Vec<u8>, String> {
    let headers = headers(file).ok_or("not an XEX2 file")?;
    let data_offset = be32(file, 0x08) as usize;
    let security = be32(file, 0x10) as usize;
    let format = headers.get(&FILE_FORMAT_INFO);
    let (Some(&format), true, true) = (format, data_offset <= file.len(), security + 352 <= file.len()) else {
        return Err("the XEX header is damaged".into());
    };
    let at = format as usize;
    let format_size = be32(file, at);
    let encryption = be16(file, at + 4);
    let compression = be16(file, at + 6);
    if compression > 1 {
        return Err(format!("the XEX uses a compression neither the disc nor an update does ({compression})"));
    }
    if encryption > 1 {
        return Err("the XEX uses an unknown encryption".into());
    }

    // The payload, padded to whole AES blocks, as xtask/src/xexdump.rs does.
    let mut data = file[data_offset..].to_vec();
    data.resize((data.len() + 15) & !15, 0);
    if encryption == 1 {
        let mut session = [0u8; 16];
        session.copy_from_slice(&file[security + 336..security + 352]);
        crypto::aes128_cbc_decrypt(&RETAIL_KEY, &mut session);
        crypto::aes128_cbc_decrypt(&session, &mut data);
    }

    let mut image = Vec::new();
    // An executable a title update was applied to is stored as it is.
    if compression == 0 {
        image.extend_from_slice(&file[data_offset..]);
    }
    // Basic compression: runs of data, each followed by a run of zeros.
    let mut read = 0;
    let mut block = 8u32;
    while compression == 1 && block.wrapping_add(8) <= format_size && block.wrapping_add(8) >= block {
        let size = be32(file, at + block as usize) as usize;
        let zeros = be32(file, at + block as usize + 4) as usize;
        if read + size > data.len() {
            return Err("the XEX is shorter than its blocks say".into());
        }
        image.extend_from_slice(&data[read..read + size]);
        image.resize(image.len() + zeros, 0);
        read += size;
        block += 8;
    }
    if !image.starts_with(b"MZ") {
        return Err("the XEX did not decrypt to an executable".into());
    }
    Ok(image)
}
