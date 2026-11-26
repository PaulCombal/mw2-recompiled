#!/usr/bin/env python3
# Holds the launcher's window in Rust (launcher/src/ui.rs, preview.rs, input.rs) to the C++
# it replaced -- launcher/ui.cpp and preview.cpp, compiled from git as they were at a
# commit before (--old), drawing with Dear ImGui 1.91.9b and reading the keyboard and the
# pointer through its SDL3 backend (ui_diff.rs, ui_diff.cpp): screens of every kind at
# every window scale the launcher can pick (0.3 to 1) and some it cannot, as triangles,
# bit for bit, and as pixels, the backdrop under them, on SDL's software renderer; and
# SDL's events, as what the window reads of them each frame. Dear ImGui is fetched at its
# tag (or taken from --imgui) and compiled as the launcher's C++ was (clang++-18 -O3,
# x86-64); the Rust with overflow checks and debug_assert! on.
#
#   usage: ui_diff.py [--build folder] [--old commit] [--frames N] [--src folder]
#                     [--imgui folder] [--work folder] [--quick]
#
# --src takes the Rust from another launcher/src, to try a deliberate mistake. Exits
# nonzero at the first difference.
import argparse, glob, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OLD_FILES = ["launcher/ui.cpp", "launcher/ui.h", "launcher/preview.cpp", "launcher/preview.h", "launcher/fonts.h",
             "launcher/icon/steam_emblem.h"]
FONTS = [("Menu", "BarlowCondensed-Regular"), ("Heading", "BarlowCondensed-Medium"), ("Body", "BarlowSemiCondensed-Regular")]


def run(command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def imgui_source(arguments, work):
    if arguments.imgui:
        return os.path.abspath(arguments.imgui)
    folder = f"{work}/imgui"
    if not os.path.exists(f"{folder}/imgui.h"):
        run(["git", "clone", "--quiet", "--depth", "1", "--branch", "v1.91.9b", "https://github.com/ocornut/imgui.git", folder])
    return folder


def build(arguments, work):
    imgui = imgui_source(arguments, work)
    if '#define IMGUI_VERSION       "1.91.9b"' not in open(f"{imgui}/imgui.h").read():
        sys.exit(f"{imgui} is not Dear ImGui 1.91.9b")
    release = f"{os.path.abspath(arguments.build)}/rust/release"
    sdl = sorted(glob.glob(f"{release}/build/sdl3-sys-*/out/include"))
    sdl_sys = sorted(glob.glob(f"{release}/deps/libsdl3_sys-*.rlib"))
    if not sdl or not sdl_sys:
        sys.exit(f"{arguments.build} has no SDL that sdl3-sys built: build it first (cargo xtask build)")
    old = f"{work}/old"
    for name in OLD_FILES:
        os.makedirs(os.path.dirname(f"{old}/{name}"), exist_ok=True)
        text = subprocess.run(["git", "-C", ROOT, "show", f"{arguments.old}:{name}"], capture_output=True, check=True).stdout
        open(f"{old}/{name}", "wb").write(text)
    # The fonts as the C arrays CMake wrote them as.
    with open(f"{work}/fonts.cpp", "w") as out:
        for name, file in FONTS:
            data = open(f"{ROOT}/assets/fonts/{file}.ttf", "rb").read()
            out.write(f"extern const unsigned char kFont{name}[] = {{ {','.join(str(b) for b in data)} }};\n")
            out.write(f"extern const unsigned kFont{name}Size = {len(data)};\n")
    objects = []
    sources = ["imgui.cpp", "imgui_draw.cpp", "imgui_tables.cpp", "imgui_widgets.cpp", "backends/imgui_impl_sdl3.cpp",
               "backends/imgui_impl_sdlrenderer3.cpp", f"{old}/launcher/ui.cpp", f"{old}/launcher/preview.cpp",
               f"{work}/fonts.cpp", f"{ROOT}/tools/ui_diff.cpp"]
    for source in sources:
        path = source if source.startswith("/") else f"{imgui}/{source}"
        obj = f"{work}/{os.path.basename(source)}.o"
        run(["clang++-18", "-std=gnu++20", "-O3", "-DNDEBUG", "-c", f"-I{imgui}", f"-I{imgui}/backends",
             f"-I{old}/launcher", f"-I{sdl[0]}", path, "-o", obj])
        objects.append(obj)
    library = f"{work}/libui_diff_c.a"
    if os.path.exists(library):
        os.remove(library)
    run(["ar", "rcs", library] + objects)
    truetype = f"{work}/libmw2_truetype.rlib"
    checks = ["-Copt-level=3", "-Cdebug-assertions=on", "-Coverflow-checks=on"]
    run(["rustc", "--edition=2024", "--crate-type=rlib", "--crate-name=mw2_truetype", *checks, "-o", truetype,
         f"{ROOT}/truetype/src/lib.rs"], cwd=ROOT)
    source = f"{work}/ui_diff.rs"
    text = open(f"{ROOT}/tools/ui_diff.rs").read().replace('"../launcher/src/', f'"{os.path.abspath(arguments.src)}/')
    open(source, "w").write(text)
    program = f"{work}/ui_diff"
    run(["rustc", "--edition=2024", *checks, "-Cpanic=abort", "-o", program, source, "--extern", f"mw2_truetype={truetype}",
         "--extern", f"sdl3_sys={sdl_sys[0]}", "-L", f"dependency={release}/deps", "-L", work, "-l", "static=ui_diff_c",
         "-l", "stdc++", "-l", "m"], cwd=ROOT)
    return program


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--build", default=f"{ROOT}/build")
    parser.add_argument("--old", default="8f5c04e")
    parser.add_argument("--frames", type=int, default=3000)
    parser.add_argument("--src", default=f"{ROOT}/launcher/src")
    parser.add_argument("--imgui")
    parser.add_argument("--work", default="/tmp/ui_diff")
    parser.add_argument("--quick", action="store_true")
    arguments = parser.parse_args()
    os.makedirs(arguments.work, exist_ok=True)
    program = build(arguments, arguments.work)

    frames = arguments.frames // 10 if arguments.quick else arguments.frames
    # The launcher's scales, min(1, the desktop over 1320x780), and some it cannot pick.
    scales = [1.0, 0.75, 0.5] if arguments.quick else \
        [1.0, 0.98, 0.95, 0.9, 0.87, 0.85, 0.8, 0.75, 0.7, 0.66, 0.6, 0.55, 0.5, 0.45, 0.4, 0.35, 0.3, 1.25, 1.5, 2.0]
    cases = [["draw", str(seed + 1), str(frames), f"{scale:g}"] for seed, scale in enumerate(scales)]
    cases += [["input", str(100 + seed), str(frames * 3)] for seed in range(2 if arguments.quick else 8)]
    for case in cases:
        result = subprocess.run([program, *case], capture_output=True, text=True, timeout=1800)
        if result.returncode:
            print(result.stdout + result.stderr)
            sys.exit(f"DIFFERENT: {' '.join(case)}\n  rerun: {program} {' '.join(case)}")
        print(f"{' '.join(case)}: {result.stdout.strip()}")
    print(f"all {len(cases)} cases the same")


main()
