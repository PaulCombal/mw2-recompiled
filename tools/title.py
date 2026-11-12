#!/usr/bin/env python3
"""Which of the disc's two executables the tools work on.

MW2_TITLE=sp (the default) is default.xex, the campaign and special ops;
MW2_TITLE=mp is default_mp.xex, the multiplayer; MW2_TITLE=mw3 is Modern Warfare
3's campaign. Everything derived from one
of them -- the flat image, the recompiled C++, the switch tables, the build
directory -- lives beside the other's under its own name, so both builds can
exist at once.

MW2_VERSION=tu6 (the default) is the executable title update 6 makes of the
disc's, under the names below. MW2_VERSION=tu0 is the disc's own, whose files
carry the version in their names: mw2/tu0/, ppc_tu0/, config/MW2.tu0.toml.

The image's section table is read here rather than hard-coded, because the
executables lay their sections out differently.
"""
import os, struct

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE = 0x82000000

TITLES = {
    "sp": dict(xex="mw2/default.xex", image="mw2/default.pe", ppc="ppc",
               toml="config/MW2.toml", tables="config/mw2_switch_tables.toml", build="build",
               syms="config/mw2_symbols.txt"),
    "mp": dict(xex="mw2/default_mp.xex", image="mw2/default_mp.pe", ppc="ppc_mp",
               toml="config/MW2MP.toml", tables="config/mw2mp_switch_tables.toml", build="build-mp",
               syms="config/mw2mp_symbols.txt"),
}

# Modern Warfare 3's campaign (iw5sp_ship.exe), the disc's executable
# (docs/MW3-BRINGUP.md). Added after the MW2 version renaming below: MW2_VERSION
# does not apply to it.
MW3 = dict(xex="mw3/tu0/default.xex", image="mw3/tu0/default.pe", ppc="ppc_mw3",
           toml="config/MW3.toml", tables="config/mw3_switch_tables.toml", build="build-mw3",
           syms="config/mw3_symbols.txt")

VERSIONS = ("tu6", "tu0")
VERSION = os.environ.get("MW2_VERSION", "tu6")
if VERSION not in VERSIONS:
    raise SystemExit(f"MW2_VERSION={VERSION!r}: expected one of {', '.join(VERSIONS)}")
if VERSION != "tu6":
    TITLES = {name: dict(xex=t["xex"].replace("mw2/", f"mw2/{VERSION}/"),
                         image=t["image"].replace("mw2/", f"mw2/{VERSION}/"),
                         ppc=f"{t['ppc']}_{VERSION}",
                         toml=t["toml"].replace(".toml", f".{VERSION}.toml"),
                         tables=t["tables"].replace(".toml", f".{VERSION}.toml"),
                         build=f"{t['build']}-{VERSION}",
                         syms=t["syms"].replace(".txt", f".{VERSION}.txt"))
              for name, t in TITLES.items()}

TITLES["mw3"] = MW3

NAME = os.environ.get("MW2_TITLE", "sp")
if NAME not in TITLES:
    raise SystemExit(f"MW2_TITLE={NAME!r}: expected one of {', '.join(TITLES)}")
_t = TITLES[NAME]

XEX = os.path.join(ROOT, _t["xex"])
IMAGE = os.path.join(ROOT, _t["image"])
PPC = os.path.join(ROOT, _t["ppc"])
TOML = os.path.join(ROOT, _t["toml"])
TABLES = os.path.join(ROOT, _t["tables"])
BUILD = os.path.join(ROOT, _t["build"])
SYMBOLS = os.path.join(ROOT, _t["syms"])


def ppc(name):
    """A file in the title's recompiled-output directory."""
    return os.path.join(PPC, name)


_syms = None
_syms_by_name = None


def _load_symbols():
    """{address: name} from the title's symbol map, a hand-maintained sidecar
    that names functions the recompiler only knows as sub_<ADDR>. One entry per
    line, `ADDR  name` (ADDR hex, 0x optional); blank lines and # comments are
    ignored. Missing file -> no names. The recompiled C++ is generated and keyed
    by address, so names live here, not in it."""
    global _syms, _syms_by_name
    if _syms is not None:
        return _syms
    _syms, _syms_by_name = {}, {}
    try:
        lines = open(SYMBOLS).read().splitlines()
    except FileNotFoundError:
        return _syms
    for raw in lines:
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.replace("=", " ").split()
        if len(parts) < 2:
            continue
        try:
            addr = int(parts[0], 16)
        except ValueError:
            continue
        name = parts[1]
        _syms[addr] = name
        _syms_by_name[name] = addr
    return _syms


def symbols():
    """{address: name} for this title (see _load_symbols)."""
    return dict(_load_symbols())


def symname(addr):
    """The name mapped to a guest address, or None."""
    return _load_symbols().get(addr)


def symaddr(name):
    """The guest address of a named symbol, or None."""
    _load_symbols()
    return _syms_by_name.get(name)


def label(addr):
    """`sub_XXXXXXXX` for a guest address, with ` (name)` appended when known --
    the way the tools print a function."""
    name = symname(addr)
    return f"sub_{addr:08X}" + (f" ({name})" if name else "")


def image():
    return open(IMAGE, "rb").read()


def sections(img):
    """{name: (guest address, size)} from the PE section table. The image is
    flat, so a section's file offset equals its address minus BASE."""
    pe = struct.unpack_from("<I", img, 0x3C)[0]
    count = struct.unpack_from("<H", img, pe + 6)[0]
    optional = struct.unpack_from("<H", img, pe + 20)[0]
    out = {}
    for i in range(count):
        o = pe + 24 + optional + i * 40
        name = img[o:o + 8].rstrip(b"\0").decode()
        size, rva = struct.unpack_from("<II", img, o + 8)
        out[name] = (BASE + rva, size)
    return out


def text(img):
    """(guest address, size) of the code section."""
    return sections(img)[".text"]


def functions(img):
    """{start: size} for every function .pdata describes. Leaf functions have
    no entry; the callers are the way to find those."""
    va, size = sections(img)[".pdata"]
    pd = img[va - BASE:va - BASE + size]
    out = {}
    for i in range(len(pd) // 8):
        a, d = struct.unpack_from(">II", pd, i * 8)
        if a == 0:
            break
        out[a] = ((d >> 8) & 0x3FFFFF) * 4
    return out


if __name__ == "__main__":
    img = image()
    t, f = text(img), functions(img)
    print(f"title {NAME}: {IMAGE}")
    print(f"  code {t[0]:08X} +{t[1]:X}, {len(f)} functions in .pdata, recompiled output in {PPC}")
