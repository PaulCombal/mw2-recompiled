#!/usr/bin/env python3
# Holds the zone reader in Rust (zone/src/: the definitions, the loading rules, the walk,
# the fastfile's zone, the shaders and their binding) to the C++ it replaced
# (launcher/zone/ and runtime/install/xex.cpp at --from, compiled from git as CMake
# compiled them: clang++-18 -O3, zlib 1.3.1). Both answer the same questions
# (zone_diff.cpp, zone_diff.rs) and print the same text:
#
#   - every type of both definition headers (the 360's and the PC game's), laid out, and
#     every constant; the loading rules of both folders as read;
#   - expressions and token strings made at random, valid and not, as both tokenisers,
#     the parser and the evaluation read them;
#   - each executable's binding tables, the memory move's declaration and its shaders;
#   - every fastfile of the title update and of the disc's version (and of the custom
#     maps, when maps/ has any): its zone, every pointer and data event of its walk in
#     order, where the walk ends, and its shaders, code word for code word, with what
#     became of its vertex shaders; fastfiles cut short and with damaged headers;
#   - zones damaged at random (a bit, a byte or a word changed, the zone cut short, most
#     in the structures at the zone's start):
#     the same shaders, or the same error, from both. Where the C++ crashes or never ends
#     on one -- it trusted sizes and offsets in the file the Rust checks -- the Rust must
#     still answer, and its answer is counted, not compared.
#
# The Rust is built with overflow checks and debug_assert! on; every run is under a
# memory cap.
#
#   usage: zone_diff.py [--from commit] [--src folder] [--work folder] [--quick]
#                       [--mutations N] [--jobs N] [--damage-only]
#
# --src takes the Rust (zone/src) from another folder, to try a deliberate mistake.
# Exits nonzero at the first difference.
import argparse, glob, multiprocessing, os, random, re, shutil, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# A walk of the largest zone (160 MB) and its copies; a damaged small zone.
MEMORY, DAMAGED_MEMORY = "4G", "1G"


def run(command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def capped(command, timeout=None, memory=MEMORY):
    """A command under a memory cap: (return code, stdout); None for one that never ended."""
    try:
        result = subprocess.run(["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={memory}",
                                 "-p", "MemorySwapMax=0", *command],
                                capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None
    return result.returncode, result.stdout.decode("utf-8", "replace")


def build_cpp(arguments, work):
    source = f"{work}/cpp"
    shutil.rmtree(source, ignore_errors=True)
    os.makedirs(source)
    archive = subprocess.run(["git", "-C", ROOT, "archive", arguments.cpp_from, "launcher/zone", "runtime/install",
                              "runtime/gpu/vulkan"], check=True, capture_output=True).stdout
    subprocess.run(["tar", "-x", "-C", source], input=archive, check=True)
    zlib = f"{work}/zlib"
    if not os.path.exists(f"{zlib}/inflate.c"):
        run(["git", "-c", "advice.detachedHead=false", "clone", "--quiet", "--depth", "1", "--branch", "v1.3.1", "https://github.com/madler/zlib.git", zlib])
    # The definitions, embedded as CMakeLists.txt embedded them.
    lines, table = ["// zone_diff.py's, as CMake made it."], []
    for folder, prefix in (("defs", ""), ("defs_pc", "pc/")):
        base = f"{source}/launcher/zone/{folder}"
        names = sorted(os.path.relpath(p, base) for p in glob.glob(f"{base}/*.h") + glob.glob(f"{base}/*.txt")
                       + glob.glob(f"{base}/assets/*.txt"))
        for name in names:
            data = open(f"{base}/{name}", "rb").read()
            lines.append(f"static const unsigned char kDef{len(table)}[] = {{ {','.join(str(b) for b in data)} }};")
            table.append(f'    {{ "{prefix}{name}", kDef{len(table)}, {len(data)} }},')
    lines.append("struct ZoneDefinition { const char* name; const unsigned char* data; unsigned size; };")
    lines.append("extern const ZoneDefinition kZoneDefinitions[] = {\n" + "\n".join(table) + "\n};")
    lines.append(f"extern const unsigned kZoneDefinitionCount = {len(table)};")
    open(f"{work}/defs.cpp", "w").write("\n".join(lines) + "\n")
    objects = []
    sources = sorted(glob.glob(f"{source}/launcher/zone/*.cpp")) + [f"{source}/runtime/install/xex.cpp",
               f"{source}/runtime/install/crypto.cpp", f"{work}/defs.cpp", f"{ROOT}/tools/zone_diff.cpp"]
    for path in sources:
        obj = f"{work}/obj/{os.path.basename(os.path.dirname(path))}_{os.path.basename(path)}.o"
        os.makedirs(os.path.dirname(obj), exist_ok=True)
        run(["clang++-18", "-std=gnu++20", "-O3", "-DNDEBUG", "-c", f"-I{source}/launcher", f"-I{source}/runtime",
             f"-I{zlib}", path, "-o", obj])
        objects.append(obj)
    for name in ("adler32", "crc32", "inffast", "inflate", "inftrees", "zutil"):
        obj = f"{work}/obj/zlib_{name}.o"
        run(["clang-18", "-O3", "-DNDEBUG", "-c", f"-I{zlib}", f"{zlib}/{name}.c", "-o", obj])
        objects.append(obj)
    program = f"{work}/zone_diff_cpp"
    run(["clang++-18", "-O3", "-o", program, *objects])
    return program


def build_rust(arguments, work):
    project = f"{work}/rs"
    crate = f"{project}/zone"
    shutil.rmtree(crate, ignore_errors=True)
    os.makedirs(crate)
    shutil.copytree(os.path.abspath(arguments.src), f"{crate}/src")
    os.symlink(f"{ROOT}/zone/defs", f"{crate}/defs")
    shutil.copy(f"{ROOT}/zone/build.rs", crate)
    manifest = open(f"{ROOT}/zone/Cargo.toml").read()
    manifest = manifest.replace('path = "../install"', f'path = "{ROOT}/install"')
    manifest = manifest.replace('path = "../shaders"', f'path = "{ROOT}/shaders"')
    manifest = manifest[:manifest.index("# The reader on its own")]
    open(f"{crate}/Cargo.toml", "w").write(manifest)
    open(f"{project}/Cargo.toml", "w").write(f"""[package]
name = "zone-diff"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
mw2-zone = {{ path = "zone" }}
mw2-install = {{ path = "{ROOT}/install" }}

[[bin]]
name = "zone_diff"
path = "{ROOT}/tools/zone_diff.rs"

[profile.release]
overflow-checks = true
debug-assertions = true
panic = "abort"

[workspace]
""")
    shutil.copy(f"{ROOT}/Cargo.lock", f"{project}/Cargo.lock")
    run(["cargo", "build", "--release", "--offline", "--quiet", "--manifest-path", f"{project}/Cargo.toml"],
        env={**os.environ, "CARGO_TARGET_DIR": f"{project}/target"})
    return f"{project}/target/release/zone_diff"


class Rig:
    def __init__(self, cpp, rust):
        self.cpp, self.rust = cpp, rust

    def both(self, *question, timeout=900, normalise=None):
        """The two answers to a question, which must be the same."""
        answers = []
        for program in (self.cpp, self.rust):
            got = capped([program, *question], timeout)
            if got is None:
                sys.exit(f"DIFFERENT: {os.path.basename(program)} never ended on {' '.join(question)}")
            code, out = got
            if code != 0:
                sys.exit(f"DIFFERENT: {os.path.basename(program)} failed ({code}) on {' '.join(question)}\n{out[-2000:]}")
            answers.append(normalise(out) if normalise else out)
        if answers[0] != answers[1]:
            a, b = answers[0].splitlines(), answers[1].splitlines()
            at = next((i for i in range(min(len(a), len(b))) if a[i] != b[i]), min(len(a), len(b)))
            show = lambda lines: "\n".join(lines[max(0, at - 2):at + 3])
            sys.exit(f"DIFFERENT on {' '.join(question)} at line {at + 1}:\n--- C++\n{show(a)}\n--- Rust\n{show(b)}")
        return answers[0]


# The C++ said why zlib refused a stream; the Rust's inflate has no number to give.
def without_zlib_code(text):
    return re.sub(r" \(zlib -?\d+\)", "", text)


def expressions(path, seed, count):
    r = random.Random(seed)
    atoms = ["a", "b", "c", "d", "e", "f", "g", "0", "1", "2", "7", "63", "64", "65", "0x1F", "0XFF", "017", "08",
             "4294967296", "9223372036854775807", "99999999999999999999", "1.5f", "3f", "x::y", "T::m::n"]
    ops = ["+", "-", "*", "/", "%", "<<", ">>", "&", "^", "|", "<", "<=", ">", ">=", "==", "!=", "&&", "||"]
    junk = ["(", ")", "[", "]", "!", "~", "::", ":", ";", ",", "{", "}", "=", "?", ".", "@", "$", " ", "\t"]

    def tree(depth):
        roll = r.random()
        if depth > 5 or roll < 0.3:
            return r.choice(atoms)
        if roll < 0.45:
            return r.choice(["-", "+", "~", "!"]) + tree(depth + 1)
        if roll < 0.55:
            return "(" + tree(depth + 1) + ")"
        return tree(depth + 1) + r.choice(["", " "]) + r.choice(ops) + r.choice(["", " "]) + tree(depth + 1)

    lines = []
    for _ in range(count):
        line = tree(0)
        if r.random() < 0.25:
            # Damaged: a token dropped, doubled or a stray one put in.
            at = r.randrange(len(line) + 1)
            line = line[:at] + r.choice(junk + ops + atoms) + line[at + r.choice([0, 1]):]
        if r.random() < 0.05:
            line = "".join(r.choice("ab01x_:()[]<>=!&|^~+-*/%.f ") for _ in range(r.randrange(1, 24)))
        lines.append(line)
    open(path, "w").write("\n".join(lines) + "\n")


def corpus(quick):
    """(name, tables' executables, fastfiles) for each set of the game's files there is."""
    sets = []
    for name, folder in (("tu6", f"{ROOT}/mw2"), ("tu0", f"{ROOT}/mw2/tu0")):
        files = sorted(p for p in glob.glob(f"{folder}/game/*.ff") if os.path.exists(p))
        if os.path.exists(f"{folder}/default_mp.xex") and files:
            if quick:
                files = [f for f in files if re.search(r"/(patch_mp|mp_rust_load|code_post_gfx|af_caves|mp_afghan)\.ff$", f)]
            sets.append((name, folder, files))
    maps = sorted(glob.glob(f"{ROOT}/out/player/maps/*.ff"))
    if maps and sets:
        sets.append(("maps", sets[0][1], maps[:2] if quick else maps))
    return sets


def mutation(task):
    cpp, rust, zone, tables, seed = task
    results = []
    for program in (cpp, rust):
        got = capped([program, "mutate", zone, tables, str(seed)], timeout=60, memory=DAMAGED_MEMORY)
        if got is None:
            results.append("never ended")
        elif got[0] != 0:
            results.append(f"died ({got[0]}) after: {got[1].strip()[-200:]}")
        else:
            results.append(got[1])
    return zone, tables, seed, results[0], results[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--from", dest="cpp_from", default="942f6e3")
    parser.add_argument("--src", default=f"{ROOT}/zone/src")
    parser.add_argument("--work", default="/tmp/zone_diff")
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--mutations", type=int, default=None)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--damage-only", action="store_true", help="only the damaged zones, of the first few files")
    arguments = parser.parse_args()
    work = os.path.abspath(arguments.work)
    os.makedirs(work, exist_ok=True)
    rig = Rig(build_cpp(arguments, work), build_rust(arguments, work))

    # The definitions the Rust embeds are the C++'s, moved.
    old, new = f"{work}/cpp/launcher/zone", f"{ROOT}/zone"
    for folder in ("defs", "defs_pc"):
        for path in sorted(glob.glob(f"{old}/{folder}/**/*", recursive=True)):
            if os.path.isfile(path) and path.endswith((".h", ".txt")):
                moved = f"{new}/{os.path.relpath(path, old)}"
                if not os.path.exists(moved) or open(path, "rb").read() != open(moved, "rb").read():
                    sys.exit(f"DIFFERENT: {os.path.relpath(moved, ROOT)} is not {arguments.cpp_from}'s copy")
    for folder in ("defs", "defs_pc"):
        rig.both("defs", f"{new}/{folder}/iw4_assets.h")
        out = rig.both("schema", f"{new}/{folder}")
        print(f"{folder}: the same types and rules ({out.count(chr(10) + 'struct ')} structs)")

    path = f"{work}/expressions.txt"
    count = 3000 if arguments.quick else 30000
    expressions(path, 1, count)
    rig.both("expr", path)
    print(f"expressions: the same {count}")

    mutations = arguments.mutations if arguments.mutations is not None else (300 if arguments.quick else 4000)
    damaged = []
    for name, folder, files in corpus(arguments.quick):
        tables = f"{work}/tables-{name}.bin"
        for multiplayer, xex in ((1, "default_mp.xex"), (0, "default.xex")):
            out = rig.both("executable", f"{folder}/{xex}", str(multiplayer))
            print(f"{name} {xex}: the same tables and {out.splitlines()[-1]}")
        run([rig.cpp, "tables", f"{folder}/default_mp.xex", "1", tables])
        zones = f"{work}/zones"
        os.makedirs(zones, exist_ok=True)
        for ff in files:
            if arguments.damage_only:
                zone = f"{zones}/{name}-{os.path.basename(ff)}.zone"
                if len(damaged) < 6 and os.path.getsize(ff) < 1 << 20:
                    run([rig.cpp, "inflate", ff, zone])
                    if os.path.getsize(zone) < 4 << 20:
                        damaged.append((zone, tables))
                continue
            label = f"{name} {os.path.basename(ff)}"
            rig.both("fastfile", ff, normalise=without_zlib_code)
            zone = f"{zones}/{name}-{os.path.basename(ff)}.zone"
            run([rig.cpp, "inflate", ff, zone])
            walked = rig.both("walk", zone).strip()
            shaders = rig.both("shaders", tables, ff).splitlines()
            print(f"{label}: {walked}; {', '.join(shaders[-2:])}")
            if os.path.getsize(zone) < 4 << 20 and len(damaged) < 6:
                damaged.append((zone, tables))
            else:
                os.remove(zone)
            # Cut short, and its header damaged.
            data = open(ff, "rb").read()
            if len(data) < 2 << 20:
                cut = f"{work}/cut.ff"
                for length in (0, 8, 0x1C, 0x1D, 0x30, 0x2000, 0x4000, len(data) // 2, len(data) - 1):
                    open(cut, "wb").write(data[:length])
                    rig.both("fastfile", cut, normalise=without_zlib_code)
                for at, value in ((0x19, 0xFF), (0x1A, 0x01), (0x25, 0x00), (0x26, 0xFF), (0x2005, 0x55)):
                    if at < len(data):
                        open(cut, "wb").write(data[:at] + bytes([value]) + data[at + 1:])
                        rig.both("fastfile", cut, normalise=without_zlib_code)

    # Zones damaged at random.
    if mutations and damaged:
        tasks = [(rig.cpp, rig.rust, zone, tables, seed) for seed in range(mutations) for zone, tables in damaged]
        tasks = tasks[:mutations]
        same = cpp_died = 0
        rust_after, answers = {}, {}
        with multiprocessing.Pool(arguments.jobs) as pool:
            for zone, tables, seed, cpp, rust in pool.imap_unordered(mutation, tasks):
                question = f"mutate {zone} {tables} {seed}"
                if rust == "never ended" or rust.startswith("died"):
                    sys.exit(f"DIFFERENT: the Rust {rust} on {question}\nC++: {cpp}")
                if cpp == "never ended" or cpp.startswith("died"):
                    cpp_died += 1
                    kind = rust.splitlines()[1] if len(rust.splitlines()) > 1 else rust
                    kind = re.sub(r"\d+", "N", kind)
                    rust_after[kind] = rust_after.get(kind, 0) + 1
                    continue
                if cpp != rust:
                    sys.exit(f"DIFFERENT on {question}:\n--- C++\n{cpp}--- Rust\n{rust}")
                same += 1
                answer = cpp.splitlines()[1] if len(cpp.splitlines()) > 1 else cpp
                answer = "ok" if answer.startswith("ok ") else re.sub(r"\d+", "N", answer)
                answers[answer] = answers.get(answer, 0) + 1
        print(f"damaged zones: {same} the same, {cpp_died} where the C++ crashed or never ended")
        for answer, n in sorted(answers.items(), key=lambda item: -item[1]):
            print(f"  {n}: both: {answer}")
        for kind, n in sorted(rust_after.items(), key=lambda item: -item[1]):
            print(f"  {n}: the Rust: {kind}")
    print("all the same")


main()
