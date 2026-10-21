// The conversion of one map from start to end: which stock files it reads, the Porter's run, and the file written.
#include "port.h"
#include "paths.h"

#include "fastfile.h"

#include <algorithm>
#include <fstream>
#include <future>
#include <thread>
#include <map>
#include <memory>
#include <regex>

namespace mapport
{
    namespace fs = std::filesystem;

    // ---- the game's pools ---------------------------------------------------

    namespace
    {
        // How many assets of each kind the game holds at once (TU6
        // default_mp.xex, the table its "Exceeded limit of %d '%s' assets"
        // check reads), shared by the map and the files loaded with it.
        const std::vector<std::pair<const char*, int>>& Pools()
        {
            static const std::vector<std::pair<const char*, int>> kPools = {
                { "PhysPreset", 64 },      { "PhysCollmap", 1024 },         { "XAnimParts", 4096 },
                { "XModelSurfs", 4096 },   { "XModel", 1536 },              { "Material", 4096 },
                { "MaterialPixelShader", 8096 }, { "MaterialTechniqueSet", 768 }, { "GfxImage", 3584 },
                { "snd_alias_list_t", 16000 }, { "SndCurve", 64 },           { "LoadedSound", 1350 },
                { "GfxLightDef", 32 },     { "FxEffectDef", 600 },          { "FxImpactTable", 4 },
                { "RawFile", 1024 },       { "StringTable", 400 },          { "TracerDef", 32 },
            };
            return kPools;
        }
    }

    int PoolLimit(std::string_view type)
    {
        for (const auto& [t, n] : Pools())
            if (type == t) return n;
        return 0;
    }

    std::map<std::string, std::set<std::string>> PoolNames(const std::vector<Entry*>& assets)
    {
        std::map<std::string, std::set<std::string>> out;
        ForEach(assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || !PoolLimit(o->asset)) return;
            const std::optional<std::string> name = PoolName(o);
            if (Named(name) && !OnlyNamed(name)) out[std::string(o->asset)].insert(Lower(*name));
        });
        return out;
    }

    namespace
    {
        constexpr int kTextureBudgetMb = 40;
        constexpr size_t kLevels = 4;   // pak table entries per picture

        std::string BaseOf(const fs::path& p) { return Lower(Utf8(p.stem())); }
        bool Resident(const std::string& base) { return base == "code_post_gfx_mp" || base == "common_mp"; }

        std::string ReadText(const fs::path& path)
        {
            std::ifstream in(path, std::ios::binary);
            return std::string(std::istreambuf_iterator<char>(in), {});
        }

        // The teams the map wants: given, from its .arena, or the game's
        // defaults for a map it doesn't know.
        Teams MapTeams(const std::optional<fs::path>& arena, const std::optional<Teams>& teams)
        {
            if (teams) return *teams;
            if (arena && fs::exists(*arena))
            {
                const std::string text = ReadText(*arena);
                static const std::regex pair(R"re((\w+)[ \t]+"?([^"\s]*)"?)re");
                std::map<std::string, std::string> kv;
                for (std::sregex_iterator it(text.begin(), text.end(), pair), end; it != end; ++it)
                    kv[(*it)[1].str()] = (*it)[2].str();
                if (kv.count("allieschar") && kv.count("axischar")) return { kv["allieschar"], kv["axischar"] };
            }
            return { "us_army", "opforce_composite" };
        }

        Struct* AssetNamed(const Tree& t, const std::string& type, const std::string& name)
        {
            for (Entry* e : t.assets)
                if (Struct* a = As<Struct>(e->asset); a && e->type == type && Name(a) == name) return a;
            return nullptr;
        }

        // The entity string of a tree ("" without one).
        std::string EntityText(const Tree& t)
        {
            std::string out;
            bool found = false;
            ForEach(t.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (found || !o || !o->IsAsset("MapEnts")) return;
                Node* c = o->Child("entityString");
                if (Ref* r = As<Ref>(c)) c = r->target;
                if (Leaf* lf = As<Leaf>(c))
                {
                    found = true;
                    out.assign(lf->raw.begin(), std::find(lf->raw.begin(), lf->raw.end(), uint8_t(0)));
                }
            });
            return out;
        }

        // ---- the 360's destructible script ------------------------------------

        // {destructible type: (names its 360 script function uses, colours that
        // function is called with)} for the types wanted, read from the 360's
        // common_scripts/_destructible_types.gsc: the function each type's case
        // calls, with the colour it's given put into the names it builds.
        std::map<std::string, std::pair<std::set<std::string>, std::set<std::string>>> DestructibleNames(
            const std::string& text, const std::set<std::string>& wanted)
        {
            static const std::regex caseRe(R"re(case\s+"([^"]+)"\s*:\s*(\w+)\s*\(\s*(?:"([^"]*)")?\s*\)\s*;)re");
            std::map<std::string, std::pair<std::string, std::optional<std::string>>> cases;
            std::map<std::string, std::set<std::string>> colours;
            for (std::sregex_iterator it(text.begin(), text.end(), caseRe), end; it != end; ++it)
            {
                const auto& m = *it;
                cases.emplace(Lower(m[1].str()), std::pair{ m[2].str(), m[3].matched ? std::optional(m[3].str()) : std::nullopt });
            }
            for (const auto& [t, c] : cases)
                if (c.second) colours[c.first].insert(*c.second);
            std::map<std::string, std::pair<std::set<std::string>, std::set<std::string>>> out;
            for (const std::string& t : wanted)
            {
                auto hit = cases.find(Lower(t));
                if (hit == cases.end()) continue;
                const auto& [f, arg] = hit->second;
                const std::regex head("(^|\\n)" + f + "\\s*\\(\\s*(\\w*)\\s*\\)\\s*\\n\\{");
                std::smatch m;
                if (!std::regex_search(text, m, head)) continue;
                const size_t from = size_t(m.position(0) + m.length(0));
                const size_t to = text.find("\n}", from);
                const std::string body = text.substr(from, to == std::string::npos ? std::string::npos : to - from);
                const std::string param = m[2].str();
                const std::string part = "\"[^\"\\n]*\"" + (param.empty() ? std::string() : "|\\b" + param + "\\b");
                const std::regex expr("(?:" + part + ")(?:\\s*\\+\\s*(?:" + part + "))*"), piece(part);
                std::set<std::string> names;
                for (std::sregex_iterator e(body.begin(), body.end(), expr), end; e != end; ++e)
                {
                    const std::string whole = e->str();
                    std::vector<std::string> pieces;
                    for (std::sregex_iterator p(whole.begin(), whole.end(), piece); p != end; ++p) pieces.push_back(p->str());
                    if (std::none_of(pieces.begin(), pieces.end(), [](const std::string& p) { return p[0] == '"'; })) continue;
                    std::string name;
                    for (const std::string& p : pieces) name += p[0] == '"' ? p.substr(1, p.size() - 2) : arg.value_or("");
                    names.insert(name);
                }
                out[t] = { names, colours.count(f) ? colours[f] : std::set<std::string>{} };
            }
            return out;
        }

        std::set<std::string> DestructibleTypes(const Tree& pc)
        {
            const std::string text = EntityText(pc);
            static const std::regex type(R"re("destructible_type"\s+"([^"]+)")re");
            std::set<std::string> out;
            for (std::sregex_iterator it(text.begin(), text.end(), type), end; it != end; ++it) out.insert((*it)[1].str());
            return out;
        }

        // What a stock file holds that a conversion looks things up in without
        // taking anything from it, worked out from the file as it is.
        struct StockScan
        {
            std::set<std::string> names;                       // every name in its zone (text between two NULs)
            std::set<std::string> techsets, models, aliases;   // its pools' names, lower case
            // The pictures it streams from the console's picture packs (a header
            // and where its levels are, no pixels), by lower-case name.
            std::vector<std::pair<std::string, Struct*>> streamed;
        };

        class MapPort
        {
        public:
            MapPort(const std::vector<fs::path>& stock, const std::vector<fs::path>& game, const Log& log) : log(log)
            {
                for (const fs::path& p : stock) byName[Lower(Utf8(p.filename()))] = p;
                for (const fs::path& p : game) gameIwds.emplace_back(p);
            }

            Ported Run(const PcMap& map, const fs::path& outDir, std::optional<Teams> teams);

        private:
            struct Conversion
            {
                fs::path pc, out;
                std::optional<fs::path> arena;
                std::vector<fs::path> refs, fx;
                std::set<fs::path> donors;
                std::optional<Teams> teams;
                int textureBudget = 0;
                // The menus' picture of the map, made of its loading screen.
                struct Picture
                {
                    std::string map;
                    fs::path file;
                };
                std::optional<Picture> preview;
            };
            void Port(Conversion c);
            void Write(const Tree& t, const fs::path& to);
            void Load(const std::vector<fs::path>& paths, bool keep, const std::set<fs::path>& donors);
            StockFile* Read(const fs::path& p, const std::set<fs::path>& donors);
            const StockScan& Scan(const fs::path& p);
            std::vector<fs::path> EffectDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                               const std::vector<fs::path>& candidates);
            std::vector<fs::path> TechsetDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                                const std::vector<fs::path>& candidates);
            std::vector<Breakable> DestructibleDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                                      const std::vector<fs::path>& stock, const std::set<fs::path>& donors);
            std::vector<std::pair<std::string, fs::path>> DestructibleSoundDonors(const Tree& pc,
                                                                                  const std::vector<fs::path>& refs,
                                                                                  const std::vector<fs::path>& stock,
                                                                                  const std::set<fs::path>& donors);
            std::optional<std::string> DestructibleScript(const std::vector<fs::path>& refs, const std::set<fs::path>& donors);
            void DropPcTables(Tree& t);
            void CheckPools(const Tree& t, const std::vector<StockFile*>& refs, Porter& porter);

            const Log& log;
            std::map<std::string, fs::path> byName;   // the stock files by lower-case file name
            std::optional<fs::path> iwd;
            std::vector<Zip> gameIwds;
            Arena arena;   // the PC files' trees and what the conversion makes
            ArenaScope scope{ arena };
            std::map<fs::path, std::unique_ptr<StockFile>> loaded;
            std::map<fs::path, StockScan> scans;
            Ported ported;
        };

        // A stock 360 file's tree, each picture whose pixels are in the disc's
        // picture packs marked with its pak entries. The always-loaded files
        // are only looked things up in: animation data, which can't be read
        // back out of the tree, may be left rough in common_mp.
        Tree StockTree(const fs::path& p, const Fastfile& ff)
        {
            std::vector<std::pair<Struct*, size_t>> starts;
            Tree tree = ReadTree(zone::XboxRules(), Xbox(), ff.zone, BaseOf(p) == "common_mp", &starts);
            // The pak table lists the levels of the pictures streamed from the
            // disc's picture packs (`streaming` set), four entries a picture, in
            // the order the zone has them.
            std::sort(starts.begin(), starts.end(), [](const auto& a, const auto& b) { return a.second < b.second; });
            size_t k = 0;
            for (const auto& [d, start] : starts)
            {
                if (!d->IsAsset("GfxImage") || !d->Int("streaming")) continue;
                auto entries = std::make_shared<std::vector<PakEntry>>();
                for (size_t i = k * kLevels; i < (k + 1) * kLevels && i < ff.table.size(); i++) entries->push_back(ff.table[i]);
                d->pak = entries;
                k++;
            }
            return tree;
        }

        // What a stock file is looked through for, from the file as it is: its
        // streamed pictures still in the arena it was read into.
        struct Survey
        {
            StockScan scan;
            std::vector<std::pair<std::string, Struct*>> streamed;
        };

        Survey Look(const std::vector<uint8_t>& z, const Tree& tree)
        {
            Survey out;
            StockScan& scan = out.scan;
            for (size_t at = 0; at < z.size(); at++)
            {
                if (z[at]) continue;
                size_t end = at + 1;
                while (end < z.size() && z[end] >= 0x20 && z[end] <= 0x7E) end++;
                if (end < z.size() && !z[end] && end - at - 1 >= 1 && end - at - 1 <= 255)
                    scan.names.emplace(reinterpret_cast<const char*>(z.data() + at + 1), end - at - 1);
                at = end - 1;
            }
            auto pools = PoolNames(tree.assets);
            scan.techsets = pools["MaterialTechniqueSet"];
            scan.models = pools["XModel"];
            scan.aliases = pools["snd_alias_list_t"];
            std::set<std::string> seen;
            ForEach(tree.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset("GfxImage") || o->Int("streaming") != 1 || o->Ptr("pixels")) return;
                const std::string name = AssetName(o).value_or("");
                if (name.empty() || name[0] == ',' || name[0] == '*' || name[0] == '$' || !seen.insert(Lower(name)).second) return;
                out.streamed.push_back({ Lower(name), o });
            });
            return out;
        }

        // A stock file read on a thread of its own, into an arena of its own:
        // one the conversion takes from is kept, one only looked through goes
        // once what it was looked through for is copied out.
        struct StockRead
        {
            std::unique_ptr<StockFile> file;   // its tree, and its arena
            std::optional<Survey> survey;
        };

        StockRead ReadStock(const fs::path& p, bool look)
        {
            StockRead out;
            out.file = std::make_unique<StockFile>();
            StockFile& file = *out.file;
            ArenaScope scope(*file.arena);
            file.path = p;
            file.base = BaseOf(p);
            file.resident = Resident(file.base);
            Fastfile ff;
            try
            {
                ff = ReadFastfile(p);
                file.tree = StockTree(p, ff);
            }
            catch (const Error&)
            {
                if (!look) throw;
                out.survey.emplace();   // a file only looked through that can't be read offers nothing
                return out;
            }
            if (look) out.survey = Look(ff.zone, file.tree);
            return out;
        }

        // A donor stock map read only for its teams or effects: its drawn
        // geometry and collision are never used. The named assets inside them
        // stay, in their place and order, so every look-up by name finds what
        // it did.
        void DropWorld(StockFile& file)
        {
            ArenaScope scope(*file.arena);
            for (Entry*& e : file.tree.assets)
            {
                if ((e->type != "gfx_map" && e->type != "col_map_mp") || !As<Struct>(e->asset)) continue;
                Array* inner = A().Make<Array>();
                Node* world = e->asset;
                ForEach(world, [&](Node* n) {
                    if (Struct* o = As<Struct>(n); o && o->IsAsset() && o != world) inner->items.push_back(o);
                });
                std::reverse(inner->items.begin(), inner->items.end());   // (the walk takes a list's last item first)
                e = A().Make<Entry>("world_parts", inner);
            }
        }

        // How many stock files are read at once: each takes a few hundred MB
        // while it is read.
        size_t Workers() { return std::clamp<size_t>(std::thread::hardware_concurrency() / 2, 1, 4); }

        // Reads the stock files given, several at a time: to keep (the files
        // the conversion takes from), or only to look through. Each is looked
        // through once, when it is first read, before anything changes it.
        void MapPort::Load(const std::vector<fs::path>& paths, bool keep, const std::set<fs::path>& donors)
        {
            std::vector<fs::path> todo;
            for (const fs::path& p : paths)
                if (!(keep ? loaded.count(p) : scans.count(p)) && std::find(todo.begin(), todo.end(), p) == todo.end())
                    todo.push_back(p);
            for (size_t at = 0; at < todo.size(); at += Workers())
            {
                std::vector<std::future<StockRead>> reads;
                for (size_t k = at; k < std::min(todo.size(), at + Workers()); k++)
                    reads.push_back(std::async(std::launch::async, ReadStock, todo[k], !scans.count(todo[k])));
                for (size_t k = 0; k < reads.size(); k++)
                {
                    const fs::path& p = todo[at + k];
                    log((keep ? "reading stock 360 file " : "looking through stock 360 file ") + Utf8(p.filename()));
                    StockRead read = reads[k].get();
                    if (read.survey)
                    {
                        StockScan& scan = scans[p] = std::move(read.survey->scan);
                        // (a picture's header, its name and pak entries: nothing points back)
                        for (const auto& [name, image] : read.survey->streamed)
                        {
                            Struct* copy = DeepCopy(image);
                            copy->slot = nullptr;
                            copy->forward = false;
                            scan.streamed.push_back({ name, copy });
                        }
                    }
                    if (!keep) continue;
                    if (donors.count(p)) DropWorld(*read.file);
                    loaded[p] = std::move(read.file);
                }
            }
        }

        StockFile* MapPort::Read(const fs::path& p, const std::set<fs::path>& donors)
        {
            Load({ p }, true, donors);
            return loaded.at(p).get();
        }

        const StockScan& MapPort::Scan(const fs::path& p)
        {
            Load({ p }, false, {});
            return scans.at(p);
        }

        // Stock maps to read as well for the map's effects the files read
        // already don't have: at most two, those carrying the most of them.
        std::vector<fs::path> Greedy(std::set<std::string> want, const std::vector<std::pair<fs::path, std::set<std::string>>>& hits)
        {
            std::vector<fs::path> out;
            auto count = [&](const std::set<std::string>& h) {
                size_t n = 0;
                for (const std::string& s : h) n += want.count(s);
                return n;
            };
            while (out.size() < 2 && !hits.empty())
            {
                const std::pair<fs::path, std::set<std::string>>* best = nullptr;
                std::pair<size_t, bool> score{};
                for (const auto& h : hits)
                {
                    const std::pair<size_t, bool> sc{ count(h.second), std::find(out.begin(), out.end(), h.first) == out.end() };
                    if (!best || sc > score)
                    {
                        best = &h;
                        score = sc;
                    }
                }
                if (std::find(out.begin(), out.end(), best->first) != out.end() || !count(best->second)) break;
                out.push_back(best->first);
                for (const std::string& s : best->second) want.erase(s);
            }
            return out;
        }

        std::vector<fs::path> MapPort::EffectDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                                    const std::vector<fs::path>& candidates)
        {
            std::set<std::string> have, want;
            for (const fs::path& p : refs)
            {
                if (Resident(BaseOf(p))) continue;
                for (Entry* e : loaded.at(p)->tree.assets)
                    if (Struct* a = As<Struct>(e->asset); a && e->type == "fx")
                        if (auto n = Name(a); n && !n->empty() && (*n)[0] != ',') have.insert(*n);
            }
            for (Entry* e : pc.assets)
                if (Struct* a = As<Struct>(e->asset); a && e->type == "fx")
                    if (auto n = Name(a); n && !have.count(*n)) want.insert(*n);
            if (want.empty() || candidates.empty()) return {};
            std::vector<std::pair<fs::path, std::set<std::string>>> hits;
            for (const fs::path& p : candidates)
            {
                std::set<std::string> h;
                const StockScan& s = Scan(p);
                for (const std::string& n : want)
                    if (s.names.count(n)) h.insert(n);
                hits.push_back({ p, std::move(h) });
            }
            return Greedy(want, hits);
        }

        // Stock maps to read as well for shader sets the map's materials use
        // (and the composite sets its decals could become) that the files read
        // already lack: at most two, those carrying the most of them.
        std::vector<fs::path> MapPort::TechsetDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                                     const std::vector<fs::path>& candidates)
        {
            std::set<std::string> names;
            ForEach(pc.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset("MaterialTechniqueSet")) return;
                std::string name = AssetName(o).value_or(Name(o).value_or(""));
                name = WithoutCommas(std::move(name));
                if (!name.empty()) names.insert(name);
            });
            std::set<std::string> want = names;
            for (std::string n : names)   // (PC-only "high" shadow sets)
                if (const size_t at = n.find("_hsm_"); at != std::string::npos) want.insert(n.replace(at, 5, "_sm_"));
            std::set<std::pair<std::string, bool>> grounds, first, second;
            for (const std::string& n : names)
            {
                if (auto g = GroundCode(n)) grounds.insert({ g->code, g->p0 });
                if (auto a = LayerCode(n, 1)) first.insert({ a->code, a->p0 });
                if (auto b = LayerCode(n, 2)) second.insert({ b->code, b->p0 });
            }
            for (const auto& g : grounds)
                for (const auto& a : first)
                {
                    // (composite sets are named without wc_: l_sm_r0c0n0_b1c1n1s1p0)
                    want.insert(g.first + "_" + a.first + (g.second || a.second ? "p0" : ""));
                    for (const auto& b : second)
                        want.insert(g.first + "_" + a.first + "_" + b.first + (g.second || a.second || b.second ? "p0" : ""));
                }
            std::set<std::string> have;
            for (const fs::path& p : refs)
            {
                const auto& t = Scan(p).techsets;
                have.insert(t.begin(), t.end());
            }
            std::set<std::string> missing;
            for (const std::string& n : want)
                if (!have.count(Lower(n))) missing.insert(Lower(n));
            if (missing.empty()) return {};
            std::vector<std::pair<fs::path, std::set<std::string>>> hits;
            for (const fs::path& p : candidates)
            {
                std::set<std::string> h;
                for (const std::string& n : Scan(p).techsets)
                    if (missing.count(n)) h.insert(n);
                hits.push_back({ p, std::move(h) });
            }
            std::vector<fs::path> out = Greedy(missing, hits);
            if (!out.empty())
            {
                std::vector<std::string> files;
                for (const fs::path& p : out) files.push_back(Utf8(p.filename()));
                log("  shader sets the files given lack: reading " + Joined(files) + " as well");
            }
            return out;
        }

        std::optional<std::string> MapPort::DestructibleScript(const std::vector<fs::path>& refs, const std::set<fs::path>& donors)
        {
            std::optional<std::string> script;
            for (const fs::path& p : refs)
            {
                if (!Resident(BaseOf(p))) continue;
                for (Entry* e : Read(p, donors)->tree.assets)
                    if (Struct* a = As<Struct>(e->asset);
                        a && e->type == "rawfile" && Lower(Name(a).value_or("")) == "common_scripts/_destructible_types.gsc")
                        script = RawfileText(a);
            }
            return script;
        }

        // The models the map's destructible entities need that neither the map
        // nor the always-loaded files have: a model no stock file has is taken
        // in another colour the script knows, its paint then the map's own.
        std::vector<Breakable> MapPort::DestructibleDonors(const Tree& pc, const std::vector<fs::path>& refs,
                                                          const std::vector<fs::path>& stock, const std::set<fs::path>& donors)
        {
            std::set<std::string> have = PoolNames(pc.assets)["XModel"];
            for (const fs::path& p : refs)
                if (Resident(BaseOf(p)))
                {
                    const auto& m = Scan(p).models;
                    have.insert(m.begin(), m.end());
                }
            const std::optional<std::string> script = DestructibleScript(refs, donors);
            if (!script) return {};
            const std::set<std::string> types = DestructibleTypes(pc);
            if (types.empty()) return {};
            std::vector<Breakable> plan;
            std::map<std::string, std::vector<std::string>> missing;
            for (const auto& [t, found] : DestructibleNames(*script, types))
            {
                const auto& [names, colours] = found;
                const std::string type = Lower(t);
                std::optional<std::string> own;
                for (const std::string& c : colours)
                    if (type.ends_with("_" + Lower(c)))
                    {
                        own = c;
                        break;
                    }
                const std::string stem = own ? type.substr(0, type.rfind('_')) : type;
                for (const std::string& n : names)
                {
                    const std::string low = Lower(n);
                    if (low == type || low.find('/') != std::string::npos || low.find(' ') != std::string::npos ||
                        !low.starts_with(stem) || have.count(low))
                        continue;
                    // The stock model to try, and the colour it is in when it is another's.
                    struct Try
                    {
                        std::string model;
                        std::optional<ColourSwap> swap;
                    };
                    std::vector<Try> tries{ { low, std::nullopt } };
                    if (own)
                        for (const std::string& c : colours)
                        {
                            if (c == *own) continue;
                            std::string src = low;
                            const std::string from = "_" + Lower(*own) + "_";
                            if (const size_t at = src.find(from); at != std::string::npos) src.replace(at, from.size(), "_" + Lower(c) + "_");
                            tries.push_back({ src, ColourSwap{ Lower(c), Lower(*own) } });
                        }
                    bool hit = false;
                    for (const auto& [src, swap] : tries)
                    {
                        auto p = std::find_if(stock.begin(), stock.end(), [&](const fs::path& s) { return Scan(s).models.count(src); });
                        if (p != stock.end() && (!swap || src != low))
                        {
                            plan.push_back({ low, src, *p, swap });
                            hit = true;
                            break;
                        }
                    }
                    if (!hit && own && low.find("_" + Lower(*own) + "_") != std::string::npos) missing[t].push_back(low);
                }
            }
            if (!plan.empty())
            {
                std::set<std::string> files;
                for (const Breakable& b : plan) files.insert(Utf8(b.path.filename()));
                log("  breakable parts the map lacks: " + std::to_string(plan.size()) + " models from " +
                    Joined(std::vector<std::string>(files.begin(), files.end())));
            }
            for (const auto& [t, names] : missing)
            {
                std::vector<std::string> some(names.begin(), names.begin() + ptrdiff_t(std::min<size_t>(6, names.size())));
                log("  note: no stock map has " + t + "'s " + Joined(some) + (names.size() > 6 ? " ..." : ""));
            }
            return plan;
        }

        // The sounds the 360's destructible script plays for the map's
        // destructible entities that the map and the always-loaded files lack,
        // and a stock file that has each (one already read first).
        std::vector<std::pair<std::string, fs::path>> MapPort::DestructibleSoundDonors(const Tree& pc,
                                                                                       const std::vector<fs::path>& refs,
                                                                                       const std::vector<fs::path>& stock,
                                                                                       const std::set<fs::path>& donors)
        {
            std::set<std::string> have = PoolNames(pc.assets)["snd_alias_list_t"];
            for (const fs::path& p : refs)
                if (Resident(BaseOf(p)))
                {
                    const auto& a = Scan(p).aliases;
                    have.insert(a.begin(), a.end());
                }
            const std::optional<std::string> script = DestructibleScript(refs, donors);
            if (!script) return {};
            const std::set<std::string> types = DestructibleTypes(pc);
            if (types.empty()) return {};
            std::vector<fs::path> order;
            for (const fs::path& p : stock)
                if (std::find(refs.begin(), refs.end(), p) != refs.end()) order.push_back(p);
            for (const fs::path& p : stock)
                if (std::find(refs.begin(), refs.end(), p) == refs.end()) order.push_back(p);
            std::vector<std::pair<std::string, fs::path>> plan;
            std::set<std::string> seen;
            for (const auto& [t, found] : DestructibleNames(*script, types))
                for (const std::string& n : found.first)
                {
                    const std::string low = Lower(n);
                    if (have.count(low) || seen.count(low)) continue;
                    auto p = std::find_if(order.begin(), order.end(), [&](const fs::path& s) { return Scan(s).aliases.count(low); });
                    if (p == order.end()) continue;
                    plan.push_back({ low, *p });
                    seen.insert(low);
                }
            if (!plan.empty())
            {
                std::set<std::string> files;
                for (const auto& [n, p] : plan) files.insert(Utf8(p.filename()));
                log("  destructible sounds the map lacks: " + std::to_string(plan.size()) + " from " +
                    Joined(std::vector<std::string>(files.begin(), files.end())));
            }
            return plan;
        }

        // The PC's config string tables go: the 360 game only ever looks for
        // configStrings_360_<map>_<mode>.csv.
        void MapPort::DropPcTables(Tree& t)
        {
            const size_t before = t.assets.size();
            std::erase_if(t.assets, [](Entry* e) {
                Struct* a = As<Struct>(e->asset);
                if (e->type != "stringtable" || !a) return false;
                std::string n = Lower(Name(a).value_or(""));
                n = WithoutCommas(std::move(n));
                return n.starts_with("mp/configstrings/configstrings_pc_");
            });
            if (const size_t gone = before - t.assets.size())
                log("  " + std::to_string(gone) + " PC config string tables left out (the 360 never reads them)");
        }

        // Stops when the map and the files loaded with it hold more assets of a
        // kind than the game has room for; warns when it is close. Each full
        // copy of an asset those files have takes a place of its own.
        void MapPort::CheckPools(const Tree& t, const std::vector<StockFile*>& refs, Porter& porter)
        {
            std::map<std::string, std::set<std::string>> have;
            for (StockFile* f : refs)
                if (f->resident || f->base == "patch_mp")
                    for (auto& [type, names] : PoolNames(f->tree.assets)) have[type].insert(names.begin(), names.end());
            std::map<std::string, int> copies;
            ForEach(t.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !PoolLimit(o->asset)) return;
                const std::optional<std::string> name = PoolName(o);
                if (Named(name) && !OnlyNamed(name) && have[std::string(o->asset)].count(Lower(*name))) copies[std::string(o->asset)]++;
            });
            std::vector<std::pair<double, std::string>> used;
            std::vector<std::string> over;
            for (auto& [type, names] : PoolNames(t.assets))
            {
                std::set<std::string> all = names;
                all.insert(have[type].begin(), have[type].end());
                const int n = int(all.size()) + copies[type], limit = PoolLimit(type);
                used.push_back({ double(n) / limit, type + " " + std::to_string(n) + "/" + std::to_string(limit) });
                if (n > limit) over.push_back(type + ": " + std::to_string(n) + " of " + std::to_string(limit));
                else if (n > limit * 0.95)
                    porter.Warn(std::to_string(n) + " " + type + " assets with the files loaded alongside, of the " +
                                std::to_string(limit) + " the game has room for");
            }
            std::sort(used.begin(), used.end(), std::greater<>());
            std::vector<std::string> top;
            for (size_t i = 0; i < std::min<size_t>(4, used.size()); i++) top.push_back(used[i].second);
            log("  game asset room used (with the files loaded alongside): " + Joined(top));
            if (!over.empty())
                throw PortError("the map holds more assets than the game has room for (" + Joined(over) +
                                "); it would stop loading part way");
        }

        // The 360 game fills block 5 (callback) at load time: a bump allocator
        // there takes, for every material, 4 bytes per texture, 4 per model
        // surface and 16 per some other assets. The file carries no data for
        // it, so a PC file converts to size 0 and the game writes through a null
        // pointer. Stock files reserve 1-72 KB.
        void ReserveCallbackBlock(std::vector<uint8_t>& zone, const Tree& t)
        {
            uint64_t need = 0;
            ForEach(t.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset()) return;
                need += 16;
                if (o->IsAsset("Material")) need += 4 * uint64_t(std::max<int64_t>(0, o->Int("textureCount")));
                else if (o->IsAsset("XModel")) need += 4 * uint64_t(std::max<int64_t>(0, o->Int("numsurfs")));
            });
            // Other load-time allocations can land here too, so at least what a
            // stock map reserves.
            need = std::max<uint64_t>((need * 2 + 4096 + 0xFFF) & ~uint64_t(0xFFF), 0x10000);
            if (Load(zone.data() + 28, 4, true) < need) Store(zone.data() + 28, 4, need, true);
        }

        // The stock pictures copied in whose pixels live in the disc's picture
        // packs: the container lists where, four entries a picture, in the
        // order the zone has them.
        std::vector<PakEntry> PakTable(std::vector<std::pair<size_t, Struct*>> written)
        {
            std::stable_sort(written.begin(), written.end(), [](const auto& a, const auto& b) { return a.first < b.first; });
            std::vector<PakEntry> table;
            for (const auto& [at, d] : written)
            {
                if (!d->IsAsset("GfxImage") || !d->Int("streaming")) continue;
                if (!d->pak) throw PortError("picture " + AssetName(d).value_or("?") + " streams, but from no pak entries");
                table.insert(table.end(), d->pak->begin(), d->pak->end());
            }
            return table;
        }

        // A converted loading screen made the menus' picture of the map: its
        // briefing material alone, named preview_<map> as the menus look a
        // map's picture up (the downloadable maps' are in dlc1_ui_mp.ff and
        // dlc2_ui_mp.ff, zones the menus load), its picture named the same.
        // Its zone is named after itself by a raw file, as the game's are.
        bool Preview(Tree& t, const std::string& map)
        {
            const std::string name = "preview_" + map;
            Entry* material = nullptr;
            Entry* zoneName = nullptr;
            for (Entry* e : t.assets)
                if (Struct* a = As<Struct>(e->asset))
                {
                    if (e->type == "material" && Name(a) == "$levelbriefing") material = e;
                    if (e->type == "rawfile" && Lower(Name(a).value_or("")) == Lower(map + "_load")) zoneName = e;
                }
            if (!material) return false;
            bool pictured = false;
            ForEach(material->asset, [&](Node* n) {
                if (Struct* o = As<Struct>(n); o && o->IsAsset("GfxImage"))
                {
                    SetName(o, name);
                    pictured = true;
                }
            });
            if (!pictured) return false;
            SetName(As<Struct>(material->asset), name);
            t.assets = { material };
            if (zoneName)
            {
                SetName(As<Struct>(zoneName->asset), map + "_preview");
                t.assets.push_back(zoneName);
            }
            return true;
        }

        void MapPort::Port(Conversion c)
        {
            const Fastfile ff = ReadFastfile(c.pc);
            if (!ff.pc) throw PortError(Utf8(c.pc.filename()) + " is not a PC fastfile");
            Tree pc = ReadTree(zone::PcRules(), Pc(), ff.zone);
            std::vector<fs::path> refs = c.refs;
            auto add = [&](const std::vector<fs::path>& extra) {
                for (const fs::path& p : extra)
                {
                    refs.push_back(p);
                    c.donors.insert(p);
                }
            };
            std::vector<fs::path> others;
            for (const fs::path& p : c.fx)
                if (std::find(refs.begin(), refs.end(), p) == refs.end()) others.push_back(p);
            Load(refs, true, c.donors);
            if (!c.fx.empty())
            {
                Load(c.fx, false, {});
                add(EffectDonors(pc, refs, others));
                others.clear();
                for (const fs::path& p : c.fx)
                    if (std::find(refs.begin(), refs.end(), p) == refs.end()) others.push_back(p);
                add(TechsetDonors(pc, refs, others));
            }
            std::vector<Breakable> breakables;
            std::vector<std::pair<std::string, fs::path>> sounds;
            if (!c.fx.empty())
            {
                breakables = DestructibleDonors(pc, refs, c.fx, c.donors);
                std::set<fs::path> extra;
                for (const Breakable& b : breakables)
                    if (std::find(refs.begin(), refs.end(), b.path) == refs.end()) extra.insert(b.path);
                add(std::vector<fs::path>(extra.begin(), extra.end()));
                sounds = DestructibleSoundDonors(pc, refs, c.fx, c.donors);
                extra.clear();
                for (const auto& [n, p] : sounds)
                    if (std::find(refs.begin(), refs.end(), p) == refs.end()) extra.insert(p);
                add(std::vector<fs::path>(extra.begin(), extra.end()));
            }
            std::vector<StockFile*> files;
            for (const fs::path& p : refs) files.push_back(Read(p, c.donors));
            std::vector<Zip> iwds;
            if (iwd) iwds.emplace_back(*iwd);
            log("converting " + Utf8(c.pc.filename()));
            Porter porter(pc, files, std::move(iwds), gameIwds, log, c.textureBudget);
            porter.breakables = std::move(breakables);
            for (const auto& [n, p] : sounds) porter.destructibleAliases.push_back(n);
            std::vector<fs::path> streamedFrom = refs;
            for (const fs::path& p : c.fx)
                if (std::find(refs.begin(), refs.end(), p) == refs.end()) streamedFrom.push_back(p);
            for (const fs::path& p : streamedFrom)
                for (const auto& [name, image] : Scan(p).streamed) porter.KnowStreamed(name, image);
            porter.Convert();
            if (!porter.mapName.empty()) porter.AddTeams(MapTeams(c.arena, c.teams));
            DropPcTables(pc);
            CheckPools(pc, files, porter);
            Write(pc, c.out);
            for (const std::string& note : porter.warnings)
                if (std::find(ported.warnings.begin(), ported.warnings.end(), note) == ported.warnings.end())
                    ported.warnings.push_back(note);
            // Without its picture the map still works: the menus show black.
            if (c.preview)
                try
                {
                    if (Preview(pc, c.preview->map)) Write(pc, c.preview->file);
                    else log("  note: the loading screen has no picture, so the menus show none for the map");
                }
                catch (const std::exception& e)
                {
                    log(std::string("  note: the menus' picture of the map couldn't be made, so they show none: ") + e.what());
                }
        }

        // Writes a converted tree as an unsigned 360 fastfile.
        void MapPort::Write(const Tree& t, const fs::path& to)
        {
            TreeWriter w(t, zone::XboxRules(), Xbox());
            w.mapRel = [&](const Ref* r) { return MapRel(r, zone::XboxRules()); };
            w.dedupe = true;
            std::vector<uint8_t> zone = w.Write();
            ReserveCallbackBlock(zone, t);
            const std::vector<uint8_t> file = XboxFastfile(zone, PakTable(w.written));
            fs::create_directories(to.parent_path());
            fs::path part = to;
            part += ".part";
            {
                std::ofstream out(part, std::ios::binary);
                out.write(reinterpret_cast<const char*>(file.data()), std::streamsize(file.size()));
                if (!out) throw Error("cannot write " + Utf8(part));
            }
            fs::rename(part, to);
            log("wrote " + Utf8(to.filename()) + " (" + std::to_string(zone.size()) + " bytes of zone)");
            if (w.deduped)
                log("  " + std::to_string(w.deduped) + " places point at an asset written earlier instead of holding a copy of it");
            ported.written.push_back(to);
        }

        Ported MapPort::Run(const PcMap& map, const fs::path& outDir, std::optional<Teams> teams)
        {
            auto stock = [&](const std::string& name) -> std::optional<fs::path> {
                auto it = byName.find(name);
                return it == byName.end() ? std::nullopt : std::optional(it->second);
            };
            const std::optional<fs::path> cpg = stock("code_post_gfx_mp.ff");
            if (!cpg) throw PortError("code_post_gfx_mp.ff (from the console) is needed next to the stock maps");
            std::vector<fs::path> maps, loads;
            for (const auto& [n, p] : byName)
            {
                if (n.starts_with("mp_") && !n.ends_with("_load.ff") && !n.ends_with("_preview.ff")) maps.push_back(p);
                if (n.ends_with("_load.ff")) loads.push_back(p);
            }
            std::sort(maps.begin(), maps.end());
            std::sort(loads.begin(), loads.end());
            if (maps.empty()) throw PortError("at least one stock 360 map (for example mp_favela.ff) is needed");
            const fs::path tpl = stock("mp_favela.ff").value_or(maps[0]);
            const std::string name = Utf8(map.ff.stem());
            iwd = map.iwd;
            fs::create_directories(outDir);
            Read(*cpg, {});
            const std::optional<fs::path> common = stock("common_mp.ff");
            if (!common)
                log("  note: common_mp.ff isn't next to the stock maps, so the materials only it carries (heat distortion, ...) "
                    "can't be used");
            Struct* arenaFile = AssetNamed(loaded.at(*cpg)->tree, "rawfile", "mp/basemaps.arena");
            const std::string arenas = arenaFile ? RawfileText(arenaFile) : "";
            Teams want = MapTeams(map.arena, teams);
            // A PC copy of a stock map: the stock 360 map of that name comes first
            // for what it has, and its teams are the 360's for the map.
            const std::optional<fs::path> same = stock(Lower(name) + ".ff");
            if (!teams && !map.arena)
                if (auto t = ArenaTeams(arenas, Lower(name))) want = *t;
            std::vector<fs::path> refs{ *cpg };
            if (common) refs.push_back(*common);
            if (same && *same != tpl) refs.push_back(*same);
            refs.push_back(tpl);
            std::set<fs::path> donors;
            for (const std::string& team : { want.allies, want.axis })
            {
                const std::vector<std::string> carriers = TeamMaps(arenas, Lower(team));
                auto donor = std::find_if(maps.begin(), maps.end(), [&](const fs::path& p) {
                    return std::find(carriers.begin(), carriers.end(), BaseOf(p)) != carriers.end();
                });
                if (donor != maps.end() && std::find(refs.begin(), refs.end(), *donor) == refs.end())
                {
                    refs.push_back(*donor);
                    donors.insert(*donor);
                }
            }
            if (map.load)
            {
                // The menus' picture is named after the .arena, as the map is installed.
                const std::string shown = map.arena ? Utf8(map.arena->stem()) : name;
                fs::path tplLoad = tpl;
                tplLoad.replace_extension();
                tplLoad += "_load.ff";
                const std::optional<fs::path> loadRef =
                    std::find(loads.begin(), loads.end(), tplLoad) != loads.end() ? std::optional(tplLoad)
                    : loads.empty()                                              ? std::nullopt
                                                                                 : std::optional(loads[0]);
                if (!loadRef) log("  note: no stock *_load.ff given; the loading screen file is left out");
                else
                    try
                    {
                        Port({ *map.load, outDir / PathOf(name + "_load.ff"), std::nullopt, { *cpg, tpl, *loadRef }, {}, {},
                               std::nullopt, 0, Conversion::Picture{ shown, outDir / PathOf(name + "_preview.ff") } });
                    }
                    catch (const Error& e)
                    {
                        // The map works without it: the game shows a plain loading screen.
                        log("  note: " + name + "_load.ff couldn't be converted, so it's left out (the map still works, "
                                                "with a plain loading screen): " + e.what());
                    }
            }
            Port({ map.ff, outDir / PathOf(name + ".ff"), map.arena, refs, maps, donors, want, kTextureBudgetMb });
            return ported;
        }
    }

    PcMap PcMap::Beside(const fs::path& ff)
    {
        PcMap map{ ff, std::nullopt, std::nullopt, std::nullopt };
        fs::path base = ff;
        base.replace_extension();
        auto beside = [&](const char* suffix) -> std::optional<fs::path> {
            fs::path p = base;
            p += suffix;
            return fs::exists(p) ? std::optional(p) : std::nullopt;
        };
        map.load = beside("_load.ff");
        map.iwd = beside(".iwd");
        map.arena = beside(".arena");
        return map;
    }

    Ported PortMap(const PcMap& map, const fs::path& outDir, const std::vector<fs::path>& stock, const Log& log,
                   const PortOptions& options)
    {
        return MapPort(stock, options.gameIwds, log).Run(map, outDir, options.teams);
    }

    std::vector<fs::path> GameIwds(const fs::path& folder)
    {
        std::vector<std::pair<std::string, fs::path>> found;   // (relative path in lower case, path)
        std::error_code ec;
        for (auto it = fs::recursive_directory_iterator(folder, ec); !ec && it != fs::recursive_directory_iterator();
             it.increment(ec))
        {
            const std::string name = Lower(Utf8(it->path().filename()));
            if (it->is_directory(ec))
            {
                if (name == "usermaps" || name == "mods") it.disable_recursion_pending();
            }
            else if (name.ends_with(".iwd"))
            {
                const std::u8string relative = it->path().lexically_relative(folder).generic_u8string();
                found.emplace_back(Lower(std::string(relative.begin(), relative.end())), it->path());
            }
        }
        std::sort(found.begin(), found.end());
        std::vector<fs::path> out;
        for (auto& [key, p] : found) out.push_back(std::move(p));
        return out;
    }
}
