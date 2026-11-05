#!/usr/bin/env python3
# Compares the game's gunzip (src/runtime/inflate.rs) with zlib's, which it replaced (see
# inflate_diff.rs): on the XLAST of every title image in mw2/ and mw2/tu0/, on gzip streams
# made here from varied data at every level and with every header field, on deflate streams
# built by hand with dynamic tables good and bad (complete, one code, incomplete,
# over-subscribed, none, too many symbols) and a block of type 3 now and then, and on
# damaged copies of each. The Rust is built with overflow checks and debug_assert! on.
#
#   usage: inflate_diff.py <build folder> [--seeds N] [--src folder] [--work folder]
#
# --src takes the Rust from another folder, to try a deliberate mistake. Exits nonzero at
# the first difference, leaving the stream beside the corpus.
import argparse, glob, gzip, os, random, struct, subprocess, sys, zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def be32(data, at):
    return struct.unpack(">I", data[at:at + 4])[0]

# Each image's XLAST, as presence.rs finds it in its XDBF.
def xlasts():
    found = {}
    for path in sorted(glob.glob(f"{ROOT}/mw2/*.pe") + glob.glob(f"{ROOT}/mw2/tu0/*.pe")):
        data = open(path, "rb").read()
        at = data.find(b"XDBF")
        while at >= 0:
            spa = data[at:]
            entries_max, entries, free_max = be32(spa, 8), be32(spa, 12), be32(spa, 16)
            base = 24 + entries_max * 18 + free_max * 8
            for i in range(min(entries, 4096)):
                entry = spa[24 + i * 18:42 + i * 18]
                if entry[:2] == b"\0\x01" and entry[6:10] == b"XSRC":
                    section = spa[base + be32(entry, 10):]
                    name = be32(section, 12)
                    size, packed = be32(section, 16 + name), be32(section, 20 + name)
                    found[section[24 + name:24 + name + packed]] = size
            at = data.find(b"XDBF", at + 1)
    return found

# A gzip member by hand, so that every header field can be there.
def member(data, level, flags, rng):
    deflate = zlib.compressobj(level, zlib.DEFLATED, -15)
    body = deflate.compress(data) + deflate.flush()
    header = bytearray(b"\x1f\x8b\x08" + bytes([flags]) + rng.randbytes(4) + bytes([rng.randrange(256), rng.randrange(256)]))
    if flags & 0x04:
        extra = rng.randbytes(rng.randrange(0, 40))
        header += struct.pack("<H", len(extra)) + extra
    if flags & 0x08:
        header += b"name-" + str(rng.randrange(1000)).encode() + b"\0"
    if flags & 0x10:
        header += b"a comment\0"
    if flags & 0x02:
        header += struct.pack("<H", zlib.crc32(bytes(header)) & 0xFFFF)
    return bytes(header) + body + struct.pack("<II", zlib.crc32(data), len(data) & 0xFFFFFFFF)

def synthetic():
    rng = random.Random(6)
    samples = [b"", b"a", bytes(range(256)) * 40, b"<LocalizedString id=\"1\">text</LocalizedString>\n" * 300,
               rng.randbytes(5000), rng.randbytes(70000), bytes(100000), (b"abcabcabd" * 9000)[:65537]]
    # Text that repeats at long distances, for the far end of the window.
    words = [rng.randbytes(rng.randrange(3, 12)) for _ in range(300)]
    samples.append(b"".join(rng.choice(words) for _ in range(20000)))
    streams = {}
    for i, data in enumerate(samples):
        for level in range(10):
            flags = [0, 0x08, 0x02 | 0x04 | 0x08 | 0x10, 0x01][(i + level) % 4]
            streams[member(data, level, flags, rng)] = len(data)
        streams[gzip.compress(data, compresslevel=9, mtime=0)] = len(data)
    return streams

class BitWriter:
    def __init__(self):
        self.bits = []
    def put(self, value, count):           # least significant bit first
        self.bits += [(value >> i) & 1 for i in range(count)]
    def code(self, code, length):          # Huffman codes go most significant bit first
        self.bits += [(code >> (length - 1 - i)) & 1 for i in range(length)]
    def align(self):
        self.bits += [0] * (-len(self.bits) % 8)
    def bytes(self):
        out = bytearray((len(self.bits) + 7) // 8)
        for i, bit in enumerate(self.bits):
            out[i // 8] |= bit << (i % 8)
        return bytes(out)

def canonical(lengths):
    codes, code, count = {}, 0, [0] * 16
    for l in lengths:
        count[l] += 1
    count[0], next_code = 0, [0] * 16
    for bits in range(1, 16):
        code = (code + count[bits - 1]) << 1
        next_code[bits] = code
    for symbol, l in enumerate(lengths):
        if l:
            codes[symbol] = (next_code[l], l)
            next_code[l] += 1
    return codes

def huffman_lengths(rng, n, limit):
    # A complete code over n symbols, at most `limit` bits: halve random weights until it fits.
    weights = [rng.randrange(1, 1000) for _ in range(n)]
    while True:
        import heapq
        heap = [(w, i, [i]) for i, w in enumerate(weights)]
        heapq.heapify(heap)
        depth = [0] * n
        if n == 1:
            return [1]
        while len(heap) > 1:
            a, b = heapq.heappop(heap), heapq.heappop(heap)
            for s in a[2] + b[2]:
                depth[s] += 1
            heapq.heappush(heap, (a[0] + b[0], min(a[1], b[1]), a[2] + b[2]))
        if max(depth) <= limit:
            return depth
        weights = [max(1, w // 2) for w in weights]

# A set of lengths for `count` symbols: complete, one code of one bit, incomplete,
# over-subscribed, or none.
def lengths_set(rng, count, used):
    kind = rng.choice(["complete"] * 5 + ["one", "incomplete", "over", "none"])
    lengths = [0] * count
    if kind == "none":
        return lengths
    if kind == "one":
        lengths[rng.choice(used)] = 1
        return lengths
    symbols = sorted(set(used + rng.sample(range(count), min(count, rng.randrange(1, 40)))))
    for symbol, l in zip(symbols, huffman_lengths(rng, len(symbols), 15)):
        lengths[symbol] = l
    if kind == "incomplete":
        lengths[max(symbols, key=lambda s: lengths[s])] = 0 if len(symbols) > 2 else 2
    if kind == "over":
        lengths[rng.choice(symbols)] = 1
    return lengths

# A raw deflate stream of dynamic blocks with tables good and bad, then literals, matches
# near and far, and the end; or now and then a block of type 3.
LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258]

# Returns the stream and what it was written to say (None where that is not known): the
# bytes a decoder that took every table and block given would give, whose trailer shows up
# a decoder taking what zlib refuses.
def crafted_deflate(rng):
    w = BitWriter()
    meant = bytearray()
    known = True
    blocks = rng.randrange(1, 3)
    for block in range(blocks):
        w.put(int(block == blocks - 1), 1)
        if rng.randrange(20) == 0:
            # Type 3, followed now and then by what would read as a stored block.
            w.put(3, 2)
            if rng.randrange(2):
                w.align()
                length = rng.randrange(0, 20)
                w.put(length, 16)
                w.put(~length & 0xFFFF, 16)
                for _ in range(length):
                    byte = rng.randrange(256)
                    w.put(byte, 8)
                    meant.append(byte)
            else:
                w.put(rng.getrandbits(32), 32)
                known = False
            continue
        if rng.randrange(6) == 0:
            # A fixed block, with now and then the codes only it has and zlib refuses:
            # lengths 286 and 287, distances 30 and 31.
            w.put(1, 2)
            fixed = canonical([8] * 144 + [9] * 112 + [7] * 24 + [8] * 8)
            written = 0
            for _ in range(rng.randrange(0, 30)):
                if written and rng.randrange(4) == 0:
                    w.code(*fixed[rng.choice([257, 258, 265, 286, 287] if rng.randrange(4) == 0 else [257, 258, 265])])
                    w.put(0, 1)  # a little of the extra bits, or none
                    w.code(rng.choice([0, 1, 30, 31]) if rng.randrange(4) == 0 else 0, 5)
                    known = False
                    written += 3
                else:
                    byte = rng.randrange(256)
                    w.code(*fixed[byte])
                    meant.append(byte)
                    written += 1
            w.code(*fixed[256])
            continue
        w.put(2, 2)
        # One table in ten too long for zlib.
        hlit = rng.randrange(30, 32) if rng.randrange(20) == 0 else rng.choice([rng.randrange(0, 30), 29])
        hdist = rng.randrange(30, 32) if rng.randrange(20) == 0 else rng.choice([rng.randrange(0, 30), 29])
        nlit, ndist = 257 + hlit, 1 + hdist
        lit = lengths_set(rng, nlit, [256] + rng.sample(range(256), 5) + ([257 + rng.randrange(min(29, hlit))] if hlit else []))
        dist = lengths_set(rng, ndist, [rng.randrange(ndist)])
        all_lengths = lit + dist
        order = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]
        used = sorted(set(all_lengths) | {16, 17, 18})
        cl = [0] * 19
        for symbol, l in zip(used, huffman_lengths(rng, len(used), 7)):
            cl[symbol] = l
        # Now and then a code-length code zlib refuses: one code of one bit, over-subscribed,
        # incomplete, or none at all.
        bad = rng.randrange(12)
        if bad == 0:
            cl = [0] * 19
            cl[rng.choice(used)] = 1
        elif bad == 1:
            cl[rng.choice(used)] = 1
        elif bad == 2 and len(used) > 2:
            cl[max(used, key=lambda s: cl[s])] = 0
        elif bad == 3:
            cl = [0] * 19
        hclen = max((i for i in range(19) if cl[order[i]]), default=-1) + 1
        hclen = max(hclen, 4)
        w.put(hlit, 5); w.put(hdist, 5); w.put(hclen - 4, 4)
        for i in range(hclen):
            w.put(cl[order[i]], 3)
        clc = canonical(cl)
        if not clc:
            known = False
            continue
        i = 0
        # Now and then a repeat of the length before the first, or one past the end.
        if rng.randrange(15) == 0 and 16 in clc:
            w.code(*clc[16]); w.put(0, 2)
        while i < len(all_lengths):
            l = all_lengths[i]
            run = 1
            while i + run < len(all_lengths) and all_lengths[i + run] == l and run < 138:
                run += 1
            if l == 0 and run >= 11 and 18 in clc:
                w.code(*clc[18]); w.put(run - 11, 7); i += run
            elif l == 0 and run >= 3 and 17 in clc:
                w.code(*clc[17]); w.put(min(run, 10) - 3, 3); i += min(run, 10)
            elif i > 0 and all_lengths[i - 1] == l and run >= 3 and 16 in clc:
                w.code(*clc[16]); w.put(min(run, 6) - 3, 2); i += min(run, 6)
            else:
                w.code(*clc[l] if l in clc else next(iter(clc.values()))); i += 1
        if rng.randrange(15) == 0 and 18 in clc:
            w.code(*clc[18]); w.put(127, 7)
        litc, distc = canonical(lit), canonical(dist)
        written = 0
        for _ in range(rng.randrange(0, 60)):
            literals = [s for s in litc if s < 256]
            matches = [s for s in litc if s > 256]
            if matches and distc and written and rng.randrange(3) == 0:
                s = rng.choice(matches)
                w.code(*litc[s])
                extra = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0, 0, 0][min(s - 257, 30)]
                extra_value = rng.getrandbits(extra) if extra else 0
                w.put(extra_value, extra)
                length = LENGTH_BASE[s - 257] + extra_value if s - 257 < 29 else None
                # Mostly a distance within what is written, now and then one too far.
                base = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 1 << 16, 1 << 16]
                near = [d for d in distc if base[min(d, 31)] <= written]
                d = rng.choice(near) if near and rng.randrange(6) else rng.choice(list(distc))
                w.code(*distc[d])
                dextra = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 0, 0][min(d, 31)]
                dextra_value = 0 if d in near else (rng.getrandbits(dextra) if dextra else 0)
                w.put(dextra_value, dextra)
                distance = base[min(d, 31)] + dextra_value
                if length is None or d >= 30 or distance > len(meant):
                    known = False
                else:
                    for _ in range(length):
                        meant.append(meant[-distance])
                written += 3
            elif literals:
                byte = rng.choice(literals)
                w.code(*litc[byte])
                meant.append(byte)
                written += 1
        if 256 in litc:
            w.code(*litc[256])
        else:
            known = False
    return w.bytes(), (bytes(meant) if known else None)

def crafted():
    rng = random.Random(7)
    streams = {}
    for i in range(600):
        raw, meant = crafted_deflate(rng)
        try:
            inflater = zlib.decompressobj(-15)
            data = inflater.decompress(raw)
            trailer = struct.pack("<II", zlib.crc32(data), len(data) & 0xFFFFFFFF) if inflater.eof else rng.randbytes(8)
            size = len(data)
        except zlib.error:
            trailer, size = rng.randbytes(8), rng.randrange(0, 2000)
            if meant is not None and rng.randrange(2):
                # What a decoder that took it would give, so that one stands out.
                trailer, size = struct.pack("<II", zlib.crc32(meant), len(meant)), len(meant)
        flags = rng.choice([0, 0x04, 0x04 | 0x02, 0x02, 0x08, 0x10 | 0x04])
        header = bytearray(b"\x1f\x8b\x08" + bytes([flags]) + bytes(6))
        if flags & 0x04:
            extra = rng.randbytes(rng.randrange(0, 8))
            header += struct.pack("<H", len(extra)) + extra
        if flags & 0x08:
            header += b"n\0"
        if flags & 0x10:
            header += b"c\0"
        if flags & 0x02:
            header += struct.pack("<H", zlib.crc32(bytes(header)) & 0xFFFF)
        # Anything after the trailer, which both leave alone.
        streams[bytes(header) + raw + trailer + rng.randbytes(rng.randrange(0, 3))] = size
    return streams

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--seeds", type=int, default=2000)
    parser.add_argument("--src", default=f"{ROOT}/src")
    parser.add_argument("--work")
    arguments = parser.parse_args()
    build = os.path.abspath(arguments.build)
    work = arguments.work or f"{build}/inflate_diff"
    os.makedirs(work, exist_ok=True)
    libraries = glob.glob(f"{build}/_deps/zlib-build/libz*.a")
    if not libraries:
        sys.exit(f"{build} has no zlib: build it first")

    made = crafted()
    streams = {**xlasts(), **synthetic(), **made}
    print(f"{len(xlasts())} XLASTs, {len(made)} crafted, {len(streams)} streams in all")
    corpus = f"{work}/corpus"
    with open(corpus, "wb") as file:
        for packed, size in streams.items():
            file.write(struct.pack("<II", size, len(packed)) + packed)

    source = f"{work}/inflate_diff.rs"
    text = open(f"{ROOT}/tools/inflate_diff.rs").read()
    open(source, "w").write(text.replace('#[path = "../src/runtime/inflate.rs"]', f'#[path = "{arguments.src}/runtime/inflate.rs"]'))
    program = f"{work}/inflate_diff"
    subprocess.run(["rustc", "--edition=2024", "-Copt-level=2", "-Cdebug-assertions=on", "-Coverflow-checks=on",
                    "-Cpanic=abort", "-o", program, source, "-L", os.path.dirname(libraries[0]), "-l", "static=z"], check=True)
    result = subprocess.run([program, corpus, str(arguments.seeds)], text=True, stdout=subprocess.PIPE, timeout=3600)
    print(result.stdout, end="")
    if result.returncode != 0:
        sys.exit(f"exit {result.returncode}")

main()
