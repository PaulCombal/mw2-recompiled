// What the runtime asks of the C++ the game links (runtime/rust_calls.cpp), and the
// calls between the runtime's own parts that pass through here.

use std::ffi::{CStr, c_char};

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct WatchCosts {
    pub faults: u64,
    pub protects: u64,
    pub protect_nanoseconds: u64,
}

unsafe extern "C" {
    // src/runtime/run.rs's, by the names crash.h declares them under (exports.rs).
    fn mw2_crash_on_fault();
    fn mw2_crash_request_exit(why: *const c_char);
}

/// The memory watch's faults and protection changes so far (gpu/memory_watch.rs).
pub fn watch_costs() -> WatchCosts {
    crate::runtime::gpu::memory_watch::costs()
}

#[cfg(all(feature = "diagnostics", not(windows)))]
pub fn gpu_frame_count() -> u64 {
    crate::runtime::gpu::gpu::frame_count()
}

/// The end-of-run reports, for a run ending on a fault (run.rs).
pub fn on_fault() {
    unsafe { mw2_crash_on_fault() }
}

/// Ends the run tidily, from any thread (run.rs).
pub fn request_exit(why: &CStr) {
    unsafe { mw2_crash_request_exit(why.as_ptr()) }
}

/// The command ring the title writes, from VdInitializeRingBuffer.
pub fn gpu_set_ring_buffer(base_physical: u32, size_log2: u32) {
    crate::runtime::gpu::gpu::set_ring_buffer(base_physical, size_log2);
}

/// Where the GPU reports how far it has read.
pub fn gpu_set_read_pointer_write_back(address_physical: u32, block_size_log2: u32) {
    crate::runtime::gpu::gpu::set_read_pointer_write_back(address_physical, block_size_log2);
}

/// The title's routine for the GPU's interrupts.
pub fn gpu_set_interrupt_callback(callback: u32, context: u32) {
    crate::runtime::gpu::gpu::set_interrupt_callback(callback, context);
}

/// VdSwap: the frame is handed over.
pub fn gpu_swap() {
    crate::runtime::gpu::gpu::swap();
}

/// Somebody saw a flash; `how` names the key.
#[cfg(feature = "diagnostics")]
pub fn flash_seen(how: &CStr) {
    crate::runtime::gpu::vulkan::renderer_diagnostics::flash_seen(how);
}

// What gpu/gpu.rs and gpu/arena.rs call of the command processor, the census and the image
// pool -- gpu/command_processor.rs's, census.rs's and image_move.rs's, the consumer's renderer
// passed down -- and of the C++ (runtime/rust_calls.cpp): the GPU capture's batch brackets
// and report.
pub mod gpu {
    use super::renderer::Renderer;
    use crate::runtime::gpu::command_processor as cp;

    /// Executes `dwords` of the ring starting at `read_index`.
    #[inline]
    pub fn execute_ring(rd: &mut Renderer, base: u32, read_index: u32, mask: u32, dwords: u32) {
        cp::execute_ring(rd, base, read_index, mask, dwords);
    }

    /// Writes the records of the occlusion queries the GPU has finished counting.
    #[inline]
    pub fn deliver_occlusion_queries(rd: &mut Renderer) {
        cp::deliver_occlusion_queries(rd);
    }

    /// False under MW2_RENDER=0: the stream is executed and counted, nothing drawn.
    pub fn rendering() -> bool {
        cp::rendering()
    }

    /// The command processor's write of a dword at a physical address.
    pub fn write_physical(rd: &mut Renderer, physical: u32, value: u32) {
        cp::write_physical(rd, physical, value);
    }

    #[cfg(feature = "diagnostics")]
    pub fn cp_report() {
        cp::report();
    }

    #[cfg(feature = "diagnostics")]
    pub fn census_report() {
        crate::runtime::gpu::census::report();
    }

    #[cfg(feature = "diagnostics")]
    pub fn imagepool_report() {
        crate::runtime::gpu::image_move::report();
    }

    #[cfg(feature = "diagnostics")]
    unsafe extern "C" {
        fn mw2_stream_batch_begin(base: u32, read_index: u32, mask: u32, dwords: u32);
        fn mw2_stream_batch_end();
        fn mw2_stream_shutdown();
        fn mw2_stream_report();
        fn mw2_stream_arena_block_seen(block: u32);
    }

    /// At each present the title makes.
    #[cfg(feature = "diagnostics")]
    pub fn census_present() {
        crate::runtime::gpu::census::present();
    }

    /// MW2_CAPTURE_GPU and MW2_REPLAY_GPU around each ring batch (stream_capture.h).
    #[cfg(feature = "diagnostics")]
    pub fn stream_batch_begin(base: u32, read_index: u32, mask: u32, dwords: u32) {
        unsafe { mw2_stream_batch_begin(base, read_index, mask, dwords) }
    }

    #[cfg(feature = "diagnostics")]
    pub fn stream_batch_end() {
        unsafe { mw2_stream_batch_end() }
    }

    /// The run ends: a capture still going is closed where it is.
    #[cfg(feature = "diagnostics")]
    pub fn stream_shutdown() {
        unsafe { mw2_stream_shutdown() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn stream_report() {
        unsafe { mw2_stream_report() }
    }

    /// The arena's progress block as the command processor finds it, for a capture.
    #[cfg(feature = "diagnostics")]
    pub fn stream_arena_block_seen(block: u32) {
        unsafe { mw2_stream_arena_block_seen(block) }
    }
}

// What the command processor, the image pool and the ring consumer ask of the renderer
// the consumer owns (src/runtime/gpu/vulkan/renderer.rs): direct calls, `rd` passed down
// beside the register file.
pub mod renderer {
    use crate::runtime::gpu::registers::RegisterFile;

    pub use crate::runtime::gpu::vulkan::renderer::Renderer;

    /// What the draw packet carries that the register file does not, as the command
    /// processor gathers it.
    #[repr(C)]
    pub struct DrawCall {
        pub primitive: u32,
        pub index_count: u32,
        pub indexed: bool,
        pub index32: bool,
        pub index_address: u32, // guest virtual, already resolved
        // A hash of each, so a shader is translated and compiled once.
        pub vertex_code: *const u32,
        pub vertex_words: usize,
        pub vertex_hash: u64,
        pub pixel_code: *const u32,
        pub pixel_words: usize,
        pub pixel_hash: u64,
    }

    const _: () = {
        assert!(std::mem::offset_of!(DrawCall, primitive) == 0);
        assert!(std::mem::offset_of!(DrawCall, index_count) == 4);
        assert!(std::mem::offset_of!(DrawCall, indexed) == 8);
        assert!(std::mem::offset_of!(DrawCall, index32) == 9);
        assert!(std::mem::offset_of!(DrawCall, index_address) == 12);
        assert!(std::mem::offset_of!(DrawCall, vertex_code) == 16);
        assert!(std::mem::offset_of!(DrawCall, vertex_words) == 24);
        assert!(std::mem::offset_of!(DrawCall, vertex_hash) == 32);
        assert!(std::mem::offset_of!(DrawCall, pixel_code) == 40);
        assert!(std::mem::offset_of!(DrawCall, pixel_words) == 48);
        assert!(std::mem::offset_of!(DrawCall, pixel_hash) == 56);
        assert!(std::mem::size_of::<DrawCall>() == 64);
    };

    /// The consumer's renderer, not yet started.
    pub fn new() -> Box<Renderer> {
        Box::new(Renderer::new())
    }

    /// vk::renderer::Initialise, on the run thread before the consumer starts: false when
    /// it cannot draw.
    pub fn initialise(rd: &mut Renderer) -> bool {
        // SAFETY: the run thread records until it hands the renderer to the consumer (a
        // spawn), and the renderer is boxed: neither it nor its device tables move.
        unsafe { rd.initialise() }
    }

    /// vk::renderer::Shutdown, once the consumer is joined.
    pub fn shutdown(rd: &mut Renderer) {
        // SAFETY: the consumer is joined, so this thread is the recorder's producer now.
        unsafe { rd.shutdown() }
    }

    /// vk::textures::Shutdown, after the renderer's: the device tables go with it, and the
    /// cache is kept for the report written after it.
    pub fn textures_shutdown(rd: &mut Renderer) {
        rd.shutdown_textures();
    }

    /// vk::renderer::Report: from what the shutdown kept and the counters in statics.
    #[cfg(feature = "diagnostics")]
    pub fn report() {
        crate::runtime::gpu::vulkan::renderer_diagnostics::report();
    }

    /// vk::textures::Report, from the cache the shutdown kept; nothing before it (the
    /// fault path, which reads statics alone).
    #[cfg(feature = "diagnostics")]
    pub fn textures_report() {
        crate::runtime::gpu::vulkan::texture_cache::report_after_shutdown();
    }

    /// What guest threads told the texture cache since the consumer's last batch: at the
    /// start of each.
    #[inline]
    pub fn apply_memory_written_elsewhere(rd: &mut Renderer) {
        rd.core.textures.apply_written_elsewhere();
    }

    // A stage's microcode as the renderer reads it: none for a null pointer.
    #[inline(always)]
    unsafe fn code<'a>(code: *const u32, words: usize) -> &'a [u32] {
        if code.is_null() {
            &[]
        } else {
            // SAFETY: the caller's: `words` dwords at `code`, the command processor's
            // own copy of the stage's shader, alive and unchanged through the draw.
            unsafe { std::slice::from_raw_parts(code, words) }
        }
    }

    /// A draw: the register file and the call, straight into the renderer.
    #[inline(always)]
    pub fn draw(rd: &mut Renderer, registers: &RegisterFile, call: &DrawCall) {
        // SAFETY: the shaders are the command processor's copies (code_of), which no
        // packet changes during a draw; the index buffer and the fetch constants' vertex
        // buffers are guest addresses the physical mapping gave, inside the reservation
        // (Renderer::draw's contract, the arena's).
        unsafe {
            let call = crate::runtime::gpu::vulkan::renderer_draw::DrawCall {
                primitive: call.primitive,
                index_count: call.index_count,
                indexed: call.indexed,
                index32: call.index32,
                index_address: call.index_address,
                vertex_code: code(call.vertex_code, call.vertex_words),
                vertex_hash: call.vertex_hash,
                pixel_code: code(call.pixel_code, call.pixel_words),
                pixel_hash: call.pixel_hash,
            };
            rd.draw(registers, &call)
        }
    }

    /// A draw issued in copy mode.
    pub fn resolve(rd: &mut Renderer, registers: &RegisterFile) {
        // SAFETY: the copy registers name guest memory inside the reservation.
        unsafe { rd.resolve(registers) }
    }

    /// The swap packet: the frame is what was resolved to this front buffer.
    pub fn swap(rd: &mut Renderer, front_buffer: u32) {
        rd.swap(front_buffer);
    }

    /// The command processor is about to tell the title the work before this point has
    /// finished.
    pub fn before_completion(rd: &mut Renderer) {
        rd.before_completion();
    }

    pub fn begin_occlusion_query(rd: &mut Renderer) {
        rd.begin_occlusion_query();
    }

    /// The count now, when there is nothing to count; None when it comes later.
    pub fn end_occlusion_query(rd: &mut Renderer, tag: u32) -> Option<u64> {
        let mut samples = 0u64;
        let now = rd.end_occlusion_query(tag, &mut samples);
        now.then_some(samples)
    }

    pub fn collect_occlusion_queries(rd: &mut Renderer, deliver: extern "C" fn(tag: u32, samples: u64)) {
        rd.collect_occlusion_queries(|tag, samples| deliver(tag, samples));
    }

    /// The ring consumer has nothing to read for now.
    pub fn idle(rd: &mut Renderer) {
        rd.idle();
    }

    /// The command stream is about to write [physical, physical + size).
    pub fn before_stream_write(rd: &mut Renderer, physical: u32, size: u32) {
        rd.before_stream_write(physical, size);
    }

    /// vk::textures::MemoryWritten, on the ring consumer.
    pub fn textures_memory_written(rd: &mut Renderer, physical: u32, size: u32) {
        rd.core.textures.memory_written(physical, size);
    }

    /// vk::textures::MemoryWritten from any other thread: queued for the consumer's next
    /// batch, so that no guest thread walks the cache while the consumer could be
    /// changing it.
    pub fn textures_memory_written_elsewhere(physical: u32, size: u32) {
        crate::runtime::gpu::vulkan::texture_cache::memory_written_elsewhere(physical, size);
    }

    /// vk::renderer::PrepareShader: a shader the title loaded, to compile before its first
    /// draw (hooks/shader_preload.rs). Any thread.
    pub fn prepare_shader(pixel: bool, code: &[u32]) {
        crate::runtime::gpu::vulkan::renderer::prepare_shader(pixel, code);
    }
}

// The memory watch: what the command processor's shader memo and the image pool ask of
// it, gpu/memory_watch.rs's.
pub mod watch {

    use crate::runtime::gpu::memory_watch as watch;

    #[inline]
    pub fn enabled() -> bool {
        watch::enabled()
    }

    /// Before reading the range: a stamp when every page of it is watched from then on.
    /// The renderer's last completed submission (a cached number) is taken first, whether
    /// or not a page has to be copied.
    pub fn track(physical: u32, bytes: u32, frame: u64) -> u64 {
        watch::track(physical, bytes, crate::runtime::gpu::vulkan::renderer_frame::completed_submission(), frame)
    }

    pub fn unwritten(physical: u32, bytes: u32, since: u64) -> bool {
        watch::unwritten(physical, bytes, since)
    }

    /// Before this runtime writes a large range of guest memory itself.
    pub fn will_write(address: u32, bytes: u32) {
        watch::will_write(address, bytes);
    }
}

// MW2_CAPTURE_GPU and MW2_REPLAY_GPU's hooks in the command processor and the image pool
// (gpu/stream_capture.h, C++): the three flags read inline, and the slow paths called
// only when they are set.
#[cfg(feature = "diagnostics")]
pub mod stream {
    use std::ffi::{CStr, c_char};

    /// gpu::stream::Wait.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Wait {
        pub memory: bool,
        pub value: u32,
        pub polls: u32,
        pub timed_out: bool,
    }
    const _: () = assert!(std::mem::size_of::<Wait>() == 16 && std::mem::offset_of!(Wait, timed_out) == 12);

    unsafe extern "C" {
        // The addresses of gpu::stream::detail's capturing, replaying and counting.
        static mw2_stream_capturing: *const bool;
        static mw2_stream_replaying: *const bool;
        static mw2_stream_counting: *const bool;
        fn mw2_stream_packet();
        fn mw2_stream_read(address: u32, bytes: u64);
        fn mw2_stream_polled(address: u32);
        fn mw2_stream_write(address: u32, bytes: u32, done: bool);
        fn mw2_stream_aperture_read(index: u32, value: u32);
        fn mw2_stream_wait_done(memory: bool, address: u32, value: u32, polls: u32, timed_out: bool);
        fn mw2_stream_replay_wait(wait: *mut Wait) -> bool;
        fn mw2_stream_pool_seen(destination: u32, source: u32, size: u32, stream_constant: u32, frame: u64);
        fn mw2_stream_diverged(what: *const c_char);
    }

    #[inline(always)]
    pub fn capturing() -> bool {
        // SAFETY: the address of a bool the C++ holds for the run; the consumer sets it.
        unsafe { *mw2_stream_capturing }
    }

    #[inline(always)]
    pub fn replaying() -> bool {
        // SAFETY: as capturing.
        unsafe { *mw2_stream_replaying }
    }

    #[inline(always)]
    fn counting() -> bool {
        // SAFETY: as capturing.
        unsafe { *mw2_stream_counting }
    }

    /// Before each packet the consumer executes.
    #[inline(always)]
    pub fn packet() {
        if counting() {
            unsafe { mw2_stream_packet() }
        }
    }

    /// A read of guest memory on the GPU side, at a guest address.
    #[inline(always)]
    pub fn note_read(address: u32, bytes: u64) {
        if counting() && bytes != 0 {
            unsafe { mw2_stream_read(address, bytes) }
        }
    }

    /// The dword a memory WAIT_REG_MEM polls, before its first poll.
    #[inline(always)]
    pub fn note_polled(address: u32) {
        if counting() {
            unsafe { mw2_stream_polled(address) }
        }
    }

    /// A write the consumer makes itself, bracketed: note_write before, note_written after.
    #[inline(always)]
    pub fn note_write(address: u32, bytes: u32) {
        if capturing() {
            unsafe { mw2_stream_write(address, bytes, false) }
        }
    }

    #[inline(always)]
    pub fn note_written(address: u32, bytes: u32) {
        if capturing() || replaying() {
            unsafe { mw2_stream_write(address, bytes, true) }
        }
    }

    /// A register read from the aperture because the stream never wrote it.
    pub fn aperture_read(index: u32, value: u32) {
        unsafe { mw2_stream_aperture_read(index, value) }
    }

    pub fn wait_done(memory: bool, address: u32, value: u32, polls: u32, timed_out: bool) {
        unsafe { mw2_stream_wait_done(memory, address, value, polls, timed_out) }
    }

    /// The replay's journal entry for the WAIT_REG_MEM at this packet.
    pub fn replay_wait() -> Option<Wait> {
        let mut wait = Wait::default();
        unsafe { mw2_stream_replay_wait(&mut wait) }.then_some(wait)
    }

    /// The image pool's queue, at the moment the consumer looks at it.
    pub fn pool_seen(destination: u32, source: u32, size: u32, stream_constant: u32, frame: u64) {
        unsafe { mw2_stream_pool_seen(destination, source, size, stream_constant, frame) }
    }

    /// The replay found the consumer doing something the capture did not have it do.
    pub fn diverged(what: &CStr) {
        unsafe { mw2_stream_diverged(what.as_ptr()) }
    }
}

// The renderer's part of a run's start and end, which only run.rs asks for.
pub mod run {
    use std::path::Path;

    /// The device, the window (hidden until show_window) and the renderer's threads.
    pub fn gpu_initialise() {
        crate::runtime::gpu::gpu::initialise();
    }

    /// Once, whoever asks first; a second ask returns once the first has finished.
    pub fn gpu_shutdown() {
        crate::runtime::gpu::gpu::shutdown();
    }

    #[cfg(feature = "diagnostics")]
    pub fn gpu_report() {
        crate::runtime::gpu::gpu::report();
    }

    pub fn show_window() {
        crate::runtime::gpu::vulkan::presenter::show_window();
    }

    /// A player's start: the pipeline cache the launcher compiled into, and what it says
    /// it compiled, read from the game folder.
    pub fn use_compiled_shaders(game_folder: &Path) {
        mw2_shaders::pipeline::set_cache_path(Some(mw2_shaders::shader_build::CACHE_FILE.as_bytes()));
        crate::runtime::gpu::vulkan::renderer::check_compiled_shaders(game_folder);
    }

    /// Whether the shaders were compiled for this device; if not, why, and the device.
    pub fn compiled_shaders() -> (bool, Vec<u8>, Vec<u8>) {
        {
            let (compiled, why, device) = crate::runtime::gpu::vulkan::renderer::compiled_shaders();
            (compiled, why.into_bytes(), device.into_bytes())
        }
    }
}

// The command processor's side of MW2_REPLAY_GPU (runtime/gpu/stream_capture.cpp and
// gpu/command_processor.rs), which only gpu_replay.rs asks for.
#[cfg(feature = "diagnostics")]
pub mod replay {
    /// Mw2ReplayPage: a page to write before the packet, or with `own`, one the
    /// consumer writes itself, to be found as captured at its next read.
    #[repr(C)]
    pub struct Page {
        pub packet: u32,
        pub page: u32,
        pub own: u32,
        pub reserved: u32,
        pub bytes: *const u8,
    }

    /// Mw2ReplayEvent, its fields by kind (stream_capture.h).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Event {
        pub kind: u32,
        pub packet: u32,
        pub a: u32,
        pub b: u32,
        pub c: u32,
        pub d: u32,
        pub e: u64,
    }

    pub const POOL: u32 = 1;
    pub const WAIT: u32 = 2;
    pub const APERTURE: u32 = 3;
    pub const ARENA: u32 = 4;

    /// Mw2ReplayBatch.
    #[repr(C)]
    pub struct Batch {
        pub pages: *const Page,
        pub page_count: u64,
        pub events: *const Event,
        pub event_count: u64,
    }

    unsafe extern "C" {
        fn mw2_replay_begin();
        fn mw2_replay_batch(batch: *const Batch);
        fn mw2_replay_consumer_read() -> u32;
        fn mw2_replay_finished(wait_timeouts: u64);
        fn mw2_cp_restore_for_replay(bytes: *const u8, size: u64) -> bool;
    }

    /// From here on the command processor counts positions and takes the journal.
    pub fn begin() {
        unsafe { mw2_replay_begin() }
    }

    /// The command processor and the arena's progress, from the capture's state.bin.
    pub fn restore(state: &[u8]) -> bool {
        unsafe { mw2_cp_restore_for_replay(state.as_ptr(), state.len() as u64) }
    }

    /// The batch's journal, for the consumer to take at the batch's start.
    ///
    /// # Safety
    /// `batch` and everything it points at live until the consumer has executed the
    /// batch (its read index has reached the write index stored after this call).
    pub unsafe fn hand(batch: *const Batch) {
        unsafe { mw2_replay_batch(batch) }
    }

    /// How far the consumer has read the ring.
    pub fn consumer_read() -> u32 {
        unsafe { mw2_replay_consumer_read() }
    }

    /// The last batch is done: the driver's timeouts, for the report.
    pub fn finished(wait_timeouts: u64) {
        unsafe { mw2_replay_finished(wait_timeouts) }
    }
}
