#!/usr/bin/env python3
"""Walks the player along a recorded route with a virtual controller, from outside the game.

    tools/padwalk.py [options] -- <game> <args...>

The game runs unchanged -- a release build too -- as this script's child. The script
makes a gamepad through the kernel's uinput, which SDL sees as one plugged in, reads
where the player stands from the game's memory (/proc/<pid>/mem: Yama allows a parent
to read its child), and holds the sticks that walk the route. It is the steering of
src/runtime/player.rs, which does the same from inside a diagnostics build, carried
over decision for decision.

Options:
  --route <file>[,<file>...]  routes recorded with MW2_RECORD_PATH; the nearest is walked
  --title sp|mp, --tu0         which block of runtime/title.h holds the addresses (mp)
  --team-up <n>                taps Up n times (1 or 2) on the team selection, as MW2_TEAM_UP
  --reach <units>              how near counts as reaching a corner, as MW2_WALK_REACH
  --quit-after-arrival <s>     SIGINT the game s seconds after arriving, or after the walk
                               stops getting nearer for --quit-if-stuck seconds (45), as
                               MW2_QUIT_AFTER_ARRIVAL and MW2_QUIT_IF_STUCK (default: stay)
  --quit-if-stuck <s>          see --quit-after-arrival
  --seconds <s>                SIGINT the game after s seconds whatever happened
  --log <file>                 the walk's log (stdout otherwise)

Only this pad is shown to the game (SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT), so a
physical one left plugged in neither becomes player one nor drifts the sticks. Menus are
dismissed with A and only A, as in player.rs, so the walk cannot quit a match. Linux
builds only: the guest memory is found by its mapping, not by a symbol, so a stripped
release works. The game has to be this script's own child, or a wrapper's that execs it
(systemd-run --scope does): one that forks it elsewhere, as a Flatpak Steam does with
MW2_NETWORK=steam, leaves nothing to read.

Exit status: 0 when --quit-after-arrival ended the game after the walk arrived, 1 when
it ended it after the walk got stuck or --seconds ran out, 2 for a usage error or a pad
that cannot be made, and otherwise the game's own when it ended by itself.
"""

import fcntl
import math
import os
import re
import signal
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# --- the virtual pad --------------------------------------------------------------------

# An Xbox 360 pad's buttons and axes, from which SDL makes its layout, under pid.codes'
# test id, which no real device carries, so the hint below lets it alone through.
VENDOR, PRODUCT, VERSION = 0x1209, 0x0001, 0x0001
EV_SYN, EV_KEY, EV_ABS = 0x00, 0x01, 0x03
BTN_A, BTN_B, BTN_X, BTN_Y = 0x130, 0x131, 0x133, 0x134
BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL, BTN_THUMBR = 0x136, 0x137, 0x13A, 0x13B, 0x13C, 0x13D, 0x13E
ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT0X, ABS_HAT0Y = 0, 1, 2, 3, 4, 5, 0x10, 0x11
UI_DEV_CREATE, UI_DEV_DESTROY = 0x5501, 0x5502
UI_SET_EVBIT, UI_SET_KEYBIT, UI_SET_ABSBIT = 0x40045564, 0x40045565, 0x40045567
UI_DEV_SETUP, UI_ABS_SETUP = 0x405C5503, 0x401C5504
BUS_USB = 0x03


class Pad:
    def __init__(self):
        self.fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_KEY)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_ABS)
        for key in (BTN_A, BTN_B, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL, BTN_THUMBR):
            fcntl.ioctl(self.fd, UI_SET_KEYBIT, key)
        ranges = {ABS_X: (-32768, 32767), ABS_Y: (-32768, 32767), ABS_RX: (-32768, 32767), ABS_RY: (-32768, 32767),
                  ABS_Z: (0, 255), ABS_RZ: (0, 255), ABS_HAT0X: (-1, 1), ABS_HAT0Y: (-1, 1)}
        for code, (low, high) in ranges.items():
            fcntl.ioctl(self.fd, UI_SET_ABSBIT, code)
            # struct uinput_abs_setup: code, then input_absinfo (value, min, max, fuzz, flat, resolution).
            fcntl.ioctl(self.fd, UI_ABS_SETUP, struct.pack("<HxxiiiiiI", code, 0, low, high, 0, 0, 0))
        name = b"padwalk virtual pad"
        fcntl.ioctl(self.fd, UI_DEV_SETUP, struct.pack("<HHHH80sI", BUS_USB, VENDOR, PRODUCT, VERSION, name, 0))
        fcntl.ioctl(self.fd, UI_DEV_CREATE)
        self.sent = {}

    def _event(self, kind, code, value):
        os.write(self.fd, struct.pack("<qqHHi", 0, 0, kind, code, value))

    # axes as the guest has them: left x, left y (up positive), right x, right y (up positive).
    def set(self, axes, a=False, up=False, left=False, fire=False):
        state = {
            (EV_ABS, ABS_X): axes[0],
            (EV_ABS, ABS_Y): -1 - axes[1],  # evdev's and SDL's Y point down
            (EV_ABS, ABS_RX): axes[2],
            (EV_ABS, ABS_RY): -1 - axes[3],
            (EV_ABS, ABS_RZ): 255 if fire else 0,
            (EV_ABS, ABS_HAT0Y): -1 if up else 0,
            (EV_ABS, ABS_HAT0X): -1 if left else 0,
            (EV_KEY, BTN_A): 1 if a else 0,
        }
        changed = False
        for (kind, code), value in state.items():
            if self.sent.get((kind, code)) != value:
                self._event(kind, code, value)
                self.sent[(kind, code)] = value
                changed = True
        if changed:
            self._event(EV_SYN, 0, 0)

    def close(self):
        try:
            self.set([0, 0, 0, 0])
            fcntl.ioctl(self.fd, UI_DEV_DESTROY)
        finally:
            os.close(self.fd)


# --- where the player is ----------------------------------------------------------------

def title_block(title, tu0):
    """The #define T_ values of the block of runtime/title.h for this title and version."""
    text = open(os.path.join(REPO, "runtime/title.h")).read().split("\n")
    blocks, current = [], None
    for line in text:
        if re.match(r"#(if|elif|else)\b", line):
            current = {}
            blocks.append(current)
        elif line.startswith("#endif"):
            current = None
        elif current is not None:
            m = re.match(r"#define\s+(T_\w+)\s+(\S+)", line)
            if m:
                try:
                    current[m.group(1)] = int(m.group(2).rstrip("uU"), 0)
                except ValueError:
                    pass  # a function entry (bare hex) or an expression: not needed here
    order = {("mp", False): 0, ("sp", False): 1, ("mp", True): 2, ("sp", True): 3}
    return blocks[order[(title, tu0)]]


# Where src/runtime/guest_memory.rs places its shared memory object: at guest 0xA0000000
# and again at 0xC0000000, inside the one reservation whose start is the guest's 0.
PHYSICAL_NAME = "/memfd:mw2-physical"
PHYSICAL_BASE = 0xA0000000


class Memory:
    """The game's guest memory, once it has mapped it. Through whatever wrapper started the
    game (systemd-run --scope execs it in its own process), the child's pid is the game's."""

    def __init__(self, pid):
        starts = []
        for line in open(f"/proc/{pid}/maps"):
            parts = line.split()
            if len(parts) >= 6 and parts[5] == PHYSICAL_NAME:
                starts.append(int(parts[0].split("-")[0], 16))
        if len(starts) < 2:
            raise RuntimeError(f"process {pid} has not mapped the guest memory (yet)")
        self.base = min(starts) - PHYSICAL_BASE
        self.mem = open(f"/proc/{pid}/mem", "rb", buffering=0)

    def _read(self, address, form):
        self.mem.seek(self.base + address)
        data = self.mem.read(4)
        if len(data) != 4:
            raise OSError("short read: the game is gone")
        return struct.unpack(form, data)[0]

    def word(self, address):
        return self._read(address, ">I")

    def real(self, address):
        return self._read(address, ">f")


class Where:
    def __init__(self, x, y, z, yaw, pitch):
        self.x, self.y, self.z, self.yaw, self.pitch = x, y, z, yaw, pitch


def locate(memory, t):
    """player.rs's locate(): None before the player exists."""
    if t["T_DATA_ClientStates"] == 0:
        return None
    client = t["T_DATA_ClientStates"] if "T_CLIENT_STATES_ARE_HERE" in t else memory.word(t["T_DATA_ClientStates"])
    if client == 0 or memory.word((client + t["T_CLIENT_VALID"]) & 0xFFFFFFFF) == 0:
        return None
    origin = (client + t["T_CLIENT_ORIGIN"]) & 0xFFFFFFFF
    angles = (client + t["T_CLIENT_ANGLES"]) & 0xFFFFFFFF
    here = Where(memory.real(origin), memory.real(origin + 4), memory.real(origin + 8),
                 memory.real(angles + 4), memory.real(angles))
    if not all(math.isfinite(v) for v in (here.x, here.y, here.yaw)):
        return None
    return here if abs(here.x) < 1e6 and abs(here.y) < 1e6 else None


# --- the steering: src/runtime/player.rs ------------------------------------------------

MILLISECOND = 1_000_000
SECOND = 1_000_000_000
PLAYING_AFTER = 300.0
TELEPORT = 1500.0
FAR = 1e30
DEGREES = 57.29578
WALK, LEFT, FIRE = "walk", "left", "fire"


class Point:
    def __init__(self, x, y, act=WALK, yaw=0.0, pitch=0.0, view=False):
        self.x, self.y, self.act, self.yaw, self.pitch, self.view = x, y, act, yaw, pitch, view


def read_route(path, log):
    out = []
    try:
        lines = open(path).read().splitlines()
    except OSError:
        log(f"player: no route file at {path}")
        return out
    for line in lines:
        if line.startswith("#") or not line.strip():
            continue
        words = line.split()
        if words[0] in ("left", "fire"):
            try:
                x, y, _, yaw, pitch = map(float, words[1:])
            except ValueError:
                continue  # sscanf reads no six fields from it
            out.append(Point(x, y, LEFT if words[0] == "left" else FIRE, yaw, pitch))
            continue
        values = []
        for word in words[:5]:
            try:
                values.append(float(word))
            except ValueError:
                break
        if len(values) >= 2:
            read = len(values)
            values += [0.0] * (5 - read)
            out.append(Point(values[0], values[1], WALK, values[3], values[4], view=read == 5))
    log(f"player: route of {len(out)} points read from {path}")
    return out


def shortest_turn(want, have):
    turn = want - have
    while turn > 180.0:
        turn -= 360.0
    while turn < -180.0:
        turn += 360.0
    return turn


def turn_stick(degrees):
    if abs(degrees) < 1.5:
        return 0
    want = max(-1.0, min(1.0, degrees / 45.0)) * 28000.0
    least = 14000.0
    return int(-(math.copysign(least, want) if abs(want) < least else want))


class Walker:
    def __init__(self, routes, team_ups, reach, log):
        self.routes, self.team_ups, self.reach, self.log = routes, team_ups, reach, log
        self.goal = None  # (x, y, radius, facing, yaw)
        self.via = []
        self.route_chosen = False
        self.last_seen = None
        self.leg = 0
        self.leg_at = 0
        self.best, self.best_at = FAR, 0
        self.escape_until, self.escape_side, self.escapes, self.escape_angle = 0, 1, 0, 90.0
        self.still = Where(0.0, 0.0, 0.0, 0.0, 0.0)
        self.still_at = 0
        self.pressing = self.spawned = self.playing = self.alive = False
        self.team_screen = self.freezes = self.ups_sent = 0
        self.up_window = None
        self.travelled = 0.0
        self.act_at = self.aimed_at = self.press_at = 0
        self.joined_from = Point(0.0, 0.0)
        self.closest = FAR
        self.arrived = False

    def start_leg(self, now, distance):
        self.leg_at, self.best, self.best_at = now, distance, now

    def choose_route(self, now, x, y):
        if self.last_seen is not None and (x - self.last_seen[0]) ** 2 + (y - self.last_seen[1]) ** 2 > TELEPORT ** 2:
            self.route_chosen = False
        self.last_seen = (x, y)
        if self.route_chosen:
            return
        best_route, best_point, best = 0, 0, FAR
        for r, route in enumerate(self.routes):
            for i, p in enumerate(route):
                d = (p.x - x) ** 2 + (p.y - y) ** 2
                if d < best:
                    best_route, best_point, best = r, i, d
        self.via = self.routes[best_route][best_point:]
        end = self.via[-1]
        if end.act == WALK:
            self.goal = (end.x, end.y, 96.0, False, 0.0)
            self.via = self.via[:-1]
        else:
            self.goal = (end.x, end.y, 96.0, True, end.yaw)
        self.route_chosen = True
        self.joined_from = Point(x, y)
        self.leg = 0
        self.start_leg(now, FAR)
        self.log(f"player: at {x:.1f} {y:.1f} -- route {best_route + 1} of {len(self.routes)}, joining at point "
                 f"{best_point} ({math.sqrt(best):.0f} units away), {len(self.via)} legs to {self.goal[0]:.1f} {self.goal[1]:.1f}")

    def dismiss_menus(self, now, here, out):
        # x and y alone, where player.rs compares z too: the title writes the height twice a
        # frame (on the team selection 268, then 260), and a read from outside, at any
        # moment, sees both, where the guest's own input poll always sees the second. So a
        # menu would never look still. On a menu x and y hold.
        if (here.x, here.y) != (self.still.x, self.still.y):
            if self.still_at != 0:
                if not self.spawned:
                    self.route_chosen = False
                    self.choose_route(now, here.x, here.y)
                self.spawned = True
                step = math.hypot(here.x - self.still.x, here.y - self.still.y)
                if step < 500.0:
                    self.travelled += step
                from_origin = self.still.x == 0.0 and self.still.y == 0.0
                if step < 500.0 or not from_origin:
                    self.alive = True
                if not self.playing and self.travelled > PLAYING_AFTER:
                    self.playing = True
                    self.log(f"player: {self.travelled:.0f} units covered -- in the world now, so no more menu presses for the rest of the run")
            self.still = here
            self.still_at = now
            if self.pressing:
                self.pressing = False
                self.log("player: moving again")
            return False
        held = now - self.still_at
        if held <= (3 if self.spawned else 2) * SECOND:
            return False
        if not self.pressing:
            self.pressing = True
            self.freezes += 1
            if self.team_screen == 0 and (here.x != 0.0 or here.y != 0.0):
                self.team_screen = self.freezes
            self.log(f"player: frozen at {here.x:.1f} {here.y:.1f} {here.z:.1f} -- pressing to get past whatever is waiting")
        self.leg = 0
        self.leg_at = now
        out["axes"] = [0, 0, 0, 0]
        if (held // (600 * MILLISECOND)) % 2 == 0:
            window = (held // (600 * MILLISECOND)) // 2
            team_screen = self.freezes == self.team_screen and not self.playing
            into = held % (1200 * MILLISECOND)
            if team_screen and self.ups_sent < self.team_ups:
                if into < 100 * MILLISECOND:
                    out["up"] = True
                    if window != self.up_window:
                        self.up_window = window
                        self.ups_sent += 1
            elif team_screen and self.ups_sent != 0 and window == self.up_window:
                pass
            else:
                out["a"] = True
        return True

    def next_corner(self, here, now):
        if self.leg_at == 0:
            self.leg_at = now
        if self.leg >= len(self.via):
            return Point(self.goal[0], self.goal[1])
        corner = self.via[self.leg]
        if corner.act != WALK:
            return corner
        reach = math.hypot(corner.x - here.x, corner.y - here.y)
        near = self.reach if self.reach > 0 else (24.0 if corner.view else 140.0)
        came = self.via[self.leg - 1] if self.leg > 0 else self.joined_from
        passed = (here.x - corner.x) * (corner.x - came.x) + (here.y - corner.y) * (corner.y - came.y) > 0.0
        if reach >= near and not passed and now - self.leg_at <= 90 * SECOND:
            return corner
        self.log(f"player: leaving corner {self.leg} at {here.x:.1f} {here.y:.1f}, {reach:.0f} units from it")
        self.leg += 1
        self.start_leg(now, FAR)
        return self.via[self.leg] if self.leg < len(self.via) else Point(self.goal[0], self.goal[1])

    def do_action(self, here, now, out):
        if self.leg >= len(self.via) or self.via[self.leg].act == WALK:
            return False
        at = self.via[self.leg]
        reach = math.hypot(at.x - here.x, at.y - here.y)
        close = 24.0 if at.act == FIRE else 96.0
        if self.act_at == 0:
            if reach > close and now - self.leg_at < 20 * SECOND:
                return False
            self.act_at, self.aimed_at, self.press_at = now, 0, 0
            self.log(f"player: at {here.x:.1f} {here.y:.1f} {here.z:.1f}, {reach:.0f} units from where the route "
                     f"{'fired' if at.act == FIRE else 'pressed left'} -- aiming at yaw {at.yaw:.1f} pitch {at.pitch:.1f}")
        axes = out["axes"]
        axes[0] = axes[1] = 0
        axes[2] = turn_stick(shortest_turn(at.yaw, here.yaw))
        axes[3] = turn_stick(shortest_turn(at.pitch, here.pitch))
        aimed = at.act == LEFT or (axes[2] == 0 and axes[3] == 0)
        if not aimed:
            self.aimed_at = 0
        elif self.aimed_at == 0:
            self.aimed_at = now
        settled = (self.aimed_at != 0 and now - self.aimed_at > 300 * MILLISECOND) or now - self.act_at > 8 * SECOND
        if not settled:
            return True
        if self.press_at == 0:
            self.press_at = now
        pressed = now - self.press_at
        if pressed < 300 * MILLISECOND:
            out["fire" if at.act == FIRE else "left"] = True
            return True
        if pressed < 2800 * MILLISECOND:
            return True
        self.log(f"player: {'fired' if at.act == FIRE else 'pressed left'} at {here.x:.1f} {here.y:.1f} {here.z:.1f}, "
                 f"yaw {here.yaw:.1f} pitch {here.pitch:.1f} (asked {at.yaw:.1f} {at.pitch:.1f})")
        self.act_at = self.press_at = 0
        self.leg += 1
        self.start_leg(now, FAR)
        return True

    def go_round(self, bearing, distance, here, now, out):
        if distance < self.best - 40.0:
            self.best, self.best_at = distance, now
        if self.best_at == 0:
            self.best_at = now
        if self.escape_until > now:
            left = self.escape_until - now
            out["backing"] = left > 2500 * MILLISECOND
            if (left // (400 * MILLISECOND)) % 2 == 0:
                out["a"] = True
            return bearing + self.escape_side * self.escape_angle
        if now - self.best_at > 8 * SECOND:
            self.escape_side = -self.escape_side
            self.escape_angle = (90.0, 135.0, 60.0, 170.0)[self.escapes % 4]
            self.escapes += 1
            self.escape_until = now + 4000 * MILLISECOND
            self.best_at, self.best = now, distance
            self.log(f"player: no closer than {distance:.0f} units for eight seconds at {here.x:.1f} {here.y:.1f} "
                     f"{here.z:.1f}; backing off and going round to the {'left' if self.escape_side < 0 else 'right'} "
                     f"at {self.escape_angle:.0f} degrees")
        return bearing

    def steer(self, now, here, out):
        """player.rs's steer(): fills out['axes'] and the presses; False leaves the pad idle."""
        self.choose_route(now, here.x, here.y)
        gx, gy, radius, facing, gyaw = self.goal
        standing = self.spawned and math.hypot(gx - here.x, gy - here.y) <= radius
        if not standing and self.act_at == 0 and self.dismiss_menus(now, here, out):
            return True
        gx, gy, radius, facing, gyaw = self.goal
        if not self.alive:
            out["axes"] = [0, 0, 0, 0]
            return True
        to = self.next_corner(here, now)
        if self.do_action(here, now, out):
            return True
        dx, dy = to.x - here.x, to.y - here.y
        distance = math.hypot(dx, dy)
        to_goal = math.hypot(gx - here.x, gy - here.y)
        if self.spawned and to_goal < self.closest:
            self.closest = to_goal
        axes = out["axes"]
        if self.leg >= len(self.via) and distance <= radius:
            if not self.arrived:
                self.arrived = True
                self.log(f"player: arrived at {here.x:.1f} {here.y:.1f} {here.z:.1f}, {distance:.0f} units from the target")
            axes[0] = axes[1] = axes[3] = 0
            axes[2] = turn_stick(shortest_turn(gyaw, here.yaw)) if facing else 6000
            return True
        self.arrived = False
        out["backing"] = False
        bearing = self.go_round(math.atan2(dy, dx) * DEGREES, distance, here, now, out)
        backing = out["backing"]
        turn = shortest_turn(bearing, here.yaw)
        axes[2] = turn_stick(turn)
        axes[1] = -30000 if backing else (32000 if abs(turn) < 60.0 else 12000)
        if not backing and self.leg < len(self.via) and self.via[self.leg].act != WALK and distance < 150.0:
            axes[1] = min(axes[1], 12000)
        axes[0] = axes[3] = 0
        if to.view and not backing and self.escape_until <= now:
            off = shortest_turn(math.atan2(dy, dx) * DEGREES, here.yaw) / DEGREES
            axes[0] = int(-32000.0 * math.sin(off))
            axes[1] = int(32000.0 * math.cos(off))
            axes[2] = turn_stick(shortest_turn(to.yaw, here.yaw))
            axes[3] = turn_stick(shortest_turn(to.pitch, here.pitch))
        return True

    def seconds_since_progress(self, now):
        if not self.spawned or self.best_at == 0:
            return 0.0
        return (now - self.best_at) / 1e9


# --- the run ----------------------------------------------------------------------------

def main():
    args = sys.argv[1:]
    def usage(text):
        print(f"padwalk: {text}\n{__doc__}", file=sys.stderr)
        sys.exit(2)

    if "--" not in args:
        usage("no -- before the game's command")
    split = args.index("--")
    options, command = args[:split], args[split + 1:]
    o = {"--route": None, "--title": "mp", "--team-up": "0", "--reach": "0", "--quit-after-arrival": None,
         "--quit-if-stuck": "45", "--seconds": None, "--log": None}
    tu0 = False
    i = 0
    while i < len(options):
        if options[i] == "--tu0":
            tu0 = True
            i += 1
        elif options[i] in o and i + 1 < len(options):
            o[options[i]] = options[i + 1]
            i += 2
        else:
            usage(f"unknown option {options[i]}")
    if not o["--route"] or not command:
        usage("--route and the game's command are needed")
    if o["--title"] not in ("sp", "mp"):
        usage(f"--title is sp or mp, not {o['--title']}")

    def number(name):
        if o[name] is None:
            return None
        try:
            return float(o[name])
        except ValueError:
            usage(f"{name} takes a number, not {o[name]}")

    team_ups, reach = number("--team-up"), number("--reach")
    after, stall, limit = number("--quit-after-arrival"), number("--quit-if-stuck"), number("--seconds")

    out_file = open(o["--log"], "w", buffering=1) if o["--log"] else sys.stdout
    started = time.monotonic_ns()

    def log(text):
        print(f"[{(time.monotonic_ns() - started) / 1e9:7.2f}] {text}", file=out_file)

    t = title_block(o["--title"], tu0)
    routes = [r for r in (read_route(path, log) for path in o["--route"].split(",") if path) if r]
    if not routes:
        log("padwalk: no route has a point to walk")
        sys.exit(2)
    walker = Walker(routes, min(int(team_ups), 2), reach, log)
    after = None if after is None else after * SECOND
    stall = (stall if stall > 0 else 45.0) * SECOND  # as run.rs: none given is 45
    limit = None if limit is None else limit * SECOND

    try:
        pad = Pad()
    except OSError as error:
        log(f"padwalk: no pad: /dev/uinput: {error}")
        sys.exit(2)
    log(f"pad: made {VENDOR:04x}:{PRODUCT:04x}; the game is shown it alone")
    env = dict(os.environ)
    env["SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT"] = f"0x{VENDOR:04x}/0x{PRODUCT:04x}"
    time.sleep(0.5)  # the device node, before SDL looks
    game = subprocess.Popen(command, env=env)
    log(f"game: started, pid {game.pid}")
    outcome = None  # "arrived", "stuck" or "time", once the game has been told to stop
    # As run.rs's start_arrival_deadline: with --quit-after-arrival, the walk ends that long
    # after it arrived, or after it stopped getting nearer, whichever comes first.
    ending, ending_at = None, None
    try:
        memory = None
        unread = None
        while game.poll() is None:
            now = time.monotonic_ns()
            if memory is None:
                try:
                    memory = Memory(game.pid)
                    log(f"game: reading the guest memory of {os.path.realpath(f'/proc/{game.pid}/exe')}")
                except (RuntimeError, OSError) as error:
                    if str(error) != unread:
                        unread = str(error)
                        log(f"game: no guest memory to read yet: {unread}")
                    if limit is not None and now - started > limit:
                        outcome = "time"
                        log("game: SIGINT, the time given is up")
                        game.send_signal(signal.SIGINT)
                        break
                    time.sleep(0.05)
                    continue
            try:
                here = locate(memory, t)
            except OSError:
                here = None
            out = {"axes": [0, 0, 0, 0], "a": False, "up": False, "left": False, "fire": False, "backing": False}
            if here is not None and walker.steer(now, here, out):
                pad.set(out["axes"], a=out["a"], up=out["up"], left=out["left"], fire=out["fire"])
            else:
                pad.set([0, 0, 0, 0])
            if after is not None and ending is None:
                if walker.arrived:
                    ending, ending_at = "arrived", now + after
                elif walker.seconds_since_progress(now) > stall / SECOND:
                    ending, ending_at = "stuck", now + after
                    log("player: the walk stopped getting anywhere")
            if ending is not None and now >= ending_at:
                outcome = ending
            elif limit is not None and now - started > limit:
                outcome = "time"
            if outcome:
                log(f"game: SIGINT, {'the time given is up' if outcome == 'time' else outcome}")
                game.send_signal(signal.SIGINT)
                break
            time.sleep(0.008)
    finally:
        try:
            pad.close()
        finally:
            stop(game, outcome, log)
        log(f"player: closest approach {0 if walker.closest == FAR else walker.closest:.0f} units, "
            f"{walker.escapes} times going round something{' (arrived)' if walker.arrived else ''}; game exit {game.returncode}")
    if outcome is None:
        sys.exit(game.returncode if game.returncode >= 0 else 128 - game.returncode)
    sys.exit(0 if outcome == "arrived" else 1)


def stop(game, outcome, log):
    """The game ended, asked once (SIGINT, unless the walk already sent it), then made to."""
    if game.poll() is None:
        if outcome is None:
            game.send_signal(signal.SIGINT)
        try:
            game.wait(timeout=30)
        except subprocess.TimeoutExpired:
            log("game: did not end 30 s after SIGINT; SIGKILL")
            game.kill()
            game.wait()


if __name__ == "__main__":
    main()
