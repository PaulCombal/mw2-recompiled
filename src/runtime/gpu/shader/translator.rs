// Xenos shader microcode -> SPIR-V.
//
// The title compiles nothing at runtime: it hands the GPU microcode its offline
// compiler produced. A Vulkan backend has to translate that while the title
// runs, so this lives in the runtime rather than in a tool.
//
// What comes out has a fixed interface, which is what lets one pipeline layout
// serve every shader: a vertex shader writes gl_Position and its interpolators
// to locations 0..15; a pixel shader reads those and writes colours to 0..3.
//
// A port of runtime/gpu/vulkan/shader_translator.cpp, which stays for the
// launcher until it is ported too: the two give the same words for every
// program (tools/shader_diff), so a change to one is made to both. Every id is
// allocated in the C++'s order, which is clang's left-to-right evaluation of
// each call's arguments, spelt out here as one `let` per argument.

use super::bindings;
use super::spirv::{self, Id, Module, decoration, op, storage};
use super::ucode::{
    Alu, ControlFlow, EXPORT_DEPTH, EXPORT_MEMORY_ADDRESS, EXPORT_MEMORY_LAST, Fetch, INTERPOLATOR_COUNT, dimension,
    fetch_op, unpack_control_flow,
};

pub(super) const EXECUTION_MODEL_VERTEX: u32 = 0;
pub(super) const EXECUTION_MODEL_FRAGMENT: u32 = 4;
pub(super) const EXECUTION_MODE_ORIGIN_UPPER_LEFT: u32 = 7;
pub(super) const FUNCTION_CONTROL_NONE: u32 = 0;
pub(super) const LOOP_CONTROL_NONE: u32 = 0;
pub(super) const SELECTION_CONTROL_NONE: u32 = 0;
pub(super) const LOOP_STACK_DEPTH: u32 = 4; // what the sequencer provides
pub(super) const CAPABILITY_IMAGE_QUERY: u32 = 50;
pub(super) const CAPABILITY_GROUP_NON_UNIFORM: u32 = 61;
pub(super) const CAPABILITY_GROUP_NON_UNIFORM_BALLOT: u32 = 64;
pub(super) const CAPABILITY_DRAW_PARAMETERS: u32 = 4427;
pub(super) const SCOPE_SUBGROUP: u32 = 3;
pub(super) const IMAGE_OPERAND_BIAS: u32 = 0x1;
pub(super) const IMAGE_OPERAND_LOD: u32 = 0x2;

const RECORD_WORDS: usize = bindings::RECORD_WORDS as usize;

// What the pipeline has to bind for this shader to run. Laid out for C, so
// the C interface (ffi.rs) hands it over whole, arrays inline.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TranslationMeta {
    pub interpolator_mask: u32, // interpolators read or written
    pub texture_mask: u32,      // texture fetch constants sampled
    // What each sampled slot is declared as: 0 a 2D image, 1 a cube map,
    // 2 a 3D image. A view of another kind is invalid there.
    pub texture_kinds: [u8; 32],
    pub vertex_fetch_mask: u32, // fetch constant groups read
    // A group of six dwords is either one texture constant or three vertex
    // ones, so the group mask alone names slots the program never reads --
    // whose bits read as a vertex fetch of some enormous buffer. Exact slot:
    // bit `group` of `vertex_fetch_subs[sub]`.
    pub vertex_fetch_subs: [u32; 3],
    // The same slots numbered as the fetch window's pairs, group * 3 +
    // sub, a bit each: what a draw walks to rewrite them.
    pub vertex_fetch_pairs: [u32; 3],
    // A draw reads `index * stride + tail` dwords of what a fetch constant
    // points at, which is far less than the extent the constant declares.
    pub vertex_fetch_stride: [u32; 96],
    pub vertex_fetch_tail: [u32; 96],
    // Every pair of the fetch window the program reads at all, vertex or
    // texture, a bit each: a draw writes these into the arena and leaves
    // the rest of the window as it found it.
    pub fetch_pairs_read: [u32; 3],
    // The float constants the program actually reads: one past the highest
    // index, or nothing if it reads none. `addressed_constants` means it
    // indexes them through a0 or aL, where the index is a run-time value and
    // the whole window has to go across.
    pub constants_read: u32,
    pub addressed_constants: bool,
    pub colour_mask: u32, // colour targets a pixel shader writes
    pub writes_depth: bool,
    pub writes_memory: bool, // uses the memory export path
    pub instruction_count: u32,
    // Exports the hardware has no output for, dropped rather than failed.
    pub dropped_exports: u32,
    // Writes to guest memory, dropped for want of anywhere to put them.
    pub dropped_memory_exports: u32,
}

// The layout the C interface hands over, which the C++ forwarder's Meta
// (shader_translator_rust.cpp) asserts too: a field moved on one side only
// stops the build instead of reading another field's bytes.
const _: () = {
    use std::mem::{align_of, offset_of, size_of};
    type M = TranslationMeta;
    assert!(size_of::<M>() == 876 && align_of::<M>() == 4);
    assert!(offset_of!(M, interpolator_mask) == 0);
    assert!(offset_of!(M, texture_mask) == 4);
    assert!(offset_of!(M, texture_kinds) == 8);
    assert!(offset_of!(M, vertex_fetch_mask) == 40);
    assert!(offset_of!(M, vertex_fetch_subs) == 44);
    assert!(offset_of!(M, vertex_fetch_pairs) == 56);
    assert!(offset_of!(M, vertex_fetch_stride) == 68);
    assert!(offset_of!(M, vertex_fetch_tail) == 452);
    assert!(offset_of!(M, fetch_pairs_read) == 836);
    assert!(offset_of!(M, constants_read) == 848);
    assert!(offset_of!(M, addressed_constants) == 852);
    assert!(offset_of!(M, colour_mask) == 856);
    assert!(offset_of!(M, writes_depth) == 860);
    assert!(offset_of!(M, writes_memory) == 861);
    assert!(offset_of!(M, instruction_count) == 864);
    assert!(offset_of!(M, dropped_exports) == 868);
    assert!(offset_of!(M, dropped_memory_exports) == 872);
};

impl Default for TranslationMeta {
    fn default() -> Self {
        TranslationMeta {
            interpolator_mask: 0,
            texture_mask: 0,
            texture_kinds: [0; 32],
            vertex_fetch_mask: 0,
            vertex_fetch_subs: [0; 3],
            vertex_fetch_pairs: [0; 3],
            vertex_fetch_stride: [0; 96],
            vertex_fetch_tail: [0; 96],
            fetch_pairs_read: [0; 3],
            constants_read: 0,
            addressed_constants: false,
            colour_mask: 0,
            writes_depth: false,
            writes_memory: false,
            instruction_count: 0,
            dropped_exports: 0,
            dropped_memory_exports: 0,
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Translation {
    pub ok: bool,
    pub error: String, // why not, when not
    pub spirv: Vec<u32>,
    pub meta: TranslationMeta,
}

#[derive(Clone, Copy)]
pub(super) struct RecordRead {
    pub block: u32,
    pub at: usize,
}

// The blocks a read can be in. A straight-line program is one block
// up to its deferred kill; one with the dispatcher has the entry
// block, the loop header, the block that switches on the counter, a
// block per case (IN_CASE + the case) and the block after the loop.
// Each case is one block too: predication is a select, not a branch.
// Code that opens a block of its own has to say so here.
pub(super) const IN_ENTRY: u32 = 0;
pub(super) const IN_LOOP_HEADER: u32 = 1;
pub(super) const IN_DISPATCH: u32 = 2;
pub(super) const AFTER_LOOP: u32 = 3;
pub(super) const IN_CASE: u32 = 4;

// Xenos temporaries are vec4, and so is everything the ALU produces, so the
// translator works almost entirely in one type.
pub(super) struct Translator<'a> {
    pub pixel: bool,
    pub words: &'a [u32],
    // Whether the render targets may be larger than the title's
    // (bindings::RECORD_SCALED).
    pub scaled: bool,

    pub m: Module,
    pub result: Translation,

    pub flow: Vec<ControlFlow>,

    pub float: Id,
    pub float4: Id,
    pub bool4: Id,
    pub uint: Id,
    pub int: Id,
    pub void: Id,
    // One array rather than one variable per register: a destination can be
    // addressed relative to aL, which needs a computed index.
    pub temps: Id,

    // Where the draw's record is (bindings.rs): BaseVertex in a vertex
    // shader, which passes it on through record_output, and that in a
    // pixel shader. `record` is the dword offset itself, loaded in the
    // entry block so every block can use it.
    pub record_input: Id,
    pub record_output: Id,
    pub record: Id,
    // Each word of the record the program reads, loaded once
    // (record_word, place_record_loads): its value, and where it is read --
    // the block and the place in the code, in the order emitted.
    pub record_words: [Id; RECORD_WORDS],
    pub record_reads: Vec<Vec<RecordRead>>,
    pub block: u32,
    // A place in the entry block, the loop header and the dispatch block
    // ahead of where each branches on to the blocks it dominates.
    pub block_end: [usize; 3],
    pub position: Id,
    pub vertex_index: Id, // the index the sequencer puts in r0.x
    pub interpolators: [Id; INTERPOLATOR_COUNT as usize],
    pub point_size: Id,
    pub colours: [Id; 4],
    pub textures: [bool; 32],                               // the slots sampled
    pub texture_types: [Id; 32],                            // the sampled image a slot's fetches go through
    pub texture_image_types: [Id; 32],                      // and the image behind it
    pub image_arrays: [Id; bindings::IMAGE_KINDS as usize], // set 1's image array, as each kind sampled
    pub samplers: Id,
    // A slot is only ever sampled at one dimensionality within one program.
    pub texture_dimensions: [u32; 32],
    pub arena: Id,        // the frame arena, as dwords
    pub arena_floats: Id, // and as vec4

    // Sequencer state, in function variables because the dispatch loop reads
    // and writes it across blocks.
    pub pc: Id,               // which control flow instruction is next
    pub address_register: Id, // aL, the loop address register
    pub a0: Id,               // a0, set by the maxa family
    pub lod: Id,              // the level of detail setTexLOD wrote
    pub killed: Id,           // pointer to a Function bool
    pub have_kill: bool,
    pub loop_depth: Id,
    pub loop_iterators: Id,
    pub loop_ids: Id,
    pub call_depth: Id,
    pub call_stack: Id,
    // A mini fetch reuses the address the preceding full fetch computed, and the
    // two need not share a basic block, so it lives in a variable.
    pub last_fetch_base: Id,   // pointer to a Function uint
    pub last_fetch_endian: Id, // the fetch constant's endianness, 2 bits
    pub have_fetch_base: bool, // whether any full fetch precedes

    // What the previous scalar produced, for the *_prev family. A variable,
    // not an SSA value: producer and reader can be in different blocks.
    pub previous_scalar: Id, // pointer to a Function float
    pub have_previous_scalar: bool,

    // Xenos predication is not branching: the instruction runs and its write
    // is suppressed, so it maps to a select on every store.
    pub predicate: Id, // pointer to a Function bool
    pub condition: Id, // bool value, or 0 for unconditional
    pub bool_: Id,
    pub entry_interface: Vec<Id>,
}

impl<'a> Translator<'a> {
    fn new(pixel: bool, words: &'a [u32], scaled: bool) -> Self {
        Translator {
            pixel,
            words,
            scaled,
            m: Module::new(),
            result: Translation::default(),
            flow: Vec::new(),
            float: 0,
            float4: 0,
            bool4: 0,
            uint: 0,
            int: 0,
            void: 0,
            temps: 0,
            record_input: 0,
            record_output: 0,
            record: 0,
            record_words: [0; RECORD_WORDS],
            record_reads: vec![Vec::new(); RECORD_WORDS],
            block: IN_ENTRY,
            block_end: [0; 3],
            position: 0,
            vertex_index: 0,
            interpolators: [0; INTERPOLATOR_COUNT as usize],
            point_size: 0,
            colours: [0; 4],
            textures: [false; 32],
            texture_types: [0; 32],
            texture_image_types: [0; 32],
            image_arrays: [0; bindings::IMAGE_KINDS as usize],
            samplers: 0,
            texture_dimensions: [0; 32],
            arena: 0,
            arena_floats: 0,
            pc: 0,
            address_register: 0,
            a0: 0,
            lod: 0,
            killed: 0,
            have_kill: false,
            loop_depth: 0,
            loop_iterators: 0,
            loop_ids: 0,
            call_depth: 0,
            call_stack: 0,
            last_fetch_base: 0,
            last_fetch_endian: 0,
            have_fetch_base: false,
            previous_scalar: 0,
            have_previous_scalar: false,
            predicate: 0,
            condition: 0,
            bool_: 0,
            entry_interface: Vec::new(),
        }
    }

    pub(super) fn fail(&mut self, what: &str, value: u32) {
        if !self.result.error.is_empty() {
            return; // keep the first failure
        }
        // snprintf into 128 bytes.
        let mut text = format!("{what} {value} is not translated yet");
        truncate_c(&mut text, 128);
        self.result.error = text;
    }

    // An instruction with a result: its id is allocated after its operands,
    // which the caller has already made.
    #[inline]
    pub(super) fn emit(&mut self, op: u32, ty: Id, operands: &[u32]) -> Id {
        let id = self.m.allocate();
        let code = &mut self.m.code;
        code.push(spirv::head(op, operands.len() as u32 + 3));
        code.push(ty);
        code.push(id);
        code.extend_from_slice(operands);
        id
    }

    #[inline]
    pub(super) fn glsl450(&mut self, instruction: u32, ty: Id, operands: &[u32]) -> Id {
        let id = self.m.allocate();
        let set = self.m.glsl_set();
        let code = &mut self.m.code;
        code.push(spirv::head(op::EXT_INST, operands.len() as u32 + 5));
        code.push(ty);
        code.push(id);
        code.push(set);
        code.push(instruction);
        code.extend_from_slice(operands);
        id
    }

    // An instruction without a result, into the function's code.
    #[inline]
    pub(super) fn code(&mut self, op: u32, operands: &[u32]) {
        spirv::emit(&mut self.m.code, op, operands);
    }

    #[inline]
    pub(super) fn splat(&mut self, value: f32) -> Id {
        self.m.constant_splat_f(value)
    }

    #[inline]
    pub(super) fn component(&mut self, vector: Id, index: u32) -> Id {
        let ty = self.float;
        self.emit(op::COMPOSITE_EXTRACT, ty, &[vector, index])
    }

    #[inline]
    pub(super) fn build_vector(&mut self, x: Id, y: Id, z: Id, w: Id) -> Id {
        let ty = self.float4;
        self.emit(op::COMPOSITE_CONSTRUCT, ty, &[x, y, z, w])
    }

    pub(super) fn set_pc_to(&mut self, index: u32) {
        let value = self.m.constant_u(index);
        let pc = self.pc;
        self.code(op::STORE, &[pc, value]);
    }

    pub(super) fn set_pc(&mut self, value: Id) {
        let pc = self.pc;
        self.code(op::STORE, &[pc, value]);
    }

    fn decode_control_flow(&mut self) -> bool {
        let words = self.words;
        let mut i = 0usize;
        while i + 3 <= words.len() && self.flow.len() < 512 {
            let (first, second) = unpack_control_flow(words[i], words[i + 1], words[i + 2]);
            i += 3;
            for cf in [first, second] {
                self.flow.push(cf);
                if cf.is_end() {
                    return true;
                }
            }
        }
        self.result.error = String::from("control flow does not terminate");
        false
    }

    // What the program uses, read before anything is emitted: the interface is
    // declared up front, and some of it -- whether a mini fetch has an address
    // to reuse -- is a property of the whole program, not of the instruction.
    fn analyze(&mut self) {
        let words = self.words;
        let meta = &mut self.result.meta;
        let mut last_slot = bindings::FETCH_SLOTS;
        for cf in &self.flow {
            if !cf.is_exec() {
                continue;
            }
            for n in 0..cf.count() {
                let at = (cf.address() + n) as usize * 3;
                if at + 3 > words.len() {
                    continue;
                }
                if cf.is_fetch(n) {
                    let f = Fetch { w0: words[at], w1: words[at + 1], w2: words[at + 2] };
                    // A fetch reads its coordinates from a temporary, and for
                    // a pixel shader that is an input like any other read
                    // below: often the only read of that interpolator.
                    if self.pixel && f.source_register() < INTERPOLATOR_COUNT {
                        meta.interpolator_mask |= 1 << f.source_register();
                    }
                    if f.opcode() == fetch_op::TEXTURE_FETCH {
                        let slot = f.constant_index() & 31;
                        meta.texture_mask |= 1 << slot;
                        self.texture_dimensions[slot as usize] = f.dimension();
                    } else if f.opcode() == fetch_op::VERTEX_FETCH {
                        meta.vertex_fetch_mask |= 1 << f.constant_index();
                        // The full fetch that computes the address a mini fetch
                        // reuses sits in an earlier control flow instruction.
                        // Only a full one names a constant, and so a slot.
                        if !f.is_mini_fetch() && f.must_be_one() {
                            self.have_fetch_base = true;
                            if f.constant_select() < 3 {
                                meta.vertex_fetch_subs[f.constant_select() as usize] |= 1 << f.constant_index();
                                let pair = f.constant_index() * 3 + f.constant_select();
                                meta.vertex_fetch_pairs[(pair / 32) as usize] |= 1 << (pair % 32);
                                last_slot = f.vertex_fetch_slot();
                            }
                        }
                        // A mini fetch reuses the last full fetch's address, so its
                        // reach belongs to that slot.
                        if last_slot < bindings::FETCH_SLOTS {
                            let stride = &mut meta.vertex_fetch_stride[last_slot as usize];
                            *stride = (*stride).max(f.stride());
                            let tail = &mut meta.vertex_fetch_tail[last_slot as usize];
                            *tail = (*tail).max(f.offset().max(0) as u32 + 4);
                        }
                    }
                    continue;
                }
                let a = Alu { w0: words[at], w1: words[at + 1], w2: words[at + 2] };
                // A pixel shader receives the interpolators pre-loaded into the
                // low temporaries, so a temporary it reads is an input. It has to
                // declare every one the vertex shader might write, since the two
                // are translated apart and the pipeline matches them by location.
                if self.pixel {
                    for s in 1..=3 {
                        if a.source_is_temp(s) {
                            let reg = a.source_register(s) & 0x3F;
                            if reg < INTERPOLATOR_COUNT {
                                meta.interpolator_mask |= 1 << reg;
                            }
                        }
                    }
                }
                if !a.is_export() {
                    continue;
                }
                let dest = a.vector_dest();
                if !self.pixel {
                    if (EXPORT_MEMORY_ADDRESS..=EXPORT_MEMORY_LAST).contains(&dest) {
                        meta.writes_memory = true;
                    }
                } else if dest < 4 {
                    meta.colour_mask |= 1 << dest;
                } else if dest == EXPORT_DEPTH {
                    meta.writes_depth = true;
                }
            }
        }

        // And a vertex shader has to write every interpolator a pixel shader might
        // read: an input with no output behind it is invalid, and the pixel side
        // counts more than any one vertex shader exports. So every vertex shader
        // exports all sixteen, zeroed until the program writes them -- sixty-four
        // components, inside the device's 128.
        if !self.pixel {
            meta.interpolator_mask = (1 << INTERPOLATOR_COUNT) - 1;
        }
    }

    // A block holding one array: `count` elements of `element`, `stride` bytes
    // apart. With `count` 0 the array is unsized and the block a read-only
    // storage buffer, spelt as SPIR-V 1.0 spelt one -- a BufferBlock struct in
    // the Uniform storage class -- which 1.3 still accepts.
    fn declare_array_block(&mut self, element: Id, count: u32, stride: u32, set: u32, binding: u32, name: &str) -> Id {
        let storage_buffer = count == 0;
        let array = if storage_buffer { self.m.runtime_array_of(element) } else { self.m.array_of(element, count) };
        spirv::emit(&mut self.m.decorations, op::DECORATE, &[array, decoration::ARRAY_STRIDE, stride]);
        let block = self.m.allocate();
        spirv::emit(&mut self.m.declarations, op::TYPE_STRUCT, &[block, array]);
        let kind = if storage_buffer { decoration::BUFFER_BLOCK } else { decoration::BLOCK };
        spirv::emit(&mut self.m.decorations, op::DECORATE, &[block, kind]);
        spirv::emit(&mut self.m.decorations, op::MEMBER_DECORATE, &[block, 0, decoration::OFFSET, 0]);
        if storage_buffer {
            spirv::emit(&mut self.m.decorations, op::MEMBER_DECORATE, &[block, 0, decoration::NON_WRITABLE]);
        }
        let pointer = self.m.pointer(storage::UNIFORM, block);
        let variable = self.m.allocate();
        spirv::emit(&mut self.m.declarations, op::VARIABLE, &[pointer, variable, storage::UNIFORM]);
        spirv::emit(&mut self.m.decorations, op::DECORATE, &[variable, decoration::DESCRIPTOR_SET, set]);
        spirv::emit(&mut self.m.decorations, op::DECORATE, &[variable, decoration::BINDING, binding]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[variable], name);
        variable
    }

    // The size the title gave a texture, from the size of the image bound for
    // it. They differ for a resolve's copy when the render targets are larger
    // than the title's: the copy is the frame's scale or the shadow maps'
    // times the surface each way, and the title's half texels and
    // unnormalised coordinates are still the surface's.
    pub(super) fn guest_size(&mut self, size: Id, slot: u32, components: u32) -> Id {
        if !self.scaled {
            return size;
        }
        // member(0, uint), member(2, float), and 1.0, in that order.
        let frame_mask = self.record_word(bindings::RECORD_SCALED);
        let frame_bound = self.scaled_bound(frame_mask, slot);
        let frame_factor = self.record_float(bindings::RECORD_SCALED + 2);
        let one = self.m.constant_f(1.0);
        let float = self.float;
        let frame = self.emit(op::SELECT, float, &[frame_bound, frame_factor, one]);
        let shadow_mask = self.record_word(bindings::RECORD_SCALED + 1);
        let shadow_bound = self.scaled_bound(shadow_mask, slot);
        let shadow_factor = self.record_float(bindings::RECORD_SCALED + 3);
        let factor = self.emit(op::SELECT, float, &[shadow_bound, shadow_factor, frame]);
        let ty = if components == 3 { self.m.float3() } else { self.m.float2() };
        self.emit(op::VECTOR_TIMES_SCALAR, ty, &[size, factor])
    }

    // Whether `slot`'s bit is set in a mask of the scaled record.
    fn scaled_bound(&mut self, mask: Id, slot: u32) -> Id {
        let bit = self.m.constant_u(1 << slot);
        let uint = self.uint;
        let masked = self.emit(op::BITWISE_AND, uint, &[mask, bit]);
        let zero = self.m.constant_u(0);
        let bool_ = self.bool_;
        self.emit(op::I_NOT_EQUAL, bool_, &[masked, zero])
    }

    fn declare_interface(&mut self) {
        self.void = self.m.void();
        self.float = self.m.float();
        self.float4 = self.m.float4();
        self.bool_ = self.m.bool();
        self.bool4 = self.m.vector(self.bool_, 4);
        self.uint = self.m.int(false);
        self.int = self.m.int(true);

        let pointer_input_float4 = self.m.pointer(storage::INPUT, self.float4);
        let pointer_output_float4 = self.m.pointer(storage::OUTPUT, self.float4);

        // Every constant is read from the frame arena, at the offset the draw's
        // record holds for its block (bindings.rs).
        self.arena = self.declare_array_block(self.uint, 0, 4, bindings::ARENA_SET, bindings::ARENA_BINDING, "arena");
        self.arena_floats =
            self.declare_array_block(self.float4, 0, 16, bindings::ARENA_SET, bindings::ARENA_FLOATS, "arena_floats");

        // Xenos can be told to skip the viewport transform, and then a vertex
        // program emits window coordinates itself -- D3D9's XYZRHW, a
        // pre-transformed vertex. Vulkan has no such mode: whatever a program
        // writes is clipped against w before any viewport is applied, so a
        // vertex at window x = 960 with w = 1 is outside the clip volume and the
        // primitive it belongs to is thrown away entirely. MW2 clears colour and
        // depth with exactly that, which is why the clears were measured
        // colouring nought and two samples of the 1228800 a frame holds.
        //
        // So the conversion happens here, as Xenia does it: the position is
        // multiplied and offset on the way out by a transform in the draw's
        // record. With the viewport transform on it is the identity and costs a
        // multiply-add; with it off it is `ndc = 2 * window / size - 1`, scaled
        // by w so it survives the perspective divide.
        //
        // The alpha test is fixed-function on the console and does not exist in
        // Vulkan at all, so the comparison has to be in the shader. Its function
        // and reference are register state, not shader state -- the same program
        // is drawn with the test on and off -- so they are in the record too,
        // rather than making a pipeline of their own for every reference value.
        // Then two to the power of RB_COLOR_INFO's exponent bias, and non-zero
        // when the colour target is 8_8_8_8_GAMMA.
        //
        // The record is found through the draw's base vertex, which the
        // vertex stage passes on flat (bindings.rs).
        {
            let pointer_uint = self.m.pointer(if self.pixel { storage::INPUT } else { storage::OUTPUT }, self.uint);
            let flat = |t: &mut Self, variable: Id, input: bool| {
                let class = if input { storage::INPUT } else { storage::OUTPUT };
                spirv::emit(&mut t.m.declarations, op::VARIABLE, &[pointer_uint, variable, class]);
                spirv::emit(
                    &mut t.m.decorations,
                    op::DECORATE,
                    &[variable, decoration::LOCATION, bindings::RECORD_LOCATION],
                );
                spirv::emit(&mut t.m.decorations, op::DECORATE, &[variable, decoration::FLAT]);
                spirv::emit_string(&mut t.m.debug, op::NAME, &[variable], "record");
                t.entry_interface.push(variable);
            };
            if !self.pixel {
                self.m.require_capability(CAPABILITY_DRAW_PARAMETERS);
                self.record_input = self.m.allocate();
                let pointer = self.m.pointer(storage::INPUT, self.int);
                spirv::emit(&mut self.m.declarations, op::VARIABLE, &[pointer, self.record_input, storage::INPUT]);
                spirv::emit(
                    &mut self.m.decorations,
                    op::DECORATE,
                    &[self.record_input, decoration::BUILT_IN, spirv::built_in::BASE_VERTEX],
                );
                spirv::emit_string(&mut self.m.debug, op::NAME, &[self.record_input], "base_vertex");
                self.entry_interface.push(self.record_input);
                self.record_output = self.m.allocate();
                let output = self.record_output;
                flat(self, output, false);
            } else {
                self.m.require_capability(CAPABILITY_GROUP_NON_UNIFORM);
                self.m.require_capability(CAPABILITY_GROUP_NON_UNIFORM_BALLOT);
                self.record_input = self.m.allocate();
                let input = self.record_input;
                flat(self, input, true);
            }
        }

        for slot in 0..32usize {
            if self.result.meta.texture_mask & (1 << slot) == 0 {
                continue;
            }
            // Dimensionality comes from the fetch. A cube map is a cube, sampled
            // by the direction its face and coordinates stand for.
            // A 1D fetch samples a 2D image at v = 0: the texture cache makes
            // every 1D texture a 2D image one row tall, and a slot may be bound
            // to a 2D texture the program only ever reads along a line, so a 1D
            // type here was a view-type mismatch on every such draw.
            // SPIR-V Dim: 1 = 2D, 2 = 3D, 3 = Cube
            let dim = match self.texture_dimensions[slot] {
                dimension::D3 => 2,
                dimension::CUBE => 3,
                _ => 1,
            };
            let kind: u32 = if dim == 2 {
                2
            } else if dim == 3 {
                1
            } else {
                0
            };
            self.result.meta.texture_kinds[slot] = kind as u8;
            let image = self.m.image(self.float, dim, false);
            self.texture_image_types[slot] = image;
            self.texture_types[slot] = self.m.sampled_image(image);
            self.textures[slot] = true;
            // Set 1's image array, as this kind, and the samplers (bindings.rs).
            // Every kind's variable names the same binding.
            let declare = |t: &mut Self, element: Id, count: u32, binding: u32, name: &str| -> Id {
                let array = t.m.array_of(element, count);
                let pointer = t.m.pointer(storage::UNIFORM_CONSTANT, array);
                let variable = t.m.allocate();
                spirv::emit(&mut t.m.declarations, op::VARIABLE, &[pointer, variable, storage::UNIFORM_CONSTANT]);
                spirv::emit(
                    &mut t.m.decorations,
                    op::DECORATE,
                    &[variable, decoration::DESCRIPTOR_SET, bindings::TEXTURE_SET],
                );
                spirv::emit(&mut t.m.decorations, op::DECORATE, &[variable, decoration::BINDING, binding]);
                spirv::emit_string(&mut t.m.debug, op::NAME, &[variable], name);
                variable
            };
            if self.image_arrays[kind as usize] == 0 {
                let name = if kind == 2 {
                    "volumes"
                } else if kind == 1 {
                    "cubes"
                } else {
                    "images"
                };
                self.image_arrays[kind as usize] = declare(self, image, bindings::IMAGE_COUNT, bindings::IMAGES, name);
            }
            if self.samplers == 0 {
                let sampler = self.m.sampler();
                self.samplers = declare(self, sampler, bindings::SAMPLER_COUNT, bindings::SAMPLERS, "samplers");
            }
            // Before SPIR-V 1.4 the entry point's interface lists only Input and
            // Output variables.
        }

        for i in 0..INTERPOLATOR_COUNT {
            if self.result.meta.interpolator_mask & (1 << i) == 0 {
                continue;
            }
            let input = self.pixel;
            let variable = self.m.allocate();
            let (pointer, class) =
                if input { (pointer_input_float4, storage::INPUT) } else { (pointer_output_float4, storage::OUTPUT) };
            spirv::emit(&mut self.m.declarations, op::VARIABLE, &[pointer, variable, class]);
            spirv::emit(&mut self.m.decorations, op::DECORATE, &[variable, decoration::LOCATION, i]);
            self.interpolators[i as usize] = variable;
            self.entry_interface.push(variable);
        }

        if !self.pixel {
            self.position = self.m.allocate();
            spirv::emit(
                &mut self.m.declarations,
                op::VARIABLE,
                &[pointer_output_float4, self.position, storage::OUTPUT],
            );
            spirv::emit(
                &mut self.m.decorations,
                op::DECORATE,
                &[self.position, decoration::BUILT_IN, spirv::built_in::POSITION],
            );
            self.entry_interface.push(self.position);

            // A point list needs a point size written, and the program's own
            // export of one is not honoured yet; one pixel is Vulkan's default.
            let pointer_output_float = self.m.pointer(storage::OUTPUT, self.float);
            self.point_size = self.m.allocate();
            spirv::emit(
                &mut self.m.declarations,
                op::VARIABLE,
                &[pointer_output_float, self.point_size, storage::OUTPUT],
            );
            spirv::emit(
                &mut self.m.decorations,
                op::DECORATE,
                &[self.point_size, decoration::BUILT_IN, spirv::built_in::POINT_SIZE],
            );
            self.entry_interface.push(self.point_size);

            // Xenos has no vertex input stage: the sequencer puts the index of the vertex
            // being shaded into r0.x as a float, and the program's own fetch computes an
            // address from it. Without this every vertex fetches vertex zero, collapsing
            // every primitive to a point.
            let pointer_input_int = self.m.pointer(storage::INPUT, self.int);
            self.vertex_index = self.m.allocate();
            spirv::emit(
                &mut self.m.declarations,
                op::VARIABLE,
                &[pointer_input_int, self.vertex_index, storage::INPUT],
            );
            spirv::emit(
                &mut self.m.decorations,
                op::DECORATE,
                &[self.vertex_index, decoration::BUILT_IN, spirv::built_in::VERTEX_INDEX],
            );
            spirv::emit_string(&mut self.m.debug, op::NAME, &[self.vertex_index], "vertex_index");
            self.entry_interface.push(self.vertex_index);
        } else {
            // Only the targets the program writes: an output left undefined would feed
            // the blender with whatever was in the register.
            for i in 0..4u32 {
                if self.result.meta.colour_mask & (1 << i) == 0 {
                    continue;
                }
                let colour = self.m.allocate();
                self.colours[i as usize] = colour;
                spirv::emit(&mut self.m.declarations, op::VARIABLE, &[pointer_output_float4, colour, storage::OUTPUT]);
                spirv::emit(&mut self.m.decorations, op::DECORATE, &[colour, decoration::LOCATION, i]);
                self.entry_interface.push(colour);
            }
        }
    }

    // Word `index` of the draw's record: one value for the whole program,
    // loaded where place_record_loads puts it once every read is known.
    pub(super) fn record_word(&mut self, index: u32) -> Id {
        let index = index as usize;
        if self.record_words[index] == 0 {
            self.record_words[index] = self.m.allocate();
        }
        let read = RecordRead { block: self.block, at: self.m.code.len() };
        self.record_reads[index].push(read);
        self.record_words[index]
    }

    pub(super) fn record_float(&mut self, index: u32) -> Id {
        let word = self.record_word(index);
        let float = self.float;
        self.emit(op::BITCAST, float, &[word])
    }

    // Each record word's one load, in the narrowest block that dominates every
    // read of it -- the block all its reads are in, if they share one, else
    // the nearest block the blocks they are in all descend from -- and there
    // ahead of the first read. That is where a driver would sink a load made
    // in the entry block; RADV does, not every driver does, so none has to: a
    // straight-line program loads a word at its first read, and the
    // dispatcher in the one case that reads it, or at the top of the
    // iteration for several. Loading the words before the dispatcher's loop
    // instead, once a draw, kept each of them live across the whole loop, and
    // RADV spilled scalar registers in 1,089 pixel shaders.
    pub(super) fn place_record_loads(&mut self) {
        // The blocks' dominator tree: the entry block, then the loop header,
        // which dominates the dispatch block and the block after the loop;
        // the dispatch block dominates the cases.
        fn parent(block: u32) -> u32 {
            if block >= IN_CASE {
                return IN_DISPATCH;
            }
            if block == IN_DISPATCH || block == AFTER_LOOP {
                return IN_LOOP_HEADER;
            }
            IN_ENTRY
        }
        fn depth(block: u32) -> u32 {
            if block >= IN_CASE {
                return 3;
            }
            if block == IN_DISPATCH || block == AFTER_LOOP {
                return 2;
            }
            if block == IN_LOOP_HEADER { 1 } else { 0 }
        }
        // (where, which word)
        let mut loads: Vec<(usize, u32)> = Vec::new();
        for index in 0..bindings::RECORD_WORDS {
            let reads = &self.record_reads[index as usize];
            if reads.is_empty() {
                continue;
            }
            let mut common = reads[0].block;
            for read in reads {
                let mut block = read.block;
                while depth(block) > depth(common) {
                    block = parent(block);
                }
                while depth(common) > depth(block) {
                    common = parent(common);
                }
                while block != common {
                    block = parent(block);
                    common = parent(common);
                }
            }
            // The first read is in that block, or in one it dominates.
            let at = if reads[0].block == common { reads[0].at } else { self.block_end[common as usize] };
            loads.push((at, index));
        }
        // From the last place back, so the places before stay where they were.
        // A stable sort, as std::stable_sort.
        loads.sort_by_key(|&(at, _)| std::cmp::Reverse(at));
        for (at, index) in loads {
            let address = self.m.allocate();
            let pointer = self.m.allocate();
            let offset = self.m.constant_u(index);
            let uniform_uint = self.m.pointer(storage::UNIFORM, self.uint);
            let zero = self.m.constant_s(0);
            let words = [
                spirv::head(op::I_ADD, 5),
                self.uint,
                address,
                self.record,
                offset,
                spirv::head(op::ACCESS_CHAIN, 6),
                uniform_uint,
                pointer,
                self.arena,
                zero,
                address,
                spirv::head(op::LOAD, 4),
                self.uint,
                self.record_words[index as usize],
                pointer,
            ];
            self.m.code.splice(at..at, words);
        }
    }

    fn run(mut self) -> Translation {
        if self.words.len() < 3 {
            self.result.error = String::from("program is too short");
            return self.result;
        }
        if !self.decode_control_flow() {
            return self.result;
        }
        self.analyze();
        self.declare_interface();
        self.emit_body();
        if !self.result.error.is_empty() {
            return self.result;
        }
        self.result.spirv = self.m.finish();
        self.result.ok = true;
        self.result
    }
}

// `words` is the program in host byte order. `scaled`: the render targets may
// be larger than the title's, and the shader reads the record's
// bindings::RECORD_SCALED.
pub fn translate(pixel: bool, words: &[u32], scaled: bool) -> Translation {
    Translator::new(pixel, words, scaled).run()
}

// What snprintf leaves of `text` in a buffer of `size` bytes.
fn truncate_c(text: &mut String, size: usize) {
    if text.len() > size - 1 {
        let mut end = size - 1;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

// The same in bytes, as snprintf cuts them, whatever they are.
fn truncate_bytes(text: &mut Vec<u8>, size: usize) {
    text.truncate(size - 1);
}

#[expect(dead_code, reason = "its callers are the renderer's, still C++ until S9")]
pub fn describe(result: &Translation) -> String {
    let text = describe_parts(result.ok, result.error.as_bytes(), result.spirv.len(), &result.meta);
    String::from_utf8_lossy(&text).into_owned()
}

// Describe from the parts, in bytes, for the C interface, which has no
// Translation: the error is what %s reads of it, up to its first NUL, and the
// text is cut into 320 bytes, the dropped clause into 48, as the C++'s
// buffers cut them.
pub fn describe_parts(ok: bool, error: &[u8], spirv_words: usize, meta: &TranslationMeta) -> Vec<u8> {
    if !ok {
        let end = error.iter().position(|&b| b == 0).unwrap_or(error.len());
        let mut text = b"not translated: ".to_vec();
        text.extend_from_slice(&error[..end]);
        truncate_bytes(&mut text, 320);
        return text;
    }
    let mut dropped = String::new();
    let total = meta.dropped_exports.wrapping_add(meta.dropped_memory_exports);
    if meta.dropped_exports != 0 || meta.dropped_memory_exports != 0 {
        dropped = format!(
            ", {} export{} dropped{}",
            total,
            if total == 1 { "" } else { "s" },
            if meta.dropped_memory_exports != 0 { " (to memory)" } else { "" }
        );
        truncate_c(&mut dropped, 48);
    }
    let mut text = format!(
        "{} instructions -> {} SPIR-V words; interpolators {:04X}, colours {:X}, textures {:08X}, vertex fetches {:08X}{}{}{}",
        meta.instruction_count,
        spirv_words,
        meta.interpolator_mask,
        meta.colour_mask,
        meta.texture_mask,
        meta.vertex_fetch_mask,
        if meta.writes_depth { ", writes depth" } else { "" },
        if meta.writes_memory { ", memory export" } else { "" },
        dropped
    );
    truncate_c(&mut text, 320);
    text.into_bytes()
}
