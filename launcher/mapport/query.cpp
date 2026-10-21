#include "query.h"

#include <zlib.h>

#include <algorithm>
#include <cctype>

namespace mapport
{
    using zone::Member;
    using zone::Type;

    namespace
    {
        bool IsAssetNode(const Node* c) { return (As<Struct>(c) && As<Struct>(c)->IsAsset()) || As<Ref>(c); }

        Node* AssetInMember(const Type* t, Struct* tgt, int64_t rel)
        {
            if (!tgt) return nullptr;
            for (const Member& m : t->members)
            {
                if (int64_t(m.offset) == rel && m.IsPointer())
                {
                    if (Node* c = tgt->Child(m.name); IsAssetNode(c)) return c;
                }
                else if (!m.mods.empty() && m.mods.back() == zone::kPointer && m.mods.size() > 1 &&
                         int64_t(m.offset) <= rel && rel < int64_t(m.offset + m.size) && (rel - m.offset) % 4 == 0)
                {
                    // One pointer of an array of them (FxElemMarkVisuals.materials[2],
                    // which effects share: "the same material as that element's").
                    Node* list = tgt->Child(m.name);
                    const size_t k = size_t((rel - m.offset) / 4);
                    Node* c = nullptr;
                    if (PtrList* pl = As<PtrList>(list)) c = k < pl->items.size() ? pl->items[k] : nullptr;
                    else if (Array* a = As<Array>(list)) c = k < a->items.size() ? a->items[k] : nullptr;
                    if (IsAssetNode(c)) return c;
                }
                else if (m.mods.empty() && m.type->Compound() && int64_t(m.offset) <= rel &&
                         rel < int64_t(m.offset + m.type->size))
                {
                    // A pointer inside an inline struct or union (MaterialTextureDef.u.image).
                    Struct* sub = tgt->Has(m.name) ? tgt->Sub(m.name) : nullptr;
                    if (Node* c = AssetInMember(m.type, sub, rel - m.offset)) return c;
                }
            }
            return nullptr;
        }
    }

    Node* AssetInSlot(const Ref* r)
    {
        Node* tgt = r->target;
        int64_t rel = r->rel;
        if (Entry* e = As<Entry>(tgt)) return rel == 4 ? e->asset : nullptr;
        if (Slot* s = As<Slot>(tgt)) return s->asset;
        if (PtrList* pl = As<PtrList>(tgt))
        {
            // An element of another asset's pointer array (XModel.materialHandles).
            if (rel % 4 || size_t(rel / 4) >= pl->items.size()) return nullptr;
            Node* c = pl->items[size_t(rel / 4)];
            return IsAssetNode(c) ? c : nullptr;
        }
        const Type* t = r->t.t;
        if (!t || (t->kind != Type::Struct && t->kind != Type::Union)) return nullptr;
        if (Array* list = As<Array>(tgt))
        {
            const size_t i = size_t(rel / t->size);
            rel %= t->size;
            if (i >= list->items.size()) return nullptr;
            tgt = list->items[i];
        }
        return AssetInMember(t, As<Struct>(tgt), rel);
    }

    Node* PtrArrayValue(const Ref* r)
    {
        const Type* t = r->t.t;
        Array* list = As<Array>(r->target);
        if (!list || !t || t->kind != Type::Struct || !(r->rel % t->size)) return nullptr;
        const size_t i = size_t(r->rel / t->size);
        Struct* el = i < list->items.size() ? As<Struct>(list->items[i]) : nullptr;
        const int64_t off = r->rel % t->size;
        for (const Member& m : t->members)
            if (!m.mods.empty() && m.mods.back() == zone::kPointer && m.mods.size() > 1 && int64_t(m.offset) < off &&
                off < int64_t(m.offset + m.size) && el)
            {
                Node* items = el->Child(m.name);
                const size_t k = size_t((off - m.offset) / 4);
                if (PtrList* pl = As<PtrList>(items)) return k < pl->items.size() ? pl->items[k] : nullptr;
                if (Array* a = As<Array>(items)) return k < a->items.size() ? a->items[k] : nullptr;
            }
        return nullptr;
    }

    Node* Deref(Node* c)
    {
        if (Ref* r = As<Ref>(c))
        {
            if (Node* a = AssetInSlot(r)) return Deref(a);
            if (r->rel == 0 && As<Struct>(r->target)) return r->target;
            return nullptr;
        }
        return c;
    }

    Struct* DerefStruct(Node* c) { return As<Struct>(Deref(c)); }

    Struct* Embedded(const Struct* d, std::string_view member)
    {
        const Member* m = d->Find(member);
        if (!m || !m->mods.empty() || !d->Has(member)) return nullptr;
        const Type* t = m->type->Unwrapped();
        if (t->kind != Type::Struct && t->kind != Type::Union) return nullptr;
        return const_cast<Struct*>(d)->Sub(*m);
    }

    namespace
    {
        Node* NameChild(const Struct* d, std::string_view member)
        {
            Node* c = d->Child(member);
            if (!c)
                if (Struct* info = Embedded(d, "info"); info && info->type->kind == Type::Struct) c = info->Child(member);
            return c;
        }
    }

    std::optional<std::string> AssetName(const Struct* d)
    {
        if (const String* s = As<String>(NameChild(d, "name"))) return s->b;
        return std::nullopt;
    }

    std::optional<std::string> Name(const Struct* d)
    {
        Node* c = NameChild(d, "name");
        if (const Ref* r = As<Ref>(c); r && r->rel == 0 && As<String>(r->target)) c = r->target;
        if (const String* s = As<String>(c)) return s->b;
        return std::nullopt;
    }

    void SetName(Struct* d, std::string name)
    {
        Struct* holder = d;
        if (!d->Child("name"))
            if (Struct* info = Embedded(d, "info")) holder = info;
        holder->SetChild("name", A().Make<String>(std::move(name)));
    }

    std::optional<std::string> AliasName(const Struct* d)
    {
        Node* c = d->Child("aliasName");
        if (const Ref* r = As<Ref>(c); r && r->rel == 0 && As<String>(r->target)) c = r->target;
        if (const String* s = As<String>(c)) return s->b;
        return std::nullopt;
    }

    std::optional<std::string> PoolName(const Struct* d)
    {
        std::optional<std::string> n = d->IsAsset("snd_alias_list_t") ? AliasName(d) : AssetName(d);
        if (!n && !d->IsAsset("snd_alias_list_t")) n = Name(d);
        if (!n)
            if (const String* f = As<String>(d->Child("filename"))) n = f->b;
        return n;
    }

    // ---- raw files ----------------------------------------------------------

    namespace
    {
        Leaf* RawfileData(Struct* d)
        {
            Struct* data = d->Sub("data");
            Node* c = data->Child("buffer");
            if (!c) c = data->Child("compressedBuffer");
            if (Ref* r = As<Ref>(c)) c = r->target;
            return As<Leaf>(c);
        }
    }

    std::string RawfileText(Struct* d)
    {
        Leaf* lf = RawfileData(d);
        if (!lf) throw Error("a raw file without its text");
        std::vector<uint8_t> raw = lf->raw;
        if (const int64_t packed = d->Int("compressedLen"))
        {
            constexpr size_t kLargest = 64u << 20;   // a script, an arena: far less
            const int64_t len = d->Int("len");
            if (len < 0 || size_t(len) > kLargest) throw Error("a raw file of " + std::to_string(len) + " bytes");
            std::vector<uint8_t> out(std::max<size_t>(size_t(len) + 1, 1 << 12));
            z_stream z{};
            if (inflateInit(&z) != Z_OK) throw Error("zlib would not start");
            z.next_in = raw.data();
            z.avail_in = uInt(std::min<size_t>(size_t(packed), raw.size()));
            int result = Z_OK;
            while (result == Z_OK)
            {
                if (z.total_out == out.size())
                {
                    if (out.size() > kLargest)
                    {
                        inflateEnd(&z);
                        throw Error("a raw file's text inflates past its length");
                    }
                    out.resize(out.size() * 2);
                }
                z.next_out = out.data() + z.total_out;
                z.avail_out = uInt(out.size() - z.total_out);
                result = inflate(&z, Z_NO_FLUSH);
            }
            const size_t made = z.total_out;
            inflateEnd(&z);
            if (result != Z_STREAM_END) throw Error("a raw file's text does not inflate");
            out.resize(made);
            raw = std::move(out);
        }
        raw.resize(std::min(raw.size(), size_t(std::max<int64_t>(0, d->Int("len")))));
        return std::string(raw.begin(), raw.end());
    }

    void SetRawfileText(Struct* d, const std::string& text)
    {
        Struct* data = d->Sub("data");
        Node* old = data->PopChild("buffer");
        if (!old) old = data->PopChild("compressedBuffer");
        Leaf* lf = As<Leaf>(old);
        if (!lf) throw Error("a raw file without its text");
        d->Set("compressedLen", 0);
        d->Set("len", int64_t(text.size()));
        std::vector<uint8_t> raw(text.begin(), text.end());
        raw.push_back(0);
        data->SetChild("buffer", A().Make<Leaf>(lf->t, int64_t(raw.size()), std::move(raw), lf->big));
    }

    bool IsBuilderSignature(Struct* d)
    {
        Leaf* lf = RawfileData(d);
        if (!lf) return false;
        static const char kSigned[] = "FastFile built using";
        if (lf->raw.size() >= sizeof kSigned - 1 && std::equal(kSigned, kSigned + sizeof kSigned - 1, lf->raw.begin()))
            return true;
        const int64_t n = d->Int("compressedLen");
        if (!n) return false;
        std::vector<uint8_t> out(1 << 16);
        z_stream z{};
        if (inflateInit(&z) != Z_OK) return false;
        z.next_in = lf->raw.data();
        z.avail_in = uInt(std::min<size_t>(size_t(n), lf->raw.size()));
        int result = Z_OK;
        while (result == Z_OK)
        {
            z.next_out = out.data();
            z.avail_out = uInt(out.size());
            result = inflate(&z, Z_NO_FLUSH);
        }
        inflateEnd(&z);
        return result != Z_STREAM_END;
    }

    // ---- render state -------------------------------------------------------

    std::optional<std::vector<std::pair<uint32_t, uint32_t>>> StateWords(Node* table)
    {
        if (Ref* r = As<Ref>(table); r && r->rel == 0) table = r->target;
        if (Leaf* lf = As<Leaf>(table))
        {
            std::vector<std::pair<uint32_t, uint32_t>> out;
            for (size_t i = 0; i + 8 <= lf->raw.size(); i += 8)
                out.push_back({ uint32_t(Load(lf->raw.data() + i, 4, lf->big)), uint32_t(Load(lf->raw.data() + i + 4, 4, lf->big)) });
            return out;
        }
        return std::nullopt;
    }

    Node* WithCull(Node* table, uint32_t cull)
    {
        Leaf* lf = As<Leaf>(table);
        if (!lf) throw Error("a render state table that is not plain data");
        std::vector<uint8_t> raw = lf->raw;
        for (size_t i = 0; i + 8 <= raw.size(); i += 8)
        {
            const uint32_t w0 = uint32_t(Load(raw.data() + i, 4, lf->big));
            Store(raw.data() + i, 4, (w0 & ~0xC000u) | (cull << 14), lf->big);
        }
        return A().Make<Leaf>(lf->t, lf->n, std::move(raw), lf->big);
    }

    std::string Lower(std::string s)
    {
        for (char& c : s) c = char(std::tolower(static_cast<unsigned char>(c)));
        return s;
    }

    std::string WithoutCommas(std::string name)
    {
        name.erase(0, name.find_first_not_of(','));
        return name;
    }

    std::string Joined(const std::vector<std::string>& v, size_t most, std::string_view more)
    {
        std::string s;
        for (size_t i = 0; i < std::min(most, v.size()); i++) s += (i ? ", " : "") + v[i];
        if (v.size() > most) s += more;
        return s;
    }
}

namespace mapport
{
    Holder HolderOf(const Struct* o, std::string_view member)
    {
        const Member* m = o->Find(member);
        if (!m || m->mods.empty()) return Holder::None;
        // T* x[n]: dimensions, then the pointer; T** x: a pointer (to pointers).
        const bool dims = std::any_of(m->mods.begin(), m->mods.end(), [](int x) { return x != zone::kPointer; });
        if (dims && m->mods.back() == zone::kPointer) return Holder::Array;
        return m->mods[0] == zone::kPointer ? Holder::One : Holder::None;
    }

    void SetHolder(Struct* o, std::string_view member, uint32_t value, size_t k)
    {
        const Member* m = o->Find(member);
        if (!m) return;
        if (o->type->kind == Type::Union)
        {
            if (m->offset + 4 * (k + 1) <= o->Size()) Store(o->Data() + m->offset + 4 * k, 4, value, o->big);
            return;
        }
        if (HolderOf(o, member) != Holder::None) o->SetPtr(member, value, k);
    }
}
