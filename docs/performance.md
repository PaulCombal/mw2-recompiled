# Performance

What limits the frame rate, how a change is measured, and every optimisation tried:
the ones in the code with what each buys, and the ones not used with why. The figures
were measured on one machine: a laptop with an 8-core Zen 3 processor (Ryzen 7 7730U,
no LBR) and its integrated Radeon graphics (RADV), on mains power. They show how big
an effect is. They are not targets.

## What limits the frame rate

**The ring consumer's thread.** The title runs one frame ahead of the swaps the GPU
has parsed, and its render thread presents a frame at the vertical blank after the
frame retired. A frame is therefore lost whenever the consumer has not parsed the
frame's swap by the next blank. The consumer reads the command stream, keeps the
register file and turns every draw into arena copies and recorded Vulkan calls
([d3d9-seam.md](d3d9-seam.md), [rendering.md](rendering.md)). It has to run in the
stream's order and read guest memory at parse time, so it can't be split across
threads without the costs measured under [Not used](#not-used). Cutting the
consumer's CPU time per frame is what raises the frame rate. Everything else is
spare cores.

**At 1x, the campaign sits at the 60 fps cap** on this machine. The consumer's time
still matters for:
- a slower processor, such as the Steam Deck's 4 cores and 8 threads;
- a higher frame-rate limit;
- split screen, where four players give the consumer about 2.8 times the work:
  ~12,800 draws and 25 MB of constants a frame.

**At high resolution on an integrated GPU, the GPU limits instead.** Resolves and
copies are memory-bandwidth bound and scale with resolution: they are the
emulation's GPU tax, since a PC port samples its targets directly. CPU work doesn't
move a GPU-bound player.

## How a change is measured

- **What to compare.** The consumer's CPU time or cycles per frame, not the frame
  rate:
  - at the cap the frame rate does not move;
  - uncapped it follows whichever thread or chip is limiting.
  - `MW2_REPORT=1` prints the consumer's time ("ms executing ring batches").
  - `perf stat -t <tid>` on the "gpu ring consum" thread gives cycles and
    instructions. Instructions are nearly free of noise; cycles are not.
  - For the recompiled code, the guest threads' cycles (every "guest thread" and
    the main thread) at the 60 fps cap, where they do the same work each second.
    Divide by the consumer's cycles in the same run: the consumer's work is the
    same in every variant of the guest code, and the two drift together, so the
    ratio's spread is about 2% where the raw cycles' is about ±3.5%.
- **Interleave.** Run base, variant, base, variant, and believe only a difference
  larger than the spread of the base runs:
  - the machine is a personal one, and background load varies;
  - run-to-run spread is about ±2–3%, so a change worth under 2% needs four or more
    interleaved runs each;
  - the laptop reaches 95 °C in back-to-back runs and drifts about 2% a run, so wait
    for `thermal_zone0` below 70 °C before each run;
  - the first run of a series is a few percent faster on every GPU pass: never
    compare a lone first run.
- **Replays.** These give the same content every run ([switches.md](switches.md#moving-the-player)):
  - the campaign flights: Team Player (`roadkill-flight`) and S.S.D.D.
    (`ssdd-flight`), `diagnosis/bench/flight.sh`;
  - multiplayer walks with `tools/padwalk.py`;
  - standing at a level's start with `MW2_CONSOLE`.
  - Multiplayer routes with random spawns are no use for A/B comparisons, because
    the detours at the end differ.
  - Lost frames vary 50% run to run: report them, don't compare them.
- **Out of sample.** A profile-guided build (PGO, BOLT) is measured on content its
  profile was not recorded on. Measured on its own training run, it reads several
  times better than it is.
- **Diagnostics and release.** A diagnostics build measures where time goes (the
  census, `MW2_TIME_RENDER`, `MW2_TIME_PASSES`). Its counters are real work, so a
  claim about players is checked on a release build.
- **Correctness of an optimisation that skips work.** Each one is checked against
  the work it skips:
  - a 60-frame GPU capture replayed with `tools/replay_diff.py` gives identical
    draws and frame hashes;
  - or an in-run check redoes the work and compares (`MW2_CHECK_TEXTURE_WATCH`,
    `MW2_CHECK_TEXTURE_PARTS`).

## In the code

### Build

| What | Why | Measured |
|---|---|---|
| x86-64-v2 target | Below SSSE3, each of the recompiled code's ~7,900 big-endian vector loads, stores and permutes (`pshufb`) becomes sixteen byte moves | Game threads −33% CPU. x86-64-v3 (AVX2) measured no better, so v2 keeps older processors |
| Registers as locals (`*_as_local` in `config/*.toml`, `register_flow.cpp`) | A guest register held in the context struct is a memory load and store per use. As a Rust local, the compiler keeps it in a host register. Whole-program data flow finds the ~140 functions per executable that must keep the context (funclets, `setjmp` callers, writers of a value a caller reads after the call) | Team Player process CPU 250% → 224–230%. Four players pinned to 4c/8t, uncapped: 61–62 → 67–71 fps |
| Return address only where it is read (release builds: XenonRecomp's `--minimal-lr`) | Every guest call stored its return address to `ctx.lr`, which only hooks and kernel imports read. `cargo xtask` lists the functions of the title's table in `runtime/title.h`, which every hook is one of, and a call writes `ctx.lr` only when it can reach one of them, an import or an indirect call through tail calls. A diagnostics build writes it everywhere, since the pacing trace walks guest frames | LR stores 64k → 10k. Guest-thread cycles ÷ consumer cycles −3.3% (4 runs against 8; raw −5.7%) |
| `codegen-units = 1`, `panic = "abort"` | One unit lets the compiler inline across the whole crate. Aborting removes unwinding paths from every call | Not measured separately |
| ThinLTO for the C++ (`MW2_LTO`) | Optimises the launcher's C++ and the diagnostics C++ across files. The game's release build compiles no C++ | Not measured on the present code |

### Guest memory and the frame arena

| What | Why | Measured |
|---|---|---|
| Write watch and shadow of guest memory (`memory_watch.rs`) | A draw's vertex window has to reach the GPU as it was at the draw. Pages the title hasn't written since they were copied are read from a GPU-visible shadow instead of being copied again. Write-protecting a page faults once on the first write, which is how Xenia finds out | Vertex bytes copied per frame: 27.7 → 1.2 MB. Texture signings 110k → 3k a run |
| Frames overlap (3 frame slots) | The GPU works on one frame while the consumer prepares the next | Route 31 → 35–36 world fps when introduced |
| A resolve submits only when its arena share is half full | Submitting at every resolve (about 6 a frame) made the CPU and GPU take turns | Fence waits 12 s → 2.4–3.6 s a route run |
| Vertex constants by changed 16-constant block (constant table) | 90% of the constant bytes copied per draw were unchanged | Constants 25 → 13 MB a frame (4 players); consumer −4% |
| Pixel constants by the blocks a program names (`constant_blocks_read`) | A pixel program reads 3–4 of its 16 blocks, and the whole window was copied | Pixel windows 3.1 → 0.7 MB a frame; consumer −3.6% |
| Fetch block assembled in ordinary memory and copied across once | Reading it back in place from the write-combined arena, which is uncached, cost a fifth of the consumer's time on Team Player | Part of S.S.D.D. 13.0 → 10.6 ms a frame, with the last-shader and last-texture-set shortcuts |
| Fetch block holds only the pairs the shader reads (`fetch_pairs`) | The whole block was 1.5 KB a draw | Part of the −13% below |
| Index swap by hand-written AVX2 (`index_swap.rs`) | The compiler's loop for x86-64-v2 split each store to the write-combined arena in two. Aligned 32-byte stores and one overlapping tail step avoid that. It is chosen at run time where the processor has AVX2 | With the decode fast path: instructions −5.6%, cycles −2%, branch misses −9% |
| Images suballocated from 64 MB blocks (`image_memory.rs`) | The driver's own allocator took over 0.5 ms to free one allocation with 40,000 live | — |
| Image memory block made ahead | Making a 64 MB block takes about 10 ms, which cost a frame when a new texture needed one at a draw | Spikes of 7–14 ms at a draw in play → none. Cost: one idle 64 MB block |
| Written idle images released | Otherwise a streaming level keeps stale copies | 2 GB of stale copies in 7 minutes without it |

### Command processing

| What | Why | Measured |
|---|---|---|
| Register runs written in one pass from the stream (`write_run`, the inline fast path) | A run swaps, compares and stores straight from the command buffer, with no gather unless the run wraps the ring. Runs above the side-effect registers, nearly all of them, take an inline path | See the index swap row |
| Microcode loads (`IM_LOAD`) memoised under the watch | A shader's microcode is copied and hashed again at every load; unchanged pages make the hash known | Part of the −4% squeeze |
| Register stamps only on a changed value | Draw-state memos compare stamps; a rewrite of the same value would invalidate them | Part of the slot memo gain |
| Texture slot memo, recent-texture cache, watch memo | 81% of slots keep their texture from draw to draw. The recent cache halves lookups. A group of 64 pages whose leave-the-watch count hasn't moved needs no page walk | Squeeze 2 (with fetch pairs and the memos): consumer −13% cycles a draw, four players pinned 67.7 → 71.5 fps. Watch memo and slots: instructions −1.5% |
| Last-pipeline and last-targets memos | Consecutive draws mostly share them | Part of squeeze 2 |

### Recording and the GPU

| What | Why | Measured |
|---|---|---|
| Vulkan calls on a recorder thread | Recording was 13–14% of the consumer's draw time, mostly inside the driver, and needs nothing from guest memory, unlike the parse, the arena copies and the texture reads | In play 53.6–56.8 → 58.8–59.5 world fps |
| The recorder sleeps on a wake counter | A wait that spun first used about 60% of a core | Recorder 80% → 39% of a core |
| Bindless textures; the per-draw record found through the base vertex; no push constants (`spirv_builtin.rs`) | Push constants cost the driver a cache miss per bound shader per draw (~30% of the recorder). Descriptor-set binds were another ~8% | Recorder 2835 → 994 cycles a draw. Four players pinned 70.5 → 73.3 fps |
| Draws gathered into `vkCmdDrawMulti*EXT` | 97% of draws form runs, ~30 a call | Included in the line above |
| Straight-line shaders (no `loop { switch (pc) }` when the program has no jumps) | RADV does not remove the dispatcher itself. 88% of pixel and all vertex shaders qualify | VGPRs −31% (pixel) and −52% (vertex). S.S.D.D. at `MW2_SCALE=3`: 61.6 → 51.1 ms a frame. Unchanged at 1x (CPU-bound) |
| Every shader compiled before play ([rendering.md](rendering.md#every-shader-compiled-before-play)) | A pipeline compiled on a cold cache takes tens of milliseconds, and a level has thousands of them | — |
| Record-word loads at the nearest common dominator of their uses | Loading every word in the entry block spills scalar registers (1,089 pixel shaders); other drivers lack RADV's sinking pass | Dispatcher pixel shaders: −6% instructions, −17 to −20% scalar loads. Shader build −4 to −6% CPU. Frame GPU time under 1% (S.S.D.D. 720p −1.0%) |
| Resolves straight into the target image; depth copies of the depth aspect only | A multisampled resolve went through a twin image and a copy. The stencil copy is never sampled | 1080p MSAA, S.S.D.D.: 54.9 → 60.4 fps, GPU 17.7 → 16.1 ms a frame. 720p 129.9 → 139.4 fps |
| A streamed texture read again only in the parts written (`refresh_in_parts`) | One 2 MB streamed texture in Team Player is rewritten ~23 KB at a time, and each fill cost a full untile, sign and stage. About 1% of its pages change between reads: a re-read reads about 3 of its 256 parts | Team Player standing: textures 9–10% → 2% of the consumer. Flight 10.7–11.1 → 9.2–10.2 ms a frame |

### The launcher and the build

| What | Why | Measured |
|---|---|---|
| Both executables' images hashed on two threads | Decrypting and hashing an image takes about half a second, before the first frame | The launcher starts half a second sooner |
| The `install` crate optimised in build scripts too | Build scripts compile unoptimised, and hashing 128 MB there ran at every launcher rebuild | 4 s → a fraction of a second |

### Settings and pacing

| What | Why | Measured |
|---|---|---|
| Field of view 65 by default (`MW2_FOV`) | A wider view draws more of the world | S.S.D.D. flight: 13.2 ms a frame at 65, 14.9 ms at 90 |
| The audio driver writes three frames ahead | Under Wine with every processor busy, a one-frame lead underruns | One frame ahead: about 15 underruns a minute. Two: none |

### Off by default

| What | Why off | Measured |
|---|---|---|
| Copy thread (`MW2_COPY_THREAD=1`, `copy_worker.rs`) | It moves the vertex copies, whose loads come from RAM, off the consumer. But on a processor whose cores share a power budget (this laptop, the Steam Deck), the extra busy core lowers the consumer's clock as much: 2.9 instead of 3.2 GHz | Consumer cycles a frame −6.3%. Uncapped fps unchanged (98.6 / 99.7 / 99.2) |

## Not used

| What | Measured | Why not |
|---|---|---|
| Rust ThinLTO across crates | Out of sample, mp_afghan walks: consumer 4.33 → 4.13 ms CPU a frame, other threads 10.63 → 10.30. Campaign flight unchanged | Within run spread (2–4%). Fat LTO fails: a hook's strong symbol over a weak guest one is refused in a merged module |
| Rust PGO | Same walks: 4.22 ms and 10.32 ms, which is −2.5% and −3%. With ThinLTO, −2% and −3.5%: they don't add up. In sample, at the spawn it was recorded on, it read −7% | Within spread; a profile per title, version and system, recorded again as the code changes |
| C++ PGO | −13% consumer when the runtime was C++ | No C++ is left in the game's release build |
| BOLT (`llvm-bolt`, sampled profile: no LBR) | Consumer cycles at the cap: Team Player (in sample) −1.6%, S.S.D.D. (out of sample) −4.7%, 7 of 10 pairs lower. I-cache misses −5 to −12%; op-cache misses unchanged at 36% | The size of Rust PGO, at the noise level, and a new tool in the build |
| Argument registers (r3–r10, f1–f13) as locals, written to the context only before a call whose callee reads them and at a return a caller reads | Context stores to r4–r10 149k → 65k, but 9% more instructions: imports and indirect calls take all of them, and values kept across calls are spilled by the host compiler. Guest cycles ÷ consumer +2.9% | Slower. An import's or an indirect target's registers would have to be known for it to pay |
| Guest calls straight to `__imp__sub_X` (past the weak hook alias) for functions no hook replaces | −1.9% guest cycles ÷ consumer; with Rust ThinLTO, 18% fewer calls, 12% more code and −2.2% | Within spread |
| Fewer flush-mode checks (none on `fmr`, `lfd`, `stfd`, `fneg`, `fabs`; the mode kept across a call that cannot change it) | Checks 21.5k → 15.1k; −0.8% | Within spread: a check that finds the mode right is a load, a test and a branch |
| `lvlx` + `lvrx` 16 bytes on + `vor` as one unaligned load | 54 places match; −1% | Within spread |
| Vector shuffle tables addressed directly instead of through the GOT | 496 → 0 pointer loads; −0.6% | Within spread |
| Huge pages for guest memory | TLB misses cost about 2–4% of the consumer's cycles | The write watch protects 4 KB pages |
| Consumer split in two threads (parse, then prepare) | Four players pinned: +3.5% fps (65.6 → 67.8) for 0.45 more of a core | The prepare thread stays at 100% |
| Multi-threaded recording; HLE of D3D9 | Recorder 72–86% while the consumer was at 100%. The D3D library and the parse are ~0.4 core, none of it on the limiting thread | Their ceiling is below what they would cost to build |
| Wait handoff in the kernel objects (the signaller wakes one waiter) | Uncapped 48 fps instead of 64 | A lock convoy: an auto-reset event handed to a sleeping thread leaves the consumer waiting on the game |
| Waiters woken after the dispatcher lock is let go | fps unchanged; CPU −2.7%, inside noise | No gain |
| Index buffers reused under the watch | 53% of index bytes reused, but consumer cycles a draw +8% (69.7 vs 71.5 fps) | The map lookups cost more than the swap |
| Six frame slots instead of three | Worse | A heavy multiplayer submission needs 87 MB of arena, so six shares overflow |
| Indirect draw batches | Same recorder gain as multi-draw | Extra consumer writes |
| `RADV_PERFTEST=bo_list` | No recorder change | — |
| Texture slots kept across segments | Lookups stayed at 18% of bindings | Draws really bind different textures |
| Dense record alignment (4 bytes; 16 for vec4 blocks) | Arena peak −7 to −15%; no CPU or GPU change | Same bytes written |
| Fetch block as one span of pairs | +1.4% consumer instructions | 78% of blocks span pairs 25–95: the same bytes |
| Cached texture handle | −1.3% instructions, nothing measurable | The handle is 0.7% of the consumer |
| Depth sampled in place instead of resolved | At most −0.26 ms GPU at 1080p, and the sampled usage costs 0.1–0.2 ms back | Both sun-shadow cascades share one EDRAM rectangle resolved to different rows; MSAA needs a multisample image kind and ~160 MB |
| Stencil resolved with depth | +0.2–0.3 ms a frame at 1080p | The stencil is attached again unresolved where the driver allows |
| Prefetching the command stream 256 bytes ahead | No gain | Decoder loads hit L1 96% of the time |
| Vertex copies reused until the title next sees progress | 1.1 MB of 10.7 MB copied a frame, about 1% of the consumer | Most repeats cross a fence or an interrupt |
| Vertex copies reused while the OS reports their pages unwritten (userfaultfd write protection with `PAGEMAP_SCAN`; `GetWriteWatch` on Windows) | Catches 4.7 of the 5.0 MB of repeated windows a frame, exactly. But each check is two ioctls (both physical windows) of 1.6 µs: 4.8 ms a frame against ~0.35 ms of copying saved | The query costs more than the copy |
| Vertex copies reused when a hash of the window matches | Estimated 2–3% at best | The hash reads every byte the copy would |
| Pixel shaders reading constants from a table | Only 1,230 of 12,322 copied blocks change a frame | A table read in every pixel shader is a GPU cost; not built |

## Recording a Rust profile

None is built (see [Not used](#not-used)). These are the constraints a future attempt
has to respect:

- **A profile fits one build.** The compiler finds a function's counts by its mangled
  name. A Rust symbol carries a hash of its crate's identity, which cargo derives
  from, among other things, the enabled features and the target. So the diagnostics
  build, the release and a Windows build name the same function three ways, and a
  profile recorded on one finds nothing in the others. A release profile is recorded
  on a release build.
- **Training a release build.** Of the run switches, only the console script
  (`MW2_CONSOLE`) works in a release: the flights, walks, input script and quit on
  arrival are diagnostics only. So a release is trained by `tools/padwalk.py`, which
  drives the game from outside and ends it with SIGINT, or by hand.
- **Flags.** Set `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS` around
  `cargo xtask build --release`. Cargo adds it to `.cargo/config.toml`'s and the
  xtask's flags, whereas `RUSTFLAGS` would replace them.
- **Writing the profile.**
  - The training build has `-Cprofile-generate` and links an object defining
    `__wrap__exit`, which calls `__llvm_profile_write_file()` and then `_exit`, with
    `-Wl,--wrap=_exit`. The game ends through `_exit`, which skips the writer.
  - LLVM's continuous mode (`%c`) writes a truncated file with rustc 1.97.1.
  - Merge the counts with rustc's own `llvm-profdata` (the `llvm-tools` component;
    clang's is another LLVM version) and read them back with `-Cprofile-use`.
- **With ThinLTO, train with ThinLTO.** A profile recorded without it leaves 1,730 of
  the game's functions without counts. A crate-local function's counts appear to be
  keyed by its codegen unit's name, which `lto` changes.
- **Joining the C++'s ThinLTO** (`-Clinker-plugin-lto`) needs clang and lld from the
  LLVM version rustc uses (22 for rustc 1.97.1). The build's clang is 18.
- **Each title, version and system needs its own profile**, recorded again as the
  code changes.

## Where the time goes

**The consumer** (release build, standing at Team Player's start, `perf` on its thread):

| Function | Share |
|---|---|
| `memmove` | 17% (the vertex-window copy ~9%) |
| `execute_draw` | 15% (about 30 steps of under 1% each) |
| `write_registers` | 11% |
| `swap_indices16_avx2` | 9% |
| `execute` (packet decode) | 8% |
| `write_constants` | 7% |
| `textures_for` | 6% |
| `write_constant_table` | 4% |

Per frame:
- ~11 MB of vertex windows copied, nearly all from pages the watch backs off because
  the title rewrites them every frame;
- ~9,800 index swaps of about 600 bytes;
- ~190,000 constant dwords decoded.

Front end:
- IPC 1.19;
- 16.5M i-cache misses a second;
- op-cache misses 38%;
- the misses spread over all the draw code.

`MW2_REPORT=1` in a diagnostics build prints the arena census (`arena_census.rs`),
which counts these copies by kind and why.

**The GPU** at 1080p (960×540 doubled, MSAA), S.S.D.D., about 16 ms a frame:
- the world pass is about 6.4 ms;
- depth resolves about 3.2 ms (the 2048×2048 shadow map's copy 1.4 ms, the scene's
  multisampled depth 0.56 ms);
- colour resolves about 1.1 ms;
- the shadow-map passes 1.6 ms;
- copies and clears between passes 1.2 ms;
- uploads 0.35 ms.

`MW2_TIME_PASSES` prints this table. The shadow copy is bandwidth (32 MB a frame):
only sampling the target in place would remove it.
