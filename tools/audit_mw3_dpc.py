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
SIGNED_INT = re.compile(r"^[+-]?(?:0x[0-9A-Fa-f]+|\d+)$")
REG_IMMEDIATE = re.compile(r"ctx\.r(\d+)\.(?:u64|s64|u32|s32|u16|u8)\s*([+-])\s*([+-]?(?:0x[0-9A-Fa-f]+|\d+))")
CALL_TARGET = re.compile(r"\b((?:__imp__)?[A-Za-z_]\w*)\s*\(\s*ctx\s*,\s*base\s*\)")
LOAD = re.compile(r"\bPPC_LOAD_([A-Za-z0-9_]+)\s*\(")
STORE = re.compile(r"\bPPC_STORE_([A-Za-z0-9_]+)\s*\(")


def parse_integer(expr: str) -> int | None:
    expr = expr.strip()
    if not SIGNED_INT.fullmatch(expr):
        return None
    try:
        return int(expr, 0)
    except ValueError:
        return None


def abstract_expr(expr: str, state: dict[int, str],
                  constants_by_register: dict[int, int]) -> str:
    """Summarize expressions and fold simple PPC register-plus-immediate chains."""
    expr = expr.strip()
    literal = parse_integer(expr)
    if literal is not None:
        if expr.lower().startswith("0x"):
            return f"constant 0x{literal & 0xFFFFFFFF:08X}"
        return f"constant {literal} (u32 0x{literal & 0xFFFFFFFF:08X})"
    match = REG_IMMEDIATE.fullmatch(expr)
    if match:
        reg, operator, immediate = match.groups()
        base = constants_by_register.get(int(reg))
        imm = parse_integer(immediate)
        if base is not None and imm is not None:
            value = base + imm if operator == "+" else base - imm
            return f"constant 0x{value & 0xFFFFFFFF:08X} (guest-address candidate)"
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
    constants_by_register: dict[int, int] = {}
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
            rhs = assignment.group(2)
            state[reg] = abstract_expr(rhs, state, constants_by_register)
            literal = parse_integer(rhs)
            if literal is not None:
                constants_by_register[reg] = literal
            else:
                folded = REG_IMMEDIATE.fullmatch(rhs.strip())
                if folded and int(folded.group(1)) in constants_by_register:
                    base = constants_by_register[int(folded.group(1))]
                    imm = parse_integer(folded.group(3))
                    if imm is not None:
                        constants_by_register[reg] = (
                            base + imm if folded.group(2) == "+" else base - imm
                        )
                    else:
                        constants_by_register.pop(reg, None)
                else:
                    constants_by_register.pop(reg, None)
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
    assert init["args"]["r4"].startswith("constant 0x82002000")
    assert init["args"]["r5"] == "constant 0x82003000"
    assert init["nearby_following_stores"] == ["U32"]
    insert = inspect_function("sub_820A95A0", funcs["sub_820A95A0"])[0]
    assert "guest load U32" in insert["args"]["r4"]
    assert insert["args"]["r5"].startswith("constant 7")
    folded_state: dict[int, str] = {}
    folded_constants: dict[int, int] = {}
    assert parse_integer("-2113208320") == -2113208320
    folded_state[11] = abstract_expr("-2113208320", folded_state, folded_constants)
    folded_constants[11] = -2113208320
    callback = abstract_expr("ctx.r11.s64 + -27288", folded_state, folded_constants)
    assert callback.startswith("constant 0x820A9568")
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

    mapping_text = mapping.read_text(errors="replace")
    functions = function_definitions(mapping_text)
    address_to_symbol = {
        int(address, 16): symbol
        for address, symbol in re.findall(
            r"\{\s*0x0*([0-9A-Fa-f]{8})\s*,\s*([A-Za-z_]\w*)\s*\}",
            mapping_text,
        )
    }
    for path in generated:
        functions.update(function_definitions(path.read_text(errors="replace")))

    rows = []
    # Avoid splitting/tokenizing every generated function body. The import
    # callsite regex is cheap; detailed provenance is needed only in the one
    # or two functions that actually invoke a DPC import.
    for name, body in functions.items():
        if CALL.search(body):
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
    for row in rows:
        candidate_match = re.search(r"constant 0x([0-9A-Fa-f]{8})", str(row["args"]["r4"]))
        if row["target"] == "__imp__KeInitializeDpc" and candidate_match:
            address = int(candidate_match.group(1), 16)
            symbol = address_to_symbol.get(address)
            if symbol:
                implementation = symbol if symbol in functions else "__imp__" + symbol
                callback_body = functions.get(implementation)
                if callback_body is not None:
                    row["callback_address"] = f"0x{address:08X}"
                    row["callback_symbol"] = symbol
                    row["callback_calls"] = sorted(set(CALL_TARGET.findall(callback_body)))
                    row["callback_loads"] = sorted(set(LOAD.findall(callback_body)))
                    row["callback_stores"] = sorted(set(STORE.findall(callback_body)))
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
            *( [f"CALLBACK ADDRESS CANDIDATE: {row['callback_address']}",
                 f"CALLBACK MAPPED SYMBOL: {row['callback_symbol']}",
                 f"CALLBACK DIRECT CALL TARGETS: {', '.join(row['callback_calls']) or '(none detected)'}",
                 f"CALLBACK MEMORY ACCESS SHAPES: loads={', '.join(row['callback_loads']) or '(none detected)'}, stores={', '.join(row['callback_stores']) or '(none detected)'}"]
               if "callback_address" in row else [] ),
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
