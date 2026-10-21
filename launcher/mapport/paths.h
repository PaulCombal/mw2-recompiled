#pragma once
// Paths as text: the converter's names, logs and messages are UTF-8, and a
// path's narrow string() is the ANSI code page's on Windows.
#include <filesystem>
#include <string>
#include <string_view>

namespace mapport
{
    inline std::string Utf8(const std::filesystem::path& p)
    {
        const std::u8string text = p.u8string();
        return std::string(text.begin(), text.end());
    }

    inline std::filesystem::path PathOf(std::string_view utf8)
    {
        return std::filesystem::path(std::u8string(utf8.begin(), utf8.end()));
    }
}
