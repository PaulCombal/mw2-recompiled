#include "fastfile.h"
#include "defs.h"

#include <algorithm>
#include <cstring>
#include <fstream>
#include <iterator>
#include <zlib.h>

namespace zone
{
    namespace
    {
        // For messages: the name as UTF-8 on every system.
        std::string Name(const std::filesystem::path& path)
        {
            const std::u8string name = path.filename().u8string();
            return std::string(name.begin(), name.end());
        }

        uint32_t BigEndian(const uint8_t* p)
        {
            return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 | uint32_t(p[2]) << 8 | p[3];
        }
    }

    std::vector<uint8_t> ReadFastfile(const std::filesystem::path& path)
    {
        std::ifstream file(path, std::ios::binary);
        if (!file) throw Error("cannot open " + Name(path));
        const std::vector<uint8_t> raw((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        if (raw.size() < 0x1D || (std::memcmp(raw.data(), "IWffu100", 8) && std::memcmp(raw.data(), "IWff0100", 8)))
            throw Error(Name(path) + " is not a 360 fastfile");

        // The picture table: a count, then 12 bytes each; then two sizes.
        const size_t pictures = BigEndian(raw.data() + 0x19);
        const size_t body = 0x1D + pictures * 12 + 8;
        if (body > raw.size()) throw Error(Name(path) + " is cut short");

        std::vector<uint8_t> stream;
        if (raw.size() - body >= 8 && !std::memcmp(raw.data() + body, "IWffs100", 8))
        {
            for (size_t at = body + 0x2000; at < raw.size();)
            {
                at += 0x2000;
                if (at >= raw.size()) break;
                const size_t n = std::min<size_t>(0x200000, raw.size() - at);
                stream.insert(stream.end(), raw.begin() + ptrdiff_t(at), raw.begin() + ptrdiff_t(at + n));
                at += 0x200000;
            }
        }
        else stream.assign(raw.begin() + ptrdiff_t(body), raw.end());

        std::vector<uint8_t> zone(stream.size() * 3);
        z_stream z{};
        if (inflateInit(&z) != Z_OK) throw Error("zlib would not start");
        z.next_in = stream.data();
        z.avail_in = uInt(stream.size());
        int result = Z_OK;
        while (result == Z_OK)
        {
            if (z.total_out == zone.size()) zone.resize(zone.size() * 2);
            z.next_out = zone.data() + z.total_out;
            z.avail_out = uInt(zone.size() - z.total_out);
            result = inflate(&z, Z_NO_FLUSH);
        }
        const size_t made = z.total_out;
        inflateEnd(&z);
        if (result != Z_STREAM_END)
            throw Error(Name(path) + ": its zone does not decompress (zlib " + std::to_string(result) + ")");
        zone.resize(made);
        return zone;
    }
}
