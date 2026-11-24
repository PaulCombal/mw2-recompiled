// The fetch half of the translator: vertex fetches through the frame arena,
// texture fetches through set 1, and the fetch unit's other operations.

use super::bindings;
use super::spirv::{Id, glsl, op, storage};
use super::translator::{CAPABILITY_IMAGE_QUERY, IMAGE_OPERAND_BIAS, IMAGE_OPERAND_LOD, Translator};
use super::ucode::{Fetch, TEMP_REGISTER_COUNT, dimension, fetch_op};

// 2^exponent, as std::ldexp(1.0f, exponent) gives it for the exponents a
// fetch can carry (-32..31), all of them normal floats: built from the bits.
fn power_of_two(exponent: i32) -> f32 {
    f32::from_bits(((exponent + 127) as u32) << 23)
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Packed,
    Float32,
    Float16,
}

// Each format names, per component, its width, its bit offset and which word
// of the vertex it lives in. Component 0 is in the *least* significant bits.
#[derive(Clone, Copy, Default)]
struct Layout {
    count: u32,
    width: [u32; 4],
    offset: [u32; 4],
    word: [u32; 4],
}

const fn layout(count: u32, width: [u32; 4], offset: [u32; 4], word: [u32; 4]) -> Layout {
    Layout { count, width, offset, word }
}

impl Translator<'_> {
    // Dword `word` (0 or 1) of fetch constant pair `pair`: the fetch window is
    // 96 pairs of dwords, each padded to four. Vertex fetches read their
    // address and endianness from it, texture fetches their LOD bias and
    // exponent adjust. A vertex fetch reads a buffer the title described in a
    // fetch constant, at an address the shader computes itself: the renderer
    // has copied what the draw reaches into the arena and pointed the fetch
    // constants at the copies.
    fn fetch_constant_word(&mut self, pair: u32, word: u32) -> Id {
        self.result.meta.fetch_pairs_read[(pair / 32) as usize] |= 1 << (pair % 32);
        let uint = self.uint;
        let block = self.constant_block(bindings::FETCH_CONSTANTS);
        let offset = self.m.constant_u(pair * 4 + word);
        let at = self.emit(op::I_ADD, uint, &[block, offset]);
        self.arena_word(at)
    }

    // The console stores everything big-endian, and the fetch constant says how:
    // none, 8-in-16, 8-in-32 or 16-in-32. Which one is a run-time value, so all
    // four are computed and selected between, exactly as the hardware would.
    fn read_guest_dword(&mut self, address_in_dwords: Id, index: u32) -> Id {
        let uint = self.uint;
        let mut at = address_in_dwords;
        if index != 0 {
            let offset = self.m.constant_u(index);
            at = self.emit(op::I_ADD, uint, &[at, offset]);
        }

        let pointer = self.m.pointer(storage::UNIFORM, uint);
        let zero = self.m.constant_s(0);
        let arena = self.arena;
        let chain = self.emit(op::ACCESS_CHAIN, pointer, &[arena, zero, at]);
        let raw = self.emit(op::LOAD, uint, &[chain]);

        // Bytes exchanged within each half.
        let low_bytes = self.m.constant_u(0x00FF_00FF);
        let low = self.emit(op::BITWISE_AND, uint, &[raw, low_bytes]);
        let eight = self.m.constant_u(8);
        let raised = self.emit(op::SHIFT_LEFT_LOGICAL, uint, &[low, eight]);
        let eight = self.m.constant_u(8);
        let lowered = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[raw, eight]);
        let low_bytes = self.m.constant_u(0x00FF_00FF);
        let high = self.emit(op::BITWISE_AND, uint, &[lowered, low_bytes]);
        let in16 = self.emit(op::BITWISE_OR, uint, &[raised, high]);
        let halves = |t: &mut Self, v: Id| -> Id {
            let sixteen = t.m.constant_u(16);
            let down = t.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[v, sixteen]);
            let sixteen = t.m.constant_u(16);
            let up = t.emit(op::SHIFT_LEFT_LOGICAL, uint, &[v, sixteen]);
            t.emit(op::BITWISE_OR, uint, &[down, up])
        };
        let in32 = halves(self, in16); // 8-in-16 then halves swapped is a full reversal
        let half32 = halves(self, raw);

        let last_fetch_endian = self.last_fetch_endian;
        let endian = self.emit(op::LOAD, uint, &[last_fetch_endian]);
        let is = |t: &mut Self, value: u32| -> Id {
            let bool_ = t.m.bool();
            let constant = t.m.constant_u(value);
            t.emit(op::I_EQUAL, bool_, &[endian, constant])
        };
        let is3 = is(self, 3);
        let mut result = self.emit(op::SELECT, uint, &[is3, half32, raw]);
        let is2 = is(self, 2);
        result = self.emit(op::SELECT, uint, &[is2, in32, result]);
        let is1 = is(self, 1);
        result = self.emit(op::SELECT, uint, &[is1, in16, result]);
        result
    }

    // SPIR-V has no arithmetic right shift guaranteed on an unsigned type, so a
    // signed field is extracted unsigned and folded down by 2^width when its top
    // bit is set -- which is what sign extension is.
    fn unpack_field(&mut self, word: Id, offset: u32, width: u32, is_signed: bool, is_integer: bool) -> Id {
        let (uint, int, float) = (self.uint, self.int, self.float);
        let shifted = if offset != 0 {
            let by = self.m.constant_u(offset);
            self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[word, by])
        } else {
            word
        };
        let field = if width == 32 {
            shifted
        } else {
            let mask = self.m.constant_u((1u32 << width) - 1);
            self.emit(op::BITWISE_AND, uint, &[shifted, mask])
        };

        let mut value;
        if is_signed {
            let mut as_int = self.emit(op::BITCAST, int, &[field]);
            if width < 32 {
                let top = self.m.constant_u(1u32 << (width - 1));
                let sign_bit = self.emit(op::BITWISE_AND, uint, &[field, top]);
                let bool_ = self.m.bool();
                let zero = self.m.constant_u(0);
                let negative = self.emit(op::I_NOT_EQUAL, bool_, &[sign_bit, zero]);
                let range = self.m.constant_s((1u32 << width) as i32);
                let folded = self.emit(op::I_SUB, int, &[as_int, range]);
                as_int = self.emit(op::SELECT, int, &[negative, folded, as_int]);
            }
            value = self.emit(op::CONVERT_S_TO_F, float, &[as_int]);
        } else {
            value = self.emit(op::CONVERT_U_TO_F, float, &[field]);
        }
        if !is_integer {
            // Normalised formats scale to [-1, 1] or [0, 1] by the largest value the
            // field can hold.
            let maximum = if is_signed { ((1u64 << (width - 1)) - 1) as f64 } else { ((1u64 << width) - 1) as f64 };
            let scale = self.m.constant_f((1.0 / maximum) as f32);
            value = self.emit(op::F_MUL, float, &[value, scale]);
        }
        value
    }

    // Component 0 sits in the least significant bits of a packed word.
    fn decode_vertex_format(&mut self, fetch: &Fetch, address: Id, out: &mut [Id; 4], count: &mut u32) -> bool {
        let format = fetch.vertex_format();
        let is_signed = fetch.signed_components();
        let is_integer = fetch.integer_components();

        let (layout, kind) = match format {
            6 => (layout(4, [8, 8, 8, 8], [0, 8, 16, 24], [0, 0, 0, 0]), Kind::Packed), // 8_8_8_8
            7 => (layout(4, [10, 10, 10, 2], [0, 10, 20, 30], [0, 0, 0, 0]), Kind::Packed), // 2_10_10_10
            16 => (layout(3, [11, 11, 10, 0], [0, 11, 22, 0], [0, 0, 0, 0]), Kind::Packed), // 10_11_11
            17 => (layout(3, [10, 11, 11, 0], [0, 10, 21, 0], [0, 0, 0, 0]), Kind::Packed), // 11_11_10
            25 => (layout(2, [16, 16, 0, 0], [0, 16, 0, 0], [0, 0, 0, 0]), Kind::Packed), // 16_16
            26 => (layout(4, [16, 16, 16, 16], [0, 16, 0, 16], [0, 0, 1, 1]), Kind::Packed), // 16_16_16_16
            33 => (layout(1, [32, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]), Kind::Packed), // 32
            34 => (layout(2, [32, 32, 0, 0], [0, 0, 0, 0], [0, 1, 0, 0]), Kind::Packed), // 32_32
            35 => (layout(4, [32, 32, 32, 32], [0, 0, 0, 0], [0, 1, 2, 3]), Kind::Packed), // 32_32_32_32
            31 => (layout(2, [16, 16, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]), Kind::Float16),
            32 => (layout(4, [16, 16, 16, 16], [0, 0, 0, 0], [0, 0, 1, 1]), Kind::Float16),
            36 => (layout(1, [32, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]), Kind::Float32),
            37 => (layout(2, [32, 32, 0, 0], [0, 0, 0, 0], [0, 1, 0, 0]), Kind::Float32),
            38 => (layout(4, [32, 32, 32, 32], [0, 0, 0, 0], [0, 1, 2, 3]), Kind::Float32),
            57 => (layout(3, [32, 32, 32, 0], [0, 0, 0, 0], [0, 1, 2, 0]), Kind::Float32),
            _ => {
                self.fail("vertex format", format);
                return false;
            }
        };
        *count = layout.count;
        let float = self.float;

        match kind {
            Kind::Float32 => {
                for (slot, &word) in out.iter_mut().zip(&layout.word).take(layout.count as usize) {
                    let value = self.read_guest_dword(address, word);
                    *slot = self.emit(op::BITCAST, float, &[value]);
                }
            }
            Kind::Float16 => {
                let mut w = 0u32;
                while w * 2 < layout.count {
                    let float2 = self.m.float2();
                    let word = self.read_guest_dword(address, w);
                    let pair = self.glsl450(glsl::UNPACK_HALF_2X16, float2, &[word]);
                    let mut c = 0u32;
                    while c < 2 && w * 2 + c < layout.count {
                        out[(w * 2 + c) as usize] = self.emit(op::COMPOSITE_EXTRACT, float, &[pair, c]);
                        c += 1;
                    }
                    w += 1;
                }
            }
            Kind::Packed => {
                for (i, slot) in out.iter_mut().enumerate().take(layout.count as usize) {
                    let word = self.read_guest_dword(address, layout.word[i]);
                    *slot = self.unpack_field(word, layout.offset[i], layout.width[i], is_signed, is_integer);
                }
            }
        }
        true
    }

    // The destination swizzle picks components, or literal 0 or 1, or leaves a
    // component of the destination register alone.
    fn write_fetch_result(&mut self, fetch: &Fetch, components: &[Id; 4], count: u32) {
        let index = self.m.constant_s((fetch.dest_register() & (TEMP_REGISTER_COUNT - 1)) as i32);
        let target = self.temp_pointer(index);
        let float4 = self.float4;
        let current = self.emit(op::LOAD, float4, &[target]);
        let mut result = [0 as Id; 4];
        for c in 0..4u32 {
            let select = fetch.dest_component(c);
            result[c as usize] = if select < 4 {
                if select < count { components[select as usize] } else { self.m.constant_f(0.0) }
            } else if select == 4 {
                self.m.constant_f(0.0)
            } else if select == 5 {
                self.m.constant_f(1.0)
            } else {
                self.component(current, c)
            };
        }
        // Through store_masked, so a predicated fetch gets the same suppression an
        // ALU instruction does.
        let value = self.build_vector(result[0], result[1], result[2], result[3]);
        self.store_masked(target, value, 0xF);
    }

    fn translate_vertex_fetch(&mut self, fetch: &Fetch) {
        // Bit 19 is set on every real vertex fetch -- the field is called must_be_one.
        // When it is clear the instruction is not one: either the exec's sequence word
        // pointed at an ALU instruction, or the program is not where it was thought.
        if !fetch.must_be_one() {
            self.fail("not a vertex fetch (must_be_one is clear)", 0);
            return;
        }
        let (uint, float) = (self.uint, self.float);

        // A mini fetch carries no vertex index or fetch constant: it reuses the
        // address the last full fetch computed and applies its own format.
        if !fetch.is_mini_fetch() {
            let constant = fetch.vertex_fetch_slot();
            if constant >= bindings::FETCH_SLOTS {
                self.fail("vertex fetch constant", constant);
                return;
            }

            let word0 = self.fetch_constant_word(constant, 0);
            // dword 0 is the address in dwords, with a two-bit type below it.
            let two = self.m.constant_u(2);
            let base = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[word0, two]);

            // dword 1 carries the endianness in its low two bits. Vertex data is
            // not always stored 8-in-32: a mesh with 16-bit positions is 8-in-16,
            // and swapping it as if it were whole dwords exchanges the halves of
            // every pair, which stretches triangles across the level.
            let word1 = self.fetch_constant_word(constant, 1);
            let three = self.m.constant_u(3);
            let endian = self.emit(op::BITWISE_AND, uint, &[word1, three]);
            let last_fetch_endian = self.last_fetch_endian;
            self.code(op::STORE, &[last_fetch_endian, endian]);

            // The vertex index arrives as a float, which is how the sequencer hands
            // it over.
            let source = self.load_temp(fetch.source_register());
            let index_float = self.component(source, fetch.source_component());
            let index = self.emit(op::CONVERT_F_TO_U, uint, &[index_float]);
            let stride = self.m.constant_u(fetch.stride());
            let offset = self.emit(op::I_MUL, uint, &[index, stride]);
            let address = self.emit(op::I_ADD, uint, &[base, offset]);
            let last_fetch_base = self.last_fetch_base;
            self.code(op::STORE, &[last_fetch_base, address]);
        }
        if !self.have_fetch_base {
            self.fail("mini vertex fetch with no preceding full fetch", 0);
            return;
        }

        let last_fetch_base = self.last_fetch_base;
        let mut address = self.emit(op::LOAD, uint, &[last_fetch_base]);
        if fetch.offset() != 0 {
            let offset = self.m.constant_u(fetch.offset() as u32);
            address = self.emit(op::I_ADD, uint, &[address, offset]);
        }

        // A full fetch whose destination swizzle selects nothing exists to compute the
        // address for the mini fetches after it, which carry no format -- which is why
        // decoding one here would fail on format 0.
        let mut reads_memory = false;
        let mut writes_anything = false;
        for c in 0..4 {
            let select = fetch.dest_component(c);
            if select < 4 {
                reads_memory = true;
            }
            if select < 6 {
                writes_anything = true;
            }
        }

        let mut components = [0 as Id; 4];
        let mut count = 0u32;
        if reads_memory && !self.decode_vertex_format(fetch, address, &mut components, &mut count) {
            return;
        }
        if !writes_anything {
            return;
        }

        let exponent = fetch.exponent_adjust();
        if exponent != 0 {
            let scale = self.m.constant_f(power_of_two(exponent));
            for component in components.iter_mut().take(count as usize) {
                *component = self.emit(op::F_MUL, float, &[*component, scale]);
            }
        }
        self.write_fetch_result(fetch, &components, count);
    }

    // The screen-space derivatives a shader uses to pick a mip level itself.
    fn translate_get_gradients(&mut self, fetch: &Fetch) {
        // A vertex shader has no screen-space derivatives, and the hardware has
        // none to give it either.
        if !self.pixel {
            let zero = self.m.constant_f(0.0);
            self.write_fetch_result(fetch, &[zero, zero, zero, zero], 4);
            return;
        }
        let float = self.float;
        let source = self.load_temp(fetch.source_register());
        let x = self.component(source, 0);
        let y = self.component(source, 1);
        let dxx = self.emit(op::DPDX, float, &[x]);
        let dxy = self.emit(op::DPDX, float, &[y]);
        let dyx = self.emit(op::DPDY, float, &[x]);
        let dyy = self.emit(op::DPDY, float, &[y]);
        self.write_fetch_result(fetch, &[dxx, dxy, dyx, dyy], 4);
    }

    // The level of detail a later fetch uses instead of, or on top of, the one
    // the derivatives give it. It outlives the instruction, so it lives in a
    // variable, and its write is suppressed by the predicate like any other.
    fn translate_set_lod(&mut self, fetch: &Fetch) {
        let float = self.float;
        let lod = self.lod;
        let source = self.load_temp(fetch.source_register());
        let mut value = self.component(source, fetch.texture_source_swizzle() & 3);
        let condition = self.condition;
        if condition != 0 {
            let current = self.emit(op::LOAD, float, &[lod]);
            value = self.emit(op::SELECT, float, &[condition, value, current]);
        }
        self.code(op::STORE, &[lod, value]);
    }

    // A dword of texture fetch constant `slot`, as the draw bound it. The
    // shader sees the fetch window as 96 pairs of dwords, each padded to a
    // vector, so dword `word` of the six is pair 3 * slot + word / 2.
    fn texture_fetch_word(&mut self, slot: u32, word: u32) -> Id {
        self.fetch_constant_word(slot * 3 + word / 2, word & 1)
    }

    // A signed bit field of a fetch constant dword, as a float.
    fn signed_field(&mut self, word: Id, offset: u32, count: u32) -> Id {
        let (int, float) = (self.int, self.float);
        let signed = self.emit(op::BITCAST, int, &[word]);
        let at = self.m.constant_u(offset);
        let width = self.m.constant_u(count);
        let field = self.emit(op::BIT_FIELD_S_EXTRACT, int, &[signed, at, width]);
        self.emit(op::CONVERT_S_TO_F, float, &[field])
    }

    // The direction a cube fetch samples, from the face and the two
    // coordinates the `cube` instruction produced -- its exact inverse. The
    // hardware takes the face and coordinates as they are; a Vulkan cube wants
    // the direction, and sampling one rather than six layers of an array is
    // what keeps the level choice right where the face changes, because a
    // direction's derivatives do not jump there. Follows Xenia (BSD-3).
    fn cube_direction(&mut self, sc: Id, tc: Id, face: Id) -> Id {
        let (float, uint, bool_) = (self.float, self.uint, self.bool_);
        // The coordinates arrive in 1..2, where the `cube` result plus the 1.5
        // the title adds leaves them.
        let remap = |t: &mut Self, v: Id| -> Id {
            let two = t.m.constant_f(2.0);
            let doubled = t.emit(op::F_MUL, float, &[v, two]);
            let minus_three = t.m.constant_f(-3.0);
            t.emit(op::F_ADD, float, &[doubled, minus_three])
        };
        let s = remap(self, sc);
        let t = remap(self, tc);
        let low = self.m.constant_f(0.0);
        let high = self.m.constant_f(5.0);
        let clamped = self.glsl450(glsl::N_CLAMP, float, &[face, low, high]);
        let index = self.emit(op::CONVERT_F_TO_U, uint, &[clamped]);
        let one = self.m.constant_u(1);
        let axis = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[index, one]);
        let odd = self.emit(op::BITWISE_AND, uint, &[index, one]);
        let zero = self.m.constant_u(0);
        let negative = self.emit(op::I_NOT_EQUAL, bool_, &[odd, zero]);
        let zero = self.m.constant_u(0);
        let x_major = self.emit(op::I_EQUAL, bool_, &[axis, zero]);
        let y_major = self.emit(op::I_EQUAL, bool_, &[axis, one]);
        let pick = |tr: &mut Self, cond: Id, yes: Id, no: Id| tr.emit(op::SELECT, float, &[cond, yes, no]);
        let negate = |tr: &mut Self, v: Id| tr.emit(op::F_NEGATE, float, &[v]);
        let minus_one = self.m.constant_f(-1.0);
        let plus_one = self.m.constant_f(1.0);
        let sign = pick(self, negative, minus_one, plus_one);
        // x = pick(xMajor, sign, pick(yMajor, s, pick(negative, -s, s)))
        let minus_s = negate(self, s);
        let x_inner = pick(self, negative, minus_s, s);
        let x_rest = pick(self, y_major, s, x_inner);
        let x = pick(self, x_major, sign, x_rest);
        // y = pick(xMajor, -t, pick(yMajor, sign, -t))
        let minus_t = negate(self, t);
        let minus_t2 = negate(self, t);
        let y_rest = pick(self, y_major, sign, minus_t2);
        let y = pick(self, x_major, minus_t, y_rest);
        // z = pick(xMajor, pick(negative, s, -s), pick(yMajor, pick(negative, -t, t), sign))
        let minus_s = negate(self, s);
        let z_x = pick(self, negative, s, minus_s);
        let minus_t = negate(self, t);
        let z_y = pick(self, negative, minus_t, t);
        let z_rest = pick(self, y_major, z_y, sign);
        let z = pick(self, x_major, z_x, z_rest);
        let float3 = self.m.float3();
        self.emit(op::COMPOSITE_CONSTRUCT, float3, &[x, y, z])
    }

    fn translate_texture_fetch(&mut self, fetch: &Fetch) {
        let slot = fetch.constant_index() & 31;
        if !self.textures[slot as usize] {
            self.fail("texture fetch constant", slot);
            return;
        }
        let (float, float4, uint) = (self.float, self.float4, self.uint);

        let source = self.load_temp(fetch.source_register());
        // The fetch names which component of the source register each coordinate
        // comes from. A colour ramp looked up once per channel is three fetches of
        // one 1D texture reading x, then y, then z.
        let swizzle = fetch.texture_source_swizzle();
        let pick = |t: &mut Self, i: u32| t.component(source, (swizzle >> (2 * i)) & 3);
        let dim = fetch.dimension();
        // The coordinates that address texels: u for 1D, u and v for 2D and for
        // a cube's face, all three for a volume. A cube's third is its face.
        let axes = if dim == dimension::D1 {
            1
        } else if dim == dimension::D3 {
            3
        } else {
            2
        };
        let c0 = pick(self, 0);
        let c1 = if dim == dimension::D1 { self.m.constant_f(0.0) } else { pick(self, 1) };
        let c2 = if axes == 3 || dim == dimension::CUBE { pick(self, 2) } else { 0 };
        let mut c = [c0, c1, c2];

        // The size of level 0, in texels: offsets are in half texels of it, and
        // unnormalised coordinates are texels of it.
        self.m.require_capability(CAPABILITY_IMAGE_QUERY);
        // The slot's image and sampler, by the indices the record holds for it.
        let first = if self.pixel { bindings::RECORD_TEXTURES } else { bindings::RECORD_VERTEX_TEXTURES };
        let handle = self.record_word(first + slot);
        let image_mask = self.m.constant_u((1 << bindings::HANDLE_IMAGE_BITS) - 1);
        let image_index = self.emit(op::BITWISE_AND, uint, &[handle, image_mask]);
        let image_shift = self.m.constant_u(bindings::HANDLE_IMAGE_BITS);
        let sampler_index = self.emit(op::SHIFT_RIGHT_LOGICAL, uint, &[handle, image_shift]);
        let kind = self.result.meta.texture_kinds[slot as usize] as usize;
        let image_type = self.texture_image_types[slot as usize];
        let image_pointer = self.m.pointer(storage::UNIFORM_CONSTANT, image_type);
        let image_array = self.image_arrays[kind];
        let image_element = self.emit(op::ACCESS_CHAIN, image_pointer, &[image_array, image_index]);
        let bare = self.emit(op::LOAD, image_type, &[image_element]);
        let sampler_type = self.m.sampler();
        let sampler_type_again = self.m.sampler();
        let sampler_pointer = self.m.pointer(storage::UNIFORM_CONSTANT, sampler_type_again);
        let samplers = self.samplers;
        let sampler_element = self.emit(op::ACCESS_CHAIN, sampler_pointer, &[samplers, sampler_index]);
        let sampler_object = self.emit(op::LOAD, sampler_type, &[sampler_element]);
        let sampled_type = self.texture_types[slot as usize];
        let sampler = self.emit(op::SAMPLED_IMAGE, sampled_type, &[bare, sampler_object]);
        let size_count = if dim == dimension::D3 { 3 } else { 2 };
        let size_type = if size_count == 3 { self.m.float3() } else { self.m.float2() };
        let query_type = self.m.int_vector(size_count);
        let level = self.m.constant_s(0);
        let query = self.emit(op::IMAGE_QUERY_SIZE_LOD, query_type, &[bare, level]);
        let mut size = self.emit(op::CONVERT_S_TO_F, size_type, &[query]);
        size = self.guest_size(size, slot, size_count);

        // Offsets are in half texels, and every axis gets a sliver more: the
        // hardware turns a coordinate into fixed point with 8 bits below the
        // texel, and a point sample exactly between two texels has to land on
        // the one the console's rounding picks, not wherever float arithmetic
        // does. The sliver is Xenia's, 1.5/1024 of a texel (BSD-3).
        const ROUNDING: f32 = 1.5f32 / 1024.0f32;
        let offsets = [fetch.offset_x(), fetch.offset_y(), fetch.offset_z()];
        for i in 0..axes {
            let offset = self.m.constant_f(offsets[i] as f32 * 0.5f32 + ROUNDING);
            let extent = self.component(size, i as u32);
            c[i] = if fetch.coordinates_unnormalised() {
                let moved = self.emit(op::F_ADD, float, &[c[i], offset]);
                self.emit(op::F_DIV, float, &[moved, extent])
            } else {
                let fraction = self.emit(op::F_DIV, float, &[offset, extent]);
                self.emit(op::F_ADD, float, &[c[i], fraction])
            };
        }

        let coordinates = if dim == dimension::CUBE {
            let face = if offsets[2] != 0 {
                let shift = self.m.constant_f(offsets[2] as f32 * 0.5f32);
                self.emit(op::F_ADD, float, &[c[2], shift])
            } else {
                c[2]
            };
            self.cube_direction(c[0], c[1], face)
        } else if axes == 3 {
            let float3 = self.m.float3();
            self.emit(op::COMPOSITE_CONSTRUCT, float3, &[c[0], c[1], c[2]])
        } else {
            let float2 = self.m.float2();
            self.emit(op::COMPOSITE_CONSTRUCT, float2, &[c[0], c[1]])
        };

        // The level of detail: the derivatives, the register setTexLOD wrote, or
        // both, moved by the fetch constant's bias (in 32nds) and the
        // instruction's (in 16ths). Implicit LOD needs derivatives, which only a
        // fragment shader has, so a vertex shader always states the level.
        let computed = fetch.use_computed_lod() && self.pixel;
        let word4 = self.texture_fetch_word(slot, 4);
        let bias = self.signed_field(word4, 12, 10);
        let thirty_second = self.m.constant_f(1.0f32 / 32.0f32);
        let mut lod = self.emit(op::F_MUL, float, &[bias, thirty_second]);
        if fetch.use_register_lod() {
            let lod_variable = self.lod;
            let register = self.emit(op::LOAD, float, &[lod_variable]);
            lod = self.emit(op::F_ADD, float, &[register, lod]);
        }
        if fetch.lod_bias() != 0 {
            let own = self.m.constant_f(fetch.lod_bias() as f32 / 16.0f32);
            lod = self.emit(op::F_ADD, float, &[lod, own]);
        }
        let sampled = if computed {
            self.emit(op::IMAGE_SAMPLE_IMPLICIT_LOD, float4, &[sampler, coordinates, IMAGE_OPERAND_BIAS, lod])
        } else {
            self.emit(op::IMAGE_SAMPLE_EXPLICIT_LOD, float4, &[sampler, coordinates, IMAGE_OPERAND_LOD, lod])
        };

        // The fetch constant's exponent adjust scales what comes back by a
        // power of two.
        let word3 = self.texture_fetch_word(slot, 3);
        let exponent = self.signed_field(word3, 13, 6);
        let scale = self.glsl450(glsl::EXP2, float, &[exponent]);
        let mut components = [0 as Id; 4];
        for (i, component) in components.iter_mut().enumerate() {
            let channel = self.component(sampled, i as u32);
            *component = self.emit(op::F_MUL, float, &[channel, scale]);
        }
        self.write_fetch_result(fetch, &components, 4);
    }

    pub(super) fn translate_fetch(&mut self, fetch: &Fetch) {
        // As with the ALU: the fetch happens, its write is suppressed.
        let outer = self.condition;
        self.condition = self.condition_for(fetch.is_predicated(), fetch.predicate_condition());

        match fetch.opcode() {
            fetch_op::VERTEX_FETCH => self.translate_vertex_fetch(fetch),
            fetch_op::TEXTURE_FETCH => self.translate_texture_fetch(fetch),
            fetch_op::GET_GRADIENTS => self.translate_get_gradients(fetch),
            fetch_op::SET_LOD => self.translate_set_lod(fetch),
            fetch_op::GET_BORDER_COLOUR_FRAC => {
                // How much of the sample fell outside the texture. No sampler here has a
                // border, so nothing ever does.
                let zero = self.m.constant_f(0.0);
                self.write_fetch_result(fetch, &[zero, zero, zero, zero], 4);
            }
            other => self.fail("fetch opcode", other),
        }
        self.condition = outer;
    }
}
