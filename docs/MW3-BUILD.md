# Building the MW3 generated-code target

MW3 does not yet have a validated, title-complete runtime. Its campaign
executable has its own entry point, globals and engine hooks; the addresses in
`runtime/title.h` are MW2 addresses. Do not copy or guess those addresses.

An isolated diagnostic link probe now exists behind `MW3_LINK_PROBE=ON`.
It links the existing host runtime with the generated MW3 PPC library and
produces a headless `mw3` executable. This proves link compatibility only;
it does not establish that MW3 boots or that MW2-derived kernel and graphics
handlers have correct MW3 semantics. The first-stage guest reaches `XamLoaderLaunchTitle("default.xex")`; the
development runner intentionally stops at that handoff rather than pretending
to have launched the next executable. The next task is to establish which
private title image is intended at this stage and provide a safe development
chain-load path.

## Build the current MW3 milestone

Place your own disc executable at `mw3/tu0/default.xex`, then run:

```sh
bash tools/build_mw3.sh
```

The script runs the existing MW3 XEX/recompiler pipeline and then configures
CMake with `MW2_TITLE=mw3` and `MW3_RECOMP_ONLY=ON`. The resulting target
compiles the generated PPC translation units into
`build-mw3/libppc_recomp.a` and compiles the shared
`runtime/kernel/*.cpp` handlers as object files against the generated MW3
headers. The kernel object target is compile-only: it does not link an
executable or establish that MW2-derived handlers are semantically correct
for MW3. Title-specific hooks, globals and full runtime linking remain gated.

The object target `mw3-runtime-compile-check` covers the whole runtime tree that
can be compiled without a GPU or decoder library: `runtime/*.cpp`, `kernel/`,
`gpu/`, `apu/`, `install/` and `online/` (88 sources at the time of writing).
Title hashes and the playlist data are placeholders, since nothing is linked.
The Vulkan renderer (`gpu/vulkan/`, minus `shader_build.cpp`, which needs
CMake's shader hash) and `apu/xma.cpp` are added only when CMake finds the
Vulkan headers or `libavcodec/avcodec.h`; the configure step prints which
optional sources were included. A machine without them still verifies every
other file, and a CI image with `libvulkan-dev` checks the renderer too. What is
still missing for a linked MW3 executable: MW3 values for the D3D device offsets
and the seven hooks not yet found, the title's real hashes and launcher
plumbing, and the SDL/Vulkan/FFmpeg dependencies on the build machine.

Equivalent CMake commands, after `ppc_mw3/` has been generated:

```sh
cmake -S . -B build-mw3 -G Ninja \
  -DCMAKE_C_COMPILER=clang-18 \
  -DCMAKE_CXX_COMPILER=clang++-18 \
  -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON
cmake --build build-mw3 --target mw3-recomp-check --parallel
```

## Diagnostic desktop link probe

The local link probe uses the same pinned XEX identity and links the
generated MW3 PPC static library with the host runtime:

```sh
cmake -S . -B build-mw3 -G Ninja \
  -DCMAKE_C_COMPILER=clang-18 \
  -DCMAKE_CXX_COMPILER=clang++-18 \
  -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON -DMW3_LINK_PROBE=ON \
  -DMW2_USE_SDL=OFF -DMW2_LTO=OFF
cmake --build build-mw3 --target mw2 --parallel
```

The target's output file is `build-mw3/mw3`. It accepts an explicit XEX
path and game-root path. Do not upload or publish this executable: it contains
recompiled code derived from the user's private game executable. Run it only on your own machine and
share only sanitized diagnostics.

## Scope and limitations

- Generated MW3 PPC compilation and diagnostic desktop linking are validated.
- First-stage guest execution runs until the title's fatal-error handler
  (`sub_8232E638`) reports `ERROR: Could not find zone 'ui.ff'` through
  launch data and asks to relaunch `default.xex`. That relaunch is the error
  restart, not a second executable. This is not a complete boot.
- Main-menu boot and gameplay are unverified.
- The CI runner's Vulkan driver is incompatible, so GPU device initialization
  is not validated by this run.
- No PS4 ELF, PKG or playable build exists.
- `runtime/title.h` has an MW3 block (entry point, `Com_Error`, `D3D_Present`, `D3D_ArenaWait`, both shader-preload hooks, the device offsets and the kernel data records); hooks not listed there are not enabled for MW3.
- MW3 hook candidates must be checked against the exact MW3 XEX before they
  are added. Unknown hooks stay undefined rather than inheriting MW2 addresses.
- The generated `ppc_mw3/` directory and extracted game executable are local
  inputs/artifacts and are not committed.

## CI is asset-free

Game files are never uploaded anywhere: not to this repository, not to a
private asset repository, not to Git LFS and not to a CI runner. The
workflow therefore only runs checks that need no game data (source and
configuration checks, `tools/check_no_game_files.py`, the asset-free unit
tests and the relay tests). Anything that needs `default.xex`, the
fastfiles or the generated `ppc_mw3/` tree, including the link probe above
and the smoke test, runs only on the developer's own machine
([MW3-LOCAL-RUN.md](MW3-LOCAL-RUN.md)).

Do not re-add a workflow, secret, variable or LFS rule that fetches game data.
