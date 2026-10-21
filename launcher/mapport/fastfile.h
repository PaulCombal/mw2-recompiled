#pragma once
// The container a zone comes in.
//
// A PC fastfile (the 2009 game's, version 276, and IW4x's) is a 21-byte
// header, then one zlib stream; IW4x scrambles every byte after inflating.
// A 360 one (launcher/zone/fastfile.h reads it for the shader pass) lists the
// pictures it streams from the console's image paks, then holds the zlib
// stream. A converted map is written as an unsigned 360 file.
#include "tree.h"

#include <array>
#include <filesystem>

namespace mapport
{
    struct Fastfile
    {
        bool pc = false;
        bool iw4x = false;
        std::vector<uint8_t> zone;
        // A 360 file's pak table: (pak, start, end) of each streamed picture's
        // levels, four a picture, in the order the zone has them.
        std::vector<PakEntry> table;
    };

    // Throws Error when the file is neither platform's fastfile.
    Fastfile ReadFastfile(const std::filesystem::path& path);
    bool IsPcFastfile(const std::filesystem::path& path);

    // An unsigned 360 file of `zone` (deflated at zlib level 6) with the pak
    // table given.
    std::vector<uint8_t> XboxFastfile(const std::vector<uint8_t>& zone, const std::vector<PakEntry>& table);
}
