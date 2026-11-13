# mw3recomp

**Call of Duty: Modern Warfare 3 (2011, Xbox 360) — static recompilation and PS4 port research.**

This project adapts the [MW2-recompiled](https://github.com/PaulCombal/mw2-recompiled) runtime and tooling to bring up the MW3 single-player campaign. [XenonRecomp](https://github.com/hedge-dev/XenonRecomp) translates the Xbox 360 PowerPC executable into C++, which is then compiled for the host. This is static recompilation, not Xbox 360 emulation.

> **Current milestone:** generated-code compilation, desktop runtime linking, and first-stage guest execution pass in CI. The title then hits a fatal error, `Could not find zone 'ui.ff'` (the game root staged in CI has no `ui.ff`), and its error handler asks to relaunch `default.xex`, where the development runner stops. **The main menu and gameplay have not been reached; there is no playable desktop or PS4 build.**

## Status

| Area | Status |
|---|---|
| XEX analysis and static recompilation | **Working:** 142 generated C++ translation units; 128 jump tables |
| Generated PPC compilation | **Pass:** builds `libppc_recomp.a` |
| Runtime integration | **In progress:** diagnostic desktop executable links; MW3-specific hooks and kernel semantics still need validation |
| First-stage execution | **Pass:** guest entry, kernel/thread/audio initialization and GPU command submission observed; stops on a missing-zone fatal error (`ui.ff` not found), which the title answers with `XamLoaderLaunchTitle("default.xex")` |
| Past the first fastfile load (`ui.ff`, then the rest) / main menu | **Not reached** |
| Campaign gameplay | **Not started** |
| PS4 build and package | **Not started** |

The first-stage CI smoke test observes 26 GPU draw calls, but GPU device initialization is not yet validated: the CI runner's Vulkan driver is incompatible. The `default.xex` relaunch is the title's fatal-error restart, not a second executable, so there is nothing to chain-load: the fix is the missing game file. These results establish progress through early startup, not a successful boot.

## Build

The local build script currently compiles and audits the generated MW3 code; it does not produce a playable game:

1. Place the supported campaign executable at `mw3/tu0/default.xex`.
2. Install CMake, Ninja, Clang 18, Python 3, Git, and the binutils tools (`nm`, `objdump`, `c++filt`).
3. Run:

```sh
bash tools/build_mw3.sh
```

The script uses the checked-in `config/MW3.toml` and switch-table configuration, runs the MW3 extraction/recompilation pipeline, compiles the generated PPC library, runs focused tests, and produces static audit reports. The main output is `build-mw3/libppc_recomp.a`; generated source and build files are kept in ignored local directories.

### CI build

The GitHub Actions workflow can run the full recompile, diagnostic desktop link, and first-stage smoke test using a private asset repository. Configure the `MW3_ASSET_REPO` repository variable and `MW3_ASSET_REPO_TOKEN` secret as described in [docs/MW3-BUILD.md](docs/MW3-BUILD.md). The workflow checks the supported XEX identity and stages the extracted game-root layout for runtime file access.

## Next milestones

1. Supply the fastfiles the title asks for (`ui.ff` first, from your own disc dump) and fix what the next error says. Each missing zone ends the run with a readable `ERROR:` line in the log.
2. Resolve MW3-specific engine hooks, globals, and runtime behavior against the supported XEX.
3. Implement and test the imports required to progress through startup; replace diagnostic stubs only when their semantics are understood.
4. Validate DPC scheduling and graphics-completion behavior, then resolve Vulkan/device initialization.
5. Reach the main menu and first campaign mission on desktop before beginning PS4-specific memory, graphics, performance, and packaging work.

## Documentation

- [MW3 bring-up notes](docs/MW3-BRINGUP.md) — executable identity, hook findings, and open questions.
- [MW3 build and CI setup](docs/MW3-BUILD.md) — build scope, private asset staging, and audit details.
- [Startup-import implementation plan](docs/MW3-STARTUP-IMPORT-PLAN.md) — prioritized import analysis and tests.
- [DPC investigation](docs/MW3-DPC-INVESTIGATION.md) — graphics-completion path and DPC implementation findings.
- [Build prerequisites](docs/building.md) and [runtime architecture](docs/runtime.md).
- [D3D9 integration](docs/d3d9-seam.md), [rendering](docs/rendering.md), and [audio](docs/audio.md).

## Scope and references

The initial target is the **offline single-player campaign**. Multiplayer services and Special Ops are outside the current milestone.

Related work informing the PS4 direction includes [LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [gta4-ps4](https://github.com/plastikman/gta4-ps4). No MW3-specific PS4 build has been validated.

## Credits

- [PaulCombal/mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled) — runtime and recompilation foundation.
- [hedge-dev/XenonRecomp](https://github.com/hedge-dev/XenonRecomp) — Xbox 360 PowerPC static recompiler.
- [OZORDI/LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [plastikman/gta4-ps4](https://github.com/plastikman/gta4-ps4) — related recompilation and PS4 porting references.
- [Xenia](https://github.com/xenia-project/xenia) — Xbox 360 reverse-engineering reference.

## License

The project's code is licensed under [GPL-3.0-only](LICENSE).
