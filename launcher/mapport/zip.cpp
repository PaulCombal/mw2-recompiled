#include "zip.h"
#include "paths.h"
#include "tree.h"

#include <zlib.h>

#include <algorithm>
#include <fstream>

namespace mapport
{
    namespace
    {
        uint32_t U16(const uint8_t* p) { return uint32_t(p[0]) | uint32_t(p[1]) << 8; }
        uint32_t U32(const uint8_t* p) { return U16(p) | U16(p + 2) << 16; }

        // An entry inflated takes at most this much, and no more than deflate
        // can make of its packed bytes (1,032 to 1): a broken header can't
        // ask for gigabytes.
        constexpr uint64_t kLargestEntry = 512u << 20;

        // Up to `n` bytes at `at`, as many as the file has there.
        std::vector<uint8_t> ReadAt(std::ifstream& in, uint64_t at, size_t n)
        {
            in.seekg(0, std::ios::end);
            const uint64_t size = uint64_t(in.tellg());
            std::vector<uint8_t> b(at < size ? size_t(std::min<uint64_t>(n, size - at)) : 0);
            in.seekg(std::streamoff(at));
            in.read(reinterpret_cast<char*>(b.data()), std::streamsize(b.size()));
            b.resize(size_t(in.gcount()));
            return b;
        }
    }

    Zip::Zip(std::filesystem::path file) : path(std::move(file))
    {
        std::ifstream in(path, std::ios::binary);
        if (!in) throw Error("cannot open " + Utf8(path));
        in.seekg(0, std::ios::end);
        const uint64_t size = uint64_t(in.tellg());
        // The end of central directory record, within the last 64 KB (its comment).
        const uint64_t tail = std::min<uint64_t>(size, 0x10000 + 22);
        const std::vector<uint8_t> end = ReadAt(in, size - tail, size_t(tail));
        size_t at = end.size() >= 22 ? end.size() - 22 : 0;
        while (at > 0 && U32(&end[at]) != 0x06054b50) at--;
        if (end.size() < 22 || U32(&end[at]) != 0x06054b50) throw Error(Utf8(path.filename()) + " is not a zip archive");
        const uint32_t count = U16(&end[at + 10]);
        const std::vector<uint8_t> dir = ReadAt(in, U32(&end[at + 16]), U32(&end[at + 12]));
        size_t p = 0;
        for (uint32_t k = 0; k < count; k++)
        {
            if (p + 46 > dir.size() || U32(&dir[p]) != 0x02014b50) throw Error(Utf8(path.filename()) + ": a broken directory");
            const uint32_t nameLength = U16(&dir[p + 28]), extra = U16(&dir[p + 30]), comment = U16(&dir[p + 32]);
            if (p + 46 + nameLength > dir.size()) throw Error(Utf8(path.filename()) + ": a broken directory");
            names.emplace_back(reinterpret_cast<const char*>(&dir[p + 46]), nameLength);
            items.push_back({ U16(&dir[p + 10]), U32(&dir[p + 20]), U32(&dir[p + 24]), U32(&dir[p + 42]) });
            p += 46 + nameLength + extra + comment;
        }
    }

    std::vector<uint8_t> Zip::Read(const std::string& name) const
    {
        for (size_t k = 0; k < names.size(); k++)
            if (names[k] == name) return Read(k);
        throw Error(Utf8(path.filename()) + " has no " + name);
    }

    std::vector<uint8_t> Zip::Read(size_t index) const
    {
        const Item& it = items.at(index);
        std::ifstream in(path, std::ios::binary);
        const std::vector<uint8_t> local = ReadAt(in, it.local, 30);
        if (local.size() < 30 || U32(local.data()) != 0x04034b50) throw Error(names[index] + ": a broken entry");
        const std::vector<uint8_t> data = ReadAt(in, it.local + 30 + U16(&local[26]) + U16(&local[28]), it.packed);
        if (data.size() != it.packed) throw Error(names[index] + " is cut short");
        if (it.method == 0) return data;
        if (it.method != 8) throw Error(names[index] + " is compressed in a way that can't be read");
        if (it.size > kLargestEntry || it.size > uint64_t(it.packed) * 1032 + 64) throw Error(names[index] + ": a broken entry");
        std::vector<uint8_t> out(it.size);
        z_stream z{};
        if (inflateInit2(&z, -MAX_WBITS) != Z_OK) throw Error("zlib would not start");
        z.next_in = const_cast<uint8_t*>(data.data());
        z.avail_in = uInt(data.size());
        z.next_out = out.data();
        z.avail_out = uInt(out.size());
        const int result = inflate(&z, Z_FINISH);
        const uLong made = z.total_out;
        inflateEnd(&z);
        if (result != Z_STREAM_END || made != it.size) throw Error(names[index] + " does not inflate");
        return out;
    }
}
