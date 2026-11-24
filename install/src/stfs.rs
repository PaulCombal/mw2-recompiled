// An Xbox 360 content package (STFS), the container a title update comes in. Reads the
// read-only kind a downloaded update is (LIVE and PIRS), which keeps one hash table per
// group of 170 blocks; a console-signed CON package keeps two, lays its blocks out
// differently, and is refused.

use std::fs;
use std::path::{Path, PathBuf};

const BLOCK: usize = 0x1000;
const PER_TABLE: usize = 0xAA; // data blocks one hash table covers
const PER_LEVEL1: usize = 0x70E4; // and one second-level table
// Where the first data block's group starts, after the header.
const DATA: usize = 0xC000;

/// A file of the package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: String,
    /// The name with the folders above it, joined by /.
    pub path: String,
    pub size: usize,
    blocks: usize,
    start: usize,
}

pub struct Package {
    data: Vec<u8>,
    files: Vec<File>,
}

struct Entry {
    name: String,
    folder: bool,
    parent: i16,
    blocks: usize,
    start: usize,
    size: usize,
}

fn le24(data: &[u8], at: usize) -> usize {
    usize::from(data[at]) | usize::from(data[at + 1]) << 8 | usize::from(data[at + 2]) << 16
}

fn be24(data: &[u8], at: usize) -> usize {
    usize::from(data[at]) << 16 | usize::from(data[at + 1]) << 8 | usize::from(data[at + 2])
}

fn be32(data: &[u8], at: usize) -> usize {
    be24(data, at) << 8 | usize::from(data[at + 3])
}

// A package's names become files in a folder, so one has to be a name, not a path.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
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
    DATA + (block + tables) * BLOCK
}

// The block's bytes, whole; none if the package ends first.
fn block(data: &[u8], block: usize) -> Option<&[u8]> {
    let at = offset(block);
    data.get(at..at + BLOCK)
}

// The block after this one in its file: its entry in the hash table, which is the block
// before its group's first.
fn next(data: &[u8], block: usize) -> Option<usize> {
    let entry = offset(block - block % PER_TABLE) - BLOCK + (block % PER_TABLE) * 0x18;
    data.get(entry..entry + 0x18).map(|entry| be24(entry, 0x15))
}

// The blocks of a file, one after another. A damaged package can chain a block back to
// one before it, so a file has no more blocks than the package does.
fn chain(data: &[u8], start: usize, count: usize) -> Option<Vec<u8>> {
    if count > data.len() / BLOCK {
        return None;
    }
    let mut out = Vec::with_capacity(count * BLOCK);
    let mut at = start;
    for i in 0..count {
        out.extend_from_slice(block(data, at)?);
        if i + 1 < count {
            at = next(data, at)?;
        }
    }
    Some(out)
}

impl Package {
    /// The package's files listed; an error says why it is not such a package.
    pub fn parse(data: Vec<u8>) -> Result<Package, String> {
        if data.len() < DATA || !(data.starts_with(b"LIVE") || data.starts_with(b"PIRS")) {
            return Err("it is not an Xbox 360 content package".into());
        }
        if ((be32(&data, 0x340) + 0xFFF) & 0xF000) >> 12 != 0xB {
            return Err("it is not a read-only package".into());
        }
        let table_blocks = usize::from(u16::from_le_bytes([data[0x37C], data[0x37D]]));
        let table = chain(&data, le24(&data, 0x37E), table_blocks).ok_or("its file table is cut short")?;

        // { name[0x28], flags (name length, folder), blocks le24, blocks again, start le24,
        //   parent be16, size be32, times }
        let mut entries = Vec::new();
        for entry in table.chunks_exact(0x40).take_while(|entry| entry[0] != 0) {
            let flags = entry[0x28];
            let name: String = entry[..usize::from(flags & 0x3F)].iter().map(|&byte| char::from(byte)).collect();
            if !is_file_name(&name) {
                return Err(format!("it holds a file named {name:?}, which is not a file name"));
            }
            entries.push(Entry {
                name,
                folder: flags & 0x80 != 0,
                parent: i16::from_be_bytes([entry[0x32], entry[0x33]]),
                blocks: le24(entry, 0x29),
                start: le24(entry, 0x2F),
                size: be32(entry, 0x34),
            });
        }

        let mut files = Vec::new();
        for entry in entries.iter().filter(|entry| !entry.folder) {
            // A folder in a damaged package can be its own ancestor: no path is deeper
            // than there are entries.
            let mut path = entry.name.clone();
            let mut parent = entry.parent;
            for _ in 0..=entries.len() {
                let Ok(index) = usize::try_from(parent) else { break };
                let folder = entries
                    .get(index)
                    .filter(|folder| folder.folder)
                    .ok_or_else(|| format!("{} in it is in a folder it does not have", entry.name))?;
                path = format!("{}/{path}", folder.name);
                parent = folder.parent;
            }
            if parent >= 0 {
                return Err("its folders make a loop".into());
            }
            files.push(File {
                name: entry.name.clone(),
                path,
                size: entry.size,
                blocks: entry.blocks,
                start: entry.start,
            });
        }
        Ok(Package { data, files })
    }

    /// The files, not the folders.
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// The file's bytes; an error if the package ends before them.
    pub fn read(&self, file: &File) -> Result<Vec<u8>, String> {
        match chain(&self.data, file.start, file.blocks) {
            Some(mut bytes) if bytes.len() >= file.size => {
                bytes.truncate(file.size);
                Ok(bytes)
            }
            _ => Err(format!("{} in it is cut short", file.name)),
        }
    }

    /// Every file into the folder, in the package's folders.
    pub fn extract(&self, folder: &Path) -> Result<(), String> {
        for file in &self.files {
            let out: PathBuf = folder.join(&file.path);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            fs::write(&out, self.read(file)?).map_err(|e| format!("{}: {e}", out.display()))?;
        }
        Ok(())
    }
}
