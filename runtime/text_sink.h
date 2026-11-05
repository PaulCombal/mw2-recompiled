#pragma once
// How text leaves a function the C++ calls in another language: it is handed to
// this, which copies it, so that neither side frees what the other made.
#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

using mw2_text_sink = void (*)(void* to, const char* text, size_t size);

inline void mw2_assign_text(void* to, const char* text, size_t size)
{
    static_cast<std::string*>(to)->assign(text, size);
}

inline void mw2_assign_bytes(void* to, const char* bytes, size_t size)
{
    const auto* first = reinterpret_cast<const uint8_t*>(bytes);
    static_cast<std::vector<uint8_t>*>(to)->assign(first, first + size);
}
