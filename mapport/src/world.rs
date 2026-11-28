// The map itself: what the 360 keeps otherwise in the world's drawn geometry (placed
// models, surfaces, culling trees, the light grid), its collision and entities, and the
// models and effects the map carries.

use crate::error::{Error, Result};
use crate::num::{load, load_f32, store, store_f32, truncate, truncate32};
use crate::ordered::Ordered;
use crate::porter::{COLOR_MAP, COLOR_MAP1, Porter, VERTEX_BYTES};
use crate::query::{asset_name, deref, deref_struct, embedded, name, set_holder, target, without_commas};
use crate::tree::{ElemType, FOLLOWING, INSERT, Leaf, NodeId, Roots, Store, Ty, for_each};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The median of stock Favela's models.
const HIMIP_RADIUS: u32 = 1238;
const PROBE_STOCK_LEVEL: i32 = 70;

// ---- reading the world's data -----------------------------------------------------------------

/// A Leaf, through a Ref to it.
pub(crate) fn leaf_of(st: &Store, c: Option<NodeId>) -> Option<NodeId> {
    target(st, c).filter(|&n| st.is_leaf(Some(n)))
}

/// An Array, through a Ref to it.
pub(crate) fn array_of(st: &Store, c: Option<NodeId>) -> Option<NodeId> {
    target(st, c).filter(|&n| st.is_array(Some(n)))
}

pub(crate) fn cut_short() -> Error {
    Error::port("the world's data is cut short")
}

/// What a list of the world's structs holds where a struct belongs.
pub(crate) fn not_as_expected() -> Error {
    Error::port("the world's data is not as expected")
}

/// A struct of the world's that must be one.
pub(crate) fn struct_at(st: &Store, n: Option<NodeId>) -> Result<NodeId> {
    n.filter(|&n| st.is_struct(Some(n))).ok_or_else(not_as_expected)
}

/// A number in plain data; one past its end is a broken file.
fn at(raw: &[u8], at: usize, size: usize) -> Result<&[u8]> {
    if at > raw.len() || size > raw.len() - at {
        return Err(cut_short());
    }
    Ok(&raw[at..at + size])
}

fn at_mut(raw: &mut [u8], at: usize, size: usize) -> Result<&mut [u8]> {
    if at > raw.len() || size > raw.len() - at {
        return Err(cut_short());
    }
    Ok(&mut raw[at..at + size])
}

fn u16_at(lf: &Leaf, off: usize) -> Result<u32> {
    Ok(load(at(&lf.raw, off, 2)?, 2, lf.big) as u32)
}

fn u32_at(lf: &Leaf, off: usize) -> Result<u32> {
    Ok(load(at(&lf.raw, off, 4)?, 4, lf.big) as u32)
}

fn f32_at(lf: &Leaf, off: usize) -> Result<f64> {
    Ok(f64::from(load_f32(at(&lf.raw, off, 4)?, lf.big)))
}

/// `count` 16-bit numbers from `off`.
fn words16(lf: &Leaf, count: usize, off: usize) -> Result<Vec<u32>> {
    (0..count).map(|i| u16_at(lf, off.wrapping_add(2usize.wrapping_mul(i)))).collect()
}

/// The size of a Leaf's elements.
fn elem_size(lf: &Leaf) -> Result<usize> {
    lf.t.map(|t| t.size() as usize).ok_or_else(not_as_expected)
}

fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) { q - 1 } else { q }
}

/// The floats of a Bounds struct (midPoint, halfSize).
fn read_bounds(st: &Store, s: NodeId) -> Result<([f64; 3], [f64; 3])> {
    let p = st.data(s);
    let big = st.st(s).big;
    let mut mid = [0.0; 3];
    let mut half = [0.0; 3];
    for k in 0..3 {
        mid[k] = f64::from(load_f32(at(p, 4 * k, 4)?, big));
        half[k] = f64::from(load_f32(at(p, 12 + 4 * k, 4)?, big));
    }
    Ok((mid, half))
}

/// The smaller of two numbers, the first when neither is (NaN).
pub(crate) fn min_f(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// The larger of two numbers, the first when neither is (NaN).
pub(crate) fn max_f(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

pub(crate) fn clamp_f(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if hi < v {
        hi
    } else {
        v
    }
}

/// A number in fixed notation with `prec` decimals, as printf's %.<prec>f writes it.
fn fixed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return if x.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    format!("{x:.prec$}")
}

/// A line cut to what a buffer of `size` bytes holds with its terminating zero.
fn fitted(mut line: String, size: usize) -> String {
    line.truncate(size - 1);
    line
}

/// Zeroed bytes for a count read from the file.
fn zeros(len: usize) -> Result<Vec<u8>> {
    if len > 1 << 31 {
        return Err(Error::port("the world's data holds a count too large"));
    }
    Ok(vec![0; len])
}

// ---- matching the entity text -------------------------------------------------------------------

/// The spans of each entity ("{" anything but braces "}") in turn.
fn entities(text: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < text.len() {
        if text[p] == b'{' {
            let mut q = p + 1;
            while q < text.len() && text[q] != b'{' && text[q] != b'}' {
                q += 1;
            }
            if q < text.len() && text[q] == b'}' {
                out.push((p, q + 1));
                p = q + 1;
                continue;
            }
        }
        p += 1;
    }
    out
}

/// The C locale's white space.
fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// The first `"classname"`, white space, `"value"` of an entity: its value.
fn classname(ent: &[u8]) -> Option<&[u8]> {
    const KEY: &[u8] = b"\"classname\"";
    for p in 0..ent.len() {
        if !ent[p..].starts_with(KEY) {
            continue;
        }
        let mut q = p + KEY.len();
        let spaces = q;
        while q < ent.len() && space(ent[q]) {
            q += 1;
        }
        if q == spaces || q >= ent.len() || ent[q] != b'"' {
            continue;
        }
        let start = q + 1;
        if let Some(len) = ent[start..].iter().position(|&c| c == b'"') {
            return Some(&ent[start..start + len]);
        }
    }
    None
}

// ---- a glass type's key -------------------------------------------------------------------------

/// A glass type's texture vectors, rounded: what its 360-only numbers follow.
fn glass_key(st: &Store, d: NodeId) -> Result<String> {
    let mut key = String::new();
    for k in 0..4 {
        key += &fitted(format!("{},", fixed(st.f_at(d, "texVecs", k)?.abs(), 6)), 32);
    }
    Ok(key)
}

/// A culling tree node's subtree's models, worked out once for each node.
fn subtree(
    lists: &[Vec<u32>],
    kids: &[Vec<usize>],
    sub: &mut BTreeMap<usize, BTreeSet<u32>>,
    i: usize,
    depth: i32,
) -> Result<()> {
    if depth > 64 {
        return Err(Error::new("tree too deep"));
    }
    if i >= lists.len() {
        return Err(Error::new("a child past the tree"));
    }
    if sub.contains_key(&i) {
        return Ok(());
    }
    let mut s: BTreeSet<u32> = lists[i].iter().copied().collect();
    for &k in &kids[i] {
        subtree(lists, kids, sub, k, depth + 1)?;
        if let Some(below) = sub.get(&k) {
            s.extend(below.iter().copied());
        }
    }
    sub.insert(i, s);
    Ok(())
}

/// A node's own models (sorted), then each child's subtree in turn; each node's run.
fn lay(
    lists: &[Vec<u32>],
    kids: &[Vec<usize>],
    sub: &BTreeMap<usize, BTreeSet<u32>>,
    i: usize,
    order: &mut Vec<u32>,
    span: &mut BTreeMap<usize, (usize, usize)>,
) {
    let start = order.len();
    let mut below = BTreeSet::new();
    for k in &kids[i] {
        if let Some(s) = sub.get(k) {
            below.extend(s.iter().copied());
        }
    }
    let mut own: Vec<u32> = lists[i].iter().copied().filter(|m| !below.contains(m)).collect();
    own.sort_unstable();
    order.extend_from_slice(&own);
    for &k in &kids[i] {
        lay(lists, kids, sub, k, order, span);
    }
    span.insert(i, (start, order.len() - start));
}

/// A surface's sort key in the 360's surface lists: its material and its lighting.
type RunKey = (Option<NodeId>, Vec<u8>);

/// What a material's surfaces count for in the world's material memory list.
struct Use {
    name: String,
    reference: NodeId,
    runs: BTreeSet<(i64, i64)>,
    tris: i64,
    surfs: i64,
}

/// A chunk of a material's surfaces: its number, the place of the material's first
/// surface, and its surfaces.
struct Chunk {
    c: usize,
    first: usize,
    members: Vec<usize>,
}

/// Renumbers the first `count` (else all) surface numbers of a list.
fn renumber(st: &mut Store, lf: Option<NodeId>, count: Option<usize>, n: usize, new_of: &[u32]) -> Result<()> {
    let Some(lf) = lf else { return Ok(()) };
    let leaf = st.leaf_mut(lf);
    if leaf.n == 0 {
        return Ok(());
    }
    let k = count.unwrap_or(leaf.n as usize);
    for i in 0..k {
        let v = u16_at(leaf, 2 * i)?;
        let renumbered = if (v as usize) < n { new_of[v as usize] } else { v };
        store(&mut leaf.raw[2 * i..], 2, u64::from(renumbered), leaf.big);
    }
    Ok(())
}

impl Porter<'_> {
    // ---- placed models ------------------------------------------------------------------------

    /// PC: dynEntVisData[2][3]; 360: six pointers dynEntVisData0_0 .. 1_2.
    pub(crate) fn pre_gfx_world_dpvs_dynamic(&mut self, d: NodeId, x: NodeId) -> Result<bool> {
        for i in 0..2i32 {
            let pl = self.st.pop_child_at(d, "dynEntVisData", &[i]);
            for j in 0..3usize {
                let k = format!("dynEntVisData{i}_{j}");
                let v = self.st.ptr_at(d, "dynEntVisData", i as usize * 3 + j)?;
                self.st.set_ptr(x, &k, v)?;
                let item = self.st.as_ptr_list(pl).and_then(|p| p.items.get(j).copied().flatten());
                if let Some(item) = item {
                    let key = &self.st.m(x, &k)?.name;
                    self.st.set_child(d, key, Some(item));
                }
            }
        }
        Ok(false)
    }

    /// PC: placement {origin, axis[3][3], scale}; 360: origin and packedAxis[4], each axis
    /// three signed 10-bit numbers (x 511, rounded) and the scale as a float (worked out
    /// from stock maps, whose collision holds the same models with plain axes). Left zero,
    /// every model drew nowhere.
    pub(crate) fn pre_gfx_static_model_draw_inst(&mut self, d: NodeId, x: NodeId) -> Result<bool> {
        if !self.model_swap.is_empty() {
            let c = self.st.child(d, "model");
            let m = if self.st.is_struct(c) {
                c
            } else if self.st.is_ref(c) {
                deref_struct(self.st, c)?
            } else {
                None
            };
            if let Some(&entry) = m.and_then(|m| self.model_swap.get(&m)) {
                let r = self.st.make_ref(0);
                let rf = self.st.reference_mut(r);
                rf.target = Some(entry); // the stock model, through its asset list entry
                rf.rel = 4;
                self.st.set_child(d, "model", Some(r));
                self.st.set_ptr(d, "model", 0)?;
            }
        }
        if self.st.find(d, "placement").is_none() {
            return Ok(false);
        }
        let pl = self.st.sub(d, "placement")?;
        for k in 0..3 {
            let v = self.st.f_at(pl, "origin", k)?;
            self.st.set_f_at(x, "origin", v, k)?;
        }
        for a in 0..3 {
            let mut v: u32 = 0;
            for k in 0..3 {
                let c = clamp_f(self.st.f_at(pl, "axis", a * 3 + k)?, -1.0, 1.0);
                v |= ((truncate((c * 511.0 + 0.5).floor()) as u32) & 0x3FF) << (10 * k);
            }
            self.st.set_at(x, "packedAxis", i64::from(v), a)?;
        }
        let scale = (self.st.f(pl, "scale")? as f32).to_bits();
        self.st.set_at(x, "packedAxis", i64::from(scale), 3)?;
        // Placed-model flags (from PC mp_rust, mp_terminal and mp_afghan against stock 360
        // ones): the PC's 0x20 (ground lighting) is the 360's 0x02, the PC's 0x10 the 360's
        // 0x01; its low bits have no 360 counterpart. The ground colour is kept with its
        // bytes the other way round.
        let f = self.st.int(d, "flags")? & 0xFF;
        let mut flags: i64 = (if f & 0x20 != 0 { 0x02 } else { 0 }) | (if f & 0x10 != 0 { 0x01 } else { 0 });
        if let Some(gl) = embedded(self.st, d, "groundLighting") {
            let lit = self.st.data(gl).iter().any(|&b| b != 0);
            if flags & 0x02 == 0 && lit {
                flags |= 0x02;
                self.ground_lit += 1;
            }
            self.st.data_mut(gl).reverse();
        }
        self.st.set(d, "flags", flags)?;
        Ok(false)
    }

    /// The PC keeps a water surface's starting waves as one array of complex numbers (H0);
    /// the 360 keeps the real and imaginary parts apart (H0X, H0Y). The game reads them every
    /// frame.
    pub(crate) fn pre_water_t(&mut self, d: NodeId, x: NodeId) -> Result<bool> {
        let popped = self.st.pop_child(d, "H0");
        let Some(h0) = leaf_of(self.st, popped) else { return Ok(false) };
        let f = Ty::named(self.p, "float").ok_or_else(|| Error::new("the PC's rules have no float"))?;
        let (n, raw, big) = {
            let lf = self.st.leaf(h0);
            ((lf.n.max(0) as usize).min(lf.raw.len() / 8), lf.raw.clone(), lf.big)
        };
        for part in 0..2 {
            let mut out = vec![0u8; 4 * n];
            for i in 0..n {
                out[4 * i..4 * i + 4].copy_from_slice(&raw[8 * i + 4 * part..8 * i + 4 * part + 4]);
            }
            let k = if part == 1 { "H0Y" } else { "H0X" };
            let key = &self.st.m(x, k)?.name;
            let leaf = self.st.make_leaf(Some(f), n as i64, out, big);
            self.st.set_child(d, key, Some(leaf));
            self.st.set_ptr(x, k, FOLLOWING)?;
        }
        Ok(false)
    }

    // ---- the world's drawn geometry -----------------------------------------------------------

    /// childrenOffset is a byte distance to the node's first child: nodes are 44 bytes on
    /// the PC and 40 on the 360.
    pub(crate) fn post_gfx_aabb_tree(&mut self, d: NodeId) -> Result<()> {
        let tp = Ty::named(self.p, "GfxAabbTree").ok_or_else(|| Error::new("the PC's rules have no GfxAabbTree"))?;
        if tp.size() == 0 {
            return Err(not_as_expected());
        }
        let v = floor_div(self.st.int(d, "childrenOffset")?, i64::from(tp.size()))
            .wrapping_mul(i64::from(self.st.st(d).ty.size()));
        self.st.set(d, "childrenOffset", v)
    }

    /// The PC sorts surfaces twice (all, then without decals); the 360 keeps one list.
    pub(crate) fn post_gfx_world_dpvs_static(&mut self, d: NodeId) -> Result<()> {
        let c = self.st.child(d, "sortedSurfIndex");
        let n = self.st.int(d, "staticSurfaceCount")?;
        let Some(lf) = self.st.as_leaf_mut(c) else { return Ok(()) };
        if lf.n > n {
            let size = elem_size(lf)?;
            if n < 0 {
                return Err(not_as_expected());
            }
            lf.raw.resize(n as usize * size, 0);
            lf.n = n;
        }
        Ok(())
    }

    /// IW4x's ZoneBuilder fills the lightmap override pointers with the sky and $outdoor
    /// pictures; stock 360 maps leave them empty (set, the game would light every surface
    /// with them).
    pub(crate) fn post_gfx_world_draw(&mut self, d: NodeId) -> Result<()> {
        for k in ["lightmapOverridePrimary", "lightmapOverrideSecondary"] {
            let c = self.st.pop_child(d, k);
            self.st.set_ptr(d, k, 0)?;
            // The picture was written here first; later pointers to it get it instead.
            if let Some(s) = c.filter(|&s| self.st.is_struct(Some(s))) {
                self.moved_images.push(s);
            }
        }
        Ok(())
    }

    pub(crate) fn post_gfx_world(&mut self, d: NodeId) -> Result<()> {
        self.merge_decals(d)?;
        self.order_surfaces(d)?;
        self.relay_surfaces(d)?;
        self.compact_vertices(d)?;
        self.fill_surface_bounds(d)?;
        self.grow_tree_bounds(d)?;
        self.fill_lighting_origins(d)?;
        self.slice_tree_lists(d)?;
        self.dim_standin_probes(d)?;
        self.world_material_memory(d)?;
        // One bit per room ("has sun-lit surfaces"), 32 to a word. Stock 360 maps always
        // carry it and the renderer reads it every frame; PC files can have none, and the
        // conversion crashed as the first frame drew.
        let planes = self.st.sub(d, "dpvsPlanes")?;
        let cells = self.st.int(planes, "cellCount")?;
        if cells != 0 && self.st.child(d, "cellHasSunLitSurfsBits").is_none() {
            let caster = leaf_of(self.st, self.st.child(d, "cellCasterBits"));
            let n = (cells.wrapping_add(31) as usize) / 32;
            let m = self.st.m(d, "cellHasSunLitSurfsBits")?;
            let t = match caster {
                Some(c) => self.st.leaf(c).t,
                None => Some(self.st.st(d).ty.of(m)),
            };
            let raw = zeros(n.wrapping_mul(4))?;
            let leaf = self.st.make_leaf(t, n as i64, raw, true);
            self.st.set_child(d, &m.name, Some(leaf));
            self.st.set_ptr(d, "cellHasSunLitSurfsBits", FOLLOWING)?;
            self.say("  world: room sun-light bits filled in (the PC file has none)")?;
        }
        if let Some(ch) = self.st.st(d).ch() {
            for i in 0..self.st.children(ch).items.len() {
                let (key, c) = self.st.children(ch).items[i].clone();
                let t = if self.st.is_ref(c) { deref(self.st, c)? } else { None };
                let Some(t) = t else { continue };
                if !self.moved_images.contains(&t) {
                    continue;
                }
                self.st.children_mut(ch).items[i].1 = Some(t);
                set_holder(self.st, d, key.member, INSERT, 0)?;
                self.moved_images.retain(|&m| m != t);
            }
        }
        if !self.moved_images.is_empty() {
            return Err(Error::port("a picture from the lightmap override is still needed elsewhere"));
        }
        Ok(())
    }

    /// The key surfaces drawn as one share: their material and lighting.
    fn solid_key(&mut self, surfs: &[Option<NodeId>], i: usize) -> Result<RunKey> {
        let s = struct_at(self.st, surfs.get(i).copied().flatten())?;
        let laf = embedded(self.st, s, "laf");
        let material = deref(self.st, self.st.child(s, "material"))?;
        Ok((material, laf.map(|l| self.st.data(l).to_vec()).unwrap_or_default()))
    }

    /// With decal layers merged: the solid surfaces sorted so those with one material (and
    /// lighting) are neighbours, which the 360 draws as one; solid surfaces depth-test, so
    /// their order doesn't change the picture. Decals and see-through ones keep their order.
    /// A run takes at most 65,535 vertices (16-bit indexes): bigger ones are cut in chunks,
    /// kept apart by other materials' surfaces (a sort key with one material keeps its
    /// surfaces as they were).
    pub(crate) fn solid_runs(
        &mut self,
        surfs: &[Option<NodeId>],
        order: &[usize],
        groups: &[i32],
        sort_keys: &[i64],
    ) -> Result<Vec<usize>> {
        let mut solid = Vec::new();
        let mut rest = Vec::new();
        for &i in order {
            if groups[i] == 0 { solid.push(i) } else { rest.push(i) }
        }
        let mut by_sort: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for &i in &solid {
            by_sort.entry(sort_keys[i]).or_default().push(i);
        }
        let mut out = Vec::new();
        for idx in by_sort.values() {
            let mut keys: Vec<(RunKey, Vec<usize>)> = Vec::new();
            for &i in idx {
                let k = self.solid_key(surfs, i)?;
                match keys.iter_mut().find(|e| e.0 == k) {
                    Some(e) => e.1.push(i),
                    None => keys.push((k, vec![i])),
                }
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            for (_, members) in &keys {
                let first = idx.iter().position(|&x| x == members[0]).unwrap_or(idx.len());
                let mut blocks: BTreeSet<(i64, i64)> = BTreeSet::new();
                let mut total: i64 = 0;
                let mut cur: Vec<usize> = Vec::new();
                let mut c = 0;
                for &i in members {
                    let s = struct_at(self.st, surfs.get(i).copied().flatten())?;
                    let t = self.st.sub(s, "tris")?;
                    let b = (self.st.int(t, "firstVertex")?, self.st.int(t, "vertexCount")?);
                    let mut add = if blocks.contains(&b) { 0 } else { b.1 };
                    if !cur.is_empty() && total.wrapping_add(add) > 0xFFFF {
                        chunks.push(Chunk { c, first, members: std::mem::take(&mut cur) });
                        blocks.clear();
                        total = 0;
                        c += 1;
                        add = b.1;
                    }
                    blocks.insert(b);
                    total = total.wrapping_add(add);
                    cur.push(i);
                }
                chunks.push(Chunk { c, first, members: cur });
            }
            if keys.len() == 1 && chunks.len() > 1 {
                out.extend_from_slice(idx); // nothing to keep its chunks apart
                continue;
            }
            // Chunk 0 of every material (in the order they first appear), then chunk 1 of
            // those that have one, ...; a round doesn't start with the material the one
            // before ended with (it's rotated).
            let mut rounds: BTreeMap<usize, Vec<(usize, Vec<usize>)>> = BTreeMap::new();
            for ch in chunks {
                rounds.entry(ch.c).or_default().push((ch.first, ch.members));
            }
            let mut flat: Vec<Vec<usize>> = Vec::new();
            for mut part in rounds.into_values() {
                part.sort();
                let mut run: Vec<Vec<usize>> = part.into_iter().map(|p| p.1).collect();
                if let Some(last) = flat.last() {
                    let (a, b) = (run[0][0], last[0]);
                    if self.solid_key(surfs, a)? == self.solid_key(surfs, b)? {
                        run.rotate_left(1);
                    }
                }
                flat.extend(run);
            }
            let mut meet = false;
            for j in 0..flat.len().saturating_sub(1) {
                let (a, b) = (flat[j][0], flat[j + 1][0]);
                meet |= self.solid_key(surfs, a)? == self.solid_key(surfs, b)?;
            }
            if meet {
                out.extend_from_slice(idx); // (one material left with chunks: as it was)
                continue;
            }
            for members in &flat {
                out.extend_from_slice(members);
            }
        }
        out.extend_from_slice(&rest);
        Ok(out)
    }

    /// The static surfaces in the 360 tools' order (solid, sort key below 6; then decals and
    /// see-through; then shadow casters; each group keeping its order) and the ranges to
    /// match. Every per-surface array moves with them and every surface number is
    /// renumbered. Culling tree nodes index sortedSurfIndex, which keeps its order. Brush
    /// model surfaces stay.
    pub(crate) fn order_surfaces(&mut self, world: NodeId) -> Result<()> {
        let dpvs = self.st.sub(world, "dpvs")?;
        let surfs = array_of(self.st, self.st.child(dpvs, "surfaces"));
        let n = self.st.int(dpvs, "staticSurfaceCount")? as usize;
        let Some(surfs) = surfs.filter(|&a| n != 0 && self.st.array(a).items.len() >= n) else {
            return self.warn("surfaces couldn't be put in 360 order (data not found)");
        };
        let items = self.st.array(surfs).items.clone();
        let mut groups = vec![0i32; n];
        let mut sort_keys = vec![0i64; n];
        for i in 0..n {
            let s = items[i].filter(|&s| self.st.is_struct(Some(s)));
            let m = match s {
                Some(s) => deref_struct(self.st, self.st.child(s, "material"))?,
                None => None,
            };
            let Some(m) = m else { continue };
            let ts = deref_struct(self.st, self.st.child(m, "techniqueSet"))?;
            if let Some(ts) = ts
                && asset_name(self.st, ts).unwrap_or_default().contains("shadowcaster")
            {
                groups[i] = 2;
                continue;
            }
            let info = self.st.sub(m, "info")?;
            let key = self.st.int(info, "sortKey")?;
            groups[i] = if key < 6 { 0 } else { 1 };
            sort_keys[i] = key;
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| groups[i]);
        let order = self.solid_runs(&items, &order, &groups, &sort_keys)?;
        let moved = (0..n).any(|i| order[i] != i);
        if moved {
            let mut new_of = vec![0u32; n];
            for (k, &o) in order.iter().enumerate().take(n) {
                new_of[o] = k as u32;
            }
            let reordered: Vec<Option<NodeId>> = order.iter().take(n).map(|&o| items[o]).collect();
            self.st.array_mut(surfs).items[..n].copy_from_slice(&reordered);
            for key in ["surfacesBounds", "surfaceMaterials"] {
                let Some(lf) = leaf_of(self.st, self.st.child(dpvs, key)) else { continue };
                let lf = self.st.leaf_mut(lf);
                if (lf.n as usize) < n {
                    continue;
                }
                let size = elem_size(lf)?;
                let mut raw = Vec::with_capacity(lf.raw.len());
                for &i in order.iter().take(n) {
                    raw.extend_from_slice(lf.raw.get(i * size..(i + 1) * size).ok_or_else(cut_short)?);
                }
                raw.extend_from_slice(lf.raw.get(n * size..).ok_or_else(cut_short)?);
                lf.raw = raw;
            }
            if let Some(bits) = leaf_of(self.st, self.st.child(dpvs, "surfaceCastsSunShadow"))
                && !self.st.leaf(bits).raw.is_empty()
            {
                let bits = self.st.leaf_mut(bits);
                let count = bits.raw.len() / 4;
                let mut words = vec![0u32; count];
                for (w, word) in words.iter_mut().enumerate() {
                    *word = u32_at(bits, 4 * w)?;
                }
                let mut out = words.clone();
                for (w, word) in out.iter_mut().enumerate().take(count.min(n.div_ceil(32))) {
                    *word &= !(if (w + 1) * 32 <= n { 0xFFFF_FFFFu32 } else { (1u32 << (n - w * 32)) - 1 });
                }
                for old in 0..n {
                    let new = new_of[old] as usize;
                    if (old >> 5) < count && (new >> 5) < count && (words[old >> 5] >> (old & 31)) & 1 != 0 {
                        out[new >> 5] |= 1u32 << (new & 31);
                    }
                }
                for (w, &word) in out.iter().enumerate() {
                    store(&mut bits.raw[4 * w..], 4, u64::from(word), bits.big);
                }
            }
            let sorted = leaf_of(self.st, self.st.child(dpvs, "sortedSurfIndex"));
            renumber(self.st, sorted, None, n, &new_of)?;
            if let Some(geoms) = array_of(self.st, self.st.child(world, "shadowGeom")) {
                for g in self.st.array(geoms).items.clone() {
                    let Some(gm) = g.filter(|&g| self.st.is_struct(Some(g))) else { continue };
                    let count = self.st.int(gm, "surfaceCount")?;
                    if count != 0 {
                        let lf = leaf_of(self.st, self.st.child(gm, "sortedSurfIndex"));
                        renumber(self.st, lf, Some(count as usize), n, &new_of)?;
                    }
                }
            }
        }
        let count = |g: i32| groups.iter().filter(|&&x| x == g).count() as i64;
        let a = count(0);
        let b = a + count(1);
        let c = b + count(2);
        self.st.set(dpvs, "litOpaqueSurfsBegin", 0)?;
        self.st.set(dpvs, "litOpaqueSurfsEnd", a)?;
        self.st.set(dpvs, "litTransSurfsBegin", a)?;
        self.st.set(dpvs, "litTransSurfsEnd", b)?;
        self.st.set(dpvs, "shadowCasterSurfsBegin", b)?;
        self.st.set(dpvs, "shadowCasterSurfsEnd", c)?;
        self.st.set(dpvs, "emissiveSurfsBegin", c)?;
        self.st.set(dpvs, "emissiveSurfsEnd", c)?;
        self.say(&format!(
            "  surfaces in 360 order: {a} solid, {} decals / see-through, {} shadow casters{}",
            b - a,
            c - b,
            if moved { "" } else { " (already in order)" }
        ))
    }

    /// The width of a material's picture of this name hash (0 without it), once for each
    /// material and hash.
    fn picture_width(
        &mut self,
        widths: &mut BTreeMap<(NodeId, u32), i64>,
        c: Option<NodeId>,
        hash: u32,
    ) -> Result<i64> {
        let mat = if self.st.is_ref(c) { deref_struct(self.st, c)? } else { c.filter(|&m| self.st.is_struct(Some(m))) };
        let Some(mat) = mat else { return Ok(0) };
        if let Some(&w) = widths.get(&(mat, hash)) {
            return Ok(w);
        }
        let mut w: i64 = 0;
        let tt = self.st.child(mat, "textureTable");
        if let Some(tt) = self.st.as_array(tt) {
            for n in tt.items.clone() {
                let t = n.filter(|&t| self.st.is_struct(Some(t)));
                let u = t.and_then(|t| embedded(self.st, t, "u"));
                let Some(t) = t else { continue };
                if self.st.u(t, "nameHash")? != u64::from(hash) {
                    continue;
                }
                let Some(u) = u else { continue };
                if let Some(img) = deref_struct(self.st, self.st.child(u, "image"))? {
                    w = self.st.int(img, "width")?;
                    for k in 0..4 {
                        let s = self.st.sub_at(img, "streams", k)?;
                        w = w.max(self.st.int(s, "width")?);
                    }
                    // A picture the game has loaded (",name"): its size isn't here.
                    if w <= 1 {
                        w = 512;
                    }
                }
            }
        }
        widths.insert((mat, hash), w);
        Ok(w)
    }

    /// GfxSurfaceBounds has two more words on the 360: the first is radius << 16 | colorMap
    /// density << 8 | colorMap1 density. Radius: from the bounds' middle to the farthest
    /// vertex, rounded up. Density: 26 x picture width / (world units per texture repeat), 0
    /// without that picture (worked out from stock mp_rust). Left 0, the game took every
    /// surface for a point.
    pub(crate) fn fill_surface_bounds(&mut self, world: NodeId) -> Result<()> {
        let draw = self.st.sub(world, "draw")?;
        let dpvs = self.st.sub(world, "dpvs")?;
        let leaf = |st: &Store, c: Option<NodeId>| -> Option<NodeId> {
            let mut c = c;
            if let Some(r) = st.as_ref(c)
                && r.rel == 0
            {
                c = r.target;
            }
            c.filter(|&n| st.is_leaf(Some(n)))
        };
        let vd = self.st.sub(draw, "vd")?;
        let verts = leaf(self.st, self.st.child(vd, "vertices"));
        let idx = leaf(self.st, self.st.child(draw, "indices"));
        let surfs = array_of(self.st, self.st.child(dpvs, "surfaces"));
        let bounds = leaf(self.st, self.st.child(dpvs, "surfacesBounds"));
        let (Some(verts), Some(idx), Some(surfs), Some(bounds)) = (verts, idx, surfs, bounds) else {
            return self.warn("the world's surfaces get no culling radius (data not found)");
        };
        let indexes = {
            let lf = self.st.leaf(idx);
            words16(lf, lf.n as usize, 0)?
        };
        let size = elem_size(self.st.leaf(bounds))?;
        let mut widths: BTreeMap<(NodeId, u32), i64> = BTreeMap::new();
        let items = self.st.array(surfs).items.clone();
        let count = items.len().min(self.st.leaf(bounds).n as usize);
        for (k, &item) in items.iter().enumerate().take(count) {
            let Some(s) = item.filter(|&s| self.st.is_struct(Some(s))) else { continue };
            let t = self.st.sub(s, "tris")?;
            let mut mid = [0.0f64; 3];
            for (q, m) in mid.iter_mut().enumerate() {
                *m = f32_at(self.st.leaf(bounds), k * size + 4 * q)?;
            }
            let base = self.st.int(t, "baseIndex")? as usize;
            let fv = self.st.int(t, "firstVertex")? as usize;
            let tri_count = self.st.int(t, "triCount")? as usize;
            let end = indexes.len().min(base.wrapping_add(3usize.wrapping_mul(tri_count)));
            let (mut r2, mut world_area, mut uv_area) = (0.0f64, 0.0f64, 0.0f64);
            let vl = self.st.leaf(verts);
            let mut j = base;
            while j.wrapping_add(2) < end {
                let mut p = [[0.0f64; 3]; 3];
                let mut uv = [[0.0f64; 2]; 3];
                for v in 0..3 {
                    let i = *indexes.get(j.wrapping_add(v)).ok_or_else(cut_short)? as usize;
                    let o = fv.wrapping_add(i).wrapping_mul(VERTEX_BYTES);
                    for (q, x) in p[v].iter_mut().enumerate() {
                        *x = f32_at(vl, o.wrapping_add(4 * q))?;
                    }
                    for (q, x) in uv[v].iter_mut().enumerate() {
                        *x = f32_at(vl, o.wrapping_add(20 + 4 * q))?;
                    }
                    let mut d2 = 0.0f64;
                    for q in 0..3 {
                        d2 += (p[v][q] - mid[q]) * (p[v][q] - mid[q]);
                    }
                    r2 = max_f(r2, d2);
                }
                let mut a = [0.0f64; 3];
                let mut b = [0.0f64; 3];
                for q in 0..3 {
                    a[q] = p[1][q] - p[0][q];
                    b[q] = p[2][q] - p[0][q];
                }
                let cr = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                world_area += (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
                uv_area += ((uv[1][0] - uv[0][0]) * (uv[2][1] - uv[0][1])
                    - (uv[1][1] - uv[0][1]) * (uv[2][0] - uv[0][0]))
                    .abs();
                j = j.wrapping_add(3);
            }
            let radius = truncate(min_f(65535.0, r2.sqrt().ceil())) as u32;
            let mut dens = [0u32; 2];
            let material = self.st.child(s, "material");
            for (h, hash) in [COLOR_MAP, COLOR_MAP1].into_iter().enumerate() {
                let w = self.picture_width(&mut widths, material, hash)?;
                if w == 0 || world_area == 0.0 || uv_area == 0.0 || !world_area.is_finite() || !uv_area.is_finite() {
                    dens[h] = 0;
                    continue;
                }
                let v = (26.0 * w as f64 / (world_area / uv_area).sqrt()).round_ties_even();
                dens[h] = truncate(max_f(1.0, min_f(255.0, v))) as u32;
            }
            let lf = self.st.leaf_mut(bounds);
            let big = lf.big;
            store(at_mut(&mut lf.raw, k * size + 24, 4)?, 4, u64::from(radius << 16 | dens[0] << 8 | dens[1]), big);
        }
        Ok(())
    }

    /// Every culling tree node's box grown to enclose the static models it lists. Stock 360
    /// maps always have them inside; maps ported from CoD4 don't, and the 360 skips a node's
    /// models when its box is off screen: they vanish as you turn. A bigger box only means
    /// drawing more.
    pub(crate) fn grow_tree_bounds(&mut self, world: NodeId) -> Result<()> {
        let dpvs = self.st.sub(world, "dpvs")?;
        let insts = leaf_of(self.st, self.st.child(dpvs, "smodelInsts"));
        let trees = array_of(self.st, self.st.child(world, "aabbTrees"));
        let (Some(insts), Some(trees)) = (insts, trees) else { return Ok(()) };
        let count = self.st.leaf(insts).raw.len() / 36;
        let mut grown = 0;
        let mut far = 0.0f64;
        for ctn in self.st.array(trees).items.clone() {
            let ct = ctn.filter(|&c| self.st.is_struct(Some(c)));
            let Some(nodes) = ct.and_then(|ct| array_of(self.st, self.st.child(ct, "aabbTree"))) else { continue };
            for ndn in self.st.array(nodes).items.clone() {
                let Some(nd) = ndn.filter(|&n| self.st.is_struct(Some(n))) else { continue };
                let cnt = self.st.int(nd, "smodelIndexCount")?;
                let mut c = self.st.child(nd, "smodelIndexes");
                let mut off = 0usize;
                if let Some(r) = self.st.as_ref(c) {
                    off = r.rel as usize;
                    c = r.target;
                }
                let lf = c.filter(|&l| self.st.is_leaf(Some(l)));
                let Some(lf) = lf.filter(|_| cnt != 0) else { continue };
                let b = self.st.sub(nd, "bounds")?;
                let (mid, half) = read_bounds(self.st, b)?;
                let mut lo = [0.0f64; 3];
                let mut hi = [0.0f64; 3];
                for k in 0..3 {
                    lo[k] = mid[k] - half[k];
                    hi[k] = mid[k] + half[k];
                }
                let mut out = false;
                let (list, inst) = (self.st.leaf(lf), self.st.leaf(insts));
                for j in 0..cnt.max(0) as usize {
                    let i = u16_at(list, off.wrapping_add(2 * j))? as usize;
                    if i >= count {
                        continue;
                    }
                    let mut bx = [0.0f64; 6];
                    for (q, v) in bx.iter_mut().enumerate() {
                        *v = f32_at(inst, 36 * i + 4 * q)?;
                    }
                    for k in 0..3 {
                        if bx[k] - bx[3 + k] < lo[k] || bx[k] + bx[3 + k] > hi[k] {
                            far = max_f(max_f(far, lo[k] - (bx[k] - bx[3 + k])), (bx[k] + bx[3 + k]) - hi[k]);
                            lo[k] = min_f(lo[k], bx[k] - bx[3 + k]);
                            hi[k] = max_f(hi[k], bx[k] + bx[3 + k]);
                            out = true;
                        }
                    }
                }
                if !out {
                    continue;
                }
                grown += 1;
                let big = self.st.st(b).big;
                let data = self.st.data_mut(b);
                for k in 0..3 {
                    store_f32(at_mut(data, 4 * k, 4)?, ((lo[k] + hi[k]) / 2.0) as f32, big);
                    store_f32(at_mut(data, 12 + 4 * k, 4)?, ((hi[k] - lo[k]) / 2.0) as f32, big);
                }
            }
        }
        if grown != 0 {
            let line = format!(
                "  culling tree: {grown} node boxes grown to enclose their static models (up to {} units)",
                fixed(far, 0)
            );
            self.say(&fitted(line, 160))?;
        }
        Ok(())
    }

    /// GfxStaticModelInst is (box centre, box half-size, lighting origin). The 360 samples
    /// the light grid at the lighting origin; custom-compiled maps can leave it (0,0,0):
    /// those take the box centre.
    pub(crate) fn fill_lighting_origins(&mut self, world: NodeId) -> Result<()> {
        let dpvs = self.st.sub(world, "dpvs")?;
        let Some(insts) = leaf_of(self.st, self.st.child(dpvs, "smodelInsts")) else { return Ok(()) };
        let lf = self.st.leaf_mut(insts);
        let count = lf.raw.len() / 36;
        let mut filled = 0;
        for i in 0..count {
            if f32_at(lf, 36 * i + 24)? != 0.0 || f32_at(lf, 36 * i + 28)? != 0.0 || f32_at(lf, 36 * i + 32)? != 0.0 {
                continue;
            }
            lf.raw.copy_within(36 * i..36 * i + 12, 36 * i + 24);
            filled += 1;
        }
        if filled != 0 {
            self.say(&format!("  static model lighting origins set to their box centres: {filled} of {count}"))?;
        }
        Ok(())
    }

    /// Each room's culling tree keeps its nodes and the models each lists, laid out as
    /// stock 360 maps have them: the root's list holds every model of the room depth first
    /// (a node's own, then each child's in turn), and every other node's list is the run of
    /// it that is its subtree. The 360 renumbers only the root lists after it reorders the
    /// models on load.
    pub(crate) fn slice_tree_lists(&mut self, world: NodeId) -> Result<()> {
        let Some(trees) = array_of(self.st, self.st.child(world, "aabbTrees")) else { return Ok(()) };
        let mut rooms = 0;
        let mut laid = 0;
        for ctn in self.st.array(trees).items.clone() {
            let ct = ctn.filter(|&c| self.st.is_struct(Some(c)));
            let node_array = ct.and_then(|ct| array_of(self.st, self.st.child(ct, "aabbTree")));
            let Some(node_array) = node_array.filter(|&a| !self.st.array(a).items.is_empty()) else { continue };
            let mut nodes = Vec::new();
            for n in self.st.array(node_array).items.clone() {
                nodes.push(struct_at(self.st, n)?);
            }
            let mut lists: Vec<Vec<u32>> = Vec::new();
            let mut readable = true;
            for &nd in &nodes {
                let mut c = self.st.child(nd, "smodelIndexes");
                let mut off = 0usize;
                if let Some(r) = self.st.as_ref(c) {
                    off = r.rel as usize;
                    c = r.target;
                }
                let n = self.st.int(nd, "smodelIndexCount")? as usize;
                let lf = self.st.as_leaf(c);
                if n == 0 {
                    lists.push(Vec::new());
                } else if let Some(lf) = lf
                    && off.wrapping_add(n.wrapping_mul(2)) <= lf.raw.len()
                {
                    lists.push(words16(lf, n, off)?);
                } else {
                    readable = false;
                }
            }
            if !readable || lists[0].is_empty() {
                continue;
            }
            let mut kids: Vec<Vec<usize>> = Vec::new();
            for (i, &nd) in nodes.iter().enumerate() {
                let f = (i as i64).wrapping_add(floor_div(self.st.int(nd, "childrenOffset")?, 40));
                let child_count = self.st.int(nd, "childCount")?;
                kids.push((0..child_count.max(0)).map(|c| f.wrapping_add(c) as usize).collect());
            }
            let mut sub: BTreeMap<usize, BTreeSet<u32>> = BTreeMap::new();
            match subtree(&lists, &kids, &mut sub, 0, 0) {
                Ok(()) => {}
                Err(e) if e.is_error() => {
                    self.warn("a culling tree's model lists couldn't be laid out (tree not as expected)")?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            // Only when each node lists exactly its subtree and no model sits under two
            // children does every node get one run of the root list.
            let empty = BTreeSet::new();
            let mut ok = true;
            for (&i, s) in &sub {
                ok &= lists[i].iter().copied().collect::<BTreeSet<u32>>() == *s;
                let mut seen: BTreeSet<u32> = BTreeSet::new();
                for k in &kids[i] {
                    let below = sub.get(k).unwrap_or(&empty);
                    for m in below {
                        ok &= !seen.contains(m);
                    }
                    seen.extend(below.iter().copied());
                }
            }
            if !ok || lists[0].len() != sub.get(&0).map_or(0, BTreeSet::len) {
                self.warn(
                    "a culling tree's model lists weren't laid out (a model listed twice, or a node not listing \
                     what is below it)",
                )?;
                continue;
            }
            let mut order: Vec<u32> = Vec::new();
            let mut span: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
            lay(&lists, &kids, &sub, 0, &mut order, &mut span);
            let base = leaf_of(self.st, self.st.child(nodes[0], "smodelIndexes")).ok_or_else(not_as_expected)?;
            let (t, big) = {
                let b = self.st.leaf(base);
                (b.t, b.big)
            };
            let root_list = self.st.make_leaf(t, order.len() as i64, pack16(&order, big), big);
            for (&i, &(start, n)) in &span {
                let nd = nodes[i];
                self.st.set(nd, "smodelIndexCount", n as i64)?;
                if i == 0 {
                    self.st.set_child(nd, "smodelIndexes", Some(root_list));
                    self.st.set_ptr(nd, "smodelIndexes", FOLLOWING)?;
                } else if n != 0 {
                    let r = self.st.make_ref(1); // non-null until the writer knows where the root list lands
                    let rf = self.st.reference_mut(r);
                    rf.target = Some(root_list);
                    rf.rel = 2 * start as i64;
                    rf.t = ElemType { t, size: 0 };
                    self.st.set_child(nd, "smodelIndexes", Some(r));
                    self.st.set_ptr(nd, "smodelIndexes", 1)?;
                } else {
                    self.st.pop_child(nd, "smodelIndexes");
                    self.st.set_ptr(nd, "smodelIndexes", 0)?;
                }
            }
            rooms += 1;
            laid += span.len();
        }
        if rooms != 0 {
            self.say(&format!(
                "  culling tree model lists laid out inside each room's list ({rooms} rooms, {laid} nodes)"
            ))?;
        }
        Ok(())
    }

    /// When every reflection probe is the same picture (the map was compiled without
    /// probes), their colour is scaled to stock 360 probes' average level.
    pub(crate) fn dim_standin_probes(&mut self, world: NodeId) -> Result<()> {
        let draw = self.st.sub(world, "draw")?;
        let probes = target(self.st, self.st.child(draw, "reflectionProbes"));
        let Some(probes) = self.st.as_ptr_list(probes) else { return Ok(()) };
        let mut leaves: Vec<NodeId> = Vec::new();
        for &c in &probes.items {
            let mut im = target(self.st, c);
            if let Some(s) = self.st.as_slot(im) {
                im = s.asset;
            }
            let ims = im.filter(|&i| self.st.is_struct(Some(i)));
            let px = ims.and_then(|i| leaf_of(self.st, self.st.child(i, "pixels")));
            let Some(px) = px else { return Ok(()) };
            let raw = &self.st.leaf(px).raw;
            if raw.is_empty() || !raw.len().is_multiple_of(4) {
                return Ok(());
            }
            leaves.push(px);
        }
        if leaves.len() < 2 {
            return Ok(());
        }
        for &l in &leaves[1..] {
            if self.st.leaf(l).raw != self.st.leaf(leaves[0]).raw {
                return Ok(());
            }
        }
        let raw = self.st.leaf(leaves[0]).raw.clone(); // A8R8G8B8: alpha first, then the colour
        let n = raw.len() / 4;
        let mut sum: u64 = 0;
        for px in raw.chunks_exact(4) {
            sum += u64::from(px[1]) + u64::from(px[2]) + u64::from(px[3]);
        }
        let level = sum as f64 / (3.0 * n as f64);
        if level <= f64::from(PROBE_STOCK_LEVEL) {
            return Ok(());
        }
        let k = f64::from(PROBE_STOCK_LEVEL) / level;
        let mut lut = [0u8; 256];
        for (v, l) in lut.iter_mut().enumerate() {
            *l = 255.min(truncate32(v as f64 * k + 0.5)) as u8;
        }
        let mut out = raw.clone();
        for (o, r) in out.chunks_exact_mut(4).zip(raw.chunks_exact(4)) {
            for c in 1..4 {
                o[c] = lut[usize::from(r[c])];
            }
        }
        for &l in &leaves {
            self.st.leaf_mut(l).raw = out.clone();
        }
        let line = format!(
            "  {} stand-in reflection probes (the map has no real ones) dimmed to stock brightness (color level {} -> \
             {PROBE_STOCK_LEVEL})",
            leaves.len(),
            fixed(level, 0)
        );
        self.say(&fitted(line, 200))
    }

    /// The world's material memory list as every stock 360 map has it: one entry for every
    /// material its surfaces draw with, sorted by name, each with 44 bytes a vertex of its
    /// vertex runs + 6 a triangle + 40 a surface. The PC counts otherwise, and leaves out the
    /// composite materials made here.
    pub(crate) fn world_material_memory(&mut self, world: NodeId) -> Result<()> {
        let list = array_of(self.st, self.st.child(world, "materialMemory"));
        let dpvs = self.st.sub(world, "dpvs")?;
        let surfs = array_of(self.st, self.st.child(dpvs, "surfaces"));
        let (Some(list), Some(surfs)) = (list, surfs) else { return Ok(()) };
        let Some(&Some(tmpl)) = self.st.array(list).items.first() else { return Ok(()) };
        if !self.st.is_struct(Some(tmpl)) {
            return Ok(());
        }
        // The PC's own entries are kept (re-ordered, with the 360's count): the list is
        // written before the surfaces, so it may hold a material itself. Materials it lacks
        // are pointed at through the asset list.
        let mut listed: HashMap<NodeId, NodeId> = HashMap::new();
        for n in self.st.array(list).items.clone() {
            let Some(e) = n.filter(|&e| self.st.is_struct(Some(e))) else { continue };
            let c = self.st.child(e, "material");
            let m = if self.st.is_struct(c) {
                c
            } else if c.is_some() {
                deref(self.st, c)?
            } else {
                None
            };
            if let Some(m) = m.filter(|&m| self.st.is_struct(Some(m))) {
                listed.entry(m).or_insert(e);
            }
        }
        let mut per: Ordered<NodeId, Use> = Ordered::new();
        let items = self.st.array(surfs).items.clone();
        let surface_count = self.st.int(world, "surfaceCount")?;
        let count = if surface_count != 0 { surface_count } else { items.len() as i64 };
        for &item in items.iter().take(count as usize) {
            let sf = item.filter(|&s| self.st.is_struct(Some(s)));
            let c = sf.and_then(|s| self.st.child(s, "material"));
            let m = if c.is_some() { deref_struct(self.st, c)? } else { None };
            let (Some(sf), Some(m), Some(c)) = (sf, m, c) else { return Ok(()) };
            if !self.st.is_ref(Some(c)) {
                return Ok(()); // (a surface whose material isn't pointed at as usual: list kept)
            }
            let own = asset_name(self.st, m);
            let fallback = name(self.st, m).unwrap_or_default();
            let u = per.find_or_add(
                m,
                Use { name: own.unwrap_or(fallback), reference: c, runs: BTreeSet::new(), tris: 0, surfs: 0 },
            );
            let t = self.st.sub(sf, "tris")?;
            u.runs.insert((self.st.int(t, "firstVertex")?, self.st.int(t, "vertexCount")?));
            u.tris = u.tris.wrapping_add(self.st.int(t, "triCount")?);
            u.surfs += 1;
        }
        let mut sorted: Vec<(NodeId, &Use)> = per.iter().map(|(&m, u)| (m, u)).collect();
        sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        let mut made: Vec<Option<NodeId>> = Vec::new();
        for (m, u) in sorted {
            let entry = if let Some(&old) = listed.get(&m) {
                self.st.copy(old)?
            } else if let Some(rt) = Some(self.st.reference(u.reference).target).filter(|&t| self.st.is_entry(t)) {
                let (rel, t) = {
                    let r = self.st.reference(u.reference);
                    (r.rel, r.t)
                };
                let r = self.st.make_ref(1);
                let rf = self.st.reference_mut(r);
                rf.target = rt;
                rf.rel = rel;
                rf.t = t;
                let entry = self.st.copy(tmpl)?;
                let ch = self.st.make_children();
                self.st.children_mut(ch).set("material", Some(r), &[]);
                self.st.st_mut(entry).set_ch(Some(ch));
                self.st.set_ptr(entry, "material", 1)?; // (an alias: a non-null placeholder)
                entry
            } else {
                return self.warn(&format!(
                    "world material list left as the PC file has it (material {} isn't pointed at as expected)",
                    u.name
                ));
            };
            let vertices = u.runs.iter().fold(0i64, |v, r| v.wrapping_add(r.1));
            let memory =
                44i64.wrapping_mul(vertices).wrapping_add(6i64.wrapping_mul(u.tris)).wrapping_add(40 * u.surfs);
            self.st.set(entry, "memory", memory)?;
            made.push(Some(entry));
        }
        let before = self.st.array(list).items.len();
        let after = made.len();
        self.st.array_mut(list).items = made;
        self.st.set(world, "materialMemoryCount", after as i64)?;
        if after != before {
            self.say(&format!(
                "  world material list: {after} materials as the 360 counts them (the PC file listed {before})"
            ))?;
        }
        Ok(())
    }

    /// rawRowData is plain bytes in the definitions but holds one row per rowDataStart entry
    /// (at 4 x that): colStart, colCount, zStart, zCount (16-bit), firstEntry (32-bit), then a
    /// byte table. The headers need the 360's byte order: as they were, every model lit from
    /// the grid read garbage rows.
    pub(crate) fn post_gfx_light_grid(&mut self, d: NodeId) -> Result<()> {
        let starts = leaf_of(self.st, self.st.child(d, "rowDataStart"));
        let raw = leaf_of(self.st, self.st.child(d, "rawRowData"));
        let (Some(starts), Some(raw)) = (starts, raw) else { return Ok(()) };
        if self.swapped_rows.contains(&raw) {
            return Ok(());
        }
        self.swapped_rows.insert(raw);
        let rows: BTreeSet<u32> = {
            let lf = self.st.leaf(starts);
            words16(lf, lf.n as usize, 0)?.into_iter().collect()
        };
        let lf = self.st.leaf_mut(raw);
        let src = lf.raw.clone();
        for s in rows {
            let o = 4 * s as usize;
            if o + 12 > src.len() {
                continue;
            }
            for k in 0..4 {
                let at = o + 2 * k;
                store(&mut lf.raw[at..], 2, load(&src[at..], 2, false), true);
            }
            store(&mut lf.raw[o + 8..], 4, load(&src[o + 8..], 4, false), true);
        }
        Ok(())
    }

    /// An effect element's colour: the PC keeps it as a D3DCOLOR (B, G, R, A), the 360 as
    /// A, R, G, B.
    pub(crate) fn post_fx_elem_visual_state(&mut self, d: NodeId) -> Result<()> {
        self.st.bytes_mut(d, "color")?.reverse();
        Ok(())
    }

    /// Two 360-only numbers per glass type: stock maps always hold 0.4077 in the second; the
    /// first follows the material (and its texture scale). Taken from a stock glass type
    /// with the same material, else com_glass_clear's.
    pub(crate) fn post_fx_glass_def(&mut self, d: NodeId) -> Result<()> {
        if self.st.int_at(d, "unknown", 0)? != 0 || self.st.int_at(d, "unknown", 1)? != 0 {
            return Ok(());
        }
        let m = deref_struct(self.st, self.st.child(d, "material"))?;
        let mat_name = without_commas(&m.and_then(|m| name(self.st, m)).unwrap_or_default());
        if self.stock_glass.is_none() {
            self.stock_glass = Some(BTreeMap::new());
            for f in self.refs.clone() {
                for_each(self, Roots::Entries(&f.tree.assets), |p, n| {
                    let st = &mut *p.st;
                    if !st.is_struct(Some(n))
                        || st.find(n, "halfThickness").is_none()
                        || st.find(n, "texVecs").is_none()
                        || st.find(n, "unknown").is_none()
                    {
                        return Ok(());
                    }
                    let u = [st.int_at(n, "unknown", 0)? as u32, st.int_at(n, "unknown", 1)? as u32];
                    if u[0] == 0 && u[1] == 0 {
                        return Ok(());
                    }
                    let Some(sm) = deref_struct(st, st.child(n, "material"))? else { return Ok(()) };
                    let mn = without_commas(&name(st, sm).unwrap_or_default());
                    let key = format!("{mn}|{}", glass_key(st, n)?);
                    if let Some(glass) = p.stock_glass.as_mut() {
                        glass.entry(mn).or_insert(u);
                        glass.entry(key).or_insert(u);
                    }
                    Ok(())
                })?;
            }
        }
        let mut u: [u32; 2] = [986759600, 1053868464]; // 0.001593, 0.4077
        let key = format!("{mat_name}|{}", glass_key(self.st, d)?);
        if let Some(glass) = &self.stock_glass {
            if let Some(&found) = glass.get(&key) {
                u = found;
            } else if let Some(&found) = glass.get(&mat_name) {
                u = found;
            }
        }
        self.st.set_at(d, "unknown", i64::from(u[0]), 0)?;
        self.st.set_at(d, "unknown", i64::from(u[1]), 1)
    }

    /// firstFreePiece is really a 16-bit number (then padding): 0xFFFF reads 0x0000FFFF on
    /// the PC and 0xFFFF0000 on the 360.
    pub(crate) fn post_fx_glass_system(&mut self, d: NodeId) -> Result<()> {
        let v = self.st.u(d, "firstFreePiece")?;
        if v <= 0xFFFF {
            self.st.set(d, "firstFreePiece", (v << 16) as i64)?;
        }
        Ok(())
    }

    /// IW4x's ZoneBuilder leaves the collision checksum 0 but writes 0xDEADBEEF into the
    /// world's: stock maps have the same number in both.
    pub(crate) fn post_clip_map(&mut self, d: NodeId) -> Result<()> {
        if self.st.int(d, "checksum")? == 0 {
            self.st.set(d, "checksum", self.world_checksum)?;
        }
        Ok(())
    }

    /// Single-player actors (IW4x maps often keep one) have no spawn code in multiplayer and
    /// name models this file doesn't carry: left out.
    pub(crate) fn post_map_ents(&mut self, d: NodeId) -> Result<()> {
        let c = self.st.child(d, "entityString");
        let Some(lf) = self.st.as_leaf(c) else { return Ok(()) };
        let mut text = lf.raw.clone();
        while text.last() == Some(&0) {
            text.pop();
        }
        let mut kept: Vec<&[u8]> = Vec::new();
        let mut dropped = 0;
        for (start, end) in entities(&text) {
            let ent = &text[start..end];
            if classname(ent).is_some_and(|c| c.starts_with(b"actor_")) {
                dropped += 1;
                continue;
            }
            kept.push(ent);
        }
        if dropped == 0 {
            return Ok(());
        }
        let mut out = kept.join(&b'\n');
        out.push(b'\n');
        out.push(0);
        self.warn(&format!("left out {dropped} single-player actor entities"))?;
        let lf = self.st.as_leaf_mut(c).ok_or_else(not_as_expected)?;
        lf.n = out.len() as i64;
        lf.raw = out;
        let n = lf.n;
        self.st.set(d, "numEntityChars", n)
    }

    // ---- models -------------------------------------------------------------------------------

    /// The PC's buffer handle means nothing here; the 360 builds its vertex and index buffers
    /// from the surface's data when the file loads.
    pub(crate) fn post_x_surface(&mut self, d: NodeId) -> Result<()> {
        self.st.set(d, "zoneHandle", 0)?;
        self.st.set(d, "unknown", 0)?;
        for k in ["vertexBuffer", "indexBuffer"] {
            self.st.bytes_mut(d, k)?.fill(0);
        }
        Ok(())
    }

    pub(crate) fn post_x_model(&mut self, d: NodeId) -> Result<()> {
        // The 360 keeps a number per surface for texture streaming (himipRadii), read for
        // every model when the file loads.
        let n = self.st.int(d, "numsurfs")?;
        self.st.set_ptr(d, "himipRadii", if n != 0 { FOLLOWING } else { 0 })?;
        if n != 0 {
            if !(0..=1 << 30).contains(&n) {
                return Err(not_as_expected());
            }
            let m = self.st.m(d, "himipRadii")?;
            let t = self.st.st(d).ty.of(m);
            let raw = pack16(&vec![HIMIP_RADIUS; n as usize], true);
            let leaf = self.st.make_leaf(Some(t), n, raw, true);
            self.st.set_child(d, &m.name, Some(leaf));
        }
        // Every model in the stock 360 files has lodRampType 0; the PC marks skinned ones 1.
        if self.st.int(d, "lodRampType")? != 0 {
            self.st.set(d, "lodRampType", 0)?;
        }
        self.fix_bone_bounds(d)?;
        // Each detail level's partBits and surfs: stock 360 models always hold 0 and null
        // (the game fills them in when it loads the model).
        let lods = self.st.m(d, "lodInfo")?;
        let count = lods.mods.first().map_or(1, |&c| c as usize);
        for k in 0..count {
            let li = self.st.sub_m(d, lods, k as i64)?;
            self.st.bytes_mut(li, "partBits")?.fill(0);
            let c = self.st.child(li, "surfs");
            if c.is_none() || self.st.is_ref(c) {
                self.st.pop_child(li, "surfs");
                self.st.set_ptr(li, "surfs", 0)?;
            }
        }
        Ok(())
    }

    /// A single-bone model's XBoneInfo rebuilt from its vertices when it is broken (a
    /// negative half-size, a box that misses vertices, or a radius squared that isn't the
    /// half-diagonal's), as stock 360 models have it.
    pub(crate) fn fix_bone_bounds(&mut self, d: NodeId) -> Result<()> {
        if self.st.int(d, "numBones")? != 1 {
            return Ok(());
        }
        let Some(info) = leaf_of(self.st, self.st.child(d, "boneInfo")) else { return Ok(()) };
        if self.st.leaf(info).raw.len() < 28 {
            return Ok(());
        }
        let mut pts: Vec<[f64; 3]> = Vec::new();
        let lods = self.st.m(d, "lodInfo")?;
        let count = lods.mods.first().map_or(1, |&c| c as usize).min(self.st.int(d, "numLods")?.max(0) as usize);
        for k in 0..count {
            let li = self.st.sub_m(d, lods, k as i64)?;
            let mut ms = target(self.st, self.st.child(li, "modelSurfs"));
            if let Some(s) = self.st.as_slot(ms) {
                ms = s.asset;
            }
            let mss = ms.filter(|&m| self.st.is_struct(Some(m)));
            let Some(surfs) = mss.and_then(|m| array_of(self.st, self.st.child(m, "surfs"))) else { continue };
            for &sf in &self.st.array(surfs).items {
                let s = sf.filter(|&s| self.st.is_struct(Some(s)));
                let Some(v) = s.and_then(|s| leaf_of(self.st, self.st.child(s, "verts0"))) else { continue };
                let v = self.st.leaf(v);
                if v.raw.is_empty() {
                    continue;
                }
                let size = v.t.map_or(0, |t| t.size() as usize);
                let size = if size != 0 { size } else { 32 };
                for i in 0..v.raw.len() / size {
                    pts.push([f32_at(v, size * i)?, f32_at(v, size * i + 4)?, f32_at(v, size * i + 8)?]);
                }
            }
        }
        if pts.is_empty() {
            return Ok(());
        }
        let mut lo = [0.0f64; 3];
        let mut hi = [0.0f64; 3];
        let mut mid = [0.0f64; 3];
        let mut half = [0.0f64; 3];
        for k in 0..3 {
            lo[k] = pts[0][k];
            hi[k] = pts[0][k];
            for p in &pts {
                lo[k] = min_f(lo[k], p[k]);
                hi[k] = max_f(hi[k], p[k]);
            }
            mid[k] = (lo[k] + hi[k]) / 2.0;
            half[k] = (hi[k] - lo[k]) / 2.0;
        }
        let lf = self.st.leaf_mut(info);
        let mut old = [0.0f64; 7];
        for (k, o) in old.iter_mut().enumerate() {
            *o = f32_at(lf, 4 * k)?;
        }
        let mut broken = false;
        for x in old {
            broken |= !x.is_finite();
        }
        for k in 0..3 {
            broken |= old[3 + k] < 0.0 || old[k] - old[3 + k] > lo[k] + 0.5 || old[k] + old[3 + k] < hi[k] - 0.5;
        }
        let r2 = old[3] * old[3] + old[4] * old[4] + old[5] * old[5];
        broken |= (r2 - old[6]).abs() > max_f(1.0, 0.01 * old[6]);
        if !broken {
            return Ok(());
        }
        let values = [
            mid[0],
            mid[1],
            mid[2],
            half[0],
            half[1],
            half[2],
            half[0] * half[0] + half[1] * half[1] + half[2] * half[2],
        ];
        let big = lf.big;
        for (k, v) in values.into_iter().enumerate() {
            store_f32(&mut lf.raw[4 * k..], v as f32, big);
        }
        self.bone_bounds += 1;
        Ok(())
    }
}

/// 16-bit numbers (indices) as plain data in a byte order.
pub fn pack16(v: &[u32], big: bool) -> Vec<u8> {
    let mut raw = vec![0u8; 2 * v.len()];
    for (i, &x) in v.iter().enumerate() {
        store(&mut raw[2 * i..], 2, u64::from(x), big);
    }
    raw
}
