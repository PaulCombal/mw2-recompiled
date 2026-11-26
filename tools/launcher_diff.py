#!/usr/bin/env python3
# Holds the launcher's Rust to the C++ it replaced (launcher_diff.rs, launcher_diff.cpp),
# compiled from git as it was at a commit before (--old): the settings on .env files
# made here, with values of every kind a player could write by hand; the profile screen's
# work on saves/ folders made here, with profiles, stats files and campaign profiles
# good and damaged; the bug report's scrubbing of logs made of every kind of line it acts
# on; and the menu sounds' finding on the game's own fastfiles and on ones made here.
# Each case runs once a side, in a copy of its folder; what each says and every file it
# leaves are compared. The Rust is built with overflow checks and debug_assert! on.
#
#   usage: launcher_diff.py <build folder> [--cases N] [--old <commit>] [--src folder]
#          [--work folder] [--only settings|profile|scrub|sound]
#
# --src takes the Rust from another launcher/src, to try a deliberate mistake. Exits
# nonzero at the first difference, leaving the case in the work folder.
import argparse, filecmp, glob, os, random, shutil, struct, subprocess, sys, zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OLD_FILES = ["launcher/settings.h", "launcher/settings.cpp", "launcher/profile.h", "launcher/profile.cpp",
             "launcher/playerdata_layout.h", "launcher/utf8.h", "launcher/report.h", "launcher/report.cpp",
             "launcher/setup.h", "launcher/update.h", "launcher/sound.h", "launcher/sound.cpp",
             "runtime/render_size.h", "runtime/install/spawn.h", "runtime/install/spawn.cpp"]
MODULES = {"settings": "launcher/settings.cpp", "profile": "launcher/profile.cpp", "report": "launcher/report.cpp",
           "sound": "launcher/sound.cpp"}


def run(command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def build(arguments, work):
    old = f"{work}/old"
    for name in OLD_FILES:
        os.makedirs(os.path.dirname(f"{old}/{name}"), exist_ok=True)
        text = subprocess.run(["git", "-C", ROOT, "show", f"{arguments.old}:{name}"], capture_output=True, check=True).stdout
        open(f"{old}/{name}", "wb").write(text)
    sdl = sorted(glob.glob(f"{os.path.abspath(arguments.build)}/rust/release/build/sdl3-sys-*/out/include"))
    if not sdl:
        sys.exit(f"{arguments.build} has no SDL that sdl3-sys built: build it first")
    sdl = os.path.dirname(sdl[0])
    objects = []
    for module, source in MODULES.items():
        obj = f"{work}/old_{module}.o"
        run(["clang++-18", "-std=gnu++20", "-O1", "-g", "-c", f"-DOLD=\"{old}/{source}\"", f"-DMODULE_{module.upper()}",
             f"-I{old}/launcher", f"-I{sdl}/include", f"{ROOT}/tools/launcher_diff.cpp", "-o", obj])
        objects.append(obj)
    run(["clang++-18", "-std=gnu++20", "-O1", "-c", f"-I{sdl}/include", f"{old}/runtime/install/spawn.cpp", "-o", f"{work}/spawn.o"])
    objects.append(f"{work}/spawn.o")
    library = f"{work}/liblauncher_diff_c.a"
    if os.path.exists(library):
        os.remove(library)
    run(["ar", "rcs", library] + objects)
    install = f"{work}/libmw2_install.rlib"
    run(["rustc", "--edition=2024", "--crate-type=rlib", "--crate-name=mw2_install", "-Copt-level=2", "-Cdebug-assertions=on",
         "-Coverflow-checks=on", "-o", install, f"{ROOT}/install/src/lib.rs"])
    source = f"{work}/launcher_diff.rs"
    text = open(f"{ROOT}/tools/launcher_diff.rs").read().replace('"../launcher/src/', f'"{os.path.abspath(arguments.src)}/')
    open(source, "w").write(text)
    program = f"{work}/launcher_diff"
    run(["rustc", "--edition=2024", "-Copt-level=2", "-Cdebug-assertions=on", "-Coverflow-checks=on", "-Cpanic=abort",
         "-o", program, source, "--extern", f"mw2_install={install}", "-L", work, "-l", "static=launcher_diff_c",
         "-L", f"{sdl}/lib", "-l", "static=SDL3", "-l", "stdc++", "-l", "m"])
    return program


# One case: the folder made by `make`, copied for each side, the program run in it.
def compare(program, work, module, make, arguments_of, environment=None):
    case = f"{work}/case"
    shutil.rmtree(case, ignore_errors=True)
    os.makedirs(case)
    made = make(case)
    said = {}
    for side in ("cpp", "rust"):
        folder = f"{work}/{side}"
        shutil.rmtree(folder, ignore_errors=True)
        shutil.copytree(case, folder, symlinks=True)
        result = subprocess.run([program, module, side] + arguments_of(folder, made), cwd=folder, capture_output=True,
                                env=environment(folder) if environment else None, timeout=120)
        if result.returncode:
            sys.exit(f"{module}: the {side} side ended {result.returncode}: {result.stderr.decode(errors='replace')}\ncase in {case}")
        said[side] = result.stdout
    if said["cpp"] != said["rust"]:
        a, b = said["cpp"].decode(errors="replace").split("\n"), said["rust"].decode(errors="replace").split("\n")
        at = next(i for i in range(max(len(a), len(b))) if i >= len(a) or i >= len(b) or a[i] != b[i])
        sys.exit(f"DIFFERENT: {module}: line {at + 1}\n  C++:  {a[at] if at < len(a) else '<end>'}\n"
                 f"  Rust: {b[at] if at < len(b) else '<end>'}\ncase in {case}")
    trees = filecmp.dircmp(f"{work}/cpp", f"{work}/rust")
    def differences(tree, path=""):
        found = [f"{path}{name}" for name in tree.left_only + tree.right_only + tree.funny_files]
        found += [f"{path}{name}" for name in tree.common_files
                  if open(f"{tree.left}/{name}", "rb").read() != open(f"{tree.right}/{name}", "rb").read()]
        for name, sub in tree.subdirs.items():
            found += differences(sub, f"{path}{name}/")
        return found
    files = differences(trees)
    if files:
        sys.exit(f"DIFFERENT: {module}: the files left differ: {files}\ncase in {case}")
    return said["cpp"]


def escape(data):
    return "".join(c if 0x21 <= ord(c) < 0x7F and c != "\\" else f"\\x{ord(c):02x}" for c in data.decode("latin-1")) or "-"


# ---- settings ------------------------------------------------------------------------------

NAMES = ["MW2_FULLSCREEN", "MW2_SCALE", "MW2_RENDER", "MW2_FPS_LIMIT", "MW2_FOV", "MW2_MSAA", "MW2_NO_MSAA",
         "MW2_SHADOW_SCALE", "MW2_ANISOTROPY", "MW2_STRETCH", "MW2_CUSTOM_MAPS", "MW2_LAUNCHER_SOUNDS", "MW2_NETWORK",
         "MW2_SERVER", "MW2_SERVER_PASSWORD", "MW2_PC_GAME"]
NUMBERS = ["0", "1", "2", "3", "4", "5", "8", "16", "-1", "+5", " 7", "\t9", "\x0b3", "99999999999", "-99999999999",
           "2147483647", "2147483648", "-2147483649", "4294967296", "9223372036854775807", "9223372036854775808",
           "-9223372036854775809", "18446744073709551616", "12abc", "abc", "", " ", "0x10", "1\x002", "65", "120", "121",
           "64", "1000", "1001", "60", "-", "+", "007"]
FRAMES = ["1280x720", "960x540", "1280x800", "960x600", "640x480", "800x600", "1024x768", "960x720", "0x0", "1280x0",
          "1280X720", " 1280x 720", "1280 x720", "-1x-1", "4294967295x1", "4294967296x720", "x720", "1280x", "abc",
          "1920x1080", "960x540junk", "+960x+540", "18446744073709551615x5", "640x480\x00x", "\t640x\n480"]
NETWORKS = ["none", "lan", "server", "steam", "LAN", "", "steam ", "relay"]


def settings_file(rng):
    lines = []
    for _ in range(rng.randrange(0, 18)):
        name = rng.choice(NAMES + ["MW2_OTHER", "ZZZ", "a b", "", "mw2_scale", "MW2_SCALE "])
        if name == "MW2_RENDER":
            value = rng.choice(FRAMES)
        elif name == "MW2_NETWORK":
            value = rng.choice(NETWORKS)
        elif rng.randrange(5) == 0:
            value = "".join(chr(rng.randrange(256)) for _ in range(rng.randrange(0, 6))).replace("\n", "")
        else:
            value = rng.choice(NUMBERS)
        line = f"{name}={value}" if rng.randrange(12) else rng.choice(["no equals", "", "=", "==x"])
        line += rng.choice(["", "", "", "\r", " ", " \r ", "\r\r"])
        lines.append(line)
    text = "\n".join(lines) + rng.choice(["\n", "", "\n\n"])
    return text.encode("latin-1")


def settings_script(rng):
    script = ["load", "get"]
    for _ in range(rng.randrange(1, 14)):
        which = rng.randrange(14)
        if which < 11:
            value = {0: rng.randrange(2), 1: rng.randrange(-1, 4), 3: rng.choice([-5, 0, 30, 60, 144, 1000, 2000]),
                     4: rng.randrange(0, 200), 5: rng.choice([-1, 0, 1, 2, 3, 4, 8, 16]), 6: rng.randrange(-1, 7),
                     7: rng.choice([0, 1, 2, 4, 8, 16, 32]), 8: rng.randrange(2), 9: rng.randrange(2),
                     10: rng.randrange(4)}.get(which, 0)
            other = rng.randrange(-2, 10) if which == 1 else 0
            if which == 1 and value == -1:
                value = 0
            script.append(f"set {which} {value} {other}")
        else:
            data = bytes(rng.randrange(1, 256) for _ in range(rng.randrange(0, 8))) if rng.randrange(3) == 0 else \
                rng.choice([b"", b"127.0.0.1:28960", b"/home/x/MW2 PC", b"pass word", b"C:\\Games\\MW2"])
            script.append(f"text {which - 11} {escape(data)}")
        script.append("get")
    return "\n".join(script) + "\n"


def check_settings(program, work, rng, cases):
    for case in range(cases):
        def make(folder):
            open(f"{folder}/.env", "wb").write(settings_file(rng))
            open(f"{folder}/script", "w").write(settings_script(rng))
        compare(program, work, "settings", make, lambda folder, _: ["script"])
    print(f"settings: all the same: {cases} cases")


# ---- profile ---------------------------------------------------------------------------------

VERSION, FORMAT, EXPERIENCE, PRESTIGE = 155, 0x7BA0F06F, 2056, 2064
RANKS = [0, 500, 1700, 3600, 6200, 9500, 13500, 18200, 23600, 29700, 36500, 44300, 53100, 62900, 73700, 85500, 98300,
         112100, 126900, 142700, 159500, 177300, 196100, 215900, 236700, 258500, 281300, 305100, 329900, 355700, 382700,
         410900, 440300, 470900, 502700, 535700, 569900, 605300, 641900, 679700, 718700, 758900, 800300, 842900,
         886700, 931700, 977900, 1025300, 1073900, 1123700, 1175000, 1227800, 1282100, 1337900, 1395200, 1454000,
         1514300, 1576100, 1639400, 1704200, 1770500, 1838300, 1907600, 1978400, 2050700, 2124500, 2199800, 2276600,
         2354900, 2434700]


def stats(rng, package):
    buffer = bytearray(rng.randbytes(8188) if rng.randrange(3) == 0 else bytes(8188))
    struct.pack_into("<II", buffer, 0, VERSION if rng.randrange(12) else 154, FORMAT if rng.randrange(12) else 1)
    experience = rng.choice(RANKS + [rng.randrange(0, 2600000), 2516000, 2516001, 0xFFFFFFFF])
    struct.pack_into("<I", buffer, EXPERIENCE, experience)
    struct.pack_into("<I", buffer, PRESTIGE, rng.choice([0, 1, 5, 10, 11, 0xFFFFFFFF]))
    body = struct.pack(">I", zlib.crc32(bytes(buffer))) + bytes(buffer)
    if package:
        body = rng.randbytes(4) + body + rng.randbytes(1)
    if rng.randrange(20) == 0:
        body = body[:rng.randrange(len(body))]
    return body


def fields(rng, special):
    out = bytearray(b"SEMV" + rng.randbytes(8))
    entries = list(special)
    for _ in range(rng.randrange(0, 5)):
        entries.append((rng.randrange(256), rng.choice([1, 2, 4, 5, 6])))
    rng.shuffle(entries)
    for number, kind in entries:
        out += bytes([number, kind])
        if kind in (1, 2):
            out.append(rng.randrange(256))
        elif kind in (4, 5):
            out += struct.pack(">I", rng.choice([0, 3, 20, 25, 69, rng.randrange(1 << 32)]))
        else:
            room = rng.choice([0, 10, 22, 23, 24, 40])
            out += struct.pack(">HH", rng.randrange(1 << 16), room)
            out += bytes(rng.choice(b"0123") for _ in range(rng.choice([0, 5, 22, 23, 30]))) + b"\0"
    if rng.randrange(15) == 0:
        out = out[:rng.randrange(len(out) + 1)]
    if rng.randrange(25) == 0:
        out += bytes([1, 9])
    return bytes(out)


def profile_bin(rng):
    settings = [(0x63E83FFF, fields(rng, [(0x0E, 4)] if rng.randrange(8) else [])),
                (0x63E83FFE, fields(rng, ([(6, 1)] if rng.randrange(8) else []) + ([(0x23, 6)] if rng.randrange(8) else [])))]
    for _ in range(rng.randrange(0, 3)):
        settings.append((rng.randrange(1 << 32), rng.randbytes(rng.randrange(0, 20))))
    rng.shuffle(settings)
    if rng.randrange(10) == 0:
        settings = settings[1:]
    out = bytearray(b"MW2PROF1" + struct.pack("<I", len(settings) + (rng.randrange(3) if rng.randrange(15) == 0 else 0)))
    for id_, data in settings:
        out += struct.pack("<III", id_, rng.randrange(1 << 32), len(data)) + data
    if rng.randrange(15) == 0:
        out = out[:rng.randrange(len(out) + 1)]
    return bytes(out)


def make_saves(rng, folder):
    saves = f"{folder}/saves"
    os.makedirs(f"{saves}/online/user")
    ids = [rng.randrange(1, 1 << 48) for _ in range(rng.randrange(0, 5))] + [rng.randrange(1, 16)]
    stamp = 1_700_000_000_000_000_000
    files = []
    for id_ in ids:
        text = f"{id_:012x}"
        if rng.randrange(3):
            os.makedirs(f"{saves}/online/user/0009{text}")
            files.append(f"{saves}/online/user/0009{text}/mpdata")
            open(files[-1], "wb").write(stats(rng, False))
        if rng.randrange(3):
            os.makedirs(f"{saves}/mpdata_e000{text}")
            files.append(f"{saves}/mpdata_e000{text}/mpdata_e000{text}")
            open(files[-1], "wb").write(stats(rng, True))
    for _ in range(rng.randrange(0, 3)):
        # Stats nobody owns, and folders that are none.
        name = f"{rng.randrange(1 << 48):012x}"
        os.makedirs(f"{saves}/online/user/0009{name}", exist_ok=True)
        files.append(f"{saves}/online/user/0009{name}/mpdata")
        open(files[-1], "wb").write(stats(rng, False))
    os.makedirs(f"{saves}/online/user/empty", exist_ok=True)
    os.makedirs(f"{saves}/mpdata_broken", exist_ok=True)
    for i, path in enumerate(files):
        when = stamp + rng.randrange(0, 400 * 86400) * 1_000_000_000 + i
        os.utime(path, ns=(when, when))
    lines = []
    for id_ in ids:
        name = rng.choice(["Soap", "Roach", "Ghost Rider", "A" * 15, "B" * 20, "x y", "", "Nikolai", "Soap"])
        text = f"{id_:012x}"
        # Not "-" and digits: strtoull wraps it past twelve digits, and the C++ wrote back
        # only fifteen of the sixteen (its buffer), where the Rust writes them all, as the
        # game does (src/runtime/signin.rs).
        if rng.randrange(10) == 0:
            text = rng.choice(["0x" + text[2:], " " + text[1:], "zz" + text[2:], text[:6] + "\0" + text[7:], "000000000000"])
        lines.append(f"{text} {name}")
    if ids and rng.randrange(2):
        lines.append(f"first {rng.choice(ids + [0, 99]):012x}" + rng.choice(["", " trailing", "\0x"]))
    lines += rng.sample(["# a line of the game's", "short 1", "123456789012", "next 2"], rng.randrange(0, 3))
    rng.shuffle(lines)
    open(f"{saves}/profiles.txt", "wb").write(("\n".join(l + rng.choice(["", "", "\r", " "]) for l in lines) + "\n").encode("latin-1"))
    if rng.randrange(6):
        open(f"{saves}/profile.bin", "wb").write(profile_bin(rng))


def profile_script(rng):
    script = ["max", "list", "campaign"]
    for _ in range(rng.randrange(1, 10)):
        op = rng.choice(["rename", "play", "rank", "unlock", "campaign", "missions", "stars", "list"])
        if op == "rename":
            name = rng.choice([b"Price", b"Soap", b"Roach", b" lead", b"trail ", b"", b"A" * 16, b"Task Force", b"bad!",
                               b"caf\xe9", b"Ghost Rider"])
            script.append(f"rename {rng.randrange(100)} {escape(name)}")
        elif op in ("play", "unlock"):
            script.append(f"{op} {rng.randrange(100)}")
        elif op == "rank":
            script.append(f"rank {rng.randrange(100)} {rng.choice([0, 1, 2, 35, 69, 70, 71, -3])} {rng.choice([-1, 0, 3, 10, 11, -5])}")
        else:
            script.append(op)
        script.append("list")
    script.append("campaign")
    return "\n".join(script) + "\n"


def check_profile(program, work, rng, cases):
    for case in range(cases):
        def make(folder):
            make_saves(rng, folder)
            open(f"{folder}/script", "w").write(profile_script(rng))
        compare(program, work, "profile", make, lambda folder, _: ["script"])
    print(f"profile: all the same: {cases} cases")


# ---- scrub -----------------------------------------------------------------------------------

# No "report: graphics": a graphics line keeps what looks like an address, where the
# C++ took it out of a driver's version too (issue #50).
def scrub_log(rng, here, home):
    pieces = ["] online: ", "] online:", "online: ", "as \"Soap\"", "as \"\"", "as \"", "as \"Mc\"Tavish\"", "Soap",
              "192.168.1.20", "10.0.0.1", "127.0.0.1", "172.16.5.4", "172.32.5.4", "169.254.1.1", "8.8.8.8",
              "1.2.3.4.5", "01.002.3.4", "1234.1.1.1", "1.2.3.4567", "a1.2.3.4", "1.2.3.4a", "1.2.3.4_", "_1.2.3.4",
              "1.2.3", "999.999.999.999", "1..2.3.4", "1.2.3.4:28960", "0123456789abcdef", "0123456789ABCDEF0",
              "x0123456789abcdef", "0123456789abcde", "u:0123456789abcdef", "mpdata_fedcba9876543210",
              "user/0011223344556677", "user\\8899aabbccddeeff", "user:0011223344556677", "u:0123", "mpdata_e000",
              "\"connect 1.2.3.4:28960\"", "\"connect 1.2.3.4", "\"connect \"", "connect 5.6.7.8", " lan joins Bob",
              " lan invites Alice", " lan accepts Carol", " invites you", "[I] ", "[E] ----",
              "] report: frames 59.9", "runtime (multiplayer", "runtime (campaign", here, home,
              here + "/game", home + "/.local", here.replace("/", "\\"), " ", "\t", "é", "\xff\xfe", "\0", "\r",
              "steam 76561198000000000", "id 00000000deadbeef!"]
    lines = []
    for _ in range(rng.randrange(1, 60)):
        line = "".join(rng.choice(pieces) for _ in range(rng.randrange(0, 7)))
        if rng.randrange(4) == 0:
            line = "[12:00:00] online: " + line
        lines.append(line)
    return ("\n".join(lines) + rng.choice(["\n", "", "\r\n"])).encode("utf-8", errors="surrogateescape")


def check_scrub(program, work, rng, cases):
    for case in range(cases):
        def make(folder):
            os.makedirs(f"{folder}/game dir+ü")
            return None
        def arguments_of(folder, _):
            here = f"{folder}/game dir+ü"
            home = f"{folder}/home ü"
            open(f"{here}/run.log", "wb").write(scrub_log(rng, here, home))
            return [f"{here}/run.log"]
        # The program runs in the game folder, with that home.
        def environment(folder):
            return dict(os.environ, HOME=f"{folder}/home ü")
        compare_in(program, work, "scrub", make, arguments_of, environment, "game dir+ü")
    print(f"scrub: all the same: {cases} cases")


# compare() for a program that runs in a folder inside the case.
def compare_in(program, work, module, make, arguments_of, environment, inner):
    case = f"{work}/case"
    shutil.rmtree(case, ignore_errors=True)
    os.makedirs(case)
    make(case)
    outputs = {}
    for side in ("cpp", "rust"):
        folder = f"{work}/{side}"
        shutil.rmtree(folder, ignore_errors=True)
        shutil.copytree(case, folder)
    # The same log for both: made once, in the first side's folder, then copied with its
    # folders renamed, so that each names its own.
    log = arguments_of(f"{work}/cpp", None)[0]
    data = open(log, "rb").read()
    open(f"{work}/rust/{inner}/run.log", "wb").write(data.replace(f"{work}/cpp".encode(), f"{work}/rust".encode()))
    open(f"{case}/run.log", "wb").write(data)
    for side in ("cpp", "rust"):
        folder = f"{work}/{side}"
        result = subprocess.run([program, module, side, f"{folder}/{inner}/run.log"], cwd=f"{folder}/{inner}",
                                capture_output=True, env=environment(folder), timeout=120)
        if result.returncode:
            sys.exit(f"{module}: the {side} side ended {result.returncode}: {result.stderr.decode(errors='replace')}\ncase in {case}")
        # The folder as it is written, and as a page's text writes it (%XX for all but
        # letters, digits and -_.~).
        named = f"{work}/{side}".encode()
        encoded = "".join(chr(c) if chr(c).isascii() and (chr(c).isalnum() or chr(c) in "-_.~") else f"%{c:02X}"
                          for c in named).encode()
        outputs[side] = result.stdout.replace(named, b"<work>").replace(encoded, b"<work>")
    if outputs["cpp"] != outputs["rust"]:
        a, b = outputs["cpp"].decode(errors="replace").split("\n"), outputs["rust"].decode(errors="replace").split("\n")
        at = next(i for i in range(max(len(a), len(b))) if i >= len(a) or i >= len(b) or a[i] != b[i])
        sys.exit(f"DIFFERENT: {module}: line {at + 1}\n  C++:  {a[at] if at < len(a) else '<end>'}\n"
                 f"  Rust: {b[at] if at < len(b) else '<end>'}\ncase in {case}")


# ---- sound -----------------------------------------------------------------------------------

ALIASES = ["user_interface/ui_over_v2", "user_interface/ui_select_hz_1"]
MAGIC = b"IWffu100\x00\x00\x01\x0D"


def sound_zone(rng):
    zone = bytearray(rng.randbytes(rng.randrange(0x200, 0x3000)))
    for _ in range(rng.randrange(0, 4)):
        name = rng.choice(ALIASES + ["user_interface/ui_over", "other/sound"]).encode()
        structure = bytearray(rng.randbytes(0xA4))
        size = rng.choice([2048, 4096, 0, 2047, 2049, 1 << 20, 8192])
        struct.pack_into(">I", structure, 0x0C, size)
        structure[0x60] = rng.choice([4, 4, 4, 3])
        structure[0x64] = rng.choice([1, 1, 1, 2])
        struct.pack_into(">I", structure, 0x68, rng.choice([8000, 44100, 48000, 7999, 48001, 22050, 0xFFFFFFFF]))
        structure[0x6C] = rng.choice([1, 1, 1, 2])
        structure[0xA0:0xA4] = b"\xff\xff\xff\xff"
        data = rng.randbytes(min(size, 9000) if rng.randrange(4) else rng.randrange(0, 3000))
        at = rng.randrange(0, len(zone))
        zone[at:at] = structure + name + b"\0" + data
    return bytes(zone)


def check_sound(program, work, rng, cases):
    real = [path for path in sorted(glob.glob(f"{ROOT}/mw2/game/*.ff") + glob.glob(f"{ROOT}/mw2/update/*.ff"))
            if os.path.isfile(path) and os.path.getsize(path) <= 2 << 20]
    count = 0
    for path in real:
        def make(folder, path=path):
            shutil.copyfile(path, f"{folder}/file.ff")
        out = compare(program, work, "sound", make, lambda folder, _: ["file.ff"] + ALIASES + ["code_post_gfx"])
        count += 1
        if path.endswith("code_post_gfx.ff"):
            print(f"sound: code_post_gfx.ff: {out.decode().strip()}")
    for case in range(cases):
        def make(folder):
            zone = sound_zone(rng)
            header = bytearray(MAGIC + rng.randbytes(0x25 - len(MAGIC)))
            struct.pack_into(">I", header, 0x19, 0 if rng.randrange(12) else 1)
            if rng.randrange(15) == 0:
                header[3] ^= 0x20
            open(f"{folder}/file.ff", "wb").write(bytes(header) + zlib.compress(zone, rng.randrange(10)))
        compare(program, work, "sound", make, lambda folder, _: ["file.ff"] + ALIASES)
        count += 1
    print(f"sound: all the same: {len(real)} of the game's fastfiles, {count} cases")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("build")
    parser.add_argument("--cases", type=int, default=2000)
    parser.add_argument("--old", default="0c60bf6")
    parser.add_argument("--src", default=f"{ROOT}/launcher/src")
    parser.add_argument("--work")
    parser.add_argument("--only")
    arguments = parser.parse_args()
    work = os.path.abspath(arguments.work or f"{arguments.build}/launcher_diff")
    os.makedirs(work, exist_ok=True)
    program = build(arguments, work)
    rng = random.Random(11)
    checks = {"settings": check_settings, "profile": check_profile, "scrub": check_scrub, "sound": check_sound}
    for name, check in checks.items():
        if arguments.only in (None, name):
            check(program, work, rng, arguments.cases // (4 if name == "sound" else 1))


main()
