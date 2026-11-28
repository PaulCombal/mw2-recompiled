// A zone as a tree of nodes: what the map converter reads a fastfile into, changes, and
// writes back out (reader.rs, writer.rs).
//
// A Struct is one struct of the zone, with its bytes as its platform lays them out (the
// PC's little-endian layout, or the 360's big-endian one) and what each of its pointers
// loaded: another Struct, an Array of them, a Leaf of plain data, a Str, a PtrList, or a
// Ref back to something loaded earlier. A struct embedded in another (a member that is a
// struct, an element of an embedded array) is a view into its owner's bytes, with
// pointers of its own.
//
// A pointer member's bytes hold what the loader reads there: 0, FOLLOWING (the data
// follows in the stream), INSERT (the loader makes a pointer to it), or a pointer back to
// earlier data, which the writer works out anew from where the Ref's target was written.
//
// Nodes have identity: a Ref names its target, and the converter collects, compares and
// marks nodes as themselves. Every node of a conversion (the PC map's and the stock files'
// it draws on) is in one Store, named by its place there, a NodeId; a stock file read on
// a thread of its own is read into a Store of its own, which the conversion's absorbs.

use crate::error::{Error, Result};
use crate::num::{load, load_f32, store, store_f32};
use mw2_zone::defs::{Kind, Member, POINTER, Type, TypeId};
use mw2_zone::schema::Schema;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const FOLLOWING: u32 = 0xFFFF_FFFF;
pub const INSERT: u32 = 0xFFFF_FFFE;

/// A type of one of the two platforms' rules.
#[derive(Clone, Copy)]
pub struct Ty {
    pub schema: &'static Schema,
    pub id: TypeId,
}

impl PartialEq for Ty {
    fn eq(&self, other: &Ty) -> bool {
        std::ptr::eq(self.schema, other.schema) && self.id == other.id
    }
}

impl Eq for Ty {}

impl std::hash::Hash for Ty {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        (self.schema as *const Schema as usize).hash(h);
        self.id.hash(h);
    }
}

impl std::fmt::Debug for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Ty {
    pub fn new(schema: &'static Schema, id: TypeId) -> Ty {
        Ty { schema, id }
    }

    /// The type named so in these rules.
    pub fn named(schema: &'static Schema, name: &str) -> Option<Ty> {
        schema.defs.by_name.get(name).map(|&id| Ty { schema, id })
    }

    pub fn get(self) -> &'static Type {
        &self.schema.defs.types[self.id]
    }

    pub fn name(self) -> &'static str {
        &self.get().name
    }

    pub fn kind(self) -> Kind {
        self.get().kind
    }

    pub fn size(self) -> u32 {
        self.get().size
    }

    pub fn align(self) -> u32 {
        self.get().align
    }

    pub fn members(self) -> &'static [Member] {
        &self.get().members
    }

    pub fn compound(self) -> bool {
        self.get().compound()
    }

    /// A struct or a union.
    pub fn is_record(self) -> bool {
        matches!(self.kind(), Kind::Struct | Kind::Union)
    }

    /// A member's element type, in the same rules.
    pub fn of(self, m: &Member) -> Ty {
        Ty { schema: self.schema, id: m.ty }
    }

    /// A typedef around a struct is the struct itself.
    pub fn unwrapped(self) -> Ty {
        Ty { schema: self.schema, id: self.schema.defs.unwrapped(self.id) }
    }
}

/// How many elements a member's arrays have, together.
pub fn elements(m: &Member) -> usize {
    m.dims().fold(1usize, |n, d| n.wrapping_mul(d as usize))
}

/// A node, by its place in its Store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NodeId(pub u32);

/// A struct's children, by their place in its Store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChId(pub u32);

/// Where in a struct a pointer is: its member, and the indices of the element when the
/// member is an array. The member is a name that outlives the tree (the rules' own, or a
/// literal).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChildKey {
    pub member: &'static str,
    pub idx: Vec<i32>,
}

/// What a struct's pointers loaded, in the order they loaded. Copies of a struct
/// (Store::copy) share it.
#[derive(Clone, Default, Debug)]
pub struct Children {
    pub items: Vec<(ChildKey, Option<NodeId>)>,
}

impl Children {
    pub fn get(&self, member: &str, idx: &[i32]) -> Option<NodeId> {
        self.items.iter().find(|(k, _)| k.member == member && k.idx == idx).and_then(|(_, n)| *n)
    }

    pub fn has(&self, member: &str, idx: &[i32]) -> bool {
        self.items.iter().any(|(k, _)| k.member == member && k.idx == idx)
    }

    pub fn set(&mut self, member: &'static str, n: Option<NodeId>, idx: &[i32]) {
        for (k, v) in &mut self.items {
            if k.member == member && k.idx == idx {
                *v = n;
                return;
            }
        }
        self.items.push((ChildKey { member, idx: idx.to_vec() }, n));
    }

    pub fn pop(&mut self, member: &str, idx: &[i32]) -> Option<NodeId> {
        let i = self.items.iter().position(|(k, _)| k.member == member && k.idx == idx)?;
        self.items.remove(i).1
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A pak entry of a streamed picture: (pak, start, end) of one level.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PakEntry {
    pub pak: u32,
    pub start: u32,
    pub end: u32,
}

/// The view made of an embedded member, by member and element.
#[derive(Clone, Debug)]
pub struct View {
    pub m: &'static Member,
    pub k: i64,
    pub s: NodeId,
}

/// What was streamed for a member after the struct's first part.
#[derive(Clone, Debug)]
pub struct LateValue {
    pub key: ChildKey,
    pub n: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub struct Struct {
    pub ty: Ty,
    pub big: bool,
    bytes: Vec<u8>,
    owner: Option<NodeId>,
    at: usize,
    size: usize,
    ch: Option<ChId>,
    views: Vec<View>,
    lates: Vec<LateValue>,
    /// Whether the lates come after its pointers when walked: in a tree as read, when its
    /// pointers loaded before them; not once converted.
    pub lates_after_pointers: bool,
    /// The asset struct's name, when this is an asset.
    pub asset: &'static str,
    /// The pointer the loader makes for it (a temp-block asset, or one pointed at from
    /// elsewhere), and whether it is written at its first pointer rather than where it is
    /// held.
    pub slot: Option<NodeId>,
    pub forward: bool,
    /// A streamed stock picture's pak entries.
    pub pak: Option<Arc<Vec<PakEntry>>>,
    /// For a union: the members a walk went into (an embedded struct in it), in the order
    /// it first did, which say which member is in use.
    pub used: Vec<&'static str>,
}

impl Struct {
    fn own(ty: Ty, big: bool, bytes: Vec<u8>) -> Struct {
        let size = bytes.len();
        Struct {
            ty: ty.unwrapped(),
            big,
            bytes,
            owner: None,
            at: 0,
            size,
            ch: None,
            views: Vec::new(),
            lates: Vec::new(),
            lates_after_pointers: false,
            asset: "",
            slot: None,
            forward: false,
            pak: None,
            used: Vec::new(),
        }
    }

    /// How many bytes it has: its type's, or fewer for one streamed in two parts (a
    /// struct with a dynamic member).
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn is_asset(&self) -> bool {
        !self.asset.is_empty()
    }

    pub fn is_asset_named(&self, name: &str) -> bool {
        self.asset == name
    }

    pub fn ch(&self) -> Option<ChId> {
        self.ch
    }

    pub fn set_ch(&mut self, ch: Option<ChId>) {
        self.ch = ch;
    }

    pub fn views(&self) -> &[View] {
        &self.views
    }

    pub fn lates(&self) -> &[LateValue] {
        &self.lates
    }

    /// The owner, for a view.
    pub fn owner(&self) -> Option<NodeId> {
        self.owner
    }

    pub fn at(&self) -> usize {
        self.at
    }

    /// A member by name; an unnamed one by its place, "_<index>".
    pub fn find(&self, member: &str) -> Option<&'static Member> {
        let members = self.ty.members();
        if let Some(m) = members.iter().find(|m| m.name == member) {
            return Some(m);
        }
        if member.len() > 1 && member.starts_with('_') {
            let k = atoi(&member[1..]) as usize;
            if k < members.len() && members[k].name.is_empty() {
                return Some(&members[k]);
            }
        }
        None
    }

    pub fn m(&self, member: &str) -> Result<&'static Member> {
        self.find(member).ok_or_else(|| Error::new(format!("{} has no member {member}", self.ty.name())))
    }

    pub fn late(&self, member: &str, idx: &[i32]) -> Option<NodeId> {
        self.lates.iter().find(|l| l.key.member == member && l.key.idx == idx).and_then(|l| l.n)
    }

    pub fn set_late(&mut self, member: &'static str, n: Option<NodeId>, idx: &[i32]) {
        for l in &mut self.lates {
            if l.key.member == member && l.key.idx == idx {
                l.n = n;
                return;
            }
        }
        self.lates.push(LateValue { key: ChildKey { member, idx: idx.to_vec() }, n });
    }

    fn elem_size(&self, m: &Member) -> u32 {
        if m.has_pointer() { 4 } else { self.ty.of(m).size() }
    }

    // Where element `k` of a member starts in the struct, checked to be in its bytes.
    fn member_offset(&self, m: &Member, k: usize) -> Result<usize> {
        let elem = self.elem_size(m) as usize;
        if k != 0 && (k + 1) * elem > m.size as usize {
            return Err(Error::new(format!("{}::{} has no element {k}", self.ty.name(), m.name)));
        }
        let off = m.offset as usize + k * elem;
        if off >= self.size && !(off == self.size && m.size == 0) {
            return Err(Error::new(format!("{}::{} is past its bytes", self.ty.name(), m.name)));
        }
        Ok(off)
    }
}

/// C's atoi of a decimal string: digits up to the first that is not.
fn atoi(s: &str) -> i64 {
    let s = s.trim_start();
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        v = v.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    if negative { -v } else { v }
}

/// An array of structs a pointer loaded. A tail is a copy of the end of another array,
/// written with as many items as the file asks for.
#[derive(Clone, Default, Debug)]
pub struct Array {
    pub items: Vec<Option<NodeId>>,
    pub tail: bool,
}

/// Plain data: `n` elements of a type, as raw bytes in either byte order.
#[derive(Clone, Debug)]
pub struct Leaf {
    pub t: Option<Ty>,
    pub n: i64,
    pub raw: Vec<u8>,
    pub big: bool,
}

/// A string: its bytes as text (crate::text).
#[derive(Clone, Debug)]
pub struct Str {
    pub b: String,
}

/// The items an array of pointers loaded (None for a null pointer), and the pointers as
/// streamed.
#[derive(Clone, Default, Debug)]
pub struct PtrList {
    pub items: Vec<Option<NodeId>>,
    pub raw: Option<Vec<u32>>,
}

/// A Ref's element type: a type, or (an asset list entry, a pointer, a string) a plain
/// size.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct ElemType {
    pub t: Option<Ty>,
    pub size: u32,
}

impl ElemType {
    pub fn size(&self) -> u32 {
        self.t.map_or(self.size, Ty::size)
    }
}

/// A pointer back to something loaded earlier: the value as streamed, and once the walk
/// has finished, what it points into, the byte offset in it and that object's element
/// type.
#[derive(Clone, Debug)]
pub struct Ref {
    pub val: u32,
    pub target: Option<NodeId>,
    pub rel: i64,
    pub t: ElemType,
}

/// The pointer a temp-block asset has the loader make for it, which later pointers to
/// the asset name.
#[derive(Clone, Debug)]
pub struct Slot {
    pub asset: Option<NodeId>,
}

/// An entry of the zone's asset list: the asset type's name and the asset.
#[derive(Clone, Debug)]
pub struct Entry {
    pub ty: String,
    pub asset: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub enum Node {
    Struct(Struct),
    Array(Array),
    Leaf(Leaf),
    Str(Str),
    PtrList(PtrList),
    Ref(Ref),
    Slot(Slot),
    Entry(Entry),
}

macro_rules! kind {
    ($is:ident, $as:ident, $as_mut:ident, $get:ident, $get_mut:ident, $variant:ident, $t:ty) => {
        pub fn $is(&self, n: Option<NodeId>) -> bool {
            matches!(n.map(|n| self.node(n)), Some(Node::$variant(_)))
        }
        /// The node as one of these, if it is one.
        pub fn $as(&self, n: Option<NodeId>) -> Option<&$t> {
            match n.map(|n| self.node(n)) {
                Some(Node::$variant(x)) => Some(x),
                _ => None,
            }
        }
        pub fn $as_mut(&mut self, n: Option<NodeId>) -> Option<&mut $t> {
            match n.map(|n| self.node_mut(n)) {
                Some(Node::$variant(x)) => Some(x),
                _ => None,
            }
        }
        /// The node, known to be one of these.
        pub fn $get(&self, n: NodeId) -> &$t {
            match self.node(n) {
                Node::$variant(x) => x,
                _ => panic!(concat!("not a ", stringify!($variant))),
            }
        }
        pub fn $get_mut(&mut self, n: NodeId) -> &mut $t {
            match self.node_mut(n) {
                Node::$variant(x) => x,
                _ => panic!(concat!("not a ", stringify!($variant))),
            }
        }
    };
}

/// A zone read as a tree: its header, script strings and asset list (Entries).
#[derive(Clone, Default, Debug)]
pub struct Tree {
    pub platform: String,
    pub header: Vec<u8>,
    pub script_strings: Option<NodeId>,
    pub assets: Vec<NodeId>,
}

impl Tree {
    /// The same tree in a Store that took its nodes `by` places further on.
    pub fn shifted(mut self, by: u32) -> Tree {
        self.script_strings = self.script_strings.map(|n| NodeId(n.0 + by));
        for a in &mut self.assets {
            a.0 += by;
        }
        self
    }
}

/// Every node of a conversion.
#[derive(Default)]
pub struct Store {
    nodes: Vec<Node>,
    children: Vec<Children>,
}

impl AsMut<Store> for Store {
    fn as_mut(&mut self) -> &mut Store {
        self
    }
}

impl Store {
    pub fn new() -> Store {
        Store::default()
    }

    pub fn count(&self) -> usize {
        self.nodes.len()
    }

    pub fn make(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    pub fn node(&self, n: NodeId) -> &Node {
        &self.nodes[n.0 as usize]
    }

    pub fn node_mut(&mut self, n: NodeId) -> &mut Node {
        &mut self.nodes[n.0 as usize]
    }

    pub fn make_children(&mut self) -> ChId {
        let id = ChId(self.children.len() as u32);
        self.children.push(Children::default());
        id
    }

    pub fn children(&self, c: ChId) -> &Children {
        &self.children[c.0 as usize]
    }

    pub fn children_mut(&mut self, c: ChId) -> &mut Children {
        &mut self.children[c.0 as usize]
    }

    /// Takes in every node of `other`; each NodeId of it is `by`, what this returns,
    /// places further on here.
    pub fn absorb(&mut self, other: Store) -> u32 {
        let by = self.nodes.len() as u32;
        let ch_by = self.children.len() as u32;
        let shift = |n: &mut Option<NodeId>| {
            if let Some(x) = n {
                x.0 += by;
            }
        };
        for mut node in other.nodes {
            match &mut node {
                Node::Struct(s) => {
                    if let Some(o) = &mut s.owner {
                        o.0 += by;
                    }
                    if let Some(c) = &mut s.ch {
                        c.0 += ch_by;
                    }
                    for v in &mut s.views {
                        v.s.0 += by;
                    }
                    for l in &mut s.lates {
                        shift(&mut l.n);
                    }
                    shift(&mut s.slot);
                }
                Node::Array(a) => a.items.iter_mut().for_each(shift),
                Node::PtrList(p) => p.items.iter_mut().for_each(shift),
                Node::Ref(r) => shift(&mut r.target),
                Node::Slot(s) => shift(&mut s.asset),
                Node::Entry(e) => shift(&mut e.asset),
                Node::Leaf(_) | Node::Str(_) => {}
            }
            self.nodes.push(node);
        }
        for mut c in other.children {
            for (_, n) in &mut c.items {
                shift(n);
            }
            self.children.push(c);
        }
        by
    }

    kind!(is_struct, as_struct, as_struct_mut, st, st_mut, Struct, Struct);
    kind!(is_array, as_array, as_array_mut, array, array_mut, Array, Array);
    kind!(is_leaf, as_leaf, as_leaf_mut, leaf, leaf_mut, Leaf, Leaf);
    kind!(is_str, as_str, as_str_mut, string, string_mut, Str, Str);
    kind!(is_ptr_list, as_ptr_list, as_ptr_list_mut, ptr_list, ptr_list_mut, PtrList, PtrList);
    kind!(is_ref, as_ref, as_ref_mut, reference, reference_mut, Ref, Ref);
    kind!(is_slot, as_slot, as_slot_mut, slot, slot_mut, Slot, Slot);
    kind!(is_entry, as_entry, as_entry_mut, entry, entry_mut, Entry, Entry);

    // ---- making nodes ---------------------------------------------------------------

    pub fn make_struct(&mut self, ty: Ty, big: bool, bytes: Vec<u8>) -> NodeId {
        self.make(Node::Struct(Struct::own(ty, big, bytes)))
    }

    /// A struct of zeros.
    pub fn zero(&mut self, ty: Ty, big: bool) -> NodeId {
        let size = ty.unwrapped().size() as usize;
        self.make_struct(ty, big, vec![0; size])
    }

    pub fn make_array(&mut self, items: Vec<Option<NodeId>>) -> NodeId {
        self.make(Node::Array(Array { items, tail: false }))
    }

    pub fn make_leaf(&mut self, t: Option<Ty>, n: i64, raw: Vec<u8>, big: bool) -> NodeId {
        self.make(Node::Leaf(Leaf { t, n, raw, big }))
    }

    pub fn make_str(&mut self, b: impl Into<String>) -> NodeId {
        self.make(Node::Str(Str { b: b.into() }))
    }

    pub fn make_ptr_list(&mut self, items: Vec<Option<NodeId>>, raw: Option<Vec<u32>>) -> NodeId {
        self.make(Node::PtrList(PtrList { items, raw }))
    }

    pub fn make_ref(&mut self, val: u32) -> NodeId {
        self.make(Node::Ref(Ref { val, target: None, rel: 0, t: ElemType::default() }))
    }

    pub fn make_slot(&mut self, asset: Option<NodeId>) -> NodeId {
        self.make(Node::Slot(Slot { asset }))
    }

    pub fn make_entry(&mut self, ty: impl Into<String>, asset: Option<NodeId>) -> NodeId {
        self.make(Node::Entry(Entry { ty: ty.into(), asset }))
    }

    // ---- a struct's bytes -------------------------------------------------------------

    // The bytes a struct's data starts at, to the end of what holds them (a view's, its
    // owner's).
    fn raw(&self, n: NodeId) -> &[u8] {
        let s = self.st(n);
        match s.owner {
            Some(o) => {
                let b = &self.st(o).bytes;
                &b[s.at.min(b.len())..]
            }
            None => &s.bytes,
        }
    }

    fn raw_mut(&mut self, n: NodeId) -> &mut [u8] {
        let (owner, at) = {
            let s = self.st(n);
            (s.owner, s.at)
        };
        match owner {
            Some(o) => {
                let b = &mut self.st_mut(o).bytes;
                let at = at.min(b.len());
                &mut b[at..]
            }
            None => &mut self.st_mut(n).bytes,
        }
    }

    /// A struct's bytes: its own, or for a view, those of its owner it stands for.
    pub fn data(&self, n: NodeId) -> &[u8] {
        let size = self.st(n).size;
        let raw = self.raw(n);
        &raw[..size.min(raw.len())]
    }

    pub fn data_mut(&mut self, n: NodeId) -> &mut [u8] {
        let size = self.st(n).size;
        let raw = self.raw_mut(n);
        let end = size.min(raw.len());
        &mut raw[..end]
    }

    // `len` bytes of a struct's data from `off`, which the struct (or what holds a view's
    // bytes) has.
    fn bytes_at(&self, n: NodeId, off: usize, len: usize) -> Result<&[u8]> {
        self.raw(n)
            .get(off..off + len)
            .ok_or_else(|| Error::new(format!("{} is read past its bytes", self.st(n).ty.name())))
    }

    fn bytes_at_mut(&mut self, n: NodeId, off: usize, len: usize) -> Result<&mut [u8]> {
        let name = self.st(n).ty.name();
        self.raw_mut(n).get_mut(off..off + len).ok_or_else(|| Error::new(format!("{name} is written past its bytes")))
    }

    /// `len` bytes of a struct's data from `off`, if what holds them has them (a view's
    /// owner may hold more than the view stands for).
    pub fn data_from(&self, n: NodeId, off: usize, len: usize) -> Option<&[u8]> {
        self.raw(n).get(off..off.checked_add(len)?)
    }

    /// The struct's own bytes, which a struct that is not a view keeps.
    pub fn own_bytes_mut(&mut self, n: NodeId) -> &mut Vec<u8> {
        &mut self.st_mut(n).bytes
    }

    // ---- members ------------------------------------------------------------------------

    pub fn find(&self, n: NodeId, member: &str) -> Option<&'static Member> {
        self.st(n).find(member)
    }

    pub fn m(&self, n: NodeId, member: &str) -> Result<&'static Member> {
        self.st(n).m(member)
    }

    /// The member is there: in its type, and within the struct's bytes or streamed after
    /// them.
    pub fn has(&self, n: NodeId, member: &str) -> bool {
        let s = self.st(n);
        match s.find(member) {
            Some(m) => (m.offset + m.size) as usize <= s.size || s.late(member, &[]).is_some(),
            None => false,
        }
    }

    /// A number member (or element `k` of an array of numbers).
    pub fn int(&self, n: NodeId, member: &str) -> Result<i64> {
        self.int_at(n, member, 0)
    }

    pub fn int_at(&self, n: NodeId, member: &str, k: usize) -> Result<i64> {
        let s = self.st(n);
        let m = s.m(member)?;
        if m.bits >= 0 {
            let off = s.member_offset(m, 0)?;
            let unit = load(self.bytes_at(n, off, m.size as usize)?, m.size, s.big);
            return Ok(((unit >> m.bitpos) & ((1u64 << m.bits) - 1)) as i64);
        }
        let t = s.ty.of(m).get();
        let off = s.member_offset(m, k)?;
        let v = load(self.bytes_at(n, off, t.size as usize)?, t.size, s.big);
        if t.is_signed && t.size < 8 && (v >> (t.size * 8 - 1)) & 1 != 0 {
            return Ok((v | (!0u64 << (t.size * 8))) as i64);
        }
        Ok(v as i64)
    }

    pub fn u(&self, n: NodeId, member: &str) -> Result<u64> {
        Ok(self.int(n, member)? as u64)
    }

    pub fn f(&self, n: NodeId, member: &str) -> Result<f64> {
        self.f_at(n, member, 0)
    }

    pub fn f_at(&self, n: NodeId, member: &str, k: usize) -> Result<f64> {
        let s = self.st(n);
        let m = s.m(member)?;
        let off = s.member_offset(m, k)?;
        if s.ty.of(m).size() == 8 {
            return Ok(f64::from_bits(load(self.bytes_at(n, off, 8)?, 8, s.big)));
        }
        Ok(f64::from(load_f32(self.bytes_at(n, off, 4)?, s.big)))
    }

    pub fn set(&mut self, n: NodeId, member: &str, v: i64) -> Result<()> {
        self.set_at(n, member, v, 0)
    }

    pub fn set_at(&mut self, n: NodeId, member: &str, v: i64, k: usize) -> Result<()> {
        let s = self.st(n);
        let m = s.m(member)?;
        let big = s.big;
        if m.bits >= 0 {
            let off = s.member_offset(m, 0)?;
            let p = self.bytes_at_mut(n, off, m.size as usize)?;
            let mut unit = load(p, m.size, big);
            let mask = ((1u64 << m.bits) - 1) << m.bitpos;
            unit = (unit & !mask) | (((v as u64) << m.bitpos) & mask);
            store(p, m.size, unit, big);
            return Ok(());
        }
        let size = s.elem_size(m);
        let off = s.member_offset(m, k)?;
        store(self.bytes_at_mut(n, off, size as usize)?, size, v as u64, big);
        Ok(())
    }

    pub fn set_f(&mut self, n: NodeId, member: &str, v: f64) -> Result<()> {
        self.set_f_at(n, member, v, 0)
    }

    pub fn set_f_at(&mut self, n: NodeId, member: &str, v: f64, k: usize) -> Result<()> {
        let s = self.st(n);
        let m = s.m(member)?;
        let big = s.big;
        let off = s.member_offset(m, k)?;
        if s.ty.of(m).size() == 8 {
            store(self.bytes_at_mut(n, off, 8)?, 8, v.to_bits(), big);
        } else {
            store_f32(self.bytes_at_mut(n, off, 4)?, v as f32, big);
        }
        Ok(())
    }

    /// A pointer member's value (element `k` of an array of pointers).
    pub fn ptr(&self, n: NodeId, member: &str) -> Result<u32> {
        self.ptr_at(n, member, 0)
    }

    pub fn ptr_at(&self, n: NodeId, member: &str, k: usize) -> Result<u32> {
        let s = self.st(n);
        let off = s.member_offset(s.m(member)?, k)?;
        Ok(load(self.bytes_at(n, off, 4)?, 4, s.big) as u32)
    }

    pub fn set_ptr(&mut self, n: NodeId, member: &str, v: u32) -> Result<()> {
        self.set_ptr_at(n, member, v, 0)
    }

    pub fn set_ptr_at(&mut self, n: NodeId, member: &str, v: u32, k: usize) -> Result<()> {
        let s = self.st(n);
        let big = s.big;
        let off = s.member_offset(s.m(member)?, k)?;
        store(self.bytes_at_mut(n, off, 4)?, 4, u64::from(v), big);
        Ok(())
    }

    /// A member's own bytes (a byte array, a union).
    pub fn bytes(&self, n: NodeId, member: &str) -> Result<&[u8]> {
        let s = self.st(n);
        let m = s.m(member)?;
        let off = s.member_offset(m, 0)?;
        self.bytes_at(n, off, m.size as usize)
    }

    pub fn bytes_mut(&mut self, n: NodeId, member: &str) -> Result<&mut [u8]> {
        let s = self.st(n);
        let m = s.m(member)?;
        let off = s.member_offset(m, 0)?;
        self.bytes_at_mut(n, off, m.size as usize)
    }

    /// An embedded struct or union, or element `k` of an embedded array of them: a view of
    /// its bytes (or what was streamed for it after the struct's first part).
    pub fn sub(&mut self, n: NodeId, member: &str) -> Result<NodeId> {
        let m = self.m(n, member)?;
        self.sub_m(n, m, -1)
    }

    pub fn sub_at(&mut self, n: NodeId, member: &str, k: i64) -> Result<NodeId> {
        let m = self.m(n, member)?;
        self.sub_m(n, m, k)
    }

    pub fn sub_m(&mut self, n: NodeId, m: &'static Member, k: i64) -> Result<NodeId> {
        let s = self.st(n);
        let idx: &[i32] = if k >= 0 { &[k as i32] } else { &[] };
        if let Some(late) = s.late(&m.name, idx)
            && self.is_struct(Some(late))
        {
            return Ok(late);
        }
        if let Some(v) = s.views.iter().find(|v| std::ptr::eq(v.m, m) && v.k == k) {
            return Ok(v.s);
        }
        let mt = s.ty.of(m);
        let t = mt.unwrapped();
        if !t.is_record() || m.is_pointer() {
            return Err(Error::new(format!("{}::{} is not an embedded struct", s.ty.name(), m.name)));
        }
        let off = m.offset as usize + if k > 0 { k as usize * mt.size() as usize } else { 0 };
        let top = s.owner.unwrap_or(n);
        let mut view = Struct::own(t, s.big, Vec::new());
        view.owner = Some(top);
        view.at = s.at + off;
        view.size = t.size() as usize;
        let v = self.make(Node::Struct(view));
        self.st_mut(n).views.push(View { m, k, s: v });
        Ok(v)
    }

    pub fn late(&self, n: NodeId, member: &str) -> Option<NodeId> {
        self.st(n).late(member, &[])
    }

    pub fn late_at(&self, n: NodeId, member: &str, idx: &[i32]) -> Option<NodeId> {
        self.st(n).late(member, idx)
    }

    pub fn set_late(&mut self, n: NodeId, member: &'static str, v: Option<NodeId>, idx: &[i32]) {
        self.st_mut(n).set_late(member, v, idx);
    }

    // ---- pointers -----------------------------------------------------------------------

    /// A struct's children, made when it has none.
    pub fn ch_make(&mut self, n: NodeId) -> ChId {
        if let Some(c) = self.st(n).ch {
            return c;
        }
        let c = self.make_children();
        self.st_mut(n).ch = Some(c);
        c
    }

    pub fn ch(&self, n: NodeId) -> Option<&Children> {
        self.st(n).ch.map(|c| self.children(c))
    }

    pub fn child(&self, n: NodeId, member: &str) -> Option<NodeId> {
        self.child_at(n, member, &[])
    }

    pub fn child_at(&self, n: NodeId, member: &str, idx: &[i32]) -> Option<NodeId> {
        self.ch(n).and_then(|c| c.get(member, idx))
    }

    pub fn set_child(&mut self, n: NodeId, member: &'static str, v: Option<NodeId>) {
        self.set_child_at(n, member, v, &[]);
    }

    pub fn set_child_at(&mut self, n: NodeId, member: &'static str, v: Option<NodeId>, idx: &[i32]) {
        let c = self.ch_make(n);
        self.children_mut(c).set(member, v, idx);
    }

    pub fn pop_child(&mut self, n: NodeId, member: &str) -> Option<NodeId> {
        self.pop_child_at(n, member, &[])
    }

    pub fn pop_child_at(&mut self, n: NodeId, member: &str, idx: &[i32]) -> Option<NodeId> {
        let c = self.st(n).ch?;
        self.children_mut(c).pop(member, idx)
    }

    // ---- whole structs ------------------------------------------------------------------

    // New views of `src`'s embedded members on `dst`, sharing their pointers.
    fn copy_views(&mut self, src: NodeId, dst: NodeId) -> Result<()> {
        let views = self.st(src).views.clone();
        for v in views {
            let mine = self.sub_m(dst, v.m, v.k)?;
            let (ch, used) = {
                let s = self.st(v.s);
                (s.ch, s.used.clone())
            };
            let m = self.st_mut(mine);
            m.ch = ch;
            m.used = used;
            self.copy_views(v.s, mine)?;
        }
        Ok(())
    }

    /// `n` becomes `src` (its type, bytes, notes and pointers), keeping its identity.
    pub fn assign(&mut self, n: NodeId, src: NodeId) -> Result<()> {
        let bytes = self.data(src).to_vec();
        let s = self.st(src).clone();
        let d = self.st_mut(n);
        d.ty = s.ty;
        d.big = s.big;
        d.bytes = bytes;
        d.owner = None;
        d.at = 0;
        d.size = s.size;
        d.ch = s.ch;
        d.views.clear();
        d.lates = s.lates;
        d.used = s.used;
        d.asset = s.asset;
        d.slot = s.slot;
        d.forward = s.forward;
        d.pak = s.pak;
        self.copy_views(src, n)
    }

    /// `n` becomes `made` (a struct built in another layout), keeping its identity and
    /// that of the views of it already made, which become views of the same members in
    /// the new layout.
    pub fn adopt(&mut self, n: NodeId, made: NodeId) {
        let (ty, big, bytes, ch, lates, used) = {
            let m = self.st_mut(made);
            (m.ty, m.big, std::mem::take(&mut m.bytes), m.ch, m.lates.clone(), m.used.clone())
        };
        let old = {
            let d = self.st_mut(n);
            d.ty = ty;
            d.big = big;
            d.size = bytes.len();
            d.bytes = bytes;
            d.ch = ch;
            d.lates = lates;
            d.used = used;
            std::mem::take(&mut d.views)
        };
        // The views made of `made`'s members: an old view of the same member (by name)
        // carries on as it, the rest become this struct's. (Unnamed members are told apart
        // only by their element: of the types with two, vec4_t's are plain floats, never
        // viewed.)
        let mut old = old.into_iter().map(|v| (v.m, v.k, Some(v.s))).collect::<Vec<_>>();
        self.rebind(n, n, made, &mut old);
    }

    fn reown(&mut self, top: NodeId, s: NodeId) {
        let views: Vec<NodeId> = self.st(s).views.iter().map(|w| w.s).collect();
        for w in views {
            self.st_mut(w).owner = Some(top);
            self.reown(top, w);
        }
    }

    fn rebind(
        &mut self,
        top: NodeId,
        parent: NodeId,
        from: NodeId,
        previous: &mut [(&'static Member, i64, Option<NodeId>)],
    ) {
        let views = self.st(from).views.clone();
        for v in views {
            let mut keep = None;
            for o in previous.iter_mut() {
                if let Some(s) = o.2
                    && o.0.name == v.m.name
                    && o.1 == v.k
                {
                    keep = Some(s);
                    o.2 = None;
                    break;
                }
            }
            let s = keep.unwrap_or(v.s);
            let mut inner: Vec<(&'static Member, i64, Option<NodeId>)> = match keep {
                Some(k) => std::mem::take(&mut self.st_mut(k).views).into_iter().map(|w| (w.m, w.k, Some(w.s))).collect(),
                None => Vec::new(),
            };
            let src = self.st(v.s).clone();
            {
                let d = self.st_mut(s);
                d.ty = src.ty;
                d.big = src.big;
                d.owner = Some(top);
                d.at = src.at;
                d.size = src.size;
                d.ch = src.ch;
                d.lates = src.lates;
                d.used = src.used;
            }
            if let Some(k) = keep {
                self.st_mut(k).views.clear();
                self.rebind(top, k, v.s, &mut inner);
            } else {
                self.reown(top, s);
            }
            self.st_mut(parent).views.push(View { m: v.m, k: v.k, s });
        }
    }

    /// A copy: its own bytes and views, the same pointers.
    pub fn copy(&mut self, n: NodeId) -> Result<NodeId> {
        let (ty, big) = {
            let s = self.st(n);
            (s.ty, s.big)
        };
        let bytes = self.data(n).to_vec();
        let c = self.make_struct(ty, big, bytes);
        self.assign(c, n)?;
        Ok(c)
    }

    // ---- walking a tree -----------------------------------------------------------------

    fn walked(&self, n: Option<NodeId>) -> bool {
        n.is_some_and(|n| !matches!(self.node(n), Node::Ref(_) | Node::Str(_)))
    }
}

/// What a walk pushes: a node, or an embedded array of structs, whose elements are pushed
/// when it comes off the stack, as a list's items are.
enum Item {
    Node(NodeId),
    Elements(NodeId, &'static Member),
}

/// Where a walk starts.
pub enum Roots<'r> {
    One(Option<NodeId>),
    /// Asset list entries: each, walked or not.
    Entries(&'r [NodeId]),
    Many(&'r [Option<NodeId>]),
}

impl<'r> From<NodeId> for Roots<'r> {
    fn from(n: NodeId) -> Roots<'r> {
        Roots::One(Some(n))
    }
}

impl<'r> From<Option<NodeId>> for Roots<'r> {
    fn from(n: Option<NodeId>) -> Roots<'r> {
        Roots::One(n)
    }
}

fn push_struct(st: &mut Store, s: NodeId, stack: &mut Vec<Item>) -> Result<()> {
    let (ty, size, after) = {
        let x = st.st(s);
        (x.ty, x.size, x.lates_after_pointers)
    };
    // Its members in order (embedded structs and unions, embedded arrays of them, and
    // what was streamed after its first part), then its pointers.
    for m in ty.members() {
        if m.has_pointer() {
            continue;
        }
        if st.late(s, &m.name).is_some() {
            if !after {
                for l in st.st(s).lates.clone() {
                    if l.key.member == m.name && st.walked(l.n) {
                        stack.push(Item::Node(l.n.unwrap()));
                    }
                }
            }
            continue;
        }
        let t = ty.of(m).unwrapped();
        if !t.is_record() || (m.offset + m.size) as usize > size {
            continue;
        }
        if m.mods.is_empty() {
            let v = st.sub_m(s, m, -1)?;
            stack.push(Item::Node(v));
        } else {
            stack.push(Item::Elements(s, m));
        }
    }
    if let Some(c) = st.ch(s) {
        for (_, n) in &c.items {
            if st.walked(*n) {
                stack.push(Item::Node(n.unwrap()));
            }
        }
    }
    if after {
        for l in &st.st(s).lates {
            if st.walked(l.n) {
                stack.push(Item::Node(l.n.unwrap()));
            }
        }
    }
    Ok(())
}

/// Calls `f` on every Struct, Array, Leaf, PtrList, Slot and Entry reachable from `roots`
/// without following Refs, each once: depth first, the last pushed first, a struct's
/// embedded members pushed before its pointers, each node's insides pushed after `f` saw
/// it. Conversions depend on the order: assets found in it go into the file in it.
///
/// `ctx` holds the Store (the converter itself, often), which `f` is handed again.
pub fn for_each<C: AsMut<Store>>(
    ctx: &mut C,
    roots: Roots,
    mut f: impl FnMut(&mut C, NodeId) -> Result<()>,
) -> Result<()> {
    let mut stack = Vec::new();
    {
        let st = ctx.as_mut();
        match roots {
            Roots::One(n) => {
                if st.walked(n) {
                    stack.push(Item::Node(n.unwrap()));
                }
            }
            Roots::Entries(entries) => stack.extend(entries.iter().map(|&e| Item::Node(e))),
            Roots::Many(list) => {
                for &n in list {
                    if st.walked(n) {
                        stack.push(Item::Node(n.unwrap()));
                    }
                }
            }
        }
    }
    let mut seen: HashSet<NodeId> = HashSet::new();
    while let Some(item) = stack.pop() {
        let n = match item {
            Item::Node(n) => n,
            Item::Elements(owner, m) => {
                let count = m.mods.iter().fold(1usize, |c, &x| c.wrapping_mul(x as usize));
                for k in 0..count {
                    let v = ctx.as_mut().sub_m(owner, m, k as i64)?;
                    stack.push(Item::Node(v));
                }
                continue;
            }
        };
        if !seen.insert(n) {
            continue;
        }
        f(ctx, n)?;
        let st = ctx.as_mut();
        match st.node(n) {
            Node::Struct(_) => push_struct(st, n, &mut stack)?,
            Node::Array(a) => {
                for &x in &a.items {
                    if st.walked(x) {
                        stack.push(Item::Node(x.unwrap()));
                    }
                }
            }
            Node::PtrList(p) => {
                for &x in &p.items {
                    if st.walked(x) {
                        stack.push(Item::Node(x.unwrap()));
                    }
                }
            }
            Node::Entry(e) => {
                if let Some(a) = e.asset.filter(|_| st.walked(e.asset)) {
                    stack.push(Item::Node(a));
                }
            }
            _ => {} // a Leaf or a Slot: nothing inside
        }
    }
    Ok(())
}

/// Every node for_each reaches, in its order.
pub fn collect(st: &mut Store, roots: Roots) -> Result<Vec<NodeId>> {
    let mut out = Vec::new();
    for_each(st, roots, |_, n| {
        out.push(n);
        Ok(())
    })?;
    Ok(out)
}

// ---- copying ------------------------------------------------------------------------------

/// Nodes mapped to what stands for them in a copy.
pub type Memo = HashMap<NodeId, Option<NodeId>>;

struct Copier<'m> {
    memo: &'m mut Memo,
}

impl Copier<'_> {
    // A node of `src` (or of `dst` itself), as it is.
    fn read(dst: &Store, src: Option<&Store>, n: NodeId) -> Node {
        src.unwrap_or(dst).node(n).clone()
    }

    fn copy(&mut self, dst: &mut Store, src: Option<&Store>, n: Option<NodeId>) -> Result<Option<NodeId>> {
        let Some(n) = n else { return Ok(None) };
        if let Some(&c) = self.memo.get(&n) {
            return Ok(c);
        }
        Ok(Some(match Copier::read(dst, src, n) {
            Node::Struct(s) => return self.copy_struct(dst, src, n, s).map(Some),
            Node::Array(a) => {
                let c = dst.make(Node::Array(Array { items: Vec::new(), tail: a.tail }));
                self.memo.insert(n, Some(c));
                for x in a.items {
                    let x = self.copy(dst, src, x)?;
                    dst.array_mut(c).items.push(x);
                }
                c
            }
            node @ (Node::Leaf(_) | Node::Str(_)) => {
                let c = dst.make(node);
                self.memo.insert(n, Some(c));
                c
            }
            Node::PtrList(p) => {
                let c = dst.make(Node::PtrList(PtrList { items: Vec::new(), raw: p.raw }));
                self.memo.insert(n, Some(c));
                for x in p.items {
                    let x = self.copy(dst, src, x)?;
                    dst.ptr_list_mut(c).items.push(x);
                }
                c
            }
            Node::Ref(r) => {
                let c = dst.make(Node::Ref(Ref { val: r.val, target: None, rel: r.rel, t: r.t }));
                self.memo.insert(n, Some(c));
                let target = self.copy(dst, src, r.target)?;
                dst.reference_mut(c).target = target;
                c
            }
            Node::Slot(s) => {
                let c = dst.make_slot(None);
                self.memo.insert(n, Some(c));
                let asset = self.copy(dst, src, s.asset)?;
                dst.slot_mut(c).asset = asset;
                c
            }
            Node::Entry(e) => {
                let c = dst.make_entry(e.ty, None);
                self.memo.insert(n, Some(c));
                let asset = self.copy(dst, src, e.asset)?;
                dst.entry_mut(c).asset = asset;
                c
            }
        }))
    }

    // A struct and its views are copied together; a view reached first is copied with its
    // owner.
    fn copy_struct(&mut self, dst: &mut Store, src: Option<&Store>, n: NodeId, s: Struct) -> Result<NodeId> {
        if let Some(owner) = s.owner {
            self.copy(dst, src, Some(owner))?;
            if let Some(&Some(c)) = self.memo.get(&n) {
                return Ok(c);
            }
            let name = src.unwrap_or(dst).st(owner).ty.name();
            return Err(Error::new(format!("a view of {name} its owner doesn't list")));
        }
        let bytes = src.unwrap_or(dst).data(n).to_vec();
        let c = dst.make_struct(s.ty, s.big, bytes);
        self.memo.insert(n, Some(c));
        let mut pairs = vec![(n, c)];
        let mut i = 0;
        while i < pairs.len() {
            let views = src.unwrap_or(dst).st(pairs[i].0).views.clone();
            for v in views {
                let mine = dst.sub_m(pairs[i].1, v.m, v.k)?;
                self.memo.insert(v.s, Some(mine));
                pairs.push((v.s, mine));
            }
            i += 1;
        }
        for (from, to) in pairs {
            let f = src.unwrap_or(dst).st(from).clone();
            let ch = f.ch.map(|c| src.unwrap_or(dst).children(c).clone());
            {
                let t = dst.st_mut(to);
                t.asset = f.asset;
                t.forward = f.forward;
                t.pak = f.pak.clone();
                t.used = f.used.clone();
                t.lates_after_pointers = f.lates_after_pointers;
            }
            let slot = match f.slot {
                Some(sl) => self.copy(dst, src, Some(sl))?,
                None => None,
            };
            dst.st_mut(to).slot = slot;
            if let Some(ch) = ch {
                let mine = dst.make_children();
                for (k, x) in ch.items {
                    let x = self.copy(dst, src, x)?;
                    dst.children_mut(mine).items.push((k, x));
                }
                dst.st_mut(to).ch = Some(mine);
            }
            for l in f.lates {
                let x = self.copy(dst, src, l.n)?;
                dst.st_mut(to).set_late(l.key.member, x, &l.key.idx);
            }
        }
        Ok(c)
    }
}

impl Store {
    /// A copy of everything `root` reaches, through pointers back to earlier data and slots
    /// too, each node once: what it shares, the copy shares. `memo`, when given: nodes
    /// mapped to what stands for them (a node mapped to itself is left as it is), and on
    /// return every node copied, mapped to its copy.
    pub fn deep_copy(&mut self, root: Option<NodeId>, memo: Option<&mut Memo>) -> Result<Option<NodeId>> {
        let mut own = Memo::new();
        Copier { memo: memo.unwrap_or(&mut own) }.copy(self, None, root)
    }

    pub fn deep_copy_struct(&mut self, root: NodeId, memo: Option<&mut Memo>) -> Result<NodeId> {
        Ok(self.deep_copy(Some(root), memo)?.unwrap_or(root))
    }

    /// A copy here of what `root` reaches in another Store, as deep_copy makes one.
    pub fn import(&mut self, from: &Store, root: NodeId) -> Result<NodeId> {
        let mut memo = Memo::new();
        Ok(Copier { memo: &mut memo }.copy(self, Some(from), Some(root))?.unwrap_or(root))
    }
}

/// A name that lives as long as the program, for a child's key made at run time (the rules'
/// names and literals already do): each text is kept once.
pub fn intern(text: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&found) = names.get(text) {
        return found;
    }
    let kept: &'static str = Box::leak(text.to_string().into_boxed_str());
    names.insert(kept);
    kept
}

/// A member of a type, by pointer identity.
pub fn same_member(a: &Member, b: &Member) -> bool {
    std::ptr::eq(a, b)
}

/// Whether a member's modifiers hold a pointer after position `pos`.
pub fn has_pointer_after(m: &Member, pos: usize) -> bool {
    m.mods.iter().skip(pos + 1).any(|&x| x == POINTER)
}
