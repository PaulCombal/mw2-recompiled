// What the Rust runtime asks of the C++ one: runtime/rust_calls.cpp. The kernel's
// go as the rest of the kernel moves, the renderer's stay.

use std::ffi::{CStr, c_char, c_void};

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct WatchCosts {
    pub faults: u64,
    pub protects: u64,
    pub protect_nanoseconds: u64,
}

unsafe extern "C" {
    fn mw2_watch_costs(costs: *mut WatchCosts);
    fn mw2_watch_handle_fault(address: *const c_void) -> bool;
    #[cfg(all(feature = "diagnostics", not(windows), not(all(feature = "rust-renderer", mw2_owns_gpu))))]
    fn mw2_gpu_frame_count() -> u64;
    // src/runtime/run.rs's, by these names so that a rig can give its own (exports.rs).
    fn mw2_crash_on_fault();
    fn mw2_crash_request_exit(why: *const c_char);
}

// The GPU's side of the kernel's Vd* calls: gpu.cpp's, or with the renderer's threads in
// Rust (build.rs, RUST_OWNS), gpu/gpu.rs's.
#[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
unsafe extern "C" {
    fn mw2_gpu_set_ring_buffer(base_physical: u32, size_log2: u32);
    fn mw2_gpu_set_read_pointer_write_back(address_physical: u32, block_size_log2: u32);
    fn mw2_gpu_set_interrupt_callback(callback: u32, context: u32);
    fn mw2_gpu_swap();
}

unsafe extern "C" {
    #[cfg(feature = "diagnostics")]
    fn mw2_renderer_flash_seen(how: *const c_char);
}

pub fn watch_costs() -> WatchCosts {
    let mut costs = WatchCosts::default();
    unsafe { mw2_watch_costs(&mut costs) };
    costs
}

/// A write to a page the renderer watches is marked, and may go on.
pub fn watch_handle_fault(address: *const c_void) -> bool {
    unsafe { mw2_watch_handle_fault(address) }
}

#[cfg(all(feature = "diagnostics", not(windows)))]
pub fn gpu_frame_count() -> u64 {
    #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
    return crate::runtime::gpu::gpu::frame_count();
    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe {
        mw2_gpu_frame_count()
    }
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
    #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
    crate::runtime::gpu::gpu::set_ring_buffer(base_physical, size_log2);
    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe {
        mw2_gpu_set_ring_buffer(base_physical, size_log2)
    }
}

/// Where the GPU reports how far it has read.
pub fn gpu_set_read_pointer_write_back(address_physical: u32, block_size_log2: u32) {
    #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
    crate::runtime::gpu::gpu::set_read_pointer_write_back(address_physical, block_size_log2);
    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe {
        mw2_gpu_set_read_pointer_write_back(address_physical, block_size_log2)
    }
}

/// The title's routine for the GPU's interrupts.
pub fn gpu_set_interrupt_callback(callback: u32, context: u32) {
    #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
    crate::runtime::gpu::gpu::set_interrupt_callback(callback, context);
    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe {
        mw2_gpu_set_interrupt_callback(callback, context)
    }
}

/// VdSwap: the frame is handed over.
pub fn gpu_swap() {
    #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
    crate::runtime::gpu::gpu::swap();
    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe {
        mw2_gpu_swap()
    }
}

/// Somebody saw a flash; `how` names the key.
#[cfg(feature = "diagnostics")]
pub fn flash_seen(how: &CStr) {
    unsafe { mw2_renderer_flash_seen(how.as_ptr()) }
}

// What the Rust renderer asks of the C++ pipeline.cpp (runtime/rust_calls.cpp), which
// stays C++ until the launcher moves: the device it makes, the queue's lock, the
// instance, and its verdict on a Vulkan result.
#[cfg(feature = "rust-renderer")]
pub mod vulkan {
    use std::ffi::{CStr, c_char, c_void};

    unsafe extern "C" {
        fn mw2_vk_device_has_extension(physical: *mut c_void, name: *const c_char) -> bool;
        fn mw2_vk_create_device(
            physical: *mut c_void,
            family: u32,
            extensions: *const *const c_char,
            count: u32,
            device: *mut *mut c_void,
            queue: *mut *mut c_void,
            features: *mut c_void,
        ) -> bool;
        fn mw2_vk_pipeline_initialise(
            device: *mut c_void,
            physical: *mut c_void,
            queue: *mut c_void,
            family: u32,
            instance: *mut c_void,
        ) -> bool;
        fn mw2_vk_failed(result: i32, at: *const c_char) -> bool;
        fn mw2_vk_queue_lock();
        fn mw2_vk_queue_unlock();
        fn mw2_vk_instance() -> *mut c_void;
    }

    /// vk::pipeline::DeviceHasExtension.
    pub fn device_has_extension(physical: *mut c_void, name: &CStr) -> bool {
        unsafe { mw2_vk_device_has_extension(physical, name.as_ptr()) }
    }

    /// vk::pipeline::CreateDevice, its feature chain and all: the device and its queue.
    ///
    /// # Safety
    /// `physical` is a physical device of the live instance, every extension name a
    /// C string, and `features` null or a feature chain that lives through the call.
    pub unsafe fn create_device(
        physical: *mut c_void,
        family: u32,
        extensions: &[*const c_char],
        features: *mut c_void,
    ) -> Option<(*mut c_void, *mut c_void)> {
        let (mut device, mut queue) = (std::ptr::null_mut(), std::ptr::null_mut());
        let made = unsafe {
            mw2_vk_create_device(
                physical,
                family,
                extensions.as_ptr(),
                extensions.len() as u32,
                &mut device,
                &mut queue,
                features,
            )
        };
        made.then_some((device, queue))
    }

    /// vk::pipeline::Initialise on a device made elsewhere, which it borrows.
    pub fn pipeline_initialise(
        device: *mut c_void,
        physical: *mut c_void,
        queue: *mut c_void,
        family: u32,
        instance: *mut c_void,
    ) -> bool {
        unsafe { mw2_vk_pipeline_initialise(device, physical, queue, family, instance) }
    }

    /// vk::pipeline::Failed.
    pub fn failed(result: i32, at: &CStr) -> bool {
        unsafe { mw2_vk_failed(result, at.as_ptr()) }
    }

    /// vk::pipeline::QueueMutex().lock(), and unlock().
    pub fn queue_lock() {
        unsafe { mw2_vk_queue_lock() }
    }

    pub fn queue_unlock() {
        unsafe { mw2_vk_queue_unlock() }
    }

    /// vk::pipeline::Instance(): null before the device is up.
    pub fn instance() -> *mut c_void {
        unsafe { mw2_vk_instance() }
    }
}

// What gpu/gpu.rs and gpu/arena.rs call of the command processor, the census, the
// image pool and the C++ renderer that is left (runtime/rust_calls.cpp): the command
// processor's own are gpu/command_processor.rs's, census.rs's and image_move.rs's when
// Rust owns them (build.rs, RUST_OWNS), the renderer's start, end, idle and reports, and
// the GPU capture's batch brackets.
#[cfg(feature = "rust-renderer")]
pub mod gpu {
    #[cfg(mw2_owns_command_processor)]
    use crate::runtime::gpu::command_processor as cp;

    #[cfg(not(mw2_owns_command_processor))]
    unsafe extern "C" {
        fn mw2_cp_execute_ring(base: u32, read_index: u32, mask: u32, dwords: u32);
        fn mw2_cp_deliver_occlusion_queries();
        fn mw2_cp_rendering() -> bool;
        fn mw2_cp_write_physical(physical: u32, value: u32);
        #[cfg(feature = "diagnostics")]
        fn mw2_cp_report();
    }

    unsafe extern "C" {
        fn mw2_renderer_initialise() -> bool;
        fn mw2_renderer_shutdown();
        fn mw2_renderer_idle();
        fn mw2_textures_shutdown();
        fn mw2_pipeline_shutdown();
    }

    #[cfg(feature = "diagnostics")]
    unsafe extern "C" {
        // tools/hook_diff.py logs the present the D3D hooks report on both sides here.
        #[cfg(any(not(mw2_owns_census), mw2_hook_rig))]
        fn mw2_census_present();
        #[cfg(not(mw2_owns_census))]
        fn mw2_census_report();
        fn mw2_renderer_report();
        fn mw2_textures_report();
        fn mw2_watch_report();
        #[cfg(not(mw2_owns_image_move))]
        fn mw2_imagepool_report();
        fn mw2_stream_batch_begin(base: u32, read_index: u32, mask: u32, dwords: u32);
        fn mw2_stream_batch_end();
        fn mw2_stream_shutdown();
        fn mw2_stream_report();
        fn mw2_stream_arena_block_seen(block: u32);
    }

    /// Executes `dwords` of the ring starting at `read_index`.
    pub fn execute_ring(base: u32, read_index: u32, mask: u32, dwords: u32) {
        #[cfg(mw2_owns_command_processor)]
        cp::execute_ring(base, read_index, mask, dwords);
        #[cfg(not(mw2_owns_command_processor))]
        unsafe {
            mw2_cp_execute_ring(base, read_index, mask, dwords)
        }
    }

    /// Writes the records of the occlusion queries the GPU has finished counting.
    pub fn deliver_occlusion_queries() {
        #[cfg(mw2_owns_command_processor)]
        cp::deliver_occlusion_queries();
        #[cfg(not(mw2_owns_command_processor))]
        unsafe {
            mw2_cp_deliver_occlusion_queries()
        }
    }

    /// False under MW2_RENDER=0: the stream is executed and counted, nothing drawn.
    pub fn rendering() -> bool {
        #[cfg(mw2_owns_command_processor)]
        return cp::rendering();
        #[cfg(not(mw2_owns_command_processor))]
        unsafe {
            mw2_cp_rendering()
        }
    }

    /// The command processor's write of a dword at a physical address.
    pub fn write_physical(physical: u32, value: u32) {
        #[cfg(mw2_owns_command_processor)]
        cp::write_physical(physical, value);
        #[cfg(not(mw2_owns_command_processor))]
        unsafe {
            mw2_cp_write_physical(physical, value)
        }
    }

    pub fn renderer_initialise() -> bool {
        unsafe { mw2_renderer_initialise() }
    }

    pub fn renderer_shutdown() {
        unsafe { mw2_renderer_shutdown() }
    }

    /// The consumer has run dry for a while: the recorder may sleep.
    pub fn renderer_idle() {
        unsafe { mw2_renderer_idle() }
    }

    pub fn textures_shutdown() {
        unsafe { mw2_textures_shutdown() }
    }

    pub fn pipeline_shutdown() {
        unsafe { mw2_pipeline_shutdown() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn cp_report() {
        #[cfg(mw2_owns_command_processor)]
        cp::report();
        #[cfg(not(mw2_owns_command_processor))]
        unsafe {
            mw2_cp_report()
        }
    }

    /// At each present the title makes.
    #[cfg(feature = "diagnostics")]
    pub fn census_present() {
        #[cfg(all(mw2_owns_census, not(mw2_hook_rig)))]
        crate::runtime::gpu::census::present();
        #[cfg(any(not(mw2_owns_census), mw2_hook_rig))]
        unsafe {
            mw2_census_present()
        }
    }

    #[cfg(feature = "diagnostics")]
    pub fn census_report() {
        #[cfg(mw2_owns_census)]
        crate::runtime::gpu::census::report();
        #[cfg(not(mw2_owns_census))]
        unsafe {
            mw2_census_report()
        }
    }

    #[cfg(feature = "diagnostics")]
    pub fn renderer_report() {
        unsafe { mw2_renderer_report() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn textures_report() {
        unsafe { mw2_textures_report() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn watch_report() {
        unsafe { mw2_watch_report() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn imagepool_report() {
        #[cfg(mw2_owns_image_move)]
        crate::runtime::gpu::image_move::report();
        #[cfg(not(mw2_owns_image_move))]
        unsafe {
            mw2_imagepool_report()
        }
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

// What the command processor and the image pool (gpu/command_processor.rs,
// gpu/image_move.rs) call of the C++ renderer, one call a draw (runtime/rust_calls.cpp).
#[cfg(feature = "rust-renderer")]
pub mod renderer {
    use crate::runtime::gpu::registers::RegisterFile;

    /// vk::renderer::DrawCall: what the draw packet carries that the register file does
    /// not. Its layout is the C++'s, asserted here and in rust_calls.cpp.
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

    unsafe extern "C" {
        fn mw2_renderer_draw(registers: *const RegisterFile, call: *const DrawCall);
        fn mw2_renderer_resolve(registers: *const RegisterFile);
        fn mw2_renderer_swap(front_buffer: u32);
        fn mw2_renderer_before_completion();
        fn mw2_renderer_begin_occlusion();
        fn mw2_renderer_end_occlusion(tag: u32, samples: *mut u64) -> bool;
        fn mw2_renderer_collect_occlusion(deliver: extern "C" fn(tag: u32, samples: u64));
        fn mw2_renderer_idle();
        fn mw2_renderer_before_stream_write(physical: u32, size: u32);
        fn mw2_textures_memory_written(physical: u32, size: u32);
    }

    #[inline(always)]
    pub fn draw(registers: &RegisterFile, call: &DrawCall) {
        unsafe { mw2_renderer_draw(registers, call) }
    }

    /// A draw issued in copy mode.
    pub fn resolve(registers: &RegisterFile) {
        unsafe { mw2_renderer_resolve(registers) }
    }

    /// The swap packet: the frame is what was resolved to this front buffer.
    pub fn swap(front_buffer: u32) {
        unsafe { mw2_renderer_swap(front_buffer) }
    }

    /// The command processor is about to tell the title the work before this point has
    /// finished.
    pub fn before_completion() {
        unsafe { mw2_renderer_before_completion() }
    }

    pub fn begin_occlusion_query() {
        unsafe { mw2_renderer_begin_occlusion() }
    }

    /// The count now, when there is nothing to count; None when it comes later.
    pub fn end_occlusion_query(tag: u32) -> Option<u64> {
        let mut samples = 0u64;
        unsafe { mw2_renderer_end_occlusion(tag, &mut samples) }.then_some(samples)
    }

    pub fn collect_occlusion_queries(deliver: extern "C" fn(tag: u32, samples: u64)) {
        unsafe { mw2_renderer_collect_occlusion(deliver) }
    }

    /// The ring consumer has nothing to read for now.
    pub fn idle() {
        unsafe { mw2_renderer_idle() }
    }

    /// The command stream is about to write [physical, physical + size).
    pub fn before_stream_write(physical: u32, size: u32) {
        unsafe { mw2_renderer_before_stream_write(physical, size) }
    }

    /// vk::textures::MemoryWritten.
    pub fn textures_memory_written(physical: u32, size: u32) {
        unsafe { mw2_textures_memory_written(physical, size) }
    }
}

// The memory watch (gpu/memory_watch.cpp, C++ until it moves): what the command
// processor's shader memo and the image pool ask of it.
#[cfg(feature = "rust-renderer")]
pub mod watch {
    unsafe extern "C" {
        fn mw2_watch_enabled() -> bool;
        fn mw2_watch_track(physical: u32, bytes: u32, frame: u64) -> u64;
        fn mw2_watch_unwritten(physical: u32, bytes: u32, since: u64) -> bool;
        fn mw2_watch_will_write(address: u32, bytes: u32);
    }

    pub fn enabled() -> bool {
        unsafe { mw2_watch_enabled() }
    }

    /// Before reading the range: a stamp when every page of it is watched from then on.
    pub fn track(physical: u32, bytes: u32, frame: u64) -> u64 {
        unsafe { mw2_watch_track(physical, bytes, frame) }
    }

    pub fn unwritten(physical: u32, bytes: u32, since: u64) -> bool {
        unsafe { mw2_watch_unwritten(physical, bytes, since) }
    }

    /// Before this runtime writes a large range of guest memory itself.
    pub fn will_write(address: u32, bytes: u32) {
        unsafe { mw2_watch_will_write(address, bytes) }
    }
}

// MW2_CAPTURE_GPU and MW2_REPLAY_GPU's hooks in the command processor and the image pool
// (gpu/stream_capture.h, C++): the three flags read where the C++ reads them inline, and
// the slow paths called only when they are set.
#[cfg(all(feature = "rust-renderer", feature = "diagnostics"))]
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

// The renderer's part of a run's start and end (runtime/rust_calls.cpp), which only
// run.rs asks for: the rigs, which have no renderer, leave it out.
#[cfg(not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig)))]
pub mod run {
    use std::ffi::{CStr, c_char, c_void};

    type TextSink = unsafe extern "C" fn(to: *mut c_void, text: *const c_char, size: usize);

    #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
    unsafe extern "C" {
        fn mw2_gpu_initialise();
        fn mw2_gpu_shutdown();
        #[cfg(feature = "diagnostics")]
        fn mw2_gpu_report();
    }

    unsafe extern "C" {
        #[cfg(not(all(feature = "rust-renderer", mw2_owns_presenter)))]
        fn mw2_window_show();
        fn mw2_shaders_use_compiled(game_folder: *const c_char);
        fn mw2_shaders_compiled(why: *mut c_void, device: *mut c_void, sink: TextSink) -> bool;
    }

    unsafe extern "C" fn take_text(to: *mut c_void, text: *const c_char, size: usize) {
        let bytes = if size == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(text.cast::<u8>(), size) } };
        unsafe { &mut *to.cast::<Vec<u8>>() }.extend_from_slice(bytes);
    }

    /// The device, the window (hidden until show_window) and the renderer's threads.
    pub fn gpu_initialise() {
        #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
        crate::runtime::gpu::gpu::initialise();
        #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
        unsafe {
            mw2_gpu_initialise()
        }
    }

    /// Once, whoever asks first; a second ask returns, with the Rust renderer once the
    /// first has finished.
    pub fn gpu_shutdown() {
        #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
        crate::runtime::gpu::gpu::shutdown();
        #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
        unsafe {
            mw2_gpu_shutdown()
        }
    }

    #[cfg(feature = "diagnostics")]
    pub fn gpu_report() {
        #[cfg(all(feature = "rust-renderer", mw2_owns_gpu))]
        crate::runtime::gpu::gpu::report();
        #[cfg(not(all(feature = "rust-renderer", mw2_owns_gpu)))]
        unsafe {
            mw2_gpu_report()
        }
    }

    pub fn show_window() {
        #[cfg(all(feature = "rust-renderer", mw2_owns_presenter))]
        crate::runtime::gpu::vulkan::presenter::show_window();
        #[cfg(not(all(feature = "rust-renderer", mw2_owns_presenter)))]
        unsafe {
            mw2_window_show()
        }
    }

    /// A player's start: the pipeline cache the launcher compiled into, and what it
    /// says it compiled, read from the game folder.
    pub fn use_compiled_shaders(game_folder: &CStr) {
        unsafe { mw2_shaders_use_compiled(game_folder.as_ptr()) }
    }

    /// Whether the shaders were compiled for this device; if not, why, and the device.
    pub fn compiled_shaders() -> (bool, Vec<u8>, Vec<u8>) {
        let (mut why, mut device) = (Vec::new(), Vec::new());
        let compiled = unsafe { mw2_shaders_compiled((&raw mut why).cast(), (&raw mut device).cast(), take_text) };
        (compiled, why, device)
    }
}

// The command processor's side of MW2_REPLAY_GPU (runtime/gpu/stream_capture.cpp and
// command_processor.cpp), which only gpu_replay.rs asks for.
#[cfg(all(feature = "diagnostics", not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig))))]
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
