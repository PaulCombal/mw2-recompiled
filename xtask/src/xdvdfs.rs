// The Xbox 360 disc's file system (XDVDFS), read to take files from the retail ISO:
// what tools/xdvdfs.py did, the launcher's disc.cpp reading the same.

use crate::Result;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECTOR: u64 = 2048;
const MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";

struct Entry {
    // The path from the root, a folder's with a / at the end: the disc's bytes taken
    // one for one as characters.
    name: String,
    sector: u32,
    size: u32,
    attributes: u8,
    folder: bool,
}

struct Disc {
    file: File,
    base: u64,
    root_sector: u32,
    root_size: u32,
}

fn le16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

// As many bytes as there are, up to the size: a short read only at the file's end.
fn read_at(file: &mut File, at: u64, size: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut data = Vec::new();
    file.take(size).read_to_end(&mut data)?;
    Ok(data)
}

impl Disc {
    fn open(path: &Path) -> Result<Disc> {
        let error = |e: std::io::Error| format!("{}: {e}", path.display());
        let mut file = File::open(path).map_err(error)?;
        // Where the game partition starts, on the disc images there are.
        let mut base = None;
        for offset in [0x0, 0xFD90000, 0x2080000, 0x18300000, 0x18310000] {
            if read_at(&mut file, offset + 32 * SECTOR, 20).map_err(error)? == MAGIC {
                base = Some(offset);
                break;
            }
        }
        let base = base.ok_or(format!("{}: XDVDFS magic not found at known offsets", path.display()))?;
        let descriptor = read_at(&mut file, base + 32 * SECTOR, SECTOR).map_err(error)?;
        if descriptor.len() < SECTOR as usize || &descriptor[..20] != MAGIC || &descriptor[0x7EC..0x800] != MAGIC {
            return Err(format!("{}: bad volume descriptor", path.display()));
        }
        Ok(Disc { file, base, root_sector: le32(&descriptor, 0x14), root_size: le32(&descriptor, 0x18) })
    }

    // A folder is a binary tree of entries, each at a 4-byte offset into it.
    // A damaged image's folder can hold itself: 64 levels are past any disc's.
    fn walk_folder(&mut self, sector: u32, size: u32, prefix: &str, out: &mut Vec<Entry>, depth: usize) -> Result {
        if depth > 64 {
            return Err("the disc's folders nest past 64 levels".into());
        }
        if size == 0 {
            return Ok(());
        }
        let data = read_at(&mut self.file, self.base + u64::from(sector) * SECTOR, u64::from(size))
            .map_err(|e| format!("the disc: {e}"))?;
        let mut stack = vec![0usize];
        let mut seen = HashSet::new();
        while let Some(offset) = stack.pop() {
            if seen.contains(&offset) || offset * 4 + 14 > data.len() {
                continue;
            }
            seen.insert(offset);
            let at = offset * 4;
            let (left, right) = (le16(&data, at), le16(&data, at + 2));
            let (start, file_size) = (le32(&data, at + 4), le32(&data, at + 8));
            let (attributes, length) = (data[at + 12], data[at + 13] as usize);
            if left == 0xFFFF {
                continue;
            }
            let end = (at + 14 + length).min(data.len());
            let name: String = data[at + 14..end].iter().map(|&byte| char::from(byte)).collect();
            if left != 0 {
                stack.push(left.into());
            }
            if right != 0 {
                stack.push(right.into());
            }
            let full = format!("{prefix}{name}");
            if attributes & 0x10 != 0 {
                out.push(Entry { name: format!("{full}/"), sector: start, size: file_size, attributes, folder: true });
                self.walk_folder(start, file_size, &format!("{full}/"), out, depth + 1)?;
            } else {
                out.push(Entry { name: full, sector: start, size: file_size, attributes, folder: false });
            }
        }
        Ok(())
    }

    fn walk(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        self.walk_folder(self.root_sector, self.root_size, "", &mut entries, 0)?;
        entries.sort_by(|a, b| {
            (&a.name, a.sector, a.size, a.attributes, a.folder).cmp(&(
                &b.name,
                b.sector,
                b.size,
                b.attributes,
                b.folder,
            ))
        });
        Ok(entries)
    }

    // The file's bytes, or as many of them as the image has.
    fn extract(&mut self, entry: &Entry, destination: &Path) -> Result {
        let error = |e: std::io::Error| format!("{}: {e}", destination.display());
        if let Some(parent) = destination.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(error)?;
        }
        self.file.seek(SeekFrom::Start(self.base + u64::from(entry.sector) * SECTOR)).map_err(error)?;
        let mut out = File::create(destination).map_err(error)?;
        std::io::copy(&mut (&mut self.file).take(u64::from(entry.size)), &mut out).map_err(error)?;
        Ok(())
    }
}

/// The files whose path holds one of the names, whatever its case, into the folder.
pub fn extract_named(iso: &Path, folder: &Path, names: &[&str]) -> Result {
    let mut disc = Disc::open(iso)?;
    for entry in disc.walk()? {
        let lower = entry.name.to_lowercase();
        if entry.folder || !names.iter().any(|name| lower.contains(&name.to_lowercase())) {
            continue;
        }
        let destination = folder.join(&entry.name);
        disc.extract(&entry, &destination)?;
        println!("extracted {} -> {} ({} bytes)", entry.name, destination.display(), entry.size);
    }
    Ok(())
}

/// Every .ff, .pak and .bik the folder does not have yet.
pub fn extract_game(iso: &Path, folder: &Path) -> Result {
    let mut disc = Disc::open(iso)?;
    let have: HashSet<String> = fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    let missing: Vec<Entry> = disc
        .walk()?
        .into_iter()
        .filter(|entry| {
            !entry.folder
                && [".ff", ".pak", ".bik"].iter().any(|end| entry.name.ends_with(end))
                && !have.contains(&entry.name)
        })
        .collect();
    if !missing.is_empty() {
        let bytes: u64 = missing.iter().map(|entry| u64::from(entry.size)).sum();
        println!("    extracting {} files, {:.2} GB", missing.len(), bytes as f64 / 1e9);
    }
    for entry in &missing {
        disc.extract(entry, &folder.join(&entry.name))?;
        println!("    {} ({} bytes)", entry.name, entry.size);
    }
    Ok(())
}
