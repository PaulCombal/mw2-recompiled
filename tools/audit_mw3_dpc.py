#!/usr/bin/env python3
"""Emit a sanitized static callsite/provenance report for MW3 DPC imports.

The report intentionally contains no generated PPC statements or XEX bytes.
It records only abstracted register provenance, guest constants, load/store
shapes, and direct call target names. All findings are static, not runtime traces.
"""
from __future__ import annotations

import argparse
import re
from pathlib import Path

from audit_mw3_startup import function_definitions

ROOT = Path(__file__).resolve().parents[1]
PPC = ROOT / "ppc_mw3"
TARGETS = ("__imp__KeInitializeDpc", "__imp__KeInsertQueueDpc")
CALL = re.compile(r"\b(__imp__Ke(?:InitializeDpc|InsertQueueDpc))\s*\(\s*ctx\s*,\s*base\s*\)")
ASSIGN = re.compile(r"\bctx\.r(\d+)\.(?:u64|s64|u32|s32|u16|u8)\s*=\s*(.*)")
REG = re.compile(r"\bctx\.r(\d+)\.(?:u64|s64|u32|s32|u16|u8)\b")
HEX = re.compile(r"0x[0-9A-Fa-f]+")
CALL_TARGET = re.compile(r"\b((?:__imp__)?[A-Za-z_]\w*)\s*\(\s*ctx\s*,\s*base\s*\)")
LOAD = re.compile(r"\bPPC_LOAD_([A-Za-z0-9_]+)\s*\(")
STORE = re.compile(r"\bPPC_STORE_([A-Za-z0-9_]+)\s*\(")


def abstract_expr(expr: str, state: dict[int, str], depth: int = 0) -> str:
    """Summarize an expression without emitting generated source text."""
    expr = expr.strip()
    if depth > 3:
        return "expression (provenance depth limit)"
    if re.fullmatch(r"(?:0x[0-9A-Fa-f]+|\d+)", expr):
        return f"constant {expr}"
    loads = LOAD.findall(expr)
    stores = STORE.findall(expr)
    targets = [m.group(1) for m in CALL_TARGET.finditer(expr)]
    regs = sorted({int(n) for n in REG.findall(expr)})
    constants = HEX.findall(expr)
    if targets:
        return "call result: " + ", ".join(targets)
    if loads:
        shape = "guest load " + "/".join(sorted(set(loads)))
        if regs:
            shape += "; base/source " + ", ".join(
                f"r{n} ({state.get(n, 'unknown')})" for n in regs
            )
        if constants:
            shape += "; displacement/constant " + ", ".join(constants)
        return shape
    if stores:
        return "guest store " + "/".join(sorted(set(stores)))
    if len(regs) == 1 and re.fullmatch(
        r"ctx\.r\d+\.(?:u64|s64|u32|s32|u16|u8)", expr
    ):
        n = regs[0]
        return f"r{n} ({state.get(n, 'unknown')})"
    if regs:
        refs = ", ".join(f"r{n} ({state.get(n, 'unknown')})" for n in regs)
        suffix = "; constants " + ", ".join(constants) if constants else ""
        return f"computed from {refs}{suffix}"
    if constants:
        return "expression with constants " + ", ".join(constants)
    return "computed expression (details suppressed)"


def split_statements(body: str) -> list[str]:
    # Generated statements are semicolon-terminated. Keep only the small
    # statement-level abstraction needed for register provenance.
    return [part.strip() for part in body.split(";") if part.strip()]


def inspect_function(name: str, body: str) -> list[dict[str, object]]:
    statements = split_statements(body)
    state: dict[int, str] = {}
    rows: list[dict[str, object]] = []
    for index, statement in enumerate(statements):
        call = CALL.search(statement)
        if call:
            target = call.group(1)
            args = {f"r{n}": state.get(n, "unknown at callsite") for n in (3, 4, 5)}
            calls = sorted(set(CALL_TARGET.findall(body)))
            before_loads = sorted(set(LOAD.findall(" ".join(statements[max(0,index-8):index]))))
            after = " ".join(statements[index+1:index+9])
            after_loads = sorted(set(LOAD.findall(after)))
            after_stores = sorted(set(STORE.findall(after)))
            rows.append({
                "function": name, "target": target, "args": args,
                "nearby_calls": calls, "nearby_prior_loads": before_loads,
                "nearby_following_loads": after_loads,
                "nearby_following_stores": after_stores,
            })
            continue
        assignment = ASSIGN.search(statement)
        if assignment:
            reg = int(assignment.group(1))
            state[reg] = abstract_expr(assignment.group(2), state)
    return rows


def self_test() -> None:
    fixture = """
      PPC_FUNC_IMPL(sub_820A9670) {
        ctx.r31.u64 = 0x82001000;
        ctx.r3.u64 = ctx.r31.u64;
        ctx.r4.u64 = 0x82002000;
        ctx.r5.u64 = 0x82003000;
        __imp__KeInitializeDpc(ctx, base);
        PPC_STORE_U32(ctx.r31.u32 + 0x18, ctx.r4.u32);
        sub_820A1234(ctx, base);
      }
      PPC_FUNC_IMPL(sub_820A95A0) {
        ctx.r3.u64 = 0x82001000;
        ctx.r4.u64 = PPC_LOAD_U32(ctx.r3.u32 + 0x10);
        ctx.r5.u64 = 7;
        __imp__KeInsertQueueDpc(ctx, base);
      }
    """
    funcs = function_definitions(fixture)
    init = inspect_function("sub_820A9670", funcs["sub_820A9670"])[0]
    assert init["args"]["r3"] == "r31 (constant 0x82001000)"
    assert init["args"]["r4"] == "constant 0x82002000"
    assert init["args"]["r5"] == "constant 0x82003000"
    assert init["nearby_following_stores"] == ["U32"]
    insert = inspect_function("sub_820A95A0", funcs["sub_820A95A0"])[0]
    assert "guest load U32" in insert["args"]["r4"]
    assert insert["args"]["r5"] == "constant 7"
    print("DPC callsite provenance self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--report", type=Path,
                        default=ROOT / "build-mw3/mw3-dpc-callsites.txt")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0

    generated = sorted(PPC.glob("ppc_recomp.*.cpp"))
    mapping = PPC / "ppc_func_mapping.cpp"
    if not generated or not mapping.is_file():
        raise SystemExit("generated MW3 PPC sources or mapping are missing")

    functions = function_definitions(mapping.read_text(errors="replace"))
    for path in generated:
        functions.update(function_definitions(path.read_text(errors="replace")))

    rows = []
    for name, body in functions.items():
        rows.extend(inspect_function(name, body))

    expected_targets = {
        "__imp__KeInitializeDpc": 1,
        "__imp__KeInsertQueueDpc": 1,
    }
    for target, expected_count in expected_targets.items():
        found = [row for row in rows if row["target"] == target]
        if len(found) != expected_count:
            callers = sorted({str(row["function"]) for row in found})
            raise SystemExit(
                f"expected {expected_count} {target} callsite(s), found {len(found)} "
                f"in callers: {callers or '(none)'}"
            )
    rows.sort(key=lambda row: (str(row["target"]), str(row["function"])))

    lines = [
        "MW3 SANITIZED DPC CALLSITE / ARGUMENT-PROVENANCE AUDIT",
        "Static generated-code analysis only; not a runtime execution trace.",
        "Generated PPC statements and proprietary XEX contents are intentionally omitted.",
        "",
    ]
    for row in rows:
        lines.extend([
            f"CALLER: {row['function']}",
            f"IMPORT: {row['target']}",
            "ARGUMENT REGISTER SNAPSHOT (Xbox PPC ABI: r3-r5):",
            *(f"  {reg}: {value}" for reg, value in row["args"].items()),
            "DIRECT CALL TARGET NAMES IN CALLER:",
            *(f"  {target}" for target in row["nearby_calls"]),
            "NEAR-CALL MEMORY ACCESS SHAPES:",
            f"  prior loads: {', '.join(row['nearby_prior_loads']) or '(none detected)'}",
            f"  following loads: {', '.join(row['nearby_following_loads']) or '(none detected in bounded window)'}",
            f"  following stores: {', '.join(row['nearby_following_stores']) or '(none detected in bounded window)'}",
            "",
        ])
    lines.extend([
        "INTERPRETATION LIMITS:",
        "- Register snapshots are last textual assignments in the caller body; control-flow joins and path conditions are not modeled.",
        "- Register expressions are abstracted; this report is not a decompilation listing.",
        "- A constant in r4 at KeInitializeDpc is a callback-address candidate, not proof the target is a valid generated function.",
        "- Loads/stores are classified by width and proximity only; exact guest field offsets and aliasing require instruction-level correlation.",
        "- Direct call target names do not include indirect calls through function pointers.",
        "- No conclusion about audio, graphics, synchronization, or runtime callback behavior is made from proximity alone.",
    ])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:28]))
    print(f"Full sanitized DPC report: {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
