#!/usr/bin/env python3
# Compares what moved to src/runtime/ with its C++, where a module's answers depend on
# its inputs alone (see runtime_diff.cpp): both in one program, on the build's title,
# on the real executables and XLAST and on random cases. What depends on threads,
# files and the system is what real runs are for. src/ is built with overflow checks
# and debug_assert! on.
#
#   usage: runtime_diff.py <build folder> [--seeds N] [--cases N] [--src folder] [--coverage]
#
# --src takes the Rust from another folder, to try a deliberate mistake. The C++ of a
# module keeps its mw2_ names as cpp_mw2_. Exits nonzero at the first difference.
import argparse, glob, os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCES = ["settings", "presence", "shadow_resolution", "console", "version", "install/crypto", "install/xex",
           "pacing_trace", "report", "stutters", "player", "screen", "signin", "friends", "guide"]
# The online service's pure parts, reached through tools/runtime_diff_online.cpp.
ONLINE = {"online_service": [], "online_steam": ["-DRIG_STEAM=1"]}
# The guide reads the rig's clock.
TIMED = {"guide"}
# These run on the rig's clock (runtime_diff_clock.h), and read the rig's machine.
CLOCKED = {"pacing_trace", "report", "stutters", "player"}
# What --coverage reports on: the C++ with a clock, and the screens.
COVERED = CLOCKED | {"screen", "signin", "friends", "guide"}
MACHINE = ["mw2_platform_processor_load=rig_load", "mw2_platform_memory_state=rig_memory",
           "mw2_platform_thread_clock=rig_thread_clock", "mw2_platform_thread_time=rig_thread_time", "usleep=rig_forever"]

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--seeds", type=int, default=8)
    parser.add_argument("--cases", type=int, default=2000)
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--work")
    parser.add_argument("--coverage", action="store_true", help="say which lines of the C++ with a clock, and of the screens, never ran")
    arguments = parser.parse_args()

    build = os.path.abspath(arguments.build)
    work = arguments.work or f"{build}/runtime_diff"
    os.makedirs(work, exist_ok=True)
    outputs = sorted(glob.glob(f"{build}/rust/release/build/mw2-*/output"), key=os.path.getmtime)
    if not outputs:
        sys.exit(f"{build} has no Cargo build: build it first")
    output = open(outputs[-1]).read().splitlines()
    out_dir = f"{os.path.dirname(outputs[-1])}/out"
    cfgs = [line.split("=", 1)[1] for line in output if line.startswith("cargo:rustc-cfg=")]
    variables = dict(line.split("=", 2)[1:] for line in output if line.startswith("cargo:rustc-env="))
    title_mp = "mw2_title_mp" in cfgs
    tu0 = "mw2_version_tu0" in cfgs
    ppc = f"{ROOT}/ppc{'_mp' if title_mp else ''}{'_tu0' if tu0 else ''}"

    tag = os.environ.get("MW2_RELEASE_TAG", "")
    variables["MW2_RELEASE_TAG"] = tag
    defines = ["-DMW2_DIAGNOSTICS=1", "-DMW2_SCREENS=1", f'-DMW2_RELEASE_TAG="{tag}"'] + ["-DMW2_TITLE_MP=1"] * title_mp + ["-DMW2_VERSION_TU0=1"] * tu0
    zlib = [f"-I{build}/_deps/zlib-src", f"-I{build}/_deps/zlib-build"]
    if not os.path.isdir(zlib[0][2:]):
        zlib = [f"-I{ROOT}/build/_deps/zlib-src", f"-I{ROOT}/build/_deps/zlib-build"]
    imgui = f"{build}/_deps/imgui-src" if os.path.isdir(f"{build}/_deps/imgui-src") else f"{ROOT}/build/_deps/imgui-src"
    includes = [f"-I{ROOT}/runtime", f"-I{ROOT}/runtime/install", f"-I{ppc}", f"-I{ROOT}/XenonRecomp-rs/thirdparty/simde", *zlib,
                f"-I{imgui}", f"-I{build}/generated"]
    compiler = ["clang++-18", "-std=gnu++20", "-O1", "-march=x86-64-v2", "-w", *defines, *includes]

    coverage = ["-fprofile-instr-generate", "-fcoverage-mapping"] if arguments.coverage else []
    objects = []
    # A header the C++ includes changes it as much as its own file does.
    headers = max(os.path.getmtime(path) for path in glob.glob(f"{ROOT}/runtime/**/*.h", recursive=True))
    for source in SOURCES:
        path, target = f"{ROOT}/runtime/{source}.cpp", f"{work}/{source.replace('/', '_')}.o"
        if not os.path.exists(target) or os.path.getmtime(target) < max(os.path.getmtime(path), headers):
            clocked = ["-include", f"{ROOT}/tools/runtime_diff_clock.h"] if source in CLOCKED else []
            run(*compiler, *clocked, *(coverage if source in COVERED else []), "-c", path, "-o", target)
        objects.append(target)
    for name, flags in ONLINE.items():
        target = f"{work}/{name}.o"
        source = f"{ROOT}/tools/runtime_diff_online.cpp"
        if not os.path.exists(target) or os.path.getmtime(target) < max(os.path.getmtime(source), headers,
                                                                         *(os.path.getmtime(p) for p in glob.glob(f"{ROOT}/runtime/online/*.cpp"))):
            run(*compiler, *flags, "-c", source, "-o", target)
        objects.append(target)
    # The fonts the C++ draws with; the Rust has its own copy of the files.
    fonts = []
    for source in [f"{build}/launcher_fonts.cpp"]:
        target = f"{work}/{os.path.basename(source)[:-4]}.o"
        if not os.path.exists(target) or os.path.getmtime(target) < os.path.getmtime(source):
            run(*compiler, "-c", source, "-o", target)
        fonts.append(target)
    exported = set()
    for path in objects:
        names = run("nm", "--defined-only", "--extern-only", path, capture_output=True).stdout.split()
        exported |= {name for name in names if name.startswith("mw2_") and name not in ("mw2_log",)}
    renames = sum((["--redefine-sym", f"{name}=cpp_{name}"] for name in sorted(exported)), [])
    renamed = []
    for path in objects:
        target = path.replace(".o", ".renamed.o")
        machine = sum((["--redefine-sym", rename] for rename in MACHINE), []) if os.path.basename(path)[:-2] in CLOCKED else []
        machine += ["--redefine-sym", "time=rig_time"] if os.path.basename(path)[:-2] in TIMED else []
        run("objcopy", *renames, *machine, path, target)
        renamed.append(target)

    source = arguments.src
    root = f"{work}/rust_root.rs"
    lines = ["#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs, improper_ctypes)]\n"]
    for module in ["env", "guest", "log", "title"]:
        lines.append(f'#[path = "{source}/{module}.rs"] mod {module};\n')
    lines.append(f'#[path = "{source}/runtime/mod.rs"] mod runtime;\n')
    lines.append(f'#[path = "{ROOT}/tools/runtime_diff.rs"] mod rig;\n')
    open(root, "w").write("".join(lines))
    def rlib(name):
        return sorted(glob.glob(f"{build}/rust/release/deps/lib{name}-*.rlib"), key=os.path.getmtime)[-1]
    library = f"{work}/libruntime.a"
    run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=runtime", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
        "-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on", "--cfg", 'feature="diagnostics"', "--cfg", "mw2_screens_rig", *sum((["--cfg", cfg] for cfg in cfgs), []),
        "--cap-lints=warn", "--extern", f"ppc_rt={rlib('ppc_rt')}", "--extern", f"sdl3_sys={rlib('sdl3_sys')}",
        "-L", f"dependency={build}/rust/release/deps", "-o", library, root,
        env={**os.environ, "OUT_DIR": out_dir, **variables})
    run("objcopy", "--redefine-sym", "time=rig_time", library)

    undefined = run("nm", "--undefined-only", library, capture_output=True).stdout.split()
    # The originals of hooks: the guest functions themselves are runtime_diff.cpp's.
    guest = sorted({name for name in undefined if re.match(r"^__imp__sub_[0-9A-F]{8}$", name)})
    stubs = f"{work}/guest_stubs.cpp"
    open(stubs, "w").write("".join(f'extern "C" void {name}(void*, void*) {{}}\n' for name in guest))

    program = f"{work}/runtime_diff"
    # SDL comes with sdl3-sys, in the library rustc made.
    libraries = glob.glob(f"{build}/_deps/zlib-build/libz*.a")
    run(*compiler, "-Wno-everything", *coverage, f"{ROOT}/tools/runtime_diff.cpp", stubs, *renamed, *fonts, library, *libraries,
        "-fuse-ld=lld", "-lpthread", "-ldl", "-lm", "-lgcc_s", "-o", program)

    profiles = []
    for seed in range(1, arguments.seeds + 1):
        environment = dict(os.environ)
        if arguments.coverage:
            profiles.append(f"{work}/{seed}.profraw")
            environment["LLVM_PROFILE_FILE"] = profiles[-1]
        result = subprocess.run([program, str(seed), str(arguments.cases), ROOT], text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, timeout=900, env=environment)
        if result.returncode != 0:
            print(result.stdout, end="")
            sys.exit(f"seed {seed}: exit {result.returncode}")
        if seed in (1, arguments.seeds):
            print(result.stdout, end="")
    print(f"all the same over {arguments.seeds} seeds of {arguments.cases} cases")

    if arguments.coverage:
        merged = f"{work}/merged.profdata"
        run("llvm-profdata-18", "merge", "-o", merged, *profiles)
        for name in sorted(COVERED):
            shown = subprocess.run(["llvm-cov-18", "show", program, f"-instr-profile={merged}", f"{ROOT}/runtime/{name}.cpp"],
                                   check=True, text=True, capture_output=True).stdout
            missed = [line for line in shown.splitlines() if re.match(r"\s*\d+\|\s*0\|", line)]
            print(f"--- {name}: {len(missed)} lines never run")
            for line in missed:
                print(line)

main()
