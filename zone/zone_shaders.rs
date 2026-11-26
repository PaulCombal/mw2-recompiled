// The reader (src/) on the files given -- fastfiles, or executables (.xex) for the
// shaders they carry themselves, which come first: for each, how many shaders it holds,
// how long it took (and of that, to inflate a fastfile's zone), and with --hashes <dir> one "<pixel> <hash>" line per shader in
// <dir>/<file>.hashes, sorted, to compare with another reader's; with --dump <dir> each
// shader's microcode, named as MW2_DUMP_SHADERS names the ones a run draws with.
//
//   cargo run --release -p mw2-zone --bin zone-shaders -- [--hashes dir] [--dump dir] \
//       game/default_mp.xex game/default.xex game/*.ff

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use mw2_zone::shaders::{self, BindCounts, Shader};
use mw2_zone::vertex_binding::BindTables;
use mw2_zone::{Error, Result, fastfile};

/// A file read: its shaders, its zone's size, and how long a fastfile's took to inflate.
struct Read {
    shaders: Vec<Shader>,
    size: usize,
    inflate: Option<f64>,
}

/// One file's shaders, `tables` taken from an executable.
fn read(path: &Path, tables: &mut Option<BindTables>, counts: &mut BindCounts) -> Result<Read> {
    let name = fastfile::name(path);
    if path.extension().is_some_and(|e| e == "xex") {
        let xex = std::fs::read(path).map_err(|_| Error::new(format!("cannot open {name}")))?;
        let image = mw2_install::xex::image(&xex).map_err(Error)?;
        let multiplayer = name == "default_mp.xex";
        let read = BindTables::read(&image, multiplayer)?;
        let shaders = shaders::executable_shaders(&image, multiplayer, &read)?;
        if tables.as_ref().is_some_and(|t| *t != read) {
            return Err(Error::new("the two executables bind vertex shaders with different tables"));
        }
        *tables = Some(read);
        return Ok(Read { shaders, size: image.len(), inflate: None });
    }
    let tables = tables.as_ref().ok_or_else(|| Error::new("give the executables (.xex) before the fastfiles"))?;
    let from = Instant::now();
    let zone = fastfile::read(path)?;
    let inflate = from.elapsed().as_secs_f64() * 1000.0;
    let (shaders, these) = shaders::shaders(&zone, tables)?;
    counts.bound += these.bound;
    counts.unbound += these.unbound;
    counts.no_declaration += these.no_declaration;
    counts.own_shader += these.own_shader;
    Ok(Read { shaders, size: zone.len(), inflate: Some(inflate) })
}

fn main() -> ExitCode {
    let mut hashes: Option<PathBuf> = None;
    let mut dump: Option<PathBuf> = None;
    let mut failed = 0;
    let mut all = BTreeSet::new();
    let mut tables = None;
    let mut counts = BindCounts::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--hashes" || arg == "--dump" {
            let into = if arg == "--hashes" { &mut hashes } else { &mut dump };
            *into = args.next().map(PathBuf::from);
            continue;
        }
        let path = PathBuf::from(arg);
        let name = fastfile::name(&path);
        let from = Instant::now();
        let Read { mut shaders, size, inflate } = match read(&path, &mut tables, &mut counts) {
            Ok(read) => read,
            Err(e) => {
                println!("{name}: {e}");
                failed += 1;
                continue;
            }
        };
        let ms = from.elapsed().as_secs_f64() * 1000.0;
        let pixel = shaders.iter().filter(|s| s.pixel).count();
        all.extend(shaders.iter().map(|s| (s.pixel, s.hash)));
        let inflated = inflate.map(|ms| format!(" ({ms:.0} ms to inflate)")).unwrap_or_default();
        println!(
            "{name}: {} shaders ({} vertex, {pixel} pixel), {size} bytes, {ms:.0} ms{inflated}",
            shaders.len(),
            shaders.len() - pixel
        );
        if let Some(dir) = &dump {
            for s in &shaders {
                let kind = if s.pixel { "pixel" } else { "vertex" };
                let bytes: Vec<u8> = s.code.iter().flat_map(|w| w.to_be_bytes()).collect();
                let _ = std::fs::write(dir.join(format!("{kind}_{:016x}.ucode", s.hash)), bytes);
            }
        }
        if let Some(dir) = &hashes {
            shaders.sort_by_key(|s| (s.pixel, s.hash));
            let mut text = String::new();
            for s in &shaders {
                let _ = writeln!(text, "{} {:016X}", u8::from(s.pixel), s.hash);
            }
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let _ = std::fs::write(dir.join(format!("{stem}.hashes")), text);
        }
    }
    println!(
        "{} distinct shaders, {failed} file{} failed; vertex shaders: {} bound, {} never bound, {} with no \
         declaration for their slot, {} a pass's own",
        all.len(),
        if failed == 1 { "" } else { "s" },
        counts.bound,
        counts.unbound,
        counts.no_declaration,
        counts.own_shader
    );
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
