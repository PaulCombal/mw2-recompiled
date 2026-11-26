// What the player sets in the launcher for the game: kept in .env beside the launcher, a
// line each of NAME=value. The game reads the file as it starts (src/runtime/settings.rs),
// however it is started, so a change is for the next start. The file can hold any of the
// game's switches; the launcher changes its own and keeps the rest, in the order of their
// names, as they were but for the carriage returns and spaces that ended a line. Numbers
// are read as C's atoi and scanf read them (cnum.rs).
#![deny(unsafe_code)]

use crate::cnum::{atoi, c_str, scan_u};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

const FILE: &str = ".env";

// Every line, so the ones a player wrote by hand are written back.
static KEPT: Mutex<BTreeMap<Vec<u8>, Vec<u8>>> = Mutex::new(BTreeMap::new());

fn kept() -> MutexGuard<'static, BTreeMap<Vec<u8>, Vec<u8>>> {
    KEPT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn get(name: &str) -> Option<Vec<u8>> {
    kept().get(name.as_bytes()).cloned()
}

// A value given, or the line gone.
fn change(name: &str, value: Option<Vec<u8>>) {
    let mut kept = kept();
    match value {
        Some(value) => kept.insert(name.as_bytes().to_vec(), value),
        None => kept.remove(name.as_bytes()),
    };
    save(&kept);
}

fn set(name: &str, value: impl Into<Vec<u8>>) {
    change(name, Some(value.into()));
}

fn save(kept: &BTreeMap<Vec<u8>, Vec<u8>>) {
    let mut text = Vec::new();
    for (name, value) in kept {
        text.extend_from_slice(name);
        text.push(b'=');
        text.extend_from_slice(value);
        text.push(b'\n');
    }
    // As an ofstream that cannot open: nothing is said.
    let _ = std::fs::write(FILE, text);
}

// The C's tests: set, and neither empty nor "0".
fn on(name: &str) -> bool {
    get(name).is_some_and(|value| !value.is_empty() && value != b"0")
}

// atoi of the value, `none` without one.
fn number(name: &str, none: i32) -> i32 {
    get(name).map_or(none, |value| atoi(c_str(&value)))
}

pub fn load() {
    let Ok(text) = std::fs::read(FILE) else { return };
    let mut kept = kept();
    let mut lines: Vec<&[u8]> = text.split(|&byte| byte == b'\n').collect();
    if text.last() == Some(&b'\n') || text.is_empty() {
        lines.pop();
    }
    for mut line in lines {
        while let [rest @ .., b'\r' | b' '] = line {
            line = rest;
        }
        let Some(equals) = line.iter().position(|&byte| byte == b'=') else { continue };
        kept.insert(line[..equals].to_vec(), line[equals + 1..].to_vec());
    }
}

pub fn fullscreen() -> bool {
    on("MW2_FULLSCREEN")
}

pub fn set_fullscreen(on: bool) {
    set("MW2_FULLSCREEN", if on { "1" } else { "0" });
}

pub const MAX_SCALE: i32 = 3;

pub fn scale() -> i32 {
    number("MW2_SCALE", 1).clamp(1, MAX_SCALE)
}

// ---- the picture's shape and size ---------------------------------------------------

/// A row of RESOLUTION, "1920x1200": the title's frame, and the scale.
pub struct Resolution {
    pub width: u32,
    pub height: u32,
    pub scale: i32,
}

const fn row(width: u32, height: u32, scale: i32) -> Resolution {
    Resolution { width, height, scale }
}

/// 16:9, the console's, 16:10 and 4:3.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Ratio {
    Wide,
    Wider,
    Standard,
}

pub const RATIOS: i32 = 3;

impl Ratio {
    pub fn of(index: i32) -> Ratio {
        match index {
            1 => Ratio::Wider,
            2 => Ratio::Standard,
            _ => Ratio::Wide,
        }
    }
}

const WIDE: [Resolution; 4] = [row(1280, 720, 1), row(960, 540, 2), row(1280, 720, 2), row(1280, 720, 3)];
const WIDER: [Resolution; 4] = [row(1280, 800, 1), row(960, 600, 2), row(1280, 800, 2), row(1280, 800, 3)];
const STANDARD: [Resolution; 7] = [
    row(640, 480, 1),
    row(800, 600, 1),
    row(1024, 768, 1),
    row(640, 480, 2),
    row(800, 600, 2),
    row(960, 720, 2),
    row(960, 720, 3),
];

pub fn resolutions(ratio: Ratio) -> &'static [Resolution] {
    match ratio {
        Ratio::Wide => &WIDE,
        Ratio::Wider => &WIDER,
        Ratio::Standard => &STANDARD,
    }
}

// Every row is a frame the title can be given, by the game's rule (frame_valid in
// src/runtime/render_size.rs): at most the console's 1280 wide and 800 tall, at least 640
// by 360, a whole number of 16 pixels wide and 4 tall, and fitting EDRAM, 2048 tiles of
// 80x16 samples, with its colour and depth at one sample.
const fn frame_valid(width: u32, height: u32) -> bool {
    let tiles = width.div_ceil(80) * height.div_ceil(16);
    width >= 640
        && height >= 360
        && width <= 1280
        && height <= 800
        && width.is_multiple_of(16)
        && height.is_multiple_of(4)
        && 2 * tiles <= 2048
}

const fn rows_valid() -> bool {
    let all: [&[Resolution]; 3] = [&WIDE, &WIDER, &STANDARD];
    let mut set = 0;
    while set < all.len() {
        let mut i = 0;
        while i < all[set].len() {
            if !frame_valid(all[set][i].width, all[set][i].height) {
                return false;
            }
            i += 1;
        }
        set += 1;
    }
    true
}
const _: () = assert!(rows_valid());

// MW2_RENDER's frame, as sscanf's "%ux%u" reads it; the console's when there is none or
// it is no size.
fn kept_frame() -> (u32, u32) {
    let frame = get("MW2_RENDER").and_then(|value| {
        let (width, rest) = scan_u(c_str(&value))?;
        let rest = rest.strip_prefix(b"x")?;
        let (height, _) = scan_u(rest)?;
        (height != 0).then_some((width, height))
    });
    frame.unwrap_or((1280, 720))
}

pub fn aspect_ratio() -> Ratio {
    let (width, height) = kept_frame();
    // unsigned arithmetic, wrapping as the C's did
    if width.wrapping_mul(10) == height.wrapping_mul(16) {
        Ratio::Wider
    } else if width.wrapping_mul(3) == height.wrapping_mul(4) {
        Ratio::Standard
    } else {
        Ratio::Wide
    }
}

/// The kept row; a frame and scale written by hand that are no row are shown as the row
/// of their shape with their scale, or the first.
pub fn resolution_row() -> usize {
    let (width, height) = kept_frame();
    let rows = resolutions(aspect_ratio());
    let scale = scale();
    rows.iter()
        .position(|row| row.width == width && row.height == height && row.scale == scale)
        .or_else(|| rows.iter().position(|row| row.scale == scale))
        .unwrap_or(0)
}

pub fn current_resolution() -> &'static Resolution {
    &resolutions(aspect_ratio())[resolution_row()]
}

pub fn set_resolution(ratio: Ratio, row: i32) {
    let rows = resolutions(ratio);
    let chosen = &rows[row.clamp(0, rows.len() as i32 - 1) as usize];
    let mut kept = kept();
    if chosen.width == 1280 && chosen.height == 720 {
        kept.remove(b"MW2_RENDER".as_slice());
    } else {
        kept.insert(b"MW2_RENDER".to_vec(), format!("{}x{}", chosen.width, chosen.height).into_bytes());
    }
    kept.insert(b"MW2_SCALE".to_vec(), chosen.scale.to_string().into_bytes());
    save(&kept);
}

/// The next shape, with the row of it nearest in height to the kept one.
pub fn next_aspect_ratio() {
    let kept = current_resolution();
    let height = kept.height * kept.scale as u32;
    let next = Ratio::of((aspect_ratio() as i32 + 1) % RATIOS);
    let rows = resolutions(next);
    let off = |row: &Resolution| (row.height as i32 * row.scale - height as i32).abs();
    let mut nearest = 0;
    for (index, row) in rows.iter().enumerate().skip(1) {
        if off(row) < off(&rows[nearest]) {
            nearest = index;
        }
    }
    set_resolution(next, nearest as i32);
}

// ---- the frame rate, the view, the picture's quality --------------------------------

pub const CONSOLE_FPS: i32 = 60;

pub fn has_fps_limit() -> bool {
    get("MW2_FPS_LIMIT").is_some()
}

pub fn fps_limit() -> i32 {
    get("MW2_FPS_LIMIT").map_or(CONSOLE_FPS, |value| atoi(c_str(&value)).clamp(0, 1000))
}

pub fn set_fps_limit(limit: i32) {
    set("MW2_FPS_LIMIT", limit.to_string());
}

pub const CONSOLE_FOV: i32 = 65;
pub const WIDEST_FOV: i32 = 120;

pub fn fov() -> i32 {
    get("MW2_FOV").map_or(CONSOLE_FOV, |value| atoi(c_str(&value)).clamp(CONSOLE_FOV, WIDEST_FOV))
}

pub fn set_fov(fov: i32) {
    set("MW2_FOV", fov.clamp(CONSOLE_FOV, WIDEST_FOV).to_string());
}

pub fn msaa() -> i32 {
    if on("MW2_NO_MSAA") {
        return 0;
    }
    let samples = number("MW2_MSAA", 0);
    if samples > 1 { samples } else { 2 }
}

pub fn set_msaa(samples: i32) {
    let mut kept = kept();
    kept.remove(b"MW2_NO_MSAA".as_slice());
    kept.remove(b"MW2_MSAA".as_slice());
    if samples == 0 {
        kept.insert(b"MW2_NO_MSAA".to_vec(), b"1".to_vec());
    } else if samples > 2 {
        kept.insert(b"MW2_MSAA".to_vec(), samples.to_string().into_bytes());
    }
    save(&kept);
}

pub const MAX_SHADOW_SCALE: i32 = 4;

pub fn shadow_scale() -> i32 {
    number("MW2_SHADOW_SCALE", 1).clamp(1, MAX_SHADOW_SCALE)
}

pub fn set_shadow_scale(scale: i32) {
    change("MW2_SHADOW_SCALE", (scale > 1).then(|| scale.min(MAX_SHADOW_SCALE).to_string().into_bytes()));
}

pub fn anisotropy() -> i32 {
    let filtering = number("MW2_ANISOTROPY", 1);
    if matches!(filtering, 2 | 4 | 8 | 16) { filtering } else { 1 }
}

pub fn set_anisotropy(filtering: i32) {
    change("MW2_ANISOTROPY", (filtering > 1).then(|| filtering.to_string().into_bytes()));
}

// Off is the game's own way: the line goes.
pub fn stretch() -> bool {
    on("MW2_STRETCH")
}

pub fn set_stretch(on: bool) {
    change("MW2_STRETCH", on.then(|| b"1".to_vec()));
}

pub fn custom_maps() -> bool {
    on("MW2_CUSTOM_MAPS")
}

pub fn set_custom_maps(on: bool) {
    change("MW2_CUSTOM_MAPS", on.then(|| b"1".to_vec()));
}

/// MW2_LAUNCHER_SOUNDS: 0 keeps the launcher's menus silent. Written by hand.
pub fn sounds() -> bool {
    get("MW2_LAUNCHER_SOUNDS").is_none_or(|value| value != b"0")
}

/// MW2_PC_GAME: None when .env has no such line, empty when it says there is none.
pub fn pc_game() -> Option<Vec<u8>> {
    get("MW2_PC_GAME")
}

pub fn set_pc_game(folder: &[u8]) {
    set("MW2_PC_GAME", folder);
}

// ---- the network --------------------------------------------------------------------

/// none, the default, keeps system link on this machine; lan is the local network; server
/// a relay server; steam is Steam.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Network {
    None,
    Lan,
    Server,
    Steam,
}

const NETWORK_VALUES: [&str; 4] = ["none", "lan", "server", "steam"];

pub fn current_network() -> Network {
    let kept = get("MW2_NETWORK").unwrap_or_default();
    match NETWORK_VALUES.iter().position(|value| value.as_bytes() == kept.as_slice()) {
        Some(1) => Network::Lan,
        Some(2) => Network::Server,
        Some(3) => Network::Steam,
        _ => Network::None,
    }
}

// An empty value is the game's own way: the line goes.
fn keep(name: &str, value: &[u8]) {
    change(name, (!value.is_empty()).then(|| value.to_vec()));
}

pub fn set_network(network: Network) {
    keep("MW2_NETWORK", if network == Network::None { b"" } else { NETWORK_VALUES[network as usize].as_bytes() });
}

pub fn server() -> Vec<u8> {
    get("MW2_SERVER").unwrap_or_default()
}

pub fn set_server(address: &[u8]) {
    keep("MW2_SERVER", address);
}

pub fn server_password() -> Vec<u8> {
    get("MW2_SERVER_PASSWORD").unwrap_or_default()
}

pub fn set_server_password(password: &[u8]) {
    keep("MW2_SERVER_PASSWORD", password);
}
