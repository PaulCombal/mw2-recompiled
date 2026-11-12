#!/usr/bin/env bash
# Build MW3's generated PPC translation units as a static library.
# This intentionally does not link the MW2 runtime: MW3 hook addresses are
# incomplete and must not be guessed.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

for tool in cmake ninja clang-18 clang++-18 python3 git; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Missing prerequisite: $tool" >&2
        exit 1
    }
done

# Extract/flatten the user's MW3 XEX, analyse jump tables, and regenerate ppc_mw3/.
bash ./tools/recomp_mw3.sh

# Compile the generated PPC code without selecting or linking MW2 runtime hooks.
cmake -S . -B build-mw3 -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_COMPILER=clang-18 \
    -DCMAKE_CXX_COMPILER=clang++-18 \
    -DMW2_TITLE=mw3 \
    -DMW3_RECOMP_ONLY=ON \
    -DMW2_LTO=OFF

cmake --build build-mw3 --target mw3-recomp-check --parallel "${JOBS:-$(nproc)}"

echo "MW3 generated PPC library compiled: build-mw3/libppc_recomp.a"
echo "This is a code-generation/build milestone, not a playable desktop build."
