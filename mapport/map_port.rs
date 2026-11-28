// The map converter (src/) from a terminal, and the checks it is built against.
//
//     map-port roundtrip <file.ff>...    read each zone as a tree and write it back
//                                        unchanged: it must give the same bytes
//     map-port list <file.ff>            every asset of the asset list: type, struct, name
//     map-port fit <file.iwd> <picture> <limit> <out>
//                                        the picture as the converter resizes one
//                                        the 360 can't take as it is: its top level
//     map-port canonical <file.ff> <zone> write the tree as converted files are
//                                        written (from members, padding zero) and
//                                        compare with a zone
//     map-port iwi <file.iwd> <picture> <out>
//                                        a picture as the converter reads it: every
//                                        level, largest first, one after another
//     map-port xma <file.wav> <out>     a sound encoded as the converter encodes
//                                        one: the packets, and on the standard
//                                        output its length, end and seek table
//     map-port port <map.ff> <out dir> <stock dir> [PC game folder]
//                                        convert a PC map with the stock 360 files
//                                        in a folder (the game's), and the PC
//                                        game's pictures and sounds

use mw2_mapport::error::{Error, Result};
use mw2_mapport::fastfile::{self, Fastfile};
use mw2_mapport::images::{self, Picture};
use mw2_mapport::port::{self, PcMap, PortOptions};
use mw2_mapport::porter::Log;
use mw2_mapport::query;
use mw2_mapport::reader::{self, Platform, WriteOptions};
use mw2_mapport::rules;
use mw2_mapport::text;
use mw2_mapport::tree::{Store, Tree};
use mw2_zone::schema::Schema;
use std::time::Instant;
use mw2_mapport::xma;
use mw2_mapport::zip::Zip;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::process::ExitCode;

// The converter's text printed as the bytes it stands for (crate::text).
fn out(line: &str) {
    let _ = std::io::stdout().lock().write_all(&text::bytes(line));
}

fn read_all(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|_| Error::new(format!("cannot open {path}")))
}

fn platform(ff: &Fastfile) -> Result<(&'static Schema, &'static Platform)> {
    Ok(if ff.pc { (rules::pc()?, reader::pc()) } else { (rules::xbox()?, reader::xbox()) })
}

fn roundtrip(files: &[&str]) -> u8 {
    let mut failed = 0;
    for file in files {
        let t0 = Instant::now();
        let mut st = Store::new();
        let done = (|| -> Result<(bool, String)> {
            let ff = fastfile::read_fastfile(Path::new(file))?;
            let (rules, plat) = platform(&ff)?;
            let (tree, _) = reader::read_tree(&mut st, rules, plat, ff.zone.clone(), false)?;
            let read = t0.elapsed().as_secs_f64();
            let options = WriteOptions { keep_bytes: true, ..WriteOptions::default() };
            let out = reader::write_tree(&mut st, &tree, rules, plat, &options, Some(&tree.header))?.zone;
            let diff = out.iter().zip(&ff.zone).take_while(|(a, b)| a == b).count();
            let same = out == ff.zone;
            let mut line = format!(
                "{file}: {}, {} assets, {} nodes, read {read:.1} s, written {:.1} s: {}",
                if ff.pc { "pc" } else { "xbox" },
                tree.assets.len(),
                st.count(),
                t0.elapsed().as_secs_f64() - read,
                if same { "the same bytes" } else { "DIFFERENT" }
            );
            if !same {
                line += &format!(" (first at {diff} of {} / {})", out.len(), ff.zone.len());
            }
            Ok((same, line))
        })();
        match done {
            Ok((same, line)) => {
                out(&format!("{line}\n"));
                failed += usize::from(!same);
            }
            Err(e) => {
                out(&format!("{file}: {}\n", e.text()));
                failed += 1;
            }
        }
    }
    u8::from(failed != 0)
}

// The first difference between two zones, and how many bytes differ.
fn compare(ours: &[u8], theirs: &[u8]) {
    let mut first = None;
    let mut count = 0usize;
    for (k, (a, b)) in ours.iter().zip(theirs).enumerate() {
        if a != b {
            first.get_or_insert(k);
            count += 1;
        }
    }
    if ours.len() == theirs.len() && count == 0 {
        println!("the same bytes ({})", ours.len());
        return;
    }
    println!(
        "DIFFERENT: {} bytes against {}, {count} differ, the first at {}",
        ours.len(),
        theirs.len(),
        first.map_or(usize::MAX, |f| f)
    );
    if let Some(first) = first {
        let from = first & !15;
        for (label, z) in [("ours  ", ours), ("theirs", theirs)] {
            let bytes: String = z.iter().skip(from).take(32).map(|b| format!(" {b:02x}")).collect();
            println!("  {label} {from:08x}:{bytes}");
        }
    }
}

fn read_any(st: &mut Store, file: &str, lenient: bool) -> Result<(Fastfile, Tree, &'static Schema, &'static Platform)> {
    let ff = fastfile::read_fastfile(Path::new(file))?;
    let (rules, plat) = platform(&ff)?;
    let (tree, _) = reader::read_tree(st, rules, plat, ff.zone.clone(), lenient)?;
    Ok((ff, tree, rules, plat))
}

fn list(file: &str) -> Result<u8> {
    let mut st = Store::new();
    let (_, tree, _, _) = read_any(&mut st, file, file.contains("common_mp"))?;
    for (k, &e) in tree.assets.iter().enumerate() {
        let (ty, asset) = {
            let e = st.entry(e);
            (e.ty.clone(), e.asset)
        };
        let a = asset.filter(|&a| st.is_struct(Some(a)));
        let kind = a.map_or("(alias)", |a| st.st(a).asset);
        let name = a.map(|a| query::name(&mut st, a).unwrap_or_else(|| "-".to_string())).unwrap_or_default();
        out(&format!("{k:4} {ty:<14} {kind:<22} {name}\n"));
    }
    Ok(0)
}

fn canonical(file: &str, expected: &str) -> Result<u8> {
    let mut st = Store::new();
    let (_, tree, rules, plat) = read_any(&mut st, file, false)?;
    let out = reader::write_tree(&mut st, &tree, rules, plat, &WriteOptions::default(), None)?.zone;
    let want = read_all(expected)?;
    compare(&out, &want);
    Ok(u8::from(out != want))
}

fn write_all(path: &str, data: &[u8]) {
    let _ = std::fs::write(path, data);
}

// The picture of that name in an .iwd, as the converter reads it.
fn find_picture(iwd: &str, picture: &str) -> Result<Option<Picture>> {
    let zip = Zip::open(Path::new(iwd))?;
    let want = format!("images/{picture}.iwi").to_ascii_lowercase();
    for (k, name) in zip.names().iter().enumerate() {
        if name.replace('\\', "/").to_ascii_lowercase() == want {
            return images::read_iwi(&zip.read(k)?).map(Some);
        }
    }
    Ok(None)
}

fn fit(iwd: &str, picture: &str, limit: i32, out: &str) -> Result<u8> {
    let Some(pic) = find_picture(iwd, picture)? else {
        eprintln!("{iwd} has no picture {picture}");
        return Ok(1);
    };
    let fit = images::fit_picture(pic.format, pic.width, pic.height, &pic.mips, limit)?;
    write_all(out, &fit.mips[0]);
    println!("{picture}: {}x{} -> {}x{}, {} bytes", pic.width, pic.height, fit.width, fit.height, fit.mips[0].len());
    Ok(0)
}

fn iwi(iwd: &str, picture: &str, out: &str) -> Result<u8> {
    let Some(pic) = find_picture(iwd, picture)? else {
        eprintln!("{iwd} has no picture {picture}");
        return Ok(1);
    };
    let all: Vec<u8> = pic.mips.concat();
    write_all(out, &all);
    println!(
        "{picture}: {} {}x{}, {} levels, {} bytes",
        images::format_of(pic.format).name,
        pic.width,
        pic.height,
        pic.mips.len(),
        all.len()
    );
    Ok(0)
}

fn encode_sound(wav: &str, out: &str) -> Result<u8> {
    let raw = read_all(wav)?;
    let sound = xma::read_sound(&raw, None);
    let Some(pcm) = sound.pcm else {
        eprintln!("{wav}: {}", if sound.compressed { "not 16-bit PCM" } else { "no audio" });
        return Ok(1);
    };
    let x = xma::encode_xma(&pcm.samples, pcm.rate, pcm.channels);
    write_all(out, &x.data);
    let seek: String = x.seek.iter().map(|s| format!(" {s}")).collect();
    println!("{} samples {} Hz, {} ms, end bit {}, seek{seek}", x.samples, x.rate, x.ms(), x.end_bit);
    Ok(0)
}

fn port(pc: &str, out_dir: &str, stock_dir: &str, game_dir: Option<&str>) -> Result<u8> {
    let t0 = Instant::now();
    let mut stock: Vec<PathBuf> = std::fs::read_dir(stock_dir)
        .map_err(|_| Error::new(format!("cannot read {stock_dir}")))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ff"))
        .collect();
    stock.sort();
    let timed = std::env::var_os("MAP_PORT_TIMES").is_some();
    let mut options = PortOptions::default();
    if let Some(game) = game_dir {
        options.game_iwds = port::game_iwds(Path::new(game));
    }
    let log: Log = Rc::new(move |line: &str| {
        if timed {
            out(&format!("{:6.1} ", t0.elapsed().as_secs_f64()));
        }
        out(&format!("{line}\n"));
        Ok(())
    });
    let written = port::port_map(&PcMap::beside(Path::new(pc)), Path::new(out_dir), &stock, &log, &options)?;
    out(&format!("WRITTEN {} files in {:.1} s\n", written.written.len(), t0.elapsed().as_secs_f64()));
    Ok(0)
}

fn atoi(s: &str) -> i32 {
    // As C's atoi: leading blanks, a sign, digits; nothing else read.
    let s = s.trim_start();
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        v = (v * 10 + i64::from(b - b'0')).min(i64::from(u32::MAX) + 1);
    }
    (if negative { -v } else { v }) as i32
}

fn run(args: &[String]) -> Result<Option<u8>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(Some(match a.as_slice() {
        [_, "roundtrip", files @ ..] if !files.is_empty() => roundtrip(files),
        [_, "canonical", file, zone] => canonical(file, zone)?,
        [_, "list", file] => list(file)?,
        [_, "fit", iwd, picture, limit, out] => fit(iwd, picture, atoi(limit), out)?,
        [_, "iwi", iwd, picture, out] => iwi(iwd, picture, out)?,
        [_, "xma", wav, out] => encode_sound(wav, out)?,
        [_, "port", pc, out, stock] => port(pc, out, stock, None)?,
        [_, "port", pc, out, stock, game] => port(pc, out, stock, Some(game))?,
        _ => return Ok(None),
    }))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(Some(code)) => return ExitCode::from(code),
        Ok(None) => {}
        Err(e) => {
            let mut line = text::bytes(e.text());
            line.push(b'\n');
            let _ = std::io::stderr().write_all(&line);
            return ExitCode::from(1);
        }
    }
    eprint!(
        "usage: map-port roundtrip <file.ff>...\n       map-port canonical <file.ff> <zone>\n       map-port fit <file.iwd> <picture> <limit> <out>\n       map-port list <file.ff>\n       map-port iwi <file.iwd> <picture> <out>\n       map-port xma <file.wav> <out>\n       map-port port <map.ff> <out dir> <stock dir> [PC game folder]\n"
    );
    ExitCode::from(2)
}
