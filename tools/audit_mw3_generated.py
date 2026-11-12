#!/usr/bin/env python3
"""Write a metadata-only audit of generated MW3 PPC output and runtime needs.

Reads generated code and the PPC archive but writes only symbol names/counts;
it never copies generated game code or the proprietary XEX into the report.
"""
from __future__ import annotations

import collections
import glob
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PPC = ROOT / "ppc_mw3"
MW3_ENTRY_POINT = 0x82459E58  # Verified from the supported MW3 XEX header/CI analysis.


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


def demangle(symbols: list[str]) -> dict[str, str]:
    if not symbols:
        return {}
    path = shutil.which("c++filt")
    if not path:
        raise SystemExit("required audit tool not found: c++filt")
    result = subprocess.run([path], input="\n".join(symbols) + "\n",
                            text=True, capture_output=True)
    if result.returncode:
        raise SystemExit(f"c++filt failed: {result.stderr[-2000:]}")
    decoded = result.stdout.splitlines()
    if len(decoded) != len(symbols):
        raise SystemExit("c++filt returned an unexpected number of symbols")
    return dict(zip(symbols, decoded))


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

    # Mirror gen_kernel_stubs.py's implementation test exactly. A missing import
    # gets a logging stub today; it is not a verified implementation.
    runtime_sources = sorted(glob.glob(str(ROOT / "runtime/kernel/*.cpp")))
    runtime_implemented: set[str] = set()
    for name in runtime_sources:
        if name.endswith("stubs_generated.cpp"):
            continue
        source = Path(name).read_text(errors="replace")
        runtime_implemented.update(re.findall(
            r"PPC_FUNC\(__imp__([A-Za-z0-9_]+)\)", source))
    implemented_imports = sorted(set(kernel_imports) & runtime_implemented)
    stubbed_imports = sorted(set(kernel_imports) - runtime_implemented)
    extra_runtime_imports = sorted(runtime_implemented - set(kernel_imports))

    mapping_text = mapping.read_text(errors="replace")
    entry_hex = f"0x{MW3_ENTRY_POINT:08X}"
    entrypoint_mapped = bool(re.search(
        rf"\b0x0*{MW3_ENTRY_POINT:08X}\b", mapping_text, re.IGNORECASE))

    defined = nm_symbols(run_tool("nm", "-g", "--defined-only", str(archive)))
    undefined = nm_symbols(run_tool("nm", "-g", "-u", str(archive)))
    external = sorted(undefined - defined)
    decoded_external = demangle(external)
    import_external = sorted(
        raw for raw, name in decoded_external.items() if name.startswith("__imp__"))
    host_external = sorted(
        (decoded_external[raw], raw) for raw in external
        if not decoded_external[raw].startswith("__imp__"))
    archive_import_names = sorted({
        match.group(1)
        for raw in import_external
        if (match := re.match(r"__imp__([A-Za-z0-9_]+)\(", decoded_external[raw]))
    })

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
        f"MW3 entry point: {entry_hex}",
        f"MW3 entry point present in ppc_func_mapping.cpp: {'YES' if entrypoint_mapped else 'NO'}",
        f"kernel/XAM imports: {len(imports_from_glue)}",
        f"imports implemented by existing runtime/kernel sources: {len(implemented_imports)}",
        f"imports requiring generated logging stubs: {len(stubbed_imports)}",
        f"extra runtime imports not used by MW3: {len(extra_runtime_imports)}",
        f"referenced non-import symbols without generated definitions: {len(missing_non_imports)}",
        f"archive global definitions: {len(defined)}",
        f"archive global undefined references: {len(undefined)}",
        f"archive external symbols not defined by this archive: {len(external)}",
        f"archive unresolved kernel-import wrappers: {len(import_external)}",
        f"archive unresolved non-kernel host symbols: {len(host_external)}",
        f"host-object relocations: {len(relocation_rows)}",
        "",
        "MW3 KERNEL/XAM IMPORTS IMPLEMENTED BY EXISTING RUNTIME:",
        *(implemented_imports or ["(none)"]),
        "",
        "MW3 KERNEL/XAM IMPORTS CURRENTLY COVERED ONLY BY GENERATED LOGGING STUBS:",
        *(stubbed_imports or ["(none)"]),
        "",
        "EXTRA RUNTIME IMPORT IMPLEMENTATIONS NOT REFERENCED BY MW3:",
        *(extra_runtime_imports or ["(none)"]),
        "",
        "UNDEFINED NON-IMPORT GENERATED-CODE SYMBOLS:",
        *(missing_non_imports or ["(none)"]),
        "",
        "ARCHIVE UNRESOLVED KERNEL-IMPORT WRAPPERS:",
        *(decoded_external[n] for n in import_external),
        "",
        "ARCHIVE UNRESOLVED NON-KERNEL HOST SYMBOLS:",
        *(f"{name} [{raw}]" for name, raw in host_external),
        "",
        "HOST RELOCATION TYPES (RUNNER OBJECT FORMAT; NOT XEX RELOCATIONS):",
        *(f"{kind}: {count}" for kind, count in sorted(relocation_types.items())),
        "",
        "HOST RELOCATION TARGETS:",
        *(relocation_targets or ["(none)"]),
        "",
        "LIMITS:",
        "- Entry-point mapping is a text match in the generated mapping source, not a runtime boot test.",
        "- Existing runtime import implementations are matched by the same macro scan as gen_kernel_stubs.py; semantic correctness is not established.",
        "- Generated logging stubs return zero and are diagnostic placeholders, not working kernel services.",
        "- Host-object relocations do not decode or certify Xbox XEX loader relocations.",
        "- This does not establish desktop/PS4 link success or game bootability.",
        "- Generated PPC C++ and the proprietary XEX are not included in this report.",
    ]
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:19]))
    print(f"Full metadata report: {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
