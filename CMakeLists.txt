cmake_minimum_required(VERSION 3.20)
project(mw2recomp CXX)

# The launcher and the tools. The game is Cargo's (Cargo.toml, build.rs), and
# cargo xtask build hands CMake the rest: the launcher, what it fetches, and the
# tools below.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

option(MW2_LOGGING "Emit the log" ON)
option(MW2_PORTABLE "Link the C++ runtime statically, for a binary that runs on other machines" OFF)

# ---- which executable ------------------------------------------------------
# The launcher is the same for the campaign's executable and the multiplayer's.
# Which version of them it installs: the one title update 6 makes of the disc's
# (tu6), or the disc's own (tu0), as it is.
set(MW2_VERSION "tu6" CACHE STRING "Which version of the executable: tu6 (title update 6) or tu0 (the disc's)")
if(MW2_VERSION STREQUAL "tu0")
    add_compile_definitions(MW2_VERSION_TU0=1)
elseif(NOT MW2_VERSION STREQUAL "tu6")
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

# What release this is: the release workflow gives the tag. The launcher looks
# for a newer one; a build without a tag does not update.
set(MW2_RELEASE_TAG "" CACHE STRING "The release tag this build is published as, e.g. v0.3.0")

# The launcher installs only the disc it is an update of, and only that update:
# each known by its SHA-256, read here from mw2/ (launcher/setup.cpp).
if(MW2_VERSION STREQUAL "tu0")
    set(MW2_XEX_DIR "${CMAKE_SOURCE_DIR}/mw2/tu0")
else()
    set(MW2_XEX_DIR "${CMAKE_SOURCE_DIR}/mw2")
endif()
function(mw2_sha256 variable path)
    if(NOT EXISTS "${path}")
        message(FATAL_ERROR "${path} is missing -- cargo xtask prepare makes it from the ISO and the title update")
    endif()
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${path}")
    file(SHA256 "${path}" hash)
    set(${variable} "${hash}" PARENT_SCOPE)
endfunction()
mw2_sha256(MW2_XEX_SHA256_SP  "${MW2_XEX_DIR}/default.xex")
mw2_sha256(MW2_XEX_SHA256_MP  "${MW2_XEX_DIR}/default_mp.xex")
mw2_sha256(MW2_DISC_SHA256_SP "${CMAKE_SOURCE_DIR}/mw2/tu0/default.xex")
mw2_sha256(MW2_DISC_SHA256_MP "${CMAKE_SOURCE_DIR}/mw2/tu0/default_mp.xex")
set(MW2_HASH_DEFINITIONS
    MW2_XEX_SHA256_SP="${MW2_XEX_SHA256_SP}" MW2_XEX_SHA256_MP="${MW2_XEX_SHA256_MP}"
    MW2_DISC_SHA256_SP="${MW2_DISC_SHA256_SP}" MW2_DISC_SHA256_MP="${MW2_DISC_SHA256_MP}")
if(NOT MW2_VERSION STREQUAL "tu0")
    mw2_sha256(MW2_UPDATE_SHA256_PATCH_MP   "${CMAKE_SOURCE_DIR}/mw2/update/patch_mp.ff")
    mw2_sha256(MW2_UPDATE_SHA256_DLC1_UI_MP "${CMAKE_SOURCE_DIR}/mw2/update/dlc1_ui_mp.ff")
    mw2_sha256(MW2_UPDATE_SHA256_DLC2_UI_MP "${CMAKE_SOURCE_DIR}/mw2/update/dlc2_ui_mp.ff")
    list(APPEND MW2_HASH_DEFINITIONS
        MW2_UPDATE_SHA256_PATCH_MP="${MW2_UPDATE_SHA256_PATCH_MP}"
        MW2_UPDATE_SHA256_DLC1_UI_MP="${MW2_UPDATE_SHA256_DLC1_UI_MP}"
        MW2_UPDATE_SHA256_DLC2_UI_MP="${MW2_UPDATE_SHA256_DLC2_UI_MP}")
endif()

include(FetchContent)
FetchContent_Declare(SDL3
    GIT_REPOSITORY https://github.com/libsdl-org/SDL.git
    GIT_TAG release-3.4.18
    GIT_SHALLOW TRUE)
set(SDL_SHARED OFF CACHE BOOL "" FORCE)
set(SDL_STATIC ON  CACHE BOOL "" FORCE)
set(SDL_TEST   OFF CACHE BOOL "" FORCE)
# Only asks X11 not to blank the screen, which SDL's other ways do as well, and
# an optional library that would have to be installed to build this.
set(SDL_X11_XSCRNSAVER OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(SDL3)
# The game has the SDL sdl3-sys builds (cmake/sdl.cmake), of the same version.

# ---- Vulkan ----------------------------------------------------------------
# The launcher's compile pass is Rust (launcher/src/precompile.rs), linked to the
# loader by name; for Windows its import library is made here from the headers
# of the version the game's build.rs checks for.
if(WIN32)
    include(cmake/vulkan-windows.cmake)
endif()

# ---- launcher --------------------------------------------------------------
# What a player opens (launcher/): it installs the game and starts it. One
# executable with nothing beside it, like the game's: Dear ImGui on SDL's own
# renderer, the fonts as arrays, and XenonRecomp's patcher, which applies the
# title update, compiled in from its checkout. The executable is cargo's
# (launcher/Cargo.toml): its entry and its compile pass are Rust, on the shaders
# crate the game builds its shaders with, and the rest is this library,
# launcher_core, which its build.rs builds here and links with SDL, the XMA
# decoder and zlib, the icon and the system's libraries.
enable_language(C)
FetchContent_Declare(imgui
    GIT_REPOSITORY https://github.com/ocornut/imgui.git
    GIT_TAG v1.91.9b
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(imgui)

# assets/fonts/*.ttf as C arrays.
set(MW2_FONTS_SOURCE "${CMAKE_BINARY_DIR}/launcher_fonts.cpp")
set(fonts_text "// Generated by CMake from assets/fonts/.\n")
foreach(font "Menu=BarlowCondensed-Regular" "Heading=BarlowCondensed-Medium" "Body=BarlowSemiCondensed-Regular")
    string(REPLACE "=" ";" font "${font}")
    list(GET font 0 font_name)
    list(GET font 1 font_file)
    set(font_path "${CMAKE_SOURCE_DIR}/assets/fonts/${font_file}.ttf")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${font_path}")
    file(READ "${font_path}" font_hex HEX)
    string(LENGTH "${font_hex}" font_length)
    math(EXPR font_size "${font_length} / 2")
    string(REGEX REPLACE "([0-9a-f][0-9a-f])" "0x\\1," font_bytes "${font_hex}")
    string(APPEND fonts_text "extern const unsigned char kFont${font_name}[] = { ${font_bytes} };\n"
                             "extern const unsigned kFont${font_name}Size = ${font_size};\n")
endforeach()
# The window's icon the same way (tools/icons.py makes it).
set(icon_path "${CMAKE_SOURCE_DIR}/assets/icons/launcher.bmp")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${icon_path}")
file(READ "${icon_path}" icon_hex HEX)
string(LENGTH "${icon_hex}" icon_length)
math(EXPR icon_size "${icon_length} / 2")
string(REGEX REPLACE "([0-9a-f][0-9a-f])" "0x\\1," icon_bytes "${icon_hex}")
string(APPEND fonts_text "extern const unsigned char kIcon[] = { ${icon_bytes} };\n"
                         "extern const unsigned kIconSize = ${icon_size};\n")
file(GENERATE OUTPUT "${MW2_FONTS_SOURCE}" CONTENT "${fonts_text}")

# The fastfile reader (launcher/zone/): zlib for the zone's stream, and
# the definitions it walks a zone by, embedded as the fonts are.
FetchContent_Declare(zlib
    GIT_REPOSITORY https://github.com/madler/zlib.git
    GIT_TAG v1.3.1
    GIT_SHALLOW TRUE
    EXCLUDE_FROM_ALL)
set(ZLIB_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(zlib)
# The 360's under their own names, the PC game's (defs_pc/, read by the map
# converter, launcher/mapport/) under pc/.
set(MW2_ZONE_DEFS_SOURCE "${CMAKE_BINARY_DIR}/launcher_zone_defs.cpp")
set(zone_text "// Generated by CMake from launcher/zone/defs/ and defs_pc/.\n")
set(zone_table "")
set(zone_index 0)
foreach(zone_set "defs:" "defs_pc:pc/")
    string(REPLACE ":" ";" zone_set "${zone_set}")
    list(GET zone_set 0 zone_folder)
    list(LENGTH zone_set zone_set_length)
    set(zone_prefix "")
    if(zone_set_length GREATER 1)
        list(GET zone_set 1 zone_prefix)
    endif()
    set(zone_defs_dir "${CMAKE_SOURCE_DIR}/launcher/zone/${zone_folder}")
    file(GLOB zone_defs RELATIVE "${zone_defs_dir}" CONFIGURE_DEPENDS
        "${zone_defs_dir}/*.h" "${zone_defs_dir}/*.txt" "${zone_defs_dir}/assets/*.txt")
    foreach(def ${zone_defs})
        file(READ "${zone_defs_dir}/${def}" def_hex HEX)
        string(LENGTH "${def_hex}" def_length)
        math(EXPR def_size "${def_length} / 2")
        string(REGEX REPLACE "([0-9a-f][0-9a-f])" "0x\\1," def_bytes "${def_hex}")
        string(APPEND zone_text "static const unsigned char kDef${zone_index}[] = { ${def_bytes} };\n")
        string(APPEND zone_table "    { \"${zone_prefix}${def}\", kDef${zone_index}, ${def_size} },\n")
        math(EXPR zone_index "${zone_index} + 1")
    endforeach()
endforeach()
string(APPEND zone_text "struct ZoneDefinition { const char* name; const unsigned char* data; unsigned size; };\n"
                        "extern const ZoneDefinition kZoneDefinitions[] = {\n${zone_table}};\n"
                        "extern const unsigned kZoneDefinitionCount = ${zone_index};\n")
file(GENERATE OUTPUT "${MW2_ZONE_DEFS_SOURCE}" CONTENT "${zone_text}")
file(GLOB ZONE_SOURCES CONFIGURE_DEPENDS "${CMAKE_SOURCE_DIR}/launcher/zone/*.cpp")
# The map converter (launcher/mapport/): PC maps made into 360 ones.
file(GLOB MAPPORT_SOURCES CONFIGURE_DEPENDS "${CMAKE_SOURCE_DIR}/launcher/mapport/*.cpp")

set(XENON "${CMAKE_SOURCE_DIR}/XenonRecomp-rs")
file(GLOB LAUNCHER_SOURCES CONFIGURE_DEPENDS "${CMAKE_SOURCE_DIR}/launcher/*.cpp")
add_library(launcher_core STATIC ${LAUNCHER_SOURCES}
    "${MW2_FONTS_SOURCE}"
    ${ZONE_SOURCES} ${MAPPORT_SOURCES} "${MW2_ZONE_DEFS_SOURCE}"
    "${CMAKE_SOURCE_DIR}/runtime/install/crypto.cpp"
    "${CMAKE_SOURCE_DIR}/runtime/install/xex.cpp"
    "${CMAKE_SOURCE_DIR}/runtime/install/spawn.cpp"
    "${imgui_SOURCE_DIR}/imgui.cpp"
    "${imgui_SOURCE_DIR}/imgui_draw.cpp"
    "${imgui_SOURCE_DIR}/imgui_tables.cpp"
    "${imgui_SOURCE_DIR}/imgui_widgets.cpp"
    "${imgui_SOURCE_DIR}/backends/imgui_impl_sdl3.cpp"
    "${imgui_SOURCE_DIR}/backends/imgui_impl_sdlrenderer3.cpp"
    "${XENON}/XenonUtils/xex_patcher.cpp"
    "${XENON}/XenonUtils/memory_mapped_file.cpp"
    "${XENON}/thirdparty/libmspack/libmspack/mspack/lzxd.c"
    "${XENON}/thirdparty/tiny-AES-c/aes.c")
target_include_directories(launcher_core PRIVATE
    "${imgui_SOURCE_DIR}" "${imgui_SOURCE_DIR}/backends"
    "${XENON}/XenonUtils" "${XENON}/thirdparty/simde"
    "${XENON}/thirdparty/libmspack/libmspack/mspack"
    "${XENON}/thirdparty/tiny-AES-c" "${XENON}/thirdparty/TinySHA1")
target_compile_definitions(launcher_core PRIVATE ${MW2_HASH_DEFINITIONS} NOMINMAX)
# What release this is, for the launcher to look for a newer one (see
# MW2_RELEASE_TAG above). A build without one does not update.
if(WIN32)
    set(MW2_RELEASE_KIND "windows")
else()
    set(MW2_RELEASE_KIND "linux")
endif()
set_source_files_properties("${CMAKE_SOURCE_DIR}/launcher/update.cpp" PROPERTIES COMPILE_DEFINITIONS
    "MW2_RELEASE_TAG=\"${MW2_RELEASE_TAG}\";MW2_RELEASE_KIND=\"${MW2_RELEASE_KIND}\"")
target_link_libraries(launcher_core PRIVATE SDL3::SDL3-static zlibstatic)
target_include_directories(launcher_core PRIVATE "${zlib_SOURCE_DIR}" "${zlib_BINARY_DIR}")
if(WIN32)
    target_include_directories(launcher_core PRIVATE "${CMAKE_SOURCE_DIR}/launcher/compat")
    # The import library the launcher's link takes vulkan-1 from.
    add_dependencies(launcher_core vulkan_import_library)
endif()
# What the launcher's build.rs links, as this target's link would take it: the
# archives, the system libraries each asks for (a name, or $<LINK_ONLY:name>) and
# the compiler's standard ones, the Vulkan import library on Windows, and the
# link's options.
set(launcher_link "archive $<TARGET_FILE:launcher_core>
archive $<TARGET_FILE:SDL3-static>
archive $<TARGET_FILE:zlibstatic>
uses $<JOIN:$<TARGET_PROPERTY:SDL3-static,INTERFACE_LINK_LIBRARIES>, >
uses $<JOIN:$<TARGET_PROPERTY:zlibstatic,INTERFACE_LINK_LIBRARIES>, >
standard ${CMAKE_CXX_STANDARD_LIBRARIES}
lto ${MW2_LTO} ${MW2_LTO_CACHE}
")
if(WIN32)
    string(APPEND launcher_link "vulkan ${Vulkan_LIBRARY}\n")
endif()
file(GENERATE OUTPUT "${CMAKE_BINARY_DIR}/launcher_core.txt" CONTENT "${launcher_link}")

# The launcher's fastfile reader on its own: every shader of the files it
# is given, to check the reader against the game's files.
if(NOT WIN32)
    add_executable(zone-shaders "${CMAKE_SOURCE_DIR}/tools/zone_shaders.cpp"
        ${ZONE_SOURCES} "${MW2_ZONE_DEFS_SOURCE}"
        "${CMAKE_SOURCE_DIR}/runtime/install/xex.cpp" "${CMAKE_SOURCE_DIR}/runtime/install/crypto.cpp")
    target_link_libraries(zone-shaders PRIVATE zlibstatic)
    target_include_directories(zone-shaders PRIVATE "${zlib_SOURCE_DIR}" "${zlib_BINARY_DIR}")
endif()

# The map converter on its own, and the checks it is built against.
add_executable(map-port "${CMAKE_SOURCE_DIR}/tools/map_port.cpp"
    ${ZONE_SOURCES} ${MAPPORT_SOURCES} "${MW2_ZONE_DEFS_SOURCE}"
    "${CMAKE_SOURCE_DIR}/runtime/install/xex.cpp" "${CMAKE_SOURCE_DIR}/runtime/install/crypto.cpp")
target_link_libraries(map-port PRIVATE zlibstatic)
target_include_directories(map-port PRIVATE "${zlib_SOURCE_DIR}" "${zlib_BINARY_DIR}")
if(NOT WIN32)
    target_link_libraries(map-port PRIVATE pthread)   # (std::async: stock files are read on worker threads)
endif()
if(MW2_PORTABLE)
    target_link_options(map-port PRIVATE -static-libstdc++ -static-libgcc)
endif()
