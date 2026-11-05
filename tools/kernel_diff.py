#!/usr/bin/env python3
# Compares the kernel in src/runtime/kernel/ with its C++ in runtime/kernel/ (see
# kernel_diff.cpp): both in one program, on the build's title, each on a guest space and
# a folder of its own. src/ is built with overflow checks and debug_assert! on.
#
#   usage: kernel_diff.py <build folder> [--seeds N] [--cases N] [--src folder]
#                         [--stress N] [--time N] [--coverage]
#
# A seed runs a script on both sides and compares everything; --stress runs many
# threads on each side and checks what must hold; --time says what a call and a wake
# cost on each. --src takes the Rust from another folder, to try a deliberate mistake.
# The C++ keeps its __imp__, mw2_kernel_, hook and timebase names with cpp_ in front.
# Exits nonzero at the first difference.
import argparse, glob, os, re, resource, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCES = ["kernel/objects", "kernel/sync_objects", "kernel/sync", "kernel/threads", "kernel/memory", "kernel/kernel",
           "kernel/files", "kernel/content", "kernel/profile", "kernel/misc", "kernel/video", "kernel/input", "kernel/net",
           "kernel/xam"]
# Two guest spaces of 4 GB each are reserved, not resident.
ADDRESS_SPACE = 24 << 30

def limited():
    resource.setrlimit(resource.RLIMIT_AS, (ADDRESS_SPACE, ADDRESS_SPACE))

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--seeds", type=int, default=10)
    parser.add_argument("--cases", type=int, default=400)
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--stress", type=int)
    parser.add_argument("--time", type=int)
    parser.add_argument("--coverage", action="store_true", help="say which lines of the C++ no case reached")
    parser.add_argument("--optimised", action="store_true", help="both sides as a release build has them, for --time and --stress")
    parser.add_argument("--work")
    arguments = parser.parse_args()

    build = os.path.abspath(arguments.build)
    work = arguments.work or f"{build}/kernel_diff"
    os.makedirs(work, exist_ok=True)
    outputs = sorted(glob.glob(f"{build}/rust/release/build/mw2-*/output"), key=os.path.getmtime)
    if not outputs:
        sys.exit(f"{build} has no Cargo build: build it first")
    output = open(outputs[-1]).read().splitlines()
    out_dir = f"{os.path.dirname(outputs[-1])}/out"
    cfgs = [line.split("=", 1)[1] for line in output if line.startswith("cargo:rustc-cfg=")]
    variables = dict(line.split("=", 2)[1:] for line in output if line.startswith("cargo:rustc-env="))
    variables.setdefault("MW2_RELEASE_TAG", "")
    title_mp = "mw2_title_mp" in cfgs
    tu0 = "mw2_version_tu0" in cfgs
    ppc = f"{ROOT}/ppc{'_mp' if title_mp else ''}{'_tu0' if tu0 else ''}"

    defines = ["-DMW2_DIAGNOSTICS=1", "-DMW2_USE_SDL=1"] + ["-DMW2_TITLE_MP=1"] * title_mp + ["-DMW2_VERSION_TU0=1"] * tu0
    # The SDL sdl3-sys built for the game.
    sdl = sorted(glob.glob(f"{build}/rust/release/build/sdl3-sys-*/out/include"), key=os.path.getmtime)
    if not sdl:
        sys.exit(f"{build} has no SDL that sdl3-sys built: build it again")
    sdl = sdl[-1]
    includes = [f"-I{ROOT}/runtime", f"-I{ppc}", f"-I{ROOT}/XenonRecomp-rs/thirdparty/simde", f"-I{sdl}"]
    level = "-O2" if arguments.optimised else "-O1"
    compiler = ["clang++-18", "-std=gnu++20", level, "-march=x86-64-v2", "-w", *defines, *includes]
    coverage = ["-fprofile-instr-generate", "-fcoverage-mapping"] if arguments.coverage else []
    tag = "cov" if arguments.coverage else "fast" if arguments.optimised else "plain"

    objects = []
    for source in SOURCES:
        path, target = f"{ROOT}/runtime/{source}.cpp", f"{work}/{source.replace('/', '_')}.{tag}.o"
        # A header the C++ includes changes it as much as its own file does.
        headers = glob.glob(f"{ROOT}/runtime/**/*.h", recursive=True)
        newest = max(os.path.getmtime(one) for one in [path, *headers])
        if not os.path.exists(target) or os.path.getmtime(target) < newest:
            run(*compiler, *(coverage if source.startswith("kernel/") else []), "-c", path, "-o", target)
        objects.append(target)
    renames, mangled = [], []
    for path in objects:
        names = run("nm", "--defined-only", "--extern-only", path, capture_output=True).stdout.split()
        renames += [name for name in names if re.match(r"^(__imp__\w+|mw2_kernel_\w+|mw2_engine_\w+|sub_[0-9A-F]{8}|PPC_QueryTimebase)$", name)]
        # An import the title does not make has no C declaration, and a C++ name.
        mangled += [(name, f"cpp___imp__{match.group(1)}") for name in names
                    if (match := re.match(r"^_Z\d+__imp__(\w+)R10PPCContextPh$", name))]
    renamed = []
    for path in objects:
        target = path.replace(".o", ".renamed.o")
        # The rig's fixed time stands in for the C++'s clock, and its bytes for its random ones.
        clock = ["--weaken-symbol=_ZN6kernel15SystemTime100nsEv"] if "kernel_misc" in path else []
        clock += ["--weaken-symbol=_ZN6kernel11RandomBytesEPvm"] if "kernel_kernel" in path else []
        names = [f"{name}=cpp_{name}" for name in sorted(set(renames))] + [f"{name}={to}" for name, to in mangled]
        run("objcopy", *sum((["--redefine-sym", one] for one in names), []), *clock, path, target)
        renamed.append(target)

    root = f"{work}/rust_root.rs"
    source = arguments.src
    lines = ["#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs, improper_ctypes)]\n"]
    for module in ["env", "guest", "log", "title"]:
        lines.append(f'#[path = "{source}/{module}.rs"] mod {module};\n')
    lines.append(f'#[path = "{source}/runtime/mod.rs"] mod runtime;\n')
    lines.append(f'#[path = "{ROOT}/tools/kernel_diff.rs"] mod rig;\n')
    open(root, "w").write("".join(lines))
    def rlib(name):
        return sorted(glob.glob(f"{build}/rust/release/deps/lib{name}-*.rlib"), key=os.path.getmtime)[-1]
    library = f"{work}/libruntime.{tag}.a"
    checks = ["-Copt-level=3", "-Cdebug-assertions=off", "-Coverflow-checks=off"] if arguments.optimised else \
             ["-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on"]
    run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=runtime", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
        *checks, "--cfg", 'feature="diagnostics"', "--cfg", "mw2_rig", *sum((["--cfg", cfg] for cfg in cfgs), []),
        "--cap-lints=warn", "--extern", f"ppc_rt={rlib('ppc_rt')}", "--extern", f"sdl3_sys={rlib('sdl3_sys')}",
        "-L", f"dependency={build}/rust/release/deps", "-o", library, root,
        env={**os.environ, "OUT_DIR": out_dir, **variables})

    # The guest functions the rest of the runtime calls, which no case here reaches.
    undefined = run("nm", "--undefined-only", library, capture_output=True).stdout.split()
    guest = sorted({name for name in undefined if re.match(r"^(__imp__)?sub_[0-9A-F]{8}$", name)})
    stubs = f"{work}/guest_stubs.cpp"
    open(stubs, "w").write("".join(f'extern "C" void {name}(void*, void*) {{}}\n' for name in guest))

    program = f"{work}/kernel_diff.{tag}"
    # SDL comes with sdl3-sys, in the library rustc made.
    libraries = glob.glob(f"{build}/_deps/zlib-build/libz*.a")
    run(*compiler, "-Wno-everything", *coverage, f"{ROOT}/tools/kernel_diff.cpp", stubs, *renamed, library, *libraries,
        "-fuse-ld=lld", "-lpthread", "-ldl", "-lm", "-lgcc_s", "-o", program)

    def go(*command, timeout, environment=None):
        result = subprocess.run([program, *command], text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=timeout, preexec_fn=limited, env=environment)
        print(result.stdout, end="")
        if result.returncode != 0:
            sys.exit(f"{' '.join(command)}: exit {result.returncode}")

    if arguments.stress:
        go("stress", str(arguments.stress), timeout=900)
    if arguments.time:
        go("time", str(arguments.time), timeout=900)
    if arguments.stress or arguments.time:
        return

    profiles = []
    for seed in range(1, arguments.seeds + 1):
        environment = dict(os.environ)
        if arguments.coverage:
            profiles.append(f"{work}/{seed}.profraw")
            environment["LLVM_PROFILE_FILE"] = profiles[-1]
        go("script", str(seed), str(arguments.cases), timeout=900, environment=environment)
    print(f"all the same over {arguments.seeds} seeds of {arguments.cases} cases")
    if arguments.coverage:
        # What only many threads reach: a critical section held by another.
        profiles.append(f"{work}/stress.profraw")
        go("stress", "2000", timeout=900, environment={**os.environ, "LLVM_PROFILE_FILE": profiles[-1]})

    if arguments.coverage:
        merged = f"{work}/merged.profdata"
        run("llvm-profdata-18", "merge", "-o", merged, *profiles)
        for source in SOURCES[:-1]:
            shown = subprocess.run(["llvm-cov-18", "show", program, f"-instr-profile={merged}", f"{ROOT}/runtime/{source}.cpp"],
                                   check=True, text=True, capture_output=True).stdout
            missed = [line for line in shown.splitlines() if re.match(r"\s*\d+\|\s*0\|", line)]
            print(f"--- {source}: {len(missed)} lines never run")
            for line in missed:
                print(line)

main()
