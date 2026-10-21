#pragma once
// What the converter asks of a tree: an asset's name, what a pointer back to
// earlier data means, a raw file's text.
#include "tree.h"

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace mapport
{
    // What a child is: a Ref's target, or the child itself.
    inline Node* Target(Node* c)
    {
        if (Ref* r = As<Ref>(c)) return r->target;
        return c;
    }

    // An asset child, or the asset an alias refers to (an alias to a pointer
    // that holds an asset, or to a struct itself), else null.
    Node* Deref(Node* c);
    Struct* DerefStruct(Node* c);
    // If alias r points at a pointer member (inside a struct, an array of
    // them, an asset list entry, a temp asset's slot, a pointer array) that
    // holds an asset, that asset (or Ref), else null.
    Node* AssetInSlot(const Ref* r);
    // If alias r points at one pointer of an array of pointers inside an
    // element of a struct array (not at the array's start), what it holds.
    Node* PtrArrayValue(const Ref* r);

    // An embedded struct member (Struct::Sub), or null when there is none.
    Struct* Embedded(const Struct* d, std::string_view member);

    // An asset's name: its "name" pointer's string (or its info's, for a
    // material). Name also follows an alias to a string.
    std::optional<std::string> AssetName(const Struct* d);
    std::optional<std::string> Name(const Struct* d);
    void SetName(Struct* d, std::string name);
    // A sound alias list's name (aliasName).
    std::optional<std::string> AliasName(const Struct* d);
    // The name an asset takes a place in the game's pool under.
    std::optional<std::string> PoolName(const Struct* d);
    inline bool Named(const std::optional<std::string>& n) { return n && !n->empty(); }
    inline bool OnlyNamed(const std::optional<std::string>& n) { return n && !n->empty() && (*n)[0] == ','; }

    // A raw file's text (inflated when the file holds it compressed), and a
    // new one stored plain.
    std::string RawfileText(Struct* rawfile);
    void SetRawfileText(Struct* rawfile, const std::string& text);
    // The rawfile IW4x's tools sign a file with, or one marked compressed
    // whose data isn't a zlib stream (the 360 would try to inflate it).
    bool IsBuilderSignature(Struct* rawfile);

    // (loadBits[0], loadBits[1]) of each entry of a material's
    // stateBitsTable, or nothing when it has none.
    std::optional<std::vector<std::pair<uint32_t, uint32_t>>> StateWords(Node* table);
    // A copy of a stateBitsTable with every entry's face culling (loadBits[0]
    // bits 14-15, the same on both platforms: 1 none, 2 back, 3 front) set.
    Node* WithCull(Node* table, uint32_t cull);

    std::string Lower(std::string s);
    // A name without its leading commas (",name" names an asset the game has
    // loaded).
    std::string WithoutCommas(std::string name);
    // The first `most` of a list, joined with commas, and `more` after them
    // when there are others.
    std::string Joined(const std::vector<std::string>& v, size_t most = SIZE_MAX, std::string_view more = "");

    // Each item of a child that is a list (an Array, a PtrList), or the child.
    template <class F>
    void EachHeld(Node* c, F&& f)
    {
        if (Array* a = As<Array>(c))
            for (Node* x : a->items) f(x);
        else if (PtrList* pl = As<PtrList>(c))
            for (Node* x : pl->items) f(x);
        else f(c);
    }

    // How a struct's member holds what its pointer loaded: not a pointer, one
    // pointer, or an inline array of them (a PtrList child, or children by
    // element).
    enum class Holder { None, One, Array };
    Holder HolderOf(const Struct* o, std::string_view member);
    // Sets the pointer that holds a child: the member, or element k of an
    // array of them; in a union, its bytes.
    void SetHolder(Struct* o, std::string_view member, uint32_t value, size_t k = 0);
}
