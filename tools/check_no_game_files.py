#!/usr/bin/env python3
"""Fail if proprietary game files, or pointers to them, are tracked by git.

Game files stay on the developer's own machine. They are never committed,
never put in Git LFS, never uploaded to a private asset repository and never
fetched by CI. This check runs in CI and can be used as a pre-commit hook:

    python3 tools/check_no_game_files.py            # check tracked files
    python3 tools/check_no_game_files.py --staged   # check what is staged
    python3 tools/check_no_game_files.py --self-test
"""
import subprocess
import sys
from pathlib import PurePosixPath

FORBIDDEN_SUFFIXES = {
    ".xex", ".xexp", ".pe", ".iso", ".god", ".xbe",
    ".ff", ".ffm", ".pak", ".bik", ".xma", ".xwb", ".xsb", ".xpr",
    ".ppc_dump",
}
FORBIDDEN_DIRS = {"mw2", "mw3", "ppc", "ppc_mw3", "ppc_mp", "ppc_tu0", "ppc_mp_tu0"}
LFS_MAGIC = b"version https://git-lfs.github.com/spec/"
# Directories where a name like "mw3" is a legitimate source directory.
ALLOWED_PREFIXES = ("docs/", "tools/", "tests/", "config/", "runtime/", ".github/")


def violations(paths, read_head):
    bad = []
    for p in paths:
        pp = PurePosixPath(p)
        if pp.suffix.lower() in FORBIDDEN_SUFFIXES:
            bad.append((p, "game-file extension"))
            continue
        if pp.parts and pp.parts[0] in FORBIDDEN_DIRS and not p.startswith(ALLOWED_PREFIXES):
            bad.append((p, "game-data or generated directory"))
            continue
        if pp.name == ".gitattributes":
            continue
        if read_head(p).startswith(LFS_MAGIC):
            bad.append((p, "Git LFS pointer (game files must not go through LFS)"))
    return bad


def lfs_attributes(read_text):
    try:
        text = read_text(".gitattributes")
    except OSError:
        return []
    return [ln for ln in text.splitlines() if "filter=lfs" in ln]


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True).stdout


def main(argv):
    if "--self-test" in argv:
        return self_test()
    staged = "--staged" in argv
    names = git("diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR") if staged \
        else git("ls-files", "-z")
    paths = [n.decode() for n in names.split(b"\0") if n]

    def read_head(p):
        try:
            with open(p, "rb") as f:
                return f.read(64)
        except OSError:
            return b""

    bad = violations(paths, read_head)
    for ln in lfs_attributes(lambda f: open(f, encoding="utf-8").read()):
        bad.append((".gitattributes", f"LFS rule: {ln.strip()}"))
    if bad:
        print("Proprietary game data must never be committed or uploaded:")
        for p, why in bad:
            print(f"  {p}: {why}")
        return 1
    print(f"ok: {len(paths)} tracked files, no game data, no LFS")
    return 0


def self_test():
    heads = {"x/lfs.bin": LFS_MAGIC + b"sha256:..."}
    rh = lambda p: heads.get(p, b"")
    assert violations(["mw3/tu0/default.xex"], rh)
    assert violations(["assets/code_post_gfx.ff"], rh)
    assert violations(["assets/imagefile1.pak"], rh)
    assert violations(["x/lfs.bin"], rh)
    assert violations(["mw3/anything.txt"], rh)
    assert violations(["ppc_mw3/ppc_recomp.0.cpp"], rh)
    assert not violations(["docs/MW3-BUILD.md", "tools/title.py", "runtime/main.cpp",
                           "tests/xex_header_test.cpp", "config/MW3.toml.in"], rh)
    assert lfs_attributes(lambda f: "*.ff filter=lfs diff=lfs merge=lfs -text\n")
    assert not lfs_attributes(lambda f: "*.sh text eol=lf\n")
    print("self-test ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
