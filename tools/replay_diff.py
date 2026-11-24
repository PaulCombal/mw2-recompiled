#!/usr/bin/env python3
"""Two executables replay one capture of the GPU's input; what they did must agree.

    tools/replay_diff.py <capture> <exe A> <exe B> [--env K=V]... [--env-b K=V]...
                         [--frames a:b] [--seconds n] [--keep DIR]

Each side runs MW2_REPLAY_GPU=<capture> (src/runtime/gpu_replay.rs) with
MW2_DETERMINISTIC=1, the command trace, frame hashes, the report and the shader
dump, through diagnosis/port/recipes/agent_run.sh: memory-capped, the window shown,
one copy at a time, the kernel log checked for GPU faults afterwards. --env is set
on both sides, --env-b on B alone (a forced path, a planted mistake's switch).

Compared: every frame hash; the command traces up to their `shutdown` line exactly
(the first difference is printed with 20 lines either side) and the destroy lines
after it as a sorted multiset, since both sides destroy in hash-map order; the
builtin_*.spv files; the report's counters; the "draws skipped" lines as a set,
whose order breaks ties by a string literal's address. --frames a:b (a >= 1)
limits the hashes to the images hashed as frames a to b-1 and the trace to the
commands that drew them: the trace's frame k ends with the marker `frame n=k+1`,
so they are its frames a-1 to b-2.

Exit 0 only when everything is identical and both sides report 0 replay
divergences, 0 wait timeouts, 0 unregistered handles and a replay that finished; 1 when they differ; 2 on
a run that could not be made; 97/98 as agent_run.sh (a GPU fault: stop everything).
"""
import argparse
import collections
import filecmp
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AGENT_RUN = os.path.join(ROOT, "diagnosis/port/recipes/agent_run.sh")

# The report's counters: a name, the line's pattern, the groups that are counts.
COUNTERS = [
    ("draws", r"gpu: (\d+) draws \("),
    ("frames, recorded draws, resolves", r"renderer: (\d+) frames, (\d+) of (\d+) draws recorded, (\d+) resolves \((\d+) of depth\), (\d+) pipelines over (\d+) shaders"),
    ("swap packets", r"gpu: (\d+) swap packets"),
    ("packets", r"gpu: (\d+) packets; (\d+) of (\d+) indirect buffers"),
    ("textures uploaded", r"textures: (\d+) uploaded \((\d+) KB\), (\d+) failed, (\d+) descriptors written"),
    ("arena: constants", r"constants\s+(\d+) KB copied,\s+(\d+) KB reused"),
    ("arena: vertex data", r"vertex data\s+(\d+) KB copied,\s+(\d+) KB reused"),
    ("arena: indices", r"indices\s+(\d+) KB copied,\s+(\d+) KB reused"),
    ("gathered draws and runs", r"renderer: (\d+) draws made, (\d+) of them in (\d+) runs made with one call each, (\d+) on their own"),
    ("submissions and segments", r"renderer: (\d+) submissions, .* in (\d+) segments more: (\d+) at the (\d+) points"),
    ("resolve images made", r"renderer: (\d+) resolve destination images made"),
    ("image-pool copies", r"image pool: (\d+) blocks moved, (\d+) MB; (\d+) copied at their own draw, (\d+) at the"),
    ("arena progress", r"gpu: arena progress: (\d+) reports applied, (\d+) stale"),
    ("waits", r"gpu: (\d+) WAIT_REG_MEM packets, (\d+) found unsatisfied"),
]
SKIPPED = re.compile(r"renderer:\s+(\d+) draws skipped: (.*)$")
HASH = re.compile(r"renderer: frame (\d+) hash ([0-9a-f]+)")
REPLAY = re.compile(r"replay: (\d+) divergences, (\d+) wait timeouts")
COUNTS = re.compile(r"replay: \d+ batches, .*$")
TRACE = re.compile(r"renderer: command trace .*: (\d+) lines of (\d+), (\d+) unregistered handles")


def run_side(name, capture, exe, env, frames, seconds, out):
    """One replay; the log, the trace and the dump folder in `out`."""
    log = os.path.join(out, name + ".log")
    trace = os.path.join(out, name + ".trace")
    dump = os.path.join(out, name + "-shaders")
    # Nothing of an earlier run in a --keep folder may stand in for this one's.
    for stale in (log, trace):
        if os.path.exists(stale):
            os.remove(stale)
    shutil.rmtree(dump, ignore_errors=True)
    os.makedirs(dump)
    trace_switch = trace if frames is None else "%s:%d:%d" % (trace, frames[0] - 1, frames[1] - frames[0])
    settings = {
        "MW2_REPLAY_GPU": capture,
        "MW2_DETERMINISTIC": "1",
        "MW2_TRACE_COMMANDS": trace_switch,
        "MW2_HASH_FRAMES": "1",
        "MW2_REPORT": "1",
        "MW2_DUMP_SHADERS": dump,
    }
    settings.update(env)
    command = [AGENT_RUN, str(seconds), log, "env"] + ["%s=%s" % kv for kv in settings.items()] + [exe]
    print("replay_diff: %s: %s" % (name, exe), flush=True)
    code = subprocess.call(command, cwd=ROOT)
    if code in (97, 98):
        print("replay_diff: agent_run.sh exited %d: a GPU fault; stop every run" % code)
        sys.exit(code)
    if not os.path.exists(log) or not os.path.exists(trace):
        print("replay_diff: %s made no %s (exit %d)" % (name, "log" if not os.path.exists(log) else "trace", code))
        sys.exit(2)
    with open(log, errors="replace") as f:
        text = f.read().splitlines()
    return text, trace, dump, code


def report(lines):
    found = {}
    for label, pattern in COUNTERS:
        rx = re.compile(pattern)
        found[label] = [m.groups() for m in map(rx.search, lines) if m]
    skipped = {(m.group(2), m.group(1)) for m in map(SKIPPED.search, lines) if m}
    hashes = {int(m.group(1)): m.group(2) for m in map(HASH.search, lines) if m}
    replay = [m.groups() for m in map(REPLAY.search, lines) if m]
    counts = [m.group(0) for m in map(COUNTS.search, lines) if m]
    if counts:
        replay = [r + (not counts[-1].endswith("the replay did not finish"),) for r in replay]
    else:
        replay = [r + (False,) for r in replay]
    trace = [m.groups() for m in map(TRACE.search, lines) if m]
    return found, skipped, hashes, replay, trace


def split_trace(path):
    with open(path, errors="replace") as f:
        lines = f.read().splitlines()
    for i, line in enumerate(lines):
        parts = line.split(" ", 2)
        if len(parts) >= 2 and parts[1] == "shutdown":
            after = collections.Counter(l.split(" ", 1)[1] if " " in l else l for l in lines[i + 1:])
            return lines[: i + 1], after
    return lines, collections.Counter()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("capture")
    parser.add_argument("a")
    parser.add_argument("b")
    parser.add_argument("--env", action="append", default=[], help="K=V on both sides")
    parser.add_argument("--env-b", action="append", default=[], help="K=V on B alone")
    parser.add_argument("--frames", help="a:b, the images hashed as frames a to b-1 and their commands")
    parser.add_argument("--seconds", type=int, default=300, help="each side's hard limit")
    parser.add_argument("--keep", help="the folder for the logs, traces and dumps (default: a temporary one)")
    args = parser.parse_args()

    def pairs(items):
        out = {}
        for item in items:
            if "=" not in item:
                parser.error("%s is not K=V" % item)
            k, v = item.split("=", 1)
            out[k] = v
        return out

    frames = None
    if args.frames:
        a, b = args.frames.split(":")
        frames = (int(a), int(b))
        if frames[0] < 1 or frames[1] <= frames[0]:
            parser.error("--frames a:b needs 1 <= a < b: frame hashes count from 1")
    out = args.keep or tempfile.mkdtemp(prefix="replay_diff-")
    os.makedirs(out, exist_ok=True)
    capture = os.path.abspath(args.capture)
    env_a = pairs(args.env)
    env_b = dict(env_a, **pairs(args.env_b))
    log_a, trace_a, dump_a, code_a = run_side("A", capture, os.path.abspath(args.a), env_a, frames, args.seconds, out)
    log_b, trace_b, dump_b, code_b = run_side("B", capture, os.path.abspath(args.b), env_b, frames, args.seconds, out)

    same = True

    def differ(text):
        nonlocal same
        same = False
        print("DIFFERENT: " + text)

    for name, code in (("A", code_a), ("B", code_b)):
        if code != 0:
            differ("%s exited %d" % (name, code))

    found_a, skipped_a, hashes_a, replay_a, tr_a = report(log_a)
    found_b, skipped_b, hashes_b, replay_b, tr_b = report(log_b)

    # The instrument's own verdict, side by side.
    for name, replay, tr in (("A", replay_a, tr_a), ("B", replay_b, tr_b)):
        if not replay:
            differ("%s printed no replay line: the replay did not reach its reports" % name)
        else:
            divergences, timeouts = int(replay[-1][0]), int(replay[-1][1])
            finished = replay[-1][2]
            print("%s: %d divergences, %d wait timeouts%s"
                  % (name, divergences, timeouts, "" if finished else ", not finished"))
            if divergences or timeouts:
                differ("%s: %d replay divergences, %d wait timeouts" % (name, divergences, timeouts))
            if not finished:
                differ("%s: the replay did not finish" % name)
        if not tr:
            differ("%s printed no command trace line" % name)
        else:
            written, total, unregistered = map(int, tr[-1])
            print("%s: trace %d lines of %d, %d unregistered handles" % (name, written, total, unregistered))
            if unregistered:
                differ("%s: %d unregistered handles" % (name, unregistered))

    # Frame hashes.
    if frames:
        hashes_a = {k: v for k, v in hashes_a.items() if frames[0] <= k < frames[1]}
        hashes_b = {k: v for k, v in hashes_b.items() if frames[0] <= k < frames[1]}
    print("frame hashes: A %d, B %d" % (len(hashes_a), len(hashes_b)))
    if not hashes_a:
        differ("A hashed no frame")
    if hashes_a != hashes_b:
        frames_ab = sorted(set(hashes_a) | set(hashes_b))
        bad = [f for f in frames_ab if hashes_a.get(f) != hashes_b.get(f)]
        differ("frame hashes: %d of %d differ, the first at frame %d (A %s, B %s)"
               % (len(bad), len(frames_ab), bad[0], hashes_a.get(bad[0]), hashes_b.get(bad[0])))

    # The traces.
    before_a, after_a = split_trace(trace_a)
    before_b, after_b = split_trace(trace_b)
    print("traces: A %d lines to shutdown and %d after, B %d and %d"
          % (len(before_a), sum(after_a.values()), len(before_b), sum(after_b.values())))
    if before_a != before_b:
        first = next((i for i, (x, y) in enumerate(zip(before_a, before_b)) if x != y), min(len(before_a), len(before_b)))
        differ("the traces part at line %d" % (first + 1))
        lo, hi = max(0, first - 20), first + 21
        for name, lines in (("A", before_a), ("B", before_b)):
            print("---- %s, lines %d to %d" % (name, lo + 1, min(hi, len(lines))))
            for i in range(lo, min(hi, len(lines))):
                print("%s %s" % (">>" if i == first else "  ", lines[i]))
    if after_a != after_b:
        extra_a = sum((after_a - after_b).values())
        extra_b = sum((after_b - after_a).values())
        differ("the destroys after shutdown: %d lines only A has, %d only B has" % (extra_a, extra_b))
        for line, n in list((after_a - after_b).items())[:5]:
            print("  A only (%d): %s" % (n, line))
        for line, n in list((after_b - after_a).items())[:5]:
            print("  B only (%d): %s" % (n, line))

    # The built-in shaders.
    spv_a = sorted(f for f in os.listdir(dump_a) if f.startswith("builtin_") and f.endswith(".spv"))
    spv_b = sorted(f for f in os.listdir(dump_b) if f.startswith("builtin_") and f.endswith(".spv"))
    print("built-in shaders: A %d, B %d" % (len(spv_a), len(spv_b)))
    if spv_a != spv_b:
        differ("built-in shaders: A has %s, B %s" % (spv_a, spv_b))
    for f in sorted(set(spv_a) & set(spv_b)):
        if not filecmp.cmp(os.path.join(dump_a, f), os.path.join(dump_b, f), shallow=False):
            differ("built-in shader %s differs" % f)

    # The report.
    for label, _ in COUNTERS:
        if found_a[label] != found_b[label]:
            differ("report, %s: A %s, B %s" % (label, found_a[label], found_b[label]))
        elif not found_a[label]:
            print("report, %s: on neither side" % label)
    if skipped_a != skipped_b:
        differ("draws skipped: A %s, B %s" % (sorted(skipped_a - skipped_b), sorted(skipped_b - skipped_a)))

    print("replay_diff: %s (logs, traces and dumps in %s)" % ("IDENTICAL" if same else "DIFFERENT", out))
    return 0 if same else 1


if __name__ == "__main__":
    sys.exit(main())
