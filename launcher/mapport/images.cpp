#include "images.h"
#include "wavelet.h"
#include "tree.h"

#include <algorithm>
#include <array>
#include <compare>
#include <cmath>
#include <cstring>
#include <map>
#include <mutex>
#include <numbers>
#include <tuple>

namespace mapport
{
    namespace
    {
        const Format kFormats[] = {
            { Gpu::DXT1, "DXT1", 4, 8 },   { Gpu::DXT3, "DXT3", 4, 16 },  { Gpu::DXT5, "DXT5", 4, 16 },
            { Gpu::DXN, "DXN", 4, 16 },    { Gpu::DXT5A, "DXT5A", 4, 8 }, { Gpu::ARGB8, "ARGB8", 1, 4 },
            { Gpu::L8, "L8", 1, 1 },       { Gpu::DXT3A, "DXT3A", 4, 8 },
        };

        uint32_t U16(const uint8_t* p) { return uint32_t(p[0]) | uint32_t(p[1]) << 8; }
        uint32_t U32(const uint8_t* p) { return U16(p) | U16(p + 2) << 16; }
        int Up(int v, int d) { return (v + d - 1) / d; }
        int Pow2(int v)
        {
            int r = 1;
            while (r < v) r <<= 1;
            return r;
        }
        int Log2Ceil(int v)
        {
            int r = 0;
            for (int c = v - 1; c > 0; c >>= 1) r++;
            return r;
        }
        int MipCount(int w, int h) { return Log2Ceil(std::max(w, h)) + 1; }
    }

    const Format& FormatOf(Gpu g)
    {
        for (const Format& f : kFormats)
            if (f.gpu == g) return f;
        throw Error("no such picture format");
    }

    bool FormatNamed(const std::string& name, Gpu& out)
    {
        for (const Format& f : kFormats)
            if (name == f.name)
            {
                out = f.gpu;
                return true;
            }
        return false;
    }

    // ---- .iwi ---------------------------------------------------------------

    namespace
    {
        constexpr uint32_t kNoMipmaps = 0x2, kCube = 0x10000;

        // The few uncompressed kinds without a 360 twin, made 32-bit: B, G, R, A.
        std::vector<uint8_t> Rgb24ToArgb8(const std::vector<uint8_t>& b)
        {
            std::vector<uint8_t> out(b.size() / 3 * 4);
            for (size_t i = 0; i < b.size() / 3; i++)
            {
                out[4 * i] = b[3 * i];
                out[4 * i + 1] = b[3 * i + 1];
                out[4 * i + 2] = b[3 * i + 2];
                out[4 * i + 3] = 0xFF;
            }
            return out;
        }

        // Luminance, alpha -> grey with that alpha.
        std::vector<uint8_t> La16ToArgb8(const std::vector<uint8_t>& b)
        {
            std::vector<uint8_t> out(b.size() * 2);
            for (size_t i = 0; i < b.size() / 2; i++)
            {
                out[4 * i] = out[4 * i + 1] = out[4 * i + 2] = b[2 * i];
                out[4 * i + 3] = b[2 * i + 1];
            }
            return out;
        }

        // Alpha only -> black with that alpha (what the PC's A8 samples as).
        std::vector<uint8_t> A8ToArgb8(const std::vector<uint8_t>& b)
        {
            std::vector<uint8_t> out(b.size() * 4);
            for (size_t i = 0; i < b.size(); i++) out[4 * i + 3] = b[i];
            return out;
        }

        // Luminance -> opaque grey.
        std::vector<uint8_t> L8ToArgb8(const std::vector<uint8_t>& b)
        {
            std::vector<uint8_t> out(b.size() * 4);
            for (size_t i = 0; i < b.size(); i++)
            {
                out[4 * i] = out[4 * i + 1] = out[4 * i + 2] = b[i];
                out[4 * i + 3] = 0xFF;
            }
            return out;
        }

        struct Expand
        {
            int bytes;
            std::vector<uint8_t> (*convert)(const std::vector<uint8_t>&);
        };

        const Expand* ExpandOf(uint8_t fmt)
        {
            static const Expand kRgb{ 3, Rgb24ToArgb8 }, kLa{ 2, La16ToArgb8 }, kA{ 1, A8ToArgb8 };
            switch (fmt)
            {
            case 0x02: return &kRgb;
            case 0x03: return &kLa;
            case 0x04: return &kA;
            default: return nullptr;
            }
        }

        bool Compressed(uint8_t fmt, Gpu& g)
        {
            switch (fmt)
            {
            case 0x0B: g = Gpu::DXT1; return true;
            case 0x0C: g = Gpu::DXT3; return true;
            case 0x0D: g = Gpu::DXT5; return true;
            case 0x01: g = Gpu::ARGB8; return true;
            default: return false;
            }
        }

        bool Wavelet(uint8_t fmt) { return fmt >= 0x06 && fmt <= 0x0A; }

        // A wavelet kind: the channels its stream has, and how they become
        // 32-bit pixels (none: B, G, R, A already).
        Expand WaveletKind(uint8_t fmt)
        {
            switch (fmt)
            {
            case 0x06: return { 4, nullptr };
            case 0x07: return { 3, Rgb24ToArgb8 };
            case 0x08: return { 2, La16ToArgb8 };
            case 0x09: return { 1, L8ToArgb8 };
            default: return { 1, A8ToArgb8 };
            }
        }

        struct IwiHeader
        {
            uint32_t flags;      // as IW4 has them
            uint8_t format;
            int width, height;
            size_t size;         // the header's own
            uint32_t fileSize;   // the file's, as the header gives it
        };

        // The header read as laid out in version 8 (IW4's own) or 6 (CoD4 /
        // World at War's).
        IwiHeader HeaderAs(int layout, const std::vector<uint8_t>& d)
        {
            if (layout == 8) return { U32(&d[4]), d[8], int(U16(&d[10])), int(U16(&d[12])), 32, U32(&d[16]) };
            const uint32_t flags = (d[5] & 2 ? kNoMipmaps : 0) | (d[5] & 4 ? kCube : 0);
            return { flags, d[4], int(U16(&d[6])), int(U16(&d[8])), 28, U32(&d[12]) };
        }

        constexpr int kLargestSide = 8192;   // the PC game's and the 360's largest picture

        // Another version is read when its header is laid out as one of those
        // and its file size matches.
        IwiHeader Header(const std::vector<uint8_t>& d)
        {
            if (d.size() < 32 || std::memcmp(d.data(), "IWi", 3) != 0) throw Error("not an .iwi picture");
            const uint8_t version = d[3];
            if (version == 8 || version == 6)
            {
                const IwiHeader h = HeaderAs(version, d);
                if (h.width < 1 || h.height < 1 || h.width > kLargestSide || h.height > kLargestSide)
                    throw Error("an .iwi picture of " + std::to_string(h.width) + "x" + std::to_string(h.height));
                return h;
            }
            for (int layout : { 8, 6 })
            {
                const IwiHeader h = HeaderAs(layout, d);
                Gpu g;
                if (h.fileSize == d.size() && h.width && h.height && h.width <= kLargestSide && h.height <= kLargestSide &&
                    (Compressed(h.format, g) || ExpandOf(h.format) || Wavelet(h.format)))
                    return h;
            }
            throw Error(".iwi version " + std::to_string(version) + " isn't supported");
        }
    }

    Picture ReadIwi(const std::vector<uint8_t>& data)
    {
        const auto [flags, fmt, w, h, head, fileSize] = Header(data);
        Picture pic;
        if (Wavelet(fmt))
        {
            // Every mip level decoded, then made 32-bit pixels.
            if (flags & kCube) throw Error("wavelet cube map pictures aren't supported yet");
            if (flags & kNoMipmaps) throw Error("wavelet pictures without mipmaps aren't supported");
            const Expand kind = WaveletKind(fmt);
            pic.format = Gpu::ARGB8;
            pic.width = w;
            pic.height = h;
            for (auto& level : DecodeWavelet(data.data() + head, data.size() - head, w, h, kind.bytes))
                pic.mips.push_back(kind.convert ? kind.convert(level) : std::move(level));
            return pic;
        }
        const Expand* expand = ExpandOf(fmt);
        if (expand) pic.format = Gpu::ARGB8;
        else if (!Compressed(fmt, pic.format)) throw Error("image format " + std::to_string(fmt) + " isn't supported yet");
        const Format& f = FormatOf(pic.format);
        const int bw = f.blockWidth;
        const int bpb = expand ? expand->bytes : f.blockBytes;
        std::vector<size_t> sizes;
        for (int lw = w, lh = h;;)
        {
            sizes.push_back(size_t(std::max(1, Up(lw, bw))) * size_t(std::max(1, Up(lh, bw))) * size_t(bpb));
            if ((lw == 1 && lh == 1) || (flags & kNoMipmaps)) break;
            lw = std::max(1, lw >> 1);
            lh = std::max(1, lh >> 1);
        }
        const size_t body = data.size() - head;
        pic.cube = flags & kCube;
        const size_t faces = pic.cube ? 6 : 1;
        size_t total = 0;
        for (size_t s : sizes) total += s * faces;
        if (body < total)
        {
            sizes.resize(1);
            total = sizes[0] * faces;
        }
        if (body < total) throw Error("an .iwi picture shorter than its size");
        // Smallest mip first in the file (each mip holds all faces of a cube map).
        std::vector<Mip> levels(sizes.size());
        size_t at = head + body - total;
        for (size_t i = sizes.size(); i-- > 0;)
        {
            levels[i].assign(data.begin() + ptrdiff_t(at), data.begin() + ptrdiff_t(at + sizes[i] * faces));
            at += sizes[i] * faces;
        }
        if (expand)
            for (size_t i = 0; i < levels.size(); i++)
            {
                levels[i] = expand->convert(levels[i]);
                sizes[i] = sizes[i] / size_t(expand->bytes) * 4;
            }
        pic.width = w;
        pic.height = h;
        if (pic.cube)
            for (size_t face = 0; face < 6; face++)
                pic.mips.emplace_back(levels[0].begin() + ptrdiff_t(face * sizes[0]),
                                      levels[0].begin() + ptrdiff_t((face + 1) * sizes[0]));
        else pic.mips = std::move(levels);
        return pic;
    }

    // ---- tiling (Xenos, after xenia's xenos_texture) ------------------------

    MipLayout Layout(int width, int height, int mip, Gpu g)
    {
        const Format& f = FormatOf(g);
        const int bw = f.blockWidth, bpb = f.blockBytes;
        const int wb = std::max(1, Up(std::max(width >> mip, 1), bw));
        const int hb = std::max(1, Up(std::max(height >> mip, 1), bw));
        int sw, sh;
        size_t row;
        if (mip == 0)
        {
            const int pitch = bw > 1 ? int(AlignUp(size_t(wb), 32)) / 8 : int(AlignUp(size_t(width), 32)) / 32;
            row = size_t(std::max(1, Up(pitch << 5, bw))) * size_t(bpb);
            sw = int(row / size_t(bpb));
            sh = int(AlignUp(size_t(hb), 32));
        }
        else
        {
            sw = int(AlignUp(size_t(Up(std::max(Pow2(width) >> mip, 1), bw)), 32));
            sh = int(AlignUp(size_t(Up(std::max(Pow2(height) >> mip, 1), bw)), 32));
            row = size_t(sw) * size_t(bpb);
        }
        return { wb, hb, sw, AlignUp(row * size_t(sh), 4096) };
    }

    namespace
    {
        int L2(int bpb) { return (bpb / 4) + ((bpb / 2) >> (bpb / 4)); }

        size_t BlockOffset(int x, int y, int sw, int bpb)
        {
            const int l2 = L2(bpb);
            int64_t macro = int64_t((y / 32) * (sw / 32)) << (l2 + 7);
            int64_t micro = int64_t((y & 6) << 2) << l2;
            const int64_t base = macro + ((micro & ~0xF) << 1) + (micro & 0xF) + ((y & 8) << (3 + l2)) + ((y & 1) << 4);
            macro = int64_t(x / 32) << (l2 + 7);
            micro = int64_t(x & 7) << l2;
            int64_t off = base + macro + ((micro & ~0xF) << 1) + (micro & 0xF);
            off = ((off & ~0x1FF) << 3) + ((off & 0x1C0) << 2) + (off & 0x3F) + ((y & 16) << 7) +
                  (((((y & 8) >> 2) + (x >> 3)) & 3) << 6);
            return size_t(off >> l2) * size_t(bpb);
        }

        // Tiled byte offset of every block of a mip, row by row (kept: many
        // pictures share a size).
        const std::vector<size_t>& Offsets(int wb, int hb, int ox, int oy, int sw, int bpb)
        {
            struct Grid
            {
                int wb, hb, ox, oy, sw, bpb;
                auto operator<=>(const Grid&) const = default;
            };
            // (Filled once under the lock; a map's entries stay where they are.)
            static std::mutex lock;
            static std::map<Grid, std::vector<size_t>> cache;
            std::lock_guard hold(lock);
            auto& offs = cache[{ wb, hb, ox, oy, sw, bpb }];
            if (offs.empty())
            {
                offs.reserve(size_t(wb) * size_t(hb));
                for (int y = 0; y < hb; y++)
                    for (int x = 0; x < wb; x++) offs.push_back(BlockOffset(x + ox, y + oy, sw, bpb));
            }
            return offs;
        }
    }

    std::pair<std::vector<MipPlace>, size_t> Plan(int width, int height, Gpu g)
    {
        const int bw = FormatOf(g).blockWidth;
        const int tail = std::max(0, Log2Ceil(std::min(width, height)) - 4);
        std::vector<size_t> offsets;
        size_t total = 0;
        for (int mip = 0; mip <= tail; mip++)
        {
            offsets.push_back(total);
            total += Layout(width, height, mip, g).slice;
        }
        const int tailSw = Layout(width, height, tail, g).sw;
        const bool wide = Log2Ceil(width) > Log2Ceil(height);
        std::vector<MipPlace> plan;
        const int count = MipCount(width, height);
        for (int mip = 0; mip < count; mip++)
        {
            if (mip < tail)
            {
                plan.push_back({ mip, offsets[size_t(mip)], Layout(width, height, mip, g).sw, 0, 0 });
                continue;
            }
            const int packed = mip - tail;
            int ox, oy;
            if (packed < 3)
            {
                ox = wide ? 0 : (16 >> packed) / bw;
                oy = wide ? (16 >> packed) / bw : 0;
            }
            else
            {
                // The smallest mips sit one block each along the long side, 1x1
                // nearest the corner.
                const int step = 1 << (count - 1 - mip);
                ox = wide ? step : 0;
                oy = wide ? 0 : step;
            }
            plan.push_back({ mip, offsets[size_t(tail)], tailSw, ox, oy });
        }
        return { plan, total };
    }

    std::vector<uint8_t> Swap(const std::vector<uint8_t>& data, Gpu g)
    {
        const Format& f = FormatOf(g);
        std::vector<uint8_t> b(data.size());
        if (f.blockBytes == 4 && f.blockWidth == 1)
            for (size_t i = 0; i + 3 < data.size(); i += 4)
            {
                b[i] = data[i + 3];
                b[i + 1] = data[i + 2];
                b[i + 2] = data[i + 1];
                b[i + 3] = data[i];
            }
        else
            for (size_t i = 0; i + 1 < data.size(); i += 2)
            {
                b[i] = data[i + 1];
                b[i + 1] = data[i];
            }
        return b;
    }

    std::vector<uint8_t> Tile(const std::vector<Mip>& mips, int width, int height, Gpu g, bool single)
    {
        const int bpb = FormatOf(g).blockBytes;
        std::vector<MipPlace> plan;
        size_t total;
        if (single)
        {
            const MipLayout l = Layout(width, height, 0, g);
            plan = { { 0, 0, l.sw, 0, 0 } };
            total = l.slice;
        }
        else std::tie(plan, total) = Plan(width, height, g);
        std::vector<uint8_t> blob(total);
        for (size_t i = 0; i < std::min(plan.size(), mips.size()); i++)
        {
            const MipPlace& p = plan[i];
            const MipLayout l = Layout(width, height, p.mip, g);
            const Mip& data = mips[i];
            size_t src = 0;
            for (size_t off : Offsets(l.wb, l.hb, p.ox, p.oy, p.sw, bpb))
            {
                const size_t dst = p.base + off;
                const size_t n = std::min(size_t(bpb), data.size() > src ? data.size() - src : 0);
                if (dst + n <= blob.size()) std::memcpy(blob.data() + dst, data.data() + src, n);
                src += size_t(bpb);
            }
        }
        return Swap(blob, g);
    }

    // ---- conversions --------------------------------------------------------

    namespace
    {
        // 16 values (0-255) -> one 8-byte BC4 block, an 8-step ramp between the
        // extremes.
        void Bc4Block(const int vals[16], uint8_t out[8])
        {
            const int hi = *std::max_element(vals, vals + 16), lo = *std::min_element(vals, vals + 16);
            out[0] = uint8_t(hi);
            out[1] = uint8_t(lo);
            if (hi == lo)
            {
                std::memset(out + 2, 0, 6);
                return;
            }
            int pal[8] = { hi, lo };
            for (int i = 1; i < 7; i++) pal[i + 1] = ((7 - i) * hi + i * lo) / 7;
            uint64_t bits = 0;
            for (int k = 0; k < 16; k++)
            {
                int best = 0;
                for (int i = 1; i < 8; i++)
                    if (std::abs(pal[i] - vals[k]) < std::abs(pal[best] - vals[k])) best = i;
                bits |= uint64_t(best) << (3 * k);
            }
            for (int i = 0; i < 6; i++) out[2 + i] = uint8_t(bits >> (8 * i));
        }
    }

    std::vector<Mip> NormalMapsToDxn(const std::vector<Mip>& mips)
    {
        // Green block -> its DXN block: normal maps repeat many blocks.
        std::map<uint64_t, std::array<uint8_t, 8>> done;
        std::vector<Mip> out;
        for (const Mip& mip : mips)
        {
            Mip o(mip.size());
            for (size_t at = 0; at + 15 < mip.size(); at += 16)
            {
                uint64_t key = 0;
                std::memcpy(&key, mip.data() + at + 8, 8);
                auto found = done.find(key);
                if (found == done.end())
                {
                    const uint32_t c0 = U16(mip.data() + at + 8), c1 = U16(mip.data() + at + 10), bits = U32(mip.data() + at + 12);
                    const int g0 = int((c0 >> 5) & 63) * 255 / 63, g1 = int((c1 >> 5) & 63) * 255 / 63;
                    int pal[4];
                    if (c0 > c1) pal[0] = g0, pal[1] = g1, pal[2] = (2 * g0 + g1) / 3, pal[3] = (g0 + 2 * g1) / 3;
                    else pal[0] = g0, pal[1] = g1, pal[2] = (g0 + g1) / 2, pal[3] = 0;
                    int vals[16];
                    for (int k = 0; k < 16; k++) vals[k] = pal[(bits >> (2 * k)) & 3];
                    std::array<uint8_t, 8> block;
                    Bc4Block(vals, block.data());
                    found = done.emplace(key, block).first;
                }
                std::memcpy(o.data() + at, mip.data() + at, 8);
                std::memcpy(o.data() + at + 8, found->second.data(), 8);
            }
            out.push_back(std::move(o));
        }
        return out;
    }

    Mip EncodeDxt3a(const std::vector<uint8_t>& lum, int width, int height)
    {
        const int bw = std::max(1, width / 4), bh = std::max(1, height / 4);
        Mip out(size_t(bw) * size_t(bh) * 8);
        uint8_t q[256];
        for (int v = 0; v < 256; v++) q[v] = uint8_t(std::min(15, (v * 15 + 127) / 255));
        for (int by = 0; by < bh; by++)
            for (int bx = 0; bx < bw; bx++)
            {
                const size_t o = (size_t(by) * size_t(bw) + size_t(bx)) * 8;
                for (int y = 0; y < 4; y++)
                {
                    const size_t row = size_t(by * 4 + y) * size_t(width) + size_t(bx * 4);
                    auto px = [&](int k) { return row + size_t(k) < lum.size() ? lum[row + size_t(k)] : uint8_t(0); };
                    out[o + 2 * size_t(y)] = uint8_t(q[px(0)] | (q[px(1)] << 4));
                    out[o + 2 * size_t(y) + 1] = uint8_t(q[px(2)] | (q[px(3)] << 4));
                }
            }
        return out;
    }

    Mip DecodeProbe(const Mip& bgra)
    {
        Mip out = bgra;
        std::map<uint8_t, std::array<uint8_t, 256>> lut;
        for (size_t o = 0; o + 3 < bgra.size(); o += 4)
        {
            const uint8_t a = bgra[o + 3];
            auto found = lut.find(a);
            if (found == lut.end())
            {
                std::array<uint8_t, 256> t;
                const double k = a / 255.0 * kProbeScale;
                for (int v = 0; v < 256; v++) t[size_t(v)] = uint8_t(std::min(255, int(v * k + 0.5)));
                found = lut.emplace(a, t).first;
            }
            for (int c = 0; c < 3; c++) out[o + size_t(c)] = found->second[bgra[o + size_t(c)]];
        }
        return out;
    }

    // ---- the DXT encoder ----------------------------------------------------

    namespace
    {
        using Pixel = std::array<int, 4>;

        uint32_t Rgb565(const Pixel& c)
        {
            return uint32_t(((c[0] * 31 + 127) / 255) << 11 | ((c[1] * 63 + 127) / 255) << 5 | ((c[2] * 31 + 127) / 255));
        }

        std::array<int, 3> Unpack565(uint32_t v)
        {
            const int r = int(v >> 11) & 31, g = int(v >> 5) & 63, b = int(v) & 31;
            return { (r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2) };
        }

        void Put(Mip& out, uint32_t c0, uint32_t c1, uint32_t bits)
        {
            const uint8_t b[8] = { uint8_t(c0), uint8_t(c0 >> 8), uint8_t(c1), uint8_t(c1 >> 8),
                                   uint8_t(bits), uint8_t(bits >> 8), uint8_t(bits >> 16), uint8_t(bits >> 24) };
            out.insert(out.end(), b, b + 8);
        }

        // 16 RGBA pixels -> an 8-byte DXT1-style colour block: the extremes
        // along the colours' principal axis.
        void ColorBlock(const std::array<Pixel, 16>& pixels, bool punchThrough, Mip& out)
        {
            std::vector<const Pixel*> use;
            for (const Pixel& p : pixels)
                if (!(punchThrough && p[3] < 128)) use.push_back(&p);
            if (use.empty())
                for (const Pixel& p : pixels) use.push_back(&p);
            const double n = double(use.size());
            double mean[3];
            for (int i = 0; i < 3; i++)
            {
                int64_t sum = 0;
                for (const Pixel* p : use) sum += (*p)[size_t(i)];
                mean[i] = double(sum) / n;
            }
            double cov[3][3];
            for (int i = 0; i < 3; i++)
                for (int j = 0; j < 3; j++)
                {
                    double sum = 0;
                    for (const Pixel* p : use) sum += ((*p)[size_t(i)] - mean[i]) * ((*p)[size_t(j)] - mean[j]);
                    cov[i][j] = sum;
                }
            double axis[3] = { 1.0, 1.0, 1.0 };
            for (int it = 0; it < 4; it++)
            {
                double next[3];
                for (int i = 0; i < 3; i++)
                {
                    double sum = 0;
                    for (int j = 0; j < 3; j++) sum += cov[i][j] * axis[j];
                    next[i] = sum;
                }
                double norm = std::max({ std::abs(next[0]), std::abs(next[1]), std::abs(next[2]) });
                if (norm == 0) norm = 1.0;
                for (int i = 0; i < 3; i++) axis[i] = next[i] / norm;
            }
            size_t lo = 0, hi = 0;
            std::vector<double> proj;
            for (const Pixel* p : use)
            {
                double sum = 0;
                for (int i = 0; i < 3; i++) sum += ((*p)[size_t(i)] - mean[i]) * axis[i];
                proj.push_back(sum);
            }
            for (size_t k = 1; k < proj.size(); k++)
            {
                if (proj[k] < proj[lo]) lo = k;
                if (proj[k] > proj[hi]) hi = k;
            }
            uint32_t c0 = Rgb565(*use[hi]), c1 = Rgb565(*use[lo]);
            bool transparent = false;
            if (punchThrough)
                for (const Pixel& p : pixels) transparent |= p[3] < 128;
            if (transparent)
            {
                if (c0 > c1) std::swap(c0, c1);
            }
            else
            {
                if (c0 < c1) std::swap(c0, c1);
                if (c0 == c1)
                {
                    Put(out, c0, c1, 0);
                    return;
                }
            }
            const auto a = Unpack565(c0), b = Unpack565(c1);
            std::vector<std::array<int, 3>> palette{ a, b };
            if (transparent || c0 <= c1) palette.push_back({ (a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2 });
            else
            {
                palette.push_back({ (2 * a[0] + b[0]) / 3, (2 * a[1] + b[1]) / 3, (2 * a[2] + b[2]) / 3 });
                palette.push_back({ (a[0] + 2 * b[0]) / 3, (a[1] + 2 * b[1]) / 3, (a[2] + 2 * b[2]) / 3 });
            }
            uint32_t bits = 0;
            for (int k = 0; k < 16; k++)
            {
                const Pixel& p = pixels[size_t(k)];
                uint32_t index = 0;
                if (transparent && p[3] < 128) index = 3;
                else
                {
                    int best = -1;
                    for (size_t i = 0; i < palette.size(); i++)
                    {
                        int d = 0;
                        for (int c = 0; c < 3; c++) d += (p[size_t(c)] - palette[i][size_t(c)]) * (p[size_t(c)] - palette[i][size_t(c)]);
                        if (best < 0 || d < best)
                        {
                            best = d;
                            index = uint32_t(i);
                        }
                    }
                }
                bits |= index << (2 * k);
            }
            Put(out, c0, c1, bits);
        }

        // 16 values -> an 8-byte DXT5 / BC4 alpha block.
        void AlphaBlock(const std::array<int, 16>& values, Mip& out)
        {
            const int a0 = *std::max_element(values.begin(), values.end());
            const int a1 = *std::min_element(values.begin(), values.end());
            out.push_back(uint8_t(a0));
            out.push_back(uint8_t(a1));
            if (a0 == a1)
            {
                out.insert(out.end(), 6, 0);
                return;
            }
            int palette[8] = { a0, a1 };
            for (int i = 1; i < 7; i++) palette[i + 1] = ((7 - i) * a0 + i * a1) / 7;
            uint64_t bits = 0;
            for (int k = 0; k < 16; k++)
            {
                int best = 0;
                for (int i = 1; i < 8; i++)
                    if (std::abs(values[size_t(k)] - palette[i]) < std::abs(values[size_t(k)] - palette[best])) best = i;
                bits |= uint64_t(best) << (3 * k);
            }
            for (int i = 0; i < 6; i++) out.push_back(uint8_t(bits >> (8 * i)));
        }
    }

    Mip EncodePixels(const std::vector<uint8_t>& rgba, int width, int height, Gpu g)
    {
        Mip out;
        if (g == Gpu::ARGB8)
        {
            out = rgba;
            for (size_t i = 0; i + 3 < out.size(); i += 4) std::swap(out[i], out[i + 2]);   // RGBA -> BGRA
            return out;
        }
        for (int by = 0; by < std::max(height, 1); by += 4)
            for (int bx = 0; bx < std::max(width, 1); bx += 4)
            {
                std::array<Pixel, 16> pixels;
                for (int y = 0; y < 4; y++)
                    for (int x = 0; x < 4; x++)
                    {
                        const size_t i = (size_t(std::min(by + y, height - 1)) * size_t(width) + size_t(std::min(bx + x, width - 1))) * 4;
                        pixels[size_t(y * 4 + x)] = { rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3] };
                    }
                auto channel = [&](int c) {
                    std::array<int, 16> v;
                    for (int k = 0; k < 16; k++) v[size_t(k)] = pixels[size_t(k)][size_t(c)];
                    return v;
                };
                switch (g)
                {
                case Gpu::DXT1:
                    ColorBlock(pixels, true, out);
                    break;
                case Gpu::DXT3:
                {
                    uint64_t alpha = 0;
                    for (int k = 0; k < 16; k++) alpha |= uint64_t((pixels[size_t(k)][3] * 15 + 127) / 255) << (4 * k);
                    for (int i = 0; i < 8; i++) out.push_back(uint8_t(alpha >> (8 * i)));
                    ColorBlock(pixels, false, out);
                    break;
                }
                case Gpu::DXT5:
                    AlphaBlock(channel(3), out);
                    ColorBlock(pixels, false, out);
                    break;
                case Gpu::DXT5A:
                    AlphaBlock(channel(0), out);
                    break;
                case Gpu::DXN:
                    AlphaBlock(channel(0), out);
                    AlphaBlock(channel(1), out);
                    break;
                default:
                    throw Error("no encoder for this picture format");
                }
            }
        return out;
    }

    // ---- Pillow: decoding and resizing --------------------------------------

    namespace
    {
        // Pillow's bcn decoder (libImaging/BcnDecode.c).
        std::array<uint8_t, 4> Decode565(uint32_t v)
        {
            uint8_t r = uint8_t((v & 0xf800) >> 8);
            r |= r >> 5;
            uint8_t g = uint8_t((v & 0x7e0) >> 3);
            g |= g >> 6;
            uint8_t b = uint8_t((v & 0x1f) << 3);
            b |= b >> 5;
            return { r, g, b, 0xff };
        }

        void DecodeColor(std::array<uint8_t, 4> dst[16], const uint8_t* src, bool separateAlpha)
        {
            const uint32_t c0 = U16(src), c1 = U16(src + 2), lut = U32(src + 4);
            std::array<uint8_t, 4> p[4];
            p[0] = Decode565(c0);
            p[1] = Decode565(c1);
            const int r0 = p[0][0], g0 = p[0][1], b0 = p[0][2], r1 = p[1][0], g1 = p[1][1], b1 = p[1][2];
            if (c0 > c1 || separateAlpha)
            {
                p[2] = { uint8_t((2 * r0 + r1) / 3), uint8_t((2 * g0 + g1) / 3), uint8_t((2 * b0 + b1) / 3), 0xff };
                p[3] = { uint8_t((r0 + 2 * r1) / 3), uint8_t((g0 + 2 * g1) / 3), uint8_t((b0 + 2 * b1) / 3), 0xff };
            }
            else
            {
                p[2] = { uint8_t((r0 + r1) / 2), uint8_t((g0 + g1) / 2), uint8_t((b0 + b1) / 2), 0xff };
                p[3] = { 0, 0, 0, 0 };
            }
            for (int n = 0; n < 16; n++) dst[n] = p[(lut >> (2 * n)) & 3];
        }

        void DecodeBc3Alpha(std::array<uint8_t, 4> dst[16], const uint8_t* src)
        {
            const int a0 = src[0], a1 = src[1];
            uint8_t a[8] = { uint8_t(a0), uint8_t(a1) };
            if (a0 > a1)
                for (int i = 1; i < 7; i++) a[i + 1] = uint8_t(((7 - i) * a0 + i * a1) / 7);
            else
            {
                for (int i = 1; i < 5; i++) a[i + 1] = uint8_t(((5 - i) * a0 + i * a1) / 5);
                a[6] = 0;
                a[7] = 0xff;
            }
            const uint32_t lut1 = uint32_t(src[2]) | uint32_t(src[3]) << 8 | uint32_t(src[4]) << 16;
            const uint32_t lut2 = uint32_t(src[5]) | uint32_t(src[6]) << 8 | uint32_t(src[7]) << 16;
            for (int n = 0; n < 8; n++) dst[n][3] = a[(lut1 >> (3 * n)) & 7];
            for (int n = 0; n < 8; n++) dst[8 + n][3] = a[(lut2 >> (3 * n)) & 7];
        }
    }

    std::vector<uint8_t> DecodePixels(const Mip& level, int width, int height, Gpu g)
    {
        std::vector<uint8_t> out(size_t(width) * size_t(height) * 4);
        if (g == Gpu::ARGB8)
        {
            for (size_t i = 0; i + 3 < out.size() && i + 3 < level.size(); i += 4)
            {
                out[i] = level[i + 2];
                out[i + 1] = level[i + 1];
                out[i + 2] = level[i];
                out[i + 3] = level[i + 3];
            }
            return out;
        }
        const int blockBytes = FormatOf(g).blockBytes;
        const int bw = Up(width, 4), bh = Up(height, 4);
        for (int by = 0; by < bh; by++)
            for (int bx = 0; bx < bw; bx++)
            {
                const size_t at = (size_t(by) * size_t(bw) + size_t(bx)) * size_t(blockBytes);
                if (at + size_t(blockBytes) > level.size()) continue;
                const uint8_t* src = level.data() + at;
                std::array<uint8_t, 4> col[16];
                switch (g)
                {
                case Gpu::DXT1:
                    DecodeColor(col, src, false);
                    break;
                case Gpu::DXT3:
                    DecodeColor(col, src + 8, true);
                    for (int n = 0; n < 16; n++)
                    {
                        const int bit = n * 4;
                        const int av = 0xf & (src[bit >> 3] >> (bit & 7));
                        col[n][3] = uint8_t((av << 4) | av);
                    }
                    break;
                case Gpu::DXT5:
                    DecodeColor(col, src + 8, true);
                    DecodeBc3Alpha(col, src);
                    break;
                default:
                    throw Error("no decoder for this picture format");
                }
                for (int n = 0; n < 16; n++)
                {
                    const int x = bx * 4 + n % 4, y = by * 4 + n / 4;
                    if (x < width && y < height) std::memcpy(&out[(size_t(y) * size_t(width) + size_t(x)) * 4], col[n].data(), 4);
                }
            }
        return out;
    }

    namespace
    {
        // Pillow's resampling (libImaging/Resample.c): coefficients in
        // doubles, applied in fixed point, horizontally then vertically.
        constexpr int kPrecision = 32 - 8 - 2;

        double Sinc(double x)
        {
            if (x == 0.0) return 1.0;
            x = x * std::numbers::pi;
            return std::sin(x) / x;
        }

        double Lanczos(double x) { return (-3.0 <= x && x < 3.0) ? Sinc(x) * Sinc(x / 3) : 0.0; }

        struct Coeffs
        {
            int ksize;
            std::vector<int> bounds;    // (min, count) per output pixel
            std::vector<int32_t> kk;
        };

        Coeffs Precompute(int inSize, double in0, double in1, int outSize)
        {
            const double scale = (in1 - in0) / outSize;
            const double filterscale = std::max(scale, 1.0);
            const double support = 3.0 * filterscale;
            Coeffs c;
            c.ksize = int(std::ceil(support)) * 2 + 1;
            c.bounds.resize(size_t(outSize) * 2);
            std::vector<double> kk(size_t(outSize) * size_t(c.ksize), 0.0);
            for (int xx = 0; xx < outSize; xx++)
            {
                const double center = in0 + (xx + 0.5) * scale;
                double ww = 0.0;
                const double ss = 1.0 / filterscale;
                int xmin = int(center - support + 0.5);
                if (xmin < 0) xmin = 0;
                int xmax = int(center + support + 0.5);
                if (xmax > inSize) xmax = inSize;
                xmax -= xmin;
                double* k = &kk[size_t(xx) * size_t(c.ksize)];
                for (int x = 0; x < xmax; x++)
                {
                    const double w = Lanczos((x + xmin - center + 0.5) * ss);
                    k[x] = w;
                    ww += w;
                }
                for (int x = 0; x < xmax; x++)
                    if (ww != 0.0) k[x] /= ww;
                c.bounds[size_t(xx) * 2] = xmin;
                c.bounds[size_t(xx) * 2 + 1] = xmax;
            }
            c.kk.resize(kk.size());
            for (size_t x = 0; x < kk.size(); x++)
                c.kk[x] = kk[x] < 0 ? int32_t(-0.5 + kk[x] * (1 << kPrecision)) : int32_t(0.5 + kk[x] * (1 << kPrecision));
            return c;
        }

        uint8_t Clip8(int64_t in)
        {
            if (in >= (int64_t(1) << kPrecision << 8)) return 255;
            if (in <= 0) return 0;
            return uint8_t(in >> kPrecision);
        }

        // MULDIV255 and Pillow's RGBA <-> RGBa (premultiplied) conversions.
        uint8_t MulDiv255(uint32_t a, uint32_t b)
        {
            const uint32_t tmp = a * b + 128;
            return uint8_t(((tmp >> 8) + tmp) >> 8);
        }
    }

    std::vector<uint8_t> ResizeLanczos(const std::vector<uint8_t>& rgba, int width, int height, int toWidth, int toHeight)
    {
        if (width == toWidth && height == toHeight) return rgba;
        // RGBA -> RGBa
        std::vector<uint8_t> in(rgba.size());
        for (size_t i = 0; i + 3 < rgba.size(); i += 4)
        {
            const uint32_t alpha = rgba[i + 3];
            for (int c = 0; c < 3; c++) in[i + size_t(c)] = MulDiv255(rgba[i + size_t(c)], alpha);
            in[i + 3] = rgba[i + 3];
        }
        const Coeffs h = Precompute(width, 0, width, toWidth);
        const Coeffs v = Precompute(height, 0, height, toHeight);
        const bool needH = toWidth != width, needV = toHeight != height;
        std::vector<int> vb = v.bounds;
        const int yFirst = vb[0], yLast = vb[size_t(toHeight) * 2 - 2] + vb[size_t(toHeight) * 2 - 1];
        int curW = width, curH = height;
        std::vector<uint8_t> cur = std::move(in);
        if (needH)
        {
            for (int i = 0; i < toHeight; i++) vb[size_t(i) * 2] -= yFirst;
            const int rows = yLast - yFirst;
            std::vector<uint8_t> tmp(size_t(toWidth) * size_t(rows) * 4);
            for (int yy = 0; yy < rows; yy++)
                for (int xx = 0; xx < toWidth; xx++)
                {
                    const int xmin = h.bounds[size_t(xx) * 2], xmax = h.bounds[size_t(xx) * 2 + 1];
                    const int32_t* k = &h.kk[size_t(xx) * size_t(h.ksize)];
                    int64_t ss[4];
                    for (int64_t& s : ss) s = int64_t(1) << (kPrecision - 1);
                    for (int x = 0; x < xmax; x++)
                    {
                        const uint8_t* px = &cur[(size_t(yy + yFirst) * size_t(curW) + size_t(x + xmin)) * 4];
                        for (int c = 0; c < 4; c++) ss[c] += int64_t(int32_t(px[c]) * k[x]);
                    }
                    for (int c = 0; c < 4; c++) tmp[(size_t(yy) * size_t(toWidth) + size_t(xx)) * 4 + size_t(c)] = Clip8(int32_t(ss[c]));
                }
            cur = std::move(tmp);
            curW = toWidth;
            curH = rows;
        }
        if (needV)
        {
            std::vector<uint8_t> tmp(size_t(curW) * size_t(toHeight) * 4);
            for (int yy = 0; yy < toHeight; yy++)
            {
                const int ymin = vb[size_t(yy) * 2], ymax = vb[size_t(yy) * 2 + 1];
                const int32_t* k = &v.kk[size_t(yy) * size_t(v.ksize)];
                for (int xx = 0; xx < curW; xx++)
                {
                    int64_t ss[4];
                    for (int64_t& s : ss) s = int64_t(1) << (kPrecision - 1);
                    for (int y = 0; y < ymax; y++)
                    {
                        const uint8_t* px = &cur[(size_t(y + ymin) * size_t(curW) + size_t(xx)) * 4];
                        for (int c = 0; c < 4; c++) ss[c] += int64_t(int32_t(px[c]) * k[y]);
                    }
                    for (int c = 0; c < 4; c++) tmp[(size_t(yy) * size_t(curW) + size_t(xx)) * 4 + size_t(c)] = Clip8(int32_t(ss[c]));
                }
            }
            cur = std::move(tmp);
            curH = toHeight;
        }
        (void)curH;
        // RGBa -> RGBA
        for (size_t i = 0; i + 3 < cur.size(); i += 4)
        {
            const uint32_t alpha = cur[i + 3];
            if (alpha != 255 && alpha != 0)
                for (int c = 0; c < 3; c++) cur[i + size_t(c)] = uint8_t(std::min<uint32_t>(255, (255 * cur[i + size_t(c)]) / alpha));
        }
        return cur;
    }

    std::pair<int, int> PictureSize(int width, int height, int limit)
    {
        auto p2 = [&](int v) {
            int r = 1;
            while (r * 2 <= v) r *= 2;
            return std::min(r, limit);
        };
        return { p2(width), p2(height) };
    }

    Fitted FitPicture(Gpu f, int width, int height, const std::vector<Mip>& mips, int limit)
    {
        const auto [nw, nh] = PictureSize(width, height, limit);
        if (nw == width && nh == height) return { width, height, mips };
        const std::vector<uint8_t> rgba = DecodePixels(mips.at(0), width, height, f);
        const std::vector<uint8_t> resized = ResizeLanczos(rgba, width, height, nw, nh);
        return { nw, nh, { EncodePixels(resized, nw, nh, f) } };
    }
}
