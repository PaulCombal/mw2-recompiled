# Shaders

MW2's shaders are Xenos microcode produced by Microsoft's offline compiler. The
title compiles nothing at run time: it hands the GPU microcode through the
command stream, so the runtime translates each program to SPIR-V as it is
loaded. When shaders are compiled and cached is covered in
[rendering.md](rendering.md); this page covers the microcode and the
translation.

| Piece | Where |
|---|---|
| Microcode field layouts | `runtime/gpu/vulkan/xenos_ucode.h` |
| Translator | `runtime/gpu/vulkan/shader_translator.{h,cpp}` |
| SPIR-V binary writer | `runtime/gpu/vulkan/spirv.{h,cpp}` |
| Resource interface | `runtime/gpu/vulkan/bindings.h` |
| Offline translator | `tools/translate_shader.cpp` (target `translate-shader`) |
| Disassembler | `tools/xenos_shader.py` (control flow), `tools/xenos_isa.py` (ALU and fetch) |
| Fastfile extractor | `tools/extract_shaders.py` |

## How shaders reach the GPU

The command processor (`runtime/gpu/command_processor.cpp`) loads a program
from two packets:

| Packet | Payload |
|---|---|
| `IM_LOAD_IMMEDIATE` (0x2B) | `{ type, start << 16 \| size, microcode... }`, type 0 vertex, 1 pixel |
| `IM_LOAD` (0x27) | `{ physical address \| type, start << 16 \| size }`; the microcode is in guest memory |

Most of the title's shaders arrive by `IM_LOAD`. `IM_LOAD_IMMEDIATE` states its
length twice (header count and the `size` field); a packet whose two lengths
disagree is a misparse of the command stream and is refused, because its
payload is command buffer rather than microcode.

A shader is identified by a hash of its microcode. An `IM_LOAD`'s microcode and
hash are kept by address, length and stage while the memory watch
(`gpu/memory_watch.h`) says nothing has written their pages, so a frame's
thousands of loads of a few hundred shaders read and hash each one once. With diagnostics compiled in
(the default; `-DMW2_DIAGNOSTICS=OFF` removes them, see
[building.md](building.md)), `MW2_DUMP_SHADERS=<dir>` writes each distinct
program as
`vertex_<hash>.ucode` / `pixel_<hash>.ucode`, big-endian as the guest holds it
(see [switches.md](switches.md)).

## Shaders in the fastfiles

`tools/extract_shaders.py` recovers the programs offline from the fastfiles,
for testing the translator against the whole corpus without running the game.

- A fastfile is `IWffu100` followed by one zlib stream.
- Compiled shaders sit in Microsoft's shader containers, tagged `0x102A1100`
  (pixel) and `0x102A1101` (vertex). The constant table inside confirms which.
- The zone is packed without alignment, so tags must be searched at every byte
  offset, not every dword.
- The container holds the constant table, debug record and name, not the
  program. D3D9's shader constructor `sub_820B8B90` copies `container[+0x04]`
  bytes of container, then `container[+0x08]` bytes of microcode from past the
  container. The microcode is stored raw.
- Between container and program lies the rest of the IW4 asset (pointer
  placeholders, the name string), of variable length. The extractor searches
  forward for a blob of exactly the declared length that decodes as a program
  *and* whose every full vertex fetch names a defined vertex format. All three
  conditions are required: control flow alone decodes from arbitrary data,
  since every 4-bit control-flow opcode is defined.

The `*packed` suffix in a container's debug name (`trivial_vertcol_simple.hlsl*packed`)
is part of the shader's name, not a storage format.

## Microcode encoding

**Control flow.** A program starts with a list of 48-bit control-flow
instructions packed two per three dwords: the first takes `dword0` and the low
half of `dword1`, the second the high half of `dword1` and `dword2`. The opcode
is bits 12..15 of each half.

| Opcode | Name | Opcode | Name |
|---|---|---|---|
| 0 | nop | 8 | loop_end |
| 1 | exec | 9 | cond_call |
| 2 | exec_end | 10 | return |
| 3 | cond_exec | 11 | cond_jmp |
| 4 | cond_exec_end | 12 | alloc |
| 5 | cond_exec_pred | 13 | cond_exec_pred_clean |
| 6 | cond_exec_pred_end | 14 | cond_exec_pred_clean_end |
| 7 | loop_start | 15 | mark_vs_fetch_done |

An exec's address is an *instruction* index; instructions are three dwords, so
instruction N starts at dword 3N.

**Instructions.** Each is three dwords, either an ALU pair (vector + scalar) or
a fetch. Nothing in the instruction says which: the exec's *sequence* word
carries two bits per instruction, bit 0 meaning fetch. A program therefore
cannot be decoded without walking its control flow first.

**Fetch opcodes** (`w0` bits 0..4): 0 `vfetch`, 1 `tfetch`, 16
`get_border_colour_frac`, 17 `get_computed_lod`, 18 `get_gradients`, 19
`get_weights`, 24 `set_lod`, 25 `set_gradients_h`, 26 `set_gradients_v`. Bit 19 of a vertex fetch
(`must_be_one`) is set on every real one; a fetch with it clear is not a vertex
fetch and is refused.

**Swizzles are component-relative.** Each two-bit source swizzle field is added
to its own position, so the identity swizzle is `0b11100100`, not zero.

**Relative constant addressing** has two independent parts. Bit 29 of `w1`
chooses the addressing register (a0 rather than aL) and says nothing about
whether addressing happens. Whether a source is addressed comes from
`const_0_rel_abs` (w1 bit 31) and `const_1_rel_abs` (w1 bit 30), selected by how
many earlier operands were constants, because the hardware carries two bits for
three possible constant operands (`Alu::SourceConstantIsAddressed`).

## The module interface

Every translated shader is built against one fixed interface
(`runtime/gpu/vulkan/bindings.h`), so a single pipeline layout serves every
shader and nothing is reflected at bind time.

```
set 0  nothing
set 1  binding 0  texture2D[65536]    every texture the cache holds, at its id + 2,
                  textureCube[65536]  declared once for each kind a shader samples:
                  texture3D[65536]    one array, each element read only as its kind
       binding 1  sampler[4096]       every sampler state
       elements 0, 1 and 2 of the image array are 1x1 stand-ins: 2D, cube, volume
set 2  binding 0  uint[]   the frame arena, as dwords
       binding 1  vec4[]   the same, as vec4
no push constants
the draw's record, in the arena at the dword the draw passes as its base vertex
(vertexOffset, or firstVertex for a draw without indices):
  0   (5 words)  where each constant block is in the arena, in dwords:
                   0  the vertex constant table
                   1  vertex fetch constants (96 pairs of dwords, each padded to four)
                   2  256 boolean constants, packed bits (8 dwords)
                   3  32 loop constants
                   4  pixel float constants, 256 vec4 from SQ_PS_CONST's base
  5   (4)        vertex: float2 scale, float2 offset, window to clip space
  9   (4)        pixel: alpha function, alpha reference, colour scale, gamma-target flag
  13  (4)        both: a bit per texture slot bound to a resolve's copy at the frame's
                 scale, the same at the shadow maps' scale, and the reciprocal of each
                 scale; read only by shaders built under MW2_SCALE or MW2_SHADOW_SCALE
  17  (1 a slot) pixel: per fetch slot up to the highest it samples, the image's
                 index (low 16 bits) and the sampler's (high 16)
  49  (1 a slot) vertex: the same for its own slots, only when it samples any; a
                 stage's own because the two may declare a slot as different kinds
```

- **Finding the record.** The vertex shader reads `BaseVertex`, the same for
  the whole draw, which the GPU keeps in one scalar register, and takes it
  away from `VertexIndex` before `r0.x` is made from it. It passes the value
  to the pixel shader as a flat output at location 16, after the sixteen
  interpolators. The base vertex rather than the first instance, because a run
  of draws made with one `vkCmdDrawMultiIndexedEXT` shares its first instance
  but not its vertex offsets. The compiler cannot know a flat input is the same
  in every pixel, so the pixel shader takes it from the subgroup's first
  invocation (`OpGroupNonUniformBroadcastFirst`). Without that, every
  constant read in a pixel shader would be a load per pixel. A subgroup never
  spans two draws: a texture index without `NonUniform` already relies on
  that.
- **Each record word is loaded once**, where a driver would sink the load:
  in the narrowest block that dominates every read of it, ahead of the first
  (`RecordWord`, `PlaceRecordLoads`). A straight-line program is one block,
  so a word is loaded at its first read. In a program with the dispatcher a
  word read in one case is loaded in that case, and one read in several at
  the top of the iteration, in the block that switches. A driver merges two
  equal loads only where one dominates the other, which a straight-line
  program's repeated loads already were; the dispatcher's cases each loaded
  the words again. Loading every word in the entry block instead leaves the
  placement to the driver: RADV sinks a load to where its reads meet but not
  into a loop, so the words stayed live across the dispatcher's loop and it
  spilled scalar registers in 1,089 pixel shaders (356 unscaled), and a
  driver that does not sink would keep every word live across every program.
  On RADV (Renoir), over the game's 15,044 shaders, the 1,688 pixel shaders
  with the dispatcher come out with 6% fewer instructions and 17-20% fewer
  scalar loads; 77 of them spill a few scalar registers more than before
  (2 unscaled), with no fewer waves; the straight-line pixel shaders and the
  vertex shaders compile to the same code as before. Building all of them
  takes 4-6% fewer CPU instructions. The GPU time of a frame moves by under
  1% (S.S.D.D.: 720p -1.0%, 1080p and three times the title's size -0.4%),
  at the edge of the run-to-run spread.
- **Why no push constants.** RADV places push constants per draw by reading
  each bound shader's description, a cache miss on every draw, which was a
  third of the recorder thread's time; binding a texture set per draw was
  another tenth. Both are gone: the two sets are bound once a command buffer,
  and a draw is a pipeline when it changes, the dynamic state that changed,
  and the draw.
- **Modules are SPIR-V 1.3**, the first version with `BaseVertex` and the
  subgroup operations in the core. Before 1.4 an entry point lists only its
  Input and Output variables.

- Every constant is in the frame arena, copied there from the register file,
  and found by the offset the draw's record holds for its block. Binding five
  dynamic uniform blocks at every draw, as the runtime once did, cost a
  descriptor upload each time, and nearly every draw moves one of them.
- The hardware constant file holds 512 vec4; `SQ_VS_CONST` and `SQ_PS_CONST`
  give each stage a window into it (D3D9 gives vertex the low half, pixel the
  high half).
- A pixel program reads its window as one run of 256 vec4. A vertex program
  reads through the constant table: a uvec4 whose x is the window's base, then
  for each of the file's 32 blocks of 16 vec4 the vec4 index of its copy in
  the arena. Constant `i` is `arena[table[4 + (base + i) / 16] + (base + i) % 16]`.
  A draw changes a block or so of the ten or so a vertex program reads (its
  world matrix), and the table lets it copy only the blocks that changed.
- The translator records the highest float constant a program reads
  (`Translation::constantsRead`) so the renderer copies only that much, unless
  the program addresses constants through a0/aL (`addressedConstants`), where
  the whole window must go across.
- A vertex shader writes `gl_Position` and all sixteen interpolators to
  locations 0..15 (zero until written), because a pixel-shader input with no
  vertex output behind it is invalid. A pixel shader reads the interpolators it
  uses and writes only the colour targets it exports (locations 0..3). The
  ones it uses are the low temporaries any instruction reads, a texture
  fetch's coordinates included: some are read by nothing else.
- Set 1 declares a slot as 2D, cube or 3D according to how the program fetches
  it (`Translation::textureKinds`); 1D fetches are sampled as 2D at v = 0.
  The three kinds' variables share binding 0, which Vulkan allows for
  variables whose types the binding's descriptor type admits: a sampled
  image's descriptor is the same whatever its view's kind, and ids are shared
  across kinds, so one array of 65536 serves them all where one of each kind
  would leave two thirds of the descriptors unreachable.

## Translation

### Control flow

Xenos jumps, calls and loops move the sequencer's program counter arbitrarily.
Rather than recover structured control flow, a program that has any of them
runs one loop whose body is a switch on the counter, one case per control-flow
instruction:

```
loop { switch (pc) { case 0: ...; pc = 1; ... default: break out; } }
```

Every case leaves `pc` set. Falling off the end selects the default, which
breaks out of the loop. All sixteen control-flow opcodes are handled this way.

SPIR-V structured-control-flow rules require two extra blocks: the switch's
merge block may not be the loop's continue target, and the default target may
not be the loop's merge block, so each gets a block of its own that only
branches.

A program with no jumps, calls or loops (`cond_jmp`, `cond_call`, `return`,
`loop_start`, `loop_end`) gets no loop and no switch: its counter can only step
through the instructions in order up to the first `*_end`, where the decoded
list ends, so they are emitted one after the other. That is nine in ten of the
game's pixel shaders and every vertex shader. The driver does not remove the
loop itself — RADV keeps the switch, and every register live across it — so
the straight-line form compiles to fewer registers and instructions and more
waves per SIMD, and compiles faster.

Sequencer state — pc, aL, loop and call stacks, the predicate, the address a
full vertex fetch computed for the mini fetches after it, the previous scalar
result for the `*_prev` opcodes, the `set_lod` value — lives in function
variables, because producer and consumer can be in different cases. Temporaries
are one array of 64 `vec4` function variables (an array because a destination
can be aL-relative). Drivers promote these to registers, so the translator
never builds SSA form.

Loops follow the hardware: `aL = iterator * step + start` from the innermost
loop constant (step and start are signed 8-bit), clamped to [-256, 256].

### Predication

Predication is not branching: a predicated instruction executes and only its
write is suppressed. Every store is a select under the condition in force —
the instruction's own predicate, a `cond_exec_pred*` block's predicate, or a
`cond_exec` boolean constant (uniform for the draw). The same applies to fetch
results and `set_lod`.

The `setp` family writes the predicate and a result together: the predicate
takes the comparison against zero, the destination 0 where it holds and 1 where
not (`setp_rstr` keeps the source, `setp_clr` writes FLT_MAX). Scalar
comparisons test one component against zero.

Kills are deferred: the discard is recorded and performed after the sequencer
finishes, because `OpKill` would terminate a block inside the dispatch switch.
A straight-line program defers them too, so both forms behave alike. The
difference is observable only to a shader that exports to memory after
killing.

### Exports

An export writes one register from both halves of the ALU pair under
complementary masks. Components neither half writes are still defined by the
encoding: literal 0 or literal 1 where the masks say so. So

```
max export62.w, r1, r1 + retain_prev export62.w
```

is `gl_Position = (0, 0, 0, 1)`, not a move. `max rN, rN` is the compiler's
move.

| Export | Handling |
|---|---|
| Vertex 0..15 | interpolators |
| Vertex 62 | position |
| Vertex point size | dropped; point size is fixed at 1 |
| Vertex eM0..eM5 (memory export) | dropped and counted (`droppedMemoryExports`) |
| Pixel 0..3 | colour targets |
| Pixel depth | translation fails |
| Pixel, anything else | dropped and counted |

Memory export has nowhere to land: set 2 is the frame arena, holding copies of
the vertex data each draw reads, not guest memory. The geometry such a program
draws is still drawn. The one memory export MW2 depends on, the image pool's
block copy, is performed on the CPU at that draw (see
[textures.md](textures.md#the-image-pools-block-copy)).

### Vertex fetch

Xenos has no vertex input stage. The sequencer writes the vertex index into
`r0.x` as a float, and the program computes its own fetch address, so the
pipeline declares no vertex input state and the shader does the same
computation:

- `r0.x` is `gl_VertexIndex - gl_BaseVertex`, converted to float at entry.
- A full `vfetch` reads its fetch constant from binding 1: dword 0 is the
  buffer address in dwords with a two-bit type below it (`>> 2`), dword 1's low
  two bits are the endianness. The address is
  `(vf.x >> 2) + index * stride + offset`, read from the arena (set 2). The
  renderer copies what the draw reads into the arena and rewrites the constant
  to point at the copy ([rendering.md](rendering.md)).
- Endianness is a run-time value (none, 8-in-16, 8-in-32, 16-in-32); all four
  swaps are computed and selected between. Vertex data is not always 8-in-32:
  a mesh with 16-bit positions is 8-in-16.
- A mini fetch has no index or fetch constant: it reuses the address of the
  preceding full fetch with its own offset and format. A full fetch whose
  destination selects nothing exists only to compute that address.
- The instruction's exponent adjust scales the result by a power of two.

Vertex formats decoded (component 0 in the least significant bits; packed
formats honour the signed and integer flags, normalising to [-1, 1] or [0, 1]
otherwise):

| Id | Format | Id | Format |
|---|---|---|---|
| 6 | 8_8_8_8 | 33 | 32 |
| 7 | 2_10_10_10 | 34 | 32_32 |
| 16 | 10_11_11 | 35 | 32_32_32_32 |
| 17 | 11_11_10 | 36 | 32_FLOAT |
| 25 | 16_16 | 37 | 32_32_FLOAT |
| 26 | 16_16_16_16 | 38 | 32_32_32_32_FLOAT |
| 31 | 16_16_FLOAT | 57 | 32_32_32_FLOAT |
| 32 | 16_16_16_16_FLOAT | | |

Format 0 is undefined; a fetch naming it or any other unlisted format fails
translation.

### Texture fetch

`tfetch` names a slot; everything about the texture comes from the fetch
constant ([textures.md](textures.md)). The translator:

- takes the coordinate components the fetch names from the source register
  (u for 1D, u/v for 2D, all three for 3D and cube);
- applies the fetch's half-texel offsets, plus a 1.5/1024-texel nudge on every
  axis (Xenia's value) so a point sample on a texel boundary lands where the
  console's fixed-point coordinates (8 fractional bits) put it. Offsets and
  unnormalised coordinates are in texels of the bound image's size, which
  under `MW2_SCALE` or `MW2_SHADOW_SCALE` is divided by the copy's scale for
  a slot bound to a resolve's copy ([rendering.md](rendering.md#resolution-scale));
- computes LOD from the fetch constant's LOD bias (dword 4 bits 12..21, signed,
  in 32nds, read from binding 1 at run time), the instruction's own bias (in
  16ths) and, when `use_register_lod` is set, the `set_lod` value;
- samples with implicit LOD plus bias in a pixel shader, explicit LOD in a
  vertex shader (which has no derivatives; `get_gradients` returns zero there);
- scales the result by the fetch constant's exponent adjust (dword 3 bits
  13..18, signed).

**Cube maps** sample a real cube. The `cube` ALU instruction produces
(T, S, 2·major axis, face); the fetch turns the face and the two coordinates
(in 1..2 after the 1.5 the title adds) back into a direction. Sampling a
direction rather than six array layers keeps the LOD continuous across face
edges. The `cube` operand arrives swizzled `.zzxy`/`.yxzz`; the hardware reads
XYZ from the first operand only.

`get_border_colour_frac` returns zero. `get_computed_lod`, `get_weights`,
`set_gradients_h` and `set_gradients_v` are not translated (the fetch opcode fails);
the instruction's filter overrides and register gradients are not applied, and
the sampler comes from the fetch constant alone.

### Fixed-function state in the shader

- **Alpha test.** The console's alpha test is fixed-function and Vulkan has
  none, so the pixel shader compares against the function and reference in
  the draw's record. The compare-function bits (001 less, 010 equal, 100
  greater) are a mask over the three comparison results.
- **Colour exponent bias.** The console scales a pixel shader's output by
  2^`RB_COLOR_INFO` exponent bias on the way into EDRAM; the title renders the
  world with a bias. Applied after the alpha test, which runs on the unbiased
  alpha.
- **Gamma targets.** Output to an `8_8_8_8_GAMMA` target goes through Xenos'
  piecewise-linear gamma curve (Xenia's `LinearToPWLGamma`); alpha is not
  converted.
- **Pre-transformed vertices.** With the viewport transform off, a program
  emits window coordinates (D3D9 XYZRHW); Vulkan would clip those away. The
  vertex shader applies `xy = xy * scale + offset * w` from the draw's record —
  the identity when the viewport transform is on, `2 * window / size - 1`
  otherwise. MW2's EDRAM clears depend on this.

## Validating a translation

```
cmake --build build --target translate-shader
./build/translate-shader [--compile] vertex_<hash>.ucode out.spv
spirv-val --target-env vulkan1.0 out.spv
```

The shader type comes from the file name (`pixel`/`ps_` means pixel). Run both
checks: `spirv-val` checks the binary against the specification, `--compile`
hands it to the real driver against the actual pipeline layout
(`runtime/gpu/vulkan/pipeline.cpp`). A driver accepting a module is not
evidence that it is valid — drivers accept, for example, duplicate
`OpTypeImage` declarations and implicit-LOD sampling in a vertex shader, both
invalid SPIR-V.

At run time, a program that fails translation or that the driver refuses is
logged (`renderer: ... shader ... not usable`) and draws using it are skipped.
