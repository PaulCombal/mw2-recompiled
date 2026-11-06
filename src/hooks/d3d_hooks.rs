// MW2's D3D9 is statically linked but not inlined: the engine reaches it through
// ordinary function calls, and a few of them are hooked here (runtime/gpu/d3d_hooks.cpp;
// the device's fields are in docs/d3d9-seam.md).

use crate::guest::{Memory, Register, guest_hook};
use crate::log::log_info;
use crate::runtime::gpu::{arena, gpu};
use crate::runtime::{console, frame_rate};
use crate::title::*;
use std::ffi::CString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

// D3D9's device structure.
const PRESENT_INTERVAL: u32 = 13596;
const PROGRESS_BLOCK: u32 = 10896;
const INTERVAL_IMMEDIATE: u32 = 0x8000_0000;

// MW2_FPS_LIMIT (frame_rate.h), read once as the C++ reads it.
fn limit() -> u32 {
    static LIMIT: OnceLock<u32> = OnceLock::new();
    *LIMIT.get_or_init(frame_rate::limit)
}

// Frames are retired at the blank, one a blank, as on the console.
fn console_rate() -> bool {
    limit() == 60
}

#[cfg(feature = "diagnostics")]
fn note(event: u8) {
    use crate::runtime::{clock, pacing_trace, platform};
    if pacing_trace::on() {
        pacing_trace::note(clock::nanoseconds(), event, 0, platform::thread_id());
    }
}

#[cfg(not(feature = "diagnostics"))]
fn note(_event: u8) {}

// MW2_FPS_LIMIT takes the title off the display's blank with what the console has for
// it. D3D's present puts the device's D3DRS_PRESENTINTERVAL in the flip request, and its
// flip handler retires a frame asked for with D3DPRESENT_INTERVAL_IMMEDIATE when the
// command processor reaches it instead of queueing it for the blank. The title never sets
// that state, but a state block put back restores it, so it is set at every present. The
// title's side is two settings of its own: com_maxfps, its limiter, and r_vsync, which has
// it take a frame's length from the blanks counted rather than from the clock.
fn unsynchronise(memory: Memory, device: u32) {
    memory.store_u32(device.wrapping_add(PRESENT_INTERVAL), INTERVAL_IMMEDIATE);

    static TOLD: AtomicBool = AtomicBool::new(false);
    if !TOLD.swap(true, Ordering::Relaxed) {
        let limit = limit();
        let command = CString::new(format!("com_maxfps {limit}")).unwrap_or_default();
        console::run_now(&command);
        console::run_now(c"r_vsync 0");
        if limit != 0 {
            log_info!("d3d: frames are retired off the blank, %u a second at most", limit);
        } else {
            log_info!("d3d: frames are retired off the blank, as many a second as there are");
        }
    }
}

// D3D9's present. Its frame is retired the way the console retires it: the command
// stream has the command processor raise an interrupt whose handler runs D3D's own flip
// handler, which counts the frame done.
guest_hook!(T_D3D_Present, |ctx, memory| {
    note(b'P');
    if !console_rate() {
        unsynchronise(memory, ctx.r3.as_u32());
    }
    T_D3D_Present.original(ctx, memory);
    note(b'p');
    gpu::presented();
    #[cfg(feature = "diagnostics")]
    {
        use crate::runtime::{clock, pacing_trace, stutters};
        if stutters::on() {
            stutters::title_presented(pacing_trace::microseconds(clock::nanoseconds()));
        }
    }
});

// D3D9's wait for arena space, which is where the block the command processor reports its
// progress through is learnt (arena.rs): the device holds a pointer to it, and it holds a
// sequence counter at +0 and a position at +4.
guest_hook!(T_D3D_ArenaWait, |ctx, memory| {
    let block = memory.u32_at(ctx.r3.as_u32().wrapping_add(PROGRESS_BLOCK));
    if block != 0 {
        arena::set_arena_progress_block(block);
    }
    T_D3D_ArenaWait.original(ctx, memory);
});

#[cfg(all(feature = "diagnostics", T_D3D_ReplayRecording))]
mod recordings {
    use super::*;
    use crate::guest::PPCContext;
    use crate::log::log_warn;
    use std::sync::atomic::AtomicU64;

    pub static ENDLESS: AtomicU64 = AtomicU64::new(0);

    // A renderer pass' recording is replayed by T_D3D_ReplayRecording, which walks the
    // chunk list at [object+116] -- next at +0, an entry count at +4, entries from +8 --
    // and copies each chunk into the command-buffer arena. The inner loop counts up to
    // [chunk+4] with a do-while, so a chunk holding no entries is 2^32 of them and one
    // whose count is not a count is as good as forever. The thread does not come back to
    // be diagnosed, so the list is checked on the way in.
    fn check(memory: Memory, ctx: &PPCContext) {
        let object = ctx.r4.as_u32();
        let head = if object != 0 { memory.u32_at(object.wrapping_add(116)) } else { 0 };
        let (mut nodes, mut entries) = (0u32, 0u32);
        let mut endless = false;
        // Tortoise and hare: a cycle is as endless as a bad count.
        let (mut slow, mut fast) = (head, head);
        while fast != 0 && !endless {
            let count = memory.u32_at(slow.wrapping_add(4));
            if count == 0 || count > 1 << 20 {
                endless = true;
                break;
            }
            entries = entries.wrapping_add(count);
            fast = memory.u32_at(fast);
            if fast != 0 {
                fast = memory.u32_at(fast);
            }
            slow = memory.u32_at(slow);
            if slow == 0 {
                break;
            }
            if slow == fast || nodes > 4096 {
                endless = true;
            }
            nodes += 1;
        }
        if endless && ENDLESS.fetch_add(1, Ordering::SeqCst) < 8 {
            log_warn!(
                "d3d9: a recorded command buffer D3D9 cannot walk to the end of (object %08X, head %08X, %u chunks, %u entries)",
                object,
                head,
                nodes,
                entries
            );
        }
    }

    guest_hook!(T_D3D_ReplayRecording, |ctx, memory| {
        check(memory, ctx);
        T_D3D_ReplayRecording.original(ctx, memory);
    });
}

/// Recordings D3D9 could not have walked to the end of, for gpu.rs's report.
#[cfg(feature = "diagnostics")]
pub fn report() {
    #[cfg(T_D3D_ReplayRecording)]
    {
        let endless = recordings::ENDLESS.load(Ordering::SeqCst);
        if endless != 0 {
            log_info!("d3d9: %llu recordings with no end", endless);
        }
    }
}
