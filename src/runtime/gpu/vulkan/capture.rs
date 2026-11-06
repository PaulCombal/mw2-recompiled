// RenderDoc, driven from inside the runtime (runtime/gpu/vulkan/capture.h says why the
// frame is chosen here): runtime/gpu/vulkan/capture.cpp in Rust.
//
// Loading RenderDoc is not this file's job. Run the title under
//
//     renderdoccmd capture -c <prefix> -- ./build/mw2
//
// which injects the library before the Vulkan instance exists; this only finds it
// already in the process and asks it for the frames wanted. MW2_RENDERDOC is read
// once, by whichever of the three entry points runs first.
//
// Unsafe by nature: RenderDoc's function table is declared by hand from
// third_party/renderdoc/renderdoc_app.h (RENDERDOC_API_1_4_1), and its pointer, which
// RenderDoc owns for the life of the process, is called from frame_boundary's thread
// only; the device pointer it is given is the first word of the live instance.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::env;
use crate::log::{log_info, log_warn};
use crate::runtime::{bridge, platform};
use std::ffi::{CStr, c_char, c_double, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// A capture is about 90 MB, so a key held on by mistake fills a disk in minutes. This
/// many is ~10 GB, and several seconds of play even at the few frames a second
/// capturing leaves.
const STREAM_LIMIT: u32 = 120;
const API_VERSION_1_4_1: c_int = 10401;

type Unused = *const c_void;

/// RENDERDOC_API_1_4_1, in renderdoc_app.h's order. Only what is called is typed.
#[repr(C)]
struct Api {
    get_api_version: Unused,
    set_capture_option_u32: Unused,
    set_capture_option_f32: Unused,
    get_capture_option_u32: Unused,
    get_capture_option_f32: Unused,
    set_focus_toggle_keys: Unused,
    set_capture_keys: Unused,
    get_overlay_bits: Unused,
    mask_overlay_bits: Unused,
    remove_hooks: Unused,
    unload_crash_handler: Unused,
    set_capture_file_path_template: unsafe extern "C" fn(template: *const c_char),
    get_capture_file_path_template: unsafe extern "C" fn() -> *const c_char,
    get_num_captures: Unused,
    get_capture: Unused,
    trigger_capture: Unused,
    is_target_control_connected: Unused,
    launch_replay_ui: Unused,
    set_active_window: Unused,
    start_frame_capture: unsafe extern "C" fn(device: *mut c_void, window: *mut c_void),
    is_frame_capturing: Unused,
    end_frame_capture: unsafe extern "C" fn(device: *mut c_void, window: *mut c_void) -> u32,
}
const _: () = assert!(std::mem::offset_of!(Api, end_frame_capture) == 21 * std::mem::size_of::<usize>());

type GetApi = unsafe extern "C" fn(version: c_int, api: *mut *mut c_void) -> c_int;

unsafe extern "C" {
    fn strtod(text: *const c_char, end: *mut *mut c_char) -> c_double;
    fn strtoul(text: *const c_char, end: *mut *mut c_char, base: c_int) -> c_ulong;
}

#[derive(Clone, Copy, PartialEq)]
enum Why {
    Timer,
    Hand,
    Stream,
}

/// MW2_RENDERDOC=<seconds>[:<frames>]. Zero seconds captures the first world frame
/// there is, which on a level is during the load.
#[derive(Clone, Copy)]
struct Request {
    asked: bool,
    after: f64,
    wanted: u32,
}

fn diag_text(name: &CStr) -> Option<&'static CStr> {
    if cfg!(feature = "diagnostics") { env::text(name) } else { None }
}

fn request() -> Request {
    static REQUEST: OnceLock<Request> = OnceLock::new();
    *REQUEST.get_or_init(|| {
        let mut request = Request { asked: false, after: 0.0, wanted: 4 };
        let Some(value) = diag_text(c"MW2_RENDERDOC") else { return request };
        request.asked = true;
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: the environment's string, which ends in its zero; `end` points into it.
        unsafe {
            request.after = strtod(value.as_ptr(), &mut end);
            if !end.is_null() && *end == b':' as c_char {
                let n = strtoul(end.add(1), std::ptr::null_mut(), 10) as u32;
                if n != 0 {
                    request.wanted = n;
                }
            }
        }
        request
    })
}

struct State {
    read: bool,  // the request is copied in
    asked: bool, // MW2_RENDERDOC was set
    after: f64,  // seconds of run time to wait first
    wanted: u32, // how many frames to capture
    taken: u32,
    looked: bool, // we have tried to find RenderDoc
    open: bool,   // a capture is in flight
    api: *const Api,
    refused: Option<&'static CStr>, // why there is no api
    device: *mut c_void,            // the instance's dispatch table pointer
    streamed: bool,                 // the last boundary saw `streaming` set
    in_stream: u32,                 // frames captured since it was set
    why: Why,
}

// SAFETY: the pointers are RenderDoc's table and the instance's dispatch pointer, both
// valid for the life of the process and only used under the state's lock.
unsafe impl Send for State {}

static STATE: Mutex<State> = Mutex::new(State {
    read: false,
    asked: false,
    after: 0.0,
    wanted: 4,
    taken: 0,
    looked: false,
    open: false,
    api: std::ptr::null(),
    refused: None,
    device: std::ptr::null_mut(),
    streamed: false,
    in_stream: 0,
    why: Why::Timer,
});
// Frames a keypress has asked for, and whether the one in flight is one of them.
// Written from the presenter thread, read from the one that presents.
static DEMANDED: AtomicU32 = AtomicU32::new(0);
// Every world frame while this is set, for a defect that comes and goes between
// frames. Toggled from the presenter thread like the above.
static STREAMING: AtomicBool = AtomicBool::new(false);

fn state() -> MutexGuard<'static, State> {
    let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.read {
        let request = request();
        (state.asked, state.after, state.wanted, state.read) = (request.asked, request.after, request.wanted, true);
    }
    state
}

fn due(after: f64) -> bool {
    if after <= 0.0 {
        return true;
    }
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() >= after
}

fn text(pointer: *const c_char) -> &'static CStr {
    if pointer.is_null() {
        return c"";
    }
    // SAFETY: RenderDoc's own string, which it keeps.
    unsafe { CStr::from_ptr(pointer) }
}

// The library already in the process: if the injector did not put RenderDoc here,
// loading it now would be too late anyway -- it hooks vkCreateInstance, and by the
// time a frame is presented the instance is long since made.
fn find(state: &mut State) {
    state.looked = true;
    let library = platform::loaded_library(if cfg!(windows) { c"renderdoc.dll" } else { c"librenderdoc.so" });
    if library.is_null() {
        state.refused = Some(c"RenderDoc is not in this process -- run under renderdoccmd capture");
        return;
    }
    // SAFETY: the loaded library's export of that name.
    let get = unsafe { platform::symbol(library, c"RENDERDOC_GetAPI") };
    if get.is_null() {
        state.refused = Some(c"RenderDoc's library has no RENDERDOC_GetAPI");
        return;
    }
    // SAFETY: RENDERDOC_GetAPI is a pRENDERDOC_GetAPI.
    let get: GetApi = unsafe { std::mem::transmute::<*mut c_void, GetApi>(get) };
    let mut api: *mut c_void = std::ptr::null_mut();
    // SAFETY: RenderDoc writes its table's address for the version asked for.
    if unsafe { get(API_VERSION_1_4_1, &mut api) } == 0 || api.is_null() {
        state.refused = Some(c"RenderDoc refused API version 1.4.1");
        return;
    }
    let instance = bridge::vulkan::instance();
    if instance.is_null() {
        state.refused = Some(c"no Vulkan instance to capture from");
        return;
    }
    state.api = api.cast_const().cast();
    // Named rather than left to RenderDoc to find: the device that draws is the one on
    // the pipeline's instance. RENDERDOC_DEVICEPOINTER_FROM_VKINSTANCE: its first word.
    // SAFETY: a dispatchable handle points at its dispatch table's pointer.
    state.device = unsafe { *instance.cast::<*mut c_void>() };
    // SAFETY: RenderDoc's table for 1.4.1.
    let api = unsafe { &*state.api };
    if let Some(out) = diag_text(c"MW2_RENDERDOC_OUT") {
        // SAFETY: a C string, which RenderDoc copies.
        unsafe { (api.set_capture_file_path_template)(out.as_ptr()) };
    }
    // SAFETY: RenderDoc's call, which returns its own string.
    let template = text(unsafe { (api.get_capture_file_path_template)() });
    log_info!("capture: RenderDoc %s, writing to %s", c"1.4.1", template);
}

/// Called once per present, after the frame has been submitted. `world_drawn` says
/// whether this frame drew the world, so a capture is not spent on the interface over
/// what EDRAM already held; `frame` is the renderer's frame count, the number its log
/// lines carry.
pub fn frame_boundary(world_drawn: bool, frame: u64) {
    let mut s = state();
    // No early return on `!asked`: that is only the timed request, and the keys have to
    // get through without MW2_RENDERDOC set. The tests below stop every frame that
    // nothing asked for.

    if s.open {
        // One capture per frame: a capture spanning several is harder to read than
        // several files, and the frames are not alike.
        // SAFETY: the table find() took, with a capture in flight on that device.
        if unsafe { ((*s.api).end_frame_capture)(s.device, std::ptr::null_mut()) } == 0 {
            log_warn!("capture: RenderDoc discarded the capture of frame %u", s.taken);
        }
        s.open = false;
        s.taken += 1;
        match s.why {
            Why::Hand => {
                DEMANDED.fetch_sub(1, Ordering::Relaxed);
                log_info!("capture: frame %u written by hand", s.taken);
            }
            Why::Stream => {
                s.in_stream += 1;
                if s.in_stream >= STREAM_LIMIT {
                    STREAMING.store(false, Ordering::Relaxed);
                    log_warn!("capture: F11 stream stopped at its limit of %u frames", STREAM_LIMIT);
                }
            }
            Why::Timer => {
                if s.taken >= s.wanted {
                    log_info!("capture: %u frame(s) captured, done", s.taken);
                    s.asked = false;
                }
            }
        }
    }

    let streaming = STREAMING.load(Ordering::Relaxed);
    if streaming != s.streamed {
        if streaming {
            s.in_stream = 0;
        } else {
            log_info!("capture: F11 stream ended, %u frame(s) written", s.in_stream);
        }
        s.streamed = streaming;
    }

    // A keypress outranks the timer, and outlives the timed budget: the point of it is
    // to catch a moment nobody could have named in advance.
    let by_hand = DEMANDED.load(Ordering::Relaxed) != 0;
    if !by_hand && !streaming && (!s.asked || s.taken >= s.wanted) {
        return;
    }
    // A stream takes every frame. Whether the frame that just ended drew the world says
    // nothing reliable about the next one -- presents do not line up with the title's
    // frames -- and skipping on it left world frames out of a stream, which is where a
    // flicker went to hide.
    if !world_drawn && !streaming {
        return;
    }
    if !by_hand && !streaming && !due(s.after) {
        return;
    }
    if !s.looked {
        find(&mut s);
    }
    if s.api.is_null() {
        // Say it at the keypress, not only in the shutdown report: somebody is standing
        // there pressing a key and deserves to know it did nothing.
        if by_hand || streaming {
            let refused = s.refused.unwrap_or(c"RenderDoc is not available");
            log_warn!("capture: %s cannot capture -- %s", if by_hand { c"F10" } else { c"F11" }, refused);
        }
        s.asked = false;
        DEMANDED.store(0, Ordering::Relaxed);
        STREAMING.store(false, Ordering::Relaxed);
        s.streamed = false;
        return;
    }

    // SAFETY: the table find() took, on the device it named.
    unsafe { ((*s.api).start_frame_capture)(s.device, std::ptr::null_mut()) };
    s.open = true;
    s.why = if by_hand {
        Why::Hand
    } else if streaming {
        Why::Stream
    } else {
        Why::Timer
    };
    // RenderDoc numbers the files of a run in order -- the first has no suffix, then _2,
    // _3 -- so this is the file's own number.
    log_info!("capture: file %u records renderer frame %llu", s.taken + 1, frame);
}

/// Capture every frame that draws the world from now until the next call, one file
/// each, up to a limit that keeps the disk alive.
pub fn toggle_stream() {
    request();
    let now = !STREAMING.load(Ordering::Relaxed);
    STREAMING.store(now, Ordering::Relaxed);
    if now {
        log_info!("capture: F11 -- capturing every frame until F11 again (at most %u)", STREAM_LIMIT);
    } else {
        log_info!("capture: F11 -- stopping after the frame in flight");
    }
}

/// Ask for the next frames that draw the world, whatever the timer says. Bound to a key
/// in the presenter, and safe to call from that thread.
pub fn request_now(frames: u32) {
    request();
    let frames = if frames != 0 { frames } else { 1 };
    DEMANDED.fetch_add(frames, Ordering::Relaxed);
    log_info!(
        "capture: F10 -- %u frame(s) asked for; they are written when the next frames that draw the world finish",
        frames
    );
}

/// For the shutdown report: how many captures were taken, and why none were when none
/// were.
pub fn report() {
    let s = state();
    if !s.looked && s.taken == 0 {
        return;
    }
    match s.refused {
        Some(refused) => log_warn!("capture: no RenderDoc capture -- %s", refused),
        None => log_info!("capture: %u RenderDoc capture(s) written", s.taken),
    }
}
