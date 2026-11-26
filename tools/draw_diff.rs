// tools/draw_diff.py's program: the launcher's drawing in Rust (launcher/src/atlas.rs,
// draw.rs) beside Dear ImGui 1.91.9b (draw_diff.cpp, linked in), in one process, given
// the same fonts and the same random calls:
//
//   draw_diff <seed> <frames> <oversample> <font> <size> [<font> <size>...]
//
// The atlases are compared byte for byte but for the room Dear ImGui keeps for its mouse
// cursors, which the launcher never draws from (only its white pixels are compared there);
// every font's metrics and, for every codepoint to U+FFFF, the glyph drawn and its advance;
// the size measured of each text; and each frame's triangles, every index's clip, picture
// and vertex, bit for bit but that any NaN is any other. Then frames drawn by SDL's software renderer, through Dear
// ImGui's SDL renderer backend and through present.rs, with pictures of SDL's, compared
// pixel for pixel. Exits 1 at the first difference, saying where.

#![allow(dead_code)]

extern crate mw2_truetype;
extern crate sdl3_sys;

#[path = "../launcher/src/atlas.rs"]
mod atlas;
#[path = "../launcher/src/draw.rs"]
mod draw;
#[path = "../launcher/src/present.rs"]
mod present;

use atlas::{Atlas, Source};
use draw::{DrawList, Point, Texture, point};
use sdl3_sys::everything as sdl;
use std::ffi::{c_char, c_int, c_uint};
use std::process::exit;

unsafe extern "C" {
    fn cpp_init(
        count: c_int,
        data: *const *const u8,
        sizes: *const c_int,
        pixels: *const f32,
        oversample: c_int,
    ) -> c_int;
    fn cpp_atlas(
        width: *mut c_int,
        height: *mut c_int,
        white: *mut f32,
        lines: *mut f32,
        cursors: *mut c_int,
    ) -> *const u8;
    fn cpp_font(font: c_int, metrics: *mut f32);
    fn cpp_glyph(font: c_int, c: c_uint, out: *mut f32) -> f32;
    fn cpp_begin(width: f32, height: f32);
    fn cpp_end(size: *mut usize) -> *const u8;
    fn cpp_push_clip(min: *const f32, max: *const f32);
    fn cpp_pop_clip();
    fn cpp_line(a: *const f32, b: *const f32, col: u32, thickness: f32);
    fn cpp_rect(a: *const f32, b: *const f32, col: u32, rounding: f32, thickness: f32);
    fn cpp_rect_filled(a: *const f32, b: *const f32, col: u32, rounding: f32);
    fn cpp_multi(a: *const f32, b: *const f32, cols: *const u32);
    fn cpp_quad(p: *const f32, col: u32);
    fn cpp_circle(c: *const f32, radius: f32, col: u32);
    fn cpp_image(texture: u64, a: *const f32, b: *const f32, uv0: *const f32, uv1: *const f32, col: u32);
    fn cpp_stroke(p: *const f32, count: c_int, col: u32, closed: c_int, thickness: f32);
    fn cpp_poly(p: *const f32, count: c_int, col: u32);
    fn cpp_text(font: c_int, size: f32, at: *const f32, col: u32, text: *const c_char, one: c_int, wrap: f32);
    fn cpp_backend_init(renderer: *mut sdl::SDL_Renderer) -> u64;
    fn cpp_render(renderer: *mut sdl::SDL_Renderer);
    fn cpp_text_size(
        font: c_int,
        size: f32,
        max_width: f32,
        wrap: f32,
        text: *const c_char,
        one: c_int,
        out: *mut f32,
    ) -> c_int;
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
    fn float(&mut self, low: f32, high: f32) -> f32 {
        low + (self.next() >> 40) as f32 / (1u64 << 24) as f32 * (high - low)
    }
    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len())]
    }
    // A coordinate: mostly on the window, on whole and half pixels or anywhere, now and
    // then off it.
    fn coordinate(&mut self, extent: f32) -> f32 {
        if self.chance(400) {
            return self.pick(&[3.0e9, -3.0e9, 1.0e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, -0.0]);
        }
        match self.below(6) {
            0 => self.below(extent as usize + 1) as f32,
            1 => self.below(extent as usize + 1) as f32 + 0.5,
            2 => self.float(-200.0, extent + 200.0),
            _ => self.float(0.0, extent),
        }
    }
    fn point(&mut self, size: Point) -> Point {
        point(self.coordinate(size.x), self.coordinate(size.y))
    }
    fn colour(&mut self) -> u32 {
        let c = self.next() as u32;
        match self.below(8) {
            0 => c & 0x00FF_FFFF,
            1 => c | 0xFF00_0000,
            _ => c,
        }
    }
    fn thickness(&mut self) -> f32 {
        let choices = [1.0, 1.5, 2.0, 0.5, 0.0, 1.1, 1.65, 3.0, 4.4, 31.0, 32.0, 33.0, 63.0, f32::NAN];
        if self.chance(3) { self.float(0.0, 40.0) } else { self.pick(&choices) }
    }
    fn rounding(&mut self) -> f32 {
        let choices = [0.0, 0.3, 0.49, 0.5, 0.50000006, 1.0, 2.2, 3.0000002, 3.3, 6.6, 10.000001, 1000.0, f32::NAN];
        if self.chance(3) { self.float(0.0, 40.0) } else { self.pick(&choices) }
    }
}

// Pieces texts are made of: the launcher's own, Latin-1, characters past the atlas,
// malformed UTF-8, the controls the layout acts on, and blanks.
const PIECES: &[&[u8]] = &[
    b"INSTALL THE GAME",
    b"PLAY",
    b"3 OF 4",
    b"UNOFFICIAL. NOT AFFILIATED WITH ACTIVISION, INFINITY WARD OR MICROSOFT.",
    b"Dropping a disc image on the window installs from it.",
    b"MW2 RECOMPILED",
    b"word",
    b"w",
    b"Caf\xC3\xA9",
    b"Gr\xC3\xB6\xC3\x9Fe",
    b"\xC3\x91and\xC3\xBA",
    b"na\xC3\xAFve \xC2\xA9 \xC2\xBF?",
    b"\xC3\xBF",
    b"\xE2\x82\xAC",
    b"\xE6\x97\xA5\xE6\x9C\xAC",
    b"\xF0\x9F\x98\x80",
    b"\xF0\x90\x81\x81",
    b"\xE3\x80\x80",
    b"\xEF\xBF\xBD",
    b"\x80",
    b"\xC3",
    b"\xC0\xAF",
    b"\xED\xA0\x80",
    b"\xF4\x90\x80\x80",
    b"\xE2\x82",
    b"\xFF\xFE",
    b"\t",
    b"\r",
    b"\n",
    b"\n\n",
    b"\r\n",
    b" ",
    b"   ",
    b".",
    b",",
    b";",
    b"!",
    b"?",
    b"\"",
    b"...",
    b"\x01\x1F",
    b"\x00",
];

fn text(random: &mut Random) -> Vec<u8> {
    let mut text = Vec::new();
    if random.chance(40) {
        // Long: past the 10000 bytes after which a text without wrapping is cut to what is seen.
        let lines = random.chance(2);
        while text.len() < 10000 + random.below(4000) {
            text.extend_from_slice(random.pick(PIECES));
            if lines && random.chance(8) {
                text.push(b'\n');
            }
        }
        return text;
    }
    for _ in 0..random.below(14) {
        if random.chance(10) {
            text.push(random.next() as u8);
        } else {
            text.extend_from_slice(random.pick(PIECES));
        }
    }
    text
}

// One byte of a text, as ui.cpp's Spaced() drew a text a byte at a time, never its end.
fn one_byte(text: &mut Vec<u8>) {
    text.truncate(1);
    if text.first().is_none_or(|&c| c == 0) {
        *text = vec![b'A'];
    }
}

fn c_string(text: &[u8]) -> Vec<u8> {
    let mut c = text.to_vec();
    c.push(0);
    c
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|v| v.to_bits()).collect()
}

// Two triangles' corners the same: every float bit for bit, but that any NaN is any other
// (which of two NaNs an operation keeps follows its operands' order, which the compiler
// may swap, and a corner that is not a number draws nothing either way), and the picture
// and colour exactly.
fn same_record(a: &[u8], b: &[u8]) -> bool {
    let float = |r: &[u8], at: usize| f32::from_ne_bytes(r[at..at + 4].try_into().unwrap());
    [0, 4, 8, 12, 24, 28, 32, 36].iter().all(|&at| {
        let (x, y) = (float(a, at), float(b, at));
        x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
    }) && a[16..24] == b[16..24]
        && a[40..44] == b[40..44]
}

fn fail(what: String) -> ! {
    println!("DIFFERENT: {what}");
    exit(1);
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.len() < 6 || arguments.len() % 2 != 0 {
        eprintln!("usage: draw_diff <seed> <frames> <oversample> <font> <size> [<font> <size>...]");
        exit(2);
    }
    let seed: u64 = arguments[1].parse().unwrap();
    let frames: usize = arguments[2].parse().unwrap();
    let oversample: i32 = arguments[3].parse().unwrap();
    let files: Vec<Vec<u8>> = arguments[4..].chunks(2).map(|pair| std::fs::read(&pair[0]).unwrap()).collect();
    let sizes: Vec<f32> = arguments[4..].chunks(2).map(|pair| pair[1].parse().unwrap()).collect();

    // ---- the atlas
    let pointers: Vec<*const u8> = files.iter().map(|f| f.as_ptr()).collect();
    let lengths: Vec<c_int> = files.iter().map(|f| f.len() as c_int).collect();
    // SAFETY: the fonts outlive the C++'s atlas, which does not own them.
    if unsafe { cpp_init(files.len() as c_int, pointers.as_ptr(), lengths.as_ptr(), sizes.as_ptr(), oversample) } == 0 {
        fail("the C++ built no atlas".into());
    }
    let sources: Vec<Source> = files.iter().zip(&sizes).map(|(data, &size)| Source { data, size }).collect();
    let atlas = match Atlas::build(&sources, oversample) {
        Ok(atlas) => atlas,
        Err(_) => fail("the Rust built no atlas".into()),
    };
    let (mut width, mut height) = (0, 0);
    let mut white = [0f32; 2];
    let mut lines = [0f32; 33 * 4];
    let mut cursors = [0 as c_int; 4];
    // SAFETY: the C++ fills what it is given, and its pixels live until the context ends.
    let pixels = unsafe {
        let p = cpp_atlas(&mut width, &mut height, white.as_mut_ptr(), lines.as_mut_ptr(), cursors.as_mut_ptr());
        std::slice::from_raw_parts(p, width as usize * height as usize)
    };
    if (width as usize, height as usize) != (atlas.width, atlas.height) {
        fail(format!("atlas size: C++ {width}x{height}, Rust {}x{}", atlas.width, atlas.height));
    }
    if bits(&white) != bits(&atlas.white) {
        fail(format!("white pixel: C++ {white:?}, Rust {:?}", atlas.white));
    }
    let rust_lines: Vec<f32> = atlas.lines.iter().flatten().copied().collect();
    if bits(&lines) != bits(&rust_lines) {
        fail("the lines' bands".into());
    }
    let [cx, cy, cw, ch] = cursors.map(|v| v as usize);
    for y in 0..atlas.height {
        for x in 0..atlas.width {
            let in_cursors = x >= cx && x < cx + cw && y >= cy && y < cy + ch;
            let white_pixel = x >= cx && x < cx + 2 && y >= cy && y < cy + 2;
            let at = y * atlas.width + x;
            if (!in_cursors || white_pixel) && pixels[at] != atlas.pixels[at] {
                fail(format!("atlas pixel {x},{y}: C++ {}, Rust {}", pixels[at], atlas.pixels[at]));
            }
        }
    }

    // ---- the fonts' glyphs
    for (index, font) in atlas.fonts.iter().enumerate() {
        let mut metrics = [0f32; 3];
        // SAFETY: the font exists; the C++ fills three floats.
        unsafe { cpp_font(index as c_int, metrics.as_mut_ptr()) };
        if bits(&metrics) != bits(&[font.size, font.ascent, font.descent]) {
            fail(format!("font {index} metrics: C++ {metrics:?}, Rust {:?}", [font.size, font.ascent, font.descent]));
        }
        for c in 0..=0xFFFFu32 {
            let mut out = [0f32; 11];
            // SAFETY: as above, eleven floats.
            let advance = unsafe { cpp_glyph(index as c_int, c, out.as_mut_ptr()) };
            let g = font.glyph(c);
            let ours = [
                g.codepoint as f32,
                if g.visible { 1.0 } else { 0.0 },
                g.x0,
                g.y0,
                g.x1,
                g.y1,
                g.u0,
                g.v0,
                g.u1,
                g.v1,
                g.advance,
            ];
            if bits(&out) != bits(&ours) || advance.to_bits() != font.advance(c).to_bits() {
                fail(format!("font {index} U+{c:04X}: C++ {out:?} {advance}, Rust {ours:?} {}", font.advance(c)));
            }
        }
    }

    // ---- frames of random shapes and text, compared as triangles
    let mut random = Random(seed);
    // The font atlas is 1, among the pictures, so that an image of it is drawn without a change of picture.
    let images: Vec<(u64, Texture)> = (1..=5).map(|id| (id, Texture(id as usize))).collect();
    let mut list = DrawList::new(&atlas, images[0].1, point(1280.0, 720.0));
    let mut triangles = 0usize;
    for frame in 0..frames {
        let size = match random.below(4) {
            0 => point(1280.0, 720.0),
            1 => point(random.pick(&[640.0, 1920.0, 2560.0, 3840.0]), random.pick(&[360.0, 1080.0, 1440.0, 2160.0])),
            _ => point(random.float(1.0, 4000.0).round(), random.float(1.0, 2400.0).round()),
        };
        // SAFETY: a frame of the C++'s context.
        unsafe { cpp_begin(size.x, size.y) };
        list.clear(images[0].1, size);
        let log = draw_frame(&mut random, &atlas, &mut list, size, &images);
        let mut length = 0;
        // SAFETY: the C++'s buffer lives until its next frame.
        let theirs = unsafe { std::slice::from_raw_parts(cpp_end(&mut length), length) };
        let mut ours = Vec::with_capacity(theirs.len());
        for command in &list.commands {
            for &index in &list.indices[command.first..command.first + command.count] {
                let v = list.vertices[index as usize];
                for f in command.clip {
                    ours.extend_from_slice(&f.to_ne_bytes());
                }
                ours.extend_from_slice(&(command.texture.0 as u64).to_ne_bytes());
                for f in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
                    ours.extend_from_slice(&f.to_ne_bytes());
                }
                ours.extend_from_slice(&v.col.to_ne_bytes());
            }
        }
        if ours.len() != theirs.len() || ours.chunks(44).zip(theirs.chunks(44)).any(|(a, b)| !same_record(a, b)) {
            let at = ours
                .chunks(44)
                .zip(theirs.chunks(44))
                .position(|(a, b)| !same_record(a, b))
                .unwrap_or(ours.len().min(theirs.len()) / 44);
            let show = |r: &[u8]| -> String {
                if r.len() < 44 {
                    return "<none>".into();
                }
                let f = |o: usize| f32::from_ne_bytes(r[o..o + 4].try_into().unwrap());
                format!(
                    "clip {:?} texture {} pos {},{} uv {},{} col {:08x}",
                    [f(0), f(4), f(8), f(12)],
                    u64::from_ne_bytes(r[16..24].try_into().unwrap()),
                    f(24),
                    f(28),
                    f(32),
                    f(36),
                    u32::from_ne_bytes(r[40..44].try_into().unwrap())
                )
            };
            let record = |all: &[u8]| show(all.get(at * 44..(at + 1) * 44).unwrap_or(&[]));
            println!("calls of frame {frame} (window {size:?}):");
            for line in &log {
                println!("  {line}");
            }
            fail(format!(
                "frame {frame}, index {at} of {} (C++) and {} (Rust):\n  C++:  {}\n  Rust: {}",
                theirs.len() / 44,
                ours.len() / 44,
                record(theirs),
                record(&ours)
            ));
        }
        triangles += ours.len() / 44 / 3;
    }
    let drawn = draw_pixels(&mut random, &atlas, frames / 8);
    println!(
        "same: atlas {}x{}, {} fonts, {frames} frames, {triangles} triangles, {drawn} frames drawn by SDL",
        atlas.width,
        atlas.height,
        atlas.fonts.len()
    );
}

// One frame's random calls, made on both sides: `images` are the pictures an image can
// be, as each side names it. What was called, for a report.
fn draw_frame(
    random: &mut Random,
    atlas: &Atlas,
    list: &mut DrawList,
    size: Point,
    images: &[(u64, Texture)],
) -> Vec<String> {
    let mut log = Vec::new();
    let mut clips = 0;
    for _ in 0..random.below(60) {
        let col = random.colour();
        let (a, b) = (random.point(size), random.point(size));
        let (pa, pb) = ([a.x, a.y], [b.x, b.y]);
        // SAFETY (each C++ call): the pointers are to locals that outlive the call.
        match random.below(18) {
            0 if clips < 4 => {
                log.push(format!("push_clip {a:?} {b:?}"));
                unsafe { cpp_push_clip(pa.as_ptr(), pb.as_ptr()) };
                list.push_clip(a, b);
                clips += 1;
            }
            1 if clips > 0 => {
                log.push("pop_clip".into());
                unsafe { cpp_pop_clip() };
                list.pop_clip();
                clips -= 1;
            }
            2 => {
                let thickness = random.thickness();
                log.push(format!("line {a:?} {b:?} {col:08x} {thickness}"));
                unsafe { cpp_line(pa.as_ptr(), pb.as_ptr(), col, thickness) };
                list.add_line(a, b, col, thickness);
            }
            3 => {
                let (rounding, thickness) = (random.rounding(), random.thickness());
                log.push(format!("rect {a:?} {b:?} {col:08x} {rounding} {thickness}"));
                unsafe { cpp_rect(pa.as_ptr(), pb.as_ptr(), col, rounding, thickness) };
                list.add_rect(a, b, col, rounding, thickness);
            }
            4 => {
                let rounding = random.rounding();
                log.push(format!("rect_filled {a:?} {b:?} {col:08x} {rounding}"));
                unsafe { cpp_rect_filled(pa.as_ptr(), pb.as_ptr(), col, rounding) };
                list.add_rect_filled(a, b, col, rounding);
            }
            5 => {
                let cols = [col, random.colour(), random.colour(), random.colour()];
                log.push(format!("multi {a:?} {b:?} {cols:08x?}"));
                unsafe { cpp_multi(pa.as_ptr(), pb.as_ptr(), cols.as_ptr()) };
                list.add_rect_filled_multi_color(a, b, cols[0], cols[1], cols[2], cols[3]);
            }
            6 => {
                let p = [a, b, random.point(size), random.point(size)];
                let flat: Vec<f32> = p.iter().flat_map(|p| [p.x, p.y]).collect();
                log.push(format!("quad {p:?} {col:08x}"));
                unsafe { cpp_quad(flat.as_ptr(), col) };
                list.add_quad_filled(p[0], p[1], p[2], p[3], col);
            }
            7 => {
                let radius = if random.chance(3) {
                    random.pick(&[
                        0.0,
                        0.4,
                        0.5,
                        0.50000006,
                        0.9,
                        1.0,
                        3.0000002,
                        10.000001,
                        62.9,
                        63.0,
                        63.000004,
                        63.5,
                        64.0,
                        f32::NAN,
                        f32::INFINITY,
                        200.0,
                        5000.0,
                    ])
                } else {
                    random.float(0.0, 120.0)
                };
                log.push(format!("circle {a:?} {radius} {col:08x}"));
                unsafe { cpp_circle(pa.as_ptr(), radius, col) };
                list.add_circle_filled(a, radius, col);
            }
            8 => {
                let texture = random.below(images.len());
                let (uv0, uv1) = (
                    [random.float(-1.0, 2.0), random.float(-1.0, 2.0)],
                    [random.float(-1.0, 2.0), random.float(-1.0, 2.0)],
                );
                log.push(format!("image {texture} {a:?} {b:?} {uv0:?} {uv1:?} {col:08x}"));
                unsafe { cpp_image(images[texture].0, pa.as_ptr(), pb.as_ptr(), uv0.as_ptr(), uv1.as_ptr(), col) };
                list.add_image(images[texture].1, a, b, point(uv0[0], uv0[1]), point(uv1[0], uv1[1]), col);
            }
            9 | 10 => {
                let count = 1 + random.below(10);
                let mut p: Vec<Point> = (0..count).map(|_| random.point(size)).collect();
                if count > 2 && random.chance(4) {
                    p[1] = p[0]; // a segment of no length
                }
                let flat: Vec<f32> = p.iter().flat_map(|p| [p.x, p.y]).collect();
                let thickness = random.thickness();
                if random.chance(2) {
                    let closed = random.chance(2);
                    log.push(format!("stroke {p:?} {col:08x} {closed} {thickness}"));
                    unsafe { cpp_stroke(flat.as_ptr(), count as c_int, col, c_int::from(closed), thickness) };
                    for &q in &p {
                        list.path_line_to(q);
                    }
                    list.path_stroke(col, closed, thickness);
                } else {
                    log.push(format!("poly {p:?} {col:08x}"));
                    unsafe { cpp_poly(flat.as_ptr(), count as c_int, col) };
                    list.add_convex_poly_filled(&p, col);
                }
            }
            11..=13 => {
                let index = random.below(atlas.fonts.len());
                let font = &atlas.fonts[index];
                let font_size = if random.chance(2) { font.size } else { random.float(1.0, 120.0) };
                let wrap = match random.below(4) {
                    0 | 1 => 0.0,
                    2 => random.float(-5.0, 30.0),
                    _ => random.float(30.0, 900.0),
                };
                let one = random.chance(6);
                let mut words = text(random);
                if one {
                    one_byte(&mut words);
                }
                let mut at = a;
                if random.chance(5) {
                    // Above the clip, so that the lines before it are skipped.
                    at.y = -random.float(0.0, 3000.0);
                }
                log.push(format!(
                    "text {index} {font_size} {at:?} {col:08x} {:?} one={one} wrap={wrap}",
                    String::from_utf8_lossy(&words)
                ));
                let c = c_string(&words);
                let pat = [at.x, at.y];
                unsafe {
                    cpp_text(index as c_int, font_size, pat.as_ptr(), col, c.as_ptr().cast(), c_int::from(one), wrap)
                };
                list.add_text(font, font_size, at, col, &words, wrap);
            }
            16 => {
                // A long text of lines without wrapping, one of them starting on the
                // clip's bottom edge, where Dear ImGui stops.
                let index = random.below(atlas.fonts.len());
                let font = &atlas.fonts[index];
                let mut words = Vec::new();
                while words.len() < 10001 + random.below(3000) {
                    words.extend_from_slice(random.pick(&PIECES[..25]));
                    if random.chance(6) {
                        words.push(b'\n');
                    }
                }
                let bottom = if clips == 0 { size.y } else { random.float(0.0, size.y).round() };
                let at = point(random.coordinate(size.x), bottom - font.size * random.below(80) as f32);
                log.push(format!("long text {index} {at:?} {col:08x} ({} bytes)", words.len()));
                let c = c_string(&words);
                let pat = [at.x, at.y];
                unsafe { cpp_text(index as c_int, font.size, pat.as_ptr(), col, c.as_ptr().cast(), 0, 0.0) };
                list.add_text(font, font.size, at, col, &words, 0.0);
            }
            17 => {
                // A glyph whose left edge is on the clip's right, or its right on the clip's left.
                let index = random.below(atlas.fonts.len());
                let font = &atlas.fonts[index];
                let font_size = if random.chance(2) { font.size } else { random.float(1.0, 120.0) };
                let c = 0x21 + random.below(0x5E) as u8;
                let glyph = font.glyph(u32::from(c));
                let scale = font_size / font.size;
                let x = (100 + random.below((size.x as usize).saturating_sub(200).max(1))) as f32;
                let y = random.coordinate(size.y).trunc();
                let (min, max) = if random.chance(2) {
                    (point(0.0, 0.0), point(x + glyph.x0 * scale, size.y))
                } else {
                    (point(x + glyph.x1 * scale, 0.0), size)
                };
                log.push(format!("edge text {index} {font_size} {:?} at {x},{y} clip {min:?} {max:?}", c as char));
                let (pmin, pmax, pat, text) = ([min.x, min.y], [max.x, max.y], [x, y], [c, 0]);
                unsafe {
                    cpp_push_clip(pmin.as_ptr(), pmax.as_ptr());
                    cpp_text(index as c_int, font_size, pat.as_ptr(), col | 0xFF00_0000, text.as_ptr().cast(), 0, 0.0);
                    cpp_pop_clip();
                }
                list.push_clip(min, max);
                list.add_text(font, font_size, point(x, y), col | 0xFF00_0000, &text[..1], 0.0);
                list.pop_clip();
            }
            _ => {
                let index = random.below(atlas.fonts.len());
                let font = &atlas.fonts[index];
                let font_size = if random.chance(2) { font.size } else { random.float(1.0, 120.0) };
                let max_width = if random.chance(2) { f32::MAX } else { random.float(0.0, 600.0) };
                let wrap = if random.chance(2) { 0.0 } else { random.float(-5.0, 700.0) };
                let one = random.chance(6);
                let mut words = text(random);
                if one {
                    one_byte(&mut words);
                }
                let c = c_string(&words);
                let mut out = [0f32; 2];
                let remaining = unsafe {
                    cpp_text_size(
                        index as c_int,
                        font_size,
                        max_width,
                        wrap,
                        c.as_ptr().cast(),
                        c_int::from(one),
                        out.as_mut_ptr(),
                    )
                };
                let (measured, ours) = font.text_size(font_size, max_width, wrap, &words);
                if bits(&out) != bits(&[measured.x, measured.y]) || remaining as usize != ours {
                    fail(format!(
                        "text size, font {index} size {font_size} max {max_width} wrap {wrap} {:?}: C++ {out:?} {remaining}, Rust {measured:?} {ours}",
                        String::from_utf8_lossy(&words)
                    ));
                }
            }
        }
    }
    log
}

// Frames drawn to pixels: one software renderer, both sides' font atlases as its pictures
// and four others both draw images of; each frame drawn by Dear ImGui's backend, read
// back, drawn by present.rs, read back, and compared. The atlases are not drawn as
// images, since the room for Dear ImGui's mouse cursors is empty in the Rust's. How many
// frames were.
fn draw_pixels(random: &mut Random, atlas: &Atlas, frames: usize) -> usize {
    const SIDE: Point = point(1600.0, 1000.0);
    // SAFETY: SDL's calls on a renderer and textures made here and alive throughout; the
    // pixels read are copied out of the surface SDL returns before it is freed.
    unsafe {
        let surface = sdl::SDL_CreateSurface(SIDE.x as c_int, SIDE.y as c_int, sdl::SDL_PIXELFORMAT_RGBA32);
        let renderer = if surface.is_null() { std::ptr::null_mut() } else { sdl::SDL_CreateSoftwareRenderer(surface) };
        if renderer.is_null() {
            fail("no software renderer".into());
        }
        cpp_backend_init(renderer);
        let Some(ours) = present::atlas_texture(renderer, atlas) else { fail("no atlas texture".into()) };
        let mut images = Vec::new();
        for (i, (w, h)) in [(1, 1), (7, 5), (64, 64), (33, 17)].into_iter().enumerate() {
            let picture =
                sdl::SDL_CreateTexture(renderer, sdl::SDL_PIXELFORMAT_RGBA32, sdl::SDL_TEXTUREACCESS_STATIC, w, h);
            let bytes: Vec<u8> = (0..w * h * 4).map(|_| random.next() as u8).collect();
            sdl::SDL_UpdateTexture(picture, std::ptr::null(), bytes.as_ptr().cast(), w * 4);
            sdl::SDL_SetTextureBlendMode(picture, sdl::SDL_BLENDMODE_BLEND);
            sdl::SDL_SetTextureScaleMode(
                picture,
                if i % 2 == 0 { sdl::SDL_SCALEMODE_LINEAR } else { sdl::SDL_SCALEMODE_NEAREST },
            );
            images.push((picture as u64, present::texture_of(picture)));
        }
        let mut list = DrawList::new(atlas, ours, SIDE);
        let mut colours = Vec::new();
        let read = || -> Vec<u8> {
            let shot = sdl::SDL_RenderReadPixels(renderer, std::ptr::null());
            if shot.is_null() {
                fail("SDL read no pixels".into());
            }
            let s = &*shot;
            let mut bytes = Vec::with_capacity((s.w * s.h * 4) as usize);
            for y in 0..s.h {
                let row =
                    std::slice::from_raw_parts(s.pixels.cast::<u8>().add((y * s.pitch) as usize), (s.w * 4) as usize);
                bytes.extend_from_slice(row);
            }
            sdl::SDL_DestroySurface(shot);
            bytes
        };
        for frame in 0..frames {
            let size = match random.below(3) {
                0 => point(1280.0, 720.0),
                1 => SIDE,
                _ => point(random.float(1.0, SIDE.x).round(), random.float(1.0, SIDE.y).round()),
            };
            cpp_begin(size.x, size.y);
            list.clear(ours, size);
            let log = draw_frame(random, atlas, &mut list, size, &images);
            let mut length = 0;
            cpp_end(&mut length);
            sdl::SDL_SetRenderDrawColor(renderer, 40, 40, 38, 255);
            sdl::SDL_RenderClear(renderer);
            cpp_render(renderer);
            let imgui = read();
            sdl::SDL_RenderClear(renderer);
            present::present(renderer, &list, size, point(1.0, 1.0), &mut colours);
            let rust = read();
            if imgui != rust {
                let at = imgui.iter().zip(&rust).position(|(a, b)| a != b).unwrap() / 4;
                let (x, y) = (at % SIDE.x as usize, at / SIDE.x as usize);
                let count = imgui.chunks(4).zip(rust.chunks(4)).filter(|(a, b)| a != b).count();
                println!("calls of frame {frame} drawn by SDL (window {size:?}):");
                for line in &log {
                    println!("  {line}");
                }
                fail(format!(
                    "pixels of frame {frame}: {count} differ, the first at {x},{y}: Dear ImGui's backend {:?}, present.rs {:?}",
                    &imgui[at * 4..at * 4 + 4],
                    &rust[at * 4..at * 4 + 4]
                ));
            }
        }
        frames
    }
}
