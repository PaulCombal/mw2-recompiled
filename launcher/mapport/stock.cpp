// What the map takes from the stock 360 files: copies of their assets made
// to stand alone, references to what the game has loaded, and the effects,
// models, animations and sounds the 360 can't run from the PC's.
#include "porter.h"

#include <algorithm>
#include <map>
#include <regex>

namespace mapport
{
    using zone::Member;
    using zone::Type;

    namespace
    {
        // Every pointer a node holds: a struct's children (each item of a
        // list), or a pointer list's items.
        template <class F>
        void EachPointer(Node* o, F&& f)
        {
            if (Struct* s = As<Struct>(o))
            {
                if (Children* ch = s->Ch())
                    for (auto& [k, c] : ch->items) EachHeld(c, f);
            }
            else if (PtrList* pl = As<PtrList>(o))
                for (Node* x : pl->items) EachHeld(x, f);
        }

        uint32_t Marker(const Node* n)
        {
            const Struct* s = As<Struct>(n);
            return s && s->slot ? kInsert : kFollowing;
        }
    }

    // ---- copies -------------------------------------------------------------

    // A copy of a stock 360 asset, safe to write into another file: pointers
    // back to things outside it are replaced by the things themselves.
    Struct* Porter::CopyIn(Struct* src)
    {
        Localize({ src });
        Struct* copy = src->Copy();
        copy->slot = nullptr;
        return copy;
    }

    // Makes stock assets (or asset list entries) stand alone, in place.
    void Porter::Localize(const std::vector<Node*>& roots)
    {
        std::unordered_set<const Node*> inside;
        auto take = [&](Node* n) {
            ForEach(n, [&](Node* x) { inside.insert(x); });
        };
        ForEach(roots, [&](Node* x) {
            inside.insert(x);
            // The slot a temp-block asset reserves counts as part of it.
            if (Struct* s = As<Struct>(x); s && s->slot) inside.insert(s->slot);
        });
        std::function<Node*(Ref*)> cp = [&](Ref* o) -> Node* {
            if (o->target && inside.count(o->target))
            {
                // A pointer into a pointer array of an element ("the same as that
                // one") is replaced by what it points at even inside: the writer
                // would take it for the element itself. So is an asset pointer
                // meaning "the asset that member holds", which this file's order
                // can put after the pointer.
                if (Node* v = PtrArrayValue(o)) return v;
                if (Struct* a = As<Struct>(AssetInSlot(o)); a && !a->slot) return a;
                return o;
            }
            if (Node* asset = AssetInSlot(o))
            {
                // A pointer to another struct's pointer to the asset (how zones
                // refer to an asset loaded earlier): the asset itself.
                if (Ref* r = As<Ref>(asset)) return cp(r);
                if (!inside.count(asset)) take(asset);
                return asset;
            }
            Array* list = As<Array>(o->target);
            const Type* t = o->t.t;
            if (list && t && t->Compound() && t->size)
            {
                const size_t i = size_t(o->rel / t->size);
                const int64_t off = o->rel % t->size;
                if (!off)
                {
                    // A later element of another array (stock files share the tail
                    // of one argument list with another): a copy of that tail.
                    Array* tail = A().Make<Array>();
                    tail->tail = true;
                    if (i < list->items.size()) tail->items.assign(list->items.begin() + ptrdiff_t(i), list->items.end());
                    for (Node* x : tail->items) take(x);
                    inside.insert(tail);
                    return tail;
                }
                // A pointer member of another array's element (stock aliases share
                // a sound file that way), or one pointer of an array of them: what
                // that pointer holds.
                Struct* el = i < list->items.size() ? As<Struct>(list->items[i]) : nullptr;
                const Member* m = nullptr;
                for (const Member& x : t->members)
                    if (!x.mods.empty() && x.mods.back() == zone::kPointer && int64_t(x.offset) <= off &&
                        off < int64_t(x.offset + x.size) && (off - x.offset) % 4 == 0)
                    {
                        m = &x;
                        break;
                    }
                if (m && el)
                {
                    std::vector<int32_t> idx;
                    int64_t flat = (off - m->offset) / 4;
                    for (size_t d = m->mods.size() - 1; d-- > 0;)
                    {
                        const int dim = m->mods[d];
                        if (dim == zone::kPointer) continue;
                        idx.insert(idx.begin(), int32_t(flat % dim));
                        flat /= dim;
                    }
                    Node* c = el->Child(m->name, idx);
                    if (!c && !idx.empty())
                    {
                        // (arrays of pointers are kept as one list under the member's name)
                        const size_t k = size_t((off - m->offset) / 4);
                        Node* all = el->Child(m->name);
                        if (PtrList* pl = As<PtrList>(all)) c = k < pl->items.size() ? pl->items[k] : nullptr;
                        else if (Array* a = As<Array>(all)) c = k < a->items.size() ? a->items[k] : nullptr;
                    }
                    if (Ref* r = As<Ref>(c)) return cp(r);
                    if (As<Struct>(c))
                    {
                        take(c);
                        return c;
                    }
                }
            }
            if (o->target && o->rel == 0)
            {
                inside.insert(o->target);
                take(o->target);
                return o->target;
            }
            throw PortError("a stock asset points outside itself");
        };
        ForEach(roots, [&](Node* n) {
            Struct* o = As<Struct>(n);
            Children* ch = o ? o->Ch() : nullptr;
            if (!ch) return;
            for (auto& [k, c] : ch->items)
            {
                if (Ref* r = As<Ref>(c))
                {
                    Node* made = cp(r);
                    if (made == c) continue;
                    c = made;
                    if (k.idx.empty() && HolderOf(o, k.member) != Holder::Array) SetHolder(o, k.member, Marker(made));
                    else if (!k.idx.empty()) SetHolder(o, k.member, Marker(made), size_t(k.idx[0]));
                }
                else if (PtrList* pl = As<PtrList>(c))
                    for (size_t i = 0; i < pl->items.size(); i++)
                    {
                        Ref* item = As<Ref>(pl->items[i]);
                        if (!item) continue;
                        Node* made = cp(item);
                        if (made == item) continue;
                        pl->items[i] = made;
                        if (pl->raw) (*pl->raw)[i] = Marker(made);
                        if (HolderOf(o, k.member) == Holder::Array) SetHolder(o, k.member, Marker(made), i);
                    }
            }
        });
    }

    // A copy of a stock asset with what it points at, left as it is.
    Struct* Porter::StockCopy(Struct* src)
    {
        Struct* made = CopyIn(src);
        ForEach(made, [&](Node* n) {
            if (As<Struct>(n)) done.insert(n);
        });
        return made;
    }

    // `d` becomes `made`, keeping its identity, slot and placement.
    void Porter::Replace(Struct* d, const Struct& made)
    {
        Slot* slot = d->slot;
        const bool forward = d->forward;
        d->Assign(made);
        if (slot) d->slot = slot;
        if (forward) d->forward = true;
        done.insert(d);
    }

    // A 360 asset that only names one the game has loaded (",name").
    Struct* Porter::Reference(const std::string& type, const std::string& name)
    {
        const zone::StructInfo* info = X.infos.at(type);
        Struct* d = Struct::Zero(info->ctype, true);
        Struct* holder = d;
        std::string_view member = "name";
        if (type == "Material") holder = d->Sub("info");
        else if (!d->Find("name")) member = "filename";   // (SndCurve)
        holder->SetPtr(member, kFollowing);
        holder->SetChild(member, A().Make<String>("," + name));
        d->asset = info->name;
        return d;
    }

    // Bone names (and an animation's notify names) are indexes into the
    // file's script strings: moved from the stock file's (`theirs`) to ours.
    // Left as they were they named whatever string had that number here.
    void Porter::RemapBones(const std::vector<Node*>& made, const PtrList* theirs)
    {
        if (!root.scriptStrings)
        {
            root.scriptStrings = A().Make<PtrList>();
            root.scriptStrings->items.push_back(nullptr);
        }
        std::vector<Node*>& ss = root.scriptStrings->items;
        std::map<std::optional<std::string>, size_t> where;
        for (size_t k = 0; k < ss.size(); k++)
        {
            const String* s = As<String>(ss[k]);
            where.insert_or_assign(s ? std::optional<std::string>(s->b) : std::nullopt, k);
        }
        auto ours = [&](uint64_t v) -> uint64_t {
            const String* s = theirs && v < theirs->items.size() ? As<String>(theirs->items[v]) : nullptr;
            const std::optional<std::string> b = s ? std::optional<std::string>(s->b) : std::nullopt;
            auto [it, added] = where.emplace(b, ss.size());
            if (added) ss.push_back(b ? A().Make<String>(*b) : nullptr);
            return it->second;
        };
        ForEach(made, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o) return;
            const char* key = o->IsAsset("XModel") ? "boneNames" : o->IsAsset("XAnimParts") ? "names" : nullptr;
            if (!key)
            {
                // XAnimNotifyInfo: {name (a script string), time}
                const Member* name = o->Find("name");
                if (o->Find("time") && name && name->mods.empty() && !name->type->Compound() && !remapped.count(o))
                {
                    remapped.insert(o);
                    o->Set("name", int64_t(ours(o->U("name"))));
                }
                return;
            }
            Node* c = o->Child(key);
            if (Ref* r = As<Ref>(c)) c = r->target;
            Leaf* lf = As<Leaf>(c);
            if (!lf || remapped.count(lf)) return;
            remapped.insert(lf);
            for (int64_t i = 0; i < lf->n; i++)
            {
                uint8_t* p = lf->raw.data() + 2 * i;
                Store(p, 2, ours(Load(p, 2, lf->big)), lf->big);
            }
        });
    }

    // ---- effects --------------------------------------------------------------

    // Effects a stock 360 file has under the same name come from there (with
    // their materials) instead of being converted: converted ones can draw far
    // stronger than on the PC (mp_backlot's dust filled the map with a yellow
    // haze). Scripts and other assets name effects, so the stock one goes in
    // under the name and the converted one is renamed out of the way, then
    // left out unless the map's other assets point into it.
    void Porter::StockEffects()
    {
        std::vector<Entry*> made;
        std::vector<std::string> names;
        std::vector<std::pair<Entry*, Entry*>> swapped;
        for (Entry* e : root.assets)
        {
            Struct* a = As<Struct>(e->asset);
            if (e->type != "fx" || !a) continue;
            const std::optional<std::string> name = Name(a);
            const auto* found = name ? stockFx.Find(*name) : nullptr;
            if (!found) continue;
            a->SetChild("name", A().Make<String>("~pc/" + *name));
            Struct* fx = CopyIn(found->asset);
            RemapBones({ fx }, found->file->tree.scriptStrings);
            done.insert(fx);
            made.push_back(A().Make<Entry>(e->type, fx));
            swapped.push_back({ e, made.back() });
            stockFxUsed.push_back(fx);
            names.push_back(*name);
        }
        if (!swapped.empty()) DropSwappedEffects(swapped);
        // First in the list: the shader sets they bring are the stock ones the
        // map's own materials of those sets point at, written before them.
        root.assets.insert(root.assets.begin(), made.begin(), made.end());
        if (!names.empty())
            log("  " + std::to_string(names.size()) + " effect" + (names.size() > 1 ? "s" : "") +
                " from the stock 360 files: " + Joined(names));
    }

    // The PC copies of the effects taken from stock go: each took one of the
    // game's 600 effect places next to the stock one. A pointer to one goes to
    // the stock copy; what it brought is written where it is pointed at next.
    // One the map's other assets point into stays (renamed).
    void Porter::DropSwappedEffects(std::vector<std::pair<Entry*, Entry*>> swapped)
    {
        const size_t count = swapped.size();
        std::unordered_map<const Node*, size_t> inside;   // node -> the swapped entry it is in
        for (size_t k = 0; k < swapped.size(); k++)
        {
            Node* asset = swapped[k].first->asset;
            ForEach(asset, [&](Node* o) {
                if (o == asset) return;
                inside[o] = k;
                if (Struct* s = As<Struct>(o); s && s->slot) inside[s->slot] = k;
            });
        }
        std::vector<bool> live(swapped.size(), true);
        while (true)
        {
            std::set<size_t> keep;
            for (Entry* e : root.assets)
            {
                bool isSwapped = false;
                for (size_t k = 0; k < swapped.size(); k++) isSwapped |= live[k] && swapped[k].first == e;
                if (isSwapped) continue;
                ForEach(e->asset, [&](Node* o) {
                    EachPointer(o, [&](Node* x) {
                        if (Ref* r = As<Ref>(x))
                            if (auto it = inside.find(r->target); it != inside.end() && live[it->second]) keep.insert(it->second);
                    });
                });
            }
            if (keep.empty()) break;
            for (size_t k : keep) live[k] = false;
        }
        std::vector<std::pair<Entry*, Entry*>> gone;
        for (size_t k = 0; k < swapped.size(); k++)
            if (live[k]) gone.push_back(swapped[k]);
        if (const size_t kept = count - gone.size())
            log("  " + std::to_string(kept) +
                " PC copies of stock effects kept (renamed ~pc/...): the map's other assets point into them");
        if (gone.empty()) return;
        std::unordered_map<const Node*, Entry*> byObj;
        for (auto [e, stock] : gone)
        {
            byObj[e] = stock;
            byObj[e->asset] = stock;
            if (Struct* a = As<Struct>(e->asset); a && a->slot) byObj[a->slot] = stock;
        }
        int moved = 0;
        ForEach(root.assets, [&](Node* n) {
            auto visit = [&](Node*& x, auto&& setHolder) {
                if (Ref* r = As<Ref>(x))
                {
                    auto it = byObj.find(r->target);
                    if (it == byObj.end() || (r->rel != 0 && r->rel != 4)) return;
                    r->target = it->second;
                    r->rel = 4;
                    r->t = {};
                    moved++;
                }
                else if (As<Struct>(x))
                {
                    auto it = byObj.find(x);
                    if (it == byObj.end()) return;
                    Ref* to = A().Make<Ref>(1);
                    to->target = it->second;
                    to->rel = 4;
                    x = to;
                    setHolder();
                    moved++;
                }
            };
            if (Struct* o = As<Struct>(n); o && o->Ch())
            {
                for (auto& [k, c] : o->Ch()->items)
                {
                    if (Array* a = As<Array>(c))
                        for (Node*& x : a->items) visit(x, [] {});
                    else if (PtrList* pl = As<PtrList>(c))
                        for (Node*& x : pl->items) visit(x, [] {});
                    else
                        visit(c, [&] {
                            if (HolderOf(o, k.member) == Holder::One || o->type->kind == Type::Union)
                                SetHolder(o, k.member, 1);
                        });
                }
            }
            else if (PtrList* pl = As<PtrList>(n))
                for (Node*& x : pl->items) visit(x, [] {});
        });
        std::erase_if(root.assets, [&](Entry* e) {
            return std::any_of(gone.begin(), gone.end(), [&](const auto& g) { return g.first == e; });
        });
        log("  " + std::to_string(gone.size()) + " PC copies of those effects left out (" + std::to_string(moved) +
            " pointers to them now name the stock copy)");
    }

    // ---- models ---------------------------------------------------------------

    // Placed models a stock 360 file also has are drawn with the stock copy:
    // it goes in first, and each placed copy points at it. The converted
    // model is renamed and left out unless other assets point into it; a
    // model anything but placed copies holds stays as it is.
    void Porter::StockModelSwap()
    {
        std::unordered_set<const Node*> top;
        for (Entry* e : root.assets)
            if (As<Struct>(e->asset)) top.insert(e->asset);
        std::unordered_map<const Node*, std::set<bool>> holders;
        ForEach(root.assets, [&](Node* n) {
            Struct* o = As<Struct>(n);
            if (!o || !o->Ch()) return;
            const bool placement = o->Find("placement") != nullptr;
            for (auto& [k, c] : o->Ch()->items)
                EachHeld(c, [&](Node* x) {
                    Struct* t = As<Struct>(x) ? As<Struct>(x) : As<Ref>(x) ? DerefStruct(x) : nullptr;
                    if (t && t->IsAsset("XModel")) holders[t].insert(placement);
                });
        });
        std::vector<Entry*> front, moved;
        std::vector<Struct*> swappedModels;
        size_t kinds = 0;
        for (Node* n : Collect(root.assets))
        {
            Struct* o = As<Struct>(n);
            if (!o || !o->IsAsset("XModel") || done.count(o)) continue;
            const std::optional<std::string> name = Name(o);
            if (!name || name->starts_with(",")) continue;
            const auto* found = stockModels.Find(*name);
            if (!found) continue;
            auto h = holders.find(o);
            if (h == holders.end() || h->second != std::set<bool>{ true }) continue;   // held by more than placed copies
            Struct* m = CopyIn(found->asset);
            RemapBones({ m }, found->file->tree.scriptStrings);
            done.insert(m);
            Entry* entry = A().Make<Entry>("xmodel", m);
            front.push_back(entry);
            modelSwap[o] = entry;
            o->SetChild("name", A().Make<String>("~pc/" + *name));
            if (!top.count(o)) moved.push_back(A().Make<Entry>("xmodel", o));
            swappedModels.push_back(o);
            kinds++;
        }
        if (!swappedModels.empty())
        {
            // The converted copy goes too when nothing but placed copies points
            // at it: each took one of the game's 1,536 model places.
            std::unordered_map<const Node*, size_t> inside;
            std::unordered_set<const Node*> own;
            for (size_t k = 0; k < swappedModels.size(); k++)
                ForEach(swappedModels[k], [&](Node* x) {
                    own.insert(x);
                    if (x == swappedModels[k]) return;
                    inside[x] = k;
                    if (Struct* s = As<Struct>(x); s && s->slot) inside[s->slot] = k;
                });
            std::set<size_t> keep;
            ForEach(root.assets, [&](Node* x) {
                if (own.count(x)) return;
                EachPointer(x, [&](Node* y) {
                    if (Ref* r = As<Ref>(y))
                        if (auto it = inside.find(r->target); it != inside.end()) keep.insert(it->second);
                });
            });
            std::unordered_set<const Node*> gone;
            for (size_t k = 0; k < swappedModels.size(); k++)
                if (!keep.count(k)) gone.insert(swappedModels[k]);
            std::erase_if(moved, [&](Entry* e) { return gone.count(e->asset) != 0; });
            std::erase_if(root.assets, [&](Entry* e) { return As<Struct>(e->asset) && gone.count(e->asset); });
            log("  " + std::to_string(gone.size()) + " converted copies of those models left out (" +
                std::to_string(keep.size()) + " kept: other assets point into them)");
        }
        front.insert(front.end(), moved.begin(), moved.end());
        root.assets.insert(root.assets.begin(), front.begin(), front.end());
        log("  " + std::to_string(kinds) + " kinds of static model drawn with the stock 360 copy");
    }

    // ---- animations -----------------------------------------------------------

    // The 360 packs animation rotations otherwise (a quaternion in 32 bits) and
    // has two more part types, so PC animations can't be carried over: those a
    // stock 360 file has come from it, the rest are left out (scripts and map
    // entities name animations; nothing points at them).
    void Porter::StockAnims()
    {
        std::vector<std::string> took, dropped;
        std::vector<Entry*> keep;
        for (Entry* e : root.assets)
        {
            Struct* a = As<Struct>(e->asset);
            if (e->type != "xanim" || !a)
            {
                keep.push_back(e);
                continue;
            }
            const std::string name = Name(a).value_or("");
            if (name.starts_with(","))
            {
                keep.push_back(e);
                continue;
            }
            if (!stockAnims)
            {
                // By resolved name: stock files name animations through a shared string.
                stockAnims.emplace();
                for (StockFile* f : refs)
                    ForEach(f->tree.assets, [&](Node* n) {
                        Struct* o = As<Struct>(n);
                        if (!o || !o->IsAsset("XAnimParts")) return;
                        const std::string nm = Name(o).value_or("");
                        if (nm.empty() || nm.starts_with(",") || (stockAnims->first.Has(nm) && f->resident)) return;
                        stockAnims->first.FindOrAdd(nm, o);
                        stockAnims->second.emplace(o, f);
                    });
            }
            Struct** src = stockAnims->first.Find(name);
            if (!src)
            {
                dropped.push_back(name);
                continue;
            }
            Struct* made = CopyIn(*src);
            RemapBones({ made }, stockAnims->second.at(*src)->tree.scriptStrings);
            Replace(a, *made);
            ForEach(a, [&](Node* n) {
                if (As<Struct>(n)) done.insert(n);
            });
            keep.push_back(e);
            took.push_back(name);
        }
        root.assets = std::move(keep);
        if (!took.empty()) log("  " + std::to_string(took.size()) + " animations from the stock 360 files");
        if (!dropped.empty())
            Warn(std::to_string(dropped.size()) + " animations left out (no stock 360 copy): " +
                 Joined(dropped, 8, " ..."));
    }

    // ---- breakable parts --------------------------------------------------------

    // The models the map's destructible entities need: each copied in after the
    // map's own assets under the name the 360's script asks for, a part taken
    // in another colour painted with the map's material for its own colour, a
    // material the map has under the same name taken from the map. Copies of
    // one stock model share its materials and geometry.
    void Porter::AddBreakables()
    {
        Ordered<std::string, Struct*> ours;
        for (Entry* e : root.assets)
            ForEach(e->asset, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset("Material")) return;
                const std::optional<std::string> name = AssetName(o);
                if (name && !name->starts_with(",")) ours.FindOrAdd(Lower(*name), o);
            });
        std::vector<Entry*> made;
        int painted = 0;
        for (const Breakable& b : breakables)
        {
            StockFile* file = nullptr;
            for (StockFile* f : refs)
                if (f->path == b.path) file = f;
            Struct* hit = nullptr;
            if (file)
                for (Entry* e : file->tree.assets)
                    if (Struct* a = As<Struct>(e->asset); a && e->type == "xmodel" && Lower(Name(a).value_or("")) == b.src)
                    {
                        hit = a;
                        break;
                    }
            if (!hit) continue;
            Struct* base = CopyIn(hit);
            // A material named through another model's list ("the same as its
            // second") becomes the material itself, so the copy stands alone.
            if (PtrList* ml = As<PtrList>(base->Child("materialHandles")))
                for (Node*& x : ml->items)
                {
                    Node* v = x;
                    while (Ref* r = As<Ref>(v))
                    {
                        if (!As<PtrList>(r->target)) break;
                        v = AssetInSlot(r);
                    }
                    if (As<Struct>(v)) x = v;
                }
            std::unordered_map<const Node*, Node*> shared;
            ForEach(base, [&](Node* n) {
                Struct* o = As<Struct>(n);
                if (!o || !o->IsAsset() || o->IsAsset("XModel")) return;
                ForEach(o, [&](Node* x) {
                    shared[x] = x;
                    if (Struct* s = As<Struct>(x); s && s->slot) shared[s->slot] = s->slot;
                });
            });
            Struct* m = DeepCopy(base, &shared);
            m->slot = nullptr;
            RemapBones({ m }, file->tree.scriptStrings);
            SetName(m, b.need);
            painted += PaintBreakable(m, b.swap, ours);
            done.insert(m);
            made.push_back(A().Make<Entry>("xmodel", m));
        }
        size_t at = root.assets.size();
        for (size_t i = 0; i < root.assets.size(); i++)
            if (root.assets[i]->type == "gfx_map")
            {
                at = i;
                break;
            }
        root.assets.insert(root.assets.begin() + ptrdiff_t(at), made.begin(), made.end());
        if (!made.empty())
            log("  " + std::to_string(made.size()) + " breakable parts copied in (" + std::to_string(painted) +
                " paint materials the map's own)");
    }

    // Points a model's materials at the map's own: the one in the map's colour
    // (swap: stock colour, map colour), else the same-named one. Returns how
    // many changed.
    int Porter::PaintBreakable(Struct* model, const std::optional<ColourSwap>& swap,
                               const Ordered<std::string, Struct*>& ours)
    {
        Node* c = model->Child("materialHandles");
        if (Ref* r = As<Ref>(c)) c = r->target;
        PtrList* ml = As<PtrList>(c);
        if (!ml) return 0;
        int count = 0;
        for (Node*& x : ml->items)
        {
            Node* m = x;
            if (Ref* r = As<Ref>(m))
            {
                m = PtrArrayValue(r);
                if (!m) m = As<Entry>(r->target) ? r->target : AssetInSlot(r);
                if (Entry* e = As<Entry>(m)) m = e->asset;
            }
            Struct* ms = As<Struct>(m);
            const std::string n = ms ? Lower(AssetName(ms).value_or("")) : "";
            if (n.empty()) continue;
            Struct* const* mine = nullptr;
            if (swap)
            {
                const std::regex color("_" + swap->stock + "(?=_|$)");
                mine = ours.Find(std::regex_replace(n, color, "_" + swap->map));
            }
            if (!mine) mine = ours.Find(n);
            if (mine && *mine != ms)
            {
                x = *mine;
                count++;
            }
        }
        return count;
    }
}
