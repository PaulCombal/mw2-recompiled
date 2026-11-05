// How far the C++ ReadTexture reads from each of its two pointers: texture.cpp compiled
// again in a namespace of its own, with every copy it makes recorded instead of done, so
// the rig can place a range against its guard page by what the reader really touches.
// texture.h comes in through texture.cpp, under the renamed namespace.
#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <vector>

namespace reach
{
    const uint8_t* base;
    const uint8_t* mips;
    uint64_t baseEnd, mipsEnd;
    constexpr uint64_t kSpan = uint64_t(1) << 40;

    void* Copy(void* to, const void* from, size_t size)
    {
        const auto* p = static_cast<const uint8_t*>(from);
        if (base && p >= base && p < base + kSpan) baseEnd = std::max<uint64_t>(baseEnd, p - base + size);
        else if (mips && p >= mips && p < mips + kSpan) mipsEnd = std::max<uint64_t>(mipsEnd, p - mips + size);
        else std::memmove(to, from, size); // the reader's own buffer (the endian swap)
        return to;
    }
}

namespace std { inline void* reach_copy(void* to, const void* from, size_t size) { return reach::Copy(to, from, size); } }

#define gpu gpu_reach
#define memcpy reach_copy
#include "../../runtime/gpu/texture.cpp"
#undef memcpy
#undef gpu

// Nothing is read: the pointers are far apart and never dereferenced.
extern "C" void gpu_reach_measure(const uint32_t* fetch, bool withBase, bool withMips, uint64_t* baseEnd,
                                  uint64_t* mipsEnd)
{
    gpu_reach::TextureFetch f;
    std::memcpy(f.d, fetch, sizeof f.d);
    reach::base = withBase ? reinterpret_cast<const uint8_t*>(uintptr_t(1) << 44) : nullptr;
    reach::mips = withMips ? reinterpret_cast<const uint8_t*>(uintptr_t(2) << 44) : nullptr;
    reach::baseEnd = reach::mipsEnd = 0;
    gpu_reach::ReadTexture(f, reach::base, reach::mips);
    *baseEnd = reach::baseEnd;
    *mipsEnd = reach::mipsEnd;
}
