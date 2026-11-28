// Shaders compiled when the title loads them, and pipelines linked from them.
//
// On the console a shader is ready the moment the title loads it: the GPU runs the
// microcode as it is, and whatever it is drawn with -- blend, depth test, target -- is
// register state that costs nothing to change. Here a shader has to be compiled for the
// host GPU, and a Vulkan pipeline is both shaders plus that state; built at the first draw,
// it was ~12 ms of the frame, a stutter every time something new came on screen.
//
// So the work is split the way VK_EXT_graphics_pipeline_library allows, and done when the
// console does it:
//   - every shader the title makes while loading a level (hooks/shader_preload.rs hooks
//     D3D's shader objects) is translated and compiled on a worker thread, as a library for
//     its stage -- pre-rasterisation for a vertex shader, fragment shader for a pixel one.
//     Nothing in either depends on how it will be drawn: the render target formats live in
//     the output library (dynamic rendering), culling and the depth and stencil tests are
//     dynamic state;
//   - the input assembly and the output stage are libraries of state alone, one per
//     topology and one per blend, mask, formats and sample count -- no shader in them,
//     cheap to make;
//   - a draw links the four, which takes well under a millisecond, and the same four are
//     linked again with link-time optimisation on a thread of its own, the result taking
//     the fast link's place when it is ready.
// A device without pipeline libraries gets whole pipelines, compiled at the first draw
// (renderer_pipelines.rs).
//
// The preparation queue and the optimiser's are statics: the title's loader threads queue
// shaders (prepare_shader) before the renderer exists.
// Each is a Mutex with its condvars; every wait is a condvar wait on its own
// predicate. The threads' handles are the renderer's (Threads, in Pipelines), and
// stop_preparing's Joined is what Pipelines::destroy takes: the libraries the optimiser
// links from go only after it has been joined.
//
// Unsafe by nature: it calls Vulkan (device.rs's contract) and the shared shader build
// (shaders/src/shader_build.rs). The optimiser and the shader workers hold the renderer's Vk
// by pointer, as the recorder does (start_preparing's contract).
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::renderer_pipelines::{PipelineState, Pipelines, Shader, shader_cache_path};
use super::renderer_state::{OutputKey, PipelineKey};
#[cfg(feature = "diagnostics")]
use super::trace;
use crate::log::log_info;
use crate::runtime::gpu::hash::{MulBuild, shader_hash};
use crate::runtime::{crash, platform, report};
use ash::vk;
#[cfg(feature = "diagnostics")]
use ash::vk::Handle;
use mw2_shaders::shader_build::build;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub fn layout() -> vk::PipelineLayout {
    mw2_shaders::pipeline::layout()
}

pub fn cache() -> vk::PipelineCache {
    mw2_shaders::pipeline::cache()
}

pub fn pipeline_libraries() -> bool {
    mw2_shaders::pipeline::pipeline_libraries()
}

/// A library of one part of a pipeline, through the launcher's own code
/// (shader_build.rs's make_library).
fn make_library(
    part: vk::GraphicsPipelineLibraryFlagsEXT,
    info: &vk::GraphicsPipelineCreateInfo,
    next: *const c_void,
) -> vk::Pipeline {
    // SAFETY: `info` and `next` (with what they point at) are valid for the call; the
    // shared code copies the info and chains `next` behind its own library info.
    unsafe { mw2_shaders::shader_build::make_library(part, None, *info, next) }
}

/// The clock: the steady clock (runtime/clock.rs).
pub mod time {

    /// Nanoseconds on the steady clock.
    #[inline]
    pub fn steady() -> u64 {
        crate::runtime::clock::nanoseconds()
    }

    /// Whole milliseconds on the steady clock.
    pub fn milliseconds() -> i64 {
        (steady() / 1_000_000) as i64
    }

    /// The pacing trace's clock (pacing_trace.rs).
    pub fn pacing() -> i64 {
        crate::runtime::pacing_trace::microseconds(crate::runtime::clock::nanoseconds())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // panic=abort: a lock is never poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- the title's frames, which hold the compiling back -------------------------------------

// The workers compile while the title loads and stand still while it plays. They compile
// far more than a level draws with -- all 2156 shaders mp_afghan loads, against the 278 its
// route draws -- and on a cold driver cache that is 17 s of CPU. Running into play, even at
// idle priority, it slowed the title itself (47 frames it presented late in a minute; the
// cores, their clocks and caches are shared). A shader wanted in play before a worker
// reached it is compiled at the draw; a warm cache makes the whole load's
// worth half a second.
static LAST_WORLD_FRAME: AtomicI64 = AtomicI64::new(0);
static WORLD_RUN: AtomicU32 = AtomicU32::new(0);

/// 30 world frames in a row, the last under half a second ago.
fn playing() -> bool {
    WORLD_RUN.load(Ordering::Relaxed) >= 30 && time::milliseconds() - LAST_WORLD_FRAME.load(Ordering::Relaxed) < 500
}

// Checked again this often while play holds the work back.
const RECHECK: Duration = Duration::from_millis(100);

/// Every finished frame, so the compiling waits while the world is drawn.
pub fn note_frame(world: bool) {
    if !world {
        return;
    }
    let now = time::milliseconds();
    // Half a second without the world is a loading screen or a menu.
    if now - LAST_WORLD_FRAME.load(Ordering::Relaxed) >= 500 {
        WORLD_RUN.store(0, Ordering::Relaxed);
    }
    WORLD_RUN.fetch_add(1, Ordering::Relaxed);
    LAST_WORLD_FRAME.store(now, Ordering::Relaxed);
}

// The libraries built, by whether the driver found them in the cache: it does not say, it
// compiled the shader, it found it.
static FROM_CACHE: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

// ---- the shaders handed over at load -------------------------------------------------------

struct Job {
    pixel: bool,
    hash: u64,
    code: Vec<u32>,
}

// The shaders handed over at load. The consumer thread takes them out; the workers put them
// in; prepare_shader queues them from the title's loader thread.
struct PrepState {
    jobs: VecDeque<Job>,
    known: HashSet<u64, MulBuild>, // every shader queued or built
    busy: HashSet<u64, MulBuild>,  // being compiled now
    ready: HashMap<u64, Shader, MulBuild>,
    running: bool,
    stopped: bool,
    handed: u64,
    compiled: u64,
    taken: u64,
    waited: u64,
    stolen: u64,
    compile_microseconds: u64,
    wait_microseconds: u64,
    compiled_in_play: u64,
    drained_at: i64,
    // What the workers build with (the renderer's scale above 1, and its depth format),
    // set when they start.
    scaled: bool,
    depth_format: vk::Format,
}

impl PrepState {
    const fn new() -> PrepState {
        PrepState {
            jobs: VecDeque::new(),
            known: HashSet::with_hasher(BuildHasherDefault::new()),
            busy: HashSet::with_hasher(BuildHasherDefault::new()),
            ready: HashMap::with_hasher(BuildHasherDefault::new()),
            running: false,
            stopped: false,
            handed: 0,
            compiled: 0,
            taken: 0,
            waited: 0,
            stolen: 0,
            compile_microseconds: 0,
            wait_microseconds: 0,
            compiled_in_play: 0,
            drained_at: 0,
            scaled: false,
            depth_format: vk::Format::UNDEFINED,
        }
    }
}

struct Preparing {
    state: Mutex<PrepState>,
    work: Condvar,
    done: Condvar,
}

static P: Preparing = Preparing { state: Mutex::new(PrepState::new()), work: Condvar::new(), done: Condvar::new() };

// ---- the optimised links -------------------------------------------------------------------

struct OptimiseJob {
    key: PipelineKey,
    libraries: [vk::Pipeline; 4],
}

// The optimised links, made on a thread of their own and taken up by the consumer between
// draws.
struct OptState {
    jobs: VecDeque<OptimiseJob>,
    finished: Vec<(PipelineKey, vk::Pipeline)>,
    running: bool,
}

struct Optimising {
    state: Mutex<OptState>,
    work: Condvar,
    any_finished: AtomicBool,
}

static O: Optimising = Optimising {
    state: Mutex::new(OptState { jobs: VecDeque::new(), finished: Vec::new(), running: false }),
    work: Condvar::new(),
    any_finished: AtomicBool::new(false),
};

/// The threads' handles, the renderer's to join.
#[derive(Default)]
pub struct Threads {
    workers: Vec<JoinHandle<()>>,
    optimiser: Option<JoinHandle<()>>,
}

/// That the optimiser has been joined (stop_preparing's): what may destroy the libraries
/// it links from.
pub struct Joined(());

/// The renderer's Vk, for the optimiser thread and the shader workers.
#[derive(Clone, Copy)]
struct VkRef(*const Vk);
// SAFETY: Vk's tables are immutable after start and its calls are free-threaded but for
// what Vulkan says is externally synchronised, which these threads touch only on what they
// made themselves (the optimiser creates pipelines; a worker destroys the module of the
// shader it has just built, build_shader); start_preparing's caller keeps it alive until
// the threads are joined.
unsafe impl Send for VkRef {}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    // The platform's default thread stack, and the name stat4.sh and prof.sh find.
    thread::Builder::new().name(name.into()).stack_size(platform::default_thread_stack()).spawn(body).unwrap_or_else(
        |_| {
            // A thread that cannot start ends the program.
            crate::log::log_error!("renderer: a shader thread would not start");
            std::process::abort()
        },
    )
}

// A shader compiler: queued shaders, compiled while the title is not playing. A job taken
// is finished even if play starts or the renderer stops meanwhile.
fn work(vk: VkRef) {
    crash::register_thread(c"shader compiler");
    platform::lower_thread_priority();
    // SAFETY: start_preparing's caller keeps the Vk alive until this thread is joined.
    let vk = unsafe { &*vk.0 };
    let mut p = lock(&P.state);
    loop {
        p = P
            .work
            .wait_timeout_while(p, RECHECK, |p| !((!p.jobs.is_empty() && !playing()) || !p.running))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
        if !p.running {
            return;
        }
        if p.jobs.is_empty() || playing() {
            continue;
        }
        let Some(job) = p.jobs.pop_front() else { continue };
        p.busy.insert(job.hash);
        let (scaled, depth_format) = (p.scaled, p.depth_format);
        drop(p);
        let from = time::steady();
        let mut built = build_shader(vk, job.pixel, &job.code, scaled, depth_format);
        built.hash = job.hash;
        let took = time::steady().wrapping_sub(from) / 1000;
        p = lock(&P.state);
        p.compile_microseconds = p.compile_microseconds.wrapping_add(took);
        p.compiled += 1;
        if playing() {
            p.compiled_in_play += 1;
        }
        if p.jobs.is_empty() {
            p.drained_at = time::pacing();
        }
        p.busy.remove(&job.hash);
        p.ready.entry(job.hash).or_insert(built);
        P.done.notify_all();
    }
}

// The optimiser: each fast link linked again with link-time optimisation.
fn optimise(vk: VkRef) {
    crash::register_thread(c"pipeline optimiser");
    // Unlike the shader workers it keeps going in play, one thread on the CPU time nothing
    // else wants: fast-linked pipelines draw as well as optimised ones in the multiplayer,
    // but cost the campaign's heavier shaders enough GPU time to lose frames on trainer. It
    // only has the pipelines actually drawn with to do, a few hundred a level.
    platform::idle_thread_priority();
    // SAFETY: start_preparing's caller keeps the Vk alive until this thread is joined.
    let vk = unsafe { &*vk.0 };
    let mut o = lock(&O.state);
    loop {
        o = O.work.wait_while(o, |o| o.jobs.is_empty() && o.running).unwrap_or_else(PoisonError::into_inner);
        if !o.running {
            return;
        }
        let Some(job) = o.jobs.pop_front() else { continue };
        drop(o);
        let optimised = link(vk, &job.libraries, true);
        o = lock(&O.state);
        if optimised != vk::Pipeline::null() {
            o.finished.push((job.key, optimised));
            O.any_finished.store(true, Ordering::Release);
        }
    }
}

/// Translates a shader and compiles it: module and, where the device has them, its stage's
/// pipeline library, through the launcher's shared build (shader_build.rs), which built
/// every shader the same way into the pipeline cache this one starts with. Any thread.
/// The hash is the caller's to set.
///
/// Once the module and library are made, nothing reads the SPIR-V or the microcode again
/// but the shader cache's write: the SPIR-V goes with the build's translation, and the
/// microcode is kept only under MW2_SHADER_CACHE. Nor is the module read once its library
/// exists -- pipelines are linked from the library, and the library keeps what an
/// optimised link needs (RETAIN_LINK_TIME_OPTIMIZATION_INFO) -- so it is destroyed here
/// and left null, and `usable` says whether one was made. A shader without a library, on a
/// device without pipeline libraries or one whose library the driver refused, keeps its
/// module for the whole pipelines built from it; its partner in such a pipeline, if that
/// has a library, has its module made again for the build (remake_module).
pub fn build_shader(vk: &Vk, pixel: bool, code: &[u32], scaled: bool, depth_format: vk::Format) -> Shader {
    let mut built = build(pixel, code, scaled, depth_format);
    #[cfg(feature = "diagnostics")]
    if built.library != vk::Pipeline::null() && trace::refused_libraries() & (1 << u8::from(pixel)) != 0 {
        // MW2_FORCE_REFUSED_LIBRARIES: as if the driver had refused this library, which
        // build answers with no library and from_cache -1, the module kept.
        // SAFETY: made by this build and handed to nothing yet.
        unsafe { vk.destroy_pipeline(built.library) };
        built.library = vk::Pipeline::null();
        built.from_cache = -1;
        trace::counts().refused_libraries.fetch_add(1, Ordering::Relaxed);
    }
    if built.library != vk::Pipeline::null() {
        FROM_CACHE[(built.from_cache.clamp(-1, 1) + 1) as usize].fetch_add(1, Ordering::Relaxed);
        report::shader_built(built.from_cache);
    }
    // Made by the shared build: named by the shader's hash (trace.rs).
    #[cfg(feature = "diagnostics")]
    if trace::on() {
        let hash = shader_hash(code);
        trace::register(trace::Kind::Pipeline, built.library.as_raw(), &format!("L{hash:016x}"));
        trace::register(trace::Kind::Module, built.module.as_raw(), &format!("M{hash:016x}"));
    }
    let usable = built.module != vk::ShaderModule::null();
    if usable && built.library != vk::Pipeline::null() {
        // SAFETY: made by this build and handed to nothing but the library's creation, which
        // has returned: no pipeline is still being made from it, and nothing records with a
        // module.
        unsafe { vk.destroy_shader_module(built.module) };
        built.module = vk::ShaderModule::null();
    }
    Shader {
        hash: 0,
        ok: built.translation.ok,
        error: std::mem::take(&mut built.translation.error).into_bytes(),
        spirv_words: built.translation.spirv.len(),
        meta: built.translation.meta,
        module: built.module,
        usable,
        library: built.library,
        pixel,
        code: if shader_cache_path().is_some() { code.to_vec() } else { Vec::new() },
    }
}

/// The module of a shader whose own build_shader destroyed once its library existed, made
/// again from its microcode for a whole pipeline: the pair of that shader and one whose
/// library the driver refused cannot be linked, and is built whole from the two
/// (renderer_pipelines.rs's new_pipeline). Made by the same shared build, so the module is
/// the one destroyed; the library the build makes with it again (from the pipeline cache)
/// is destroyed here. Null if the build made no module this time.
/// The caller destroys the module once the pipeline is built.
pub fn remake_module(vk: &Vk, pixel: bool, code: &[u32], scaled: bool, depth_format: vk::Format) -> vk::ShaderModule {
    let built = build(pixel, code, scaled, depth_format);
    #[cfg(feature = "diagnostics")]
    {
        if trace::on() {
            let hash = shader_hash(code);
            trace::register(trace::Kind::Pipeline, built.library.as_raw(), &format!("L{hash:016x}"));
            trace::register(trace::Kind::Module, built.module.as_raw(), &format!("M{hash:016x}"));
        }
        trace::counts().remade_modules.fetch_add(1, Ordering::Relaxed);
    }
    if built.library != vk::Pipeline::null() {
        // SAFETY: made by this Build and handed to nothing.
        unsafe { vk.destroy_pipeline(built.library) };
    }
    built.module
}

/// A shader compiled since the title loaded it, moved out; waits for one still compiling.
/// None if it was never handed over -- or is still in the queue: the draw needs it now, and
/// compiling it there is sooner than waiting for a worker to reach it.
pub fn take_prepared(hash: u64) -> Option<Shader> {
    let mut p = lock(&P.state);
    if !p.known.insert(hash) {
        if let Some(at) = p.jobs.iter().position(|job| job.hash == hash) {
            p.jobs.remove(at);
            p.stolen += 1;
            return None;
        }
        if p.busy.contains(&hash) {
            let from = time::steady();
            p = P.done.wait_while(p, |p| p.busy.contains(&hash)).unwrap_or_else(PoisonError::into_inner);
            p.wait_microseconds = p.wait_microseconds.wrapping_add(time::steady().wrapping_sub(from) / 1000);
            p.waited += 1;
        }
    }
    let shader = p.ready.remove(&hash)?;
    p.taken += 1;
    Some(shader)
}

/// A shader the title made while loading a level (hooks/shader_preload.rs): translated and
/// compiled on a worker thread now, so its first draw finds it ready. Any thread; returns
/// at once. Before the renderer is up the job waits in the queue: the title loads its first
/// zones -- the menus, the common materials -- before then.
pub fn prepare_shader(pixel: bool, code: &[u32]) {
    if code.is_empty() {
        return;
    }
    let hash = shader_hash(code);
    let mut p = lock(&P.state);
    if p.stopped || !p.known.insert(hash) {
        return;
    }
    p.jobs.push_back(Job { pixel, hash, code: code.to_vec() });
    p.handed += 1;
    P.work.notify_one();
}

/// Whether an optimised link has finished since the last take: one atomic load per draw.
#[inline]
pub fn any_optimised() -> bool {
    O.any_finished.load(Ordering::Acquire)
}

/// The optimised links finished since the last take.
pub fn take_optimised() -> Vec<(PipelineKey, vk::Pipeline)> {
    let mut o = lock(&O.state);
    let finished = std::mem::take(&mut o.finished);
    O.any_finished.store(false, Ordering::Relaxed);
    finished
}

fn link(vk: &Vk, libraries: &[vk::Pipeline; 4], optimise: bool) -> vk::Pipeline {
    let linked =
        vk::PipelineLibraryCreateInfoKHR { library_count: 4, p_libraries: libraries.as_ptr(), ..Default::default() };
    let info = vk::GraphicsPipelineCreateInfo {
        p_next: (&linked as *const vk::PipelineLibraryCreateInfoKHR).cast(),
        flags: if optimise {
            vk::PipelineCreateFlags::LINK_TIME_OPTIMIZATION_EXT
        } else {
            vk::PipelineCreateFlags::empty()
        },
        layout: layout(),
        ..Default::default()
    };
    let mut made = [vk::Pipeline::null()];
    // SAFETY: the info, its chain and the libraries live across the call; the libraries
    // live until the renderer's shutdown, after this thread is joined.
    if unsafe { vk.create_graphics_pipelines(cache(), std::slice::from_ref(&info), &mut made) } != vk::Result::SUCCESS {
        return vk::Pipeline::null();
    }
    made[0]
}

impl Pipelines {
    fn input_library(&mut self, topology: u32) -> vk::Pipeline {
        if let Some(&found) = self.input_libraries.get(&topology) {
            return found;
        }
        let key = PipelineKey { topology, ..PipelineKey::default() };
        let state = PipelineState::new(&key);
        let info = vk::GraphicsPipelineCreateInfo {
            p_vertex_input_state: &state.input,
            p_input_assembly_state: &state.assembly,
            p_dynamic_state: &state.dynamic,
            ..Default::default()
        };
        let part = vk::GraphicsPipelineLibraryFlagsEXT::VERTEX_INPUT_INTERFACE;
        let made = make_library(part, &info, std::ptr::null());
        self.input_libraries.insert(topology, made);
        // Made by the launcher's shared code: named by its role (trace.rs).
        #[cfg(feature = "diagnostics")]
        if trace::on() {
            trace::register(trace::Kind::Pipeline, made.as_raw(), &format!("LInput{topology}"));
            // SAFETY: the info and its state are live.
            unsafe { super::device::state_library(made, part, &info, std::ptr::null()) };
        }
        made
    }

    fn output_library(&mut self, key: &PipelineKey) -> vk::Pipeline {
        let output = OutputKey {
            blend_control: key.blend_control,
            colour_mask: key.colour_mask,
            colour_format: key.colour_format,
            depth_format: key.depth_format,
            samples: key.samples,
        };
        if let Some(&found) = self.output_libraries.get(&output) {
            return found;
        }
        let state = PipelineState::new(key);
        let info = vk::GraphicsPipelineCreateInfo {
            p_color_blend_state: &state.blending,
            p_multisample_state: &state.multisample,
            p_dynamic_state: &state.dynamic,
            ..Default::default()
        };
        let part = vk::GraphicsPipelineLibraryFlagsEXT::FRAGMENT_OUTPUT_INTERFACE;
        let rendering = (&state.rendering as *const vk::PipelineRenderingCreateInfo).cast::<c_void>();
        let made = make_library(part, &info, rendering);
        self.output_libraries.insert(output, made);
        #[cfg(feature = "diagnostics")]
        if trace::on() {
            let name = format!(
                "LOutput_{:x}_{:x}_{}_{}_{}",
                output.blend_control, output.colour_mask, output.colour_format, output.depth_format, output.samples
            );
            trace::register(trace::Kind::Pipeline, made.as_raw(), &name);
            // SAFETY: the info, its state and the rendering info are live.
            unsafe { super::device::state_library(made, part, &info, rendering) };
        }
        made
    }

    /// A pipeline linked from the two shaders' libraries and the state-only ones, with an
    /// optimised link asked for in the background.
    pub(super) fn link_pipeline(
        &mut self,
        vk: &Vk,
        key: &PipelineKey,
        vertex: vk::Pipeline,
        pixel: vk::Pipeline,
    ) -> vk::Pipeline {
        let input = self.input_library(key.topology);
        let output = self.output_library(key);
        if input == vk::Pipeline::null() || output == vk::Pipeline::null() {
            return vk::Pipeline::null();
        }
        let libraries = [input, vertex, pixel, output];
        let linked = link(vk, &libraries, false);
        if linked == vk::Pipeline::null() {
            return vk::Pipeline::null();
        }
        self.pipelines_linked += 1;
        let mut o = lock(&O.state);
        if o.running {
            o.jobs.push_back(OptimiseJob { key: *key, libraries });
            O.work.notify_one();
        }
        linked
    }

    /// At the renderer's initialise: the shader workers, clamp(cores / 4, 1, 4) of them at
    /// a lower priority, and the optimiser; neither on a device without pipeline libraries,
    /// nor under MW2_DETERMINISTIC, where every shader is built at its first draw.
    ///
    /// # Safety
    /// `vk` stays valid until stop_preparing has returned.
    pub unsafe fn start_preparing(&mut self, vk: *const Vk) {
        // MW2_DETERMINISTIC: every shader built at its first draw, no optimised links.
        #[cfg(feature = "diagnostics")]
        if trace::deterministic() && pipeline_libraries() {
            let mut p = lock(&P.state);
            p.stopped = true;
            p.jobs.clear();
            log_info!(
                "renderer: MW2_DETERMINISTIC: no shader workers, no optimiser; shaders are compiled at their first draw"
            );
            return;
        }
        if !pipeline_libraries() {
            let mut p = lock(&P.state);
            p.stopped = true;
            p.jobs.clear();
            log_info!("renderer: no pipeline libraries on this device; shaders are compiled at their first draw");
            return;
        }
        {
            let mut p = lock(&P.state);
            p.running = true;
            p.scaled = self.scaled;
            p.depth_format = self.depth_format;
        }
        let threads = (platform::processor_threads() / 4).clamp(1, 4);
        for _ in 0..threads {
            let vk = VkRef(vk);
            self.threads.workers.push(spawn("shader compiler", move || work(vk)));
        }
        // SAFETY: the caller's.
        unsafe { self.start_optimiser(vk) };
        log_info!(
            "renderer: shaders are compiled as the title loads them, on %u thread%s; pipelines are linked from them at the draw and optimised in the background",
            threads,
            if threads == 1 { c"" } else { c"s" }
        );
    }

    /// # Safety
    /// As start_preparing's.
    pub(super) unsafe fn start_optimiser(&mut self, vk: *const Vk) {
        lock(&O.state).running = true;
        let vk = VkRef(vk);
        self.threads.optimiser = Some(spawn("pipeline optimiser", move || optimise(vk)));
    }

    /// At the renderer's end, once the GPU is idle: the queue emptied, the workers joined
    /// (each finishing the shader it holds), then the optimiser; its finished links join
    /// the replaced ones, and the shaders compiled and never taken are destroyed.
    pub fn stop_preparing(&mut self, vk: &Vk) -> Joined {
        {
            let mut p = lock(&P.state);
            p.running = false;
            p.stopped = true;
            p.jobs.clear();
            P.work.notify_all();
        }
        for worker in self.threads.workers.drain(..) {
            let _ = worker.join();
        }
        {
            let mut o = lock(&O.state);
            o.running = false;
            o.jobs.clear();
            O.work.notify_all();
        }
        if let Some(optimiser) = self.threads.optimiser.take() {
            let _ = optimiser.join();
        }
        let finished = std::mem::take(&mut lock(&O.state).finished);
        self.replaced.extend(finished.into_iter().map(|(_, pipeline)| pipeline));
        let ready = std::mem::take(&mut lock(&P.state).ready);
        for shader in ready.into_values() {
            // SAFETY: never handed to the draw path, so nothing recorded uses them.
            unsafe {
                if shader.library != vk::Pipeline::null() {
                    vk.destroy_pipeline(shader.library);
                }
                if shader.module != vk::ShaderModule::null() {
                    vk.destroy_shader_module(shader.module);
                }
            }
        }
        Joined(())
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
    /// The preparation's lines of the renderer's report.
    pub fn report_preparing(&self) {
        let p = lock(&P.state);
        if p.handed == 0 && self.pipelines_linked == 0 {
            return;
        }
        log_info!(
            "renderer: %llu shaders handed over at load, %llu compiled on the workers in %llu ms; %llu of them drawn with, %llu waited for (%llu ms), %llu compiled at the draw instead of in the queue, %llu never handed over",
            p.handed,
            p.compiled,
            p.compile_microseconds / 1000,
            p.taken,
            p.waited,
            p.wait_microseconds / 1000,
            p.stolen,
            self.shaders_built_at_draw.wrapping_sub(p.stolen)
        );
        log_info!(
            "renderer: %llu shader libraries out of the pipeline cache, %llu compiled by the driver, %llu it did not say which",
            FROM_CACHE[2].load(Ordering::Relaxed),
            FROM_CACHE[1].load(Ordering::Relaxed),
            FROM_CACHE[0].load(Ordering::Relaxed)
        );
        log_info!(
            "renderer: %llu of the workers' shaders finished during play; their queue last ran dry at %.1f s",
            p.compiled_in_play,
            p.drained_at as f64 / 1e6
        );
        log_info!(
            "renderer: %llu pipelines linked from libraries, %llu replaced by an optimised link",
            self.pipelines_linked,
            self.pipelines_optimised
        );
    }
}
