#include "reader.h"

#include <algorithm>
#include <cstring>

namespace mapport
{
    using zone::Member;
    using zone::MemberInfo;
    using zone::StructInfo;
    using zone::Type;

    // ---- platforms ----------------------------------------------------------

    namespace
    {
        Platform MakeXbox()
        {
            // The title update's asset types (DB_GetXAssetTypeName).
            Platform p{ "xbox", true, 6,
                        { "physpreset", "phys_collmap", "xanim", "xmodelsurfs", "xmodel", "material", "pixelshader",
                          "techset", "image", "sound", "sndcurve", "loaded_sound", "col_map_sp", "col_map_mp",
                          "com_map", "game_map_sp", "game_map_mp", "map_ents", "fx_map", "gfx_map", "lightdef",
                          "ui_map", "font", "menufile", "menu", "localize", "weapon", "snddriverglobals", "fx",
                          "impactfx", "aitype", "mptype", "character", "xmodelalias", "rawfile", "stringtable",
                          "leaderboarddef", "structureddatadef", "tracer", "vehicle", "addon_map_ents" },
                        {}, false };
            p.structs = { { 0, "AssetPhysPreset" }, { 1, "AssetPhysCollMap" }, { 2, "AssetXAnim" },
                          { 3, "AssetXModelSurfs" }, { 4, "AssetXModel" }, { 5, "AssetMaterial" },
                          { 6, "AssetPixelShader" }, { 7, "AssetTechniqueSet" }, { 8, "AssetImage" },
                          { 9, "AssetSound" }, { 10, "AssetSoundCurve" }, { 11, "AssetLoadedSound" },
                          { 12, "AssetClipMapMp" }, { 13, "AssetClipMapMp" }, { 14, "AssetComWorld" },
                          { 15, "AssetGameWorldSp" }, { 16, "AssetGameWorldMp" }, { 17, "AssetMapEnts" },
                          { 18, "AssetFxWorld" }, { 19, "AssetGfxWorld" }, { 20, "AssetLightDef" },
                          { 22, "AssetFont" }, { 23, "AssetMenuList" }, { 24, "AssetMenu" },
                          { 25, "AssetLocalize" }, { 26, "AssetWeapon" }, { 27, "AssetSndDriverGlobals" },
                          { 28, "AssetFx" }, { 29, "AssetImpactFx" }, { 34, "AssetRawFile" },
                          { 35, "AssetStringTable" }, { 36, "AssetLeaderboard" }, { 37, "AssetStructuredDataDef" },
                          { 38, "AssetTracer" }, { 39, "AssetVehicle" }, { 40, "AssetAddonMapEnts" } };
            return p;
        }

        Platform MakePc()
        {
            // The PC's (version 276): the 360's and vertex shaders and declarations.
            Platform p{ "pc", false, 8,
                        { "physpreset", "phys_collmap", "xanim", "xmodelsurfs", "xmodel", "material", "pixelshader",
                          "vertexshader", "vertexdecl", "techset", "image", "sound", "sndcurve", "loaded_sound",
                          "col_map_sp", "col_map_mp", "com_map", "game_map_sp", "game_map_mp", "map_ents", "fx_map",
                          "gfx_map", "lightdef", "ui_map", "font", "menufile", "menu", "localize", "weapon",
                          "snddriverglobals", "fx", "impactfx", "aitype", "mptype", "character", "xmodelalias",
                          "rawfile", "stringtable", "leaderboarddef", "structureddatadef", "tracer", "vehicle",
                          "addon_map_ents" },
                        {}, true };
            p.structs = { { 0, "AssetPhysPreset" }, { 1, "AssetPhysCollMap" }, { 2, "AssetXAnim" },
                          { 3, "AssetXModelSurfs" }, { 4, "AssetXModel" }, { 5, "AssetMaterial" },
                          { 6, "AssetPixelShader" }, { 7, "AssetVertexShader" }, { 8, "AssetVertexDecl" },
                          { 9, "AssetTechniqueSet" }, { 10, "AssetImage" }, { 11, "AssetSound" },
                          { 12, "AssetSoundCurve" }, { 13, "AssetLoadedSound" }, { 14, "AssetClipMapMp" },
                          { 15, "AssetClipMapMp" }, { 16, "AssetComWorld" }, { 17, "AssetGameWorldSp" },
                          { 18, "AssetGameWorldMp" }, { 19, "AssetMapEnts" }, { 20, "AssetFxWorld" },
                          { 21, "AssetGfxWorld" }, { 22, "AssetLightDef" }, { 24, "AssetFont" },
                          { 25, "AssetMenuList" }, { 26, "AssetMenu" }, { 27, "AssetLocalize" },
                          { 28, "AssetWeapon" }, { 30, "AssetFx" }, { 31, "AssetImpactFx" },
                          { 36, "AssetRawFile" }, { 37, "AssetStringTable" }, { 38, "AssetLeaderboard" },
                          { 39, "AssetStructuredDataDef" }, { 40, "AssetTracer" }, { 41, "AssetVehicle" },
                          { 42, "AssetAddonMapEnts" } };
            return p;
        }

        bool HasPointerAfter(const Member& m, size_t modPos)
        {
            for (size_t k = modPos + 1; k < m.mods.size(); k++)
                if (m.mods[k] == zone::kPointer) return true;
            return false;
        }

        std::string Name(const MemberInfo* mi, const std::vector<int32_t>& idx, Reader::Kind kind)
        {
            static const char* const kKinds[] = { "dynamic", "single", "array", "ptrarray", "embarray", "embedded" };
            std::string s = mi->name;
            for (int32_t i : idx) s += "[" + std::to_string(i) + "]";
            return s + "(" + kKinds[kind] + ")";
        }
    }

    const Platform& Xbox()
    {
        static const Platform p = MakeXbox();
        return p;
    }

    const Platform& Pc()
    {
        static const Platform p = MakePc();
        return p;
    }

    int Platform::BlockOf(const std::string& n) const
    {
        if (n == "XFILE_BLOCK_TEMP") return kTemp;
        if (n == "XFILE_BLOCK_PHYSICAL") return kPhysical;
        if (n == "XFILE_BLOCK_RUNTIME") return kRuntime;
        if (n == "XFILE_BLOCK_VIRTUAL") return kVirtual;
        if (n == "XFILE_BLOCK_LARGE") return kLarge;
        if (n == "XFILE_BLOCK_CALLBACK") return kCallback;
        // Vertex and index data: blocks of their own on the PC, the physical one on the 360.
        if (n == "XFILE_BLOCK_VERTEX") return vertexBlocks ? kVertex : kPhysical;
        if (n == "XFILE_BLOCK_INDEX") return vertexBlocks ? kIndex : kPhysical;
        throw Error("no memory block " + n);
    }

    uint32_t Platform::TypeNumber(const std::string& typeName) const
    {
        for (size_t k = 0; k < types.size(); k++)
            if (types[k] == typeName) return uint32_t(k);
        throw Error(name + " zones have no asset type " + typeName);
    }

    // ---- buffers ------------------------------------------------------------

    int64_t Buf::ZoneAt(size_t loc) const
    {
        const std::pair<size_t, int64_t>* best = nullptr;
        for (const auto& seg : segs)
            if (seg.first <= loc) best = &seg;
        if (!best || best->second < 0) return -1;
        return best->second + int64_t(loc - best->first);
    }

    namespace
    {
        // Run-time data a zone may ask the loader for (stock files ask for a few MB).
        constexpr size_t kLargestRuntimeBlock = 256u << 20;

        // src's `n` bytes from `off` on appended to dst, keeping where they came from.
        void Extend(Buf& dst, const Buf& src, size_t off, size_t n)
        {
            const size_t base = dst.b.size();
            dst.b.insert(dst.b.end(), src.b.begin() + ptrdiff_t(off), src.b.begin() + ptrdiff_t(off + n));
            if (src.pieces)
            {
                for (const auto& [so, zp] : src.segs)
                    if (so < off + n)
                    {
                        const size_t rel = std::max(so, off);
                        dst.segs.push_back({ base + rel - off, zp < 0 ? -1 : zp + int64_t(rel - so) });
                    }
            }
            else dst.segs.push_back({ base, src.ZoneAt(off) });
        }

        BufPtr Pieces()
        {
            auto b = std::make_shared<Buf>();
            b->pieces = true;
            return b;
        }
    }

    // ---- the walk -----------------------------------------------------------

    Reader::Reader(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> z)
        : s(schema), plat(plat), big(plat.big), zone(std::move(z))
    {
        blockSizes.assign(size_t(plat.blocks), 0);
        blockPos.assign(size_t(plat.blocks), 0);
        blockMax.assign(size_t(plat.blocks), 0);
        if (zone.size() >= plat.HeaderBytes())
            for (int k = 0; k < plat.blocks; k++) blockSizes[size_t(k)] = uint32_t(Load(zone.data() + 8 + 4 * k, 4, big));
        pos = plat.HeaderBytes();
        if (auto it = s.infos.find("XModelLodInfo"); it != s.infos.end()) lodInfo = it->second;
        if (auto it = s.infos.find("XModelSurfs"); it != s.infos.end()) surfsInfo = it->second;
    }

    std::string Reader::Path() const
    {
        std::string out;
        for (const std::string& p : path) out += (out.empty() ? "" : " > ") + p;
        return out;
    }

    uint32_t Reader::U32(const Buf& buf, size_t at) const
    {
        if (at + 4 > buf.b.size()) throw Error("a pointer past its struct at " + Path());
        return uint32_t(Load(buf.b.data() + at, 4, big));
    }

    std::pair<BufPtr, Where> Reader::Read(size_t n, const Desc& desc) { return ReadZone(n, desc); }

    std::pair<BufPtr, Where> Reader::ReadZone(size_t n, const Desc&)
    {
        auto data = std::make_shared<Buf>();
        if (block == kRuntime)
        {
            // Made by the loader at run time: nothing of it is in the file.
            if (n > kLargestRuntimeBlock) throw Error("the zone asks for " + std::to_string(n) + " bytes of run-time data at " + Path());
            data->b.assign(n, 0);
            data->segs.push_back({ 0, -1 });
        }
        else
        {
            if (n > zone.size() - pos) throw Error("the zone ends inside " + Path());
            data->b.assign(zone.begin() + ptrdiff_t(pos), zone.begin() + ptrdiff_t(pos + n));
            data->segs.push_back({ 0, int64_t(pos) });
            pos += n;
        }
        const Where where{ block, blockPos[size_t(block)] };
        blockPos[size_t(block)] += uint32_t(n);
        blockMax[size_t(block)] = std::max(blockMax[size_t(block)], blockPos[size_t(block)]);
        return { data, where };
    }

    void Reader::Alloc(uint32_t align)
    {
        uint32_t& p = blockPos[size_t(block)];
        p = (p + align - 1) / align * align;
    }

    void Reader::Push(int b)
    {
        stack.push_back({ blockPos[size_t(b)], block });
        block = b;
    }

    // The temp block is scratch space: what was pushed onto it is gone once popped.
    void Reader::Pop()
    {
        if (block == kTemp) blockPos[kTemp] = stack.back().first;
        block = stack.back().second;
        stack.pop_back();
    }

    void Reader::InsertPointer() { MakePointer(); }

    // The pointer the loader makes in the virtual block for data in the temp
    // block that something later points to.
    void Reader::MakePointer()
    {
        Push(kVirtual);
        Alloc(4);
        blockPos[kVirtual] += 4;
        blockMax[kVirtual] = std::max(blockMax[kVirtual], blockPos[kVirtual]);
        Pop();
    }

    Inst* Reader::NewInst(const StructInfo* info, BufPtr buf, size_t off, int64_t index, Where where)
    {
        insts.push_back({ info, std::move(buf), off, index, where });
        return &insts.back();
    }

    // ---- the rules' expressions --------------------------------------------

    int64_t Reader::Field(const Inst& inst, const std::vector<MemberInfo*>& chain, const std::vector<int64_t>& idx) const
    {
        size_t off = inst.off;
        for (size_t k = 0; k + 1 < chain.size(); k++) off += chain[k]->m->offset;
        const Member& m = *chain.back()->m;
        off += m.offset;
        if (m.IsPointer()) return U32(*inst.buf, off);
        const std::vector<int> dims = m.Dims();
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
        if (off + m.type->size > inst.buf->b.size()) throw Error("a rule reads past its struct: " + chain.back()->name);
        uint64_t v = Load(inst.buf->b.data() + off, m.type->size, big);
        int64_t value = int64_t(v);
        if (m.type->isSigned && m.type->size < 8 && (v >> (m.type->size * 8 - 1)) & 1)
            value = int64_t(v | (~uint64_t(0) << (m.type->size * 8)));
        if (m.type->isFloat)
        {
            if (m.type->size == 4) value = int64_t(LoadF32(inst.buf->b.data() + off, big));
            else
            {
                double d;
                std::memcpy(&d, &v, 8);
                value = int64_t(d);
            }
        }
        if (m.bits >= 0) value = (value >> m.bitpos) & ((int64_t(1) << m.bits) - 1);
        return value;
    }

    int64_t Reader::Lookup(const zone::ExprRef& ref, const std::vector<int64_t>& idx)
    {
        auto found = vars.find(ref.info);
        if (ref.isIndex)
        {
            if (found == vars.end()) throw Error("a rule needs the index of " + ref.info->name);
            return found->second->index;
        }
        // A model surfaces asset listed on its own (not reached through a model's
        // LOD entry) carries its surface count itself; an earlier model's is stale.
        if (ref.info == lodInfo && assetType == 3 && surfsInfo)
            if (auto own = vars.find(surfsInfo); own != vars.end()) found = own;
        if (found == vars.end()) throw Error("a rule needs " + ref.info->name + ", and none is loaded");
        const Inst& inst = *found->second;
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
        if (!idx.empty() && m.IsPointer())
        {
            // Indexing through a pointer: the data loaded for it.
            auto data = pointed.find(chain.back());
            if (data == pointed.end()) throw Error("a rule needs the data of " + chain.back()->name);
            const size_t at = size_t(idx[0]) * m.type->size;
            if (at + m.type->size > data->second->b.size()) throw Error("a rule reads past " + chain.back()->name);
            const uint64_t v = Load(data->second->b.data() + at, m.type->size, big);
            if (m.type->isSigned && m.type->size < 8 && (v >> (m.type->size * 8 - 1)) & 1)
                return int64_t(v | (~uint64_t(0) << (m.type->size * 8)));
            return int64_t(v);
        }
        return Field(inst, chain, idx);
    }

    int64_t Reader::Eval(const zone::Expr* e)
    {
        if (!e) throw Error("a count is missing from the rules at " + Path());
        if (e->constant) return *e->constant;
        return e->root->Eval([&](int r, const std::vector<int64_t>& idx) { return Lookup(e->refs[size_t(r)], idx); },
                             true);
    }

    // ---- structs ------------------------------------------------------------

    Inst* Reader::LoadStruct(const StructInfo* info, Inst* given, bool atStart)
    {
        path.push_back(info->name);
        const MemberInfo* dyn = info->dynamic;
        Inst* inst = given;
        if (atStart && !(info->isUnion && dyn))
        {
            // Up to a dynamic member, the struct streams at once.
            const size_t n = dyn ? dyn->m->offset : info->ctype->size;
            Desc desc{ dyn ? Desc::Partial : Desc::Type, info->ctype, 1 };
            auto [data, where] = Read(n, desc);
            if (dyn)
            {
                BufPtr whole = Pieces();
                Extend(*whole, *data, 0, data->b.size());
                data = whole;
            }
            inst = NewInst(info, data, 0, 0, where);
        }
        else if (atStart)
        {
            // A union whose members are only partly known: each streams itself.
            inst = NewInst(info, Pieces(), 0, 0, { block, blockPos[size_t(block)] });
        }
        vars[info] = inst;
        OnInst(inst, atStart);
        bool pushed = false;
        if (!info->asset.empty())
        {
            Push(kVirtual);
            pushed = true;
        }
        else if (info->block)
        {
            Push(plat.BlockOf(info->block->name));
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
        path.pop_back();
        return inst;
    }

    BufPtr Reader::LoadArray(const StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base)
    {
        if (count < 0) throw Error("a negative count of " + info->name + " at " + Path());
        Where where;
        if (atStart)
        {
            std::tie(buf, where) = Read(info->ctype->size * size_t(count), { Desc::Type, info->ctype, count });
            base = 0;
        }
        for (int64_t k = 0; k < count; k++)
        {
            Inst* e = NewInst(info, buf, base + size_t(k) * info->ctype->size, k, where);
            vars[info] = e;
            LoadStruct(info, e, false);
        }
        return buf;
    }

    // ---- members ------------------------------------------------------------

    size_t Reader::MemberLoc(const Inst& inst, const MemberInfo* mi, const std::vector<int32_t>& indices) const
    {
        const Member& m = *mi->m;
        std::vector<int64_t> dims;
        for (int x : m.mods)
        {
            if (x == zone::kPointer) break;
            dims.push_back(x);
        }
        const bool pointer = std::find(m.mods.begin(), m.mods.end(), zone::kPointer) != m.mods.end();
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

    void Reader::LoadReference(Inst* inst, const MemberInfo* mi, size_t modPos, std::vector<int32_t> indices)
    {
        const std::vector<int>& mods = mi->m->mods;
        const bool hasCur = modPos < mods.size(), hasNext = modPos + 1 < mods.size();
        const int cur = hasCur ? mods[modPos] : 0, nxt = hasNext ? mods[modPos + 1] : 0;
        const bool curPointer = hasCur && cur == zone::kPointer, curArray = hasCur && cur != zone::kPointer;
        int64_t combined = 0;
        if (!indices.empty())
        {
            const std::vector<int> dims = mi->m->Dims();
            for (size_t k = 0; k < indices.size(); k++)
            {
                int64_t st = 1;
                for (size_t d = k + 1; d < dims.size(); d++) st *= dims[d];
                combined += indices[k] * st;
            }
        }
        const bool followingPointer = HasPointerAfter(*mi->m, modPos);
        Kind kind;
        if (curArray && mi->arraySize.count(modPos)) kind = kDynamic;
        else if (curPointer && !mi->CountIsArray(modPos, combined) && !followingPointer) kind = kSingle;
        else if (curPointer && mi->CountIsArray(modPos, combined) && !followingPointer) kind = kArray;
        else if (((curPointer && mi->CountIsArray(modPos, combined)) || curArray) && hasNext &&
                 nxt == zone::kPointer && !mi->AnyCountIsArray(modPos + 1))
            kind = kPtrArray;
        else if (curArray && !hasNext) kind = kEmbArray;
        else if (!hasCur) kind = kEmbedded;
        else if (curArray)
        {
            for (int k = 0; k < cur; k++)
            {
                std::vector<int32_t> deeper = indices;
                deeper.push_back(k);
                LoadReference(inst, mi, modPos + 1, deeper);
            }
            return;
        }
        else throw Error("an unsupported member: " + mi->parent->name + "::" + mi->name);
        const size_t at = MemberLoc(*inst, mi, indices);
        path.push_back(Name(mi, indices, kind));
        ctx.push_back({ inst, mi, indices, kind });
        LoadBlock(inst, mi, modPos, combined, kind, at);
        ctx.pop_back();
        path.pop_back();
    }

    void Reader::LoadBlock(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t loc)
    {
        const zone::Block* b = mi->block;
        if (mi->blockCond.cond) b = Eval(mi->blockCond.cond) ? mi->blockCond.ifTrue : mi->blockCond.ifFalse;
        const bool push = b && !(b->kind == "normal" && b->isDefault);
        if (push) Push(plat.BlockOf(b->name));
        // A pointer that is null loads nothing (a single string's pointer is
        // looked at where the string loads).
        const bool check = (kind == kArray || kind == kPtrArray || kind == kSingle) &&
                           !(kind == kPtrArray && IsArrayAt(mi, modPos)) && !(kind == kSingle && mi->isString);
        if (!check) Reuse(inst, mi, modPos, combined, kind, loc, std::nullopt);
        else if (const uint32_t val = U32(*inst->buf, loc)) Reuse(inst, mi, modPos, combined, kind, loc, val);
        if (push) Pop();
    }

    void Reader::Reuse(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t loc,
                       std::optional<uint32_t> val)
    {
        const bool make = (kind == kArray || kind == kSingle || kind == kPtrArray) &&
                          !(kind == kPtrArray && IsArrayAt(mi, modPos));
        if (!make || !mi->isReusable) return AllocMember(inst, mi, modPos, combined, kind, loc, val);
        if (!val) val = U32(*inst->buf, loc);
        if (*val == kFollowing || (InTemp(mi) && *val == kInsert))
            return AllocMember(inst, mi, modPos, combined, kind, loc, val);
        // A pointer back to what was loaded before: nothing of it is in the stream here.
        Alias(*inst->buf, loc, *val);
    }

    void Reader::AllocMember(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                             size_t loc, std::optional<uint32_t> val)
    {
        bool make = kind == kArray || kind == kPtrArray || kind == kSingle;
        if (kind == kPtrArray) make = !IsArrayAt(mi, modPos);
        else if (mi->isString || IsAsset(mi)) make = false;
        if (make)
        {
            uint32_t align;
            if (mi->allocAlign) align = mi->allocAlign;
            else if (HasPointerAfter(*mi->m, modPos)) align = 4;
            else
            {
                align = mi->type ? mi->type->ctype->align : mi->m->type->align;
                if (mi->type && mi->type->allocAlign) align = mi->type->allocAlign;
            }
            Alloc(align);
            if (!val) val = U32(*inst->buf, loc);
            if (InTemp(mi) && *val == kInsert) InsertPointer();
        }
        TypeCheck(inst, mi, modPos, combined, kind, loc);
    }

    size_t Reader::ElemSize(const MemberInfo* mi, size_t modPos) const
    {
        size_t n = mi->m->type->size;
        for (size_t k = modPos + 1; k < mi->m->mods.size(); k++)
        {
            if (mi->m->mods[k] == zone::kPointer) return 4;
            n *= size_t(mi->m->mods[k]);
        }
        return n;
    }

    Desc Reader::ElemDesc(const MemberInfo* mi, size_t modPos, int64_t n) const
    {
        int64_t k = 1;
        for (size_t d = modPos + 1; d < mi->m->mods.size(); d++)
        {
            if (mi->m->mods[d] == zone::kPointer) return { Desc::Ptrs, nullptr, n * k };
            k *= mi->m->mods[d];
        }
        return { Desc::Type, mi->m->type, n * k };
    }

    void Reader::TypeCheck(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t loc)
    {
        const std::vector<int>& mods = mi->m->mods;
        const bool inRuntime = mi->block && mi->block->kind == "runtime";
        if (mi->isString)
        {
            if (kind == kSingle) LoadXString(*inst->buf, loc);
            else if (kind == kPtrArray)
            {
                if (mods[modPos] != zone::kPointer) LoadXStringArray(false, mods[modPos], inst->buf, loc);
                else LoadXStringArray(true, Eval(mi->Count(modPos, combined)), {}, 0);
            }
            return;
        }
        if (IsAsset(mi))
        {
            if (kind == kSingle) LoadAssetPtr(mi->type, *inst->buf, loc);
            else if (kind == kPtrArray) PtrArray(inst, mi, modPos, combined, loc);
            return;
        }
        const StructInfo* t = mi->type;
        switch (kind)
        {
        case kArray:
        {
            const int64_t n = Eval(mi->Count(modPos, combined));
            if (n < 0) throw Error("a negative count at " + Path());
            if (t && !t->isLeaf && !inRuntime) LoadArray(t, true, n, {}, 0);
            else pointed[mi] = Read(ElemSize(mi, modPos) * size_t(n), ElemDesc(mi, modPos, n)).first;
            break;
        }
        case kSingle:
            if (t && !t->isLeaf && !inRuntime) LoadStruct(t, nullptr, true);
            else Read(ElemSize(mi, modPos), ElemDesc(mi, modPos, 1));
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
                if (after) LoadArray(t, true, n, {}, 0);
                else LoadArray(t, false, n, inst->buf, loc);
            }
            else if (after)
            {
                const Type* et = t ? t->ctype : mi->m->type;
                Read(et->size * size_t(n), { Desc::Type, et, n });
            }
            break;
        }
        case kEmbedded:
        {
            const bool after = s.AfterPartialLoad(mi);
            if (!mi->isLeaf && t)
            {
                if (after)
                {
                    Inst* sub = LoadStruct(t, nullptr, true);
                    if (inst->buf->pieces && sub)
                    {
                        if (sub->buf->pieces) Extend(*inst->buf, *sub->buf, 0, sub->buf->b.size());
                        else Extend(*inst->buf, *sub->buf, sub->off, t->ctype->size);
                    }
                }
                else
                {
                    // A struct wrapping another at offset 0 (GfxCellTree128 around
                    // GfxCellTree) keeps its array index, for counts written as
                    // pointer differences.
                    const int64_t index = loc == inst->off ? inst->index : 0;
                    LoadStruct(t, NewInst(t, inst->buf, loc, index, inst->where), false);
                }
            }
            else if (after)
            {
                const Type* et = t ? t->ctype : mi->m->type;
                auto [data, where] = Read(et->size, { Desc::Type, et, 1 });
                if (inst->buf->pieces) Extend(*inst->buf, *data, 0, data->b.size());
            }
            break;
        }
        case kDynamic:
        {
            const int64_t n = Eval(mi->arraySize.at(modPos));
            if (n < 0) throw Error("a negative size at " + Path());
            BufPtr buf;
            if (t && !t->isLeaf) buf = LoadArray(t, true, n, {}, 0);
            else
            {
                size_t elem = mi->m->type->size;
                for (size_t k = 1; k < mods.size(); k++) elem *= size_t(mods[k]);
                buf = Read(elem * size_t(n), ElemDesc(mi, 0, n)).first;
            }
            if (inst->buf->pieces) Extend(*inst->buf, *buf, 0, buf->b.size());
            break;
        }
        }
    }

    void Reader::PtrArray(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, size_t loc)
    {
        const int cur = mi->m->mods[modPos];
        if (cur != zone::kPointer) LoadPtrArray(mi, false, cur, inst->buf, loc);
        else LoadPtrArray(mi, true, Eval(mi->Count(modPos, combined)), {}, 0);
    }

    void Reader::LoadPtrArray(const MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t loc)
    {
        if (count < 0) throw Error("a negative count at " + Path());
        if (atStart)
        {
            buf = Read(4 * size_t(count), { Desc::Ptrs, nullptr, count }).first;
            loc = 0;
        }
        const StructInfo* t = mi->type;
        for (int64_t k = 0; k < count; k++)
        {
            const size_t at = loc + 4 * size_t(k);
            const uint32_t val = U32(*buf, at);
            if (!val) continue;
            if (t && !t->asset.empty())
            {
                LoadAssetPtr(t, *buf, at);
                continue;
            }
            if (mi->isReusable && val != kFollowing)
            {
                Alias(*buf, at, val);
                continue;
            }
            Alloc(t ? AlignOf(t) : mi->m->type->align);
            if (t && !t->isLeaf) LoadStruct(t, nullptr, true);
            else
            {
                const Type* et = t ? t->ctype : mi->m->type;
                Read(et->size, { Desc::Type, et, 1 });
            }
        }
    }

    // ---- strings and assets -------------------------------------------------

    void Reader::LoadXString(Buf& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        if (!val) return;
        if (val != kFollowing)
        {
            Alias(buf, loc, val);
            return;
        }
        auto end = std::find(zone.begin() + ptrdiff_t(pos), zone.end(), uint8_t(0));
        if (end == zone.end()) throw Error("a string runs off the end of the zone");
        Read(size_t(end - zone.begin()) + 1 - pos, { Desc::String });
    }

    void Reader::LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t loc)
    {
        if (count < 0) throw Error("a negative count at " + Path());
        if (atStart)
        {
            buf = Read(4 * size_t(count), { Desc::Ptrs, nullptr, count }).first;
            loc = 0;
        }
        for (int64_t k = 0; k < count; k++) LoadXString(*buf, loc + 4 * size_t(k));
    }

    Inst* Reader::LoadAssetPtr(const StructInfo* info, Buf& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        const bool inTemp = info->block && info->block->kind == "temp";
        if (inTemp) Push(kTemp);
        Inst* inst = nullptr;
        if (val)
        {
            if (val == kFollowing || (inTemp && val == kInsert))
            {
                Alloc(AlignOf(info));
                if (inTemp && val == kInsert) InsertPointer();
                inst = LoadStruct(info, nullptr, true);
            }
            else Alias(buf, loc, val);
        }
        if (inTemp) Pop();
        return inst;
    }

    // ---- where a member's struct is in its owner's node ---------------------

    Struct* MemberStruct(Struct* parent, const StructInfo* parentInfo, const Member& m, const std::vector<int32_t>& idx,
                         int64_t k)
    {
        // A typedef's one member is the typedef itself.
        if (parentInfo->ctype->kind == Type::Typedef) return parent;
        // A union notes which of its members a walk goes into: that is the one in use.
        if (parent->type->kind == Type::Union &&
            std::find(parent->used.begin(), parent->used.end(), std::string_view(m.name)) == parent->used.end())
            parent->used.push_back(m.name);
        std::vector<int32_t> full = idx;
        if (k >= 0) full.push_back(int32_t(k));
        if (full.empty()) return parent->Sub(m);
        const std::vector<int> dims = m.Dims();
        int64_t flat = 0;
        for (size_t d = 0; d < full.size(); d++)
        {
            int64_t stride = 1;
            for (size_t e = d + 1; e < dims.size(); e++) stride *= dims[e];
            flat += full[d] * stride;
        }
        return parent->Sub(m, flat);
    }

    // ---- the tree reader ----------------------------------------------------

    TreeReader::TreeReader(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> zone, bool lenient)
        : Reader(schema, plat, std::move(zone)), lenient(lenient)
    {
    }

    std::pair<BufPtr, Where> TreeReader::Read(size_t n, const Desc& desc)
    {
        auto [data, where] = ReadZone(n, desc);
        if (desc.kind == Desc::Type)
        {
            // Plain data, if that is what is loading: made into a Leaf only
            // when a load keeps it (most typed reads are structs).
            cap.reset();
            leafData = data;
            leafDesc = desc;
            capWhere = where;
        }
        return { data, where };
    }

    void TreeReader::Alias(Buf&, size_t, uint32_t val)
    {
        Ref* r = A().Make<Ref>(val);
        refs.push_back(r);
        SetCap(r);
    }

    void TreeReader::SetCap(std::optional<Node*> v)
    {
        cap = v;
        leafData.reset();
    }

    std::optional<Node*> TreeReader::Made()
    {
        if (leafData)
        {
            cap = A().Make<Leaf>(leafDesc.t, leafDesc.count, leafData->b, big);
            leafData.reset();
        }
        return cap;
    }

    void TreeReader::OnInst(Inst* inst, bool atStart)
    {
        if (reg.count(inst)) return;
        if (atStart)
        {
            if (inst->buf->pieces && inst->buf->b.empty())
            {
                if (!lenient)
                    throw Error(inst->info->name + ": unions streamed member by member aren't supported yet");
                // Only for files looked things up in, never written (an animation's
                // translation data): the walk stays in step, the union isn't kept.
                reg[inst] = A().Make<Struct>(inst->info->ctype, big, std::vector<uint8_t>());
                return;
            }
            std::vector<uint8_t> raw(inst->buf->b.begin() + ptrdiff_t(inst->off), inst->buf->b.end());
            const uint32_t n = uint32_t(raw.size());
            Struct* node = A().Make<Struct>(inst->info->ctype, big, std::move(raw));
            placed.push_back({ inst->where.block, inst->where.pos, n, node, { inst->info->ctype, 0 } });
            reg[inst] = node;
            return;
        }
        const Ctx& c = ctx.back();
        reg[inst] = MemberStruct(reg.at(c.inst), c.inst->info, *c.mi->m, c.idx);
    }

    Inst* TreeReader::LoadStruct(const StructInfo* info, Inst* inst, bool atStart)
    {
        inst = Reader::LoadStruct(info, inst, atStart);
        if (atStart) SetCap(reg.at(inst));
        return inst;
    }

    BufPtr TreeReader::LoadArray(const StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base)
    {
        const size_t size = info->ctype->size;
        Where where;
        Array* list = nullptr;
        if (atStart)
        {
            std::tie(buf, where) = ReadZone(size * size_t(count), { Desc::Type, info->ctype, count });
            base = 0;
            list = A().Make<Array>();
            list->items.reserve(size_t(count));
            for (int64_t k = 0; k < count; k++)
                list->items.push_back(A().Make<Struct>(
                    info->ctype, big,
                    std::vector<uint8_t>(buf->b.begin() + ptrdiff_t(size_t(k) * size), buf->b.begin() + ptrdiff_t(size_t(k + 1) * size))));
            placed.push_back({ where.block, where.pos, uint32_t(size * size_t(count)), list, { info->ctype, 0 } });
        }
        const Ctx* c = atStart ? nullptr : &ctx.back();
        for (int64_t k = 0; k < count; k++)
        {
            Inst* e = NewInst(info, buf, base + size_t(k) * size, k, where);
            reg[e] = list ? static_cast<Struct*>(list->items[size_t(k)])
                          : MemberStruct(reg.at(c->inst), c->inst->info, *c->mi->m, c->idx, k);
            vars[info] = e;
            Reader::LoadStruct(info, e, false);
        }
        if (list) SetCap(list);
        else SetCap(std::nullopt);
        return buf;
    }

    void TreeReader::LoadBlock(Inst* inst, const MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t at)
    {
        SetCap(std::nullopt);
        const size_t depth = memberSlots.size();
        Reader::LoadBlock(inst, mi, modPos, combined, kind, at);
        std::optional<Node*> made = Made();
        SetCap(std::nullopt);
        if (memberSlots.size() > depth)
        {
            // This member had the loader make a pointer to what it holds
            // (XModelLodInfo.modelSurfs): later pointers to it point there.
            const uint32_t s = memberSlots.back();
            memberSlots.pop_back();
            if (made)
            {
                Slot* mark = A().Make<Slot>(*made);
                if (Struct* d = As<Struct>(*made)) d->slot = mark;
                placed.push_back({ kVirtual, s, 4, mark, { nullptr, 4 } });
            }
        }
        if (!made) return;
        if (Leaf* lf = As<Leaf>(*made))
            placed.push_back({ capWhere.block, capWhere.pos, uint32_t(lf->raw.size()), lf, { lf->t, 0 } });
        Struct* d = reg.at(inst);
        const std::vector<int32_t>& idx = ctx.back().idx;
        if (kind == kEmbedded || kind == kEmbArray || kind == kDynamic)
        {
            if (!d->Lates().size() && d->Ch() && !d->Ch()->Empty()) d->latesAfterPointers = true;
            d->SetLate(mi->m->name, *made, idx);
        }
        else d->SetChild(mi->m->name, *made, idx);
    }

    void TreeReader::LoadPtrArray(const MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t loc)
    {
        Where where;
        if (atStart)
        {
            std::tie(buf, where) = ReadZone(4 * size_t(count), { Desc::Ptrs, nullptr, count });
            loc = 0;
        }
        PtrList* pl = A().Make<PtrList>();
        pl->raw.emplace();
        for (int64_t k = 0; k < count; k++) pl->raw->push_back(U32(*buf, loc + 4 * size_t(k)));
        if (atStart) placed.push_back({ where.block, where.pos, uint32_t(4 * count), pl, { nullptr, 4 } });
        const StructInfo* t = mi->type;
        for (int64_t k = 0; k < count; k++)
        {
            const size_t at = loc + 4 * size_t(k);
            const uint32_t val = U32(*buf, at);
            if (!val)
            {
                pl->items.push_back(nullptr);
                continue;
            }
            if (t && !t->asset.empty())
            {
                SetCap(std::nullopt);
                LoadAssetPtr(t, *buf, at);
                pl->items.push_back(Made().value_or(nullptr));
                continue;
            }
            if (mi->isReusable && val != kFollowing)
            {
                Alias(*buf, at, val);
                pl->items.push_back(*Made());
                continue;
            }
            Alloc(t ? AlignOf(t) : mi->m->type->align);
            if (t && !t->isLeaf)
            {
                Inst* inst = LoadStruct(t, nullptr, true);
                pl->items.push_back(reg.at(inst));
            }
            else
            {
                const Type* et = t ? t->ctype : mi->m->type;
                Read(et->size, { Desc::Type, et, 1 });
                Leaf* lf = As<Leaf>(*Made());
                placed.push_back({ capWhere.block, capWhere.pos, uint32_t(lf->raw.size()), lf, { et, 0 } });
                pl->items.push_back(lf);
            }
        }
        SetCap(pl);
    }

    void TreeReader::LoadXString(Buf& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        if (!val)
        {
            SetCap(std::nullopt);
            return;
        }
        if (val != kFollowing)
        {
            Alias(buf, loc, val);
            return;
        }
        auto end = std::find(zone.begin() + ptrdiff_t(pos), zone.end(), uint8_t(0));
        if (end == zone.end()) throw Error("a string runs off the end of the zone");
        auto [data, where] = ReadZone(size_t(end - zone.begin()) + 1 - pos, { Desc::String });
        String* st = A().Make<String>(std::string(data->b.begin(), data->b.end() - 1));
        placed.push_back({ where.block, where.pos, uint32_t(data->b.size()), st, { nullptr, 1 } });
        SetCap(st);
    }

    void TreeReader::LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t loc)
    {
        Where where;
        if (atStart)
        {
            std::tie(buf, where) = ReadZone(4 * size_t(count), { Desc::Ptrs, nullptr, count });
            loc = 0;
        }
        PtrList* pl = A().Make<PtrList>();
        pl->raw.emplace();
        for (int64_t k = 0; k < count; k++) pl->raw->push_back(U32(*buf, loc + 4 * size_t(k)));
        if (atStart) placed.push_back({ where.block, where.pos, uint32_t(4 * count), pl, { nullptr, 4 } });
        for (int64_t k = 0; k < count; k++)
        {
            LoadXString(*buf, loc + 4 * size_t(k));
            pl->items.push_back(Made().value_or(nullptr));
        }
        SetCap(pl);
    }

    void TreeReader::InsertPointer()
    {
        Push(kVirtual);
        Alloc(4);
        slot = blockPos[kVirtual];
        Pop();
        MakePointer();
        if (!assetInsert) memberSlots.push_back(slot);
    }

    Inst* TreeReader::LoadAssetPtr(const StructInfo* info, Buf& buf, size_t loc)
    {
        const uint32_t val = U32(buf, loc);
        const bool inTemp = info->block && info->block->kind == "temp";
        if (inTemp) Push(kTemp);
        Inst* inst = nullptr;
        std::optional<Node*> res;
        if (val)
        {
            if (val == kFollowing || (inTemp && val == kInsert))
            {
                Alloc(AlignOf(info));
                std::optional<uint32_t> s;
                if (inTemp && val == kInsert)
                {
                    assetInsert = true;
                    InsertPointer();
                    assetInsert = false;
                    s = slot;
                }
                const size_t start = pos;
                inst = LoadStruct(info, nullptr, true);
                Struct* d = reg.at(inst);
                assetStarts.push_back({ d, start });
                d->asset = info->name;
                if (s)
                {
                    d->slot = A().Make<Slot>(d);
                    placed.push_back({ kVirtual, *s, 4, d->slot, { nullptr, 4 } });
                }
                res = d;
            }
            else
            {
                Alias(buf, loc, val);
                res = Made();
            }
        }
        if (inTemp) Pop();
        SetCap(res);
        return inst;
    }

    Tree TreeReader::Walk()
    {
        Tree tree;
        tree.platform = plat.name;
        if (zone.size() < plat.HeaderBytes() + 16) throw Error("the zone is cut short");
        tree.header.assign(zone.begin(), zone.begin() + ptrdiff_t(plat.HeaderBytes()));
        const uint32_t ssCount = uint32_t(Load(zone.data() + pos, 4, big));
        const uint32_t ssPtr = uint32_t(Load(zone.data() + pos + 4, 4, big));
        const uint32_t count = uint32_t(Load(zone.data() + pos + 8, 4, big));
        const uint32_t list = uint32_t(Load(zone.data() + pos + 12, 4, big));
        pos += 16;
        Push(kVirtual);
        if (ssPtr)
        {
            Alloc(4);
            LoadXStringArray(true, ssCount, {}, 0);
            tree.scriptStrings = As<PtrList>(*Made());
        }
        Pop();
        Push(kVirtual);
        if (list)
        {
            Alloc(4);
            auto [buf, where] = ReadZone(8 * size_t(count), { Desc::U32, nullptr, 2 * int64_t(count) });
            std::vector<uint32_t> types;
            for (uint32_t k = 0; k < count; k++)
            {
                types.push_back(U32(*buf, 8 * size_t(k)));
                if (types.back() >= plat.types.size())
                    throw Error("asset " + std::to_string(k) + " has an unknown type " + std::to_string(types.back()));
                Entry* e = A().Make<Entry>(plat.types[types.back()], nullptr);
                tree.assets.push_back(e);
                placed.push_back({ where.block, where.pos + 8 * k, 8, e, { nullptr, 8 } });
            }
            for (uint32_t k = 0; k < count; k++)
            {
                auto name = plat.structs.find(types[k]);
                if (name == plat.structs.end())
                    throw Error("asset " + std::to_string(k) + " has an unsupported type (" + plat.types[types[k]] + ")");
                const StructInfo* info = s.assets.at(name->second);
                assetType = int(types[k]);
                SetCap(std::nullopt);
                LoadAssetPtr(info, *buf, 8 * size_t(k) + 4);
                tree.assets[k]->asset = Made().value_or(nullptr);
            }
        }
        Pop();
        Resolve();
        return tree;
    }

    // Every alias pointed at the object it refers into.
    void TreeReader::Resolve()
    {
        std::map<int, std::vector<const Placed*>> byBlock;
        for (const Placed& p : placed)
            if (p.n) byBlock[p.block].push_back(&p);
        for (auto& [b, rows] : byBlock)
            std::stable_sort(rows.begin(), rows.end(), [](const Placed* x, const Placed* y) { return x->off < y->off; });
        for (Ref* r : refs)
        {
            const uint32_t raw = r->val - 1;
            const int b = int(raw >> kBlockShift);
            const uint32_t off = raw & kOffsetMask;
            auto found = byBlock.find(b);
            if (found == byBlock.end()) continue;
            const auto& rows = found->second;
            auto it = std::upper_bound(rows.begin(), rows.end(), off, [](uint32_t v, const Placed* p) { return v < p->off; });
            int64_t i = int64_t(it - rows.begin()) - 1;
            // Several things can start at one place (a struct and its first
            // member's array): the last one that holds the offset.
            while (i >= 0)
            {
                const Placed& p = *rows[size_t(i)];
                if (p.off <= off && off < p.off + p.n)
                {
                    r->target = p.node;
                    r->rel = off - p.off;
                    r->t = p.t;
                    break;
                }
                if (p.off + p.n <= off && i > 0 && rows[size_t(i - 1)]->off + rows[size_t(i - 1)]->n <= p.off) break;
                i--;
            }
        }
    }

    Tree ReadTree(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> zone, bool lenient,
                  std::vector<std::pair<Struct*, size_t>>* assetStarts)
    {
        TreeReader r(schema, plat, std::move(zone), lenient);
        Tree tree = r.Walk();
        if (r.pos != r.zone.size())
            throw Error("the walk stopped at " + std::to_string(r.pos) + " of " + std::to_string(r.zone.size()) + " bytes");
        if (assetStarts) *assetStarts = std::move(r.assetStarts);
        return tree;
    }

    // ---- what aliases name --------------------------------------------------

    Node* SlotAsset(const Ref* r)
    {
        Node* tgt = r->target;
        int64_t rel = r->rel;
        if (Entry* e = As<Entry>(tgt)) return rel == 4 ? e->asset : nullptr;
        if (Slot* s = As<Slot>(tgt)) return s->asset;
        // (A union of pointers too: an effect element's visuals "the same as that
        // element's" point at the other element's union, whose pointer holds the asset.)
        const Type* t = r->t.t;
        if (!t || (t->kind != Type::Struct && t->kind != Type::Union)) return nullptr;
        if (Array* list = As<Array>(tgt))
        {
            const size_t i = size_t(rel / t->size);
            rel %= t->size;
            if (i >= list->items.size()) return nullptr;
            tgt = list->items[i];
        }
        Struct* d = As<Struct>(tgt);
        if (!d) return nullptr;
        for (const Member& m : t->members)
            if (int64_t(m.offset) == rel && m.IsPointer())
            {
                Node* c = d->Child(m.name);
                if ((As<Struct>(c) && As<Struct>(c)->IsAsset()) || As<Ref>(c)) return c;
            }
        return nullptr;
    }
}
