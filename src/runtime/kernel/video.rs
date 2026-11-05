// Vd* -- the kernel's video/GPU surface. These are the only calls between the title's
// statically linked D3D9 and the hardware, so they are where the command processor
// (gpu/, C++) attaches.

use super::allocate_physical;
use crate::guest::{Memory, Register, kernel_import};
use crate::runtime::{bridge, render_size};
use std::sync::atomic::{AtomicU32, Ordering};

const DISPLAY_WIDTH: u32 = 1280;
const DISPLAY_HEIGHT: u32 = 720;

/// The title's physical address of a virtual one, as it computes it inline around its Vd*
/// calls (runtime/kernel/physical.h): MmGetPhysicalAddress has to agree with it.
pub fn to_physical(virtual_address: u32) -> u32 {
    (virtual_address & 0x1FFF_FFFF).wrapping_add((virtual_address >> 20).wrapping_add(0x200) & 0x1000)
}

fn widescreen() -> bool {
    render_size::widescreen(render_size::frame())
}

// X_VIDEO_MODE: width, height, interlaced, widescreen, high definition, refresh rate (a
// float), the standard, then two words the dashboard sets and three reserved.
fn fill_video_mode(memory: Memory, mode: u32) {
    if mode == 0 {
        return;
    }
    let words = [
        DISPLAY_WIDTH,
        DISPLAY_HEIGHT,
        0,
        u32::from(widescreen()),
        1,
        60.0f32.to_bits(),
        1, // NTSC
        0x8A,
        0x01,
        0,
        0,
        0,
    ];
    for (i, word) in (0u32..).zip(words) {
        memory.store_u32(mode + i * 4, word);
    }
}

// VdInitializeRingBuffer(physicalBase, log2(sizeBytes) - 3)
kernel_import!(VdInitializeRingBuffer, |ctx, _memory| {
    bridge::gpu_set_ring_buffer(ctx.r3.as_u32(), ctx.r4.as_u32());
    ctx.r3.u64 = 0;
});

kernel_import!(VdEnableRingBufferRPtrWriteBack, |ctx, _memory| {
    bridge::gpu_set_read_pointer_write_back(ctx.r3.as_u32(), ctx.r4.as_u32());
    ctx.r3.u64 = 0;
});

kernel_import!(VdSetGraphicsInterruptCallback, |ctx, _memory| {
    bridge::gpu_set_interrupt_callback(ctx.r3.as_u32(), ctx.r4.as_u32());
    ctx.r3.u64 = 0;
});

kernel_import!(VdInitializeEngines, |ctx, _memory| {
    ctx.r3.u64 = 1;
});

kernel_import!(VdShutdownEngines, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdQueryVideoMode, |ctx, memory| {
    fill_video_mode(memory, ctx.r3.as_u32());
    ctx.r3.u64 = 0;
});

// XGetVideoMode is XAM's view of the same structure.
kernel_import!(XGetVideoMode, |ctx, memory| {
    fill_video_mode(memory, ctx.r3.as_u32());
    ctx.r3.u64 = 0;
});

kernel_import!(VdQueryVideoFlags, |ctx, _memory| {
    // Widescreen | HiDef; the title only tests individual bits.
    ctx.r3.u64 = if widescreen() { 3 } else { 2 };
});

kernel_import!(VdGetCurrentDisplayInformation, |ctx, memory| {
    // The first fields overlap XVIDEO_MODE; the rest is front-buffer geometry the title
    // does not read before a mode is set.
    let information = ctx.r3.as_u32();
    if information != 0 {
        memory.zero(information, 0x60);
        fill_video_mode(memory, information);
    }
    ctx.r3.u64 = 0;
});

kernel_import!(VdGetCurrentDisplayGamma, |ctx, memory| {
    if ctx.r3.as_u32() != 0 {
        memory.store_u32(ctx.r3.as_u32(), 2);
    }
    if ctx.r4.as_u32() != 0 {
        memory.store_u32(ctx.r4.as_u32(), 2.222_222_3f32.to_bits());
    }
    ctx.r3.u64 = 0;
});

kernel_import!(VdSetDisplayMode, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// VdSwap -- the title asks the kernel to build a present command list.
//
// The first argument is 64 dwords D3D9 reserved in its own command stream for the
// kernel's commands, between the frame's last resolve and the flip handshake. Left
// unwritten they hold whatever the arena held on its previous lap: zeros at first, then
// stale packets once it wraps, which the command processor walks straight into -- it
// swallowed the handshake and the title stopped a second into a level. Filled as Xenia
// fills them: a swap packet naming the front buffer, then type-2 no-ops. Xenia also
// writes the front buffer's fetch constant to slot 0 first; not here, where the renderer
// would take it for a texture binding.
kernel_import!(VdSwap, |ctx, memory| {
    const RESERVED_DWORDS: u32 = 64;
    const SWAP_SIGNATURE: u32 = 0x5357_4150; // 'SWAP'
    const XE_SWAP_PACKET: u32 = 0xC000_0000 | (3 << 16) | (0x64 << 8);
    const TYPE2_NOP: u32 = 0x8000_0000;
    let dwords = ctx.r3.as_u32();
    if dwords != 0 {
        // The fetch constant D3D9 keeps in the front buffer's texture header: base
        // address in dword 1, size in dword 2.
        let fetch = ctx.r4.as_u32();
        let front_buffer = if ctx.r8.as_u32() != 0 { memory.u32_at(ctx.r8.as_u32()) } else { 0 };
        let size = if fetch != 0 { memory.u32_at(fetch + 8) } else { 0 };
        let packet = [
            XE_SWAP_PACKET,
            SWAP_SIGNATURE,
            if front_buffer != 0 { to_physical(front_buffer) } else { 0 },
            if fetch != 0 { (size & 0x1FFF) + 1 } else { DISPLAY_WIDTH },
            if fetch != 0 { ((size >> 13) & 0x1FFF) + 1 } else { DISPLAY_HEIGHT },
        ];
        for i in 0..RESERVED_DWORDS {
            memory.store_u32(dwords + i * 4, packet.get(i as usize).copied().unwrap_or(TYPE2_NOP));
        }
    }
    bridge::gpu_swap();
    ctx.r3.u64 = 0;
});

// A scratch buffer the title may append to; it is never submitted.
static SYSTEM_COMMAND_BUFFER: AtomicU32 = AtomicU32::new(0);

kernel_import!(VdGetSystemCommandBuffer, |ctx, memory| {
    let mut buffer = SYSTEM_COMMAND_BUFFER.load(Ordering::Relaxed);
    if buffer == 0 {
        buffer = allocate_physical(64 * 1024, 0x1000);
        SYSTEM_COMMAND_BUFFER.store(buffer, Ordering::Relaxed);
    }
    if ctx.r3.as_u32() != 0 {
        memory.store_u32(ctx.r3.as_u32(), buffer);
    }
    if ctx.r4.as_u32() != 0 {
        memory.store_u32(ctx.r4.as_u32(), 0);
    }
    ctx.r3.u64 = 0;
});

kernel_import!(VdSetSystemCommandBufferGpuIdentifierAddress, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdCallGraphicsNotificationRoutines, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdIsHSIOTrainingSucceeded, |ctx, _memory| {
    ctx.r3.u64 = 1;
});

kernel_import!(VdPersistDisplay, |ctx, _memory| {
    ctx.r3.u64 = 1;
});

kernel_import!(VdEnableDisableClockGating, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdRetrainEDRAM, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdRetrainEDRAMWorker, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(VdInitializeScalerCommandBuffer, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// This has to match the masking sequence the title also inlines, or the two disagree.
kernel_import!(MmGetPhysicalAddress, |ctx, _memory| {
    ctx.r3.u64 = u64::from(to_physical(ctx.r3.as_u32()));
});
