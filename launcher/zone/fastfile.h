#pragma once
// A 360 fastfile's zone: the file is a short header, a table of the pictures
// it streams from the image paks, then one zlib stream. A signed file
// (IWff0100) interleaves that stream with hash blocks: 8 KB after its own
// 8 KB header, then 8 KB before each 2 MB of the stream.
#include <cstdint>
#include <filesystem>
#include <vector>

namespace zone
{
    // Throws zone::Error when the file is not a 360 fastfile or its stream is cut short.
    std::vector<uint8_t> ReadFastfile(const std::filesystem::path& path);
}
