// Pictures: the PC keeps a map's pictures in its .iwd (and lightmaps and reflection probes
// in the fastfile); the 360 map carries them in its own file, tiled for the console's
// GPU, each with the header of a stock picture of its format.

use crate::error::{Error, Result};
use crate::images::{
    Gpu, Mip, align_up, decode_probe, encode_dxt3a, fit_picture, format_of, layout, normal_maps_to_dxn, picture_size,
    plan, read_iwi, swap, tile,
};
use crate::num::{load, store};
use crate::ordered::AssetKey;
use crate::paths::file_name;
use crate::porter::{MissingImage, Porter, port_error};
use crate::query::{asset_name, embedded, lower, name as name_of};
use crate::tree::{FOLLOWING, NodeId, Ty};
use std::collections::BTreeSet;

/// A 16x16 DXT1 picture of one block repeated.
fn plain(colour: u16, indexes: u32) -> Vec<Mip> {
    let mut block = vec![0u8; 8];
    store(&mut block[0..], 2, u64::from(colour), false);
    store(&mut block[2..], 2, u64::from(colour), false);
    store(&mut block[4..], 4, u64::from(indexes), false);
    let mut mip = Mip::new();
    for _ in 0..16 {
        mip.extend_from_slice(&block);
    }
    vec![mip]
}

impl Porter<'_> {
    pub(crate) fn in_iwd(&self, name: &str) -> bool {
        self.map_pictures.contains_key(&lower(name))
    }

    /// A picture a stock map streams that the map doesn't bring in its own .iwd (the PC
    /// game's pictures are the 360's; a map's own may not be).
    pub(crate) fn streams_from_stock(&self, name: &str) -> bool {
        let low = lower(name);
        self.stock_streamed.has(&low) && !self.map_pictures.contains_key(&low)
    }

    // ---- making 360 pictures ----------------------------------------------------------

    /// Image `d` (in place) as a 360 picture with these linear mips (for a cube map, each
    /// face's chain): the header of `tpl`, or of a stock picture of the format, with the
    /// size and levels changed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_image(
        &mut self,
        d: NodeId,
        f: Gpu,
        width: i32,
        height: i32,
        faces: &[Vec<Mip>],
        cube: bool,
        tpl: Option<NodeId>,
    ) -> Result<()> {
        let gpu = f as i32;
        let stock = |p: &Porter, g: i32, c: bool| p.templates.get(&(g, c)).copied();
        let mut tpl = tpl.or_else(|| stock(self, gpu, cube));
        let mut make_cube = false;
        if tpl.is_none()
            && f == Gpu::Dxn
            && let Some(dxt5) = stock(self, Gpu::Dxt5 as i32, cube)
        {
            // Stock DXN pictures (normal maps) all stream from the disc, so no file
            // carries one to copy: a DXT5 one with the format changed.
            let t = self.st.copy(dxt5)?;
            let texture = self.st.bytes_mut(t, "texture")?;
            let word = texture.get_mut(32..36).ok_or_else(|| Error::new("a picture header cut short"))?;
            let v = (load(word, 4, false) as u32 & !0x3F) | 0x31;
            store(word, 4, u64::from(v), false);
            let format = self.st.int(t, "textureFormat")?;
            self.st.set(t, "textureFormat", (format & !0x3F) | 0x31)?;
            self.templates.insert((Gpu::Dxn as i32, cube), t);
            tpl = Some(t);
        }
        if tpl.is_none() && cube {
            // No stock cube map of this format in a file: a flat one made a cube.
            tpl = stock(self, gpu, false);
            make_cube = true;
        }
        let Some(tpl) = tpl else {
            return Err(port_error(format!(
                "no stock 360 {}{} picture to copy settings from",
                format_of(f).name,
                if cube { " cube" } else { "" }
            )));
        };
        let first = faces.first().ok_or_else(|| Error::new("a picture without levels"))?;
        let mut pixels: Vec<u8> = Vec::new();
        let levels: i32;
        if cube {
            // Six faces, each tiled on its own: every face's top level, then each further
            // mip slice for all six (as stock reflection probes are).
            levels = faces.iter().map(|c| c.len() as i32).fold(first.len() as i32, i32::min);
            if levels == 1 {
                for c in faces {
                    pixels.extend_from_slice(&tile(&c[..1], width, height, f, true));
                }
            } else {
                let (places, total) = plan(width, height, f);
                let starts: BTreeSet<usize> = places.iter().take(levels.max(0) as usize).map(|p| p.base).collect();
                let mut cuts: Vec<usize> = starts.into_iter().collect();
                cuts.push(total);
                let tiled: Vec<Vec<u8>> =
                    faces.iter().map(|c| tile(&c[..levels.max(0) as usize], width, height, f, false)).collect();
                for i in 0..cuts.len() - 1 {
                    for t in &tiled {
                        let a = cuts[i].min(t.len());
                        let b = cuts[i + 1].min(t.len());
                        pixels.extend_from_slice(&t[a..a.max(b)]);
                    }
                }
            }
        } else {
            levels = first.len() as i32;
            pixels = tile(first, width, height, f, levels == 1);
        }
        if f == Gpu::L8 {
            pixels = swap(&pixels, f); // 8-bit data isn't byte-swapped on the 360
        }
        let mut header = self.st.bytes(tpl, "texture")?.to_vec();
        if header.len() < 52 {
            return Err(Error::new("a picture header cut short"));
        }
        let mut dw = [0u32; 6];
        for (k, w) in dw.iter_mut().enumerate() {
            *w = load(&header[28 + 4 * k..], 4, false) as u32;
        }
        let pitch = (align_up(width as usize, if format_of(f).block_width == 4 { 128 } else { 32 }) / 32) as u32;
        dw[0] = (dw[0] & !(0x1FF << 22)) | (pitch << 22);
        dw[2] = (dw[2] & !0x3FF_FFFF) | (width - 1) as u32 | (((height - 1) as u32) << 13);
        dw[4] = (dw[4] & !(0xF << 6)) | (((levels - 1) as u32) << 6);
        let mip_address =
            if levels > 1 { ((layout(width, height, 0, f).slice * if cube { 6 } else { 1 }) >> 12) as u32 } else { 0 };
        dw[5] = (dw[5] & 0xFFF & !(1 << 11)) | (mip_address << 12) | if levels > 1 { 1 << 11 } else { 0 };
        if make_cube {
            dw[2] = (dw[2] & 0x3FF_FFFF) | (5 << 26); // six faces
            dw[5] = (dw[5] & !(3 << 9)) | (3 << 9); // dimension: cube
        }
        for (k, &w) in dw.iter().enumerate() {
            store(&mut header[28 + 4 * k..], 4, u64::from(w), false);
        }

        let made = self.st.copy(tpl)?;
        {
            let texture = self.st.bytes_mut(made, "texture")?;
            let n = header.len().min(texture.len());
            texture[..n].copy_from_slice(&header[..n]);
        }
        let st = &mut *self.st;
        st.set(made, "cardMemory", pixels.len() as i64)?;
        st.set(made, "width", i64::from(width))?;
        st.set(made, "height", i64::from(height))?;
        st.set(made, "depth", 1)?;
        st.set(made, "levelCount", i64::from(levels))?;
        st.set(made, "streaming", 0)?;
        st.set_ptr(made, "pixels", FOLLOWING)?;
        st.set_ptr(made, "name", FOLLOWING)?;
        st.bytes_mut(made, "streams")?.fill(0);
        for k in ["mapType", "semantic", "category"] {
            if st.has(d, k) {
                let v = st.int(d, k)?;
                st.set(made, k, v)?;
            }
        }
        if cube {
            st.set(made, "mapType", 5)?;
        }
        let byte = Ty::named(self.x, "unsigned char").ok_or_else(|| Error::new("the rules have no unsigned char"))?;
        let name = st.child(d, "name");
        let count = pixels.len() as i64;
        let leaf = st.make_leaf(Some(byte), count, pixels, true);
        let ch = st.make_children();
        st.children_mut(ch).set("name", name, &[]);
        st.children_mut(ch).set("pixels", Some(leaf), &[]);
        st.st_mut(made).set_ch(Some(ch));
        let info = *self.x.infos.get("GfxImage").ok_or_else(|| Error::new("the rules have no GfxImage"))?;
        st.st_mut(made).asset = &self.x.structs[info].name;
        let (slot, forward) = {
            let s = st.st(d);
            (s.slot, s.forward)
        };
        st.assign(d, made)?;
        if slot.is_some() {
            st.st_mut(d).slot = slot;
        }
        if forward {
            st.st_mut(d).forward = true;
        }
        Ok(())
    }

    /// A small plain magenta picture (what's missing is easy to see).
    pub(crate) fn magenta(&mut self, d: NodeId, name: Option<String>) -> Result<NodeId> {
        if let Some(name) = name {
            let s = self.st.make_str(name);
            self.st.set_child(d, "name", Some(s));
        }
        self.build_image(d, Gpu::Dxt1, 16, 16, &[plain(0xF81F, 0)], false, None)?;
        self.st.set(d, "category", 3)?;
        self.done.insert(d);
        Ok(d)
    }

    /// A small picture alpha tested away entirely.
    pub(crate) fn see_through(&mut self, d: NodeId, name: &str) -> Result<NodeId> {
        let s = self.st.make_str(name);
        self.st.set_child(d, "name", Some(s));
        self.build_image(d, Gpu::Dxt1, 16, 16, &[plain(0, 0xFFFF_FFFF)], false, None)?;
        self.st.set(d, "category", 3)?;
        self.done.insert(d);
        Ok(d)
    }

    /// The built-in picture standing in for one the map doesn't bring: flat for a normal
    /// map, black (no shine) for a specular one; for the rest nothing (magenta stands in).
    pub(crate) fn stand_in(&mut self, d: NodeId, name: &str) -> Result<Option<String>> {
        let semantic = self.st.int(d, "semantic")?;
        if semantic == 5 || name.ends_with("_nml") || name.ends_with("_n") {
            return Ok(Some("$identitynormalmap".to_string()));
        }
        if semantic == 8 || name.ends_with("_spc") || name.ends_with("_s") {
            return Ok(Some("$black".to_string()));
        }
        Ok(None)
    }

    /// A copy of a stock water picture under `name` (the game draws the waves into it;
    /// the PC file has no pixels for it), or None.
    pub(crate) fn water_image(&mut self, d: NodeId, name: &str) -> Result<Option<NodeId>> {
        let (width, height) = (self.st.int(d, "width")?, self.st.int(d, "height")?);
        let mut best = None;
        for (k, &v) in self.library.iter() {
            if k.ty != "GfxImage" || self.st.int(v, "category")? != 5 || self.st.ptr(v, "pixels")? != FOLLOWING {
                continue;
            }
            if best.is_none() || (self.st.int(v, "width")? == width && self.st.int(v, "height")? == height) {
                best = Some(v);
            }
        }
        let Some(best) = best else { return Ok(None) };
        let made = self.copy_in(best)?;
        let st = &mut *self.st;
        let ch = st.make_children();
        if let Some(theirs) = st.ch(made).cloned() {
            *st.children_mut(ch) = theirs;
        }
        st.st_mut(made).set_ch(Some(ch));
        let s = st.make_str(name);
        st.children_mut(ch).set("name", Some(s), &[]);
        let pixels = st.children(ch).get("pixels", &[]);
        if let Some(lf) = st.as_leaf(pixels).cloned() {
            let copy = st.make_leaf(lf.t, lf.n, lf.raw, lf.big);
            st.children_mut(ch).set("pixels", Some(copy), &[]);
        }
        Ok(Some(made))
    }

    /// Stock 360 probes carry the same pattern in alpha in every probe of a map: ours get
    /// a stock one's (the same size, so the same place in the tiling).
    pub(crate) fn probe_alpha(&mut self, d: NodeId) -> Result<()> {
        let px = self.st.child(d, "pixels").filter(|&n| self.st.is_leaf(Some(n)));
        let Some(px) = px else { return Ok(()) };
        let width = self.st.int(d, "width")?;
        let mut found = None;
        for (k, &v) in self.library.iter() {
            if k.ty != "GfxImage" || !k.name.starts_with("*reflection_probe") {
                continue;
            }
            let Some(theirs) = self.st.as_leaf(self.st.child(v, "pixels")) else { continue };
            if theirs.n != self.st.leaf(px).n || self.st.int(v, "width")? != width {
                continue;
            }
            found = Some(theirs.raw.clone());
            break;
        }
        if let Some(theirs) = found {
            let raw = &mut self.st.leaf_mut(px).raw;
            for i in (0..raw.len().min(theirs.len())).step_by(4) {
                raw[i] = theirs[i];
            }
        }
        Ok(())
    }

    // ---- the map's pictures -------------------------------------------------------------

    pub(crate) fn pre_gfx_image(&mut self, d: NodeId) -> Result<bool> {
        // (name: one shared with another asset)
        let own = asset_name(self.st, d);
        let shared = name_of(self.st, d);
        let mut name = own.or(shared).unwrap_or_default();
        if self.st.int(d, "category")? == 5 && !name.starts_with(',') {
            // Water: a stand-in of another size would have the game write the waves past
            // its end.
            let Some(made) = self.water_image(d, &name)? else {
                return Err(port_error(format!(
                    "water picture {}: no stock 360 water picture in the stock files given to copy (mp_favela has some)",
                    name
                )));
            };
            self.replace(d, made)?;
            return Ok(true);
        }
        if name.starts_with(',') {
            // The PC file only names it: so does the 360 file when the 360 has it loaded;
            // else it becomes a picture of the map's own.
            name.remove(0);
            if self.resident.has(&AssetKey::new("GfxImage", name.clone()))
                || self.named.contains(&("GfxImage".to_string(), name.clone()))
            {
                let r = self.reference("GfxImage", &name)?;
                self.replace(d, r)?;
                return Ok(true);
            }
            let s = self.st.make_str(name.clone());
            self.st.set_child(d, "name", Some(s));
        }
        // Built-in pictures ($identitynormalmap, ...) stay the game's own even when an
        // .iwd carries a copy: a second one would replace the resident one.
        if self.resident.has(&AssetKey::new("GfxImage", name.clone())) && (name.starts_with('$') || !self.in_iwd(&name))
        {
            let r = self.reference("GfxImage", &name)?;
            self.replace(d, r)?;
            return Ok(true);
        }
        if self.streams_from_stock(&name) {
            self.streamed_stock += 1;
            // (streams_from_stock found it)
            let src = *self.stock_streamed.find(&lower(&name)).expect("a streamed stock picture");
            let copy = self.st.deep_copy_struct(src, None)?;
            let made = self.stock_copy(copy)?;
            self.replace(d, made)?;
            return Ok(true);
        }
        let texture = embedded(self.st, d, "texture");
        let load_def = texture.and_then(|t| self.st.child(t, "loadDef")).filter(|&n| self.st.is_struct(Some(n)));
        let from_load_def = match load_def {
            Some(l) => self.st.int(l, "resourceSize")? != 0,
            None => false,
        };
        if let (true, Some(load_def)) = (from_load_def, load_def) {
            self.image_from_load_def(d, load_def)?;
        } else if !self.picture_from_files(d, &name)? {
            // Not in the map's .iwd, or no copy there can be read: a built-in picture
            // stands in, so the map still loads.
            let stand = self.stand_in(d, &name)?;
            self.missing_images.push(MissingImage { name: name.clone(), stand_in: stand.clone() });
            match stand {
                None => {
                    self.magenta(d, None)?;
                }
                Some(stand) => {
                    let r = self.reference("GfxImage", &stand)?;
                    self.replace(d, r)?;
                    return Ok(true);
                }
            }
        }
        self.done.insert(d);
        Ok(true)
    }

    /// Fills image `d` from the map's .iwd, else the PC game's; false when neither has a
    /// copy that can be read (an unreadable one is noted, not fatal).
    pub(crate) fn picture_from_files(&mut self, d: NodeId, name: &str) -> Result<bool> {
        let low = lower(name);
        for from_game in [false, true] {
            let index = if from_game { &self.game_pictures } else { &self.map_pictures };
            let Some(found) = index.get(&low).cloned() else { continue };
            let read = found.zip.read(found.index).and_then(|data| self.image_from_iwd(d, name, &data));
            match read {
                Ok(()) => {}
                Err(e) if e.is_error() => {
                    self.unreadable.push(format!("{} ({}: {})", name, file_name(found.zip.path()), e.text()));
                    continue;
                }
                Err(e) => return Err(e),
            }
            if from_game {
                self.from_game += 1;
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn image_from_iwd(&mut self, d: NodeId, name: &str, data: &[u8]) -> Result<()> {
        let pic = read_iwi(data)?;
        let loadscreen = name.starts_with("loadscreen");
        let limit = if loadscreen { 1024 } else { 2048 };
        let (mut w, mut h) = (pic.width, pic.height);
        let mut mips = pic.mips;
        if !pic.cube {
            if picture_size(w, h, limit) != (w, h) {
                let fit = fit_picture(pic.format, w, h, &mips, limit)?;
                w = fit.width;
                h = fit.height;
                mips = fit.mips;
            }
            let k = self.mip_drop.get(name).copied().unwrap_or(0);
            if k > 0 && mips.len() as i32 > k {
                mips.drain(..k as usize);
                w = (w >> k).max(1);
                h = (h >> k).max(1);
            }
        }
        // Stock loading screens have no mipmaps; with them the console stopped loading a
        // big map ("MT_GetSize: max allocation exceeded").
        if loadscreen {
            mips.resize(1, Mip::new());
        }
        let mut f = pic.format;
        // (semantic 5, or named _nml: mp_afghan's wavy_nml, semantic 3, is DXN in stock too)
        if f == Gpu::Dxt5 && !pic.cube && (self.st.int(d, "semantic")? == 5 || name.ends_with("_nml")) {
            mips = normal_maps_to_dxn(&mips);
            f = Gpu::Dxn;
            self.dxn_count += 1;
        }
        let faces: Vec<Vec<Mip>> = if pic.cube { mips.into_iter().map(|m| vec![m]).collect() } else { vec![mips] };
        self.build_image(d, f, w, h, &faces, pic.cube, None)?;
        // Pictures from files are "load from file" on the 360; stock loading screens are
        // plain 2D pictures.
        self.st.set(d, "category", 3)?;
        if loadscreen {
            self.st.set(d, "semantic", 0)?;
        }
        Ok(())
    }

    /// Pictures the PC keeps in the fastfile: lightmaps, reflection probes, $outdoor.
    pub(crate) fn image_from_load_def(&mut self, d: NodeId, load_def: NodeId) -> Result<()> {
        let name = asset_name(self.st, d).unwrap_or_default();
        let format = self.st.int(load_def, "format")?;
        let data: Vec<u8> =
            self.st.as_leaf(self.st.late(load_def, "data")).map(|lf| lf.raw.clone()).unwrap_or_default();
        let w = self.st.int(d, "width")? as i32;
        let h = self.st.int(d, "height")? as i32;
        let cube = (self.st.int(load_def, "flags")? & 4) != 0 || self.st.int(d, "mapType")? == 5;
        let (bpp, out): (usize, Gpu) = match format {
            // A8R8G8B8: a DDS's byte order, which tiling takes
            21 => (4, Gpu::Argb8),
            // L8. The 360 keeps the primary lightmap as DXT3A (4 bits a pixel, as stock
            // maps do); other single-channel pictures as plain 8-bit.
            50 => (1, if name.starts_with("*lightmap") { Gpu::Dxt3a } else { Gpu::L8 }),
            _ => {
                return Err(port_error(format!("picture {}: PC format {format} isn't supported yet", name)));
            }
        };
        if !(1..=8192).contains(&w) || !(1..=8192).contains(&h) {
            return Err(port_error(format!("picture {} is {w}x{h}", name)));
        }
        let count = if cube { 6 } else { 1 };
        let face = data.len() / count;
        let top = w as usize * h as usize * bpp;
        let slice = |from: usize, n: usize| -> Mip {
            let from = from.min(data.len());
            data[from..data.len().min(from + n)].to_vec()
        };
        let mut faces: Vec<Vec<Mip>> = (0..count).map(|i| vec![slice(i * face, top)]).collect();
        if cube && out == Gpu::Argb8 {
            // Reflection probes keep every face's mips: the smaller ones are blurrier
            // copies for less glossy surfaces (with the top one alone every shiny surface
            // reflects the sharp picture).
            for (i, chain_out) in faces.iter_mut().enumerate() {
                let mut chain = Vec::new();
                let mut at = i * face;
                let (mut mw, mut mh) = (w, h);
                while at + mw as usize * mh as usize * bpp <= (i + 1) * face {
                    chain.push(slice(at, mw as usize * mh as usize * bpp));
                    at += mw as usize * mh as usize * bpp;
                    if mw == 1 && mh == 1 {
                        break;
                    }
                    mw = (mw / 2).max(1);
                    mh = (mh / 2).max(1);
                }
                if name.starts_with("*reflection_probe") {
                    for m in &mut chain {
                        *m = decode_probe(m);
                    }
                }
                *chain_out = chain;
            }
        }
        if out == Gpu::Dxt3a {
            for c in &mut faces {
                for m in c.iter_mut() {
                    *m = encode_dxt3a(m, w, h);
                }
            }
        }
        let mut tpl = None;
        if name.starts_with("*lightmap") {
            let key = name[name.rfind('_').map_or(0, |at| at + 1)..].to_string();
            tpl = self.lightmap_templates.find(&key).copied();
        }
        if !cube {
            faces.truncate(1);
        }
        self.build_image(d, out, w, h, &faces, cube, tpl)?;
        if name.starts_with("*reflection_probe") {
            self.probe_alpha(d)?;
        }
        Ok(())
    }
}
