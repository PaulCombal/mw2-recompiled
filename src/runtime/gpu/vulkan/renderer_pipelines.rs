// Translated shaders, the pipelines built from them, and the shader cache that rebuilds a
// previous run's pipelines before the title asks for them.
//
// Xenos's fixed-function state as Vulkan's: the comparisons, stencil operations, blend
// factors and operations, topologies, the depth and stencil tests and the culling a draw
// sets as dynamic state, and every piece of a pipeline's state for one key
// (PipelineState). The mappings are pure functions; PipelineState holds pointers into
// itself, and so lives pinned in a box.
//
// Pipelines holds the renderer's shaders and pipelines: ShaderFor's slab with the last
// shader of each stage, EnsurePipeline's map with its last key, the state-only libraries,
// the optimised links finished for no pipeline, and the shader cache's record. The fast
// links an optimised one replaced go to Retiring, which the frame holds and frees each one
// from at BeginFrame once the frames that may draw with it are done (the plan's S12c). The
// threads that compile shaders and optimise
// links, and the libraries, are renderer_libraries.rs's.
//
// Unsafe by nature: it calls Vulkan (device.rs's contract). Handles stay valid by the
// renderer's order: a pipeline or library goes only at shutdown, after the optimiser that
// links them has been joined (Joined) and the GPU is idle; a replaced fast link before
// that only once the GPU has finished every submission and the recorder every call that
// can name it (Retiring::free).
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::renderer_libraries::{self as libraries, Joined, Threads};
use super::renderer_state::{DepthTests, OutputKey, PipelineKey, PipelineMap};
use crate::log::{c_string, log_info, log_warn};
use crate::runtime::gpu::hash::MulBuild;
use crate::runtime::gpu::registers::{BlendControl, DepthControl, ModeCntl};
use crate::runtime::gpu::shader::TranslationMeta;
use crate::runtime::{platform, report};
use ash::vk;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::CStr;
use std::io::{Read, Write};
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::{LazyLock, OnceLock};

use super::recorder::{done, position};

pub fn compare_op(function: u32) -> vk::CompareOp {
    match function & 7 {
        0 => vk::CompareOp::NEVER,
        1 => vk::CompareOp::LESS,
        2 => vk::CompareOp::EQUAL,
        3 => vk::CompareOp::LESS_OR_EQUAL,
        4 => vk::CompareOp::GREATER,
        5 => vk::CompareOp::NOT_EQUAL,
        6 => vk::CompareOp::GREATER_OR_EQUAL,
        _ => vk::CompareOp::ALWAYS,
    }
}

/// Xenos's stencil operations are Vulkan's, in the same order.
pub fn stencil_op(op: u32) -> vk::StencilOp {
    const OPS: [vk::StencilOp; 8] = [
        vk::StencilOp::KEEP,
        vk::StencilOp::ZERO,
        vk::StencilOp::REPLACE,
        vk::StencilOp::INCREMENT_AND_CLAMP,
        vk::StencilOp::DECREMENT_AND_CLAMP,
        vk::StencilOp::INVERT,
        vk::StencilOp::INCREMENT_AND_WRAP,
        vk::StencilOp::DECREMENT_AND_WRAP,
    ];
    OPS[(op & 7) as usize]
}

pub fn blend_factor(factor: u32) -> vk::BlendFactor {
    match factor {
        0 => vk::BlendFactor::ZERO,
        1 => vk::BlendFactor::ONE,
        4 => vk::BlendFactor::SRC_COLOR,
        5 => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        6 => vk::BlendFactor::SRC_ALPHA,
        7 => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        8 => vk::BlendFactor::DST_COLOR,
        9 => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        10 => vk::BlendFactor::DST_ALPHA,
        11 => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        12 => vk::BlendFactor::CONSTANT_COLOR,
        13 => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
        14 => vk::BlendFactor::CONSTANT_ALPHA,
        15 => vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA,
        16 => vk::BlendFactor::SRC_ALPHA_SATURATE,
        _ => vk::BlendFactor::ONE,
    }
}

pub fn blend_op(op: u32) -> vk::BlendOp {
    match op & 7 {
        1 => vk::BlendOp::SUBTRACT,
        2 => vk::BlendOp::MIN,
        3 => vk::BlendOp::MAX,
        4 => vk::BlendOp::REVERSE_SUBTRACT,
        _ => vk::BlendOp::ADD,
    }
}

/// TOPOLOGY_NONE for a primitive the renderer does not draw.
pub fn topology_for(primitive: u32) -> vk::PrimitiveTopology {
    match primitive {
        1 => vk::PrimitiveTopology::POINT_LIST,
        2 => vk::PrimitiveTopology::LINE_LIST,
        3 => vk::PrimitiveTopology::LINE_STRIP,
        4 => vk::PrimitiveTopology::TRIANGLE_LIST,
        5 => vk::PrimitiveTopology::TRIANGLE_FAN,
        6 => vk::PrimitiveTopology::TRIANGLE_STRIP,
        // A rectangle list is expanded into two triangles a rectangle before the draw, so
        // by here it is an ordinary triangle list.
        8 => vk::PrimitiveTopology::TRIANGLE_LIST,
        12 => vk::PrimitiveTopology::LINE_STRIP, // line loop
        _ => TOPOLOGY_NONE,
    }
}

/// VK_PRIMITIVE_TOPOLOGY_MAX_ENUM: a primitive type the renderer skips.
pub const TOPOLOGY_NONE: vk::PrimitiveTopology = vk::PrimitiveTopology::from_raw(0x7FFF_FFFF);

/// The depth and stencil tests RB_DEPTHCONTROL asks for. The reference and the masks are
/// set on their own. Back faces share the front's operations unless the title gives them
/// their own.
pub fn depth_tests_for(control: u32) -> DepthTests {
    let depth_control = DepthControl(control);
    let stencil = |fail, pass, depth_fail, compare| vk::StencilOpState {
        fail_op: stencil_op(fail),
        pass_op: stencil_op(pass),
        depth_fail_op: stencil_op(depth_fail),
        compare_op: compare_op(compare),
        compare_mask: 0,
        write_mask: 0,
        reference: 0,
    };
    let front = stencil(
        depth_control.stencil_fail(),
        depth_control.stencil_z_pass(),
        depth_control.stencil_z_fail(),
        depth_control.stencil_func(),
    );
    let back = if depth_control.backface_enable() {
        stencil(
            depth_control.stencil_fail_back(),
            depth_control.stencil_z_pass_back(),
            depth_control.stencil_z_fail_back(),
            depth_control.stencil_func_back(),
        )
    } else {
        front
    };
    DepthTests {
        depth_test: depth_control.z_enable().into(),
        depth_write: depth_control.z_write().into(),
        stencil_test: depth_control.stencil_enable().into(),
        depth_compare: compare_op(depth_control.z_func()),
        front,
        back,
    }
}

/// MW2_NO_CULL: a whole class of geometry missing and nothing else wrong is what a winding
/// read backwards looks like, so the culling can be taken away in one run. Read once.
fn no_cull() -> bool {
    static NO_CULL: OnceLock<bool> = OnceLock::new();
    *NO_CULL.get_or_init(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_NO_CULL"))
}

/// The culling and winding PA_SU_SC_MODE_CNTL asks for.
pub fn faces_for(mode_cntl: u32) -> (vk::CullModeFlags, vk::FrontFace) {
    let mode = ModeCntl(mode_cntl);
    let cull = if no_cull() {
        vk::CullModeFlags::NONE
    } else {
        (if mode.cull_front() { vk::CullModeFlags::FRONT } else { vk::CullModeFlags::NONE })
            | (if mode.cull_back() { vk::CullModeFlags::BACK } else { vk::CullModeFlags::NONE })
    };
    let front = if mode.front_is_clockwise() { vk::FrontFace::CLOCKWISE } else { vk::FrontFace::COUNTER_CLOCKWISE };
    (cull, front)
}

/// Every piece of a pipeline's state, filled in for one key. A whole pipeline takes all of
/// it; each library takes the part that is its own. Its members point at each other
/// (`blending` at `attachment`, `rendering` at `colour_format`), so it is made pinned in a
/// box and never moves.
#[repr(C)]
pub struct PipelineState {
    pub input: vk::PipelineVertexInputStateCreateInfo<'static>,
    pub assembly: vk::PipelineInputAssemblyStateCreateInfo<'static>,
    pub viewport: vk::PipelineViewportStateCreateInfo<'static>,
    pub raster: vk::PipelineRasterizationStateCreateInfo<'static>,
    pub multisample: vk::PipelineMultisampleStateCreateInfo<'static>,
    pub depth: vk::PipelineDepthStencilStateCreateInfo<'static>,
    pub attachment: vk::PipelineColorBlendAttachmentState,
    pub blending: vk::PipelineColorBlendStateCreateInfo<'static>,
    pub dynamic: vk::PipelineDynamicStateCreateInfo<'static>,
    pub colour_format: vk::Format,
    pub rendering: vk::PipelineRenderingCreateInfo<'static>,
    _pinned: PhantomPinned,
}

impl PipelineState {
    pub fn new(key: &PipelineKey) -> Pin<Box<PipelineState>> {
        // Xenos has no fixed-function vertex input at all: the shader fetches its own
        // attributes out of set 2.
        // The shared shader build's rasterisation and dynamic states: whole pipelines are
        // built from the same as the launcher's libraries.
        let raster = mw2_shaders::shader_build::rasterisation();
        let dynamic_states = mw2_shaders::shader_build::DYNAMIC_STATES.as_ptr();
        let dynamic_count = mw2_shaders::shader_build::DYNAMIC_STATES.len() as u32;

        let blend = BlendControl(key.blend_control);
        // Four bits per target, ARGB on the console and RGBA here.
        let mask = key.colour_mask & 0xF;
        let mut write_mask = vk::ColorComponentFlags::empty();
        for (bit, component) in [
            (1, vk::ColorComponentFlags::R),
            (2, vk::ColorComponentFlags::G),
            (4, vk::ColorComponentFlags::B),
            (8, vk::ColorComponentFlags::A),
        ] {
            if mask & bit != 0 {
                write_mask |= component;
            }
        }
        let viewport =
            vk::PipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
        let assembly = vk::PipelineInputAssemblyStateCreateInfo {
            topology: vk::PrimitiveTopology::from_raw(key.topology as i32),
            ..Default::default()
        };
        let multisample = vk::PipelineMultisampleStateCreateInfo {
            rasterization_samples: vk::SampleCountFlags::from_raw(key.samples.max(1)),
            ..Default::default()
        };
        let dynamic = vk::PipelineDynamicStateCreateInfo {
            dynamic_state_count: dynamic_count,
            p_dynamic_states: dynamic_states,
            ..Default::default()
        };
        let mut state = Box::new(PipelineState {
            input: vk::PipelineVertexInputStateCreateInfo::default(),
            assembly,
            viewport,
            raster,
            multisample,
            // The tests themselves are set per draw (depth_tests_for); `depth` stays empty.
            depth: vk::PipelineDepthStencilStateCreateInfo::default(),
            attachment: vk::PipelineColorBlendAttachmentState {
                blend_enable: (!blend.is_pass_through()).into(),
                src_color_blend_factor: blend_factor(blend.color_source()),
                dst_color_blend_factor: blend_factor(blend.color_destination()),
                color_blend_op: blend_op(blend.color_op()),
                src_alpha_blend_factor: blend_factor(blend.alpha_source()),
                dst_alpha_blend_factor: blend_factor(blend.alpha_destination()),
                alpha_blend_op: blend_op(blend.alpha_op()),
                color_write_mask: write_mask,
            },
            blending: vk::PipelineColorBlendStateCreateInfo::default(),
            dynamic,
            // Dynamic rendering: the formats of the targets stand where a render pass would.
            colour_format: vk::Format::from_raw(key.colour_format as i32),
            rendering: vk::PipelineRenderingCreateInfo::default(),
            _pinned: PhantomPinned,
        });
        state.blending.attachment_count = 1;
        state.blending.p_attachments = &state.attachment;
        state.rendering.color_attachment_count = 1;
        state.rendering.p_color_attachment_formats = &state.colour_format;
        state.rendering.depth_attachment_format = vk::Format::from_raw(key.depth_format as i32);
        state.rendering.stencil_attachment_format = vk::Format::from_raw(key.depth_format as i32);
        Box::into_pin(state)
    }
}

// ---- shaders and pipelines (ShaderFor, BuildPipeline, EnsurePipeline) ----

/// A shader made ready for the GPU: the translation the draw
/// reads (its metadata; whether it worked, and why not, with its SPIR-V's length for
/// Describe), the module and, where the device has pipeline libraries, the shader compiled
/// on its own as its stage's library (renderer_libraries.rs); null on a device without
/// them. The module is kept only where there is no library: once the library exists it is
/// destroyed (build_shader), so `usable`, not the module, says whether the shader was
/// compiled, and a whole pipeline with a partner without a
/// library makes it again (new_pipeline). `code` is the microcode, kept only under
/// MW2_SHADER_CACHE so that the shader cache can be written, and empty without one
/// (build_shader); the SPIR-V is never kept.
pub struct Shader {
    pub hash: u64,
    pub ok: bool,
    pub error: Vec<u8>,
    pub spirv_words: usize,
    pub meta: TranslationMeta,
    pub module: vk::ShaderModule,
    pub usable: bool,
    pub library: vk::Pipeline,
    pub pixel: bool,
    pub code: Vec<u32>,
}

/// A pipeline a previous run needed, with the formats its render pass was built from, so
/// it can be rebuilt before the title asks for it: 48 bytes
/// in the shader cache, little-endian.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Recorded {
    pub vertex_shader: u64,
    pub pixel_shader: u64,
    pub colour_format: u32,
    pub depth_format: u32,
    pub topology: u32,
    pub depth_control: u32,
    pub blend_control: u32,
    pub colour_mask: u32,
    pub mode_cntl: u32,
    pub samples: u32,
}

impl Default for Recorded {
    fn default() -> Self {
        Recorded {
            vertex_shader: 0,
            pixel_shader: 0,
            colour_format: 0,
            depth_format: 0,
            topology: 0,
            depth_control: 0,
            blend_control: 0,
            colour_mask: 0,
            mode_cntl: 0,
            samples: 1,
        }
    }
}

const RECORDED_BYTES: usize = 48;
const _: () = assert!(std::mem::size_of::<Recorded>() == RECORDED_BYTES);

impl Recorded {
    fn write_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.vertex_shader.to_le_bytes());
        out.extend_from_slice(&self.pixel_shader.to_le_bytes());
        for word in [
            self.colour_format,
            self.depth_format,
            self.topology,
            self.depth_control,
            self.blend_control,
            self.colour_mask,
            self.mode_cntl,
            self.samples,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }

    fn read_le(bytes: &[u8; RECORDED_BYTES]) -> Recorded {
        let d = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default());
        let w = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default());
        Recorded {
            vertex_shader: d(0),
            pixel_shader: d(8),
            colour_format: w(16),
            depth_format: w(20),
            topology: w(24),
            depth_control: w(28),
            blend_control: w(32),
            colour_mask: w(36),
            mode_cntl: w(40),
            samples: w(44),
        }
    }
}

/// The last usable shader of a stage ShaderFor answered with (State::lastShader): draws
/// come in runs with the same pair, and an entry stays where it is in the slab until the
/// slab is cleared. `valid` says there is one.
#[derive(Clone, Copy, Default)]
pub(super) struct LastShader {
    pub(super) hash: u64,
    pub(super) index: u32,
    pub(super) valid: bool,
}

/// A fast link an optimised one replaced, and what must be done before it can go: the
/// submission the draws recorded when it was replaced go in, and the recorder's queue just
/// past the calls queued by then, the last that can name it.
#[derive(Clone, Copy)]
pub struct Retired {
    pub serial: u64,
    pub queued_at: u64,
    pub pipeline: vk::Pipeline,
}

/// The fast links optimised ones replaced, oldest first, until the frames that may draw
/// with them are done (the plan's S12c). The frame holds
/// it (Core::retiring) and frees from it at BeginFrame, once the completed serial has been
/// sampled; a fresh command buffer forgets its bound pipeline (Bound), so a handle the
/// driver hands out again after a free is never taken for the one destroyed.
pub struct Retiring {
    pub queue: VecDeque<Retired>,
    /// The submission the draws recorded from here on go in: one past the frame's
    /// submission serial, set where that serial moves (Core::submit). Kept here so that a
    /// draw hands the take one pointer.
    pub serial: u64,
    /// How many have been destroyed.
    pub freed: u64,
}

impl Default for Retiring {
    fn default() -> Retiring {
        Retiring { queue: VecDeque::new(), serial: 1, freed: 0 }
    }
}

impl Retiring {
    /// A fast link an optimised one has just replaced.
    pub fn push(&mut self, pipeline: vk::Pipeline) {
        self.queue.push_back(Retired { serial: self.serial, queued_at: position(), pipeline });
    }

    /// Destroys, oldest first, each one whose submission the GPU has finished
    /// (`completed`, sampled by NoteCompleted) and whose calls the recorder has made. The
    /// serials and positions only grow along the queue, so the first that must wait
    /// stops it.
    ///
    /// # Safety
    /// `completed` is the renderer's completed serial: every submission up to it is done.
    pub unsafe fn free(&mut self, vk: &Vk, completed: u64) {
        while let Some(&front) = self.queue.front() {
            if front.serial > completed || !done(front.queued_at) {
                break;
            }
            self.queue.pop_front();
            // SAFETY: the caller's: the submissions that could draw with it are finished,
            // and no call that names it is still with the recorder; nothing else holds it
            // (the map holds its replacement, `last` is answered only while no link has
            // been taken since).
            unsafe { vk.destroy_pipeline(front.pipeline) };
            self.freed += 1;
        }
    }

    /// The ones still waiting, for the renderer's shutdown, which destroys them with the
    /// rest once the device is idle.
    pub fn take_all(&mut self) -> impl Iterator<Item = vk::Pipeline> + '_ {
        self.queue.drain(..).map(|r| r.pipeline)
    }
}

/// The renderer's shaders and pipelines (State's shaders, pipelines and libraries).
pub struct Pipelines {
    pub(super) map: PipelineMap,
    // The last pipeline EnsurePipeline answered, while no optimised one has replaced an
    // entry since (pipelines_optimised): draws come in runs with one pipeline, and the
    // key's hash was a few percent of a draw.
    pub(super) last_key: PipelineKey,
    pub(super) last: vk::Pipeline,
    pub(super) last_swaps: u64,
    // Every shader asked for, usable or not (a failure is remembered), in the order they
    // came; `by_hash` names each one's place. Boxed (§3.8): a Shader is ~1 KB, and an entry
    // stays where it is while the slab grows.
    #[allow(clippy::vec_box, reason = "stable entries, cheap growth: the plan's slab (§3.8)")]
    pub(super) shaders: Vec<Box<Shader>>,
    pub(super) by_hash: HashMap<u64, u32, MulBuild>,
    pub(super) last_shader: [LastShader; 2],
    // The state-only libraries: the input assembly for each topology, and the output stage
    // for each OutputKey (std::map's order).
    pub(super) input_libraries: BTreeMap<u32, vk::Pipeline>,
    pub(super) output_libraries: BTreeMap<OutputKey, vk::Pipeline>,
    // Optimised links finished for a key with no pipeline, or one that failed, and those
    // finished when the optimiser was stopped: never drawn with, they go at shutdown.
    // The fast links an optimised one replaced are Retiring's.
    pub(super) replaced: Vec<vk::Pipeline>,
    pub shaders_built_at_draw: u64,
    pub pipelines_linked: u64,
    pub pipelines_optimised: u64,
    /// For the shader cache: every pipeline the draw path made (renderer_draw's), and
    /// those the cache rebuilt.
    pub recorded: Vec<Recorded>,
    pub translate_microseconds: u64,
    pub compile_microseconds: u64,
    pub prewarm_microseconds: u64,
    pub prewarmed: u32,
    pub prewarm_wanted: u32,
    /// What a shader is built with (State's shadowScale > 1 and depthFormat), set at the
    /// renderer's initialise, before start_preparing.
    pub scaled: bool,
    pub depth_format: vk::Format,
    pub(super) threads: Threads,
}

/// MW2_TRACE_SHADER_LOADS: a line per shader first drawn. Read once.
fn traced_shader_loads() -> bool {
    static TRACED: OnceLock<bool> = OnceLock::new();
    *TRACED.get_or_init(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_TRACE_SHADER_LOADS"))
}

// stutters.rs's costs: "translating shaders" and "building pipelines".
const STUTTER_SHADERS: usize = 0;
const STUTTER_PIPELINES: usize = 1;

/// stutters::Timed: the time from here to `done`, when MW2_STUTTERS is on.
struct Timed {
    #[cfg(feature = "diagnostics")]
    from: Option<u64>,
}

impl Timed {
    #[inline]
    fn start() -> Timed {
        Timed {
            #[cfg(feature = "diagnostics")]
            from: crate::runtime::stutters::on().then(libraries::time::steady),
        }
    }

    #[inline]
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    fn done(self, cost: usize) {
        #[cfg(feature = "diagnostics")]
        if let Some(from) = self.from {
            crate::runtime::stutters::add(cost, libraries::time::steady().wrapping_sub(from));
        }
    }
}

impl Pipelines {
    pub fn new(scaled: bool, depth_format: vk::Format) -> Pipelines {
        Pipelines {
            map: PipelineMap::default(),
            last_key: PipelineKey::default(),
            last: vk::Pipeline::null(),
            last_swaps: 0,
            shaders: Vec::new(),
            by_hash: HashMap::default(),
            last_shader: [LastShader::default(); 2],
            input_libraries: BTreeMap::new(),
            output_libraries: BTreeMap::new(),
            replaced: Vec::new(),
            shaders_built_at_draw: 0,
            pipelines_linked: 0,
            pipelines_optimised: 0,
            recorded: Vec::new(),
            translate_microseconds: 0,
            compile_microseconds: 0,
            prewarm_microseconds: 0,
            prewarmed: 0,
            prewarm_wanted: 0,
            scaled,
            depth_format,
            threads: Threads::default(),
        }
    }

    /// The shader at `index`, as shader_for answered it.
    #[inline]
    pub fn shader(&self, index: u32) -> &Shader {
        &self.shaders[index as usize]
    }

    /// How many shaders and pipelines there are (State's shaderCount, pipelineCount).
    pub fn counts(&self) -> (usize, usize) {
        (self.shaders.len(), self.map.len())
    }

    /// The usable shader `hash` names, by its place: one the title has not drawn with
    /// before here is taken from the ones compiled when it was loaded (take_prepared), or
    /// translated and compiled now -- a stall, which is why the load-time path exists.
    /// None for no code, and for a shader that is not usable (remembered, so it fails once).
    #[inline]
    pub fn shader_for(&mut self, vk: &Vk, pixel: bool, hash: u64, code: &[u32]) -> Option<u32> {
        if code.is_empty() {
            return None;
        }
        let last = self.last_shader[usize::from(pixel)];
        if last.valid && last.hash == hash {
            return Some(last.index);
        }
        if let Some(&index) = self.by_hash.get(&hash) {
            // by_hash names only entries of the slab: `get` is the index without a panic.
            if self.shaders.get(index as usize).is_none_or(|shader| !shader.usable) {
                return None;
            }
            self.last_shader[usize::from(pixel)] = LastShader { hash, index, valid: true };
            return Some(index);
        }
        self.new_shader(vk, pixel, hash, code)
    }

    #[cold]
    #[inline(never)]
    fn new_shader(&mut self, vk: &Vk, pixel: bool, hash: u64, code: &[u32]) -> Option<u32> {
        if traced_shader_loads() {
            log_info!("shader drawn: %s %016llX", if pixel { c"pixel" } else { c"vertex" }, hash);
        }
        let mut entry = match libraries::take_prepared(hash) {
            Some(entry) => entry,
            None => {
                let timed = Timed::start();
                let start = libraries::time::steady();
                let entry = libraries::build_shader(vk, pixel, code, self.scaled, self.depth_format);
                let took = libraries::time::steady().wrapping_sub(start) / 1000;
                self.translate_microseconds = self.translate_microseconds.wrapping_add(took);
                report::add(report::SHADER, took);
                self.shaders_built_at_draw += 1;
                timed.done(STUTTER_SHADERS);
                entry
            }
        };
        entry.hash = hash;
        let ok = entry.usable;
        if !ok {
            let why = if entry.ok { c"driver refused the module".to_owned() } else { c_string(entry.error.clone()) };
            log_warn!(
                "renderer: %s shader %016llX not usable: %s",
                if pixel { c"pixel" } else { c"vertex" },
                hash,
                why.as_c_str()
            );
        }
        let index = self.shaders.len() as u32;
        self.shaders.push(Box::new(entry));
        self.by_hash.insert(hash, index);
        ok.then_some(index)
    }

    /// The pipeline for `key` drawn with the shaders at `vertex` and `pixel`; null if it
    /// could not be made (remembered, so it fails once). `made` says whether this call
    /// built it. `code` is the two shaders' microcode (vertex, pixel), read only to make a
    /// module again (new_pipeline); empty where the shader keeps its own. The fast links
    /// an optimised one replaces here go to `retire`.
    #[inline]
    #[allow(clippy::too_many_arguments, reason = "EnsurePipeline's, and where its take puts what it replaces")]
    pub fn ensure_pipeline(
        &mut self,
        vk: &Vk,
        key: &PipelineKey,
        vertex: u32,
        pixel: u32,
        code: [&[u32]; 2],
        retire: &mut Retiring,
        made: Option<&mut bool>,
    ) -> vk::Pipeline {
        self.take_optimised_pipelines(retire);
        if self.last != vk::Pipeline::null() && self.last_swaps == self.pipelines_optimised && *key == self.last_key {
            return self.last;
        }
        if let Some(&found) = self.map.get(key) {
            self.last_key = *key;
            self.last = found;
            self.last_swaps = self.pipelines_optimised;
            return found;
        }
        if let Some(made) = made {
            *made = true;
        }
        self.new_pipeline(vk, key, vertex, pixel, code)
    }

    #[cold]
    #[inline(never)]
    fn new_pipeline(&mut self, vk: &Vk, key: &PipelineKey, vertex: u32, pixel: u32, code: [&[u32]; 2]) -> vk::Pipeline {
        let start = libraries::time::steady();
        let timed = Timed::start();
        // Linked from the two shaders' libraries, compiled when the title loaded them, in
        // well under a millisecond; or the whole pipeline compiled here, on a device that
        // cannot.
        let (vertex_library, pixel_library) = (self.shader(vertex).library, self.shader(pixel).library);
        let built = if vertex_library != vk::Pipeline::null() && pixel_library != vk::Pipeline::null() {
            self.link_pipeline(vk, key, vertex_library, pixel_library)
        } else {
            // Whole, from the two modules. One shader may have its library, its module
            // destroyed (build_shader), where the other's library the driver refused: that
            // module is made again for this build from the microcode and destroyed after it,
            // so the pair is built whole.
            let mut modules = [vk::ShaderModule::null(); 2];
            let mut remade = [false; 2];
            for (side, index) in [vertex, pixel].into_iter().enumerate() {
                let shader = self.shader(index);
                modules[side] = shader.module;
                if shader.module == vk::ShaderModule::null() {
                    let words = if shader.code.is_empty() { code[side] } else { &shader.code };
                    if !words.is_empty() {
                        // Its own stage, as built: a key may pair two shaders in either slot.
                        modules[side] =
                            libraries::remake_module(vk, shader.pixel, words, self.scaled, self.depth_format);
                        remade[side] = true;
                    }
                }
            }
            let built = if modules.contains(&vk::ShaderModule::null()) {
                vk::Pipeline::null()
            } else {
                build_pipeline(vk, key, modules[0], modules[1])
            };
            for side in 0..2 {
                if remade[side] && modules[side] != vk::ShaderModule::null() {
                    // SAFETY: made above for this build alone, which has returned.
                    unsafe { vk.destroy_shader_module(modules[side]) };
                }
            }
            built
        };
        let took = libraries::time::steady().wrapping_sub(start) / 1000;
        self.compile_microseconds = self.compile_microseconds.wrapping_add(took);
        report::add(report::PIPELINE, took);
        if built == vk::Pipeline::null() {
            log_warn!("renderer: pipeline for %016llX/%016llX rejected", key.vertex_shader, key.pixel_shader);
        }
        // A failure is remembered, so it fails once.
        self.map.insert(*key, built);
        timed.done(STUTTER_PIPELINES);
        built
    }

    /// Puts the optimised links that have finished in place of the fast ones, which go to
    /// `retire`: one atomic load when none has.
    #[inline]
    pub fn take_optimised_pipelines(&mut self, retire: &mut Retiring) {
        if libraries::any_optimised() {
            self.take_optimised_slow(retire);
        }
    }

    #[cold]
    #[inline(never)]
    fn take_optimised_slow(&mut self, retire: &mut Retiring) {
        for (key, optimised) in libraries::take_optimised() {
            match self.map.get_mut(&key) {
                Some(found) if *found != vk::Pipeline::null() => {
                    retire.push(*found);
                    *found = optimised;
                    self.pipelines_optimised += 1;
                }
                _ => self.replaced.push(optimised),
            }
        }
    }

    /// Everything this holds destroyed, as the renderer's Shutdown has it: the pipelines,
    /// the optimised links never placed and the fast links still retiring (which the
    /// shutdown hands over), the state-only libraries and the shaders.
    /// Only once the optimiser, which links from the libraries, has been joined (`Joined`,
    /// stop_preparing's) and the GPU is idle.
    ///
    /// # Safety
    /// The device is idle: nothing recorded with these is still to run.
    pub unsafe fn destroy(&mut self, vk: &Vk, _joined: Joined) {
        // SAFETY: the caller's; each handle is this device's and is destroyed once.
        unsafe {
            for &pipeline in self.map.values() {
                if pipeline != vk::Pipeline::null() {
                    vk.destroy_pipeline(pipeline);
                }
            }
            for &pipeline in &self.replaced {
                vk.destroy_pipeline(pipeline);
            }
            for &library in self.input_libraries.values().chain(self.output_libraries.values()) {
                if library != vk::Pipeline::null() {
                    vk.destroy_pipeline(library);
                }
            }
            self.replaced.clear();
            self.input_libraries.clear();
            self.output_libraries.clear();
            for shader in &self.shaders {
                if shader.library != vk::Pipeline::null() {
                    vk.destroy_pipeline(shader.library);
                }
                if shader.module != vk::ShaderModule::null() {
                    vk.destroy_shader_module(shader.module);
                }
            }
        }
        self.map.clear();
        self.last = vk::Pipeline::null();
        self.shaders.clear();
        self.by_hash.clear();
        self.last_shader = [LastShader::default(); 2];
    }
}

/// The whole pipeline in one call, both shaders compiled for it: what a device without
/// pipeline libraries gets.
fn build_pipeline(vk: &Vk, key: &PipelineKey, vertex: vk::ShaderModule, pixel: vk::ShaderModule) -> vk::Pipeline {
    let vertex_stage = vk::PipelineShaderStageCreateInfo {
        stage: vk::ShaderStageFlags::VERTEX,
        module: vertex,
        p_name: c"main".as_ptr(),
        ..Default::default()
    };
    let stages = [
        vertex_stage,
        vk::PipelineShaderStageCreateInfo { stage: vk::ShaderStageFlags::FRAGMENT, module: pixel, ..vertex_stage },
    ];
    let state = PipelineState::new(key);
    let info = vk::GraphicsPipelineCreateInfo {
        p_next: (&state.rendering as *const vk::PipelineRenderingCreateInfo).cast(),
        stage_count: 2,
        p_stages: stages.as_ptr(),
        p_vertex_input_state: &state.input,
        p_input_assembly_state: &state.assembly,
        p_viewport_state: &state.viewport,
        p_rasterization_state: &state.raster,
        p_multisample_state: &state.multisample,
        p_depth_stencil_state: &state.depth,
        p_color_blend_state: &state.blending,
        p_dynamic_state: &state.dynamic,
        layout: libraries::layout(),
        ..Default::default()
    };
    let mut built = [vk::Pipeline::null()];
    // SAFETY: the info and everything it points at (the stages, the pinned state) live
    // across the call; the layout and cache are the shared pipeline's.
    if unsafe { vk.create_graphics_pipelines(libraries::cache(), std::slice::from_ref(&info), &mut built) }
        != vk::Result::SUCCESS
    {
        return vk::Pipeline::null();
    }
    built[0]
}

// ---- the shader cache (WriteShaderCache, PrewarmFromCache) ----

/// MW2_SHADER_CACHE=<file> records every pipeline a run needed and rebuilds them at
/// start-up on the next -- the "compiling shaders" pass the console never needed. It
/// cannot be built from the shaders alone: a pipeline is a shader pair *and* the state it
/// is drawn with, and a previous run is the only place that state exists. Read once.
pub fn shader_cache_path() -> Option<&'static CStr> {
    static PATH: LazyLock<Option<&'static CStr>> = LazyLock::new(|| crate::env::text(c"MW2_SHADER_CACHE"));
    *PATH
}

const CACHE_MAGIC: u32 = 0x3257_4D53; // 'SMW2'
// 3: a vertex program ends by turning window coordinates into clip space;
// 4: D24FS8 surfaces are D32S8 everywhere.
const CACHE_VERSION: u32 = 4;

/// The cache file as read: little-endian words, and a short read wherever what is asked
/// for runs past the end. A count is bounded by what is left before anything is made for
/// it, so a garbage header cannot ask for a vector of up to 16 GB of code or 192 GB of
/// pipelines before the short read is found.
struct CacheReader<'a> {
    bytes: &'a [u8],
}

impl<'a> CacheReader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        if count > self.bytes.len() {
            return None;
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// `count` words, or None when fewer are left.
    fn words(&mut self, count: u32) -> Option<Vec<u32>> {
        let bytes = self.take((count as usize).checked_mul(4)?)?;
        Some(bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
    }
}

impl Pipelines {
    /// At the renderer's end: every shader and every recorded pipeline, for the next run's
    /// prewarm_from_cache. 'SMW2', the version, the counts, then each shader's hash, stage,
    /// length and code, then the 48-byte pipelines; all little-endian.
    pub fn write_shader_cache(&self) {
        let Some(path) = shader_cache_path() else { return };
        if self.recorded.is_empty() {
            return;
        }
        let Ok(mut file) = std::fs::File::create(platform::system_path(path)) else { return };
        let code_words: usize = self.shaders.iter().map(|shader| shader.code.len()).sum();
        let mut bytes = Vec::with_capacity(16 + self.shaders.len() * 16 + code_words * 4 + self.recorded.len() * 48);
        for word in [CACHE_MAGIC, CACHE_VERSION, self.shaders.len() as u32, self.recorded.len() as u32] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for shader in &self.shaders {
            bytes.extend_from_slice(&shader.hash.to_le_bytes());
            bytes.extend_from_slice(&u32::from(shader.pixel).to_le_bytes());
            bytes.extend_from_slice(&(shader.code.len() as u32).to_le_bytes());
            for word in &shader.code {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        for recorded in &self.recorded {
            recorded.write_le(&mut bytes);
        }
        // A write that fails is not looked at.
        let _ = file.write_all(&bytes);
        drop(file);
        log_info!(
            "renderer: wrote %llu shaders and %llu pipelines to %s",
            self.shaders.len() as u64,
            self.recorded.len() as u64,
            path
        );
    }

    /// At the renderer's start: the shaders and pipelines a previous run wrote, built now.
    /// Work the draw path would otherwise do mid-frame, which is where a hitch comes from --
    /// the total is small, the moment it is paid is the problem.
    pub fn prewarm_from_cache(&mut self, vk: &Vk, retire: &mut Retiring) {
        let Some(path) = shader_cache_path() else { return };
        let Ok(mut file) = std::fs::File::open(platform::system_path(path)) else { return };
        let mut bytes = Vec::new();
        // What could be read is what there is: a read that fails part way is a short file.
        let _ = file.read_to_end(&mut bytes);
        drop(file);
        let mut reader = CacheReader { bytes: &bytes };

        let header = [reader.u32(), reader.u32(), reader.u32(), reader.u32()];
        let [Some(magic), Some(version), Some(shaders), Some(pipelines)] = header else {
            log_warn!("renderer: %s is not a shader cache for this build, ignoring it", path);
            return;
        };
        if magic != CACHE_MAGIC || version != CACHE_VERSION {
            log_warn!("renderer: %s is not a shader cache for this build, ignoring it", path);
            return;
        }

        let start = libraries::time::steady();
        for _ in 0..shaders {
            let (Some(hash), Some(stage), Some(words)) = (reader.u64(), reader.u32(), reader.u32()) else { return };
            let Some(code) = reader.words(words) else { return };
            self.shader_for(vk, stage != 0, hash, &code);
        }

        let Some(wanted_bytes) = reader.take((pipelines as usize).saturating_mul(RECORDED_BYTES)) else { return };
        let wanted: Vec<Recorded> = wanted_bytes
            .chunks_exact(RECORDED_BYTES)
            .map(|entry| Recorded::read_le(entry.try_into().unwrap_or(&[0; RECORDED_BYTES])))
            .collect();

        self.prewarm_wanted = wanted.len() as u32;
        let mut last = 0u32;
        for (at, want) in wanted.iter().enumerate() {
            let (Some(&vertex), Some(&pixel)) =
                (self.by_hash.get(&want.vertex_shader), self.by_hash.get(&want.pixel_shader))
            else {
                continue;
            };
            if !self.shader(vertex).usable || !self.shader(pixel).usable {
                continue;
            }
            let key = PipelineKey {
                vertex_shader: want.vertex_shader,
                pixel_shader: want.pixel_shader,
                colour_format: want.colour_format,
                depth_format: want.depth_format,
                topology: want.topology,
                blend_control: want.blend_control,
                colour_mask: want.colour_mask,
                samples: want.samples,
            };
            // The shaders keep their microcode here (MW2_SHADER_CACHE).
            if self.ensure_pipeline(vk, &key, vertex, pixel, [&[], &[]], retire, None) != vk::Pipeline::null() {
                self.prewarmed += 1;
            }
            // Kept, so the cache written at shutdown is the union of this run and earlier
            // ones. Recording only what this run built would shrink it every time.
            self.recorded.push(*want);

            // Progress, because this is the pass a player would be looking at.
            let percent = (100 * (at as u64 + 1) / wanted.len() as u64) as u32;
            if percent >= last.wrapping_add(25) {
                log_info!("compiling shaders: %u%%", percent);
                last = percent;
            }
        }
        self.prewarm_microseconds = libraries::time::steady().wrapping_sub(start) / 1000;
        log_info!(
            "renderer: prewarmed %u of %u pipelines over %llu shaders in %llu us",
            self.prewarmed,
            self.prewarm_wanted,
            self.shaders.len() as u64,
            self.prewarm_microseconds
        );
    }
}
