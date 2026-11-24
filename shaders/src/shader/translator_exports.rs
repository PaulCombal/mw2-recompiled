// Where results go: masked and predicated stores, exports, and what a pixel
// or vertex shader does to its outputs once the program has run -- the alpha
// test, the exponent bias, the PWL gamma encode and the window-to-clip
// transform, in that order.

use super::bindings;
use super::spirv::{Id, glsl, op};
use super::translator::Translator;
use super::ucode::{
    Alu, EXPORT_DEPTH, EXPORT_MEMORY_ADDRESS, EXPORT_MEMORY_LAST, EXPORT_POINT_SIZE, EXPORT_POSITION,
    INTERPOLATOR_COUNT,
};

impl Translator<'_> {
    pub(super) fn store_masked(&mut self, pointer: Id, value: Id, mask: u32) {
        if mask == 0 {
            return;
        }
        let (float4, bool4, condition) = (self.float4, self.bool4, self.condition);
        let mut merged = value;
        if mask != 0xF || condition != 0 {
            let current = self.emit(op::LOAD, float4, &[pointer]);
            if mask != 0xF {
                // Shuffle picks from `current` (0..3) or `value` (4..7).
                let mut pick = [0u32; 4];
                for c in 0..4u32 {
                    pick[c as usize] = if mask & (1 << c) != 0 { 4 + c } else { c };
                }
                merged = self.emit(op::VECTOR_SHUFFLE, float4, &[current, value, pick[0], pick[1], pick[2], pick[3]]);
            }
            if condition != 0 {
                // SPIR-V 1.0 wants the condition to match the operands in shape.
                let wide = self.emit(op::COMPOSITE_CONSTRUCT, bool4, &[condition, condition, condition, condition]);
                merged = self.emit(op::SELECT, float4, &[wide, merged, current]);
            }
        }
        self.code(op::STORE, &[pointer, merged]);
    }

    pub(super) fn store_result(&mut self, alu: &Alu, vector_result: Id, scalar_result: Id) {
        if !alu.is_export() {
            if alu.vector_write_mask() != 0 && vector_result != 0 {
                let index = self.temp_index(alu.vector_dest(), alu.vector_dest_relative());
                let pointer = self.temp_pointer(index);
                self.store_masked(pointer, vector_result, alu.vector_write_mask());
            }
            if alu.scalar_write_mask() != 0 && scalar_result != 0 {
                let index = self.temp_index(alu.scalar_dest(), alu.scalar_dest_relative());
                let pointer = self.temp_pointer(index);
                self.store_masked(pointer, scalar_result, alu.scalar_write_mask());
            }
            return;
        }

        // An export is one destination written by both halves under complementary
        // masks, plus literal 0 and 1 where the masks say so.
        let dest = alu.vector_dest();
        let mut target = 0;
        if !self.pixel {
            if dest < INTERPOLATOR_COUNT {
                target = self.interpolators[dest as usize];
            } else if dest == EXPORT_POSITION {
                target = self.position;
            } else if dest == EXPORT_POINT_SIZE {
                return; // no point sprites yet
            } else if (EXPORT_MEMORY_ADDRESS..=EXPORT_MEMORY_LAST).contains(&dest) {
                // A memory export writes to a guest physical address, and set 2
                // is not guest memory: it is the frame arena, holding copies of
                // only the vertex data each draw reads, at rewritten addresses.
                // There is nowhere for this to land without mapping guest RAM to
                // the GPU, which is a change to the whole design and not to this
                // program. The geometry it draws is perfectly good, though, and
                // refusing the program threw that away too -- 2376 draws a run,
                // whose absence left holes in the depth buffer that the sky then
                // filled in, white, over the floor. So the write is dropped and
                // counted, and the drawing goes ahead.
                self.result.meta.dropped_memory_exports = self.result.meta.dropped_memory_exports.wrapping_add(1);
                return;
            }
        } else if dest < 4 {
            target = self.colours[dest as usize];
        } else if dest == EXPORT_DEPTH {
            self.fail("depth export", dest);
            return;
        } else {
            // A pixel shader has four colour outputs and depth. An export to anything else
            // has nowhere to land, on this hardware as much as here, so it is dropped --
            // but counted.
            self.result.meta.dropped_exports = self.result.meta.dropped_exports.wrapping_add(1);
            return;
        }
        if target == 0 {
            self.fail("export register", dest);
            return;
        }

        if vector_result != 0 {
            self.store_masked(target, vector_result, alu.vector_result_mask());
        }
        if scalar_result != 0 {
            self.store_masked(target, scalar_result, alu.scalar_result_mask());
        }
        let mask = alu.constant_zero_mask();
        if mask != 0 {
            let zero = self.splat(0.0);
            self.store_masked(target, zero, mask);
        }
        let mask = alu.constant_one_mask();
        if mask != 0 {
            let one = self.splat(1.0);
            self.store_masked(target, one, mask);
        }
    }

    // The alpha test, folded into the same discard. The three comparison
    // results are exactly the bits of the console's compare function --
    // 001 less, 010 equal, 100 greater -- so the whole test is one mask and
    // one and, with `never` and `always` falling out of it for free.
    pub(super) fn emit_alpha_test(&mut self) {
        let (float, float4, bool_) = (self.float, self.float4, self.bool_);
        let uint32 = self.m.int(false);
        let function = self.record_word(bindings::RECORD_PIXEL);
        let reference = self.record_float(bindings::RECORD_PIXEL + 1);
        let colour = self.colours[0];
        let value = self.emit(op::LOAD, float4, &[colour]);
        let alpha = self.emit(op::COMPOSITE_EXTRACT, float, &[value, 3]);
        let zero = self.m.constant_u(0);
        let bit = |t: &mut Self, comparison: u32, value: u32| -> Id {
            let holds = t.emit(comparison, bool_, &[alpha, reference]);
            let set = t.m.constant_u(value);
            t.emit(op::SELECT, uint32, &[holds, set, zero])
        };
        let less = bit(self, op::F_ORD_LESS_THAN, 1);
        let equal = bit(self, op::F_ORD_EQUAL, 2);
        let low = self.emit(op::BITWISE_OR, uint32, &[less, equal]);
        let greater = bit(self, op::F_ORD_GREATER_THAN, 4);
        let mask = self.emit(op::BITWISE_OR, uint32, &[low, greater]);
        // `always` has to be inert, not merely a function that happens to match
        // every comparison. The three comparisons are ordered, so an alpha that
        // is not a number answers false to all of them, the mask comes out zero
        // and the pixel dies whatever the function says -- and a shader that
        // does not write its alpha leaves exactly that. The prepass shaders do
        // not write alpha, which is why their fragments were being discarded
        // and their depth never written, and the sky then filled the hole in.
        let always = self.m.constant_u(7);
        let enabled = self.emit(op::I_NOT_EQUAL, bool_, &[function, always]);
        let passed = self.emit(op::BITWISE_AND, uint32, &[function, mask]);
        let rejected = self.emit(op::I_EQUAL, bool_, &[passed, zero]);
        let failed = self.emit(op::LOGICAL_AND, bool_, &[enabled, rejected]);
        let killed = self.killed;
        let already = self.emit(op::LOAD, bool_, &[killed]);
        let either = self.emit(op::LOGICAL_OR, bool_, &[already, failed]);
        self.code(op::STORE, &[killed, either]);
        self.have_kill = true;
    }

    // The console scales what a pixel shader wrote by two to the power of
    // RB_COLOR_INFO's exponent bias on the way into EDRAM, and the title
    // renders the world with a bias -- so a shader's output is not the
    // colour the screen gets, and ignoring it left every lit surface a
    // fixed fraction of its brightness. After the alpha test, which the
    // hardware runs on the unbiased alpha.
    pub(super) fn emit_exponent_bias(&mut self) {
        let float4 = self.float4;
        let scale = self.record_float(bindings::RECORD_PIXEL + 2);
        for colour in self.colours {
            if colour == 0 {
                continue;
            }
            let value = self.emit(op::LOAD, float4, &[colour]);
            let scaled = self.emit(op::VECTOR_TIMES_SCALAR, float4, &[value, scale]);
            self.code(op::STORE, &[colour, scaled]);
        }
    }

    // Xenos runs a pixel shader's colour through a piecewise-linear gamma
    // curve on the way into an 8_8_8_8_GAMMA target, and MW2 draws the
    // world's colour pass into one -- 4.4 million draws a run against
    // 230 thousand into a plain 8_8_8_8. Storing the linear value instead
    // left the whole world both too dark and too saturated, which is what a
    // missing gamma encode looks like from the outside. Follows Xenia's
    // SpirvShaderTranslator::LinearToPWLGamma (BSD-3). Alpha is not
    // converted, and neither is anything drawn into a target that is not
    // the gamma form.
    pub(super) fn emit_gamma(&mut self) {
        let (float, float4, bool_) = (self.float, self.float4, self.bool_);
        let _uint32 = self.m.int(false);
        let flag = self.record_word(bindings::RECORD_PIXEL + 3);
        let zero_word = self.m.constant_u(0);
        let wanted = self.emit(op::I_NOT_EQUAL, bool_, &[flag, zero_word]);
        let float3 = self.m.float3();
        let bool_type = self.m.bool();
        let bool3 = self.m.vector(bool_type, 3);
        let wanted3 = self.emit(op::COMPOSITE_CONSTRUCT, bool3, &[wanted, wanted, wanted]);
        let splat = |t: &mut Self, v: f32| -> Id {
            let c = t.m.constant_f(v);
            t.emit(op::COMPOSITE_CONSTRUCT, float3, &[c, c, c])
        };
        for colour in self.colours {
            if colour == 0 {
                continue;
            }
            let value = self.emit(op::LOAD, float4, &[colour]);
            let rgb = self.emit(op::VECTOR_SHUFFLE, float3, &[value, value, 0, 1, 2]);
            // Saturate first; NClamp puts a NaN at zero rather than leaving
            // it to the driver, which is how the alpha test got caught out.
            let zero3 = splat(self, 0.0);
            let one3 = splat(self, 1.0);
            let linear = self.glsl450(glsl::N_CLAMP, float3, &[rgb, zero3, one3]);
            let at_least = |t: &mut Self, edge: f32| -> Id {
                let e = splat(t, edge);
                t.emit(op::F_ORD_GREATER_THAN_EQUAL, bool3, &[linear, e])
            };
            let high = at_least(self, 512.0f32 / 1023.0f32);
            let mid = at_least(self, 128.0f32 / 1023.0f32);
            let low = at_least(self, 64.0f32 / 1023.0f32);
            let a = splat(self, 1023.0f32 / 8.0f32);
            let b = splat(self, 1023.0f32 / 4.0f32);
            let scale_high = self.emit(op::SELECT, float3, &[high, a, b]);
            let a = splat(self, 128.0f32 / 255.0f32);
            let b = splat(self, 64.0f32 / 255.0f32);
            let offset_high = self.emit(op::SELECT, float3, &[high, a, b]);
            let a = splat(self, 1023.0f32 / 2.0f32);
            let b = splat(self, 1023.0f32);
            let scale_low = self.emit(op::SELECT, float3, &[low, a, b]);
            let a = splat(self, 32.0f32 / 255.0f32);
            let b = splat(self, 0.0f32);
            let offset_low = self.emit(op::SELECT, float3, &[low, a, b]);
            let scale_of = self.emit(op::SELECT, float3, &[mid, scale_high, scale_low]);
            let offset_of = self.emit(op::SELECT, float3, &[mid, offset_high, offset_low]);
            let product = self.emit(op::F_MUL, float3, &[linear, scale_of]);
            let stepped = self.glsl450(glsl::TRUNC, float3, &[product]);
            let step = self.m.constant_f(1.0f32 / 255.0f32);
            let steps = self.emit(op::VECTOR_TIMES_SCALAR, float3, &[stepped, step]);
            let encoded = self.emit(op::F_ADD, float3, &[steps, offset_of]);
            let chosen = self.emit(op::SELECT, float3, &[wanted3, encoded, rgb]);
            let alpha = self.emit(op::COMPOSITE_EXTRACT, float, &[value, 3]);
            let r = self.emit(op::COMPOSITE_EXTRACT, float, &[chosen, 0]);
            let g = self.emit(op::COMPOSITE_EXTRACT, float, &[chosen, 1]);
            let b = self.emit(op::COMPOSITE_EXTRACT, float, &[chosen, 2]);
            let out = self.emit(op::COMPOSITE_CONSTRUCT, float4, &[r, g, b, alpha]);
            self.code(op::STORE, &[colour, out]);
        }
    }

    // Window coordinates to clip space, from the transform in the draw's
    // record: xy = xy * scale + offset * w. The identity when the viewport
    // transform is on, which is every draw but the EDRAM clears.
    pub(super) fn emit_window_to_clip(&mut self) {
        let (float, float4) = (self.float, self.float4);
        let sx = self.record_float(bindings::RECORD_NDC);
        let sy = self.record_float(bindings::RECORD_NDC + 1);
        let ox = self.record_float(bindings::RECORD_NDC + 2);
        let oy = self.record_float(bindings::RECORD_NDC + 3);
        let ndc = self.build_vector(sx, sy, ox, oy);
        let position = self.position;
        let value = self.emit(op::LOAD, float4, &[position]);
        let w = self.emit(op::COMPOSITE_EXTRACT, float, &[value, 3]);
        let term = |t: &mut Self, component: u32| -> Id {
            let v = t.emit(op::COMPOSITE_EXTRACT, float, &[value, component]);
            let scale = t.emit(op::COMPOSITE_EXTRACT, float, &[ndc, component]);
            let offset = t.emit(op::COMPOSITE_EXTRACT, float, &[ndc, component + 2]);
            let offset_w = t.emit(op::F_MUL, float, &[offset, w]);
            t.glsl450(glsl::FMA, float, &[v, scale, offset_w])
        };
        let x = term(self, 0);
        let y = term(self, 1);
        let z = self.emit(op::COMPOSITE_EXTRACT, float, &[value, 2]);
        let out = self.emit(op::COMPOSITE_CONSTRUCT, float4, &[x, y, z, w]);
        self.code(op::STORE, &[position, out]);
    }
}
