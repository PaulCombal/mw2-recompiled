#!/usr/bin/env python3
# Compares the XMA decoder's contexts in src/runtime/kernel/xma.rs with their C++ in
# runtime/apu/xma.cpp (see xma_diff.cpp): both in one program, replaying recordings of
# real runs (MW2_XMA_RECORD=<file> on a diagnostics build), each on a guest space of its
# own, with FFmpeg's decoder under both. src/ is built with overflow checks and
# debug_assert! on.
#
#   usage: xma_diff.py <build folder> <recording>... [--fuzz N] [--src folder] [--coverage]
#
# Each recording is replayed as it is, then with N fuzz seeds that change some kicks the
# same way on both sides. --src takes the Rust from another folder, to try a deliberate
# mistake. --coverage names the lines of xma.cpp nothing reached. Exits nonzero at the
# first difference.
import argparse, glob, os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("recordings", nargs="+")
    parser.add_argument("--fuzz", type=int, default=0)
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--coverage", action="store_true")
    parser.add_argument("--work")
    arguments = parser.parse_args()

    build = os.path.abspath(arguments.build)
    work = arguments.work or f"{build}/xma_diff"
    os.makedirs(work, exist_ok=True)
    outputs = sorted(glob.glob(f"{build}/rust/release/build/mw2-*/output"), key=os.path.getmtime)
    if not outputs:
        sys.exit(f"{build} has no Cargo build: build it first")
    output = open(outputs[-1]).read().splitlines()
    cfgs = [line.split("=", 1)[1] for line in output if line.startswith("cargo:rustc-cfg=")]
    title_mp = "mw2_title_mp" in cfgs
    tu0 = "mw2_version_tu0" in cfgs
    ppc = f"{ROOT}/ppc{'_mp' if title_mp else ''}{'_tu0' if tu0 else ''}"
    cache = open(f"{build}/CMakeCache.txt").read()
    ffmpeg = re.search(r"^FETCHCONTENT_SOURCE_DIR_FFMPEG_XMA:PATH=(.*)$", cache, re.M)
    ffmpeg = ffmpeg.group(1) if ffmpeg else f"{build}/_deps/ffmpeg_xma-src"
    library_ffmpeg = f"{build}/third_party/ffmpeg-xenia/libffmpeg_xma.a"

    defines = ["-DMW2_DIAGNOSTICS=1"] + ["-DMW2_TITLE_MP=1"] * title_mp + ["-DMW2_VERSION_TU0=1"] * tu0
    includes = [f"-I{ROOT}/runtime", f"-I{ppc}", f"-I{ROOT}/XenonRecomp-rs/thirdparty/simde",
                f"-I{ROOT}/third_party/ffmpeg-xenia", f"-I{ffmpeg}"]
    compiler = ["clang++-18", "-std=gnu++20", "-O1", "-march=x86-64-v2", "-w", *defines, *includes]
    coverage = ["-fprofile-instr-generate", "-fcoverage-mapping"] if arguments.coverage else []
    tag = "cov" if arguments.coverage else "plain"

    # The C++ decoder with its names given cpp_ in front and its error count made visible,
    # and the shim the Rust calls FFmpeg through; both log through the rig.
    objects = []
    for source, extra in (("apu/xma", coverage), ("apu/xma_codec", [])):
        path = f"{ROOT}/runtime/{source}.cpp"
        plain, target = f"{work}/{source.replace('/', '_')}.{tag}.o", f"{work}/{source.replace('/', '_')}.{tag}.renamed.o"
        run(*compiler, *extra, "-c", path, "-o", plain)
        names = run("nm", "--defined-only", plain, capture_output=True).stdout.split()
        renames = [f"{name}=cpp_{name}" for name in names if re.match(r"^mw2_xma_(initialise|allocate_context|release_context|register_store|report)$", name)]
        changes = ["--redefine-sym", "fprintf=rig_fprintf"]
        if source == "apu/xma":
            errors = "_ZN12_GLOBAL__N_18g_errorsE"
            # objcopy renames before it globalizes.
            changes += ["--redefine-sym", f"{errors}=cpp_xma_errors", "--globalize-symbol", "cpp_xma_errors"]
        run("objcopy", *sum((["--redefine-sym", one] for one in renames), []), *changes, plain, target)
        objects.append(target)

    source = arguments.src
    root = f"{work}/rust_root.rs"
    open(root, "w").write(
        "#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs, improper_ctypes)]\n"
        + "".join(f'#[path = "{source}/{module}.rs"] mod {module};\n' for module in ["env", "guest", "log"])
        + f'#[path = "{ROOT}/tools/xma_diff.rs"] mod rig;\n'
        + "mod runtime {\n"
        + "    pub use crate::rig::{guest_memory, platform};\n"
        + f'    #[path = "{source}/runtime/wmapro/mod.rs"] pub mod wmapro;\n'
        + "    pub mod kernel {\n"
        + "        pub use crate::rig::kernel::*;\n"
        + f'        #[path = "{source}/runtime/kernel/xma.rs"] pub mod xma;\n'
        + "    }\n"
        + "}\n")
    def rlib(name):
        return sorted(glob.glob(f"{build}/rust/release/deps/lib{name}-*.rlib"), key=os.path.getmtime)[-1]
    library = f"{work}/libxma.{tag}.a"
    run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=xma", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
        "-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on", "--cfg", 'feature="diagnostics"', "--cfg", "mw2_xma_rig",
        *sum((["--cfg", cfg] for cfg in cfgs), []), "--cap-lints=warn", "--extern", f"ppc_rt={rlib('ppc_rt')}",
        "-L", f"dependency={build}/rust/release/deps", "-o", library, root)
    # src/log.rs writes its lines with fprintf, as the C++ does: both to the rig's log.
    run("objcopy", "--redefine-sym", "fprintf=rig_fprintf", library)

    program = f"{work}/xma_diff.{tag}"
    run(*compiler, "-Wno-everything", *coverage, f"{ROOT}/tools/xma_diff.cpp", *objects, library, library_ffmpeg,
        "-fuse-ld=lld", "-lpthread", "-ldl", "-lm", "-lgcc_s", "-o", program)

    profiles = []
    def go(*command):
        environment = dict(os.environ, MW2_TRACE_XMA="1")
        if arguments.coverage:
            profiles.append(f"{work}/{len(profiles)}.profraw")
            environment["LLVM_PROFILE_FILE"] = profiles[-1]
        try:
            # A replay takes seconds: one that does not end has a side that never returns.
            result = subprocess.run([program, *command], text=True, stdout=subprocess.PIPE, timeout=300, env=environment)
        except subprocess.TimeoutExpired:
            sys.exit(f"{' '.join(command)}: never ended; a kick did not return")
        lines = result.stdout.splitlines()
        print(f"{os.path.basename(command[0])}{' fuzz ' + command[1] if len(command) > 1 else ''}: "
              + ("\n".join(lines[:60]) if result.returncode else lines[-1] if lines else "(nothing)"))
        if result.returncode != 0:
            sys.exit(f"{' '.join(command)}: exit {result.returncode}")

    for recording in arguments.recordings:
        go(recording)
        for seed in range(1, arguments.fuzz + 1):
            go(recording, str(seed))

    if arguments.coverage:
        merged = f"{work}/merged.profdata"
        run("llvm-profdata-18", "merge", "-o", merged, *profiles)
        shown = subprocess.run(["llvm-cov-18", "show", program, f"-instr-profile={merged}", f"{ROOT}/runtime/apu/xma.cpp"],
                               check=True, text=True, capture_output=True).stdout
        missed = [line for line in shown.splitlines() if re.match(r"\s*\d+\|\s*0\|", line)]
        print(f"--- apu/xma.cpp: {len(missed)} lines never run")
        for line in missed:
            print(line)

main()
