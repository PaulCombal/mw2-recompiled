// Xenos shader microcode, decoded.
//
// A program is a run of 32-bit words, big-endian in guest memory, holding a
// control flow section followed by the instructions it names. Control flow
// instructions are 48 bits, packed two into every three words; everything else
// is exactly three words, and an exec's address is an *instruction* index.
//
// Nothing in an instruction says whether it is an ALU pair or a fetch: that
// comes from the sequence word of the exec that runs it, so a program cannot be
// decoded without walking its control flow first.
//
// Field layouts follow Xenia's src/xenia/gpu/ucode.h (BSD-3), written as
// explicit shifts so the encoding is legible.

#[inline]
pub fn bits(value: u32, lo: u32, count: u32) -> u32 {
    (value >> lo) & ((1u32 << count) - 1)
}

#[inline]
pub fn sign_extend(value: u32, count: u32) -> i32 {
    let sign = 1u32 << (count - 1);
    (value ^ sign).wrapping_sub(sign) as i32
}

// The control flow opcodes.
pub mod cf_op {
    pub const NOP: u32 = 0;
    pub const EXEC: u32 = 1;
    pub const EXEC_END: u32 = 2;
    pub const COND_EXEC: u32 = 3;
    pub const COND_EXEC_END: u32 = 4;
    pub const COND_EXEC_PRED: u32 = 5;
    pub const COND_EXEC_PRED_END: u32 = 6;
    pub const LOOP_START: u32 = 7;
    pub const LOOP_END: u32 = 8;
    pub const COND_CALL: u32 = 9;
    pub const RETURN: u32 = 10;
    pub const COND_JMP: u32 = 11;
    pub const ALLOC: u32 = 12;
    pub const COND_EXEC_PRED_CLEAN: u32 = 13;
    pub const COND_EXEC_PRED_CLEAN_END: u32 = 14;
    pub const MARK_VS_FETCH_DONE: u32 = 15;
}

// A vertex shader exports its position here, interpolators at 0..15.
pub const EXPORT_POSITION: u32 = 62;
pub const EXPORT_POINT_SIZE: u32 = 63;
// A pixel shader exports colour at 0..3 and depth in the x of 61.
pub const EXPORT_DEPTH: u32 = 61;
// Memory export: 32 carries the address, 33..37 are eM0..eM4.
pub const EXPORT_MEMORY_ADDRESS: u32 = 32;
pub const EXPORT_MEMORY_FIRST: u32 = 33;
pub const EXPORT_MEMORY_LAST: u32 = 37;

pub const TEMP_REGISTER_COUNT: u32 = 64;
pub const FLOAT_CONSTANT_COUNT: u32 = 256;
pub const INTERPOLATOR_COUNT: u32 = 16;

#[derive(Clone, Copy, Default)]
pub struct ControlFlow {
    pub word0: u32,
    pub word1: u32, // only the low 16 bits are the instruction
}

impl ControlFlow {
    #[inline]
    pub fn opcode(&self) -> u32 {
        bits(self.word1, 12, 4)
    }

    pub fn is_exec(&self) -> bool {
        matches!(
            self.opcode(),
            cf_op::EXEC
                | cf_op::EXEC_END
                | cf_op::COND_EXEC
                | cf_op::COND_EXEC_END
                | cf_op::COND_EXEC_PRED
                | cf_op::COND_EXEC_PRED_END
                | cf_op::COND_EXEC_PRED_CLEAN
                | cf_op::COND_EXEC_PRED_CLEAN_END
        )
    }

    pub fn is_end(&self) -> bool {
        matches!(
            self.opcode(),
            cf_op::EXEC_END | cf_op::COND_EXEC_END | cf_op::COND_EXEC_PRED_END | cf_op::COND_EXEC_PRED_CLEAN_END
        )
    }

    pub fn address(&self) -> u32 {
        bits(self.word0, 0, 12)
    }
    pub fn count(&self) -> u32 {
        bits(self.word0, 12, 3)
    }
    pub fn sequence(&self) -> u32 {
        bits(self.word0, 16, 12)
    }
    pub fn is_fetch(&self, n: u32) -> bool {
        (self.sequence() >> (2 * n)) & 1 != 0
    }

    // Bit 9 is the "predicate clean" hint; the condition is the bit above it.
    pub fn predicate_condition(&self) -> bool {
        bits(self.word1, 10, 1) != 0
    }

    // cond_exec tests one of the 256 boolean constants instead of the predicate
    // register. The two lowest bits of word1 are a vertex cache field, so the
    // constant index starts above them.
    pub fn bool_address(&self) -> u32 {
        bits(self.word1, 2, 8)
    }

    // All of them carry a 13-bit target in the low bits of word0; the rest of
    // the layout differs per opcode.
    pub fn target(&self) -> u32 {
        bits(self.word0, 0, 13)
    }

    // loop_start: `address` is where to go when the loop is skipped entirely,
    // and `is_repeat` keeps the current aL rather than resetting.
    pub fn is_repeat(&self) -> bool {
        bits(self.word0, 13, 1) != 0
    }
    pub fn loop_id(&self) -> u32 {
        bits(self.word0, 16, 5)
    }

    // loop_end: the same loop id, plus an optional predicated break.
    pub fn loop_end_id(&self) -> u32 {
        bits(self.word0, 16, 5)
    }
    pub fn is_predicated_break(&self) -> bool {
        bits(self.word0, 21, 1) != 0
    }
    pub fn loop_condition(&self) -> bool {
        bits(self.word1, 10, 1) != 0
    }

    // Unconditional, predicated, or on a boolean constant, in that order of
    // precedence.
    pub fn is_unconditional(&self) -> bool {
        bits(self.word0, 13, 1) != 0
    }
    pub fn is_predicated_jump(&self) -> bool {
        bits(self.word0, 14, 1) != 0
    }
    pub fn jump_bool_address(&self) -> u32 {
        bits(self.word1, 2, 8)
    }
    pub fn jump_condition(&self) -> bool {
        bits(self.word1, 10, 1) != 0
    }

    // AllocType: 0 none, 1 position, 2 interpolators, 3 memory.
    pub fn allocation(&self) -> u32 {
        bits(self.word1, 9, 2)
    }
    pub fn alloc_size(&self) -> u32 {
        bits(self.word0, 0, 3)
    }
}

// Two control flow instructions live in three words.
#[inline]
pub fn unpack_control_flow(w0: u32, w1: u32, w2: u32) -> (ControlFlow, ControlFlow) {
    let first = ControlFlow { word0: w0, word1: w1 & 0xFFFF };
    let second = ControlFlow { word0: (w1 >> 16) | (w2 << 16), word1: w2 >> 16 };
    (first, second)
}

pub mod vector_op {
    pub const ADD: u32 = 0;
    pub const MUL: u32 = 1;
    pub const MAX: u32 = 2;
    pub const MIN: u32 = 3;
    pub const SEQ: u32 = 4;
    pub const SGT: u32 = 5;
    pub const SGE: u32 = 6;
    pub const SNE: u32 = 7;
    pub const FRC: u32 = 8;
    pub const TRUNC: u32 = 9;
    pub const FLOOR: u32 = 10;
    pub const MAD: u32 = 11;
    pub const CND_EQ: u32 = 12;
    pub const CND_GE: u32 = 13;
    pub const CND_GT: u32 = 14;
    pub const DP4: u32 = 15;
    pub const DP3: u32 = 16;
    pub const DP2_ADD: u32 = 17;
    pub const CUBE: u32 = 18;
    pub const MAX4: u32 = 19;
    pub const SETP_EQ_PUSH: u32 = 20;
    pub const SETP_NE_PUSH: u32 = 21;
    pub const SETP_GT_PUSH: u32 = 22;
    pub const SETP_GE_PUSH: u32 = 23;
    pub const KILL_EQ: u32 = 24;
    pub const KILL_GT: u32 = 25;
    pub const KILL_GE: u32 = 26;
    pub const KILL_NE: u32 = 27;
    pub const DST: u32 = 28;
    pub const MAX_A: u32 = 29;
}

pub mod scalar_op {
    pub const ADDS: u32 = 0;
    pub const ADDS_PREV: u32 = 1;
    pub const MULS: u32 = 2;
    pub const MULS_PREV: u32 = 3;
    pub const MULS_PREV2: u32 = 4;
    pub const MAXS: u32 = 5;
    pub const MINS: u32 = 6;
    pub const SEQS: u32 = 7;
    pub const SGTS: u32 = 8;
    pub const SGES: u32 = 9;
    pub const SNES: u32 = 10;
    pub const FRCS: u32 = 11;
    pub const TRUNCS: u32 = 12;
    pub const FLOORS: u32 = 13;
    pub const EXP: u32 = 14;
    pub const LOGC: u32 = 15;
    pub const LOG: u32 = 16;
    pub const RCPC: u32 = 17;
    pub const RCPF: u32 = 18;
    pub const RCP: u32 = 19;
    pub const RSQC: u32 = 20;
    pub const RSQF: u32 = 21;
    pub const RSQ: u32 = 22;
    pub const MAX_AS: u32 = 23;
    pub const MAX_ASF: u32 = 24;
    pub const SUBS: u32 = 25;
    pub const SUBS_PREV: u32 = 26;
    pub const SETP_EQ: u32 = 27;
    pub const SETP_NE: u32 = 28;
    pub const SETP_GT: u32 = 29;
    pub const SETP_GE: u32 = 30;
    pub const SETP_INV: u32 = 31;
    pub const SETP_POP: u32 = 32;
    pub const SETP_CLR: u32 = 33;
    pub const SETP_RSTR: u32 = 34;
    pub const KILLS_EQ: u32 = 35;
    pub const KILLS_GT: u32 = 36;
    pub const KILLS_GE: u32 = 37;
    pub const KILLS_NE: u32 = 38;
    pub const KILLS_ONE: u32 = 39;
    pub const SQRT: u32 = 40;
    pub const MULSC0: u32 = 42;
    pub const MULSC1: u32 = 43;
    pub const ADDSC0: u32 = 44;
    pub const ADDSC1: u32 = 45;
    pub const SUBSC0: u32 = 46;
    pub const SUBSC1: u32 = 47;
    pub const SIN: u32 = 48;
    pub const COS: u32 = 49;
    pub const RETAIN_PREV: u32 = 50;
}

// How many register operands each vector operation reads, so a translator
// never evaluates a source the hardware ignores.
pub fn vector_operand_count(op: u32) -> u32 {
    match op {
        vector_op::FRC | vector_op::TRUNC | vector_op::FLOOR | vector_op::MAX4 => 1,
        vector_op::MAD | vector_op::CND_EQ | vector_op::CND_GE | vector_op::CND_GT | vector_op::DP2_ADD => 3,
        _ => 2,
    }
}

#[derive(Clone, Copy, Default)]
pub struct Alu {
    pub w0: u32,
    pub w1: u32,
    pub w2: u32,
}

impl Alu {
    pub fn vector_dest(&self) -> u32 {
        bits(self.w0, 0, 6)
    }
    pub fn vector_dest_relative(&self) -> bool {
        bits(self.w0, 6, 1) != 0
    }
    pub fn absolute_constants(&self) -> bool {
        bits(self.w0, 7, 1) != 0
    }
    pub fn scalar_dest(&self) -> u32 {
        bits(self.w0, 8, 6)
    }
    pub fn scalar_dest_relative(&self) -> bool {
        bits(self.w0, 14, 1) != 0
    }
    pub fn is_export(&self) -> bool {
        bits(self.w0, 15, 1) != 0
    }
    pub fn vector_write_mask(&self) -> u32 {
        bits(self.w0, 16, 4)
    }
    pub fn scalar_write_mask(&self) -> u32 {
        bits(self.w0, 20, 4)
    }
    pub fn vector_clamp(&self) -> bool {
        bits(self.w0, 24, 1) != 0
    }
    pub fn scalar_clamp(&self) -> bool {
        bits(self.w0, 25, 1) != 0
    }
    pub fn scalar(&self) -> u32 {
        bits(self.w0, 26, 6)
    }

    // Operand 1 is the most significant field of each word, which is why the
    // shifts run backwards.
    pub fn source_swizzle(&self, i: u32) -> u32 {
        bits(
            self.w1,
            if i == 1 {
                16
            } else if i == 2 {
                8
            } else {
                0
            },
            8,
        )
    }
    pub fn source_negate(&self, i: u32) -> bool {
        bits(
            self.w1,
            if i == 1 {
                26
            } else if i == 2 {
                25
            } else {
                24
            },
            1,
        ) != 0
    }
    pub fn source_register(&self, i: u32) -> u32 {
        bits(
            self.w2,
            if i == 1 {
                16
            } else if i == 2 {
                8
            } else {
                0
            },
            8,
        )
    }
    pub fn source_is_temp(&self, i: u32) -> bool {
        bits(
            self.w2,
            if i == 1 {
                31
            } else if i == 2 {
                30
            } else {
                29
            },
            1,
        ) != 0
    }

    pub fn predicate_condition(&self) -> bool {
        bits(self.w1, 27, 1) != 0
    }
    pub fn is_predicated(&self) -> bool {
        bits(self.w1, 28, 1) != 0
    }
    // Relative constant addressing has two independent parts. Bit 29 picks which
    // register does the addressing -- a0 rather than aL -- and says nothing about
    // whether addressing happens at all. Whether a given source is addressed
    // comes from const_0_rel_abs / const_1_rel_abs, chosen by how many earlier
    // operands were constants: the hardware carries two such bits for three
    // possible constant operands.
    pub fn constant_address_uses_a0(&self) -> bool {
        bits(self.w1, 29, 1) != 0
    }
    pub fn const0_rel_abs(&self) -> bool {
        bits(self.w1, 31, 1) != 0
    }
    pub fn const1_rel_abs(&self) -> bool {
        bits(self.w1, 30, 1) != 0
    }
    pub fn source_constant_is_addressed(&self, i: u32) -> bool {
        match i {
            1 => self.const0_rel_abs(),
            2 => {
                if self.source_is_temp(1) {
                    self.const0_rel_abs()
                } else {
                    self.const1_rel_abs()
                }
            }
            3 => {
                if self.source_is_temp(1) && self.source_is_temp(2) {
                    self.const0_rel_abs()
                } else {
                    self.const1_rel_abs()
                }
            }
            _ => false,
        }
    }

    pub fn vector(&self) -> u32 {
        bits(self.w2, 24, 5)
    }

    // The mulsc/addsc/subsc family read a constant alongside a *temporary*, and
    // it is the temporary whose index is assembled from bits scattered across
    // the instruction. The constant is source 3's own register number.
    pub fn scalar_constant_form_temp(&self) -> u32 {
        (self.scalar() & 1) | (bits(self.w2, 29, 1) << 1) | (self.source_swizzle(3) & 0x3C)
    }

    // Exports write one register from both halves under complementary masks, and
    // can also write literal 0 and 1 where neither writes.
    pub fn vector_result_mask(&self) -> u32 {
        let mask = self.vector_write_mask();
        if self.is_export() { mask & !self.scalar_write_mask() } else { mask }
    }
    pub fn scalar_result_mask(&self) -> u32 {
        let mask = self.scalar_write_mask();
        if self.is_export() { mask & !self.vector_write_mask() } else { mask }
    }
    pub fn constant_zero_mask(&self) -> u32 {
        if !self.is_export() || !self.scalar_dest_relative() {
            return 0;
        }
        0xF & !(self.vector_write_mask() | self.scalar_write_mask())
    }
    pub fn constant_one_mask(&self) -> u32 {
        if self.is_export() { self.vector_write_mask() & self.scalar_write_mask() } else { 0 }
    }

    // Swizzles are component-relative: each two-bit field is added to the
    // position it sits in.
    pub fn swizzled_component(&self, swizzle: u32, component: u32) -> u32 {
        ((swizzle >> (2 * component)).wrapping_add(component)) & 3
    }
}

pub mod fetch_op {
    pub const VERTEX_FETCH: u32 = 0;
    pub const TEXTURE_FETCH: u32 = 1;
    pub const GET_BORDER_COLOUR_FRAC: u32 = 16;
    pub const GET_COMPUTED_LOD: u32 = 17;
    pub const GET_GRADIENTS: u32 = 18;
    pub const GET_WEIGHTS: u32 = 19;
    pub const SET_LOD: u32 = 24;
    pub const SET_GRADIENTS_H: u32 = 25;
    pub const SET_GRADIENTS_V: u32 = 26;
}

// FetchDimension.
pub mod dimension {
    pub const D1: u32 = 0;
    pub const D2: u32 = 1;
    pub const D3: u32 = 2;
    pub const CUBE: u32 = 3;
}

#[derive(Clone, Copy, Default)]
pub struct Fetch {
    pub w0: u32,
    pub w1: u32,
    pub w2: u32,
}

impl Fetch {
    pub fn opcode(&self) -> u32 {
        bits(self.w0, 0, 5)
    }
    pub fn source_register(&self) -> u32 {
        bits(self.w0, 5, 6)
    }
    pub fn dest_register(&self) -> u32 {
        bits(self.w0, 12, 6)
    }
    pub fn constant_index(&self) -> u32 {
        bits(self.w0, 20, 5)
    }

    // Four components, three bits each: 0-3 pick x/y/z/w of the fetched value,
    // 4 is literal 0, 5 is literal 1, 7 leaves the component alone.
    pub fn dest_swizzle(&self) -> u32 {
        bits(self.w1, 0, 12)
    }
    pub fn dest_component(&self, i: u32) -> u32 {
        (self.dest_swizzle() >> (3 * i)) & 7
    }
    pub fn is_predicated(&self) -> bool {
        bits(self.w1, 31, 1) != 0
    }
    pub fn predicate_condition(&self) -> bool {
        bits(self.w2, 31, 1) != 0
    }

    // A vertex fetch. Bit 19 is set on every real one (Xenia calls it
    // must_be_one), and bits 25-26 pick which of the group's three vertex
    // constants it reads.
    pub fn must_be_one(&self) -> bool {
        bits(self.w0, 19, 1) != 0
    }
    pub fn constant_select(&self) -> u32 {
        bits(self.w0, 25, 2)
    }
    pub fn vertex_fetch_slot(&self) -> u32 {
        self.constant_index() * 3 + self.constant_select()
    }
    pub fn source_component(&self) -> u32 {
        bits(self.w0, 30, 2)
    }
    // The components are signed, and integers rather than a normalised fraction.
    pub fn signed_components(&self) -> bool {
        bits(self.w1, 12, 1) != 0
    }
    pub fn integer_components(&self) -> bool {
        bits(self.w1, 13, 1) != 0
    }
    pub fn vertex_format(&self) -> u32 {
        bits(self.w1, 16, 6)
    }
    pub fn exponent_adjust(&self) -> i32 {
        sign_extend(bits(self.w1, 24, 6), 6)
    }
    pub fn is_mini_fetch(&self) -> bool {
        bits(self.w1, 30, 1) != 0
    }
    pub fn stride(&self) -> u32 {
        bits(self.w2, 0, 8)
    }
    pub fn offset(&self) -> i32 {
        sign_extend(bits(self.w2, 8, 23), 23)
    }

    // The coordinates are in texels rather than in 0..1. A volume lookup
    // table is addressed that way, and read as if normalised it samples far
    // outside the texture and clamps to a corner.
    pub fn coordinates_unnormalised(&self) -> bool {
        bits(self.w0, 25, 1) != 0
    }
    pub fn texture_source_swizzle(&self) -> u32 {
        bits(self.w0, 26, 6)
    }
    pub fn dimension(&self) -> u32 {
        bits(self.w2, 14, 2)
    }
    pub fn use_computed_lod(&self) -> bool {
        bits(self.w1, 28, 1) != 0
    }
    pub fn use_register_lod(&self) -> bool {
        bits(self.w1, 29, 1) != 0
    }
    // Per-instruction sampler state. 3 (7 for anisotropy) is "as the fetch
    // constant says"; anything else overrides it for this fetch alone.
    // Field layout follows Xenia's `TextureFetchInstruction` (BSD-3).
    pub fn mag_filter(&self) -> u32 {
        bits(self.w1, 12, 2)
    }
    pub fn min_filter(&self) -> u32 {
        bits(self.w1, 14, 2)
    }
    pub fn mip_filter(&self) -> u32 {
        bits(self.w1, 16, 2)
    }
    pub fn aniso_filter(&self) -> u32 {
        bits(self.w1, 18, 3)
    }
    pub fn volume_mag_filter(&self) -> u32 {
        bits(self.w1, 24, 2)
    }
    pub fn volume_min_filter(&self) -> u32 {
        bits(self.w1, 26, 2)
    }
    pub fn use_register_gradients(&self) -> bool {
        bits(self.w2, 0, 1) != 0
    }
    // In sixteenths of a level.
    pub fn lod_bias(&self) -> i32 {
        sign_extend(bits(self.w2, 2, 7), 7)
    }
    // In half texels.
    pub fn offset_x(&self) -> i32 {
        sign_extend(bits(self.w2, 16, 5), 5)
    }
    pub fn offset_y(&self) -> i32 {
        sign_extend(bits(self.w2, 21, 5), 5)
    }
    pub fn offset_z(&self) -> i32 {
        sign_extend(bits(self.w2, 26, 5), 5)
    }
}
