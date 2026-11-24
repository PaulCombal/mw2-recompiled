#pragma once
// An XEX2 as the disc carries it, or as a title update leaves it, turned into
// the memory image the recompiled code was made from: decrypted with the
// retail key and its basic compression undone, where it has either -- what
// cargo xtask writes as default.pe, made at every launch instead of kept
// on disk.
#include <cstdint>
#include <string>
#include <vector>

namespace install::xex
{
    // False, with `error` saying why, for anything but encryption none/normal
    // and compression none/basic -- all the disc and a patched executable use.
    bool Image(const std::vector<uint8_t>& file, std::vector<uint8_t>& image, std::string& error);
}
