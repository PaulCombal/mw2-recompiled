#include "walker.h"

#include <algorithm>
#include <cstring>

namespace zone
{
    namespace
    {
        // A pointer that says its data follows in the stream, and one that says
        // the loader makes a pointer to it in the virtual block.
        constexpr uint32_t kFollowing = 0xFFFFFFFF;
        constexpr uint32_t kInsert = 0xFFFFFFFE;

        enum Kind { kDynamic, kSingle, kArray, kPtrArray, kEmbArray, kEmbedded };

        // The title update's asset types (DB_GetXAssetTypeName) and the rules'
        // name for each; the ones missing never appear in a 360 zone.
        const char* AssetStruct(uint32_t type)
        {
            static const char* const kStructs[] = {
                "AssetPhysPreset", "AssetPhysCollMap", "AssetXAnim", "AssetXModelSurfs", "AssetXModel",
                "AssetMaterial", "AssetPixelShader", "AssetTechniqueSet", "AssetImage", "AssetSound",
                "AssetSoundCurve", "AssetLoadedSound", "AssetClipMapMp", "AssetClipMapMp", "AssetComWorld",
                "AssetGameWorldSp", "AssetGameWorldMp", "AssetMapEnts", "AssetFxWorld", "AssetGfxWorld",
                "AssetLightDef", nullptr, "AssetFont", "AssetMenuList", "AssetMenu", "AssetLocalize",
                "AssetWeapon", "AssetSndDriverGlobals", "AssetFx", "AssetImpactFx", nullptr, nullptr, nullptr,
                nullptr, "AssetRawFile", "AssetStringTable", "AssetLeaderboard", "AssetStructuredDataDef",
                "AssetTracer", "AssetVehicle", "AssetAddonMapEnts",
            };
            return type < std::size(kStructs) ? kStructs[type] : nullptr;
        }

        int64_t ReadValue(const uint8_t* p, const Type* t)
        {
            uint64_t v = 0;
            for (uint32_t k = 0; k < t->size; k++) v = (v << 8) | p[k];
            if (t->isFloat)
            {
                if (t->size == 4)
                {
                    uint32_t bits = uint32_t(v);
                    float f;
                    std::memcpy(&f, &bits, 4);
                    return int64_t(f);
                }
                double d;
                std::memcpy(&d, &v, 8);
                return int64_t(d);
            }
            if (t->isSigned && t->size < 8 && (v >> (t->size * 8 - 1)) & 1) v |= ~uint64_t(0) << (t->size * 8);
            return int64_t(v);
        }
    }

    Walker::Walker(const Schema& schema, const uint8_t* zone, size_t size) : s(schema), zone(zone), size(size)
    {
        if (auto it = s.infos.find("XModelLodInfo"); it != s.infos.end()) lodInfo = it->second;
        if (auto it = s.infos.find("XModelSurfs"); it != s.infos.end()) surfs = it->second;
    }

    // ---- the stream ---------------------------------------------------------

    Buffer Walker::Read(size_t n)
    {
        Buffer b;
        last = { block, blockPos[block], pos, block != kRuntime };
        blockPos[block] += uint32_t(n);
        if (block == kRuntime)
        {
            // Made by the loader at run time: nothing of it is in the file.
            b.own = std::make_shared<std::vector<uint8_t>>(n);
            return b;
        }
        if (n > size - pos) throw Error("the zone ends inside the data at " + std::to_string(pos));
        b.view = zone + pos;
        b.viewSize = n;
        pos += n;
        return b;
    }

    // The temp block is a scratch area: what was pushed onto it is gone when
    // it is popped.
    void Walker::Push(int b)
    {
        stack.push_back({ blockPos[b], block });
        block = b;
    }

    void Walker::Pop()
    {
        if (block == kTemp) blockPos[kTemp] = stack.back().first;
        block = stack.back().second;
        stack.pop_back();
    }

    void Walker::Alloc(uint32_t align)
    {
        if (align > 1) blockPos[block] = (blockPos[block] + align - 1) / align * align;
    }

    // A pointer the loader makes in the virtual block, for data in the temp
    // one that something later points to.
    void Walker::InsertPointer()
    {
        Push(kVirtual);
        Alloc(4);
        blockPos[kVirtual] += 4;
        Pop();
    }

    int Walker::BlockOf(const Block* b)
    {
        const std::string& n = b->name;
        if (n == "XFILE_BLOCK_TEMP") return kTemp;
        if (n == "XFILE_BLOCK_RUNTIME") return kRuntime;
        if (n == "XFILE_BLOCK_VIRTUAL") return kVirtual;
        if (n == "XFILE_BLOCK_LARGE") return kLarge;
        if (n == "XFILE_BLOCK_CALLBACK") return kCallback;
        return kPhysical;   // vertex and index data go in the physical block on the 360
    }

    uint32_t Walker::U32(const Buffer& buf, size_t at) const
    {
        if (at + 4 > buf.Size()) throw Error("a pointer outside its struct at " + std::to_string(pos));
        const uint8_t* p = buf.Data() + at;
        return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 | uint32_t(p[2]) << 8 | p[3];
    }

    void Walker::Append(Buffer& dst, const Buffer& src, size_t off, size_t n)
    {
        dst.own->insert(dst.own->end(), src.Data() + off, src.Data() + off + n);
    }

    // ---- the rules' expressions --------------------------------------------

    int64_t Walker::Field(const Inst& inst, const std::vector<MemberInfo*>& chain, const std::vector<int64_t>& idx) const
    {
        size_t off = inst.off;
        for (size_t k = 0; k + 1 < chain.size(); k++) off += chain[k]->m->offset;
        const Member& m = *chain.back()->m;
        off += m.offset;
        if (!m.mods.empty() && m.mods[0] == kPointer) return U32(inst.buf, off);
        std::vector<int64_t> dims;
        for (int x : m.mods)
            if (x != kPointer) dims.push_back(x);
        if (!idx.empty())
        {
            int64_t stride = m.type->size;
            for (size_t d = idx.size(); d < dims.size(); d++) stride *= dims[d];
            for (size_t k = 0; k < idx.size(); k++)
            {
                int64_t sub = stride;
                for (size_t d = k + 1; d < idx.size() && d < dims.size(); d++) sub *= dims[d];
                off += size_t(idx[k] * sub);
            }
        }
        if (m.type->kind != Type::Prim && m.type->kind != Type::Enum)
            throw Error("a rule reads " + chain.back()->name + ", which is not a number");
        if (off + m.type->size > inst.buf.Size()) throw Error("a rule reads past its struct: " + chain.back()->name);
        int64_t v = ReadValue(inst.buf.Data() + off, m.type);
        if (m.bits >= 0) v = (v >> m.bitpos) & ((int64_t(1) << m.bits) - 1);
        return v;
    }

    int64_t Walker::Lookup(const ExprRef& ref, const std::vector<int64_t>& idx) const
    {
        auto found = vars.find(ref.info);
        if (ref.isIndex)
        {
            if (found == vars.end()) throw Error("a rule needs the index of " + ref.info->name);
            return found->second.index;
        }
        // A model surfaces asset listed on its own (not reached through a model's
        // LOD entry) carries its surface count itself; an earlier model's LOD
        // entry is stale.
        if (ref.info == lodInfo && assetType == 3 && surfs)
            if (auto own = vars.find(surfs); own != vars.end()) found = own;
        if (found == vars.end()) throw Error("a rule needs " + ref.info->name + ", and none is loaded");
        const Inst& inst = found->second;
        std::vector<MemberInfo*> chain = ref.chain;
        if (inst.info != ref.info)
        {
            chain.clear();
            const StructInfo* cur = inst.info;
            for (const std::string& n : ref.names)
            {
                auto m = s.FindMember(cur, n);
                if (m.empty()) throw Error("a rule needs " + n + " of " + cur->name);
                chain.insert(chain.end(), m.begin(), m.end());
                cur = m.back()->type;
            }
        }
        const Member& m = *chain.back()->m;
        if (!idx.empty() && !m.mods.empty() && m.mods[0] == kPointer)
        {
            // Indexing through a pointer: the data loaded for it.
            auto data = pointed.find(chain.back());
            if (data == pointed.end()) throw Error("a rule needs the data of " + chain.back()->name);
            const size_t at = size_t(idx[0]) * m.type->size;
            if (at + m.type->size > data->second.Size()) throw Error("a rule reads past " + chain.back()->name);
            return ReadValue(data->second.Data() + at, m.type);
        }
        return Field(inst, chain, idx);
    }

    int64_t Walker::Eval(const Expr* e) const
    {
        if (!e) throw Error("a count is missing from the rules");
        if (e->constant) return *e->constant;
        return e->root->Eval([&](int ref, const std::vector<int64_t>& idx) { return Lookup(e->refs[ref], idx); },
                             true);
    }

    // ---- structs ------------------------------------------------------------

    Walker::Inst Walker::LoadStruct(const StructInfo* info, const Inst* given, bool atStart)
    {
        const MemberInfo* dyn = info->dynamic;
        Inst inst;
        if (atStart && !(info->isUnion && dyn))
        {
            // Up to a dynamic member, the struct is streamed at once.
            const size_t n = dyn ? dyn->m->offset : info->ctype->size;
            Buffer data = Read(n);
            if (dyn)
            {
                Buffer grown;
                grown.own = std::make_shared<std::vector<uint8_t>>(data.Data(), data.Data() + data.Size());
                grown.growable = true;
                data = grown;
            }
            inst = { info, data, 0, 0 };
        }
        else if (atStart)
        {
            // A union whose members are only partly known: each streams itself.
            Buffer grown;
            grown.own = std::make_shared<std::vector<uint8_t>>();
            grown.growable = true;
            inst = { info, grown, 0, 0 };
        }
        else inst = *given;
        vars[info] = inst;

        bool pushed = false;
        if (!info->asset.empty())
        {
            Push(kVirtual);
            pushed = true;
        }
        else if (info->block)
        {
            Push(BlockOf(info->block));
            pushed = true;
        }
        if (info->isUnion)
        {
            for (const MemberInfo* mi : info->used)
            {
                if (!s.NeedsTreatment(mi)) continue;
                if (!mi->condition || Eval(mi->condition))
                {
                    vars[info] = inst;
                    LoadReference(inst, mi, 0, {});
                    break;
                }
            }
        }
        else
        {
            for (const MemberInfo* mi : info->ordered)
            {
                if (!s.NeedsTreatment(mi)) continue;
                if (mi->condition && !Eval(mi->condition)) continue;
                vars[info] = inst;
                LoadReference(inst, mi, 0, {});
            }
        }
        if (pushed) Pop();
        return inst;
    }

    Buffer Walker::LoadArray(const StructInfo* info, bool atStart, int64_t count, Buffer buf, size_t base)
    {
        if (count < 0) throw Error("a negative count for " + info->name);
        if (atStart)
        {
            buf = Read(info->ctype->size * size_t(count));
            base = 0;
        }
        for (int64_t k = 0; k < count; k++)
        {
            Inst e{ info, buf, base + size_t(k) * info->ctype->size, k };
            vars[info] = e;
            LoadStruct(info, &e, false);
        }
        return buf;
    }

    // ---- members ------------------------------------------------------------

    size_t Walker::MemberLoc(const Inst& inst, const MemberInfo* mi, const std::vector<int64_t>& indices) const
    {
        const Member& m = *mi->m;
        // The array sizes before the first pointer, and an element's size: a
        // pointer's if there is one anywhere.
        std::vector<int64_t> dims;
        for (int x : m.mods)
        {
            if (x == kPointer) break;
            dims.push_back(x);
        }
        const bool pointer = std::find(m.mods.begin(), m.mods.end(), kPointer) != m.mods.end();
        const int64_t elem = pointer ? 4 : m.type->size;
        size_t off = inst.off + m.offset;
        for (size_t k = 0; k < indices.size(); k++)
        {
            int64_t stride = elem;
            for (size_t d = k + 1; d < dims.size(); d++) stride *= dims[d];
            off += size_t(indices[k] * stride);
        }
        return off;
    }

    void Walker::LoadReference(const Inst& inst, const MemberInfo* mi, size_t modPos, std::vector<int64_t> indices)
    {
        const std::vector<int>& mods = mi->m->mods;
        const bool hasCur = modPos < mods.size(), hasNext = modPos + 1 < mods.size();
        const int cur = hasCur ? mods[modPos] : 0, nxt = hasNext ? mods[modPos + 1] : 0;
        const bool curPointer = hasCur && cur == kPointer, curArray = hasCur && cur != kPointer;
        const bool nextPointer = hasNext && nxt == kPointer;
        int64_t combined = 0;
        if (!indices.empty())
        {
            std::vector<int64_t> dims;
            for (int x : mods)
                if (x != kPointer) dims.push_back(x);
            for (size_t k = 0; k < indices.size(); k++)
            {
                int64_t st = 1;
                for (size_t d = k + 1; d < dims.size(); d++) st *= dims[d];
                combined += indices[k] * st;
            }
        }
        bool followingPointer = false;
        for (size_t k = modPos + 1; k < mods.size(); k++)
            if (mods[k] == kPointer) followingPointer = true;

        int kind;
        if (curArray && mi->arraySize.count(modPos)) kind = kDynamic;
        else if (curPointer && !mi->CountIsArray(modPos, combined) && !followingPointer) kind = kSingle;
        else if (curPointer && mi->CountIsArray(modPos, combined) && !followingPointer) kind = kArray;
        else if (((curPointer && mi->CountIsArray(modPos, combined)) || curArray) && nextPointer &&
                 !mi->AnyCountIsArray(modPos + 1))
            kind = kPtrArray;
        else if (curArray && !hasNext) kind = kEmbArray;
        else if (!hasCur) kind = kEmbedded;
        else if (curArray)
        {
            for (int k = 0; k < cur; k++)
            {
                std::vector<int64_t> deeper = indices;
                deeper.push_back(k);
                LoadReference(inst, mi, modPos + 1, deeper);
            }
            return;
        }
        else throw Error("an unsupported member: " + mi->parent->name + "::" + mi->name);
        LoadBlock(inst, mi, modPos, combined, kind, MemberLoc(inst, mi, indices));
    }

    void Walker::LoadBlock(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, int kind, size_t loc)
    {
        const Block* b = mi->block;
        if (mi->blockCond.cond) b = Eval(mi->blockCond.cond) ? mi->blockCond.ifTrue : mi->blockCond.ifFalse;
        const bool push = b && !(b->kind == "normal" && b->isDefault);
        if (push) Push(BlockOf(b));

        const std::vector<int>& mods = mi->m->mods;
        const bool isArray = modPos < mods.size() && mods[modPos] != kPointer;
        const bool pointerKind = kind == kArray || kind == kPtrArray || kind == kSingle;
        const bool check = pointerKind && !(kind == kPtrArray && isArray) && !(kind == kSingle && mi->isString);
        bool load = true;
        uint32_t val = 0;
        bool haveVal = false;
        if (check)
        {
            val = U32(inst.buf, loc);
            haveVal = true;
            if (onPointer && kind == kSingle) onPointer(*mi, -1, val);
            if (!val) load = false;
        }
        if (load && pointerKind && !(kind == kPtrArray && isArray) && mi->isReusable)
        {
            // Anything but data that follows is a pointer back to what was loaded
            // before: nothing of it is in the stream here.
            if (!haveVal) val = U32(inst.buf, loc);
            const bool inTemp = mi->block && mi->block->kind == "temp";
            if (!(val == kFollowing || (inTemp && val == kInsert))) load = false;
        }
        if (load)
        {
            AllocMember(inst, mi, modPos, kind, loc);
            TypeCheck(inst, mi, modPos, combined, kind, loc);
        }
        if (push) Pop();
    }

    // Where the loader puts what a pointer names: aligned for it, and, for
    // data in the temp block a later pointer may name, with a pointer to it
    // made in the virtual block.
    void Walker::AllocMember(const Inst& inst, const MemberInfo* mi, size_t modPos, int kind, size_t loc)
    {
        const std::vector<int>& mods = mi->m->mods;
        const bool isArray = modPos < mods.size() && mods[modPos] != kPointer;
        bool make = kind == kArray || kind == kPtrArray || kind == kSingle;
        if (kind == kPtrArray) make = !isArray;
        else if (mi->isString || (mi->type && !mi->type->asset.empty())) make = false;
        if (!make) return;
        uint32_t align;
        bool followingPointer = false;
        for (size_t k = modPos + 1; k < mods.size(); k++)
            if (mods[k] == kPointer) followingPointer = true;
        if (mi->allocAlign) align = mi->allocAlign;
        else if (followingPointer) align = 4;
        else
        {
            align = mi->type ? mi->type->ctype->align : mi->m->type->align;
            if (mi->type && mi->type->allocAlign) align = mi->type->allocAlign;
        }
        Alloc(align);
        const bool inTemp = mi->block && mi->block->kind == "temp";
        if (inTemp && U32(inst.buf, loc) == kInsert) InsertPointer();
    }

    size_t Walker::ElemSize(const MemberInfo* mi, size_t modPos) const
    {
        size_t n = mi->m->type->size;
        for (size_t k = modPos + 1; k < mi->m->mods.size(); k++)
        {
            if (mi->m->mods[k] == kPointer) return 4;
            n *= size_t(mi->m->mods[k]);
        }
        return n;
    }

    void Walker::TypeCheck(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, int kind, size_t loc)
    {
        const std::vector<int>& mods = mi->m->mods;
        const bool inRuntime = mi->block && mi->block->kind == "runtime";
        if (mi->isString)
        {
            if (kind == kSingle) LoadXString(inst.buf, loc);
            else if (kind == kPtrArray)
            {
                if (mods[modPos] != kPointer) LoadXStringArray(false, mods[modPos], inst.buf, loc);
                else LoadXStringArray(true, Eval(mi->Count(modPos, combined)));
            }
            return;
        }
        const StructInfo* t = mi->type;
        if (t && !t->asset.empty())
        {
            if (kind == kSingle) LoadAssetPtr(t, inst.buf, loc);
            else if (kind == kPtrArray) PtrArray(inst, mi, modPos, combined, loc);
            return;
        }
        switch (kind)
        {
        case kArray:
        {
            const int64_t n = Eval(mi->Count(modPos, combined));
            if (n < 0) throw Error("a negative count for " + mi->name);
            if (t && !t->isLeaf && !inRuntime) LoadArray(t, true, n);
            else
            {
                const size_t bytes = ElemSize(mi, modPos) * size_t(n);
                Buffer data = Read(bytes);
                pointed[mi] = data;
                if (onData) onData(*mi, last, bytes);
            }
            break;
        }
        case kSingle:
            if (t && !t->isLeaf && !inRuntime) LoadStruct(t, nullptr, true);
            else
            {
                const size_t bytes = ElemSize(mi, modPos);
                Read(bytes);
                if (onData) onData(*mi, last, bytes);
            }
            break;
        case kPtrArray:
            PtrArray(inst, mi, modPos, combined, loc);
            break;
        case kEmbArray:
        {
            const int64_t n = mods[modPos];
            const bool after = s.AfterPartialLoad(mi);
            if (!mi->isLeaf && t)
            {
                if (after) LoadArray(t, true, n);
                else LoadArray(t, false, n, inst.buf, loc);
            }
            else if (after) Read((t ? t->ctype->size : mi->m->type->size) * size_t(n));
            break;
        }
        case kEmbedded:
        {
            const bool after = s.AfterPartialLoad(mi);
            if (!mi->isLeaf && t)
            {
                if (after)
                {
                    Inst sub = LoadStruct(t, nullptr, true);
                    if (inst.buf.growable)
                    {
                        Buffer dst = inst.buf;
                        if (sub.buf.growable) Append(dst, sub.buf, 0, sub.buf.Size());
                        else Append(dst, sub.buf, sub.off, t->ctype->size);
                    }
                }
                else
                {
                    // A struct wrapping another at offset 0 (GfxCellTree128 around
                    // GfxCellTree) keeps its array index, for counts written as
                    // pointer differences.
                    Inst e{ t, inst.buf, loc, loc == inst.off ? inst.index : 0 };
                    LoadStruct(t, &e, false);
                }
            }
            else if (after)
            {
                Buffer data = Read(t ? t->ctype->size : mi->m->type->size);
                if (inst.buf.growable)
                {
                    Buffer dst = inst.buf;
                    Append(dst, data, 0, data.Size());
                }
            }
            break;
        }
        case kDynamic:
        {
            const int64_t n = Eval(mi->arraySize.at(modPos));
            if (n < 0) throw Error("a negative size for " + mi->name);
            Buffer buf;
            if (t && !t->isLeaf) buf = LoadArray(t, true, n);
            else
            {
                size_t elem = mi->m->type->size;
                for (size_t k = 1; k < mods.size(); k++) elem *= size_t(mods[k]);
                buf = Read(elem * size_t(n));
            }
            if (inst.buf.growable)
            {
                Buffer dst = inst.buf;
                Append(dst, buf, 0, buf.Size());
            }
            break;
        }
        }
    }

    void Walker::PtrArray(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, size_t loc)
    {
        const int cur = mi->m->mods[modPos];
        if (cur != kPointer) LoadPtrArray(mi, false, cur, inst.buf, loc);
        else LoadPtrArray(mi, true, Eval(mi->Count(modPos, combined)));
    }

    void Walker::LoadPtrArray(const MemberInfo* mi, bool atStart, int64_t count, Buffer buf, size_t loc)
    {
        if (count < 0) throw Error("a negative count for " + mi->name);
        if (atStart)
        {
            buf = Read(4 * size_t(count));
            loc = 0;
        }
        const StructInfo* t = mi->type;
        for (int64_t k = 0; k < count; k++)
        {
            const size_t at = loc + 4 * size_t(k);
            const uint32_t val = U32(buf, at);
            if (onPointer) onPointer(*mi, k, val);
            if (!val) continue;
            if (t && !t->asset.empty())
            {
                LoadAssetPtr(t, buf, at);
                continue;
            }
            if (mi->isReusable && val != kFollowing) continue;
            Alloc(t ? (t->allocAlign ? t->allocAlign : t->ctype->align) : mi->m->type->align);
            if (t && !t->isLeaf) LoadStruct(t, nullptr, true);
            else Read(t ? t->ctype->size : mi->m->type->size);
        }
    }

    // ---- strings and assets -------------------------------------------------

    void Walker::LoadXString(const Buffer& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        if (val != kFollowing) return;   // none, or one loaded before
        const void* end = std::memchr(zone + pos, 0, size - pos);
        if (!end) throw Error("a string runs off the end of the zone");
        Read(size_t(static_cast<const uint8_t*>(end) - (zone + pos)) + 1);
    }

    void Walker::LoadXStringArray(bool atStart, int64_t count, Buffer buf, size_t loc)
    {
        if (count < 0) throw Error("a negative string count");
        if (atStart)
        {
            buf = Read(4 * size_t(count));
            loc = 0;
        }
        for (int64_t k = 0; k < count; k++) LoadXString(buf, loc + 4 * size_t(k));
    }

    void Walker::LoadAssetPtr(const StructInfo* info, const Buffer& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        const bool inTemp = info->block && info->block->kind == "temp";
        if (inTemp) Push(kTemp);
        if (val == kFollowing || (inTemp && val == kInsert))
        {
            Alloc(info->allocAlign ? info->allocAlign : info->ctype->align);
            if (inTemp && val == kInsert) InsertPointer();
            LoadStruct(info, nullptr, true);
        }
        if (inTemp) Pop();
    }

    void Walker::Walk()
    {
        // The header: the zone's size, then each of the six blocks'.
        if (size < 8 + 4 * 6 + 16) throw Error("the zone is too short for its header");
        for (int k = 0; k < 6; k++) headerSizes[k] = U32({ zone, size }, 8 + 4 * size_t(k));
        pos = 8 + 4 * 6;
        Buffer head{ zone + pos, 16 };
        const uint32_t stringCount = U32(head, 0), strings = U32(head, 4);
        const uint32_t count = U32(head, 8), list = U32(head, 12);
        pos += 16;
        Push(kVirtual);
        if (strings)
        {
            Alloc(4);
            Buffer b = Read(4 * size_t(stringCount));
            for (uint32_t k = 0; k < stringCount; k++) LoadXString(b, 4 * size_t(k));
        }
        Pop();
        Push(kVirtual);
        if (list)
        {
            Alloc(4);
            Buffer b = Read(8 * size_t(count));
            for (uint32_t k = 0; k < count; k++)
            {
                const uint32_t type = U32(b, 8 * size_t(k));
                const char* name = AssetStruct(type);
                auto info = name ? s.assets.find(name) : s.assets.end();
                if (info == s.assets.end())
                    throw Error("asset " + std::to_string(k) + " has a type the rules do not describe (" +
                                std::to_string(type) + ")");
                assetType = type;
                LoadAssetPtr(info->second, b, 8 * size_t(k) + 4);
            }
            assetCount = count;
        }
        Pop();
        if (pos != size)
            throw Error("the walk stopped at " + std::to_string(pos) + " of " + std::to_string(size) + " bytes");
        // The temp block is a scratch area and the callback one is not
        // streamed; the others end where the header says.
        for (int k : { kPhysical, kRuntime, kVirtual, kLarge })
            if (blockPos[k] != headerSizes[k])
                throw Error("block " + std::to_string(k) + " ends at " + std::to_string(blockPos[k]) + ", not at the " +
                            std::to_string(headerSizes[k]) + " the zone's header gives");
    }
}
