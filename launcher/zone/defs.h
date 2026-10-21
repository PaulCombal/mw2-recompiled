#pragma once
// The structures of a zone, read from defs/iw4_assets.h and laid out as the
// 360's compiler lays them out: 32-bit pointers, each member at its natural
// alignment, a struct padded to its largest member's.
//
// Only the C the header uses is understood: structs, unions, enums (with an
// underlying type), typedefs, arrays, pointers, bitfields and OpenAssetTools'
// alignment macros. Text between `#ifndef __zonecodegenerator` and its
// `#endif` is skipped, as OpenAssetTools' own generator skips it.
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace zone
{
    struct Error : std::runtime_error
    {
        using std::runtime_error::runtime_error;
    };

    // A declarator modifier: a pointer, or an array of that many.
    constexpr int kPointer = -1;

    struct Type;

    struct Member
    {
        std::string name;              // empty for an anonymous struct or union
        Type* type = nullptr;          // the element type, after every modifier
        std::vector<int> mods;         // outermost first: kPointer or an array size
        int bits = -1;                 // a bitfield's width
        uint32_t offset = 0, bitpos = 0, size = 0, align = 1;

        // A pointer (T* x, T (*x)[n]), and one somewhere in its modifiers
        // (T* x[n] too).
        bool IsPointer() const { return !mods.empty() && mods[0] == kPointer; }
        bool HasPointer() const
        {
            for (int m : mods)
                if (m == kPointer) return true;
            return false;
        }
        // Its array sizes, and how many elements they make.
        std::vector<int> Dims() const
        {
            std::vector<int> d;
            for (int m : mods)
                if (m != kPointer) d.push_back(m);
            return d;
        }
        size_t Elements() const
        {
            size_t n = 1;
            for (int m : mods)
                if (m != kPointer) n *= size_t(m);
            return n;
        }
    };

    struct Type
    {
        enum Kind { Prim, Enum, Struct, Union, Typedef } kind = Prim;
        std::string name;
        uint32_t size = 0, align = 1;
        bool isSigned = false, isFloat = false;   // how a primitive or enum is read
        std::vector<Member> members;
        uint32_t alignas_ = 0;
        bool defined = false, laidOut = false;
        Type* parent = nullptr;

        bool Compound() const { return kind == Struct || kind == Union || kind == Typedef; }
        // A typedef around a struct (GfxCellTree128 = GfxCellTree, aligned)
        // is the struct itself.
        const Type* Unwrapped() const
        {
            const Type* t = this;
            while (t->kind == Typedef && t->members.size() == 1 && t->members[0].type->Compound() && t->members[0].mods.empty())
                t = t->members[0].type;
            return t;
        }
    };

    class Defs
    {
    public:
        explicit Defs(std::string_view header);

        // In the order they were first named, as the header names them.
        std::vector<Type*> named;
        std::unordered_map<std::string, Type*> types;
        std::unordered_map<std::string, int64_t> consts;

    private:
        std::vector<std::unique_ptr<Type>> owned;
        std::vector<std::string> tok;
        size_t i = 0;

        Type* New(Type::Kind kind, std::string name);
        void Name(const std::string& name, Type* type);
        const std::string& Peek(size_t k = 0) const;
        std::string Next();
        void Expect(std::string_view t);
        void Top();
        uint32_t Alignas();
        Type* CompoundDecl(Type* parent);
        Type* GetCompound(const std::string& kind, const std::string& name);
        Type* EnumDecl();
        Type* BaseType(Type* parent);
        std::pair<std::string, std::vector<int>> Declarator();
        void MemberDecl(Type* c);
        void Typedef();
        int64_t Eval(const std::vector<std::string>& expr);
        std::pair<uint32_t, uint32_t> MemberLayout(Member& m);
        void Layout(Type* c);
    };
}
