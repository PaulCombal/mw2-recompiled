#pragma once
// Walks every asset of a decompressed 360 zone in the order the game's loader
// streams it, so each byte of the zone is reached as what it is.
//
// The loader also keeps a position in each memory block: a pointer back to
// earlier data is that block and position, plus one. They are kept here the
// way the loader keeps them, so a caller can tell what such a pointer names.
// A walk that ends anywhere but at the zone's last byte, or with a block not
// at the size the zone's header gives it, has misread something, and says so.
#include "schema.h"

#include <functional>

namespace zone
{
    // Bytes a struct is read from: a view of the zone, or (for a struct put
    // together from pieces, and for runtime-block data, which is not in the
    // file) bytes of its own.
    struct Buffer
    {
        const uint8_t* view = nullptr;
        size_t viewSize = 0;
        std::shared_ptr<std::vector<uint8_t>> own;
        bool growable = false;

        const uint8_t* Data() const { return own ? own->data() : view; }
        size_t Size() const { return own ? own->size() : viewSize; }
    };

    // Where data was put: its memory block and the position in it, and where
    // its bytes are in the zone (none for the runtime block, made at run time).
    struct Place
    {
        int block = 0;
        uint32_t blockPos = 0;
        size_t zonePos = 0;
        bool inZone = false;
        // What a pointer to it holds: block << 28 | position, plus one.
        uint32_t Pointer() const { return (uint32_t(block) << 28 | blockPos) + 1; }
    };

    class Walker
    {
    public:
        Walker(const Schema& schema, const uint8_t* zone, size_t size);

        // Called with the data of every leaf array or single leaf a pointer
        // streams: the member it is, where it went, and how many bytes.
        std::function<void(const MemberInfo& member, const Place& place, size_t bytes)> onData;
        // Called with each pointer as it is met, before what it names loads:
        // the member, the element of a pointer array (-1 for a single one) and
        // the value -- 0xFFFFFFFF when the data follows in the stream.
        std::function<void(const MemberInfo& member, int64_t index, uint32_t value)> onPointer;

        // Throws zone::Error when the zone is not what the rules describe.
        void Walk();

        size_t Position() const { return pos; }
        uint32_t AssetCount() const { return assetCount; }

    private:
        struct Inst
        {
            const StructInfo* info = nullptr;
            Buffer buf;
            size_t off = 0;
            int64_t index = 0;
        };

        enum BlockId { kTemp, kPhysical, kRuntime, kVirtual, kLarge, kCallback };

        const Schema& s;
        const uint8_t* zone;
        size_t size;
        size_t pos = 0;
        int block = kTemp;
        uint32_t blockPos[6]{};
        std::vector<std::pair<uint32_t, int>> stack;   // the pushed block's position, the block before
        uint32_t headerSizes[6]{};
        Place last;   // of the last read
        std::unordered_map<const StructInfo*, Inst> vars;
        std::unordered_map<const MemberInfo*, Buffer> pointed;
        uint32_t assetType = ~0u, assetCount = 0;
        const StructInfo* lodInfo = nullptr;
        const StructInfo* surfs = nullptr;

        Buffer Read(size_t n);
        void Push(int b);
        void Pop();
        void Alloc(uint32_t align);
        void InsertPointer();
        void AllocMember(const Inst& inst, const MemberInfo* mi, size_t modPos, int kind, size_t loc);
        static int BlockOf(const Block* b);
        uint32_t U32(const Buffer& buf, size_t at) const;
        int64_t Field(const Inst& inst, const std::vector<MemberInfo*>& chain, const std::vector<int64_t>& idx) const;
        int64_t Lookup(const ExprRef& ref, const std::vector<int64_t>& idx) const;
        int64_t Eval(const Expr* e) const;

        Inst LoadStruct(const StructInfo* info, const Inst* inst, bool atStart);
        Buffer LoadArray(const StructInfo* info, bool atStart, int64_t count, Buffer buf = {}, size_t base = 0);
        size_t MemberLoc(const Inst& inst, const MemberInfo* mi, const std::vector<int64_t>& indices) const;
        void LoadReference(const Inst& inst, const MemberInfo* mi, size_t modPos, std::vector<int64_t> indices);
        void LoadBlock(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, int kind, size_t loc);
        void TypeCheck(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, int kind, size_t loc);
        void PtrArray(const Inst& inst, const MemberInfo* mi, size_t modPos, int64_t combined, size_t loc);
        void LoadPtrArray(const MemberInfo* mi, bool atStart, int64_t count, Buffer buf = {}, size_t loc = 0);
        void LoadXString(const Buffer& buf, size_t loc);
        void LoadXStringArray(bool atStart, int64_t count, Buffer buf = {}, size_t loc = 0);
        void LoadAssetPtr(const StructInfo* info, const Buffer& buf, size_t loc);
        size_t ElemSize(const MemberInfo* mi, size_t modPos) const;
        static void Append(Buffer& dst, const Buffer& src, size_t off, size_t n);
    };
}
