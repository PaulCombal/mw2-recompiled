// The shader rig's program: the C++ translator (shader_translator.cpp) and the
// Rust one (src/runtime/gpu/shader, through shader_translator_rust.cpp and its
// C interface) linked into one program, each program run through both,
// unscaled and scaled, and everything they return compared. shader_diff.py
// builds and drives it.
//
// This file is compiled twice. Once plain: the C++ translator's side, the
// comparison and the drivers. Once with RUST_SIDE, -Dshader=shader_rust and
// MW2_RUST_RENDERER=1: shader_translator_rust.cpp included under a namespace
// of its own, so both shader::Translate definitions live in one program and
// the forwarder the game links is the one tested.
//
//   shader_diff corpus <list>                  every file of the list
//   shader_diff fuzz <list> <seed> <cases>     mutated and random microcode
//   shader_diff dump <list> <dir> <every>      the Rust SPIR-V of every n-th file
//   shader_diff bench <list> cpp|rust <rounds> translation time, one thread
//   shader_diff bindings                       bindings.h, then bindings.rs
//   shader_diff layout                         the forwarder's Meta, then TranslationMeta
//   shader_diff module <seed> <scripts>        the SPIR-V writers alone, call for call
//   shader_diff describe <seed> <cases>        Describe of any values, truncation included
//
// A list holds one .ucode path a line; the name says pixel or vertex. Exits 1
// at the first difference, printing the case.
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <utility>
#include <vector>

struct Flat
{
    std::string error, describe;
    std::vector<uint32_t> spirv;
    // Every other field of the Translation, by name, widened.
    std::vector<std::pair<const char*, std::vector<uint64_t>>> fields;
};

template <class T>
static void Flatten(const T& t, std::string describe, Flat& out)
{
    out.error = t.error;
    out.describe = std::move(describe);
    out.spirv = t.spirv;
    out.fields.clear();
    auto one = [&](const char* name, uint64_t v) { out.fields.push_back({ name, { v } }); };
    auto many = [&](const char* name, const auto& array) {
        std::vector<uint64_t> v;
        for (const auto& x : array) v.push_back(uint64_t(x));
        out.fields.push_back({ name, v });
    };
    one("ok", t.ok);
    one("interpolatorMask", t.interpolatorMask);
    one("textureMask", t.textureMask);
    many("textureKinds", t.textureKinds);
    one("vertexFetchMask", t.vertexFetchMask);
    many("vertexFetchSubs", t.vertexFetchSubs);
    many("vertexFetchPairs", t.vertexFetchPairs);
    many("vertexFetchStride", t.vertexFetchStride);
    many("vertexFetchTail", t.vertexFetchTail);
    many("fetchPairsRead", t.fetchPairsRead);
    one("constantsRead", t.constantsRead);
    one("addressedConstants", t.addressedConstants);
    one("colourMask", t.colourMask);
    one("writesDepth", t.writesDepth);
    one("writesMemory", t.writesMemory);
    one("instructionCount", t.instructionCount);
    one("droppedExports", t.droppedExports);
    one("droppedMemoryExports", t.droppedMemoryExports);
}

// What Describe reads, for the describe mode: any values at all, the error
// any bytes (NULs and bytes above 127 too), so its truncation into 320 and
// 48 bytes is reached, which no translation reaches.
struct DescribeInput
{
    bool ok = false, writesDepth = false, writesMemory = false;
    std::string error;
    size_t spirvWords = 0;
    uint32_t instructionCount = 0, interpolatorMask = 0, colourMask = 0, textureMask = 0, vertexFetchMask = 0,
             droppedExports = 0, droppedMemoryExports = 0;
};

template <class T>
static T MakeTranslation(const DescribeInput& in)
{
    T t;
    t.ok = in.ok;
    t.error = in.error;
    t.spirv.resize(in.spirvWords);
    t.instructionCount = in.instructionCount;
    t.interpolatorMask = in.interpolatorMask;
    t.colourMask = in.colourMask;
    t.textureMask = in.textureMask;
    t.vertexFetchMask = in.vertexFetchMask;
    t.droppedExports = in.droppedExports;
    t.droppedMemoryExports = in.droppedMemoryExports;
    t.writesDepth = in.writesDepth;
    t.writesMemory = in.writesMemory;
    return t;
}

#ifdef RUST_SIDE

#include "gpu/vulkan/shader_translator_rust.cpp"

std::string RustDescribe(const DescribeInput& in)
{
    return shader::Describe(MakeTranslation<shader::Translation>(in));
}

void RustTranslate(bool pixel, const uint32_t* words, size_t count, bool scaled, Flat& out)
{
    auto r = shader::Translate(pixel ? shader::Type::Pixel : shader::Type::Vertex, words, count, scaled);
    Flatten(r, shader::Describe(r), out);
}

void RustTranslateOnly(bool pixel, const uint32_t* words, size_t count, bool scaled, size_t& spirvWords)
{
    auto r = shader::Translate(pixel ? shader::Type::Pixel : shader::Type::Vertex, words, count, scaled);
    spirvWords += r.spirv.size();
}

// The forwarder's Meta as this compiler lays it out: `name offset size` a
// field, then the whole, for the layout mode to hold against TranslationMeta.
void ForwarderLayout()
{
#define F(name) std::printf("%s %zu %zu\n", #name, offsetof(Meta, name), sizeof(Meta::name));
    F(interpolatorMask) F(textureMask) F(textureKinds) F(vertexFetchMask) F(vertexFetchSubs)
    F(vertexFetchPairs) F(vertexFetchStride) F(vertexFetchTail) F(fetchPairsRead) F(constantsRead)
    F(addressedConstants) F(colourMask) F(writesDepth) F(writesMemory) F(instructionCount)
    F(droppedExports) F(droppedMemoryExports)
#undef F
    std::printf("size %zu %zu\n", sizeof(Meta), alignof(Meta));
}

#else

#include "gpu/vulkan/bindings.h"
#include "gpu/vulkan/shader_translator.h"
#include "gpu/vulkan/spirv.h"

void RustTranslate(bool pixel, const uint32_t* words, size_t count, bool scaled, Flat& out);
void RustTranslateOnly(bool pixel, const uint32_t* words, size_t count, bool scaled, size_t& spirvWords);
std::string RustDescribe(const DescribeInput& in);
void ForwarderLayout();
extern "C" void mw2_shader_meta_layout(void* context, void (*sink)(void*, const char*, size_t));
extern "C" void mw2_shader_bindings(void* context, void (*sink)(void*, const char*, size_t));
extern "C" void mw2_spirv_script(const uint32_t* ops, size_t count, void* context,
                                 void (*sink)(void*, const uint32_t*, size_t));

namespace
{
    struct Program
    {
        std::string path;
        bool pixel = false;
        std::vector<uint32_t> words;
    };

    std::vector<Program> LoadList(const char* list)
    {
        std::vector<Program> programs;
        std::FILE* f = std::fopen(list, "r");
        if (!f) { std::perror(list); std::exit(2); }
        char line[4096];
        while (std::fgets(line, sizeof line, f))
        {
            size_t n = std::strlen(line);
            while (n && (line[n - 1] == '\n' || line[n - 1] == '\r')) line[--n] = 0;
            if (!n) continue;
            Program p;
            p.path = line;
            const size_t slash = p.path.rfind('/');
            const std::string name = slash == std::string::npos ? p.path : p.path.substr(slash + 1);
            p.pixel = name.find("pixel") != std::string::npos;
            std::FILE* in = std::fopen(line, "rb");
            if (!in) { std::perror(line); std::exit(2); }
            std::vector<unsigned char> bytes;
            unsigned char buffer[65536];
            size_t got;
            while ((got = std::fread(buffer, 1, sizeof buffer, in)) > 0) bytes.insert(bytes.end(), buffer, buffer + got);
            std::fclose(in);
            // Written big-endian, the way the guest holds it.
            for (size_t i = 0; i + 4 <= bytes.size(); i += 4)
                p.words.push_back(uint32_t(bytes[i]) << 24 | uint32_t(bytes[i + 1]) << 16 |
                                  uint32_t(bytes[i + 2]) << 8 | uint32_t(bytes[i + 3]));
            programs.push_back(std::move(p));
        }
        std::fclose(f);
        return programs;
    }

    void CppTranslate(bool pixel, const uint32_t* words, size_t count, bool scaled, Flat& out)
    {
        auto r = shader::Translate(pixel ? shader::Type::Pixel : shader::Type::Vertex, words, count, scaled);
        Flatten(r, shader::Describe(r), out);
    }

    void PrintCase(const std::string& what, bool pixel, bool scaled, const std::vector<uint32_t>& words)
    {
        std::printf("case: %s, %s, %s, %zu words:\n", what.c_str(), pixel ? "pixel" : "vertex",
                    scaled ? "scaled" : "unscaled", words.size());
        for (size_t i = 0; i < words.size(); i++)
            std::printf("%08X%s", words[i], (i % 8 == 7 || i + 1 == words.size()) ? "\n" : " ");
    }

    // Empty when the two agree, else what differs.
    std::string Difference(const Flat& c, const Flat& r)
    {
        char text[512];
        for (size_t i = 0; i < c.fields.size() && i < r.fields.size(); i++)
        {
            const auto& a = c.fields[i];
            const auto& b = r.fields[i];
            if (a.second == b.second) continue;
            for (size_t j = 0; j < a.second.size() && j < b.second.size(); j++)
                if (a.second[j] != b.second[j])
                {
                    std::snprintf(text, sizeof text, "%s[%zu]: C++ %llu, Rust %llu", a.first, j,
                                  (unsigned long long)a.second[j], (unsigned long long)b.second[j]);
                    return text;
                }
            std::snprintf(text, sizeof text, "%s: lengths %zu and %zu", a.first, a.second.size(), b.second.size());
            return text;
        }
        if (c.error != r.error) return "error: C++ \"" + c.error + "\", Rust \"" + r.error + "\"";
        if (c.spirv != r.spirv)
        {
            size_t j = 0;
            while (j < c.spirv.size() && j < r.spirv.size() && c.spirv[j] == r.spirv[j]) j++;
            std::string s;
            std::snprintf(text, sizeof text, "SPIR-V: %zu and %zu words, first difference at word %zu\n",
                          c.spirv.size(), r.spirv.size(), j);
            s = text;
            const size_t from = j > 8 ? j - 8 : 0;
            for (int side = 0; side < 2; side++)
            {
                const auto& v = side ? r.spirv : c.spirv;
                s += side ? "  Rust:" : "  C++: ";
                for (size_t k = from; k < v.size() && k < j + 8; k++)
                {
                    std::snprintf(text, sizeof text, " %s%08X", k == j ? "*" : "", v[k]);
                    s += text;
                }
                s += "\n";
            }
            return s;
        }
        if (c.describe != r.describe) return "describe: C++ \"" + c.describe + "\", Rust \"" + r.describe + "\"";
        return {};
    }

    struct Tally
    {
        uint64_t cases = 0, translations = 0, ok = 0, failed = 0, spirvWords = 0;
    };

    // Both sides, unscaled and scaled. False at a difference, after printing it.
    bool Check(const std::string& what, bool pixel, const std::vector<uint32_t>& words, Tally& tally)
    {
        tally.cases++;
        for (int scaled = 0; scaled < 2; scaled++)
        {
            Flat c, r;
            CppTranslate(pixel, words.data(), words.size(), scaled, c);
            RustTranslate(pixel, words.data(), words.size(), scaled, r);
            tally.translations++;
            if (c.fields[0].second[0]) { tally.ok++; tally.spirvWords += c.spirv.size(); }
            else tally.failed++;
            const std::string d = Difference(c, r);
            if (!d.empty())
            {
                std::printf("DIFFERENCE: %s\n", d.c_str());
                PrintCase(what, pixel, scaled, words);
                return false;
            }
        }
        return true;
    }

    // splitmix64: a stream from a seed, the same on every machine.
    struct Random
    {
        uint64_t state;
        uint64_t Next()
        {
            uint64_t z = (state += 0x9E3779B97F4A7C15ull);
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint32_t Word() { return uint32_t(Next() >> 32); }
        uint32_t Below(uint32_t n) { return n ? uint32_t((Next() >> 33) % n) : 0; }
        bool Chance(uint32_t percent) { return Below(100) < percent; }
    };

    uint32_t SetBits(uint32_t word, uint32_t lo, uint32_t count, uint32_t value)
    {
        const uint32_t mask = (count == 32 ? ~0u : ((1u << count) - 1u)) << lo;
        return (word & ~mask) | ((value << lo) & mask);
    }

    struct Cf { uint32_t word0 = 0, word1 = 0; };

    void PackCf(const Cf& a, const Cf& b, std::vector<uint32_t>& out)
    {
        out.push_back(a.word0);
        out.push_back((a.word1 & 0xFFFF) | (b.word0 << 16));
        out.push_back((b.word0 >> 16) | (b.word1 << 16));
    }

    // An ALU instruction with every opcode and addressing mode in reach.
    void RandomAlu(Random& rng, uint32_t out[3])
    {
        static const uint32_t dests[] = { 0, 1, 2, 3, 4, 5, 15, 16, 31, 32, 33, 35, 37, 38, 60, 61, 62, 63 };
        uint32_t w0 = rng.Word(), w1 = rng.Word(), w2 = rng.Word();
        w0 = SetBits(w0, 0, 6, rng.Chance(70) ? dests[rng.Below(sizeof dests / 4)] : rng.Below(64));
        w0 = SetBits(w0, 6, 1, rng.Chance(15));                    // vector dest relative
        w0 = SetBits(w0, 7, 1, rng.Chance(20));                    // absolute constants
        w0 = SetBits(w0, 14, 1, rng.Chance(15));                   // scalar dest relative
        w0 = SetBits(w0, 15, 1, rng.Chance(35));                   // export
        // Scalar opcode: 0..50 but 41 are the valid ones; the rest, 1 in 20.
        uint32_t scalar = rng.Below(51);
        if (scalar == 41 || rng.Chance(5)) scalar = rng.Below(64);
        w0 = SetBits(w0, 26, 6, scalar);
        if (rng.Chance(30)) w1 = SetBits(w1, 0, 24, 0);            // identity swizzles
        w1 = SetBits(w1, 28, 1, rng.Chance(25));                   // predicated
        w1 = SetBits(w1, 29, 3, rng.Chance(60) ? 0 : rng.Below(8)); // a0, rel abs
        // Vector opcode: 0..27 are translated, 28..31 (dst, maxa) refused.
        w2 = SetBits(w2, 24, 5, rng.Chance(95) ? rng.Below(28) : rng.Below(32));
        for (uint32_t s = 0; s < 3; s++)
        {
            uint32_t reg = rng.Below(256);
            if (rng.Chance(60)) reg &= 0x3F | (rng.Chance(20) ? 0xC0 : 0);
            w2 = SetBits(w2, 16 - 8 * s, 8, reg);
        }
        out[0] = w0; out[1] = w1; out[2] = w2;
    }

    // A fetch with every opcode, format, dimension and the mini form in reach.
    void RandomFetch(Random& rng, uint32_t out[3])
    {
        static const uint32_t opcodes[] = { 0, 0, 0, 0, 1, 1, 1, 1, 16, 18, 24, 17, 19, 25, 26 };
        static const uint32_t formats[] = { 6, 7, 16, 17, 25, 26, 31, 32, 33, 34, 35, 36, 37, 38, 57 };
        uint32_t w0 = rng.Word(), w1 = rng.Word(), w2 = rng.Word();
        w0 = SetBits(w0, 0, 5, rng.Chance(95) ? opcodes[rng.Below(sizeof opcodes / 4)] : rng.Below(32));
        w0 = SetBits(w0, 19, 1, rng.Chance(90));                   // must be one
        if (rng.Chance(50)) w0 = SetBits(w0, 20, 5, rng.Below(4)); // a few constants
        w1 = SetBits(w1, 16, 6, rng.Chance(85) ? formats[rng.Below(sizeof formats / 4)] : rng.Below(64));
        w1 = SetBits(w1, 30, 1, rng.Chance(30));                   // mini fetch
        w1 = SetBits(w1, 31, 1, rng.Chance(20));                   // predicated
        if (rng.Chance(50)) w1 = SetBits(w1, 24, 6, 0);            // no exponent adjust
        if (rng.Chance(60)) w2 = SetBits(w2, 8, 23, rng.Below(16)); // a small offset
        if (rng.Chance(20)) w2 = SetBits(w2, 2, 7, 0);             // no LOD bias
        out[0] = w0; out[1] = w1; out[2] = w2;
    }

    // A program built from scratch: a control flow list of random opcodes
    // whose execs mostly point at the instructions after it, then those.
    std::vector<uint32_t> RandomProgram(Random& rng)
    {
        const bool long_ = rng.Chance(2);
        const uint32_t cfCount = long_ ? 520 + rng.Below(200) : 1 + rng.Below(rng.Chance(80) ? 12 : 40);
        const uint32_t cfSlots = (cfCount + 1) / 2;   // instruction slots the list takes
        const uint32_t instructionCount = rng.Below(rng.Chance(80) ? 24 : 80);
        std::vector<Cf> flow(cfCount);
        std::vector<bool> isFetch(instructionCount);
        for (uint32_t i = 0; i < instructionCount; i++) isFetch[i] = rng.Chance(35);
        for (uint32_t i = 0; i < cfCount; i++)
        {
            Cf cf{ rng.Word(), rng.Word() & 0xFFFF };
            uint32_t opcode;
            const uint32_t pick = rng.Below(100);
            if (pick < 45) opcode = 1 + rng.Below(6);                     // the execs
            else if (pick < 55) opcode = 13 + rng.Below(2);                // the clean ones
            else opcode = rng.Below(16);
            if (!long_ && i + 1 == cfCount && rng.Chance(90))
            {
                static const uint32_t ends[] = { 2, 4, 6, 14 };
                opcode = ends[rng.Below(4)];
            }
            if (long_ && (opcode == 2 || opcode == 4 || opcode == 6 || opcode == 14)) opcode = 1;
            cf.word1 = SetBits(cf.word1, 12, 4, opcode);
            const bool exec = (opcode >= 1 && opcode <= 6) || opcode == 13 || opcode == 14;
            if (exec)
            {
                const uint32_t count = rng.Below(8);
                uint32_t address = cfSlots + (instructionCount ? rng.Below(instructionCount) : 0);
                if (rng.Chance(5)) address = rng.Below(4096);           // anywhere, past the end too
                cf.word0 = SetBits(cf.word0, 0, 12, address);
                cf.word0 = SetBits(cf.word0, 12, 3, count);
                uint32_t sequence = rng.Below(4096);
                if (rng.Chance(85))
                    for (uint32_t n = 0; n < count; n++)
                    {
                        const uint32_t at = address - cfSlots + n;
                        const bool fetch = at < instructionCount && isFetch[at];
                        sequence = SetBits(sequence, 2 * n, 1, fetch);
                    }
                cf.word0 = SetBits(cf.word0, 16, 12, sequence);
            }
            else if (opcode >= 7 && opcode <= 11)
            {
                // A target inside the list, most of the time.
                if (rng.Chance(85)) cf.word0 = SetBits(cf.word0, 0, 13, rng.Below(cfCount + 1));
                if (rng.Chance(50)) cf.word0 = SetBits(cf.word0, 13, 1, 0);
            }
            flow[i] = cf;
        }
        std::vector<uint32_t> words;
        for (uint32_t i = 0; i < cfCount; i += 2)
            PackCf(flow[i], i + 1 < cfCount ? flow[i + 1] : Cf{}, words);
        for (uint32_t i = 0; i < instructionCount; i++)
        {
            uint32_t w[3];
            if (isFetch[i]) RandomFetch(rng, w); else RandomAlu(rng, w);
            words.insert(words.end(), w, w + 3);
        }
        if (rng.Chance(5) && !words.empty()) words.resize(rng.Below(uint32_t(words.size())));
        return words;
    }

    // A program of the corpus with a few things changed.
    std::vector<uint32_t> Mutate(Random& rng, const std::vector<uint32_t>& base)
    {
        std::vector<uint32_t> words = base;
        if (words.empty()) return words;
        const uint32_t edits = 1 + rng.Below(rng.Chance(70) ? 3 : 16);
        for (uint32_t e = 0; e < edits; e++)
        {
            const uint32_t n = uint32_t(words.size());
            switch (rng.Below(10))
            {
            case 0: case 1: case 2:   // one bit anywhere
                words[rng.Below(n)] ^= 1u << rng.Below(32);
                break;
            case 3:                   // a word anywhere
                words[rng.Below(n)] = rng.Word();
                break;
            case 4:                   // one field of an instruction: opcodes, dests, formats
            {
                const uint32_t at = rng.Below(n);
                static const uint32_t fields[][2] = { { 0, 6 }, { 26, 6 }, { 24, 5 }, { 0, 5 }, { 16, 6 },
                                                      { 15, 1 }, { 28, 1 }, { 29, 3 }, { 30, 1 }, { 14, 2 } };
                const auto& f = fields[rng.Below(10)];
                words[at] = SetBits(words[at], f[0], f[1], rng.Word());
                break;
            }
            case 5:                   // a control flow opcode
            {
                const uint32_t pair = rng.Below(std::min<uint32_t>(n / 3, 8) + 1);
                const uint32_t at = std::min(pair * 3 + 1 + rng.Below(2), n - 1);
                words[at] ^= (rng.Below(16)) << (rng.Chance(50) ? 12 : 28);
                break;
            }
            case 6:                   // a whole instruction from the generator
            {
                const uint32_t at = rng.Below(n / 3 + 1) * 3;
                uint32_t w[3];
                if (rng.Chance(50)) RandomAlu(rng, w); else RandomFetch(rng, w);
                for (uint32_t k = 0; k < 3 && at + k < n; k++) words[at + k] = w[k];
                break;
            }
            case 7:                   // shorter
                words.resize(rng.Below(n + 1));
                if (words.empty()) return words;
                break;
            case 8:                   // longer
            {
                const uint32_t more = 1 + rng.Below(12);
                for (uint32_t k = 0; k < more; k++) words.push_back(rng.Word());
                break;
            }
            default:                  // the predicate and condition bits of a control flow word
                words[rng.Below(std::min<uint32_t>(n, 24))] ^= 1u << (8 + rng.Below(8));
                break;
            }
        }
        return words;
    }

    int Corpus(const char* list)
    {
        const auto programs = LoadList(list);
        // Nothing compared is not a pass.
        if (programs.empty())
        {
            std::printf("corpus: the list names no program\n");
            return 2;
        }
        Tally tally;
        for (const auto& p : programs)
            if (!Check(p.path, p.pixel, p.words, tally)) return 1;
        std::printf("corpus: %llu programs, %llu translations each side (%llu ok, %llu refused), "
                    "%llu SPIR-V words: all identical\n",
                    (unsigned long long)tally.cases, (unsigned long long)tally.translations,
                    (unsigned long long)tally.ok, (unsigned long long)tally.failed,
                    (unsigned long long)tally.spirvWords);
        return 0;
    }

    int Fuzz(const char* list, uint64_t seed, uint64_t cases)
    {
        const auto programs = LoadList(list);
        Random rng{ seed * 0x100000001B3ull + 0x51ED };
        Tally tally;
        uint64_t kinds[3] = {};
        for (uint64_t i = 0; i < cases; i++)
        {
            std::vector<uint32_t> words;
            const uint32_t pick = rng.Below(100);
            bool pixel = rng.Chance(50);
            std::string what;
            char name[96];
            if (pick < 2)
            {
                // Fewer than three words.
                words.resize(rng.Below(3));
                for (auto& w : words) w = rng.Word();
                kinds[0]++;
                std::snprintf(name, sizeof name, "fuzz seed %llu case %llu (short)", (unsigned long long)seed,
                              (unsigned long long)i);
            }
            else if (pick < 55 && !programs.empty())
            {
                const auto& base = programs[rng.Below(uint32_t(programs.size()))];
                pixel = rng.Chance(90) ? base.pixel : !base.pixel;
                words = Mutate(rng, base.words);
                kinds[1]++;
                std::snprintf(name, sizeof name, "fuzz seed %llu case %llu (mutated)", (unsigned long long)seed,
                              (unsigned long long)i);
            }
            else
            {
                words = RandomProgram(rng);
                kinds[2]++;
                std::snprintf(name, sizeof name, "fuzz seed %llu case %llu (random)", (unsigned long long)seed,
                              (unsigned long long)i);
            }
            what = name;
            if (!Check(what, pixel, words, tally)) return 1;
        }
        std::printf("fuzz seed %llu: %llu cases (%llu short, %llu mutated, %llu random), %llu translations each "
                    "side (%llu ok, %llu refused): all identical\n",
                    (unsigned long long)seed, (unsigned long long)tally.cases, (unsigned long long)kinds[0],
                    (unsigned long long)kinds[1], (unsigned long long)kinds[2],
                    (unsigned long long)tally.translations, (unsigned long long)tally.ok,
                    (unsigned long long)tally.failed);
        return 0;
    }

    int Dump(const char* list, const char* directory, uint32_t every)
    {
        const auto programs = LoadList(list);
        uint32_t written = 0;
        for (size_t i = 0; i < programs.size(); i += every)
            for (int scaled = 0; scaled < 2; scaled++)
            {
                const auto& p = programs[i];
                Flat r;
                RustTranslate(p.pixel, p.words.data(), p.words.size(), scaled, r);
                if (r.spirv.empty()) continue;
                char path[4096];
                std::snprintf(path, sizeof path, "%s/%06zu_%s_%d.spv", directory, i, p.pixel ? "pixel" : "vertex",
                              scaled);
                std::FILE* f = std::fopen(path, "wb");
                if (!f) { std::perror(path); return 2; }
                std::fwrite(r.spirv.data(), 4, r.spirv.size(), f);
                std::fclose(f);
                written++;
            }
        std::printf("dump: %u modules\n", written);
        return 0;
    }

    int Bench(const char* list, const char* side, uint32_t rounds)
    {
        const auto programs = LoadList(list);
        const bool rust = std::strcmp(side, "rust") == 0;
        size_t words = 0;
        double best = 1e30;
        for (uint32_t round = 0; round < rounds; round++)
        {
            const auto start = std::chrono::steady_clock::now();
            for (const auto& p : programs)
                for (int scaled = 0; scaled < 2; scaled++)
                {
                    if (rust) RustTranslateOnly(p.pixel, p.words.data(), p.words.size(), scaled, words);
                    else
                    {
                        auto r = shader::Translate(p.pixel ? shader::Type::Pixel : shader::Type::Vertex,
                                                   p.words.data(), p.words.size(), scaled);
                        words += r.spirv.size();
                    }
                }
            const double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - start).count();
            if (ms < best) best = ms;
        }
        std::printf("bench %s: %zu programs x 2, best of %u rounds %.1f ms (%.2f us a translation), %zu words\n",
                    side, programs.size(), rounds, best, best * 1000.0 / double(programs.size() * 2), words);
        return 0;
    }

    // The SPIR-V writer on its own: a script of its calls, the same on both
    // sides, and the module each finishes with. Constants the translator
    // never asks for -- -0.0, NaNs, any bits -- are reached here. Three words
    // a step: what to call, then two arguments; an argument that names a
    // type or a function is an earlier step's result, by index.
    std::vector<uint32_t> CppScript(const std::vector<uint32_t>& ops)
    {
        using namespace spirv;
        Module m;
        std::vector<Id> results;
        auto pick = [&](uint32_t a) { return results.empty() ? a : results[a % results.size()]; };
        static const char kText[] = "abcdefghijklmnopq";
        for (size_t i = 0; i + 3 <= ops.size(); i += 3)
        {
            const uint32_t a = ops[i + 1], b = ops[i + 2];
            float f;
            Id r = 0;
            switch (ops[i] % 21)
            {
            case 0: std::memcpy(&f, &a, 4); r = m.ConstantF(f); break;
            case 1: r = m.ConstantU(a); break;
            case 2: r = m.ConstantS(int32_t(a)); break;
            case 3: std::memcpy(&f, &a, 4); r = m.ConstantSplatF(f); break;
            case 4: r = m.Vector(pick(a), b % 5); break;
            case 5: r = m.Pointer(StorageClass(a % 16), pick(b)); break;
            case 6: r = m.ArrayOf(pick(a), b); break;
            case 7: r = m.RuntimeArrayOf(pick(a)); break;
            case 8: r = m.Image(pick(a), b % 8, (b >> 8) & 1); break;
            case 9: r = m.SampledImage(pick(a)); break;
            case 10: r = m.Sampler(); break;
            case 11: r = m.Void(); break;
            case 12: r = m.Bool(); break;
            case 13: r = m.Int((a & 1) != 0); break;
            case 14: r = m.Float(); break;
            case 15: r = m.ConstantFalseValue(); break;
            case 16: m.RequireCapability(a % 8); break;
            case 17: r = m.Allocate(); break;
            case 18: Module::EmitString(m.Debug(), Op::Name, { pick(a) }, std::string(kText, b % 17)); break;
            case 19:
            {
                std::vector<Id> interface;
                for (uint32_t k = 0; k < b % 4; k++) interface.push_back(pick(a + k));
                m.EntryPoint(a % 5, pick(a), std::string(kText, (a >> 8) % 17), interface);
                break;
            }
            default:
            {
                std::vector<uint32_t>* sections[] = { &m.Capabilities(), &m.Preamble(), &m.EntryPoints(),
                                                      &m.ExecutionModes(), &m.Debug(), &m.Decorations(),
                                                      &m.Declarations(), &m.Code() };
                Module::Emit(*sections[a % 8], Op(b & 0xFFFF), { pick(b), a });
                break;
            }
            }
            if (r) results.push_back(r);
        }
        return m.Finish();
    }

    void TakeWords(void* context, const uint32_t* words, size_t count)
    {
        static_cast<std::vector<uint32_t>*>(context)->assign(words, words + count);
    }

    int WriterScripts(uint64_t seed, uint64_t scripts)
    {
        Random rng{ seed * 0x9E3779B97F4A7C15ull + 7 };
        static const uint32_t kFloats[] = { 0x00000000u, 0x80000000u, 0x3F800000u, 0xBF800000u, 0x7FC00000u,
                                            0x7F800001u, 0xFFC00000u, 0x7F800000u, 0xFF800000u, 0x00000001u,
                                            0x7F7FFFFFu, 0xFF7FFFFFu };
        uint64_t steps = 0;
        for (uint64_t s = 0; s < scripts; s++)
        {
            std::vector<uint32_t> ops;
            const uint32_t length = 1 + rng.Below(rng.Chance(80) ? 40 : 300);
            for (uint32_t k = 0; k < length; k++)
            {
                const uint32_t what = rng.Below(21);
                uint32_t a = rng.Chance(50) ? rng.Below(8) : rng.Word();
                if ((what == 0 || what == 3) && rng.Chance(70)) a = kFloats[rng.Below(sizeof kFloats / 4)];
                ops.push_back(what);
                ops.push_back(a);
                ops.push_back(rng.Chance(50) ? rng.Below(8) : rng.Word());
            }
            steps += length;
            const std::vector<uint32_t> c = CppScript(ops);
            std::vector<uint32_t> r;
            mw2_spirv_script(ops.data(), ops.size(), &r, TakeWords);
            if (c != r)
            {
                size_t j = 0;
                while (j < c.size() && j < r.size() && c[j] == r[j]) j++;
                std::printf("DIFFERENCE: the SPIR-V writer, seed %llu script %llu: %zu and %zu words, "
                            "first difference at word %zu\ncase: script (what, a, b):\n",
                            (unsigned long long)seed, (unsigned long long)s, c.size(), r.size(), j);
                for (size_t k = 0; k + 3 <= ops.size(); k += 3)
                    std::printf("  %u %08X %08X\n", ops[k], ops[k + 1], ops[k + 2]);
                return 1;
            }
        }
        std::printf("spirv writer seed %llu: %llu scripts, %llu calls: all identical\n", (unsigned long long)seed,
                    (unsigned long long)scripts, (unsigned long long)steps);
        return 0;
    }

    int Describes(uint64_t seed, uint64_t cases)
    {
        Random rng{ seed * 0xD1B54A32D192ED03ull + 3 };
        auto value = [&]() -> uint32_t {
            switch (rng.Below(4))
            {
            case 0: return 0;
            case 1: return 1;
            case 2: return rng.Below(1000);
            default: return rng.Word();
            }
        };
        for (uint64_t i = 0; i < cases; i++)
        {
            DescribeInput in;
            in.ok = rng.Chance(60);
            in.writesDepth = rng.Chance(30);
            in.writesMemory = rng.Chance(30);
            const uint32_t length = rng.Chance(50) ? rng.Below(40) : rng.Below(700);
            for (uint32_t k = 0; k < length; k++)
                in.error.push_back(char(rng.Chance(90) ? 32 + rng.Below(95) : rng.Below(256)));
            in.spirvWords = rng.Chance(98) ? rng.Below(5000) : rng.Below(3000000);
            in.instructionCount = value();
            in.interpolatorMask = value();
            in.colourMask = value();
            in.textureMask = value();
            in.vertexFetchMask = value();
            in.droppedExports = value();
            in.droppedMemoryExports = value();
            const std::string c = shader::Describe(MakeTranslation<shader::Translation>(in));
            const std::string r = RustDescribe(in);
            if (c != r)
            {
                std::printf("DIFFERENCE: describe, seed %llu case %llu: C++ \"%s\" (%zu bytes), Rust \"%s\" (%zu bytes)\n",
                            (unsigned long long)seed, (unsigned long long)i, c.c_str(), c.size(), r.c_str(),
                            r.size());
                return 1;
            }
        }
        std::printf("describe seed %llu: %llu cases: all identical\n", (unsigned long long)seed,
                    (unsigned long long)cases);
        return 0;
    }

    void PrintText(void*, const char* text, size_t length) { std::fwrite(text, 1, length, stdout); }

    int Bindings()
    {
        namespace b = vk::bindings;
        std::printf("== C++\n");
#define B(name) std::printf("%s %u\n", #name, unsigned(b::name));
        B(kConstantSet) B(kVertexFloats) B(kFetchConstants) B(kBoolConstants) B(kLoopConstants)
        B(kPixelFloats) B(kConstantBlocks) B(kFloatConstants) B(kConstantFileBlocks) B(kConstantTableWords)
        B(kFetchSlots) B(kTextureSet) B(kTextureSlots) B(kImages) B(kSamplers) B(kImageKinds) B(kImageCount)
        B(kSamplerCount) B(kHandleImageBits) B(kArenaSet) B(kArenaBinding) B(kArenaFloats) B(kRecordOffsets)
        B(kRecordNdc) B(kRecordPixel) B(kRecordScaled) B(kRecordTextures) B(kRecordVertexTextures)
        B(kRecordWords) B(kRecordLocation)
#undef B
        std::printf("== Rust\n");
        std::fflush(stdout);
        mw2_shader_bindings(nullptr, PrintText);
        return 0;
    }

    int Layout()
    {
        std::printf("== C++\n");
        ForwarderLayout();
        std::printf("== Rust\n");
        std::fflush(stdout);
        mw2_shader_meta_layout(nullptr, PrintText);
        return 0;
    }
}

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    const std::string mode = argc > 1 ? argv[1] : "";
    if (mode == "corpus" && argc == 3) return Corpus(argv[2]);
    if (mode == "fuzz" && argc == 5) return Fuzz(argv[2], std::strtoull(argv[3], nullptr, 10),
                                                 std::strtoull(argv[4], nullptr, 10));
    if (mode == "dump" && argc == 5) return Dump(argv[2], argv[3], uint32_t(std::strtoul(argv[4], nullptr, 10)));
    if (mode == "bench" && argc == 5) return Bench(argv[2], argv[3], uint32_t(std::strtoul(argv[4], nullptr, 10)));
    if (mode == "bindings") return Bindings();
    if (mode == "layout") return Layout();
    if (mode == "describe" && argc == 4) return Describes(std::strtoull(argv[2], nullptr, 10),
                                                          std::strtoull(argv[3], nullptr, 10));
    if (mode == "module" && argc == 4) return WriterScripts(std::strtoull(argv[2], nullptr, 10),
                                                            std::strtoull(argv[3], nullptr, 10));
    std::fprintf(stderr, "usage: see the head of tools/shader_diff/shader_diff.cpp\n");
    return 2;
}

#endif
