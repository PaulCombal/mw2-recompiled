// tools/ui_diff.py's program: the launcher's window in Rust (launcher/src/ui.rs, preview.rs,
// input.rs) beside the C++ it replaced (ui_diff.cpp, with launcher/ui.cpp and preview.cpp
// compiled from git and Dear ImGui 1.91.9b, linked in), in one process:
//
//   ui_diff draw <seed> <frames> <scale>    the screen
//   ui_diff input <seed> <frames>           the keyboard and the pointer
//
// draw: frames of random screen descriptions -- entries, headings, cards of every emblem,
// a job's progress, sliders, the field of view's picture, pictures, texts long and short,
// in UTF-8 good and bad -- with the pointer anywhere, still or moving, its button up, down
// or going down, drawn by ui::Draw on each side at the window scale given, compared
// triangle by triangle (draw_diff.rs's records) with the entry chosen, the focus and the
// slider's place; then frames drawn by SDL's software renderer, the backdrop under them,
// compared pixel for pixel. input: SDL's events of every kind the window gets -- keys, the
// modifiers, text, the pointer, its buttons and wheel, the focus, the pointer leaving --
// a few a frame at random steps of the clock, given to Dear ImGui's SDL3 backend and to
// input.rs, and what the window asks each frame compared: each key it reads pressed and
// repeated, the pointer, its button, the time, the window's size. Exits 1 at the first
// difference.

#![allow(dead_code)]

extern crate mw2_truetype;
extern crate sdl3_sys;

#[path = "../launcher/src/atlas.rs"]
mod atlas;
#[path = "../launcher/src/draw.rs"]
mod draw;
#[path = "../launcher/src/input.rs"]
mod input;
#[path = "../launcher/src/present.rs"]
mod present;
#[path = "../launcher/src/preview.rs"]
mod preview;
#[path = "../launcher/src/ui.rs"]
mod ui;

use draw::{DrawList, Point, Texture, point};
use input::{Input, Key};
use preview::Preview;
use sdl3_sys::everything as sdl;
use std::ffi::c_int;
use std::process::exit;
use ui::{Card, Emblem, Entry, Frame, Pointer};

unsafe extern "C" {
    fn cpp_init(scale: f32);
    fn cpp_set_steam(id: u64);
    fn cpp_begin(width: f32, height: f32, nowhere: c_int, x: f32, y: f32, down: c_int, dt: f32, io: *mut f64);
    fn cpp_draw(data: *const u8, scale: f32, focus: *mut c_int, slid: *mut f32) -> c_int;
    fn cpp_end(size: *mut usize) -> *const u8;
    fn cpp_backdrop(renderer: *mut sdl::SDL_Renderer) -> u64;
    fn cpp_render(renderer: *mut sdl::SDL_Renderer, seconds: f64);
    fn cpp_input_init(window: *mut sdl::SDL_Window, renderer: *mut sdl::SDL_Renderer);
    fn cpp_event(event: *const sdl::SDL_Event);
    fn cpp_input_frame(dt: f32, keys: *mut c_int, io: *mut f64);
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
    // Now and then a value no screen has.
    fn odd(&mut self, usual: f32) -> f32 {
        if self.chance(60) { self.pick(&[f32::NAN, f32::INFINITY, -f32::INFINITY, -0.0, 1e30, -1e30]) } else { usual }
    }
}

// Pieces texts are made of: the launcher's own, Latin-1, past the atlas, malformed UTF-8,
// the controls the layout acts on.
const PIECES: &[&[u8]] = &[
    b"PLAY CAMPAIGN",
    b"INSTALL GAME",
    b"3 OF 4",
    b"BETA",
    b"The game is installed.",
    b"Choose the image of your Xbox 360 disc (.iso), or drop it on this window.",
    b"NETWORK SETTING",
    b"PLAYER 1 \xC2\xB7 RANK 70 \xC2\xB7 PRESTIGE 10",
    b"mw2.example.com:3074",
    b"word",
    b"w",
    b"Caf\xC3\xA9",
    b"\xC3\x91and\xC3\xBA",
    b"\xE2\x82\xAC",
    b"\xE6\x97\xA5\xE6\x9C\xAC",
    b"\xF0\x9F\x98\x80",
    b"\x80",
    b"\xC3",
    b"\xE2\x82",
    b"\t",
    b"\r",
    b"\n",
    b"\n\n",
    b" ",
    b"   ",
    b".",
    b"...",
    b"\x00",
];

fn text(random: &mut Random, most: usize) -> Vec<u8> {
    let mut text = Vec::new();
    if random.chance(30) {
        while text.len() < 600 + random.below(3000) {
            text.extend_from_slice(random.pick(PIECES));
        }
        return text;
    }
    for _ in 0..random.below(most + 1) {
        if random.chance(12) {
            text.push(random.next() as u8);
        } else {
            text.extend_from_slice(random.pick(PIECES));
        }
    }
    text
}

fn frame(random: &mut Random, picture: u64) -> Frame {
    let mut f = Frame::default();
    for _ in 0..random.below(14) {
        f.entries.push(Entry {
            label: text(random, 3),
            enabled: !random.chance(4),
            rule_above: random.chance(4),
            tag: if random.chance(5) { text(random, 2) } else { Vec::new() },
        });
    }
    let odd_focus = random.pick(&[-1, 99, i32::MIN]);
    let focus = random.below(f.entries.len() + 1) as i32;
    f.focus = if random.chance(10) { odd_focus } else { focus };
    if !random.chance(4) {
        f.heading = text(random, 3);
    }
    f.text = text(random, 12);
    f.error = random.chance(5);
    if random.chance(3) {
        f.busy = true;
        f.step = random.below(4) as i32;
        f.steps = random.below(4) as i32;
        f.step_title = text(random, 3);
        f.detail = text(random, 6);
        f.amount = if random.chance(3) { Vec::new() } else { text(random, 2) };
        let fraction = if random.chance(3) { -1.0 } else { random.float(-0.2, 1.2) };
        f.fraction = random.odd(fraction);
    }
    if random.chance(3) {
        for _ in 0..random.below(5) {
            f.cards.push(Card {
                emblem: random.pick(&[Emblem::Computer, Emblem::Lan, Emblem::Server, Emblem::Steam, Emblem::Profile]),
                name: text(random, 3),
                title: text(random, 4),
                label: if random.chance(3) { Vec::new() } else { text(random, 2) },
            });
        }
    }
    if random.chance(3) {
        let slider = random.float(-0.2, 1.2);
        f.slider = random.odd(slider);
    }
    if random.chance(3) {
        let fov = if random.chance(2) { (65 + 5 * random.below(12)) as f32 } else { random.float(-10.0, 170.0) };
        f.fov = random.odd(fov);
        let any = random.float(0.2, 4.0);
        let aspect = random.pick(&[16.0 / 9.0, 16.0 / 10.0, 4.0 / 3.0, any]);
        f.aspect = random.odd(aspect);
    }
    if random.chance(3) {
        f.picture = Some(Texture(picture as usize));
    }
    f.status = text(random, 3);
    f.corner = text(random, 2);
    f.hint = text(random, 3);
    f
}

// The frame as ui_diff.cpp reads it.
fn encode(f: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    let text = |out: &mut Vec<u8>, t: &[u8]| {
        out.extend_from_slice(&(t.len() as u32).to_ne_bytes());
        out.extend_from_slice(t);
    };
    out.extend_from_slice(&(f.entries.len() as u32).to_ne_bytes());
    for e in &f.entries {
        text(&mut out, &e.label);
        out.push(e.enabled as u8);
        out.push(e.rule_above as u8);
        text(&mut out, &e.tag);
    }
    out.extend_from_slice(&f.focus.to_ne_bytes());
    text(&mut out, &f.heading);
    text(&mut out, &f.text);
    out.push(f.error as u8);
    out.push(f.busy as u8);
    out.extend_from_slice(&f.step.to_ne_bytes());
    out.extend_from_slice(&f.steps.to_ne_bytes());
    text(&mut out, &f.step_title);
    text(&mut out, &f.detail);
    text(&mut out, &f.amount);
    out.extend_from_slice(&f.fraction.to_ne_bytes());
    out.extend_from_slice(&(f.cards.len() as u32).to_ne_bytes());
    for c in &f.cards {
        out.extend_from_slice(&(c.emblem as i32).to_ne_bytes());
        text(&mut out, &c.name);
        text(&mut out, &c.title);
        text(&mut out, &c.label);
    }
    out.extend_from_slice(&f.slider.to_ne_bytes());
    out.extend_from_slice(&f.fov.to_ne_bytes());
    out.extend_from_slice(&f.aspect.to_ne_bytes());
    out.extend_from_slice(&(f.picture.map_or(0, |t| t.0) as u64).to_ne_bytes());
    text(&mut out, &f.status);
    text(&mut out, &f.corner);
    text(&mut out, &f.hint);
    out
}

fn same_record(a: &[u8], b: &[u8]) -> bool {
    let float = |r: &[u8], at: usize| f32::from_ne_bytes(r[at..at + 4].try_into().unwrap());
    [0, 4, 8, 12, 24, 28, 32, 36].iter().all(|&at| {
        let (x, y) = (float(a, at), float(b, at));
        x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
    }) && a[16..24] == b[16..24]
        && a[40..44] == b[40..44]
}

fn records(list: &DrawList) -> Vec<u8> {
    let mut ours = Vec::new();
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
    ours
}

fn fail(what: String) -> ! {
    println!("DIFFERENT: {what}");
    exit(1);
}

// The pointer for a frame, given to Dear ImGui as events; what its IO then says, as
// ui::Draw reads it.
fn begin(random: &mut Random, size: Point, scale: f32, last: &mut (bool, f32, f32, bool)) -> Pointer {
    let (mut nowhere, mut x, mut y, mut down) = *last;
    if random.chance(2) {
        nowhere = random.chance(15);
        x = if random.chance(3) {
            random.below(size.x as usize + 1) as f32
        } else {
            random.float(-50.0, size.x + 50.0)
        };
        y = if random.chance(3) {
            random.below(size.y as usize + 1) as f32
        } else {
            random.float(-50.0, size.y + 50.0)
        };
        // Now and then on an edge: of the entries' rows, of the slider's track.
        let s = |v: f32| (v * scale).round();
        if random.chance(3) {
            let edge = random.pick(&[s(416.0) + s(48.0), s(520.0) - s(7.0), s(520.0) + s(416.0) + s(7.0), 0.0]);
            x = edge + random.pick(&[-1.0, 0.0, 0.0, 0.5]);
        }
        if random.chance(3) {
            let row =
                s(150.0) + random.below(14) as f32 * s(30.0) + random.pick(&[0.0, s(27.0), s(13.0), s(13.0) + s(27.0)]);
            y = row + random.pick(&[-1.0, 0.0, 0.0]);
        }
    }
    if random.chance(3) {
        down = !down;
    }
    *last = (nowhere, x, y, down);
    let dt = if random.chance(10) { random.pick(&[1e-9, 0.5, 3.0]) } else { random.float(0.001, 0.05) };
    let mut io = [0f64; 8];
    // SAFETY: a frame of the C++'s context; it fills eight numbers.
    unsafe { cpp_begin(size.x, size.y, nowhere as c_int, x, y, down as c_int, dt, io.as_mut_ptr()) };
    Pointer {
        pos: point(io[0] as f32, io[1] as f32),
        delta: point(io[2] as f32, io[3] as f32),
        clicked: io[4] != 0.0,
        down: io[5] != 0.0,
        time: io[6],
        delta_time: io[7] as f32,
    }
}

fn draw(seed: u64, frames: usize, scale: f32) {
    // SAFETY: the C++'s context, made once.
    unsafe { cpp_init(scale) };
    let (atlas, fonts) = ui::load_fonts(scale).unwrap_or_else(|_| fail("the Rust built no atlas".into()));
    let mut random = Random(seed);
    const STEAM: u64 = 7;
    const PICTURE: u64 = 9;
    unsafe { cpp_set_steam(STEAM) };
    let font = Texture(1);
    let mut list = DrawList::new(&atlas, font, point(1280.0, 720.0));
    let mut preview = Preview::new();
    let mut last = (true, 0.0, 0.0, false);
    let mut triangles = 0;
    for n in 0..frames {
        let size = if random.chance(2) {
            point(draw::int(1280.0 * scale) as f32, draw::int(720.0 * scale) as f32)
        } else {
            point(random.float(1.0, 2600.0).round(), random.float(1.0, 1500.0).round())
        };
        let pointer = begin(&mut random, size, scale, &mut last);
        list.clear(font, size);
        let f = frame(&mut random, PICTURE);
        let encoded = encode(&f);
        let start = random.below(f.entries.len() + 2) as i32 - 1;
        let (mut focus_c, mut slid_c) = (start, 5.0f32);
        let (mut focus_r, mut slid_r) = (start, 5.0f32);
        // SAFETY: the frame's bytes, alive across the call.
        let chosen_c = unsafe { cpp_draw(encoded.as_ptr(), scale, &mut focus_c, &mut slid_c) };
        let chosen_r = ui::draw(
            &mut list,
            &atlas,
            &fonts,
            Some(Texture(STEAM as usize)),
            &f,
            scale,
            &mut focus_r,
            &mut slid_r,
            &pointer,
            &mut preview,
        );
        let mut length = 0;
        // SAFETY: the C++'s buffer lives until its next frame.
        let theirs = unsafe { std::slice::from_raw_parts(cpp_end(&mut length), length) };
        let ours = records(&list);
        if (chosen_c, focus_c, slid_c.to_bits()) != (chosen_r, focus_r, slid_r.to_bits()) {
            fail(format!(
                "frame {n}: chosen, focus, slid: C++ {chosen_c} {focus_c} {slid_c}, Rust {chosen_r} {focus_r} {slid_r}"
            ));
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
            fail(format!(
                "frame {n} (window {size:?}), index {at} of {} (C++) and {} (Rust):\n  C++:  {}\n  Rust: {}",
                theirs.len() / 44,
                ours.len() / 44,
                record(theirs),
                record(&ours)
            ));
        }
        triangles += ours.len() / 132;
    }
    let drawn = pixels(&mut random, &atlas, &fonts, scale, (frames / 10).max(4), &mut preview, &mut last);
    println!("same: {frames} frames, {triangles} triangles, {drawn} frames drawn by SDL");
}

// Frames drawn as main.cpp drew them, by SDL's software renderer: the backdrop, then the
// list, through Dear ImGui's renderer backend and through present.rs.
fn pixels(
    random: &mut Random,
    atlas: &atlas::Atlas,
    fonts: &ui::Fonts,
    scale: f32,
    frames: usize,
    preview: &mut Preview,
    last: &mut (bool, f32, f32, bool),
) -> usize {
    let size = point(draw::int(1280.0 * scale) as f32, draw::int(720.0 * scale) as f32);
    // SAFETY: SDL's calls on a renderer and textures made here and alive throughout; the
    // pixels read are copied out of the surface SDL returns before it is freed.
    unsafe {
        let surface = sdl::SDL_CreateSurface(size.x as c_int, size.y as c_int, sdl::SDL_PIXELFORMAT_RGBA32);
        let renderer = if surface.is_null() { std::ptr::null_mut() } else { sdl::SDL_CreateSoftwareRenderer(surface) };
        if renderer.is_null() {
            fail("no software renderer".into());
        }
        cpp_backdrop(renderer);
        let backdrop = ui::Backdrop::new(renderer);
        let Some(font) = present::atlas_texture(renderer, atlas) else { fail("no atlas texture".into()) };
        let (w, h) = (416, 234);
        let picture =
            sdl::SDL_CreateTexture(renderer, sdl::SDL_PIXELFORMAT_RGBA32, sdl::SDL_TEXTUREACCESS_STATIC, w, h);
        let bytes: Vec<u8> = (0..w * h * 4).map(|_| random.next() as u8).collect();
        sdl::SDL_UpdateTexture(picture, std::ptr::null(), bytes.as_ptr().cast(), w * 4);
        let mut list = DrawList::new(atlas, font, size);
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
        for n in 0..frames {
            let pointer = begin(random, size, scale, last);
            list.clear(font, size);
            let f = frame(random, picture as u64);
            let encoded = encode(&f);
            let (mut focus_c, mut slid_c) = (0, -1.0f32);
            let (mut focus_r, mut slid_r) = (0, -1.0f32);
            cpp_draw(encoded.as_ptr(), scale, &mut focus_c, &mut slid_c);
            ui::draw(
                &mut list,
                atlas,
                fonts,
                backdrop.steam(),
                &f,
                scale,
                &mut focus_r,
                &mut slid_r,
                &pointer,
                preview,
            );
            let mut length = 0;
            cpp_end(&mut length);
            let seconds =
                if random.chance(5) { random.float(-1000.0, 1e6) as f64 } else { random.float(0.0, 600.0) as f64 };
            cpp_render(renderer, seconds);
            let imgui = read();
            sdl::SDL_SetRenderDrawColor(renderer, 40, 40, 38, 255);
            sdl::SDL_RenderClear(renderer);
            backdrop.draw(renderer, seconds);
            present::present(renderer, &list, size, point(1.0, 1.0), &mut colours);
            let rust = read();
            if imgui != rust {
                let at = imgui.iter().zip(&rust).position(|(a, b)| a != b).unwrap() / 4;
                let (x, y) = (at % size.x as usize, at / size.x as usize);
                let count = imgui.chunks(4).zip(rust.chunks(4)).filter(|(a, b)| a != b).count();
                fail(format!(
                    "pixels of frame {n} ({seconds} s): {count} differ, the first at {x},{y}: C++ {:?}, Rust {:?}",
                    &imgui[at * 4..at * 4 + 4],
                    &rust[at * 4..at * 4 + 4]
                ));
            }
        }
        frames
    }
}

// ---- the input ---------------------------------------------------------------------

// The keys ui_diff.cpp asks about, in its order.
const ASKED: [Key; 21] = [
    Key::LeftArrow,
    Key::RightArrow,
    Key::UpArrow,
    Key::DownArrow,
    Key::Enter,
    Key::KeypadEnter,
    Key::Space,
    Key::Escape,
    Key::GamepadDpadLeft,
    Key::GamepadDpadRight,
    Key::GamepadDpadUp,
    Key::GamepadDpadDown,
    Key::GamepadLStickLeft,
    Key::GamepadLStickRight,
    Key::GamepadLStickUp,
    Key::GamepadLStickDown,
    Key::GamepadFaceDown,
    Key::GamepadFaceRight,
    Key::Letter(0),
    Key::Comma,
    Key::Keypad(5),
];

fn input(seed: u64, frames: usize) {
    use sdl::*;
    let mut random = Random(seed);
    // SAFETY: SDL's calls on a window and renderer made here, alive throughout; every
    // event handed over is a whole SDL_Event of the kind its type says.
    unsafe {
        SDL_SetHint(SDL_HINT_VIDEO_DRIVER, c"offscreen".as_ptr());
        if !SDL_Init(SDL_INIT_VIDEO | SDL_INIT_GAMEPAD) {
            fail("SDL could not start".into());
        }
        let (mut window, mut renderer) = (std::ptr::null_mut(), std::ptr::null_mut());
        if !SDL_CreateWindowAndRenderer(
            c"ui_diff".as_ptr(),
            960,
            540,
            SDL_WindowFlags::default(),
            &mut window,
            &mut renderer,
        ) {
            fail("no window".into());
        }
        let id = SDL_GetWindowID(window);
        cpp_input_init(window, renderer);
        let mut ours = Input::new(window);
        let keys: [(SDL_Keycode, SDL_Scancode); 16] = [
            (SDLK_LEFT, SDL_SCANCODE_LEFT),
            (SDLK_RIGHT, SDL_SCANCODE_RIGHT),
            (SDLK_UP, SDL_SCANCODE_UP),
            (SDLK_DOWN, SDL_SCANCODE_DOWN),
            (SDLK_RETURN, SDL_SCANCODE_RETURN),
            (SDLK_KP_ENTER, SDL_SCANCODE_KP_ENTER),
            (SDLK_SPACE, SDL_SCANCODE_SPACE),
            (SDLK_ESCAPE, SDL_SCANCODE_ESCAPE),
            (SDLK_A, SDL_SCANCODE_A),
            (SDLK_COMMA, SDL_SCANCODE_COMMA),
            (SDLK_SEMICOLON, SDL_SCANCODE_M),
            (SDLK_M, SDL_SCANCODE_COMMA),
            (SDLK_KP_5, SDL_SCANCODE_KP_5),
            (SDLK_LSHIFT, SDL_SCANCODE_LSHIFT),
            (SDLK_LCTRL, SDL_SCANCODE_LCTRL),
            (SDLK_UNKNOWN, SDL_SCANCODE_GRAVE),
        ];
        let text = c"\xc3\xa9t\xc3\xa9";
        let mut counter: u64 = 0;
        let mut previous: u64 = 0;
        for n in 0..frames {
            for _ in 0..random.below(5) {
                let mut e: SDL_Event = std::mem::zeroed();
                match random.below(10) {
                    0..=3 => {
                        let (key, scancode) = random.pick(&keys);
                        let down = random.chance(2);
                        e.key.r#type = if down { SDL_EVENT_KEY_DOWN } else { SDL_EVENT_KEY_UP };
                        e.key.windowID = if random.chance(30) { SDL_WindowID(id.0 + 1) } else { id };
                        e.key.key = key;
                        e.key.scancode = scancode;
                        e.key.r#mod = SDL_Keymod(random.pick(&[0, 0, 0, 0x1, 0x40, 0x41, 0x100, 0x400]));
                        e.key.down = down;
                        e.key.repeat = down && random.chance(3);
                    }
                    4 | 5 => {
                        e.motion.r#type = SDL_EVENT_MOUSE_MOTION;
                        e.motion.windowID = id;
                        e.motion.which = if random.chance(10) { SDL_TOUCH_MOUSEID } else { SDL_MouseID(0) };
                        e.motion.x = random.float(-100.0, 1100.0);
                        e.motion.y = random.float(-100.0, 700.0);
                    }
                    6 => {
                        e.button.r#type =
                            if random.chance(2) { SDL_EVENT_MOUSE_BUTTON_DOWN } else { SDL_EVENT_MOUSE_BUTTON_UP };
                        e.button.windowID = id;
                        e.button.which = if random.chance(10) { SDL_TOUCH_MOUSEID } else { SDL_MouseID(0) };
                        e.button.button = random.pick(&[1u8, 1, 1, 2, 3, 4, 5, 9]);
                        e.button.down = e.button.r#type == SDL_EVENT_MOUSE_BUTTON_DOWN;
                    }
                    7 => {
                        e.wheel.r#type = SDL_EVENT_MOUSE_WHEEL;
                        e.wheel.windowID = id;
                        e.wheel.x = random.pick(&[0.0, 0.0, 1.0]);
                        e.wheel.y = random.pick(&[0.0, 1.0, -1.0]);
                    }
                    8 => {
                        e.text.r#type = SDL_EVENT_TEXT_INPUT;
                        e.text.windowID = id;
                        e.text.text = text.as_ptr();
                    }
                    _ => {
                        e.window.r#type = random.pick(&[
                            SDL_EVENT_WINDOW_FOCUS_LOST,
                            SDL_EVENT_WINDOW_FOCUS_GAINED,
                            SDL_EVENT_WINDOW_MOUSE_LEAVE,
                            SDL_EVENT_WINDOW_MOUSE_ENTER,
                            SDL_EVENT_GAMEPAD_ADDED,
                        ]);
                        e.window.windowID = id;
                    }
                }
                ours.event(&e);
                cpp_event(&e);
            }
            // The clock: a frame's step, a long pause, or none at all.
            counter += match random.below(12) {
                0 => 0,
                1 => random.below(3_000_000_000) as u64,
                _ => 5_000_000 + random.below(40_000_000) as u64,
            };
            let frequency = SDL_GetPerformanceFrequency();
            let now = if counter <= previous { previous + 1 } else { counter };
            let dt = if previous > 0 { ((now - previous) as f64 / frequency as f64) as f32 } else { 1.0 / 60.0 };
            previous = now;
            ours.new_frame_at(counter);
            let mut keys_c = [0 as c_int; ASKED.len() * 2];
            let mut io = [0f64; 12];
            cpp_input_frame(dt, keys_c.as_mut_ptr(), io.as_mut_ptr());
            let mut keys_r = [0 as c_int; ASKED.len() * 2];
            for (i, &key) in ASKED.iter().enumerate() {
                keys_r[i * 2] = ours.pressed(&[key], false) as c_int;
                keys_r[i * 2 + 1] = ours.pressed(&[key], true) as c_int;
            }
            let (size, scale) = ours.display();
            let p = ours.mouse_pos();
            let d = ours.mouse_delta();
            let mine = [
                f64::from(p.x),
                f64::from(p.y),
                f64::from(d.x),
                f64::from(d.y),
                ours.clicked() as u8 as f64,
                ours.mouse_down() as u8 as f64,
                ours.time(),
                f64::from(ours.delta_time()),
                f64::from(size.x),
                f64::from(size.y),
                f64::from(scale.x),
                f64::from(scale.y),
            ];
            if keys_c != keys_r {
                fail(format!("frame {n}: keys pressed, repeated: C++ {keys_c:?}, Rust {keys_r:?}"));
            }
            if io.iter().zip(&mine).any(|(a, b)| a.to_bits() != b.to_bits()) {
                fail(format!("frame {n}: pointer, time, window: C++ {io:?}, Rust {mine:?}"));
            }
            ours.end_frame();
        }
        println!("same: {frames} frames of input");
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    match arguments.get(1).map(String::as_str) {
        Some("draw") if arguments.len() == 5 => {
            draw(arguments[2].parse().unwrap(), arguments[3].parse().unwrap(), arguments[4].parse().unwrap())
        }
        Some("input") if arguments.len() == 4 => input(arguments[2].parse().unwrap(), arguments[3].parse().unwrap()),
        _ => {
            eprintln!("usage: ui_diff draw <seed> <frames> <scale> | ui_diff input <seed> <frames>");
            exit(2);
        }
    }
}
