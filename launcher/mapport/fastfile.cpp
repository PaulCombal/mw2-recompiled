#include "fastfile.h"
#include "paths.h"

#include <zlib.h>

#include <cstring>
#include <fstream>
#include <iterator>

namespace mapport
{
    namespace
    {
        constexpr uint8_t kPcVersion[4] = { 0x14, 0x01, 0x00, 0x00 };   // 276, little-endian
        constexpr size_t kPcHeader = 21;   // magic, version, a flag byte, a timestamp
        constexpr size_t kLargestZone = 1u << 30;   // (the stock files' largest zone is a few hundred MB)

        std::vector<uint8_t> ReadFile(const std::filesystem::path& path)
        {
            std::ifstream in(path, std::ios::binary);
            if (!in) throw Error("cannot open " + Utf8(path));
            return std::vector<uint8_t>(std::istreambuf_iterator<char>(in), {});
        }

        bool Starts(const std::vector<uint8_t>& b, const char* magic, size_t n = 8)
        {
            return b.size() >= n && std::memcmp(b.data(), magic, n) == 0;
        }

        // A signed file's stream: past an 8 KB signature header, an 8 KB block of
        // hashes before each 2 MB of it.
        std::vector<uint8_t> Unsigned(const uint8_t* body, size_t n)
        {
            if (n < 8 || std::memcmp(body, "IWffs100", 8) != 0) return std::vector<uint8_t>(body, body + n);
            std::vector<uint8_t> stream;
            for (size_t at = 0x2000; at < n;)
            {
                at += 0x2000;
                if (at >= n) break;
                const size_t take = std::min<size_t>(0x200000, n - at);
                stream.insert(stream.end(), body + at, body + at + take);
                at += 0x200000;
            }
            return stream;
        }

        std::vector<uint8_t> Inflate(const std::vector<uint8_t>& stream, const std::string& name)
        {
            std::vector<uint8_t> out(std::max<size_t>(stream.size() * 3, 1 << 16));
            z_stream z{};
            if (inflateInit(&z) != Z_OK) throw Error("zlib would not start");
            z.next_in = const_cast<uint8_t*>(stream.data());
            z.avail_in = uInt(stream.size());
            int result = Z_OK;
            while (result == Z_OK)
            {
                if (z.total_out == out.size())
                {
                    if (out.size() >= kLargestZone)
                    {
                        inflateEnd(&z);
                        throw Error(name + ": its zone inflates past " + std::to_string(kLargestZone >> 20) + " MB");
                    }
                    out.resize(std::min(out.size() * 2, kLargestZone));
                }
                z.next_out = out.data() + z.total_out;
                z.avail_out = uInt(out.size() - z.total_out);
                result = inflate(&z, Z_NO_FLUSH);
            }
            const size_t made = z.total_out;
            inflateEnd(&z);
            if (result != Z_STREAM_END) throw Error(name + ": its zone does not decompress (zlib " + std::to_string(result) + ")");
            out.resize(made);
            return out;
        }

        // IW4x scrambles every byte after inflating (FastFiles::ReadXFileStub).
        void Unscramble(std::vector<uint8_t>& data)
        {
            uint8_t last = 0;
            for (uint8_t& b : data)
            {
                uint8_t v = uint8_t(b ^ last);
                v = uint8_t((v << 4) | (v >> 4));
                v ^= 0xFF;
                v = uint8_t((v >> 6) | (v << 2));
                b = last = v;
            }
        }
    }

    bool IsPcFastfile(const std::filesystem::path& path)
    {
        std::ifstream in(path, std::ios::binary);
        uint8_t head[12] = {};
        in.read(reinterpret_cast<char*>(head), 12);
        return in.gcount() == 12 && std::memcmp(head + 8, kPcVersion, 4) == 0;
    }

    Fastfile ReadFastfile(const std::filesystem::path& path)
    {
        const std::vector<uint8_t> raw = ReadFile(path);
        const std::string name = Utf8(path.filename());
        Fastfile ff;
        if (raw.size() >= 12 && std::memcmp(raw.data() + 8, kPcVersion, 4) == 0)
        {
            ff.pc = true;
            ff.iw4x = Starts(raw, "IW4x\x03\0\0\0");
            if (!ff.iw4x && !Starts(raw, "IWffu100") && !Starts(raw, "IWff0100"))
                throw Error(name + " is not a PC fastfile of the game");
            if (raw.size() < kPcHeader) throw Error(name + " is cut short");
            ff.zone = Inflate(Unsigned(raw.data() + kPcHeader, raw.size() - kPcHeader), name);
            if (ff.iw4x)
            {
                // And reads one pad byte before the zone's header (FastFiles::ReadXFileHeader).
                Unscramble(ff.zone);
                if (!ff.zone.empty()) ff.zone.erase(ff.zone.begin());
            }
            return ff;
        }
        if (!Starts(raw, "IWffu100") && !Starts(raw, "IWff0100")) throw Error(name + " is not a fastfile of the game");
        if (raw.size() < 0x1D) throw Error(name + " is cut short");
        const uint32_t count = uint32_t(Load(raw.data() + 0x19, 4, true));
        const size_t body = 0x1D + size_t(count) * 12 + 8;
        if (body > raw.size()) throw Error(name + " is cut short");
        for (uint32_t k = 0; k < count; k++)
        {
            const uint8_t* e = raw.data() + 0x1D + 12 * k;
            ff.table.push_back({ uint32_t(Load(e, 4, true)), uint32_t(Load(e + 4, 4, true)), uint32_t(Load(e + 8, 4, true)) });
        }
        ff.zone = Inflate(Unsigned(raw.data() + body, raw.size() - body), name);
        return ff;
    }

    std::vector<uint8_t> XboxFastfile(const std::vector<uint8_t>& zone, const std::vector<PakEntry>& table)
    {
        // A stock file's header: magic, version 0x10D, flags, build time; then
        // the pak table and the file's size twice, the second counting the
        // pictures read from imagefile1.pak while it loads.
        static const uint8_t kHead[25] = { 0x49, 0x57, 0x66, 0x66, 0x75, 0x31, 0x30, 0x30, 0x00, 0x00, 0x01, 0x0d, 0x01,
                                           0x01, 0xca, 0x3e, 0xc0, 0x38, 0xc2, 0xe4, 0xa0, 0x00, 0x00, 0x00, 0x01 };
        uLongf bound = compressBound(uLong(zone.size()));
        std::vector<uint8_t> stream(bound);
        if (compress2(stream.data(), &bound, zone.data(), uLong(zone.size()), 6) != Z_OK) throw Error("zlib would not compress");
        stream.resize(bound);
        std::vector<uint8_t> out(kHead, kHead + sizeof kHead);
        auto put = [&](uint32_t v) {
            uint8_t b[4];
            Store(b, 4, v, true);
            out.insert(out.end(), b, b + 4);
        };
        put(uint32_t(table.size()));
        uint32_t extra = 0;
        for (const PakEntry& e : table)
        {
            put(e.pak);
            put(e.start);
            put(e.end);
            if (e.pak == 1) extra += e.end - e.start;
        }
        const uint32_t total = uint32_t(out.size() + 8 + stream.size());
        put(total);
        put(total + extra);
        out.insert(out.end(), stream.begin(), stream.end());
        return out;
    }
}
