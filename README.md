# mw3recomp

**Call of Duty: Modern Warfare 3 (2011, Xbox 360) — static recompilation research and eventual PS4 port.**

This project is bringing up the offline single-player campaign of MW3 (IW5) using the static-recompilation approach pioneered by [mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled). [XenonRecomp](https://github.com/hedge-dev/XenonRecomp) translates the Xbox 360 PowerPC executable into C++, which is compiled with a native host runtime.

This is **not Xbox 360 emulation**. The long-term target is a native PS4 build, but current work is still focused on getting the recompiled title through startup on a desktop host.

> [!IMPORTANT]
> **Current state (13 November 2026):** the MW3 generated PPC library builds, the diagnostic desktop executable links, and CI runs the guest through early startup and graphics submission. The latest smoke run gets past the previously observed `\\Device\\Harddisk0\\Partition0` request, then encounters missing disc files (`patch.ff`, `patch.ffm`, `imagefile4.pak`, and `imagefile4.pakm`) and exits through the title's dirty-disc path. **The main menu has not been reached. There is no playable desktop build, PS4 executable, or FPKG.**

## Project status

| Area | Current status |
|---|---|
| Target executable analysis | **Established** for the supported MW3 campaign XEX; identity is pinned in the build workflow |
| Static recompilation | **Generated:** 142 PPC translation units and 128 jump tables |
| Generated PPC compilation | **Pass:** produces `libppc_recomp.a` |
| Runtime compile/link integration | **Pass in CI:** diagnostic desktop executable links against the generated MW3 library |
| Static startup audit | **Implemented:** entry-point call-graph and import coverage reports are generated in CI |
| Early guest execution | **Observed:** guest startup, profile sign-in, file-system requests, graphics submissions and frame swaps |
| File-system compatibility | **In progress:** device/path diagnostics and a `Harddisk0\\Partition0` null-device implementation are present |
| Kernel / XAM semantics | **In progress:** several handlers and focused tests exist; name-level coverage is not proof of MW3 compatibility |
| Main menu / campaign | **Not reached** |
| PS4 port / package | **Not started** |

### What the latest smoke run establishes

- The title enters guest execution and reaches graphics frame submission.
- The file diagnostics added in [PR #28](https://github.com/the-drk/mw3recomp/pull/28) identify the guest API, device, relative path and open parameters for failed file opens.
- [PR #29](https://github.com/the-drk/mw3recomp/pull/29) adds a read-only, zero-length virtual object for the observed `\\Device\\Harddisk0\\Partition0` path, following the null-device model used by Xenia. The latest run no longer reports that object as a missing host file.
- The latest smoke log proceeds to requests for `update:\\patch.ff`, `D:\\patch.ff`, `D:\\patch.ffm`, `D:\\imagefile4.pak` and `D:\\imagefile4.pakm`. The title reports a dirty-disc error and exits to the dashboard.
- The run reports 17 GPU swap packets. This is evidence of progress through early rendering submission, **not** proof that the host GPU device initialized correctly or that the game rendered a usable frame.

The smoke executable is a diagnostic host runner. Its success means the build and diagnostic run completed as expected; it does not mean the game booted successfully.

## Technical progress

### Recompiler and static analysis

- Supported campaign executable: `iw5sp_ship.exe`, title ID `0x415608CB`, XEX version `0x16`.
- Guest entry point: `0x82459E58`; image base: `0x82000000`.
- XenonAnalyse identifies 128 jump tables.
- XenonRecomp emits 142 PPC source translation units.
- The generated-code audit currently indexes approximately 36,235 function definitions and 36,493 function references.
- The startup call-graph audit follows direct calls from the entry point. Its reachability results are static approximations: indirect calls, function pointers, callbacks, data imports and dynamically selected thread entry points are not fully represented.

### Runtime and import coverage

The CI audit inventories 258 kernel/XAM imports and separates reachable imports from those elsewhere in the executable. Current source-level handler matching is useful for triage, but it is only a name/macro match; it does not establish correct signatures or Xbox 360 behavior.

The startup audit currently identifies three reachable imports that still use generated logging stubs:

- `NetDll_WSACreateEvent`
- `NetDll___WSAFDIsSet`
- `XamShowFriendRequestUI`

These are not currently considered blockers for the initial offline-campaign bring-up. Revisit them if runtime evidence shows they are required.

Other runtime work includes:
- MW3 title configuration and selected engine hooks/globals, with MW3-specific D3D device offsets rather than blindly reusing MW2 offsets.
- Kernel/XAM implementations and offline networking support added during bring-up.
- `RtlRaiseException` handling for the thread-naming exception observed during startup.
- File-open diagnostics that expose the actual guest path and open contract.
- A virtual null-device implementation for the observed `Harddisk0\\Partition0` object.

### DPC / graphics-completion investigation

The DPC investigation traced a six-entry structure array, matching initialization and insertion paths, a callback, a four-slot completion ring and an event-driven worker. This is strong evidence that the path participates in graphics-interrupt/completion handling.

A dedicated DPC queue implementation and unit tests now exist. Tests cover duplicate insertion, argument capture, FIFO ordering, callback requeue, reinitialization and worker wake/stop behavior. The implementation is **not yet validated against a live MW3 boot**; IRQL, affinity, timer delivery and exact ordering against the graphics interrupt path remain open.

## Build

The local build script compiles and audits the generated MW3 code. It does not produce a playable game.

1. Place your own supported campaign executable at `mw3/tu0/default.xex`.
2. Install CMake, Ninja, Clang 18, Python 3, Git and the required binutils tools (`nm`, `objdump`, `c++filt`).
3. Run:

```sh
bash tools/build_mw3.sh
```

The script uses `config/MW3.toml`, runs the MW3 extraction/recompilation pipeline, compiles the generated PPC library, runs focused checks and produces static audit reports. The principal output is:

```
build-mw3/libppc_recomp.a
```

Generated PPC source and build outputs are local artifacts and are not committed.

For the diagnostic desktop link probe and the full CI workflow, see [docs/MW3-BUILD.md](docs/MW3-BUILD.md).

## Game data and repository hygiene

Use only game files you are legally entitled to use. Keep the XEX, extracted game data, fastfiles, image packs, generated PPC source and build outputs out of this public repository.

- The supported executable and extracted game-root layout are local inputs.
- Do not commit `.xex`, `.pe`, `.ff`, `.ffm`, `.pak`, proprietary generated PPC source or runtime game data.
- The optional CI asset workflow uses a separate private asset repository. Large files can consume substantial Git LFS storage and Actions download bandwidth; local testing is preferable when the remaining required assets are large.
- Do not create placeholder files or rename a different asset to imitate a missing filename.

The initial target is the **offline single-player campaign**. Multiplayer services and Special Ops are outside the current milestone.

## Next milestones

1. **Resolve the remaining file requests.** Establish the correct local game-root/device mapping and supply the exact files the title requests. Do not assume that `patch.*` files are required or optional without evidence; determine the role of each request from the next run.
2. **Run against the complete local data set.** Avoid repeatedly downloading multi-gigabyte assets through CI; keep proprietary files local and retain only sanitized diagnostics.
3. **Advance past the dirty-disc path.** Capture the next fatal error or startup stage, then fix the underlying file-system or runtime behavior.
4. **Validate runtime semantics.** Continue checking kernel/XAM handlers, DPC dispatch and graphics-completion ordering against actual execution rather than source-name matches alone.
5. **Reach the main menu and first campaign mission on desktop.** Validate input, audio, saves, rendering and performance.
6. **Only then begin PS4-specific work:** OpenOrbis integration, guest-memory mapping, Vulkan/RADV compatibility, controller/audio integration, performance profiling and package generation.

A successful compile, link or smoke-test job is a development milestone—not a boot milestone. The project will report those separately.

## Documentation

- [MW3 bring-up notes](docs/MW3-BRINGUP.md) — executable identity, hook findings, fastfile history and open questions.
- [MW3 build and CI setup](docs/MW3-BUILD.md) — build targets, private asset staging and audit details.
- [Startup-import implementation plan](docs/MW3-STARTUP-IMPORT-PLAN.md) — import triage, evidence requirements and tests.
- [DPC investigation](docs/MW3-DPC-INVESTIGATION.md) — callback, object-array and graphics-completion analysis.
- [Build prerequisites](docs/building.md) and [runtime architecture](docs/runtime.md).
- [D3D9 integration](docs/d3d9-seam.md), [rendering](docs/rendering.md) and [audio](docs/audio.md).

## Related projects and references

- [mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled) — runtime and recompilation foundation.
- [XenonRecomp](https://github.com/hedge-dev/XenonRecomp) — Xbox 360 PowerPC static recompiler.
- [LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [gta4-ps4](https://github.com/plastikman/gta4-ps4) — related recompilation and PS4 porting references.
- [Xenia](https://github.com/xenia-project/xenia) — Xbox 360 reverse-engineering and compatibility reference.

## Credits

- [PaulCombal/mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled)
- [hedge-dev/XenonRecomp](https://github.com/hedge-dev/XenonRecomp)
- [OZORDI/LibertyRecomp](https://github.com/OZORDI/LibertyRecomp)
- [plastikman/gta4-ps4](https://github.com/plastikman/gta4-ps4)
- [Xenia](https://github.com/xenia-project/xenia)

## License

The project's code is licensed under [GPL-3.0-only](LICENSE).
