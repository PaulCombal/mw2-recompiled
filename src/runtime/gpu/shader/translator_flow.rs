// The sequencer: the function's variables, the control flow instructions,
// and the body that runs them -- straight-line or through the dispatcher.

use super::spirv::{self, Id, glsl, op, storage};
use super::translator::{
    AFTER_LOOP, EXECUTION_MODE_ORIGIN_UPPER_LEFT, EXECUTION_MODEL_FRAGMENT, EXECUTION_MODEL_VERTEX,
    FUNCTION_CONTROL_NONE, IN_CASE, IN_DISPATCH, IN_ENTRY, IN_LOOP_HEADER, LOOP_CONTROL_NONE, LOOP_STACK_DEPTH,
    SCOPE_SUBGROUP, SELECTION_CONTROL_NONE, Translator,
};
use super::ucode::{Alu, ControlFlow, Fetch, INTERPOLATOR_COUNT, TEMP_REGISTER_COUNT, cf_op};

impl Translator<'_> {
    fn array_element(&mut self, array: Id, index: Id) -> Id {
        let uint = self.uint;
        let pointer = self.m.pointer(storage::FUNCTION, uint);
        let chain = self.emit(op::ACCESS_CHAIN, pointer, &[array, index]);
        self.emit(op::LOAD, uint, &[chain])
    }

    fn set_array_element(&mut self, array: Id, index: Id, value: Id) {
        let pointer = self.m.pointer(storage::FUNCTION, self.uint);
        let chain = self.emit(op::ACCESS_CHAIN, pointer, &[array, index]);
        self.code(op::STORE, &[chain, value]);
    }

    // Unconditional, or on the predicate, or on a boolean constant, in that
    // order of precedence.
    fn jump_taken(&mut self, cf: &ControlFlow) -> Id {
        if cf.is_unconditional() {
            return 0;
        }
        let bool_ = self.bool_;
        let wanted = if cf.is_predicated_jump() {
            let predicate = self.predicate;
            self.emit(op::LOAD, bool_, &[predicate])
        } else {
            self.bool_constant(cf.jump_bool_address())
        };
        if cf.jump_condition() { wanted } else { self.emit(op::LOGICAL_NOT, bool_, &[wanted]) }
    }

    fn emit_sequencer_state(&mut self) {
        let uint_pointer = self.m.pointer(storage::FUNCTION, self.uint);
        let zero = self.m.constant_u(0);
        let scalar = |t: &mut Self, name: &str| -> Id {
            let slot = t.m.allocate();
            spirv::emit(&mut t.m.code, op::VARIABLE, &[uint_pointer, slot, storage::FUNCTION, zero]);
            spirv::emit_string(&mut t.m.debug, op::NAME, &[slot], name);
            slot
        };
        self.pc = scalar(self, "pc");
        self.last_fetch_base = scalar(self, "vertex_fetch_address");
        self.last_fetch_endian = scalar(self, "vertex_fetch_endian");

        let float_pointer = self.m.pointer(storage::FUNCTION, self.float);
        self.previous_scalar = self.m.allocate();
        let nought = self.m.constant_f(0.0);
        spirv::emit(&mut self.m.code, op::VARIABLE, &[float_pointer, self.previous_scalar, storage::FUNCTION, nought]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[self.previous_scalar], "previous_scalar");
        self.loop_depth = scalar(self, "loop_depth");
        self.call_depth = scalar(self, "call_depth");

        // aL is signed and clamped to [-256, 256] by the sequencer.
        let int_pointer = self.m.pointer(storage::FUNCTION, self.int);
        self.address_register = self.m.allocate();
        let zero_int = self.m.constant_s(0);
        spirv::emit(&mut self.m.code, op::VARIABLE, &[int_pointer, self.address_register, storage::FUNCTION, zero_int]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[self.address_register], "aL");
        self.a0 = self.m.allocate();
        let zero_int = self.m.constant_s(0);
        spirv::emit(&mut self.m.code, op::VARIABLE, &[int_pointer, self.a0, storage::FUNCTION, zero_int]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[self.a0], "a0");
        self.lod = self.m.allocate();
        let nought = self.m.constant_f(0.0);
        spirv::emit(&mut self.m.code, op::VARIABLE, &[float_pointer, self.lod, storage::FUNCTION, nought]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[self.lod], "lod");

        let bool_pointer = self.m.pointer(storage::FUNCTION, self.bool_);
        self.killed = self.m.allocate();
        let no = self.m.constant_false_value();
        spirv::emit(&mut self.m.code, op::VARIABLE, &[bool_pointer, self.killed, storage::FUNCTION, no]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[self.killed], "killed");

        let stack = self.m.array_of(self.uint, LOOP_STACK_DEPTH);
        let stack_pointer = self.m.pointer(storage::FUNCTION, stack);
        let array = |t: &mut Self, name: &str| -> Id {
            let slot = t.m.allocate();
            spirv::emit(&mut t.m.code, op::VARIABLE, &[stack_pointer, slot, storage::FUNCTION]);
            spirv::emit_string(&mut t.m.debug, op::NAME, &[slot], name);
            slot
        };
        self.loop_iterators = array(self, "loop_iterators");
        self.loop_ids = array(self, "loop_ids");
        self.call_stack = array(self, "call_stack");
    }

    // aL = iterator * step + start, from the innermost loop's constant,
    // clamped to [-256, 256]. Step and start are signed 8-bit fields.
    fn update_address_register(&mut self, depth: Id, iterator: Id) {
        let (uint, int, bool_) = (self.uint, self.int, self.bool_);
        let loop_ids = self.loop_ids;
        let id = self.array_element(loop_ids, depth);
        let constant = self.loop_constant(id);
        let eight = self.m.constant_u(8);
        let shifted = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[constant, eight]);
        let byte = self.m.constant_u(0xFF);
        let start = self.emit(op::BITWISE_AND, uint, &[shifted, byte]);
        let sixteen = self.m.constant_u(16);
        let shifted = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[constant, sixteen]);
        let byte = self.m.constant_u(0xFF);
        let step = self.emit(op::BITWISE_AND, uint, &[shifted, byte]);
        // Values above 127 are negative.
        let mut signed_step = self.emit(op::BITCAST, int, &[step]);
        let limit = self.m.constant_s(127);
        let negative = self.emit(op::S_GREATER_THAN, bool_, &[signed_step, limit]);
        let range = self.m.constant_s(256);
        let folded = self.emit(op::I_SUB, int, &[signed_step, range]);
        signed_step = self.emit(op::SELECT, int, &[negative, folded, signed_step]);
        let signed_iterator = self.emit(op::BITCAST, int, &[iterator]);
        let product = self.emit(op::I_MUL, int, &[signed_iterator, signed_step]);
        let signed_start = self.emit(op::BITCAST, int, &[start]);
        let mut value = self.emit(op::I_ADD, int, &[product, signed_start]);
        let low = self.m.constant_s(-256);
        let high = self.m.constant_s(256);
        value = self.glsl450(glsl::S_CLAMP, int, &[value, low, high]);
        let address_register = self.address_register;
        self.code(op::STORE, &[address_register, value]);
    }

    // `self_index` is the instruction's index, and every path through has to
    // leave the program counter set.
    fn emit_control_flow(&mut self, cf: &ControlFlow, self_index: u32) {
        let next = self_index.wrapping_add(1);
        let stop = self.flow.len() as u32; // no case: falls to default
        let (uint, bool_) = (self.uint, self.bool_);

        match cf.opcode() {
            cf_op::NOP | cf_op::ALLOC | cf_op::MARK_VS_FETCH_DONE => {
                self.set_pc_to(next);
                return;
            }

            cf_op::COND_EXEC | cf_op::COND_EXEC_END => {
                // Same shape as a predicated block, but on a boolean constant. Both are
                // uniform within the block, so a select on every store is exact.
                let value = self.bool_constant(cf.bool_address());
                self.condition =
                    if cf.predicate_condition() { value } else { self.emit(op::LOGICAL_NOT, bool_, &[value]) };
            }

            cf_op::COND_EXEC_PRED
            | cf_op::COND_EXEC_PRED_END
            | cf_op::COND_EXEC_PRED_CLEAN
            | cf_op::COND_EXEC_PRED_CLEAN_END => {
                self.condition = self.predicate_is(cf.predicate_condition());
            }

            cf_op::EXEC | cf_op::EXEC_END => {}

            cf_op::LOOP_START => {
                // Reset the iterator unless this is a repeat, then skip the whole loop if
                // it has already run its count.
                let (loop_depth, loop_ids, loop_iterators) = (self.loop_depth, self.loop_ids, self.loop_iterators);
                let depth = self.emit(op::LOAD, uint, &[loop_depth]);
                let id = self.m.constant_u(cf.loop_id());
                self.set_array_element(loop_ids, depth, id);
                if !cf.is_repeat() {
                    let zero = self.m.constant_u(0);
                    self.set_array_element(loop_iterators, depth, zero);
                }
                let iterator = self.array_element(loop_iterators, depth);
                let constant = self.loop_constant(id);
                let byte = self.m.constant_u(0xFF);
                let count = self.emit(op::BITWISE_AND, uint, &[constant, byte]);
                let done = self.emit(op::U_GREATER_THAN_EQUAL, bool_, &[iterator, count]);
                // Entering the loop pushes and sets aL; skipping it does neither.
                let one = self.m.constant_u(1);
                let entered = self.emit(op::I_ADD, uint, &[depth, one]);
                let new_depth = self.emit(op::SELECT, uint, &[done, depth, entered]);
                self.code(op::STORE, &[loop_depth, new_depth]);
                self.update_address_register(depth, iterator);
                let target = self.m.constant_u(cf.target());
                let after = self.m.constant_u(next);
                let pc = self.emit(op::SELECT, uint, &[done, target, after]);
                self.set_pc(pc);
                return;
            }

            cf_op::LOOP_END => {
                // Go round again unless the count is exhausted or a predicated break
                // fires.
                let (loop_depth, loop_ids, loop_iterators) = (self.loop_depth, self.loop_ids, self.loop_iterators);
                let depth = self.emit(op::LOAD, uint, &[loop_depth]);
                let one = self.m.constant_u(1);
                let inner = self.emit(op::I_SUB, uint, &[depth, one]);
                let id = self.array_element(loop_ids, inner);
                let current = self.array_element(loop_iterators, inner);
                let one = self.m.constant_u(1);
                let iterator = self.emit(op::I_ADD, uint, &[current, one]);
                self.set_array_element(loop_iterators, inner, iterator);
                let constant = self.loop_constant(id);
                let byte = self.m.constant_u(0xFF);
                let count = self.emit(op::BITWISE_AND, uint, &[constant, byte]);
                let mut more = self.emit(op::U_LESS_THAN, bool_, &[iterator, count]);
                if cf.is_predicated_break() {
                    let breaks = self.predicate_is(cf.loop_condition());
                    let stays = self.emit(op::LOGICAL_NOT, bool_, &[breaks]);
                    more = self.emit(op::LOGICAL_AND, bool_, &[more, stays]);
                }
                // Leaving pops; going round again keeps the depth and steps aL.
                let new_depth = self.emit(op::SELECT, uint, &[more, depth, inner]);
                self.code(op::STORE, &[loop_depth, new_depth]);
                self.update_address_register(inner, iterator);
                let target = self.m.constant_u(cf.target());
                let after = self.m.constant_u(next);
                let pc = self.emit(op::SELECT, uint, &[more, target, after]);
                self.set_pc(pc);
                return;
            }

            cf_op::COND_CALL => {
                let (call_depth, call_stack) = (self.call_depth, self.call_stack);
                let depth = self.emit(op::LOAD, uint, &[call_depth]);
                let limit = self.m.constant_u(LOOP_STACK_DEPTH);
                let room = self.emit(op::U_LESS_THAN, bool_, &[depth, limit]);
                let mut taken = self.jump_taken(cf);
                taken = if taken != 0 { self.emit(op::LOGICAL_AND, bool_, &[taken, room]) } else { room };
                // The stack slot is written unconditionally and the depth only when the
                // call is taken, which keeps this one basic block.
                let last = self.m.constant_u(LOOP_STACK_DEPTH - 1);
                let slot = self.emit(op::SELECT, uint, &[room, depth, last]);
                let after = self.m.constant_u(next);
                self.set_array_element(call_stack, slot, after);
                let one = self.m.constant_u(1);
                let pushed = self.emit(op::I_ADD, uint, &[depth, one]);
                let new_depth = self.emit(op::SELECT, uint, &[taken, pushed, depth]);
                self.code(op::STORE, &[call_depth, new_depth]);
                let target = self.m.constant_u(cf.target());
                let after = self.m.constant_u(next);
                let pc = self.emit(op::SELECT, uint, &[taken, target, after]);
                self.set_pc(pc);
                return;
            }

            cf_op::RETURN => {
                // A return outside a call is well defined: it does nothing.
                let (call_depth, call_stack) = (self.call_depth, self.call_stack);
                let depth = self.emit(op::LOAD, uint, &[call_depth]);
                let zero = self.m.constant_u(0);
                let in_call = self.emit(op::U_LESS_THAN, bool_, &[zero, depth]);
                let one = self.m.constant_u(1);
                let popped = self.emit(op::I_SUB, uint, &[depth, one]);
                let zero = self.m.constant_u(0);
                let inner = self.emit(op::SELECT, uint, &[in_call, popped, zero]);
                self.code(op::STORE, &[call_depth, inner]);
                let back = self.array_element(call_stack, inner);
                let after = self.m.constant_u(next);
                let pc = self.emit(op::SELECT, uint, &[in_call, back, after]);
                self.set_pc(pc);
                return;
            }

            cf_op::COND_JMP => {
                let taken = self.jump_taken(cf);
                if taken == 0 {
                    self.set_pc_to(cf.target());
                    return;
                }
                let target = self.m.constant_u(cf.target());
                let after = self.m.constant_u(next);
                let pc = self.emit(op::SELECT, uint, &[taken, target, after]);
                self.set_pc(pc);
                return;
            }

            other => {
                self.fail("control flow opcode", other);
                return;
            }
        }

        let words = self.words;
        for n in 0..cf.count() {
            let at = (cf.address() + n) as usize * 3;
            if at + 3 > words.len() {
                self.fail("instruction past the end of the program", at as u32);
                return;
            }
            self.result.meta.instruction_count = self.result.meta.instruction_count.wrapping_add(1);
            if cf.is_fetch(n) {
                self.translate_fetch(&Fetch { w0: words[at], w1: words[at + 1], w2: words[at + 2] });
            } else {
                self.translate_alu(&Alu { w0: words[at], w1: words[at + 1], w2: words[at + 2] });
            }
        }
        self.condition = 0;
        self.set_pc_to(if cf.is_end() { stop } else { next });
    }

    pub(super) fn emit_body(&mut self) {
        let (void, uint, int, float, float4, bool_) =
            (self.void, self.uint, self.int, self.float, self.float4, self.bool_);
        let function_type = self.m.allocate();
        spirv::emit(&mut self.m.declarations, op::TYPE_FUNCTION, &[function_type, void]);

        let entry = self.m.allocate();
        let model = if self.pixel { EXECUTION_MODEL_FRAGMENT } else { EXECUTION_MODEL_VERTEX };
        let interface = std::mem::take(&mut self.entry_interface);
        self.m.entry_point(model, entry, "main", &interface);
        self.entry_interface = interface;
        if self.pixel {
            spirv::emit(&mut self.m.execution_modes, op::EXECUTION_MODE, &[entry, EXECUTION_MODE_ORIGIN_UPPER_LEFT]);
        }

        self.code(op::FUNCTION, &[void, entry, FUNCTION_CONTROL_NONE, function_type]);
        let label = self.m.allocate();
        self.code(op::LABEL, &[label]);

        // SPIR-V wants every function variable declared in the entry block, and
        // drivers promote them to registers, so this saves building SSA by hand.
        let temps = self.m.array_of(float4, TEMP_REGISTER_COUNT);
        let pointer = self.m.pointer(storage::FUNCTION, temps);
        self.temps = self.m.allocate();
        let temps_variable = self.temps;
        self.code(op::VARIABLE, &[pointer, temps_variable, storage::FUNCTION]);
        spirv::emit_string(&mut self.m.debug, op::NAME, &[temps_variable], "r");

        let bool_pointer = self.m.pointer(storage::FUNCTION, bool_);
        self.predicate = self.m.allocate();
        let no = self.m.constant_false_value();
        let predicate = self.predicate;
        self.code(op::VARIABLE, &[bool_pointer, predicate, storage::FUNCTION, no]);

        self.emit_sequencer_state();

        // The record's offset, for every block below. The pixel stage's comes
        // in as a flat value, the same for every invocation of the draw, which
        // the compiler cannot know: taken from the first invocation, it is one
        // scalar register again rather than one per pixel, and so is every
        // address computed from it.
        let mut base_vertex = 0;
        let record_input = self.record_input;
        if !self.pixel {
            base_vertex = self.emit(op::LOAD, int, &[record_input]);
            self.record = self.emit(op::BITCAST, uint, &[base_vertex]);
            let (record_output, record) = (self.record_output, self.record);
            self.code(op::STORE, &[record_output, record]);
        } else {
            let scope = self.m.constant_u(SCOPE_SUBGROUP);
            let flat = self.emit(op::LOAD, uint, &[record_input]);
            self.record = self.emit(op::GROUP_NON_UNIFORM_BROADCAST_FIRST, uint, &[scope, flat]);
        }

        if !self.pixel && self.vertex_index != 0 {
            // Less the base vertex, which is the record's offset and not the
            // title's: the index the title's draw names.
            let vertex_index = self.vertex_index;
            let loaded = self.emit(op::LOAD, int, &[vertex_index]);
            let index = self.emit(op::I_SUB, int, &[loaded, base_vertex]);
            let as_float = self.emit(op::CONVERT_S_TO_F, float, &[index]);
            let zero = self.m.constant_f(0.0);
            let value = self.emit(op::COMPOSITE_CONSTRUCT, float4, &[as_float, zero, zero, zero]);
            let first = self.m.constant_s(0);
            let pointer = self.temp_pointer(first);
            self.code(op::STORE, &[pointer, value]);
        }

        if !self.pixel {
            let zero = self.m.constant_f(0.0);
            let zero4 = self.emit(op::COMPOSITE_CONSTRUCT, float4, &[zero, zero, zero, zero]);
            for i in 0..INTERPOLATOR_COUNT as usize {
                let interpolator = self.interpolators[i];
                if interpolator != 0 {
                    self.code(op::STORE, &[interpolator, zero4]);
                }
            }
            let point_size = self.point_size;
            if point_size != 0 {
                let one = self.m.constant_f(1.0);
                self.code(op::STORE, &[point_size, one]);
            }
        }

        if self.pixel {
            for i in 0..INTERPOLATOR_COUNT {
                let interpolator = self.interpolators[i as usize];
                if interpolator != 0 {
                    let value = self.emit(op::LOAD, float4, &[interpolator]);
                    let index = self.m.constant_s(i as i32);
                    let pointer = self.temp_pointer(index);
                    self.code(op::STORE, &[pointer, value]);
                }
            }
        }

        // The sequencer is a program counter over the control flow list, and Xenos
        // jumps, calls and loops move it arbitrarily. Rather than recover structured
        // control flow, the module runs one loop whose body is a switch on the
        // counter, each control flow instruction a case:
        //
        //   loop { switch (pc) { case 0: ...; pc = 1; ... default: break; } }
        //
        // Most programs have no jumps, calls or loops (nine in ten of the
        // game's pixel shaders, every vertex shader). The counter then only
        // ever steps through the cases in order, and the list ends at the
        // first End (decode_control_flow stops there), so those are emitted
        // straight-line instead. Drivers do not undo the loop themselves --
        // RADV keeps the switch, and every register live across it -- so the
        // straight-line form needs fewer registers and instructions. Each case
        // still stores the next pc, unread.
        let straight = !self.flow.iter().any(|cf| {
            matches!(
                cf.opcode(),
                cf_op::LOOP_START | cf_op::LOOP_END | cf_op::COND_CALL | cf_op::RETURN | cf_op::COND_JMP
            )
        });
        if straight {
            for i in 0..self.flow.len() as u32 {
                let cf = self.flow[i as usize];
                self.emit_control_flow(&cf, i);
                if !self.result.error.is_empty() {
                    return;
                }
            }
        } else {
            let case_count = self.flow.len() as u32;
            let header = self.m.allocate();
            let dispatch = self.m.allocate();
            // The switch needs a merge block of its own: a selection construct
            // may not merge at the loop's continue target. This one exists only
            // to branch on.
            let case_merge = self.m.allocate();
            let continue_target = self.m.allocate();
            let merge = self.m.allocate();
            // The default needs its own block for the same reason. It breaks
            // out of the loop, which is how a shader ends.
            let default_block = self.m.allocate();
            let mut blocks: Vec<Id> = Vec::with_capacity(case_count as usize);
            for _ in 0..case_count {
                blocks.push(self.m.allocate());
            }

            self.block_end[IN_ENTRY as usize] = self.m.code.len();
            self.code(op::BRANCH, &[header]);
            self.code(op::LABEL, &[header]);
            self.block_end[IN_LOOP_HEADER as usize] = self.m.code.len();
            self.code(op::LOOP_MERGE, &[merge, continue_target, LOOP_CONTROL_NONE]);
            self.code(op::BRANCH, &[dispatch]);

            self.code(op::LABEL, &[dispatch]);
            self.block_end[IN_DISPATCH as usize] = self.m.code.len();
            let pc = self.pc;
            let current = self.emit(op::LOAD, uint, &[pc]);
            self.code(op::SELECTION_MERGE, &[case_merge, SELECTION_CONTROL_NONE]);
            {
                let code = &mut self.m.code;
                code.push(spirv::head(op::SWITCH, 2 * case_count + 3));
                code.push(current);
                code.push(default_block);
                for (i, &block) in blocks.iter().enumerate() {
                    code.push(i as u32);
                    code.push(block);
                }
            }

            for i in 0..case_count {
                self.code(op::LABEL, &[blocks[i as usize]]);
                self.block = IN_CASE + i;
                let cf = self.flow[i as usize];
                self.emit_control_flow(&cf, i);
                if !self.result.error.is_empty() {
                    return;
                }
                self.code(op::BRANCH, &[case_merge]);
            }

            self.code(op::LABEL, &[default_block]);
            self.code(op::BRANCH, &[merge]);

            self.code(op::LABEL, &[case_merge]);
            self.code(op::BRANCH, &[continue_target]);
            self.code(op::LABEL, &[continue_target]);
            self.code(op::BRANCH, &[header]);
            self.code(op::LABEL, &[merge]);
            self.block = AFTER_LOOP;
        }

        if self.pixel && self.colours[0] != 0 {
            self.emit_alpha_test();
        }
        if self.pixel {
            self.emit_exponent_bias();
        }
        if self.pixel {
            self.emit_gamma();
        }
        if !self.pixel && self.position != 0 {
            self.emit_window_to_clip();
        }

        // The discard any kill asked for, now that OpKill can terminate a block
        // without breaking the switch. A straight-line program defers it too,
        // so a memory export after a kill behaves the same in both forms.
        if self.have_kill && self.pixel {
            let discard = self.m.allocate();
            let carry_on = self.m.allocate();
            let killed_variable = self.killed;
            let killed = self.emit(op::LOAD, bool_, &[killed_variable]);
            self.code(op::SELECTION_MERGE, &[carry_on, SELECTION_CONTROL_NONE]);
            self.code(op::BRANCH_CONDITIONAL, &[killed, discard, carry_on]);
            self.code(op::LABEL, &[discard]);
            self.code(op::KILL, &[]);
            self.code(op::LABEL, &[carry_on]);
        }

        self.code(op::RETURN, &[]);
        self.code(op::FUNCTION_END, &[]);
        self.place_record_loads();
    }
}
