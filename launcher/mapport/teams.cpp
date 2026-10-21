// The map's two teams. IW4x loads team assets (soldier bodies, heads and
// arms, flag and crate models, team icons) from its own files; on the 360
// each map carries its teams', so they are copied in from the stock maps
// that have them, and the map's script is set to use them.
#include "porter.h"
#include "paths.h"

#include <algorithm>
#include <regex>

namespace mapport
{
    namespace
    {
        struct TeamAssets
        {
            const char* team;
            std::vector<const char*> models, materials;
        };

        // Per team (mp/factionTable.csv and the character scripts in common_mp):
        // the models and icons a map carries for it.
        const std::vector<TeamAssets>& AllTeamAssets()
        {
            static const std::vector<TeamAssets> kTeams = {
            { "us_army",
                { "head_allies_us_army_sniper", "head_us_army_a", "head_us_army_b", "head_us_army_c",
                  "head_us_army_d", "head_us_army_e", "head_us_army_f", "mp_body_army_sniper",
                  "mp_body_us_army_assault_a", "mp_body_us_army_assault_b", "mp_body_us_army_assault_c",
                  "mp_body_us_army_lmg", "mp_body_us_army_lmg_b", "mp_body_us_army_lmg_c", "mp_body_us_army_riot",
                  "mp_body_us_army_shotgun", "mp_body_us_army_shotgun_b", "mp_body_us_army_shotgun_c",
                  "mp_body_us_army_smg", "mp_body_us_army_smg_b", "mp_body_us_army_smg_c",
                  "viewhands_sniper_us_army", "viewhands_us_army", "prop_flag_ranger", "prop_flag_ranger_carry",
                  "com_plasticcase_rangers" },
                { "faction_128_rangers", "faction_128_rangers_fade", "objpoint_flag_rangers", "headicon_rangers" },
            },
            { "opforce_composite",
                { "head_op_arab_sniper", "head_opforce_arab_a", "head_opforce_arab_b", "head_opforce_arab_c",
                  "head_opforce_arab_d_hat", "head_opforce_arab_e", "head_riot_op_arab", "mp_body_op_arab_sniper",
                  "mp_body_opforce_arab_assault_a", "mp_body_opforce_arab_lmg_a", "mp_body_opforce_arab_shotgun_a",
                  "mp_body_opforce_arab_smg_a", "mp_body_riot_op_arab", "viewhands_militia",
                  "viewhands_sniper_op_arab", "prop_flag_opforce", "prop_flag_opforce_carry", "com_plasticcase_arab" },
                { "faction_128_arab", "faction_128_arab_fade", "objpoint_flag_arab", "headicon_arab" },
            },
            { "opforce_arctic",
                { "head_op_arctic_sniper", "head_opforce_arctic_a", "head_opforce_arctic_b", "head_opforce_arctic_c",
                  "head_opforce_arctic_d", "head_riot_op_arctic", "mp_body_op_arctic_sniper",
                  "mp_body_opforce_arctic_assault_a", "mp_body_opforce_arctic_assault_b",
                  "mp_body_opforce_arctic_assault_c", "mp_body_opforce_arctic_lmg", "mp_body_opforce_arctic_lmg_b",
                  "mp_body_opforce_arctic_lmg_c", "mp_body_opforce_arctic_shotgun",
                  "mp_body_opforce_arctic_shotgun_b", "mp_body_opforce_arctic_shotgun_c",
                  "mp_body_opforce_arctic_smg", "mp_body_opforce_arctic_smg_b", "mp_body_opforce_arctic_smg_c",
                  "mp_body_riot_op_arctic", "viewhands_arctic_opforce", "viewhands_sniper_op_arctic",
                  "prop_flag_speznas", "prop_flag_speznas_carry", "com_plasticcase_ussr" },
                { "faction_128_ussr", "faction_128_ussr_fade", "objpoint_flag_ussr", "headicon_ussr" },
            },
            { "opforce_airborne",
                { "head_airborne_a", "head_airborne_b", "head_airborne_c", "head_airborne_d", "head_airborne_e",
                  "head_op_airborne_sniper", "head_riot_op_airborne", "mp_body_airborne_assault_a",
                  "mp_body_airborne_assault_b", "mp_body_airborne_assault_c", "mp_body_airborne_lmg",
                  "mp_body_airborne_lmg_b", "mp_body_airborne_lmg_c", "mp_body_airborne_shotgun",
                  "mp_body_airborne_shotgun_b", "mp_body_airborne_shotgun_c", "mp_body_airborne_smg",
                  "mp_body_airborne_smg_b", "mp_body_airborne_smg_c", "mp_body_op_airborne_sniper",
                  "mp_body_riot_op_airborne", "viewhands_russian_airborne", "viewhands_sniper_op_airborne",
                  "prop_flag_speznas", "prop_flag_speznas_carry", "com_plasticcase_ussr" },
                { "faction_128_ussr", "faction_128_ussr_fade", "objpoint_flag_ussr", "headicon_ussr" },
            },
            { "militia",
                { "head_militia_a_wht", "head_militia_ba_blk", "head_militia_bb_blk_hat", "head_militia_bc_blk",
                  "head_militia_bd_blk", "head_op_militia_sniper", "head_riot_op_militia",
                  "mp_body_militia_assault_aa_blk", "mp_body_militia_assault_aa_wht",
                  "mp_body_militia_assault_ab_blk", "mp_body_militia_assault_ac_blk", "mp_body_militia_lmg_aa_blk",
                  "mp_body_militia_lmg_ab_blk", "mp_body_militia_lmg_ac_blk", "mp_body_militia_smg_aa_blk",
                  "mp_body_militia_smg_aa_wht", "mp_body_militia_smg_ab_blk", "mp_body_militia_smg_ac_blk",
                  "mp_body_op_miltia_sniper", "mp_body_riot_op_militia", "viewhands_militia", "prop_flag_militia",
                  "prop_flag_militia_carry", "com_plasticcase_militia" },
                { "faction_128_militia", "faction_128_militia_fade", "objpoint_flag_militia", "headicon_militia" },
            },
            { "socom_141",
                { "head_seal_soccom_a", "head_seal_soccom_ba", "head_seal_soccom_ca", "head_seal_soccom_da",
                  "mp_body_seal_soccom_assault_a", "mp_body_seal_soccom_assault_b",
                  "mp_body_seal_soccom_assault_b_blk", "mp_body_seal_soccom_assault_c",
                  "mp_body_seal_soccom_assault_c_blk", "mp_body_seal_soccom_assault_d", "viewhands_us_army",
                  "prop_flag_tf141", "prop_flag_tf141_carry", "com_plasticcase_taskforce141" },
                { "faction_128_taskforce141", "faction_128_taskforce141_fade", "objpoint_flag_taskforce",
                  "headicon_taskforce141" },
            },
            { "socom_141_arctic",
                { "head_allies_tf141_arctic_sniper", "head_riot_tf141_arctic", "head_tf141_arctic_a",
                  "head_tf141_arctic_b", "head_tf141_arctic_c", "head_tf141_arctic_d", "mp_body_riot_tf141_arctic",
                  "mp_body_tf141_arctic_sniper", "mp_body_tf141_assault_a", "mp_body_tf141_assault_b",
                  "mp_body_tf141_lmg", "mp_body_tf141_shotgun", "mp_body_tf141_smg", "viewhands_arctic",
                  "viewhands_sniper_tf141_arctic", "viewhands_tf141", "prop_flag_tf141", "prop_flag_tf141_carry",
                  "com_plasticcase_taskforce141" },
                { "faction_128_taskforce141", "faction_128_taskforce141_fade", "objpoint_flag_taskforce",
                  "headicon_taskforce141" },
            },
            { "socom_141_desert",
                { "head_allies_tf141_desert_sniper", "head_riot_tf141_desert", "head_tf141_desert_a",
                  "head_tf141_desert_b", "head_tf141_desert_c", "head_tf141_desert_d",
                  "mp_body_desert_tf141_assault_a", "mp_body_desert_tf141_assault_b", "mp_body_desert_tf141_lmg",
                  "mp_body_desert_tf141_shotgun", "mp_body_desert_tf141_smg", "mp_body_riot_tf141_desert",
                  "mp_body_tf141_desert_sniper", "viewhands_sniper_tf141_desert", "viewhands_tf141",
                  "prop_flag_tf141", "prop_flag_tf141_carry", "com_plasticcase_taskforce141" },
                { "faction_128_taskforce141", "faction_128_taskforce141_fade", "objpoint_flag_taskforce",
                  "headicon_taskforce141" },
            },
            { "socom_141_forest",
                { "head_allies_tf141_forest_sniper", "head_riot_tf141_forest", "head_tf141_forest_a",
                  "head_tf141_forest_b", "head_tf141_forest_c", "head_tf141_forest_d",
                  "mp_body_forest_tf141_assault_a", "mp_body_forest_tf141_assault_b", "mp_body_forest_tf141_lmg",
                  "mp_body_forest_tf141_shotgun", "mp_body_forest_tf141_smg", "mp_body_riot_tf141_forest",
                  "mp_body_tf141_forest_sniper", "viewhands_sniper_tf141_forest", "viewhands_tf141",
                  "prop_flag_tf141", "prop_flag_tf141_carry", "com_plasticcase_taskforce141" },
                { "faction_128_taskforce141", "faction_128_taskforce141_fade", "objpoint_flag_taskforce",
                  "headicon_taskforce141" },
            },
            { "seals_udt",
                { "head_allies_seal_udt_sniper", "head_riot_udt", "head_seal_udt_a", "head_seal_udt_c",
                  "head_seal_udt_d", "head_seal_udt_e", "mp_body_riot_udt", "mp_body_seal_udt_assault_a",
                  "mp_body_seal_udt_assault_b", "mp_body_seal_udt_lmg", "mp_body_seal_udt_smg",
                  "mp_body_seal_udt_sniper", "viewhands_sniper_udt", "viewhands_udt", "prop_flag_seal",
                  "prop_flag_seal_carry", "com_plasticcase_seals" },
                { "faction_128_seals", "faction_128_seals_fade", "objpoint_flag_seals", "headicon_seals" },
            },
            };
            return kTeams;
        }

        bool OnSide(const std::string& side, const std::string& team)
        {
            static const std::vector<std::string> kAllies = { "us_army", "seals_udt", "socom_141", "socom_141_desert",
                                                              "socom_141_forest", "socom_141_arctic" };
            static const std::vector<std::string> kAxis = { "opforce_composite", "opforce_airborne", "opforce_arctic",
                                                            "militia" };
            const auto& list = side == "allies" ? kAllies : kAxis;
            return std::find(list.begin(), list.end(), team) != list.end();
        }
    }

    // ---- arenas ---------------------------------------------------------------

    namespace
    {
        // An arena file's { ... } blocks, each as its keys and values.
        std::vector<std::map<std::string, std::string>> ArenaBlocks(const std::string& text)
        {
            static const std::regex block(R"(\{([\s\S]*?)\})"), pair(R"re((\w+)\s+"?([^"\s]*)"?)re");
            std::vector<std::map<std::string, std::string>> out;
            for (std::sregex_iterator it(text.begin(), text.end(), block), end; it != end; ++it)
            {
                const std::string body = (*it)[1].str();
                std::map<std::string, std::string> kv;
                for (std::sregex_iterator p(body.begin(), body.end(), pair); p != end; ++p) kv[(*p)[1].str()] = (*p)[2].str();
                out.push_back(std::move(kv));
            }
            return out;
        }
    }

    std::vector<std::string> TeamMaps(const std::string& arena, const std::string& team)
    {
        std::vector<std::string> out;
        for (auto& kv : ArenaBlocks(arena))
            if (kv["allieschar"] == team || kv["axischar"] == team) out.push_back(kv["map"]);
        return out;
    }

    std::optional<Teams> ArenaTeams(const std::string& arena, const std::string& map)
    {
        for (auto& kv : ArenaBlocks(arena))
            if (kv.count("map") && kv["map"] == map && kv.count("allieschar") && kv.count("axischar"))
                return Teams{ kv["allieschar"], kv["axischar"] };
        return std::nullopt;
    }

    // ---- picking stock assets -------------------------------------------------

    // Picks a stock file's asset list entries named (type, name), with every
    // entry they point into.
    void Porter::Pick(StockFile* src, const std::set<std::pair<std::string, std::string>>& names)
    {
        std::vector<Entry*>& ents = src->tree.assets;
        std::unordered_map<const Node*, size_t> pos;
        for (size_t i = 0; i < ents.size(); i++) pos.emplace(ents[i], i);
        auto found = owners.find(src);
        if (found == owners.end())
        {
            // (Kept for the next pick from the file: both teams and the titles can come from one.)
            found = owners.emplace(src, std::unordered_map<const Node*, size_t>{}).first;
            for (size_t i = 0; i < ents.size(); i++)
                ForEach(ents[i]->asset, [&](Node* n) {
                    if (Struct* o = As<Struct>(n); o && o->IsAsset()) found->second.emplace(o, i);
                });
        }
        const auto& owner = found->second;
        std::set<size_t> sel;
        for (size_t i = 0; i < ents.size(); i++)
            if (Struct* a = As<Struct>(ents[i]->asset); a && names.count({ ents[i]->type, Name(a).value_or("") })) sel.insert(i);
        std::vector<size_t> todo(sel.begin(), sel.end());
        while (!todo.empty())
        {
            const size_t at = todo.back();
            todo.pop_back();
            ForEach(ents[at]->asset, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->Ch()) return;
                for (auto& [k, c] : o->Ch()->items)
                {
                    auto visit = [&](Node* x) {
                        Ref* r = As<Ref>(x);
                        if (!r) return;
                        Node* a = As<Entry>(r->target) ? r->target : AssetInSlot(r);
                        std::optional<size_t> j;
                        if (As<Entry>(a))
                        {
                            if (auto p = pos.find(a); p != pos.end()) j = p->second;   // (none: in a world let go)
                        }
                        else if (As<Struct>(a))
                            if (auto p = owner.find(a); p != owner.end()) j = p->second;
                        if (j && sel.insert(*j).second) todo.push_back(*j);
                    };
                    if (Array* l = As<Array>(c))
                        for (Node* x : l->items) visit(x);
                    else if (PtrList* pl = As<PtrList>(c))
                        for (Node* x : pl->items) visit(x);
                    else visit(c);
                }
            });
        }
        picked[src].insert(sel.begin(), sel.end());
    }

    // Copies the entries picked from a file in their stock order (both teams
    // of one file together: a later model can point into an earlier one's parts).
    void Porter::CopyPicked(StockFile* src)
    {
        auto it = picked.find(src);
        if (it == picked.end()) return;
        const std::set<size_t> sel = std::move(it->second);
        picked.erase(it);
        if (sel.empty()) return;
        owners.erase(src);   // copying changes the entries' pointers
        std::vector<Node*> made;
        std::vector<Entry*> entries;
        for (size_t i : sel)
        {
            entries.push_back(src->tree.assets[i]);
            made.push_back(src->tree.assets[i]);
            stockEntries.insert(src->tree.assets[i]);
        }
        Localize(made);
        RemapBones(made, src->tree.scriptStrings);
        root.assets.insert(root.assets.begin(), entries.begin(), entries.end());
    }

    // The calling card titles and emblems a stock map carries: in a match the
    // game draws them from the map's own copy, not ui_mp's. They come as stock
    // maps have them, their pixels streamed from the console's picture packs.
    void Porter::PickCardPictures()
    {
        std::set<std::pair<std::string, std::string>> have;
        for (Entry* e : root.assets)
            if (Struct* a = As<Struct>(e->asset)) have.insert({ e->type, Name(a).value_or("") });
        for (StockFile* f : refs)
        {
            if (!f->base.starts_with("mp_") || f->base.ends_with("_load")) continue;
            std::set<std::pair<std::string, std::string>> names;
            for (Entry* e : f->tree.assets)
            {
                Struct* a = As<Struct>(e->asset);
                const std::string n = a ? Name(a).value_or("") : "";
                if (e->type == "material" && (n.starts_with("cardtitle_") || n.starts_with("cardicon_")) &&
                    !have.count({ "material", n }))
                    names.insert({ "material", n });
            }
            if (names.empty()) continue;
            Pick(f, names);
            log("  titles and emblems: " + std::to_string(names.size()) + " from " + Utf8(f->path.filename()));
            return;
        }
        Warn("no stock map given carries the calling card titles and emblems; they show as missing in matches");
    }

    Teams Porter::AddTeams(const Teams& wanted)
    {
        std::string arena;
        for (StockFile* f : refs)
            for (Entry* e : f->tree.assets)
                if (Struct* a = As<Struct>(e->asset); a && e->type == "rawfile" && arena.empty() &&
                                                      Name(a) == "mp/basemaps.arena")
                    arena = RawfileText(a);
        struct Donor
        {
            StockFile* file;
            const TeamAssets* team;
        };
        std::vector<Donor> donors;
        for (StockFile* f : refs)
        {
            std::set<std::pair<std::string, std::string>> have;
            for (Entry* e : f->tree.assets)
                if (Struct* a = As<Struct>(e->asset)) have.insert({ e->type, Name(a).value_or("") });
            for (const TeamAssets& t : AllTeamAssets())
            {
                const bool all = std::all_of(t.models.begin(), t.models.end(), [&](const char* m) { return have.count({ "xmodel", m }); }) &&
                                 std::all_of(t.materials.begin(), t.materials.end(),
                                             [&](const char* m) { return have.count({ "material", m }); });
                if (all) donors.push_back({ f, &t });
            }
        }
        std::vector<Donor> used;
        auto isUsed = [&](const TeamAssets* t) {
            return std::any_of(used.begin(), used.end(), [&](const Donor& u) { return u.team == t; });
        };
        for (const auto& [side, want] : { std::pair{ std::string("allies"), Lower(wanted.allies) },
                                          std::pair{ std::string("axis"), Lower(wanted.axis) } })
        {
            auto pick = std::find_if(donors.begin(), donors.end(), [&](const Donor& d) { return want == d.team->team; });
            if (pick == donors.end())
            {
                pick = std::find_if(donors.begin(), donors.end(),
                                    [&](const Donor& d) { return OnSide(side, d.team->team) && !isUsed(d.team); });
                if (pick == donors.end())
                    pick = std::find_if(donors.begin(), donors.end(), [&](const Donor& d) { return !isUsed(d.team); });
                if (pick == donors.end()) throw PortError("no stock map given has the " + want + " team");
                const std::vector<std::string> maps = TeamMaps(arena, want);
                Warn("the map wants the " + (want.empty() ? std::string("(none)") : want) + " team for " + side + "; using " +
                     pick->team->team + " instead (for " + want + ", add one of these stock 360 maps: " +
                     (maps.empty() ? std::string("?") : Joined(maps)) + ")");
            }
            used.push_back(*pick);
        }
        PickCardPictures();
        for (const Donor& d : used)
        {
            std::set<std::pair<std::string, std::string>> names;
            for (const char* m : d.team->models) names.insert({ "xmodel", m });
            for (const char* m : d.team->materials) names.insert({ "material", m });
            Pick(d.file, names);
        }
        for (const Donor& d : used) CopyPicked(d.file);
        for (StockFile* f : refs) CopyPicked(f);   // titles and emblems from a map no team came from
        // The 360 only knows the teams of its own maps (mp/basemaps.arena): they
        // are set in the map's script, so the game uses the assets copied in.
        const std::string gsc = "maps/mp/" + mapName + ".gsc";
        bool set = false;
        for (Entry* e : root.assets)
        {
            Struct* a = As<Struct>(e->asset);
            if (!a || e->type != "rawfile" || Name(a) != gsc) continue;
            const std::string text = RawfileText(a);
            static const std::regex main(R"(main\s*\(\s*\)\s*\{[^\n]*\n)");
            std::smatch m;
            if (!std::regex_search(text, m, main)) continue;
            const std::string line = "\tgame[ \"allies\" ] = \"" + std::string(used[0].team->team) + "\";\n\tgame[ \"axis\" ] = \"" +
                                     used[1].team->team + "\";\n";
            SetRawfileText(a, m.prefix().str() + m[0].str() + line + m.suffix().str());
            set = true;
            break;
        }
        if (!set) Warn("couldn't set the teams in " + gsc);
        log("  teams: " + std::string(used[0].team->team) + " (from " + Utf8(used[0].file->path.filename()) + ") vs " +
            used[1].team->team + " (from " + Utf8(used[1].file->path.filename()) + ")");
        return { used[0].team->team, used[1].team->team };
    }
}
