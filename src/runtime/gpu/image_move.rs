// Moving a streamed image's pixels (runtime/image_move.cpp; the hook is
// src/hooks/image_move.rs).
//
// MW2 keeps its streamed images in a compacting pool. When the pool relocates a block,
// sub_823A5CB8 allocates the new home, asks sub_823A5780 to move the bytes and then
// points the image record at the new address. The move is not a memcpy: it is queued
// and flushed through T_Image_FlushMove, which builds a surface header over the two
// blocks and has the GPU do the copy -- the 360's fast path for shifting several
// megabytes of texture memory.
//
// The copy is one draw: a vertex shader that fetches the source and memory-exports it
// to the destination, with the export stream constant in c0.x -- 0x40000000 | the
// destination's physical address in dwords. Our shaders drop memory exports, so the
// copy is made on the CPU instead, but at that draw: every draw the title queued before
// it still sees the old bytes, every one after it the new. Made when the pool asks, it
// ran up to two frames ahead of draws still naming the block, and they drew whatever the
// pool laid on it next. The pool says what to copy; the draw says when.
//
// The queue is taken only while copies wait (COPIES_WAITING, which the command
// processor reads at every draw); the statistics are single-writer atomics. The copy
// itself moves guest memory with ptr::copy, never through a slice.

use crate::log::format_text;
use crate::runtime::bridge::{renderer, watch};
use crate::runtime::kernel;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

#[cfg(feature = "diagnostics")]
use crate::log::log_info;
#[cfg(feature = "diagnostics")]
use crate::runtime::bridge::stream;

#[derive(Clone, Copy)]
struct PendingCopy {
    destination: u32, // guest virtual
    source: u32,
    size: u32,
    stream_constant: u32, // c0.x of the copy draw
    frame: u64,           // the guest frame asked in
    // MW2_CAPTURE_GPU has it, at the draw the consumer first saw it.
    #[cfg(feature = "diagnostics")]
    journalled: bool,
}

struct Queue {
    pending: Vec<PendingCopy>,
    // The copy draw's vertex shader, once seen.
    copy_shader: u64,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { pending: Vec::new(), copy_shader: 0 });
/// How many copies wait: read at every draw, without the lock.
static COPIES_WAITING: AtomicU32 = AtomicU32::new(0);

// What the pool has moved lately, for the flash hunt's log lines (diagnostics only).
#[derive(Clone, Copy)]
struct Move {
    destination: u32, // guest virtual
    source: u32,
    size: u32,
    frame: u64,
    sequence: u64,
}
const MOVES: usize = 1024;
// Recorded in a diagnostics build only; a release describes no move.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
struct Moves {
    list: [Move; MOVES],
    count: u64,
}
static MOVES_SEEN: Mutex<Moves> =
    Mutex::new(Moves { list: [Move { destination: 0, source: 0, size: 0, frame: 0, sequence: 0 }; MOVES], count: 0 });

static ASKED: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
// The GPU thread's.
static AT_DRAW: AtomicU64 = AtomicU64::new(0);
static AT_FRAME_END: AtomicU64 = AtomicU64::new(0);
static OTHER_SHADER: AtomicU64 = AtomicU64::new(0);
static IMMEDIATE: AtomicU64 = AtomicU64::new(0);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn bump(counter: &AtomicU64, n: u64) {
    counter.store(counter.load(Relaxed).wrapping_add(n), Relaxed);
}

// What the image pool asks of the GPU's threads: the rig (tools/gpu_diff.py) answers
// these itself, as gpu.rs's hook-rig values do.
fn frame_count() -> u64 {
    super::gpu::frame_count()
}

// `in_stream` when the copy is made where the command stream makes it, on the thread
// that parses the stream: the renderer then orders it against the draws before it.
fn copy(destination: u32, source: u32, size: u32, in_stream: bool) {
    if in_stream {
        renderer::before_stream_write(kernel::to_physical(destination), size);
    }
    #[cfg(feature = "diagnostics")]
    if in_stream {
        stream::note_read(source, u64::from(size));
        stream::note_write(destination, size);
    }
    // The pool never asks for a move that overlaps; memmove says so for free.
    #[cfg(all(feature = "diagnostics", not(windows)))]
    crate::runtime::watchpoint::suspend();
    watch::will_write(destination, size);
    let base = crate::runtime::guest_memory::base();
    // SAFETY: two blocks of the pool in guest memory, which is mapped whole; ptr::copy
    // is memmove.
    unsafe {
        std::ptr::copy(base.wrapping_add(source as usize), base.wrapping_add(destination as usize), size as usize)
    };
    #[cfg(all(feature = "diagnostics", not(windows)))]
    crate::runtime::watchpoint::resume();
    #[cfg(feature = "diagnostics")]
    {
        if in_stream {
            stream::note_written(destination, size);
        }
        let mut moves = lock(&MOVES_SEEN);
        moves.count += 1;
        let count = moves.count;
        moves.list[(count % MOVES as u64) as usize] =
            Move { destination, source, size, frame: frame_count(), sequence: count };
    }
    renderer::textures_memory_written(kernel::to_physical(destination), size);
}

/// T_Image_FlushMove, before the title's own call writes the copy draw: queued, so the
/// parse never meets the draw before it knows what the draw copies.
pub fn queue(destination: u32, source: u32, size: u32, stream_constant: u32, frame: u64) {
    let mut queue = lock(&QUEUE);
    queue.pending.push(PendingCopy {
        destination,
        source,
        size,
        stream_constant,
        frame,
        #[cfg(feature = "diagnostics")]
        journalled: false,
    });
    COPIES_WAITING.store(queue.pending.len() as u32, Relaxed);
}

/// After the title's call, for every move it asked for.
pub fn asked(size: u32) {
    ASKED.fetch_add(1, Relaxed);
    BYTES.fetch_add(u64::from(size), Relaxed);
}

/// Nothing is consuming the command stream to order the copy against: made now.
pub fn copy_now(destination: u32, source: u32, size: u32) {
    IMMEDIATE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    copy(destination, source, size, false);
}

/// MW2_REPLAY_GPU: a move the capture saw the title queue, queued as its hook would.
#[cfg(feature = "diagnostics")]
pub fn queue_for_replay(destination: u32, source: u32, size: u32, stream_constant: u32, frame: u64) {
    asked(size);
    queue(destination, source, size, stream_constant, frame);
}

// MW2_CAPTURE_GPU: what the consumer finds in the queue that it has not seen before, at
// the packet it looks. Under the queue's lock.
#[cfg(feature = "diagnostics")]
fn journal_pending(queue: &mut Queue) {
    if !stream::capturing() {
        return;
    }
    for c in queue.pending.iter_mut().filter(|c| !c.journalled) {
        c.journalled = true;
        stream::pool_seen(c.destination, c.source, c.size, c.stream_constant, c.frame);
    }
}

/// For the command processor, at every draw: whether to call at_draw.
#[inline(always)]
pub fn copies_waiting() -> bool {
    COPIES_WAITING.load(Relaxed) != 0
}

/// At every draw while copies_waiting(): the draw's c0.x (the memory-export stream
/// constant) and vertex shader. When they name a copy the pool asked for, the copy is
/// made now -- the point in the stream where the console's GPU makes it.
#[inline(never)]
pub fn at_draw(stream_constant: u32, vertex_shader: u64) {
    if stream_constant & 0xC000_0000 != 0x4000_0000 {
        return;
    }
    let copy_wanted = {
        let mut queue = lock(&QUEUE);
        #[cfg(feature = "diagnostics")]
        journal_pending(&mut queue);
        let Some(at) = queue.pending.iter().position(|c| c.stream_constant == stream_constant) else { return };
        // c0 outlives the draw that set it. Once the copy shader is known, an old stream
        // constant under some other draw is not a copy.
        if queue.copy_shader != 0 && vertex_shader != queue.copy_shader {
            bump(&OTHER_SHADER, 1);
            return;
        }
        queue.copy_shader = vertex_shader;
        let wanted = queue.pending.remove(at);
        COPIES_WAITING.store(queue.pending.len() as u32, Relaxed);
        wanted
    };
    copy(copy_wanted.destination, copy_wanted.source, copy_wanted.size, true);
    bump(&AT_DRAW, 1);
}

/// At each swap packet the parse meets; `parsed_frames` counts them. A copy whose frame
/// the parse has left behind without meeting its draw is made anyway, so the pool's
/// blocks never go uncopied.
#[inline(never)]
pub fn at_frame_end(parsed_frames: u64) {
    let overdue: Vec<PendingCopy> = {
        let mut queue = lock(&QUEUE);
        #[cfg(feature = "diagnostics")]
        journal_pending(&mut queue);
        // std::stable_partition: those still in time keep their order, then the late
        // ones in theirs.
        let (keep, late): (Vec<PendingCopy>, Vec<PendingCopy>) =
            queue.pending.iter().partition(|c| parsed_frames <= c.frame.wrapping_add(1));
        queue.pending = keep;
        COPIES_WAITING.store(queue.pending.len() as u32, Relaxed);
        late
    };
    for c in &overdue {
        copy(c.destination, c.source, c.size, true);
    }
    bump(&AT_FRAME_END, overdue.len() as u64);
}

// snprintf into `out` at `at`: as much as fits, always ended, and the length it wanted.
fn append(out: &mut [u8], at: usize, text: &[u8]) -> usize {
    if at < out.len() {
        let room = out.len() - at - 1;
        let n = text.len().min(room);
        out[at..at + n].copy_from_slice(&text[..n]);
        out[at + n] = 0;
    }
    text.len()
}

/// Every copy in the last `frames` frames with either end on this physical range,
/// oldest first, for the log line of a texture that reached the screen empty.
pub fn describe_moves_around(physical_address: u32, size: u32, frames: u64, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    out[0] = 0;
    let address = kernel::from_physical(physical_address);
    if address == 0 || size == 0 {
        return;
    }
    let now = frame_count();
    let since = now.saturating_sub(frames);
    let touches = |m: &Move| {
        let into = m.destination.wrapping_add(m.size) > address && m.destination < address.wrapping_add(size);
        let out_of = m.source.wrapping_add(m.size) > address && m.source < address.wrapping_add(size);
        (into, out_of)
    };
    let mut near: Vec<Move> = {
        let moves = lock(&MOVES_SEEN);
        moves
            .list
            .iter()
            .filter(|m| m.size != 0 && m.frame >= since)
            .filter(|m| {
                let (into, out_of) = touches(m);
                into || out_of
            })
            .copied()
            .collect()
    };
    near.sort_by_key(|m| m.sequence);
    let mut at = 0usize;
    for m in &near {
        if at + 80 >= out.len() {
            break;
        }
        let (into, _) = touches(m);
        let other = if into { m.source } else { m.destination };
        let text = format_text!(
            "%s#%llu f%llu %s %08X..%08X",
            if at != 0 { c", " } else { c"" },
            m.sequence,
            m.frame,
            if into { c"IN from" } else { c"OUT to" },
            other,
            other.wrapping_add(m.size)
        );
        at += append(out, at, text.as_bytes());
    }
    if near.is_empty() {
        let text = format_text!("no copy touched it in %llu frames", frames);
        append(out, 0, text.as_bytes());
    }
}

#[cfg(feature = "diagnostics")]
pub fn report() {
    let asked = ASKED.load(Relaxed);
    if asked == 0 {
        return;
    }
    log_info!(
        "image pool: %llu blocks moved, %llu MB; %llu copied at their own draw, %llu at the end of their frame because no draw named them, %llu with nothing consuming the stream; %llu times an old stream constant sat under another shader",
        asked,
        BYTES.load(Relaxed) >> 20,
        AT_DRAW.load(Relaxed),
        AT_FRAME_END.load(Relaxed),
        IMMEDIATE.load(Relaxed),
        OTHER_SHADER.load(Relaxed)
    );
}
