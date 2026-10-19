// The launcher's fastfile reader (launcher/zone/) on the files given -- fastfiles,
// or executables (.xex) for the shaders they carry themselves: for each,
// how many shaders it holds, how long the walk took, and with --hashes <dir>
// one "<pixel> <hash>" line per shader in <dir>/<file>.hashes, sorted, to
// compare with another reader's; with --dump <dir> each shader's microcode, named
// as MW2_DUMP_SHADERS names the ones a run draws with.
#include "../launcher/zone/fastfile.h"
#include "../launcher/zone/shaders.h"
#include "../launcher/zone/vertex_binding.h"
#include "../runtime/install/xex.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <optional>
#include <set>
#include <string>

int main(int argc, char** argv)
{
    const char* hashes = nullptr;
    const char* dump = nullptr;   // --dump <dir>: each shader's microcode, big-endian, as MW2_DUMP_SHADERS names it
    int failed = 0;
    std::set<std::pair<bool, uint64_t>> all;
    // Vertex shaders are bound with tables from the executables, which come first.
    std::optional<zone::BindTables> tables;
    zone::BindCounts counts;
    for (int i = 1; i < argc; i++)
    {
        if (!std::strcmp(argv[i], "--hashes") && i + 1 < argc)
        {
            hashes = argv[++i];
            continue;
        }
        if (!std::strcmp(argv[i], "--dump") && i + 1 < argc)
        {
            dump = argv[++i];
            continue;
        }
        const std::filesystem::path path(argv[i]);
        try
        {
            const auto from = std::chrono::steady_clock::now();
            // An executable (.xex) carries a few shaders of its own.
            std::vector<uint8_t> zone;
            std::vector<zone::Shader> shaders;
            if (path.extension() == ".xex")
            {
                std::ifstream file(path, std::ios::binary);
                const std::vector<uint8_t> xex((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
                std::string error;
                if (!install::xex::Image(xex, zone, error)) throw std::runtime_error(error);
                const bool multiplayer = path.filename() == "default_mp.xex";
                const zone::BindTables read = zone::ReadBindTables(zone, multiplayer);
                shaders = zone::ExecutableShaders(zone, multiplayer, read);
                if (tables && std::memcmp(&*tables, &read, sizeof read))
                    throw std::runtime_error("the two executables bind vertex shaders with different tables");
                tables = read;
            }
            else
            {
                if (!tables) throw std::runtime_error("give the executables (.xex) before the fastfiles");
                zone = zone::ReadFastfile(path);
                shaders = zone::Shaders(zone, *tables, &counts);
            }
            const double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - from).count();
            size_t pixel = 0;
            for (const zone::Shader& s : shaders)
            {
                pixel += s.pixel;
                all.insert({ s.pixel, s.hash });
            }
            std::printf("%s: %zu shaders (%zu vertex, %zu pixel), %zu bytes, %.0f ms\n",
                        path.filename().string().c_str(), shaders.size(), shaders.size() - pixel, pixel, zone.size(), ms);
            if (dump)
                for (const zone::Shader& s : shaders)
                {
                    char name[600];
                    std::snprintf(name, sizeof name, "%s/%s_%016llx.ucode", dump, s.pixel ? "pixel" : "vertex",
                                  (unsigned long long)s.hash);
                    if (std::FILE* f = std::fopen(name, "wb"))
                    {
                        for (uint32_t w : s.code)
                        {
                            const uint8_t be[4] = { uint8_t(w >> 24), uint8_t(w >> 16), uint8_t(w >> 8), uint8_t(w) };
                            std::fwrite(be, 1, 4, f);
                        }
                        std::fclose(f);
                    }
                }
            if (hashes)
            {
                std::sort(shaders.begin(), shaders.end(), [](const zone::Shader& a, const zone::Shader& b) {
                    return std::make_pair(a.pixel, a.hash) < std::make_pair(b.pixel, b.hash);
                });
                const std::string out = std::string(hashes) + "/" + path.stem().string() + ".hashes";
                if (std::FILE* f = std::fopen(out.c_str(), "w"))
                {
                    for (const zone::Shader& s : shaders)
                        std::fprintf(f, "%d %016llX\n", int(s.pixel), (unsigned long long)s.hash);
                    std::fclose(f);
                }
            }
        }
        catch (const std::exception& e)
        {
            std::printf("%s: %s\n", path.filename().string().c_str(), e.what());
            failed++;
        }
    }
    std::printf("%zu distinct shaders, %d file%s failed; vertex shaders: %u bound, %u never bound, %u with no"
                " declaration for their slot, %u a pass's own\n",
                all.size(), failed, failed == 1 ? "" : "s", counts.bound, counts.unbound, counts.noDeclaration,
                counts.ownShader);
    return failed ? 1 : 0;
}
