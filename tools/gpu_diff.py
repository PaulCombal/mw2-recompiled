#!/usr/bin/env python3
# The renderer's moved parts against their C++ (docs/building.md, "The renderer moves
# to Rust"): the C++ files as they are and the Rust of src/runtime/ in one program,
# on generated cases, compared byte for byte. Each rig is tools/gpu_diff/<rig>.cpp,
# which calls the Rust through the C functions the C++ renderer itself calls, or, for
# the presenter's, backdrop's and gpu_time's decisions, through the rig's own
# (exports.rs, mod rig) while the C++ runs on stand-in Vulkan and SDL calls
# (presenter_rig.h): neither side makes a device or a window.
#
#   usage: gpu_diff.py <build folder> --rig <name>[,<name>...] [--seeds N] [--cases N]
#                      [--src folder] [--bench]
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
RIGS = {
    "tiling": (TEXTURE, "texture", ["tiling"]),
    "texture": (TEXTURE, "texture", ["texture"]),
    "formats": (TEXTURE, "texture", ["formats"]),
    "texture_bench": (TEXTURE, "texture", ["bench"]),
    "presenter": (PRESENTER, "presenter", ["letterbox", "swapchain", "follow", "display", "blank", "fade", "push",
                                           "shaders", "queue", "gpu_time", "abi"]),
}
# What each program compiles: the originals as they are, then its helpers.
PROGRAMS = {
    "texture": (TEXTURE, ["texture_reach", "texture_forwarders", "rig_bridge"]),
    # The presenter's C++ is included whole by its helpers, its calls renamed to stand-ins
    # (presenter_rig.h); the backdrop's shaders need the C++ SPIR-V writer.
    "presenter": (["gpu/vulkan/spirv.cpp"], ["presenter_stubs", "presenter_cpp", "backdrop_cpp", "gpu_time_cpp",
                                             "presenter_forwarders", "rig_bridge"]),
}

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
    library = f"{work}/libgpu.a"
    externs = ["--extern", f"ppc_rt={rlib('ppc_rt')}", "--extern", f"sdl3_sys={rlib('sdl3_sys')}"]
    if glob.glob(f"{build}/rust/release/deps/libash-*.rlib"):
        externs += ["--extern", f"ash={rlib('ash')}"]
    run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=gpu", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
        *checks, "--cfg", 'feature="diagnostics"', "--cfg", 'feature="rust-renderer"', "--cfg", "mw2_screens_rig",
        "--cfg", "mw2_gpu_rig", "--cfg", "mw2_no_logging", *sum((["--cfg", cfg] for cfg in cfgs), []), "--cap-lints=warn", *externs,
        "-L", f"dependency={build}/rust/release/deps", "-o", library, root,
        env={**os.environ, "OUT_DIR": out_dir, **variables})

    for name in arguments.rig.split(","):
        owned, program_name, parts = RIGS[name]
        for source in owned:
            stem = os.path.basename(source)[:-4]
            if f"mw2_owns_{stem}" not in owns:
                sys.exit(f"{build}: Rust does not own {source} (renderer_cfgs.txt)")
        sources, helpers = PROGRAMS[program_name]
        objects = []
        for source in sources:
            target = f"{work}/{source.replace('/', '_')}.o"
            run(*compiler, "-c", f"{ROOT}/runtime/{source}", "-o", target)
            objects.append(target)
        # The texture rig measures how far the C++ reader reads (texture_reach.cpp) and
        # runs the build's C++ forwarders (texture_forwarders.cpp); the presenter rig's
        # helpers are the originals on stand-ins, and its forwarders.
        for helper in helpers:
            target = f"{work}/{helper}.o"
            run(*compiler, "-c", f"{ROOT}/tools/gpu_diff/{helper}.cpp", "-o", target)
            objects.append(target)
        program = f"{work}/{program_name}"
        run(*compiler, f"{ROOT}/tools/gpu_diff/{program_name}.cpp", *objects, library, "-fuse-ld=lld",
            "-Wl,--gc-sections,--unresolved-symbols=ignore-all", "-lvulkan", "-lpthread", "-ldl", "-lm", "-lgcc_s",
            "-o", program)
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

main()
