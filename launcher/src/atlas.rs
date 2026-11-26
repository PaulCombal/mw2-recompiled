// The launcher's fonts, rasterised once into one picture the window's text is drawn from
// (draw.rs), as Dear ImGui 1.91.9b built its atlas, which drew the launcher's window
// before: each font's glyphs from U+0020 to U+00FF, three samples a pixel across, packed
// with stb_rect_pack's skyline, beside the pixels every shape takes its colour from and
// the bands its smoothed lines are drawn with. Every glyph is where Dear ImGui put it,
// so the window is drawn to the same pixels; tools/draw_diff.py holds the two together.
//
// stb_rect_pack is Sean Barrett's, in the public domain (or, at the user's choice, under
// the MIT licence), and taken here under the former; Dear ImGui is Copyright (c) 2014-2025
// Omar Cornut, under the MIT licence, whose text is in launcher/NOTICE.md.
#![deny(unsafe_code)]

use mw2_truetype::{Face, Malformed};

/// Where `a` is in the atlas and how it is drawn: its box from the pen, in pixels of a
/// font at its own size, its corners in the picture, and how far it moves the pen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub codepoint: u32,
    pub visible: bool,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub advance: f32,
}

/// One font at one size.
pub struct Font {
    /// The size it was rasterised at: its line's height, in pixels.
    pub size: f32,
    // Dear ImGui's font has them; the window draws without, and tools/draw_diff.rs holds
    // them to Dear ImGui's.
    #[allow(dead_code)]
    pub ascent: f32,
    #[allow(dead_code)]
    pub descent: f32,
    glyphs: Vec<Glyph>,
    // By codepoint: the glyph's place in `glyphs` (u16::MAX for none) and its advance.
    lookup: Vec<u16>,
    advances: Vec<f32>,
    fallback: usize,
    fallback_advance: f32,
}

impl Font {
    /// The glyph drawn for `c`: its own, or the font's '?' when it has none.
    pub fn glyph(&self, c: u32) -> &Glyph {
        match self.lookup.get(c as usize) {
            Some(&i) if i != u16::MAX => &self.glyphs[usize::from(i)],
            _ => &self.glyphs[self.fallback],
        }
    }

    /// How far `c` moves the pen at the font's own size.
    pub fn advance(&self, c: u32) -> f32 {
        self.advances.get(c as usize).copied().unwrap_or(self.fallback_advance)
    }
}

/// A font to rasterise: its file and its size in pixels.
pub struct Source<'a> {
    pub data: &'a [u8],
    pub size: f32,
}

/// The fonts in one picture of `width` × `height` coverage bytes.
pub struct Atlas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub fonts: Vec<Font>,
    /// The middle of a white pixel: what a shape of one colour samples.
    pub white: [f32; 2],
    /// For a smoothed line `n` pixels thick (0 to LINES_MOST): the left and right ends of
    /// its band and the band's middle row, (u0, v, u1, v).
    pub lines: [[f32; 4]; LINES_MOST + 1],
}

pub const LINES_MOST: usize = 32;

const PADDING: i32 = 1;
const FIRST: u32 = 0x20;
const LAST: u32 = 0xFF;
// The room Dear ImGui kept for its mouse cursors and its white pixels (two pictures of
// 122 × 27 and a column between), and its bands for lines. Only the white pixels are
// drawn here, but the room is kept so that the glyphs are packed around it as they were.
const CURSORS: (i32, i32) = (122 * 2 + 1, 27);
const LINES: (i32, i32) = (LINES_MOST as i32 + 2, LINES_MOST as i32 + 1);
const TALLEST: i32 = 1024 * 32;

impl Atlas {
    /// The fonts, sampled `oversample` times a pixel across; a font that cannot be read
    /// fails them all.
    pub fn build(sources: &[Source], oversample: i32) -> Result<Atlas, Malformed> {
        struct Pending<'a> {
            face: Face<'a>,
            size: f32,
            codepoints: Vec<u32>,
            glyphs: Vec<i32>,
            rects: Vec<Rect>,
        }
        let over = (oversample, 1);
        let mut pending = Vec::with_capacity(sources.len());
        let mut surface: i32 = 0;
        for source in sources {
            let face = Face::new(source.data)?;
            // Dear ImGui rounds a font's size down to whole pixels.
            let size = source.size.trunc();
            if size <= 0.0 {
                return Err(Malformed);
            }
            let (mut codepoints, mut glyphs) = (Vec::new(), Vec::new());
            for codepoint in FIRST..=LAST {
                let glyph = face.glyph_index(codepoint as i32)?;
                if glyph != 0 {
                    codepoints.push(codepoint);
                    glyphs.push(glyph);
                }
            }
            // Dear ImGui asserts a font has a glyph; one without any here is refused.
            if glyphs.is_empty() {
                return Err(Malformed);
            }
            let scale = face.scale_for_pixel_height(size)?;
            let mut rects = Vec::with_capacity(glyphs.len());
            for &glyph in &glyphs {
                let [x0, y0, x1, y1] = face.bitmap_box(glyph, scale * over.0 as f32, scale * over.1 as f32)?;
                let rect = Rect::sized(x1 - x0 + PADDING + over.0 - 1, y1 - y0 + PADDING + over.1 - 1);
                surface = surface.wrapping_add(rect.w.wrapping_mul(rect.h));
                rects.push(rect);
            }
            pending.push(Pending { face, size, codepoints, glyphs, rects });
        }
        let mut custom =
            [Rect::sized(CURSORS.0 + PADDING, CURSORS.1 + PADDING), Rect::sized(LINES.0 + PADDING, LINES.1 + PADDING)];
        for rect in &custom {
            surface = surface.wrapping_add(rect.w * rect.h);
        }

        // A width for the packer from the area the glyphs need.
        let side = (surface as f32).sqrt() as i32 + 1;
        let width: i32 = if side as f32 >= 4096.0 * 0.7 {
            4096
        } else if side as f32 >= 2048.0 * 0.7 {
            2048
        } else if side as f32 >= 1024.0 * 0.7 {
            1024
        } else {
            512
        };
        let mut packer = Packer::new(width, TALLEST);
        let mut height = 0;
        packer.pack(&mut custom);
        for rect in &custom {
            if !rect.packed {
                return Err(Malformed);
            }
            height = height.max(rect.y + rect.h);
        }
        for font in &mut pending {
            packer.pack(&mut font.rects);
            for rect in &font.rects {
                if !rect.packed {
                    return Err(Malformed);
                }
                height = height.max(rect.y + rect.h);
            }
        }
        let height = (height as u32).next_power_of_two() as i32;
        let (width_scale, height_scale) = (1.0 / width as f32, 1.0 / height as f32);
        let mut pixels = vec![0u8; width as usize * height as usize];

        let mut fonts = Vec::with_capacity(pending.len());
        for font in &pending {
            let mut glyphs = Vec::with_capacity(font.glyphs.len() + 1);
            let scale = font.face.scale_for_pixel_height(font.size)?;
            let (ascent, descent) = font.face.v_metrics()?;
            let ascent = (ascent as f32 * scale).ceil();
            let descent = (descent as f32 * scale).floor();
            let top = 0.0 + (ascent + 0.5) as i32 as f32;
            for ((&codepoint, &glyph), rect) in font.codepoints.iter().zip(&font.glyphs).zip(&font.rects) {
                let packed = font.face.render_packed(
                    &mut pixels,
                    width,
                    [rect.x, rect.y, rect.w, rect.h],
                    PADDING,
                    font.size,
                    over,
                    glyph,
                )?;
                // stbtt_GetPackedQuad from a pen at 0, 0.
                let (mut x0, y0) = ((0.0 + packed.xoff) * 1.0 + 0.0, (0.0 + packed.yoff) * 1.0 + top);
                let (mut x1, y1) = ((0.0 + packed.xoff2) * 1.0 + 0.0, (0.0 + packed.yoff2) * 1.0 + top);
                let (u0, v0) = (f32::from(packed.x0) * width_scale, f32::from(packed.y0) * height_scale);
                let (u1, v1) = (f32::from(packed.x1) * width_scale, f32::from(packed.y1) * height_scale);
                // A glyph that would move the pen back moves it nowhere, centred on where it was.
                let mut advance = packed.xadvance * 1.0;
                if advance < 0.0 {
                    let off = (0.0 - advance) * 0.5;
                    x0 += off;
                    x1 += off;
                    advance = 0.0;
                }
                advance += 0.0;
                glyphs.push(Glyph {
                    codepoint,
                    visible: x0 != x1 && y0 != y1,
                    x0,
                    y0,
                    x1,
                    y1,
                    u0,
                    v0,
                    u1,
                    v1,
                    advance,
                });
            }
            fonts.push(Font::finish(font.size, ascent, descent, glyphs));
        }

        // The white pixels: a square of four, its first sampled at its middle.
        let cursors = custom[0];
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            pixels[(cursors.x + dx) as usize + (cursors.y + dy) as usize * width as usize] = 0xFF;
        }
        let white = [(cursors.x as f32 + 0.5) * width_scale, (cursors.y as f32 + 0.5) * height_scale];

        // The bands: row n has n solid pixels in the middle, clear ones either side.
        let band = custom[1];
        let mut lines = [[0.0; 4]; LINES_MOST + 1];
        for (n, line) in lines.iter_mut().enumerate() {
            let n = n as i32;
            let left = (LINES.0 - n) / 2;
            let row = (band.y + n) as usize * width as usize;
            for i in 0..LINES.0 {
                pixels[row + (band.x + i) as usize] = if i >= left && i < left + n { 0xFF } else { 0x00 };
            }
            let (u0, v0) = ((band.x + left - 1) as f32 * width_scale, (band.y + n) as f32 * height_scale);
            let (u1, v1) = ((band.x + left + n + 1) as f32 * width_scale, (band.y + n + 1) as f32 * height_scale);
            let middle = (v0 + v1) * 0.5;
            *line = [u0, middle, u1, middle];
        }

        Ok(Atlas { width: width as usize, height: height as usize, pixels, fonts, white, lines })
    }

    /// The picture as white RGBA pixels with its coverage as their alpha.
    pub fn rgba(&self) -> Vec<u8> {
        self.pixels.iter().flat_map(|&a| [0xFF, 0xFF, 0xFF, a]).collect()
    }
}

impl Font {
    // ImFont::BuildLookupTable: the index by codepoint, a tab four spaces wide, spaces and
    // tabs not drawn, and '?' for what the font does not have (U+FFFD, the first choice,
    // is past what was rasterised).
    fn finish(size: f32, ascent: f32, descent: f32, mut glyphs: Vec<Glyph>) -> Font {
        let highest = glyphs.iter().map(|g| g.codepoint).max().unwrap_or(0) as usize;
        let mut lookup = vec![u16::MAX; highest + 1];
        let mut advances = vec![-1.0f32; highest + 1];
        for (i, glyph) in glyphs.iter().enumerate() {
            lookup[glyph.codepoint as usize] = i as u16;
            advances[glyph.codepoint as usize] = glyph.advance;
        }
        let space = lookup.get(usize::from(b' ')).copied().filter(|&i| i != u16::MAX);
        if let Some(space) = space {
            let mut tab = glyphs[usize::from(space)];
            tab.codepoint = u32::from(b'\t');
            tab.advance *= 4.0;
            glyphs.push(tab);
            if lookup.len() <= usize::from(b'\t') {
                lookup.resize(usize::from(b'\t') + 1, u16::MAX);
                advances.resize(usize::from(b'\t') + 1, -1.0);
            }
            lookup[usize::from(b'\t')] = (glyphs.len() - 1) as u16;
            advances[usize::from(b'\t')] = tab.advance;
            glyphs[usize::from(space)].visible = false;
            glyphs.last_mut().unwrap().visible = false;
        }
        let found = |c: u32| lookup.get(c as usize).copied().filter(|&i| i != u16::MAX).map(usize::from);
        let fallback =
            [0xFFFD, u32::from(b'?'), u32::from(b' ')].into_iter().find_map(found).unwrap_or(glyphs.len() - 1);
        let fallback_advance = glyphs[fallback].advance;
        for advance in &mut advances {
            if *advance < 0.0 {
                *advance = fallback_advance;
            }
        }
        Font { size, ascent, descent, glyphs, lookup, advances, fallback, fallback_advance }
    }
}

// ---------------------------------------------------------------------------------------
// stb_rect_pack: a skyline of the heights reached so far, each rectangle put where it
// rests lowest (bottom-left first), the tallest first.

#[derive(Clone, Copy, Default, Debug)]
struct Rect {
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    packed: bool,
}

impl Rect {
    fn sized(w: i32, h: i32) -> Rect {
        Rect { w, h, ..Rect::default() }
    }
}

#[derive(Clone, Copy)]
struct Node {
    x: i32,
    y: i32,
    next: Option<usize>,
}

// Where a list's link is: its head, or a node's next.
#[derive(Clone, Copy)]
enum Link {
    Head,
    Next(usize),
}

struct Packer {
    width: i32,
    height: i32,
    nodes: Vec<Node>,
    free: Option<usize>,
    head: usize,
}

impl Packer {
    // stbrp_init_target with a node for every column: the skyline is one node at 0 and
    // a sentinel at the right edge, and the rest are free.
    fn new(width: i32, height: i32) -> Packer {
        let count = width as usize;
        let mut nodes: Vec<Node> =
            (0..count).map(|i| Node { x: 0, y: 0, next: (i + 1 < count).then_some(i + 1) }).collect();
        nodes.push(Node { x: 0, y: 0, next: Some(count + 1) });
        nodes.push(Node { x: width, y: 1 << 30, next: None });
        Packer { width, height, nodes, free: (count > 0).then_some(0), head: count }
    }

    fn get(&self, link: Link) -> usize {
        match link {
            Link::Head => self.head,
            Link::Next(node) => self.nodes[node].next.unwrap(),
        }
    }

    fn set(&mut self, link: Link, to: usize) {
        match link {
            Link::Head => self.head = to,
            Link::Next(node) => self.nodes[node].next = Some(to),
        }
    }

    fn next(&self, node: usize) -> usize {
        self.nodes[node].next.unwrap()
    }

    // stbrp__skyline_find_min_y: the lowest a rectangle `width` wide starting at x0 can
    // rest on the skyline from `first`.
    fn min_y(&self, first: usize, x0: i32, width: i32) -> i32 {
        let x1 = x0 + width;
        let (mut node, mut min_y, mut visited) = (first, 0, 0);
        while self.nodes[node].x < x1 {
            let next = self.next(node);
            if self.nodes[node].y > min_y {
                min_y = self.nodes[node].y;
                visited += if self.nodes[node].x < x0 {
                    self.nodes[next].x - x0
                } else {
                    self.nodes[next].x - self.nodes[node].x
                };
            } else {
                let under = (self.nodes[next].x - self.nodes[node].x).min(width - visited);
                visited += under;
            }
            node = next;
        }
        min_y
    }

    // stbrp__skyline_pack_rectangle with the bottom-left heuristic.
    fn place(&mut self, width: i32, height: i32) -> Option<(i32, i32)> {
        if width > self.width || height > self.height {
            return None;
        }
        let (mut best, mut best_y) = (None, 1 << 30);
        let (mut node, mut prev) = (self.head, Link::Head);
        while self.nodes[node].x + width <= self.width {
            let y = self.min_y(node, self.nodes[node].x, width);
            if y < best_y {
                best_y = y;
                best = Some(prev);
            }
            prev = Link::Next(node);
            node = self.next(node);
        }
        let best = best?;
        let best_x = self.nodes[self.get(best)].x;
        if best_y + height > self.height {
            return None;
        }
        let made = self.free?;
        self.free = self.nodes[made].next;
        self.nodes[made].x = best_x;
        self.nodes[made].y = best_y + height;
        // The new node goes in at the best place; the nodes it covers go back to the free list.
        let mut cur = self.get(best);
        if self.nodes[cur].x < best_x {
            let next = self.next(cur);
            self.nodes[cur].next = Some(made);
            cur = next;
        } else {
            self.set(best, made);
        }
        while let Some(next) = self.nodes[cur].next
            && self.nodes[next].x <= best_x + width
        {
            self.nodes[cur].next = self.free;
            self.free = Some(cur);
            cur = next;
        }
        self.nodes[made].next = Some(cur);
        if self.nodes[cur].x < best_x + width {
            self.nodes[cur].x = best_x + width;
        }
        Some((best_x, best_y))
    }

    // stbrp_pack_rects: the tallest first, then the widest, rectangles of the same size in
    // the order given (C's qsort leaves that open; glibc's keeps it).
    fn pack(&mut self, rects: &mut [Rect]) {
        let mut order: Vec<usize> = (0..rects.len()).collect();
        order.sort_by(|&a, &b| rects[b].h.cmp(&rects[a].h).then(rects[b].w.cmp(&rects[a].w)));
        for i in order {
            let rect = &mut rects[i];
            if rect.w == 0 || rect.h == 0 {
                (rect.x, rect.y, rect.packed) = (0, 0, true);
            } else if let Some((x, y)) = self.place(rect.w, rect.h) {
                (rect.x, rect.y, rect.packed) = (x, y, true);
            } else {
                rect.packed = false;
            }
        }
    }
}
