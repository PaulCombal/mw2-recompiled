// The C interface the GPU capture (stream_capture.cpp, diagnostics) calls: each function
// is a C++ declaration's (gpu/internal.h, memory_watch.h, image_move.h ...), forwarded by
// runtime/rust_calls.cpp, and converts between C and the module's types and nothing else.
//
// Unsafe by nature: every pointer comes from the C++ forwarders, which pass out-structs
// on their stack whose layouts both sides assert and a callback that resizes a
// std::vector the forwarder owns and returns its zeroed bytes, valid for the call.
#![deny(unsafe_op_in_unsafe_fn)]

// What the capture calls of the GPU and the arena (gpu/gpu.rs, gpu/arena.rs).
// Out-pointers are the forwarders' references.
#[cfg(feature = "diagnostics")]
mod gpu {
    use crate::runtime::gpu::gpu as g;

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_gpu_ring_position(read: *mut u32, written: *mut u32, write_back: *mut u32) {
        let position = g::ring_position();
        // SAFETY: the forwarder's three references.
        unsafe {
            read.write(position.0);
            written.write(position.1);
            write_back.write(position.2);
        }
    }

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_gpu_ring_layout(base: *mut u32, dwords: *mut u32, write_back: *mut u32) {
        let layout = g::ring_layout();
        // SAFETY: the forwarder's three references.
        unsafe {
            base.write(layout.0);
            dwords.write(layout.1);
            write_back.write(layout.2);
        }
    }
}

#[cfg(feature = "diagnostics")]
mod arena {
    use crate::runtime::gpu::arena as a;

    // gpu::detail::ArenaState, the same four words (asserted in the forwarders).
    #[cfg(feature = "diagnostics")]
    const _: () = assert!(size_of::<a::ArenaState>() == 16);

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_arena_save(out: *mut a::ArenaState) {
        // SAFETY: the forwarder's ArenaState.
        unsafe { out.write(a::save()) }
    }

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    extern "C" fn mw2_arena_set_block(physical: u32) {
        a::set_block(physical);
    }
}

// What the C++ calls of the command processor and the image pool (gpu/command_processor.rs,
// gpu/image_move.rs): the GPU capture's state, swap count and replayed moves
// (stream_capture.cpp), and the replay driver's restore.
#[cfg(feature = "diagnostics")]
mod command_processor {
    use crate::runtime::gpu::command_processor as cp;

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    extern "C" fn mw2_cp_swaps_parsed() -> u64 {
        cp::swaps_parsed()
    }

    #[cfg(feature = "diagnostics")]
    type Reserve = unsafe extern "C" fn(context: *mut std::ffi::c_void, size: usize) -> *mut u8;

    /// state.bin's part: the bytes go where `reserve(size)` puts them (the forwarder's
    /// std::vector, resized).
    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_cp_save(reserve: Reserve, context: *mut std::ffi::c_void) {
        let bytes = cp::save();
        // SAFETY: reserve returns `size` writable bytes of the forwarder's vector.
        unsafe {
            let to = reserve(context, bytes.len());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), to, bytes.len());
        }
    }

    /// The replay driver's step 4 (bridge::replay::restore).
    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_cp_restore_for_replay(bytes: *const u8, size: u64) -> bool {
        if bytes.is_null() {
            return false;
        }
        // SAFETY: the driver's state.bin, `size` bytes, read for the call.
        cp::restore_for_replay(unsafe { std::slice::from_raw_parts(bytes, size as usize) })
    }
}

#[cfg(feature = "diagnostics")]
mod image_move {
    use crate::runtime::gpu::image_move as pool;

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    extern "C" fn mw2_imagepool_queue_for_replay(
        destination: u32,
        source: u32,
        size: u32,
        stream_constant: u32,
        frame: u64,
    ) {
        pool::queue_for_replay(destination, source, size, stream_constant, frame);
    }
}

// What the capture calls of the memory watch (gpu/memory_watch.rs): Loosened
// (stream_capture.cpp).
#[cfg(feature = "diagnostics")]
mod memory_watch {
    use crate::runtime::gpu::memory_watch as watch;

    #[cfg(feature = "diagnostics")]
    #[unsafe(no_mangle)]
    extern "C" fn mw2_memory_watch_loosened(page: u32) -> u32 {
        watch::loosened(page)
    }
}
