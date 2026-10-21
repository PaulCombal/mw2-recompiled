// Decal layers merged into the ground under them, as the 360 map compiler
// does.
//
// The PC draws a blend decal (a dirt patch, a rust streak) as its own
// see-through surface laid over the ground's triangles. The 360 tools give
// those ground triangles a composite material instead (stock mp_rust: 130 of
// them, "*42n_34n") whose shader set draws the ground and up to two decal
// layers in one pass, blended per vertex.
//
// What a composite is (worked out from stock mp_rust against PC mp_rust):
//  - shader set: the ground's without "wc_" and a trailing "p0", then one part
//    per layer, the decal's feature codes with its layer number
//    (wc_l_sm_b0c0n0s0 -> _b1c1n1s1; wc_unlit_multiply_lin -> _m1c1), then
//    "p0" if the ground or a layer had it;
//  - pictures and constants: the ground's, then each layer's, renamed with its
//    number (colorMap1; name hash x = x * 33 ^ lower-case c);
//  - draw order, flags, culling: the ground's; render state: any stock
//    material with the same shader set; surface types: all of theirs;
//  - each vertex: its colour bytes 2 and 3 hold layer 1's and layer 2's weight
//    (the decal vertex's alpha); the world's layer data holds, per vertex,
//    every layer's texture coordinates, then for each normal-mapped layer 4
//    signed bytes: the layer's texture directions in the ground's.
#include "porter.h"

#include <algorithm>
#include <charconv>
#include <compare>
#include <cstdlib>
#include <cmath>
#include <map>
#include <regex>

namespace mapport
{
    namespace
    {
        constexpr size_t kMaxLayers = 2;

        uint32_t NameHash(const std::string& s)
        {
            uint32_t x = 0;
            for (char c : Lower(s)) x = (x * 33) ^ uint8_t(c);
            return x;
        }

        const std::map<uint32_t, std::string>& TexturesByHash()
        {
            static const std::map<uint32_t, std::string> kByHash = [] {
                std::map<uint32_t, std::string> m;
                for (const char* n : { "colorMap", "normalMap", "specularMap", "detailMap" }) m[NameHash(n)] = n;
                return m;
            }();
            return kByHash;
        }

        // Bytes of layer data a vertex has for a surface's material (0 for a plain one).
        size_t LayerBytes(Struct* s)
        {
            Struct* m = DerefStruct(s->Child("material"));
            Struct* ts = m ? DerefStruct(m->Child("techniqueSet")) : nullptr;
            std::string name = ts ? AssetName(ts).value_or("") : "";
            name = WithoutCommas(std::move(name));
            if (!m || AssetName(m).value_or("").rfind('*', 0) != 0) return 0;
            static const std::regex layer("_([bmt][123][a-z0-9]*?)(?=_|p0|$)");
            size_t bytes = 0;
            for (std::sregex_iterator it(name.begin(), name.end(), layer), end; it != end; ++it)
            {
                bytes += 8;
                if ((*it)[1].str().find('n', 2) != std::string::npos) bytes += 4;
            }
            return bytes;
        }

        // A position rounded to a tenth: its exact value rounded to one
        // decimal (ties to even), read back as the nearest double (-0.0 is
        // 0.0). Through decimal text, which to_chars and from_chars round
        // exactly on every platform.
        double Tenth(double x)
        {
            char b[400];   // (a double's largest fixed-point text)
            const auto [end, error] = std::to_chars(b, b + sizeof b, x, std::chars_format::fixed, 1);
            double v = x;
            if (error == std::errc()) std::from_chars(b, end, v);
            return v == 0 ? 0.0 : v;
        }

        // The world's surfaces and buffers.
        struct Geometry
        {
            Struct* dpvs;
            Struct* draw;
            Array* surfs;
            Leaf* V;
            Leaf* I;
            Leaf* LD;
            bool Found() const { return surfs && V && I && LD; }
        };
        Geometry GeometryOf(Struct* world)
        {
            Geometry g;
            g.dpvs = world->Sub("dpvs");
            g.draw = world->Sub("draw");
            g.surfs = As<Array>(Target(g.dpvs->Child("surfaces")));
            g.V = As<Leaf>(Target(g.draw->Sub("vd")->Child("vertices")));
            g.I = As<Leaf>(Target(g.draw->Child("indices")));
            g.LD = As<Leaf>(Target(g.draw->Sub("vld")->Child("data")));
            return g;
        }

        std::vector<uint32_t> Indexes(const Leaf* I)
        {
            std::vector<uint32_t> idx(I->raw.size() / 2);
            for (size_t i = 0; i < idx.size(); i++) idx[i] = uint32_t(Load(I->raw.data() + 2 * i, 2, I->big));
            return idx;
        }
        // Whether the first `n` surfaces, and the indices, vertices and layer
        // data they name, are within the world's buffers: a broken file is
        // left unmerged rather than read past its end.
        bool SurfacesFit(const Geometry& g, size_t n)
        {
            if (n > g.surfs->items.size()) return false;
            const std::vector<uint32_t> idx = Indexes(g.I);
            const int64_t verts = int64_t(g.V->raw.size() / kVertexBytes), layers = int64_t(g.LD->raw.size());
            for (size_t i = 0; i < n; i++)
            {
                Struct* s = As<Struct>(g.surfs->items[i]);
                Struct* t = s && s->Has("tris") ? s->Sub("tris") : nullptr;
                if (!t) return false;
                const int64_t first = t->Int("firstVertex"), count = t->Int("vertexCount");
                const int64_t base = t->Int("baseIndex"), tris = t->Int("triCount"), layer = t->Int("vertexLayerData");
                if (first < 0 || count < 0 || first + count > verts || base < 0 || tris < 0 ||
                    base + 3 * tris > int64_t(idx.size()) || layer < 0 || layer > layers)
                    return false;
                for (int64_t k = base; k < base + 3 * tris; k++)
                    if (first + idx[size_t(k)] >= verts) return false;
            }
            return true;
        }

        struct PairHash
        {
            size_t operator()(const std::pair<size_t, size_t>& p) const { return p.first * 1000003u ^ p.second; }
        };

        // The key neighbouring surfaces are drawn as one by: material and lighting.
        std::pair<const Node*, std::vector<uint8_t>> RunKey(Struct* s)
        {
            Struct* laf = s->Sub("laf");
            return { Deref(s->Child("material")), std::vector<uint8_t>(laf->Data(), laf->Data() + laf->Size()) };
        }
    }

    std::optional<TechsetCode> GroundCode(const std::string& ts)
    {
        static const std::regex re("^wc_(l_sm_(?:[a-z]0)+?)(p0)?$");
        std::smatch m;
        if (!std::regex_match(ts, m, re)) return std::nullopt;
        return TechsetCode{ m[1].str(), m[2].matched };
    }

    std::optional<TechsetCode> LayerCode(const std::string& ts, size_t i)
    {
        const std::string n = std::to_string(i);
        if (ts == "wc_unlit_multiply_lin") return TechsetCode{ "m" + n + "c" + n, false };
        static const std::regex re("^wc_l_sm_([bt]0(?:[a-z]0)*?)(p0)?$");
        std::smatch m;
        if (!std::regex_match(ts, m, re)) return std::nullopt;
        std::string code = m[1].str();
        for (char& c : code)
            if (c == '0') c = n[0];
        return TechsetCode{ code, m[2].matched };
    }

    class DecalMerger
    {
    public:
        DecalMerger(Porter& porter, Struct* world) : porter(porter), world(world) {}
        void Run();

    private:
        using Key = std::vector<const Node*>;   // the ground material, then the layers'
        struct Plan
        {
            std::string techset;
            Struct* tpl;
        };

        // Only materials written before the world (pointed at): a composite
        // points at their pictures, which must be in the file already.
        Struct* Mat(Struct* s) const
        {
            Node* c = s->Child("material");
            return As<Ref>(c) ? DerefStruct(c) : nullptr;
        }
        std::string TechsetName(Struct* m) const
        {
            Struct* ts = DerefStruct(m->Child("techniqueSet"));
            std::string n = ts ? AssetName(ts).value_or("") : "";
            n = WithoutCommas(std::move(n));
            return n;
        }
        static int64_t SortKey(Struct* m) { return m->Sub("info")->Int("sortKey"); }
        static std::vector<Struct*> Textures(Struct* m)
        {
            std::vector<Struct*> out;
            if (Array* tt = As<Array>(Target(m->Child("textureTable"))))
                for (Node* n : tt->items)
                    if (Struct* e = As<Struct>(n)) out.push_back(e);
            return out;
        }
        struct Constant
        {
            std::string name;
            std::vector<uint8_t> literal;
        };
        static std::pair<std::vector<Constant>, Leaf*> Constants(Struct* m)
        {
            Leaf* ct = As<Leaf>(Target(m->Child("constantTable")));
            if (!ct || ct->raw.empty()) return { {}, nullptr };
            const size_t size = ct->t->size;
            std::vector<Constant> out;
            for (size_t i = 0; i < ct->raw.size() / size; i++)
            {
                const uint8_t* e = ct->raw.data() + i * size;
                std::string name(reinterpret_cast<const char*>(e + 4), 12);
                while (!name.empty() && name.back() == '\0') name.pop_back();
                out.push_back({ name, std::vector<uint8_t>(e + 16, e + 32) });
            }
            return { out, ct };
        }
        static Leaf* TemplateState(Struct* tpl)
        {
            Node* state = tpl->Child("stateBitsTable");
            if (Ref* r = As<Ref>(state)) state = r->rel == 0 ? r->target : nullptr;
            return As<Leaf>(state);
        }

        const std::optional<Plan>& PlanFor(Struct* ground, const std::vector<Struct*>& layers);
        Entry* Listed(const std::string& type, Struct* asset);
        static Ref* PointerTo(Entry* entry);
        static Node* ImagePointer(Node* c);
        Struct* EntryCopy(Struct* e, int number = 0);
        Ref* TechsetRef(const std::string& name);
        Entry* Composite(Struct* ground, const std::vector<Struct*>& layers);
        void Rebuild(const Geometry& g, size_t n, const std::map<size_t, std::vector<Node*>>& split,
                     const std::set<size_t>& merged);

        Porter& porter;
        Struct* world;
        std::map<Key, std::optional<Plan>> plans;
        std::map<Key, Entry*> composites;
        std::map<std::string, Entry*> techsets;
        int serial = 0;
    };

    // The shader set and stock template for a ground material with these
    // layers, or nothing when the 360 has no such composite.
    const std::optional<DecalMerger::Plan>& DecalMerger::PlanFor(Struct* ground, const std::vector<Struct*>& layers)
    {
        Key key{ ground };
        key.insert(key.end(), layers.begin(), layers.end());
        if (auto it = plans.find(key); it != plans.end()) return it->second;
        std::optional<Plan> res;
        const std::optional<TechsetCode> g = GroundCode(TechsetName(ground));
        if (g && !layers.empty() && layers.size() <= kMaxLayers)
        {
            std::string name = g->code;
            bool p0 = g->p0, ok = true;
            for (size_t i = 1; i <= layers.size(); i++)
            {
                const std::optional<TechsetCode> lc = LayerCode(TechsetName(layers[i - 1]), i);
                // Every picture and constant needs a name the composite can number.
                bool named = lc.has_value();
                for (Struct* e : Textures(layers[i - 1])) named &= TexturesByHash().count(uint32_t(e->U("nameHash"))) != 0;
                for (const Constant& c : Constants(layers[i - 1]).first) named &= c.name.size() < 12;
                if (!named)
                {
                    ok = false;
                    break;
                }
                name += "_" + lc->code;
                p0 = p0 || lc->p0;
            }
            if (ok)
            {
                if (p0) name += "p0";
                Struct* const* tpl = porter.HaveTechset(name) ? porter.materialTemplates.Find(name) : nullptr;
                if (tpl && TemplateState(*tpl)) res = Plan{ name, *tpl };
            }
        }
        return plans[key] = res;
    }

    // An asset as an entry of its own in the file's asset list (before the
    // world): the world's surfaces are written before what their pointers lead
    // to, so pointing at an asset written there, later ones would point ahead.
    Entry* DecalMerger::Listed(const std::string& type, Struct* asset)
    {
        Entry* e = A().Make<Entry>(type, asset);
        porter.extraAssets.push_back(e);
        return e;
    }

    // A pointer to a listed asset through its list entry, as stock files point
    // from one asset at another.
    Ref* DecalMerger::PointerTo(Entry* entry)
    {
        Ref* r = A().Make<Ref>(1);
        r->target = entry;
        r->rel = 4;
        return r;
    }

    // A pointer to the picture a texture entry points at (written before the world).
    Node* DecalMerger::ImagePointer(Node* c)
    {
        if (Ref* r = As<Ref>(c))
        {
            Ref* copy = A().Make<Ref>(r->val);
            copy->target = r->target;
            copy->rel = r->rel;
            copy->t = r->t;
            return copy;
        }
        if (Struct* s = As<Struct>(c))
        {
            Ref* r = A().Make<Ref>(1);   // (written out as this until the target is placed: not null)
            r->target = s->slot ? static_cast<Node*>(s->slot) : s;
            return r;
        }
        return c;
    }

    Struct* DecalMerger::EntryCopy(Struct* e, int number)
    {
        Struct* made = e->Copy();
        if (Children* ch = e->Ch())
        {
            Children* own = A().MakeChildren();
            *own = *ch;
            made->SetCh(own);
        }
        Struct* u = made->Sub("u");
        Children* uch = A().MakeChildren();
        if (Children* theirs = Embedded(e, "u")->Ch())
            for (const auto& [k, v] : theirs->items) uch->items.push_back({ k, ImagePointer(v) });
        u->SetCh(uch);
        const uint32_t held = uint32_t(Load(u->Data(), 4, u->big));
        if (held == kFollowing || held == kInsert) Store(u->Data(), 4, 1, u->big);
        if (number)
        {
            const std::string digit = std::to_string(number);
            made->Set("nameHash", NameHash(TexturesByHash().at(uint32_t(e->U("nameHash"))) + digit));
            made->Set("nameEnd", digit[0]);
            // As the stock composites have them (the ground's): specular 1, others 2.
            Struct* ss = made->Sub("samplerState");
            if (ss->Int("mipMap")) ss->Set("mipMap", e->Int("semantic") == 8 ? 1 : 2);
        }
        return made;
    }

    Ref* DecalMerger::TechsetRef(const std::string& name)
    {
        auto it = techsets.find(name);
        if (it == techsets.end())
        {
            const AssetKey key{ "MaterialTechniqueSet", name };
            Struct* d = porter.resident.Has(key) || porter.named.count({ key.type, name }) ? porter.Reference(key.type, name)
                                                                                 : porter.StockCopy(*porter.library.Find(key));
            d->asset = porter.X.infos.at(key.type)->name;
            d->slot = nullptr;
            it = techsets.emplace(name, Listed("techset", d)).first;
        }
        return PointerTo(it->second);
    }

    Entry* DecalMerger::Composite(Struct* ground, const std::vector<Struct*>& layers)
    {
        Key key{ ground };
        key.insert(key.end(), layers.begin(), layers.end());
        if (auto it = composites.find(key); it != composites.end()) return it->second;
        const Plan plan = *PlanFor(ground, layers);
        std::vector<Node*> table;
        for (Struct* e : Textures(ground)) table.push_back(EntryCopy(e));
        for (size_t i = 1; i <= layers.size(); i++)
            for (Struct* e : Textures(layers[i - 1])) table.push_back(EntryCopy(e, int(i)));
        std::stable_sort(table.begin(), table.end(),
                         [](Node* a, Node* b) { return static_cast<Struct*>(a)->U("nameHash") < static_cast<Struct*>(b)->U("nameHash"); });
        auto [consts, ct] = Constants(ground);
        // (In name hash order, as the game looks them up.)
        struct NamedConstant
        {
            uint32_t hash;
            std::string name;
            std::vector<uint8_t> literal;
            auto operator<=>(const NamedConstant&) const = default;
        };
        std::vector<NamedConstant> entries;
        for (const Constant& c : consts) entries.push_back({ NameHash(c.name), c.name, c.literal });
        for (size_t i = 1; i <= layers.size(); i++)
        {
            auto [lc, lct] = Constants(layers[i - 1]);
            if (!ct) ct = lct;
            for (const Constant& c : lc)
            {
                const std::string n = c.name + std::to_string(i);
                entries.push_back({ NameHash(n), n, c.literal });
            }
        }
        std::sort(entries.begin(), entries.end());
        Leaf* constants = nullptr;
        if (!entries.empty())
        {
            std::vector<uint8_t> raw;
            for (const auto& [h, n, lit] : entries)
            {
                uint8_t b[4];
                Store(b, 4, h, ct->big);
                raw.insert(raw.end(), b, b + 4);
                std::string padded = n;
                padded.resize(12, '\0');
                raw.insert(raw.end(), padded.begin(), padded.end());
                raw.insert(raw.end(), lit.begin(), lit.end());
            }
            constants = A().Make<Leaf>(ct->t, int64_t(entries.size()), std::move(raw), ct->big);
        }
        uint64_t surfaceTypes = 0;
        surfaceTypes |= ground->Sub("info")->U("surfaceTypeBits");
        for (Struct* m : layers) surfaceTypes |= m->Sub("info")->U("surfaceTypeBits");
        const std::string name = "*dl" + std::to_string(++serial);
        Struct* d = ground->Copy();
        d->slot = nullptr;
        d->forward = false;
        Struct* info = d->Sub("info");
        Children* ich = A().MakeChildren();
        ich->Set("name", A().Make<String>(name));
        info->SetCh(ich);
        info->SetPtr("name", kFollowing);
        info->Set("surfaceTypeBits", int64_t(surfaceTypes));
        for (const char* k : { "stateBitsEntry", "stateBitsCount" })
        {
            const auto from = plan.tpl->Bytes(k);
            std::copy(from.begin(), from.end(), d->Bytes(k).begin());
        }
        d->Set("textureCount", int64_t(table.size()));
        d->Set("constantCount", int64_t(entries.size()));
        d->SetPtr("techniqueSet", 1);
        d->SetPtr("textureTable", kFollowing);
        d->SetPtr("constantTable", constants ? kFollowing : 0);
        d->SetPtr("stateBitsTable", kFollowing);
        Children* ch = A().MakeChildren();
        ch->Set("techniqueSet", TechsetRef(plan.techset));
        Array* tt = A().Make<Array>();
        tt->items = std::move(table);
        ch->Set("textureTable", tt);
        ch->Set("stateBitsTable", TemplateState(plan.tpl));
        if (constants) ch->Set("constantTable", constants);
        d->SetCh(ch);
        d->asset = porter.X.infos.at("Material")->name;
        return composites[key] = Listed("material", d);
    }

    void DecalMerger::Run()
    {
        const Geometry g = GeometryOf(world);
        if (!g.Found())
        {
            porter.Warn("decal layers couldn't be merged (world data not found)");
            return;
        }
        if (g.draw->Int("vertexLayerDataSize") > 4) return;   // already has layer data (a 360 file)
        const size_t n = size_t(g.dpvs->Int("staticSurfaceCount"));
        if (!SurfacesFit(g, n))
        {
            porter.Warn("decal layers couldn't be merged (the world's surfaces point past its data)");
            return;
        }
        Leaf* V = g.V;
        const std::vector<uint8_t> vraw = V->raw;
        const bool big = V->big;
        std::vector<uint32_t> idx = Indexes(g.I);
        auto surf = [&](size_t i) { return static_cast<Struct*>(g.surfs->items[i]); };
        using Tri = std::array<uint32_t, 3>;
        auto tris = [&](Struct* s) {
            Struct* t = s->Sub("tris");
            const uint32_t fv = uint32_t(t->Int("firstVertex"));
            const size_t b = size_t(t->Int("baseIndex"));
            std::vector<Tri> out;
            for (int64_t k = 0; k < t->Int("triCount"); k++)
                out.push_back({ fv + idx[b + 3 * size_t(k)], fv + idx[b + 3 * size_t(k) + 1], fv + idx[b + 3 * size_t(k) + 2] });
            return out;
        };
        using Pos = std::array<double, 3>;
        std::unordered_map<uint32_t, Pos> posCache;
        auto pos = [&](uint32_t v) -> const Pos& {
            auto [it, added] = posCache.emplace(v, Pos{});
            if (added)
                for (int k = 0; k < 3; k++) it->second[size_t(k)] = Tenth(double(LoadF32(vraw.data() + kVertexBytes * v + 4 * size_t(k), big)));
            return it->second;
        };
        auto uv = [&](uint32_t v) {
            return std::array<double, 2>{ double(LoadF32(vraw.data() + kVertexBytes * v + 20, big)),
                                          double(LoadF32(vraw.data() + kVertexBytes * v + 24, big)) };
        };
        // A triangle by the set of its corners' positions.
        auto shape = [&](const Tri& t) {
            std::vector<Pos> s{ pos(t[0]), pos(t[1]), pos(t[2]) };
            std::sort(s.begin(), s.end());
            s.erase(std::unique(s.begin(), s.end()), s.end());
            return s;
        };
        std::vector<Struct*> mats(n);
        for (size_t i = 0; i < n; i++) mats[i] = Mat(surf(i));
        using Place = std::pair<size_t, size_t>;   // (surface, triangle)
        std::map<std::vector<Pos>, Place> groundTris;
        std::map<size_t, std::vector<Tri>> gtris;
        for (size_t i = 0; i < n; i++)
        {
            Struct* m = mats[i];
            if (!m || SortKey(m) >= 6 || !GroundCode(TechsetName(m))) continue;
            gtris[i] = tris(surf(i));
            for (size_t k = 0; k < gtris[i].size(); k++) groundTris.emplace(shape(gtris[i][k]), Place{ i, k });
        }
        // (ground surface, triangle) -> {decal surface: its triangle}, both in the order first met.
        Ordered<Place, Ordered<size_t, Tri>, PairHash> cover;
        std::set<size_t> decals, blocked;
        for (size_t i = 0; i < n; i++)
        {
            Struct* m = mats[i];
            if (!m || SortKey(m) < 6 || !LayerCode(TechsetName(m), 1)) continue;
            decals.insert(i);
            for (const Tri& t : tris(surf(i)))
            {
                auto found = groundTris.find(shape(t));
                if (found == groundTris.end() || cover.FindOrAdd(found->second).Has(i))
                {
                    blocked.insert(i);
                    break;
                }
                cover.FindOrAdd(found->second).Set(i, t);
            }
        }
        auto layersOf = [&](const Place& p) {
            std::vector<size_t> js;
            if (const auto* c = cover.Find(p))
                for (const auto& [j, t] : *c)
                    if (!blocked.count(j)) js.push_back(j);
            std::stable_sort(js.begin(), js.end(), [&](size_t a, size_t b) {
                return std::pair{ SortKey(mats[a]), a } < std::pair{ SortKey(mats[b]), b };
            });
            return js;
        };
        auto materialsOf = [&](const std::vector<size_t>& js) {
            std::vector<Struct*> out;
            for (size_t j : js) out.push_back(mats[j]);
            return out;
        };
        while (true)
        {
            std::set<size_t> fresh;
            for (const auto& [p, c] : cover)
            {
                const std::vector<size_t> js = layersOf(p);
                if (!js.empty() && !PlanFor(mats[p.first], materialsOf(js))) fresh.insert(js.begin(), js.end());
            }
            bool grew = false;
            for (size_t j : fresh) grew |= blocked.insert(j).second;
            if (!grew) break;
        }
        std::set<size_t> merged;
        for (size_t j : decals)
            if (!blocked.count(j)) merged.insert(j);
        if (merged.empty())
        {
            porter.log("  decal layers: none could be merged");
            return;
        }

        // New surfaces for every ground surface a merged decal lies on.
        std::vector<uint8_t> vout = vraw, lout;
        size_t nverts = vraw.size() / kVertexBytes;
        std::map<size_t, std::vector<Node*>> split;
        std::set<size_t> grounds;
        for (const auto& [p, c] : cover)
            if (!layersOf(p).empty()) grounds.insert(p.first);
        for (size_t gi : grounds)
        {
            Struct* s = surf(gi);
            std::vector<std::pair<std::vector<size_t>, std::vector<size_t>>> groups;   // layer surfaces -> triangles
            for (size_t k = 0; k < gtris[gi].size(); k++)
            {
                const std::vector<size_t> js = layersOf({ gi, k });
                auto it = std::find_if(groups.begin(), groups.end(), [&](const auto& e) { return e.first == js; });
                if (it == groups.end()) groups.push_back({ js, { k } });
                else it->second.push_back(k);
            }
            std::vector<Node*> out;
            for (const auto& [js, ks] : groups)
            {
                Struct* ns = s->Copy();
                if (Children* ch = s->Ch())
                {
                    Children* own = A().MakeChildren();
                    *own = *ch;
                    ns->SetCh(own);
                }
                Struct* t = ns->Sub("tris");
                t->Set("baseIndex", int64_t(idx.size()));
                t->Set("triCount", int64_t(ks.size()));
                if (js.empty())
                {
                    const uint32_t fv = uint32_t(t->Int("firstVertex"));
                    for (size_t k : ks)
                        for (uint32_t v : gtris[gi][k]) idx.push_back(v - fv);
                    out.push_back(ns);
                    continue;
                }
                const std::vector<Struct*> lms = materialsOf(js);
                std::vector<size_t> normal;
                for (size_t i = 0; i < lms.size(); i++)
                    if (LayerCode(TechsetName(lms[i]), i + 1)->code.find('n', 2) != std::string::npos) normal.push_back(i);
                std::map<std::pair<uint32_t, std::vector<uint32_t>>, size_t> verts;   // (ground vertex, decal vertices) -> number
                std::map<size_t, std::vector<std::array<double, 4>>> rot;
                std::vector<std::array<size_t, 3>> triNew;
                for (size_t k : ks)
                {
                    const Tri& gt = gtris[gi][k];
                    std::vector<Tri> dts;
                    for (size_t j : js) dts.push_back(*cover.Find({ gi, k })->Find(j));
                    auto decalVertex = [&](const Tri& dt, uint32_t gv) {
                        for (uint32_t dv : dt)
                            if (pos(dv) == pos(gv)) return dv;
                        throw PortError("decal merge: a decal corner not on the ground's");
                    };
                    std::array<size_t, 3> corner{};
                    for (int c = 0; c < 3; c++)
                    {
                        std::vector<uint32_t> dvs;
                        for (const Tri& dt : dts) dvs.push_back(decalVertex(dt, gt[size_t(c)]));
                        corner[size_t(c)] = verts.emplace(std::pair{ gt[size_t(c)], dvs }, verts.size()).first->second;
                    }
                    triNew.push_back(corner);
                    if (normal.empty()) continue;
                    const auto g0 = uv(gt[0]), g1 = uv(gt[1]), g2 = uv(gt[2]);
                    const double d1[2] = { g1[0] - g0[0], g1[1] - g0[1] }, d2[2] = { g2[0] - g0[0], g2[1] - g0[1] };
                    const double det = d1[0] * d2[1] - d2[0] * d1[1];
                    for (size_t q = 0; q < normal.size(); q++)
                    {
                        const Tri& dt = dts[normal[q]];
                        const auto l0 = uv(decalVertex(dt, gt[0])), l1 = uv(decalVertex(dt, gt[1])), l2 = uv(decalVertex(dt, gt[2]));
                        const double e1[2] = { l1[0] - l0[0], l1[1] - l0[1] }, e2[2] = { l2[0] - l0[0], l2[1] - l0[1] };
                        if (std::abs(det) < 1e-12) continue;
                        // A = dL * inverse(dB), dB's columns the ground's uv edges.
                        const double a00 = (e1[0] * d2[1] - e2[0] * d1[1]) / det;
                        const double a01 = (e2[0] * d1[0] - e1[0] * d2[0]) / det;
                        const double a10 = (e1[1] * d2[1] - e2[1] * d1[1]) / det;
                        const double a11 = (e2[1] * d1[0] - e1[1] * d2[0]) / det;
                        for (size_t c : corner)
                        {
                            auto& acc = rot.try_emplace(c, std::vector<std::array<double, 4>>(normal.size(), { 0, 0, 0, 0 })).first->second[q];
                            acc[0] += a00;
                            acc[1] += a01;
                            acc[2] += a10;
                            acc[3] += a11;
                        }
                    }
                }
                if (verts.size() > 0xFFFF) throw PortError("decal merge: vertex block too big");
                const size_t fv = nverts, layerAt = lout.size();
                std::vector<const std::pair<const std::pair<uint32_t, std::vector<uint32_t>>, size_t>*> byNumber(verts.size());
                for (const auto& v : verts) byNumber[v.second] = &v;
                for (const auto* v : byNumber)
                {
                    const auto& [gv, dvs] = v->first;
                    std::vector<uint8_t> rec(vraw.begin() + ptrdiff_t(kVertexBytes * gv), vraw.begin() + ptrdiff_t(kVertexBytes * (gv + 1)));
                    for (size_t li = 0; li < dvs.size(); li++) rec[16 + 2 + li] = vraw[kVertexBytes * dvs[li] + 16];   // the decal's alpha
                    vout.insert(vout.end(), rec.begin(), rec.end());
                }
                for (const auto* v : byNumber)
                {
                    const auto& [gv, dvs] = v->first;
                    const size_t num = v->second;
                    for (uint32_t dv : dvs)
                        lout.insert(lout.end(), vraw.begin() + ptrdiff_t(kVertexBytes * dv + 20), vraw.begin() + ptrdiff_t(kVertexBytes * dv + 28));
                    for (size_t q = 0; q < normal.size(); q++)
                    {
                        std::array<double, 4> a = rot.count(num) ? rot[num][q] : std::array<double, 4>{ 1, 0, 0, 1 };
                        double r0 = std::sqrt(a[0] * a[0] + a[1] * a[1]), r1 = std::sqrt(a[2] * a[2] + a[3] * a[3]);
                        if (r0 == 0) r0 = 1;
                        if (r1 == 0) r1 = 1;
                        if (a[0] == 0 && a[1] == 0 && a[2] == 0 && a[3] == 0)
                        {
                            a = { 1, 0, 0, 1 };
                            r0 = r1 = 1;
                        }
                        for (double x : { a[3] / r1, a[0] / r0, a[2] / r1, a[1] / r0 })
                            lout.push_back(uint8_t(std::max(0, std::min(254, int(std::nearbyint(std::clamp(x, -1.0, 1.0) * 127)) + 127))));
                    }
                }
                nverts += verts.size();
                for (const auto& c : triNew)
                    for (size_t v : c) idx.push_back(uint32_t(v));
                t->Set("firstVertex", int64_t(fv));
                t->Set("vertexCount", int64_t(verts.size()));
                t->Set("vertexLayerData", int64_t(layerAt));
                ns->SetPtr("material", 1);
                ns->SetChild("material", PointerTo(Composite(mats[gi], lms)));
                out.push_back(ns);
            }
            split[gi] = std::move(out);
        }
        V->raw = std::move(vout);
        V->n = int64_t(nverts);
        g.I->raw = Pack16(idx, g.I->big);
        g.I->n = int64_t(idx.size());
        Rebuild(g, n, split, merged);
        if (lout.empty()) lout = g.LD->raw;
        g.LD->raw = std::move(lout);
        g.LD->n = int64_t(g.LD->raw.size());
        g.draw->Set("vertexCount", int64_t(nverts));
        g.draw->Set("indexCount", int64_t(idx.size()));
        g.draw->Set("vertexLayerDataSize", g.LD->n);
        porter.log("  decal layers: " + std::to_string(merged.size()) + " decal surfaces merged into the ground under them (" +
              std::to_string(composites.size()) + " composite materials, " + std::to_string(split.size()) +
              " ground surfaces split; " + std::to_string(decals.size() - merged.size()) + " decals left as they were)");
    }

    // Every surface table and surface number after the surfaces changed:
    // merged decals go, split ground surfaces become their parts.
    void DecalMerger::Rebuild(const Geometry& g, size_t n, const std::map<size_t, std::vector<Node*>>& split,
                              const std::set<size_t>& merged)
    {
        std::vector<Node*> fresh;
        std::vector<std::vector<uint32_t>> expand;
        for (size_t i = 0; i < n; i++)
        {
            std::vector<Node*> parts;
            if (merged.count(i)) {}
            else if (auto it = split.find(i); it != split.end()) parts = it->second;
            else parts = { g.surfs->items[i] };
            std::vector<uint32_t> e;
            for (size_t k = 0; k < parts.size(); k++) e.push_back(uint32_t(fresh.size() + k));
            expand.push_back(std::move(e));
            fresh.insert(fresh.end(), parts.begin(), parts.end());
        }
        const size_t n2 = fresh.size();
        std::vector<size_t> src;   // new surface -> the old one it came from
        for (size_t i = 0; i < n; i++) src.insert(src.end(), expand[i].size(), i);
        g.surfs->items.erase(g.surfs->items.begin(), g.surfs->items.begin() + ptrdiff_t(n));
        g.surfs->items.insert(g.surfs->items.begin(), fresh.begin(), fresh.end());
        Struct* dpvs = g.dpvs;
        // Bounds: kept, or worked out for the parts (middle, half size).
        if (Leaf* bl = As<Leaf>(Target(dpvs->Child("surfacesBounds"))))
        {
            const size_t size = bl->t->size;
            std::vector<uint8_t> out;
            for (size_t j = 0; j < src.size(); j++)
            {
                const size_t i = src[j];
                std::vector<uint8_t> old(bl->raw.begin() + ptrdiff_t(i * size), bl->raw.begin() + ptrdiff_t((i + 1) * size));
                if (split.count(i))
                {
                    Struct* t = static_cast<Struct*>(fresh[j])->Sub("tris");
                    double lo[3] = { 1e30, 1e30, 1e30 }, hi[3] = { -1e30, -1e30, -1e30 };
                    const size_t base = size_t(t->Int("baseIndex")), fv = size_t(t->Int("firstVertex"));
                    std::set<uint32_t> used;
                    for (int64_t k = 0; k < 3 * t->Int("triCount"); k++)
                        used.insert(uint32_t(Load(g.I->raw.data() + 2 * (base + size_t(k)), 2, g.I->big)));
                    for (uint32_t v : used)
                        for (int q = 0; q < 3; q++)
                        {
                            const double x = double(LoadF32(g.V->raw.data() + kVertexBytes * (fv + v) + 4 * size_t(q), g.V->big));
                            lo[q] = std::min(lo[q], x);
                            hi[q] = std::max(hi[q], x);
                        }
                    std::fill(old.begin(), old.end(), uint8_t(0));
                    for (int q = 0; q < 3; q++)
                    {
                        StoreF32(old.data() + 4 * q, float((lo[q] + hi[q]) / 2), bl->big);
                        StoreF32(old.data() + 12 + 4 * q, float((hi[q] - lo[q]) / 2), bl->big);
                    }
                }
                out.insert(out.end(), old.begin(), old.end());
            }
            out.insert(out.end(), bl->raw.begin() + ptrdiff_t(n * size), bl->raw.end());
            bl->n = int64_t(n2) + std::max<int64_t>(0, bl->n - int64_t(n));
            bl->raw = std::move(out);
        }
        if (Leaf* sm = As<Leaf>(Target(dpvs->Child("surfaceMaterials"))))
        {
            const size_t size = sm->t->size;
            std::vector<uint8_t> raw(size * n2);
            raw.insert(raw.end(), sm->raw.begin() + ptrdiff_t(std::min(sm->raw.size(), n * size)), sm->raw.end());
            sm->n = int64_t(n2) + std::max<int64_t>(0, sm->n - int64_t(n));
            sm->raw = std::move(raw);
        }
        const size_t words = ((n2 + 31) / 32 + 3) / 4 * 4;
        if (Children* ch = dpvs->Ch())
            for (auto& [k, c] : ch->items)
                if (k.member == "surfaceVisData")
                    if (Leaf* lf = As<Leaf>(Target(c)))
                    {
                        lf->raw.assign(n2, 0);
                        lf->n = int64_t(n2);
                    }
        dpvs->Set("surfaceVisDataCount", int64_t(words));
        if (Leaf* bits = As<Leaf>(Target(dpvs->Child("surfaceCastsSunShadow"))); bits && !bits->raw.empty())
        {
            const size_t count = bits->raw.size() / 4;
            std::vector<uint32_t> set(words);
            for (size_t j = 0; j < src.size(); j++)
            {
                const size_t i = src[j];
                if ((i >> 5) < count && (Load(bits->raw.data() + 4 * (i >> 5), 4, bits->big) >> (i & 31)) & 1)
                    set[j >> 5] |= 1u << (j & 31);
            }
            const size_t unit = bits->n ? bits->raw.size() / size_t(bits->n) : 4;
            bits->raw.assign(4 * words, 0);
            for (size_t w = 0; w < words; w++) Store(bits->raw.data() + 4 * w, 4, set[w], bits->big);
            bits->n = int64_t(bits->raw.size() / unit);
        }
        auto renumbered = [&](uint32_t i) {
            return i < n ? expand[i] : std::vector<uint32_t>{ uint32_t(i - n + n2) };
        };
        // The sorted list (culling tree nodes hold ranges of it) and the lists holding numbers.
        if (Leaf* so = As<Leaf>(Target(dpvs->Child("sortedSurfIndex"))))
        {
            std::vector<uint32_t> old(size_t(so->n)), sorted;
            for (size_t i = 0; i < old.size(); i++) old[i] = uint32_t(Load(so->raw.data() + 2 * i, 2, so->big));
            std::vector<size_t> at{ 0 };
            for (uint32_t i : old)
            {
                const auto e = renumbered(i);
                sorted.insert(sorted.end(), e.begin(), e.end());
                at.push_back(sorted.size());
            }
            so->raw = Pack16(sorted, so->big);
            so->n = int64_t(sorted.size());
            if (Array* trees = As<Array>(Target(world->Child("aabbTrees"))))
                for (Node* ctn : trees->items)
                    if (Array* nodes = As<Array>(Target(static_cast<Struct*>(ctn)->Child("aabbTree"))))
                        for (Node* ndn : nodes->items)
                        {
                            Struct* nd = static_cast<Struct*>(ndn);
                            const size_t a = size_t(nd->Int("startSurfIndex")), c = size_t(nd->Int("surfaceCount"));
                            if (a + c > old.size()) continue;
                            nd->Set("startSurfIndex", int64_t(at[a]));
                            nd->Set("surfaceCount", int64_t(at[a + c] - at[a]));
                        }
        }
        if (Array* geoms = As<Array>(Target(world->Child("shadowGeom"))))
            for (Node* gn : geoms->items)
            {
                Struct* gm = As<Struct>(gn);
                Leaf* lf = gm ? As<Leaf>(Target(gm->Child("sortedSurfIndex"))) : nullptr;
                if (!lf || !gm->Int("surfaceCount")) continue;
                std::vector<uint32_t> list;
                for (int64_t i = 0; i < gm->Int("surfaceCount"); i++)
                {
                    const auto e = renumbered(uint32_t(Load(lf->raw.data() + 2 * size_t(i), 2, lf->big)));
                    list.insert(list.end(), e.begin(), e.end());
                }
                lf->raw = Pack16(list, lf->big);
                lf->n = int64_t(list.size());
                gm->Set("surfaceCount", int64_t(list.size()));
            }
        if (Leaf* models = As<Leaf>(Target(world->Child("models"))))
        {
            const size_t size = models->t->size;
            size_t off = 0;
            for (const zone::Member& m : models->t->members)
                if (m.name == "surfaceCount") off = m.offset;
            for (int64_t k = 0; k < models->n; k++)
            {
                uint8_t* p = models->raw.data() + size_t(k) * size + off;
                uint32_t c = uint32_t(Load(p, 2, models->big)), a = uint32_t(Load(p + 2, 2, models->big));
                if (a == 0 && c == n) c = uint32_t(n2);
                else if (a != 0xFFFF && c && a < n && !expand[a].empty()) a = expand[a][0];
                Store(p, 2, c, models->big);
                Store(p + 2, 2, a, models->big);
            }
        }
        if (Array* skies = As<Array>(Target(world->Child("skies"))))
            for (Node* sn : skies->items)
            {
                Struct* sky = As<Struct>(sn);
                Leaf* lf = sky ? As<Leaf>(Target(sky->Child("skyStartSurfs"))) : nullptr;
                if (!lf) continue;
                for (int64_t i = 0; i < lf->n; i++)
                {
                    uint8_t* p = lf->raw.data() + 4 * size_t(i);
                    const int32_t x = int32_t(uint32_t(Load(p, 4, lf->big)));
                    if (x >= 0 && size_t(x) < n && !expand[size_t(x)].empty()) Store(p, 4, expand[size_t(x)][0], lf->big);
                }
            }
        dpvs->Set("staticSurfaceCount", int64_t(n2));
        const int64_t count = world->Int("surfaceCount") ? world->Int("surfaceCount") : int64_t(n);
        world->Set("surfaceCount", count - int64_t(n) + int64_t(n2));
    }

    void Porter::MergeDecals(Struct* world) { DecalMerger(*this, world).Run(); }

    // The 360 draws a run of neighbouring surfaces with the same material (and
    // lighting) as one draw: from the first one's first index to the last
    // one's last, in the first one's vertex block. Stock files lay the index
    // buffer out in surface order and keep such runs in one block; after
    // merging decals that no longer held (a GPU hang). So runs in more than
    // one block get one block together, then the whole index buffer is
    // written again in surface order.
    void Porter::RelaySurfaces(Struct* world)
    {
        const Geometry g = GeometryOf(world);
        if (!g.Found()) return;
        const size_t n = size_t(g.dpvs->Int("staticSurfaceCount"));
        if (!SurfacesFit(g, n)) return;
        const std::vector<uint32_t> idx = Indexes(g.I);
        const std::vector<uint8_t> vraw = g.V->raw, lraw = g.LD->raw;
        std::vector<uint8_t> vout = vraw, lout = lraw;
        size_t nverts = vraw.size() / kVertexBytes;
        std::unordered_map<const Node*, uint32_t> rel;   // surface -> index offset (its block moved into a run's)
        int joined = 0;
        auto surf = [&](size_t i) { return static_cast<Struct*>(g.surfs->items[i]); };
        using Block = std::array<int64_t, 3>;
        auto blockOf = [](Struct* s) {
            Struct* t = s->Sub("tris");
            return Block{ t->Int("firstVertex"), t->Int("vertexCount"), t->Int("vertexLayerData") };
        };
        for (size_t i = 0; i < n;)
        {
            size_t j = i;
            while (j + 1 < n && RunKey(surf(j + 1)) == RunKey(surf(i))) j++;
            std::vector<Block> blocks;
            for (size_t k = i; k <= j; k++)
                if (std::find(blocks.begin(), blocks.end(), blockOf(surf(k))) == blocks.end()) blocks.push_back(blockOf(surf(k)));
            if (blocks.size() > 1)
            {
                int64_t total = 0;
                for (const Block& b : blocks) total += b[1];
                if (total > 0xFFFF)
                    Warn(std::to_string(j - i + 1) +
                         " neighbouring surfaces with one material need more than 65,535 vertices together (decal layers)");
                else
                {
                    const size_t bpv = LayerBytes(surf(i));
                    const size_t at = nverts, layerAt = lout.size();
                    std::map<Block, uint32_t> off;
                    for (const Block& b : blocks)
                    {
                        off[b] = uint32_t(nverts - at);
                        vout.insert(vout.end(), vraw.begin() + ptrdiff_t(kVertexBytes * size_t(b[0])),
                                    vraw.begin() + ptrdiff_t(kVertexBytes * size_t(b[0] + b[1])));
                        if (bpv)
                            lout.insert(lout.end(), lraw.begin() + ptrdiff_t(b[2]),
                                        lraw.begin() + ptrdiff_t(std::min(lraw.size(), size_t(b[2]) + bpv * size_t(b[1]))));
                        nverts += size_t(b[1]);
                    }
                    for (size_t k = i; k <= j; k++)
                    {
                        Struct* t = surf(k)->Sub("tris");
                        rel[surf(k)] = off.at(blockOf(surf(k)));
                        t->Set("firstVertex", int64_t(at));
                        t->Set("vertexCount", total);
                        t->Set("vertexLayerData", bpv ? int64_t(layerAt) : 0);
                    }
                    joined++;
                }
            }
            i = j + 1;
        }
        std::vector<uint32_t> out;
        for (Node* sn : g.surfs->items)
        {
            Struct* t = static_cast<Struct*>(sn)->Sub("tris");
            const size_t b = size_t(t->Int("baseIndex"));
            const auto k = rel.find(sn);
            const uint32_t add = k == rel.end() ? 0 : k->second;
            t->Set("baseIndex", int64_t(out.size()));
            for (size_t x = b; x < std::min(idx.size(), b + 3 * size_t(t->Int("triCount"))); x++) out.push_back(idx[x] + add);
        }
        g.V->raw = std::move(vout);
        g.V->n = int64_t(nverts);
        g.I->raw = Pack16(out, g.I->big);
        g.I->n = int64_t(out.size());
        if (lout.size() > g.LD->raw.size())
        {
            g.LD->raw = std::move(lout);
            g.LD->n = int64_t(g.LD->raw.size());
        }
        g.draw->Set("vertexCount", int64_t(nverts));
        g.draw->Set("indexCount", int64_t(out.size()));
        g.draw->Set("vertexLayerDataSize", int64_t(g.LD->raw.size()));
        log("  decal layers: index buffer laid out in surface order, " + std::to_string(joined) +
            " runs of one material put in one vertex block");
    }

    // The world's vertex buffer (and layer data) made again from just the
    // vertices surfaces use, block by block: merging decals and joining runs
    // leave the old copies behind. Each block keeps the order of what it
    // holds; indexes are renumbered to match.
    void Porter::CompactVertices(Struct* world)
    {
        const Geometry g = GeometryOf(world);
        if (!g.Found()) return;
        std::vector<uint32_t> idx = Indexes(g.I);
        struct Block
        {
            std::vector<Struct*> surfs;
            std::set<uint32_t> used;
            std::set<size_t> bpv;
        };
        std::map<std::array<int64_t, 3>, Block> blocks;   // (first vertex, count, layer data)
        std::vector<Struct*> empty;
        for (Node* sn : g.surfs->items)
        {
            Struct* s = As<Struct>(sn);
            if (!s) continue;
            Struct* t = s->Sub("tris");
            if (!t->Int("triCount"))
            {
                empty.push_back(t);   // (draws nothing: pointed at no vertices)
                continue;
            }
            Block& b = blocks[{ t->Int("firstVertex"), t->Int("vertexCount"), t->Int("vertexLayerData") }];
            b.surfs.push_back(s);
            const size_t base = size_t(t->Int("baseIndex"));
            for (size_t x = base; x < std::min(idx.size(), base + 3 * size_t(t->Int("triCount"))); x++) b.used.insert(idx[x]);
            b.bpv.insert(LayerBytes(s));
        }
        for (const auto& [k, b] : blocks)
            if (b.bpv.size() > 1)
            {
                Warn("decal layers: a vertex block shared by surfaces with different layer data; vertices left as they were");
                return;
            }
        const std::vector<uint8_t>& vraw = g.V->raw;
        const std::vector<uint8_t>& lraw = g.LD->raw;
        std::vector<uint8_t> vout, lout(lraw.begin(), lraw.begin() + ptrdiff_t(std::min<size_t>(4, lraw.size())));
        const size_t before = vraw.size() / kVertexBytes;
        for (auto& [key, b] : blocks)
        {
            const auto [fv, cnt, lay] = key;
            const size_t bpv = *b.bpv.begin();
            std::unordered_map<uint32_t, uint32_t> newOf;
            uint32_t next = 0;
            for (uint32_t old : b.used) newOf[old] = next++;
            const size_t first = vout.size() / kVertexBytes, layerAt = lout.size();
            for (uint32_t k : b.used)
            {
                const size_t at = kVertexBytes * (size_t(fv) + k);
                vout.insert(vout.end(), vraw.begin() + ptrdiff_t(std::min(at, vraw.size())),
                            vraw.begin() + ptrdiff_t(std::min(at + kVertexBytes, vraw.size())));
                if (bpv)
                {
                    const size_t from = size_t(lay) + bpv * k;
                    lout.insert(lout.end(), lraw.begin() + ptrdiff_t(std::min(from, lraw.size())),
                                lraw.begin() + ptrdiff_t(std::min(from + bpv, lraw.size())));
                }
            }
            for (Struct* s : b.surfs)
            {
                Struct* t = s->Sub("tris");
                const size_t base = size_t(t->Int("baseIndex"));
                for (size_t x = base; x < std::min(idx.size(), base + 3 * size_t(t->Int("triCount"))); x++) idx[x] = newOf.at(idx[x]);
                t->Set("firstVertex", int64_t(first));
                t->Set("vertexCount", int64_t(b.used.size()));
                t->Set("vertexLayerData", bpv ? int64_t(layerAt) : 0);
            }
        }
        for (Struct* t : empty)
        {
            t->Set("firstVertex", 0);
            t->Set("vertexCount", 0);
            t->Set("vertexLayerData", 0);
        }
        g.V->raw = std::move(vout);
        g.V->n = int64_t(g.V->raw.size() / kVertexBytes);
        g.I->raw = Pack16(idx, g.I->big);
        g.LD->raw = std::move(lout);
        g.LD->n = int64_t(g.LD->raw.size());
        g.draw->Set("vertexCount", g.V->n);
        g.draw->Set("vertexLayerDataSize", int64_t(g.LD->raw.size()));
        log("  decal layers: vertex buffer holds just what surfaces use (" + std::to_string(g.V->n) + " vertices, was " +
            std::to_string(before) + ")");
    }
}
