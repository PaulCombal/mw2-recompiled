// A title update's executable patch (.xexp) applied to the disc's executable: the
// patched XEX the update's title runs, as the console's loader makes it. The patch is
// an XEX of its own whose payload is LZX "delta" records -- zero a run, move a run, or
// decompress a run with the run it replaces as the window's reference data -- first
// over the executable's header, then, block by SHA-1 checked block, over its decrypted
// and decompressed image. The result is stored with neither encryption nor compression,
// which is how an update leaves it on the console.
//
// A patch is for one executable: its image key, decrypted, must be the executable's
// own, so a patch from another region's update, whose disc is signed with another key,
// is refused rather than applied to the wrong bytes.
//
// This is XenonRecomp's patcher (XenonUtils/xex_patcher.cpp), which follows Xenia's
// (src/xenia/cpu/xex_module.cc), step by step, so that it makes the same bytes. Where
// the C++ reads or writes past a buffer on a damaged file, this returns the error of the
// file at fault instead.
//
// xex_patcher.cpp: Copyright (c) 2025 hedge-dev and contributors, MIT License; after
// Xenia's, Copyright 2023 Ben Vanik, BSD License. The licences' texts are in
// NOTICE.md, beside src/.

use crate::crypto::{self, Key};
use crate::lzx::Lzxd;
use crate::xex::RETAIL_KEY;
use std::fmt;

// The XEX2 header: { magic, module flags, header size, reserved, security info offset,
// optional header count }, then the optional headers, { key, value or offset } each.
const MODULE_FLAGS: usize = 0x04;
const HEADER_SIZE: usize = 0x08;
const SECURITY_OFFSET: usize = 0x10;
const HEADER_COUNT: usize = 0x14;
const OPTIONAL_HEADERS: usize = 0x18;
// Module flags: a patch, of either kind.
const MODULE_PATCH: u32 = 0x10;
const PATCH_FULL: u32 = 0x20;
const PATCH_DELTA: u32 = 0x40;
const FILE_FORMAT_INFO: u32 = 0x0000_03FF;
const DELTA_PATCH_DESCRIPTOR: u32 = 0x0000_05FF;
// In the security info: the image's size and its encrypted session key.
const SECURITY_IMAGE_SIZE: usize = 4;
const SECURITY_AES_KEY: usize = 0x150;
// The file format info: { info size, encryption, compression }, then for normal and
// delta compression { window size, first block { size, SHA-1 } }, for basic compression
// { data size, zero size } blocks.
const FORMAT_ENCRYPTION: usize = 4;
const FORMAT_COMPRESSION: usize = 6;
const FORMAT_WINDOW_SIZE: usize = 8;
const FORMAT_FIRST_BLOCK: usize = 12;
const ENCRYPTION_NONE: u16 = 0;
const ENCRYPTION_NORMAL: u16 = 1;
const COMPRESSION_NONE: u16 = 0;
const COMPRESSION_BASIC: u16 = 1;
const COMPRESSION_NORMAL: u16 = 2;
const COMPRESSION_DELTA: u16 = 3;
// The delta patch descriptor's fields, and its first record.
const DESCRIPTOR_SIZE: usize = 0;
const DESCRIPTOR_IMAGE_KEY_SOURCE: usize = 32;
const DESCRIPTOR_TARGET_HEADERS_SIZE: usize = 48;
const DESCRIPTOR_HEADERS_SOURCE_OFFSET: usize = 52;
const DESCRIPTOR_HEADERS_SOURCE_SIZE: usize = 56;
const DESCRIPTOR_HEADERS_TARGET_OFFSET: usize = 60;
const DESCRIPTOR_IMAGE_SOURCE_OFFSET: usize = 64;
const DESCRIPTOR_IMAGE_SOURCE_SIZE: usize = 68;
const DESCRIPTOR_IMAGE_TARGET_OFFSET: usize = 72;
const DESCRIPTOR_RECORDS: usize = 76;
// A record: { old address, new address, uncompressed length (16 bits), compressed
// length (16 bits) }, and its LZX data.
const RECORD_HEADER: usize = 12;
// A block of compressed data starts with the next block's { size, SHA-1 }.
const BLOCK_HEADER: usize = 24;
// No executable is larger than the console's memory; a damaged header that says
// otherwise is not allocated for.
const MAX_IMAGE: usize = 512 << 20;

/// Why a patch was not applied: XenonRecomp's XexPatcher::Result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The executable is damaged or not an XEX2.
    XexInvalid,
    /// The executable is itself stored as a delta.
    XexUnsupported,
    /// The patch is damaged or not a patch.
    PatchInvalid,
    /// The patch is for another executable.
    PatchIncompatible,
    /// The patch's data did not check out or did not apply.
    PatchFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Error::XexInvalid => "the executable is damaged",
            Error::XexUnsupported => "the executable is stored in a way the patcher does not read",
            Error::PatchInvalid => "the patch is damaged",
            Error::PatchIncompatible => "the patch is for another executable",
            Error::PatchFailed => "the patch did not apply",
        })
    }
}

impl std::error::Error for Error {}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([word[0], word[1], word[2], word[3]]))
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    let half = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([half[0], half[1]]))
}

fn key(bytes: &[u8], at: usize) -> Option<Key> {
    bytes.get(at..at.checked_add(16)?)?.try_into().ok()
}

fn sha1_at(bytes: &[u8], at: usize) -> Option<[u8; 20]> {
    bytes.get(at..at.checked_add(20)?)?.try_into().ok()
}

// getOptHeaderPtr(): where an optional header's data is -- the value itself for keys
// whose low byte is 0 or 1, else the offset it holds. The first with the key wins.
fn optional_header(bytes: &[u8], wanted: u32) -> Option<usize> {
    let count = be32(bytes, HEADER_COUNT)? as usize;
    for i in 0..count {
        let at = OPTIONAL_HEADERS.checked_add(i.checked_mul(8)?)?;
        if be32(bytes, at)? == wanted {
            return Some(if wanted & 0xFF <= 1 { at + 4 } else { be32(bytes, at + 4)? as usize });
        }
    }
    None
}

// memcpy() of a run within one buffer; None if either end is outside it.
fn move_run(bytes: &mut [u8], from: usize, size: usize, to: usize) -> Option<()> {
    if from.max(to).checked_add(size)? > bytes.len() {
        return None;
    }
    bytes.copy_within(from..from + size, to);
    Some(())
}

// lzxDecompress(): one LZX stream into all of `output`, with libmspack's decoder set
// up as the patcher sets it: the window's size is the lowest bit set in window_size,
// and a reference, when there is one, is put at the end of the window with the rest of
// it zeroed, and the whole window open to matches.
fn lzx_decompress(input: &[u8], output: &mut [u8], window_size: u32, reference: Option<&[u8]>) -> Option<()> {
    if window_size == 0 {
        return None;
    }
    let window_bits = window_size.trailing_zeros() as i32;
    let length = output.len() as i64;
    let mut lzx = Lzxd::new(input, output, window_bits, 0, 0x8000, length, false)?;
    if let Some(reference) = reference {
        let window_size = window_size as usize;
        let padding = window_size.checked_sub(reference.len())?;
        lzx.window.get_mut(..padding)?.fill(0);
        lzx.window.get_mut(padding..window_size)?.copy_from_slice(reference);
        lzx.ref_data_size = window_size as u32;
    }
    lzx.decompress(length).ok()
}

// lzxDeltaApplyPatch(): the records from `start` up to `end` of `patch`, or up to the
// record of zeros that ends them, applied to `out`.
fn apply_records(patch: &[u8], start: usize, end: usize, window_size: u32, out: &mut [u8]) -> Option<()> {
    let mut at = start;
    while end > at {
        let old_address = be32(patch, at)? as usize;
        let new_address = be32(patch, at + 4)? as usize;
        let uncompressed_length = be16(patch, at + 8)? as usize;
        let compressed_length = be16(patch, at + 10)? as usize;
        if compressed_length == 0 && uncompressed_length == 0 && new_address == 0 && old_address == 0 {
            // The end of the patch.
            break;
        }
        let mut record = RECORD_HEADER;
        match compressed_length {
            // Set the data to zeroes.
            0 => out.get_mut(new_address..new_address.checked_add(uncompressed_length)?)?.fill(0),
            // Move the data.
            1 => move_run(out, old_address, uncompressed_length, new_address)?,
            // Decompress the data into the destination, with the data it replaces as
            // the reference.
            _ => {
                record += compressed_length;
                let input = patch.get(at + RECORD_HEADER..at + record)?;
                let reference = out.get(old_address..old_address.checked_add(uncompressed_length)?)?.to_vec();
                let target = out.get_mut(new_address..new_address.checked_add(uncompressed_length)?)?;
                lzx_decompress(input, target, window_size, Some(&reference))?;
            }
        }
        at += record;
    }
    Some(())
}

/// The executable `xex` with the title update's patch `xexp` applied, as an XEX
/// with neither encryption nor compression.
pub fn apply(xex: &[u8], xexp: &[u8]) -> Result<Vec<u8>, Error> {
    use Error::*;

    // Validate the headers.
    if !xex.starts_with(b"XEX2") {
        return Err(XexInvalid);
    }
    if !xexp.starts_with(b"XEX2") {
        return Err(PatchInvalid);
    }
    let patch_flags = be32(xexp, MODULE_FLAGS).ok_or(PatchInvalid)?;
    if patch_flags & (MODULE_PATCH | PATCH_DELTA | PATCH_FULL) == 0 {
        return Err(PatchInvalid);
    }

    // Validate the patch.
    let descriptor = optional_header(xexp, DELTA_PATCH_DESCRIPTOR).ok_or(PatchInvalid)?;
    let patch_format = optional_header(xexp, FILE_FORMAT_INFO).ok_or(PatchInvalid)?;
    if be16(xexp, patch_format + FORMAT_COMPRESSION).ok_or(PatchInvalid)? != COMPRESSION_DELTA {
        return Err(PatchInvalid);
    }
    let field = |offset| be32(xexp, descriptor + offset).ok_or(PatchInvalid);
    let xex_header_size = be32(xex, HEADER_SIZE).ok_or(XexInvalid)?;
    if xex_header_size as usize > xex.len() {
        return Err(XexInvalid);
    }
    let headers_source_offset = field(DESCRIPTOR_HEADERS_SOURCE_OFFSET)?;
    let headers_source_size = field(DESCRIPTOR_HEADERS_SOURCE_SIZE)?;
    let headers_target_offset = field(DESCRIPTOR_HEADERS_TARGET_OFFSET)?;
    let target_headers_size = field(DESCRIPTOR_TARGET_HEADERS_SIZE)?;
    if headers_source_offset > xex_header_size
        || headers_source_size > xex_header_size - headers_source_offset
        || headers_target_offset > target_headers_size
        || headers_source_size > target_headers_size - headers_target_offset
    {
        return Err(PatchIncompatible);
    }

    // Apply the patch to the header: the new header starts as a copy of the old.
    let mut header_target_size = target_headers_size as usize;
    if header_target_size == 0 {
        header_target_size = headers_target_offset as usize + headers_source_size as usize;
    }
    let old_header = xex.get(..header_target_size).ok_or(XexInvalid)?;
    let new_header_size = header_target_size.max(xex_header_size as usize);
    let mut out = vec![0u8; new_header_size];
    out[..header_target_size].copy_from_slice(old_header);
    if headers_source_offset > 0 {
        let (from, size, to) =
            (headers_source_offset as usize, headers_source_size as usize, headers_target_offset as usize);
        move_run(&mut out, from, size, to).ok_or(PatchInvalid)?;
    }
    let window_size = be32(xexp, patch_format + FORMAT_WINDOW_SIZE).ok_or(PatchInvalid)?;
    // The records run for the descriptor's size from the first, which takes them
    // past the descriptor's end; the record of zeros ends them first.
    let records = descriptor + DESCRIPTOR_RECORDS;
    let records_end = records.saturating_add(field(DESCRIPTOR_SIZE)? as usize);
    apply_records(xexp, records, records_end, window_size, &mut out).ok_or(PatchFailed)?;

    // The header is the size the patch says; the image follows it.
    out.truncate(header_target_size);
    let new_security = be32(&out, SECURITY_OFFSET).ok_or(PatchFailed)? as usize;
    let image_size = be32(&out, new_security + SECURITY_IMAGE_SIZE).ok_or(PatchFailed)? as usize;
    if image_size > MAX_IMAGE {
        return Err(PatchFailed);
    }
    out.resize(header_target_size + image_size, 0);
    let xex_data = xex.get(xex_header_size as usize..).ok_or(XexInvalid)?;
    out.get_mut(header_target_size..header_target_size + xex_data.len())
        .ok_or(PatchIncompatible)?
        .copy_from_slice(xex_data);

    // Decrypt the keys, and check that the patch is for this executable: the image
    // key it was made from is the executable's own.
    let xex_security = be32(xex, SECURITY_OFFSET).ok_or(XexInvalid)? as usize;
    let patch_security = be32(xexp, SECURITY_OFFSET).ok_or(PatchInvalid)? as usize;
    let mut original_key = key(xex, xex_security + SECURITY_AES_KEY).ok_or(XexInvalid)?;
    let mut new_key = key(&out, new_security + SECURITY_AES_KEY).ok_or(PatchFailed)?;
    let mut patch_key = key(xexp, patch_security + SECURITY_AES_KEY).ok_or(PatchInvalid)?;
    let mut image_key_source = key(xexp, descriptor + DESCRIPTOR_IMAGE_KEY_SOURCE).ok_or(PatchInvalid)?;
    crypto::aes128_cbc_decrypt(&RETAIL_KEY, &mut original_key);
    crypto::aes128_cbc_decrypt(&RETAIL_KEY, &mut new_key);
    crypto::aes128_cbc_decrypt(&new_key, &mut patch_key);
    crypto::aes128_cbc_decrypt(&new_key, &mut image_key_source);
    if image_key_source != original_key {
        return Err(PatchIncompatible);
    }

    // Decrypt the executable's image if it is encrypted. The C decrypts whole blocks
    // up to the end of the data, reading into the zeros after it.
    let format = optional_header(xex, FILE_FORMAT_INFO).ok_or(XexInvalid)?;
    let encryption = be16(xex, format + FORMAT_ENCRYPTION).ok_or(XexInvalid)?;
    let compression = be16(xex, format + FORMAT_COMPRESSION).ok_or(XexInvalid)?;
    match encryption {
        ENCRYPTION_NORMAL => {
            let end = header_target_size + xex_data.len().next_multiple_of(16);
            crypto::aes128_cbc_decrypt(&original_key, out.get_mut(header_target_size..end).ok_or(XexInvalid)?);
        }
        ENCRYPTION_NONE => {}
        _ => return Err(XexInvalid),
    }

    // Decompress it if it is compressed.
    match compression {
        COMPRESSION_BASIC => {
            // Runs of data, each followed by a run of zeros, which is undone in place
            // from the last run back.
            let info_size = be32(xex, format).ok_or(XexInvalid)? as usize;
            let count = (info_size / 8).saturating_sub(1);
            let mut blocks = Vec::with_capacity(count.min(xex.len() / 8));
            let (mut compressed_size, mut image_size) = (0usize, 0usize);
            for i in 0..count {
                let at = format + 8 + i * 8;
                let data_size = be32(xex, at).ok_or(XexInvalid)? as usize;
                let zero_size = be32(xex, at + 4).ok_or(XexInvalid)? as usize;
                compressed_size = compressed_size.checked_add(data_size).ok_or(XexInvalid)?;
                image_size = image_size.checked_add(data_size + zero_size).ok_or(XexInvalid)?;
                blocks.push((data_size, zero_size));
            }
            if out.len() < header_target_size + image_size {
                return Err(XexInvalid);
            }
            let mut source = header_target_size + compressed_size;
            let mut target = header_target_size + image_size;
            for &(data_size, zero_size) in blocks.iter().rev() {
                target -= zero_size;
                out[target..target + zero_size].fill(0);
                target -= data_size;
                source -= data_size;
                out.copy_within(source..source + data_size, target);
            }
        }
        COMPRESSION_NORMAL => {
            // Blocks, each checked by the SHA-1 the one before it holds and holding
            // chunks of one LZX stream.
            let window = be32(xex, format + FORMAT_WINDOW_SIZE).ok_or(XexInvalid)?;
            let mut block_size = be32(xex, format + FORMAT_FIRST_BLOCK).ok_or(XexInvalid)? as usize;
            let mut block_hash = sha1_at(xex, format + FORMAT_FIRST_BLOCK + 4).ok_or(XexInvalid)?;
            let exe = &out[header_target_size..];
            let mut compressed = Vec::with_capacity(xex_data.len());
            let mut p = 0usize;
            while block_size != 0 {
                let block = exe.get(p..p.checked_add(block_size).ok_or(XexInvalid)?).ok_or(XexInvalid)?;
                let next_size = be32(exe, p).ok_or(XexInvalid)? as usize;
                let next_hash = sha1_at(exe, p + 4).ok_or(XexInvalid)?;
                if crypto::sha1(block) != block_hash {
                    return Err(PatchFailed);
                }
                let next = p + block_size;
                p += BLOCK_HEADER;
                loop {
                    let chunk = be16(exe, p).ok_or(XexInvalid)? as usize;
                    p += 2;
                    if chunk == 0 {
                        break;
                    }
                    if compressed.len() + chunk > xex_data.len() {
                        return Err(XexInvalid);
                    }
                    compressed.extend_from_slice(exe.get(p..p + chunk).ok_or(XexInvalid)?);
                    p += chunk;
                }
                p = next;
                block_size = next_size;
                block_hash = next_hash;
            }
            // The C decompresses to after the larger of the two headers.
            let uncompressed_size = be32(xex, xex_security + SECURITY_IMAGE_SIZE).ok_or(XexInvalid)? as usize;
            let target = out.get_mut(new_header_size..new_header_size + uncompressed_size).ok_or(PatchFailed)?;
            lzx_decompress(&compressed, target, window, None).ok_or(PatchFailed)?;
        }
        COMPRESSION_DELTA => return Err(XexUnsupported),
        COMPRESSION_NONE => {}
        _ => return Err(XexInvalid),
    }

    // The new header says that neither encryption nor compression is used.
    let new_format = optional_header(&out, FILE_FORMAT_INFO).ok_or(PatchFailed)?;
    out.get_mut(new_format + FORMAT_ENCRYPTION..new_format + FORMAT_COMPRESSION + 2)
        .ok_or(PatchFailed)?
        .copy_from_slice(&[0, ENCRYPTION_NONE as u8, 0, COMPRESSION_NONE as u8]);

    // Decrypt the patch's data if it is encrypted.
    let patch_header_size = be32(xexp, HEADER_SIZE).ok_or(PatchInvalid)? as usize;
    let mut patch_data = xexp.get(patch_header_size..).ok_or(PatchInvalid)?.to_vec();
    match be16(xexp, patch_format + FORMAT_ENCRYPTION).ok_or(PatchInvalid)? {
        ENCRYPTION_NORMAL => {
            // The C would decrypt past the end of a buffer that is not whole blocks.
            if patch_data.len() % 16 != 0 {
                return Err(PatchInvalid);
            }
            crypto::aes128_cbc_decrypt(&patch_key, &mut patch_data);
        }
        ENCRYPTION_NONE => {}
        _ => return Err(PatchInvalid),
    }

    // Apply the patch to the image.
    let new_xex_header_size = be32(&out, HEADER_SIZE).ok_or(PatchFailed)? as usize;
    let exe = out.get_mut(new_xex_header_size..).ok_or(PatchFailed)?;
    let image_source_offset = field(DESCRIPTOR_IMAGE_SOURCE_OFFSET)? as usize;
    if image_source_offset > 0 {
        let size = field(DESCRIPTOR_IMAGE_SOURCE_SIZE)? as usize;
        let to = field(DESCRIPTOR_IMAGE_TARGET_OFFSET)? as usize;
        move_run(exe, image_source_offset, size, to).ok_or(PatchFailed)?;
    }
    let mut block_size = be32(xexp, patch_format + FORMAT_FIRST_BLOCK).ok_or(PatchInvalid)? as usize;
    let mut block_hash = sha1_at(xexp, patch_format + FORMAT_FIRST_BLOCK + 4).ok_or(PatchInvalid)?;
    let mut at = 0usize;
    while block_size > 0 {
        // Hash and validate the block.
        let block = patch_data.get(at..at.checked_add(block_size).ok_or(PatchFailed)?).ok_or(PatchFailed)?;
        let next_size = be32(block, 0).ok_or(PatchFailed)? as usize;
        let next_hash = sha1_at(block, 4).ok_or(PatchFailed)?;
        if crypto::sha1(block) != block_hash {
            return Err(PatchFailed);
        }
        // Apply the block's records.
        at += BLOCK_HEADER;
        let records_end = at + (block_size - BLOCK_HEADER);
        apply_records(&patch_data, at, records_end, window_size, exe).ok_or(PatchFailed)?;
        at = records_end;
        block_size = next_size;
        block_hash = next_hash;
    }
    Ok(out)
}
