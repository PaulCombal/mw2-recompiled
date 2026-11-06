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
    #[cfg(all(feature = "diagnostics", not(windows)))]
    fn mw2_gpu_frame_count() -> u64;
    // src/runtime/run.rs's, by these names so that a rig can give its own (exports.rs).
    fn mw2_crash_on_fault();
    fn mw2_crash_request_exit(why: *const c_char);
    fn mw2_gpu_set_ring_buffer(base_physical: u32, size_log2: u32);
    fn mw2_gpu_set_read_pointer_write_back(address_physical: u32, block_size_log2: u32);
    fn mw2_gpu_set_interrupt_callback(callback: u32, context: u32);
    fn mw2_gpu_swap();
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
    unsafe { mw2_gpu_frame_count() }
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
    unsafe { mw2_gpu_set_ring_buffer(base_physical, size_log2) }
}

/// Where the GPU reports how far it has read.
pub fn gpu_set_read_pointer_write_back(address_physical: u32, block_size_log2: u32) {
    unsafe { mw2_gpu_set_read_pointer_write_back(address_physical, block_size_log2) }
}

/// The title's routine for the GPU's interrupts.
pub fn gpu_set_interrupt_callback(callback: u32, context: u32) {
    unsafe { mw2_gpu_set_interrupt_callback(callback, context) }
}

/// VdSwap: the frame is handed over.
pub fn gpu_swap() {
    unsafe { mw2_gpu_swap() }
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

// The renderer's part of a run's start and end (runtime/rust_calls.cpp), which only
// run.rs asks for: the rigs, which have no renderer, leave it out.
#[cfg(not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig)))]
pub mod run {
    use std::ffi::{CStr, c_char, c_void};

    type TextSink = unsafe extern "C" fn(to: *mut c_void, text: *const c_char, size: usize);

    unsafe extern "C" {
        fn mw2_gpu_initialise();
        fn mw2_gpu_shutdown();
        #[cfg(feature = "diagnostics")]
        fn mw2_gpu_report();
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
        unsafe { mw2_gpu_initialise() }
    }

    pub fn gpu_shutdown() {
        unsafe { mw2_gpu_shutdown() }
    }

    #[cfg(feature = "diagnostics")]
    pub fn gpu_report() {
        unsafe { mw2_gpu_report() }
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
