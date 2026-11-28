// Pictures: the PC game's .iwi files, and the 360's tiled textures.
//
// A picture is a chain of mip levels, largest first, each one linear (rows of pixels or
// of 4x4 blocks, little-endian, as a DDS file stores them). The 360 keeps them tiled
// (Xenos' 2D tiling, the small levels packed into the last slice) and byte-swapped.

use crate::error::{Error, Result};
use crate::num::truncate32;
use crate::wavelet::decode_wavelet;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

pub type Mip = Vec<u8>;

/// The 360's texture formats (the fetch constant's low six bits).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Gpu {
    L8 = 0x02,
    Argb8 = 0x06,
    Dxt1 = 0x12,
    Dxt3 = 0x13,
    Dxt5 = 0x14,
    Dxn = 0x31,
    Dxt3a = 0x3A,
    Dxt5a = 0x3B,
}

pub struct Format {
    pub gpu: Gpu,
    pub name: &'static str,
    /// Pixels a block is wide (1: not block compressed).
    pub block_width: i32,
    pub block_bytes: i32,
}

const FORMATS: [Format; 8] = [
    Format { gpu: Gpu::Dxt1, name: "DXT1", block_width: 4, block_bytes: 8 },
    Format { gpu: Gpu::Dxt3, name: "DXT3", block_width: 4, block_bytes: 16 },
    Format { gpu: Gpu::Dxt5, name: "DXT5", block_width: 4, block_bytes: 16 },
    Format { gpu: Gpu::Dxn, name: "DXN", block_width: 4, block_bytes: 16 },
    Format { gpu: Gpu::Dxt5a, name: "DXT5A", block_width: 4, block_bytes: 8 },
    Format { gpu: Gpu::Argb8, name: "ARGB8", block_width: 1, block_bytes: 4 },
    Format { gpu: Gpu::L8, name: "L8", block_width: 1, block_bytes: 1 },
    Format { gpu: Gpu::Dxt3a, name: "DXT3A", block_width: 4, block_bytes: 8 },
];

pub fn format_of(g: Gpu) -> &'static Format {
    &FORMATS[match g {
        Gpu::Dxt1 => 0,
        Gpu::Dxt3 => 1,
        Gpu::Dxt5 => 2,
        Gpu::Dxn => 3,
        Gpu::Dxt5a => 4,
        Gpu::Argb8 => 5,
        Gpu::L8 => 6,
        Gpu::Dxt3a => 7,
    }]
}

pub fn format_named(name: &str) -> Option<Gpu> {
    FORMATS.iter().find(|f| f.name == name).map(|f| f.gpu)
}

fn u16_at(p: &[u8]) -> u32 {
    u32::from(p[0]) | u32::from(p[1]) << 8
}

fn u32_at(p: &[u8]) -> u32 {
    u16_at(p) | u16_at(&p[2..]) << 16
}

fn up(v: i32, d: i32) -> i32 {
    (v + d - 1) / d
}

fn pow2(v: i32) -> i32 {
    let mut r = 1;
    while r < v {
        r <<= 1;
    }
    r
}

fn log2_ceil(v: i32) -> i32 {
    let mut r = 0;
    let mut c = v - 1;
    while c > 0 {
        r += 1;
        c >>= 1;
    }
    r
}

fn mip_count(w: i32, h: i32) -> i32 {
    log2_ceil(w.max(h)) + 1
}

pub fn align_up(v: usize, a: usize) -> usize {
    v.wrapping_add(a - 1) / a * a
}

// ---- .iwi ---------------------------------------------------------------------------

const NO_MIPMAPS: u32 = 0x2;
const CUBE: u32 = 0x10000;

/// A picture read from an .iwi: its format, size and mips (for a cube map, the six
/// faces' top levels instead).
#[derive(Clone, Debug)]
pub struct Picture {
    pub format: Gpu,
    pub width: i32,
    pub height: i32,
    pub mips: Vec<Mip>,
    pub cube: bool,
}

impl Default for Picture {
    fn default() -> Picture {
        Picture { format: Gpu::Dxt1, width: 0, height: 0, mips: Vec::new(), cube: false }
    }
}

// The few uncompressed kinds without a 360 twin, made 32-bit: B, G, R, A.
fn rgb24_to_argb8(b: &[u8]) -> Vec<u8> {
    b.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 0xFF]).collect()
}

// Luminance, alpha -> grey with that alpha.
fn la16_to_argb8(b: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect();
    out.resize(b.len() * 2, 0);
    out
}

// Alpha only -> black with that alpha (what the PC's A8 samples as).
fn a8_to_argb8(b: &[u8]) -> Vec<u8> {
    b.iter().flat_map(|&a| [0, 0, 0, a]).collect()
}

// Luminance -> opaque grey.
fn l8_to_argb8(b: &[u8]) -> Vec<u8> {
    b.iter().flat_map(|&l| [l, l, l, 0xFF]).collect()
}

type Convert = fn(&[u8]) -> Vec<u8>;

struct Expand {
    bytes: usize,
    convert: Convert,
}

fn expand_of(fmt: u8) -> Option<Expand> {
    match fmt {
        0x02 => Some(Expand { bytes: 3, convert: rgb24_to_argb8 }),
        0x03 => Some(Expand { bytes: 2, convert: la16_to_argb8 }),
        0x04 => Some(Expand { bytes: 1, convert: a8_to_argb8 }),
        _ => None,
    }
}

fn compressed(fmt: u8) -> Option<Gpu> {
    match fmt {
        0x0B => Some(Gpu::Dxt1),
        0x0C => Some(Gpu::Dxt3),
        0x0D => Some(Gpu::Dxt5),
        0x01 => Some(Gpu::Argb8),
        _ => None,
    }
}

fn wavelet(fmt: u8) -> bool {
    (0x06..=0x0A).contains(&fmt)
}

/// A wavelet kind: the channels its stream has, and how they become 32-bit pixels (none:
/// B, G, R, A already).
fn wavelet_kind(fmt: u8) -> (i32, Option<Convert>) {
    match fmt {
        0x06 => (4, None),
        0x07 => (3, Some(rgb24_to_argb8)),
        0x08 => (2, Some(la16_to_argb8)),
        0x09 => (1, Some(l8_to_argb8)),
        _ => (1, Some(a8_to_argb8)),
    }
}

struct IwiHeader {
    /// As IW4 has them.
    flags: u32,
    format: u8,
    width: i32,
    height: i32,
    /// The header's own size.
    size: usize,
    /// The file's, as the header gives it.
    file_size: u32,
}

/// The header read as laid out in version 8 (IW4's own) or 6 (CoD4 / World at War's).
fn header_as(layout: u8, d: &[u8]) -> IwiHeader {
    if layout == 8 {
        return IwiHeader {
            flags: u32_at(&d[4..]),
            format: d[8],
            width: u16_at(&d[10..]) as i32,
            height: u16_at(&d[12..]) as i32,
            size: 32,
            file_size: u32_at(&d[16..]),
        };
    }
    let flags = (if d[5] & 2 != 0 { NO_MIPMAPS } else { 0 }) | (if d[5] & 4 != 0 { CUBE } else { 0 });
    IwiHeader {
        flags,
        format: d[4],
        width: u16_at(&d[6..]) as i32,
        height: u16_at(&d[8..]) as i32,
        size: 28,
        file_size: u32_at(&d[12..]),
    }
}

/// The PC game's and the 360's largest picture.
const LARGEST_SIDE: i32 = 8192;

/// Another version is read when its header is laid out as one of those and its file
/// size matches.
fn header(d: &[u8]) -> Result<IwiHeader> {
    if d.len() < 32 || &d[..3] != b"IWi" {
        return Err(Error::new("not an .iwi picture"));
    }
    let version = d[3];
    if version == 8 || version == 6 {
        let h = header_as(version, d);
        if h.width < 1 || h.height < 1 || h.width > LARGEST_SIDE || h.height > LARGEST_SIDE {
            return Err(Error::new(format!("an .iwi picture of {}x{}", h.width, h.height)));
        }
        return Ok(h);
    }
    for layout in [8, 6] {
        let h = header_as(layout, d);
        if u64::from(h.file_size) == d.len() as u64
            && h.width != 0
            && h.height != 0
            && h.width <= LARGEST_SIDE
            && h.height <= LARGEST_SIDE
            && (compressed(h.format).is_some() || expand_of(h.format).is_some() || wavelet(h.format))
        {
            return Ok(h);
        }
    }
    Err(Error::new(format!(".iwi version {version} isn't supported")))
}

pub fn read_iwi(data: &[u8]) -> Result<Picture> {
    let IwiHeader { flags, format: fmt, width: w, height: h, size: head, .. } = header(data)?;
    let mut pic = Picture::default();
    if wavelet(fmt) {
        // Every mip level decoded, then made 32-bit pixels.
        if flags & CUBE != 0 {
            return Err(Error::new("wavelet cube map pictures aren't supported yet"));
        }
        if flags & NO_MIPMAPS != 0 {
            return Err(Error::new("wavelet pictures without mipmaps aren't supported"));
        }
        let (channels, convert) = wavelet_kind(fmt);
        pic.format = Gpu::Argb8;
        pic.width = w;
        pic.height = h;
        for level in decode_wavelet(&data[head..], w, h, channels)? {
            pic.mips.push(match convert {
                Some(convert) => convert(&level),
                None => level,
            });
        }
        return Ok(pic);
    }
    let expand = expand_of(fmt);
    if expand.is_some() {
        pic.format = Gpu::Argb8;
    } else if let Some(g) = compressed(fmt) {
        pic.format = g;
    } else {
        return Err(Error::new(format!("image format {fmt} isn't supported yet")));
    }
    let f = format_of(pic.format);
    let bw = f.block_width;
    let bpb = expand.as_ref().map_or(f.block_bytes as usize, |e| e.bytes);
    let mut sizes: Vec<usize> = Vec::new();
    let (mut lw, mut lh) = (w, h);
    loop {
        sizes.push(up(lw, bw).max(1) as usize * up(lh, bw).max(1) as usize * bpb);
        if (lw == 1 && lh == 1) || flags & NO_MIPMAPS != 0 {
            break;
        }
        lw = (lw >> 1).max(1);
        lh = (lh >> 1).max(1);
    }
    let body = data.len() - head;
    pic.cube = flags & CUBE != 0;
    let faces = if pic.cube { 6 } else { 1 };
    let mut total: usize = sizes.iter().map(|s| s * faces).sum();
    if body < total {
        sizes.truncate(1);
        total = sizes[0] * faces;
    }
    if body < total {
        return Err(Error::new("an .iwi picture shorter than its size"));
    }
    // Smallest mip first in the file (each mip holds all faces of a cube map).
    let mut levels: Vec<Mip> = vec![Vec::new(); sizes.len()];
    let mut at = head + body - total;
    for i in (0..sizes.len()).rev() {
        levels[i] = data[at..at + sizes[i] * faces].to_vec();
        at += sizes[i] * faces;
    }
    if let Some(e) = &expand {
        for (level, size) in levels.iter_mut().zip(sizes.iter_mut()) {
            *level = (e.convert)(level);
            *size = *size / e.bytes * 4;
        }
    }
    pic.width = w;
    pic.height = h;
    if pic.cube {
        pic.mips = levels[0].chunks_exact(sizes[0]).take(6).map(<[u8]>::to_vec).collect();
    } else {
        pic.mips = levels;
    }
    Ok(pic)
}

// ---- tiling (Xenos, after xenia's xenos_texture) ------------------------------------

/// (width blocks, height blocks, stored width blocks, slice bytes) of a mip.
#[derive(Clone, Copy, Debug)]
pub struct MipLayout {
    pub wb: i32,
    pub hb: i32,
    pub sw: i32,
    pub slice: usize,
}

pub fn layout(width: i32, height: i32, mip: i32, g: Gpu) -> MipLayout {
    let f = format_of(g);
    let (bw, bpb) = (f.block_width, f.block_bytes);
    let wb = up((width >> mip).max(1), bw).max(1);
    let hb = up((height >> mip).max(1), bw).max(1);
    let (sw, sh, row);
    if mip == 0 {
        let pitch =
            if bw > 1 { align_up(wb as usize, 32) as i32 / 8 } else { align_up(width as usize, 32) as i32 / 32 };
        row = up(pitch << 5, bw).max(1) as usize * bpb as usize;
        sw = (row / bpb as usize) as i32;
        sh = align_up(hb as usize, 32) as i32;
    } else {
        sw = align_up(up((pow2(width) >> mip).max(1), bw) as usize, 32) as i32;
        sh = align_up(up((pow2(height) >> mip).max(1), bw) as usize, 32) as i32;
        row = sw as usize * bpb as usize;
    }
    MipLayout { wb, hb, sw, slice: align_up(row * sh as usize, 4096) }
}

fn l2(bpb: i32) -> i32 {
    (bpb / 4) + ((bpb / 2) >> (bpb / 4))
}

fn block_offset(x: i32, y: i32, sw: i32, bpb: i32) -> usize {
    let l2 = l2(bpb);
    let mut macro_ = i64::from((y / 32).wrapping_mul(sw / 32)) << (l2 + 7);
    let mut micro = i64::from((y & 6) << 2) << l2;
    let base = macro_ + ((micro & !0xF) << 1) + (micro & 0xF) + i64::from((y & 8) << (3 + l2)) + i64::from((y & 1) << 4);
    macro_ = i64::from(x / 32) << (l2 + 7);
    micro = i64::from(x & 7) << l2;
    let mut off = base + macro_ + ((micro & !0xF) << 1) + (micro & 0xF);
    off = ((off & !0x1FF) << 3)
        + ((off & 0x1C0) << 2)
        + (off & 0x3F)
        + i64::from((y & 16) << 7)
        + i64::from(((((y & 8) >> 2) + (x >> 3)) & 3) << 6);
    ((off >> l2) as usize).wrapping_mul(bpb as usize)
}

/// Tiled byte offset of every block of a mip, row by row (kept: many pictures share a
/// size).
fn offsets(wb: i32, hb: i32, ox: i32, oy: i32, sw: i32, bpb: i32) -> Arc<Vec<usize>> {
    type Grid = (i32, i32, i32, i32, i32, i32);
    static CACHE: Mutex<BTreeMap<Grid, Arc<Vec<usize>>>> = Mutex::new(BTreeMap::new());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry((wb, hb, ox, oy, sw, bpb))
        .or_insert_with(|| {
            let mut offs = Vec::with_capacity(wb.max(0) as usize * hb.max(0) as usize);
            for y in 0..hb {
                for x in 0..wb {
                    offs.push(block_offset(x + ox, y + oy, sw, bpb));
                }
            }
            Arc::new(offs)
        })
        .clone()
}

/// A mip's place in a tiled chain: the mip, its slice's offset, its stored width and
/// its block offsets x and y.
#[derive(Clone, Copy, Debug)]
pub struct MipPlace {
    pub mip: i32,
    pub base: usize,
    pub sw: i32,
    pub ox: i32,
    pub oy: i32,
}

/// Each mip's place in a tiled chain, and the chain's size.
pub fn plan(width: i32, height: i32, g: Gpu) -> (Vec<MipPlace>, usize) {
    let bw = format_of(g).block_width;
    let tail = (log2_ceil(width.min(height)) - 4).max(0);
    let mut offsets = Vec::new();
    let mut total = 0;
    for mip in 0..=tail {
        offsets.push(total);
        total += layout(width, height, mip, g).slice;
    }
    let tail_sw = layout(width, height, tail, g).sw;
    let wide = log2_ceil(width) > log2_ceil(height);
    let mut plan = Vec::new();
    let count = mip_count(width, height);
    for mip in 0..count {
        if mip < tail {
            plan.push(MipPlace { mip, base: offsets[mip as usize], sw: layout(width, height, mip, g).sw, ox: 0, oy: 0 });
            continue;
        }
        let packed = mip - tail;
        let (ox, oy);
        if packed < 3 {
            ox = if wide { 0 } else { (16 >> packed) / bw };
            oy = if wide { (16 >> packed) / bw } else { 0 };
        } else {
            // The smallest mips sit one block each along the long side, 1x1 nearest the
            // corner.
            let step = 1 << (count - 1 - mip);
            ox = if wide { step } else { 0 };
            oy = if wide { 0 } else { step };
        }
        plan.push(MipPlace { mip, base: offsets[tail as usize], sw: tail_sw, ox, oy });
    }
    (plan, total)
}

/// The console's byte order: 16-bit swaps for block formats, 32-bit for ARGB8.
pub fn swap(data: &[u8], g: Gpu) -> Vec<u8> {
    let f = format_of(g);
    let mut b = vec![0u8; data.len()];
    if f.block_bytes == 4 && f.block_width == 1 {
        for (o, i) in b.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
            o.copy_from_slice(&[i[3], i[2], i[1], i[0]]);
        }
    } else {
        for (o, i) in b.chunks_exact_mut(2).zip(data.chunks_exact(2)) {
            o.copy_from_slice(&[i[1], i[0]]);
        }
    }
    b
}

/// Linear mips -> one tiled level chunk, as the 360 keeps it. `single`: the top mip
/// alone, in a slice of its own.
pub fn tile(mips: &[Mip], width: i32, height: i32, g: Gpu, single: bool) -> Vec<u8> {
    let bpb = format_of(g).block_bytes;
    let (plan, total) = if single {
        let l = layout(width, height, 0, g);
        (vec![MipPlace { mip: 0, base: 0, sw: l.sw, ox: 0, oy: 0 }], l.slice)
    } else {
        plan(width, height, g)
    };
    let mut blob = vec![0u8; total];
    for (p, data) in plan.iter().zip(mips) {
        let l = layout(width, height, p.mip, g);
        let mut src = 0usize;
        for &off in offsets(l.wb, l.hb, p.ox, p.oy, p.sw, bpb).iter() {
            let dst = p.base.wrapping_add(off);
            let n = (bpb as usize).min(data.len().saturating_sub(src));
            if n > 0
                && let Some(to) = dst.checked_add(n).and_then(|end| blob.get_mut(dst..end))
            {
                to.copy_from_slice(&data[src..src + n]);
            }
            src += bpb as usize;
        }
    }
    swap(&blob, g)
}

// ---- conversions --------------------------------------------------------------------

/// A PC DXT5 normal map (X in alpha, Y in green) as 360 DXN: the alpha block as it is,
/// then green as a second block.
pub fn normal_maps_to_dxn(mips: &[Mip]) -> Vec<Mip> {
    // Green block -> its DXN block: normal maps repeat many blocks.
    let mut done: HashMap<[u8; 8], [u8; 8]> = HashMap::new();
    let mut out = Vec::new();
    for mip in mips {
        let mut o = vec![0u8; mip.len()];
        for (to, from) in o.chunks_exact_mut(16).zip(mip.chunks_exact(16)) {
            let green: [u8; 8] = from[8..16].try_into().unwrap_or_default();
            let block = done.entry(green).or_insert_with(|| {
                let (c0, c1, bits) = (u16_at(&green), u16_at(&green[2..]), u32_at(&green[4..]));
                let g0 = ((c0 >> 5) & 63) as i32 * 255 / 63;
                let g1 = ((c1 >> 5) & 63) as i32 * 255 / 63;
                let pal = if c0 > c1 {
                    [g0, g1, (2 * g0 + g1) / 3, (g0 + 2 * g1) / 3]
                } else {
                    [g0, g1, (g0 + g1) / 2, 0]
                };
                alpha_block(&std::array::from_fn(|k| pal[((bits >> (2 * k)) & 3) as usize]))
            });
            to[..8].copy_from_slice(&from[..8]);
            to[8..].copy_from_slice(block);
        }
        out.push(o);
    }
    out
}

/// 8-bit single channel -> DXT3A blocks (4 bits a pixel), linear.
pub fn encode_dxt3a(lum: &[u8], width: i32, height: i32) -> Mip {
    let bw = (width / 4).max(1);
    let bh = (height / 4).max(1);
    let mut out = vec![0u8; bw as usize * bh as usize * 8];
    let q: [u8; 256] = std::array::from_fn(|v| ((v as i32 * 15 + 127) / 255).min(15) as u8);
    for by in 0..bh {
        for bx in 0..bw {
            let o = (by as usize * bw as usize + bx as usize) * 8;
            for y in 0..4 {
                let row = ((by * 4 + y) as usize).wrapping_mul(width as usize).wrapping_add((bx * 4) as usize);
                let px = |k: usize| q[usize::from(lum.get(row.wrapping_add(k)).copied().unwrap_or(0))];
                out[o + 2 * y as usize] = px(0) | (px(1) << 4);
                out[o + 2 * y as usize + 1] = px(2) | (px(3) << 4);
            }
        }
    }
    out
}

/// What a reflection probe's brightness multiplier, in its alpha, goes up to.
pub const PROBE_SCALE: f64 = 2.5;

/// A reflection probe mip (B, G, R, A) as plain colour: the PC keeps a brightness
/// multiplier in alpha.
pub fn decode_probe(bgra: &[u8]) -> Mip {
    let mut out = bgra.to_vec();
    let mut lut: Vec<Option<[u8; 256]>> = vec![None; 256];
    for o in out.chunks_exact_mut(4) {
        let a = o[3];
        let t = lut[usize::from(a)].get_or_insert_with(|| {
            let k = f64::from(a) / 255.0 * PROBE_SCALE;
            std::array::from_fn(|v| (truncate32(v as f64 * k + 0.5)).min(255) as u8)
        });
        for c in &mut o[..3] {
            *c = t[usize::from(*c)];
        }
    }
    out
}

// ---- the DXT encoder ----------------------------------------------------------------

type Pixel = [i32; 4];

fn rgb565(c: &Pixel) -> u32 {
    (((c[0] * 31 + 127) / 255) << 11 | ((c[1] * 63 + 127) / 255) << 5 | ((c[2] * 31 + 127) / 255)) as u32
}

fn unpack565(v: u32) -> [i32; 3] {
    let r = (v >> 11) as i32 & 31;
    let g = (v >> 5) as i32 & 63;
    let b = v as i32 & 31;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

fn put(out: &mut Mip, c0: u32, c1: u32, bits: u32) {
    out.extend_from_slice(&(c0 as u16).to_le_bytes());
    out.extend_from_slice(&(c1 as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
}

/// 16 RGBA pixels -> an 8-byte DXT1-style colour block: the extremes along the colours'
/// principal axis.
fn color_block(pixels: &[Pixel; 16], punch_through: bool, out: &mut Mip) {
    let mut used: Vec<&Pixel> = pixels.iter().filter(|p| !(punch_through && p[3] < 128)).collect();
    if used.is_empty() {
        used = pixels.iter().collect();
    }
    let n = used.len() as f64;
    let mean: [f64; 3] = std::array::from_fn(|i| used.iter().map(|p| i64::from(p[i])).sum::<i64>() as f64 / n);
    let mut cov = [[0.0f64; 3]; 3];
    for (i, row) in cov.iter_mut().enumerate() {
        for (j, c) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for p in &used {
                sum += (f64::from(p[i]) - mean[i]) * (f64::from(p[j]) - mean[j]);
            }
            *c = sum;
        }
    }
    let mut axis = [1.0f64; 3];
    for _ in 0..4 {
        let next: [f64; 3] = std::array::from_fn(|i| {
            let mut sum = 0.0;
            for j in 0..3 {
                sum += cov[i][j] * axis[j];
            }
            sum
        });
        let mut norm = next[0].abs();
        for v in &next[1..] {
            if norm < v.abs() {
                norm = v.abs();
            }
        }
        if norm == 0.0 {
            norm = 1.0;
        }
        for i in 0..3 {
            axis[i] = next[i] / norm;
        }
    }
    let proj: Vec<f64> = used
        .iter()
        .map(|p| {
            let mut sum = 0.0;
            for i in 0..3 {
                sum += (f64::from(p[i]) - mean[i]) * axis[i];
            }
            sum
        })
        .collect();
    let (mut lo, mut hi) = (0, 0);
    for k in 1..proj.len() {
        if proj[k] < proj[lo] {
            lo = k;
        }
        if proj[k] > proj[hi] {
            hi = k;
        }
    }
    let mut c0 = rgb565(used[hi]);
    let mut c1 = rgb565(used[lo]);
    let transparent = punch_through && pixels.iter().any(|p| p[3] < 128);
    if transparent {
        if c0 > c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
    } else {
        if c0 < c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
        if c0 == c1 {
            put(out, c0, c1, 0);
            return;
        }
    }
    let a = unpack565(c0);
    let b = unpack565(c1);
    let mut palette = vec![a, b];
    if transparent || c0 <= c1 {
        palette.push(std::array::from_fn(|c| (a[c] + b[c]) / 2));
    } else {
        palette.push(std::array::from_fn(|c| (2 * a[c] + b[c]) / 3));
        palette.push(std::array::from_fn(|c| (a[c] + 2 * b[c]) / 3));
    }
    let mut bits = 0u32;
    for (k, p) in pixels.iter().enumerate() {
        let mut index = 0u32;
        if transparent && p[3] < 128 {
            index = 3;
        } else {
            let mut best = -1;
            for (i, e) in palette.iter().enumerate() {
                let d: i32 = (0..3).map(|c| (p[c] - e[c]) * (p[c] - e[c])).sum();
                if best < 0 || d < best {
                    best = d;
                    index = i as u32;
                }
            }
        }
        bits |= index << (2 * k);
    }
    put(out, c0, c1, bits);
}

/// 16 values (0-255) -> an 8-byte DXT5 / BC4 alpha block, an 8-step ramp between the
/// extremes.
fn alpha_block(values: &[i32; 16]) -> [u8; 8] {
    let a0 = values.iter().copied().max().unwrap_or(0);
    let a1 = values.iter().copied().min().unwrap_or(0);
    let mut out = [a0 as u8, a1 as u8, 0, 0, 0, 0, 0, 0];
    if a0 == a1 {
        return out;
    }
    let mut palette = [a0, a1, 0, 0, 0, 0, 0, 0];
    for i in 1..7 {
        palette[i as usize + 1] = ((7 - i) * a0 + i * a1) / 7;
    }
    let mut bits = 0u64;
    for (k, &v) in values.iter().enumerate() {
        let mut best = 0;
        for i in 1..8 {
            if (v - palette[i]).abs() < (v - palette[best]).abs() {
                best = i;
            }
        }
        bits |= (best as u64) << (3 * k);
    }
    out[2..].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
}

/// RGBA pixels -> a level in a format.
pub fn encode_pixels(rgba: &[u8], width: i32, height: i32, g: Gpu) -> Result<Mip> {
    let mut out = Mip::new();
    if g == Gpu::Argb8 {
        out = rgba.to_vec();
        for p in out.chunks_exact_mut(4) {
            p.swap(0, 2); // RGBA -> BGRA
        }
        return Ok(out);
    }
    for by in (0..height.max(1)).step_by(4) {
        for bx in (0..width.max(1)).step_by(4) {
            let pixels: [Pixel; 16] = std::array::from_fn(|k| {
                let (x, y) = (k as i32 % 4, k as i32 / 4);
                let i = ((by + y).min(height - 1) as usize * width as usize + (bx + x).min(width - 1) as usize) * 4;
                std::array::from_fn(|c| i32::from(rgba[i + c]))
            });
            let channel = |c: usize| -> [i32; 16] { std::array::from_fn(|k| pixels[k][c]) };
            match g {
                Gpu::Dxt1 => color_block(&pixels, true, &mut out),
                Gpu::Dxt3 => {
                    let mut alpha = 0u64;
                    for (k, p) in pixels.iter().enumerate() {
                        alpha |= (((p[3] * 15 + 127) / 255) as u64) << (4 * k);
                    }
                    out.extend_from_slice(&alpha.to_le_bytes());
                    color_block(&pixels, false, &mut out);
                }
                Gpu::Dxt5 => {
                    out.extend_from_slice(&alpha_block(&channel(3)));
                    color_block(&pixels, false, &mut out);
                }
                Gpu::Dxt5a => out.extend_from_slice(&alpha_block(&channel(0))),
                Gpu::Dxn => {
                    out.extend_from_slice(&alpha_block(&channel(0)));
                    out.extend_from_slice(&alpha_block(&channel(1)));
                }
                _ => return Err(Error::new("no encoder for this picture format")),
            }
        }
    }
    Ok(out)
}

// ---- Pillow: decoding and resizing --------------------------------------------------

// Pillow's bcn decoder (libImaging/BcnDecode.c).
fn decode565(v: u32) -> [u8; 4] {
    let mut r = ((v & 0xf800) >> 8) as u8;
    r |= r >> 5;
    let mut g = ((v & 0x7e0) >> 3) as u8;
    g |= g >> 6;
    let mut b = ((v & 0x1f) << 3) as u8;
    b |= b >> 5;
    [r, g, b, 0xff]
}

fn decode_color(dst: &mut [[u8; 4]; 16], src: &[u8], separate_alpha: bool) {
    let (c0, c1, lut) = (u16_at(src), u16_at(&src[2..]), u32_at(&src[4..]));
    let p0 = decode565(c0);
    let p1 = decode565(c1);
    let (r0, g0, b0) = (i32::from(p0[0]), i32::from(p0[1]), i32::from(p0[2]));
    let (r1, g1, b1) = (i32::from(p1[0]), i32::from(p1[1]), i32::from(p1[2]));
    let (p2, p3) = if c0 > c1 || separate_alpha {
        (
            [((2 * r0 + r1) / 3) as u8, ((2 * g0 + g1) / 3) as u8, ((2 * b0 + b1) / 3) as u8, 0xff],
            [((r0 + 2 * r1) / 3) as u8, ((g0 + 2 * g1) / 3) as u8, ((b0 + 2 * b1) / 3) as u8, 0xff],
        )
    } else {
        ([((r0 + r1) / 2) as u8, ((g0 + g1) / 2) as u8, ((b0 + b1) / 2) as u8, 0xff], [0, 0, 0, 0])
    };
    let p = [p0, p1, p2, p3];
    for (n, d) in dst.iter_mut().enumerate() {
        *d = p[((lut >> (2 * n)) & 3) as usize];
    }
}

fn decode_bc3_alpha(dst: &mut [[u8; 4]; 16], src: &[u8]) {
    let (a0, a1) = (i32::from(src[0]), i32::from(src[1]));
    let mut a = [a0 as u8, a1 as u8, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for i in 1..7 {
            a[i as usize + 1] = (((7 - i) * a0 + i * a1) / 7) as u8;
        }
    } else {
        for i in 1..5 {
            a[i as usize + 1] = (((5 - i) * a0 + i * a1) / 5) as u8;
        }
        a[6] = 0;
        a[7] = 0xff;
    }
    let lut1 = u32::from(src[2]) | u32::from(src[3]) << 8 | u32::from(src[4]) << 16;
    let lut2 = u32::from(src[5]) | u32::from(src[6]) << 8 | u32::from(src[7]) << 16;
    for n in 0..8 {
        dst[n][3] = a[((lut1 >> (3 * n)) & 7) as usize];
        dst[8 + n][3] = a[((lut2 >> (3 * n)) & 7) as usize];
    }
}

/// A level decoded to RGBA as Pillow decodes it.
pub fn decode_pixels(level: &[u8], width: i32, height: i32, g: Gpu) -> Result<Vec<u8>> {
    let mut out = vec![0u8; width as usize * height as usize * 4];
    if g == Gpu::Argb8 {
        for (o, i) in out.chunks_exact_mut(4).zip(level.chunks_exact(4)) {
            o.copy_from_slice(&[i[2], i[1], i[0], i[3]]);
        }
        return Ok(out);
    }
    let block_bytes = format_of(g).block_bytes as usize;
    let (bw, bh) = (up(width, 4), up(height, 4));
    for by in 0..bh {
        for bx in 0..bw {
            let at = (by as usize * bw as usize + bx as usize) * block_bytes;
            let Some(src) = level.get(at..at + block_bytes) else { continue };
            let mut col = [[0u8; 4]; 16];
            match g {
                Gpu::Dxt1 => decode_color(&mut col, src, false),
                Gpu::Dxt3 => {
                    decode_color(&mut col, &src[8..], true);
                    for (n, c) in col.iter_mut().enumerate() {
                        let bit = n * 4;
                        let av = 0xf & (src[bit >> 3] >> (bit & 7));
                        c[3] = (av << 4) | av;
                    }
                }
                Gpu::Dxt5 => {
                    decode_color(&mut col, &src[8..], true);
                    decode_bc3_alpha(&mut col, src);
                }
                _ => return Err(Error::new("no decoder for this picture format")),
            }
            for (n, c) in col.iter().enumerate() {
                let x = bx * 4 + n as i32 % 4;
                let y = by * 4 + n as i32 / 4;
                if x < width && y < height {
                    let o = (y as usize * width as usize + x as usize) * 4;
                    out[o..o + 4].copy_from_slice(c);
                }
            }
        }
    }
    Ok(out)
}

// Pillow's resampling (libImaging/Resample.c): coefficients in doubles, applied in fixed
// point, horizontally then vertically.
const PRECISION: i32 = 32 - 8 - 2;

fn sinc(mut x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    x *= std::f64::consts::PI;
    x.sin() / x
}

fn lanczos(x: f64) -> f64 {
    if (-3.0..3.0).contains(&x) { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

struct Coeffs {
    ksize: usize,
    /// (min, count) per output pixel.
    bounds: Vec<i32>,
    kk: Vec<i32>,
}

fn precompute(in_size: i32, in0: f64, in1: f64, out_size: i32) -> Coeffs {
    let scale = (in1 - in0) / f64::from(out_size);
    let filterscale = scale.max(1.0);
    let support = 3.0 * filterscale;
    let ksize = (truncate32(support.ceil()) * 2 + 1) as usize;
    let mut bounds = vec![0; out_size as usize * 2];
    let mut kk = vec![0.0f64; out_size as usize * ksize];
    for xx in 0..out_size {
        let center = in0 + (f64::from(xx) + 0.5) * scale;
        let mut ww = 0.0;
        let ss = 1.0 / filterscale;
        let xmin = truncate32(center - support + 0.5).max(0);
        let xmax = truncate32(center + support + 0.5).min(in_size) - xmin;
        let k = &mut kk[xx as usize * ksize..][..xmax.max(0) as usize];
        for (x, kx) in (xmin..).zip(k.iter_mut()) {
            let w = lanczos((f64::from(x) - center + 0.5) * ss);
            *kx = w;
            ww += w;
        }
        if ww != 0.0 {
            for w in k.iter_mut() {
                *w /= ww;
            }
        }
        bounds[xx as usize * 2] = xmin;
        bounds[xx as usize * 2 + 1] = xmax;
    }
    let one = f64::from(1 << PRECISION);
    let kk = kk.iter().map(|&k| if k < 0.0 { truncate32(-0.5 + k * one) } else { truncate32(0.5 + k * one) }).collect();
    Coeffs { ksize, bounds, kk }
}

fn clip8(v: i64) -> u8 {
    if v >= (1i64 << PRECISION << 8) {
        255
    } else if v <= 0 {
        0
    } else {
        (v >> PRECISION) as u8
    }
}

// MULDIV255, for Pillow's RGBA <-> RGBa (premultiplied) conversions.
fn mul_div255(a: u32, b: u32) -> u8 {
    let tmp = a * b + 128;
    (((tmp >> 8) + tmp) >> 8) as u8
}

/// One output sample of a pass: the fixed-point sum of the taps, as Pillow rounds it.
fn convolve(taps: impl Iterator<Item = (u8, i32)>) -> u8 {
    let mut ss = 1i64 << (PRECISION - 1);
    for (px, k) in taps {
        ss += i64::from(i32::from(px).wrapping_mul(k));
    }
    clip8(i64::from(ss as i32))
}

/// Pillow's Image.resize(size, LANCZOS) of an RGBA picture.
pub fn resize_lanczos(rgba: &[u8], width: i32, height: i32, to_width: i32, to_height: i32) -> Result<Vec<u8>> {
    if width == to_width && height == to_height {
        return Ok(rgba.to_vec());
    }
    // A negative size has no coefficients to make, and an empty one no rows to take their
    // bounds from.
    if to_width < 0 || to_height < 0 {
        return Err(Error::new("vector::_M_default_append"));
    }
    if to_width == 0 || to_height == 0 {
        return Err(Error::new(format!("a picture can't be resized to {to_width}x{to_height}")));
    }
    // RGBA -> RGBa
    let mut input = vec![0u8; rgba.len()];
    for (o, i) in input.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        let alpha = u32::from(i[3]);
        for c in 0..3 {
            o[c] = mul_div255(u32::from(i[c]), alpha);
        }
        o[3] = i[3];
    }
    let h = precompute(width, 0.0, f64::from(width), to_width);
    let v = precompute(height, 0.0, f64::from(height), to_height);
    let need_h = to_width != width;
    let need_v = to_height != height;
    let mut vb = v.bounds.clone();
    let rows_out = to_height as usize;
    let y_first = vb[0];
    let y_last = vb[rows_out * 2 - 2] + vb[rows_out * 2 - 1];
    let mut cur_w = width as usize;
    let mut cur = input;
    if need_h {
        for i in 0..rows_out {
            vb[i * 2] -= y_first;
        }
        let rows = (y_last - y_first) as usize;
        let to_w = to_width as usize;
        let mut tmp = vec![0u8; to_w * rows * 4];
        for yy in 0..rows {
            let line = (yy + y_first as usize) * cur_w;
            for xx in 0..to_w {
                let xmin = h.bounds[xx * 2] as usize;
                let xmax = h.bounds[xx * 2 + 1] as usize;
                let k = &h.kk[xx * h.ksize..][..xmax];
                for c in 0..4 {
                    tmp[(yy * to_w + xx) * 4 + c] =
                        convolve(k.iter().enumerate().map(|(x, &k)| (cur[(line + x + xmin) * 4 + c], k)));
                }
            }
        }
        cur = tmp;
        cur_w = to_w;
    }
    if need_v {
        let mut tmp = vec![0u8; cur_w * rows_out * 4];
        for yy in 0..rows_out {
            let ymin = vb[yy * 2] as usize;
            let ymax = vb[yy * 2 + 1] as usize;
            let k = &v.kk[yy * v.ksize..][..ymax];
            for xx in 0..cur_w {
                for c in 0..4 {
                    tmp[(yy * cur_w + xx) * 4 + c] =
                        convolve(k.iter().enumerate().map(|(y, &k)| (cur[((y + ymin) * cur_w + xx) * 4 + c], k)));
                }
            }
        }
        cur = tmp;
    }
    // RGBa -> RGBA
    for p in cur.chunks_exact_mut(4) {
        let alpha = u32::from(p[3]);
        if alpha != 255 && alpha != 0 {
            for c in &mut p[..3] {
                *c = (255 * u32::from(*c) / alpha).min(255) as u8;
            }
        }
    }
    Ok(cur)
}

/// The size a picture the 360 can't take is resized to: each side down to a power of
/// two, at most `limit`.
pub fn picture_size(width: i32, height: i32, limit: i32) -> (i32, i32) {
    let p2 = |v: i32| {
        let mut r = 1;
        while r * 2 <= v {
            r *= 2;
        }
        r.min(limit)
    };
    (p2(width), p2(height))
}

/// A picture resized: its size and its levels.
pub struct Fitted {
    pub width: i32,
    pub height: i32,
    pub mips: Vec<Mip>,
}

/// Pictures the 360 can't take as they are, resized: decoded, Lanczos-resampled with
/// premultiplied alpha (as Pillow resamples), compressed again, top level only.
pub fn fit_picture(f: Gpu, width: i32, height: i32, mips: &[Mip], limit: i32) -> Result<Fitted> {
    let (nw, nh) = picture_size(width, height, limit);
    if nw == width && nh == height {
        return Ok(Fitted { width, height, mips: mips.to_vec() });
    }
    let Some(top) = mips.first() else {
        return Err(Error::new("vector::_M_range_check: __n (which is 0) >= this->size() (which is 0)"));
    };
    let rgba = decode_pixels(top, width, height, f)?;
    let resized = resize_lanczos(&rgba, width, height, nw, nh)?;
    Ok(Fitted { width: nw, height: nh, mips: vec![encode_pixels(&resized, nw, nh, f)?] })
}
