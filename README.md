# mw3recomp

**Call of Duty: Modern Warfare 3 (2011), Xbox 360 → PS4, by static recompilation.**

The game's PowerPC executable is converted ahead of time to C++ with [XenonRecomp](https://github.com/hedge-dev/XenonRecomp), then linked against a host runtime that stands in for the Xbox 360's kernel, GPU and audio hardware. It is not an emulator. No game files are included: you need your own disc.

> **Status: early bring-up. Nothing here runs MW3 yet, on PC or on PS4.**
> The runtime in this tree is inherited from a working Modern Warfare 2 port. The MW3 steps are written down, but none has been run against an MW3 executable. See [Status](#status).

## Contents

- [Plan](#plan)
- [Status](#status)
- [Why PS4 is hard](#why-ps4-is-hard)
- [Repository layout](#repository-layout)
- [Building (MW2 today)](#building-mw2-today)
- [Contributing](#contributing)
- [Credits](#credits)
- [License](#license)

## Plan

The work goes in this order, and each stage has to pass its check before the next begins.

1. **Desktop first.** Recompile MW3's campaign, boot it on Linux with the existing Vulkan runtime, and get the first mission to play at a stable frame rate.
2. **Measure.** Record the per-frame CPU cost on the desktop build. That number decides whether a PS4 build is worth starting.
3. **PS4 port.** Follow the approach of [gta4-ps4](https://github.com/plastikman/gta4-ps4): an OpenOrbis build with Mesa's RADV for Vulkan, plus audio and memory layers for the console, on a jailbroken PS4.

Scope for now is **the campaign, offline**. Multiplayer needs online services, and Special Ops comes after the campaign boots.

## Status

| Stage | State |
|---|---|
| Fork of [mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled) (runtime, tools, launcher) | Done. MW2 builds and runs on PC upstream |
| Recompiler config for MW3 | Done for the disc's `iw5sp_ship.exe`: [`config/MW3.toml`](config/MW3.toml) |
| Finding MW2's hooks in MW3's executable | Run. All 10 recompiler helpers and 8 engine hooks found, including `D3D_Present` and `Com_Error`; 7 not found. Details in [docs/MW3-BRINGUP.md](docs/MW3-BRINGUP.md#progress) |
| Recompiling MW3 | Done: `tools/recomp_mw3.sh` gives 142 C++ files with no switch-case errors, all passing a syntax check. Not linked or run yet |
| Booting MW3 on the desktop | Not started |
| PS4 build | Not started |

Open questions, such as which executable is the baseline (the disc's or the title-updated one) and whether MW3 needs the same fastfile-signing handling as MW2, are listed at the end of [docs/MW3-BRINGUP.md](docs/MW3-BRINGUP.md).

## Why PS4 is hard

Two problems to settle before anything else, and neither is known to work on the console:

- **Address space.** The runtime reserves 4 GiB of address space and maps the 512 MB of guest physical memory at two windows ([docs/runtime.md](docs/runtime.md#guest-address-space)).
- **GPU write-watch.** It is built on page-protection faults.

CPU speed is the other risk. gta4-ps4 reports GTA IV at about 20-25 FPS on a PS4 Pro, limited by the console's Jaguar cores, while MW3 targets 60 FPS on the Xbox 360. Hence the measuring stage above.

## Repository layout

| Path | What it is |
|---|---|
| `runtime/` | The host runtime: guest memory, threads, kernel and XAM stubs, GPU through Vulkan, audio |
| `launcher/` | Installer and settings UI, inherited from the MW2 port |
| `tools/` | Python and C++ helpers: XEX inspection and dumping, hook porting, shader and texture extraction, STFS unpacking |
| `config/` | Recompiler configs. `MW3.toml.in` is the MW3 template; the rest are MW2's |
| `patches/` | Local changes to XenonRecomp |
| `relay/` | MW2 multiplayer relay server (not used for MW3 yet) |
| `docs/` | Design notes. Start with [MW3-BRINGUP.md](docs/MW3-BRINGUP.md) |
| `pgo/` | Recorded compiler profiles for the MW2 builds |

## Building (MW2 today)

`./build.sh` runs the full pipeline for **Modern Warfare 2**: it fetches and patches XenonRecomp, extracts the executables and game data from your ISO, recompiles, and builds the runtime and launcher. There is no MW3 title in the build script yet; adding one is step 3 of the bring-up guide.

You need `cmake`, `ninja-build`, `clang-18`, `lld-18`, `python3`, `git`, the Vulkan headers and loader, and your own legally obtained disc image. The full list of requirements, variables and layout is in [docs/building.md](docs/building.md).

For the MW3 work, follow [docs/MW3-BRINGUP.md](docs/MW3-BRINGUP.md):

1. Put your disc's `default.xex` and game files under `mw3/` (git-ignored; never commit anything from the disc).
2. Dump the executable to a flat image with `tools/xexdump.py`.
3. Find MW2's hooks in MW3 with `tools/port_hooks.py`.
4. Fill in `config/MW3.toml`, recompile, then try to boot.

## Contributing

Help is most useful on the bring-up steps above, and on the PS4 memory and GPU questions. Docs worth reading first:

- [docs/MW3-BRINGUP.md](docs/MW3-BRINGUP.md): the step-by-step plan, with a check after each step
- [docs/runtime.md](docs/runtime.md): how the runtime works
- [docs/d3d9-seam.md](docs/d3d9-seam.md): the D3D9 ring buffer and flip handshake, a likely early failure point
- [docs/switches.md](docs/switches.md), [docs/rendering.md](docs/rendering.md), [docs/audio.md](docs/audio.md): subsystem notes

Crash, glitch and performance reports for the MW2 build go through the launcher's REPORT A BUG button, as described in the issue template.

## Credits

- [PaulCombal/mw2-recompiled](https://github.com/PaulCombal/mw2-recompiled): the Modern Warfare 2 recompilation this project is forked from
- [hedge-dev/XenonRecomp](https://github.com/hedge-dev/XenonRecomp): the PowerPC-to-C++ recompiler
- [OZORDI/LibertyRecomp](https://github.com/OZORDI/LibertyRecomp) and [plastikman/gta4-ps4](https://github.com/plastikman/gta4-ps4): the GTA IV recompilation and its PS4 port, which this project's PS4 plan follows
- [Xenia](https://github.com/xenia-project/xenia): the XMA decoder branch of FFmpeg used for audio

## License

[GPL-3.0-only](LICENSE). It covers this project's own code: the runtime, tools, patches and build files. It grants nothing for the game: Activision's code and data, and the C++ generated from the game's executables, are not part of this repository and not under this license.

No game code or data is included; builds only run with files from your own disc. Not affiliated with Activision, Infinity Ward, Sledgehammer Games, Microsoft, Sony or Valve.
