// tools/font_diff.py's program: src/runtime/truetype.rs against stb_truetype as
// the C++ build's screens call it (font_diff.cpp, linked in), byte for byte: the atlas,
// the 95 glyphs, the result and the ascent's bits.
//
//   usage: font_diff sweep <jobs> <threads>
//          font_diff fuzz <font> <first seed> <seeds> <child> <work> <threads>
//          font_diff one <font> <size> <side>
//          font_diff time <font> <size> <side> <repeats>
//
// sweep runs a list of lines "<font>\t<size>\t<sides>", sides being "loop" for screen.rs's
// doubling from 512 to 8192 or a comma-separated list. fuzz damages the font's glyf, loca,
// cmap, hmtx and CFF tables per seed and runs each copy through the C in font_diff_child
// and through the port in `font_diff one`, each in its own process: the C may read outside
// the font, and then only the port's not panicking is asked for. Exits nonzero at the first
// difference, saying where, and leaves the font beside the work folder.

#![allow(dead_code)]

#[path = "../src/runtime/truetype.rs"]
mod truetype;

use std::ffi::c_int;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use truetype::Glyph;

unsafe extern "C" {
    fn font_diff_c_bake(
        data: *const u8,
        size: f32,
        atlas: *mut u8,
        side: c_int,
        glyphs: *mut u8,
        ascent: *mut f32,
    ) -> c_int;
    fn font_diff_c_init(data: *const u8) -> c_int;
    fn font_diff_guarded(font: *const u8, size: usize) -> *const u8;
    fn font_diff_free_guarded(font: *const u8, size: usize);
}

const GLYPH_BYTES: usize = 28;

fn glyph_bytes(glyph: &Glyph) -> [u8; GLYPH_BYTES] {
    let mut out = [0u8; GLYPH_BYTES];
    for (i, v) in [glyph.x0, glyph.y0, glyph.x1, glyph.y1].iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in [glyph.xoff, glyph.yoff, glyph.xadvance, glyph.xoff2, glyph.yoff2].iter().enumerate() {
        out[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_bits().to_le_bytes());
    }
    out
}

// What a bake gives, in font_diff_child's layout: whether the font was taken, then the
// result, the ascent, the glyphs and the atlas.
fn encode(taken: bool, fitted: bool, ascent: f32, glyphs: &[u8], atlas: &[u8]) -> Vec<u8> {
    let mut out = i32::from(taken).to_le_bytes().to_vec();
    if taken {
        out.extend(i32::from(fitted).to_le_bytes());
        out.extend(ascent.to_bits().to_le_bytes());
        out.extend(glyphs);
        out.extend(atlas);
    }
    out
}

fn c_bake(font: &[u8], size: f32, side: usize) -> Vec<u8> {
    // Against a guard page: the sweep faults where the C would read past the font.
    let data = unsafe { font_diff_guarded(font.as_ptr(), font.len()) };
    let out = if unsafe { font_diff_c_init(data) } == 0 {
        encode(false, false, 0.0, &[], &[])
    } else {
        let mut atlas = vec![0u8; side * side];
        let mut glyphs = [0u8; GLYPH_BYTES * 95];
        let mut ascent = 0.0f32;
        let fitted = unsafe {
            font_diff_c_bake(data, size, atlas.as_mut_ptr(), side as c_int, glyphs.as_mut_ptr(), &mut ascent)
        } != 0;
        encode(true, fitted, ascent, &glyphs, &atlas)
    };
    unsafe { font_diff_free_guarded(data, font.len()) };
    out
}

fn rust_bake(font: &[u8], size: f32, side: usize) -> Vec<u8> {
    let mut atlas = vec![0u8; side * side];
    let mut glyphs = [Glyph::default(); 95];
    match truetype::bake_checked(font, size, &mut atlas, side, &mut glyphs) {
        Ok((ascent, fitted)) => {
            let bytes: Vec<u8> = glyphs.iter().flat_map(glyph_bytes).collect();
            encode(true, fitted, ascent, &bytes, &atlas)
        }
        Err(_) => encode(false, false, 0.0, &[], &[]),
    }
}

// The first difference between two encoded bakes, in words.
fn difference(c: &[u8], rust: &[u8], side: usize) -> Option<String> {
    if c == rust {
        return None;
    }
    let word = |b: &[u8], at: usize| i32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    if word(c, 0) != word(rust, 0) {
        return Some(format!(
            "the C {} the font, the port {}",
            if word(c, 0) != 0 { "takes" } else { "refuses" },
            if word(rust, 0) != 0 { "takes it" } else { "refuses it" }
        ));
    }
    if word(c, 4) != word(rust, 4) {
        return Some(format!("fitted: the C {}, the port {}", word(c, 4), word(rust, 4)));
    }
    if word(c, 8) != word(rust, 8) {
        let f = |b: &[u8]| f32::from_bits(word(b, 8) as u32);
        return Some(format!("ascent: the C {:?}, the port {:?}", f(c), f(rust)));
    }
    let names = ["x0", "y0", "x1", "y1", "xoff", "yoff", "xadvance", "xoff2", "yoff2"];
    for g in 0..95 {
        let at = 12 + g * GLYPH_BYTES;
        for (k, name) in names.iter().enumerate() {
            let (from, to) =
                if k < 4 { (at + k * 2, at + k * 2 + 2) } else { (at + 8 + (k - 4) * 4, at + 12 + (k - 4) * 4) };
            if c[from..to] != rust[from..to] {
                let show = |b: &[u8]| {
                    if k < 4 {
                        format!("{}", u16::from_le_bytes(b[from..to].try_into().unwrap()))
                    } else {
                        format!("{:?}", f32::from_bits(u32::from_le_bytes(b[from..to].try_into().unwrap())))
                    }
                };
                return Some(format!(
                    "glyph '{}' {name}: the C {}, the port {}",
                    (32 + g as u8) as char,
                    show(c),
                    show(rust)
                ));
            }
        }
    }
    let atlas = 12 + 95 * GLYPH_BYTES;
    let (ca, ra) = (&c[atlas..], &rust[atlas..]);
    let count = ca.iter().zip(ra).filter(|(a, b)| a != b).count();
    let first = ca.iter().zip(ra).position(|(a, b)| a != b).unwrap_or(0);
    let (x, y) = (first % side, first / side);
    let u16_at = |at: usize| usize::from(u16::from_le_bytes(c[at..at + 2].try_into().unwrap()));
    let owner = (0..95)
        .find(|g| {
            let at = 12 + g * GLYPH_BYTES;
            (u16_at(at)..u16_at(at + 4)).contains(&x) && (u16_at(at + 2)..u16_at(at + 6)).contains(&y)
        })
        .map_or("no glyph's box".to_string(), |g| format!("glyph '{}'", (32 + g as u8) as char));
    Some(format!(
        "atlas: {count} texels differ, the first at x {x} y {y}, in {owner}: the C {}, the port {}",
        ca.get(first).copied().unwrap_or(0),
        ra.get(first).copied().unwrap_or(0)
    ))
}

fn sweep(jobs: &str, threads: usize) {
    let lines: Vec<(String, f32, String)> = std::fs::read_to_string(jobs)
        .expect("the jobs")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0].to_string(), f[1].parse().expect("a size"), f[2].to_string())
        })
        .collect();
    let fonts: Mutex<std::collections::HashMap<String, std::sync::Arc<Vec<u8>>>> = Mutex::default();
    let next = AtomicUsize::new(0);
    let (cases, fitted, refused) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let failed = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= lines.len() || failed.load(Ordering::Relaxed) {
                        return;
                    }
                    let (path, size, sides) = &lines[index];
                    let font = fonts
                        .lock()
                        .unwrap()
                        .entry(path.clone())
                        .or_insert_with(|| std::sync::Arc::new(std::fs::read(path).expect("a font")))
                        .clone();
                    let check = |side: usize| -> bool {
                        let (c, rust) = (c_bake(&font, *size, side), rust_bake(&font, *size, side));
                        cases.fetch_add(1, Ordering::Relaxed);
                        if let Some(what) = difference(&c, &rust, side) {
                            println!("DIFFERENT: {path} at size {size:?} on a {side} atlas: {what}");
                            failed.store(true, Ordering::Relaxed);
                        }
                        if c[0] == 0 {
                            refused.fetch_add(1, Ordering::Relaxed);
                            return true;
                        }
                        let fit = c[4] != 0;
                        fitted.fetch_add(u64::from(fit), Ordering::Relaxed);
                        fit
                    };
                    if sides == "refuse" {
                        // where the C is undefined or runs out of stack: the port must refuse
                        cases.fetch_add(1, Ordering::Relaxed);
                        if rust_bake(&font, *size, 512)[0] != 0 {
                            println!("DIFFERENT: {path} at size {size:?}: the port takes a font it must refuse");
                            failed.store(true, Ordering::Relaxed);
                        }
                        refused.fetch_add(1, Ordering::Relaxed);
                    } else if sides == "loop" {
                        let mut side = 512;
                        while !check(side) && side < 8192 && !failed.load(Ordering::Relaxed) {
                            side *= 2;
                        }
                    } else {
                        for side in sides.split(',') {
                            check(side.parse().expect("a side"));
                        }
                    }
                }
            });
        }
    });
    if failed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
    println!(
        "all the same: {} jobs, {} bakes, {} fitted, {} refused by both",
        lines.len(),
        cases.load(Ordering::Relaxed),
        fitted.load(Ordering::Relaxed),
        refused.load(Ordering::Relaxed)
    );
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// Where the tables the damage goes into are: (tag, offset, length).
fn tables(font: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let be16 = |at: usize| u16::from_be_bytes(font[at..at + 2].try_into().unwrap()) as usize;
    let be32 = |at: usize| u32::from_be_bytes(font[at..at + 4].try_into().unwrap()) as usize;
    let start = if &font[..4] == b"ttcf" { be32(12) } else { 0 };
    let mut found = Vec::new();
    for i in 0..be16(start + 4) {
        let entry = start + 12 + 16 * i;
        let tag: [u8; 4] = font[entry..entry + 4].try_into().unwrap();
        let (at, length) = (be32(entry + 8), be32(entry + 12));
        if matches!(&tag, b"glyf" | b"loca" | b"cmap" | b"hmtx" | b"CFF ") && at + length <= font.len() {
            found.push((tag, at, length));
        }
    }
    found
}

// A damaged copy: bytes flipped, set to an extreme or to a random value, mostly in glyf,
// some in loca, cmap (most in its first bytes and its subtables' headers), hmtx and CFF.
fn damage(random: &mut Random, font: &[u8], tables: &[([u8; 4], usize, usize)]) -> Vec<u8> {
    let mut bytes = font.to_vec();
    for _ in 0..1 + random.below(4) {
        let weights: Vec<usize> = tables
            .iter()
            .map(|(tag, _, _)| match tag {
                b"glyf" | b"CFF " => 6,
                b"cmap" => 3,
                _ => 2,
            })
            .collect();
        let mut pick = random.below(weights.iter().sum());
        let mut table = 0;
        while pick >= weights[table] {
            pick -= weights[table];
            table += 1;
        }
        let (_, start, length) = tables[table];
        if length == 0 {
            continue;
        }
        let at = start
            + match random.below(3) {
                0 => random.below(length.min(64)),
                _ => random.below(length),
            };
        bytes[at] = match random.below(5) {
            0 | 1 => bytes[at] ^ 1 << random.below(8),
            2 => [0x00, 0xFF, 0x7F, 0x80, 0x01][random.below(5)],
            3 => bytes[at].wrapping_add(1),
            _ => random.next() as u8,
        };
    }
    bytes
}

// A child's exit and output, or None when it ran out of time.
fn run(command: &mut Command, limit: Duration) -> Option<(Option<i32>, Vec<u8>, String)> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).ok();
        out
    });
    let errors = std::thread::spawn(move || {
        let mut out = String::new();
        stderr.read_to_string(&mut out).ok();
        out
    });
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some((status.code(), reader.join().unwrap(), errors.join().unwrap()));
        }
        if start.elapsed() > limit {
            child.kill().ok();
            child.wait().ok();
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn fuzz(font_path: &str, first: u64, seeds: u64, child: &str, work: &str, threads: usize) {
    let font = std::fs::read(font_path).expect("the font");
    let tables = tables(&font);
    let me = std::env::current_exe().unwrap();
    let next = AtomicU64::new(first);
    let failed = AtomicBool::new(false);
    let counts: Mutex<[u64; 6]> = Mutex::default(); // same, taken, refused by both, C faulted, port refused those, C slow
    let taken_on_fault: Mutex<Vec<(u64, String)>> = Mutex::default();
    std::thread::scope(|scope| {
        for worker in 0..threads {
            let (font, tables, me, next, failed, counts) = (&font, &tables, &me, &next, &failed, &counts);
            let taken_on_fault = &taken_on_fault;
            scope.spawn(move || {
                let path = format!("{work}/fuzz{worker}.ttf");
                loop {
                    let seed = next.fetch_add(1, Ordering::Relaxed);
                    if seed >= first + seeds || failed.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut random = Random(seed.wrapping_mul(1_000_003) ^ 0x5EED);
                    let damaged = damage(&mut random, font, tables);
                    let size = [6.0f32, 13.0, 18.0, 28.0, 38.0, 57.0, 24.5][random.below(7)];
                    let side = 512;
                    std::fs::write(&path, &damaged).unwrap();
                    let (size_arg, side_arg) = (format!("{size}"), format!("{side}"));
                    let limit = Duration::from_secs(30);
                    let c = run(Command::new(child).args([&path, &size_arg, &side_arg]), limit);
                    let rust = run(Command::new(me).args(["one", &path, &size_arg, &side_arg]), limit);
                    let fail = |what: String| {
                        let kept = format!("{work}/different-{seed}.ttf");
                        std::fs::write(&kept, &damaged).unwrap();
                        println!("DIFFERENT: {font_path} seed {seed} at size {size} on a {side} atlas: {what}; written to {kept}");
                        failed.store(true, Ordering::Relaxed);
                    };
                    let Some((rust_code, rust_out, rust_err)) = rust else {
                        fail("the port did not finish in 30 s".into());
                        return;
                    };
                    if rust_code != Some(0) {
                        fail(format!("the port exited {rust_code:?}: {}", rust_err.trim()));
                        return;
                    }
                    let mut counts = counts.lock().unwrap();
                    match c {
                        Some((Some(0), c_out, _)) => {
                            if let Some(what) = difference(&c_out, &rust_out, side) {
                                drop(counts);
                                fail(what);
                                return;
                            }
                            counts[0] += 1;
                            counts[if c_out[0] != 0 { 1 } else { 2 }] += 1;
                        }
                        Some((_, _, c_err)) => {
                            counts[3] += 1;
                            counts[4] += u64::from(rust_out[0] == 0);
                            if rust_out[0] != 0 {
                                // what the C did that it should not have, for the report
                                let why = c_err
                                    .lines()
                                    .find(|l| l.contains("runtime error") || l.contains("ERROR") || l.contains("differ"))
                                    .unwrap_or("")
                                    .to_string();
                                taken_on_fault.lock().unwrap().push((seed, why));
                            }
                        }
                        None => counts[5] += 1,
                    }
                }
            });
        }
    });
    if failed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
    let c = counts.lock().unwrap();
    println!(
        "fuzz {font_path}: {seeds} seeds, {} the same ({} taken, {} refused by both); the C read outside the font or \
         crashed on {}, of which the port refused {} and took {}; the C ran out of time on {}",
        c[0],
        c[1],
        c[2],
        c[3],
        c[4],
        c[3] - c[4],
        c[5]
    );
    let mut taken = taken_on_fault.into_inner().unwrap();
    taken.sort();
    for (seed, why) in taken.iter().take(8) {
        println!("  the port took seed {seed}, where the C: {}", why.trim());
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    match arguments.get(1).map(String::as_str) {
        Some("sweep") => sweep(&arguments[2], arguments[3].parse().unwrap()),
        Some("fuzz") => fuzz(
            &arguments[2],
            arguments[3].parse().unwrap(),
            arguments[4].parse().unwrap(),
            &arguments[5],
            &arguments[6],
            arguments[7].parse().unwrap(),
        ),
        Some("one") => {
            let font = std::fs::read(&arguments[2]).expect("the font");
            let out = rust_bake(&font, arguments[3].parse().unwrap(), arguments[4].parse().unwrap());
            use std::io::Write;
            std::io::stdout().write_all(&out).unwrap();
        }
        Some("time") => {
            let font = std::fs::read(&arguments[2]).expect("the font");
            let (size, side): (f32, usize) = (arguments[3].parse().unwrap(), arguments[4].parse().unwrap());
            let repeats: u32 = arguments[5].parse().unwrap();
            let mut timings = Vec::new();
            for (name, bake) in [("C", c_bake as fn(&[u8], f32, usize) -> Vec<u8>), ("port", rust_bake)] {
                let mut best = Duration::MAX;
                for _ in 0..repeats {
                    let start = Instant::now();
                    std::hint::black_box(bake(&font, size, side));
                    best = best.min(start.elapsed());
                }
                timings.push(format!("{name} {:.2} ms", best.as_secs_f64() * 1000.0));
            }
            println!("size {size} on {side}: best of {repeats}: {}", timings.join(", "));
        }
        _ => {
            eprintln!("usage: font_diff sweep|fuzz|one|time ...");
            std::process::exit(2);
        }
    }
}
