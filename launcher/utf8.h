#pragma once
// Paths as UTF-8, which SDL, the log and the Rust half of the launcher take,
// whatever the system's code page: on Linux a path's own bytes, on Windows its
// whole name.
#include <filesystem>
#include <string>

namespace install
{
    inline std::string Utf8(const std::filesystem::path& path)
    {
        const auto text = path.u8string();
        return std::string(text.begin(), text.end());
    }
    inline std::filesystem::path FromUtf8(const std::string& text)
    {
        return std::filesystem::path(std::u8string(text.begin(), text.end()));
    }
}
