#pragma once

#include <algorithm>
#include <cstdint>
#include <cstring>

namespace kernel::xex
{
    inline uint16_t ReadPeLe16(const uint8_t* p)
    {
        return uint16_t(p[0]) | (uint16_t(p[1]) << 8);
    }

    inline uint32_t ReadPeLe32(const uint8_t* p)
    {
        return uint32_t(p[0]) | (uint32_t(p[1]) << 8) |
               (uint32_t(p[2]) << 16) | (uint32_t(p[3]) << 24);
    }

    // Find a data-import slot in a loaded PE image. Only non-executable
    // sections are searched, and callers must require exactly one match.
    // Xboxkrnl data imports are represented by 0x00010000 | ordinal.
    inline uint32_t FindUniqueDataImportRecord(const uint8_t* image,
                                               uint32_t imageSize,
                                               uint32_t imageBase,
                                               uint16_t ordinal,
                                               unsigned& matches)
    {
        matches = 0;
        if (!image || imageSize < 0x40 || std::memcmp(image, "MZ", 2) != 0)
            return 0;

        const uint32_t pe = ReadPeLe32(image + 0x3C);
        if (uint64_t(pe) + 24 > imageSize ||
            std::memcmp(image + pe, "PE\0\0", 4) != 0)
            return 0;

        const uint16_t sectionCount = ReadPeLe16(image + pe + 6);
        const uint16_t optionalSize = ReadPeLe16(image + pe + 20);
        const uint64_t sectionTable = uint64_t(pe) + 24 + optionalSize;
        if (sectionTable + uint64_t(sectionCount) * 40 > imageSize)
            return 0;

        const uint32_t marker = 0x00010000u | ordinal;
        uint32_t found = 0;
        for (uint32_t i = 0; i < sectionCount; ++i)
        {
            const uint8_t* section = image + sectionTable + uint64_t(i) * 40;
            const uint32_t virtualSize = ReadPeLe32(section + 8);
            const uint32_t virtualAddress = ReadPeLe32(section + 12);
            const uint32_t rawSize = ReadPeLe32(section + 16);
            const uint32_t characteristics = ReadPeLe32(section + 36);
            if (characteristics & 0x20000000u) continue; // IMAGE_SCN_MEM_EXECUTE

            const uint32_t span = std::max(virtualSize, rawSize);
            if (!span || virtualAddress >= imageSize ||
                uint64_t(virtualAddress) + span > imageSize)
                continue;

            const uint32_t begin = (virtualAddress + 3u) & ~3u;
            const uint32_t end = virtualAddress + span;
            for (uint32_t at = begin; uint64_t(at) + 4 <= end; at += 4)
            {
                const uint32_t value = uint32_t(image[at]) << 24 |
                                       uint32_t(image[at + 1]) << 16 |
                                       uint32_t(image[at + 2]) << 8 |
                                       uint32_t(image[at + 3]);
                if (value != marker) continue;
                ++matches;
                if (uint64_t(imageBase) + at > 0xFFFFFFFFull) continue;
                found = imageBase + at;
            }
        }
        return matches == 1 ? found : 0;
    }
}
