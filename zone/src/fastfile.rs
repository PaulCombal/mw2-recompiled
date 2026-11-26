// A 360 fastfile's zone: the file is a short header, a table of the pictures it streams
// from the image paks, then one zlib stream. A signed file (IWff0100) interleaves that
// stream with hash blocks: 8 KB after its own 8 KB header, then 8 KB before each 2 MB of
// the stream.

use std::path::Path;

use mw2_install::inflate;

use crate::{Error, Result};

/// A file's name, as the errors give it.
pub fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn big32(p: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

/// The zone of the fastfile at `path`; an Error when it is not a 360 fastfile, or its
/// stream is cut short or damaged.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let raw = std::fs::read(path).map_err(|_| Error::new(format!("cannot open {}", name(path))))?;
    zone(&raw, &name(path))
}

/// The zone of the fastfile `raw`, named `name` in the errors.
pub fn zone(raw: &[u8], name: &str) -> Result<Vec<u8>> {
    if raw.len() < 0x1D || (&raw[..8] != b"IWffu100" && &raw[..8] != b"IWff0100") {
        return Err(Error::new(format!("{name} is not a 360 fastfile")));
    }
    // The picture table: a count, then 12 bytes each; then two sizes.
    let pictures = big32(raw, 0x19) as usize;
    let body = pictures.checked_mul(12).and_then(|n| n.checked_add(0x1D + 8)).filter(|&b| b <= raw.len());
    let Some(body) = body else { return Err(Error::new(format!("{name} is cut short"))) };

    let signed;
    let stream: &[u8] = if raw.len() - body >= 8 && &raw[body..body + 8] == b"IWffs100" {
        let mut joined = Vec::with_capacity(raw.len() - body);
        let mut at = body + 0x2000;
        while at < raw.len() {
            at += 0x2000;
            if at >= raw.len() {
                break;
            }
            let n = (raw.len() - at).min(0x200000);
            joined.extend_from_slice(&raw[at..at + n]);
            at += 0x200000;
        }
        signed = joined;
        &signed
    } else {
        &raw[body..]
    };
    inflate::unzlib(stream, usize::MAX).ok_or_else(|| Error::new(format!("{name}: its zone does not decompress")))
}
