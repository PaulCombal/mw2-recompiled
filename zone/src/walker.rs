// Walks every asset of a decompressed 360 zone in the order the game's loader streams
// it, so each byte of the zone is reached as what it is.
//
// The loader also keeps a position in each memory block: a pointer back to earlier data
// is that block and position, plus one. They are kept here the way the loader keeps
// them, so a visitor can tell what such a pointer names. A walk that ends anywhere but
// at the zone's last byte, or with a block not at the size the zone's header gives it,
// has misread something, and says so.
//
// Sizes and offsets that come from the file are computed so that one too large for
// memory is an Error, never a wrapped number or a panic.

use std::cell::RefCell;
use std::rc::Rc;

use crate::defs::{Kind, POINTER, Type};
use crate::schema::{BlockId, ExprId, ExprRef, MemberId, Schema, StructId};
use crate::{Error, Result};

/// A pointer that says its data follows in the stream, and one that says the loader
/// makes a pointer to it in the virtual block.
pub const FOLLOWING: u32 = 0xFFFF_FFFF;
const INSERT: u32 = 0xFFFF_FFFE;

const TEMP: usize = 0;
const PHYSICAL: usize = 1;
const RUNTIME: usize = 2;
const VIRTUAL: usize = 3;
const LARGE: usize = 4;
const CALLBACK: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    Dynamic,
    Single,
    Array,
    PtrArray,
    EmbArray,
    Embedded,
}

/// The title update's asset types (DB_GetXAssetTypeName) and the rules' name for each;
/// the ones missing never appear in a 360 zone.
fn asset_struct(ty: u32) -> Option<&'static str> {
    const STRUCTS: [Option<&str>; 41] = [
        Some("AssetPhysPreset"),
        Some("AssetPhysCollMap"),
        Some("AssetXAnim"),
        Some("AssetXModelSurfs"),
        Some("AssetXModel"),
        Some("AssetMaterial"),
        Some("AssetPixelShader"),
        Some("AssetTechniqueSet"),
        Some("AssetImage"),
        Some("AssetSound"),
        Some("AssetSoundCurve"),
        Some("AssetLoadedSound"),
        Some("AssetClipMapMp"),
        Some("AssetClipMapMp"),
        Some("AssetComWorld"),
        Some("AssetGameWorldSp"),
        Some("AssetGameWorldMp"),
        Some("AssetMapEnts"),
        Some("AssetFxWorld"),
        Some("AssetGfxWorld"),
        Some("AssetLightDef"),
        None,
        Some("AssetFont"),
        Some("AssetMenuList"),
        Some("AssetMenu"),
        Some("AssetLocalize"),
        Some("AssetWeapon"),
        Some("AssetSndDriverGlobals"),
        Some("AssetFx"),
        Some("AssetImpactFx"),
        None,
        None,
        None,
        None,
        Some("AssetRawFile"),
        Some("AssetStringTable"),
        Some("AssetLeaderboard"),
        Some("AssetStructuredDataDef"),
        Some("AssetTracer"),
        Some("AssetVehicle"),
        Some("AssetAddonMapEnts"),
    ];
    STRUCTS.get(ty as usize).copied().flatten()
}

/// A float as x86-64's conversion truncates it to an integer: the "integer indefinite"
/// value, i64::MIN, for NaN and anything out of range.
fn truncate(f: f64) -> i64 {
    const LIMIT: f64 = (1u64 << 63) as f64;
    if (-LIMIT..LIMIT).contains(&f) { f as i64 } else { i64::MIN }
}

/// A primitive or enum of type `t`, big-endian.
fn read_value(p: &[u8], t: &Type) -> i64 {
    let mut v: u64 = 0;
    for &b in &p[..t.size as usize] {
        v = (v << 8) | u64::from(b);
    }
    if t.is_float {
        return if t.size == 4 { truncate(f64::from(f32::from_bits(v as u32))) } else { truncate(f64::from_bits(v)) };
    }
    if t.is_signed && t.size < 8 && (v >> (t.size * 8 - 1)) & 1 != 0 {
        v |= !0u64 << (t.size * 8);
    }
    v as i64
}

#[derive(Clone)]
enum Bytes {
    /// Of the zone: where, and how many.
    View(usize, usize),
    /// Its own, which a struct put together from pieces grows.
    Own(Rc<RefCell<Vec<u8>>>),
    /// The runtime block's: made by the loader at run time, so nothing of it is in the
    /// file. Zeros, as many as that.
    Zeros(usize),
}

/// Bytes a struct is read from.
#[derive(Clone)]
pub struct Buffer {
    bytes: Bytes,
    growable: bool,
}

impl Buffer {
    fn view(at: usize, n: usize) -> Buffer {
        Buffer { bytes: Bytes::View(at, n), growable: false }
    }

    fn own(data: Vec<u8>, growable: bool) -> Buffer {
        Buffer { bytes: Bytes::Own(Rc::new(RefCell::new(data))), growable }
    }

    fn size(&self) -> usize {
        match &self.bytes {
            Bytes::View(_, n) | Bytes::Zeros(n) => *n,
            Bytes::Own(v) => v.borrow().len(),
        }
    }

    /// `n` (at most 8) bytes at `at`, when they are inside.
    fn get(&self, zone: &[u8], at: usize, n: usize) -> Option<[u8; 8]> {
        let end = at.checked_add(n).filter(|&end| end <= self.size())?;
        let mut out = [0u8; 8];
        match &self.bytes {
            Bytes::View(start, _) => out[..n].copy_from_slice(&zone[start + at..start + end]),
            Bytes::Own(v) => out[..n].copy_from_slice(&v.borrow()[at..end]),
            Bytes::Zeros(_) => {}
        }
        Some(out)
    }
}

/// Where data was put: its memory block and the position in it, and where its bytes are
/// in the zone (none for the runtime block, made at run time).
#[derive(Clone, Copy, Default, Debug)]
pub struct Place {
    pub block: usize,
    pub block_pos: u32,
    pub zone_pos: usize,
    pub in_zone: bool,
}

impl Place {
    /// What a pointer to it holds: block << 28 | position, plus one.
    pub fn pointer(&self) -> u32 {
        ((self.block as u32) << 28 | self.block_pos).wrapping_add(1)
    }
}

/// What a walk tells as it goes. An Error from either ends the walk with it.
pub trait Visitor {
    /// The data of every leaf array or single leaf a pointer streams: the member it is,
    /// where it went, and how many bytes.
    fn on_data(&mut self, _member: MemberId, _place: &Place, _bytes: usize) -> Result<()> {
        Ok(())
    }
    /// Each pointer as it is met, before what it names loads: the member, the element of
    /// a pointer array (-1 for a single one) and the value -- FOLLOWING when the data
    /// follows in the stream.
    fn on_pointer(&mut self, _member: MemberId, _index: i64, _value: u32) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct Inst {
    info: StructId,
    buf: Buffer,
    off: usize,
    index: i64,
}

pub struct Walker<'a, V: Visitor> {
    s: &'a Schema,
    zone: &'a [u8],
    visitor: &'a mut V,
    pos: usize,
    block: usize,
    block_pos: [u32; 6],
    /// The pushed block's position, and the block before.
    stack: Vec<(u32, usize)>,
    header_sizes: [u32; 6],
    /// Of the last read.
    last: Place,
    vars: Vec<Option<Inst>>,
    pointed: Vec<Option<Buffer>>,
    asset_type: u32,
    asset_count: u32,
    lod_info: Option<StructId>,
    surfs: Option<StructId>,
}

fn too_large(pos: usize) -> Error {
    Error::new(format!("the zone ends inside the data at {pos}"))
}

impl<'a, V: Visitor> Walker<'a, V> {
    pub fn new(schema: &'a Schema, zone: &'a [u8], visitor: &'a mut V) -> Walker<'a, V> {
        Walker {
            s: schema,
            zone,
            visitor,
            pos: 0,
            block: TEMP,
            block_pos: [0; 6],
            stack: Vec::new(),
            header_sizes: [0; 6],
            last: Place::default(),
            vars: vec![None; schema.structs.len()],
            pointed: vec![None; schema.members.len()],
            asset_type: !0,
            asset_count: 0,
            lod_info: schema.infos.get("XModelLodInfo").copied(),
            surfs: schema.infos.get("XModelSurfs").copied(),
        }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn asset_count(&self) -> u32 {
        self.asset_count
    }

    // ---- the stream ---------------------------------------------------------------

    fn read(&mut self, n: usize) -> Result<Buffer> {
        self.last = Place {
            block: self.block,
            block_pos: self.block_pos[self.block],
            zone_pos: self.pos,
            in_zone: self.block != RUNTIME,
        };
        self.block_pos[self.block] = self.block_pos[self.block].wrapping_add(n as u32);
        if self.block == RUNTIME {
            return Ok(Buffer { bytes: Bytes::Zeros(n), growable: false });
        }
        if n > self.zone.len() - self.pos {
            return Err(too_large(self.pos));
        }
        let b = Buffer::view(self.pos, n);
        self.pos += n;
        Ok(b)
    }

    /// `count` elements of `size` bytes, read; an Error when they could not be in the
    /// zone at all.
    fn read_many(&mut self, size: usize, count: i64) -> Result<Buffer> {
        let n = usize::try_from(count).ok().and_then(|c| c.checked_mul(size)).ok_or_else(|| too_large(self.pos))?;
        self.read(n)
    }

    // The temp block is a scratch area: what was pushed onto it is gone when it is popped.
    fn push(&mut self, b: usize) {
        self.stack.push((self.block_pos[b], self.block));
        self.block = b;
    }

    fn pop(&mut self) {
        let (pos, block) = self.stack.pop().unwrap_or((self.block_pos[TEMP], TEMP));
        if self.block == TEMP {
            self.block_pos[TEMP] = pos;
        }
        self.block = block;
    }

    fn alloc(&mut self, align: u32) {
        // In 32 bits, as the loader's positions are.
        if align > 1 {
            self.block_pos[self.block] = self.block_pos[self.block].wrapping_add(align - 1) / align * align;
        }
    }

    /// A pointer the loader makes in the virtual block, for data in the temp one that
    /// something later points to.
    fn insert_pointer(&mut self) {
        self.push(VIRTUAL);
        self.alloc(4);
        self.block_pos[VIRTUAL] = self.block_pos[VIRTUAL].wrapping_add(4);
        self.pop();
    }

    fn block_of(&self, b: BlockId) -> usize {
        match self.s.blocks[b].name.as_str() {
            "XFILE_BLOCK_TEMP" => TEMP,
            "XFILE_BLOCK_RUNTIME" => RUNTIME,
            "XFILE_BLOCK_VIRTUAL" => VIRTUAL,
            "XFILE_BLOCK_LARGE" => LARGE,
            "XFILE_BLOCK_CALLBACK" => CALLBACK,
            // Vertex and index data go in the physical block on the 360.
            _ => PHYSICAL,
        }
    }

    fn block_kind(&self, b: Option<BlockId>, kind: &str) -> bool {
        b.is_some_and(|b| self.s.blocks[b].kind == kind)
    }

    fn u32_at(&self, buf: &Buffer, at: usize) -> Result<u32> {
        let p = buf
            .get(self.zone, at, 4)
            .ok_or_else(|| Error::new(format!("a pointer outside its struct at {}", self.pos)))?;
        Ok(u32::from_be_bytes([p[0], p[1], p[2], p[3]]))
    }

    /// `n` bytes of `src` from `off` added to `dst`'s own.
    fn append(&self, dst: &Buffer, src: &Buffer, off: usize, n: usize) -> Result<()> {
        let Bytes::Own(into) = &dst.bytes else { return Ok(()) };
        let short = || Error::new(format!("a struct is put together past its data at {}", self.pos));
        let end = off.checked_add(n).filter(|&e| e <= src.size()).ok_or_else(short)?;
        let piece: Vec<u8> = match &src.bytes {
            Bytes::View(start, _) => self.zone[start + off..start + end].to_vec(),
            Bytes::Own(v) => v.borrow()[off..end].to_vec(),
            Bytes::Zeros(_) => {
                let mut zeros = Vec::new();
                zeros.try_reserve_exact(n).map_err(|_| too_large(self.pos))?;
                zeros.resize(n, 0);
                zeros
            }
        };
        let mut into = into.borrow_mut();
        into.try_reserve(piece.len()).map_err(|_| too_large(self.pos))?;
        into.extend_from_slice(&piece);
        Ok(())
    }

    // ---- the rules' expressions -----------------------------------------------------

    fn field(&self, inst: &Inst, chain: &[MemberId], idx: &[i64]) -> Result<i64> {
        let mut off = inst.off;
        for &m in &chain[..chain.len() - 1] {
            off = off.wrapping_add(self.s.member(m).offset as usize);
        }
        let last = *chain.last().unwrap_or(&0);
        let m = self.s.member(last);
        let name = &self.s.members[last].name;
        off = off.wrapping_add(m.offset as usize);
        if m.is_pointer() {
            return Ok(i64::from(self.u32_at(&inst.buf, off)?));
        }
        let ty = &self.s.defs.types[m.ty];
        let dims: Vec<i64> = m.dims().collect();
        if !idx.is_empty() {
            let mut stride = i64::from(ty.size);
            for &d in dims.iter().skip(idx.len()) {
                stride = stride.wrapping_mul(d);
            }
            for k in 0..idx.len() {
                let mut sub = stride;
                for &d in dims.iter().take(idx.len()).skip(k + 1) {
                    sub = sub.wrapping_mul(d);
                }
                off = off.wrapping_add(idx[k].wrapping_mul(sub) as usize);
            }
        }
        if ty.kind != Kind::Prim && ty.kind != Kind::Enum {
            return Err(Error::new(format!("a rule reads {name}, which is not a number")));
        }
        let p = inst
            .buf
            .get(self.zone, off, ty.size as usize)
            .ok_or_else(|| Error::new(format!("a rule reads past its struct: {name}")))?;
        let mut v = read_value(&p, ty);
        if m.bits >= 0 {
            v = v.wrapping_shr(m.bitpos) & 1i64.wrapping_shl(m.bits as u32).wrapping_sub(1);
        }
        Ok(v)
    }

    fn lookup(&self, r: &ExprRef, idx: &[i64]) -> Result<i64> {
        let info_name = || &self.s.structs[r.info].name;
        let mut found = self.vars[r.info].as_ref();
        if r.is_index {
            let inst = found.ok_or_else(|| Error::new(format!("a rule needs the index of {}", info_name())))?;
            return Ok(inst.index);
        }
        // A model surfaces asset listed on its own (not reached through a model's LOD
        // entry) carries its surface count itself; an earlier model's LOD entry is stale.
        if Some(r.info) == self.lod_info
            && self.asset_type == 3
            && let Some(surfs) = self.surfs
            && let Some(own) = self.vars[surfs].as_ref()
        {
            found = Some(own);
        }
        let inst = found.ok_or_else(|| Error::new(format!("a rule needs {}, and none is loaded", info_name())))?;
        let mut owned_chain;
        let mut chain: &[MemberId] = &r.chain;
        if inst.info != r.info {
            owned_chain = Vec::new();
            let mut cur = Some(inst.info);
            for n in &r.names {
                let m = self.s.find_member(cur, n);
                let Some(&last) = m.last() else {
                    let of = cur.map_or("?", |c| self.s.structs[c].name.as_str());
                    return Err(Error::new(format!("a rule needs {n} of {of}")));
                };
                owned_chain.extend_from_slice(&m);
                cur = self.s.members[last].ty;
            }
            chain = &owned_chain;
        }
        let Some(&last) = chain.last() else {
            return Err(Error::new(format!("a rule needs a member of {}", info_name())));
        };
        let m = self.s.member(last);
        if !idx.is_empty() && m.is_pointer() {
            // Indexing through a pointer: the data loaded for it.
            let name = &self.s.members[last].name;
            let data =
                self.pointed[last].as_ref().ok_or_else(|| Error::new(format!("a rule needs the data of {name}")))?;
            let ty = &self.s.defs.types[m.ty];
            let at = (idx[0] as usize).wrapping_mul(ty.size as usize);
            let p = data
                .get(self.zone, at, ty.size as usize)
                .ok_or_else(|| Error::new(format!("a rule reads past {name}")))?;
            return Ok(read_value(&p, ty));
        }
        self.field(inst, chain, idx)
    }

    fn eval(&self, e: Option<ExprId>) -> Result<i64> {
        let e = &self.s.exprs[e.ok_or_else(|| Error::new("a count is missing from the rules"))?];
        if let Some(c) = e.constant {
            return Ok(c);
        }
        e.root.eval(&|r, idx| self.lookup(&e.refs[r], idx), true)
    }

    // ---- structs --------------------------------------------------------------------

    fn load_struct(&mut self, info: StructId, given: Option<&Inst>, at_start: bool) -> Result<Inst> {
        let st = &self.s.structs[info];
        let dynamic = st.dynamic;
        let inst = if at_start && !(st.is_union && dynamic.is_some()) {
            // Up to a dynamic member, the struct is streamed at once.
            let n = match dynamic {
                Some(d) => self.s.member(d).offset,
                None => self.s.defs.types[st.ctype].size,
            } as usize;
            let mut data = self.read(n)?;
            if dynamic.is_some() {
                let mut copy = vec![0u8; n];
                if let Bytes::View(at, _) = data.bytes {
                    copy.copy_from_slice(&self.zone[at..at + n]);
                }
                data = Buffer::own(copy, true);
            }
            Inst { info, buf: data, off: 0, index: 0 }
        } else if at_start {
            // A union whose members are only partly known: each streams itself.
            Inst { info, buf: Buffer::own(Vec::new(), true), off: 0, index: 0 }
        } else {
            given.cloned().ok_or_else(|| Error::new("a struct is loaded from nothing"))?
        };
        self.vars[info] = Some(inst.clone());

        let st = &self.s.structs[info];
        let mut pushed = false;
        if !st.asset.is_empty() {
            self.push(VIRTUAL);
            pushed = true;
        } else if let Some(b) = st.block {
            self.push(self.block_of(b));
            pushed = true;
        }
        if st.is_union {
            for &mi in &st.used {
                if !self.s.needs_treatment(mi) {
                    continue;
                }
                let cond = self.s.members[mi].condition;
                if cond.is_none() || self.eval(cond)? != 0 {
                    self.vars[info] = Some(inst.clone());
                    self.load_reference(&inst, mi, 0, Vec::new())?;
                    break;
                }
            }
        } else {
            for &mi in &st.ordered {
                if !self.s.needs_treatment(mi) {
                    continue;
                }
                let cond = self.s.members[mi].condition;
                if cond.is_some() && self.eval(cond)? == 0 {
                    continue;
                }
                self.vars[info] = Some(inst.clone());
                self.load_reference(&inst, mi, 0, Vec::new())?;
            }
        }
        if pushed {
            self.pop();
        }
        Ok(inst)
    }

    fn load_array(
        &mut self,
        info: StructId,
        at_start: bool,
        count: i64,
        buf: Option<Buffer>,
        base: usize,
    ) -> Result<Buffer> {
        if count < 0 {
            return Err(Error::new(format!("a negative count for {}", self.s.structs[info].name)));
        }
        let size = self.s.defs.types[self.s.structs[info].ctype].size as usize;
        let (buf, base) = match buf {
            Some(buf) if !at_start => (buf, base),
            _ => (self.read_many(size, count)?, 0),
        };
        for k in 0..count {
            let e = Inst { info, buf: buf.clone(), off: base.wrapping_add((k as usize).wrapping_mul(size)), index: k };
            self.vars[info] = Some(e.clone());
            self.load_struct(info, Some(&e), false)?;
        }
        Ok(buf)
    }

    // ---- members --------------------------------------------------------------------

    fn member_loc(&self, inst: &Inst, mi: MemberId, indices: &[i64]) -> usize {
        let m = self.s.member(mi);
        // The array sizes before the first pointer, and an element's size: a pointer's
        // if there is one anywhere.
        let dims: Vec<i64> = m.mods.iter().take_while(|&&x| x != POINTER).map(|&x| i64::from(x)).collect();
        let elem = if m.has_pointer() { 4 } else { i64::from(self.s.defs.types[m.ty].size) };
        let mut off = inst.off.wrapping_add(m.offset as usize);
        for (k, &index) in indices.iter().enumerate() {
            let mut stride = elem;
            for &d in dims.iter().skip(k + 1) {
                stride = stride.wrapping_mul(d);
            }
            off = off.wrapping_add(index.wrapping_mul(stride) as usize);
        }
        off
    }

    fn load_reference(&mut self, inst: &Inst, mi: MemberId, mod_pos: usize, indices: Vec<i64>) -> Result<()> {
        let s = self.s;
        let mods = &s.member(mi).mods;
        let has_cur = mod_pos < mods.len();
        let has_next = mod_pos + 1 < mods.len();
        let cur = if has_cur { mods[mod_pos] } else { 0 };
        let cur_pointer = has_cur && cur == POINTER;
        let cur_array = has_cur && cur != POINTER;
        let next_pointer = has_next && mods[mod_pos + 1] == POINTER;
        let mut combined: i64 = 0;
        if !indices.is_empty() {
            let dims: Vec<i64> = s.member(mi).dims().collect();
            for (k, &index) in indices.iter().enumerate() {
                let mut st: i64 = 1;
                for &d in dims.iter().skip(k + 1) {
                    st = st.wrapping_mul(d);
                }
                combined = combined.wrapping_add(index.wrapping_mul(st));
            }
        }
        let following_pointer = mods.iter().skip(mod_pos + 1).any(|&m| m == POINTER);
        let info = &s.members[mi];
        let is_array = |k| s.count_is_array(mi, k, combined);

        let kind = if cur_array && info.array_size.contains_key(&mod_pos) {
            Load::Dynamic
        } else if cur_pointer && !is_array(mod_pos) && !following_pointer {
            Load::Single
        } else if cur_pointer && is_array(mod_pos) && !following_pointer {
            Load::Array
        } else if ((cur_pointer && is_array(mod_pos)) || cur_array)
            && next_pointer
            && !s.any_count_is_array(mi, mod_pos + 1)
        {
            Load::PtrArray
        } else if cur_array && !has_next {
            Load::EmbArray
        } else if !has_cur {
            Load::Embedded
        } else if cur_array {
            for k in 0..i64::from(cur) {
                let mut deeper = indices.clone();
                deeper.push(k);
                self.load_reference(inst, mi, mod_pos + 1, deeper)?;
            }
            return Ok(());
        } else {
            return Err(Error::new(format!("an unsupported member: {}::{}", s.structs[info.parent].name, info.name)));
        };
        let loc = self.member_loc(inst, mi, &indices);
        self.load_block(inst, mi, mod_pos, combined, kind, loc)
    }

    fn load_block(
        &mut self,
        inst: &Inst,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: Load,
        loc: usize,
    ) -> Result<()> {
        let s = self.s;
        let info = &s.members[mi];
        let mut b = info.block;
        if let Some(bc) = &info.block_cond {
            b = Some(if self.eval(Some(bc.cond))? != 0 { bc.if_true } else { bc.if_false });
        }
        let push = b.is_some_and(|b| !(s.blocks[b].kind == "normal" && s.blocks[b].is_default));
        if let (true, Some(b)) = (push, b) {
            self.push(self.block_of(b));
        }

        let mods = &s.member(mi).mods;
        let is_array = mod_pos < mods.len() && mods[mod_pos] != POINTER;
        let pointer_kind = matches!(kind, Load::Array | Load::PtrArray | Load::Single);
        let check = pointer_kind && !(kind == Load::PtrArray && is_array) && !(kind == Load::Single && info.is_string);
        let mut load = true;
        let mut val = None;
        if check {
            let v = self.u32_at(&inst.buf, loc)?;
            val = Some(v);
            if kind == Load::Single {
                self.visitor.on_pointer(mi, -1, v)?;
            }
            if v == 0 {
                load = false;
            }
        }
        if load && pointer_kind && !(kind == Load::PtrArray && is_array) && info.is_reusable {
            // Anything but data that follows is a pointer back to what was loaded before:
            // nothing of it is in the stream here.
            let v = match val {
                Some(v) => v,
                None => self.u32_at(&inst.buf, loc)?,
            };
            let in_temp = self.block_kind(info.block, "temp");
            if !(v == FOLLOWING || (in_temp && v == INSERT)) {
                load = false;
            }
        }
        if load {
            self.alloc_member(inst, mi, mod_pos, kind, loc)?;
            self.type_check(inst, mi, mod_pos, combined, kind, loc)?;
        }
        if push {
            self.pop();
        }
        Ok(())
    }

    /// Where the loader puts what a pointer names: aligned for it, and, for data in the
    /// temp block a later pointer may name, with a pointer to it made in the virtual block.
    fn alloc_member(&mut self, inst: &Inst, mi: MemberId, mod_pos: usize, kind: Load, loc: usize) -> Result<()> {
        let s = self.s;
        let info = &s.members[mi];
        let m = s.member(mi);
        let is_array = mod_pos < m.mods.len() && m.mods[mod_pos] != POINTER;
        let mut make = matches!(kind, Load::Array | Load::PtrArray | Load::Single);
        if kind == Load::PtrArray {
            make = !is_array;
        } else if info.is_string || info.ty.is_some_and(|t| !s.structs[t].asset.is_empty()) {
            make = false;
        }
        if !make {
            return Ok(());
        }
        let following_pointer = m.mods.iter().skip(mod_pos + 1).any(|&x| x == POINTER);
        let align = if info.alloc_align != 0 {
            info.alloc_align
        } else if following_pointer {
            4
        } else {
            match info.ty {
                Some(t) if s.structs[t].alloc_align != 0 => s.structs[t].alloc_align,
                Some(t) => s.defs.types[s.structs[t].ctype].align,
                None => s.defs.types[m.ty].align,
            }
        };
        self.alloc(align);
        if self.block_kind(info.block, "temp") && self.u32_at(&inst.buf, loc)? == INSERT {
            self.insert_pointer();
        }
        Ok(())
    }

    fn elem_size(&self, mi: MemberId, mod_pos: usize) -> usize {
        let m = self.s.member(mi);
        let mut n = self.s.defs.types[m.ty].size as usize;
        for &x in m.mods.iter().skip(mod_pos + 1) {
            if x == POINTER {
                return 4;
            }
            n = n.wrapping_mul(x as usize);
        }
        n
    }

    /// The size of an element of what `mi` names: its struct's, or its type's.
    fn element_size(&self, mi: MemberId) -> usize {
        let s = self.s;
        match s.members[mi].ty {
            Some(t) => s.defs.types[s.structs[t].ctype].size as usize,
            None => s.defs.types[s.member(mi).ty].size as usize,
        }
    }

    fn type_check(
        &mut self,
        inst: &Inst,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: Load,
        loc: usize,
    ) -> Result<()> {
        let s = self.s;
        let info = &s.members[mi];
        let mods = &s.member(mi).mods;
        let in_runtime = self.block_kind(info.block, "runtime");
        if info.is_string {
            if kind == Load::Single {
                self.load_xstring(&inst.buf, loc)?;
            } else if kind == Load::PtrArray {
                if mods[mod_pos] != POINTER {
                    self.load_xstring_array(false, i64::from(mods[mod_pos]), Some(inst.buf.clone()), loc)?;
                } else {
                    let n = self.eval(s.count_of(mi, mod_pos, combined))?;
                    self.load_xstring_array(true, n, None, 0)?;
                }
            }
            return Ok(());
        }
        let t = info.ty;
        if let Some(t) = t
            && !s.structs[t].asset.is_empty()
        {
            if kind == Load::Single {
                self.load_asset_ptr(t, &inst.buf, loc)?;
            } else if kind == Load::PtrArray {
                self.ptr_array(inst, mi, mod_pos, combined, loc)?;
            }
            return Ok(());
        }
        let non_leaf = t.filter(|&t| !s.structs[t].is_leaf);
        match kind {
            Load::Array => {
                let n = self.eval(s.count_of(mi, mod_pos, combined))?;
                if n < 0 {
                    return Err(Error::new(format!("a negative count for {}", info.name)));
                }
                match non_leaf {
                    Some(t) if !in_runtime => {
                        self.load_array(t, true, n, None, 0)?;
                    }
                    _ => {
                        let elem = self.elem_size(mi, mod_pos);
                        let data = self.read_many(elem, n)?;
                        let bytes = data.size();
                        self.pointed[mi] = Some(data);
                        let last = self.last;
                        self.visitor.on_data(mi, &last, bytes)?;
                    }
                }
            }
            Load::Single => match non_leaf {
                Some(t) if !in_runtime => {
                    self.load_struct(t, None, true)?;
                }
                _ => {
                    let bytes = self.elem_size(mi, mod_pos);
                    self.read(bytes)?;
                    let last = self.last;
                    self.visitor.on_data(mi, &last, bytes)?;
                }
            },
            Load::PtrArray => self.ptr_array(inst, mi, mod_pos, combined, loc)?,
            Load::EmbArray => {
                let n = i64::from(mods[mod_pos]);
                let after = s.after_partial_load(mi);
                if let (false, Some(t)) = (info.is_leaf, t) {
                    if after {
                        self.load_array(t, true, n, None, 0)?;
                    } else {
                        self.load_array(t, false, n, Some(inst.buf.clone()), loc)?;
                    }
                } else if after {
                    self.read_many(self.element_size(mi), n)?;
                }
            }
            Load::Embedded => {
                let after = s.after_partial_load(mi);
                if let (false, Some(t)) = (info.is_leaf, t) {
                    if after {
                        let sub = self.load_struct(t, None, true)?;
                        if inst.buf.growable {
                            if sub.buf.growable {
                                self.append(&inst.buf, &sub.buf, 0, sub.buf.size())?;
                            } else {
                                let size = s.defs.types[s.structs[t].ctype].size as usize;
                                self.append(&inst.buf, &sub.buf, sub.off, size)?;
                            }
                        }
                    } else {
                        // A struct wrapping another at offset 0 (GfxCellTree128 around
                        // GfxCellTree) keeps its array index, for counts written as
                        // pointer differences.
                        let e = Inst {
                            info: t,
                            buf: inst.buf.clone(),
                            off: loc,
                            index: if loc == inst.off { inst.index } else { 0 },
                        };
                        self.load_struct(t, Some(&e), false)?;
                    }
                } else if after {
                    let data = self.read(self.element_size(mi))?;
                    if inst.buf.growable {
                        self.append(&inst.buf, &data, 0, data.size())?;
                    }
                }
            }
            Load::Dynamic => {
                let n = self.eval(info.array_size.get(&mod_pos).copied())?;
                if n < 0 {
                    return Err(Error::new(format!("a negative size for {}", info.name)));
                }
                let buf = match non_leaf {
                    Some(t) => self.load_array(t, true, n, None, 0)?,
                    None => {
                        let mut elem = s.defs.types[s.member(mi).ty].size as usize;
                        for &x in mods.iter().skip(1) {
                            elem = elem.wrapping_mul(x as usize);
                        }
                        self.read_many(elem, n)?
                    }
                };
                if inst.buf.growable {
                    self.append(&inst.buf, &buf, 0, buf.size())?;
                }
            }
        }
        Ok(())
    }

    fn ptr_array(&mut self, inst: &Inst, mi: MemberId, mod_pos: usize, combined: i64, loc: usize) -> Result<()> {
        let cur = self.s.member(mi).mods[mod_pos];
        if cur != POINTER {
            self.load_ptr_array(mi, false, i64::from(cur), Some(inst.buf.clone()), loc)
        } else {
            let n = self.eval(self.s.count_of(mi, mod_pos, combined))?;
            self.load_ptr_array(mi, true, n, None, 0)
        }
    }

    fn load_ptr_array(
        &mut self,
        mi: MemberId,
        at_start: bool,
        count: i64,
        buf: Option<Buffer>,
        loc: usize,
    ) -> Result<()> {
        let s = self.s;
        if count < 0 {
            return Err(Error::new(format!("a negative count for {}", s.members[mi].name)));
        }
        let (buf, loc) = match buf {
            Some(buf) if !at_start => (buf, loc),
            _ => (self.read_many(4, count)?, 0),
        };
        let info = &s.members[mi];
        let t = info.ty;
        for k in 0..count {
            let at = loc.wrapping_add(4 * k as usize);
            let val = self.u32_at(&buf, at)?;
            self.visitor.on_pointer(mi, k, val)?;
            if val == 0 {
                continue;
            }
            if let Some(t) = t
                && !s.structs[t].asset.is_empty()
            {
                self.load_asset_ptr(t, &buf, at)?;
                continue;
            }
            if info.is_reusable && val != FOLLOWING {
                continue;
            }
            let align = match t {
                Some(t) if s.structs[t].alloc_align != 0 => s.structs[t].alloc_align,
                Some(t) => s.defs.types[s.structs[t].ctype].align,
                None => s.defs.types[s.member(mi).ty].align,
            };
            self.alloc(align);
            match t {
                Some(t) if !s.structs[t].is_leaf => {
                    self.load_struct(t, None, true)?;
                }
                _ => {
                    self.read(self.element_size(mi))?;
                }
            }
        }
        Ok(())
    }

    // ---- strings and assets ---------------------------------------------------------

    fn load_xstring(&mut self, buf: &Buffer, loc: usize) -> Result<()> {
        if self.u32_at(buf, loc)? != FOLLOWING {
            return Ok(()); // none, or one loaded before
        }
        let length = self.zone[self.pos..]
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| Error::new("a string runs off the end of the zone"))?;
        self.read(length + 1)?;
        Ok(())
    }

    fn load_xstring_array(&mut self, at_start: bool, count: i64, buf: Option<Buffer>, loc: usize) -> Result<()> {
        if count < 0 {
            return Err(Error::new("a negative string count"));
        }
        let (buf, loc) = match buf {
            Some(buf) if !at_start => (buf, loc),
            _ => (self.read_many(4, count)?, 0),
        };
        for k in 0..count {
            self.load_xstring(&buf, loc.wrapping_add(4 * k as usize))?;
        }
        Ok(())
    }

    fn load_asset_ptr(&mut self, info: StructId, buf: &Buffer, loc: usize) -> Result<()> {
        let val = self.u32_at(buf, loc)?;
        let st = &self.s.structs[info];
        let in_temp = self.block_kind(st.block, "temp");
        if in_temp {
            self.push(TEMP);
        }
        if val == FOLLOWING || (in_temp && val == INSERT) {
            let align = if st.alloc_align != 0 { st.alloc_align } else { self.s.defs.types[st.ctype].align };
            self.alloc(align);
            if in_temp && val == INSERT {
                self.insert_pointer();
            }
            self.load_struct(info, None, true)?;
        }
        if in_temp {
            self.pop();
        }
        Ok(())
    }

    /// The walk; an Error when the zone is not what the rules describe.
    pub fn walk(&mut self) -> Result<()> {
        // The header: the zone's size, then each of the six blocks'.
        if self.zone.len() < 8 + 4 * 6 + 16 {
            return Err(Error::new("the zone is too short for its header"));
        }
        let whole = Buffer::view(0, self.zone.len());
        for k in 0..6 {
            self.header_sizes[k] = self.u32_at(&whole, 8 + 4 * k)?;
        }
        self.pos = 8 + 4 * 6;
        let head = Buffer::view(self.pos, 16);
        let (string_count, strings) = (self.u32_at(&head, 0)?, self.u32_at(&head, 4)?);
        let (count, list) = (self.u32_at(&head, 8)?, self.u32_at(&head, 12)?);
        self.pos += 16;
        self.push(VIRTUAL);
        if strings != 0 {
            self.alloc(4);
            let b = self.read_many(4, i64::from(string_count))?;
            for k in 0..string_count as usize {
                self.load_xstring(&b, 4 * k)?;
            }
        }
        self.pop();
        self.push(VIRTUAL);
        if list != 0 {
            self.alloc(4);
            let b = self.read_many(8, i64::from(count))?;
            for k in 0..count as usize {
                let ty = self.u32_at(&b, 8 * k)?;
                let Some(&info) = asset_struct(ty).and_then(|name| self.s.assets.get(name)) else {
                    return Err(Error::new(format!("asset {k} has a type the rules do not describe ({ty})")));
                };
                self.asset_type = ty;
                self.load_asset_ptr(info, &b, 8 * k + 4)?;
            }
            self.asset_count = count;
        }
        self.pop();
        if self.pos != self.zone.len() {
            return Err(Error::new(format!("the walk stopped at {} of {} bytes", self.pos, self.zone.len())));
        }
        // The temp block is a scratch area and the callback one is not streamed; the
        // others end where the header says.
        for k in [PHYSICAL, RUNTIME, VIRTUAL, LARGE] {
            if self.block_pos[k] != self.header_sizes[k] {
                return Err(Error::new(format!(
                    "block {k} ends at {}, not at the {} the zone's header gives",
                    self.block_pos[k], self.header_sizes[k]
                )));
            }
        }
        Ok(())
    }
}
