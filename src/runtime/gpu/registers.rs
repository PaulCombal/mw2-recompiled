// The Xenos register file (runtime/gpu/registers.h).
//
// Almost everything the GPU does is a register write, carried in four packet shapes
// that all land here. A draw packet then says only "go": what to draw with is whatever
// the registers hold at that moment. So this is the state the Vulkan backend reads --
// the float, boolean, loop, vertex fetch and texture fetch constants are all windows
// into this one array.
//
// Indices and window bases follow Xenia's `register_table.inc` and
// `command_processor.cc` (BSD-3). The C++ renderer reads this structure through
// registers.h, so its layout is the C++'s, asserted here and in rust_calls.cpp.

// The names only the census and the traces read are unused in a release, which leaves
// those out; the renderer reads them all once it moves.
#![cfg_attr(not(feature = "diagnostics"), allow(dead_code))]

use std::ffi::CStr;

/// The register space proper, then the constant windows above it; the top of the
/// range is the loop constants.
pub const REGISTER_COUNT: u32 = 0x5003;

// SET_CONSTANT names a window and an index within it, not an absolute register, so
// each type adds its own base.
pub const CONSTANT_BASE_ALU: u32 = 0x4000; // float, 4 dwords each
pub const CONSTANT_BASE_FETCH: u32 = 0x4800; // vertex and texture
pub const CONSTANT_BASE_BOOL: u32 = 0x4900; // 256 bits, 8 dwords
pub const CONSTANT_BASE_LOOP: u32 = 0x4908; // 32 dwords
pub const CONSTANT_BASE_REGISTERS: u32 = 0x2000; // ordinary registers

// The few registers the command processor and the census read by name.
pub const RB_SURFACE_INFO: u32 = 0x2000;
pub const RB_COLOR_INFO: u32 = 0x2001;
pub const RB_DEPTH_INFO: u32 = 0x2002;
pub const PA_SC_WINDOW_SCISSOR_TL: u32 = 0x2081;
pub const PA_SC_WINDOW_SCISSOR_BR: u32 = 0x2082;
pub const SQ_PROGRAM_CNTL: u32 = 0x2180;
// The display's colour lookup table, applied by the scanout hardware.
pub const DC_LUT_RW_INDEX: u32 = 0x1922; // which entry the next write lands in
pub const DC_LUT_30_COLOR: u32 = 0x1925; // ten bits per channel, blue lowest
pub const DC_LUT_WRITE_EN_MASK: u32 = 0x1927; // blue, green, red
// Where the command processor leaves an occlusion query's sample counts.
pub const RB_SAMPLE_COUNT_ADDR: u32 = 0x2325;
pub const VGT_DRAW_INITIATOR: u32 = 0x21FC;
pub const RB_MODECONTROL: u32 = 0x2208;
pub const RB_COPY_CONTROL: u32 = 0x2318;
pub const RB_COPY_DEST_BASE: u32 = 0x2319;
pub const RB_COPY_DEST_PITCH: u32 = 0x231A;
pub const RB_COPY_DEST_INFO: u32 = 0x231B;

/// A draw issued in `Copy` mode is not geometry: it is a resolve, and the rectangle it
/// covers is the region of EDRAM to copy out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EdramMode(pub u32);

impl EdramMode {
    pub const NO_OPERATION: EdramMode = EdramMode(0);
    pub const COLOR_DEPTH: EdramMode = EdramMode(4);
    pub const DEPTH_ONLY: EdramMode = EdramMode(5);
    pub const COPY: EdramMode = EdramMode(6);

    pub fn name(self) -> &'static CStr {
        match self {
            EdramMode::NO_OPERATION => c"no operation",
            EdramMode::COLOR_DEPTH => c"colour+depth",
            EdramMode::DEPTH_ONLY => c"depth only",
            EdramMode::COPY => c"copy",
            _ => c"unknown",
        }
    }
}

// Render target addresses are in EDRAM tiles, not bytes.
pub struct SurfaceInfo(pub u32);
impl SurfaceInfo {
    pub fn pitch(&self) -> u32 {
        self.0 & 0x3FFF
    }
    pub fn msaa_samples(&self) -> u32 {
        (self.0 >> 16) & 0x3
    }
}

pub struct ColorInfo(pub u32);
impl ColorInfo {
    pub fn base_tile(&self) -> u32 {
        self.0 & 0xFFF
    }
    pub fn format(&self) -> u32 {
        (self.0 >> 16) & 0xF
    }
}

pub struct DepthInfo(pub u32);
impl DepthInfo {
    pub fn base_tile(&self) -> u32 {
        self.0 & 0xFFF
    }
    pub fn format(&self) -> u32 {
        (self.0 >> 16) & 0x1
    }
}

pub fn color_target_format_name(format: u32) -> Option<&'static CStr> {
    Some(match format {
        0 => c"8_8_8_8",
        1 => c"8_8_8_8_GAMMA",
        2 => c"2_10_10_10",
        3 => c"2_10_10_10_FLOAT",
        4 => c"16_16",
        5 => c"16_16_16_16",
        6 => c"16_16_FLOAT",
        7 => c"16_16_16_16_FLOAT",
        10 => c"2_10_10_10_AS_10_10_10_10",
        12 => c"2_10_10_10_FLOAT_AS_16_16_16_16",
        14 => c"32_FLOAT",
        15 => c"32_32_FLOAT",
        _ => return None,
    })
}

pub fn depth_target_format_name(format: u32) -> &'static CStr {
    if format != 0 { c"D24FS8" } else { c"D24S8" }
}

pub struct CopyControl(pub u32);
impl CopyControl {
    // 0-3 select a colour target; 4 is depth.
    pub fn source_select(&self) -> u32 {
        self.0 & 0x7
    }
    pub fn copies_depth(&self) -> bool {
        self.source_select() == 4
    }
    pub fn clears_color(&self) -> bool {
        (self.0 >> 8) & 1 != 0
    }
    pub fn clears_depth(&self) -> bool {
        (self.0 >> 9) & 1 != 0
    }
    // 0 raw, 1 converting, 2 constant one, 3 null.
    pub fn command(&self) -> u32 {
        (self.0 >> 20) & 0x3
    }
}

pub struct CopyDestInfo(pub u32);
impl CopyDestInfo {
    pub fn endian(&self) -> u32 {
        self.0 & 0x7
    }
    pub fn format(&self) -> u32 {
        (self.0 >> 7) & 0x3F
    }
}

pub struct CopyDestPitch(pub u32);
impl CopyDestPitch {
    pub fn pitch(&self) -> u32 {
        self.0 & 0x3FFF
    }
    pub fn height(&self) -> u32 {
        (self.0 >> 16) & 0x3FFF
    }
}

/// Supplied by the draw packet rather than the register file, and says what the draw
/// actually is.
#[derive(Clone, Copy)]
pub struct DrawInitiator(pub u32);
impl DrawInitiator {
    pub fn primitive_type(&self) -> u32 {
        self.0 & 0x3F
    }
    // 0 = indices fetched by DMA, 1 = immediate in the packet, 2 = auto.
    pub fn source_select(&self) -> u32 {
        (self.0 >> 6) & 0x3
    }
    pub fn index32(&self) -> bool {
        (self.0 >> 11) & 1 != 0
    }
    pub fn index_count(&self) -> u32 {
        (self.0 >> 16) & 0xFFFF
    }
}

pub fn primitive_name(primitive: u32) -> Option<&'static CStr> {
    Some(match primitive {
        0x00 => c"none",
        0x01 => c"point list",
        0x02 => c"line list",
        0x03 => c"line strip",
        0x04 => c"triangle list",
        0x05 => c"triangle fan",
        0x06 => c"triangle strip",
        0x07 => c"triangle with w flags",
        0x08 => c"rectangle list",
        0x0C => c"line loop",
        0x0D => c"quad list",
        0x0E => c"quad strip",
        0x0F => c"polygon",
        _ => return None,
    })
}

/// Two 14-bit corners, with the window offset disable in the top bit of the top-left one.
pub struct ScissorCorner(pub u32);
impl ScissorCorner {
    pub fn x(&self) -> u32 {
        self.0 & 0x7FFF
    }
    pub fn y(&self) -> u32 {
        (self.0 >> 16) & 0x7FFF
    }
}

// One monotonic write counter stamped into the block a write lands in, so a constant
// window whose highest stamp has not moved need not be copied again. 64 dwords a block
// is 16 float constants.
pub const STAMP_SHIFT: u32 = 6;
pub const STAMP_BLOCKS: usize = (REGISTER_COUNT >> STAMP_SHIFT) as usize + 1;
const COUNT: usize = REGISTER_COUNT as usize;
// The fetch constants: 32 of six dwords.
const FETCH_DWORDS: u32 = 32 * 6;

#[repr(C)]
pub struct RegisterFile {
    pub values: [u32; COUNT],
    pub stamps: [u64; STAMP_BLOCKS],
    pub writes: u64,
}

// registers.h's layout, which the C++ renderer reads (asserted in rust_calls.cpp too).
const _: () = {
    assert!(std::mem::offset_of!(RegisterFile, values) == 0);
    assert!(std::mem::offset_of!(RegisterFile, stamps) == 81936);
    assert!(std::mem::offset_of!(RegisterFile, writes) == 84504);
    assert!(std::mem::size_of::<RegisterFile>() == 84512);
    assert!(STAMP_BLOCKS == 321);
};

impl RegisterFile {
    pub const fn new() -> Self {
        RegisterFile { values: [0; COUNT], stamps: [0; STAMP_BLOCKS], writes: 0 }
    }

    /// 0 out of range, as the C++'s operator[].
    #[inline(always)]
    pub fn get(&self, index: u32) -> u32 {
        self.values.get(index as usize).copied().unwrap_or(0)
    }

    #[inline(always)]
    pub fn in_range(index: u32) -> bool {
        index < REGISTER_COUNT
    }

    /// A write of the value already there stamps nothing: D3D sends a draw's constants
    /// whole, most of them what the last draw had, and the copies the stamps guard
    /// depend only on the values. Except the fetch constants: the vertex data a fetch
    /// names is copied with the block and the title rewrites some buffers in place
    /// between draws, so a fetch constant written again, even unchanged, keeps meaning
    /// "look at the memory again". `index` is in range (the caller's check).
    #[inline(always)]
    pub fn write(&mut self, index: u32, value: u32) {
        let fetch = index.wrapping_sub(CONSTANT_BASE_FETCH) < FETCH_DWORDS;
        let Some(slot) = self.values.get_mut(index as usize) else { return };
        if *slot == value && !fetch {
            return;
        }
        *slot = value;
        self.writes += 1;
        // In range with the value: REGISTER_COUNT >> STAMP_SHIFT < STAMP_BLOCKS.
        if let Some(stamp) = self.stamps.get_mut((index >> STAMP_SHIFT) as usize) {
            *stamp = self.writes;
        }
    }

    /// `count` registers from `first`, as `write` would take them one by one, from the
    /// guest's big-endian dwords (`source(i)` gives the i-th as the guest holds it), a
    /// stamp block at a time: one stamp for a block. One pass swaps, compares and stores
    /// -- an unchanged dword is stored as it was, and only a change stamps. The run must
    /// lie inside the file (the caller's check); one that does not is not written.
    ///
    /// The run's range is checked once, and each block indexed from 0 as registers.h's
    /// WriteRun does: a slice checked per block and an iterator's two pointers cost the
    /// consumer instructions on every register packet that the C++ does not spend.
    #[inline(always)]
    #[expect(clippy::needless_range_loop, reason = "the index from 0 is what makes the loop the C++'s")]
    pub fn write_run(&mut self, first: u32, count: u32, source: impl Fn(usize) -> u32) {
        let (first, count) = (first as usize, count as usize);
        let Some(mut rest) = self.values.get_mut(first..first + count) else { return };
        let mut at = first;
        let mut taken = 0usize;
        while !rest.is_empty() {
            let block = at >> STAMP_SHIFT;
            let n = rest.len().min(((block + 1) << STAMP_SHIFT) - at);
            let fetch = at + n > CONSTANT_BASE_FETCH as usize && at < (CONSTANT_BASE_FETCH + FETCH_DWORDS) as usize;
            let (to, tail) = std::mem::take(&mut rest).split_at_mut(n);
            let mut changed = 0u32;
            for i in 0..n {
                let value = u32::from_be(source(taken + i));
                changed |= to[i] ^ value;
                to[i] = value;
            }
            if fetch || changed != 0 {
                self.writes += 1;
                // In range with the value: REGISTER_COUNT >> STAMP_SHIFT < STAMP_BLOCKS.
                if let Some(stamp) = self.stamps.get_mut(block) {
                    *stamp = self.writes;
                }
            }
            rest = tail;
            at += n;
            taken += n;
        }
    }

    /// Zero for a window nothing has ever written, which is its own answer: the newest
    /// stamp of the blocks the window covers.
    #[expect(dead_code, reason = "the renderer's, which is C++ until S9 (registers.h's StampOf)")]
    pub fn stamp_of(&self, first: u32, count: u32) -> u64 {
        let last = (first.wrapping_add(count).wrapping_sub(1) >> STAMP_SHIFT) as usize;
        let from = (first >> STAMP_SHIFT) as usize;
        self.stamps.get(from..=last.min(STAMP_BLOCKS - 1)).map_or(0, |blocks| blocks.iter().copied().max().unwrap_or(0))
    }
}
