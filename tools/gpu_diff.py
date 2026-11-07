#!/usr/bin/env python3
# The renderer's moved parts against their C++ (docs/building.md, "The renderer moves
# to Rust"): the C++ files as they are and the Rust of src/runtime/ in one program,
# on generated cases, compared byte for byte. Each rig is tools/gpu_diff/<rig>.cpp,
# which calls the Rust through the C functions the C++ renderer itself calls, or, for
# the presenter's, backdrop's and gpu_time's decisions, through the rig's own
# (exports.rs, mod rig) while the C++ runs on stand-in Vulkan and SDL calls
# (presenter_rig.h): neither side makes a device or a window. The arena's progress and
# the interrupts (gpu.cpp, arena.cpp) are held against the Rust through the functions
# the C++ command processor calls, the interrupts on a guest memory of the rig's own
# with a stub of the title's handler (interrupts.cpp), the consumer's and the vblank's
# loops on a clock of the rig's (gpu_cpp.cpp, gpu.rs's rig_loop_*); these two compare
# what each side printed too (rig_log.h), so their Rust is built with its log on.
# The command processor, census and image pool (pm4, image_pool) run one side a process
# (--side), the same program twice, and the two outputs are compared case by case; the
# first case that differs is shown whole from both (pm4.cpp says what a case logs).
# --capture <dir> feeds a capture of the game's GPU input (MW2_CAPTURE_GPU) to pm4.
# The memory watch (watch, watch_stress, watch_livelock, watch_teardown) runs both sides in one process on
# real page protections, a memfd mapped as the two physical windows (watch.cpp says how).
# preload is tools/hook_diff.py on the two shader-preload hooks, whose cases are the
# synthetic shader objects (hook_diff.cpp's Tune).
# The recorder (recorder, recorder_inline, recorder_sleeps, recorder_bench) and the Rust
# Vk's trace lines (device_trace) run both sides in one process: the C++ recorder.cpp and
# trace.cpp as they are, the Vulkan both reach a stand-in (device_stubs.cpp); --cases is
# the script's ops, the trace's cases, or the benchmark's rounds (recorder.cpp says how).
# Their Rust is not wired into the game yet, so they need no file in RUST_OWNS.
#
#   usage: gpu_diff.py <build folder> --rig <name>[,<name>...] [--seeds N] [--cases N]
#                      [--src folder] [--bench] [--side cpp|rust] [--capture <dir>]
#
# The build folder must be a Rust-renderer build (cargo xtask build): its cfgs say
# which files Rust owns. --src takes the Rust from another folder, to try a deliberate
# mistake. --bench builds both halves at -O3 with no checks and runs the timings.
# Exits nonzero at the first difference.
import argparse, glob, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Each rig: the C++ files under runtime/ the Rust must own, the parts its program runs,
# and, for a program that compiles the originals into helpers of its own instead, those
# helpers and the C++ they need beside them.
TEXTURE = ["gpu/texture.cpp", "gpu/vulkan/texture_formats.cpp"]
PRESENTER = ["gpu/vulkan/presenter.cpp", "gpu/vulkan/backdrop.cpp", "gpu/vulkan/gpu_time.cpp", "gpu/vulkan/capture.cpp"]
THREADS = ["gpu/gpu.cpp", "gpu/arena.cpp", "gpu/d3d_hooks.cpp"]
PM4 = ["gpu/command_processor.cpp", "gpu/census.cpp", "image_move.cpp"]
WATCH = ["gpu/memory_watch.cpp"]
RIGS = {
    "tiling": (TEXTURE, "texture", ["tiling"]),
    "texture": (TEXTURE, "texture", ["texture"]),
    "formats": (TEXTURE, "texture", ["formats"]),
    "texture_bench": (TEXTURE, "texture", ["bench"]),
    "presenter": (PRESENTER, "presenter", ["letterbox", "swapchain", "follow", "display", "blank", "fade", "push",
                                           "shaders", "queue", "gpu_time", "abi"]),
    "arena_progress": (THREADS, "arena", ["arena"]),
    "interrupts": (THREADS, "interrupts", ["interrupts", "vblank", "worker"]),
    "pm4": (PM4, "pm4", ["pm4"]),
    "image_pool": (PM4, "pm4", ["image_pool"]),
    "watch": (WATCH, "watch", ["watch"]),
    "watch_stress": (WATCH, "watch", ["stress"]),
    "watch_livelock": (WATCH, "watch", ["livelock"]),
    "watch_teardown": (WATCH, "watch", ["teardown"]),
    # Dormant until S10: nothing for Rust to own yet.
    "recorder": ([], "recorder", ["recorder"]),
    "recorder_inline": ([], "recorder", ["inline"]),
    "recorder_sleeps": ([], "recorder", ["sleeps"]),
    "recorder_bench": ([], "recorder", ["bench"]),
    "device_trace": ([], "recorder", ["trace"]),
}
# What each program compiles: the originals as they are, then its helpers.
PROGRAMS = {
    "texture": (TEXTURE, ["texture_reach", "texture_forwarders", "rig_bridge"]),
    # The presenter's C++ is included whole by its helpers, its calls renamed to stand-ins
    # (presenter_rig.h); the backdrop's shaders need the C++ SPIR-V writer.
    "presenter": (["gpu/vulkan/spirv.cpp"], ["presenter_stubs", "presenter_cpp", "backdrop_cpp", "gpu_time_cpp",
                                             "presenter_forwarders", "rig_bridge"]),
    # arena.cpp as it is; gpu.cpp included whole by gpu_cpp.cpp, for its file statics.
    "arena": (["gpu/arena.cpp"], ["rig_bridge"]),
    "interrupts": ([], ["gpu_cpp", "rig_bridge"]),
    # The command processor, census, image pool and arena as they are; the guest's clock
    # in the command processor is the rig's (rig_timebase), and its mw2_* export renamed.
    "pm4": (["gpu/command_processor.cpp", "gpu/census.cpp", "image_move.cpp", "gpu/arena.cpp"], ["rig_bridge"]),
    # memory_watch.cpp included whole by watch.cpp, for its file statics.
    "watch": ([], ["rig_bridge"]),
    # recorder.cpp included whole by recorder.cpp, trace.cpp by trace_cpp.cpp, both for their
    # file statics; the Vulkan they reach is device_stubs.cpp's.
    "recorder": ([], ["device_stubs", "trace_cpp", "device_trace", "rig_bridge"]),
}
# What a program links with beyond the others: the watch rig wraps mprotect, to see and
# steer every protection change either side makes.
LINK_FLAGS = {"watch": ["-Wl,--wrap=mprotect"]}
# Flags of one original's own.
# Its sleeps between WAIT_REG_MEM polls, the rig's (pm4_sleep.h).
SOURCE_FLAGS = {"gpu/command_processor.cpp": ["-DPPC_QueryTimebase=rig_timebase", "-include",
                                              f"{ROOT}/tools/gpu_diff/pm4_sleep.h"]}
# Programs whose originals define mw2_* names the Rust defines too: renamed cpp_mw2_*.
RENAMED = {"pm4"}
# Programs run one side a process, the two outputs compared.
TWO_SIDED = {"pm4"}
# The switches each seed of a two-sided program runs under, both sides alike.
PM4_ENVIRONMENTS = [{}, {"MW2_TRACE_FRAME": "6", "MW2_TRACE_DRAWS": "60"},
                    {"MW2_WATCH": "A0380000:65536", "MW2_TRACE_SHADER_LOADS": "1"}, {"MW2_RENDER": "0"},
                    {"MW2_TRACE_DRAWS": "200", "MW2_TRACE_DRAWS_AFTER": "300"}, {"MW2_RENDER": "1", "MW2_WATCH": "A0390000"}]
# What a program's C++ needs beyond the others': gpu.cpp includes the title's table and
# the recompiled code's header. These programs compare the lines each side prints, so
# their C++ and their Rust log, into memory (rig_log.h); the others' do not.
TITLED = {"arena", "interrupts", "pm4", "watch"}
# Programs that compare printed lines but need no title.
LOGGED = {"recorder"}

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--rig", required=True)
    parser.add_argument("--seeds", type=int, default=8)
    parser.add_argument("--cases", type=int, default=20000)
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--bench", action="store_true")
    parser.add_argument("--work")
    parser.add_argument("--side", choices=["cpp", "rust"])
    parser.add_argument("--capture")
    arguments = parser.parse_args()

    build = os.path.abspath(arguments.build)
    kinds = open(f"{build}/renderer_cfgs.txt").read().split()
    if "rust" not in kinds:
        sys.exit(f"{build} is not a Rust-renderer build")
    owns = [k for k in kinds if k.startswith("mw2_owns_")]
    flavour = "bench" if arguments.bench else "check"
    work = arguments.work or f"{build}/gpu_diff/{flavour}"
    os.makedirs(work, exist_ok=True)
    # The build-script run cargo used for this build (cargo xtask writes out_dir.txt).
    if not os.path.exists(f"{build}/out_dir.txt"):
        sys.exit(f"{build} has no Cargo build: build it first")
    out_dir = open(f"{build}/out_dir.txt").read().strip()
    output = open(f"{os.path.dirname(out_dir)}/output").read().splitlines()
    cfgs = [line.split("=", 1)[1] for line in output if line.startswith("cargo:rustc-cfg=")]
    variables = dict(line.split("=", 2)[1:] for line in output if line.startswith("cargo:rustc-env="))
    variables.setdefault("MW2_RELEASE_TAG", "")

    level = "-O3" if arguments.bench else "-O1"
    defines = ["-DMW2_DIAGNOSTICS=1", "-DMW2_HAVE_VULKAN=1", "-DMW2_RUST=1", "-DMW2_USE_SDL=1"]
    # The SDL cargo built, whose headers the presenter's C++ includes.
    sdl = sorted(glob.glob(f"{build}/rust/release/build/sdl3-sys-*/out/include"), key=os.path.getmtime)
    includes = [f"-I{ROOT}/runtime"] + [f"-I{sdl[-1]}"] * bool(sdl)
    compiler = ["clang++-18", "-std=gnu++20", level, "-march=x86-64-v2", "-w", *defines, *includes]
    title_mp, tu0 = "mw2_title_mp" in cfgs, "mw2_version_tu0" in cfgs
    ppc = f"{ROOT}/ppc{'_mp' if title_mp else ''}{'_tu0' if tu0 else ''}"
    titled = ["-DMW2_TRACE_INDIRECT=1", *["-DMW2_TITLE_MP=1"] * title_mp,
              *["-DMW2_VERSION_TU0=1"] * tu0, f"-I{ppc}", f"-I{ROOT}/XenonRecomp-rs/thirdparty/simde"]

    # The Rust: src/runtime/ through a root of its own, as a static library.
    root = f"{work}/rust_root.rs"
    lines = ["#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs, improper_ctypes)]\n"]
    for module in ["env", "guest", "log", "title"]:
        lines.append(f'#[path = "{arguments.src}/{module}.rs"] mod {module};\n')
    lines.append(f'#[path = "{arguments.src}/runtime/mod.rs"] mod runtime;\n')
    open(root, "w").write("".join(lines))
    def rlib(name):
        return sorted(glob.glob(f"{build}/rust/release/deps/lib{name}-*.rlib"), key=os.path.getmtime)[-1]
    checks = ["-Copt-level=3"] if arguments.bench else ["-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on"]
    externs = ["--extern", f"ppc_rt={rlib('ppc_rt')}", "--extern", f"sdl3_sys={rlib('sdl3_sys')}"]
    if glob.glob(f"{build}/rust/release/deps/libash-*.rlib"):
        externs += ["--extern", f"ash={rlib('ash')}"]
    libraries = {}
    def library_for(logged):
        if logged not in libraries:
            library = f"{work}/libgpu{'_logged' * logged}.a"
            run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=gpu", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
                *checks, "--cfg", 'feature="diagnostics"', "--cfg", 'feature="rust-renderer"', "--cfg", "mw2_screens_rig",
                "--cfg", "mw2_gpu_rig", *(["--cfg", "mw2_no_logging"] * (not logged)), *sum((["--cfg", cfg] for cfg in cfgs), []),
                "--cap-lints=warn", *externs, "-L", f"dependency={build}/rust/release/deps", "-o", library, root,
                env={**os.environ, "OUT_DIR": out_dir, **variables})
            libraries[logged] = library
        return libraries[logged]

    for name in arguments.rig.split(","):
        if name == "preload":
            # The shader-preload hooks are hooks: hook_diff.py runs them, C++ against Rust,
            # on the synthetic shader objects of its cases (hook_diff.cpp, Tune).
            if "mw2_owns_shader_preload" not in owns:
                sys.exit(f"{build}: Rust does not own shader_preload.cpp (renderer_cfgs.txt)")
            result = subprocess.run([sys.executable, "-I", f"{ROOT}/tools/hook_diff.py", build, "--hook",
                                     "T_D3D_InitPixelShader,T_D3D_PatchVertexFetches", "--src", arguments.src,
                                     "--seeds", str(arguments.seeds), "--cases", str(arguments.cases),
                                     "--work", f"{work}/preload"], text=True, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT)
            print(result.stdout[-4000:], end="")
            if result.returncode != 0:
                sys.exit(f"preload: hook_diff exits {result.returncode}")
            continue
        owned, program_name, parts = RIGS[name]
        for source in owned:
            stem = os.path.basename(source)[:-4]
            if f"mw2_owns_{stem}" not in owns:
                sys.exit(f"{build}: Rust does not own {source} (renderer_cfgs.txt)")
        sources, helpers = PROGRAMS[program_name]
        flags = compiler + (titled if program_name in TITLED else [])
        objects = []
        for source in sources:
            target = f"{work}/{source.replace('/', '_')}.o"
            run(*flags, *SOURCE_FLAGS.get(source, []), "-c", f"{ROOT}/runtime/{source}", "-o", target)
            if program_name in RENAMED:
                defined = subprocess.run(["nm", "--defined-only", "--extern-only", target], check=True, text=True,
                                         capture_output=True).stdout.split()
                renames = sum((["--redefine-sym", f"{name}=cpp_{name}"] for name in defined if name.startswith("mw2_")), [])
                if renames:
                    run("objcopy", *renames, target)
            objects.append(target)
        # The texture rig measures how far the C++ reader reads (texture_reach.cpp) and
        # runs the build's C++ forwarders (texture_forwarders.cpp); the presenter rig's
        # helpers are the originals on stand-ins, and its forwarders.
        for helper in helpers:
            target = f"{work}/{helper}.o"
            run(*flags, "-c", f"{ROOT}/tools/gpu_diff/{helper}.cpp", "-o", target)
            objects.append(target)
        program = f"{work}/{program_name}"
        library = library_for(program_name in TITLED or program_name in LOGGED)
        run(*flags, f"{ROOT}/tools/gpu_diff/{program_name}.cpp", *objects, library, "-fuse-ld=lld",
            "-Wl,--gc-sections,--unresolved-symbols=ignore-all", *LINK_FLAGS.get(program_name, []), "-lvulkan",
            "-lpthread", "-ldl", "-lm", "-lgcc_s", "-o", program)
        if program_name in TWO_SIDED:
            two_sided(program, name, parts, arguments, work)
            continue
        seeds = 1 if arguments.bench else arguments.seeds
        for seed in range(1, seeds + 1):
            result = subprocess.run([program, str(seed), str(arguments.cases), *parts], text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=3600)
            if result.returncode != 0:
                print(result.stdout, end="")
                sys.exit(f"{name}: seed {seed}: exit {result.returncode}")
            if seed in (1, seeds):
                print(f"{name} seed {seed}: " + result.stdout.strip().replace("\n", f"\n{name} seed {seed}: "))
        print(f"{name}: all the same over {seeds} seeds of {arguments.cases} cases")

def capture_file(folder, path):
    """A capture (MW2_CAPTURE_GPU, read as src/runtime/gpu_replay.rs reads it) as pm4.cpp's
    Capture takes it: little-endian words, the pages' bytes inline."""
    import mmap, struct
    lines = open(f"{folder}/capture.idx").read().splitlines()
    if lines[0] != "mw2 gpu capture 2":
        sys.exit(f"{folder}: {lines[0]!r}, not capture format 2")
    blobs_file = open(f"{folder}/pages.blob", "rb")
    blobs = mmap.mmap(blobs_file.fileno(), 0, access=mmap.ACCESS_READ)
    ring = open(f"{folder}/ring.bin", "rb").read()
    state = open(f"{folder}/state.bin", "rb").read()
    blob = lambda i: blobs[i * 4096:(i + 1) * 4096]
    top = ring_base = ring_dwords = write_back = 0
    snapshot, batches = [], []
    at = 1
    while at < len(lines):
        words = lines[at].split()
        at += 1
        if words[0] == "ring":
            ring_base, ring_dwords = int(words[1], 16), int(words[2])
        elif words[0] == "writeback":
            write_back = int(words[1], 16)
        elif words[0] == "top":
            top = int(words[1], 16)
        elif words[0] == "snapshot":
            for _ in range(int(words[1])):
                page, b = lines[at].split()
                at += 1
                snapshot.append((int(page, 16), int(b)))
        elif words[0] == "batch":
            dwords, npages, nevents = int(words[3]), int(words[5]), int(words[7])
            pages, events = [], []
            for _ in range(npages + nevents):
                w = lines[at].split()
                at += 1
                kinds = {"pool": 1, "wait": 2, "aperture": 3, "arena": 4}
                if w[0] not in kinds:
                    pages.append((int(w[0]), int(w[1], 16), int(w[2]), len(w) > 3 and w[3] == "own"))
                    continue
                kind, packet = kinds[w[0]], int(w[1])
                if kind == 1:
                    a, b, c, d, e = int(w[2], 16), int(w[3], 16), int(w[4], 16), int(w[5], 16), int(w[6])
                elif kind == 2:
                    a, b, c, d, e = int(w[2]), int(w[3], 16), int(w[4], 16), int(w[5]), int(w[6])
                elif kind == 3:
                    a, b, c, d, e = int(w[2], 16), int(w[3], 16), 0, 0, 0
                else:
                    a, b, c, d, e = int(w[2], 16), 0, 0, 0, 0
                events.append((kind, packet, a, b, c, d, e))
            batches.append((dwords, pages, events))
    with open(path, "wb") as out:
        out.write(struct.pack("<8I", 0x5232574D, top, ring_base, ring_dwords, write_back, len(state), len(snapshot),
                              len(batches)))
        out.write(state)
        for page, b in snapshot:
            out.write(struct.pack("<I", page))
            out.write(blob(b))
        ring_at = 0
        for dwords, pages, events in batches:
            out.write(struct.pack("<3I", dwords, len(pages), len(events)))
            out.write(ring[ring_at * 4:(ring_at + dwords) * 4])
            ring_at += dwords
            for packet, page, b, own in pages:
                out.write(struct.pack("<3I", packet, page, int(own)))
                out.write(blob(b))
            for kind, packet, a, b, c, d, e in events:
                out.write(struct.pack("<6IQ", kind, packet, a, b, c, d, e))
    return len(batches)

def two_sided(program, name, parts, arguments, work):
    """Each side in a process of its own; their outputs, a line a case, compared. The
    first case that differs is run again on both, shown whole."""
    def side(which, seed, cases, extra, environment):
        return subprocess.run([program, str(seed), str(cases), which, *extra], text=True, errors="replace",
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=7200,
                              env={**os.environ, **environment})
    runs = []
    if arguments.capture:
        capture = f"{work}/capture.bin"
        batches = capture_file(arguments.capture, capture)
        runs.append((1, 0, [f"capture:{capture}"], {}, f"capture {os.path.basename(arguments.capture.rstrip('/'))} ({batches} batches)"))
    else:
        for seed in range(1, arguments.seeds + 1):
            runs.append((seed, arguments.cases, parts, PM4_ENVIRONMENTS[seed % len(PM4_ENVIRONMENTS)], f"seed {seed}"))
    for seed, cases, extra, environment, label in runs:
        if arguments.side:
            result = side(arguments.side, seed, cases, extra, environment)
            print(result.stdout[-2000:], end="")
            continue
        out = {}
        for which in ("cpp", "rust"):
            result = side(which, seed, cases, extra, environment)
            if result.returncode != 0:
                print(result.stdout[-3000:], result.stderr[-3000:])
                sys.exit(f"{name}: {label}: the {which} side exits {result.returncode}")
            out[which] = result.stdout.splitlines()
        if out["cpp"] != out["rust"]:
            first = next((i for i, (a, b) in enumerate(zip(out["cpp"], out["rust"])) if a != b),
                         min(len(out["cpp"]), len(out["rust"])))
            line = (out["cpp"] + [""])[first]
            print(f"DIFFERENCE: {name}: {label}: line {first + 1}: C++ {line!r}, Rust {(out['rust'] + [''])[first]!r}",
                  flush=True)
            words = line.split()
            if len(words) >= 2 and words[0] in ("case", "pool", "batch") and words[1].isdigit():
                shown = {which: side(which, seed, cases, [*extra, "--show", words[1]], environment).stdout.splitlines()
                         for which in ("cpp", "rust")}
                # The case's first differing line, with what came before it: a case can log
                # millions of lines, too many for a diff.
                a, b = shown["cpp"], shown["rust"]
                at = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
                print(f"case {words[1]}: {len(a)} lines from C++, {len(b)} from Rust; the first difference at line {at + 1}:")
                for i in range(max(0, at - 12), at):
                    print(f"   {a[i]}")
                for i in range(at, min(at + 8, max(len(a), len(b)))):
                    print(f"  C++:  {a[i] if i < len(a) else '(nothing)'}")
                    print(f"  Rust: {b[i] if i < len(b) else '(nothing)'}")
            sys.exit(1)
        tail = [l for l in out["cpp"] if l.endswith("fields compared")]
        print(f"{name} {label}: {len(out['cpp'])} lines the same; {tail[-1] if tail else ''}")
    print(f"{name}: all the same over {len(runs)} {'captures' if arguments.capture else 'seeds'}"
          f"{'' if arguments.capture else f' of {arguments.cases} cases'}")

main()
