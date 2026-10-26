#pragma once
// Which release this build is, and which other copies it can play with.
#include <cstdint>

namespace version
{
    // The release tag the build is published as, "v0.8.2", or "" for a build
    // that is no release (CMake's MW2_RELEASE_TAG).
    const char* Release();
    // What copies of the same release share and copies of another do not:
    // 1 to 32767, from the tag, or from "dev" for every build without one.
    // Two releases may share it, by a chance of one in 32767.
    uint16_t Network();
}
