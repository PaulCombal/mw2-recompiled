# Rendering

This document covers how draws and resolves from the PM4 stream become Vulkan
work and reach the screen. Stream parsing is in [d3d9-seam.md](d3d9-seam.md),
shader translation in [shaders.md](shaders.md), and the texture cache in
[textures.md](textures.md). The code is in `src/runtime/gpu/vulkan/`, and
`mod.rs` lists the parts.

A draw packet carries only the primitive and the indices. Everything else
(shaders, constants, fetch constants, targets, blend and depth state) is
whatever the register file holds, so the renderer's `draw` takes the register
file plus a `DrawCall`.

The **ring consumer** does everything tied to its point in the stream. It reads
guest memory, chooses targets and pipelines, and fills the arena. It queues the
Vulkan calls to the **recorder thread**. A **window thread** presents. Worker
threads compile shaders and optimised pipelines.

## Render targets: an image per EDRAM surface

EDRAM is not emulated. MW2 neither aliases surfaces nor renders in tiles, so a
surface is a Vulkan image keyed by `TargetKey { baseTile, pitch, host format,
samples, depth }`. The key comes from `RB_COLOR_INFO` / `RB_DEPTH_INFO` and
`RB_SURFACE_INFO` (pitch in bits 0-13, samples in bits 16-17).

- **Depth keys have no pitch.** EDRAM has one depth buffer per base tile, and
  MW2 tests world depth in a pass that describes it at another pitch than the
  pass that wrote it. Depth keys keep the sample count, which must match the
  colour image beside them.
- **Keys hold the host format**, so `8_8_8_8` and `8_8_8_8_GAMMA` at one tile
  are one image, as the title's resolves expect.
- A target is as wide as its pitch and as tall as the bottom of the window
  scissor, each at most 4096. A target that must grow is rebuilt.
- A new target is cleared once. Depth goes to 0 (MW2 tests `GEQUAL` and
  clears to 0). Colour goes to dark blue-grey, so an undrawn surface is not
  mistaken for black.
- Only render target 0 is bound.

| Guest colour format | Vulkan |
|---|---|
| 0, 1, 10 (`8_8_8_8`, `_GAMMA`) | `R8G8B8A8_UNORM` |
| 2, 3, 12 | `A2B10G10R10_UNORM_PACK32` |
| 4, 5 | `R16G16_SNORM`, `R16G16B16A16_SNORM` |
| 6, 7 | `R16G16_SFLOAT`, `R16G16B16A16_SFLOAT` |
| 14, 15 | `R32_SFLOAT`, `R32G32_SFLOAT` |

Depth is per the surface's format, as Xenia has it: a `D24S8` surface is
`D24_UNORM_S8_UINT`, or `D32_SFLOAT_S8_UINT` on devices without it (AMD), and a
`D24FS8` surface is always `D32_SFLOAT_S8_UINT`. MW2 draws all its depth in
`D24FS8`, reversed (cleared to zero, tested `GEQUAL`), so it is float on every
device: in a 24-bit integer the distance gets few steps, and the depth bias,
counted in the format's steps, comes out too strong there. The shader libraries
are built with the `D24S8` format, which a fragment shader library ignores but
for there being a depth attachment. The `D24S8` surfaces' format is
`D24_UNORM_S8_UINT` where it can be drawn into and sampled (Xenia's rule), else
`D32_SFLOAT_S8_UINT` where it can be drawn into (Xenia takes it unchecked), else
`D24_UNORM_S8_UINT` where it can be drawn into; the float format is
`D32_SFLOAT_S8_UINT` where it can be drawn into, else the `D24S8` surfaces'
(`shaders/src/shader_build.rs`, shared with the launcher). Sampling is asked of each use: a
depth resolve's copy, and the drawn depth resolve's view, are made only where the
format can be sampled. With no depth format the renderer does not start: it
then draws nothing and the run goes on, shutdown still destroying what it made.

What the renderer does where a format lacks a feature (`format_support.rs`,
each from Xenia's texture cache, render target cache or presenter): a target
whose format cannot be drawn into is refused like one of no format, and a
colour target is made sampled only where its format can be (nothing samples a
colour target: resolves copy out of it); a resolve's copy is made only of a
format that can be sampled -- otherwise the image there goes and the textures
at that address are read from memory -- with its sRGB view only where the sRGB
format can be, and is sampled with point filters where its format cannot be
filtered; `Swap`'s blit into the present image, the presenter's into the
swapchain and the backdrop's out of the frame and down its blur's levels filter
linearly only where the source format can be filtered (the backdrop's sampler the
same), and a
front buffer whose format cannot be blitted out of is not presented. The
present images are `R8G8B8A8_UNORM`, which Vulkan requires to take blits and
storage writes; the renderer checks it. `MW2_FAKE_FORMAT_UNSUPPORTED` takes
features away to try each of these ([switches.md](switches.md)). A clear decodes `RB_DEPTH_CLEAR` per
format (Xenia's `UNorm24To32`, `Float20e4To32`).
Draws use dynamic rendering. Both attachments are `LOAD`/`STORE`, because a
frame is many rendering instances over the same images and only the title
clears.

### Barriers

The renderer keeps, for every image it uses -- each target and its twin,
each resolve copy, each texture, each present image, the display table's
table, the presenter's swapchain image and overlay, and each level of the
backdrop's blur -- what it was last used for: its layout and the stages and
accesses that used it there (`TrackedImage`, `tracked.rs`; Xenia keeps the same
on its render targets). Each use asks for the barrier from the last one, as
Xenia's `TransitionRenderTarget` does:

- none for a read after reads in the same layout (the reads are added to the
  last use, so the next write waits for all of them);
- otherwise one from the last use's stages and accesses (`TOP_OF_PIPE` for an
  image never used) to the next use's, including between two rendering
  instances over the same attachment, which have no dependency of their own
  (the attachment is loaded and kept); the barrier comes out of `UNDEFINED`
  only where the next use overwrites the whole image.
- for an attachment (an image a rendering instance has used: a target, its
  twin, a depth resolve copy, the swapchain image while the backdrop draws
  into it), while only reads follow a write, the source names that write too.
  Vulkan would take the chain of barriers, but the validation layer's
  synchronisation checks do not follow one across submissions for an
  attachment (a target resolved at a frame's end, then drawn into at the next
  frame's start). Every other image -- a texture, a colour resolve copy, the
  renderer's present images, the display table's images -- names its last use
  alone: a barrier's stages are the queue's, not the image's, so naming the
  write there too would add a wait.

An image is left where its last use left it and its next use brings it on: a
target read by a resolve or cleared goes back to being an attachment only when
the next rendering instance opens over it. An image draws sample -- a texture,
a resolve copy, the front buffer -- goes back to `SHADER_READ_ONLY` right after
a transfer, because a draw inside a rendering instance cannot bring it on. The
present image is left in `TRANSFER_SRC` for the window's copy and the frame
read back, which its tracked use stands for.

### Clears

- **With a resolve.** `RB_COPY_CONTROL` bits 8/9 clear what the resolve read:
  `RB_COLOR_CLEAR` is ARGB, and `RB_DEPTH_CLEAR` is depth << 8 | stencil.
  - The renderer clears the resolve's rectangle only, by Xenia's rule
    (`resolve_clear`, from `draw_util.cc` `GetResolveInfo` and
    `render_target_cache.cc` `PrepareHostRenderTargetsResolveClear`): the
    rectangle aligned out to 8 pixels, x clamped to the surface pitch, then
    turned into EDRAM tiles and back into pixels through the 11-bit base field,
    clamped to the pitch and to the render target's height. Depth's base is used
    when depth is cleared, else colour's (halved for a 64bpp format). Colour is
    cleared only when the resolve copies colour (a source select below 4), in the
    target the source select names (`RB_COLOR_INFO`, `RB_COLOR1_INFO`..3).
  - Each target is cleared only where Xenia's tile range for it is not empty: the
    rectangle's tiles, their start clamped to EDRAM's 2048 tiles (colour's doubled
    for 64bpp first), and, when both are cleared, each stopped where the other
    starts in the 11-bit tile space. Depth and colour at the same base clear
    neither.
  - Nothing is cleared for a surface Xenia refuses: no pitch, a pitch past 8192
    pixels, or a pitch at the targets' scale wider than the device's widest
    render target (`maxFramebufferWidth` and `maxImageDimension2D`, Xenia's
    `GetMaxRenderTargetWidth`).
  - The clear does not depend on the copy. A resolve whose copy is refused (its
    surface never drawn into, no destination, an empty copy, no destination
    image) still clears, as Xenia's `Resolve` does (`clear_without_copy`).
  - Xenia clamps vertex buffer 0's corners to the window scissor; the renderer's
    resolve copies the scissor itself, so the scissor is the rectangle here too.
  - The copy itself always reads RT0's `RB_COLOR_INFO`, whatever the source
    select; Xenia reads the selected target's.
  - A rectangle covering the whole image is a transfer clear. A smaller
    one is a rendering instance of that area whose load clears it (Xenia's
    `vkCmdClearAttachments` with the rectangle), which keeps the rest of the image.
  - The report counts the clears by what the rectangle came to (`clears after a
    resolve by Xenia's rectangle`), and those not made for an empty tile range.
  - MW2 asks for no clear after a resolve.
- **The EDRAM clear idiom.** MW2 draws a rectangle list with depth writes on,
  `ZFunc` `ALWAYS`, and the X/Y viewport scale off. The rectangles come in pairs:
  most of a row at half pitch and twice the samples, then the leftover columns,
  because a surface is not a whole number of 80-sample tiles.
  - In depth-only mode, the rectangles are not drawn. `clear_aliased_depth` zeroes
    every depth target at or above their base tile. This happens before any
    target is sized, because the scissor is at its 8192 reset value.
  - In colour+depth mode, each half is drawn into the colour surface with the
    lowest sample count at that tile and format whose `pitch << samples` spans
    the same row.
- **Ordinary draws.** Nothing clears through a load op.

## A draw

`Draw` resolves the topology, both shaders (by microcode hash) and the scissor,
and handles the clear idiom. It finds the targets and opens a rendering
instance if they changed. It writes indices, constants and vertex data into the
arena, builds the pipeline, finds each sampled texture's place in the texture
set, and writes the draw's record: where its constants are, its textures, and
the register state its shaders apply. It queues a `DrawOp` holding only the
dynamic state that differs from what the command buffer has bound, and the
draw, which names its record as its base vertex. Draws that cannot be made
are counted by reason (`Skip`).

Primitives 1-6 map to Vulkan's point, line and triangle topologies, and 12 (line
loop) to a line strip. 8 (rectangle list) becomes triangles `a b c a c d`, with
a fourth vertex `v0 + v2 - v1` appended per rectangle, computed per dword as a
float except where all three corners agree (packed colours). The indices are
16-bit while the list's 4n vertices fit, and 32-bit past 16,384 rectangles, as
Xenia's expansion index buffer is (the guest's vertex count allows 21,845).

| State | Registers | Where |
|---|---|---|
| blend, write mask | `RB_BLENDCONTROL0`, `RB_COLOR_MASK` | pipeline |
| culling, winding | `PA_SU_SC_MODE_CNTL` | dynamic |
| depth and stencil tests | `RB_DEPTHCONTROL` | dynamic |
| stencil ref and masks | `RB_STENCILREFMASK(_BF)` | dynamic |
| depth bias | `PA_SU_POLY_OFFSET_*` | dynamic |
| viewport, scissor | `PA_CL_VPORT_*`, `PA_CL_VTE_CNTL`, `PA_SC_WINDOW_SCISSOR_*` | dynamic |
| alpha test | `RB_COLORCONTROL`, `RB_ALPHA_REF` | the draw's record |

- **Viewport.** The Vulkan viewport is `height = 2 * scale`, `y = offset -
  scale`, **with signs kept**. Xenos expresses +Y up as a negative Y scale.
  Taking the magnitude flips the frame and every winding. A term that
  `PA_CL_VTE_CNTL` disables is scale 1 and offset 0 (Xenia's
  `GetViewportInfo`).
- **Pre-transformed vertices.** With the X/Y scale off, the shader has emitted
  window coordinates (D3D9's `XYZRHW`), which Vulkan would clip away. The
  vertex epilogue applies `(2 / guest pitch, 2 / height, -1, -1)` from the
  draw's record over a whole-target viewport, and the identity for other draws.
- **Depth bias** uses Xenia's conversion. The offset is scaled by 2^24
  (`D24FS8`) or 2^24 - 1 (`D24S8`), and the slope is divided by 16.
- **Alpha test** runs in the pixel shader, and is forced to `ALWAYS` in
  depth-only mode, where the console runs no pixel shader. Shadow casters have
  none and would otherwise run whatever the stream loaded last.

## The frame arena

Everything a draw reads that is not a texture is copied into one host-visible
buffer. The runtime asks for 512 MB (`MW2_ARENA_MB`) and halves the request
down to 32 MB. The buffer is split into one share per frame slot, each 4 KB
short so that a full 256-vec4 uniform range stays in bounds. A draw that finds
its share full is skipped.

The buffer is written and never read back. The driver maps it write-combined,
where a read is many times the cost of one from ordinary memory, so whatever
has to be looked at again after it is written (the fetch constants while their
addresses are rewritten, a rectangle's corners) is put together in ordinary
memory and copied across once. Reading the fetch block in place cost a fifth
of the ring consumer's time on Team Player. Of the fetch block's 96 pairs only
those the draw's two programs read are put together and written (the
translator records them, `fetchPairsRead`); the rest of it is whatever the
arena held, which nothing reads.

| Set | Contents |
|---|---|
| 0 | nothing |
| 1 | every texture and sampler, bound once per command buffer |
| 2 | the arena as dwords and as vec4, bound once per command buffer |

A draw's record says where each of its five constant blocks is in the arena
([shaders.md](shaders.md#the-module-interface)). Consecutive draws whose
records would be the same share one.

- **Float constants.** Each stage reads 256 from its `SQ_VS_CONST` /
  `SQ_PS_CONST` base. A pixel program's window is copied as far as the shader
  reads (all of it under relative addressing), and reused while the register
  file's write stamp over that range is unchanged. A vertex program's is
  copied a block of 16 at a time: a block whose stamp has not moved since its
  last copy into this frame's arena is not copied again, and the draw gets a
  table of where the blocks are. With four players in split screen that halved
  the constants copied a frame, 25 MB to 13 MB. D3D sends a draw's constants
  whole, so a write of the value a register already holds does not stamp it,
  and a run of them is swapped, compared and stored in one pass straight from
  the command buffer, and stamped, a block of 64 registers at a time
  (`RegisterFile::WriteRun`). Fetch constants are stamped by every write: the
  vertex data they name is copied with them, and the title rewrites some
  buffers in place.
- **Vertex data is relocated, not mapped.** Each vertex fetch's reachable
  window (lowest to highest index times the stride, plus the fetch tail,
  clipped to the declared size) is copied. Its address in the uploaded fetch
  constant is rewritten to point at the copy. A copy is shared only within
  **one draw**, because the title rewrites dynamic geometry in place during a
  frame.
- **Indices** are byte-swapped from big-endian for every draw, never cached,
  and scanned for their range, ignoring the restart value.

The arena is `src/runtime/gpu/vulkan/renderer_arena.rs` (the allocator, the
five constant blocks, the vertex windows, the index swap built for AVX2 and for
x86-64-v2 and picked at start), the arithmetic of a draw `renderer_draw.rs`
(scissor, viewport, the clip transform, the alpha test, the record's length, the
depth bias), the state mappings and `PipelineState` `renderer_pipelines.rs`, and
the keys they share `renderer_state.rs`.

### The shadow of guest memory

The same buffer continues with a 512 MB shadow of guest physical memory, in
which physical address N sits at offset N. A vertex window in pages the title
has not written since they were copied is read there instead of being copied.
`src/runtime/gpu/memory_watch.rs` tracks writes as Xenia does. Copied pages
are made read-only through both windows of the physical bank, and the title's
first write faults once, which marks the page and makes it writable again.

A page is `Never` (never copied), `Loose` (written since its copy), `Arming`
(read-only, being copied) or `Watched` (read-only, the copy current). The
consumer stores `Arming`, protects the run read-only, copies it and moves it to
`Watched`. A writer makes the page writable before it moves it to `Loose`.

- **A fault on a `Loose` page makes the page writable again** (counted, and
  reported as `loose faults` when there are any). A writer can loosen a page
  between the consumer's store of `Arming` and its protect, which leaves the
  page `Loose` and read-only. Answered without making the page writable, the
  writing thread would fault for ever, unless the consumer happened to copy
  that page again. The handler makes it writable and reads the state again, so
  a page armed again in between is loosened properly. The count is every fault
  on a `Loose` page, not only stuck ones: when two threads fault on one page
  together, the second handler finds it `Loose` and protects it once more (two
  more protections under `MW2_REPORT`). The handler cannot tell the two cases apart
  without asking the kernel, so `loose faults` is not a livelock count.
- **Turning the watch off makes every `Loose` page writable too.** Such a page
  can still be read-only when the renderer shuts down, and once the watch is off
  the handler no longer answers, so a write to it would be taken for a crash.
  `set_shadow(null)` makes each run of `Loose` pages writable in one call before
  it turns the handler off. These protections are not counted in the report.
- The fault handler, `will_write` and the protect take no lock, allocate
  nothing and log nothing. Whether protections are timed (`MW2_REPORT`) is read
  once, when the shadow is set.

- A page's copy is replaced only once no submission on the GPU has read it.
- A page rewritten within 1024 frames of its copy waits 2^strikes frames (at
  most 2^10) before it is copied again. This keeps the dynamic-geometry ring
  from costing faults and `mprotect`s on every lap.
- The host kernel must never write into guest memory, because a system call
  fails with `EFAULT` on a read-only page. File reads and network packets go
  through a runtime buffer. Bulk runtime writes call `memory_watch::will_write`.

The texture cache uses the same watch. `MW2_NO_SHADOW=1` (diagnostics builds)
turns the shadow off.

## Resolves

A draw in `Copy` mode is a resolve over the window scissor. `RB_COPY_CONTROL`
selects colour or depth (source 4) and the clears, `RB_COPY_DEST_BASE` gives
the destination, and `RB_COPY_DEST_PITCH` / `_INFO` describe the destination
surface.

**Resolves are not written to guest memory.** They fill a Vulkan image keyed by
destination address, and a texture fetch of that address binds the image.

- **An image per frame slot.** The first resolve of a frame advances to the next
  image, and the frame's later resolves reuse it. Frames overlap on the GPU,
  and the title resolves several shadow cascades into one destination per
  frame.
- The image has the size of the destination surface, exactly so on the frame's
  first resolve, because fetches normalise over the image. Each rectangle lands
  at its own offset.
- **The surface's width is its texture's.** `RB_COPY_DEST_PITCH` gives the
  pitch, the width rounded up to 32 texels, and a fetch normalises over its
  own width. A 960-wide scene's quarter-size blur buffers are 240 wide in
  256-wide surfaces, and an image as wide as the pitch stretched them: the
  aimed sight's blur doubled, the menus' blurred backdrop slid off the right.
  The first fetch narrower than its copy is remembered
  (`State::fetchedWidth`), and the resolves after it make the image that
  wide. The console's sizes never differ.
- **A resolve that lands inside a surface started this frame** goes into that
  surface's image at the matching row, without discarding it. The sun shadow
  map is one 1024x2048 surface filled by two resolves.
- Colour and depth are copied, or resolved straight into the image from a
  multisampled surface ([Multisampling](#multisampling)), at the place the
  rectangle names, with nothing in between. Colour also gets an sRGB view.
  Depth is sampled through a depth-only view, since the title reads scene
  depth as a texture; nothing reads a resolved stencil, so a copy takes the
  depth alone.
- **The frame** is the front buffer the swap packet names (`PM4_XE_SWAP`, which
  `VdSwap` writes): a resolve destination like any other. At the swap its
  newest copy is blitted into one of four present images, run through the
  display colour table, and submitted. A resolve is never shown for having the
  display's size: the title also keeps the screen in a 1280x720 texture, before
  the interface is drawn, for the next frame's blur.

A resolve does not end the submission unless the slot's arena share is half used.

The targets are `src/runtime/gpu/vulkan/renderer_targets.rs` (the images, their
twins, the clears, the passes, the depth resolve by a draw or by a rendering
instance, and the multisampling decisions), the resolves and the swap
`renderer_resolve.rs`, the display colour table `display_table.rs` and the
renderer's own shaders `spirv_builtin.rs` (outside the shader-build hash); they
reach the rest of the renderer through a `Frame` trait (the frame, the pass, the
submission, the skips).
A depth resolve whose rectangle starts past its surface is empty and refused, as
one starting at its edge is. Across, that is Xenia's rule: its resolve clamps x to
the surface pitch and takes an empty region as a no-op. Down, Xenia clamps nothing
and reads EDRAM on below the surface, which the target image does not hold, so the
refusal there is the renderer's own; the report counts both, and apart those past
the height that start short of the right edge, the ones Xenia's x clamp would not
already have emptied. The display colour table
keeps a descriptor set for each of at most 8 presented images (the renderer
presents from 4); a frame of any other goes out without the table, counted.

## Gamma

- **Encode on write.** Xenos applies a piecewise-linear gamma curve to RGB
  written into an `8_8_8_8_GAMMA` target, where MW2 draws the world. The pixel
  shader does the same (Xenia's `LinearToPWLGamma`) when the flag in the
  draw's record is set.
- **Linearise on read.** A fetch whose component signs say `gamma` samples
  through an sRGB view or format, which matches the Xenos curve to within a
  code value.
- Both are required. With only one, the post chain compounds the encoding.
- **Display colour table.** The title writes `DC_LUT_30_COLOR` 256 times from
  `DC_LUT_RW_INDEX` 0, which auto-increments, with channels selected by
  `DC_LUT_WRITE_EN_MASK`. Each entry is 10 bits per channel, blue lowest. Only
  a whole sweep is a curve, so the table is published under a lock at entry 255,
  and frames are shown untouched until then. `display_table.rs` applies it as
  a compute pass on the present image.

## Resolution scale

`MW2_SCALE=2` or `3`, read once at start-up, makes every EDRAM surface, every
resolve copy and the present images that many times wider and taller. The
title is not told. A `Target`'s and a `Resolved`'s sizes, the keys, and every
rectangle worked out from the registers stay in the title's pixels, and are
multiplied where they are handed to Vulkan: image extents, render areas,
viewport and scissor, and the regions of resolves, copies and blits. So the
frame is still "a colour resolve of 1280x720". The scale is lowered until
4096 of the title's pixels fit one of the device's images.

The scale is a whole number, so the title's scissors and resolve rectangles
land on pixel boundaries, and it applies to every surface, since the passes
read each other's at matching sizes. Three things are not a plain
multiplication:

- **Fetches of a resolve's copy.** The translated shader takes a texture's
  size from the bound image, for half-texel offsets and unnormalised
  coordinates, and a copy is larger than the surface the title means. Each
  draw pushes a bit per slot bound to a copy, with the reciprocal of the
  copy's scale, and a shader built under a scale divides those slots' sizes
  by it. A shader built without one has no such code ([shaders.md](shaders.md)).
- **Occlusion counts** are divided by the scale squared, each query's by its
  pass's: the title compares them with numbers of its own pixels.
- **Depth bias.** The slope term is multiplied by the scale, as Xenia does: a
  slope is depth per pixel, and the pixels are smaller.

The launcher's RESOLUTION entry keeps the scale in `.env` as
`MW2_SCALE=<n>`, with the title's frame below
([switches.md](switches.md#the-settings-file)).

### The shadow maps' scale

`MW2_SHADOW_SCALE=2` to `4` draws the shadow maps at a scale of their own,
never below `MW2_SCALE`'s. The sun's map is 1024x2048, two cascades, and the
spots' 512x2048, as on the PC; a shadow's edge steps by a texel of them.

**Which passes** (`src/runtime/shadow_resolution.rs`). `R_InitRenderTargets`
fills `gfxRenderTargets`, `{ image, colour surface, depth surface, width,
height }` a target; 11 and 12 are the two shadow maps. A D3D surface keeps
the registers D3D writes when it is bound: `RB_SURFACE_INFO` at +24 and
`RB_DEPTH_INFO` at +28. Both maps draw depth at tile 0 in D24FS8 at one
sample, and no other target the title made does; a pass with no depth
surface has `RB_DEPTH_INFO` 0, tile 0 in D24S8. So a pass whose depth
registers name that tile, format and sample count is drawn at the shadow
maps' scale (`pass_scale`), its colour surface too.

**Keys.** A `TargetKey` and a `Resolved` carry the scale they were made at,
and a copy takes its source surface's. The spots' map has its colour surface
at the tile and pitch of the half-size particles' (tile 0, 560 wide), and
each pass gets its own image at its own scale: nothing reads one through the
other.

**Height.** The title also draws a stencil rectangle into the shadow surface
in depth-only mode, with the scissor at its 8192 reset value, which makes the
surface 4096 of its rows tall at 1x. A shadow pass's surface is held to the
height the title made the maps, 2048: at 4x the depth surface is then
4160x8192 rather than twice that.

The shaders depend only on whether either scale is past 1, so the
precompiled shaders' marker records that (`scaled yes`), not the scales.
The launcher's SHADOWS entry keeps it as `MW2_SHADOW_SCALE=<n>`.

## The title's frame and the picture's shape

`MW2_RENDER=<w>x<h>` (`src/hooks/render_size.rs`, `src/runtime/render_size.rs`) gives the title another frame
than the console's 1280x720, and so another shape: 960x600 is 16:10, and at
`MW2_SCALE=2` the window gets 1920x1200. The present images are the frame
times the scale; the window letterboxes them as before.

**Where the title takes its sizes.** Not from the video mode: `R_VideoSizes`
writes a 1024x600 scene and a 1280x720 frame whenever the mode is high
definition (896x576 and the mode's own size, clamped to 640x480-1280x720,
otherwise). `R_SetVidConfig` makes the `vidConfig` of them, with the
picture's aspect 16/9 or 4/3 by the widescreen flag alone, and the pixel aspects
`h * aspect / w` of the scene and the frame. Everything after reads that
`vidConfig`: D3D's back buffer and render targets, the views' projections,
the HUD's and the menus' placement, the cinematics' letterbox. It is the PC
game's engine, which draws at any shape. The hooks write the sizes after
`R_VideoSizes`, and after `R_SetVidConfig` the aspect, the frame's width over
its height, with square pixels. With the console's frame they do nothing.

**A 4:3 frame.** The video mode (`src/runtime/kernel/video.rs`) says the TV is
widescreen only for a frame wider than 4:3, so a 4:3 frame is the console on a
4:3 TV: `R_SetVidConfig`'s aspect is already 4/3, `dvarbool(widescreen)` is 0
and the menus take their own 4:3 layout, 640 of their units across. The
launcher's 4:3 rows are 640x480, 800x600 and 1024x768 at 1x, and 1280x960 to
2880x2160 scaled.

**The sizes EDRAM allows** (`src/runtime/render_size.rs`; `launcher/src/settings.rs` has the same check of a frame for the launcher's resolutions). EDRAM is 2048 tiles of
80x16 samples. The title puts a surface's depth at the top and its colour
below it: the console's scene, 2x and 1040 wide, is colour at tile 98 and depth
at 1073, 975 tiles each. So:

- the frame, drawn at 1x, fits when its colour and depth together take 2048
  tiles or fewer. The frames taken are 640x360 to 1280x800, a whole number of
  16 pixels wide and 4 tall.
- the scene, drawn at the console's 2x, is given no more tiles than the
  console's takes, 1950: what the title keeps beside it is not known. It is the
  frame itself when that fits, else the largest of the frame's shape that does,
  16 pixels narrower at a time: 960x600 for a 1280x800 frame. 1024x640, 2080
  tiles, would not fit.

The title's own count of samples stays 2, the console's: `MW2_MSAA` and
`MW2_NO_MSAA` act on the surfaces the title multisamples, and a title drawing
at 1x would leave them none.

**Pictures made for 16:9.** A cinematic is letterboxed by the title itself:
`R_DrawCinematicLetterboxed` draws the 16:9 video whole, as wide as the frame,
between black bars. A 2D picture over the whole frame (a loading screen, a
menu's background, a full-screen overlay) would be stretched; it covers the
frame instead: the hook on `R_DrawStretchPic` makes it as large as the frame
and cuts what spills over, its sides on a 16:10 or 4:3 frame. It does so for a
picture whose every texture was loaded from a file
(`IMG_CATEGORY_LOAD_FROM_FILE`). A copy of the screen (a render target), which
the menus blur behind them, is drawn over the frame too, and is left whole.

The run's log names each material the first time it is cut ("a picture covers
the frame (mw2_main_background)").

**The menus' own width.** The menus place most things by their alignment,
which follows the `vidConfig`, but some lay out across the screen by
expression, taking its width in their units as `640 + 213.333 *
dvarbool(widescreen)`: 640 at 4:3, 853.33 at 16:9. The experience bar's
width is one. Before the evaluator (`UI_EvaluateStatement`) runs a
statement, a constant it multiplies `dvarbool(widescreen)` by, on either
side, is scaled by the share of the way from 4:3 to 16:9 the picture's shape
is: 0.6 at 16:10, so the width is 768. Each constant is scaled once; one a
zone loads again is scaled again. `widescreen` itself stays 1: the menus also
test it (the loading screen shows its bar `when( dvarbool(widescreen) )`), and
a test makes a whole number of what it is given.

## Multisampling

The title's 2x and 4x surfaces are drawn with that many samples. The count is in
the target, pipeline and output-library keys, rounded up to one the device
supports for colour, depth and stencil. Multisampling needs Vulkan 1.2 and a
`SAMPLE_ZERO` depth and stencil resolve. Without them, or under
`MW2_NO_MSAA=1`, everything is 1x. `MW2_MSAA=<n>` forces n.

A multisampled image cannot be blitted or copied, so every read of one is a
resolve. Colour goes through `vkCmdResolveImage`, which averages as the console
does. Depth takes sample 0, because an averaged depth describes a surface
nothing was drawn at. It is copied by a draw over the rectangle whose fragment
shader fetches the first sample and writes it as its depth, as Xenia reads
multisampled surfaces with a shader. Where the device cannot sample a
multisampled depth image of both formats at the counts in use, an empty
rendering instance resolves it with `SAMPLE_ZERO`, the stencil too though
nothing reads it: Intel's Windows driver, asked to resolve the depth alone (a
stencil resolve of `NONE`, which `independentResolveNone` allows), resolved
nothing, and the depth of field blurred the whole screen when aiming (issue
#37). The draw costs what a depth-only resolve costs; resolving the stencil as
well costs 0.3 ms a frame at 1080p on a Renoir iGPU. Both resolve straight into
the resolve's image, which for depth must be an attachment as well. The resolve
writes where the rectangle sits in its surface. So a depth resolve goes through
a single-sampled **twin** of the surface, and is copied from there, in two
cases: when it lands lower in its image, and when the image is not yet an
attachment but already holds an earlier resolve of the frame. The target dumps
use the twin too. Sample positions are Vulkan's standard ones.

## Anisotropic filtering

A fetch constant carries the anisotropy the title asks for, and the sampler
takes it, up to the device's `maxSamplerAnisotropy`; a fetch whose LOD does not
come from derivatives filters along none, as the hardware does. The title
picks it from two settings of the PC game's that the console keeps:
`r_texFilterAnisoMax`, 16, and `r_texFilterAnisoMin`, 1, between which each
material's sampler state falls. On the console the materials ask for 1:1 to
4:1, and samplers that filter without mips (the menus, the HUD) for none.

`MW2_ANISOTROPY=<n>` (`src/hooks/texture_filtering.rs`) is the PC's ANISOTROPIC
FILTERING slider: `Dvar_RegisterInt` registers `r_texFilterAnisoMin` with n as
its default. The setting has no flags, so it is no cheat, and nothing the title
runs sets it; with 16 every mipmapped sampler of a level asks for 16:1, and the
menus' stay at none.

## Submissions, segments and frame slots

There are three frame slots, each with a command pool, a fence, an arena share
and a submission serial. `BeginFrame` moves to the next slot and waits only if
the GPU still holds it. A submission is normally one frame.

A submission is cut into **segments** at each completion point
([d3d9-seam.md](d3d9-seam.md#completion-points)). Once the title learns that
draws have finished, it may reuse the memory they read, which the console's GPU
had already read. So `before_completion` closes the segment:
the texture cache's `end_segment` re-reads the segment's watched textures, and the
draws end in a command buffer of their own. Each segment is submitted as its
uploads followed by its draws. `before_stream_write` also closes a segment before
the image pool's copy overwrites memory the segment binds.

Before an image a submission may use is destroyed (a target that grows, or a
resolve image that is replaced), `retire_before_destroy` submits and waits,
drains the recorder, and idles the device. Otherwise a command buffer points at
freed memory and the GPU page-faults.

The renderer whole is `src/runtime/gpu/vulkan/renderer.rs` (its start, its end,
and the report it leaves in a static for the report written after the shutdown),
`renderer_frame.rs` (frame slots, segments, submissions, the occlusion
queries), `renderer_draw.rs` (a draw and the runs the recorder makes with one
multi-draw), `renderer_diagnostics.rs` and `renderer_passtime.rs`. It has one
owner, the ring consumer (`src/runtime/gpu/gpu.rs`): the run thread makes and
starts it before the consumer starts and hands it over; the
command processor passes it down, beside the register file, to each draw,
resolve, swap, completion point and occlusion event, a direct call; the
shutdown takes it back when the consumer is joined. It is boxed, and its device
tables boxed inside it, because the recorder thread and the pipeline optimiser
hold a pointer to them. What other threads tell it goes through atomics or a
queue: the texture cache's `memory_written_elsewhere` from a guest thread (the image
pool's move made at once: whenever nothing consumes the stream, and in play
when the destination has no word-aligned physical address for the copy draw to
name) is queued and applied at the start of the consumer's next batch; the report counts the ranges
queued and the ranges applied, and only one queued after the consumer's last
batch, during the exit, is never applied. The texture cache is kept after the
shutdown for the report;
the report of a run that ends on a fault prints only what statics hold. A slot's
fence counts only once the recorder has made the submission
(`queued_at`), the submission resets its fence under the queue's lock and
signals it with an empty submission when its command buffer will not end or the
submission fails, and the trace writes the submission where it is made, before
`gpu_time` submits it.

## The recorder thread

The renderer queues each Vulkan call (`recorder::command`) as a closure holding
a copy of its arguments, in a byte ring. The recorder thread makes the calls in order,
including submissions and the frame's hand-off to the window. The swap is
parsed on the consumer, so the title's frame retires at parse time.

The recorder works in bursts. It sleeps while less than 64 KB is queued and
none of it is due, and is woken when that much is waiting, at once for a
submission or a hand-off, when a thread waits for it, and after a millisecond
in which the consumer had nothing to read. One wake for gathered calls a sleep is
enough. A call due now does not spend it: the recorder can make that call and
sleep again at the same position before the producer looks at its flag, and the
next 64 KB would then wait for a flush. A diagnostics build counts the wakes
made only because a call due now left the wake unspent. Calls come every couple of
microseconds and the recorder is faster than that, so a recorder that spun
until the next one never slept, and held a core; one that waited on the queue
position itself woke at every call, because the wait spins first.

Draws are made in runs. A draw that changes no state since the last one is
held back, and a run of them is made with one `vkCmdDrawMultiIndexedEXT` (or
`vkCmdDrawMultiEXT`), each with its record as its own vertex offset: the
driver checks and emits the state once a run, and each draw is then a base
vertex and a draw packet. A run ends at a draw that changes any state, and at
any other call: the recorder runs `make_gathered_draws` before every call not
queued with `recorder::joining`. With four players in split screen 97% of draws are made
this way, about thirty to a call. Without `VK_EXT_multi_draw` each draw is
made on its own.

An indirect draw over commands the consumer writes into the arena does as much
for the recorder, but it puts twenty more bytes a draw on the consumer, which
is what four players wait on; the multi-draw keeps all of it on the recorder.

- A queued call must own what it uses: the renderer's state will have moved on
  by the time the call runs.
- A slot's fence means nothing until the recorder has submitted to it.
  `wait_for_slot` first waits for the recorder to reach the slot's `queued_at`.
- Anything that destroys what a frame may use, or that submits on its own,
  calls `recorder::drain` first.

`MW2_RECORD_THREAD=0` (diagnostics builds) makes the calls on the consumer.

The recorder is `src/runtime/gpu/vulkan/recorder.rs`, its device wrapper
`device.rs` (ash's tables, a method for each Vulkan call the renderer makes,
which writes the command trace's line when the trace is on) and the trace itself
`trace.rs`. A queued call is a Rust closure, moved into the ring behind its
header and moved out, and dropped, once by the recorder. Every `park` sits in a
loop on its own condition, which its waker sets before it unparks, so a stray
token costs one more look.

## Presenting

The window, its device and the frame queue are
`src/runtime/gpu/vulkan/presenter.rs`, on a thread of its own (`vulkan presenter`,
with `vulkan present wait` beside it). The system's screens' blur is `backdrop.rs`,
the GPU's busy time for `MW2_REPORT` is `gpu_time.rs` and RenderDoc's frames are
`capture.rs`, in the same folder. The
swapchain's format is Xenia's choice (`B8G8R8A8_UNORM`, then `R8G8B8A8_UNORM`, each
in the sRGB colour space before another, then another format in that colour space,
then the first) among the formats that are not sRGB-encoded and can be blitted into:
an sRGB-encoded one would encode the frame a second time, and is taken, with a
warning, only where nothing else can be blitted into. A surface whose images cannot
be transfer destinations, or that offers nothing to blit into, gets no swapchain and
a warning. Each
of the three warnings is printed once a run, however often the swapchain is remade. The
presenter makes the instance and the window, and the device through
`create_device` (`shaders/src/pipeline.rs`, shared with the launcher), which the
renderer then shares.

The swap waits (`wait_until_taken`) until the window has copied out
the present image it is about to reuse. With four present images, the renderer
can run three frames ahead. The window presents every frame once, in order,
one per blank (`FIFO`). Frames the window has not taken within 250 ms are
dropped, so a hidden window never stalls the game -- but a frame the window has
already taken off the queue is waited for until its copy is submitted, which
waits on nothing but the queue, so the renderer never draws over an image a copy
still reads (`FrameQueue::copying`). The window is resizable and
letterboxed, or stretched to fill it under `MW2_STRETCH=1`. `F8` or
`MW2_FULLSCREEN=1` switches to borderless fullscreen.

**The guest's blank follows the display.** On the console the blank the title
counts is the display's own. `VK_KHR_present_wait` timestamps each present.
The display period comes from the mode's exact rate, or from a slow average of
single-frame gaps. For a 59-61 Hz display, `guest_blank_period` returns that
period, 0.5% longer while more than 1.5 frames wait behind the one on screen,
so that latency drains unseen. It is never shorter. It returns 0, and the
vblank thread keeps its own 60 Hz, when the display runs at another rate,
present wait is missing, or nothing has been shown for half a second.

### More than 60 frames a second

`MW2_FPS_LIMIT` (`src/runtime/frame_rate.rs`) at anything but 60 takes the title off
the blank. Three things hold it at 60, and each is released its own way:

- **D3D's present interval.** The title's present puts the device's
  `D3DRS_PRESENTINTERVAL` in the flip request, and D3D's flip handler queues
  the frame for the next blank. `T_D3D_Present`'s hook sets the state to
  `D3DPRESENT_INTERVAL_IMMEDIATE` first, and the handler then retires the frame
  when the command processor reaches the swap
  ([d3d9-seam.md](d3d9-seam.md#the-flip-handshake)).
- **The title's limiter.** `Com_Frame` sleeps until `1000 / com_maxfps` whole
  milliseconds have passed since the last frame; `com_maxfps` is 60 unless
  set. The hook queues `com_maxfps <limit>` on the title's console, and
  `r_vsync 0`: with `r_vsync` on, the title takes a frame's length from the
  blanks it counted between swaps instead of from its clock.
- **The window.** It still presents one frame a blank, without tearing, but
  the newest: frames finished before it and not yet shown are let go. Nothing
  waits behind the frame on screen, so the guest's blank is never lengthened.

The guest's blank itself stays the display's; only the retiring of frames
leaves it. The simulation's speed does not change: a route walked at 93 frames
a second takes the time it takes at 60. `MW2_STUTTERS`, which counts blanks
without a new frame, is off in this mode.

## Occlusion queries

The title brackets counted draws with two `EVENT_WRITE_ZPD` events, with the
record address in `RB_SAMPLE_COUNT_ADDR` (`0x2325`). A query owns 64 bytes: the
end record at the base and the begin record at +32. A record is eight
**little-endian** dwords (D3D reads them with `lwbrx`):

    +0 total  +8 z-fail  +16 z-pass  +24 stencil-fail     (lanes A and B)

The title computes the end record's z-pass minus the begin record's. D3D
writes `FFFFFEED` into the z-pass lanes and polls until they change, so **every
end record must eventually be written**, or the title hangs.

- Begin writes zeros at once and resets 64 queries of a 256-query ring.
- A Vulkan query cannot span rendering instances, so the bracket opens one in
  each instance it covers and sums them.
- End answers 0 at once if nothing was covered. Otherwise the result is
  written by `collect_occlusion_queries` (in the consumer loop and during
  `WAIT_REG_MEM` polls) once the GPU has finished. Results go out in the order
  the title closed the brackets, saturated at `0x7FFFFFFF`.

## Pipelines

`PipelineKey` holds the two shader hashes, the attachment formats, the
topology, `RB_BLENDCONTROL0`, `RB_COLOR_MASK` and the sample count. Culling and
the depth and stencil tests are dynamic state. Dynamic rendering and extended
dynamic state are required, and so are what the module interface is built on
([shaders.md](shaders.md#the-module-interface)): descriptor indexing with
update-after-bind and partially bound sampled images, room for 65,536 of them
and 4,096 samplers in one set (counted, with set 2's two storage buffers and
the colour target, against each per-stage, per-set and pool limit as the
specification sums it), shader
draw parameters, subgroup ballot in pixel shaders and seventeen interpolators.
A device without one says which at the start. `VK_EXT_multi_draw` is used where
there is one.

On the console a shader is ready as soon as it is loaded. With
`VK_EXT_graphics_pipeline_library` the runtime compiles it at that point too
(`src/runtime/gpu/vulkan/renderer_libraries.rs`, the hooks in
`src/hooks/shader_preload.rs`):

- `T_D3D_InitPixelShader` and `T_D3D_PatchVertexFetches` are hooked. The
  shader object's header (magic `0x102A11xx`) is at +40 (pixel) or +872
  (vertex). Its seventh word locates `{offset in program, byte length}`, which
  gives exactly the bytes the stream later `IM_LOAD`s. Workers compile each
  shader as a pre-rasterisation or fragment-shader library, built by
  `shaders/src/shader_build.rs`, which the launcher's compile pass shares.
- A vertex shader is handed over once D3D has bound it, not when it is made.
  Its fetches name no format until the title binds it to a vertex declaration
  (`IDirect3DVertexShader9::Bind`), and D3D rewrites them in place: fetch
  constant, format, offset and stride, with neighbours on one stream merged
  into mini-fetches. The shader as made is never drawn.
- **A fragment library must carry the depth format in its own rendering info.**
  Without it, the library legally ignores depth and stencil state, and every
  draw comes out empty.
- State libraries are made per topology (vertex input) and per blend, mask,
  formats and samples (output). A draw fast-links the four. An idle-priority
  thread re-links them with link-time optimisation and swaps the result in.
- A fast link the optimised one replaces is destroyed at the start of a later
  frame, once the GPU has finished the submission the draws recorded at the
  swap went into and the recorder has made every call queued by then. A
  command buffer forgets the pipeline it last bound when it begins, so a
  handle the driver hands out again is never taken for the destroyed one.
- Workers pause during play, defined as 30 consecutive world frames, the last
  under 500 ms ago. A world frame has at least 8 colour draws into a
  multisampled target, or 200 into any target. Shaders not seen at load are
  compiled at their first draw.

Without pipeline libraries, or under `MW2_NO_PIPELINE_LIBRARIES=1`, whole
pipelines are built at their first draw.

A shader keeps its library and what the draw reads of its translation. Its
SPIR-V is dropped once the module and library are made, and its microcode too
unless `MW2_SHADER_CACHE` will write it out. Its module is destroyed once the
library exists: pipelines are linked from the library, which keeps what the
optimised link needs (`RETAIN_LINK_TIME_OPTIMIZATION_INFO`). A shader without a
library keeps its module, from which whole pipelines are built: every shader on
a device without pipeline libraries, and one whose library the driver refused.
A pair of one shader with a library and one without is built whole too: the
first's module is made again from the microcode the draw hands over, through
the same shared build, for that pipeline alone, and destroyed once it is
built.

`MW2_FORCE_REFUSED_LIBRARIES=vertex|pixel|all` (diagnostics builds) treats
every library of those shaders as refused by the driver, so that these whole
pipelines are built on a device that has pipeline libraries.

### Every shader compiled before play

The workers compile a level's shaders as it loads, but a driver with a cold
cache still takes tens of milliseconds a shader, and a level has thousands. So
a player's copy is played only with every shader of the game already in the
pipeline cache it starts with, `shaders/pipelines.bin`, or
`shaders/pipelines-scaled.bin` when anything is drawn larger than the title's
sizes (the translated code differs). The launcher fills it
(`launcher/src/precompile.rs`, `mw2-launcher --compile-shaders` from a terminal):

- **The list.** The zone crate (`zone/`) walks the fastfiles in `game/` the
  way the game's loader streams it, using the OpenAssetTools zone definitions
  in `zone/defs/`. Every pixel shader is listed as it is loaded. Every
  vertex shader is listed as the title binds it: the pass's declaration, the
  title's declaration tables (`sub_823EA570`) and D3D's patch routine are
  reproduced from the same data (`zone/src/vertex_binding.rs`). The title binds only
  a shader loaded fresh into one of a pass's 15 `vertexShaderArray` slots
  (`sub_821BBE70` → `sub_823C2E28` → `Bind`). A slot whose source has no
  stream in its declaration type is not bound, and neither is the pass's own
  `vertexShader`, which `sub_821BBC08` loads alone. Those stay as made and are
  never drawn. The executables add D3D's own shaders, the ones sent with
  `IM_LOAD_IMMEDIATE` and the image pool's memory move, bound to the
  declaration the title made for it. The list is 15,044 shaders. The
  fastfiles are the disc's and the update's, by name, unless custom maps are
  on (`MW2_CUSTOM_MAPS`): then every one in `game/`, as the multiplayer loads
  them then.
- **The build.** Each one is translated and compiled on every core as the
  game would (`shader_build::build`): same translator, layout, depth format and
  scale, on the device the game's window would draw on. The pass reuses the
  last cache when only the game's files changed.
- **The marker.** `shaders/compiled.txt` (`compiled-scaled.txt` beside the
  scaled cache) is written last, after the cache is in place. Both files are written beside their place and renamed over it. The
  marker records what the cache was made for:
  - the build of the shader code: `MW2_SHADER_BUILD`, a SHA-256 `shaders/build.rs`
    takes of the shaders crate's, the compile pass's and the reader's sources;
  - the device (`deviceUUID`), its driver version and `pipelineCacheUUID`;
  - the resolution scale;
  - each fastfile's and executable's name, size and time.

  A change to any of these means the cache no longer holds what the game will
  build. Each scale keeps its pair, so a player who changes the resolution and
  goes back finds the first still compiled; a pass removes the other scale's
  pair only when the code, the device or the driver changed.
- **The gates.** The launcher checks the marker before PLAY and REPORT A BUG,
  and runs the pass again first when it no longer fits; it also runs the pass
  after an install. The game checks the marker on a player start, once the
  renderer has settled its device and scale and before the title runs. If it
  does not fit, the game starts `mw2-launcher --play <title> --device <id>` and
  ends, without writing its own cache over the launcher's. The cache is read
  only then, once the scale that names it is known. Its window is made
  hidden and shown only after this check (`show_window`), so a start that
  hands over shows nothing. The launcher then
  compiles for that device and starts the game, naming the device in
  `MW2_SHADERS_COMPILED_FOR`. A game that still finds no fit after that says so
  and plays on. Development runs are not gated.
- **The check.** `VK_EXT_pipeline_creation_feedback` says, for each library,
  whether it came out of the cache. The run's log and its bug report count the
  ones that did not ("N of them not in the launcher's"). That count should be
  0.

A device without pipeline libraries compiles a shader only together with a
draw's state, so the pass compiles nothing there. It leaves a marker that
says so, and the game builds pipelines at their first draw as before, into
that scale's cache, which it writes back at its end for the next run.

`MW2_SHADER_CACHE=<file>` records every pipeline a run builds: microcode,
formats and state (`SMW2`, version 4). The next run rebuilds them at start-up.
It has to be recorded, because the state a shader pair is drawn with exists
only in a previous run. The file is written back as a union, so it never
shrinks. `MW2_PIPELINE_CACHE=<file>` persists the driver's `VkPipelineCache`
in place of the player's cache.

## Checking

Diagnostics switches make the renderer a reference that another build of it
can be held against ([switches.md](switches.md#checking-the-renderer)): the
command trace, frame hashes and determinism say what a renderer did, and a
capture of the GPU's input lets two builds do it on the same input.

**The command trace.** `MW2_TRACE_COMMANDS=<file>[:<first frame>:<frames>]`
writes every Vulkan call the renderer makes that creates, destroys, allocates,
frees, records, begins, ends, resets or submits, one line each:
`<seq> <command> key=value ...`. The renderer makes its Vulkan calls through
`device.rs`, a method each, extension commands included; with the trace on
(`trace.rs`) the method also writes the line, after the call for one that makes
something and before it for the rest. Off, a method is the call and one test of
a flag.

- A handle is written as its kind and the ordinal of its creation within the
  kind: `Image12`, `View3`, `Cmd40`. A handle traced code did not make is named
  by its role: `Device`, `Physical`, `Queue`, `Cache`, `Layout` (the shaders'
  pipeline layout), `SetLayout0` to `SetLayout2`, `Present0` to `Present3`
  (the images handed to the window), `L<shader hash>` and `M<shader hash>`
  (a shader's library and module, made by the launcher's shared `Build`),
  `LInput<topology>` and `LOutput_...` (the state libraries). Anything else
  is written `?` and counted.
- Floats are written as their bits, `%08x`; arrays whole; a create info's
  `pNext` chain as its `sType`s, with the structures the renderer chains
  decoded. A graphics pipeline is written field by field: flags, stages,
  topology, rasteriser, samples, depth and stencil, blend and colour masks,
  dynamic states, layout, and the rendering formats and libraries in its chain.
  A state library, which the launcher's shared code makes, gets a
  `CreateStateLibrary out=<name> part=<flag>` line with the create info and
  chain it was made from, field by field.
- Never written: fence status, query results, waits, time.
- The submission of a frame is written where it is made (`gpu_time` submits it,
  and is not traced). The presenter, the backdrop, `gpu_time`, the capture and
  the launcher's shared code are not traced.
- Hash lines: `ArenaHash slot= used= fnv=` over a submission's arena share, at
  each submission; `UploadSlot slot= segment= from= to= fnv=` over the staging a
  segment's uploads read, at each segment's end; `ShadowArm page= fnv=` for each
  page copied into the shadow of guest memory.
- `frame n=<n>` after each finished frame (the window counts in these),
  `Present image=Present<i> frame=<n>` as the window is handed it, and
  `shutdown` when the renderer's shutdown begins. The destroys after that line
  come in hash-map order.
- The run ends with `renderer: command trace <file>: <n> lines of <m>, <k>
  unregistered handles` in the log.

The recorder thread makes the frame's commands later than the ring consumer
queues them, so the two threads' lines are put in one order: a line the
consumer writes goes after the commands it queued before it and before those it
queued after. The order is then the program's, not the threads' timing.

**Frame hashes.** `MW2_HASH_FRAMES=1` reads every present image back, as the
window is handed it, and logs `renderer: frame <n> hash <fnv64>`.

**Dumps.** The renderer sizes a dump's buffer by its format's texel (eight
bytes for a 16_16_16_16, 16_16_16_16_FLOAT or 32_32_FLOAT surface); such a dump
is written eight bits a channel. With `MW2_DUMP_TARGETS` the resolves' copies
are dumped in order of address.

**Deterministic.** `MW2_DETERMINISTIC=1` takes the clock out of the renderer's
decisions: the completed submission is the last one submitted (each is waited
for), an occlusion query's count is waited for, the shaders are built at their
first draw with no workers and no optimised links, every doomed texture is
destroyed at once, and neither `gpu_time` nor the pass timing runs.

**Forced fallbacks**, read once when the renderer starts: the depth resolve by
a rendering instance, every multisampled resolve through the surface's twin, a
small pool for the drawn resolve's sets, every resolve image made again every
n frames, one call a draw, every texture upload submitted alone, a small least
staging buffer, and a device lost after n seconds. The report's `renderer:
forced paths taken:` line counts the ones that count.

**The capture.** `MW2_CAPTURE_GPU=<dir>:<after presents>:<presents>`
(`runtime/gpu/stream_capture.h`) records what the command processor reads,
from the batch in which it has met `<after presents>` swap packets to the
batch in which it has met `<presents>` more. `MW2_REPLAY_GPU=<dir>`
(`src/runtime/gpu_replay.rs`) feeds it back with no title running, and
`tools/replay_diff.py <capture> <exe A> <exe B>` replays it on two
executables and compares their traces, frame hashes, built-in shaders and
report counters.

- *Positions.* A position is `(batch, packet)`: the ring batch the consumer
  took, counted from the capture's start, and the packet within it, counted
  from 1 over every packet it executes, those of its indirect buffers
  included. Packet 0 is before the batch's first packet; what happens between
  two batches belongs to the next one's packet 0. The replay makes the same
  batches, so a position means the same point in both.
- *The snapshot*, taken on the consumer between two batches: every non-zero
  page of physical memory up to the physical heap's top and of the register
  aperture, and the command processor's state: the register file with its
  stamps, the bin mask and select, each stage's shader (its words, hash, and
  the `IM_LOAD` it came from or that it was immediate), the display's colour
  table, the swap packets met, a pending wait for idle, and the arena's
  progress (its block, the last sequence, the pending position).
- *Page versions.* Every guest read on the GPU side notes its range: the
  indirect buffers, `IM_LOAD` microcode, `LOAD_ALU_CONSTANT` sources and
  `WAIT_REG_MEM` dwords in `src/runtime/gpu/command_processor.rs`, vertex windows, rectangle
  corners and indices in `renderer_arena.rs`, every texture range the cache
  reads or signs (with the page after it, as every capture has held it), the
  pages the memory watch copies into its shadow, and the source of an
  image-pool move. A page is hashed at its first read in a batch, and again at
  its next read after the consumer wrote it or the memory watch saw the title
  write it; the page a memory `WAIT_REG_MEM` polls is hashed again before its
  first poll in any case (the title may have written it, on a page the watch
  does not guard). It
  is stored, with its position, when it differs from what the
  replay will hold there, and when the title wrote it with the bytes it held
  (the replay's write then takes the fault the title's did). A page the
  consumer itself wrote (`MEM_WRITE`, `REG_TO_MEM`, the scratch write-back,
  `EVENT_WRITE*`, occlusion records, image-pool moves) that holds just what
  the replay's own write will leave is stored as `own`, and the replay only
  checks it. The ring's pages are left out: the ring is kept as dwords.
- *Events,* with their positions: each image-pool move the consumer finds
  queued, at the packet where it first looks at the queue; each
  `WAIT_REG_MEM`, with the dword its last poll read, how many polls it took
  and whether it gave up; each register read from the aperture whose value
  changed; each new arena progress block.
- *Files* in `<dir>`: `capture.idx`, text: `mw2 gpu capture 2`, then `after`,
  `ring <physical base> <dwords>`, `writeback <physical>`, `top <physical heap
  top>`, `snapshot <n>` and n lines `<page> <blob>`, then per batch `batch <n>
  ring <dwords> pages <k> events <e>` followed by its page lines `<packet>
  <page> <blob> [own]` and event lines (`pool <packet> <destination> <source>
  <size> <stream constant> <frame>`, `wait <packet> <memory> <address>
  <value> <polls> <timed out>`, `aperture <packet> <register> <value>`,
  `arena <packet> <block>`), in position order, and `end <batches>`;
  `ring.bin`, every batch's ring dwords as the guest held them; `pages.blob`,
  4 KB pages addressed by content, blob n at n x 4096, with `pages.idx`, each
  blob's hash; `state.bin`, the command processor's state. Pages are guest
  addresses shifted right 12, in hexadecimal. A thread of the capture's own
  writes the files.
- *The replay* skips the title: guest memory, the physical heap's top, the
  renderer and its window, the snapshot and the command processor's state
  (`mw2_cp_restore_for_replay`), the ring from its first dword. Then each
  batch, once the consumer has caught up: its journal handed to the command
  processor's hook, its packet-0 pages written through their plain addresses
  (so that the memory watch takes real faults), its ring dwords, the write
  pointer; the consumer is waited for, 30 s at most. Before each packet the
  hook writes that packet's pages, queues its image-pool moves, sets the
  aperture registers and the arena block, and makes a journalled
  `WAIT_REG_MEM` poll as often as it did: one the first poll met reads its
  dword as the capture's page left it (an `own` page there is checked), one met later
  has its dword written before the last poll, as the title's write during the
  wait. The capture writes an `own` page at the first read of it after the
  consumer wrote it, and the replay checks it at the same read: the first
  read of that page in that packet since the replay's own write to it (an
  image-pool move whose source and destination share a page reads it before
  writing it), or once the packet is done. After the last batch, two blanks, then
  the reports: `replay: <n> divergences, <m> wait timeouts`, and a line of
  counts that ends `; the replay did not finish` when the replay was cut short. The interrupts
  the stream raises go nowhere: no title set a callback.

## Not implemented

- Resolve write-back: the CPU cannot read resolved pixels from guest memory.
- Memory export in shaders. Its one use, the image pool's block copy, is made
  on the CPU at its draw (`src/runtime/gpu/image_move.rs`).
- Multiple render targets, the console's sample positions, and alpha to
  coverage (MW2 never sets `RB_COLORCONTROL`'s alpha-to-mask bit).
