// Xenos texture fetch constants, layouts and the untiler (runtime/gpu/texture.h).
//
// Six dwords per slot, thirty-two slots, in the register file at 0x4800. A shader's
// `tfetch` names a slot; everything about the texture comes from here rather than
// from the instruction. Field layout follows Xenia's `xe_gpu_texture_fetch_t`
// (BSD-3). The arithmetic is the C++'s, in its integer widths: a port that must
// give the same bytes.
//
// Unsafe by nature: read_texture copies from the two guest ranges its caller names, by
// raw pointer, as the texture cache gives them (host addresses of guest memory, which
// guest threads may write meanwhile, so never a slice over them). The contract: each
// pointer is null or readable as far as the layout reaches (which can be past what
// extents_of names, as the C++'s can: see tools/gpu_diff/texture_reach.cpp), and
// `reserve`/`rgba` return that many writable zeroed bytes that nothing else touches.
// Writes stay inside the reserved buffer, a row at a time, asserted.
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::CStr;
use std::ptr;

pub const DIMENSION_1D: u32 = 0;
pub const DIMENSION_3D: u32 = 2;
pub const DIMENSION_CUBE: u32 = 3;

pub const ENDIAN_NONE: u32 = 0;
pub const ENDIAN_8IN16: u32 = 1;
pub const ENDIAN_8IN32: u32 = 2;
pub const ENDIAN_16IN32: u32 = 3;

#[expect(dead_code, reason = "the fetch's other readers are C++ until the renderer moves (S5 on)")]
pub const SIGN_GAMMA: u32 = 3;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct TextureFetch {
    pub d: [u32; 6],
}

#[inline]
fn bits(w: u32, lo: u32, count: u32) -> u32 {
    (w >> lo) & if count >= 32 { u32::MAX } else { (1 << count) - 1 }
}

#[expect(dead_code, reason = "the fetch's other readers are C++ until the renderer moves (S5 on)")]
impl TextureFetch {
    pub fn texture_type(&self) -> u32 {
        bits(self.d[0], 0, 2)
    }
    pub fn is_texture(&self) -> bool {
        self.texture_type() == 2
    }
    /// TEX_FORMAT_COMP, one per component; 3 is gamma, which the sampler linearises.
    pub fn sign(&self, component: u32) -> u32 {
        bits(self.d[0], 2 + component * 2, 2)
    }
    /// Colour only: the alpha channel of a gamma surface stays linear.
    pub fn linearises(&self) -> bool {
        self.sign(0) == SIGN_GAMMA || self.sign(1) == SIGN_GAMMA || self.sign(2) == SIGN_GAMMA
    }
    /// Row pitch in pixels >> 5.
    pub fn pitch(&self) -> u32 {
        bits(self.d[0], 22, 9) << 5
    }
    pub fn tiled(&self) -> bool {
        bits(self.d[0], 31, 1) != 0
    }
    pub fn format(&self) -> u32 {
        bits(self.d[1], 0, 6)
    }
    pub fn endian(&self) -> u32 {
        bits(self.d[1], 6, 2)
    }
    pub fn stacked(&self) -> bool {
        bits(self.d[1], 10, 1) != 0
    }
    pub fn base_address(&self) -> u32 {
        bits(self.d[1], 12, 20) << 12
    }
    /// Sizes are stored with one subtracted from each component.
    pub fn width(&self) -> u32 {
        match self.dimension() {
            DIMENSION_1D => bits(self.d[2], 0, 24) + 1,
            DIMENSION_3D => bits(self.d[2], 0, 11) + 1,
            _ => bits(self.d[2], 0, 13) + 1,
        }
    }
    pub fn height(&self) -> u32 {
        match self.dimension() {
            DIMENSION_1D => 1,
            DIMENSION_3D => bits(self.d[2], 11, 11) + 1,
            _ => bits(self.d[2], 13, 13) + 1,
        }
    }
    pub fn depth(&self) -> u32 {
        match self.dimension() {
            DIMENSION_3D => bits(self.d[2], 22, 10) + 1,
            DIMENSION_CUBE => 6,
            _ if self.stacked() => bits(self.d[2], 26, 6) + 1,
            _ => 1,
        }
    }
    pub fn clamp_x(&self) -> u32 {
        bits(self.d[0], 10, 3)
    }
    pub fn clamp_y(&self) -> u32 {
        bits(self.d[0], 13, 3)
    }
    pub fn clamp_z(&self) -> u32 {
        bits(self.d[0], 16, 3)
    }
    pub fn swizzle(&self) -> u32 {
        bits(self.d[3], 1, 12)
    }
    pub fn mag_filter(&self) -> u32 {
        bits(self.d[3], 19, 2)
    }
    pub fn min_filter(&self) -> u32 {
        bits(self.d[3], 21, 2)
    }
    pub fn mip_filter(&self) -> u32 {
        bits(self.d[3], 23, 2)
    }
    pub fn anisotropy(&self) -> u32 {
        bits(self.d[3], 25, 3)
    }
    pub fn border_colour(&self) -> u32 {
        bits(self.d[5], 0, 2)
    }
    pub fn min_mip_level(&self) -> u32 {
        bits(self.d[4], 2, 4)
    }
    pub fn max_mip_level(&self) -> u32 {
        bits(self.d[4], 6, 4)
    }
    pub fn dimension(&self) -> u32 {
        bits(self.d[5], 9, 2)
    }
    pub fn packed_mips(&self) -> bool {
        bits(self.d[5], 11, 1) != 0
    }
    pub fn mip_address(&self) -> u32 {
        bits(self.d[5], 12, 20) << 12
    }
}

struct Entry {
    name: Option<&'static CStr>,
    bits: u32,
    bw: u32,
    bh: u32,
}

const fn e(name: &'static CStr, bits: u32, bw: u32, bh: u32) -> Entry {
    Entry { name: Some(name), bits, bw, bh }
}

// A missing name is a value the hardware does not define; there are none in the 64,
// but the C++ table allows them and so does this one.
static FORMATS: [Entry; 64] = [
    e(c"1_REVERSE", 1, 1, 1),
    e(c"1", 1, 1, 1),
    e(c"8", 8, 1, 1),
    e(c"1_5_5_5", 16, 1, 1),
    e(c"5_6_5", 16, 1, 1),
    e(c"6_5_5", 16, 1, 1),
    e(c"8_8_8_8", 32, 1, 1),
    e(c"2_10_10_10", 32, 1, 1),
    e(c"8_A", 8, 1, 1),
    e(c"8_B", 8, 1, 1),
    e(c"8_8", 16, 1, 1),
    e(c"Cr_Y1_Cb_Y0_REP", 16, 2, 1),
    e(c"Y1_Cr_Y0_Cb_REP", 16, 2, 1),
    e(c"16_16_EDRAM", 32, 1, 1),
    e(c"8_8_8_8_A", 32, 1, 1),
    e(c"4_4_4_4", 16, 1, 1),
    e(c"10_11_11", 32, 1, 1),
    e(c"11_11_10", 32, 1, 1),
    e(c"DXT1", 64, 4, 4),
    e(c"DXT2_3", 128, 4, 4),
    e(c"DXT4_5", 128, 4, 4),
    e(c"16_16_16_16_EDRAM", 64, 1, 1),
    e(c"24_8", 32, 1, 1),
    e(c"24_8_FLOAT", 32, 1, 1),
    e(c"16", 16, 1, 1),
    e(c"16_16", 32, 1, 1),
    e(c"16_16_16_16", 64, 1, 1),
    e(c"16_EXPAND", 16, 1, 1),
    e(c"16_16_EXPAND", 32, 1, 1),
    e(c"16_16_16_16_EXPAND", 64, 1, 1),
    e(c"16_FLOAT", 16, 1, 1),
    e(c"16_16_FLOAT", 32, 1, 1),
    e(c"16_16_16_16_FLOAT", 64, 1, 1),
    e(c"32", 32, 1, 1),
    e(c"32_32", 64, 1, 1),
    e(c"32_32_32_32", 128, 1, 1),
    e(c"32_FLOAT", 32, 1, 1),
    e(c"32_32_FLOAT", 64, 1, 1),
    e(c"32_32_32_32_FLOAT", 128, 1, 1),
    e(c"32_AS_8", 8, 1, 1),
    e(c"32_AS_8_8", 16, 1, 1),
    e(c"16_MPEG", 16, 1, 1),
    e(c"16_16_MPEG", 32, 1, 1),
    e(c"8_INTERLACED", 8, 1, 1),
    e(c"32_AS_8_INTERLACED", 8, 1, 1),
    e(c"32_AS_8_8_INTERLACED", 16, 1, 1),
    e(c"16_INTERLACED", 16, 1, 1),
    e(c"16_MPEG_INTERLACED", 16, 1, 1),
    e(c"16_16_MPEG_INTERLACED", 32, 1, 1),
    e(c"DXN", 128, 4, 4),
    e(c"8_8_8_8_AS_16_16_16_16", 32, 1, 1),
    e(c"DXT1_AS_16_16_16_16", 64, 4, 4),
    e(c"DXT2_3_AS_16_16_16_16", 128, 4, 4),
    e(c"DXT4_5_AS_16_16_16_16", 128, 4, 4),
    e(c"2_10_10_10_AS_16_16_16_16", 32, 1, 1),
    e(c"10_11_11_AS_16_16_16_16", 32, 1, 1),
    e(c"11_11_10_AS_16_16_16_16", 32, 1, 1),
    e(c"32_32_32_FLOAT", 96, 1, 1),
    e(c"DXT3A", 64, 4, 4),
    e(c"DXT5A", 64, 4, 4),
    e(c"CTX1", 64, 4, 4),
    e(c"DXT3A_AS_1_1_1_1", 64, 4, 4),
    e(c"8_8_8_8_GAMMA_EDRAM", 32, 1, 1),
    e(c"2_10_10_10_FLOAT_EDRAM", 32, 1, 1),
];

/// None for one the hardware does not define.
pub fn texture_format_name(format: u32) -> Option<&'static CStr> {
    FORMATS.get(format as usize).and_then(|entry| entry.name)
}

/// A block format stores `block_width * block_height` pixels in `bits_per_block`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FormatInfo {
    pub bits_per_block: u32,
    pub block_width: u32,
    pub block_height: u32,
    pub known: bool,
}

impl Default for FormatInfo {
    fn default() -> Self {
        FormatInfo { bits_per_block: 0, block_width: 1, block_height: 1, known: false }
    }
}

pub fn texture_format_of(format: u32) -> FormatInfo {
    match FORMATS.get(format as usize) {
        Some(entry) if entry.name.is_some() => {
            FormatInfo { bits_per_block: entry.bits, block_width: entry.bw, block_height: entry.bh, known: true }
        }
        _ => FormatInfo::default(),
    }
}

// The scatter across two banks and four pipes that ends both tiled address functions.
// Identical for 2D and 3D; only what feeds it differs.
macro_rules! tiled_combine {
    ($t:ty, $outer_inner:expr, $bank:expr, $pipe:expr, $y_lsb:expr) => {{
        let outer_inner: $t = $outer_inner;
        ((($y_lsb << 4) | ($pipe << 6) | ($bank << 11)) as $t)
            | (outer_inner & 0xF)
            | (((outer_inner >> 4) & 0x1) << 5)
            | (((outer_inner >> 5) & 0x7) << 8)
            | ((outer_inner >> 8) << 12)
    }};
}

/// Xenos stores textures in 32x32-block macro tiles with the address bits shuffled
/// across banks and pipes; this is Xenia's `Tiled2D` (BSD-3). Wrapping where the
/// C++'s int arithmetic would overflow, as clang's code does.
#[inline]
pub fn tiled_offset_2d(x: i32, y: i32, pitch_aligned_blocks: u32, bytes_per_block_log2: u32) -> i32 {
    // Blocks are grouped into 32x32 macro tiles; within one, rows pair up and the
    // address is scattered across two banks and four pipes. The bit shuffle at the
    // end is that scatter, not an optimisation.
    let outer_blocks = ((y >> 5).wrapping_mul((pitch_aligned_blocks >> 5) as i32).wrapping_add(x >> 5)) << 6;
    let inner_blocks = (((y >> 1) & 0x7) << 3) | (x & 0x7);
    let outer_inner = (outer_blocks | inner_blocks).wrapping_shl(bytes_per_block_log2);
    let bank = ((y >> 4) & 0x1) as u32;
    let pipe = (((x >> 3) & 0x3) ^ (((y >> 3) & 0x1) << 1)) as u32;
    let y_lsb = (y as u32) & 0x1;
    tiled_combine!(i32, outer_inner, bank, pipe, y_lsb)
}

/// A volume's macro tile is 32x16x4 blocks, not 32x32x1, and the slice index takes
/// part in both the inner address and the bank -- so a volume is not a stack of
/// 2D-tiled images and cannot be read as one. Xenia's texture_address::Tiled3D.
///
/// Checked against the light grid every draw binds, 512x256x4 with four bytes a
/// block: all 524288 texels at 524288 distinct addresses spanning exactly the 2 MB
/// the texture occupies.
#[inline]
pub fn tiled_offset_3d(
    x: i32,
    y: i32,
    z: i32,
    pitch_aligned_blocks: u32,
    height_aligned_blocks: u32,
    bytes_per_block_log2: u32,
) -> i64 {
    let outer_blocks = ((z >> 2)
        .wrapping_mul((height_aligned_blocks >> 4) as i32)
        .wrapping_add(y >> 4)
        .wrapping_mul((pitch_aligned_blocks >> 5) as i32)
        .wrapping_add(x >> 5))
        << 7;
    let inner_blocks = ((z & 0x3) << 5) | (((y >> 1) & 0x3) << 3) | (x & 0x7);
    let outer_inner = ((outer_blocks | inner_blocks) as i64) << bytes_per_block_log2;
    let bank = ((y >> 3) ^ (z >> 2)) as u32 & 0x1;
    let pipe = (((x >> 3) as u32) & 0x3) ^ (bank << 1);
    tiled_combine!(i64, outer_inner, bank, pipe, (y as u32) & 0x1)
}

fn log2_ceil(value: u32) -> u32 {
    let mut n = 0;
    while 1u32.wrapping_shl(n) < value {
        n += 1;
    }
    n
}

fn log2_floor(mut value: u32) -> u32 {
    let mut n = 0;
    loop {
        value >>= 1;
        if value == 0 {
            return n;
        }
        n += 1;
    }
}

fn align_up(value: u32, alignment: u32) -> u32 {
    value.wrapping_add(alignment).wrapping_sub(1) / alignment * alignment
}

// Small levels do not begin where they say they do. Once a level's shorter side is
// 16 texels or less, it and every level after it are packed into a single tile, each
// at its own place inside it: the first three a quarter, an eighth and a sixteenth of
// a tile along the shorter side, the rest along the longer one. The level the packing
// starts at can be 0. Xenia's GetPackedMipLevel and GetPackedMipOffset (BSD-3).
const NOT_PACKED: u32 = !0;

fn packed_level_of(fetch: &TextureFetch) -> u32 {
    if !fetch.packed_mips() {
        return NOT_PACKED;
    }
    let shorter = log2_ceil(fetch.width().min(fetch.height()));
    if shorter > 4 { shorter - 4 } else { 0 }
}

/// (x blocks, y blocks, z) of a packed level inside its tail.
fn packed_offset(fetch: &TextureFetch, info: &FormatInfo, level: u32) -> (u32, u32, u32) {
    let log2_width = log2_ceil(fetch.width());
    let log2_height = log2_ceil(fetch.height());
    let log2_size = log2_width.min(log2_height);
    if log2_size > 4u32.wrapping_add(level) {
        return (0, 0, 0);
    }
    let packed_base = if log2_size > 4 { log2_size - 4 } else { 0 };
    let packed = level.wrapping_sub(packed_base);
    let (mut x, mut y, mut z) = (0u32, 0u32, 0u32);
    if packed < 3 {
        if log2_width > log2_height {
            y = 16 >> packed;
        } else {
            x = 16 >> packed;
        }
    } else {
        let offset;
        if log2_width > log2_height {
            offset = 1u32.wrapping_shl(log2_width.wrapping_sub(packed_base)).wrapping_shr(packed - 2);
            x = offset;
        } else {
            offset = 1u32.wrapping_shl(log2_height.wrapping_sub(packed_base)).wrapping_shr(packed - 2);
            y = offset;
        }
        // The 1x1 levels of a volume go along its third axis instead.
        if offset < 4 && fetch.dimension() == DIMENSION_3D {
            let log2_depth = log2_ceil(fetch.depth());
            z = if log2_depth > 1 + packed { (log2_depth - packed) * 4 } else { 4 };
        }
    }
    let bw = if info.block_width != 0 { info.block_width } else { 1 };
    let bh = if info.block_height != 0 { info.block_height } else { 1 };
    (x / bw, y / bh, z)
}

// How one stored level is laid out: the base, a level of the mip chain, or a packed
// tail, which holds several levels and is stored as one. Xenia's GetGuestTextureLayout.
#[derive(Clone, Copy, Default)]
struct Stored {
    pitch_blocks: u32, // the row pitch the address functions take
    row_bytes: u32,
    slice_rows: u32,     // block rows to a slice of a volume
    slice_stride: usize, // between array slices and cube faces
    slice_extent: usize, // what one slice actually covers
}

fn stored_level(fetch: &TextureFetch, info: &FormatInfo, is_base: bool, level: u32) -> Stored {
    let bytes_per_block = info.bits_per_block / 8;
    let (pitch_texels, rows_texels) = if is_base {
        (if fetch.pitch() != 0 { fetch.pitch() } else { fetch.width() }, fetch.height())
    } else {
        (
            (1u32.wrapping_shl(log2_ceil(fetch.width())).wrapping_shr(level)).max(1),
            (1u32.wrapping_shl(log2_ceil(fetch.height())).wrapping_shr(level)).max(1),
        )
    };
    let mut s = Stored::default();
    s.row_bytes =
        align_up(align_up(pitch_texels, info.block_width) / info.block_width, 32).wrapping_mul(bytes_per_block);
    // A linear mip's rows are 256-byte aligned; the base's pitch is the title's word
    // for it.
    if !fetch.tiled() && !is_base {
        s.row_bytes = align_up(s.row_bytes, 256);
    }
    s.pitch_blocks = s.row_bytes / bytes_per_block;
    s.slice_rows = if fetch.dimension() == DIMENSION_1D {
        1
    } else {
        align_up(align_up(rows_texels, info.block_height) / info.block_height, 32)
    };
    let mut slice = s.row_bytes as usize * s.slice_rows as usize;
    // A volume's every level is strided by the base's depth, rounded to the four
    // slices of a volume tile -- Xenia's reading.
    if fetch.dimension() == DIMENSION_3D {
        slice = slice.wrapping_mul(align_up(fetch.depth(), 4) as usize);
    }
    s.slice_extent = slice;
    s.slice_stride = slice.wrapping_add(4095) & !4095;
    s
}

/// Array slices to the level: six for a cube, the stack for a stacked 2D texture, one
/// otherwise -- a volume's slices are inside its level.
fn array_size_of(fetch: &TextureFetch) -> u32 {
    match fetch.dimension() {
        DIMENSION_CUBE => 6,
        DIMENSION_3D => 1,
        _ => fetch.depth(),
    }
}

/// Where stored level `level` starts, from the mip address. Level 1 is at the address
/// itself -- or, when the packing starts at level 0, the tail holding every level is.
fn mip_offset(fetch: &TextureFetch, info: &FormatInfo, level: u32, packed_level: u32) -> usize {
    if packed_level == 0 {
        return 0;
    }
    let mut offset = 0usize;
    for l in 1..level {
        offset = offset
            .wrapping_add(stored_level(fetch, info, false, l).slice_stride.wrapping_mul(array_size_of(fetch) as usize));
    }
    offset
}

/// Where a texture's levels are in guest memory, and which of them the sampler may
/// use. Xenia's GetSubresourcesFromFetchConstant and GetGuestTextureLayout (BSD-3).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Extents {
    pub base_address: u32, // physical; zero when not read
    pub base_bytes: u32,
    pub mip_address: u32,
    pub mip_bytes: u32,
    pub min_level: u32,
    pub max_level: u32,
}

pub fn extents_of(fetch: &TextureFetch) -> Extents {
    let mut out = Extents::default();
    let info = texture_format_of(fetch.format());
    if !info.known || info.bits_per_block == 0 || info.bits_per_block % 8 != 0 {
        return out;
    }
    let volume = fetch.dimension() == DIMENSION_3D;
    let mut longest = fetch.width().max(fetch.height());
    if volume {
        longest = longest.max(fetch.depth());
    }
    let size_max_level = log2_floor(longest);

    let mut base = fetch.base_address();
    let mut mip = fetch.mip_address();
    let (mut min_level, mut max_level) = (0, 0);
    // No mip address, no mips, whatever the levels say. The mip filter is not
    // consulted: a fetch instruction may override it.
    if mip != 0 {
        min_level = fetch.min_mip_level().min(size_max_level);
        max_level = fetch.max_mip_level().min(size_max_level).max(min_level);
    }
    if max_level != 0 {
        // A texture that starts above level 0 may point both addresses at its chain,
        // and has no base to read then.
        if min_level != 0 && base == mip {
            base = 0;
        }
        if base == 0 {
            min_level = min_level.max(1);
        }
    } else {
        mip = 0;
    }

    let array_size = array_size_of(fetch) as usize;
    let packed_level = packed_level_of(fetch);
    if base != 0 {
        let s = stored_level(fetch, &info, true, 0);
        out.base_address = base;
        out.base_bytes = s.slice_stride.wrapping_mul(array_size.wrapping_sub(1)).wrapping_add(s.slice_extent) as u32;
    }
    if mip != 0 {
        // The last level stored on its own, or the tail the rest are packed in.
        let stored = if packed_level == 0 { 0 } else { max_level.min(packed_level) };
        let s = stored_level(fetch, &info, false, stored);
        out.mip_address = mip;
        out.mip_bytes = mip_offset(fetch, &info, stored, packed_level)
            .wrapping_add(s.slice_stride.wrapping_mul(array_size.wrapping_sub(1)))
            .wrapping_add(s.slice_extent) as u32;
    }
    out.min_level = min_level;
    out.max_level = max_level;
    out
}

/// Each level's place in the bytes: its layers (a volume's own depth at that level) of
/// blocks_high rows of blocks_wide blocks. The C++'s TextureData::Level, field for field.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub blocks_wide: u32,
    pub blocks_high: u32,
    pub layers: u32,
    pub offset: usize,
    pub size: usize,
}

/// The most levels a texture has: the fetch constant's level fields are four bits.
pub const MAX_LEVELS: usize = 16;

/// TextureData without its bytes, which go where the caller's `reserve` puts them.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TextureHeader {
    pub ok: bool,
    pub error: *const std::ffi::c_char,
    pub width: u32,
    pub height: u32,
    pub blocks_wide: u32,
    pub blocks_high: u32,
    pub bytes_per_block: u32,
    pub layers: u32,
    pub volume: bool,
    pub expanded: bool,
    pub min_level: u32,
    pub level_count: u32,
    pub levels: [Level; MAX_LEVELS],
}

impl Default for TextureHeader {
    fn default() -> Self {
        TextureHeader {
            ok: false,
            error: ptr::null(),
            width: 0,
            height: 0,
            blocks_wide: 0,
            blocks_high: 0,
            bytes_per_block: 0,
            layers: 1,
            volume: false,
            expanded: false,
            min_level: 0,
            level_count: 0,
            levels: [Level::default(); MAX_LEVELS],
        }
    }
}

// The C++ side's layouts (runtime/rust_renderer.cpp asserts the same numbers).
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<Level>() == 40 && offset_of!(Level, offset) == 24 && offset_of!(Level, size) == 32);
    assert!(size_of::<Extents>() == 24);
    assert!(size_of::<FormatInfo>() == 16 && offset_of!(FormatInfo, known) == 12);
    assert!(offset_of!(TextureHeader, levels) == 56 && size_of::<TextureHeader>() == 56 + MAX_LEVELS * 40);
};

impl TextureHeader {
    fn fail(mut self, error: &'static CStr) -> Self {
        self.error = error.as_ptr();
        self
    }
}

// Guest memory is big-endian and a texture says how its bytes were grouped when
// stored. Applied to the whole buffer: a block's internal layout is the format's
// business, and the padding between levels is zero either way.
fn swap_endian(data: &mut [u8], endian: u32) {
    match endian {
        ENDIAN_8IN16 => {
            for word in data.chunks_exact_mut(2) {
                let w = u16::from_ne_bytes([word[0], word[1]]).swap_bytes();
                word.copy_from_slice(&w.to_ne_bytes());
            }
        }
        ENDIAN_8IN32 => {
            for word in data.chunks_exact_mut(4) {
                let w = u32::from_ne_bytes([word[0], word[1], word[2], word[3]]).swap_bytes();
                word.copy_from_slice(&w.to_ne_bytes());
            }
        }
        ENDIAN_16IN32 => {
            for word in data.chunks_exact_mut(4) {
                let w = u32::from_ne_bytes([word[0], word[1], word[2], word[3]]);
                word.copy_from_slice(&((w << 16) | (w >> 16)).to_ne_bytes());
            }
        }
        _ => {}
    }
}

/// Untiled if it was tiled, byte-swapped either way, still in the guest's block
/// format -- DXT stays DXT -- because that is what a Vulkan driver wants. `base` and
/// `mips` are the host addresses of the two ranges extents_of names; a null `mips`
/// reads the base level alone, and a null `base` leaves level 0 zero. The bytes go
/// into what `reserve(size)` returns: `size` zeroed bytes the caller owns, asked for
/// once (DXT3A untiles into a buffer of its own and asks once, for the expanded bytes).
///
/// # Safety
/// `base` and `mips`, when not null, must be readable as far as the layout reaches,
/// which for a tiled slice under 4 KB or a 1D packed tail is past the extents
/// extents_of gives (guest memory: a page never touched is committed by the fault
/// handler). `reserve` must return `size` writable, zeroed bytes.
pub unsafe fn read_texture(
    fetch: &TextureFetch,
    base: *const u8,
    mips: *const u8,
    reserve: &mut dyn FnMut(usize) -> *mut u8,
) -> TextureHeader {
    let mut out = TextureHeader::default();
    let info = texture_format_of(fetch.format());
    if !info.known {
        return out.fail(c"unknown texture format");
    }
    if info.bits_per_block % 8 != 0 {
        return out.fail(c"sub-byte block size");
    }
    let bytes_per_block = info.bits_per_block / 8;
    // The tiled address function works in whole blocks of a power-of-two size; the
    // odd formats that are not (96-bit float triples) never appear here.
    if bytes_per_block & bytes_per_block.wrapping_sub(1) != 0 {
        return out.fail(c"block size not a power of two");
    }
    out.width = fetch.width();
    out.height = fetch.height();
    out.blocks_wide = (out.width + info.block_width - 1) / info.block_width;
    out.blocks_high = (out.height + info.block_height - 1) / info.block_height;
    out.bytes_per_block = bytes_per_block;
    if out.blocks_wide == 0 || out.blocks_high == 0 {
        return out.fail(c"empty texture");
    }

    let extents = extents_of(fetch);
    let read_base = !base.is_null() && extents.base_bytes != 0;
    let max_level = if !mips.is_null() && extents.mip_bytes != 0 { extents.max_level } else { 0 };
    if !read_base && max_level == 0 {
        return out.fail(c"no level to read");
    }
    out.min_level = if max_level != 0 { extents.min_level } else { 0 };
    if !read_base {
        out.min_level = out.min_level.max(1);
    }

    // A cube map uploads all six faces as an array and a volume all of its slices as
    // a 3D image; a stacked 2D texture uploads its first slice.
    out.volume = fetch.dimension() == DIMENSION_3D;
    out.layers = if fetch.dimension() == DIMENSION_CUBE {
        6
    } else if out.volume {
        fetch.depth()
    } else {
        1
    };
    if out.layers == 0 {
        out.layers = 1;
    }

    // Every level laid out first, each on a sixteen-byte boundary.
    assert!((max_level as usize) < MAX_LEVELS);
    let mut total = 0usize;
    for level in 0..=max_level {
        let mut l =
            Level { width: (out.width >> level).max(1), height: (out.height >> level).max(1), ..Level::default() };
        l.depth = if out.volume { (fetch.depth() >> level).max(1) } else { 1 };
        l.blocks_wide = (l.width + info.block_width - 1) / info.block_width;
        l.blocks_high = (l.height + info.block_height - 1) / info.block_height;
        l.layers = if out.volume { l.depth } else { out.layers };
        l.offset = (total + 15) & !15;
        l.size = l.blocks_wide as usize * l.blocks_high as usize * bytes_per_block as usize * l.layers as usize;
        total = l.offset + l.size;
        out.levels[level as usize] = l;
    }
    out.level_count = max_level + 1;

    let dxt3a = fetch.format() == 58 && bytes_per_block == 8;
    let mut blocks = Vec::new();
    let into: &mut [u8] = if dxt3a {
        blocks.resize(total, 0);
        &mut blocks
    } else {
        // SAFETY: reserve's contract: `total` writable zeroed bytes.
        unsafe { std::slice::from_raw_parts_mut(reserve(total), total) }
    };

    let bytes_per_block_log2 = log2_ceil(bytes_per_block);
    let packed_level = packed_level_of(fetch);
    for level in 0..=max_level {
        let l = out.levels[level as usize];
        if level == 0 && !read_base {
            continue;
        }
        // Level 0 is the base; a level in the packed tail is read from the tail at its
        // own offset.
        let is_base = level == 0;
        let stored = if is_base { 0 } else { level.min(packed_level) };
        let s = stored_level(fetch, &info, is_base, stored);
        let source = if is_base { base } else { mips.wrapping_add(mip_offset(fetch, &info, stored, packed_level)) };
        let (ox, oy, oz) = if level >= packed_level { packed_offset(fetch, &info, level) } else { (0, 0, 0) };
        // SAFETY: the level lies inside `into`, laid out above; the guest ranges are
        // the caller's (see the function's contract).
        unsafe {
            untile_level(fetch, &l, &s, source, (ox, oy, oz), bytes_per_block, bytes_per_block_log2, out.volume, into)
        };
    }
    // The swap is within each block, and a block is a whole number of the units it
    // swaps -- so one pass over the copy does every block alike. A block smaller than
    // the unit has always been left as it is.
    let endian = fetch.endian();
    let unit = match endian {
        ENDIAN_NONE => 1,
        ENDIAN_8IN16 => 2,
        _ => 4,
    };
    if bytes_per_block >= unit {
        swap_endian(into, endian);
    }
    if dxt3a {
        // SAFETY: read_texture's own contract on `reserve`.
        unsafe { expand_dxt3a(&mut out, &blocks, reserve) };
    }
    out.ok = true;
    out
}

/// One level from `source` into its place in `into`, a row at a time.
///
/// # Safety
/// `source` must be readable as far as the level's layout reaches from it (the
/// stored level `s`, the packed offset, every layer).
#[allow(clippy::too_many_arguments)]
#[inline]
unsafe fn untile_level(
    fetch: &TextureFetch,
    l: &Level,
    s: &Stored,
    source: *const u8,
    (ox, oy, oz): (u32, u32, u32),
    bytes_per_block: u32,
    bytes_per_block_log2: u32,
    volume: bool,
    into: &mut [u8],
) {
    let bpb = bytes_per_block as usize;
    let row_bytes = l.blocks_wide as usize * bpb;
    let tiled = fetch.tiled();
    // The low three bits of x stay in the address's low bits, so an aligned run of
    // blocks -- eight of them, at most sixteen bytes -- lies side by side in memory,
    // in both tilings: one address and one fixed-size copy a run instead of one a
    // block.
    let run = if bytes_per_block == 1 { 8 } else { 8u32.min(16 / bytes_per_block).max(1) };
    let run_bytes = (run * bytes_per_block) as usize;
    let end = into.len();
    let to_base = into.as_mut_ptr();
    for layer in 0..l.layers {
        for by in 0..l.blocks_high {
            let row_at = l.offset + (layer as usize * l.blocks_high as usize + by as usize) * row_bytes;
            assert!(row_at + row_bytes <= end);
            let sy = by.wrapping_add(oy);
            if !tiled {
                let slice = if volume {
                    layer.wrapping_add(oz) as usize * s.row_bytes as usize * s.slice_rows as usize
                } else {
                    layer as usize * s.slice_stride
                };
                let from = source.wrapping_add(slice + sy as usize * s.row_bytes as usize + ox as usize * bpb);
                // SAFETY: the row is inside `into` (asserted above); the source is the
                // caller's.
                unsafe { ptr::copy_nonoverlapping(from, to_base.add(row_at), row_bytes) };
                continue;
            }
            let mut bx = 0u32;
            while bx < l.blocks_wide {
                let sx = bx.wrapping_add(ox);
                // A volume addresses all three coordinates at once; there is no
                // per-slice offset to add to a 2D one.
                let at = if volume {
                    tiled_offset_3d(
                        sx as i32,
                        sy as i32,
                        layer.wrapping_add(oz) as i32,
                        s.pitch_blocks,
                        s.slice_rows,
                        bytes_per_block_log2,
                    ) as usize
                } else {
                    (layer as usize * s.slice_stride).wrapping_add(tiled_offset_2d(
                        sx as i32,
                        sy as i32,
                        s.pitch_blocks,
                        bytes_per_block_log2,
                    ) as usize)
                };
                let from = source.wrapping_add(at);
                // SAFETY: bx + run <= blocks_wide, or one block, stays in the row.
                unsafe {
                    let to = to_base.add(row_at + bx as usize * bpb);
                    if sx % run == 0 && bx + run <= l.blocks_wide {
                        if run_bytes == 16 {
                            ptr::copy_nonoverlapping(from, to, 16);
                        } else if run_bytes == 8 {
                            ptr::copy_nonoverlapping(from, to, 8);
                        } else {
                            ptr::copy_nonoverlapping(from, to, run_bytes);
                        }
                        bx += run;
                    } else {
                        ptr::copy_nonoverlapping(from, to, bpb);
                        bx += 1;
                    }
                }
            }
        }
    }
}

// DXT3A is the alpha block of DXT2/3: sixteen explicit four-bit values, not the two
// endpoints and interpolated indices of BC4. The two are both eight bytes a block, so
// reading one as the other is not caught by any size check -- it just decodes to
// plausible nonsense. MW2 stores the world's light map in it. Follows Xenia, which
// gives k_DXT3A a load shader of its own into R8 (BSD-3).
/// # Safety
/// `reserve(size)` must return `size` writable, zeroed bytes that nothing else touches.
unsafe fn expand_dxt3a(out: &mut TextureHeader, blocks: &[u8], reserve: &mut dyn FnMut(usize) -> *mut u8) {
    let count = out.level_count as usize;
    let mut total = 0usize;
    let mut at = [0usize; MAX_LEVELS];
    for (i, level) in out.levels[..count].iter().enumerate() {
        at[i] = (total + 15) & !15;
        total = at[i] + level.width as usize * level.height as usize * level.layers as usize;
    }
    // SAFETY: this function's contract: `total` writable zeroed bytes, ours alone.
    let flat = unsafe { std::slice::from_raw_parts_mut(reserve(total), total) };
    for (i, level) in out.levels[..count].iter_mut().enumerate() {
        let size = level.width as usize * level.height as usize * level.layers as usize;
        let (width, height) = (level.width as usize, level.height as usize);
        let (wide, high) = (level.blocks_wide as usize, level.blocks_high as usize);
        for layer in 0..level.layers as usize {
            for by in 0..high {
                // The block row's texel rows that are inside the level, and the blocks' bytes.
                let rows = (by * 4)..(by * 4 + 4).min(height);
                let source = &blocks[level.offset + (layer * high + by) * wide * 8..][..wide * 8];
                for y in rows {
                    let row = &mut flat[at[i] + (layer * height + y) * width..][..width];
                    let pair_at = (y & 3) * 2; // a block row is two bytes of nibbles
                    for (bx, block) in source.chunks_exact(8).enumerate() {
                        let pair = &block[pair_at..pair_at + 2];
                        let x0 = bx * 4;
                        for (dx, texel) in row[x0..(x0 + 4).min(width)].iter_mut().enumerate() {
                            let nibble = (pair[dx >> 1] >> ((dx & 1) * 4)) & 0xF;
                            *texel = nibble * 17; // 0..15 over 0..255
                        }
                    }
                }
            }
        }
        level.offset = at[i];
        level.size = size;
        level.blocks_wide = level.width;
        level.blocks_high = level.height;
    }
    out.blocks_wide = out.width;
    out.blocks_high = out.height;
    out.bytes_per_block = 1;
    out.expanded = true;
}

/// A read texture as decode_to_rgba sees it.
pub struct Decoded<'a> {
    pub ok: bool,
    pub expanded: bool,
    pub bytes_per_block: u32,
    pub levels: &'a [Level],
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Default)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

// A 5:6:5 colour pair and two bits per pixel: the BC1 colour block, which BC2 and BC3
// also use.
fn decode_colour_block(block: &[u8], punch_through: bool) -> [Rgba; 16] {
    let c0 = u16::from(block[0]) | (u16::from(block[1]) << 8);
    let c1 = u16::from(block[2]) | (u16::from(block[3]) << 8);
    let expand = |c: u16| {
        let c = u32::from(c);
        Rgba {
            r: (((c >> 11) & 0x1F) * 255 / 31) as u8,
            g: (((c >> 5) & 0x3F) * 255 / 63) as u8,
            b: ((c & 0x1F) * 255 / 31) as u8,
            a: 255,
        }
    };
    let mut palette = [Rgba::default(); 4];
    palette[0] = expand(c0);
    palette[1] = expand(c1);
    let (a, b) = (palette[0], palette[1]);
    let mix = |f: fn(u32, u32) -> u32| Rgba {
        r: f(u32::from(a.r), u32::from(b.r)) as u8,
        g: f(u32::from(a.g), u32::from(b.g)) as u8,
        b: f(u32::from(a.b), u32::from(b.b)) as u8,
        a: 255,
    };
    if !punch_through || c0 > c1 {
        palette[2] = mix(|a, b| (2 * a + b) / 3);
        palette[3] = mix(|a, b| (a + 2 * b) / 3);
    } else {
        palette[2] = mix(|a, b| (a + b) / 2);
        palette[3] = Rgba { r: 0, g: 0, b: 0, a: 0 };
    }
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|i| palette[((bits >> (2 * i)) & 3) as usize])
}

// Two endpoints and three bits per pixel: the BC3 alpha block, which BC4 (DXT5A here)
// uses on its own as a single channel.
fn decode_alpha_block(block: &[u8]) -> [u8; 16] {
    let mut a = [0u8; 8];
    a[0] = block[0];
    a[1] = block[1];
    let (a0, a1) = (u32::from(a[0]), u32::from(a[1]));
    if a[0] > a[1] {
        for i in 0..6u32 {
            a[2 + i as usize] = (((6 - i) * a0 + (1 + i) * a1) / 7) as u8;
        }
    } else {
        for i in 0..4u32 {
            a[2 + i as usize] = (((4 - i) * a0 + (1 + i) * a1) / 5) as u8;
        }
        a[6] = 0;
        a[7] = 255;
    }
    let mut bits = 0u64;
    for i in 0..6 {
        bits |= u64::from(block[2 + i]) << (8 * i);
    }
    std::array::from_fn(|i| a[((bits >> (3 * i)) & 7) as usize])
}

/// Not the upload path -- a driver takes the block formats directly -- but the only
/// way to confirm by eye that untiling, endianness and addressing are all right at
/// once. One level, its layers one under another. `rgba(size)` gives the caller's
/// zeroed buffer, asked for only once the level is known to exist.
///
/// # Safety
/// `rgba(size)` must return `size` writable, zeroed bytes that nothing else touches
/// while this runs.
pub unsafe fn decode_to_rgba(data: &Decoded, format: u32, level: u32, rgba: &mut dyn FnMut(usize) -> *mut u8) -> bool {
    if !data.ok || level as usize >= data.levels.len() {
        return false;
    }
    let info = texture_format_of(format);
    if !info.known {
        return false;
    }
    let l = data.levels[level as usize];
    let bytes = &data.bytes[l.offset.min(data.bytes.len())..];
    // Every layer, one under another.
    let rows = l.height * l.layers;
    let size = l.width as usize * rows as usize * 4;
    // SAFETY: this function's contract: `size` writable zeroed bytes, ours alone.
    let out = unsafe { std::slice::from_raw_parts_mut(rgba(size), size) };
    // DXT3A arrives unpacked to one byte a texel (see read_texture).
    if data.expanded && data.bytes_per_block == 1 {
        let mut i = 0usize;
        while i < l.width as usize * rows as usize && i < l.size {
            let v = bytes[i];
            out[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            i += 1;
        }
        return true;
    }
    for layer in 0..l.layers {
        let mut put = |x: u32, y: u32, v: Rgba| {
            if x >= l.width || y >= l.height {
                return;
            }
            let p = ((layer as usize * l.height as usize + y as usize) * l.width as usize + x as usize) * 4;
            out[p..p + 4].copy_from_slice(&[v.r, v.g, v.b, v.a]);
        };
        for by in 0..l.blocks_high {
            for bx in 0..l.blocks_wide {
                let block = &bytes[((layer as usize * l.blocks_high as usize + by as usize)
                    * l.blocks_wide as usize
                    + bx as usize)
                    * data.bytes_per_block as usize..];
                let (x0, y0) = (bx * info.block_width, by * info.block_height);
                match format {
                    18 | 51 => {
                        // DXT1
                        let colours = decode_colour_block(block, true);
                        for (i, c) in colours.iter().enumerate() {
                            put(x0 + (i as u32 & 3), y0 + (i as u32 >> 2), *c);
                        }
                    }
                    19 | 20 | 52 | 53 => {
                        // DXT2_3, DXT4_5: BC2 stores four bits of alpha per pixel, BC3 an
                        // interpolated block; both put the colour block second.
                        let alpha: [u8; 16] = if format == 19 || format == 52 {
                            std::array::from_fn(|i| ((block[i >> 1] >> ((i & 1) * 4)) & 0xF) * 17)
                        } else {
                            decode_alpha_block(block)
                        };
                        let mut colours = decode_colour_block(&block[8..], false);
                        for (i, c) in colours.iter_mut().enumerate() {
                            c.a = alpha[i];
                            put(x0 + (i as u32 & 3), y0 + (i as u32 >> 2), *c);
                        }
                    }
                    59 => {
                        // DXT5A
                        let value = decode_alpha_block(block);
                        for (i, v) in value.iter().enumerate() {
                            put(x0 + (i as u32 & 3), y0 + (i as u32 >> 2), Rgba { r: *v, g: *v, b: *v, a: 255 });
                        }
                    }
                    6 | 14 | 50 => put(x0, y0, Rgba { r: block[2], g: block[1], b: block[0], a: block[3] }),
                    10 => put(x0, y0, Rgba { r: block[0], g: block[1], b: 0, a: 255 }),
                    2 | 8 | 9 => put(x0, y0, Rgba { r: block[0], g: block[0], b: block[0], a: 255 }),
                    _ => return false,
                }
            }
        }
    }
    true
}
