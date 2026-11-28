// The generic part of the conversion: every PC struct carried over to the 360's struct of
// the same name, member by member, with the hooks of porter.rs on the structs that need
// more.

use crate::error::{Error, Result};
use crate::num::{load, load_f32, store, store_f32};
use crate::porter::{Porter, port_error};
use crate::tree::{ChildKey, Node, NodeId, Ref, Store, Ty, intern};
use mw2_zone::defs::{Kind, Member, POINTER};
use mw2_zone::schema::Schema;
use std::collections::{BTreeSet, HashMap};

/// How many pointers a pointer member holds: one (T* x, T (*x)[n]), or one an element of
/// an array of them (T* x[n]).
fn pointer_count(m: &Member) -> usize {
    let mut n: usize = 1;
    for &x in &m.mods {
        if x == POINTER {
            break;
        }
        n = n.wrapping_mul(x as usize);
    }
    n
}

/// A member's name in a tree, "_<index>" for an unnamed one. It outlives the conversion
/// (a struct's children and late parts keep it): the rules' own name, or one kept for good.
fn key(t: Ty, m: &'static Member) -> &'static str {
    if !m.name.is_empty() {
        return &m.name;
    }
    let members = t.members();
    let index = match members.iter().position(|x| std::ptr::eq(x, m)) {
        Some(k) => k as isize,
        // (A member of another type: its distance from this one's members.)
        None => {
            (m as *const Member as isize).wrapping_sub(members.as_ptr() as isize)
                / std::mem::size_of::<Member>() as isize
        }
    };
    intern(&format!("_{index}"))
}

fn member_named(t: Ty, k: &str) -> Option<&'static Member> {
    t.members().iter().find(|&m| key(t, m) == k)
}

fn is_struct(t: Ty) -> bool {
    t.unwrapped().is_record()
}

/// The element size when a member (of type `owner`) is made only of numbers (or pointers)
/// of one size, else 0.
fn uniform(owner: Ty, m: &Member) -> u32 {
    if m.has_pointer() {
        return 4;
    }
    let t = owner.of(m);
    if matches!(t.kind(), Kind::Prim | Kind::Enum) {
        return if m.bits >= 0 { 0 } else { t.size() };
    }
    if !t.compound() {
        return 0;
    }
    let mut sizes = BTreeSet::new();
    for x in t.members() {
        let u = uniform(t, x);
        if u == 0 {
            return 0;
        }
        sizes.insert(u);
    }
    if sizes.len() == 1 { sizes.into_iter().next().unwrap_or(0) } else { 0 }
}

/// Whether a compound type holds a pointer, in its members or theirs.
fn holds_pointers(t: Ty) -> bool {
    t.members().iter().any(|y| y.has_pointer() || (t.of(y).compound() && holds_pointers(t.of(y))))
}

fn swap_words(raw: &[u8], size: u32) -> Vec<u8> {
    let mut b = raw.to_vec();
    if size <= 1 {
        return b;
    }
    let size = size as usize;
    let mut i = 0;
    while i + size <= b.len() {
        b[i..i + size].reverse();
        i += size;
    }
    b
}

/// The unions whose members are separate bytes on both platforms (the "packed" number is
/// only a way to copy them): their bytes stay.
fn byte_union(name: &str) -> bool {
    name == "GfxSurfaceLightingAndFlags"
}

/// `len` bytes of a struct's data from `off`.
fn read(st: &Store, n: NodeId, off: usize, len: usize) -> Result<Vec<u8>> {
    st.data_from(n, off, len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::new(format!("{} is read past its bytes", st.st(n).ty.name())))
}

/// A struct's data from `off` on, `len` bytes of it to write.
fn written(st: &mut Store, n: NodeId, off: usize, len: usize) -> Result<&mut [u8]> {
    let name = st.st(n).ty.name();
    st.data_mut(n).get_mut(off..off + len).ok_or_else(|| Error::new(format!("{name} is written past its bytes")))
}

/// The pairs of flat elements (PC, 360) of two arrays' elements, the leading dimension as
/// far as both have it.
fn element_pairs(dp: &[i64], dx: &[i64]) -> Vec<(usize, usize)> {
    fn walk(dp: &[i64], dx: &[i64], depth: usize, fp: usize, fx: usize, pairs: &mut Vec<(usize, usize)>) {
        if depth == dp.len().max(dx.len()) {
            pairs.push((fp, fx));
            return;
        }
        let np = if depth < dp.len() { dp[depth] as usize } else { 1 };
        let nx = if depth < dx.len() { dx[depth] as usize } else { 1 };
        for k in 0..np.min(nx) {
            let p = fp.wrapping_mul(if depth < dp.len() { np } else { 1 }).wrapping_add(k);
            let x = fx.wrapping_mul(if depth < dx.len() { nx } else { 1 }).wrapping_add(k);
            walk(dp, dx, depth + 1, p, x, pairs);
        }
    }
    let mut pairs = Vec::new();
    walk(dp, dx, 0, 0, 0, &mut pairs);
    pairs
}

impl Porter<'_> {
    // ---- structs ------------------------------------------------------------------------

    pub(crate) fn convert_struct(&mut self, d: NodeId, tp: Ty, tx: Ty, track: bool) -> Result<()> {
        let tx = tx.unwrapped();
        let x = self.st.zero(tx, true);
        if track {
            if self.done.contains(&d) {
                return Ok(());
            }
            self.done.insert(d);
        }
        if self.pre(d, tp.unwrapped(), tx, x)? {
            return Ok(());
        }
        self.convert_into(d, tp.unwrapped(), x, false, track)?;
        self.st.adopt(d, x);
        if tx.kind() == Kind::Struct {
            self.post(d, tx)?;
        }
        Ok(())
    }

    /// Converts `d` into `x`, the 360 struct being built (a view of it, for a member).
    /// `member`: a member's struct, which runs its own hooks here; `track`: it is noted
    /// done (not a throwaway element of plain data).
    pub(crate) fn convert_into(&mut self, d: NodeId, tp: Ty, x: NodeId, member: bool, track: bool) -> Result<()> {
        let tx = self.st.st(x).ty;
        if member {
            if track {
                if self.done.contains(&d) {
                    return Ok(());
                }
                self.done.insert(d);
            }
            if self.pre(d, tp, tx, x)? {
                return Ok(());
            }
        }
        if tx.kind() == Kind::Union {
            self.convert_union(d, tp, x, track)?;
            self.convert_children(d, tp, tx)?;
            let ch = self.st.st(d).ch();
            self.st.st_mut(x).set_ch(ch);
            return Ok(());
        }
        if tx.kind() == Kind::Typedef {
            return Ok(());
        }
        for mx in tx.members() {
            // (A member only the 360 has: zero, or what a hook set already.)
            let Some(mp) = member_named(tp, key(tx, mx)) else { continue };
            if !self.st.has(d, key(tp, mp)) || mp.has_pointer() != mx.has_pointer() {
                continue;
            }
            self.convert_member(d, tp, mp, x, tx, mx, track)?;
        }
        self.convert_children(d, tp, tx)?;
        let (ch, asset, slot, forward) = {
            let s = self.st.st(d);
            (s.ch(), s.asset, s.slot, s.forward)
        };
        let xs = self.st.st_mut(x);
        xs.set_ch(ch);
        xs.asset = asset;
        xs.slot = slot;
        xs.forward = forward;
        if member && tx.kind() == Kind::Struct {
            self.post(x, tx)?;
        }
        Ok(())
    }

    // Member `mp` of `d` (one of type `tp`'s) into member `mx` of `x` (one of `tx`'s).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn convert_member(
        &mut self,
        d: NodeId,
        tp: Ty,
        mp: &'static Member,
        x: NodeId,
        tx: Ty,
        mx: &'static Member,
        track: bool,
    ) -> Result<()> {
        let kp = key(self.st.st(d).ty, mp);
        let kx = key(self.st.st(x).ty, mx);
        if mx.has_pointer() {
            // Pointers: their values as they are (what they point at is converted with the
            // struct's children).
            let n = pointer_count(mp).min(pointer_count(mx));
            for k in 0..n {
                let v = self.st.ptr_at(d, kp, k)?;
                self.st.set_ptr_at(x, kx, v, k)?;
            }
            return Ok(());
        }
        if mx.bits >= 0 {
            let v = self.st.int(d, kp)?;
            self.st.set(x, kx, v)?;
            return Ok(());
        }
        let mpt = tp.of(mp);
        let mxt = tx.of(mx);
        if let Some(late) = self.st.late(d, kp) {
            // Streamed after the struct's first part (never in a map's own structs):
            // converted where it is a struct, else as it is.
            if self.st.is_struct(Some(late)) {
                self.convert_struct(late, mpt, mxt, track)?;
            } else if let Some(a) = self.st.as_array(Some(late)) {
                let items = a.items.clone();
                for item in items {
                    if let Some(e) = item.filter(|&e| self.st.is_struct(Some(e))) {
                        self.convert_struct(e, mpt, mxt, track)?;
                    }
                }
            }
            self.st.set_late(x, kx, Some(late), &[]);
            return Ok(());
        }
        let dp: Vec<i64> = mp.dims().collect();
        let dx: Vec<i64> = mx.dims().collect();
        // Element k (flat) of each side's array, the leading dimension as far as both have it.
        let pairs = element_pairs(&dp, &dx);
        if is_struct(mpt) && is_struct(mxt) {
            for (fp, fxk) in pairs {
                let ps = self.st.sub_m(d, mp, if dp.is_empty() { -1 } else { fp as i64 })?;
                let xs = self.st.sub_m(x, mx, if dx.is_empty() { -1 } else { fxk as i64 })?;
                self.convert_into(ps, mpt.unwrapped(), xs, true, track)?;
            }
            return Ok(());
        }
        let (dbig, xbig) = (self.st.st(d).big, self.st.st(x).big);
        if mpt.compound() || mxt.compound() {
            // A typedef around an array of numbers (vec3_t): number for number.
            let first = |t: Ty| {
                t.members().first().map(|m| t.of(m)).ok_or_else(|| port_error(format!("{} is empty", t.name())))
            };
            let ip = if mpt.compound() { first(mpt)? } else { mpt };
            let ix = if mxt.compound() { first(mxt)? } else { mxt };
            let (ips, ixs) = (ip.size() as usize, ix.size() as usize);
            let n = mp.size.min(mx.size) / ip.size().max(1);
            for k in 0..n as usize {
                let src = mp.offset as usize + k * ips;
                let dst = mx.offset as usize + k * ixs;
                if ip.get().is_float && ip.size() == 4 {
                    let v = load_f32(&read(self.st, d, src, 4)?, dbig);
                    store_f32(written(self.st, x, dst, 4)?, v, xbig);
                } else {
                    let v = load(&read(self.st, d, src, ips)?, ip.size(), dbig);
                    store(written(self.st, x, dst, ixs)?, ix.size(), v, xbig);
                }
            }
            return Ok(());
        }
        let (sp, sx) = (mpt.size(), mxt.size());
        let signed = mpt.get().is_signed;
        for (fp, fxk) in pairs {
            let src = (mp.offset as usize).wrapping_add(fp.wrapping_mul(sp as usize));
            let dst = (mx.offset as usize).wrapping_add(fxk.wrapping_mul(sx as usize));
            if mpt.get().is_float {
                let v = if sp == 4 {
                    f64::from(load_f32(&read(self.st, d, src, 4)?, dbig))
                } else {
                    f64::from_bits(load(&read(self.st, d, src, 8)?, 8, dbig))
                };
                if v.is_nan() || v.is_infinite() {
                    // Kept as the bytes they are, not swapped: the 360 reads them as a tiny
                    // number, not as NaN or infinity.
                    let n = sp.min(sx) as usize;
                    let b = read(self.st, d, src, n)?;
                    written(self.st, x, dst, n)?.copy_from_slice(&b);
                    continue;
                }
                if sx == 4 {
                    store_f32(written(self.st, x, dst, 4)?, v as f32, xbig);
                } else {
                    store(written(self.st, x, dst, 8)?, 8, v.to_bits(), xbig);
                }
                continue;
            }
            let mut v = load(&read(self.st, d, src, sp as usize)?, sp, dbig);
            if signed && sp < 8 && (v >> (sp * 8 - 1)) & 1 != 0 {
                v |= !0u64 << (sp * 8);
            }
            store(written(self.st, x, dst, sx as usize)?, sx, v, xbig);
        }
        Ok(())
    }

    /// A union's bytes, converted as the member in use: the one a walk went into, else the
    /// only member without pointers, else (members all numbers of one size) any of them.
    pub(crate) fn convert_union(&mut self, d: NodeId, tp: Ty, x: NodeId, track: bool) -> Result<()> {
        let tx = self.st.st(x).ty;
        let n = self.st.st(d).size().min(self.st.st(x).size());
        // The union's bytes, as they are or with each word of `size` bytes swapped.
        let copy = |st: &mut Store, size: u32| -> Result<()> {
            let b = swap_words(&read(st, d, 0, n)?, size);
            written(st, x, 0, n)?.copy_from_slice(&b);
            Ok(())
        };
        if byte_union(tx.name()) {
            return copy(self.st, 1);
        }
        if tx.name() == "FxGlassGeometryData" {
            // A glass piece's vertices (two 16-bit numbers) and fan indexes; the hole and
            // crack headers only exist at run time.
            return copy(self.st, 2);
        }
        let mut names = self.st.st(d).used.clone();
        if names.is_empty() {
            let mut words = BTreeSet::new();
            let mut none = false;
            for m in tx.members() {
                let u = uniform(tx, m);
                if u == 0 {
                    none = true;
                }
                words.insert(u);
            }
            if words.len() == 1 && !none {
                let size = words.into_iter().next().unwrap_or(0);
                return copy(self.st, size);
            }
            let size = self.st.st(d).size();
            for m in tx.members() {
                let t = tx.of(m);
                if m.mods.is_empty()
                    && m.bits < 0
                    && matches!(t.kind(), Kind::Prim | Kind::Enum)
                    && t.size() as usize == size
                {
                    // A "packed" member holding the whole thing as one number.
                    return copy(self.st, t.size());
                }
            }
            for m in tx.members() {
                if !m.has_pointer() && !(tx.of(m).compound() && holds_pointers(tx.of(m))) {
                    names.push(key(tx, m));
                }
            }
            if names.len() != 1 {
                return Err(port_error(format!("don't know which member of union {} is in use", tx.name())));
            }
        }
        for mx in tx.members() {
            let k = key(tx, mx);
            if !names.contains(&k) {
                continue;
            }
            let Some(mp) = member_named(tp, k) else { continue };
            self.convert_member(d, tp, mp, x, tx, mx, track)?;
        }
        Ok(())
    }

    // ---- what pointers loaded -----------------------------------------------------------

    pub(crate) fn convert_children(&mut self, d: NodeId, tp: Ty, tx: Ty) -> Result<()> {
        let Some(ch) = self.st.st(d).ch() else { return Ok(()) };
        let items = self.st.children(ch).items.clone();
        let out = self.st.make_children();
        for (k, c) in items {
            let Some(mx) = member_named(tx, k.member) else { continue };
            // (Not one of the PC's: put there by a hook, in 360 terms already, or plain data.)
            let ctp = match member_named(tp, k.member) {
                Some(mp) => tp.of(mp),
                None => tx.of(mx),
            };
            let member = if mx.name.is_empty() { k.member } else { mx.name.as_str() };
            let converted = self.convert_child(c, ctp, tx.of(mx))?;
            self.st.children_mut(out).items.push((ChildKey { member, idx: k.idx }, converted));
        }
        self.st.st_mut(d).set_ch(Some(out));
        Ok(())
    }

    pub(crate) fn convert_child(&mut self, c: Option<NodeId>, tp: Ty, tx: Ty) -> Result<Option<NodeId>> {
        let Some(n) = c else { return Ok(None) };
        enum What {
            List(Vec<Option<NodeId>>),
            Array(Vec<Option<NodeId>>),
            Asset,
            Struct,
            Leaf,
            Other,
        }
        let what = match self.st.node(n) {
            Node::PtrList(list) => What::List(list.items.clone()),
            Node::Array(a) => What::Array(a.items.clone()),
            Node::Struct(s) if s.is_asset() => What::Asset,
            Node::Struct(_) => What::Struct,
            Node::Leaf(_) => What::Leaf,
            _ => What::Other,
        };
        match what {
            What::List(items) => {
                for (k, item) in items.into_iter().enumerate() {
                    if item.is_some() {
                        let converted = self.convert_child(item, tp, tx)?;
                        if let Some(slot) = self.st.ptr_list_mut(n).items.get_mut(k) {
                            *slot = converted;
                        }
                    }
                }
            }
            What::Array(items) => {
                for item in items {
                    if let Some(s) = item.filter(|&s| self.st.is_struct(Some(s))) {
                        self.convert_struct(s, tp, tx, true)?;
                    }
                }
            }
            What::Asset => self.convert_asset(n)?,
            What::Struct => self.convert_struct(n, tp, tx, true)?,
            What::Leaf => return self.convert_leaf(n, tx).map(Some),
            What::Other => {}
        }
        Ok(c)
    }

    /// Plain data, in place (pointers may refer to it).
    pub(crate) fn convert_leaf(&mut self, lf: NodeId, tx: Ty) -> Result<NodeId> {
        if self.st.leaf(lf).big {
            return Ok(lf);
        }
        if tx.name() == "GfxWorldVertex" || tx.name() == "GfxPackedVertex" {
            // Floats to big-endian; colour and (for model vertices) packed texture
            // coordinates as the same 32-bit value; normal and tangent repacked from the
            // PC's 8-bit + scale to the 360's 10:10:10.
            let world = tx.name() == "GfxWorldVertex";
            let size = if world { 44 } else { 32 };
            let raw = std::mem::take(&mut self.st.leaf_mut(lf).raw);
            let mut out = vec![0u8; raw.len()];
            let mut cache: HashMap<u32, u32> = HashMap::new();
            let mut unit = |b: &[u8]| -> u32 {
                let key = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                if let Some(&u) = cache.get(&key) {
                    return u;
                }
                let s = (i32::from(b[3]) + 192) as f64 / 32385.0;
                let mut u: u32 = 0;
                for (k, &c) in b[..3].iter().enumerate() {
                    let v = (f64::from(i32::from(c) - 127) * s).clamp(-1.0, 1.0) * 511.0;
                    let x = v.round_ties_even() as i32; // half to even
                    u |= (x as u32 & 0x3FF) << (10 * k);
                }
                cache.insert(key, u);
                u
            };
            let mut o = 0;
            while o + size <= raw.len() {
                let src = &raw[o..o + size];
                let dst = &mut out[o..o + size];
                for k in 0..4 {
                    store_f32(&mut dst[4 * k..], load_f32(&src[4 * k..], false), true);
                }
                dst[16..20].copy_from_slice(&src[16..20]);
                dst[16..20].reverse();
                if world {
                    for k in 0..4 {
                        store_f32(&mut dst[20 + 4 * k..], load_f32(&src[20 + 4 * k..], false), true);
                    }
                    for k in [36, 40] {
                        store(&mut dst[k..], 4, u64::from(unit(&src[k..])), true);
                    }
                } else {
                    dst[20..24].copy_from_slice(&src[20..24]);
                    dst[20..24].reverse();
                    for k in [24, 28] {
                        store(&mut dst[k..], 4, u64::from(unit(&src[k..])), true);
                    }
                }
                o += size;
            }
            let leaf = self.st.leaf_mut(lf);
            leaf.raw = out;
            leaf.big = true;
            return Ok(lf);
        }
        let Some(tp) = self.st.leaf(lf).t else {
            return Err(port_error(format!("can't convert plain data for {}", tx.name())));
        };
        if matches!(tp.kind(), Kind::Prim | Kind::Enum) {
            if tp.size() != tx.size() {
                return Err(port_error(format!("{} changes size", tp.name())));
            }
            let leaf = self.st.leaf_mut(lf);
            leaf.raw = swap_words(&leaf.raw, tp.size());
            leaf.t = Some(tx);
            leaf.big = true;
            return Ok(lf);
        }
        if !tp.compound() {
            return Err(port_error(format!("can't convert {}", tp.name())));
        }
        let (n, raw) = {
            let leaf = self.st.leaf(lf);
            (leaf.n, leaf.raw.clone())
        };
        if n < 0 {
            return Err(port_error(format!("{n} elements of {}", tp.name())));
        }
        let (tps, txs) = (tp.size() as usize, tx.size() as usize);
        let mut out = vec![0u8; txs * n as usize];
        for i in 0..n as usize {
            let bytes = raw
                .get(i * tps..(i + 1) * tps)
                .ok_or_else(|| Error::new(format!("plain data of {} ends early", tp.name())))?
                .to_vec();
            let e = self.st.make_struct(tp, false, bytes);
            self.convert_struct(e, tp, tx, false)?;
            let data = self.st.data(e);
            let len = data.len().min(txs);
            out[i * txs..i * txs + len].copy_from_slice(&data[..len]);
        }
        let leaf = self.st.leaf_mut(lf);
        leaf.raw = out;
        leaf.t = Some(tx);
        leaf.big = true;
        Ok(lf)
    }

    pub(crate) fn convert_asset(&mut self, d: NodeId) -> Result<()> {
        if self.done.contains(&d) {
            return Ok(());
        }
        let name = self.st.st(d).asset;
        let (Some(&p), Some(&x)) = (self.p.infos.get(name), self.x.infos.get(name)) else {
            return Err(port_error(format!("the 360 has no {name} assets")));
        };
        let tp = Ty::new(self.p, self.p.structs[p].ctype);
        let tx = Ty::new(self.x, self.x.structs[x].ctype);
        self.convert_struct(d, tp, tx, true)
    }

    // ---- hooks --------------------------------------------------------------------------

    /// The hook on a struct before its conversion; it may handle the struct itself,
    /// returning true.
    pub(crate) fn pre(&mut self, d: NodeId, tp: Ty, tx: Ty, x: NodeId) -> Result<bool> {
        let _ = tp;
        match tx.name() {
            "GfxWorldDpvsDynamic" => self.pre_gfx_world_dpvs_dynamic(d, x),
            "MaterialTechniqueSet" => self.pre_material_technique_set(d),
            "GfxStaticModelDrawInst" => self.pre_gfx_static_model_draw_inst(d, x),
            "water_t" => self.pre_water_t(d, x),
            "GfxImage" => self.pre_gfx_image(d),
            "Material" => self.pre_material(d),
            _ => Ok(false),
        }
    }

    /// The hook on a struct after its conversion.
    pub(crate) fn post(&mut self, d: NodeId, tx: Ty) -> Result<()> {
        match tx.name() {
            "GfxAabbTree" => self.post_gfx_aabb_tree(d),
            "GfxWorldDpvsStatic" => self.post_gfx_world_dpvs_static(d),
            "GfxWorldDraw" => self.post_gfx_world_draw(d),
            "GfxWorld" => self.post_gfx_world(d),
            "GfxLightGrid" => self.post_gfx_light_grid(d),
            "FxElemVisualState" => self.post_fx_elem_visual_state(d),
            "FxGlassDef" => self.post_fx_glass_def(d),
            "FxGlassSystem" => self.post_fx_glass_system(d),
            "clipMap_t" => self.post_clip_map(d),
            "MapEnts" => self.post_map_ents(d),
            "XSurface" => self.post_x_surface(d),
            "XModel" => self.post_x_model(d),
            "Material" => self.post_material(d),
            _ => Ok(()),
        }
    }
}

// ---- pointers into objects of another layout -------------------------------------------------

fn member_offset(tp: Ty, tx: Ty, rem: i64) -> Result<i64> {
    if rem == 0 {
        return Ok(0);
    }
    for m in tp.members() {
        if !(i64::from(m.offset) <= rem && rem < i64::from(m.offset.wrapping_add(m.size))) {
            continue;
        }
        let Some(mx) = tx.members().iter().rev().find(|c| c.name == m.name) else {
            return Err(port_error(format!(
                "a pointer points at {}.{}, which the 360 doesn't have",
                tp.name(),
                m.name
            )));
        };
        let inner = rem - i64::from(m.offset);
        if tp.of(m).compound() && m.dims().next().is_none() {
            return Ok(i64::from(mx.offset) + member_offset(tp.of(m), tx.of(mx), inner)?);
        }
        return Ok(i64::from(mx.offset) + inner);
    }
    Err(port_error(format!("a pointer points into padding of {}", tp.name())))
}

/// The pointer value a Ref into an object of another layout keeps once converted: its
/// target's member by name.
pub fn map_rel(r: &Ref, x: &'static Schema) -> Result<i64> {
    let Some(t) = r.t.t.filter(|t| t.compound()) else { return Ok(r.rel) };
    let found = if t.name().is_empty() { None } else { Ty::named(x, t.name()) };
    let Some(tx) = found else { return Ok(r.rel) };
    let same = tx.size() == t.size()
        && tx.members().len() == t.members().len()
        && tx.members().iter().zip(t.members()).all(|(a, b)| a.offset == b.offset);
    if same {
        return Ok(r.rel);
    }
    if t.size() == 0 {
        return Err(port_error(format!("a pointer points into {}, which has no size", t.name())));
    }
    let size = i64::from(t.size());
    let (i, rem) = (r.rel / size, r.rel % size);
    Ok(i.wrapping_mul(i64::from(tx.size())).wrapping_add(member_offset(t, tx, rem)?))
}
