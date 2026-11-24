// The files a build is made from, taken out of the retail disc and the title update's
// package with the reader the game and the launcher share (install/), the executables
// the update's patches make, applied as the launcher applies them, and the image the
// recompiler reads made from an executable as the game makes it at every start.

use crate::Result;
use mw2_install::xdvdfs::{Disc, File};
use mw2_install::{patch, stfs, xex};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

// The file into the folder under its own name. One cut short is removed rather than
// left, as a file there is taken for done by every later build.
fn extract(disc: &mut Disc, file: &File, destination: &Path) -> Result {
    let mut out = fs::File::create(destination).map_err(|e| format!("{}: {e}", destination.display()))?;
    if let Err(error) = disc.copy(file, &mut out) {
        drop(out);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

fn open(iso: &Path) -> Result<Disc> {
    Disc::open(iso).map_err(|e| format!("{}: {e}", iso.display()))
}

/// The disc's files whose name holds one of the names, whatever its case, into the
/// folder.
pub fn extract_named(iso: &Path, folder: &Path, names: &[&str]) -> Result {
    let mut disc = open(iso)?;
    for file in disc.files().to_vec() {
        let lower = file.name.to_lowercase();
        if !names.iter().any(|name| lower.contains(&name.to_lowercase())) {
            continue;
        }
        let destination = folder.join(&file.name);
        extract(&mut disc, &file, &destination)?;
        println!("extracted {} -> {} ({} bytes)", file.name, destination.display(), file.size);
    }
    Ok(())
}

/// Every .ff, .pak and .bik the folder does not have yet.
pub fn extract_game(iso: &Path, folder: &Path) -> Result {
    let mut disc = open(iso)?;
    let have: HashSet<String> = fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    let missing: Vec<File> = disc
        .files()
        .iter()
        .filter(|file| [".ff", ".pak", ".bik"].iter().any(|end| file.name.ends_with(end)) && !have.contains(&file.name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let bytes: u64 = missing.iter().map(|file| file.size).sum();
        println!("    extracting {} files, {:.2} GB", missing.len(), bytes as f64 / 1e9);
    }
    for file in &missing {
        extract(&mut disc, file, &folder.join(&file.name))?;
        println!("    {} ({} bytes)", file.name, file.size);
    }
    Ok(())
}

/// Every file of the update's package, into the folder.
pub fn extract_package(package: &Path, folder: &Path) -> Result {
    let data = fs::read(package).map_err(|e| format!("{}: {e}", package.display()))?;
    stfs::Package::parse(data)
        .and_then(|parsed| parsed.extract(folder))
        .map_err(|e| format!("{}: {e}", package.display()))
}

/// The disc's executable with the update's patch applied. One cut short is removed,
/// as one there is taken for done by every later build.
pub fn write_patched(xex: &Path, xexp: &Path, out: &Path) -> Result {
    let base = fs::read(xex).map_err(|e| format!("{}: {e}", xex.display()))?;
    let update = fs::read(xexp).map_err(|e| format!("{}: {e}", xexp.display()))?;
    let patched = patch::apply(&base, &update).map_err(|e| format!("{} on {}: {e}", xexp.display(), xex.display()))?;
    if let Err(error) = fs::write(out, &patched) {
        let _ = fs::remove_file(out);
        return Err(format!("{}: {error}", out.display()));
    }
    println!("    patched {} -> {} ({} bytes)", xex.display(), out.display(), patched.len());
    Ok(())
}

/// The executable's flat memory image, checked to be a PE before it is written: one
/// left behind would be taken for done by every later build.
pub fn write_image(xex: &Path, out: &Path) -> Result {
    let file = fs::read(xex).map_err(|e| format!("{}: {e}", xex.display()))?;
    let image = xex::image(&file).map_err(|e| format!("{}: {e}", xex.display()))?;
    let pe = image.get(0x3C..0x40).map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize);
    if pe.and_then(|pe| image.get(pe..pe + 4)) != Some(b"PE\0\0") {
        return Err(format!("{}: not a PE after decrypting", xex.display()));
    }
    fs::write(out, &image).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("    image {} bytes -> {}", image.len(), out.display());
    Ok(())
}
