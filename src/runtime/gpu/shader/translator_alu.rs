// The ALU half of the translator: sources, the vector and scalar units,
// constants, predication and kills.

use super::bindings;
use super::spirv::{Id, glsl, op, storage};
use super::translator::Translator;
use super::ucode::{Alu, FLOAT_CONSTANT_COUNT, TEMP_REGISTER_COUNT, scalar_op, vector_op, vector_operand_count};

// FLT_MAX, as the C++ spells it: 3.402823466e+38f.
const FLT_MAX: f32 = f32::MAX;

impl Translator<'_> {
    pub(super) fn load_temp(&mut self, reg: u32) -> Id {
        let index = self.m.constant_s((reg & (TEMP_REGISTER_COUNT - 1)) as i32);
        let pointer = self.temp_pointer(index);
        let float4 = self.float4;
        self.emit(op::LOAD, float4, &[pointer])
    }

    // The index is a value rather than a constant so that a destination
    // addressed relative to aL can use the same path.
    pub(super) fn temp_pointer(&mut self, index: Id) -> Id {
        let pointer = self.m.pointer(storage::FUNCTION, self.float4);
        let temps = self.temps;
        self.emit(op::ACCESS_CHAIN, pointer, &[temps, index])
    }

    pub(super) fn temp_index(&mut self, reg: u32, relative: bool) -> Id {
        if !relative {
            return self.m.constant_s((reg & (TEMP_REGISTER_COUNT - 1)) as i32);
        }
        let (int, uint, address_register) = (self.int, self.uint, self.address_register);
        let base = self.emit(op::LOAD, int, &[address_register]);
        let offset = self.m.constant_s(reg as i32);
        let at = self.emit(op::I_ADD, int, &[base, offset]);
        let unsigned = self.emit(op::BITCAST, uint, &[at]);
        let count = self.m.constant_u(TEMP_REGISTER_COUNT);
        let wrapped = self.emit(op::U_MOD, uint, &[unsigned, count]);
        self.emit(op::BITCAST, int, &[wrapped])
    }

    pub(super) fn load_constant(&mut self, index: u32) -> Id {
        let at32 = index % FLOAT_CONSTANT_COUNT;
        // So the renderer can copy the window this program reads rather than
        // all 256 of them.
        self.result.meta.constants_read = self.result.meta.constants_read.max(at32 + 1);
        if !self.pixel {
            let window = self.m.constant_u(at32);
            return self.vertex_constant(window);
        }
        let uint = self.uint;
        let block = self.constant_block(bindings::PIXEL_FLOATS);
        let two = self.m.constant_u(2);
        let vec4 = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[block, two]);
        let offset = self.m.constant_u(at32);
        let at = self.emit(op::I_ADD, uint, &[vec4, offset]);
        self.arena_float4(at)
    }

    // The index is computed at run time, so it is wrapped into range rather
    // than masked at translation time.
    pub(super) fn load_constant_relative(&mut self, index: u32, use_a0: bool) -> Id {
        // The index is only known when the shader runs, so nothing can be left
        // out of the copy.
        self.result.meta.addressed_constants = true;
        let (int, uint) = (self.int, self.uint);
        let register = if use_a0 { self.a0 } else { self.address_register };
        let base = self.emit(op::LOAD, int, &[register]);
        let offset = self.m.constant_s(index as i32);
        let at = self.emit(op::I_ADD, int, &[base, offset]);
        let unsigned = self.emit(op::BITCAST, uint, &[at]);
        let count = self.m.constant_u(FLOAT_CONSTANT_COUNT);
        let wrapped = self.emit(op::U_MOD, uint, &[unsigned, count]);
        if !self.pixel {
            return self.vertex_constant(wrapped);
        }
        let block = self.constant_block(bindings::PIXEL_FLOATS);
        let two = self.m.constant_u(2);
        let vec4 = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[block, two]);
        let at = self.emit(op::I_ADD, uint, &[vec4, wrapped]);
        self.arena_float4(at)
    }

    // Where constant block `block` (bindings.rs) is in the arena, in dwords, as
    // the draw's record says.
    pub(super) fn constant_block(&mut self, block: u32) -> Id {
        self.record_word(bindings::RECORD_OFFSETS + block)
    }

    pub(super) fn arena_word(&mut self, dword: Id) -> Id {
        let uint = self.uint;
        let pointer = self.m.pointer(storage::UNIFORM, uint);
        let zero = self.m.constant_s(0);
        let arena = self.arena;
        let chain = self.emit(op::ACCESS_CHAIN, pointer, &[arena, zero, dword]);
        self.emit(op::LOAD, uint, &[chain])
    }

    pub(super) fn arena_float4(&mut self, vec4: Id) -> Id {
        let float4 = self.float4;
        let pointer = self.m.pointer(storage::UNIFORM, float4);
        let zero = self.m.constant_s(0);
        let arena_floats = self.arena_floats;
        let chain = self.emit(op::ACCESS_CHAIN, pointer, &[arena_floats, zero, vec4]);
        self.emit(op::LOAD, float4, &[chain])
    }

    // Constant `window` (0-255) of a vertex program: the file's entry base +
    // window, found through the table at the block it falls in.
    pub(super) fn vertex_constant(&mut self, window: Id) -> Id {
        let uint = self.uint;
        let table = self.constant_block(bindings::VERTEX_FLOATS);
        let base = self.arena_word(table);
        let entry = self.emit(op::I_ADD, uint, &[base, window]);
        let sixteen = self.m.constant_u(4);
        let block = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[entry, sixteen]);
        let header = self.m.constant_u(4);
        let row = self.emit(op::I_ADD, uint, &[header, block]);
        let at = self.emit(op::I_ADD, uint, &[table, row]);
        let copy = self.arena_word(at);
        let fifteen = self.m.constant_u(15);
        let within = self.emit(op::BITWISE_AND, uint, &[entry, fifteen]);
        let vec4 = self.emit(op::I_ADD, uint, &[copy, within]);
        self.arena_float4(vec4)
    }

    pub(super) fn swizzle(&mut self, value: Id, swizzle: u32, alu: &Alu) -> Id {
        // Component-relative: field n is added to n. An identity swizzle is
        // 0b11100100 read that way, and is common enough to skip.
        let mut components = [0u32; 4];
        let mut identity = true;
        for c in 0..4u32 {
            components[c as usize] = alu.swizzled_component(swizzle, c);
            if components[c as usize] != c {
                identity = false;
            }
        }
        if identity {
            return value;
        }
        let float4 = self.float4;
        self.emit(
            op::VECTOR_SHUFFLE,
            float4,
            &[value, value, components[0], components[1], components[2], components[3]],
        )
    }

    pub(super) fn splat_component(&mut self, value: Id, component: u32) -> Id {
        let float4 = self.float4;
        self.emit(op::VECTOR_SHUFFLE, float4, &[value, value, component, component, component, component])
    }

    pub(super) fn load_source(&mut self, alu: &Alu, index: u32) -> Id {
        let reg = alu.source_register(index);
        let mut value;
        let absolute;
        if alu.source_is_temp(index) {
            // Bit 6 makes the number relative to aL, exactly as a destination
            // can be; the temporaries are an array, so either kind indexes it.
            value = if reg & 0x40 != 0 {
                let at = self.temp_index(reg & 0x3F, true);
                let pointer = self.temp_pointer(at);
                let float4 = self.float4;
                self.emit(op::LOAD, float4, &[pointer])
            } else {
                self.load_temp(reg & 0x3F)
            };
            absolute = reg & 0x80 != 0;
        } else {
            value = if alu.source_constant_is_addressed(index) {
                self.load_constant_relative(reg, alu.constant_address_uses_a0())
            } else {
                self.load_constant(reg)
            };
            absolute = alu.absolute_constants();
        }
        value = self.swizzle(value, alu.source_swizzle(index), alu);
        let float4 = self.float4;
        if absolute {
            value = self.glsl450(glsl::F_ABS, float4, &[value]);
        }
        if alu.source_negate(index) {
            value = self.emit(op::F_NEGATE, float4, &[value]);
        }
        value
    }

    fn vector_compare(&mut self, comparison: u32, a: Id, b: Id) -> Id {
        let (bool4, float4) = (self.bool4, self.float4);
        let mask = self.emit(comparison, bool4, &[a, b]);
        let one = self.splat(1.0);
        let zero = self.splat(0.0);
        self.emit(op::SELECT, float4, &[mask, one, zero])
    }

    // cnd* pick b or c per component according to a's comparison to 0.
    fn vector_select(&mut self, comparison: u32, a: Id, b: Id, c: Id) -> Id {
        let (bool4, float4) = (self.bool4, self.float4);
        let zero = self.splat(0.0);
        let mask = self.emit(comparison, bool4, &[a, zero]);
        self.emit(op::SELECT, float4, &[mask, b, c])
    }

    // The push family write the predicate and produce a counter: p0 = (a.w == 0 &&
    // b.w op 0), and the result steps a.x by one unless the same test on x holds.
    fn vector_push(&mut self, comparison: u32, a: Id, b: Id) -> Id {
        let zero = self.m.constant_f(0.0);
        let both = |t: &mut Self, component: u32| -> Id {
            let x = t.component(a, component);
            let y = t.component(b, component);
            let bool_ = t.bool_;
            let first = t.emit(op::F_ORD_EQUAL, bool_, &[x, zero]);
            let second = t.emit(comparison, bool_, &[y, zero]);
            t.emit(op::LOGICAL_AND, bool_, &[first, second])
        };
        let w = both(self, 3);
        self.store_predicate(w);
        let restart = both(self, 0);
        let minus_one = self.m.constant_f(-1.0);
        let ax = self.component(a, 0);
        let float = self.float;
        let stepped = self.emit(op::SELECT, float, &[restart, minus_one, ax]);
        let one = self.m.constant_f(1.0);
        let result = self.emit(op::F_ADD, float, &[stepped, one]);
        self.build_vector(result, result, result, result)
    }

    // The hardware stops the invocation at a kill; here the discard is deferred to
    // the end, which differs only for a shader exporting to memory after killing.
    fn vector_kill(&mut self, comparison: u32, a: Id, b: Id) -> Id {
        let (bool4, bool_) = (self.bool4, self.bool_);
        let mask = self.emit(comparison, bool4, &[a, b]);
        let any = self.emit(op::ANY, bool_, &[mask]);
        self.request_kill(any);
        self.splat(0.0)
    }

    fn vector_dot(&mut self, count: u32, a: Id, b: Id) -> Id {
        let float = self.float;
        let x = self.component(a, 0);
        let y = self.component(b, 0);
        let mut sum = self.emit(op::F_MUL, float, &[x, y]);
        for i in 1..count {
            let ai = self.component(a, i);
            let bi = self.component(b, i);
            sum = self.glsl450(glsl::FMA, float, &[ai, bi, sum]);
        }
        self.build_vector(sum, sum, sum, sum)
    }

    pub(super) fn vector_operation(&mut self, alu: &Alu) -> Id {
        let operands = vector_operand_count(alu.vector());
        let a = if operands >= 1 { self.load_source(alu, 1) } else { 0 };
        let b = if operands >= 2 { self.load_source(alu, 2) } else { 0 };
        let c = if operands >= 3 { self.load_source(alu, 3) } else { 0 };
        let (float, float4) = (self.float, self.float4);

        match alu.vector() {
            vector_op::ADD => self.emit(op::F_ADD, float4, &[a, b]),
            vector_op::MUL => self.emit(op::F_MUL, float4, &[a, b]),
            vector_op::MAX => self.glsl450(glsl::F_MAX, float4, &[a, b]),
            vector_op::MIN => self.glsl450(glsl::F_MIN, float4, &[a, b]),
            vector_op::SEQ => self.vector_compare(op::F_ORD_EQUAL, a, b),
            vector_op::SGT => self.vector_compare(op::F_ORD_GREATER_THAN, a, b),
            vector_op::SGE => self.vector_compare(op::F_ORD_GREATER_THAN_EQUAL, a, b),
            vector_op::SNE => self.vector_compare(op::F_ORD_NOT_EQUAL, a, b),
            vector_op::FRC => self.glsl450(glsl::FRACT, float4, &[a]),
            vector_op::TRUNC => self.glsl450(glsl::TRUNC, float4, &[a]),
            vector_op::FLOOR => self.glsl450(glsl::FLOOR, float4, &[a]),
            vector_op::MAD => self.glsl450(glsl::FMA, float4, &[a, b, c]),
            vector_op::CND_EQ => self.vector_select(op::F_ORD_EQUAL, a, b, c),
            vector_op::CND_GE => self.vector_select(op::F_ORD_GREATER_THAN_EQUAL, a, b, c),
            vector_op::CND_GT => self.vector_select(op::F_ORD_GREATER_THAN, a, b, c),
            vector_op::SETP_EQ_PUSH => self.vector_push(op::F_ORD_EQUAL, a, b),
            vector_op::SETP_NE_PUSH => self.vector_push(op::F_ORD_NOT_EQUAL, a, b),
            vector_op::SETP_GT_PUSH => self.vector_push(op::F_ORD_GREATER_THAN, a, b),
            vector_op::SETP_GE_PUSH => self.vector_push(op::F_ORD_GREATER_THAN_EQUAL, a, b),
            vector_op::KILL_EQ => self.vector_kill(op::F_ORD_EQUAL, a, b),
            vector_op::KILL_GT => self.vector_kill(op::F_ORD_GREATER_THAN, a, b),
            vector_op::KILL_GE => self.vector_kill(op::F_ORD_GREATER_THAN_EQUAL, a, b),
            vector_op::KILL_NE => self.vector_kill(op::F_ORD_NOT_EQUAL, a, b),
            vector_op::CUBE => self.cube_coordinates(a, b),
            vector_op::DP4 => self.vector_dot(4, a, b),
            vector_op::DP3 => self.vector_dot(3, a, b),
            vector_op::DP2_ADD => {
                let two = self.vector_dot(2, a, b);
                let cx = self.splat_component(c, 0);
                self.emit(op::F_ADD, float4, &[two, cx])
            }
            vector_op::MAX4 => {
                let mut best = self.component(a, 0);
                for i in 1..4 {
                    let ai = self.component(a, i);
                    best = self.glsl450(glsl::F_MAX, float, &[best, ai]);
                }
                self.build_vector(best, best, best, best)
            }
            other => {
                self.fail("vector opcode", other);
                self.splat(0.0)
            }
        }
    }

    // The scalar comparisons test one component against zero -- they are not
    // the two-operand form the vector half uses. Comparing against a second
    // component instead gives a per-pixel 0 or 1, which is a speckle.
    fn scalar_compare(&mut self, comparison: u32, x: Id) -> Id {
        let bool_ = self.m.bool();
        let zero = self.m.constant_f(0.0);
        let b = self.emit(comparison, bool_, &[x, zero]);
        let one = self.m.constant_f(1.0);
        let zero = self.m.constant_f(0.0);
        let float = self.float;
        self.emit(op::SELECT, float, &[b, one, zero])
    }

    pub(super) fn scalar_operation(&mut self, alu: &Alu) -> Id {
        // Scalar operations read source 3: one operand takes its x and y, the
        // mulsc/addsc/subsc family take its x and a constant's x.
        let opcode = alu.scalar();
        let (float, bool_, int) = (self.float, self.bool_, self.int);
        if opcode == scalar_op::RETAIN_PREV {
            if !self.have_previous_scalar {
                return 0;
            }
            let previous_scalar = self.previous_scalar;
            let previous = self.emit(op::LOAD, float, &[previous_scalar]);
            return self.build_vector(previous, previous, previous, previous);
        }
        if opcode == scalar_op::SETP_CLR {
            // Reads nothing. Evaluating source 3 anyway would report an addressing
            // mode this instruction does not use.
            let no = self.m.constant_false_value();
            let predicate = self.predicate;
            self.code(op::STORE, &[predicate, no]);
            let maximum = self.m.constant_f(FLT_MAX);
            let previous_scalar = self.previous_scalar;
            self.code(op::STORE, &[previous_scalar, maximum]);
            self.have_previous_scalar = true;
            return self.build_vector(maximum, maximum, maximum, maximum);
        }

        let x;
        let y;
        if (scalar_op::MULSC0..=scalar_op::SUBSC1).contains(&opcode) {
            // These read a constant and a temporary, and neither is where source 3
            // says: the constant is c[source 3's register number] whatever the
            // source-is-temporary bit claims, and the temporary's index is
            // assembled from bits scattered across the instruction. Both take
            // their absolute value from the constant flag, not the register one.
            let swizzle = alu.source_swizzle(3);
            let reg = alu.source_register(3);
            let constant = if alu.source_constant_is_addressed(3) {
                self.load_constant_relative(reg, alu.constant_address_uses_a0())
            } else {
                self.load_constant(reg)
            };
            let temporary = self.load_temp(alu.scalar_constant_form_temp());
            let sign = |t: &mut Self, mut v: Id| -> Id {
                if alu.absolute_constants() {
                    v = t.glsl450(glsl::F_ABS, float, &[v]);
                }
                if alu.source_negate(3) {
                    v = t.emit(op::F_NEGATE, float, &[v]);
                }
                v
            };
            let cx = self.component(constant, alu.swizzled_component(swizzle, 3));
            x = sign(self, cx);
            let tx = self.component(temporary, alu.swizzled_component(swizzle, 0));
            y = sign(self, tx);
        } else {
            // The left operand is the swizzle's fourth slot, not its first, and the
            // right one is the first -- or the third, when the vector half has
            // already claimed source 3 for a third operand of its own.
            let source = self.load_source(alu, 3);
            x = self.component(source, 3);
            y = self.component(source, if vector_operand_count(alu.vector()) == 3 { 2 } else { 0 });
        }

        let r = match opcode {
            scalar_op::ADDS => self.emit(op::F_ADD, float, &[x, y]),
            scalar_op::MULS => self.emit(op::F_MUL, float, &[x, y]),
            scalar_op::SUBS => self.emit(op::F_SUB, float, &[x, y]),
            scalar_op::MULS_PREV2 => {
                // src.x * ps, except that a previous scalar of -FLT_MAX, or a src.y that
                // is negative or not a number, forces -FLT_MAX through.
                let previous_scalar = self.previous_scalar;
                let previous = self.emit(op::LOAD, float, &[previous_scalar]);
                let negative_max = self.m.constant_f(-FLT_MAX);
                let is_max = self.emit(op::F_ORD_EQUAL, bool_, &[previous, negative_max]);
                let zero = self.m.constant_f(0.0);
                let positive = self.emit(op::F_ORD_GREATER_THAN_EQUAL, bool_, &[y, zero]);
                let not_positive = self.emit(op::LOGICAL_NOT, bool_, &[positive]);
                let bad = self.emit(op::LOGICAL_OR, bool_, &[is_max, not_positive]);
                let product = self.emit(op::F_MUL, float, &[x, previous]);
                self.emit(op::SELECT, float, &[bad, negative_max, product])
            }
            scalar_op::MAXS => self.glsl450(glsl::F_MAX, float, &[x, y]),
            // The maxa family is a max that also loads a0 from the first source.
            // maxas rounds to nearest, maxasf truncates towards minus infinity.
            scalar_op::MAX_AS | scalar_op::MAX_ASF => {
                let rounded = if opcode == scalar_op::MAX_AS {
                    let half = self.m.constant_f(0.5);
                    let sum = self.emit(op::F_ADD, float, &[x, half]);
                    self.glsl450(glsl::FLOOR, float, &[sum])
                } else {
                    self.glsl450(glsl::FLOOR, float, &[x])
                };
                let low = self.m.constant_f(-256.0);
                let high = self.m.constant_f(255.0);
                let clamped = self.glsl450(glsl::F_CLAMP, float, &[rounded, low, high]);
                let a0 = self.a0;
                let value = self.emit(op::CONVERT_F_TO_S, int, &[clamped]);
                self.code(op::STORE, &[a0, value]);
                self.glsl450(glsl::F_MAX, float, &[x, y])
            }
            scalar_op::MINS => self.glsl450(glsl::F_MIN, float, &[x, y]),
            scalar_op::SEQS => self.scalar_compare(op::F_ORD_EQUAL, x),
            scalar_op::SGTS => self.scalar_compare(op::F_ORD_GREATER_THAN, x),
            scalar_op::SGES => self.scalar_compare(op::F_ORD_GREATER_THAN_EQUAL, x),
            scalar_op::SNES => self.scalar_compare(op::F_ORD_NOT_EQUAL, x),
            scalar_op::FRCS => self.glsl450(glsl::FRACT, float, &[x]),
            scalar_op::TRUNCS => self.glsl450(glsl::TRUNC, float, &[x]),
            scalar_op::FLOORS => self.glsl450(glsl::FLOOR, float, &[x]),
            scalar_op::EXP => self.glsl450(glsl::EXP2, float, &[x]),
            scalar_op::LOG | scalar_op::LOGC => self.glsl450(glsl::LOG2, float, &[x]),
            scalar_op::SQRT => self.glsl450(glsl::SQRT, float, &[x]),
            scalar_op::SIN => self.glsl450(glsl::SIN, float, &[x]),
            scalar_op::COS => self.glsl450(glsl::COS, float, &[x]),
            scalar_op::RCP | scalar_op::RCPC | scalar_op::RCPF => {
                let one = self.m.constant_f(1.0);
                self.emit(op::F_DIV, float, &[one, x])
            }
            scalar_op::RSQ | scalar_op::RSQC | scalar_op::RSQF => self.glsl450(glsl::INVERSE_SQRT, float, &[x]),
            scalar_op::ADDS_PREV | scalar_op::MULS_PREV | scalar_op::SUBS_PREV => {
                if !self.have_previous_scalar {
                    self.fail("scalar prev with no previous result", 0);
                    return 0;
                }
                let previous_scalar = self.previous_scalar;
                let previous = self.emit(op::LOAD, float, &[previous_scalar]);
                let operation = match opcode {
                    scalar_op::ADDS_PREV => op::F_ADD,
                    scalar_op::MULS_PREV => op::F_MUL,
                    _ => op::F_SUB,
                };
                self.emit(operation, float, &[x, previous])
            }
            scalar_op::MULSC0 | scalar_op::MULSC1 => self.emit(op::F_MUL, float, &[x, y]),
            scalar_op::ADDSC0 | scalar_op::ADDSC1 => self.emit(op::F_ADD, float, &[x, y]),
            scalar_op::SUBSC0 | scalar_op::SUBSC1 => self.emit(op::F_SUB, float, &[x, y]),
            // The setp family write the predicate and a result together: the predicate
            // takes the comparison, the destination 0 when it holds and 1 when not.
            scalar_op::SETP_EQ
            | scalar_op::SETP_NE
            | scalar_op::SETP_GT
            | scalar_op::SETP_GE
            | scalar_op::SETP_RSTR => {
                let zero = self.m.constant_f(0.0);
                let comparison = match opcode {
                    scalar_op::SETP_NE => op::F_ORD_NOT_EQUAL,
                    scalar_op::SETP_GT => op::F_ORD_GREATER_THAN,
                    scalar_op::SETP_GE => op::F_ORD_GREATER_THAN_EQUAL,
                    _ => op::F_ORD_EQUAL,
                };
                let set = self.emit(comparison, bool_, &[x, zero]);
                let predicate = self.predicate;
                self.code(op::STORE, &[predicate, set]);
                // setp_rstr keeps the source when the predicate does not hold.
                let otherwise = if opcode == scalar_op::SETP_RSTR { x } else { self.m.constant_f(1.0) };
                self.emit(op::SELECT, float, &[set, zero, otherwise])
            }
            // The scalar kills test one component against zero, or against one, and
            // report whether they fired.
            scalar_op::KILLS_EQ
            | scalar_op::KILLS_GT
            | scalar_op::KILLS_GE
            | scalar_op::KILLS_NE
            | scalar_op::KILLS_ONE => {
                let against =
                    if opcode == scalar_op::KILLS_ONE { self.m.constant_f(1.0) } else { self.m.constant_f(0.0) };
                let comparison = match opcode {
                    scalar_op::KILLS_GT => op::F_ORD_GREATER_THAN,
                    scalar_op::KILLS_GE => op::F_ORD_GREATER_THAN_EQUAL,
                    scalar_op::KILLS_NE => op::F_ORD_NOT_EQUAL,
                    _ => op::F_ORD_EQUAL,
                };
                let fired = self.emit(comparison, bool_, &[x, against]);
                self.request_kill(fired);
                let one = self.m.constant_f(1.0);
                let zero = self.m.constant_f(0.0);
                self.emit(op::SELECT, float, &[fired, one, zero])
            }
            scalar_op::SETP_INV => {
                let one = self.m.constant_f(1.0);
                let zero = self.m.constant_f(0.0);
                let is_one = self.emit(op::F_ORD_EQUAL, bool_, &[x, one]);
                let predicate = self.predicate;
                self.code(op::STORE, &[predicate, is_one]);
                let is_zero = self.emit(op::F_ORD_EQUAL, bool_, &[x, zero]);
                let not_one = self.emit(op::SELECT, float, &[is_zero, one, x]);
                self.emit(op::SELECT, float, &[is_one, zero, not_one])
            }
            scalar_op::SETP_POP => {
                let zero = self.m.constant_f(0.0);
                let one = self.m.constant_f(1.0);
                let decremented = self.emit(op::F_SUB, float, &[x, one]);
                let set = self.emit(op::F_ORD_LESS_THAN_EQUAL, bool_, &[decremented, zero]);
                let predicate = self.predicate;
                self.code(op::STORE, &[predicate, set]);
                self.emit(op::SELECT, float, &[set, zero, decremented])
            }
            other => {
                self.fail("scalar opcode", other);
                return 0;
            }
        };

        let previous_scalar = self.previous_scalar;
        self.code(op::STORE, &[previous_scalar, r]);
        self.have_previous_scalar = true;
        self.build_vector(r, r, r, r)
    }

    pub(super) fn translate_alu(&mut self, alu: &Alu) {
        // A predicated instruction still executes; only its write is suppressed.
        let outer = self.condition;
        self.condition = self.condition_for(alu.is_predicated(), alu.predicate_condition());
        let float4 = self.float4;

        // The vector unit carries no state between instructions, so a fully masked
        // operation need not be built. The scalar unit does, so it always runs.
        let mut vector_result = 0;
        let writes = if alu.is_export() { alu.vector_result_mask() != 0 } else { alu.vector_write_mask() != 0 };
        if writes {
            vector_result = self.vector_operation(alu);
            if alu.vector_clamp() {
                let zero = self.splat(0.0);
                let one = self.splat(1.0);
                vector_result = self.glsl450(glsl::F_CLAMP, float4, &[vector_result, zero, one]);
            }
        }

        let mut scalar_result = 0;
        if alu.scalar_write_mask() != 0 || alu.scalar() != scalar_op::RETAIN_PREV {
            scalar_result = self.scalar_operation(alu);
            if scalar_result != 0 && alu.scalar_clamp() {
                let zero = self.splat(0.0);
                let one = self.splat(1.0);
                scalar_result = self.glsl450(glsl::F_CLAMP, float4, &[scalar_result, zero, one]);
            }
        }

        self.store_result(alu, vector_result, scalar_result);
        self.condition = outer;
    }

    // The 256 boolean constants in the hardware's packed shape, 8 dwords, so
    // the block is a straight copy of the registers: bit index%32 of dword
    // index/32.
    pub(super) fn bool_constant(&mut self, index: u32) -> Id {
        let index = index & 0xFF;
        let (uint, bool_) = (self.uint, self.bool_);
        let block = self.constant_block(bindings::BOOL_CONSTANTS);
        let dword = self.m.constant_u(index / 32);
        let at = self.emit(op::I_ADD, uint, &[block, dword]);
        let word = self.arena_word(at);
        let bit = self.m.constant_u(index % 32);
        let shifted = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[word, bit]);
        let one = self.m.constant_u(1);
        let masked = self.emit(op::BITWISE_AND, uint, &[shifted, one]);
        let zero = self.m.constant_u(0);
        self.emit(op::I_NOT_EQUAL, bool_, &[masked, zero])
    }

    // The 32 loop constants, one dword each, packing the iteration count, the
    // aL start value and the aL step.
    pub(super) fn loop_constant(&mut self, index: Id) -> Id {
        let uint = self.uint;
        let block = self.constant_block(bindings::LOOP_CONSTANTS);
        let at = self.emit(op::I_ADD, uint, &[block, index]);
        self.arena_word(at)
    }

    // The predicate register, or its negation.
    pub(super) fn predicate_is(&mut self, value: bool) -> Id {
        let (bool_, predicate_variable) = (self.bool_, self.predicate);
        let predicate = self.emit(op::LOAD, bool_, &[predicate_variable]);
        if value { predicate } else { self.emit(op::LOGICAL_NOT, bool_, &[predicate]) }
    }

    // What an instruction's writes are suppressed under: the block's condition,
    // and the instruction's own predicate when it has one.
    pub(super) fn condition_for(&mut self, predicated: bool, condition: bool) -> Id {
        if !predicated {
            return self.condition;
        }
        let wanted = self.predicate_is(condition);
        let current = self.condition;
        if current != 0 {
            let bool_ = self.bool_;
            self.emit(op::LOGICAL_AND, bool_, &[current, wanted])
        } else {
            wanted
        }
    }

    // Suppressed under the condition in force exactly as a register write is.
    pub(super) fn store_predicate(&mut self, mut value: Id) {
        let (bool_, predicate, condition) = (self.bool_, self.predicate, self.condition);
        if condition != 0 {
            let current = self.emit(op::LOAD, bool_, &[predicate]);
            value = self.emit(op::SELECT, bool_, &[condition, value, current]);
        }
        self.code(op::STORE, &[predicate, value]);
    }

    // OpKill terminates its block and every block here belongs to the dispatch
    // switch, so the discard is recorded and performed once the sequencer
    // finishes. Observable only to a shader exporting to memory after killing.
    pub(super) fn request_kill(&mut self, mut condition: Id) {
        let (bool_, killed, current) = (self.bool_, self.killed, self.condition);
        if current != 0 {
            condition = self.emit(op::LOGICAL_AND, bool_, &[current, condition]);
        }
        let already = self.emit(op::LOAD, bool_, &[killed]);
        let either = self.emit(op::LOGICAL_OR, bool_, &[already, condition]);
        self.code(op::STORE, &[killed, either]);
        self.have_kill = true;
    }

    // The operand arrives swizzled .z_xy -- the title writes `cube r, src.zzxy,
    // src.yxzz` -- so Z is component 0, X is component 2 and Y is component 3.
    // Component 1 is the one that is skipped. The result is (T, S, 2 * major
    // axis, face). Selects rather than branches, to stay in one basic block. The
    // second operand is decorative: the hardware reads XYZ from the first alone.
    pub(super) fn cube_coordinates(&mut self, a: Id, _b: Id) -> Id {
        let (float, bool_) = (self.float, self.bool_);
        let x = self.component(a, 2);
        let y = self.component(a, 3);
        let z = self.component(a, 0);
        let ax = self.glsl450(glsl::F_ABS, float, &[x]);
        let ay = self.glsl450(glsl::F_ABS, float, &[y]);
        let az = self.glsl450(glsl::F_ABS, float, &[z]);
        let negate = |t: &mut Self, v: Id| t.emit(op::F_NEGATE, float, &[v]);
        let ge = |t: &mut Self, p: Id, q: Id| t.emit(op::F_ORD_GREATER_THAN_EQUAL, bool_, &[p, q]);
        let pick = |t: &mut Self, cond: Id, yes: Id, no: Id| t.emit(op::SELECT, float, &[cond, yes, no]);

        let zero = self.m.constant_f(0.0);
        let z_over_x = ge(self, az, ax);
        let z_over_y = ge(self, az, ay);
        let z_major = self.emit(op::LOGICAL_AND, bool_, &[z_over_x, z_over_y]);
        let not_z = self.emit(op::LOGICAL_NOT, bool_, &[z_major]);
        let y_over_x = ge(self, ay, ax);
        let y_major = self.emit(op::LOGICAL_AND, bool_, &[not_z, y_over_x]);
        let z_neg = self.emit(op::F_ORD_LESS_THAN, bool_, &[z, zero]);
        let y_neg = self.emit(op::F_ORD_LESS_THAN, bool_, &[y, zero]);
        let x_neg = self.emit(op::F_ORD_LESS_THAN, bool_, &[x, zero]);

        // sc = pick(zMajor, pick(zNeg, -x, x), pick(yMajor, x, pick(xNeg, z, -z)))
        let minus_x = negate(self, x);
        let sc_z = pick(self, z_neg, minus_x, x);
        let minus_z = negate(self, z);
        let sc_x = pick(self, x_neg, z, minus_z);
        let sc_rest = pick(self, y_major, x, sc_x);
        let sc = pick(self, z_major, sc_z, sc_rest);
        // tc = pick(zMajor, -y, pick(yMajor, pick(yNeg, -z, z), -y))
        let minus_y = negate(self, y);
        let minus_z = negate(self, z);
        let tc_y = pick(self, y_neg, minus_z, z);
        let minus_y2 = negate(self, y);
        let tc_rest = pick(self, y_major, tc_y, minus_y2);
        let tc = pick(self, z_major, minus_y, tc_rest);
        // ma = pick(zMajor, z, pick(yMajor, y, x))
        let ma_rest = pick(self, y_major, y, x);
        let ma = pick(self, z_major, z, ma_rest);
        // face = pick(zMajor, pick(zNeg, 5, 4), pick(yMajor, pick(yNeg, 3, 2), pick(xNeg, 1, 0)))
        let five = self.m.constant_f(5.0);
        let four = self.m.constant_f(4.0);
        let face_z = pick(self, z_neg, five, four);
        let three = self.m.constant_f(3.0);
        let two = self.m.constant_f(2.0);
        let face_y = pick(self, y_neg, three, two);
        let one = self.m.constant_f(1.0);
        let nought = self.m.constant_f(0.0);
        let face_x = pick(self, x_neg, one, nought);
        let face_rest = pick(self, y_major, face_y, face_x);
        let face = pick(self, z_major, face_z, face_rest);
        let two = self.m.constant_f(2.0);
        let two_ma = self.emit(op::F_MUL, float, &[ma, two]);
        self.build_vector(tc, sc, two_ma, face)
    }
}
