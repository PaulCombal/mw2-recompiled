cmake_minimum_required(VERSION 3.20)
project(mw2recomp CXX)

# What cargo xtask build hands CMake: the game and the launcher are Cargo's
# (Cargo.toml, build.rs, launcher/), and this holds their options (the
# launcher's build.rs reads MW2_VERSION and MW2_LOGGING from the cache), makes a
# Windows launcher's Vulkan import library, and fetches the zlib a test tool
# compares against.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

option(MW2_LOGGING "Emit the log" ON)
option(MW2_PORTABLE "A player's build (cargo xtask build --release)" OFF)

# ---- which executable ------------------------------------------------------
# The launcher is the same for the campaign's executable and the multiplayer's.
# Which version of them it installs: the one title update 6 makes of the disc's
# (tu6), or the disc's own (tu0), as it is. The install is Rust: the launcher's
# build.rs reads this from the cache, and hashes the files it knows the version
# by from mw2/ (launcher/src/setup.rs).
set(MW2_VERSION "tu6" CACHE STRING "Which version of the executable: tu6 (title update 6) or tu0 (the disc's)")
if(NOT MW2_VERSION MATCHES "^(tu6|tu0)$")
    message(FATAL_ERROR "MW2_VERSION must be tu6 or tu0, not '${MW2_VERSION}'")
endif()
if(NOT MW2_LOGGING)
    # Compiles every LOGI/LOGW/LOGE/LOGK call to nothing, warnings included.
    add_compile_definitions(MW2_NO_LOGGING=1)
endif()

# ---- how the code is optimised ---------------------------------------------
# Every target below, the libraries fetched included, is compiled for ThinLTO,
# optimised across files at the link (docs/building.md, "Optimisation"). The
# game's C++ is compiled the same way by build.rs.
option(MW2_LTO "Optimise across files at the link (ThinLTO)" ON)
# Where the linker keeps what ThinLTO made of each module, for the next link to
# reuse what did not change. A result taken from it is the one the linker would
# have made, so a folder kept between builds (the release workflow's) only
# makes the link faster.
set(MW2_LTO_CACHE "${CMAKE_BINARY_DIR}/lto-cache" CACHE PATH "The ThinLTO cache folder")
if(MW2_LTO)
    add_compile_options(-flto=thin)
    add_link_options(-flto=thin "-Wl,--thinlto-cache-dir=${MW2_LTO_CACHE}")
    if(NOT WIN32)
        # The system linker reads no LLVM bitcode; llvm-mingw's already is lld.
        add_link_options(-fuse-ld=lld)
    endif()
    # Static libraries of bitcode need LLVM's archiver to be indexed; CMake
    # takes it with clang.
    if(NOT CMAKE_AR MATCHES "llvm-ar")
        message(FATAL_ERROR "ThinLTO needs llvm-ar, not ${CMAKE_AR}: build with clang (cargo xtask), or -DMW2_LTO=OFF")
    endif()
endif()

include(FetchContent)

# ---- Vulkan ----------------------------------------------------------------
# The launcher links the loader by name; for Windows its import library is made
# here (target vulkan_import_library, which launcher/build.rs builds) from the
# headers of the version the game's build.rs checks for.
if(WIN32)
    include(cmake/vulkan-windows.cmake)
endif()

# ---- zlib ------------------------------------------------------------------
# What tools/inflate_diff.py holds the Rust inflate (install/src/inflate.rs) to.
FetchContent_Declare(zlib
    GIT_REPOSITORY https://github.com/madler/zlib.git
    GIT_TAG v1.3.1
    GIT_SHALLOW TRUE
    EXCLUDE_FROM_ALL)
set(ZLIB_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(zlib)

