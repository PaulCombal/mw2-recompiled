#!/usr/bin/env python3
"""Write a metadata-only audit of generated MW3 PPC output and its host archive.

This intentionally reads generated files but never copies generated game code
into the report. The report is suitable for a CI artifact and helps identify
runtime symbols that a future MW3 executable link must provide.
"""
from __future__ import annotations

import collections
import glob
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PPC = ROOT / "ppc_mw3"


def run_tool(tool: str, *args: str) -> str:
    path = shutil.which(tool)
    if not path:
        raise SystemExit(f"required audit tool not found: {tool}")
    result = subprocess.run([path, *args], text=True, capture_output=True)
    if result.returncode:
        raise SystemExit(f"{tool} failed ({result.returncode}): {result.stderr[-2000:]}")
    return result.stdout


def nm_symbols(output: str) -> set[str]:
    found: set[str] = set()
    for line in output.splitlines():
        match = re.match(r"^\s*(?:(?:[0-9A-Fa-f]+)\s+)?([A-Za-z?])\s+(\S+)\s*$", line)
        if match:
            found.add(match.group(2))
    return found


def main() -> int:
    archive = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "build-mw3/libppc_recomp.a"
    report_path = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "build-mw3/mw3-runtime-audit.txt"
    if not archive.is_file():
        raise SystemExit(f"generated PPC archive not found: {archive}")

    sources = sorted(glob.glob(str(PPC / "ppc_recomp.*.cpp")))
    mapping = PPC / "ppc_func_mapping.cpp"
    if not sources or not mapping.is_file():
        raise SystemExit("MW3 generated translation units or ppc_func_mapping.cpp are missing")

    definitions: set[str] = set()
    references: set[str] = set()
    for name in [*sources, str(mapping)]:
        source = Path(name).read_text(errors="replace")
        definitions.update(re.findall(r"PPC_WEAK_FUNC\(([A-Za-z_][A-Za-z0-9_]*)\)", source))
        references.update(re.findall(r"\b([A-Za-z_][A-Za-z0-9_]*)\(ctx, base\)", source))
        references.update(re.findall(r"\{\s*0x[0-9A-Fa-f]+,\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}", source))

    references.discard("PPC_CALL_INDIRECT_FUNC")
    kernel_imports = sorted(n[len("__imp__"):] for n in references - definitions
                            if n.startswith("__imp__"))
    missing_non_imports = sorted(n for n in references - definitions
                                 if not n.startswith("__imp__"))

    defined = nm_symbols(run_tool("nm", "-g", "--defined-only", str(archive)))
    undefined = nm_symbols(run_tool("nm", "-g", "-u", str(archive)))
    external = sorted(undefined - defined)

    relocation_output = run_tool("objdump", "-r", str(archive))
    relocation_rows = re.findall(
        r"^\s*[0-9A-Fa-f]+\s+(R_[A-Za-z0-9_]+)\s+(\S+)", relocation_output, re.M)
    relocation_types = collections.Counter(kind for kind, _ in relocation_rows)
    relocation_targets = sorted({target for _, target in relocation_rows})

    imports_file = PPC / "kernel_imports.txt"
    if imports_file.is_file():
        imports_from_glue = [line.strip() for line in imports_file.read_text().splitlines()
                             if line.strip()]
        if sorted(imports_from_glue) != kernel_imports:
            raise SystemExit("kernel_imports.txt does not match generated-source import scan")
    else:
        imports_from_glue = kernel_imports

    lines = [
        "MW3 GENERATED PPC RUNTIME-INTEGRATION AUDIT",
        f"archive: {archive.name}",
        f"generated PPC translation units: {len(sources)}",
        f"generated PPC function definitions (source scan): {len(definitions)}",
        f"generated function references (source scan): {len(references)}",
        f"kernel/XAM imports: {len(imports_from_glue)}",
        f"referenced non-import symbols without generated definitions: {len(missing_non_imports)}",
        f"archive global definitions: {len(defined)}",
        f"archive global undefined references: {len(undefined)}",
        f"archive external symbols not defined by this archive: {len(external)}",
        f"host-object relocations: {len(relocation_rows)}",
        "",
        "KERNEL/XAM IMPORTS:",
        *(imports_from_glue or ["(none)"]),
        "",
        "UNDEFINED NON-IMPORT GENERATED-CODE SYMBOLS:",
        *(missing_non_imports or ["(none)"]),
        "",
        "ARCHIVE EXTERNAL SYMBOLS REQUIRING LINK-TIME RESOLUTION:",
        *(external or ["(none)"]),
        "",
        "HOST RELOCATION TYPES:",
        *(f"{kind}: {count}" for kind, count in sorted(relocation_types.items())),
        "",
        "HOST RELOCATION TARGETS:",
        *(relocation_targets or ["(none)"]),
        "",
        "LIMITS:",
        "- This is a source/archive symbol and host-object relocation inventory.",
        "- It does not decode or certify Xbox XEX loader relocations.",
        "- It does not establish that runtime callbacks or kernel imports are implemented.",
        "- It does not establish desktop/PS4 link success or game bootability.",
        "- Generated PPC C++ and the proprietary XEX are not included in this report.",
    ]
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:13]))
    print(f"Full metadata report: {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
