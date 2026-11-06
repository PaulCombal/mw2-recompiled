// D3D9's command-buffer arena, from the command processor's side (runtime/gpu/arena.cpp).
//
// D3D9's arena is a ring of its own, and the producer will not overwrite a byte until
// the command processor reports it read past -- a pair of EVENT_WRITE_SHDs closing each
// segment: position, then sequence. Missing one report stops the title for good, with an
// empty ring.
//
// The state is the consumer's: it alone reports progress (and MW2_REPLAY_GPU puts it
// back before the consumer runs). The block is learnt on a guest thread, so it is an
// atomic; the rest are atomics only to be statics, each loaded and stored by the one
// thread.

use crate::log::log_info;
use crate::runtime::bridge::gpu as calls;
use crate::runtime::kernel;
#[cfg(feature = "diagnostics")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

// Physical.
static BLOCK: AtomicU32 = AtomicU32::new(0);
static PENDING_POSITION: AtomicU32 = AtomicU32::new(0);
static HAVE_POSITION: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU32 = AtomicU32::new(0);
// For the report.
#[cfg(feature = "diagnostics")]
static APPLIED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "diagnostics")]
static STALE_SEQUENCES: AtomicU64 = AtomicU64::new(0);

// The sequence advances by two per segment, and signed arithmetic keeps the comparison
// right across a wrap. A report claiming thousands ahead would poison the counter for
// good -- every genuine one after it would read as stale -- so it is dropped too.
const MAX_ADVANCE: i32 = 1024;

/// An EVENT_WRITE_SHD aimed at the arena's progress block: true when it was one, applied
/// or not.
///
/// A segment is allocated at a fixed size but filled only as far as the frame needs, so
/// a stale trailer from its previous use can sit before the real one. Applying it would
/// move the position backwards and deadlock the title with an empty ring. The sequence
/// only moves forward, so it settles which is which.
pub fn arena_progress(address: u32, value: u32) -> bool {
    let block = BLOCK.load(Relaxed);
    #[cfg(feature = "diagnostics")]
    calls::stream_arena_block_seen(block);
    if block == 0 {
        return false;
    }
    let target = address & !3;
    if target == block.wrapping_add(4) {
        PENDING_POSITION.store(value, Relaxed);
        HAVE_POSITION.store(true, Relaxed);
        return true;
    }
    if target != block {
        return false;
    }

    let sequence = SEQUENCE.load(Relaxed);
    let advance = value.wrapping_sub(sequence) as i32;
    if sequence != 0 && (advance <= 0 || advance > MAX_ADVANCE) {
        #[cfg(feature = "diagnostics")]
        STALE_SEQUENCES.store(STALE_SEQUENCES.load(Relaxed) + 1, Relaxed);
        HAVE_POSITION.store(false, Relaxed);
        return true;
    }
    SEQUENCE.store(value, Relaxed);
    #[cfg(feature = "diagnostics")]
    APPLIED.store(APPLIED.load(Relaxed) + 1, Relaxed);
    if HAVE_POSITION.load(Relaxed) {
        calls::write_physical(block.wrapping_add(4), PENDING_POSITION.load(Relaxed));
        HAVE_POSITION.store(false, Relaxed);
    }
    calls::write_physical(block, value);
    true
}

/// The block D3D9 polls for the command processor's progress through its command-buffer
/// arena: a position at +4 and a sequence counter at +0.
pub fn set_arena_progress_block(address: u32) {
    let physical = kernel::to_physical(address);
    if BLOCK.swap(physical, Relaxed) != physical {
        log_info!("gpu: arena progress block at %08X (physical %08X)", address, physical);
    }
}

/// The state MW2_CAPTURE_GPU stores and MW2_REPLAY_GPU puts back (gpu/internal.h's
/// ArenaState).
#[cfg(feature = "diagnostics")]
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ArenaState {
    pub block: u32,
    pub sequence: u32,
    pub pending: u32,
    pub have_position: u32,
}

#[cfg(feature = "diagnostics")]
pub fn save() -> ArenaState {
    ArenaState {
        block: BLOCK.load(Relaxed),
        sequence: SEQUENCE.load(Relaxed),
        pending: PENDING_POSITION.load(Relaxed),
        have_position: u32::from(HAVE_POSITION.load(Relaxed)),
    }
}

#[cfg(feature = "diagnostics")]
pub fn restore(state: &ArenaState) {
    BLOCK.store(state.block, Relaxed);
    SEQUENCE.store(state.sequence, Relaxed);
    PENDING_POSITION.store(state.pending, Relaxed);
    HAVE_POSITION.store(state.have_position != 0, Relaxed);
}

#[cfg(feature = "diagnostics")]
pub fn set_block(physical: u32) {
    BLOCK.store(physical, Relaxed);
}

#[cfg(feature = "diagnostics")]
pub fn report() {
    log_info!(
        "gpu: arena progress: %llu reports applied, %llu stale ones dropped",
        APPLIED.load(Relaxed),
        STALE_SEQUENCES.load(Relaxed)
    );
}
