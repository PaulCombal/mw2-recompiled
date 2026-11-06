// The system's screens (screen.rs) as the window shows them: the game behind, blurred
// and darkened, and the screen's picture over the middle with rounded corners and a
// shadow, faded in and out. One draw, into the swapchain image after the frame was
// copied into it. runtime/gpu/vulkan/backdrop.cpp in Rust.
//
// The blur is the frame copied down a chain of halvings, each a linear blit, and read
// back at the smallest with a tent of nine taps. Two fixed shaders, written as SPIR-V
// here with the translator's writer (shader/spirv.rs), which has every instruction
// they use.
//
// Unsafe by nature: everything is made on the presenter's device and used only by the
// window thread; a slot's view and set are replaced only after that presenter frame's
// fence was waited for, and the blur only after `settle` has waited for the others.
#![deny(unsafe_op_in_unsafe_fn)]

use super::util;
use crate::log::log_warn;
use crate::runtime::gpu::shader::spirv::{Id, Module, built_in, decoration, emit, glsl, head, op, storage};
use ash::vk;
use std::ffi::CStr;

const EXECUTION_MODEL_VERTEX: u32 = 0;
const EXECUTION_MODEL_FRAGMENT: u32 = 4;
const EXECUTION_MODE_ORIGIN_UPPER_LEFT: u32 = 7;
const IMAGE_OPERANDS_LOD: u32 = 0x2;
const DIM_2D: u32 = 1;
/// At least the presenter's frames in flight: each has its own descriptor set and view
/// of the image it draws into.
pub const SLOTS: u32 = 4;
/// The blur: the frame at half size, then halved again three times, and read at the
/// smallest.
pub const LEVELS: u32 = 4;

/// The fragment shader's push constants, four vectors: where the game's frame is in the
/// window (from x, y, to x, y); where the picture is, as drawn now; shown, corner
/// radius, outline, 0; the window's size and the blur's texel in the frame's
/// coordinates.
pub type Push = [f32; 16];

// A function body under construction: each instruction with a result is (type, id,
// operands). The result's id is allocated after its operands', as the C++ does.
struct Code {
    m: Module,
}

impl Code {
    fn op(&mut self, op: u32, ty: Id, operands: &[u32]) -> Id {
        let result = self.m.allocate();
        self.m.code.push(head(op, operands.len() as u32 + 3));
        self.m.code.push(ty);
        self.m.code.push(result);
        self.m.code.extend_from_slice(operands);
        result
    }

    fn ext(&mut self, function: u32, ty: Id, operands: &[u32]) -> Id {
        let mut all = vec![self.m.glsl_set(), function];
        all.extend_from_slice(operands);
        self.op(op::EXT_INST, ty, &all)
    }

    fn variable(&mut self, storage: u32, ty: Id) -> Id {
        let pointer = self.m.pointer(storage, ty);
        let variable = self.m.allocate();
        emit(&mut self.m.declarations, op::VARIABLE, &[pointer, variable, storage]);
        variable
    }

    fn decorate(&mut self, target: Id, decoration: u32, value: u32) {
        emit(&mut self.m.decorations, op::DECORATE, &[target, decoration, value]);
    }

    // OpFunction for `main`, after its entry point is declared.
    fn main(&mut self, model: u32, interface: &[Id]) -> Id {
        let function = self.m.allocate();
        let void = self.m.void();
        emit(&mut self.m.declarations, op::TYPE_FUNCTION, &[function, void]);
        let main = self.m.allocate();
        self.m.entry_point(model, main, "main", interface);
        let void = self.m.void();
        emit(&mut self.m.code, op::FUNCTION, &[void, main, 0, function]);
        let label = self.m.allocate();
        emit(&mut self.m.code, op::LABEL, &[label]);
        main
    }

    fn end(mut self) -> Vec<u32> {
        emit(&mut self.m.code, op::RETURN, &[]);
        emit(&mut self.m.code, op::FUNCTION_END, &[]);
        self.m.finish()
    }

    fn f(&mut self, value: f32) -> Id {
        self.m.constant_f(value)
    }
}

/// A triangle that covers the window: vertices (-1,-1), (3,-1), (-1,3).
pub fn vertex_shader() -> Vec<u32> {
    let mut c = Code { m: Module::new() };
    let i32_ = c.m.int(true);
    let f32_ = c.m.float();
    let f4 = c.m.float4();
    let index = c.variable(storage::INPUT, i32_);
    c.decorate(index, decoration::BUILT_IN, built_in::VERTEX_INDEX);
    let position = c.variable(storage::OUTPUT, f4);
    c.decorate(position, decoration::BUILT_IN, built_in::POSITION);
    c.main(EXECUTION_MODEL_VERTEX, &[index, position]);

    let i = c.op(op::LOAD, i32_, &[index]);
    let one = c.m.constant_s(1);
    let doubled = c.op(op::SHIFT_LEFT_LOGICAL, i32_, &[i, one]);
    let two = c.m.constant_s(2);
    let masked = c.op(op::BITWISE_AND, i32_, &[doubled, two]);
    let x = c.op(op::CONVERT_S_TO_F, f32_, &[masked]);
    let two = c.m.constant_s(2);
    let masked = c.op(op::BITWISE_AND, i32_, &[i, two]);
    let y = c.op(op::CONVERT_S_TO_F, f32_, &[masked]);
    let clip = |c: &mut Code, v: Id| {
        let two = c.f(2.0);
        let scaled = c.op(op::F_MUL, f32_, &[v, two]);
        let one = c.f(1.0);
        c.op(op::F_SUB, f32_, &[scaled, one])
    };
    let cx = clip(&mut c, x);
    let cy = clip(&mut c, y);
    let zero = c.f(0.0);
    let one = c.f(1.0);
    let at = c.op(op::COMPOSITE_CONSTRUCT, f4, &[cx, cy, zero, one]);
    emit(&mut c.m.code, op::STORE, &[position, at]);
    c.end()
}

/// Per pixel p:
///   backdrop = blur(frame at p) * 0.42 * vignette, black outside the frame
///   shadow   = the picture's rounded rectangle, lowered and softened
///   colour   = mix(backdrop * (1 - shadow), picture (outline at its edge), inside)
/// and `shown` as alpha, blended over the frame already in the window. Each line
/// allocates its ids in the order the C++'s expression evaluates them.
pub fn fragment_shader() -> Vec<u32> {
    let mut c = Code { m: Module::new() };
    let f32_ = c.m.float();
    let f2 = c.m.float2();
    let f3 = c.m.float3();
    let f4 = c.m.float4();

    let frag_coord = c.variable(storage::INPUT, f4);
    c.decorate(frag_coord, decoration::BUILT_IN, built_in::FRAG_COORD);
    let colour = c.variable(storage::OUTPUT, f4);
    c.decorate(colour, decoration::LOCATION, 0);

    let image = c.m.image(f32_, DIM_2D, false);
    let sampled = c.m.sampled_image(image);
    let blur = c.variable(storage::UNIFORM_CONSTANT, sampled);
    let picture = c.variable(storage::UNIFORM_CONSTANT, sampled);
    for (variable, binding) in [(blur, 0), (picture, 1)] {
        c.decorate(variable, decoration::DESCRIPTOR_SET, 0);
        c.decorate(variable, decoration::BINDING, binding);
    }

    let block = c.m.allocate();
    emit(&mut c.m.declarations, op::TYPE_STRUCT, &[block, f4, f4, f4, f4]);
    emit(&mut c.m.decorations, op::DECORATE, &[block, decoration::BLOCK]);
    for member in 0..4u32 {
        emit(&mut c.m.decorations, op::MEMBER_DECORATE, &[block, member, decoration::OFFSET, member * 16]);
    }
    let push = c.variable(storage::PUSH_CONSTANT, block);

    let main = c.main(EXECUTION_MODEL_FRAGMENT, &[frag_coord, colour]);
    emit(&mut c.m.execution_modes, op::EXECUTION_MODE, &[main, EXECUTION_MODE_ORIGIN_UPPER_LEFT]);

    let member = |c: &mut Code, index: i32| {
        let pointer = c.m.pointer(storage::PUSH_CONSTANT, f4);
        let at_index = c.m.constant_s(index);
        let at = c.op(op::ACCESS_CHAIN, pointer, &[push, at_index]);
        c.op(op::LOAD, f4, &[at])
    };
    let pair = |c: &mut Code, v: Id, a: u32, b: u32| c.op(op::VECTOR_SHUFFLE, f2, &[v, v, a, b]);
    let part = |c: &mut Code, v: Id, i: u32| c.op(op::COMPOSITE_EXTRACT, f32_, &[v, i]);
    let two = |c: &mut Code, a: Id, b: Id| c.op(op::COMPOSITE_CONSTRUCT, f2, &[a, b]);
    let three = |c: &mut Code, a: Id| c.op(op::COMPOSITE_CONSTRUCT, f3, &[a, a, a]);
    let add = |c: &mut Code, ty: Id, a: Id, b: Id| c.op(op::F_ADD, ty, &[a, b]);
    let sub = |c: &mut Code, ty: Id, a: Id, b: Id| c.op(op::F_SUB, ty, &[a, b]);
    let mul = |c: &mut Code, ty: Id, a: Id, b: Id| c.op(op::F_MUL, ty, &[a, b]);
    let scale = |c: &mut Code, ty: Id, v: Id, s: Id| c.op(op::VECTOR_TIMES_SCALAR, ty, &[v, s]);
    let sample = |c: &mut Code, variable: Id, uv: Id, lod: Id| {
        let image = c.op(op::LOAD, sampled, &[variable]);
        c.op(op::IMAGE_SAMPLE_EXPLICIT_LOD, f4, &[image, uv, IMAGE_OPERANDS_LOD, lod])
    };
    let rgb = |c: &mut Code, v: Id| c.op(op::VECTOR_SHUFFLE, f3, &[v, v, 0, 1, 2]);
    let step = |c: &mut Code, edge: Id, x: Id| c.ext(glsl::STEP, f32_, &[edge, x]);
    let smooth = |c: &mut Code, from: Id, to: Id, x: Id| c.ext(glsl::SMOOTH_STEP, f32_, &[from, to, x]);

    let coord = c.op(op::LOAD, f4, &[frag_coord]);
    let p = pair(&mut c, coord, 0, 1);
    let frame = member(&mut c, 0);
    let place = member(&mut c, 1);
    let params = member(&mut c, 2);
    let view = member(&mut c, 3);

    // The backdrop.
    let frame_from = pair(&mut c, frame, 0, 1);
    let frame_to = pair(&mut c, frame, 2, 3);
    let offset = sub(&mut c, f2, p, frame_from);
    let size = sub(&mut c, f2, frame_to, frame_from);
    let uv = c.op(op::F_DIV, f2, &[offset, size]);
    let texel = pair(&mut c, view, 2, 3);
    let lod = c.f((LEVELS - 1) as f32);
    let mut sum: Id = 0;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let weight = ((2 - dx.abs()) * (2 - dy.abs())) as f32 / 16.0;
            let fx = c.f(dx as f32);
            let fy = c.f(dy as f32);
            let d = two(&mut c, fx, fy);
            let shift = mul(&mut c, f2, d, texel);
            let at = add(&mut c, f2, uv, shift);
            let tap = sample(&mut c, blur, at, lod);
            let w = c.f(weight);
            let tap = scale(&mut c, f4, tap, w);
            sum = if sum != 0 { add(&mut c, f4, sum, tap) } else { tap };
        }
    }
    let u = part(&mut c, uv, 0);
    let v = part(&mut c, uv, 1);
    let zero = c.f(0.0);
    let a = step(&mut c, zero, u);
    let one = c.f(1.0);
    let b = step(&mut c, u, one);
    let across = mul(&mut c, f32_, a, b);
    let zero = c.f(0.0);
    let a = step(&mut c, zero, v);
    let one = c.f(1.0);
    let b = step(&mut c, v, one);
    let down = mul(&mut c, f32_, a, b);
    let in_frame = mul(&mut c, f32_, across, down);
    let shape = pair(&mut c, view, 0, 1);
    let over = c.op(op::F_DIV, f2, &[p, shape]);
    let half_a = c.f(0.5);
    let half_b = c.f(0.5);
    let middle = two(&mut c, half_a, half_b);
    let centred = sub(&mut c, f2, over, middle);
    let one = c.f(1.0);
    let dot = c.op(op::DOT, f32_, &[centred, centred]);
    let k = c.f(0.6);
    let fall = mul(&mut c, f32_, dot, k);
    let vignette = sub(&mut c, f32_, one, fall);
    let colours = rgb(&mut c, sum);
    let lit = mul(&mut c, f32_, in_frame, vignette);
    let dark = c.f(0.42);
    let factor = mul(&mut c, f32_, lit, dark);
    let backdrop = scale(&mut c, f3, colours, factor);

    // The picture's rounded rectangle, as a signed distance: negative inside.
    let from = pair(&mut c, place, 0, 1);
    let to = pair(&mut c, place, 2, 3);
    let both = add(&mut c, f2, from, to);
    let half = c.f(0.5);
    let centre = scale(&mut c, f2, both, half);
    let across = sub(&mut c, f2, to, from);
    let half_f = c.f(0.5);
    let half = scale(&mut c, f2, across, half_f);
    let radius = part(&mut c, params, 1);
    let outline = part(&mut c, params, 2);
    let shown = part(&mut c, params, 0);
    let distance = |c: &mut Code, offset: Id| {
        let relative = sub(c, f2, p, centre);
        let moved = sub(c, f2, relative, offset);
        let local = c.ext(glsl::F_ABS, f2, &[moved]);
        let inner = sub(c, f2, local, half);
        let corner = two(c, radius, radius);
        let q = add(c, f2, inner, corner);
        let zero_a = c.f(0.0);
        let zero_b = c.f(0.0);
        let zeros = two(c, zero_a, zero_b);
        let positive = c.ext(glsl::F_MAX, f2, &[q, zeros]);
        let outside = c.ext(glsl::LENGTH, f32_, &[positive]);
        let qx = part(c, q, 0);
        let qy = part(c, q, 1);
        let largest = c.ext(glsl::F_MAX, f32_, &[qx, qy]);
        let zero = c.f(0.0);
        let within = c.ext(glsl::F_MIN, f32_, &[largest, zero]);
        let total = add(c, f32_, outside, within);
        sub(c, f32_, total, radius)
    };

    let zero = c.f(0.0);
    let k = c.f(0.8);
    let lowered = mul(&mut c, f32_, radius, k);
    let drop = two(&mut c, zero, lowered);
    let below = distance(&mut c, drop);
    let strength = c.f(0.6);
    let one = c.f(1.0);
    let soft_from = c.op(op::F_NEGATE, f32_, &[radius]);
    let k = c.f(3.0);
    let soft_to = mul(&mut c, f32_, radius, k);
    let soft = smooth(&mut c, soft_from, soft_to, below);
    let lit = sub(&mut c, f32_, one, soft);
    let shadow = mul(&mut c, f32_, strength, lit);
    let one = c.f(1.0);
    let unshadowed = sub(&mut c, f32_, one, shadow);
    let behind = scale(&mut c, f3, backdrop, unshadowed);

    let zero_a = c.f(0.0);
    let zero_b = c.f(0.0);
    let none = two(&mut c, zero_a, zero_b);
    let edge = distance(&mut c, none);
    let one = c.f(1.0);
    let lo = c.f(-0.75);
    let hi = c.f(0.75);
    let blend = smooth(&mut c, lo, hi, edge);
    let inside = sub(&mut c, f32_, one, blend);
    let within = sub(&mut c, f2, p, from);
    let extent = sub(&mut c, f2, to, from);
    let at = c.op(op::F_DIV, f2, &[within, extent]);
    let k = c.f(0.75);
    let wide = add(&mut c, f32_, outline, k);
    let rim_from = c.op(op::F_NEGATE, f32_, &[wide]);
    let k = c.f(0.25);
    let narrow = sub(&mut c, f32_, outline, k);
    let rim_to = c.op(op::F_NEGATE, f32_, &[narrow]);
    let rim = smooth(&mut c, rim_from, rim_to, edge);
    let r = c.f(70.0 / 255.0);
    let g = c.f(73.0 / 255.0);
    let b = c.f(78.0 / 255.0);
    let outline_colour = c.op(op::COMPOSITE_CONSTRUCT, f3, &[r, g, b]);
    let lod0 = c.f(0.0);
    let texture = sample(&mut c, picture, at, lod0);
    let texture = rgb(&mut c, texture);
    let rims = three(&mut c, rim);
    let face = c.ext(glsl::F_MIX, f3, &[texture, outline_colour, rims]);
    let insides = three(&mut c, inside);
    let mixed = c.ext(glsl::F_MIX, f3, &[behind, face, insides]);
    let out = c.op(op::COMPOSITE_CONSTRUCT, f4, &[mixed, shown]);
    emit(&mut c.m.code, op::STORE, &[colour, out]);
    c.end()
}

/// The blur's levels for a frame of this size: the frame halved, then halved again
/// until 1x1 or LEVELS of them.
pub fn blur_levels(frame_width: u32, frame_height: u32) -> (u32, [vk::Extent2D; LEVELS as usize]) {
    let mut level = [vk::Extent2D::default(); LEVELS as usize];
    let mut size = vk::Extent2D { width: (frame_width / 2).max(1), height: (frame_height / 2).max(1) };
    let mut levels = 0;
    while levels < LEVELS {
        level[levels as usize] = size;
        if size.width == 1 && size.height == 1 {
            levels += 1;
            break;
        }
        size = vk::Extent2D { width: (size.width / 2).max(1), height: (size.height / 2).max(1) };
        levels += 1;
    }
    (levels, level)
}

/// What a draw is: where its frame and picture are, and how far it has faded in.
#[derive(Clone, Copy, Default)]
pub struct Draw {
    pub commands: vk::CommandBuffer,
    pub slot: u32,         // the presenter's frame in flight
    pub target: vk::Image, // TRANSFER_DST, and left so
    pub extent: vk::Extent2D,
    // The game's frame, TRANSFER_SRC, and where it was copied in the window. With no
    // image the blur of the last one is shown where that was, or black before there
    // was any.
    pub frame: vk::Image,
    pub frame_width: u32,
    pub frame_height: u32,
    pub frame_from: vk::Offset2D,
    pub frame_to: vk::Offset2D,
    // The picture, SHADER_READ_ONLY, and where it sits in the window when fully shown;
    // outline is its frame's width in pixels.
    pub picture: vk::ImageView,
    pub picture_from: vk::Offset2D,
    pub picture_to: vk::Offset2D,
    pub outline: f32,
    pub shown: f32, // 0 to 1, the fade
}

/// The fragment shader's push constants for `draw`, with the blur's smallest level.
pub fn push_constants(draw: &Draw, smallest: vk::Extent2D) -> Push {
    // The picture grows the last few percent as it fades in, about its centre.
    let grow = 0.96f32 + 0.04f32 * draw.shown;
    let cx = 0.5f32 * draw.picture_from.x.wrapping_add(draw.picture_to.x) as f32;
    let cy = 0.5f32 * draw.picture_from.y.wrapping_add(draw.picture_to.y) as f32;
    let hw = 0.5f32 * draw.picture_to.x.wrapping_sub(draw.picture_from.x) as f32 * grow;
    let hh = 0.5f32 * draw.picture_to.y.wrapping_sub(draw.picture_from.y) as f32 * grow;
    // Before any frame, the whole window: the blur is black there anyway, and the
    // shader divides by the frame's size.
    let framed = draw.frame_to.x > draw.frame_from.x && draw.frame_to.y > draw.frame_from.y;
    let from = if framed { draw.frame_from } else { vk::Offset2D { x: 0, y: 0 } };
    let to =
        if framed { draw.frame_to } else { vk::Offset2D { x: draw.extent.width as i32, y: draw.extent.height as i32 } };
    [
        from.x as f32,
        from.y as f32,
        to.x as f32,
        to.y as f32,
        cx - hw,
        cy - hh,
        cx + hw,
        cy + hh,
        draw.shown,
        draw.outline * 5.0,
        draw.outline * grow,
        0.0,
        draw.extent.width as f32,
        draw.extent.height as f32,
        1.0 / smallest.width as f32,
        1.0 / smallest.height as f32,
    ]
}

/// What a draw does to the blur first, if making it succeeds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BlurPlan {
    /// The presenter's other frames are waited for: the blur they may read goes.
    pub settle: bool,
    /// Made again for the frame, at its size.
    pub remake: bool,
    /// None is left, and a 16x16 black one is made.
    pub placeholder: bool,
}

/// The plan for a draw with this frame (None: no new frame), when the blur was made
/// for `made_for` (0x0 when there is none) and `has_image` says whether it exists.
pub fn blur_plan(made_for: (u32, u32), has_image: bool, frame: Option<(u32, u32)>) -> BlurPlan {
    let remake = frame.is_some_and(|size| size != made_for);
    BlurPlan { settle: remake && has_image, remake, placeholder: !remake && !has_image }
}

#[derive(Default)]
struct Blur {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    frame_width: u32, // what it was made for
    frame_height: u32,
    levels: u32,
    level: [vk::Extent2D; LEVELS as usize],
    filled: bool,
}

/// The backdrop's objects on the presenter's device.
#[derive(Default)]
pub struct Backdrop {
    device: Option<ash::Device>,
    rendering: Option<ash::khr::dynamic_rendering::Device>,
    physical_memory: vk::PhysicalDeviceMemoryProperties,
    format: vk::Format,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    sets: [vk::DescriptorSet; SLOTS as usize],
    targets: [vk::ImageView; SLOTS as usize], // a view of the image each slot drew into last
    blur: Blur,
    // A format the pipeline could not be made for, not tried again.
    failed: vk::Format,
}

fn check(result: Result<(), vk::Result>, what: &CStr) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            log_warn!("vulkan: the screens' backdrop: %s failed (%d)", what, error.as_raw());
            false
        }
    }
}

// The blur's level range of `image`, from one layout to another.
fn level_range_barrier(
    image: vk::Image,
    first: u32,
    count: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    from_access: vk::AccessFlags,
    to_access: vk::AccessFlags,
) -> [vk::ImageMemoryBarrier<'static>; 1] {
    [vk::ImageMemoryBarrier {
        src_access_mask: from_access,
        dst_access_mask: to_access,
        old_layout: from,
        new_layout: to,
        src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        image,
        subresource_range: vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: first,
            level_count: count,
            base_array_layer: 0,
            layer_count: 1,
        },
        ..Default::default()
    }]
}

// MW2_DUMP_SHADERS=<dir>: the two shaders as SPIR-V files, for spirv-val and for
// holding against the C++'s.
#[cfg(feature = "diagnostics")]
fn dump(vertex: &[u32], fragment: &[u32]) {
    let Some(dir) = crate::env::text(c"MW2_DUMP_SHADERS") else { return };
    let dir = crate::runtime::platform::path(dir);
    for (name, words) in [("builtin_backdrop_vertex.spv", vertex), ("builtin_backdrop_pixel.spv", fragment)] {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let _ = std::fs::write(dir.join(name), bytes);
    }
}

impl Backdrop {
    /// For the swapchain's format, on the presenter's device. False when it cannot be
    /// made: the presenter then copies the picture over as it is.
    ///
    /// # Safety
    /// The device is live, made on `instance` from `physical`, and outlives destroy().
    pub unsafe fn create(
        &mut self,
        instance: &ash::Instance,
        device: &ash::Device,
        physical: vk::PhysicalDevice,
        format: vk::Format,
    ) -> bool {
        if self.pipeline != vk::Pipeline::null() && self.format == format {
            return true;
        }
        if format == self.failed {
            return false;
        }
        // SAFETY: what destroy() destroys is this backdrop's, and no frame reads it: the
        // presenter makes a backdrop only between its frames' commands.
        unsafe { self.destroy() };
        self.device = Some(device.clone());
        // SAFETY: the instance's physical device.
        self.physical_memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        self.format = format;
        // SAFETY: names asked of the live device.
        let begin = unsafe { instance.get_device_proc_addr(device.handle(), c"vkCmdBeginRenderingKHR".as_ptr()) };
        let end = unsafe { instance.get_device_proc_addr(device.handle(), c"vkCmdEndRenderingKHR".as_ptr()) };
        if begin.is_some() && end.is_some() {
            self.rendering = Some(ash::khr::dynamic_rendering::Device::new(instance, device));
        }
        // SAFETY: Vulkan calls on the device just stored.
        if begin.is_none() || end.is_none() || !unsafe { self.make_pipeline() } || !unsafe { self.make_descriptors() } {
            log_warn!("vulkan: the screens are drawn without their backdrop");
            // SAFETY: as above.
            unsafe { self.destroy() };
            self.failed = format;
            return false;
        }
        true
    }

    /// # Safety
    /// Nothing the GPU still runs reads what the backdrop made.
    pub unsafe fn destroy(&mut self) {
        let Some(device) = self.device.take() else { return };
        // SAFETY: this backdrop's objects, idle by the caller's word.
        unsafe {
            Self::destroy_blur_on(&device, &mut self.blur);
            for view in &mut self.targets {
                if *view != vk::ImageView::null() {
                    device.destroy_image_view(*view, None);
                    *view = vk::ImageView::null();
                }
            }
            if self.pool != vk::DescriptorPool::null() {
                device.destroy_descriptor_pool(self.pool, None);
            }
            if self.sampler != vk::Sampler::null() {
                device.destroy_sampler(self.sampler, None);
            }
            if self.pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(self.pipeline, None);
            }
            if self.layout != vk::PipelineLayout::null() {
                device.destroy_pipeline_layout(self.layout, None);
            }
            if self.set_layout != vk::DescriptorSetLayout::null() {
                device.destroy_descriptor_set_layout(self.set_layout, None);
            }
        }
        let failed = self.failed;
        *self = Backdrop { failed, ..Backdrop::default() };
    }

    fn device(&self) -> &ash::Device {
        self.device.as_ref().expect("the backdrop's device")
    }

    // SAFETY (callers): the device is set and live.
    unsafe fn module(&self, words: &[u32]) -> vk::ShaderModule {
        let info = vk::ShaderModuleCreateInfo {
            code_size: std::mem::size_of_val(words),
            p_code: words.as_ptr(),
            ..Default::default()
        };
        // SAFETY: SPIR-V words that live through the call.
        let made = unsafe { self.device().create_shader_module(&info, None) };
        check(made.map(drop), c"a shader");
        made.unwrap_or_default()
    }

    unsafe fn make_pipeline(&mut self) -> bool {
        let device = self.device().clone();
        let bindings: [vk::DescriptorSetLayoutBinding; 2] = std::array::from_fn(|i| vk::DescriptorSetLayoutBinding {
            binding: i as u32,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            ..Default::default()
        });
        let set_info =
            vk::DescriptorSetLayoutCreateInfo { binding_count: 2, p_bindings: bindings.as_ptr(), ..Default::default() };
        // SAFETY: Vulkan calls on the backdrop's device, with infos on this stack.
        unsafe {
            match device.create_descriptor_set_layout(&set_info, None) {
                Ok(made) => self.set_layout = made,
                Err(error) => return check(Err(error), c"the set layout"),
            }
            let range = [vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                offset: 0,
                size: std::mem::size_of::<Push>() as u32,
            }];
            let layout_info = vk::PipelineLayoutCreateInfo {
                set_layout_count: 1,
                p_set_layouts: &self.set_layout,
                push_constant_range_count: 1,
                p_push_constant_ranges: range.as_ptr(),
                ..Default::default()
            };
            match device.create_pipeline_layout(&layout_info, None) {
                Ok(made) => self.layout = made,
                Err(error) => return check(Err(error), c"the pipeline layout"),
            }

            let (vertex_words, fragment_words) = (vertex_shader(), fragment_shader());
            #[cfg(feature = "diagnostics")]
            dump(&vertex_words, &fragment_words);
            let vertex = self.module(&vertex_words);
            let fragment = self.module(&fragment_words);
            let stages = [
                vk::PipelineShaderStageCreateInfo {
                    stage: vk::ShaderStageFlags::VERTEX,
                    module: vertex,
                    p_name: c"main".as_ptr(),
                    ..Default::default()
                },
                vk::PipelineShaderStageCreateInfo {
                    stage: vk::ShaderStageFlags::FRAGMENT,
                    module: fragment,
                    p_name: c"main".as_ptr(),
                    ..Default::default()
                },
            ];
            let input = vk::PipelineVertexInputStateCreateInfo::default();
            let assembly = vk::PipelineInputAssemblyStateCreateInfo {
                topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                ..Default::default()
            };
            let viewport =
                vk::PipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
            let raster = vk::PipelineRasterizationStateCreateInfo {
                polygon_mode: vk::PolygonMode::FILL,
                cull_mode: vk::CullModeFlags::NONE,
                line_width: 1.0,
                ..Default::default()
            };
            let multisample = vk::PipelineMultisampleStateCreateInfo {
                rasterization_samples: vk::SampleCountFlags::TYPE_1,
                ..Default::default()
            };
            // Over the frame by `shown`; the window's alpha is left alone.
            let attachment = vk::PipelineColorBlendAttachmentState {
                blend_enable: vk::TRUE,
                src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
                dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                color_blend_op: vk::BlendOp::ADD,
                src_alpha_blend_factor: vk::BlendFactor::ZERO,
                dst_alpha_blend_factor: vk::BlendFactor::ONE,
                alpha_blend_op: vk::BlendOp::ADD,
                color_write_mask: vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B,
            };
            let blend = vk::PipelineColorBlendStateCreateInfo {
                attachment_count: 1,
                p_attachments: &attachment,
                ..Default::default()
            };
            let dynamics = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo {
                dynamic_state_count: 2,
                p_dynamic_states: dynamics.as_ptr(),
                ..Default::default()
            };
            let rendering = vk::PipelineRenderingCreateInfoKHR {
                color_attachment_count: 1,
                p_color_attachment_formats: &self.format,
                ..Default::default()
            };
            let info = vk::GraphicsPipelineCreateInfo {
                p_next: (&raw const rendering).cast(),
                stage_count: 2,
                p_stages: stages.as_ptr(),
                p_vertex_input_state: &input,
                p_input_assembly_state: &assembly,
                p_viewport_state: &viewport,
                p_rasterization_state: &raster,
                p_multisample_state: &multisample,
                p_color_blend_state: &blend,
                p_dynamic_state: &dynamic,
                layout: self.layout,
                ..Default::default()
            };
            let made = vertex != vk::ShaderModule::null() && fragment != vk::ShaderModule::null() && {
                let mut pipeline = vk::Pipeline::null();
                let result = (device.fp_v1_0().create_graphics_pipelines)(
                    device.handle(),
                    vk::PipelineCache::null(),
                    1,
                    &info,
                    std::ptr::null(),
                    &mut pipeline,
                );
                self.pipeline = pipeline;
                check(result.result(), c"the pipeline")
            };
            if vertex != vk::ShaderModule::null() {
                device.destroy_shader_module(vertex, None);
            }
            if fragment != vk::ShaderModule::null() {
                device.destroy_shader_module(fragment, None);
            }
            made
        }
    }

    unsafe fn make_descriptors(&mut self) -> bool {
        let device = self.device().clone();
        let sampler = vk::SamplerCreateInfo {
            mag_filter: vk::Filter::LINEAR,
            min_filter: vk::Filter::LINEAR,
            mipmap_mode: vk::SamplerMipmapMode::LINEAR,
            address_mode_u: vk::SamplerAddressMode::CLAMP_TO_EDGE,
            address_mode_v: vk::SamplerAddressMode::CLAMP_TO_EDGE,
            address_mode_w: vk::SamplerAddressMode::CLAMP_TO_EDGE,
            max_lod: vk::LOD_CLAMP_NONE,
            ..Default::default()
        };
        // SAFETY: Vulkan calls on the backdrop's device, with infos on this stack.
        unsafe {
            match device.create_sampler(&sampler, None) {
                Ok(made) => self.sampler = made,
                Err(error) => return check(Err(error), c"the sampler"),
            }
            let size = [vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 2 * SLOTS,
            }];
            let pool_info = vk::DescriptorPoolCreateInfo {
                max_sets: SLOTS,
                pool_size_count: 1,
                p_pool_sizes: size.as_ptr(),
                ..Default::default()
            };
            match device.create_descriptor_pool(&pool_info, None) {
                Ok(made) => self.pool = made,
                Err(error) => return check(Err(error), c"the descriptor pool"),
            }
            let layouts = [self.set_layout; SLOTS as usize];
            let allocate = vk::DescriptorSetAllocateInfo {
                descriptor_pool: self.pool,
                descriptor_set_count: SLOTS,
                p_set_layouts: layouts.as_ptr(),
                ..Default::default()
            };
            let result =
                (device.fp_v1_0().allocate_descriptor_sets)(device.handle(), &allocate, self.sets.as_mut_ptr());
            check(result.result(), c"the descriptor sets")
        }
    }

    unsafe fn destroy_blur_on(device: &ash::Device, blur: &mut Blur) {
        // SAFETY: the blur's objects, which no command still reads (the callers' word).
        unsafe {
            if blur.view != vk::ImageView::null() {
                device.destroy_image_view(blur.view, None);
            }
            if blur.image != vk::Image::null() {
                device.destroy_image(blur.image, None);
            }
            if blur.memory != vk::DeviceMemory::null() {
                device.free_memory(blur.memory, None);
            }
        }
        *blur = Blur::default();
    }

    unsafe fn make_blur(&mut self, frame_width: u32, frame_height: u32) -> bool {
        let device = self.device().clone();
        let b = &mut self.blur;
        b.frame_width = frame_width;
        b.frame_height = frame_height;
        (b.levels, b.level) = blur_levels(frame_width, frame_height);
        let image = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::B8G8R8A8_UNORM,
            extent: vk::Extent3D { width: b.level[0].width, height: b.level[0].height, depth: 1 },
            mip_levels: b.levels,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..Default::default()
        };
        // SAFETY: Vulkan calls on the backdrop's device, with infos on this stack.
        unsafe {
            match device.create_image(&image, None) {
                Ok(made) => b.image = made,
                Err(error) => return check(Err(error), c"the blur's image"),
            }
            let needs = device.get_image_memory_requirements(b.image);
            let allocate = vk::MemoryAllocateInfo {
                allocation_size: needs.size,
                memory_type_index: util::find_memory(
                    &self.physical_memory,
                    needs.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ),
                ..Default::default()
            };
            if allocate.memory_type_index == u32::MAX {
                return false;
            }
            match device.allocate_memory(&allocate, None) {
                Ok(made) => b.memory = made,
                Err(error) => return check(Err(error), c"the blur's memory"),
            }
            if !check(device.bind_image_memory(b.image, b.memory, 0), c"the blur's memory") {
                return false;
            }
            let view = vk::ImageViewCreateInfo {
                image: b.image,
                view_type: vk::ImageViewType::TYPE_2D,
                format: image.format,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: b.levels,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                ..Default::default()
            };
            match device.create_image_view(&view, None) {
                Ok(made) => {
                    b.view = made;
                    true
                }
                Err(error) => check(Err(error), c"the blur's view"),
            }
        }
    }

    unsafe fn level_barrier(
        &self,
        commands: vk::CommandBuffer,
        first: u32,
        count: u32,
        layouts: (vk::ImageLayout, vk::ImageLayout),
        accesses: (vk::AccessFlags, vk::AccessFlags),
        stages: (vk::PipelineStageFlags, vk::PipelineStageFlags),
    ) {
        let barrier = level_range_barrier(self.blur.image, first, count, layouts.0, layouts.1, accesses.0, accesses.1);
        // SAFETY: the caller's recording command buffer, on the blur's device.
        unsafe {
            self.device().cmd_pipeline_barrier(
                commands,
                stages.0,
                stages.1,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barrier,
            )
        };
    }

    // The frame copied down the chain: each level a linear blit of the one above, so
    // every texel of the frame counts. Read in the fragment shader.
    unsafe fn fill(&mut self, draw: &Draw) {
        use vk::{AccessFlags as A, ImageLayout as L, PipelineStageFlags as S};
        let device = self.device().clone();
        let levels = self.blur.levels;
        let last = levels - 1;
        let image = self.blur.image;
        // SAFETY: recording into the draw's command buffer: the blur's image, and the
        // frame, in TRANSFER_SRC until the presenter's submission.
        unsafe {
            self.level_barrier(
                draw.commands,
                0,
                levels,
                (if self.blur.filled { L::SHADER_READ_ONLY_OPTIMAL } else { L::UNDEFINED }, L::TRANSFER_DST_OPTIMAL),
                (A::SHADER_READ, A::TRANSFER_WRITE),
                (S::FRAGMENT_SHADER, S::TRANSFER),
            );
            for level in 0..levels {
                let size = self.blur.level[level as usize];
                let mut blit = vk::ImageBlit {
                    dst_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: level,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    ..Default::default()
                };
                blit.dst_offsets[1] = vk::Offset3D { x: size.width as i32, y: size.height as i32, z: 1 };
                if level == 0 {
                    blit.src_subresource = vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    blit.src_offsets[1] =
                        vk::Offset3D { x: draw.frame_width as i32, y: draw.frame_height as i32, z: 1 };
                    device.cmd_blit_image(
                        draw.commands,
                        draw.frame,
                        L::TRANSFER_SRC_OPTIMAL,
                        image,
                        L::TRANSFER_DST_OPTIMAL,
                        &[blit],
                        vk::Filter::LINEAR,
                    );
                } else {
                    self.level_barrier(
                        draw.commands,
                        level - 1,
                        1,
                        (L::TRANSFER_DST_OPTIMAL, L::TRANSFER_SRC_OPTIMAL),
                        (A::TRANSFER_WRITE, A::TRANSFER_READ),
                        (S::TRANSFER, S::TRANSFER),
                    );
                    let above = self.blur.level[level as usize - 1];
                    blit.src_subresource = vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: level - 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    blit.src_offsets[1] = vk::Offset3D { x: above.width as i32, y: above.height as i32, z: 1 };
                    device.cmd_blit_image(
                        draw.commands,
                        image,
                        L::TRANSFER_SRC_OPTIMAL,
                        image,
                        L::TRANSFER_DST_OPTIMAL,
                        &[blit],
                        vk::Filter::LINEAR,
                    );
                }
            }
            if last > 0 {
                self.level_barrier(
                    draw.commands,
                    0,
                    last,
                    (L::TRANSFER_SRC_OPTIMAL, L::SHADER_READ_ONLY_OPTIMAL),
                    (A::TRANSFER_READ, A::SHADER_READ),
                    (S::TRANSFER, S::FRAGMENT_SHADER),
                );
            }
            self.level_barrier(
                draw.commands,
                last,
                1,
                (L::TRANSFER_DST_OPTIMAL, L::SHADER_READ_ONLY_OPTIMAL),
                (A::TRANSFER_WRITE, A::SHADER_READ),
                (S::TRANSFER, S::FRAGMENT_SHADER),
            );
        }
        self.blur.filled = true;
    }

    // Before any frame, a blur of nothing: black.
    unsafe fn clear(&mut self, commands: vk::CommandBuffer) {
        use vk::{AccessFlags as A, ImageLayout as L, PipelineStageFlags as S};
        let levels = self.blur.levels;
        // SAFETY: recording into the caller's command buffer, on the blur's image.
        unsafe {
            self.level_barrier(
                commands,
                0,
                levels,
                (L::UNDEFINED, L::TRANSFER_DST_OPTIMAL),
                (A::empty(), A::TRANSFER_WRITE),
                (S::TOP_OF_PIPE, S::TRANSFER),
            );
            let black = vk::ClearColorValue::default();
            let range = [vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: levels,
                base_array_layer: 0,
                layer_count: 1,
            }];
            self.device().cmd_clear_color_image(commands, self.blur.image, L::TRANSFER_DST_OPTIMAL, &black, &range);
            self.level_barrier(
                commands,
                0,
                levels,
                (L::TRANSFER_DST_OPTIMAL, L::SHADER_READ_ONLY_OPTIMAL),
                (A::TRANSFER_WRITE, A::SHADER_READ),
                (S::TRANSFER, S::FRAGMENT_SHADER),
            );
        }
        self.blur.filled = true;
    }

    /// Records the draw. `settle` waits for the presenter's other frames, before an
    /// image they may read is replaced.
    ///
    /// # Safety
    /// `draw.commands` is recording on the backdrop's device; the target, the frame and
    /// the picture are live and in the layouts Draw names.
    pub unsafe fn record(&mut self, draw: &Draw, settle: &mut dyn FnMut()) {
        if self.pipeline == vk::Pipeline::null() || draw.slot >= SLOTS {
            return;
        }
        let commands = draw.commands;
        let device = self.device().clone();
        let plan = blur_plan(
            (self.blur.frame_width, self.blur.frame_height),
            self.blur.image != vk::Image::null(),
            (draw.frame != vk::Image::null()).then_some((draw.frame_width, draw.frame_height)),
        );
        // SAFETY (the block): Vulkan calls on the backdrop's device; a slot's view is
        // destroyed after its frame's fence was waited for, the blur after settle().
        unsafe {
            if plan.remake {
                if plan.settle {
                    settle();
                }
                Self::destroy_blur_on(&device, &mut self.blur);
                if !self.make_blur(draw.frame_width, draw.frame_height) {
                    Self::destroy_blur_on(&device, &mut self.blur);
                    return;
                }
            }
            if plan.placeholder {
                if !self.make_blur(16, 16) {
                    Self::destroy_blur_on(&device, &mut self.blur);
                    return;
                }
                self.clear(commands);
            }
            if draw.frame != vk::Image::null() {
                self.fill(draw);
            }

            // The slot's last view is no longer read: its frame's fence was waited for.
            let slot = draw.slot as usize;
            if self.targets[slot] != vk::ImageView::null() {
                device.destroy_image_view(self.targets[slot], None);
            }
            self.targets[slot] = vk::ImageView::null();
            let view_info = vk::ImageViewCreateInfo {
                image: draw.target,
                view_type: vk::ImageViewType::TYPE_2D,
                format: self.format,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                ..Default::default()
            };
            match device.create_image_view(&view_info, None) {
                Ok(made) => self.targets[slot] = made,
                Err(error) => {
                    check(Err(error), c"the window's view");
                    return;
                }
            }
            let view = self.targets[slot];

            let images = [
                vk::DescriptorImageInfo {
                    sampler: self.sampler,
                    image_view: self.blur.view,
                    image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                },
                vk::DescriptorImageInfo {
                    sampler: self.sampler,
                    image_view: draw.picture,
                    image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                },
            ];
            let writes: [vk::WriteDescriptorSet; 2] = std::array::from_fn(|i| vk::WriteDescriptorSet {
                dst_set: self.sets[slot],
                dst_binding: i as u32,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                p_image_info: &images[i],
                ..Default::default()
            });
            device.update_descriptor_sets(&writes, &[]);

            util::barrier(
                &device,
                commands,
                draw.target,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            );

            let attachment = vk::RenderingAttachmentInfoKHR {
                image_view: view,
                image_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                load_op: vk::AttachmentLoadOp::LOAD,
                store_op: vk::AttachmentStoreOp::STORE,
                ..Default::default()
            };
            let rendering = vk::RenderingInfoKHR {
                render_area: vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: draw.extent },
                layer_count: 1,
                color_attachment_count: 1,
                p_color_attachments: &attachment,
                ..Default::default()
            };
            let dynamic = self.rendering.as_ref().expect("dynamic rendering");
            dynamic.cmd_begin_rendering(commands, &rendering);

            let viewport = [vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: draw.extent.width as f32,
                height: draw.extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }];
            let scissor = [vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: draw.extent }];
            device.cmd_bind_pipeline(commands, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_set_viewport(commands, 0, &viewport);
            device.cmd_set_scissor(commands, 0, &scissor);
            device.cmd_bind_descriptor_sets(
                commands,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &self.sets[slot..slot + 1],
                &[],
            );

            let smallest = self.blur.level[self.blur.levels as usize - 1];
            let push = push_constants(draw, smallest);
            let mut raw = [0u8; std::mem::size_of::<Push>()];
            for (chunk, value) in raw.chunks_exact_mut(4).zip(push) {
                chunk.copy_from_slice(&value.to_ne_bytes());
            }
            device.cmd_push_constants(commands, self.layout, vk::ShaderStageFlags::FRAGMENT, 0, &raw);
            device.cmd_draw(commands, 3, 1, 0, 0);
            dynamic.cmd_end_rendering(commands);

            util::barrier(
                &device,
                commands,
                draw.target,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
            );
        }
    }
}

// tools/gpu_diff.py --rig presenter: Backdrop::record run on the rig's stand-in Vulkan
// calls (tools/gpu_diff/presenter_stubs.cpp, through presenter::rig::tables), from the
// state create() leaves, against the C++ Record on the same stand-ins.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;
    use ash::vk::Handle;

    /// What a draw is, as the rig's C++ gives it.
    #[repr(C)]
    pub struct RigDraw {
        frame_width: u32,
        frame_height: u32,
        frame: [i32; 4],
        picture: [i32; 4],
        extent: [u32; 2],
        outline: f32,
        shown: f32,
    }

    /// The blur a draw left.
    #[repr(C)]
    pub struct Recorded {
        settles: u32,
        levels: u32,
        level: [u32; 8],
        made_width: u32,
        made_height: u32,
        has_image: u32,
    }

    /// As after create(): a pipeline and its sets for B8G8R8A8_UNORM, no blur, with the
    /// handles the C++ rig gives its own.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_backdrop_new() -> *mut Backdrop {
        let (_, instance, device) = crate::runtime::gpu::vulkan::presenter::rig::tables();
        // SAFETY: the rig's stand-ins.
        let physical_memory = unsafe { instance.get_physical_device_memory_properties(vk::PhysicalDevice::null()) };
        let backdrop = Backdrop {
            rendering: Some(ash::khr::dynamic_rendering::Device::new(&instance, &device)),
            device: Some(device),
            physical_memory,
            format: vk::Format::B8G8R8A8_UNORM,
            pipeline: vk::Pipeline::from_raw(0x9002),
            layout: vk::PipelineLayout::from_raw(0x9003),
            sampler: vk::Sampler::from_raw(0x9004),
            sets: std::array::from_fn(|i| vk::DescriptorSet::from_raw(0x9010 + i as u64)),
            ..Backdrop::default()
        };
        Box::into_raw(Box::new(backdrop))
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_backdrop_free(backdrop: *mut Backdrop) {
        drop(unsafe { Box::from_raw(backdrop) });
    }

    /// record() of this draw into command buffer 0x9100 over image 0x9101, with frame
    /// 0x9102 when `has_frame` and picture 0x9103.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_backdrop_record(
        backdrop: *mut Backdrop,
        d: *const RigDraw,
        has_frame: bool,
        slot: u32,
        out: *mut Recorded,
    ) {
        let (backdrop, d) = unsafe { (&mut *backdrop, &*d) };
        let draw = Draw {
            commands: vk::CommandBuffer::from_raw(0x9100),
            slot,
            target: vk::Image::from_raw(0x9101),
            extent: vk::Extent2D { width: d.extent[0], height: d.extent[1] },
            frame: if has_frame { vk::Image::from_raw(0x9102) } else { vk::Image::null() },
            frame_width: d.frame_width,
            frame_height: d.frame_height,
            frame_from: vk::Offset2D { x: d.frame[0], y: d.frame[1] },
            frame_to: vk::Offset2D { x: d.frame[2], y: d.frame[3] },
            picture: vk::ImageView::from_raw(0x9103),
            picture_from: vk::Offset2D { x: d.picture[0], y: d.picture[1] },
            picture_to: vk::Offset2D { x: d.picture[2], y: d.picture[3] },
            outline: d.outline,
            shown: d.shown,
        };
        let mut settles = 0u32;
        // SAFETY: the rig's stand-ins, which keep no state of their own.
        unsafe { backdrop.record(&draw, &mut || settles += 1) };
        let b = &backdrop.blur;
        let mut level = [0u32; 8];
        for (i, size) in b.level.iter().enumerate() {
            level[2 * i] = size.width;
            level[2 * i + 1] = size.height;
        }
        let recorded = Recorded {
            settles,
            levels: b.levels,
            level,
            made_width: b.frame_width,
            made_height: b.frame_height,
            has_image: (b.image != vk::Image::null()).into(),
        };
        unsafe { out.write(recorded) };
    }
}
