// The map itself: what the 360 keeps otherwise in the world's drawn geometry
// (placed models, surfaces, culling trees, the light grid), its collision
// and entities, and the models and effects the map carries.
#include "porter.h"

#include <algorithm>
#include <cstdio>
#include <cmath>
#include <cstring>
#include <map>
#include <regex>

namespace mapport
{
    using zone::Member;
    using zone::Type;

    namespace
    {
        Leaf* LeafOf(Node* c) { return As<Leaf>(Target(c)); }
        Array* ArrayOf(Node* c) { return As<Array>(Target(c)); }

        // A number in plain data; one past its end is a broken file.
        template <class L>   // (a Leaf or a const one)
        auto At(L* lf, size_t at, size_t size) -> decltype(lf->raw.data())
        {
            if (at > lf->raw.size() || size > lf->raw.size() - at) throw PortError("the world's data is cut short");
            return lf->raw.data() + at;
        }
        uint32_t U16(const Leaf* lf, size_t at) { return uint32_t(Load(At(lf, at, 2), 2, lf->big)); }
        uint32_t U32(const Leaf* lf, size_t at) { return uint32_t(Load(At(lf, at, 4), 4, lf->big)); }
        double F32(const Leaf* lf, size_t at) { return double(LoadF32(At(lf, at, 4), lf->big)); }

        std::vector<uint32_t> Words16(const Leaf* lf, size_t count, size_t at = 0)
        {
            std::vector<uint32_t> v(count);
            for (size_t i = 0; i < count; i++) v[i] = U16(lf, at + 2 * i);
            return v;
        }

        int64_t FloorDiv(int64_t a, int64_t b)
        {
            const int64_t q = a / b;
            return (a % b != 0 && (a < 0) != (b < 0)) ? q - 1 : q;
        }

        // The floats of a Bounds struct (midPoint, halfSize) at `p`.
        void ReadBounds(const Struct* s, double mid[3], double half[3])
        {
            const uint8_t* p = s->Data();
            for (int k = 0; k < 3; k++)
            {
                mid[k] = double(LoadF32(p + 4 * k, s->big));
                half[k] = double(LoadF32(p + 12 + 4 * k, s->big));
            }
        }

        constexpr uint32_t kHimipRadius = 1238;      // the median of stock Favela's models
        constexpr int kProbeStockLevel = 70;
    }

    // ---- placed models --------------------------------------------------------

    // PC: dynEntVisData[2][3]; 360: six pointers dynEntVisData0_0 .. 1_2.
    bool Porter::PreGfxWorldDpvsDynamic(Struct* d, Struct* x)
    {
        for (int i = 0; i < 2; i++)
        {
            PtrList* pl = As<PtrList>(d->PopChild("dynEntVisData", { i }));
            for (int j = 0; j < 3; j++)
            {
                const std::string k = "dynEntVisData" + std::to_string(i) + "_" + std::to_string(j);
                x->SetPtr(k, d->Ptr("dynEntVisData", size_t(i * 3 + j)));
                if (pl && size_t(j) < pl->items.size() && pl->items[size_t(j)]) d->SetChild(x->M(k).name, pl->items[size_t(j)]);
            }
        }
        return false;
    }

    // PC: placement {origin, axis[3][3], scale}; 360: origin and packedAxis[4],
    // each axis three signed 10-bit numbers (x 511, rounded) and the scale as
    // a float (worked out from stock maps, whose collision holds the same
    // models with plain axes). Left zero, every model drew nowhere.
    bool Porter::PreGfxStaticModelDrawInst(Struct* d, Struct* x)
    {
        if (!modelSwap.empty())
        {
            Node* c = d->Child("model");
            Struct* m = As<Struct>(c) ? As<Struct>(c) : As<Ref>(c) ? DerefStruct(c) : nullptr;
            if (auto it = m ? modelSwap.find(m) : modelSwap.end(); it != modelSwap.end())
            {
                Ref* r = A().Make<Ref>(0);
                r->target = it->second;   // the stock model, through its asset list entry
                r->rel = 4;
                d->SetChild("model", r);
                d->SetPtr("model", 0);
            }
        }
        if (!d->Find("placement")) return false;
        Struct* pl = d->Sub("placement");
        for (int k = 0; k < 3; k++) x->SetF("origin", pl->F("origin", size_t(k)), size_t(k));
        for (int a = 0; a < 3; a++)
        {
            uint32_t v = 0;
            for (int k = 0; k < 3; k++)
            {
                const double c = std::clamp(pl->F("axis", size_t(a * 3 + k)), -1.0, 1.0);
                v |= (uint32_t(int64_t(std::floor(c * 511 + 0.5))) & 0x3FF) << (10 * k);
            }
            x->Set("packedAxis", v, size_t(a));
        }
        uint32_t scale;
        const float s = float(pl->F("scale"));
        std::memcpy(&scale, &s, 4);
        x->Set("packedAxis", scale, 3);
        // Placed-model flags (from PC mp_rust, mp_terminal and mp_afghan against
        // stock 360 ones): the PC's 0x20 (ground lighting) is the 360's 0x02, the
        // PC's 0x10 the 360's 0x01; its low bits have no 360 counterpart. The
        // ground colour is kept with its bytes the other way round.
        const int64_t f = d->Int("flags") & 0xFF;
        int64_t flags = ((f & 0x20) ? 0x02 : 0) | ((f & 0x10) ? 0x01 : 0);
        Struct* gl = Embedded(d, "groundLighting");
        if (gl)
        {
            const bool lit = std::any_of(gl->Data(), gl->Data() + gl->Size(), [](uint8_t b) { return b != 0; });
            if (!(flags & 0x02) && lit)
            {
                flags |= 0x02;
                groundLit++;
            }
            std::reverse(gl->Data(), gl->Data() + gl->Size());
        }
        d->Set("flags", flags);
        return false;
    }

    // The PC keeps a water surface's starting waves as one array of complex
    // numbers (H0); the 360 keeps the real and imaginary parts apart (H0X,
    // H0Y). The game reads them every frame.
    bool Porter::PreWaterT(Struct* d, Struct* x)
    {
        Leaf* h0 = LeafOf(d->PopChild("H0"));
        if (!h0) return false;
        const Type* f = P.defs.types.at("float");
        const size_t n = std::min(size_t(std::max<int64_t>(h0->n, 0)), h0->raw.size() / 8);
        for (int part = 0; part < 2; part++)
        {
            std::vector<uint8_t> raw(4 * n);
            for (size_t i = 0; i < n; i++) std::memcpy(raw.data() + 4 * i, h0->raw.data() + 8 * i + 4 * part, 4);
            const char* k = part ? "H0Y" : "H0X";
            d->SetChild(x->M(k).name, A().Make<Leaf>(f, int64_t(n), std::move(raw), h0->big));
            x->SetPtr(k, kFollowing);
        }
        return false;
    }

    // ---- the world's drawn geometry --------------------------------------------

    // childrenOffset is a byte distance to the node's first child: nodes are 44
    // bytes on the PC and 40 on the 360.
    void Porter::PostGfxAabbTree(Struct* d)
    {
        const Type* tp = P.defs.types.at("GfxAabbTree");
        d->Set("childrenOffset", FloorDiv(d->Int("childrenOffset"), tp->size) * d->type->size);
    }

    // The PC sorts surfaces twice (all, then without decals); the 360 keeps one list.
    void Porter::PostGfxWorldDpvsStatic(Struct* d)
    {
        Leaf* lf = As<Leaf>(d->Child("sortedSurfIndex"));
        const int64_t n = d->Int("staticSurfaceCount");
        if (lf && lf->n > n)
        {
            lf->raw.resize(size_t(n) * lf->t->size);
            lf->n = n;
        }
    }

    // IW4x's ZoneBuilder fills the lightmap override pointers with the sky and
    // $outdoor pictures; stock 360 maps leave them empty (set, the game would
    // light every surface with them).
    void Porter::PostGfxWorldDraw(Struct* d)
    {
        for (const char* k : { "lightmapOverridePrimary", "lightmapOverrideSecondary" })
        {
            Node* c = d->PopChild(k);
            d->SetPtr(k, 0);
            // The picture was written here first; later pointers to it get it instead.
            if (Struct* s = As<Struct>(c)) movedImages.push_back(s);
        }
    }

    void Porter::PostGfxWorld(Struct* d)
    {
        MergeDecals(d);
        OrderSurfaces(d);
        RelaySurfaces(d);
        CompactVertices(d);
        FillSurfaceBounds(d);
        GrowTreeBounds(d);
        FillLightingOrigins(d);
        SliceTreeLists(d);
        DimStandinProbes(d);
        WorldMaterialMemory(d);
        // One bit per room ("has sun-lit surfaces"), 32 to a word. Stock 360 maps
        // always carry it and the renderer reads it every frame; PC files can
        // have none, and the conversion crashed as the first frame drew.
        const int64_t cells = d->Sub("dpvsPlanes")->Int("cellCount");
        if (cells && !d->Child("cellHasSunLitSurfsBits"))
        {
            const Leaf* caster = LeafOf(d->Child("cellCasterBits"));
            const size_t n = size_t(cells + 31) / 32;
            d->SetChild(d->M("cellHasSunLitSurfsBits").name,
                        A().Make<Leaf>(caster ? caster->t : d->M("cellHasSunLitSurfsBits").type, int64_t(n),
                                       std::vector<uint8_t>(4 * n), true));
            d->SetPtr("cellHasSunLitSurfsBits", kFollowing);
            log("  world: room sun-light bits filled in (the PC file has none)");
        }
        if (Children* ch = d->Ch())
            for (auto& [k, c] : ch->items)
            {
                Node* t = As<Ref>(c) ? Deref(c) : nullptr;
                auto moved = std::find(movedImages.begin(), movedImages.end(), t);
                if (!t || moved == movedImages.end()) continue;
                c = t;
                SetHolder(d, k.member, kInsert);
                std::erase(movedImages, t);
            }
        if (!movedImages.empty()) throw PortError("a picture from the lightmap override is still needed elsewhere");
    }

    // With decal layers merged: the solid surfaces sorted so those with one
    // material (and lighting) are neighbours, which the 360 draws as one;
    // solid surfaces depth-test, so their order doesn't change the picture.
    // Decals and see-through ones keep their order. A run takes at most 65,535
    // vertices (16-bit indexes): bigger ones are cut in chunks, kept apart by
    // other materials' surfaces (a sort key with one material keeps its
    // surfaces as they were).
    std::vector<size_t> Porter::SolidRuns(const std::vector<Node*>& surfs, const std::vector<size_t>& order,
                                          const std::vector<int>& groups, const std::vector<int64_t>& sortKeys)
    {
        std::vector<size_t> solid, rest;
        for (size_t i : order) (groups[i] == 0 ? solid : rest).push_back(i);
        using Key = std::pair<const Node*, std::vector<uint8_t>>;
        auto key = [&](size_t i) -> Key {
            Struct* s = As<Struct>(surfs[i]);
            Struct* laf = Embedded(s, "laf");
            return { Deref(s->Child("material")),
                     laf ? std::vector<uint8_t>(laf->Data(), laf->Data() + laf->Size()) : std::vector<uint8_t>{} };
        };
        std::map<int64_t, std::vector<size_t>> bySort;
        for (size_t i : solid) bySort[sortKeys[i]].push_back(i);
        std::vector<size_t> out;
        for (auto& [sk, idx] : bySort)
        {
            std::vector<std::pair<Key, std::vector<size_t>>> keys;
            for (size_t i : idx)
            {
                Key k = key(i);
                auto it = std::find_if(keys.begin(), keys.end(), [&](const auto& e) { return e.first == k; });
                if (it == keys.end()) keys.push_back({ std::move(k), { i } });
                else it->second.push_back(i);
            }
            struct Chunk
            {
                size_t c, first;
                std::vector<size_t> members;
            };
            std::vector<Chunk> chunks;
            for (auto& [k, members] : keys)
            {
                const size_t first = size_t(std::find(idx.begin(), idx.end(), members[0]) - idx.begin());
                std::set<std::pair<int64_t, int64_t>> blocks;
                int64_t total = 0;
                std::vector<size_t> cur;
                size_t c = 0;
                for (size_t i : members)
                {
                    Struct* t = As<Struct>(surfs[i])->Sub("tris");
                    const std::pair<int64_t, int64_t> b{ t->Int("firstVertex"), t->Int("vertexCount") };
                    int64_t add = blocks.count(b) ? 0 : b.second;
                    if (!cur.empty() && total + add > 0xFFFF)
                    {
                        chunks.push_back({ c, first, cur });
                        blocks.clear();
                        total = 0;
                        cur.clear();
                        c++;
                        add = b.second;
                    }
                    blocks.insert(b);
                    total += add;
                    cur.push_back(i);
                }
                chunks.push_back({ c, first, cur });
            }
            if (keys.size() == 1 && chunks.size() > 1)
            {
                out.insert(out.end(), idx.begin(), idx.end());   // nothing to keep its chunks apart
                continue;
            }
            // Chunk 0 of every material (in the order they first appear), then
            // chunk 1 of those that have one, ...; a round doesn't start with the
            // material the one before ended with (it's rotated).
            std::map<size_t, std::vector<std::pair<size_t, std::vector<size_t>>>> rounds;
            for (const Chunk& ch : chunks) rounds[ch.c].push_back({ ch.first, ch.members });
            std::vector<std::vector<size_t>> flat;
            for (auto& [c, part] : rounds)
            {
                std::sort(part.begin(), part.end());
                std::vector<std::vector<size_t>> run;
                for (auto& p : part) run.push_back(p.second);
                if (!flat.empty() && key(run[0][0]) == key(flat.back()[0])) std::rotate(run.begin(), run.begin() + 1, run.end());
                flat.insert(flat.end(), run.begin(), run.end());
            }
            bool meet = false;
            for (size_t j = 0; j + 1 < flat.size(); j++) meet |= key(flat[j][0]) == key(flat[j + 1][0]);
            if (meet)
            {
                out.insert(out.end(), idx.begin(), idx.end());   // (one material left with chunks: as it was)
                continue;
            }
            for (const auto& members : flat) out.insert(out.end(), members.begin(), members.end());
        }
        out.insert(out.end(), rest.begin(), rest.end());
        return out;
    }

    // The static surfaces in the 360 tools' order (solid, sort key below 6;
    // then decals and see-through; then shadow casters; each group keeping its
    // order) and the ranges to match. Every per-surface array moves with them
    // and every surface number is renumbered. Culling tree nodes index
    // sortedSurfIndex, which keeps its order. Brush model surfaces stay.
    void Porter::OrderSurfaces(Struct* world)
    {
        Struct* dpvs = world->Sub("dpvs");
        Array* surfs = ArrayOf(dpvs->Child("surfaces"));
        const size_t n = size_t(dpvs->Int("staticSurfaceCount"));
        if (!surfs || !n || surfs->items.size() < n)
        {
            Warn("surfaces couldn't be put in 360 order (data not found)");
            return;
        }
        std::vector<int> groups(n);
        std::vector<int64_t> sortKeys(n);
        for (size_t i = 0; i < n; i++)
        {
            Struct* s = As<Struct>(surfs->items[i]);
            Struct* m = s ? DerefStruct(s->Child("material")) : nullptr;
            if (!m) continue;
            Struct* ts = DerefStruct(m->Child("techniqueSet"));
            if (ts && AssetName(ts).value_or("").find("shadowcaster") != std::string::npos)
            {
                groups[i] = 2;
                continue;
            }
            const int64_t key = m->Sub("info")->Int("sortKey");
            groups[i] = key < 6 ? 0 : 1;
            sortKeys[i] = key;
        }
        std::vector<size_t> order(n);
        for (size_t i = 0; i < n; i++) order[i] = i;
        std::stable_sort(order.begin(), order.end(), [&](size_t a, size_t b) { return groups[a] < groups[b]; });
        order = SolidRuns(surfs->items, order, groups, sortKeys);
        bool moved = false;
        for (size_t i = 0; i < n; i++) moved |= order[i] != i;
        if (moved)
        {
            std::vector<uint32_t> newOf(n);
            for (size_t k = 0; k < n; k++) newOf[order[k]] = uint32_t(k);
            std::vector<Node*> reordered(n);
            for (size_t k = 0; k < n; k++) reordered[k] = surfs->items[order[k]];
            std::copy(reordered.begin(), reordered.end(), surfs->items.begin());
            for (const char* key : { "surfacesBounds", "surfaceMaterials" })
            {
                Leaf* lf = LeafOf(dpvs->Child(key));
                if (!lf || size_t(lf->n) < n) continue;
                const size_t size = lf->t->size;
                std::vector<uint8_t> raw;
                for (size_t i : order) raw.insert(raw.end(), lf->raw.begin() + ptrdiff_t(i * size), lf->raw.begin() + ptrdiff_t((i + 1) * size));
                raw.insert(raw.end(), lf->raw.begin() + ptrdiff_t(n * size), lf->raw.end());
                lf->raw = std::move(raw);
            }
            if (Leaf* bits = LeafOf(dpvs->Child("surfaceCastsSunShadow")); bits && !bits->raw.empty())
            {
                const size_t count = bits->raw.size() / 4;
                std::vector<uint32_t> words(count), out(count);
                for (size_t w = 0; w < count; w++) words[w] = out[w] = U32(bits, 4 * w);
                for (size_t w = 0; w < std::min(count, (n + 31) / 32); w++)
                    out[w] &= ~((w + 1) * 32 <= n ? 0xFFFFFFFFu : (1u << (n - w * 32)) - 1);
                for (size_t old = 0; old < n; old++)
                    if ((old >> 5) < count && (newOf[old] >> 5) < count && (words[old >> 5] >> (old & 31)) & 1)
                        out[newOf[old] >> 5] |= 1u << (newOf[old] & 31);
                for (size_t w = 0; w < count; w++) Store(bits->raw.data() + 4 * w, 4, out[w], bits->big);
            }
            auto renumber = [&](Leaf* lf, std::optional<size_t> count) {
                if (!lf || !lf->n) return;
                const size_t k = count.value_or(size_t(lf->n));
                for (size_t i = 0; i < k; i++)
                {
                    const uint32_t v = U16(lf, 2 * i);
                    Store(lf->raw.data() + 2 * i, 2, v < n ? newOf[v] : v, lf->big);
                }
            };
            renumber(LeafOf(dpvs->Child("sortedSurfIndex")), std::nullopt);
            if (Array* geoms = ArrayOf(world->Child("shadowGeom")))
                for (Node* g : geoms->items)
                    if (Struct* gm = As<Struct>(g); gm && gm->Int("surfaceCount"))
                        renumber(LeafOf(gm->Child("sortedSurfIndex")), size_t(gm->Int("surfaceCount")));
        }
        const int64_t a = std::count(groups.begin(), groups.end(), 0);
        const int64_t b = a + std::count(groups.begin(), groups.end(), 1);
        const int64_t c = b + std::count(groups.begin(), groups.end(), 2);
        dpvs->Set("litOpaqueSurfsBegin", 0);
        dpvs->Set("litOpaqueSurfsEnd", a);
        dpvs->Set("litTransSurfsBegin", a);
        dpvs->Set("litTransSurfsEnd", b);
        dpvs->Set("shadowCasterSurfsBegin", b);
        dpvs->Set("shadowCasterSurfsEnd", c);
        dpvs->Set("emissiveSurfsBegin", c);
        dpvs->Set("emissiveSurfsEnd", c);
        log("  surfaces in 360 order: " + std::to_string(a) + " solid, " + std::to_string(b - a) +
            " decals / see-through, " + std::to_string(c - b) + " shadow casters" + (moved ? "" : " (already in order)"));
    }

    // GfxSurfaceBounds has two more words on the 360: the first is radius << 16
    // | colorMap density << 8 | colorMap1 density. Radius: from the bounds'
    // middle to the farthest vertex, rounded up. Density: 26 x picture width /
    // (world units per texture repeat), 0 without that picture (worked out from
    // stock mp_rust). Left 0, the game took every surface for a point.
    void Porter::FillSurfaceBounds(Struct* world)
    {
        Struct* draw = world->Sub("draw");
        Struct* dpvs = world->Sub("dpvs");
        auto leaf = [](Node* c) -> Leaf* {
            if (Ref* r = As<Ref>(c); r && r->rel == 0) c = r->target;
            return As<Leaf>(c);
        };
        Leaf* verts = leaf(draw->Sub("vd")->Child("vertices"));
        Leaf* idx = leaf(draw->Child("indices"));
        Array* surfs = ArrayOf(dpvs->Child("surfaces"));
        Leaf* bounds = leaf(dpvs->Child("surfacesBounds"));
        if (!verts || !idx || !surfs || !bounds)
        {
            Warn("the world's surfaces get no culling radius (data not found)");
            return;
        }
        const std::vector<uint32_t> I = Words16(idx, size_t(idx->n));
        const size_t size = bounds->t->size;
        std::map<std::pair<const Node*, uint32_t>, int64_t> widths;
        auto width = [&](Node* c, uint32_t hash) -> int64_t {
            Struct* mat = As<Ref>(c) ? DerefStruct(c) : As<Struct>(c);
            if (!mat) return 0;
            auto [it, added] = widths.emplace(std::pair{ mat, hash }, 0);
            if (!added) return it->second;
            int64_t w = 0;
            if (Array* tt = As<Array>(mat->Child("textureTable")))
                for (Node* n : tt->items)
                {
                    Struct* t = As<Struct>(n);
                    Struct* u = t ? Embedded(t, "u") : nullptr;
                    if (!t || t->U("nameHash") != hash || !u) continue;
                    if (Struct* img = DerefStruct(u->Child("image")))
                    {
                        w = img->Int("width");
                        for (int k = 0; k < 4; k++) w = std::max(w, img->Sub("streams", k)->Int("width"));
                        // A picture the game has loaded (",name"): its size isn't here.
                        if (w <= 1) w = 512;
                    }
                }
            return it->second = w;
        };
        for (size_t k = 0; k < std::min(surfs->items.size(), size_t(bounds->n)); k++)
        {
            Struct* s = As<Struct>(surfs->items[k]);
            if (!s) continue;
            Struct* t = s->Sub("tris");
            double mid[3];
            for (int q = 0; q < 3; q++) mid[q] = F32(bounds, k * size + 4 * size_t(q));
            const size_t base = size_t(t->Int("baseIndex")), fv = size_t(t->Int("firstVertex"));
            const size_t end = std::min(I.size(), base + 3 * size_t(t->Int("triCount")));
            double r2 = 0, worldArea = 0, uvArea = 0;
            for (size_t j = base; j + 2 < end; j += 3)
            {
                double p[3][3], uv[3][2];
                for (int v = 0; v < 3; v++)
                {
                    const size_t o = (fv + I[j + size_t(v)]) * kVertexBytes;
                    for (int q = 0; q < 3; q++) p[v][q] = F32(verts, o + 4 * size_t(q));
                    for (int q = 0; q < 2; q++) uv[v][q] = F32(verts, o + 20 + 4 * size_t(q));
                    double d2 = 0;
                    for (int q = 0; q < 3; q++) d2 += (p[v][q] - mid[q]) * (p[v][q] - mid[q]);
                    r2 = std::max(r2, d2);
                }
                double a[3], b[3];
                for (int q = 0; q < 3; q++)
                {
                    a[q] = p[1][q] - p[0][q];
                    b[q] = p[2][q] - p[0][q];
                }
                const double cr[3] = { a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0] };
                worldArea += std::sqrt(cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]);
                uvArea += std::abs((uv[1][0] - uv[0][0]) * (uv[2][1] - uv[0][1]) - (uv[1][1] - uv[0][1]) * (uv[2][0] - uv[0][0]));
            }
            const uint32_t radius = uint32_t(std::min(65535.0, std::ceil(std::sqrt(r2))));
            uint32_t dens[2];
            const uint32_t hashes[2] = { kColorMap, kColorMap1 };
            for (int h = 0; h < 2; h++)
            {
                const int64_t w = width(s->Child("material"), hashes[h]);
                if (!w || !worldArea || !uvArea || !std::isfinite(worldArea) || !std::isfinite(uvArea))
                {
                    dens[h] = 0;
                    continue;
                }
                const double v = std::nearbyint(26.0 * double(w) / std::sqrt(worldArea / uvArea));
                dens[h] = uint32_t(std::max(1.0, std::min(255.0, v)));
            }
            Store(At(bounds, k * size + 24, 4), 4, radius << 16 | dens[0] << 8 | dens[1], bounds->big);
        }
    }

    // Every culling tree node's box grown to enclose the static models it
    // lists. Stock 360 maps always have them inside; maps ported from CoD4
    // don't, and the 360 skips a node's models when its box is off screen: they
    // vanish as you turn. A bigger box only means drawing more.
    void Porter::GrowTreeBounds(Struct* world)
    {
        Leaf* insts = LeafOf(world->Sub("dpvs")->Child("smodelInsts"));
        Array* trees = ArrayOf(world->Child("aabbTrees"));
        if (!insts || !trees) return;
        const size_t count = insts->raw.size() / 36;
        int grown = 0;
        double far = 0;
        for (Node* ctn : trees->items)
        {
            Struct* ct = As<Struct>(ctn);
            Array* nodes = ct ? ArrayOf(ct->Child("aabbTree")) : nullptr;
            if (!nodes) continue;
            for (Node* ndn : nodes->items)
            {
                Struct* nd = As<Struct>(ndn);
                if (!nd) continue;
                const int64_t cnt = nd->Int("smodelIndexCount");
                Node* c = nd->Child("smodelIndexes");
                size_t off = 0;
                if (Ref* r = As<Ref>(c))
                {
                    off = size_t(r->rel);
                    c = r->target;
                }
                Leaf* lf = As<Leaf>(c);
                if (!cnt || !lf) continue;
                Struct* b = nd->Sub("bounds");
                double mid[3], half[3], lo[3], hi[3];
                ReadBounds(b, mid, half);
                for (int k = 0; k < 3; k++)
                {
                    lo[k] = mid[k] - half[k];
                    hi[k] = mid[k] + half[k];
                }
                bool out = false;
                for (int64_t j = 0; j < cnt; j++)
                {
                    const size_t i = U16(lf, off + 2 * size_t(j));
                    if (i >= count) continue;
                    double box[6];
                    for (int q = 0; q < 6; q++) box[q] = F32(insts, 36 * i + 4 * size_t(q));
                    for (int k = 0; k < 3; k++)
                        if (box[k] - box[3 + k] < lo[k] || box[k] + box[3 + k] > hi[k])
                        {
                            far = std::max({ far, lo[k] - (box[k] - box[3 + k]), (box[k] + box[3 + k]) - hi[k] });
                            lo[k] = std::min(lo[k], box[k] - box[3 + k]);
                            hi[k] = std::max(hi[k], box[k] + box[3 + k]);
                            out = true;
                        }
                }
                if (!out) continue;
                grown++;
                for (int k = 0; k < 3; k++)
                {
                    StoreF32(b->Data() + 4 * k, float((lo[k] + hi[k]) / 2), b->big);
                    StoreF32(b->Data() + 12 + 4 * k, float((hi[k] - lo[k]) / 2), b->big);
                }
            }
        }
        if (grown)
        {
            char line[160];
            std::snprintf(line, sizeof line, "  culling tree: %d node boxes grown to enclose their static models (up to %.0f units)",
                          grown, far);
            log(line);
        }
    }

    // GfxStaticModelInst is (box centre, box half-size, lighting origin). The
    // 360 samples the light grid at the lighting origin; custom-compiled maps
    // can leave it (0,0,0): those take the box centre.
    void Porter::FillLightingOrigins(Struct* world)
    {
        Leaf* insts = LeafOf(world->Sub("dpvs")->Child("smodelInsts"));
        if (!insts) return;
        const size_t count = insts->raw.size() / 36;
        int filled = 0;
        for (size_t i = 0; i < count; i++)
        {
            if (F32(insts, 36 * i + 24) != 0 || F32(insts, 36 * i + 28) != 0 || F32(insts, 36 * i + 32) != 0) continue;
            std::copy(insts->raw.begin() + ptrdiff_t(36 * i), insts->raw.begin() + ptrdiff_t(36 * i + 12),
                      insts->raw.begin() + ptrdiff_t(36 * i + 24));
            filled++;
        }
        if (filled)
            log("  static model lighting origins set to their box centres: " + std::to_string(filled) + " of " +
                std::to_string(count));
    }

    // Each room's culling tree keeps its nodes and the models each lists, laid
    // out as stock 360 maps have them: the root's list holds every model of
    // the room depth first (a node's own, then each child's in turn), and
    // every other node's list is the run of it that is its subtree. The 360
    // renumbers only the root lists after it reorders the models on load.
    void Porter::SliceTreeLists(Struct* world)
    {
        Array* trees = ArrayOf(world->Child("aabbTrees"));
        if (!trees) return;
        int rooms = 0, laid = 0;
        for (Node* ctn : trees->items)
        {
            Struct* ct = As<Struct>(ctn);
            Array* nodeArray = ct ? ArrayOf(ct->Child("aabbTree")) : nullptr;
            if (!nodeArray || nodeArray->items.empty()) continue;
            std::vector<Struct*> nodes;
            for (Node* n : nodeArray->items) nodes.push_back(As<Struct>(n));
            std::vector<std::vector<uint32_t>> lists;
            bool readable = true;
            for (Struct* nd : nodes)
            {
                Node* c = nd->Child("smodelIndexes");
                size_t off = 0;
                if (Ref* r = As<Ref>(c))
                {
                    off = size_t(r->rel);
                    c = r->target;
                }
                const size_t n = size_t(nd->Int("smodelIndexCount"));
                Leaf* lf = As<Leaf>(c);
                if (!n) lists.emplace_back();
                else if (!lf || off + 2 * n > lf->raw.size()) readable = false;
                else lists.push_back(Words16(lf, n, off));
            }
            if (!readable || lists[0].empty()) continue;
            std::vector<std::vector<size_t>> kids;
            for (size_t i = 0; i < nodes.size(); i++)
            {
                const int64_t f = int64_t(i) + FloorDiv(nodes[i]->Int("childrenOffset"), 40);
                std::vector<size_t> k;
                for (int64_t c = 0; c < nodes[i]->Int("childCount"); c++) k.push_back(size_t(f + c));
                kids.push_back(std::move(k));
            }
            std::map<size_t, std::set<uint32_t>> sub;
            std::function<const std::set<uint32_t>&(size_t, int)> subtree = [&](size_t i, int depth) -> const std::set<uint32_t>& {
                if (depth > 64) throw Error("tree too deep");
                if (i >= lists.size()) throw Error("a child past the tree");
                if (auto it = sub.find(i); it != sub.end()) return it->second;
                std::set<uint32_t> s(lists[i].begin(), lists[i].end());
                for (size_t k : kids[i])
                {
                    const std::set<uint32_t>& below = subtree(k, depth + 1);
                    s.insert(below.begin(), below.end());
                }
                return sub[i] = std::move(s);
            };
            try
            {
                subtree(0, 0);
            }
            catch (const Error&)
            {
                Warn("a culling tree's model lists couldn't be laid out (tree not as expected)");
                continue;
            }
            // Only when each node lists exactly its subtree and no model sits
            // under two children does every node get one run of the root list.
            bool ok = true;
            for (auto& [i, s] : sub)
            {
                ok &= std::set<uint32_t>(lists[i].begin(), lists[i].end()) == s;
                std::set<uint32_t> seen;
                for (size_t k : kids[i])
                {
                    for (uint32_t m : sub[k]) ok &= !seen.count(m);
                    seen.insert(sub[k].begin(), sub[k].end());
                }
            }
            if (!ok || lists[0].size() != sub[0].size())
            {
                Warn("a culling tree's model lists weren't laid out (a model listed twice, or a node not listing what is "
                     "below it)");
                continue;
            }
            std::vector<uint32_t> order;
            std::map<size_t, std::pair<size_t, size_t>> span;
            std::function<void(size_t)> lay = [&](size_t i) {
                const size_t start = order.size();
                std::set<uint32_t> below;
                for (size_t k : kids[i]) below.insert(sub[k].begin(), sub[k].end());
                std::vector<uint32_t> own;
                for (uint32_t m : lists[i])
                    if (!below.count(m)) own.push_back(m);
                std::sort(own.begin(), own.end());
                order.insert(order.end(), own.begin(), own.end());
                for (size_t k : kids[i]) lay(k);
                span[i] = { start, order.size() - start };
            };
            lay(0);
            const Leaf* base = LeafOf(nodes[0]->Child("smodelIndexes"));
            Leaf* rootList = A().Make<Leaf>(base->t, int64_t(order.size()), Pack16(order, base->big), base->big);
            for (auto& [i, sp] : span)
            {
                Struct* nd = nodes[i];
                const auto [start, n] = sp;
                nd->Set("smodelIndexCount", int64_t(n));
                if (i == 0)
                {
                    nd->SetChild("smodelIndexes", rootList);
                    nd->SetPtr("smodelIndexes", kFollowing);
                }
                else if (n)
                {
                    Ref* r = A().Make<Ref>(1);   // non-null until the writer knows where the root list lands
                    r->target = rootList;
                    r->rel = int64_t(2 * start);
                    r->t = { rootList->t, 0 };
                    nd->SetChild("smodelIndexes", r);
                    nd->SetPtr("smodelIndexes", 1);
                }
                else
                {
                    nd->PopChild("smodelIndexes");
                    nd->SetPtr("smodelIndexes", 0);
                }
            }
            rooms++;
            laid += int(span.size());
        }
        if (rooms)
            log("  culling tree model lists laid out inside each room's list (" + std::to_string(rooms) + " rooms, " +
                std::to_string(laid) + " nodes)");
    }

    // When every reflection probe is the same picture (the map was compiled
    // without probes), their colour is scaled to stock 360 probes' average level.
    void Porter::DimStandinProbes(Struct* world)
    {
        PtrList* probes = As<PtrList>(Target(world->Sub("draw")->Child("reflectionProbes")));
        if (!probes) return;
        std::vector<Leaf*> leaves;
        for (Node* c : probes->items)
        {
            Node* im = Target(c);
            if (Slot* s = As<Slot>(im)) im = s->asset;
            Struct* ims = As<Struct>(im);
            Leaf* px = ims ? LeafOf(ims->Child("pixels")) : nullptr;
            if (!px || px->raw.empty() || px->raw.size() % 4) return;
            leaves.push_back(px);
        }
        if (leaves.size() < 2) return;
        for (size_t i = 1; i < leaves.size(); i++)
            if (leaves[i]->raw != leaves[0]->raw) return;
        const std::vector<uint8_t> raw = leaves[0]->raw;   // A8R8G8B8: alpha first, then the colour
        const size_t n = raw.size() / 4;
        uint64_t sum = 0;
        for (size_t i = 0; i < raw.size(); i += 4) sum += uint64_t(raw[i + 1]) + raw[i + 2] + raw[i + 3];
        const double level = double(sum) / (3.0 * double(n));
        if (level <= kProbeStockLevel) return;
        const double k = kProbeStockLevel / level;
        uint8_t lut[256];
        for (int v = 0; v < 256; v++) lut[v] = uint8_t(std::min(255, int(v * k + 0.5)));
        std::vector<uint8_t> out = raw;
        for (size_t i = 0; i < out.size(); i += 4)
            for (int c = 1; c < 4; c++) out[i + size_t(c)] = lut[raw[i + size_t(c)]];
        for (Leaf* l : leaves) l->raw = out;
        char line[200];
        std::snprintf(line, sizeof line,
                      "  %zu stand-in reflection probes (the map has no real ones) dimmed to stock brightness (color level "
                      "%.0f -> %d)",
                      leaves.size(), level, kProbeStockLevel);
        log(line);
    }

    // The world's material memory list as every stock 360 map has it: one entry
    // for every material its surfaces draw with, sorted by name, each with 44
    // bytes a vertex of its vertex runs + 6 a triangle + 40 a surface. The PC
    // counts otherwise, and leaves out the composite materials made here.
    void Porter::WorldMaterialMemory(Struct* world)
    {
        Array* list = ArrayOf(world->Child("materialMemory"));
        Array* surfs = ArrayOf(world->Sub("dpvs")->Child("surfaces"));
        if (!list || list->items.empty() || !As<Struct>(list->items[0]) || !surfs) return;
        Struct* tmpl = As<Struct>(list->items[0]);
        // The PC's own entries are kept (re-ordered, with the 360's count): the
        // list is written before the surfaces, so it may hold a material itself.
        // Materials it lacks are pointed at through the asset list.
        std::unordered_map<const Node*, Struct*> listed;
        for (Node* n : list->items)
        {
            Struct* e = As<Struct>(n);
            Node* c = e ? e->Child("material") : nullptr;
            Node* m = As<Struct>(c) ? c : c ? Deref(c) : nullptr;
            if (As<Struct>(m)) listed.emplace(m, e);
        }
        struct Use
        {
            std::string name;
            Ref* ref;
            std::set<std::pair<int64_t, int64_t>> runs;
            int64_t tris = 0, surfs = 0;
        };
        Ordered<const Node*, Use> per;
        const int64_t count = world->Int("surfaceCount") ? world->Int("surfaceCount") : int64_t(surfs->items.size());
        for (size_t i = 0; i < std::min(surfs->items.size(), size_t(count)); i++)
        {
            Struct* sf = As<Struct>(surfs->items[i]);
            Node* c = sf ? sf->Child("material") : nullptr;
            Struct* m = c ? DerefStruct(c) : nullptr;
            if (!m || !As<Ref>(c)) return;   // (a surface whose material isn't pointed at as usual: list kept)
            Use& u = per.FindOrAdd(m, Use{ AssetName(m).value_or(Name(m).value_or("")), As<Ref>(c), {} });
            Struct* t = sf->Sub("tris");
            u.runs.insert({ t->Int("firstVertex"), t->Int("vertexCount") });
            u.tris += t->Int("triCount");
            u.surfs++;
        }
        std::vector<std::pair<const Node*, Use*>> sorted;
        for (auto& [m, u] : per) sorted.push_back({ m, &u });
        std::stable_sort(sorted.begin(), sorted.end(), [](const auto& a, const auto& b) { return a.second->name < b.second->name; });
        std::vector<Node*> made;
        for (auto& [m, u] : sorted)
        {
            Struct* entry;
            if (auto old = listed.find(m); old != listed.end()) entry = old->second->Copy();
            else if (As<Entry>(u->ref->target))
            {
                Ref* r = A().Make<Ref>(1);
                r->target = u->ref->target;
                r->rel = u->ref->rel;
                r->t = u->ref->t;
                entry = tmpl->Copy();
                Children* ch = A().MakeChildren();
                ch->Set("material", r);
                entry->SetCh(ch);
                entry->SetPtr("material", 1);   // (an alias: a non-null placeholder)
            }
            else
            {
                Warn("world material list left as the PC file has it (material " + u->name + " isn't pointed at as expected)");
                return;
            }
            int64_t vertices = 0;
            for (const auto& r : u->runs) vertices += r.second;
            entry->Set("memory", 44 * vertices + 6 * u->tris + 40 * u->surfs);
            made.push_back(entry);
        }
        const size_t before = list->items.size();
        list->items = std::move(made);
        world->Set("materialMemoryCount", int64_t(list->items.size()));
        if (list->items.size() != before)
            log("  world material list: " + std::to_string(list->items.size()) +
                " materials as the 360 counts them (the PC file listed " + std::to_string(before) + ")");
    }

    // rawRowData is plain bytes in the definitions but holds one row per
    // rowDataStart entry (at 4 x that): colStart, colCount, zStart, zCount
    // (16-bit), firstEntry (32-bit), then a byte table. The headers need the
    // 360's byte order: as they were, every model lit from the grid read
    // garbage rows.
    void Porter::PostGfxLightGrid(Struct* d)
    {
        Leaf* starts = LeafOf(d->Child("rowDataStart"));
        Leaf* raw = LeafOf(d->Child("rawRowData"));
        if (!starts || !raw || swappedRows.count(raw)) return;
        swappedRows.insert(raw);
        const std::vector<uint32_t> rows = Words16(starts, size_t(starts->n));
        const std::vector<uint8_t> src = raw->raw;
        for (uint32_t s : std::set<uint32_t>(rows.begin(), rows.end()))
        {
            const size_t o = 4 * size_t(s);
            if (o + 12 > src.size()) continue;
            for (int k = 0; k < 4; k++) Store(raw->raw.data() + o + 2 * size_t(k), 2, Load(src.data() + o + 2 * size_t(k), 2, false), true);
            Store(raw->raw.data() + o + 8, 4, Load(src.data() + o + 8, 4, false), true);
        }
    }

    // An effect element's colour: the PC keeps it as a D3DCOLOR (B, G, R, A),
    // the 360 as A, R, G, B.
    void Porter::PostFxElemVisualState(Struct* d)
    {
        const auto c = d->Bytes("color");
        std::reverse(c.begin(), c.end());
    }

    namespace
    {
        // A glass type's texture vectors, rounded: what its 360-only numbers follow.
        std::string GlassKey(const Struct* d)
        {
            std::string key;
            for (size_t k = 0; k < 4; k++)
            {
                char b[32];
                std::snprintf(b, sizeof b, "%.6f,", std::abs(d->F("texVecs", k)));
                key += b;
            }
            return key;
        }
    }

    // Two 360-only numbers per glass type: stock maps always hold 0.4077 in the
    // second; the first follows the material (and its texture scale). Taken
    // from a stock glass type with the same material, else com_glass_clear's.
    void Porter::PostFxGlassDef(Struct* d)
    {
        if (d->Int("unknown", 0) || d->Int("unknown", 1)) return;
        Struct* m = DerefStruct(d->Child("material"));
        std::string name = m ? Name(m).value_or("") : "";
        name = WithoutCommas(std::move(name));
        if (!stockGlass)
        {
            stockGlass.emplace();
            for (StockFile* f : refs)
                ForEach(f->tree.assets, [&](Node* n) {
                    Struct* o = As<Struct>(n);
                    if (!o || !o->Find("halfThickness") || !o->Find("texVecs") || !o->Find("unknown")) return;
                    const std::array<uint32_t, 2> u{ uint32_t(o->U("unknown", 0)), uint32_t(o->U("unknown", 1)) };
                    if (!u[0] && !u[1]) return;
                    Struct* sm = DerefStruct(o->Child("material"));
                    if (!sm) return;
                    std::string mn = Name(sm).value_or("");
                    mn = WithoutCommas(std::move(mn));
                    stockGlass->emplace(mn, u);
                    stockGlass->emplace(mn + "|" + GlassKey(o), u);
                });
        }
        std::array<uint32_t, 2> u{ 986759600, 1053868464 };   // 0.001593, 0.4077
        if (auto it = stockGlass->find(name + "|" + GlassKey(d)); it != stockGlass->end()) u = it->second;
        else if (auto it2 = stockGlass->find(name); it2 != stockGlass->end()) u = it2->second;
        d->Set("unknown", u[0], 0);
        d->Set("unknown", u[1], 1);
    }

    // firstFreePiece is really a 16-bit number (then padding): 0xFFFF reads
    // 0x0000FFFF on the PC and 0xFFFF0000 on the 360.
    void Porter::PostFxGlassSystem(Struct* d)
    {
        const uint64_t v = d->U("firstFreePiece");
        if (v <= 0xFFFF) d->Set("firstFreePiece", int64_t(v << 16));
    }

    // IW4x's ZoneBuilder leaves the collision checksum 0 but writes 0xDEADBEEF
    // into the world's: stock maps have the same number in both.
    void Porter::PostClipMap(Struct* d)
    {
        if (!d->Int("checksum")) d->Set("checksum", worldChecksum);
    }

    // Single-player actors (IW4x maps often keep one) have no spawn code in
    // multiplayer and name models this file doesn't carry: left out.
    void Porter::PostMapEnts(Struct* d)
    {
        Leaf* lf = As<Leaf>(d->Child("entityString"));
        if (!lf) return;
        std::string text(lf->raw.begin(), lf->raw.end());
        while (!text.empty() && text.back() == '\0') text.pop_back();
        static const std::regex entity(R"(\{[^{}]*\})"), classname(R"re("classname"\s+"([^"]*)")re");
        std::vector<std::string> kept;
        int dropped = 0;
        for (std::sregex_iterator it(text.begin(), text.end(), entity), end; it != end; ++it)
        {
            const std::string ent = it->str();
            std::smatch m;
            if (std::regex_search(ent, m, classname) && m[1].str().starts_with("actor_"))
            {
                dropped++;
                continue;
            }
            kept.push_back(ent);
        }
        if (!dropped) return;
        Warn("left out " + std::to_string(dropped) + " single-player actor entities");
        std::string out;
        for (size_t i = 0; i < kept.size(); i++) out += (i ? "\n" : "") + kept[i];
        out += "\n";
        lf->raw.assign(out.begin(), out.end());
        lf->raw.push_back(0);
        lf->n = int64_t(lf->raw.size());
        d->Set("numEntityChars", lf->n);
    }

    // ---- models -------------------------------------------------------------

    // The PC's buffer handle means nothing here; the 360 builds its vertex and
    // index buffers from the surface's data when the file loads.
    void Porter::PostXSurface(Struct* d)
    {
        d->Set("zoneHandle", 0);
        d->Set("unknown", 0);
        for (const char* k : { "vertexBuffer", "indexBuffer" })
        {
            const auto b = d->Bytes(k);
            std::fill(b.begin(), b.end(), uint8_t(0));
        }
    }

    void Porter::PostXModel(Struct* d)
    {
        // The 360 keeps a number per surface for texture streaming (himipRadii),
        // read for every model when the file loads.
        const int64_t n = d->Int("numsurfs");
        d->SetPtr("himipRadii", n ? kFollowing : 0);
        if (n)
            d->SetChild(d->M("himipRadii").name,
                        A().Make<Leaf>(d->M("himipRadii").type, n, Pack16(std::vector<uint32_t>(size_t(n), kHimipRadius), true), true));
        // Every model in the stock 360 files has lodRampType 0; the PC marks
        // skinned ones 1.
        if (d->Int("lodRampType")) d->Set("lodRampType", 0);
        FixBoneBounds(d);
        // Each detail level's partBits and surfs: stock 360 models always hold 0
        // and null (the game fills them in when it loads the model).
        const Member& lods = d->M("lodInfo");
        const size_t count = lods.mods.empty() ? 1 : size_t(lods.mods[0]);
        for (size_t k = 0; k < count; k++)
        {
            Struct* li = d->Sub(lods, int64_t(k));
            const auto bits = li->Bytes("partBits");
            std::fill(bits.begin(), bits.end(), uint8_t(0));
            Node* c = li->Child("surfs");
            if (!c || As<Ref>(c))
            {
                li->PopChild("surfs");
                li->SetPtr("surfs", 0);
            }
        }
    }

    // A single-bone model's XBoneInfo rebuilt from its vertices when it is
    // broken (a negative half-size, a box that misses vertices, or a radius
    // squared that isn't the half-diagonal's), as stock 360 models have it.
    void Porter::FixBoneBounds(Struct* d)
    {
        if (d->Int("numBones") != 1) return;
        Leaf* info = LeafOf(d->Child("boneInfo"));
        if (!info || info->raw.size() < 28) return;
        std::vector<std::array<double, 3>> pts;
        const Member& lods = d->M("lodInfo");
        const size_t count = std::min<size_t>(lods.mods.empty() ? 1 : size_t(lods.mods[0]), size_t(std::max<int64_t>(0, d->Int("numLods"))));
        for (size_t k = 0; k < count; k++)
        {
            Node* ms = Target(d->Sub(lods, int64_t(k))->Child("modelSurfs"));
            if (Slot* s = As<Slot>(ms)) ms = s->asset;
            Struct* mss = As<Struct>(ms);
            Array* surfs = mss ? ArrayOf(mss->Child("surfs")) : nullptr;
            if (!surfs) continue;
            for (Node* sf : surfs->items)
            {
                Struct* s = As<Struct>(sf);
                Leaf* v = s ? LeafOf(s->Child("verts0")) : nullptr;
                if (!v || v->raw.empty()) continue;
                const size_t size = v->t && v->t->size ? v->t->size : 32;
                for (size_t i = 0; i < v->raw.size() / size; i++) pts.push_back({ F32(v, size * i), F32(v, size * i + 4), F32(v, size * i + 8) });
            }
        }
        if (pts.empty()) return;
        double lo[3], hi[3], mid[3], half[3];
        for (int k = 0; k < 3; k++)
        {
            lo[k] = hi[k] = pts[0][size_t(k)];
            for (const auto& p : pts)
            {
                lo[k] = std::min(lo[k], p[size_t(k)]);
                hi[k] = std::max(hi[k], p[size_t(k)]);
            }
            mid[k] = (lo[k] + hi[k]) / 2;
            half[k] = (hi[k] - lo[k]) / 2;
        }
        double old[7];
        for (int k = 0; k < 7; k++) old[k] = F32(info, 4 * size_t(k));
        bool broken = false;
        for (double x : old) broken |= !std::isfinite(x);
        for (int k = 0; k < 3; k++)
            broken |= old[3 + k] < 0 || old[k] - old[3 + k] > lo[k] + 0.5 || old[k] + old[3 + k] < hi[k] - 0.5;
        const double r2 = old[3] * old[3] + old[4] * old[4] + old[5] * old[5];
        broken |= std::abs(r2 - old[6]) > std::max(1.0, 0.01 * old[6]);
        if (!broken) return;
        const double values[7] = { mid[0], mid[1], mid[2], half[0], half[1], half[2],
                                   half[0] * half[0] + half[1] * half[1] + half[2] * half[2] };
        for (int k = 0; k < 7; k++) StoreF32(info->raw.data() + 4 * k, float(values[k]), info->big);
        boneBounds++;
    }

    std::vector<uint8_t> Pack16(const std::vector<uint32_t>& v, bool big)
    {
        std::vector<uint8_t> raw(2 * v.size());
        for (size_t i = 0; i < v.size(); i++) Store(raw.data() + 2 * i, 2, v[i], big);
        return raw;
    }
}
