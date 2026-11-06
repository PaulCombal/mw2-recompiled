// The crate root the shader rig builds the Rust translator in, as a static
// library: shader_diff.py copies the shader folder (src/runtime/gpu/shader,
// or the one --src names) next to this file as `shader/`, so the folder is
// compiled exactly as it stands, with nothing of the runtime around it.
#![deny(unsafe_code)]

pub mod shader;

use std::ffi::{c_char, c_void};

/// Every constant of bindings.rs as `name value` lines, for the rig to hold
/// against bindings.h.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn mw2_shader_bindings(context: *mut c_void, sink: shader::ffi::TextSink) {
    let mut text = String::new();
    for (name, value) in shader::bindings::ALL {
        text.push_str(name);
        text.push(' ');
        text.push_str(&value.to_string());
        text.push('\n');
    }
    sink(context, text.as_ptr().cast::<c_char>(), text.len());
}

/// TranslationMeta as rustc lays it out, `name offset size` a field, then
/// `size <size> <alignment>`: what the C interface hands over, for the rig to
/// hold against the forwarder's Meta as the C++ compiler lays that out.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn mw2_shader_meta_layout(context: *mut c_void, sink: shader::ffi::TextSink) {
    use shader::TranslationMeta as M;
    use std::mem::{align_of, offset_of, size_of, size_of_val};
    let m = M::default();
    let mut text = String::new();
    let mut field = |name: &str, offset: usize, size: usize| text.push_str(&format!("{name} {offset} {size}\n"));
    field("interpolator_mask", offset_of!(M, interpolator_mask), size_of_val(&m.interpolator_mask));
    field("texture_mask", offset_of!(M, texture_mask), size_of_val(&m.texture_mask));
    field("texture_kinds", offset_of!(M, texture_kinds), size_of_val(&m.texture_kinds));
    field("vertex_fetch_mask", offset_of!(M, vertex_fetch_mask), size_of_val(&m.vertex_fetch_mask));
    field("vertex_fetch_subs", offset_of!(M, vertex_fetch_subs), size_of_val(&m.vertex_fetch_subs));
    field("vertex_fetch_pairs", offset_of!(M, vertex_fetch_pairs), size_of_val(&m.vertex_fetch_pairs));
    field("vertex_fetch_stride", offset_of!(M, vertex_fetch_stride), size_of_val(&m.vertex_fetch_stride));
    field("vertex_fetch_tail", offset_of!(M, vertex_fetch_tail), size_of_val(&m.vertex_fetch_tail));
    field("fetch_pairs_read", offset_of!(M, fetch_pairs_read), size_of_val(&m.fetch_pairs_read));
    field("constants_read", offset_of!(M, constants_read), size_of_val(&m.constants_read));
    field("addressed_constants", offset_of!(M, addressed_constants), size_of_val(&m.addressed_constants));
    field("colour_mask", offset_of!(M, colour_mask), size_of_val(&m.colour_mask));
    field("writes_depth", offset_of!(M, writes_depth), size_of_val(&m.writes_depth));
    field("writes_memory", offset_of!(M, writes_memory), size_of_val(&m.writes_memory));
    field("instruction_count", offset_of!(M, instruction_count), size_of_val(&m.instruction_count));
    field("dropped_exports", offset_of!(M, dropped_exports), size_of_val(&m.dropped_exports));
    field("dropped_memory_exports", offset_of!(M, dropped_memory_exports), size_of_val(&m.dropped_memory_exports));
    text.push_str(&format!("size {} {}\n", size_of::<M>(), align_of::<M>()));
    sink(context, text.as_ptr().cast::<c_char>(), text.len());
}

/// The SPIR-V writer driven by a script of its calls, three words a step,
/// and the module it finishes with: shader_diff.cpp's CppScript, call for
/// call, so the two writers are held to one another beyond what the
/// translator asks of them (-0.0, NaNs, any constant's bits).
///
/// # Safety
///
/// `ops` must point at `count` readable words.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mw2_spirv_script(ops: *const u32, count: usize, context: *mut c_void, sink: shader::ffi::WordSink) {
    use shader::spirv::{self, Module, op};
    // SAFETY: the rig hands `count` readable words.
    let ops: &[u32] = if count == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ops, count) } };
    let mut m = Module::new();
    let mut results: Vec<u32> = Vec::new();
    let pick = |results: &Vec<u32>, a: u32| if results.is_empty() { a } else { results[a as usize % results.len()] };
    const TEXT: &str = "abcdefghijklmnopq";
    for step in ops.chunks_exact(3) {
        let (a, b) = (step[1], step[2]);
        let mut r = 0;
        match step[0] % 21 {
            0 => r = m.constant_f(f32::from_bits(a)),
            1 => r = m.constant_u(a),
            2 => r = m.constant_s(a as i32),
            3 => r = m.constant_splat_f(f32::from_bits(a)),
            4 => r = m.vector(pick(&results, a), b % 5),
            5 => r = m.pointer(a % 16, pick(&results, b)),
            6 => r = m.array_of(pick(&results, a), b),
            7 => r = m.runtime_array_of(pick(&results, a)),
            8 => r = m.image(pick(&results, a), b % 8, (b >> 8) & 1 != 0),
            9 => r = m.sampled_image(pick(&results, a)),
            10 => r = m.sampler(),
            11 => r = m.void(),
            12 => r = m.bool(),
            13 => r = m.int(a & 1 != 0),
            14 => r = m.float(),
            15 => r = m.constant_false_value(),
            16 => m.require_capability(a % 8),
            17 => r = m.allocate(),
            18 => spirv::emit_string(&mut m.debug, op::NAME, &[pick(&results, a)], &TEXT[..(b % 17) as usize]),
            19 => {
                let interface: Vec<u32> = (0..b % 4).map(|k| pick(&results, a.wrapping_add(k))).collect();
                let function = pick(&results, a);
                m.entry_point(a % 5, function, &TEXT[..((a >> 8) % 17) as usize], &interface);
            }
            _ => {
                let operand = pick(&results, b);
                let section = match a % 8 {
                    0 => &mut m.capabilities,
                    1 => &mut m.preamble,
                    2 => &mut m.entry_points,
                    3 => &mut m.execution_modes,
                    4 => &mut m.debug,
                    5 => &mut m.decorations,
                    6 => &mut m.declarations,
                    _ => &mut m.code,
                };
                spirv::emit(section, b & 0xFFFF, &[operand, a]);
            }
        }
        if r != 0 {
            results.push(r);
        }
    }
    let words = m.finish();
    sink(context, words.as_ptr(), words.len());
}
