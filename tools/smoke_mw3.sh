#!/usr/bin/env bash
# Local diagnostic build/run. Raw output stays local; nothing is uploaded.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -m)" != x86_64 || "$(uname -s)" != Linux ]]; then
    echo "This diagnostic build currently requires x86-64 Linux." >&2
    exit 1
fi
seconds=${MW2_RUN_SECONDS:-12}
if [[ ! "$seconds" =~ ^[1-9][0-9]*$ ]] || (( seconds > 3600 )); then
    echo "MW2_RUN_SECONDS must be an integer from 1 to 3600." >&2
    exit 1
fi
command -v timeout >/dev/null
python3 tools/check_mw3_xex.py
export JOBS=${JOBS:-2}
if [[ ! "$JOBS" =~ ^[1-9][0-9]*$ ]]; then
    echo "JOBS must be a positive integer." >&2
    exit 1
fi
bash tools/build_mw3.sh --compile-only
cmake -S . -B build-mw3 -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_COMPILER=clang-18 -DCMAKE_CXX_COMPILER=clang++-18 \
    -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON -DMW3_LINK_PROBE=ON \
    -DMW2_USE_SDL=OFF -DMW2_LTO=OFF
cmake --build build-mw3 --target mw2 --parallel "${JOBS:-2}"
# Logs may contain private paths and guest diagnostics. Do not print/upload them.
umask 077
log=$(mktemp "${TMPDIR:-/tmp}/mw3-smoke.XXXXXXXX.log")
status=0
ulimit -c 0
timeout --signal=TERM --kill-after=5s "$((seconds + 30))s" \
    env MW2_RUN_SECONDS="$seconds" MW2_LOG_FILE="$log" \
    ./build-mw3/mw3 mw3/tu0/default.xex mw3/tu0 >"$log.console" 2>&1 || status=$?
if (( status != 0 && status != 1 )); then
    echo "Diagnostic runner failed or timed out (status $status). Private logs: $log"
    exit 1
fi
for marker in 'function table:' 'entering guest at' 'memory: virtual heap'; do
    if ! grep -Fq "$marker" "$log"; then
        echo "Diagnostic runner did not complete its startup/report checks. Private logs: $log"
        exit 1
    fi
done
if grep -Eq 'dirty disc|Could not find zone' "$log" "$log.console"; then
    echo "Guest reported a disc/zone failure. Inspect private logs: $log"
    exit 1
fi
echo "Startup/report checks passed; menu and gameplay remain unverified. Private logs: $log"
