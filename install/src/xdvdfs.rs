// Where an install reads the game from: an Xbox 360 disc image, whose file system is
// XDVDFS, or a folder a disc was already extracted to. Only the files at the disc's
// root are listed: everything the title reads is there, and the one folder a disc has
// beside them is the console's system update.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 2048;
const MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";
// Where the game partition starts, on the disc images there are: a full dump carries
// the video partition first.
const PARTITIONS: [u64; 5] = [0x0, 0xFD90000, 0x2080000, 0x18300000, 0x18310000];

/// A file at the disc's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// As the disc spells it, its bytes taken one for one as characters.
    pub name: String,
    pub size: u64,
    // Where it starts in the image; unused for a folder.
    offset: u64,
}

enum Kind {
    Image(fs::File),
    // The file last read stays open, as a copy reads one file a piece at a time.
    Folder { root: PathBuf, open: Option<(String, fs::File)> },
}

pub struct Disc {
    kind: Kind,
    files: Vec<File>,
}

fn le16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

// As many bytes as there are, up to the size: a short read only at the file's end.
fn read_at(file: &mut fs::File, at: u64, size: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut data = Vec::new();
    file.take(size).read_to_end(&mut data)?;
    Ok(data)
}

// An install copies a disc's file under the name the image gives it, so a name has to
// be one file in a folder: not a path ("..\x.ff", "/home/x.ff"), not a drive
// ("C:x.ff"). An XDVDFS name never holds a separator or a colon; one that does is not
// from a disc, and is skipped.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

// A folder is a binary tree of entries at 4-byte offsets into it:
// { left u16, right u16, sector u32, size u32, attributes u8, name length u8, name }.
// A damaged one can point back into itself, so an entry is visited once.
fn root_files(folder: &[u8], base: u64) -> Vec<File> {
    let mut files = Vec::new();
    let mut pending = vec![0usize];
    let mut seen = HashSet::new();
    while let Some(offset) = pending.pop() {
        let at = offset * 4;
        if at + 14 > folder.len() || !seen.insert(at) {
            continue;
        }
        let (left, right) = (le16(folder, at), le16(folder, at + 2));
        if left == 0xFFFF {
            continue;
        }
        if left != 0 {
            pending.push(left.into());
        }
        if right != 0 {
            pending.push(right.into());
        }
        let (attributes, length) = (folder[at + 12], usize::from(folder[at + 13]));
        // A folder, or a name past the end.
        if attributes & 0x10 != 0 || at + 14 + length > folder.len() {
            continue;
        }
        let name: String = folder[at + 14..at + 14 + length].iter().map(|&byte| char::from(byte)).collect();
        if is_file_name(&name) {
            let offset = base + u64::from(le32(folder, at + 4)) * SECTOR;
            files.push(File { name, size: u64::from(le32(folder, at + 8)), offset });
        }
    }
    files
}

impl Disc {
    /// An image file or a folder; an error says why it is neither.
    pub fn open(path: &Path) -> Result<Disc, String> {
        if path.is_dir() {
            Disc::open_folder(path)
        } else if path.is_file() {
            Disc::open_image(path)
        } else {
            Err(format!("{} is neither a disc image nor a folder", path.display()))
        }
    }

    fn open_image(path: &Path) -> Result<Disc, String> {
        let error = |e: io::Error| format!("cannot read {}: {e}", path.display());
        // The launcher's words, which a player reads.
        let mut file = fs::File::open(path).map_err(|_| format!("cannot open {}", path.display()))?;
        let image_size = file.metadata().map_err(error)?.len();
        let mut base = None;
        for offset in PARTITIONS {
            if read_at(&mut file, offset + 32 * SECTOR, 20).map_err(error)? == MAGIC {
                base = Some(offset);
                break;
            }
        }
        let base = base.ok_or("not an Xbox 360 disc image")?;
        let volume = read_at(&mut file, base + 32 * SECTOR, SECTOR).map_err(error)?;
        if volume.len() < SECTOR as usize {
            return Err("the disc image is cut short".into());
        }
        if &volume[0x7EC..0x800] != MAGIC {
            return Err("the disc image's volume descriptor is damaged".into());
        }
        let (root_sector, root_size) = (u64::from(le32(&volume, 0x14)), u64::from(le32(&volume, 0x18)));
        // Sized from the image's own bytes: compared with the image before anything is
        // allocated for it.
        if base + root_sector * SECTOR + root_size > image_size {
            return Err("the disc image is cut short".into());
        }
        let root = read_at(&mut file, base + root_sector * SECTOR, root_size).map_err(error)?;
        if root.len() as u64 != root_size {
            return Err("the disc image is cut short".into());
        }
        let files = root_files(&root, base);
        if files.is_empty() {
            return Err("the disc image has no files".into());
        }
        Ok(Disc { kind: Kind::Image(file), files })
    }

    fn open_folder(path: &Path) -> Result<Disc, String> {
        // The launcher's words, which a player reads: a folder that cannot be listed and
        // one with nothing to install are alike to them.
        let error = || format!("cannot read the folder {}", path.display());
        let mut files = Vec::new();
        for entry in fs::read_dir(path).map_err(|_| error())? {
            let entry = entry.map_err(|_| error())?;
            // Through a link too, as the files of an extracted disc may be.
            let Ok(metadata) = fs::metadata(entry.path()) else { continue };
            // A name that is not Unicode is none the title reads.
            let Ok(name) = entry.file_name().into_string() else { continue };
            if metadata.is_file() {
                files.push(File { name, size: metadata.len(), offset: 0 });
            }
        }
        if files.is_empty() {
            return Err(error());
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Disc { kind: Kind::Folder { root: path.to_path_buf(), open: None }, files })
    }

    /// The files at the root of the disc.
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// The file of that name, whatever its case.
    pub fn find(&self, name: &str) -> Option<File> {
        self.files.iter().find(|file| file.name.eq_ignore_ascii_case(name)).cloned()
    }

    // The file, at the offset into it, to read from.
    fn seek(&mut self, file: &File, offset: u64) -> io::Result<&mut fs::File> {
        let (handle, at) = match &mut self.kind {
            Kind::Image(image) => (image, file.offset + offset),
            Kind::Folder { root, open } => {
                if open.as_ref().is_none_or(|(name, _)| *name != file.name) {
                    *open = Some((file.name.clone(), fs::File::open(root.join(&file.name))?));
                }
                let (_, handle) = open.as_mut().ok_or(io::ErrorKind::NotFound)?;
                (handle, offset)
            }
        };
        handle.seek(SeekFrom::Start(at))?;
        Ok(handle)
    }

    /// Fills `out` from `offset` into the file; an error if the file is not that long
    /// or the image is cut short.
    pub fn read(&mut self, file: &File, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let error = |e: io::Error| format!("cannot read {} from the disc: {e}", file.name);
        if offset.checked_add(out.len() as u64).is_none_or(|end| end > file.size) {
            return Err(format!("{} is not that long", file.name));
        }
        self.seek(file, offset).map_err(error)?.read_exact(out).map_err(error)
    }

    /// The whole file.
    pub fn read_all(&mut self, file: &File) -> Result<Vec<u8>, String> {
        let size = usize::try_from(file.size).map_err(|_| format!("{} is too large to read", file.name))?;
        let mut data = vec![0; size];
        self.read(file, 0, &mut data)?;
        Ok(data)
    }

    /// The whole file into `out`, which an image cut short leaves short and says so.
    pub fn copy(&mut self, file: &File, out: &mut dyn Write) -> Result<(), String> {
        let error = |e: io::Error| format!("cannot copy {} from the disc: {e}", file.name);
        let mut from = self.seek(file, 0).map_err(error)?.take(file.size);
        let copied = io::copy(&mut from, out).map_err(error)?;
        if copied != file.size {
            return Err(format!("{} is cut short on the disc", file.name));
        }
        Ok(())
    }
}
