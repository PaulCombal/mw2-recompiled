#include "vertex_binding.h"
#include "defs.h"

#include <algorithm>
#include <cstring>

namespace zone
{
    namespace
    {
        constexpr uint32_t kImageBase = 0x82000000;

        // Where the tables are: the title's (the same in both executables) and
        // D3D's, read from the code that uses them.
        struct Addresses { uint32_t types, usages, strides0, strides1, swizzles, formats; };
        constexpr Addresses kMultiplayer{ 0x8206F550, 0x8206F534, 0x8244F244, 0x82070700, 0x82079C30, 0x8207D750 };
        constexpr Addresses kCampaign{ 0x8205BA60, 0x8205BA44, 0x8241EFB8, 0x8205D32C, 0x82066070, 0x82069040 };
        // The image pool's memory move: the element list sub_823DDF08 makes its
        // declaration from (the campaign's sub_823A4AC8).
        constexpr struct { uint32_t multiplayer, campaign; } kMoveDeclaration{ 0x8244F348, 0x8241F0B8 };

        uint32_t Get32(const uint8_t* p) { return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 | uint32_t(p[2]) << 8 | p[3]; }
        uint16_t Get16(const uint8_t* p) { return uint16_t(p[0] << 8 | p[1]); }
        void Put32(uint8_t* p, uint32_t v) { p[0] = uint8_t(v >> 24); p[1] = uint8_t(v >> 16); p[2] = uint8_t(v >> 8); p[3] = uint8_t(v); }
        void Put16(uint8_t* p, uint16_t v) { p[0] = uint8_t(v >> 8); p[1] = uint8_t(v); }

        // The PowerPC's rotate-and-mask, with its bit numbering (0 the highest).
        uint32_t Rotl(uint32_t x, int n) { n &= 31; return n ? (x << n) | (x >> (32 - n)) : x; }
        uint32_t Mask(int mb, int me)
        {
            uint32_t m = 0;
            for (int i = mb;; i = (i + 1) & 31)
            {
                m |= 0x80000000u >> i;
                if (i == me) break;
            }
            return m;
        }
        uint32_t Rlwinm(uint32_t x, int sh, int mb, int me) { return Rotl(x, sh) & Mask(mb, me); }
        uint32_t Rlwimi(uint32_t dst, uint32_t src, int sh, int mb, int me)
        {
            const uint32_t m = Mask(mb, me);
            return (Rotl(src, sh) & m) | (dst & ~m);
        }

        // D3D's declaration object (CreateVertexDeclaration, sub_820D7AB8):
        // 52 bytes of header -- the element count at 24, the highest stream at
        // 28, the streams used at 32 -- then the elements as they were given.
        std::vector<uint8_t> Created(const std::vector<std::array<uint8_t, 12>>& elements)
        {
            std::vector<uint8_t> object(52 + 12 * elements.size());
            uint32_t highest = 0;
            for (const auto& e : elements)
            {
                const uint32_t stream = Get16(e.data());
                highest = std::max(highest, stream);
                if (stream < 16) object[32 + stream] = 255;
            }
            Put32(object.data() + 24, uint32_t(elements.size()));
            Put32(object.data() + 28, highest);
            for (size_t k = 0; k < elements.size(); k++) std::memcpy(object.data() + 52 + 12 * k, elements[k].data(), 12);
            return object;
        }

        // The pass's routing as one declaration type's D3D vertex elements
        // (sub_823EA570), made into D3D's declaration object
        // (CreateVertexDeclaration, sub_820D7AB8): { 52 bytes of header, the
        // elements }. Empty when a source has no stream in this type.
        std::vector<uint8_t> Declaration(const BindTables& t, const uint8_t* decl, unsigned type)
        {
            const uint8_t* types = t.types.data() + 72 * type;
            const unsigned routes = decl[0];
            std::vector<std::array<uint8_t, 12>> elements;
            for (unsigned k = 0; k < routes; k++)
            {
                const uint8_t source = decl[4 + 2 * k], dest = decl[5 + 2 * k];
                const uint8_t* from = types + 8 * source;
                const uint16_t stream = Get16(from);
                if (stream == 255) return {};
                if (2 * size_t(dest) + 1 >= t.usages.size()) throw Error("a vertex routing names a destination past the table");
                std::array<uint8_t, 12> e{};
                Put16(e.data(), stream);
                Put16(e.data() + 2, Get16(from + 2));
                Put32(e.data() + 4, Get32(from + 4));
                e[8] = 0;   // method
                e[9] = t.usages[2 * dest];
                e[10] = t.usages[2 * dest + 1];
                // Kept in stream order, the earlier first among equals.
                size_t at = elements.size();
                while (at > 0 && Get16(elements[at - 1].data()) > stream) at--;
                elements.insert(elements.begin() + ptrdiff_t(at), e);
            }
            return Created(elements);
        }

        // The core of the binding (sub_820D5220 in the multiplayer), for one of
        // the shader's programs: `code` its microcode, `copies` the fetch
        // instructions InitVertexShader kept, `table` the program's table in the
        // shader object. Register names follow the code.
        void Patch(const BindTables& t, const uint8_t* table, const uint8_t* copies, const std::vector<uint8_t>& decl,
                   const uint8_t* strides, uint8_t* code, size_t codeBytes)
        {
            const uint32_t count = Get32(table + 28);   // r27
            if (!count) return;
            const uint8_t* list = table + (Get32(table + 24) + 9) * 4;   // r30: { usage, index, group, instruction }
            const uint32_t elementCount = Get32(decl.data() + 24);       // r24
            const uint8_t* elements = decl.data() + 52;                  // r23
            auto swizzle = [&](uint32_t at) { return uint32_t(Get16(t.swizzles.data() + at)); };

            std::vector<uint8_t> built(12 * size_t(count));   // the stack buffer at r1+96
            // The low bits of r10 start as whatever a stack slot holds; only bit
            // 0 survives to the result, and every swizzle the and with keeps
            // has it clear.
            uint32_t r10 = 0;
            for (uint32_t k = 0; k < count; k++)
            {
                uint8_t* r7 = built.data() + 12 * k;
                uint32_t r11 = Get32(copies + 12 * k), r3 = Get32(copies + 12 * k + 4), r31 = Get32(copies + 12 * k + 8);
                uint32_t r9 = 0, r8;
                const uint8_t* element = nullptr;
                if (elementCount)
                {
                    const uint32_t r5 = Get32(list + 4 * k);
                    const uint32_t usage = Rlwinm(r5, 20, 28, 31), index = Rlwinm(r5, 16, 28, 31);
                    for (; r9 < elementCount; r9++)
                    {
                        const uint8_t* e = elements + 12 * r9;
                        if (e[9] == usage && e[10] == index)
                        {
                            element = e;
                            break;
                        }
                    }
                }
                if (!element)
                {
                    // Nothing in the declaration for it: fetch constant 95.
                    const uint32_t r12 = 0xBFC6CFFF;
                    r8 = strides[0];
                    r11 = Rlwimi(r11, 95, 20, 2, 11);
                    r10 &= 0xF;
                    const uint32_t r5 = r3 & r12;
                    const uint32_t r4 = r31 & 0x80000000;
                    r9 = r11;
                    r10 |= 37456;
                    r11 = r5 | 0x00060000;
                    r8 = r4 | r8;
                }
                else
                {
                    r9 = Get32(element + 4);   // the element's format
                    r10 &= 0xF;
                    uint32_t r4 = Rlwinm(r9, 16, 29, 31);
                    uint32_t r5 = Rlwinm(r9, 22, 26, 28);
                    const uint32_t r21 = Rlwinm(r9, 13, 29, 31);
                    r5 |= r4;
                    r4 = Rlwinm(r9, 0, 16, 21);
                    r5 = Rlwinm(r5, 3, 0, 28);
                    const uint32_t r20 = Rlwinm(r9, 0, 24, 25);
                    r5 |= r21;
                    r5 = Rlwinm(r5, 1, 0, 30);
                    r5 |= r4;
                    r5 = Rlwinm(r5, 3, 0, 28);
                    r10 = r5 | r10;
                    r10 &= 0xFFFF;
                    if (r20 == 64)
                    {
                        if ((r10 & 0xE000) <= 24576) r10 = ((r10 & 0xE000) ^ 0x2000) | (r10 & 0x1FFF);
                        if ((r10 & 0x1C00) <= 3072) r10 = ((r10 & 0x1C00) ^ 0x400) | (r10 & 58367);
                        if ((r10 & 0x380) <= 384) r10 = ((r10 & 0x380) ^ 0x80) | (r10 & 64639);
                        if ((r10 & 0x70) <= 48) r10 = ((r10 & 0x70) ^ 0x10) | (r10 & 65423);
                    }
                    const uint32_t stream = Get16(element), offset = Get16(element + 2);
                    uint32_t r4b = 0x01560056;
                    const uint32_t r21b = Rlwinm(r9, 12, 14, 19);
                    uint32_t r20b = 95 - stream;
                    r9 = Rlwinm(r9, 0, 22, 23);
                    r4b = r20b * r4b;
                    const uint32_t r5b = strides[stream];
                    r20b = r4b;
                    const uint32_t r12 = 0xBFC0CFFF;
                    r20b = Rlwimi(r20b, r4b, 11, 8, 12);
                    r8 = Rlwinm(offset, 6, 1, 23);
                    r9 = r21b | r9;
                    r11 = Rlwimi(r11, r20b, 1, 5, 11);
                    r8 |= r5b;
                    const uint32_t r4c = Rlwinm(r9, 4, 0, 27);
                    r3 &= r12;
                    const uint32_t r5c = r31 & 0x80000000;
                    r9 = Rlwinm(r11, 0, 5, 1);
                    r11 = r4c | r3;
                    r8 |= r5c;
                }

                // loc_820D544C: the destination swizzle, a component at a time.
                uint32_t r5 = Rlwinm(r11, 1, 28, 30);
                Put32(r7, r9);
                r10 &= 0xFFFF;
                Put32(r7 + 8, r8);
                r10 |= 14;
                r9 = swizzle(r5);
                uint32_t r8b = r10 & 0xFFFF;
                r5 = r10 & 0xFFFF;
                r9 &= r8b;
                r8b = r10 & 0xFFFF;
                uint32_t r4 = r9 & 0xFFFF;
                r9 = Rlwinm(r9, 29, 19, 31);
                const uint32_t r3b = r10 & 0xFFFF;
                r9 |= r4;
                r9 = Rlwinm(r9, 29, 3, 31);
                r9 |= r4;
                r9 = Rlwinm(r9, 29, 3, 31);
                r9 |= r4;
                r9 = Rlwinm(r9, 29, 3, 31);
                r9 |= r4;
                r11 = Rlwimi(r11, r9, 31, 29, 31);
                r9 = Rlwinm(r11, 30, 28, 30);
                r9 = swizzle(r9);
                r9 &= r5;
                r5 = r9 & 0xFFFF;
                r4 = Rlwinm(r9, 29, 19, 31);
                r9 = Rlwinm(r9, 2, 14, 29);
                r4 |= r5;
                r4 = Rlwinm(r4, 29, 3, 31);
                r4 |= r5;
                r4 = Rlwinm(r4, 29, 3, 31);
                r5 = r4 | r5;
                r5 = Rlwinm(r5, 31, 1, 31);
                r9 = r5 | r9;
                r9 = Rlwimi(r9, r11, 0, 29, 25);
                r11 = Rlwinm(r9, 27, 28, 30);
                r11 = swizzle(r11);
                r11 &= r8b;
                r8b = r11 & 0xFFFF;
                r5 = Rlwinm(r11, 29, 19, 31);
                r11 = Rlwinm(r11, 3, 13, 28);
                r5 |= r8b;
                r11 |= r8b;
                r5 = Rlwinm(r5, 29, 3, 31);
                r11 = Rlwinm(r11, 2, 0, 29);
                r8b = r5 | r8b;
                r8b = Rlwinm(r8b, 31, 1, 31);
                r11 = r8b | r11;
                r11 = Rlwimi(r11, r9, 0, 26, 22);
                r9 = Rlwinm(r11, 24, 28, 30);
                r9 = swizzle(r9);
                r9 &= r3b;
                r8b = r9 & 0xFFFF;
                r5 = Rlwinm(r9, 3, 13, 28);
                r9 = Rlwinm(r9, 29, 19, 31);
                r5 |= r8b;
                r9 |= r8b;
                r5 = Rlwinm(r5, 3, 0, 28);
                r9 = Rlwinm(r9, 31, 1, 31);
                r8b = r5 | r8b;
                r8b = Rlwinm(r8b, 2, 0, 29);
                r9 = r8b | r9;
                r9 = Rlwimi(r9, r11, 0, 23, 19);
                Put32(r7 + 4, r9);
            }

            // loc_820D555C: within each group of fetches the list marks, sorted
            // by fetch constant, then offset, then size; then a fetch that reads
            // the same constant just past the one before becomes a mini-fetch of it.
            auto key = [](uint32_t w) { return Rlwinm(w, 14, 25, 29) | Rlwinm(w, 7, 30, 31); };
            auto word = [&](size_t i, int w) { return Get32(built.data() + 12 * i + 4 * w); };
            auto marked = [&](size_t i) { return (Get32(list + 4 * i) & 0x00300000) != 0; };
            size_t r27 = 0;
            while (r27 < count)
            {
                while (r27 < count && !marked(r27)) r27++;
                if (r27 >= count) break;
                size_t r11 = r27 + 1;
                const size_t r10s = r27;
                if (r11 >= count) break;
                while (r11 < count && !marked(r11)) r11++;
                if (r11 >= count) break;
                const size_t start = r10s;
                const uint32_t length = uint32_t(r11 - r10s);   // r10
                const uint32_t r5 = length + 1;
                uint32_t r9 = length, r4 = r5;
                int32_t r29 = -1;
                r27 = r11 + 1;
                if ((Get32(list + 4 * r11) & 0x00300000) == 0x00300000)
                {
                    r29 = int32_t(key(word(r11, 0)));
                    r9 = length - 1;
                    r4 = length;
                }
                // The sort: a pass per position, as the code makes it.
                for (uint32_t r31 = 1; r9 != 0; r9--, r31++)
                {
                    if (r31 >= r4) continue;
                    const size_t a = start + (r31 - 1);
                    for (uint32_t r28 = r4 - r31, b = uint32_t(a + 1); r28 != 0; r28--, b++)
                    {
                        const int32_t ka = int32_t(key(word(a, 0))), kb = int32_t(key(word(b, 0)));
                        int32_t r8 = kb - ka;
                        bool swap;
                        if (r8 != 0)
                        {
                            if (r29 == ka) swap = true;
                            else
                            {
                                if (r29 == kb) r8 = -1;
                                swap = r8 > 0;
                            }
                        }
                        else
                        {
                            const int32_t oa = int32_t(Rlwinm(word(a, 2), 1, 0, 30)) >> 9;
                            const int32_t ob = int32_t(Rlwinm(word(b, 2), 1, 0, 30)) >> 9;
                            r8 = oa - ob;
                            if (r8 == 0)
                                r8 = int32_t(t.formats[Get16(built.data() + 12 * a + 4) & 0x3F]) -
                                     int32_t(t.formats[Get16(built.data() + 12 * b + 4) & 0x3F]);
                            swap = r8 > 0;
                        }
                        if (swap)
                        {
                            uint8_t tmp[12];
                            std::memcpy(tmp, built.data() + 12 * a, 12);
                            std::memcpy(built.data() + 12 * a, built.data() + 12 * b, 12);
                            std::memcpy(built.data() + 12 * b, tmp, 12);
                        }
                    }
                }
                // loc_820D5730: the mini-fetches.
                size_t r7 = start;
                for (size_t i = start + 1; i < start + r5; i++)
                {
                    const uint32_t w10 = word(i, 0), w8 = word(r7, 0);
                    const uint32_t c10 = Rlwimi(w10, w10, 25, 12, 13) & Mask(7, 13);
                    const uint32_t c8 = Rlwimi(w8, w8, 25, 12, 13) & Mask(7, 13);
                    bool merge = c8 == c10;
                    uint32_t r10 = 0, r9b = 0;
                    if (merge)
                    {
                        r10 = uint32_t(int32_t(Rlwinm(word(r7, 2), 1, 0, 30)) >> 9);
                        r9b = uint32_t(int32_t(Rlwinm(word(i, 2), 1, 0, 30)) >> 9);
                        merge = r10 <= r9b;
                    }
                    if (merge)
                    {
                        const uint32_t size = t.formats[Get16(built.data() + 12 * i + 4) & 0x3F];
                        r10 = size - r10 + r9b;
                        merge = r10 <= 8;
                    }
                    if (merge)
                    {
                        const uint32_t r9c = Rlwinm(w8, 5, 29, 31);
                        r10 -= 1;
                        if (r9c > r10) r10 = r9c;
                        Put32(built.data() + 12 * r7, Rlwimi(w8, r10, 27, 2, 4));
                        Put32(built.data() + 12 * i + 4, word(i, 1) | 0x40000000);
                    }
                    else r7 = i;
                }
            }

            // loc_820D5800: back over the fetches in the microcode.
            for (uint32_t k = 0; k < count; k++)
            {
                const size_t at = size_t(Get32(list + 4 * k) & 0xFFF) * 12;
                if (at + 12 > codeBytes) throw Error("a vertex fetch is past its shader's microcode");
                std::memcpy(code + at, built.data() + 12 * k, 12);
            }
        }
    }

    BindTables ReadBindTables(const std::vector<uint8_t>& image, bool multiplayer)
    {
        const Addresses& a = multiplayer ? kMultiplayer : kCampaign;
        auto at = [&](uint32_t address, size_t bytes) {
            if (address < kImageBase || address - kImageBase + bytes > image.size())
                throw Error("an executable is shorter than the tables vertex binding reads");
            return image.data() + (address - kImageBase);
        };
        BindTables t;
        std::memcpy(t.types.data(), at(a.types, t.types.size()), t.types.size());
        std::memcpy(t.usages.data(), at(a.usages, t.usages.size()), t.usages.size());
        for (int s = 0; s < 2; s++)
        {
            const uint8_t* p = at(s ? a.strides1 : a.strides0, 60);
            for (int k = 0; k < 15; k++) t.strides[s][size_t(k)] = Get32(p + 4 * k);
        }
        std::memcpy(t.swizzles.data(), at(a.swizzles, t.swizzles.size()), t.swizzles.size());
        std::memcpy(t.formats.data(), at(a.formats, t.formats.size()), t.formats.size());
        return t;
    }

    bool Bind(const BindTables& t, const uint8_t* declaration, unsigned slot, const std::vector<uint8_t>& object,
              std::vector<uint8_t>& program)
    {
        // sub_823EA6D0 makes declarations for the 15 types; the pass's own
        // vertex shader comes as a 16th, whose declaration the title reads
        // from past the 15.
        if (slot >= 15) return false;
        const std::vector<uint8_t> decl = Declaration(t, declaration, slot);
        if (decl.empty()) return false;

        // Bind (sub_820D6A90): the strides as dwords, a byte per stream.
        uint8_t strides[16]{};
        const uint32_t highest = Get32(decl.data() + 28);
        if (highest > 1) throw Error("a vertex declaration has a stream past the two the title gives strides for");
        for (uint32_t s = 0; s <= highest; s++) strides[s] = uint8_t(Rlwinm(t.strides[s][slot], 30, 24, 31));

        BindPrograms(t, decl, strides, object, 872, program);
        return true;
    }

    std::vector<uint8_t> MoveDeclaration(const std::vector<uint8_t>& image, bool multiplayer)
    {
        const uint32_t address = multiplayer ? kMoveDeclaration.multiplayer : kMoveDeclaration.campaign;
        std::vector<std::array<uint8_t, 12>> elements;
        for (size_t at = address - kImageBase;; at += 12)
        {
            if (at + 12 > image.size()) throw Error("an executable is shorter than the memory move's declaration in it");
            if (Get16(image.data() + at) == 255) break;
            std::array<uint8_t, 12> e{};
            std::memcpy(e.data(), image.data() + at, 12);
            elements.push_back(e);
        }
        return Created(elements);
    }

    void BindPrograms(const BindTables& t, const std::vector<uint8_t>& decl, const uint8_t* strides,
                      const std::vector<uint8_t>& object, size_t header, std::vector<uint8_t>& program)
    {
        if (header + 40 > object.size()) throw Error("a vertex shader's header is outside it");
        // Two programs when the header's 0x20 is set, each with its table at the
        // header + the header's word at 24 + 8 * program.
        const unsigned programs = (Get32(object.data() + header) & 0x20) ? 2 : 1;
        for (unsigned v = 0; v < programs; v++)
        {
            const size_t table = header + Get32(object.data() + header + 24 + 8 * v);
            if (table + 40 > object.size()) throw Error("a vertex shader object's table is outside it");
            const uint8_t* tab = object.data() + table;
            const uint32_t count = Get32(tab + 28);
            const size_t list = table + (Get32(tab + 24) + 9) * 4;
            if (list + 4 * size_t(count) > object.size()) throw Error("a vertex shader's fetch list is outside it");
            const size_t microcode = Get32(tab);   // its offset in the program
            if (microcode > program.size()) throw Error("a vertex shader's microcode is outside its program");
            // InitVertexShader (sub_820D7588) keeps a copy of each fetch.
            std::vector<uint8_t> copies(12 * size_t(count));
            for (uint32_t k = 0; k < count; k++)
            {
                const size_t at = microcode + size_t(Get32(object.data() + list + 4 * k) & 0xFFF) * 12;
                if (at + 12 > program.size()) throw Error("a vertex fetch is past its shader's program");
                std::memcpy(copies.data() + 12 * k, program.data() + at, 12);
            }
            Patch(t, tab, copies.data(), decl, strides, program.data() + microcode, program.size() - microcode);
        }
    }
}
