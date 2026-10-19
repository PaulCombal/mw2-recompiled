#include "expr.h"

#include <cctype>

namespace zone
{
    namespace
    {
        bool Word(char c) { return std::isalnum(static_cast<unsigned char>(c)) || c == '_'; }
        bool Alpha(char c) { return std::isalpha(static_cast<unsigned char>(c)) || c == '_'; }
        bool Digit(char c) { return std::isdigit(static_cast<unsigned char>(c)) != 0; }
        bool Hex(char c) { return std::isxdigit(static_cast<unsigned char>(c)) != 0; }

        // Python's floor division and modulo.
        int64_t FloorDiv(int64_t a, int64_t b)
        {
            if (!b) throw Error("division by zero in a definition");
            int64_t q = a / b;
            if ((a % b != 0) && ((a < 0) != (b < 0))) q--;
            return q;
        }
        int64_t FloorMod(int64_t a, int64_t b) { return a - FloorDiv(a, b) * b; }
    }

    std::vector<std::string> Tokens(std::string_view s, bool paths)
    {
        std::vector<std::string> out;
        size_t i = 0;
        while (i < s.size())
        {
            const char c = s[i];
            if (Alpha(c))
            {
                size_t j = i + 1;
                while (j < s.size() && Word(s[j])) j++;
                // Type::member::member, one token in a rule.
                while (paths && j + 2 < s.size() && s[j] == ':' && s[j + 1] == ':' && Alpha(s[j + 2]))
                {
                    j += 3;
                    while (j < s.size() && Word(s[j])) j++;
                }
                out.emplace_back(s.substr(i, j - i));
                i = j;
                continue;
            }
            if (c == '0' && i + 2 < s.size() && s[i + 1] == 'x' && Hex(s[i + 2]))
            {
                size_t j = i + 2;
                while (j < s.size() && Hex(s[j])) j++;
                out.emplace_back(s.substr(i, j - i));
                i = j;
                continue;
            }
            if (Digit(c))
            {
                size_t j = i;
                while (j < s.size() && Digit(s[j])) j++;
                if (!paths)
                {
                    // \d+\.?\d*[fF]?
                    if (j < s.size() && s[j] == '.') { j++; while (j < s.size() && Digit(s[j])) j++; }
                    if (j < s.size() && (s[j] == 'f' || s[j] == 'F')) j++;
                }
                out.emplace_back(s.substr(i, j - i));
                i = j;
                continue;
            }
            const std::string_view two = s.substr(i, 2);
            if (two == "<<" || two == ">>" ||
                (!paths && two == "::") ||
                (paths && (two == "&&" || two == "||" || two == "==" || two == "!=" || two == "<=" || two == ">=")))
            {
                out.emplace_back(two);
                i += 2;
                continue;
            }
            const std::string_view singles = paths ? "-+*/%()<>!&|^~[]" : "{}()[];,:=*&|+-~<>/%^!?.";
            if (singles.find(c) != std::string_view::npos) out.emplace_back(1, c);
            i++;
        }
        return out;
    }

    std::unique_ptr<Node> ExprParser::Binary(Node::Op op, std::unique_ptr<Node> a, std::unique_ptr<Node> b)
    {
        auto n = std::make_unique<Node>();
        n->op = op;
        n->kids.push_back(std::move(a));
        n->kids.push_back(std::move(b));
        return n;
    }

    std::unique_ptr<Node> ExprParser::Parse(size_t at, size_t stop)
    {
        const size_t savedPos = pos, savedEnd = end;
        pos = at;
        end = stop;
        auto n = Or();
        if (pos != end) throw Error("unexpected '" + toks[pos] + "' in an expression");
        pos = savedPos;
        end = savedEnd;
        return n;
    }

    std::unique_ptr<Node> ExprParser::Or()
    {
        auto n = And();
        while (At("||")) { pos++; n = Binary(Node::Or, std::move(n), And()); }
        return n;
    }

    std::unique_ptr<Node> ExprParser::And()
    {
        auto n = NotExpr();
        while (At("&&")) { pos++; n = Binary(Node::And, std::move(n), NotExpr()); }
        return n;
    }

    std::unique_ptr<Node> ExprParser::NotExpr()
    {
        if (At("!"))
        {
            pos++;
            auto n = std::make_unique<Node>();
            n->op = Node::Not;
            n->kids.push_back(NotExpr());
            return n;
        }
        return Compare();
    }

    std::unique_ptr<Node> ExprParser::Compare()
    {
        auto n = BitOrExpr();
        while (true)
        {
            Node::Op op;
            if (At("<")) op = Node::Lt;
            else if (At("<=")) op = Node::Le;
            else if (At(">")) op = Node::Gt;
            else if (At(">=")) op = Node::Ge;
            else if (At("==")) op = Node::Eq;
            else if (At("!=")) op = Node::Ne;
            else return n;
            pos++;
            n = Binary(op, std::move(n), BitOrExpr());
        }
    }

    std::unique_ptr<Node> ExprParser::BitOrExpr()
    {
        auto n = BitXorExpr();
        while (At("|")) { pos++; n = Binary(Node::BitOr, std::move(n), BitXorExpr()); }
        return n;
    }

    std::unique_ptr<Node> ExprParser::BitXorExpr()
    {
        auto n = BitAndExpr();
        while (At("^")) { pos++; n = Binary(Node::BitXor, std::move(n), BitAndExpr()); }
        return n;
    }

    std::unique_ptr<Node> ExprParser::BitAndExpr()
    {
        auto n = Shift();
        while (At("&")) { pos++; n = Binary(Node::BitAnd, std::move(n), Shift()); }
        return n;
    }

    std::unique_ptr<Node> ExprParser::Shift()
    {
        auto n = Sum();
        while (At("<<") || At(">>"))
        {
            const Node::Op op = At("<<") ? Node::Shl : Node::Shr;
            pos++;
            n = Binary(op, std::move(n), Sum());
        }
        return n;
    }

    std::unique_ptr<Node> ExprParser::Sum()
    {
        auto n = Product();
        while (At("+") || At("-"))
        {
            const Node::Op op = At("+") ? Node::Add : Node::Sub;
            pos++;
            n = Binary(op, std::move(n), Product());
        }
        return n;
    }

    std::unique_ptr<Node> ExprParser::Product()
    {
        auto n = Unary();
        while (At("*") || At("/") || At("%"))
        {
            const Node::Op op = At("*") ? Node::Mul : At("/") ? Node::Div : Node::Mod;
            pos++;
            n = Binary(op, std::move(n), Unary());
        }
        return n;
    }

    std::unique_ptr<Node> ExprParser::Unary()
    {
        if (At("-") || At("+") || At("~"))
        {
            const std::string& t = toks[pos++];
            auto inner = Unary();
            if (t == "+") return inner;
            auto n = std::make_unique<Node>();
            n->op = t == "-" ? Node::Neg : Node::Inv;
            n->kids.push_back(std::move(inner));
            return n;
        }
        return Atom();
    }

    std::unique_ptr<Node> ExprParser::Atom()
    {
        if (pos >= end) throw Error("an expression ends too early");
        const std::string& t = toks[pos];
        if (t == "(")
        {
            pos++;
            auto n = Or();
            if (!At(")")) throw Error("a ')' is missing in an expression");
            pos++;
            return n;
        }
        if (Digit(t[0]))
        {
            pos++;
            auto n = std::make_unique<Node>();
            n->value = int64_t(std::stoll(t, nullptr, 0));
            return n;
        }
        if (Alpha(t[0]))
        {
            return identifier(*this, pos);
        }
        throw Error("unexpected '" + t + "' in an expression");
    }

    int64_t Node::Eval(const Lookup& lookup, bool floorDivision) const
    {
        auto v = [&](size_t k) { return kids[k]->Eval(lookup, floorDivision); };
        switch (op)
        {
        case Num: return value;
        case Ref:
        {
            std::vector<int64_t> indices;
            for (const auto& k : kids) indices.push_back(k->Eval(lookup, floorDivision));
            return lookup(int(value), indices);
        }
        case Neg: return -v(0);
        case Inv: return ~v(0);
        case Not: return v(0) ? 0 : 1;
        case Mul: return v(0) * v(1);
        case Div:
        {
            const int64_t a = v(0), b = v(1);
            if (floorDivision) return FloorDiv(a, b);
            if (!b) throw Error("division by zero in a definition");
            return a / b;   // int(a / b): towards zero
        }
        case Mod: return FloorMod(v(0), v(1));
        case Add: return v(0) + v(1);
        case Sub: return v(0) - v(1);
        case Shl: return v(0) << v(1);
        case Shr: return v(0) >> v(1);
        case BitAnd: return v(0) & v(1);
        case BitXor: return v(0) ^ v(1);
        case BitOr: return v(0) | v(1);
        case Lt: return v(0) < v(1);
        case Le: return v(0) <= v(1);
        case Gt: return v(0) > v(1);
        case Ge: return v(0) >= v(1);
        case Eq: return v(0) == v(1);
        case Ne: return v(0) != v(1);
        case And: { const int64_t a = v(0); return a ? v(1) : a; }
        case Or: { const int64_t a = v(0); return a ? a : v(1); }
        }
        return 0;
    }
}
