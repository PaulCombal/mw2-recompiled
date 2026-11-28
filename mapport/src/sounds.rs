// The map's sounds: alias lists from the stock 360 files, and the map's own audio encoded
// as XMA (xma.rs) for those no stock file has.

use crate::error::{Error, Result};
use crate::num::{store, truncate32};
use crate::ordered::Ordered;
use crate::porter::{EncodedSound, HeadAudio, Porter, StockAlias, ZipEntry, ZipIndex};
use crate::query::{alias_name, each_held, embedded, joined, named, only_named, pool_name, without_commas};
use crate::stock::{entry_asset, entry_type};
use crate::tree::{FOLLOWING, NodeId, Roots, Store, Ty, collect, elements, for_each};
use crate::xma::{Pcm, PcmFormat, SoundPcm, Xma, encode_xma, read_sound};
use crate::zip::Zip;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

/// XMA made at most (stock maps carry 4 to 8 MB of sounds).
const ENCODED_SOUND_BUDGET_MB: usize = 12;

/// Alias head members the PC's alias keeps (the stock "null" alias's head gives the rest).
const HEAD_FIELDS: [&str; 16] = [
    "sequence",
    "volMin",
    "volMax",
    "pitchMin",
    "pitchMax",
    "distMin",
    "distMax",
    "velocityMin",
    "flags",
    "probability",
    "lfePercentage",
    "centerPercentage",
    "startDelay",
    "envelopMin",
    "envelopMax",
    "envelopPercentage",
];

/// The 360 name of a PC sound file (dir/name.wav, .mp3, ...): stock 360 sounds have no
/// extension.
fn sound_name(path: &str) -> String {
    let Some(dot) = path.rfind('.') else { return path.to_string() };
    if path[dot..].contains('/') {
        return path.to_string();
    }
    let ext = path[dot..].to_ascii_lowercase();
    if matches!(ext.as_str(), ".wav" | ".mp3" | ".flac" | ".ogg") { path[..dot].to_string() } else { path.to_string() }
}

/// A PC LoadedSound's audio bytes (the leaf holding them), or None for one that only names
/// its sound.
fn sound_data(st: &mut Store, ls: NodeId) -> Option<NodeId> {
    let s = embedded(st, ls, "sound")?;
    let mut d = st.child(s, "data");
    if let Some(r) = st.as_ref(d) {
        d = r.target;
    }
    d.filter(|&l| st.as_leaf(Some(l)).is_some_and(|lf| !lf.raw.is_empty()))
}

/// A PC LoadedSound's audio: a .wav file, or plain samples its info describes as 16-bit
/// PCM.
fn loaded_pcm(st: &mut Store, ls: NodeId) -> Result<SoundPcm> {
    let sound = embedded(st, ls, "sound").ok_or_else(|| Error::new("a loaded sound without its sound"))?;
    let info = embedded(st, sound, "info");
    let mut plain = None;
    if let Some(info) = info
        && st.int(info, "format")? == 1
        && st.int(info, "bits")? == 16
        && (st.int(info, "channels")? == 1 || st.int(info, "channels")? == 2)
        && st.int(info, "rate")? != 0
    {
        plain = Some(PcmFormat { rate: st.int(info, "rate")? as u32, channels: st.int(info, "channels")? as i32 });
    }
    let data = sound_data(st, ls).ok_or_else(|| Error::new("a loaded sound without its audio"))?;
    Ok(read_sound(&st.leaf(data).raw, plain))
}

fn megabytes(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / 1048576.0)
}

/// The sound/ files of zip archives, by path under sound/ in lower case (later ones win).
fn sound_index(zips: &[&Rc<Zip>]) -> ZipIndex {
    let mut index = ZipIndex::new();
    for z in zips {
        for (k, n) in z.names().iter().enumerate() {
            let low = n.to_ascii_lowercase().replace('\\', "/");
            if let Some(rest) = low.strip_prefix("sound/") {
                index.insert(rest.to_string(), ZipEntry { zip: Rc::clone(z), index: k });
            }
        }
    }
    index
}

/// Where a PC alias head's audio is read from, once it is needed.
enum Source {
    None,
    Loaded(NodeId),
    Zip(ZipEntry),
}

impl Porter<'_> {
    /// An alias list entry for each sound the map's destructibles need and lack: only its
    /// name, which stock_sounds fills from the stock file.
    pub(crate) fn add_destructible_aliases(&mut self) -> Result<()> {
        let mut have: BTreeSet<String> = BTreeSet::new();
        let assets = self.root.assets.clone();
        for_each(&mut *self.st, Roots::Entries(&assets), |st, n| {
            if st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("snd_alias_list_t")) {
                have.insert(alias_name(st, n).unwrap_or_default().to_ascii_lowercase());
            }
            Ok(())
        })?;
        let p = self.p;
        let info = p
            .infos
            .get("snd_alias_list_t")
            .map(|&id| &p.structs[id])
            .ok_or_else(|| Error::new("no type snd_alias_list_t"))?;
        let mut made: Vec<NodeId> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for name in self.destructible_aliases.clone() {
            if have.contains(&name.to_ascii_lowercase()) || !self.stock_aliases.has(&name) {
                continue;
            }
            let d = self.st.zero(Ty::new(p, info.ctype), false);
            self.st.st_mut(d).asset = info.name.as_str();
            let s = self.st.make_str(name.clone());
            self.st.set_child(d, "aliasName", Some(s));
            made.push(self.st.make_entry("sound", Some(d)));
            have.insert(name.to_ascii_lowercase());
            names.push(name);
        }
        let count = made.len();
        self.root.assets.splice(0..0, made);
        if count != 0 {
            self.say(&format!(
                "  {count} destructible sounds added from the stock 360 files ({})",
                joined(&names, usize::MAX, "")
            ))?;
        }
        Ok(())
    }

    /// Sound alias lists come from a stock 360 file with the same alias, audio and all, as
    /// stock maps carry them: the PC file only names its sound files, which the 360 has
    /// nothing under. An alias no stock file has gets the PC's own audio, encoded (the map's
    /// loaded sounds, streamed ones the .iwd files have), else the stock "null" alias's
    /// silent sound, under its own name. The PC's sound files then go.
    pub(crate) fn stock_sounds(&mut self) -> Result<()> {
        let mut swapped = 0;
        let mut silent: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let mut played: Vec<String> = Vec::new();
        let null = self.stock_aliases.find(&"null".to_string()).cloned();
        let mut ours: Vec<NodeId> = Vec::new();
        let assets = self.root.assets.clone();
        let done = &self.done;
        for_each(&mut *self.st, Roots::Entries(&assets), |st, n| {
            if st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("snd_alias_list_t")) && !done.contains(&n) {
                ours.push(n);
            }
            Ok(())
        })?;
        // Stock aliases share sound files (one alias's points into another's): those from
        // one file are made to stand alone together, so they keep sharing, and go in that
        // file's order, shared parts first.
        let mut take: HashMap<NodeId, StockAlias> = HashMap::new();
        let mut by_file: Ordered<usize, Ordered<NodeId, NodeId>> = Ordered::new();
        for &d in &ours {
            let Some(n) = alias_name(self.st, d) else { continue };
            if n.starts_with(',') {
                continue;
            }
            if let Some(s) = self.stock_aliases.find(&n) {
                take.insert(d, s.clone());
                by_file.find_or_add(s.file.id, Ordered::new()).find_or_add(s.alias, s.alias);
            }
        }
        for (_, group) in by_file.iter() {
            let roots: Vec<Option<NodeId>> = group.iter().map(|(_, &v)| Some(v)).collect();
            self.localize(&roots)?;
        }
        let mut file_order: HashMap<usize, usize> = HashMap::new();
        for (_, s) in self.stock_aliases.iter() {
            let next = file_order.len();
            file_order.entry(s.file.id).or_insert(next);
        }
        let mut rank: HashMap<NodeId, (usize, i64)> = HashMap::new();
        for &d in &ours {
            let Some(s) = take.get(&d) else { continue };
            let made = self.st.copy(s.alias)?;
            self.st.st_mut(made).slot = None;
            self.replace(d, made)?;
            let order = *file_order.get(&s.file.id).ok_or_else(|| Error::new("a stock alias of no file"))?;
            rank.insert(d, (order, s.order));
            swapped += 1;
        }
        let mut at: Vec<usize> = Vec::new();
        let mut ranked: Vec<(NodeId, (usize, i64))> = Vec::new();
        for (i, &e) in self.root.assets.iter().enumerate() {
            if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)))
                && let Some(&r) = rank.get(&a)
            {
                at.push(i);
                ranked.push((e, r));
            }
        }
        ranked.sort_by_key(|r| r.1);
        for (k, &i) in at.iter().enumerate() {
            self.root.assets[i] = ranked[k].0;
        }
        // Silent ones share the first one's sound (in the order they are written).
        let mut pos: HashMap<NodeId, usize> = HashMap::new();
        for (i, &e) in self.root.assets.iter().enumerate() {
            if let Some(a) = entry_asset(self.st, e) {
                pos.entry(a).or_insert(i);
            }
        }
        let end = self.root.assets.len();
        ours.sort_by_key(|d| pos.get(d).copied().unwrap_or(end));
        for e in self.root.assets.clone() {
            if let Some(ls) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)))
                && entry_type(self.st, e) == "loaded_sound"
            {
                let n = pool_name(self.st, ls);
                if named(&n)
                    && !only_named(&n)
                    && let Some(n) = n
                {
                    self.pc_loaded.entry(n.to_ascii_lowercase()).or_insert(ls);
                }
            }
        }
        let mut null_file: Option<NodeId> = None;
        for &d in &ours {
            if rank.contains_key(&d) {
                continue;
            }
            let Some(name) = alias_name(self.st, d) else { continue };
            if name.is_empty() || name.starts_with(',') {
                continue;
            }
            let Some(null) = &null else {
                missing.push(name);
                continue;
            };
            let copy = self.copy_in(null.alias)?;
            let made = self.st.deep_copy_struct(copy, None)?;
            // (An ambience track the script no longer plays isn't encoded.)
            let audio = if self.ambient_swaps.has(&name) { Vec::new() } else { self.pc_audio(d)? };
            if !audio.is_empty() {
                self.encoded_heads(made, &audio)?;
            }
            self.name_loaded_curves(made)?;
            let st = &mut *self.st;
            let s = st.make_str(name.clone());
            st.set_child(made, "aliasName", Some(s));
            let mut head = st.child(made, "head");
            if let Some(r) = st.as_ref(head) {
                head = r.target;
            }
            for x in each_held(st, head) {
                let Some(h) = x.filter(|&h| st.is_struct(Some(h))) else { continue };
                let s = st.make_str(name.clone());
                st.set_child(h, "aliasName", Some(s));
                st.set_ptr(h, "aliasName", FOLLOWING)?;
                let Some(sf) = st.child(h, "soundFile").filter(|&f| st.is_struct(Some(f))) else { continue };
                if !audio.is_empty() {
                    continue;
                }
                let Some(nf) = null_file else {
                    null_file = Some(sf);
                    continue;
                };
                // Each keeps its own sound file record and shares the first one's silent
                // sound through that sound's slot. Pointing at the first one's record pointed
                // into another asset's temporary data, which the 360 reuses once that asset
                // is loaded.
                let first_u = embedded(st, nf, "u");
                let first = first_u.and_then(|fu| st.child(fu, "loadSnd")).filter(|&f| st.is_struct(Some(f)));
                let u = embedded(st, sf, "u");
                if let Some(slot) = first.and_then(|f| st.st(f).slot)
                    && let Some(u) = u
                    && st.is_struct(st.child(u, "loadSnd"))
                {
                    let r = st.make_ref(1);
                    st.reference_mut(r).target = Some(slot);
                    st.set_child(u, "loadSnd", Some(r));
                }
            }
            self.replace(d, made)?;
            if audio.is_empty() { &mut silent } else { &mut played }.push(name);
        }
        // The PC's sound files (each only a name) are pointed at by nothing now.
        let mut held: HashSet<NodeId> = HashSet::new();
        for e in self.root.assets.clone() {
            if entry_type(self.st, e) == "loaded_sound" {
                continue;
            }
            let asset = entry_asset(self.st, e);
            for_each(&mut *self.st, Roots::One(asset), |st, n| {
                let Some(ch) = st.as_struct(Some(n)).and_then(|s| s.ch()) else { return Ok(()) };
                for (_, c) in &st.children(ch).items {
                    for x in each_held(st, *c) {
                        if let Some(r) = st.as_ref(x) {
                            let mut t = r.target;
                            if let Some(s) = st.as_slot(t) {
                                t = s.asset;
                            }
                            if let Some(t) = t {
                                held.insert(t);
                            }
                        } else if let Some(x) = x.filter(|&x| st.is_struct(Some(x))) {
                            held.insert(x);
                        }
                    }
                }
                Ok(())
            })?;
        }
        let before = self.root.assets.len();
        let st = &*self.st;
        self.root.assets.retain(|&e| {
            !(entry_type(st, e) == "loaded_sound"
                && entry_asset(st, e).is_some_and(|a| st.is_struct(Some(a)) && !held.contains(&a)))
        });
        self.say(&format!(
            "  sounds: {swapped} aliases from the stock 360 files, {} silent (no stock copy), {} PC sound files left out",
            silent.len(),
            before - self.root.assets.len()
        ))?;
        if !played.is_empty() {
            // (Sums: the order the sounds are gone through doesn't matter.)
            let (mut sounds, mut bytes, mut resident) = (0usize, 0usize, 0usize);
            for sound in self.encoded.values() {
                if let Some(x) = &sound.xma {
                    sounds += 1;
                    bytes += x.data.len();
                }
                resident += usize::from(sound.resident);
            }
            self.say(&format!(
                "  {} aliases play the PC's audio, encoded for the 360 ({sounds} sounds, {} MB; {resident} more the game has loaded)",
                played.len(),
                megabytes(bytes)
            ))?;
        }
        if self.encoded_bytes >= ENCODED_SOUND_BUDGET_MB * 1024 * 1024 {
            self.warn(&format!("encoded sounds reached {ENCODED_SOUND_BUDGET_MB} MB: the rest stay silent"))?;
        }
        if self.compressed_sounds {
            self.warn("some sounds are .mp3 or compressed .wav files, which aren't decoded: they stay silent")?;
        }
        if !self.no_audio.is_empty() {
            self.say(&format!(
                "    no audio found for {} sound files{}",
                self.no_audio.len(),
                if self.game_iwds.is_empty() { " (give the PC game folder for its streamed sounds)" } else { "" }
            ))?;
        }
        if !silent.is_empty() {
            self.say(&format!("    silent: {}", joined(&silent, 20, " ...")))?;
        }
        if !missing.is_empty() {
            self.warn(&format!(
                "{} sound aliases have no stock copy and no stock \"null\" alias was found to stand in: add a stock map, they stay as the PC has them",
                missing.len()
            ))?;
        }
        Ok(())
    }

    /// Curves (SndCurve) in an alias list made here that the game has loaded only name
    /// them, as stock maps do: a copy of its own in each took one of the game's 64 curve
    /// places apiece.
    pub(crate) fn name_loaded_curves(&mut self, alias: NodeId) -> Result<()> {
        for o in collect(self.st, Roots::One(Some(alias)))? {
            let Some(ch) = self.st.as_struct(Some(o)).and_then(|s| s.ch()) else { continue };
            for i in 0..self.st.children(ch).items.len() {
                let c = self.st.children(ch).items[i].1;
                if let Some(a) = c.filter(|&a| self.st.is_array(Some(a))) {
                    for j in 0..self.st.array(a).items.len() {
                        let x = self.st.array(a).items[j];
                        if let Some(r) = self.loaded_curve(x)? {
                            self.st.array_mut(a).items[j] = Some(r);
                        }
                    }
                } else if let Some(pl) = c.filter(|&p| self.st.is_ptr_list(Some(p))) {
                    for j in 0..self.st.ptr_list(pl).items.len() {
                        let x = self.st.ptr_list(pl).items[j];
                        if let Some(r) = self.loaded_curve(x)? {
                            self.st.ptr_list_mut(pl).items[j] = Some(r);
                        }
                    }
                } else if let Some(r) = self.loaded_curve(c)? {
                    self.st.children_mut(ch).items[i].1 = Some(r);
                }
            }
        }
        Ok(())
    }

    /// What stands for a curve the game has loaded: a reference naming it.
    fn loaded_curve(&mut self, x: Option<NodeId>) -> Result<Option<NodeId>> {
        let Some(c) = x.filter(|&c| self.st.as_struct(Some(c)).is_some_and(|s| s.is_asset_named("SndCurve"))) else {
            return Ok(None);
        };
        let name = pool_name(self.st, c).unwrap_or_default();
        if name.is_empty()
            || name.starts_with(',')
            || !self.loaded.contains(&("SndCurve".to_string(), name.to_ascii_lowercase()))
        {
            return Ok(None);
        }
        let r = self.reference("SndCurve", &name)?;
        self.done.insert(r);
        Ok(Some(r))
    }

    /// The heads of a PC alias list whose audio is here (the map's loaded sounds, streamed
    /// ones in the .iwd files); each sound is encoded once.
    pub(crate) fn pc_audio(&mut self, alias: NodeId) -> Result<Vec<HeadAudio>> {
        let mut heads = self.st.child(alias, "head");
        if let Some(r) = self.st.as_ref(heads) {
            heads = r.target;
        }
        let mut out = Vec::new();
        for x in each_held(self.st, heads) {
            let Some(h) = x.filter(|&h| self.st.is_struct(Some(h))) else { continue };
            let Some(file) = self.st.child(h, "soundFile").filter(|&f| self.st.is_struct(Some(f))) else { continue };
            let u = embedded(self.st, file, "u");
            let mut key = String::new();
            let mut read = Source::None;
            let ty = self.st.int(file, "type")?;
            if ty == 1 {
                // loaded
                let mut n = u.and_then(|u| self.st.child(u, "loadSnd"));
                if let Some(r) = self.st.as_ref(n) {
                    n = match self.st.as_slot(r.target) {
                        Some(s) => s.asset,
                        None => r.target,
                    };
                }
                if let Some(mut ls) = n.filter(|&l| self.st.is_struct(Some(l))) {
                    key = without_commas(&pool_name(self.st, ls).unwrap_or_default()).to_ascii_lowercase();
                    // (An alias of the map's may only name a sound the map loads.)
                    if sound_data(self.st, ls).is_none()
                        && let Some(&found) = self.pc_loaded.get(&key)
                    {
                        ls = found;
                    }
                    if sound_data(self.st, ls).is_some() {
                        read = Source::Loaded(ls);
                    }
                }
            } else if ty == 2 {
                // streamed: from the .iwd files
                let u = u.ok_or_else(|| Error::new("a streamed sound file without its sound"))?;
                let ss = embedded(self.st, u, "streamSnd");
                let dir = ss.and_then(|s| self.st.as_str(self.st.child(s, "dir"))).map(|s| s.b.clone());
                let name = ss.and_then(|s| self.st.as_str(self.st.child(s, "name"))).map(|s| s.b.clone());
                if let Some(name) = name {
                    key = match dir {
                        Some(dir) if !dir.is_empty() => format!("{dir}/{name}"),
                        _ => name,
                    };
                    key = key.replace('\\', "/").to_ascii_lowercase();
                    if let Some(found) = self.sound_files()?.get(&key) {
                        read = Source::Zip(found.clone());
                    }
                }
            }
            if key.is_empty() {
                continue;
            }
            if !self.encoded.contains_key(&key) {
                let mut sound = EncodedSound::default();
                // The game has it loaded already (common_mp): named only, as stock maps do.
                if self.loaded.contains(&("LoadedSound".to_string(), sound_name(&key))) {
                    sound.resident = true;
                } else {
                    let pcm = match read {
                        Source::None => SoundPcm { pcm: None, compressed: false },
                        Source::Loaded(ls) => loaded_pcm(self.st, ls)?,
                        Source::Zip(found) => read_sound(&found.zip.read(found.index)?, None),
                    };
                    self.compressed_sounds |= pcm.compressed;
                    match &pcm.pcm {
                        Some(p) if self.encoded_bytes < ENCODED_SOUND_BUDGET_MB * 1024 * 1024 => {
                            let x = self.encode(&key, p)?;
                            self.encoded_bytes += x.data.len();
                            sound.xma = Some(x);
                        }
                        Some(_) => {}
                        None => {
                            self.no_audio.insert(key.clone());
                        }
                    }
                }
                self.encoded.insert(key.clone(), sound);
            }
            if self.encoded.get(&key).is_some_and(EncodedSound::plays) {
                out.push(HeadAudio { head: h, sound: key });
            }
        }
        Ok(out)
    }

    /// One sound encoded; the log shows a long one going (ambience tracks take a while).
    pub(crate) fn encode(&mut self, name: &str, pcm: &Pcm) -> Result<Xma> {
        let seconds =
            pcm.samples.len().checked_div((pcm.channels as usize).wrapping_mul(pcm.rate as usize)).unwrap_or(0);
        if seconds >= 30 {
            self.say(&format!("    encoding {name} ({seconds} seconds of sound)"))?;
        }
        let start = Instant::now();
        let x = encode_xma(&pcm.samples, pcm.rate, pcm.channels);
        self.encode_seconds += start.elapsed().as_secs_f64();
        self.encode_count += 1;
        if self.encode_count % 20 == 0 {
            self.say(&format!(
                "    {} sounds encoded so far ({} MB, {} s)",
                self.encode_count,
                megabytes(self.encoded_bytes + x.data.len()),
                truncate32(self.encode_seconds)
            ))?;
        }
        Ok(x)
    }

    /// Alias list `alias` (a copy of the stock "null" one) gets a head for each (PC head,
    /// sound) in `audio`: the null head with the PC's numbers, playing its own 360 sound (a
    /// loaded one, as stock aliases' are). A sound used again points at its first copy.
    pub(crate) fn encoded_heads(&mut self, alias: NodeId, audio: &[HeadAudio]) -> Result<()> {
        let mut held = self.st.child(alias, "head");
        if let Some(r) = self.st.as_ref(held) {
            held = r.target;
        }
        let list = held.filter(|&l| self.st.is_array(Some(l)));
        let tpl = match list {
            Some(l) => *self.st.array(l).items.first().ok_or_else(|| Error::new("a sound alias without heads"))?,
            None => held,
        };
        let tpl =
            tpl.filter(|&t| self.st.is_struct(Some(t))).ok_or_else(|| Error::new("a sound alias without heads"))?;
        let mut heads: Vec<Option<NodeId>> = Vec::new();
        for HeadAudio { head: pc, sound: key } in audio {
            let pc = *pc;
            let h = self.st.deep_copy_struct(tpl, None)?;
            let st = &mut *self.st;
            for k in HEAD_FIELDS {
                if st.has(pc, k) && st.has(h, k) {
                    let m = st.m(h, k)?;
                    if st.st(h).ty.of(m).get().is_float {
                        let v = st.f(pc, k)?;
                        st.set_f(h, k, v)?;
                    } else {
                        let v = st.int(pc, k)?;
                        st.set(h, k, v)?;
                    }
                }
            }
            // Bit 7: a loaded sound, bit 8: streamed (stock: 1,684 and 3,802 of 5,486 heads).
            let flags = st.int(h, "flags")?;
            st.set(h, "flags", (flags | 0x80) & !0x100i64)?;
            for k in ["secondaryAliasName", "chainAliasName"] {
                if let Some(name) = st.as_str(st.child(pc, k)).map(|s| s.b.clone())
                    && st.has(h, k)
                {
                    st.set_ptr(h, k, FOLLOWING)?;
                    let s = st.make_str(name);
                    st.set_child(h, k, Some(s));
                }
            }
            let no_file = || Error::new("a sound alias head without its sound file");
            let sf = st.child(h, "soundFile").filter(|&f| st.is_struct(Some(f))).ok_or_else(no_file)?;
            let u = embedded(st, sf, "u").ok_or_else(no_file)?;
            let (resident, first) = {
                let sound = self.encoded.get(key).ok_or_else(|| Error::new("a sound not encoded"))?;
                (sound.resident, sound.first)
            };
            if resident {
                let r = self.reference("LoadedSound", &sound_name(key))?;
                self.done.insert(r);
                self.st.set_child(u, "loadSnd", Some(r));
            } else if let Some(first) = first {
                let st = &mut *self.st;
                let r = st.make_ref(1);
                st.reference_mut(r).target = st.st(first).slot;
                st.set_child(u, "loadSnd", Some(r));
            } else {
                let st = &mut *self.st;
                let ls = st.child(u, "loadSnd").filter(|&l| st.is_struct(Some(l))).ok_or_else(no_file)?;
                let slot = match st.st(ls).slot {
                    Some(slot) => slot,
                    None => {
                        let slot = st.make_slot(Some(ls));
                        st.st_mut(ls).slot = Some(slot);
                        slot
                    }
                };
                if let Some(s) = st.as_slot_mut(Some(slot)) {
                    s.asset = Some(ls);
                }
                let s = st.make_str(sound_name(key));
                st.set_child(ls, "name", Some(s));
                let sound = self.encoded.get_mut(key).ok_or_else(|| Error::new("a sound not encoded"))?;
                sound.first = Some(ls);
                let x = sound.xma.as_ref().ok_or_else(|| Error::new("a sound not encoded"))?;
                let no_audio = || Error::new("a sound file without its audio");
                let s = embedded(st, ls, "sound").ok_or_else(no_audio)?;
                let info = embedded(st, s, "info").ok_or_else(no_audio)?;
                let (t, big) = st.as_leaf(st.child(info, "data")).map(|l| (l.t, l.big)).ok_or_else(no_audio)?;
                let data = st.make_leaf(t, x.data.len() as i64, x.data.clone(), big);
                st.set_child(info, "data", Some(data));
                st.set(info, "dataSize", x.data.len() as i64)?;
                // Where the audio starts and ends (in bits), and which quarter of its last
                // frame it ends in (3: the whole frame plays).
                let bounds: [u32; 4] = [0xFFFFFFFF, 32, x.end_bit as u32, (3 << 24) | (3 << 16)];
                for k in 0..elements(st.m(info, "unknown")?) {
                    st.set_at(info, "unknown", bounds.get(k).map_or(0, |&b| i64::from(b)), k)?;
                }
                // XMA, one stream, its rate, one channel, its length in ms.
                let n = elements(st.m(s, "unknown")?);
                for k in 0..n {
                    st.set_at(s, "unknown", 0, k)?;
                }
                st.set_at(s, "unknown", 0x04000000, 0)?;
                st.set_at(s, "unknown", 0x01000000, 1)?;
                st.set_at(s, "unknown", i64::from(x.rate), 2)?;
                st.set_at(s, "unknown", 0x01000000, 3)?;
                st.set_at(s, "unknown", i64::from(x.ms()), n.wrapping_sub(1))?;
                let table = embedded(st, s, "seekTable").ok_or_else(no_audio)?;
                let (t, big) = st.as_leaf(st.child(table, "data")).map(|l| (l.t, l.big)).ok_or_else(no_audio)?;
                let mut words: Vec<u32> = vec![1, x.seek.len() as u32];
                words.extend_from_slice(&x.seek);
                let mut bytes = vec![0u8; words.len() * 4];
                for (k, &w) in words.iter().enumerate() {
                    store(&mut bytes[k * 4..k * 4 + 4], 4, u64::from(w), big);
                }
                st.set(table, "size", words.len() as i64)?;
                let data = st.make_leaf(t, words.len() as i64, bytes, big);
                st.set_child(table, "data", Some(data));
            }
            heads.push(Some(h));
        }
        match list {
            Some(l) => self.st.array_mut(l).items = heads,
            None => {
                let made = self.st.make_array(heads);
                self.st.set_child(alias, "head", Some(made));
            }
        }
        self.st.set(alias, "count", audio.len() as i64)
    }

    /// The sound/ files of the PC game's .iwd files, then the map's (later ones win), by
    /// path under sound/ in lower case.
    pub(crate) fn sound_files(&mut self) -> Result<&ZipIndex> {
        if self.sound_files.is_none() {
            let zips: Vec<&Rc<Zip>> = self.game_iwds.iter().chain(self.iwds.iter()).collect();
            self.sound_files = Some(sound_index(&zips));
        }
        Ok(self.sound_files.get_or_insert_default())
    }
}
