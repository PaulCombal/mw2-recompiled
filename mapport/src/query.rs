// What the converter asks of a tree: an asset's name, what a pointer back to earlier data
// means, a raw file's text.

use crate::error::{Error, Result};
use crate::num::{load, store};
use crate::text;
use crate::tree::{Node, NodeId, Store, Ty};
use mw2_zone::defs::{Kind, POINTER};

/// What a child is: a Ref's target, or the child itself.
pub fn target(st: &Store, c: Option<NodeId>) -> Option<NodeId> {
    match st.as_ref(c) {
        Some(r) => r.target,
        None => c,
    }
}

fn is_asset_node(st: &Store, c: Option<NodeId>) -> bool {
    st.as_struct(c).is_some_and(|s| s.is_asset()) || st.is_ref(c)
}

/// What an alias pointing at a pointer to an asset (an asset list entry, a temp asset's
/// own slot, or a pointer member of a struct) names: that asset (or Ref), or None.
pub fn slot_asset(st: &Store, r: NodeId) -> Option<NodeId> {
    let x = st.reference(r);
    let mut tgt = x.target;
    let mut rel = x.rel;
    if let Some(e) = st.as_entry(tgt) {
        return if rel == 4 { e.asset } else { None };
    }
    if let Some(s) = st.as_slot(tgt) {
        return s.asset;
    }
    // (A union of pointers too: an effect element's visuals "the same as that element's"
    // point at the other element's union, whose pointer holds the asset.)
    let t = x.t.t?;
    if !t.is_record() {
        return None;
    }
    if let Some(list) = st.as_array(tgt) {
        let size = i64::from(t.size());
        let i = (rel / size) as usize;
        rel %= size;
        tgt = *list.items.get(i)?;
    }
    st.as_struct(tgt)?;
    let d = tgt.unwrap();
    for m in t.members() {
        if i64::from(m.offset) == rel && m.is_pointer() {
            let c = st.child(d, &m.name);
            if is_asset_node(st, c) {
                return c;
            }
        }
    }
    None
}

fn asset_in_member(st: &mut Store, t: Ty, tgt: Option<NodeId>, rel: i64) -> Result<Option<NodeId>> {
    let Some(tgt) = tgt.filter(|&n| st.is_struct(Some(n))) else { return Ok(None) };
    for m in t.members() {
        let off = i64::from(m.offset);
        if off == rel && m.is_pointer() {
            let c = st.child(tgt, &m.name);
            if is_asset_node(st, c) {
                return Ok(c);
            }
        } else if m.mods.last() == Some(&POINTER)
            && m.mods.len() > 1
            && off <= rel
            && rel < off + i64::from(m.size)
            && (rel - off) % 4 == 0
        {
            // One pointer of an array of them (FxElemMarkVisuals.materials[2], which
            // effects share: "the same material as that element's").
            let list = st.child(tgt, &m.name);
            let k = ((rel - off) / 4) as usize;
            let c = if let Some(pl) = st.as_ptr_list(list) {
                pl.items.get(k).copied().flatten()
            } else if let Some(a) = st.as_array(list) {
                a.items.get(k).copied().flatten()
            } else {
                None
            };
            if is_asset_node(st, c) {
                return Ok(c);
            }
        } else if m.mods.is_empty() && t.of(m).compound() && off <= rel && rel < off + i64::from(t.of(m).size()) {
            // A pointer inside an inline struct or union (MaterialTextureDef.u.image).
            let sub = if st.has(tgt, &m.name) { Some(st.sub_m(tgt, m, -1)?) } else { None };
            if let Some(c) = asset_in_member(st, t.of(m), sub, rel - off)? {
                return Ok(Some(c));
            }
        }
    }
    Ok(None)
}

/// If alias r points at a pointer member (inside a struct, an array of them, an asset
/// list entry, a temp asset's slot, a pointer array) that holds an asset, that asset (or
/// Ref), else None.
pub fn asset_in_slot(st: &mut Store, r: NodeId) -> Result<Option<NodeId>> {
    let x = st.reference(r);
    let mut tgt = x.target;
    let mut rel = x.rel;
    let t = x.t.t;
    if let Some(e) = st.as_entry(tgt) {
        return Ok(if rel == 4 { e.asset } else { None });
    }
    if let Some(s) = st.as_slot(tgt) {
        return Ok(s.asset);
    }
    if let Some(pl) = st.as_ptr_list(tgt) {
        // An element of another asset's pointer array (XModel.materialHandles).
        if rel % 4 != 0 || (rel / 4) as usize >= pl.items.len() {
            return Ok(None);
        }
        let c = pl.items[(rel / 4) as usize];
        return Ok(if is_asset_node(st, c) { c } else { None });
    }
    let Some(t) = t.filter(|t| t.is_record()) else { return Ok(None) };
    if let Some(list) = st.as_array(tgt) {
        let size = i64::from(t.size());
        let i = (rel / size) as usize;
        rel %= size;
        let Some(&item) = list.items.get(i) else { return Ok(None) };
        tgt = item;
    }
    asset_in_member(st, t, tgt, rel)
}

/// If alias r points at one pointer of an array of pointers inside an element of a
/// struct array (not at the array's start), what it holds.
pub fn ptr_array_value(st: &Store, r: NodeId) -> Option<NodeId> {
    let x = st.reference(r);
    let t = x.t.t?;
    let list = st.as_array(x.target)?;
    let size = i64::from(t.size());
    if t.kind() != Kind::Struct || x.rel % size == 0 {
        return None;
    }
    let i = (x.rel / size) as usize;
    let el = list.items.get(i).copied().flatten().filter(|&e| st.is_struct(Some(e)));
    let off = x.rel % size;
    for m in t.members() {
        let mo = i64::from(m.offset);
        if m.mods.last() == Some(&POINTER) && m.mods.len() > 1 && mo < off && off < mo + i64::from(m.size) {
            let Some(el) = el else { continue };
            let items = st.child(el, &m.name);
            let k = ((off - mo) / 4) as usize;
            if let Some(pl) = st.as_ptr_list(items) {
                return pl.items.get(k).copied().flatten();
            }
            if let Some(a) = st.as_array(items) {
                return a.items.get(k).copied().flatten();
            }
        }
    }
    None
}

/// An asset child, or the asset an alias refers to (an alias to a pointer that holds an
/// asset, or to a struct itself), else None.
pub fn deref(st: &mut Store, c: Option<NodeId>) -> Result<Option<NodeId>> {
    let Some(r) = c.filter(|&n| st.is_ref(Some(n))) else { return Ok(c) };
    if let Some(a) = asset_in_slot(st, r)? {
        return deref(st, Some(a));
    }
    let x = st.reference(r);
    if x.rel == 0 && st.is_struct(x.target) {
        return Ok(x.target);
    }
    Ok(None)
}

pub fn deref_struct(st: &mut Store, c: Option<NodeId>) -> Result<Option<NodeId>> {
    Ok(deref(st, c)?.filter(|&n| st.is_struct(Some(n))))
}

/// An embedded struct member (Store::sub), or None when there is none.
pub fn embedded(st: &mut Store, d: NodeId, member: &str) -> Option<NodeId> {
    let m = st.find(d, member)?;
    if !m.mods.is_empty() || !st.has(d, member) {
        return None;
    }
    let t = st.st(d).ty.of(m).unwrapped();
    if !t.is_record() {
        return None;
    }
    st.sub_m(d, m, -1).ok()
}

fn name_child(st: &mut Store, d: NodeId, member: &str) -> Option<NodeId> {
    let mut c = st.child(d, member);
    if c.is_none()
        && let Some(info) = embedded(st, d, "info")
        && st.st(info).ty.kind() == Kind::Struct
    {
        c = st.child(info, member);
    }
    c
}

/// An asset's name: its "name" pointer's string (or its info's, for a material).
pub fn asset_name(st: &mut Store, d: NodeId) -> Option<String> {
    let c = name_child(st, d, "name");
    st.as_str(c).map(|s| s.b.clone())
}

/// An asset's name, also through an alias to a string.
pub fn name(st: &mut Store, d: NodeId) -> Option<String> {
    let mut c = name_child(st, d, "name");
    if let Some(r) = st.as_ref(c)
        && r.rel == 0
        && st.is_str(r.target)
    {
        c = r.target;
    }
    st.as_str(c).map(|s| s.b.clone())
}

pub fn set_name(st: &mut Store, d: NodeId, name: impl Into<String>) {
    let mut holder = d;
    if st.child(d, "name").is_none()
        && let Some(info) = embedded(st, d, "info")
    {
        holder = info;
    }
    let s = st.make_str(name);
    st.set_child(holder, "name", Some(s));
}

/// A sound alias list's name (aliasName).
pub fn alias_name(st: &Store, d: NodeId) -> Option<String> {
    let mut c = st.child(d, "aliasName");
    if let Some(r) = st.as_ref(c)
        && r.rel == 0
        && st.is_str(r.target)
    {
        c = r.target;
    }
    st.as_str(c).map(|s| s.b.clone())
}

/// The name an asset takes a place in the game's pool under.
pub fn pool_name(st: &mut Store, d: NodeId) -> Option<String> {
    let sound = st.st(d).is_asset_named("snd_alias_list_t");
    let mut n = if sound { alias_name(st, d) } else { asset_name(st, d) };
    if n.is_none() && !sound {
        n = name(st, d);
    }
    if n.is_none()
        && let Some(f) = st.as_str(st.child(d, "filename"))
    {
        n = Some(f.b.clone());
    }
    n
}

pub fn named(n: &Option<String>) -> bool {
    n.as_ref().is_some_and(|n| !n.is_empty())
}

/// ",name" names an asset the game has loaded.
pub fn only_named(n: &Option<String>) -> bool {
    n.as_ref().is_some_and(|n| n.starts_with(','))
}

// ---- raw files --------------------------------------------------------------------------------

fn rawfile_data(st: &mut Store, d: NodeId) -> Result<Option<NodeId>> {
    let data = st.sub(d, "data")?;
    let mut c = st.child(data, "buffer");
    if c.is_none() {
        c = st.child(data, "compressedBuffer");
    }
    if let Some(r) = st.as_ref(c) {
        c = r.target;
    }
    Ok(c.filter(|&n| st.is_leaf(Some(n))))
}

/// A raw file's text (inflated when the file holds it compressed).
pub fn rawfile_text(st: &mut Store, d: NodeId) -> Result<String> {
    let lf = rawfile_data(st, d)?.ok_or_else(|| Error::new("a raw file without its text"))?;
    let mut raw = st.leaf(lf).raw.clone();
    let packed = st.int(d, "compressedLen")?;
    if packed != 0 {
        // A script, an arena: far less.
        const LARGEST: usize = 64 << 20;
        let len = st.int(d, "len")?;
        if len < 0 || len as usize > LARGEST {
            return Err(Error::new(format!("a raw file of {len} bytes")));
        }
        let input = &raw[..(packed as usize).min(raw.len())];
        // Room doubled from at least 4 KB as zlib's output fills it, while it has room for
        // no more than LARGEST: the most it can grow to.
        let mut room = (len as usize + 1).max(1 << 12);
        while room <= LARGEST {
            room *= 2;
        }
        raw = mw2_install::inflate::unzlib(input, room)
            .ok_or_else(|| Error::new("a raw file's text does not inflate"))?;
    }
    let len = st.int(d, "len")?.max(0) as usize;
    raw.truncate(len);
    Ok(text::of(&raw))
}

/// A raw file's new text, stored plain.
pub fn set_rawfile_text(st: &mut Store, d: NodeId, text: &str) -> Result<()> {
    let data = st.sub(d, "data")?;
    let mut old = st.pop_child(data, "buffer");
    if old.is_none() {
        old = st.pop_child(data, "compressedBuffer");
    }
    let lf = st.as_leaf(old).ok_or_else(|| Error::new("a raw file without its text"))?;
    let (t, big) = (lf.t, lf.big);
    let mut raw = text::bytes(text);
    st.set(d, "compressedLen", 0)?;
    st.set(d, "len", raw.len() as i64)?;
    raw.push(0);
    let n = raw.len() as i64;
    let leaf = st.make_leaf(t, n, raw, big);
    st.set_child(data, "buffer", Some(leaf));
    Ok(())
}

/// The rawfile IW4x's tools sign a file with, or one marked compressed whose data isn't a
/// zlib stream (the 360 would try to inflate it).
pub fn is_builder_signature(st: &mut Store, d: NodeId) -> Result<bool> {
    let Some(lf) = rawfile_data(st, d)? else { return Ok(false) };
    const SIGNED: &[u8] = b"FastFile built using";
    let raw = &st.leaf(lf).raw;
    if raw.starts_with(SIGNED) {
        return Ok(true);
    }
    let n = st.int(d, "compressedLen")?;
    if n == 0 {
        return Ok(false);
    }
    let raw = &st.leaf(lf).raw;
    let input = &raw[..(n as usize).min(raw.len())];
    Ok(mw2_install::inflate::unzlib(input, usize::MAX).is_none())
}

// ---- render state -----------------------------------------------------------------------------

/// (loadBits[0], loadBits[1]) of each entry of a material's stateBitsTable, or None when
/// it has none.
pub fn state_words(st: &Store, table: Option<NodeId>) -> Option<Vec<(u32, u32)>> {
    let mut table = table;
    if let Some(r) = st.as_ref(table)
        && r.rel == 0
    {
        table = r.target;
    }
    let lf = st.as_leaf(table)?;
    Some(
        lf.raw
            .chunks_exact(8)
            .map(|w| (load(w, 4, lf.big) as u32, load(&w[4..], 4, lf.big) as u32))
            .collect(),
    )
}

/// A copy of a stateBitsTable with every entry's face culling (loadBits[0] bits 14-15,
/// the same on both platforms: 1 none, 2 back, 3 front) set.
pub fn with_cull(st: &mut Store, table: Option<NodeId>, cull: u32) -> Result<NodeId> {
    let lf = st.as_leaf(table).ok_or_else(|| Error::new("a render state table that is not plain data"))?;
    let (t, n, big) = (lf.t, lf.n, lf.big);
    let mut raw = lf.raw.clone();
    for w in raw.chunks_exact_mut(8) {
        let w0 = load(w, 4, big) as u32;
        store(w, 4, u64::from((w0 & !0xC000) | (cull << 14)), big);
    }
    Ok(st.make_leaf(t, n, raw, big))
}

pub fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// A name without its leading commas (",name" names an asset the game has loaded).
pub fn without_commas(name: &str) -> String {
    name.trim_start_matches(',').to_string()
}

/// The first `most` of a list, joined with commas, and `more` after them when there are
/// others.
pub fn joined(v: &[String], most: usize, more: &str) -> String {
    let mut s = v.iter().take(most).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > most {
        s += more;
    }
    s
}

/// Each item of a child that is a list (an Array, a PtrList), or the child.
pub fn each_held(st: &Store, c: Option<NodeId>) -> Vec<Option<NodeId>> {
    match c.map(|c| st.node(c)) {
        Some(Node::Array(a)) => a.items.clone(),
        Some(Node::PtrList(p)) => p.items.clone(),
        _ => vec![c],
    }
}

/// How a struct's member holds what its pointer loaded: not a pointer, one pointer, or an
/// inline array of them (a PtrList child, or children by element).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Holder {
    None,
    One,
    Array,
}

pub fn holder_of(st: &Store, o: NodeId, member: &str) -> Holder {
    let Some(m) = st.find(o, member) else { return Holder::None };
    if m.mods.is_empty() {
        return Holder::None;
    }
    // T* x[n]: dimensions, then the pointer; T** x: a pointer (to pointers).
    let dims = m.mods.iter().any(|&x| x != POINTER);
    if dims && m.mods.last() == Some(&POINTER) {
        return Holder::Array;
    }
    if m.mods[0] == POINTER { Holder::One } else { Holder::None }
}

/// Sets the pointer that holds a child: the member, or element k of an array of them; in
/// a union, its bytes.
pub fn set_holder(st: &mut Store, o: NodeId, member: &str, value: u32, k: usize) -> Result<()> {
    let Some(m) = st.find(o, member) else { return Ok(()) };
    let s = st.st(o);
    if s.ty.kind() == Kind::Union {
        let at = m.offset as usize + 4 * k;
        if at + 4 <= s.size() {
            let big = s.big;
            if let Some(d) = st.data_mut(o).get_mut(at..at + 4) {
                store(d, 4, u64::from(value), big);
            }
        }
        return Ok(());
    }
    if holder_of(st, o, member) != Holder::None {
        st.set_ptr_at(o, member, value, k)?;
    }
    Ok(())
}
