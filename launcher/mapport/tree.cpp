#include "tree.h"

#include <cstdlib>
#include <cstring>
#include <unordered_set>

namespace mapport
{
    using zone::Member;
    using zone::Type;

    namespace
    {
        thread_local Arena* tArena = nullptr;

    }

    // ---- numbers ------------------------------------------------------------

    uint64_t Load(const uint8_t* p, uint32_t size, bool big)
    {
        uint64_t v = 0;
        if (big)
            for (uint32_t k = 0; k < size; k++) v = (v << 8) | p[k];
        else
            for (uint32_t k = size; k-- > 0;) v = (v << 8) | p[k];
        return v;
    }

    void Store(uint8_t* p, uint32_t size, uint64_t v, bool big)
    {
        if (big)
            for (uint32_t k = size; k-- > 0; v >>= 8) p[k] = uint8_t(v);
        else
            for (uint32_t k = 0; k < size; k++, v >>= 8) p[k] = uint8_t(v);
    }

    float LoadF32(const uint8_t* p, bool big)
    {
        const uint32_t bits = uint32_t(Load(p, 4, big));
        float f;
        std::memcpy(&f, &bits, 4);
        return f;
    }

    void StoreF32(uint8_t* p, float v, bool big)
    {
        uint32_t bits;
        std::memcpy(&bits, &v, 4);
        Store(p, 4, bits, big);
    }

    // ---- children -----------------------------------------------------------

    Node* Children::Get(std::string_view member, const std::vector<int32_t>& idx) const
    {
        for (const auto& [k, n] : items)
            if (k.member == member && k.idx == idx) return n;
        return nullptr;
    }

    void Children::Set(std::string_view member, Node* n, const std::vector<int32_t>& idx)
    {
        for (auto& [k, v] : items)
            if (k.member == member && k.idx == idx)
            {
                v = n;
                return;
            }
        items.push_back({ { member, idx }, n });
    }

    Node* Children::Pop(std::string_view member, const std::vector<int32_t>& idx)
    {
        for (size_t i = 0; i < items.size(); i++)
            if (items[i].first.member == member && items[i].first.idx == idx)
            {
                Node* n = items[i].second;
                items.erase(items.begin() + ptrdiff_t(i));
                return n;
            }
        return nullptr;
    }

    // ---- structs ------------------------------------------------------------

    Struct::Struct(const Type* type, bool big, std::vector<uint8_t> bytes)
        : Node(kKind), type(type->Unwrapped()), big(big), bytes(std::move(bytes))
    {
        size = this->bytes.size();
    }

    Struct::Struct(Struct* owner, const Type* type, size_t at, size_t size)
        : Node(kKind), type(type->Unwrapped()), big(owner->big), owner(owner), at(at), size(size)
    {
    }

    uint8_t* Struct::Data() { return owner ? owner->Data() + at : bytes.data(); }
    const uint8_t* Struct::Data() const { return owner ? owner->Data() + at : bytes.data(); }

    const Member* Struct::Find(std::string_view member) const
    {
        for (const Member& m : type->members)
            if (m.name == member) return &m;
        // An unnamed member, by its place: "_<index>".
        if (member.size() > 1 && member[0] == '_')
        {
            const size_t k = size_t(std::atoi(std::string(member.substr(1)).c_str()));
            if (k < type->members.size() && type->members[k].name.empty()) return &type->members[k];
        }
        return nullptr;
    }

    const Member& Struct::M(std::string_view member) const
    {
        const Member* m = Find(member);
        if (!m) throw Error(type->name + " has no member " + std::string(member));
        return *m;
    }

    bool Struct::Has(std::string_view member) const
    {
        const Member* m = Find(member);
        if (!m) return false;
        return m->offset + m->size <= size || Late(member);
    }

    uint32_t Struct::ElemSize(const Member& m) const
    {
        for (int x : m.mods)
            if (x == zone::kPointer) return 4;
        return m.type->size;
    }

    uint8_t* Struct::MemberData(const Member& m, size_t k)
    {
        if (k && (k + 1) * ElemSize(m) > m.size) throw Error(type->name + "::" + m.name + " has no element " + std::to_string(k));
        const size_t off = m.offset + k * ElemSize(m);
        if (off >= size && !(off == size && m.size == 0)) throw Error(type->name + "::" + m.name + " is past its bytes");
        return Data() + off;
    }

    const uint8_t* Struct::MemberData(const Member& m, size_t k) const
    {
        return const_cast<Struct*>(this)->MemberData(m, k);
    }

    int64_t Struct::Int(std::string_view member, size_t k) const
    {
        const Member& m = M(member);
        if (m.bits >= 0)
        {
            const uint64_t unit = Load(MemberData(m, 0), m.size, big);
            return int64_t((unit >> m.bitpos) & ((uint64_t(1) << m.bits) - 1));
        }
        const Type* t = m.type;
        const uint64_t v = Load(MemberData(m, k), t->size, big);
        if (t->isSigned && t->size < 8 && (v >> (t->size * 8 - 1)) & 1) return int64_t(v | (~uint64_t(0) << (t->size * 8)));
        return int64_t(v);
    }

    uint64_t Struct::U(std::string_view member, size_t k) const { return uint64_t(Int(member, k)); }

    double Struct::F(std::string_view member, size_t k) const
    {
        const Member& m = M(member);
        if (m.type->size == 8)
        {
            const uint64_t bits = Load(MemberData(m, k), 8, big);
            double d;
            std::memcpy(&d, &bits, 8);
            return d;
        }
        return double(LoadF32(MemberData(m, k), big));
    }

    void Struct::Set(std::string_view member, int64_t v, size_t k)
    {
        const Member& m = M(member);
        if (m.bits >= 0)
        {
            uint8_t* p = MemberData(m, 0);
            uint64_t unit = Load(p, m.size, big);
            const uint64_t mask = ((uint64_t(1) << m.bits) - 1) << m.bitpos;
            unit = (unit & ~mask) | ((uint64_t(v) << m.bitpos) & mask);
            Store(p, m.size, unit, big);
            return;
        }
        Store(MemberData(m, k), ElemSize(m), uint64_t(v), big);
    }

    void Struct::SetF(std::string_view member, double v, size_t k)
    {
        const Member& m = M(member);
        if (m.type->size == 8)
        {
            uint64_t bits;
            std::memcpy(&bits, &v, 8);
            Store(MemberData(m, k), 8, bits, big);
        }
        else StoreF32(MemberData(m, k), float(v), big);
    }

    uint32_t Struct::Ptr(std::string_view member, size_t k) const
    {
        return uint32_t(Load(MemberData(M(member), k), 4, big));
    }

    void Struct::SetPtr(std::string_view member, uint32_t v, size_t k)
    {
        Store(MemberData(M(member), k), 4, v, big);
    }

    std::span<uint8_t> Struct::Bytes(std::string_view member)
    {
        const Member& m = M(member);
        return { MemberData(m, 0), m.size };
    }

    std::span<const uint8_t> Struct::Bytes(std::string_view member) const
    {
        const Member& m = M(member);
        return { MemberData(m, 0), m.size };
    }

    Struct* Struct::Sub(std::string_view member, int64_t k) { return Sub(M(member), k); }

    Struct* Struct::Sub(const Member& m, int64_t k)
    {
        if (Node* late = Late(m.name, k >= 0 ? std::vector<int32_t>{ int32_t(k) } : std::vector<int32_t>{}))
            if (Struct* s = As<Struct>(late)) return s;
        for (const View& v : views)
            if (v.m == &m && v.k == k) return v.s;
        const Type* t = m.type->Unwrapped();
        if ((t->kind != Type::Struct && t->kind != Type::Union) || m.IsPointer())
            throw Error(type->name + "::" + m.name + " is not an embedded struct");
        const size_t off = m.offset + (k > 0 ? size_t(k) * m.type->size : 0);
        Struct* top = owner ? owner : this;
        Struct* s = A().Make<Struct>(top, t, at + off, t->size);
        views.push_back({ &m, k, s });
        return s;
    }

    Node* Struct::Late(std::string_view member, const std::vector<int32_t>& idx) const
    {
        for (const LateValue& l : lates)
            if (l.key.member == member && l.key.idx == idx) return l.n;
        return nullptr;
    }

    void Struct::SetLate(std::string_view member, Node* n, const std::vector<int32_t>& idx)
    {
        for (LateValue& l : lates)
            if (l.key.member == member && l.key.idx == idx)
            {
                l.n = n;
                return;
            }
        lates.push_back({ { member, idx }, n });
    }

    Children& Struct::ChMake()
    {
        if (!ch) ch = A().MakeChildren();
        return *ch;
    }

    Node* Struct::Child(std::string_view member, const std::vector<int32_t>& idx) const
    {
        return ch ? ch->Get(member, idx) : nullptr;
    }

    void Struct::SetChild(std::string_view member, Node* n, const std::vector<int32_t>& idx)
    {
        ChMake().Set(member, n, idx);
    }

    Node* Struct::PopChild(std::string_view member, const std::vector<int32_t>& idx)
    {
        return ch ? ch->Pop(member, idx) : nullptr;
    }

    namespace
    {
        // New views of `src`'s embedded members on `dst`, sharing their pointers.
        void CopyViews(const Struct& src, Struct& dst)
        {
            for (const Struct::View& v : src.Views())
            {
                Struct* mine = dst.Sub(*v.m, v.k);
                mine->SetCh(v.s->Ch());
                mine->used = v.s->used;
                CopyViews(*v.s, *mine);
            }
        }
    }

    void Struct::Assign(const Struct& src)
    {
        type = src.type;
        big = src.big;
        bytes.assign(src.Data(), src.Data() + src.Size());
        owner = nullptr;
        at = 0;
        size = src.Size();
        ch = src.ch;
        views.clear();
        lates = src.lates;
        used = src.used;
        asset = src.asset;
        slot = src.slot;
        forward = src.forward;
        pak = src.pak;
        CopyViews(src, *this);
    }

    void Struct::Adopt(Struct& made)
    {
        type = made.type;
        big = made.big;
        bytes = std::move(made.bytes);
        size = bytes.size();
        ch = made.ch;
        lates = made.lates;
        used = made.used;
        // The views made of `made`'s members: an old view of the same member
        // (by name) carries on as it, the rest become this struct's. (Unnamed
        // members are told apart only by their element: of the types with two,
        // vec4_t's are plain floats, never viewed.)
        std::vector<View> old = std::move(views);
        views.clear();
        std::function<void(Struct&)> reown = [&](Struct& s) {
            for (View& w : s.views)
            {
                w.s->owner = this;
                reown(*w.s);
            }
        };
        std::function<void(Struct&, Struct&, std::vector<View>&)> rebind = [&](Struct& parent, Struct& from,
                                                                              std::vector<View>& previous) {
            for (const View& v : from.views)
            {
                Struct* keep = nullptr;
                for (View& o : previous)
                    if (o.s && o.m->name == v.m->name && o.k == v.k)
                    {
                        keep = o.s;
                        o.s = nullptr;
                        break;
                    }
                Struct* s = keep ? keep : v.s;
                std::vector<View> inner = keep ? std::move(keep->views) : std::vector<View>();
                s->type = v.s->type;
                s->big = v.s->big;
                s->owner = this;
                s->at = v.s->at;
                s->size = v.s->size;
                s->ch = v.s->ch;
                s->lates = v.s->lates;
                s->used = v.s->used;
                if (keep)
                {
                    keep->views.clear();
                    rebind(*keep, *v.s, inner);
                }
                else reown(*s);
                parent.views.push_back({ v.m, v.k, s });
            }
        };
        rebind(*this, made, old);
    }

    Struct* Struct::Zero(const Type* type, bool big)
    {
        return A().Make<Struct>(type, big, std::vector<uint8_t>(type->Unwrapped()->size, 0));
    }

    Struct* Struct::Copy() const
    {
        Struct* s = A().Make<Struct>(type, big, std::vector<uint8_t>(Data(), Data() + Size()));
        s->Assign(*this);
        return s;
    }

    // ---- the arena ----------------------------------------------------------

    Arena& A()
    {
        if (!tArena) throw Error("a tree node made outside a conversion");
        return *tArena;
    }

    ArenaScope::ArenaScope(Arena& a) : prev(tArena) { tArena = &a; }
    ArenaScope::~ArenaScope() { tArena = prev; }

    // ---- walking a tree -----------------------------------------------------

    namespace
    {
        // What the walk pushes: a node, or an embedded array of structs, whose
        // elements are pushed when it comes off the stack, as a list's items
        // are.
        struct Item
        {
            Node* n = nullptr;
            Struct* owner = nullptr;
            const Member* m = nullptr;
        };

        bool Walked(const Node* n)
        {
            return n && n->kind != Node::Kind::Ref && n->kind != Node::Kind::String;
        }

        void PushStruct(Struct* s, std::vector<Item>& stack)
        {
            auto pushLates = [&] {
                for (const Struct::LateValue& l : s->Lates())
                    if (Walked(l.n)) stack.push_back({ l.n });
            };
            // Its members in order (embedded structs and unions, embedded
            // arrays of them, and what was streamed after its first part),
            // then its pointers.
            for (const Member& m : s->type->members)
            {
                if (m.HasPointer()) continue;
                if (s->Late(m.name))
                {
                    if (!s->latesAfterPointers)
                        for (const Struct::LateValue& l : s->Lates())
                            if (l.key.member == m.name && Walked(l.n)) stack.push_back({ l.n });
                    continue;
                }
                const Type* t = m.type->Unwrapped();
                if ((t->kind != Type::Struct && t->kind != Type::Union) || m.offset + m.size > s->Size()) continue;
                if (m.mods.empty()) stack.push_back({ s->Sub(m) });
                else stack.push_back({ nullptr, s, &m });
            }
            if (Children* ch = s->Ch())
                for (const auto& [k, n] : ch->items)
                    if (Walked(n)) stack.push_back({ n });
            if (s->latesAfterPointers) pushLates();
        }

        void Walk(std::vector<Item> stack, const std::function<void(Node*)>& f)
        {
            std::unordered_set<const Node*> seen;
            while (!stack.empty())
            {
                const Item it = stack.back();
                stack.pop_back();
                if (!it.n)
                {
                    size_t count = 1;
                    for (int x : it.m->mods) count *= size_t(x);
                    for (size_t k = 0; k < count; k++) stack.push_back({ it.owner->Sub(*it.m, int64_t(k)) });
                    continue;
                }
                if (!seen.insert(it.n).second) continue;
                f(it.n);
                switch (it.n->kind)
                {
                case Node::Kind::Struct:
                    PushStruct(static_cast<Struct*>(it.n), stack);
                    break;
                case Node::Kind::Array:
                    for (Node* x : static_cast<Array*>(it.n)->items)
                        if (Walked(x)) stack.push_back({ x });
                    break;
                case Node::Kind::PtrList:
                    for (Node* x : static_cast<PtrList*>(it.n)->items)
                        if (Walked(x)) stack.push_back({ x });
                    break;
                case Node::Kind::Entry:
                    if (Walked(static_cast<Entry*>(it.n)->asset)) stack.push_back({ static_cast<Entry*>(it.n)->asset });
                    break;
                default:
                    break;   // a Leaf or a Slot: nothing inside
                }
            }
        }
    }

    void ForEach(Node* root, const std::function<void(Node*)>& f)
    {
        std::vector<Item> stack;
        if (Walked(root)) stack.push_back({ root });
        Walk(std::move(stack), f);
    }

    void ForEach(const std::vector<Entry*>& entries, const std::function<void(Node*)>& f)
    {
        std::vector<Item> stack;
        for (Entry* e : entries) stack.push_back({ e });
        Walk(std::move(stack), f);
    }

    void ForEach(const std::vector<Node*>& roots, const std::function<void(Node*)>& f)
    {
        std::vector<Item> stack;
        for (Node* n : roots)
            if (Walked(n)) stack.push_back({ n });
        Walk(std::move(stack), f);
    }

    std::vector<Node*> Collect(const std::vector<Node*>& roots)
    {
        std::vector<Node*> out;
        ForEach(roots, [&](Node* n) { out.push_back(n); });
        return out;
    }

    std::vector<Node*> Collect(Node* root)
    {
        std::vector<Node*> out;
        ForEach(root, [&](Node* n) { out.push_back(n); });
        return out;
    }

    std::vector<Node*> Collect(const std::vector<Entry*>& entries)
    {
        std::vector<Node*> out;
        ForEach(entries, [&](Node* n) { out.push_back(n); });
        return out;
    }
}

namespace mapport
{
    namespace
    {
        class Copier
        {
        public:
            explicit Copier(std::unordered_map<const Node*, Node*>& memo) : memo(memo) {}

            Node* Copy(Node* n)
            {
                if (!n) return nullptr;
                if (auto it = memo.find(n); it != memo.end()) return it->second;
                switch (n->kind)
                {
                case Node::Kind::Struct:
                    return CopyStruct(static_cast<Struct*>(n));
                case Node::Kind::Array:
                {
                    const Array* a = static_cast<Array*>(n);
                    Array* c = A().Make<Array>();
                    memo[n] = c;
                    c->tail = a->tail;
                    for (Node* x : a->items) c->items.push_back(Copy(x));
                    return c;
                }
                case Node::Kind::Leaf:
                {
                    const Leaf* l = static_cast<Leaf*>(n);
                    return memo[n] = A().Make<Leaf>(l->t, l->n, l->raw, l->big);
                }
                case Node::Kind::String:
                    return memo[n] = A().Make<String>(static_cast<String*>(n)->b);
                case Node::Kind::PtrList:
                {
                    const PtrList* p = static_cast<PtrList*>(n);
                    PtrList* c = A().Make<PtrList>();
                    memo[n] = c;
                    c->raw = p->raw;
                    for (Node* x : p->items) c->items.push_back(Copy(x));
                    return c;
                }
                case Node::Kind::Ref:
                {
                    const Ref* r = static_cast<Ref*>(n);
                    Ref* c = A().Make<Ref>(r->val);
                    memo[n] = c;
                    c->rel = r->rel;
                    c->t = r->t;
                    c->target = Copy(r->target);
                    return c;
                }
                case Node::Kind::Slot:
                {
                    Slot* c = A().Make<Slot>(nullptr);
                    memo[n] = c;
                    c->asset = Copy(static_cast<Slot*>(n)->asset);
                    return c;
                }
                case Node::Kind::Entry:
                {
                    const Entry* e = static_cast<Entry*>(n);
                    Entry* c = A().Make<Entry>(e->type, nullptr);
                    memo[n] = c;
                    c->asset = Copy(e->asset);
                    return c;
                }
                }
                return n;
            }

        private:
            // A struct and its views are copied together; a view reached
            // first is copied with its owner.
            Node* CopyStruct(Struct* s)
            {
                if (Struct* owner = s->Owner())
                {
                    Copy(owner);
                    if (auto it = memo.find(s); it != memo.end()) return it->second;
                    throw Error("a view of " + owner->type->name + " its owner doesn't list");
                }
                Struct* c = A().Make<Struct>(s->type, s->big, std::vector<uint8_t>(s->Data(), s->Data() + s->Size()));
                memo[s] = c;
                std::vector<std::pair<Struct*, Struct*>> pairs{ { s, c } };
                for (size_t i = 0; i < pairs.size(); i++)
                    for (const Struct::View& v : pairs[i].first->Views())
                    {
                        Struct* mine = pairs[i].second->Sub(*v.m, v.k);
                        memo[v.s] = mine;
                        pairs.push_back({ v.s, mine });
                    }
                for (auto [from, to] : pairs)
                {
                    to->asset = from->asset;
                    to->forward = from->forward;
                    to->pak = from->pak;
                    to->used = from->used;
                    to->latesAfterPointers = from->latesAfterPointers;
                    to->slot = from->slot ? static_cast<Slot*>(Copy(from->slot)) : nullptr;
                    if (const Children* ch = from->Ch())
                    {
                        Children* mine = A().MakeChildren();
                        for (const auto& [k, x] : ch->items) mine->items.push_back({ k, Copy(x) });
                        to->SetCh(mine);
                    }
                    for (const Struct::LateValue& l : from->Lates()) to->SetLate(l.key.member, Copy(l.n), l.key.idx);
                }
                return c;
            }

            std::unordered_map<const Node*, Node*>& memo;
        };
    }

    Node* DeepCopy(Node* root, std::unordered_map<const Node*, Node*>* memo)
    {
        std::unordered_map<const Node*, Node*> own;
        return Copier(memo ? *memo : own).Copy(root);
    }

    Struct* DeepCopy(Struct* root, std::unordered_map<const Node*, Node*>* memo)
    {
        return static_cast<Struct*>(DeepCopy(static_cast<Node*>(root), memo));
    }
}
