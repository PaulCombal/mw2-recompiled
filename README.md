# mw3recomp

**Call of Duty: Modern Warfare 3 (2011, Xbox 360) → PS4, via static recompilation.**

This project adapts the [MW2-recompiled](https://github.com/PaulCombal/mw2-recompiled) codebase to bring up the MW3 single-player campaign. The Xbox 360 PowerPC executable is translated ahead of time into C++ with [XenonRecomp](https://github.com/hedge-dev/XenonRecomp), then progressively integrated with a host runtime. This is static recompilation, not an Xbox 360 emulator.

> **Current status: generated-code build and static audits pass. MW3 does not boot yet.**
>
> The MW3 executable has been recompiled into 142 generated C++ translation units. CI successfully builds the generated PPC library and compile-checks the MW3 runtime/import glue. Startup, kernel-import and DPC investigations have advanced, but there is no linked desktop game executable, playable build, PS4 ELF or FPKG yet.

No game executable or game data is included. You must supply your own legally obtained copy. Do not commit XEX files, extracted game data, or generated proprietary game code.

## Project status

| Area | Current status |
|---|---|
| MW2-derived project/runtime foundation | Present; MW2 remains the existing working title |
| MW3 executable identification | Supported image identified and SHA-256 pinned; see [bring-up notes](docs/MW3-BRINGUP.md) |
| MW3 hook discovery | Static comparison against MW2 completed for the current image: 8 engine hooks located (including `D3D_Present` and `Com_Error`) and 7 not found; D3D's device structure moved, so the located hooks are not enabled until their offsets are re-derived. See the [bring-up notes](docs/MW3-BRINGUP.md#progress) |
| MW3 title block (`runtime/title.h`) | **Present:** entry point, `Com_Error`, kernel data-import records. 74 of 77 non-GPU runtime sources compile against it; the GPU sources have not been checked |
| MW3 recompilation | **Pass:** 128 jump tables; 142 generated PPC translation units plus function mapping; no remaining switch-case-bound errors in the validated output |
| Generated PPC compilation | **Pass:** CI compiles the generated code into `libppc_recomp.a` |
| Runtime/import compile validation | **Pass:** MW3-generated declarations and import glue are generated and compile-checked; this is not proof of MW3 runtime compatibility |
| Generated symbol/import audit | **Pass:** metadata-only audit inventories generated symbols and runtime/import coverage |
| Startup call-graph audit | **Pass:** entry-point mapping, weak aliases and reachable direct-call paths are analyzed |
| DPC investigation | **Research complete for the current callsites; implementation deliberately deferred** |
| Linked desktop executable / first boot | **Not started** |
| Playable desktop campaign | **Not started** |
| PS4 build, package and runtime testing | **Not started** |

### What the static audits established

The current validated executable produces the following audit results:

- 142 generated PPC translation units and 36,235 generated function definitions.
- 36,493 function references.
- 9,964 generated implementation nodes reachable from the startup entry point through the audited direct-call graph.
- 258 kernel/XAM imports: 191 match existing runtime implementation macros and 67 are diagnostic logging stubs.
- 27 reachable imports are covered only by logging stubs.
- The DPC audit identifies one reachable direct callsite each for `KeInitializeDpc` and `KeInsertQueueDpc`.

These are static-analysis and compilation results, not evidence that the game executes those paths successfully. A macro-level runtime match does not establish semantic compatibility with MW3. Logging stubs are placeholders, not implementations.

The DPC investigation traced a graphics-completion notification path involving a six-entry DPC record array, a four-slot completion ring, an event, and a guest worker thread. The Xbox-specific KDPC layout and dispatch semantics are not sufficiently verified, so no speculative DPC handlers have been added. See [MW3 DPC investigation](docs/MW3-DPC-INVESTIGATION.md).

## Build the current MW3 milestone

The current supported build validates generated MW3 code; it does **not** produce a game executable.

1. Prepare your own MW3 campaign XEX at `mw3/tu0/default.xex`.
2. Prepare the matching `config/MW3.toml` from the template and the documented address/configuration work.
3. Install the prerequisites listed in [building.md](docs/building.md), including CMake, Ninja, Clang 18, Python 3 and Git.
4. Run:

```sh
bash tools/build_mw3.sh
```

The script runs the MW3 extraction/recompilation pipeline, configures CMake in generated-code-only mode, compiles the generated PPC library, generates shared declarations and kernel-import diagnostics, and runs the metadata audits.

Key outputs include:

- `ppc_mw3/` — generated PPC C++ source (local build output; do not commit).
- `build-mw3/libppc_recomp.a` — compiled generated PPC static library.
- `build-mw3/mw3-runtime-audit.txt` — generated symbol/import audit.
- `build-mw3/mw3-startup-callgraph.txt` — static startup reachability report.
- `build-mw3/mw3-dpc-callsites.txt` — sanitized DPC callsite report.

See [MW3-BUILD.md](docs/MW3-BUILD.md) for the precise scope, CI asset setup and limitations. The full MW2 build pipeline in `build.sh` is not yet an MW3 executable build path.

## Development roadmap

The next engineering milestone is **runtime integration and a first desktop boot**:

1. Resolve MW3-specific engine hooks and globals against the exact supported XEX; do not reuse MW2 addresses by assumption.
2. Review reachable imports currently represented by diagnostic stubs and implement only those required by the MW3 startup path, with tests.
3. Establish safe guest callback/thread behavior and a verified DPC dispatch design before implementing the graphics-completion DPC imports.
4. Add an MW3-specific title/runtime configuration and link the generated PPC code against the compatible runtime.
5. Produce a desktop executable and debug startup until the main menu renders; then work toward loading and playing the first campaign mission.
6. Measure desktop CPU cost and validate memory/GPU assumptions before investing in PS4-specific runtime and packaging work.

The intended first PS4 milestone is an installable package that launches to the MW3 main menu. A package build alone is not proof of a working port.

## Documentation

- [MW3 bring-up notes](docs/MW3-BRINGUP.md) — executable identity, hook findings, recompiler work and open questions.
- [MW3 generated-code build](docs/MW3-BUILD.md) — build commands, CI asset handling and audit interpretation.
- [MW3 DPC investigation](docs/MW3-DPC-INVESTIGATION.md) — callback, record-array and completion-worker analysis; implementation decision.
- [Runtime architecture](docs/runtime.md)
- [Build prerequisites and pipeline](docs/building.md)
- [D3D9 integration seam](docs/d3d9-seam.md)
- [Rendering](docs/rendering.md), [audio](docs/audio.md), [switches](docs/switches.md)

## Scope

The initial target is the **offline single-player campaign**. Multiplayer services and Special Ops are outside the current bring-up milestone.

The PS4 direction is informed by [LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [gta4-ps4](https://github.com/plastikman/gta4-ps4), but no PS4-specific MW3 build has been validated. Address-space reservation, guest-memory mapping, GPU write-watch behavior, graphics integration and Jaguar CPU performance remain major risks.

## Credits

- [PaulCombal/mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled) — MW2 recompilation foundation and runtime.
- [hedge-dev/XenonRecomp](https://github.com/hedge-dev/XenonRecomp) — Xbox 360 PowerPC-to-C++ static recompiler.
- [OZORDI/LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [plastikman/gta4-ps4](https://github.com/plastikman/gta4-ps4) — related recompilation and PS4 porting references.
- [Xenia](https://github.com/xenia-project/xenia) — Xbox 360 reverse-engineering reference material.

## License

[GPL-3.0-only](LICENSE) applies to this project's own code. It does not grant rights to Activision's game code or data, or to generated code derived from proprietary game executables. Not affiliated with Activision, Infinity Ward, Sledgehammer Games, Microsoft or Sony.
