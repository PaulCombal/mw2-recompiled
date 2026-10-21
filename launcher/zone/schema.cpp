#include "schema.h"

#include <algorithm>
#include <cctype>
#include <functional>
#include <unordered_set>

namespace zone
{
    namespace
    {
        bool Space(char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; }

        std::string Strip(std::string_view s)
        {
            size_t a = 0, b = s.size();
            while (a < b && Space(s[a])) a++;
            while (b > a && Space(s[b - 1])) b--;
            return std::string(s.substr(a, b - a));
        }

        std::vector<std::string> Words(std::string_view s)
        {
            std::vector<std::string> out;
            size_t i = 0;
            while (i < s.size())
            {
                while (i < s.size() && Space(s[i])) i++;
                size_t j = i;
                while (j < s.size() && !Space(s[j])) j++;
                if (j > i) out.emplace_back(s.substr(i, j - i));
                i = j;
            }
            return out;
        }

        // What is left after the first `n` words (runs of blanks between them).
        std::string After(std::string_view s, int n)
        {
            size_t i = 0;
            for (int k = 0; k < n; k++)
            {
                while (i < s.size() && Space(s[i])) i++;
                while (i < s.size() && !Space(s[i])) i++;
            }
            while (i < s.size() && Space(s[i])) i++;
            return std::string(s.substr(i));
        }

        std::vector<std::string> Split(const std::string& s, const std::string& by)
        {
            std::vector<std::string> out;
            size_t from = 0;
            while (true)
            {
                const size_t at = s.find(by, from);
                out.push_back(s.substr(from, at == std::string::npos ? std::string::npos : at - from));
                if (at == std::string::npos) return out;
                from = at + by.size();
            }
        }

        bool HasPointer(const std::vector<int>& mods)
        {
            return std::find(mods.begin(), mods.end(), kPointer) != mods.end();
        }
    }

    // ---- members ------------------------------------------------------------

    const Expr* MemberInfo::Count(size_t mod, int64_t combined) const
    {
        auto byIndex = countsByIndex.find(mod);
        if (byIndex != countsByIndex.end())
        {
            auto e = byIndex->second.find(combined);
            if (e != byIndex->second.end() && e->second) return e->second;
        }
        auto e = counts.find(mod);
        return e == counts.end() ? nullptr : e->second;
    }

    static bool IsArrayCount(const Expr* e) { return e && (!e->constant || *e->constant > 1); }

    bool MemberInfo::CountIsArray(size_t mod, int64_t combined) const { return IsArrayCount(Count(mod, combined)); }

    bool MemberInfo::AnyCountIsArray(size_t mod) const
    {
        auto e = counts.find(mod);
        if (e != counts.end() && IsArrayCount(e->second)) return true;
        auto byIndex = countsByIndex.find(mod);
        if (byIndex == countsByIndex.end()) return false;
        for (const auto& [index, count] : byIndex->second)
            if (IsArrayCount(count)) return true;
        return false;
    }

    bool MemberInfo::CountStaticZero(size_t mod) const
    {
        auto e = counts.find(mod);
        if (e == counts.end() || !e->second->constant || *e->second->constant != 0) return false;
        auto byIndex = countsByIndex.find(mod);
        return byIndex == countsByIndex.end() || byIndex->second.empty();
    }

    MemberInfo* StructInfo::Find(const std::string& n) const
    {
        for (MemberInfo* m : members)
            if (m->name == n) return m;
        return nullptr;
    }

    // ---- the schema ---------------------------------------------------------

    Schema::Schema(std::string_view header, std::string_view commands,
                   const std::function<std::string(const std::string&)>& include)
        : defs(header)
    {
        for (Type* t : defs.named)
            if (t->Compound() && t->defined) Info(t);

        // #include "XAssets/X.txt": the rules of each asset, in place.
        std::string text;
        size_t from = 0;
        while (from < commands.size())
        {
            size_t to = commands.find('\n', from);
            if (to == std::string_view::npos) to = commands.size();
            const std::string line(commands.substr(from, to - from));
            const std::string s = Strip(line);
            if (s.rfind("#include", 0) == 0)
            {
                const size_t a = s.find('"'), b = s.rfind('"');
                if (a == std::string::npos || b <= a) throw Error("rules: a bad #include: " + s);
                text += include(s.substr(a + 1, b - a - 1));
            }
            else text += line;
            text += '\n';
            from = to + 1;
        }
        Commands(text);
        PostProcess();
    }

    StructInfo* Schema::Info(const Type* t)
    {
        auto found = byType.find(t);
        if (found != byType.end()) return found->second;
        StructInfo& info = structs.emplace_back();
        info.ctype = t;
        info.name = t->name.empty() ? "__anon" + std::to_string(++anonymous) : t->name;
        info.isUnion = t->kind == Type::Union;
        byType[t] = &info;
        infos[info.name] = &info;
        for (const Member& m : t->members)
        {
            MemberInfo& mi = members.emplace_back();
            mi.parent = &info;
            mi.m = &m;
            mi.name = m.name;
            if (m.type->Compound() && m.type->defined) mi.type = Info(m.type);
            info.members.push_back(&mi);
        }
        info.ordered = info.members;
        return &info;
    }

    std::vector<MemberInfo*> Schema::FindMember(const StructInfo* info, const std::string& name) const
    {
        if (!info) return {};
        if (MemberInfo* m = info->Find(name)) return { m };
        for (MemberInfo* a : info->members)
        {
            if (!a->name.empty() || !a->type) continue;
            auto sub = FindMember(a->type, name);
            if (!sub.empty())
            {
                sub.insert(sub.begin(), a);
                return sub;
            }
        }
        return {};
    }

    std::pair<StructInfo*, std::vector<MemberInfo*>> Schema::Resolve(const std::string& path, StructInfo* root)
    {
        std::vector<std::string> parts = Split(path, "::");
        StructInfo* info = nullptr;
        auto named = infos.find(parts[0]);
        if (named != infos.end() && (!root || !root->Find(parts[0])))
        {
            info = named->second;
            parts.erase(parts.begin());
        }
        if (!info) info = root;
        if (!info) throw Error("rules: nothing to find " + path + " in");
        std::vector<MemberInfo*> chain;
        const StructInfo* cur = info;
        for (const std::string& p : parts)
        {
            auto m = FindMember(cur, p);
            if (m.empty()) throw Error("rules: " + path + ": no member " + p + " in " + (cur ? cur->name : "?"));
            chain.insert(chain.end(), m.begin(), m.end());
            cur = m.back()->type;
        }
        return { info, chain };
    }

    const Block* Schema::BlockNamed(const std::string& name) const
    {
        for (const Block& b : blocks)
            if (b.name == name) return &b;
        throw Error("rules: no block " + name);
    }

    const Expr* Schema::Compile(const std::string& text, StructInfo* root)
    {
        Expr& e = exprs.emplace_back();
        const std::vector<std::string> toks = Tokens(text, true);
        ExprParser parser(toks, [&](ExprParser& p, size_t& at) -> std::unique_ptr<Node> {
            const std::string& t = p.toks[at];
            auto n = std::make_unique<Node>();
            if (auto c = defs.consts.find(t); c != defs.consts.end())
            {
                n->value = c->second;
                at++;
                return n;
            }
            if (t == "never")
            {
                at++;
                return n;
            }
            // 'Type - Owner::array': which element of that array the Type being
            // loaded is.
            if (infos.count(t) && at + 2 < p.End() && p.toks[at + 1] == "-" &&
                p.toks[at + 2].find("::") != std::string::npos)
            {
                ExprRef ref;
                ref.isIndex = true;
                ref.info = infos[t];
                n->op = Node::Ref;
                n->value = int64_t(e.refs.size());
                e.refs.push_back(ref);
                at += 3;
                return n;
            }
            auto [info, chain] = Resolve(t, root);
            ExprRef ref;
            ref.info = info;
            ref.chain = chain;
            for (MemberInfo* m : chain) ref.names.push_back(m->name);
            n->op = Node::Ref;
            n->value = int64_t(e.refs.size());
            e.refs.push_back(ref);
            while (at + 1 < p.End() && p.toks[at + 1] == "[")
            {
                size_t j = at + 1;
                int depth = 0;
                while (true)
                {
                    if (p.toks[j] == "[") depth++;
                    else if (p.toks[j] == "]") depth--;
                    if (!depth) break;
                    j++;
                }
                n->kids.push_back(p.Parse(at + 2, j));
                at = j;
            }
            at++;
            return n;
        });
        e.root = parser.Parse(0, toks.size());
        if (e.refs.empty()) e.constant = e.root->Eval({}, true);
        return &e;
    }

    void Schema::Commands(const std::string& raw)
    {
        std::string text;
        for (size_t i = 0; i < raw.size();)
        {
            if (raw.compare(i, 2, "//") == 0)
            {
                while (i < raw.size() && raw[i] != '\n') i++;
                continue;
            }
            text += raw[i++];
        }
        StructInfo* use = nullptr;
        for (const std::string& statement : Split(text, ";"))
        {
            const std::string s = Strip(statement);
            if (s.empty()) continue;
            const std::vector<std::string> w = Words(s);
            auto get = [&](const std::string& name) {
                auto found = infos.find(name);
                if (found == infos.end()) throw Error("rules: no type " + name);
                return found->second;
            };
            if (w[0] == "game" || w[0] == "wordsize") continue;
            if (w[0] == "asset")
            {
                StructInfo* info = get(w.at(1));
                info->asset = w.at(2);
                assets[w[2]] = info;
                continue;
            }
            if (w[0] == "block")
            {
                blocks.push_back({ w.at(1), w.at(2), w.size() > 3 && w[3] == "default" });
                continue;
            }
            if (w[0] == "use")
            {
                use = get(w.at(1));
                continue;
            }
            if (w[0].rfind("reorder", 0) == 0)
            {
                Reorder(s, use);
                continue;
            }
            if (w[0] != "set") throw Error("rules: unknown command: " + s);
            const std::string cmd = w.at(1);
            const std::string rest = w.size() > 2 ? After(s, 2) : "";
            if (cmd == "block")
            {
                const auto a = Words(rest);
                if (a.size() == 1) use->block = BlockNamed(a[0]);
                else Resolve(a.at(0), use).second.back()->block = BlockNamed(a.at(1));
            }
            else if (cmd == "string" || cmd == "scriptstring" || cmd == "reusable")
            {
                MemberInfo* m = Resolve(Strip(rest), use).second.back();
                if (cmd == "string") m->isString = true;
                else if (cmd == "scriptstring") m->isScriptString = true;
                else m->isReusable = true;
            }
            else if (cmd == "allocalign")
            {
                const auto a = Words(rest);
                auto [info, chain] = Resolve(a.at(0), use);
                const uint32_t align = uint32_t(std::stoul(a.at(1)));
                if (!chain.empty()) chain.back()->allocAlign = align;
                else info->allocAlign = align;
            }
            else if (cmd == "condition")
            {
                const std::string target = Words(rest).at(0);
                auto [info, chain] = Resolve(target, use);
                chain.back()->condition = Compile(After(rest, 1), info);
            }
            else if (cmd == "count") Count(rest, use);
            else if (cmd == "arraysize")
            {
                const std::string target = Words(rest).at(0);
                auto [info, chain] = Resolve(target, use);
                MemberInfo* m = chain.back();
                size_t k = 0;
                while (k < m->m->mods.size() && m->m->mods[k] == kPointer) k++;
                m->arraySize[k] = Compile(After(rest, 1), info);
            }
            else if (cmd == "blockcond")
            {
                // The condition, then the block when it holds and the one when not.
                const std::string target = Words(rest).at(0);
                std::string cond = Strip(After(rest, 1));
                std::string blocksNamed[2];
                for (int k = 1; k >= 0; k--)
                {
                    const size_t at = cond.find_last_of(" \t\r\n");
                    if (at == std::string::npos) throw Error("rules: a bad blockcond: " + s);
                    blocksNamed[k] = cond.substr(at + 1);
                    cond = Strip(cond.substr(0, at));
                }
                auto [info, chain] = Resolve(target, use);
                MemberInfo* m = chain.back();
                m->blockCond.cond = Compile(cond, info);
                m->blockCond.ifTrue = BlockNamed(blocksNamed[0]);
                m->blockCond.ifFalse = BlockNamed(blocksNamed[1]);
                m->block = m->blockCond.ifFalse;
            }
            else if (cmd == "assetref" || cmd == "action" || cmd == "name") continue;
            else throw Error("rules: unknown set command: " + s);
        }
    }

    void Schema::Count(const std::string& rest, StructInfo* use)
    {
        // [*...] target[index]... expression
        size_t i = 0;
        int stars = 0;
        while (i < rest.size() && rest[i] == '*') { stars++; i++; }
        while (i < rest.size() && Space(rest[i])) i++;
        size_t j = i;
        while (j < rest.size() && (std::isalnum(static_cast<unsigned char>(rest[j])) || rest[j] == '_' || rest[j] == ':')) j++;
        const std::string target = rest.substr(i, j - i);
        std::vector<std::string> indices;
        while (true)
        {
            size_t k = j;
            while (k < rest.size() && Space(rest[k])) k++;
            if (k >= rest.size() || rest[k] != '[') break;
            const size_t close = rest.find(']', k);
            if (close == std::string::npos) throw Error("rules: a bad count: " + rest);
            indices.push_back(Strip(rest.substr(k + 1, close - k - 1)));
            j = close + 1;
        }
        const std::string expr = Strip(rest.substr(j));

        auto [info, chain] = Resolve(target, use);
        MemberInfo* member = chain.back();
        const std::vector<int>& mods = member->m->mods;
        size_t pointer = mods.size();
        int skip = stars;
        for (size_t k = 0; k < mods.size(); k++)
        {
            if (mods[k] != kPointer) continue;
            if (skip) { skip--; continue; }
            pointer = k;
            break;
        }
        if (pointer == mods.size()) throw Error("rules: no pointer to count: " + rest);
        const Expr* e = Compile(expr, info);
        if (indices.empty())
        {
            member->counts[pointer] = e;
            return;
        }
        std::vector<int> sizes;
        for (int m : mods)
            if (m != kPointer) sizes.push_back(m);
        std::vector<int64_t> depth;
        int64_t cur = 1;
        for (size_t k = sizes.size(); k > 0; k--)
        {
            if (k < sizes.size()) cur *= sizes[k];
            depth.push_back(cur);
        }
        std::reverse(depth.begin(), depth.end());
        int64_t combined = 0;
        for (size_t k = 0; k < depth.size() && k < indices.size(); k++)
        {
            const std::string& ix = indices[k];
            const int64_t v = std::isdigit(static_cast<unsigned char>(ix[0])) ? std::stoll(ix) : defs.consts.at(ix);
            combined += depth[k] * v;
        }
        member->countsByIndex[pointer][combined] = e;
    }

    void Schema::Reorder(const std::string& s, StructInfo* use)
    {
        const size_t colon = s.find(':');
        const std::vector<std::string> head = Words(s.substr(0, colon));
        StructInfo* info = use;
        if (head.size() > 1)
        {
            auto [found, chain] = Resolve(head[1], nullptr);
            info = chain.empty() ? found : chain.back()->type;
        }
        std::vector<std::string> names = Words(s.substr(colon + 1));
        // "reorder: ... anchor a b": up to the anchor stays, then the named
        // members, then the rest (OpenAssetTools' SequenceReorder).
        std::string anchor;
        if (!names.empty() && names[0] == "...")
        {
            anchor = names.at(1);
            names.erase(names.begin(), names.begin() + 2);
        }
        std::vector<MemberInfo*> old = info->ordered, picked;
        for (const std::string& n : names)
        {
            auto it = std::find_if(old.begin(), old.end(), [&](MemberInfo* m) { return m->name == n; });
            if (it == old.end()) throw Error("rules: reorder: no member " + n + " in " + info->name);
            picked.push_back(*it);
            old.erase(it);
        }
        std::vector<MemberInfo*> first;
        while (!anchor.empty() && !old.empty())
        {
            MemberInfo* m = old.front();
            old.erase(old.begin());
            first.push_back(m);
            if (m->name == anchor) break;
        }
        info->ordered = first;
        info->ordered.insert(info->ordered.end(), picked.begin(), picked.end());
        info->ordered.insert(info->ordered.end(), old.begin(), old.end());
    }

    // ---- what the walk needs, worked out once -------------------------------

    void Schema::PostProcess()
    {
        auto pointerToCount = [](const MemberInfo* m) {
            const auto& mods = m->m->mods;
            for (size_t k = 0; k < mods.size(); k++)
                if (mods[k] == kPointer && !m->CountStaticZero(k)) return true;
            return false;
        };

        // In the order the structs were made: a cycle is cut where it is met,
        // so the order decides.
        std::unordered_map<const StructInfo*, bool> memo;
        std::function<bool(const StructInfo*, std::vector<const StructInfo*>&)> leaf =
            [&](const StructInfo* info, std::vector<const StructInfo*>& stack) {
                if (auto found = memo.find(info); found != memo.end()) return found->second;
                if (std::find(stack.begin(), stack.end(), info) != stack.end()) return true;
                bool isLeaf = true;
                for (const MemberInfo* m : info->ordered)
                {
                    if (Ignored(m)) continue;
                    if (m->isScriptString || m->isString || pointerToCount(m) || !m->arraySize.empty())
                    {
                        isLeaf = false;
                        break;
                    }
                    if (m->type && m->type != info)
                    {
                        stack.push_back(info);
                        const bool sub = leaf(m->type, stack);
                        stack.pop_back();
                        if (!sub)
                        {
                            isLeaf = false;
                            break;
                        }
                    }
                }
                memo[info] = isLeaf;
                return isLeaf;
            };
        for (StructInfo& info : structs)
        {
            std::vector<const StructInfo*> stack;
            info.isLeaf = leaf(&info, stack);
        }
        for (StructInfo& info : structs)
            for (MemberInfo* m : info.ordered)
                m->isLeaf = !(m->isString || m->isScriptString || (m->type && !m->type->isLeaf) || pointerToCount(m) ||
                              !m->arraySize.empty());
        // In a union, the one member without a condition loads last.
        for (StructInfo& info : structs)
        {
            if (!info.isUnion) continue;
            std::vector<MemberInfo*> free;
            for (MemberInfo* m : info.ordered)
                if (!m->condition && !m->isLeaf) free.push_back(m);
            if (free.size() != 1) continue;
            info.ordered.erase(std::find(info.ordered.begin(), info.ordered.end(), free[0]));
            info.ordered.push_back(free[0]);
        }

        // The member a struct's streamed part ends before (its first dynamic one).
        std::unordered_set<const StructInfo*> done;
        std::function<void(StructInfo*)> dynamic = [&](StructInfo* info) {
            if (!done.insert(info).second) return;
            for (MemberInfo* m : info->ordered)
            {
                if (m->type && !HasPointer(m->m->mods)) dynamic(m->type);
                if (IsDynamicMember(m))
                {
                    info->dynamic = m;
                    return;
                }
            }
        };
        for (StructInfo& info : structs) dynamic(&info);
        for (StructInfo& info : structs)
            for (MemberInfo* m : info.ordered)
            {
                if (Ignored(m)) continue;
                if ((info.isUnion && info.dynamic) || !m->isLeaf) info.used.push_back(m);
            }
    }

    bool Schema::IsDynamicMember(const MemberInfo* m) const
    {
        if (!m->arraySize.empty()) return true;
        return !HasPointer(m->m->mods) && m->type && m->type->dynamic;
    }

    MemberInfo* Schema::DynamicMember(const StructInfo* info) const { return info->dynamic; }

    bool Schema::AfterPartialLoad(const MemberInfo* m) const
    {
        return IsDynamicMember(m) || (m->parent->isUnion && m->parent->dynamic);
    }

    bool Schema::NeedsTreatment(const MemberInfo* m) const
    {
        if (Ignored(m)) return false;
        return m->isString || HasPointer(m->m->mods) || (m->type && !m->type->isLeaf) || AfterPartialLoad(m);
    }
}
