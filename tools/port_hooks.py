#!/usr/bin/env python3
"""Find MW2's hooked functions in another title's executable (MW3).

The runtime hooks the title's own functions by address (runtime/title.h) and
the recompiler needs the C runtime's save/restore helpers and setjmp/longjmp
(config/MW2*.toml). Modern Warfare 3 is the same engine family, and D3D9,
XAudio2 and the C runtime are statically linked libraries, so many of those
functions exist in MW3's executable with the same instructions except for what
the linker filled in. tools/find_in_title.py does this between MW2's own
campaign and multiplayer executables; this tool does it between two flat PE
images of any two titles.

    tools/port_hooks.py --src mw2/default.pe --dst mw3/default.pe
    tools/port_hooks.py --src mw2/default.pe --dst mw3/default.pe --emit mw3_candidates.h
    tools/port_hooks.py --selftest

Both images are the flat PEs tools/xexdump.py writes. The functions to look
for are every T_ function entry in runtime/title.h's campaign block, and the
`*_address` keys of the campaign's recompiler config.

Each result is one of:
  found       exactly one match; same .pdata size is as good as it gets
  verify      exactly one match but a different size; read both before trusting
  ambiguous   several matches even with a long signature; pick by hand
  unresolved  no match: the function changed, or MW3 has no twin. Look for it
              with tools/xref.py / tools/strings_in.py from a string it uses.

A match is a candidate, not a proof. Anything that matters is checked by
reading the recompiled function (and, for the D3D9 ones, with
docs/d3d9-seam.md) before it is named in the MW3 title.h.

Data entries (T_DATA_*) are globals, not code, and cannot be found this way;
they are listed so none is forgotten.
"""
import os, re, struct, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))

BASE = 0x82000000
# signature lengths tried in turn when a match is ambiguous
ESCALATE = (2, 4, 8)
TOML_KEYS = re.compile(r"^(\w+_address)\s*=\s*(0x[0-9A-Fa-f]+)", re.M)


def parse_title_h(path, block):
    """([(name, address)] functions, [(name, value)] data) of one block of
    runtime/title.h: 'mp' or 'sp' of the title update's version."""
    text = open(path).read()
    blocks = re.split(r"^#(?:if|elif|else|endif)\b.*$", text, flags=re.M)[1:5]
    mp, sp = blocks[:2]
    body = sp if block == "sp" else mp
    funcs = [(n, int(a, 16)) for n, a in
             re.findall(r"#define (T_\w+)\s+([0-9A-F]{8})\b", body)]
    data = re.findall(r"#define (T_DATA_\w+)\s+(0x[0-9A-Fa-f]+)u", body)
    return funcs, data


def parse_toml(path):
    return [(k, int(v, 16)) for k, v in TOML_KEYS.findall(open(path).read())]


def match(src, dst, address, words):
    """(status, [hits], src size) for one source function."""
    size = src.sizes.get(address)
    limit = words if size is None else max(4, min(words, size // 4))
    hits = dst.find(src.signature(address, limit))
    if len(hits) > 1:
        for factor in ESCALATE:
            n = limit * factor
            if size is not None:
                n = min(n, size // 4)
            if n <= limit:
                break
            narrowed = dst.find(src.signature(address, n))
            if narrowed:
                hits = narrowed
            if len(hits) <= 1:
                break
    if not hits:
        return "unresolved", hits, size
    if len(hits) > 1:
        return "ambiguous", hits, size
    other = dst.sizes.get(hits[0])
    if size is not None and other is not None and other != size:
        return "verify", hits, size
    return "found", hits, size


def report(src, dst, funcs, keys, data, words):
    rows = []
    for name, address in funcs + keys:
        status, hits, size = match(src, dst, address, words)
        rows.append((name, address, status, hits, size))
    width = max(len(r[0]) for r in rows) if rows else 10
    for name, address, status, hits, size in rows:
        found = ", ".join(f"{h:08X} ({dst.sizes.get(h, '-')} bytes)" for h in hits) or "-"
        print(f"{name:{width}}  mw2 {address:08X} ({size or '-'} bytes)  {status:10} {found}")
    for name, value in data:
        print(f"{name:{width}}  mw2 {value}  data        find by hand")
    counts = {}
    for r in rows:
        counts[r[2]] = counts.get(r[2], 0) + 1
    print("\n" + ", ".join(f"{n} {k}" for k, n in sorted(counts.items())))
    return rows


def emit(path, rows, data):
    with open(path, "w") as f:
        f.write("// Candidates from tools/port_hooks.py. NOT a title.h: every line is\n"
                "// a guess until the function is read. Do not include this file.\n\n")
        for name, address, status, hits, size in rows:
            if name.endswith("_address"):
                continue
            if status in ("found", "verify") and len(hits) == 1:
                tag = "" if status == "found" else "   // size differs: verify"
                f.write(f"#define {name:30} {hits[0]:08X}{tag}\n")
            elif status == "ambiguous":
                f.write(f"// {name}: ambiguous: " + ", ".join(f"{h:08X}" for h in hits) + "\n")
            else:
                f.write(f"// {name}: UNRESOLVED (mw2 {address:08X})\n")
        for name, value in data:
            f.write(f"// {name}: find by hand (mw2 {value})\n")
        f.write("\n// config/MW3.toml\n")
        for name, address, status, hits, size in rows:
            if not name.endswith("_address"):
                continue
            if status in ("found", "verify") and len(hits) == 1:
                f.write(f"{name} = 0x{hits[0]:08X}\n")
            else:
                f.write(f"# {name}: {status} (mw2 0x{address:08X})\n")
    print(f"wrote {path}")


def selftest():
    import find_in_title as fit

    def image(words, sizes):
        img = object.__new__(fit.Image)
        img.sizes = sizes
        img.words = [fit.mask(w) for w in words]
        img.first = {}
        for i, w in enumerate(img.words):
            img.first.setdefault(w, []).append(i)
        return img

    def prologue(disp, tag):
        # mflr r12; bl +disp (masked); lis r11,tag (masked); addi; stw; blr
        return [0x7D8802A6, 0x48000001 | (disp << 2 & 0x03FFFFFC),
                0x3D600000 | tag, 0x396B0000 | (tag + 1), 0x91610000, 0x4E800020]

    a_words = [0x60000000] * 8 + prologue(10, 0x8200) + [0x60000000] * 8
    b_words = [0x60000000] * 4 + prologue(99, 0x8300) + [0x60000000] * 8
    src = image(a_words, {BASE + 4 * 8: 24})
    dst = image(b_words, {BASE + 4 * 4: 24})
    status, hits, _ = match(src, dst, BASE + 4 * 8, 24)
    assert status == "found" and hits == [BASE + 4 * 4], (status, hits)
    dst = image(b_words, {BASE + 4 * 4: 20})
    status, hits, _ = match(src, dst, BASE + 4 * 8, 24)
    assert status == "verify", status
    dst = image(b_words + b_words, {})
    status, hits, _ = match(src, dst, BASE + 4 * 8, 24)
    assert status == "ambiguous" and len(hits) == 2, (status, hits)
    dst = image([0x60000000] * 40, {})
    status, hits, _ = match(src, dst, BASE + 4 * 8, 24)
    assert status == "unresolved", status
    print("selftest ok")


def main():
    args = sys.argv[1:]
    opts = dict(src=None, dst=None, block="sp", words=24, emit=None,
                title_h=os.path.join(ROOT, "runtime/title.h"),
                toml=os.path.join(ROOT, "config/MW2.toml"))
    while args:
        a = args.pop(0)
        if a == "--selftest":
            return selftest()
        if a in ("--src", "--dst", "--block", "--emit", "--title-h", "--toml"):
            opts[a[2:].replace("-", "_")] = args.pop(0)
        elif a == "--words":
            opts["words"] = int(args.pop(0))
        else:
            raise SystemExit(f"unknown argument {a!r}; see the module docstring")
    if not opts["src"] or not opts["dst"]:
        raise SystemExit("--src and --dst are required (flat PE images)")
    if opts["block"] not in ("sp", "mp"):
        raise SystemExit("--block is sp or mp")
    import find_in_title as fit
    src = fit.Image(opts["src"])
    dst = fit.Image(opts["dst"])
    funcs, data = parse_title_h(opts["title_h"], opts["block"])
    keys = parse_toml(opts["toml"])
    rows = report(src, dst, funcs, keys, data, opts["words"])
    if opts["emit"]:
        emit(opts["emit"], rows, data)


if __name__ == "__main__":
    main()
