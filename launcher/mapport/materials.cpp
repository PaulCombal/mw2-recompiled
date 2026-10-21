// Materials and shader sets. PC shaders can't run on the 360, so each shader
// set is swapped for the 360's own of the same name (or the nearest one the
// stock files have), and a material's render state comes from a stock
// material using that set; its draw order and face culling stay the PC's.
#include "porter.h"

#include <algorithm>
#include <compare>
#include <map>
#include <regex>

namespace mapport
{
    namespace
    {
        bool Contains(const std::string& s, std::string_view p) { return s.find(p) != std::string::npos; }

        std::vector<std::string> Split(const std::string& s, char sep)
        {
            std::vector<std::string> out;
            size_t from = 0;
            while (true)
            {
                const size_t at = s.find(sep, from);
                out.push_back(s.substr(from, at == std::string::npos ? std::string::npos : at - from));
                if (at == std::string::npos) return out;
                from = at + 1;
            }
        }

        // A lit world or model shader set's name in parts: wc_l_sm_t0c0s0_nocast
        // is kind "wc_l_sm_", inputs t0 c0 s0, extra "_nocast".
        struct TechsetCodes
        {
            std::string kind;
            std::vector<std::string> inputs;
            std::string extra;
        };
        std::optional<TechsetCodes> CodesOf(const std::string& name)
        {
            static const std::regex re("^((?:wc|mc)_l_(?:sm_)?(?:ua_)?)((?:[a-z]\\d)+)(_.*)?$");
            std::smatch m;
            if (!std::regex_match(name, m, re)) return std::nullopt;
            TechsetCodes c{ m[1].str(), {}, m[3].matched ? m[3].str() : "" };
            const std::string in = m[2].str();
            for (size_t i = 0; i + 1 < in.size(); i += 2) c.inputs.push_back(in.substr(i, 2));
            return c;
        }

        size_t Common(const std::vector<std::string>& a, const std::vector<std::string>& b)
        {
            const std::set<std::string> sa(a.begin(), a.end());
            size_t n = 0;
            for (const std::string& x : std::set<std::string>(b.begin(), b.end())) n += sa.count(x);
            return n;
        }

        // The value seen most often, the first seen of those tied.
        std::optional<uint32_t> MostCommon(const std::vector<uint32_t>& values)
        {
            std::vector<std::pair<uint32_t, int>> counts;
            for (uint32_t v : values)
            {
                auto it = std::find_if(counts.begin(), counts.end(), [&](const auto& c) { return c.first == v; });
                if (it == counts.end()) counts.push_back({ v, 1 });
                else it->second++;
            }
            if (counts.empty()) return std::nullopt;
            auto best = counts.begin();
            for (auto it = counts.begin(); it != counts.end(); ++it)
                if (it->second > best->second) best = it;
            return best->first;
        }
    }

    // ---- shader sets ----------------------------------------------------------

    bool Porter::HaveTechset(const std::string& name) const
    {
        if (!materialTemplates.Has(name)) return false;
        const AssetKey key{ "MaterialTechniqueSet", name };
        return resident.Has(key) || library.Has(key) || named.count({ key.type, name });
    }

    // Radiant's tool shader sets (wc_tools, mc_tools), which draw nothing in
    // game, and the HDR portal sets (hidden like them).
    bool Porter::IsToolsTechset(const std::string& name) const
    {
        const std::vector<std::string> parts = Split(name, '_');
        return (parts.size() > 1 && parts[1] == "tools") || Contains(name, "_distfalloff");
    }

    // A stock alpha tested shader set of the same kind (wc_/mc_) with a colour
    // picture and as few other inputs as there are: given a see-through
    // picture, it draws nothing (no shadow either, which reads the same picture).
    std::optional<std::string> Porter::InvisibleTechset(const std::string& name) const
    {
        // Fewest inputs, then a plain lit one, then the last name.
        struct Score
        {
            int fewerInputs;
            bool litSm;
            std::string name;
            auto operator<=>(const Score&) const = default;
        };
        std::optional<std::string> best;
        std::optional<Score> score;
        for (const auto& [cand, tpl] : materialTemplates)
        {
            const std::optional<TechsetCodes> got = CodesOf(cand);
            if (!got || !got->kind.starts_with(name.substr(0, 3)) || !got->extra.empty() || got->inputs[0][0] != 't' ||
                std::find(got->inputs.begin(), got->inputs.end(), "c0") == got->inputs.end() || !HaveTechset(cand))
                continue;
            const Score sc{ -int(got->inputs.size()), got->kind == name.substr(0, 3) + "l_sm_", cand };
            if (!score || sc > *score)
            {
                best = cand;
                score = sc;
            }
        }
        return best;
    }

    bool Porter::Hides(const std::string& name, const std::string& near) const
    {
        return IsToolsTechset(name) && InvisibleTechset(name) == near;
    }

    // The same kind, blend and extras as `name`, with the fewest inputs added
    // (dropping one is harmless: a picture the shader doesn't read).
    std::optional<std::string> Porter::SimilarTechset(const std::string& name) const
    {
        const std::optional<TechsetCodes> want = CodesOf(name);
        if (!want) return std::nullopt;
        // Fewest inputs added, then most kept, then the last name.
        struct Score
        {
            int fewerAdded;
            size_t kept;
            std::string name;
            auto operator<=>(const Score&) const = default;
        };
        std::optional<std::string> best;
        std::optional<Score> score;
        for (const auto& [cand, tpl] : materialTemplates)
        {
            const std::optional<TechsetCodes> got = CodesOf(cand);
            if (!got || got->kind != want->kind || got->extra != want->extra || got->inputs[0] != want->inputs[0] ||
                !HaveTechset(cand))
                continue;
            const size_t extra = std::set<std::string>(got->inputs.begin(), got->inputs.end()).size() -
                                 Common(got->inputs, want->inputs);
            const Score sc{ -int(extra), Common(got->inputs, want->inputs), cand };
            if (!score || sc > *score)
            {
                best = cand;
                score = sc;
            }
        }
        return best;
    }

    // A stock shader set to use in place of `name`: lit world and model sets
    // keep their kind and blend and drop or add inputs (the material's missing
    // pictures are filled in); tool sets one that draws nothing; others the
    // one sharing the most leading name parts, and a wc_/mc_ one sharing only
    // that the plain lit set.
    std::optional<std::string> Porter::NearestTechset(const std::string& name)
    {
        if (const std::string* swapped = techsetSwaps.Find(name)) return *swapped;
        std::optional<std::string> best = IsToolsTechset(name) ? InvisibleTechset(name) : std::nullopt;
        if (!best) best = SimilarTechset(name);
        if (best)
        {
            techsetSwaps.Set(name, *best);
            return best;
        }
        const std::vector<std::string> want = Split(name, '_');
        const std::set<std::string> wantSet(want.begin(), want.end());
        std::pair<size_t, int64_t> score{ 0, 0 };
        for (const auto& [cand, tpl] : materialTemplates)
        {
            const AssetKey key{ "MaterialTechniqueSet", cand };
            if (!(resident.Has(key) || library.Has(key) || named.count({ key.type, cand }))) continue;
            const std::vector<std::string> parts = Split(cand, '_');
            size_t lead = 0;
            while (lead < std::min(want.size(), parts.size()) && want[lead] == parts[lead]) lead++;
            const std::set<std::string> partSet(parts.begin(), parts.end());
            int64_t missing = 0;
            for (const std::string& p : partSet) missing += !wantSet.count(p);
            const std::pair<size_t, int64_t> sc{ lead, int64_t(Common(want, parts)) - missing };
            if (lead && sc > score)
            {
                best = cand;
                score = sc;
            }
        }
        if ((name.starts_with("wc_") || name.starts_with("mc_")) && score.first < 2 &&
            HaveTechset(name.substr(0, 3) + "l_sm_r0c0"))
            best = name.substr(0, 3) + "l_sm_r0c0";   // only "wc"/"mc" in common: plain lit
        if (best) techsetSwaps.Set(name, *best);
        return best;
    }

    bool Porter::PreMaterialTechniqueSet(Struct* d)
    {
        std::string name = AssetName(d).value_or("");
        if (name.starts_with(",")) name.erase(0, 1);   // the PC file only names it too
        while (true)
        {
            const AssetKey key{ "MaterialTechniqueSet", name };
            if (resident.Has(key) || named.count({ key.type, name }))
            {
                Replace(d, *Reference(key.type, name));
                return true;
            }
            if (Struct** src = library.Find(key))
            {
                Replace(d, *CopyIn(*src));
                return true;
            }
            const std::optional<std::string> near = NearestTechset(name);
            if (!near || *near == name) throw PortError("no 360 shader set " + name + " in the stock files given");
            if (Hides(name, *near)) invisibleTs.insert(d);
            d->SetChild("name", A().Make<String>(*near));
            name = *near;
        }
    }

    // Whether stock shader set `name` (or the one it is remapped to) has the
    // technique, or nothing when no stock file given has it.
    std::optional<bool> Porter::TechsetHas(const std::string& raw, int technique)
    {
        if (raw.empty()) return std::nullopt;
        const std::string name = WithoutCommas(raw);
        if (auto it = techsetHasCache.find({ name, technique }); it != techsetHasCache.end()) return it->second;
        // Its techniques, or nothing for a set that is only a name (most stock
        // maps only name the sets common_mp has: every technique empty).
        std::function<const PtrList*(Struct*)> techniques = [&](Struct* ts) -> const PtrList* {
            if (!ts) return nullptr;
            if (Struct* rm = DerefStruct(ts->Child("remappedTechniqueSet")))
                if (const PtrList* t = techniques(rm)) return t;
            const PtrList* techs = As<PtrList>(Deref(ts->Child("techniques")));
            if (!techs || std::all_of(techs->items.begin(), techs->items.end(), [](Node* t) { return !t; })) return nullptr;
            return techs;
        };
        if (!stockTechniques)
        {
            // Stock shader sets by name, the copies that carry their techniques:
            // reached through the stock materials using them.
            stockTechniques.emplace();
            auto add = [&](const AssetKey& k, Struct* v) {
                Struct* ts = k.type == "Material" ? DerefStruct(v->Child("techniqueSet"))
                             : k.type == "MaterialTechniqueSet" ? v
                                                                : nullptr;
                if (const PtrList* techs = techniques(ts))
                {
                    std::string n = AssetName(ts).value_or("");
                    n = WithoutCommas(std::move(n));
                    stockTechniques->emplace(n, techs->items);
                }
            };
            for (auto& [k, v] : library) add(k, v);
            for (auto& [k, v] : resident) add(k, v);
            for (auto& [k, v] : commonMaterials) add(k, v);
        }
        std::optional<bool> out;
        if (auto it = stockTechniques->find(name); it != stockTechniques->end())
            out = int(it->second.size()) > technique && it->second[size_t(technique)] != nullptr;
        techsetHasCache[{ name, technique }] = out;
        return out;
    }

    // ---- materials ----------------------------------------------------------

    // The face culling a PC material draws itself with (the most common over
    // its drawing passes: 4 up to the debug views at 44; 0-3 are depth and
    // shadow passes, which some materials draw two-sided), or nothing.
    std::optional<uint32_t> Porter::PcCullOf(Struct* d)
    {
        const auto words = StateWords(d->Child("stateBitsTable"));
        if (!words || words->empty()) return std::nullopt;
        std::vector<uint32_t> votes;
        const auto entry = d->Bytes("stateBitsEntry");
        for (size_t t = 4; t < std::min<size_t>(44, entry.size()); t++)
            if (entry[t] < words->size()) votes.push_back(((*words)[entry[t]].first >> 14) & 3);
        return MostCommon(votes);
    }

    bool Porter::PreMaterial(Struct* d)
    {
        const std::string name = AssetName(d).value_or(Name(d).value_or(""));   // (Name: one shared with another asset)
        if (name.starts_with(","))
        {
            // The PC file only names it (the game has it loaded): so does the 360 file.
            Replace(d, *Reference("Material", name.substr(1)));
            return true;
        }
        bool own = false;
        if (Array* tt = As<Array>(d->Child("textureTable")))
            for (Node* n : tt->items)
                if (Struct* t = As<Struct>(n))
                    if (Struct* u = Embedded(t, "u"))
                        if (Struct* image = DerefStruct(u->Child("image"))) own |= InIwd(AssetName(image).value_or(""));
        if (resident.Has({ "Material", name }) && !own)
        {
            // The game has it loaded and the map brings no pictures of its own
            // for it (a map's $levelbriefing does, so it gets its own copy).
            Replace(d, *Reference("Material", name));
            return true;
        }
        Struct* ts = DerefStruct(d->Child("techniqueSet"));
        std::string tsname = ts ? AssetName(ts).value_or("") : "";
        if (tsname.starts_with(",")) tsname.erase(0, 1);   // converted to a reference already
        const std::string origTs = tsname;
        Struct* const* found = materialTemplates.Find(tsname);
        Struct* tpl = found ? *found : nullptr;
        // A stock material of the same name and shader set: its own render state
        // (culling, draw order), not that of another one sharing the set.
        Struct** same = library.Find({ "Material", name });
        if (!same) same = commonMaterials.Find({ "Material", name });
        if (same && !tsname.empty())
            if (Struct* sts = DerefStruct((*same)->Child("techniqueSet")))
            {
                std::string sn = AssetName(sts).value_or("");
                sn = WithoutCommas(std::move(sn));
                if (sn == tsname) tpl = *same;
            }
        if (!tpl && !tsname.empty())
            if (const std::optional<std::string> near = NearestTechset(tsname))
            {
                // No stock file given has this shader set: the closest one there is.
                if (Hides(tsname, *near)) invisibleTs.insert(ts);
                ts->SetChild("name", A().Make<String>(*near));
                tsname = *near;
                tpl = *materialTemplates.Find(*near);
            }
        if (ts && invisibleTs.count(ts)) invisible.insert(d);
        if (!tpl) throw PortError("material " + name + ": no stock 360 material uses shader set " + tsname);
        // PC render state means nothing to the 360: it comes from the template,
        // all but its face culling (the same bits on both).
        if (const std::optional<uint32_t> cull = PcCullOf(d); cull == 1u || cull == 3u) pcCull[d] = *cull;
        d->PopChild("stateBitsTable");
        materialTemplate[d] = tpl;
        // The PC's draw order (sort key) means the same on the 360. Kept unless
        // the shader set hides it, or stock uses the key only for the other kind
        // of set (with the lit technique: sort keys 0-33; without: 34 and up;
        // the game sorts one key's materials by that technique's shaders).
        if (!stockSortKeys)
        {
            stockSortKeys.emplace();
            auto add = [&](const AssetKey& k, Struct* v) {
                if (k.type != "Material") return;
                Struct* sts = DerefStruct(v->Child("techniqueSet"));
                const std::optional<bool> lit = TechsetHas(sts ? AssetName(sts).value_or("") : "");
                (*stockSortKeys)[v->Sub("info")->Int("sortKey")].insert(lit ? int(*lit) : -1);
            };
            for (auto& [k, v] : library) add(k, v);
            for (auto& [k, v] : resident) add(k, v);
            for (auto& [k, v] : commonMaterials) add(k, v);
        }
        const int64_t sort = d->Sub("info")->Int("sortKey");
        auto sides = stockSortKeys->find(sort);
        if (!invisible.count(d) && !Contains(origTs, "_distfalloff") && sides != stockSortKeys->end())
        {
            const std::optional<bool> ours = TechsetHas(tsname);
            std::set<int> known = sides->second;
            known.erase(-1);
            if (!ours || known.empty() || known == std::set<int>{ int(*ours) }) pcSort[d] = sort;
            else if (const int64_t theirs = tpl->Sub("info")->Int("sortKey"); sort != theirs)
                sortKeyKindSwapped.push_back({ name, sort, theirs });
        }
        return false;
    }

    void Porter::PostMaterial(Struct* d)
    {
        Struct* tpl = materialTemplate.at(d);
        materialTemplate.erase(d);
        for (const char* k : { "stateBitsEntry", "stateBitsCount", "stateFlags", "cameraRegion", "stateBitsTable", "unknown" })
        {
            const auto from = tpl->Bytes(k);
            std::copy(from.begin(), from.end(), d->Bytes(k).begin());
        }
        Struct* info = d->Sub("info");
        Struct* tinfo = tpl->Sub("info");
        auto sort = pcSort.find(d);
        info->Set("sortKey", sort != pcSort.end() ? sort->second : tinfo->Int("sortKey"));
        if (sort != pcSort.end()) pcSort.erase(sort);
        // The game builds drawSurf itself when it registers the material; stock
        // 360 files always hold zero here (the PC keeps its own bit layout).
        {
            const auto from = tinfo->Bytes("drawSurf");
            std::copy(from.begin(), from.end(), info->Bytes("drawSurf").begin());
        }
        Node* c = tpl->Child("stateBitsTable");
        if (Ref* r = As<Ref>(c)) c = r->rel == 0 ? r->target : nullptr;
        if (!c) throw PortError("a template material's render state can't be copied");
        const auto words = StateWords(c).value_or(std::vector<std::pair<uint32_t, uint32_t>>{});
        std::vector<uint32_t> culls;
        for (uint8_t e : d->Bytes("stateBitsEntry"))
            if (e < words.size()) culls.push_back((words[e].first >> 14) & 3);
        const std::optional<uint32_t> main = MostCommon(culls);
        // (Only where the template draws mostly otherwise: a same-named stock
        // material drawing two-sided already keeps its own state.)
        if (auto cull = pcCull.find(d); cull != pcCull.end())
        {
            if (main && *main != cull->second)
            {
                c = WithCull(c, cull->second);
                twoSided++;
            }
            pcCull.erase(cull);
        }
        d->SetChild("stateBitsTable", c);
        d->SetPtr("stateBitsTable", kFollowing);
        const bool hidden = invisible.erase(d) != 0;
        const std::string name = AssetName(d).value_or("");
        if (hidden)
        {
            // Its own pictures go: every one the shader set reads is a stand-in,
            // the colour one see-through.
            d->SetChild("textureTable", A().Make<Array>());
            d->SetPtr("textureTable", kFollowing);
            d->Set("textureCount", 0);
            if (std::find(invisibleMaterials.begin(), invisibleMaterials.end(), name) == invisibleMaterials.end())
                invisibleMaterials.push_back(name);
        }
        FillTextures(d, tpl, name, hidden);
    }

    // Gives material `d` every picture its shader set reads (as stock material
    // `tpl`, of the same set, has them) that it lacks: a swapped-in set can
    // want one the PC material never had, and a shader without its picture
    // draws nothing. Stand-ins: a flat normal map, no shine, magenta colour.
    void Porter::FillTextures(Struct* d, Struct* tpl, const std::string& name, bool hidden)
    {
        Array* table = As<Array>(d->Child("textureTable"));
        std::vector<Node*> items = table ? table->items : std::vector<Node*>{};
        std::set<uint32_t> have;
        for (Node* n : items)
            if (Struct* e = As<Struct>(n)) have.insert(uint32_t(e->U("nameHash")));
        const zone::StructInfo* imageInfo = X.infos.at("GfxImage");
        std::vector<Node*> added;
        Array* theirs = As<Array>(tpl->Child("textureTable"));
        for (Node* n : theirs ? theirs->items : std::vector<Node*>{})
        {
            Struct* e = As<Struct>(n);
            if (!e || have.count(uint32_t(e->U("nameHash")))) continue;
            Struct* u = Embedded(e, "u");
            Struct* img = u ? DerefStruct(u->Child("image")) : nullptr;
            std::string iname = img ? AssetName(img).value_or("") : "";
            iname = WithoutCommas(std::move(iname));
            const int64_t semantic = e->Int("semantic");
            Struct* made;
            if (semantic == 5) made = Reference("GfxImage", "$identitynormalmap");
            else if (semantic == 8) made = Reference("GfxImage", "$black");
            else if (hidden)
            {
                made = SeeThrough(Struct::Zero(imageInfo->ctype, true),
                                  "~invisible_" + std::to_string(invisibleMaterials.size()));
                made->asset = imageInfo->name;
            }
            else if (iname.starts_with("$") &&
                     (resident.Has({ "GfxImage", iname }) || named.count({ "GfxImage", iname })))
                made = Reference("GfxImage", iname);
            else
            {
                made = Magenta(Struct::Zero(imageInfo->ctype, true), "~magenta_" + std::to_string(magentaMaterials.size()));
                made->asset = imageInfo->name;
                if (std::find(magentaMaterials.begin(), magentaMaterials.end(), name) == magentaMaterials.end())
                    magentaMaterials.push_back(name);
            }
            Struct* entry = e->Copy();
            Struct* mu = entry->Sub("u");
            std::fill(mu->Data(), mu->Data() + mu->Size(), uint8_t(0));
            Store(mu->Data(), 4, kFollowing, mu->big);
            mu->used.clear();
            Children* ch = A().MakeChildren();
            ch->Set("image", made);
            mu->SetCh(ch);
            added.push_back(entry);
        }
        if (added.empty()) return;
        items.insert(items.end(), added.begin(), added.end());
        std::stable_sort(items.begin(), items.end(), [](Node* a, Node* b) {
            return static_cast<Struct*>(a)->U("nameHash") < static_cast<Struct*>(b)->U("nameHash");
        });
        Array* out = A().Make<Array>();
        out->items = std::move(items);
        d->SetChild("textureTable", out);
        d->SetPtr("textureTable", kFollowing);
        d->Set("textureCount", int64_t(out->items.size()));
    }
}
