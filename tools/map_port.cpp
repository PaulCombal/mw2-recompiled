// The launcher's map converter (launcher/mapport/) from a terminal, and the
// checks it is built against.
//
//     map-port roundtrip <file.ff>...    read each zone as a tree and write it back
//                                        unchanged: it must give the same bytes
//     map-port list <file.ff>            every asset of the asset list: type, struct, name
//     map-port fit <file.iwd> <picture> <limit> <out>
//                                        the picture as the converter resizes one
//                                        the 360 can't take as it is: its top level
//     map-port canonical <file.ff> <zone> write the tree as converted files are
//                                        written (from members, padding zero) and
//                                        compare with a zone
//     map-port iwi <file.iwd> <picture> <out>
//                                        a picture as the converter reads it: every
//                                        level, largest first, one after another
//     map-port xma <file.wav> <out>     a sound encoded as the converter encodes
//                                        one: the packets, and on the standard
//                                        output its length, end and seek table
//     map-port port <map.ff> <out dir> <stock dir> [PC game folder]
//                                        convert a PC map with the stock 360 files
//                                        in a folder (the game's), and the PC
//                                        game's pictures and sounds
#include "../launcher/mapport/fastfile.h"
#include "../launcher/mapport/images.h"
#include "../launcher/mapport/port.h"
#include "../launcher/mapport/zip.h"
#include "../launcher/mapport/query.h"
#include "../launcher/mapport/reader.h"
#include "../launcher/mapport/xma.h"
#include "../launcher/zone/rules.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iterator>

namespace
{
    using namespace mapport;

    double Seconds(std::chrono::steady_clock::time_point since)
    {
        return std::chrono::duration<double>(std::chrono::steady_clock::now() - since).count();
    }

    int Roundtrip(int argc, char** argv)
    {
        int failed = 0;
        for (int k = 0; k < argc; k++)
        {
            const auto t0 = std::chrono::steady_clock::now();
            try
            {
                Arena arena;
                ArenaScope scope(arena);
                const Fastfile ff = ReadFastfile(argv[k]);
                const Platform& plat = ff.pc ? Pc() : Xbox();
                const zone::Schema& rules = ff.pc ? zone::PcRules() : zone::XboxRules();
                const Tree tree = ReadTree(rules, plat, ff.zone, false);
                const double read = Seconds(t0);
                TreeWriter w(tree, rules, plat);
                w.keepBytes = true;
                const std::vector<uint8_t> out = w.Write(&tree.header);
                size_t diff = 0;
                while (diff < std::min(out.size(), ff.zone.size()) && out[diff] == ff.zone[diff]) diff++;
                const bool same = out == ff.zone;
                std::printf("%s: %s, %zu assets, %zu nodes, read %.1f s, written %.1f s: %s", argv[k],
                            ff.pc ? "pc" : "xbox", tree.assets.size(), arena.Count(), read, Seconds(t0) - read,
                            same ? "the same bytes" : "DIFFERENT");
                if (!same) std::printf(" (first at %zu of %zu / %zu)", diff, out.size(), ff.zone.size());
                std::printf("\n");
                failed += !same;
            }
            catch (const std::exception& e)
            {
                std::printf("%s: %s\n", argv[k], e.what());
                failed++;
            }
        }
        return failed ? 1 : 0;
    }

    std::vector<uint8_t> ReadAll(const char* path)
    {
        std::ifstream in(path, std::ios::binary);
        if (!in) throw Error(std::string("cannot open ") + path);
        return std::vector<uint8_t>(std::istreambuf_iterator<char>(in), {});
    }

    // The first difference between two zones, and how many bytes differ.
    void Compare(const std::vector<uint8_t>& ours, const std::vector<uint8_t>& theirs)
    {
        size_t first = SIZE_MAX, count = 0;
        for (size_t k = 0; k < std::min(ours.size(), theirs.size()); k++)
            if (ours[k] != theirs[k])
            {
                if (first == SIZE_MAX) first = k;
                count++;
            }
        if (ours.size() == theirs.size() && !count)
        {
            std::printf("the same bytes (%zu)\n", ours.size());
            return;
        }
        std::printf("DIFFERENT: %zu bytes against %zu, %zu differ, the first at %zu\n", ours.size(), theirs.size(), count,
                    first);
        if (first != SIZE_MAX)
        {
            const size_t from = first & ~size_t(15);
            for (const auto* z : { &ours, &theirs })
            {
                std::printf("  %s %08zx:", z == &ours ? "ours  " : "theirs", from);
                for (size_t k = from; k < std::min(z->size(), from + 32); k++) std::printf(" %02x", (*z)[k]);
                std::printf("\n");
            }
        }
    }

    int List(const char* file)
    {
        Arena arena;
        ArenaScope scope(arena);
        const Fastfile ff = ReadFastfile(file);
        const Tree tree = ReadTree(ff.pc ? zone::PcRules() : zone::XboxRules(), ff.pc ? Pc() : Xbox(), ff.zone,
                                   std::string(file).find("common_mp") != std::string::npos);
        for (size_t k = 0; k < tree.assets.size(); k++)
        {
            const Entry* e = tree.assets[k];
            const Struct* a = As<Struct>(e->asset);
            std::printf("%4zu %-14s %-22s %s\n", k, e->type.c_str(), a ? std::string(a->asset).c_str() : "(alias)",
                        a ? Name(a).value_or("-").c_str() : "");
        }
        return 0;
    }

    int Fit(const char* iwd, const std::string& picture, int limit, const char* out)
    {
        const Zip zip(iwd);
        for (size_t k = 0; k < zip.Names().size(); k++)
        {
            std::string name = zip.Names()[k];
            std::replace(name.begin(), name.end(), '\\', '/');
            if (Lower(name) != Lower("images/" + picture + ".iwi")) continue;
            const Picture pic = ReadIwi(zip.Read(k));
            const Fitted fit = FitPicture(pic.format, pic.width, pic.height, pic.mips, limit);
            std::ofstream(out, std::ios::binary)
                .write(reinterpret_cast<const char*>(fit.mips[0].data()), std::streamsize(fit.mips[0].size()));
            std::printf("%s: %dx%d -> %dx%d, %zu bytes\n", picture.c_str(), pic.width, pic.height, fit.width, fit.height,
                        fit.mips[0].size());
            return 0;
        }
        std::fprintf(stderr, "%s has no picture %s\n", iwd, picture.c_str());
        return 1;
    }

    int EncodeSound(const char* wav, const char* out)
    {
        const std::vector<uint8_t> raw = ReadAll(wav);
        const SoundPcm sound = ReadSound(raw);
        if (!sound.pcm)
        {
            std::fprintf(stderr, "%s: %s\n", wav, sound.compressed ? "not 16-bit PCM" : "no audio");
            return 1;
        }
        const Xma x = EncodeXma(sound.pcm->samples, sound.pcm->rate, sound.pcm->channels);
        std::ofstream(out, std::ios::binary).write(reinterpret_cast<const char*>(x.data.data()), std::streamsize(x.data.size()));
        std::printf("%u samples %u Hz, %u ms, end bit %llu, seek", x.samples, x.rate, x.Ms(),
                    static_cast<unsigned long long>(x.endBit));
        for (uint32_t s : x.seek) std::printf(" %u", s);
        std::printf("\n");
        return 0;
    }

    int Iwi(const char* iwd, const std::string& picture, const char* out)
    {
        const Zip zip(iwd);
        for (size_t k = 0; k < zip.Names().size(); k++)
        {
            std::string name = zip.Names()[k];
            std::replace(name.begin(), name.end(), '\\', '/');
            if (Lower(name) != Lower("images/" + picture + ".iwi")) continue;
            const Picture pic = ReadIwi(zip.Read(k));
            std::ofstream file(out, std::ios::binary);
            size_t bytes = 0;
            for (const Mip& m : pic.mips)
            {
                file.write(reinterpret_cast<const char*>(m.data()), std::streamsize(m.size()));
                bytes += m.size();
            }
            std::printf("%s: %s %dx%d, %zu levels, %zu bytes\n", picture.c_str(), FormatOf(pic.format).name, pic.width,
                        pic.height, pic.mips.size(), bytes);
            return 0;
        }
        std::fprintf(stderr, "%s has no picture %s\n", iwd, picture.c_str());
        return 1;
    }

    int Canonical(const char* file, const char* expected)
    {
        Arena arena;
        ArenaScope scope(arena);
        const Fastfile ff = ReadFastfile(file);
        const Platform& plat = ff.pc ? Pc() : Xbox();
        const zone::Schema& rules = ff.pc ? zone::PcRules() : zone::XboxRules();
        const Tree tree = ReadTree(rules, plat, ff.zone, false);
        TreeWriter w(tree, rules, plat);
        const std::vector<uint8_t> out = w.Write();
        const std::vector<uint8_t> want = ReadAll(expected);
        Compare(out, want);
        return out == want ? 0 : 1;
    }

    int Port(const char* pc, const char* out, const char* stockDir, const char* gameDir)
    {
        const auto t0 = std::chrono::steady_clock::now();
        std::vector<std::filesystem::path> stock;
        for (const auto& e : std::filesystem::directory_iterator(stockDir))
            if (e.path().extension() == ".ff") stock.push_back(e.path());
        std::sort(stock.begin(), stock.end());
        const bool timed = std::getenv("MAP_PORT_TIMES") != nullptr;
        PortOptions options;
        if (gameDir) options.gameIwds = GameIwds(gameDir);
        const auto written = PortMap(PcMap::Beside(pc), out, stock, [&](const std::string& line) {
            if (timed) std::printf("%6.1f ", Seconds(t0));
            std::printf("%s\n", line.c_str());
        }, options);
        std::printf("WRITTEN %zu files in %.1f s\n", written.written.size(), Seconds(t0));
        return 0;
    }
}

int main(int argc, char** argv)
{
    try
    {
        if (argc >= 3 && !std::strcmp(argv[1], "roundtrip")) return Roundtrip(argc - 2, argv + 2);
        if (argc == 4 && !std::strcmp(argv[1], "canonical")) return Canonical(argv[2], argv[3]);
        if (argc == 3 && !std::strcmp(argv[1], "list")) return List(argv[2]);
        if (argc == 6 && !std::strcmp(argv[1], "fit")) return Fit(argv[2], argv[3], std::atoi(argv[4]), argv[5]);
        if (argc == 5 && !std::strcmp(argv[1], "iwi")) return Iwi(argv[2], argv[3], argv[4]);
        if (argc == 4 && !std::strcmp(argv[1], "xma")) return EncodeSound(argv[2], argv[3]);
        if ((argc == 5 || argc == 6) && !std::strcmp(argv[1], "port"))
            return Port(argv[2], argv[3], argv[4], argc == 6 ? argv[5] : nullptr);
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "%s\n", e.what());
        return 1;
    }
    std::fprintf(stderr, "usage: map-port roundtrip <file.ff>...\n"
                         "       map-port canonical <file.ff> <zone>\n"
                         "       map-port fit <file.iwd> <picture> <limit> <out>\n"
                         "       map-port list <file.ff>\n"
                         "       map-port iwi <file.iwd> <picture> <out>\n"
                         "       map-port xma <file.wav> <out>\n"
                         "       map-port port <map.ff> <out dir> <stock dir> [PC game folder]\n");
    return 2;
}
