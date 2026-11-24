# Building from source

The retail Modern Warfare 2 disc's executables, as title update 6 patches them,
are recompiled to Rust with
[XenonRecomp-rs](https://github.com/PaulCombal/XenonRecomp-rs), a fork of
[XenonRecomp](https://github.com/hedge-dev/XenonRecomp), compiled as native
x86-64, and linked with a host runtime that stands in for the console's kernel,
GPU (through Vulkan) and audio hardware ([runtime.md](runtime.md)).

The game data and the generated Rust are gitignored, and XenonRecomp-rs is a
submodule at a pinned commit. Everything else under version control is hand-written
or a patch.

## Requirements

- `cmake`, `ninja-build`, `clang-18`, `lld-18`, `llvm-18` (`llvm-nm-18` and
  `llvm-objdump-18` for the hook check), `git`,
  and rustup, which installs the compiler and the Windows target
  `rust-toolchain.toml` names. `python3` for the tools only.
- The Vulkan loader's development files (`libvulkan-dev`): the game and the launcher
  link the loader, and a Windows build makes its import library from the headers.
  Neither builds without them.
- SDL3 is linked statically: the game's is built from the `sdl3-src` crate's copy by
  `sdl3-sys`, the launcher's fetched by CMake at the same version, with Dear ImGui.
  On Linux it needs the usual development packages for its video, audio and input
  backends (X11, Wayland, ALSA/PulseAudio/PipeWire, udev, dbus).
  `.github/workflows/release.yml` lists them.
- The retail ISO, or at least `mw2/tu0/default.xex` and
  `mw2/tu0/default_mp.xex`.
- Title update 6: its package, or its files in `mw2/update/`
  ([Title update 6](#title-update-6)).

## cargo xtask

`cargo xtask build`, at the top of the tree, does everything, in order
(`xtask/src/main.rs`):

1. checks out the [XenonRecomp-rs](https://github.com/PaulCombal/XenonRecomp-rs)
   submodule and its own, when `git clone --recursive` did not, and stops if it is
   neither at the commit the tree records nor at a later one of its history;
2. builds XenonRecomp and XenonAnalyse into `out/xenonrecomp/`;
3. extracts `default.xex` and `default_mp.xex` from the ISO into `mw2/tu0/`,
   and every `.ff`, `.pak` and `.bik` into `mw2/game/` (`xtask/src/disc.rs`, on
   the disc reader of the `install/` crate; files already there are kept); then
   applies the title update with the same crate's patcher, which writes the
   executables the rest works on, `mw2/default.xex` and `mw2/default_mp.xex`
   ([Title update 6](#title-update-6));
4. writes the flat PE images (`mw2/default.pe` and `mw2/default_mp.pe`, and the
   disc's in `mw2/tu0/`) as the game makes them at every start
   (`install/src/xex.rs`): both titles' whichever is built, since the launcher,
   built with either, knows an install by both;
5. finds jump tables with XenonAnalyse (`config/mw2_switch_tables.toml`,
   `config/mw2mp_switch_tables.toml`);
6. recompiles as Rust into `ppc_rs/` or `ppc_mp_rs/` with `--no-cpp-fallback`, so
   that a function the Rust backend would leave to C++ fails the build, and points
   the `generated` link at it;
7. writes the kernel import stubs (`kernel_stubs.rs`) into that tree
   (`xtask/src/generate.rs`);
8. builds the game with cargo, then the launcher (cargo, its C++ through CMake) and the tools
   ([Cargo](#cargo)).

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
| `--release` | builds without diagnostics (the `diagnostics` feature off) and, on Linux, with the launcher's C++ library linked statically (`portable`); the release game compiles no C++, and on Linux links no C++ library; adds `-release` to the build directory |
| `--windows` | cross-compiles for Windows with llvm-mingw; the build directory becomes `build-win...` and the executable `mw2.exe` |
| `--llvm-mingw <dir>` | the unpacked [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) release, required by `--windows`; `LLVM_MINGW` in the environment names it too |
| `--iso <path>` | the disc image. Without one, `mw2/tu0/default.xex` and `mw2/tu0/default_mp.xex` are enough to build, and no game data is extracted |
| `--build-dir <dir>` | builds there instead of the default directory |
| `--targets "<target> ..."` | builds the game and only those targets instead of all of them: `launcher` and `translate-shader`, which cargo builds, or CMake's (`map-port`, `zone-shaders`, ...) |
| `--keep-generated` | keeps the recompiled tree already there (skips steps 5-6; the stubs are written again), for a second build of the same title |
| `--release-tag <tag>` | the release the build is published as (`v0.3.0`): the launcher looks for a newer one than it. Without it the launcher does not |
| `--cmake-extra <arg>` | one argument for CMake's configure step (a compiler launcher, `-DFETCHCONTENT_BASE_DIR=...`), passed as it is, not split at spaces; repeated for more |

Build directories:

| | campaign | multiplayer |
|---|---|---|
| diagnostic | `build/` | `build-mp/` |
| `--release` | `build-release/` | `build-mp-release/` |
| `--windows` | `build-win/`, `build-win-release/` | `build-win-mp/`, `build-win-mp-release/` |

A second build directory reuses the SDL3 and Dear ImGui sources `build/`
fetched.

### Cargo

`cargo xtask build` writes the guest code as Rust into `ppc_rs/` or `ppc_mp_rs/`,
points the `generated` link at it, writes the kernel stubs into it, and hands step 8 to
cargo. Once that is done, `cargo build --release` at the top builds the same title
again, into `target/`.

| file | what |
|---|---|
| `Cargo.toml`, `.cargo/config.toml`, `rust-toolchain.toml` | the package, the target CPU, the linker, and the compiler version |
| `build.rs` | compiles the game's C++ with clang and gives rustc its link line |
| `src/main.rs` | the executable's `main`, which hands the command line to `src/runtime/run.rs`, the run from start to end |
| `src/runtime/` | the runtime's modules |
| `src/runtime/gpu/` | the renderer ([The renderer](#the-renderer)) |
| `src/guest.rs` | what a hook sees of the guest: registers, memory, functions, `guest_hook!` |
| `src/hooks/` | the hooks |
| `src/title.rs` | `runtime/title.h` as Rust, written by `build.rs` |
| `src/env.rs`, `src/log.rs` | the switches and the log, through the C library and `stderr` |
| `generated`, `generated.title` | the link to the guest code in use, and its title and version (gitignored) |
| `xtask/` | the build from the disc to the executables ([cargo xtask](#cargo-xtask)) |
| `xtask/src/check_hooks.rs` | after the link, that every hook won it |
| `tools/inflate_diff.py`, `tools/inflate_diff.rs` | the game's gunzip against zlib's, on every image's XLAST, made streams and damaged copies |
| `tools/font_diff.py`, `tools/font_diff.rs`, `tools/font_diff.cpp` | the screens' font rasteriser against stb_truetype |
| `rustfmt.toml` | the width for `cargo fmt`; `rustfmt` and `clippy` come with the pinned toolchain |

Generating has to come first: cargo reads the crates before any `build.rs` runs,
so a `build.rs` cannot make them. `build.rs` compiles the game's C++, in this link
order: the GPU capture (`runtime/gpu/stream_capture.cpp`) and `runtime/rust_calls.cpp`,
through which the Rust calls it, both diagnostics only; a player's build compiles none.
It compiles them as CMake compiles the launcher: clang-18
(llvm-mingw's for Windows) run with the same flags and nothing from the environment,
`-O3`, `gnu++20`, `-march` and ThinLTO, and the definitions and
include folders these files use. A file is compiled again only when it, a header it
includes (clang's depfile) or its flags changed, as many at a time as cargo gives
jobs; clang's warnings come out as cargo's. A Linux build links the loader by name
(`-lvulkan`), and `build.rs` first links an empty program against it, so that a machine
without `libvulkan.so` stops with the package to install. A Windows build makes the
loader's import library from the Vulkan headers of the build machine (`libvulkan-dev`,
or the folder `MW2_VULKAN_HEADERS` names), which must be the version
`cmake/vulkan-windows.cmake` fetches for the launcher's import library. `build.rs`
writes `link.rsp`, which rustc passes to the linker, and `build.txt`, the title,
version and diagnostics the hook check reads. Cargo keeps a build-script run for each
feature set, so `cargo xtask` copies the two into the build folder from the run cargo
used, with that run's OUT_DIR in `out_dir.txt`. Cargo then builds the launcher
([The launcher](#the-launcher)), whose C++ CMake builds with zlib
and Dear ImGui, and `translate-shader`; CMake builds the other tools, and on Linux
zlib's static library for `tools/inflate_diff.py`. A build directory holds CMake's
files, cargo's in `<build>/rust/`, and the executables as `<build>/mw2`,
`<build>/mw2-launcher` and so on.

Three things hold this together:

- **A hook is a strong `sub_X` in the `mw2` crate.** One in an archive, or in a
  library crate nothing else pulls in, is not taken out of it to replace the guest
  code's weak one, so the game would link and run the original function.
  `xtask/src/check_hooks.rs` fails the build for a hook that did not win, and
  counts them.
- **The linker is clang-18's lld**, named in `.cargo/config.toml`. Otherwise rustc
  brings its own, on a newer LLVM than the one that compiled the C++, and that
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
#[cfg(T_DB_StartStream)]                    // in hooks/mod.rs: set when title.h has the entry
guest_hook!(T_DB_StartStream, |ctx, memory| {
    let name = memory.u32_at(T_DATA_DB_Load).wrapping_add(8);
    log_info!("fastfiles: %s", memory.text(name));
    T_DB_StartStream.original(ctx, memory);  // the recompiled function; .call() goes through the hook
});
```

- `runtime/title.h` is the one table. A name that is not in this build's block
  has no `cfg`, and a `cfg` of a name in no block warns.
- A log line is printf: `log_warn!` checks the format against its arguments when
  it compiles, and the C library formats it, so that `%g` and a `%s` of the guest's
  bytes read as C's do. It supports `%g`, `%.2f`, `%s`, `%.64s`, `%d`, `%u`,
  `%#x`, `%08X`, `%llu` and `%p` so far.
- Arithmetic on anything from the guest wraps by name (`wrapping_add`), as the
  console's does: with `+`, what a hostile peer sends would panic a `cargo build`,
  whose overflow checks are on. Numbers are parsed with the C library's
  (`src/env.rs`, `scan.rs`): `str::parse` is not `strtod`.
- `xtask/src/check_hooks.rs` knows how many hooks each build has, so a hook lost
  fails the build.
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

The runtime is Rust; the C++ beside it is the GPU capture and the glue the Rust calls
it through, both diagnostics only. A player's build compiles no C++: the guest code and the
kernel stubs are Rust.

- A module is `src/runtime/<name>.rs`, safe code under `deny(unsafe_code)`. What is
  the system's or a C library's is unsafe by nature and kept to itself:
  `guest_memory`, `platform`, `windows`, `install/spawn`, `cpu_level`, `crash`,
  `fault_text`, `watchpoint`, `scan`, `online/udp` and `online/steam/client`.
  `exports.rs` gives the `extern "C"` functions the C++ and the recompiled code call,
  and turns C's pointers into safe types.
- The launcher compiles its own C++ of the install's checks (`runtime/install/`
  `crypto.cpp`, `xex.cpp` and `spawn.cpp`); the game's are `src/runtime/install/`.
- Neither language frees what the other made. Text crosses as a C string the callee
  reads during the call, and bytes go back into a buffer the caller reserves
  (`mw2_cp_save` in `src/runtime/gpu/exports.rs`).
- The Rust calls the GPU capture through `runtime/rust_calls.cpp` and
  `src/runtime/bridge.rs`; the capture calls back through
  `src/runtime/gpu/exports.rs`.
- `src/log.rs` writes a line whole, with one `fprintf`, to the C library's stderr,
  where the C++'s lines go too. `runtime/log.h` takes `MW2_LOG_TIME`'s stamp from
  `src/log.rs`, so both count from the same moment, and `MW2_LOGGING=OFF` silences
  both.
- A log line and a line of a file go through printf, from `log_info!` and
  `format_text!`, with the format checked against the arguments as it compiles. A
  route's numbers are read by `sscanf`, in `scan.rs`. The launcher reads the log for
  its bug report, so its text is an interface.
- A signal handler that returns takes no lock and allocates nothing: `crash` keeps
  its threads in a table it reads as it stands, `watchpoint` has a lock of its own,
  and text goes out through `fault_text`, from the stack, to the descriptor. A run
  that is ending still writes its reports, as a bug report needs them, so a fault
  inside the allocator can hang there.
- SDL comes from the `sdl3-sys` crate, which builds it statically from the source the
  `sdl3-src` crate carries, the version CMake fetches for the launcher, with CMake and
  `cmake/sdl.cmake` as its toolchain file (`cmake/sdl-mingw-w64.cmake` for Windows,
  both named in `.cargo/config.toml`): clang, only the static library, and the options
  the game needs. The C++ `build.rs` compiles takes its headers from that build, and
  rustc links it, with what it asks for. Unlike the rest of the C, it is compiled
  without ThinLTO and the profile. `sdl3-sys` builds it once in a target folder and
  never again, so `build.rs` stops a build whose toolchain files or llvm-mingw
  changed since, and one whose SDL is not the version CMake fetches for the launcher.
  `cargo clean -p sdl3-sys` builds it again, as a newly installed development package
  of one of its backends needs too. Win32's declarations are by hand in
  `src/runtime/windows.rs`. The game links no zlib and no FFmpeg: the title's XLAST
  is gunzipped by `src/runtime/inflate.rs`, which `tools/inflate_diff.py` holds to
  zlib, and XMA frames are decoded by the `wmapro` crate (`wmapro/`). The system's screens
  draw their letters with `src/runtime/truetype.rs`, the part of stb_truetype (the
  copy that comes with Dear ImGui) that bakes a font into an atlas, step for step.
  `tools/font_diff.py` holds it to the C bit for bit -- the atlas, the glyphs' boxes
  and the ascent -- on the launcher's three fonts at every size the screens can ask
  for and every atlas side, on the machine's own fonts, CFF ones among them, on fonts
  it makes to reach every branch, and on damaged copies, where the Rust refuses a
  font wherever the C would read outside it.
- The game's side of the relay protocol, `src/runtime/online/relay_wire.rs`, names
  every number and place it reads, and the relay's `cargo test`
  (`relay/tests/game_wire.rs`) holds it to `relay/src/protocol.rs`.
- A constructor that must run first is C's `constructor(101)`, spelled by hand:
  `.init_array.00101`, or `.ctors.65434` on Windows, in a writable static, or the
  linker leaves it out of the table the C runtime walks. `src/runtime/cpu_level.rs`
  is one, and runs in a crate compiled for the processors it refuses, so
  `xtask/src/check_hooks.rs` disassembles it.
- A kernel import is `kernel_import!(<Name>, |ctx, memory| ...)` (`src/guest.rs`),
  safe code as a hook is.

Rust code that builds these crates needs `panic = "abort"` and
`-Ctarget-cpu=x86-64-v2`, set here in the top workspace; the generated workspace's
own settings do not apply. `cargo build --features` chooses `diagnostics` (on by
default) and `portable`. `diagnostics` is the traces, dumps, statistics, end-of-run
reports, the stutter detector and pacing timeline, the flash hunt, the write
watchpoint and the headless harness (walker, input script, run deadlines,
watchdog); without it every diagnostic switch is off whatever the environment says.
`portable` links the launcher's libstdc++ and libgcc statically (Linux); the game
links a C++ library only with `diagnostics`, and then takes `libstdc++.a` for it. `MW2_BUILD_DIR` and
`MW2_RELEASE_TAG` pass the rest of what `cargo xtask` decides, and `MW2_LOGGING`,
`MW2_LTO` and `MW2_LTO_CACHE` in the environment are
the options below, as `ON` or `OFF`; `cargo xtask` refuses them in `--cmake-extra`,
which only CMake's part reads, and hands `MW2_LTO_CACHE` to CMake too, so that the
launcher's link keeps its results in the same folder.
`cargo xtask` also passes `--remap-path-prefix`, mapping the toolchain's
`lib/rustlib/src/rust` to `/rustc/<commit>`: with rustup's `rust-src` component
installed (an IDE adds it) rustc would otherwise write the local path, home folder
included, into the panic locations of standard-library code built into the game, so the
executable would depend on whether that component is installed.

### The renderer

The renderer is Rust: the command processor, the texture reader and the write watch
in `src/runtime/gpu/`, the Vulkan renderer, the recorder thread and the presenter in
`src/runtime/gpu/vulkan/`. What it shares with the launcher is the `mw2-shaders` crate
in `shaders/`, a member of the workspace the game's `Cargo.toml` makes: the shader
translator (`shaders/src/shader/`), the device the renderer draws on, its features and
limits, the pipeline layout and the pipeline cache (`pipeline.rs`), a shader's module
and pipeline library and the state every pipeline starts from (`shader_build.rs`), and
the marker the launcher's compile pass leaves (`marker.rs`). The game and the
launcher's compile pass call it directly, so both build every shader the same way. The
crate's log lines go to the game's log (`mw2_shaders::set_log`). The GPU
capture (`runtime/gpu/stream_capture.cpp`, diagnostics) is C++, which the Rust calls
through `runtime/rust_calls.cpp` (`src/runtime/bridge.rs`, `gpu` and `stream`) and
which calls back through `src/runtime/gpu/exports.rs`.

The renderer uses `ash` for Vulkan, linked to the loader by name (`vulkan`, or
`vulkan-1` on Windows, whose import library `build.rs` makes). `build.rs` stops when
the Vulkan development files are missing. `cargo xtask` clears `VULKAN_SDK`, which
ash's build script would add to the link, so the game links the system's loader.

Whatever decides the shaders' SPIR-V is part of the shader build's hash
([shaders.md](shaders.md)): every `*.rs` under `shaders/src/` is in
`shaders/build.rs`'s list, beside the launcher's compile pass and its zone reader.
Built-in shaders (the backdrop, the display table, the drawn resolve) are made
outside that folder, so they do not change the hash.

### Title update 6

The update is a package holding a patch for each executable (`default.xexp`,
`default_mp.xexp`) and three fastfiles for the multiplayer (`patch_mp.ff`,
`dlc1_ui_mp.ff`, `dlc2_ui_mp.ff`). After extracting the disc's executables,
`cargo xtask`:

- unpacks the package (`install/src/stfs.rs`) into `mw2/update/`, or copies the
  files from the folder `--tu` names;
- applies each patch to the disc's executable (`install/src/patch.rs`, [The
  install crate](#the-install-crate)), writing `mw2/default.xex` and
  `mw2/default_mp.xex`;
- copies the three fastfiles into `mw2/game/`.

The update changes one of the campaign's 11,133 functions, and about 260 of the
multiplayer's 11,579 with about 270 added; nearly every multiplayer function
sits at another address than on the disc. The kernel imports are the same.

A player gets the same thing from the launcher, which applies the update to
the disc's executables at install time ([The launcher](#the-launcher)).

### The disc version

`--tu0` builds the disc's executables as they are. Everything of theirs
carries the version: the image and a game folder in `mw2/tu0/`, the configs
`config/MW2.tu0.toml` and `config/MW2MP.tu0.toml`, the trees `ppc_tu0_rs/` and
`ppc_mp_tu0_rs/`, and `-tu0` on the build directory. `build.rs` takes the disc's
blocks of `runtime/title.h` and sets the cfg `mw2_version_tu0`, and so does the
launcher's `build.rs`, from CMake's `MW2_VERSION`.

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

CMake builds the launcher's C++ (`launcher_core`, which the launcher's `build.rs` links)
and the tools.

| option | default | effect |
|---|---|---|
| `MW2_VERSION` | `tu6` | `tu6`, or `tu0` for the disc's: which executables in `mw2/` the launcher installs, by their SHA-256, which the launcher's `build.rs` reads from the cache; with `tu0` the launcher installs the disc as it is |
| `MW2_LOGGING` | `ON` | `OFF` compiles out every log line, warnings included |
| `MW2_PORTABLE` | `OFF` | links map-port's libstdc++ and libgcc statically (Linux) |
| `MW2_LTO` | `ON` | ThinLTO: every target, the fetched libraries included, optimised across files at the link, with `lld` ([Optimisation](#optimisation)) |
| `MW2_LTO_CACHE` | `<build>/lto-cache` | where the linker keeps ThinLTO's results for the next link |

The game's options are environment variables of the same names that `build.rs`
reads (`MW2_LOGGING`, `MW2_LTO`, `MW2_LTO_CACHE`),
with the same effect on the C++ it compiles; `MW2_LOGGING=OFF` compiles out the
Rust's log lines too. The game is compiled for x86-64-v2 (`-march` in `build.rs`,
`target-cpu` in `.cargo/config.toml`): below SSSE3 every big-endian vector load,
store and permute of the recompiled code becomes sixteen byte moves.
`src/runtime/cpu_level.rs` tells a player whose processor lacks the level.

The launcher's `build.rs` reads the SHA-256 of the images in `mw2/` and `mw2/tu0/`
(the `.pe` files `cargo xtask prepare` writes, both titles' and the disc's whatever is built) and
of the update's fastfiles in `mw2/update/`, and `build.rs` the game's own image's:
a game executable runs only the code it was recompiled from, and the launcher
installs only a disc with that code and that disc's update. The image, not the
file: every region's executable decrypts to the same one under its own signed
header. Both hash with the `install` crate, which the workspace compiles
optimised in build scripts too (`[profile.release.package.mw2-install]`):
unoptimised, the 128 MB took 4 s at every run of the launcher's, which any
change to its sources runs again.

### Optimisation

ThinLTO optimises the C++: what `build.rs` compiles into the game (the GPU capture and
`rust_calls.cpp`, diagnostics only) and the launcher. Nothing is built with a profile.

- **ThinLTO** (`MW2_LTO`). Every file is compiled to LLVM bitcode, and the
  link optimises across them: a function inlined into another file's loop,
  code nothing calls dropped. A relink reuses the work of the
  files that did not change (`lto-cache/` in the build folder, or
  `MW2_LTO_CACHE`): a result is keyed by everything that went into it, so one
  taken from the cache is the one the link would have made.

#### The Rust code

The C++'s ThinLTO does not reach the Rust. The guest code, the runtime and the
renderer arrive at the link as machine code, without a profile.

**Why a Rust profile is tied to one build.** The compiler finds a function's counts by
its mangled name and a hash of its shape. A C++ name depends only on the code, so one
recording would serve the release, the diagnostics build and Windows alike. A Rust
symbol carries a hash of its crate's identity, which cargo
derives from the package and, among other things, its enabled features and the
target. The diagnostics build (`diagnostics`), the release (`portable`) and a
Windows build therefore name the same function three ways, and a profile recorded
on one finds no function in the others. A release profile is therefore recorded
on a release build, where only the console script (`MW2_CONSOLE`) of the run
switches works: the flights and walks, the input script and the quit on arrival
are diagnostics only. A release recording run walks a route with
`tools/padwalk.py` ([switches.md](switches.md#moving-the-player)), which drives the
game from outside and ends it with SIGINT, which SDL turns into a quit; or it is
played by hand.

**LTO across the Rust crates.** Cargo sets `lto` for the whole build and not per
crate. Fat LTO (`lto = "fat"`) merges every Rust crate into one module, the guest
code included, and fails there: a hook replaces a weak guest function with a strong
one, which the linker resolves but a merged module refuses ("changed binding to
STB_GLOBAL", one error per hook). ThinLTO across the Rust crates (`lto = "thin"`)
builds, keeps every hook and inlines runtime code into the guest code; what it gains
is measured below. Joining the Rust to the C++'s ThinLTO (`-Clinker-plugin-lto`)
needs clang and lld from the LLVM version rustc uses (22 for rustc 1.97.1; the
build's clang is 18).

**A Rust profile.** None is built. To build one on Linux, the flags reach the release
without a change to the code: `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS`
around `cargo xtask build --release`, which cargo adds to `.cargo/config.toml`'s
and the xtask's flags (`RUSTFLAGS` would replace them). Between a training build and
the release built from its profile the names match: no function of the game's crates
lacked counts. The training build has `-Cprofile-generate` and links an object
defining `__wrap__exit`, which calls `__llvm_profile_write_file()` and then `_exit`,
with `-Wl,--wrap=_exit`: the game ends through `_exit`, which skips the writer.
LLVM's continuous mode (`%c`) would avoid the wrap, but with rustc 1.97.1 it writes a
truncated file. The counts are merged with rustc's own `llvm-profdata` (the
`llvm-tools` component; clang's is another LLVM version) and read back with
`-Cprofile-use`. A profile for a build with ThinLTO across the Rust crates is
recorded on a training build with it: one recorded without left 1730 of the game's
functions without counts, one recorded with it none. The counts of a function local
to its crate appear to be keyed by its codegen unit's name, which `lto` changes.
Each title, version and system needs a profile of its own, recorded again as the
code changes.

**Measured, and why neither is on.** The builds were multiplayer releases. The
profiles were recorded by `tools/padwalk.py` walks on mp_terminal and mp_checkpoint.
Each build was measured by three walks of mp_afghan's route, which no profile had
seen, the builds taking turns, and the graphics chip held the frame rate near 166.
Per frame, the plain build's consumer took 4.33 ms of CPU time and its other threads
10.63 ms; with ThinLTO across the Rust crates 4.13 and 10.30, with the profile 4.22
and 10.32, with both 4.24 and 10.26. That is 2 to 5% less, about as much as walks of
one build differ from each other (2 to 4%), and the two together gain no more than
either alone. The campaign flight, in diagnostics builds, ran no faster with ThinLTO
across the Rust crates. Standing at the spawn a profile was recorded at, the
consumer's time fell 7% and the other threads' 5%, and at S.S.D.D.'s nothing
changed: that measures a profile on its own training run, which a walk of an unseen
map does not.

## The launcher

`mw2-launcher` (`launcher/`) is built with every build directory, beside
`mw2`. It is one executable: Dear ImGui (fetched, like SDL3) on SDL's own
renderer, three fonts from `assets/fonts/` written out as arrays at
configure time, and, for the install, the `install` crate the game reads its
executable with ([The install crate](#the-install-crate)), whose patcher applies
the title update. For the compile pass it also has zlib (fetched, v1.3.1), the
zone definitions in `launcher/zone/defs/` written out as arrays like the fonts,
and the shaders crate the game builds its shaders with (`shaders/`).

It is a cargo binary of the workspace (`launcher/Cargo.toml`), which `cargo xtask
build` builds with the build directory's features (`diagnostics`, or `portable` for
`--release`) and puts in the build directory as `mw2-launcher`. Its entry, its
compile pass, its install and download and its menu sounds' decoding are Rust
(`launcher/src/`); the rest is C++, the static library
`launcher_core` of `CMakeLists.txt`. `cargo xtask` builds it with CMake in the build
directory it configured, with SDL3 and zlib, so ninja's dependencies
decide what is compiled again; the launcher's `build.rs` builds the same targets there
(`MW2_BUILD_DIR`), which then has nothing left to do, runs again when one of their
archives or a source folder changed, and links them as CMake's link of them would.
Without `MW2_BUILD_DIR` it does neither: a `cargo check` of the workspace (an editor's)
checks the Rust, and a `cargo build` fails at the link. CMake writes what the link is
into `launcher_core.txt` there (the archives, SDL's system libraries, the compiler's
standard ones, the Vulkan import library on Windows, ThinLTO and its cache). On Linux
the archives and, for `portable`, `libstdc++.a`, `libgcc_eh.a` and `libgcc.a` are
rustc's own native libraries, which it puts ahead of the standard library's `-lgcc_s`:
from a response file after it, lld would take the shared `libgcc_s` for symbols of
`libstdc++.a`, and the executable would need it. On Windows everything is linked
statically (`-static`, libc++ and libunwind by file name), the icon is compiled by
llvm-mingw's `windres`, and a release is a GUI program (`windows_subsystem`) where the
diagnostics build keeps its console. `main` is the C runtime's (`#![no_main]`), so the
C++ gets the arguments it always had and nothing of Rust's start-up runs: SIGPIPE is
not ignored, which the game the launcher starts would inherit.

The icons are in `assets/icons/`, the launcher's and the game's, `sp` and `mp`,
each an `.ico`, the `.rc` that names it and a `.bmp`. The `.rc` is compiled by
`windres` (the launcher's in `launcher/build.rs`, the title's in `build.rs`) and
linked into the Windows program as a resource; the `.bmp` is given to the window
on both systems, the launcher's written out as an array like the fonts, the
game's included by the presenter. `tools/icons.py` makes all three from the
campaign's icon in the PC game's `iw4sp.exe`: the launcher's with the 2, its left
half as it is and its right half in the multiplayer's colours, and the game's
without the 2 (the numeral, its outline and glow masked, filled from the
background around, and the waveform it hid drawn again), the campaign's as it
is and the multiplayer's in its colours. `tools/steam_emblem.py`
makes `steam_emblem.h`, the Steam card's emblem, from Steam's own 256-pixel
icon: a coverage mask of its white parts, which the launcher tints. `tools/readme_art.py` draws the
README's banner, tiles and headings into `docs/images/readme/`.

| file | what |
|---|---|
| `src/main.rs` | the entry: `main`, which hands the C runtime's arguments to `main.cpp`'s `launcher_run` |
| `src/precompile.rs` | every shader of the game compiled before it is played ([rendering.md](rendering.md#every-shader-compiled-before-play)), on the shaders crate |
| `src/setup.rs` | the install: checks the disc, gets the update, patches the executables, copies the files, on the `install` crate; its progress to `setup.cpp`'s `setup::Progress` |
| `src/download.rs` | one file over HTTPS: `URLDownloadToFileW` (urlmon) on Windows, the `curl` program elsewhere, or `wget` where curl is not installed; the install's and the self-update's |
| `src/update.rs` | the self-update: looks for a newer release and puts it in place of the running programs; its progress to the same `setup::Progress` as the install's |
| `src/release.rs` | what the self-update reads in a release's description, and whether the release is newer: the package's library target, so that `cargo test -p mw2-launcher` runs its tests, which the binary, whose `main` is the C runtime's and which links only with a build folder's C++, cannot |
| `precompile.cpp` | the compile pass's C side: the game's shaders as `zone/` reads them, the settings, the threads, its progress and result for `main.cpp` |
| `main.cpp` | the window's loop, what each entry does, the file dialog and dropped files, and the terminal mode |
| `single.cpp` | one window a copy: a second start brings the first up in front and ends |
| `ui.cpp` | the one screen, drawn by hand: a computed backdrop whose smoke drifts, the column of entries, the pane, the wordmark |
| `setup.cpp` | the install's C side: `setup.h`'s functions over `src/setup.rs` and `src/download.rs`, and what they call back to report progress |
| `update.cpp` | the self-update's C side: `update.h`'s functions over `src/update.rs` |
| `report.cpp` | a bug report: a run with its log kept, the system's description, the new-issue page |
| `settings.cpp` | what the player sets for the game, kept in `.env`, which the game reads as it starts |
| `sound.cpp`, `src/sound.rs` | the game's two menu sounds, read out of the installed `code_post_gfx.ff` at each start, decoded by the game's XMA decoder (the `wmapro` crate), and played as the entries are moved through and chosen; none without an install |
| `profile.cpp` | the profile screen's changes to the files under `saves/` ([saves.md](saves.md#what-the-launcher-changes)) |
| `maps.cpp` | custom maps: added (a PC map converted, a 360 one checked, copied, a picture made from the `.iwd`), put in or out of the game, removed ([gameplay.md](gameplay.md)) |
| `mapport/` | the map converter: a PC map's fastfiles made the 360's ([maps.md](maps.md)); GPL v3, see its `NOTICE.md` |
| `zone/` | reads a fastfile: inflates it (`fastfile.cpp`), walks its assets by the OpenAssetTools zone definitions (`defs.cpp`, `schema.cpp`, `expr.cpp`, `walker.cpp`), and lists its shaders (`shaders.cpp`), with vertex shaders bound as the title binds them (`vertex_binding.cpp`) |
| `zone/defs/` | the zone definitions, from the map converter (`pc2xbox360mw2ools`), which takes them from OpenAssetTools; GPL v3, see its `NOTICE.md` |
| `playerdata_layout.h` | generated: where the multiplayer's stats file keeps what `profile.cpp` changes |

An install (`src/setup.rs`), in order: the disc's two executables are read and
their images checked against the known disc's, by the SHA-256 the launcher's
`build.rs` takes of `mw2/tu0/*.pe` and, for what the update makes, `mw2/*.pe`; the disc is told by the media id in their
headers (USA/Europe `2C8C0267`, Spain `11D15B16`), and each has its own title
update 6: the same patches to the code, with fastfiles in the disc's language.
A fastfile's header carries a mask of its languages (`1 << language`: English
`0x01`, Spanish `0x10`); the first the title loads sets the game's language
(`loc_language`), and a later one sharing no language with it ends the run with
"Language mismatch on download content", so one disc's update cannot go with
another. The update is taken from the file or folder given, from a package left
beside the launcher, or downloaded from CoD Xenon's archive of title updates
(`codxenon/xbox360-title-updates`, its maintainer's suggestion), for now from a
fork that adds the other languages' (`PaulCombal/xbox360-title-updates`), at a
pinned commit; its three fastfiles are checked against that disc's (the
USA/Europe ones hashed from `mw2/update/` by `build.rs`, the others written in
`setup.rs`), and each patch is applied in memory and the result's image checked
against the one the build was made from.
Only then are the disc's `.ff`, `.pak` and `.bik` copied into `game/` (a file
already there, whole, is kept, so an install cut short resumes; each is written
under a `.part` name and renamed when whole), then the update's fastfiles, and
the two patched executables last: both present and the right ones is what a
finished install is. With the disc's executables already
in `game/` (an install made by the disc version), only the update is applied
and no disc is asked for. A `--tu0` build installs the disc as it is.

The window tells what `game/` holds the same way, before its first frame
(`setup::Detect`): installed when both executables' images are the ones the
build was made from, needing the update when each is that or the disc's, and
not installed otherwise. Each image takes about half a second to decrypt and
hash, so the two are hashed on two threads.

The window and the terminal mode run the install on a thread of their own
through `setup::Run`; `setup.cpp` hands it the paths as UTF-8 and the
`setup::Progress` the window draws, which it fills through `mw2l_setup_*` calls
(the step and its title, the file being copied, the bytes done of how many,
and whether CANCEL was pressed), and takes its error back as text, worded as
the player reads it. The update is downloaded as the self-update's files are
(`src/download.rs`): Windows' own `URLDownloadToFileW`, and the `curl` program
elsewhere, or `wget` where curl is not installed (Debian and Ubuntu ship wget and
not curl, Fedora, Arch and openSUSE the other way round), with no HTTP or TLS code
built into the launcher. With neither, the player is told to install one, or to
fetch the update themselves and choose the file.

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

The shader code's build, `MW2_SHADER_BUILD`, is a SHA-256 that `shaders/build.rs`
takes of the sources that decide what a compiled shader is: every `*.rs` under
`shaders/src/`, the compile pass (`launcher/src/precompile.rs`,
`launcher/precompile.cpp`, `launcher/precompile.h`) and the zone reader
(`launcher/zone/`). The game and the launcher both take it from the crate, so
they always agree. A change to any of them makes a player's copy compile
again.

### The install crate

`install/` (the `mw2-install` crate) is the one reader of what a player's disc
and update hold, so that the files `cargo xtask` builds from, the files the
launcher installs and the executable the game checks at every start are read
the same way. The game and the launcher take it as a dependency; `xtask/`,
a workspace of its own, as a path dependency, which is why the crate uses
nothing but std.

| file | what |
|---|---|
| `crypto.rs` | AES-128 in CBC with a zero IV, to decrypt an executable, SHA-256, to know one, and SHA-1, which a patch's blocks are checked with |
| `xex.rs` | an XEX2's memory image, decrypted with the retail key and its basic compression undone (encryption none or normal, compression none or basic: all the disc and a patched executable use), and the header fields a wrong disc is told apart by: title id, media id and version, from the execution info |
| `xdvdfs.rs` | a disc image, or the folder a disc was extracted to: the files at its root, read by range or copied whole |
| `stfs.rs` | a title update's package, of the read-only kind (`LIVE`, `PIRS`): its files, with the folders above them |
| `patch.rs` | a title update's executable patch (`.xexp`) applied to the disc's executable: a port of XenonRecomp's patcher (MIT), which follows Xenia's |
| `lzx.rs` | the LZX decoder the patch is made of: a line-by-line port of libmspack's `lzxd.c` (LGPL 2.1, see the crate's `NOTICE.md`) |

A disc image's game partition starts at one of five offsets, depending on how
the disc was dumped (a full dump carries the video partition first); its volume
descriptor is sector 32 of it, and a folder is a binary tree of entries. Only
the root's files are listed: everything the title reads is there, and the one
folder a disc has beside them is the console's system update. A name that is
not one file in a folder (a separator, a drive's colon, `..`) is no disc's and
is skipped, since an install copies a file under the name the disc gives it.

A read-only package keeps one hash table for each group of 170 blocks, and a
second-level one every 0x70E4; a file's next block is in its entry of the
table. A console-signed package (`CON`) keeps two tables a group and lays its
blocks out differently, and is refused, as is one that names a file with a path.

A patch is an XEX of its own whose payload is records, each of which zeroes a
run of the executable, moves one, or decompresses one with LZX, the run it
replaces as the window's reference data. The first records rewrite the header;
the rest come in blocks, each checked by the SHA-1 the block before it holds,
and rewrite the image once it is decrypted and its compression undone. The
result is stored with neither, as an update leaves it on the console. A patch
is for one executable: the image key it holds, decrypted with the new header's
key, must be the executable's own, so another region's patch is refused
(the Polish and Russian updates on the USA/Europe or Spanish disc) rather than
applied to bytes it was not made for; the French, German, Italian, Spanish and
USA/Europe ones apply to either disc. Both ports follow the C step by step and
make the same bytes as XenonRecomp's patcher for each of those, and refuse what
it refuses.

A damaged disc, package or executable is an error that says what is wrong,
never a panic: every read is checked against the data there is, a chain of
blocks is no longer than the package, an image no larger than the
console's memory, and a patch record or LZX match that would reach outside the
executable or the window is refused, as libmspack refuses a damaged stream.

### Updating itself

CHECK FOR UPDATES (`src/update.rs`, which the window and the terminal mode call
through `update.h`) looks only when the player asks. It fetches
GitHub's description of the newest published release
(`api.github.com/repos/.../releases/latest`), which has the release's tag and,
for each archive, its name, size, SHA-256 and address, and compares the tag
with its own. The workflow gives the build its tag
(`--release-tag v0.3.0`, the game's and the launcher's `build.rs`'s `MW2_RELEASE_TAG`); a build without
one has nothing to compare and the entry is off. The archive it wants is the
one for its system, `mw2-<tag>-<linux|windows>`. Releases up to v0.7.x were
built once per online service and their launchers want
`mw2-<tag>-<system>-<steam|lan>`, which releases after v0.9.0 no longer publish:
such a launcher finds no archive of its name and offers no update, so its player
downloads a release by hand.

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
how the run performed (`src/runtime/report.rs`): the frame rate in play, how the
frame times spread, what was compiled in the middle of a draw, and where the
ring consumer's thread spent play. That last line says what holds a slow run
back: the thread itself when it is at work nearly all the time, the graphics
chip when much of that work is waiting for it, and the title's own threads or
the display when it is neither. Of the work, it also gives the share that went
on reading textures and on compiling at a draw.

Three more lines say whether the machine itself was short of something. Once a
second of play the report reads the processor time of the whole system, of
the process and of each thread the runtime registered (`crash::register_thread`
hands them to `report::thread_started`), and the free memory and the commit
charge (`platform::processor_load`, `thread_time`, `memory_state`). From them:
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
`build-win-mp-release/mw2.exe`: cargo builds for `x86_64-pc-windows-gnullvm`, with
llvm-mingw's clang as `build.rs`'s compiler and the linker, against the UCRT, and
CMake builds the launcher's C++ with `cmake/mingw-w64.cmake`. XenonRecomp itself is built
with the host's clang-18. The executable needs nothing beside it:

- the C++ runtime, Rust's and threads are linked statically;
- `vulkan-1.dll` comes with the GPU driver, so `build.rs` makes the import library
  from the build machine's Vulkan headers (`libvulkan-dev`'s) with llvm-mingw's
  `dlltool`, and `cmake/vulkan-windows.cmake` fetches the headers and does the same
  for the launcher;
- the executable asks for 16 MB thread stacks (Linux gives 8 MB), since the
  recompiled code nests deeply on the host stack;
- a release build is a windowed program (`-mwindows`) that attaches to the
  terminal it was started from, if any.

It needs Windows 10 1803 or later, for the placeholder mapping of guest memory.

## Release workflow

`.github/workflows/release.yml` runs by hand from the Actions tab with a version
tag. It builds both titles of title update 6 for Linux (Ubuntu 22.04,
so the binaries need glibc 2.35 at most) and for Windows (cross-compiled on
Ubuntu with llvm-mingw, and the Vulkan headers from the archive
`cmake/vulkan-windows.cmake` names, checked
against its SHA-256, in `MW2_VULKAN_HEADERS`),
the campaign's build with the launcher and the multiplayer's with the game
alone (`--targets`). ccache's and ThinLTO's caches (`.ccache`, `.lto-cache`) are
kept between runs. It then packages `mw2-sp` and
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
| `shaders/` | the shaders crate the game and the launcher share ([The renderer](#the-renderer)) |
| `install/` | what the game, the launcher and `cargo xtask` read the disc, the update and the executables with, and apply the update's patches with ([The install crate](#the-install-crate)); its LZX decoder is LGPL 2.1 and its patcher MIT, see its `NOTICE.md` |
| `XenonRecomp-rs/` | the recompiler, a submodule |
| `CMakeLists.txt`, `cmake/` | the launcher's C++ (`launcher_core`) and the tools' build; the Windows toolchain, the Vulkan import library and SDL's toolchain for `sdl3-sys` |
| `config/MW2.toml`, `config/MW2MP.toml` | recompiler config, campaign and multiplayer; `*.tu0.toml` for the disc version |
| `ppc_rs/`, `ppc_mp_rs/` | generated Rust and the kernel stubs (`kernel_stubs.rs`); its `cpp/ppc_fallback.cpp` has no functions, as `--no-cpp-fallback` holds it, and is not compiled (gitignored) |
| `src/` | the host runtime ([The runtime](#the-runtime)) |
| `runtime/` | the game's C++ beside the Rust, the launcher's install checks, the headers both include, and the address table |
| `tools/` | generators, reverse-engineering scripts, capture helpers |
| `tools/routes/` | recorded walks and flights for `MW2_WALK_PATH` and `MW2_FLY_PATH` |
| `launcher/` | the launcher, a cargo binary with its C++ in `launcher_core`: install, and starting the game ([The launcher](#the-launcher)) |
| `assets/icons/` | the game's and the launcher's icons, made by `tools/icons.py` ([The launcher](#the-launcher)) |
| `assets/fonts/` | the fonts of the launcher and the system's screens (Barlow), and Michroma for the README's titles; SIL Open Font License |
| `relay/` | the relay server for NETWORK SERVER, and its Dockerfile ([relay.md](relay.md)) |
| `docs/` | this documentation |

### The runtime

| path | what |
|---|---|
| `src/runtime/run.rs` | the run: reserves the guest space, loads the image, starts the title, ends the run |
| `src/guest.rs`, `src/runtime/guest_memory.rs` | guest pointers, big-endian values, the address space |
| `src/env.rs`, `src/log.rs`, `src/runtime/settings.rs` | the switch readers, the log, the `.env` file |
| `src/runtime/platform.rs`, `windows.rs` | what Linux and Windows spell differently |
| `src/title.rs` | the guest addresses of `runtime/title.h` for this build |
| `src/runtime/install/` | a player's start: the executable check (on the `install` crate), the hand-over to the launcher |
| `src/runtime/kernel/` | kernel and XAM imports: memory, objects and waits, threads, files, content and profile, input, networking, video, audio and the XMA decoder's contexts |
| `src/runtime/online/` | the online services (`lan`, `server`, `steam`), chosen as the game starts |
| `src/runtime/gpu/` | the command processor: ring and PM4 packets, interrupts, D3D9's command-buffer arena, texture fetch and tiling, the write watch, the image pool's block copy |
| `src/runtime/gpu/vulkan/` | the Vulkan renderer: draws, targets, resolves, frames, pipelines and pipeline libraries, the texture cache, the recorder thread, the display table, the presenter window, its backdrop, the GPU's busy time and RenderDoc capture |
| `shaders/` | the crate the game shares with the launcher: the shader translator (`src/shader/`, [shaders.md](shaders.md)), the device and pipeline layout (`src/pipeline.rs`), a shader's build (`src/shader_build.rs`) and the compile pass's marker (`src/marker.rs`); the `translate-shader` tool (`translate_shader.rs`): the translator against a real driver, without the game; `evaluate.py` runs the translator's SPIR-V against Xenia's results (with `evaluate_dump.rs`) |
| `wmapro/` | the XMA frame decoder, a port of FFmpeg's, which the game and the launcher's menu sounds share; LGPL 2.1 or later, see its `NOTICE.md` ([audio.md](audio.md)) |
| `src/runtime/screen.rs`, `signin.rs`, `friends.rs`, `guide.rs` | the system's screens over the title |
| `src/runtime/console.rs` | the scripted console: commands into the title's command buffer |
| `src/runtime/player.rs` | the headless walker and route recorder |
| `src/runtime/shadow_resolution.rs` | which depth surface the title made for its shadow maps, for `MW2_SHADOW_SCALE` |
| `src/runtime/render_size.rs` | `MW2_RENDER`: the title's frame and scene sizes, the picture's shape, and 16:9 pictures covering another shape |
| `src/runtime/stutters.rs`, `pacing_trace.rs` | the stutter detector and the frame pacing timeline |
| `src/runtime/report.rs` | `MW2_REPORT=1`: the driver's name and a run's performance summary, for a bug report; the GPU's own busy time comes from `src/runtime/gpu/vulkan/gpu_time.rs` |
| `src/runtime/crash.rs`, `watchpoint.rs` | the fault handler, the write watchpoint |
| `src/hooks/` | hooks into the title: print and error paths (`engine_log.rs`, `com_error.rs`), spin waits (`predicate_waits.rs`), D3D9 (`d3d_hooks.rs`, `shader_preload.rs`, `image_move.rs`), `MW2_FOV` (`field_of_view.rs`), `MW2_ANISOTROPY` (`texture_filtering.rs`), `MW2_RENDER` (`render_size.rs`), custom maps (`unsigned_fastfiles.rs`, `custom_map_list.rs`), the network version (`network_version.rs`), and the security fixes ([security.md](security.md)) |
| `runtime/title.h` | the guest addresses the runtime names, per executable |
| `runtime/exports.list` | the symbols the Linux executable exports: the recompiled functions alone, so a backtrace names the guest call stack |
| `runtime/rust_calls.cpp` | the Rust's calls into the GPU capture (`src/runtime/bridge.rs`), diagnostics only |
| `runtime/gpu/stream_capture.*` | the GPU capture (diagnostics) |
| `runtime/install/` | the launcher's executable check, XEX decryption and spawn, and the `.arena` reader (`arena.h`) |
| `runtime/*.h`, `gpu/*.h`, `kernel/*.h`, `gpu/vulkan/renderer.h` | the headers those files and the launcher include: the Rust's functions they call (`crash.h`, `log.h`, `gpu/internal.h`, `gpu/memory_watch.h`, `image_move.h`), guest memory (`guest.h`, `guest_memory.h`, `kernel/physical.h`), the GPU capture's (`gpu/stream_capture.h`), and the launcher's share of the game's rules (`render_size.h`, `report.h`, `gpu/vulkan/renderer.h`'s shader name) |

### Tools

| path | what |
|---|---|
| `xexinfo.py` | dumps an XEX2's headers |
| `title.py` | which executable the other tools work on (`MW2_TITLE=sp\|mp`, `MW2_VERSION=tu6\|tu0`) |
| `fixbounds.py` | explicit function boundaries for leaf functions holding jump tables |
| `extract_shaders.py` | every shader's microcode from the fastfiles |
| `xenos_shader.py`, `xenos_isa.py` | Xenos microcode disassembler |
| `inflate_diff.py`, `font_diff.py` | the game's gunzip against zlib, its font rasteriser against stb_truetype ([Cargo](#cargo)) |
| `replay_diff.py` | a GPU capture replayed on two executables and compared ([rendering.md](rendering.md)) |
| `map_port.cpp` | the `map-port` tool: the launcher's map converter on its own ([maps.md](maps.md)) |
| `zone_shaders.cpp` | the `zone-shaders` tool: the launcher's fastfile reader on the files given, executables first; each file's shader count and walk time, `--hashes <dir>` to compare with another reader, `--dump <dir>` for the microcode (Linux) |
| `find_in_title.py` | finds the other executable's copy of a function by its code |
| `analyze.py`, `callgraph.py`, `xref.py`, `strings_in.py` | questions about the guest image: indirect branches, the call graph, address references, strings |
| `d3d_region.py`, `classify_d3d.py`, `resource_audit.py` | map the statically linked D3D9 ([d3d9-seam.md](d3d9-seam.md)) |
| `capture_by_hand.sh`, `capture_xenia.sh` | RenderDoc captures from this runtime or from Xenia |
| `rd_run.sh` | runs a RenderDoc analysis script and closes RenderDoc afterwards; the only way to run one |
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
