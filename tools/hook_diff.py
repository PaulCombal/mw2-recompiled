#!/usr/bin/env python3
# Compares every hook of src/hooks/ with the C++ of the same name in runtime/, on the build's title (see
# hook_diff.cpp). The C++ files are compiled as they are, with their sub_X renamed
# so that both can be in one program, and src/ as a static library with overflow
# checks and debug_assert! on, so that a panic path a hostile input reaches stops
# the run; the title's own functions are stubs.
#
#   usage: hook_diff.py <build folder> [--seeds N] [--cases N] [--hook T_x[,T_y...]]
#                       [--src folder] [--coverage] [--release]
#
# --src takes the Rust from another folder, to try a deliberate mistake: a run that
# does not fail on one has not tested that line. --coverage lists the lines of the
# C++ hooks no case reached. Exits nonzero at the first difference.
#
# The renderer's hooks (runtime/gpu/<n>.cpp, runtime/image_move.cpp and
# runtime/shader_preload.cpp) are paired only where the build has them in Rust:
# <build>/renderer_cfgs.txt names a cfg mw2_owns_<n> for each C++ file Rust owns. A
# C++-renderer build (renderer cpp) leaves them out on both sides. Their calls into the
# renderer are loggers on both sides (hook_diff.cpp).
import argparse, glob, os, re, resource, shutil, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# The renderer's C++ files outside runtime/gpu/, which RUST_OWNS may list (build.rs).
RENDERER_FILES = {"image_move", "shader_preload"}
# What a renderer hook's C++ calls of its own renderer, compiled beside it as it is: the
# D3D hooks set the arena's progress block (arena.cpp, whose Rust is arena.rs).
COMPANIONS = {"d3d_hooks": ["gpu/arena.cpp"]}
GUEST = re.compile(r"^(__imp__)?sub_[0-9A-F]{8}$")
# The switches the C++ and the Rust each read once. Each set is a process of its own.
# With MW2_TRACE_PACING and MW2_STUTTERS, the pacing notes and the stutter detector's
# calls log here (src/runtime/pacing_trace.rs, stutters.rs), so their order is compared.
ENVIRONMENTS = [{}, {"MW2_FOV": "90", "MW2_CUSTOM_MAPS": "1"}, {"MW2_FOV": "130", "MW2_ANISOTROPY": "8x"}, {"MW2_FOV": "nan", "MW2_TRACE_PACING": "/dev/null", "MW2_STUTTERS": "1"}, {"MW2_ANISOTROPY": "3", "MW2_CUSTOM_MAPS": "1"},
                {"MW2_ENGINE_LOG": "1", "MW2_TRACE_CBUF": "1"}, {"MW2_ENGINE_LOG": "1"}, {"MW2_RENDER": "960x600", "MW2_CUSTOM_MAPS": "1"}, {"MW2_RENDER": "1280x800"}, {"MW2_RENDER": "640x480", "MW2_CUSTOM_MAPS": "1"}, {"MW2_RENDER": "1600x900"}, {"MW2_RENDER": " 1024x+640"}]

# The custom maps' hooks list .arena files in the game folder: each seed gets a folder
# of its own, of arenas whose maps are installed or not, with a picture or not, and
# whose text is well formed, odd, or too large for the title's buffer. The texts are
# tools/hook_diff.cpp's kMapTexts, which the string lookup is asked for.
MAP_TEXTS = [b"Dust II", b"Crash Remix", b"A map of sand and doors", b"@MPUI_AFGHAN", b"x", b"Underpass at night",
             b"\xC9t\xE9", b"MPUI_DESC_MAP_RUST"]
# 55, 56 and 57 letters: a picture's zone name of 63, 64 and 65 bytes, either side of the 64 it must fit.
STEMS = ["mp_dust2", "mp_crash_snow", "mp_x", "a" * 57, "a" * 55, "a" * 56, "mp_\u00e9t\u00e9", "zz", "mp_strike", "mp_q", "Mp_Upper"]

def arena_text(random):
    keys = [b"map", b"longname", b"description", b"mapimage", b"other", b"map2", b"2map", b"_map"]
    values = [stem.encode() for stem in STEMS[:4]] + MAP_TEXTS + [b"", b"mp_rust", b"loadscreen_mp_x", b"Dust\0II"]
    parts = []
    for _ in range(random.choice([0, 1, 1, 2, 3, 6, 14])):
        entry = [b"{"]
        if random.randrange(4):   # most entries name their map, and say what it is called
            entry.append(b'map "' + random.choice(values[:4] + [b"mp_rust", b""]) + b'" ')
        for key in [b"longname", b"description"]:
            if random.randrange(2):
                entry.append(key + b' "' + random.choice(MAP_TEXTS + [b"", b"Dust\0II"]) + b'" ')
        for _ in range(random.randrange(6)):
            key = random.choice(keys)
            entry.append(key if random.randrange(4) else b'"' + key + b'"')
            entry.append(b'"' + random.choice(values) + b'"')
            entry.append(random.choice([b" ", b"\n", b"\t", b"", b"\r\n"]))
        if random.randrange(6) == 0:
            entry.insert(random.randrange(len(entry) + 1), random.choice([b"{", b"}", b'"', b"\0", b"9x", b"//", b"\xE9"]))
        entry.append(b"}" if random.randrange(8) else b"")
        parts.append(b"".join(entry))
    text = b"\n".join(parts)
    if random.randrange(4) == 0:
        text += b" " * (0x18000 - len(text) + random.choice([-1, 0, 0, 1]))   # the buffer's size, and either side
    return text

def game_folder(path, seed):
    import random as randomness
    random = randomness.Random(seed)
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path)
    for stem in random.sample(STEMS, random.randrange(len(STEMS) + 1)):
        open(f"{path}/{stem}.arena", "wb").write(arena_text(random))
        for ending in [".ff", "_load.ff", "_preview.ff"]:
            if random.randrange(6):
                open(f"{path}/{stem}{ending}", "wb").close()
    if random.randrange(4) == 0:
        os.makedirs(f"{path}/folder.arena")

# A limit of its own, wherever it is started from: a store over gigabytes of the
# guest is kept page by page, and running this out of memory takes the desktop with
# it. The 4 GB of guest memory count here, though they are not resident.
ADDRESS_SPACE = 8 << 30
SECONDS_PER_SEED = 900

def limited():
    resource.setrlimit(resource.RLIMIT_AS, (ADDRESS_SPACE, ADDRESS_SPACE))

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

def symbols(path, flags):
    out = subprocess.run(["nm", *flags, path], check=True, text=True, capture_output=True).stdout
    return [line.split()[-1] for line in out.splitlines() if line.strip()]

def script_output(build):
    outputs = sorted(glob.glob(f"{build}/rust/release/build/mw2-*/output"), key=os.path.getmtime)
    if not outputs:
        sys.exit(f"{build} has no Cargo build: build it first")
    return outputs[-1]

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--seeds", type=int, default=8)
    parser.add_argument("--cases", type=int, default=20000)
    parser.add_argument("--hook")
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--coverage", action="store_true")
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--work")
    arguments = parser.parse_args()

    build = os.path.abspath(arguments.build)
    work = arguments.work or f"{build}/hook_diff"
    os.makedirs(work, exist_ok=True)

    output = script_output(build)
    out_dir = f"{os.path.dirname(output)}/out"
    table = open(f"{out_dir}/title.rs").read()
    cfgs = [line.split("=", 1)[1] for line in open(output).read().splitlines() if line.startswith("cargo:rustc-cfg=")]
    title_mp = "mw2_title_mp" in cfgs
    tu0 = "mw2_version_tu0" in cfgs
    ppc = f"{ROOT}/ppc{'_mp' if title_mp else ''}{'_tu0' if tu0 else ''}"

    # Which renderer this build has, and which of its C++ files Rust owns.
    if not os.path.exists(f"{build}/renderer_cfgs.txt"):
        sys.exit(f"{build} has no renderer_cfgs.txt: build it again")
    renderer = open(f"{build}/renderer_cfgs.txt").read().split()
    rust_renderer = "rust" in renderer
    owned = {kind[len("mw2_owns_"):] for kind in renderer if kind.startswith("mw2_owns_")}

    # Every hook of src/hooks/ that has a C++ one of the same name in runtime/ or
    # runtime/gpu/; a renderer file's only where Rust owns it in this build.
    def cpp_of(name):
        if name in RENDERER_FILES:
            return f"{name}.cpp" if name in owned and os.path.exists(f"{ROOT}/runtime/{name}.cpp") else None
        if os.path.exists(f"{ROOT}/runtime/{name}.cpp"):
            return f"{name}.cpp"
        if os.path.exists(f"{ROOT}/runtime/gpu/{name}.cpp"):
            return f"gpu/{name}.cpp" if name in owned else None
        return None
    sources = {name: cpp_of(name) for name in (os.path.basename(path)[:-3] for path in glob.glob(f"{ROOT}/src/hooks/*.rs"))}
    ported = sorted(name for name, source in sources.items() if source)
    names = {address: name for name, address in re.findall(r'\((T_\w+)\) => \{ "sub_([0-9A-F]{8})" \}', table)}
    data = sorted({int(value, 16) for value in re.findall(r"pub const T_DATA_\w+: u32 = (0x[0-9A-Fa-f]+)", table)} - {0})

    # The C++.
    defines = ["-DMW2_DIAGNOSTICS=1", "-DMW2_TRACE_INDIRECT=1"]
    defines += ["-DMW2_TITLE_MP=1"] * title_mp + ["-DMW2_VERSION_TU0=1"] * tu0
    includes = [f"-I{ROOT}/runtime", f"-I{ppc}", f"-I{ROOT}/XenonRecomp-rs/thirdparty/simde"]
    compiler = ["clang++-18", "-std=gnu++20", "-O1", "-march=x86-64-v2", "-w", *defines, *includes]
    coverage = ["-fprofile-instr-generate", "-fcoverage-mapping"] if arguments.coverage else []
    tag = "cov" if arguments.coverage else "plain"
    objects = []
    # A header the C++ includes changes it as much as its own file does.
    headers = max(os.path.getmtime(path) for path in glob.glob(f"{ROOT}/runtime/**/*.h", recursive=True))
    files = [sources[name] for name in ported]
    files += [companion for name in ported for companion in COMPANIONS.get(name, [])]
    for source in files:
        path, object_path = f"{ROOT}/runtime/{source}", f"{work}/{source[:-4].replace('/', '_')}.{tag}.o"
        if not os.path.exists(object_path) or os.path.getmtime(object_path) < max(os.path.getmtime(path), headers):
            run(*compiler, *coverage, "-c", path, "-o", object_path)
        objects.append(object_path)

    defined = {}
    exported = set()
    for path in objects:
        for symbol in symbols(path, ["--defined-only", "--extern-only"]):
            if GUEST.match(symbol) and not symbol.startswith("__imp__"):
                defined[symbol] = path
            # What a C++ file gives the C++ that src/runtime/ gives the Rust.
            elif symbol.startswith("mw2_") and symbol != "mw2_log":
                exported.add(symbol)
    renames = sum((["--redefine-sym", f"{symbol}=cpp_{symbol}"] for symbol in [*defined, *exported]), [])
    renamed = []
    for path in objects:
        target = path.replace(".o", ".renamed.o")
        run("objcopy", *renames, path, target)
        renamed.append(target)

    # The Rust.
    root = f"{work}/rust_root.rs"
    source = arguments.src
    lines = ["#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs, improper_ctypes)]\n"]
    for module in ["env", "guest", "log", "title"]:
        lines.append(f'#[path = "{source}/{module}.rs"] mod {module};\n')
    lines.append(f'#[path = "{source}/runtime/mod.rs"] mod runtime;\n')
    lines.append(f'#[deny(unsafe_code)] #[path = "{source}/hooks/mod.rs"] mod hooks;\n')
    open(root, "w").write("".join(lines))
    def rlib(name):
        return sorted(glob.glob(f"{build}/rust/release/deps/lib{name}-*.rlib"), key=os.path.getmtime)[-1]
    variables = dict(line.split("=", 2)[1:] for line in open(output).read().splitlines() if line.startswith("cargo:rustc-env="))
    library = f"{work}/libhooks.a"
    checks = ["-Copt-level=1"] if arguments.release else ["-Copt-level=1", "-Cdebug-assertions=on", "-Coverflow-checks=on"]
    # The Rust renderer's build: its feature, its cfgs (which the build script's output
    # also holds) and ash.
    features = ["--cfg", 'feature="diagnostics"']
    externs = ["--extern", f"ppc_rt={rlib('ppc_rt')}", "--extern", f"sdl3_sys={rlib('sdl3_sys')}"]
    if rust_renderer:
        features += ["--cfg", 'feature="rust-renderer"']
        cfgs += [f"mw2_owns_{name}" for name in sorted(owned) if f"mw2_owns_{name}" not in cfgs]
        externs += ["--extern", f"ash={rlib('ash')}"]
    run("rustc", "--edition=2024", "--crate-type=staticlib", "--crate-name=hooks", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
        *checks, *features, "--cfg", "mw2_hook_rig", *sum((["--cfg", cfg] for cfg in cfgs), []), "--cap-lints=warn", *externs,
        "-L", f"dependency={build}/rust/release/deps", "-o", library, root,
        env={**os.environ, **variables, "OUT_DIR": out_dir, "MW2_RELEASE_TAG": os.environ.get("MW2_RELEASE_TAG", "")})

    rust_defined = {symbol for symbol in symbols(library, ["--defined-only", "--extern-only"]) if GUEST.match(symbol)}
    # The kernel's hooks (src/runtime/) are tools/kernel_diff.py's.
    kernel_hooks = set()
    for path in glob.glob(f"{source}/runtime/**/*.rs", recursive=True):
        kernel_hooks |= set(re.findall(r"guest_hook!\(\s*(T_\w+)", open(path).read()))
    addresses = {name: address for address, name in names.items()}
    kernel_hooks = {f"sub_{addresses[name]}" for name in kernel_hooks if name in addresses}
    rust_hooks = {symbol for symbol in rust_defined if not symbol.startswith("__imp__")} - kernel_hooks
    if rust_hooks != set(defined):
        sys.exit(f"not the same hooks: only C++ {sorted(set(defined) - rust_hooks)}, only Rust {sorted(rust_hooks - set(defined))}")

    undefined = set()
    for path in renamed + [library]:
        undefined |= {symbol for symbol in symbols(path, ["--undefined-only"]) if GUEST.match(symbol)}
    undefined -= rust_defined

    def stub_name(symbol):
        imported = symbol.startswith("__imp__")
        name = names.get(symbol[-8:], symbol)
        return f"__imp__{name}" if imported else name

    stubs = sorted(undefined)
    header = ["struct PPCContext;\n"]
    for symbol in sorted(defined):
        header.append(f'extern "C" void {symbol}(PPCContext&, uint8_t*); extern "C" void cpp_{symbol}(PPCContext&, uint8_t*);\n')
    header.append("constexpr Stub kStubs[] = {\n" + "".join(f'    {{ "{stub_name(symbol)}" }},\n' for symbol in stubs) + "};\n")
    for index, symbol in enumerate(stubs):
        header.append(f'extern "C" void {symbol}(PPCContext& ctx, uint8_t* base) {{ StubCalled({index}, ctx, base); }}\n')
    header.append("constexpr Hook kHooks[] = {\n")
    for symbol in sorted(defined):
        header.append(f'    {{ "{names.get(symbol[-8:], symbol)}", {{ cpp_{symbol}, {symbol} }} }},\n')
    header.append("};\n")
    reader = next((symbol for symbol in defined if names.get(symbol[-8:]) == "T_MSG_ReadBitsCompress"), None)
    header.append(f"constexpr Function kReadBitsCompress[2] = {{ cpp_{reader}, {reader} }};\n" if reader
                  else "constexpr Function kReadBitsCompress[2] = { nullptr, nullptr };\n")
    header.append("constexpr uint32_t kData[] = { " + ", ".join(f"{value:#x}u" for value in data) + " };\n")
    named = re.findall(r"pub const (T_(?:DATA|RET)_\w+): u32 = (0x[0-9A-Fa-f]+)", table)
    header.append("constexpr Datum kNamed[] = {\n" + "".join(f'    {{ "{name}", {value}u }},\n' for name, value in named) + "};\n")
    open(f"{work}/hook_diff_table.h", "w").write("".join(header))

    program = f"{work}/hook_diff.{tag}"
    # SDL comes with sdl3-sys, in the library rustc made.
    libraries = glob.glob(f"{build}/_deps/zlib-build/libz*.a")
    # What neither side's hooks reach -- the rest of the renderer each links in -- is
    # left unresolved, and never called.
    vulkan = ["-lvulkan"] if rust_renderer else []
    run(*compiler, f"-I{work}", "-Wno-everything", *coverage, f"{ROOT}/tools/hook_diff.cpp", *renamed, library, *libraries,
        "-fuse-ld=lld", "-Wl,--gc-sections,--unresolved-symbols=ignore-all", *vulkan, "-lpthread", "-ldl", "-lm", "-lgcc_s", "-o", program)

    hook = [arguments.hook] if arguments.hook else []
    profiles = []
    for seed in range(1, arguments.seeds + 1):
        environment = {**os.environ, **ENVIRONMENTS[seed % len(ENVIRONMENTS)], "HOOK_DIFF_GAME": f"{work}/game"}
        game_folder(f"{work}/game", seed)
        if arguments.coverage:
            profiles.append(f"{work}/{seed}.profraw")
            environment["LLVM_PROFILE_FILE"] = profiles[-1]
        result = subprocess.run([program, str(seed), str(arguments.cases), *hook], env=environment, text=True, errors="replace",
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, preexec_fn=limited,
                                timeout=SECONDS_PER_SEED)
        if result.returncode != 0:
            print(result.stdout, end="")
            sys.exit(f"seed {seed}: exit {result.returncode}")
        if seed == arguments.seeds:
            print(result.stdout, end="")
    print(f"all the same over {arguments.seeds} seeds of {arguments.cases} cases")

    if arguments.coverage:
        merged = f"{work}/merged.profdata"
        run("llvm-profdata-18", "merge", "-o", merged, *profiles)
        for name in ported:
            shown = subprocess.run(["llvm-cov-18", "show", program, f"-instr-profile={merged}", f"{ROOT}/runtime/{sources[name]}"],
                                   check=True, text=True, capture_output=True).stdout
            missed = [line for line in shown.splitlines() if re.match(r"\s*\d+\|\s*0\|", line)]
            print(f"--- {name}: {len(missed)} lines never run")
            for line in missed:
                print(line)

main()
