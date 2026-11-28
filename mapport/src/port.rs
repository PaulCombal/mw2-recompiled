// Converting a map made for the PC game (IW4x, or the 2009 game) into one the 360 game
// loads: its .ff (and the _load.ff, .iwd and .arena next to it), with what it needs from
// the console's own files. The conversion of one map from start to end: which stock files
// it reads, the Porter's run, and the file written.

use crate::convert::map_rel;
use crate::decals::{ground_code, layer_code};
use crate::error::{Error, Result};
use crate::fastfile::{Fastfile, read_fastfile, xbox_fastfile};
use crate::num::{load, store};
use crate::paths::{file_name, path_of, utf8};
use crate::porter::{Breakable, ColourSwap, Log, Porter, StockFile, Teams, port_error};
use crate::query::{
    asset_name, joined, lower, name as name_of, named, only_named, pool_name, rawfile_text, set_name, without_commas,
};
use crate::reader::{self, WriteOptions};
use crate::rules;
use crate::teams::{arena_teams, team_maps};
use crate::text;
use crate::tree::{NodeId, PakEntry, Ref, Roots, Store, Tree, for_each};
use crate::zip::Zip;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

// ---- the game's pools -----------------------------------------------------------------------

/// How many assets of each kind the game holds at once (TU6 default_mp.xex, the table its
/// "Exceeded limit of %d '%s' assets" check reads), shared by the map and the files loaded
/// with it.
const POOLS: [(&str, i32); 18] = [
    ("PhysPreset", 64),
    ("PhysCollmap", 1024),
    ("XAnimParts", 4096),
    ("XModelSurfs", 4096),
    ("XModel", 1536),
    ("Material", 4096),
    ("MaterialPixelShader", 8096),
    ("MaterialTechniqueSet", 768),
    ("GfxImage", 3584),
    ("snd_alias_list_t", 16000),
    ("SndCurve", 64),
    ("LoadedSound", 1350),
    ("GfxLightDef", 32),
    ("FxEffectDef", 600),
    ("FxImpactTable", 4),
    ("RawFile", 1024),
    ("StringTable", 400),
    ("TracerDef", 32),
];

/// The room each pool has.
pub fn pool_limit(ty: &str) -> i32 {
    POOLS.iter().find(|(t, _)| *t == ty).map_or(0, |&(_, n)| n)
}

/// The names an asset list's assets take a place in the game's pools under, lower case,
/// by type ("name" references take none).
pub fn pool_names(st: &mut Store, assets: &[NodeId]) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for_each(st, Roots::Entries(assets), |st, n| {
        if !st.is_struct(Some(n)) || pool_limit(st.st(n).asset) == 0 {
            return Ok(());
        }
        let name = pool_name(st, n);
        if named(&name) && !only_named(&name) {
            out.entry(st.st(n).asset.to_string()).or_default().insert(lower(name.as_deref().unwrap_or("")));
        }
        Ok(())
    })?;
    Ok(out)
}

// ---- the public face ------------------------------------------------------------------------

/// A PC map's files: the map, and what came with it.
#[derive(Clone, Debug)]
pub struct PcMap {
    /// <map>.ff
    pub ff: PathBuf,
    /// <map>_load.ff, its loading screen.
    pub load: Option<PathBuf>,
    /// <map>.iwd, its pictures.
    pub iwd: Option<PathBuf>,
    /// <map>.arena, its names and teams.
    pub arena: Option<PathBuf>,
}

impl PcMap {
    /// The map's files beside `ff`, named after it.
    pub fn beside(ff: &Path) -> PcMap {
        let base = without_extension(ff);
        let beside = |suffix: &str| {
            let p = with_suffix(&base, suffix);
            p.exists().then_some(p)
        };
        PcMap { ff: ff.to_path_buf(), load: beside("_load.ff"), iwd: beside(".iwd"), arena: beside(".arena") }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Ported {
    pub written: Vec<PathBuf>,
    /// What the conversion warns of (pictures the .iwd lacks, shader sets taken in place
    /// of others, ...), also in the log as notes.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PortOptions {
    /// The teams; else the map's .arena's, or the game's defaults.
    pub teams: Option<Teams>,
    /// The PC game's .iwd files (game_iwds), for the pictures and sounds a map takes from
    /// the game's own files rather than carrying them.
    pub game_iwds: Vec<PathBuf>,
}

/// Converts a PC map into `out_dir` (<map>.ff and, when it has one, <map>_load.ff and
/// <map>_preview.ff, the menus' picture), taking what it needs from the stock 360 files
/// given (the game's: code_post_gfx_mp.ff, common_mp.ff and the stock maps and their
/// _load.ff). A port error (or another Error) when the map can't be converted.
pub fn port_map(map: &PcMap, out_dir: &Path, stock: &[PathBuf], log: &Log, options: &PortOptions) -> Result<Ported> {
    MapPort::new(stock, &options.game_iwds, log)?.run(map, out_dir, options.teams.clone())
}

/// The .iwd files in a PC game's folder and its subfolders (main/, IW4x's iw4x/), in the
/// order of their paths; usermaps/ and mods/ are left out, not being the game's own.
pub fn game_iwds(folder: &Path) -> Vec<PathBuf> {
    // (relative path in lower case, path)
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    // A folder that can't be read ends the walk, with what it found so far.
    fn walk(folder: &Path, dir: &Path, found: &mut Vec<(String, PathBuf)>) -> std::result::Result<(), ()> {
        let entries = std::fs::read_dir(dir).map_err(|_| ())?;
        for entry in entries {
            let entry = entry.map_err(|_| ())?;
            let path = entry.path();
            let name = lower(&file_name(&path));
            let is_dir = std::fs::metadata(&path).is_ok_and(|m| m.is_dir());
            if is_dir {
                // (a link to a folder is not walked into)
                let link = entry.file_type().is_ok_and(|t| t.is_symlink());
                if name != "usermaps" && name != "mods" && !link {
                    walk(folder, &path, found)?;
                }
            } else if name.ends_with(".iwd") {
                let relative = path.strip_prefix(folder).unwrap_or(&path);
                let parts: Vec<String> = relative.iter().map(|c| utf8(Path::new(c))).collect();
                found.push((lower(&parts.join("/")), path));
            }
        }
        Ok(())
    }
    let _ = walk(folder, folder, &mut found);
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

// ---- the conversion -------------------------------------------------------------------------

const TEXTURE_BUDGET_MB: i32 = 40;
/// Pak table entries per picture.
const LEVELS: usize = 4;

fn base_of(p: &Path) -> String {
    lower(&p.file_stem().map(|s| utf8(Path::new(s))).unwrap_or_default())
}

fn stem_of(p: &Path) -> String {
    p.file_stem().map(|s| utf8(Path::new(s))).unwrap_or_default()
}

fn is_resident(base: &str) -> bool {
    base == "code_post_gfx_mp" || base == "common_mp"
}

/// The path without its extension.
fn without_extension(p: &Path) -> PathBuf {
    if p.extension().is_some() { p.with_extension("") } else { p.to_path_buf() }
}

/// The path with text added to its last part.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(p.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

/// An error of the file system, as the player is told.
fn fs_error(what: &str, e: &std::io::Error, paths: &[&Path]) -> Error {
    let reason = e.to_string();
    let reason = reason.split(" (os error").next().unwrap_or(&reason).to_string();
    let mut text = format!("filesystem error: {what}: {reason}");
    for p in paths {
        text += &format!(" [{}]", utf8(p));
    }
    Error::new(text)
}

fn read_text(path: &Path) -> String {
    text::of(&std::fs::read(path).unwrap_or_default())
}

// ---- reading text as the patterns do ----------------------------------------------------------

/// A character of a word (letters, digits, underscore).
fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// White space: space, tab, line feed, vertical tab, form feed, carriage return.
fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

fn skip_space(t: &[u8], mut i: usize) -> usize {
    while i < t.len() && space(t[i]) {
        i += 1;
    }
    i
}

fn at(t: &[u8], i: usize, b: u8) -> bool {
    t.get(i) == Some(&b)
}

/// Each `key value` pair of an arena file (a word, blanks, then a value up to a quote or
/// white space, in quotes or not).
fn arena_pairs(t: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < t.len() {
        let mut s = pos;
        let mut found = None;
        while s < t.len() {
            if !word(t[s]) {
                s += 1;
                continue;
            }
            let mut e = s;
            while e < t.len() && word(t[e]) {
                e += 1;
            }
            if at(t, e, b' ') || at(t, e, b'\t') {
                found = Some((s, e));
                break;
            }
            s = e;
        }
        let Some((s, e)) = found else { break };
        let mut i = e;
        while at(t, i, b' ') || at(t, i, b'\t') {
            i += 1;
        }
        if at(t, i, b'"') {
            i += 1;
        }
        let vs = i;
        while i < t.len() && t[i] != b'"' && !space(t[i]) {
            i += 1;
        }
        let ve = i;
        if at(t, i, b'"') {
            i += 1;
        }
        out.push((text::of(&t[s..e]), text::of(&t[vs..ve])));
        pos = i;
    }
    out
}

/// A `case "type": function("colour");` of a script at `s` (the colour may be left out):
/// its end, the type, the function and the colour.
fn case_at(t: &[u8], s: usize) -> Option<(usize, String, String, Option<String>)> {
    if !t[s..].starts_with(b"case") {
        return None;
    }
    let mut i = skip_space(t, s + 4);
    if i == s + 4 || !at(t, i, b'"') {
        return None;
    }
    i += 1;
    let ks = i;
    while i < t.len() && t[i] != b'"' {
        i += 1;
    }
    if i == ks || i >= t.len() {
        return None;
    }
    let key = text::of(&t[ks..i]);
    i = skip_space(t, i + 1);
    if !at(t, i, b':') {
        return None;
    }
    i = skip_space(t, i + 1);
    let fs = i;
    while i < t.len() && word(t[i]) {
        i += 1;
    }
    if i == fs {
        return None;
    }
    let function = text::of(&t[fs..i]);
    i = skip_space(t, i);
    if !at(t, i, b'(') {
        return None;
    }
    i = skip_space(t, i + 1);
    let mut arg = None;
    if at(t, i, b'"') {
        let a = i + 1;
        let mut k = a;
        while k < t.len() && t[k] != b'"' {
            k += 1;
        }
        if k >= t.len() {
            return None;
        }
        arg = Some(text::of(&t[a..k]));
        i = k + 1;
    }
    i = skip_space(t, i);
    if !at(t, i, b')') {
        return None;
    }
    i = skip_space(t, i + 1);
    if !at(t, i, b';') {
        return None;
    }
    Some((i + 1, key, function, arg))
}

/// The head of function `f` at the start of a line: `f(param)` and then `{` at the start
/// of a line. Where its body starts, and the parameter's name.
fn function_head(t: &[u8], f: &[u8]) -> Option<(usize, String)> {
    let try_at = |fs: usize| -> Option<(usize, String)> {
        if !t.get(fs..)?.starts_with(f) {
            return None;
        }
        let mut i = skip_space(t, fs + f.len());
        if !at(t, i, b'(') {
            return None;
        }
        i = skip_space(t, i + 1);
        let ps = i;
        while i < t.len() && word(t[i]) {
            i += 1;
        }
        let param = text::of(&t[ps..i]);
        i = skip_space(t, i);
        if !at(t, i, b')') {
            return None;
        }
        i += 1;
        let r = skip_space(t, i);
        if r > i && t[r - 1] == b'\n' && at(t, r, b'{') {
            return Some((r + 1, param));
        }
        None
    };
    for p in 0..=t.len() {
        if p == 0
            && let Some(hit) = try_at(0)
        {
            return Some(hit);
        }
        if at(t, p, b'\n')
            && let Some(hit) = try_at(p + 1)
        {
            return Some(hit);
        }
    }
    None
}

/// A part of a name a script builds at `i`: a string without line breaks, or the
/// parameter as a whole word.
fn part_at(t: &[u8], i: usize, param: &[u8]) -> Option<usize> {
    if at(t, i, b'"') {
        let mut j = i + 1;
        while j < t.len() && t[j] != b'"' && t[j] != b'\n' {
            j += 1;
        }
        return at(t, j, b'"').then_some(j + 1);
    }
    let boundary = |k: usize| {
        let before = k > 0 && word(t[k - 1]);
        let after = k < t.len() && word(t[k]);
        before != after
    };
    if !param.is_empty() && boundary(i) && t[i..].starts_with(param) && boundary(i + param.len()) {
        return Some(i + param.len());
    }
    None
}

/// Each name a function's body builds: parts joined with `+`, at least one a string;
/// as the pieces, in order.
fn built_names(body: &[u8], param: &[u8]) -> Vec<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        let mut s = pos;
        let mut hit = None;
        while s < body.len() {
            if let Some(e) = part_at(body, s, param) {
                hit = Some((s, e));
                break;
            }
            s += 1;
        }
        let Some((s, mut e)) = hit else { break };
        let mut pieces = vec![(s, e)];
        loop {
            let j = skip_space(body, e);
            if !at(body, j, b'+') {
                break;
            }
            let k = skip_space(body, j + 1);
            match part_at(body, k, param) {
                Some(e2) => {
                    pieces.push((k, e2));
                    e = e2;
                }
                None => break,
            }
        }
        out.push(pieces);
        pos = e;
    }
    out
}

// ---- the 360's destructible script ------------------------------------------------------------

type DestructibleFound = BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)>;

/// {destructible type: (names its 360 script function uses, colours that function is
/// called with)} for the types wanted, read from the 360's
/// common_scripts/_destructible_types.gsc: the function each type's case calls, with the
/// colour it's given put into the names it builds.
fn destructible_names(script: &str, wanted: &BTreeSet<String>) -> DestructibleFound {
    let t = text::bytes(script);
    let mut cases: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let mut colours: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut s = 0;
    while s < t.len() {
        match case_at(&t, s) {
            Some((end, key, function, arg)) => {
                cases.entry(lower(&key)).or_insert((function, arg));
                s = end;
            }
            None => s += 1,
        }
    }
    for (f, arg) in cases.values() {
        if let Some(arg) = arg {
            colours.entry(f.clone()).or_default().insert(arg.clone());
        }
    }
    let mut out = BTreeMap::new();
    for ty in wanted {
        let Some((f, arg)) = cases.get(&lower(ty)) else { continue };
        let Some((from, param)) = function_head(&t, &text::bytes(f)) else { continue };
        let rest = &t[from..];
        let body = match rest.windows(2).position(|w| w == b"\n}") {
            Some(to) => &rest[..to],
            None => rest,
        };
        let param = text::bytes(&param);
        let mut names = BTreeSet::new();
        for pieces in built_names(body, &param) {
            if !pieces.iter().any(|&(a, _)| body[a] == b'"') {
                continue;
            }
            let mut name = String::new();
            for (a, b) in pieces {
                if body[a] == b'"' {
                    name += &text::of(&body[a + 1..b - 1]);
                } else {
                    name += arg.as_deref().unwrap_or("");
                }
            }
            names.insert(name);
        }
        out.insert(ty.clone(), (names, colours.get(f).cloned().unwrap_or_default()));
    }
    out
}

/// The entity string of a tree ("" without one).
fn entity_text(st: &mut Store, t: &Tree) -> Result<String> {
    let mut out = String::new();
    let mut found = false;
    for_each(st, Roots::Entries(&t.assets), |st, n| {
        if found || !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("MapEnts")) {
            return Ok(());
        }
        let mut c = st.child(n, "entityString");
        if let Some(r) = st.as_ref(c) {
            c = r.target;
        }
        if let Some(lf) = st.as_leaf(c) {
            found = true;
            let end = lf.raw.iter().position(|&b| b == 0).unwrap_or(lf.raw.len());
            out = text::of(&lf.raw[..end]);
        }
        Ok(())
    })?;
    Ok(out)
}

fn destructible_types(st: &mut Store, pc: &Tree) -> Result<BTreeSet<String>> {
    let t = text::bytes(&entity_text(st, pc)?);
    const KEY: &[u8] = b"\"destructible_type\"";
    let mut out = BTreeSet::new();
    let mut s = 0;
    while s < t.len() {
        let hit = (|| {
            if !t[s..].starts_with(KEY) {
                return None;
            }
            let i = skip_space(&t, s + KEY.len());
            if i == s + KEY.len() || !at(&t, i, b'"') {
                return None;
            }
            let vs = i + 1;
            let mut e = vs;
            while e < t.len() && t[e] != b'"' {
                e += 1;
            }
            (e > vs && e < t.len()).then(|| (e + 1, text::of(&t[vs..e])))
        })();
        match hit {
            Some((end, ty)) => {
                out.insert(ty);
                s = end;
            }
            None => s += 1,
        }
    }
    Ok(out)
}

/// The teams the map wants: given, from its .arena, or the game's defaults for a map it
/// doesn't know.
fn map_teams(arena: &Option<PathBuf>, teams: &Option<Teams>) -> Teams {
    if let Some(teams) = teams {
        return teams.clone();
    }
    if let Some(arena) = arena
        && arena.exists()
    {
        let mut kv: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in arena_pairs(&text::bytes(&read_text(arena))) {
            kv.insert(k, v);
        }
        if let (Some(allies), Some(axis)) = (kv.get("allieschar"), kv.get("axischar")) {
            return Teams { allies: allies.clone(), axis: axis.clone() };
        }
    }
    Teams { allies: "us_army".to_string(), axis: "opforce_composite".to_string() }
}

fn asset_named(st: &mut Store, t: &Tree, ty: &str, name: &str) -> Option<NodeId> {
    for &e in &t.assets {
        let (ety, a) = {
            let e = st.entry(e);
            (e.ty.clone(), e.asset)
        };
        if let Some(a) = a.filter(|&a| st.is_struct(Some(a)))
            && ety == ty
            && name_of(st, a).as_deref() == Some(name)
        {
            return Some(a);
        }
    }
    None
}

/// The asset of each asset list entry that is a struct, with the entry's type.
fn entry_assets(st: &Store, t: &Tree) -> Vec<(String, NodeId)> {
    t.assets
        .iter()
        .filter_map(|&e| {
            let e = st.entry(e);
            e.asset.filter(|&a| st.is_struct(Some(a))).map(|a| (e.ty.clone(), a))
        })
        .collect()
}

// ---- stock files ----------------------------------------------------------------------------

/// What a stock file holds that a conversion looks things up in without taking anything
/// from it, worked out from the file as it is.
#[derive(Default)]
struct StockScan {
    /// Every name in its zone (text between two NULs).
    names: BTreeSet<String>,
    /// Its pools' names, lower case.
    techsets: BTreeSet<String>,
    models: BTreeSet<String>,
    aliases: BTreeSet<String>,
    /// The pictures it streams from the console's picture packs (a header and where its
    /// levels are, no pixels), by lower-case name.
    streamed: Vec<(String, NodeId)>,
}

/// What a stock file is looked through for, from the file as it is: its streamed pictures
/// still in the Store it was read into.
#[derive(Default)]
struct Survey {
    scan: StockScan,
    streamed: Vec<(String, NodeId)>,
}

/// A stock file read on a thread of its own, into a Store of its own: one the conversion
/// takes from is kept, one only looked through goes once what it was looked through for
/// is copied out.
struct StockRead {
    store: Store,
    tree: Tree,
    survey: Option<Survey>,
}

/// A stock 360 file's tree, each picture whose pixels are in the disc's picture packs
/// marked with its pak entries. The always-loaded files are only looked things up in:
/// animation data, which can't be read back out of the tree, may be left rough in
/// common_mp.
fn stock_tree(st: &mut Store, p: &Path, ff: &Fastfile) -> Result<Tree> {
    let (tree, mut starts) =
        reader::read_tree(st, rules::xbox()?, reader::xbox(), ff.zone.clone(), base_of(p) == "common_mp")?;
    // The pak table lists the levels of the pictures streamed from the disc's picture
    // packs (`streaming` set), four entries a picture, in the order the zone has them.
    starts.sort_by_key(|&(_, start)| start);
    let mut k = 0;
    for (d, _) in starts {
        if !st.as_struct(Some(d)).is_some_and(|s| s.is_asset_named("GfxImage")) || st.int(d, "streaming")? == 0 {
            continue;
        }
        let entries: Vec<PakEntry> = ff.table.iter().skip(k * LEVELS).take(LEVELS).copied().collect();
        st.st_mut(d).pak = Some(Arc::new(entries));
        k += 1;
    }
    Ok(tree)
}

fn look(st: &mut Store, z: &[u8], tree: &Tree) -> Result<Survey> {
    let mut out = Survey::default();
    let scan = &mut out.scan;
    let mut at = 0;
    while at < z.len() {
        if z[at] == 0 {
            let mut end = at + 1;
            while end < z.len() && (0x20..=0x7E).contains(&z[end]) {
                end += 1;
            }
            if end < z.len() && z[end] == 0 && (1..=255).contains(&(end - at - 1)) {
                scan.names.insert(text::of(&z[at + 1..end]));
            }
            at = end;
        } else {
            at += 1;
        }
    }
    let mut pools = pool_names(st, &tree.assets)?;
    scan.techsets = pools.remove("MaterialTechniqueSet").unwrap_or_default();
    scan.models = pools.remove("XModel").unwrap_or_default();
    scan.aliases = pools.remove("snd_alias_list_t").unwrap_or_default();
    let mut seen = BTreeSet::new();
    let streamed = &mut out.streamed;
    for_each(st, Roots::Entries(&tree.assets), |st, n| {
        if !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("GfxImage"))
            || st.int(n, "streaming")? != 1
            || st.ptr(n, "pixels")? != 0
        {
            return Ok(());
        }
        let name = asset_name(st, n).unwrap_or_default();
        if name.is_empty() || name.starts_with([',', '*', '$']) || !seen.insert(lower(&name)) {
            return Ok(());
        }
        streamed.push((lower(&name), n));
        Ok(())
    })?;
    Ok(out)
}

fn read_stock(p: &Path, look_through: bool) -> Result<StockRead> {
    let mut store = Store::new();
    let read = read_fastfile(p).and_then(|ff| Ok((stock_tree(&mut store, p, &ff)?, ff)));
    let (tree, ff) = match read {
        Ok(x) => x,
        Err(e) if e.is_error() => {
            if !look_through {
                return Err(e);
            }
            // A file only looked through that can't be read offers nothing.
            return Ok(StockRead { store: Store::new(), tree: Tree::default(), survey: Some(Survey::default()) });
        }
        Err(e) => return Err(e),
    };
    let survey = if look_through { Some(look(&mut store, &ff.zone, &tree)?) } else { None };
    Ok(StockRead { store, tree, survey })
}

/// A donor stock map read only for its teams or effects: its drawn geometry and collision
/// are never used. The named assets inside them stay, in their place and order, so every
/// look-up by name finds what it did.
fn drop_world(st: &mut Store, tree: &mut Tree) -> Result<()> {
    for k in 0..tree.assets.len() {
        let (ty, world) = {
            let e = st.entry(tree.assets[k]);
            (e.ty.clone(), e.asset)
        };
        let Some(world) = world.filter(|&w| st.is_struct(Some(w))) else { continue };
        if ty != "gfx_map" && ty != "col_map_mp" {
            continue;
        }
        let mut items = Vec::new();
        for_each(st, Roots::One(Some(world)), |st, n| {
            if n != world && st.as_struct(Some(n)).is_some_and(|o| o.is_asset()) {
                items.push(Some(n));
            }
            Ok(())
        })?;
        items.reverse(); // (the walk takes a list's last item first)
        let inner = st.make_array(items);
        tree.assets[k] = st.make_entry("world_parts", Some(inner));
    }
    Ok(())
}

/// How many stock files are read at once: each takes a few hundred MB while it is read.
fn workers() -> usize {
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    (threads / 2).clamp(1, 4)
}

/// Stock maps to read as well for the map's effects the files read already don't have:
/// at most two, those carrying the most of them.
fn greedy(mut want: BTreeSet<String>, hits: &[(PathBuf, BTreeSet<String>)]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let count = |want: &BTreeSet<String>, h: &BTreeSet<String>| h.iter().filter(|s| want.contains(*s)).count();
    while out.len() < 2 && !hits.is_empty() {
        let mut best: Option<&(PathBuf, BTreeSet<String>)> = None;
        let mut score = (0usize, false);
        for h in hits {
            let sc = (count(&want, &h.1), !out.contains(&h.0));
            if best.is_none() || sc > score {
                best = Some(h);
                score = sc;
            }
        }
        let Some(best) = best else { break };
        if out.contains(&best.0) || count(&want, &best.1) == 0 {
            break;
        }
        out.push(best.0.clone());
        for s in &best.1 {
            want.remove(s);
        }
    }
    out
}

/// The menus' picture of the map, made of its loading screen.
struct Picture {
    map: String,
    file: PathBuf,
}

struct Conversion {
    pc: PathBuf,
    out: PathBuf,
    arena: Option<PathBuf>,
    refs: Vec<PathBuf>,
    fx: Vec<PathBuf>,
    donors: BTreeSet<PathBuf>,
    teams: Option<Teams>,
    texture_budget: i32,
    preview: Option<Picture>,
}

struct MapPort {
    log: Log,
    /// The stock files by lower-case file name.
    by_name: BTreeMap<String, PathBuf>,
    iwd: Option<PathBuf>,
    game_iwds: Vec<Rc<Zip>>,
    /// The PC files' trees, the stock files kept and what the conversion makes.
    st: Store,
    loaded: BTreeMap<PathBuf, Rc<StockFile>>,
    scans: BTreeMap<PathBuf, StockScan>,
    files_read: usize,
    ported: Ported,
}

impl MapPort {
    fn new(stock: &[PathBuf], game: &[PathBuf], log: &Log) -> Result<MapPort> {
        let mut by_name = BTreeMap::new();
        for p in stock {
            by_name.insert(lower(&file_name(p)), p.clone());
        }
        let mut game_iwds = Vec::new();
        for p in game {
            game_iwds.push(Rc::new(Zip::open(p)?));
        }
        Ok(MapPort {
            log: log.clone(),
            by_name,
            iwd: None,
            game_iwds,
            st: Store::new(),
            loaded: BTreeMap::new(),
            scans: BTreeMap::new(),
            files_read: 0,
            ported: Ported::default(),
        })
    }

    fn say(&self, line: &str) -> Result<()> {
        (self.log)(line)
    }

    /// Reads the stock files given, several at a time: to keep (the files the conversion
    /// takes from), or only to look through. Each is looked through once, when it is
    /// first read, before anything changes it.
    fn load(&mut self, paths: &[PathBuf], keep: bool, donors: &BTreeSet<PathBuf>) -> Result<()> {
        let mut todo: Vec<PathBuf> = Vec::new();
        for p in paths {
            let have = if keep { self.loaded.contains_key(p) } else { self.scans.contains_key(p) };
            if !have && !todo.contains(p) {
                todo.push(p.clone());
            }
        }
        let mut at = 0;
        while at < todo.len() {
            let chunk = &todo[at..todo.len().min(at + workers())];
            let looks: Vec<bool> = chunk.iter().map(|p| !self.scans.contains_key(p)).collect();
            std::thread::scope(|s| -> Result<()> {
                let reads: Vec<_> = chunk.iter().zip(&looks).map(|(p, &l)| s.spawn(move || read_stock(p, l))).collect();
                for (p, handle) in chunk.iter().zip(reads) {
                    self.say(&format!(
                        "{}{}",
                        if keep { "reading stock 360 file " } else { "looking through stock 360 file " },
                        file_name(p)
                    ))?;
                    let read = handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
                    self.collect(p, read, keep, donors)?;
                }
                Ok(())
            })?;
            at += chunk.len();
        }
        Ok(())
    }

    // A stock file read, taken in: what it was looked through for, and the file itself
    // when it is kept.
    fn collect(&mut self, p: &Path, read: StockRead, keep: bool, donors: &BTreeSet<PathBuf>) -> Result<()> {
        let StockRead { mut store, mut tree, survey } = read;
        if let Some(survey) = survey {
            let mut scan = survey.scan;
            // (a picture's header, its name and pak entries: nothing points back)
            for (name, image) in survey.streamed {
                let copy = self.st.import(&store, image)?;
                let s = self.st.st_mut(copy);
                s.slot = None;
                s.forward = false;
                scan.streamed.push((name, copy));
            }
            self.scans.insert(p.to_path_buf(), scan);
        }
        if !keep {
            return Ok(());
        }
        if donors.contains(p) {
            drop_world(&mut store, &mut tree)?;
        }
        let by = self.st.absorb(store);
        let base = base_of(p);
        let file = StockFile {
            id: self.files_read,
            path: p.to_path_buf(),
            resident: is_resident(&base),
            base,
            tree: tree.shifted(by),
        };
        self.files_read += 1;
        self.loaded.insert(p.to_path_buf(), Rc::new(file));
        Ok(())
    }

    fn read(&mut self, p: &Path, donors: &BTreeSet<PathBuf>) -> Result<Rc<StockFile>> {
        self.load(&[p.to_path_buf()], true, donors)?;
        self.loaded.get(p).cloned().ok_or_else(|| Error::new(format!("{} was not read", utf8(p))))
    }

    fn scan(&mut self, p: &Path) -> Result<&StockScan> {
        self.load(&[p.to_path_buf()], false, &BTreeSet::new())?;
        self.scans.get(p).ok_or_else(|| Error::new(format!("{} was not looked through", utf8(p))))
    }

    fn effect_donors(&mut self, pc: &Tree, refs: &[PathBuf], candidates: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut have = BTreeSet::new();
        let mut want = BTreeSet::new();
        for p in refs {
            if is_resident(&base_of(p)) {
                continue;
            }
            let file = self.loaded.get(p).cloned().ok_or_else(|| Error::new(format!("{} was not read", utf8(p))))?;
            for (ty, a) in entry_assets(&self.st, &file.tree) {
                if ty == "fx"
                    && let Some(n) = name_of(&mut self.st, a)
                    && !n.is_empty()
                    && !n.starts_with(',')
                {
                    have.insert(n);
                }
            }
        }
        for (ty, a) in entry_assets(&self.st, pc) {
            if ty == "fx"
                && let Some(n) = name_of(&mut self.st, a)
                && !have.contains(&n)
            {
                want.insert(n);
            }
        }
        if want.is_empty() || candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        for p in candidates {
            let s = self.scan(p)?;
            let h: BTreeSet<String> = want.iter().filter(|n| s.names.contains(*n)).cloned().collect();
            hits.push((p.clone(), h));
        }
        Ok(greedy(want, &hits))
    }

    /// Stock maps to read as well for shader sets the map's materials use (and the
    /// composite sets its decals could become) that the files read already lack: at most
    /// two, those carrying the most of them.
    fn techset_donors(&mut self, pc: &Tree, refs: &[PathBuf], candidates: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut names = BTreeSet::new();
        for_each(&mut self.st, Roots::Entries(&pc.assets), |st, n| {
            if !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("MaterialTechniqueSet")) {
                return Ok(());
            }
            let own = asset_name(st, n);
            let shared = name_of(st, n);
            let name = without_commas(&own.or(shared).unwrap_or_default());
            if !name.is_empty() {
                names.insert(name);
            }
            Ok(())
        })?;
        let mut want = names.clone();
        for n in &names {
            // (PC-only "high" shadow sets)
            if let Some(at) = n.find("_hsm_") {
                want.insert(format!("{}_sm_{}", &n[..at], &n[at + 5..]));
            }
        }
        let mut grounds = BTreeSet::new();
        let mut first = BTreeSet::new();
        let mut second = BTreeSet::new();
        for n in &names {
            if let Some(g) = ground_code(n) {
                grounds.insert((g.code, g.p0));
            }
            if let Some(a) = layer_code(n, 1) {
                first.insert((a.code, a.p0));
            }
            if let Some(b) = layer_code(n, 2) {
                second.insert((b.code, b.p0));
            }
        }
        for g in &grounds {
            for a in &first {
                // (composite sets are named without wc_: l_sm_r0c0n0_b1c1n1s1p0)
                want.insert(format!("{}_{}{}", g.0, a.0, if g.1 || a.1 { "p0" } else { "" }));
                for b in &second {
                    want.insert(format!("{}_{}_{}{}", g.0, a.0, b.0, if g.1 || a.1 || b.1 { "p0" } else { "" }));
                }
            }
        }
        let mut have = BTreeSet::new();
        for p in refs {
            have.extend(self.scan(p)?.techsets.iter().cloned());
        }
        let missing: BTreeSet<String> = want.iter().map(|n| lower(n)).filter(|n| !have.contains(n)).collect();
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        for p in candidates {
            let h: BTreeSet<String> = self.scan(p)?.techsets.iter().filter(|n| missing.contains(*n)).cloned().collect();
            hits.push((p.clone(), h));
        }
        let out = greedy(missing, &hits);
        if !out.is_empty() {
            let files: Vec<String> = out.iter().map(|p| file_name(p)).collect();
            self.say(&format!(
                "  shader sets the files given lack: reading {} as well",
                joined(&files, usize::MAX, "")
            ))?;
        }
        Ok(out)
    }

    fn destructible_script(&mut self, refs: &[PathBuf], donors: &BTreeSet<PathBuf>) -> Result<Option<String>> {
        let mut script = None;
        for p in refs {
            if !is_resident(&base_of(p)) {
                continue;
            }
            let file = self.read(p, donors)?;
            for (ty, a) in entry_assets(&self.st, &file.tree) {
                if ty == "rawfile"
                    && lower(&name_of(&mut self.st, a).unwrap_or_default()) == "common_scripts/_destructible_types.gsc"
                {
                    script = Some(rawfile_text(&mut self.st, a)?);
                }
            }
        }
        Ok(script)
    }

    /// The first of `files` whose look-through `has`, looking through each as far as that.
    fn first_having(&mut self, files: &[PathBuf], has: impl Fn(&StockScan) -> bool) -> Result<Option<PathBuf>> {
        for s in files {
            if has(self.scan(s)?) {
                return Ok(Some(s.clone()));
            }
        }
        Ok(None)
    }

    /// The names of `kind` (a pool) the map and the always-loaded files among `refs` have.
    fn resident_names(
        &mut self,
        pc: &Tree,
        refs: &[PathBuf],
        kind: &str,
        of: impl Fn(&StockScan) -> &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        let mut have = pool_names(&mut self.st, &pc.assets)?.remove(kind).unwrap_or_default();
        for p in refs {
            if is_resident(&base_of(p)) {
                have.extend(of(self.scan(p)?).iter().cloned());
            }
        }
        Ok(have)
    }

    /// The models the map's destructible entities need that neither the map nor the
    /// always-loaded files have: a model no stock file has is taken in another colour the
    /// script knows, its paint then the map's own.
    fn destructible_donors(
        &mut self,
        pc: &Tree,
        refs: &[PathBuf],
        stock: &[PathBuf],
        donors: &BTreeSet<PathBuf>,
    ) -> Result<Vec<Breakable>> {
        let have = self.resident_names(pc, refs, "XModel", |s| &s.models)?;
        let Some(script) = self.destructible_script(refs, donors)? else { return Ok(Vec::new()) };
        let types = destructible_types(&mut self.st, pc)?;
        if types.is_empty() {
            return Ok(Vec::new());
        }
        let mut plan = Vec::new();
        let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (t, (names, colours)) in destructible_names(&script, &types) {
            let ty = lower(&t);
            let own = colours.iter().find(|c| ty.ends_with(&format!("_{}", lower(c)))).cloned();
            let stem = match &own {
                Some(_) => ty[..ty.rfind('_').unwrap_or(ty.len())].to_string(),
                None => ty.clone(),
            };
            for n in &names {
                let low = lower(n);
                if low == ty || low.contains('/') || low.contains(' ') || !low.starts_with(&stem) || have.contains(&low)
                {
                    continue;
                }
                // The stock model to try, and the colour it is in when it is another's.
                let mut tries: Vec<(String, Option<ColourSwap>)> = vec![(low.clone(), None)];
                if let Some(own) = &own {
                    for c in &colours {
                        if c == own {
                            continue;
                        }
                        let from = format!("_{}_", lower(own));
                        let src = match low.find(&from) {
                            Some(at) => format!("{}_{}_{}", &low[..at], lower(c), &low[at + from.len()..]),
                            None => low.clone(),
                        };
                        tries.push((src, Some(ColourSwap { stock: lower(c), map: lower(own) })));
                    }
                }
                let mut hit = false;
                for (src, swap) in tries {
                    let p = self.first_having(stock, |s| s.models.contains(&src))?;
                    if let Some(p) = p
                        && (swap.is_none() || src != low)
                    {
                        plan.push(Breakable { need: low.clone(), src, path: p, swap });
                        hit = true;
                        break;
                    }
                }
                if !hit
                    && let Some(own) = &own
                    && low.contains(&format!("_{}_", lower(own)))
                {
                    missing.entry(t.clone()).or_default().push(low);
                }
            }
        }
        if !plan.is_empty() {
            let files: BTreeSet<String> = plan.iter().map(|b: &Breakable| file_name(&b.path)).collect();
            let files: Vec<String> = files.into_iter().collect();
            self.say(&format!(
                "  breakable parts the map lacks: {} models from {}",
                plan.len(),
                joined(&files, usize::MAX, "")
            ))?;
        }
        for (t, names) in &missing {
            let some: Vec<String> = names.iter().take(6).cloned().collect();
            self.say(&format!(
                "  note: no stock map has {t}'s {}{}",
                joined(&some, usize::MAX, ""),
                if names.len() > 6 { " ..." } else { "" }
            ))?;
        }
        Ok(plan)
    }

    /// The sounds the 360's destructible script plays for the map's destructible entities
    /// that the map and the always-loaded files lack, and a stock file that has each (one
    /// already read first).
    fn destructible_sound_donors(
        &mut self,
        pc: &Tree,
        refs: &[PathBuf],
        stock: &[PathBuf],
        donors: &BTreeSet<PathBuf>,
    ) -> Result<Vec<(String, PathBuf)>> {
        let have = self.resident_names(pc, refs, "snd_alias_list_t", |s| &s.aliases)?;
        let Some(script) = self.destructible_script(refs, donors)? else { return Ok(Vec::new()) };
        let types = destructible_types(&mut self.st, pc)?;
        if types.is_empty() {
            return Ok(Vec::new());
        }
        let mut order: Vec<PathBuf> = stock.iter().filter(|p| refs.contains(p)).cloned().collect();
        order.extend(stock.iter().filter(|p| !refs.contains(p)).cloned());
        let mut plan: Vec<(String, PathBuf)> = Vec::new();
        let mut seen = BTreeSet::new();
        for (_, (names, _)) in destructible_names(&script, &types) {
            for n in &names {
                let low = lower(n);
                if have.contains(&low) || seen.contains(&low) {
                    continue;
                }
                let Some(p) = self.first_having(&order, |s| s.aliases.contains(&low))? else { continue };
                plan.push((low.clone(), p));
                seen.insert(low);
            }
        }
        if !plan.is_empty() {
            let files: BTreeSet<String> = plan.iter().map(|(_, p)| file_name(p)).collect();
            let files: Vec<String> = files.into_iter().collect();
            self.say(&format!(
                "  destructible sounds the map lacks: {} from {}",
                plan.len(),
                joined(&files, usize::MAX, "")
            ))?;
        }
        Ok(plan)
    }

    fn port(&mut self, mut c: Conversion) -> Result<()> {
        let ff = read_fastfile(&c.pc)?;
        if !ff.pc {
            return Err(port_error(format!("{} is not a PC fastfile", file_name(&c.pc))));
        }
        let (mut pc, _) = reader::read_tree(&mut self.st, rules::pc()?, reader::pc(), ff.zone, false)?;
        let mut refs = c.refs.clone();
        let add = |refs: &mut Vec<PathBuf>, donors: &mut BTreeSet<PathBuf>, extra: Vec<PathBuf>| {
            for p in extra {
                refs.push(p.clone());
                donors.insert(p);
            }
        };
        let outside = |refs: &[PathBuf], list: &[PathBuf]| -> Vec<PathBuf> {
            list.iter().filter(|p| !refs.contains(p)).cloned().collect()
        };
        let mut others = outside(&refs, &c.fx);
        self.load(&refs, true, &c.donors)?;
        if !c.fx.is_empty() {
            self.load(&c.fx, false, &BTreeSet::new())?;
            let extra = self.effect_donors(&pc, &refs, &others)?;
            add(&mut refs, &mut c.donors, extra);
            others = outside(&refs, &c.fx);
            let extra = self.techset_donors(&pc, &refs, &others)?;
            add(&mut refs, &mut c.donors, extra);
        }
        let mut breakables = Vec::new();
        let mut sounds = Vec::new();
        if !c.fx.is_empty() {
            breakables = self.destructible_donors(&pc, &refs, &c.fx, &c.donors)?;
            let extra: BTreeSet<PathBuf> =
                breakables.iter().filter(|b| !refs.contains(&b.path)).map(|b| b.path.clone()).collect();
            add(&mut refs, &mut c.donors, extra.into_iter().collect());
            sounds = self.destructible_sound_donors(&pc, &refs, &c.fx, &c.donors)?;
            let extra: BTreeSet<PathBuf> =
                sounds.iter().filter(|(_, p)| !refs.contains(p)).map(|(_, p)| p.clone()).collect();
            add(&mut refs, &mut c.donors, extra.into_iter().collect());
        }
        let mut files = Vec::new();
        for p in &refs {
            files.push(self.read(p, &c.donors)?);
        }
        let mut iwds = Vec::new();
        if let Some(iwd) = &self.iwd {
            iwds.push(Rc::new(Zip::open(iwd)?));
        }
        self.say(&format!("converting {}", file_name(&c.pc)))?;
        // (every file here was looked through when it was read)
        let mut streamed_from = refs.clone();
        streamed_from.extend(outside(&refs, &c.fx));
        let mut streamed = Vec::new();
        for p in &streamed_from {
            streamed.extend(self.scan(p)?.streamed.iter().cloned());
        }
        let log = self.log.clone();
        let mut porter =
            Porter::new(&mut self.st, &mut pc, files.clone(), iwds, &self.game_iwds, log.clone(), c.texture_budget)?;
        porter.breakables = breakables;
        for (n, _) in &sounds {
            porter.destructible_aliases.push(n.clone());
        }
        for (name, image) in streamed {
            porter.know_streamed(name, image);
        }
        porter.convert()?;
        if !porter.map_name.is_empty() {
            porter.add_teams(&map_teams(&c.arena, &c.teams))?;
        }
        drop_pc_tables(porter.st, porter.root, &log)?;
        check_pools(&mut porter, &files, &log)?;
        let warnings = std::mem::take(&mut porter.warnings);
        drop(porter);
        self.write(&pc, &c.out)?;
        for note in warnings {
            if !self.ported.warnings.contains(&note) {
                self.ported.warnings.push(note);
            }
        }
        // Without its picture the map still works: the menus show black.
        if let Some(picture) = &c.preview {
            let made = (|| -> Result<()> {
                if preview(&mut self.st, &mut pc, &picture.map)? {
                    self.write(&pc, &picture.file)
                } else {
                    self.say("  note: the loading screen has no picture, so the menus show none for the map")
                }
            })();
            match made {
                Err(e) if e.is_error() => {
                    self.say(&format!("  note: the menus' picture of the map couldn't be made, so they show none: {e}"))?
                }
                Err(e) => return Err(e),
                Ok(()) => {}
            }
        }
        Ok(())
    }

    /// Writes a converted tree as an unsigned 360 fastfile.
    fn write(&mut self, t: &Tree, to: &Path) -> Result<()> {
        let x = rules::xbox()?;
        let rel = move |r: &Ref| map_rel(r, x);
        let options = WriteOptions { map_rel: Some(&rel), dedupe: true, keep_bytes: false };
        let w = reader::write_tree(&mut self.st, t, x, reader::xbox(), &options, None)?;
        let mut zone = w.zone;
        reserve_callback_block(&mut self.st, &mut zone, t)?;
        let file = xbox_fastfile(&zone, &pak_table(&mut self.st, w.written)?);
        let parent = to.parent().unwrap_or(Path::new(""));
        std::fs::create_dir_all(parent).map_err(|e| fs_error("cannot create directories", &e, &[parent]))?;
        let part = with_suffix(to, ".part");
        std::fs::write(&part, &file).map_err(|_| Error::new(format!("cannot write {}", utf8(&part))))?;
        std::fs::rename(&part, to).map_err(|e| fs_error("cannot rename", &e, &[&part, to]))?;
        self.say(&format!("wrote {} ({} bytes of zone)", file_name(to), zone.len()))?;
        if w.deduped != 0 {
            self.say(&format!(
                "  {} places point at an asset written earlier instead of holding a copy of it",
                w.deduped
            ))?;
        }
        self.ported.written.push(to.to_path_buf());
        Ok(())
    }

    fn run(mut self, map: &PcMap, out_dir: &Path, teams: Option<Teams>) -> Result<Ported> {
        let stock = |by_name: &BTreeMap<String, PathBuf>, name: &str| by_name.get(name).cloned();
        let Some(cpg) = stock(&self.by_name, "code_post_gfx_mp.ff") else {
            return Err(port_error("code_post_gfx_mp.ff (from the console) is needed next to the stock maps"));
        };
        let mut maps = Vec::new();
        let mut loads = Vec::new();
        for (n, p) in &self.by_name {
            if n.starts_with("mp_") && !n.ends_with("_load.ff") && !n.ends_with("_preview.ff") {
                maps.push(p.clone());
            }
            if n.ends_with("_load.ff") {
                loads.push(p.clone());
            }
        }
        maps.sort();
        loads.sort();
        let Some(first_map) = maps.first() else {
            return Err(port_error("at least one stock 360 map (for example mp_favela.ff) is needed"));
        };
        let tpl = stock(&self.by_name, "mp_favela.ff").unwrap_or_else(|| first_map.clone());
        let name = stem_of(&map.ff);
        self.iwd = map.iwd.clone();
        std::fs::create_dir_all(out_dir).map_err(|e| fs_error("cannot create directories", &e, &[out_dir]))?;
        let cpg_file = self.read(&cpg, &BTreeSet::new())?;
        let common = stock(&self.by_name, "common_mp.ff");
        if common.is_none() {
            self.say(
                "  note: common_mp.ff isn't next to the stock maps, so the materials only it carries (heat \
                 distortion, ...) can't be used",
            )?;
        }
        let arenas = match asset_named(&mut self.st, &cpg_file.tree, "rawfile", "mp/basemaps.arena") {
            Some(a) => rawfile_text(&mut self.st, a)?,
            None => String::new(),
        };
        let mut want = map_teams(&map.arena, &teams);
        // A PC copy of a stock map: the stock 360 map of that name comes first for what it
        // has, and its teams are the 360's for the map.
        let same = stock(&self.by_name, &format!("{}.ff", lower(&name)));
        if teams.is_none()
            && map.arena.is_none()
            && let Some(t) = arena_teams(&arenas, &lower(&name))
        {
            want = t;
        }
        let mut refs = vec![cpg.clone()];
        if let Some(common) = &common {
            refs.push(common.clone());
        }
        if let Some(same) = &same
            && *same != tpl
        {
            refs.push(same.clone());
        }
        refs.push(tpl.clone());
        let mut donors = BTreeSet::new();
        for team in [&want.allies, &want.axis] {
            let carriers = team_maps(&arenas, &lower(team));
            let donor = maps.iter().find(|p| carriers.contains(&base_of(p)));
            if let Some(donor) = donor
                && !refs.contains(donor)
            {
                refs.push(donor.clone());
                donors.insert(donor.clone());
            }
        }
        if map.load.is_some() {
            // The menus' picture is named after the .arena, as the map is installed.
            let shown = map.arena.as_ref().map_or_else(|| name.clone(), |a| stem_of(a));
            let tpl_load = with_suffix(&without_extension(&tpl), "_load.ff");
            let load_ref = if loads.contains(&tpl_load) { Some(tpl_load) } else { loads.first().cloned() };
            match load_ref {
                None => self.say("  note: no stock *_load.ff given; the loading screen file is left out")?,
                Some(load_ref) => {
                    let done = self.port(Conversion {
                        pc: map.load.clone().unwrap_or_default(),
                        out: out_dir.join(path_of(&format!("{name}_load.ff"))),
                        arena: None,
                        refs: vec![cpg.clone(), tpl.clone(), load_ref],
                        fx: Vec::new(),
                        donors: BTreeSet::new(),
                        teams: None,
                        texture_budget: 0,
                        preview: Some(Picture {
                            map: shown,
                            file: out_dir.join(path_of(&format!("{name}_preview.ff"))),
                        }),
                    });
                    match done {
                        Ok(()) => {}
                        // The map works without it: the game shows a plain loading screen.
                        Err(e) if e.is_error() => self.say(&format!(
                            "  note: {name}_load.ff couldn't be converted, so it's left out (the map still works, \
                             with a plain loading screen): {e}"
                        ))?,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        self.port(Conversion {
            pc: map.ff.clone(),
            out: out_dir.join(path_of(&format!("{name}.ff"))),
            arena: map.arena.clone(),
            refs,
            fx: maps,
            donors,
            teams: Some(want),
            texture_budget: TEXTURE_BUDGET_MB,
            preview: None,
        })?;
        Ok(self.ported)
    }
}

/// The PC's config string tables go: the 360 game only ever looks for
/// configStrings_360_<map>_<mode>.csv.
fn drop_pc_tables(st: &mut Store, t: &mut Tree, log: &Log) -> Result<()> {
    let before = t.assets.len();
    t.assets.retain(|&e| {
        let (ty, a) = {
            let e = st.entry(e);
            (e.ty.clone(), e.asset)
        };
        let Some(a) = a.filter(|&a| st.is_struct(Some(a))) else { return true };
        if ty != "stringtable" {
            return true;
        }
        let n = without_commas(&lower(&name_of(st, a).unwrap_or_default()));
        !n.starts_with("mp/configstrings/configstrings_pc_")
    });
    let gone = before - t.assets.len();
    if gone != 0 {
        log(&format!("  {gone} PC config string tables left out (the 360 never reads them)"))?;
    }
    Ok(())
}

/// Stops when the map and the files loaded with it hold more assets of a kind than the
/// game has room for; warns when it is close. Each full copy of an asset those files have
/// takes a place of its own.
fn check_pools(porter: &mut Porter, refs: &[Rc<StockFile>], log: &Log) -> Result<()> {
    let mut have: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in refs {
        if f.resident || f.base == "patch_mp" {
            for (ty, names) in pool_names(porter.st, &f.tree.assets)? {
                have.entry(ty).or_default().extend(names);
            }
        }
    }
    let assets = porter.root.assets.clone();
    let mut copies: BTreeMap<String, i32> = BTreeMap::new();
    for_each(&mut *porter.st, Roots::Entries(&assets), |st, n| {
        if !st.is_struct(Some(n)) || pool_limit(st.st(n).asset) == 0 {
            return Ok(());
        }
        let ty = st.st(n).asset;
        let name = pool_name(st, n);
        if named(&name)
            && !only_named(&name)
            && have.get(ty).is_some_and(|h| h.contains(&lower(name.as_deref().unwrap_or(""))))
        {
            *copies.entry(ty.to_string()).or_insert(0) += 1;
        }
        Ok(())
    })?;
    let mut used: Vec<(f64, String)> = Vec::new();
    let mut over = Vec::new();
    for (ty, names) in pool_names(porter.st, &assets)? {
        let mut all = names;
        if let Some(h) = have.get(&ty) {
            all.extend(h.iter().cloned());
        }
        let n = all.len() as i32 + copies.get(&ty).copied().unwrap_or(0);
        let limit = pool_limit(&ty);
        used.push((f64::from(n) / f64::from(limit), format!("{ty} {n}/{limit}")));
        if n > limit {
            over.push(format!("{ty}: {n} of {limit}"));
        } else if f64::from(n) > f64::from(limit) * 0.95 {
            porter.warn(&format!(
                "{n} {ty} assets with the files loaded alongside, of the {limit} the game has room for"
            ))?;
        }
    }
    // (largest first; the texts tell apart two of the same share)
    used.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let top: Vec<String> = used.iter().take(4).map(|u| u.1.clone()).collect();
    log(&format!("  game asset room used (with the files loaded alongside): {}", joined(&top, usize::MAX, "")))?;
    if !over.is_empty() {
        return Err(port_error(format!(
            "the map holds more assets than the game has room for ({}); it would stop loading part way",
            joined(&over, usize::MAX, "")
        )));
    }
    Ok(())
}

/// The 360 game fills block 5 (callback) at load time: a bump allocator there takes, for
/// every material, 4 bytes per texture, 4 per model surface and 16 per some other assets.
/// The file carries no data for it, so a PC file converts to size 0 and the game writes
/// through a null pointer. Stock files reserve 1-72 KB.
fn reserve_callback_block(st: &mut Store, zone: &mut [u8], t: &Tree) -> Result<()> {
    let mut need: u64 = 0;
    for_each(st, Roots::Entries(&t.assets), |st, n| {
        let Some(o) = st.as_struct(Some(n)).filter(|o| o.is_asset()) else { return Ok(()) };
        let material = o.is_asset_named("Material");
        let model = o.is_asset_named("XModel");
        need = need.wrapping_add(16);
        if material {
            need = need.wrapping_add(4u64.wrapping_mul(st.int(n, "textureCount")?.max(0) as u64));
        } else if model {
            need = need.wrapping_add(4u64.wrapping_mul(st.int(n, "numsurfs")?.max(0) as u64));
        }
        Ok(())
    })?;
    // Other load-time allocations can land here too, so at least what a stock map
    // reserves.
    need = (need.wrapping_mul(2).wrapping_add(4096).wrapping_add(0xFFF) & !0xFFF).max(0x10000);
    let word = zone.get_mut(28..32).ok_or_else(|| Error::new("a zone without its header"))?;
    if load(word, 4, true) < need {
        store(word, 4, need, true);
    }
    Ok(())
}

/// The stock pictures copied in whose pixels live in the disc's picture packs: the
/// container lists where, four entries a picture, in the order the zone has them.
fn pak_table(st: &mut Store, mut written: Vec<(usize, NodeId)>) -> Result<Vec<PakEntry>> {
    written.sort_by_key(|&(at, _)| at);
    let mut table = Vec::new();
    for (_, d) in written {
        if !st.as_struct(Some(d)).is_some_and(|s| s.is_asset_named("GfxImage")) || st.int(d, "streaming")? == 0 {
            continue;
        }
        match st.st(d).pak.clone() {
            Some(pak) => table.extend_from_slice(&pak),
            None => {
                let name = asset_name(st, d).unwrap_or_else(|| "?".to_string());
                return Err(port_error(format!("picture {name} streams, but from no pak entries")));
            }
        }
    }
    Ok(table)
}

/// A converted loading screen made the menus' picture of the map: its briefing material
/// alone, named preview_<map> as the menus look a map's picture up (the downloadable
/// maps' are in dlc1_ui_mp.ff and dlc2_ui_mp.ff, zones the menus load), its picture named
/// the same. Its zone is named after itself by a raw file, as the game's are.
fn preview(st: &mut Store, t: &mut Tree, map: &str) -> Result<bool> {
    let name = format!("preview_{map}");
    let mut material = None;
    let mut zone_name = None;
    for &e in &t.assets {
        let (ty, a) = {
            let entry = st.entry(e);
            (entry.ty.clone(), entry.asset)
        };
        if let Some(a) = a.filter(|&a| st.is_struct(Some(a))) {
            if ty == "material" && name_of(st, a).as_deref() == Some("$levelbriefing") {
                material = Some((e, a));
            }
            if ty == "rawfile" && lower(&name_of(st, a).unwrap_or_default()) == lower(&format!("{map}_load")) {
                zone_name = Some((e, a));
            }
        }
    }
    let Some((material, material_asset)) = material else { return Ok(false) };
    let mut pictured = false;
    for_each(st, Roots::One(Some(material_asset)), |st, n| {
        if st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("GfxImage")) {
            set_name(st, n, name.clone());
            pictured = true;
        }
        Ok(())
    })?;
    if !pictured {
        return Ok(false);
    }
    set_name(st, material_asset, name);
    t.assets = vec![material];
    if let Some((e, a)) = zone_name {
        set_name(st, a, format!("{map}_preview"));
        t.assets.push(e);
    }
    Ok(true)
}
