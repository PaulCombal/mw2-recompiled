#include "runtime/kernel/xex_header.h"
#include "runtime/kernel/xex_module_handle.h"
#include "runtime/install/xex.h"

#ifdef NDEBUG
#undef NDEBUG
#endif

#include <array>
#include <cassert>
#include <cstdint>
#include <string>
#include <vector>

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

    // Build a minimal loaded PE image with one xboxkrnl data-import marker.
    std::array<uint8_t, 0x1000> pe{};
    auto putLe16 = [&](size_t at, uint16_t value) {
        pe[at] = uint8_t(value);
        pe[at + 1] = uint8_t(value >> 8);
    };
    auto putLe32 = [&](size_t at, uint32_t value) {
        for (unsigned i = 0; i < 4; ++i) pe[at + i] = uint8_t(value >> (i * 8));
    };
    auto putBe32 = [&](size_t at, uint32_t value) {
        pe[at] = uint8_t(value >> 24);
        pe[at + 1] = uint8_t(value >> 16);
        pe[at + 2] = uint8_t(value >> 8);
        pe[at + 3] = uint8_t(value);
    };

    pe[0] = 'M'; pe[1] = 'Z';
    putLe32(0x3C, 0x80);
    pe[0x80] = 'P'; pe[0x81] = 'E';
    putLe16(0x86, 2);       // section count
    putLe16(0x94, 0xE0);    // optional-header size
    constexpr size_t sectionTable = 0x80 + 24 + 0xE0;

    // .text: executable, must not be searched for data imports.
    putLe32(sectionTable + 8, 0x100);
    putLe32(sectionTable + 12, 0x400);
    putLe32(sectionTable + 16, 0x100);
    putLe32(sectionTable + 36, 0x60000020);
    putBe32(0x404, 0x00010193);

    // .rdata: non-executable; the single ordinal 0x193 marker is here.
    putLe32(sectionTable + 40 + 8, 0x100);
    putLe32(sectionTable + 40 + 12, 0x600);
    putLe32(sectionTable + 40 + 16, 0x100);
    putLe32(sectionTable + 40 + 36, 0x40000040);
    putBe32(0x604, 0x00010193);

    unsigned matches = 0;
    assert(kernel::xex::FindUniqueDataImportRecord(
               pe.data(), uint32_t(pe.size()), 0x82000000, 0x0193, matches)
           == 0x82000604);
    assert(matches == 1);
    // A unique marker whose absolute guest address would wrap is rejected.
    assert(kernel::xex::FindUniqueDataImportRecord(
               pe.data(), uint32_t(pe.size()), 0xFFFFFA00, 0x0193, matches) == 0);
    assert(matches == 1);

    // Ambiguous matches fail closed; executable-section lookalikes are ignored.
    putBe32(0x608, 0x00010193);
    assert(kernel::xex::FindUniqueDataImportRecord(
               pe.data(), uint32_t(pe.size()), 0x82000000, 0x0193, matches) == 0);
    assert(matches == 2);
    putBe32(0x608, 0);
    pe[0] = 0;
    assert(kernel::xex::FindUniqueDataImportRecord(
               pe.data(), uint32_t(pe.size()), 0x82000000, 0x0193, matches) == 0);
    assert(matches == 0);

    return 0;
}
