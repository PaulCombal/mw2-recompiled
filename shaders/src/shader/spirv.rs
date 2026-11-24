// A small SPIR-V binary writer: a five-word header, then instructions, each a
// word of (length << 16 | opcode) followed by its operands. Emitting that
// directly avoids a glslang or SPIRV-Tools dependency inside the runtime, which
// matters because shaders are translated while the title runs.
//
// The specification fixes the order of a module's parts, so each is built into
// its own buffer and they are concatenated in finish(). Ids are handed out in
// one sequence across all of them, so an id may be numerically higher than one
// appearing later in the stream, which SPIR-V allows.
//
// Only the methods the translator uses are here: this folder is what the
// shader build id has to hash (mod.rs). Methods only built-in shaders use go
// in a second `impl Module` block in a file outside it.

use std::collections::BTreeMap;

pub type Id = u32;

// Only what the translator needs; the values are from the specification.
pub mod op {
    pub const NOP: u32 = 0;
    pub const NAME: u32 = 5;
    pub const MEMBER_NAME: u32 = 6;
    pub const EXT_INST_IMPORT: u32 = 11;
    pub const EXT_INST: u32 = 12;
    pub const MEMORY_MODEL: u32 = 14;
    pub const ENTRY_POINT: u32 = 15;
    pub const EXECUTION_MODE: u32 = 16;
    pub const CAPABILITY: u32 = 17;
    pub const TYPE_VOID: u32 = 19;
    pub const TYPE_BOOL: u32 = 20;
    pub const TYPE_INT: u32 = 21;
    pub const TYPE_FLOAT: u32 = 22;
    pub const TYPE_VECTOR: u32 = 23;
    pub const TYPE_MATRIX: u32 = 24;
    pub const TYPE_IMAGE: u32 = 25;
    pub const TYPE_SAMPLER: u32 = 26;
    pub const TYPE_SAMPLED_IMAGE: u32 = 27;
    pub const TYPE_ARRAY: u32 = 28;
    pub const TYPE_RUNTIME_ARRAY: u32 = 29;
    pub const TYPE_STRUCT: u32 = 30;
    pub const TYPE_POINTER: u32 = 32;
    pub const TYPE_FUNCTION: u32 = 33;
    pub const CONSTANT_TRUE: u32 = 41;
    pub const CONSTANT_FALSE: u32 = 42;
    pub const CONSTANT: u32 = 43;
    pub const CONSTANT_COMPOSITE: u32 = 44;
    pub const FUNCTION: u32 = 54;
    pub const FUNCTION_END: u32 = 56;
    pub const FUNCTION_CALL: u32 = 57;
    pub const VARIABLE: u32 = 59;
    pub const LOAD: u32 = 61;
    pub const STORE: u32 = 62;
    pub const ACCESS_CHAIN: u32 = 65;
    pub const DECORATE: u32 = 71;
    pub const MEMBER_DECORATE: u32 = 72;
    pub const VECTOR_SHUFFLE: u32 = 79;
    pub const COMPOSITE_CONSTRUCT: u32 = 80;
    pub const COMPOSITE_EXTRACT: u32 = 81;
    pub const COMPOSITE_INSERT: u32 = 82;
    pub const TRANSPOSE: u32 = 84;
    pub const SAMPLED_IMAGE: u32 = 86;
    pub const IMAGE_SAMPLE_IMPLICIT_LOD: u32 = 87;
    pub const IMAGE_SAMPLE_EXPLICIT_LOD: u32 = 88;
    pub const IMAGE_READ: u32 = 98;
    pub const IMAGE_WRITE: u32 = 99;
    pub const IMAGE: u32 = 100;
    pub const IMAGE_QUERY_SIZE_LOD: u32 = 103;
    pub const DPDX: u32 = 207;
    pub const DPDY: u32 = 208;
    pub const CONVERT_F_TO_U: u32 = 109;
    pub const CONVERT_F_TO_S: u32 = 110;
    pub const CONVERT_S_TO_F: u32 = 111;
    pub const CONVERT_U_TO_F: u32 = 112;
    pub const BITCAST: u32 = 124;
    pub const S_NEGATE: u32 = 126;
    pub const F_NEGATE: u32 = 127;
    pub const I_ADD: u32 = 128;
    pub const F_ADD: u32 = 129;
    pub const I_SUB: u32 = 130;
    pub const F_SUB: u32 = 131;
    pub const I_MUL: u32 = 132;
    pub const F_MUL: u32 = 133;
    pub const U_DIV: u32 = 134;
    pub const S_DIV: u32 = 135;
    pub const F_DIV: u32 = 136;
    pub const U_MOD: u32 = 137;
    pub const S_REM: u32 = 138;
    pub const S_MOD: u32 = 139;
    pub const F_REM: u32 = 140;
    pub const F_MOD: u32 = 141;
    pub const VECTOR_TIMES_SCALAR: u32 = 142;
    pub const DOT: u32 = 148;
    pub const ANY: u32 = 154;
    pub const ALL: u32 = 155;
    pub const LOGICAL_OR: u32 = 166;
    pub const LOGICAL_AND: u32 = 167;
    pub const LOGICAL_NOT: u32 = 168;
    pub const SELECT: u32 = 169;
    pub const I_EQUAL: u32 = 170;
    pub const I_NOT_EQUAL: u32 = 171;
    pub const S_GREATER_THAN: u32 = 173;
    pub const S_GREATER_THAN_EQUAL: u32 = 174;
    pub const S_LESS_THAN: u32 = 177;
    pub const S_LESS_THAN_EQUAL: u32 = 178;
    pub const F_ORD_EQUAL: u32 = 180;
    pub const F_ORD_NOT_EQUAL: u32 = 182;
    pub const F_ORD_LESS_THAN: u32 = 184;
    pub const F_ORD_GREATER_THAN: u32 = 186;
    pub const F_ORD_LESS_THAN_EQUAL: u32 = 188;
    pub const F_ORD_GREATER_THAN_EQUAL: u32 = 190;
    pub const SHIFT_RIGHT_LOGICAL: u32 = 194;
    pub const SHIFT_LEFT_LOGICAL: u32 = 196;
    pub const BITWISE_OR: u32 = 197;
    pub const BITWISE_XOR: u32 = 198;
    pub const BITWISE_AND: u32 = 199;
    pub const NOT: u32 = 200;
    pub const BIT_FIELD_S_EXTRACT: u32 = 202;
    pub const U_GREATER_THAN_EQUAL: u32 = 175;
    pub const U_LESS_THAN: u32 = 176;
    pub const LOOP_MERGE: u32 = 246;
    pub const SELECTION_MERGE: u32 = 247;
    pub const LABEL: u32 = 248;
    pub const BRANCH: u32 = 249;
    pub const BRANCH_CONDITIONAL: u32 = 250;
    pub const SWITCH: u32 = 251;
    pub const KILL: u32 = 252;
    pub const RETURN: u32 = 253;
    pub const UNREACHABLE: u32 = 255;
    pub const GROUP_NON_UNIFORM_BROADCAST_FIRST: u32 = 338;
}

// GLSL.std.450's instruction numbers.
pub mod glsl {
    pub const TRUNC: u32 = 3;
    pub const F_ABS: u32 = 4;
    pub const FLOOR: u32 = 8;
    pub const FRACT: u32 = 10;
    pub const SIN: u32 = 13;
    pub const COS: u32 = 14;
    pub const POW: u32 = 26;
    pub const EXP2: u32 = 29;
    pub const LOG2: u32 = 30;
    pub const SQRT: u32 = 31;
    pub const INVERSE_SQRT: u32 = 32;
    pub const S_MIN: u32 = 39;
    pub const F_MIN: u32 = 37;
    pub const S_MAX: u32 = 42;
    pub const F_MAX: u32 = 40;
    pub const F_CLAMP: u32 = 43;
    pub const S_CLAMP: u32 = 45;
    pub const F_MIX: u32 = 46;
    pub const STEP: u32 = 48;
    pub const SMOOTH_STEP: u32 = 49;
    pub const FMA: u32 = 50;
    pub const LENGTH: u32 = 66;
    pub const NORMALIZE: u32 = 69;
    pub const UNPACK_HALF_2X16: u32 = 62;
    pub const N_CLAMP: u32 = 81;
}

pub mod storage {
    pub const UNIFORM_CONSTANT: u32 = 0;
    pub const INPUT: u32 = 1;
    pub const UNIFORM: u32 = 2;
    pub const OUTPUT: u32 = 3;
    pub const FUNCTION: u32 = 7;
    pub const PUSH_CONSTANT: u32 = 9;
    pub const STORAGE_BUFFER: u32 = 12;
}

pub mod decoration {
    pub const BLOCK: u32 = 2;
    pub const BUFFER_BLOCK: u32 = 3;
    pub const ARRAY_STRIDE: u32 = 6;
    pub const BUILT_IN: u32 = 11;
    pub const FLAT: u32 = 14;
    pub const NON_WRITABLE: u32 = 24;
    pub const LOCATION: u32 = 30;
    pub const BINDING: u32 = 33;
    pub const DESCRIPTOR_SET: u32 = 34;
    pub const OFFSET: u32 = 35;
}

pub mod built_in {
    pub const POSITION: u32 = 0;
    pub const POINT_SIZE: u32 = 1;
    pub const FRAG_COORD: u32 = 15;
    pub const FRAG_DEPTH: u32 = 22;
    pub const GLOBAL_INVOCATION_ID: u32 = 28;
    pub const VERTEX_INDEX: u32 = 42;
    pub const INSTANCE_INDEX: u32 = 43;
    pub const BASE_VERTEX: u32 = 4424;
}

// 1.3 is Vulkan 1.1's, the first with BaseVertex and subgroup operations
// in the core, which the translated shaders read a draw's record with
// (bindings.rs). Still before 1.4, so an entry point lists only its Input
// and Output variables.
const MAGIC: u32 = 0x0723_0203;
const VERSION: u32 = 0x0001_0300;
// Registered generator ids are handed out by Khronos; 0 is "unknown".
const GENERATOR: u32 = 0;

const CAPABILITY_SHADER: u32 = 1;
const ADDRESSING_LOGICAL: u32 = 0;
const MEMORY_GLSL450: u32 = 1;

#[inline]
pub fn head(op: u32, words: u32) -> u32 {
    (words << 16) | op
}

// Tagged with the opcode, so different kinds never collide.
type Key = (u32, u32, u32);

pub struct Module {
    next_id: Id,
    glsl_set: Id,
    pub capabilities: Vec<u32>,
    pub preamble: Vec<u32>,
    pub entry_points: Vec<u32>,
    pub execution_modes: Vec<u32>,
    pub debug: Vec<u32>,
    pub decorations: Vec<u32>,
    pub declarations: Vec<u32>,
    pub code: Vec<u32>,
    cache: BTreeMap<Key, Id>,
}

impl Default for Module {
    fn default() -> Self {
        Module::new()
    }
}

impl Module {
    pub fn new() -> Module {
        let mut module = Module {
            next_id: 1,
            glsl_set: 0,
            capabilities: Vec::new(),
            preamble: Vec::new(),
            entry_points: Vec::new(),
            execution_modes: Vec::new(),
            debug: Vec::new(),
            decorations: Vec::new(),
            declarations: Vec::new(),
            code: Vec::new(),
            cache: BTreeMap::new(),
        };
        emit(&mut module.capabilities, op::CAPABILITY, &[CAPABILITY_SHADER]);
        // The specification fixes the order: the extended instruction import and the
        // memory model come before the entry point, and before any type or constant.
        module.glsl_set = module.allocate();
        let set = module.glsl_set;
        emit_string(&mut module.preamble, op::EXT_INST_IMPORT, &[set], "GLSL.std.450");
        emit(&mut module.preamble, op::MEMORY_MODEL, &[ADDRESSING_LOGICAL, MEMORY_GLSL450]);
        module
    }

    #[inline]
    pub fn allocate(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn bound(&self) -> Id {
        self.next_id
    }

    pub fn glsl_set(&self) -> Id {
        self.glsl_set
    }

    pub fn require_capability(&mut self, capability: u32) {
        let mut i = 0;
        while i + 1 < self.capabilities.len() {
            if self.capabilities[i + 1] == capability {
                return;
            }
            i += 2;
        }
        emit(&mut self.capabilities, op::CAPABILITY, &[capability]);
    }

    // OpEntryPoint: the execution model, the function, its name, and the
    // Input and Output variables it uses (all that SPIR-V before 1.4 lists).
    pub fn entry_point(&mut self, model: u32, function: Id, name: &str, interface: &[Id]) {
        let start = self.entry_points.len();
        emit_string(&mut self.entry_points, op::ENTRY_POINT, &[model, function], name);
        self.entry_points.extend_from_slice(interface);
        let words = (self.entry_points.len() - start) as u32;
        self.entry_points[start] = head(op::ENTRY_POINT, words);
    }

    // A type declaration puts its result id first (`result_type` 0); anything
    // with a result *type* -- the constants -- puts the type first and the id
    // second. Getting that backwards defines an id twice.
    fn cached(&mut self, key: Key, op: u32, result_type: Id, tail: &[u32]) -> Id {
        if let Some(&id) = self.cache.get(&key) {
            return id;
        }
        let id = self.allocate();
        let words = tail.len() as u32 + if result_type != 0 { 3 } else { 2 };
        self.declarations.push(head(op, words));
        if result_type != 0 {
            self.declarations.push(result_type);
        }
        self.declarations.push(id);
        self.declarations.extend_from_slice(tail);
        self.cache.insert(key, id);
        id
    }

    // Image types are non-aggregate, so the specification forbids declaring the
    // same one twice. Every image here shares everything but its dimensionality,
    // so that is the whole key.
    pub fn image(&mut self, sampled_type: Id, dim: u32, arrayed: bool) -> Id {
        // result, sampled type, Dim, Depth, Arrayed, MS, Sampled, Format
        let arrayed_bit = if arrayed { 0x100 } else { 0 };
        self.cached(
            (op::TYPE_IMAGE, sampled_type, dim | arrayed_bit),
            op::TYPE_IMAGE,
            0,
            &[sampled_type, dim, 0, arrayed as u32, 0, 1, 0],
        )
    }

    pub fn sampler(&mut self) -> Id {
        self.cached((op::TYPE_SAMPLER, 0, 0), op::TYPE_SAMPLER, 0, &[])
    }

    pub fn sampled_image(&mut self, image: Id) -> Id {
        self.cached((op::TYPE_SAMPLED_IMAGE, image, 0), op::TYPE_SAMPLED_IMAGE, 0, &[image])
    }

    pub fn void(&mut self) -> Id {
        self.cached((op::TYPE_VOID, 0, 0), op::TYPE_VOID, 0, &[])
    }

    pub fn bool(&mut self) -> Id {
        self.cached((op::TYPE_BOOL, 0, 0), op::TYPE_BOOL, 0, &[])
    }

    pub fn float(&mut self) -> Id {
        self.cached((op::TYPE_FLOAT, 32, 0), op::TYPE_FLOAT, 0, &[32])
    }

    pub fn int(&mut self, sign: bool) -> Id {
        self.cached((op::TYPE_INT, 32, sign as u32), op::TYPE_INT, 0, &[32, sign as u32])
    }

    pub fn vector(&mut self, component: Id, count: u32) -> Id {
        self.cached((op::TYPE_VECTOR, component, count), op::TYPE_VECTOR, 0, &[component, count])
    }

    pub fn pointer(&mut self, storage: u32, pointee: Id) -> Id {
        self.cached((op::TYPE_POINTER, storage, pointee), op::TYPE_POINTER, 0, &[storage, pointee])
    }

    pub fn array_of(&mut self, element: Id, count: u32) -> Id {
        let length = self.constant_u(count);
        self.cached((op::TYPE_ARRAY, element, length), op::TYPE_ARRAY, 0, &[element, length])
    }

    pub fn runtime_array_of(&mut self, element: Id) -> Id {
        self.cached((op::TYPE_RUNTIME_ARRAY, element, 0), op::TYPE_RUNTIME_ARRAY, 0, &[element])
    }

    pub fn constant_false_value(&mut self) -> Id {
        let ty = self.bool();
        self.cached((op::CONSTANT_FALSE, 0, 0), op::CONSTANT_FALSE, ty, &[])
    }

    pub fn constant_f(&mut self, value: f32) -> Id {
        // -0.0 and 0.0 have different bit patterns and must stay distinct.
        let bits = value.to_bits();
        let ty = self.float();
        self.cached((op::CONSTANT, ty, bits), op::CONSTANT, ty, &[bits])
    }

    pub fn constant_u(&mut self, value: u32) -> Id {
        let ty = self.int(false);
        self.cached((op::CONSTANT, ty, value), op::CONSTANT, ty, &[value])
    }

    pub fn constant_s(&mut self, value: i32) -> Id {
        let ty = self.int(true);
        self.cached((op::CONSTANT, ty, value as u32), op::CONSTANT, ty, &[value as u32])
    }

    pub fn constant_splat_f(&mut self, value: f32) -> Id {
        let bits = value.to_bits();
        let component = self.constant_f(value);
        let ty = self.float4();
        self.cached(
            (op::CONSTANT_COMPOSITE, ty, bits),
            op::CONSTANT_COMPOSITE,
            ty,
            &[component, component, component, component],
        )
    }

    pub fn int_vector(&mut self, count: u32) -> Id {
        let int = self.int(true);
        self.vector(int, count)
    }
    pub fn float2(&mut self) -> Id {
        let float = self.float();
        self.vector(float, 2)
    }
    pub fn float3(&mut self) -> Id {
        let float = self.float();
        self.vector(float, 3)
    }
    pub fn float4(&mut self) -> Id {
        let float = self.float();
        self.vector(float, 4)
    }

    pub fn finish(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(
            5 + self.capabilities.len()
                + self.preamble.len()
                + self.entry_points.len()
                + self.execution_modes.len()
                + self.debug.len()
                + self.decorations.len()
                + self.declarations.len()
                + self.code.len(),
        );
        out.push(MAGIC);
        out.push(VERSION);
        out.push(GENERATOR);
        out.push(self.next_id); // id bound: ids are 1..bound-1
        out.push(0); // reserved
        for section in [
            &self.capabilities,
            &self.preamble,
            &self.entry_points,
            &self.execution_modes,
            &self.debug,
            &self.decorations,
            &self.declarations,
            &self.code,
        ] {
            out.extend_from_slice(section);
        }
        out
    }
}

#[inline]
pub fn emit(into: &mut Vec<u32>, op: u32, operands: &[u32]) {
    into.push(head(op, operands.len() as u32 + 1));
    into.extend_from_slice(operands);
}

// Literal strings are NUL-terminated and padded to a whole number of words.
pub fn emit_string(into: &mut Vec<u32>, op: u32, before: &[u32], text: &str) {
    let bytes = text.as_bytes();
    let words = bytes.len() / 4 + 1;
    into.push(head(op, (1 + before.len() + words) as u32));
    into.extend_from_slice(before);
    for chunk in 0..words {
        let mut word = [0u8; 4];
        for (i, byte) in word.iter_mut().enumerate() {
            if let Some(&b) = bytes.get(chunk * 4 + i) {
                *byte = b;
            }
        }
        into.push(u32::from_le_bytes(word));
    }
}
