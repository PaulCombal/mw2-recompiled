#!/usr/bin/env python3
"""The shader rig: the C++ translator against the Rust one, word for word.

Builds runtime/gpu/vulkan/shader_translator.cpp (with spirv.cpp) and the Rust
shader folder (src/runtime/gpu/shader, as a static library through
shader_translator_rust.cpp's forwarders) into one program, shader_diff.cpp,
and runs every program through both, unscaled and scaled: ok, the error text,
every metadata field, the SPIR-V words and the Describe text must agree. It
stops at the first difference and prints the case.

    tools/shader_diff/shader_diff.py                 corpus, fuzzer, bindings, layout
    tools/shader_diff/shader_diff.py --cases 1000    a shorter fuzz
    tools/shader_diff/shader_diff.py --src DIR       the Rust from another folder
    tools/shader_diff/shader_diff.py --validate 8    spirv-val on every 8th program
    tools/shader_diff/shader_diff.py --bench         both at -O3, the corpus timed

The corpus is every *.ucode under diagnosis/ (the main checkout's, from a
worktree), or --corpus DIR. The name says pixel or vertex. The whole run sits
in a systemd scope capped at 4 GB, as every run on this machine does.
"""

import argparse
import concurrent.futures
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
TOOLCHAIN = os.path.expanduser("~/.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin")
CXX = "clang++-18"
CXXFLAGS = ["-std=gnu++20", "-march=x86-64-v2", "-DMW2_DIAGNOSTICS=1", "-DMW2_HAVE_VULKAN=1",
            "-I" + os.path.join(ROOT, "runtime")]
RUSTFLAGS = ["--edition", "2024", "--crate-type=staticlib", "--crate-name", "mw2_shader",
             "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
             # The build's cfg for a Rust-renderer build that owns the translator (build.rs).
             "--cfg", "mw2_owns_shader_translator", "--check-cfg", "cfg(mw2_owns_shader_translator)"]


def scoped():
    """Re-runs this script inside a 4 GB systemd scope, once."""
    if os.environ.get("MW2_SHADER_DIFF_SCOPED") or not shutil.which("systemd-run"):
        return
    env = dict(os.environ, MW2_SHADER_DIFF_SCOPED="1")
    command = ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryMax=4G", "-p", "MemorySwapMax=0",
               sys.executable, "-u", os.path.abspath(__file__)] + sys.argv[1:]
    os.execvpe(command[0], command, env)


def run(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, **kwargs)
    if result.returncode != 0:
        sys.stdout.write(result.stdout)
        sys.stdout.write(result.stderr)
        sys.exit("failed: " + " ".join(command))
    return result


def build(out, src, optimised, coverage=False):
    """The rig's program in `out`, from the Rust in `src`. `coverage`
    instruments the C++ translator for llvm-cov."""
    os.makedirs(out, exist_ok=True)
    rust = os.path.join(out, "rust")
    shutil.rmtree(rust, ignore_errors=True)
    shutil.copytree(src, os.path.join(rust, "shader"))
    shutil.copy(os.path.join(HERE, "root.rs"), rust)
    level = ["-O3"] if optimised else ["-O1"]
    rust_level = (["-Copt-level=3"] if optimised else
                  ["-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on"])
    vulkan = os.path.join(ROOT, "runtime", "gpu", "vulkan")
    instrument = ["-fprofile-instr-generate", "-fcoverage-mapping"] if coverage else []
    jobs = [
        [CXX] + CXXFLAGS + level + instrument + ["-c", os.path.join(vulkan, "shader_translator.cpp"),
                                                 "-o", os.path.join(out, "shader_translator.o")],
        [CXX] + CXXFLAGS + level + instrument + ["-c", os.path.join(vulkan, "spirv.cpp"),
                                                 "-o", os.path.join(out, "spirv.o")],
        [CXX] + CXXFLAGS + level + ["-c", os.path.join(HERE, "shader_diff.cpp"),
                                    "-o", os.path.join(out, "shader_diff.o")],
        [CXX] + CXXFLAGS + level + ["-DRUST_SIDE", "-Dshader=shader_rust", "-DMW2_RUST_RENDERER=1",
                                    "-c", os.path.join(HERE, "shader_diff.cpp"), "-o", os.path.join(out, "rust_side.o")],
        ["rustc"] + RUSTFLAGS + rust_level + [os.path.join(rust, "root.rs"),
                                              "-o", os.path.join(out, "libmw2_shader.a")],
    ]
    env = dict(os.environ)
    if os.path.isdir(TOOLCHAIN):
        env["PATH"] = TOOLCHAIN + os.pathsep + env["PATH"]
    with concurrent.futures.ThreadPoolExecutor(len(jobs)) as pool:
        list(pool.map(lambda c: run(c, env=env), jobs))
    program = os.path.join(out, "shader_diff")
    run([CXX, os.path.join(out, "shader_translator.o"), os.path.join(out, "spirv.o"),
         os.path.join(out, "shader_diff.o"), os.path.join(out, "rust_side.o"),
         os.path.join(out, "libmw2_shader.a"), "-lpthread", "-ldl", "-lm", "-o", program] + instrument)
    return program


def build_windows(out, src, llvm_mingw):
    """The same program for Windows: llvm-mingw's clang for the C++ (what the Windows
    build's launcher compiles the translator with) and the Rust for the Windows build's
    target, both optimised as the game is, to run under Wine."""
    os.makedirs(out, exist_ok=True)
    rust = os.path.join(out, "rust")
    shutil.rmtree(rust, ignore_errors=True)
    shutil.copytree(src, os.path.join(rust, "shader"))
    shutil.copy(os.path.join(HERE, "root.rs"), rust)
    cxx = os.path.join(llvm_mingw, "bin", "x86_64-w64-mingw32-clang++")
    flags = [f for f in CXXFLAGS] + ["-O2"]
    vulkan = os.path.join(ROOT, "runtime", "gpu", "vulkan")
    env = dict(os.environ)
    if os.path.isdir(TOOLCHAIN):
        env["PATH"] = TOOLCHAIN + os.pathsep + env["PATH"]
    jobs = [
        [cxx] + flags + ["-c", os.path.join(vulkan, "shader_translator.cpp"), "-o", os.path.join(out, "shader_translator.o")],
        [cxx] + flags + ["-c", os.path.join(vulkan, "spirv.cpp"), "-o", os.path.join(out, "spirv.o")],
        [cxx] + flags + ["-c", os.path.join(HERE, "shader_diff.cpp"), "-o", os.path.join(out, "shader_diff.o")],
        [cxx] + flags + ["-DRUST_SIDE", "-Dshader=shader_rust", "-DMW2_RUST_RENDERER=1",
                         "-c", os.path.join(HERE, "shader_diff.cpp"), "-o", os.path.join(out, "rust_side.o")],
    ]
    with concurrent.futures.ThreadPoolExecutor(len(jobs)) as pool:
        list(pool.map(lambda c: run(c, env=env), jobs))
    # What the Rust standard library needs from Windows, as rustc says.
    r = run(["rustc"] + RUSTFLAGS + ["-Copt-level=3", "--target", "x86_64-pc-windows-gnullvm",
                                     "--print", "native-static-libs", os.path.join(rust, "root.rs"),
                                     "-o", os.path.join(out, "libmw2_shader.a")], env=env)
    native = re.search(r"native-static-libs: (.*)", r.stdout + r.stderr).group(1).split()
    program = os.path.join(out, "shader_diff.exe")
    run([cxx, os.path.join(out, "shader_translator.o"), os.path.join(out, "spirv.o"),
         os.path.join(out, "shader_diff.o"), os.path.join(out, "rust_side.o"),
         os.path.join(out, "libmw2_shader.a"), "-static", "-o", program] + native)
    return program


def windows(out, src, corpus, llvm_mingw):
    """The corpus through both Windows-built translators under Wine, from a scratch
    folder (Wine is another machine: never the repository's root)."""
    program = build_windows(os.path.join(out, "windows"), src, llvm_mingw)
    scratch = os.path.join(out, "windows", "scratch")
    os.makedirs(scratch, exist_ok=True)
    # The corpus, then the other modes once each: the fuzzer, Describe's truncation over
    # any values and the SPIR-V writer on its own.
    for mode in (["corpus", corpus], ["fuzz", corpus, "1", "200000"], ["describe", "1", "100000"],
                 ["module", "1", "20000"]):
        r = subprocess.run(["wine", program] + mode, capture_output=True, text=True, cwd=scratch,
                           env=dict(os.environ, WINEDEBUG="-all"))
        sys.stdout.write("windows: " + r.stdout + "".join(l + "\n" for l in r.stderr.splitlines() if "fixme" not in l))
        if r.returncode != 0:
            return False
    return True


def coverage(out, src, corpus, seeds, cases):
    """Which lines of the C++ translator the corpus and the fuzzer reach."""
    program = build(os.path.join(out, "coverage"), src, False, coverage=True)
    profiles = os.path.join(out, "coverage", "profiles")
    shutil.rmtree(profiles, ignore_errors=True)
    os.makedirs(profiles)
    env = dict(os.environ, LLVM_PROFILE_FILE=os.path.join(profiles, "%p.profraw"))
    runs = [[program, "corpus", corpus]] + [[program, "fuzz", corpus, str(seed), str(cases)]
                                            for seed in range(1, seeds + 1)]
    with concurrent.futures.ThreadPoolExecutor(len(runs)) as pool:
        for r in pool.map(lambda c: run(c, env=env), runs):
            sys.stdout.write(r.stdout)
    data = os.path.join(out, "coverage", "merged.profdata")
    run(["llvm-profdata-18", "merge", "-o", data] +
        [os.path.join(profiles, n) for n in os.listdir(profiles)])
    sources = [os.path.join(ROOT, "runtime", "gpu", "vulkan", n) for n in ("shader_translator.cpp", "spirv.cpp")]
    report = run(["llvm-cov-18", "report", program, "-instr-profile=" + data] + sources).stdout
    print(report.rstrip())
    shown = run(["llvm-cov-18", "show", program, "-instr-profile=" + data, sources[0]]).stdout
    unreached = []
    for line in shown.splitlines():
        m = re.match(r"\s*(\d+)\|\s*0\|(.*)", line)
        if m:
            unreached.append("  %5s %s" % (m.group(1), m.group(2).strip()))
    print("shader_translator.cpp lines never run (%d):" % len(unreached))
    print("\n".join(unreached))


def corpus_list(corpus, out):
    if corpus is None:
        corpus = os.path.join(ROOT, "diagnosis")
        if not os.path.isdir(corpus):
            # A worktree has no diagnosis/ of its own: the main checkout's.
            common = run(["git", "-C", ROOT, "rev-parse", "--path-format=absolute", "--git-common-dir"]).stdout
            corpus = os.path.join(os.path.dirname(common.strip()), "diagnosis")
    files = []
    for directory, _, names in os.walk(corpus):
        files.extend(os.path.join(directory, n) for n in names if n.endswith(".ucode"))
    files.sort()
    path = os.path.join(out, "corpus.txt")
    with open(path, "w") as f:
        f.write("".join(p + "\n" for p in files))
    return path, len(files), corpus


def bindings(program):
    text = run([program, "bindings"]).stdout
    cpp_part, rust_part = text.split("== Rust\n")
    parse = lambda part: dict(l.split() for l in part.splitlines() if l and not l.startswith("=="))
    cpp, rust = parse(cpp_part), parse(rust_part)
    # Every constant of each file is in its list.
    header = open(os.path.join(ROOT, "runtime", "gpu", "vulkan", "bindings.h")).read()
    declared_cpp = set(re.findall(r"constexpr\s+uint32_t\s+(k\w+)", header) +
                       re.findall(r",\s*(k\w+)\s*=", header))
    rust_source = open(os.path.join(SRC, "bindings.rs")).read()
    declared_rust = re.findall(r"pub const ([A-Z_0-9]+): u32", rust_source)
    problems = []
    if set(cpp) != declared_cpp:
        problems.append("shader_diff.cpp prints %s, bindings.h declares %s" % (sorted(cpp), sorted(declared_cpp)))
    if len(rust) != len(declared_rust):
        problems.append("bindings.rs declares %d constants and lists %d" % (len(declared_rust), len(rust)))
    for name in sorted(set(cpp) | set(rust)):
        if cpp.get(name) != rust.get(name):
            problems.append("%s: C++ %s, Rust %s" % (name, cpp.get(name), rust.get(name)))
    if problems:
        print("bindings: DIFFERENCE")
        for p in problems:
            print("  " + p)
        return False
    print("bindings: %d constants, the same names and values" % len(cpp))
    return True


def layout(program):
    """The metadata the C interface hands over, as each compiler lays it out:
    the forwarder's Meta and TranslationMeta, field for field, in order."""
    text = run([program, "layout"]).stdout
    cpp_part, rust_part = text.split("== Rust\n")
    parse = lambda part: [l.split() for l in part.splitlines() if l and not l.startswith("==")]
    snake = lambda name: re.sub(r"([A-Z])", lambda m: "_" + m.group(1).lower(), name)
    cpp = [[snake(l[0])] + l[1:] for l in parse(cpp_part)]
    rust = parse(rust_part)
    # Every field the rig compares (Flatten), but `ok`, which is the result.
    flattened = re.findall(r'(?:one|many)\("(\w+)"', open(os.path.join(HERE, "shader_diff.cpp")).read())
    expected = [snake(n) for n in flattened if n != "ok"] + ["size"]
    problems = []
    for side, rows in (("C++", cpp), ("Rust", rust)):
        if [r[0] for r in rows] != expected:
            problems.append("%s lists %s, the rig compares %s" % (side, [r[0] for r in rows], expected))
    if cpp != rust:
        for c, r in zip(cpp, rust):
            if c != r:
                problems.append("%s: C++ offset/size %s, Rust %s" % (c[0], " ".join(c[1:]), " ".join(r[1:])))
    if problems:
        print("layout: DIFFERENCE")
        for p in problems:
            print("  " + p)
        return False
    size = dict((r[0], r[1:]) for r in cpp)["size"]
    print("layout: %d fields, %s bytes aligned to %s, the same offsets and sizes" % (len(cpp) - 1, size[0], size[1]))
    return True


def writer(program, seeds, scripts):
    """The SPIR-V writers alone, and Describe of any values."""
    ok = True
    for mode, what in (("module", "spirv writer: %d seeds x %d scripts of calls"),
                       ("describe", "describe: %d seeds x %d sets of any values")):
        processes = [subprocess.Popen([program, mode, str(seed), str(scripts)], stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, text=True) for seed in range(1, seeds + 1)]
        good = True
        for p in processes:
            output, _ = p.communicate()
            if p.returncode != 0:
                sys.stdout.write(output)
                good = False
        print((what + ": %s") % (seeds, scripts, "all identical" if good else "DIFFERENCE"))
        ok = ok and good
    return ok


def fuzz(program, corpus, seeds, cases):
    start = time.time()
    processes = [subprocess.Popen([program, "fuzz", corpus, str(seed), str(cases)], stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, text=True) for seed in range(1, seeds + 1)]
    ok = True
    for p in processes:
        output, _ = p.communicate()
        sys.stdout.write(output)
        if p.returncode != 0:
            ok = False
    print("fuzz: %d seeds x %d cases in %.0f s: %s" % (seeds, cases, time.time() - start,
                                                     "all identical" if ok else "DIFFERENCE"))
    return ok


def validate(program, corpus, out, every):
    validator = shutil.which("spirv-val")
    if not validator:
        print("validate: spirv-val is not installed")
        return True
    directory = os.path.join(out, "dump")
    shutil.rmtree(directory, ignore_errors=True)
    os.makedirs(directory)
    print(run([program, "dump", corpus, directory, str(every)]).stdout.strip())
    modules = sorted(os.path.join(directory, n) for n in os.listdir(directory))

    def check(path):
        r = subprocess.run([validator, "--target-env", "vulkan1.1", path], capture_output=True, text=True)
        return path, r.returncode, (r.stdout + r.stderr).strip()

    with concurrent.futures.ThreadPoolExecutor(os.cpu_count() or 4) as pool:
        results = list(pool.map(check, modules))
    bad = [r for r in results if r[1] != 0]
    print("validate: spirv-val --target-env vulkan1.1 on %d Rust modules: %d valid, %d invalid"
          % (len(results), len(results) - len(bad), len(bad)))
    for path, _, text in bad[:5]:
        print("  %s: %s" % (os.path.basename(path), text.splitlines()[0] if text else ""))
    return not bad


def bench(out, src, corpus, repeats):
    program = build(os.path.join(out, "bench"), src, True)
    best = {"cpp": [], "rust": []}
    for _ in range(repeats):
        for side in ("cpp", "rust"):          # interleaved, so background load lands on both
            line = run([program, "bench", corpus, side, "3"]).stdout.strip()
            print(line)
            best[side].append(float(re.search(r"rounds ([0-9.]+) ms", line).group(1)))
    cpp, rust = min(best["cpp"]), min(best["rust"])
    print("bench: C++ %.1f ms, Rust %.1f ms for the corpus unscaled and scaled, one thread: Rust/C++ = %.3f"
          % (cpp, rust, rust / cpp))


def main():
    global SRC
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--src", default=os.path.join(ROOT, "src", "runtime", "gpu", "shader"),
                        help="the Rust shader folder to test")
    parser.add_argument("--out", default=os.path.join(ROOT, "out", "shader_diff"), help="where to build")
    parser.add_argument("--corpus", help="the folder whose *.ucode are the corpus (default diagnosis/)")
    parser.add_argument("--cases", type=int, default=1000000, help="fuzz cases a seed")
    parser.add_argument("--seeds", type=int, default=8, help="fuzz seeds, one process each")
    parser.add_argument("--scripts", type=int, default=100000,
                        help="scripts of SPIR-V writer calls a seed, the writers held alone")
    parser.add_argument("--no-corpus", action="store_true")
    parser.add_argument("--no-fuzz", action="store_true")
    parser.add_argument("--validate", type=int, default=0, metavar="EVERY",
                        help="spirv-val the Rust output of every EVERY-th corpus program")
    parser.add_argument("--bench", action="store_true", help="build both at -O3 and time the corpus")
    parser.add_argument("--windows", metavar="LLVM_MINGW",
                        help="build both for Windows with this llvm-mingw and run the corpus under Wine")
    parser.add_argument("--coverage", action="store_true",
                        help="the lines of the C++ translator the corpus and the fuzz reach (llvm-cov-18)")
    args = parser.parse_args()
    scoped()
    SRC = os.path.abspath(args.src)
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    corpus, count, where = corpus_list(args.corpus, out)
    print("corpus: %d programs under %s" % (count, where))

    if args.bench:
        bench(out, SRC, corpus, 3)
        return 0
    if args.windows:
        return 0 if windows(out, SRC, corpus, os.path.abspath(args.windows)) else 1
    if args.coverage:
        coverage(out, SRC, corpus, args.seeds, args.cases)
        return 0

    start = time.time()
    program = build(os.path.join(out, "check"), SRC, False)
    print("built in %.0f s (C++ -O1, Rust -Copt-level=1 with debug assertions and overflow checks)"
          % (time.time() - start))
    if not bindings(program):
        return 1
    if not layout(program):
        return 1
    if args.scripts > 0 and not writer(program, args.seeds, args.scripts):
        return 1
    if not args.no_corpus:
        r = subprocess.run([program, "corpus", corpus], capture_output=True, text=True)
        sys.stdout.write(r.stdout + r.stderr)
        if r.returncode != 0:
            return 1
    if args.validate and not validate(program, corpus, out, args.validate):
        return 1
    if not args.no_fuzz and args.cases > 0 and not fuzz(program, corpus, args.seeds, args.cases):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
