// An Xbox 360 content package, the container a title update comes in: its files
// written out, as tools/stfs.py did. Reads the read-only kind (LIVE and PIRS), which
// keeps one hash table per group of 170 blocks; a console-signed CON package keeps
// two and is refused.

use crate::Result;
use std::fs;
use std::path::{Path, PathBuf};

const BLOCK: usize = 0x1000;
const PER_TABLE: usize = 0xAA; // data blocks one hash table covers
const PER_LEVEL1: usize = 0x70E4; // and one second-level table

struct Entry {
    name: String,
    folder: bool,
    blocks: usize,
    start: usize,
    parent: i16,
    size: usize,
}

struct Package {
    data: Vec<u8>,
    entries: Vec<Entry>,
}

// A big- or little-endian number of the bytes there are of the range.
fn number(data: &[u8], from: usize, to: usize, big: bool) -> usize {
    let bytes = &data[from.min(data.len())..to.min(data.len())];
    let fold = |value: usize, &byte: &u8| value << 8 | usize::from(byte);
    if big { bytes.iter().fold(0, fold) } else { bytes.iter().rev().fold(0, fold) }
}

impl Package {
    fn open(path: &Path) -> Result<Package> {
        let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let magic: String = data.iter().take(4).map(|&byte| char::from(byte)).collect();
        if magic != "LIVE" && magic != "PIRS" {
            return Err(format!("{}: a {magic:?} package, not LIVE or PIRS", path.display()));
        }
        if data.len() < 0x381 {
            return Err(format!("{}: too short for a package", path.display()));
        }
        let header_size = number(&data, 0x340, 0x344, true);
        if ((header_size + 0xFFF) & 0xF000) >> 12 != 0xB {
            return Err(format!("{}: two hash tables per group, which this does not read", path.display()));
        }
        let mut package = Package { data, entries: Vec::new() };
        let table_blocks = number(&package.data, 0x37C, 0x37E, false);
        let first = number(&package.data, 0x37E, 0x381, false);
        let table: Vec<u8> =
            package.chain(first, table_blocks).iter().flat_map(|&block| package.block(block)).copied().collect();
        for entry in table.chunks(0x40) {
            if entry[0] == 0 {
                break;
            }
            if entry.len() < 0x38 {
                return Err(format!("{}: a file table entry is cut short", path.display()));
            }
            let flags = entry[0x28];
            package.entries.push(Entry {
                name: entry[..usize::from(flags & 0x3F).min(entry.len())]
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect(),
                folder: flags & 0x80 != 0,
                blocks: number(entry, 0x29, 0x2C, false),
                start: number(entry, 0x2F, 0x32, false),
                parent: i16::from_be_bytes([entry[0x32], entry[0x33]]),
                size: number(entry, 0x34, 0x38, true),
            });
        }
        Ok(package)
    }

    // Where a data block is in the file: the hash tables sit between them.
    fn offset(block: usize) -> usize {
        let mut tables = 0;
        if block >= PER_TABLE {
            tables += block / PER_TABLE + 1;
        }
        if block >= PER_LEVEL1 {
            tables += block / PER_LEVEL1 + 1;
        }
        0xC000 + (block + tables) * BLOCK
    }

    fn block(&self, block: usize) -> &[u8] {
        let at = Package::offset(block);
        &self.data[at.min(self.data.len())..(at + BLOCK).min(self.data.len())]
    }

    // The block after this one in its file: its entry in the hash table, which is the
    // block before its group's first.
    fn next(&self, block: usize) -> usize {
        let table = Package::offset(block - block % PER_TABLE) - BLOCK;
        let entry = table + (block % PER_TABLE) * 0x18;
        number(&self.data, entry + 0x15, entry + 0x18, true)
    }

    fn chain(&self, start: usize, count: usize) -> Vec<usize> {
        let mut blocks = Vec::with_capacity(count);
        let mut block = start;
        for _ in 0..count {
            blocks.push(block);
            block = self.next(block);
        }
        blocks
    }

    // A folder in a damaged package can be its own ancestor: no path is deeper than
    // there are entries.
    fn path(&self, entry: &Entry, depth: usize) -> Result<PathBuf> {
        if entry.parent < 0 {
            return Ok(PathBuf::from(&entry.name));
        }
        let parent = self.entries.get(entry.parent as usize).ok_or("a file's folder is not in the package")?;
        if depth > self.entries.len() {
            return Err("the package's folders make a loop".into());
        }
        Ok(self.path(parent, depth + 1)?.join(&entry.name))
    }

    fn read(&self, entry: &Entry) -> Vec<u8> {
        let mut data: Vec<u8> =
            self.chain(entry.start, entry.blocks).iter().flat_map(|&block| self.block(block)).copied().collect();
        data.truncate(entry.size);
        data
    }
}

/// Every file of the package, into the folder.
pub fn extract(package: &Path, folder: &Path) -> Result {
    let package = Package::open(package)?;
    for entry in package.entries.iter().filter(|entry| !entry.folder) {
        let out = folder.join(package.path(entry, 0)?);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&out, package.read(entry)).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(())
}
