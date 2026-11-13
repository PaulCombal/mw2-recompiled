#!/usr/bin/env python3
"""Emit a sanitized structural report for a missing MW3 indirect-call target.

Reads the private flat PE and generated address map, but reports only addresses,
section/function boundaries, instruction words, and reference locations.
"""
from __future__ import annotations

import argparse
import bisect
import os
import re
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
os.environ["MW2_TITLE"] = "mw3"
os.environ["MW2_VERSION"] = "tu0"
import title  # noqa: E402

MAP_RE = re.compile(r"\{\s*0x([0-9A-Fa-f]+)\s*,\s*([A-Za-z_]\w*)\s*\}")


def signed(value: int, bits: int) -> int:
    sign = 1 << (bits - 1)
    return (value ^ sign) - sign


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--address", type=lambda x: int(x, 0), default=0x823E0968)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()

    image = title.image()
    base = title.BASE
    text_va, text_size = title.text(image)
    pdata = title.functions(image)
    mapping_path = Path(title.PPC) / "ppc_func_mapping.cpp"
    if not mapping_path.is_file():
        raise SystemExit(f"generated mapping is missing: {mapping_path}")
    mapping = {int(a, 16): name for a, name in MAP_RE.findall(mapping_path.read_text(errors="replace"))}
    starts = sorted(mapping)
    addr = args.address
    if not (text_va <= addr < text_va + text_size):
        raise SystemExit(f"target 0x{addr:08X} is outside .text {text_va:08X}..{text_va+text_size:08X}")

    pdata_starts = sorted(pdata)
    pdata_i = bisect.bisect_right(pdata_starts, addr) - 1
    pdata_owner = None
    if pdata_i >= 0:
        s = pdata_starts[pdata_i]
        if s <= addr < s + pdata[s]:
            pdata_owner = (s, pdata[s])

    map_i = bisect.bisect_right(starts, addr) - 1
    preceding = starts[max(0, map_i - 5):map_i + 1]
    following = starts[map_i + 1:map_i + 7]
    raw_lines = []
    for a in range(max(text_va, addr - 0x20), min(text_va + text_size, addr + 0x40), 4):
        off = a - base
        word = struct.unpack_from(">I", image, off)[0]
        suffix = ""
        if (word >> 26) == 18:
            disp = signed(word & 0x03FFFFFC, 26)
            target = disp if (word & 2) else a + disp
            suffix = f" branch->0x{target:08X}" + (" link" if word & 1 else "")
        raw_lines.append(f"  0x{a:08X}: {word:08X}{suffix}")

    branch_refs = []
    for a in range(text_va, text_va + text_size, 4):
        word = struct.unpack_from(">I", image, a - base)[0]
        if (word >> 26) == 18:
            disp = signed(word & 0x03FFFFFC, 26)
            target = disp if (word & 2) else a + disp
            if target == addr:
                branch_refs.append(a)

    ptr_be = struct.pack(">I", addr)
    ptr_le = struct.pack("<I", addr)
    data_refs = []
    for name, (va, size) in title.sections(image).items():
        if name == ".text":
            continue
        start = va - base
        blob = image[start:start + size]
        for needle, endian in ((ptr_be, "BE"), (ptr_le, "LE")):
            pos = 0
            while True:
                pos = blob.find(needle, pos)
                if pos < 0:
                    break
                data_refs.append((va + pos, name, endian))
                pos += 1

    lines = [
        "MW3 INDIRECT-CALL TARGET STRUCTURAL AUDIT",
        f"target: 0x{addr:08X}",
        f".text: 0x{text_va:08X}..0x{text_va+text_size:08X}",
        f"generated mapping entries: {len(mapping)}",
        f"XEX .pdata function entries: {len(pdata)}",
        f"target is an exact generated mapping entry: {'YES' if addr in mapping else 'NO'}",
        f"target is an exact .pdata function entry: {'YES' if addr in pdata else 'NO'}",
        f"target is inside a .pdata-described function: {('0x%08X size 0x%X' % pdata_owner) if pdata_owner else 'NO'}",
        "",
        "NEAREST GENERATED MAPPING ENTRIES BEFORE:",
        *(f"0x{x:08X} {mapping[x]}" for x in preceding),
        "NEAREST GENERATED MAPPING ENTRIES AFTER:",
        *(f"0x{x:08X} {mapping[x]}" for x in following),
        "",
        "INSTRUCTIONS AROUND TARGET (BIG-ENDIAN WORDS; PPC BRANCHES ANNOTATED):",
        *raw_lines,
        "",
        "DIRECT BRANCH REFERENCES TO TARGET:",
        *((f"0x{x:08X}" for x in branch_refs) if branch_refs else ["(none)"]),
        "",
        "NON-TEXT ABSOLUTE POINTER REFERENCES TO TARGET:",
        *((f"0x{x:08X} section={section} endian={endian}" for x, section, endian in data_refs) if data_refs else ["(none)"]),
        "",
        "LIMITS:",
        "- Raw instruction words are not a full disassembly or proof of a function boundary.",
        "- Pointer scans find exact absolute words only; relocated or computed pointers may be missed.",
        "- This report contains no proprietary code bytes beyond individual instruction words.",
    ]
    output = "\n".join(lines) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(output)
    print(output, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
