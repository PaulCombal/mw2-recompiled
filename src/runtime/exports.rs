// What the C++ runtime calls here. Each function is what the header of its name
// declares, and converts between C and a safe module's types and nothing else. The
// pointers are what those headers promise: valid, and a string has its zero.

use crate::guest::{Memory, PPCContext};
use crate::runtime::platform::{self, Load, MemoryState};
use crate::runtime::render_size::{self, Size};
use crate::runtime::{
    clock, console, crash, frame_rate, guest_memory, install, pacing_trace, presence, report, settings, version,
};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::PathBuf;

type TextSink = unsafe extern "C" fn(to: *mut c_void, text: *const c_char, size: usize);

unsafe fn path_from(text: *const c_char) -> PathBuf {
    platform::path(unsafe { CStr::from_ptr(text) })
}

unsafe fn give(to: *mut c_void, sink: TextSink, text: &[u8]) {
    unsafe { sink(to, text.as_ptr().cast(), text.len()) }
}

#[unsafe(no_mangle)]
extern "C" fn mw2_version_release() -> *const c_char {
    version::RELEASE.as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn mw2_version_network() -> u16 {
    version::NETWORK
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_settings_load(path: *const c_char, set: *mut c_void, sink: TextSink) {
    let kept = settings::load(&unsafe { path_from(path) });
    unsafe { give(set, sink, &kept) }
}

#[cfg(T_DATA_RenderTargets)]
mod shadows {
    use crate::runtime::shadow_resolution;

    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_is_map_depth(depth_info: u32, samples: u32) -> bool {
        shadow_resolution::is_shadow_map_depth(depth_info, samples)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_surfaces_known() -> bool {
        shadow_resolution::surfaces_known()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_surface_height() -> u32 {
        shadow_resolution::surface_height()
    }
}

#[cfg(not(T_DATA_RenderTargets))]
mod shadows {
    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_is_map_depth(_depth_info: u32, _samples: u32) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_surfaces_known() -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_shadows_surface_height() -> u32 {
        4096
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_console_pump(ctx: &mut PPCContext, base: *mut u8) {
    console::pump(ctx, unsafe { Memory::new(base) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_console_run_now(text: *const c_char) {
    console::run_now(unsafe { CStr::from_ptr(text) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_console_place(text: *const c_char) {
    console::place(unsafe { CStr::from_ptr(text) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_render_frame(frame: *mut Size) {
    unsafe { frame.write(render_size::frame()) }
}

#[unsafe(no_mangle)]
extern "C" fn mw2_guest_initialise() -> *mut u8 {
    guest_memory::initialise()
}

#[unsafe(no_mangle)]
extern "C" fn mw2_guest_commit(address: u32, size: u32) -> bool {
    guest_memory::commit(address, size)
}

#[unsafe(no_mangle)]
extern "C" fn mw2_guest_commit_on_touch(address: *const c_void) -> bool {
    guest_memory::commit_on_touch(address)
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_initialise() {
    platform::initialise();
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_thread_id() -> u32 {
    platform::thread_id()
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_is_main_thread() -> bool {
    platform::is_main_thread()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_set_thread_name(name: *const c_char) {
    platform::set_thread_name(unsafe { CStr::from_ptr(name) });
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_lower_thread_priority() {
    platform::lower_thread_priority();
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_idle_thread_priority() {
    platform::idle_thread_priority();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_print_backtrace(why: *const c_char) {
    platform::print_backtrace(unsafe { CStr::from_ptr(why) });
}

#[unsafe(no_mangle)]
extern "C" fn mw2_platform_thread_clock() -> usize {
    platform::thread_clock()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_thread_time(clock: usize, nanoseconds: *mut u64) -> bool {
    let Some(time) = platform::thread_time(clock) else { return false };
    unsafe { nanoseconds.write(time) };
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_processor_load(load: *mut Load) -> bool {
    platform::processor_load(unsafe { &mut *load })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_memory_state(memory: *mut MemoryState) {
    platform::memory_state(unsafe { &mut *memory });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_open_library(path: *const c_char) -> *mut c_void {
    platform::open_library(unsafe { CStr::from_ptr(path) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_loaded_library(name: *const c_char) -> *mut c_void {
    platform::loaded_library(unsafe { CStr::from_ptr(name) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_platform_symbol(library: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { platform::symbol(library, CStr::from_ptr(name)) }
}

#[unsafe(no_mangle)]
extern "C" fn mw2_presence_set_context(user: u32, id: u32, value: u32) {
    presence::set_context(user, id, value);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_presence_text(user: u32, line: *mut c_void, sink: TextSink) {
    let text = presence::text(user);
    unsafe { give(line, sink, &text) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_folder(argc: c_int, folder: *mut c_void, sink: TextSink) {
    let path = install::spawn::c_path(&install::folder(argc));
    unsafe { give(folder, sink, path.as_bytes()) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_prepare(image: *mut c_void, sink: TextSink, exit_code: *mut c_int) -> bool {
    let Some(bytes) = install::prepare(unsafe { &mut *exit_code }) else { return false };
    unsafe { give(image, sink, &bytes) };
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_load_image(path: *const c_char, image: *mut c_void, sink: TextSink) -> bool {
    let Some(bytes) = install::load_image_file(&unsafe { path_from(path) }) else { return false };
    unsafe { give(image, sink, &bytes) };
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_compile_shaders(device: *const c_char, why: *const c_char) -> bool {
    install::compile_shaders(unsafe { CStr::from_ptr(device) }, unsafe { CStr::from_ptr(why) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_launch_data(data: *mut c_void, sink: TextSink) {
    let bytes = install::launch_data();
    unsafe { give(data, sink, &bytes) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_set_launch_data(data: *const u8, size: usize) {
    // None, as the title's own call says with a null pointer.
    install::set_launch_data(if size == 0 || data.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, size) }
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_install_set_next_title(xex: *const c_char) {
    install::set_next_title(unsafe { CStr::from_ptr(xex) });
}

#[unsafe(no_mangle)]
extern "C" fn mw2_install_start_next_title() {
    install::start_next_title();
}

#[unsafe(no_mangle)]
extern "C" fn mw2_frame_rate_limit() -> u32 {
    frame_rate::limit()
}

#[unsafe(no_mangle)]
extern "C" fn mw2_pacing_microseconds() -> i64 {
    pacing_trace::microseconds(clock::nanoseconds())
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_frame(world: bool) {
    report::frame(clock::nanoseconds(), &report::System, world);
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_add(cost: c_int, microseconds: u64) {
    report::add(cost as usize, microseconds);
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_spent(time: c_int, nanoseconds: u64) {
    report::spent(time as usize, nanoseconds);
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_shader_built(from_cache: c_int) {
    report::shader_built(from_cache);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_report_thread_started(name: *const c_char) {
    report::thread_started(&report::System, unsafe { CStr::from_ptr(name) });
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_thread_ended() {
    report::thread_ended(&report::System);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_report_shaders_compiled(state: *const c_char) {
    report::shaders_compiled(unsafe { CStr::from_ptr(state) }.to_bytes());
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_timed_begin(time: c_int) -> u64 {
    report::timed_begin(clock::nanoseconds(), time as usize)
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_timed_end(time: c_int, began: u64) {
    report::timed_end(clock::nanoseconds(), time as usize, began);
}

#[unsafe(no_mangle)]
extern "C" fn mw2_report_write() {
    report::write(&report::System);
}

#[unsafe(no_mangle)]
extern "C" fn mw2_crash_install() {
    crash::install();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_crash_register_thread(name: *const c_char) {
    crash::register_thread(unsafe { CStr::from_ptr(name) });
}

#[unsafe(no_mangle)]
extern "C" fn mw2_crash_current_thread_name() -> *const c_char {
    crash::current_thread_name().map_or(std::ptr::null(), CStr::as_ptr)
}

#[unsafe(no_mangle)]
extern "C" fn mw2_crash_unregister_thread() {
    crash::unregister_thread();
}

// The run's end (run.rs): crash.rs's fault and the C++ renderer's window ask for it
// by these names, which each rig gives its own of.
#[cfg(not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig)))]
#[unsafe(no_mangle)]
extern "C" fn mw2_crash_on_fault() {
    crate::runtime::run::on_fault();
}

#[cfg(not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig)))]
#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_crash_request_exit(why: *const c_char) {
    crate::runtime::run::request_exit(unsafe { CStr::from_ptr(why) });
}

/// runtime/log.h's stamp in the Rust build: src/log.rs's clock.
#[unsafe(no_mangle)]
extern "C" fn mw2_log_stamp() -> *const c_char {
    crate::log::stamp()
}

#[cfg(feature = "diagnostics")]
mod diagnostics {
    use crate::runtime::player::{self, Press, Where};
    use crate::runtime::{clock, pacing_trace, platform, stutters};
    use std::ffi::{CStr, c_char, c_int};
    use std::sync::Once;

    fn now_us() -> i64 {
        pacing_trace::microseconds(clock::nanoseconds())
    }

    fn note(event: u8, value: u32) {
        pacing_trace::note(clock::nanoseconds(), event, value, platform::thread_id());
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_pacing_wanted() -> bool {
        pacing_trace::path().is_some()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_pacing_note(event: c_char, value: u32) {
        note(event as u8, value);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_pacing_note_callers(r1: u32) {
        pacing_trace::note_callers(|caller| note(pacing_trace::CALLER, caller), r1);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_pacing_write() {
        if let Some(path) = pacing_trace::path() {
            pacing_trace::write(path);
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_wanted() -> bool {
        stutters::wanted()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_on() -> bool {
        stutters::on()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_add(cost: c_int, nanoseconds: u64) {
        stutters::add(cost as usize, nanoseconds);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_title_presented() {
        stutters::title_presented(now_us());
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_finished(_serial: u64, world: bool) {
        static WATCHING: Once = Once::new();
        stutters::finished(now_us(), world, || {
            WATCHING.call_once(|| {
                std::thread::spawn(stutters::watch_for_stalls);
            });
        });
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_shown(serial: u64) {
        stutters::shown(now_us(), serial);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_displayed(serial: u64, extra_blanks: u32, gap_ms: f64) {
        stutters::displayed(now_us(), serial, extra_blanks, gap_ms);
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_stutters_mark(how: *const c_char) {
        stutters::mark(now_us(), unsafe { CStr::from_ptr(how) });
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_ending() {
        stutters::ending();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_report() {
        stutters::report();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_wanted() -> bool {
        player::wanted()
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_player_locate(out: *mut Where) -> bool {
        let Some(here) = player::locate() else { return false };
        unsafe { out.write(here) };
        true
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_player_steer(axes: *mut [i16; 4], pressed: *mut c_int) -> bool {
        let mut press = Press::None;
        let wrote = player::steer(clock::nanoseconds(), unsafe { &mut *axes }, &mut press);
        unsafe { pressed.write(press as c_int) };
        wrote
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_walking() -> bool {
        player::walking()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_record() {
        player::start_recording();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_saw_pad(buttons: u16, right_trigger: u8) {
        player::saw_pad(buttons, right_trigger);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_arrived() -> bool {
        player::arrived()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_seconds_since_progress() -> f64 {
        player::seconds_since_progress(clock::nanoseconds())
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_player_report() {
        player::report();
    }

    #[cfg(not(windows))]
    mod watch {
        use crate::runtime::watchpoint;

        #[unsafe(no_mangle)]
        extern "C" fn mw2_watchpoint_install() {
            watchpoint::install();
        }

        #[unsafe(no_mangle)]
        extern "C" fn mw2_watchpoint_suspend() {
            watchpoint::suspend();
        }

        #[unsafe(no_mangle)]
        extern "C" fn mw2_watchpoint_resume() {
            watchpoint::resume();
        }
    }
}

// The kernel, for the C++ that stays: the GPU and the audio (runtime/kernel/kernel.h).
mod kernel_exports {
    use crate::guest::{GuestCode, PPCContext};
    use crate::runtime::guest_memory;
    use crate::runtime::kernel;
    use std::ffi::{CStr, c_char, c_void};
    use std::mem::ManuallyDrop;

    type Release = unsafe extern "C" fn(object: *mut c_void);

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_kernel_initialise(game_root: *const c_char) {
        kernel::initialise(unsafe { CStr::from_ptr(game_root) });
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_allocate_guest(size: u32, alignment: u32) -> u32 {
        kernel::allocate_guest(size, alignment)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_allocate_physical(size: u32, alignment: u32) -> u32 {
        kernel::allocate_physical(size, alignment)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_free_guest(address: u32) -> bool {
        kernel::free_guest(address)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_free_physical(address: u32) -> bool {
        kernel::free_physical(address)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_report_memory() {
        kernel::report_memory();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_create_thread_block(thread_id: u32) -> u32 {
        kernel::create_thread_block(thread_id)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_new_thread_id() -> u32 {
        kernel::new_thread_id()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_update_time_stamp_bundle() {
        kernel::update_time_stamp_bundle();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_guest_function(address: u32) -> Option<GuestCode> {
        guest_memory::memory().function_at(address).map(|function| function.code())
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_deliver_user_apcs(caller: &PPCContext) -> bool {
        kernel::deliver_user_apcs(caller, guest_memory::memory())
    }

    // The C++'s work for an APC: run once, or else discarded with the thread's queue.
    struct Work {
        run: Release,
        discard: Release,
        work: *mut c_void,
    }

    impl Work {
        fn run(self) {
            let work = ManuallyDrop::new(self);
            unsafe { (work.run)(work.work) }
        }
    }

    impl Drop for Work {
        fn drop(&mut self) {
            unsafe { (self.discard)(self.work) }
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_queue_user_apc(
        routine: u32,
        context: u32,
        io_status_block: u32,
        run: Release,
        discard: Release,
        work: *mut c_void,
    ) {
        let work = (!work.is_null()).then(|| {
            let work = Work { run, discard, work };
            Box::new(move || work.run()) as kernel::Work
        });
        kernel::queue_user_apc(routine, context, io_status_block, work);
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_kernel_unimplemented(name: *const c_char) {
        kernel::unimplemented(unsafe { CStr::from_ptr(name) });
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_report_unimplemented() {
        kernel::report_unimplemented();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_report_missing_files() {
        kernel::files::report_missing_files();
    }

    /// The guest's timebase, which every mftb reads (ppc_context.h).
    #[unsafe(no_mangle)]
    extern "C" fn PPC_QueryTimebase() -> u64 {
        kernel::misc::timebase()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_report_waits() {
        #[cfg(feature = "diagnostics")]
        kernel::sync_objects::report_waits();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_audio_report() {
        kernel::report_audio();
    }

    // The XMA decoder (runtime/apu/xma.h): run.rs starts it and reports it, the guest's
    // stores into its register window come through runtime/mmio_hook.h, and FFmpeg's log
    // counts its errors (runtime/apu/xma_codec.cpp).
    #[unsafe(no_mangle)]
    extern "C" fn mw2_xma_initialise() {
        kernel::xma::initialise();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_xma_report() {
        kernel::xma::report();
    }

    /// What the recompiled code's stw would write; the hardware reads its registers
    /// little-endian, so the title wrote them with stwbrx.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_xma_register_store(address: u32, value: u32) {
        kernel::xma::register_store(address, value.swap_bytes());
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_xma_count_error() -> bool {
        kernel::xma::count_error()
    }
}

// The online service, for the C++ build's main.cpp (runtime/online/service.h): Steam's namespace, and the
// log-in before the title starts.
mod online {
    use crate::runtime::online;
    use std::ffi::c_int;

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_online_enter_namespace(exit_code: *mut c_int) -> bool {
        match online::enter_service_namespace() {
            Some(code) => {
                unsafe { *exit_code = code };
                false
            }
            None => true,
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_online_started() -> bool {
        online::get().is_some()
    }
}

// The screens the system draws over the title (runtime/signin.h, friends.h, guide.h).
mod screens {
    use super::{TextSink, give};
    use crate::runtime::screen::Image;
    use crate::runtime::{friends, guide, signin};
    use std::ffi::c_void;

    #[repr(C)]
    pub struct CImage {
        version: u64,
        width: u32,
        height: u32,
        outline: f32,
        pixels: *mut c_void,
        sink: TextSink,
    }

    // The picture as the presenter has it; the pixels go back only when drawn again.
    unsafe fn picture(window_height: u32, c: *mut CImage, draw: fn(u32, &mut Image) -> bool) -> bool {
        let c = unsafe { &mut *c };
        let mut image =
            Image { pixels: Vec::new(), width: c.width, height: c.height, version: c.version, outline: c.outline };
        if !draw(window_height, &mut image) {
            return false;
        }
        if !image.pixels.is_empty() {
            unsafe { give(c.pixels, c.sink, &image.pixels) };
        }
        (c.version, c.width, c.height, c.outline) = (image.version, image.width, image.height, image.outline);
        true
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_signin_at(
        user: u32,
        signed_in: *mut bool,
        id: *mut u64,
        name: *mut c_void,
        sink: TextSink,
    ) {
        let player = signin::at(user);
        unsafe {
            *signed_in = player.signed_in;
            *id = player.id;
            give(name, sink, &player.name);
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_signin_first(id: *mut u64, name: *mut c_void, sink: TextSink) {
        let player = signin::first();
        unsafe {
            *id = player.id;
            give(name, sink, &player.name);
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_signin_mask() -> u32 {
        signin::mask()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_signin_ask() {
        signin::ask();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_signin_input(user: u32, buttons: u16) -> bool {
        signin::input(user, buttons)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_signin_picture(window_height: u32, image: *mut CImage) -> bool {
        unsafe { picture(window_height, image, signin::picture) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_friends_ask() {
        friends::ask();
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_friends_input(user: u32, buttons: u16) -> bool {
        friends::input(user, buttons)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_friends_picture(window_height: u32, image: *mut CImage) -> bool {
        unsafe { picture(window_height, image, friends::picture) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_guide_input(user: u32, buttons: u16) -> bool {
        guide::input(user, buttons)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_guide_picture(window_height: u32, image: *mut CImage) -> bool {
        unsafe { picture(window_height, image, guide::picture) }
    }
}

// tools/hook_diff.py's: both sides of a case allocate from the same empty heap, and
// read the game folder it made.
#[cfg(mw2_hook_rig)]
#[unsafe(no_mangle)]
extern "C" fn mw2_rig_reset_heap() {
    crate::runtime::kernel::memory::VIRTUAL.reset();
}

#[cfg(mw2_hook_rig)]
#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_rig_set_game_root(root: *const c_char) {
    crate::runtime::kernel::files::set_game_root(unsafe { path_from(root) });
}
