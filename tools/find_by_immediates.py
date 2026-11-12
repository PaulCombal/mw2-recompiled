#!/usr/bin/env python3
"""Find a function of one executable in another by its large immediates.

Usage: find_by_immediates.py MW2_ADDR [--src ppc] [--dst ppc_mw3] [--min 4096]

Collects the constants a recompiled function loads (`s64 = N;` with |N| >= --min)
and lists the functions of the other tree that contain every one of them, with
sizes and the difference between the two sequences of structure accesses. A
function with few or common constants matches many; a unique match with the same
size and access sequence is a strong identification, anything else is a lead.
Works on the generated C++ only: no game bytes are read or printed.
"""
import argparse, glob, re, sys

def functions(tree):
    out = {}
    for f in sorted(glob.glob(f"{tree}/ppc_recomp.*.cpp")):
        cur, body = None, []
        for line in open(f):
            m = re.match(r"PPC_FUNC_IMPL\(__imp__sub_([0-9A-F]+)\)", line)
            if m:
                if cur: out[cur] = "".join(body)
                cur, body = m.group(1), []
            else:
                body.append(line)
        if cur: out[cur] = "".join(body)
    return out

def immediates(text, lo):
    return {int(v) for v in re.findall(r"s64 = (-?\d+);", text) if abs(int(v)) >= lo}

def accesses(text):
    return [m for m in re.findall(r"PPC_(?:LOAD|STORE)_U(?:8|16|32|64)\([a-z0-9.]+ \+ (\d+)", text)]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("addr")
    ap.add_argument("--src", default="ppc")
    ap.add_argument("--dst", default="ppc_mw3")
    ap.add_argument("--min", type=int, default=4096)
    a = ap.parse_args()
    src, dst = functions(a.src), functions(a.dst)
    key = a.addr.upper().replace("0X", "")
    if key not in src: sys.exit(f"sub_{key} not in {a.src}")
    imms = immediates(src[key], a.min)
    seq = accesses(src[key])
    print(f"sub_{key}: {len(imms)} large immediates, {len(seq)} offset accesses, {len(src[key].splitlines())} lines")
    if not imms: sys.exit("no large immediates: nothing to search by")
    hits = [(k, t) for k, t in dst.items() if all(f"s64 = {v};" in t for v in imms)]
    for k, t in hits[:10]:
        same = accesses(t) == seq
        print(f"  match sub_{k}: {len(t.splitlines())} lines, access sequence {'IDENTICAL' if same else 'differs'}")
    if not hits: print("  no function in", a.dst, "has all of them")
    if len(hits) > 10: print(f"  ... and {len(hits) - 10} more")

main()
