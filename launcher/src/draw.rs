// The window's shapes and text as triangles for SDL's renderer (present.rs draws them):
// lines, rectangles square and rounded, quadrilaterals, circles, gradients, pictures and
// text, each made as Dear ImGui 1.91.9b's draw list makes it -- every edge smoothed by a
// fringe a pixel wide that fades to clear, a line of whole pixels drawn from a band in the
// font atlas (atlas.rs), text set on whole pixels and wrapped where Dear ImGui wraps it --
// so that the window is drawn to the same pixels. Each vertex is made with the same float
// operations in the same order, down to the SSE reciprocal square root Dear ImGui
// normalises with on x86.
//
// Dear ImGui is Copyright (c) 2014-2025 Omar Cornut, under the MIT licence, whose text is
// in launcher/NOTICE.md.
#![deny(unsafe_code)]

use crate::atlas::{Atlas, Font, LINES_MOST};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

pub const fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

/// A colour as Dear ImGui's IM_COL32 packs one: red in the low byte, alpha in the high.
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (b as u32) << 16 | (g as u32) << 8 | r as u32
}

const ALPHA: u32 = 0xFF00_0000;

/// A picture the triangles sample: the font atlas's, or another the caller made. It is
/// the SDL texture's address, which only present.rs turns back into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture(pub usize);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Vertex {
    pub pos: Point,
    pub uv: Point,
    pub col: u32,
}

/// A run of triangles drawn with one picture inside one rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    pub clip: [f32; 4],
    pub texture: Texture,
    pub first: usize,
    pub count: usize,
}

pub struct DrawList {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub commands: Vec<Command>,
    white: Point,
    lines: [[f32; 4]; LINES_MOST + 1],
    clip: [f32; 4],
    clips: Vec<[f32; 4]>,
    texture: Texture,
    path: Vec<Point>,
    temp: Vec<Point>,
    // Points around a circle, as many as an arc can step through.
    arc: [Point; ARC_SAMPLES],
    // How many sides a circle of each whole radius below 64 has.
    circle_sides: [u8; 64],
}

const ARC_SAMPLES: usize = 48;
const CIRCLE_ERROR: f32 = 0.30;
const PI: f32 = std::f32::consts::PI;

// What C's (int) of a float is on x86: truncated, or i32::MIN when it does not fit (NaN
// too), where Rust's `as` saturates.
pub fn int(v: f32) -> i32 {
    if (-2_147_483_648.0..2_147_483_648.0).contains(&v) { v as i32 } else { i32::MIN }
}

// Dear ImGui's ImMax and ImMin, which differ from f32's max and min at NaN and at -0.
pub fn im_max(a: f32, b: f32) -> f32 {
    if a >= b { a } else { b }
}
pub fn im_min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

// IM_DRAWLIST_CIRCLE_AUTO_SEGMENT_CALC: sides enough for the circle's edge to stray from
// round by at most CIRCLE_ERROR pixels, an even number from 4 to 512.
fn circle_sides(radius: f32) -> i32 {
    let sides = int((PI / (1.0 - im_min(CIRCLE_ERROR, radius) / radius).acos()).ceil());
    (sides.wrapping_add(1) / 2).wrapping_mul(2).clamp(4, 512)
}

// ImRsqrt: on x86 the SSE estimate Dear ImGui uses there.
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
fn rsqrt(x: f32) -> f32 {
    use std::arch::x86_64::{_mm_cvtss_f32, _mm_rsqrt_ss, _mm_set_ss};
    // SAFETY: every x86-64 processor has SSE.
    unsafe { _mm_cvtss_f32(_mm_rsqrt_ss(_mm_set_ss(x))) }
}
#[cfg(not(target_arch = "x86_64"))]
fn rsqrt(x: f32) -> f32 {
    1.0 / x.sqrt()
}

// IM_NORMALIZE2F_OVER_ZERO
fn normalize(x: f32, y: f32) -> (f32, f32) {
    let d2 = x * x + y * y;
    if d2 > 0.0 {
        let inv = rsqrt(d2);
        (x * inv, y * inv)
    } else {
        (x, y)
    }
}

// IM_FIXNORMAL2F: the mean of two normals, lengthened so that the edge it offsets stays
// as far from both segments, at most tenfold.
fn fix_normal(x: f32, y: f32) -> (f32, f32) {
    let d2 = x * x + y * y;
    if d2 > 0.000001 {
        let mut inv = 1.0 / d2;
        if inv > 100.0 {
            inv = 100.0;
        }
        (x * inv, y * inv)
    } else {
        (x, y)
    }
}

impl DrawList {
    /// A list for a window of `size`, drawing with `atlas`, whose picture is `font`.
    pub fn new(atlas: &Atlas, font: Texture, size: Point) -> DrawList {
        let mut arc = [Point::default(); ARC_SAMPLES];
        for (i, p) in arc.iter_mut().enumerate() {
            let a = (i as f32 * 2.0 * PI) / ARC_SAMPLES as f32;
            *p = point(a.cos(), a.sin());
        }
        let mut sides = [0u8; 64];
        for (i, s) in sides.iter_mut().enumerate() {
            *s = if i > 0 { circle_sides(i as f32) as u8 } else { ARC_SAMPLES as u8 };
        }
        let mut list = DrawList {
            vertices: Vec::new(),
            indices: Vec::new(),
            commands: Vec::new(),
            white: point(atlas.white[0], atlas.white[1]),
            lines: atlas.lines,
            clip: [0.0; 4],
            clips: Vec::new(),
            texture: font,
            path: Vec::new(),
            temp: Vec::new(),
            arc,
            circle_sides: sides,
        };
        list.clear(font, size);
        list
    }

    /// Empty, for the next frame, in a window of `size`.
    pub fn clear(&mut self, font: Texture, size: Point) {
        self.vertices.clear();
        self.indices.clear();
        self.commands.clear();
        self.clips.clear();
        self.path.clear();
        self.texture = font;
        let whole = [0.0, 0.0, size.x, size.y];
        self.clips.push(whole);
        self.clip = whole;
    }

    /// What follows is drawn only inside min..max, and inside what it was drawn in before.
    pub fn push_clip(&mut self, min: Point, max: Point) {
        let mut clip = [min.x, min.y, max.x, max.y];
        let current = self.clip;
        if clip[0] < current[0] {
            clip[0] = current[0];
        }
        if clip[1] < current[1] {
            clip[1] = current[1];
        }
        if clip[2] > current[2] {
            clip[2] = current[2];
        }
        if clip[3] > current[3] {
            clip[3] = current[3];
        }
        clip[2] = im_max(clip[0], clip[2]);
        clip[3] = im_max(clip[1], clip[3]);
        self.clips.push(clip);
        self.clip = clip;
    }

    pub fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
        self.clip = *self.clips.last().unwrap();
    }

    // Room for `count` more indices: the command they go in, a new one when the
    // rectangle or the picture changed. The base the new vertices start at.
    fn reserve(&mut self, count: usize) -> u32 {
        match self.commands.last_mut() {
            Some(last) if last.clip == self.clip && last.texture == self.texture => last.count += count,
            _ => {
                self.commands.push(Command { clip: self.clip, texture: self.texture, first: self.indices.len(), count })
            }
        }
        self.vertices.len() as u32
    }

    fn vertex(&mut self, pos: Point, uv: Point, col: u32) {
        self.vertices.push(Vertex { pos, uv, col });
    }

    // PrimRectUV: two triangles, a b c and a c d.
    fn rect_uv(&mut self, a: Point, c: Point, uv_a: Point, uv_c: Point, col: u32) {
        let base = self.reserve(6);
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        self.vertex(a, uv_a, col);
        self.vertex(point(c.x, a.y), point(uv_c.x, uv_a.y), col);
        self.vertex(c, uv_c, col);
        self.vertex(point(a.x, c.y), point(uv_a.x, uv_c.y), col);
    }

    pub fn path_line_to(&mut self, p: Point) {
        self.path.push(p);
    }

    /// The path drawn as a line `thickness` wide, and cleared.
    pub fn path_stroke(&mut self, col: u32, closed: bool, thickness: f32) {
        let path = std::mem::take(&mut self.path);
        self.add_polyline(&path, col, closed, thickness);
        self.path = path;
        self.path.clear();
    }

    /// The path, a convex shape whose corners go clockwise, filled, and cleared.
    pub fn path_fill_convex(&mut self, col: u32) {
        let path = std::mem::take(&mut self.path);
        self.add_convex_poly_filled(&path, col);
        self.path = path;
        self.path.clear();
    }

    /// A line from p1 to p2 through the middles of their pixels.
    pub fn add_line(&mut self, p1: Point, p2: Point, col: u32, thickness: f32) {
        if col & ALPHA == 0 {
            return;
        }
        self.path_line_to(point(p1.x + 0.5, p1.y + 0.5));
        self.path_line_to(point(p2.x + 0.5, p2.y + 0.5));
        self.path_stroke(col, false, thickness);
    }

    /// The outline of a rectangle, its corners rounded `rounding` pixels.
    pub fn add_rect(&mut self, min: Point, max: Point, col: u32, rounding: f32, thickness: f32) {
        if col & ALPHA == 0 {
            return;
        }
        self.path_rect(point(min.x + 0.5, min.y + 0.5), point(max.x - 0.5, max.y - 0.5), rounding);
        self.path_stroke(col, true, thickness);
    }

    pub fn add_rect_filled(&mut self, min: Point, max: Point, col: u32, rounding: f32) {
        if col & ALPHA == 0 {
            return;
        }
        if rounding < 0.5 {
            let white = self.white;
            self.rect_uv(min, max, white, white, col);
        } else {
            self.path_rect(min, max, rounding);
            self.path_fill_convex(col);
        }
    }

    /// A rectangle shaded from a colour at each corner to the next.
    pub fn add_rect_filled_multi_color(
        &mut self,
        min: Point,
        max: Point,
        upper_left: u32,
        upper_right: u32,
        lower_right: u32,
        lower_left: u32,
    ) {
        if (upper_left | upper_right | lower_right | lower_left) & ALPHA == 0 {
            return;
        }
        let white = self.white;
        let base = self.reserve(6);
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        self.vertex(min, white, upper_left);
        self.vertex(point(max.x, min.y), white, upper_right);
        self.vertex(max, white, lower_right);
        self.vertex(point(min.x, max.y), white, lower_left);
    }

    pub fn add_quad_filled(&mut self, p1: Point, p2: Point, p3: Point, p4: Point, col: u32) {
        if col & ALPHA == 0 {
            return;
        }
        for p in [p1, p2, p3, p4] {
            self.path_line_to(p);
        }
        self.path_fill_convex(col);
    }

    pub fn add_circle_filled(&mut self, center: Point, radius: f32, col: u32) {
        if col & ALPHA == 0 || radius < 0.5 {
            return;
        }
        self.path_arc_fast(center, radius, 0, ARC_SAMPLES as i32);
        self.path.pop();
        self.path_fill_convex(col);
    }

    /// The picture's min_uv..max_uv over min..max, its colours multiplied by `col`.
    pub fn add_image(&mut self, texture: Texture, min: Point, max: Point, uv_min: Point, uv_max: Point, col: u32) {
        if col & ALPHA == 0 {
            return;
        }
        let before = self.texture;
        self.texture = texture;
        self.rect_uv(min, max, uv_min, uv_max, col);
        self.texture = before;
    }

    // PathRect, its corners rounded. Only a rounding of half a pixel or more names every
    // corner, so one that is not a number leaves each corner's arc of no radius.
    fn path_rect(&mut self, a: Point, b: Point, rounding: f32) {
        let mut rounding = rounding;
        let corners = rounding >= 0.5;
        if corners {
            rounding = im_min(rounding, (b.x - a.x).abs() * 0.5 - 1.0);
            rounding = im_min(rounding, (b.y - a.y).abs() * 0.5 - 1.0);
        }
        if rounding < 0.5 {
            self.path_line_to(a);
            self.path_line_to(point(b.x, a.y));
            self.path_line_to(b);
            self.path_line_to(point(a.x, b.y));
        } else {
            let r = if corners { rounding } else { 0.0 };
            self.path_arc_fast_12(point(a.x + r, a.y + r), r, 6, 9);
            self.path_arc_fast_12(point(b.x - r, a.y + r), r, 9, 12);
            self.path_arc_fast_12(point(b.x - r, b.y - r), r, 0, 3);
            self.path_arc_fast_12(point(a.x + r, b.y - r), r, 3, 6);
        }
    }

    // PathArcToFast: an arc between twelfths of a turn.
    fn path_arc_fast_12(&mut self, center: Point, radius: f32, from: i32, to: i32) {
        if radius < 0.5 {
            self.path.push(center);
            return;
        }
        let samples = ARC_SAMPLES as i32;
        self.path_arc_fast(center, radius, from * samples / 12, to * samples / 12);
    }

    // _PathArcToFastEx: the points of the table from sample `min` to `max`, stepping as
    // many as a circle of this radius needs.
    fn path_arc_fast(&mut self, center: Point, radius: f32, min: i32, max: i32) {
        if radius < 0.5 {
            self.path.push(center);
            return;
        }
        let samples = ARC_SAMPLES as i32;
        let index = int(radius + 0.999999);
        let sides =
            if (0..64).contains(&index) { i32::from(self.circle_sides[index as usize]) } else { circle_sides(radius) };
        let mut step = (samples / sides).clamp(1, samples / 4);
        let range = (max - min).abs();
        let next_step = step;
        let mut extra = false;
        if step > 1 {
            let overstep = range % step;
            if overstep > 0 {
                extra = true;
                if range > 0 {
                    step -= (step - overstep) / 2;
                }
            }
        }
        let mut sample = min;
        if !(0..samples).contains(&sample) {
            sample %= samples;
            if sample < 0 {
                sample += samples;
            }
        }
        let at = |s: Point| point(center.x + s.x * radius, center.y + s.y * radius);
        if max >= min {
            let mut a = min;
            while a <= max {
                if sample >= samples {
                    sample -= samples;
                }
                self.path.push(at(self.arc[sample as usize]));
                a += step;
                sample += step;
                step = next_step;
            }
        } else {
            let mut a = min;
            while a >= max {
                if sample < 0 {
                    sample += samples;
                }
                self.path.push(at(self.arc[sample as usize]));
                a -= step;
                sample -= step;
                step = next_step;
            }
        }
        if extra {
            let mut last = max % samples;
            if last < 0 {
                last += samples;
            }
            self.path.push(at(self.arc[last as usize]));
        }
    }

    // AddPolyline, smoothed: a line of whole pixels from its band in the atlas, a thinner
    // one as a core with a fringe on each side, a thicker one as a core between fringes.
    pub fn add_polyline(&mut self, points: &[Point], col: u32, closed: bool, thickness: f32) {
        let count_points = points.len();
        if count_points < 2 || col & ALPHA == 0 {
            return;
        }
        let aa = 1.0f32;
        let transparent = col & !ALPHA;
        let segments = if closed { count_points } else { count_points - 1 };
        let thick = thickness > aa;
        let thickness = im_max(thickness, 1.0);
        let whole = int(thickness);
        let fraction = thickness - whole as f32;
        let textured = whole < LINES_MOST as i32 && fraction <= 0.00001 && aa == 1.0;
        let opaque = self.white;

        let base = self.reserve(if textured {
            segments * 6
        } else if thick {
            segments * 18
        } else {
            segments * 12
        });
        let mut temp = std::mem::take(&mut self.temp);
        temp.clear();
        temp.resize(count_points * if textured || !thick { 3 } else { 5 }, Point::default());
        let (normals, out) = temp.split_at_mut(count_points);

        for i1 in 0..segments {
            let i2 = if i1 + 1 == count_points { 0 } else { i1 + 1 };
            let (dx, dy) = normalize(points[i2].x - points[i1].x, points[i2].y - points[i1].y);
            normals[i1] = point(dy, -dx);
        }
        if !closed {
            normals[count_points - 1] = normals[count_points - 2];
        }

        if textured || !thick {
            let half = if textured { thickness * 0.5 + 1.0 } else { aa };
            if !closed {
                let last = count_points - 1;
                out[0] = point(points[0].x + normals[0].x * half, points[0].y + normals[0].y * half);
                out[1] = point(points[0].x - normals[0].x * half, points[0].y - normals[0].y * half);
                out[last * 2] = point(points[last].x + normals[last].x * half, points[last].y + normals[last].y * half);
                out[last * 2 + 1] =
                    point(points[last].x - normals[last].x * half, points[last].y - normals[last].y * half);
            }
            let mut idx1 = base;
            for i1 in 0..segments {
                let i2 = if i1 + 1 == count_points { 0 } else { i1 + 1 };
                let idx2 = if i1 + 1 == count_points { base } else { idx1 + if textured { 2 } else { 3 } };
                let (dm_x, dm_y) =
                    fix_normal((normals[i1].x + normals[i2].x) * 0.5, (normals[i1].y + normals[i2].y) * 0.5);
                let (dm_x, dm_y) = (dm_x * half, dm_y * half);
                out[i2 * 2] = point(points[i2].x + dm_x, points[i2].y + dm_y);
                out[i2 * 2 + 1] = point(points[i2].x - dm_x, points[i2].y - dm_y);
                if textured {
                    self.indices.extend_from_slice(&[idx2, idx1, idx1 + 1, idx2 + 1, idx1 + 1, idx2]);
                } else {
                    self.indices.extend_from_slice(&[
                        idx2,
                        idx1,
                        idx1 + 2,
                        idx1 + 2,
                        idx2 + 2,
                        idx2,
                        idx2 + 1,
                        idx1 + 1,
                        idx1,
                        idx1,
                        idx2,
                        idx2 + 1,
                    ]);
                }
                idx1 = idx2;
            }
            if textured {
                let [u0, v0, u1, v1] = self.lines[whole as usize];
                for i in 0..count_points {
                    self.vertex(out[i * 2], point(u0, v0), col);
                    self.vertex(out[i * 2 + 1], point(u1, v1), col);
                }
            } else {
                for (i, &p) in points.iter().enumerate() {
                    self.vertex(p, opaque, col);
                    self.vertex(out[i * 2], opaque, transparent);
                    self.vertex(out[i * 2 + 1], opaque, transparent);
                }
            }
        } else {
            let inner = (thickness - aa) * 0.5;
            if !closed {
                let last = count_points - 1;
                for (at, p) in [(0, 0), (last * 4, last)] {
                    let n = normals[p];
                    out[at] = point(points[p].x + n.x * (inner + aa), points[p].y + n.y * (inner + aa));
                    out[at + 1] = point(points[p].x + n.x * inner, points[p].y + n.y * inner);
                    out[at + 2] = point(points[p].x - n.x * inner, points[p].y - n.y * inner);
                    out[at + 3] = point(points[p].x - n.x * (inner + aa), points[p].y - n.y * (inner + aa));
                }
            }
            let mut idx1 = base;
            for i1 in 0..segments {
                let i2 = if i1 + 1 == count_points { 0 } else { i1 + 1 };
                let idx2 = if i1 + 1 == count_points { base } else { idx1 + 4 };
                let (dm_x, dm_y) =
                    fix_normal((normals[i1].x + normals[i2].x) * 0.5, (normals[i1].y + normals[i2].y) * 0.5);
                let (out_x, out_y) = (dm_x * (inner + aa), dm_y * (inner + aa));
                let (in_x, in_y) = (dm_x * inner, dm_y * inner);
                let p = points[i2];
                out[i2 * 4] = point(p.x + out_x, p.y + out_y);
                out[i2 * 4 + 1] = point(p.x + in_x, p.y + in_y);
                out[i2 * 4 + 2] = point(p.x - in_x, p.y - in_y);
                out[i2 * 4 + 3] = point(p.x - out_x, p.y - out_y);
                self.indices.extend_from_slice(&[
                    idx2 + 1,
                    idx1 + 1,
                    idx1 + 2,
                    idx1 + 2,
                    idx2 + 2,
                    idx2 + 1,
                    idx2 + 1,
                    idx1 + 1,
                    idx1,
                    idx1,
                    idx2,
                    idx2 + 1,
                    idx2 + 2,
                    idx1 + 2,
                    idx1 + 3,
                    idx1 + 3,
                    idx2 + 3,
                    idx2 + 2,
                ]);
                idx1 = idx2;
            }
            for i in 0..count_points {
                self.vertex(out[i * 4], opaque, transparent);
                self.vertex(out[i * 4 + 1], opaque, col);
                self.vertex(out[i * 4 + 2], opaque, col);
                self.vertex(out[i * 4 + 3], opaque, transparent);
            }
        }
        self.temp = temp;
    }

    /// A convex shape whose corners go clockwise, filled, its edge smoothed by a fringe
    /// half a pixel either side.
    pub fn add_convex_poly_filled(&mut self, points: &[Point], col: u32) {
        let count = points.len();
        if count < 3 || col & ALPHA == 0 {
            return;
        }
        let aa = 1.0f32;
        let transparent = col & !ALPHA;
        let base = self.reserve((count - 2) * 3 + count * 6);
        let (inner, outer) = (base, base + 1);
        for i in 2..count as u32 {
            self.indices.extend_from_slice(&[inner, inner + ((i - 1) << 1), inner + (i << 1)]);
        }
        let mut normals = std::mem::take(&mut self.temp);
        normals.clear();
        normals.resize(count, Point::default());
        let mut i0 = count - 1;
        for i1 in 0..count {
            let (dx, dy) = normalize(points[i1].x - points[i0].x, points[i1].y - points[i0].y);
            normals[i0] = point(dy, -dx);
            i0 = i1;
        }
        let mut i0 = count - 1;
        for i1 in 0..count {
            let (n0, n1) = (normals[i0], normals[i1]);
            let (dm_x, dm_y) = fix_normal((n0.x + n1.x) * 0.5, (n0.y + n1.y) * 0.5);
            let (dm_x, dm_y) = (dm_x * (aa * 0.5), dm_y * (aa * 0.5));
            let white = self.white;
            self.vertex(point(points[i1].x - dm_x, points[i1].y - dm_y), white, col);
            self.vertex(point(points[i1].x + dm_x, points[i1].y + dm_y), white, transparent);
            let (a, b) = ((i1 as u32) << 1, (i0 as u32) << 1);
            self.indices.extend_from_slice(&[inner + a, inner + b, outer + b, outer + b, outer + a, inner + a]);
            i0 = i1;
        }
        self.temp = normals;
    }

    /// Text in `font` drawn `size` pixels tall from `pos` (its top left, set on a whole
    /// pixel), wrapped at `wrap` pixels across when that is above 0. It ends at a zero
    /// byte, as a C string does.
    pub fn add_text(&mut self, font: &Font, size: f32, pos: Point, col: u32, text: impl AsRef<[u8]>, wrap: f32) {
        let text = c_str(text.as_ref());
        if col & ALPHA == 0 || text.is_empty() {
            return;
        }
        self.render_text(font, size, pos, col, text, wrap);
    }

    // ImFont::RenderText, with the list's rectangle as the clip and no finer clipping.
    fn render_text(&mut self, font: &Font, size: f32, pos: Point, col: u32, text: &[u8], wrap: f32) {
        let clip = self.clip;
        let mut x = int(pos.x) as f32;
        let mut y = int(pos.y) as f32;
        if y > clip[3] {
            return;
        }
        let scale = size / font.size;
        let line_height = font.size * scale;
        let origin_x = x;
        let wrapping = wrap > 0.0;
        let mut end = text.len();

        // Past the lines above the clip.
        let mut s = 0;
        while y + line_height < clip[1] && s < end {
            let line_end = text[s..end].iter().position(|&c| c == b'\n').map(|at| s + at);
            if wrapping {
                s = wrap_position(font, scale, text, s, line_end.unwrap_or(end), wrap);
                s = next_line_start(text, s, end);
            } else {
                s = line_end.map_or(end, |at| at + 1);
            }
            y += line_height;
        }
        // A long text without wrapping: only up to the last line that can be seen.
        if end - s > 10000 && !wrapping {
            let (mut s_end, mut y_end) = (s, y);
            while y_end < clip[3] && s_end < end {
                s_end = text[s_end..end].iter().position(|&c| c == b'\n').map_or(end, |at| s_end + at + 1);
                y_end += line_height;
            }
            end = s_end;
        }
        if s == end {
            return;
        }

        let mut wrap_end: Option<usize> = None;
        while s < end {
            if wrapping {
                let eol =
                    *wrap_end.get_or_insert_with(|| wrap_position(font, scale, text, s, end, wrap - (x - origin_x)));
                if s >= eol {
                    x = origin_x;
                    y += line_height;
                    if y > clip[3] {
                        break;
                    }
                    wrap_end = None;
                    s = next_line_start(text, s, end);
                    continue;
                }
            }
            let (c, length) = decode(&text[s..end]);
            s += length;
            if c < 32 {
                if c == u32::from(b'\n') {
                    x = origin_x;
                    y += line_height;
                    if y > clip[3] {
                        break;
                    }
                    continue;
                }
                if c == u32::from(b'\r') {
                    continue;
                }
            }
            let glyph = font.glyph(c & 0xFFFF);
            let char_width = glyph.advance * scale;
            if glyph.visible {
                let x1 = x + glyph.x0 * scale;
                let x2 = x + glyph.x1 * scale;
                let y1 = y + glyph.y0 * scale;
                let y2 = y + glyph.y1 * scale;
                if x1 <= clip[2] && x2 >= clip[0] {
                    let (u1, v1, u2, v2) = (glyph.u0, glyph.v0, glyph.u1, glyph.v1);
                    let base = self.reserve(6);
                    self.vertex(point(x1, y1), point(u1, v1), col);
                    self.vertex(point(x2, y1), point(u2, v1), col);
                    self.vertex(point(x2, y2), point(u2, v2), col);
                    self.vertex(point(x1, y2), point(u1, v2), col);
                    self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                }
            }
            x += char_width;
        }
    }
}

/// The text up to its first zero byte.
fn c_str(text: &[u8]) -> &[u8] {
    text.split(|&c| c == 0).next().unwrap_or_default()
}

// ImTextCharFromUtf8: the character at the start of `text` and how many bytes it took.
// A malformed or incomplete one, and any past U+FFFF, is U+FFFD; a bad first byte takes
// one byte, a bad later one all the bytes up to it.
fn decode(text: &[u8]) -> (u32, usize) {
    const LENGTHS: [u8; 32] =
        [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2, 3, 3, 4, 0];
    const MASKS: [u32; 5] = [0x00, 0x7f, 0x1f, 0x0f, 0x07];
    const MINS: [u32; 5] = [0x400000, 0, 0x80, 0x800, 0x10000];
    const SHIFT_C: [u32; 5] = [0, 18, 12, 6, 0];
    const SHIFT_E: [u32; 5] = [0, 6, 4, 2, 0];
    let first = text.first().copied().unwrap_or(0);
    if first < 0x80 {
        return (u32::from(first), 1);
    }
    let len = usize::from(LENGTHS[usize::from(first >> 3)]);
    let wanted = len + usize::from(len == 0);
    let s: [u8; 4] = std::array::from_fn(|i| text.get(i).copied().unwrap_or(0));
    let mut c = (u32::from(s[0]) & MASKS[len]) << 18;
    c |= (u32::from(s[1]) & 0x3f) << 12;
    c |= (u32::from(s[2]) & 0x3f) << 6;
    c |= u32::from(s[3]) & 0x3f;
    c >>= SHIFT_C[len];
    let mut e = u32::from(c < MINS[len]) << 6;
    e |= u32::from((c >> 11) == 0x1b) << 7;
    e |= u32::from(c > 0xFFFF) << 8;
    e |= (u32::from(s[1]) & 0xc0) >> 2;
    e |= (u32::from(s[2]) & 0xc0) >> 4;
    e |= u32::from(s[3]) >> 6;
    e ^= 0x2a;
    e >>= SHIFT_E[len];
    if e != 0 {
        let present = s.iter().filter(|&&b| b != 0).count();
        return (0xFFFD, wanted.min(present));
    }
    (c, wanted)
}

fn blank(c: u32) -> bool {
    c == u32::from(b' ') || c == u32::from(b'\t') || c == 0x3000
}

// CalcWordWrapNextLineStartA: past the blanks after a wrap, and the line's end if it is next.
fn next_line_start(text: &[u8], mut s: usize, end: usize) -> usize {
    while s < end && (text[s] == b' ' || text[s] == b'\t') {
        s += 1;
    }
    if text.get(s) == Some(&b'\n') {
        s += 1;
    }
    s
}

// ImFont::CalcWordWrapPositionA: where the line from `s` wraps to fit `wrap` pixels: after
// the last word that fits, or after a punctuation mark, or anywhere in a word too long for
// a whole line; at least one byte on.
fn wrap_position(font: &Font, scale: f32, text: &[u8], start: usize, end: usize, wrap: f32) -> usize {
    let (mut line_width, mut word_width, mut blank_width) = (0.0f32, 0.0f32, 0.0f32);
    let wrap = wrap / scale;
    let mut word_end = start;
    let mut prev_word_end: Option<usize> = None;
    let mut inside_word = true;
    let mut s = start;
    while s < end {
        let (c, length) = decode(&text[s..end]);
        let next = s + length;
        if c < 32 {
            if c == u32::from(b'\n') {
                line_width = 0.0;
                word_width = 0.0;
                blank_width = 0.0;
                inside_word = true;
                s = next;
                continue;
            }
            if c == u32::from(b'\r') {
                s = next;
                continue;
            }
        }
        let char_width = font.advance(c);
        if blank(c) {
            if inside_word {
                line_width += blank_width;
                blank_width = 0.0;
                word_end = s;
            }
            blank_width += char_width;
            inside_word = false;
        } else {
            word_width += char_width;
            if inside_word {
                word_end = next;
            } else {
                prev_word_end = Some(word_end);
                line_width += word_width + blank_width;
                word_width = 0.0;
                blank_width = 0.0;
            }
            inside_word = !matches!(c, 0x2E | 0x2C | 0x3B | 0x21 | 0x3F | 0x22);
        }
        if line_width + word_width > wrap {
            if word_width < wrap {
                s = prev_word_end.unwrap_or(word_end);
            }
            break;
        }
        s = next;
    }
    if s == start && start < end {
        return s + 1;
    }
    s
}

impl Font {
    /// How big `text` is at `size`: as wide as its widest line, a line tall for each, up to
    /// where it would pass `max_width` (and where that is), wrapped at `wrap` when above 0.
    pub fn text_size(&self, size: f32, max_width: f32, wrap: f32, text: impl AsRef<[u8]>) -> (Point, usize) {
        let text = c_str(text.as_ref());
        let end = text.len();
        let line_height = size;
        let scale = size / self.size;
        let mut text_size = Point::default();
        let mut line_width = 0.0f32;
        let wrapping = wrap > 0.0;
        let mut wrap_end: Option<usize> = None;
        let mut s = 0;
        while s < end {
            if wrapping {
                let eol = *wrap_end.get_or_insert_with(|| wrap_position(self, scale, text, s, end, wrap - line_width));
                if s >= eol {
                    if text_size.x < line_width {
                        text_size.x = line_width;
                    }
                    text_size.y += line_height;
                    line_width = 0.0;
                    wrap_end = None;
                    s = next_line_start(text, s, end);
                    continue;
                }
            }
            let prev = s;
            let (c, length) = decode(&text[s..end]);
            s += length;
            if c < 32 {
                if c == u32::from(b'\n') {
                    text_size.x = im_max(text_size.x, line_width);
                    text_size.y += line_height;
                    line_width = 0.0;
                    continue;
                }
                if c == u32::from(b'\r') {
                    continue;
                }
            }
            let char_width = self.advance(c) * scale;
            if line_width + char_width >= max_width {
                s = prev;
                break;
            }
            line_width += char_width;
        }
        if text_size.x < line_width {
            text_size.x = line_width;
        }
        if line_width > 0.0 || text_size.y == 0.0 {
            text_size.y += line_height;
        }
        (text_size, s)
    }

    /// How wide `text` is at `size`, on one line or the widest of its lines.
    pub fn width(&self, size: f32, text: impl AsRef<[u8]>) -> f32 {
        self.text_size(size, f32::MAX, 0.0, text).0.x
    }
}
