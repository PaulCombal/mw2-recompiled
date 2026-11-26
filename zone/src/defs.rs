// The structures of a zone, read from defs/iw4_assets.h and laid out as the 360's
// compiler lays them out: 32-bit pointers, each member at its natural alignment, a
// struct padded to its largest member's.
//
// Only the C the header uses is understood: structs, unions, enums (with an underlying
// type), typedefs, arrays, pointers, bitfields and OpenAssetTools' alignment macros.
// Text between `#ifndef __zonecodegenerator` and its `#endif` is skipped, as
// OpenAssetTools' own generator skips it.

use std::collections::HashMap;

use crate::expr::{self, Node, Parser, c_number};
use crate::{Error, Result};

/// A declarator modifier: a pointer, or an array of that many.
pub const POINTER: i32 = -1;

const POINTER_SIZE: u32 = 4;

/// A type, by its place in Defs::types.
pub type TypeId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Prim,
    Enum,
    Struct,
    Union,
    Typedef,
}

#[derive(Clone, Debug)]
pub struct Member {
    /// Empty for an anonymous struct or union.
    pub name: String,
    /// The element type, after every modifier.
    pub ty: TypeId,
    /// Outermost first: POINTER or an array size.
    pub mods: Vec<i32>,
    /// A bitfield's width, or -1.
    pub bits: i32,
    pub offset: u32,
    pub bitpos: u32,
    pub size: u32,
    pub align: u32,
}

impl Member {
    fn new(ty: TypeId) -> Member {
        Member { name: String::new(), ty, mods: Vec::new(), bits: -1, offset: 0, bitpos: 0, size: 0, align: 1 }
    }

    /// A pointer (T* x, T (*x)[n]).
    pub fn is_pointer(&self) -> bool {
        self.mods.first() == Some(&POINTER)
    }

    /// A pointer somewhere in its modifiers (T* x[n] too).
    pub fn has_pointer(&self) -> bool {
        self.mods.contains(&POINTER)
    }

    /// Its array sizes.
    pub fn dims(&self) -> impl Iterator<Item = i64> + '_ {
        self.mods.iter().filter(|&&m| m != POINTER).map(|&m| i64::from(m))
    }
}

#[derive(Clone, Debug)]
pub struct Type {
    pub kind: Kind,
    pub name: String,
    pub size: u32,
    pub align: u32,
    /// How a primitive or enum is read.
    pub is_signed: bool,
    pub is_float: bool,
    pub members: Vec<Member>,
    pub alignas: u32,
    pub defined: bool,
    laid_out: bool,
    pub parent: Option<TypeId>,
}

impl Type {
    pub fn compound(&self) -> bool {
        matches!(self.kind, Kind::Struct | Kind::Union | Kind::Typedef)
    }
}

pub struct Defs {
    pub types: Vec<Type>,
    /// In the order they were first named, as the header names them.
    pub named: Vec<TypeId>,
    pub by_name: HashMap<String, TypeId>,
    pub consts: HashMap<String, i64>,
}

// (name, size, signed, float)
const PRIMS: [(&str, u32, bool, bool); 24] = [
    ("char", 1, true, false),
    ("signed char", 1, true, false),
    ("unsigned char", 1, false, false),
    ("bool", 1, false, false),
    ("int8_t", 1, true, false),
    ("uint8_t", 1, false, false),
    ("short", 2, true, false),
    ("unsigned short", 2, false, false),
    ("int16_t", 2, true, false),
    ("uint16_t", 2, false, false),
    ("int", 4, true, false),
    ("unsigned int", 4, false, false),
    ("int32_t", 4, true, false),
    ("uint32_t", 4, false, false),
    ("long", 4, true, false),
    ("unsigned long", 4, false, false),
    ("unsigned", 4, false, false),
    ("float", 4, true, true),
    ("double", 8, true, true),
    ("int64_t", 8, true, false),
    ("uint64_t", 8, false, false),
    ("long long", 8, true, false),
    ("unsigned long long", 8, false, false),
    ("void", 0, false, false),
];

/// Comments, and the parts only a compiler sees.
fn preprocess(text: &str) -> String {
    let t = text.as_bytes();
    let mut plain = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i..].starts_with(b"//") {
            while i < t.len() && t[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if t[i..].starts_with(b"/*") {
            i = match t[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(at) => i + 2 + at + 2,
                None => t.len(),
            };
            continue;
        }
        plain.push(t[i]);
        i += 1;
    }

    let mut out = Vec::with_capacity(plain.len());
    let mut skip: Vec<bool> = Vec::new();
    let mut from = 0;
    while from <= plain.len() {
        let to = plain[from..].iter().position(|&c| c == b'\n').map_or(plain.len(), |at| from + at);
        let line = &plain[from..to];
        let s = match line.iter().position(|c| !matches!(c, b' ' | b'\t' | b'\r')) {
            Some(first) => &line[first..],
            None => &[][..],
        };
        if s.first() == Some(&b'#') {
            if s.starts_with(b"#ifndef __zonecodegenerator") {
                skip.push(true);
            } else if s.starts_with(b"#if") {
                skip.push(false);
            } else if s.starts_with(b"#endif") && !skip.is_empty() {
                skip.pop();
            }
        } else if !skip.contains(&true) {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
        from = to + 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// OpenAssetTools' alignment macros: the gcc ones mean nothing on the 360, the
/// others are alignas.
fn macros(toks: Vec<String>) -> Vec<String> {
    const DROPPED: [&str; 5] = ["gcc_align", "gcc_align32", "gcc_align64", "type_align64", "tdef_align64"];
    const ALIGNING: [&str; 4] = ["type_align", "type_align32", "tdef_align", "tdef_align32"];
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let drop = DROPPED.contains(&toks[i].as_str());
        if (drop || ALIGNING.contains(&toks[i].as_str()))
            && i + 3 < toks.len()
            && toks[i + 1] == "("
            && toks[i + 2].as_bytes()[0].is_ascii_digit()
            && toks[i + 3] == ")"
        {
            if !drop {
                out.extend(["alignas".to_string(), "(".to_string(), toks[i + 2].clone(), ")".to_string()]);
            }
            i += 4;
            continue;
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// The header being read.
struct Reader<'d> {
    defs: &'d mut Defs,
    tok: Vec<String>,
    i: usize,
}

impl Defs {
    pub fn new(header: &str) -> Result<Defs> {
        let mut defs = Defs { types: Vec::new(), named: Vec::new(), by_name: HashMap::new(), consts: HashMap::new() };
        for (name, size, signed, float) in PRIMS {
            let t = defs.new_type(Kind::Prim, name);
            defs.types[t].size = size;
            defs.types[t].align = size.max(1);
            defs.types[t].is_signed = signed;
            defs.types[t].is_float = float;
            defs.name(name, t);
        }
        let tok = macros(expr::tokens(&preprocess(header), false));
        let mut reader = Reader { defs: &mut defs, tok, i: 0 };
        while reader.i < reader.tok.len() {
            reader.top()?;
        }
        for k in 0..defs.named.len() {
            let t = defs.named[k];
            if defs.types[t].compound() && defs.types[t].defined {
                defs.layout(t);
            }
        }
        Ok(defs)
    }

    /// A typedef around a struct (GfxCellTree128 = GfxCellTree, aligned) is the struct
    /// itself.
    pub fn unwrapped(&self, mut t: TypeId) -> TypeId {
        loop {
            let ty = &self.types[t];
            if ty.kind == Kind::Typedef
                && ty.members.len() == 1
                && self.types[ty.members[0].ty].compound()
                && ty.members[0].mods.is_empty()
            {
                t = ty.members[0].ty;
            } else {
                return t;
            }
        }
    }

    fn new_type(&mut self, kind: Kind, name: &str) -> TypeId {
        self.types.push(Type {
            kind,
            name: name.to_string(),
            size: 0,
            align: 1,
            is_signed: false,
            is_float: false,
            members: Vec::new(),
            alignas: 0,
            defined: false,
            laid_out: false,
            parent: None,
        });
        self.types.len() - 1
    }

    fn name(&mut self, name: &str, t: TypeId) {
        match self.by_name.get(name).copied() {
            None => {
                self.by_name.insert(name.to_string(), t);
                self.named.push(t);
            }
            Some(old) => {
                // A name defined again keeps its first place.
                for n in &mut self.named {
                    if *n == old {
                        *n = t;
                    }
                }
                self.by_name.insert(name.to_string(), t);
            }
        }
    }

    /// A member's size and alignment.
    fn member_layout(&mut self, c: TypeId, k: usize) -> (u32, u32) {
        let m = &self.types[c].members[k];
        if m.is_pointer() {
            return (POINTER_SIZE, POINTER_SIZE);
        }
        let mut n: u32 = 1;
        let mut j = 0;
        while j < m.mods.len() && m.mods[j] != POINTER {
            n = n.wrapping_mul(m.mods[j] as u32);
            j += 1;
        }
        if j < m.mods.len() {
            return (n.wrapping_mul(POINTER_SIZE), POINTER_SIZE);
        }
        let ty = m.ty;
        if self.types[ty].compound() && !self.types[ty].laid_out {
            self.layout(ty);
        }
        (n.wrapping_mul(self.types[ty].size), self.types[ty].align)
    }

    fn layout(&mut self, c: TypeId) {
        if self.types[c].laid_out {
            return;
        }
        self.types[c].laid_out = true;
        let union = self.types[c].kind == Kind::Union;
        let (mut off, mut size, mut align) = (0u32, 0u32, 1u32);
        let mut in_unit = false;
        let (mut unit_off, mut unit_size, mut unit_used) = (0u32, 0u32, 0u32);
        for k in 0..self.types[c].members.len() {
            let (msize, malign) = self.member_layout(c, k);
            let m = &mut self.types[c].members[k];
            m.size = msize;
            m.align = malign;
            align = align.max(malign);
            if union {
                m.offset = 0;
                if m.bits >= 0 {
                    m.bitpos = 0;
                }
                size = size.max(msize);
                continue;
            }
            if m.bits >= 0 {
                if in_unit && unit_size == msize && unit_used + m.bits as u32 <= msize * 8 {
                    m.offset = unit_off;
                    m.bitpos = unit_used;
                    unit_used += m.bits as u32;
                    continue;
                }
                off = off.div_ceil(malign) * malign;
                in_unit = true;
                unit_off = off;
                unit_size = msize;
                unit_used = m.bits as u32;
                m.offset = off;
                m.bitpos = 0;
                off += msize;
                continue;
            }
            in_unit = false;
            off = off.div_ceil(malign) * malign;
            m.offset = off;
            off += msize;
        }
        let ty = &mut self.types[c];
        if !union {
            size = off;
        }
        align = align.max(if ty.alignas != 0 { ty.alignas } else { 1 });
        ty.align = align;
        // An over-aligned typedef moves where its data starts, not its size: the game
        // streams arrays of it at the unpadded element size.
        ty.size = if ty.kind == Kind::Typedef { size } else { size.div_ceil(align) * align };
    }
}

impl Reader<'_> {
    fn peek(&self, k: usize) -> &str {
        self.tok.get(self.i + k).map_or("", |t| t.as_str())
    }

    fn next(&mut self) -> Result<String> {
        let t = self.tok.get(self.i).cloned().ok_or_else(|| Error::new("the definitions end too early"))?;
        self.i += 1;
        Ok(t)
    }

    fn expect(&mut self, t: &str) -> Result<()> {
        let n = self.next()?;
        if n != t {
            return Err(Error::new(format!("definitions: expected '{t}', found '{n}'")));
        }
        Ok(())
    }

    fn top(&mut self) -> Result<()> {
        match self.peek(0) {
            ";" | "}" => {
                self.next()?;
            }
            "struct" | "union" => {
                self.compound_decl(None)?;
                self.expect(";")?;
            }
            "enum" => {
                self.enum_decl()?;
                self.expect(";")?;
            }
            "typedef" => self.typedef()?,
            "namespace" => {
                self.next()?;
                self.next()?;
                self.expect("{")?;
            }
            t => return Err(Error::new(format!("definitions: unexpected '{t}'"))),
        }
        Ok(())
    }

    fn alignas(&mut self) -> Result<u32> {
        let mut a = 0u32;
        while self.peek(0) == "alignas" {
            self.next()?;
            self.expect("(")?;
            let n = self.next()?;
            a = a.max(c_number(&n, 0, "stoul", false)? as u32);
            self.expect(")")?;
        }
        Ok(a)
    }

    fn compound_kind(kind: &str) -> Kind {
        if kind == "union" { Kind::Union } else { Kind::Struct }
    }

    fn get_compound(&mut self, kind: &str, name: &str) -> TypeId {
        if let Some(&t) = self.defs.by_name.get(name) {
            return t;
        }
        let t = self.defs.new_type(Self::compound_kind(kind), name);
        self.defs.name(name, t);
        t
    }

    fn compound_decl(&mut self, parent: Option<TypeId>) -> Result<TypeId> {
        let kind = self.next()?;
        let mut align = self.alignas()?;
        let mut name = String::new();
        if self.peek(0) != "{" {
            name = self.next()?;
            align = align.max(self.alignas()?);
        }
        if self.peek(0) != "{" {
            // Declared, or used as a type.
            return Ok(self.get_compound(&kind, &name));
        }
        let c = if name.is_empty() {
            self.defs.new_type(Self::compound_kind(&kind), "")
        } else {
            self.get_compound(&kind, &name)
        };
        let ty = &mut self.defs.types[c];
        ty.alignas = ty.alignas.max(align);
        ty.parent = parent;
        self.expect("{")?;
        while self.peek(0) != "}" {
            self.member_decl(c)?;
        }
        self.expect("}")?;
        self.defs.types[c].defined = true;
        Ok(c)
    }

    fn enum_type(&mut self, name: &str) -> TypeId {
        let e = self.defs.new_type(Kind::Enum, name);
        let ty = &mut self.defs.types[e];
        ty.size = 4;
        ty.align = 4;
        ty.is_signed = true;
        e
    }

    fn enum_decl(&mut self) -> Result<TypeId> {
        self.expect("enum")?;
        let mut name = String::new();
        if self.peek(0) != "{" {
            name = self.next()?;
        }
        let mut under = None;
        if self.peek(0) == ":" {
            self.next()?;
            under = Some(self.base_type(None)?);
        }
        let e = self.enum_type(&name);
        if let Some(u) = under {
            let (size, signed) = (self.defs.types[u].size, self.defs.types[u].is_signed);
            let ty = &mut self.defs.types[e];
            ty.size = size;
            ty.align = size;
            ty.is_signed = signed;
        }
        if !name.is_empty() {
            self.defs.name(&name, e);
        }
        if self.peek(0) != "{" {
            return Ok(e);
        }
        self.expect("{")?;
        let mut v: i64 = -1;
        while self.peek(0) != "}" {
            let n = self.next()?;
            if self.peek(0) == "=" {
                self.next()?;
                let mut expr = Vec::new();
                while self.peek(0) != "," && self.peek(0) != "}" {
                    expr.push(self.next()?);
                }
                v = self.eval(&expr)?;
            } else {
                v = v.wrapping_add(1);
            }
            self.defs.consts.insert(n, v);
            if self.peek(0) == "," {
                self.next()?;
            }
        }
        self.expect("}")?;
        Ok(e)
    }

    fn opens_body(&self) -> bool {
        matches!(self.peek(0), "struct" | "union") && matches!(self.peek(1), "{" | "alignas")
    }

    fn base_type(&mut self, parent: Option<TypeId>) -> Result<TypeId> {
        while matches!(self.peek(0), "const" | "volatile" | "struct" | "union" | "enum") && !self.opens_body() {
            let t = self.next()?;
            if t == "enum" {
                let n = self.next()?;
                if let Some(&found) = self.defs.by_name.get(&n) {
                    return Ok(found);
                }
                let e = self.enum_type(&n);
                self.defs.name(&n, e);
                return Ok(e);
            }
            if t == "struct" || t == "union" {
                let n = self.next()?;
                return Ok(self.get_compound(&t, &n));
            }
        }
        if matches!(self.peek(0), "struct" | "union") {
            return self.compound_decl(parent);
        }
        if self.peek(0) == "enum" {
            return self.enum_decl();
        }
        let mut words = String::new();
        loop {
            let t = self.peek(0);
            let size_word = matches!(t, "unsigned" | "signed" | "long" | "short" | "int" | "char");
            let extends = matches!(
                words.as_str(),
                "" | "unsigned"
                    | "signed"
                    | "long"
                    | "unsigned long"
                    | "short"
                    | "unsigned short"
                    | "signed long"
                    | "long long"
            );
            if size_word && extends {
                if !words.is_empty() {
                    words.push(' ');
                }
                words += &self.next()?;
                continue;
            }
            if words.is_empty() {
                words = self.next()?;
            }
            break;
        }
        while self.peek(0) == "const" {
            self.next()?;
        }
        if words == "unsigned long long" {
            words = "uint64_t".into();
        }
        self.defs.by_name.get(&words).copied().ok_or_else(|| Error::new(format!("definitions: unknown type '{words}'")))
    }

    fn declarator(&mut self) -> Result<(String, Vec<i32>)> {
        let mut pointers = 0;
        while matches!(self.peek(0), "*" | "const") {
            if self.next()? == "*" {
                pointers += 1;
            }
        }
        let mut name = String::new();
        let mut inner = Vec::new();
        let mut nested = false;
        if self.peek(0) == "(" {
            self.next()?;
            let (n, mods) = self.declarator()?;
            self.expect(")")?;
            name = n;
            inner = mods;
            nested = true;
        } else if !matches!(self.peek(0), ";" | "," | ":" | "[") {
            name = self.next()?;
        }
        let mut mods = Vec::new();
        while self.peek(0) == "[" {
            self.next()?;
            let mut expr = Vec::new();
            while self.peek(0) != "]" {
                expr.push(self.next()?);
            }
            self.expect("]")?;
            mods.push(self.eval(&expr)? as i32);
        }
        // Arrays bind tighter than pointers.
        mods.extend(std::iter::repeat_n(POINTER, pointers));
        if nested {
            inner.extend(mods);
        } else {
            inner = mods;
        }
        Ok((name, inner))
    }

    fn member_decl(&mut self, c: TypeId) -> Result<()> {
        let base = if self.opens_body() {
            let sub = self.compound_decl(Some(c))?;
            if self.peek(0) == ";" {
                self.next()?;
                self.defs.types[c].members.push(Member::new(sub));
                return Ok(());
            }
            sub
        } else {
            self.base_type(Some(c))?
        };
        loop {
            self.alignas()?;
            let (name, mods) = self.declarator()?;
            let mut m = Member::new(base);
            m.name = name;
            m.mods = mods;
            if self.peek(0) == ":" {
                self.next()?;
                let n = self.next()?;
                m.bits = c_number(&n, 0, "stol", true)? as i32;
            }
            self.defs.types[c].members.push(m);
            if self.peek(0) == "," {
                self.next()?;
                continue;
            }
            break;
        }
        self.expect(";")
    }

    fn typedef(&mut self) -> Result<()> {
        self.expect("typedef")?;
        let mut align = self.alignas()?;
        let base = self.base_type(None)?;
        align = align.max(self.alignas()?);
        let (name, mods) = self.declarator()?;
        self.expect(";")?;
        if mods.is_empty() && align == 0 {
            self.defs.name(&name, base);
            return Ok(());
        }
        // An array or an alignment: a wrapper the size of what it wraps.
        let t = self.defs.new_type(Kind::Typedef, &name);
        let mut m = Member::new(base);
        m.mods = mods;
        let ty = &mut self.defs.types[t];
        ty.members.push(m);
        ty.alignas = align;
        ty.defined = true;
        self.defs.name(&name, t);
        Ok(())
    }

    fn eval(&self, expr: &[String]) -> Result<i64> {
        // A float literal's suffix goes; constants become their values. A hex number's
        // last digit can be an F too, and stays.
        let toks: Vec<String> = expr
            .iter()
            .map(|t| {
                let b = t.as_bytes();
                let hex = b.len() > 1 && b[0] == b'0' && (b[1] == b'x' || b[1] == b'X');
                if !b.is_empty() && !hex && b[0].is_ascii_digit() && matches!(b[b.len() - 1], b'f' | b'F') {
                    t[..t.len() - 1].to_string()
                } else {
                    t.clone()
                }
            })
            .collect();
        let consts = &self.defs.consts;
        let mut ident = |p: &mut Parser, at: &mut usize| -> Result<Node> {
            let name = &p.toks[*at];
            let value =
                *consts.get(name).ok_or_else(|| Error::new(format!("definitions: unknown constant '{name}'")))?;
            *at += 1;
            Ok(Node::number(value))
        };
        let mut parser = Parser::new(&toks);
        parser.parse(0, toks.len(), &mut ident)?.eval(&|_, _| Ok(0), false)
    }
}
