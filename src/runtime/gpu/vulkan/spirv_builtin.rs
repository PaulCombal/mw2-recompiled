// The renderer's own shaders, which no title gives it: the drawn depth resolve's two
// and the display table's, written with the translator's writer (shaders/src/shader/
// spirv.rs). The builder methods only these use are the Builtin trait here, outside the
// shaders crate the shader build id hashes, so a change to a built-in shader recompiles
// no title shader.
//
// The order of every allocation and emission is fixed, arguments evaluated before the
// call they are passed to: an inner instruction is written before the outer, a variable's
// id is taken before its pointer type is declared. MW2_DUMP_SHADERS dumps each.

use crate::runtime::gpu::shader::spirv::{Id, Module, built_in, decoration, emit, head, op, storage};

// Execution models and modes, and the image fetch, which the translator does not use.
const MODEL_VERTEX: u32 = 0;
const MODEL_FRAGMENT: u32 = 4;
const MODEL_GL_COMPUTE: u32 = 5;
const MODE_ORIGIN_UPPER_LEFT: u32 = 7;
const MODE_DEPTH_REPLACING: u32 = 12;
const MODE_LOCAL_SIZE: u32 = 17;
const OP_IMAGE_FETCH: u32 = 95;
const IMAGE_OPERANDS_SAMPLE: u32 = 0x40;
const IMAGE_FORMAT_RGBA8: u32 = 4;
// The display table's workgroup: 8x8.
pub const DISPLAY_GROUP: u32 = 8;

/// The builder methods only the built-in shaders use, on the translator's writer.
pub trait Builtin {
    fn result(&mut self, op: u32, ty: Id, operands: &[u32]) -> Id;
    fn global(&mut self, storage: u32, ty: Id) -> Id;
    fn main_function(&mut self, model: u32, interface: &[Id]) -> Id;
    fn end_function(&mut self) -> Vec<u32>;
    fn storage_image(&mut self, sampled_type: Id, dim: u32, format: u32) -> Id;
}

impl Builtin for Module {
    /// An instruction with a result: its id taken after its operands were written.
    fn result(&mut self, op: u32, ty: Id, operands: &[u32]) -> Id {
        let result = self.allocate();
        self.code.push(head(op, operands.len() as u32 + 3));
        self.code.push(ty);
        self.code.push(result);
        self.code.extend_from_slice(operands);
        result
    }

    /// A global variable of `ty` in `storage`: the id first, then its pointer type.
    fn global(&mut self, storage: u32, ty: Id) -> Id {
        let variable = self.allocate();
        let pointer = self.pointer(storage, ty);
        emit(&mut self.declarations, op::VARIABLE, &[pointer, variable, storage]);
        variable
    }

    /// `void main()` of `model` and its first block.
    fn main_function(&mut self, model: u32, interface: &[Id]) -> Id {
        let function = self.allocate();
        let void = self.void();
        emit(&mut self.declarations, op::TYPE_FUNCTION, &[function, void]);
        let main = self.allocate();
        self.entry_point(model, main, "main", interface);
        let void = self.void();
        emit(&mut self.code, op::FUNCTION, &[void, main, 0, function]);
        let label = self.allocate();
        emit(&mut self.code, op::LABEL, &[label]);
        main
    }

    /// The function's end and the module.
    fn end_function(&mut self) -> Vec<u32> {
        emit(&mut self.code, op::RETURN, &[]);
        emit(&mut self.code, op::FUNCTION_END, &[]);
        self.finish()
    }

    /// A storage image of `sampled_type`: Sampled = 2, used without a sampler. Declared
    /// where it is asked for; a built-in shader asks once, so nothing here looks for an
    /// earlier one.
    fn storage_image(&mut self, sampled_type: Id, dim: u32, format: u32) -> Id {
        let id = self.allocate();
        emit(&mut self.declarations, op::TYPE_IMAGE, &[id, sampled_type, dim, 0, 0, 0, 2, format]);
        id
    }
}

/// A triangle over the viewport: (-1,-1), (3,-1), (-1,3).
pub fn cover_vertex_shader() -> Vec<u32> {
    let mut m = Module::new();
    let i32_ = m.int(true);
    let f32_ = m.float();
    let f4 = m.float4();
    let index = m.global(storage::INPUT, i32_);
    emit(&mut m.decorations, op::DECORATE, &[index, decoration::BUILT_IN, built_in::VERTEX_INDEX]);
    let position = m.global(storage::OUTPUT, f4);
    emit(&mut m.decorations, op::DECORATE, &[position, decoration::BUILT_IN, built_in::POSITION]);
    m.main_function(MODEL_VERTEX, &[index, position]);
    let i = m.result(op::LOAD, i32_, &[index]);
    let one = m.constant_s(1);
    let doubled = m.result(op::SHIFT_LEFT_LOGICAL, i32_, &[i, one]);
    let two = m.constant_s(2);
    let masked = m.result(op::BITWISE_AND, i32_, &[doubled, two]);
    let x = m.result(op::CONVERT_S_TO_F, f32_, &[masked]);
    let two = m.constant_s(2);
    let masked = m.result(op::BITWISE_AND, i32_, &[i, two]);
    let y = m.result(op::CONVERT_S_TO_F, f32_, &[masked]);
    let clip = |m: &mut Module, v: Id| {
        let two = m.constant_f(2.0);
        let times = m.result(op::F_MUL, f32_, &[v, two]);
        let one = m.constant_f(1.0);
        m.result(op::F_SUB, f32_, &[times, one])
    };
    let cx = clip(&mut m, x);
    let cy = clip(&mut m, y);
    let zero = m.constant_f(0.0);
    let one = m.constant_f(1.0);
    let at = m.result(op::COMPOSITE_CONSTRUCT, f4, &[cx, cy, zero, one]);
    emit(&mut m.code, op::STORE, &[position, at]);
    m.end_function()
}

/// depth = texelFetch(surface, ivec2(gl_FragCoord.xy), 0).r. The same words for every
/// depth format: the pipeline is per format.
pub fn fetch_depth_shader() -> Vec<u32> {
    let mut m = Module::new();
    let f32_ = m.float();
    let f2 = m.float2();
    let f4 = m.float4();
    let i2 = m.int_vector(2);
    // Sampled type, 2D, not depth, not arrayed, multisampled, sampled, no format.
    let image = m.allocate();
    emit(&mut m.declarations, op::TYPE_IMAGE, &[image, f32_, 1, 0, 0, 1, 1, 0]);
    let surface = m.global(storage::UNIFORM_CONSTANT, image);
    emit(&mut m.decorations, op::DECORATE, &[surface, decoration::DESCRIPTOR_SET, 0]);
    emit(&mut m.decorations, op::DECORATE, &[surface, decoration::BINDING, 0]);
    let frag_coord = m.global(storage::INPUT, f4);
    emit(&mut m.decorations, op::DECORATE, &[frag_coord, decoration::BUILT_IN, built_in::FRAG_COORD]);
    let frag_depth = m.global(storage::OUTPUT, f32_);
    emit(&mut m.decorations, op::DECORATE, &[frag_depth, decoration::BUILT_IN, built_in::FRAG_DEPTH]);
    let main = m.main_function(MODEL_FRAGMENT, &[frag_coord, frag_depth]);
    emit(&mut m.execution_modes, op::EXECUTION_MODE, &[main, MODE_ORIGIN_UPPER_LEFT]);
    emit(&mut m.execution_modes, op::EXECUTION_MODE, &[main, MODE_DEPTH_REPLACING]);
    let coord = m.result(op::LOAD, f4, &[frag_coord]);
    let xy = m.result(op::VECTOR_SHUFFLE, f2, &[coord, coord, 0, 1]);
    let texel = m.result(op::CONVERT_F_TO_S, i2, &[xy]);
    let loaded = m.result(op::LOAD, image, &[surface]);
    let zero = m.constant_s(0);
    let fetched = m.result(OP_IMAGE_FETCH, f4, &[loaded, texel, IMAGE_OPERANDS_SAMPLE, zero]);
    let depth = m.result(op::COMPOSITE_EXTRACT, f32_, &[fetched, 0]);
    emit(&mut m.code, op::STORE, &[frag_depth, depth]);
    m.end_function()
}

/// c = imageLoad(frame, id.xy); each channel indexes the table; store back.
pub fn display_table_shader() -> Vec<u32> {
    let mut m = Module::new();
    m.require_capability(1); // Shader

    let void_type = m.void();
    let f32_ = m.float();
    let f4 = m.float4();
    let u32_ = m.int(false);
    let i32_ = m.int(true);
    let i2 = m.vector(i32_, 2);
    let u3 = m.vector(u32_, 3);

    let image_type = m.storage_image(f32_, 1, IMAGE_FORMAT_RGBA8); // Dim 1 = 2D
    let image_pointer = m.pointer(storage::UNIFORM_CONSTANT, image_type);

    let frame = m.allocate();
    let table = m.allocate();
    for (variable, binding) in [(frame, 0), (table, 1)] {
        emit(&mut m.declarations, op::VARIABLE, &[image_pointer, variable, storage::UNIFORM_CONSTANT]);
        emit(&mut m.decorations, op::DECORATE, &[variable, decoration::DESCRIPTOR_SET, 0]);
        emit(&mut m.decorations, op::DECORATE, &[variable, decoration::BINDING, binding]);
    }

    let global_id = m.allocate();
    let global_pointer = m.pointer(storage::INPUT, u3);
    emit(&mut m.declarations, op::VARIABLE, &[global_pointer, global_id, storage::INPUT]);
    emit(&mut m.decorations, op::DECORATE, &[global_id, decoration::BUILT_IN, built_in::GLOBAL_INVOCATION_ID]);

    let function_type = m.allocate();
    emit(&mut m.declarations, op::TYPE_FUNCTION, &[function_type, void_type]);
    let entry = m.allocate();

    // Before SPIR-V 1.4 the interface lists only Input and Output variables.
    m.entry_point(MODEL_GL_COMPUTE, entry, "main", &[global_id]);
    emit(&mut m.execution_modes, op::EXECUTION_MODE, &[entry, MODE_LOCAL_SIZE, DISPLAY_GROUP, DISPLAY_GROUP, 1]);

    emit(&mut m.code, op::FUNCTION, &[void_type, entry, 0 /* None */, function_type]);
    let label = m.allocate();
    emit(&mut m.code, op::LABEL, &[label]);

    let id3 = m.result(op::LOAD, u3, &[global_id]);
    let x = m.result(op::COMPOSITE_EXTRACT, u32_, &[id3, 0]);
    let y = m.result(op::COMPOSITE_EXTRACT, u32_, &[id3, 1]);
    let sx = m.result(op::BITCAST, i32_, &[x]);
    let sy = m.result(op::BITCAST, i32_, &[y]);
    let at = m.result(op::COMPOSITE_CONSTRUCT, i2, &[sx, sy]);

    let frame_image = m.result(op::LOAD, image_type, &[frame]);
    let table_image = m.result(op::LOAD, image_type, &[table]);
    let colour = m.result(op::IMAGE_READ, f4, &[frame_image, at]);

    // Round rather than truncate: the value came from an eight-bit image, so it lands
    // exactly on an entry and floating point should not decide which.
    let scale = m.constant_f(255.0);
    let half = m.constant_f(0.5);
    let zero = m.constant_s(0);
    let mut components = [0; 4];
    for (c, component) in components.iter_mut().take(3).enumerate() {
        let value = m.result(op::COMPOSITE_EXTRACT, f32_, &[colour, c as u32]);
        let times = m.result(op::F_MUL, f32_, &[value, scale]);
        let plus = m.result(op::F_ADD, f32_, &[times, half]);
        let index = m.result(op::CONVERT_F_TO_S, i32_, &[plus]);
        let coordinate = m.result(op::COMPOSITE_CONSTRUCT, i2, &[index, zero]);
        let texel = m.result(op::IMAGE_READ, f4, &[table_image, coordinate]);
        *component = m.result(op::COMPOSITE_EXTRACT, f32_, &[texel, c as u32]);
    }
    components[3] = m.result(op::COMPOSITE_EXTRACT, f32_, &[colour, 3]);

    let out = m.result(op::COMPOSITE_CONSTRUCT, f4, &components);
    emit(&mut m.code, op::IMAGE_WRITE, &[frame_image, at, out]);
    emit(&mut m.code, op::RETURN, &[]);
    emit(&mut m.code, op::FUNCTION_END, &[]);
    m.finish()
}

// MW2_SUBGROUP_CHECK (subgroup_check.rs, diagnostics builds): the capabilities, the scope and the location the
// translated shaders use for a draw's record (shader/translator.rs, shader/bindings.rs).
#[cfg(feature = "diagnostics")]
const CAPABILITY_GROUP_NON_UNIFORM: u32 = 61;
#[cfg(feature = "diagnostics")]
const CAPABILITY_GROUP_NON_UNIFORM_BALLOT: u32 = 64;
#[cfg(feature = "diagnostics")]
const CAPABILITY_DRAW_PARAMETERS: u32 = 4427;
#[cfg(feature = "diagnostics")]
const SCOPE_SUBGROUP: u32 = 3;
#[cfg(feature = "diagnostics")]
const RECORD_LOCATION: u32 = crate::runtime::gpu::shader::bindings::RECORD_LOCATION;

/// The subgroup check's triangles: triangle k = VertexIndex / 3 is half of a cell of
/// `cell` pixels, cells laid `columns` to a row over a `width`x`height` target, the top
/// left half for an even k ((0,0), (1,0), (0,1), y down), the bottom right for an odd one
/// ((1,0), (1,1), (0,1)). The record handed on flat at the translated shaders' location is
/// BaseVertex, as the translated vertex shader hands its draw's on; `per_triangle` hands
/// on the triangle's own first vertex instead (3k, which BaseVertex is when each triangle
/// is its own draw), for the check's control, one draw of every triangle.
#[cfg(feature = "diagnostics")]
pub fn subgroup_check_vertex_shader(per_triangle: bool, columns: i32, cell: i32, width: u32, height: u32) -> Vec<u32> {
    let mut m = Module::new();
    m.require_capability(CAPABILITY_DRAW_PARAMETERS);
    let u32_ = m.int(false);
    let i32_ = m.int(true);
    let f32_ = m.float();
    let f4 = m.float4();
    let bool_ = m.bool();
    let index = m.global(storage::INPUT, i32_);
    emit(&mut m.decorations, op::DECORATE, &[index, decoration::BUILT_IN, built_in::VERTEX_INDEX]);
    let base = m.global(storage::INPUT, i32_);
    emit(&mut m.decorations, op::DECORATE, &[base, decoration::BUILT_IN, built_in::BASE_VERTEX]);
    let position = m.global(storage::OUTPUT, f4);
    emit(&mut m.decorations, op::DECORATE, &[position, decoration::BUILT_IN, built_in::POSITION]);
    let record = m.global(storage::OUTPUT, u32_);
    emit(&mut m.decorations, op::DECORATE, &[record, decoration::LOCATION, RECORD_LOCATION]);
    emit(&mut m.decorations, op::DECORATE, &[record, decoration::FLAT]);
    m.main_function(MODEL_VERTEX, &[index, base, position, record]);

    let vertex = m.result(op::LOAD, i32_, &[index]);
    let three = m.constant_s(3);
    let triangle = m.result(op::S_DIV, i32_, &[vertex, three]);
    let first = m.result(op::I_MUL, i32_, &[triangle, three]);
    let corner = m.result(op::I_SUB, i32_, &[vertex, first]);
    let value = if per_triangle { first } else { m.result(op::LOAD, i32_, &[base]) };
    let value = m.result(op::BITCAST, u32_, &[value]);
    emit(&mut m.code, op::STORE, &[record, value]);

    let one = m.constant_s(1);
    let two = m.constant_s(2);
    let cell_index = m.result(op::SHIFT_RIGHT_LOGICAL, i32_, &[triangle, one]);
    let half = m.result(op::BITWISE_AND, i32_, &[triangle, one]);
    let second = m.result(op::I_EQUAL, bool_, &[half, one]);
    let columns = m.constant_s(columns);
    let column = m.result(op::S_REM, i32_, &[cell_index, columns]);
    let row = m.result(op::S_DIV, i32_, &[cell_index, columns]);
    // Top left: x = corner & 1, y = corner >> 1. Bottom right: x = (corner ^ 2) >> 1,
    // y = (corner + 1) >> 1.
    let first_x = m.result(op::BITWISE_AND, i32_, &[corner, one]);
    let first_y = m.result(op::SHIFT_RIGHT_LOGICAL, i32_, &[corner, one]);
    let flipped = m.result(op::BITWISE_XOR, i32_, &[corner, two]);
    let second_x = m.result(op::SHIFT_RIGHT_LOGICAL, i32_, &[flipped, one]);
    let next = m.result(op::I_ADD, i32_, &[corner, one]);
    let second_y = m.result(op::SHIFT_RIGHT_LOGICAL, i32_, &[next, one]);
    let x = m.result(op::SELECT, i32_, &[second, second_x, first_x]);
    let y = m.result(op::SELECT, i32_, &[second, second_y, first_y]);
    let cell = m.constant_s(cell);
    let clip = |m: &mut Module, at: Id, offset: Id, size: u32| {
        let sum = m.result(op::I_ADD, i32_, &[at, offset]);
        let pixels = m.result(op::I_MUL, i32_, &[sum, cell]);
        let float = m.result(op::CONVERT_S_TO_F, f32_, &[pixels]);
        let scale = m.constant_f(2.0 / size as f32);
        let times = m.result(op::F_MUL, f32_, &[float, scale]);
        let one = m.constant_f(1.0);
        m.result(op::F_SUB, f32_, &[times, one])
    };
    let cx = clip(&mut m, column, x, width);
    let cy = clip(&mut m, row, y, height);
    let zero = m.constant_f(0.0);
    let one = m.constant_f(1.0);
    let at = m.result(op::COMPOSITE_CONSTRUCT, f4, &[cx, cy, zero, one]);
    emit(&mut m.code, op::STORE, &[position, at]);
    m.end_function()
}

/// out = record << 1 | (subgroupBroadcastFirst(record) != record): the translated pixel
/// shader's broadcast of its draw's record (shader/translator_flow.rs), the low bit set
/// where it handed an invocation another draw's, into an R32_UINT attachment; the record
/// above it, so that the check sees each pixel drawn by the draw that covers it.
#[cfg(feature = "diagnostics")]
pub fn subgroup_check_pixel_shader() -> Vec<u32> {
    let mut m = Module::new();
    m.require_capability(CAPABILITY_GROUP_NON_UNIFORM);
    m.require_capability(CAPABILITY_GROUP_NON_UNIFORM_BALLOT);
    let u32_ = m.int(false);
    let bool_ = m.bool();
    let record = m.global(storage::INPUT, u32_);
    emit(&mut m.decorations, op::DECORATE, &[record, decoration::LOCATION, RECORD_LOCATION]);
    emit(&mut m.decorations, op::DECORATE, &[record, decoration::FLAT]);
    let out = m.global(storage::OUTPUT, u32_);
    emit(&mut m.decorations, op::DECORATE, &[out, decoration::LOCATION, 0]);
    let main = m.main_function(MODEL_FRAGMENT, &[record, out]);
    emit(&mut m.execution_modes, op::EXECUTION_MODE, &[main, MODE_ORIGIN_UPPER_LEFT]);
    let flat = m.result(op::LOAD, u32_, &[record]);
    let scope = m.constant_u(SCOPE_SUBGROUP);
    let first = m.result(op::GROUP_NON_UNIFORM_BROADCAST_FIRST, u32_, &[scope, flat]);
    let differs = m.result(op::I_NOT_EQUAL, bool_, &[first, flat]);
    let yes = m.constant_u(1);
    let no = m.constant_u(0);
    let bit = m.result(op::SELECT, u32_, &[differs, yes, no]);
    let shifted = m.result(op::SHIFT_LEFT_LOGICAL, u32_, &[flat, yes]);
    let value = m.result(op::BITWISE_OR, u32_, &[shifted, bit]);
    emit(&mut m.code, op::STORE, &[out, value]);
    m.end_function()
}
