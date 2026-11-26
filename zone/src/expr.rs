// The integer expressions of the definitions: array sizes and enum values in the
// header, counts and conditions in the loading rules. OpenAssetTools' generator wrote
// them as C, but they are read otherwise, as the rules were written to be read: `!`
// binds looser than a comparison and `&`, `^`, `|` tighter, and && / || give one of
// their operands rather than 0 or 1.
//
// The arithmetic is 64-bit and wraps, as the compiled C++ it replaced did on x86-64; a
// shift takes its count modulo 64, as the processor's does. A count read from a damaged
// file is then a wrong number the walk refuses, never a panic.

use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Num,
    Ref,
    Neg,
    Inv,
    Not,
    Mul,
    Div,
    Mod,
    Add,
    Sub,
    Shl,
    Shr,
    BitAnd,
    BitXor,
    BitOr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Debug)]
pub struct Node {
    pub op: Op,
    /// Num: the number; Ref: which reference.
    pub value: i64,
    pub kids: Vec<Node>,
}

impl Node {
    pub fn number(value: i64) -> Node {
        Node { op: Op::Num, value, kids: Vec::new() }
    }

    /// The value, with `lookup` giving a reference's from the values of its indices.
    /// `floor_division`: `/` rounds down, as the rules' do, rather than towards zero, as
    /// the header's do; `%` always takes the sign of its divisor.
    pub fn eval(&self, lookup: &dyn Fn(usize, &[i64]) -> Result<i64>, floor_division: bool) -> Result<i64> {
        let v = |k: usize| self.kids[k].eval(lookup, floor_division);
        Ok(match self.op {
            Op::Num => self.value,
            Op::Ref => {
                let mut indices = Vec::with_capacity(self.kids.len());
                for kid in &self.kids {
                    indices.push(kid.eval(lookup, floor_division)?);
                }
                lookup(self.value as usize, &indices)?
            }
            Op::Neg => v(0)?.wrapping_neg(),
            Op::Inv => !v(0)?,
            Op::Not => i64::from(v(0)? == 0),
            Op::Mul => v(0)?.wrapping_mul(v(1)?),
            Op::Div => {
                let (a, b) = (v(0)?, v(1)?);
                if floor_division {
                    floor_div(a, b)?
                } else if b == 0 {
                    return Err(Error::new("division by zero in a definition"));
                } else {
                    a.wrapping_div(b)
                }
            }
            Op::Mod => {
                let (a, b) = (v(0)?, v(1)?);
                a.wrapping_sub(floor_div(a, b)?.wrapping_mul(b))
            }
            Op::Add => v(0)?.wrapping_add(v(1)?),
            Op::Sub => v(0)?.wrapping_sub(v(1)?),
            Op::Shl => {
                let a = v(0)?;
                a.wrapping_shl(v(1)? as u32)
            }
            Op::Shr => {
                let a = v(0)?;
                a.wrapping_shr(v(1)? as u32)
            }
            Op::BitAnd => v(0)? & v(1)?,
            Op::BitXor => v(0)? ^ v(1)?,
            Op::BitOr => v(0)? | v(1)?,
            Op::Lt => i64::from(v(0)? < v(1)?),
            Op::Le => i64::from(v(0)? <= v(1)?),
            Op::Gt => i64::from(v(0)? > v(1)?),
            Op::Ge => i64::from(v(0)? >= v(1)?),
            Op::Eq => i64::from(v(0)? == v(1)?),
            Op::Ne => i64::from(v(0)? != v(1)?),
            Op::And => {
                let a = v(0)?;
                if a != 0 { v(1)? } else { a }
            }
            Op::Or => {
                let a = v(0)?;
                if a != 0 { a } else { v(1)? }
            }
        })
    }
}

/// Division rounding down, not towards zero.
fn floor_div(a: i64, b: i64) -> Result<i64> {
    if b == 0 {
        return Err(Error::new("division by zero in a definition"));
    }
    let q = a.wrapping_div(b);
    Ok(if a.wrapping_rem(b) != 0 && ((a < 0) != (b < 0)) { q.wrapping_sub(1) } else { q })
}

fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}
fn alpha(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

/// Splits text the way the definitions' tokenisers do. `paths` keeps
/// Type::member::member together, as the loading rules write them.
pub fn tokens(text: &str, paths: bool) -> Vec<String> {
    let s = text.as_bytes();
    let mut out = Vec::new();
    let piece = |from: usize, to: usize| String::from_utf8_lossy(&s[from..to]).into_owned();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if alpha(c) {
            let mut j = i + 1;
            while j < s.len() && word(s[j]) {
                j += 1;
            }
            // Type::member::member, one token in a rule.
            while paths && j + 2 < s.len() && s[j] == b':' && s[j + 1] == b':' && alpha(s[j + 2]) {
                j += 3;
                while j < s.len() && word(s[j]) {
                    j += 1;
                }
            }
            out.push(piece(i, j));
            i = j;
            continue;
        }
        if c == b'0' && i + 2 < s.len() && s[i + 1] == b'x' && s[i + 2].is_ascii_hexdigit() {
            let mut j = i + 2;
            while j < s.len() && s[j].is_ascii_hexdigit() {
                j += 1;
            }
            out.push(piece(i, j));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
            if !paths {
                // \d+\.?\d*[fF]?
                if j < s.len() && s[j] == b'.' {
                    j += 1;
                    while j < s.len() && s[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j < s.len() && (s[j] == b'f' || s[j] == b'F') {
                    j += 1;
                }
            }
            out.push(piece(i, j));
            i = j;
            continue;
        }
        let two = &s[i..(i + 2).min(s.len())];
        let pair = matches!(two, b"<<" | b">>")
            || (!paths && two == b"::")
            || (paths && matches!(two, b"&&" | b"||" | b"==" | b"!=" | b"<=" | b">="));
        if pair {
            out.push(piece(i, i + 2));
            i += 2;
            continue;
        }
        let singles: &[u8] = if paths { b"-+*/%()<>!&|^~[]" } else { b"{}()[];,:=*&|+-~<>/%^!?." };
        if singles.contains(&c) {
            out.push(piece(i, i + 1));
        }
        i += 1;
    }
    out
}

/// C's strtoull in base `base` (0: by its prefix, as 0x1F, 017 and 15 are read) of the
/// digits `text` starts with, after blanks and a sign: what std::stoll, stol and stoul
/// read. `what` names the function for the error a text without digits, or a number
/// too large, gives, as the C++ library's exceptions name it.
pub fn c_number(text: &str, base: u32, what: &str, signed: bool) -> Result<i64> {
    let s = text.as_bytes();
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        i += 1;
    }
    let mut negative = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        negative = s[i] == b'-';
        i += 1;
    }
    let mut base = base;
    let hex_prefix = i + 1 < s.len() && s[i] == b'0' && (s[i + 1] | 0x20) == b'x';
    if (base == 0 || base == 16) && hex_prefix && i + 2 < s.len() && s[i + 2].is_ascii_hexdigit() {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if i < s.len() && s[i] == b'0' { 8 } else { 10 };
    }
    let mut value: u64 = 0;
    let mut digits = 0;
    let mut overflow = false;
    while i < s.len() {
        let Some(d) = (s[i] as char).to_digit(base) else { break };
        match value.checked_mul(u64::from(base)).and_then(|v| v.checked_add(u64::from(d))) {
            Some(v) => value = v,
            None => overflow = true,
        }
        digits += 1;
        i += 1;
    }
    if digits == 0 {
        return Err(Error::new(what));
    }
    if signed {
        let limit = if negative { 1u64 << 63 } else { i64::MAX as u64 };
        if overflow || value > limit {
            return Err(Error::new(what));
        }
        Ok(if negative { (value as i64).wrapping_neg() } else { value as i64 })
    } else {
        // strtoul's: a minus negates the unsigned value; the 64-bit unsigned long of
        // Linux, which the definitions and the rules never come near.
        if overflow {
            return Err(Error::new(what));
        }
        Ok(if negative { value.wrapping_neg() as i64 } else { value as i64 })
    }
}

/// What an identifier is, at toks[*at]: it consumes what it names and returns the
/// node, using Parser::parse for any index inside brackets.
pub type Identifier<'i> = dyn FnMut(&mut Parser, &mut usize) -> Result<Node> + 'i;

pub struct Parser<'t> {
    pub toks: &'t [String],
    pos: usize,
    end: usize,
}

impl<'t> Parser<'t> {
    pub fn new(toks: &'t [String]) -> Parser<'t> {
        Parser { toks, pos: 0, end: 0 }
    }

    /// The end of the expression being parsed.
    pub fn end(&self) -> usize {
        self.end
    }

    /// The expression in toks[at, stop).
    pub fn parse(&mut self, at: usize, stop: usize, ident: &mut Identifier) -> Result<Node> {
        let (saved_pos, saved_end) = (self.pos, self.end);
        self.pos = at;
        self.end = stop;
        let n = self.or(ident)?;
        if self.pos != self.end {
            return Err(Error::new(format!("unexpected '{}' in an expression", self.toks[self.pos])));
        }
        self.pos = saved_pos;
        self.end = saved_end;
        Ok(n)
    }

    fn at(&self, t: &str) -> bool {
        self.pos < self.end && self.toks[self.pos] == t
    }

    fn binary(op: Op, a: Node, b: Node) -> Node {
        Node { op, value: 0, kids: vec![a, b] }
    }

    fn or(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.and(ident)?;
        while self.at("||") {
            self.pos += 1;
            n = Self::binary(Op::Or, n, self.and(ident)?);
        }
        Ok(n)
    }

    fn and(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.not(ident)?;
        while self.at("&&") {
            self.pos += 1;
            n = Self::binary(Op::And, n, self.not(ident)?);
        }
        Ok(n)
    }

    fn not(&mut self, ident: &mut Identifier) -> Result<Node> {
        if self.at("!") {
            self.pos += 1;
            return Ok(Node { op: Op::Not, value: 0, kids: vec![self.not(ident)?] });
        }
        self.compare(ident)
    }

    fn compare(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.bit_or(ident)?;
        loop {
            let op = if self.at("<") {
                Op::Lt
            } else if self.at("<=") {
                Op::Le
            } else if self.at(">") {
                Op::Gt
            } else if self.at(">=") {
                Op::Ge
            } else if self.at("==") {
                Op::Eq
            } else if self.at("!=") {
                Op::Ne
            } else {
                return Ok(n);
            };
            self.pos += 1;
            n = Self::binary(op, n, self.bit_or(ident)?);
        }
    }

    fn bit_or(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.bit_xor(ident)?;
        while self.at("|") {
            self.pos += 1;
            n = Self::binary(Op::BitOr, n, self.bit_xor(ident)?);
        }
        Ok(n)
    }

    fn bit_xor(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.bit_and(ident)?;
        while self.at("^") {
            self.pos += 1;
            n = Self::binary(Op::BitXor, n, self.bit_and(ident)?);
        }
        Ok(n)
    }

    fn bit_and(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.shift(ident)?;
        while self.at("&") {
            self.pos += 1;
            n = Self::binary(Op::BitAnd, n, self.shift(ident)?);
        }
        Ok(n)
    }

    fn shift(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.sum(ident)?;
        while self.at("<<") || self.at(">>") {
            let op = if self.at("<<") { Op::Shl } else { Op::Shr };
            self.pos += 1;
            n = Self::binary(op, n, self.sum(ident)?);
        }
        Ok(n)
    }

    fn sum(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.product(ident)?;
        while self.at("+") || self.at("-") {
            let op = if self.at("+") { Op::Add } else { Op::Sub };
            self.pos += 1;
            n = Self::binary(op, n, self.product(ident)?);
        }
        Ok(n)
    }

    fn product(&mut self, ident: &mut Identifier) -> Result<Node> {
        let mut n = self.unary(ident)?;
        while self.at("*") || self.at("/") || self.at("%") {
            let op = if self.at("*") {
                Op::Mul
            } else if self.at("/") {
                Op::Div
            } else {
                Op::Mod
            };
            self.pos += 1;
            n = Self::binary(op, n, self.unary(ident)?);
        }
        Ok(n)
    }

    fn unary(&mut self, ident: &mut Identifier) -> Result<Node> {
        if self.at("-") || self.at("+") || self.at("~") {
            let t = self.toks[self.pos].clone();
            self.pos += 1;
            let inner = self.unary(ident)?;
            if t == "+" {
                return Ok(inner);
            }
            let op = if t == "-" { Op::Neg } else { Op::Inv };
            return Ok(Node { op, value: 0, kids: vec![inner] });
        }
        self.atom(ident)
    }

    fn atom(&mut self, ident: &mut Identifier) -> Result<Node> {
        if self.pos >= self.end {
            return Err(Error::new("an expression ends too early"));
        }
        let t = &self.toks[self.pos];
        let first = t.as_bytes().first().copied().unwrap_or(0);
        if t == "(" {
            self.pos += 1;
            let n = self.or(ident)?;
            if !self.at(")") {
                return Err(Error::new("a ')' is missing in an expression"));
            }
            self.pos += 1;
            return Ok(n);
        }
        if first.is_ascii_digit() {
            let n = Node::number(c_number(t, 0, "stoll", true)?);
            self.pos += 1;
            return Ok(n);
        }
        if alpha(first) {
            let mut at = self.pos;
            let n = ident(self, &mut at)?;
            self.pos = at;
            return Ok(n);
        }
        Err(Error::new(format!("unexpected '{t}' in an expression")))
    }
}
