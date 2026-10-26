#pragma once
// Whether every shader of the game is compiled for this computer.
//
// The launcher's compile pass (launcher/precompile.cpp) builds every shader
// in the game's files into the pipeline cache, then writes this marker: what
// the cache was made for. The cache holds good while all of it still holds --
// the same shader code (this build's), the same device, driver and cache
// format, the same resolution scale (it is in the translated code), and the
// same files of the game. Anything else and the driver would compile shaders
// as the game meets them, which is the stutter the pass is there to remove,
// so the launcher runs the pass again before the game starts, and a game
// started without the launcher starts it.
#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>

namespace vk::shaders
{
    // Beside the cache, shaders/pipelines.bin (shader_build.h).
    inline constexpr const char* kMarkerFile = "shaders/compiled.txt";

    // The files the pass reads its shaders from: the fastfiles in `game` and
    // the two executables, which carry a few of their own. The fastfiles are
    // the game's own, from the disc and the update, unless `customMaps`
    // (MW2_CUSTOM_MAPS): then every one, a custom map's included. Without it
    // the multiplayer loads no other, and the pass does not read one either.
    std::vector<std::filesystem::path> GameFiles(const std::filesystem::path& game, bool customMaps);

    // What a cache made now would be good for: a line a fact, as the marker
    // keeps them. `physical` is the VkPhysicalDevice, `scaled` whether either
    // scale is past 1 as the device draws them (shader_build.h), which is
    // what the shaders depend on, `customMaps` as GameFiles.
    std::string Fingerprint(void* physical, bool scaled, const std::filesystem::path& game, bool customMaps);

    struct Marker
    {
        bool found = false;
        std::string fingerprint;
        std::string device;       // the device-id line's DeviceId: which device it was made on
        uint32_t shaders = 0;     // how many the pass compiled: none on a device without pipeline libraries
    };
    Marker ReadMarker();

    // Whether the cache is there and the marker says it was made for exactly
    // `fingerprint`; if not, in `why`, what changed, in a player's words.
    bool Compiled(const std::string& fingerprint, std::string* why = nullptr);

    // The pass's last step, once the cache is in place. RemoveMarker is its
    // first: a pass that does not finish leaves no marker behind it.
    bool WriteMarker(const std::string& fingerprint, uint32_t shaders, std::string& error);
    void RemoveMarker();

    // The lines of two fingerprints that say what the code was compiled for
    // and on -- all but the game's files -- are the same: a cache made for
    // one holds shaders the other can use.
    bool SameTarget(const std::string& a, const std::string& b);
}
