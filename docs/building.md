# Building from source

The retail Modern Warfare 2 disc's executables, as title update 6 patches them,
are converted to C++ with
[XenonRecomp](https://github.com/hedge-dev/XenonRecomp), compiled as native
x86-64, and linked with a host runtime that stands in for the console's kernel,
GPU (through Vulkan) and audio hardware ([runtime.md](runtime.md)).

The game data and the generated C++ and Rust are gitignored, and XenonRecomp-rs is a
submodule at a pinned commit. Everything else under version control is hand-written
or a patch.

## Requirements

- `cmake`, `ninja-build`, `clang-18`, `lld-18`, `llvm-18` (`llvm-nm-18` and
  `llvm-objdump-18` for the hook check, `llvm-profdata-18` to record a profile), `git`,
  and rustup, which installs the compiler and the Windows target
  `rust-toolchain.toml` names. `python3` for the tools and the rigs only.
- The Vulkan headers and loader (`libvulkan-dev`). Without them the runtime
  builds with no window.
- SDL3 and Dear ImGui are fetched; SDL3 is linked statically (in a Cargo build, built
  from the `sdl3-src` crate's copy by `sdl3-sys`); on Linux it needs the usual
  development packages for its video, audio and input backends (X11, Wayland,
  ALSA/PulseAudio/PipeWire, udev, dbus). `.github/workflows/release.yml` lists
  them.
- The retail ISO, or at least `mw2/tu0/default.xex` and
  `mw2/tu0/default_mp.xex`.
- Title update 6: its package, or its files in `mw2/update/`
  ([Title update 6](#title-update-6)).

## macOS (Apple silicon)

`cargo xtask build` builds and runs the game natively on an arm64 Mac, on MoltenVK
with CoreAudio. It needs Xcode's command line tools (Apple's clang and linker, whose
ThinLTO the build uses), rustup (which fetches the compiler `rust-toolchain.toml`
pins), and `brew install cmake ninja vulkan-headers vulkan-loader molten-vk`.

What differs from Linux, so that the other two are not touched:

- The guest code's vector instructions are the x86 intrinsics of `ppc_rt`, which
  `XenonUtils/rust/neon.rs` implements in NEON for arm64 with x86's results
  (`XenonTests/rust_backend/neon_semantics.rs` checks each against the manuals), and
  `sync`, `lwsync` and `eieio` are real fences there, where the title's locks and ring
  buffers need them. Rounding and flush state are FPCR's.
- Mach-O has no weak alias, so each guest function `sub_X` is a weak definition that
  jumps to `__imp__sub_X`; a hook's strong `sub_X` replaces it as on Linux.
- The window belongs to the process's first thread (`platform::run_main`): the game
  runs on a thread of its own, and the presenter is a job the first thread serves.
- The host's page is 16 KB, which is what the GPU's memory watch protects, and a write
  to a read-only page arrives as SIGBUS as often as SIGSEGV.
- A thread macOS starts gets 512 KB of stack, so the runtime's threads ask for 16 MB.
- Not there yet: Steam (the game plays without it; LAN and relays work), `MW2_WATCH`
  (it steps with x86's trap flag), the PGO profile (x86 counts), and `--cpp`.

## cargo xtask

`cargo xtask build`, at the top of the tree, does everything, in order
(`xtask/src/main.rs`):

1. checks out the [XenonRecomp-rs](https://github.com/PaulCombal/XenonRecomp-rs)
   submodule and its own, when `git clone --recursive` did not, and stops if it is
   neither at the commit the tree records nor at a later one of its history;
2. builds XenonRecomp and XenonAnalyse into `out/xenonrecomp/`;
3. extracts `default.xex` and `default_mp.xex` from the ISO into `mw2/tu0/`,
   and every `.ff`, `.pak` and `.bik` into `mw2/game/` (`xtask/src/xdvdfs.rs`;
   files already there are kept); then applies the title update, which writes the
   executables the rest works on, `mw2/default.xex` and `mw2/default_mp.xex`
   ([Title update 6](#title-update-6));
4. writes the flat PE image (`mw2/default.pe` or `mw2/default_mp.pe`) with
   `xtask/src/xexdump.rs`;
5. finds jump tables with XenonAnalyse (`config/mw2_switch_tables.toml`,
   `config/mw2mp_switch_tables.toml`);
6. recompiles into `ppc/` or `ppc_mp/`, and, unless `--cpp`, as Rust into `ppc_rs/`
   or `ppc_mp_rs/`, pointing the `generated` link at it;
7. generates `ppc_recomp_shared.h` and the kernel import stubs
   (`xtask/src/generate.rs`);
8. builds the game with cargo and the rest -- the launcher, the tools, the rigs' C
   libraries -- with CMake ([Cargo](#cargo)); with `--cpp`, all of it with CMake.

`cargo xtask prepare` stops after step 4, `cargo xtask recompile` after step 7. A
step does only what is not done yet, but the recompilation, which runs every time
unless `--keep-generated`. The xtask is a workspace of its own, with nothing but std,
run through the alias in `.cargo/config.toml`: cargo cannot read the game's workspace
before the guest code it depends on is generated.

| option | effect |
|---|---|
| `--title sp\|mp` | `sp` (default): `default.xex`, the campaign and special ops. `mp`: `default_mp.xex`, the multiplayer. Each has its own recompiled tree, switch tables, recompiler config and build directory |
| `--tu <path>` | the title update: its package, or a folder holding its files. Needed until `mw2/update/` has them |
| `--tu0` | builds the disc's own executables, without the update; see [The disc version](#the-disc-version) |
| `--release` | builds without diagnostics (the `diagnostics` feature off, `-DMW2_DIAGNOSTICS=OFF`) and, on Linux, with the C++ runtime linked statically (`portable`, `MW2_PORTABLE`); adds `-release` to the build directory |
| `--windows` | cross-compiles for Windows with llvm-mingw; the build directory becomes `build-win...` and the executable `mw2.exe` |
| `--llvm-mingw <dir>` | the unpacked [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) release, required by `--windows`; `LLVM_MINGW` in the environment names it too |
| `--iso <path>` | the disc image. Without one, `mw2/tu0/default.xex` and `mw2/tu0/default_mp.xex` are enough to build, and no game data is extracted |
| `--build-dir <dir>` | builds there instead of the default directory |
| `--targets "<target> ..."` | builds only those targets (`mw2`, `launcher`, ...) instead of all of them |
| `--keep-generated` | keeps the recompiled trees already there (skips steps 5-7), for a second build of the same title |
| `--release-tag <tag>` | the release the build is published as (`v0.3.0`): the launcher looks for a newer one than it. Without it the launcher does not |
| `--cmake-extra <arg>` | one argument for CMake's configure step (a compiler launcher, `-DFETCHCONTENT_BASE_DIR=...`), passed as it is, not split at spaces; repeated for more |
| `--cpp` | builds the game from the C++ tree with CMake, for as long as the C++ build exists |
| `--renderer rust\|cpp` | `rust` (default): the renderer's files Rust has taken over are its. `cpp`: the C++ renderer whole, with the Rust runtime; adds `-cppgpu` to the build directory. Not with `--cpp` ([The renderer moves to Rust](#the-renderer-moves-to-rust)) |

Build directories:

| | campaign | multiplayer |
|---|---|---|
| diagnostic | `build/` | `build-mp/` |
| `--release` | `build-release/` | `build-mp-release/` |
| `--windows` | `build-win/`, `build-win-release/` | `build-win-mp/`, `build-win-mp-release/` |
| `--renderer cpp` | each of the above with `-cppgpu` (`build-cppgpu/`) | `build-mp-cppgpu/`, ... |

A second build directory reuses the SDL3 and FFmpeg sources `build/` fetched.

### Cargo

`cargo xtask build` writes the guest code as Rust into `ppc_rs/` or `ppc_mp_rs/`,
points the `generated` link at it, and hands step 8 to cargo. Once that is done,
`cargo build --release` at the top builds the same title again, into `target/`. The
C++ tree is still written: the headers and the kernel stubs come from it.

| file | what |
|---|---|
| `Cargo.toml`, `.cargo/config.toml`, `rust-toolchain.toml` | the package, the target CPU, the linker, and the compiler version |
| `build.rs` | compiles the renderer's C++ with clang and gives rustc its link line |
| `src/main.rs` | the executable's `main`, which hands the command line to `src/runtime/run.rs`, the run from start to end |
| `src/runtime/` | the runtime's modules ported from C++ |
| `src/guest.rs` | what a hook sees of the guest: registers, memory, functions, `guest_hook!` |
| `src/hooks/` | the hooks ported from C++ |
| `src/title.rs` | `runtime/title.h` as Rust, written by `build.rs` |
| `src/env.rs`, `src/log.rs` | the switches and the log, through the C++ runtime's C library and `stderr` |
| `generated`, `generated.title` | the link to the guest code in use, and its title and version (gitignored) |
| `xtask/` | the build from the disc to the executables ([cargo xtask](#cargo-xtask)) |
| `xtask/src/check_hooks.rs` | after the link, that every hook won it |
| `tools/hook_diff.py`, `tools/hook_diff.cpp` | a ported hook against its C++ |
| `tools/runtime_diff.py`, `tools/runtime_diff.cpp`, `tools/runtime_diff.rs`, `tools/runtime_diff_online.cpp` | a ported module against its C++ |
| `tools/kernel_diff.py`, `tools/kernel_diff.cpp`, `tools/kernel_diff.rs` | the kernel against its C++ |
| `tools/xma_diff.py`, `tools/xma_diff.cpp`, `tools/xma_diff.rs` | the XMA decoder's contexts against their C++, on recordings of real runs |
| `tools/inflate_diff.py`, `tools/inflate_diff.rs` | the game's gunzip against zlib's, on every image's XLAST, made streams and damaged copies |
| `tools/font_diff.py`, `tools/font_diff.rs`, `tools/font_diff.cpp` | the screens' font rasteriser against stb_truetype |
| `rustfmt.toml` | the width for `cargo fmt`; `rustfmt` and `clippy` come with the pinned toolchain |

Generating has to come first: cargo reads the crates before any `build.rs` runs,
so a `build.rs` cannot make them. `build.rs` compiles the C++ the game still has --
`runtime/gpu/`, `image_move.cpp`, `shader_preload.cpp`, `rust_calls.cpp`, the
generated kernel stubs and the guest code's fallback -- as CMake compiled it: clang-18
(llvm-mingw's for Windows) run with the same flags and nothing from the environment,
`-O3`, `gnu++20`, `-march`, ThinLTO and the profile in `pgo/`, and the definitions and
include folders these files use. A file is compiled again only when it, a header it
includes (clang's depfile) or its flags changed, as many at a time as cargo gives
jobs; clang's warnings come out as cargo's. A Windows build takes the Vulkan headers
of the build machine (`libvulkan-dev`, or the folder `MW2_VULKAN_HEADERS` names), which
must be the version `cmake/vulkan-windows.cmake` fetches for the C++ build. It writes `link.rsp`, which rustc
passes to the linker, and a copy beside `build.txt` in the folder `MW2_BUILD_DIR`
names, for the hook check. Every other `runtime/*.cpp` is the C++ build's
alone. CMake builds the rest with `-DMW2_RUST=ON`, which leaves its own game out of
`all`: the launcher, the tools, and FFmpeg's decoder and zlib for the rigs.
`cargo xtask` keeps its build directories, with CMake's files where they always were,
cargo's in `<build>/rust/`, and the executable copied to `<build>/mw2`.

Three things hold this together:

- **The runtime is linked as objects, not an archive.** A strong `sub_X` in an
  archive is not taken out of it to replace the guest code's weak one, so the
  game would link and run the original function. A hook in Rust has the same
  problem in a library crate: put it in the `mw2` crate or check it is pulled in.
  `xtask/src/check_hooks.rs` fails the build for one in the runtime's objects, and
  counts the ones from Rust.
- **The linker is clang-18's lld**, named in `.cargo/config.toml`. Otherwise rustc
  brings its own, on a newer LLVM than the one that compiled the runtime, and that
  would do its ThinLTO. rustc does not take part in ThinLTO: its code arrives as
  machine code. It has to be lld in every build, ThinLTO or not: rustc puts its
  system libraries ahead of `link.rsp`, among them `-lgcc_s`, whose linker script
  brings the `libgcc.a` that `-nodefaultlibs` would otherwise leave out. lld takes
  them in any order.
- **A target folder holds one title.** The two titles' crates have the same path,
  through `generated`, so cargo would call the other's up to date and link it.
  `build.rs` stops such a build. The xtask's directories are separate anyway;
  with plain cargo, set `CARGO_TARGET_DIR` for each title.

`main` is `#![no_main]` so that nothing runs before the runtime as it would under
Rust's own start, which installs signal handlers and ignores SIGPIPE.

A hook is written like this, and `src/hooks/` has `deny(unsafe_code)` over it:

```rust
#[cfg(T_DB_StartStream)]                    // in hooks/mod.rs: the C++'s #ifdef T_x
guest_hook!(T_DB_StartStream, |ctx, memory| {
    let name = memory.u32_at(T_DATA_DB_Load).wrapping_add(8);
    log_info!("fastfiles: %s", memory.text(name));
    T_DB_StartStream.original(ctx, memory);  // GUEST_ORIG; .call() is GUEST_FUNC
});
```

- A hook's C++ file keeps its name in `runtime/`, for the C++ build until it goes,
  and the Cargo build does not compile it.
- `runtime/title.h` stays the one table. A name that is not in this build's block
  has no `cfg`, as it has no `#define`, and a `cfg` of a name in no block warns.
- A log line is printf: `log_warn!` checks the format against its arguments when
  it compiles, and the C library formats it, so that `%g` and a `%s` of the guest's
  bytes read as they always have. It supports `%g`, `%.2f`, `%s`, `%.64s`, `%d`, `%u`,
  `%#x`, `%08X`, `%llu` and `%p` so far.
- A port keeps what C++ did at the edges. `f64::min` gives 170 for a NaN where
  `std::min` gives the NaN, `str::parse` is not `strtod`, and a comparison of a guest
  string stops at the first difference, as `strcmp` does. Arithmetic on anything
  from the guest wraps by name (`wrapping_add`), as a C++ `uint32_t` does by itself:
  with `+`, what a hostile peer sends would panic a `cargo build`, whose overflow
  checks are on.
- `xtask/src/check_hooks.rs` knows how many hooks each build has, so a hook lost on the
  way to Rust fails the build.
- `tools/hook_diff.py <build folder>` runs each ported hook and its C++ on the same
  random guest states, in one program, with the title's functions as stubs, and
  compares the registers, all 4 GB, every call out and the log. It is how a hook is
  checked that only a hostile peer's packet reaches: every hook of `src/hooks/` with
  a C++ file of the same name is covered. `--coverage` lists the C++ lines no case ran, and `--src` takes the
  Rust from a copy with a deliberate mistake in it, which a run has to catch.
  `--hook` takes one hook or several, separated by commas. The custom maps' hooks get
  what random memory never makes: a game folder of generated `.arena` files for each
  seed, a bonus menu of map buttons, and Com_LoadUiFastFile's zone list; both sides
  allocate from the kernel's heap emptied before each (`--cfg mw2_hook_rig`), so
  they are given the same addresses. `HOOK_DIFF_SHOW_LOG=1` prints each case's log.
  A renderer hook (a file of `runtime/gpu/`, `image_move.cpp`, `shader_preload.cpp`)
  is paired only where the build's `renderer_cfgs.txt` says Rust owns its file, so a
  `--renderer cpp` build leaves it out on both sides; the Rust side is then built with
  the `rust-renderer` feature and ash, and what such a hook calls of the renderer is a
  logger on both sides (`tools/hook_diff.cpp`), while the D3D hooks' arena block is
  `arena.cpp` against `arena.rs`. Anything else left unresolved is never called.
- `hooks/com_error.rs` raises the title's error. It takes only the levels that
  cannot return, and returns `!`, so nothing after it runs: the console has no
  instruction there. A `longjmp` then skips the hook's frame, which is why a hook
  holds nothing to drop across a guest call, and never calls the guest inside a
  `Once` or a `LazyLock`, which would stay in progress for ever.
- No hook may panic. A panic aborts here, by `panic = "abort"`, which `main.rs`
  refuses to compile without, and `cargo build` (without `--release`) is the way to
  have this crate's `debug_assert!`s, overflow checks and `guest_hook!`'s check of
  the guest's stack, with the guest code and the C++ as optimised as ever. The
  result is `target/debug/mw2`.

The runtime moves module by module, and C++ keeps calling what has moved, `gpu/`
above all:

- A module is `src/runtime/<name>.rs`, safe code under `deny(unsafe_code)`. Its C++
  callers keep `runtime/<name>.h`, whose functions are now inline and call the
  `extern "C"` ones `src/runtime/exports.rs` gives, so no caller changes. `exports.rs`
  turns C's pointers into safe types. What is the system's or a C library's is unsafe
  by nature and kept to itself: `guest_memory`, `platform`, `windows`,
  `install/spawn`, `cpu_level`, `crash`, `fault_text`, `watchpoint`, `scan`,
  `online/udp` and `online/steam/client`.
- A module's C++ file stays for the C++ build, with the same `extern "C"` names, and
  the Cargo build does not compile it. The launcher
  compiles `install/crypto.cpp`, `install/xex.cpp` and `install/spawn.cpp` itself, so
  those stay with it.
- Neither language frees what the other made. Text and bytes go back through a sink
  that copies them (`runtime/text_sink.h`).
- What the C++ calls for every draw or every guest pointer stays inline, as Rust is
  outside its ThinLTO and profile. `guest::Base()` reads `mw2_guest_base`, a variable
  of `guest_memory.rs`, and a diagnostic's `On()` is a C++ `static` in its header.
- The Rust runtime calls the C++ one through `runtime/rust_calls.cpp` and
  `src/runtime/bridge.rs`. The run's end, `mw2_crash_request_exit` and
  `mw2_crash_on_fault`, is `src/runtime/run.rs`'s, by C names so that each rig gives
  its own; the renderer's start and end are left out of the rigs' builds.
- `src/log.rs` writes a line whole, with one `fprintf`, to the C library's stderr,
  where the renderer's C++ lines go too. In the Rust build `runtime/log.h` takes
  `MW2_LOG_TIME`'s stamp from `src/log.rs`, so both count from the same moment, and
  `MW2_LOGGING=OFF` silences the Rust's lines as well as the C++'s.
- A log line and a line of a file go through printf, from `log_info!` and
  `format_text!`, with the format checked against the arguments as it compiles, so
  they read as the C++'s did. A route's numbers are read by `sscanf`, in `scan.rs`.
  The launcher reads the log for its bug report, so its text is an interface.
- A module with a clock (`report`, `stutters`, `pacing_trace`, `player`) takes the
  time as an argument, and `report` the machine as a trait, so that the rig can give
  them what it gives the C++. `exports.rs` gives them the real ones.
- A signal handler that returns takes no lock and allocates nothing: `crash` keeps
  its threads in a table it reads as it stands, `watchpoint` has a lock of its own,
  and text goes out through `fault_text`, from the stack, to the descriptor. A run
  that is ending still writes its reports, as a bug report needs them, so a fault
  inside the allocator can hang there.
- SDL comes from the `sdl3-sys` crate, which builds it statically from the source the
  `sdl3-src` crate carries, the version CMake fetches for the launcher, with CMake and
  `cmake/sdl.cmake` as its toolchain file (`cmake/sdl-mingw-w64.cmake` for Windows,
  both named in `.cargo/config.toml`): clang, only the static library, and the options
  the game had when CMake built it. The renderer's C++ takes its headers from that
  build, and rustc links it, with what it asks for. Unlike the rest of the C, it is
  compiled without ThinLTO and the profile. `sdl3-sys` builds it once in a target folder
  and never again, so `build.rs` stops a build whose toolchain files or llvm-mingw
  changed since, and one whose SDL is not the version CMake fetches for the launcher.
  `cargo clean -p sdl3-sys` builds it again, as a newly installed development package
  of one of its backends needs too. Win32's declarations are by hand in
  `runtime/windows.rs`. The game links no zlib and no FFmpeg: the title's XLAST is
  gunzipped by `runtime/inflate.rs` and XMA frames are decoded by `runtime/wmapro/`, and
  both C libraries are built only for the rigs that hold the Rust to them
  (`tools/inflate_diff.py`, `tools/xma_diff.py`). The system's screens draw their
  letters with `runtime/truetype.rs`, the part of stb_truetype (the copy that comes
  with Dear ImGui, which the C++ build's screens use) that bakes a font into an atlas,
  step for step. `tools/font_diff.py` holds it to the C bit for bit -- the atlas, the
  glyphs' boxes and the ascent -- on the launcher's three fonts at every size the
  screens can ask for and every atlas side, on the machine's own fonts, CFF ones
  among them, on fonts it makes to reach every branch, and on damaged copies, where the
  Rust refuses a font wherever the C would read outside it.
- `runtime/online/service.h` declares the C++ build's online service and its
  backends; of the Rust one, `src/runtime/run.rs` asks only for Steam's namespace and the
  log-in. A picture of the system's screens crosses as an `mw2_image`
  (`runtime/screen.h`), its pixels only when they are drawn again.
- The game's side of the relay protocol, `src/runtime/online/relay_wire.rs`, names
  every number and place it reads, and the relay's `cargo test`
  (`relay/tests/game_wire.rs`) holds it to `relay/src/protocol.rs`.
- A constructor that must run first is the C++'s `constructor(101)`, spelled by hand:
  `.init_array.00101`, or `.ctors.65434` on Windows, in a writable static, or the
  linker leaves it out of the table the C runtime walks. `runtime/cpu_level.rs` is one,
  and runs in a crate compiled for the processors it refuses, so `xtask/src/check_hooks.rs`
  disassembles it.
- `tools/runtime_diff.py <build folder>` runs the modules whose answers depend on
  their inputs alone and their C++ on the same inputs in one program, on this title's
  real executables and XLAST (seed 1) and on made-up ones. The modules with a clock
  run one script of events on each side, the C++ on the rig's clock
  (`tools/runtime_diff_clock.h`), and what they logged and answered is compared;
  `--coverage` says which of their C++ lines never ran, theirs and the system
  screens'. The screens (`signin`, `friends`, `guide`) run one script of controllers,
  asks and pictures on each side, with a stand-in service of the rig's for Steam and
  LAN, and each side's profiles file and kept pictures; the pictures are compared
  byte for byte. Neither side has a XAM there: the Rust, built with
  `--cfg mw2_screens_rig`, tells the rig what it tells XAM, as the C++ does. The online service's pure parts -- the connect line, a Find Game
  listing's text, Steam's release check -- are reached in their C++ through
  `tools/runtime_diff_online.cpp`. Threads, sockets, files, signals and the system
  are for real runs.
- A kernel import is `kernel_import!(<Name>, |ctx, memory| ...)` (`src/guest.rs`),
  safe code as a hook is. `tools/kernel_diff.py <build folder>` runs the kernel
  and its C++ on one script of calls in one program, each side on a guest space
  and in a folder of its own with the same game files, and compares every status,
  every word written, the guest memory, what guest threads and APC routines saw,
  the log, and every file and folder each side left: files, saved games, the
  profile, the clocks, the video calls and the controllers as a script stands in
  for them, and XAM and the network with no online service, one that carries
  datagrams, or one with Live's lobbies: both sides talk to the rig's, which
  writes down what it is told and answers from the script (`tools/kernel_diff.rs`
  is its Rust half). Without a service the sockets are the host's, on the
  loopback, and each side's are closed when its part of a case ends. The C++'s
  clock is inlined where the rig cannot replace it, so it is held to the host's
  time and the Rust (built with `--cfg mw2_rig`) to the rig's; random bytes are
  the rig's on both sides.
  A script never blocks for ever or races: threads are made suspended and waited
  for, a timer is waited for only when it is due, and other waits time out. `--stress` checks what
  many threads must keep on each side (no wake lost, nothing taken twice) and
  `--time` what a call and a wake cost, with `--optimised` for release code.
- The XMA decoder's contexts have `tools/xma_diff.py`, which replays what a real
  run recorded (`MW2_XMA_RECORD`) through both sides ([audio.md](audio.md)).

Rust code that builds these crates needs `panic = "abort"` and
`-Ctarget-cpu=x86-64-v2`, set here in the top workspace; the generated workspace's
own settings do not apply. `cargo build --features` chooses `diagnostics` and
`rust-renderer` (both on by default) and `portable`. `MW2_BUILD_DIR` and `MW2_RELEASE_TAG` pass the rest of what
`cargo xtask` decides, and `MW2_LOGGING`, `MW2_LTO`, `MW2_LTO_CACHE`, `MW2_PGO` and
`MW2_PGO_TRAINING` in the environment are the options below, as `ON` or `OFF`;
`cargo xtask` refuses them in `--cmake-extra`, which only CMake's part reads.

### The renderer moves to Rust

The renderer, `runtime/gpu/` and `runtime/gpu/vulkan/`, moves to `src/runtime/gpu/`
a file at a time, each Rust module named after the C++ file it takes over. Until it
is all there, two renderers build from one tree:

| build | renderer |
|---|---|
| `cargo xtask build` | the Rust of every file taken over so far, the rest C++ (feature `rust-renderer`) |
| `cargo xtask build --renderer cpp` | the C++ renderer whole, with the Rust runtime, in `<build>-cppgpu` |
| `cargo xtask build --cpp` | the all-C++ game, as before |

`build.rs` keeps two lists. `RUST_OWNS` holds the C++ files Rust has taken over:
with the feature they are not compiled, and each gives rustc the cfg
`mw2_owns_<stem>` (`mw2_owns_texture` for `runtime/gpu/texture.cpp`). Every Rust
hook, export and `bridge.rs` route that stands in for such a file is under that cfg,
never under the feature alone, so each function exists once in every build, and
taking a file back out of the list unwires its Rust with it. The command processor,
the census and the image pool are the exception: the Rust command processor calls
only the Rust census and pool, so the three leave or stay together, and a build that
lists some of them stops at a `compile_error!` in `src/runtime/gpu/mod.rs`. The C++ glue is told the
same list as definitions, `MW2_OWNS_<STEM>` (`MW2_OWNS_COMMAND_PROCESSOR`): for the
command processor, the census and the image pool, `rust_calls.cpp` routes into the C++
file only while it is compiled and `rust_renderer.cpp` forwards to the Rust only while
Rust owns it; the memory watch the same way (`MW2_OWNS_MEMORY_WATCH`), whose fault
handler `crash.rs` then calls in Rust. `RUST_ONLY` holds the
C++ that exists only beside the Rust renderer: `runtime/rust_renderer.cpp` defines
the C++ declarations of what moved as calls to `src/runtime/gpu/exports.rs`, so no
header and no caller of the C++ renderer changes. Calls from the Rust into the C++
that is left go through `runtime/rust_calls.cpp` and `src/runtime/bridge.rs`. A
`RUST_ONLY` file is wrapped whole in `#if MW2_RUST_RENDERER`, which only `build.rs`
defines, and CMake leaves it out. `build.rs` checks that every listed file exists,
and that what `RUST_OWNS` names is a file it compiles. It writes `renderer_cfgs.txt`
(`renderer rust` or `renderer cpp`, then the cfgs) beside `link.rsp` and `build.txt`
in its OUT_DIR, and `cargo xtask` copies the three into the build folder from the
run cargo used, with that OUT_DIR's path in `out_dir.txt`: one folder can be built
with either renderer, and cargo keeps a build-script run for each.

The C++ renderer stays as it is: the executables of `--renderer cpp` and `--cpp`
come out the same, byte for byte, whatever has moved. The only C++ changes allowed
are diagnostics-only instruments (under `MW2_DIAGNOSTICS`), linkage the rigs need,
and the memory watch's inline read under `#if MW2_RUST_RENDERER`; each is checked
against that identity.

The Rust renderer uses `ash` for Vulkan, linked to the loader by name (`vulkan`, or
`vulkan-1` on Windows, whose import library `build.rs` makes) as the C++ is. A
machine without the Vulkan development files builds the window-less game only with
`--renderer cpp`; `build.rs` says so. `cargo xtask` clears `VULKAN_SDK`, which ash's
build script would add to the link, so the game links the system's loader.

Whatever decides the shaders' SPIR-V is part of the shader build's hash
([shaders.md](shaders.md)), so the translator's Rust folder is in `build.rs`'s and
CMake's lists alike, beside the C++ copy the launcher keeps compiling, which stays
hashed too. Built-in shaders (the backdrop, the display
table, the drawn resolve) are made outside that folder, so they do not change the
hash.

### Title update 6

The update is a package holding a patch for each executable (`default.xexp`,
`default_mp.xexp`) and three fastfiles for the multiplayer (`patch_mp.ff`,
`dlc1_ui_mp.ff`, `dlc2_ui_mp.ff`). After extracting the disc's executables,
`cargo xtask`:

- unpacks the package (`xtask/src/stfs.rs`) into `mw2/update/`, or copies the
  files from the folder `--tu` names;
- applies each patch to the disc's executable with XenonRecomp's own patcher
  (`tools/xexpatch.cpp`, built against its library), writing `mw2/default.xex`
  and `mw2/default_mp.xex`;
- copies the three fastfiles into `mw2/game/`.

The update changes one of the campaign's 11,133 functions, and about 260 of the
multiplayer's 11,579 with about 270 added; nearly every multiplayer function
sits at another address than on the disc. The kernel imports are the same.

A player gets the same thing from the launcher, which applies the update to
the disc's executables at install time ([The launcher](#the-launcher)).

### The disc version

`--tu0` builds the disc's executables as they are. Everything of theirs
carries the version: the image and a game folder in `mw2/tu0/`, the configs
`config/MW2.tu0.toml` and `config/MW2MP.tu0.toml`, the trees `ppc_tu0/` and
`ppc_mp_tu0/`, and `-tu0` on the build directory. The build defines
`MW2_VERSION_TU0`, which selects the disc's blocks in `runtime/title.h`.

    cargo xtask build --title mp --tu0
    ./build-mp-tu0/mw2 mw2/tu0/default_mp.pe mw2/tu0/game

`mw2/tu0/game/` links to everything in `mw2/game/` but the update's fastfiles:
the disc's multiplayer would load a `patch_mp.ff` it found. The installer
works with this version, which is the one a player can install.

The two versions' configs and blocks in `title.h` hold the same functions,
found in the other executable by their instructions with the address-bearing
fields masked out, and the same data, found by what the same instructions of a
matched function form. The multiplayer's client state is 0x200 larger in the
update; the fields the runtime reads kept their offsets.

## Running

A player's copy is the launcher and the two game executables in one folder,
with the game installed into `game/` beside them. A game executable started
with no arguments loads the installed title, or starts the launcher when there
is none ([runtime.md](runtime.md#loading-the-image)).
A development run names the image and the game folder:

    ./build/mw2 mw2/default.pe mw2/game
    ./build-mp/mw2 mw2/default_mp.pe mw2/game

The multiplayer hosts a system-link match to reach a map:

    MW2_NET_LINK=1 MW2_CONSOLE="8:map mp_afghan" ./build-mp/mw2 mw2/default_mp.pe mw2/game

The switches are in [switches.md](switches.md).

## CMake options

| option | default | effect |
|---|---|---|
| `MW2_TITLE` | `sp` | which executable: `sp` or `mp` (defines `MW2_TITLE_MP`, selecting the addresses in `runtime/title.h`) |
| `MW2_VERSION` | `tu6` | which version of it: `tu6`, or `tu0` for the disc's (defines `MW2_VERSION_TU0`) |
| `MW2_DIAGNOSTICS` | `ON` | traces, dumps, statistics, end-of-run reports, the stutter detector and pacing timeline, the flash hunt, the write watchpoint and the headless harness (walker, input script, run deadlines, watchdog); see `runtime/diagnostics.h`. `OFF` unsets every diagnostic switch whatever the environment says |
| `MW2_LOGGING` | `ON` | `OFF` compiles out every log line, warnings included |
| `MW2_TRACE_INDIRECT` | `ON` | the generated code checks an indirect call's target and reports a missing one instead of jumping to null; a target outside the code section is reported in every build |
| `MW2_PORTABLE` | `OFF` | links libstdc++ and libgcc statically (Linux) |
| `MW2_USE_SDL` | `ON` | SDL3 for the window, input and audio |
| `MW2_X86_LEVEL` | `x86-64-v2` | the `-march` the game is compiled for. Below SSSE3 every big-endian vector load, store and permute of the recompiled code becomes sixteen byte moves; `runtime/cpu_level.cpp`, compiled for the baseline, tells a player whose processor lacks the level |
| `MW2_LTO` | `ON` | ThinLTO: every target, the fetched libraries included, optimised across files at the link, with `lld` ([Optimisation](#optimisation)) |
| `MW2_LTO_CACHE` | `<build>/lto-cache` | where the linker keeps ThinLTO's results for the next link |
| `MW2_PGO` | `ON` | compiles with the recorded profile in `pgo/<title>.tar.xz` when there is one (`tu6` only) |
| `MW2_PGO_TRAINING` | `OFF` | builds the executable with counters that a profile is recorded with (`tools/pgo_record.sh`); no LTO, no profile |

In a Cargo build the game's are environment variables of the same names that
`build.rs` reads (`MW2_LOGGING`, `MW2_LTO`, `MW2_LTO_CACHE`, `MW2_PGO`,
`MW2_PGO_TRAINING`); the others do not apply.

CMake reads the SHA-256 of the executables in `mw2/` and `mw2/tu0/` and of the
update's fastfiles in `mw2/update/` at configure time, and `build.rs` the game's own: a game executable runs
only the title executable it was recompiled from, and the launcher installs
only that disc and that update.

### Optimisation

The thread that turns the title's GPU commands into Vulkan calls limits the
frame rate, and its time is CPU time: copies, texture checks, driver calls.
Two compiler techniques take ~13% off it together, measured on the campaign's
flights and, with a profile from the campaign alone, ~9.5% on a multiplayer
route it never ran.

- **ThinLTO** (`MW2_LTO`). Every file is compiled to LLVM bitcode, and the
  link optimises across them: a runtime function inlined into another file's
  draw loop, code nothing calls dropped. A relink reuses the work of the
  files that did not change (`lto-cache/` in the build folder, or
  `MW2_LTO_CACHE`): a result is keyed by everything that went into it, so one
  taken from the cache is the one the link would have made.
- **Profile-guided optimisation** (`MW2_PGO`). A build with counters plays a
  few routes and counts how often each branch went which way and each function
  was called; the real build reads those counts, lays the hot paths out
  straight, inlines the hot calls and moves the cold code aside.
  `pgo/sp.tar.xz` and `pgo/mp.tar.xz` are the counts, one per executable
  (the two share the runtime, not the game code).

`tools/pgo_record.sh` records the two profiles: it builds each title with
counters into `out/pgo-sp/` and `out/pgo-mp/`, flies Team Player and S.S.D.D.
in the campaign and walks Terminal and Karachi in the multiplayer, uncapped,
and writes the archives. It needs the game's data, a GPU and about 15 minutes,
so it runs on a developer's machine and not in the release workflow, which
builds with what is committed. Record again after large changes to the
runtime: each function's counts carry a hash of its shape, and a function
that changed since is compiled without them. An old profile helps less; it
cannot make the game wrong.

`runtime/cpu_level.cpp` is compiled to machine code at once (`-fno-lto`), so
that none of the higher level's code is mixed into the check that runs before it.

## The launcher

`mw2-launcher` (`launcher/`) is built with every build directory, beside
`mw2`. It is one executable: Dear ImGui (fetched, like SDL3) on SDL's own
renderer, three fonts from `third_party/fonts/` written out as arrays at
configure time, and XenonRecomp's patcher compiled in from its checkout with
the LZX decoder and AES it uses. For the compile pass it also has zlib (fetched,
v1.3.1), the zone definitions in `launcher/zone/defs/` written out as arrays
like the fonts, and the game's own shader translator and pipeline code from
`runtime/gpu/vulkan/`.

Its icon is in `launcher/icon/`: `launcher.ico` is linked into the Windows
program as a resource, and `launcher.bmp` is written out as an array like the
fonts and given to the window on both systems. `tools/launcher_icon.py` makes
the two from the campaign's icon in the PC game's `iw4sp.exe`: the left half as
it is, the right half in the multiplayer's colours. `tools/steam_emblem.py`
makes `steam_emblem.h`, the Steam card's emblem, from Steam's own 256-pixel
icon: a coverage mask of its white parts, which the launcher tints. `tools/readme_art.py` draws the
README's banner, tiles and headings into `docs/images/readme/`.

| file | what |
|---|---|
| `main.cpp` | the window's loop, what each entry does, the file dialog and dropped files, and the terminal mode |
| `single.cpp` | one window a copy: a second start brings the first up in front and ends |
| `ui.cpp` | the one screen, drawn by hand: a computed backdrop whose smoke drifts, the column of entries, the pane, the wordmark |
| `setup.cpp` | the install: checks the disc, gets the update, patches the executables, copies the files |
| `disc.cpp` | reads a disc image (XDVDFS) or an extracted folder |
| `package.cpp` | reads the update's package (STFS), as `xtask/src/stfs.rs` does |
| `download.cpp` | one file over HTTPS: `URLDownloadToFile` on Windows, the `curl` program elsewhere |
| `update.cpp` | looks for a newer release and puts it in place of the running programs |
| `report.cpp` | a bug report: a run with its log kept, the system's description, the new-issue page |
| `settings.cpp` | what the player sets for the game, kept in `.env`, which the game reads as it starts |
| `sound.cpp` | the game's two menu sounds, read out of the installed `code_post_gfx.ff` at each start and played as the entries are moved through and chosen; none without an install |
| `profile.cpp` | the profile screen's changes to the files under `saves/` ([saves.md](saves.md#what-the-launcher-changes)) |
| `precompile.cpp` | every shader of the game compiled before it is played ([rendering.md](rendering.md#every-shader-compiled-before-play)) |
| `maps.cpp` | custom maps: added (a PC map converted, a 360 one checked, copied, a picture made from the `.iwd`), put in or out of the game, removed ([gameplay.md](gameplay.md)) |
| `mapport/` | the map converter: a PC map's fastfiles made the 360's ([maps.md](maps.md)); GPL v3, see its `NOTICE.md` |
| `zone/` | reads a fastfile: inflates it (`fastfile.cpp`), walks its assets by the OpenAssetTools zone definitions (`defs.cpp`, `schema.cpp`, `expr.cpp`, `walker.cpp`), and lists its shaders (`shaders.cpp`), with vertex shaders bound as the title binds them (`vertex_binding.cpp`) |
| `zone/defs/` | the zone definitions, from the map converter (`pc2xbox360mw2ools`), which takes them from OpenAssetTools; GPL v3, see its `NOTICE.md` |
| `playerdata_layout.h` | generated: where the multiplayer's stats file keeps what `profile.cpp` changes |

An install, in order: the disc's two executables are read and checked against
the known disc; the update is taken from the file or folder given, from a
package left beside the launcher, or downloaded from CoD Xenon's archive of
title updates (`codxenon/xbox360-title-updates`) at a pinned commit; its three fastfiles are checked, and each patch is applied in
memory and the result checked against the executable the build was made from.
Only then are the disc's `.ff`, `.pak` and `.bik` copied into `game/` (a file
already there, whole, is kept, so an install cut short resumes), then the
update's fastfiles, and the two patched executables last: both present and the
right ones is what a finished install is. With the disc's executables already
in `game/` (an install made by the disc version), only the update is applied
and no disc is asked for. A `--tu0` build installs the disc as it is.

    mw2-launcher --install [<disc image or folder>] [--update <package or folder>]

does the same from a terminal; with no disc it updates the install in place.

The screen is a description (`ui::Frame`) that `main.cpp` fills each frame from
a list of entries, each with an action and the text the pane shows for it.
PROFILES, GRAPHICS, MAPS and REPORT A BUG swap the list for their own (`ProfileEntries`, `GraphicsEntries`,
`MapsEntries`, `ReportEntries`). MAPS holds CUSTOM MAPS (`MW2_CUSTOM_MAPS`), which turns off at once and
on only from a screen that says first what a fastfile nobody signed can do (`mapsWarning`); PC GAME, the
PC game's folder a conversion takes shared pictures and sounds from (`maps::PcGame`, a folder dialog); ADD MAP;
and one custom map at a time (left and right for another), with IN THE GAME and REMOVE (`maps.h`).
A map in the game has its files in `game/` (`<map>.ff`, `<map>_load.ff`, `<map>.arena`, and
`<map>_preview.ff`, its picture in the game's menus, when it was converted with one), which is all
the game looks at; one out of it
has them in `maps/` beside the launcher, which also keeps each map's picture, made when it was
added from the loading screen in the PC map's `.iwd` (a zip of DXT `.iwi` images). The game's
BONUS MAPS menu has room for 8, so IN THE GAME stops there. ADD MAP takes a file from the
system's file dialog, or `mw2-launcher --add-map <file>...` from a terminal, and the map's other
files beside it, which share its name (`<map>.ff`, `<map>_load.ff`, `<map>.arena`, `<map>.iwd`). A PC map's fastfiles (`IW4x`, or version 276) are converted first, on the job
thread the install uses, its progress and CANCEL on the same screen ([maps.md](maps.md)); the
log of a conversion is kept as `maps/<map>.log`. A 360 fastfile is taken as it is, and refused
when it is not an unsigned one of the game's own version. An entry with a range (FOV,
RESOLUTION, ANTIALIASING...) sets `ui::Frame::slider`, and the pane draws a track for it; FOV's also shows a street drawn at the
angle (`preview.cpp`), with the game's projection, to say what the number does before the game
is started.
PLAY CAMPAIGN and PLAY MULTIPLAYER start `mw2-sp` and `mw2-mp` beside the
launcher, the names a release gives the two game executables. Before that,
and before REPORT A BUG starts either, the launcher checks the compiled
shaders, and compiles them first when they no longer fit (SHADERS, with its
progress and CANCEL). It does the same after an install. While either entry
is chosen, the pane shows two cards in the look of the game's callsigns
(`ui::Frame::cards`): the NETWORK setting the game will play on, and the
profile at the first controller. On Steam the name the other players see is
Steam's, which the launcher does not know, so that card says STEAM PROFILE.

    mw2-launcher --compile-shaders

compiles them from a terminal, whether they fit or not, and prints what it
did. `mw2-launcher --play campaign|multiplayer [--device <id>]` is how the game
hands over when its shaders do not fit: the window, the compile, then the game.

The shader code's build, `MW2_SHADER_BUILD`, is a SHA-256 that CMake takes at
configure time of the sources that decide what a compiled shader is (the
translators -- `runtime/gpu/vulkan/shader_translator.*`, `spirv.*`, `bindings.h`,
`xenos_ucode.h`, and `src/runtime/gpu/shader/*.rs` with
`shader_translator_rust.cpp` --, the pipeline layout, `shader_build.*`,
`launcher/precompile.*` and `launcher/zone/`), and `build.rs` the same way for a Cargo build's game: the two must
agree, or the launcher's compile pass is for another build. A change to any of them makes a player's copy compile again.

### Updating itself

CHECK FOR UPDATES (`update.cpp`) looks only when the player asks. It fetches
GitHub's description of the newest published release
(`api.github.com/repos/.../releases/latest`), which has the release's tag and,
for each archive, its name, size, SHA-256 and address, and compares the tag
with its own. The workflow gives the build its tag
(`--release-tag v0.3.0`, CMake's and `build.rs`'s `MW2_RELEASE_TAG`); a build without
one has nothing to compare and the entry is off. The archive it wants is the
one for its system, `mw2-<tag>-<linux|windows>`. Releases up to v0.7.x were
built once per online service and their launchers want
`mw2-<tag>-<system>-<steam|lan>`, so the workflow publishes the archive under
those two names as well. The first start after such an update finds that name
in `update/` and keeps the service as NETWORK (`update::Finish`).

Choosing UPDATE TO downloads that archive into `update/` beside the launcher,
checks its SHA-256, unpacks it with the system's `tar` (Windows 10 and later
have one, and it reads a `.zip`), and swaps each file in: the one it replaces
is moved to `update/old/` first, and moved back if a later one fails. A
running program can be moved but not deleted, the launcher included, so
`update/` is removed by the next start, which the launcher makes itself.
That start is told so (`MW2_LAUNCHER_TAKES_OVER`), and waits up to five
seconds for the old one to end: a copy has one launcher window, and any other
start of the window brings the one open to the front and ends (`single.cpp`;
an abstract socket named after the folder on Linux, a named mutex and event
on Windows). `--play` runs whatever else is open.
`game/` and `saves/` are not touched; a release built from another version of
the game shows UPDATE GAME as any install of another version does.

    mw2-launcher --upgrade

does the same from a terminal. `MW2_UPDATE_URL=<address>` names another
description of the same form: a mirror's, or a file as `file://` to try an
update without publishing one.

### Reporting a bug

REPORT A BUG (`report.cpp`) starts the campaign or the multiplayer with
`MW2_REPORT=1` and `MW2_LOG_FILE=reports/run.log`, and waits for it to end;
the title one starts from its menus writes on in the same log. In that mode
the game's log names the graphics driver and ends, at exit or at a fault, with
how the run performed (`runtime/report.cpp`): the frame rate in play, how the
frame times spread, what was compiled in the middle of a draw, and where the
ring consumer's thread spent play. That last line says what holds a slow run
back: the thread itself when it is at work nearly all the time, the graphics
chip when much of that work is waiting for it, and the title's own threads or
the display when it is neither. Of the work, it also gives the share that went
on reading textures and on compiling at a draw.

Three more lines say whether the machine itself was short of something. Once a
second of play the report reads the processor time of the whole system, of
the process and of each thread the runtime registered (`crash::RegisterThread`
hands them to `report::ThreadStarted`), and the free memory and the commit
charge (`platform::ProcessorLoad`, `ThreadTime`, `MemoryState`). From them:
how busy the processor's threads were in play, and how much of that was the
game rather than other programs; the busiest of the game's threads, in % of
one processor thread, the rest being the driver's and the system's threads;
and the memory, with what the hardware keeps for itself (an integrated
graphics chip's share, on Windows), the least that was free in play and the
game's own peak. A reading costs about 0.1 ms, once a second, on the ring
consumer's thread. A fourth line counts the memory watch's write faults and
changes of page protection a second of play (`src/runtime/gpu/memory_watch.rs`), and
what the changes cost: a few microseconds each on Linux, more on Windows,
and more again with Memory Integrity on. They are counted and timed only in
this mode.

The launcher then writes `reports/mw2-report-<date>-<time>.txt`: the version,
the system, processor, memory, whether Windows' Memory Integrity runs, whether
a laptop is on its battery, graphics and displays, the game's own summary,
and the log. An issue is public, so the log first loses the player's name, IP
addresses, the session keys of a `connect` line, account numbers, other
players' names, and the game's and the home folder's paths; the log as the
game wrote it is deleted. GitHub's new-issue page is opened with the same
description filled in, in place of the text an issue written by hand starts
with (`.github/ISSUE_TEMPLATE/issue.md`, which sends people to the launcher), and
the folder with the file beside it: the launcher
has no account to file an issue with and a page cannot be handed a file, so
the player drags it in.

The game ends cleanly when it is told to from outside, so its summary is
written. The launcher may be ended along with it (Steam's "Exit game", the
only way out on a Steam Deck in Gaming Mode): the log is then still there,
and the next start of the launcher asks whether to send it: YES makes the
report from it, NO deletes it.

## Windows

`cargo xtask build --windows --title mp --release --llvm-mingw <dir>` produces
`build-win-mp-release/mw2.exe` with `cmake/mingw-w64.cmake` (clang for
`x86_64-w64-mingw32` against the UCRT). XenonRecomp itself is still built with
the host's clang-18. The executable needs nothing beside it:

- the C++ runtime and threads are linked statically;
- `vulkan-1.dll` comes with the GPU driver, so `cmake/vulkan-windows.cmake`
  fetches the Vulkan headers and makes the import library from them (a Cargo
  build's `build.rs` takes the build machine's headers, `libvulkan-dev`'s, and does
  the same with llvm-mingw's `dlltool`);
- the executable asks for 16 MB thread stacks (Linux gives 8 MB), since the
  recompiled code nests deeply on the host stack;
- a release build is a windowed program (`-mwindows`) that attaches to the
  terminal it was started from, if any.

It needs Windows 10 1803 or later, for the placeholder mapping of guest memory.

## Release workflow

`.github/workflows/release.yml` runs by hand from the Actions tab with a version
tag. It builds both titles of title update 6 for Linux (Ubuntu 22.04,
so the binaries need glibc 2.35 at most, with Vulkan-Headers 1.3.275 installed
over the system's) and for Windows (cross-compiled on Ubuntu with llvm-mingw),
the campaign's build with the launcher and the multiplayer's with the game
alone (`TARGETS`). ccache's and ThinLTO's caches (`.ccache`, `.lto-cache`) are
kept between runs; each title's own PGO profile is part of a compile's key, so
the two titles share little of the compiler's. It then packages `mw2-sp` and
`mw2-mp` with the README per platform,
and drafts a GitHub Release. A third job builds the relay's image
([relay.md](relay.md)) for amd64, pushes it to
`ghcr.io/<owner>/mw2-relay:<tag>` with the workflow's own token, and adds the
amd64 binary to the release. The registry makes a new package private: it is
made public once, in the package's settings on GitHub. The two executables come from a
private repository named by the secret `ASSET_REPO`, read with
`ASSET_REPO_TOKEN`, which holds `default.xex` and `default_mp.xex` at its root
and the update's two patches and three fastfiles under `title-updates/TU6/`.
It packages `mw2-launcher` with them, and the fonts' licence.

## Layout

| path | what |
|---|---|
| `xtask/` | the whole build (`cargo xtask`) |
| `Cargo.toml`, `build.rs`, `src/` | the executable |
| `XenonRecomp-rs/` | the recompiler, a submodule |
| `CMakeLists.txt`, `cmake/` | the C++ build, the launcher's and the tools'; the Windows toolchain, the Vulkan import library and SDL's toolchain for `sdl3-sys` |
| `config/MW2.toml`, `config/MW2MP.toml` | recompiler config, campaign and multiplayer; `*.tu0.toml` for the disc version |
| `ppc/`, `ppc_mp/` | generated C++ and its generated glue (gitignored) |
| `ppc_rs/`, `ppc_mp_rs/` | generated Rust (gitignored) |
| `runtime/` | the host runtime |
| `tools/` | generators, reverse-engineering scripts, capture helpers |
| `tools/routes/` | recorded walks and flights for `MW2_WALK_PATH` and `MW2_FLY_PATH` |
| `pgo/` | the recorded profiles the build is optimised with ([Optimisation](#optimisation)) |
| `launcher/` | the launcher: install, and starting the game ([The launcher](#the-launcher)) |
| `relay/` | the relay server for NETWORK SERVER, and its Dockerfile ([relay.md](relay.md)) |
| `third_party/ffmpeg-xenia/` | builds the XMA frame decoder from Xenia's FFmpeg branch, fetched at configure time, for the C++ build, the launcher and `tools/xma_diff.py` (the Rust build has `src/runtime/wmapro/`) |
| `third_party/fonts/` | the launcher's fonts (Barlow), and Michroma for the README's titles; SIL Open Font License |
| `docs/` | this documentation |

### The runtime

| path | what |
|---|---|
| `main.cpp` | the C++ build's run: reserves the guest space, loads the image, starts the title, ends the run (the Rust build's is `src/runtime/run.rs`) |
| `guest.h`, `guest_memory.*` | guest pointers, big-endian values, the address space |
| `env.h`, `diagnostics.h`, `log.h`, `counter.h`, `words_hash.h` | switch readers, the diagnostics gate, the log, small helpers |
| `platform.*` | what Linux and Windows spell differently |
| `title.h` | the guest addresses the runtime names, per executable |
| `install/` | a player's start: the executable check, XEX decryption, the hand-over to the launcher |
| `kernel/` | kernel and XAM imports: memory, objects and waits, threads, files, content and profile, input, networking, video, audio |
| `apu/` | the XAudio render driver and the XMA decoder (the C++ build's), and `xma_codec.cpp`, FFmpeg's decoder behind C calls for `tools/xma_diff.py` |
| `online/` | the online services (`lan`, `server`, `steam`), chosen as the game starts |
| `gpu/` | the command processor: ring and PM4 packets, interrupts, D3D9's command-buffer arena, texture fetch and tiling, the D3D9 hooks, the write watch (`memory_watch.*`); the ring consumer, the arena, the command processor, the register file, the census, the write watch, the D3D9 hooks and texture tiling are `src/runtime/gpu/`'s and `src/hooks/d3d_hooks.rs`, and their C++ the `--renderer cpp` build's |
| `gpu/vulkan/` | the Vulkan renderer: draws, targets, resolves, frames, pipelines and pipeline libraries, the shader translator, the texture cache, the recorder thread, the display table; the presenter window, its backdrop, the GPU's busy time and RenderDoc capture are `src/runtime/gpu/vulkan/`'s, and their C++ the `--renderer cpp` build's |
| `image_move.cpp` | the image pool's block copy, which the console makes with a memory-export draw; `src/runtime/gpu/image_move.rs` and `src/hooks/image_move.rs`, and this the `--renderer cpp` build's |
| `shader_preload.cpp` | hands shaders loaded with a level to the renderer to compile; `src/hooks/shader_preload.rs`, and this the `--renderer cpp` build's |
| `engine.h`, `engine_log.cpp`, `predicate_waits.cpp`, `console.cpp` | hooks into the engine: print and error paths, spin waits, the console command buffer |
| `player.cpp` | the headless walker and route recorder |
| `field_of_view.cpp` | `MW2_FOV`: scales the view angle the title computes |
| `shadow_resolution.cpp`, `shadow_resolution.h` | which depth surface the title made for its shadow maps, for `MW2_SHADOW_SCALE` |
| `texture_filtering.cpp` | `MW2_ANISOTROPY`: the title's least anisotropic filtering, registered with the setting as its default |
| `render_size.cpp`, `render_size.h` | `MW2_RENDER`: the title's frame and scene sizes, the picture's shape, and 16:9 pictures covering another shape |
| `unsigned_fastfiles.cpp`, `custom_map_list.cpp` | lets the multiplayer load a fastfile nobody signed, a converted map, and lists the map from its `.arena` |
| `stutters.cpp`, `pacing_trace.cpp` | the stutter detector and the frame pacing timeline |
| `report.cpp` | `MW2_REPORT=1`: the driver's name and a run's performance summary, for a bug report; the GPU's own busy time comes from `src/runtime/gpu/vulkan/gpu_time.rs` (`gpu/vulkan/gpu_time.cpp` in the `--renderer cpp` build) |
| `crash.cpp`, `watchpoint.cpp`, `mmio_hook.h` | the fault handler, the write watchpoint, the hardware-register store hook |

### Tools

| path | what |
|---|---|
| `xexinfo.py` | dumps an XEX2's headers |
| `xexpatch.cpp` | applies an executable patch (`.xexp`) with XenonRecomp's patcher, for `cargo xtask` |
| `title.py` | which executable the other tools work on (`MW2_TITLE=sp\|mp`, `MW2_VERSION=tu6\|tu0`) |
| `fixbounds.py` | explicit function boundaries for leaf functions holding jump tables |
| `extract_shaders.py` | every shader's microcode from the fastfiles |
| `xenos_shader.py`, `xenos_isa.py` | Xenos microcode disassembler |
| `translate_shader.cpp`, `upload_texture.cpp` | the `translate-shader` and `upload-texture` tools: the shader translator and texture upload against a real driver, without the game (Linux) |
| `zone_shaders.cpp` | the `zone-shaders` tool: the launcher's fastfile reader on the files given, executables first; each file's shader count and walk time, `--hashes <dir>` to compare with another reader, `--dump <dir>` for the microcode (Linux) |
| `find_in_title.py` | finds the other executable's copy of a function by its code |
| `analyze.py`, `callgraph.py`, `xref.py`, `strings_in.py` | questions about the guest image: indirect branches, the call graph, address references, strings |
| `d3d_region.py`, `classify_d3d.py`, `resource_audit.py` | map the statically linked D3D9 ([d3d9-seam.md](d3d9-seam.md)) |
| `capture_by_hand.sh`, `capture_xenia.sh` | RenderDoc captures from this runtime or from Xenia |
| `rd_run.sh` | runs a RenderDoc analysis script and closes RenderDoc afterwards; the only way to run one |
| `pgo_record.sh` | records the profiles in `pgo/` ([Optimisation](#optimisation)) |
| `rd_thumbs.sh` | the picture from every `.rdc` as a PNG, plus a contact sheet |
| `flash_hunt.sh`, `flash_hunt_report.py`, `flash_draws.py` | the flash hunt: mark flashes in play, then compare the marked frame's draws with its neighbours' |

### Docs

| file | what |
|---|---|
| [building.md](building.md) | this page |
| [runtime.md](runtime.md) | how the runtime stands in for the console |
| [switches.md](switches.md) | every `MW2_*` switch |
| [d3d9-seam.md](d3d9-seam.md) | where the title meets the GPU: the statically linked D3D9, the ring, the flip handshake |
| [shaders.md](shaders.md) | Xenos microcode translated to SPIR-V |
| [textures.md](textures.md) | fetch constants, tiling, upload and invalidation |
| [rendering.md](rendering.md) | render targets, resolves, pipeline state, gamma and MSAA |
| [gameplay.md](gameplay.md) | getting from the menu into a level |
| [multiplayer.md](multiplayer.md) | the multiplayer executable and the online services |
| [relay.md](relay.md) | running a server for NETWORK SERVER, and its protocol |
| [audio.md](audio.md) | the audio driver and the XMA decoder |
| [saves.md](saves.md) | content packages, saved games and the profile |

## Local changes to XenonRecomp

These are in [XenonRecomp-rs](https://github.com/PaulCombal/XenonRecomp-rs), the
fork the `XenonRecomp-rs/` submodule holds:

- **`XenonRecomp/recompiler.cpp`**
  - Instructions MW2 uses that the recompiler did not handle: load/store update
    forms (`lhzu`, `lhau`, `lfsu`, `lfdu`, `sthu`, `stfsu`, `stfdu`, `stbux`,
    `lbzux`, `lhzux`, `lwzux`, `ldux`, `lfsux`, `sthux`, `stdux`), `lhbrx`,
    `dcbst` (no-op), `bdzf`, `eqv`, `addc`, `addme`, `subfze`, `mulhdu`,
    `frsqrte`, `vsubuws`, `vpkswss`/`vpkswss128`, `vcfpuxws128`, `vsel128`.
  - `vpkd3d128`/`vupkd3d128`: the 10-10-10-2 signed normalised format (x in the
    low bits, w unsigned in the top two), and `float16_2` packing.
  - A jump table switches on the low 32 bits of the index register: the guard
    ahead of it is `cmplwi`, and the high half may hold other bits.
  - A jump table's `default:` returns instead of `__builtin_unreachable()`, which
    let the compiler drop the bounds check.
  - `mftb` reads `PPC_QUERY_TIMEBASE()`, a 50 MHz counter the runtime supplies,
    instead of `__rdtsc()`.
- **`XenonRecomp/register_flow.cpp`** (new), with `recompiler.cpp` and
  `recompiler.h`: the configs turn the `*_as_local` options on, so registers
  are host locals, which the host compiler keeps in host registers, instead of
  fields of `PPCContext` that every instruction loads and stores. A local
  belongs to one function, which is only correct while no value crosses a call
  in a register that becomes one, and MSVC's output does not always keep to
  the calling convention: `__chkstk` takes its size in r12, exception handler
  funclets take their parent's frame in r12 and read its non-volatile
  registers, a function `setjmp` returns to twice resumes with the registers
  the host `longjmp` restores from the context, and XenonAnalyse splits a few
  functions in two that share registers. `register_flow.cpp` finds those
  functions by data flow over every instruction, with what each reads and
  writes taken from what `recompiler.cpp` emits for it, and what a call reads
  and writes from the callee, to a fixed point over the call graph. They keep
  every register in the context (about 140 of 28,000 in each executable; the
  recompiler prints the count and why), and so do the register save/restore
  helpers, which the other functions skip. `PPCContext` keeps every register:
  the `PPC_CONFIG_*_AS_LOCAL` defines are no longer emitted.
- **`XenonAnalyse/main.cpp`**: jump-table patterns for XDK 8276's instruction
  order, which schedules the index shift between the `lis` and the `addi`; the
  table and base address pairs are located by opcode rather than fixed offset,
  and `nop` padding inside a pattern is skipped.
- **`XenonAnalyse/function.cpp`**: a `bclr` ends a function only when its BO
  ignores both the condition and the counter. `bdzlr` tests the counter, and
  treating it as a return cut `memset` short.
- **`XenonUtils/ppc_context.h`**: `simde_mm_vctuxs` (float to unsigned word with
  saturation), and the `PPC_QUERY_TIMEBASE` declaration.
