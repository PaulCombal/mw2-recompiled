// Decal layers merged into the ground under them, as the 360 map compiler does.
//
// The PC draws a blend decal (a dirt patch, a rust streak) as its own see-through surface
// laid over the ground's triangles. The 360 tools give those ground triangles a composite
// material instead (stock mp_rust: 130 of them, "*42n_34n") whose shader set draws the
// ground and up to two decal layers in one pass, blended per vertex.
//
// What a composite is (worked out from stock mp_rust against PC mp_rust):
//  - shader set: the ground's without "wc_" and a trailing "p0", then one part per layer,
//    the decal's feature codes with its layer number (wc_l_sm_b0c0n0s0 -> _b1c1n1s1;
//    wc_unlit_multiply_lin -> _m1c1), then "p0" if the ground or a layer had it;
//  - pictures and constants: the ground's, then each layer's, renamed with its number
//    (colorMap1; name hash x = x * 33 ^ lower-case c);
//  - draw order, flags, culling: the ground's; render state: any stock material with the
//    same shader set; surface types: all of theirs;
//  - each vertex: its colour bytes 2 and 3 hold layer 1's and layer 2's weight (the decal
//    vertex's alpha); the world's layer data holds, per vertex, every layer's texture
//    coordinates, then for each normal-mapped layer 4 signed bytes: the layer's texture
//    directions in the ground's.

use crate::error::{Error, Result};
use crate::num::{load, load_f32, store, store_f32, truncate32};
use crate::ordered::{AssetKey, Ordered};
use crate::porter::{Porter, VERTEX_BYTES};
use crate::query::{asset_name, deref, deref_struct, embedded, target, without_commas};
use crate::text;
use crate::tree::{FOLLOWING, INSERT, NodeId, Store};
use crate::world::{clamp_f, cut_short, max_f, min_f, not_as_expected, pack16, struct_at};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_LAYERS: usize = 2;

/// A lit world shader set's name as a composite decal set's part: "wc_l_sm_r0c0n0s0p0" ->
/// "l_sm_r0c0n0s0" and its "p0", and a decal's set as layer i's part.
pub struct TechsetCode {
    pub code: String,
    pub p0: bool,
}

fn name_hash(s: &str) -> u32 {
    let mut x: u32 = 0;
    for c in s.to_ascii_lowercase().chars() {
        x = x.wrapping_mul(33) ^ u32::from(c as u32 as u8);
    }
    x
}

/// The texture names a layer's pictures can have, by name hash.
fn texture_by_hash(hash: u32) -> Option<&'static str> {
    ["colorMap", "normalMap", "specularMap", "detailMap"].into_iter().find(|n| name_hash(n) == hash)
}

/// Pairs of a lower-case letter and "0" from the start of `s`: how many there are.
fn zero_pairs(s: &[u8]) -> usize {
    s.chunks_exact(2).take_while(|p| p[0].is_ascii_lowercase() && p[1] == b'0').count()
}

/// After "wc_" and the code's fixed start: the fewest further letter-"0" pairs (at least
/// `least`) that leave just "p0" or nothing; the code's length after `rest`'s start, and
/// whether "p0" ended it.
fn shortest_code(rest: &[u8], least: usize) -> Option<(usize, bool)> {
    let pairs = zero_pairs(rest);
    (least..=pairs).find_map(|k| match &rest[2 * k..] {
        b"p0" => Some((2 * k, true)),
        b"" => Some((2 * k, false)),
        _ => None,
    })
}

pub fn ground_code(techset: &str) -> Option<TechsetCode> {
    // ^wc_(l_sm_(?:[a-z]0)+?)(p0)?$
    let rest = techset.strip_prefix("wc_l_sm_")?;
    let (len, p0) = shortest_code(rest.as_bytes(), 1)?;
    Some(TechsetCode { code: format!("l_sm_{}", &rest[..len]), p0 })
}

pub fn layer_code(techset: &str, layer: usize) -> Option<TechsetCode> {
    let n = layer.to_string();
    if techset == "wc_unlit_multiply_lin" {
        return Some(TechsetCode { code: format!("m{n}c{n}"), p0: false });
    }
    // ^wc_l_sm_([bt]0(?:[a-z]0)*?)(p0)?$
    let rest = techset.strip_prefix("wc_l_sm_")?.as_bytes();
    if rest.len() < 2 || !matches!(rest[0], b'b' | b't') || rest[1] != b'0' {
        return None;
    }
    let (len, p0) = shortest_code(&rest[2..], 0)?;
    let digit = n.as_bytes()[0];
    let code = rest[..2 + len].iter().map(|&c| char::from(if c == b'0' { digit } else { c })).collect();
    Some(TechsetCode { code, p0 })
}

/// The layer parts of a composite shader set's name: each "_" [bmt] [123] [a-z0-9]*,
/// shortest, before "_", "p0" or the end; without the "_".
fn layer_parts(name: &str) -> Vec<&str> {
    let s = name.as_bytes();
    let mut out = Vec::new();
    let mut p = 0;
    while p < s.len() {
        if s[p] == b'_' {
            let ok = s.get(p + 1).is_some_and(|c| matches!(c, b'b' | b'm' | b't'))
                && s.get(p + 2).is_some_and(|c| matches!(c, b'1' | b'2' | b'3'));
            if ok {
                let mut q = p + 3;
                let end = loop {
                    if q == s.len() || s[q] == b'_' || s[q..].starts_with(b"p0") {
                        break Some(q);
                    }
                    if s[q].is_ascii_lowercase() || s[q].is_ascii_digit() {
                        q += 1;
                    } else {
                        break None;
                    }
                };
                if let Some(q) = end {
                    out.push(&name[p + 1..q]);
                    p = q;
                    continue;
                }
            }
        }
        p += 1;
    }
    out
}

/// Whether a feature code holds an "n" (a normal map) after its first two characters.
fn has_normal(code: &str) -> bool {
    code.get(2..).is_some_and(|c| c.contains('n'))
}

/// Bytes of layer data a vertex has for a surface's material (0 for a plain one).
fn layer_bytes(st: &mut Store, s: NodeId) -> Result<usize> {
    let m = deref_struct(st, st.child(s, "material"))?;
    let ts = match m {
        Some(m) => deref_struct(st, st.child(m, "techniqueSet"))?,
        None => None,
    };
    let name = without_commas(&ts.and_then(|ts| asset_name(st, ts)).unwrap_or_default());
    let Some(m) = m else { return Ok(0) };
    if !asset_name(st, m).unwrap_or_default().starts_with('*') {
        return Ok(0);
    }
    let mut bytes = 0;
    for part in layer_parts(&name) {
        bytes += 8;
        if has_normal(part) {
            bytes += 4;
        }
    }
    Ok(bytes)
}

/// A position rounded to a tenth: its exact value rounded to one decimal (ties to even),
/// read back as the nearest double (-0.0 is 0.0). Through decimal text, which formatting and
/// parsing round exactly.
fn tenth(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let v = format!("{x:.1}").parse::<f64>().unwrap_or(x);
    if v == 0.0 { 0.0 } else { v }
}

/// The world's surfaces and buffers.
#[derive(Clone, Copy)]
struct Geometry {
    dpvs: NodeId,
    draw: NodeId,
    surfs: Option<NodeId>,
    v: Option<NodeId>,
    i: Option<NodeId>,
    ld: Option<NodeId>,
}

/// The world's surfaces and buffers, all found.
#[derive(Clone, Copy)]
struct Found {
    dpvs: NodeId,
    draw: NodeId,
    surfs: NodeId,
    v: NodeId,
    i: NodeId,
    ld: NodeId,
}

impl Geometry {
    fn found(&self) -> Option<Found> {
        Some(Found { dpvs: self.dpvs, draw: self.draw, surfs: self.surfs?, v: self.v?, i: self.i?, ld: self.ld? })
    }
}

fn geometry_of(st: &mut Store, world: NodeId) -> Result<Geometry> {
    let dpvs = st.sub(world, "dpvs")?;
    let draw = st.sub(world, "draw")?;
    let surfs = target(st, st.child(dpvs, "surfaces")).filter(|&n| st.is_array(Some(n)));
    let vd = st.sub(draw, "vd")?;
    let v = target(st, st.child(vd, "vertices")).filter(|&n| st.is_leaf(Some(n)));
    let i = target(st, st.child(draw, "indices")).filter(|&n| st.is_leaf(Some(n)));
    let vld = st.sub(draw, "vld")?;
    let ld = target(st, st.child(vld, "data")).filter(|&n| st.is_leaf(Some(n)));
    Ok(Geometry { dpvs, draw, surfs, v, i, ld })
}

fn indexes(st: &Store, i: NodeId) -> Vec<u32> {
    let lf = st.leaf(i);
    lf.raw.chunks_exact(2).map(|w| load(w, 2, lf.big) as u32).collect()
}

/// Whether the first `n` surfaces, and the indices, vertices and layer data they name, are
/// within the world's buffers: a broken file is left unmerged rather than read past its
/// end.
fn surfaces_fit(st: &mut Store, g: &Found, n: usize) -> Result<bool> {
    let items = st.array(g.surfs).items.clone();
    if n > items.len() {
        return Ok(false);
    }
    let idx = indexes(st, g.i);
    let verts = (st.leaf(g.v).raw.len() / VERTEX_BYTES) as i64;
    let layers = st.leaf(g.ld).raw.len() as i64;
    for &item in &items[..n] {
        let s = item.filter(|&s| st.is_struct(Some(s)));
        let t = match s {
            Some(s) if st.has(s, "tris") => st.sub(s, "tris")?,
            _ => return Ok(false),
        };
        let first = st.int(t, "firstVertex")?;
        let count = st.int(t, "vertexCount")?;
        let base = st.int(t, "baseIndex")?;
        let tris = st.int(t, "triCount")?;
        let layer = st.int(t, "vertexLayerData")?;
        if first < 0
            || count < 0
            || first.wrapping_add(count) > verts
            || base < 0
            || tris < 0
            || base.wrapping_add(tris.wrapping_mul(3)) > idx.len() as i64
            || layer < 0
            || layer > layers
        {
            return Ok(false);
        }
        for k in base..base + 3 * tris {
            if first + i64::from(idx[k as usize]) >= verts {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The key neighbouring surfaces are drawn as one by: material and lighting.
fn run_key(st: &mut Store, s: NodeId) -> Result<(Option<NodeId>, Vec<u8>)> {
    let laf = st.sub(s, "laf")?;
    let material = deref(st, st.child(s, "material"))?;
    Ok((material, st.data(laf).to_vec()))
}

/// Only materials written before the world (pointed at): a composite points at their
/// pictures, which must be in the file already.
fn mat(st: &mut Store, s: NodeId) -> Result<Option<NodeId>> {
    let c = st.child(s, "material");
    if st.is_ref(c) { deref_struct(st, c) } else { Ok(None) }
}

fn techset_name(st: &mut Store, m: NodeId) -> Result<String> {
    let ts = deref_struct(st, st.child(m, "techniqueSet"))?;
    Ok(without_commas(&ts.and_then(|ts| asset_name(st, ts)).unwrap_or_default()))
}

fn sort_key(st: &mut Store, m: NodeId) -> Result<i64> {
    let info = st.sub(m, "info")?;
    st.int(info, "sortKey")
}

fn textures(st: &Store, m: NodeId) -> Vec<NodeId> {
    let tt = target(st, st.child(m, "textureTable"));
    match st.as_array(tt) {
        Some(a) => a.items.iter().flatten().copied().filter(|&e| st.is_struct(Some(e))).collect(),
        None => Vec::new(),
    }
}

struct Constant {
    name: String,
    literal: Vec<u8>,
}

fn constants(st: &Store, m: NodeId) -> Result<(Vec<Constant>, Option<NodeId>)> {
    let ct = target(st, st.child(m, "constantTable")).filter(|&n| st.is_leaf(Some(n)));
    let Some(ct) = ct.filter(|&c| !st.leaf(c).raw.is_empty()) else { return Ok((Vec::new(), None)) };
    let lf = st.leaf(ct);
    let size = lf.t.map_or(0, |t| t.size() as usize);
    if size == 0 {
        return Err(not_as_expected());
    }
    let mut out = Vec::new();
    for i in 0..lf.raw.len() / size {
        let e = lf.raw.get(i * size..i * size + 32).ok_or_else(cut_short)?;
        let mut name = &e[4..16];
        while let [rest @ .., 0] = name {
            name = rest;
        }
        out.push(Constant { name: text::of(name), literal: e[16..32].to_vec() });
    }
    Ok((out, Some(ct)))
}

fn template_state(st: &Store, tpl: NodeId) -> Option<NodeId> {
    let mut state = st.child(tpl, "stateBitsTable");
    if let Some(r) = st.as_ref(state) {
        state = if r.rel == 0 { r.target } else { None };
    }
    state.filter(|&n| st.is_leaf(Some(n)))
}

/// A pointer to a listed asset through its list entry, as stock files point from one asset
/// at another.
fn pointer_to(st: &mut Store, entry: NodeId) -> NodeId {
    let r = st.make_ref(1);
    let rf = st.reference_mut(r);
    rf.target = Some(entry);
    rf.rel = 4;
    r
}

/// A pointer to the picture a texture entry points at (written before the world).
fn image_pointer(st: &mut Store, c: Option<NodeId>) -> Option<NodeId> {
    if let Some(r) = st.as_ref(c).cloned() {
        let copy = st.make_ref(r.val);
        let x = st.reference_mut(copy);
        x.target = r.target;
        x.rel = r.rel;
        x.t = r.t;
        return Some(copy);
    }
    if let Some(s) = c.filter(|&s| st.is_struct(Some(s))) {
        let slot = st.st(s).slot;
        let r = st.make_ref(1); // (written out as this until the target is placed: not null)
        st.reference_mut(r).target = Some(slot.unwrap_or(s));
        return Some(r);
    }
    c
}

/// A struct's copy with children of its own (a copy shares them).
fn own_copy(st: &mut Store, s: NodeId) -> Result<NodeId> {
    let made = st.copy(s)?;
    if let Some(ch) = st.st(s).ch() {
        let items = st.children(ch).items.clone();
        let own = st.make_children();
        st.children_mut(own).items = items;
        st.st_mut(made).set_ch(Some(own));
    }
    Ok(made)
}

/// Bounds-checked bytes of a buffer.
fn span(raw: &[u8], from: usize, to: usize) -> Result<&[u8]> {
    raw.get(from..to).ok_or_else(cut_short)
}

type Pos = [f64; 3];
type Tri = [u32; 3];
type Place = (usize, usize); // (surface, triangle)

/// The positions of the world's vertices, rounded to a tenth, each worked out once.
struct Positions<'r> {
    vraw: &'r [u8],
    big: bool,
    cache: HashMap<u32, Pos>,
}

impl Positions<'_> {
    fn pos(&mut self, v: u32) -> Result<Pos> {
        if let Some(&p) = self.cache.get(&v) {
            return Ok(p);
        }
        let mut p = [0.0; 3];
        for (k, x) in p.iter_mut().enumerate() {
            let o = VERTEX_BYTES * v as usize + 4 * k;
            *x = tenth(f64::from(load_f32(span(self.vraw, o, o + 4)?, self.big)));
        }
        self.cache.insert(v, p);
        Ok(p)
    }

    fn uv(&self, v: u32) -> Result<[f64; 2]> {
        let o = VERTEX_BYTES * v as usize;
        Ok([
            f64::from(load_f32(span(self.vraw, o + 20, o + 24)?, self.big)),
            f64::from(load_f32(span(self.vraw, o + 24, o + 28)?, self.big)),
        ])
    }

    /// A triangle by the set of its corners' positions.
    fn shape(&mut self, t: &Tri) -> Result<Vec<[u64; 3]>> {
        let mut s = vec![self.pos(t[0])?, self.pos(t[1])?, self.pos(t[2])?];
        s.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])).then(a[2].total_cmp(&b[2])));
        s.dedup_by(|a, b| a == b);
        Ok(s.into_iter().map(|p| p.map(f64::to_bits)).collect())
    }

    /// The corner of a decal triangle on a ground vertex.
    fn decal_vertex(&mut self, dt: &Tri, gv: u32) -> Result<u32> {
        for &dv in dt {
            if self.pos(dv)? == self.pos(gv)? {
                return Ok(dv);
            }
        }
        Err(Error::port("decal merge: a decal corner not on the ground's"))
    }
}

/// A surface's triangles, as vertex numbers in the world's buffer.
fn tris(st: &mut Store, idx: &[u32], s: NodeId) -> Result<Vec<Tri>> {
    let t = st.sub(s, "tris")?;
    let fv = st.int(t, "firstVertex")? as u32;
    let b = st.int(t, "baseIndex")? as usize;
    let mut out = Vec::new();
    for k in 0..st.int(t, "triCount")?.max(0) as usize {
        let at = |q: usize| idx.get(b + 3 * k + q).map(|&i| fv.wrapping_add(i)).ok_or_else(cut_short);
        out.push([at(0)?, at(1)?, at(2)?]);
    }
    Ok(out)
}

/// The decal surfaces merged into a ground triangle, by sort key (and place).
fn layers_of(
    st: &mut Store,
    cover: &Ordered<Place, Ordered<usize, Tri>>,
    blocked: &BTreeSet<usize>,
    mats: &[Option<NodeId>],
    p: &Place,
) -> Result<Vec<usize>> {
    let mut js = Vec::new();
    if let Some(c) = cover.find(p) {
        for (&j, _) in c.iter() {
            if !blocked.contains(&j) {
                let m = mats[j].ok_or_else(not_as_expected)?;
                js.push((sort_key(st, m)?, j));
            }
        }
    }
    js.sort();
    Ok(js.into_iter().map(|(_, j)| j).collect())
}

fn materials_of(mats: &[Option<NodeId>], js: &[usize]) -> Result<Vec<NodeId>> {
    js.iter().map(|&j| mats[j].ok_or_else(not_as_expected)).collect()
}

#[derive(Clone)]
struct Plan {
    techset: String,
    tpl: NodeId,
}

/// The ground material, then the layers'.
type Key = Vec<NodeId>;

struct DecalMerger {
    world: NodeId,
    plans: HashMap<Key, Option<Plan>>,
    composites: HashMap<Key, NodeId>,
    techsets: HashMap<String, NodeId>,
    serial: i32,
}

impl DecalMerger {
    /// The shader set and stock template for a ground material with these layers, or
    /// nothing when the 360 has no such composite.
    fn plan_for(&mut self, p: &mut Porter, ground: NodeId, layers: &[NodeId]) -> Result<Option<Plan>> {
        let mut key = vec![ground];
        key.extend_from_slice(layers);
        if let Some(plan) = self.plans.get(&key) {
            return Ok(plan.clone());
        }
        let mut res = None;
        let ground_name = techset_name(p.st, ground)?;
        if let Some(g) = ground_code(&ground_name)
            && !layers.is_empty()
            && layers.len() <= MAX_LAYERS
        {
            let mut name = g.code;
            let mut p0 = g.p0;
            let mut ok = true;
            for i in 1..=layers.len() {
                let lc = layer_code(&techset_name(p.st, layers[i - 1])?, i);
                // Every picture and constant needs a name the composite can number.
                let mut named = lc.is_some();
                for e in textures(p.st, layers[i - 1]) {
                    named &= texture_by_hash(p.st.u(e, "nameHash")? as u32).is_some();
                }
                for c in constants(p.st, layers[i - 1])?.0 {
                    named &= c.name.chars().count() < 12;
                }
                let Some(lc) = lc.filter(|_| named) else {
                    ok = false;
                    break;
                };
                name += "_";
                name += &lc.code;
                p0 = p0 || lc.p0;
            }
            if ok {
                if p0 {
                    name += "p0";
                }
                let tpl = if p.have_techset(&name)? { p.material_templates.find(&name).copied() } else { None };
                if let Some(tpl) = tpl
                    && template_state(p.st, tpl).is_some()
                {
                    res = Some(Plan { techset: name, tpl });
                }
            }
        }
        self.plans.insert(key, res.clone());
        Ok(res)
    }

    /// An asset as an entry of its own in the file's asset list (before the world): the
    /// world's surfaces are written before what their pointers lead to, so pointing at an
    /// asset written there, later ones would point ahead.
    fn listed(p: &mut Porter, ty: &str, asset: NodeId) -> NodeId {
        let e = p.st.make_entry(ty, Some(asset));
        p.extra_assets.push(e);
        e
    }

    fn entry_copy(p: &mut Porter, e: NodeId, number: usize) -> Result<NodeId> {
        let made = own_copy(p.st, e)?;
        let u = p.st.sub(made, "u")?;
        let uch = p.st.make_children();
        let theirs = embedded(p.st, e, "u").ok_or_else(not_as_expected)?;
        if let Some(ch) = p.st.st(theirs).ch() {
            for (k, v) in p.st.children(ch).items.clone() {
                let v = image_pointer(p.st, v);
                p.st.children_mut(uch).items.push((k, v));
            }
        }
        p.st.st_mut(u).set_ch(Some(uch));
        let big = p.st.st(u).big;
        let held = load(span(p.st.data(u), 0, 4)?, 4, big) as u32;
        if held == FOLLOWING || held == INSERT {
            store(p.st.data_mut(u), 4, 1, big);
        }
        if number != 0 {
            let digit = number.to_string();
            let texture = texture_by_hash(p.st.u(e, "nameHash")? as u32)
                .ok_or_else(|| Error::port("a decal layer's picture has no name the composite can number"))?;
            p.st.set(made, "nameHash", i64::from(name_hash(&format!("{texture}{digit}"))))?;
            p.st.set(made, "nameEnd", i64::from(digit.as_bytes()[0]))?;
            // As the stock composites have them (the ground's): specular 1, others 2.
            let ss = p.st.sub(made, "samplerState")?;
            if p.st.int(ss, "mipMap")? != 0 {
                let mip = if p.st.int(e, "semantic")? == 8 { 1 } else { 2 };
                p.st.set(ss, "mipMap", mip)?;
            }
        }
        Ok(made)
    }

    fn techset_ref(&mut self, p: &mut Porter, name: &str) -> Result<NodeId> {
        let entry = match self.techsets.get(name) {
            Some(&e) => e,
            None => {
                let key = AssetKey::new("MaterialTechniqueSet", name);
                let d = if p.resident.has(&key) || p.named.contains(&(key.ty.clone(), name.to_string())) {
                    p.reference(&key.ty, name)?
                } else {
                    let src =
                        *p.library.find(&key).ok_or_else(|| Error::port(format!("no stock shader set {name}")))?;
                    p.stock_copy(src)?
                };
                let info = *p.x.infos.get(&key.ty).ok_or_else(|| Error::new("the 360's rules have no shader sets"))?;
                let s = p.st.st_mut(d);
                s.asset = &p.x.structs[info].name;
                s.slot = None;
                let e = DecalMerger::listed(p, "techset", d);
                self.techsets.insert(name.to_string(), e);
                e
            }
        };
        Ok(pointer_to(p.st, entry))
    }

    fn composite(&mut self, p: &mut Porter, ground: NodeId, layers: &[NodeId]) -> Result<NodeId> {
        let mut key = vec![ground];
        key.extend_from_slice(layers);
        if let Some(&c) = self.composites.get(&key) {
            return Ok(c);
        }
        let plan = self.plan_for(p, ground, layers)?.ok_or_else(not_as_expected)?;
        let mut table = Vec::new();
        for e in textures(p.st, ground) {
            table.push(DecalMerger::entry_copy(p, e, 0)?);
        }
        for (i, &layer) in layers.iter().enumerate() {
            for e in textures(p.st, layer) {
                table.push(DecalMerger::entry_copy(p, e, i + 1)?);
            }
        }
        let mut keyed = Vec::new();
        for e in table {
            keyed.push((p.st.u(e, "nameHash")?, e));
        }
        keyed.sort_by_key(|k| k.0);
        let table: Vec<Option<NodeId>> = keyed.into_iter().map(|(_, e)| Some(e)).collect();
        let (consts, mut ct) = constants(p.st, ground)?;
        // (In name hash order, as the game looks them up.)
        let mut entries: Vec<(u32, String, Vec<u8>)> = Vec::new();
        for c in consts {
            entries.push((name_hash(&c.name), c.name, c.literal));
        }
        for (i, &layer) in layers.iter().enumerate() {
            let (lc, lct) = constants(p.st, layer)?;
            if ct.is_none() {
                ct = lct;
            }
            for c in lc {
                let n = format!("{}{}", c.name, i + 1);
                entries.push((name_hash(&n), n, c.literal));
            }
        }
        entries.sort();
        let mut constant_table = None;
        if !entries.is_empty() {
            let ct = ct.ok_or_else(not_as_expected)?;
            let (t, big) = {
                let lf = p.st.leaf(ct);
                (lf.t, lf.big)
            };
            let mut raw = Vec::new();
            for (h, n, lit) in &entries {
                let mut b = [0u8; 4];
                store(&mut b, 4, u64::from(*h), big);
                raw.extend_from_slice(&b);
                let mut padded = text::bytes(n);
                padded.resize(12, 0);
                raw.extend_from_slice(&padded);
                raw.extend_from_slice(lit);
            }
            constant_table = Some(p.st.make_leaf(t, entries.len() as i64, raw, big));
        }
        let mut surface_types: u64 = 0;
        let info = p.st.sub(ground, "info")?;
        surface_types |= p.st.u(info, "surfaceTypeBits")?;
        for &m in layers {
            let info = p.st.sub(m, "info")?;
            surface_types |= p.st.u(info, "surfaceTypeBits")?;
        }
        self.serial += 1;
        let name = format!("*dl{}", self.serial);
        let d = p.st.copy(ground)?;
        p.st.st_mut(d).slot = None;
        p.st.st_mut(d).forward = false;
        let info = p.st.sub(d, "info")?;
        let ich = p.st.make_children();
        let s = p.st.make_str(name);
        p.st.children_mut(ich).set("name", Some(s), &[]);
        p.st.st_mut(info).set_ch(Some(ich));
        p.st.set_ptr(info, "name", FOLLOWING)?;
        p.st.set(info, "surfaceTypeBits", surface_types as i64)?;
        for k in ["stateBitsEntry", "stateBitsCount"] {
            let from = p.st.bytes(plan.tpl, k)?.to_vec();
            p.st.bytes_mut(d, k)?.get_mut(..from.len()).ok_or_else(not_as_expected)?.copy_from_slice(&from);
        }
        p.st.set(d, "textureCount", table.len() as i64)?;
        p.st.set(d, "constantCount", entries.len() as i64)?;
        p.st.set_ptr(d, "techniqueSet", 1)?;
        p.st.set_ptr(d, "textureTable", FOLLOWING)?;
        p.st.set_ptr(d, "constantTable", if constant_table.is_some() { FOLLOWING } else { 0 })?;
        p.st.set_ptr(d, "stateBitsTable", FOLLOWING)?;
        let ch = p.st.make_children();
        let techset = self.techset_ref(p, &plan.techset)?;
        p.st.children_mut(ch).set("techniqueSet", Some(techset), &[]);
        let tt = p.st.make_array(table);
        p.st.children_mut(ch).set("textureTable", Some(tt), &[]);
        let state = template_state(p.st, plan.tpl);
        p.st.children_mut(ch).set("stateBitsTable", state, &[]);
        if let Some(c) = constant_table {
            p.st.children_mut(ch).set("constantTable", Some(c), &[]);
        }
        p.st.st_mut(d).set_ch(Some(ch));
        let info = *p.x.infos.get("Material").ok_or_else(|| Error::new("the 360's rules have no materials"))?;
        p.st.st_mut(d).asset = &p.x.structs[info].name;
        let e = DecalMerger::listed(p, "material", d);
        self.composites.insert(key, e);
        Ok(e)
    }

    fn run(&mut self, p: &mut Porter) -> Result<()> {
        let Some(g) = geometry_of(p.st, self.world)?.found() else {
            return p.warn("decal layers couldn't be merged (world data not found)");
        };
        if p.st.int(g.draw, "vertexLayerDataSize")? > 4 {
            return Ok(()); // already has layer data (a 360 file)
        }
        let n = p.st.int(g.dpvs, "staticSurfaceCount")? as usize;
        if !surfaces_fit(p.st, &g, n)? {
            return p.warn("decal layers couldn't be merged (the world's surfaces point past its data)");
        }
        let vraw = p.st.leaf(g.v).raw.clone();
        let big = p.st.leaf(g.v).big;
        let mut idx = indexes(p.st, g.i);
        // (surfaces_fit found the first n surfaces structs.)
        let surfs: Vec<NodeId> = p.st.array(g.surfs).items[..n].iter().map(|s| s.unwrap_or(NodeId(0))).collect();
        let mut at = Positions { vraw: &vraw, big, cache: HashMap::new() };
        let mut mats: Vec<Option<NodeId>> = Vec::with_capacity(n);
        for &s in &surfs {
            mats.push(mat(p.st, s)?);
        }
        let mut ground_tris: HashMap<Vec<[u64; 3]>, Place> = HashMap::new();
        let mut gtris: BTreeMap<usize, Vec<Tri>> = BTreeMap::new();
        for i in 0..n {
            let Some(m) = mats[i] else { continue };
            if sort_key(p.st, m)? >= 6 || ground_code(&techset_name(p.st, m)?).is_none() {
                continue;
            }
            let ts = tris(p.st, &idx, surfs[i])?;
            for (k, t) in ts.iter().enumerate() {
                ground_tris.entry(at.shape(t)?).or_insert((i, k));
            }
            gtris.insert(i, ts);
        }
        // (ground surface, triangle) -> {decal surface: its triangle}, both in the order
        // first met.
        let mut cover: Ordered<Place, Ordered<usize, Tri>> = Ordered::new();
        let mut decals = BTreeSet::new();
        let mut blocked = BTreeSet::new();
        for i in 0..n {
            let Some(m) = mats[i] else { continue };
            if sort_key(p.st, m)? < 6 || layer_code(&techset_name(p.st, m)?, 1).is_none() {
                continue;
            }
            decals.insert(i);
            for t in tris(p.st, &idx, surfs[i])? {
                let found = ground_tris.get(&at.shape(&t)?).copied();
                let Some(found) = found.filter(|&f| !cover.find_or_add(f, Ordered::new()).has(&i)) else {
                    blocked.insert(i);
                    break;
                };
                cover.find_or_add(found, Ordered::new()).set(i, t);
            }
        }
        let places: Vec<Place> = cover.iter().map(|(&k, _)| k).collect();
        loop {
            let mut fresh = BTreeSet::new();
            for pl in &places {
                let js = layers_of(p.st, &cover, &blocked, &mats, pl)?;
                if js.is_empty() {
                    continue;
                }
                let ground = mats[pl.0].ok_or_else(not_as_expected)?;
                if self.plan_for(p, ground, &materials_of(&mats, &js)?)?.is_none() {
                    fresh.extend(js);
                }
            }
            let mut grew = false;
            for j in fresh {
                grew |= blocked.insert(j);
            }
            if !grew {
                break;
            }
        }
        let merged: BTreeSet<usize> = decals.iter().copied().filter(|j| !blocked.contains(j)).collect();
        if merged.is_empty() {
            return p.say("  decal layers: none could be merged");
        }

        // New surfaces for every ground surface a merged decal lies on.
        let mut vout = vraw.clone();
        let mut lout: Vec<u8> = Vec::new();
        let mut nverts = vraw.len() / VERTEX_BYTES;
        let mut split: BTreeMap<usize, Vec<Option<NodeId>>> = BTreeMap::new();
        let mut grounds = BTreeSet::new();
        for pl in &places {
            if !layers_of(p.st, &cover, &blocked, &mats, pl)?.is_empty() {
                grounds.insert(pl.0);
            }
        }
        for gi in grounds {
            let s = surfs[gi];
            let gt_list = gtris.get(&gi).cloned().unwrap_or_default();
            let mut groups: Vec<(Vec<usize>, Vec<usize>)> = Vec::new(); // layer surfaces -> triangles
            for k in 0..gt_list.len() {
                let js = layers_of(p.st, &cover, &blocked, &mats, &(gi, k))?;
                match groups.iter_mut().find(|e| e.0 == js) {
                    Some(e) => e.1.push(k),
                    None => groups.push((js, vec![k])),
                }
            }
            let mut out = Vec::new();
            for (js, ks) in &groups {
                let ns = own_copy(p.st, s)?;
                let t = p.st.sub(ns, "tris")?;
                p.st.set(t, "baseIndex", idx.len() as i64)?;
                p.st.set(t, "triCount", ks.len() as i64)?;
                if js.is_empty() {
                    let fv = p.st.int(t, "firstVertex")? as u32;
                    for &k in ks {
                        for v in gt_list[k] {
                            idx.push(v.wrapping_sub(fv));
                        }
                    }
                    out.push(Some(ns));
                    continue;
                }
                let lms = materials_of(&mats, js)?;
                let mut normal = Vec::new();
                for (i, &lm) in lms.iter().enumerate() {
                    let code = layer_code(&techset_name(p.st, lm)?, i + 1).ok_or_else(not_as_expected)?;
                    if has_normal(&code.code) {
                        normal.push(i);
                    }
                }
                // (ground vertex, decal vertices) -> number, and the keys by number
                let mut verts: BTreeMap<(u32, Vec<u32>), usize> = BTreeMap::new();
                let mut by_number: Vec<(u32, Vec<u32>)> = Vec::new();
                let mut rot: BTreeMap<usize, Vec<[f64; 4]>> = BTreeMap::new();
                let mut tri_new: Vec<[usize; 3]> = Vec::new();
                for &k in ks {
                    let gt = gt_list[k];
                    let covered = cover.find(&(gi, k)).ok_or_else(not_as_expected)?;
                    let mut dts: Vec<Tri> = Vec::new();
                    for j in js {
                        dts.push(*covered.find(j).ok_or_else(not_as_expected)?);
                    }
                    let mut corner = [0usize; 3];
                    for c in 0..3 {
                        let mut dvs = Vec::new();
                        for dt in &dts {
                            dvs.push(at.decal_vertex(dt, gt[c])?);
                        }
                        let key = (gt[c], dvs);
                        corner[c] = match verts.get(&key) {
                            Some(&num) => num,
                            None => {
                                let num = verts.len();
                                verts.insert(key.clone(), num);
                                by_number.push(key);
                                num
                            }
                        };
                    }
                    tri_new.push(corner);
                    if normal.is_empty() {
                        continue;
                    }
                    let (g0, g1, g2) = (at.uv(gt[0])?, at.uv(gt[1])?, at.uv(gt[2])?);
                    let d1 = [g1[0] - g0[0], g1[1] - g0[1]];
                    let d2 = [g2[0] - g0[0], g2[1] - g0[1]];
                    let det = d1[0] * d2[1] - d2[0] * d1[1];
                    for (q, &nq) in normal.iter().enumerate() {
                        let dt = dts[nq];
                        let c0 = at.decal_vertex(&dt, gt[0])?;
                        let l0 = at.uv(c0)?;
                        let c1 = at.decal_vertex(&dt, gt[1])?;
                        let l1 = at.uv(c1)?;
                        let c2 = at.decal_vertex(&dt, gt[2])?;
                        let l2 = at.uv(c2)?;
                        let e1 = [l1[0] - l0[0], l1[1] - l0[1]];
                        let e2 = [l2[0] - l0[0], l2[1] - l0[1]];
                        if det.abs() < 1e-12 {
                            continue;
                        }
                        // A = dL * inverse(dB), dB's columns the ground's uv edges.
                        let a00 = (e1[0] * d2[1] - e2[0] * d1[1]) / det;
                        let a01 = (e2[0] * d1[0] - e1[0] * d2[0]) / det;
                        let a10 = (e1[1] * d2[1] - e2[1] * d1[1]) / det;
                        let a11 = (e2[1] * d1[0] - e1[1] * d2[0]) / det;
                        for &c in &corner {
                            let acc = &mut rot.entry(c).or_insert_with(|| vec![[0.0; 4]; normal.len()])[q];
                            acc[0] += a00;
                            acc[1] += a01;
                            acc[2] += a10;
                            acc[3] += a11;
                        }
                    }
                }
                if verts.len() > 0xFFFF {
                    return Err(Error::port("decal merge: vertex block too big"));
                }
                let fv = nverts;
                let layer_at = lout.len();
                for (gv, dvs) in &by_number {
                    let o = VERTEX_BYTES * *gv as usize;
                    let mut rec = span(&vraw, o, o + VERTEX_BYTES)?.to_vec();
                    for (li, &dv) in dvs.iter().enumerate() {
                        let alpha = VERTEX_BYTES * dv as usize + 16;
                        rec[16 + 2 + li] = *vraw.get(alpha).ok_or_else(cut_short)?; // the decal's alpha
                    }
                    vout.extend_from_slice(&rec);
                }
                for (num, (_, dvs)) in by_number.iter().enumerate() {
                    for &dv in dvs {
                        let o = VERTEX_BYTES * dv as usize;
                        lout.extend_from_slice(span(&vraw, o + 20, o + 28)?);
                    }
                    for q in 0..normal.len() {
                        let mut a = rot.get(&num).map_or([1.0, 0.0, 0.0, 1.0], |r| r[q]);
                        let mut r0 = (a[0] * a[0] + a[1] * a[1]).sqrt();
                        let mut r1 = (a[2] * a[2] + a[3] * a[3]).sqrt();
                        if r0 == 0.0 {
                            r0 = 1.0;
                        }
                        if r1 == 0.0 {
                            r1 = 1.0;
                        }
                        if a[0] == 0.0 && a[1] == 0.0 && a[2] == 0.0 && a[3] == 0.0 {
                            a = [1.0, 0.0, 0.0, 1.0];
                            r0 = 1.0;
                            r1 = 1.0;
                        }
                        for x in [a[3] / r1, a[0] / r0, a[2] / r1, a[1] / r0] {
                            let v = truncate32((clamp_f(x, -1.0, 1.0) * 127.0).round_ties_even()).wrapping_add(127);
                            lout.push(0.max(254.min(v)) as u8);
                        }
                    }
                }
                nverts += verts.len();
                for c in &tri_new {
                    for &v in c {
                        idx.push(v as u32);
                    }
                }
                p.st.set(t, "firstVertex", fv as i64)?;
                p.st.set(t, "vertexCount", verts.len() as i64)?;
                p.st.set(t, "vertexLayerData", layer_at as i64)?;
                p.st.set_ptr(ns, "material", 1)?;
                let ground = mats[gi].ok_or_else(not_as_expected)?;
                let composite = self.composite(p, ground, &lms)?;
                let pointer = pointer_to(p.st, composite);
                p.st.set_child(ns, "material", Some(pointer));
                out.push(Some(ns));
            }
            split.insert(gi, out);
        }
        {
            let v = p.st.leaf_mut(g.v);
            v.raw = vout;
            v.n = nverts as i64;
        }
        {
            let i = p.st.leaf_mut(g.i);
            i.raw = pack16(&idx, i.big);
            i.n = idx.len() as i64;
        }
        self.rebuild(p, &g, n, &split, &merged)?;
        let ld = p.st.leaf_mut(g.ld);
        if lout.is_empty() {
            lout = ld.raw.clone();
        }
        ld.raw = lout;
        ld.n = ld.raw.len() as i64;
        let ld_n = ld.n;
        p.st.set(g.draw, "vertexCount", nverts as i64)?;
        p.st.set(g.draw, "indexCount", idx.len() as i64)?;
        p.st.set(g.draw, "vertexLayerDataSize", ld_n)?;
        p.say(&format!(
            "  decal layers: {} decal surfaces merged into the ground under them ({} composite materials, {} ground \
             surfaces split; {} decals left as they were)",
            merged.len(),
            self.composites.len(),
            split.len(),
            decals.len() - merged.len()
        ))
    }

    /// Every surface table and surface number after the surfaces changed: merged decals go,
    /// split ground surfaces become their parts.
    fn rebuild(
        &mut self,
        p: &mut Porter,
        g: &Found,
        n: usize,
        split: &BTreeMap<usize, Vec<Option<NodeId>>>,
        merged: &BTreeSet<usize>,
    ) -> Result<()> {
        let st = &mut *p.st;
        let world = self.world;
        let items = st.array(g.surfs).items.clone();
        let mut fresh: Vec<Option<NodeId>> = Vec::new();
        let mut expand: Vec<Vec<u32>> = Vec::new();
        for (i, &item) in items.iter().enumerate().take(n) {
            let parts: Vec<Option<NodeId>> = if merged.contains(&i) {
                Vec::new()
            } else if let Some(parts) = split.get(&i) {
                parts.clone()
            } else {
                vec![item]
            };
            expand.push((0..parts.len()).map(|k| (fresh.len() + k) as u32).collect());
            fresh.extend(parts);
        }
        let n2 = fresh.len();
        let mut src: Vec<usize> = Vec::new(); // new surface -> the old one it came from
        for (i, e) in expand.iter().enumerate() {
            src.extend(std::iter::repeat_n(i, e.len()));
        }
        st.array_mut(g.surfs).items.splice(..n, fresh.iter().copied());
        let dpvs = g.dpvs;
        // Bounds: kept, or worked out for the parts (middle, half size).
        if let Some(bl) = target(st, st.child(dpvs, "surfacesBounds")).filter(|&n| st.is_leaf(Some(n))) {
            let (size, braw, bbig, bn) = {
                let lf = st.leaf(bl);
                (lf.t.map(|t| t.size() as usize).ok_or_else(not_as_expected)?, lf.raw.clone(), lf.big, lf.n)
            };
            let mut out = Vec::new();
            for (j, &i) in src.iter().enumerate() {
                let mut old = span(&braw, i * size, (i + 1) * size)?.to_vec();
                if split.contains_key(&i) {
                    let part = struct_at(st, fresh[j])?;
                    let t = st.sub(part, "tris")?;
                    let mut lo = [1e30f64; 3];
                    let mut hi = [-1e30f64; 3];
                    let base = st.int(t, "baseIndex")? as usize;
                    let fv = st.int(t, "firstVertex")? as usize;
                    let tri_count = st.int(t, "triCount")?;
                    let (il, vl) = (st.leaf(g.i), st.leaf(g.v));
                    let mut used = BTreeSet::new();
                    for k in 0..(3 * tri_count).max(0) as usize {
                        let o = 2 * (base + k);
                        used.insert(load(span(&il.raw, o, o + 2)?, 2, il.big) as u32);
                    }
                    for v in used {
                        for q in 0..3 {
                            let o = VERTEX_BYTES * (fv + v as usize) + 4 * q;
                            let x = f64::from(load_f32(span(&vl.raw, o, o + 4)?, vl.big));
                            lo[q] = min_f(lo[q], x);
                            hi[q] = max_f(hi[q], x);
                        }
                    }
                    old.fill(0);
                    for q in 0..3 {
                        store_f32(old.get_mut(4 * q..).ok_or_else(cut_short)?, ((lo[q] + hi[q]) / 2.0) as f32, bbig);
                        store_f32(
                            old.get_mut(12 + 4 * q..).ok_or_else(cut_short)?,
                            ((hi[q] - lo[q]) / 2.0) as f32,
                            bbig,
                        );
                    }
                }
                out.extend_from_slice(&old);
            }
            out.extend_from_slice(braw.get(n * size..).ok_or_else(cut_short)?);
            let lf = st.leaf_mut(bl);
            lf.n = n2 as i64 + 0.max(bn - n as i64);
            lf.raw = out;
        }
        if let Some(sm) = target(st, st.child(dpvs, "surfaceMaterials")).filter(|&n| st.is_leaf(Some(n))) {
            let lf = st.leaf_mut(sm);
            let size = lf.t.map(|t| t.size() as usize).ok_or_else(not_as_expected)?;
            let mut raw = vec![0u8; size * n2];
            raw.extend_from_slice(&lf.raw[lf.raw.len().min(n * size)..]);
            lf.n = n2 as i64 + 0.max(lf.n - n as i64);
            lf.raw = raw;
        }
        let words = n2.div_ceil(32).div_ceil(4) * 4;
        if let Some(ch) = st.st(dpvs).ch() {
            for (k, c) in st.children(ch).items.clone() {
                if k.member == "surfaceVisData"
                    && let Some(lf) = target(st, c).filter(|&n| st.is_leaf(Some(n)))
                {
                    let lf = st.leaf_mut(lf);
                    lf.raw = vec![0; n2];
                    lf.n = n2 as i64;
                }
            }
        }
        st.set(dpvs, "surfaceVisDataCount", words as i64)?;
        if let Some(bits) = target(st, st.child(dpvs, "surfaceCastsSunShadow")).filter(|&n| st.is_leaf(Some(n)))
            && !st.leaf(bits).raw.is_empty()
        {
            let lf = st.leaf_mut(bits);
            let count = lf.raw.len() / 4;
            let mut set = vec![0u32; words];
            for (j, &i) in src.iter().enumerate() {
                if (i >> 5) < count && (load(&lf.raw[4 * (i >> 5)..], 4, lf.big) >> (i & 31)) & 1 != 0 {
                    set[j >> 5] |= 1u32 << (j & 31);
                }
            }
            let unit = if lf.n != 0 { lf.raw.len() / lf.n as usize } else { 4 };
            if unit == 0 {
                return Err(not_as_expected());
            }
            lf.raw = vec![0; 4 * words];
            for (w, &word) in set.iter().enumerate() {
                store(&mut lf.raw[4 * w..], 4, u64::from(word), lf.big);
            }
            lf.n = (lf.raw.len() / unit) as i64;
        }
        let renumbered = |i: u32| -> Vec<u32> {
            if (i as usize) < n { expand[i as usize].clone() } else { vec![(i as usize - n).wrapping_add(n2) as u32] }
        };
        // The sorted list (culling tree nodes hold ranges of it) and the lists holding
        // numbers.
        if let Some(so) = target(st, st.child(dpvs, "sortedSurfIndex")).filter(|&n| st.is_leaf(Some(n))) {
            let lf = st.leaf_mut(so);
            let old = (0..lf.n.max(0) as usize)
                .map(|i| Ok(load(span(&lf.raw, 2 * i, 2 * i + 2)?, 2, lf.big) as u32))
                .collect::<Result<Vec<u32>>>()?;
            let mut sorted = Vec::new();
            let mut at = vec![0usize];
            for &i in &old {
                sorted.extend(renumbered(i));
                at.push(sorted.len());
            }
            lf.raw = pack16(&sorted, lf.big);
            lf.n = sorted.len() as i64;
            if let Some(trees) = target(st, st.child(world, "aabbTrees")).filter(|&n| st.is_array(Some(n))) {
                for ctn in st.array(trees).items.clone() {
                    let ct = struct_at(st, ctn)?;
                    let Some(nodes) = target(st, st.child(ct, "aabbTree")).filter(|&n| st.is_array(Some(n))) else {
                        continue;
                    };
                    for ndn in st.array(nodes).items.clone() {
                        let nd = struct_at(st, ndn)?;
                        let a = st.int(nd, "startSurfIndex")? as usize;
                        let c = st.int(nd, "surfaceCount")? as usize;
                        let Some(end) = a.checked_add(c).filter(|&e| e <= old.len()) else { continue };
                        st.set(nd, "startSurfIndex", at[a] as i64)?;
                        st.set(nd, "surfaceCount", (at[end] - at[a]) as i64)?;
                    }
                }
            }
        }
        if let Some(geoms) = target(st, st.child(world, "shadowGeom")).filter(|&n| st.is_array(Some(n))) {
            for gn in st.array(geoms).items.clone() {
                let Some(gm) = gn.filter(|&g| st.is_struct(Some(g))) else { continue };
                let Some(lf) = target(st, st.child(gm, "sortedSurfIndex")).filter(|&n| st.is_leaf(Some(n))) else {
                    continue;
                };
                let count = st.int(gm, "surfaceCount")?;
                if count == 0 {
                    continue;
                }
                let leaf = st.leaf_mut(lf);
                let mut list = Vec::new();
                for i in 0..count.max(0) as usize {
                    list.extend(renumbered(load(span(&leaf.raw, 2 * i, 2 * i + 2)?, 2, leaf.big) as u32));
                }
                leaf.raw = pack16(&list, leaf.big);
                leaf.n = list.len() as i64;
                st.set(gm, "surfaceCount", list.len() as i64)?;
            }
        }
        if let Some(models) = target(st, st.child(world, "models")).filter(|&n| st.is_leaf(Some(n))) {
            let lf = st.leaf_mut(models);
            let t = lf.t.ok_or_else(not_as_expected)?;
            let size = t.size() as usize;
            let mut off = 0usize;
            for m in t.members() {
                if m.name == "surfaceCount" {
                    off = m.offset as usize;
                }
            }
            for k in 0..lf.n.max(0) as usize {
                let at = k * size + off;
                let big = lf.big;
                let p = lf.raw.get_mut(at..at + 4).ok_or_else(cut_short)?;
                let mut c = load(p, 2, big) as u32;
                let mut a = load(&p[2..], 2, big) as u32;
                if a == 0 && c as usize == n {
                    c = n2 as u32;
                } else if a != 0xFFFF && c != 0 && (a as usize) < n && !expand[a as usize].is_empty() {
                    a = expand[a as usize][0];
                }
                store(p, 2, u64::from(c), big);
                store(&mut p[2..], 2, u64::from(a), big);
            }
        }
        if let Some(skies) = target(st, st.child(world, "skies")).filter(|&n| st.is_array(Some(n))) {
            for sn in st.array(skies).items.clone() {
                let Some(sky) = sn.filter(|&s| st.is_struct(Some(s))) else { continue };
                let Some(lf) = target(st, st.child(sky, "skyStartSurfs")).filter(|&n| st.is_leaf(Some(n))) else {
                    continue;
                };
                let lf = st.leaf_mut(lf);
                let big = lf.big;
                for i in 0..lf.n.max(0) as usize {
                    let p = lf.raw.get_mut(4 * i..4 * i + 4).ok_or_else(cut_short)?;
                    let x = load(p, 4, big) as u32 as i32;
                    if x >= 0 && (x as usize) < n && !expand[x as usize].is_empty() {
                        store(p, 4, u64::from(expand[x as usize][0]), big);
                    }
                }
            }
        }
        st.set(dpvs, "staticSurfaceCount", n2 as i64)?;
        let surface_count = st.int(world, "surfaceCount")?;
        let count = if surface_count != 0 { surface_count } else { n as i64 };
        st.set(world, "surfaceCount", count - n as i64 + n2 as i64)
    }
}

impl Porter<'_> {
    pub(crate) fn merge_decals(&mut self, world: NodeId) -> Result<()> {
        DecalMerger { world, plans: HashMap::new(), composites: HashMap::new(), techsets: HashMap::new(), serial: 0 }
            .run(self)
    }

    /// The 360 draws a run of neighbouring surfaces with the same material (and lighting)
    /// as one draw: from the first one's first index to the last one's last, in the first
    /// one's vertex block. Stock files lay the index buffer out in surface order and keep
    /// such runs in one block; after merging decals that no longer held (a GPU hang). So
    /// runs in more than one block get one block together, then the whole index buffer is
    /// written again in surface order.
    pub(crate) fn relay_surfaces(&mut self, world: NodeId) -> Result<()> {
        let Some(g) = geometry_of(self.st, world)?.found() else { return Ok(()) };
        let n = self.st.int(g.dpvs, "staticSurfaceCount")? as usize;
        if !surfaces_fit(self.st, &g, n)? {
            return Ok(());
        }
        let idx = indexes(self.st, g.i);
        let vraw = self.st.leaf(g.v).raw.clone();
        let lraw = self.st.leaf(g.ld).raw.clone();
        let mut vout = vraw.clone();
        let mut lout = lraw.clone();
        let mut nverts = vraw.len() / VERTEX_BYTES;
        let mut rel: HashMap<NodeId, u32> = HashMap::new(); // surface -> index offset (its block moved into a run's)
        let mut joined = 0;
        let items = self.st.array(g.surfs).items.clone();
        // (surfaces_fit found the first n surfaces structs.)
        let surf = |i: usize| items[i].unwrap_or(NodeId(0));
        type Block = [i64; 3];
        let block_of = |st: &mut Store, s: NodeId| -> Result<Block> {
            let t = st.sub(s, "tris")?;
            Ok([st.int(t, "firstVertex")?, st.int(t, "vertexCount")?, st.int(t, "vertexLayerData")?])
        };
        let mut i = 0;
        while i < n {
            let mut j = i;
            while j + 1 < n && run_key(self.st, surf(j + 1))? == run_key(self.st, surf(i))? {
                j += 1;
            }
            let mut blocks: Vec<Block> = Vec::new();
            for k in i..=j {
                let b = block_of(self.st, surf(k))?;
                if !blocks.contains(&b) {
                    blocks.push(b);
                }
            }
            if blocks.len() > 1 {
                let total: i64 = blocks.iter().map(|b| b[1]).sum();
                if total > 0xFFFF {
                    self.warn(&format!(
                        "{} neighbouring surfaces with one material need more than 65,535 vertices together (decal \
                         layers)",
                        j - i + 1
                    ))?;
                } else {
                    let bpv = layer_bytes(self.st, surf(i))?;
                    let at = nverts;
                    let layer_at = lout.len();
                    let mut off: BTreeMap<Block, u32> = BTreeMap::new();
                    for b in &blocks {
                        off.insert(*b, (nverts - at) as u32);
                        let (first, count, layer) = (b[0] as usize, b[1] as usize, b[2] as usize);
                        vout.extend_from_slice(span(&vraw, VERTEX_BYTES * first, VERTEX_BYTES * (first + count))?);
                        if bpv != 0 {
                            lout.extend_from_slice(span(&lraw, layer, lraw.len().min(layer + bpv * count))?);
                        }
                        nverts += count;
                    }
                    for k in i..=j {
                        let b = block_of(self.st, surf(k))?;
                        let t = self.st.sub(surf(k), "tris")?;
                        rel.insert(surf(k), *off.get(&b).ok_or_else(not_as_expected)?);
                        self.st.set(t, "firstVertex", at as i64)?;
                        self.st.set(t, "vertexCount", total)?;
                        self.st.set(t, "vertexLayerData", if bpv != 0 { layer_at as i64 } else { 0 })?;
                    }
                    joined += 1;
                }
            }
            i = j + 1;
        }
        let mut out: Vec<u32> = Vec::new();
        for sn in self.st.array(g.surfs).items.clone() {
            let s = struct_at(self.st, sn)?;
            let t = self.st.sub(s, "tris")?;
            let b = self.st.int(t, "baseIndex")? as usize;
            let add = rel.get(&s).copied().unwrap_or(0);
            let tri_count = self.st.int(t, "triCount")? as usize;
            self.st.set(t, "baseIndex", out.len() as i64)?;
            let end = idx.len().min(b.wrapping_add(3usize.wrapping_mul(tri_count)));
            for &x in idx.get(b..end).unwrap_or(&[]) {
                out.push(x.wrapping_add(add));
            }
        }
        {
            let v = self.st.leaf_mut(g.v);
            v.raw = vout;
            v.n = nverts as i64;
        }
        {
            let lf = self.st.leaf_mut(g.i);
            lf.raw = pack16(&out, lf.big);
            lf.n = out.len() as i64;
        }
        let ld = self.st.leaf_mut(g.ld);
        if lout.len() > ld.raw.len() {
            ld.raw = lout;
            ld.n = ld.raw.len() as i64;
        }
        let ld_size = ld.raw.len() as i64;
        self.st.set(g.draw, "vertexCount", nverts as i64)?;
        self.st.set(g.draw, "indexCount", out.len() as i64)?;
        self.st.set(g.draw, "vertexLayerDataSize", ld_size)?;
        self.say(&format!(
            "  decal layers: index buffer laid out in surface order, {joined} runs of one material put in one vertex block"
        ))
    }

    /// The world's vertex buffer (and layer data) made again from just the vertices
    /// surfaces use, block by block: merging decals and joining runs leave the old copies
    /// behind. Each block keeps the order of what it holds; indexes are renumbered to match.
    pub(crate) fn compact_vertices(&mut self, world: NodeId) -> Result<()> {
        let Some(g) = geometry_of(self.st, world)?.found() else { return Ok(()) };
        let mut idx = indexes(self.st, g.i);
        #[derive(Default)]
        struct Block {
            surfs: Vec<NodeId>,
            used: BTreeSet<u32>,
            bpv: BTreeSet<usize>,
        }
        let mut blocks: BTreeMap<[i64; 3], Block> = BTreeMap::new(); // (first vertex, count, layer data)
        let mut empty: Vec<NodeId> = Vec::new();
        for sn in self.st.array(g.surfs).items.clone() {
            let Some(s) = sn.filter(|&s| self.st.is_struct(Some(s))) else { continue };
            let t = self.st.sub(s, "tris")?;
            let tri_count = self.st.int(t, "triCount")?;
            if tri_count == 0 {
                empty.push(t); // (draws nothing: pointed at no vertices)
                continue;
            }
            let key =
                [self.st.int(t, "firstVertex")?, self.st.int(t, "vertexCount")?, self.st.int(t, "vertexLayerData")?];
            let base = self.st.int(t, "baseIndex")? as usize;
            let bpv = layer_bytes(self.st, s)?;
            let b = blocks.entry(key).or_default();
            b.surfs.push(s);
            let end = idx.len().min(base.wrapping_add(3usize.wrapping_mul(tri_count as usize)));
            b.used.extend(idx.get(base..end).unwrap_or(&[]).iter().copied());
            b.bpv.insert(bpv);
        }
        if blocks.values().any(|b| b.bpv.len() > 1) {
            return self.warn(
                "decal layers: a vertex block shared by surfaces with different layer data; vertices left as they were",
            );
        }
        let vraw = self.st.leaf(g.v).raw.clone();
        let lraw = self.st.leaf(g.ld).raw.clone();
        let mut vout: Vec<u8> = Vec::new();
        let mut lout: Vec<u8> = lraw[..lraw.len().min(4)].to_vec();
        let before = vraw.len() / VERTEX_BYTES;
        let clipped = |raw: &[u8], from: usize, len: usize| -> Result<Vec<u8>> {
            Ok(span(raw, from.min(raw.len()), from.wrapping_add(len).min(raw.len()))?.to_vec())
        };
        for (key, b) in &blocks {
            let [fv, _, lay] = *key;
            let bpv = b.bpv.first().copied().unwrap_or(0);
            let mut new_of: HashMap<u32, u32> = HashMap::new();
            for (next, &old) in b.used.iter().enumerate() {
                new_of.insert(old, next as u32);
            }
            let first = vout.len() / VERTEX_BYTES;
            let layer_at = lout.len();
            for &k in &b.used {
                let at = VERTEX_BYTES.wrapping_mul((fv as usize).wrapping_add(k as usize));
                vout.extend(clipped(&vraw, at, VERTEX_BYTES)?);
                if bpv != 0 {
                    let from = (lay as usize).wrapping_add(bpv.wrapping_mul(k as usize));
                    lout.extend(clipped(&lraw, from, bpv)?);
                }
            }
            for &s in &b.surfs {
                let t = self.st.sub(s, "tris")?;
                let base = self.st.int(t, "baseIndex")? as usize;
                let tri_count = self.st.int(t, "triCount")? as usize;
                let end = idx.len().min(base.wrapping_add(3usize.wrapping_mul(tri_count)));
                for x in idx.iter_mut().take(end).skip(base) {
                    *x = *new_of.get(x).ok_or_else(not_as_expected)?;
                }
                self.st.set(t, "firstVertex", first as i64)?;
                self.st.set(t, "vertexCount", b.used.len() as i64)?;
                self.st.set(t, "vertexLayerData", if bpv != 0 { layer_at as i64 } else { 0 })?;
            }
        }
        for t in empty {
            self.st.set(t, "firstVertex", 0)?;
            self.st.set(t, "vertexCount", 0)?;
            self.st.set(t, "vertexLayerData", 0)?;
        }
        let vn = {
            let v = self.st.leaf_mut(g.v);
            v.raw = vout;
            v.n = (v.raw.len() / VERTEX_BYTES) as i64;
            v.n
        };
        {
            let lf = self.st.leaf_mut(g.i);
            lf.raw = pack16(&idx, lf.big);
        }
        let ld_size = {
            let ld = self.st.leaf_mut(g.ld);
            ld.raw = lout;
            ld.n = ld.raw.len() as i64;
            ld.n
        };
        self.st.set(g.draw, "vertexCount", vn)?;
        self.st.set(g.draw, "vertexLayerDataSize", ld_size)?;
        self.say(&format!("  decal layers: vertex buffer holds just what surfaces use ({vn} vertices, was {before})"))
    }
}
