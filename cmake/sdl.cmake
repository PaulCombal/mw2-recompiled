# How the game's SDL is built: sdl3-sys (Cargo.toml) builds it from its source with
# CMake and reads this as its toolchain file (.cargo/config.toml names it). The
# options are the ones the game had when CMake fetched SDL itself, and the compiler
# the rest of the game's C and C++ is built with.
if(NOT CMAKE_C_COMPILER)
    set(CMAKE_C_COMPILER clang-18)
    set(CMAKE_CXX_COMPILER clang++-18)
endif()
# Only the static library; sdl3-sys asks for it.
set(SDL_SHARED OFF CACHE BOOL "" FORCE)
set(SDL_TEST_LIBRARY OFF CACHE BOOL "" FORCE)
# Only asks X11 not to blank the screen, which SDL's other ways do as well, and an
# optional library that would have to be installed to build this.
set(SDL_X11_XSCRNSAVER OFF CACHE BOOL "" FORCE)
