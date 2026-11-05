// An XEX2 decrypted and its basic compression undone into the flat memory image the
// analysis and the recompiler read: what tools/xexdump.py did, with the AES the game
// itself uses at launch (src/runtime/install/xex.rs does the same at every start).

use crate::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[allow(dead_code)]
#[path = "../../src/runtime/install/crypto.rs"]
mod crypto;

// Every retail XEX's session key is encrypted with this one.
const RETAIL_KEY: crypto::Key =
    [0x20, 0xB1, 0x85, 0xA5, 0x9D, 0x28, 0xFD, 0xC3, 0x40, 0x58, 0x3F, 0xBB, 0x08, 0x96, 0xBF, 0x91];
const FILE_FORMAT_INFO: u32 = 0x000003FF;

fn be32(data: &[u8], at: usize) -> Result<u32> {
    let bytes = data.get(at..at + 4).ok_or("the XEX is cut short")?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn be16(data: &[u8], at: usize) -> Result<u16> {
    let bytes = data.get(at..at + 2).ok_or("the XEX is cut short")?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn image(file: &[u8]) -> Result<Vec<u8>> {
    if !file.starts_with(b"XEX2") {
        return Err("not an XEX2 file".into());
    }
    let header_size = be32(file, 8)? as usize;
    let security = be32(file, 0x10)? as usize;
    let mut headers = BTreeMap::new();
    for i in 0..be32(file, 0x14)? as usize {
        headers.insert(be32(file, 0x18 + i * 8)?, be32(file, 0x18 + i * 8 + 4)?);
    }
    let format = *headers.get(&FILE_FORMAT_INFO).ok_or("the XEX has no file format header")? as usize;
    let format_size = be32(file, format)?;
    let encryption = be16(file, format + 4)?;
    let compression = be16(file, format + 6)?;
    let mut data = file.get(header_size..).ok_or("the XEX is cut short")?.to_vec();
    if encryption == 1 {
        let mut session: crypto::Key =
            file.get(security + 336..security + 352).ok_or("the XEX is cut short")?.try_into().unwrap();
        crypto::aes128_cbc_decrypt(&RETAIL_KEY, &mut session);
        // Padded to whole AES blocks, and kept so.
        data.resize(data.len().next_multiple_of(16), 0);
        crypto::aes128_cbc_decrypt(&session, &mut data);
    }
    // Stored as it is: what applying a title update writes.
    if compression == 0 {
        return Ok(data);
    }
    if compression != 1 {
        return Err(format!("unsupported compression {compression}"));
    }
    // Basic compression: runs of data, each followed by a run of zeros.
    let mut image = Vec::new();
    let mut read = 0;
    for i in 0..(i64::from(format_size) - 8).div_euclid(8).max(0) as usize {
        let size = be32(file, format + 8 + i * 8)? as usize;
        let zeros = be32(file, format + 8 + i * 8 + 4)? as usize;
        image.extend_from_slice(&data[read.min(data.len())..(read + size).min(data.len())]);
        read += size;
        image.resize(image.len() + zeros, 0);
    }
    Ok(image)
}

/// The image, checked to be a PE before it is written: one left behind would be
/// taken for done by every later build.
pub fn write(xex: &Path, out: &Path) -> Result {
    let file = fs::read(xex).map_err(|e| format!("{}: {e}", xex.display()))?;
    let image = image(&file).map_err(|e| format!("{}: {e}", xex.display()))?;
    let pe = image.get(0x3C..0x40).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize);
    if !image.starts_with(b"MZ") || pe.and_then(|pe| image.get(pe..pe + 4)) != Some(b"PE\0\0") {
        return Err(format!("{}: not a PE after decrypting", xex.display()));
    }
    fs::write(out, &image).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("    image {} bytes -> {}", image.len(), out.display());
    Ok(())
}
