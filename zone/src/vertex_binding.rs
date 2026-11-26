// A vertex shader as the game draws it.
//
// A vertex shader in a fastfile names no vertex format: its fetches read format 0.
// While the title loads a material pass it binds the pass's vertex shader for each of
// the 15 vertex declaration types to that declaration (IDirect3DVertexShader9::Bind),
// and D3D rewrites the shader's fetches in place -- fetch constant, format, offset and
// stride -- merging neighbours that read one stream into mini-fetches. That rewritten
// program is the one the command stream loads, and so the one to compile.
//
// The declaration comes from the pass's stream routing, through the title's tables of
// where each source is in each declaration type's streams (sub_823EA570 in the
// multiplayer), and D3D's CreateVertexDeclaration. All of it is reproduced here from
// the same data: the routing and the shader in the fastfile, the tables in the
// executable.

use crate::{Error, Result};

const IMAGE_BASE: u32 = 0x8200_0000;

/// Where the tables are: the title's (the same in both executables) and D3D's, read
/// from the code that uses them.
struct Addresses {
    types: u32,
    usages: u32,
    strides0: u32,
    strides1: u32,
    swizzles: u32,
    formats: u32,
}
const MULTIPLAYER: Addresses = Addresses {
    types: 0x8206F550,
    usages: 0x8206F534,
    strides0: 0x8244F244,
    strides1: 0x82070700,
    swizzles: 0x82079C30,
    formats: 0x8207D750,
};
const CAMPAIGN: Addresses = Addresses {
    types: 0x8205BA60,
    usages: 0x8205BA44,
    strides0: 0x8241EFB8,
    strides1: 0x8205D32C,
    swizzles: 0x82066070,
    formats: 0x82069040,
};
/// The image pool's memory move: the element list sub_823DDF08 makes its declaration
/// from (the campaign's sub_823A4AC8), in the multiplayer and the campaign.
const MOVE_DECLARATION: (u32, u32) = (0x8244F348, 0x8241F0B8);

/// Its stream's stride, in dwords: T_Image_FlushMove sets the vertex stream with 64
/// bytes (li r7,64 before the call), the 8 elements of 8.
pub const MOVE_STRIDE: u8 = 64 / 4;

/// What the binding reads from the executable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BindTables {
    /// Per declaration type: { stream, offset, format } per source.
    pub types: [u8; 15 * 72],
    /// Per destination: { usage, usage index }.
    pub usages: [u8; 14 * 2],
    /// Per declaration type, of streams 0 and 1, in bytes.
    pub strides: [[u32; 15]; 2],
    /// D3D's: the components a fetch's format leaves.
    pub swizzles: [u8; 16],
    /// D3D's: the dwords a fetch of each format reads.
    pub formats: [u8; 64],
}

fn get32(p: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}
fn get16(p: &[u8], at: usize) -> u32 {
    u32::from(u16::from_be_bytes([p[at], p[at + 1]]))
}
fn put32(p: &mut [u8], at: usize, v: u32) {
    p[at..at + 4].copy_from_slice(&v.to_be_bytes());
}
fn put16(p: &mut [u8], at: usize, v: u32) {
    p[at..at + 2].copy_from_slice(&(v as u16).to_be_bytes());
}

// The PowerPC's rotate-and-mask, with its bit numbering (0 the highest).
fn mask(mb: u32, me: u32) -> u32 {
    let mut m = 0;
    let mut i = mb;
    loop {
        m |= 0x8000_0000u32 >> i;
        if i == me {
            return m;
        }
        i = (i + 1) & 31;
    }
}
fn rlwinm(x: u32, sh: u32, mb: u32, me: u32) -> u32 {
    x.rotate_left(sh & 31) & mask(mb, me)
}
fn rlwimi(dst: u32, src: u32, sh: u32, mb: u32, me: u32) -> u32 {
    let m = mask(mb, me);
    (src.rotate_left(sh & 31) & m) | (dst & !m)
}

/// D3D's declaration object (CreateVertexDeclaration, sub_820D7AB8): 52 bytes of header
/// -- the element count at 24, the highest stream at 28, the streams used at 32 -- then
/// the elements as they were given.
fn created(elements: &[[u8; 12]]) -> Vec<u8> {
    let mut object = vec![0u8; 52 + 12 * elements.len()];
    let mut highest = 0;
    for e in elements {
        let stream = get16(e, 0);
        highest = highest.max(stream);
        if stream < 16 {
            object[32 + stream as usize] = 255;
        }
    }
    put32(&mut object, 24, elements.len() as u32);
    put32(&mut object, 28, highest);
    for (k, e) in elements.iter().enumerate() {
        object[52 + 12 * k..64 + 12 * k].copy_from_slice(e);
    }
    object
}

/// The pass's routing as one declaration type's D3D vertex elements (sub_823EA570),
/// made into D3D's declaration object: { 52 bytes of header, the elements }. Empty when
/// a source has no stream in this type. `decl` is the pass's 64 bytes, and what follows
/// them in the zone.
fn declaration(t: &BindTables, decl: &[u8], ty: usize) -> Result<Vec<u8>> {
    let past = || Error::new("a vertex declaration runs past the end of the zone");
    let routes = *decl.first().ok_or_else(past)? as usize;
    let mut elements: Vec<[u8; 12]> = Vec::new();
    for k in 0..routes {
        let source = *decl.get(4 + 2 * k).ok_or_else(past)? as usize;
        let dest = *decl.get(5 + 2 * k).ok_or_else(past)? as usize;
        let from = 72 * ty + 8 * source;
        let Some(from) = t.types.get(from..from + 8) else {
            return Err(Error::new("a vertex routing names a source past the table"));
        };
        let stream = get16(from, 0);
        if stream == 255 {
            return Ok(Vec::new());
        }
        if 2 * dest + 1 >= t.usages.len() {
            return Err(Error::new("a vertex routing names a destination past the table"));
        }
        let mut e = [0u8; 12];
        put16(&mut e, 0, stream);
        put16(&mut e, 2, get16(from, 2));
        put32(&mut e, 4, get32(from, 4));
        e[8] = 0; // method
        e[9] = t.usages[2 * dest];
        e[10] = t.usages[2 * dest + 1];
        // Kept in stream order, the earlier first among equals.
        let mut at = elements.len();
        while at > 0 && get16(&elements[at - 1], 0) > stream {
            at -= 1;
        }
        elements.insert(at, e);
    }
    Ok(created(&elements))
}

/// The core of the binding (sub_820D5220 in the multiplayer), for one of the shader's
/// programs: `code` its microcode, `copies` the fetch instructions InitVertexShader
/// kept, `table` and `list` the program's table and fetch list in the shader object.
/// Register names follow the code.
fn patch(
    t: &BindTables,
    table: &[u8],
    list: &[u8],
    copies: &[u8],
    decl: &[u8],
    strides: &[u8; 16],
    code: &mut [u8],
) -> Result<()> {
    let count = get32(table, 28) as usize; // r27
    if count == 0 {
        return Ok(());
    }
    let element_count = get32(decl, 24) as usize; // r24
    let elements = &decl[52..]; // r23
    let swizzle = |at: u32| get16(&t.swizzles, at as usize);
    let stride = |stream: u32| {
        strides
            .get(stream as usize)
            .map(|&s| u32::from(s))
            .ok_or_else(|| Error::new("a vertex declaration has a stream past the 16 D3D has"))
    };

    let mut built = vec![0u8; 12 * count]; // the stack buffer at r1+96
    // The low bits of r10 start as whatever a stack slot holds; only bit 0 survives to
    // the result, and every swizzle the and with keeps has it clear.
    let mut r10: u32 = 0;
    for k in 0..count {
        let r7 = 12 * k;
        let mut r11 = get32(copies, 12 * k);
        let mut r3 = get32(copies, 12 * k + 4);
        let r31 = get32(copies, 12 * k + 8);
        let mut r9: u32;
        let r8: u32;
        let mut element = None;
        if element_count != 0 {
            let r5 = get32(list, 4 * k);
            let usage = rlwinm(r5, 20, 28, 31);
            let index = rlwinm(r5, 16, 28, 31);
            element = (0..element_count)
                .map(|r9| &elements[12 * r9..12 * r9 + 12])
                .find(|e| u32::from(e[9]) == usage && u32::from(e[10]) == index);
        }
        match element {
            None => {
                // Nothing in the declaration for it: fetch constant 95.
                let r12: u32 = 0xBFC6CFFF;
                let r8a = u32::from(strides[0]);
                r11 = rlwimi(r11, 95, 20, 2, 11);
                r10 &= 0xF;
                let r5 = r3 & r12;
                let r4 = r31 & 0x8000_0000;
                r9 = r11;
                r10 |= 37456;
                r11 = r5 | 0x0006_0000;
                r8 = r4 | r8a;
            }
            Some(element) => {
                r9 = get32(element, 4); // the element's format
                r10 &= 0xF;
                let mut r4 = rlwinm(r9, 16, 29, 31);
                let mut r5 = rlwinm(r9, 22, 26, 28);
                let r21 = rlwinm(r9, 13, 29, 31);
                r5 |= r4;
                r4 = rlwinm(r9, 0, 16, 21);
                r5 = rlwinm(r5, 3, 0, 28);
                let r20 = rlwinm(r9, 0, 24, 25);
                r5 |= r21;
                r5 = rlwinm(r5, 1, 0, 30);
                r5 |= r4;
                r5 = rlwinm(r5, 3, 0, 28);
                r10 |= r5;
                r10 &= 0xFFFF;
                if r20 == 64 {
                    if (r10 & 0xE000) <= 24576 {
                        r10 = ((r10 & 0xE000) ^ 0x2000) | (r10 & 0x1FFF);
                    }
                    if (r10 & 0x1C00) <= 3072 {
                        r10 = ((r10 & 0x1C00) ^ 0x400) | (r10 & 58367);
                    }
                    if (r10 & 0x380) <= 384 {
                        r10 = ((r10 & 0x380) ^ 0x80) | (r10 & 64639);
                    }
                    if (r10 & 0x70) <= 48 {
                        r10 = ((r10 & 0x70) ^ 0x10) | (r10 & 65423);
                    }
                }
                let stream = get16(element, 0);
                let offset = get16(element, 2);
                let mut r4b: u32 = 0x0156_0056;
                let r21b = rlwinm(r9, 12, 14, 19);
                let mut r20b = 95u32.wrapping_sub(stream);
                r9 = rlwinm(r9, 0, 22, 23);
                r4b = r20b.wrapping_mul(r4b);
                let r5b = stride(stream)?;
                r20b = r4b;
                let r12: u32 = 0xBFC0CFFF;
                r20b = rlwimi(r20b, r4b, 11, 8, 12);
                let mut r8a = rlwinm(offset, 6, 1, 23);
                r9 |= r21b;
                r11 = rlwimi(r11, r20b, 1, 5, 11);
                r8a |= r5b;
                let r4c = rlwinm(r9, 4, 0, 27);
                r3 &= r12;
                let r5c = r31 & 0x8000_0000;
                r9 = rlwinm(r11, 0, 5, 1);
                r11 = r4c | r3;
                r8 = r8a | r5c;
            }
        }

        // loc_820D544C: the destination swizzle, a component at a time.
        let mut r5 = rlwinm(r11, 1, 28, 30);
        put32(&mut built, r7, r9);
        r10 &= 0xFFFF;
        put32(&mut built, r7 + 8, r8);
        r10 |= 14;
        r9 = swizzle(r5);
        let mut r8b = r10 & 0xFFFF;
        r5 = r10 & 0xFFFF;
        r9 &= r8b;
        r8b = r10 & 0xFFFF;
        let mut r4 = r9 & 0xFFFF;
        r9 = rlwinm(r9, 29, 19, 31);
        let r3b = r10 & 0xFFFF;
        r9 |= r4;
        r9 = rlwinm(r9, 29, 3, 31);
        r9 |= r4;
        r9 = rlwinm(r9, 29, 3, 31);
        r9 |= r4;
        r9 = rlwinm(r9, 29, 3, 31);
        r9 |= r4;
        r11 = rlwimi(r11, r9, 31, 29, 31);
        r9 = rlwinm(r11, 30, 28, 30);
        r9 = swizzle(r9);
        r9 &= r5;
        r5 = r9 & 0xFFFF;
        r4 = rlwinm(r9, 29, 19, 31);
        r9 = rlwinm(r9, 2, 14, 29);
        r4 |= r5;
        r4 = rlwinm(r4, 29, 3, 31);
        r4 |= r5;
        r4 = rlwinm(r4, 29, 3, 31);
        r5 |= r4;
        r5 = rlwinm(r5, 31, 1, 31);
        r9 |= r5;
        r9 = rlwimi(r9, r11, 0, 29, 25);
        r11 = rlwinm(r9, 27, 28, 30);
        r11 = swizzle(r11);
        r11 &= r8b;
        r8b = r11 & 0xFFFF;
        r5 = rlwinm(r11, 29, 19, 31);
        r11 = rlwinm(r11, 3, 13, 28);
        r5 |= r8b;
        r11 |= r8b;
        r5 = rlwinm(r5, 29, 3, 31);
        r11 = rlwinm(r11, 2, 0, 29);
        r8b |= r5;
        r8b = rlwinm(r8b, 31, 1, 31);
        r11 |= r8b;
        r11 = rlwimi(r11, r9, 0, 26, 22);
        r9 = rlwinm(r11, 24, 28, 30);
        r9 = swizzle(r9);
        r9 &= r3b;
        r8b = r9 & 0xFFFF;
        r5 = rlwinm(r9, 3, 13, 28);
        r9 = rlwinm(r9, 29, 19, 31);
        r5 |= r8b;
        r9 |= r8b;
        r5 = rlwinm(r5, 3, 0, 28);
        r9 = rlwinm(r9, 31, 1, 31);
        r8b |= r5;
        r8b = rlwinm(r8b, 2, 0, 29);
        r9 |= r8b;
        r9 = rlwimi(r9, r11, 0, 23, 19);
        put32(&mut built, r7 + 4, r9);
    }

    // loc_820D555C: within each group of fetches the list marks, sorted by fetch
    // constant, then offset, then size; then a fetch that reads the same constant just
    // past the one before becomes a mini-fetch of it.
    let key = |w: u32| rlwinm(w, 14, 25, 29) | rlwinm(w, 7, 30, 31);
    let word = |built: &[u8], i: usize, w: usize| get32(built, 12 * i + 4 * w);
    let marked = |i: usize| get32(list, 4 * i) & 0x0030_0000 != 0;
    let offset_of = |w: u32| (rlwinm(w, 1, 0, 30) as i32) >> 9;
    let size_of = |built: &[u8], i: usize| t.formats[(get16(built, 12 * i + 4) & 0x3F) as usize];
    let mut r27 = 0;
    while r27 < count {
        while r27 < count && !marked(r27) {
            r27 += 1;
        }
        if r27 >= count {
            break;
        }
        let mut r11 = r27 + 1;
        let start = r27;
        if r11 >= count {
            break;
        }
        while r11 < count && !marked(r11) {
            r11 += 1;
        }
        if r11 >= count {
            break;
        }
        let length = (r11 - start) as u32; // r10
        let r5 = length + 1;
        let mut r9 = length;
        let mut r4 = r5;
        let mut r29: i32 = -1;
        r27 = r11 + 1;
        if get32(list, 4 * r11) & 0x0030_0000 == 0x0030_0000 {
            r29 = key(word(&built, r11, 0)) as i32;
            r9 = length - 1;
            r4 = length;
        }
        // The sort: a pass per position, as the code makes it.
        let mut r31: u32 = 1;
        while r9 != 0 {
            if r31 < r4 {
                let a = start + (r31 - 1) as usize;
                let mut b = a + 1;
                let mut r28 = r4 - r31;
                while r28 != 0 {
                    let ka = key(word(&built, a, 0)) as i32;
                    let kb = key(word(&built, b, 0)) as i32;
                    let mut r8 = kb.wrapping_sub(ka);
                    let swap = if r8 != 0 {
                        if r29 == ka {
                            true
                        } else {
                            if r29 == kb {
                                r8 = -1;
                            }
                            r8 > 0
                        }
                    } else {
                        r8 = offset_of(word(&built, a, 2)).wrapping_sub(offset_of(word(&built, b, 2)));
                        if r8 == 0 {
                            r8 = i32::from(size_of(&built, a)) - i32::from(size_of(&built, b));
                        }
                        r8 > 0
                    };
                    if swap {
                        let mut tmp = [0u8; 12];
                        tmp.copy_from_slice(&built[12 * a..12 * a + 12]);
                        built.copy_within(12 * b..12 * b + 12, 12 * a);
                        built[12 * b..12 * b + 12].copy_from_slice(&tmp);
                    }
                    r28 -= 1;
                    b += 1;
                }
            }
            r9 -= 1;
            r31 += 1;
        }
        // loc_820D5730: the mini-fetches.
        let mut r7 = start;
        for i in start + 1..start + r5 as usize {
            let w10 = word(&built, i, 0);
            let w8 = word(&built, r7, 0);
            let c10 = rlwimi(w10, w10, 25, 12, 13) & mask(7, 13);
            let c8 = rlwimi(w8, w8, 25, 12, 13) & mask(7, 13);
            let mut merge = c8 == c10;
            let (mut r10, mut r9b) = (0u32, 0u32);
            if merge {
                r10 = offset_of(word(&built, r7, 2)) as u32;
                r9b = offset_of(word(&built, i, 2)) as u32;
                merge = r10 <= r9b;
            }
            if merge {
                let size = u32::from(size_of(&built, i));
                r10 = size.wrapping_sub(r10).wrapping_add(r9b);
                merge = r10 <= 8;
            }
            if merge {
                let r9c = rlwinm(w8, 5, 29, 31);
                r10 = r10.wrapping_sub(1);
                if r9c > r10 {
                    r10 = r9c;
                }
                put32(&mut built, 12 * r7, rlwimi(w8, r10, 27, 2, 4));
                let w1 = word(&built, i, 1);
                put32(&mut built, 12 * i + 4, w1 | 0x4000_0000);
            } else {
                r7 = i;
            }
        }
    }

    // loc_820D5800: back over the fetches in the microcode.
    for k in 0..count {
        let at = (get32(list, 4 * k) & 0xFFF) as usize * 12;
        let Some(into) = code.get_mut(at..at + 12) else {
            return Err(Error::new("a vertex fetch is past its shader's microcode"));
        };
        into.copy_from_slice(&built[12 * k..12 * k + 12]);
    }
    Ok(())
}

impl BindTables {
    /// From an executable's memory image (at 0x82000000).
    pub fn read(image: &[u8], multiplayer: bool) -> Result<BindTables> {
        let a = if multiplayer { &MULTIPLAYER } else { &CAMPAIGN };
        let at = |address: u32, bytes: usize| -> Result<&[u8]> {
            address
                .checked_sub(IMAGE_BASE)
                .and_then(|from| image.get(from as usize..from as usize + bytes))
                .ok_or_else(|| Error::new("an executable is shorter than the tables vertex binding reads"))
        };
        let mut t = BindTables {
            types: [0; 15 * 72],
            usages: [0; 28],
            strides: [[0; 15]; 2],
            swizzles: [0; 16],
            formats: [0; 64],
        };
        t.types.copy_from_slice(at(a.types, 15 * 72)?);
        t.usages.copy_from_slice(at(a.usages, 28)?);
        for s in 0..2 {
            let p = at(if s == 1 { a.strides1 } else { a.strides0 }, 60)?;
            for k in 0..15 {
                t.strides[s][k] = get32(p, 4 * k);
            }
        }
        t.swizzles.copy_from_slice(at(a.swizzles, 16)?);
        t.formats.copy_from_slice(at(a.formats, 64)?);
        Ok(t)
    }
}

/// The programs of `object` (a vertex shader object as a fastfile holds it: the load
/// def's cached part) in `program` (its physical part), bound for declaration type
/// `slot` of a pass with this declaration (the fastfile's 64 bytes, and what follows).
/// False when the title binds nothing there: a source the type has no stream for
/// (sub_823C2E28 finds no declaration and skips Bind), or slot 15, the pass's own
/// vertexShader, which the pass loader reads with the alias loader alone
/// (sub_821BBC08), never through sub_821BBE70.
pub fn bind(t: &BindTables, declaration: &[u8], slot: usize, object: &[u8], program: &mut [u8]) -> Result<bool> {
    // sub_823EA6D0 makes declarations for the 15 types; the pass's own vertex shader
    // comes as a 16th, whose declaration the title reads from past the 15.
    if slot >= 15 {
        return Ok(false);
    }
    let decl = self::declaration(t, declaration, slot)?;
    if decl.is_empty() {
        return Ok(false);
    }

    // Bind (sub_820D6A90): the strides as dwords, a byte per stream.
    let mut strides = [0u8; 16];
    let highest = get32(&decl, 28);
    if highest > 1 {
        return Err(Error::new("a vertex declaration has a stream past the two the title gives strides for"));
    }
    for (stride, of) in strides.iter_mut().zip(&t.strides).take(highest as usize + 1) {
        *stride = rlwinm(of[slot], 30, 24, 31) as u8;
    }

    bind_programs(t, &decl, &strides, object, 872, program)?;
    Ok(true)
}

/// The image pool's memory move binds its shader at its draw (D3D binds a shader the
/// declaration set beside it has not bound), to the declaration the title made for it,
/// D3D's object of it.
pub fn move_declaration(image: &[u8], multiplayer: bool) -> Result<Vec<u8>> {
    let address = if multiplayer { MOVE_DECLARATION.0 } else { MOVE_DECLARATION.1 };
    let mut elements = Vec::new();
    let mut at = (address - IMAGE_BASE) as usize;
    loop {
        let Some(e) = image.get(at..at + 12) else {
            return Err(Error::new("an executable is shorter than the memory move's declaration in it"));
        };
        if get16(e, 0) == 255 {
            break;
        }
        elements.push(<[u8; 12]>::try_from(e).unwrap_or([0; 12]));
        at += 12;
    }
    Ok(created(&elements))
}

/// The binding itself: the programs of the shader whose header is at `header` in
/// `object`, for D3D's declaration object `decl` with a stride per stream in dwords.
pub fn bind_programs(
    t: &BindTables,
    decl: &[u8],
    strides: &[u8; 16],
    object: &[u8],
    header: usize,
    program: &mut [u8],
) -> Result<()> {
    if header + 40 > object.len() {
        return Err(Error::new("a vertex shader's header is outside it"));
    }
    // Two programs when the header's 0x20 is set, each with its table at the header +
    // the header's word at 24 + 8 * program.
    let programs = if get32(object, header) & 0x20 != 0 { 2 } else { 1 };
    for v in 0..programs {
        let table = header + get32(object, header + 24 + 8 * v) as usize;
        if table + 40 > object.len() {
            return Err(Error::new("a vertex shader object's table is outside it"));
        }
        let tab = &object[table..];
        let count = get32(tab, 28) as usize;
        let list = table + (get32(tab, 24) as usize + 9) * 4;
        if list + 4 * count > object.len() {
            return Err(Error::new("a vertex shader's fetch list is outside it"));
        }
        let microcode = get32(tab, 0) as usize; // its offset in the program
        if microcode > program.len() {
            return Err(Error::new("a vertex shader's microcode is outside its program"));
        }
        // InitVertexShader (sub_820D7588) keeps a copy of each fetch.
        let mut copies = vec![0u8; 12 * count];
        for k in 0..count {
            let at = microcode + (get32(object, list + 4 * k) & 0xFFF) as usize * 12;
            let Some(fetch) = program.get(at..at + 12) else {
                return Err(Error::new("a vertex fetch is past its shader's program"));
            };
            copies[12 * k..12 * k + 12].copy_from_slice(fetch);
        }
        patch(t, tab, &object[list..], &copies, decl, strides, &mut program[microcode..])?;
    }
    Ok(())
}
