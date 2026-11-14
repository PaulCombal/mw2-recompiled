#!/usr/bin/env python3
"""Sanitized static check for MW3's XEX module-handle startup access.

The generated PPC source is private build output. This tool reads it locally and
prints only pass/fail facts; it never copies or emits generated source.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys


FUNCTION = "__imp__sub_82462E50"


def function_body(source: str) -> str | None:
    marker = f"PPC_FUNC_IMPL({FUNCTION})"
    start = source.find(marker)
    if start < 0:
        return None
    brace = source.find("{", start + len(marker))
    if brace < 0:
        return None
    depth = 0
    for index in range(brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[brace + 1:index]
    return None


def audit_body(body: str) -> tuple[bool, str]:
    # These are the translated forms of:
    #   lis r11, -32256       ; 0x82000000
    #   lwz r11, 2172(r11)    ; XexExecutableModuleHandle global at +0x87c
    #   lwz r11, 0(r11)       ; dereference global pointer slot
    #   lwz r3, 88(r11)       ; LDR_DATA_TABLE_ENTRY.xex_header_base (+0x58)
    #   bl RtlImageXexHeaderField
    patterns = [
        r"ctx[.]r11[.]s64[ \t]*=[ \t]*-2113929216[ \t]*;",
        r"PPC_LOAD_U32[(]ctx[.]r11[.]u32[ \t]*[+][ \t]*2172[)]",
        r"PPC_LOAD_U32[(]ctx[.]r11[.]u32[ \t]*[+][ \t]*0[)]",
        r"ctx[.]r3[.]u64[ \t]*=[ \t]*PPC_LOAD_U32[(]ctx[.]r11[.]u32[ \t]*[+][ \t]*88[)]",
        r"__imp__RtlImageXexHeaderField[(]ctx,[ \t]*base[)][ \t]*;",
    ]
    positions = []
    for pattern in patterns:
        match = re.search(pattern, body)
        if not match:
            return False, "required module-handle access pattern missing"
        positions.append(match.start())
    if positions != sorted(positions):
        return False, "module-handle access order differs from expected pointer chain"
    return True, "verified global -> loader-entry -> XEX-header (+0x58) -> RTL call"


def self_test() -> bool:
    sample = """
    PPC_FUNC_IMPL(__imp__sub_82462E50) {
      ctx.r11.s64 = -2113929216;
      ctx.r11.u64 = PPC_LOAD_U32(ctx.r11.u32 + 2172);
      ctx.r11.u64 = PPC_LOAD_U32(ctx.r11.u32 + 0);
      ctx.r3.u64 = PPC_LOAD_U32(ctx.r11.u32 + 88);
      __imp__RtlImageXexHeaderField(ctx, base);
    }
    """
    body = function_body(sample)
    if body is None or not audit_body(body)[0]:
        return False
    altered = sample.replace("ctx.r11.u32 + 88", "ctx.r11.u32 + 84")
    body = function_body(altered)
    return body is not None and not audit_body(body)[0]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=Path("ppc_mw3"))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        if not self_test():
            print("MW3 XEX module-handle audit self-test failed", file=sys.stderr)
            return 1
        print("MW3 XEX module-handle audit self-test passed")
        return 0

    found = None
    for path in sorted(args.source_dir.glob("ppc_recomp.*.cpp")):
        try:
            body = function_body(path.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue
        if body is not None:
            if found is not None:
                print("MW3 XEX module-handle audit failed: duplicate callsite definitions", file=sys.stderr)
                return 1
            found = body

    if found is None:
        print("MW3 XEX module-handle audit failed: startup callsite not found", file=sys.stderr)
        return 1

    ok, reason = audit_body(found)
    if not ok:
        print(f"MW3 XEX module-handle audit failed: {reason}", file=sys.stderr)
        return 1
    print(f"MW3 XEX module-handle audit passed: {reason}")
    print("MW3 startup reads the module global, dereferences its loader entry,")
    print("loads xex_header_base at +0x58, and passes it to RtlImageXexHeaderField.")
    print("Generated PPC source was inspected locally and was not emitted.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
