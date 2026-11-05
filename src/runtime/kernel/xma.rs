// The console's XMA decoder: 320 hardware contexts, each a 64-byte block in physical
// memory the title fills in directly, driven by a register window at 0x7FEA0000. The
// title's XAudio2 (statically linked, so already recompiled) allocates contexts through
// XMACreateContext, points them at 2 KB XMA2 packets and a small PCM ring, and kicks them
// by writing a bit into the window; the decoder writes 16-bit big-endian PCM into the
// ring and advances the context's bit offset, and XAudio2 reads the ring on its next pass.
//
// A port of Xenia's current context decoder (xma_context_new): the same context layout,
// the same packet and frame walk, and FFmpeg's "xmaframes" codec, which takes one XMA frame
// at a time, ported too (src/runtime/wmapro/).
//
// XMA2 is WMA Pro in 2 KB packets. A packet starts with a 32-bit header (frame count, bit
// offset of the first frame that starts in it, a skip count to the next packet of the same
// stream) and holds 15-bit-length-prefixed frames of 512 samples, which may straddle
// packets. The context records a read offset in bits into its input buffer, and the
// title's XAudio2 loops sounds by having the decoder jump from loop_end back to loop_start.
//
// Decoding is done on the kicking thread, so by the time the store returns the context
// already reflects the work -- the closest thing to the hardware's latency that costs
// nothing to reason about.

use super::allocate_physical;
use super::video::to_physical;
use crate::env;
use crate::guest::Memory;
use crate::log::{log_error, log_info, log_kernel, log_warn};
use crate::runtime::guest_memory::{self, HEAP_END, PHYSICAL_BASE, mw2_kernel_physical_top};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

const WINDOW: u32 = 0x7FEA_0000;
const CONTEXT_COUNT: usize = 320;
const CONTEXT_SIZE: u32 = 64;

// Register indices (byte offset / 4) inside the window.
const REG_CONTEXT_ARRAY_ADDRESS: u32 = 0x600; // 0x1800: physical address of context 0
const REG_CURRENT_CONTEXT_INDEX: u32 = 0x606; // 0x1818: the context the hardware is on
const REG_NEXT_CONTEXT_INDEX: u32 = 0x607;
const REG_KICK0: u32 = 0x650; // ten words of bits, one per context
const REG_LOCK0: u32 = 0x690;
const REG_CLEAR0: u32 = 0x6A0;
const REG_LOCK_ALL: u32 = 0x601;

const BYTES_PER_PACKET: u32 = 2048;
const BYTES_PER_PACKET_HEADER: u32 = 4;
const BYTES_PER_PACKET_DATA: usize = (BYTES_PER_PACKET - BYTES_PER_PACKET_HEADER) as usize;
const BITS_PER_PACKET: u32 = BYTES_PER_PACKET * 8;
const BITS_PER_PACKET_HEADER: u32 = 32;
const BITS_PER_FRAME_HEADER: usize = 15;
const SAMPLES_PER_FRAME: usize = wmapro::SAMPLES;
const PADDING: usize = 64; // what the decoder may read past a frame, as FFmpeg's did
const BYTES_PER_FRAME_CHANNEL: u32 = SAMPLES_PER_FRAME as u32 * 2;
const OUTPUT_BYTES_PER_BLOCK: u32 = 256;
const OUTPUT_MAX_SIZE_BYTES: u32 = 31 * OUTPUT_BYTES_PER_BLOCK;
const MAX_FRAME_LENGTH: u64 = 0x7FFF;
const MAX_FRAME_SIZE_IN_BITS: u32 = 0x4000 - BITS_PER_PACKET_HEADER;
const ID_TO_SAMPLE_RATE: [i32; 4] = [24000, 32000, 44100, 48000];
const XMA_FRAME_SIZE: usize = 1 + 4097 + PADDING;

// MW2_TRACE_XMA=1 (diagnostics builds): every kick and pass.
static TRACE: LazyLock<bool> = LazyLock::new(|| cfg!(feature = "diagnostics") && env::flag(c"MW2_TRACE_XMA"));

macro_rules! xma_trace {
    ($format:literal $(, $argument:expr)* $(,)?) => {
        if *TRACE {
            log_kernel!($format $(, $argument)*);
        }
    };
}

// Errors are per frame and a broken stream repeats them; keep the first few.
static ERRORS: AtomicU32 = AtomicU32::new(0);

/// Counts an error; true while they are still logged.
pub fn count_error() -> bool {
    ERRORS.fetch_add(1, Ordering::Relaxed) < 20
}

macro_rules! xma_error {
    ($format:literal $(, $argument:expr)* $(,)?) => {
        if count_error() {
            log_warn!($format $(, $argument)*);
        }
    };
}

use crate::runtime::wmapro;

// tools/xma_diff.py's build (--cfg mw2_xma_rig) decodes every frame with FFmpeg as well,
// through runtime/apu/xma_codec.cpp, and logs where the two differ by a bit.
#[cfg(mw2_xma_rig)]
#[allow(unsafe_code)]
mod ffmpeg {
    use super::{PADDING, SAMPLES_PER_FRAME};
    use std::ffi::{CStr, c_char, c_int};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// While FFmpeg decodes beside the port: the rig keeps its log out of this side's. One
    /// flag for the process, as the rig replays on one thread.
    pub static SHADOW: AtomicBool = AtomicBool::new(false);

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn mw2_xma_codec_initialise(trace: bool);
        fn mw2_xma_codec_new() -> *mut Opaque;
        fn mw2_xma_codec_decode(
            codec: *mut Opaque,
            sample_rate: c_int,
            channels: c_int,
            data: *const u8,
            size: c_int,
            left: *mut f32,
            right: *mut f32,
            error: *mut c_char,
            error_size: usize,
        ) -> c_int;
    }

    pub fn initialise() {
        unsafe { mw2_xma_codec_initialise(false) }
    }

    pub struct Codec(NonNull<Opaque>);
    unsafe impl Send for Codec {}

    /// The channels, 0 for no frame, or the error's text.
    pub type Outcome = Result<u32, std::ffi::CString>;

    impl Codec {
        pub fn new() -> Self {
            Self(NonNull::new(unsafe { mw2_xma_codec_new() }).expect("FFmpeg's xmaframes"))
        }

        pub fn decode(
            &mut self,
            sample_rate: i32,
            channels: i32,
            data: &[u8],
            left: &mut [f32; SAMPLES_PER_FRAME],
            right: &mut [f32; SAMPLES_PER_FRAME],
        ) -> Outcome {
            let mut error = [0 as c_char; 64];
            SHADOW.store(true, Ordering::Relaxed);
            let got = unsafe {
                mw2_xma_codec_decode(
                    self.0.as_ptr(),
                    sample_rate,
                    channels,
                    data.as_ptr(),
                    (data.len() - PADDING) as c_int,
                    left.as_mut_ptr(),
                    right.as_mut_ptr(),
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            SHADOW.store(false, Ordering::Relaxed);
            if got >= 0 { Ok(got as u32) } else { Err(unsafe { CStr::from_ptr(error.as_ptr()) }.to_owned()) }
        }
    }
}

#[cfg(mw2_xma_rig)]
pub use ffmpeg::SHADOW;

// ---- the context block ------------------------------------------------------------------
// Sixteen big-endian words in guest memory. The fields are bitfields of the C++ build's
// struct on a little-endian host: the first named sits in the low bits.

#[derive(Clone, Copy)]
struct Field {
    word: usize,
    shift: u32,
    width: u32,
}

const fn field(word: usize, shift: u32, width: u32) -> Field {
    Field { word, shift, width }
}

// DWORD 0
const INPUT_BUFFER_0_PACKET_COUNT: Field = field(0, 0, 12);
const LOOP_COUNT: Field = field(0, 12, 8);
const INPUT_BUFFER_0_VALID: Field = field(0, 20, 1);
const INPUT_BUFFER_1_VALID: Field = field(0, 21, 1);
const OUTPUT_BUFFER_BLOCK_COUNT: Field = field(0, 22, 5);
const OUTPUT_BUFFER_WRITE_OFFSET: Field = field(0, 27, 5);
// DWORD 1
const INPUT_BUFFER_1_PACKET_COUNT: Field = field(1, 0, 12);
const LOOP_SUBFRAME_END: Field = field(1, 14, 3);
const LOOP_SUBFRAME_SKIP: Field = field(1, 17, 3);
const SUBFRAME_DECODE_COUNT: Field = field(1, 20, 4);
const OUTPUT_BUFFER_PADDING: Field = field(1, 24, 3);
const SAMPLE_RATE: Field = field(1, 27, 2);
const IS_STEREO: Field = field(1, 29, 1);
const OUTPUT_BUFFER_VALID: Field = field(1, 31, 1);
// DWORD 2
const INPUT_BUFFER_READ_OFFSET: Field = field(2, 0, 26);
const ERROR_STATUS: Field = field(2, 26, 5);
// DWORD 3
const LOOP_START: Field = field(3, 0, 26);
// DWORD 4
const LOOP_END: Field = field(4, 0, 26);
const CURRENT_BUFFER: Field = field(4, 31, 1);
// DWORD 5..8, physical addresses
const INPUT_BUFFER_0_PTR: Field = field(5, 0, 32);
const INPUT_BUFFER_1_PTR: Field = field(6, 0, 32);
const OUTPUT_BUFFER_PTR: Field = field(7, 0, 32);
// DWORD 9
const OUTPUT_BUFFER_READ_OFFSET: Field = field(9, 0, 5);

#[derive(Clone, Copy)]
struct ContextData([u32; 16]);

impl ContextData {
    fn load(memory: Memory, address: u32) -> Self {
        Self(std::array::from_fn(|i| memory.u32_at(address + 4 * i as u32)))
    }

    fn store(&self, memory: Memory, address: u32) {
        for (i, word) in self.0.iter().enumerate() {
            memory.store_u32(address + 4 * i as u32, *word);
        }
    }

    fn mask(f: Field) -> u32 {
        if f.width == 32 { u32::MAX } else { (1 << f.width) - 1 }
    }

    fn get(&self, f: Field) -> u32 {
        (self.0[f.word] >> f.shift) & Self::mask(f)
    }

    // As a store to a bitfield does: the value is cut to the field's width.
    fn set(&mut self, f: Field, value: u32) {
        let mask = Self::mask(f) << f.shift;
        self.0[f.word] = (self.0[f.word] & !mask) | ((value << f.shift) & mask);
    }

    fn input_buffer_valid(&self, i: u32) -> bool {
        self.get(if i == 0 { INPUT_BUFFER_0_VALID } else { INPUT_BUFFER_1_VALID }) != 0
    }
    fn current_input_buffer_valid(&self) -> bool {
        self.input_buffer_valid(self.get(CURRENT_BUFFER))
    }
    fn any_input_buffer_valid(&self) -> bool {
        self.get(INPUT_BUFFER_0_VALID) != 0 || self.get(INPUT_BUFFER_1_VALID) != 0
    }
    fn input_buffer_address(&self, i: u32) -> u32 {
        self.get(if i == 0 { INPUT_BUFFER_0_PTR } else { INPUT_BUFFER_1_PTR })
    }
    fn current_input_buffer_address(&self) -> u32 {
        self.input_buffer_address(self.get(CURRENT_BUFFER))
    }
    fn input_buffer_packet_count(&self, i: u32) -> u32 {
        self.get(if i == 0 { INPUT_BUFFER_0_PACKET_COUNT } else { INPUT_BUFFER_1_PACKET_COUNT })
    }
    fn current_input_buffer_packet_count(&self) -> u32 {
        self.input_buffer_packet_count(self.get(CURRENT_BUFFER))
    }
    fn consume_only(&self) -> bool {
        (self.get(INPUT_BUFFER_0_PACKET_COUNT) | self.get(INPUT_BUFFER_1_PACKET_COUNT)) == 0
    }
    fn stereo(&self) -> u32 {
        self.get(IS_STEREO)
    }
}

// ---- packet header ----------------------------------------------------------------------
fn packet_frame_count(p: &[u8]) -> u8 {
    p[0] >> 2
}
fn packet_is_xma2(p: &[u8]) -> bool {
    p[2] & 0x7 == 1
}
fn packet_skip_count(p: &[u8]) -> u8 {
    p[3]
}
fn packet_frame_offset(p: &[u8]) -> u32 {
    let v = ((u32::from(p[0]) & 0x3) << 13) | (u32::from(p[1]) << 5) | (u32::from(p[2]) >> 3);
    (v & 0xFFFF) + BITS_PER_PACKET_HEADER
}

// ---- bit stream over a big-endian buffer ------------------------------------------------
struct BitStream<'a> {
    buffer: &'a [u8],
    offset_bits: usize,
    size_bits: usize,
}

impl<'a> BitStream<'a> {
    fn new(buffer: &'a [u8], size_bits: usize) -> Self {
        Self { buffer, offset_bits: 0, size_bits }
    }
    fn set_offset(&mut self, bits: usize) {
        self.offset_bits = bits.min(self.size_bits);
    }
    fn advance(&mut self, bits: usize) {
        self.set_offset(self.offset_bits + bits);
    }
    fn bits_remaining(&self) -> usize {
        self.size_bits - self.offset_bits
    }

    // At most 57 bits, which fits in the 8 bytes read.
    fn peek(&self, bits: usize) -> u64 {
        let offset_bytes = (self.offset_bits >> 3).min((self.size_bits - 64) >> 3);
        let relative = self.offset_bits - (offset_bytes << 3);
        let word = u64::from_be_bytes(self.buffer[offset_bytes..offset_bytes + 8].try_into().expect("8 bytes"));
        word.wrapping_shr(64usize.wrapping_sub(relative + bits) as u32) & ((1u64 << bits) - 1)
    }
    fn read(&mut self, bits: usize) -> u64 {
        let value = self.peek(bits);
        self.advance(bits);
        value
    }

    // Copies `bits` to `dest`, keeping the source's alignment within the first byte;
    // returns that alignment (bits of padding before the data).
    fn copy(&mut self, dest: &mut [u8], bits: usize) -> usize {
        let offset_bytes = self.offset_bits >> 3;
        let relative = self.offset_bits - (offset_bytes << 3);
        let mut left = bits;
        let mut out = 0;
        if relative != 0 {
            let value = self.peek(8 - relative);
            dest[out] &= !((1u8 << relative) - 1);
            dest[out] |= value as u8;
            // A damaged length can be shorter than the rest of the byte (the C++ wraps its
            // count and copies for ever): the byte is all there is.
            if left <= 8 - relative {
                self.advance(left);
                return relative;
            }
            left -= 8 - relative;
            self.advance(8 - relative);
            out += 1;
        }
        if left >= 8 {
            let whole = left / 8;
            // A damaged length can run past the buffer: what is not there stays zero.
            let from = (offset_bytes + out).min(self.buffer.len());
            let there = whole.min(self.buffer.len() - from);
            dest[out..out + there].copy_from_slice(&self.buffer[from..from + there]);
            out += whole;
            self.advance(whole * 8);
            left -= whole * 8;
        }
        if left != 0 {
            let value = self.peek(left) << (8 - left);
            dest[out] &= (1u8 << left) - 1;
            dest[out] |= value as u8;
            self.advance(left);
        }
        relative
    }
}

// ---- the PCM output ring ----------------------------------------------------------------
struct RingBuffer {
    address: u32,
    capacity: u32,
    read: u32,
    write: u32,
}

impl RingBuffer {
    fn empty(&self) -> bool {
        self.read == self.write
    }
    fn set_read_offset(&mut self, offset: u32) {
        self.read = offset.checked_rem(self.capacity).unwrap_or(0);
    }
    fn set_write_offset(&mut self, offset: u32) {
        self.write = offset.checked_rem(self.capacity).unwrap_or(0);
    }
    fn write_count(&self) -> u32 {
        if self.read == self.write {
            self.capacity
        } else if self.write < self.read {
            self.read - self.write
        } else {
            (self.capacity - self.write) + self.read
        }
    }
    fn write(&mut self, memory: Memory, source: &[u8]) {
        let count = (source.len() as u32).min(self.capacity);
        if count == 0 {
            return;
        }
        if self.write + count < self.capacity {
            memory.store_bytes(self.address.wrapping_add(self.write), &source[..count as usize]);
            self.write += count;
        } else {
            let left = self.capacity - self.write;
            memory.store_bytes(self.address.wrapping_add(self.write), &source[..left as usize]);
            memory.store_bytes(self.address, &source[left as usize..count as usize]);
            self.write = count - left;
        }
    }
}

/// The guest address of a physical one: the physical arena first, as everything the
/// hardware is handed comes from there (runtime/kernel/physical.h).
fn from_physical(physical: u32) -> u32 {
    let top = mw2_kernel_physical_top.load(Ordering::Acquire);
    if physical < top.wrapping_sub(PHYSICAL_BASE) {
        return PHYSICAL_BASE.wrapping_add(physical);
    }
    let cached = 0x8000_0000u32.wrapping_add(physical);
    if (0x8200_0000..HEAP_END).contains(&cached) { cached } else { 0 }
}

fn load_packet(memory: Memory, address: u32) -> [u8; BYTES_PER_PACKET as usize] {
    let mut packet = [0; BYTES_PER_PACKET as usize];
    memory.load_bytes(address, &mut packet);
    packet
}

fn load_header(memory: Memory, address: u32) -> [u8; 4] {
    memory.u32_at(address).to_be_bytes()
}

#[derive(Default)]
struct PacketInfo {
    frame_count: u8,
    current_frame: u8,
    current_frame_size: u32,
}

impl PacketInfo {
    fn is_last_frame_in_packet(&self) -> bool {
        self.frame_count == 0 || self.current_frame == self.frame_count - 1
    }
}

// ---- one hardware context ---------------------------------------------------------------

// What a decode works on, held under the context's lock.
struct State {
    // Made for the stream's rate and channels, and made again when they change.
    decoder: Option<Box<wmapro::Decoder>>,
    #[cfg(mw2_xma_rig)]
    ffmpeg: Option<ffmpeg::Codec>,
    input_buffer: [u8; BYTES_PER_PACKET_DATA * 2],
    // Byte 0 carries the bit alignment; then the frame, with FFmpeg's padding. The longest
    // frame, 0x7FFE bits after up to 7 of alignment, takes 4097 bytes (the C++'s array is a
    // byte short, and FFmpeg's padding reads past it).
    xma_frame: [u8; XMA_FRAME_SIZE],
    raw_frame: [u8; BYTES_PER_FRAME_CHANNEL as usize * 2],
    remaining_blocks: i32,
    remaining_subframes: u8,
    loop_frame_output_limit: u8,
    loop_start_skip_pending: bool,
}

struct Context {
    state: Mutex<State>,
    allocated: AtomicBool,
    // For the report, which takes no lock a decode may hold.
    frames_decoded: AtomicU32,
    kicks: AtomicU32,
}

static CONTEXTS: [Context; CONTEXT_COUNT] = [const {
    Context {
        state: Mutex::new(State {
            decoder: None,
            #[cfg(mw2_xma_rig)]
            ffmpeg: None,
            input_buffer: [0; BYTES_PER_PACKET_DATA * 2],
            xma_frame: [0; XMA_FRAME_SIZE],
            raw_frame: [0; BYTES_PER_FRAME_CHANNEL as usize * 2],
            remaining_blocks: 0,
            remaining_subframes: 0,
            loop_frame_output_limit: 0,
            loop_start_skip_pending: false,
        }),
        allocated: AtomicBool::new(false),
        frames_decoded: AtomicU32::new(0),
        kicks: AtomicU32::new(0),
    }
}; CONTEXT_COUNT];

static CONTEXT_BASE: AtomicU32 = AtomicU32::new(0); // guest address of context 0
static READY: AtomicBool = AtomicBool::new(false);
static ALLOCATION: Mutex<()> = Mutex::new(());
// The most allocated at once, for the report, which may run on a fault and takes no lock.
static PEAK_ALLOCATED: AtomicU32 = AtomicU32::new(0);
static KICKS: AtomicU32 = AtomicU32::new(0);
static LOCKS: AtomicU32 = AtomicU32::new(0);
static CLEARS: AtomicU32 = AtomicU32::new(0);
static UNKNOWN_STORES: AtomicU32 = AtomicU32::new(0);
static LOCK_ALL_STORES: AtomicU32 = AtomicU32::new(0);

fn lock(id: usize) -> MutexGuard<'static, State> {
    CONTEXTS[id].state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn guest_address(id: usize) -> u32 {
    CONTEXT_BASE.load(Ordering::Relaxed) + id as u32 * CONTEXT_SIZE
}

fn clear(memory: Memory, id: usize) {
    let mut state = lock(id);
    let address = guest_address(id);
    record::cleared(memory, id, address);
    let mut data = ContextData::load(memory, address);
    state.clear(&mut data);
    data.store(memory, address);
}

// A zeroed context holds no frame in flight. The decoder is kept as it is: the C++ build
// flushes FFmpeg's here, but its xmaframes has no flush of its own, so the next voice given
// this context starts with the last one's overlap in it, as on the C++ build.
fn release(memory: Memory, id: usize) {
    let mut state = lock(id);
    // Written down before another thread can be given the context.
    record::released(id);
    CONTEXTS[id].allocated.store(false, Ordering::Release);
    memory.zero(guest_address(id), CONTEXT_SIZE);
    state.remaining_subframes = 0;
    state.loop_frame_output_limit = 0;
    state.loop_start_skip_pending = false;
}

// Runs the context until its input is spent or its output ring is full.
fn work(memory: Memory, id: usize) {
    if !CONTEXTS[id].allocated.load(Ordering::Acquire) {
        return;
    }
    let mut state = lock(id);
    CONTEXTS[id].kicks.fetch_add(1, Ordering::Relaxed);
    record::kicked(memory, id, guest_address(id));
    state.work(memory, id as u32, guest_address(id));
}

impl State {
    fn clear(&mut self, data: &mut ContextData) {
        data.set(INPUT_BUFFER_0_VALID, 0);
        data.set(INPUT_BUFFER_1_VALID, 0);
        data.set(OUTPUT_BUFFER_VALID, 0);
        data.set(INPUT_BUFFER_READ_OFFSET, BITS_PER_PACKET_HEADER);
        data.set(OUTPUT_BUFFER_READ_OFFSET, 0);
        data.set(OUTPUT_BUFFER_WRITE_OFFSET, 0);
        self.remaining_subframes = 0;
        self.loop_frame_output_limit = 0;
        self.loop_start_skip_pending = false;
    }

    fn work(&mut self, memory: Memory, id: u32, address: u32) {
        let mut data = ContextData::load(memory, address);
        let initial = data;
        xma_trace!(
            "xma: context %u kicked: in0 %08X x%u%s in1 %08X x%u%s out %08X x%u blocks%s, offset %u",
            id,
            data.get(INPUT_BUFFER_0_PTR),
            data.get(INPUT_BUFFER_0_PACKET_COUNT),
            if data.get(INPUT_BUFFER_0_VALID) != 0 { c" valid" } else { c"" },
            data.get(INPUT_BUFFER_1_PTR),
            data.get(INPUT_BUFFER_1_PACKET_COUNT),
            if data.get(INPUT_BUFFER_1_VALID) != 0 { c" valid" } else { c"" },
            data.get(OUTPUT_BUFFER_PTR),
            data.get(OUTPUT_BUFFER_BLOCK_COUNT),
            if data.get(OUTPUT_BUFFER_VALID) != 0 { c" valid" } else { c"" },
            data.get(INPUT_BUFFER_READ_OFFSET)
        );

        if data.get(OUTPUT_BUFFER_VALID) == 0 {
            return;
        }

        let mut output = self.prepare_output_ring(&data, id);

        if data.consume_only() {
            // Nothing to drain: leave the ring offsets alone, or stale PCM is re-read.
            if self.remaining_subframes == 0 {
                return;
            }
            self.consume(memory, &mut output, &data);
            data.set(OUTPUT_BUFFER_WRITE_OFFSET, output.write / OUTPUT_BYTES_PER_BLOCK);
            if output.empty() {
                data.set(OUTPUT_BUFFER_VALID, 0);
            }
            store_merged(memory, &data, &initial, address);
            return;
        }

        // The blocks one pass writes (subframe_decode_count, at least 1) plus the headroom
        // the title asked for.
        let effective_sdc = data.get(SUBFRAME_DECODE_COUNT).max(1);
        let minimum_blocks = (effective_sdc + data.get(OUTPUT_BUFFER_PADDING)) as i32;
        if minimum_blocks > self.remaining_blocks {
            xma_trace!(
                "xma: context %u: no room for a pass (%d of %d blocks)",
                id,
                minimum_blocks,
                self.remaining_blocks
            );
            store_merged(memory, &data, &initial, address);
            return;
        }

        while self.remaining_blocks >= minimum_blocks {
            let pre_offset = data.get(INPUT_BUFFER_READ_OFFSET);
            let pre_remaining = self.remaining_subframes;
            self.decode(memory, &mut data, id);
            self.consume(memory, &mut output, &data);
            // A partly consumed frame has to be drained now: the title polls for the
            // remainder without kicking again.
            if (!data.any_input_buffer_valid() || data.get(ERROR_STATUS) == 4) && self.remaining_subframes == 0 {
                break;
            }
            // No progress and nothing pending: wait for the next buffer.
            if pre_remaining == 0 && data.get(INPUT_BUFFER_READ_OFFSET) == pre_offset && self.remaining_subframes == 0 {
                break;
            }
        }

        if initial.any_input_buffer_valid() {
            data.set(OUTPUT_BUFFER_WRITE_OFFSET, output.write / OUTPUT_BYTES_PER_BLOCK);
        } else if data.get(OUTPUT_BUFFER_WRITE_OFFSET) != data.get(OUTPUT_BUFFER_READ_OFFSET) {
            // Starved on input: some titles read write == read as the stall.
            data.set(OUTPUT_BUFFER_WRITE_OFFSET, data.get(OUTPUT_BUFFER_READ_OFFSET));
            data.set(OUTPUT_BUFFER_VALID, 0);
        }

        if self.remaining_blocks == 0 && output.empty() {
            data.set(OUTPUT_BUFFER_VALID, 0); // full
        }

        store_merged(memory, &data, &initial, address);
    }

    fn prepare_output_ring(&mut self, data: &ContextData, id: u32) -> RingBuffer {
        let capacity = data.get(OUTPUT_BUFFER_BLOCK_COUNT) * OUTPUT_BYTES_PER_BLOCK;
        if capacity > OUTPUT_MAX_SIZE_BYTES {
            xma_error!("xma: context %u: output ring of %u bytes is larger than the hardware's", id, capacity);
        }
        let mut ring = RingBuffer { address: from_physical(data.get(OUTPUT_BUFFER_PTR)), capacity, read: 0, write: 0 };
        ring.set_read_offset(data.get(OUTPUT_BUFFER_READ_OFFSET) * OUTPUT_BYTES_PER_BLOCK);
        ring.set_write_offset(data.get(OUTPUT_BUFFER_WRITE_OFFSET) * OUTPUT_BYTES_PER_BLOCK);
        self.remaining_blocks = (ring.write_count() / OUTPUT_BYTES_PER_BLOCK) as i32;
        ring
    }

    fn consume(&mut self, memory: Memory, ring: &mut RingBuffer, data: &ContextData) {
        if self.remaining_subframes == 0 {
            return;
        }
        let total_subframes = ((BYTES_PER_FRAME_CHANNEL / OUTPUT_BYTES_PER_BLOCK) << data.stereo()) as u8;
        if self.loop_frame_output_limit > 0 {
            let consumed = total_subframes.wrapping_sub(self.remaining_subframes);
            if consumed >= self.loop_frame_output_limit {
                // Loop end truncation: drop the rest of the frame, charging the headroom
                // as if it had completed.
                self.remaining_blocks -= data.get(OUTPUT_BUFFER_PADDING) as i32;
                self.remaining_subframes = 0;
                self.loop_frame_output_limit = 0;
                return;
            }
        }

        let effective_sdc = data.get(SUBFRAME_DECODE_COUNT).max(1) as u8;
        let mut to_write = self.remaining_subframes.min(effective_sdc) as i8;
        if self.loop_frame_output_limit > 0 {
            let consumed = total_subframes.wrapping_sub(self.remaining_subframes);
            let until_limit = self.loop_frame_output_limit.wrapping_sub(consumed) as i8;
            if to_write > until_limit {
                to_write = until_limit;
            }
        }

        let raw_offset = total_subframes.wrapping_sub(self.remaining_subframes) as i8;
        let start = OUTPUT_BYTES_PER_BLOCK as isize * raw_offset as isize;
        let size = OUTPUT_BYTES_PER_BLOCK as usize * to_write.max(0) as usize;
        match usize::try_from(start).ok().and_then(|start| self.raw_frame.get(start..start + size)) {
            Some(blocks) => ring.write(memory, blocks),
            // The title turned a stereo context mono mid-frame: the blocks it is owed are
            // not in the frame, so it gets silence.
            None => ring.write(memory, &vec![0; size]),
        }

        let headroom = if self.remaining_subframes as i32 - to_write as i32 == 0 {
            data.get(OUTPUT_BUFFER_PADDING) as i8
        } else {
            0
        };
        self.remaining_blocks -= i32::from(to_write) + i32::from(headroom);
        self.remaining_subframes = self.remaining_subframes.wrapping_sub(to_write as u8);
    }

    fn update_loop_status(&mut self, data: &mut ContextData) {
        let loop_count = data.get(LOOP_COUNT);
        if loop_count == 0 {
            return;
        }
        let loop_start = data.get(LOOP_START).max(BITS_PER_PACKET_HEADER);
        let loop_end = data.get(LOOP_END).max(BITS_PER_PACKET_HEADER);
        if data.get(INPUT_BUFFER_READ_OFFSET) != loop_end {
            return;
        }
        data.set(INPUT_BUFFER_READ_OFFSET, loop_start);
        self.loop_start_skip_pending = true;
        if loop_count != 255 {
            data.set(LOOP_COUNT, loop_count - 1);
        }
    }

    // The buffer and packet of a packet index counted on from the current buffer, if
    // that packet is there.
    fn packet_handle(
        data: &ContextData,
        id: u32,
        mut buffer: u32,
        mut packet: u32,
        current_packet_count: u32,
    ) -> Option<(u32, u32)> {
        let in_next_buffer = packet >= current_packet_count;
        if in_next_buffer {
            buffer ^= 1;
            packet -= current_packet_count;
        }
        let which = if in_next_buffer { c"next" } else { c"current" };
        if !data.input_buffer_valid(buffer) {
            return None;
        }
        if data.input_buffer_address(buffer) == 0 {
            xma_error!("xma: context %u: the %s buffer is marked valid but null", id, which);
            return None;
        }
        if packet >= data.input_buffer_packet_count(buffer) {
            xma_error!("xma: context %u: packet %u is past the end of the %s buffer", id, packet, which);
            return None;
        }
        Some((buffer, packet))
    }

    fn next_packet(
        memory: Memory,
        data: &ContextData,
        id: u32,
        next_packet_index: u32,
        current_packet_count: u32,
    ) -> Option<[u8; BYTES_PER_PACKET as usize]> {
        let (buffer, packet) =
            Self::packet_handle(data, id, data.get(CURRENT_BUFFER), next_packet_index, current_packet_count)?;
        let address = from_physical(data.input_buffer_address(buffer)).wrapping_add(packet * BYTES_PER_PACKET);
        Some(load_packet(memory, address))
    }

    // The bit offset of the first frame that starts at or after this packet, or
    // BITS_PER_PACKET_HEADER when there is none in the buffer.
    fn next_packet_read_offset_in(memory: Memory, buffer: u32, mut next_packet_index: u32, packet_count: u32) -> u32 {
        while next_packet_index < packet_count {
            let header = load_header(memory, buffer.wrapping_add(next_packet_index * BYTES_PER_PACKET));
            let frame_offset = packet_frame_offset(&header);
            if frame_offset <= MAX_FRAME_SIZE_IN_BITS {
                return next_packet_index * BITS_PER_PACKET + frame_offset;
            }
            // Entirely the continuation of a split frame: follow its skip count.
            let skip = packet_skip_count(&header);
            if skip == 0xFF {
                break;
            }
            next_packet_index += u32::from(skip) + 1;
        }
        BITS_PER_PACKET_HEADER
    }

    fn next_packet_read_offset(
        memory: Memory,
        data: &ContextData,
        id: u32,
        next_packet_index: u32,
        current_packet_count: u32,
    ) -> u32 {
        let Some((buffer, packet)) =
            Self::packet_handle(data, id, data.get(CURRENT_BUFFER), next_packet_index, current_packet_count)
        else {
            return BITS_PER_PACKET_HEADER;
        };
        Self::next_packet_read_offset_in(
            memory,
            from_physical(data.input_buffer_address(buffer)),
            packet,
            data.input_buffer_packet_count(buffer),
        )
    }

    fn packet_info(packet: &[u8], frame_offset: u32) -> PacketInfo {
        let mut info = PacketInfo::default();
        let first_frame_offset = packet_frame_offset(packet);
        let mut stream = BitStream::new(packet, BITS_PER_PACKET as usize);
        stream.set_offset(first_frame_offset as usize);
        if frame_offset < first_frame_offset {
            // The tail of a frame split from the previous packet.
            info.current_frame = 0;
            info.current_frame_size = first_frame_offset - frame_offset;
        }
        loop {
            if stream.bits_remaining() < BITS_PER_FRAME_HEADER {
                break;
            }
            let frame_size = stream.peek(BITS_PER_FRAME_HEADER);
            if frame_size == 0 || frame_size == MAX_FRAME_LENGTH {
                break;
            }
            if stream.offset_bits == frame_offset as usize {
                info.current_frame = info.frame_count;
                info.current_frame_size = frame_size as u32;
            }
            info.frame_count = info.frame_count.wrapping_add(1);
            if frame_size as usize > stream.bits_remaining() {
                break; // last, split
            }
            stream.advance(frame_size as usize - 1);
            if stream.read(1) == 0 {
                break; // no frame follows
            }
        }
        if packet_is_xma2(packet) {
            let header_count = packet_frame_count(packet);
            if header_count > info.frame_count {
                // A frame header split across the boundary could not be peeked; the XMA2
                // header's count is authoritative.
                if info.current_frame_size == 0 {
                    info.current_frame = info.frame_count;
                }
                info.frame_count = header_count;
            } else if header_count != info.frame_count {
                xma_error!(
                    "xma: packet header says %u frames, the walk found %u",
                    u32::from(header_count),
                    u32::from(info.frame_count)
                );
            }
        }
        info
    }

    fn packet_number(size: u32, bit_offset: u32) -> Option<u32> {
        if bit_offset < BITS_PER_PACKET_HEADER || u64::from(bit_offset) >= u64::from(size) << 3 {
            return None;
        }
        Some((bit_offset >> 3) / BYTES_PER_PACKET)
    }

    fn swap_input_buffer(data: &mut ContextData) {
        if data.get(CURRENT_BUFFER) == 0 {
            data.set(INPUT_BUFFER_0_VALID, 0);
        } else {
            data.set(INPUT_BUFFER_1_VALID, 0);
        }
        data.set(CURRENT_BUFFER, data.get(CURRENT_BUFFER) ^ 1);
        data.set(INPUT_BUFFER_READ_OFFSET, BITS_PER_PACKET_HEADER);
    }

    // Sets the packet's size and the alignment byte in front of the frame.
    fn prepare_packet(&mut self, frame_size: u32, frame_padding: u32) -> usize {
        let bits = frame_padding + frame_size;
        let size = 1 + bits.div_ceil(8);
        let padding_end = size * 8 - (8 + bits);
        self.xma_frame[0] = (((frame_padding & 7) << 5) | ((padding_end & 7) << 2)) as u8;
        size as usize
    }

    // Planar float to interleaved 16-bit big-endian PCM, saturated.
    fn convert_frame(
        &mut self,
        stereo: bool,
        left: &[f32; SAMPLES_PER_FRAME],
        right: Option<&[f32; SAMPLES_PER_FRAME]>,
    ) {
        const SCALE: f32 = ((1 << 15) - 1) as f32;
        let sample = |v: f32| ((v.clamp(-1.0, 1.0) * SCALE) as i16).to_be_bytes();
        let out = &mut self.raw_frame;
        match (stereo, right) {
            (false, _) => {
                for (to, l) in out.chunks_exact_mut(2).zip(left) {
                    to.copy_from_slice(&sample(*l));
                }
            }
            (true, Some(right)) => {
                for ((to, l), r) in out.chunks_exact_mut(4).zip(left).zip(right) {
                    to[..2].copy_from_slice(&sample(*l));
                    to[2..].copy_from_slice(&sample(*r));
                }
            }
            // A stereo stream the decoder gave one channel of plays it on both.
            (true, None) => {
                for (to, l) in out.chunks_exact_mut(4).zip(left) {
                    let l = sample(*l);
                    to[..2].copy_from_slice(&l);
                    to[2..].copy_from_slice(&l);
                }
            }
        }
    }

    // Decodes the frame in xma_frame, with a decoder for the stream's rate and channels;
    // the channels it gave, or None.
    fn decode_frame(
        &mut self,
        id: u32,
        size: usize,
        sample_rate_id: u32,
        stereo: bool,
        left: &mut [f32; SAMPLES_PER_FRAME],
        right: &mut [f32; SAMPLES_PER_FRAME],
    ) -> Option<u32> {
        let sample_rate = ID_TO_SAMPLE_RATE[sample_rate_id.min(3) as usize];
        let channels = if stereo { 2 } else { 1 };
        let decoder = match &mut self.decoder {
            Some(decoder) if decoder.sample_rate() == sample_rate && decoder.channels() == channels => decoder,
            slot => slot.insert(wmapro::Decoder::new(sample_rate, channels)),
        };
        let frame = &self.xma_frame[..size + PADDING];
        let mut output = [[0f32; SAMPLES_PER_FRAME]; 2];
        let mut log = |text: &str| {
            if count_error() {
                log_warn!("xma: ffmpeg: %s", crate::log::c_string(text).as_c_str());
            }
        };
        let decoded = decoder.decode(frame, size, &mut output, &mut log);
        let got = match &decoded {
            wmapro::Decoded::Frame => Some(channels as u32),
            wmapro::Decoded::None => None,
            wmapro::Decoded::Refused(error) => {
                xma_error!("xma: context %u: send_packet: %s", id, *error);
                None
            }
        };
        #[cfg(mw2_xma_rig)]
        self.check_with_ffmpeg(id, size, sample_rate, channels, &decoded, &output);
        [*left, *right] = output;
        got
    }

    // The rig's: FFmpeg's decoder on the same frame, which must give the same bits.
    #[cfg(mw2_xma_rig)]
    fn check_with_ffmpeg(
        &mut self,
        id: u32,
        size: usize,
        sample_rate: i32,
        channels: usize,
        decoded: &wmapro::Decoded,
        output: &[[f32; SAMPLES_PER_FRAME]; 2],
    ) {
        let codec = self.ffmpeg.get_or_insert_with(ffmpeg::Codec::new);
        let mut theirs = [[0f32; SAMPLES_PER_FRAME]; 2];
        let [left, right] = &mut theirs;
        let outcome = codec.decode(sample_rate, channels as i32, &self.xma_frame[..size + PADDING], left, right);
        let same_outcome = match (decoded, &outcome) {
            (wmapro::Decoded::Frame, Ok(got)) => *got as usize == channels,
            (wmapro::Decoded::None, Ok(0)) => true,
            (wmapro::Decoded::Refused(ours), Err(error)) => error.as_c_str() == *ours,
            _ => false,
        };
        if !same_outcome {
            log_warn!("xma: decoders differ: context %u, a frame of %u bytes: not the same outcome", id, size as u32);
            return;
        }
        if !matches!(decoded, wmapro::Decoded::Frame) {
            return;
        }
        for channel in 0..channels {
            for i in 0..SAMPLES_PER_FRAME {
                let (a, b) = (output[channel][i].to_bits(), theirs[channel][i].to_bits());
                if a != b {
                    log_warn!(
                        "xma: decoders differ: context %u, channel %u, sample %u: port %08X, FFmpeg %08X",
                        id,
                        channel as u32,
                        i as u32,
                        a,
                        b
                    );
                    return;
                }
            }
        }
    }

    fn decode(&mut self, memory: Memory, data: &mut ContextData, id: u32) {
        if !data.any_input_buffer_valid() || self.remaining_subframes > 0 {
            return;
        }
        if !data.current_input_buffer_valid() {
            Self::swap_input_buffer(data);
            if !data.current_input_buffer_valid() {
                return;
            }
        }

        let current_input = from_physical(data.current_input_buffer_address());
        self.input_buffer.fill(0);

        let mut is_loop_end_frame = false;
        if data.get(LOOP_COUNT) > 0 {
            let loop_end = data.get(LOOP_END).max(BITS_PER_PACKET_HEADER);
            is_loop_end_frame = data.get(INPUT_BUFFER_READ_OFFSET) == loop_end;
        }
        self.update_loop_status(data);

        if data.get(OUTPUT_BUFFER_BLOCK_COUNT) == 0 {
            xma_error!("xma: context %u: an output ring of zero blocks", id);
            return;
        }
        if data.get(INPUT_BUFFER_READ_OFFSET) < BITS_PER_PACKET_HEADER {
            data.set(INPUT_BUFFER_READ_OFFSET, BITS_PER_PACKET_HEADER); // inside the header
        }

        let input_size = data.current_input_buffer_packet_count() * BYTES_PER_PACKET;
        let packet_count = input_size / BYTES_PER_PACKET;
        let Some(packet_index) = Self::packet_number(input_size, data.get(INPUT_BUFFER_READ_OFFSET)) else {
            xma_error!(
                "xma: context %u: read offset %u is outside its %u-packet buffer",
                id,
                data.get(INPUT_BUFFER_READ_OFFSET),
                packet_count
            );
            return;
        };

        let packet = load_packet(memory, current_input.wrapping_add(packet_index * BYTES_PER_PACKET));
        let packet_first_frame_offset = packet_frame_offset(&packet);
        let mut relative_offset = data.get(INPUT_BUFFER_READ_OFFSET) % BITS_PER_PACKET;

        // Before the first frame of the packet is the tail of a frame whose start we never
        // had: skip to the first complete one.
        if relative_offset < packet_first_frame_offset {
            data.set(INPUT_BUFFER_READ_OFFSET, packet_index * BITS_PER_PACKET + packet_first_frame_offset);
            relative_offset = packet_first_frame_offset;
        }

        let skip_count = packet_skip_count(&packet);
        if skip_count == 0xFF {
            // No frame starts here at all; step to the next packet.
            let next_index = packet_index + 1;
            let next_offset = Self::next_packet_read_offset(memory, data, id, next_index, packet_count);
            if next_index >= packet_count || next_offset == BITS_PER_PACKET_HEADER {
                Self::swap_input_buffer(data);
            }
            data.set(INPUT_BUFFER_READ_OFFSET, next_offset);
            return;
        }

        let mut info = Self::packet_info(&packet, relative_offset);
        let next_packet_index = packet_index + u32::from(skip_count) + 1;
        let data_part = &packet[BYTES_PER_PACKET_HEADER as usize..];

        if info.current_frame_size == 0 {
            // The 15-bit length is split across the boundary: combine packets.
            let Some(next) = Self::next_packet(memory, data, id, next_packet_index, packet_count) else {
                Self::swap_input_buffer(data);
                return;
            };
            self.input_buffer[..BYTES_PER_PACKET_DATA].copy_from_slice(data_part);
            self.input_buffer[BYTES_PER_PACKET_DATA..].copy_from_slice(&next[BYTES_PER_PACKET_HEADER as usize..]);
            let mut combined =
                BitStream::new(&self.input_buffer, (BITS_PER_PACKET - BITS_PER_PACKET_HEADER) as usize * 2);
            combined.set_offset((relative_offset - BITS_PER_PACKET_HEADER) as usize);
            let frame_size = combined.peek(BITS_PER_FRAME_HEADER);
            if frame_size == MAX_FRAME_LENGTH {
                data.set(ERROR_STATUS, 4);
                return;
            }
            info.current_frame_size = frame_size as u32;
        }

        // What is left of the input up to this packet's end.
        let bits_remaining = ((packet_index + 1) * BITS_PER_PACKET).saturating_sub(data.get(INPUT_BUFFER_READ_OFFSET));
        let bits_to_copy = bits_remaining.min(info.current_frame_size);
        if bits_to_copy == 0 {
            xma_error!("xma: context %u: nothing to copy at offset %u", id, data.get(INPUT_BUFFER_READ_OFFSET));
            Self::swap_input_buffer(data);
            return;
        }

        if info.is_last_frame_in_packet() && bits_remaining < info.current_frame_size {
            // A split frame: bring in the next packet's data.
            let Some(next) = Self::next_packet(memory, data, id, next_packet_index, packet_count) else {
                data.set(ERROR_STATUS, 4);
                return;
            };
            self.input_buffer[BYTES_PER_PACKET_DATA..].copy_from_slice(&next[BYTES_PER_PACKET_HEADER as usize..]);
        }
        self.input_buffer[..BYTES_PER_PACKET_DATA].copy_from_slice(data_part);
        let mut stream = BitStream::new(&self.input_buffer, (BITS_PER_PACKET - BITS_PER_PACKET_HEADER) as usize * 2);
        stream.set_offset((relative_offset - BITS_PER_PACKET_HEADER) as usize);

        self.xma_frame.fill(0);
        let padding_start = stream.copy(&mut self.xma_frame[1..], info.current_frame_size as usize) as u8;
        let size = self.prepare_packet(info.current_frame_size, u32::from(padding_start));
        let stereo = data.stereo() != 0;
        let mut left = [0f32; SAMPLES_PER_FRAME];
        let mut right = [0f32; SAMPLES_PER_FRAME];
        if let Some(channels) = self.decode_frame(id, size, data.get(SAMPLE_RATE), stereo, &mut left, &mut right) {
            CONTEXTS[id as usize].frames_decoded.fetch_add(1, Ordering::Relaxed);
            self.convert_frame(stereo, &left, (stereo && channels == 2).then_some(&right));
            self.remaining_subframes = (4u32 << data.stereo()) as u8;
            self.loop_frame_output_limit =
                if is_loop_end_frame { ((data.get(LOOP_SUBFRAME_END) + 1) << data.stereo()) as u8 } else { 0 };
            if self.loop_start_skip_pending {
                let skip = (data.get(LOOP_SUBFRAME_SKIP) << data.stereo()) as u8;
                if skip < self.remaining_subframes {
                    self.remaining_subframes -= skip;
                }
                self.loop_start_skip_pending = false;
            }
        }

        if !info.is_last_frame_in_packet() {
            let next_frame_offset = (data.get(INPUT_BUFFER_READ_OFFSET) + bits_to_copy) % BITS_PER_PACKET;
            data.set(INPUT_BUFFER_READ_OFFSET, packet_index * BITS_PER_PACKET + next_frame_offset);
            return;
        }

        let mut next_offset = Self::next_packet_read_offset(memory, data, id, next_packet_index, packet_count);
        if next_packet_index >= packet_count || next_offset == BITS_PER_PACKET_HEADER {
            Self::swap_input_buffer(data);
        }
        if next_offset == BITS_PER_PACKET_HEADER && data.any_input_buffer_valid() {
            // The start of the other buffer: its first packet's first frame, or straight
            // on if it has none.
            next_offset = packet_frame_offset(&load_header(memory, from_physical(data.current_input_buffer_address())));
            if next_offset > MAX_FRAME_SIZE_IN_BITS {
                Self::swap_input_buffer(data);
                return;
            }
        }
        data.set(INPUT_BUFFER_READ_OFFSET, next_offset);
    }
}

// Writes back only the fields the decoder owns, over whatever the title changed meanwhile.
fn store_merged(memory: Memory, data: &ContextData, initial: &ContextData, address: u32) {
    let mut fresh = ContextData::load(memory, address);
    fresh.set(LOOP_COUNT, data.get(LOOP_COUNT));
    fresh.set(OUTPUT_BUFFER_WRITE_OFFSET, data.get(OUTPUT_BUFFER_WRITE_OFFSET));
    for valid in [INPUT_BUFFER_0_VALID, INPUT_BUFFER_1_VALID, OUTPUT_BUFFER_VALID] {
        if initial.get(valid) != 0 && data.get(valid) == 0 {
            fresh.set(valid, 0);
        }
    }
    fresh.set(INPUT_BUFFER_READ_OFFSET, data.get(INPUT_BUFFER_READ_OFFSET));
    fresh.set(ERROR_STATUS, data.get(ERROR_STATUS));
    fresh.set(CURRENT_BUFFER, data.get(CURRENT_BUFFER));
    fresh.set(OUTPUT_BUFFER_READ_OFFSET, data.get(OUTPUT_BUFFER_READ_OFFSET));
    fresh.store(memory, address);
}

// ---- the decoder ------------------------------------------------------------------------

// The registers as the little-endian hardware holds them.
fn set_register(memory: Memory, index: u32, value: u32) {
    memory.store_u32(WINDOW + index * 4, value.swap_bytes());
}

fn context_id(guest_address: u32) -> Option<usize> {
    let ready = READY.load(Ordering::Acquire);
    let base = CONTEXT_BASE.load(Ordering::Relaxed);
    (ready && guest_address >= base && guest_address < base + CONTEXT_COUNT as u32 * CONTEXT_SIZE)
        .then(|| ((guest_address - base) / CONTEXT_SIZE) as usize)
}

pub fn initialise() {
    #[cfg(mw2_xma_rig)]
    ffmpeg::initialise();
    let memory = guest_memory::memory();

    // The contexts live in physical memory: the title finds a context's hardware index by
    // taking its physical address from the register. Not at physical address 0, which
    // reads as "no context" in too many places.
    let size = CONTEXT_COUNT as u32 * CONTEXT_SIZE;
    let mut base = allocate_physical(size, 256);
    if base != 0 && to_physical(base) == 0 {
        base = allocate_physical(size, 256);
    }
    if base == 0 {
        log_error!("xma: no physical memory for the contexts");
        return;
    }
    memory.zero(base, size);
    CONTEXT_BASE.store(base, Ordering::Relaxed);
    record::started(base);

    // The registers the title reads: the array's physical address, and a "current
    // context" that never matches a context the title owns, so its lock-all never sees a
    // busy one.
    set_register(memory, REG_CONTEXT_ARRAY_ADDRESS, to_physical(base));
    set_register(memory, REG_CURRENT_CONTEXT_INDEX, 0xFFFF);
    set_register(memory, REG_NEXT_CONTEXT_INDEX, 1);

    READY.store(true, Ordering::Release);
    log_info!("xma: %u contexts at %08X (physical %08X)", CONTEXT_COUNT as u32, base, to_physical(base));
}

/// The guest address of a free context (in the physical arena, as the title turns it back
/// into a physical address by masking), or 0 when all are taken.
pub fn allocate_context() -> u32 {
    if !READY.load(Ordering::Acquire) {
        return 0;
    }
    let _held = ALLOCATION.lock().unwrap_or_else(PoisonError::into_inner);
    let mut in_use = 0;
    let mut found = None;
    for (id, context) in CONTEXTS.iter().enumerate() {
        if context.allocated.load(Ordering::Acquire) {
            in_use += 1;
        } else if found.is_none() {
            context.allocated.store(true, Ordering::Release);
            found = Some(id);
            in_use += 1;
        }
    }
    PEAK_ALLOCATED.fetch_max(in_use, Ordering::Relaxed);
    if let Some(id) = found {
        record::allocated(id);
    }
    match found {
        Some(id) => guest_address(id),
        None => {
            log_warn!("xma: all %u contexts are in use", CONTEXT_COUNT as u32);
            0
        }
    }
}

pub fn release_context(guest_address: u32) {
    match context_id(guest_address) {
        Some(id) => release(guest_memory::memory(), id),
        None => log_warn!("xma: release of %08X, which is not a context", guest_address),
    }
}

// The kick, lock and clear registers are ten words of bits, one per context.
fn for_each_context(first: u32, mut bits: u32, mut each: impl FnMut(usize)) {
    while bits != 0 {
        let id = (first + bits.trailing_zeros()) as usize;
        if id < CONTEXT_COUNT {
            each(id);
        }
        bits &= bits - 1;
    }
}

/// A store into the register window. `value` is the register's content as the
/// little-endian hardware holds it.
pub fn register_store(address: u32, value: u32) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let memory = guest_memory::memory();
    let r = (address & 0xFFFF) / 4;
    if (REG_KICK0..REG_KICK0 + 10).contains(&r) {
        // Decoded here and now, so the title sees the results when the store returns.
        KICKS.fetch_add(1, Ordering::Relaxed);
        for_each_context((r - REG_KICK0) * 32, value, |id| work(memory, id));
    } else if (REG_LOCK0..REG_LOCK0 + 10).contains(&r) {
        // The title is about to edit these contexts: wait out a decode in flight on
        // another thread.
        LOCKS.fetch_add(1, Ordering::Relaxed);
        for_each_context((r - REG_LOCK0) * 32, value, |id| drop(lock(id)));
    } else if (REG_CLEAR0..REG_CLEAR0 + 10).contains(&r) {
        CLEARS.fetch_add(1, Ordering::Relaxed);
        for_each_context((r - REG_CLEAR0) * 32, value, |id| clear(memory, id));
    } else if r == REG_LOCK_ALL {
        // Written with 0 and then 3 around the title's lock of every context.
        LOCK_ALL_STORES.fetch_add(1, Ordering::Relaxed);
        xma_trace!("xma: store of %08X to register 601", value);
    } else if UNKNOWN_STORES.fetch_add(1, Ordering::Relaxed) < 8 {
        log_warn!("xma: store of %08X to register %04X", value, r);
    }
}

pub fn report() {
    if !cfg!(feature = "diagnostics") || !READY.load(Ordering::Acquire) {
        return;
    }
    record::finish();
    let mut frames = 0u64;
    let mut used = 0u32;
    for context in &CONTEXTS {
        frames += u64::from(context.frames_decoded.load(Ordering::Relaxed));
        used += u32::from(context.kicks.load(Ordering::Relaxed) != 0);
    }
    let peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
    log_info!(
        "xma: %u kicks, %u locks, %u clears, %u lock-all stores; %llu frames decoded on %u contexts (peak %u allocated); %u errors",
        KICKS.load(Ordering::Relaxed),
        LOCKS.load(Ordering::Relaxed),
        CLEARS.load(Ordering::Relaxed),
        LOCK_ALL_STORES.load(Ordering::Relaxed),
        frames,
        used,
        peak,
        ERRORS.load(Ordering::Relaxed)
    );
}

// MW2_XMA_RECORD=<file> (diagnostics builds): every context the title is given, releases,
// clears and kicks, each kick with the context block and the input buffers it sees, for
// tools/xma_diff.py to replay. The records are in the order the contexts saw them: each
// is written under the context's lock, a release before the context can be given out
// again. Little-endian:
//   'I' base u32                      the guest address of context 0
//   'A' id u16 / 'R' id u16           allocated, released
//   'C' id u16 block[64]              cleared; the block as it was
//   'B' key u64 size u32 bytes        an input buffer's bytes, once for each content
//   'K' id u16 block[64] n u8 then n of (physical u32, key u64, size u32)
//                                     kicked, with its valid input buffers
#[cfg(feature = "diagnostics")]
mod record {
    use super::{BYTES_PER_PACKET, CONTEXT_SIZE, ContextData, from_physical};
    use crate::env;
    use crate::guest::Memory;
    use crate::log::{log_info, log_warn};
    use crate::runtime::platform;
    use std::collections::HashSet;
    use std::fs::File;
    use std::hash::{DefaultHasher, Hasher};
    use std::io::{BufWriter, Write};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    struct Recording {
        file: BufWriter<File>,
        written: HashSet<(u64, u32)>,
        kicks: u64,
    }

    static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

    fn recording() -> MutexGuard<'static, Option<Recording>> {
        RECORDING.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn started(base: u32) {
        let Some(path) = env::text(c"MW2_XMA_RECORD") else { return };
        let file = match File::create(platform::path(path)) {
            Ok(file) => file,
            Err(_) => {
                log_warn!("xma: cannot write the recording %s", path);
                return;
            }
        };
        let mut held = recording();
        let recording = held.insert(Recording { file: BufWriter::new(file), written: HashSet::new(), kicks: 0 });
        write(recording, &[b"I", &base.to_le_bytes()]);
        log_info!("xma: recording to %s", path);
    }

    fn write(recording: &mut Recording, parts: &[&[u8]]) {
        for part in parts {
            if recording.file.write_all(part).is_err() {
                return;
            }
        }
    }

    fn id_bytes(id: usize) -> [u8; 2] {
        (id as u16).to_le_bytes()
    }

    pub fn allocated(id: usize) {
        if let Some(recording) = recording().as_mut() {
            write(recording, &[b"A", &id_bytes(id)]);
        }
    }

    pub fn released(id: usize) {
        if let Some(recording) = recording().as_mut() {
            write(recording, &[b"R", &id_bytes(id)]);
        }
    }

    fn block(memory: Memory, address: u32) -> [u8; CONTEXT_SIZE as usize] {
        let mut block = [0; CONTEXT_SIZE as usize];
        memory.load_bytes(address, &mut block);
        block
    }

    pub fn cleared(memory: Memory, id: usize, address: u32) {
        if let Some(recording) = recording().as_mut() {
            write(recording, &[b"C", &id_bytes(id), &block(memory, address)]);
        }
    }

    pub fn kicked(memory: Memory, id: usize, address: u32) {
        let mut held = recording();
        let Some(recording) = held.as_mut() else { return };
        let raw = block(memory, address);
        let data = ContextData::load(memory, address);
        let mut buffers = Vec::new();
        for i in 0..2 {
            let (physical, packets) = (data.input_buffer_address(i), data.input_buffer_packet_count(i));
            if !data.input_buffer_valid(i) || physical == 0 {
                continue;
            }
            // A buffer of no packets still has its first packet's header read.
            let bytes = memory.bytes(from_physical(physical), (packets.max(1) * BYTES_PER_PACKET) as usize);
            let mut hasher = DefaultHasher::new();
            hasher.write(&bytes);
            let key = (hasher.finish(), bytes.len() as u32);
            if recording.written.insert(key) {
                write(recording, &[b"B", &key.0.to_le_bytes(), &key.1.to_le_bytes(), &bytes]);
            }
            buffers.push((physical, key));
        }
        write(recording, &[b"K", &id_bytes(id), &raw, &[buffers.len() as u8]]);
        for (physical, (key, size)) in buffers {
            write(recording, &[&physical.to_le_bytes(), &key.to_le_bytes(), &size.to_le_bytes()]);
        }
        recording.kicks += 1;
    }

    // At the end of the run, perhaps on a fault: a recording a thread was in the middle of
    // writing is left as it is.
    pub fn finish() {
        let mut held = match RECORDING.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(held)) => held.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if let Some(mut recording) = held.take() {
            let _ = recording.file.flush();
            log_info!("xma: recorded %llu kicks, %u input buffers", recording.kicks, recording.written.len() as u32);
        }
    }
}

#[cfg(not(feature = "diagnostics"))]
mod record {
    use crate::guest::Memory;
    pub fn started(_: u32) {}
    pub fn allocated(_: usize) {}
    pub fn released(_: usize) {}
    pub fn cleared(_: Memory, _: usize, _: u32) {}
    pub fn kicked(_: Memory, _: usize, _: u32) {}
    pub fn finish() {}
}
