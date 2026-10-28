#!/bin/bash
# Records the profiles every build is optimised with (CMakeLists.txt, "how the
# code is optimised"): pgo/sp.tar.xz and pgo/mp.tar.xz. Builds each title with
# counters (MW2_PGO_TRAINING) into out/pgo-sp and out/pgo-mp, plays it -- the
# campaign flies Team Player and S.S.D.D., the multiplayer walks Terminal and
# Karachi -- and keeps what the counters saw. About 15 minutes; the game's
# windows open and close by themselves, and want no input.
#   Prereqs: the game built once (build.sh), llvm-profdata-18.
#   TITLES="sp" records only the campaign's.
# Commit the two archives: a release build uses what is in pgo/. Record again
# after large changes to the runtime; a function changed since the recording is
# compiled without counts, so an old profile helps less, and breaks nothing.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
WORK=out/pgo-record
PROFDATA=$(command -v llvm-profdata-18 || command -v llvm-profdata) || { echo "llvm-profdata-18 is missing"; exit 1; }

# One run of the training executable, capped in memory, ended by the run
# itself once its route is done. $1 = name, then the environment and the title.
play() {
    local name=$1; shift
    local log=$WORK/$name.log
    command -v xdotool >/dev/null && xdotool mousemove 0 0
    echo "    $name"
    env LLVM_PROFILE_FILE="$ROOT/$WORK/$TITLE-%p.profraw" MW2_LOG_FILE=$log MW2_WATCHDOG=300 MW2_QUIT_AFTER_ARRIVAL=1 "$@" \
        systemd-run --user --scope --quiet -p MemoryMax=6G -p MemorySwapMax=0 \
        $BUILD/mw2 mw2/$PE mw2/game >/dev/null 2>&1 || true
    if ! grep -q "exiting: the walk arrived" $log; then
        echo "$name did not finish its route; see $log"; exit 1
    fi
}

rm -rf $WORK && mkdir -p $WORK pgo
for TITLE in ${TITLES:-sp mp}; do
    BUILD=out/pgo-$TITLE
    echo "==> $TITLE: the executable with counters, in $BUILD"
    TITLE=$TITLE BUILD_DIR=$BUILD REGENERATE=0 TARGETS=mw2 CMAKE_EXTRA="-DMW2_PGO_TRAINING=ON" ./build.sh > $WORK/$TITLE-build.log 2>&1 \
        || { echo "the build failed; see $WORK/$TITLE-build.log"; exit 1; }
    echo "==> $TITLE: playing"
    if [ $TITLE = sp ]; then
        PE=default.pe
        for flight in roadkill:roadkill-flight trainer:ssdd-flight; do
            play ${flight#*:} MW2_INPUT_SCRIPT="200:a" MW2_FLY_PATH=tools/routes/${flight#*:}.txt \
                MW2_CONSOLE="1:set ui_autoContinue 1;2:devmap ${flight%%:*};20:god;21:noclip"
        done
    else
        PE=default_mp.pe
        for route in mp_terminal:terminal mp_checkpoint:karachi; do
            play ${route#*:} MW2_FPS_LIMIT=0 MW2_TEAM_UP=1 MW2_NET_LINK=1 MW2_WALK_PATH=tools/routes/${route#*:}.txt \
                MW2_CONSOLE="3:map ${route%%:*}"
        done
    fi
    "$PROFDATA" merge -o $WORK/$TITLE.profdata $WORK/$TITLE-*.profraw
    # The same bytes for the same profile: no dates or owners in the archive.
    tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C $WORK -cf - $TITLE.profdata | xz -9 > pgo/$TITLE.tar.xz
    echo "    pgo/$TITLE.tar.xz: $(du -h pgo/$TITLE.tar.xz | cut -f1)"
done
echo "==> done: rebuild (the profile is taken at the next configure) and commit pgo/"
