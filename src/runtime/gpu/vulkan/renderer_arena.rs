// What a draw reads besides its textures -- constants, vertex data, indices -- copied out of
// guest memory and the register file into the frame arena.
//
// Unsafe by nature: the arena is a mapping of device memory, write-combined, that nothing
// here may read back or take a slice of (§3.5): it is written through raw copies and plain
// stores at offsets Allocate has just checked against the slot's share, and the guest's
// memory, which guest threads write concurrently, is read through raw copies at addresses
// the fetch constants name.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "diagnostics")]
use super::arena_census::{self as census, Constants, FetchMiss, VertexCopy};
use super::renderer_state::{ARENA_KINDS, ArenaKind, Cached, ConstantBlock, ConstantOffsets, FRAME_SLOTS};
#[cfg(feature = "diagnostics")]
use crate::runtime::bridge::stream;
use crate::runtime::gpu::registers::{
    CONSTANT_BASE_ALU, CONSTANT_BASE_BOOL, CONSTANT_BASE_FETCH, CONSTANT_BASE_LOOP, ConstantWindow, RegisterFile,
    SQ_PS_CONST, SQ_VS_CONST, STAMP_SHIFT,
};
use crate::runtime::gpu::shader::TranslationMeta;
use crate::runtime::gpu::shader::bindings::{CONSTANT_FILE_BLOCKS, CONSTANT_TABLE_WORDS};
use crate::runtime::guest_memory::{self, PHYSICAL_BASE};
use crate::runtime::kernel;
use std::mem::MaybeUninit;
use std::ptr;

use crate::runtime::gpu::memory_watch::serve;

// A block of 16 constants is one stamp block.
const _: () = assert!(CONSTANT_BASE_ALU.is_multiple_of(1 << STAMP_SHIFT) && (1 << STAMP_SHIFT) == 64);

/// The flash hunt's notes of what a draw read (renderer_diagnostics.rs), made only while a
/// draw is recorded, which a release never does.
pub trait DrawNotes {
    fn constant_windows(&mut self, r: &RegisterFile, vertex_needed: u32, pixel_needed: u32);
    /// The bytes are guest memory, null with a size of 0 for "the previous draw's".
    fn vertex_window(&mut self, bytes: *const u8, size: usize);
    fn vertex_buffer(&mut self, physical: u32);
    fn indices(&mut self, bytes: *const u8, size: usize);
}

/// No draw is being recorded.
pub struct NoNotes;
impl DrawNotes for NoNotes {
    fn constant_windows(&mut self, _: &RegisterFile, _: u32, _: u32) {}
    fn vertex_window(&mut self, _: *const u8, _: usize) {}
    fn vertex_buffer(&mut self, _: u32) {}
    fn indices(&mut self, _: *const u8, _: usize) {}
}

/// What the memory watch's Serve is asked with for a vertex window the shadow may hold:
/// the submission being recorded (g.submissionSerial + 1), the last one the GPU has
/// finished (CompletedSerial) and the frame (g.frames).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct ServeClock {
    pub recording: u64,
    pub completed: u64,
    pub frame: u64,
}

/// The index swap (index_swap.rs): `count` big-endian indices from `source` (any alignment)
/// into `out` (32-byte aligned),
/// and the lowest and highest index on the way through.
pub type Swap16 = unsafe fn(*const u8, *mut u16, u32) -> (u16, u16);
pub type Swap32 = unsafe fn(*const u8, *mut u32, u32) -> (u32, u32);

/// One host-visible buffer holding everything a draw reads that is not a texture, split
/// into a share per frame slot, and the caches of what this slot's share already holds
/// (State's arena fields).
///
/// What the writes below write lies inside the mapping by construction: the mapping, the
/// shares and the current slot are set by `new` and `begin_frame` alone and every write
/// goes through `allocate`, so the fields that place them are private.
pub struct Arena {
    mapped: *mut u8,
    bytes: u32, // what the device actually gave
    uniform_alignment: u32,
    /// Where the shadow of guest physical memory starts in the same buffer, just past the
    /// arena; 0 when there is none.
    shadow_base: u32,
    slot_bytes: u32,                         // one slot's share
    slot_bases: [u32; FRAME_SLOTS as usize], // where each slot's share starts
    base: u32,                               // where the current slot's share starts
    used: u32,                               // and how much of that share is spent

    pixel_window: Cached, // the pixel program's constants
    /// The vertex program's constants (shader/bindings.rs): where in this slot's arena each
    /// block of 16 was copied, at which register stamp, and the table that points at them.
    constant_blocks: [ConstantBlock; CONSTANT_FILE_BLOCKS as usize],
    constant_table: Cached,
    /// The table as the next copy of it would be: the base, then each block's vec4 index.
    constant_table_words: [u32; CONSTANT_TABLE_WORDS as usize],
    fetch_block: Cached,
    booleans: Cached,
    loops: Cached,
    fetch_subs: [u32; 3],  // what fetch_block was assembled for
    fetch_pairs: [u32; 3], // and which of its pairs it holds

    // Counted for the report only.
    pub overflows: u64,
    pub written: [u64; ARENA_KINDS],
    pub reused: [u64; ARENA_KINDS],

    // A rectangle list's vertices, put together here and not in the arena.
    corners: Vec<u32>,
    // The census's: the vertex windows copied this frame, and the fetch constants the
    // fetch block was last assembled from.
    #[cfg(feature = "diagnostics")]
    copied: census::Copied,
    #[cfg(feature = "diagnostics")]
    fetch_values: [u32; 192],
    swap16: Swap16,
    swap32: Swap32,
}

// SAFETY: the arena is the ring consumer's, moved into its thread with the renderer (§3.6);
// the mapping it points at lives until the renderer's shutdown unmaps it, after the worker
// has been joined.
unsafe impl Send for Arena {}

/// The arena's offsets are descriptor offsets and storage-buffer indices: the larger of
/// the device's two minimums, and never under 256.
pub fn alignment(min_uniform: u64, min_storage: u64) -> u32 {
    min_uniform.max(min_storage).max(256) as u32
}

/// One slot's share of the arena, a whole number of alignments.
pub fn share(bytes: u32, alignment: u32) -> u32 {
    (bytes / FRAME_SLOTS) & !(alignment - 1)
}

/// The blocks of 16 a program's constants need copied, a bit each: those it names, or
/// all of them when it indexes them with a run-time value.
#[inline]
pub fn constant_blocks(program: &TranslationMeta) -> u32 {
    if program.addressed_constants {
        return 0xFFFF;
    }
    program.constant_blocks_read & 0xFFFF
}

/// What a program needs copied: one past the highest constant it names, or the whole
/// window when it indexes them with a run-time value.
#[inline]
pub fn constants_needed(program: &TranslationMeta) -> u32 {
    if program.addressed_constants {
        return 256;
    }
    program.constants_read.min(256)
}

#[inline]
pub fn vertex_range(lowest: u32, highest: u32) -> u64 {
    (u64::from(lowest) << 32) | u64::from(highest)
}

/// Xenos takes three corners of a rectangle and works out the fourth as v0 + v2 - v1. The
/// data is still in the guest's byte order here, and a field that is the same at all three
/// corners -- a packed colour, most often -- is carried across rather than run through
/// arithmetic that would not mean anything for it.
pub fn build_fourth_corner(vertices: &mut [u32], stride: u32, rect: u32, at: u32) {
    let (stride, rect, at) = (stride as usize, rect as usize, at as usize);
    for d in 0..stride {
        let a = u32::from_be(vertices[(rect * 3) * stride + d]);
        let b = u32::from_be(vertices[(rect * 3 + 1) * stride + d]);
        let c = u32::from_be(vertices[(rect * 3 + 2) * stride + d]);
        let value =
            if a == b && b == c { a } else { (f32::from_bits(a) + f32::from_bits(c) - f32::from_bits(b)).to_bits() };
        vertices[at * stride + d] = value.to_be();
    }
}

/// The swaps this CPU runs: AVX2 where it has it, else x86-64-v2.
pub fn swaps(avx2: bool) -> (Swap16, Swap32) {
    use super::index_swap::{swap16_avx2, swap16_v2, swap32_avx2, swap32_v2};
    if avx2 { (swap16_avx2, swap32_avx2) } else { (swap16_v2, swap32_v2) }
}

impl Arena {
    /// The arena over `bytes` of mapped device memory, its shares a whole number of
    /// `uniform_alignment`s, the current slot the last (BeginFrame moves on before it
    /// records), the swaps picked for this CPU.
    ///
    /// # Safety
    /// `bytes` bytes at `mapped` are writable, and stay so while the arena lives.
    pub unsafe fn new(mapped: *mut u8, bytes: u32, uniform_alignment: u32, shadow_base: u32) -> Arena {
        let slot_bytes = share(bytes, uniform_alignment);
        let slot_bases: [u32; FRAME_SLOTS as usize] = std::array::from_fn(|i| i as u32 * slot_bytes);
        let (swap16, swap32) = swaps(std::arch::is_x86_feature_detected!("avx2"));
        Arena {
            mapped,
            bytes,
            uniform_alignment,
            shadow_base,
            slot_bytes,
            slot_bases,
            base: slot_bases[FRAME_SLOTS as usize - 1],
            used: 0,
            pixel_window: Cached::default(),
            constant_blocks: [ConstantBlock::default(); CONSTANT_FILE_BLOCKS as usize],
            constant_table: Cached::default(),
            constant_table_words: [0; CONSTANT_TABLE_WORDS as usize],
            fetch_block: Cached::default(),
            booleans: Cached::default(),
            loops: Cached::default(),
            fetch_subs: [0; 3],
            fetch_pairs: [0; 3],
            overflows: 0,
            written: [0; ARENA_KINDS],
            reused: [0; ARENA_KINDS],
            corners: Vec::new(),
            #[cfg(feature = "diagnostics")]
            copied: census::Copied::default(),
            #[cfg(feature = "diagnostics")]
            fetch_values: [0; 192],
            swap16,
            swap32,
        }
    }

    /// BeginFrame's part: slot `slot`'s share from its start, and nothing it held cached.
    pub fn begin_frame(&mut self, slot: u32) {
        // Nothing still to be copied into the share being left (a submission settled it
        // already; this holds even without one).
        super::copy_worker::settle();
        self.base = self.slot_bases[slot as usize];
        self.used = 0;
        self.pixel_window.valid = false;
        for block in &mut self.constant_blocks {
            block.valid = false;
        }
        self.constant_table.valid = false;
        self.constant_table_words = [0; CONSTANT_TABLE_WORDS as usize];
        self.fetch_block.valid = false;
        self.booleans.valid = false;
        self.loops.valid = false;
        #[cfg(feature = "diagnostics")]
        self.copied.new_frame();
    }

    /// A draw's constants, vertex data and indices are copied here and addressed by offset;
    /// u32::MAX when the slot's share has no room ("frame arena full": the draw is skipped).
    /// Offsets are into the whole buffer -- they become descriptor offsets and storage-buffer
    /// indices -- so the slot's base is carried in the answer.
    #[inline]
    pub fn allocate(&mut self, bytes: u32, kind: ArenaKind) -> u32 {
        let aligned = self.used.wrapping_add(self.uniform_alignment - 1) & !(self.uniform_alignment - 1);
        // Checked: a sum that wrapped past the share's end would write past it.
        let end = match aligned.checked_add(bytes) {
            Some(end) if end <= self.slot_bytes => end,
            _ => {
                self.overflows += 1;
                return u32::MAX;
            }
        };
        self.used = end;
        self.written[kind as usize] += u64::from(bytes);
        self.base.wrapping_add(aligned)
    }

    /// `size` bytes from `bytes` into the arena.
    ///
    /// # Safety
    /// `size` bytes are readable at `bytes`.
    #[inline]
    pub unsafe fn copy_into_arena(&mut self, bytes: *const u8, size: u32, kind: ArenaKind) -> u32 {
        let at = self.allocate(size, kind);
        if at == u32::MAX {
            return at;
        }
        // SAFETY: allocate put [at, at + size) inside the slot's share, inside the mapping;
        // the caller's source.
        unsafe { ptr::copy_nonoverlapping(bytes, self.mapped.add(at as usize), size as usize) };
        at
    }

    /// `count` registers from `first` into the arena: register-file dwords, never outside it.
    #[inline]
    fn copy_registers(&mut self, r: &RegisterFile, first: u32, count: u32, kind: ArenaKind) -> u32 {
        debug_assert!(first as usize + count as usize <= r.values.len());
        // SAFETY: every caller's window lies inside the file (asserted above): the constant
        // windows end at the fetch constants, the booleans and loops at the file's end.
        unsafe { self.copy_into_arena(r.values.as_ptr().add(first as usize).cast(), count * 4, kind) }
    }

    /// The file holds 512 vec4 and a pixel program sees 256 from wherever SQ_PS_CONST's base
    /// says -- usually 256, so uploading the low half would hand it the vertex program's
    /// constants. `blocks` are the blocks of 16 of the 256 the program names, a bit each
    /// (TranslationMeta::constant_blocks_read, all 16 for a program that indexes them): the
    /// copy is laid out as the whole window, from its first constant, but only those blocks
    /// are written into it -- a program names three or four of the sixteen, often the first
    /// and one near the top -- and what lies between them is whatever the arena held, which
    /// the program never reads.
    fn write_pixel_constants(&mut self, r: &RegisterFile, blocks: u32) -> u32 {
        let base = ConstantWindow(r.get(SQ_PS_CONST)).base().min(256);
        let first = CONSTANT_BASE_ALU + base * 4;
        let blocks = blocks & 0xFFFF;
        // The newest write to any of `mask`'s blocks: each is 64 registers from the window's
        // first (inside the file: base <= 256 and the last block ends 256 vec4 above it).
        let written = |mask: u32| {
            let mut newest = 0;
            let mut bits = mask;
            while bits != 0 {
                let b = bits.trailing_zeros();
                bits &= bits - 1;
                newest = newest.max(r.stamp_of(first + b * 64, 64));
            }
            newest
        };
        // The cached copy holds the blocks in `count`, as the registers were when the
        // file's write count was `stamp`; it serves this draw if it holds every block the
        // program names and none of those has been written since. A write to a block the
        // program does not name does not throw it out.
        let cache = self.pixel_window;
        let current = cache.valid && cache.base == u64::from(base) && written(cache.count) <= cache.stamp;
        if current && cache.count & blocks == blocks {
            self.reused[ArenaKind::Constants as usize] += u64::from(blocks.count_ones() * 256);
            return cache.at;
        }
        // A copy that is still current but lacks a block is copied again with what it held
        // as well, so that programs taking turns over one set of constants settle on one copy.
        let copy = if current { blocks | cache.count } else { blocks };
        let span = 32 - copy.leading_zeros(); // blocks to the highest one copied
        let at = self.allocate(span * 256, ArenaKind::Constants);
        if at == u32::MAX {
            return at;
        }
        let mut bits = copy;
        while bits != 0 {
            let b = bits.trailing_zeros();
            bits &= bits - 1;
            debug_assert!((first + b * 64 + 64) as usize <= r.values.len());
            // SAFETY: allocate gave span * 256 bytes at `at`, and b < span; the block's 64
            // registers are inside the file (above).
            unsafe {
                ptr::copy_nonoverlapping(
                    r.values.as_ptr().add((first + b * 64) as usize).cast::<u8>(),
                    self.mapped.add((at + b * 256) as usize),
                    256,
                )
            };
        }
        // allocate counted the whole span.
        self.written[ArenaKind::Constants as usize] -= u64::from((span - copy.count_ones()) * 256);
        #[cfg(feature = "diagnostics")]
        census::constants(Constants::PixelWindow, copy.count_ones() * 256);
        self.pixel_window = Cached { stamp: r.writes, base: u64::from(base), at, valid: true, count: copy };
        at
    }

    /// A vertex program's constants: each block of 16 its window reaches that has been
    /// written since its last copy into this slot is copied again, and the table it reads
    /// them through is written when one has moved or the window has. A draw changes a block
    /// or so -- its world matrix -- of the ten it reads.
    ///
    /// `needed` is at most 256 (constants_needed).
    fn write_constant_table(&mut self, r: &RegisterFile, needed: u32) -> u32 {
        let base = ConstantWindow(r.get(SQ_VS_CONST)).base().min(256);
        let mut moved = !self.constant_table.valid || self.constant_table.base != u64::from(base);
        if needed != 0 {
            // base <= 256 and needed <= 256: the last block is (256 + 255) / 16 = 31.
            for b in base / 16..=(base + needed - 1) / 16 {
                let b = b as usize & (CONSTANT_FILE_BLOCKS as usize - 1);
                let first = CONSTANT_BASE_ALU + b as u32 * 64;
                let stamp = r.stamps[(first >> STAMP_SHIFT) as usize];
                let block = self.constant_blocks[b];
                if block.valid && block.stamp == stamp {
                    self.reused[ArenaKind::Constants as usize] += 256;
                    continue;
                }
                let at = self.copy_registers(r, first, 64, ArenaKind::Constants);
                if at == u32::MAX {
                    return at;
                }
                #[cfg(feature = "diagnostics")]
                census::constants(Constants::VertexBlock, 256);
                self.constant_blocks[b] = ConstantBlock { stamp, valid: true };
                self.constant_table_words[4 + b] = at / 16;
                moved = true;
            }
        }
        if !moved {
            return self.constant_table.at;
        }
        self.constant_table_words[0] = base;
        let words = self.constant_table_words;
        // SAFETY: the table is this function's own array.
        let at = unsafe { self.copy_into_arena(words.as_ptr().cast(), CONSTANT_TABLE_WORDS * 4, ArenaKind::Constants) };
        if at == u32::MAX {
            return at;
        }
        #[cfg(feature = "diagnostics")]
        census::constants(Constants::Table, CONSTANT_TABLE_WORDS * 4);
        self.constant_table = Cached { stamp: 0, base: u64::from(base), at, valid: true, count: 0 };
        at
    }

    /// The five constant blocks of a draw: the vertex constants' table, the pixel window,
    /// the fetch window with its vertex fetches pointed at copies of what they name, the
    /// booleans and the loops. `ok` false when the arena ran out (the draw is skipped).
    #[allow(clippy::too_many_arguments)]
    pub fn write_constants<N: DrawNotes>(
        &mut self,
        r: &RegisterFile,
        program: &TranslationMeta,
        pixel_program: &TranslationMeta,
        lowest_vertex: u32,
        highest_vertex: u32,
        rectangles: u32,
        clock: ServeClock,
        notes: Option<&mut N>,
    ) -> ConstantOffsets {
        let mut notes = if cfg!(feature = "diagnostics") { notes } else { None };
        let subs = program.vertex_fetch_subs;
        let mut out = ConstantOffsets::default();
        // The pairs of the fetch window either program reads, and the vertex fetches
        // rewritten below in any case.
        let mut pairs = [0u32; 3];
        for (i, pair) in pairs.iter_mut().enumerate() {
            *pair = program.fetch_pairs_read[i] | pixel_program.fetch_pairs_read[i] | program.vertex_fetch_pairs[i];
        }

        out.vertex_floats = self.write_constant_table(r, constants_needed(program));
        if out.vertex_floats == u32::MAX {
            return out;
        }
        out.pixel_floats = self.write_pixel_constants(r, constant_blocks(pixel_program));
        if out.pixel_floats == u32::MAX {
            return out;
        }
        if let Some(n) = notes.as_mut() {
            n.constant_windows(r, constants_needed(program), constants_needed(pixel_program));
        }

        // The assembled block depends on the fetch registers and on which groups the shader
        // reads, and the vertex copies it points at stay where they are for the rest of the
        // frame -- so an unchanged pair is the same block. A rectangle list gets a copy of
        // its own, with the corners it does not carry written into it.
        let fetch_stamp = r.stamp_of(CONSTANT_BASE_FETCH, 32 * 6);
        let fetch_cached = rectangles == 0
            && self.fetch_block.valid
            && self.fetch_block.stamp == fetch_stamp
            && self.fetch_block.base == vertex_range(lowest_vertex, highest_vertex)
            && subs == self.fetch_subs
            && pairs == self.fetch_pairs;
        // The first reason the block is not the previous draw's, and whether this draw
        // copies any vertex data (rather than every fetch served by the shadow).
        #[cfg(feature = "diagnostics")]
        let cause = if fetch_cached {
            FetchMiss::Reused
        } else if rectangles != 0 {
            FetchMiss::Rectangles
        } else if !self.fetch_block.valid {
            FetchMiss::NewFrame
        } else if self.fetch_block.stamp != fetch_stamp {
            FetchMiss::Stamp
        } else if self.fetch_block.base != vertex_range(lowest_vertex, highest_vertex) {
            FetchMiss::Range
        } else if subs != self.fetch_subs {
            FetchMiss::Subs
        } else {
            FetchMiss::Pairs
        };
        #[cfg(feature = "diagnostics")]
        let mut copied_any = false;
        if fetch_cached {
            #[cfg(feature = "diagnostics")]
            census::fetch(FetchMiss::Reused, 0);
            if let Some(n) = notes.as_mut() {
                n.vertex_window(ptr::null(), 0); // the previous draw's
            }
            self.reused[ArenaKind::Constants as usize] += 96 * 16;
            out.fetch = self.fetch_block.at;
            out.booleans = self.booleans.at;
            out.loops = self.loops.at;
            out.ok = true;
            return out;
        }
        out.fetch = self.allocate(96 * 16, ArenaKind::Constants);
        if out.fetch == u32::MAX {
            return out;
        }

        // The fetch window is 32 groups of six dwords; the shader sees 96 constants of two
        // dwords each, padded to a vec4. Assembled here and copied across, only the pairs
        // the programs read: the arena is write-combined, and the rewriting below reads
        // back what it has just written. Entries no pair names are never read.
        let mut block = MaybeUninit::<Fetch>::uninit();
        let fetch = block.as_mut_ptr().cast::<u32>();
        for (i, &read) in pairs.iter().enumerate() {
            let mut bits = read;
            while bits != 0 {
                let pair = i * 32 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let source = CONSTANT_BASE_FETCH as usize + pair * 2;
                debug_assert!(pair < 96 && source + 1 < r.values.len());
                // SAFETY: pair < 96 (three words of 32 bits), so the entry is inside the
                // 384-dword block and the source inside the fetch constants.
                unsafe {
                    let entry = fetch.add(pair * 4);
                    entry.write(*r.values.get_unchecked(source));
                    entry.add(1).write(*r.values.get_unchecked(source + 1));
                    entry.add(2).write(0);
                    entry.add(3).write(0);
                }
            }
        }

        // A vertex fetch constant holds a physical address. The data each fetch names is
        // copied into the frame arena and the address rewritten to point at the copy, so
        // the shader's arithmetic is untouched and only what a draw reads crosses. What has
        // been copied for this draw, so two of its fetch slots naming one buffer copy it
        // once: it does not outlive the draw (below says why).
        let mut made = [const { MaybeUninit::<Made>::uninit() }; 96];
        let mut made_count = 0usize;
        let guest = guest_memory::base();
        for (i, &vertex_pairs) in program.vertex_fetch_pairs.iter().enumerate() {
            let mut bits = vertex_pairs;
            while bits != 0 {
                let slot = i * 32 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                debug_assert!(slot < 96);
                // SAFETY: slot < 96; every vertex pair is in `pairs`, so assembled above.
                let entry = unsafe { fetch.add(slot * 4) };
                // SAFETY: as above.
                let (word0, word1) = unsafe { (entry.read(), entry.add(1).read()) };
                let kind = word0 & 3;
                if kind != 3 {
                    continue; // not a vertex fetch
                }
                // SAFETY (each write of `entry` below): the entry assembled above.
                let declared = (word1 >> 2) & 0xFF_FFFF;
                if declared == 0 {
                    unsafe { entry.write(0) };
                    continue;
                }
                let physical = word0 & !3;
                let va = kernel::from_physical(physical);
                if va == 0 {
                    unsafe { entry.write(0) };
                    continue;
                }
                if let Some(n) = notes.as_mut() {
                    n.vertex_buffer(physical);
                }

                // The window the program reaches, not the extent the constant spans.
                // SAFETY: slot < 96, the arrays' length.
                let (stride, tail) = unsafe {
                    (*program.vertex_fetch_stride.get_unchecked(slot), *program.vertex_fetch_tail.get_unchecked(slot))
                };

                if rectangles != 0 && stride != 0 {
                    let given = rectangles.wrapping_mul(3);
                    let total = given.wrapping_add(rectangles);
                    let at = self.allocate(rectangle_vertex_bytes(rectangles, stride), ArenaKind::Vertices);
                    if at == u32::MAX {
                        return out;
                    }
                    // The fourth corners are worked out from the other three, so the
                    // vertices are put together here and not in the arena.
                    self.rectangle_vertices(va, given, total, stride, declared, rectangles, at, notes.as_mut());
                    #[cfg(feature = "diagnostics")]
                    {
                        copied_any = true;
                    }
                    unsafe { entry.write(at | kind) };
                    continue;
                }

                let from = u64::from(lowest_vertex) * u64::from(stride);
                let to = u64::from(highest_vertex) * u64::from(stride) + u64::from(tail);
                // The copy stops at the end of the buffer the fetch constant declares; a
                // window that starts past it has nothing to copy.
                if from >= u64::from(declared) {
                    unsafe { entry.write(0) };
                    continue;
                }
                let first = from as u32;
                let last = to.min(u64::from(declared)) as u32;
                if last <= first {
                    unsafe { entry.write(0) };
                    continue;
                }
                let words = last - first;
                #[cfg(feature = "diagnostics")]
                stream::note_read(va.wrapping_add(first * 4), u64::from(words) * 4);
                // The window's guest bytes: inside the 4 GB guest space whatever the
                // constant says, which the reservation maps whole.
                let window = guest.wrapping_add(va as usize + first as usize * 4);
                if let Some(n) = notes.as_mut() {
                    n.vertex_window(window, words as usize * 4);
                }

                // Pages the title has not written since the shadow copied them are read
                // there, at the guest's own address, and not copied.
                if self.shadow_base != 0
                    && va == PHYSICAL_BASE.wrapping_add(physical)
                    && serve(physical.wrapping_add(first * 4), words * 4, clock.recording, clock.completed, clock.frame)
                {
                    self.reused[ArenaKind::Vertices as usize] += u64::from(words * 4);
                    #[cfg(feature = "diagnostics")]
                    census::vertex_served(words * 4);
                    unsafe { entry.write(self.shadow_base.wrapping_add(physical) | kind) };
                    continue;
                }

                // One copy serves the other fetch slots of THIS draw that name the same
                // buffer, and no draw after it. Nothing stamps a write to guest memory the
                // way the register file stamps its own, so a copy made earlier in the frame
                // cannot be told from one the title has overwritten since -- and it does,
                // into a ring of dynamic geometry it reuses within the frame: a stale copy
                // draws one mesh through another's indices, a triangle across the screen.
                // An entry is made at most once a slot and the slots are 96, so made_count
                // never passes the array's length: every index below is in range.
                debug_assert!(made_count <= made.len());
                let entries = made.as_mut_ptr();
                let mut found = None;
                for k in 0..made_count {
                    // SAFETY: k < made_count (above); the first made_count entries are written.
                    if unsafe { (*entries.add(k)).assume_init_ref() }.address == physical {
                        found = Some(k);
                        break;
                    }
                }
                // SAFETY: a found entry is below made_count, written.
                let covered = found.is_some_and(|k| {
                    let m = unsafe { (*entries.add(k)).assume_init_ref() };
                    m.dwords != 0 && m.first <= first && m.first.wrapping_add(m.dwords) >= last
                });
                let k = if let (true, Some(k)) = (covered, found) {
                    self.reused[ArenaKind::Vertices as usize] += u64::from(words * 4);
                    k
                } else {
                    // Never the union with what was cached: draws reaching into a shared
                    // heap from both ends would ratchet it out to the whole buffer.
                    let at = self.allocate(words * 4, ArenaKind::Vertices);
                    if at == u32::MAX {
                        return out;
                    }
                    // Made by the copy thread, or here, before the consumer next settles
                    // (copy_worker.rs): the title rewrites none of the window before then.
                    // SAFETY: the window above, in guest memory; allocate gave words * 4
                    // bytes at `at`, which nothing else touches before the settle.
                    unsafe { super::copy_worker::hand_over(window, self.mapped.add(at as usize), words * 4) };
                    #[cfg(feature = "diagnostics")]
                    {
                        copied_any = true;
                        let why = if self.shadow_base == 0 {
                            VertexCopy::NoShadow
                        } else if va != PHYSICAL_BASE.wrapping_add(physical) {
                            VertexCopy::NotPhysical
                        } else {
                            use crate::runtime::gpu::memory_watch::{Refusal, last_refusal};
                            match last_refusal() {
                                Refusal::NoShadow => VertexCopy::NoShadow,
                                Refusal::Outside => VertexCopy::Outside,
                                Refusal::InFlight => VertexCopy::InFlight,
                                Refusal::BackingOff => VertexCopy::BackingOff,
                                Refusal::ArmFailed => VertexCopy::ArmFailed,
                            }
                        };
                        // SAFETY: the window just copied, guest memory.
                        let bytes = unsafe { std::slice::from_raw_parts(window, words as usize * 4) };
                        self.copied.copied(physical, first, last, why, bytes);
                    }
                    let k = found.unwrap_or_else(|| {
                        made_count += 1;
                        made_count - 1
                    });
                    debug_assert!(k < 96);
                    // SAFETY: k < made_count <= 96 (above).
                    unsafe {
                        (*entries.add(k)).write(Made {
                            address: physical,
                            base: at.wrapping_sub(first * 4),
                            first,
                            dwords: words,
                        })
                    };
                    k
                };
                // SAFETY: entry k was found or has just been written, below made_count.
                unsafe { entry.write((*entries.add(k)).assume_init_ref().base | kind) };
            }
        }

        let bool_stamp = r.stamp_of(CONSTANT_BASE_BOOL, 8);
        if self.booleans.valid && self.booleans.stamp == bool_stamp {
            self.reused[ArenaKind::Constants as usize] += 8 * 4;
            out.booleans = self.booleans.at;
        } else {
            out.booleans = self.copy_registers(r, CONSTANT_BASE_BOOL, 8, ArenaKind::Constants);
            if out.booleans == u32::MAX {
                return out;
            }
            #[cfg(feature = "diagnostics")]
            census::constants(Constants::Booleans, 32);
            self.booleans = Cached { stamp: bool_stamp, base: 0, at: out.booleans, valid: true, count: 0 };
        }

        let loop_stamp = r.stamp_of(CONSTANT_BASE_LOOP, 32);
        if self.loops.valid && self.loops.stamp == loop_stamp {
            self.reused[ArenaKind::Constants as usize] += 32 * 4;
            out.loops = self.loops.at;
        } else {
            out.loops = self.copy_registers(r, CONSTANT_BASE_LOOP, 32, ArenaKind::Constants);
            if out.loops == u32::MAX {
                return out;
            }
            #[cfg(feature = "diagnostics")]
            census::constants(Constants::Loops, 128);
            self.loops = Cached { stamp: loop_stamp, base: 0, at: out.loops, valid: true, count: 0 };
        }

        let mut written = 0u64;
        for (i, &read) in pairs.iter().enumerate() {
            let mut bits = read;
            while bits != 0 {
                let pair = i * 32 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                // SAFETY: the entry assembled above, into the block allocate gave (96 * 16
                // bytes from out.fetch).
                unsafe {
                    ptr::copy_nonoverlapping(
                        fetch.add(pair * 4).cast::<u8>(),
                        self.mapped.add(out.fetch as usize + pair * 16),
                        16,
                    )
                };
                written += 16;
            }
        }
        // allocate counted the whole block.
        self.written[ArenaKind::Constants as usize] -= 96 * 16 - written;
        #[cfg(feature = "diagnostics")]
        {
            census::constants(Constants::FetchBlock, written as u32);
            census::fetch(cause, written as u32);
            if !copied_any {
                census::fetch_all_served(cause);
            }
            let fetch_now = &r.values[CONSTANT_BASE_FETCH as usize..CONSTANT_BASE_FETCH as usize + 192];
            if cause == FetchMiss::Stamp {
                let mut same = true;
                for (i, &read) in pairs.iter().enumerate() {
                    let mut bits = read;
                    while bits != 0 {
                        let pair = i * 32 + bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        same &= fetch_now[pair * 2] == self.fetch_values[pair * 2]
                            && fetch_now[pair * 2 + 1] == self.fetch_values[pair * 2 + 1];
                    }
                }
                if same {
                    census::fetch_same_values();
                }
            }
            self.fetch_values.copy_from_slice(fetch_now);
        }
        self.fetch_block = Cached {
            stamp: fetch_stamp,
            base: vertex_range(lowest_vertex, highest_vertex),
            at: out.fetch,
            valid: true,
            count: 0,
        };
        self.fetch_subs = subs;
        self.fetch_pairs = pairs;
        out.ok = true;
        out
    }

    /// A rectangle list's vertices for one fetch: the three corners the title gave, then the
    /// fourth of each rectangle, into the arena at `at` (allocated for `total` vertices).
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn rectangle_vertices<N: DrawNotes>(
        &mut self,
        va: u32,
        given: u32,
        total: u32,
        stride: u32,
        declared: u32,
        rectangles: u32,
        at: u32,
        notes: Option<&mut &mut N>,
    ) {
        let mut corners = std::mem::take(&mut self.corners);
        corners.clear();
        corners.resize(total as usize * stride as usize, 0);
        // allocate took total * stride * 4 bytes, counted without wrapping
        // (rectangle_vertex_bytes), so total and given * stride did not wrap and
        // given * stride < total * stride, corners' length; the min is a second guard.
        let have = (given.wrapping_mul(stride).min(declared) as usize).min(corners.len());
        #[cfg(feature = "diagnostics")]
        stream::note_read(va, have as u64 * 4);
        let source = guest_memory::base().wrapping_add(va as usize);
        // SAFETY: have <= corners' length; the guest's bytes at va, inside its reservation.
        unsafe { ptr::copy_nonoverlapping(source, corners.as_mut_ptr().cast::<u8>(), have * 4) };
        for rect in 0..rectangles {
            build_fourth_corner(&mut corners, stride, rect, given + rect);
        }
        // SAFETY: allocate gave total * stride * 4 bytes at `at`, corners' size (none of
        // it wrapped: rectangle_vertex_bytes).
        unsafe {
            ptr::copy_nonoverlapping(corners.as_ptr().cast::<u8>(), self.mapped.add(at as usize), corners.len() * 4)
        };
        if let Some(n) = notes {
            n.vertex_window(source, have * 4);
        }
        self.corners = corners;
    }

    /// Indices are big-endian in guest memory and the hardware reads them that way; Vulkan
    /// does not. `lowest` and `highest` bound the window of the vertex buffer the draw can
    /// reach. Copied afresh for every draw: a cache could only serve a later draw from an
    /// earlier one's bytes, and the title rewrites its dynamic geometry's index buffers
    /// within the frame. u32::MAX when the
    /// arena is full, `lowest` and `highest` then left as they were.
    ///
    /// # Safety
    /// `count` indices (4 bytes each when `wide`, else 2) at guest `address` lie inside the
    /// guest's reservation: the draw's index buffer, from kernel::from_physical.
    pub unsafe fn write_indices<N: DrawNotes>(
        &mut self,
        address: u32,
        count: u32,
        wide: bool,
        lowest: &mut u32,
        highest: &mut u32,
        notes: Option<&mut N>,
    ) -> u32 {
        let bytes = arena_bytes(u64::from(count) * if wide { 4 } else { 2 });
        let at = self.allocate(bytes, ArenaKind::Indices);
        if at == u32::MAX {
            return at;
        }
        let source = guest_memory::base().wrapping_add(address as usize);
        #[cfg(feature = "diagnostics")]
        stream::note_read(address, u64::from(bytes));
        #[cfg(feature = "diagnostics")]
        census::indices(bytes);
        if cfg!(feature = "diagnostics")
            && let Some(n) = notes
        {
            n.indices(source, bytes as usize);
        }
        // SAFETY: allocate gave `bytes` at `at`, count indices; the guest's at `address`,
        // inside the reservation (the caller's).
        unsafe {
            let out = self.mapped.add(at as usize);
            // The AVX2 swaps store 32 bytes at a time, aligned (index_swap.rs).
            debug_assert!(out as usize % 32 == 0);
            if wide {
                let (low, high) = (self.swap32)(source, out.cast(), count);
                *lowest = low;
                *highest = high;
            } else {
                let (low, high) = (self.swap16)(source, out.cast(), count);
                *lowest = u32::from(low);
                *highest = u32::from(high);
            }
        }
        if *lowest > *highest {
            *lowest = *highest;
        }
        at
    }

    /// A rectangle list's indices, two triangles a rectangle, the fourth corner the one
    /// write_constants made after the title's: 16-bit while every
    /// vertex fits, 32-bit past 16,384 rectangles (65,536 vertices) where those would wrap
    /// (rectangle_indices_wide). u32::MAX when the arena is full.
    pub fn write_rectangle_indices(&mut self, rectangles: u32) -> u32 {
        if rectangle_indices_wide(rectangles) {
            return self.write_wide_rectangle_indices(rectangles);
        }
        let at = self.allocate(arena_bytes(u64::from(rectangles) * 6 * 2), ArenaKind::Indices);
        if at == u32::MAX {
            return at;
        }
        // SAFETY: allocate gave rectangles * 12 bytes at `at`, which the alignment keeps
        // 2-aligned; written, never read.
        let list = unsafe { self.mapped.add(at as usize).cast::<u16>() };
        for rect in 0..rectangles {
            let a = (rect * 3) as u16;
            let b = (rect * 3 + 1) as u16;
            let c = (rect * 3 + 2) as u16;
            let d = (rectangles * 3 + rect) as u16; // the corner made here
            let six = [a, b, c, a, c, d];
            // SAFETY: rect < rectangles, inside the allocation.
            unsafe { ptr::copy_nonoverlapping(six.as_ptr(), list.add(rect as usize * 6), 6) };
        }
        at
    }

    /// The same as 32-bit indices, as Xenia's built-in index buffer for expanded rectangle
    /// lists holds them (primitive_processor.cc: "may contain values above UINT16_MAX").
    #[cold]
    fn write_wide_rectangle_indices(&mut self, rectangles: u32) -> u32 {
        let at = self.allocate(arena_bytes(u64::from(rectangles) * 6 * 4), ArenaKind::Indices);
        if at == u32::MAX {
            return at;
        }
        // SAFETY: allocate gave rectangles * 24 bytes at `at`, which the alignment keeps
        // 4-aligned; written, never read. rectangles * 4 fits 32 bits: the allocation's
        // size, rectangles * 24, did.
        let list = unsafe { self.mapped.add(at as usize).cast::<u32>() };
        for rect in 0..rectangles {
            let (a, b, c) = (rect * 3, rect * 3 + 1, rect * 3 + 2);
            let d = rectangles * 3 + rect; // the corner made here
            let six = [a, b, c, a, c, d];
            // SAFETY: rect < rectangles, inside the allocation.
            unsafe { ptr::copy_nonoverlapping(six.as_ptr(), list.add(rect as usize * 6), 6) };
        }
        WIDE_RECTANGLE_LISTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        at
    }
}

/// Whether a rectangle list's indices are 32-bit: its 4 * rectangles vertices, numbered
/// from 0, do not all fit 16 bits (no draw enables primitive restart, so 0xFFFF is a
/// vertex). The guest's 16-bit vertex count holds at most 21,845 rectangles.
#[inline]
pub fn rectangle_indices_wide(rectangles: u32) -> bool {
    rectangles > 0x4000
}

/// Rectangle lists drawn with 32-bit indices, for the report.
pub static WIDE_RECTANGLE_LISTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The fetch window as a draw assembles it: 96 entries of four dwords, 64-aligned.
#[repr(C, align(64))]
struct Fetch([u32; 96 * 4]);

/// A size for allocate: one that does not fit 32 bits asks for more than any share holds,
/// where a 32-bit product would wrap to a small allocation its copy then writes past.
#[inline]
fn arena_bytes(bytes: u64) -> u32 {
    u32::try_from(bytes).unwrap_or(u32::MAX)
}

// A rectangle list's vertex bytes for one fetch, 4 corners of `stride` dwords a rectangle,
// counted without wrapping: past 2^30 rectangles a 32-bit total wraps to a small
// allocation its copy then writes past, and a stride near 2^31 would wrap even 64 bits.
// Saturates to u32::MAX, which allocate refuses.
fn rectangle_vertex_bytes(rectangles: u32, stride: u32) -> u32 {
    arena_bytes((u64::from(rectangles) * 16).saturating_mul(u64::from(stride)))
}

#[derive(Clone, Copy)]
struct Made {
    address: u32,
    base: u32,
    first: u32,
    dwords: u32,
}

// What the rest of the renderer reads of the arena (S9e: renderer_frame.rs, renderer.rs).
impl Arena {
    /// No arena: the renderer before its start and after its end. Every allocation fails
    /// (the share is empty) and nothing is written.
    pub fn empty() -> Arena {
        // SAFETY: no bytes, and nothing writes through it: the renderer allocates from its
        // arena only while it has a device (Draw returns first without one), and its start
        // replaces this with the real one.
        unsafe { Arena::new(ptr::null_mut(), 0, 256, 0) }
    }

    /// The mapping is gone (the renderer's shutdown): nothing is written through it again --
    /// every allocation fails -- and the counters stay for the report.
    pub fn unmapped(&mut self) {
        self.mapped = ptr::null_mut();
        self.slot_bytes = 0;
    }

    /// How much of the current slot's share is spent (State::arenaUsed).
    #[inline]
    pub fn used(&self) -> u32 {
        self.used
    }

    /// One slot's share (State::arenaSlotBytes).
    #[inline]
    pub fn slot_bytes(&self) -> u32 {
        self.slot_bytes
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
    /// The mapped bytes the current slot's share starts at, for the trace's hash of it.
    #[inline]
    pub fn share_start(&self) -> *const u8 {
        self.mapped.wrapping_add(self.base as usize)
    }

    /// The mapping, `at` bytes in: where a draw's record goes, and what MW2_TRACE_SHADER_VERTS
    /// reads back.
    #[inline]
    pub fn mapped_at(&self, at: u32) -> *mut u8 {
        self.mapped.wrapping_add(at as usize)
    }

    /// The whole arena's size, which the shadow follows.
    #[inline]
    pub fn bytes(&self) -> u32 {
        self.bytes
    }
}
