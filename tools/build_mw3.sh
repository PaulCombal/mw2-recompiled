#!/usr/bin/env bash
# Build MW3's generated PPC translation units as a static library.
# This intentionally does not link the MW2 runtime: MW3 hook addresses are
# incomplete and must not be guessed.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

for tool in cmake ninja clang-18 clang++-18 python3 git nm objdump; do
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

# Generate the same shared declarations/import inventory used by the MW2
# executable build, but explicitly route title.py to MW3. These are diagnostic
# glue files only; this does not assert that the MW2 runtime implements MW3.
export MW2_TITLE=mw3
export MW2_VERSION=tu0
python3 tools/genshared.py
python3 tools/gen_kernel_stubs.py

# Record unresolved archive symbols and host relocations without publishing
# generated game code or the user's executable.
python3 tools/audit_mw3_generated.py build-mw3/libppc_recomp.a build-mw3/mw3-runtime-audit.txt

echo "MW3 generated PPC library compiled: build-mw3/libppc_recomp.a"
echo "MW3 runtime audit metadata: build-mw3/mw3-runtime-audit.txt"
echo "This is a code-generation/build milestone, not a playable desktop build."
