// The conversion's main sequence and the steps that look at the whole tree.
#include "porter.h"

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <queue>
#include <regex>

namespace mapport
{
    using zone::Type;

    namespace
    {
        // Every Struct, Array, Leaf and PtrList the writer will write: those the
        // asset list holds and those reached only through a pointer back to
        // earlier data, each once.
        std::vector<Node*> Reach(const std::vector<Node*>& roots)
        {
            std::unordered_set<const Node*> seen;
            std::vector<Node*> stack = roots, out;
            auto follow = [&](Node* x) {
                if (Ref* r = As<Ref>(x))
                {
                    Node* t = r->target;
                    if (Slot* s = As<Slot>(t)) t = s->asset;
                    else if (Entry* e = As<Entry>(t)) t = e->asset;
                    if (As<Struct>(t) || As<Array>(t) || As<PtrList>(t)) stack.push_back(t);
                }
                else if (As<Struct>(x) || As<Array>(x) || As<Leaf>(x) || As<PtrList>(x)) stack.push_back(x);
            };
            while (!stack.empty())
            {
                Node* o = stack.back();
                stack.pop_back();
                if (!o || !seen.insert(o).second) continue;
                out.push_back(o);
                if (Struct* s = As<Struct>(o))
                {
                    for (const zone::Member& m : s->type->members)
                    {
                        if (m.HasPointer() || m.offset + m.size > s->Size()) continue;
                        if (Node* late = s->Late(m.name))
                        {
                            stack.push_back(late);
                            continue;
                        }
                        const Type* t = m.type->Unwrapped();
                        if (t->kind != Type::Struct && t->kind != Type::Union) continue;
                        if (m.mods.empty()) stack.push_back(s->Sub(m));
                        else
                        {
                            size_t n = 1;
                            for (int x : m.mods) n *= size_t(x);
                            for (size_t k = 0; k < n; k++) stack.push_back(s->Sub(m, int64_t(k)));
                        }
                    }
                    if (Children* ch = s->Ch())
                        for (const auto& [k, c] : ch->items)
                        {
                            EachHeld(c, follow);
                            if (As<PtrList>(c)) stack.push_back(c);
                        }
                }
                else if (Array* a = As<Array>(o))
                    for (Node* x : a->items) follow(x);
                else if (PtrList* pl = As<PtrList>(o))
                    for (Node* x : pl->items) follow(x);
            }
            return out;
        }

        std::vector<Node*> Reach(const std::vector<Entry*>& entries)
        {
            std::vector<Node*> roots;
            for (Entry* e : entries) roots.push_back(e->asset);
            return Reach(roots);
        }

        // The images/*.iwi pictures of some .iwd files, by name in lower case (the
        // game looks names up whatever their case or slashes); later files win.
        ZipIndex PictureIndex(const std::vector<Zip>& zips)
        {
            ZipIndex found;
            for (const Zip& z : zips)
                for (size_t k = 0; k < z.Names().size(); k++)
                {
                    std::string low = Lower(z.Names()[k]);
                    std::replace(low.begin(), low.end(), '\\', '/');
                    if (low.starts_with("images/") && low.size() > 11 && low.ends_with(".iwi"))
                        found[low.substr(7, low.size() - 11)] = { &z, k };
                }
            return found;
        }
    }

    // ---- the stock files' indexes -------------------------------------------

    Porter::Porter(Tree& pc, std::vector<StockFile*> stock, std::vector<Zip> archives, const std::vector<Zip>& game,
                   Log logTo, int budget)
        : P(zone::PcRules()), X(zone::XboxRules()), root(pc), refs(std::move(stock)), log(std::move(logTo)),
          textureBudget(budget), iwds(std::move(archives)), gameIwds(game), mapPictures(PictureIndex(iwds)),
          gamePictures(PictureIndex(gameIwds))
    {
        for (StockFile* f : refs)
        {
            AssetIndex<Struct*> idx;
            Ordered<std::string, Struct*> aliases;
            ForEach(f->tree.assets, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset()) return;
                std::optional<std::string> name = AssetName(o);
                if (!Named(name)) name = Name(o);
                if (name) idx.FindOrAdd({ std::string(o->asset), *name }, o);
                if (o->IsAsset("snd_alias_list_t"))
                    if (auto alias = AliasName(o)) aliases.FindOrAdd(*alias, o);
            });
            if (f->resident)
                for (const auto& [type, names] : PoolNames(f->tree.assets))
                    for (const std::string& n : names)
                    {
                        loaded.insert({ type, n });
                        if (f->base == "common_mp") commonNames.insert({ type, n });
                    }
            std::unordered_map<const Node*, int64_t> order;
            for (size_t i = 0; i < f->tree.assets.size(); i++) order.emplace(f->tree.assets[i]->asset, int64_t(i));
            for (auto& [k, v] : aliases)
            {
                if (k.starts_with(",")) continue;
                StockAlias* old = stockAliases.Find(k);
                auto at = order.find(v);
                if (!old || (old->resident && !f->resident))
                    stockAliases.Set(k, { v, f->resident, f, at == order.end() ? -1 : at->second });
            }
            if (!f->resident)
            {
                for (Entry* e : f->tree.assets)
                    if (Struct* a = As<Struct>(e->asset); a && e->type == "fx")
                        if (auto n = Name(a); n && !n->starts_with(",")) stockFx.FindOrAdd(*n, { a, f });
                for (auto& [k, v] : idx)
                    if (k.type == "XModel" && !k.name.starts_with(",")) stockModels.FindOrAdd(k.name, { v, f });
            }
            for (auto& [k, v] : idx)
            {
                if (k.name.starts_with(","))
                {
                    named.insert({ k.type, k.name.substr(1) });
                    continue;
                }
                if (f->base == "common_mp")
                {
                    // Not used by name: only what a map's own materials (heat
                    // distortion, ...) need a stock material of the same shader set for.
                    if (k.type == "Material") commonMaterials.FindOrAdd(k, v);
                    continue;
                }
                if (f->resident) resident.FindOrAdd(k, v);
                else library.FindOrAdd(k, v);
            }
        }
        // A stock picture held in its file, of each format: the header to copy.
        auto pictureTemplate = [&](const AssetKey& k, Struct* v) {
            if (k.type != "GfxImage" || v->Ptr("pixels") != kFollowing || v->Int("streaming") != 0) return;
            const int format = v->Bytes("texture")[32] & 0x3F;
            templates.emplace(std::pair{ format, v->Int("mapType") == 5 }, v);
        };
        for (auto& [k, v] : resident) pictureTemplate(k, v);
        for (auto& [k, v] : library) pictureTemplate(k, v);
        // Stock lightmaps: their channel order differs from other pictures'.
        for (auto& [k, v] : library)
            if (k.type == "GfxImage" && k.name.starts_with("*lightmap") && v->Ptr("pixels") == kFollowing)
                lightmapTemplates.FindOrAdd(k.name.substr(k.name.rfind('_') + 1), v);
        // Stock 360 materials by the shader set they use (render state templates).
        auto byTechset = [&](const AssetKey& k, Struct* v) {
            if (k.type != "Material") return;
            if (Struct* ts = DerefStruct(v->Child("techniqueSet")))
                if (auto tn = AssetName(ts))
                {
                    std::string n = *tn;
                    n = WithoutCommas(std::move(n));
                    materialTemplates.FindOrAdd(n, v);
                }
        };
        for (auto& [k, v] : library) byTechset(k, v);
        for (auto& [k, v] : resident) byTechset(k, v);
        for (auto& [k, v] : commonMaterials) byTechset(k, v);
    }

    void Porter::Warn(const std::string& msg)
    {
        if (std::find(warnings.begin(), warnings.end(), msg) != warnings.end()) return;
        warnings.push_back(msg);
        log("  note: " + msg);
    }

    // ---- the picture budget -------------------------------------------------

    // Which pictures lose top mip levels so the map's pictures fit the budget:
    // the largest picture (shine maps counted double) goes down a level at a
    // time. Lightmaps, reflection probes and loading screens stay as they are.
    void Porter::PlanTextureBudget()
    {
        constexpr double kTilingPad = 1.28;   // the 360's tiling: the block comes to about this much more
        const int64_t budget = int64_t(textureBudget * 1048576 / kTilingPad);
        int64_t fixed = 0;
        struct Item
        {
            std::vector<size_t> sizes;
            int w, h;
        };
        Ordered<std::string, Item> items;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || !o->IsAsset("GfxImage")) return;
            auto name = AssetName(o);
            if (!Named(name) || OnlyNamed(name) || name->starts_with("loadscreen")) return;
            if (resident.Has({ "GfxImage", *name }) && (name->starts_with("$") || !InIwd(*name))) return;
            if (StreamsFromStock(*name)) return;   // copied from stock, streamed
            Struct* tex = Embedded(o, "texture");
            if (Struct* ld = tex ? As<Struct>(tex->Child("loadDef")) : nullptr; ld && ld->Int("resourceSize"))
            {
                fixed += ld->Int("resourceSize");
                return;
            }
            auto found = mapPictures.find(Lower(*name));
            if (found == mapPictures.end() && (found = gamePictures.find(Lower(*name))) == gamePictures.end()) return;
            Picture pic;
            try
            {
                pic = ReadIwi(found->second.zip->Read(found->second.index));
            }
            catch (const Error&)
            {
                return;
            }
            size_t total = 0;
            for (const Mip& m : pic.mips) total += m.size();
            if (pic.cube || pic.mips.size() < 2) fixed += int64_t(total);
            else
            {
                Item it{ {}, pic.width, pic.height };
                for (const Mip& m : pic.mips) it.sizes.push_back(m.size());
                items.Set(*name, it);
            }
        });
        int64_t total = fixed;
        for (auto& [n, it] : items)
            for (size_t s : it.sizes) total += int64_t(s);
        const int64_t before = total;
        // Specular (shine) maps count double: they go down before colour and
        // normal maps of the same size, whose loss shows far more.
        auto weight = [](const std::string& n, int64_t size) {
            const std::string low = Lower(n);
            const bool spec = low.starts_with("~") || low.find("_spc") != std::string::npos ||
                              low.find("_spec") != std::string::npos || low.find("_cos") != std::string::npos ||
                              low.find("_gloss") != std::string::npos;
            return -size * (spec ? 2 : 1);
        };
        // The smallest (weight, name) first.
        using Weighed = std::pair<int64_t, std::string>;   // (weight, picture)
        std::priority_queue<Weighed, std::vector<Weighed>, std::greater<>> heap;
        for (auto& [n, it] : items)
        {
            int64_t sum = 0;
            for (size_t s : it.sizes) sum += int64_t(s);
            heap.push({ weight(n, sum), n });
        }
        while (total > budget && !heap.empty())
        {
            const std::string n = heap.top().second;
            heap.pop();
            const Item& it = *items.Find(n);
            const int k = mipDrop.count(n) ? mipDrop[n] : 0;
            if (size_t(k + 1) >= it.sizes.size() || std::min(it.w, it.h) >> (k + 1) < 32) continue;
            total -= int64_t(it.sizes[size_t(k)]);
            mipDrop[n] = k + 1;
            int64_t rest = 0;
            for (size_t i = size_t(k + 1); i < it.sizes.size(); i++) rest += int64_t(it.sizes[i]);
            heap.push({ weight(n, rest), n });
        }
        if (!mipDrop.empty())
        {
            char line[256];
            std::snprintf(line, sizeof line,
                          "  pictures: about %.0f MB, over the %d MB budget, so %zu pictures lose their top mip levels "
                          "(about %.0f MB left)",
                          double(before) * kTilingPad / 1048576, textureBudget, mipDrop.size(),
                          double(total) * kTilingPad / 1048576);
            log(line);
            if (total > budget)
            {
                std::snprintf(line, sizeof line,
                              "the pictures still come to about %.0f MB (budget %d MB): the rest are lightmaps, probes and "
                              "pictures that are already small",
                              double(total) * kTilingPad / 1048576, textureBudget);
                Warn(line);
            }
        }
    }

    // ---- the conversion -----------------------------------------------------

    void Porter::Convert()
    {
        std::vector<Entry*>& ents = root.assets;
        if (textureBudget) PlanTextureBudget();
        std::vector<std::string> fromStock;
        for (Entry* e : ents)
        {
            Struct* d = As<Struct>(e->asset);
            if (e->type != "rawfile" || !d) continue;
            // IW4x's ZoneBuilder signs a file with a raw file stored as it is
            // but marked compressed: the 360 would take it for a zlib stream.
            if (IsBuilderSignature(d))
            {
                SetRawfileText(d, "");
                continue;
            }
            // The stock 360 map's own script: PC scripts can call what only
            // later PC patches have (PC mp_rust's killTrigger).
            if (Struct** src = library.Find({ "RawFile", Name(d).value_or("") }))
            {
                std::optional<std::string> text;
                try
                {
                    text = RawfileText(*src);
                }
                catch (const Error&)
                {
                }
                if (text && *text != RawfileText(d))
                {
                    SetRawfileText(d, *text);
                    fromStock.push_back(*Name(d));
                }
            }
            else EditScript(d);
        }
        if (!fromStock.empty())
        {
            std::string list;
            for (const std::string& n : fromStock) list += (list.empty() ? "" : ", ") + n;
            log("  " + std::to_string(fromStock.size()) + " script" + (fromStock.size() > 1 ? "s" : "") +
                " from the stock 360 files: " + list);
        }
        Struct* gfx = nullptr;
        for (Entry* e : ents)
            if (e->type == "gfx_map" && As<Struct>(e->asset))
            {
                gfx = As<Struct>(e->asset);
                break;
            }
        mapName = gfx ? std::regex_replace(Name(gfx).value_or(""), std::regex("^maps/mp/|\\.d3dbsp$"), "") : "";
        worldChecksum = gfx ? gfx->Int("checksum") : 0;
        // Some PC files list the pieces of a model or shader set as assets of
        // their own; stock 360 files never do. The PC shaders go; model
        // surfaces are written where the model that uses them points at them.
        std::unordered_set<const Node*> gone;
        std::unordered_map<const Node*, Slot*> slots;
        for (Entry* e : ents)
        {
            Struct* a = As<Struct>(e->asset);
            if (!a) continue;
            if (e->type == "pixelshader" || e->type == "vertexshader" || e->type == "vertexdecl") gone.insert(a);
            if (e->type == "xmodelsurfs")
            {
                if (!a->slot) a->slot = A().Make<Slot>(a);
                slots[e] = a->slot;
            }
        }
        // Pointers to a model surfaces asset's entry become pointers to its slot.
        if (!slots.empty())
            ForEach(ents, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->Ch()) return;
                for (auto& [k, c] : o->Ch()->items)
                    EachHeld(c, [&](Node* x) {
                        Ref* r = As<Ref>(x);
                        if (!r || r->rel != 4) return;
                        auto found = slots.find(r->target);
                        if (found == slots.end()) return;
                        r->target = found->second;
                        r->rel = 0;
                        r->t = {};
                    });
            });
        std::erase_if(ents, [](Entry* e) {
            return e->type == "pixelshader" || e->type == "vertexshader" || e->type == "vertexdecl" || e->type == "xmodelsurfs";
        });
        StockEffects();
        AddDestructibleAliases();
        StockSounds();
        StockAnims();
        StockModelSwap();
        NameCommonAssets();
        EffectSisterMaterials();
        const std::vector<Struct*> orphans = MarkOrphans(gone, false);
        for (Entry* e : ents)
        {
            if (e->type == "vertexshader" || e->type == "vertexdecl")
                throw PortError("top-level " + e->type + " assets can't be converted");
            if (Struct* a = As<Struct>(e->asset)) ConvertAsset(a);
        }
        for (Struct* a : orphans)
        {
            ConvertAsset(a);
            a->forward = true;
        }
        if (!extraAssets.empty())
        {
            // Assets made here (merged decal layers' composite materials): before
            // the world, whose pointers to them must point back; shader sets
            // first, the materials point at them.
            size_t at = ents.size();
            for (size_t i = 0; i < ents.size(); i++)
                if (ents[i]->type == "gfx_map")
                {
                    at = i;
                    break;
                }
            std::vector<Entry*> made = extraAssets;
            std::stable_sort(made.begin(), made.end(), [](Entry* a, Entry* b) {
                return (a->type != "techset") < (b->type != "techset");
            });
            ents.insert(ents.begin() + ptrdiff_t(at), made.begin(), made.end());
            PicturesBeforeComposites(at, at + made.size());
        }
        if (!breakables.empty()) AddBreakables();
        if (fromGame) log("  " + std::to_string(fromGame) + " pictures come from the PC game's own .iwd files");
        if (!sortKeyKindSwapped.empty())
        {
            std::vector<std::string> swaps;
            for (const auto& [name, pc, stock] : sortKeyKindSwapped)
                swaps.push_back(name + " " + std::to_string(pc) + "->" + std::to_string(stock));
            log("  " + std::to_string(swaps.size()) +
                " PC sort keys stock uses only for the other kind of material replaced by the template's (" +
                Joined(swaps, 5, ", ...") + ")");
        }
        if (twoSided)
            log("  " + std::to_string(twoSided) +
                " materials drawn two-sided as on the PC (the stock render state they take culls back faces)");
        if (dxnCount) log("  " + std::to_string(dxnCount) + " normal maps converted to DXN, as the 360 keeps them");
        log("  " + std::to_string(streamedStock) + " pictures stream from the 360's own picture packs, as stock maps have them (" +
            std::to_string(stockStreamed.Size()) + " stock streamed pictures known)");
        // Merging decal layers drops the decal materials it drew into
        // composites: what they held first is only pointed at from then on.
        OwnReferenceCopies();
        const std::vector<Struct*> late = MarkOrphans(gone, true);
        if (!late.empty())
            log("  " + std::to_string(late.size()) +
                " assets the merged decal materials held written where their next pointer is");
        for (size_t i = 0; i < std::min<size_t>(5, unreadable.size()); i++)
            Warn("picture " + unreadable[i] + " can't be read, so it's treated as missing");
        if (unreadable.size() > 5)
            Warn("... and " + std::to_string(unreadable.size() - 5) + " more pictures that can't be read");
        // The first few of a list, for a note.
        auto list = [](const std::vector<std::string>& v) { return Joined(v, 5, ", ..."); };
        if (!missingImages.empty())
        {
            const std::string where = gamePictures.empty() ? "the map's .iwd" : "the map's .iwd or the PC game's files";
            std::vector<std::string> magenta, flat;
            for (const auto& [n, stand] : missingImages) (stand ? flat : magenta).push_back(n);
            if (!magenta.empty())
                Warn(std::to_string(magenta.size()) + " picture" + (magenta.size() > 1 ? "s aren't" : " isn't") +
                     " in " + where + ", so magenta stands in (" + list(magenta) + ")");
            if (!flat.empty())
                Warn(std::to_string(flat.size()) + " normal/specular map" + (flat.size() > 1 ? "s aren't" : " isn't") +
                     " in " + where + ", so flat / no-shine ones stand in (" + list(flat) + ")");
        }
        if (const size_t n = magentaMaterials.size())
            Warn(std::to_string(n) + (n > 1 ? " materials lack" : " material lacks") +
                 " a picture the shader set used needs, so " + (n > 1 ? "they show" : "it shows") + " magenta (" +
                 list(magentaMaterials) + ")");
        if (const size_t n = invisibleMaterials.size())
            Warn(std::to_string(n) + (n > 1 ? " materials" : " material") + " with a tool or distance-fade shader set draw" +
                 (n > 1 ? "" : "s") + " nothing (" + list(invisibleMaterials) + ")");
        std::vector<std::pair<std::string, std::string>> swaps(techsetSwaps.begin(), techsetSwaps.end());
        std::sort(swaps.begin(), swaps.end());
        for (const auto& [a, b] : swaps)
        {
            if (IsToolsTechset(a))
            {
                if (Hides(a, b)) continue;
                Warn("shader set " + a + " isn't in the stock files given and no alpha tested one is either, so " + b +
                     " is used and its surfaces show");
                continue;
            }
            Warn("shader set " + a + " isn't in the stock files given, so " + b + " is used");
        }
        RenameClashes();
        if (boneBounds)
            log("  " + std::to_string(boneBounds) + " models' bone boxes rebuilt from their vertices (as stock 360 models have them)");
        if (groundLit)
            log("  " + std::to_string(groundLit) + " placed models with a ground colour given the ground-lit flag");
        root.platform = "xbox";
    }

    // ---- scripts ------------------------------------------------------------

    namespace
    {
        // common_mp's ambience tracks (all streamed from the 360's disc) and the
        // words that pick each, first match wins: one word, or words that must
        // all be there ("middle east").
        struct Ambience
        {
            const char* track;
            std::vector<const char*> keys;
        };
        const std::vector<Ambience>& Ambiences()
        {
            static const std::vector<Ambience> kAll = {
                { "ambient_mp_duststorm", { "duststorm", "sandstorm", "dust storm" } },
                { "ambient_mp_airport", { "airport", "terminal", "hangar" } },
                { "ambient_mp_estate", { "estate" } },
                { "embient_mp_highrise", { "highrise", "rooftop", "skyscraper" } },   // (sic: the stock alias's spelling)
                { "ambient_mp_favela", { "favela", "jungle", "tropical" } },
                { "ambient_mp_rain", { "rain", "thunder", "storm" } },
                { "ambient_mp_snow", { "snow", "winter", "arctic", "blizzard", "frozen", "frost" } },
                { "ambient_mp_desert", { "desert", "middle east", "mideast", "arab", "sand", "afghan", "quarry",
                                         "boneyard", "backlot", "bog", "broadcast", "citystreets", "district", "convoy",
                                         "ambush", "crash", "crossfire", "showdown", "strike", "toujane" } },
                { "ambient_mp_urban", { "urban", "city", "town", "street", "traffic", "industrial", "factory", "docks",
                                        "harbor", "harbour" } },
                { "ambient_mp_rural", { "rural", "farm", "forest", "country", "village", "field", "swamp", "creek",
                                        "russia", "birds", "crickets", "bloc", "pipeline", "vacant", "overgrown",
                                        "france", "carentan" } },
            };
            return kAll;
        }
        constexpr const char* kAmbienceDefault = "ambient_mp_rural";

        // Whether every word of `key` ("middle east") is among `words`.
        bool HasAll(const std::set<std::string>& words, std::string_view key)
        {
            for (size_t from = 0; from <= key.size();)
            {
                const size_t space = std::min(key.find(' ', from), key.size());
                if (!words.count(std::string(key.substr(from, space - from)))) return false;
                from = space + 1;
            }
            return true;
        }

        std::set<std::string> Words(const std::string& text)
        {
            std::set<std::string> out;
            std::string w;
            for (char c : Lower(text))
            {
                if (c >= 'a' && c <= 'z') w += c;
                else if (!w.empty())
                {
                    out.insert(w);
                    w.clear();
                }
            }
            if (!w.empty()) out.insert(w);
            return out;
        }
    }

    // A map script's ambience track (ambientPlay("name")) that no stock file
    // has becomes the closest track the 360 has loaded.
    void Porter::EditScript(Struct* d)
    {
        const std::string name = Name(d).value_or("");
        if (!name.ends_with(".gsc")) return;
        const std::string text = RawfileText(d);
        scriptWords = Words(name);
        static const std::regex ambient(R"re((\bambientPlay\s*\(\s*)"([^"]+)")re");
        std::string out;
        auto at = text.cbegin();
        for (std::sregex_iterator it(text.begin(), text.end(), ambient), end; it != end; ++it)
        {
            const std::smatch& m = *it;
            out.append(at, m[0].first);
            const std::string pick = AmbientTrack(m[2].str());
            out += pick.empty() ? m[0].str() : m[1].str() + "\"" + pick + "\"";
            at = m[0].second;
        }
        out.append(at, text.cend());
        if (out != text) SetRawfileText(d, out);
    }

    // The track to play in place of `name`, or "" to keep it.
    std::string Porter::AmbientTrack(const std::string& name)
    {
        if (stockAliases.Has(name)) return "";
        std::vector<std::string> have;
        for (const Ambience& a : Ambiences())
            if (stockAliases.Has(a.track)) have.push_back(a.track);
        if (have.empty()) return "";
        // The words of its name and of everything its alias names (sound files, ...).
        std::set<std::string> words = Words(name);
        for (Entry* e : root.assets)
            if (Struct* a = As<Struct>(e->asset); a && e->type == "sound" && AliasName(a) == name)
            {
                ForEach(a, [&](Node* n) {
                    if (Struct* o = As<Struct>(n); o && o->Ch())
                        for (auto& [k, c] : o->Ch()->items)
                            if (String* s = As<String>(c))
                                for (const std::string& w : Words(s->b)) words.insert(w);
                });
                break;
            }
        // The track's own words first (what the PC plays), then the map's name.
        std::string pick;
        for (const std::set<std::string>* ws : { &words, &scriptWords })
        {
            for (const Ambience& a : Ambiences())
                if (std::find(have.begin(), have.end(), a.track) != have.end() &&
                    std::any_of(a.keys.begin(), a.keys.end(), [&](std::string_view key) { return HasAll(*ws, key); }))
                {
                    pick = a.track;
                    break;
                }
            if (!pick.empty()) break;
        }
        if (pick.empty())
            pick = std::find(have.begin(), have.end(), kAmbienceDefault) != have.end() ? kAmbienceDefault : have[0];
        ambientSwaps.Set(name, pick);
        log("  ambience track " + name + " plays the 360's " + pick + " (it plays ambience only streamed from its disc)");
        return pick;
    }

    // ---- orphans: what only pointers hold any more --------------------------

    // Materials and pictures a left-out asset brought in first are only pointed
    // at from then on: the writer puts each where its first remaining pointer
    // is. Returns them (`newOnly`: those not marked already).
    std::vector<Struct*> Porter::MarkOrphans(const std::unordered_set<const Node*>& gone, bool newOnly)
    {
        const std::vector<Node*> all = Collect(root.assets);
        const std::unordered_set<const Node*> reached(all.begin(), all.end());
        Ordered<const Node*, Struct*> orphans;
        for (Node* n : all)
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            for (auto& [k, c] : o->Ch()->items)
                EachHeld(c, [&](Node* x) {
                    Ref* r = As<Ref>(x);
                    Slot* s = r ? As<Slot>(r->target) : nullptr;
                    Struct* a = s ? As<Struct>(s->asset) : nullptr;
                    if (a && !reached.count(a) && !gone.count(a)) orphans.FindOrAdd(a, a);
                });
        }
        std::unordered_set<const Node*> inner;
        for (auto& [k, a] : orphans)
            ForEach(a, [&](Node* n) {
                if (n != a) inner.insert(n);
            });
        std::vector<Struct*> found;
        for (auto& [k, a] : orphans)
        {
            if (inner.count(k) || (newOnly && a->forward)) continue;
            a->forward = true;
            found.push_back(a);
        }
        return found;
    }

    // A pointer to the slot of a picture that is only a name (",$white") gets
    // its own copy of that name, as stock materials repeat such names inline:
    // the 360 orders some structs' members otherwise than the PC, so the
    // pointer can come before the asset it points into.
    void Porter::OwnReferenceCopies()
    {
        // A pointer into another element's pointer array (effects share a mark
        // material): pointed at the material itself, or a copy of a name-only one.
        int shared = 0;
        auto point = [&](Node*& held) {
            Ref* c = As<Ref>(held);
            Node* v = c ? PtrArrayValue(c) : nullptr;
            if (!v) return;
            Struct* vs = As<Struct>(v);
            if (vs && vs->slot)
            {
                Ref* r = A().Make<Ref>(0);
                r->target = vs->slot;
                v = r;
            }
            else if (vs && OnlyNamed(Name(vs)))
            {
                Struct* copy = vs->Copy();
                copy->slot = nullptr;
                copy->forward = false;
                done.insert(copy);
                v = copy;
            }
            held = v;
            shared++;
        };
        for (Node* n : Collect(root.assets))
        {
            if (Struct* o = As<Struct>(n))
            {
                if (Children* ch = o->Ch())
                    for (auto& item : ch->items) point(item.second);
            }
            else if (PtrList* pl = As<PtrList>(n))
                for (Node*& item : pl->items) point(item);
        }
        if (shared) log("  " + std::to_string(shared) + " pointers into another element's material list pointed at the material");
        // Name-only assets of other kinds too once nothing holds them any more.
        const std::vector<Node*> objs = Collect(root.assets);
        const std::unordered_set<const Node*> held(objs.begin(), objs.end());
        std::map<std::string, int> types;
        int count = 0;
        for (Node* n : objs)
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            for (auto& [k, c] : o->Ch()->items)
            {
                Ref* r = As<Ref>(c);
                Slot* s = r && r->rel == 0 ? As<Slot>(r->target) : nullptr;
                Struct* a = s ? As<Struct>(s->asset) : nullptr;
                if (!a || !OnlyNamed(Name(a)) || !(a->IsAsset("GfxImage") || !held.count(a))) continue;
                Struct* copy = a->Copy();
                copy->slot = nullptr;
                copy->forward = false;
                if (Children* ach = a->Ch())
                {
                    Children* own = A().MakeChildren();
                    *own = *ach;
                    copy->SetCh(own);
                }
                // (its info, a material's, keeps sharing the name's: a copy of the
                // struct, not of what it holds)
                c = copy;
                if (o->type->kind == Type::Union && k.idx.empty()) Store(o->Data(), 4, kFollowing, o->big);
                else if (const zone::Member* m = o->Find(k.member); m && m->IsPointer())
                    o->SetPtr(k.member, kFollowing);
                done.insert(copy);
                count++;
                types[a->IsAsset() ? std::string(a->asset) : "other"]++;
            }
        }
        if (count)
        {
            std::string list;
            for (const auto& [t, n] : types) list += (list.empty() ? "" : ", ") + t + " " + std::to_string(n);
            log("  " + std::to_string(count) + " pointers to a name-only asset given their own copy of the name (" + list + ")");
        }
    }

    // The composite materials of merged decal layers point at the pictures of
    // the ground and decal materials; one first written further on would be
    // pointed at before it is written: it is written at the composite's
    // pointer instead, and the places that held it point back at it.
    void Porter::PicturesBeforeComposites(size_t start, size_t end)
    {
        std::vector<Entry*>& ents = root.assets;
        std::unordered_set<const Node*> before;
        for (Node* n : Collect(std::vector<Entry*>(ents.begin(), ents.begin() + ptrdiff_t(start)))) before.insert(n);
        Ordered<const Node*, Struct*> late;
        for (Node* n : Collect(std::vector<Entry*>(ents.begin() + ptrdiff_t(start), ents.begin() + ptrdiff_t(end))))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            for (auto& [k, c] : o->Ch()->items)
                EachHeld(c, [&](Node* x) {
                    Ref* r = As<Ref>(x);
                    Slot* s = r ? As<Slot>(r->target) : nullptr;
                    Struct* a = s ? As<Struct>(s->asset) : nullptr;
                    if (a && a->IsAsset("GfxImage") && !before.count(a)) late.FindOrAdd(a, a);
                });
        }
        if (late.Empty()) return;
        for (Node* n : Collect(std::vector<Entry*>(ents.begin() + ptrdiff_t(end), ents.end())))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            for (auto& [k, v] : o->Ch()->items)
            {
                Struct* vs = As<Struct>(v);
                if (!vs || !late.Has(vs) || !vs->slot) continue;
                Ref* r = A().Make<Ref>(1);    // (not null until the writer places the picture)
                r->target = vs->slot;
                v = r;
                if (o->type->kind == Type::Union)
                {
                    const uint32_t cur = uint32_t(Load(o->Data(), 4, o->big));
                    if (cur == kFollowing || cur == kInsert) Store(o->Data(), 4, 1, o->big);
                }
                else if (const zone::Member* m = o->Find(k.member); m && m->IsPointer())
                {
                    const uint32_t cur = o->Ptr(k.member);
                    if (cur == kFollowing || cur == kInsert) o->SetPtr(k.member, 1);
                }
            }
        }
        for (auto& [k, a] : late) a->forward = true;
        // Every pointer to such a picture points at the picture itself, not its
        // slot: the first one writes it there.
        ForEach(ents, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) return;
            for (auto& [k, c] : o->Ch()->items)
                EachHeld(c, [&](Node* x) {
                    Ref* r = As<Ref>(x);
                    Slot* s = r ? As<Slot>(r->target) : nullptr;
                    if (s && late.Has(s->asset))
                    {
                        r->target = s->asset;
                        r->rel = 0;
                        r->t = {};
                    }
                });
        });
        log("  decal layers: " + std::to_string(late.Size()) +
            " pictures written with the composite materials that use them (first written further on before)");
    }

    // ---- names --------------------------------------------------------------

    // The map's own materials and pictures named as one a stock effect brings
    // get a name of their own: the game keeps one asset per name.
    void Porter::RenameClashes()
    {
        std::unordered_set<const Node*> ours;
        std::set<std::pair<std::string, std::string>> names;
        for (Struct* fx : stockFxUsed)
            ForEach(fx, [&](Node* n) {
                ours.insert(n);
                Struct* o = As<Struct>(n);
                if (o && (o->IsAsset("Material") || o->IsAsset("GfxImage")))
                    if (auto nm = Name(o); nm && !nm->starts_with(",")) names.insert({ std::string(o->asset), *nm });
            });
        ShareEffectPictures(ours);
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || ours.count(o) || !(o->IsAsset("Material") || o->IsAsset("GfxImage"))) return;
            auto nm = Name(o);
            if (nm && names.count({ std::string(o->asset), *nm })) SetName(o, "~pc/" + *nm);
        });
    }

    // A picture of the map's with the name of one a stock effect brings, when
    // it comes from the PC game's files rather than the map's .iwd, is the
    // same picture: the map's materials point at the stock effect's copy.
    void Porter::ShareEffectPictures(const std::unordered_set<const Node*>& ours)
    {
        Ordered<std::string, Struct*> stock;
        for (Struct* fx : stockFxUsed)
            ForEach(fx, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (o && o->IsAsset("GfxImage"))
                    if (auto nm = Name(o).value_or(","); !nm.starts_with(",")) stock.FindOrAdd(nm, o);
            });
        if (stock.Empty()) return;
        std::unordered_map<const Node*, Struct*> swap;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || ours.count(o) || !o->IsAsset("GfxImage")) return;
            const std::string nm = Name(o).value_or("");
            Struct** s = stock.Find(nm);
            if (s && !mapPictures.count(Lower(nm)))
            {
                swap[o] = *s;
                if (o->slot) swap[o->slot] = *s;
            }
        });
        if (swap.empty()) return;
        for (auto& [k, v] : swap)
        {
            // Stock effects hold their pictures inline: written with a slot of
            // its own at its first pointer, which the map's pointers then name.
            if (!v->slot) v->slot = A().Make<Slot>(v);
            v->forward = true;
        }
        int moved = 0;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || ours.count(o) || !o->Ch()) return;
            for (auto& [k, c] : o->Ch()->items)
            {
                Struct* target = nullptr;
                if (auto it = swap.find(c); it != swap.end() && As<Struct>(c)) target = it->second;
                else if (Ref* r = As<Ref>(c); r && r->rel == 0)
                    if (auto it2 = swap.find(r->target); it2 != swap.end()) target = it2->second;
                if (!target) continue;
                Ref* r = A().Make<Ref>(1);
                r->target = target->slot;
                c = r;
                if (o->type->kind == Type::Union)
                {
                    const uint32_t cur = uint32_t(Load(o->Data(), 4, o->big));
                    if (cur == kFollowing || cur == kInsert) Store(o->Data(), 4, 1, o->big);
                }
                else if (const zone::Member* m = o->Find(k.member); m && m->IsPointer())
                {
                    const uint32_t cur = o->Ptr(k.member);
                    if (cur == kFollowing || cur == kInsert) o->SetPtr(k.member, 1);
                }
                moved++;
            }
        });
        std::erase_if(root.assets, [&](Entry* e) { return As<Struct>(e->asset) && swap.count(e->asset); });
        std::unordered_set<const Struct*> distinct;
        for (auto& [k, v] : swap) distinct.insert(v);
        log("  " + std::to_string(distinct.size()) + " pictures the map shares with stock effects it uses taken from them (" +
            std::to_string(moved) + " pointers), not kept twice");
    }

    // A full asset under a name common_mp has (always loaded) becomes a
    // reference to the loaded one, as stock maps name them. One that other
    // assets point into stays, as the parts would be written nowhere.
    void Porter::NameCommonAssets()
    {
        // A raw file common_mp has goes: the game loads raw files by name.
        std::vector<std::string> dropped;
        std::erase_if(root.assets, [&](Entry* e) {
            Struct* a = As<Struct>(e->asset);
            if (e->type != "rawfile" || !a || !commonNames.count({ "RawFile", Lower(Name(a).value_or("")) })) return false;
            dropped.push_back(*Name(a));
            return true;
        });
        if (!dropped.empty())
        {
            log("  " + std::to_string(dropped.size()) + " raw files common_mp has left out (" + Joined(dropped, 4) + ")");
        }
        static const char* const kNamed[] = { "GfxImage", "Material", "XModel", "FxEffectDef", "MaterialTechniqueSet", "PhysPreset" };
        std::vector<Struct*> found;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || done.count(o) || !o->IsAsset()) return;
            if (std::none_of(std::begin(kNamed), std::end(kNamed), [&](const char* t) { return o->asset == t; })) return;
            auto nm = AssetName(o);
            if (!nm || nm->starts_with(",")) return;
            if (commonNames.count({ std::string(o->asset), Lower(*nm) })) found.push_back(o);
        });
        if (found.empty()) return;
        std::unordered_map<const Node*, const Node*> inside;
        for (Struct* o : found)
        {
            ForEach(o, [&](Node* x) {
                if (x != o) inside[x] = o;
            });
            if (o->slot) inside.erase(o->slot);
        }
        std::unordered_set<const Node*> held(found.begin(), found.end());
        std::unordered_set<const Node*> pointedInto;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || inside.count(o) || held.count(o) || !o->Ch()) return;
            for (auto& [k, c] : o->Ch()->items)
            {
                auto check = [&](Node* x) {
                    Ref* r = As<Ref>(x);
                    if (!r) return;
                    Node* t = r->target;
                    if (Slot* s = As<Slot>(t)) t = s->asset;
                    auto it = inside.find(t);
                    if (it != inside.end() && (r->rel || !As<Struct>(t) || !As<Struct>(t)->IsAsset())) pointedInto.insert(it->second);
                };
                if (As<Array>(c) || As<PtrList>(c)) EachHeld(c, check);
                else check(c);
            }
        });
        std::map<std::string, int> byType;
        std::vector<std::string> kept;
        for (Struct* o : found)
        {
            if (pointedInto.count(o))
            {
                kept.push_back(*AssetName(o));
                continue;
            }
            byType[std::string(o->asset)]++;
            Replace(o, *Reference(std::string(o->asset), *AssetName(o)));
        }
        if (!byType.empty())
        {
            int total = 0;
            std::string list;
            for (const auto& [t, c] : byType)
            {
                total += c;
                list += (list.empty() ? "" : ", ") + t + " " + std::to_string(c);
            }
            log("  " + std::to_string(total) + " assets common_mp has named instead of copied (" + list + ")");
        }
        if (!kept.empty())
        {
            log("  note: " + std::to_string(kept.size()) + " kept as copies (other assets point inside them: " + Joined(kept, 5) +
                ")");
        }
    }

    // Effect materials named as one of common_mp's with an "_eye<n>" suffix
    // and the same render state and colour picture are drawn with common_mp's.
    void Porter::EffectSisterMaterials()
    {
        Ordered<std::string, Struct*> common;
        for (auto& [k, v] : commonMaterials)
            if (k.type == "Material") common.FindOrAdd(Lower(k.name), v);
        std::set<std::string> commonTs;
        for (const auto& [t, n] : commonNames)
            if (t == "MaterialTechniqueSet") commonTs.insert(n);
        auto colorImage = [&](Struct* m) -> std::optional<std::string> {
            Array* tt = As<Array>(Deref(m->Child("textureTable")));
            if (!tt) return std::nullopt;
            for (Node* n : tt->items)
            {
                Struct* td = As<Struct>(n);
                if (!td || td->U("nameHash") != kColorMap) continue;
                Struct* u = Embedded(td, "u");
                Struct* im = u ? DerefStruct(u->Child("image")) : nullptr;
                if (!im) return std::nullopt;
                std::string nm = Name(im).value_or("");
                nm = WithoutCommas(std::move(nm));
                return Lower(nm);
            }
            return std::nullopt;
        };
        std::vector<Entry*> fx;
        for (Entry* e : root.assets)
            if (e->type == "fx") fx.push_back(e);
        std::unordered_map<const Node*, std::string> swap;
        static const std::regex eye("_eye\\d+$");
        for (Node* n : Reach(fx))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->IsAsset("Material") || swap.count(o)) continue;
            const std::string name = AssetName(o).value_or("");
            const std::string sister = std::regex_replace(name, eye, "");
            if (name.starts_with(",") || sister == name || !common.Has(Lower(sister))) continue;
            Struct* ts = DerefStruct(o->Child("techniqueSet"));
            const std::string tsn = ts ? Name(ts).value_or("") : "";
            if (tsn.empty() || tsn.starts_with(",") || commonTs.count(Lower(tsn))) continue;
            Struct* s = *common.Find(Lower(sister));
            const auto se = s->Bytes("stateBitsEntry"), oe = o->Bytes("stateBitsEntry");
            if (!std::equal(se.begin(), se.end(), oe.begin(), oe.end()) || colorImage(s) != colorImage(o)) continue;
            swap[o] = sister;
        }
        if (swap.empty()) return;
        int count = 0;
        for (Node* n : Reach(fx))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) continue;
            for (auto& [k, v] : o->Ch()->items)
            {
                Struct* m = As<Ref>(v) ? DerefStruct(v) : As<Struct>(v);
                auto it = m ? swap.find(m) : swap.end();
                if (it == swap.end()) continue;
                v = Reference("Material", it->second);
                if (const zone::Member* mem = o->Find(k.member); mem && mem->IsPointer())
                    o->SetPtr(k.member, kFollowing);
                count++;
            }
        }
        std::set<std::string> names;
        for (auto& [k, v] : swap) names.insert(v);
        std::string list;
        for (const std::string& nm : names) list += (list.empty() ? "" : ", ") + nm;
        log("  " + std::to_string(count) + " effect material pointers drawn with common_mp's own material (" + list + ")");
    }
}
