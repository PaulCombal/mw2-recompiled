#pragma once
// A zip archive (a PC map's .iwd): its entries' names, in the order the
// archive lists them, and an entry's bytes (stored or deflated).
#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>

namespace mapport
{
    class Zip
    {
    public:
        // Throws Error when the file is not a zip archive.
        explicit Zip(std::filesystem::path path);

        const std::filesystem::path& Path() const { return path; }
        const std::vector<std::string>& Names() const { return names; }
        // Throws Error for an entry it doesn't have or can't read.
        std::vector<uint8_t> Read(const std::string& name) const;
        std::vector<uint8_t> Read(size_t index) const;

    private:
        struct Item
        {
            uint32_t method, packed, size;
            uint64_t local;
        };
        std::filesystem::path path;
        std::vector<std::string> names;
        std::vector<Item> items;
    };
}
