// XAudio and XMA -- the kernel's side of sound. The mixer itself is XAudio2, statically
// linked into the title and so already recompiled; what it needs from the kernel is a
// render driver to pace it and hand its frames to, and hardware decoder contexts for its
// XMA voices (xma.rs).
//
// The render driver: the title registers a mixer callback once, and the console calls it
// every 256 samples at 48 kHz (5.333 ms); it hands back a frame of six channels of 256
// big-endian floats through XAudioSubmitRenderDriverFrame. This runs that cadence on a
// thread of its own and pushes the frames to SDL.
//
// Pacing: on the console the audio hardware's clock calls the driver, a frame for every
// 256 samples it plays. Here the playback device's clock does: each time the device takes
// sound from SDL's queue, SDL tells the worker (a get callback) how much it took and how
// much is left, and the worker calls the title's callback until the queue again holds a
// take and three frames more -- the device's own demand, whatever its clock's drift from
// ours, with the margin a late worker needs (LEAD_FRAMES). Without a device the wall clock
// calls it at the nominal rate, because the title's sound state machines advance inside it
// and a silent run must still finish its sounds.

use super::misc::ERROR_SUCCESS;
use super::xma;
use super::{STATUS_NO_MEMORY, STATUS_SUCCESS, allocate_guest, create_thread_block, new_thread_id};
use crate::env;
use crate::guest::{self, Memory, Register, kernel_import};
use crate::log::{log_error, log_info, log_kernel, log_warn};
use crate::runtime::{crash, guest_memory};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const E_INVALIDARG: u32 = 0x8007_0057;

const FREQUENCY: u32 = 48000;
const CHANNELS: usize = 6;
const SAMPLES_PER_CHANNEL: usize = 256;
const FRAME_FLOATS: usize = CHANNELS * SAMPLES_PER_CHANNEL;
const FRAME_BYTES: usize = FRAME_FLOATS * 4;
const FRAME_INTERVAL: Duration = Duration::from_micros(5333);

// Beyond a take and the lead, the host queue drops frames past this many (128 ms of
// frames of 5.333 ms): a guard for a device that stalls with its queue full, which device
// pacing does not otherwise reach.
const HIGH_WATER_FRAMES: usize = 24;

const MAX_CLIENTS: usize = 8;
const HANDLE_TAG: u32 = 0x4155_0000;
// Twice what a guest thread gets: the mixer's frames are deep, and an overflow here lands
// in whichever allocation sits below the stack.
const STACK_SIZE: u32 = 1024 * 1024;

#[derive(Clone, Copy)]
struct Client {
    callback: u32,
    context: u32,
    next: Instant,
}

struct Clients {
    slots: [Option<Client>; MAX_CLIENTS],
    worker_started: bool,
}

static CLIENTS: Mutex<Clients> = Mutex::new(Clients { slots: [None; MAX_CLIENTS], worker_started: false });
static WAKE: Condvar = Condvar::new();

fn clients() -> MutexGuard<'static, Clients> {
    CLIENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

// The audio thread's, for the report.
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static UNDERRUNS: AtomicU64 = AtomicU64::new(0);
// Clients registered: a take with none has nothing to wait for, as at the run's end.
static REGISTERED: AtomicU32 = AtomicU32::new(0);
static OUTPUT_OPEN: AtomicBool = AtomicBool::new(false);

// The device's takes, from SDL's thread to the worker: what the queue held after the last
// one and its size, and how much the takes since the worker last looked asked for. SDL
// calls with its stream locked, so this lock is taken after SDL's and the worker holds it
// for nothing else.
struct Takes {
    pending: bool,
    left: usize,
    last: usize,
    asked: usize,
}

static TAKES: Mutex<Takes> = Mutex::new(Takes { pending: false, left: 0, last: 0, asked: 0 });
static TAKEN: Condvar = Condvar::new();
// The last take's size, for submit_frame's limit.
static LAST_TAKE: AtomicUsize = AtomicUsize::new(0);

// What the queue keeps beyond a take for the worker to refill in time, in frames. The
// worker answers a take by waking the title's mixer thread and waiting for its mix, so the
// margin is how late those two threads may be scheduled: under Wine with every processor
// busy, one frame (5.3 ms) underran about 15 times a minute and two never did; three leave
// room for a slower machine, at 26 ms of latency with WASAPI's 480-sample period.
const LEAD_FRAMES: usize = 3;

// SDL's thread: the device is about to take `asked` bytes from a queue holding `queued`;
// `short` of them are not there, and become silence.
fn device_takes(queued: usize, asked: usize, short: usize) {
    if short > 0 && FRAMES.load(Ordering::Relaxed) > 0 && REGISTERED.load(Ordering::Relaxed) > 0 {
        UNDERRUNS.fetch_add(1, Ordering::Relaxed);
    }
    let mut takes = TAKES.lock().unwrap_or_else(PoisonError::into_inner);
    takes.pending = true;
    takes.left = queued.saturating_sub(asked);
    takes.last = asked;
    takes.asked = takes.asked.saturating_add(asked);
    drop(takes);
    LAST_TAKE.store(asked, Ordering::Relaxed);
    TAKEN.notify_one();
}

// What of SDL this asks: one playback stream.
#[allow(unsafe_code)]
mod sdl {
    use super::{CHANNELS, FREQUENCY};
    use crate::log::{log_info, log_warn};
    use sdl3_sys::everything as sdl;
    use std::ffi::{CStr, c_int, c_void};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// The stream, once open. SDL's stream calls may come from any thread.
    pub struct Stream(*mut sdl::SDL_AudioStream);
    unsafe impl Send for Stream {}

    static STREAM: Mutex<Option<Stream>> = Mutex::new(None);

    pub fn stream() -> MutexGuard<'static, Option<Stream>> {
        STREAM.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn text(pointer: *const std::ffi::c_char) -> std::ffi::CString {
        if pointer.is_null() { std::ffi::CString::default() } else { unsafe { CStr::from_ptr(pointer) }.to_owned() }
    }

    fn error() -> std::ffi::CString {
        text(sdl::SDL_GetError())
    }

    fn driver() -> std::ffi::CString {
        text(unsafe { sdl::SDL_GetCurrentAudioDriver() })
    }

    // Called by SDL's device thread before each take, with the stream locked (SDL's
    // mutexes are reentrant, so the stream can be asked what it holds).
    unsafe extern "C" fn taking(_: *mut c_void, stream: *mut sdl::SDL_AudioStream, short: c_int, asked: c_int) {
        let queued = usize::try_from(unsafe { sdl::SDL_GetAudioStreamQueued(stream) }).unwrap_or(0);
        super::device_takes(queued, usize::try_from(asked).unwrap_or(0), usize::try_from(short).unwrap_or(0));
    }

    /// Opens the default playback device for 5.1 float at 48 kHz; false without one.
    pub fn open() -> bool {
        if !unsafe { sdl::SDL_InitSubSystem(sdl::SDL_INIT_AUDIO) } {
            log_warn!("audio: SDL audio would not start (%s)", error().as_c_str());
            return false;
        }
        let spec =
            sdl::SDL_AudioSpec { format: sdl::SDL_AUDIO_F32, channels: CHANNELS as c_int, freq: FREQUENCY as c_int };
        let mut held = stream();
        let opened = unsafe {
            sdl::SDL_OpenAudioDeviceStream(sdl::SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, None, std::ptr::null_mut())
        };
        if opened.is_null() {
            log_warn!("audio: no playback device (%s); the driver is %s", error().as_c_str(), driver().as_c_str());
            return false;
        }
        let mut device = sdl::SDL_AudioSpec { format: sdl::SDL_AUDIO_F32, channels: 0, freq: 0 };
        let mut device_frames: c_int = 0;
        unsafe {
            sdl::SDL_GetAudioDeviceFormat(sdl::SDL_GetAudioStreamDevice(opened), &mut device, &mut device_frames);
            sdl::SDL_SetAudioStreamGetCallback(opened, Some(taking), std::ptr::null_mut());
            sdl::SDL_ResumeAudioStreamDevice(opened);
        }
        *held = Some(Stream(opened));
        log_info!(
            "audio: %s, device %d Hz %d ch, %d-sample buffers; feeding 5.1 float at 48 kHz",
            driver().as_c_str(),
            device.freq,
            device.channels,
            device_frames
        );
        true
    }

    impl Stream {
        /// Bytes the host still holds.
        pub fn queued(&self) -> usize {
            usize::try_from(unsafe { sdl::SDL_GetAudioStreamQueued(self.0) }).unwrap_or(0)
        }

        pub fn put(&self, samples: &[f32]) -> Result<(), std::ffi::CString> {
            let size = std::mem::size_of_val(samples) as c_int;
            if unsafe { sdl::SDL_PutAudioStreamData(self.0, samples.as_ptr().cast(), size) } {
                Ok(())
            } else {
                Err(error())
            }
        }
    }
}

// MW2_NO_AUDIO=1: no playback device; the mixer and the decoder still run.
fn open_output() {
    if env::flag(c"MW2_NO_AUDIO") {
        log_info!("audio: output disabled (MW2_NO_AUDIO)");
        return;
    }
    if sdl::open() {
        OUTPUT_OPEN.store(true, Ordering::Relaxed);
    }
}

// The worker owns a guest stack and thread block: the callback is guest code and reads
// its thread id out of r13 like any other.
struct GuestCaller {
    stack_top: u32,
    pcr: u32,
}

impl GuestCaller {
    fn prepare() -> Option<Self> {
        let stack_base = allocate_guest(STACK_SIZE, 0x1000);
        let pcr = create_thread_block(new_thread_id());
        (stack_base != 0 && pcr != 0)
            .then(|| Self { stack_top: stack_base.wrapping_add(STACK_SIZE).wrapping_sub(0x100) & !0xF, pcr })
    }

    fn call(&self, memory: Memory, address: u32, a0: u32) {
        static WARNED: AtomicBool = AtomicBool::new(false);
        let Some(function) = memory.function_at(address) else {
            if !WARNED.swap(true, Ordering::Relaxed) {
                log_warn!("audio: no recompiled function at callback %08X", address);
            }
            return;
        };
        let mut ctx = guest::blank_context();
        ctx.r1.u64 = u64::from(self.stack_top);
        ctx.r13.u64 = u64::from(self.pcr);
        ctx.r3.u64 = u64::from(a0);
        guest::load_float_mode(&mut ctx);
        function.call(&mut ctx, memory);
    }
}

fn worker() {
    crash::register_thread(c"audio worker");
    let Some(caller) = GuestCaller::prepare() else {
        log_error!("audio: could not set up the worker's guest state");
        return;
    };
    open_output();
    let memory = guest_memory::memory();
    if OUTPUT_OPEN.load(Ordering::Relaxed) {
        paced_by_the_device(&caller, memory);
    } else {
        paced_by_the_clock(&caller, memory);
    }
}

// One call of every client whose first callback is due, each with the context it
// registered (the title's own object); false when none is yet.
fn call_clients(caller: &GuestCaller, memory: Memory) -> bool {
    let now = Instant::now();
    let ready = {
        let held = clients();
        held.slots.map(|slot| slot.filter(|client| client.next <= now).map(|client| (client.callback, client.context)))
    };
    let mut called = false;
    for (callback, context) in ready.into_iter().flatten() {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        caller.call(memory, callback, context);
        called = true;
    }
    called
}

// After each take, the callback until the queue holds that take and the lead again, the
// size of a take being the device's to change (another device, another period). A take
// that comes while the worker is still calling is answered with what the queue then holds,
// which counts the frames already put, so a call that submits refills the queue up to the
// target and no further. A call that submits nothing is paid for by what the device asked,
// a credit of its bytes capped at the target, so a title that submits nothing runs at the
// device's rate, not faster.
fn paced_by_the_device(caller: &GuestCaller, memory: Memory) {
    let mut credit = usize::MAX;
    loop {
        let (left, last, asked) = {
            let mut takes = TAKES.lock().unwrap_or_else(PoisonError::into_inner);
            while !takes.pending {
                takes = TAKEN.wait(takes).unwrap_or_else(PoisonError::into_inner);
            }
            takes.pending = false;
            (takes.left, takes.last, std::mem::take(&mut takes.asked))
        };
        let target = last.saturating_add(LEAD_FRAMES * FRAME_BYTES);
        credit = credit.saturating_add(asked).min(target);
        let frames = target.saturating_sub(left).div_ceil(FRAME_BYTES).min(credit.div_ceil(FRAME_BYTES));
        for _ in 0..frames {
            let submitted = FRAMES.load(Ordering::Relaxed);
            if !call_clients(caller, memory) {
                break;
            }
            if FRAMES.load(Ordering::Relaxed) == submitted {
                credit = credit.saturating_sub(FRAME_BYTES);
            }
        }
    }
}

// Without a device: every client on its own 5.333 ms schedule.
fn paced_by_the_clock(caller: &GuestCaller, memory: Memory) {
    loop {
        let (callback, context) = {
            let mut held = clients();
            let Some(at) = (0..MAX_CLIENTS)
                .filter(|&i| held.slots[i].is_some())
                .min_by_key(|&i| held.slots[i].map(|client| client.next))
            else {
                drop(WAKE.wait(held).unwrap_or_else(PoisonError::into_inner));
                continue;
            };
            let pick = held.slots[at].expect("picked");
            let now = Instant::now();
            let mut due = pick.next;
            // Behind by a lot (the process was stopped): resynchronise rather than call a
            // burst.
            if due + FRAME_INTERVAL * 40 < now {
                due = now;
            }
            if due > now {
                drop(WAKE.wait_timeout(held, due - now).unwrap_or_else(PoisonError::into_inner));
                continue;
            }
            let next = due.max(now.checked_sub(FRAME_INTERVAL).unwrap_or(now)) + FRAME_INTERVAL;
            held.slots[at] = Some(Client { next, ..pick });
            (pick.callback, pick.context)
        };
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        // The callback takes the context it registered: the title's own object.
        caller.call(memory, callback, context);
    }
}

/// The driver handle, or 0 when no client slot is free.
fn register_client(callback: u32, context: u32) -> u32 {
    let mut held = clients();
    let Some(at) = held.slots.iter().position(Option::is_none) else {
        log_warn!("audio: no free client slot");
        return 0;
    };
    // The first callback only once the registering call has returned and the title has
    // finished setting its object up.
    held.slots[at] = Some(Client { callback, context, next: Instant::now() + Duration::from_millis(100) });
    REGISTERED.fetch_add(1, Ordering::Relaxed);
    if !held.worker_started {
        held.worker_started = true;
        std::thread::Builder::new()
            .name("audio worker".into())
            .stack_size(super::threads::host_stack_size())
            .spawn(worker)
            .expect("the audio worker's host thread");
    }
    WAKE.notify_all();
    log_kernel!("audio: client %u registered, callback %08X context %08X", at as u32, callback, context);
    HANDLE_TAG | at as u32
}

fn slot_of(handle: u32) -> Option<usize> {
    let at = (handle & 0xFFFF) as usize;
    (handle & 0xFFFF_0000 == HANDLE_TAG && at < MAX_CLIENTS).then_some(at)
}

fn unregister_client(handle: u32) {
    let Some(at) = slot_of(handle) else { return };
    let mut held = clients();
    if held.slots[at].take().is_some() {
        REGISTERED.fetch_sub(1, Ordering::Relaxed);
        log_kernel!("audio: client %u unregistered", at as u32);
    }
}

fn submit_frame(memory: Memory, handle: u32, samples: u32) {
    if slot_of(handle).is_none() || samples == 0 {
        return;
    }
    FRAMES.fetch_add(1, Ordering::Relaxed);
    let held = sdl::stream();
    let Some(stream) = held.as_ref() else { return };
    let limit = (HIGH_WATER_FRAMES + LEAD_FRAMES) * FRAME_BYTES + LAST_TAKE.load(Ordering::Relaxed);
    if stream.queued() > limit {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Six planes of 256 big-endian floats to interleaved host floats.
    let mut out = [0f32; FRAME_FLOATS];
    for s in 0..SAMPLES_PER_CHANNEL {
        for c in 0..CHANNELS {
            let at = samples.wrapping_add(((c * SAMPLES_PER_CHANNEL + s) * 4) as u32);
            out[s * CHANNELS + c] = f32::from_bits(memory.u32_at(at));
        }
    }
    static WARNED: AtomicBool = AtomicBool::new(false);
    if let Err(error) = stream.put(&out)
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        log_warn!("audio: put failed (%s)", error.as_c_str());
    }
}

/// The run's counts, at its end.
pub fn report() {
    let callbacks = CALLBACKS.load(Ordering::Relaxed);
    if callbacks == 0 {
        return;
    }
    log_info!(
        "audio: %llu callbacks, %llu frames submitted, %llu dropped, %llu underruns; %s",
        callbacks,
        FRAMES.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
        UNDERRUNS.load(Ordering::Relaxed),
        if OUTPUT_OPEN.load(Ordering::Relaxed) { c"paced by the device" } else { c"no device, paced by the clock" }
    );
}

// XAudioRegisterRenderDriverClient(const struct { callback; context; }* client, uint32_t* driverOut)
kernel_import!(XAudioRegisterRenderDriverClient, |ctx, memory| {
    let (client, driver_out) = (ctx.r3.as_u32(), ctx.r4.as_u32());
    let callback = if client != 0 { memory.u32_at(client) } else { 0 };
    if callback == 0 {
        ctx.r3.u64 = u64::from(E_INVALIDARG);
        return;
    }
    let handle = register_client(callback, memory.u32_at(client.wrapping_add(4)));
    if handle == 0 {
        ctx.r3.u64 = u64::from(E_INVALIDARG);
        return;
    }
    if driver_out != 0 {
        memory.store_u32(driver_out, handle);
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XAudioUnregisterRenderDriverClient, |ctx, _memory| {
    unregister_client(ctx.r3.as_u32());
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// XAudioSubmitRenderDriverFrame(driver, const float samples[6][256])
kernel_import!(XAudioSubmitRenderDriverFrame, |ctx, memory| {
    submit_frame(memory, ctx.r3.as_u32(), ctx.r4.as_u32());
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// The dashboard's per-category volumes; there is no dashboard, so full.
kernel_import!(XAudioGetVoiceCategoryVolume, |ctx, memory| {
    let out = ctx.r4.as_u32();
    if out != 0 {
        memory.store_u32(out, 1.0f32.to_bits());
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// The mixer only fetches a category's volume when this says it changed, and its object
// starts out zeroed, so the first query has to report every category changed or every
// voice is scaled by zero for the whole run.
kernel_import!(XAudioGetVoiceCategoryVolumeChangeMask, |ctx, memory| {
    static QUERIES: AtomicU32 = AtomicU32::new(0);
    let out = ctx.r4.as_u32();
    if out != 0 {
        memory.store_u32(out, if QUERIES.fetch_add(1, Ordering::Relaxed) == 0 { u32::MAX } else { 0 });
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// XMACreateContext(void** contextOut)
kernel_import!(XMACreateContext, |ctx, memory| {
    let context = xma::allocate_context();
    let out = ctx.r3.as_u32();
    if out != 0 {
        memory.store_u32(out, context);
    }
    ctx.r3.u64 = u64::from(if context != 0 { STATUS_SUCCESS } else { STATUS_NO_MEMORY });
});

kernel_import!(XMAReleaseContext, |ctx, _memory| {
    xma::release_context(ctx.r3.as_u32());
    ctx.r3.u64 = 0;
});
