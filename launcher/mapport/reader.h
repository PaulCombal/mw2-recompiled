#pragma once
// A zone read into a tree (tree.h), and a tree written back out as a zone.
//
// Reader walks a zone the way the game's loader streams it (as
// launcher/zone/walker.h does for the shader pass), with a hook at every
// step. TreeReader keeps every struct it meets as a node, and what each
// pointer loaded under it. TreeWriter walks a platform's rules over such a
// tree and streams the zone the loader would read it from: every count,
// block and alignment comes out right, and pointers back to earlier data are
// worked out anew from where their targets were written. Written with the
// 360's rules, after the converter has turned the PC's structs into the
// 360's, it converts the zone.
#include "tree.h"

#include <array>
#include <map>

namespace mapport
{
    enum Block : int { kTemp, kPhysical, kRuntime, kVirtual, kLarge, kCallback, kVertex, kIndex };
    constexpr int kBlockShift = 28;
    constexpr uint32_t kOffsetMask = (1u << kBlockShift) - 1;

    // What a platform's zones are: their byte order, the memory blocks their
    // header lists (the PC's eight, the 360's six), their asset types.
    struct Platform
    {
        std::string name;
        bool big;
        int blocks;
        std::vector<std::string> types;                // asset type number -> its name
        std::map<uint32_t, std::string> structs;       // asset type number -> the rules' asset name
        bool vertexBlocks;                             // vertex and index data have blocks of their own
        size_t HeaderBytes() const { return 8 + 4 * size_t(blocks); }
        int BlockOf(const std::string& rulesName) const;
        uint32_t TypeNumber(const std::string& typeName) const;
    };
    const Platform& Xbox();
    const Platform& Pc();

    // Bytes the walk streamed (or, for the runtime block, made), and where
    // each piece of them is in the zone: a struct can be put together from
    // several pieces.
    struct Buf
    {
        std::vector<uint8_t> b;
        std::vector<std::pair<size_t, int64_t>> segs;   // (offset in b, zone position or -1)
        bool pieces = false;                            // put together from pieces
        int64_t ZoneAt(size_t loc) const;
    };
    using BufPtr = std::shared_ptr<Buf>;

    struct Where
    {
        int block = 0;
        uint32_t pos = 0;
    };

    struct Inst
    {
        const zone::StructInfo* info = nullptr;
        BufPtr buf;
        size_t off = 0;
        int64_t index = 0;
        Where where;
    };

    // What a read streams.
    struct Desc
    {
        enum Kind { Type, Partial, Ptrs, U32, String, Raw } kind = Raw;
        const zone::Type* t = nullptr;
        int64_t count = 0;
    };

    class Reader
    {
    public:
        Reader(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> zone);
        virtual ~Reader() = default;

        const zone::Schema& s;
        const Platform& plat;
        const bool big;
        std::vector<uint8_t> zone;
        size_t pos = 0;

        enum Kind { kDynamic, kSingle, kArray, kPtrArray, kEmbArray, kEmbedded };

    protected:
        // ---- the stream and the blocks -------------------------------------
        virtual std::pair<BufPtr, Where> Read(size_t n, const Desc& desc);
        std::pair<BufPtr, Where> ReadZone(size_t n, const Desc& desc);
        void Alloc(uint32_t align);
        void Push(int b);
        void Pop();
        virtual void InsertPointer();
        void MakePointer();
        virtual void Alias(Buf& buf, size_t loc, uint32_t val) {}
        uint32_t U32(const Buf& buf, size_t at) const;

        // ---- the load, as the game's Load_X functions do it ----------------
        virtual void OnInst(Inst* inst, bool atStart) {}
        virtual Inst* LoadStruct(const zone::StructInfo* info, Inst* inst, bool atStart);
        virtual BufPtr LoadArray(const zone::StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base);
        void LoadReference(Inst* inst, const zone::MemberInfo* mi, size_t modPos, std::vector<int32_t> indices);
        virtual void LoadBlock(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                               size_t loc);
        virtual void Reuse(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                           size_t loc, std::optional<uint32_t> val);
        void AllocMember(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                         size_t loc, std::optional<uint32_t> val);
        virtual void LoadPtrArray(const zone::MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t loc);
        virtual void LoadXString(Buf& buf, size_t loc);
        virtual void LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t loc);
        virtual Inst* LoadAssetPtr(const zone::StructInfo* info, Buf& buf, size_t loc);

        static uint32_t AlignOf(const zone::StructInfo* info)
        {
            return info->allocAlign ? info->allocAlign : info->ctype->align;
        }
        static bool IsAsset(const zone::MemberInfo* mi) { return mi->type && !mi->type->asset.empty(); }
        static bool InTemp(const zone::MemberInfo* mi) { return mi->block && mi->block->kind == "temp"; }
        static bool IsArrayAt(const zone::MemberInfo* mi, size_t modPos)
        {
            return modPos < mi->m->mods.size() && mi->m->mods[modPos] != zone::kPointer;
        }
        int64_t Eval(const zone::Expr* e);

        // The struct whose member is being loaded, the member, its element's
        // indices and the kind of load.
        struct Ctx
        {
            Inst* inst;
            const zone::MemberInfo* mi;
            std::vector<int32_t> idx;
            Kind kind;
        };
        std::vector<Ctx> ctx;
        Inst* NewInst(const zone::StructInfo* info, BufPtr buf, size_t off, int64_t index, Where where);
        std::string Path() const;

        std::vector<uint32_t> blockSizes, blockPos, blockMax;
        int block = kTemp;
        int assetType = -1;   // of the asset being loaded
        // The instance of each struct being loaded, for the rules' expressions.
        std::unordered_map<const zone::StructInfo*, Inst*> vars;

    private:
        void TypeCheck(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t loc);
        void PtrArray(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, size_t loc);
        int64_t Lookup(const zone::ExprRef& ref, const std::vector<int64_t>& idx);
        int64_t Field(const Inst& inst, const std::vector<zone::MemberInfo*>& chain, const std::vector<int64_t>& idx) const;
        size_t MemberLoc(const Inst& inst, const zone::MemberInfo* mi, const std::vector<int32_t>& indices) const;
        size_t ElemSize(const zone::MemberInfo* mi, size_t modPos) const;
        Desc ElemDesc(const zone::MemberInfo* mi, size_t modPos, int64_t n) const;

        std::vector<std::pair<uint32_t, int>> stack;
        std::unordered_map<const zone::MemberInfo*, BufPtr> pointed;
        std::deque<Inst> insts;
        std::vector<std::string> path;
        const zone::StructInfo* lodInfo = nullptr;
        const zone::StructInfo* surfsInfo = nullptr;
    };

    // The struct an instance being loaded inside another one is: the member
    // `m` (at `idx`) of `parent`, or element `k` of that member's array.
    Struct* MemberStruct(Struct* parent, const zone::StructInfo* parentInfo, const zone::Member& m,
                         const std::vector<int32_t>& idx, int64_t k = -1);

    // ---- reading a tree -----------------------------------------------------

    class TreeReader : public Reader
    {
    public:
        // `lenient`: a union streamed member by member (an animation's data,
        // in a file only looked things up in) is passed over, not kept.
        TreeReader(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> zone, bool lenient = false);

        Tree Walk();
        // Every asset loaded from the asset list or a pointer: (its node,
        // where it starts in the zone).
        std::vector<std::pair<Struct*, size_t>> assetStarts;

    protected:
        std::pair<BufPtr, Where> Read(size_t n, const Desc& desc) override;
        void Alias(Buf& buf, size_t loc, uint32_t val) override;
        void OnInst(Inst* inst, bool atStart) override;
        Inst* LoadStruct(const zone::StructInfo* info, Inst* inst, bool atStart) override;
        BufPtr LoadArray(const zone::StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base) override;
        void LoadBlock(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                       size_t loc) override;
        void LoadPtrArray(const zone::MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t loc) override;
        void LoadXString(Buf& buf, size_t loc) override;
        void LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t loc) override;
        void InsertPointer() override;
        Inst* LoadAssetPtr(const zone::StructInfo* info, Buf& buf, size_t loc) override;

    private:
        // A piece of a block the walk filled, and the node it holds.
        struct Placed
        {
            int block;
            uint32_t off, n;
            Node* node;
            ElemType t;
        };
        void Resolve();

        bool lenient;
        std::unordered_map<const Inst*, Struct*> reg;
        std::vector<Placed> placed;
        std::vector<Ref*> refs;
        // What the last load made (nullopt: nothing), or the plain data it
        // read, which becomes a Leaf when it is kept.
        std::optional<Node*> cap;
        BufPtr leafData;
        Desc leafDesc;
        Where capWhere;
        void SetCap(std::optional<Node*> v);
        std::optional<Node*> Made();
        uint32_t slot = 0;
        bool assetInsert = false;
        std::vector<uint32_t> memberSlots;
    };

    // ---- writing a tree -----------------------------------------------------

    class TreeWriter : public Reader
    {
    public:
        TreeWriter(const Tree& tree, const zone::Schema& schema, const Platform& plat);

        // Where a Ref into an object of another layout points once
        // converted; by default where it pointed.
        std::function<int64_t(const Ref*)> mapRel;
        // An asset met again after it is written becomes a pointer to the
        // pointer that first held it, which the game sets to it.
        bool dedupe = false;
        // Each struct is written from its members, padding and unused bits
        // zero, as converted files are written; with keepBytes, as its
        // bytes are (a tree written back as it was read gives the zone back).
        bool keepBytes = false;
        size_t deduped = 0;
        // Where each asset was written, and every asset written, in order.
        std::unordered_map<const Node*, size_t> starts;
        std::vector<std::pair<size_t, Struct*>> written;

        // The zone. `header`: the original zone's header (of this platform),
        // whose temp and callback sizes are kept when bigger.
        std::vector<uint8_t> Write(const std::vector<uint8_t>* header = nullptr);

    protected:
        std::pair<BufPtr, Where> Read(size_t n, const Desc& desc) override;
        void Alias(Buf& buf, size_t loc, uint32_t val) override;
        void OnInst(Inst* inst, bool atStart) override;
        BufPtr LoadArray(const zone::StructInfo* info, bool atStart, int64_t count, BufPtr buf, size_t base) override;
        void LoadBlock(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind,
                       size_t loc) override;
        void Reuse(Inst* inst, const zone::MemberInfo* mi, size_t modPos, int64_t combined, Kind kind, size_t loc,
                   std::optional<uint32_t> val) override;
        void LoadPtrArray(const zone::MemberInfo* mi, bool atStart, int64_t count, BufPtr buf, size_t loc) override;
        void LoadXString(Buf& buf, size_t loc) override;
        void LoadXStringArray(bool atStart, int64_t count, BufPtr buf, size_t loc) override;
        void InsertPointer() override;
        Inst* LoadAssetPtr(const zone::StructInfo* info, Buf& buf, size_t loc) override;

    private:
        uint32_t RefValue(const Ref* r, bool strict = false);
        Node* Forward(Node* r, bool asset = false);
        std::optional<Where> Address(const Buf& buf, size_t loc) const;
        void Patch(Buf& buf, size_t loc, uint32_t val);
        std::vector<uint8_t> Encode(const std::optional<Node*>& obj, size_t n, const Desc& desc);
        void EncodeStruct(Struct* s, uint8_t* out, size_t size, bool fixPointers);
        void EncodeUnion(Struct* s, uint8_t* out);
        uint32_t PtrRaw(const PtrList* pl, size_t i);
        bool HasPointers(const zone::Type* t);

        const Tree& tree;
        std::unordered_map<const Inst*, Struct*> reg;
        std::unordered_map<const Node*, Where> loc;
        std::optional<Node*> obj;       // what the next read writes (nullopt: nothing; null: a null pointer)
        Node* last = nullptr;
        uint32_t slot = 0;
        std::unordered_map<const zone::Type*, bool> pointerTypes;
        std::unordered_map<const Node*, uint32_t> first;
        std::vector<size_t> zonePositions;
        std::vector<std::array<uint32_t, 3>> zoneSpans;   // (block, offset, n) of each written span
    };

    // What an alias pointing at a pointer to an asset (an asset list entry, a
    // temp asset's own slot, or a pointer member of a struct) names: that
    // asset (or Ref), or null.
    Node* SlotAsset(const Ref* r);

    // A zone's tree (`lenient` as TreeReader's), checked to have been read to
    // its last byte.
    Tree ReadTree(const zone::Schema& schema, const Platform& plat, std::vector<uint8_t> zone, bool lenient = false,
                  std::vector<std::pair<Struct*, size_t>>* assetStarts = nullptr);
}
