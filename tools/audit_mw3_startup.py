#!/usr/bin/env python3
"""Metadata-only direct-call reachability audit from the MW3 XEX entry point.

The generated PPC C++ and XEX are inputs only. The report contains function and
import names/counts, never source bodies or executable bytes.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import deque
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PPC = ROOT / "ppc_mw3"
ENTRY_POINT = 0x82459E58
ENTRY_SYMBOL = "_xstart"

# XenonRecomp emits implementation bodies and thin callable wrappers. Imports
# supplied by the kernel runtime use PPC_FUNC; generated functions use both
# PPC_FUNC_IMPL and PPC_WEAK_FUNC.
DEFINITION = re.compile(
    r"\b(PPC_FUNC_IMPL|PPC_FUNC|PPC_WEAK_FUNC)\s*\(\s*([A-Za-z_]\w*)\s*\)\s*\{"
)
CALL = re.compile(r"\b([A-Za-z_]\w*)\s*\(\s*ctx\s*,\s*base\s*\)\s*;")


def function_definitions(source: str) -> dict[str, str]:
    """Return macro-defined function bodies using brace-balanced extraction."""
    found: dict[str, str] = {}
    for match in DEFINITION.finditer(source):
        depth = 1
        pos = match.end()
        start = pos
        while pos < len(source) and depth:
            char = source[pos]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
            pos += 1
        if depth == 0:
            found[match.group(2)] = source[start:pos - 1]
    return found


def parse_mapping(mapping_text: str) -> str:
    pattern = rf"\{{\s*0x0*{ENTRY_POINT:08X}\s*,\s*([A-Za-z_]\w*)\s*\}}"
    match = re.search(pattern, mapping_text, re.IGNORECASE)
    if not match:
        raise ValueError(f"entry point 0x{ENTRY_POINT:08X} has no function mapping")
    return match.group(1)


def reachable_graph(functions: dict[str, str], root: str,
                    imports: set[str]) -> tuple[set[str], set[str], set[str], dict[str, set[str]]]:
    """Walk generated direct calls; stop at imports and retain call adjacency."""
    if root not in functions:
        raise ValueError(f"mapped entry wrapper {root!r} has no generated definition")

    adjacency = {
        name: set(CALL.findall(body))
        for name, body in functions.items()
    }
    visited: set[str] = set()
    reached_imports: set[str] = set()
    unresolved: set[str] = set()
    queue = deque([root])

    while queue:
        name = queue.popleft()
        if name in visited:
            continue
        visited.add(name)
        for target in adjacency.get(name, set()):
            if target in imports:
                reached_imports.add(target)
            elif target in functions:
                if target not in visited:
                    queue.append(target)
            else:
                unresolved.add(target)

    return visited, reached_imports, unresolved, adjacency


def self_test() -> None:
    sample = """
    PPC_WEAK_FUNC(_xstart) { __imp___xstart(ctx, base); }
    PPC_FUNC_IMPL(__imp___xstart) {
      sub_100(ctx, base);
      __imp__NtCreateFile(ctx, base);
    }
    PPC_WEAK_FUNC(sub_100) { __imp__sub_100(ctx, base); }
    PPC_FUNC_IMPL(__imp__sub_100) {
      __imp__KeWaitForSingleObject(ctx, base);
      sub_200(ctx, base);
    }
    PPC_WEAK_FUNC(sub_200) { __imp__sub_200(ctx, base); }
    PPC_FUNC_IMPL(__imp__sub_200) { }
    """
    functions = function_definitions(sample)
    imports = {"__imp__NtCreateFile", "__imp__KeWaitForSingleObject"}
    visited, reached, unresolved, _ = reachable_graph(functions, "_xstart", imports)
    assert reached == imports, reached
    assert {"_xstart", "__imp___xstart", "sub_100", "__imp__sub_100",
            "sub_200", "__imp__sub_200"} <= visited
    assert not unresolved, unresolved
    assert parse_mapping("{{ 0x82459E58, _xstart }},") == "_xstart"
    print("startup call-graph self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--report", type=Path, default=ROOT / "build-mw3/mw3-startup-callgraph.txt")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0

    generated = sorted(PPC.glob("ppc_recomp.*.cpp"))
    mapping = PPC / "ppc_func_mapping.cpp"
    imports_file = PPC / "kernel_imports.txt"
    if not generated or not mapping.is_file() or not imports_file.is_file():
        raise SystemExit("generated MW3 PPC sources, mapping, or kernel import list is missing")

    root = parse_mapping(mapping.read_text(errors="replace"))
    if root != ENTRY_SYMBOL:
        raise SystemExit(f"entry point maps to {root!r}, expected XenonRecomp's {ENTRY_SYMBOL!r}")

    functions: dict[str, str] = {}
    for path in generated:
        functions.update(function_definitions(path.read_text(errors="replace")))

    imports = {
        line.strip() if line.strip().startswith("__imp__") else "__imp__" + line.strip()
        for line in imports_file.read_text().splitlines() if line.strip()
    }
    if not imports:
        raise SystemExit("MW3 kernel import list is empty")

    visited, reached, unresolved, adjacency = reachable_graph(functions, root, imports)

    # Direct imports are those called by the entry wrapper or its implementation.
    direct = set(adjacency.get(root, set())) & reached
    direct |= set(adjacency.get(f"__imp__{root}", set())) & reached

    runtime_implemented: set[str] = set()
    for path in sorted((ROOT / "runtime/kernel").glob("*.cpp")):
        if path.name == "stubs_generated.cpp":
            continue
        runtime_implemented.update(re.findall(
            r"\bPPC_FUNC\s*\(\s*(__imp__[A-Za-z0-9_]+)\s*\)", 
            path.read_text(errors="replace")))

    runtime_imports = reached & runtime_implemented
    logging_stub_imports = reached - runtime_implemented
    non_import_nodes = visited - reached
    generated_reachable = {
        name for name in non_import_nodes
        if name in functions and not name.startswith("__imp__")
    }

    lines = [
        "MW3 STARTUP DIRECT-CALL REACHABILITY AUDIT",
        f"entry point: 0x{ENTRY_POINT:08X}",
        f"entry mapping symbol: {root}",
        f"generated function bodies indexed: {len(functions)}",
        f"generated wrapper/implementation nodes reachable: {len(visited)}",
        f"reachable generated wrapper nodes: {len(generated_reachable)}",
        f"kernel/XAM imports in full image: {len(imports)}",
        f"kernel/XAM imports reachable from entry point: {len(reached)}",
        f"reachable imports called directly by entry wrapper/implementation: {len(direct)}",
        f"reachable imports matching existing runtime PPC_FUNC definitions: {len(runtime_imports)}",
        f"reachable imports currently covered only by generated logging stubs: {len(logging_stub_imports)}",
        f"unresolved non-import direct-call targets: {len(unresolved)}",
        "",
        "DIRECT ENTRY-POINT IMPORTS:",
        *(sorted(direct) or ["(none)"]),
        "",
        "ALL TRANSITIVELY REACHABLE KERNEL/XAM IMPORTS:",
        *(sorted(reached) or ["(none)"]),
        "",
        "REACHABLE IMPORTS WITH EXISTING RUNTIME DEFINITIONS (MACRO MATCH ONLY):",
        *(sorted(runtime_imports) or ["(none)"]),
        "",
        "REACHABLE IMPORTS STILL COVERED ONLY BY LOGGING STUBS:",
        *(sorted(logging_stub_imports) or ["(none)"]),
        "",
        "UNRESOLVED NON-IMPORT CALL TARGETS:",
        *(sorted(unresolved) or ["(none)"]),
        "",
        "LIMITS:",
        "- This is a static direct-call graph over XenonRecomp-emitted function(ctx, base) calls.",
        "- Indirect calls, function pointers, callbacks, data imports, and dynamically selected thread entry points are not resolved.",
        "- Reachability is not execution order: a reachable call may be conditional or occur only on an error path.",
        "- Runtime import classification is a PPC_FUNC macro-name match, not proof of MW3 semantic compatibility.",
        "- Logging stubs are diagnostic placeholders and do not implement kernel behavior.",
        "- The report contains metadata only; generated PPC source and the proprietary XEX are not included.",
    ]
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:13]))
    print(f"Full startup call-graph report: {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
