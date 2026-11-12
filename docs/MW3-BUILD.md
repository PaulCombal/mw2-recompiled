# Building the MW3 generated-code target

MW3 does not yet have a validated, title-complete runtime. Its campaign
executable has its own entry point, globals and engine hooks; the addresses in
`runtime/title.h` are MW2 addresses. Do not copy or guess those addresses.

An isolated diagnostic link probe now exists behind `MW3_LINK_PROBE=ON`.
It links the existing host runtime with the generated MW3 PPC library and
produces a headless `mw3` executable. This proves link compatibility only;
it does not establish that MW3 boots or that MW2-derived kernel and graphics
handlers have correct MW3 semantics. The CI workflow runs the linked executable against the private supported XEX.
The first-stage guest reaches `XamLoaderLaunchTitle("default.xex")`; the
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

The optional CI link probe uses the same pinned XEX identity and links the
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
recompiled code derived from the user's private game executable. CI should run
it in place on the runner and retain only appropriately sanitized diagnostics.

## Scope and limitations

- Generated MW3 PPC compilation and diagnostic desktop linking are validated.
- First-stage guest execution is observed through an explicit
  `XamLoaderLaunchTitle("default.xex")` handoff. This is not a complete boot.
- The next executable has not been loaded; main-menu boot and gameplay are
  unverified.
- The CI runner's Vulkan driver is incompatible, so GPU device initialization
  is not validated by this run.
- No PS4 ELF, PKG or playable build exists.
- The runtime hook table in `runtime/title.h` remains MW2-specific.
- MW3 hook candidates must be checked against the exact MW3 XEX before they
  are added. Unknown hooks stay undefined rather than inheriting MW2 addresses.
- The generated `ppc_mw3/` directory and extracted game executable are local
  inputs/artifacts and are not committed.

## Optional GitHub Actions build with a private executable

The pull-request workflow always runs source/configuration checks. The full
recompile-and-compile job is enabled only when the repository has:

- Repository variable `MW3_ASSET_REPO`: the private `owner/repository` holding
  the user's own MW3 executable as `default.xex` at its root.
- Repository secret `MW3_ASSET_REPO_TOKEN`: a fine-grained token with
  read-only Contents access to that private asset repository.

The private repository should contain the **extracted Xbox 360 MW3 campaign
game-root contents**, with the supported `default.xex` at the repository root
and the original relative directory structure preserved. Include the
game-root fastfiles (including `code_post_gfx.ff` and
`code_post_gfx.ffm` when present), other `.ff`/`.ffm` files, and any
additional campaign `.xex` files supplied alongside the disc executable.
Do not upload an ISO/GOD container, generated PPC sources, build outputs,
saves, profiles, or unrelated multiplayer/DLC data for this campaign-only
bring-up. Keep this repository private and use only game files you are
authorized to use.

The workflow verifies that root-level `default.xex` has SHA-256
`d90bc8d93b7b8f6001ead8330271196a6ef7a5c2e4a9e9b63118f47d58a05912`
before staging. A different executable is rejected rather than silently
generating code for a different image. It then copies the extracted game-root
contents (excluding Git checkout metadata) into the ignored
`mw3/tu0/` working directory, preserving subfolders. The private checkout is
deleted from the runner workspace after staging; none of these game assets
are committed or uploaded as workflow artifacts.

The latest first-stage smoke run requested `D:\\code_post_gfx.ff` and
`D:\\code_post_gfx.ffm`, and both were absent from the current private
asset repository. Add those files at the game-root level if they exist in
your extracted campaign data. The inventory step will report filenames and
sizes only, not file contents.

### Triggering the build while this PR is still open

GitHub's manual **Run workflow** control requires the workflow file to exist
on the repository's default branch. While this draft PR remains unmerged, a
new commit to a path watched by the `pull_request` trigger (including this
document) will rerun the PR workflow.

## Generated runtime audit

The full CI build also runs `tools/genshared.py` and
`tools/gen_kernel_stubs.py` with `MW2_TITLE=mw3` and `MW2_VERSION=tu0`,
then runs `tools/audit_mw3_generated.py`. The audit artifact contains symbol
names and counts only; it does not contain the XEX or generated PPC source.

The report checks whether the XEX entry point recorded for this supported
image (`0x82459E58`) appears in `ppc_func_mapping.cpp`, compares the MW3
import inventory with the existing `runtime/kernel/*.cpp` macro
implementations, lists imports that would receive diagnostic logging stubs,
and separates unresolved kernel-import wrappers from other host link symbols.
It also records host-object relocation types. Those relocations describe the
CI runner's object format; they are not Xbox XEX loader relocations.

An import counted as implemented is only a macro-level match. It does not
prove that the existing MW2 implementation is semantically correct for MW3.
Generated logging stubs return zero and are temporary diagnostics, not real
kernel implementations. Review each missing import in the context of the
MW3 boot path before implementing it. Do not replace the stubs with guessed
return values or claim runtime compatibility from this report.

## Startup call-graph audit

The CI build also runs `tools/audit_mw3_startup.py` after generating MW3 PPC
sources and kernel import glue. It resolves the XEX entry-point mapping to
XenonRecomp's `_xstart` implementation, indexes generated function bodies,
and follows direct generated calls of the form `function(ctx, base)`.
Importantly, XenonRecomp emits guest-callable functions as weak alias
declarations such as `sub_XXXXXXXX` pointing to implementation bodies named
`__imp__sub_XXXXXXXX`; these aliases are resolved before traversing the
call graph. The report separately classifies known PowerPC save/restore
helper references from other unresolved call targets. For every reachable
import that still has only a generated logging stub, it also records a
shortest static direct-call path from the entry implementation, plus the
number of reachable call sites and distinct generated callers. This helps
separate widely referenced stubs from isolated ones and prioritize further
inspection; it is not proof that a branch is taken during a normal boot.

This is a static direct-call approximation, not a dynamic execution trace.
Indirect calls, function pointers, callbacks, data imports, and runtime-selected
thread entry points are not resolved. Reachability does not imply that a
conditional path executes during a normal boot. Existing runtime matches
remain macro-level only; no kernel import is promoted to verified MW3
compatibility by this report. The artifact contains names/counts only, not
generated game source.
