// What the launcher shows: one screen, drawn by hand in the look of the game's own menus
// -- a smoky backdrop with a glow in one corner, a column of right-aligned entries with a
// black bar under the chosen one, and a pane beside it that says what the entry does or
// how a job is going.
//
// The screen is a description (Frame) the window fills each frame (window.rs), drawn
// with the draw list (draw.rs) in the fonts of the atlas (atlas.rs), over the backdrop,
// which is SDL's textures drawn first. tools/ui_diff.py holds it to the C++ it replaced,
// which drew it with Dear ImGui, as triangles and as pixels.
#![allow(unsafe_code)]

use crate::atlas::{Atlas, Font, Source};
use crate::draw::{DrawList, Point, Texture, int, point, rgba};
use crate::present::texture_of;
use crate::preview::{self, Preview};
use mw2_truetype::Malformed;
use sdl3_sys::everything as sdl;
use std::ptr;

#[path = "../icon/steam_emblem.rs"]
mod steam_emblem;

// The fonts, in the executable (assets/fonts/).
const FONT_MENU: &[u8] = include_bytes!("../../assets/fonts/BarlowCondensed-Regular.ttf");
const FONT_HEADING: &[u8] = include_bytes!("../../assets/fonts/BarlowCondensed-Medium.ttf");
const FONT_BODY: &[u8] = include_bytes!("../../assets/fonts/BarlowSemiCondensed-Regular.ttf");

// C++'s std::max, std::min and std::clamp, which answer otherwise than f32's at NaN.
pub fn std_max(a: f32, b: f32) -> f32 {
    if a < b { b } else { a }
}
pub fn std_min(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}
pub fn std_clamp(v: f32, low: f32, high: f32) -> f32 {
    if v < low {
        low
    } else if high < v {
        high
    } else {
        v
    }
}

/// The fonts, by their places in the atlas.
pub struct Fonts {
    pub menu: usize,    // the entries
    pub heading: usize, // the pane's heading and the wordmark
    pub body: usize,
    pub small: usize,
}

/// The embedded fonts, at sizes for a window `scale` times the design's 1280x720, in one
/// atlas: the body first, as Dear ImGui's default font was.
pub fn load_fonts(scale: f32) -> Result<(Atlas, Fonts), Malformed> {
    let size = |pixels: f32| (pixels * scale).round();
    let atlas = Atlas::build(
        &[
            Source { data: FONT_BODY, size: size(21.0) },
            Source { data: FONT_MENU, size: size(30.0) },
            Source { data: FONT_HEADING, size: size(40.0) },
            Source { data: FONT_BODY, size: size(16.0) },
        ],
        3,
    )?;
    Ok((atlas, Fonts { body: 0, menu: 1, heading: 2, small: 3 }))
}

/// The backdrop, made once: there is no artwork to ship, so it is computed. Its smoke
/// drifts, so it is drawn for a time.
pub struct Backdrop {
    smoke: [*mut sdl::SDL_Texture; 2],
    shade: *mut sdl::SDL_Texture,
    glow: *mut sdl::SDL_Texture,
    steam: *mut sdl::SDL_Texture, // Steam's emblem, white, for the cards to tint
}

#[derive(Clone, Default)]
pub struct Entry {
    pub label: Vec<u8>,
    pub enabled: bool,
    pub rule_above: bool, // a thin rule between this entry and the one before
    pub tag: Vec<u8>,     // a small note after the label: "SOON", "3 OF 4"
}

impl Entry {
    pub fn new(label: impl Into<Vec<u8>>) -> Entry {
        Entry { label: label.into(), enabled: true, rule_above: false, tag: Vec::new() }
    }
}

/// The cards' emblems: a screen for this computer only, three linked boxes for the local
/// network, a rack for a server, Steam's logo without its disc, rank chevrons for a
/// profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Emblem {
    Computer,
    Lan,
    Server,
    Steam,
    Profile,
}

/// A card under the heading in the look of the game's callsigns: an emblem, a name and a
/// title under it, and a small line above: "NETWORK SETTING".
#[derive(Clone)]
pub struct Card {
    pub emblem: Emblem,
    pub name: Vec<u8>,
    pub title: Vec<u8>,
    pub label: Vec<u8>,
    /// One of several to choose from: the chosen one is framed in amber, the others
    /// dimmed. None: shown as it is.
    pub chosen: Option<bool>,
}

#[derive(Clone)]
pub struct Frame {
    pub entries: Vec<Entry>,
    pub focus: i32,

    pub heading: Vec<u8>, // the pane
    pub text: Vec<u8>,
    pub error: bool, // the text reports a failure

    pub busy: bool, // a job is running: the pane shows its progress
    pub step: i32,
    pub steps: i32,
    pub step_title: Vec<u8>,
    pub detail: Vec<u8>,
    pub amount: Vec<u8>,
    pub fraction: f32, // of the step, or below zero when it has no amount

    // The play entries show the network the game plays on and the profile at the first
    // controller.
    pub cards: Vec<Card>,
    // And under them, what the chosen card's setting allows, in two columns.
    pub features: Vec<Vec<u8>>,

    pub slider: f32,              // a setting with a range: where it is on its track, 0 to 1
    pub fov: f32,                 // and the field of view the pane shows a picture of (preview.rs),
    pub aspect: f32,              // at the game picture's shape
    pub picture: Option<Texture>, // a picture above the text, 16:9

    pub status: Vec<u8>, // one line at the bottom left
    pub corner: Vec<u8>, // and one at the top right
    pub hint: Vec<u8>,   // which controls do what, bottom right
}

impl Default for Frame {
    fn default() -> Frame {
        Frame {
            entries: Vec::new(),
            focus: 0,
            heading: Vec::new(),
            text: Vec::new(),
            error: false,
            busy: false,
            step: 0,
            steps: 0,
            step_title: Vec::new(),
            detail: Vec::new(),
            amount: Vec::new(),
            fraction: -1.0,
            cards: Vec::new(),
            features: Vec::new(),
            slider: -1.0,
            fov: 0.0,
            aspect: 16.0 / 9.0,
            picture: None,
            status: Vec::new(),
            corner: Vec::new(),
            hint: Vec::new(),
        }
    }
}

/// What ui::Draw reads of the pointer and the clock, as Dear ImGui's IO had them.
#[derive(Clone, Copy, Default)]
pub struct Pointer {
    pub pos: Point,
    pub delta: Point,
    pub clicked: bool,
    pub down: bool,
    pub time: f64,
    pub delta_time: f32,
}

impl Pointer {
    // ImGui::IsMouseHoveringRect, unclipped.
    fn hovering(&self, min: Point, max: Point) -> bool {
        let p = self.pos;
        p.x >= min.x && p.y >= min.y && p.x < max.x && p.y < max.y
    }
}

const WHITE: u32 = rgba(236, 236, 232, 255);
const DIM: u32 = rgba(236, 236, 232, 96);
const BLACK: u32 = rgba(0, 0, 0, 255);
const AMBER: u32 = rgba(232, 178, 52, 255);
const GREEN: u32 = rgba(186, 232, 176, 255);
const RED: u32 = rgba(240, 150, 120, 255);

// `text` cut to fit `width`, ending in "..." when it doesn't (cut between whole UTF-8
// characters).
fn fitted(font: &Font, text: &[u8], width: f32) -> Vec<u8> {
    let wide = |t: &[u8]| font.width(font.size, t);
    if wide(text) <= width {
        return text.to_vec();
    }
    let with_dots = |t: &[u8]| [t, b"..."].concat();
    let mut cut = text.to_vec();
    while !cut.is_empty() && wide(&with_dots(&cut)) > width {
        // The last character, its continuation bytes first.
        let mut more = true;
        while more && !cut.is_empty() {
            more = cut.last().is_some_and(|&byte| byte & 0xC0 == 0x80);
            cut.pop();
        }
    }
    with_dots(&cut)
}

// The cards' emblems, drawn in a `side`-wide square at `at`.
fn draw_emblem(draw: &mut DrawList, steam: Option<Texture>, emblem: Emblem, at: Point, side: f32, colour: u32) {
    let u = side / 12.0;
    let line = std_max(1.0, u * 1.1);
    let p = |x: f32, y: f32| point(at.x + x * u, at.y + y * u);
    match emblem {
        Emblem::Computer => {
            draw.add_rect(p(1.0, 1.5), p(11.0, 8.5), colour, u, line);
            draw.add_line(p(6.0, 8.5), p(6.0, 10.5), colour, line);
            draw.add_line(p(3.5, 10.5), p(8.5, 10.5), colour, line);
        }
        Emblem::Lan => {
            draw.add_rect_filled(p(4.5, 0.5), p(7.5, 3.5), colour, u * 0.5);
            draw.add_rect_filled(p(0.5, 8.5), p(3.5, 11.5), colour, u * 0.5);
            draw.add_rect_filled(p(8.5, 8.5), p(11.5, 11.5), colour, u * 0.5);
            draw.add_line(p(6.0, 3.5), p(6.0, 6.0), colour, line);
            draw.add_line(p(2.0, 6.0), p(10.0, 6.0), colour, line);
            draw.add_line(p(2.0, 6.0), p(2.0, 8.5), colour, line);
            draw.add_line(p(10.0, 6.0), p(10.0, 8.5), colour, line);
        }
        Emblem::Server => {
            for y in [1.0, 6.5] {
                draw.add_rect(p(1.0, y), p(11.0, y + 4.5), colour, u, line);
                draw.add_circle_filled(p(3.5, y + 2.25), u * 0.9, colour);
            }
        }
        Emblem::Steam => {
            if let Some(steam) = steam {
                draw.add_image(steam, p(-0.6, -0.6), p(12.6, 12.6), point(0.0, 0.0), point(1.0, 1.0), colour);
            }
        }
        Emblem::Profile => {
            for y in [1.5, 5.0, 8.5] {
                draw.path_line_to(p(1.5, y + 2.5));
                draw.path_line_to(p(6.0, y));
                draw.path_line_to(p(10.5, y + 2.5));
                draw.path_stroke(colour, false, line * 1.6);
            }
        }
    }
}

// ---- the smoke: value noise, summed over a few octaves -------------------------------

pub fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393).wrapping_add((y as u32).wrapping_mul(668265263));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    ((h ^ (h >> 16)) & 0xFFFF) as f32 / 65535.0
}

// `period` cells across, after which it repeats: 0 for never.
fn noise(x: f32, y: f32, period: i32) -> f32 {
    let (xi, yi) = (int(x.floor()), int(y.floor()));
    let (mut fx, mut fy) = (x - xi as f32, y - yi as f32);
    fx = fx * fx * (3.0 - 2.0 * fx);
    fy = fy * fy * (3.0 - 2.0 * fy);
    let x0 = if period != 0 { xi.wrapping_rem(period) } else { xi };
    let x1 = if period != 0 { xi.wrapping_add(1).wrapping_rem(period) } else { xi.wrapping_add(1) };
    let (a, b, c, d) = (hash(x0, yi), hash(x1, yi), hash(x0, yi.wrapping_add(1)), hash(x1, yi.wrapping_add(1)));
    a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy
}

fn smoke(mut x: f32, mut y: f32) -> f32 {
    let (mut sum, mut weight) = (0.0f32, 0.5f32);
    for _ in 0..5 {
        sum += noise(x, y, 0) * weight;
        x *= 2.03;
        y *= 2.03;
        weight *= 0.5;
    }
    sum
}

// The same, repeating every `period` cells of x (x from 0), so that a picture of it can
// follow itself across the window.
fn smoke_around(mut x: f32, mut y: f32, mut period: i32) -> f32 {
    let (mut sum, mut weight) = (0.0f32, 0.5f32);
    for _ in 0..5 {
        sum += noise(x, y, period) * weight;
        x *= 2.0;
        y *= 2.0;
        period = period.wrapping_mul(2);
        weight *= 0.5;
    }
    sum
}

// Text with its letters set apart, as a wordmark is.
fn spaced(draw: &mut DrawList, font: &Font, size: f32, mut at: Point, colour: u32, text: &[u8], spacing: f32) {
    for c in text.iter().take_while(|&&c| c != 0) {
        let c = std::slice::from_ref(c);
        draw.add_text(font, size, at, colour, c, 0.0);
        at.x += font.width(size, c) + spacing;
    }
}

fn spaced_width(font: &Font, size: f32, text: &[u8], spacing: f32) -> f32 {
    let mut width = 0.0f32;
    for c in text.iter().take_while(|&&c| c != 0) {
        width += font.width(size, std::slice::from_ref(c)) + spacing;
    }
    width - spacing
}

// printf's %.0f, which spells what is not a number as C does.
fn c_whole(v: f64) -> Vec<u8> {
    let sign = if v.is_sign_negative() { "-" } else { "" };
    if v.is_nan() {
        format!("{sign}nan").into_bytes()
    } else if v.is_infinite() {
        format!("{sign}inf").into_bytes()
    } else {
        format!("{v:.0}").into_bytes()
    }
}

// The backdrop's pictures' size.
const BACKDROP_WIDTH: usize = 640;
const BACKDROP_HEIGHT: usize = 360;
// A layer holds the smoke as a factor about 1, and 1 is kept at this much of white so
// that a brighter wisp has room; the shade makes up for it.
const ONE: f32 = 0.75;

/// The backdrop's four pictures, as RGBA: two layers of smoke, each repeating across its
/// width; the shade the smoke is seen in, which multiplies them; and the glow, which is
/// added.
pub fn backdrop_pixels() -> [Vec<u8>; 4] {
    let fill = |colour: &dyn Fn(i32, i32, f32, f32) -> [f32; 3]| {
        let mut pixels = vec![0u8; BACKDROP_WIDTH * BACKDROP_HEIGHT * 4];
        for y in 0..BACKDROP_HEIGHT {
            for x in 0..BACKDROP_WIDTH {
                let rgb =
                    colour(x as i32, y as i32, x as f32 / BACKDROP_WIDTH as f32, y as f32 / BACKDROP_HEIGHT as f32);
                let p = &mut pixels[(y * BACKDROP_WIDTH + x) * 4..][..4];
                for c in 0..3 {
                    p[c] = (std_clamp(rgb[c], 0.0, 1.0) * 255.0) as u8;
                }
                p[3] = 255;
            }
        }
        pixels
    };
    // The two layers differ in grain and in place, and the second is drawn faintly over
    // the first: cells, tall, across, down.
    let layers: [(i32, f32, f32, f32); 2] = [(3, 2.3, 0.0, 3.0), (4, 3.1, 0.0, 41.0)];
    let layer = |(cells, tall, across, down): (i32, f32, f32, f32)| {
        fill(&|_, _, u, v| {
            let smoke = smoke_around(u * cells as f32 + across, v * tall + down, cells);
            let grey = ONE * (1.0 + (smoke - 0.48) * 1.05);
            [grey; 3]
        })
    };
    let shade = fill(&|x, y, u, v| {
        // Lit from the upper right, darker toward the bottom.
        let mut grey = 0.47 - 0.24 * v;
        let (lx, ly) = ((u - 0.70) * 1.78, v - 0.18);
        grey += 0.16 * (-(lx * lx + ly * ly) * 3.2).exp();
        // The column the entries stand in.
        #[allow(clippy::approx_constant)] // the design's figure, not 1/pi
        let column = 0.318;
        grey *= 0.74 + 0.26 * std_clamp((u - column) * 60.0, 0.0, 1.0);
        // The corners fall away.
        let (cx, cy) = (u - 0.5, v - 0.5);
        grey *= 1.0 - 0.55 * (cx * cx + cy * cy).powf(1.2);
        // A little grain keeps the gradients from banding.
        grey =
            (grey + (hash(x.wrapping_mul(3).wrapping_add(1), y.wrapping_mul(7).wrapping_add(5)) - 0.5) * 0.018) / ONE;
        [grey, grey, grey * 0.98]
    });
    let glow = fill(&|_, _, u, v| {
        // The glow in the bottom left corner.
        let (gx, gy) = ((u - 0.02) * 1.78, v - 1.06);
        let glow = (-(gx * gx * 1.1 + gy * gy * 2.6) * 2.4).exp() * (0.75 + 0.5 * smoke(u * 4.0, v * 4.0 + 11.0));
        [glow * 0.95, glow * 0.66, glow * 0.06]
    });
    [layer(layers[0]), layer(layers[1]), shade, glow]
}

/// Steam's emblem: white, its coverage as alpha, for the cards to tint.
pub fn steam_pixels() -> (usize, Vec<u8>) {
    let side = steam_emblem::SIZE;
    let mut white = vec![0u8; side * side * 4];
    for (i, &part) in steam_emblem::PARTS.iter().enumerate() {
        white[i * 4..i * 4 + 3].fill(255);
        white[i * 4 + 3] = part;
    }
    (side, white)
}

impl Backdrop {
    pub fn new(renderer: *mut sdl::SDL_Renderer) -> Backdrop {
        let [first, second, shade, glow] = backdrop_pixels();
        let (side, steam) = steam_pixels();
        // SAFETY: the caller's renderer, alive; each picture is its size in RGBA.
        unsafe {
            let texture = |pixels: &[u8], width: usize, height: usize, blend: sdl::SDL_BlendMode| {
                let made = sdl::SDL_CreateTexture(
                    renderer,
                    sdl::SDL_PIXELFORMAT_RGBA32,
                    sdl::SDL_TEXTUREACCESS_STATIC,
                    width as i32,
                    height as i32,
                );
                if !made.is_null() {
                    sdl::SDL_UpdateTexture(made, ptr::null(), pixels.as_ptr().cast(), (width * 4) as i32);
                    sdl::SDL_SetTextureScaleMode(made, sdl::SDL_SCALEMODE_LINEAR);
                    sdl::SDL_SetTextureBlendMode(made, blend);
                }
                made
            };
            let (w, h) = (BACKDROP_WIDTH, BACKDROP_HEIGHT);
            let smoke =
                [texture(&first, w, h, sdl::SDL_BLENDMODE_NONE), texture(&second, w, h, sdl::SDL_BLENDMODE_BLEND)];
            if !smoke[1].is_null() {
                sdl::SDL_SetTextureAlphaMod(smoke[1], 80);
            }
            Backdrop {
                smoke,
                shade: texture(&shade, w, h, sdl::SDL_BLENDMODE_MOD),
                glow: texture(&glow, w, h, sdl::SDL_BLENDMODE_ADD),
                steam: texture(&steam, side, side, sdl::SDL_BLENDMODE_BLEND),
            }
        }
    }

    /// The backdrop, `seconds` into its drift.
    pub fn draw(&self, renderer: *mut sdl::SDL_Renderer, seconds: f64) {
        // SAFETY: the caller's renderer, alive; the textures this made with it.
        unsafe {
            let (mut width, mut height) = (0, 0);
            sdl::SDL_GetCurrentRenderOutputSize(renderer, &mut width, &mut height);
            // Widths of the window a second, leftward: the first layer drifts right and
            // the fainter one, slower, left, as the smoke of the game's menus does.
            let drift = [-1.0 / 32.0, 1.0 / 50.0];
            for (&smoke, drift) in self.smoke.iter().zip(drift) {
                if smoke.is_null() {
                    continue;
                }
                let mut turn = (seconds * drift) % 1.0;
                if turn < 0.0 {
                    turn += 1.0;
                }
                // The picture and, where it has left the window, itself again.
                let left = -((turn * f64::from(width)) as f32);
                let (w, h) = (width as f32, height as f32);
                let first = sdl::SDL_FRect { x: left, y: 0.0, w, h };
                let second = sdl::SDL_FRect { x: left + w, y: 0.0, w, h };
                sdl::SDL_RenderTexture(renderer, smoke, ptr::null(), &first);
                sdl::SDL_RenderTexture(renderer, smoke, ptr::null(), &second);
            }
            if !self.shade.is_null() {
                sdl::SDL_RenderTexture(renderer, self.shade, ptr::null(), ptr::null());
            }
            if !self.glow.is_null() {
                sdl::SDL_RenderTexture(renderer, self.glow, ptr::null(), ptr::null());
            }
        }
    }

    /// Steam's emblem, as a picture of the backdrop's.
    pub fn steam(&self) -> Option<Texture> {
        (!self.steam.is_null()).then(|| texture_of(self.steam))
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        // SAFETY: the textures new() made; SDL takes a null one.
        unsafe {
            for texture in [self.smoke[0], self.smoke[1], self.shade, self.glow, self.steam] {
                sdl::SDL_DestroyTexture(texture);
            }
        }
    }
}

/// Draws the frame, Steam's card with `steam` (Backdrop::steam). Returns the entry the
/// pointer chose, or -1; `focus` follows the pointer when it moves over an entry. `slid`
/// is where the pointer holds the frame's slider, 0 to 1, or below zero.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    draw: &mut DrawList,
    atlas: &Atlas,
    fonts: &Fonts,
    steam: Option<Texture>,
    frame: &Frame,
    scale: f32,
    focus: &mut i32,
    slid: &mut f32,
    pointer: &Pointer,
    preview: &mut Preview,
) -> i32 {
    *slid = -1.0;
    let (menu, heading, body, small) =
        (&atlas.fonts[fonts.menu], &atlas.fonts[fonts.heading], &atlas.fonts[fonts.body], &atlas.fonts[fonts.small]);
    let s = |v: f32| (v * scale).round();
    let mut chosen = -1;

    // ---- the entries: right-aligned in the left column, the chosen one on a black bar
    // that ends in a slant
    let (column_right, row_height, bar_height) = (s(416.0), s(30.0), s(27.0));
    let mut y = s(150.0);
    for (i, entry) in frame.entries.iter().enumerate() {
        let i = i as i32;
        if entry.rule_above {
            draw.add_line(
                point(s(96.0), y + s(6.0)),
                point(column_right + s(4.0), y + s(6.0)),
                rgba(236, 236, 232, 110),
                1.0,
            );
            y += s(13.0);
        }
        let size = menu.size;
        let width = menu.width(size, &entry.label);
        let tag_width = if entry.tag.is_empty() { 0.0 } else { small.width(small.size, &entry.tag) + s(10.0) };
        let (row_min, row_max) = (point(0.0, y), point(column_right + s(48.0), y + bar_height));
        if pointer.hovering(row_min, row_max) {
            if pointer.delta.x != 0.0 || pointer.delta.y != 0.0 {
                *focus = i;
            }
            if pointer.clicked {
                *focus = i;
                chosen = i;
            }
        }
        if i == *focus {
            let slant = s(12.0);
            draw.add_rect_filled(point(0.0, y), point(s(90.0), y + bar_height), BLACK, 0.0);
            draw.add_quad_filled(
                point(s(96.0), y),
                point(column_right + s(30.0) + slant, y),
                point(column_right + s(30.0), y + bar_height),
                point(s(96.0), y + bar_height),
                BLACK,
            );
            for stroke in 0..2 {
                let x = column_right + s(36.0) + stroke as f32 * s(8.0);
                draw.add_quad_filled(
                    point(x + slant, y),
                    point(x + slant + s(4.0), y),
                    point(x + s(4.0), y + bar_height),
                    point(x, y + bar_height),
                    BLACK,
                );
            }
        }
        let colour = if entry.enabled { WHITE } else { DIM };
        let at = point(column_right - width - tag_width, y + (bar_height - size) * 0.5 - s(1.0));
        draw.add_text(menu, size, at, colour, &entry.label, 0.0);
        if !entry.tag.is_empty() {
            draw.add_text(
                small,
                small.size,
                point(column_right - tag_width + s(10.0), y + s(8.0)),
                AMBER,
                &entry.tag,
                0.0,
            );
        }
        y += row_height;
    }

    // ---- the pane
    let (left, right) = (s(520.0), s(1210.0));
    let mut py = s(150.0);
    if !frame.heading.is_empty() {
        spaced(draw, heading, heading.size, point(left, py), WHITE, &frame.heading, s(1.5));
        py += heading.size + s(6.0);
        draw.add_rect_filled(point(left, py), point(left + s(54.0), py + s(3.0)), AMBER, 0.0);
        py += s(22.0);
    }
    if !frame.cards.is_empty() {
        // Side by side: a square emblem on the left of a banner -- stripes for the
        // title's picture -- with the name, and the title under it -- and what the card
        // is above it.
        // The line above a card is left out when no card has one.
        let labelled = frame.cards.iter().any(|card| !card.label.is_empty());
        let (height, width, gap) = (s(58.0), s(300.0), s(16.0));
        let label = if labelled { small.size + s(6.0) } else { 0.0 };
        // A card not chosen is its emblem alone.
        let width_of = |card: &Card| if card.chosen == Some(false) { height } else { width };
        let mut x = left;
        for (index, card) in frame.cards.iter().enumerate() {
            let width = width_of(card);
            let whole = card.chosen != Some(false);
            if !card.label.is_empty() {
                spaced(draw, small, small.size, point(x, py), rgba(236, 236, 232, 170), &card.label, s(1.0));
            }
            let (min, max) = (point(x, py + label), point(x + width, py + label + height));
            // Shades of the backdrop's own grey, see-through, fading to the right.
            draw.add_rect_filled_multi_color(
                min,
                max,
                rgba(236, 236, 232, 26),
                rgba(236, 236, 232, 6),
                rgba(236, 236, 232, 6),
                rgba(236, 236, 232, 26),
            );
            draw.push_clip(min, max);
            let mut stripe = x + height - s(20.0);
            while whole && stripe < max.x {
                draw.add_quad_filled(
                    point(stripe + s(26.0), min.y),
                    point(stripe + s(32.0), min.y),
                    point(stripe + s(6.0), max.y),
                    point(stripe, max.y),
                    rgba(236, 236, 232, 8),
                );
                stripe += s(14.0);
            }
            draw.pop_clip();
            draw.add_rect_filled(min, point(x + height, max.y), rgba(0, 0, 0, 60), 0.0);
            draw_emblem(draw, steam, card.emblem, point(x + s(13.0), min.y + s(13.0)), height - s(26.0), AMBER);
            draw.add_rect(min, max, rgba(236, 236, 232, 30), 0.0, std_max(1.0, s(1.0)));
            if whole {
                let text_left = x + height + s(12.0);
                let text_width = max.x - text_left - s(10.0);
                let name = fitted(menu, &card.name, text_width);
                draw.add_text(menu, menu.size, point(text_left, min.y + s(6.0)), WHITE, &name, 0.0);
                let title = fitted(small, &card.title, text_width);
                draw.add_text(small, small.size, point(text_left, max.y - small.size - s(7.0)), AMBER, &title, 0.0);
            }
            match card.chosen {
                Some(true) => {
                    let edge = std_max(2.0, s(2.0));
                    draw.add_rect(min, max, AMBER, 0.0, edge);
                }
                Some(false) => draw.add_rect_filled(min, max, rgba(0, 0, 0, 120), 0.0),
                None => {}
            }
            x += width + gap;
            if frame.cards.get(index + 1).is_some_and(|next| x + width_of(next) > right) {
                x = left;
                py += label + height + gap;
            }
        }
        py += label + height + s(18.0);
    }
    if !frame.features.is_empty() {
        // An amber square before each, down the first column and then the second; a
        // little smaller than the body's text.
        let size = s(18.0);
        let (row, rows) = (size + s(7.0), frame.features.len().div_ceil(2));
        let (mark, indent) = (s(6.0), s(16.0));
        let wide = |text: &[u8]| body.width(size, text);
        let first = frame.features[..rows].iter().map(|feature| wide(feature)).fold(0.0, std_max);
        let last = frame.features[rows..].iter().map(|feature| wide(feature)).fold(0.0, std_max);
        // Past the first column's widest, unless the second's widest would then run off.
        let second = std_min(left + indent + first + s(36.0), right - indent - last);
        for (index, feature) in frame.features.iter().enumerate() {
            let x = if index < rows { left } else { second };
            let y = py + row * (index % rows) as f32;
            let middle = y + size * 0.5;
            draw.add_rect_filled(point(x, middle - mark * 0.5), point(x + mark, middle + mark * 0.5), AMBER, 0.0);
            // fitted measures at the font's own size: the room scaled to it.
            let room = if index < rows { second - x - indent - s(12.0) } else { right - x - indent };
            let text = fitted(body, feature, room * body.size / size);
            draw.add_text(body, size, point(x + indent, y), rgba(236, 236, 232, 225), &text, 0.0);
        }
        py += row * rows as f32 + s(14.0);
    }
    if frame.busy {
        let count = format!("STEP {} OF {}", frame.step, frame.steps);
        draw.add_text(small, small.size, point(left, py), AMBER, count, 0.0);
        py += small.size + s(6.0);
        draw.add_text(menu, menu.size, point(left, py), WHITE, &frame.step_title, 0.0);
        py += menu.size + s(18.0);
        // The bar: the step's own amount, or a band that travels when it has none.
        let (bar_width, bar) = (right - left, s(8.0));
        draw.add_rect_filled(point(left, py), point(right, py + bar), rgba(0, 0, 0, 150), 0.0);
        if frame.fraction >= 0.0 {
            let end = left + bar_width * std_clamp(frame.fraction, 0.0, 1.0);
            draw.add_rect_filled(point(left, py), point(end, py + bar), AMBER, 0.0);
        } else {
            let at = ((pointer.time * 0.6) % 1.0) as f32 * (bar_width + s(160.0)) - s(160.0);
            draw.add_rect_filled(
                point(std_max(left, left + at), py),
                point(std_min(right, left + at + s(160.0)), py + bar),
                AMBER,
                0.0,
            );
        }
        py += bar + s(12.0);
        draw.add_text(body, body.size, point(left, py), WHITE, &frame.amount, 0.0);
        // What the step is doing, right-aligned in the room the amount leaves.
        let amount_width = if frame.amount.is_empty() { 0.0 } else { body.width(body.size, &frame.amount) + s(24.0) };
        let detail = fitted(body, &frame.detail, std_max(0.0, bar_width - amount_width));
        let detail_width = body.width(body.size, &detail);
        draw.add_text(body, body.size, point(right - detail_width, py), rgba(236, 236, 232, 170), &detail, 0.0);
        py += body.size + s(24.0);
    }
    if frame.slider >= 0.0 {
        // A track as wide as the picture under it, filled as far as the knob.
        let (width, track, knob) = (s(416.0), s(4.0), s(7.0));
        let at = left + width * std_clamp(frame.slider, 0.0, 1.0);
        let middle = py + knob;
        if pointer.down
            && pointer.hovering(point(left - knob, py - s(4.0)), point(left + width + knob, py + knob * 2.0 + s(4.0)))
        {
            *slid = std_clamp((pointer.pos.x - left) / width, 0.0, 1.0);
        }
        let (top, bottom) = (middle - track * 0.5, middle + track * 0.5);
        draw.add_rect_filled(point(left, top), point(left + width, bottom), rgba(0, 0, 0, 150), 0.0);
        draw.add_rect_filled(point(left, top), point(at, bottom), AMBER, 0.0);
        draw.add_quad_filled(
            point(at, middle - knob),
            point(at + knob, middle),
            point(at, middle + knob),
            point(at - knob, middle),
            WHITE,
        );
        py += knob * 2.0 + s(12.0);
    }
    if frame.fov > 0.0 {
        let (min, max) = (point(left, py), point(left + s(416.0), py + s(416.0) / frame.aspect));
        preview.draw(draw, min, max, frame.fov, frame.aspect, pointer.delta_time);
        let across =
            [c_whole(f64::from(preview::across(frame.fov, frame.aspect))), b" DEGREES ACROSS THE SCREEN".to_vec()]
                .concat();
        draw.add_text(small, small.size, point(max.x + s(14.0), min.y), AMBER, across, 0.0);
        py = max.y + s(12.0);
    }
    if let Some(picture) = frame.picture {
        let (min, max) = (point(left, py), point(left + s(416.0), py + s(234.0)));
        draw.add_image(picture, min, max, point(0.0, 0.0), point(1.0, 1.0), rgba(255, 255, 255, 255));
        py = max.y + s(16.0);
    }
    if !frame.text.is_empty() {
        let colour = if frame.error { RED } else { rgba(236, 236, 232, 225) };
        draw.add_text(body, body.size, point(left, py), colour, &frame.text, right - left);
    }

    // ---- the wordmark, in the game's pale green with a glow behind it
    {
        let word = b"MW2 RECOMPILED";
        let (size, spacing) = (s(46.0), s(7.0));
        let width = spaced_width(heading, size, word, spacing);
        let at = point(s(1210.0) - width, s(560.0));
        for ring in (1..=3).rev() {
            for k in 0..8 {
                #[allow(clippy::approx_constant)] // the C++'s 0.785398f, which is not f32's pi/4
                let angle = k as f32 * 0.785398;
                let ring = ring as f32;
                let to = point(at.x + angle.cos() * ring * s(1.6), at.y + angle.sin() * ring * s(1.6));
                spaced(draw, heading, size, to, rgba(120, 220, 110, 14), word, spacing);
            }
        }
        spaced(draw, heading, size, at, GREEN, word, spacing);
        // Under it, ending where it ends: the line is the longer of the two.
        let note = b"UNOFFICIAL. NOT AFFILIATED WITH ACTIVISION, INFINITY WARD OR MICROSOFT.";
        let note_width = small.width(small.size, note);
        let to = point(s(1210.0) - note_width, at.y + size + s(4.0));
        draw.add_text(small, small.size, to, rgba(236, 236, 232, 150), note, 0.0);
    }

    // ---- the lines at the edges
    draw.add_text(body, body.size, point(s(152.0), s(640.0)), rgba(236, 236, 232, 215), &frame.status, 0.0);
    let corner_width = small.width(small.size, &frame.corner);
    let to = point(s(1210.0) - corner_width, s(78.0));
    draw.add_text(small, small.size, to, rgba(236, 236, 232, 170), &frame.corner, 0.0);
    let hint_width = small.width(small.size, &frame.hint);
    let to = point(s(1210.0) - hint_width, s(676.0));
    draw.add_text(small, small.size, to, rgba(236, 236, 232, 150), &frame.hint, 0.0);
    chosen
}
