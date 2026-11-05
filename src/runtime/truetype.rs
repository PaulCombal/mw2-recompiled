// The fonts of the system's screens (screen.rs), rasterised as stb_truetype 1.26 does it in
// the copy that comes with Dear ImGui (imstb_truetype.h), as the C++ build's screens
// (runtime/screen.cpp) call it: stbtt_PackBegin with padding 1 and no oversampling, stbtt_PackFontRange for
// glyphs 32 to 126, stbtt_PackEnd, then stbtt_InitFont, stbtt_GetFontVMetrics and
// stbtt_ScaleForPixelHeight for the ascent. Only what those reach is here: the table
// directory, cmap formats 0, 4, 6, 12 and 13, glyf's simple and compound glyphs, CFF's
// Type 2 charstrings for an OpenType font with no glyf, hmtx, curve flattening, the
// version 2 scanline rasteriser with its float coverage, and stb_truetype's own row packer
// (the C++ does not include stb_rect_pack, so imstb_truetype.h's fallback runs).
// With one sample a pixel the box filters never run and are left out.
//
// Each step is the C's, in the same order and precision: f32 where the C++ build used
// float (sqrt, fabs and floor took float overloads there, so nothing is done in double),
// a float's conversion to int as x86's cvttss2si gives it (the lowest int when out of
// range or NaN), int sums and products wrapping as the machine did. tools/font_diff.py
// holds the atlas, the glyphs, the result and the ascent to the C's, bit for bit.
//
// stb_truetype trusts the font: on damaged data it reads outside it, writes outside its
// buffers, goes on with fields it never set or recurses until the stack is gone. Where the
// C would do any of that, this refuses the whole font instead (Malformed), and bake leaves
// an empty atlas and says nothing fitted; where the C stays inside, this gives what it gives.
// A compound glyph whose transform takes a point or a curve's control point out of a short is
// refused too, since the C's result is undefined
// there and differs between its vectorised and scalar code. Compound glyphs nest at most
// MAX_COMPONENT_DEPTH deep, where the C has no limit but its stack.
//
// stb_truetype is Sean Barrett's and its contributors', released into the public domain
// (or, at the user's choice, under the MIT licence), and taken here under the former.

/// stb_truetype's stbtt_packedchar: where a glyph is in the atlas, where its box goes from
/// the pen, and how far the pen moves.
#[derive(Copy, Clone, Default, Debug, PartialEq)]
pub struct Glyph {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
    pub xoff: f32,
    pub yoff: f32,
    pub xadvance: f32,
    pub xoff2: f32,
    pub yoff2: f32,
}

const FIRST_CHAR: i32 = 32;
const CHARS: usize = 95;
const PADDING: i32 = 1;
const OVERSAMPLE: u32 = 1;
const MAX_COMPONENT_DEPTH: u32 = 64;

/// Glyphs 32..=126 of `font` at `size` px into a `side`×`side` atlas (zeroed by the caller),
/// and the ascent at that size; false when some did not fit.
/// A font it refuses leaves an empty atlas and no glyphs.
pub fn bake(font: &[u8], size: f32, atlas: &mut [u8], side: usize, glyphs: &mut [Glyph; CHARS]) -> (f32, bool) {
    bake_checked(font, size, atlas, side, glyphs).unwrap_or_else(|Malformed| {
        atlas.fill(0);
        *glyphs = [Glyph::default(); CHARS];
        (0.0, false)
    })
}

/// A font on which stb_truetype would not stay inside its buffers or its own state.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed;

type Result<T> = std::result::Result<T, Malformed>;

/// bake, telling a font stb_truetype could not have read apart from one that did not fit.
pub fn bake_checked(
    font: &[u8],
    size: f32,
    atlas: &mut [u8],
    side: usize,
    glyphs: &mut [Glyph; CHARS],
) -> Result<(f32, bool)> {
    let font = Font(font);
    let width = i32::try_from(side).map_err(|_| Malformed)?;
    let pixels = side.checked_mul(side).ok_or(Malformed)?;
    let atlas = atlas.get_mut(..pixels).ok_or(Malformed)?;
    // stbtt_PackBegin
    let mut packer = Packer { width: width - PADDING, height: width - PADDING, x: 0, y: 0, bottom_y: 0 };
    atlas.fill(0);
    let fitted = pack_font_range(font, size, atlas, width, &mut packer, glyphs)?;
    // stbtt_InitFont(&info, data, 0), whose result the C++ does not look at: the
    // metrics come from the hhea it found whether it went on or not.
    let mut info = Info::new(font, 0);
    info.init()?;
    Ok((f32::from(info.ascent()?) * info.scale_for_pixel_height(size)?, fitted))
}

// ---------------------------------------------------------------------------------------
// Reading the font

#[derive(Clone, Copy)]
struct Font<'a>(&'a [u8]);

impl Font<'_> {
    fn byte(self, at: i64) -> Result<u8> {
        usize::try_from(at).ok().and_then(|at| self.0.get(at)).copied().ok_or(Malformed)
    }
    fn ushort(self, at: i64) -> Result<u16> {
        Ok(u16::from(self.byte(at)?) << 8 | u16::from(self.byte(at + 1)?))
    }
    fn short(self, at: i64) -> Result<i16> {
        Ok(self.ushort(at)? as i16)
    }
    fn ulong(self, at: i64) -> Result<u32> {
        Ok(u32::from(self.byte(at)?) << 24
            | u32::from(self.byte(at + 1)?) << 16
            | u32::from(self.byte(at + 2)?) << 8
            | u32::from(self.byte(at + 3)?))
    }
    fn tag(self, at: i64, tag: &[u8; 4]) -> Result<bool> {
        // stbtt_tag4: each byte is read only if the ones before matched.
        for (i, &c) in tag.iter().enumerate() {
            if self.byte(at + i as i64)? != c {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// What C's (int) does to a float on x86: truncation, or the lowest int when it does not fit.
fn ftoi(v: f32) -> i32 {
    if (-2_147_483_648.0..2_147_483_648.0).contains(&v) { v as i32 } else { i32::MIN }
}

// C's (short) of a float, which is undefined out of range: clang truncates some vertices and
// saturates others in the same loop once it is vectorised. Refused there.
fn to_short(v: f32) -> Result<i16> {
    if v > -32769.0 && v < 32768.0 { Ok(v as i16) } else { Err(Malformed) }
}

fn ifloor(v: f32) -> i32 {
    ftoi(v.floor())
}

fn iceil(v: f32) -> i32 {
    ftoi(v.ceil())
}

// stbtt__buf: a window on the font, read as stb reads it (0 past its end).
#[derive(Clone, Copy, Default)]
struct Buf {
    base: i64,
    cursor: i32,
    size: i32,
}

impl Buf {
    fn get8(&mut self, font: Font) -> Result<u8> {
        if self.cursor >= self.size {
            return Ok(0);
        }
        let v = font.byte(self.base + i64::from(self.cursor))?;
        self.cursor += 1;
        Ok(v)
    }
    fn peek8(&self, font: Font) -> Result<u8> {
        if self.cursor >= self.size {
            return Ok(0);
        }
        font.byte(self.base + i64::from(self.cursor))
    }
    fn seek(&mut self, o: i32) {
        self.cursor = if o > self.size || o < 0 { self.size } else { o };
    }
    fn skip(&mut self, o: i32) {
        self.seek(self.cursor.wrapping_add(o));
    }
    fn get(&mut self, font: Font, n: i32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = v << 8 | u32::from(self.get8(font)?);
        }
        Ok(v)
    }
    fn get16(&mut self, font: Font) -> Result<u32> {
        self.get(font, 2)
    }
    fn get32(&mut self, font: Font) -> Result<u32> {
        self.get(font, 4)
    }
    fn range(&self, o: i32, s: i32) -> Buf {
        if o < 0 || s < 0 || o > self.size || s > self.size - o {
            return Buf::default();
        }
        Buf { base: self.base + i64::from(o), cursor: 0, size: s }
    }
}

fn cff_get_index(b: &mut Buf, font: Font) -> Result<Buf> {
    let start = b.cursor;
    let count = b.get16(font)? as i32;
    if count != 0 {
        let offsize = i32::from(b.get8(font)?);
        b.skip(offsize.wrapping_mul(count));
        let last = b.get(font, offsize)?;
        b.skip(last.wrapping_sub(1) as i32);
    }
    Ok(b.range(start, b.cursor - start))
}

fn cff_int(b: &mut Buf, font: Font) -> Result<u32> {
    let b0 = i32::from(b.get8(font)?);
    Ok(match b0 {
        32..=246 => (b0 - 139) as u32,
        247..=250 => ((b0 - 247) * 256 + i32::from(b.get8(font)?) + 108) as u32,
        251..=254 => (-(b0 - 251) * 256 - i32::from(b.get8(font)?) - 108) as u32,
        28 => b.get16(font)?,
        29 => b.get32(font)?,
        _ => 0,
    })
}

fn cff_skip_operand(b: &mut Buf, font: Font) -> Result<()> {
    let b0 = b.peek8(font)?;
    if b0 == 30 {
        b.skip(1);
        while b.cursor < b.size {
            let v = b.get8(font)?;
            if v & 0xF == 0xF || v >> 4 == 0xF {
                break;
            }
        }
    } else {
        cff_int(b, font)?;
    }
    Ok(())
}

fn dict_get(b: &mut Buf, font: Font, key: i32) -> Result<Buf> {
    b.seek(0);
    while b.cursor < b.size {
        let start = b.cursor;
        while b.peek8(font)? >= 28 {
            cff_skip_operand(b, font)?;
        }
        let end = b.cursor;
        let mut op = i32::from(b.get8(font)?);
        if op == 12 {
            op = i32::from(b.get8(font)?) | 0x100;
        }
        if op == key {
            return Ok(b.range(start, end - start));
        }
    }
    Ok(b.range(0, 0))
}

fn dict_get_ints(b: &mut Buf, font: Font, key: i32, out: &mut [u32]) -> Result<()> {
    let mut operands = dict_get(b, font, key)?;
    for value in out.iter_mut() {
        if operands.cursor >= operands.size {
            break;
        }
        *value = cff_int(&mut operands, font)?;
    }
    Ok(())
}

fn cff_index_count(b: &mut Buf, font: Font) -> Result<i32> {
    b.seek(0);
    Ok(b.get16(font)? as i32)
}

fn cff_index_get(mut b: Buf, font: Font, i: i32) -> Result<Buf> {
    b.seek(0);
    let count = b.get16(font)? as i32;
    let offsize = i32::from(b.get8(font)?);
    b.skip(i.wrapping_mul(offsize));
    let start = b.get(font, offsize)? as i32;
    let end = b.get(font, offsize)? as i32;
    Ok(b.range(
        2i32.wrapping_add(count.wrapping_add(1).wrapping_mul(offsize)).wrapping_add(start),
        end.wrapping_sub(start),
    ))
}

fn get_subrs(mut cff: Buf, font: Font, mut fontdict: Buf) -> Result<Buf> {
    let mut private_loc = [0u32; 2];
    dict_get_ints(&mut fontdict, font, 18, &mut private_loc)?;
    if private_loc[1] == 0 || private_loc[0] == 0 {
        return Ok(Buf::default());
    }
    let mut pdict = cff.range(private_loc[1] as i32, private_loc[0] as i32);
    let mut subrsoff = [0u32];
    dict_get_ints(&mut pdict, font, 19, &mut subrsoff)?;
    if subrsoff[0] == 0 {
        return Ok(Buf::default());
    }
    cff.seek(private_loc[1].wrapping_add(subrsoff[0]) as i32);
    cff_get_index(&mut cff, font)
}

// stbtt_fontinfo, less what bake never reads.
struct Info<'a> {
    font: Font<'a>,
    fontstart: i32,
    num_glyphs: i32,
    loca: i32,
    head: i32,
    glyf: i32,
    hhea: i32,
    hmtx: i32,
    index_map: i32,
    index_to_loc_format: i32,
    cff: Buf,
    charstrings: Buf,
    gsubrs: Buf,
    subrs: Buf,
    fontdicts: Buf,
    fdselect: Buf,
}

fn find_table(font: Font, fontstart: u32, tag: &[u8; 4]) -> Result<u32> {
    let num_tables = i32::from(font.ushort(i64::from(fontstart) + 4)?);
    let tabledir = fontstart.wrapping_add(12);
    for i in 0..num_tables {
        let loc = tabledir.wrapping_add((16 * i) as u32);
        if font.tag(i64::from(loc), tag)? {
            return font.ulong(i64::from(loc) + 8);
        }
    }
    Ok(0)
}

fn is_font(font: Font) -> Result<bool> {
    Ok(font.tag(0, b"1\0\0\0")?
        || font.tag(0, b"typ1")?
        || font.tag(0, b"OTTO")?
        || font.tag(0, b"\0\x01\0\0")?
        || font.tag(0, b"true")?)
}

// stbtt_GetFontOffsetForIndex(data, 0)
fn offset_for_first_font(font: Font) -> Result<i32> {
    if is_font(font)? {
        return Ok(0);
    }
    if font.tag(0, b"ttcf")? {
        let version = font.ulong(4)?;
        if version == 0x0001_0000 || version == 0x0002_0000 {
            let n = font.ulong(8)? as i32;
            if 0 >= n {
                return Ok(-1);
            }
            return Ok(font.ulong(12)? as i32);
        }
    }
    Ok(-1)
}

impl<'a> Info<'a> {
    fn new(font: Font<'a>, fontstart: i32) -> Self {
        Info {
            font,
            fontstart,
            num_glyphs: 0,
            loca: 0,
            head: 0,
            glyf: 0,
            hhea: 0,
            hmtx: 0,
            index_map: 0,
            index_to_loc_format: 0,
            cff: Buf::default(),
            charstrings: Buf::default(),
            gsubrs: Buf::default(),
            subrs: Buf::default(),
            fontdicts: Buf::default(),
            fdselect: Buf::default(),
        }
    }

    // stbtt_InitFont: false where stb returns 0, having set only some of the fields.
    fn init(&mut self) -> Result<bool> {
        let font = self.font;
        let start = self.fontstart as u32;
        let cmap = find_table(font, start, b"cmap")?;
        self.loca = find_table(font, start, b"loca")? as i32;
        self.head = find_table(font, start, b"head")? as i32;
        self.glyf = find_table(font, start, b"glyf")? as i32;
        self.hhea = find_table(font, start, b"hhea")? as i32;
        self.hmtx = find_table(font, start, b"hmtx")? as i32;
        find_table(font, start, b"kern")?;
        find_table(font, start, b"GPOS")?;
        if cmap == 0 || self.head == 0 || self.hhea == 0 || self.hmtx == 0 {
            return Ok(false);
        }
        if self.glyf != 0 {
            if self.loca == 0 {
                return Ok(false);
            }
        } else {
            let (mut cstype, mut charstrings, mut fdarrayoff, mut fdselectoff) = ([2u32], [0u32], [0u32], [0u32]);
            let cff = find_table(font, start, b"CFF ")?;
            if cff == 0 {
                return Ok(false);
            }
            self.fontdicts = Buf::default();
            self.fdselect = Buf::default();
            self.cff = Buf { base: i64::from(cff), cursor: 0, size: 512 * 1024 * 1024 };
            let mut b = self.cff;
            b.skip(2);
            let header_size = b.get8(font)?;
            b.seek(i32::from(header_size));
            cff_get_index(&mut b, font)?; // names
            let topdictidx = cff_get_index(&mut b, font)?;
            let mut topdict = cff_index_get(topdictidx, font, 0)?;
            cff_get_index(&mut b, font)?; // strings
            self.gsubrs = cff_get_index(&mut b, font)?;
            dict_get_ints(&mut topdict, font, 17, &mut charstrings)?;
            dict_get_ints(&mut topdict, font, 0x100 | 6, &mut cstype)?;
            dict_get_ints(&mut topdict, font, 0x100 | 36, &mut fdarrayoff)?;
            dict_get_ints(&mut topdict, font, 0x100 | 37, &mut fdselectoff)?;
            self.subrs = get_subrs(b, font, topdict)?;
            if cstype[0] != 2 || charstrings[0] == 0 {
                return Ok(false);
            }
            if fdarrayoff[0] != 0 {
                if fdselectoff[0] == 0 {
                    return Ok(false);
                }
                b.seek(fdarrayoff[0] as i32);
                self.fontdicts = cff_get_index(&mut b, font)?;
                self.fdselect = b.range(fdselectoff[0] as i32, b.size.wrapping_sub(fdselectoff[0] as i32));
            }
            b.seek(charstrings[0] as i32);
            self.charstrings = cff_get_index(&mut b, font)?;
        }
        let maxp = find_table(font, start, b"maxp")?;
        self.num_glyphs = if maxp != 0 { i32::from(font.ushort(i64::from(maxp) + 4)?) } else { 0xffff };
        let num_tables = i32::from(font.ushort(i64::from(cmap) + 2)?);
        self.index_map = 0;
        for i in 0..num_tables {
            let record = i64::from(cmap.wrapping_add(4).wrapping_add((8 * i) as u32));
            match font.ushort(record)? {
                3 => {
                    if let 1 | 10 = font.ushort(record + 2)? {
                        self.index_map = cmap.wrapping_add(font.ulong(record + 4)?) as i32;
                    }
                }
                0 => self.index_map = cmap.wrapping_add(font.ulong(record + 4)?) as i32,
                _ => {}
            }
        }
        if self.index_map == 0 {
            return Ok(false);
        }
        self.index_to_loc_format = i32::from(font.ushort(i64::from(self.head) + 50)?);
        Ok(true)
    }

    // stbtt_GetFontVMetrics's ascent: the C++ uses neither the descent nor the gap.
    fn ascent(&self) -> Result<i16> {
        self.font.short(i64::from(self.hhea) + 4)
    }

    fn scale_for_pixel_height(&self, height: f32) -> Result<f32> {
        let hhea = i64::from(self.hhea);
        let fheight = i32::from(self.font.short(hhea + 4)?) - i32::from(self.font.short(hhea + 6)?);
        Ok(height / fheight as f32)
    }

    fn scale_for_mapping_em_to_pixels(&self, pixels: f32) -> Result<f32> {
        let units_per_em = i32::from(self.font.ushort(i64::from(self.head) + 18)?);
        Ok(pixels / units_per_em as f32)
    }

    fn find_glyph_index(&self, codepoint: i32) -> Result<i32> {
        let font = self.font;
        let index_map = i64::from(self.index_map as u32);
        let format = font.ushort(index_map)?;
        match format {
            0 => {
                let bytes = i32::from(font.ushort(index_map + 2)?);
                if codepoint < bytes - 6 {
                    return Ok(i32::from(font.byte(index_map + 6 + i64::from(codepoint))?));
                }
                Ok(0)
            }
            6 => {
                let first = u32::from(font.ushort(index_map + 6)?);
                let count = u32::from(font.ushort(index_map + 8)?);
                if codepoint as u32 >= first && (codepoint as u32) < first + count {
                    let at = index_map + 10 + i64::from((codepoint as u32).wrapping_sub(first).wrapping_mul(2));
                    return Ok(i32::from(font.ushort(at)?));
                }
                Ok(0)
            }
            4 => {
                let segcount = font.ushort(index_map + 6)? >> 1;
                let mut search_range = font.ushort(index_map + 8)? >> 1;
                let mut entry_selector = font.ushort(index_map + 10)?;
                let range_shift = font.ushort(index_map + 12)? >> 1;
                let end_count = (index_map as u32).wrapping_add(14);
                let mut search = end_count;
                if codepoint > 0xffff {
                    return Ok(0);
                }
                if codepoint >= i32::from(font.ushort(i64::from(search) + i64::from(range_shift) * 2)?) {
                    search = search.wrapping_add(u32::from(range_shift) * 2);
                }
                search = search.wrapping_sub(2);
                while entry_selector != 0 {
                    search_range >>= 1;
                    let end = font.ushort(i64::from(search) + i64::from(search_range) * 2)?;
                    if codepoint > i32::from(end) {
                        search = search.wrapping_add(u32::from(search_range) * 2);
                    }
                    entry_selector -= 1;
                }
                search = search.wrapping_add(2);
                let item = i64::from((search.wrapping_sub(end_count) >> 1) as u16);
                let segcount = i64::from(segcount);
                let start = font.ushort(index_map + 14 + segcount * 2 + 2 + 2 * item)?;
                let last = font.ushort(i64::from(end_count) + 2 * item)?;
                if codepoint < i32::from(start) || codepoint > i32::from(last) {
                    return Ok(0);
                }
                let offset = font.ushort(index_map + 14 + segcount * 6 + 2 + 2 * item)?;
                if offset == 0 {
                    let delta = font.short(index_map + 14 + segcount * 4 + 2 + 2 * item)?;
                    return Ok(i32::from(codepoint.wrapping_add(i32::from(delta)) as u16));
                }
                let at = i64::from(offset)
                    + i64::from(codepoint - i32::from(start)) * 2
                    + index_map
                    + 14
                    + segcount * 6
                    + 2
                    + 2 * item;
                Ok(i32::from(font.ushort(at)?))
            }
            12 | 13 => {
                let ngroups = font.ulong(index_map + 12)?;
                let (mut low, mut high) = (0i32, ngroups as i32);
                while low < high {
                    let mid = low + ((high - low) >> 1);
                    let group = index_map + 16 + i64::from(mid) * 12;
                    let start_char = font.ulong(group)?;
                    let end_char = font.ulong(group + 4)?;
                    if (codepoint as u32) < start_char {
                        high = mid;
                    } else if codepoint as u32 > end_char {
                        low = mid + 1;
                    } else {
                        let start_glyph = font.ulong(group + 8)?;
                        if format == 12 {
                            return Ok(start_glyph.wrapping_add(codepoint as u32).wrapping_sub(start_char) as i32);
                        }
                        return Ok(start_glyph as i32);
                    }
                }
                Ok(0)
            }
            _ => Ok(0), // format 2 and the unknown: stb asserts, then gives 0
        }
    }

    // stbtt_GetGlyphHMetrics's advance. Its left side bearing is not used, and the compiled C
    // does not read it, so neither does this: past the font's end it would refuse for nothing.
    fn advance(&self, glyph: i32) -> Result<i16> {
        let font = self.font;
        let hmtx = i64::from(self.hmtx);
        let long_metrics = i32::from(font.ushort(i64::from(self.hhea) + 34)?);
        let entry = if glyph < long_metrics { glyph } else { long_metrics - 1 };
        font.short(hmtx + 4 * i64::from(entry))
    }

    fn glyf_offset(&self, glyph: i32) -> Result<i32> {
        if glyph >= self.num_glyphs || self.index_to_loc_format >= 2 {
            return Ok(-1);
        }
        let (font, loca) = (self.font, i64::from(self.loca));
        let (g1, g2) = if self.index_to_loc_format == 0 {
            let at = loca + i64::from(glyph) * 2;
            (
                self.glyf.wrapping_add(i32::from(font.ushort(at)?) * 2),
                self.glyf.wrapping_add(i32::from(font.ushort(at + 2)?) * 2),
            )
        } else {
            let at = loca + i64::from(glyph) * 4;
            (
                (self.glyf as u32).wrapping_add(font.ulong(at)?) as i32,
                (self.glyf as u32).wrapping_add(font.ulong(at + 4)?) as i32,
            )
        };
        Ok(if g1 == g2 { -1 } else { g1 })
    }

    // stbtt_GetGlyphBox: None for a glyph with no outline.
    fn glyph_box(&self, glyph: i32) -> Result<Option<(i32, i32, i32, i32)>> {
        if self.cff.size != 0 {
            let mut c = Charstring::new(true);
            let ran = self.run_charstring(glyph, &mut c)?;
            return Ok(Some(if ran { (c.min_x, c.min_y, c.max_x, c.max_y) } else { (0, 0, 0, 0) }));
        }
        let g = self.glyf_offset(glyph)?;
        if g < 0 {
            return Ok(None);
        }
        let (font, g) = (self.font, i64::from(g));
        Ok(Some((
            i32::from(font.short(g + 2)?),
            i32::from(font.short(g + 4)?),
            i32::from(font.short(g + 6)?),
            i32::from(font.short(g + 8)?),
        )))
    }

    // stbtt_GetGlyphBitmapBoxSubpixel
    fn bitmap_box(&self, glyph: i32, scale_x: f32, scale_y: f32, shift_x: f32, shift_y: f32) -> Result<[i32; 4]> {
        Ok(match self.glyph_box(glyph)? {
            None => [0; 4],
            Some((x0, y0, x1, y1)) => [
                ifloor(x0 as f32 * scale_x + shift_x),
                ifloor(-y1 as f32 * scale_y + shift_y),
                iceil(x1 as f32 * scale_x + shift_x),
                iceil(-y0 as f32 * scale_y + shift_y),
            ],
        })
    }

    fn glyph_shape(&self, glyph: i32, depth: u32) -> Result<Vec<Vertex>> {
        if self.cff.size == 0 { self.glyph_shape_tt(glyph, depth) } else { self.glyph_shape_t2(glyph) }
    }

    fn glyph_shape_tt(&self, glyph: i32, depth: u32) -> Result<Vec<Vertex>> {
        if depth > MAX_COMPONENT_DEPTH {
            return Err(Malformed);
        }
        let font = self.font;
        let g = self.glyf_offset(glyph)?;
        if g < 0 {
            return Ok(Vec::new());
        }
        let g = i64::from(g);
        let number_of_contours = font.short(g)?;
        if number_of_contours > 0 {
            simple_glyph(font, g, i32::from(number_of_contours))
        } else if number_of_contours < 0 {
            self.compound_glyph(g, depth)
        } else {
            Ok(Vec::new())
        }
    }

    fn compound_glyph(&self, g: i64, depth: u32) -> Result<Vec<Vertex>> {
        let font = self.font;
        let mut comp = g + 10;
        let mut vertices = Vec::new();
        loop {
            let mut mtx = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
            let flags = font.short(comp)? as u16;
            comp += 2;
            let gidx = font.short(comp)? as u16;
            comp += 2;
            if flags & 2 != 0 {
                if flags & 1 != 0 {
                    mtx[4] = f32::from(font.short(comp)?);
                    comp += 2;
                    mtx[5] = f32::from(font.short(comp)?);
                    comp += 2;
                } else {
                    mtx[4] = f32::from(font.byte(comp)? as i8);
                    comp += 1;
                    mtx[5] = f32::from(font.byte(comp)? as i8);
                    comp += 1;
                }
            }
            let f2dot14 = |at: i64| -> Result<f32> { Ok(f32::from(font.short(at)?) / 16384.0) };
            if flags & (1 << 3) != 0 {
                mtx[0] = f2dot14(comp)?;
                mtx[3] = mtx[0];
                comp += 2;
                mtx[1] = 0.0;
                mtx[2] = 0.0;
            } else if flags & (1 << 6) != 0 {
                mtx[0] = f2dot14(comp)?;
                comp += 2;
                mtx[1] = 0.0;
                mtx[2] = 0.0;
                mtx[3] = f2dot14(comp)?;
                comp += 2;
            } else if flags & (1 << 7) != 0 {
                for value in &mut mtx[..4] {
                    *value = f2dot14(comp)?;
                    comp += 2;
                }
            }
            let m = (mtx[0] * mtx[0] + mtx[1] * mtx[1]).sqrt();
            let n = (mtx[2] * mtx[2] + mtx[3] * mtx[3]).sqrt();
            let mut comp_verts = self.glyph_shape(i32::from(gidx), depth + 1)?;
            for v in &mut comp_verts {
                let (x, y) = (f32::from(v.x), f32::from(v.y));
                v.x = to_short(m * (mtx[0] * x + mtx[2] * y + mtx[4]))?;
                v.y = to_short(n * (mtx[1] * x + mtx[3] * y + mtx[5]))?;
                let (x, y) = (f32::from(v.cx), f32::from(v.cy));
                let (cx, cy) = (m * (mtx[0] * x + mtx[2] * y + mtx[4]), n * (mtx[1] * x + mtx[3] * y + mtx[5]));
                // A move's or a line's control point is never read, so where it lands
                // changes nothing, and only a curve's is held to a short.
                if v.kind == VCURVE || v.kind == VCUBIC {
                    (v.cx, v.cy) = (to_short(cx)?, to_short(cy)?);
                } else {
                    (v.cx, v.cy) = (cx as i16, cy as i16);
                }
            }
            vertices.append(&mut comp_verts);
            if flags & (1 << 5) == 0 {
                return Ok(vertices);
            }
        }
    }
}

const VMOVE: u8 = 1;
const VLINE: u8 = 2;
const VCURVE: u8 = 3;
const VCUBIC: u8 = 4;

#[derive(Clone, Copy, Default)]
struct Vertex {
    x: i16,
    y: i16,
    cx: i16,
    cy: i16,
    cx1: i16,
    cy1: i16,
    kind: u8,
}

fn set_vertex(vertices: &mut [Vertex], at: usize, kind: u8, x: i32, y: i32, cx: i32, cy: i32) -> Result<()> {
    let v = vertices.get_mut(at).ok_or(Malformed)?;
    v.kind = kind;
    v.x = x as i16;
    v.y = y as i16;
    v.cx = cx as i16;
    v.cy = cy as i16;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // stbtt__close_shape's
fn close_shape(
    vertices: &mut [Vertex],
    mut num_vertices: usize,
    was_off: bool,
    start_off: bool,
    (sx, sy): (i32, i32),
    (scx, scy): (i32, i32),
    (cx, cy): (i32, i32),
) -> Result<usize> {
    if start_off {
        if was_off {
            set_vertex(vertices, num_vertices, VCURVE, (cx + scx) >> 1, (cy + scy) >> 1, cx, cy)?;
            num_vertices += 1;
        }
        set_vertex(vertices, num_vertices, VCURVE, sx, sy, scx, scy)?;
    } else if was_off {
        set_vertex(vertices, num_vertices, VCURVE, sx, sy, cx, cy)?;
    } else {
        set_vertex(vertices, num_vertices, VLINE, sx, sy, 0, 0)?;
    }
    Ok(num_vertices + 1)
}

// A glyph of contours, decoded in one array as stb does: the points at its end, the
// outline written over them from its start.
fn simple_glyph(font: Font, g: i64, number_of_contours: i32) -> Result<Vec<Vertex>> {
    let contours = i64::from(number_of_contours);
    let end_pts = g + 10;
    let ins = i64::from(font.ushort(g + 10 + contours * 2)?);
    let mut points = g + 10 + contours * 2 + 2 + ins;
    let n = 1 + i32::from(font.ushort(end_pts + contours * 2 - 2)?);
    let m = n + 2 * number_of_contours;
    let mut vertices = vec![Vertex::default(); m as usize];
    let (n, off) = (n as usize, (m - n) as usize);

    let mut flags = 0u8;
    let mut flagcount = 0u8;
    for vertex in &mut vertices[off..off + n] {
        if flagcount == 0 {
            flags = font.byte(points)?;
            points += 1;
            if flags & 8 != 0 {
                flagcount = font.byte(points)?;
                points += 1;
            }
        } else {
            flagcount -= 1;
        }
        vertex.kind = flags;
    }
    let mut x = 0i32;
    for vertex in &mut vertices[off..off + n] {
        let flags = vertex.kind;
        if flags & 2 != 0 {
            let dx = i32::from(font.byte(points)?);
            points += 1;
            x = x.wrapping_add(if flags & 16 != 0 { dx } else { -dx });
        } else if flags & 16 == 0 {
            let delta = (i32::from(font.byte(points)?) * 256 + i32::from(font.byte(points + 1)?)) as i16;
            x = x.wrapping_add(i32::from(delta));
            points += 2;
        }
        vertex.x = x as i16;
    }
    let mut y = 0i32;
    for vertex in &mut vertices[off..off + n] {
        let flags = vertex.kind;
        if flags & 4 != 0 {
            let dy = i32::from(font.byte(points)?);
            points += 1;
            y = y.wrapping_add(if flags & 32 != 0 { dy } else { -dy });
        } else if flags & 32 == 0 {
            let delta = (i32::from(font.byte(points)?) * 256 + i32::from(font.byte(points + 1)?)) as i16;
            y = y.wrapping_add(i32::from(delta));
            points += 2;
        }
        vertex.y = y as i16;
    }

    let mut num_vertices = 0usize;
    let (mut sx, mut sy, mut cx, mut cy, mut scx, mut scy) = (0i32, 0i32, 0i32, 0i32, 0i32, 0i32);
    let (mut was_off, mut start_off) = (false, false);
    let mut next_move = 0usize;
    let mut j = 0i64;
    let mut i = 0usize;
    while i < n {
        let point = vertices[off + i];
        let (flags, x, y) = (point.kind, i32::from(point.x), i32::from(point.y));
        if next_move == i {
            if i != 0 {
                num_vertices =
                    close_shape(&mut vertices, num_vertices, was_off, start_off, (sx, sy), (scx, scy), (cx, cy))?;
            }
            start_off = flags & 1 == 0;
            if start_off {
                scx = x;
                scy = y;
                let next = *vertices.get(off + i + 1).ok_or(Malformed)?;
                if next.kind & 1 == 0 {
                    sx = (x + i32::from(next.x)) >> 1;
                    sy = (y + i32::from(next.y)) >> 1;
                } else {
                    sx = i32::from(next.x);
                    sy = i32::from(next.y);
                    i += 1;
                }
            } else {
                sx = x;
                sy = y;
            }
            set_vertex(&mut vertices, num_vertices, VMOVE, sx, sy, 0, 0)?;
            num_vertices += 1;
            was_off = false;
            next_move = 1 + usize::from(font.ushort(end_pts + j * 2)?);
            j += 1;
        } else if flags & 1 == 0 {
            if was_off {
                set_vertex(&mut vertices, num_vertices, VCURVE, (cx + x) >> 1, (cy + y) >> 1, cx, cy)?;
                num_vertices += 1;
            }
            cx = x;
            cy = y;
            was_off = true;
        } else {
            if was_off {
                set_vertex(&mut vertices, num_vertices, VCURVE, x, y, cx, cy)?;
            } else {
                set_vertex(&mut vertices, num_vertices, VLINE, x, y, 0, 0)?;
            }
            num_vertices += 1;
            was_off = false;
        }
        i += 1;
    }
    num_vertices = close_shape(&mut vertices, num_vertices, was_off, start_off, (sx, sy), (scx, scy), (cx, cy))?;
    vertices.truncate(num_vertices);
    Ok(vertices)
}

// ---------------------------------------------------------------------------------------
// CFF's Type 2 charstrings (stbtt__csctx and stbtt__run_charstring)

struct Charstring {
    bounds: bool,
    started: bool,
    first_x: f32,
    first_y: f32,
    x: f32,
    y: f32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    vertices: Vec<Vertex>,
}

impl Charstring {
    fn new(bounds: bool) -> Self {
        Charstring {
            bounds,
            started: false,
            first_x: 0.0,
            first_y: 0.0,
            x: 0.0,
            y: 0.0,
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
            vertices: Vec::new(),
        }
    }

    fn track_vertex(&mut self, x: i32, y: i32) {
        if x > self.max_x || !self.started {
            self.max_x = x;
        }
        if y > self.max_y || !self.started {
            self.max_y = y;
        }
        if x < self.min_x || !self.started {
            self.min_x = x;
        }
        if y < self.min_y || !self.started {
            self.min_y = y;
        }
        self.started = true;
    }

    #[allow(clippy::too_many_arguments)] // stbtt__csctx_v's
    fn v(&mut self, kind: u8, x: i32, y: i32, cx: i32, cy: i32, cx1: i32, cy1: i32) {
        if self.bounds {
            self.track_vertex(x, y);
            if kind == VCUBIC {
                self.track_vertex(cx, cy);
                self.track_vertex(cx1, cy1);
            }
        } else {
            self.vertices.push(Vertex {
                kind,
                x: x as i16,
                y: y as i16,
                cx: cx as i16,
                cy: cy as i16,
                cx1: cx1 as i16,
                cy1: cy1 as i16,
            });
        }
    }

    fn close_shape(&mut self) {
        if self.first_x != self.x || self.first_y != self.y {
            self.v(VLINE, ftoi(self.first_x), ftoi(self.first_y), 0, 0, 0, 0);
        }
    }

    fn rmove_to(&mut self, dx: f32, dy: f32) {
        self.close_shape();
        self.x += dx;
        self.first_x = self.x;
        self.y += dy;
        self.first_y = self.y;
        self.v(VMOVE, ftoi(self.x), ftoi(self.y), 0, 0, 0, 0);
    }

    fn rline_to(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.v(VLINE, ftoi(self.x), ftoi(self.y), 0, 0, 0, 0);
    }

    fn rccurve_to(&mut self, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
        let cx1 = self.x + dx1;
        let cy1 = self.y + dy1;
        let cx2 = cx1 + dx2;
        let cy2 = cy1 + dy2;
        self.x = cx2 + dx3;
        self.y = cy2 + dy3;
        self.v(VCUBIC, ftoi(self.x), ftoi(self.y), ftoi(cx1), ftoi(cy1), ftoi(cx2), ftoi(cy2));
    }
}

impl Info<'_> {
    fn get_subr(&self, mut idx: Buf, n: i32) -> Result<Buf> {
        let count = cff_index_count(&mut idx, self.font)?;
        let bias = if count >= 33900 {
            32768
        } else if count >= 1240 {
            1131
        } else {
            107
        };
        let n = n.wrapping_add(bias);
        if n < 0 || n >= count {
            return Ok(Buf::default());
        }
        cff_index_get(idx, self.font, n)
    }

    fn cid_glyph_subrs(&self, glyph: i32) -> Result<Buf> {
        let font = self.font;
        let mut fdselect = self.fdselect;
        let mut fdselector = -1;
        fdselect.seek(0);
        let fmt = fdselect.get8(font)?;
        if fmt == 0 {
            fdselect.skip(glyph);
            fdselector = i32::from(fdselect.get8(font)?);
        } else if fmt == 3 {
            let nranges = fdselect.get16(font)? as i32;
            let mut start = fdselect.get16(font)? as i32;
            for _ in 0..nranges {
                let v = i32::from(fdselect.get8(font)?);
                let end = fdselect.get16(font)? as i32;
                if glyph >= start && glyph < end {
                    fdselector = v;
                    break;
                }
                start = end;
            }
        }
        if fdselector == -1 {
            return Ok(Buf::default());
        }
        get_subrs(self.cff, font, cff_index_get(self.fontdicts, font, fdselector)?)
    }

    // stbtt__run_charstring: false where stb gives up on the glyph.
    fn run_charstring(&self, glyph: i32, c: &mut Charstring) -> Result<bool> {
        let font = self.font;
        let (mut in_header, mut maskbits, mut sp, mut has_subrs) = (true, 0i32, 0usize, false);
        let mut s = [0.0f32; 48];
        let mut subr_stack = [Buf::default(); 10];
        let mut subr_stack_height = 0usize;
        let mut subrs = self.subrs;
        let mut b = cff_index_get(self.charstrings, font, glyph)?;
        while b.cursor < b.size {
            let mut i = 0usize;
            let mut clear_stack = true;
            let b0 = b.get8(font)?;
            match b0 {
                0x13 | 0x14 => {
                    // hintmask, cntrmask
                    if in_header {
                        maskbits = maskbits.wrapping_add((sp / 2) as i32); // an implicit vstem
                    }
                    in_header = false;
                    b.skip(maskbits.wrapping_add(7) / 8);
                }
                0x01 | 0x03 | 0x12 | 0x17 => maskbits = maskbits.wrapping_add((sp / 2) as i32), // the stems
                0x15 => {
                    in_header = false;
                    if sp < 2 {
                        return Ok(false);
                    }
                    c.rmove_to(s[sp - 2], s[sp - 1]);
                }
                0x04 => {
                    in_header = false;
                    if sp < 1 {
                        return Ok(false);
                    }
                    c.rmove_to(0.0, s[sp - 1]);
                }
                0x16 => {
                    in_header = false;
                    if sp < 1 {
                        return Ok(false);
                    }
                    c.rmove_to(s[sp - 1], 0.0);
                }
                0x05 => {
                    if sp < 2 {
                        return Ok(false);
                    }
                    while i + 1 < sp {
                        c.rline_to(s[i], s[i + 1]);
                        i += 2;
                    }
                }
                0x06 | 0x07 => {
                    // hlineto, vlineto: alternating, from a different start
                    if sp < 1 {
                        return Ok(false);
                    }
                    let mut vertical = b0 == 0x07;
                    while i < sp {
                        if vertical {
                            c.rline_to(0.0, s[i]);
                        } else {
                            c.rline_to(s[i], 0.0);
                        }
                        i += 1;
                        vertical = !vertical;
                    }
                }
                0x1E | 0x1F => {
                    // vhcurveto, hvcurveto
                    if sp < 4 {
                        return Ok(false);
                    }
                    let mut horizontal = b0 == 0x1F;
                    while i + 3 < sp {
                        let last = if sp - i == 5 { s[i + 4] } else { 0.0 };
                        if horizontal {
                            c.rccurve_to(s[i], 0.0, s[i + 1], s[i + 2], last, s[i + 3]);
                        } else {
                            c.rccurve_to(0.0, s[i], s[i + 1], s[i + 2], s[i + 3], last);
                        }
                        i += 4;
                        horizontal = !horizontal;
                    }
                }
                0x08 => {
                    if sp < 6 {
                        return Ok(false);
                    }
                    while i + 5 < sp {
                        c.rccurve_to(s[i], s[i + 1], s[i + 2], s[i + 3], s[i + 4], s[i + 5]);
                        i += 6;
                    }
                }
                0x18 => {
                    // rcurveline
                    if sp < 8 {
                        return Ok(false);
                    }
                    while i + 5 < sp - 2 {
                        c.rccurve_to(s[i], s[i + 1], s[i + 2], s[i + 3], s[i + 4], s[i + 5]);
                        i += 6;
                    }
                    if i + 1 >= sp {
                        return Ok(false);
                    }
                    c.rline_to(s[i], s[i + 1]);
                }
                0x19 => {
                    // rlinecurve
                    if sp < 8 {
                        return Ok(false);
                    }
                    while i + 1 < sp - 6 {
                        c.rline_to(s[i], s[i + 1]);
                        i += 2;
                    }
                    if i + 5 >= sp {
                        return Ok(false);
                    }
                    c.rccurve_to(s[i], s[i + 1], s[i + 2], s[i + 3], s[i + 4], s[i + 5]);
                }
                0x1A | 0x1B => {
                    // vvcurveto, hhcurveto
                    if sp < 4 {
                        return Ok(false);
                    }
                    let mut f = 0.0f32;
                    if sp & 1 != 0 {
                        f = s[i];
                        i += 1;
                    }
                    while i + 3 < sp {
                        if b0 == 0x1B {
                            c.rccurve_to(s[i], f, s[i + 1], s[i + 2], s[i + 3], 0.0);
                        } else {
                            c.rccurve_to(f, s[i], s[i + 1], s[i + 2], 0.0, s[i + 3]);
                        }
                        f = 0.0;
                        i += 4;
                    }
                }
                0x0A | 0x1D => {
                    // callsubr, callgsubr
                    if b0 == 0x0A && !has_subrs {
                        if self.fdselect.size != 0 {
                            subrs = self.cid_glyph_subrs(glyph)?;
                        }
                        has_subrs = true;
                    }
                    if sp < 1 {
                        return Ok(false);
                    }
                    sp -= 1;
                    let v = ftoi(s[sp]);
                    if subr_stack_height >= 10 {
                        return Ok(false);
                    }
                    subr_stack[subr_stack_height] = b;
                    subr_stack_height += 1;
                    b = self.get_subr(if b0 == 0x0A { subrs } else { self.gsubrs }, v)?;
                    if b.size == 0 {
                        return Ok(false);
                    }
                    b.cursor = 0;
                    clear_stack = false;
                }
                0x0B => {
                    // return
                    if subr_stack_height == 0 {
                        return Ok(false);
                    }
                    subr_stack_height -= 1;
                    b = subr_stack[subr_stack_height];
                    clear_stack = false;
                }
                0x0E => {
                    // endchar
                    c.close_shape();
                    return Ok(true);
                }
                0x0C => {
                    let b1 = b.get8(font)?;
                    match b1 {
                        0x22 => {
                            // hflex
                            if sp < 7 {
                                return Ok(false);
                            }
                            let (dx1, dx2, dy2, dx3, dx4, dx5, dx6) = (s[0], s[1], s[2], s[3], s[4], s[5], s[6]);
                            c.rccurve_to(dx1, 0.0, dx2, dy2, dx3, 0.0);
                            c.rccurve_to(dx4, 0.0, dx5, -dy2, dx6, 0.0);
                        }
                        0x23 => {
                            // flex
                            if sp < 13 {
                                return Ok(false);
                            }
                            c.rccurve_to(s[0], s[1], s[2], s[3], s[4], s[5]);
                            c.rccurve_to(s[6], s[7], s[8], s[9], s[10], s[11]);
                        }
                        0x24 => {
                            // hflex1
                            if sp < 9 {
                                return Ok(false);
                            }
                            let (dx1, dy1, dx2, dy2, dx3) = (s[0], s[1], s[2], s[3], s[4]);
                            let (dx4, dx5, dy5, dx6) = (s[5], s[6], s[7], s[8]);
                            c.rccurve_to(dx1, dy1, dx2, dy2, dx3, 0.0);
                            c.rccurve_to(dx4, 0.0, dx5, dy5, dx6, -(dy1 + dy2 + dy5));
                        }
                        0x25 => {
                            // flex1
                            if sp < 11 {
                                return Ok(false);
                            }
                            let (dx1, dy1, dx2, dy2, dx3, dy3) = (s[0], s[1], s[2], s[3], s[4], s[5]);
                            let (dx4, dy4, dx5, dy5) = (s[6], s[7], s[8], s[9]);
                            let (mut dx6, mut dy6) = (s[10], s[10]);
                            let dx = dx1 + dx2 + dx3 + dx4 + dx5;
                            let dy = dy1 + dy2 + dy3 + dy4 + dy5;
                            if dx.abs() > dy.abs() {
                                dy6 = -dy;
                            } else {
                                dx6 = -dx;
                            }
                            c.rccurve_to(dx1, dy1, dx2, dy2, dx3, dy3);
                            c.rccurve_to(dx4, dy4, dx5, dy5, dx6, dy6);
                        }
                        _ => return Ok(false),
                    }
                }
                _ => {
                    if b0 != 255 && b0 != 28 && b0 < 32 {
                        return Ok(false); // a reserved operator
                    }
                    let f = if b0 == 255 {
                        b.get32(font)? as i32 as f32 / 65536.0
                    } else {
                        b.skip(-1);
                        f32::from(cff_int(&mut b, font)? as i16)
                    };
                    if sp >= 48 {
                        return Ok(false);
                    }
                    s[sp] = f;
                    sp += 1;
                    clear_stack = false;
                }
            }
            if clear_stack {
                sp = 0;
            }
        }
        Ok(false) // no endchar
    }

    fn glyph_shape_t2(&self, glyph: i32) -> Result<Vec<Vertex>> {
        // stb runs the charstring twice, to count and then to write: the same run.
        let mut c = Charstring::new(false);
        Ok(if self.run_charstring(glyph, &mut c)? { c.vertices } else { Vec::new() })
    }
}

// ---------------------------------------------------------------------------------------
// Flattening and rasterising

#[derive(Clone, Copy)]
struct Point {
    x: f32,
    y: f32,
}

#[allow(clippy::too_many_arguments)] // stbtt__tesselate_curve's
fn tesselate_curve(points: &mut Vec<Point>, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32, flat: f32, n: i32) {
    let mx = (x0 + 2.0 * x1 + x2) / 4.0;
    let my = (y0 + 2.0 * y1 + y2) / 4.0;
    let dx = (x0 + x2) / 2.0 - mx;
    let dy = (y0 + y2) / 2.0 - my;
    if n > 16 {
        return;
    }
    if dx * dx + dy * dy > flat {
        tesselate_curve(points, x0, y0, (x0 + x1) / 2.0, (y0 + y1) / 2.0, mx, my, flat, n + 1);
        tesselate_curve(points, mx, my, (x1 + x2) / 2.0, (y1 + y2) / 2.0, x2, y2, flat, n + 1);
    } else {
        points.push(Point { x: x2, y: y2 });
    }
}

#[allow(clippy::too_many_arguments)] // stbtt__tesselate_cubic's
fn tesselate_cubic(
    points: &mut Vec<Point>,
    (x0, y0): (f32, f32),
    (x1, y1): (f32, f32),
    (x2, y2): (f32, f32),
    (x3, y3): (f32, f32),
    flat: f32,
    n: i32,
) {
    let (dx0, dy0) = (x1 - x0, y1 - y0);
    let (dx1, dy1) = (x2 - x1, y2 - y1);
    let (dx2, dy2) = (x3 - x2, y3 - y2);
    let (dx, dy) = (x3 - x0, y3 - y0);
    let longlen = (dx0 * dx0 + dy0 * dy0).sqrt() + (dx1 * dx1 + dy1 * dy1).sqrt() + (dx2 * dx2 + dy2 * dy2).sqrt();
    let shortlen = (dx * dx + dy * dy).sqrt();
    let flatness_squared = longlen * longlen - shortlen * shortlen;
    if n > 16 {
        return;
    }
    if flatness_squared > flat {
        let (x01, y01) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (x12, y12) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        let (x23, y23) = ((x2 + x3) / 2.0, (y2 + y3) / 2.0);
        let (xa, ya) = ((x01 + x12) / 2.0, (y01 + y12) / 2.0);
        let (xb, yb) = ((x12 + x23) / 2.0, (y12 + y23) / 2.0);
        let (mx, my) = ((xa + xb) / 2.0, (ya + yb) / 2.0);
        tesselate_cubic(points, (x0, y0), (x01, y01), (xa, ya), (mx, my), flat, n + 1);
        tesselate_cubic(points, (mx, my), (xb, yb), (x23, y23), (x3, y3), flat, n + 1);
    } else {
        points.push(Point { x: x3, y: y3 });
    }
}

// stbtt_FlattenCurves: the points, and how many make each contour; None with no contour.
// Points before the first move stay at the front, counted in no contour, as in stb.
fn flatten_curves(vertices: &[Vertex], objspace_flatness: f32) -> Option<(Vec<Point>, Vec<usize>)> {
    let flat = objspace_flatness * objspace_flatness;
    let contours = vertices.iter().filter(|v| v.kind == VMOVE).count();
    if contours == 0 {
        return None;
    }
    let mut lengths = vec![0usize; contours];
    let mut points = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut start = 0;
    let mut n: Option<usize> = None;
    for v in vertices {
        match v.kind {
            VMOVE => {
                if let Some(n) = n {
                    lengths[n] = points.len() - start;
                }
                n = Some(n.map_or(0, |n| n + 1));
                start = points.len();
                (x, y) = (f32::from(v.x), f32::from(v.y));
                points.push(Point { x, y });
            }
            VLINE => {
                (x, y) = (f32::from(v.x), f32::from(v.y));
                points.push(Point { x, y });
            }
            VCURVE => {
                let (cx, cy, vx, vy) = (f32::from(v.cx), f32::from(v.cy), f32::from(v.x), f32::from(v.y));
                tesselate_curve(&mut points, x, y, cx, cy, vx, vy, flat, 0);
                (x, y) = (vx, vy);
            }
            VCUBIC => {
                let (cx, cy) = (f32::from(v.cx), f32::from(v.cy));
                let (cx1, cy1) = (f32::from(v.cx1), f32::from(v.cy1));
                let (vx, vy) = (f32::from(v.x), f32::from(v.y));
                tesselate_cubic(&mut points, (x, y), (cx, cy), (cx1, cy1), (vx, vy), flat, 0);
                (x, y) = (vx, vy);
            }
            _ => {}
        }
    }
    lengths[n.unwrap_or(0)] = points.len() - start;
    Some((points, lengths))
}

#[derive(Clone, Copy, Default)]
struct Edge {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    invert: bool,
}

fn edge_less(a: &Edge, b: &Edge) -> bool {
    a.y0 < b.y0
}

fn sort_edges_ins_sort(p: &mut [Edge]) {
    for i in 1..p.len() {
        let t = p[i];
        let mut j = i;
        while j > 0 {
            if !edge_less(&t, &p[j - 1]) {
                break;
            }
            p[j] = p[j - 1];
            j -= 1;
        }
        if i != j {
            p[j] = t;
        }
    }
}

fn sort_edges_quicksort(mut p: &mut [Edge]) -> Result<()> {
    while p.len() > 12 {
        let n = p.len();
        let m = n >> 1;
        let c01 = edge_less(&p[0], &p[m]);
        let c12 = edge_less(&p[m], &p[n - 1]);
        if c01 != c12 {
            let c = edge_less(&p[0], &p[n - 1]);
            let z = if c == c12 { 0 } else { n - 1 };
            p.swap(z, m);
        }
        p.swap(0, m);
        let (mut i, mut j) = (1usize, n - 1);
        loop {
            while edge_less(p.get(i).ok_or(Malformed)?, &p[0]) {
                i += 1;
            }
            while edge_less(&p[0], &p[j]) {
                j = j.checked_sub(1).ok_or(Malformed)?;
            }
            if i >= j {
                break;
            }
            p.swap(i, j);
            i += 1;
            j -= 1;
        }
        let whole = std::mem::take(&mut p);
        if j < n.saturating_sub(i) {
            sort_edges_quicksort(&mut whole[..j])?;
            p = whole.get_mut(i..).ok_or(Malformed)?;
        } else {
            sort_edges_quicksort(whole.get_mut(i..).ok_or(Malformed)?)?;
            p = &mut whole[..j];
        }
    }
    Ok(())
}

// Where a glyph's pixels go in the atlas.
#[derive(Clone, Copy)]
struct Bitmap {
    base: i64,
    w: i32,
    h: i32,
    stride: i32,
}

#[derive(Clone, Copy)]
struct ActiveEdge {
    fx: f32,
    fdx: f32,
    fdy: f32,
    direction: f32,
    sy: f32,
    ey: f32,
}

fn new_active(e: &Edge, off_x: i32, start_point: f32) -> ActiveEdge {
    let dxdy = (e.x1 - e.x0) / (e.y1 - e.y0);
    let mut fx = e.x0 + dxdy * (start_point - e.y0);
    fx -= off_x as f32;
    ActiveEdge {
        fx,
        fdx: dxdy,
        fdy: if dxdy != 0.0 { 1.0 / dxdy } else { 0.0 },
        direction: if e.invert { 1.0 } else { -1.0 },
        sy: e.y0,
        ey: e.y1,
    }
}

// The scanline's two rows in one buffer, as stb lays them out: the coverage at 0, the fill
// that carries to the right at `w` (stb's scanline2, of which scanline_fill is one on).
struct Scanline {
    data: Vec<f32>,
    w: usize,
}

impl Scanline {
    fn add(&mut self, row: usize, x: i32, v: f32) -> Result<()> {
        let at = usize::try_from(x).map_err(|_| Malformed)? + row;
        *self.data.get_mut(at).ok_or(Malformed)? += v;
        Ok(())
    }
}

const COVERAGE: usize = 0;

#[allow(clippy::too_many_arguments)] // stbtt__handle_clipped_edge's
fn handle_clipped_edge(
    scanline: &mut Scanline,
    row: usize,
    x: i32,
    e: &ActiveEdge,
    mut x0: f32,
    mut y0: f32,
    mut x1: f32,
    mut y1: f32,
) -> Result<()> {
    if y0 == y1 {
        return Ok(());
    }
    if y0 > e.ey {
        return Ok(());
    }
    if y1 < e.sy {
        return Ok(());
    }
    if y0 < e.sy {
        x0 += (x1 - x0) * (e.sy - y0) / (y1 - y0);
        y0 = e.sy;
    }
    if y1 > e.ey {
        x1 += (x1 - x0) * (e.ey - y1) / (y1 - y0);
        y1 = e.ey;
    }
    let (xf, xf1) = (x as f32, x.wrapping_add(1) as f32);
    if x0 <= xf && x1 <= xf {
        scanline.add(row, x, e.direction * (y1 - y0))?;
    } else if x0 >= xf1 && x1 >= xf1 {
    } else {
        scanline.add(row, x, e.direction * (y1 - y0) * (1.0 - ((x0 - xf) + (x1 - xf)) / 2.0))?;
    }
    Ok(())
}

fn sized_trapezoid_area(height: f32, top_width: f32, bottom_width: f32) -> f32 {
    (top_width + bottom_width) / 2.0 * height
}

fn position_trapezoid_area(height: f32, tx0: f32, tx1: f32, bx0: f32, bx1: f32) -> f32 {
    sized_trapezoid_area(height, tx1 - tx0, bx1 - bx0)
}

fn sized_triangle_area(height: f32, width: f32) -> f32 {
    height * width / 2.0
}

fn fill_active_edges(scanline: &mut Scanline, active: &[ActiveEdge], y_top: f32) -> Result<()> {
    let len = scanline.w as i32;
    let fill_row = scanline.w + 1;
    let y_bottom = y_top + 1.0;
    // stb's list has the newest first; this one has it last.
    for e in active.iter().rev() {
        if e.fdx == 0.0 {
            let x0 = e.fx;
            if x0 < len as f32 {
                if x0 >= 0.0 {
                    handle_clipped_edge(scanline, COVERAGE, ftoi(x0), e, x0, y_top, x0, y_bottom)?;
                    handle_clipped_edge(scanline, scanline.w, ftoi(x0) + 1, e, x0, y_top, x0, y_bottom)?;
                } else {
                    handle_clipped_edge(scanline, scanline.w, 0, e, x0, y_top, x0, y_bottom)?;
                }
            }
            continue;
        }
        let mut x0 = e.fx;
        let dx = e.fdx;
        let mut xb = x0 + dx;
        let mut dy = e.fdy;
        let (mut x_top, mut sy0) = if e.sy > y_top { (x0 + dx * (e.sy - y_top), e.sy) } else { (x0, y_top) };
        let (mut x_bottom, mut sy1) = if e.ey < y_bottom { (x0 + dx * (e.ey - y_top), e.ey) } else { (xb, y_bottom) };
        if x_top >= 0.0 && x_bottom >= 0.0 && x_top < len as f32 && x_bottom < len as f32 {
            if ftoi(x_top) == ftoi(x_bottom) {
                // one pixel
                let x = ftoi(x_top);
                let height = (sy1 - sy0) * e.direction;
                let right = x as f32 + 1.0;
                scanline.add(COVERAGE, x, position_trapezoid_area(height, x_top, right, x_bottom, right))?;
                scanline.add(fill_row, x, height)?;
            } else {
                if x_top > x_bottom {
                    // flipped vertically: the signed area is the same
                    sy0 = y_bottom - (sy0 - y_top);
                    sy1 = y_bottom - (sy1 - y_top);
                    std::mem::swap(&mut sy0, &mut sy1);
                    std::mem::swap(&mut x_bottom, &mut x_top);
                    // stb negates dx here too, and reads it no more
                    dy = -dy;
                    std::mem::swap(&mut x0, &mut xb);
                }
                let x1 = ftoi(x_top);
                let x2 = ftoi(x_bottom);
                let mut y_crossing = y_top + dy * ((x1 + 1) as f32 - x0);
                let mut y_final = y_top + dy * (x2 as f32 - x0);
                if y_crossing > y_bottom {
                    y_crossing = y_bottom;
                }
                let sign = e.direction;
                let mut area = sign * (y_crossing - sy0);
                scanline.add(COVERAGE, x1, sized_triangle_area(area, (x1 + 1) as f32 - x_top))?;
                if y_final > y_bottom {
                    let denom = x2 - (x1 + 1);
                    y_final = y_bottom;
                    if denom != 0 {
                        dy = (y_final - y_crossing) / denom as f32;
                    }
                }
                let step = sign * dy * 1.0;
                for x in x1 + 1..x2 {
                    scanline.add(COVERAGE, x, area + step / 2.0)?;
                    area += step;
                }
                let right = x2 as f32 + 1.0;
                let last = area + sign * position_trapezoid_area(sy1 - y_final, x2 as f32, right, x_bottom, right);
                scanline.add(COVERAGE, x2, last)?;
                scanline.add(fill_row, x2, sign * (sy1 - sy0))?;
            }
        } else {
            // The edge leaves the box: stb's slow way, pixel by pixel.
            for x in 0..len {
                let y0 = y_top;
                let x1 = x as f32;
                let x2 = (x + 1) as f32;
                let x3 = xb;
                let y3 = y_bottom;
                let y1 = (x as f32 - x0) / dx + y_top;
                let y2 = ((x + 1) as f32 - x0) / dx + y_top;
                let mut clip =
                    |ax: f32, ay: f32, bx: f32, by: f32| handle_clipped_edge(scanline, 0, x, e, ax, ay, bx, by);
                if x0 < x1 && x3 > x2 {
                    clip(x0, y0, x1, y1)?;
                    clip(x1, y1, x2, y2)?;
                    clip(x2, y2, x3, y3)?;
                } else if x3 < x1 && x0 > x2 {
                    clip(x0, y0, x2, y2)?;
                    clip(x2, y2, x1, y1)?;
                    clip(x1, y1, x3, y3)?;
                } else if (x0 < x1 && x3 > x1) || (x3 < x1 && x0 > x1) {
                    clip(x0, y0, x1, y1)?;
                    clip(x1, y1, x3, y3)?;
                } else if (x0 < x2 && x3 > x2) || (x3 < x2 && x0 > x2) {
                    clip(x0, y0, x2, y2)?;
                    clip(x2, y2, x3, y3)?;
                } else {
                    clip(x0, y0, x3, y3)?;
                }
            }
        }
    }
    Ok(())
}

// stbtt__rasterize_sorted_edges, version 2: `edges` ends with the sentinel.
fn rasterize_sorted_edges(
    result: Bitmap,
    atlas: &mut [u8],
    edges: &mut [Edge],
    n: usize,
    off_x: i32,
    off_y: i32,
) -> Result<()> {
    if result.h <= 0 {
        return Ok(());
    }
    // stb clears a negative width's rows with a memset of a negative size.
    let w = usize::try_from(result.w).map_err(|_| Malformed)?;
    let mut scanline = Scanline { data: vec![0.0; w * 2 + 1], w };
    let mut active: Vec<ActiveEdge> = Vec::new();
    let mut y = off_y;
    edges.get_mut(n).ok_or(Malformed)?.y0 = off_y.wrapping_add(result.h) as f32 + 1.0;
    let mut next = 0usize;
    for j in 0..result.h {
        let scan_y_top = y as f32 + 0.0;
        let scan_y_bottom = y as f32 + 1.0;
        scanline.data.fill(0.0);
        active.retain(|z| z.ey > scan_y_top || z.ey.is_nan());
        loop {
            let e = *edges.get(next).ok_or(Malformed)?;
            if e.y0 > scan_y_bottom || e.y0.is_nan() {
                break;
            }
            if e.y0 != e.y1 {
                let mut z = new_active(&e, off_x, scan_y_top);
                if j == 0 && off_y != 0 && z.ey < scan_y_top {
                    z.ey = scan_y_top; // subpixel positioning and rounding, stb says
                }
                active.push(z);
            }
            next += 1;
        }
        if !active.is_empty() {
            fill_active_edges(&mut scanline, &active, scan_y_top)?;
        }
        let mut sum = 0.0f32;
        let row = result.base + i64::from(j) * i64::from(result.stride);
        for i in 0..w {
            sum += scanline.data[w + i];
            let k = (scanline.data[i] + sum).abs() * 255.0 + 0.5;
            let m = ftoi(k).min(255);
            let at = usize::try_from(row + i as i64).map_err(|_| Malformed)?;
            *atlas.get_mut(at).ok_or(Malformed)? = m as u8;
        }
        for z in &mut active {
            z.fx += z.fdx;
        }
        y = y.wrapping_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // stbtt__rasterize's
fn rasterize_points(
    result: Bitmap,
    atlas: &mut [u8],
    points: &[Point],
    lengths: &[usize],
    (scale_x, scale_y): (f32, f32),
    (shift_x, shift_y): (f32, f32),
    (off_x, off_y): (i32, i32),
    invert: bool,
) -> Result<()> {
    let y_scale_inv = if invert { -scale_y } else { scale_y };
    let vsubsample = 1.0f32;
    let total: usize = lengths.iter().sum();
    let mut edges = Vec::with_capacity(total + 1);
    let mut m = 0;
    for &count in lengths {
        let p = points.get(m..m + count).ok_or(Malformed)?;
        m += count;
        for k in 0..count {
            let j = if k == 0 { count - 1 } else { k - 1 };
            if p[j].y == p[k].y {
                continue;
            }
            let flip = if invert { p[j].y > p[k].y } else { p[j].y < p[k].y };
            let (a, b) = if flip { (j, k) } else { (k, j) };
            edges.push(Edge {
                x0: p[a].x * scale_x + shift_x,
                y0: (p[a].y * y_scale_inv + shift_y) * vsubsample,
                x1: p[b].x * scale_x + shift_x,
                y1: (p[b].y * y_scale_inv + shift_y) * vsubsample,
                invert: flip,
            });
        }
    }
    let n = edges.len();
    sort_edges_quicksort(&mut edges)?;
    sort_edges_ins_sort(&mut edges);
    edges.push(Edge::default());
    rasterize_sorted_edges(result, atlas, &mut edges, n, off_x, off_y)
}

impl Info<'_> {
    // stbtt_MakeGlyphBitmapSubpixel, which rasterises with stbtt_Rasterize.
    #[allow(clippy::too_many_arguments)]
    fn make_glyph_bitmap(
        &self,
        atlas: &mut [u8],
        result: Bitmap,
        scale_x: f32,
        scale_y: f32,
        shift_x: f32,
        shift_y: f32,
        glyph: i32,
    ) -> Result<()> {
        let vertices = self.glyph_shape(glyph, 0)?;
        let [ix0, iy0, _, _] = self.bitmap_box(glyph, scale_x, scale_y, shift_x, shift_y)?;
        if result.w == 0 || result.h == 0 {
            return Ok(());
        }
        let scale = if scale_x > scale_y { scale_y } else { scale_x };
        let Some((points, lengths)) = flatten_curves(&vertices, 0.35 / scale) else {
            return Ok(());
        };
        rasterize_points(result, atlas, &points, &lengths, (scale_x, scale_y), (shift_x, shift_y), (ix0, iy0), true)
    }
}

// ---------------------------------------------------------------------------------------
// Packing (stb_truetype's own stbrp_ fallback and stbtt_PackFontRanges)

struct Packer {
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    bottom_y: i32,
}

#[derive(Clone, Copy, Default)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    was_packed: bool,
}

// stbrp_pack_rects: left to right in rows, and nothing more once one does not fit.
fn pack_rects(packer: &mut Packer, rects: &mut [Rect]) {
    let mut fitted = 0;
    for rect in rects.iter_mut() {
        if packer.x.wrapping_add(rect.w) > packer.width {
            packer.x = 0;
            packer.y = packer.bottom_y;
        }
        if packer.y.wrapping_add(rect.h) > packer.height {
            break;
        }
        rect.x = packer.x;
        rect.y = packer.y;
        rect.was_packed = true;
        packer.x = packer.x.wrapping_add(rect.w);
        if packer.y.wrapping_add(rect.h) > packer.bottom_y {
            packer.bottom_y = packer.y.wrapping_add(rect.h);
        }
        fitted += 1;
    }
    for rect in &mut rects[fitted..] {
        rect.was_packed = false;
    }
}

fn oversample_shift(oversample: i32) -> f32 {
    if oversample == 0 {
        return 0.0;
    }
    -(oversample - 1) as f32 / (2.0 * oversample as f32)
}

// stbtt_PackFontRange with one range of CHARS from FIRST_CHAR.
fn pack_font_range(
    font: Font,
    size: f32,
    atlas: &mut [u8],
    stride: i32,
    packer: &mut Packer,
    chardata: &mut [Glyph; CHARS],
) -> Result<bool> {
    for glyph in chardata.iter_mut() {
        (glyph.x0, glyph.y0, glyph.x1, glyph.y1) = (0, 0, 0, 0);
    }
    let mut info = Info::new(font, offset_for_first_font(font)?);
    if !info.init()? {
        return Err(Malformed); // stb would go on with fields InitFont never set
    }
    let scale =
        if size > 0.0 { info.scale_for_pixel_height(size)? } else { info.scale_for_mapping_em_to_pixels(-size)? };
    let (scale_x, scale_y) = (scale * OVERSAMPLE as f32, scale * OVERSAMPLE as f32);

    // stbtt_PackFontRangesGatherRects
    let mut rects = [Rect::default(); CHARS];
    let mut missing_glyph_added = false;
    for (j, rect) in rects.iter_mut().enumerate() {
        let glyph = info.find_glyph_index(FIRST_CHAR + j as i32)?;
        if glyph == 0 && missing_glyph_added {
            (rect.w, rect.h) = (0, 0);
        } else {
            let [x0, y0, x1, y1] = info.bitmap_box(glyph, scale_x, scale_y, 0.0, 0.0)?;
            let extra = (PADDING as u32).wrapping_add(OVERSAMPLE).wrapping_sub(1);
            rect.w = (x1.wrapping_sub(x0) as u32).wrapping_add(extra) as i32;
            rect.h = (y1.wrapping_sub(y0) as u32).wrapping_add(extra) as i32;
            if glyph == 0 {
                missing_glyph_added = true;
            }
        }
    }

    pack_rects(packer, &mut rects);

    // stbtt_PackFontRangesRenderIntoRects
    let mut return_value = true;
    let mut missing_glyph: Option<usize> = None;
    let recip_h = 1.0 / OVERSAMPLE as f32;
    let recip_v = 1.0 / OVERSAMPLE as f32;
    let sub_x = oversample_shift(OVERSAMPLE as i32);
    let sub_y = oversample_shift(OVERSAMPLE as i32);
    for (j, rect) in rects.iter_mut().enumerate() {
        if rect.was_packed && rect.w != 0 && rect.h != 0 {
            let glyph = info.find_glyph_index(FIRST_CHAR + j as i32)?;
            rect.x = rect.x.wrapping_add(PADDING);
            rect.y = rect.y.wrapping_add(PADDING);
            rect.w = rect.w.wrapping_sub(PADDING);
            rect.h = rect.h.wrapping_sub(PADDING);
            let advance = info.advance(glyph)?;
            let [x0, y0, _, _] = info.bitmap_box(glyph, scale_x, scale_y, 0.0, 0.0)?;
            let bitmap = Bitmap {
                base: i64::from(rect.x) + i64::from(rect.y) * i64::from(stride),
                w: rect.w.wrapping_sub(OVERSAMPLE as i32).wrapping_add(1),
                h: rect.h.wrapping_sub(OVERSAMPLE as i32).wrapping_add(1),
                stride,
            };
            info.make_glyph_bitmap(atlas, bitmap, scale_x, scale_y, 0.0, 0.0, glyph)?;
            let bc = &mut chardata[j];
            bc.x0 = rect.x as u16;
            bc.y0 = rect.y as u16;
            bc.x1 = rect.x.wrapping_add(rect.w) as u16;
            bc.y1 = rect.y.wrapping_add(rect.h) as u16;
            bc.xadvance = scale * f32::from(advance);
            bc.xoff = x0 as f32 * recip_h + sub_x;
            bc.yoff = y0 as f32 * recip_v + sub_y;
            bc.xoff2 = x0.wrapping_add(rect.w) as f32 * recip_h + sub_x;
            bc.yoff2 = y0.wrapping_add(rect.h) as f32 * recip_v + sub_y;
            if glyph == 0 {
                missing_glyph = Some(j);
            }
        } else if let (true, 0, 0, Some(missing)) = (rect.was_packed, rect.w, rect.h, missing_glyph) {
            chardata[j] = chardata[missing];
        } else {
            return_value = false; // if any fail, stb reports failure
        }
    }
    Ok(return_value)
}
