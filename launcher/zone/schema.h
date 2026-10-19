#pragma once
// How the game streams the structures out of a zone, from the loading rules
// (defs/commands.txt and defs/assets/*.txt, OpenAssetTools' ZoneCode format):
// which pointers are strings, how many elements an array pointer has, which
// member of a union is there, which memory block data goes to, and the order
// a struct's members load in.
#include "defs.h"
#include "expr.h"

#include <deque>
#include <map>
#include <optional>

namespace zone
{
    struct Block
    {
        std::string kind, name;   // kind: temp, runtime or normal
        bool isDefault = false;
    };

    struct StructInfo;
    struct MemberInfo;

    // A reference in a rule's expression: a member of the struct being loaded
    // (or of one loaded around it), or the index of the element of a type
    // being loaded.
    struct ExprRef
    {
        bool isIndex = false;
        StructInfo* info = nullptr;
        std::vector<std::string> names;
        std::vector<MemberInfo*> chain;   // the names, resolved from info
    };

    struct Expr
    {
        std::unique_ptr<Node> root;
        std::vector<ExprRef> refs;
        std::optional<int64_t> constant;   // when it refers to nothing
    };

    struct MemberInfo
    {
        StructInfo* parent = nullptr;
        const Member* m = nullptr;
        std::string name;
        bool isString = false, isScriptString = false, isReusable = false;
        const Expr* condition = nullptr;
        const Block* block = nullptr;
        uint32_t allocAlign = 0;
        struct { const Expr* cond = nullptr; const Block* ifTrue = nullptr; const Block* ifFalse = nullptr; } blockCond;
        std::map<size_t, const Expr*> counts;                       // per pointer modifier
        std::map<size_t, std::map<int64_t, const Expr*>> countsByIndex;
        std::map<size_t, const Expr*> arraySize;                    // per array modifier
        StructInfo* type = nullptr;   // when the element is a struct or union
        bool isLeaf = true;

        const Expr* Count(size_t mod, int64_t combined) const;
        bool CountIsArray(size_t mod, int64_t combined) const;
        bool AnyCountIsArray(size_t mod) const;
        bool CountStaticZero(size_t mod) const;
    };

    struct StructInfo
    {
        const Type* ctype = nullptr;
        std::string name;
        std::vector<MemberInfo*> members;   // declared order
        std::vector<MemberInfo*> ordered;   // load order
        const Block* block = nullptr;
        std::string asset;                  // the asset enum's name, when this is an asset
        uint32_t allocAlign = 0;
        bool isLeaf = true, isUnion = false;

        // Worked out once the rules are read.
        MemberInfo* dynamic = nullptr;
        std::vector<MemberInfo*> used;

        MemberInfo* Find(const std::string& name) const;
    };

    class Schema
    {
    public:
        // The header, and the rules with every #include already resolved by
        // `include` (given the name as the rules write it).
        Schema(std::string_view header, std::string_view commands,
               const std::function<std::string(const std::string&)>& include);

        Defs defs;
        std::unordered_map<std::string, StructInfo*> infos;
        std::unordered_map<std::string, StructInfo*> assets;   // by asset enum name
        std::deque<Block> blocks;

        // A member of info by name, looking into anonymous members too.
        std::vector<MemberInfo*> FindMember(const StructInfo* info, const std::string& name) const;

        bool NeedsTreatment(const MemberInfo* m) const;
        bool AfterPartialLoad(const MemberInfo* m) const;
        static bool Ignored(const MemberInfo* m)
        {
            return m->condition && m->condition->constant && *m->condition->constant == 0;
        }

    private:
        std::deque<StructInfo> structs;
        std::deque<MemberInfo> members;
        std::deque<Expr> exprs;
        std::unordered_map<const Type*, StructInfo*> byType;
        int anonymous = 0;

        StructInfo* Info(const Type* t);
        std::pair<StructInfo*, std::vector<MemberInfo*>> Resolve(const std::string& path, StructInfo* root);
        const Block* BlockNamed(const std::string& name) const;
        const Expr* Compile(const std::string& text, StructInfo* root);
        void Commands(const std::string& text);
        void Count(const std::string& rest, StructInfo* use);
        void Reorder(const std::string& statement, StructInfo* use);
        void PostProcess();
        bool IsDynamicMember(const MemberInfo* m) const;
        MemberInfo* DynamicMember(const StructInfo* info) const;
    };
}
