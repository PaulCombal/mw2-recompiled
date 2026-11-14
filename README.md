# mw3recomp

**Call of Duty: Modern Warfare 3 (2011, Xbox 360): static recompilation research and an eventual PS4 port.**

The goal is the offline single-player campaign of MW3 (IW5), running natively. [XenonRecomp](https://github.com/hedge-dev/XenonRecomp) translates the Xbox 360 PowerPC executable into C++, which is compiled together with a native host runtime. The approach comes from [mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled). This is **not** Xbox 360 emulation.

> [!IMPORTANT]
> **Status (14 November 2026):** the generated MW3 code compiles, a diagnostic desktop runner links, and locally the guest has been seen running through early startup and graphics submission. **The main menu has not been reached. There is no playable build, no PS4 executable and no FPKG.**

## No game files, ever

This repository contains no game data and never will.

- You supply your own legally obtained disc dump. It stays on your machine under `mw3/`, which is git-ignored.
- Game files are never uploaded anywhere: not to this repository, not to a private asset repository, not to Git LFS and not to CI.
- CI is asset-free by design. Everything that needs the executable or game data runs locally only.
- [`tools/check_no_game_files.py`](tools/check_no_game_files.py) fails CI if a game file, an LFS pointer or an LFS rule is tracked. Run it with `--staged` as a pre-commit hook.
- Do not commit `.xex`, `.pe`, `.ff`, `.ffm`, `.pak`, generated PPC source or build outputs, and do not create placeholder files to imitate missing ones.

## Where things stand

| Area | Status |
|---|---|
| Executable analysis | Done for the supported campaign XEX (`iw5sp_ship.exe`, title ID `0x415608CB`, XEX version `0x16`) |
| Static recompilation | 142 PPC translation units, 128 jump tables |
| Generated code compiles | Yes, as `libppc_recomp.a` |
| Desktop runner links | Yes, as a diagnostic executable |
| Startup audit | Entry-point call graph and import coverage reports |
| Early guest execution | Seen locally: sign-in, file requests, graphics submissions, frame swaps |
| Kernel / XAM semantics | In progress; name-level coverage is not proof of compatibility |
| Main menu and campaign | Not reached |
| PS4 port | Not started |

A successful compile, link or smoke run is a development milestone, not a boot milestone. The 17 GPU swaps seen in the last local run show progress through early submission; they do not prove the host GPU initialized or that a usable frame was drawn.

## Quick start

### Checks that need no game files

These are what CI runs, and they work on any machine:

```sh
python3 tools/check_no_game_files.py
python3 tools/port_hooks.py --selftest
cmake -S . -B build-tests -G Ninja -DMW2_TESTS_ONLY=ON -DCMAKE_BUILD_TYPE=Debug
cmake --build build-tests --parallel 2
ctest --test-dir build-tests --output-on-failure
```

The `kernel_*` and `gpu_*` tests compile the runtime's real kernel and GPU sources against a fake guest memory (`tests/fake_guest.h`) and a stub of the generated header (`tests/stubs/`).

You need CMake 3.20 or newer, Ninja, a C++20 compiler and Python 3.11 or newer.

### Building against your own executable

1. Put your supported campaign executable at `mw3/tu0/default.xex`.
2. Install CMake, Ninja, Clang 18, Python 3, Git and binutils (`nm`, `objdump`, `c++filt`).
3. Run:

```sh
bash tools/build_mw3.sh
```

This extracts and recompiles the executable, compiles the generated library to `build-mw3/libppc_recomp.a`, and writes static audit reports. It does not produce a playable game.

To run the bounded diagnostic smoke test with your own game root, see [docs/MW3-LOCAL-RUN.md](docs/MW3-LOCAL-RUN.md). Linux x86-64 (or WSL2 with Ubuntu 24.04) is the supported host.

## How it works

1. **Extract:** `xexdump.py` turns the XEX into a flat image; the XEX identity is pinned and checked before anything else.
2. **Analyse and recompile:** XenonAnalyse finds jump tables, XenonRecomp emits C++ from `config/MW3.toml`.
3. **Link with the host runtime:** guest memory, threads, kernel and XAM handlers, DPC dispatch, a Vulkan-side GPU seam and audio replace the console. MW3-specific hooks and D3D device offsets are added one at a time; MW2 addresses are never reused without checking them against the MW3 image.
4. **Run and iterate:** each failure (a missing import, a file request, a deadlock) is traced to a cause and fixed against evidence rather than by name-matching.

## What has been learned so far

- **Entry:** guest entry `0x82459E58`, image base `0x82000000`. The generated-code audit indexes about 36,000 functions.
- **Imports:** 258 kernel/XAM imports inventoried. Three reachable ones still use logging stubs (`NetDll_WSACreateEvent`, `NetDll___WSAFDIsSet`, `XamShowFriendRequestUI`); none is a known blocker for the offline campaign.
- **File system:** failed opens now log the guest API, device, path and parameters. A read-only zero-length object stands in for `\Device\Harddisk0\Partition0`, modelled on Xenia's null device.
- **Disc layout:** the title asks for `update:\patch.ff`, `D:\patch.ff`, `D:\patch.ffm` and the `imagefile*.pak` packs, and exits through its dirty-disc path when a required one is missing. Which requests are required versus optional is decided from evidence, not assumed.
- **DPC and graphics completion:** a six-entry structure array, a callback, a four-slot completion ring and an event-driven worker form the graphics-interrupt path. A DPC queue implementation with unit tests exists but is **not yet validated against a live boot**; IRQL, affinity, timer delivery and ordering are open.

Details: [bring-up notes](docs/MW3-BRINGUP.md), [DPC investigation](docs/MW3-DPC-INVESTIGATION.md), [startup-import plan](docs/MW3-STARTUP-IMPORT-PLAN.md).

## Roadmap

1. **Asset-free runtime validation (underway):** the runtime's own sources are tested against synthetic inputs, with no game data. Covered so far: dispatcher objects and waits, the file layer, the guest heap, the memory and thread imports, and the GPU command processor fed hand-built PM4 streams. Still to come: XAM and the remaining kernel imports, DPC delivery, and the renderer seam.
2. **Local boot work:** resolve the remaining file requests, get past the dirty-disc path and capture the next startup stage, all on your own machine.
3. **Validate runtime semantics** against actual execution.
4. **Main menu and first mission on desktop:** input, audio, saves, rendering and performance.
5. **Only then PS4:** OpenOrbis integration, guest-memory mapping, Vulkan/RADV compatibility, controller and audio, profiling and packaging.

Out of scope for now: multiplayer services and Special Ops.

## Documentation

- [Build and local workflow](docs/MW3-BUILD.md), [running locally](docs/MW3-LOCAL-RUN.md), [CI](docs/ci.md)
- [Build prerequisites](docs/building.md) and [runtime architecture](docs/runtime.md)
- [D3D9 integration](docs/d3d9-seam.md), [rendering](docs/rendering.md), [audio](docs/audio.md)

## References and credits

- [mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled): runtime and recompilation foundation
- [XenonRecomp](https://github.com/hedge-dev/XenonRecomp): Xbox 360 PowerPC static recompiler
- [LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [gta4-ps4](https://github.com/plastikman/gta4-ps4): related recompilation and PS4 porting work
- [Xenia](https://github.com/xenia-project/xenia): Xbox 360 compatibility reference

## License

Code is licensed under [GPL-3.0-only](LICENSE). Game content belongs to its owners and is not part of this project.
