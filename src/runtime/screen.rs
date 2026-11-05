// The screens the console's system draws over a title -- signing in, picking
// friends, the guide -- drawn here into a picture the presenter lays over the middle
// of the frame. They share the launcher's fonts and look.
//
// One is open at a time, as on the console: a screen asked for while another is open
// stays shut. The title hears XN_SYS_UI when the first opens and when the last shuts,
// not at each.

use crate::env;
use crate::runtime::kernel;
use crate::runtime::truetype::{self, Glyph};
use std::sync::{Mutex, MutexGuard};

/// BGRA, opaque. The pixels are only rewritten when `version` is not theirs.
#[derive(Default)]
pub struct Image {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub version: u64,
    /// the frame's width in pixels, which the window rounds the corners of
    pub outline: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    None,
    Signin,
    Friends,
    Guide,
}

const XN_SYS_UI: u32 = 0x0000_0009;
static OPEN: Mutex<Which> = Mutex::new(Which::None);

fn open() -> MutexGuard<'static, Which> {
    OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Opens `which`: false while another is open. True, with nothing posted, when it is
/// the one open.
pub fn claim(which: Which) -> bool {
    let mut open = open();
    if *open == which {
        return true;
    }
    if *open != Which::None {
        return false;
    }
    *open = which;
    kernel::xam::post_notification(XN_SYS_UI, 1);
    true
}

/// Shuts it, if it is the one open.
pub fn release(which: Which) {
    let mut open = open();
    if *open != which {
        return;
    }
    *open = Which::None;
    kernel::xam::post_notification(XN_SYS_UI, 0);
}

/// The screen open becomes `to` with no notification in between: the guide giving
/// way to the friends screen it opens.
pub fn pass(from: Which, to: Which) {
    let mut open = open();
    if *open == from {
        *open = to;
    }
}

#[derive(Clone, Copy)]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub const WHITE: Colour = Colour { r: 236, g: 236, b: 232 };
pub const DIM: Colour = Colour { r: 150, g: 151, b: 150 };
pub const AMBER: Colour = Colour { r: 232, g: 178, b: 52 };
pub const BACK: Colour = Colour { r: 22, g: 24, b: 28 };
pub const BAR: Colour = Colour { r: 0, g: 0, b: 0 };
pub const EDGE: Colour = Colour { r: 70, g: 73, b: 78 };

#[derive(Clone, Copy)]
pub enum Font {
    Heading,
    Entry,
    Small,
}

// The atlas starts at 512 texels a side and doubles until every glyph fits: a tall
// window's letters do not fit the smallest.
const FIRST_ATLAS: usize = 512;
const LARGEST_ATLAS: usize = 8192;

struct Baked {
    atlas: Vec<u8>,
    side: usize,
    glyphs: [Glyph; 95],
    size: f32,
    ascent: f32,
}

// The launcher's fonts (third_party/fonts/), in the order of Font.
const FONTS_DATA: [&[u8]; 3] = [
    include_bytes!("../../third_party/fonts/BarlowCondensed-Medium.ttf"),
    include_bytes!("../../third_party/fonts/BarlowCondensed-Regular.ttf"),
    include_bytes!("../../third_party/fonts/BarlowSemiCondensed-Regular.ttf"),
];

// Each the size of the last sheet drawn; the presenter draws one at a time.
static FONTS: Mutex<[Option<Baked>; 3]> = Mutex::new([None, None, None]);

fn baked<R>(which: Font, unit: f32, read: impl FnOnce(&Baked) -> R) -> R {
    const SIZES: [f32; 3] = [38.0, 28.0, 18.0];
    let size = (SIZES[which as usize] * unit).round();
    let mut fonts = FONTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let font = &mut fonts[which as usize];
    if font.as_ref().is_none_or(|font| font.size != size) {
        let mut side = FIRST_ATLAS;
        *font = Some(loop {
            let mut baked =
                Baked { atlas: vec![0; side * side], side, glyphs: [Glyph::default(); 95], size, ascent: 0.0 };
            let (ascent, fitted) =
                truetype::bake(FONTS_DATA[which as usize], size, &mut baked.atlas, side, &mut baked.glyphs);
            baked.ascent = ascent;
            if fitted || side >= LARGEST_ATLAS {
                break baked;
            }
            side *= 2;
        });
    }
    read(font.as_ref().unwrap())
}

/// stb_truetype's stbtt_GetPackedQuad, aligned to whole pixels: where the glyph's
/// box goes, its first texel in the atlas, and the pen moved on.
struct Quad {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    s0: f32,
    t0: f32,
}

fn packed_quad(glyph: &Glyph, side: usize, x: &mut f32, y: f32) -> Quad {
    let texel = 1.0f32 / side as f32;
    let left = ((*x + glyph.xoff) + 0.5).floor();
    let top = ((y + glyph.yoff) + 0.5).floor();
    let quad = Quad {
        x0: left,
        y0: top,
        x1: left + glyph.xoff2 - glyph.xoff,
        y1: top + glyph.yoff2 - glyph.yoff,
        s0: f32::from(glyph.x0) * texel,
        t0: f32::from(glyph.y0) * texel,
    };
    *x += glyph.xadvance;
    quad
}

fn glyph_index(c: u8) -> usize {
    usize::from(if (32..=126).contains(&c) { c } else { b'?' }) - 32
}

/// A sheet of `sheet_width` by `sheet_height` units, drawn `height` pixels tall;
/// everything below is placed in units.
pub struct Sheet<'a> {
    image: &'a mut Image,
    unit: f32,
}

impl<'a> Sheet<'a> {
    pub fn new(image: &'a mut Image, height: u32, sheet_width: f32, sheet_height: f32, version: u64) -> Self {
        let unit = height as f32 / sheet_height;
        image.height = height;
        image.width = (sheet_width * unit).round() as i64 as u32 & !1;
        image.version = version;
        image.outline = 2.0 * unit;
        image.pixels.clear();
        image.pixels.resize(image.width as usize * image.height as usize * 4, 0);
        Self { image, unit }
    }

    pub fn at(&self, units: f32) -> i32 {
        (units * self.unit).round() as i64 as i32
    }

    fn pixel(&mut self, x: i32, y: i32) -> &mut [u8] {
        let at = (y as usize * self.image.width as usize + x as usize) * 4;
        &mut self.image.pixels[at..at + 4]
    }

    pub fn fill(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, colour: Colour) {
        let (x0, y0) = (self.at(x0).max(0), self.at(y0).max(0));
        let (x1, y1) = (self.at(x1).min(self.image.width as i32), self.at(y1).min(self.image.height as i32));
        for y in y0..y1 {
            for x in x0..x1 {
                self.pixel(x, y).copy_from_slice(&[colour.b, colour.g, colour.r, 255]);
            }
        }
    }

    /// Left-aligned at x, the line's top at y; returns where the text ends.
    pub fn text(&mut self, which: Font, ux: f32, uy: f32, colour: Colour, text: &[u8]) -> f32 {
        let (width, height) = (self.image.width as i32, self.image.height as i32);
        let mut x = self.at(ux) as f32;
        let pixels = &mut self.image.pixels;
        let at_y = (uy * self.unit).round() as i64 as i32;
        baked(which, self.unit, |font| {
            let pen_y = at_y as f32 + font.ascent;
            for &c in text {
                let quad = packed_quad(&font.glyphs[glyph_index(c)], font.side, &mut x, pen_y);
                let sx = (quad.s0 * font.side as f32 + 0.5) as i32;
                let sy = (quad.t0 * font.side as f32 + 0.5) as i32;
                let (w, h) = ((quad.x1 - quad.x0) as i32, (quad.y1 - quad.y0) as i32);
                for row in 0..h {
                    for column in 0..w {
                        let (px, py) = (quad.x0 as i32 + column, quad.y0 as i32 + row);
                        if px < 0 || py < 0 || px >= width || py >= height {
                            continue;
                        }
                        let alpha = u32::from(font.atlas[(sy + row) as usize * font.side + (sx + column) as usize]);
                        let at = (py as usize * width as usize + px as usize) * 4;
                        let p = &mut pixels[at..at + 3];
                        for (channel, value) in p.iter_mut().zip([colour.b, colour.g, colour.r]) {
                            *channel = ((u32::from(value) * alpha + u32::from(*channel) * (255 - alpha)) / 255) as u8;
                        }
                    }
                }
            }
        });
        x / self.unit
    }

    pub fn width(&mut self, which: Font, text: &[u8]) -> f32 {
        let mut x = 0.0;
        baked(which, self.unit, |font| {
            for &c in text {
                packed_quad(&font.glyphs[glyph_index(c)], font.side, &mut x, 0.0);
            }
        });
        x / self.unit
    }

    /// An RGBA square, scaled to `size` units.
    pub fn picture(&mut self, ux: f32, uy: f32, size: f32, rgba: &[u8], side: u32) {
        let side = side as usize;
        if side == 0 || rgba.len() < side * side * 4 {
            return;
        }
        let (x0, y0, pixels) = (self.at(ux), self.at(uy), self.at(size).max(1));
        for y in 0..pixels {
            for x in 0..pixels {
                let (px, py) = (x0 + x, y0 + y);
                if px < 0 || py < 0 || px >= self.image.width as i32 || py >= self.image.height as i32 {
                    continue;
                }
                let from = ((y * side as i32 / pixels) as usize * side + (x * side as i32 / pixels) as usize) * 4;
                let s = &rgba[from..from + 3];
                let (r, g, b) = (s[0], s[1], s[2]);
                self.pixel(px, py).copy_from_slice(&[b, g, r, 255]);
            }
        }
    }

    /// The outline, the dark inside and the heading every screen has.
    pub fn frame(&mut self, heading: &[u8]) {
        let (width, height) = (self.image.width as f32 / self.unit, self.image.height as f32 / self.unit);
        self.fill(0.0, 0.0, width, height, EDGE);
        self.fill(2.0, 2.0, width - 2.0, height - 2.0, BACK);
        self.text(Font::Heading, 28.0, 20.0, WHITE, heading);
    }

    /// "A  Select      B  Back" along the bottom.
    pub fn buttons(&mut self, y: f32, buttons: &[(&str, &str)]) {
        let mut x = 28.0;
        for (button, does) in buttons {
            x = self.text(Font::Small, x, y, AMBER, button.as_bytes());
            x = self.text(Font::Small, x, y, DIM, format!("  {does}      ").as_bytes());
        }
    }
}

/// MW2_DUMP_FRAMES keeps the screens too, which the frames do not show.
pub fn dump(name: &str, image: &Image) {
    if !cfg!(feature = "diagnostics") {
        return;
    }
    let Some(folder) = env::text(c"MW2_DUMP_FRAMES") else { return };
    let path = format!("{}/{name}_{}.ppm", folder.to_string_lossy(), image.version);
    let mut bytes = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
    for pixel in image.pixels.chunks_exact(4) {
        bytes.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let _ = std::fs::write(path, bytes);
}
