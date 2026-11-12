# Building the MW3 generated-code target

MW3 is not yet ready to link against the MW2 runtime. Its campaign executable
has its own entry point, globals and engine hooks; the addresses in
`runtime/title.h` are MW2 addresses. Do not enable the MW2 runtime for MW3 by
copying or guessing those addresses.

## Build the current MW3 milestone

Place your own disc executable at `mw3/tu0/default.xex`, then run:

```sh
./tools/build_mw3.sh
```

The script runs the existing MW3 XEX/recompiler pipeline and then configures
CMake with `MW2_TITLE=mw3` and `MW3_RECOMP_ONLY=ON`. The resulting target
compiles the generated PPC translation units into
`build-mw3/libppc_recomp.a` without compiling or linking MW2 runtime sources.

Equivalent CMake commands, after `ppc_mw3/` has been generated:

```sh
cmake -S . -B build-mw3 -G Ninja \
  -DCMAKE_C_COMPILER=clang-18 \
  -DCMAKE_CXX_COMPILER=clang++-18 \
  -DMW2_TITLE=mw3 -DMW3_RECOMP_ONLY=ON
cmake --build build-mw3 --target mw3-recomp-check --parallel
```

## Scope and limitations

- This milestone validates compilation of generated MW3 PPC C++ only.
- It does not produce a desktop game executable, PS4 ELF, PKG or playable build.
- The runtime hook table in `runtime/title.h` remains MW2-specific.
- MW3 hook candidates must be checked against the exact MW3 XEX before they
  are added. Unknown hooks stay undefined rather than inheriting MW2 addresses.
- The generated `ppc_mw3/` directory and extracted game executable are local
  inputs/artifacts and are not committed.
