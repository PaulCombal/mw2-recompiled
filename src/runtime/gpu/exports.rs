// What the C++ renderer calls in the Rust one while the two share the game: each
// function is the C++ declaration of its name (runtime/rust_renderer.cpp forwards to
// it) and converts between C and the module's types and nothing else.
//
// Unsafe by nature: every pointer comes from the C++ forwarders, which pass a
// TextureFetch's six dwords, out-structs on their stack whose layouts both sides
// assert, the host addresses of guest ranges (read_texture's contract), a callback
// that resizes a std::vector the forwarder owns and returns its zeroed bytes, and the
// renderer's Vulkan handles and VkSubmitInfo, valid for the call. The rig's functions
// (mw2_gpu_rig) take only pointers tools/gpu_diff/presenter.cpp owns and keeps valid
// for the call, at the sizes their types say.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(mw2_owns_texture)]
mod texture {
    use crate::runtime::gpu::texture::{self, Decoded, Extents, FormatInfo, Level, TextureFetch, TextureHeader};
    use std::ffi::{c_char, c_void};
    use std::ptr;

    type Reserve = unsafe extern "C" fn(context: *mut c_void, size: usize) -> *mut u8;

    /// # Safety
    /// `words` must point at six readable dwords.
    unsafe fn fetch_of(words: *const u32) -> TextureFetch {
        // SAFETY: the C++ passes TextureFetch::d, six dwords.
        TextureFetch { d: unsafe { *words.cast::<[u32; 6]>() } }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_read(
        fetch: *const u32,
        base: *const u8,
        mips: *const u8,
        out: *mut TextureHeader,
        reserve: Reserve,
        context: *mut c_void,
    ) -> bool {
        // SAFETY: six dwords (the forwarder's TextureFetch::d).
        let fetch = unsafe { fetch_of(fetch) };
        // SAFETY: base and mips are the texture cache's guest ranges; reserve resizes the
        // forwarder's vector (zero-filled) and returns its bytes, read_texture's contract.
        let header = unsafe { texture::read_texture(&fetch, base, mips, &mut |size| reserve(context, size)) };
        // SAFETY: `out` is the forwarder's TextureHeader, the same layout (asserted both sides).
        unsafe { out.write(header) };
        header.ok
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_extents(fetch: *const u32, out: *mut Extents) {
        // SAFETY: six dwords, and the forwarder's TextureExtents (layout asserted both sides).
        unsafe { out.write(texture::extents_of(&fetch_of(fetch))) }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_format_of(format: u32, out: *mut FormatInfo) {
        // SAFETY: the forwarder's TextureFormatInfo (layout asserted both sides).
        unsafe { out.write(texture::texture_format_of(format)) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_format_name(format: u32) -> *const c_char {
        texture::texture_format_name(format).map_or(ptr::null(), |name| name.as_ptr())
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_tiled_offset_2d(x: i32, y: i32, pitch: u32, log2: u32) -> i32 {
        texture::tiled_offset_2d(x, y, pitch, log2)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_tiled_offset_3d(x: i32, y: i32, z: i32, pitch: u32, height: u32, log2: u32) -> i64 {
        texture::tiled_offset_3d(x, y, z, pitch, height, log2)
    }

    #[unsafe(no_mangle)]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn mw2_texture_decode_rgba(
        ok: bool,
        expanded: bool,
        bytes_per_block: u32,
        levels: *const Level,
        level_count: usize,
        bytes: *const u8,
        size: usize,
        format: u32,
        level: u32,
        rgba: Reserve,
        context: *mut c_void,
    ) -> bool {
        // SAFETY: the C++'s TextureData: its levels and its bytes.
        let data = unsafe {
            Decoded {
                ok,
                expanded,
                bytes_per_block,
                levels: if level_count == 0 { &[] } else { std::slice::from_raw_parts(levels, level_count) },
                bytes: if size == 0 { &[] } else { std::slice::from_raw_parts(bytes, size) },
            }
        };
        // SAFETY: rgba resizes the forwarder's vector (zero-filled) and returns its bytes.
        unsafe { texture::decode_to_rgba(&data, format, level, &mut |n| rgba(context, n)) }
    }
}

#[cfg(mw2_owns_texture_formats)]
mod formats {
    use crate::runtime::gpu::vulkan::texture_formats as f;
    use std::ffi::c_char;

    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_vulkan_format_for(format: u32) -> i32 {
        f::vulkan_format_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_srgb_format_for(format: i32) -> i32 {
        f::srgb_format_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_vulkan_format_name(format: i32) -> *const c_char {
        f::vulkan_format_name(format).as_ptr()
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_host_swizzle_for(format: u32) -> u32 {
        f::host_swizzle_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_compose_swizzle(guest: u32, host: u32) -> u32 {
        f::compose_swizzle(guest, host)
    }
}

// vk::Start and the rest of presenter.h (gpu/vulkan/presenter.rs). Handles cross as
// their Vulkan types: a dispatchable one a pointer, a non-dispatchable one 64 bits,
// which ash's handle types are, transparently.
#[cfg(mw2_owns_presenter)]
mod presenter {
    use crate::runtime::gpu::vulkan::presenter as p;
    use ash::vk;

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_start() -> bool {
        p::start()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_show_window() {
        p::show_window();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_stop_presenting() {
        p::stop_presenting();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_stop() {
        p::stop();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_show_image(image: vk::Image, width: u32, height: u32, serial: u64) {
        p::show_image(image, width, height, serial);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_wait_until_taken(serial: u64) {
        p::wait_until_taken(serial);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_guest_blank_period() -> u64 {
        p::guest_blank_period()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_running() -> bool {
        p::running()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_presenter_presented_frames() -> u64 {
        p::presented_frames()
    }
}

// vk::gpu_time (gpu/vulkan/gpu_time.rs). VkSubmitInfo crosses by pointer; its layout is
// asserted here and in the forwarder.
#[cfg(mw2_owns_gpu_time)]
mod gpu_time {
    use crate::runtime::gpu::vulkan::gpu_time;
    use ash::vk;

    const _: () = assert!(
        std::mem::size_of::<vk::SubmitInfo>() == 72
            && std::mem::offset_of!(vk::SubmitInfo, command_buffer_count) == 40
            && std::mem::offset_of!(vk::SubmitInfo, p_command_buffers) == 48
            && std::mem::offset_of!(vk::SubmitInfo, p_signal_semaphores) == 64
    );

    /// # Safety
    /// The renderer's live physical device, device and queue.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_gpu_time_start(
        physical: vk::PhysicalDevice,
        device: vk::Device,
        queue: vk::Queue,
        family: u32,
    ) {
        // SAFETY: the renderer's handles (gpu_time::start's contract).
        unsafe { gpu_time::start(physical, device, queue, family) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_gpu_time_stop() {
        gpu_time::stop();
    }

    /// # Safety
    /// `queue_submit` is the loader's vkQueueSubmit, `submit` the caller's VkSubmitInfo,
    /// valid for the call, under vk::pipeline::QueueMutex.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_gpu_time_submit(
        queue_submit: vk::PFN_vkQueueSubmit,
        queue: vk::Queue,
        submit: *const vk::SubmitInfo<'static>,
        fence: vk::Fence,
    ) -> vk::Result {
        // SAFETY: the forwarder's VkSubmitInfo (layout asserted both sides), under the lock.
        unsafe { gpu_time::submit(queue_submit, queue, &*submit, fence) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_gpu_time_collect() {
        gpu_time::collect();
    }
}

// vk::capture (gpu/vulkan/capture.rs).
#[cfg(mw2_owns_capture)]
mod capture {
    use crate::runtime::gpu::vulkan::capture;

    #[unsafe(no_mangle)]
    extern "C" fn mw2_capture_frame_boundary(world_drawn: bool, frame: u64) {
        capture::frame_boundary(world_drawn, frame);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_capture_request_now(frames: u32) {
        capture::request_now(frames);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_capture_toggle_stream() {
        capture::toggle_stream();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_capture_report() {
        capture::report();
    }
}

// tools/gpu_diff.py --rig presenter: the presenter's, the backdrop's and gpu_time's
// decisions as functions of values, for the rig to hold against the C++ originals
// compiled beside them. The window thread's own code and the backdrop's draws run on
// the rig's stand-in Vulkan calls instead (presenter.rs and backdrop.rs, mod rig).
// Nothing here makes a device or a window.
#[cfg(all(mw2_gpu_rig, mw2_owns_presenter, mw2_owns_backdrop, mw2_owns_gpu_time))]
mod rig {
    use crate::runtime::gpu::vulkan::presenter::{self as p, Display};
    use crate::runtime::gpu::vulkan::{backdrop, gpu_time};
    use ash::vk;
    use std::ffi::c_void;

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_letterbox(
        stretch: bool,
        width: u32,
        height: u32,
        fw: u32,
        fh: u32,
        out: *mut [i32; 6],
    ) {
        let [from, to] = p::letterbox(stretch, vk::Extent2D { width, height }, fw, fh);
        unsafe { out.write([from.x, from.y, from.z, to.x, to.y, to.z]) };
    }

    #[repr(C)]
    struct Follow {
        exact: u32,
        hz: f64,
        period: f64,
        nominal: f64,
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_follow(
        has_mode: bool,
        numerator: i32,
        denominator: i32,
        refresh: f32,
        present_wait: bool,
        out: *mut Follow,
    ) {
        let mode = has_mode.then_some(p::Mode { numerator, denominator, refresh });
        let (exact, hz, period, nominal) = p::follow(mode, present_wait);
        unsafe { out.write(Follow { exact: exact.into(), hz, period, nominal }) };
    }

    #[repr(C)]
    struct DisplayState {
        nominal: f64,
        period: f64,
        last_ns: i64,
        last_id: u64,
        exact: u32,
        buffered: f64,
        on_time: u64,
        stretched: u64,
        behind: [u64; 4],
    }

    type Displayed = unsafe extern "C" fn(context: *mut c_void, serial: u64, extra: u32, gap_ms: f64);

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_display_new(nominal: f64, period: f64, exact: bool) -> *mut Display {
        let mut display = Display::new();
        (display.nominal_ns, display.period_ns, display.exact_rate) = (nominal, period, exact);
        Box::into_raw(Box::new(display))
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_display_free(display: *mut Display) {
        drop(unsafe { Box::from_raw(display) });
    }

    /// Returns 1 with the guest's period and time when they are published.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_display_seen(
        display: *mut Display,
        id: u64,
        at: i64,
        queued: u64,
        console: bool,
        displayed: Displayed,
        context: *mut c_void,
        period: *mut u64,
        published_at: *mut i64,
    ) -> u32 {
        let display = unsafe { &mut *display };
        let mut sink = |serial, extra, gap| unsafe { displayed(context, serial, extra, gap) };
        match display.saw_displayed(id, at, queued, console, &mut sink) {
            Some((guest, when)) => {
                unsafe {
                    period.write(guest);
                    published_at.write(when);
                }
                1
            }
            None => 0,
        }
    }

    // A present that did not reach the screen, or a swapchain remade.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_display_lost(display: *mut Display) {
        unsafe { (*display).last_shown_ns = -1 };
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_display_read(display: *const Display, out: *mut DisplayState) {
        let d = unsafe { &*display };
        unsafe {
            out.write(DisplayState {
                nominal: d.nominal_ns,
                period: d.period_ns,
                last_ns: d.last_shown_ns,
                last_id: d.last_shown_id,
                exact: d.exact_rate.into(),
                buffered: d.buffered,
                on_time: d.shown_on_time,
                stretched: d.stretched,
                behind: d.behind_count,
            })
        };
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_guest_blank(period: u64, at: i64, now: i64) -> u64 {
        p::blank_period(period, at, now)
    }

    /// The shader's words into `out`, at most `capacity`; returns how many there are.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_backdrop_shader(fragment: bool, out: *mut u32, capacity: usize) -> usize {
        let words = if fragment { backdrop::fragment_shader() } else { backdrop::vertex_shader() };
        let n = words.len().min(capacity);
        unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), out, n) };
        words.len()
    }

    // ---- gpu_time

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_gpu_timing(
        bits: *const u32,
        families: u32,
        family: u32,
        period: f32,
        mask: *mut u64,
        nanoseconds_per_tick: *mut f64,
    ) -> bool {
        let bits = if families == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(bits, families as usize) } };
        match gpu_time::timing(bits, family, period) {
            Some((m, n)) => {
                unsafe {
                    mask.write(m);
                    nanoseconds_per_tick.write(n);
                }
                true
            }
            None => false,
        }
    }

    struct RigRing {
        ring: gpu_time::Ring,
        collector: gpu_time::Collector,
        live: [bool; gpu_time::PAIRS as usize],
        collected: u64,
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_gpu_ring_new() -> *mut c_void {
        let ring = RigRing {
            ring: gpu_time::Ring::default(),
            collector: gpu_time::Collector::new(),
            live: [false; gpu_time::PAIRS as usize],
            collected: 0,
        };
        Box::into_raw(Box::new(ring)).cast()
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_gpu_ring_free(ring: *mut c_void) {
        drop(unsafe { Box::from_raw(ring.cast::<RigRing>()) });
    }

    /// A submission, which the queue takes or refuses: its pair, or -1 untimed.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_gpu_submit(ring: *mut c_void, accepted: bool) -> i32 {
        let r = unsafe { &mut *ring.cast::<RigRing>() };
        let Some(pair) = r.ring.take(r.collected) else { return -1 };
        r.live[pair as usize] = accepted;
        pair as i32
    }

    type Read = unsafe extern "C" fn(context: *mut c_void, pair: u32, results: *mut [u64; 4]) -> i32;

    #[repr(C)]
    struct Collected {
        collected: u64,
        nanoseconds: u64,
        last_end: u64,
        any_end: u32,
        seen: [[u64; 2]; gpu_time::PAIRS as usize],
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_gpu_collect(
        ring: *mut c_void,
        mask: u64,
        nanoseconds_per_tick: f64,
        read: Read,
        context: *mut c_void,
        out: *mut Collected,
    ) {
        let r = unsafe { &mut *ring.cast::<RigRing>() };
        let live = r.live;
        let (collected, busy) = r.collector.collect(
            r.collected,
            r.ring.taken,
            mask,
            |pair| live[pair as usize],
            |pair| {
                let mut values = [0u64; 4];
                let result = unsafe { read(context, pair, &mut values) };
                (vk::Result::from_raw(result), [[values[0], values[1]], [values[2], values[3]]])
            },
        );
        r.collected = collected;
        let nanoseconds = if busy != 0 { gpu_time::busy_nanoseconds(busy, nanoseconds_per_tick) } else { 0 };
        unsafe {
            out.write(Collected {
                collected,
                nanoseconds,
                last_end: r.collector.last_end,
                any_end: r.collector.any_end.into(),
                seen: r.collector.seen,
            })
        };
    }
}
