#include "runtime/kernel/xex_header.h"

#ifdef NDEBUG
#undef NDEBUG
#endif

#include <array>
#include <cassert>
#include <cstdint>

int main()
{
    alignas(16) std::array<uint8_t, 512> memory{};
    constexpr uint32_t headerAddress = 0x40;
    auto* h = reinterpret_cast<be32*>(memory.data() + headerAddress);

    h[0] = 0x58455832; // XEX2
    h[2] = 0x80;       // header size
    h[5] = 3;          // optional-header count

    auto* fields = h + 6;
    fields[0] = 0x00010100; fields[1] = 0x82459E58; // inline entry point
    fields[2] = 0x00010201; fields[3] = 0x00000070; // pointer to value word
    fields[4] = 0x000003FF; fields[5] = 0x00000060; // offset to structured data

    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00010100)
           == 0x82459E58);
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00010201)
           == headerAddress + 0x18 + 8 + 4);
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x000003FF)
           == headerAddress + 0x60);
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00040404) == 0);

    // Reject a truncated optional-header table and an offset outside the header.
    h[5] = 100;
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00010100) == 0);
    h[5] = 3;
    fields[5] = 0x7F;
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x000003FF) == 0);

    // Reject a non-XEX header and a header whose declared size is too small.
    h[0] = 0;
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00010100) == 0);
    h[0] = 0x58455832;
    h[2] = 0x17;
    assert(kernel::xex::ImageHeaderField(memory.data(), headerAddress, 0x00010100) == 0);

    assert(kernel::xex::ImageHeaderField(memory.data(), 0, 0x00010100) == 0);
    return 0;
}
