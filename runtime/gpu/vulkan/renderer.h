#pragma once
#include <cstdint>
#include <cstddef>

namespace vk::renderer
{
    // The name a shader goes by: FNV-1a over its microcode dwords, as the
    // command stream loads them. The game's is src/runtime/gpu/hash.rs's shader_hash,
    // the same; the launcher names the shaders it compiles by it.
    inline uint64_t ShaderHash(const uint32_t* code, size_t words)
    {
        uint64_t hash = 1469598103934665603ull;
        for (size_t i = 0; i < words; i++) { hash ^= code[i]; hash *= 1099511628211ull; }
        return hash;
    }
}
