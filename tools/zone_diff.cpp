// tools/zone_diff.py's C++ side: the launcher's zone reader as it was in C++
// (launcher/zone/, runtime/install/xex.cpp, compiled from git), run as
// zone_diff.rs runs the Rust that replaced it (zone/src/), each printing the same
// text for the same question, which zone_diff.py compares:
//
//   defs <header>                 every type, laid out, and every constant
//   schema <folder>               the loading rules of a definitions folder, as read
//   walk <zone> [dump]            the walk's events, as a digest (or one per line)
//   fastfile <ff>                 a fastfile's zone, as a digest
//   inflate <ff> <out>            a fastfile's zone, written out (the C++ only)
//   tables <xex> <mp> <out>       an executable's binding tables, written out (the C++ only)
//   executable <xex> <mp>         an executable's tables and shaders
//   shaders <tables> <ff>         a fastfile's shaders, bound with the tables written out
//   mutate <zone> <tables> <seed> the shaders of the zone after the seed's damage
//   expr <file>                   each line's tokens and expression, as both read them
#include "zone/defs.h"
#include "zone/expr.h"
#include "zone/fastfile.h"
#include "zone/rules.h"
#include "zone/schema.h"
#include "zone/shaders.h"
#include "zone/vertex_binding.h"
#include "zone/walker.h"
#include "install/xex.h"

#include <algorithm>
#include <cinttypes>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <map>
#include <set>
#include <string>
#include <vector>

namespace
{
    std::vector<uint8_t> ReadFile(const std::string& path)
    {
        std::ifstream file(path, std::ios::binary);
        if (!file) throw zone::Error("cannot open " + path);
        return std::vector<uint8_t>((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    }

    std::string Text(const std::string& path)
    {
        const std::vector<uint8_t> data = ReadFile(path);
        return std::string(data.begin(), data.end());
    }

    void WriteFile(const std::string& path, const void* data, size_t size)
    {
        std::FILE* f = std::fopen(path.c_str(), "wb");
        if (!f) throw zone::Error("cannot write " + path);
        std::fwrite(data, 1, size, f);
        std::fclose(f);
    }

    // FNV-1a 64, the digests both sides print.
    struct Digest
    {
        uint64_t h = 1469598103934665603ull;
        uint64_t n = 0;
        void Add(const void* data, size_t size)
        {
            const uint8_t* p = static_cast<const uint8_t*>(data);
            for (size_t i = 0; i < size; i++) h = (h ^ p[i]) * 1099511628211ull;
        }
        void Line(const std::string& s)
        {
            Add(s.data(), s.size());
            Add("\n", 1);
            n++;
        }
    };

    // ---- defs ----------------------------------------------------------------------
    const char* const kKinds[] = { "prim", "enum", "struct", "union", "typedef" };

    struct DefsDump
    {
        std::map<const zone::Type*, std::string> labels;
        std::vector<const zone::Type*> anonymous;
        std::string Label(const zone::Type* t)
        {
            if (!t->name.empty()) return t->name;
            auto found = labels.find(t);
            if (found != labels.end()) return found->second;
            const std::string label = "<anon" + std::to_string(anonymous.size()) + ">";
            labels[t] = label;
            anonymous.push_back(t);
            return label;
        }
        void Type(const zone::Type* t, const std::string& label)
        {
            std::printf("type %s kind=%s size=%u align=%u signed=%d float=%d alignas=%u defined=%d parent=%s\n",
                        label.c_str(), kKinds[t->kind], t->size, t->align, t->isSigned, t->isFloat, t->alignas_,
                        t->defined, t->parent ? Label(t->parent).c_str() : "-");
            for (const zone::Member& m : t->members)
            {
                std::string mods;
                for (int x : m.mods) mods += std::to_string(x) + ",";
                std::printf("  member %s type=%s mods=%s bits=%d offset=%u bitpos=%u size=%u align=%u\n", m.name.c_str(),
                            Label(m.type).c_str(), mods.c_str(), m.bits, m.offset, m.bitpos, m.size, m.align);
            }
        }
    };

    void Defs(const std::string& path)
    {
        const zone::Defs defs(Text(path));
        DefsDump d;
        for (const zone::Type* t : defs.named) d.Type(t, d.Label(t));
        for (size_t k = 0; k < d.anonymous.size(); k++) d.Type(d.anonymous[k], d.Label(d.anonymous[k]));
        std::printf("named");
        for (const zone::Type* t : defs.named) std::printf(" %s", d.Label(t).c_str());
        std::printf("\n");
        std::map<std::string, int64_t> consts(defs.consts.begin(), defs.consts.end());
        for (const auto& [name, value] : consts) std::printf("const %s %" PRId64 "\n", name.c_str(), value);
        std::map<std::string, std::string> types;
        for (const auto& [name, t] : defs.types) types[name] = d.Label(t);
        for (const auto& [name, label] : types) std::printf("name %s %s\n", name.c_str(), label.c_str());
    }

    // ---- schema --------------------------------------------------------------------
    std::string MemberLabel(const zone::MemberInfo* m)
    {
        if (!m) return "-";
        std::string name = m->name;
        if (name.empty())
        {
            const auto& list = m->parent->members;
            name = "#" + std::to_string(std::find(list.begin(), list.end(), m) - list.begin());
        }
        return m->parent->name + "::" + name;
    }

    std::string NodeText(const zone::Node* n)
    {
        std::string s = "(" + std::to_string(int(n->op)) + " " + std::to_string(n->value);
        for (const auto& k : n->kids) s += " " + NodeText(k.get());
        return s + ")";
    }

    std::string ExprText(const zone::Expr* e)
    {
        if (!e) return "-";
        std::string s = NodeText(e->root.get());
        for (const zone::ExprRef& r : e->refs)
        {
            s += " [" + std::string(r.isIndex ? "index " : "") + (r.info ? r.info->name : "-") + ":";
            for (const std::string& n : r.names) s += " " + n;
            s += " /";
            for (const zone::MemberInfo* m : r.chain) s += " " + MemberLabel(m);
            s += "]";
        }
        if (e->constant) s += " =" + std::to_string(*e->constant);
        return s;
    }

    std::string BlockText(const zone::Block* b) { return b ? b->name : "-"; }

    void Schema(const std::string& folder)
    {
        const zone::Schema s(Text(folder + "/iw4_assets.h"), Text(folder + "/commands.txt"), [&](const std::string& name) {
            return Text(folder + "/assets/" + name.substr(name.rfind('/') + 1));
        });
        for (const zone::Block& b : s.blocks)
            std::printf("block %s %s %d\n", b.kind.c_str(), b.name.c_str(), b.isDefault);
        std::map<std::string, std::string> assets;
        for (const auto& [name, info] : s.assets) assets[name] = info->name;
        for (const auto& [name, info] : assets) std::printf("asset %s %s\n", name.c_str(), info.c_str());
        std::map<std::string, const zone::StructInfo*> infos(s.infos.begin(), s.infos.end());
        for (const auto& [name, info] : infos)
        {
            std::printf("struct %s as %s ctype=%s block=%s asset=%s allocAlign=%u leaf=%d union=%d dynamic=%s\n",
                        name.c_str(), info->name.c_str(), info->ctype->name.c_str(), BlockText(info->block).c_str(),
                        info->asset.c_str(), info->allocAlign, info->isLeaf, info->isUnion,
                        MemberLabel(info->dynamic).c_str());
            for (const char* which : { "members", "ordered", "used" })
            {
                const auto& list = !std::strcmp(which, "members") ? info->members
                                   : !std::strcmp(which, "ordered") ? info->ordered
                                                                     : info->used;
                std::printf("  %s", which);
                for (const zone::MemberInfo* m : list) std::printf(" %s", MemberLabel(m).c_str());
                std::printf("\n");
            }
            for (const zone::MemberInfo* m : info->members)
            {
                std::printf("  member %s string=%d script=%d reusable=%d block=%s allocAlign=%u type=%s leaf=%d "
                            "treat=%d after=%d ignored=%d\n",
                            MemberLabel(m).c_str(), m->isString, m->isScriptString, m->isReusable,
                            BlockText(m->block).c_str(), m->allocAlign, m->type ? m->type->name.c_str() : "-",
                            m->isLeaf, s.NeedsTreatment(m), s.AfterPartialLoad(m), zone::Schema::Ignored(m));
                std::printf("    condition %s\n", ExprText(m->condition).c_str());
                if (m->blockCond.cond)
                    std::printf("    blockcond %s ? %s : %s\n", ExprText(m->blockCond.cond).c_str(),
                                BlockText(m->blockCond.ifTrue).c_str(), BlockText(m->blockCond.ifFalse).c_str());
                for (const auto& [k, e] : m->counts) std::printf("    count %zu %s\n", k, ExprText(e).c_str());
                for (const auto& [k, by] : m->countsByIndex)
                    for (const auto& [i, e] : by)
                        std::printf("    count %zu [%" PRId64 "] %s\n", k, i, ExprText(e).c_str());
                for (const auto& [k, e] : m->arraySize) std::printf("    arraysize %zu %s\n", k, ExprText(e).c_str());
            }
        }
    }

    // ---- the walk ------------------------------------------------------------------
    void Walk(const std::string& path, bool dump)
    {
        const std::vector<uint8_t> zone = ReadFile(path);
        zone::Walker w(zone::XboxRules(), zone.data(), zone.size());
        Digest d;
        char line[512];
        auto emit = [&](const char* text) {
            if (dump) std::printf("%s\n", text);
            d.Line(text);
        };
        w.onPointer = [&](const zone::MemberInfo& m, int64_t index, uint32_t value) {
            std::snprintf(line, sizeof line, "P %s %" PRId64 " %08X", MemberLabel(&m).c_str(), index, value);
            emit(line);
        };
        w.onData = [&](const zone::MemberInfo& m, const zone::Place& p, size_t bytes) {
            std::snprintf(line, sizeof line, "D %s %d %u %zu %d %zu", MemberLabel(&m).c_str(), p.block, p.blockPos,
                          p.zonePos, p.inZone, bytes);
            emit(line);
        };
        try
        {
            w.Walk();
            std::printf("walked %" PRIu64 " events %016" PRIX64 ", to %zu, %u assets\n", d.n, d.h, w.Position(),
                        w.AssetCount());
        }
        catch (const std::exception& e)
        {
            std::printf("walked %" PRIu64 " events %016" PRIX64 ", error: %s\n", d.n, d.h, e.what());
        }
    }

    // ---- shaders -------------------------------------------------------------------
    void PrintShaders(const std::vector<zone::Shader>& shaders)
    {
        for (const zone::Shader& s : shaders)
        {
            Digest code;
            code.Add(s.code.data(), 4 * s.code.size());
            std::printf("shader %d %016" PRIX64 " %zu %016" PRIX64 "\n", s.pixel, s.hash, s.code.size(), code.h);
        }
        std::printf("%zu shaders\n", shaders.size());
    }

    zone::BindTables ReadTables(const std::string& path)
    {
        const std::vector<uint8_t> data = ReadFile(path);
        zone::BindTables t;
        if (data.size() != sizeof t) throw zone::Error("tables of the wrong size");
        std::memcpy(&t, data.data(), sizeof t);
        return t;
    }

    std::vector<uint8_t> Image(const std::string& path)
    {
        std::vector<uint8_t> image;
        std::string error;
        if (!install::xex::Image(ReadFile(path), image, error)) throw zone::Error(error);
        return image;
    }

    void Executable(const std::string& path, bool multiplayer)
    {
        const std::vector<uint8_t> image = Image(path);
        const zone::BindTables t = zone::ReadBindTables(image, multiplayer);
        std::printf("tables");
        // As the Rust lays them out: types, usages, strides (big-endian), swizzles, formats.
        for (uint8_t b : t.types) std::printf(" %02X", b);
        for (uint8_t b : t.usages) std::printf(" %02X", b);
        for (const auto& s : t.strides)
            for (uint32_t v : s) std::printf(" %08X", v);
        for (uint8_t b : t.swizzles) std::printf(" %02X", b);
        for (uint8_t b : t.formats) std::printf(" %02X", b);
        std::printf("\n");
        const std::vector<uint8_t> move = zone::MoveDeclaration(image, multiplayer);
        std::printf("move");
        for (uint8_t b : move) std::printf(" %02X", b);
        std::printf("\n");
        PrintShaders(zone::ExecutableShaders(image, multiplayer, t));
    }

    void Counts(const zone::BindCounts& c)
    {
        std::printf("counts %u %u %u %u\n", c.bound, c.unbound, c.noDeclaration, c.ownShader);
    }

    void Shaders(const std::string& tables, const std::string& path)
    {
        zone::BindCounts c;
        PrintShaders(zone::Shaders(zone::ReadFastfile(path), ReadTables(tables), &c));
        Counts(c);
    }

    // ---- damage --------------------------------------------------------------------
    // splitmix64, as zone_diff.rs has it.
    struct Random
    {
        uint64_t s;
        uint64_t Next()
        {
            uint64_t z = (s += 0x9E3779B97F4A7C15ull);
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint64_t Below(uint64_t n) { return n ? Next() % n : 0; }
    };

    // The seed's damage to a zone: one to four changes -- a bit flipped, a word or a byte
    // written, the zone cut short -- most in its first 4 KB, where its asset list and
    // first structures are, or its first 64 KB, past its 56 bytes of header; in that
    // header now and then.
    void Damage(std::vector<uint8_t>& zone, uint64_t seed)
    {
        Random r{ seed };
        const uint64_t changes = 1 + r.Below(4);
        for (uint64_t c = 0; c < changes && zone.size() > 64; c++)
        {
            const uint64_t from = r.Below(16) == 0 ? 0 : 56;
            const uint64_t reach[] = { 4096, 65536, zone.size() };
            const uint64_t span = std::min<uint64_t>(reach[r.Below(3)], zone.size() - from - 4);
            const size_t at = size_t(from + r.Below(span));
            switch (r.Below(5))
            {
            case 0: zone[at] ^= uint8_t(1u << r.Below(8)); break;
            case 1: zone[at] = uint8_t(r.Next()); break;
            case 2:
            {
                static const uint32_t kWords[] = { 0, 0xFFFFFFFFu, 0xFFFFFFFEu, 1, 2, 0x7FFFFFFFu, 0x80000000u, 0x10000000u };
                uint32_t v;
                switch (r.Below(3))
                {
                case 0: v = kWords[r.Below(8)]; break;
                case 1: v = uint32_t(r.Below(300)); break;
                default: v = uint32_t(r.Next()); break;
                }
                const size_t w = at & ~size_t(3);
                zone[w] = uint8_t(v >> 24);
                zone[w + 1] = uint8_t(v >> 16);
                zone[w + 2] = uint8_t(v >> 8);
                zone[w + 3] = uint8_t(v);
                break;
            }
            case 3: zone[at] = uint8_t(zone[at] + 1); break;
            default: zone.resize(at); break;
            }
        }
    }

    void Mutate(const std::string& path, const std::string& tables, uint64_t seed)
    {
        std::vector<uint8_t> zone = ReadFile(path);
        Damage(zone, seed);
        Digest z;
        z.Add(zone.data(), zone.size());
        std::printf("zone %zu %016" PRIX64 "\n", zone.size(), z.h);
        std::fflush(stdout);
        try
        {
            zone::BindCounts c;
            const std::vector<zone::Shader> shaders = zone::Shaders(zone, ReadTables(tables), &c);
            Digest all;
            for (const zone::Shader& s : shaders)
            {
                all.Add(&s.hash, 8);
                all.Add(s.code.data(), 4 * s.code.size());
            }
            std::printf("ok %zu %016" PRIX64 "\n", shaders.size(), all.h);
            Counts(c);
        }
        catch (const std::exception& e)
        {
            std::printf("error %s\n", e.what());
        }
    }

    // ---- expressions ---------------------------------------------------------------
    // Each line: its tokens both ways, then its value as a header's expression (towards
    // zero) and a rule's (floor division), its names the constants a..f.
    void Expressions(const std::string& path)
    {
        const std::string text = Text(path);
        const std::map<std::string, int64_t> names = {
            { "a", 3 }, { "b", -7 }, { "c", 0 }, { "d", int64_t(1) << 40 }, { "e", INT64_MAX }, { "f", 1 },
        };
        size_t from = 0;
        while (from < text.size())
        {
            size_t to = text.find('\n', from);
            if (to == std::string::npos) to = text.size();
            const std::string line = text.substr(from, to - from);
            from = to + 1;
            for (bool paths : { false, true })
            {
                std::printf("tokens%d", paths);
                for (const std::string& t : zone::Tokens(line, paths)) std::printf(" «%s»", t.c_str());
                std::printf("\n");
            }
            const std::vector<std::string> toks = zone::Tokens(line, true);
            for (bool floor : { false, true })
            {
                try
                {
                    zone::ExprParser parser(toks, [&](zone::ExprParser& p, size_t& at) {
                        auto found = names.find(p.toks[at]);
                        if (found == names.end()) throw zone::Error("unknown constant '" + p.toks[at] + "'");
                        at++;
                        auto n = std::make_unique<zone::Node>();
                        n->value = found->second;
                        return n;
                    });
                    const auto root = parser.Parse(0, toks.size());
                    std::printf("value%d %s = %" PRId64 "\n", floor, NodeText(root.get()).c_str(), root->Eval({}, floor));
                }
                catch (const std::exception& e)
                {
                    std::printf("value%d error %s\n", floor, e.what());
                }
            }
        }
    }
}

int main(int argc, char** argv)
{
    const std::vector<std::string> a(argv + 1, argv + argc);
    try
    {
        if (a.size() == 2 && a[0] == "defs") Defs(a[1]);
        else if (a.size() == 2 && a[0] == "schema") Schema(a[1]);
        else if (a.size() >= 2 && a[0] == "walk") Walk(a[1], a.size() > 2);
        else if (a.size() == 2 && a[0] == "fastfile")
        {
            try
            {
                const std::vector<uint8_t> zone = zone::ReadFastfile(a[1]);
                Digest d;
                d.Add(zone.data(), zone.size());
                std::printf("zone %zu %016" PRIX64 "\n", zone.size(), d.h);
            }
            catch (const std::exception& e)
            {
                std::printf("error %s\n", e.what());
            }
        }
        else if (a.size() == 3 && a[0] == "inflate")
        {
            const std::vector<uint8_t> zone = zone::ReadFastfile(a[1]);
            WriteFile(a[2], zone.data(), zone.size());
        }
        else if (a.size() == 4 && a[0] == "tables")
        {
            const zone::BindTables t = zone::ReadBindTables(Image(a[1]), a[2] == "1");
            WriteFile(a[3], &t, sizeof t);
        }
        else if (a.size() == 3 && a[0] == "executable") Executable(a[1], a[2] == "1");
        else if (a.size() == 3 && a[0] == "shaders") Shaders(a[1], a[2]);
        else if (a.size() == 4 && a[0] == "mutate") Mutate(a[1], a[2], std::stoull(a[3]));
        else if (a.size() == 2 && a[0] == "expr") Expressions(a[1]);
        else
        {
            std::fprintf(stderr, "usage: see tools/zone_diff.cpp\n");
            return 2;
        }
    }
    catch (const std::exception& e)
    {
        std::printf("error %s\n", e.what());
    }
    return 0;
}
