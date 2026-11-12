#pragma once

#include "../guest.h"

#include <cstdint>

namespace kernel::xex
{
    // XEX2 header layout, in guest big-endian memory. Optional-header keys
    // encode the value representation in their low byte:
    //   0: value is inline
    //   1: value is a pointer to the value word in the header
    //   other: value is an offset from the beginning of the XEX header
    //
    // Returns a guest virtual address/value exactly as the Xbox routine does.
    // Zero means the key is absent or the supplied header is malformed.
    inline uint32_t ImageHeaderField(const uint8_t* guestBase,
                                     uint32_t headerAddress,
                                     uint32_t key)
    {
        constexpr uint32_t kHeaderFixedSize = 0x18;
        constexpr uint32_t kOptionalHeaderSize = 8;
        constexpr uint32_t kXex2Magic = 0x58455832; // "XEX2"

        if (!guestBase || !headerAddress) return 0;
        if (uint64_t(headerAddress) + kHeaderFixedSize > 0x100000000ull) return 0;

        const auto* header = reinterpret_cast<const be32*>(guestBase + headerAddress);
        if (uint32_t(header[0]) != kXex2Magic) return 0;

        const uint32_t headerSize = header[2];
        const uint32_t headerCount = header[5];
        if (headerSize < kHeaderFixedSize ||
            uint64_t(headerAddress) + headerSize > 0x100000000ull)
            return 0;

        const uint32_t available = (headerSize - kHeaderFixedSize) / kOptionalHeaderSize;
        if (headerCount > available) return 0;

        const auto* optional = reinterpret_cast<const be32*>(
            guestBase + headerAddress + kHeaderFixedSize);
        for (uint32_t i = 0; i < headerCount; ++i)
        {
            const uint32_t fieldKey = optional[i * 2];
            if (fieldKey != key) continue;

            const uint32_t value = optional[i * 2 + 1];
            switch (key & 0xFF)
            {
            case 0x00:
                return value;
            case 0x01:
                // Return the guest address of the value word itself.
                return headerAddress + kHeaderFixedSize + i * kOptionalHeaderSize + 4;
            default:
                // Structured data is stored at an offset from the XEX header.
                if (value < kHeaderFixedSize ||
                    uint64_t(value) + sizeof(be32) > headerSize)
                    return 0;
                return headerAddress + value;
            }
        }
        return 0;
    }
}
