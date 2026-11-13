#!/usr/bin/env python3
"""Reject unsupported MW3 inputs before generating address-dependent code."""
import argparse
import hashlib
from pathlib import Path

SUPPORTED_SHA256 = "d90bc8d93b7b8f6001ead8330271196a6ef7a5c2e4a9e9b63118f47d58a05912"


def verify(path: Path, expected: str = SUPPORTED_SHA256) -> None:
    with path.open("rb") as stream:
        prefix = stream.read(128)
        if prefix.startswith(b"version https://git-lfs.github.com/spec/v1"):
            raise ValueError("MW3 XEX is an LFS pointer; supply the actual executable")
        if prefix[:4] != b"XEX2":
            raise ValueError("MW3 executable does not have an XEX2 header")
        digest = hashlib.sha256(prefix)
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected:
        raise ValueError("Unsupported MW3 executable; refusing to use fixed title addresses")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("xex", type=Path, nargs="?", default=Path("mw3/tu0/default.xex"))
    args = parser.parse_args()
    try:
        verify(args.xex)
    except (OSError, ValueError) as error:
        # Do not print input bytes, derived keys or the input's actual digest.
        if isinstance(error, OSError):
            print("MW3 executable is missing or unreadable")
        else:
            print(error)
        return 1
    print("Supported MW3 executable verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
