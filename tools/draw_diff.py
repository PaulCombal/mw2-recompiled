#!/usr/bin/env python3
# Holds the launcher's drawing in Rust (launcher/src/atlas.rs, draw.rs) to Dear ImGui
# 1.91.9b, which drew its window before (draw_diff.rs, draw_diff.cpp): the atlas of the
# launcher's four fonts at every window scale the launcher can pick (ui::LoadFonts' sizes
# times 0.3 to 1) and some it cannot, at other oversamplings, and of fonts of this machine
# (TrueType and CFF outlines); every glyph of each font; and frames of random lines,
# rectangles, quadrilaterals, circles, gradients, pictures, polylines, polygons and texts
# (the launcher's own, Latin-1, past the atlas, malformed UTF-8, controls, long ones,
# wrapped and not, clipped) and the sizes measured of texts, triangle by triangle, bit for
# bit; and frames drawn by SDL's software renderer through Dear ImGui's SDL renderer
# backend and through launcher/src/present.rs, pixel for pixel. Dear ImGui is fetched at
# its tag (or taken from --imgui) and compiled as the launcher's C++ was (clang++-18 -O3,
# x86-64); the Rust with overflow checks and debug_assert! on.
#
#   usage: draw_diff.py [--build folder] [--frames N] [--src folder] [--truetype folder]
#                       [--imgui folder] [--work folder] [--quick]
#
# --src takes the Rust from another launcher/src, and --truetype the rasteriser (lib.rs)
# from another folder, to try a deliberate mistake. Exits nonzero at the first difference.
import argparse, glob, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONTS = f"{ROOT}/assets/fonts"
MENU, HEADING, BODY = (f"{FONTS}/{n}.ttf" for n in ("BarlowCondensed-Regular", "BarlowCondensed-Medium", "BarlowSemiCondensed-Regular"))
SYSTEM = ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
          "/usr/share/fonts/truetype/freefont/FreeMono.ttf", "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf",
          "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf"]


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
    version = open(f"{imgui}/imgui.h").read()
    if '#define IMGUI_VERSION       "1.91.9b"' not in version:
        sys.exit(f"{imgui} is not Dear ImGui 1.91.9b")
    release = f"{os.path.abspath(arguments.build)}/rust/release"
    sdl = sorted(glob.glob(f"{release}/build/sdl3-sys-*/out/include"))
    sdl_sys = sorted(glob.glob(f"{release}/deps/libsdl3_sys-*.rlib"))
    if not sdl or not sdl_sys:
        sys.exit(f"{arguments.build} has no SDL that sdl3-sys built: build it first (cargo xtask build)")
    objects = []
    for source in ["imgui.cpp", "imgui_draw.cpp", "imgui_tables.cpp", "imgui_widgets.cpp", "backends/imgui_impl_sdlrenderer3.cpp",
                   f"{ROOT}/tools/draw_diff.cpp"]:
        path = source if source.startswith("/") else f"{imgui}/{source}"
        obj = f"{work}/{os.path.basename(source)}.o"
        run(["clang++-18", "-std=gnu++20", "-O3", "-DNDEBUG", "-c", f"-I{imgui}", f"-I{imgui}/backends", f"-I{sdl[0]}", path,
             "-o", obj])
        objects.append(obj)
    library = f"{work}/libdraw_diff_c.a"
    if os.path.exists(library):
        os.remove(library)
    run(["ar", "rcs", library] + objects)
    truetype = f"{work}/libmw2_truetype.rlib"
    checks = ["-Copt-level=3", "-Cdebug-assertions=on", "-Coverflow-checks=on"]
    run(["rustc", "--edition=2024", "--crate-type=rlib", "--crate-name=mw2_truetype", *checks, "-o", truetype,
         f"{os.path.abspath(arguments.truetype)}/lib.rs"], cwd=ROOT)
    source = f"{work}/draw_diff.rs"
    text = open(f"{ROOT}/tools/draw_diff.rs").read().replace('"../launcher/src/', f'"{os.path.abspath(arguments.src)}/')
    open(source, "w").write(text)
    program = f"{work}/draw_diff"
    run(["rustc", "--edition=2024", *checks, "-Cpanic=abort", "-o", program, source, "--extern", f"mw2_truetype={truetype}",
         "--extern", f"sdl3_sys={sdl_sys[0]}", "-L", f"dependency={release}/deps", "-L", work, "-l", "static=draw_diff_c",
         "-l", "stdc++", "-l", "m"], cwd=ROOT)
    return program


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--build", default=f"{ROOT}/build")
    parser.add_argument("--frames", type=int, default=400)
    parser.add_argument("--src", default=f"{ROOT}/launcher/src")
    parser.add_argument("--truetype", default=f"{ROOT}/truetype/src")
    parser.add_argument("--imgui")
    parser.add_argument("--work", default="/tmp/draw_diff")
    parser.add_argument("--quick", action="store_true")
    arguments = parser.parse_args()
    os.makedirs(arguments.work, exist_ok=True)
    program = build(arguments, arguments.work)

    frames = arguments.frames // 4 if arguments.quick else arguments.frames
    cases = []
    # The launcher's: body, menu, heading and small, in ui::LoadFonts' order, at a window
    # scale -- min(1, the desktop over 1320x780) -- rounded as it rounds them.
    scales = [1.0, 0.5, 0.75] if arguments.quick else \
        [1.0, 0.98, 0.95, 0.9, 0.87, 0.85, 0.8, 0.75, 0.7, 0.66, 0.6, 0.55, 0.5, 0.45, 0.4, 0.35, 0.3, 1.25, 1.5, 2.0, 3.0]
    for scale in scales:
        sizes = [float(int(px * scale + (0.5 if px * scale >= 0 else -0.5))) for px in (21, 30, 40, 16)]
        cases.append((3, frames, [(BODY, sizes[0]), (MENU, sizes[1]), (HEADING, sizes[2]), (BODY, sizes[3])]))
    for oversample in (1, 2, 4, 8):
        cases.append((oversample, frames // 4, [(BODY, 21.0), (MENU, 30.0), (HEADING, 40.0), (BODY, 16.0)]))
    system = [font for font in SYSTEM if os.path.exists(font)]
    for i, font in enumerate(system):
        cases.append((3, frames // 4, [(font, 13.0 + 7 * i), (font, 40.0), (BODY, 21.0)]))
    if not system:
        print("no fonts of this machine found to try")

    for seed, (oversample, count, fonts) in enumerate(cases):
        command = [program, str(seed + 1), str(count), str(oversample)]
        for path, size in fonts:
            command += [path, f"{size:g}"]
        result = subprocess.run(command, capture_output=True, text=True, timeout=600)
        name = " ".join(f"{os.path.basename(p)}@{s:g}" for p, s in fonts)
        if result.returncode:
            print(result.stdout + result.stderr)
            sys.exit(f"DIFFERENT with oversample {oversample}, {name}\n  rerun: {' '.join(command)}")
        print(f"oversample {oversample}, {name}: {result.stdout.strip()}")
    print(f"all {len(cases)} cases the same")


main()
