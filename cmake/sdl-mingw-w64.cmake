# cmake/sdl.cmake for the Windows cross-build: llvm-mingw's compilers.
include("${CMAKE_CURRENT_LIST_DIR}/mingw-w64.cmake")
include("${CMAKE_CURRENT_LIST_DIR}/sdl.cmake")
# The cmake crate names a resource compiler of its own when it finds the C compiler
# (llvm-mingw on PATH, or CC set): the clang, which is none. A configure that met both
# would start again without this file.
set(CMAKE_RC_COMPILER "${LLVM_MINGW}/bin/x86_64-w64-mingw32-windres" CACHE FILEPATH "" FORCE)
