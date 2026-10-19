#pragma once
// The integer expressions of the definitions: array sizes and enum values in
// the header, counts and conditions in the loading rules. OpenAssetTools'
// generator wrote them as C; pc2xbox360mw2ools evaluates them as Python, and
// these follow Python's precedence and its && / || returning an operand, so a
// rule means here what it means there.
#include "defs.h"

#include <functional>
#include <memory>
#include <string>
#include <vector>

namespace zone
{
    struct Node
    {
        enum Op
        {
            Num, Ref, Neg, Inv, Not, Mul, Div, Mod, Add, Sub, Shl, Shr,
            BitAnd, BitXor, BitOr, Lt, Le, Gt, Ge, Eq, Ne, And, Or,
        } op = Num;
        int64_t value = 0;   // Num: the number; Ref: which reference
        std::vector<std::unique_ptr<Node>> kids;

        // A reference's value, given the values of its indices.
        using Lookup = std::function<int64_t(int ref, const std::vector<int64_t>& indices)>;
        int64_t Eval(const Lookup& lookup, bool floorDivision) const;
    };

    // Splits text the way the definitions' tokenisers do. `paths` keeps
    // Type::member::member together, as the loading rules write them.
    std::vector<std::string> Tokens(std::string_view text, bool paths);

    class ExprParser
    {
    public:
        // Called at an identifier: it consumes what it names (from toks[at])
        // and returns the node, using Parse for any index inside brackets.
        using Identifier = std::function<std::unique_ptr<Node>(ExprParser&, size_t& at)>;

        ExprParser(const std::vector<std::string>& toks, Identifier identifier)
            : toks(toks), identifier(std::move(identifier)) {}

        // The expression in toks[at, end).
        std::unique_ptr<Node> Parse(size_t at, size_t end);

        const std::vector<std::string>& toks;
        size_t End() const { return end; }   // of the expression being parsed

    private:
        Identifier identifier;
        size_t pos = 0, end = 0;

        std::unique_ptr<Node> Or();
        std::unique_ptr<Node> And();
        std::unique_ptr<Node> NotExpr();
        std::unique_ptr<Node> Compare();
        std::unique_ptr<Node> BitOrExpr();
        std::unique_ptr<Node> BitXorExpr();
        std::unique_ptr<Node> BitAndExpr();
        std::unique_ptr<Node> Shift();
        std::unique_ptr<Node> Sum();
        std::unique_ptr<Node> Product();
        std::unique_ptr<Node> Unary();
        std::unique_ptr<Node> Atom();
        bool At(std::string_view t) const { return pos < end && toks[pos] == t; }
        static std::unique_ptr<Node> Binary(Node::Op op, std::unique_ptr<Node> a, std::unique_ptr<Node> b);
    };
}
