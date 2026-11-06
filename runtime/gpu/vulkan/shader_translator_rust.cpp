// shader::Translate and shader::Describe in the Rust renderer's build: the
// translator is src/runtime/gpu/shader, and these forward to its C interface
// (ffi.rs), filling the C++ Translation the callers know. shader_translator.cpp
// is the C++ build's translator, and stays for the launcher, which compiles
// it; the two give the same words for every program (tools/shader_diff).
#if MW2_RUST_RENDERER

#include "shader_translator.h"

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

namespace
{
    // TranslationMeta in translator.rs, field for field, #[repr(C)].
    struct Meta
    {
        uint32_t interpolatorMask;
        uint32_t textureMask;
        uint8_t textureKinds[32];
        uint32_t vertexFetchMask;
        uint32_t vertexFetchSubs[3];
        uint32_t vertexFetchPairs[3];
        uint32_t vertexFetchStride[96];
        uint32_t vertexFetchTail[96];
        uint32_t fetchPairsRead[3];
        uint32_t constantsRead;
        bool addressedConstants;
        uint32_t colourMask;
        bool writesDepth;
        bool writesMemory;
        uint32_t instructionCount;
        uint32_t droppedExports;
        uint32_t droppedMemoryExports;
    };

    // The layout translator.rs asserts for TranslationMeta: a field moved on
    // one side only stops the build instead of reading another field's bytes.
    static_assert(sizeof(bool) == 1, "Rust's bool is one byte");
    static_assert(sizeof(Meta) == 876 && alignof(Meta) == 4);
    static_assert(offsetof(Meta, interpolatorMask) == 0);
    static_assert(offsetof(Meta, textureMask) == 4);
    static_assert(offsetof(Meta, textureKinds) == 8);
    static_assert(offsetof(Meta, vertexFetchMask) == 40);
    static_assert(offsetof(Meta, vertexFetchSubs) == 44);
    static_assert(offsetof(Meta, vertexFetchPairs) == 56);
    static_assert(offsetof(Meta, vertexFetchStride) == 68);
    static_assert(offsetof(Meta, vertexFetchTail) == 452);
    static_assert(offsetof(Meta, fetchPairsRead) == 836);
    static_assert(offsetof(Meta, constantsRead) == 848);
    static_assert(offsetof(Meta, addressedConstants) == 852);
    static_assert(offsetof(Meta, colourMask) == 856);
    static_assert(offsetof(Meta, writesDepth) == 860);
    static_assert(offsetof(Meta, writesMemory) == 861);
    static_assert(offsetof(Meta, instructionCount) == 864);
    static_assert(offsetof(Meta, droppedExports) == 868);
    static_assert(offsetof(Meta, droppedMemoryExports) == 872);

    using WordSink = void (*)(void* context, const uint32_t* words, size_t count);
    using TextSink = void (*)(void* context, const char* text, size_t length);

    // noexcept: an allocation failure ends the program here rather than unwinding
    // through the Rust frames that call these.
    void TakeWords(void* context, const uint32_t* words, size_t count) noexcept
    {
        static_cast<std::vector<uint32_t>*>(context)->assign(words, words + count);
    }

    void TakeText(void* context, const char* text, size_t length) noexcept
    {
        static_cast<std::string*>(context)->assign(text, length);
    }

    void ToMeta(const shader::Translation& from, Meta& to)
    {
        to.interpolatorMask = from.interpolatorMask;
        to.textureMask = from.textureMask;
        std::memcpy(to.textureKinds, from.textureKinds, sizeof to.textureKinds);
        to.vertexFetchMask = from.vertexFetchMask;
        std::memcpy(to.vertexFetchSubs, from.vertexFetchSubs, sizeof to.vertexFetchSubs);
        std::memcpy(to.vertexFetchPairs, from.vertexFetchPairs, sizeof to.vertexFetchPairs);
        std::memcpy(to.vertexFetchStride, from.vertexFetchStride, sizeof to.vertexFetchStride);
        std::memcpy(to.vertexFetchTail, from.vertexFetchTail, sizeof to.vertexFetchTail);
        std::memcpy(to.fetchPairsRead, from.fetchPairsRead, sizeof to.fetchPairsRead);
        to.constantsRead = from.constantsRead;
        to.addressedConstants = from.addressedConstants;
        to.colourMask = from.colourMask;
        to.writesDepth = from.writesDepth;
        to.writesMemory = from.writesMemory;
        to.instructionCount = from.instructionCount;
        to.droppedExports = from.droppedExports;
        to.droppedMemoryExports = from.droppedMemoryExports;
    }

    void FromMeta(const Meta& from, shader::Translation& to)
    {
        to.interpolatorMask = from.interpolatorMask;
        to.textureMask = from.textureMask;
        std::memcpy(to.textureKinds, from.textureKinds, sizeof to.textureKinds);
        to.vertexFetchMask = from.vertexFetchMask;
        std::memcpy(to.vertexFetchSubs, from.vertexFetchSubs, sizeof to.vertexFetchSubs);
        std::memcpy(to.vertexFetchPairs, from.vertexFetchPairs, sizeof to.vertexFetchPairs);
        std::memcpy(to.vertexFetchStride, from.vertexFetchStride, sizeof to.vertexFetchStride);
        std::memcpy(to.vertexFetchTail, from.vertexFetchTail, sizeof to.vertexFetchTail);
        std::memcpy(to.fetchPairsRead, from.fetchPairsRead, sizeof to.fetchPairsRead);
        to.constantsRead = from.constantsRead;
        to.addressedConstants = from.addressedConstants;
        to.colourMask = from.colourMask;
        to.writesDepth = from.writesDepth;
        to.writesMemory = from.writesMemory;
        to.instructionCount = from.instructionCount;
        to.droppedExports = from.droppedExports;
        to.droppedMemoryExports = from.droppedMemoryExports;
    }
}

extern "C"
{
    bool mw2_shader_translate(bool pixel, const uint32_t* words, size_t count, bool scaled, Meta* meta,
                              void* spirvContext, WordSink spirvSink, void* errorContext, TextSink errorSink);
    void mw2_shader_describe(bool ok, const char* error, size_t errorLength, size_t spirvWords,
                             const Meta* meta, void* textContext, TextSink textSink);
}

shader::Translation shader::Translate(Type type, const uint32_t* words, size_t count, bool scaled)
{
    Translation result;
    Meta meta{};
    result.ok = mw2_shader_translate(type == Type::Pixel, words, count, scaled, &meta,
                                     &result.spirv, TakeWords, &result.error, TakeText);
    FromMeta(meta, result);
    return result;
}

std::string shader::Describe(const Translation& result)
{
    Meta meta{};
    ToMeta(result, meta);
    std::string text;
    mw2_shader_describe(result.ok, result.error.data(), result.error.size(), result.spirv.size(), &meta,
                        &text, TakeText);
    return text;
}

#endif
