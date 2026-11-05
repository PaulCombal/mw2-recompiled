#!/usr/bin/env python3
# Compares the game's font rasteriser (src/runtime/truetype.rs) with stb_truetype as
# the C++ build's screens call it (runtime/screen.cpp; see font_diff.rs and font_diff.cpp), byte for byte:
# the atlas, the 95 glyphs, the result and the ascent. On the three fonts of third_party/fonts
# that screen.rs uses at every whole size from 1 to 600 px with screen.rs's doubling of the
# atlas from 512 to 8192, every atlas side for a sample of sizes, sizes that are not whole,
# zero and negative; on fonts of this machine's /usr/share/fonts chosen for what they reach
# (each cmap format stb picks, compound glyphs, CFF outlines, a collection); on copies of
# Barlow made here with every cmap format stb reads, the other loca format and compound
# glyphs with every transform; and on damaged copies of the fonts, the C run apart under
# AddressSanitizer with guard pages around the font. The Rust is built with overflow checks
# and debug_assert! on, and the C++ as the game builds it (clang++ -O3, gnu++20, x86-64-v2, NDEBUG).
#
#   usage: font_diff.py [--src folder] [--work folder] [--seeds N] [--quick] [--coverage]
#
# --src takes the Rust from another folder, to try a deliberate mistake; --quick runs a
# smaller sweep and fuzz; --coverage builds the C with llvm's coverage and lists the lines
# of the functions the port covers that no input reached. Exits nonzero at the first
# difference, leaving the font in the work folder.
import argparse, glob, os, random, re, shutil, struct, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONTS = [f"{ROOT}/third_party/fonts/{name}.ttf"
         for name in ("BarlowCondensed-Medium", "BarlowCondensed-Regular", "BarlowSemiCondensed-Regular")]
HEADER_DIR = f"{ROOT}/build/_deps/imgui-src"
CXX = ["clang++-18", "-std=gnu++20", "-march=x86-64-v2", "-DNDEBUG", "-fno-exceptions", "-fno-rtti", "-I", HEADER_DIR]

# What stb_truetype functions the port covers, for --coverage.
PORTED = """stbtt__buf_get8 stbtt__buf_peek8 stbtt__buf_seek stbtt__buf_skip stbtt__buf_get stbtt__new_buf
stbtt__buf_range stbtt__cff_get_index stbtt__cff_int stbtt__cff_skip_operand stbtt__dict_get stbtt__dict_get_ints
stbtt__cff_index_count stbtt__cff_index_get ttUSHORT ttSHORT ttULONG ttLONG stbtt__isfont stbtt__find_table
stbtt_GetFontOffsetForIndex_internal stbtt__get_subrs stbtt_InitFont_internal stbtt_FindGlyphIndex stbtt_setvertex
stbtt__GetGlyfOffset stbtt_GetGlyphBox stbtt__close_shape stbtt__GetGlyphShapeTT stbtt__track_vertex stbtt__csctx_v
stbtt__csctx_close_shape stbtt__csctx_rmove_to stbtt__csctx_rline_to stbtt__csctx_rccurve_to stbtt__get_subr
stbtt__cid_get_glyph_subrs stbtt__run_charstring stbtt__GetGlyphShapeT2 stbtt__GetGlyphInfoT2 stbtt_GetGlyphShape
stbtt_GetGlyphHMetrics stbtt_GetFontVMetrics stbtt_ScaleForPixelHeight stbtt_ScaleForMappingEmToPixels
stbtt_GetGlyphBitmapBoxSubpixel stbtt_GetGlyphBitmapBox stbtt__new_active stbtt__handle_clipped_edge
stbtt__sized_trapezoid_area stbtt__position_trapezoid_area stbtt__sized_triangle_area stbtt__fill_active_edges_new
stbtt__rasterize_sorted_edges stbtt__sort_edges_ins_sort stbtt__sort_edges_quicksort stbtt__sort_edges stbtt__rasterize
stbtt__add_point stbtt__tesselate_curve stbtt__tesselate_cubic stbtt_FlattenCurves stbtt_Rasterize
stbtt_MakeGlyphBitmapSubpixel stbrp_init_target stbrp_pack_rects stbtt_PackBegin stbtt_PackEnd stbtt__oversample_shift
stbtt_PackFontRangesGatherRects stbtt_PackFontRangesRenderIntoRects stbtt_PackFontRangesPackRects stbtt_PackFontRanges
stbtt_PackFontRange stbtt_GetFontOffsetForIndex stbtt_InitFont""".split()

def run(command, **kw):
    return subprocess.run(command, check=True, **kw)

# --- reading and writing fonts -------------------------------------------------------

def be16(d, at): return struct.unpack(">H", d[at:at + 2])[0]
def be32(d, at): return struct.unpack(">I", d[at:at + 4])[0]

def directory(data):
    start = be32(data, 12) if data[:4] == b"ttcf" else 0
    return start, {bytes(data[start + 12 + 16 * i:start + 16 + 16 * i]): (be32(data, start + 20 + 16 * i), be32(data, start + 24 + 16 * i))
                   for i in range(be16(data, start + 4))}

# The cmap subtable stb picks: the last Microsoft Unicode (1 or 10) or Unicode-platform one.
def chosen_cmap(data):
    _, tables = directory(data)
    if b"cmap" not in tables:
        return None
    cmap = tables[b"cmap"][0]
    chosen = None
    for i in range(be16(data, cmap + 2)):
        record = cmap + 4 + 8 * i
        platform, encoding = be16(data, record), be16(data, record + 2)
        if (platform == 3 and encoding in (1, 10)) or platform == 0:
            chosen = cmap + be32(data, record + 4)
    return chosen

def glyph_index(data, cmap, c):
    fmt = be16(data, cmap)
    if fmt == 4:
        segs = be16(data, cmap + 6) // 2
        for s in range(segs):
            end, start = be16(data, cmap + 14 + 2 * s), be16(data, cmap + 16 + 2 * segs + 2 * s)
            if start <= c <= end:
                delta = be16(data, cmap + 16 + 4 * segs + 2 * s)
                ro_at = cmap + 16 + 6 * segs + 2 * s
                ro = be16(data, ro_at)
                if ro == 0:
                    return (c + delta) & 0xFFFF
                g = be16(data, ro_at + ro + 2 * (c - start))
                return (g + delta) & 0xFFFF if g else 0
        return 0
    if fmt in (12, 13):
        for k in range(be32(data, cmap + 12)):
            s, e, g = struct.unpack(">III", data[cmap + 16 + 12 * k:cmap + 28 + 12 * k])
            if s <= c <= e:
                return g + c - s if fmt == 12 else g
        return 0
    return None

def has_compound_ascii(data):
    _, tables = directory(data)
    if not all(t in tables for t in (b"glyf", b"loca", b"head")):
        return False
    cmap = chosen_cmap(data)
    if cmap is None:
        return False
    glyf, loca = tables[b"glyf"][0], tables[b"loca"][0]
    long = be16(data, tables[b"head"][0] + 50)
    for c in range(32, 127):
        g = glyph_index(data, cmap, c)
        if not g:
            continue
        at = glyf + (be32(data, loca + 4 * g) if long else 2 * be16(data, loca + 2 * g))
        if at + 2 <= len(data) and struct.unpack(">h", data[at:at + 2])[0] < 0:
            return True
    return False

# A copy with tables replaced or added at the end (the directory points at them; stb reads
# no checksum).
def with_tables(data, new):
    data = bytearray(data)
    start, tables = directory(data)
    count = be16(data, start + 4)
    for tag, body in new.items():
        while len(data) % 4:
            data.append(0)
        at = len(data)
        data += body
        for i in range(count):
            entry = start + 12 + 16 * i
            if data[entry:entry + 4] == tag:
                data[entry + 8:entry + 16] = struct.pack(">II", at, len(body))
                break
        else:
            raise ValueError(f"no {tag} to replace")
    return bytes(data)

def cmap_table(records):
    # records: (platform, encoding, subtable bytes)
    head = struct.pack(">HH", 0, len(records))
    offset = 4 + 8 * len(records)
    out, bodies = bytearray(head), bytearray()
    for platform, encoding, body in records:
        out += struct.pack(">HHI", platform, encoding, offset + len(bodies))
        bodies += body
        while len(bodies) % 2:
            bodies.append(0)
    return bytes(out + bodies)

def format4(mapping, use_ranges):
    # Segments of consecutive codes; idRangeOffset into a glyph array when use_ranges.
    codes = sorted(mapping)
    segs, run = [], [codes[0]]
    for c in codes[1:]:
        if c == run[-1] + 1 and (use_ranges or mapping[c] == mapping[run[-1]] + 1):
            run.append(c)
        else:
            segs.append(run)
            run = [c]
    segs.append(run)
    segs.append([0xFFFF])
    n = len(segs)
    ends = [s[-1] for s in segs]
    starts = [s[0] for s in segs]
    deltas, ranges, array = [], [], []
    for i, s in enumerate(segs):
        if s == [0xFFFF]:
            deltas.append(1); ranges.append(0)
        elif use_ranges and i % 2 == 0:
            deltas.append(0)
            ranges.append(2 * (n - i) + 2 * len(array))
            array += [mapping[c] for c in s]
        else:
            deltas.append((mapping[s[0]] - s[0]) & 0xFFFF); ranges.append(0)
    search = 1
    selector = 0
    while search * 2 <= n:
        search *= 2
        selector += 1
    body = struct.pack(">HHHH", n * 2, search * 2, selector, n * 2 - search * 2)
    body += struct.pack(f">{n}H", *ends) + b"\0\0" + struct.pack(f">{n}H", *starts)
    body += struct.pack(f">{n}H", *deltas) + struct.pack(f">{n}H", *ranges) + struct.pack(f">{len(array)}H", *array)
    return struct.pack(">HHH", 4, 6 + len(body), 0) + body

def format12(mapping, fmt=12):
    groups, codes = [], sorted(mapping)
    for c in codes:
        if groups and c == groups[-1][1] + 1 and (mapping[c] == mapping[groups[-1][0]] + c - groups[-1][0] if fmt == 12
                                                 else mapping[c] == mapping[groups[-1][0]]):
            groups[-1][1] = c
        else:
            groups.append([c, c])
    body = b"".join(struct.pack(">III", s, e, mapping[s]) for s, e in groups)
    return struct.pack(">HHIII", fmt, 0, 16 + len(body), 0, len(groups)) + body

def crafted(work):
    base = open(FONTS[1], "rb").read()
    cmap = chosen_cmap(base)
    mapping = {c: glyph_index(base, cmap, c) for c in range(32, 127)}
    mapping = {c: g for c, g in mapping.items() if g}
    out = {}
    def save(name, data):
        path = f"{work}/crafted-{name}.{'otf' if name.startswith('cff') else 'ttf'}"
        open(path, "wb").write(data)
        out[name] = path
    # cmap formats, each as the one stb picks
    f0 = bytes(mapping.get(c, 0) & 0xFF if mapping.get(c, 0) < 256 else 0 for c in range(256))
    save("cmap0", with_tables(base, {b"cmap": cmap_table([(0, 3, struct.pack(">HHH", 0, 262, 0) + f0)])}))
    short0 = struct.pack(">HHH", 0, 6 + 70, 0) + bytes(mapping.get(c, 0) & 0xFF for c in range(70))
    save("cmap0-short", with_tables(base, {b"cmap": cmap_table([(0, 3, short0)])}))
    f6 = struct.pack(">HHHHH", 6, 10 + 2 * 60, 0, 40, 60) + struct.pack(">60H", *(mapping.get(c, 0) for c in range(40, 100)))
    save("cmap6", with_tables(base, {b"cmap": cmap_table([(3, 1, f6)])}))
    save("cmap4-ranges", with_tables(base, {b"cmap": cmap_table([(3, 1, format4(mapping, True))])}))
    # every code its own segment, so that a binary search lands on each; and a glyph past
    # the font's count, whose delta wraps
    shuffled = sorted(mapping.values())
    random.Random(4).shuffle(shuffled)
    save("cmap4-singles", with_tables(base, {b"cmap": cmap_table([(3, 1, format4(dict(zip(sorted(mapping), shuffled)), False))])}))
    save("cmap4-high-glyph", with_tables(base, {b"cmap": cmap_table([(3, 1, format4({**mapping, ord("z"): 40000, ord("y"): 65500}, False))])}))
    save("cmap4-gaps", with_tables(base, {b"cmap": cmap_table([(3, 1, format4({c: g for c, g in mapping.items() if c % 7}, False))])}))
    save("cmap12", with_tables(base, {b"cmap": cmap_table([(3, 10, format12(mapping))])}))
    save("cmap12-gaps", with_tables(base, {b"cmap": cmap_table([(3, 10, format12({c: g for c, g in mapping.items() if c % 5}))])}))
    save("cmap13", with_tables(base, {b"cmap": cmap_table([(3, 10, format12({c: mapping[65 + c % 26] for c in range(32, 127)}, 13))])}))
    save("cmap2", with_tables(base, {b"cmap": cmap_table([(3, 1, struct.pack(">HHH", 2, 6, 0))])}))
    save("cmap99", with_tables(base, {b"cmap": cmap_table([(3, 1, struct.pack(">HHH", 99, 6, 0))])}))
    save("cmap-mac-only", with_tables(base, {b"cmap": cmap_table([(1, 0, struct.pack(">HHH", 0, 262, 0) + f0)])}))
    save("cmap-ms-symbol-then-unicode", with_tables(base, {b"cmap": cmap_table(
        [(3, 0, f6), (3, 1, format4(mapping, False)), (2, 1, f6)])}))
    # the other loca format
    _, tables = directory(base)
    head_at = tables[b"head"][0]
    loca_at, loca_len = tables[b"loca"]
    long = be16(base, head_at + 50)
    count = loca_len // (4 if long else 2)
    offsets = [be32(base, loca_at + 4 * i) if long else 2 * be16(base, loca_at + 2 * i) for i in range(count)]
    glyf_at, glyf_len = tables[b"glyf"]
    glyf = bytearray(base[glyf_at:glyf_at + glyf_len])
    if long:
        # short offsets need even ones: realign each glyph
        new_glyf, new_offsets = bytearray(), []
        for i in range(count - 1):
            new_offsets.append(len(new_glyf))
            new_glyf += glyf[offsets[i]:offsets[i + 1]]
            if len(new_glyf) % 2:
                new_glyf.append(0)
        new_offsets.append(len(new_glyf))
        loca = struct.pack(f">{count}H", *(o // 2 for o in new_offsets))
        glyf = new_glyf
    else:
        loca = struct.pack(f">{count}I", *offsets)
    head = bytearray(base[head_at:head_at + 54])
    head[50:52] = struct.pack(">H", 0 if long else 1)
    other = with_tables(base, {b"loca": loca, b"glyf": bytes(glyf), b"head": bytes(head)})
    save("loca-other", other)
    head[50:52] = struct.pack(">H", 2)
    save("loca-unknown", with_tables(base, {b"head": bytes(head)}))
    # compound glyphs: letters remade from others with each kind of transform
    glyphs = [bytes(glyf[offsets[i]:offsets[i + 1]]) if long else bytes(base[glyf_at + offsets[i]:glyf_at + offsets[i + 1]])
              for i in range(count - 1)]
    def component(flags, index, args, transform):
        return struct.pack(">HH", flags, index) + args + transform
    kinds = []
    def add_compound(parts):
        body = struct.pack(">hhhhh", -1, -100, -300, 1400, 1700)
        for k, (flags, index, args, transform) in enumerate(parts):
            more = 0x20 if k + 1 < len(parts) else 0
            body += component(flags | more, index, args, transform)
        if len(body) % 2:
            body += b"\0"
        glyphs.append(body)
        return len(glyphs) - 1
    g = lambda c: mapping[c]
    words = lambda *v: struct.pack(f">{len(v)}h", *v)
    sbytes = lambda *v: struct.pack(f">{len(v)}b", *v)
    kinds.append(add_compound([(0x0003, g(ord("A")), words(30, -40), b"")]))                        # word offsets
    kinds.append(add_compound([(0x0002, g(ord("B")), sbytes(-20, 15), b"")]))                       # byte offsets
    kinds.append(add_compound([(0x000B, g(ord("C")), words(10, 10), words(0x2000))]))               # one scale
    kinds.append(add_compound([(0x0043, g(ord("D")), words(0, 0), words(0x3000, 0x5000))]))         # x and y scales
    kinds.append(add_compound([(0x0083, g(ord("E")), words(5, 5), words(0x4000, 0x1000, -0x0800, 0x3800))]))  # 2 by 2
    kinds.append(add_compound([(0x0000, g(ord("F")), sbytes(1, 2), b"")]))                          # points to match: ignored
    kinds.append(add_compound([(0x0003, g(ord("G")), words(0, 0), b""), (0x0003, g(ord("o")), words(400, 0), b"")]))  # two
    inner = add_compound([(0x0003, g(ord("H")), words(-50, 0), b""), (0x0002, g(ord("i")), sbytes(100, -100), b"")])
    kinds.append(add_compound([(0x000B, inner, words(20, 20), words(0x3800))]))                     # nested
    kinds.append(add_compound([(0x0083, g(ord("W")), words(0, 0), words(-0x4000, 0, 0, 0x4000))]))  # mirrored
    kinds.append(add_compound([(0x0003, g(ord(" ")), words(0, 0), b"")]))                           # of an empty glyph
    head = bytearray(base[head_at:head_at + 54])
    head[50:52] = struct.pack(">H", 1)
    maxp_at, maxp_len = tables[b"maxp"]
    maxp = bytearray(base[maxp_at:maxp_at + maxp_len])
    hhea_at = tables[b"hhea"][0]
    long_metrics = be16(base, hhea_at + 34)
    remap = dict(mapping)
    for k, c in enumerate("acegikmoqs"):
        remap[ord(c)] = kinds[k]
    def compound_font(glyphs, remap):
        new_glyf, new_offsets = bytearray(), []
        for body in glyphs:
            new_offsets.append(len(new_glyf))
            new_glyf += body
            while len(new_glyf) % 4:
                new_glyf.append(0)
        new_offsets.append(len(new_glyf))
        loca = struct.pack(f">{len(new_offsets)}I", *new_offsets)
        maxp[4:6] = struct.pack(">H", len(glyphs))
        hmtx = base[tables[b"hmtx"][0]:tables[b"hmtx"][0] + 4 * long_metrics] + struct.pack(">h", 77) * (len(glyphs) - long_metrics)
        return with_tables(base, {b"glyf": bytes(new_glyf), b"loca": loca, b"head": bytes(head), b"maxp": bytes(maxp),
                                  b"hmtx": hmtx, b"cmap": cmap_table([(3, 1, format4(remap, False))])})
    save("compound", compound_font(glyphs, remap))
    # A transform that takes points out of a short, where the C is undefined: the port refuses.
    past = add_compound([(0x0043, g(ord("M")), words(-30000, 30000), words(0x7FFF, 0x7FFF))])
    save("refused-compound-past-short", compound_font(glyphs, {**remap, ord("u"): past}))
    # A compound glyph made of itself, on which the C runs out of stack: the port refuses.
    glyphs.append(struct.pack(">hhhhh", -1, 0, 0, 100, 100) + struct.pack(">HHhh", 0x0003, len(glyphs), 0, 0))
    save("refused-compound-cycle", compound_font(glyphs, {**remap, ord("u"): len(glyphs) - 1}))
    # a font with no maxp, and one whose metrics have one long entry
    one_metric = bytearray(base[hhea_at:hhea_at + 36])
    one_metric[34:36] = struct.pack(">H", 1)
    save("hmtx-one-long", with_tables(base, {b"hhea": bytes(one_metric)}))
    # glyph boxes that do not hold the outline: above it, beside it, shifted; the rasteriser's
    # clipping and its edges that end above the first row
    wrong = bytearray(base)
    for k, c in enumerate("abcdefghijklmnopqrstuvwxyz0123456789"):
        g = mapping[ord(c)]
        at = glyf_at + offsets[g]
        if offsets[g + 1] == offsets[g]:
            continue
        x0, y0, x1, y1 = struct.unpack(">hhhh", wrong[at + 2:at + 10])
        box = [(x0, -200, x1, -1), (x1 - 2, y0, x1, y1), (x0 + 300, y0 + 200, x1 + 300, y1 + 200), (x0, y0, x0 + 1, y0 + 1),
               (x0, 0, x1, 0), (x0 - 400, y0, x1 + 400, y1)][k % 6]
        wrong[at + 2:at + 10] = struct.pack(">hhhh", *box)
    save("wrong-boxes", bytes(wrong))
    # an ascent equal to the descent: an infinite scale, boxes of floats out of an int's range
    # (x86 gives the lowest int), and one of a height of a unit
    hhea_at = tables[b"hhea"][0]
    flat = bytearray(base[hhea_at:hhea_at + 36])
    flat[6:8] = flat[4:6]
    save("zero-height", with_tables(base, {b"hhea": bytes(flat)}))
    flat[6:8] = struct.pack(">h", struct.unpack(">h", flat[4:6])[0] - 1)
    save("unit-height", with_tables(base, {b"hhea": bytes(flat)}))
    # no maxp: stb takes 65535 glyphs
    data = bytearray(base)
    start, _ = directory(base)
    for i in range(be16(base, start + 4)):
        if data[start + 12 + 16 * i:start + 16 + 16 * i] == b"maxp":
            data[start + 12 + 16 * i:start + 16 + 16 * i] = b"maxq"
    save("no-maxp", bytes(data))
    # every glyph of the range missing: the first is drawn, the others copy it
    save("cmap-all-missing", with_tables(base, {b"cmap": cmap_table([(3, 1, format4({0x4E00: 5}, False))])}))
    for name, data in cff_fonts(base).items():
        save(name, data)
    return out

# --- CFF fonts made here: every Type 2 operator stb reads, each of its errors, local and
# global subroutines, the three subroutine biases, and CID fonts with FDSelect 0 and 3.

def cff_number(v):
    if isinstance(v, float):
        return b"\xff" + struct.pack(">i", int(round(v * 65536)))
    if -107 <= v <= 107:
        return bytes([v + 139])
    if 108 <= v <= 1131:
        v -= 108
        return bytes([247 + (v >> 8), v & 0xFF])
    if -1131 <= v <= -108:
        v = -v - 108
        return bytes([251 + (v >> 8), v & 0xFF])
    return b"\x1c" + struct.pack(">h", v)

def charstring(*items):
    out = bytearray()
    for item in items:
        if isinstance(item, (int, float)):
            out += cff_number(item)
        elif isinstance(item, str):
            out += bytes(int(op, 16) for op in item.split())
        else:
            out += bytes(item)
    return bytes(out)

def cff_index(items):
    if not items:
        return b"\0\0"
    offsets, at = [1], 1
    for item in items:
        at += len(item)
        offsets.append(at)
    size = 4 if at > 0xFFFF else 2
    return struct.pack(">HB", len(items), size) + b"".join(o.to_bytes(size, "big") for o in offsets) + b"".join(items)

def dict_int(v):
    return b"\x1d" + struct.pack(">i", v)  # five bytes, so that offsets can be laid out first

def cff_table(glyphs, gsubrs, subrs, cid=None, real_in_top=True):
    # cid: None, or (fdselect format, [local subrs per font dict], glyph -> font dict)
    header = b"\x01\x00\x04\x02"
    name = cff_index([b"Crafted"])
    strings = cff_index([])
    global_subrs = cff_index(gsubrs)
    def layout(top_size):
        at = len(header) + len(name) + 2 + 2 + top_size + len(strings) + len(global_subrs)
        return at
    def top_dict(charstrings_at, private_at, private_size, fdarray_at, fdselect_at):
        d = b""
        if real_in_top:
            d += b"\x1e\x1a\x5f" + b"\x1e\x0a\x01\x2f" + b"\x8b\x8b" + b"\x1e\x0a\x01\x2f\x8b\x8b" + b"\x0c\x07"  # FontMatrix
        d += dict_int(charstrings_at) + b"\x11"
        if cid is None:
            d += dict_int(private_size) + dict_int(private_at) + b"\x12"
        else:
            d += dict_int(fdarray_at) + b"\x0c\x24" + dict_int(fdselect_at) + b"\x0c\x25"
        return d
    top_size = len(top_dict(0, 0, 0, 0, 0))
    top_index_size = len(cff_index([b"x" * top_size]))
    at = len(header) + len(name) + top_index_size + len(strings) + len(global_subrs)
    charstrings = cff_index(glyphs)
    charstrings_at = at
    at += len(charstrings)
    tail = b""
    if cid is None:
        private = dict_int(6) + b"\x13"  # Subrs, just after the private dict
        local = cff_index(subrs)
        private_at, private_size = at, len(private)
        tail = private + local
        top = top_dict(charstrings_at, private_at, private_size, 0, 0)
    else:
        fmt, locals_, select = cid
        # each font dict's private dict and subrs, then the FDArray, then the FDSelect
        privates, fd_dicts = b"", []
        for subr_list in locals_:
            private = dict_int(6) + b"\x13"  # Subrs, just after it
            local = cff_index(subr_list) if subr_list is not None else b""
            if subr_list is None:
                private = b"\x8b\x8b\x0f"  # no Subrs
            fd_dicts.append(dict_int(len(private)) + dict_int(at + len(privates)) + b"\x12")
            privates += private + local
        fdarray = cff_index(fd_dicts)
        fdarray_at = at + len(privates)
        if fmt == 0:
            fdselect = bytes([0]) + bytes(select)
        else:
            ranges, start = [], 0
            for g in range(1, len(select) + 1):
                if g == len(select) or select[g] != select[start]:
                    ranges.append((start, select[start]))
                    start = g
            fdselect = struct.pack(">BH", 3, len(ranges)) + b"".join(struct.pack(">HB", s, f) for s, f in ranges) + struct.pack(">H", len(select))
        fdselect_at = fdarray_at + len(fdarray)
        tail = privates + fdarray + fdselect
        top = top_dict(charstrings_at, 0, 0, fdarray_at, fdselect_at)
    assert len(top) == top_size
    return header + name + cff_index([top]) + strings + global_subrs + charstrings + tail

def otf(base, cff, count, mapping):
    _, tables = directory(base)
    get = lambda tag: base[tables[tag][0]:tables[tag][0] + tables[tag][1]]
    hhea = bytearray(get(b"hhea"))
    hhea[34:36] = struct.pack(">H", count)
    hmtx = b"".join(struct.pack(">hh", 500 + 7 * g, 10) for g in range(count))
    maxp = struct.pack(">IH", 0x5000, count)
    built = {b"CFF ": cff, b"cmap": cmap_table([(3, 1, format4(mapping, False))]), b"head": get(b"head"),
             b"hhea": bytes(hhea), b"hmtx": hmtx, b"maxp": maxp}
    tags = sorted(built)
    out = bytearray(b"OTTO" + struct.pack(">HHHH", len(tags), 0, 0, 0))
    at = 12 + 16 * len(tags)
    bodies = b""
    for tag in tags:
        body = built[tag] + b"\0" * (-len(built[tag]) % 4)
        out += tag + struct.pack(">III", 0, at + len(bodies), len(built[tag]))
        bodies += body
    return bytes(out + bodies)

def cff_fonts(base):
    E = "0e"  # endchar
    square = (100, 100, "15", 400, 0, 0, 400, -400, 0, "05")
    glyphs = [
        charstring(*square, E),                                                    # .notdef
        charstring(20, 30, 40, 50, "01", 10, 20, "17", "13", 0xFF, *square, E),    # stems and a hint mask
        charstring(10, 20, "12", 30, 40, "03", 300, "14", 0xF0, 50, "16", 300, 200, -300, "07", 100, 50, -100, 20, "06", E),
        charstring(50, 60, "04", 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, "08", E),  # rrcurveto, two
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, "18", E),          # rcurveline
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, "19", E),          # rlinecurve
        charstring(0, 0, "15", 15, 10, 20, 30, 40, 50, 60, 70, 80, "1a", E),      # vvcurveto, odd
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, "1b", E),          # hhcurveto, even
        charstring(0, 0, "15", 15, 10, 20, 30, 40, "1b", E),                      # hhcurveto, odd
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, 90, "1e", E),      # vhcurveto, five left
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, "1f", E),          # hvcurveto
        charstring(0, 0, "15", 10, 20, 30, 40, 50, "1f", E),                      # hvcurveto, five
        charstring(0, 0, "15", 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 50, "0c 23", E),  # flex
        charstring(0, 0, "15", 100, 50, 30, 100, 100, 30, 100, "0c 22", E),                         # hflex
        charstring(0, 0, "15", 100, 10, 50, 30, 100, 100, 30, -20, 100, "0c 24", E),                # hflex1
        charstring(0, 0, "15", 100, 10, 50, 30, 100, 0, 100, 0, 30, -20, 77, "0c 25", E),           # flex1, wide
        charstring(0, 0, "15", 10, 100, 30, 50, 0, 100, 0, 100, -20, 30, 77, "0c 25", E),           # flex1, tall
        charstring(0, 0, "15", 1.5, -2.25, 300.75, 40, 1000, -1000, 5000, "08", E),                 # fixed and long numbers
        charstring(0, 0, "15", 0, "0a", E),                                          # a local subroutine
        charstring(0, 0, "15", -107, "1d", 3, 4, "05", E),                            # a global one, that calls another
        charstring(0, 0, "15", 2 - 107, "0a"),                                        # endchar in a subroutine
        charstring(0, 0, "15", 3 - 107, "0a", E),                                     # recursion past ten
        charstring(0, 0, "15", 30, "0a", E),                                          # no such subroutine
        charstring(0, 0, "15", "0b", E),                                              # return outside one
        charstring(*square, *range(49), E),                                           # push past 48
        charstring(*square),                                                          # no endchar
        charstring(*square, "02", E),                                                 # reserved operator
        charstring(*square, "0c 30", E),                                              # unknown escape
        charstring(100, 100, "05", 50, 50, "15", E),                                  # a line before any move
        charstring(5, 5, 5, 5, 5, 5, 5, "15", 50, "15", E),                           # moves on a deep stack
        charstring(0, 0, "15", 10, 50, 20, 50, 30, 0, 40, 0, 0, 0, 77, "0c 25", E),                # flex1, |dx| = |dy|
        charstring(-500, -500, "15", -300, 0, 0, -300, 300, 0, "05", E),                           # all below and left
        charstring(100, 100, "15", 300, 0, 0, 300, "05", 50, 0, "15", 0, 100, 80, 0, "05", 200, "16", 9, 9, "05", E),  # open paths
    ]
    # each operator with too few arguments
    for op, need in [("15", 2), ("04", 1), ("16", 1), ("05", 2), ("07", 1), ("06", 1), ("1f", 4), ("1e", 4), ("08", 6),
                     ("18", 8), ("19", 8), ("1a", 4), ("1b", 4), ("0c 22", 7), ("0c 23", 13), ("0c 24", 9), ("0c 25", 11),
                     ("0a", 1), ("1d", 1)]:
        glyphs.append(charstring(0, 0, "15", *range(need - 1), op, *square, E) if op not in ("15",) else charstring(*range(need - 1), op, E))
    gsubrs = [charstring(*square, "0b")] * 1 + [charstring(-106, "1d", "0b"), charstring(20, 0, "05", "0b")]
    subrs = [charstring(200, 0, 0, 200, "05", "0b"), charstring(30, 30, "05", "0b"), charstring(*square, E),
             charstring(3 - 107, "0a", "0b")]
    count = len(glyphs)
    mapping = {33 + k: k for k in range(1, min(count, 94))}
    fonts = {"cff": otf(base, cff_table(glyphs, gsubrs, subrs), count, mapping)}
    # the biases: 1240 and 33900 subroutines, the first a square
    many = lambda n: [charstring(*square, "0b")] + [charstring(10, 10, "05", "0b")] * (n - 1)
    calls = [charstring(*square, E)] + [charstring(0, 0, "15", k, "0a", k, "1d", 1, 1, "05", E) for k in (-1131, -1130, 0, 1000, -32768 + 5, 1)]
    fonts["cff-subrs-1240"] = otf(base, cff_table(calls, many(1240), many(1240)), len(calls), {33 + k: k for k in range(1, len(calls))})
    fonts["cff-subrs-33900"] = otf(base, cff_table(calls, many(33900), many(33900)), len(calls), {33 + k: k for k in range(1, len(calls))})
    # CID: two font dicts, one with no Subrs, picked per glyph by FDSelect 3 and 0
    cid_glyphs = [charstring(*square, E)] + [charstring(0, 0, "15", -107, "0a", 40, 40, "05", E)] * 8
    select = [0, 0, 1, 1, 0, 1, 1, 0, 1]
    lists = [[charstring(100, 0, 0, 100, "05", "0b")], None]
    for fmt in (3, 0):
        fonts[f"cff-cid-fdselect{fmt}"] = otf(base, cff_table(cid_glyphs, [], [], cid=(fmt, lists, select)), len(cid_glyphs),
                                               {33 + k: k for k in range(1, len(cid_glyphs))})
    fonts["cff-cid-fdselect-short"] = otf(base, cff_table(cid_glyphs, [], [], cid=(3, lists, select[:4])), len(cid_glyphs),
                                           {33 + k: k for k in range(1, len(cid_glyphs))})
    return fonts

# Fonts of this machine that reach something: each cmap format stb picks, compound glyphs,
# CFF, a collection. Chosen by name so that a run picks the same ones.
def system_fonts():
    candidates = sorted(glob.glob("/usr/share/fonts/**/*.[ot]t[fc]", recursive=True))
    picked, why, more_cff = [], {}, []
    def take(path, reason, limit):
        if why.get(reason, 0) < limit and path not in picked:
            picked.append(path)
            why[reason] = why.get(reason, 0) + 1
    for path in candidates:
        try:
            data = open(path, "rb").read()
            start, tables = directory(data)
            cmap = chosen_cmap(data)
        except Exception:
            continue
        if data[:4] == b"ttcf":
            take(path, "collection", 2)
        if b"CFF " in tables and b"glyf" not in tables:
            take(path, "cff", 4)
            if path not in picked:
                more_cff.append(path)  # a few sizes each: some have flex, some not
            continue
        if cmap is None:
            continue
        fmt = be16(data, cmap)
        take(path, f"cmap{fmt}", 3)
        try:
            if has_compound_ascii(data):
                take(path, "compound", 4)
        except Exception:
            pass
    for name in ("DejaVuSans.ttf", "DejaVuSerif-Bold.ttf", "LiberationMono-Regular.ttf", "NotoSansMono-Regular.ttf"):
        for path in candidates:
            if os.path.basename(path) == name:
                take(path, "plain", 4)
    return picked, why, more_cff

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--work", default=f"{ROOT}/build/font_diff")
    parser.add_argument("--seeds", type=int, default=3000)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--coverage", action="store_true")
    parser.add_argument("--report", action="store_true", help=argparse.SUPPRESS)  # the coverage of the last run again
    arguments = parser.parse_args()
    work = os.path.abspath(arguments.work)
    os.makedirs(work, exist_ok=True)
    if arguments.report:
        return report_coverage(work, f"{work}/font_diff", f"{work}/font_diff_child")

    profile = ["-fprofile-instr-generate", "-fcoverage-mapping"] if arguments.coverage else []
    library = f"{work}/libfont_diff_c.a"
    run(CXX + ["-O3"] + profile + ["-c", f"{ROOT}/tools/font_diff.cpp", "-o", f"{work}/font_diff_c.o"])
    if os.path.exists(library):
        os.remove(library)
    run(["ar", "rcs", library, f"{work}/font_diff_c.o"])
    child = f"{work}/font_diff_child"
    run(CXX + ["-O1", "-g", "-fsanitize=address,float-cast-overflow,signed-integer-overflow",
               "-fno-sanitize-recover=all", "-DFONT_DIFF_CHILD"] + profile + [f"{ROOT}/tools/font_diff.cpp", "-o", child])
    source = f"{work}/font_diff.rs"
    text = open(f"{ROOT}/tools/font_diff.rs").read()
    open(source, "w").write(text.replace('#[path = "../src/runtime/truetype.rs"]', f'#[path = "{arguments.src}/runtime/truetype.rs"]'))
    program = f"{work}/font_diff"
    link = []
    if arguments.coverage:
        runtime = subprocess.run(["clang-18", "--print-file-name=libclang_rt.profile-x86_64.a"], capture_output=True, text=True).stdout.strip()
        link = ["-C", f"link-arg={runtime}", "-C", "link-arg=-Wl,--undefined=__llvm_profile_runtime"]
    run(["rustc", "--edition=2024", "-Copt-level=3", "-Cdebug-assertions=on", "-Coverflow-checks=on", "-Ctarget-cpu=x86-64-v2",
         "-o", program, source, "-L", work, "-l", "static=font_diff_c", "-l", "m"] + link)
    environment = dict(os.environ)
    if arguments.coverage:
        shutil.rmtree(f"{work}/profiles", ignore_errors=True)
        environment["LLVM_PROFILE_FILE"] = f"{work}/profiles/%8m.profraw"  # merged as they go
    environment["ASAN_OPTIONS"] = "detect_leaks=0:allocator_may_return_null=1"

    extra = crafted(work)
    system, why, more_cff = system_fonts()
    print(f"{len(system)} fonts of this machine: {', '.join(f'{k} {v}' for k, v in sorted(why.items()))}, "
          f"and {len(more_cff)} more CFF fonts at a few sizes")
    for path in system:
        print(f"  {path}")

    jobs = []
    sizes = range(1, 601) if not arguments.quick else list(range(1, 80)) + list(range(80, 601, 13))
    for font in FONTS:
        for size in sizes:
            jobs.append((font, size, "loop"))
        for size in [0, -0.0, -18, -38, -1000, 0.25, 0.5, 1.5, 7.3, 12.25, 17.7, 18.5, 27.49, 37.999, 38.5, 55.5, 100.3,
                     333.33, 599.9, 1e-7, 1200, 2400]:
            jobs.append((font, size, "loop"))
        for size in ([1, 2, 5, 9, 18, 28, 38, 57, 76, 100, 160, 255, 333, 480, 600] if not arguments.quick else [18, 38, 300]):
            jobs.append((font, size, "512,1024,2048,4096,8192"))
    other_sizes = list(range(1, 65)) + [72, 96, 128, 200, 300, 450, 600] if not arguments.quick else [1, 9, 18, 38, 120, 400]
    for name, font in sorted(extra.items()):
        if name.startswith("refused-"):
            jobs.append((font, 38, "refuse"))
    for font in system + sorted(path for name, path in extra.items() if not name.startswith("refused-")):
        for size in other_sizes:
            jobs.append((font, size, "loop"))
        jobs.append((font, -24, "loop"))
    for font in more_cff:
        for size in [7, 18, 38, 120] if not arguments.quick else [18]:
            jobs.append((font, size, "loop"))
    jobs_file = f"{work}/jobs"
    with open(jobs_file, "w") as file:
        for font, size, sides in jobs:
            file.write(f"{font}\t{size!r}\t{sides}\n")
    result = subprocess.run([program, "sweep", jobs_file, str(arguments.threads)], env=environment, stdout=subprocess.PIPE, text=True)
    print(result.stdout, end="")
    if result.returncode != 0:
        sys.exit(f"exit {result.returncode}")

    seeds = arguments.seeds if not arguments.quick else arguments.seeds // 10
    fuzzed = FONTS + [extra["compound"], extra["cmap12"], extra["cmap6"], extra["cmap0"], extra["cmap4-ranges"], extra["loca-other"]]
    fuzzed += [path for path in system if path.endswith(".otf")][:2]
    for k, font in enumerate(fuzzed):
        result = subprocess.run([program, "fuzz", font, str(k * 1_000_000), str(seeds), child, work, str(arguments.threads)],
                                env=environment, stdout=subprocess.PIPE, text=True)
        print(result.stdout, end="")
        if result.returncode != 0:
            sys.exit(f"exit {result.returncode}")

    if arguments.coverage:
        report_coverage(work, program, child)

def report_coverage(work, program, child):
    # each process of the run left its counts in profiles/
    data = f"{work}/merged.profdata"
    listing = f"{work}/profiles.txt"
    open(listing, "w").write("\n".join(glob.glob(f"{work}/profiles/*.profraw")) + "\n")
    run(["llvm-profdata-18", "merge", "-sparse", "-o", data, "--input-files", listing])
    shown = subprocess.run(["llvm-cov-18", "show", program, "-object", child, "-instr-profile", data,
                            f"{HEADER_DIR}/imstb_truetype.h"], capture_output=True, text=True, check=True).stdout
    lines = open(f"{HEADER_DIR}/imstb_truetype.h").read().split("\n")
    # each ported function's lines, from its definition to the closing brace at column 0
    ranges = []
    for i, line in enumerate(lines):
        for name in PORTED:
            if re.match(rf"(static|STBTT_DEF)\b[^(]*\b{name}\(", line) and not line.rstrip().endswith(";"):
                end = i
                while end < len(lines) and not lines[end].startswith("}"):
                    end += 1
                ranges.append((name, i + 1, end + 1))
    missed = []
    for row in shown.split("\n"):
        parts = row.split("|", 2)
        if len(parts) < 3:
            continue
        try:
            number = int(parts[0].strip())
        except ValueError:
            continue
        count = parts[1].strip()
        if count == "0" or count.startswith("0") and count.strip("0") == "":
            for name, first, last in ranges:
                if first <= number <= last:
                    missed.append((number, name, lines[number - 1].strip()))
    if missed:
        print(f"{len(missed)} lines of the ported functions not reached:")
        for number, name, text in sorted(set(missed)):
            print(f"  {number:5} {name}: {text}")
    else:
        print("every line of the ported functions reached")

main()
