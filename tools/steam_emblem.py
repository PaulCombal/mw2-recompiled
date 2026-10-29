#!/usr/bin/env python3
"""Make the launcher's Steam emblem from Steam's own icon.

    tools/steam_emblem.py /snap/steam/current/usr/share/icons/hicolor/256x256/apps/steam.png

The icon is a blue disc with the white parts -- two rings and the arm between
them -- some of them cut out of it. Writes launcher/icon/steam_emblem.h: a
64-pixel coverage mask of the parts alone, without the disc, which the
launcher tints (ui.cpp). Needs ffmpeg and ffprobe to read the picture.
"""
import os, subprocess, sys

OUT = 64
if len(sys.argv) != 2:
    raise SystemExit(__doc__)
try:
    size = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height",
                           "-of", "csv=p=0", sys.argv[1]], check=True, capture_output=True, text=True).stdout.strip()
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", sys.argv[1], "-f", "rawvideo", "-pix_fmt", "rgba", "-"],
                         check=True, capture_output=True).stdout
except FileNotFoundError:
    raise SystemExit("ffmpeg and ffprobe are needed to read the icon")
except subprocess.CalledProcessError as error:
    said = error.stderr.decode() if isinstance(error.stderr, bytes) else error.stderr or ""
    raise SystemExit(said.strip() or f"{sys.argv[1]}: not a picture")
if size != "256,256" or len(raw) != 256 * 256 * 4:
    raise SystemExit(f"{sys.argv[1]}: {size.replace(',', 'x')}, the 256-pixel icon is wanted")

# The disc: as far from the middle as the icon is mostly opaque, less its
# smoothed rim. Inside it a see-through pixel is a cut-out part; outside it,
# nothing.
edge = -1 + max(((x + 0.5 - 128) ** 2 + (y + 0.5 - 128) ** 2) ** 0.5
                for y in range(256) for x in range(256) if raw[(y * 256 + x) * 4 + 3] >= 128)
if edge < 64:
    raise SystemExit(f"{sys.argv[1]}: no disc found")

def coverage(x, y):
    """How much of the pixel is the white parts."""
    r, g, b, a = raw[(y * 256 + x) * 4:(y * 256 + x) * 4 + 4]
    inside = (x + 0.5 - 128) ** 2 + (y + 0.5 - 128) ** 2 < edge ** 2
    white = 1 - a / 255 * (1 - min(r, g, b) / 255) if inside else 0.0   # as it looks on white
    white = min(1.0, max(0.0, (white - 0.15) / 0.7))   # the disc's blue is not quite black
    return white

step = 256 // OUT
parts = []
for y in range(OUT):
    for x in range(OUT):
        p = sum(coverage(x * step + i, y * step + j) for j in range(step) for i in range(step))
        parts.append(round(255 * p / step / step))

def array(name, values):
    rows = [", ".join(str(v) for v in values[i:i + OUT]) for i in range(0, len(values), OUT)]
    return f"    constexpr unsigned char {name}[] = {{\n" + ",\n".join("        " + r for r in rows) + "\n    };\n"

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "launcher", "icon", "steam_emblem.h")
with open(path, "w") as out:
    out.write("#pragma once\n// Made by tools/steam_emblem.py from Steam's 256-pixel icon: coverage, 0 to 255,\n"
              "// of its white parts without the disc.\n"
              f"namespace steam_emblem\n{{\n    constexpr int kSize = {OUT};\n")
    out.write(array("kParts", parts))
    out.write("}\n")
print(path)
