#pragma once
// Which release this build is, and which other copies it can play with.
#include <cstdint>

// src/runtime/version.rs in a Cargo build, version.cpp in a C++ one.
extern "C"
{
    const char* mw2_version_release();
    uint16_t mw2_version_network();
}

namespace version
{
    // The release tag the build is published as, "v0.8.2", or "" for a build
    // that is no release (CMake's MW2_RELEASE_TAG).
    inline const char* Release() { return mw2_version_release(); }
    // What copies of the same release share and copies of another do not:
    // 1 to 32767, from the tag, or from "dev" for every build without one.
    // Two releases may share it, by a chance of one in 32767.
    inline uint16_t Network() { return mw2_version_network(); }
}
