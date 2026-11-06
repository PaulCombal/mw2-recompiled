// The command processor: executes the PM4 stream the title's own D3D9 builds, the way
// the Xenos front end does -- registers, constants, shader loads, memory writes, waits,
// events and draws, in stream order (runtime/gpu/command_processor.cpp). Each draw goes
// to the C++ renderer through runtime/rust_calls.cpp, one call a draw, with the register
// file by pointer; docs/d3d9-seam.md is the reference for what each packet does.
//
// Unsafe by nature: it reads the ring and the indirect buffers, the shaders and the
// constants straight from guest memory, as raw big-endian dwords, and writes the
// command processor's own results there (never through a slice: guest threads write
// the same memory). Every address it follows is a guest virtual one the kernel's
// physical mapping gave (kernel::from_physical), inside the 4 GB the guest base maps.
// The state is the ring consumer's alone, as the C++'s file statics were: one
// &mut CommandProcessor is made at each batch and passed down, and nothing a batch calls
// makes another (the arena's writes, the occlusion deliveries and the reports use only
// the statics beside it).
#![deny(unsafe_op_in_unsafe_fn)]

use super::hash::{MulBuild, shader_hash};
use super::registers::{self as r, CONSTANT_BASE_ALU, DrawInitiator, EdramMode, REGISTER_COUNT, RegisterFile};
use super::{arena, image_move};
use crate::log::log_warn;
#[cfg(feature = "diagnostics")]
use crate::log::{log_info, log_kernel};
use crate::runtime::bridge::{renderer, watch};
use crate::runtime::guest_memory::{self, PHYSICAL_BASE};
use crate::runtime::kernel;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "diagnostics")]
use crate::runtime::bridge::stream;
#[cfg(feature = "diagnostics")]
use std::collections::HashSet;

// Registers are plain memory in this address space; the CPU writes some straight
// through the aperture rather than through the command stream (gpu/internal.h).
pub const REGISTER_APERTURE: u32 = 0x7FC8_0000;
const SCRATCH_UMSK: u32 = 0x01DC;
const SCRATCH_ADDR: u32 = 0x01DD;
const SCRATCH_REG0: u32 = 0x0578;

// Same command processor family as Adreno A2xx.
const PM4_NOP: u32 = 0x10;
const PM4_INDIRECT_BUFFER: u32 = 0x3F;
const PM4_INDIRECT_BUFFER_PFD: u32 = 0x37;
const PM4_WAIT_FOR_IDLE: u32 = 0x26;
const PM4_WAIT_REG_MEM: u32 = 0x3C;
const PM4_REG_RMW: u32 = 0x21;
const PM4_REG_TO_MEM: u32 = 0x3E;
const PM4_MEM_WRITE: u32 = 0x3D;
const PM4_COND_WRITE: u32 = 0x45;
const PM4_EVENT_WRITE: u32 = 0x46;
const PM4_EVENT_WRITE_SHD: u32 = 0x58;
const PM4_EVENT_WRITE_CFL: u32 = 0x59;
const PM4_EVENT_WRITE_ZPD: u32 = 0x5B;
const PM4_DRAW_INDX: u32 = 0x22;
const PM4_DRAW_INDX_2: u32 = 0x36;
const PM4_SET_CONSTANT: u32 = 0x2D;
const PM4_SET_CONSTANT2: u32 = 0x55;
const PM4_IM_LOAD: u32 = 0x27;
const PM4_IM_LOAD_IMMEDIATE: u32 = 0x2B;
const PM4_INVALIDATE_STATE: u32 = 0x3B;
const PM4_SET_BIN_MASK: u32 = 0x50;
const PM4_SET_BIN_SELECT: u32 = 0x51;
const PM4_CONTEXT_UPDATE: u32 = 0x5E;
const PM4_INTERRUPT: u32 = 0x54;
const PM4_ME_INIT: u32 = 0x48;
const PM4_VIZ_QUERY: u32 = 0x23;
const PM4_LOAD_ALU_CONSTANT: u32 = 0x2F;
const PM4_EVENT_WRITE_EXT: u32 = 0x5A;
const PM4_SET_BIN_MASK_LO: u32 = 0x60;
const PM4_SET_BIN_MASK_HI: u32 = 0x61;
const PM4_SET_BIN_SELECT_LO: u32 = 0x62;
const PM4_SET_BIN_SELECT_HI: u32 = 0x63;
const PM4_XE_SWAP: u32 = 0x64;

const fn opcode_name(op: u32) -> Option<&'static std::ffi::CStr> {
    Some(match op {
        PM4_NOP => c"NOP",
        PM4_INDIRECT_BUFFER => c"INDIRECT_BUFFER",
        PM4_INDIRECT_BUFFER_PFD => c"INDIRECT_BUFFER_PFD",
        PM4_WAIT_FOR_IDLE => c"WAIT_FOR_IDLE",
        PM4_WAIT_REG_MEM => c"WAIT_REG_MEM",
        PM4_REG_RMW => c"REG_RMW",
        PM4_REG_TO_MEM => c"REG_TO_MEM",
        PM4_MEM_WRITE => c"MEM_WRITE",
        PM4_COND_WRITE => c"COND_WRITE",
        PM4_EVENT_WRITE => c"EVENT_WRITE",
        PM4_EVENT_WRITE_SHD => c"EVENT_WRITE_SHD",
        PM4_EVENT_WRITE_CFL => c"EVENT_WRITE_CFL",
        PM4_EVENT_WRITE_ZPD => c"EVENT_WRITE_ZPD",
        PM4_DRAW_INDX => c"DRAW_INDX",
        PM4_DRAW_INDX_2 => c"DRAW_INDX_2",
        PM4_SET_CONSTANT => c"SET_CONSTANT",
        PM4_SET_CONSTANT2 => c"SET_CONSTANT2",
        PM4_IM_LOAD => c"IM_LOAD",
        PM4_IM_LOAD_IMMEDIATE => c"IM_LOAD_IMMEDIATE",
        PM4_INVALIDATE_STATE => c"INVALIDATE_STATE",
        PM4_SET_BIN_MASK => c"SET_BIN_MASK",
        PM4_SET_BIN_SELECT => c"SET_BIN_SELECT",
        PM4_CONTEXT_UPDATE => c"CONTEXT_UPDATE",
        PM4_INTERRUPT => c"INTERRUPT",
        PM4_ME_INIT => c"ME_INIT",
        PM4_VIZ_QUERY => c"VIZ_QUERY",
        PM4_LOAD_ALU_CONSTANT => c"LOAD_ALU_CONSTANT",
        PM4_EVENT_WRITE_EXT => c"EVENT_WRITE_EXT",
        PM4_SET_BIN_MASK_LO => c"SET_BIN_MASK_LO",
        PM4_SET_BIN_MASK_HI => c"SET_BIN_MASK_HI",
        PM4_SET_BIN_SELECT_LO => c"SET_BIN_SELECT_LO",
        PM4_SET_BIN_SELECT_HI => c"SET_BIN_SELECT_HI",
        PM4_XE_SWAP => c"XE_SWAP",
        _ => return None,
    })
}

// Which opcodes opcode_name knows, looked up once a packet rather than through the match.
const KNOWN_OPCODES: [bool; 128] = {
    let mut known = [false; 128];
    let mut op = 0;
    while op < 128 {
        known[op] = opcode_name(op as u32).is_some();
        op += 1;
    }
    known
};

// The five constant windows SET_CONSTANT and LOAD_ALU_CONSTANT name by type.
const CONSTANT_WINDOWS: [u32; 5] = [
    r::CONSTANT_BASE_ALU,
    r::CONSTANT_BASE_FETCH,
    r::CONSTANT_BASE_BOOL,
    r::CONSTANT_BASE_LOOP,
    r::CONSTANT_BASE_REGISTERS,
];

// One pass over the ring must finish. A pair of dwords misread as an indirect buffer
// sends the parse into unrelated memory where everything it finds can be another
// indirect buffer, so a batch's work is bounded, far above a real frame (~50k dwords).
const BATCH_WORK_LIMIT: u64 = 8 << 20;

/// A statistic for the reports: single writer, relaxed load and store (counter.h), or
/// nothing in a release build (diag::Stat). Nothing the game does reads one.
pub(super) struct Stat(#[cfg(feature = "diagnostics")] AtomicU64);

impl Stat {
    #[allow(clippy::new_without_default)]
    pub(super) const fn new() -> Self {
        Stat(
            #[cfg(feature = "diagnostics")]
            AtomicU64::new(0),
        )
    }

    #[inline(always)]
    pub(super) fn add(&self, _n: u64) {
        #[cfg(feature = "diagnostics")]
        self.0.store(self.0.load(Relaxed).wrapping_add(_n), Relaxed);
    }

    #[inline(always)]
    pub(super) fn bump(&self) {
        self.add(1);
    }

    #[inline(always)]
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(super) fn get(&self) -> u64 {
        #[cfg(feature = "diagnostics")]
        return self.0.load(Relaxed);
        #[cfg(not(feature = "diagnostics"))]
        0
    }
}

// What the report prints, read after the consumer has stopped or, on a fault, at any
// time: atomics in statics (D13). A release has no report.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
struct Stats {
    packets: Stat,
    type_count: [Stat; 4],
    opcode_count: [Stat; 128],
    padding: Stat,
    buffers: Stat,
    buffers_lost_step: Stat,
    implausible_buffers: Stat,
    batches_abandoned: Stat,
    shader_loads_refused: Stat,
    predicated_run: Stat,
    predicated_skipped: Stat,
    register_writes_out_of_range: Stat,
    // WAIT_REG_MEM: seen, found not yet satisfied, time spent waiting, given up on.
    reg_mem_waits: Stat,
    reg_mem_stalls: Stat,
    reg_mem_wait_micros: Stat,
    reg_mem_timeouts: Stat,
    // A frame's cost as the consumer sees it, swap to swap: its own CPU time, and the
    // wall clock.
    frame_cpu: [Stat; FRAME_BINS],
    frame_wall: [Stat; FRAME_BINS],
    shaders_seen: Stat,
}

const FRAME_BINS: usize = 7;

static STATS: Stats = Stats {
    packets: Stat::new(),
    type_count: [const { Stat::new() }; 4],
    opcode_count: [const { Stat::new() }; 128],
    padding: Stat::new(),
    buffers: Stat::new(),
    buffers_lost_step: Stat::new(),
    implausible_buffers: Stat::new(),
    batches_abandoned: Stat::new(),
    shader_loads_refused: Stat::new(),
    predicated_run: Stat::new(),
    predicated_skipped: Stat::new(),
    register_writes_out_of_range: Stat::new(),
    reg_mem_waits: Stat::new(),
    reg_mem_stalls: Stat::new(),
    reg_mem_wait_micros: Stat::new(),
    reg_mem_timeouts: Stat::new(),
    frame_cpu: [const { Stat::new() }; FRAME_BINS],
    frame_wall: [const { Stat::new() }; FRAME_BINS],
    shaders_seen: Stat::new(),
};
// Counted in every build: the first 32 are warned of.
static UNKNOWN_OPCODES: AtomicU32 = AtomicU32::new(0);
// PM4_XE_SWAP packets met: the image pool, the memory watch and the capture go by it.
static SWAPS_PARSED: AtomicU64 = AtomicU64::new(0);
// A WAIT_FOR_IDLE with no draw after it: the GPU has finished every draw so far, so
// whatever the command processor writes or raises now tells the title they are done
// (vk::renderer::BeforeCompletion). The consumer's; an atomic so that write_physical,
// which the arena calls in the middle of a batch, needs no command processor.
static IDLE_SINCE_DRAW: AtomicBool = AtomicBool::new(false);

// The display's colour table: 256 entries of ten bits per channel, blue in the low bits.
// The title rewrites the whole ramp as one sweep of entry 0 to entry 255, so the sweep
// in progress (the command processor's) is kept apart from the last one that finished:
// a present that lands mid-sweep must not see half of each.
static DISPLAY_LUT: Mutex<[u32; 256]> = Mutex::new([0; 256]);
static DISPLAY_LUT_READY: AtomicBool = AtomicBool::new(false);

fn lut() -> std::sync::MutexGuard<'static, [u32; 256]> {
    DISPLAY_LUT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An IM_LOAD's microcode and its hash, for as long as the memory watch says nothing
/// has written its pages. Boxed in the memo, so a stage's shader can point at it; a
/// reload replaces the code's contents in place (the key holds its length).
#[derive(Default)]
struct Loaded {
    stamp: u64,
    hash: u64,
    code: Box<[u32]>,
}

/// Where a stage's shader is, as the C++'s pointer to the vector held it: the draw
/// reads the code where it is then.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shader {
    None,
    Immediate,
    Load(*const Loaded),
}

pub struct CommandProcessor {
    registers: RegisterFile,
    // Predication: a type-3 packet with bit 0 set runs only when the bin select and the
    // bin mask share a bit.
    bin_select: u64,
    bin_mask: u64,
    // The shaders the last IM_LOADs set: a draw uses whichever is loaded, as it uses
    // whatever registers are set.
    shader: [Shader; 2],
    shader_hash: [u64; 2],
    immediate: [Vec<u32>; 2],
    loads: Option<HashMap<u64, Box<Loaded>, MulBuild>>,
    #[cfg(feature = "diagnostics")]
    shaders_seen: Option<HashSet<u64, MulBuild>>,
    #[cfg(feature = "diagnostics")]
    shader_loads_told: Option<HashSet<u64, MulBuild>>,
    #[cfg(feature = "diagnostics")]
    last_swap_cpu: i64,
    #[cfg(feature = "diagnostics")]
    last_swap_wall: i64,
    batch_work: u64,
    // The sweep of the display table in progress.
    display_lut_pending: [u32; 256],
}

impl CommandProcessor {
    const fn new() -> Self {
        CommandProcessor {
            registers: RegisterFile::new(),
            bin_select: 0xFFFF_FFFF,
            bin_mask: 0xFFFF_FFFF,
            shader: [Shader::None; 2],
            shader_hash: [0; 2],
            immediate: [Vec::new(), Vec::new()],
            loads: None,
            #[cfg(feature = "diagnostics")]
            shaders_seen: None,
            #[cfg(feature = "diagnostics")]
            shader_loads_told: None,
            #[cfg(feature = "diagnostics")]
            last_swap_cpu: 0,
            #[cfg(feature = "diagnostics")]
            last_swap_wall: 0,
            batch_work: 0,
            display_lut_pending: [0; 256],
        }
    }
}

struct Consumer(UnsafeCell<CommandProcessor>);
// SAFETY: only the ring consumer touches it (and the replay's restore before the
// consumer runs a batch, and the capture's save between batches, on the consumer).
unsafe impl Sync for Consumer {}
static CONSUMER: Consumer = Consumer(UnsafeCell::new(CommandProcessor::new()));

/// # Safety
/// The caller is the ring consumer (or the replay's restore before it runs), and no
/// other reference to the command processor is alive.
unsafe fn consumer() -> &'static mut CommandProcessor {
    // SAFETY: the caller's promise.
    unsafe { &mut *CONSUMER.0.get() }
}

// What the command processor asks of the GPU's threads and the guest's clock. The rig
// (tools/gpu_diff.py --rig pm4) answers these itself, as its C++ half's stubs do.
mod host {
    #[cfg(mw2_gpu_rig)]
    unsafe extern "C" {
        fn rig_running() -> bool;
        fn rig_cp_interrupt(cpu_mask: u32);
        fn rig_timebase() -> u64;
    }

    #[inline]
    pub fn running() -> bool {
        #[cfg(mw2_gpu_rig)]
        // SAFETY: the rig's function.
        return unsafe { rig_running() };
        #[cfg(not(mw2_gpu_rig))]
        super::super::gpu::running()
    }

    pub fn raise_interrupt(cpu_mask: u32) {
        #[cfg(mw2_gpu_rig)]
        // SAFETY: the rig's function.
        unsafe {
            rig_cp_interrupt(cpu_mask)
        };
        #[cfg(not(mw2_gpu_rig))]
        super::super::gpu::raise_command_processor_interrupt(cpu_mask);
    }

    pub fn timebase() -> u64 {
        #[cfg(mw2_gpu_rig)]
        // SAFETY: the rig's function.
        return unsafe { rig_timebase() };
        #[cfg(not(mw2_gpu_rig))]
        // SAFETY: the runtime's (src/runtime/exports.rs), which every mftb reads.
        unsafe {
            ppc_rt::PPC_QueryTimebase()
        }
    }
}

#[inline(always)]
fn base() -> *mut u8 {
    guest_memory::base()
}

/// A dword of guest memory as the guest holds it, big-endian, read as a value.
///
/// # Safety
/// `va` is a guest address inside the mapping.
#[inline(always)]
unsafe fn be_u32(va: u32) -> u32 {
    // SAFETY: the caller's; guest memory is mapped whole, and a page never touched is
    // committed by the fault handler.
    u32::from_be(unsafe { base().wrapping_add(va as usize).cast::<u32>().read() })
}

/// # Safety
/// As be_u32.
#[inline(always)]
unsafe fn store_be_u32(va: u32, value: u32) {
    // SAFETY: the caller's.
    unsafe { base().wrapping_add(va as usize).cast::<u32>().write(value.to_be()) }
}

// ---- the source of dwords -------------------------------------------------------------

struct Source {
    base: u32,
    index: u32,
    wrap_mask: u32, // 0 for a linear indirect buffer
    remaining: u32,
    // Set when the buffer produced a packet that cannot be one: the rest of it is not
    // read, as Xenia's command processor ends a buffer there.
    lost_step: bool,
    // Where `base` is, and what an index is masked with: all ones for a linear buffer.
    // A dword is read for every one the title wrote, so reading one is a mask and a
    // load, with no branch.
    words: *const u32,
    index_mask: u32,
}

impl Source {
    fn new(base: u32, index: u32, wrap_mask: u32, remaining: u32) -> Self {
        Source {
            base,
            index,
            wrap_mask,
            remaining,
            lost_step: false,
            words: self::base().wrapping_add(base as usize).cast::<u32>(),
            index_mask: if wrap_mask != 0 { wrap_mask } else { !0 },
        }
    }

    #[inline(always)]
    fn next(&mut self) -> u32 {
        // SAFETY: the ring or an indirect buffer the parse was handed, in guest memory
        // (a linear buffer is at most 16k dwords, the ring masked to its size).
        let word = unsafe { self.words.wrapping_add((self.index & self.index_mask) as usize).read() };
        self.index = self.index.wrapping_add(1);
        self.remaining -= u32::from(self.remaining != 0);
        u32::from_be(word)
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        let n = n.min(self.remaining);
        self.index = self.index.wrapping_add(n);
        self.remaining -= n;
    }

    /// The next `n` dwords as the guest holds them, big-endian, and past them: where
    /// they lie in one piece, in place; where they wrap round the ring, gathered into
    /// `room`.
    #[inline(always)]
    fn take(&mut self, room: &mut [MaybeUninit<u32>; 256], n: u32) -> *const u32 {
        let mut taken = self.words.wrapping_add((self.index & self.index_mask) as usize);
        if self.wrap_mask != 0 && (self.index & self.wrap_mask) + n > self.wrap_mask + 1 {
            for (i, slot) in room.iter_mut().take(n as usize).enumerate() {
                let at = self.index.wrapping_add(i as u32) & self.wrap_mask;
                // SAFETY: inside the ring.
                slot.write(unsafe { self.words.wrapping_add(at as usize).read() });
            }
            taken = room.as_ptr().cast::<u32>();
        }
        self.index = self.index.wrapping_add(n);
        self.remaining -= self.remaining.min(n);
        taken
    }

    fn address_of(&self, at: u32) -> u32 {
        self.base.wrapping_add(if self.wrap_mask != 0 { at & self.wrap_mask } else { at }.wrapping_mul(4))
    }
}

// ---- settings, read once ----------------------------------------------------------------

/// False under MW2_RENDER=0: the stream is executed and counted, nothing drawn.
pub fn rendering() -> bool {
    // A release build reads no debug switch (diag::Text).
    if !cfg!(feature = "diagnostics") {
        return true;
    }
    static ON: LazyLock<bool> =
        LazyLock::new(|| crate::env::text(c"MW2_RENDER").is_none() || crate::env::flag(c"MW2_RENDER"));
    *ON
}

// MW2_WATCH=<hex>[:bytes]: the command processor's own writes into the watched range are
// reported here; the CPU's go through watchpoint.rs.
#[cfg(feature = "diagnostics")]
fn watched() -> (u32, u32) {
    static WATCH: LazyLock<(u32, u32)> = LazyLock::new(|| {
        let Some(text) = crate::env::text(c"MW2_WATCH") else { return (0, 0) };
        let (lo, rest) = crate::env::parse_number(text, 16);
        let lo = lo as u32;
        let bytes = match rest.to_bytes().first() {
            Some(b':') => crate::env::parse_number(&rest[1..], 0).0 as u32,
            _ => 4,
        };
        (lo, lo.wrapping_add(if bytes != 0 { bytes } else { 4 }))
    });
    *WATCH
}

// MW2_DUMP_SHADERS=<dir>: every distinct shader's microcode, big-endian as the title
// holds it, for tools/xenos_shader.py and translate-shader.
#[cfg(feature = "diagnostics")]
fn shader_dump_directory() -> Option<&'static std::ffi::CStr> {
    static DIR: LazyLock<Option<&'static std::ffi::CStr>> = LazyLock::new(|| crate::env::text(c"MW2_DUMP_SHADERS"));
    *DIR
}

// ---- writes -------------------------------------------------------------------------

/// The command processor's write of a dword at a physical address.
pub fn write_physical(physical_address: u32, value: u32) {
    let va = kernel::from_physical(physical_address & !3);
    if va == 0 {
        return;
    }
    // The title reads this as soon as it lands. Without a wait for idle, the command
    // processor writes as it meets the packet, ahead of the draws before it, and the
    // title can conclude nothing about them from it.
    if IDLE_SINCE_DRAW.load(Relaxed) {
        renderer::before_completion();
    }
    #[cfg(feature = "diagnostics")]
    {
        let (lo, hi) = watched();
        if va >= lo && va < hi {
            // SAFETY: a guest address the physical mapping gave.
            let was = unsafe { be_u32(va) };
            log_kernel!(
                "watch: command processor writes %08X = %08X (physical %08X, was %08X)",
                va,
                value,
                physical_address,
                was
            );
        }
        stream::note_write(va, 4);
    }
    // SAFETY: as above.
    unsafe { store_be_u32(va, value) };
    #[cfg(feature = "diagnostics")]
    stream::note_written(va, 4);
}

/// A register the command stream may never have written: then the value the CPU put in
/// the aperture.
fn register_or_aperture(registers: &RegisterFile, index: u32) -> u32 {
    let v = registers.get(index);
    if v != 0 {
        return v;
    }
    // SAFETY: the aperture is guest memory.
    let held = unsafe { be_u32(REGISTER_APERTURE.wrapping_add(index.wrapping_mul(4))) };
    #[cfg(feature = "diagnostics")]
    if stream::capturing() {
        stream::aperture_read(index, held);
    }
    held
}

// The occlusion query. The command processor leaves eight counters in a 32-byte record
// -- { total, z-fail, z-pass, stencil-fail } x two lanes the title sums -- and they are
// little-endian, because D3D byte-swaps them when it reads them back (`lwbrx`). A query
// owns a 64-byte slot: the end record at its base and the begin record 32 bytes above,
// and the answer the title computes is the end's z-pass count less the begin's.
//
// D3D marks the record it is waiting for by writing FFFFFEED into both z-pass lanes,
// and D3DQuery::GetData reports "not finished yet" until they change. The two events
// bracket the draws being counted, so they open and close a real Vulkan occlusion
// query; the begin record is written as zero at once, and the end record carries the
// whole count, written when the GPU has finished counting -- as the console's GPU
// writes it.
fn write_sample_counts(address: u32, count: u32) {
    const RECORD_BYTES: u32 = 0x20;
    let record = address & !(RECORD_BYTES - 1);
    let va = if record != 0 { kernel::from_physical(record) } else { 0 };
    if va == 0 {
        return;
    }
    #[cfg(feature = "diagnostics")]
    stream::note_write(va, RECORD_BYTES);
    // Little-endian, so the host's own word order is what the title wants.
    let words = [count, 0, 0, 0, count, 0, 0, 0];
    // SAFETY: 32 bytes of guest memory at an aligned record.
    unsafe { ptr::copy_nonoverlapping(words.as_ptr(), base().wrapping_add(va as usize).cast::<u32>(), 8) };
    #[cfg(feature = "diagnostics")]
    stream::note_written(va, RECORD_BYTES);
}

// The title reads the count as a signed difference; saturate rather than hand it a
// number that comes back negative.
extern "C" fn write_end_record(address: u32, samples: u64) {
    write_sample_counts(address, samples.min(0x7FFF_FFFF) as u32);
}

fn sample_count_event(address: u32) {
    // The begin record is the one with bit 5 of its address set.
    if address & 0x20 != 0 {
        renderer::begin_occlusion_query();
        write_sample_counts(address, 0);
        return;
    }
    if let Some(samples) = renderer::end_occlusion_query(address) {
        write_end_record(address, samples);
    }
}

/// Writes the records of the occlusion queries the GPU has finished counting.
pub fn deliver_occlusion_queries() {
    renderer::collect_occlusion_queries(write_end_record);
}

// MW2_TRACE_SHADER_LOADS: each distinct shader the stream loads, once -- from memory (at
// its address) or from the packet itself (at 0).
#[cfg(feature = "diagnostics")]
fn trace_shader_load(told: &mut Option<HashSet<u64, MulBuild>>, kind: u32, code: &[u32], va: u32) {
    static TRACED: LazyLock<bool> = LazyLock::new(|| crate::env::flag(c"MW2_TRACE_SHADER_LOADS"));
    if !*TRACED || code.is_empty() {
        return;
    }
    let told = told.get_or_insert_with(Default::default);
    let hash = shader_hash(code);
    if told.len() < 20000 && told.insert(hash ^ u64::from(kind)) {
        log_info!(
            "shader load: %s %llu dwords at %08X, %016llX",
            if kind != 0 { c"pixel" } else { c"vertex" },
            code.len() as u64,
            va,
            hash
        );
    }
}

// Bins in milliseconds: under 8, 12, 16.7, 20, 33.3, 50, and above.
#[cfg(feature = "diagnostics")]
fn frame_bin(ms: f64) -> usize {
    const EDGES: [f64; 6] = [8.0, 12.0, 16.7, 20.0, 33.3, 50.0];
    let mut bin = 0;
    while bin < 6 && ms >= EDGES[bin] {
        bin += 1;
    }
    bin
}

impl CommandProcessor {
    #[cfg(feature = "diagnostics")]
    fn note_frame_cost(&mut self) {
        let cpu_now = crate::runtime::platform::thread_cpu_time() as i64;
        let wall_now = crate::runtime::clock::nanoseconds() as i64;
        if self.last_swap_wall != 0 {
            STATS.frame_cpu[frame_bin((cpu_now - self.last_swap_cpu) as f64 / 1e6)].bump();
            STATS.frame_wall[frame_bin((wall_now - self.last_swap_wall) as f64 / 1e6)].bump();
        }
        self.last_swap_cpu = cpu_now;
        self.last_swap_wall = wall_now;
    }

    fn write_register(&mut self, index: u32, value: u32) {
        if !RegisterFile::in_range(index) {
            STATS.register_writes_out_of_range.bump();
            return;
        }
        self.registers.write(index, value);

        // A scratch register whose bit is set in SCRATCH_UMSK is written back to memory
        // at SCRATCH_ADDR + 4n, as Xenia's command processor does. The title writes one
        // and then WAIT_REG_MEMs on that memory to know the GPU has got this far.
        if (SCRATCH_REG0..=SCRATCH_REG0 + 7).contains(&index) {
            let n = index - SCRATCH_REG0;
            let umsk = register_or_aperture(&self.registers, SCRATCH_UMSK);
            if umsk & (1 << n) != 0 {
                write_physical(register_or_aperture(&self.registers, SCRATCH_ADDR).wrapping_add(n * 4), value);
            }
            return;
        }

        // The display's own colour lookup table, which the scanout hardware applies to
        // the front buffer. The title fills it by pointing DC_LUT_RW_INDEX at an entry
        // and writing DC_LUT_30_COLOR, which then steps the index on by itself.
        if index == r::DC_LUT_30_COLOR {
            let entry = (self.registers.get(r::DC_LUT_RW_INDEX) & 0xFF) as usize;
            let mask = self.registers.get(r::DC_LUT_WRITE_EN_MASK) & 7;
            let held = &mut self.display_lut_pending[entry];
            if mask & 1 != 0 {
                *held = (*held & !0x0000_03FF) | (value & 0x3FF);
            }
            if mask & 2 != 0 {
                *held = (*held & !0x000F_FC00) | (value & 0xF_FC00);
            }
            if mask & 4 != 0 {
                *held = (*held & !0x3FF0_0000) | (value & 0x3FF0_0000);
            }
            self.registers.write(r::DC_LUT_RW_INDEX, ((entry + 1) & 0xFF) as u32);
            // Entry 255 ends the sweep: the ramp the title means is complete.
            if entry == 0xFF {
                *lut() = self.display_lut_pending;
                DISPLAY_LUT_READY.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }

    // A run of `count` registers from `first`, read from `s`. Above the ordinary
    // registers' start nothing has a side effect -- every constant window is there -- so
    // a run there goes into the file a block at a time.
    #[inline]
    fn write_registers(&mut self, mut first: u32, mut count: u32, s: &mut Source) {
        if first < r::CONSTANT_BASE_REGISTERS || first.wrapping_add(count) > REGISTER_COUNT {
            for i in 0..count {
                let value = s.next();
                self.write_register(first.wrapping_add(i), value);
            }
            return;
        }
        // Left unset, as the C++'s: only what take() gathers into it is read.
        let mut room = [MaybeUninit::<u32>::uninit(); 256];
        while count != 0 {
            let n = count.min(256);
            let taken = s.take(&mut room, n);
            // SAFETY: `n` dwords at `taken`, in guest memory or in `room` (Source::take).
            self.registers.write_run(first, n, |i| unsafe { taken.add(i).read() });
            first += n;
            count -= n;
        }
    }

    // An IM_LOAD or IM_LOAD_IMMEDIATE: `kind` 0 is the vertex shader, 1 the pixel. The
    // code stays where it is until the next load of that kind.
    fn load_shader(&mut self, kind: u32, shader: Shader, hash: u64) {
        let kind = (kind & 1) as usize;
        let code = self.code_of(shader, kind);
        if code.1 == 0 {
            return;
        }
        self.shader_hash[kind] = hash;
        self.shader[kind] = shader;
        #[cfg(feature = "diagnostics")]
        if self.shaders_seen.get_or_insert_with(Default::default).insert(hash) {
            STATS.shaders_seen.bump();
            if let Some(dir) = shader_dump_directory() {
                use std::io::Write;
                let path = crate::log::format_text!(
                    "%s/%s_%016llx.ucode",
                    dir,
                    if kind != 0 { c"pixel" } else { c"vertex" },
                    hash
                );
                if let Ok(mut file) = std::fs::File::create(crate::runtime::platform::system_path(&path)) {
                    // SAFETY: the stage's code, `code.1` dwords at `code.0` (code_of).
                    let words = unsafe { std::slice::from_raw_parts(code.0, code.1) };
                    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
                    let _ = file.write_all(&bytes);
                }
            }
        }
    }

    // The code a stage's shader names, where it is now: null and 0 for none.
    #[inline(always)]
    fn code_of(&self, shader: Shader, kind: usize) -> (*const u32, usize) {
        match shader {
            Shader::None => (ptr::null(), 0),
            Shader::Immediate => {
                let code = &self.immediate[kind & 1];
                (if code.is_empty() { ptr::null() } else { code.as_ptr() }, code.len())
            }
            // SAFETY: a Loaded the memo holds boxed; entries are never removed.
            Shader::Load(loaded) => {
                let code = unsafe { &(*loaded).code };
                (if code.is_empty() { ptr::null() } else { code.as_ptr() }, code.len())
            }
        }
    }

    // MW2_TRACE_DRAWS=<n> describes n draw packets in full, after
    // MW2_TRACE_DRAWS_AFTER=<presents>: the opening burst is not representative.
    #[cfg(feature = "diagnostics")]
    fn trace_draw(&self, draw: DrawInitiator, indexed: bool, dma_base: u32, dma_size: u32) {
        static LIMIT: LazyLock<u64> = LazyLock::new(|| crate::env::number(c"MW2_TRACE_DRAWS", 0));
        static AFTER: LazyLock<u64> = LazyLock::new(|| crate::env::number(c"MW2_TRACE_DRAWS_AFTER", 0));
        static TRACED: AtomicU64 = AtomicU64::new(0);
        let traced = TRACED.load(Relaxed);
        if traced >= *LIMIT || frame_count() < *AFTER {
            return;
        }
        TRACED.store(traced + 1, Relaxed);
        let name = r::primitive_name(draw.primitive_type());
        let registers = &self.registers;
        log_kernel!(
            "draw: %-22s %5u indices, %s, surface %08X colour %08X depth %08X, program %08X, mode %08X",
            name.unwrap_or(c"unknown primitive"),
            draw.index_count(),
            if indexed { c"indexed" } else { c"auto" },
            registers.get(r::RB_SURFACE_INFO),
            registers.get(r::RB_COLOR_INFO),
            registers.get(r::RB_DEPTH_INFO),
            registers.get(r::SQ_PROGRAM_CNTL),
            registers.get(r::RB_MODECONTROL)
        );
        if indexed {
            let count = dma_size & 0x3F_FFFF; // indices, not bytes
            let bits: u32 = if draw.index32() { 32 } else { 16 };
            log_kernel!(
                "      indices from %08X, %u of them, %u-bit (%u bytes)",
                dma_base,
                count,
                bits,
                count.wrapping_mul(if draw.index32() { 4 } else { 2 })
            );
        }
    }

    fn execute_draw(&mut self, s: &mut Source, count: u32) {
        if count < 1 {
            return;
        }
        IDLE_SINCE_DRAW.store(false, Relaxed);
        let draw = DrawInitiator(s.next());
        self.write_register(r::VGT_DRAW_INITIATOR, draw.0);

        // An auto-indexed draw has the shader compute its own indices.
        let (mut dma_base, mut dma_size) = (0u32, 0u32);
        let indexed = draw.source_select() == 0;
        if indexed && count >= 3 {
            dma_base = s.next();
            dma_size = s.next();
        }
        #[cfg(not(feature = "diagnostics"))]
        let _ = dma_size;

        let primitive = draw.primitive_type();
        let mode = EdramMode(self.registers.get(r::RB_MODECONTROL) & 0x7);
        #[cfg(feature = "diagnostics")]
        super::census::draw(&self.registers, primitive, draw.index_count(), indexed, mode);
        if image_move::copies_waiting() {
            image_move::at_draw(self.registers.get(CONSTANT_BASE_ALU), self.shader_hash[0]);
        }
        #[cfg(feature = "diagnostics")]
        if super::census::dumping_textures() {
            super::census::dump_bound_textures(&self.registers);
        }

        if rendering() {
            if mode == EdramMode::COPY {
                renderer::resolve(&self.registers);
            } else {
                let (vertex_code, vertex_words) = self.code_of(self.shader[0], 0);
                let (pixel_code, pixel_words) = self.code_of(self.shader[1], 1);
                let call = renderer::DrawCall {
                    primitive,
                    index_count: draw.index_count(),
                    indexed,
                    index32: draw.index32(),
                    // Physical, as every address the GPU is handed is.
                    index_address: if indexed { kernel::from_physical(dma_base) } else { 0 },
                    vertex_code,
                    vertex_words,
                    vertex_hash: self.shader_hash[0],
                    pixel_code,
                    pixel_words,
                    pixel_hash: self.shader_hash[1],
                };
                renderer::draw(&self.registers, &call);
            }
        }
        #[cfg(feature = "diagnostics")]
        self.trace_draw(draw, indexed, dma_base, dma_size);
    }

    fn execute_indirect(&mut self, physical_address: u32, size_dwords: u32, depth: i32) {
        if depth > 4 || size_dwords == 0 {
            return;
        }
        // Every buffer the title issues is 32-byte aligned and a few thousand dwords
        // long. A pair of dwords that only reads like an address and a length almost
        // never satisfies both, and running one sends the parse into unrelated memory.
        if physical_address & 0x1F != 0 || size_dwords > (16 << 10) {
            STATS.implausible_buffers.bump();
            return;
        }
        let va = kernel::from_physical(physical_address);
        if va == 0 {
            return;
        }
        #[cfg(feature = "diagnostics")]
        stream::note_read(va, u64::from(size_dwords) * 4);

        let mut ib = Source::new(va, 0, 0, size_dwords);
        self.execute(&mut ib, depth + 1);
        STATS.buffers.bump();
        if ib.lost_step {
            STATS.buffers_lost_step.bump();
        }
    }

    // WAIT_REG_MEM: the command processor stalls until a register or a dword of memory
    // matches. D3D's flip handshake is built on it -- wait for the scratch write-back of
    // the handler it names, raise the interrupt, wait for the handler to clear the
    // request. Waited for as Xenia waits, but not for ever.
    fn wait_reg_mem(&mut self, info: u32, address: u32, reference: u32, mask: u32) {
        const COHER_STATUS_HOST: u32 = 0x0A31;
        let memory = info & 0x10 != 0;
        let mut word: *mut u32 = ptr::null_mut();
        if memory {
            let va = kernel::from_physical(address & !3);
            if va == 0 {
                return;
            }
            word = base().wrapping_add(va as usize).cast::<u32>();
        } else if !RegisterFile::in_range(address) {
            return;
        }
        let current = |cp: &mut CommandProcessor| -> u32 {
            if !memory {
                // A coherency request is complete as soon as it is made: nothing caches
                // guest memory ahead of the GPU.
                if address == COHER_STATUS_HOST {
                    let held = cp.registers.get(address);
                    cp.registers.write(address, held & !0x8000_0000);
                }
                return cp.registers.get(address);
            }
            // The guest's bytes as a host dword, then the swap the address' low two bits
            // ask for, as Xenia's GpuSwap does. Read anew at every poll: the title writes it.
            // SAFETY: the dword the physical mapping gave.
            let raw = unsafe { ptr::read_volatile(word) };
            match address & 3 {
                1 => ((raw & 0x00FF_00FF) << 8) | ((raw >> 8) & 0x00FF_00FF),
                2 => raw.swap_bytes(),
                3 => raw.rotate_left(16),
                _ => raw,
            }
        };
        let matches = |value: u32| {
            let value = value & mask;
            match info & 7 {
                1 => value < reference,
                2 => value <= reference,
                3 => value == reference,
                4 => value != reference,
                5 => value >= reference,
                6 => value > reference,
                7 => true,
                _ => false,
            }
        };
        #[cfg(feature = "diagnostics")]
        let mut polls: u32 = 1;
        #[cfg(feature = "diagnostics")]
        {
            // MW2_REPLAY_GPU: polled as often as the capture says it was, a timeout taken
            // as one. A wait the first poll met was met by what memory held, which the
            // capture looked at just before (NotePolled): the replay holds it, and an `own`
            // page, the consumer's own write, is checked. One met later was met by the
            // title's write during the wait, which the replay makes before the last poll.
            if stream::replaying() {
                if memory {
                    stream::note_polled(kernel::from_physical(address & !3));
                }
                match stream::replay_wait() {
                    None => stream::diverged(c"a WAIT_REG_MEM the capture does not have"),
                    Some(wait) => {
                        STATS.reg_mem_waits.bump();
                        for poll in 1..=wait.polls {
                            if poll > 1 {
                                deliver_occlusion_queries();
                                renderer::idle();
                                super::gpu::sleep_for(Duration::from_micros(50));
                            }
                            let last = poll == wait.polls;
                            if last && poll > 1 && !wait.timed_out && memory {
                                // SAFETY: the dword polled.
                                unsafe { ptr::write_volatile(word, wait.value) };
                            }
                            let matched = matches(current(self));
                            if poll == 1 && !matched {
                                STATS.reg_mem_stalls.bump();
                            }
                            if matched != (last && !wait.timed_out) {
                                stream::diverged(if matched {
                                    c"a WAIT_REG_MEM satisfied sooner than captured"
                                } else {
                                    c"a WAIT_REG_MEM not satisfied where captured"
                                });
                            }
                        }
                        if wait.timed_out {
                            STATS.reg_mem_timeouts.bump();
                        }
                        return;
                    }
                }
            }
            if memory {
                stream::note_polled(kernel::from_physical(address & !3));
            }
        }
        #[cfg(feature = "diagnostics")]
        let captured_wait = |cp: &mut CommandProcessor, polls: u32, timed_out: bool| {
            if stream::capturing() {
                // SAFETY: the dword polled.
                let value = if memory { unsafe { ptr::read_volatile(word) } } else { current(cp) };
                stream::wait_done(memory, address, value, polls, timed_out);
            }
        };
        STATS.reg_mem_waits.bump();
        if matches(current(self)) {
            #[cfg(feature = "diagnostics")]
            captured_wait(self, polls, false);
            return;
        }
        STATS.reg_mem_stalls.bump();

        let began = Instant::now();
        let mut matched = false;
        while host::running() {
            deliver_occlusion_queries();
            renderer::idle();
            super::gpu::sleep_for(Duration::from_micros(50));
            #[cfg(feature = "diagnostics")]
            {
                polls += 1;
            }
            if matches(current(self)) {
                matched = true;
                break;
            }
            if began.elapsed() > Duration::from_millis(500) {
                break;
            }
        }
        let micros = began.elapsed().as_micros() as u64;
        STATS.reg_mem_wait_micros.add(micros);
        #[cfg(feature = "diagnostics")]
        captured_wait(self, polls, !matched);
        if matched {
            return;
        }
        STATS.reg_mem_timeouts.bump();
        static TOLD: AtomicU32 = AtomicU32::new(0);
        let told = TOLD.load(Relaxed);
        TOLD.store(told.wrapping_add(1), Relaxed);
        if told < 8 {
            let holds = current(self);
            log_warn!(
                "gpu: gave up after 500 ms waiting for %s %08X & %08X to compare (function %u) with %08X; it holds %08X",
                if memory { c"memory" } else { c"register" },
                address,
                mask,
                info & 7,
                reference,
                holds
            );
        }
    }

    // A type-3 packet. `count` is its payload in dwords.
    fn execute_type3(&mut self, s: &mut Source, header: u32, count: u32, depth: i32) {
        let opcode = (header >> 8) & 0x7F;
        STATS.opcode_count[opcode as usize].bump();

        if !KNOWN_OPCODES[opcode as usize] {
            let unknown = UNKNOWN_OPCODES.load(Relaxed);
            UNKNOWN_OPCODES.store(unknown.wrapping_add(1), Relaxed);
            if unknown < 32 {
                log_warn!(
                    "pm4: unknown type-3 opcode 0x%02X, %u dwords (header %08X) at %08X (depth %d)",
                    opcode,
                    count,
                    header,
                    s.address_of(s.index.wrapping_sub(1)),
                    depth
                );
            }
            s.skip(count);
            return;
        }

        // A predicated packet (bit 0) runs only when the bin select and the bin mask
        // share a bit, and a predicated swap never does -- as Xenia has it.
        if header & 1 != 0 {
            if self.bin_select & self.bin_mask == 0 || opcode == PM4_XE_SWAP {
                STATS.predicated_skipped.bump();
                s.skip(count);
                return;
            }
            STATS.predicated_run.bump();
        }

        // What a handler consumed is measured rather than declared: one that read past
        // its own packet would shift everything after it.
        let before = s.remaining;
        match opcode {
            PM4_INDIRECT_BUFFER | PM4_INDIRECT_BUFFER_PFD => {
                if count >= 2 {
                    let address = s.next();
                    let size = s.next() & 0xF_FFFF;
                    self.execute_indirect(address, size, depth);
                }
            }

            PM4_MEM_WRITE => {
                // { address, value... } -- the address' low bits are flags.
                let address = s.next();
                for i in 1..count {
                    let value = s.next();
                    write_physical(address.wrapping_add((i - 1).wrapping_mul(4)), value);
                }
            }

            PM4_EVENT_WRITE_SHD => {
                // { initiator, address, value }. Bit 31 of the initiator asks for the
                // GPU's clock instead of the value, which is the timebase's, as Xenia
                // writes it; MW2 has not been seen to ask.
                if count >= 3 {
                    let initiator = s.next();
                    let address = s.next();
                    let value = s.next();
                    // An end-of-pipe event: its write lands once the draws before it have
                    // run -- which is how D3D's fence, the arena's progress report, tells
                    // the title they are done.
                    renderer::before_completion();
                    if !arena::arena_progress(address, value) {
                        write_physical(address, if initiator >> 31 != 0 { host::timebase() as u32 } else { value });
                    }
                }
            }

            PM4_EVENT_WRITE_CFL => {
                // { initiator, address, value }, end of pipe as EVENT_WRITE_SHD.
                if count >= 3 {
                    renderer::before_completion();
                    s.next();
                    let address = s.next();
                    let value = s.next();
                    write_physical(address, value);
                }
            }

            PM4_EVENT_WRITE_ZPD => {
                // Unlike the other event writes this one carries only the event
                // initiator: the record's address is a register.
                if count >= 1 {
                    s.next();
                    sample_count_event(self.registers.get(r::RB_SAMPLE_COUNT_ADDR));
                }
            }

            PM4_XE_SWAP => {
                // The swap packet VdSwap writes into the present segment: the end of a
                // frame as the command processor meets it. Its dwords: a signature, the
                // front buffer, its width and height.
                if count >= 2 {
                    s.next();
                    let front_buffer = s.next();
                    if rendering() {
                        renderer::swap(front_buffer);
                    }
                }
                let swaps = SWAPS_PARSED.load(Relaxed) + 1;
                SWAPS_PARSED.store(swaps, Relaxed);
                #[cfg(feature = "diagnostics")]
                {
                    note_swap();
                    self.note_frame_cost();
                    super::census::swap();
                }
                if image_move::copies_waiting() {
                    image_move::at_frame_end(swaps);
                }
            }

            PM4_SET_BIN_MASK_LO | PM4_SET_BIN_MASK_HI | PM4_SET_BIN_SELECT_LO | PM4_SET_BIN_SELECT_HI => {
                // Half of the 64-bit bin mask or select that predicated packets are
                // tested against.
                if count >= 1 {
                    let value = u64::from(s.next());
                    let target = if opcode == PM4_SET_BIN_MASK_LO || opcode == PM4_SET_BIN_MASK_HI {
                        &mut self.bin_mask
                    } else {
                        &mut self.bin_select
                    };
                    if opcode == PM4_SET_BIN_MASK_LO || opcode == PM4_SET_BIN_SELECT_LO {
                        *target = (*target & 0xFFFF_FFFF_0000_0000) | value;
                    } else {
                        *target = (*target & 0xFFFF_FFFF) | (value << 32);
                    }
                }
            }

            PM4_SET_BIN_MASK | PM4_SET_BIN_SELECT => {
                // { high, low }
                if count >= 2 {
                    let high = u64::from(s.next());
                    let low = u64::from(s.next());
                    *(if opcode == PM4_SET_BIN_MASK { &mut self.bin_mask } else { &mut self.bin_select }) =
                        (high << 32) | low;
                }
            }

            PM4_IM_LOAD_IMMEDIATE => {
                // { type, start<<16 | size, microcode... }
                if count >= 2 {
                    let kind = s.next();
                    let start_size = s.next();
                    let mut code = Vec::with_capacity((count - 2) as usize);
                    for _ in 2..count {
                        code.push(s.next());
                    }
                    // The packet states its length twice, and a real one agrees with itself.
                    if start_size & 0xFFFF != count - 2 {
                        STATS.shader_loads_refused.bump();
                    } else {
                        #[cfg(feature = "diagnostics")]
                        trace_shader_load(&mut self.shader_loads_told, kind & 1, &code, 0);
                        let hash = shader_hash(&code);
                        self.immediate[(kind & 1) as usize] = code;
                        self.load_shader(kind, Shader::Immediate, hash);
                    }
                }
            }

            PM4_IM_LOAD => {
                // { address|type, start<<16 | size }: the microcode is in memory; the
                // address' low two bits select vertex or pixel.
                if count >= 2 {
                    let address_type = s.next();
                    let start_size = s.next();
                    self.im_load(address_type, start_size);
                }
            }

            PM4_WAIT_FOR_IDLE => {
                // The command processor stops until the GPU has finished everything
                // before. Nothing here runs ahead of the parse to wait for.
                IDLE_SINCE_DRAW.store(true, Relaxed);
            }

            PM4_INTERRUPT => {
                // { cpu mask }: a command-processor interrupt, once per processor named.
                if count >= 1 {
                    if IDLE_SINCE_DRAW.load(Relaxed) {
                        renderer::before_completion();
                    }
                    let mask = s.next();
                    host::raise_interrupt(mask);
                }
            }

            PM4_SET_CONSTANT => {
                // { offset and type, values... }. The type names a constant window; the
                // offset is an index within it.
                if count >= 1 {
                    let offset_type = s.next();
                    let index = offset_type & 0x7FF;
                    let kind = (offset_type >> 16) & 0xFF;
                    if let Some(&window) = CONSTANT_WINDOWS.get(kind as usize) {
                        self.write_registers(window + index, count - 1, s);
                    }
                }
            }

            PM4_LOAD_ALU_CONSTANT => {
                // { physical address, offset and type, size in dwords }: the same five
                // windows SET_CONSTANT names, filled from memory rather than from the
                // stream, as big-endian dwords.
                if count >= 3 {
                    let address = s.next() & 0x3FFF_FFFF;
                    let offset_type = s.next();
                    let size_dwords = s.next() & 0xFFF;
                    self.load_alu_constant(address, offset_type, size_dwords);
                }
            }

            PM4_SET_CONSTANT2 => {
                // The offset is already an absolute register.
                if count >= 1 {
                    let index = s.next() & 0xFFFF;
                    self.write_registers(index, count - 1, s);
                }
            }

            PM4_DRAW_INDX => {
                // { viz query token, draw initiator, [dma base, dma size] }
                if count >= 2 {
                    s.next();
                    self.execute_draw(s, count - 1);
                }
            }

            // The same without the viz query token: where MW2's draws come from.
            PM4_DRAW_INDX_2 => self.execute_draw(s, count),

            PM4_REG_TO_MEM => {
                // { register, address }: copy one register to memory.
                if count >= 2 {
                    let reg = s.next() & 0xFFFF;
                    let address = s.next();
                    write_physical(address, self.registers.get(reg));
                }
            }

            PM4_WAIT_REG_MEM => {
                // { info, address, reference, mask, poll interval }
                if count >= 5 {
                    let info = s.next();
                    let address = s.next();
                    let reference = s.next();
                    let mask = s.next();
                    s.next();
                    self.wait_reg_mem(info, address, reference, mask);
                }
            }

            // { mode, poll address, reference, mask, write address, value }. Nothing here
            // can fail a poll, so the write is taken.
            PM4_COND_WRITE if count >= 6 => {
                let mode = s.next();
                s.skip(3);
                let write_address = s.next();
                let value = s.next();
                if mode & 0x100 == 0 {
                    // memory, not register
                    write_physical(write_address, value);
                }
            }

            _ => {}
        }
        let used = before - s.remaining;
        if used < count {
            s.skip(count - used);
        }
    }

    #[inline(never)]
    fn im_load(&mut self, address_type: u32, start_size: u32) {
        let va = kernel::from_physical(address_type & !3);
        let size_dwords = start_size & 0xFFFF;
        if va == 0 || size_dwords == 0 || size_dwords > (64 << 10) {
            return;
        }
        let physical = address_type & !3;
        #[cfg(feature = "diagnostics")]
        stream::note_read(va, u64::from(size_dwords) * 4);
        let key = u64::from(physical) << 17 | u64::from(size_dwords) << 1 | u64::from(address_type & 1);
        let loaded: &mut Loaded = self.loads.get_or_insert_with(Default::default).entry(key).or_default();
        let watched = va == PHYSICAL_BASE.wrapping_add(physical) && watch::enabled();
        if !(loaded.stamp != 0 && watch::unwritten(physical, size_dwords * 4, loaded.stamp)) {
            // Watched from before it is read, so a write while it is read counts.
            loaded.stamp =
                if watched { watch::track(physical, size_dwords * 4, SWAPS_PARSED.load(Relaxed)) } else { 0 };
            if loaded.code.len() != size_dwords as usize {
                loaded.code = vec![0; size_dwords as usize].into_boxed_slice();
            }
            // The guest's base read once: an atomic load each dword (be_u32's) is not
            // hoisted out of the loop, which then neither unrolls nor vectorises.
            let words = base().wrapping_add(va as usize).cast::<u32>().cast_const();
            for (i, slot) in loaded.code.iter_mut().enumerate() {
                // SAFETY: the shader's dwords in guest memory, `size_dwords` of them, at
                // a dword-aligned address (the physical one's low two bits cleared).
                *slot = u32::from_be(unsafe { words.wrapping_add(i).read() });
            }
            loaded.hash = shader_hash(&loaded.code);
            #[cfg(feature = "diagnostics")]
            trace_shader_load(&mut self.shader_loads_told, address_type & 3, &loaded.code, va);
        }
        let (at, hash): (*const Loaded, u64) = (&*loaded, loaded.hash);
        self.load_shader(address_type & 3, Shader::Load(at), hash);
    }

    #[inline(never)]
    fn load_alu_constant(&mut self, address: u32, offset_type: u32, size_dwords: u32) {
        let index = offset_type & 0x7FF;
        let kind = (offset_type >> 16) & 0xFF;
        let va = kernel::from_physical(address);
        if kind >= 5 || va == 0 || size_dwords == 0 {
            return;
        }
        #[cfg(feature = "diagnostics")]
        stream::note_read(va, u64::from(size_dwords) * 4);
        let first = CONSTANT_WINDOWS[kind as usize] + index;
        if first + size_dwords > REGISTER_COUNT {
            for i in 0..size_dwords {
                // SAFETY: the constants' dwords in guest memory. Nothing aligns the
                // address, and the C++ reads it as it is: an unaligned load.
                let word =
                    unsafe { base().wrapping_add(va.wrapping_add(i * 4) as usize).cast::<u32>().read_unaligned() };
                self.write_register(first + i, u32::from_be(word));
            }
            return;
        }
        let words = base().wrapping_add(va as usize).cast::<u32>().cast_const();
        // SAFETY: `size_dwords` dwords of guest memory at `va`, which may be unaligned.
        self.registers.write_run(first, size_dwords, |i| unsafe { words.wrapping_add(i).read_unaligned() });
    }

    fn execute(&mut self, s: &mut Source, depth: i32) {
        while s.remaining != 0 {
            self.batch_work += 1;
            if self.batch_work > BATCH_WORK_LIMIT {
                if depth == 0 {
                    STATS.batches_abandoned.bump();
                }
                return;
            }
            let header = s.next();
            // A zero dword is filler, not a type-0 packet: reading it as one swallows the
            // dword after it.
            if header == 0 {
                STATS.padding.bump();
                continue;
            }

            let kind = header >> 30;
            let payload = match kind {
                1 => 2,
                2 => 0,
                _ => ((header >> 16) & 0x3FFF) + 1,
            };
            if payload > s.remaining {
                s.lost_step = true;
                static SHOWN: AtomicU32 = AtomicU32::new(0);
                let shown = SHOWN.load(Relaxed);
                SHOWN.store(shown.wrapping_add(1), Relaxed);
                if shown < 8 {
                    log_warn!(
                        "pm4: a packet at %08X (header %08X) runs past the end of its buffer; the rest of the buffer is not read",
                        s.address_of(s.index.wrapping_sub(1)),
                        header
                    );
                }
                return;
            }
            STATS.type_count[kind as usize].bump();
            STATS.packets.bump();
            #[cfg(feature = "diagnostics")]
            stream::packet();

            match kind {
                0 => {
                    // A run of registers from a base index, or the same register `count`
                    // times when ONE_REG_WRITE is set -- how a FIFO register is fed.
                    let base = header & 0x7FFF;
                    if header & 0x8000 == 0 {
                        self.write_registers(base, payload, s);
                    } else {
                        for _ in 0..payload {
                            let value = s.next();
                            self.write_register(base, value);
                        }
                    }
                }
                1 => {
                    // Two register indices packed into the header.
                    let first = header & 0x7FF;
                    let second = (header >> 11) & 0x7FF;
                    let value = s.next();
                    self.write_register(first, value);
                    let value = s.next();
                    self.write_register(second, value);
                }
                2 => {}
                _ => self.execute_type3(s, header, payload, depth),
            }
        }
    }
}

#[cfg(feature = "diagnostics")]
fn note_swap() {
    use crate::runtime::{clock, pacing_trace, platform};
    if pacing_trace::on() {
        pacing_trace::note(clock::nanoseconds(), b'S', 0, platform::thread_id());
    }
}

#[cfg(feature = "diagnostics")]
fn frame_count() -> u64 {
    super::gpu::frame_count()
}

/// Executes `dwords` of the ring starting at `read_index`. The ring consumer's.
pub fn execute_ring(base: u32, read_index: u32, mask: u32, dwords: u32) {
    // SAFETY: the ring consumer, and nothing it calls makes another reference.
    let cp = unsafe { consumer() };
    cp.batch_work = 0;
    let mut s = Source::new(base, read_index, mask, dwords);
    cp.execute(&mut s, 0);
}

/// The display's 256-entry colour table, as the scanout hardware holds it: ten bits per
/// channel, blue in the low bits. None until the title has filled it. Any thread.
pub fn display_colour_table() -> Option<[u32; 256]> {
    // Only a ramp the title finished writing. Before the first one is complete the
    // frame is presented untouched.
    if !DISPLAY_LUT_READY.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    Some(*lut())
}

/// The swap packets met so far.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub fn swaps_parsed() -> u64 {
    SWAPS_PARSED.load(Relaxed)
}

// ---- MW2_CAPTURE_GPU and MW2_REPLAY_GPU ---------------------------------------------------

/// The command processor's part of a capture's state.bin. Fixed layout: a replay reads
/// it back field for field (command_processor.cpp's SavedState).
#[cfg(feature = "diagnostics")]
#[repr(C)]
#[derive(Clone, Copy)]
struct SavedState {
    magic: u32,
    version: u32,
    values: [u32; REGISTER_COUNT as usize],
    lut_ready: u32,
    stamps: [u64; r::STAMP_BLOCKS],
    writes: u64,
    bin_mask: u64,
    bin_select: u64,
    // Each stage's shader: its hash, the IM_LOAD it came from (0: immediate), and its
    // length; the words follow the structure, vertex then pixel.
    shader_hash: [u64; 2],
    shader_load: [u64; 2],
    shader_words: [u32; 2],
    shader_immediate: [u32; 2],
    display_lut: [u32; 256],
    display_lut_pending: [u32; 256],
    swaps_parsed: u64,
    idle_since_draw: u32,
    arena_block: u32,
    arena_sequence: u32,
    arena_pending: u32,
    arena_have_position: u32,
    reserved: u32,
}

#[cfg(feature = "diagnostics")]
const SAVED_MAGIC: u32 = 0x4332_574D; // "MW2C"

// The C++'s, byte for byte: a capture taken by one build replays in the other.
#[cfg(feature = "diagnostics")]
const _: () = {
    assert!(std::mem::offset_of!(SavedState, values) == 8);
    assert!(std::mem::offset_of!(SavedState, lut_ready) == 81940);
    assert!(std::mem::offset_of!(SavedState, stamps) == 81944);
    assert!(std::mem::offset_of!(SavedState, writes) == 84512);
    assert!(std::mem::offset_of!(SavedState, shader_hash) == 84536);
    assert!(std::mem::offset_of!(SavedState, shader_words) == 84568);
    assert!(std::mem::offset_of!(SavedState, display_lut) == 84584);
    assert!(std::mem::offset_of!(SavedState, swaps_parsed) == 86632);
    assert!(std::mem::size_of::<SavedState>() == 86664);
};

/// state.bin's command-processor part (SaveCommandProcessor). Between batches, on the
/// consumer.
#[cfg(feature = "diagnostics")]
pub fn save() -> Vec<u8> {
    // SAFETY: on the consumer between batches (the capture's start, at BatchBegin).
    let cp = unsafe { consumer() };
    // SAFETY: plain data; every field is set below or zero.
    let mut saved: SavedState = unsafe { std::mem::zeroed() };
    saved.magic = SAVED_MAGIC;
    saved.version = 1;
    saved.values = cp.registers.values;
    saved.stamps = cp.registers.stamps;
    saved.writes = cp.registers.writes;
    saved.bin_mask = cp.bin_mask;
    saved.bin_select = cp.bin_select;
    let mut code = Vec::new();
    for t in 0..2 {
        saved.shader_hash[t] = cp.shader_hash[t];
        let (words, size) = cp.code_of(cp.shader[t], t);
        saved.shader_words[t] = if cp.shader[t] == Shader::None { 0 } else { size as u32 };
        saved.shader_immediate[t] = u32::from(cp.shader[t] == Shader::Immediate);
        if let (Shader::Load(at), Some(loads)) = (cp.shader[t], &cp.loads) {
            for (key, loaded) in loads {
                if ptr::eq(&**loaded, at) {
                    saved.shader_load[t] = *key;
                }
            }
        }
        if cp.shader[t] != Shader::None && size != 0 {
            // SAFETY: the stage's code (code_of).
            code.extend_from_slice(unsafe { std::slice::from_raw_parts(words, size) });
        }
    }
    saved.display_lut = *lut();
    saved.display_lut_pending = cp.display_lut_pending;
    saved.lut_ready = u32::from(DISPLAY_LUT_READY.load(std::sync::atomic::Ordering::Acquire));
    saved.swaps_parsed = SWAPS_PARSED.load(Relaxed);
    saved.idle_since_draw = u32::from(IDLE_SINCE_DRAW.load(Relaxed));
    let arena = arena::save();
    saved.arena_block = arena.block;
    saved.arena_sequence = arena.sequence;
    saved.arena_pending = arena.pending;
    saved.arena_have_position = arena.have_position;
    let mut out = Vec::with_capacity(size_of::<SavedState>() + code.len() * 4);
    // SAFETY: SavedState is plain data, read as its bytes.
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts((&raw const saved).cast::<u8>(), size_of::<SavedState>())
    });
    for word in code {
        out.extend_from_slice(&word.to_ne_bytes());
    }
    out
}

/// The replay's step 4 (src/runtime/gpu_replay.rs): the command processor and the
/// arena's progress as the capture found them. False when `bytes` is not a state.bin
/// this build wrote. Before the consumer runs a batch.
#[cfg(feature = "diagnostics")]
pub fn restore_for_replay(bytes: &[u8]) -> bool {
    if bytes.len() < size_of::<SavedState>() {
        return false;
    }
    // SAFETY: at least a SavedState's bytes, read unaligned into plain data.
    let saved: SavedState = unsafe { bytes.as_ptr().cast::<SavedState>().read_unaligned() };
    if saved.magic != SAVED_MAGIC || saved.version != 1 {
        return false;
    }
    let words_total = u64::from(saved.shader_words[0]) + u64::from(saved.shader_words[1]);
    if bytes.len() as u64 != size_of::<SavedState>() as u64 + words_total * 4 {
        return false;
    }
    // SAFETY: before the consumer runs a batch (the replay's step 4).
    let cp = unsafe { consumer() };
    cp.registers.values = saved.values;
    cp.registers.stamps = saved.stamps;
    cp.registers.writes = saved.writes;
    cp.bin_mask = saved.bin_mask;
    cp.bin_select = saved.bin_select;
    let mut at = size_of::<SavedState>();
    for t in 0..2 {
        cp.shader[t] = Shader::None;
        cp.shader_hash[t] = saved.shader_hash[t];
        let size = saved.shader_words[t] as usize;
        if size == 0 {
            continue;
        }
        let code: Vec<u32> = bytes[at..at + size * 4]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        at += size * 4;
        // An IM_LOAD's microcode goes back where that load keeps it, unstamped, so the
        // next load of it reads memory as one the watch knows nothing of would.
        if saved.shader_immediate[t] != 0 {
            cp.immediate[t] = code;
            cp.shader[t] = Shader::Immediate;
        } else {
            let loads = cp.loads.get_or_insert_with(Default::default);
            let loaded = loads.entry(saved.shader_load[t]).or_default();
            loaded.code = code.into_boxed_slice();
            loaded.hash = saved.shader_hash[t];
            cp.shader[t] = Shader::Load(&**loaded);
        }
    }
    *lut() = saved.display_lut;
    cp.display_lut_pending = saved.display_lut_pending;
    DISPLAY_LUT_READY.store(saved.lut_ready != 0, std::sync::atomic::Ordering::Release);
    SWAPS_PARSED.store(saved.swaps_parsed, Relaxed);
    IDLE_SINCE_DRAW.store(saved.idle_since_draw != 0, Relaxed);
    arena::restore(&arena::ArenaState {
        block: saved.arena_block,
        sequence: saved.arena_sequence,
        pending: saved.arena_pending,
        have_position: saved.arena_have_position,
    });
    true
}

// ---- the report ---------------------------------------------------------------------

#[cfg(feature = "diagnostics")]
pub fn report() {
    let s = &STATS;
    log_info!(
        "gpu: %llu packets; %llu of %llu indirect buffers lost step, %llu refused as implausible, %llu ring batches abandoned; %llu padding dwords, %llu unknown opcodes",
        s.packets.get(),
        s.buffers_lost_step.get(),
        s.buffers.get(),
        s.implausible_buffers.get(),
        s.batches_abandoned.get(),
        s.padding.get(),
        u64::from(UNKNOWN_OPCODES.load(Relaxed))
    );
    log_info!(
        "gpu: %llu distinct shaders loaded, %llu loads refused as inconsistent",
        s.shaders_seen.get(),
        s.shader_loads_refused.get()
    );
    log_info!("gpu: predicated packets: %llu run, %llu skipped", s.predicated_run.get(), s.predicated_skipped.get());
    if s.register_writes_out_of_range.get() != 0 {
        log_info!("gpu: %llu register writes out of range", s.register_writes_out_of_range.get());
    }
    if s.reg_mem_waits.get() != 0 {
        log_info!(
            "gpu: %llu WAIT_REG_MEM packets, %llu found unsatisfied (%.1f ms waited in all), %llu given up on after 500 ms",
            s.reg_mem_waits.get(),
            s.reg_mem_stalls.get(),
            s.reg_mem_wait_micros.get() as f64 / 1000.0,
            s.reg_mem_timeouts.get()
        );
    }
    log_info!("gpu: %llu swap packets", SWAPS_PARSED.load(Relaxed));
    let bins = |n: &[Stat; FRAME_BINS]| {
        crate::log::format_text!(
            "<8 ms %llu, 8-12 %llu, 12-16.7 %llu, 16.7-20 %llu, 20-33 %llu, 33-50 %llu, 50+ %llu",
            n[0].get(),
            n[1].get(),
            n[2].get(),
            n[3].get(),
            n[4].get(),
            n[5].get(),
            n[6].get()
        )
    };
    log_info!("gpu: frames by the consumer's CPU time, swap to swap: %s", bins(&s.frame_cpu).as_c_str());
    log_info!("gpu: frames by wall time, swap to swap: %s", bins(&s.frame_wall).as_c_str());
    for (t, count) in s.type_count.iter().enumerate() {
        if count.get() != 0 {
            log_info!("  type %u: %llu", t as u32, count.get());
        }
    }
    for (op, count) in s.opcode_count.iter().enumerate() {
        let n = count.get();
        if n != 0 {
            log_info!("  0x%02X %-22s %llu", op as u32, opcode_name(op as u32).unwrap_or(c"(unknown)"), n);
        }
    }
}
