#include "wavelet.h"
#include "tree.h"

#include <array>

namespace mapport
{
    namespace
    {
        constexpr int kEscape = -32768;

        // A codebook entry: its symbol, its code (least significant bit
        // first) and the code's length.
        struct Code
        {
            int symbol, code, length;
        };
        // How a codebook's escape code works: the code, its length, the raw
        // bits after it and the bias taken off them.
        struct Escape
        {
            int code, length, bits, bias;
        };

        // Codebook A: refinement and the last channel.
        constexpr Code kBookACodes[] = {
            { 0, 1, 1 }, { 1, 2, 4 }, { -1, 10, 4 }, { 2, 12, 5 }, { -2, 28, 5 }, { 3, 22, 6 }, { 4, 24, 6 },
            { -3, 54, 6 }, { -4, 56, 6 }, { 7, 4, 7 }, { 5, 46, 7 }, { 6, 52, 7 }, { -7, 68, 7 }, { -5, 110, 7 },
            { -6, 116, 7 }, { 11, 6, 8 }, { 14, 8, 8 }, { 12, 20, 8 }, { 9, 30, 8 }, { 15, 72, 8 }, { 10, 102, 8 },
            { 13, 104, 8 }, { 8, 126, 8 }, { -11, 134, 8 }, { -14, 136, 8 }, { -12, 148, 8 }, { -9, 158, 8 },
            { -15, 200, 8 }, { -10, 230, 8 }, { -13, 232, 8 }, { -8, 254, 8 }, { 23, 40, 9 }, { 19, 70, 9 },
            { 20, 84, 9 }, { 17, 142, 9 }, { 22, 164, 9 }, { 24, 168, 9 }, { 18, 198, 9 }, { 16, 222, 9 },
            { 21, 228, 9 }, { -23, 296, 9 }, { -19, 326, 9 }, { -20, 340, 9 }, { -17, 398, 9 }, { -22, 420, 9 },
            { -24, 424, 9 }, { -18, 454, 9 }, { -16, 478, 9 }, { -21, 484, 9 }, { 29, 14, 10 }, { 37, 36, 10 },
            { 31, 38, 10 }, { 28, 78, 10 }, { 35, 100, 10 }, { 32, 190, 10 }, { 33, 212, 10 }, { 36, 292, 10 },
            { 30, 294, 10 }, { 25, 318, 10 }, { 26, 350, 10 }, { 34, 356, 10 }, { 127, 422, 10 }, { 27, 462, 10 },
            { 128, 468, 10 }, { -29, 526, 10 }, { -37, 548, 10 }, { -31, 550, 10 }, { -28, 590, 10 },
            { -35, 612, 10 }, { -32, 702, 10 }, { -33, 724, 10 }, { -36, 804, 10 }, { -30, 806, 10 },
            { -25, 830, 10 }, { -26, 862, 10 }, { -34, 868, 10 }, { -127, 934, 10 }, { -27, 974, 10 },
            { -128, 980, 10 }, { 41, 62, 11 }, { 43, 94, 11 }, { 50, 166, 11 }, { 48, 206, 11 }, { 49, 270, 11 },
            { 64, 334, 11 }, { 39, 446, 11 }, { 40, 574, 11 }, { 42, 606, 11 }, { 47, 678, 11 }, { 44, 718, 11 },
            { 46, 782, 11 }, { 45, 846, 11 }, { 38, 958, 11 }, { -41, 1086, 11 }, { -43, 1118, 11 },
            { -50, 1190, 11 }, { -48, 1230, 11 }, { -49, 1294, 11 }, { -64, 1358, 11 }, { -39, 1470, 11 },
            { -40, 1598, 11 }, { -42, 1630, 11 }, { -47, 1702, 11 }, { -44, 1742, 11 }, { -46, 1806, 11 },
            { -45, 1870, 11 }, { -38, 1982, 11 },
        };
        constexpr Escape kBookAEscape{ 0, 4, 9, 255 };
        // Codebook B: the first channel's detail.
        constexpr Code kBookBCodes[] = {
            { 0, 1, 3 }, { 4, 4, 5 }, { 2, 5, 5 }, { 1, 7, 5 }, { 3, 10, 5 }, { -4, 20, 5 }, { -2, 21, 5 },
            { -1, 23, 5 }, { -3, 26, 5 }, { 12, 0, 6 }, { 10, 2, 6 }, { 7, 3, 6 }, { 9, 6, 6 }, { 6, 11, 6 },
            { 11, 24, 6 }, { 8, 30, 6 }, { 5, 31, 6 }, { -12, 32, 6 }, { -10, 34, 6 }, { -7, 35, 6 }, { -9, 38, 6 },
            { -6, 43, 6 }, { -11, 56, 6 }, { -8, 62, 6 }, { -5, 63, 6 }, { 13, 15, 7 }, { 19, 18, 7 }, { 18, 22, 7 },
            { 14, 27, 7 }, { 21, 40, 7 }, { 20, 44, 7 }, { 16, 45, 7 }, { 17, 46, 7 }, { 22, 48, 7 }, { 15, 61, 7 },
            { -13, 79, 7 }, { -19, 82, 7 }, { -18, 86, 7 }, { -14, 91, 7 }, { -21, 104, 7 }, { -20, 108, 7 },
            { -16, 109, 7 }, { -17, 110, 7 }, { -22, 112, 7 }, { -15, 125, 7 }, { 34, 8, 8 }, { 28, 13, 8 },
            { 29, 14, 8 }, { 26, 19, 8 }, { 27, 29, 8 }, { 23, 47, 8 }, { 25, 51, 8 }, { 24, 59, 8 }, { 33, 72, 8 },
            { 32, 76, 8 }, { 31, 92, 8 }, { 30, 114, 8 }, { -34, 136, 8 }, { -28, 141, 8 }, { -29, 142, 8 },
            { -26, 147, 8 }, { -27, 157, 8 }, { -23, 175, 8 }, { -25, 179, 8 }, { -24, 187, 8 }, { -33, 200, 8 },
            { -32, 204, 8 }, { -31, 220, 8 }, { -30, 242, 8 }, { 47, 12, 9 }, { 46, 28, 9 }, { 45, 50, 9 },
            { 44, 54, 9 }, { 48, 80, 9 }, { 43, 118, 9 }, { 37, 123, 9 }, { 49, 144, 9 }, { 40, 205, 9 },
            { 41, 206, 9 }, { 38, 211, 9 }, { 39, 221, 9 }, { 35, 239, 9 }, { 42, 246, 9 }, { 36, 251, 9 },
            { -47, 268, 9 }, { -46, 284, 9 }, { -45, 306, 9 }, { -44, 310, 9 }, { -48, 336, 9 }, { -43, 374, 9 },
            { -37, 379, 9 }, { -49, 400, 9 }, { -40, 461, 9 }, { -41, 462, 9 }, { -38, 467, 9 }, { -39, 477, 9 },
            { -35, 495, 9 }, { -42, 502, 9 }, { -36, 507, 9 }, { 65, 16, 10 }, { 56, 77, 10 }, { 57, 78, 10 },
            { 55, 93, 10 }, { 62, 140, 10 }, { 61, 156, 10 }, { 64, 272, 10 }, { 53, 339, 10 }, { 54, 349, 10 },
            { 50, 367, 10 }, { 52, 371, 10 }, { 60, 412, 10 }, { 59, 434, 10 }, { 58, 438, 10 }, { 63, 464, 10 },
            { 51, 499, 10 }, { -65, 528, 10 }, { -56, 589, 10 }, { -57, 590, 10 }, { -55, 605, 10 },
            { -62, 652, 10 }, { -61, 668, 10 }, { -64, 784, 10 }, { -53, 851, 10 }, { -54, 861, 10 },
            { -50, 879, 10 }, { -52, 883, 10 }, { -60, 924, 10 }, { -59, 946, 10 }, { -58, 950, 10 },
            { -63, 976, 10 }, { -51, 1011, 10 }, { 70, 83, 11 }, { 66, 111, 11 }, { 69, 115, 11 }, { 77, 178, 11 },
            { 75, 182, 11 }, { 81, 208, 11 }, { 73, 334, 11 }, { 79, 396, 11 }, { 68, 627, 11 }, { 76, 690, 11 },
            { 74, 694, 11 }, { 80, 720, 11 }, { 67, 755, 11 }, { 71, 845, 11 }, { 72, 846, 11 }, { 78, 908, 11 },
            { -70, 1107, 11 }, { -66, 1135, 11 }, { -69, 1139, 11 }, { -77, 1202, 11 }, { -75, 1206, 11 },
            { -81, 1232, 11 }, { -73, 1358, 11 }, { -79, 1420, 11 }, { -68, 1651, 11 }, { -76, 1714, 11 },
            { -74, 1718, 11 }, { -80, 1744, 11 }, { -67, 1779, 11 }, { -71, 1869, 11 }, { -72, 1870, 11 },
            { -78, 1932, 11 }, { 85, 243, 12 }, { 89, 333, 12 }, { 87, 595, 12 }, { 83, 623, 12 }, { 84, 1267, 12 },
            { 88, 1357, 12 }, { 86, 1619, 12 }, { 82, 1647, 12 }, { -85, 2291, 12 }, { -89, 2381, 12 },
            { -87, 2643, 12 }, { -83, 2671, 12 }, { -84, 3315, 12 }, { -88, 3405, 12 }, { -86, 3667, 12 },
            { -82, 3695, 12 },
        };
        constexpr Escape kBookBEscape{ 60, 6, 9, 255 };
        // Codebook C: the other colour channels' detail, as a difference from the first.
        constexpr Code kBookCCodes[] = {
            { 0, 3, 2 }, { 1, 2, 3 }, { -1, 6, 3 }, { 2, 1, 4 }, { -2, 9, 4 }, { 4, 4, 5 }, { 3, 13, 5 },
            { -4, 20, 5 }, { -3, 29, 5 }, { 6, 12, 6 }, { 7, 16, 6 }, { 5, 21, 6 }, { -6, 44, 6 }, { -7, 48, 6 },
            { -5, 53, 6 }, { 10, 24, 7 }, { 9, 28, 7 }, { 11, 32, 7 }, { 8, 37, 7 }, { -10, 88, 7 }, { -9, 92, 7 },
            { -11, 96, 7 }, { -8, 101, 7 }, { 14, 56, 8 }, { 16, 64, 8 }, { 12, 69, 8 }, { 15, 72, 8 },
            { 13, 124, 8 }, { -14, 184, 8 }, { -16, 192, 8 }, { -12, 197, 8 }, { -15, 200, 8 }, { -13, 252, 8 },
            { 22, 128, 9 }, { 17, 133, 9 }, { 21, 136, 9 }, { 20, 168, 9 }, { 18, 188, 9 }, { 19, 248, 9 },
            { -22, 384, 9 }, { -17, 389, 9 }, { -21, 392, 9 }, { -20, 424, 9 }, { -18, 444, 9 }, { -19, 504, 9 },
            { 30, 0, 10 }, { 25, 60, 10 }, { 26, 120, 10 }, { 29, 256, 10 }, { 23, 261, 10 }, { 28, 264, 10 },
            { 27, 296, 10 }, { 24, 316, 10 }, { -30, 512, 10 }, { -25, 572, 10 }, { -26, 632, 10 }, { -29, 768, 10 },
            { -23, 773, 10 }, { -28, 776, 10 }, { -27, 808, 10 }, { -24, 828, 10 }, { 31, 5, 11 }, { 37, 8, 11 },
            { 35, 40, 11 }, { 33, 376, 11 }, { 36, 520, 11 }, { 34, 552, 11 }, { 32, 888, 11 }, { -31, 1029, 11 },
            { -37, 1032, 11 }, { -35, 1064, 11 }, { -33, 1400, 11 }, { -36, 1544, 11 }, { -34, 1576, 11 },
            { -32, 1912, 11 }, { 39, 517, 12 }, { 38, 1541, 12 }, { -39, 2565, 12 }, { -38, 3589, 12 },
        };
        constexpr Escape kBookCEscape{ 104, 7, 10, 510 };

        // A codebook as a table of the next 12 bits of the stream: the symbol
        // they start with and its code's length.
        struct Book
        {
            std::array<int16_t, 4096> symbol;
            std::array<uint8_t, 4096> length;
            int bits, bias;
        };

        template <size_t N>
        Book Make(const Code (&codes)[N], const Escape& e)
        {
            Book b{};
            auto put = [&](int symbol, int code, int length) {
                for (int i = code; i < 4096; i += 1 << length)
                {
                    b.symbol[size_t(i)] = int16_t(symbol);
                    b.length[size_t(i)] = uint8_t(length);
                }
            };
            for (const Code& c : codes) put(c.symbol, c.code, c.length);
            put(kEscape, e.code, e.length);
            b.bits = e.bits;
            b.bias = e.bias;
            return b;
        }

        const Book& BookA()
        {
            static const Book b = Make(kBookACodes, kBookAEscape);
            return b;
        }
        const Book& BookB()
        {
            static const Book b = Make(kBookBCodes, kBookBEscape);
            return b;
        }
        const Book& BookC()
        {
            static const Book b = Make(kBookCCodes, kBookCEscape);
            return b;
        }

        // The stream, read least significant bit first; reads past its end see
        // zeros, as in the game.
        class Bits
        {
        public:
            Bits(const uint8_t* data, size_t size) : d(data, data + size), size(size) {}

            int Bit()
            {
                const int r = (Byte(pos >> 3) >> (pos & 7)) & 1;
                pos++;
                return r;
            }
            int Symbol(const Book& b)
            {
                const uint32_t v = Window() & 4095;
                pos += b.length[v];
                int s = b.symbol[v];
                if (s == kEscape)
                {
                    s = int(Window() & ((1u << b.bits) - 1)) - b.bias;
                    pos += size_t(b.bits);
                }
                return s;
            }
            // `count` raw bytes, from a byte boundary as the stream stands.
            std::vector<uint8_t> Raw(size_t count)
            {
                const size_t at = pos >> 3;
                if (at + count > size) throw Error("wavelet picture: picture data is cut short");
                pos += count * 8;
                return { d.begin() + ptrdiff_t(at), d.begin() + ptrdiff_t(at + count) };
            }

        private:
            uint32_t Window() const
            {
                const size_t b = pos >> 3;
                return (uint32_t(Byte(b)) | uint32_t(Byte(b + 1)) << 8 | uint32_t(Byte(b + 2)) << 16) >> (pos & 7);
            }
            uint8_t Byte(size_t at) const { return at < size ? d[at] : 0; }
            std::vector<uint8_t> d;
            size_t size, pos = 0;
        };

        uint8_t Clamp(int v) { return uint8_t(v < 0 ? 0 : v > 255 ? 255 : v); }
    }

    std::vector<std::vector<uint8_t>> DecodeWavelet(const uint8_t* data, size_t size, int width, int height, int channels)
    {
        if (width <= 0 || height <= 0 || (width & (width - 1)) || (height & (height - 1)))
            throw Error("wavelet picture: picture sides aren't powers of two");
        const size_t n = size_t(channels);
        std::vector<std::pair<int, int>> levels{ { width, height } };
        for (int w = width, h = height; w > 1 || h > 1;)
        {
            w = std::max(1, w >> 1);
            h = std::max(1, h >> 1);
            levels.push_back({ w, h });
        }
        Bits in(data, size);
        // Smallest first; each level is built on the one before it, which a
        // level's first bit can say it corrects (the level kept is the
        // corrected one).
        std::vector<std::vector<uint8_t>> out;
        for (auto it = levels.rbegin(); it != levels.rend(); ++it)
        {
            const auto [w, h] = *it;
            if (w <= 1 || h <= 1)
            {
                out.push_back(in.Raw(size_t(w) * size_t(h) * n));
                continue;
            }
            if (out.empty()) throw Error("wavelet picture: picture has no mipmap chain to build on");
            std::vector<uint8_t>& parent = out.back();
            // Coefficients: one continuous bit stream over all the transformed levels.
            const size_t row = size_t(w) * n;
            std::vector<uint8_t> dest(row * size_t(h));
            if (in.Bit())   // this level also corrects the one below it
                for (size_t i = 0; i < size_t(w) * size_t(h) / 4 * n; i++)
                    parent[i] = Clamp(in.Symbol(BookA()) + parent[i]);
            // One channel of one 2x2 block: the parent sample, a parity bit, three details.
            auto lift = [&](size_t o, int p, int parity, int c1, int c2, int c3) {
                p *= 2;
                dest[o] = Clamp(parity + ((p + c1 + c2 + c3) >> 1));
                dest[o + n] = Clamp((p + c1 - c3 - c2) >> 1);
                dest[o + row] = Clamp((p + c2 - c3 - c1) >> 1);
                dest[o + row + n] = Clamp((p + c3 - c2 - c1) >> 1);
            };
            const size_t parentRow = row / 2;
            for (size_t by = 0; by < size_t(h) / 2; by++)
                for (size_t bx = 0; bx < size_t(w) / 2; bx++)
                {
                    const size_t po = by * parentRow + bx * n, o = by * 2 * row + bx * 2 * n;
                    if (n != 1)
                    {
                        const int parity = in.Bit();
                        const int c1 = in.Symbol(BookB()), c2 = in.Symbol(BookB()), c3 = in.Symbol(BookB());
                        lift(o, parent[po], parity, c1, c2, c3);
                        if (n >= 3)
                            for (size_t ch = 1; ch <= 2; ch++)
                            {
                                const int p2 = in.Bit();
                                const int d1 = in.Symbol(BookC()) + c1;
                                const int d2 = in.Symbol(BookC()) + c2;
                                const int d3 = in.Symbol(BookC()) + c3;
                                lift(o + ch, parent[po + ch], p2, d1, d2, d3);
                            }
                    }
                    if (n == 3) continue;
                    const size_t ch = n - 1;
                    const int parity = in.Bit();
                    const int c1 = in.Symbol(BookA()), c2 = in.Symbol(BookA()), c3 = in.Symbol(BookA());
                    lift(o + ch, parent[po + ch], parity, c1, c2, c3);
                }
            out.push_back(std::move(dest));
        }
        return { out.rbegin(), out.rend() };
    }
}
