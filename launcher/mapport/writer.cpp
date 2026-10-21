// TreeWriter: a tree streamed out as the zone the loader would read it from
// (reader.h).
#include "reader.h"
#include "query.h"

#include <algorithm>
#include <cstring>

namespace mapport
{
    using zone::Member;
    using zone::MemberInfo;
    using zone::StructInfo;
    using zone::Type;

    namespace
    {
    }

    // (The zone written starts as its header's room; the Reader stands after it.)
    TreeWriter::TreeWriter(const Tree& tree, const zone::Schema& schema, const Platform& plat)
        : Reader(schema, plat, std::vector<uint8_t>(plat.HeaderBytes())), tree(tree)
    {
    }

    // ---- pointers -----------------------------------------------------------

    // Where an alias's target is now. A target written after the pointer
    // (a struct's pointers stream before what they point at) keeps its value
    // here and gets the real one in Alias, when the walk reaches the pointer.
    uint32_t TreeWriter::RefValue(const Ref* r, bool strict)
    {
        if (!r->target) throw Error("a pointer points at something that isn't in the file");
        auto at = loc.find(r->target);
        if (at == loc.end())
        {
            if (strict) throw Error("a pointer refers to something before it is written, at " + Path());
            return r->val;
        }
        const int64_t rel = mapRel ? mapRel(r) : r->rel;
        return ((uint32_t(at->second.block) << kBlockShift) | uint32_t(at->second.pos + rel)) + 1;
    }

    // What to write in place of alias r when its target isn't written yet (it
    // comes later in this file's order): the target itself, else null.
    Node* TreeWriter::Forward(Node* node, bool asset)
    {
        Ref* r = As<Ref>(node);
        if (!r || !r->target) return nullptr;
        if (asset)
        {
            if (Node* a = SlotAsset(r))
            {
                if (Ref* ar = As<Ref>(a)) return loc.count(ar->target) ? nullptr : Forward(ar, true);
                // A temp asset with its own slot is written where it sits, unless
                // nothing holds it any more but pointers ("forward": its first
                // pointer takes it).
                Struct* s = As<Struct>(a);
                return !loc.count(a) && (!s || !s->slot || s->forward) ? a : nullptr;
            }
        }
        if (loc.count(r->target) || r->rel) return nullptr;
        if (Slot* slotted = As<Slot>(r->target))
        {
            // A member pointing at another asset's slot (a model's surfaces):
            // that asset, if nothing but pointers hold it.
            Struct* a = As<Struct>(slotted->asset);
            return a && a->forward && !loc.count(a) ? a : nullptr;
        }
        return r->target;
    }

    std::optional<Where> TreeWriter::Address(const Buf& buf, size_t at) const
    {
        const int64_t zp = buf.ZoneAt(at);
        if (zp < 0) return std::nullopt;
        auto it = std::upper_bound(zonePositions.begin(), zonePositions.end(), size_t(zp));
        if (it == zonePositions.begin()) return std::nullopt;
        const size_t i = size_t(it - zonePositions.begin()) - 1;
        const auto& span = zoneSpans[i];
        if (size_t(zp) >= zonePositions[i] + span[2]) return std::nullopt;
        return Where{ int(span[0]), uint32_t(span[1] + size_t(zp) - zonePositions[i]) };
    }

    void TreeWriter::Patch(Buf& buf, size_t at, uint32_t val)
    {
        Store(buf.b.data() + at, 4, val, big);
        const int64_t zp = buf.ZoneAt(at);
        if (zp >= 0) Store(zone.data() + zp, 4, val, big);
    }

    void TreeWriter::Reuse(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t at,
                           std::optional<uint32_t> val)
    {
        if (obj && As<Ref>(*obj) && !mi->isString)
            if (Node* t = Forward(*obj, IsAsset(mi)))
            {
                const Struct* s = As<Struct>(t);
                val = InTemp(mi) && s && s->slot ? kInsert : kFollowing;
                Patch(*inst->buf, at, *val);
                obj = t;
                return AllocMember(inst, mi, modPos, combined, kind, at, val);
            }
        Reader::Reuse(inst, mi, modPos, combined, kind, at, val);
    }

    void TreeWriter::Alias(Buf& buf, size_t at, uint32_t)
    {
        if (!obj) return;
        if (const Ref* r = As<Ref>(*obj))
        {
            const uint32_t val = RefValue(r, true);
            const int64_t zp = buf.ZoneAt(at);
            if (zp >= 0) Store(zone.data() + zp, 4, val, big);
        }
    }

    // ---- encoding -----------------------------------------------------------

    bool TreeWriter::HasPointers(const Type* t)
    {
        if (!t->Compound()) return false;
        auto found = pointerTypes.find(t);
        if (found != pointerTypes.end()) return found->second;
        pointerTypes[t] = false;
        bool any = false;
        for (const Member& m : t->members)
            if (m.HasPointer() || HasPointers(m.type)) any = true;
        pointerTypes[t] = any;
        return any;
    }

    // A struct's bytes as its members say them, its pointers back to earlier
    // data set to where their targets now are.
    void TreeWriter::EncodeStruct(Struct* s, uint8_t* out, size_t size, bool fix)
    {
        const Type* t = s->type;
        if (t->kind == Type::Union)
        {
            EncodeUnion(s, out);
            return;
        }
        const uint8_t* src = s->Data();
        fix = fix && HasPointers(t);
        Children* ch = fix ? s->Ch() : nullptr;
        if (keepBytes) std::memcpy(out, src, std::min(size, s->Size()));
        for (const Member& m : t->members)
        {
            if (m.offset + m.size > size) break;
            if (m.IsPointer())
            {
                uint32_t v = uint32_t(Load(src + m.offset, 4, big));
                if (ch)
                    if (const Ref* r = As<Ref>(ch->Get(m.name))) v = RefValue(r);
                Store(out + m.offset, 4, v, big);
                continue;
            }
            if (m.HasPointer())
            {
                // An array of pointers: each element's own pointer, or the
                // list's, when it is a Ref.
                const size_t n = m.Elements();
                Node* list = ch ? ch->Get(m.name) : nullptr;
                for (size_t k = 0; k < n; k++)
                {
                    uint32_t v = uint32_t(Load(src + m.offset + 4 * k, 4, big));
                    if (ch)
                    {
                        Node* c = ch->Get(m.name, { int32_t(k) });
                        if (!c)
                        {
                            if (const PtrList* pl = As<PtrList>(list)) c = k < pl->items.size() ? pl->items[k] : nullptr;
                            else if (const Array* a = As<Array>(list)) c = k < a->items.size() ? a->items[k] : nullptr;
                        }
                        if (const Ref* r = As<Ref>(c)) v = RefValue(r);
                    }
                    Store(out + m.offset + 4 * k, 4, v, big);
                }
                continue;
            }
            if (m.bits >= 0)
            {
                const uint64_t mask = ((uint64_t(1) << m.bits) - 1) << m.bitpos;
                const uint64_t v = Load(src + m.offset, m.size, big) & mask;
                Store(out + m.offset, m.size, (Load(out + m.offset, m.size, big) & ~mask) | v, big);
                continue;
            }
            const Type* mt = m.type->Unwrapped();
            if (mt->kind == Type::Struct || mt->kind == Type::Union)
            {
                // A typedef around a struct is written whole, as its bytes: its
                // pointers are not looked at.
                const bool wrapped = m.type->kind == Type::Typedef;
                const size_t n = m.Elements();
                for (size_t k = 0; k < n; k++)
                {
                    Struct* sub = s->Sub(m, m.mods.empty() ? -1 : int64_t(k));
                    EncodeStruct(sub, out + m.offset + k * mt->size, mt->size, fix && !wrapped);
                }
                continue;
            }
            std::memcpy(out + m.offset, src + m.offset, m.size);
        }
    }

    void TreeWriter::EncodeUnion(Struct* s, uint8_t* out)
    {
        std::memcpy(out, s->Data(), s->Size());
        const Type* t = s->type;
        Children* ch = s->Ch();
        if (!ch || !HasPointers(t)) return;
        for (const Member& m : t->members)
        {
            if (!m.IsPointer()) continue;
            Node* c = ch->Get(m.name);
            if (const Ref* r = As<Ref>(c))
                Store(out + m.offset, 4, RefValue(r), big);
            else if (c)
            {
                // The data itself is here (the converter put what an alias meant
                // in its place): the pointer must say so, in the union itself,
                // which the walk goes on to load from.
                const uint32_t cur = uint32_t(Load(out + m.offset, 4, big));
                if (cur != 0 && cur != kFollowing && cur != kInsert)
                {
                    const Struct* cs = As<Struct>(c);
                    const uint32_t mark = cs && cs->slot ? kInsert : kFollowing;
                    Store(out + m.offset, 4, mark, big);
                    Store(s->Data() + m.offset, 4, mark, big);
                }
            }
        }
    }

    uint32_t TreeWriter::PtrRaw(const PtrList* pl, size_t i)
    {
        Node* c = pl->items[i];
        if (const Ref* r = As<Ref>(c)) return RefValue(r);
        if (pl->raw && i < pl->raw->size()) return (*pl->raw)[i];
        return c ? kFollowing : 0;
    }

    std::vector<uint8_t> TreeWriter::Encode(const std::optional<Node*>& o, size_t n, const Desc& desc)
    {
        if (desc.kind == Desc::U32)
        {
            std::vector<uint8_t> out(8 * tree.assets.size());
            for (size_t i = 0; i < tree.assets.size(); i++)
            {
                const Entry* e = tree.assets[i];
                uint32_t p;
                if (const Ref* r = As<Ref>(e->asset)) p = RefValue(r);
                else if (!e->asset) p = 0;
                else if (const Struct* a = As<Struct>(e->asset); a && a->slot) p = kInsert;
                else p = kFollowing;
                Store(out.data() + 8 * i, 4, plat.TypeNumber(e->type), big);
                Store(out.data() + 8 * i + 4, 4, p, big);
            }
            return out;
        }
        if (!o) throw Error("nothing to write at " + Path());
        Node* node = *o;
        if (desc.kind == Desc::String)
        {
            const String* st = As<String>(node);
            if (!st) throw Error("a string is something else at " + Path());
            std::vector<uint8_t> out(st->b.begin(), st->b.end());
            out.push_back(0);
            return out;
        }
        if (desc.kind == Desc::Ptrs)
        {
            std::vector<uint8_t> out;
            if (const PtrList* pl = As<PtrList>(node))
            {
                out.resize(4 * pl->items.size());
                for (size_t i = 0; i < pl->items.size(); i++) Store(out.data() + 4 * i, 4, PtrRaw(pl, i), big);
            }
            else if (const Array* a = As<Array>(node))
            {
                out.resize(4 * a->items.size());
                for (size_t i = 0; i < a->items.size(); i++)
                {
                    Node* c = a->items[i];
                    const uint32_t v = As<Ref>(c) ? RefValue(As<Ref>(c)) : c ? kFollowing : 0;
                    Store(out.data() + 4 * i, 4, v, big);
                }
            }
            else throw Error("a pointer list is something else at " + Path());
            return out;
        }
        if (const Leaf* lf = As<Leaf>(node))
        {
            if (lf->big != big && lf->t && lf->t->size > 1)
                throw Error(lf->t->name + " data is still in the other platform's byte order at " + Path());
            return lf->raw;
        }
        std::vector<uint8_t> out(n);
        if (desc.kind == Desc::Partial)
        {
            Struct* s = As<Struct>(node);
            if (!s) throw Error("a struct is something else at " + Path());
            EncodeStruct(s, out.data(), n, true);
            return out;
        }
        const Type* t = desc.t;
        std::vector<Node*> items;
        if (const Array* a = As<Array>(node))
        {
            items = a->items;
            if (a->tail && items.size() > size_t(desc.count)) items.resize(size_t(desc.count));
        }
        else items = { node };
        if (int64_t(items.size()) != desc.count)
            throw Error(t->name + ": " + std::to_string(items.size()) + " items to write, the file says " +
                        std::to_string(desc.count) + " (at " + Path() + ")");
        const bool wrapped = t->kind == Type::Typedef;
        for (size_t i = 0; i < items.size(); i++)
        {
            Struct* s = As<Struct>(items[i]);
            if (!s) throw Error("a " + t->name + " is something else at " + Path());
            EncodeStruct(s, out.data() + i * t->size, t->size, !wrapped);
        }
        return out;
    }

    // ---- the stream ---------------------------------------------------------

    std::pair<BufPtr, Where> TreeWriter::Read(size_t n, const Desc& desc)
    {
        std::optional<Node*> o = obj;
        obj.reset();
        auto data = std::make_shared<Buf>();
        int64_t zp = -1;
        if (block == kRuntime) data->b.assign(n, 0);
        else
        {
            data->b = Encode(o, n, desc);
            if (desc.kind != Desc::String && data->b.size() != n)
                throw Error("wrote " + std::to_string(data->b.size()) + " bytes, expected " + std::to_string(n) + " (at " +
                            Path() + ")");
            n = data->b.size();
            zp = int64_t(pos);
            zone.insert(zone.end(), data->b.begin(), data->b.end());
            pos += n;
        }
        data->segs.push_back({ 0, zp });
        const Where where{ block, blockPos[size_t(block)] };
        if (o && *o && !loc.count(*o)) loc[*o] = where;
        if (n && zp >= 0)
        {
            zonePositions.push_back(size_t(zp));
            zoneSpans.push_back({ uint32_t(block), where.pos, uint32_t(n) });
        }
        blockPos[size_t(block)] += uint32_t(n);
        blockMax[size_t(block)] = std::max(blockMax[size_t(block)], blockPos[size_t(block)]);
        last = o.value_or(nullptr);
        return { data, where };
    }

    void TreeWriter::OnInst(Inst* inst, bool atStart)
    {
        if (reg.count(inst)) return;
        if (atStart)
        {
            Struct* s = As<Struct>(last);
            if (!s) throw Error("a struct is something else at " + Path());
            reg[inst] = s;
            return;
        }
        const Ctx& c = ctx.back();
        reg[inst] = MemberStruct(reg.at(c.inst), c.inst->info, *c.mi->m, c.idx);
    }

    BufPtr TreeWriter::LoadArray(const StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base)
    {
        const size_t size = info->ctype->size;
        Where where;
        const Array* list = nullptr;
        if (atStart)
        {
            list = obj ? As<Array>(*obj) : nullptr;
            std::tie(buf, where) = Read(size * size_t(count), { Desc::Type, info->ctype, count });
            base = 0;
            if (!list && count) throw Error("an array is something else at " + Path());
        }
        const Ctx* c = atStart ? nullptr : &ctx.back();
        for (int64_t k = 0; k < count; k++)
        {
            Inst* e = NewInst(info, buf, base + size_t(k) * size, k, where);
            if (list)
            {
                Struct* s = As<Struct>(list->items[size_t(k)]);
                if (!s) throw Error("an array item is something else at " + Path());
                reg[e] = s;
            }
            else reg[e] = MemberStruct(reg.at(c->inst), c->inst->info, *c->mi->m, c->idx, k);
            vars[info] = e;
            Reader::LoadStruct(info, e, false);
        }
        return buf;
    }

    void TreeWriter::LoadBlock(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t at)
    {
        Struct* d = reg.at(inst);
        const std::vector<int32_t>& idx = ctx.back().idx;
        Node* o;
        if (kind == kEmbedded || kind == kEmbArray || kind == kDynamic) o = d->Late(mi->m->name, idx);
        else o = d->Child(mi->m->name, idx);
        if (o) obj = o;
        else obj.reset();
        Reader::LoadBlock(inst, mi, modPos, combined, kind, at);
        obj.reset();
    }

    void TreeWriter::LoadPtrArray(const MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t at)
    {
        Node* listNode = obj.value_or(nullptr);
        if (atStart)
        {
            buf = Read(4 * size_t(count), { Desc::Ptrs, nullptr, count }).first;
            at = 0;
        }
        auto item = [&](size_t i) -> Node* {
            if (const PtrList* pl = As<PtrList>(listNode)) return pl->items.at(i);
            if (const Array* a = As<Array>(listNode)) return a->items.at(i);
            throw Error("a pointer list is something else at " + Path());
        };
        const StructInfo* t = mi->type;
        for (int64_t k = 0; k < count; k++)
        {
            const size_t p = at + 4 * size_t(k);
            uint32_t val = U32(*buf, p);
            if (!val) continue;
            Node* c = item(size_t(k));
            if (t && !t->asset.empty())
            {
                obj = c;
                LoadAssetPtr(t, *buf, p);
                continue;
            }
            if (mi->isReusable && val != kFollowing)
                if (Node* f = Forward(c))
                {
                    c = f;
                    val = kFollowing;
                    Patch(*buf, p, val);
                }
            if (mi->isReusable && val != kFollowing)
            {
                obj = c;
                Alias(*buf, p, val);
                continue;
            }
            Alloc(t ? AlignOf(t) : mi->m->type->align);
            obj = c;
            if (t && !t->isLeaf) LoadStruct(t, nullptr, true);
            else
            {
                const Type* et = t ? t->ctype : mi->m->type;
                Read(et->size, { Desc::Type, et, 1 });
            }
        }
        obj.reset();
    }

    void TreeWriter::LoadXString(Buf& buf, size_t at)
    {
        std::optional<Node*> o = obj;
        obj.reset();
        uint32_t val = U32(buf, at);
        if (!val) return;
        if (val != kFollowing && o)
            if (Node* f = Forward(*o))
            {
                o = f;
                val = kFollowing;
                Patch(buf, at, val);
            }
        if (val != kFollowing)
        {
            obj = o;
            Alias(buf, at, val);
            obj.reset();
            return;
        }
        const String* st = o ? As<String>(*o) : nullptr;
        if (!st) throw Error("a string is something else at " + Path());
        obj = o;
        Read(st->b.size() + 1, { Desc::String });
    }

    void TreeWriter::LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t at)
    {
        const PtrList* pl = obj ? As<PtrList>(*obj) : nullptr;
        if (!pl && count) throw Error("a string list is something else at " + Path());
        if (atStart)
        {
            buf = Read(4 * size_t(count), { Desc::Ptrs, nullptr, count }).first;
            at = 0;
        }
        for (int64_t k = 0; k < count; k++)
        {
            obj = pl->items[size_t(k)];
            LoadXString(*buf, at + 4 * size_t(k));
        }
        obj.reset();
    }

    void TreeWriter::InsertPointer()
    {
        Push(kVirtual);
        Alloc(4);
        slot = blockPos[kVirtual];
        Pop();
        MakePointer();
        // A struct member's slot (XModelLodInfo.modelSurfs); an asset's is
        // placed in LoadAssetPtr.
        if (obj)
            if (const Struct* s = As<Struct>(*obj); s && s->slot && !loc.count(s->slot)) loc[s->slot] = { kVirtual, slot };
    }

    Inst* TreeWriter::LoadAssetPtr(const StructInfo* info, Buf& buf, size_t at)
    {
        Node* d = obj.value_or(nullptr);
        obj.reset();
        uint32_t val = U32(buf, at);
        const bool inTemp = info->block && info->block->kind == "temp";
        if (val && val != kFollowing && val != kInsert)
            if (Node* f = Forward(d, true))
            {
                d = f;
                const Struct* s = As<Struct>(d);
                val = inTemp && s && s->slot ? kInsert : kFollowing;
                Patch(buf, at, val);
            }
        Struct* ds = As<Struct>(d);
        if (val == kFollowing && inTemp && ds && ds->slot && ds->forward)
        {
            // Pointed at from elsewhere too (written at its first pointer): it
            // keeps a slot, which later pointers name (its data is in the temp
            // block, which the game reuses for the next asset).
            val = kInsert;
            Patch(buf, at, val);
        }
        if (dedupe && (val == kFollowing || val == kInsert) && ds && first.count(ds))
        {
            // Written already: point at the pointer that first held it, which
            // the game sets to it when it loads it, as stock files do.
            val = first[ds];
            Patch(buf, at, val);
            deduped++;
            return nullptr;
        }
        if (dedupe && val == kFollowing && ds)
            if (auto where = Address(buf, at); where && where->block != kTemp)
                first[ds] = ((uint32_t(where->block) << kBlockShift) | where->pos) + 1;
        if (inTemp) Push(kTemp);
        Inst* inst = nullptr;
        if (val)
        {
            if (val == kFollowing || (inTemp && val == kInsert))
            {
                Alloc(AlignOf(info));
                if (inTemp && val == kInsert)
                {
                    InsertPointer();
                    if (ds && ds->slot) loc[ds->slot] = { kVirtual, slot };
                    if (dedupe && ds) first.emplace(ds, ((uint32_t(kVirtual) << kBlockShift) | slot) + 1);
                }
                if (!ds) throw Error("an asset is something else at " + Path());
                starts[ds] = pos;
                written.push_back({ pos, ds });
                obj = ds;
                inst = LoadStruct(info, nullptr, true);
            }
            else
            {
                if (Ref* r = As<Ref>(d))
                {
                    Struct* target = As<Struct>(r->target);
                    if (target && !r->rel && target->slot && loc.count(target->slot))
                    {
                        // A temp asset written already (where its first pointer
                        // took it): point at its slot, which the game sets to
                        // the asset, not at its data in the temp block.
                        Ref* to = A().Make<Ref>(r->val);
                        to->target = target->slot;
                        d = to;
                    }
                }
                obj = d;
                Alias(buf, at, val);
                obj.reset();
            }
        }
        if (inTemp) Pop();
        return inst;
    }

    // ---- the zone -----------------------------------------------------------

    std::vector<uint8_t> TreeWriter::Write(const std::vector<uint8_t>* header)
    {
        const PtrList* ss = tree.scriptStrings;
        const bool haveStrings = ss && !ss->items.empty();
        const bool haveAssets = !tree.assets.empty();
        uint8_t list[16];
        Store(list, 4, haveStrings ? ss->items.size() : 0, big);
        Store(list + 4, 4, haveStrings ? kFollowing : 0, big);
        Store(list + 8, 4, tree.assets.size(), big);
        Store(list + 12, 4, haveAssets ? kFollowing : 0, big);
        zone.insert(zone.end(), list, list + 16);
        pos += 16;
        Push(kVirtual);
        if (haveStrings)
        {
            Alloc(4);
            obj = const_cast<PtrList*>(ss);
            LoadXStringArray(true, int64_t(ss->items.size()), {}, 0);
        }
        Pop();
        Push(kVirtual);
        if (haveAssets)
        {
            Alloc(4);
            obj.reset();
            auto [buf, where] = Read(8 * tree.assets.size(), { Desc::U32, nullptr, 2 * int64_t(tree.assets.size()) });
            for (size_t i = 0; i < tree.assets.size(); i++) loc[tree.assets[i]] = { where.block, where.pos + uint32_t(8 * i) };
            for (size_t i = 0; i < tree.assets.size(); i++)
            {
                const Entry* e = tree.assets[i];
                const uint32_t type = plat.TypeNumber(e->type);
                const StructInfo* info = s.assets.at(plat.structs.at(type));
                assetType = int(type);
                obj = e->asset;
                try
                {
                    LoadAssetPtr(info, *buf, 8 * i + 4);
                }
                catch (const Error& err)
                {
                    const Struct* a = As<Struct>(e->asset);
                    throw Error(std::string(err.what()) + ", in asset " + std::to_string(i) + " (" + e->type + " " +
                                (a ? Name(a).value_or("?") : "?") + ")");
                }
            }
        }
        Pop();
        std::vector<uint32_t> hdr{ uint32_t(zone.size() - plat.HeaderBytes()), 0 };
        if (header) hdr[1] = uint32_t(Load(header->data() + 4, 4, big));
        for (int b = 0; b < plat.blocks; b++) hdr.push_back(b == kTemp ? blockMax[size_t(b)] : blockPos[size_t(b)]);
        if (header)
        {
            // The temp and callback sizes are allowances the walk alone doesn't
            // show (loader scratch space, data set up by callbacks): the
            // original's are kept when bigger.
            for (int b : { int(kTemp), int(kCallback) })
                hdr[2 + size_t(b)] = std::max(hdr[2 + size_t(b)], uint32_t(Load(header->data() + 8 + 4 * b, 4, big)));
        }
        for (size_t k = 0; k < hdr.size(); k++) Store(zone.data() + 4 * k, 4, hdr[k], big);
        return zone;
    }
}
