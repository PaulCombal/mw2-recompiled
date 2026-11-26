// Every shader in a fastfile, as the game will hand it to the renderer.
//
// A material's techniques carry their shaders inline. Each is in two parts: the D3D
// shader object (the "cached" part) and the program the GPU runs (the "physical" part).
// When the game loads one, src/hooks/shader_preload.rs reads the object's header to find
// where in the program the microcode is, and hands the renderer those bytes. The same
// rule over the same two parts in the file gives the same bytes, and so the same name
// (mw2_shaders::shader::hash).

use std::collections::{HashMap, HashSet};

use mw2_shaders::shader;

use crate::schema::{MemberId, Schema};
use crate::vertex_binding::{self, BindTables, MOVE_STRIDE};
use crate::walker::{FOLLOWING, Place, Visitor, Walker};
use crate::{Error, Result};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shader {
    pub pixel: bool,
    pub hash: u64,
    /// Microcode dwords, as the command stream loads them.
    pub code: Vec<u32>,
}

/// What became of a zone's vertex shaders (vertex_binding.rs).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct BindCounts {
    /// Bound for their pass's declaration, as the title binds them.
    pub bound: u32,
    /// Loaded where nothing binds them.
    pub unbound: u32,
    /// In a slot whose declaration type has no stream for a source.
    pub no_declaration: u32,
    /// A pass's own vertex shader, past the 15 declaration types.
    pub own_shader: u32,
}

fn word(p: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

/// The microcode a shader header describes: the table its seventh word points to holds
/// {offset in the program, bytes}. None if the header says something impossible. The
/// header is data[header, header_end), the program data[program, program_end), or all
/// of `separate` when the program is in a buffer of its own.
fn microcode(
    data: &[u8],
    header: usize,
    header_end: usize,
    program: usize,
    program_end: usize,
    pixel: bool,
    separate: Option<&[u8]>,
) -> Option<Shader> {
    let (program, program_end) = match separate {
        Some(p) => (0, p.len()),
        None => (program, program_end),
    };
    if header + 28 > header_end {
        return None;
    }
    if word(data, header) & 0xFFFF_FF00 != 0x102A_1100 {
        return None;
    }
    let table = header + word(data, header + 24) as usize;
    if table + 8 > header_end {
        return None;
    }
    let (offset, length) = (word(data, table) as usize, word(data, table + 4) as usize);
    if length == 0
        || length & 3 != 0
        || offset > 4096
        || program + offset + length > program_end
        || (separate.is_none() && program_end > data.len())
    {
        return None;
    }
    let code_bytes = &separate.unwrap_or(data)[program + offset..program + offset + length];
    let code: Vec<u32> = code_bytes.chunks_exact(4).map(|w| word(w, 0)).collect();
    Some(Shader { pixel, hash: shader::hash(&code), code })
}

/// The rules' members the walk is watched for.
struct Members {
    /// Of a vertex and a pixel shader's load def: the program, and the object.
    programs: [MemberId; 2],
    objects: [MemberId; 2],
    pass_declaration: MemberId,
    pass_shaders: MemberId,
    pass_shader: MemberId,
}

impl Members {
    fn of(s: &Schema) -> Result<Members> {
        let member = |ty: &str, name: &str| {
            s.infos
                .get(ty)
                .and_then(|&info| s.find(info, name))
                .ok_or_else(|| Error::new(format!("the rules have no {ty}::{name}")))
        };
        Ok(Members {
            programs: [
                member("GfxVertexShaderLoadDef", "physicalPart")?,
                member("GfxPixelShaderLoadDef", "physicalPart")?,
            ],
            objects: [member("GfxVertexShaderLoadDef", "cachedPart")?, member("GfxPixelShaderLoadDef", "cachedPart")?],
            pass_declaration: member("MaterialPass", "vertexDecl")?,
            pass_shaders: member("MaterialPass", "vertexShaderArray")?,
            pass_shader: member("MaterialPass", "vertexShader")?,
        })
    }
}

/// A part's place in the zone.
#[derive(Clone, Copy, Default)]
struct Span {
    at: usize,
    bytes: usize,
    set: bool,
}

struct Lister<'a> {
    zone: &'a [u8],
    tables: &'a BindTables,
    m: Members,
    out: Vec<Shader>,
    seen: HashSet<(bool, u64)>,
    tally: BindCounts,
    /// The program streams before its object (the rules' reorder).
    program: [Span; 2],
    /// The pass being loaded: its declaration's place in the zone (an earlier pass's
    /// when it points back to one), and the slot of the vertex shader being loaded fresh
    /// -- the title binds a shader only then (sub_821BBE70).
    declarations: HashMap<u32, usize>,
    declaration: Option<usize>,
    slot: i64,
}

impl Lister<'_> {
    fn add(&mut self, shader: Shader) {
        if self.seen.insert((shader.pixel, shader.hash)) {
            self.out.push(shader);
        }
    }
}

impl Visitor for Lister<'_> {
    fn on_pointer(&mut self, m: MemberId, index: i64, value: u32) -> Result<()> {
        if m == self.m.pass_declaration {
            self.declaration = None;
            if value != 0 && value != FOLLOWING {
                let Some(&found) = self.declarations.get(&value) else {
                    return Err(Error::new("a material pass points to a vertex declaration not loaded before it"));
                };
                self.declaration = Some(found);
            }
        } else if m == self.m.pass_shaders {
            self.slot = if value == FOLLOWING { index } else { -1 };
        } else if m == self.m.pass_shader {
            self.slot = if value == FOLLOWING { 15 } else { -1 };
        }
        Ok(())
    }

    fn on_data(&mut self, m: MemberId, place: &Place, bytes: usize) -> Result<()> {
        let at = place.zone_pos;
        if m == self.m.pass_declaration {
            if bytes != 64 || !place.in_zone {
                return Err(Error::new("a vertex declaration is not the 64 bytes it should be"));
            }
            self.declaration = Some(at);
            self.declarations.insert(place.pointer(), at);
            return Ok(());
        }
        for pixel in 0..2 {
            if m == self.m.programs[pixel] {
                self.program[pixel] = Span { at, bytes, set: true };
                return Ok(());
            }
            if m != self.m.objects[pixel] {
                continue;
            }
            if !self.program[pixel].set {
                return Err(Error::new(format!("a shader object at {at} has no program")));
            }
            let p = self.program[pixel];
            self.program[pixel].set = false;

            // The object's header is at +40 in a pixel shader's, at +872 in a vertex
            // shader's, where the vertex fetch tables come first.
            let zone = self.zone;
            let header = at + if pixel == 1 { 40 } else { 872 };
            let shader = microcode(zone, header, at + bytes, p.at, p.at + p.bytes, pixel == 1, None)
                .ok_or_else(|| Error::new(format!("the shader object at {at} has no header this can read")))?;
            if pixel == 1 {
                self.add(shader);
                return Ok(());
            }
            // A vertex shader is drawn as its binding leaves it.
            let bound = self.slot;
            self.slot = -1;
            let Some(declaration) = self.declaration.filter(|_| bound >= 0) else {
                self.tally.unbound += 1;
                self.add(shader);
                return Ok(());
            };
            let object = &zone[at..at + bytes];
            let mut patched = zone[p.at..p.at + p.bytes].to_vec();
            if !vertex_binding::bind(self.tables, &zone[declaration..], bound as usize, object, &mut patched)? {
                if bound >= 15 {
                    self.tally.own_shader += 1;
                } else {
                    self.tally.no_declaration += 1;
                }
                self.add(shader);
                return Ok(());
            }
            self.tally.bound += 1;
            let made = microcode(object, 872, object.len(), 0, 0, false, Some(&patched))
                .ok_or_else(|| Error::new("a bound vertex shader has no header this can read"))?;
            self.add(made);
            return Ok(());
        }
        Ok(())
    }
}

/// Each shader of the zone once, vertex shaders as their binding leaves them, and what
/// became of the vertex shaders; an Error when the zone does not walk to its end, or a
/// shader's header cannot be read.
pub fn shaders(zone: &[u8], tables: &BindTables) -> Result<(Vec<Shader>, BindCounts)> {
    let s = crate::rules::xbox()?;
    let mut lister = Lister {
        zone,
        tables,
        m: Members::of(s)?,
        out: Vec::new(),
        seen: HashSet::new(),
        tally: BindCounts::default(),
        program: [Span::default(); 2],
        declarations: HashMap::new(),
        declaration: None,
        slot: -1,
    };
    Walker::new(s, zone, &mut lister).walk()?;
    Ok((lister.out, lister.tally))
}

/// D3D's shaders: { pixel, address, dwords }, at each site that sends one (sub_820D6C18,
/// 820DA6C8, 820DEC80, 820E0C88, 820E5570, 820E56C8, 820EF308 -- the three of its switch
/// --, 820F0128 and 820F09A8 in the multiplayer; the same D3D, elsewhere, in the campaign).
const INTERNAL_MULTIPLAYER: [(bool, u32, u32); 17] = [
    (false, 0x82079AB0, 24),
    (true, 0x820799BC, 9),
    (false, 0x82079A50, 24),
    (false, 0x820799E0, 27),
    (true, 0x82079998, 9),
    (false, 0x82079554, 15),
    (false, 0x82078684, 15),
    (false, 0x82079938, 24),
    (true, 0x82077E2C, 9),
    (true, 0x8207785C, 15),
    (false, 0x82077E50, 525),
    (false, 0x820786C0, 933),
    (false, 0x82079590, 210),
    (false, 0x82077088, 24),
    (true, 0x820771A8, 429),
    (false, 0x82077958, 309),
    (true, 0x82076E74, 9),
];
const INTERNAL_CAMPAIGN: [(bool, u32, u32); 17] = [
    (false, 0x82065EF0, 24),
    (true, 0x82065DFC, 9),
    (false, 0x82065E90, 24),
    (false, 0x82065E20, 27),
    (true, 0x82065DD8, 9),
    (false, 0x82065994, 15),
    (false, 0x82064AC4, 15),
    (false, 0x82065D78, 24),
    (true, 0x8206426C, 9),
    (true, 0x82063C9C, 15),
    (false, 0x82064290, 525),
    (false, 0x82064B00, 933),
    (false, 0x820659D0, 210),
    (false, 0x820634C8, 24),
    (true, 0x820635E8, 429),
    (false, 0x82063D98, 309),
    (true, 0x820632B4, 9),
];
const IMAGE_BASE: u32 = 0x8200_0000;

/// The shaders an executable carries in itself, from its memory image
/// (mw2_install::xex::image, at 0x82000000):
///   - the SDK's GPU memory move, the image pool's block copy: compiled shader files
///     whose header -- the same one a shader object holds -- starts with the magic
///     0x102A11xx; its second word is where the program starts, and the same table as
///     above says where in the program the microcode is;
///   - D3D's own, for its clears and the like: bare microcode the D3D code copies into
///     the command stream (IM_LOAD_IMMEDIATE) from addresses it names, read from that
///     code once. The install checks each executable by its SHA-256, so they are where
///     the table says.
///   - the memory move's vertex shader again, bound as its draw binds it.
pub fn executable_shaders(image: &[u8], multiplayer: bool, tables: &BindTables) -> Result<Vec<Shader>> {
    let mut out = Vec::new();
    let internal = if multiplayer { &INTERNAL_MULTIPLAYER } else { &INTERNAL_CAMPAIGN };
    for &(pixel, address, dwords) in internal {
        let at = (address - IMAGE_BASE) as usize;
        let Some(bytes) = image.get(at..at + 4 * dwords as usize) else {
            return Err(Error::new("an executable is shorter than D3D's shaders in it"));
        };
        let code: Vec<u32> = bytes.chunks_exact(4).map(|w| word(w, 0)).collect();
        out.push(Shader { pixel, hash: shader::hash(&code), code });
    }
    let mut at = 0;
    while at + 32 <= image.len() {
        let magic = word(image, at);
        if magic & 0xFFFF_FF00 != 0x102A_1100 {
            at += 4;
            continue;
        }
        // The magic's low bit is set in a vertex shader's: every one in the fastfiles has
        // 0x102A1101 and every pixel shader 0x102A1100. The memory move's vertex shader
        // has 0x102A1111.
        let pixel = magic & 1 == 0;
        let program = at + word(image, at + 4) as usize;
        let Some(shader) = microcode(image, at, image.len(), program, image.len(), pixel, None) else {
            at += 4;
            continue;
        };
        out.push(shader);
        if !pixel {
            // The memory move's vertex shader, as its draw binds it.
            let object = &image[at..program];
            let mut patched = image[program..].to_vec();
            let mut strides = [0u8; 16];
            strides[0] = MOVE_STRIDE;
            let decl = vertex_binding::move_declaration(image, multiplayer)?;
            vertex_binding::bind_programs(tables, &decl, &strides, object, 0, &mut patched)?;
            let bound = microcode(object, 0, object.len(), 0, 0, false, Some(&patched))
                .ok_or_else(|| Error::new("the memory move's vertex shader has no header this can read"))?;
            out.push(bound);
        }
        at += 4;
    }
    Ok(out)
}
