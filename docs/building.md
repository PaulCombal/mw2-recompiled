# Building from source

The retail Modern Warfare 2 disc's executables, as title update 6 patches them,
are converted to C++ with
[XenonRecomp](https://github.com/hedge-dev/XenonRecomp), compiled as native
x86-64, and linked with a host runtime that stands in for the console's kernel,
GPU (through Vulkan) and audio hardware ([runtime.md](runtime.md)).

The game data, the generated C++ and the XenonRecomp checkout are gitignored.
Everything under version control is hand-written or a patch.

## Requirements

- `cmake`, `ninja-build`, `clang-18`, `lld-18`, `python3`, `git`; `llvm-18`
  (`llvm-profdata-18`) to record a profile.
- The Vulkan headers and loader (`libvulkan-dev`). Without them the runtime
  builds with no window.
- SDL3 and Dear ImGui are fetched; SDL3 is linked statically; on Linux it needs the usual
  development packages for its video, audio and input backends (X11, Wayland,
  ALSA/PulseAudio/PipeWire, udev, dbus). `.github/workflows/release.yml` lists
  them.
- The retail ISO, or at least `mw2/tu0/default.xex` and
  `mw2/tu0/default_mp.xex`.
- Title update 6: its package, or its files in `mw2/update/`
  ([Title update 6](#title-update-6)).

## build.sh

`./build.sh` does everything, in order:

1. clones XenonRecomp at a pinned commit and applies
   `patches/xenonrecomp-mw2.patch` (an existing `XenonRecomp/` is reused as it
   is);
2. builds XenonRecomp and XenonAnalyse;
3. extracts `default.xex` and `default_mp.xex` from the ISO into `mw2/tu0/`,
   and every `.ff`, `.pak` and `.bik` into `mw2/game/` (files already there
   are kept); then applies the title update, which writes the executables the
   rest works on, `mw2/default.xex` and `mw2/default_mp.xex`
   ([Title update 6](#title-update-6));
4. writes the flat PE image (`mw2/default.pe` or `mw2/default_mp.pe`) with
   `tools/xexdump.py`;
5. finds jump tables with XenonAnalyse (`config/mw2_switch_tables.toml`,
   `config/mw2mp_switch_tables.toml`);
6. recompiles into `ppc/` or `ppc_mp/`;
7. generates `ppc_recomp_shared.h` and the kernel import stubs
   (`tools/genshared.py`, `tools/gen_kernel_stubs.py`);
8. configures and builds the runtime and the launcher with CMake.

| variable | effect |
|---|---|
| `TITLE=sp\|mp` | `sp` (default): `default.xex`, the campaign and special ops. `mp`: `default_mp.xex`, the multiplayer. Each has its own recompiled tree, switch tables, recompiler config and build directory |
| `TU=<path>` | the title update: its package, or a folder holding its files. Needed until `mw2/update/` has them |
| `VERSION=tu0` | builds the disc's own executables, without the update; see [The disc version](#the-disc-version) |
| `RELEASE=1` | builds without diagnostics (`-DMW2_DIAGNOSTICS=OFF`) and, on Linux, with the C++ runtime linked statically (`MW2_PORTABLE`); adds `-release` to the build directory |
| `WINDOWS=1` | cross-compiles for Windows with llvm-mingw; the build directory becomes `build-win...` and the executable `mw2.exe` |
| `LLVM_MINGW=<dir>` | the unpacked [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) release, required by `WINDOWS=1` |
| `ISO=<path>` | the disc image. Without one, `mw2/tu0/default.xex` and `mw2/tu0/default_mp.xex` are enough to build, and no game data is extracted |
| `BUILD_DIR=<dir>` | builds there instead of the default directory |
| `TARGETS="<target> ..."` | builds only those targets (`mw2`, `launcher`, ...) instead of all of them |
| `REGENERATE=0` | keeps the recompiled tree already there (skips steps 5-7), for a second build of the same title, e.g. with another `ONLINE` |
| `CMAKE_EXTRA` | passed to the configure step (a compiler launcher, `FETCHCONTENT_BASE_DIR`, ...) |

Build directories:

| | campaign | multiplayer |
|---|---|---|
| diagnostic | `build/` | `build-mp/` |
| `RELEASE=1` | `build-release/` | `build-mp-release/` |
| `WINDOWS=1` | `build-win/`, `build-win-release/` | `build-win-mp/`, `build-win-mp-release/` |

A second build directory reuses the SDL3 and FFmpeg sources `build/` fetched.

### Title update 6

The update is a package holding a patch for each executable (`default.xexp`,
`default_mp.xexp`) and three fastfiles for the multiplayer (`patch_mp.ff`,
`dlc1_ui_mp.ff`, `dlc2_ui_mp.ff`). After extracting the disc's executables,
`build.sh`:

- unpacks the package (`tools/stfs.py`) into `mw2/update/`, or copies the
  files from the folder `TU` names;
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

`VERSION=tu0` builds the disc's executables as they are. Everything of theirs
carries the version: the image and a game folder in `mw2/tu0/`, the configs
`config/MW2.tu0.toml` and `config/MW2MP.tu0.toml`, the trees `ppc_tu0/` and
`ppc_mp_tu0/`, and `-tu0` on the build directory. The build defines
`MW2_VERSION_TU0`, which selects the disc's blocks in `runtime/title.h`.

    VERSION=tu0 TITLE=mp ./build.sh
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

CMake reads the SHA-256 of the executables in `mw2/` and `mw2/tu0/` and of the
update's fastfiles in `mw2/update/` at configure time: a game executable runs
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
it is, the right half in the multiplayer's colours. `tools/readme_art.py` draws the
README's banner, tiles and headings into `docs/images/readme/`.

| file | what |
|---|---|
| `main.cpp` | the window's loop, what each entry does, the file dialog and dropped files, and the terminal mode |
| `single.cpp` | one window a copy: a second start brings the first up in front and ends |
| `ui.cpp` | the one screen, drawn by hand: a computed backdrop whose smoke drifts, the column of entries, the pane, the wordmark |
| `setup.cpp` | the install: checks the disc, gets the update, patches the executables, copies the files |
| `disc.cpp` | reads a disc image (XDVDFS) or an extracted folder |
| `package.cpp` | reads the update's package (STFS), as `tools/stfs.py` does |
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
and no disc is asked for. A `VERSION=tu0` build installs the disc as it is.

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
when it is not an unsigned one of the game's own version. An entry with a range (FOV) sets
`ui::Frame::slider`, and the pane draws a track for it; FOV's also shows a street drawn at the
angle (`preview.cpp`), with the game's projection, to say what the number does before the game
is started.
PLAY CAMPAIGN and PLAY MULTIPLAYER start `mw2-sp` and `mw2-mp` beside the
launcher, the names a release gives the two game executables. Before that,
and before REPORT A BUG starts either, the launcher checks the compiled
shaders, and compiles them first when they no longer fit (SHADERS, with its
progress and CANCEL). It does the same after an install.

    mw2-launcher --compile-shaders

compiles them from a terminal, whether they fit or not, and prints what it
did. `mw2-launcher --play campaign|multiplayer [--device <id>]` is how the game
hands over when its shaders do not fit: the window, the compile, then the game.

The shader code's build, `MW2_SHADER_BUILD`, is a SHA-256 that CMake takes at
configure time of the sources that decide what a compiled shader is (the
translator, the pipeline layout, `shader_build.*`, `launcher/precompile.*` and
`launcher/zone/`). A change to any of them makes a player's copy compile again.

### Updating itself

CHECK FOR UPDATES (`update.cpp`) looks only when the player asks. It fetches
GitHub's description of the newest published release
(`api.github.com/repos/.../releases/latest`), which has the release's tag and,
for each archive, its name, size, SHA-256 and address, and compares the tag
with its own. The workflow gives the build its tag
(`RELEASE_TAG=v0.3.0 ./build.sh`, CMake's `MW2_RELEASE_TAG`); a build without
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
changes of page protection a second of play (`gpu/memory_watch.cpp`), and
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

`WINDOWS=1 TITLE=mp RELEASE=1 LLVM_MINGW=<dir> ./build.sh` produces
`build-win-mp-release/mw2.exe` with `cmake/mingw-w64.cmake` (clang for
`x86_64-w64-mingw32` against the UCRT). XenonRecomp itself is still built with
the host's clang-18. The executable needs nothing beside it:

- the C++ runtime and threads are linked statically;
- `vulkan-1.dll` comes with the GPU driver, so `cmake/vulkan-windows.cmake`
  fetches the Vulkan headers and makes the import library from them;
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
([relay.md](relay.md)) for amd64 and arm64, pushes it to
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
| `build.sh` | the whole build |
| `CMakeLists.txt`, `cmake/` | the runtime's build; the Windows toolchain and Vulkan import library |
| `patches/` | local changes to XenonRecomp |
| `config/MW2.toml`, `config/MW2MP.toml` | recompiler config, campaign and multiplayer; `*.tu0.toml` for the disc version |
| `ppc/`, `ppc_mp/` | generated C++ and its generated glue (gitignored) |
| `runtime/` | the host runtime |
| `tools/` | generators, reverse-engineering scripts, capture helpers |
| `tools/routes/` | recorded walks and flights for `MW2_WALK_PATH` and `MW2_FLY_PATH` |
| `pgo/` | the recorded profiles the build is optimised with ([Optimisation](#optimisation)) |
| `launcher/` | the launcher: install, and starting the game ([The launcher](#the-launcher)) |
| `relay/` | the relay server for NETWORK SERVER, and its Dockerfile ([relay.md](relay.md)) |
| `third_party/ffmpeg-xenia/` | builds the XMA frame decoder from Xenia's FFmpeg branch, fetched at configure time |
| `third_party/fonts/` | the launcher's fonts (Barlow), and Michroma for the README's titles; SIL Open Font License |
| `docs/` | this documentation |

### The runtime

| path | what |
|---|---|
| `main.cpp` | reserves the guest space, loads the image, starts the title, ends the run |
| `guest.h`, `guest_memory.*` | guest pointers, big-endian values, the address space |
| `env.h`, `diagnostics.h`, `log.h`, `counter.h`, `words_hash.h` | switch readers, the diagnostics gate, the log, small helpers |
| `platform.*` | what Linux and Windows spell differently |
| `title.h` | the guest addresses the runtime names, per executable |
| `install/` | a player's start: the executable check, XEX decryption, the hand-over to the launcher |
| `kernel/` | kernel and XAM imports: memory, objects and waits, threads, files, content and profile, input, networking, video, audio |
| `apu/` | the XAudio render driver and the XMA decoder |
| `online/` | the online services (`lan`, `server`, `steam`), chosen as the game starts |
| `gpu/` | the command processor: ring and PM4 packets, interrupts, D3D9's command-buffer arena, texture fetch and tiling, the D3D9 hooks, the write watch (`memory_watch.*`) |
| `gpu/vulkan/` | the Vulkan renderer: draws, targets, resolves, frames, pipelines and pipeline libraries, the shader translator, the texture cache, the recorder thread, the display table, the presenter window, RenderDoc capture |
| `image_move.cpp` | the image pool's block copy, which the console makes with a memory-export draw |
| `shader_preload.cpp` | hands shaders loaded with a level to the renderer to compile |
| `engine.h`, `engine_log.cpp`, `predicate_waits.cpp`, `console.cpp` | hooks into the engine: print and error paths, spin waits, the console command buffer |
| `player.cpp` | the headless walker and route recorder |
| `field_of_view.cpp` | `MW2_FOV`: scales the view angle the title computes |
| `shadow_resolution.cpp`, `shadow_resolution.h` | which depth surface the title made for its shadow maps, for `MW2_SHADOW_SCALE` |
| `texture_filtering.cpp` | `MW2_ANISOTROPY`: the title's least anisotropic filtering, registered with the setting as its default |
| `render_size.cpp`, `render_size.h` | `MW2_RENDER`: the title's frame and scene sizes, the picture's shape, and 16:9 pictures covering another shape |
| `unsigned_fastfiles.cpp`, `custom_map_list.cpp` | lets the multiplayer load a fastfile nobody signed, a converted map, and lists the map from its `.arena` |
| `stutters.cpp`, `pacing_trace.cpp` | the stutter detector and the frame pacing timeline |
| `report.cpp` | `MW2_REPORT=1`: the driver's name and a run's performance summary, for a bug report |
| `crash.cpp`, `watchpoint.cpp`, `mmio_hook.h` | the fault handler, the write watchpoint, the hardware-register store hook |

### Tools

| path | what |
|---|---|
| `xdvdfs.py`, `xexdump.py`, `xexinfo.py` | read the ISO; decrypt and decompress an XEX2 into a flat PE; dump its headers |
| `stfs.py`, `xexpatch.cpp` | unpack a title update's package; apply an executable patch (`.xexp`) with XenonRecomp's patcher |
| `title.py` | which executable the other tools work on (`MW2_TITLE=sp\|mp`, `MW2_VERSION=tu6\|tu0`) |
| `fixbounds.py` | explicit function boundaries for leaf functions holding jump tables |
| `genshared.py`, `gen_kernel_stubs.py` | `ppc_recomp_shared.h`, the import list, and stubs for unimplemented imports |
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

`patches/xenonrecomp-mw2.patch`, applied by `build.sh`:

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
- **`XenonAnalyse/main.cpp`**: jump-table patterns for XDK 8276's instruction
  order, which schedules the index shift between the `lis` and the `addi`; the
  table and base address pairs are located by opcode rather than fixed offset,
  and `nop` padding inside a pattern is skipped.
- **`XenonAnalyse/function.cpp`**: a `bclr` ends a function only when its BO
  ignores both the condition and the counter. `bdzlr` tests the counter, and
  treating it as a return cut `memset` short.
- **`XenonUtils/ppc_context.h`**: `simde_mm_vctuxs` (float to unsigned word with
  saturation), and the `PPC_QUERY_TIMEBASE` declaration.
