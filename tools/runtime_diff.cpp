// tools/runtime_diff.py's program: the C++ of a module and the Rust that replaced it,
// given the same inputs, in one process. A seed is a process, because a module reads
// its switches and its XLAST once. Seed 1 is the title's own XLAST and executables,
// the rest made-up ones.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "guest_memory.h"
#include "console.h"
#include "install/crypto.h"
#include "install/xex.h"
#include "presence.h"
#include "gpu/memory_watch.h"
#include "platform.h"
#include "player.h"
#include "render_size.h"
#include "settings.h"
#include "shadow_resolution.h"
#include "text_sink.h"
#include "version.h"
#include "screen.h"
#include "signin.h"
#include "friends.h"
#include "guide.h"
#include "online/service.h"
#include "kernel/kernel.h"
#include "crash.h"

#include <sys/mman.h>
#include <unistd.h>
#include <zlib.h>

#include <algorithm>
#include <bit>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <map>
#include <set>
#include <string>
#include <vector>

extern "C"
{
    void cpp_mw2_settings_load(const char* path, void* set, mw2_text_sink sink);
    void cpp_mw2_presence_set_context(uint32_t user, uint32_t id, uint32_t value);
    void cpp_mw2_presence_text(uint32_t user, void* line, mw2_text_sink sink);
    bool cpp_mw2_shadows_is_map_depth(uint32_t depthInfo, uint32_t samples);
    bool cpp_mw2_shadows_surfaces_known();
    uint32_t cpp_mw2_shadows_surface_height();
    void cpp_mw2_console_pump(PPCContext& ctx, unsigned char* base);
    void cpp_mw2_console_run_now(const char* text);
    void cpp_mw2_console_place(const char* text);
    const char* cpp_mw2_version_release();
    uint16_t cpp_mw2_version_network();
    void rig_sha256(const uint8_t* data, size_t size, char* out);
    void rig_aes(const uint8_t (*key)[16], uint8_t* data, size_t size);
    bool rig_xex(const uint8_t* file, size_t size, uint8_t* to, size_t room, char* error, size_t* written);
    bool rig_parse(const char* text, render::Size* size);
    render::Size rig_scene(render::Size frame);
    bool rig_frame_valid(render::Size frame);
}

// The C++ wrote these under cpp_mw2_ too, and the header's own is the Rust's.
namespace
{
    struct Random
    {
        uint64_t state;
        uint64_t Next()
        {
            state += 0x9E3779B97F4A7C15ull;
            uint64_t z = state;
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint32_t Below(uint32_t n) { return n ? uint32_t(Next() % n) : 0; }
    };

    [[noreturn]] void Different(const char* what, const std::string& detail = {})
    {
        std::printf("DIFFERENT: %s %s\n", what, detail.c_str());
        std::exit(1);
    }

    std::vector<uint8_t> Read(const std::string& path)
    {
        std::ifstream in(path, std::ios::binary);
        std::vector<uint8_t> bytes(std::istreambuf_iterator<char>(in), {});
        if (bytes.empty()) Different("nothing to read in", path);
        return bytes;
    }

    void Crypto(Random& random, uint64_t cases)
    {
        for (uint64_t i = 0; i < cases; i++)
        {
            constexpr uint32_t kEdges[] = { 0, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129, 1000 };
            const size_t size = kEdges[random.Below(15)] + (random.Below(4) ? 0 : random.Below(200));
            std::vector<uint8_t> data(size);
            for (uint8_t& byte : data) byte = uint8_t(random.Next());
            install::crypto::Sha256 hash;
            for (size_t at = 0; at < size;)
            {
                const size_t take = std::min<size_t>(size - at, 1 + random.Below(90));
                hash.Update(data.data() + at, take);
                at += take;
            }
            char hex[65] = {};
            rig_sha256(data.data(), size, hex);
            if (hash.FinalHex() != hex) Different("sha256", std::to_string(size));

            install::crypto::Key key;
            for (uint8_t& byte : key) byte = uint8_t(random.Next());
            std::vector<uint8_t> a((1 + random.Below(16)) * 16);
            for (uint8_t& byte : a) byte = uint8_t(random.Next());
            std::vector<uint8_t> b = a;
            install::crypto::Aes128CbcDecrypt(key, a.data(), a.size());
            rig_aes(reinterpret_cast<const uint8_t(*)[16]>(key.data()), b.data(), b.size());
            if (a != b) Different("aes");
        }
    }

    void CompareXex(const std::vector<uint8_t>& file, const char* what)
    {
        std::vector<uint8_t> cpp, rust(file.size() * 40 + 0x400000);
        std::string error;
        char rustError[256] = {};
        size_t written = 0;
        const bool a = install::xex::Image(file, cpp, error);
        const bool b = rig_xex(file.data(), file.size(), rust.data(), rust.size(), rustError, &written);
        if (a != b) Different(what, "one refused: " + error + " / " + rustError);
        if (!a && error != rustError) Different(what, error + " / " + rustError);
        rust.resize(written);
        if (a && cpp != rust) Different(what, "images differ");
    }

    void Put32(std::vector<uint8_t>& d, size_t at, uint32_t value)
    {
        for (int i = 0; i < 4; i++) d[at + i] = uint8_t(value >> (24 - i * 8));
    }

    // An XEX of its parts: each field right, or now and then one a check should catch.
    void MadeUpXex(Random& random)
    {
        const uint32_t payload = 32 + 16 * random.Below(40) + (random.Below(2) ? 0 : random.Below(16));
        const uint32_t format = 0x40, security = 0x80, data = 0x80 + 352 + random.Below(8);
        std::vector<uint8_t> file(data + payload);
        for (size_t i = data; i < file.size(); i++) file[i] = uint8_t(random.Next());
        std::memcpy(file.data(), random.Below(16) ? "XEX2" : "XEX1", 4);
        Put32(file, 0x08, random.Below(16) ? data : uint32_t(file.size()) + random.Below(3));
        Put32(file, 0x10, random.Below(8) ? security : uint32_t(file.size()) - 353 + random.Below(3));
        // One header, or exactly as many as fit and each side of that, or far too many.
        const uint32_t fit = uint32_t(file.size() - 0x18) / 8;
        Put32(file, 0x14, random.Below(4) ? 1 : random.Below(4) ? fit + random.Below(2) : 5000);
        Put32(file, 0x18, random.Below(16) ? 0x3FF : 0x400);
        Put32(file, 0x1C, format);
        const uint32_t compression = random.Below(16) ? random.Below(2) : 2, encryption = random.Below(16) ? random.Below(2) : 7;
        const uint32_t blocks = random.Below(4);
        Put32(file, format, 8 + blocks * 8 + random.Below(8));
        file[format + 4] = uint8_t(encryption >> 8); file[format + 5] = uint8_t(encryption);
        file[format + 6] = uint8_t(compression >> 8); file[format + 7] = uint8_t(compression);
        for (uint32_t i = 0; i < blocks; i++)
        {
            Put32(file, format + 8 + i * 8, random.Below(payload / 2 + 2));
            Put32(file, format + 12 + i * 8, random.Below(64));
        }
        for (int i = 0; i < 16; i++) file[security + 336 + i] = uint8_t(random.Next());
        if ((!encryption || !compression) && random.Below(2)) { file[data] = 'M'; file[data + 1] = 'Z'; }
        CompareXex(file, "made-up xex");
    }

    void Sizes(Random& random, uint64_t cases)
    {
        constexpr const char* kParts[] = { "960", "600", "x", "X", " ", "\t", "+", "-", "0", "1280", "720", "abc", "99999999999", "\n", "0x10", "4294968256", "18446744073709551617" };
        for (uint64_t i = 0; i < cases; i++)
        {
            std::string text;
            for (uint32_t parts = random.Below(6); parts > 0; parts--) text += kParts[random.Below(17)];
            render::Size rust{};
            const auto cpp = render::Parse(text.c_str());
            const bool parsed = rig_parse(text.c_str(), &rust);
            if (parsed != cpp.has_value()) Different("parse", "\"" + text + "\"");
            if (parsed && !(*cpp == rust)) Different("parse value", "\"" + text + "\"");

            const render::Size frame{ random.Below(1600), random.Below(1000) };
            if (render::FrameValid(frame) != rig_frame_valid(frame)) Different("frame valid");
            const render::Size even{ 640 + 16 * random.Below(41), 360 + 4 * random.Below(112) };
            if (render::FrameValid(even) != rig_frame_valid(even)) Different("frame valid");
            if (render::FrameValid(even) && !(render::Scene(even) == rig_scene(even))) Different("scene");
        }
    }

    void Settings(Random& random, uint64_t cases, const std::string& folder)
    {
        const std::string path = folder + "/settings." + std::to_string(random.Next());
        constexpr const char* kNames[] = { "MW2_A", "MW2_B", "MW2_C_PASSWORD", "MW2_PASSWORD_D", "NOT_MW2", "MW2_", "mw2_x", "MW2", "" };
        constexpr const char* kValues[] = { "1", "", "a b", " x ", "=", "a=b", "\r", "  ", "\xC3\xA9" };
        constexpr const char* kEnds[] = { "\n", "\r\n", " \n", "", " \r \n" };
        for (uint64_t i = 0; i < cases; i++)
        {
            std::string text;
            for (uint32_t lines = random.Below(8); lines > 0; lines--)
            {
                text += kNames[random.Below(9)];
                if (random.Below(8)) text += '=';
                text += kValues[random.Below(9)];
                text += kEnds[random.Below(5)];
            }
            std::ofstream(path, std::ios::binary) << text;
            const auto ours = [] {
                std::map<std::string, std::string> found;
                for (char** entry = environ; *entry; entry++)
                    if (const char* equals = std::strchr(*entry, '='); equals && !std::strncmp(*entry, "MW2_", 4) &&
                        std::strncmp(*entry, "MW2_CONSOLE", 11))
                        found[std::string(*entry, size_t(equals - *entry))] = equals + 1;
                return found;
            };
            const auto clear = [&] {
                for (const auto& [name, value] : ours()) unsetenv(name.c_str());
                if (random.Below(4) == 0) setenv("MW2_B", "kept", 1);
            };
            const auto read = [&] {
                std::string all;
                for (const auto& [name, value] : ours()) all += name + "=" + value + ";";
                return all;
            };
            const uint64_t state = random.state;
            clear();
            std::string cpp, rust;
            cpp_mw2_settings_load(path.c_str(), &cpp, mw2_assign_text);
            const std::string cppSet = read();
            random.state = state;
            clear();
            mw2_settings_load(path.c_str(), &rust, mw2_assign_text);
            if (cpp != rust) Different("settings' text", "\n" + text + "\n" + cpp + "\n" + rust);
            if (cppSet != read()) Different("settings' variables", "\n" + text + "\n" + cppSet + "\n" + read());
        }
        std::remove(path.c_str());
        std::string none = "x", also = "x";
        cpp_mw2_settings_load("/no/such/file", &none, mw2_assign_text);
        mw2_settings_load("/no/such/file", &also, mw2_assign_text);
        if (none != also) Different("settings without a file");
    }

    uint8_t* Spa() { return guest::Base() + T_DATA_Spa; }

    void Set32(uint32_t address, uint32_t value)
    {
        uint8_t* p = guest::Base() + address;
        for (int i = 0; i < 4; i++) p[i] = uint8_t(value >> (24 - i * 8));
    }

    // A gzipped XLAST in an XDBF, with text that tries each thing the reader does.
    void MadeUpSpa(Random& random, std::string& xml)
    {
        constexpr const char* kLines[] = {
            "Plays {c10} on {c11}", "{c10}", "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;  ", "Mark\xC2\xAE", "{p5} left", "{c99}", "{c}", "{}",
            "{c10", "x{c0x0B}y", "&amp;amp;", "\t trimmed \r\n", "{c11}{c10}{c11}", "{p10}", "{x11} too", "&a\xC2\xAEmp;", "tab\t",
            "pair \x01 euro \x02 del \x03 bare \x04" };
        xml = "<?xml?>";
        for (uint32_t id = 0; id < 30; id++)
        {
            if (random.Below(8) == 0) continue;
            xml += "<LocalizedString id=\"" + std::to_string(id) + "\">";
            xml += random.Below(8) ? "<Translation locale=\"en-US\">" : "<Translation locale=\"fr-FR\">";
            xml += kLines[random.Below(4) ? id % std::size(kLines) : random.Below(uint32_t(std::size(kLines)))];
            xml += "</Translation>";
            if (random.Below(16)) xml += "</LocalizedString>";
        }
        for (uint32_t mode = 0; mode < 20; mode++)
            xml += (random.Below(8) ? "<PresenceMode contextValue=\"" + std::to_string(mode) + "\" stringId=\"" : std::string("<PresenceMode stringId=\"")) +
                   std::to_string(random.Below(8) ? mode : random.Below(32)) + "\"/>";
        for (uint32_t id : { 10u, 11u, 12u })
        {
            xml += "<Context id=\"" + std::string(id == 11 ? "0xB" : std::to_string(id)) + "\" defaultValue=\"" + std::to_string(random.Below(3)) + "\">";
            for (uint32_t value = 0; value < 3; value++)
                xml += "<ContextValue value=\"" + std::to_string(value) + "\" stringId=\"" + std::to_string(random.Below(32)) + "\"/>";
            if (id != 12 || random.Below(2)) xml += "</Context>";
        }
        std::string wide = random.Below(2) ? std::string("\xFF\xFE", 2) : std::string();
        for (const char c : xml)
        {
            // The markers MadeUpSpa's lines hold: what UTF-8 has no ASCII for.
            if (c == 1) wide += std::string("\x3D\xD8\x00\xDE", 4);        // a surrogate pair
            else if (c == 2) wide += std::string("\xAC\x20", 2);
            else if (c == 3) wide += std::string("\x7F\x00", 2);
            else if (c == 4) wide += std::string("\x00\xD8", 2);             // a high surrogate alone
            else if (uint8_t(c) == 0xC2) continue;                            // the lead of U+00AE, the only one here
            else { wide += c; wide += '\0'; }
        }
        std::vector<uint8_t> packed(wide.size() + 1024);
        z_stream stream{};
        deflateInit2(&stream, 6, Z_DEFLATED, 16 + MAX_WBITS, 8, Z_DEFAULT_STRATEGY);
        stream.next_in = reinterpret_cast<Bytef*>(wide.data());
        stream.avail_in = uInt(wide.size());
        stream.next_out = packed.data();
        stream.avail_out = uInt(packed.size());
        deflate(&stream, Z_FINISH);
        const uint32_t size = uint32_t(stream.total_out);
        deflateEnd(&stream);

        const uint32_t entriesMax = 4 + random.Below(4), freeMax = random.Below(4), base = 24 + entriesMax * 18 + freeMax * 8;
        const uint32_t name = random.Below(32);
        std::memset(Spa(), 0, 64);
        std::memcpy(Spa(), "XDBF", 4);
        Set32(T_DATA_Spa + 8, entriesMax);
        Set32(T_DATA_Spa + 12, 3);
        Set32(T_DATA_Spa + 16, freeMax);
        const uint32_t kOther = T_DATA_Spa + 24;
        std::memset(guest::Base() + kOther, 0, 18 * 3);
        guest::Base()[kOther + 1] = 2;                  // another namespace
        guest::Base()[kOther + 18 + 1] = 1;
        std::memcpy(guest::Base() + kOther + 18 + 6, "XOTH", 4);
        const uint32_t entry = kOther + 36;
        guest::Base()[entry + 1] = 1;
        std::memcpy(guest::Base() + entry + 6, "XSRC", 4);
        Set32(entry + 10, 64);
        Set32(entry + 14, 24 + name + size);
        const uint32_t section = T_DATA_Spa + base + 64;
        Set32(section + 12, name);
        Set32(section + 16 + name, uint32_t(wide.size()));
        Set32(section + 20 + name, size);
        std::memcpy(guest::Base() + section + 24 + name, packed.data(), size);
    }

    void Presence(Random& random, uint64_t cases, bool real, const std::string& root)
    {
        std::string xml;
        if (real)
        {
#ifdef MW2_VERSION_TU0
            const std::string folder = "/mw2/tu0";
#else
            const std::string folder = "/mw2";
#endif
            const std::vector<uint8_t> image = Read(root + folder + (std::string(MW2_TITLE_NAME).find("multi") == 0 ? "/default_mp.pe" : "/default.pe"));
            std::memcpy(guest::Base() + PPC_IMAGE_BASE, image.data(), image.size());
            if (std::memcmp(Spa(), "XDBF", 4)) Different("no XLAST in", folder);
        }
        else
        {
            MadeUpSpa(random, xml);
        }
        constexpr uint32_t kPresence = 0x8001;
        std::vector<std::pair<uint32_t, uint32_t>> known = { { kPresence, 0 } };
        for (uint64_t i = 0; i < cases; i++)
        {
            const uint32_t user = random.Below(8) ? random.Below(4) : 4 + random.Below(4);
            uint32_t id, value;
            if (random.Below(4))
            {
                id = random.Below(2) ? kPresence : 10 + random.Below(3);
                value = random.Below(24);
            }
            else
            {
                id = uint32_t(random.Next()) % 256;
                value = uint32_t(random.Next()) % 64;
            }
            cpp_mw2_presence_set_context(user, id, value);
            mw2_presence_set_context(user, id, value);
            std::string a = "x", b = "y";
            cpp_mw2_presence_text(random.state % 5, &a, mw2_assign_text);
            mw2_presence_text(random.state % 5, &b, mw2_assign_text);
            if (a != b) Different("presence", "\n" + a + "\n" + b + "\n" + xml);
        }
        // One line a user can reach at all.
        std::string line;
        mw2_presence_text(0, &line, mw2_assign_text);
        std::printf("presence: last line \"%s\"\n", line.c_str());
    }

    void Shadows(Random& random, uint64_t cases)
    {
#ifdef T_DATA_RenderTargets
        constexpr uint32_t kSurfaces = 0x90000000;
        for (uint64_t i = 0; i < cases; i++)
        {
            for (uint32_t target : { 11u, 12u })
            {
                const uint32_t row = T_DATA_RenderTargets + target * 20;
                const uint32_t surface = kSurfaces + target * 64;
                Set32(row + 8, random.Below(4) ? surface : 0);
                Set32(row + 16, random.Below(8) ? 2048 >> random.Below(3) : 0);
                Set32(surface + 24, uint32_t(random.Next()));
                Set32(surface + 28, random.Below(2) ? 0x10000 : uint32_t(random.Next()));
            }
            // No surface and a pass with none: identity 0, for a port that read one anyway.
            const uint32_t info = random.Below(4) == 0 ? 0 : random.Below(2) ? 0x10000 | random.Below(2) : uint32_t(random.Next());
            const uint32_t samples = info ? random.Below(4) : 0;
            if (cpp_mw2_shadows_is_map_depth(info, samples) != mw2_shadows_is_map_depth(info, samples)) Different("shadow depth");
            if (cpp_mw2_shadows_surfaces_known() != mw2_shadows_surfaces_known()) Different("shadow known");
            if (cpp_mw2_shadows_surface_height() != mw2_shadows_surface_height()) Different("shadow height");
        }
#endif
    }

    std::vector<std::string> g_queued;
    int g_side = 0;
}

#define RIG_CAT(a, b) a##b
#define RIG_NAME(a, b) RIG_CAT(a, b)
extern "C" void RIG_NAME(sub_, T_Cbuf_AddText)(PPCContext& ctx, uint8_t* base)
{
    char line[64];
    std::snprintf(line, sizeof line, "r1=%llx r3=%llx r4=%llx ", (unsigned long long)ctx.r1.u64, (unsigned long long)ctx.r3.u64, (unsigned long long)ctx.r4.u64);
    g_queued.push_back(line + std::string(reinterpret_cast<const char*>(base + ctx.r4.u32)));
    ctx.r5.u64 = 0x1234;
}
extern "C" void RIG_NAME(__imp__sub_, T_Cbuf_AddText)(PPCContext& ctx, uint8_t* base) { RIG_NAME(sub_, T_Cbuf_AddText)(ctx, base); }

namespace
{
    void Console(Random& random, uint64_t cases)
    {
        std::string script;
        constexpr const char* kItems[] = { "0:map af_caves", ":no time", "no colon", "0:", " 0x0:hex", "0:a:b", "1e-9:small", "0:with newline\n", "", "9999:later", "0:x" };
        for (uint32_t items = random.Below(12); items > 0; items--) script += std::string(kItems[random.Below(11)]) + ";";
        script += "0:" + std::string(186 + random.Below(10), 'c');
        setenv("MW2_CONSOLE", script.c_str(), 1);
        const auto run = [&](auto pump, auto now, auto place, std::vector<std::string>& out, uint64_t seed) {
            Random own{ seed };
            g_queued.clear();
            for (uint64_t i = 0; i < cases; i++)
            {
                switch (own.Below(6))
                {
                case 0: now(("text" + std::to_string(own.Below(100))).c_str()); break;
                case 1: now(std::string(190 + own.Below(5), 'n').c_str()); break;
                case 2: place(("camera" + std::to_string(own.Below(100))).c_str()); break;
                case 3: now(""); break;
                default: break;
                }
                PPCContext ctx{};
                ctx.r1.u64 = own.Below(8) ? 0x90010000u - 0x10 * own.Below(16) : own.Below(0x2000);
                ctx.r5.u64 = 77;
                pump(ctx, guest::Base());
                if (ctx.r5.u64 != 77) out.push_back("r5 left");
                if (ctx.r1.u64 == 0) out.push_back("r1 lost");
            }
            out.insert(out.end(), g_queued.begin(), g_queued.end());
        };
        std::vector<std::string> a, b;
        const uint64_t seed = random.Next();
        run(cpp_mw2_console_pump, cpp_mw2_console_run_now, cpp_mw2_console_place, a, seed);
        run(mw2_console_pump, mw2_console_run_now, mw2_console_place, b, seed);
        if (a != b)
        {
            size_t i = 0;
            while (i < a.size() && i < b.size() && a[i] == b[i]) i++;
            Different("console", std::to_string(i) + "\n" + (i < a.size() ? a[i] : "(end)") + "\n" + (i < b.size() ? b[i] : "(end)"));
        }
        std::printf("console: %zu lines queued\n", a.size());
    }
}

// The modules with a clock: one script of events, run on each side from the same time,
// and what each wrote to the log compared.
uint64_t rig_nanoseconds = 1ull << 50;

extern "C"
{
    bool cpp_mw2_player_wanted();
    bool cpp_mw2_player_walking();
    bool cpp_mw2_player_arrived();
    double cpp_mw2_player_seconds_since_progress();
    bool cpp_mw2_player_locate(player::Where* out);
    bool cpp_mw2_player_steer(short* axes, int* press);
    void cpp_mw2_player_saw_pad(uint16_t buttons, uint8_t rightTrigger);
    void cpp_mw2_player_report();
    bool rig_player_wanted();
    bool rig_player_walking();
    bool rig_player_arrived();
    double rig_player_seconds();
    bool rig_player_locate(player::Where* out);
    bool rig_player_steer(short* axes, int* press);
    void rig_player_saw_pad(uint16_t buttons, uint8_t rightTrigger);
    void rig_player_report();

    void cpp_mw2_report_frame(bool world);
    void cpp_mw2_report_add(int cost, uint64_t microseconds);
    void cpp_mw2_report_spent(int time, uint64_t nanoseconds);
    void cpp_mw2_report_shader_built(int fromCache);
    void cpp_mw2_report_thread_started(const char* name);
    void cpp_mw2_report_thread_ended();
    void cpp_mw2_report_shaders_compiled(const char* state);
    uint64_t cpp_mw2_report_timed_begin(int time);
    void cpp_mw2_report_timed_end(int time, uint64_t began);
    void cpp_mw2_report_write();
    void rig_report_frame(bool world);
    void rig_report_add(int cost, uint64_t microseconds);
    void rig_report_spent(int time, uint64_t nanoseconds);
    void rig_report_shader_built(int fromCache);
    void rig_report_thread_started(const char* name);
    void rig_report_thread_ended();
    void rig_report_shaders_compiled(const char* state);
    uint64_t rig_report_timed_begin(int time);
    void rig_report_timed_end(int time, uint64_t began);
    void rig_report_write();

    bool cpp_mw2_stutters_wanted();
    void cpp_mw2_stutters_add(int cost, uint64_t nanoseconds);
    void cpp_mw2_stutters_title_presented();
    void cpp_mw2_stutters_finished(uint64_t serial, bool world);
    void cpp_mw2_stutters_shown(uint64_t serial);
    void cpp_mw2_stutters_displayed(uint64_t serial, uint32_t extraBlanks, double gapMs);
    void cpp_mw2_stutters_mark(const char* how);
    void cpp_mw2_stutters_report();
    bool rig_stutters_wanted();
    void rig_stutters_add(int cost, uint64_t nanoseconds);
    void rig_stutters_title_presented();
    void rig_stutters_finished(bool world);
    void rig_stutters_shown(uint64_t serial);
    void rig_stutters_displayed(uint64_t serial, uint32_t extraBlanks, double gapMs);
    void rig_stutters_mark(const char* how);
    void rig_stutters_report();

    void cpp_mw2_pacing_note(char event, uint32_t value);
    void cpp_mw2_pacing_note_callers(uint32_t r1);
    void cpp_mw2_pacing_write();
    void rig_pacing_note(uint8_t event, uint32_t value);
    void rig_pacing_note_callers(uint32_t r1);
    void rig_pacing_write();

    // What the Rust runtime asks of the C++ one, which is not here.
    uint64_t mw2_kernel_system_time() { return 0x01DD000000000000ull; }
    bool mw2_watch_handle_fault(const void*) { return false; }
    uint64_t mw2_gpu_frame_count() { return 0; }
    void mw2_crash_on_fault() {}

    // What the screens and the online service ask of the kernel, written down as the
    // side that runs now said it.
    std::string g_said;
    std::filesystem::path g_cppRoot, g_rustRoot;
    long g_rigTime = 1800000000;
    // What the Rust screens tell its XAM (src/runtime/kernel/xam.rs), as the C++'s below.
    void rig_xam_told(const char* what, uint32_t id, uint32_t param)
    {
        g_said += std::string(what) == "move" ? std::string("move\n")
                                               : "notify " + std::to_string(id) + " " + std::to_string(param) + "\n";
    }
    void mw2_crash_request_exit(const char* why) { g_said += std::string("exit ") + why + "\n"; }
    // The GPU's side of the kernel's video calls, which no case here makes.
    void mw2_gpu_set_ring_buffer(uint32_t, uint32_t) {}
    void mw2_gpu_set_read_pointer_write_back(uint32_t, uint32_t) {}
    void mw2_gpu_set_interrupt_callback(uint32_t, uint32_t) {}
    void mw2_gpu_swap() {}
    void mw2_renderer_flash_seen(const char*) {}
    long rig_time(long* out)
    {
        if (out) *out = g_rigTime;
        return g_rigTime;
    }

    bool cpp_mw2_signin_input(uint32_t user, uint16_t buttons);
    bool cpp_mw2_friends_input(uint32_t user, uint16_t buttons);
    bool cpp_mw2_guide_input(uint32_t user, uint16_t buttons);
    void cpp_mw2_signin_ask();
    void cpp_mw2_friends_ask();
    void cpp_mw2_signin_at(uint32_t user, bool* signedIn, uint64_t* id, void* name, mw2_text_sink sink);
    void cpp_mw2_signin_first(uint64_t* id, void* name, mw2_text_sink sink);
    uint32_t cpp_mw2_signin_mask();
    bool cpp_mw2_signin_picture(uint32_t windowHeight, mw2_image* image);
    bool cpp_mw2_friends_picture(uint32_t windowHeight, mw2_image* image);
    bool cpp_mw2_guide_picture(uint32_t windowHeight, mw2_image* image);

    bool cpp_mw2_online_started();
    bool rig_start_rig_steam();
    bool rig_start_rig_lan();
    bool rigcpp_is_connect_line(const char* line, size_t size);
    bool rigcpp_read_game(const char* text, uint64_t* counts, uint8_t* session, uint32_t* numbers, size_t* count);
    bool rigcpp_read_session(const char* hex, uint8_t* session);
    void rigcpp_elsewhere(const char* playing, char* out, size_t room);
    bool rig_is_connect_line(const char* line, size_t size);
    bool rig_read_game(const char* text, uint64_t* counts, uint8_t* session, uint32_t* numbers, size_t* count);
    bool rig_read_session(const char* hex, uint8_t* session);
    void rig_elsewhere(const char* playing, char* out, size_t room);
    void rig_game_text(const uint8_t* session, const uint32_t* counts, const uint32_t* numbers, size_t count, void* text,
                       mw2_text_sink sink);

    // The rig's machine: what it reads each time is the same on both sides, as long as
    // they read it in the same order, and that they do is part of what is compared.
    uint64_t g_readings = 0;
    int rig_forever(unsigned) { for (;;) pause(); }
    bool rig_load(platform::Load* load)
    {
        const uint64_t n = ++g_readings;
        if (n % 17 == 0) return false;
        load->process = n * 3000000000ull;
        load->busy = n * 5000000000ull;
        load->total = n * 12000000000ull;
        return true;
    }
    void rig_memory(platform::Memory* memory)
    {
        const uint64_t n = ++g_readings;
        *memory = {};
        memory->installed = (n % 2 ? 16ull : 15ull) << 30;
        memory->usable = n % 5 ? 15ull << 30 : 0;
        memory->free = memory->usable ? (9ull << 30) - (n * 37 % 64) * (1ull << 26) : 1ull << 20;
        if (n % 3)
        {
            memory->commitLimit = 32ull << 30;
            memory->committed = (7ull << 30) + (n * 53 % 64) * (1ull << 26);
            memory->processCommitted = 3ull << 30;
        }
        memory->processPeak = n % 7 ? 2ull << 30 : 0;
    }
    void rig_watch(gpu::watch::Costs* costs)
    {
        const uint64_t n = ++g_readings;
        *costs = { n * 400, n * 90, n * 77777 };
    }
    uintptr_t rig_thread_clock() { return uintptr_t(++g_readings); }
    bool rig_thread_time(uintptr_t clock, uint64_t* time)
    {
        const uint64_t n = ++g_readings;
        *time = n * (clock % 5 == 0 ? clock * 40000000 : clock * 2654435761u % 4000000);
        return (n + clock) % 41 != 0;
    }
    void mw2_watch_costs(gpu::watch::Costs* costs) { rig_watch(costs); }
}

namespace gpu::watch
{
    Costs CostsSoFar()
    {
        Costs costs;
        rig_watch(&costs);
        return costs;
    }
}

namespace
{
    // What the block writes to stderr.
    template <typename Body> std::string Logged(Body body)
    {
        std::fflush(stderr);
        const int kept = dup(2);
        FILE* file = std::tmpfile();
        dup2(fileno(file), 2);
        body();
        std::fflush(stderr);
        dup2(kept, 2);
        close(kept);
        std::string text(size_t(ftell(file) >= 0 ? lseek(fileno(file), 0, SEEK_END) : 0), '\0');
        lseek(fileno(file), 0, SEEK_SET);
        if (read(fileno(file), text.data(), text.size()) < 0) text.clear();
        std::fclose(file);
        return text;
    }

    void Same(const char* what, const std::string& a, const std::string& b)
    {
        if (a == b) return;
        size_t i = 0;
        while (i < a.size() && i < b.size() && a[i] == b[i]) i++;
        const size_t from = i > 200 ? i - 200 : 0;
        Different(what, std::to_string(i) + "\n" + a.substr(from, 400) + "\n----\n" + b.substr(from, 400));
    }

    void Advance(Random& own)
    {
        const uint32_t kind = own.Below(40);
        rig_nanoseconds += kind == 0 ? 1000000000ull + own.Below(3) * own.Below(500000) * 1000ull
                         : kind == 1 ? 40000000ull + own.Below(900) * 1000000ull
                         : kind == 2 ? 24900000ull + own.Below(200) * 1000ull
                         : kind < 6 ? 26000000ull + own.Below(120) * 1000000ull
                         : 16000000ull + own.Below(1500000);
    }

    struct ReportCalls
    {
        void (*frame)(bool); void (*add)(int, uint64_t); void (*spent)(int, uint64_t); void (*shader)(int);
        void (*started)(const char*); void (*ended)(); void (*compiled)(const char*);
        uint64_t (*begin)(int); void (*end)(int, uint64_t); void (*write)();
    };

    void ReportScript(const ReportCalls& side, uint64_t seed, uint64_t cases)
    {
        Random own{ seed };
        rig_nanoseconds = (1ull << 50) + 1000000000ull;
        g_readings = 0;
        uint64_t began = 0;
        int open = -1;
        if (seed % 4 == 2) side.shader(-1);   // one, and no more
        for (uint64_t i = 0; i < cases; i++)
        {
            Advance(own);
            switch (own.Below(14))
            {
            case 0: side.add(own.Below(2), own.Below(3000000)); break;
            case 1: side.spent(own.Below(6), own.Below(9000000)); break;
            case 2: side.shader(seed & 1 ? int(own.Below(7)) - 3 : int(own.Below(2))); break;
            case 3:
                if (own.Below(8) == 0)
                {
                    char name[64];
                    std::snprintf(name, sizeof name, own.Below(2) ? "guest thread %u (routine 8228B2C8)" : "thread %u", own.Below(30));
                    side.started(name);
                }
                break;
            case 4: if (own.Below(32) == 0) side.ended(); break;
            case 5: if (own.Below(64) == 0) side.compiled(own.Below(4) ? "fresh" : ""); break;
            case 6:
                if (open < 0) { open = int(own.Below(6)); began = side.begin(open); }
                else { side.end(open, began); open = -1; }
                break;
            default: side.frame(own.Below(16) != 0); break;
            }
        }
        side.write();
        side.write();   // it writes once
    }

    void Report(uint64_t seed, uint64_t cases)
    {
        setenv("MW2_REPORT", "1", 1);
        const std::string a = Logged([&] {
            ReportScript({ cpp_mw2_report_frame, cpp_mw2_report_add, cpp_mw2_report_spent, cpp_mw2_report_shader_built,
                           cpp_mw2_report_thread_started, cpp_mw2_report_thread_ended, cpp_mw2_report_shaders_compiled,
                           cpp_mw2_report_timed_begin, cpp_mw2_report_timed_end, cpp_mw2_report_write }, seed, cases);
        });
        const std::string b = Logged([&] {
            ReportScript({ rig_report_frame, rig_report_add, rig_report_spent, rig_report_shader_built, rig_report_thread_started,
                           rig_report_thread_ended, rig_report_shaders_compiled, rig_report_timed_begin, rig_report_timed_end,
                           rig_report_write }, seed, cases);
        });
        Same("report", a, b);
        std::printf("report: %zu bytes of log\n", a.size());
    }

    struct StutterCalls
    {
        bool (*wanted)(); void (*add)(int, uint64_t); void (*presented)(); void (*finished)(bool);
        void (*shown)(uint64_t); void (*displayed)(uint64_t, uint32_t, double); void (*mark)(const char*); void (*report)();
    };

    void StutterScript(const StutterCalls& side, uint64_t seed, uint64_t cases)
    {
        Random own{ seed };
        rig_nanoseconds = (1ull << 50) + 1000000000ull;
        std::printf("%s", "");
        if (!side.wanted()) std::fprintf(stderr, "off\n");
        uint64_t serial = 1;
        for (uint64_t i = 0; i < cases; i++)
        {
            Advance(own);
            side.presented();
            for (uint32_t costs = own.Below(3); costs > 0; costs--)
                side.add(own.Below(7), own.Below(4) ? own.Below(400000) : own.Below(9000000));
            side.finished(own.Below(32) != 0);
            serial += own.Below(16) ? 1 : 2 + own.Below(4);
            if (own.Below(3)) side.shown(serial);
            if (own.Below(16) == 0) side.displayed(serial, own.Below(4), double(own.Below(90000)) / 1000.0);
            if (own.Below(64) == 0) side.mark(own.Below(2) ? "pad Y" : "F7");
        }
        side.report();
    }

    void Stutters(uint64_t seed, uint64_t cases)
    {
        setenv("MW2_STUTTERS", "1", 1);
        if (seed % 5 == 0) setenv("MW2_FPS_LIMIT", "90", 1);
        const uint64_t own = seed * 77;
        const std::string a = Logged([&] {
            StutterScript({ cpp_mw2_stutters_wanted, cpp_mw2_stutters_add, cpp_mw2_stutters_title_presented,
                            [](bool world) { cpp_mw2_stutters_finished(0, world); }, cpp_mw2_stutters_shown,
                            cpp_mw2_stutters_displayed, cpp_mw2_stutters_mark, cpp_mw2_stutters_report }, own, cases);
        });
        const std::string b = Logged([&] {
            StutterScript({ rig_stutters_wanted, rig_stutters_add, rig_stutters_title_presented, rig_stutters_finished,
                            rig_stutters_shown, rig_stutters_displayed, rig_stutters_mark, rig_stutters_report }, own, cases);
        });
        Same("stutters", a, b);
        std::printf("stutters: %zu bytes of log\n", a.size());
    }

    std::string TraceLines(const char* path)
    {
        std::ifstream in(path);
        std::string line, kept;
        while (std::getline(in, line))
            if (line.empty() || line[0] != '#') kept += line + "\n";
        return kept;
    }

    void Pacing(uint64_t seed, uint64_t cases)
    {
        const std::string path = "/tmp/mw2_rig_trace." + std::to_string(getpid());
        setenv("MW2_TRACE_PACING", path.c_str(), 1);
        // A chain of guest frames for the callers.
        constexpr uint32_t kFrames = 0x90200000;
        for (uint32_t i = 0; i < 6; i++)
        {
            Set32(kFrames + i * 0x100, i < 5 ? kFrames + (i + 1) * 0x100 : 0);
            Set32(kFrames + i * 0x100 - 8, 0x82100000 + i);
        }
        const auto script = [&](auto note, auto callers, auto write) {
            Random own{ seed };
            rig_nanoseconds = (1ull << 50) + 1000000000ull;
            for (uint64_t i = 0; i < cases; i++)
            {
                Advance(own);
                if (own.Below(8) == 0) callers(own.Below(4) ? kFrames + own.Below(6) * 0x100 : 0);
                else note("BESWRCKDkXFPp"[own.Below(13)], own.Below(1 << 31));
            }
            write();
        };
        const std::string a = Logged([&] { script(cpp_mw2_pacing_note, cpp_mw2_pacing_note_callers, cpp_mw2_pacing_write); });
        const std::string fileA = TraceLines(path.c_str());
        const std::string b = Logged([&] { script(rig_pacing_note, rig_pacing_note_callers, rig_pacing_write); });
        const std::string fileB = TraceLines(path.c_str());
        unlink(path.c_str());
        Same("pacing log", a, b);
        Same("pacing file", fileA, fileB);
        std::printf("pacing: %zu bytes of trace\n", fileA.size());
    }

    void SetFloat(uint32_t address, float value) { Set32(address, std::bit_cast<uint32_t>(value)); }

    void PlayerScript(uint64_t seed, uint64_t cases, uint32_t client, const std::vector<std::pair<float, float>>& places,
                      std::string& answers, auto wanted, auto walking, auto arrived, auto seconds, auto locate, auto steer,
                      auto sawPad, auto report)
    {
        Random own{ seed };
        rig_nanoseconds = (1ull << 50) + 1000000000ull;
        player::Where at{};
        float x = 0, y = 0, z = 0, yaw = 0, pitch = 0;
        uint32_t phase = 0, place = 0, stillFor = 0;
        // Half the seeds walk where the autopilot's sticks say, as the game would.
        const bool obeys = seed % 2;
        const float camera = seed % 3 ? 1200.5f : 0.0f;
        short sticks[4] = {};
        bool steered = false;
        for (uint64_t i = 0; i < cases; i++)
        {
            rig_nanoseconds += own.Below(obeys ? 256 : 8) ? 40000000ull + own.Below(60000000) : 1000000000ull + own.Below(1500000000);
            // Nothing there, the origin, the map's camera, then a player: who walks from
            // place to place, stops, is thrown somewhere else, and now and then reads as
            // a state being written.
            if (phase == 0 && own.Below(16) == 0 && seed % 7) phase = 1;
            if (phase == 1 && own.Below(16) == 0) { phase = 2; x = camera; y = -800.25f; z = 90; }
            if (phase == 2 && own.Below(64) == 0)
            {
                phase = 3;
                x = places[0].first + 20.0f;
                y = places[0].second - 30.0f;
                stillFor = 6;
            }
            if (phase == 3)
            {
                if (stillFor) stillFor--;
                else if (own.Below(obeys ? 256 : 48) == 0) stillFor = 2 + own.Below(200);
                else if (obeys && steered)
                {
                    // A second of stick is 200 units and 120 degrees, for 80 ms a poll.
                    constexpr float kRadians = 3.14159265f / 180.0f;
                    const float forward = float(sticks[1]) / 32768.0f * 14.0f, right = float(sticks[0]) / 32768.0f * 14.0f;
                    x += std::cos(yaw * kRadians) * forward + std::sin(yaw * kRadians) * right;
                    y += std::sin(yaw * kRadians) * forward - std::cos(yaw * kRadians) * right;
                    z += float(int(own.Below(3)) - 1) / 8.0f;
                    yaw -= float(sticks[2]) / 32768.0f * 8.0f;
                    pitch -= float(sticks[3]) / 32768.0f * 8.0f;
                    while (yaw > 360.0f) yaw -= 360.0f;
                    while (yaw < 0.0f) yaw += 360.0f;
                    while (pitch > 360.0f) pitch -= 360.0f;
                    while (pitch < 0.0f) pitch += 360.0f;
                }
                else
                {
                    const auto [toX, toY] = places[place % places.size()];
                    const float dx = toX - x, dy = toY - y, length = std::sqrt(dx * dx + dy * dy);
                    if (length < 90.0f) { if (own.Below(4)) place++; else place += own.Below(4); }
                    const float step = length > 1.0f ? 30.0f / length + float(own.Below(80)) / length : 0.0f;
                    x += dx * step + float(int(own.Below(9)) - 4);
                    y += dy * step + float(int(own.Below(9)) - 4);
                    z += float(int(own.Below(5)) - 2) / 4.0f;
                    yaw = float(own.Below(7200)) / 10.0f - 360.0f;
                    pitch = float(own.Below(3600)) / 10.0f;
                }
                switch (own.Below(obeys ? 2048 : 256))
                {
                case 0: x += 4000.0f; y -= 2500.0f; break;
                case 1: x += 1550.0f; break;
                case 2: y += 450.0f; break;
                case 3: x = 200000.0f; break;
                default: break;
                }
            }
            const bool valid = phase != 0 && own.Below(64) != 0;
            Set32(client + T_CLIENT_VALID, valid);
            SetFloat(client + T_CLIENT_ORIGIN, x);
            SetFloat(client + T_CLIENT_ORIGIN + 4, own.Below(128) ? y : __builtin_inff());
            SetFloat(client + T_CLIENT_ORIGIN + 8, own.Below(128) ? z : __builtin_nanf(""));
            SetFloat(client + T_CLIENT_ANGLES, pitch);
            SetFloat(client + T_CLIENT_ANGLES + 4, yaw);

            short axes[4] = { short(own.Next()), short(own.Next()), short(own.Next()), short(own.Next()) };
            int press = 77;
            sawPad(uint16_t(own.Below(16) ? 0 : own.Next()), uint8_t(own.Below(8) ? 0 : own.Next()));
            const bool wrote = steer(axes, &press);
            steered = wrote;
            std::copy(axes, axes + 4, sticks);
            player::Where here{};
            const bool found = locate(&here);
            char line[200];
            std::snprintf(line, sizeof line, "%d %d %d %d %d %d %d w%d%d a%d %.6f l%d %a %a %a %a %a\n", wrote, press, axes[0], axes[1],
                          axes[2], axes[3], int(i), wanted(), walking(), arrived(), seconds(), found, found ? here.x : 0,
                          found ? here.y : 0, found ? here.z : 0, found ? here.yaw : 0, found ? here.pitch : 0);
            answers += line;
        }
        report();
    }

    void Player(uint64_t seed, uint64_t cases)
    {
        Random setup{ seed * 991 };
        std::vector<std::pair<float, float>> places;
        const float spread = seed % 2 ? 900.0f : 6000.0f;
        for (uint32_t i = 0; i < (seed % 2 ? 8 : 24); i++)
            places.push_back({ float(setup.Below(uint32_t(spread))) - spread / 2, float(setup.Below(uint32_t(spread))) - spread / 2 });
        const std::string file = "/tmp/mw2_rig_route." + std::to_string(getpid()), other = file + "b";
        const auto route = [&](const std::string& path, size_t from) {
            std::string text = "# a route\n\n";
            for (size_t i = from; i < places.size(); i++)
            {
                const auto [x, y] = places[i];
                char line[120];
                switch (setup.Below(8))
                {
                case 0: std::snprintf(line, sizeof line, "left %.1f %.1f 12.5 %.2f %.2f\n", x, y, float(setup.Below(360)), float(setup.Below(360))); break;
                case 1: std::snprintf(line, sizeof line, "fire %.1f %.1f 12.5 %.2f %.2f\n", x, y, float(setup.Below(360)), float(setup.Below(360))); break;
                case 2: std::snprintf(line, sizeof line, "%.1f %.1f\n", x, y); break;
                case 3: std::snprintf(line, sizeof line, "%.1f %.1f 3.0 %.1f\n", x, y, float(setup.Below(360))); break;
                case 4: std::snprintf(line, sizeof line, "nonsense %.1f\nwalk %.1f %.1f 1 2 3\nleft %.1f %.1f 1 2\n", x, y, x, x, y); break;
                default: std::snprintf(line, sizeof line, "%.1f %.1f 3.0 %.1f %.1f\n", x, y, float(setup.Below(360)), float(setup.Below(360))); break;
                }
                text += line;
            }
            text += std::string(253, ' ') + "1234.5 -987.25 3 4 5\n";
            if (from == 0 && setup.Below(2)) text += setup.Below(2) ? "fire 300.5 -40.25 1 90.5 355\n" : "left 300.5 -40.25 1 90.5 355\n";
            text += std::string(300, 'x') + "\n";
            std::ofstream(path) << text;
        };
        route(file, 0);
        route(other, places.size() / 2);
        switch (seed % 8)
        {
        case 0: setenv("MW2_WALK_PATH", (file + "," + other + ",,/no/such/route").c_str(), 1); break;
        case 1: setenv("MW2_WALK_PATH", file.c_str(), 1); setenv("MW2_TEAM_UP", "2", 1); break;
        case 2: setenv("MW2_WALK_TO", "250.5,-300.25", 1); setenv("MW2_TRACE_VIEWPOS", "1", 1); break;
        case 3: setenv("MW2_WALK_TO", "-100,200,150,45.5", 1); setenv("MW2_WALK_PATH", file.c_str(), 1); setenv("MW2_WALK_PAUSE", "0.5", 1); break;
        case 4: setenv("MW2_WALK_PATH", other.c_str(), 1); setenv("MW2_WALK_REACH", "60", 1); setenv("MW2_TEAM_UP", "1", 1); break;
        case 5: setenv("MW2_TRACE_VIEWPOS", "1", 1); break;
        case 6: setenv("MW2_WALK_TO", "150,-120,200", 1); break;
        default: setenv("MW2_WALK_PATH", (other + "," + file).c_str(), 1); setenv("MW2_WALK_REACH", "60", 1); break;
        }
        constexpr uint32_t kClient = 0x90400000;
        Set32(T_DATA_ClientStates, kClient);
#ifdef T_CLIENT_STATES_ARE_HERE
        const uint32_t client = T_DATA_ClientStates;
#else
        const uint32_t client = kClient;
#endif
        std::string answersA, answersB;
        const uint64_t own = seed * 13;
        const std::string a = Logged([&] {
            PlayerScript(own, cases, client, places, answersA, cpp_mw2_player_wanted, cpp_mw2_player_walking, cpp_mw2_player_arrived,
                         cpp_mw2_player_seconds_since_progress, cpp_mw2_player_locate, cpp_mw2_player_steer, cpp_mw2_player_saw_pad,
                         cpp_mw2_player_report);
        });
        const std::string b = Logged([&] {
            PlayerScript(own, cases, client, places, answersB, rig_player_wanted, rig_player_walking, rig_player_arrived,
                         rig_player_seconds, rig_player_locate, rig_player_steer, rig_player_saw_pad, rig_player_report);
        });
        unlink(file.c_str());
        unlink(other.c_str());
        Same("player answers", answersA, answersB);
        Same("player log", a, b);
        std::printf("player: %zu bytes of log\n", a.size());
    }
}

namespace online
{
    // runtime_diff_online.cpp's service.cpp calls this where it would make Steam.
    std::unique_ptr<Backend> MakeRigSteam();
    std::unique_ptr<Backend> MakeRigLan();
}

// The rig's online service, the same on both sides: a friend list the script
// changes, and what is asked of it written down.
namespace
{
    online::FriendList g_friendList;
    std::string g_rigName;
    constexpr const char* kElsewhere[] = { "", "On another release", "In multiplayer", "In the campaign or Spec Ops" };

    bool RigInvite(uint64_t id)
    {
        g_said += "invite " + std::to_string(id) + "\n";
        return id % 3 != 0;
    }

    class RigBackend final : public online::Backend
    {
    public:
        explicit RigBackend(bool friends) : friends_(friends) {}
        bool Start() override { return true; }
        uint64_t LocalId() override { return 1; }
        uint64_t Account() override { return 2; }
        std::string LocalName() override { return g_rigName; }
        bool Send(uint64_t, uint16_t, uint16_t, const void*, size_t) override { return false; }
        bool Broadcast(uint16_t, uint16_t, const void*, size_t) override { return false; }
        bool Receive(uint16_t, online::Datagram&) override { return false; }
        void InviteFriends() override { g_said += "invite all\n"; }
        bool Friends(online::FriendList& list) override
        {
            if (!friends_) return false;
            if (list.version != g_friendList.version) list = g_friendList;
            return true;
        }
        bool InviteFriend(uint64_t id) override { return RigInvite(id); }
        bool Lobbies() override { return true; }

    private:
        bool friends_;
    };
}

std::unique_ptr<online::Backend> online::MakeRigSteam() { return std::make_unique<RigBackend>(true); }
std::unique_ptr<online::Backend> online::MakeRigLan() { return std::make_unique<RigBackend>(false); }

// What the Rust rig's service reads of the list (tools/runtime_diff.rs).
extern "C" uint64_t rig_friends_version() { return g_friendList.version; }
extern "C" void rig_friends_counts(size_t* playing, int* online, int* offline)
{
    *playing = g_friendList.playing.size();
    *online = g_friendList.online;
    *offline = g_friendList.offline;
}
extern "C" void rig_friend(size_t at, uint64_t* id, void* name, mw2_text_sink sink, bool* withYou, int* elsewhere, void* avatar,
                           uint32_t* side)
{
    const online::Friend& one = g_friendList.playing[at];
    *id = one.id;
    sink(name, one.name.data(), one.name.size());
    *withYou = one.withYou;
    *elsewhere = int(std::find(std::begin(kElsewhere), std::end(kElsewhere), one.elsewhere) - std::begin(kElsewhere));
    sink(avatar, reinterpret_cast<const char*>(one.avatar.data()), one.avatar.size());
    *side = one.avatarSide;
}
extern "C" bool rig_invite_friend(uint64_t id) { return RigInvite(id); }
extern "C" void rig_invite_friends() { g_said += "invite all\n"; }
extern "C" void rig_local_name(void* name, mw2_text_sink sink) { sink(name, g_rigName.data(), g_rigName.size()); }

// The kernel's side of the C++ screens.
void kernel::PostNotification(uint32_t id, uint32_t param) { g_said += "notify " + std::to_string(id) + " " + std::to_string(param) + "\n"; }
void kernel::MovePlayerData(uint64_t, uint64_t) { g_said += "move\n"; }

namespace
{
    struct ScreenCalls
    {
        bool (*start)();
        bool (*signinInput)(uint32_t, uint16_t), (*friendsInput)(uint32_t, uint16_t), (*guideInput)(uint32_t, uint16_t);
        void (*signinAsk)(), (*friendsAsk)();
        void (*at)(uint32_t, bool*, uint64_t*, void*, mw2_text_sink);
        void (*first)(uint64_t*, void*, mw2_text_sink);
        uint32_t (*mask)();
        bool (*pictures[3])(uint32_t, mw2_image*);
    };

    // A profile number as the order it was first seen in: both sides make theirs at
    // random.
    struct Numbers
    {
        std::map<uint64_t, size_t> seen;
        std::string Of(uint64_t id)
        {
            if (!id) return "0";
            return "#" + std::to_string(seen.emplace(id, seen.size() + 1).first->second);
        }
        std::string File(const std::filesystem::path& path)
        {
            std::ifstream in(path, std::ios::binary);
            std::string text(std::istreambuf_iterator<char>(in), {}), out;
            for (size_t at = 0; at < text.size();)
            {
                size_t end = text.find('\n', at);
                if (end == std::string::npos) end = text.size();
                std::string line = text.substr(at, end - at);
                at = end + 1;
                const bool first = line.rfind("first ", 0) == 0;
                const size_t from = first ? 6 : 0;
                if ((first || (line.size() > 12 && line[12] == ' ')) && line.size() >= from + 12)
                    line = line.substr(0, from) + Of(std::strtoull(line.substr(from, 12).c_str(), nullptr, 16)) + line.substr(from + 12);
                out += line + "\n";
            }
            return out;
        }
    };

    uint64_t Fnv(const uint8_t* data, size_t size)
    {
        uint64_t hash = 0xCBF29CE484222325ull;
        for (size_t i = 0; i < size; i++) hash = (hash ^ data[i]) * 0x100000001B3ull;
        return hash;
    }

    // One script of controllers, asks and pictures, as input.cpp and the presenter
    // make them: every answer, the pictures' bytes, what was said to the kernel and the
    // profiles file, with the profile numbers by order.
    std::string ScreenScript(uint64_t seed, uint64_t cases, const ScreenCalls& calls, const std::filesystem::path& root)
    {
        Random random{ seed * 0x5151 };
        Numbers numbers;
        g_friendList = {};
        std::string out = "started " + std::to_string(calls.start()) + "\n";
        screen::Image images[3];
        std::vector<uint8_t> pixels[3];
        // The guide's Invite friends, then the friends screen it opens, first.
        // Then Quit, asked twice, taken back, asked again, and the guide shut.
        constexpr uint16_t kOpening[] = { 0x0400, 0, 0x0002, 0, 0x1000, 0, 0x0002, 0x0001, 0x1000, 0, 0x2000, 0,
                                          0x0400, 0, 0x0001, 0, 0x1000, 0, 0x2000, 0, 0x1000, 0, 0x1000, 0, 0x0400, 0 };
        for (const uint16_t held : kOpening)
        {
            const bool picking = calls.friendsInput(0, held);
            const bool guided = calls.guideInput(0, picking ? uint16_t(0) : held) || picking;
            out += "opening " + std::to_string(picking) + std::to_string(calls.signinInput(0, guided ? uint16_t(0) : held) || guided);
            mw2_image c{ images[1].version, images[1].width, images[1].height, images[1].outline, &pixels[1], mw2_assign_bytes };
            out += std::to_string(calls.pictures[1](720, &c)) + std::to_string(c.version) + "\n";
            images[1] = {};
            pixels[1].clear();
        }
        for (uint64_t i = 0; i < cases; i++)
        {
            if (i == cases / 2)
            {
                // Halfway, the friends screen over a list of every kind: one to
                // invite, one already here, one elsewhere, one Steam refuses, then
                // the one chosen gone from the list, and back.
                online::FriendList& list = g_friendList;
                list.playing.clear();
                const char* kinds[][2] = { { "anna", "" }, { "bob", "" }, { "carl", "In multiplayer" }, { "dora", "" }, { "emil", "" },
                                           { "fred", "" }, { "gina", "On another release" } };
                for (size_t k = 0; k < 7; k++)
                {
                    online::Friend one;
                    one.id = k == 3 ? 9 : 10 + k;
                    one.name = kinds[k][0];
                    one.withYou = k == 1;
                    one.elsewhere = kinds[k][1];
                    list.playing.push_back(one);
                }
                list.online = 3;
                list.version++;
                // Whatever is open is shut first: B, twice, at every controller.
                for (int round = 0; round < 2; round++)
                    for (uint32_t user = 0; user < 4; user++)
                        for (const uint16_t held : { uint16_t(0x2000), uint16_t(0) })
                        {
                            const bool picking = calls.friendsInput(user, held);
                            const bool guided = calls.guideInput(user, picking ? uint16_t(0) : held) || picking;
                            calls.signinInput(user, guided ? uint16_t(0) : held);
                        }
                calls.friendsAsk();
                constexpr uint16_t kPresses[] = { 0x1000, 0x0002, 0x1000, 0x0002, 0x1000, 0x0002, 0x1000, 0x0001, 0x1000,
                                                  0x0002, 0x0002, 0x1000, 0xFFFF, 0x0001, 0x1000, 0x0002, 0x0002, 0x0002,
                                                  0x0002, 0xFFFE, 0x2000 };
                for (const uint16_t press : kPresses)
                {
                    if (press == 0xFFFF && !list.playing.empty())
                    {
                        // The one chosen, emil, goes.
                        list.playing.erase(list.playing.begin() + 4);
                        list.version++;
                    }
                    else if (press == 0xFFFE)
                    {
                        list.playing.clear();
                        list.version++;
                    }
                    else
                    {
                        out += "friends " + std::to_string(calls.friendsInput(0, press)) + std::to_string(calls.friendsInput(0, 0));
                        calls.guideInput(0, 0);
                        calls.signinInput(0, 0);
                    }
                    mw2_image c{ images[1].version, images[1].width, images[1].height, images[1].outline, &pixels[1], mw2_assign_bytes };
                    out += " " + std::to_string(calls.pictures[1](720, &c)) + " " + std::to_string(c.version) + " " +
                           std::to_string(Fnv(pixels[1].data(), pixels[1].size())) + "\n";
                    images[1].version = c.version;
                    images[1].width = c.width;
                    images[1].height = c.height;
                    images[1].outline = c.outline;
                }
            }
            const uint32_t what = random.Below(21);
            if (what == 20)
            {
                // The service's list changes: a friend more or less, a picture, a name.
                online::FriendList& list = g_friendList;
                constexpr const char* kNames[] = { "anna", "Bob", "carol the very long name of a friend", "Ünïcode", "bob", "", "Zed" };
                if (list.playing.size() < 9 && random.Below(2))
                {
                    online::Friend one;
                    one.id = 1 + random.Below(12);
                    one.name = kNames[random.Below(7)];
                    one.withYou = random.Below(5) == 0;
                    one.elsewhere = kElsewhere[random.Below(3) ? 0 : random.Below(4)];
                    constexpr uint32_t kSides[] = { 0, 1, 32, 64, 256 };
                    one.avatarSide = kSides[random.Below(5)];
                    one.avatar.resize(size_t(one.avatarSide) * one.avatarSide * 4 - (random.Below(9) ? 0 : std::min<size_t>(one.avatarSide, 1)));
                    for (uint8_t& byte : one.avatar) byte = uint8_t(random.Next());
                    list.playing.insert(list.playing.begin() + random.Below(uint32_t(list.playing.size() + 1)), one);
                }
                else if (!list.playing.empty())
                    list.playing.erase(list.playing.begin() + random.Below(uint32_t(list.playing.size())));
                list.online = int(random.Below(40));
                list.offline = int(random.Below(3) ? random.Below(400) : 1);
                list.version++;
                out += "list " + std::to_string(list.version) + "\n";
                continue;
            }
            if (what < 10)
            {
                constexpr uint16_t kButtons[] = { 0, 0, 0, 0x0001, 0x0002, 0x1000, 0x2000, 0x4000, 0x0010, 0x0020, 0x0400, 0x0400, 0x1001, 0x2400 };
                const uint32_t user = random.Below(5);
                const uint16_t held = kButtons[random.Below(14)];
                const bool picking = calls.friendsInput(user, held);
                const bool guided = calls.guideInput(user, picking ? uint16_t(0) : held) || picking;
                const bool taken = calls.signinInput(user, guided ? uint16_t(0) : held) || guided;
                out += "in " + std::to_string(user) + " " + std::to_string(held) + " " + std::to_string(picking) +
                       std::to_string(guided) + std::to_string(taken) + "\n";
            }
            else if (what < 12)
            {
                calls.signinAsk();
                out += "ask signin\n";
            }
            else if (what < 13)
            {
                calls.friendsAsk();
                out += "ask friends\n";
            }
            else if (what < 17)
            {
                constexpr uint32_t kHeights[] = { 0, 1, 200, 480, 720, 1080, 1440, 2160, 4320 };
                const uint32_t height = random.Below(3) ? kHeights[random.Below(9)] : random.Below(3000);
                const uint32_t which = random.Below(3);
                screen::Image& image = images[which];
                mw2_image c{ image.version, image.width, image.height, image.outline, &pixels[which], mw2_assign_bytes };
                const bool shown = calls.pictures[which](height, &c);
                image.version = c.version;
                image.width = c.width;
                image.height = c.height;
                image.outline = c.outline;
                char line[160];
                std::snprintf(line, sizeof line, "picture %u %u: %d %llu %ux%u %a %016llx\n", which, height, shown,
                              (unsigned long long)image.version, image.width, image.height, double(image.outline),
                              (unsigned long long)Fnv(pixels[which].data(), pixels[which].size()));
                out += line;
            }
            else if (what < 18)
            {
                bool signedIn = false;
                uint64_t id = 0;
                std::string name;
                const uint32_t user = random.Below(5);
                calls.at(user, &signedIn, &id, &name, mw2_assign_text);
                out += "at " + std::to_string(user) + " " + std::to_string(signedIn) + " " + numbers.Of(id) + " " + name + "\n";
            }
            else if (what < 19)
            {
                uint64_t id = 0;
                std::string name;
                calls.first(&id, &name, mw2_assign_text);
                out += "first " + numbers.Of(id) + " " + name + " mask " + std::to_string(calls.mask()) + "\n";
            }
            else
                g_rigTime += random.Below(2) ? 1 : 3600 + random.Below(100000);
        }
        return out + numbers.File(root / "profiles.txt");
    }

    void Screens(uint64_t seed, uint64_t cases)
    {
        const std::filesystem::path base = "/tmp/runtime_diff_screens_" + std::to_string(getpid());
        // Each side in a folder of its own, whose saves/ is its save root.
        g_cppRoot = base / "cpp" / "saves";
        g_rustRoot = base / "rust" / "saves";
        std::filesystem::remove_all(base);
        // A book of profiles, or none, or one another machine wrote, or one with
        // lines that are not profiles.
        Random setup{ seed * 77 };
        std::string book;
        switch (seed % 4)
        {
        case 0: break;
        case 1: book = "machine 0000000000000001\nfirst 0000000a0b0c\n0000000a0b0c Anna\n00000000beef Bob  \r\n00000000cafe  Cleo \n"; break;
        case 2: book = "first 00000000beef\n0000000a0b0c Anna\n00000000beef Bob with a name that is far too long\n"; break;
        default:
            book = "  \nfirst zz\n000000000000 zero\n0000000a0b0g bad\n0000000a0b0c\n00000000beefX\n+0000000beef plus\n";
            for (uint32_t i = 0, n = 7 + setup.Below(6); i < n; i++)
            {
                char line[64];
                std::snprintf(line, sizeof line, "%012llx Player %u\n", (unsigned long long)(0x10000 + setup.Below(100000)), setup.Below(9));
                book += line;
            }
        }
        for (const auto& root : { g_cppRoot, g_rustRoot })
        {
            std::filesystem::create_directories(root);
            if (!book.empty()) std::ofstream(root / "profiles.txt", std::ios::binary) << book;
        }
        if (seed % 3 == 0) setenv("MW2_NAME", "Rig Player With A Long Name", 1);
        // A third of the seeds have a service with friends to pick from, Steam's
        // place, and a third one without, LAN's.
        const uint32_t served = uint32_t(seed % 3);
        if (served) setenv("MW2_NETWORK", served == 2 ? "steam" : "lan", 1);
        g_rigName = seed % 4 == 2 ? "" : "Rig Steam Name";
        // Some keep the screens' pictures (MW2_DUMP_FRAMES), each side in a folder.
        const bool dumped = seed % 5 == 0;

        const ScreenCalls cpp{ cpp_mw2_online_started, cpp_mw2_signin_input, cpp_mw2_friends_input, cpp_mw2_guide_input, cpp_mw2_signin_ask, cpp_mw2_friends_ask,
                               cpp_mw2_signin_at, cpp_mw2_signin_first, cpp_mw2_signin_mask,
                               { cpp_mw2_signin_picture, cpp_mw2_friends_picture, cpp_mw2_guide_picture } };
        const ScreenCalls rust{ served == 2 ? rig_start_rig_steam : served == 1 ? rig_start_rig_lan : mw2_online_started, mw2_signin_input, mw2_friends_input, mw2_guide_input, mw2_signin_ask, mw2_friends_ask,
                                mw2_signin_at, mw2_signin_first, mw2_signin_mask,
                                { mw2_signin_picture, mw2_friends_picture, mw2_guide_picture } };
        const long time = g_rigTime;
        std::string answersA, answersB, saidA, saidB;
        g_said.clear();
        if (dumped) setenv("MW2_DUMP_FRAMES", (base / "dump-cpp").c_str(), 1);
        std::filesystem::create_directories(base / "dump-cpp");
        std::filesystem::create_directories(base / "dump-rust");
        std::filesystem::current_path(g_cppRoot.parent_path());
        const std::string a = Logged([&] { answersA = ScreenScript(seed, cases, cpp, g_cppRoot); });
        saidA.swap(g_said);
        g_rigTime = time;
        if (dumped) setenv("MW2_DUMP_FRAMES", (base / "dump-rust").c_str(), 1);
        std::filesystem::current_path(g_rustRoot.parent_path());
        const std::string b = Logged([&] { answersB = ScreenScript(seed, cases, rust, g_rustRoot); });
        saidB.swap(g_said);
        unsetenv("MW2_DUMP_FRAMES");
        std::string dumpsA, dumpsB;
        for (const auto& [folder, dumps] : { std::pair{ base / "dump-cpp", &dumpsA }, std::pair{ base / "dump-rust", &dumpsB } })
        {
            std::set<std::string> names;
            for (const auto& entry : std::filesystem::directory_iterator(folder)) names.insert(entry.path().filename().string());
            for (const std::string& name : names)
            {
                const std::vector<uint8_t> bytes = Read((folder / name).string());
                *dumps += name + " " + std::to_string(Fnv(bytes.data(), bytes.size())) + "\n";
            }
        }
        Same("screen dumps", dumpsA, dumpsB);
        if (dumped && dumpsA.empty()) Different("screen dumps", "none were kept");
        std::filesystem::current_path(base.parent_path());
        std::filesystem::remove_all(base);
        unsetenv("MW2_NAME");
        Same("screens", answersA, answersB);
        Same("screens said", saidA, saidB);
        Same("screens log", a, b);
        std::printf("screens: %zu answers, %zu bytes of log\n", size_t(std::count(answersA.begin(), answersA.end(), '\n')), a.size());
    }

    std::string Hex(Random& random, size_t digits)
    {
        std::string text;
        for (size_t i = 0; i < digits; i++) text += "0123456789abcdefABCDEF"[random.Below(random.Below(4) ? 16 : 22)];
        return text;
    }

    // A line made to be one, then broken a little, or not.
    std::string Mutated(Random& random, std::string text)
    {
        const uint32_t times = random.Below(3) ? 0 : 1 + random.Below(3);
        constexpr const char* kBits[] = { " ", "  ", ";", "\n", "-", "+", "0x", "x", "g", "/", ",", "=", "\t", "99999999999", "%",
                                          "a", "F", "0" };
        for (uint32_t i = 0; i < times && !text.empty(); i++)
        {
            const size_t at = random.Below(uint32_t(text.size() + 1));
            switch (random.Below(3))
            {
            case 0: text.insert(at, kBits[random.Below(18)]); break;
            case 1: if (at < text.size()) text.erase(at, 1 + random.Below(3)); break;
            default: if (at < text.size()) text[at] = kBits[random.Below(18)][0]; break;
            }
        }
        return text;
    }

    void Online(Random& random, uint64_t cases)
    {
        for (uint64_t i = 0; i < cases; i++)
        {
            // A host's connect line.
            std::string line = "connect " + Hex(random, 72) + " " + Hex(random, 32) + " " + Hex(random, 16) + " " +
                               std::to_string(random.Below(19)) + " " + std::to_string(random.Next() % (random.Below(2) ? 100 : 100000000000ull)) +
                               " " + Hex(random, 16);
            line = Mutated(random, line);
            if (rigcpp_is_connect_line(line.data(), line.size()) != rig_is_connect_line(line.data(), line.size()))
                Different("connect line", line);

            // A Find Game listing, as Rust writes one, read by both.
            uint8_t session[60];
            for (uint8_t& byte : session) byte = uint8_t(random.Next());
            uint32_t counts[4];
            for (uint32_t& count : counts) count = random.Below(4) ? random.Below(19) : random.Below(40);
            std::vector<uint32_t> pairs;
            for (uint32_t n = random.Below(12); n; n--)
            {
                pairs.push_back(random.Below(2) ? random.Below(40) : uint32_t(random.Next()));
                pairs.push_back(random.Below(2) ? random.Below(20) : uint32_t(random.Next()));
            }
            std::string text;
            rig_game_text(session, counts, pairs.data(), pairs.size() / 2, &text, mw2_assign_text);
            // What Rust wrote, the C++ reads back as it was listed, where it can.
            {
                uint64_t read[4] = {};
                uint8_t back[60] = {};
                std::vector<uint32_t> numbers(2 * pairs.size() + 2);
                size_t count = 0;
                const bool fits = std::all_of(counts, counts + 4, [](uint32_t n) { return n <= 18; });
                if (rigcpp_read_game(text.c_str(), read, back, numbers.data(), &count) != (fits && text.size() <= 1024))
                    Different("game text read back", text);
                std::map<uint32_t, uint32_t> listed;
                for (size_t n = 0; n < pairs.size(); n += 2) listed[pairs[n]] = pairs[n + 1];
                std::map<uint32_t, uint32_t> got;
                for (size_t n = 0; n < count; n++) got[numbers[2 * n]] = numbers[2 * n + 1];
                if (fits && (std::memcmp(back, session, 60) || got != listed || read[0] != counts[0] || read[1] != counts[1] ||
                             read[2] != counts[2] || read[3] != counts[3]))
                    Different("game text's values read back", text);
            }
            if (random.Below(2)) text = Mutated(random, text);
            if (!random.Below(20)) text += std::string(1024 - std::min<size_t>(text.size(), 1024) + random.Below(3), ',');
            uint64_t countsA[4] = {}, countsB[4] = {};
            uint8_t sessionA[60] = {}, sessionB[60] = {};
            std::vector<uint32_t> numbersA(2 * text.size()), numbersB(2 * text.size());
            size_t countA = 0, countB = 0;
            const bool readA = rigcpp_read_game(text.c_str(), countsA, sessionA, numbersA.data(), &countA);
            const bool readB = rig_read_game(text.c_str(), countsB, sessionB, numbersB.data(), &countB);
            if (readA != readB) Different("read game", text);
            if (readA && (std::memcmp(countsA, countsB, sizeof countsA) || std::memcmp(sessionA, sessionB, 60) || countA != countB ||
                          !std::equal(numbersA.begin(), numbersA.begin() + 2 * countA, numbersB.begin())))
                Different("read game's values", text);

            const std::string hex = Mutated(random, Hex(random, 120 + (random.Below(8) ? 0 : random.Below(5)) - 2));
            if (rigcpp_read_session(hex.c_str(), sessionA) != rig_read_session(hex.c_str(), sessionB) || std::memcmp(sessionA, sessionB, 60))
                Different("read session", hex);

            constexpr const char* kPlaying[] = { "mp:", "sp:", "xx:", "", "mp", "sp:", "mp:" };
            std::string playing = kPlaying[random.Below(7)];
            if (random.Below(2)) playing += std::to_string(mw2_version_network()) + (random.Below(4) ? "" : "-tu0");
            playing = Mutated(random, playing);
            char elsewhereA[64], elsewhereB[64];
            rigcpp_elsewhere(playing.c_str(), elsewhereA, sizeof elsewhereA);
            rig_elsewhere(playing.c_str(), elsewhereB, sizeof elsewhereB);
            if (std::strcmp(elsewhereA, elsewhereB)) Different("elsewhere", playing);
        }
    }
}

int main(int, char** argv)
{
    const uint64_t seed = std::strtoull(argv[1], nullptr, 10), cases = std::strtoull(argv[2], nullptr, 10);
    const std::string root = argv[3];
    Random random{ seed * 0x12345 };
    mw2_guest_base = static_cast<uint8_t*>(mmap(nullptr, 1ull << 32, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));

    if (std::strcmp(cpp_mw2_version_release(), mw2_version_release()) || cpp_mw2_version_network() != mw2_version_network()) Different("version");
    Crypto(random, cases / 4);
    Sizes(random, cases * 4);
    Settings(random, cases / 4, "/tmp");
    for (uint64_t i = 0; i < cases; i++) MadeUpXex(random);
    if (seed == 1)
        for (const char* file : { "/mw2/default.xex", "/mw2/default_mp.xex", "/mw2/tu0/default.xex", "/mw2/tu0/default_mp.xex" })
            CompareXex(Read(root + file), file);
    Presence(random, cases, seed == 1, root);
    Shadows(random, cases);
    Console(random, cases);
    Report(seed, cases * 8);
    Stutters(seed, cases * 8);
    Pacing(seed, cases);
    Player(seed, cases * 4);
    Screens(seed, cases);
    Online(random, cases * 4);
    std::printf("same: seed %llu\n", (unsigned long long)seed);
    return 0;
}
