// The generic part of the conversion: every PC struct carried over to the
// 360's struct of the same name, member by member, with the hooks of
// porter.h on the structs that need more.
#include "porter.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <mutex>
#include <set>

namespace mapport
{
    using zone::Member;
    using zone::Type;

    namespace
    {
        // How many pointers a pointer member holds: one (T* x, T (*x)[n]), or
        // one an element of an array of them (T* x[n]).
        size_t PointerCount(const Member& m)
        {
            size_t n = 1;
            for (int x : m.mods)
            {
                if (x == zone::kPointer) break;
                n *= size_t(x);
            }
            return n;
        }

        // A member's name in a tree, "_<index>" for an unnamed one. It outlives
        // the conversion (a struct's children and late parts keep views of it):
        // the schema's own name, or one kept here.
        std::string_view Key(const Type* t, const Member& m)
        {
            if (!m.name.empty()) return m.name;
            static std::mutex lock;
            static std::set<std::string, std::less<>> kept;
            const std::string name = "_" + std::to_string(&m - t->members.data());
            std::lock_guard hold(lock);
            return *kept.insert(name).first;
        }

        const Member* MemberNamed(const Type* t, std::string_view key)
        {
            for (const Member& m : t->members)
                if (Key(t, m) == key) return &m;
            return nullptr;
        }

        bool IsStruct(const Type* t)
        {
            t = t->Unwrapped();
            return t->kind == Type::Struct || t->kind == Type::Union;
        }

        // The element size when a member is made only of numbers (or
        // pointers) of one size, else 0.
        uint32_t Uniform(const Member& m)
        {
            if (m.HasPointer()) return 4;
            const Type* t = m.type;
            if (t->kind == Type::Prim || t->kind == Type::Enum) return m.bits >= 0 ? 0 : t->size;
            if (!t->Compound()) return 0;
            std::set<uint32_t> sizes;
            for (const Member& x : t->members)
            {
                const uint32_t u = Uniform(x);
                if (!u) return 0;
                sizes.insert(u);
            }
            return sizes.size() == 1 ? *sizes.begin() : 0;
        }

        std::vector<uint8_t> SwapWords(const uint8_t* raw, size_t n, uint32_t size)
        {
            std::vector<uint8_t> b(raw, raw + n);
            if (size <= 1) return b;
            for (size_t i = 0; i + size <= n; i += size) std::reverse(b.begin() + ptrdiff_t(i), b.begin() + ptrdiff_t(i + size));
            return b;
        }

        // The unions whose members are separate bytes on both platforms (the
        // "packed" number is only a way to copy them): their bytes stay.
        bool ByteUnion(const std::string& name) { return name == "GfxSurfaceLightingAndFlags"; }
    }

    // ---- structs ------------------------------------------------------------

    void Porter::ConvertStruct(Struct* d, const Type* tp, const Type* tx, bool track)
    {
        tx = tx->Unwrapped();
        Struct* x = Struct::Zero(tx, true);
        if (track)
        {
            if (done.count(d)) return;
            done.insert(d);
        }
        if (Pre(d, tp->Unwrapped(), tx, x)) return;
        ConvertInto(d, tp->Unwrapped(), x, false, track);
        d->Adopt(*x);
        if (tx->kind == Type::Struct) Post(d, tx);
    }

    // Converts `d` into `x`, the 360 struct being built (a view of it, for a
    // member). `member`: a member's struct, which runs its own hooks here;
    // `track`: it is noted done (not a throwaway element of plain data).
    void Porter::ConvertInto(Struct* d, const Type* tp, Struct* x, bool member, bool track)
    {
        const Type* tx = x->type;
        if (member)
        {
            if (track)
            {
                if (done.count(d)) return;
                done.insert(d);
            }
            if (Pre(d, tp, tx, x)) return;
        }
        if (tx->kind == Type::Union)
        {
            ConvertUnion(d, tp, x, track);
            ConvertChildren(d, tp, tx);
            x->SetCh(d->Ch());
            return;
        }
        if (tx->kind == Type::Typedef) return;
        for (const Member& mx : tx->members)
        {
            const Member* mp = MemberNamed(tp, Key(tx, mx));
            // (A member only the 360 has: zero, or what a hook set already.)
            if (!mp) continue;
            if (!d->Has(Key(tp, *mp)) || mp->HasPointer() != mx.HasPointer()) continue;
            ConvertMember(d, *mp, x, mx, track);
        }
        ConvertChildren(d, tp, tx);
        x->SetCh(d->Ch());
        x->asset = d->asset;
        x->slot = d->slot;
        x->forward = d->forward;
        if (member && tx->kind == Type::Struct) Post(x, tx);
    }

    void Porter::ConvertMember(Struct* d, const Member& mp, Struct* x, const Member& mx, bool track)
    {
        const std::string_view kp = Key(d->type, mp), kx = Key(x->type, mx);
        if (mx.HasPointer())
        {
            // Pointers: their values as they are (what they point at is converted
            // with the struct's children).
            const size_t n = std::min(PointerCount(mp), PointerCount(mx));
            for (size_t k = 0; k < n; k++) x->SetPtr(kx, d->Ptr(kp, k), k);
            return;
        }
        if (mx.bits >= 0)
        {
            x->Set(kx, d->Int(kp));
            return;
        }
        if (Node* late = d->Late(kp))
        {
            // Streamed after the struct's first part (never in a map's own
            // structs): converted where it is a struct, else as it is.
            if (Struct* s = As<Struct>(late)) ConvertStruct(s, mp.type, mx.type, track);
            else if (Array* a = As<Array>(late))
                for (Node* item : a->items)
                    if (Struct* e = As<Struct>(item)) ConvertStruct(e, mp.type, mx.type, track);
            x->SetLate(kx, late);
            return;
        }
        const std::vector<int> dp = mp.Dims(), dx = mx.Dims();
        // Element k (flat) of each side's array, the leading dimension as far as both have it.
        std::vector<std::pair<size_t, size_t>> pairs;
        std::function<void(size_t, size_t, size_t)> walk = [&](size_t depth, size_t fp, size_t fx) {
            if (depth == std::max(dp.size(), dx.size()))
            {
                pairs.push_back({ fp, fx });
                return;
            }
            const size_t np = depth < dp.size() ? size_t(dp[depth]) : 1, nx = depth < dx.size() ? size_t(dx[depth]) : 1;
            for (size_t k = 0; k < std::min(np, nx); k++)
                walk(depth + 1, fp * (depth < dp.size() ? np : 1) + k, fx * (depth < dx.size() ? nx : 1) + k);
        };
        walk(0, 0, 0);
        if (IsStruct(mp.type) && IsStruct(mx.type))
        {
            for (const auto& [fp, fxk] : pairs)
            {
                Struct* ps = d->Sub(mp, dp.empty() ? -1 : int64_t(fp));
                Struct* xs = x->Sub(mx, dx.empty() ? -1 : int64_t(fxk));
                ConvertInto(ps, mp.type->Unwrapped(), xs, true, track);
            }
            return;
        }
        const Type* tp = mp.type;
        const Type* txt = mx.type;
        if (tp->Compound() || txt->Compound())
        {
            // A typedef around an array of numbers (vec3_t): number for number.
            const Member& ip = tp->Compound() ? tp->members[0] : mp;
            const Member& ix = txt->Compound() ? txt->members[0] : mx;
            const size_t n = std::min(mp.size, mx.size) / std::max<uint32_t>(1, ip.type->size);
            for (size_t k = 0; k < n; k++)
            {
                const uint8_t* src = d->Data() + mp.offset + k * ip.type->size;
                uint8_t* dst = x->Data() + mx.offset + k * ix.type->size;
                if (ip.type->isFloat && ip.type->size == 4) StoreF32(dst, LoadF32(src, d->big), x->big);
                else Store(dst, ix.type->size, Load(src, ip.type->size, d->big), x->big);
            }
            return;
        }
        const uint32_t sp = tp->size, sx = txt->size;
        for (const auto& [fp, fxk] : pairs)
        {
            const uint8_t* src = d->Data() + mp.offset + fp * sp;
            uint8_t* dst = x->Data() + mx.offset + fxk * sx;
            if (tp->isFloat)
            {
                const double v = sp == 4 ? double(LoadF32(src, d->big)) : [&] {
                    const uint64_t bits = Load(src, 8, d->big);
                    double f;
                    std::memcpy(&f, &bits, 8);
                    return f;
                }();
                if (std::isnan(v) || std::isinf(v))
                {
                    // Kept as the bytes they are, not swapped: the 360 reads them as
                    // a tiny number, not as NaN or infinity.
                    std::memcpy(dst, src, std::min(sp, sx));
                    continue;
                }
                if (sx == 4) StoreF32(dst, float(v), x->big);
                else
                {
                    uint64_t bits;
                    std::memcpy(&bits, &v, 8);
                    Store(dst, 8, bits, x->big);
                }
                continue;
            }
            uint64_t v = Load(src, sp, d->big);
            if (tp->isSigned && sp < 8 && (v >> (sp * 8 - 1)) & 1) v |= ~uint64_t(0) << (sp * 8);
            Store(dst, sx, v, x->big);
        }
    }

    // A union's bytes, converted as the member in use: the one a walk went
    // into, else the only member without pointers, else (members all numbers
    // of one size) any of them.
    void Porter::ConvertUnion(Struct* d, const Type* tp, Struct* x, bool track)
    {
        const Type* tx = x->type;
        const size_t n = std::min(d->Size(), x->Size());
        if (ByteUnion(tx->name))
        {
            std::memcpy(x->Data(), d->Data(), n);
            return;
        }
        if (tx->name == "FxGlassGeometryData")
        {
            // A glass piece's vertices (two 16-bit numbers) and fan indexes; the
            // hole and crack headers only exist at run time.
            const std::vector<uint8_t> b = SwapWords(d->Data(), n, 2);
            std::memcpy(x->Data(), b.data(), n);
            return;
        }
        std::vector<std::string_view> names = d->used;
        if (names.empty())
        {
            std::set<uint32_t> words;
            bool none = false;
            for (const Member& m : tx->members)
            {
                const uint32_t u = Uniform(m);
                if (!u) none = true;
                words.insert(u);
            }
            if (words.size() == 1 && !none)
            {
                const std::vector<uint8_t> b = SwapWords(d->Data(), n, *words.begin());
                std::memcpy(x->Data(), b.data(), n);
                return;
            }
            for (const Member& m : tx->members)
                if (m.mods.empty() && m.bits < 0 && (m.type->kind == Type::Prim || m.type->kind == Type::Enum) &&
                    m.type->size == d->Size())
                {
                    // A "packed" member holding the whole thing as one number.
                    const std::vector<uint8_t> b = SwapWords(d->Data(), n, m.type->size);
                    std::memcpy(x->Data(), b.data(), n);
                    return;
                }
            for (const Member& m : tx->members)
                if (!m.HasPointer() && !(m.type->Compound() && [&] {
                        std::function<bool(const Type*)> ptrs = [&](const Type* t) {
                            for (const Member& y : t->members)
                                if (y.HasPointer() || (y.type->Compound() && ptrs(y.type))) return true;
                            return false;
                        };
                        return ptrs(m.type);
                    }()))
                    names.push_back(Key(tx, m));
            if (names.size() != 1) throw PortError("don't know which member of union " + tx->name + " is in use");
        }
        for (const Member& mx : tx->members)
        {
            const std::string_view k = Key(tx, mx);
            if (std::find(names.begin(), names.end(), k) == names.end()) continue;
            const Member* mp = MemberNamed(tp, k);
            if (!mp) continue;
            ConvertMember(d, *mp, x, mx, track);
        }
    }

    // ---- what pointers loaded -----------------------------------------------

    void Porter::ConvertChildren(Struct* d, const Type* tp, const Type* tx)
    {
        Children* ch = d->Ch();
        if (!ch) return;
        Children* out = A().MakeChildren();
        for (const auto& [key, c] : ch->items)
        {
            const Member* mx = MemberNamed(tx, std::string(key.member));
            if (!mx) continue;
            const Member* mp = MemberNamed(tp, std::string(key.member));
            if (!mp) mp = mx;   // put there by a hook: in 360 terms already, or plain data
            out->items.push_back({ { mx->name.empty() ? key.member : std::string_view(mx->name), key.idx },
                                   ConvertChild(c, mp->type, mx->type) });
        }
        d->SetCh(out);
    }

    Node* Porter::ConvertChild(Node* c, const Type* tp, const Type* tx)
    {
        switch (c->kind)
        {
        case Node::Kind::PtrList:
            for (Node*& item : static_cast<PtrList*>(c)->items)
                if (item) item = ConvertChild(item, tp, tx);
            return c;
        case Node::Kind::Array:
            for (Node* item : static_cast<Array*>(c)->items)
                if (Struct* s = As<Struct>(item)) ConvertStruct(s, tp, tx);
            return c;
        case Node::Kind::Struct:
        {
            Struct* s = static_cast<Struct*>(c);
            if (s->IsAsset()) ConvertAsset(s);
            else ConvertStruct(s, tp, tx);
            return c;
        }
        case Node::Kind::Leaf:
            return ConvertLeaf(static_cast<Leaf*>(c), tx);
        default:
            return c;
        }
    }

    // Plain data, in place (pointers may refer to it).
    Node* Porter::ConvertLeaf(Leaf* lf, const Type* tx)
    {
        if (lf->big) return lf;
        if (tx->name == "GfxWorldVertex" || tx->name == "GfxPackedVertex")
        {
            // Floats to big-endian; colour and (for model vertices) packed
            // texture coordinates as the same 32-bit value; normal and tangent
            // repacked from the PC's 8-bit + scale to the 360's 10:10:10.
            const bool world = tx->name == "GfxWorldVertex";
            const size_t size = world ? 44 : 32;
            std::vector<uint8_t> out(lf->raw.size());
            std::unordered_map<uint32_t, uint32_t> cache;
            auto unit = [&](const uint8_t* b) {
                uint32_t key;
                std::memcpy(&key, b, 4);
                auto found = cache.find(key);
                if (found != cache.end()) return found->second;
                const double s = (b[3] + 192) / 32385.0;
                uint32_t u = 0;
                for (int k = 0; k < 3; k++)
                {
                    const double v = std::clamp((b[k] - 127) * s, -1.0, 1.0) * 511;
                    const int x = int(std::nearbyint(v));    // half to even
                    u |= (uint32_t(x) & 0x3FF) << (10 * k);
                }
                cache[key] = u;
                return u;
            };
            for (size_t o = 0; o + size <= lf->raw.size(); o += size)
            {
                const uint8_t* src = lf->raw.data() + o;
                uint8_t* dst = out.data() + o;
                for (int k = 0; k < 4; k++) StoreF32(dst + 4 * k, LoadF32(src + 4 * k, false), true);
                std::reverse_copy(src + 16, src + 20, dst + 16);
                if (world)
                {
                    for (int k = 0; k < 4; k++) StoreF32(dst + 20 + 4 * k, LoadF32(src + 20 + 4 * k, false), true);
                    for (size_t k : { size_t(36), size_t(40) }) Store(dst + k, 4, unit(src + k), true);
                }
                else
                {
                    std::reverse_copy(src + 20, src + 24, dst + 20);
                    for (size_t k : { size_t(24), size_t(28) }) Store(dst + k, 4, unit(src + k), true);
                }
            }
            lf->raw = std::move(out);
            lf->big = true;
            return lf;
        }
        const Type* tp = lf->t;
        if (tp->kind == Type::Prim || tp->kind == Type::Enum)
        {
            if (tp->size != tx->size) throw PortError(tp->name + " changes size");
            lf->raw = SwapWords(lf->raw.data(), lf->raw.size(), tp->size);
            lf->t = tx;
            lf->big = true;
            return lf;
        }
        if (!tp->Compound()) throw PortError("can't convert " + tp->name);
        std::vector<uint8_t> out(size_t(tx->size) * size_t(lf->n));
        for (int64_t i = 0; i < lf->n; i++)
        {
            Struct* e = A().Make<Struct>(tp, false,
                                         std::vector<uint8_t>(lf->raw.begin() + ptrdiff_t(size_t(i) * tp->size),
                                                              lf->raw.begin() + ptrdiff_t(size_t(i + 1) * tp->size)));
            ConvertStruct(e, tp, tx, false);
            std::memcpy(out.data() + size_t(i) * tx->size, e->Data(), std::min<size_t>(e->Size(), tx->size));
        }
        lf->raw = std::move(out);
        lf->t = tx;
        lf->big = true;
        return lf;
    }

    void Porter::ConvertAsset(Struct* d)
    {
        if (done.count(d)) return;
        const std::string name(d->asset);
        auto p = P.infos.find(name);
        auto x = X.infos.find(name);
        if (p == P.infos.end() || x == X.infos.end()) throw PortError("the 360 has no " + name + " assets");
        ConvertStruct(d, p->second->ctype, x->second->ctype);
    }

    // ---- hooks --------------------------------------------------------------

    bool Porter::Pre(Struct* d, const Type* tp, const Type* tx, Struct* x)
    {
        const std::string& n = tx->name;
        if (n == "GfxWorldDpvsDynamic") return PreGfxWorldDpvsDynamic(d, x);
        if (n == "MaterialTechniqueSet") return PreMaterialTechniqueSet(d);
        if (n == "GfxStaticModelDrawInst") return PreGfxStaticModelDrawInst(d, x);
        if (n == "water_t") return PreWaterT(d, x);
        if (n == "GfxImage") return PreGfxImage(d);
        if (n == "Material") return PreMaterial(d);
        (void)tp;
        return false;
    }

    void Porter::Post(Struct* d, const Type* tx)
    {
        const std::string& n = tx->name;
        if (n == "GfxAabbTree") PostGfxAabbTree(d);
        else if (n == "GfxWorldDpvsStatic") PostGfxWorldDpvsStatic(d);
        else if (n == "GfxWorldDraw") PostGfxWorldDraw(d);
        else if (n == "GfxWorld") PostGfxWorld(d);
        else if (n == "GfxLightGrid") PostGfxLightGrid(d);
        else if (n == "FxElemVisualState") PostFxElemVisualState(d);
        else if (n == "FxGlassDef") PostFxGlassDef(d);
        else if (n == "FxGlassSystem") PostFxGlassSystem(d);
        else if (n == "clipMap_t") PostClipMap(d);
        else if (n == "MapEnts") PostMapEnts(d);
        else if (n == "XSurface") PostXSurface(d);
        else if (n == "XModel") PostXModel(d);
        else if (n == "Material") PostMaterial(d);
    }

    // ---- pointers into objects of another layout ----------------------------

    namespace
    {
        int64_t MemberOffset(const Type* tp, const Type* tx, int64_t rem)
        {
            if (rem == 0) return 0;
            for (const Member& m : tp->members)
            {
                if (!(int64_t(m.offset) <= rem && rem < int64_t(m.offset + m.size))) continue;
                const Member* mx = nullptr;
                for (const Member& c : tx->members)
                    if (c.name == m.name) mx = &c;
                if (!mx) throw PortError("a pointer points at " + tp->name + "." + m.name + ", which the 360 doesn't have");
                const int64_t inner = rem - m.offset;
                if (m.type->Compound() && m.Dims().empty()) return mx->offset + MemberOffset(m.type, mx->type, inner);
                return mx->offset + inner;
            }
            throw PortError("a pointer points into padding of " + tp->name);
        }
    }

    int64_t MapRel(const Ref* r, const zone::Schema& X)
    {
        const Type* t = r->t.t;
        if (!t || !t->Compound()) return r->rel;
        auto found = t->name.empty() ? X.defs.types.end() : X.defs.types.find(t->name);
        if (found == X.defs.types.end()) return r->rel;
        const Type* tx = found->second;
        bool same = tx->size == t->size && tx->members.size() == t->members.size();
        for (size_t k = 0; same && k < t->members.size(); k++) same = tx->members[k].offset == t->members[k].offset;
        if (same) return r->rel;
        const int64_t i = r->rel / t->size, rem = r->rel % t->size;
        return i * tx->size + MemberOffset(t, tx, rem);
    }
}
