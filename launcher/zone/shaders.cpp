#include "shaders.h"
#include "rules.h"
#include "walker.h"
#include "vertex_binding.h"
#include "../../runtime/gpu/vulkan/renderer.h"

#include <span>
#include <set>
#include <unordered_map>

namespace zone
{
    namespace
    {
        uint32_t Word(const uint8_t* p) { return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 | uint32_t(p[2]) << 8 | p[3]; }

        // The microcode a shader header describes: the table its seventh word
        // points to holds {offset in the program, bytes}. False if the header
        // says something impossible.
        // `separate`: the program is in a buffer of its own, from its start.
        bool Microcode(const uint8_t* data, size_t size, size_t header, size_t headerEnd, size_t program,
                       size_t programEnd, bool pixel, Shader& shader, const std::vector<uint8_t>* separate = nullptr)
        {
            if (separate)
            {
                program = 0;
                programEnd = separate->size();
            }
            if (header + 28 > headerEnd) return false;
            if ((Word(data + header) & 0xFFFFFF00) != 0x102A1100) return false;
            const size_t table = header + Word(data + header + 24);
            if (table + 8 > headerEnd) return false;
            const uint32_t offset = Word(data + table), length = Word(data + table + 4);
            if (!length || (length & 3) || offset > 4096 || program + offset + size_t(length) > programEnd ||
                (!separate && programEnd > size))
                return false;
            const uint8_t* code = (separate ? separate->data() : data) + program + offset;
            shader.pixel = pixel;
            shader.code.resize(length / 4);
            for (size_t k = 0; k < shader.code.size(); k++) shader.code[k] = Word(code + 4 * k);
            shader.hash = vk::renderer::ShaderHash(shader.code.data(), shader.code.size());
            return true;
        }
    }

    std::vector<Shader> Shaders(const std::vector<uint8_t>& zone, const BindTables& tables, BindCounts* counts)
    {
        const Schema& s = XboxRules();
        auto member = [&](const char* type, const char* name) {
            const MemberInfo* m = s.infos.at(type)->Find(name);
            if (!m) throw Error(std::string("the rules have no ") + type + "::" + name);
            return m;
        };
        const MemberInfo* programs[2] = { member("GfxVertexShaderLoadDef", "physicalPart"),
                                          member("GfxPixelShaderLoadDef", "physicalPart") };
        const MemberInfo* objects[2] = { member("GfxVertexShaderLoadDef", "cachedPart"),
                                         member("GfxPixelShaderLoadDef", "cachedPart") };
        const MemberInfo* passDeclaration = member("MaterialPass", "vertexDecl");
        const MemberInfo* passShaders = member("MaterialPass", "vertexShaderArray");
        const MemberInfo* passShader = member("MaterialPass", "vertexShader");

        std::vector<Shader> out;
        std::set<std::pair<bool, uint64_t>> seen;
        auto add = [&](Shader&& shader) {
            if (seen.insert({ shader.pixel, shader.hash }).second) out.push_back(std::move(shader));
        };
        BindCounts tally;
        // The program streams before its object (the rules' reorder).
        struct Span { size_t at = 0, bytes = 0; bool set = false; } program[2];

        // The pass being loaded: its declaration's 64 bytes (an earlier pass's
        // when it points back to one), and the slot of the vertex shader being
        // loaded fresh -- the title binds a shader only then (sub_821BBE70).
        std::unordered_map<uint32_t, const uint8_t*> declarations;   // by what a pointer to one holds
        const uint8_t* declaration = nullptr;
        int slot = -1;

        Walker walker(s, zone.data(), zone.size());
        walker.onPointer = [&](const MemberInfo& m, int64_t index, uint32_t value) {
            constexpr uint32_t kFollowing = 0xFFFFFFFF;
            if (&m == passDeclaration)
            {
                declaration = nullptr;
                if (value && value != kFollowing)
                {
                    auto found = declarations.find(value);
                    if (found == declarations.end()) throw Error("a material pass points to a vertex declaration not loaded before it");
                    declaration = found->second;
                }
            }
            else if (&m == passShaders) slot = value == kFollowing ? int(index) : -1;
            else if (&m == passShader) slot = value == kFollowing ? 15 : -1;
        };
        walker.onData = [&](const MemberInfo& m, const Place& place, size_t bytes) {
            const size_t at = place.zonePos;
            if (&m == passDeclaration)
            {
                if (bytes != 64 || !place.inZone) throw Error("a vertex declaration is not the 64 bytes it should be");
                declaration = zone.data() + at;
                declarations[place.Pointer()] = declaration;
                return;
            }
            for (int pixel = 0; pixel < 2; pixel++)
            {
                if (&m == programs[pixel])
                {
                    program[pixel] = { at, bytes, true };
                    return;
                }
                if (&m != objects[pixel]) continue;
                if (!program[pixel].set) throw Error("a shader object at " + std::to_string(at) + " has no program");
                const Span p = program[pixel];
                program[pixel].set = false;

                // The object's header is at +40 in a pixel shader's, at +872 in a
                // vertex shader's, where the vertex fetch tables come first.
                Shader shader;
                if (!Microcode(zone.data(), zone.size(), at + (pixel ? 40 : 872), at + bytes, p.at, p.at + p.bytes,
                               pixel != 0, shader))
                    throw Error("the shader object at " + std::to_string(at) + " has no header this can read");
                if (pixel)
                {
                    add(std::move(shader));
                    return;
                }
                // A vertex shader is drawn as its binding leaves it.
                const int bound = slot;
                slot = -1;
                if (bound < 0 || !declaration)
                {
                    tally.unbound++;
                    add(std::move(shader));
                    return;
                }
                const std::vector<uint8_t> object(zone.begin() + ptrdiff_t(at), zone.begin() + ptrdiff_t(at + bytes));
                std::vector<uint8_t> patched(zone.begin() + ptrdiff_t(p.at), zone.begin() + ptrdiff_t(p.at + p.bytes));
                if (!Bind(tables, declaration, unsigned(bound), object, patched))
                {
                    if (bound >= 15) tally.ownShader++;
                    else tally.noDeclaration++;
                    add(std::move(shader));
                    return;
                }
                tally.bound++;
                Shader made;
                if (!Microcode(object.data(), object.size(), 872, object.size(), 0, 0, false, made, &patched))
                    throw Error("a bound vertex shader has no header this can read");
                add(std::move(made));
                return;
            }
        };
        walker.Walk();
        if (counts)
        {
            counts->bound += tally.bound;
            counts->unbound += tally.unbound;
            counts->noDeclaration += tally.noDeclaration;
            counts->ownShader += tally.ownShader;
        }
        return out;
    }
}

namespace
{
    // D3D's shaders: { pixel, address, dwords }, at each site that sends one
    // (sub_820D6C18, 820DA6C8, 820DEC80, 820E0C88, 820E5570, 820E56C8,
    // 820EF308 -- the three of its switch --, 820F0128 and 820F09A8 in the
    // multiplayer; the same D3D, elsewhere, in the campaign).
    struct Internal { bool pixel; uint32_t address, dwords; };
    constexpr Internal kMultiplayer[] = {
        { false, 0x82079AB0, 24 }, { true, 0x820799BC, 9 }, { false, 0x82079A50, 24 },
        { false, 0x820799E0, 27 }, { true, 0x82079998, 9 }, { false, 0x82079554, 15 },
        { false, 0x82078684, 15 }, { false, 0x82079938, 24 }, { true, 0x82077E2C, 9 },
        { true, 0x8207785C, 15 }, { false, 0x82077E50, 525 }, { false, 0x820786C0, 933 },
        { false, 0x82079590, 210 }, { false, 0x82077088, 24 }, { true, 0x820771A8, 429 },
        { false, 0x82077958, 309 }, { true, 0x82076E74, 9 },
    };
    constexpr Internal kCampaign[] = {
        { false, 0x82065EF0, 24 }, { true, 0x82065DFC, 9 }, { false, 0x82065E90, 24 },
        { false, 0x82065E20, 27 }, { true, 0x82065DD8, 9 }, { false, 0x82065994, 15 },
        { false, 0x82064AC4, 15 }, { false, 0x82065D78, 24 }, { true, 0x8206426C, 9 },
        { true, 0x82063C9C, 15 }, { false, 0x82064290, 525 }, { false, 0x82064B00, 933 },
        { false, 0x820659D0, 210 }, { false, 0x820634C8, 24 }, { true, 0x820635E8, 429 },
        { false, 0x82063D98, 309 }, { true, 0x820632B4, 9 },
    };
    constexpr uint32_t kImageBase = 0x82000000;
}

std::vector<zone::Shader> zone::ExecutableShaders(const std::vector<uint8_t>& image, bool multiplayer,
                                                  const BindTables& tables)
{
    std::vector<Shader> out;
    for (const Internal& d : multiplayer ? std::span<const Internal>(kMultiplayer) : std::span<const Internal>(kCampaign))
    {
        const size_t at = d.address - kImageBase;
        if (at + 4 * size_t(d.dwords) > image.size()) throw Error("an executable is shorter than D3D's shaders in it");
        Shader shader;
        shader.pixel = d.pixel;
        shader.code.resize(d.dwords);
        for (size_t k = 0; k < d.dwords; k++) shader.code[k] = Word(image.data() + at + 4 * k);
        shader.hash = vk::renderer::ShaderHash(shader.code.data(), shader.code.size());
        out.push_back(std::move(shader));
    }
    for (size_t at = 0; at + 32 <= image.size(); at += 4)
    {
        if ((Word(image.data() + at) & 0xFFFFFF00) != 0x102A1100) continue;
        // The magic's low bit is set in a vertex shader's: every one in the
        // fastfiles has 0x102A1101 and every pixel shader 0x102A1100. The
        // memory move's vertex shader has 0x102A1111.
        const bool pixel = !(Word(image.data() + at) & 1);
        Shader shader;
        const size_t program = at + Word(image.data() + at + 4);
        if (!Microcode(image.data(), image.size(), at, image.size(), program, image.size(), pixel, shader)) continue;
        out.push_back(std::move(shader));
        if (pixel) continue;
        // The memory move's vertex shader, as its draw binds it.
        const std::vector<uint8_t> object(image.begin() + ptrdiff_t(at), image.begin() + ptrdiff_t(program));
        std::vector<uint8_t> patched(image.begin() + ptrdiff_t(program), image.end());
        const uint8_t strides[16] = { kMoveStride };
        BindPrograms(tables, MoveDeclaration(image, multiplayer), strides, object, 0, patched);
        Shader bound;
        if (!Microcode(object.data(), object.size(), 0, object.size(), 0, 0, false, bound, &patched))
            throw Error("the memory move's vertex shader has no header this can read");
        out.push_back(std::move(bound));
    }
    return out;
}
