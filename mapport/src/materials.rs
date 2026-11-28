// Materials and shader sets. PC shaders can't run on the 360, so each shader set is
// swapped for the 360's own of the same name (or the nearest one the stock files have),
// and a material's render state comes from a stock material using that set; its draw
// order and face culling stay the PC's.

use crate::error::{Error, Result};
use crate::num::store;
use crate::ordered::AssetKey;
use crate::porter::{Porter, SortKeySwap, port_error};
use crate::query;
use crate::tree::{FOLLOWING, NodeId};
use std::collections::{BTreeSet, HashSet};

const TECHSET: &str = "MaterialTechniqueSet";


/// The text split at each `sep`, empty parts kept.
fn split(s: &str, sep: char) -> Vec<String> {
    s.split(sep).map(str::to_string).collect()
}

/// The first `n` characters of a name.
fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A lit world or model shader set's name in parts: wc_l_sm_t0c0s0_nocast is kind
/// "wc_l_sm_", inputs t0 c0 s0, extra "_nocast".
struct TechsetCodes {
    kind: String,
    inputs: Vec<String>,
    extra: String,
}

/// The parts of a name of the form `(wc|mc)_l_[sm_][ua_]` then one or more of a letter
/// and a digit, then nothing or `_` and anything but line breaks.
fn codes_of(name: &str) -> Option<TechsetCodes> {
    let b = name.as_bytes();
    if !(b.starts_with(b"wc_l_") || b.starts_with(b"mc_l_")) {
        return None;
    }
    let mut at = 5;
    // (Neither "sm_" nor "ua_" can start the inputs: their second letter is no digit.)
    if b[at..].starts_with(b"sm_") {
        at += 3;
    }
    if b[at..].starts_with(b"ua_") {
        at += 3;
    }
    let kind = name[..at].to_string();
    let start = at;
    while at + 1 < b.len() && b[at].is_ascii_lowercase() && b[at + 1].is_ascii_digit() {
        at += 2;
    }
    if at == start {
        return None;
    }
    let rest = &name[at..];
    if !rest.is_empty() && (!rest.starts_with('_') || rest.contains(['\n', '\r'])) {
        return None;
    }
    let inputs = (start..at).step_by(2).map(|i| name[i..i + 2].to_string()).collect();
    Some(TechsetCodes { kind, inputs, extra: rest.to_string() })
}

/// How many distinct values the two lists share.
fn common(a: &[String], b: &[String]) -> usize {
    let sa: BTreeSet<&String> = a.iter().collect();
    b.iter().collect::<BTreeSet<&String>>().into_iter().filter(|x| sa.contains(x)).count()
}

/// The value seen most often, the first seen of those tied.
fn most_common(values: &[u32]) -> Option<u32> {
    let mut counts: Vec<(u32, i32)> = Vec::new();
    for &v in values {
        match counts.iter_mut().find(|c| c.0 == v) {
            Some(c) => c.1 += 1,
            None => counts.push((v, 1)),
        }
    }
    let mut best = counts.first()?;
    for c in &counts {
        if c.1 > best.1 {
            best = c;
        }
    }
    Some(best.0)
}

impl Porter<'_> {
    // ---- shader sets ----------------------------------------------------------------------

    // Whether a stock file given has the shader set: one a stock material uses, which is
    // loaded, copied in or named.
    fn stock_has_techset(&self, name: &str) -> bool {
        let key = AssetKey::new(TECHSET, name);
        self.resident.has(&key) || self.library.has(&key) || self.named.contains(&(key.ty, key.name))
    }

    fn have(&self, name: &str) -> bool {
        self.material_templates.has(&name.to_string()) && self.stock_has_techset(name)
    }

    pub(crate) fn have_techset(&mut self, name: &str) -> Result<bool> {
        Ok(self.have(name))
    }

    /// Radiant's tool shader sets (wc_tools, mc_tools), which draw nothing in game, and the
    /// HDR portal sets (hidden like them).
    fn is_tools(name: &str) -> bool {
        name.split('_').nth(1) == Some("tools") || name.contains("_distfalloff")
    }

    pub(crate) fn is_tools_techset(&mut self, name: &str) -> Result<bool> {
        Ok(Self::is_tools(name))
    }

    /// A stock alpha tested shader set of the same kind (wc_/mc_) with a colour picture
    /// and as few other inputs as there are: given a see-through picture, it draws nothing
    /// (no shadow either, which reads the same picture).
    pub(crate) fn invisible_techset(&self, name: &str) -> Option<String> {
        // Fewest inputs, then a plain lit one, then the last name.
        let prefix = head(name, 3);
        let mut best: Option<(i32, bool, String)> = None;
        for (cand, _) in self.material_templates.iter() {
            let Some(got) = codes_of(cand) else { continue };
            if !got.kind.starts_with(&prefix)
                || !got.extra.is_empty()
                || !got.inputs[0].starts_with('t')
                || !got.inputs.iter().any(|i| i == "c0")
                || !self.have(cand)
            {
                continue;
            }
            let score = (-(got.inputs.len() as i32), got.kind == format!("{prefix}l_sm_"), cand.clone());
            if best.as_ref().is_none_or(|b| score > *b) {
                best = Some(score);
            }
        }
        best.map(|b| b.2)
    }

    fn hiding(&self, name: &str, near: &str) -> bool {
        Self::is_tools(name) && self.invisible_techset(name).as_deref() == Some(near)
    }

    pub(crate) fn hides(&mut self, name: &str, near: &str) -> Result<bool> {
        Ok(self.hiding(name, near))
    }

    /// The same kind, blend and extras as `name`, with the fewest inputs added (dropping
    /// one is harmless: a picture the shader doesn't read).
    pub(crate) fn similar_techset(&self, name: &str) -> Option<String> {
        let want = codes_of(name)?;
        // Fewest inputs added, then most kept, then the last name.
        let mut best: Option<(i32, usize, String)> = None;
        for (cand, _) in self.material_templates.iter() {
            let Some(got) = codes_of(cand) else { continue };
            if got.kind != want.kind || got.extra != want.extra || got.inputs[0] != want.inputs[0] || !self.have(cand) {
                continue;
            }
            let kept = common(&got.inputs, &want.inputs);
            let extra = got.inputs.iter().collect::<BTreeSet<_>>().len() - kept;
            let score = (-(extra as i32), kept, cand.clone());
            if best.as_ref().is_none_or(|b| score > *b) {
                best = Some(score);
            }
        }
        best.map(|b| b.2)
    }

    /// A stock shader set to use in place of `name`: lit world and model sets keep their
    /// kind and blend and drop or add inputs (the material's missing pictures are filled
    /// in); tool sets one that draws nothing; others the one sharing the most leading name
    /// parts, and a wc_/mc_ one sharing only that the plain lit set.
    pub(crate) fn nearest_techset(&mut self, name: &str) -> Result<Option<String>> {
        if let Some(swapped) = self.techset_swaps.find(&name.to_string()) {
            return Ok(Some(swapped.clone()));
        }
        let mut best = if Self::is_tools(name) { self.invisible_techset(name) } else { None };
        if best.is_none() {
            best = self.similar_techset(name);
        }
        if let Some(b) = best {
            self.techset_swaps.set(name.to_string(), b.clone());
            return Ok(Some(b));
        }
        let want = split(name, '_');
        let want_set: BTreeSet<&String> = want.iter().collect();
        let mut score: (usize, i64) = (0, 0);
        for (cand, _) in self.material_templates.iter() {
            if !self.stock_has_techset(cand) {
                continue;
            }
            let parts = split(cand, '_');
            let mut lead = 0;
            while lead < want.len().min(parts.len()) && want[lead] == parts[lead] {
                lead += 1;
            }
            let part_set: BTreeSet<&String> = parts.iter().collect();
            let missing = part_set.iter().filter(|p| !want_set.contains(*p)).count() as i64;
            let sc = (lead, common(&want, &parts) as i64 - missing);
            if lead != 0 && sc > score {
                best = Some(cand.clone());
                score = sc;
            }
        }
        if (name.starts_with("wc_") || name.starts_with("mc_")) && score.0 < 2 {
            // Only "wc"/"mc" in common: plain lit.
            let lit = format!("{}l_sm_r0c0", head(name, 3));
            if self.have(&lit) {
                best = Some(lit);
            }
        }
        if let Some(b) = &best {
            self.techset_swaps.set(name.to_string(), b.clone());
        }
        Ok(best)
    }

    pub(crate) fn pre_material_technique_set(&mut self, d: NodeId) -> Result<bool> {
        let mut name = query::asset_name(self.st, d).unwrap_or_default();
        if name.starts_with(',') {
            name.remove(0); // the PC file only names it too
        }
        loop {
            let key = AssetKey::new(TECHSET, name.clone());
            if self.resident.has(&key) || self.named.contains(&(key.ty.clone(), name.clone())) {
                let made = self.reference(TECHSET, &name)?;
                self.replace(d, made)?;
                return Ok(true);
            }
            if let Some(&src) = self.library.find(&key) {
                let made = self.copy_in(src)?;
                self.replace(d, made)?;
                return Ok(true);
            }
            let near = self.nearest_techset(&name)?;
            let Some(near) = near.filter(|n| *n != name) else {
                return Err(port_error(format!("no 360 shader set {name} in the stock files given")));
            };
            if self.hiding(&name, &near) {
                self.invisible_ts.insert(d);
            }
            let s = self.st.make_str(near.clone());
            self.st.set_child(d, "name", Some(s));
            name = near;
        }
    }

    // A shader set's techniques, or None for a set that is only a name (most stock maps
    // only name the sets common_mp has: every technique empty). `seen`: the sets it was
    // remapped from.
    fn techniques(&mut self, ts: Option<NodeId>, seen: &mut HashSet<NodeId>) -> Result<Option<NodeId>> {
        let Some(ts) = ts else { return Ok(None) };
        if !seen.insert(ts) {
            return Err(Error::new("a shader set is remapped to itself"));
        }
        let c = self.st.child(ts, "remappedTechniqueSet");
        if let Some(rm) = query::deref_struct(self.st, c)?
            && let Some(t) = self.techniques(Some(rm), seen)?
        {
            return Ok(Some(t));
        }
        let c = self.st.child(ts, "techniques");
        let techs = query::deref(self.st, c)?;
        match self.st.as_ptr_list(techs) {
            Some(list) if list.items.iter().any(Option::is_some) => Ok(techs),
            _ => Ok(None),
        }
    }

    /// Whether stock shader set `name` (or the one it is remapped to) has the technique, or
    /// nothing when no stock file given has it.
    pub(crate) fn techset_has(&mut self, name: &str, technique: i32) -> Result<Option<bool>> {
        if name.is_empty() {
            return Ok(None);
        }
        let name = query::without_commas(name);
        if let Some(&known) = self.techset_has_cache.get(&(name.clone(), technique)) {
            return Ok(known);
        }
        if self.stock_techniques.is_none() {
            // Stock shader sets by name, the copies that carry their techniques: reached
            // through the stock materials using them.
            self.stock_techniques = Some(Default::default());
            let stock: Vec<(AssetKey, NodeId)> = self
                .library
                .iter()
                .chain(self.resident.iter())
                .chain(self.common_materials.iter())
                .map(|(k, &v)| (k.clone(), v))
                .collect();
            for (k, v) in stock {
                let ts = match k.ty.as_str() {
                    "Material" => {
                        let c = self.st.child(v, "techniqueSet");
                        query::deref_struct(self.st, c)?
                    }
                    TECHSET => Some(v),
                    _ => None,
                };
                if let Some(techs) = self.techniques(ts, &mut HashSet::new())? {
                    let n = ts.and_then(|ts| query::asset_name(self.st, ts)).unwrap_or_default();
                    let n = query::without_commas(&n);
                    let items = self.st.ptr_list(techs).items.clone();
                    if let Some(map) = self.stock_techniques.as_mut() {
                        map.entry(n).or_insert(items);
                    }
                }
            }
        }
        let out = self.stock_techniques.as_ref().and_then(|map| map.get(&name)).map(|items| {
            usize::try_from(technique).ok().and_then(|t| items.get(t)).is_some_and(Option::is_some)
        });
        self.techset_has_cache.insert((name, technique), out);
        Ok(out)
    }

    // ---- materials ------------------------------------------------------------------------

    /// The face culling a PC material draws itself with (the most common over its drawing
    /// passes: 4 up to the debug views at 44; 0-3 are depth and shadow passes, which some
    /// materials draw two-sided), or nothing.
    pub(crate) fn pc_cull_of(&mut self, d: NodeId) -> Result<Option<u32>> {
        let words = query::state_words(self.st, self.st.child(d, "stateBitsTable"));
        let Some(words) = words.filter(|w| !w.is_empty()) else { return Ok(None) };
        let entry = self.st.bytes(d, "stateBitsEntry")?;
        let mut votes = Vec::new();
        for &e in entry.iter().take(44).skip(4) {
            if let Some(w) = words.get(usize::from(e)) {
                votes.push((w.0 >> 14) & 3);
            }
        }
        Ok(most_common(&votes))
    }

    pub(crate) fn pre_material(&mut self, d: NodeId) -> Result<bool> {
        // (Name: one shared with another asset.)
        let own_name = query::asset_name(self.st, d);
        let shared = query::name(self.st, d);
        let name = own_name.or(shared).unwrap_or_default();
        if let Some(loaded) = name.strip_prefix(',') {
            // The PC file only names it (the game has it loaded): so does the 360 file.
            let made = self.reference("Material", loaded)?;
            self.replace(d, made)?;
            return Ok(true);
        }
        let mut own = false;
        let table = self.st.child(d, "textureTable");
        if let Some(tt) = self.st.as_array(table) {
            for t in tt.items.clone().into_iter().flatten() {
                if !self.st.is_struct(Some(t)) {
                    continue;
                }
                if let Some(u) = query::embedded(self.st, t, "u") {
                    let c = self.st.child(u, "image");
                    if let Some(image) = query::deref_struct(self.st, c)? {
                        let iname = query::asset_name(self.st, image).unwrap_or_default();
                        own |= self.in_iwd(&iname);
                    }
                }
            }
        }
        if self.resident.has(&AssetKey::new("Material", name.clone())) && !own {
            // The game has it loaded and the map brings no pictures of its own for it (a
            // map's $levelbriefing does, so it gets its own copy).
            let made = self.reference("Material", &name)?;
            self.replace(d, made)?;
            return Ok(true);
        }
        let c = self.st.child(d, "techniqueSet");
        let ts = query::deref_struct(self.st, c)?;
        let mut tsname = match ts {
            Some(ts) => query::asset_name(self.st, ts).unwrap_or_default(),
            None => String::new(),
        };
        if tsname.starts_with(',') {
            tsname.remove(0); // converted to a reference already
        }
        let orig_ts = tsname.clone();
        let mut tpl = self.material_templates.find(&tsname).copied();
        // A stock material of the same name and shader set: its own render state (culling,
        // draw order), not that of another one sharing the set.
        let key = AssetKey::new("Material", name.clone());
        let same = self.library.find(&key).or_else(|| self.common_materials.find(&key)).copied();
        if let Some(same) = same
            && !tsname.is_empty()
        {
            let c = self.st.child(same, "techniqueSet");
            if let Some(sts) = query::deref_struct(self.st, c)? {
                let sn = query::without_commas(&query::asset_name(self.st, sts).unwrap_or_default());
                if sn == tsname {
                    tpl = Some(same);
                }
            }
        }
        if tpl.is_none()
            && !tsname.is_empty()
            && let Some(near) = self.nearest_techset(&tsname)?
        {
            // No stock file given has this shader set: the closest one there is.
            // (A shader set's name is not empty without the set.)
            let ts = ts.ok_or_else(|| Error::new("a material without its shader set"))?;
            if self.hiding(&tsname, &near) {
                self.invisible_ts.insert(ts);
            }
            let s = self.st.make_str(near.clone());
            self.st.set_child(ts, "name", Some(s));
            tpl = self.material_templates.find(&near).copied();
            tsname = near;
        }
        if ts.is_some_and(|ts| self.invisible_ts.contains(&ts)) {
            self.invisible.insert(d);
        }
        let Some(tpl) = tpl else {
            return Err(port_error(format!("material {name}: no stock 360 material uses shader set {tsname}")));
        };
        // PC render state means nothing to the 360: it comes from the template, all but its
        // face culling (the same bits on both).
        if let Some(cull) = self.pc_cull_of(d)?
            && (cull == 1 || cull == 3)
        {
            self.pc_cull.insert(d, cull);
        }
        self.st.pop_child(d, "stateBitsTable");
        self.material_template.insert(d, tpl);
        // The PC's draw order (sort key) means the same on the 360. Kept unless the shader
        // set hides it, or stock uses the key only for the other kind of set (with the lit
        // technique: sort keys 0-33; without: 34 and up; the game sorts one key's materials
        // by that technique's shaders).
        if self.stock_sort_keys.is_none() {
            self.stock_sort_keys = Some(Default::default());
            let stock: Vec<(AssetKey, NodeId)> = self
                .library
                .iter()
                .chain(self.resident.iter())
                .chain(self.common_materials.iter())
                .map(|(k, &v)| (k.clone(), v))
                .collect();
            for (k, v) in stock {
                if k.ty != "Material" {
                    continue;
                }
                let c = self.st.child(v, "techniqueSet");
                let sts = query::deref_struct(self.st, c)?;
                let sname = match sts {
                    Some(sts) => query::asset_name(self.st, sts).unwrap_or_default(),
                    None => String::new(),
                };
                let lit = self.techset_has(&sname, 9)?;
                let info = self.st.sub(v, "info")?;
                let sort = self.st.int(info, "sortKey")?;
                if let Some(keys) = self.stock_sort_keys.as_mut() {
                    keys.entry(sort).or_default().insert(lit.map_or(-1, i32::from));
                }
            }
        }
        let info = self.st.sub(d, "info")?;
        let sort = self.st.int(info, "sortKey")?;
        let sides = self.stock_sort_keys.as_ref().and_then(|keys| keys.get(&sort)).cloned();
        if !self.invisible.contains(&d)
            && !orig_ts.contains("_distfalloff")
            && let Some(mut known) = sides
        {
            let ours = self.techset_has(&tsname, 9)?;
            known.remove(&-1);
            match ours {
                Some(ours) if !known.is_empty() && known != BTreeSet::from([i32::from(ours)]) => {
                    let tinfo = self.st.sub(tpl, "info")?;
                    let theirs = self.st.int(tinfo, "sortKey")?;
                    if sort != theirs {
                        self.sort_key_kind_swapped.push(SortKeySwap { material: name, pc: sort, stock: theirs });
                    }
                }
                _ => {
                    self.pc_sort.insert(d, sort);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn post_material(&mut self, d: NodeId) -> Result<()> {
        let tpl =
            self.material_template.remove(&d).ok_or_else(|| Error::new("a material converted without its template"))?;
        for k in ["stateBitsEntry", "stateBitsCount", "stateFlags", "cameraRegion", "stateBitsTable", "unknown"] {
            let from = self.st.bytes(tpl, k)?.to_vec();
            copy_into(self.st.bytes_mut(d, k)?, &from)?;
        }
        let info = self.st.sub(d, "info")?;
        let tinfo = self.st.sub(tpl, "info")?;
        let sort = match self.pc_sort.remove(&d) {
            Some(sort) => sort,
            None => self.st.int(tinfo, "sortKey")?,
        };
        self.st.set(info, "sortKey", sort)?;
        // The game builds drawSurf itself when it registers the material; stock 360 files
        // always hold zero here (the PC keeps its own bit layout).
        {
            let from = self.st.bytes(tinfo, "drawSurf")?.to_vec();
            copy_into(self.st.bytes_mut(info, "drawSurf")?, &from)?;
        }
        let mut c = self.st.child(tpl, "stateBitsTable");
        if let Some(r) = self.st.as_ref(c) {
            c = if r.rel == 0 { r.target } else { None };
        }
        let Some(mut c) = c else {
            return Err(port_error("a template material's render state can't be copied"));
        };
        let words = query::state_words(self.st, Some(c)).unwrap_or_default();
        let culls: Vec<u32> = self
            .st
            .bytes(d, "stateBitsEntry")?
            .iter()
            .filter_map(|&e| words.get(usize::from(e)).map(|w| (w.0 >> 14) & 3))
            .collect();
        let main = most_common(&culls);
        // (Only where the template draws mostly otherwise: a same-named stock material
        // drawing two-sided already keeps its own state.)
        if let Some(cull) = self.pc_cull.remove(&d)
            && main.is_some_and(|m| m != cull)
        {
            c = query::with_cull(self.st, Some(c), cull)?;
            self.two_sided += 1;
        }
        self.st.set_child(d, "stateBitsTable", Some(c));
        self.st.set_ptr(d, "stateBitsTable", FOLLOWING)?;
        let hidden = self.invisible.remove(&d);
        let name = query::asset_name(self.st, d).unwrap_or_default();
        if hidden {
            // Its own pictures go: every one the shader set reads is a stand-in, the colour
            // one see-through.
            let empty = self.st.make_array(Vec::new());
            self.st.set_child(d, "textureTable", Some(empty));
            self.st.set_ptr(d, "textureTable", FOLLOWING)?;
            self.st.set(d, "textureCount", 0)?;
            if !self.invisible_materials.contains(&name) {
                self.invisible_materials.push(name.clone());
            }
        }
        self.fill_textures(d, tpl, &name, hidden)
    }

    /// Gives material `d` every picture its shader set reads (as stock material `tpl`, of
    /// the same set, has them) that it lacks: a swapped-in set can want one the PC material
    /// never had, and a shader without its picture draws nothing. Stand-ins: a flat normal
    /// map, no shine, magenta colour.
    pub(crate) fn fill_textures(&mut self, d: NodeId, tpl: NodeId, name: &str, invisible: bool) -> Result<()> {
        let table = self.st.child(d, "textureTable");
        let mut items: Vec<Option<NodeId>> = self.st.as_array(table).map(|a| a.items.clone()).unwrap_or_default();
        let mut have = BTreeSet::new();
        for e in items.iter().flatten().copied() {
            if self.st.is_struct(Some(e)) {
                have.insert(self.st.u(e, "nameHash")? as u32);
            }
        }
        let image_info = self.x.infos.get("GfxImage").map(|&i| &self.x.structs[i]);
        let image_info = image_info.ok_or_else(|| Error::new("the 360's rules have no GfxImage"))?;
        let image_type = crate::tree::Ty::new(self.x, image_info.ctype);
        let image_asset: &'static str = &image_info.name;
        let mut added = Vec::new();
        let theirs = self.st.child(tpl, "textureTable");
        let theirs = self.st.as_array(theirs).map(|a| a.items.clone()).unwrap_or_default();
        for e in theirs.into_iter().flatten() {
            if !self.st.is_struct(Some(e)) || have.contains(&(self.st.u(e, "nameHash")? as u32)) {
                continue;
            }
            let u = query::embedded(self.st, e, "u");
            let img = match u {
                Some(u) => {
                    let c = self.st.child(u, "image");
                    query::deref_struct(self.st, c)?
                }
                None => None,
            };
            let iname = match img {
                Some(img) => query::asset_name(self.st, img).unwrap_or_default(),
                None => String::new(),
            };
            let iname = query::without_commas(&iname);
            let semantic = self.st.int(e, "semantic")?;
            let made = if semantic == 5 {
                self.reference("GfxImage", "$identitynormalmap")?
            } else if semantic == 8 {
                self.reference("GfxImage", "$black")?
            } else if invisible {
                let blank = self.st.zero(image_type, true);
                let made = self.see_through(blank, &format!("~invisible_{}", self.invisible_materials.len()))?;
                self.st.st_mut(made).asset = image_asset;
                made
            } else if iname.starts_with('$')
                && (self.resident.has(&AssetKey::new("GfxImage", iname.clone()))
                    || self.named.contains(&("GfxImage".to_string(), iname.clone())))
            {
                self.reference("GfxImage", &iname)?
            } else {
                let blank = self.st.zero(image_type, true);
                let made = self.magenta(blank, Some(format!("~magenta_{}", self.magenta_materials.len())))?;
                self.st.st_mut(made).asset = image_asset;
                if !self.magenta_materials.iter().any(|m| m == name) {
                    self.magenta_materials.push(name.to_string());
                }
                made
            };
            let entry = self.st.copy(e)?;
            let mu = self.st.sub(entry, "u")?;
            let big = self.st.st(mu).big;
            let data = self.st.data_mut(mu);
            data.fill(0);
            if data.len() < 4 {
                return Err(Error::new("a material's picture slot is too small"));
            }
            store(data, 4, u64::from(FOLLOWING), big);
            self.st.st_mut(mu).used.clear();
            let ch = self.st.make_children();
            self.st.children_mut(ch).set("image", Some(made), &[]);
            self.st.st_mut(mu).set_ch(Some(ch));
            added.push(Some(entry));
        }
        if added.is_empty() {
            return Ok(());
        }
        items.extend(added);
        let mut keyed = Vec::with_capacity(items.len());
        for item in items {
            let hash = match item.filter(|&n| self.st.is_struct(Some(n))) {
                Some(n) => self.st.u(n, "nameHash")?,
                None => return Err(Error::new("a material's picture table holds something else")),
            };
            keyed.push((hash, item));
        }
        keyed.sort_by_key(|k| k.0); // stable: ties keep their order
        let items: Vec<Option<NodeId>> = keyed.into_iter().map(|(_, item)| item).collect();
        let count = items.len() as i64;
        let out = self.st.make_array(items);
        self.st.set_child(d, "textureTable", Some(out));
        self.st.set_ptr(d, "textureTable", FOLLOWING)?;
        self.st.set(d, "textureCount", count)
    }
}

/// `from` over the start of `to`.
fn copy_into(to: &mut [u8], from: &[u8]) -> Result<()> {
    to.get_mut(..from.len())
        .ok_or_else(|| Error::new("a material's render state is larger than its room"))?
        .copy_from_slice(from);
    Ok(())
}
