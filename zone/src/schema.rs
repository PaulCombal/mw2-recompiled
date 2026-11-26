// How the game streams the structures out of a zone, from the loading rules
// (defs/commands.txt and defs/assets/*.txt, OpenAssetTools' ZoneCode format): which
// pointers are strings, how many elements an array pointer has, which member of a union
// is there, which memory block data goes to, and the order a struct's members load in.
//
// Structs, members, expressions and blocks are kept in lists and named by their place
// in them, as the rules name one from another.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::defs::{Defs, Kind, Member, POINTER, TypeId};
use crate::expr::{self, Node, Op, Parser, c_number};
use crate::{Error, Result};

pub type StructId = usize;
pub type MemberId = usize;
pub type ExprId = usize;
pub type BlockId = usize;

#[derive(Debug)]
pub struct Block {
    /// temp, runtime or normal.
    pub kind: String,
    pub name: String,
    pub is_default: bool,
}

/// A reference in a rule's expression: a member of the struct being loaded (or of one
/// loaded around it), or the index of the element of a type being loaded.
#[derive(Debug)]
pub struct ExprRef {
    pub is_index: bool,
    pub info: StructId,
    pub names: Vec<String>,
    /// The names, resolved from info.
    pub chain: Vec<MemberId>,
}

#[derive(Debug)]
pub struct Expr {
    pub root: Node,
    pub refs: Vec<ExprRef>,
    /// When it refers to nothing.
    pub constant: Option<i64>,
}

#[derive(Debug)]
pub struct BlockCond {
    pub cond: ExprId,
    pub if_true: BlockId,
    pub if_false: BlockId,
}

#[derive(Debug)]
pub struct MemberInfo {
    pub parent: StructId,
    /// The member: the type it is declared in, and its place there.
    pub owner: TypeId,
    pub index: usize,
    pub name: String,
    pub is_string: bool,
    pub is_script_string: bool,
    pub is_reusable: bool,
    pub condition: Option<ExprId>,
    pub block: Option<BlockId>,
    pub alloc_align: u32,
    pub block_cond: Option<BlockCond>,
    /// Per pointer modifier.
    pub counts: BTreeMap<usize, ExprId>,
    pub counts_by_index: BTreeMap<usize, BTreeMap<i64, ExprId>>,
    /// Per array modifier.
    pub array_size: BTreeMap<usize, ExprId>,
    /// When the element is a struct or union.
    pub ty: Option<StructId>,
    pub is_leaf: bool,
}

#[derive(Debug)]
pub struct StructInfo {
    pub ctype: TypeId,
    pub name: String,
    /// Declared order.
    pub members: Vec<MemberId>,
    /// Load order.
    pub ordered: Vec<MemberId>,
    pub block: Option<BlockId>,
    /// The asset enum's name, when this is an asset.
    pub asset: String,
    pub alloc_align: u32,
    pub is_leaf: bool,
    pub is_union: bool,
    // Worked out once the rules are read.
    pub dynamic: Option<MemberId>,
    pub used: Vec<MemberId>,
}

pub struct Schema {
    pub defs: Defs,
    pub structs: Vec<StructInfo>,
    pub members: Vec<MemberInfo>,
    pub exprs: Vec<Expr>,
    pub blocks: Vec<Block>,
    pub infos: HashMap<String, StructId>,
    /// By asset enum name.
    pub assets: HashMap<String, StructId>,
    by_type: HashMap<TypeId, StructId>,
    anonymous: usize,
}

fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii() && space(c as u8))
}

fn words(s: &str) -> Vec<&str> {
    s.split(|c: char| c.is_ascii() && space(c as u8)).filter(|w| !w.is_empty()).collect()
}

/// What is left after the first `n` words (runs of blanks between them).
fn after(s: &str, n: usize) -> &str {
    let b = s.as_bytes();
    let mut i = 0;
    for _ in 0..n {
        while i < b.len() && space(b[i]) {
            i += 1;
        }
        while i < b.len() && !space(b[i]) {
            i += 1;
        }
    }
    while i < b.len() && space(b[i]) {
        i += 1;
    }
    &s[i..]
}

fn missing(what: &str) -> Error {
    Error::new(format!("rules: {what} is missing"))
}

impl Schema {
    /// The header, and the rules with every #include resolved by `include` (given the
    /// name as the rules write it).
    pub fn new(header: &str, commands: &str, include: &dyn Fn(&str) -> Result<String>) -> Result<Schema> {
        let mut s = Schema {
            defs: Defs::new(header)?,
            structs: Vec::new(),
            members: Vec::new(),
            exprs: Vec::new(),
            blocks: Vec::new(),
            infos: HashMap::new(),
            assets: HashMap::new(),
            by_type: HashMap::new(),
            anonymous: 0,
        };
        for k in 0..s.defs.named.len() {
            let t = s.defs.named[k];
            if s.defs.types[t].compound() && s.defs.types[t].defined {
                s.info(t);
            }
        }

        // #include "XAssets/X.txt": the rules of each asset, in place.
        let mut text = String::new();
        let mut from = 0;
        while from < commands.len() {
            let to = commands[from..].find('\n').map_or(commands.len(), |at| from + at);
            let line = &commands[from..to];
            let st = strip(line);
            if st.starts_with("#include") {
                match (st.find('"'), st.rfind('"')) {
                    (Some(a), Some(b)) if b > a => text += &include(&st[a + 1..b])?,
                    _ => return Err(Error::new(format!("rules: a bad #include: {st}"))),
                }
            } else {
                text += line;
            }
            text.push('\n');
            from = to + 1;
        }
        s.commands(&text)?;
        s.post_process();
        Ok(s)
    }

    /// The member a member info is.
    pub fn member(&self, m: MemberId) -> &Member {
        let mi = &self.members[m];
        &self.defs.types[mi.owner].members[mi.index]
    }

    /// A struct's member by name.
    pub fn find(&self, info: StructId, name: &str) -> Option<MemberId> {
        self.structs[info].members.iter().copied().find(|&m| self.members[m].name == name)
    }

    /// A member of info by name, looking into anonymous members too: the chain to it.
    pub fn find_member(&self, info: Option<StructId>, name: &str) -> Vec<MemberId> {
        let Some(info) = info else { return Vec::new() };
        if let Some(m) = self.find(info, name) {
            return vec![m];
        }
        for &a in &self.structs[info].members {
            let mi = &self.members[a];
            if !mi.name.is_empty() || mi.ty.is_none() {
                continue;
            }
            let mut sub = self.find_member(mi.ty, name);
            if !sub.is_empty() {
                sub.insert(0, a);
                return sub;
            }
        }
        Vec::new()
    }

    pub fn ignored(&self, m: MemberId) -> bool {
        self.members[m].condition.is_some_and(|c| self.exprs[c].constant == Some(0))
    }

    fn is_dynamic_member(&self, m: MemberId) -> bool {
        let mi = &self.members[m];
        if !mi.array_size.is_empty() {
            return true;
        }
        !self.member(m).has_pointer() && mi.ty.is_some_and(|t| self.structs[t].dynamic.is_some())
    }

    pub fn after_partial_load(&self, m: MemberId) -> bool {
        let parent = &self.structs[self.members[m].parent];
        self.is_dynamic_member(m) || (parent.is_union && parent.dynamic.is_some())
    }

    pub fn needs_treatment(&self, m: MemberId) -> bool {
        if self.ignored(m) {
            return false;
        }
        let mi = &self.members[m];
        mi.is_string
            || self.member(m).has_pointer()
            || mi.ty.is_some_and(|t| !self.structs[t].is_leaf)
            || self.after_partial_load(m)
    }

    fn info(&mut self, t: TypeId) -> StructId {
        if let Some(&found) = self.by_type.get(&t) {
            return found;
        }
        let ty = &self.defs.types[t];
        let name = if ty.name.is_empty() {
            self.anonymous += 1;
            format!("__anon{}", self.anonymous)
        } else {
            ty.name.clone()
        };
        let id = self.structs.len();
        self.structs.push(StructInfo {
            ctype: t,
            name: name.clone(),
            members: Vec::new(),
            ordered: Vec::new(),
            block: None,
            asset: String::new(),
            alloc_align: 0,
            is_leaf: true,
            is_union: ty.kind == Kind::Union,
            dynamic: None,
            used: Vec::new(),
        });
        self.by_type.insert(t, id);
        self.infos.insert(name, id);
        for k in 0..self.defs.types[t].members.len() {
            let m = &self.defs.types[t].members[k];
            let (mname, mty) = (m.name.clone(), m.ty);
            let mid = self.members.len();
            self.members.push(MemberInfo {
                parent: id,
                owner: t,
                index: k,
                name: mname,
                is_string: false,
                is_script_string: false,
                is_reusable: false,
                condition: None,
                block: None,
                alloc_align: 0,
                block_cond: None,
                counts: BTreeMap::new(),
                counts_by_index: BTreeMap::new(),
                array_size: BTreeMap::new(),
                ty: None,
                is_leaf: true,
            });
            if self.defs.types[mty].compound() && self.defs.types[mty].defined {
                let sub = self.info(mty);
                self.members[mid].ty = Some(sub);
            }
            self.structs[id].members.push(mid);
        }
        self.structs[id].ordered = self.structs[id].members.clone();
        id
    }

    fn resolve(&self, path: &str, root: Option<StructId>) -> Result<(StructId, Vec<MemberId>)> {
        let mut parts: Vec<&str> = path.split("::").collect();
        let mut info = None;
        if let Some(&named) = self.infos.get(parts[0])
            && root.is_none_or(|r| self.find(r, parts[0]).is_none())
        {
            info = Some(named);
            parts.remove(0);
        }
        let info = info.or(root).ok_or_else(|| Error::new(format!("rules: nothing to find {path} in")))?;
        let mut chain = Vec::new();
        let mut cur = Some(info);
        for p in parts {
            let m = self.find_member(cur, p);
            let Some(&last) = m.last() else {
                let within = cur.map_or("?", |c| self.structs[c].name.as_str());
                return Err(Error::new(format!("rules: {path}: no member {p} in {within}")));
            };
            chain.extend_from_slice(&m);
            cur = self.members[last].ty;
        }
        Ok((info, chain))
    }

    /// The last member of a resolved path.
    fn resolve_member(&self, path: &str, root: Option<StructId>) -> Result<(StructId, MemberId)> {
        let (info, chain) = self.resolve(path, root)?;
        let last = *chain.last().ok_or_else(|| Error::new(format!("rules: {path} names no member")))?;
        Ok((info, last))
    }

    fn block_named(&self, name: &str) -> Result<BlockId> {
        self.blocks.iter().position(|b| b.name == name).ok_or_else(|| Error::new(format!("rules: no block {name}")))
    }

    /// A reference, at p.toks[*at], in an expression compiled for `root`.
    fn identifier(
        &self,
        root: Option<StructId>,
        refs: &mut Vec<ExprRef>,
        p: &mut Parser,
        at: &mut usize,
    ) -> Result<Node> {
        let t = p.toks[*at].clone();
        if let Some(&c) = self.defs.consts.get(&t) {
            *at += 1;
            return Ok(Node::number(c));
        }
        if t == "never" {
            *at += 1;
            return Ok(Node::number(0));
        }
        // 'Type - Owner::array': which element of that array the Type being loaded is.
        if let Some(&info) = self.infos.get(&t)
            && *at + 2 < p.end()
            && p.toks[*at + 1] == "-"
            && p.toks[*at + 2].contains("::")
        {
            refs.push(ExprRef { is_index: true, info, names: Vec::new(), chain: Vec::new() });
            *at += 3;
            return Ok(Node { op: Op::Ref, value: refs.len() as i64 - 1, kids: Vec::new() });
        }
        let (info, chain) = self.resolve(&t, root)?;
        let names = chain.iter().map(|&m| self.members[m].name.clone()).collect();
        refs.push(ExprRef { is_index: false, info, names, chain });
        let mut n = Node { op: Op::Ref, value: refs.len() as i64 - 1, kids: Vec::new() };
        while *at + 1 < p.end() && p.toks[*at + 1] == "[" {
            let mut j = *at + 1;
            let mut depth = 0;
            loop {
                let tok = p.toks.get(j).ok_or_else(|| Error::new("a ']' is missing in an expression"))?;
                if tok == "[" {
                    depth += 1;
                } else if tok == "]" {
                    depth -= 1;
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            let mut inner = |p: &mut Parser, at: &mut usize| self.identifier(root, refs, p, at);
            n.kids.push(p.parse(*at + 2, j, &mut inner)?);
            *at = j;
        }
        *at += 1;
        Ok(n)
    }

    fn compile(&mut self, text: &str, root: StructId) -> Result<ExprId> {
        let toks = expr::tokens(text, true);
        let mut refs = Vec::new();
        let root_node = {
            let mut ident = |p: &mut Parser, at: &mut usize| self.identifier(Some(root), &mut refs, p, at);
            Parser::new(&toks).parse(0, toks.len(), &mut ident)?
        };
        let constant = if refs.is_empty() { Some(root_node.eval(&|_, _| Ok(0), true)?) } else { None };
        self.exprs.push(Expr { root: root_node, refs, constant });
        Ok(self.exprs.len() - 1)
    }

    fn commands(&mut self, raw: &str) -> Result<()> {
        let mut text = String::with_capacity(raw.len());
        let mut rest = raw;
        while !rest.is_empty() {
            match rest.find("//") {
                Some(at) => {
                    text += &rest[..at];
                    rest = &rest[at..];
                    rest = rest.find('\n').map_or("", |nl| &rest[nl..]);
                }
                None => {
                    text += rest;
                    rest = "";
                }
            }
        }
        let mut use_: Option<StructId> = None;
        for statement in text.split(';') {
            let s = strip(statement);
            if s.is_empty() {
                continue;
            }
            let w = words(s);
            let word = |k: usize| w.get(k).copied().ok_or_else(|| Error::new(format!("rules: too short: {s}")));
            let get = |schema: &Schema, name: &str| {
                schema.infos.get(name).copied().ok_or_else(|| Error::new(format!("rules: no type {name}")))
            };
            let used = || use_.ok_or_else(|| missing("a use before this"));
            match w[0] {
                "game" | "wordsize" => continue,
                "asset" => {
                    let info = get(self, word(1)?)?;
                    self.structs[info].asset = word(2)?.to_string();
                    self.assets.insert(word(2)?.to_string(), info);
                    continue;
                }
                "block" => {
                    self.blocks.push(Block {
                        kind: word(1)?.to_string(),
                        name: word(2)?.to_string(),
                        is_default: w.len() > 3 && w[3] == "default",
                    });
                    continue;
                }
                "use" => {
                    use_ = Some(get(self, word(1)?)?);
                    continue;
                }
                first if first.starts_with("reorder") => {
                    self.reorder(s, use_)?;
                    continue;
                }
                "set" => {}
                _ => return Err(Error::new(format!("rules: unknown command: {s}"))),
            }
            let cmd = word(1)?;
            let rest = if w.len() > 2 { after(s, 2) } else { "" };
            match cmd {
                "block" => {
                    let a = words(rest);
                    if a.len() == 1 {
                        let b = self.block_named(a[0])?;
                        self.structs[used()?].block = Some(b);
                    } else {
                        let target = a.first().ok_or_else(|| missing("a block's target"))?;
                        let (_, m) = self.resolve_member(target, use_)?;
                        let b = self.block_named(a.get(1).ok_or_else(|| missing("a block's name"))?)?;
                        self.members[m].block = Some(b);
                    }
                }
                "string" | "scriptstring" | "reusable" => {
                    let (_, m) = self.resolve_member(strip(rest), use_)?;
                    let mi = &mut self.members[m];
                    match cmd {
                        "string" => mi.is_string = true,
                        "scriptstring" => mi.is_script_string = true,
                        _ => mi.is_reusable = true,
                    }
                }
                "allocalign" => {
                    let a = words(rest);
                    let target = a.first().ok_or_else(|| missing("an allocalign's target"))?;
                    let (info, chain) = self.resolve(target, use_)?;
                    let align = c_number(a.get(1).ok_or_else(|| missing("an alignment"))?, 10, "stoul", false)? as u32;
                    match chain.last() {
                        Some(&m) => self.members[m].alloc_align = align,
                        None => self.structs[info].alloc_align = align,
                    }
                }
                "condition" => {
                    let target = *words(rest).first().ok_or_else(|| missing("a condition's target"))?;
                    let (info, m) = self.resolve_member(target, use_)?;
                    let e = self.compile(after(rest, 1), info)?;
                    self.members[m].condition = Some(e);
                }
                "count" => self.count(rest, use_)?,
                "arraysize" => {
                    let target = *words(rest).first().ok_or_else(|| missing("an arraysize's target"))?;
                    let (info, m) = self.resolve_member(target, use_)?;
                    let k = self.member(m).mods.iter().take_while(|&&x| x == POINTER).count();
                    let e = self.compile(after(rest, 1), info)?;
                    self.members[m].array_size.insert(k, e);
                }
                "blockcond" => {
                    // The condition, then the block when it holds and the one when not.
                    let target = *words(rest).first().ok_or_else(|| missing("a blockcond's target"))?;
                    let mut cond = strip(after(rest, 1));
                    let mut named = ["", ""];
                    for k in (0..2).rev() {
                        let at = cond
                            .rfind([' ', '\t', '\r', '\n'])
                            .ok_or_else(|| Error::new(format!("rules: a bad blockcond: {s}")))?;
                        named[k] = &cond[at + 1..];
                        cond = strip(&cond[..at]);
                    }
                    let (info, m) = self.resolve_member(target, use_)?;
                    let e = self.compile(cond, info)?;
                    let (if_true, if_false) = (self.block_named(named[0])?, self.block_named(named[1])?);
                    let mi = &mut self.members[m];
                    mi.block_cond = Some(BlockCond { cond: e, if_true, if_false });
                    mi.block = Some(if_false);
                }
                "assetref" | "action" | "name" => continue,
                _ => return Err(Error::new(format!("rules: unknown set command: {s}"))),
            }
        }
        Ok(())
    }

    fn count(&mut self, rest: &str, use_: Option<StructId>) -> Result<()> {
        // [*...] target[index]... expression
        let b = rest.as_bytes();
        let mut i = 0;
        let mut stars = 0;
        while i < b.len() && b[i] == b'*' {
            stars += 1;
            i += 1;
        }
        while i < b.len() && space(b[i]) {
            i += 1;
        }
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b':') {
            j += 1;
        }
        let target = &rest[i..j];
        let mut indices = Vec::new();
        loop {
            let mut k = j;
            while k < b.len() && space(b[k]) {
                k += 1;
            }
            if k >= b.len() || b[k] != b'[' {
                break;
            }
            let close = rest[k..].find(']').ok_or_else(|| Error::new(format!("rules: a bad count: {rest}")))? + k;
            indices.push(strip(&rest[k + 1..close]).to_string());
            j = close + 1;
        }
        let expr = strip(&rest[j..]);

        let (info, member) = self.resolve_member(target, use_)?;
        let mods = self.member(member).mods.clone();
        let mut pointer = mods.len();
        let mut skip = stars;
        for (k, &m) in mods.iter().enumerate() {
            if m != POINTER {
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            pointer = k;
            break;
        }
        if pointer == mods.len() {
            return Err(Error::new(format!("rules: no pointer to count: {rest}")));
        }
        let e = self.compile(expr, info)?;
        if indices.is_empty() {
            self.members[member].counts.insert(pointer, e);
            return Ok(());
        }
        let sizes: Vec<i64> = mods.iter().filter(|&&m| m != POINTER).map(|&m| i64::from(m)).collect();
        let mut depth = Vec::new();
        let mut cur: i64 = 1;
        for k in (1..=sizes.len()).rev() {
            if k < sizes.len() {
                cur *= sizes[k];
            }
            depth.push(cur);
        }
        depth.reverse();
        let mut combined: i64 = 0;
        for (k, ix) in indices.iter().enumerate().take(depth.len()) {
            let v = if ix.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                c_number(ix, 10, "stoll", true)?
            } else {
                *self.defs.consts.get(ix).ok_or_else(|| Error::new(format!("rules: no constant {ix}")))?
            };
            combined += depth[k] * v;
        }
        self.members[member].counts_by_index.entry(pointer).or_default().insert(combined, e);
        Ok(())
    }

    fn reorder(&mut self, s: &str, use_: Option<StructId>) -> Result<()> {
        let (head, list) = match s.find(':') {
            Some(colon) => (&s[..colon], &s[colon + 1..]),
            None => (s, s),
        };
        let head = words(head);
        let info = if head.len() > 1 {
            let (found, chain) = self.resolve(head[1], None)?;
            match chain.last() {
                None => found,
                Some(&m) => self.members[m].ty.ok_or_else(|| Error::new(format!("rules: {} is no struct", head[1])))?,
            }
        } else {
            use_.ok_or_else(|| missing("a use before a reorder"))?
        };
        let mut names = words(list);
        // "reorder: ... anchor a b": up to the anchor stays, then the named members,
        // then the rest (OpenAssetTools' SequenceReorder).
        let mut anchor = "";
        if names.first() == Some(&"...") {
            anchor = names.get(1).ok_or_else(|| missing("a reorder's anchor"))?;
            names.drain(..2);
        }
        let mut old = self.structs[info].ordered.clone();
        let mut picked = Vec::new();
        for n in names {
            let at = old
                .iter()
                .position(|&m| self.members[m].name == n)
                .ok_or_else(|| Error::new(format!("rules: reorder: no member {n} in {}", self.structs[info].name)))?;
            picked.push(old.remove(at));
        }
        let mut ordered = Vec::new();
        while !anchor.is_empty() && !old.is_empty() {
            let m = old.remove(0);
            ordered.push(m);
            if self.members[m].name == anchor {
                break;
            }
        }
        ordered.extend(picked);
        ordered.extend(old);
        self.structs[info].ordered = ordered;
        Ok(())
    }

    // ---- what the walk needs, worked out once ---------------------------------------

    fn count_static_zero(&self, m: MemberId, k: usize) -> bool {
        let mi = &self.members[m];
        let Some(&e) = mi.counts.get(&k) else { return false };
        if self.exprs[e].constant != Some(0) {
            return false;
        }
        mi.counts_by_index.get(&k).is_none_or(BTreeMap::is_empty)
    }

    fn pointer_to_count(&self, m: MemberId) -> bool {
        let mods = &self.member(m).mods;
        (0..mods.len()).any(|k| mods[k] == POINTER && !self.count_static_zero(m, k))
    }

    fn leaf(&self, info: StructId, stack: &mut Vec<StructId>, memo: &mut HashMap<StructId, bool>) -> bool {
        if let Some(&found) = memo.get(&info) {
            return found;
        }
        if stack.contains(&info) {
            return true;
        }
        let mut is_leaf = true;
        for &m in &self.structs[info].ordered {
            if self.ignored(m) {
                continue;
            }
            let mi = &self.members[m];
            if mi.is_script_string || mi.is_string || self.pointer_to_count(m) || !mi.array_size.is_empty() {
                is_leaf = false;
                break;
            }
            if let Some(t) = mi.ty
                && t != info
            {
                stack.push(info);
                let sub = self.leaf(t, stack, memo);
                stack.pop();
                if !sub {
                    is_leaf = false;
                    break;
                }
            }
        }
        memo.insert(info, is_leaf);
        is_leaf
    }

    fn mark_dynamic(&mut self, info: StructId, done: &mut HashSet<StructId>) {
        if !done.insert(info) {
            return;
        }
        for k in 0..self.structs[info].ordered.len() {
            let m = self.structs[info].ordered[k];
            if let Some(t) = self.members[m].ty
                && !self.member(m).has_pointer()
            {
                self.mark_dynamic(t, done);
            }
            if self.is_dynamic_member(m) {
                self.structs[info].dynamic = Some(m);
                return;
            }
        }
    }

    fn post_process(&mut self) {
        // In the order the structs were made: a cycle is cut where it is met, so the
        // order decides.
        let mut memo = HashMap::new();
        for info in 0..self.structs.len() {
            let mut stack = Vec::new();
            self.structs[info].is_leaf = self.leaf(info, &mut stack, &mut memo);
        }
        for info in 0..self.structs.len() {
            for k in 0..self.structs[info].ordered.len() {
                let m = self.structs[info].ordered[k];
                let mi = &self.members[m];
                let leaf = !(mi.is_string
                    || mi.is_script_string
                    || mi.ty.is_some_and(|t| !self.structs[t].is_leaf)
                    || self.pointer_to_count(m)
                    || !mi.array_size.is_empty());
                self.members[m].is_leaf = leaf;
            }
        }
        // In a union, the one member without a condition loads last.
        for info in 0..self.structs.len() {
            if !self.structs[info].is_union {
                continue;
            }
            let free: Vec<MemberId> = self.structs[info]
                .ordered
                .iter()
                .copied()
                .filter(|&m| self.members[m].condition.is_none() && !self.members[m].is_leaf)
                .collect();
            if free.len() != 1 {
                continue;
            }
            let ordered = &mut self.structs[info].ordered;
            let at = ordered.iter().position(|&m| m == free[0]).unwrap_or(0);
            ordered.remove(at);
            ordered.push(free[0]);
        }

        // The member a struct's streamed part ends before (its first dynamic one).
        let mut done = HashSet::new();
        for info in 0..self.structs.len() {
            self.mark_dynamic(info, &mut done);
        }
        for info in 0..self.structs.len() {
            let mut used = Vec::new();
            for &m in &self.structs[info].ordered {
                if self.ignored(m) {
                    continue;
                }
                if (self.structs[info].is_union && self.structs[info].dynamic.is_some()) || !self.members[m].is_leaf {
                    used.push(m);
                }
            }
            self.structs[info].used = used;
        }
    }

    /// A member's count for pointer modifier `k`, by the element `combined` of the
    /// arrays around it when the rules give one for it.
    pub fn count_of(&self, m: MemberId, k: usize, combined: i64) -> Option<ExprId> {
        let mi = &self.members[m];
        if let Some(&e) = mi.counts_by_index.get(&k).and_then(|by| by.get(&combined)) {
            return Some(e);
        }
        mi.counts.get(&k).copied()
    }

    fn is_array_count(&self, e: Option<ExprId>) -> bool {
        e.is_some_and(|e| self.exprs[e].constant.is_none_or(|c| c > 1))
    }

    pub fn count_is_array(&self, m: MemberId, k: usize, combined: i64) -> bool {
        self.is_array_count(self.count_of(m, k, combined))
    }

    pub fn any_count_is_array(&self, m: MemberId, k: usize) -> bool {
        let mi = &self.members[m];
        if self.is_array_count(mi.counts.get(&k).copied()) {
            return true;
        }
        mi.counts_by_index.get(&k).is_some_and(|by| by.values().any(|&e| self.is_array_count(Some(e))))
    }
}
