// A zone read into a tree (tree.rs), and a tree written back out as a zone.
//
// The walk goes through a zone the way the game's loader streams it (as zone/src/walker.rs
// does for the shader pass), and does at each step what reading or writing does there.
// Reading keeps every struct it meets as a node, and what each pointer loaded under it.
// Writing walks a platform's rules over such a tree and streams the zone the loader would
// read it from: every count, block and alignment comes out right, and pointers back to
// earlier data are worked out anew from where their targets were written. Written with
// the 360's rules, after the converter has turned the PC's structs into the 360's, it
// converts the zone.

use crate::error::{Error, Result};
use crate::num::{load, load_f32, store, truncate};
use crate::query;
use crate::text;
use crate::tree::{ElemType, FOLLOWING, INSERT, NodeId, Store, Tree, Ty, elements, has_pointer_after};
use mw2_zone::defs::{Kind, Member, POINTER};
use mw2_zone::schema::{BlockId, ExprId, ExprRef, MemberId, Schema, StructId};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::OnceLock;

pub const TEMP: i32 = 0;
pub const PHYSICAL: i32 = 1;
pub const RUNTIME: i32 = 2;
pub const VIRTUAL: i32 = 3;
pub const LARGE: i32 = 4;
pub const CALLBACK: i32 = 5;
pub const VERTEX: i32 = 6;
pub const INDEX: i32 = 7;
pub const BLOCK_SHIFT: u32 = 28;
pub const OFFSET_MASK: u32 = (1 << BLOCK_SHIFT) - 1;

/// What a platform's zones are: their byte order, the memory blocks their header lists
/// (the PC's eight, the 360's six), their asset types.
pub struct Platform {
    pub name: &'static str,
    pub big: bool,
    pub blocks: i32,
    /// Asset type number -> its name.
    pub types: Vec<&'static str>,
    /// Asset type number -> the rules' asset name.
    pub structs: BTreeMap<u32, &'static str>,
    /// Vertex and index data have blocks of their own.
    pub vertex_blocks: bool,
}

impl Platform {
    pub fn header_bytes(&self) -> usize {
        8 + 4 * self.blocks as usize
    }

    pub fn block_of(&self, name: &str) -> Result<i32> {
        Ok(match name {
            "XFILE_BLOCK_TEMP" => TEMP,
            "XFILE_BLOCK_PHYSICAL" => PHYSICAL,
            "XFILE_BLOCK_RUNTIME" => RUNTIME,
            "XFILE_BLOCK_VIRTUAL" => VIRTUAL,
            "XFILE_BLOCK_LARGE" => LARGE,
            "XFILE_BLOCK_CALLBACK" => CALLBACK,
            // Vertex and index data: blocks of their own on the PC, the physical one on
            // the 360.
            "XFILE_BLOCK_VERTEX" => {
                if self.vertex_blocks {
                    VERTEX
                } else {
                    PHYSICAL
                }
            }
            "XFILE_BLOCK_INDEX" => {
                if self.vertex_blocks {
                    INDEX
                } else {
                    PHYSICAL
                }
            }
            _ => return Err(Error::new(format!("no memory block {name}"))),
        })
    }

    pub fn type_number(&self, type_name: &str) -> Result<u32> {
        self.types
            .iter()
            .position(|&t| t == type_name)
            .map(|k| k as u32)
            .ok_or_else(|| Error::new(format!("{} zones have no asset type {type_name}", self.name)))
    }
}

pub fn xbox() -> &'static Platform {
    static P: OnceLock<Platform> = OnceLock::new();
    P.get_or_init(|| Platform {
        name: "xbox",
        big: true,
        blocks: 6,
        // The title update's asset types (DB_GetXAssetTypeName).
        types: vec![
            "physpreset",
            "phys_collmap",
            "xanim",
            "xmodelsurfs",
            "xmodel",
            "material",
            "pixelshader",
            "techset",
            "image",
            "sound",
            "sndcurve",
            "loaded_sound",
            "col_map_sp",
            "col_map_mp",
            "com_map",
            "game_map_sp",
            "game_map_mp",
            "map_ents",
            "fx_map",
            "gfx_map",
            "lightdef",
            "ui_map",
            "font",
            "menufile",
            "menu",
            "localize",
            "weapon",
            "snddriverglobals",
            "fx",
            "impactfx",
            "aitype",
            "mptype",
            "character",
            "xmodelalias",
            "rawfile",
            "stringtable",
            "leaderboarddef",
            "structureddatadef",
            "tracer",
            "vehicle",
            "addon_map_ents",
        ],
        structs: BTreeMap::from([
            (0, "AssetPhysPreset"),
            (1, "AssetPhysCollMap"),
            (2, "AssetXAnim"),
            (3, "AssetXModelSurfs"),
            (4, "AssetXModel"),
            (5, "AssetMaterial"),
            (6, "AssetPixelShader"),
            (7, "AssetTechniqueSet"),
            (8, "AssetImage"),
            (9, "AssetSound"),
            (10, "AssetSoundCurve"),
            (11, "AssetLoadedSound"),
            (12, "AssetClipMapMp"),
            (13, "AssetClipMapMp"),
            (14, "AssetComWorld"),
            (15, "AssetGameWorldSp"),
            (16, "AssetGameWorldMp"),
            (17, "AssetMapEnts"),
            (18, "AssetFxWorld"),
            (19, "AssetGfxWorld"),
            (20, "AssetLightDef"),
            (22, "AssetFont"),
            (23, "AssetMenuList"),
            (24, "AssetMenu"),
            (25, "AssetLocalize"),
            (26, "AssetWeapon"),
            (27, "AssetSndDriverGlobals"),
            (28, "AssetFx"),
            (29, "AssetImpactFx"),
            (34, "AssetRawFile"),
            (35, "AssetStringTable"),
            (36, "AssetLeaderboard"),
            (37, "AssetStructuredDataDef"),
            (38, "AssetTracer"),
            (39, "AssetVehicle"),
            (40, "AssetAddonMapEnts"),
        ]),
        vertex_blocks: false,
    })
}

pub fn pc() -> &'static Platform {
    static P: OnceLock<Platform> = OnceLock::new();
    P.get_or_init(|| Platform {
        name: "pc",
        big: false,
        blocks: 8,
        // The PC's (version 276): the 360's and vertex shaders and declarations.
        types: vec![
            "physpreset",
            "phys_collmap",
            "xanim",
            "xmodelsurfs",
            "xmodel",
            "material",
            "pixelshader",
            "vertexshader",
            "vertexdecl",
            "techset",
            "image",
            "sound",
            "sndcurve",
            "loaded_sound",
            "col_map_sp",
            "col_map_mp",
            "com_map",
            "game_map_sp",
            "game_map_mp",
            "map_ents",
            "fx_map",
            "gfx_map",
            "lightdef",
            "ui_map",
            "font",
            "menufile",
            "menu",
            "localize",
            "weapon",
            "snddriverglobals",
            "fx",
            "impactfx",
            "aitype",
            "mptype",
            "character",
            "xmodelalias",
            "rawfile",
            "stringtable",
            "leaderboarddef",
            "structureddatadef",
            "tracer",
            "vehicle",
            "addon_map_ents",
        ],
        structs: BTreeMap::from([
            (0, "AssetPhysPreset"),
            (1, "AssetPhysCollMap"),
            (2, "AssetXAnim"),
            (3, "AssetXModelSurfs"),
            (4, "AssetXModel"),
            (5, "AssetMaterial"),
            (6, "AssetPixelShader"),
            (7, "AssetVertexShader"),
            (8, "AssetVertexDecl"),
            (9, "AssetTechniqueSet"),
            (10, "AssetImage"),
            (11, "AssetSound"),
            (12, "AssetSoundCurve"),
            (13, "AssetLoadedSound"),
            (14, "AssetClipMapMp"),
            (15, "AssetClipMapMp"),
            (16, "AssetComWorld"),
            (17, "AssetGameWorldSp"),
            (18, "AssetGameWorldMp"),
            (19, "AssetMapEnts"),
            (20, "AssetFxWorld"),
            (21, "AssetGfxWorld"),
            (22, "AssetLightDef"),
            (24, "AssetFont"),
            (25, "AssetMenuList"),
            (26, "AssetMenu"),
            (27, "AssetLocalize"),
            (28, "AssetWeapon"),
            (30, "AssetFx"),
            (31, "AssetImpactFx"),
            (36, "AssetRawFile"),
            (37, "AssetStringTable"),
            (38, "AssetLeaderboard"),
            (39, "AssetStructuredDataDef"),
            (40, "AssetTracer"),
            (41, "AssetVehicle"),
            (42, "AssetAddonMapEnts"),
        ]),
        vertex_blocks: true,
    })
}

// ---- buffers --------------------------------------------------------------------------------

/// Bytes the walk streamed (or, for the runtime block, made), and where each piece of
/// them is in the zone: a struct can be put together from several pieces.
#[derive(Default)]
struct Buf {
    b: Vec<u8>,
    /// (offset in b, zone position or -1)
    segs: Vec<(usize, i64)>,
    /// Put together from pieces.
    pieces: bool,
}

impl Buf {
    fn zone_at(&self, loc: usize) -> i64 {
        let best = self.segs.iter().rev().find(|s| s.0 <= loc);
        match best {
            Some(&(at, zp)) if zp >= 0 => zp + (loc - at) as i64,
            _ => -1,
        }
    }
}

type BufPtr = Rc<RefCell<Buf>>;

fn pieces() -> BufPtr {
    Rc::new(RefCell::new(Buf { pieces: true, ..Buf::default() }))
}

// src's `n` bytes from `off` on appended to dst, keeping where they came from.
fn extend(dst: &BufPtr, src: &BufPtr, off: usize, n: usize) {
    let (bytes, segs) = {
        let s = src.borrow();
        let end = (off + n).min(s.b.len());
        let bytes = s.b[off.min(end)..end].to_vec();
        let segs: Vec<(usize, i64)> = if s.pieces {
            s.segs
                .iter()
                .filter(|&&(so, _)| so < off + n)
                .map(|&(so, zp)| {
                    let rel = so.max(off);
                    (rel - off, if zp < 0 { -1 } else { zp + (rel - so) as i64 })
                })
                .collect()
        } else {
            vec![(0, s.zone_at(off))]
        };
        (bytes, segs)
    };
    let mut d = dst.borrow_mut();
    let base = d.b.len();
    d.b.extend_from_slice(&bytes);
    d.segs.extend(segs.into_iter().map(|(at, zp)| (base + at, zp)));
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Where {
    pub block: i32,
    pub pos: u32,
}

type InstId = usize;

struct Inst {
    info: StructId,
    buf: BufPtr,
    off: usize,
    index: i64,
    at: Where,
}

/// What a read streams.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DescKind {
    Type,
    Partial,
    Ptrs,
    U32,
    String,
}

#[derive(Clone, Copy)]
struct Desc {
    kind: DescKind,
    t: Option<Ty>,
    count: i64,
}

impl Desc {
    fn of(kind: DescKind) -> Desc {
        Desc { kind, t: None, count: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadKind {
    Dynamic,
    Single,
    Array,
    PtrArray,
    EmbArray,
    Embedded,
}

impl LoadKind {
    fn name(self) -> &'static str {
        match self {
            LoadKind::Dynamic => "dynamic",
            LoadKind::Single => "single",
            LoadKind::Array => "array",
            LoadKind::PtrArray => "ptrarray",
            LoadKind::EmbArray => "embarray",
            LoadKind::Embedded => "embedded",
        }
    }
}

/// The struct whose member is being loaded, the member, its element's indices and the
/// kind of load.
struct Ctx {
    inst: InstId,
    mi: MemberId,
    idx: Vec<i32>,
}

/// A piece of a block the reading walk filled, and the node it holds.
struct Placed {
    block: i32,
    off: u32,
    n: u32,
    node: NodeId,
    t: ElemType,
}

/// What reading keeps as it goes.
#[derive(Default)]
struct ReadState {
    // A union streamed member by member (an animation's data, in a file only looked things
    // up in) is passed over, not kept.
    lenient: bool,
    placed: Vec<Placed>,
    refs: Vec<NodeId>,
    // What the last load made (None: nothing), or the plain data it read, which becomes a
    // Leaf when it is kept.
    cap: Option<NodeId>,
    leaf_data: Option<BufPtr>,
    leaf_desc: Option<Desc>,
    cap_where: Where,
    slot: u32,
    asset_insert: bool,
    member_slots: Vec<u32>,
    asset_starts: Vec<(NodeId, usize)>,
}

/// Where a Ref into an object of another layout points once converted.
pub type MapRel = dyn Fn(&crate::tree::Ref) -> Result<i64>;

/// What writing keeps as it goes.
#[derive(Default)]
struct WriteState<'a> {
    tree: Option<&'a Tree>,
    map_rel: Option<&'a MapRel>,
    dedupe: bool,
    keep_bytes: bool,
    deduped: usize,
    starts: HashMap<NodeId, usize>,
    written: Vec<(usize, NodeId)>,
    loc: HashMap<NodeId, Where>,
    // What the next read writes (None: nothing).
    obj: Option<NodeId>,
    last: Option<NodeId>,
    slot: u32,
    pointer_types: HashMap<Ty, bool>,
    first: HashMap<NodeId, u32>,
    zone_positions: Vec<usize>,
    // (block, offset, n) of each written span
    zone_spans: Vec<[u32; 3]>,
}

struct Walk<'a> {
    s: &'static Schema,
    plat: &'static Platform,
    big: bool,
    zone: Vec<u8>,
    pos: usize,
    st: &'a mut Store,
    writing: bool,
    r: ReadState,
    w: WriteState<'a>,

    ctx: Vec<Ctx>,
    block_pos: Vec<u32>,
    block_max: Vec<u32>,
    block: i32,
    // Of the asset being loaded.
    asset_type: i32,
    // The instance of each struct being loaded, for the rules' expressions.
    vars: Vec<Option<InstId>>,
    stack: Vec<(u32, i32)>,
    pointed: HashMap<MemberId, BufPtr>,
    insts: Vec<Inst>,
    // The node each instance is.
    reg: Vec<Option<NodeId>>,
    path: Vec<String>,
    lod_info: Option<StructId>,
    surfs_info: Option<StructId>,
}

// Run-time data a zone may ask the loader for (stock files ask for a few MB).
const LARGEST_RUNTIME_BLOCK: usize = 256 << 20;

impl<'a> Walk<'a> {
    fn new(st: &'a mut Store, schema: &'static Schema, plat: &'static Platform, zone: Vec<u8>) -> Walk<'a> {
        let blocks = plat.blocks as usize;
        Walk {
            s: schema,
            plat,
            big: plat.big,
            zone,
            pos: plat.header_bytes(),
            st,
            writing: false,
            r: ReadState::default(),
            w: WriteState::default(),
            ctx: Vec::new(),
            block_pos: vec![0; blocks],
            block_max: vec![0; blocks],
            block: TEMP,
            asset_type: -1,
            vars: vec![None; schema.structs.len()],
            stack: Vec::new(),
            pointed: HashMap::new(),
            insts: Vec::new(),
            reg: Vec::new(),
            path: Vec::new(),
            lod_info: schema.infos.get("XModelLodInfo").copied(),
            surfs_info: schema.infos.get("XModelSurfs").copied(),
        }
    }

    fn member(&self, mi: MemberId) -> &'static Member {
        let s: &'static Schema = self.s;
        s.member(mi)
    }

    fn ty(&self, id: usize) -> Ty {
        Ty::new(self.s, id)
    }

    fn ctype(&self, info: StructId) -> Ty {
        self.ty(self.s.structs[info].ctype)
    }

    fn path_text(&self) -> String {
        self.path.join(" > ")
    }

    fn u32_at(&self, buf: &BufPtr, at: usize) -> Result<u32> {
        let b = buf.borrow();
        if at + 4 > b.b.len() {
            return Err(Error::new(format!("a pointer past its struct at {}", self.path_text())));
        }
        Ok(load(&b.b[at..], 4, self.big) as u32)
    }

    // ---- the stream and the blocks ------------------------------------------------------

    fn read_zone(&mut self, n: usize) -> Result<(BufPtr, Where)> {
        let mut data = Buf::default();
        if self.block == RUNTIME {
            // Made by the loader at run time: nothing of it is in the file.
            if n > LARGEST_RUNTIME_BLOCK {
                return Err(Error::new(format!(
                    "the zone asks for {n} bytes of run-time data at {}",
                    self.path_text()
                )));
            }
            data.b = vec![0; n];
            data.segs.push((0, -1));
        } else {
            if n > self.zone.len() - self.pos {
                return Err(Error::new(format!("the zone ends inside {}", self.path_text())));
            }
            data.b = self.zone[self.pos..self.pos + n].to_vec();
            data.segs.push((0, self.pos as i64));
            self.pos += n;
        }
        let b = self.block as usize;
        let at = Where { block: self.block, pos: self.block_pos[b] };
        self.block_pos[b] = self.block_pos[b].wrapping_add(n as u32);
        self.block_max[b] = self.block_max[b].max(self.block_pos[b]);
        Ok((Rc::new(RefCell::new(data)), at))
    }

    fn read(&mut self, n: usize, desc: Desc) -> Result<(BufPtr, Where)> {
        if self.writing { self.write_read(n, desc) } else { self.tree_read(n, desc) }
    }

    fn alloc(&mut self, align: u32) {
        let align = align.max(1);
        let p = &mut self.block_pos[self.block as usize];
        *p = (p.wrapping_add(align - 1)) / align * align;
    }

    fn push(&mut self, b: i32) {
        self.stack.push((self.block_pos[b as usize], self.block));
        self.block = b;
    }

    // The temp block is scratch space: what was pushed onto it is gone once popped.
    fn pop(&mut self) {
        let Some((pos, block)) = self.stack.pop() else { return };
        if self.block == TEMP {
            self.block_pos[TEMP as usize] = pos;
        }
        self.block = block;
    }

    // The pointer the loader makes in the virtual block for data in the temp block that
    // something later points to.
    fn make_pointer(&mut self) {
        self.push(VIRTUAL);
        self.alloc(4);
        let v = VIRTUAL as usize;
        self.block_pos[v] = self.block_pos[v].wrapping_add(4);
        self.block_max[v] = self.block_max[v].max(self.block_pos[v]);
        self.pop();
    }

    fn insert_pointer(&mut self) {
        self.push(VIRTUAL);
        self.alloc(4);
        let slot = self.block_pos[VIRTUAL as usize];
        self.pop();
        self.make_pointer();
        if self.writing {
            self.w.slot = slot;
            // A struct member's slot (XModelLodInfo.modelSurfs); an asset's is placed in
            // load_asset_ptr.
            if let Some(s) = self.st.as_struct(self.w.obj).and_then(|s| s.slot)
                && !self.w.loc.contains_key(&s)
            {
                self.w.loc.insert(s, Where { block: VIRTUAL, pos: slot });
            }
        } else {
            self.r.slot = slot;
            if !self.r.asset_insert {
                self.r.member_slots.push(slot);
            }
        }
    }

    fn new_inst(&mut self, info: StructId, buf: BufPtr, off: usize, index: i64, at: Where) -> InstId {
        self.insts.push(Inst { info, buf, off, index, at });
        self.reg.push(None);
        self.insts.len() - 1
    }

    // ---- the rules' expressions ---------------------------------------------------------

    fn field(&self, inst: &Inst, chain: &[MemberId], idx: &[i64]) -> Result<i64> {
        let mut off = inst.off;
        for &m in &chain[..chain.len() - 1] {
            off = off.wrapping_add(self.member(m).offset as usize);
        }
        let last = *chain.last().ok_or_else(|| Error::new("a rule reads nothing"))?;
        let m = self.member(last);
        let name = &self.s.members[last].name;
        off = off.wrapping_add(m.offset as usize);
        if m.is_pointer() {
            return Ok(i64::from(self.u32_at(&inst.buf, off)?));
        }
        let t = &self.s.defs.types[m.ty];
        let dims: Vec<i64> = m.dims().collect();
        if !idx.is_empty() {
            let mut stride = i64::from(t.size);
            for &d in dims.iter().skip(idx.len()) {
                stride = stride.wrapping_mul(d);
            }
            for (k, &i) in idx.iter().enumerate() {
                let mut sub = stride;
                for &d in dims.iter().take(idx.len()).skip(k + 1) {
                    sub = sub.wrapping_mul(d);
                }
                off = off.wrapping_add(i.wrapping_mul(sub) as usize);
            }
        }
        if t.kind != Kind::Prim && t.kind != Kind::Enum {
            return Err(Error::new(format!("a rule reads {name}, which is not a number")));
        }
        let b = inst.buf.borrow();
        if off.wrapping_add(t.size as usize) > b.b.len() || off > b.b.len() {
            return Err(Error::new(format!("a rule reads past its struct: {name}")));
        }
        let p = &b.b[off..];
        let v = load(p, t.size, self.big);
        let mut value = v as i64;
        if t.is_signed && t.size < 8 && (v >> (t.size * 8 - 1)) & 1 != 0 {
            value = (v | (!0u64 << (t.size * 8))) as i64;
        }
        if t.is_float {
            value = if t.size == 4 {
                truncate(f64::from(load_f32(p, self.big)))
            } else {
                truncate(f64::from_bits(v))
            };
        }
        if m.bits >= 0 {
            value = (value >> m.bitpos) & ((1i64 << m.bits) - 1);
        }
        Ok(value)
    }

    fn lookup(&self, r: &ExprRef, idx: &[i64]) -> Result<i64> {
        let mut found = self.vars[r.info];
        let info_name = &self.s.structs[r.info].name;
        if r.is_index {
            let inst = found.ok_or_else(|| Error::new(format!("a rule needs the index of {info_name}")))?;
            return Ok(self.insts[inst].index);
        }
        // A model surfaces asset listed on its own (not reached through a model's LOD
        // entry) carries its surface count itself; an earlier model's is stale.
        if Some(r.info) == self.lod_info
            && self.asset_type == 3
            && let Some(surfs) = self.surfs_info
            && let Some(own) = self.vars[surfs]
        {
            found = Some(own);
        }
        let inst = &self.insts
            [found.ok_or_else(|| Error::new(format!("a rule needs {info_name}, and none is loaded")))?];
        let mut chain = r.chain.clone();
        if inst.info != r.info {
            chain.clear();
            let mut cur = Some(inst.info);
            for n in &r.names {
                let m = self.s.find_member(cur, n);
                if m.is_empty() {
                    let cur_name = cur.map_or("?", |c| self.s.structs[c].name.as_str());
                    return Err(Error::new(format!("a rule needs {n} of {cur_name}")));
                }
                cur = self.s.members[*m.last().unwrap()].ty;
                chain.extend(m);
            }
        }
        let last = *chain.last().ok_or_else(|| Error::new("a rule reads nothing"))?;
        let m = self.member(last);
        if !idx.is_empty() && m.is_pointer() {
            // Indexing through a pointer: the data loaded for it.
            let name = &self.s.members[last].name;
            let data =
                self.pointed.get(&last).ok_or_else(|| Error::new(format!("a rule needs the data of {name}")))?;
            let t = &self.s.defs.types[m.ty];
            let at = (idx[0] as usize).wrapping_mul(t.size as usize);
            let b = data.borrow();
            if at.wrapping_add(t.size as usize) > b.b.len() || at > b.b.len() {
                return Err(Error::new(format!("a rule reads past {name}")));
            }
            let v = load(&b.b[at..], t.size, self.big);
            if t.is_signed && t.size < 8 && (v >> (t.size * 8 - 1)) & 1 != 0 {
                return Ok((v | (!0u64 << (t.size * 8))) as i64);
            }
            return Ok(v as i64);
        }
        self.field(inst, &chain, idx)
    }

    fn eval(&self, e: Option<ExprId>) -> Result<i64> {
        let e = &self.s.exprs
            [e.ok_or_else(|| Error::new(format!("a count is missing from the rules at {}", self.path_text())))?];
        if let Some(c) = e.constant {
            return Ok(c);
        }
        Ok(e.root.eval(&|r, idx| self.lookup(&e.refs[r], idx).map_err(|e| mw2_zone::Error::new(e.text())), true)?)
    }

    // ---- the load, as the game's Load_X functions do it -------------------------------

    fn block_is(&self, b: Option<BlockId>, kind: &str) -> bool {
        b.is_some_and(|b| self.s.blocks[b].kind == kind)
    }

    fn in_temp(&self, mi: MemberId) -> bool {
        self.block_is(self.s.members[mi].block, "temp")
    }

    fn is_asset(&self, mi: MemberId) -> bool {
        self.s.members[mi].ty.is_some_and(|t| !self.s.structs[t].asset.is_empty())
    }

    fn is_array_at(&self, mi: MemberId, mod_pos: usize) -> bool {
        let m = self.member(mi);
        mod_pos < m.mods.len() && m.mods[mod_pos] != POINTER
    }

    fn align_of(&self, info: StructId) -> u32 {
        let i = &self.s.structs[info];
        if i.alloc_align != 0 { i.alloc_align } else { self.s.defs.types[i.ctype].align }
    }

    fn load_struct(&mut self, info: StructId, given: Option<InstId>, at_start: bool) -> Result<Option<InstId>> {
        let inst = self.base_load_struct(info, given, at_start)?;
        if at_start && !self.writing {
            let made = inst.and_then(|i| self.reg[i]);
            self.set_cap(made);
        }
        Ok(inst)
    }

    fn base_load_struct(&mut self, info: StructId, given: Option<InstId>, at_start: bool) -> Result<Option<InstId>> {
        let s: &'static Schema = self.s;
        let si = &s.structs[info];
        self.path.push(si.name.clone());
        let dynamic = si.dynamic;
        let mut inst = given;
        if at_start && !(si.is_union && dynamic.is_some()) {
            // Up to a dynamic member, the struct streams at once.
            let ctype = self.ctype(info);
            let n = match dynamic {
                Some(d) => self.member(d).offset as usize,
                None => ctype.size() as usize,
            };
            let desc =
                Desc { kind: if dynamic.is_some() { DescKind::Partial } else { DescKind::Type }, t: Some(ctype), count: 1 };
            let (mut data, at) = self.read(n, desc)?;
            if dynamic.is_some() {
                let whole = pieces();
                let len = data.borrow().b.len();
                extend(&whole, &data, 0, len);
                data = whole;
            }
            inst = Some(self.new_inst(info, data, 0, 0, at));
        } else if at_start {
            // A union whose members are only partly known: each streams itself.
            let at = Where { block: self.block, pos: self.block_pos[self.block as usize] };
            inst = Some(self.new_inst(info, pieces(), 0, 0, at));
        }
        let Some(inst) = inst else {
            self.path.pop();
            return Ok(None);
        };
        self.vars[info] = Some(inst);
        self.on_inst(inst, at_start)?;
        let mut pushed = false;
        if !si.asset.is_empty() {
            self.push(VIRTUAL);
            pushed = true;
        } else if let Some(b) = si.block {
            let b = self.plat.block_of(&s.blocks[b].name)?;
            self.push(b);
            pushed = true;
        }
        if si.is_union {
            for &mi in &si.used {
                if !s.needs_treatment(mi) {
                    continue;
                }
                let cond = s.members[mi].condition;
                if cond.is_none() || self.eval(cond)? != 0 {
                    self.vars[info] = Some(inst);
                    self.load_reference(inst, mi, 0, Vec::new())?;
                    break;
                }
            }
        } else {
            for &mi in &si.ordered {
                if !s.needs_treatment(mi) {
                    continue;
                }
                let cond = s.members[mi].condition;
                if cond.is_some() && self.eval(cond)? == 0 {
                    continue;
                }
                self.vars[info] = Some(inst);
                self.load_reference(inst, mi, 0, Vec::new())?;
            }
        }
        if pushed {
            self.pop();
        }
        self.path.pop();
        Ok(Some(inst))
    }

    // ---- members ------------------------------------------------------------------------

    fn member_loc(&self, inst: InstId, mi: MemberId, indices: &[i32]) -> usize {
        let m = self.member(mi);
        let dims: Vec<i64> = m.mods.iter().take_while(|&&x| x != POINTER).map(|&x| i64::from(x)).collect();
        let elem: i64 = if m.has_pointer() { 4 } else { i64::from(self.s.defs.types[m.ty].size) };
        let mut off = self.insts[inst].off.wrapping_add(m.offset as usize);
        for (k, &i) in indices.iter().enumerate() {
            let mut stride = elem;
            for &d in dims.iter().skip(k + 1) {
                stride = stride.wrapping_mul(d);
            }
            off = off.wrapping_add(i64::from(i).wrapping_mul(stride) as usize);
        }
        off
    }

    fn load_reference(&mut self, inst: InstId, mi: MemberId, mod_pos: usize, indices: Vec<i32>) -> Result<()> {
        let s: &'static Schema = self.s;
        let m = self.member(mi);
        let mods = &m.mods;
        let (has_cur, has_next) = (mod_pos < mods.len(), mod_pos + 1 < mods.len());
        let cur = if has_cur { mods[mod_pos] } else { 0 };
        let nxt = if has_next { mods[mod_pos + 1] } else { 0 };
        let (cur_pointer, cur_array) = (has_cur && cur == POINTER, has_cur && cur != POINTER);
        let mut combined: i64 = 0;
        if !indices.is_empty() {
            let dims: Vec<i64> = m.dims().collect();
            for (k, &i) in indices.iter().enumerate() {
                let mut st: i64 = 1;
                for &d in dims.iter().skip(k + 1) {
                    st = st.wrapping_mul(d);
                }
                combined = combined.wrapping_add(i64::from(i).wrapping_mul(st));
            }
        }
        let following_pointer = has_pointer_after(m, mod_pos);
        let mii = &s.members[mi];
        let kind = if cur_array && mii.array_size.contains_key(&mod_pos) {
            LoadKind::Dynamic
        } else if cur_pointer && !s.count_is_array(mi, mod_pos, combined) && !following_pointer {
            LoadKind::Single
        } else if cur_pointer && s.count_is_array(mi, mod_pos, combined) && !following_pointer {
            LoadKind::Array
        } else if ((cur_pointer && s.count_is_array(mi, mod_pos, combined)) || cur_array)
            && has_next
            && nxt == POINTER
            && !s.any_count_is_array(mi, mod_pos + 1)
        {
            LoadKind::PtrArray
        } else if cur_array && !has_next {
            LoadKind::EmbArray
        } else if !has_cur {
            LoadKind::Embedded
        } else if cur_array {
            for k in 0..cur {
                let mut deeper = indices.clone();
                deeper.push(k);
                self.load_reference(inst, mi, mod_pos + 1, deeper)?;
            }
            return Ok(());
        } else {
            return Err(Error::new(format!(
                "an unsupported member: {}::{}",
                s.structs[mii.parent].name, mii.name
            )));
        };
        let at = self.member_loc(inst, mi, &indices);
        let mut name = mii.name.clone();
        for i in &indices {
            name += &format!("[{i}]");
        }
        self.path.push(format!("{name}({})", kind.name()));
        self.ctx.push(Ctx { inst, mi, idx: indices });
        let done = self.load_block(inst, mi, mod_pos, combined, kind, at);
        self.ctx.pop();
        self.path.pop();
        done
    }

    fn load_block(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        at: usize,
    ) -> Result<()> {
        if self.writing { self.write_load_block(inst, mi, mod_pos, combined, kind, at) } else { self.tree_load_block(inst, mi, mod_pos, combined, kind, at) }
    }

    fn base_load_block(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        loc: usize,
    ) -> Result<()> {
        let s: &'static Schema = self.s;
        let mii = &s.members[mi];
        let mut b = mii.block;
        if let Some(bc) = &mii.block_cond {
            b = Some(if self.eval(Some(bc.cond))? != 0 { bc.if_true } else { bc.if_false });
        }
        let push = b.is_some_and(|b| !(s.blocks[b].kind == "normal" && s.blocks[b].is_default));
        if let Some(b) = b
            && push
        {
            let b = self.plat.block_of(&s.blocks[b].name)?;
            self.push(b);
        }
        // A pointer that is null loads nothing (a single string's pointer is looked at
        // where the string loads).
        let check = matches!(kind, LoadKind::Array | LoadKind::PtrArray | LoadKind::Single)
            && !(kind == LoadKind::PtrArray && self.is_array_at(mi, mod_pos))
            && !(kind == LoadKind::Single && mii.is_string);
        let done = if !check {
            self.reuse(inst, mi, mod_pos, combined, kind, loc, None)
        } else {
            match self.u32_at(&self.insts[inst].buf, loc) {
                Ok(0) => Ok(()),
                Ok(val) => self.reuse(inst, mi, mod_pos, combined, kind, loc, Some(val)),
                Err(e) => Err(e),
            }
        };
        if push {
            self.pop();
        }
        done
    }

    #[allow(clippy::too_many_arguments)]
    fn reuse(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        loc: usize,
        val: Option<u32>,
    ) -> Result<()> {
        if self.writing && !self.s.members[mi].is_string && self.st.is_ref(self.w.obj) {
            let obj = self.w.obj.unwrap();
            if let Some(t) = self.forward(Some(obj), self.is_asset(mi)) {
                let slotted = self.st.as_struct(Some(t)).is_some_and(|s| s.slot.is_some());
                let val = if self.in_temp(mi) && slotted { INSERT } else { FOLLOWING };
                let buf = self.insts[inst].buf.clone();
                self.patch(&buf, loc, val);
                self.w.obj = Some(t);
                return self.alloc_member(inst, mi, mod_pos, combined, kind, loc, Some(val));
            }
        }
        let make = matches!(kind, LoadKind::Array | LoadKind::Single | LoadKind::PtrArray)
            && !(kind == LoadKind::PtrArray && self.is_array_at(mi, mod_pos));
        if !make || !self.s.members[mi].is_reusable {
            return self.alloc_member(inst, mi, mod_pos, combined, kind, loc, val);
        }
        let val = match val {
            Some(v) => v,
            None => self.u32_at(&self.insts[inst].buf, loc)?,
        };
        if val == FOLLOWING || (self.in_temp(mi) && val == INSERT) {
            return self.alloc_member(inst, mi, mod_pos, combined, kind, loc, Some(val));
        }
        // A pointer back to what was loaded before: nothing of it is in the stream here.
        let buf = self.insts[inst].buf.clone();
        self.alias(&buf, loc, val)
    }

    #[allow(clippy::too_many_arguments)]
    fn alloc_member(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        loc: usize,
        val: Option<u32>,
    ) -> Result<()> {
        let s: &'static Schema = self.s;
        let mii = &s.members[mi];
        let m = self.member(mi);
        let mut make = matches!(kind, LoadKind::Array | LoadKind::PtrArray | LoadKind::Single);
        if kind == LoadKind::PtrArray {
            make = !self.is_array_at(mi, mod_pos);
        } else if mii.is_string || self.is_asset(mi) {
            make = false;
        }
        if make {
            let align = if mii.alloc_align != 0 {
                mii.alloc_align
            } else if has_pointer_after(m, mod_pos) {
                4
            } else {
                let mut align = match mii.ty {
                    Some(t) => s.defs.types[s.structs[t].ctype].align,
                    None => s.defs.types[m.ty].align,
                };
                if let Some(t) = mii.ty
                    && s.structs[t].alloc_align != 0
                {
                    align = s.structs[t].alloc_align;
                }
                align
            };
            self.alloc(align);
            let val = match val {
                Some(v) => v,
                None => self.u32_at(&self.insts[inst].buf, loc)?,
            };
            if self.in_temp(mi) && val == INSERT {
                self.insert_pointer();
            }
        }
        self.type_check(inst, mi, mod_pos, combined, kind, loc)
    }

    fn elem_size(&self, mi: MemberId, mod_pos: usize) -> usize {
        let m = self.member(mi);
        let mut n = self.s.defs.types[m.ty].size as usize;
        for &x in m.mods.iter().skip(mod_pos + 1) {
            if x == POINTER {
                return 4;
            }
            n = n.wrapping_mul(x as usize);
        }
        n
    }

    fn elem_desc(&self, mi: MemberId, mod_pos: usize, n: i64) -> Desc {
        let m = self.member(mi);
        let mut k: i64 = 1;
        for &x in m.mods.iter().skip(mod_pos + 1) {
            if x == POINTER {
                return Desc { kind: DescKind::Ptrs, t: None, count: n.wrapping_mul(k) };
            }
            k = k.wrapping_mul(i64::from(x));
        }
        Desc { kind: DescKind::Type, t: Some(self.ty(m.ty)), count: n.wrapping_mul(k) }
    }

    fn type_check(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        loc: usize,
    ) -> Result<()> {
        let s: &'static Schema = self.s;
        let mii = &s.members[mi];
        let m = self.member(mi);
        let mods = &m.mods;
        let in_runtime = self.block_is(mii.block, "runtime");
        let buf = self.insts[inst].buf.clone();
        if mii.is_string {
            if kind == LoadKind::Single {
                self.load_xstring(&buf, loc)?;
            } else if kind == LoadKind::PtrArray {
                if mods[mod_pos] != POINTER {
                    self.load_xstring_array(false, i64::from(mods[mod_pos]), Some(buf), loc)?;
                } else {
                    let n = self.eval(s.count_of(mi, mod_pos, combined))?;
                    self.load_xstring_array(true, n, None, 0)?;
                }
            }
            return Ok(());
        }
        if self.is_asset(mi) {
            if kind == LoadKind::Single {
                self.load_asset_ptr(mii.ty.unwrap(), &buf, loc)?;
            } else if kind == LoadKind::PtrArray {
                self.ptr_array(inst, mi, mod_pos, combined, loc)?;
            }
            return Ok(());
        }
        let t = mii.ty;
        let struct_load = t.is_some_and(|t| !s.structs[t].is_leaf);
        match kind {
            LoadKind::Array => {
                let n = self.eval(s.count_of(mi, mod_pos, combined))?;
                if n < 0 {
                    return Err(Error::new(format!("a negative count at {}", self.path_text())));
                }
                if struct_load && !in_runtime {
                    self.load_array(t.unwrap(), true, n, None, 0)?;
                } else {
                    let size = self.elem_size(mi, mod_pos).wrapping_mul(n as usize);
                    let desc = self.elem_desc(mi, mod_pos, n);
                    let (data, _) = self.read(size, desc)?;
                    self.pointed.insert(mi, data);
                }
            }
            LoadKind::Single => {
                if struct_load && !in_runtime {
                    self.load_struct(t.unwrap(), None, true)?;
                } else {
                    let size = self.elem_size(mi, mod_pos);
                    let desc = self.elem_desc(mi, mod_pos, 1);
                    self.read(size, desc)?;
                }
            }
            LoadKind::PtrArray => self.ptr_array(inst, mi, mod_pos, combined, loc)?,
            LoadKind::EmbArray => {
                let n = i64::from(mods[mod_pos]);
                let after = s.after_partial_load(mi);
                if let Some(t) = t.filter(|_| !mii.is_leaf) {
                    if after {
                        self.load_array(t, true, n, None, 0)?;
                    } else {
                        self.load_array(t, false, n, Some(buf), loc)?;
                    }
                } else if after {
                    let et = match t {
                        Some(t) => self.ctype(t),
                        None => self.ty(m.ty),
                    };
                    self.read((et.size() as usize).wrapping_mul(n as usize), Desc {
                        kind: DescKind::Type,
                        t: Some(et),
                        count: n,
                    })?;
                }
            }
            LoadKind::Embedded => {
                let after = s.after_partial_load(mi);
                if let Some(t) = t.filter(|_| !mii.is_leaf) {
                    if after {
                        let sub = self.load_struct(t, None, true)?;
                        if buf.borrow().pieces
                            && let Some(sub) = sub
                        {
                            let sub_buf = self.insts[sub].buf.clone();
                            if sub_buf.borrow().pieces {
                                let len = sub_buf.borrow().b.len();
                                extend(&buf, &sub_buf, 0, len);
                            } else {
                                let size = self.ctype(t).size() as usize;
                                extend(&buf, &sub_buf, self.insts[sub].off, size);
                            }
                        }
                    } else {
                        // A struct wrapping another at offset 0 (GfxCellTree128 around
                        // GfxCellTree) keeps its array index, for counts written as
                        // pointer differences.
                        let i = &self.insts[inst];
                        let index = if loc == i.off { i.index } else { 0 };
                        let at = i.at;
                        let sub = self.new_inst(t, buf, loc, index, at);
                        self.load_struct(t, Some(sub), false)?;
                    }
                } else if after {
                    let et = match t {
                        Some(t) => self.ctype(t),
                        None => self.ty(m.ty),
                    };
                    let (data, _) =
                        self.read(et.size() as usize, Desc { kind: DescKind::Type, t: Some(et), count: 1 })?;
                    if buf.borrow().pieces {
                        let len = data.borrow().b.len();
                        extend(&buf, &data, 0, len);
                    }
                }
            }
            LoadKind::Dynamic => {
                let n = self.eval(mii.array_size.get(&mod_pos).copied())?;
                if n < 0 {
                    return Err(Error::new(format!("a negative size at {}", self.path_text())));
                }
                let data = if struct_load {
                    self.load_array(t.unwrap(), true, n, None, 0)?
                } else {
                    let mut elem = self.s.defs.types[m.ty].size as usize;
                    for &x in mods.iter().skip(1) {
                        elem = elem.wrapping_mul(x as usize);
                    }
                    let desc = self.elem_desc(mi, 0, n);
                    self.read(elem.wrapping_mul(n as usize), desc)?.0
                };
                if buf.borrow().pieces {
                    let len = data.borrow().b.len();
                    extend(&buf, &data, 0, len);
                }
            }
        }
        Ok(())
    }

    fn ptr_array(&mut self, inst: InstId, mi: MemberId, mod_pos: usize, combined: i64, loc: usize) -> Result<()> {
        let cur = self.member(mi).mods[mod_pos];
        if cur != POINTER {
            let buf = self.insts[inst].buf.clone();
            self.load_ptr_array(mi, false, i64::from(cur), Some(buf), loc)
        } else {
            let n = self.eval(self.s.count_of(mi, mod_pos, combined))?;
            self.load_ptr_array(mi, true, n, None, 0)
        }
    }

    // The node of the struct an instance being loaded inside another one is (ctx's last
    // member), or element `k` of that member's array.
    fn member_node(&mut self, k: i64) -> Result<NodeId> {
        let c = self.ctx.last().ok_or_else(|| Error::new("a struct loaded inside nothing"))?;
        let parent_inst = c.inst;
        let parent = self.reg[parent_inst].ok_or_else(|| Error::new("a struct loaded inside nothing kept"))?;
        let info = self.insts[parent_inst].info;
        let m = self.member(c.mi);
        let idx = c.idx.clone();
        member_struct(self.st, parent, self.s, info, m, &idx, k)
    }

    fn on_inst(&mut self, inst: InstId, at_start: bool) -> Result<()> {
        if self.reg[inst].is_some() {
            return Ok(());
        }
        if self.writing {
            if at_start {
                let s = self.w.last.filter(|&l| self.st.is_struct(Some(l)));
                let s = s.ok_or_else(|| Error::new(format!("a struct is something else at {}", self.path_text())))?;
                self.reg[inst] = Some(s);
                return Ok(());
            }
            let n = self.member_node(-1)?;
            self.reg[inst] = Some(n);
            return Ok(());
        }
        if at_start {
            let (info, buf, off, at) = {
                let i = &self.insts[inst];
                (i.info, i.buf.clone(), i.off, i.at)
            };
            let ctype = self.ctype(info);
            let b = buf.borrow();
            if b.pieces && b.b.is_empty() {
                if !self.r.lenient {
                    return Err(Error::new(format!(
                        "{}: unions streamed member by member aren't supported yet",
                        self.s.structs[info].name
                    )));
                }
                // Only for files looked things up in, never written (an animation's
                // translation data): the walk stays in step, the union isn't kept.
                drop(b);
                let node = self.st.make_struct(ctype, self.big, Vec::new());
                self.reg[inst] = Some(node);
                return Ok(());
            }
            let raw = b.b[off.min(b.b.len())..].to_vec();
            drop(b);
            let n = raw.len() as u32;
            let node = self.st.make_struct(ctype, self.big, raw);
            self.r.placed.push(Placed { block: at.block, off: at.pos, n, node, t: ElemType { t: Some(ctype), size: 0 } });
            self.reg[inst] = Some(node);
            return Ok(());
        }
        let n = self.member_node(-1)?;
        self.reg[inst] = Some(n);
        Ok(())
    }

    fn alias(&mut self, buf: &BufPtr, at: usize, val: u32) -> Result<()> {
        if self.writing {
            let Some(o) = self.w.obj else { return Ok(()) };
            if self.st.is_ref(Some(o)) {
                let val = self.ref_value(o, true)?;
                let zp = buf.borrow().zone_at(at);
                if zp >= 0 {
                    store(&mut self.zone[zp as usize..], 4, u64::from(val), self.big);
                }
            }
            return Ok(());
        }
        let r = self.st.make_ref(val);
        self.r.refs.push(r);
        self.set_cap(Some(r));
        Ok(())
    }

    fn load_array(
        &mut self,
        info: StructId,
        at_start: bool,
        count: i64,
        buf: Option<BufPtr>,
        base: usize,
    ) -> Result<BufPtr> {
        if count < 0 {
            return Err(Error::new(format!(
                "a negative count of {} at {}",
                self.s.structs[info].name,
                self.path_text()
            )));
        }
        let ctype = self.ctype(info);
        let size = ctype.size() as usize;
        let mut at = Where::default();
        let mut base = base;
        let mut list = None;
        let buf = if at_start {
            let total = size.wrapping_mul(count as usize);
            let desc = Desc { kind: DescKind::Type, t: Some(ctype), count };
            let (buf, w) = if self.writing {
                list = self.w.obj.filter(|&o| self.st.is_array(Some(o)));
                let read = self.write_read(total, desc)?;
                if list.is_none() && count != 0 {
                    return Err(Error::new(format!("an array is something else at {}", self.path_text())));
                }
                read
            } else {
                let read = self.read_zone(total)?;
                let items: Vec<Option<NodeId>> = {
                    let b = read.0.borrow();
                    (0..count as usize)
                        .map(|k| Some(self.st.make_struct(ctype, self.big, b.b[k * size..(k + 1) * size].to_vec())))
                        .collect()
                };
                let l = self.st.make_array(items);
                self.r.placed.push(Placed {
                    block: read.1.block,
                    off: read.1.pos,
                    n: total as u32,
                    node: l,
                    t: ElemType { t: Some(ctype), size: 0 },
                });
                list = Some(l);
                read
            };
            at = w;
            base = 0;
            buf
        } else {
            buf.ok_or_else(|| Error::new("an embedded array without its struct"))?
        };
        for k in 0..count {
            let e = self.new_inst(info, buf.clone(), base.wrapping_add(k as usize * size), k, at);
            let node = match list {
                Some(l) => {
                    let item = self.st.array(l).items.get(k as usize).copied().flatten();
                    match item.filter(|&i| self.st.is_struct(Some(i))) {
                        Some(i) => i,
                        None => {
                            return Err(Error::new(format!(
                                "an array item is something else at {}",
                                self.path_text()
                            )));
                        }
                    }
                }
                None => self.member_node(k)?,
            };
            self.reg[e] = Some(node);
            self.vars[info] = Some(e);
            self.base_load_struct(info, Some(e), false)?;
        }
        if !self.writing {
            self.set_cap(list);
        }
        Ok(buf)
    }

    fn load_ptr_array(
        &mut self,
        mi: MemberId,
        at_start: bool,
        count: i64,
        buf: Option<BufPtr>,
        loc: usize,
    ) -> Result<()> {
        if count < 0 {
            return Err(Error::new(format!("a negative count at {}", self.path_text())));
        }
        if self.writing { self.write_load_ptr_array(mi, at_start, count, buf, loc) } else { self.tree_load_ptr_array(mi, at_start, count, buf, loc) }
    }

    fn load_xstring(&mut self, buf: &BufPtr, loc: usize) -> Result<()> {
        if self.writing { self.write_load_xstring(buf, loc) } else { self.tree_load_xstring(buf, loc) }
    }

    fn load_xstring_array(&mut self, at_start: bool, count: i64, buf: Option<BufPtr>, loc: usize) -> Result<()> {
        if count < 0 {
            return Err(Error::new(format!("a negative count at {}", self.path_text())));
        }
        if self.writing {
            self.write_load_xstring_array(at_start, count, buf, loc)
        } else {
            self.tree_load_xstring_array(at_start, count, buf, loc)
        }
    }

    fn load_asset_ptr(&mut self, info: StructId, buf: &BufPtr, loc: usize) -> Result<Option<InstId>> {
        if self.writing { self.write_load_asset_ptr(info, buf, loc) } else { self.tree_load_asset_ptr(info, buf, loc) }
    }
}

/// The struct an instance being loaded inside another one is: the member `m` (at `idx`)
/// of `parent`, or element `k` of that member's array.
fn member_struct(
    st: &mut Store,
    parent: NodeId,
    s: &'static Schema,
    parent_info: StructId,
    m: &'static Member,
    idx: &[i32],
    k: i64,
) -> Result<NodeId> {
    // A typedef's one member is the typedef itself.
    if s.defs.types[s.structs[parent_info].ctype].kind == Kind::Typedef {
        return Ok(parent);
    }
    // A union notes which of its members a walk goes into: that is the one in use.
    {
        let p = st.st_mut(parent);
        if p.ty.kind() == Kind::Union && !p.used.contains(&m.name.as_str()) {
            p.used.push(&m.name);
        }
    }
    let mut full = idx.to_vec();
    if k >= 0 {
        full.push(k as i32);
    }
    if full.is_empty() {
        return st.sub_m(parent, m, -1);
    }
    let dims: Vec<i64> = m.dims().collect();
    let mut flat: i64 = 0;
    for (d, &f) in full.iter().enumerate() {
        let mut stride: i64 = 1;
        for &e in dims.iter().skip(d + 1) {
            stride = stride.wrapping_mul(e);
        }
        flat = flat.wrapping_add(i64::from(f).wrapping_mul(stride));
    }
    st.sub_m(parent, m, flat)
}

// ---- reading a tree ---------------------------------------------------------------------------

impl Walk<'_> {
    fn tree_read(&mut self, n: usize, desc: Desc) -> Result<(BufPtr, Where)> {
        let (data, at) = self.read_zone(n)?;
        if desc.kind == DescKind::Type {
            // Plain data, if that is what is loading: made into a Leaf only when a load
            // keeps it (most typed reads are structs).
            self.r.cap = None;
            self.r.leaf_data = Some(data.clone());
            self.r.leaf_desc = Some(desc);
            self.r.cap_where = at;
        }
        Ok((data, at))
    }

    fn set_cap(&mut self, v: Option<NodeId>) {
        self.r.cap = v;
        self.r.leaf_data = None;
    }

    fn made(&mut self) -> Option<NodeId> {
        if let Some(data) = self.r.leaf_data.take() {
            let desc = self.r.leaf_desc.unwrap_or(Desc::of(DescKind::Type));
            let raw = data.borrow().b.clone();
            self.r.cap = Some(self.st.make_leaf(desc.t, desc.count, raw, self.big));
        }
        self.r.cap
    }

    fn tree_load_block(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        at: usize,
    ) -> Result<()> {
        self.set_cap(None);
        let depth = self.r.member_slots.len();
        self.base_load_block(inst, mi, mod_pos, combined, kind, at)?;
        let made = self.made();
        self.set_cap(None);
        if self.r.member_slots.len() > depth {
            // This member had the loader make a pointer to what it holds
            // (XModelLodInfo.modelSurfs): later pointers to it point there.
            let s = self.r.member_slots.pop().unwrap();
            if let Some(made) = made {
                let mark = self.st.make_slot(Some(made));
                if let Some(d) = self.st.as_struct_mut(Some(made)) {
                    d.slot = Some(mark);
                }
                self.r.placed.push(Placed { block: VIRTUAL, off: s, n: 4, node: mark, t: ElemType { t: None, size: 4 } });
            }
        }
        let Some(made) = made else { return Ok(()) };
        if let Some(lf) = self.st.as_leaf(Some(made)) {
            let (n, t) = (lf.raw.len() as u32, lf.t);
            let w = self.r.cap_where;
            self.r.placed.push(Placed { block: w.block, off: w.pos, n, node: made, t: ElemType { t, size: 0 } });
        }
        let d = self.reg[inst].ok_or_else(|| Error::new("a member of nothing kept"))?;
        let idx = self.ctx.last().map(|c| c.idx.clone()).unwrap_or_default();
        let name: &'static str = &self.member(mi).name;
        if matches!(kind, LoadKind::Embedded | LoadKind::EmbArray | LoadKind::Dynamic) {
            let has_children = self.st.ch(d).is_some_and(|c| !c.is_empty());
            let s = self.st.st_mut(d);
            if s.lates().is_empty() && has_children {
                s.lates_after_pointers = true;
            }
            s.set_late(name, Some(made), &idx);
        } else {
            self.st.set_child_at(d, name, Some(made), &idx);
        }
        Ok(())
    }

    fn tree_load_ptr_array(
        &mut self,
        mi: MemberId,
        at_start: bool,
        count: i64,
        buf: Option<BufPtr>,
        loc: usize,
    ) -> Result<()> {
        let s: &'static Schema = self.s;
        let mut at = Where::default();
        let (buf, loc) = if at_start {
            let (b, w) = self.read_zone(4usize.wrapping_mul(count as usize))?;
            at = w;
            (b, 0)
        } else {
            (buf.ok_or_else(|| Error::new("a pointer array without its struct"))?, loc)
        };
        let mut raw = Vec::with_capacity(count as usize);
        for k in 0..count as usize {
            raw.push(self.u32_at(&buf, loc + 4 * k)?);
        }
        let pl = self.st.make_ptr_list(Vec::new(), Some(raw));
        if at_start {
            self.r.placed.push(Placed {
                block: at.block,
                off: at.pos,
                n: (4 * count) as u32,
                node: pl,
                t: ElemType { t: None, size: 4 },
            });
        }
        let t = s.members[mi].ty;
        let m = self.member(mi);
        for k in 0..count as usize {
            let p = loc + 4 * k;
            let val = self.u32_at(&buf, p)?;
            if val == 0 {
                self.st.ptr_list_mut(pl).items.push(None);
                continue;
            }
            if let Some(t) = t
                && !s.structs[t].asset.is_empty()
            {
                self.set_cap(None);
                self.tree_load_asset_ptr(t, &buf, p)?;
                let item = self.made();
                self.st.ptr_list_mut(pl).items.push(item);
                continue;
            }
            if s.members[mi].is_reusable && val != FOLLOWING {
                self.alias(&buf, p, val)?;
                let item = self.made();
                self.st.ptr_list_mut(pl).items.push(item);
                continue;
            }
            let align = match t {
                Some(t) => self.align_of(t),
                None => s.defs.types[m.ty].align,
            };
            self.alloc(align);
            if let Some(t) = t
                && !s.structs[t].is_leaf
            {
                let inst = self.load_struct(t, None, true)?;
                let item = inst.and_then(|i| self.reg[i]);
                self.st.ptr_list_mut(pl).items.push(item);
            } else {
                let et = match t {
                    Some(t) => self.ctype(t),
                    None => self.ty(m.ty),
                };
                self.read(et.size() as usize, Desc { kind: DescKind::Type, t: Some(et), count: 1 })?;
                let lf = self.made().ok_or_else(|| Error::new("plain data that wasn't read"))?;
                let n = self.st.leaf(lf).raw.len() as u32;
                let w = self.r.cap_where;
                self.r.placed.push(Placed { block: w.block, off: w.pos, n, node: lf, t: ElemType { t: Some(et), size: 0 } });
                self.st.ptr_list_mut(pl).items.push(Some(lf));
            }
        }
        self.set_cap(Some(pl));
        Ok(())
    }

    fn tree_load_xstring(&mut self, buf: &BufPtr, loc: usize) -> Result<()> {
        let val = self.u32_at(buf, loc)?;
        if val == 0 {
            self.set_cap(None);
            return Ok(());
        }
        if val != FOLLOWING {
            return self.alias(buf, loc, val);
        }
        let end = self.zone[self.pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::new("a string runs off the end of the zone"))?;
        let (data, at) = self.read_zone(end + 1)?;
        let b = data.borrow();
        let s = self.st.make_str(text::of(&b.b[..b.b.len() - 1]));
        self.r.placed.push(Placed { block: at.block, off: at.pos, n: b.b.len() as u32, node: s, t: ElemType { t: None, size: 1 } });
        drop(b);
        self.set_cap(Some(s));
        Ok(())
    }

    fn tree_load_xstring_array(&mut self, at_start: bool, count: i64, buf: Option<BufPtr>, loc: usize) -> Result<()> {
        let mut at = Where::default();
        let (buf, loc) = if at_start {
            let (b, w) = self.read_zone(4usize.wrapping_mul(count as usize))?;
            at = w;
            (b, 0)
        } else {
            (buf.ok_or_else(|| Error::new("a string array without its struct"))?, loc)
        };
        let mut raw = Vec::with_capacity(count as usize);
        for k in 0..count as usize {
            raw.push(self.u32_at(&buf, loc + 4 * k)?);
        }
        let pl = self.st.make_ptr_list(Vec::new(), Some(raw));
        if at_start {
            self.r.placed.push(Placed {
                block: at.block,
                off: at.pos,
                n: (4 * count) as u32,
                node: pl,
                t: ElemType { t: None, size: 4 },
            });
        }
        for k in 0..count as usize {
            self.tree_load_xstring(&buf, loc + 4 * k)?;
            let item = self.made();
            self.st.ptr_list_mut(pl).items.push(item);
        }
        self.set_cap(Some(pl));
        Ok(())
    }

    fn tree_load_asset_ptr(&mut self, info: StructId, buf: &BufPtr, loc: usize) -> Result<Option<InstId>> {
        let s: &'static Schema = self.s;
        let val = self.u32_at(buf, loc)?;
        let in_temp = self.block_is(s.structs[info].block, "temp");
        if in_temp {
            self.push(TEMP);
        }
        let mut inst = None;
        let mut res = None;
        if val != 0 {
            if val == FOLLOWING || (in_temp && val == INSERT) {
                self.alloc(self.align_of(info));
                let mut slot = None;
                if in_temp && val == INSERT {
                    self.r.asset_insert = true;
                    self.insert_pointer();
                    self.r.asset_insert = false;
                    slot = Some(self.r.slot);
                }
                let start = self.pos;
                inst = self.load_struct(info, None, true)?;
                let d = inst.and_then(|i| self.reg[i]).ok_or_else(|| Error::new("an asset that wasn't kept"))?;
                self.r.asset_starts.push((d, start));
                self.st.st_mut(d).asset = &s.structs[info].name;
                if let Some(slot) = slot {
                    let mark = self.st.make_slot(Some(d));
                    self.st.st_mut(d).slot = Some(mark);
                    self.r.placed.push(Placed { block: VIRTUAL, off: slot, n: 4, node: mark, t: ElemType { t: None, size: 4 } });
                }
                res = Some(d);
            } else {
                self.alias(buf, loc, val)?;
                res = self.made();
            }
        }
        if in_temp {
            self.pop();
        }
        self.set_cap(res);
        Ok(inst)
    }

    fn walk_tree(&mut self) -> Result<Tree> {
        let s: &'static Schema = self.s;
        let mut tree = Tree { platform: self.plat.name.to_string(), ..Tree::default() };
        let header = self.plat.header_bytes();
        if self.zone.len() < header + 16 {
            return Err(Error::new("the zone is cut short"));
        }
        tree.header = self.zone[..header].to_vec();
        let p = self.pos;
        let ss_count = load(&self.zone[p..], 4, self.big) as u32;
        let ss_ptr = load(&self.zone[p + 4..], 4, self.big) as u32;
        let count = load(&self.zone[p + 8..], 4, self.big) as u32;
        let list = load(&self.zone[p + 12..], 4, self.big) as u32;
        self.pos += 16;
        self.push(VIRTUAL);
        if ss_ptr != 0 {
            self.alloc(4);
            self.tree_load_xstring_array(true, i64::from(ss_count), None, 0)?;
            tree.script_strings = self.made().filter(|&m| self.st.is_ptr_list(Some(m)));
        }
        self.pop();
        self.push(VIRTUAL);
        if list != 0 {
            self.alloc(4);
            let (buf, at) = self.read_zone(8 * count as usize)?;
            let mut types = Vec::with_capacity(count as usize);
            for k in 0..count as usize {
                let t = self.u32_at(&buf, 8 * k)?;
                if t as usize >= self.plat.types.len() {
                    return Err(Error::new(format!("asset {k} has an unknown type {t}")));
                }
                types.push(t);
                let e = self.st.make_entry(self.plat.types[t as usize], None);
                tree.assets.push(e);
                self.r.placed.push(Placed {
                    block: at.block,
                    off: at.pos.wrapping_add(8 * k as u32),
                    n: 8,
                    node: e,
                    t: ElemType { t: None, size: 8 },
                });
            }
            for (k, &t) in types.iter().enumerate() {
                let name = self.plat.structs.get(&t).ok_or_else(|| {
                    Error::new(format!("asset {k} has an unsupported type ({})", self.plat.types[t as usize]))
                })?;
                let info = *s.assets.get(*name).ok_or_else(|| Error::new(format!("the rules have no {name}")))?;
                self.asset_type = t as i32;
                self.set_cap(None);
                self.tree_load_asset_ptr(info, &buf, 8 * k + 4)?;
                let asset = self.made();
                self.st.entry_mut(tree.assets[k]).asset = asset;
            }
        }
        self.pop();
        self.resolve();
        Ok(tree)
    }

    // Every alias pointed at the object it refers into.
    fn resolve(&mut self) {
        let mut by_block: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
        for (i, p) in self.r.placed.iter().enumerate() {
            if p.n != 0 {
                by_block.entry(p.block).or_default().push(i);
            }
        }
        let placed = &self.r.placed;
        for rows in by_block.values_mut() {
            rows.sort_by_key(|&i| placed[i].off);
        }
        for &r in &self.r.refs {
            let val = self.st.reference(r).val;
            let raw = val.wrapping_sub(1);
            let b = (raw >> BLOCK_SHIFT) as i32;
            let off = raw & OFFSET_MASK;
            let Some(rows) = by_block.get(&b) else { continue };
            let mut i = rows.partition_point(|&p| placed[p].off <= off) as i64 - 1;
            // Several things can start at one place (a struct and its first member's
            // array): the last one that holds the offset.
            while i >= 0 {
                let p = &placed[rows[i as usize]];
                if p.off <= off && off < p.off.wrapping_add(p.n) {
                    let x = self.st.reference_mut(r);
                    x.target = Some(p.node);
                    x.rel = i64::from(off - p.off);
                    x.t = p.t;
                    break;
                }
                if p.off.wrapping_add(p.n) <= off && i > 0 {
                    let q = &placed[rows[i as usize - 1]];
                    if q.off.wrapping_add(q.n) <= p.off {
                        break;
                    }
                }
                i -= 1;
            }
        }
    }
}

/// A zone's tree, checked to have been read to its last byte, and every asset loaded from
/// the asset list or a pointer: (its node, where it starts in the zone). `lenient`: a
/// union streamed member by member (an animation's data, in a file only looked things up
/// in) is passed over, not kept.
pub fn read_tree(
    st: &mut Store,
    schema: &'static Schema,
    plat: &'static Platform,
    zone: Vec<u8>,
    lenient: bool,
) -> Result<(Tree, Vec<(NodeId, usize)>)> {
    let mut w = Walk::new(st, schema, plat, zone);
    w.r.lenient = lenient;
    let tree = w.walk_tree()?;
    if w.pos != w.zone.len() {
        return Err(Error::new(format!("the walk stopped at {} of {} bytes", w.pos, w.zone.len())));
    }
    Ok((tree, std::mem::take(&mut w.r.asset_starts)))
}

// ---- writing a tree ---------------------------------------------------------------------------

/// How a tree is written.
#[derive(Default)]
pub struct WriteOptions<'a> {
    /// Where a Ref into an object of another layout points once converted; by default
    /// where it pointed.
    pub map_rel: Option<&'a MapRel>,
    /// An asset met again after it is written becomes a pointer to the pointer that first
    /// held it, which the game sets to it.
    pub dedupe: bool,
    /// Each struct is written from its members, padding and unused bits zero, as
    /// converted files are written; with keep_bytes, as its bytes are (a tree written back
    /// as it was read gives the zone back).
    pub keep_bytes: bool,
}

/// A tree written: the zone, and where each asset was written, and every asset written,
/// in order.
pub struct Written {
    pub zone: Vec<u8>,
    pub deduped: usize,
    pub starts: HashMap<NodeId, usize>,
    pub written: Vec<(usize, NodeId)>,
}

/// The zone of a tree. `header`: the original zone's header (of this platform), whose
/// temp and callback sizes are kept when bigger.
pub fn write_tree(
    st: &mut Store,
    tree: &Tree,
    schema: &'static Schema,
    plat: &'static Platform,
    options: &WriteOptions,
    header: Option<&[u8]>,
) -> Result<Written> {
    // (The zone written starts as its header's room; the walk stands after it.)
    let mut w = Walk::new(st, schema, plat, vec![0; plat.header_bytes()]);
    w.writing = true;
    w.w.tree = Some(tree);
    w.w.map_rel = options.map_rel;
    w.w.dedupe = options.dedupe;
    w.w.keep_bytes = options.keep_bytes;
    let zone = w.write(tree, header)?;
    Ok(Written {
        zone,
        deduped: w.w.deduped,
        starts: std::mem::take(&mut w.w.starts),
        written: std::mem::take(&mut w.w.written),
    })
}

impl Walk<'_> {
    // ---- pointers -----------------------------------------------------------------------

    // Where an alias's target is now. A target written after the pointer (a struct's
    // pointers stream before what they point at) keeps its value here and gets the real
    // one in alias, when the walk reaches the pointer.
    fn ref_value(&self, r: NodeId, strict: bool) -> Result<u32> {
        let x = self.st.reference(r);
        let target = x.target.ok_or_else(|| Error::new("a pointer points at something that isn't in the file"))?;
        let Some(at) = self.w.loc.get(&target) else {
            if strict {
                return Err(Error::new(format!(
                    "a pointer refers to something before it is written, at {}",
                    self.path_text()
                )));
            }
            return Ok(x.val);
        };
        let rel = match self.w.map_rel {
            Some(f) => f(x)?,
            None => x.rel,
        };
        Ok(((at.block as u32) << BLOCK_SHIFT | (i64::from(at.pos).wrapping_add(rel) as u32)).wrapping_add(1))
    }

    // What to write in place of alias r when its target isn't written yet (it comes later
    // in this file's order): the target itself, else None.
    fn forward(&self, node: Option<NodeId>, asset: bool) -> Option<NodeId> {
        let r = self.st.as_ref(node)?;
        let target = r.target?;
        let loc = &self.w.loc;
        if asset && let Some(a) = query::slot_asset(self.st, node.unwrap()) {
            if let Some(ar) = self.st.as_ref(Some(a)) {
                return if ar.target.is_some_and(|t| loc.contains_key(&t)) { None } else { self.forward(Some(a), true) };
            }
            // A temp asset with its own slot is written where it sits, unless nothing holds
            // it any more but pointers ("forward": its first pointer takes it).
            let s = self.st.as_struct(Some(a));
            return if !loc.contains_key(&a) && s.is_none_or(|s| s.slot.is_none() || s.forward) { Some(a) } else { None };
        }
        if loc.contains_key(&target) || r.rel != 0 {
            return None;
        }
        if let Some(slotted) = self.st.as_slot(Some(target)) {
            // A member pointing at another asset's slot (a model's surfaces): that asset,
            // if nothing but pointers hold it.
            let a = slotted.asset.filter(|&a| self.st.as_struct(Some(a)).is_some_and(|s| s.forward));
            return a.filter(|a| !loc.contains_key(a));
        }
        Some(target)
    }

    fn address(&self, buf: &BufPtr, at: usize) -> Option<Where> {
        let zp = buf.borrow().zone_at(at);
        if zp < 0 {
            return None;
        }
        let zp = zp as usize;
        let i = self.w.zone_positions.partition_point(|&p| p <= zp);
        if i == 0 {
            return None;
        }
        let i = i - 1;
        let span = self.w.zone_spans[i];
        if zp >= self.w.zone_positions[i] + span[2] as usize {
            return None;
        }
        Some(Where { block: span[0] as i32, pos: span[1].wrapping_add((zp - self.w.zone_positions[i]) as u32) })
    }

    fn patch(&mut self, buf: &BufPtr, at: usize, val: u32) {
        let mut b = buf.borrow_mut();
        if at + 4 <= b.b.len() {
            store(&mut b.b[at..], 4, u64::from(val), self.big);
        }
        let zp = b.zone_at(at);
        if zp >= 0 {
            store(&mut self.zone[zp as usize..], 4, u64::from(val), self.big);
        }
    }

    // ---- encoding -----------------------------------------------------------------------

    fn has_pointers(&mut self, t: Ty) -> bool {
        if !t.compound() {
            return false;
        }
        if let Some(&found) = self.w.pointer_types.get(&t) {
            return found;
        }
        self.w.pointer_types.insert(t, false);
        let mut any = false;
        for m in t.members() {
            if m.has_pointer() || self.has_pointers(t.of(m)) {
                any = true;
            }
        }
        self.w.pointer_types.insert(t, any);
        any
    }

    // `n` bytes of a struct's data from `off`.
    fn src_bytes(&self, s: NodeId, off: usize, n: usize) -> Result<&[u8]> {
        self.st.data_from(s, off, n).ok_or_else(|| {
            Error::new(format!("{} is read past its bytes at {}", self.st.st(s).ty.name(), self.path_text()))
        })
    }

    // A struct's bytes as its members say them, its pointers back to earlier data set to
    // where their targets now are.
    fn encode_struct(&mut self, s: NodeId, out: &mut [u8], size: usize, fix: bool) -> Result<()> {
        let t = self.st.st(s).ty;
        if t.kind() == Kind::Union {
            return self.encode_union(s, out);
        }
        let big = self.big;
        let fix = fix && self.has_pointers(t);
        let ch = if fix { self.st.st(s).ch() } else { None };
        if self.w.keep_bytes {
            let data = self.st.data(s);
            let n = size.min(data.len()).min(out.len());
            out[..n].copy_from_slice(&data[..n]);
        }
        for m in t.members() {
            let off = m.offset as usize;
            if off + m.size as usize > size {
                break;
            }
            if m.is_pointer() {
                let mut v = load(self.src_bytes(s, off, 4)?, 4, big) as u32;
                if let Some(c) = ch
                    && let Some(r) = self.st.children(c).get(&m.name, &[])
                    && self.st.is_ref(Some(r))
                {
                    v = self.ref_value(r, false)?;
                }
                store(&mut out[off..], 4, u64::from(v), big);
                continue;
            }
            if m.has_pointer() {
                // An array of pointers: each element's own pointer, or the list's, when it
                // is a Ref.
                let n = elements(m);
                let list = ch.and_then(|c| self.st.children(c).get(&m.name, &[]));
                for k in 0..n {
                    let mut v = load(self.src_bytes(s, off + 4 * k, 4)?, 4, big) as u32;
                    if let Some(c) = ch {
                        let mut item = self.st.children(c).get(&m.name, &[k as i32]);
                        if item.is_none() {
                            if let Some(pl) = self.st.as_ptr_list(list) {
                                item = pl.items.get(k).copied().flatten();
                            } else if let Some(a) = self.st.as_array(list) {
                                item = a.items.get(k).copied().flatten();
                            }
                        }
                        if self.st.is_ref(item) {
                            v = self.ref_value(item.unwrap(), false)?;
                        }
                    }
                    store(&mut out[off + 4 * k..], 4, u64::from(v), big);
                }
                continue;
            }
            if m.bits >= 0 {
                let mask = ((1u64 << m.bits) - 1) << m.bitpos;
                let v = load(self.src_bytes(s, off, m.size as usize)?, m.size, big) & mask;
                let o = load(&out[off..], m.size, big);
                store(&mut out[off..], m.size, (o & !mask) | v, big);
                continue;
            }
            let mt = t.of(m).unwrapped();
            if mt.is_record() {
                // A typedef around a struct is written whole, as its bytes: its pointers are
                // not looked at.
                let wrapped = t.of(m).kind() == Kind::Typedef;
                let n = elements(m);
                let es = mt.size() as usize;
                for k in 0..n {
                    let sub = self.st.sub_m(s, m, if m.mods.is_empty() { -1 } else { k as i64 })?;
                    let at = off + k * es;
                    self.encode_struct(sub, &mut out[at..at + es], es, fix && !wrapped)?;
                }
                continue;
            }
            out[off..off + m.size as usize].copy_from_slice(self.src_bytes(s, off, m.size as usize)?);
        }
        Ok(())
    }

    fn encode_union(&mut self, s: NodeId, out: &mut [u8]) -> Result<()> {
        let data = self.st.data(s).to_vec();
        let n = data.len().min(out.len());
        out[..n].copy_from_slice(&data[..n]);
        let t = self.st.st(s).ty;
        let Some(c) = self.st.st(s).ch() else { return Ok(()) };
        if !self.has_pointers(t) {
            return Ok(());
        }
        let big = self.big;
        for m in t.members() {
            if !m.is_pointer() {
                continue;
            }
            let off = m.offset as usize;
            let item = self.st.children(c).get(&m.name, &[]);
            if self.st.is_ref(item) {
                let v = self.ref_value(item.unwrap(), false)?;
                store(&mut out[off..], 4, u64::from(v), big);
            } else if let Some(item) = item {
                // The data itself is here (the converter put what an alias meant in its
                // place): the pointer must say so, in the union itself, which the walk goes
                // on to load from.
                let cur = load(&out[off..], 4, big) as u32;
                if cur != 0 && cur != FOLLOWING && cur != INSERT {
                    let slotted = self.st.as_struct(Some(item)).is_some_and(|x| x.slot.is_some());
                    let mark = if slotted { INSERT } else { FOLLOWING };
                    store(&mut out[off..], 4, u64::from(mark), big);
                    if let Some(d) = self.st.data_mut(s).get_mut(off..off + 4) {
                        store(d, 4, u64::from(mark), big);
                    }
                }
            }
        }
        Ok(())
    }

    fn ptr_raw(&self, pl: NodeId, i: usize) -> Result<u32> {
        let p = self.st.ptr_list(pl);
        let c = p.items[i];
        if self.st.is_ref(c) {
            return self.ref_value(c.unwrap(), false);
        }
        if let Some(raw) = &p.raw
            && i < raw.len()
        {
            return Ok(raw[i]);
        }
        Ok(if c.is_some() { FOLLOWING } else { 0 })
    }

    fn encode(&mut self, o: Option<NodeId>, n: usize, desc: Desc) -> Result<Vec<u8>> {
        let big = self.big;
        if desc.kind == DescKind::U32 {
            let tree = self.w.tree.ok_or_else(|| Error::new("no tree to write"))?;
            let mut out = vec![0u8; 8 * tree.assets.len()];
            for (i, &e) in tree.assets.iter().enumerate() {
                let entry = self.st.entry(e);
                let p = if self.st.is_ref(entry.asset) {
                    self.ref_value(entry.asset.unwrap(), false)?
                } else if entry.asset.is_none() {
                    0
                } else if self.st.as_struct(entry.asset).is_some_and(|a| a.slot.is_some()) {
                    INSERT
                } else {
                    FOLLOWING
                };
                store(&mut out[8 * i..], 4, u64::from(self.plat.type_number(&entry.ty)?), big);
                store(&mut out[8 * i + 4..], 4, u64::from(p), big);
            }
            return Ok(out);
        }
        let node = o.ok_or_else(|| Error::new(format!("nothing to write at {}", self.path_text())))?;
        if desc.kind == DescKind::String {
            let s = self
                .st
                .as_str(Some(node))
                .ok_or_else(|| Error::new(format!("a string is something else at {}", self.path_text())))?;
            let mut out = text::bytes(&s.b);
            out.push(0);
            return Ok(out);
        }
        if desc.kind == DescKind::Ptrs {
            if let Some(pl) = self.st.as_ptr_list(Some(node)) {
                let count = pl.items.len();
                let mut out = vec![0u8; 4 * count];
                for i in 0..count {
                    let v = self.ptr_raw(node, i)?;
                    store(&mut out[4 * i..], 4, u64::from(v), big);
                }
                return Ok(out);
            }
            if let Some(a) = self.st.as_array(Some(node)) {
                let items = a.items.clone();
                let mut out = vec![0u8; 4 * items.len()];
                for (i, c) in items.into_iter().enumerate() {
                    let v = if self.st.is_ref(c) {
                        self.ref_value(c.unwrap(), false)?
                    } else if c.is_some() {
                        FOLLOWING
                    } else {
                        0
                    };
                    store(&mut out[4 * i..], 4, u64::from(v), big);
                }
                return Ok(out);
            }
            return Err(Error::new(format!("a pointer list is something else at {}", self.path_text())));
        }
        if let Some(lf) = self.st.as_leaf(Some(node)) {
            if let Some(t) = lf.t
                && lf.big != big
                && t.size() > 1
            {
                return Err(Error::new(format!(
                    "{} data is still in the other platform's byte order at {}",
                    t.name(),
                    self.path_text()
                )));
            }
            return Ok(lf.raw.clone());
        }
        let mut out = vec![0u8; n];
        if desc.kind == DescKind::Partial {
            if !self.st.is_struct(Some(node)) {
                return Err(Error::new(format!("a struct is something else at {}", self.path_text())));
            }
            self.encode_struct(node, &mut out, n, true)?;
            return Ok(out);
        }
        let t = desc.t.ok_or_else(|| Error::new("a struct of no type"))?;
        let items: Vec<Option<NodeId>> = match self.st.as_array(Some(node)) {
            Some(a) => {
                let mut items = a.items.clone();
                if a.tail && items.len() as i64 > desc.count {
                    items.truncate(desc.count as usize);
                }
                items
            }
            None => vec![Some(node)],
        };
        if items.len() as i64 != desc.count {
            return Err(Error::new(format!(
                "{}: {} items to write, the file says {} (at {})",
                t.name(),
                items.len(),
                desc.count,
                self.path_text()
            )));
        }
        let wrapped = t.kind() == Kind::Typedef;
        let size = t.size() as usize;
        for (i, item) in items.into_iter().enumerate() {
            let s = item
                .filter(|&s| self.st.is_struct(Some(s)))
                .ok_or_else(|| Error::new(format!("a {} is something else at {}", t.name(), self.path_text())))?;
            self.encode_struct(s, &mut out[i * size..(i + 1) * size], size, !wrapped)?;
        }
        Ok(out)
    }

    // ---- the stream ---------------------------------------------------------------------

    fn write_read(&mut self, n: usize, desc: Desc) -> Result<(BufPtr, Where)> {
        let o = self.w.obj.take();
        let mut data = Buf::default();
        let mut n = n;
        let mut zp: i64 = -1;
        if self.block == RUNTIME {
            data.b = vec![0; n];
        } else {
            data.b = self.encode(o, n, desc)?;
            if desc.kind != DescKind::String && data.b.len() != n {
                return Err(Error::new(format!(
                    "wrote {} bytes, expected {n} (at {})",
                    data.b.len(),
                    self.path_text()
                )));
            }
            n = data.b.len();
            zp = self.pos as i64;
            self.zone.extend_from_slice(&data.b);
            self.pos += n;
        }
        data.segs.push((0, zp));
        let b = self.block as usize;
        let at = Where { block: self.block, pos: self.block_pos[b] };
        if let Some(o) = o {
            self.w.loc.entry(o).or_insert(at);
        }
        if n != 0 && zp >= 0 {
            self.w.zone_positions.push(zp as usize);
            self.w.zone_spans.push([self.block as u32, at.pos, n as u32]);
        }
        self.block_pos[b] = self.block_pos[b].wrapping_add(n as u32);
        self.block_max[b] = self.block_max[b].max(self.block_pos[b]);
        self.w.last = o;
        Ok((Rc::new(RefCell::new(data)), at))
    }

    fn write_load_block(
        &mut self,
        inst: InstId,
        mi: MemberId,
        mod_pos: usize,
        combined: i64,
        kind: LoadKind,
        at: usize,
    ) -> Result<()> {
        let d = self.reg[inst].ok_or_else(|| Error::new("a member of nothing written"))?;
        let idx = self.ctx.last().map(|c| c.idx.clone()).unwrap_or_default();
        let name = &self.member(mi).name;
        let o = if matches!(kind, LoadKind::Embedded | LoadKind::EmbArray | LoadKind::Dynamic) {
            self.st.late_at(d, name, &idx)
        } else {
            self.st.child_at(d, name, &idx)
        };
        self.w.obj = o;
        let done = self.base_load_block(inst, mi, mod_pos, combined, kind, at);
        self.w.obj = None;
        done
    }

    fn write_load_ptr_array(
        &mut self,
        mi: MemberId,
        at_start: bool,
        count: i64,
        buf: Option<BufPtr>,
        at: usize,
    ) -> Result<()> {
        let s: &'static Schema = self.s;
        let list_node = self.w.obj;
        let (buf, at) = if at_start {
            (self.write_read(4usize.wrapping_mul(count as usize), Desc { kind: DescKind::Ptrs, t: None, count })?.0, 0)
        } else {
            (buf.ok_or_else(|| Error::new("a pointer array without its struct"))?, at)
        };
        let t = s.members[mi].ty;
        let m = self.member(mi);
        for k in 0..count as usize {
            let p = at + 4 * k;
            let mut val = self.u32_at(&buf, p)?;
            if val == 0 {
                continue;
            }
            let mut c = if let Some(pl) = self.st.as_ptr_list(list_node) {
                *pl.items.get(k).ok_or_else(|| Error::new("a pointer list is shorter than its count"))?
            } else if let Some(a) = self.st.as_array(list_node) {
                *a.items.get(k).ok_or_else(|| Error::new("a pointer list is shorter than its count"))?
            } else {
                return Err(Error::new(format!("a pointer list is something else at {}", self.path_text())));
            };
            if let Some(t) = t
                && !s.structs[t].asset.is_empty()
            {
                self.w.obj = c;
                self.write_load_asset_ptr(t, &buf, p)?;
                continue;
            }
            if s.members[mi].is_reusable
                && val != FOLLOWING
                && let Some(f) = self.forward(c, false)
            {
                c = Some(f);
                val = FOLLOWING;
                self.patch(&buf, p, val);
            }
            if s.members[mi].is_reusable && val != FOLLOWING {
                self.w.obj = c;
                self.alias(&buf, p, val)?;
                continue;
            }
            let align = match t {
                Some(t) => self.align_of(t),
                None => s.defs.types[m.ty].align,
            };
            self.alloc(align);
            self.w.obj = c;
            if let Some(t) = t
                && !s.structs[t].is_leaf
            {
                self.load_struct(t, None, true)?;
            } else {
                let et = match t {
                    Some(t) => self.ctype(t),
                    None => self.ty(m.ty),
                };
                self.write_read(et.size() as usize, Desc { kind: DescKind::Type, t: Some(et), count: 1 })?;
            }
        }
        self.w.obj = None;
        Ok(())
    }

    fn write_load_xstring(&mut self, buf: &BufPtr, at: usize) -> Result<()> {
        let mut o = self.w.obj.take();
        let mut val = self.u32_at(buf, at)?;
        if val == 0 {
            return Ok(());
        }
        if val != FOLLOWING
            && o.is_some()
            && let Some(f) = self.forward(o, false)
        {
            o = Some(f);
            val = FOLLOWING;
            self.patch(buf, at, val);
        }
        if val != FOLLOWING {
            self.w.obj = o;
            let done = self.alias(buf, at, val);
            self.w.obj = None;
            return done;
        }
        let len = self
            .st
            .as_str(o)
            .map(|s| text::bytes(&s.b).len())
            .ok_or_else(|| Error::new(format!("a string is something else at {}", self.path_text())))?;
        self.w.obj = o;
        self.write_read(len + 1, Desc::of(DescKind::String))?;
        Ok(())
    }

    fn write_load_xstring_array(&mut self, at_start: bool, count: i64, buf: Option<BufPtr>, at: usize) -> Result<()> {
        let pl = self.w.obj.filter(|&o| self.st.is_ptr_list(Some(o)));
        if pl.is_none() && count != 0 {
            return Err(Error::new(format!("a string list is something else at {}", self.path_text())));
        }
        let (buf, at) = if at_start {
            (self.write_read(4usize.wrapping_mul(count as usize), Desc { kind: DescKind::Ptrs, t: None, count })?.0, 0)
        } else {
            (buf.ok_or_else(|| Error::new("a string array without its struct"))?, at)
        };
        for k in 0..count as usize {
            let item = pl
                .and_then(|pl| self.st.ptr_list(pl).items.get(k).copied())
                .ok_or_else(|| Error::new("a string list is shorter than its count"))?;
            self.w.obj = item;
            self.write_load_xstring(&buf, at + 4 * k)?;
        }
        self.w.obj = None;
        Ok(())
    }

    fn write_load_asset_ptr(&mut self, info: StructId, buf: &BufPtr, at: usize) -> Result<Option<InstId>> {
        let s: &'static Schema = self.s;
        let mut d = self.w.obj.take();
        let mut val = self.u32_at(buf, at)?;
        let in_temp = self.block_is(s.structs[info].block, "temp");
        if val != 0
            && val != FOLLOWING
            && val != INSERT
            && let Some(f) = self.forward(d, true)
        {
            d = Some(f);
            let slotted = self.st.as_struct(d).is_some_and(|x| x.slot.is_some());
            val = if in_temp && slotted { INSERT } else { FOLLOWING };
            self.patch(buf, at, val);
        }
        let ds = d.filter(|&x| self.st.is_struct(Some(x)));
        let (ds_slot, ds_forward) = match ds {
            Some(x) => {
                let x = self.st.st(x);
                (x.slot, x.forward)
            }
            None => (None, false),
        };
        if val == FOLLOWING && in_temp && ds_slot.is_some() && ds_forward {
            // Pointed at from elsewhere too (written at its first pointer): it keeps a slot,
            // which later pointers name (its data is in the temp block, which the game
            // reuses for the next asset).
            val = INSERT;
            self.patch(buf, at, val);
        }
        if self.w.dedupe
            && (val == FOLLOWING || val == INSERT)
            && let Some(x) = ds
            && let Some(&first) = self.w.first.get(&x)
        {
            // Written already: point at the pointer that first held it, which the game sets
            // to it when it loads it, as stock files do.
            self.patch(buf, at, first);
            self.w.deduped += 1;
            return Ok(None);
        }
        if self.w.dedupe
            && val == FOLLOWING
            && let Some(x) = ds
            && let Some(w) = self.address(buf, at)
            && w.block != TEMP
        {
            self.w.first.insert(x, (((w.block as u32) << BLOCK_SHIFT) | w.pos).wrapping_add(1));
        }
        if in_temp {
            self.push(TEMP);
        }
        let mut inst = None;
        if val != 0 {
            if val == FOLLOWING || (in_temp && val == INSERT) {
                self.alloc(self.align_of(info));
                if in_temp && val == INSERT {
                    self.insert_pointer();
                    let slot = self.w.slot;
                    if let Some(sl) = ds_slot {
                        self.w.loc.insert(sl, Where { block: VIRTUAL, pos: slot });
                    }
                    if self.w.dedupe
                        && let Some(x) = ds
                    {
                        self.w.first.entry(x).or_insert((((VIRTUAL as u32) << BLOCK_SHIFT) | slot).wrapping_add(1));
                    }
                }
                let x = ds.ok_or_else(|| Error::new(format!("an asset is something else at {}", self.path_text())))?;
                self.w.starts.insert(x, self.pos);
                self.w.written.push((self.pos, x));
                self.w.obj = Some(x);
                inst = self.load_struct(info, None, true)?;
            } else {
                if let Some(r) = self.st.as_ref(d) {
                    let target = r.target;
                    let (rel, val) = (r.rel, r.val);
                    if let Some(tslot) = self.st.as_struct(target).and_then(|t| t.slot)
                        && rel == 0
                        && self.w.loc.contains_key(&tslot)
                    {
                        // A temp asset written already (where its first pointer took it):
                        // point at its slot, which the game sets to the asset, not at its
                        // data in the temp block.
                        let to = self.st.make_ref(val);
                        self.st.reference_mut(to).target = Some(tslot);
                        d = Some(to);
                    }
                }
                self.w.obj = d;
                self.alias(buf, at, val)?;
                self.w.obj = None;
            }
        }
        if in_temp {
            self.pop();
        }
        Ok(inst)
    }

    // ---- the zone -----------------------------------------------------------------------

    fn write(&mut self, tree: &Tree, header: Option<&[u8]>) -> Result<Vec<u8>> {
        let s: &'static Schema = self.s;
        let big = self.big;
        let ss = tree.script_strings.filter(|&p| self.st.is_ptr_list(Some(p)));
        let strings = ss.map_or(0, |p| self.st.ptr_list(p).items.len());
        let have_strings = strings != 0;
        let have_assets = !tree.assets.is_empty();
        let mut list = [0u8; 16];
        store(&mut list, 4, strings as u64, big);
        store(&mut list[4..], 4, if have_strings { u64::from(FOLLOWING) } else { 0 }, big);
        store(&mut list[8..], 4, tree.assets.len() as u64, big);
        store(&mut list[12..], 4, if have_assets { u64::from(FOLLOWING) } else { 0 }, big);
        self.zone.extend_from_slice(&list);
        self.pos += 16;
        self.push(VIRTUAL);
        if have_strings {
            self.alloc(4);
            self.w.obj = ss;
            self.write_load_xstring_array(true, strings as i64, None, 0)?;
        }
        self.pop();
        self.push(VIRTUAL);
        if have_assets {
            self.alloc(4);
            self.w.obj = None;
            let count = tree.assets.len();
            let (buf, at) =
                self.write_read(8 * count, Desc { kind: DescKind::U32, t: None, count: 2 * count as i64 })?;
            for (i, &e) in tree.assets.iter().enumerate() {
                self.w.loc.insert(e, Where { block: at.block, pos: at.pos.wrapping_add(8 * i as u32) });
            }
            for (i, &e) in tree.assets.iter().enumerate() {
                let (ty, asset) = {
                    let entry = self.st.entry(e);
                    (entry.ty.clone(), entry.asset)
                };
                let number = self.plat.type_number(&ty)?;
                let name = self.plat.structs.get(&number).ok_or_else(|| Error::new(format!("no asset struct for {ty}")))?;
                let info = *s.assets.get(*name).ok_or_else(|| Error::new(format!("the rules have no {name}")))?;
                self.asset_type = number as i32;
                self.w.obj = asset;
                if let Err(err) = self.write_load_asset_ptr(info, &buf, 8 * i + 4) {
                    if !err.is_error() {
                        return Err(err);
                    }
                    let named = asset
                        .filter(|&a| self.st.is_struct(Some(a)))
                        .and_then(|a| query::name(self.st, a))
                        .unwrap_or_else(|| "?".into());
                    return Err(Error::new(format!("{err}, in asset {i} ({ty} {named})")));
                }
            }
        }
        self.pop();
        let header_bytes = self.plat.header_bytes();
        let mut hdr = vec![(self.zone.len() - header_bytes) as u32, 0];
        if let Some(h) = header {
            hdr[1] = load(&h[4..], 4, big) as u32;
        }
        for b in 0..self.plat.blocks as usize {
            hdr.push(if b == TEMP as usize { self.block_max[b] } else { self.block_pos[b] });
        }
        if let Some(h) = header {
            // The temp and callback sizes are allowances the walk alone doesn't show
            // (loader scratch space, data set up by callbacks): the original's are kept
            // when bigger.
            for b in [TEMP as usize, CALLBACK as usize] {
                hdr[2 + b] = hdr[2 + b].max(load(&h[8 + 4 * b..], 4, big) as u32);
            }
        }
        for (k, &v) in hdr.iter().enumerate() {
            store(&mut self.zone[4 * k..], 4, u64::from(v), big);
        }
        Ok(std::mem::take(&mut self.zone))
    }
}
