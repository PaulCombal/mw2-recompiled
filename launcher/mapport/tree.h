#pragma once
// A zone as a tree of nodes: what the map converter reads a fastfile into,
// changes, and writes back out (reader.h).
//
// A Struct is one struct of the zone, with its bytes as its platform lays
// them out (the PC's little-endian layout, or the 360's big-endian one) and
// what each of its pointers loaded: another Struct, an Array of them, a Leaf
// of plain data, a String, a PtrList, or a Ref back to something loaded
// earlier. A struct embedded in another (a member that is a struct, an
// element of an embedded array) is a view into its owner's bytes, with
// pointers of its own.
//
// A pointer member's bytes hold what the loader reads there: 0, kFollowing
// (the data follows in the stream), kInsert (the loader makes a pointer to
// it), or a pointer back to earlier data, which the writer works out anew
// from where the Ref's target was written.
//
// Nodes have identity: a Ref names its target, and the converter collects,
// compares and marks nodes as themselves. A Tree owns every node it makes;
// nodes of one conversion (the PC map and the stock files it draws on) live
// in one Arena.
#include "../zone/schema.h"

#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace mapport
{
    struct Error : std::runtime_error
    {
        using std::runtime_error::runtime_error;
    };

    constexpr uint32_t kFollowing = 0xFFFFFFFF;
    constexpr uint32_t kInsert = 0xFFFFFFFE;

    // Numbers in either byte order.
    uint64_t Load(const uint8_t* p, uint32_t size, bool big);
    void Store(uint8_t* p, uint32_t size, uint64_t v, bool big);
    float LoadF32(const uint8_t* p, bool big);
    void StoreF32(uint8_t* p, float v, bool big);

    class Node
    {
    public:
        enum class Kind : uint8_t { Struct, Array, Leaf, String, PtrList, Ref, Slot, Entry };
        virtual ~Node() = default;
        const Kind kind;

    protected:
        explicit Node(Kind k) : kind(k) {}
    };

    template <class T>
    T* As(Node* n)
    {
        return n && n->kind == T::kKind ? static_cast<T*>(n) : nullptr;
    }
    template <class T>
    const T* As(const Node* n)
    {
        return n && n->kind == T::kKind ? static_cast<const T*>(n) : nullptr;
    }

    // Where in a struct a pointer is: its member, and the indices of the
    // element when the member is an array. The member is a view of a name
    // that outlives the tree (the schema's own, or a literal).
    struct ChildKey
    {
        std::string_view member;
        std::vector<int32_t> idx;
        bool operator==(const ChildKey& b) const { return member == b.member && idx == b.idx; }
    };

    // What a struct's pointers loaded, in the order they loaded. Copies of a
    // struct (Struct::Copy) share it.
    class Children
    {
    public:
        Node* Get(std::string_view member, const std::vector<int32_t>& idx = {}) const;
        void Set(std::string_view member, Node* n, const std::vector<int32_t>& idx = {});
        Node* Pop(std::string_view member, const std::vector<int32_t>& idx = {});
        bool Empty() const { return items.empty(); }
        std::vector<std::pair<ChildKey, Node*>> items;
    };

    class Slot;

    // A pak entry of a streamed picture: (pak, start, end) of one level.
    struct PakEntry
    {
        uint32_t pak = 0, start = 0, end = 0;
    };

    class Struct : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Struct;
        Struct(const zone::Type* type, bool big, std::vector<uint8_t> bytes);
        Struct(Struct* owner, const zone::Type* type, size_t at, size_t size);

        const zone::Type* type;
        bool big;

        // ---- bytes ---------------------------------------------------------
        uint8_t* Data();
        const uint8_t* Data() const;
        // How many bytes it has: its type's, or fewer for one streamed in two
        // parts (a struct with a dynamic member).
        size_t Size() const { return size; }
        std::vector<uint8_t>& Own() { return bytes; }

        // ---- members -------------------------------------------------------
        const zone::Member* Find(std::string_view member) const;
        const zone::Member& M(std::string_view member) const;
        // The member is there: in its type, and within the struct's bytes or
        // streamed after them.
        bool Has(std::string_view member) const;
        // A number member (or element `k` of an array of numbers).
        int64_t Int(std::string_view member, size_t k = 0) const;
        uint64_t U(std::string_view member, size_t k = 0) const;
        double F(std::string_view member, size_t k = 0) const;
        void Set(std::string_view member, int64_t v, size_t k = 0);
        void SetF(std::string_view member, double v, size_t k = 0);
        // A pointer member's value (element `k` of an array of pointers).
        uint32_t Ptr(std::string_view member, size_t k = 0) const;
        void SetPtr(std::string_view member, uint32_t v, size_t k = 0);
        // A member's own bytes (a byte array, a union).
        std::span<uint8_t> Bytes(std::string_view member);
        std::span<const uint8_t> Bytes(std::string_view member) const;
        // An embedded struct or union, or element `k` of an embedded array of
        // them: a view of its bytes (or what was streamed for it after the
        // struct's first part).
        Struct* Sub(std::string_view member, int64_t k = -1);
        Struct* Sub(const zone::Member& m, int64_t k = -1);
        // What was streamed for a member after the struct's first part (a
        // dynamic array, a struct loaded after a partial one), or null.
        Node* Late(std::string_view member, const std::vector<int32_t>& idx = {}) const;
        void SetLate(std::string_view member, Node* n, const std::vector<int32_t>& idx = {});
        // Whether they come after its pointers when walked: in a tree as read,
        // when its pointers loaded before them; not once converted.
        bool latesAfterPointers = false;

        // ---- pointers ------------------------------------------------------
        Children* Ch() const { return ch; }
        Children& ChMake();
        void SetCh(Children* c) { ch = c; }
        Node* Child(std::string_view member, const std::vector<int32_t>& idx = {}) const;
        void SetChild(std::string_view member, Node* n, const std::vector<int32_t>& idx = {});
        Node* PopChild(std::string_view member, const std::vector<int32_t>& idx = {});

        // ---- the converter's notes -----------------------------------------
        // The asset struct's name, when this is an asset.
        std::string_view asset;
        bool IsAsset() const { return !asset.empty(); }
        bool IsAsset(std::string_view name) const { return asset == name; }
        // The pointer the loader makes for it (a temp-block asset, or one
        // pointed at from elsewhere), and whether it is written at its first
        // pointer rather than where it is held.
        Slot* slot = nullptr;
        bool forward = false;
        // A streamed stock picture's pak entries.
        std::shared_ptr<std::vector<PakEntry>> pak;

        // The views made of embedded members, by member and element.
        struct View
        {
            const zone::Member* m;
            int64_t k;
            Struct* s;
        };
        const std::vector<View>& Views() const { return views; }
        struct LateValue
        {
            ChildKey key;
            Node* n;
        };
        const std::vector<LateValue>& Lates() const { return lates; }

        // Becomes `src` (its type, bytes, notes and pointers), keeping its
        // identity.
        void Assign(const Struct& src);
        // Becomes `made` (a struct built in another layout), keeping its
        // identity and that of the views of it already made, which become
        // views of the same members in the new layout.
        void Adopt(Struct& made);
        // A copy: its own bytes and views, the same pointers.
        Struct* Copy() const;
        // A struct of zeros.
        static Struct* Zero(const zone::Type* type, bool big);

        // For a union: the members a walk went into (an embedded struct in it),
        // in the order it first did, which say which member is in use (views
        // of the schema's names).
        std::vector<std::string_view> used;

        // The owner, for a view.
        Struct* Owner() const { return owner; }
        size_t At() const { return at; }

    private:
        uint8_t* MemberData(const zone::Member& m, size_t k);
        const uint8_t* MemberData(const zone::Member& m, size_t k) const;
        uint32_t ElemSize(const zone::Member& m) const;

        std::vector<uint8_t> bytes;
        Struct* owner = nullptr;
        size_t at = 0;
        size_t size = 0;
        Children* ch = nullptr;
        std::vector<View> views;
        std::vector<LateValue> lates;
    };

    // An array of structs a pointer loaded. A Tail is a copy of the end of
    // another array, written with as many items as the file asks for.
    class Array : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Array;
        Array() : Node(kKind) {}
        std::vector<Node*> items;
        bool tail = false;
    };

    // Plain data: `n` elements of a type, as raw bytes in either byte order.
    class Leaf : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Leaf;
        Leaf(const zone::Type* t, int64_t n, std::vector<uint8_t> raw, bool big)
            : Node(kKind), t(t), n(n), raw(std::move(raw)), big(big) {}
        const zone::Type* t;
        int64_t n;
        std::vector<uint8_t> raw;
        bool big;
    };

    class String : public Node
    {
    public:
        static constexpr Kind kKind = Kind::String;
        explicit String(std::string b) : Node(kKind), b(std::move(b)) {}
        std::string b;
    };

    // The items an array of pointers loaded (null for a null pointer), and
    // the pointers as streamed.
    class PtrList : public Node
    {
    public:
        static constexpr Kind kKind = Kind::PtrList;
        PtrList() : Node(kKind) {}
        std::vector<Node*> items;
        std::optional<std::vector<uint32_t>> raw;
    };

    // A Ref's element type: a type, or (an asset list entry, a pointer, a
    // string) a plain size.
    struct ElemType
    {
        const zone::Type* t = nullptr;
        uint32_t size = 0;
        uint32_t Size() const { return t ? t->size : size; }
    };

    // A pointer back to something loaded earlier: the value as streamed, and
    // once the walk has finished, what it points into, the byte offset in
    // it and that object's element type.
    class Ref : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Ref;
        explicit Ref(uint32_t val) : Node(kKind), val(val) {}
        uint32_t val;
        Node* target = nullptr;
        int64_t rel = 0;
        ElemType t;
    };

    // The pointer a temp-block asset has the loader make for it, which later
    // pointers to the asset name.
    class Slot : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Slot;
        explicit Slot(Node* asset) : Node(kKind), asset(asset) {}
        Node* asset;
    };

    // An entry of the zone's asset list: the asset type's name and the asset.
    class Entry : public Node
    {
    public:
        static constexpr Kind kKind = Kind::Entry;
        Entry(std::string type, Node* asset) : Node(kKind), type(std::move(type)), asset(asset) {}
        std::string type;
        Node* asset;
    };

    // Every node of a conversion.
    class Arena
    {
    public:
        template <class T, class... A>
        T* Make(A&&... a)
        {
            auto p = std::make_unique<T>(std::forward<A>(a)...);
            T* raw = p.get();
            nodes.push_back(std::move(p));
            return raw;
        }
        Children* MakeChildren()
        {
            children.emplace_back();
            return &children.back();
        }
        size_t Count() const { return nodes.size(); }

    private:
        std::vector<std::unique_ptr<Node>> nodes;
        std::deque<Children> children;
    };
    // The arena nodes go in, while a conversion runs (ArenaScope).
    Arena& A();
    class ArenaScope
    {
    public:
        explicit ArenaScope(Arena& a);
        ~ArenaScope();

    private:
        Arena* prev;
    };

    // A zone read as a tree: its header, script strings and asset list.
    struct Tree
    {
        std::string platform;
        std::vector<uint8_t> header;
        PtrList* scriptStrings = nullptr;     // of Strings
        std::vector<Entry*> assets;
    };

    // Every Struct, Array, Leaf, PtrList, Slot and Entry reachable from
    // `root` without following Refs, each once: depth first, the last pushed
    // first, a struct's embedded members pushed before its pointers.
    // Conversions depend on the order: assets found in it go into the file in
    // it.
    void ForEach(Node* root, const std::function<void(Node*)>& f);
    void ForEach(const std::vector<Entry*>& entries, const std::function<void(Node*)>& f);
    void ForEach(const std::vector<Node*>& roots, const std::function<void(Node*)>& f);
    std::vector<Node*> Collect(Node* root);
    std::vector<Node*> Collect(const std::vector<Entry*>& entries);
    std::vector<Node*> Collect(const std::vector<Node*>& roots);

    // A copy of everything `root` reaches, through pointers back to earlier
    // data and slots too, each node once: what it shares, the
    // copy shares. `memo`, when given: nodes mapped to what stands for them
    // (a node mapped to itself is left as it is), and on return every node
    // copied, mapped to its copy.
    Node* DeepCopy(Node* root, std::unordered_map<const Node*, Node*>* memo = nullptr);
    Struct* DeepCopy(Struct* root, std::unordered_map<const Node*, Node*>* memo = nullptr);
}
