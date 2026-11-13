# Running the MW3 smoke test on your own machine

This repeats what the CI job does (`.github/workflows/mw3-validation.yml`,
job `generated-code-build`), without GitHub's LFS bandwidth limit. It was written
from that workflow and `tools/build_mw3.sh`; it has not been run on a local
machine yet, so if a step fails, the error message is the useful thing to send.

Your game files never leave your machine and are never committed: `mw3/` is
git-ignored.

## What you need

- Ubuntu 24.04, or Windows with WSL2 running Ubuntu 24.04. On WSL, keep the
  repository and game files inside the Linux filesystem (`~/`), not under
  `/mnt/c`, which is many times slower.
- About 25 GB of free disk space once all image packs are in place, and 8 GB of
  RAM or more. The recompile builds about 36,000 functions into a large library.
- Your own game files, extracted from your disc.

```sh
sudo apt-get update
sudo apt-get install -y clang-18 lld-18 ninja-build python3 cmake git binutils libvulkan-dev
```

## 1. Get the code

```sh
git clone https://github.com/the-drk/mw3recomp.git
cd mw3recomp
git checkout mw3-bringup
```

## 2. Stage the game files

Put your files in `mw3/tu0/` keeping the disc's names and folders. The title
reads them as `D:\<name>`, so `default.xex` and the fastfiles and packs all sit
directly in that folder:

```
mw3/tu0/default.xex
mw3/tu0/code_post_gfx.ff
mw3/tu0/ui.ff
mw3/tu0/common.ff
mw3/tu0/imagefile1.pak
mw3/tu0/imagefile3.pak
mw3/tu0/imagefile4.pak     (0.95 GB, requested next)
mw3/tu0/imagefile2.pak     (1.89 GB, not requested yet)
```

Check that `default.xex` is the supported executable before anything else; the
build refuses any other:

```sh
sha256sum mw3/tu0/default.xex
# d90bc8d93b7b8f6001ead8330271196a6ef7a5c2e4a9e9b63118f47d58a05912
```

Do not create placeholders for files your dump lacks (`patch.ff`, `ui.ffm`,
`.pakm` files). The title asks for them, does not get them and carries on.

## 3. Recompile and build (about 10 minutes on a modern desktop)

```sh
bash tools/build_mw3.sh --compile-only
cmake -S . -B build-mw3 -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_COMPILER=clang-18 -DCMAKE_CXX_COMPILER=clang++-18 \
  -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON -DMW3_LINK_PROBE=ON \
  -DMW2_USE_SDL=OFF -DMW2_LTO=OFF
cmake --build build-mw3 --target mw2 --parallel
```

The first command regenerates `ppc_mw3/` from your executable and compiles it.
The second and third link the diagnostic runtime to `build-mw3/mw3`.

## 4. Run it

```sh
MW2_RUN_SECONDS=12 MW2_LOG_FILE=/tmp/mw3.log \
  ./build-mw3/mw3 mw3/tu0/default.xex mw3/tu0
tail -n 120 /tmp/mw3.log
```

`MW2_RUN_SECONDS` ends the run and prints the end-of-run report after that many
seconds. Raise it (try 60) once the title stops failing early. Without SDL there
is no window and no sound; it submits graphics commands and counts them, and the
renderer reports `no Vulkan instance` or `no device came up` when the machine
has no usable Vulkan driver, which does not stop the run.

## 5. Reading the log

Look for these, in this order:

1. `XamLoaderSetLaunchData: ... text "eERROR: Could not find zone 'X.ff'"`: the
   title's fatal-error handler naming a zone it could not open. Add that file.
2. `file not found: D:\<name>` followed by `the title reported a dirty disc`:
   a required file is missing. The last `file not found` before it is the one.
   `patch.ff`, `patch.ffm`, `ui.ffm` and `update:\patch.ff` are optional and
   always appear.
3. The end-of-run report: `gpu: N frames`, `N draws`, `audio: N callbacks`,
   `unimplemented imports actually reached`, and `files the guest asked for and
   did not get`. A run that lasts the full `MW2_RUN_SECONDS` with frames and no
   `exiting:` line has not hit any of the above.

The log contains file names, counts and addresses, not game data, so it is safe
to paste into an issue or a chat.

## Where the run stands

See [MW3-BRINGUP.md](MW3-BRINGUP.md): with `ui.ff`, `common.ff`,
`imagefile1.pak` and `imagefile3.pak` supplied the title reaches
`imagefile4.pak`, rendering 17 frames before the dirty-disc error.
