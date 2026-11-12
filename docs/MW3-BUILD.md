# Building the MW3 generated-code target

MW3 is not yet ready to link against the MW2 runtime. Its campaign executable
has its own entry point, globals and engine hooks; the addresses in
`runtime/title.h` are MW2 addresses. Do not enable the MW2 runtime for MW3 by
copying or guessing those addresses.

## Build the current MW3 milestone

Place your own disc executable at `mw3/tu0/default.xex`, then run:

```sh
bash tools/build_mw3.sh
```

The script runs the existing MW3 XEX/recompiler pipeline and then configures
CMake with `MW2_TITLE=mw3` and `MW3_RECOMP_ONLY=ON`. The resulting target
compiles the generated PPC translation units into
`build-mw3/libppc_recomp.a` without compiling or linking MW2 runtime sources.

Equivalent CMake commands, after `ppc_mw3/` has been generated:

```sh
cmake -S . -B build-mw3 -G Ninja \
  -DCMAKE_C_COMPILER=clang-18 \
  -DCMAKE_CXX_COMPILER=clang++-18 \
  -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON
cmake --build build-mw3 --target mw3-recomp-check --parallel
```

## Scope and limitations

- This milestone validates compilation of generated MW3 PPC C++ only.
- It does not produce a desktop game executable, PS4 ELF, PKG or playable build.
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

The workflow verifies that `default.xex` has SHA-256
`d90bc8d93b7b8f6001ead8330271196a6ef7a5c2e4a9e9b63118f47d58a05912`
before copying it into the ignored working tree. A different executable is
rejected rather than silently generating code for a different image. The XEX
is deleted from the runner workspace after staging and is never committed.

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
