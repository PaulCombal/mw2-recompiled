#include "defs.h"
#include "expr.h"

#include <algorithm>

namespace zone
{
    namespace
    {
        constexpr uint32_t kPointerSize = 4;

        struct PrimDef { const char* name; uint32_t size; bool isSigned, isFloat; };
        constexpr PrimDef kPrims[] = {
            { "char", 1, true, false }, { "signed char", 1, true, false }, { "unsigned char", 1, false, false },
            { "bool", 1, false, false }, { "int8_t", 1, true, false }, { "uint8_t", 1, false, false },
            { "short", 2, true, false }, { "unsigned short", 2, false, false },
            { "int16_t", 2, true, false }, { "uint16_t", 2, false, false },
            { "int", 4, true, false }, { "unsigned int", 4, false, false },
            { "int32_t", 4, true, false }, { "uint32_t", 4, false, false },
            { "long", 4, true, false }, { "unsigned long", 4, false, false }, { "unsigned", 4, false, false },
            { "float", 4, true, true }, { "double", 8, true, true },
            { "int64_t", 8, true, false }, { "uint64_t", 8, false, false },
            { "long long", 8, true, false }, { "unsigned long long", 8, false, false },
            { "void", 0, false, false },
        };

        // Comments, and the parts only a compiler sees.
        std::string Preprocess(std::string_view text)
        {
            std::string plain;
            plain.reserve(text.size());
            for (size_t i = 0; i < text.size();)
            {
                if (text.compare(i, 2, "//") == 0)
                {
                    while (i < text.size() && text[i] != '\n') i++;
                    continue;
                }
                if (text.compare(i, 2, "/*") == 0)
                {
                    const size_t close = text.find("*/", i + 2);
                    i = close == std::string_view::npos ? text.size() : close + 2;
                    continue;
                }
                plain += text[i++];
            }

            std::string out;
            std::vector<bool> skip;
            size_t from = 0;
            while (from <= plain.size())
            {
                size_t to = plain.find('\n', from);
                if (to == std::string::npos) to = plain.size();
                const std::string_view line(plain.data() + from, to - from);
                const size_t first = line.find_first_not_of(" \t\r");
                const std::string_view s = first == std::string_view::npos ? std::string_view() : line.substr(first);
                if (!s.empty() && s[0] == '#')
                {
                    if (s.rfind("#ifndef __zonecodegenerator", 0) == 0) skip.push_back(true);
                    else if (s.rfind("#if", 0) == 0) skip.push_back(false);
                    else if (s.rfind("#endif", 0) == 0 && !skip.empty()) skip.pop_back();
                }
                else if (std::find(skip.begin(), skip.end(), true) == skip.end())
                {
                    out.append(line);
                    out += '\n';
                }
                from = to + 1;
            }
            return out;
        }

        // OpenAssetTools' alignment macros: the gcc ones mean nothing on the
        // 360, the others are alignas.
        std::vector<std::string> Macros(std::vector<std::string> toks)
        {
            static const char* dropped[] = { "gcc_align", "gcc_align32", "gcc_align64", "type_align64", "tdef_align64" };
            static const char* aligning[] = { "type_align", "type_align32", "tdef_align", "tdef_align32" };
            std::vector<std::string> out;
            for (size_t i = 0; i < toks.size(); i++)
            {
                auto is = [&](const char* const* names, size_t n) {
                    return std::find_if(names, names + n, [&](const char* m) { return toks[i] == m; }) != names + n;
                };
                const bool drop = is(dropped, std::size(dropped));
                if ((drop || is(aligning, std::size(aligning))) && i + 3 < toks.size() && toks[i + 1] == "(" &&
                    std::isdigit(static_cast<unsigned char>(toks[i + 2][0])) && toks[i + 3] == ")")
                {
                    if (!drop)
                    {
                        out.push_back("alignas");
                        out.push_back("(");
                        out.push_back(toks[i + 2]);
                        out.push_back(")");
                    }
                    i += 3;
                    continue;
                }
                out.push_back(std::move(toks[i]));
            }
            return out;
        }
    }

    Defs::Defs(std::string_view header)
    {
        for (const PrimDef& p : kPrims)
        {
            Type* t = New(Type::Prim, p.name);
            t->size = p.size;
            t->align = std::max(p.size, 1u);
            t->isSigned = p.isSigned;
            t->isFloat = p.isFloat;
            Name(p.name, t);
        }
        tok = Macros(Tokens(Preprocess(header), false));
        while (i < tok.size()) Top();
        for (Type* t : named)
            if (t->Compound() && t->defined) Layout(t);
    }

    Type* Defs::New(Type::Kind kind, std::string name)
    {
        owned.push_back(std::make_unique<Type>());
        Type* t = owned.back().get();
        t->kind = kind;
        t->name = std::move(name);
        return t;
    }

    void Defs::Name(const std::string& name, Type* type)
    {
        auto [at, fresh] = types.emplace(name, type);
        if (fresh) named.push_back(type);
        else
        {
            // A name defined again keeps its first place.
            std::replace(named.begin(), named.end(), at->second, type);
            at->second = type;
        }
    }

    const std::string& Defs::Peek(size_t k) const
    {
        static const std::string none;
        return i + k < tok.size() ? tok[i + k] : none;
    }

    std::string Defs::Next()
    {
        if (i >= tok.size()) throw Error("the definitions end too early");
        return tok[i++];
    }

    void Defs::Expect(std::string_view t)
    {
        const std::string n = Next();
        if (n != t) throw Error("definitions: expected '" + std::string(t) + "', found '" + n + "'");
    }

    void Defs::Top()
    {
        const std::string& t = Peek();
        if (t == ";") Next();
        else if (t == "struct" || t == "union") { CompoundDecl(nullptr); Expect(";"); }
        else if (t == "enum") { EnumDecl(); Expect(";"); }
        else if (t == "typedef") Typedef();
        else if (t == "namespace") { Next(); Next(); Expect("{"); }
        else if (t == "}") Next();
        else throw Error("definitions: unexpected '" + t + "'");
    }

    uint32_t Defs::Alignas()
    {
        uint32_t a = 0;
        while (Peek() == "alignas")
        {
            Next();
            Expect("(");
            a = std::max(a, uint32_t(std::stoul(Next(), nullptr, 0)));
            Expect(")");
        }
        return a;
    }

    Type* Defs::GetCompound(const std::string& kind, const std::string& name)
    {
        auto found = types.find(name);
        if (found != types.end()) return found->second;
        Type* t = New(kind == "union" ? Type::Union : Type::Struct, name);
        Name(name, t);
        return t;
    }

    Type* Defs::CompoundDecl(Type* parent)
    {
        const std::string kind = Next();
        uint32_t align = Alignas();
        std::string name;
        if (Peek() != "{")
        {
            name = Next();
            align = std::max(align, Alignas());
        }
        if (Peek() != "{") return GetCompound(kind, name);   // declared, or used as a type
        Type* c = name.empty() ? New(kind == "union" ? Type::Union : Type::Struct, "") : GetCompound(kind, name);
        c->alignas_ = std::max(c->alignas_, align);
        c->parent = parent;
        Expect("{");
        while (Peek() != "}") MemberDecl(c);
        Expect("}");
        c->defined = true;
        return c;
    }

    Type* Defs::EnumDecl()
    {
        Expect("enum");
        std::string name;
        if (Peek() != "{") name = Next();
        Type* under = nullptr;
        if (Peek() == ":")
        {
            Next();
            under = BaseType(nullptr);
        }
        Type* e = New(Type::Enum, name);
        e->size = e->align = 4;
        e->isSigned = true;
        if (under)
        {
            e->size = e->align = under->size;
            e->isSigned = under->isSigned;
        }
        if (!name.empty()) Name(name, e);
        if (Peek() != "{") return e;
        Expect("{");
        int64_t v = -1;
        while (Peek() != "}")
        {
            const std::string n = Next();
            if (Peek() == "=")
            {
                Next();
                std::vector<std::string> expr;
                while (Peek() != "," && Peek() != "}") expr.push_back(Next());
                v = Eval(expr);
            }
            else v++;
            consts[n] = v;
            if (Peek() == ",") Next();
        }
        Expect("}");
        return e;
    }

    Type* Defs::BaseType(Type* parent)
    {
        auto opensBody = [&] {
            return (Peek() == "struct" || Peek() == "union") && (Peek(1) == "{" || Peek(1) == "alignas");
        };
        while ((Peek() == "const" || Peek() == "volatile" || Peek() == "struct" || Peek() == "union" ||
                Peek() == "enum") && !opensBody())
        {
            const std::string t = Next();
            if (t == "enum")
            {
                const std::string n = Next();
                auto found = types.find(n);
                if (found != types.end()) return found->second;
                Type* e = New(Type::Enum, n);
                e->size = e->align = 4;
                e->isSigned = true;
                Name(n, e);
                return e;
            }
            if (t == "struct" || t == "union") return GetCompound(t, Next());
        }
        if (Peek() == "struct" || Peek() == "union") return CompoundDecl(parent);
        if (Peek() == "enum") return EnumDecl();
        std::string words;
        while (true)
        {
            const std::string& t = Peek();
            const bool sizeWord = t == "unsigned" || t == "signed" || t == "long" || t == "short" || t == "int" ||
                                  t == "char";
            const bool extends = words.empty() || words == "unsigned" || words == "signed" || words == "long" ||
                                 words == "unsigned long" || words == "short" || words == "unsigned short" ||
                                 words == "signed long" || words == "long long";
            if (sizeWord && extends)
            {
                words += (words.empty() ? "" : " ") + Next();
                continue;
            }
            if (words.empty()) words = Next();
            break;
        }
        while (Peek() == "const") Next();
        if (words == "unsigned long long") words = "uint64_t";
        auto found = types.find(words);
        if (found == types.end()) throw Error("definitions: unknown type '" + words + "'");
        return found->second;
    }

    std::pair<std::string, std::vector<int>> Defs::Declarator()
    {
        int pointers = 0;
        while (Peek() == "*" || Peek() == "const")
            if (Next() == "*") pointers++;
        std::string name;
        std::vector<int> inner;
        bool nested = false;
        if (Peek() == "(")
        {
            Next();
            auto [n, mods] = Declarator();
            Expect(")");
            name = n;
            inner = mods;
            nested = true;
        }
        else if (Peek() != ";" && Peek() != "," && Peek() != ":" && Peek() != "[")
            name = Next();
        std::vector<int> mods;
        while (Peek() == "[")
        {
            Next();
            std::vector<std::string> expr;
            while (Peek() != "]") expr.push_back(Next());
            Expect("]");
            mods.push_back(int(Eval(expr)));
        }
        // Arrays bind tighter than pointers.
        for (int k = 0; k < pointers; k++) mods.push_back(kPointer);
        if (nested) inner.insert(inner.end(), mods.begin(), mods.end());
        else inner = mods;
        return { name, inner };
    }

    void Defs::MemberDecl(Type* c)
    {
        Type* base;
        if ((Peek() == "struct" || Peek() == "union") && (Peek(1) == "{" || Peek(1) == "alignas"))
        {
            Type* sub = CompoundDecl(c);
            if (Peek() == ";")
            {
                Next();
                Member m;
                m.type = sub;
                c->members.push_back(m);
                return;
            }
            base = sub;
        }
        else base = BaseType(c);
        while (true)
        {
            Alignas();
            auto [name, mods] = Declarator();
            Member m;
            m.name = name;
            m.type = base;
            m.mods = mods;
            if (Peek() == ":")
            {
                Next();
                m.bits = int(std::stol(Next(), nullptr, 0));
            }
            c->members.push_back(m);
            if (Peek() == ",")
            {
                Next();
                continue;
            }
            break;
        }
        Expect(";");
    }

    void Defs::Typedef()
    {
        Expect("typedef");
        uint32_t align = Alignas();
        Type* base = BaseType(nullptr);
        align = std::max(align, Alignas());
        auto [name, mods] = Declarator();
        Expect(";");
        if (mods.empty() && !align)
        {
            Name(name, base);
            return;
        }
        // An array or an alignment: a wrapper the size of what it wraps.
        Type* t = New(Type::Typedef, name);
        Member m;
        m.type = base;
        m.mods = mods;
        t->members.push_back(m);
        t->alignas_ = align;
        t->defined = true;
        Name(name, t);
    }

    int64_t Defs::Eval(const std::vector<std::string>& expr)
    {
        // A float literal's suffix goes; constants become their values. A hex
        // number's last digit can be an F too, and stays.
        std::vector<std::string> toks;
        for (std::string t : expr)
        {
            const bool hex = t.size() > 1 && t[0] == '0' && (t[1] == 'x' || t[1] == 'X');
            if (!t.empty() && !hex && std::isdigit(static_cast<unsigned char>(t[0])) && (t.back() == 'f' || t.back() == 'F'))
                t.pop_back();
            toks.push_back(t);
        }
        ExprParser parser(toks, [&](ExprParser& p, size_t& at) {
            auto found = consts.find(p.toks[at]);
            if (found == consts.end()) throw Error("definitions: unknown constant '" + p.toks[at] + "'");
            at++;
            auto n = std::make_unique<Node>();
            n->value = found->second;
            return n;
        });
        return parser.Parse(0, toks.size())->Eval({}, false);
    }

    std::pair<uint32_t, uint32_t> Defs::MemberLayout(Member& m)
    {
        if (!m.mods.empty() && m.mods[0] == kPointer) return { kPointerSize, kPointerSize };
        uint32_t n = 1;
        size_t k = 0;
        while (k < m.mods.size() && m.mods[k] != kPointer) n *= uint32_t(m.mods[k++]);
        if (k < m.mods.size()) return { n * kPointerSize, kPointerSize };
        if (m.type->Compound() && !m.type->laidOut) Layout(m.type);
        return { n * m.type->size, m.type->align };
    }

    void Defs::Layout(Type* c)
    {
        if (c->laidOut) return;
        c->laidOut = true;
        uint32_t off = 0, size = 0, align = 1;
        bool inUnit = false;
        uint32_t unitOff = 0, unitSize = 0, unitUsed = 0;
        for (Member& m : c->members)
        {
            const auto [msize, malign] = MemberLayout(m);
            m.size = msize;
            m.align = malign;
            align = std::max(align, malign);
            if (c->kind == Type::Union)
            {
                m.offset = 0;
                if (m.bits >= 0) m.bitpos = 0;
                size = std::max(size, msize);
                continue;
            }
            if (m.bits >= 0)
            {
                if (inUnit && unitSize == msize && unitUsed + uint32_t(m.bits) <= msize * 8)
                {
                    m.offset = unitOff;
                    m.bitpos = unitUsed;
                    unitUsed += uint32_t(m.bits);
                    continue;
                }
                off = (off + malign - 1) / malign * malign;
                inUnit = true;
                unitOff = off;
                unitSize = msize;
                unitUsed = uint32_t(m.bits);
                m.offset = off;
                m.bitpos = 0;
                off += msize;
                continue;
            }
            inUnit = false;
            off = (off + malign - 1) / malign * malign;
            m.offset = off;
            off += msize;
        }
        if (c->kind != Type::Union) size = off;
        align = std::max(align, c->alignas_ ? c->alignas_ : 1u);
        c->align = align;
        // An over-aligned typedef moves where its data starts, not its size:
        // the game streams arrays of it at the unpadded element size.
        c->size = c->kind == Type::Typedef ? size : (size + align - 1) / align * align;
    }
}
