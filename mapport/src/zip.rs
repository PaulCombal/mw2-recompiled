// A zip archive (a PC map's .iwd): its entries' names, in the order the archive lists
// them, and an entry's bytes (stored or deflated).

use crate::error::{Error, Result};
use crate::paths::{self, utf8};
use crate::text;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

fn u16_at(p: &[u8], at: usize) -> u32 {
    u32::from(p[at]) | u32::from(p[at + 1]) << 8
}

fn u32_at(p: &[u8], at: usize) -> u32 {
    u16_at(p, at) | u16_at(p, at + 2) << 16
}

// An entry inflated takes at most this much, and no more than deflate can make of its
// packed bytes (1,032 to 1): a broken header can't ask for gigabytes.
const LARGEST_ENTRY: u64 = 512 << 20;

// Up to `n` bytes at `at`, as many as the file has there.
fn read_at(file: &mut File, at: u64, n: usize) -> Vec<u8> {
    let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let want = if at < size { (n as u64).min(size - at) as usize } else { 0 };
    let mut b = vec![0u8; want];
    if file.seek(SeekFrom::Start(at)).is_err() {
        return Vec::new();
    }
    let mut got = 0;
    while got < b.len() {
        match file.read(&mut b[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    b.truncate(got);
    b
}

#[derive(Clone, Copy)]
struct Item {
    method: u32,
    packed: u32,
    size: u32,
    local: u64,
}

pub struct Zip {
    path: PathBuf,
    names: Vec<String>,
    items: Vec<Item>,
}

impl Zip {
    /// An Error when the file is not a zip archive.
    pub fn open(path: &Path) -> Result<Zip> {
        let mut file = File::open(path).map_err(|_| Error::new(format!("cannot open {}", utf8(path))))?;
        let file_name = || paths::file_name(path);
        let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
        // The end of central directory record, within the last 64 KB (its comment).
        let tail = size.min(0x10000 + 22);
        let end = read_at(&mut file, size - tail, tail as usize);
        let mut at = if end.len() >= 22 { end.len() - 22 } else { 0 };
        while at > 0 && u32_at(&end, at) != 0x06054b50 {
            at -= 1;
        }
        if end.len() < 22 || u32_at(&end, at) != 0x06054b50 {
            return Err(Error::new(format!("{} is not a zip archive", file_name())));
        }
        let count = u16_at(&end, at + 10);
        let dir = read_at(&mut file, u64::from(u32_at(&end, at + 16)), u32_at(&end, at + 12) as usize);
        let broken = || Error::new(format!("{}: a broken directory", file_name()));
        let mut names = Vec::new();
        let mut items = Vec::new();
        let mut p = 0usize;
        for _ in 0..count {
            if p + 46 > dir.len() || u32_at(&dir, p) != 0x02014b50 {
                return Err(broken());
            }
            let (name_length, extra, comment) =
                (u16_at(&dir, p + 28) as usize, u16_at(&dir, p + 30) as usize, u16_at(&dir, p + 32) as usize);
            if p + 46 + name_length > dir.len() {
                return Err(broken());
            }
            // A name's bytes, each the char of its value, as the zone's names are kept
            // (crate::text).
            names.push(text::of(&dir[p + 46..p + 46 + name_length]));
            items.push(Item {
                method: u16_at(&dir, p + 10),
                packed: u32_at(&dir, p + 20),
                size: u32_at(&dir, p + 24),
                local: u64::from(u32_at(&dir, p + 42)),
            });
            p += 46 + name_length + extra + comment;
        }
        Ok(Zip { path: path.to_path_buf(), names, items })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// An Error for an entry it doesn't have or can't read.
    pub fn read_named(&self, name: &str) -> Result<Vec<u8>> {
        match self.names.iter().position(|n| n == name) {
            Some(k) => self.read(k),
            None => Err(Error::new(format!("{} has no {name}", paths::file_name(&self.path)))),
        }
    }

    pub fn read(&self, index: usize) -> Result<Vec<u8>> {
        let it = *self.items.get(index).ok_or_else(|| Error::new("a zip entry it doesn't have"))?;
        let name = &self.names[index];
        let mut file = File::open(&self.path).map_err(|_| Error::new(format!("cannot open {}", utf8(&self.path))))?;
        let local = read_at(&mut file, it.local, 30);
        if local.len() < 30 || u32_at(&local, 0) != 0x04034b50 {
            return Err(Error::new(format!("{name}: a broken entry")));
        }
        let start = it.local + 30 + u64::from(u16_at(&local, 26)) + u64::from(u16_at(&local, 28));
        let data = read_at(&mut file, start, it.packed as usize);
        if data.len() != it.packed as usize {
            return Err(Error::new(format!("{name} is cut short")));
        }
        if it.method == 0 {
            return Ok(data);
        }
        if it.method != 8 {
            return Err(Error::new(format!("{name} is compressed in a way that can't be read")));
        }
        if u64::from(it.size) > LARGEST_ENTRY || u64::from(it.size) > u64::from(it.packed) * 1032 + 64 {
            return Err(Error::new(format!("{name}: a broken entry")));
        }
        mw2_install::inflate::inflate_raw(&data, it.size as usize)
            .ok_or_else(|| Error::new(format!("{name} does not inflate")))
    }
}
