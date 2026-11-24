#!/usr/bin/env python3
"""S14f's evaluation rig: small pixel programs through the translator, their
SPIR-V run by an interpreter, the results held to models written from Xenia.

    shaders/evaluate.py [--src DIR] [--out DIR]

What it checks, unscaled and scaled:

- the clamped and floored scalar ops (rcpc, rcpf, rsqc, rsqf, logc) and the
  plain ones beside them (rcp, rsq, log) at +-0, +-denormals, +-FLT_MIN,
  +-inf, NaN and ordinary values, against the result Xenia's
  ProcessScalarAluOperation (spirv_shader_translator_alu.cc) gives: the
  same IEEE operation, then -inf/+inf replaced as it replaces them. Compared
  by bits, -0 against +0 included, any NaN against any NaN;
- what an instruction switched off leaves: the previous scalar result, the
  predicate and a0, under the instruction's predicate and under a cond_exec
  on a boolean constant, against Xenia's rule (the instruction is skipped:
  UpdateInstructionPredication and the exec's conditional,
  spirv_shader_translator.cc);
- a vertex fetch's read, 8-in-32 data at 920 MB;
- the arena's reads: the record (whose loads the translator places
  afterwards), a boolean constant (the dword view) and a relatively addressed
  pixel constant (the vec4 view) in a 1 GB buffer, past the end of a 544 MB
  one and past 1 GB, against robust buffer access (0 out of range).

The translator is the shader folder as it stands (shaders/src/shader, or
--src DIR), built on its own with evaluate_dump.rs. The interpreter
implements the instructions these programs reach and stops at any other,
naming it, so a program it cannot run fails rather than passes.
"""

import argparse
import ctypes
import math
import os
import shutil
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TOOLCHAIN = os.path.expanduser("~/.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin")


def build(out, src):
    """evaluate_dump.rs with the shader folder `src` beside it, in `out`."""
    rust = os.path.join(out, "rust")
    shutil.rmtree(rust, ignore_errors=True)
    shutil.copytree(src, os.path.join(rust, "shader"))
    shutil.copy(os.path.join(HERE, "evaluate_dump.rs"), os.path.join(rust, "main.rs"))
    program = os.path.join(out, "evaluate_dump")
    env = dict(os.environ)
    if os.path.isdir(TOOLCHAIN):
        env["PATH"] = TOOLCHAIN + os.pathsep + env["PATH"]
    r = subprocess.run(["rustc", "--edition", "2024", "-Copt-level=1", "-Cdebug-assertions=on",
                        "-Coverflow-checks=on", "-Cpanic=abort", "-Ctarget-cpu=x86-64-v2",
                        os.path.join(rust, "main.rs"), "-o", program], capture_output=True, text=True, env=env)
    if r.returncode:
        sys.exit(r.stdout + r.stderr + "failed: rustc evaluate_dump.rs")
    return program

# ---- floats as the GPU holds them ------------------------------------------------------------

FLT_MAX = 3.4028234663852886e38


def f32(x):
    return ctypes.c_float(x).value


def bits_of(x):
    return struct.unpack("<I", struct.pack("<f", x))[0]


def float_of(b):
    return struct.unpack("<f", struct.pack("<I", b & 0xFFFFFFFF))[0]


def fdiv(a, b):
    if b == 0.0:
        if a == 0.0 or math.isnan(a):
            return math.nan
        return math.copysign(math.inf, a) * math.copysign(1.0, b)
    return f32(a / b)


def log2(x):
    if math.isnan(x) or x < 0:
        return math.nan
    if x == 0:
        return -math.inf
    return f32(math.log2(x))


def inverse_sqrt(x):
    if math.isnan(x) or (x < 0):
        return math.nan
    if x == 0:
        return math.copysign(math.inf, x)
    return f32(1.0 / math.sqrt(x))


# ---- the interpreter -------------------------------------------------------------------------

class Unsupported(Exception):
    pass


class Module:
    def __init__(self, words):
        assert words[0] == 0x07230203
        self.types, self.constants, self.variables, self.decorations = {}, {}, {}, {}
        self.blocks, self.order = {}, []
        i = 5
        current = None
        while i < len(words):
            count, op = words[i] >> 16, words[i] & 0xFFFF
            operands = words[i + 1:i + count]
            i += count
            if op == 71:    # Decorate
                self.decorations.setdefault(operands[0], {})[operands[1]] = operands[2:]
            elif op == 19:
                self.types[operands[0]] = ("void",)
            elif op == 20:
                self.types[operands[0]] = ("bool",)
            elif op == 21:
                self.types[operands[0]] = ("int", operands[2])
            elif op == 22:
                self.types[operands[0]] = ("float",)
            elif op == 23:
                self.types[operands[0]] = ("vector", operands[1], operands[2])
            elif op == 28:
                self.types[operands[0]] = ("array", operands[1], self.constants[operands[2]])
            elif op == 29:
                self.types[operands[0]] = ("runtime", operands[1])
            elif op == 30:
                self.types[operands[0]] = ("struct",) + tuple(operands[1:])
            elif op == 32:
                self.types[operands[0]] = ("pointer", operands[1], operands[2])
            elif op in (25, 26, 27):
                self.types[operands[0]] = ("opaque",)
            elif op == 33:
                self.types[operands[0]] = ("function",)
            elif op == 41:
                self.constants[operands[1]] = True
            elif op == 42:
                self.constants[operands[1]] = False
            elif op == 43:
                kind = self.types[operands[0]]
                self.constants[operands[1]] = float_of(operands[2]) if kind[0] == "float" else operands[2]
            elif op == 44:
                self.constants[operands[1]] = tuple(self.constants[c] for c in operands[2:])
            elif op == 59 and current is None:
                self.variables[operands[1]] = (operands[0], operands[2])
            elif op == 248:   # Label
                current = operands[0]
                self.blocks[current] = []
                self.order.append(current)
            elif current is not None:
                self.blocks[current].append((op, operands))

    def location(self, variable):
        return self.decorations.get(variable, {}).get(30, [None])[0]


class Arena:
    """The buffer behind set 2, sparse, one descriptor over the whole of it, as the
    renderer writes it."""

    def __init__(self, size):
        self.words = {}
        self.size = size
        self.descriptors = [(0, size)]

    def read(self, descriptor, element, size):
        if descriptor >= len(self.descriptors):
            raise Unsupported("descriptor %d of %d" % (descriptor, len(self.descriptors)))
        offset, extent = self.descriptors[descriptor]
        at = element * size
        if at + size > extent:
            return None          # robust buffer access: out of the descriptor's range
        first = (offset + at) // 4
        # Bytes past the buffer's end are not the buffer's.
        return [self.words.get(first + k, 0) if (first + k) * 4 < self.size else 0 for k in range(size // 4)]


def run(module, inputs, record, arena, vertex=0):
    """Runs main. `inputs`: location -> float4 tuple; `record`: the flat record
    word, or a vertex shader's BaseVertex; `vertex`: the vertex's number past it.
    Returns (outputs by location, killed)."""
    m = module
    memory = {}
    views = {}
    for var, (pointer_type, storage) in m.variables.items():
        pointee = m.types[pointer_type][2]
        if storage == 2:   # Uniform: the arena's views
            binding = m.decorations[var][33][0]
            views[var] = (binding, m.types[pointee][0] == "array")
        elif storage == 1:
            location = m.location(var)
            built_in = m.decorations.get(var, {}).get(11, [None])[0]
            if built_in == 4424:      # BaseVertex
                memory[var] = record
            elif built_in == 42:      # VertexIndex
                memory[var] = record + vertex
            elif location == 16:
                memory[var] = record
            elif location is not None:
                memory[var] = list(inputs.get(location, (0.0, 0.0, 0.0, 0.0)))
            else:
                raise Unsupported("an input without a location")
        elif storage == 3:
            memory[var] = None      # made below, once zero_of is known
    values = dict(m.constants)
    for var in memory:
        values[var] = (var, ())

    def zero_of(t):
        kind = m.types[t]
        if kind[0] == "float":
            return 0.0
        if kind[0] == "int":
            return 0
        if kind[0] == "bool":
            return False
        if kind[0] == "vector":
            return [zero_of(kind[1]) for _ in range(kind[2])]
        if kind[0] == "array":
            return [zero_of(kind[1]) for _ in range(kind[2])]
        raise Unsupported("a variable of %s" % (kind,))

    for var, (pointer_type, storage) in m.variables.items():
        if storage == 3:
            memory[var] = zero_of(m.types[pointer_type][2])

    def scalar_kind(t):
        kind = m.types[t]
        return m.types[kind[1]] if kind[0] == "vector" else kind

    def each(fn, *args):
        if isinstance(args[0], (list, tuple)):
            return tuple(fn(*xs) for xs in zip(*args))
        return fn(*args)

    def to_signed(x):
        return x - (1 << 32) if x & 0x80000000 else x

    def load(pointer):
        if pointer[0] == "arena":
            _, binding, descriptor, element = pointer
            size = 4 if binding == 0 else 16
            got = arena.read(descriptor, element, size)
            if binding == 0:
                return 0 if got is None else got[0]
            return (0.0,) * 4 if got is None else tuple(float_of(w) for w in got)
        var, path = pointer
        v = memory[var]
        for p in path:
            v = v[p]
        return tuple(v) if isinstance(v, list) else v

    def store(pointer, value):
        var, path = pointer
        if not path:
            memory[var] = list(value) if isinstance(value, tuple) else value
            return
        v = memory[var]
        for p in path[:-1]:
            v = v[p]
        v[path[-1]] = list(value) if isinstance(value, tuple) else value

    block = m.order[0]
    previous = None
    killed = False
    steps = 0
    while True:
        jump = None
        for op, o in m.blocks[block]:
            steps += 1
            if steps > 2_000_000:
                raise Unsupported("does not end")
            if op == 59:     # Variable (Function)
                values[o[1]] = (o[1], ())
                memory[o[1]] = values[o[3]] if len(o) > 3 else zero_of(m.types[o[0]][2])
                if isinstance(memory[o[1]], tuple):
                    memory[o[1]] = list(memory[o[1]])
            elif op == 61:
                values[o[1]] = load(values[o[2]])
            elif op == 62:
                store(values[o[0]], values[o[1]])
            elif op == 65:   # AccessChain
                base = o[2]
                indices = [values[x] for x in o[3:]]
                if base in views:
                    binding, _ = views[base]
                    values[o[1]] = ("arena", binding, 0, indices[1])
                else:
                    var, path = values[base]
                    values[o[1]] = (var, path + tuple(to_signed(i) for i in indices))
            elif op == 79:   # VectorShuffle
                a, b = values[o[2]], values[o[3]]
                both = tuple(a) + tuple(b)
                values[o[1]] = tuple(both[k] for k in o[4:])
            elif op == 80:
                parts = []
                for x in o[2:]:
                    v = values[x]
                    parts.extend(v if isinstance(v, (list, tuple)) else [v])
                values[o[1]] = tuple(parts)
            elif op == 81:
                v = values[o[2]]
                for k in o[3:]:
                    v = v[k]
                values[o[1]] = v
            elif op == 82:
                v = list(values[o[3]])
                v[o[4]] = values[o[2]]
                values[o[1]] = tuple(v)
            elif op in (109, 110):   # ConvertFToU/S
                def convert(x, signed=(op == 110)):
                    if math.isnan(x) or math.isinf(x):
                        raise Unsupported("a conversion of %r" % x)
                    return int(x) & 0xFFFFFFFF
                values[o[1]] = each(convert, values[o[2]])
            elif op == 111:
                values[o[1]] = each(lambda x: f32(float(to_signed(x))), values[o[2]])
            elif op == 112:
                values[o[1]] = each(lambda x: f32(float(x)), values[o[2]])
            elif op == 124:   # Bitcast
                target = scalar_kind(o[0])
                v = values[o[2]]
                if target[0] == "float":
                    values[o[1]] = each(lambda x: x if isinstance(x, float) else float_of(x), v)
                else:
                    values[o[1]] = each(lambda x: bits_of(x) if isinstance(x, float) else x, v)
            elif op == 126:
                values[o[1]] = each(lambda x: (-x) & 0xFFFFFFFF, values[o[2]])
            elif op == 127:
                values[o[1]] = each(lambda x: -x, values[o[2]])
            elif op in (128, 130, 132, 134, 137):
                fn = {128: lambda a, b: (a + b) & 0xFFFFFFFF, 130: lambda a, b: (a - b) & 0xFFFFFFFF,
                      132: lambda a, b: (a * b) & 0xFFFFFFFF, 134: lambda a, b: a // b if b else 0,
                      137: lambda a, b: a % b if b else 0}[op]
                values[o[1]] = each(fn, values[o[2]], values[o[3]])
            elif op in (129, 131, 133, 136):
                fn = {129: lambda a, b: f32(a + b) if not (math.isinf(a) and math.isinf(b) and a != b) else math.nan,
                      131: lambda a, b: f32(a - b) if not (math.isinf(a) and math.isinf(b) and a == b) else math.nan,
                      133: lambda a, b: math.nan if (a == 0 and math.isinf(b)) or (b == 0 and math.isinf(a))
                      else f32(a * b),
                      136: fdiv}[op]
                values[o[1]] = each(fn, values[o[2]], values[o[3]])
            elif op == 142:   # VectorTimesScalar
                s = values[o[3]]
                values[o[1]] = tuple(math.nan if (x == 0 and math.isinf(s)) or (s == 0 and math.isinf(x))
                                     else f32(x * s) for x in values[o[2]])
            elif op in (154, 155):
                v = values[o[2]]
                values[o[1]] = any(v) if op == 154 else all(v)
            elif op in (164, 165, 166, 167):
                fn = {164: lambda a, b: a == b, 165: lambda a, b: a != b, 166: lambda a, b: a or b,
                      167: lambda a, b: a and b}[op]
                values[o[1]] = each(fn, values[o[2]], values[o[3]])
            elif op == 168:
                values[o[1]] = each(lambda a: not a, values[o[2]])
            elif op == 169:   # Select
                c, a, b = values[o[2]], values[o[3]], values[o[4]]
                if isinstance(c, (list, tuple)):
                    values[o[1]] = tuple(x if k else y for k, x, y in zip(c, a, b))
                else:
                    values[o[1]] = a if c else b
            elif 170 <= op <= 179:
                signed = op in (173, 175, 177, 179)
                fn = {170: lambda a, b: a == b, 171: lambda a, b: a != b, 172: lambda a, b: a > b,
                      173: lambda a, b: a > b, 174: lambda a, b: a >= b, 175: lambda a, b: a >= b,
                      176: lambda a, b: a < b, 177: lambda a, b: a < b, 178: lambda a, b: a <= b,
                      179: lambda a, b: a <= b}[op]
                conv = to_signed if signed else (lambda x: x)
                values[o[1]] = each(lambda a, b: fn(conv(a), conv(b)), values[o[2]], values[o[3]])
            elif 180 <= op <= 191:
                base = {180: lambda a, b: a == b, 182: lambda a, b: a != b, 184: lambda a, b: a < b,
                        186: lambda a, b: a > b, 188: lambda a, b: a <= b, 190: lambda a, b: a >= b}[op & ~1]
                ordered = op % 2 == 0

                def compare(a, b, base=base, ordered=ordered):
                    if math.isnan(a) or math.isnan(b):
                        return not ordered
                    return base(a, b)
                values[o[1]] = each(compare, values[o[2]], values[o[3]])
            elif op in (194, 195, 196, 197, 198, 199):
                fn = {194: lambda a, b: a >> b, 195: lambda a, b: (to_signed(a) >> b) & 0xFFFFFFFF,
                      196: lambda a, b: (a << b) & 0xFFFFFFFF, 197: lambda a, b: a | b, 198: lambda a, b: a ^ b,
                      199: lambda a, b: a & b}[op]
                values[o[1]] = each(fn, values[o[2]], values[o[3]])
            elif op == 200:
                values[o[1]] = each(lambda a: (~a) & 0xFFFFFFFF, values[o[2]])
            elif op == 338:   # GroupNonUniformBroadcastFirst: one invocation here
                values[o[1]] = values[o[3]]
            elif op == 12:    # ExtInst GLSL.std.450
                instruction, args = o[3], [values[x] for x in o[4:]]
                fns = {
                    3: lambda x: math.copysign(float(math.trunc(x)), x) if math.isfinite(x) else x,
                    4: abs,
                    8: lambda x: float(math.floor(x)) if math.isfinite(x) else x,
                    10: lambda x: f32(x - math.floor(x)) if math.isfinite(x) else math.nan,
                    29: lambda x: f32(2.0 ** x) if x < 129 else math.inf,
                    30: log2,
                    31: lambda x: math.nan if x < 0 or math.isnan(x) else f32(math.sqrt(x)),
                    32: inverse_sqrt,
                    37: lambda a, b: min(a, b),
                    40: lambda a, b: max(a, b),
                    43: lambda x, lo, hi: min(max(x, lo), hi),
                    50: lambda a, b, c: f32(a * b + c),
                    81: lambda x, lo, hi: lo if math.isnan(x) else min(max(x, lo), hi),
                }
                if instruction not in fns:
                    raise Unsupported("GLSL.std.450 instruction %d" % instruction)
                values[o[1]] = each(fns[instruction], *args)
            elif op == 245:   # Phi
                pairs = list(zip(o[2::2], o[3::2]))
                values[o[1]] = next(values[v] for v, b in pairs if b == previous)
            elif op in (246, 247):
                pass
            elif op == 249:
                jump = o[0]
                break
            elif op == 250:
                jump = o[1] if values[o[0]] else o[2]
                break
            elif op == 251:
                selector = values[o[0]]
                jump = o[1]
                for k in range(2, len(o), 2):
                    if o[k] == selector:
                        jump = o[k + 1]
                break
            elif op == 252:   # Kill
                killed = True
                jump = "end"
                break
            elif op == 253:
                jump = "end"
                break
            elif op in (54, 56, 5, 6):
                pass
            else:
                raise Unsupported("opcode %d" % op)
        if jump == "end":
            break
        if jump is None:
            raise Unsupported("a block without a terminator")
        previous, block = block, jump
    colours = {}
    for var, (pointer_type, storage) in m.variables.items():
        if storage == 3:
            v = memory[var]
            colours[m.location(var)] = tuple(v) if isinstance(v, list) else v
    return colours, killed


# ---- microcode -------------------------------------------------------------------------------

def cf_word(opcode, address=0, count=0, sequence=0, bool_address=0, condition=0):
    word0 = (address & 0xFFF) | (count & 7) << 12 | (sequence & 0xFFF) << 16
    word1 = (bool_address & 0xFF) << 2 | (condition & 1) << 10 | (opcode & 0xF) << 12
    return word0, word1


def pack_cf(flow):
    if len(flow) % 2:
        flow = flow + [cf_word(0)]
    words = []
    for (a0, a1), (b0, b1) in zip(flow[0::2], flow[1::2]):
        words += [a0, (a1 & 0xFFFF) | (b0 << 16) & 0xFFFFFFFF, (b0 >> 16) | (b1 << 16) & 0xFFFFFFFF]
    return words


def swizzle_for(x_component, y_component):
    """Source 3's swizzle that gives a scalar op x = r.<x_component> (slot 3)
    and y = r.<y_component> (slot 0)."""
    return ((x_component - 3) & 3) << 6 | (y_component & 3)


def alu(scalar, *, register=0, temp=True, swizzle=0x40, mask=0, export=False, dest=0, predicated=False,
        condition=True, a0_relative=False):
    w0 = (dest & 0x3F) | (dest & 0x3F) << 8 | (0x8000 if export else 0) | (mask & 0xF) << 20 | (scalar & 0x3F) << 26
    w1 = swizzle & 0xFF
    if predicated:
        w1 |= 1 << 28 | (1 << 27 if condition else 0)
    if a0_relative:
        w1 |= 1 << 29 | 1 << 31
    # Sources 1 and 2 temporaries, so source 3's constant is addressed by bit 31.
    w2 = (register & 0xFF) | (1 << 29 if temp else 0) | 1 << 31 | 1 << 30
    return [w0, w1, w2]


def program(groups):
    """`groups`: [(kind, instructions)], kind 'exec' or ('cond', bool, condition):
    each its own exec (the last one an end), at most six instructions each."""
    flow, body = [], []
    first = (len(groups) + 1) // 2      # control flow pairs come first
    at = first
    for n, (kind, instructions) in enumerate(groups):
        last = n + 1 == len(groups)
        if kind == "exec":
            flow.append(cf_word(2 if last else 1, at, len(instructions)))
        else:
            _, index, condition = kind
            flow.append(cf_word(4 if last else 3, at, len(instructions), bool_address=index, condition=condition))
        for i in instructions:
            body += i
        at += len(instructions)
    return pack_cf(flow) + body


# Scalar opcodes (ucode.rs scalar_op).
MAXS, LOGC, LOG, RCPC, RCPF, RCP, RSQC, RSQF, RSQ = 5, 15, 16, 17, 18, 19, 20, 21, 22
MAX_AS, SETP_EQ, SETP_CLR, RETAIN_PREV = 23, 27, 33, 50

# ---- the cases and their models --------------------------------------------------------------

EDGES = [0x00000000, 0x80000000, 0x00000001, 0x80000001, 0x007FFFFF, 0x807FFFFF, 0x00800000, 0x80800000,
         0x7F800000, 0xFF800000, 0x7FC00000, 0xFFC00000, 0x3F800000, 0xBF800000, 0x40000000, 0x3F000000,
         0x40400000, 0xC0E80000, 0x0DA24260, 0x7149F2CA, 0x7F7FFFFF, 0xFF7FFFFF]


def xenia(op, x):
    """ProcessScalarAluOperation's result for one op (spirv_shader_translator_alu.cc,
    without ReduceFloatPrecision, which S14f does not take: see the ledger)."""
    if op in (RCP, RCPC, RCPF):
        r = fdiv(1.0, x)
    elif op in (RSQ, RSQC, RSQF):
        r = inverse_sqrt(x)
    else:
        r = log2(x)
    if op == LOGC and r == -math.inf:
        return -FLT_MAX
    if op in (RCPC, RSQC):
        if r == -math.inf:
            r = -FLT_MAX
        if r == math.inf:
            r = FLT_MAX
    if op in (RCPF, RSQF):
        if r == math.inf:
            r = 0.0
        if r == -math.inf:
            r = -0.0
    return r


RECORD = 0x400          # the record's dword
PIXEL_FLOATS = 75 * 1024 * 1024    # dwords: 300 MB in
BOOLS = 175 * 1024 * 1024          # 700 MB in
# A buffer of 544 MB (a 32 MB arena and the shadow), the constants at 600 MB: past
# its end.
SHORT = (544 << 20, 150 * 1024 * 1024)
# The constants and the booleans at 1.25 GB, past what the shaders address: past the
# whole buffer's descriptor.
BEYOND = 320 * 1024 * 1024


def arena_for(kind=None):
    size, pixel_floats, bools = {None: (1 << 30, PIXEL_FLOATS, BOOLS), "short": SHORT + (BOOLS,),
                                 "beyond": (1 << 30, BEYOND, BEYOND + 4096)}[kind]
    arena = Arena(size)
    w = arena.words
    w[RECORD + 2] = bools                 # RECORD_OFFSETS + BOOL_CONSTANTS
    w[RECORD + 4] = pixel_floats          # RECORD_OFFSETS + PIXEL_FLOATS
    w[RECORD + 9] = 7                     # alpha function: always
    w[RECORD + 11] = bits_of(1.0)         # colour scale
    w[RECORD + 12] = 0                    # not the gamma form
    w[bools] = 0b10                       # boolean constant 1 true, 0 false
    for k in range(256):                  # c[k] = (k + 0.5, -k, 2k, 1)
        for c, v in enumerate((k + 0.5, -float(k), 2.0 * k, 1.0)):
            w[pixel_floats + 4 * k + c] = bits_of(f32(v))
    return arena


def cases():
    """(name, microcode, inputs, model, arena) -- the model gives oC0.x; the
    arena None for the whole 1 GB, else 'short' or 'beyond'."""
    out = []
    for op in (RCP, RCPC, RCPF, RSQ, RSQC, RSQF, LOG, LOGC):
        code = program([("exec", [alu(op, swizzle=swizzle_for(0, 0), mask=0xF, export=True, dest=0)])])
        for e in EDGES:
            x = float_of(e)
            out.append(("op%d(%08x)" % (op, e), code, {0: (x, 0.0, 0.0, 0.0)}, xenia(op, x), None))

    # The previous scalar result under a predicate: setp_eq r0.x; maxs r0.y (ps = y);
    # (p) maxs r0.z (ps = z only where p holds); retain_prev into oC0.
    ps_code = program([("exec", [
        alu(SETP_EQ, swizzle=swizzle_for(0, 0)),
        alu(MAXS, swizzle=swizzle_for(1, 1)),
        alu(MAXS, swizzle=swizzle_for(2, 2), predicated=True, condition=True),
        alu(RETAIN_PREV, mask=0xF, export=True, dest=0),
    ])])
    # The predicate under a predicate: setp_eq r0.x; (p) setp_eq r0.y (p = y == 0 only
    # where p held); maxs r0.z; (p) maxs r0.w; retain_prev: w where p, else z.
    p_code = program([("exec", [
        alu(SETP_EQ, swizzle=swizzle_for(0, 0)),
        alu(SETP_EQ, swizzle=swizzle_for(1, 1), predicated=True, condition=True),
        alu(MAXS, swizzle=swizzle_for(2, 2)),
        alu(MAXS, swizzle=swizzle_for(3, 3), predicated=True, condition=True),
        alu(RETAIN_PREV, mask=0xF, export=True, dest=0),
    ])])
    # setp_clr inside an open instruction-level check: setp_eq r0.x; maxs r0.y;
    # (p) setp_clr; (p) maxs r0.z; retain_prev. The translator's rule: every
    # predicated instruction reads the predicate as last stored, so where x == 0
    # setp_clr runs (ps = FLT_MAX, p false) and (p) maxs is off: FLT_MAX; else y.
    # Xenia's kSetpClr leaves predicate_written unset, so it keeps the second
    # instruction in the check opened by the first and gives z where x == 0 -- the
    # named deviation in docs/shaders.md (no game shader reaches it); the model here
    # is the translator's rule.
    clr_code = program([("exec", [
        alu(SETP_EQ, swizzle=swizzle_for(0, 0)),
        alu(MAXS, swizzle=swizzle_for(1, 1)),
        alu(SETP_CLR, predicated=True, condition=True),
        alu(MAXS, swizzle=swizzle_for(2, 2), predicated=True, condition=True),
        alu(RETAIN_PREV, mask=0xF, export=True, dest=0),
    ])])
    # a0 under a predicate: maxas r0.y (a0 = round y); setp_eq r0.x; (p) maxas r0.z;
    # maxs c[a0 + 0].x into oC0: c[k].x is k + 0.5.
    a0_code = program([("exec", [
        alu(MAX_AS, swizzle=swizzle_for(1, 1)),
        alu(SETP_EQ, swizzle=swizzle_for(0, 0)),
        alu(MAX_AS, swizzle=swizzle_for(2, 2), predicated=True, condition=True),
        alu(MAXS, register=0, temp=False, a0_relative=True, swizzle=swizzle_for(0, 0), mask=0xF, export=True,
            dest=0),
    ])])
    # The previous scalar result under a cond_exec on boolean constant 0 (false) or 1
    # (true): maxs r0.y; cond_exec b<n>: maxs r0.z; exec: retain_prev.
    cond_codes = {n: program([
        ("exec", [alu(MAXS, swizzle=swizzle_for(1, 1))]),
        (("cond", n, 1), [alu(MAXS, swizzle=swizzle_for(2, 2))]),
        ("exec", [alu(RETAIN_PREV, mask=0xF, export=True, dest=0)]),
    ]) for n in (0, 1)}
    # A constant past the buffer's end: maxs c[a0 + 0] with a0 set so the vec4 index is
    # PAST, which no descriptor reaches.
    for x in (0.0, 1.0):
        for y, z in ((2.0, 3.0), (5.0, 7.0)):
            out.append(("ps x=%g y=%g z=%g" % (x, y, z), ps_code, {0: (x, y, z, 0.0)}, z if x == 0 else y, None))
            out.append(("setp_clr then (p) x=%g y=%g z=%g" % (x, y, z), clr_code, {0: (x, y, z, 0.0)},
                        FLT_MAX if x == 0 else y, None))
            for w in (11.0,):
                p_after = (x == 0) and (y == 0)
                out.append(("p x=%g y=%g" % (x, y), p_code, {0: (x, y, z, w)}, w if p_after else z, None))
                out.append(("p x=%g y=0" % x, p_code, {0: (x, 0.0, z, w)}, w if x == 0 else z, None))
        for y, z in ((3.0, 9.0), (100.0, 4.0)):
            a0 = round(z) if x == 0 else round(y)
            out.append(("a0 x=%g y=%g z=%g" % (x, y, z), a0_code, {0: (x, y, z, 0.0)}, a0 + 0.5, None))
            # Past the buffer: robust buffer access's 0.
            out.append(("past the buffer x=%g y=%g z=%g" % (x, y, z), a0_code, {0: (x, y, z, 0.0)}, 0.0, "short"))
    for n in (0, 1):
        out.append(("cond_exec b%d" % n, cond_codes[n], {0: (0.0, 2.0, 3.0, 0.0)}, 3.0 if n else 2.0, None))
        # Past 1 GB the boolean reads 0 (robust access
        # past a whole buffer), so the block does not run.
        out.append(("cond_exec b%d beyond 1 GB" % n, cond_codes[n], {0: (0.0, 2.0, 3.0, 0.0)}, 2.0, "beyond"))
    out.append(("constant beyond 1 GB", a0_code, {0: (1.0, 3.0, 4.0, 0.0)}, 0.0, "beyond"))
    return out


VERTEX_FETCHES = 0x2000         # the fetch constants' dword
VERTEX_DATA = 230 * 1024 * 1024    # 920 MB in


def vertex_program():
    """vfetch r1 = slot 0's 32_32_32_32_FLOAT at r0.x, 4 dwords a vertex; then
    max r1, r1 exported to interpolator 0."""
    fetch = [0 | 1 << 12 | 1 << 19, (0 | 1 << 3 | 2 << 6 | 3 << 9) | 38 << 16, 4]
    export = [0 | 0x8000 | 0xF << 16 | RETAIN_PREV << 26, 0, 1 << 16 | 1 << 8 | 1 << 31 | 1 << 30 | 2 << 24]
    flow = [cf_word(2, 1, 2, sequence=0b01)]
    return pack_cf(flow) + fetch + export


def vertex_value(n, c):
    return f32(n * 10.0 + c + 0.25)


def vertex_arena():
    arena = arena_for()
    w = arena.words
    w[RECORD + 1] = VERTEX_FETCHES                     # RECORD_OFFSETS + FETCH_CONSTANTS
    w[VERTEX_FETCHES] = VERTEX_DATA << 2 | 3           # the address in dwords, type 3
    w[VERTEX_FETCHES + 1] = 2                          # 8-in-32
    for n in range(8):
        for c in range(4):
            w[VERTEX_DATA + 4 * n + c] = int.from_bytes(struct.pack(">f", vertex_value(n, c)), "little")
    return arena


def same(a, b):
    if math.isnan(a) or math.isnan(b):
        return math.isnan(a) and math.isnan(b)
    return bits_of(a) == bits_of(b)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--src", default=os.path.join(ROOT, "shaders", "src", "shader"))
    parser.add_argument("--out", default=os.path.join(ROOT, "out", "shader_evaluate"))
    args = parser.parse_args()
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    program_path = build(os.path.join(out, "build"), os.path.abspath(args.src))

    all_cases = cases()
    codes = []
    for _, code, _, _, _ in all_cases:
        if code not in codes:
            codes.append(code)
    programs = os.path.join(out, "programs")
    os.makedirs(programs, exist_ok=True)
    listing = []
    for n, code in enumerate(codes + [vertex_program()]):
        path = os.path.join(programs, "%s_%03d.ucode" % ("pixel" if n < len(codes) else "vertex", n))
        with open(path, "wb") as f:
            f.write(b"".join(struct.pack(">I", w) for w in code))
        listing.append(path)
    list_path = os.path.join(out, "list.txt")
    with open(list_path, "w") as f:
        f.write("".join(p + "\n" for p in listing))
    dump = os.path.join(out, "dump")
    for name in os.listdir(dump) if os.path.isdir(dump) else []:
        os.remove(os.path.join(dump, name))
    os.makedirs(dump, exist_ok=True)
    r = subprocess.run([program_path, list_path, dump], capture_output=True, text=True)
    if r.returncode:
        print(r.stdout + r.stderr)
        return 1

    failures = 0
    checked = 0
    for scaled in (0, 1):
        modules = {}
        for n in range(len(codes)):
            with open(os.path.join(dump, "%06d_pixel_%d.spv" % (n, scaled)), "rb") as f:
                data = f.read()
            modules[n] = Module(list(struct.unpack("<%dI" % (len(data) // 4), data)))
        arenas = {kind: arena_for(kind) for kind in (None, "short", "beyond")}
        with open(os.path.join(dump, "%06d_vertex_%d.spv" % (len(codes), scaled)), "rb") as f:
            data = f.read()
        vertex_module = Module(list(struct.unpack("<%dI" % (len(data) // 4), data)))
        for n in (0, 5):
            outputs, _ = run(vertex_module, {}, RECORD, vertex_arena(), vertex=n)
            got = outputs[0]
            checked += 1
            if not all(same(g, vertex_value(n, c)) for c, g in enumerate(got)):
                failures += 1
                print("DIFFERENCE: vertex fetch of vertex %d, %s: got %r, the data %r"
                      % (n, "scaled" if scaled else "unscaled", got,
                         [vertex_value(n, c) for c in range(4)]))
        for name, code, inputs, model, kind in all_cases:
            colours, killed = run(modules[codes.index(code)], inputs, RECORD, arenas[kind])
            got = colours[0][0]
            checked += 1
            if killed or not same(got, model):
                failures += 1
                if failures <= 20:
                    print("DIFFERENCE: %s, %s: got %r (%08x)%s, Xenia %r (%08x)"
                          % (name, "scaled" if scaled else "unscaled", got, bits_of(got),
                             " killed" if killed else "", model, bits_of(model)))
    print("evaluate: %d programs, %d cases and 2 vertex fetches x 2 scales = %d results: %s"
          % (len(codes) + 1, len(all_cases), checked, "all as the models (Xenia's rules, and setp_clr's named deviation)" if not failures else "%d DIFFER" % failures))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
