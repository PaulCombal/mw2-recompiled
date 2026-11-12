#!/bin/bash
# MW3 campaign: flat image -> jump tables -> recompiled C++ (docs/MW3-BRINGUP.md,
# steps 1 and 3). build.sh is MW2's pipeline and has no MW3 title yet; this runs
# the same XenonRecomp steps for MW3 alone.
#   Input: mw3/tu0/default.xex (your disc's executable) and config/MW3.toml.
#   Output: mw3/tu0/default.pe, config/mw3_switch_tables.toml, ppc_mw3/.
#   Prereqs: cmake ninja-build clang-18 python3 git (as build.sh).
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
XEX=mw3/tu0/default.xex
[ -f "$XEX" ] || { echo "$XEX is missing: put the disc's default.xex there"; exit 1; }
[ -f config/MW3.toml ] || { echo "config/MW3.toml is missing: see config/MW3.toml.in"; exit 1; }

XENON_COMMIT=ddd128bcca99fe8bfbb99bea583c972351fa6ace   # as build.sh
if [ ! -d XenonRecomp ]; then
    git clone --recursive https://github.com/hedge-dev/XenonRecomp.git
    git -C XenonRecomp checkout -q "$XENON_COMMIT"
    git -C XenonRecomp submodule update --init --recursive
    git -C XenonRecomp apply "$ROOT/patches/xenonrecomp-mw2.patch"
fi
cmake -B XenonRecomp/build -S XenonRecomp -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_C_COMPILER=clang-18 -DCMAKE_CXX_COMPILER=clang++-18 >/dev/null
cmake --build XenonRecomp/build -j"$(nproc)" >/dev/null

echo "sha256: $(sha256sum $XEX | cut -d' ' -f1)"
python3 tools/xexdump.py "$XEX" mw3/tu0/default.pe | head -2
./XenonRecomp/build/XenonAnalyse/XenonAnalyse "$XEX" config/mw3_switch_tables.toml >/dev/null
# XenonRecomp crashes, rather than complaining, when its output directory is missing.
rm -rf ppc_mw3 && mkdir -p ppc_mw3
( cd config && ../XenonRecomp/build/XenonRecomp/XenonRecomp MW3.toml ../XenonRecomp/XenonUtils/ppc_context.h ) > /tmp/mw3_recomp.log
echo "switch-case errors: $(grep -c 'trying to jump outside function' /tmp/mw3_recomp.log || true)"
echo "files: $(ls ppc_mw3 | wc -l)  (log: /tmp/mw3_recomp.log)"
