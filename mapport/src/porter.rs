// The conversion of one PC fastfile's tree into the 360's (port.rs runs it).
//
// Every struct is carried over to the 360's struct of the same name member by member
// (numbers in big-endian, members matched by name); what the two platforms keep
// differently is converted by hooks on the structs concerned: map and model vertices,
// placed models, the world's surfaces and culling trees, materials and their render
// state, pictures. What the 360 can't run from the PC file comes from the stock 360 files
// given: shader sets, the render state of materials using them, effects, sounds,
// animations, models and the teams' assets.
//
// The steps are in the modules named after them: convert.rs (structs), porter.rs (the
// main sequence and the steps that look at the whole tree), stock.rs (stock assets),
// sounds.rs, materials.rs, pictures.rs, world.rs and decals.rs (the world), teams.rs.

use crate::error::{Error, Result};
use crate::images::read_iwi;
use crate::num::{load, store, truncate};
use crate::ordered::{AssetIndex, AssetKey, Ordered};
use crate::port::pool_names;
use crate::query::{
    self, alias_name, asset_name, deref, deref_struct, each_held, embedded, is_builder_signature, joined, named,
    only_named, ptr_array_value, rawfile_text, set_name, set_rawfile_text, without_commas,
};
use crate::rules;
use crate::tree::{ElemType, FOLLOWING, INSERT, NodeId, Roots, Store, Tree, collect, for_each};
use crate::xma::Xma;
use crate::zip::Zip;
use mw2_zone::defs::Kind;
use mw2_zone::schema::Schema;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// Where the converter's log goes. It returns Error::Cancelled to stop the conversion.
pub type Log = Rc<dyn Fn(&str) -> Result<()>>;

/// An entry of a zip archive (an .iwd file).
#[derive(Clone)]
pub struct ZipEntry {
    pub zip: Rc<Zip>,
    pub index: usize,
}

/// Entries of zip archives by name in lower case.
pub type ZipIndex = HashMap<String, ZipEntry>;

/// A stock 360 file read for the conversion.
pub struct StockFile {
    /// Its place among the files read, which tells it from the others.
    pub id: usize,
    pub path: PathBuf,
    /// Its name without .ff, lower case.
    pub base: String,
    pub tree: Tree,
    /// The game keeps it loaded (code_post_gfx_mp, common_mp).
    pub resident: bool,
}

/// A match's two sides, by their teams' names (as arena files give them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Teams {
    pub allies: String,
    pub axis: String,
}

/// A part in another colour: the stock model's colour, and the map's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColourSwap {
    pub stock: String,
    pub map: String,
}

/// A breakable part to copy in: the name needed, the stock model, its file, and the
/// colour swap when the stock part has another colour.
#[derive(Clone, Debug)]
pub struct Breakable {
    pub need: String,
    pub src: String,
    pub path: PathBuf,
    pub swap: Option<ColourSwap>,
}

#[derive(Clone)]
pub struct StockAsset {
    pub asset: NodeId,
    pub file: Rc<StockFile>,
}

#[derive(Clone)]
pub struct StockAlias {
    pub alias: NodeId,
    pub resident: bool,
    pub file: Rc<StockFile>,
    pub order: i64,
}

pub struct MissingImage {
    pub name: String,
    /// A built-in picture, else magenta.
    pub stand_in: Option<String>,
}

pub struct SortKeySwap {
    pub material: String,
    pub pc: i64,
    pub stock: i64,
}

/// The map's own sounds, encoded: by PC sound file name, lower case.
#[derive(Default)]
pub struct EncodedSound {
    pub xma: Option<Xma>,
    /// The game has it loaded: only named.
    pub resident: bool,
    /// The 360 sound made of it, once made.
    pub first: Option<NodeId>,
}

impl EncodedSound {
    pub fn plays(&self) -> bool {
        self.xma.is_some() || self.resident
    }
}

/// A PC alias head that plays a sound found here, and the sound file's name in lower
/// case.
pub struct HeadAudio {
    pub head: NodeId,
    pub sound: String,
}

// ---- the world's data (world.rs, decals.rs) ---------------------------------------------------

/// A GfxWorldVertex's size.
pub const VERTEX_BYTES: usize = 44;
/// The name hashes of a material's colour pictures (colorMap, colorMap1).
pub const COLOR_MAP: u32 = 0xA0AB1041;
pub const COLOR_MAP1: u32 = 0xB60D1850;

/// The stock animations by name, and the file each is in.
pub type StockAnims = (Ordered<String, NodeId>, HashMap<NodeId, Rc<StockFile>>);

pub struct Porter<'a> {
    pub st: &'a mut Store,
    pub breakables: Vec<Breakable>,
    pub destructible_aliases: Vec<String>,
    pub map_name: String,
    pub warnings: Vec<String>,
    /// The PC's rules and the 360's.
    pub p: &'static Schema,
    pub x: &'static Schema,

    pub(crate) root: &'a mut Tree,
    pub(crate) refs: Vec<Rc<StockFile>>,
    pub(crate) log: Log,
    pub(crate) texture_budget: i32,
    pub(crate) iwds: Vec<Rc<Zip>>,
    pub(crate) game_iwds: Vec<Rc<Zip>>,
    /// The pictures of the map's .iwd files and of the PC game's.
    pub(crate) map_pictures: ZipIndex,
    pub(crate) game_pictures: ZipIndex,
    /// Pictures taken from the PC game's files.
    pub(crate) from_game: i32,

    /// Stock 360 assets: those the game has loaded (named only, ",name"), those to copy
    /// in, and those stock files only name.
    pub(crate) resident: AssetIndex<NodeId>,
    pub(crate) library: AssetIndex<NodeId>,
    pub(crate) common_materials: AssetIndex<NodeId>,
    pub(crate) named: BTreeSet<(String, String)>,
    /// (type, lower name) the resident files have.
    pub(crate) loaded: BTreeSet<(String, String)>,
    /// (type, lower name) common_mp has.
    pub(crate) common_names: BTreeSet<(String, String)>,
    pub(crate) stock_fx: Ordered<String, StockAsset>,
    pub(crate) stock_models: Ordered<String, StockAsset>,
    pub(crate) stock_aliases: Ordered<String, StockAlias>,
    /// Lower-case picture name -> a stock streamed one.
    pub(crate) stock_streamed: Ordered<String, NodeId>,
    /// (gpu format, cube) -> a stock picture.
    pub(crate) templates: BTreeMap<(i32, bool), NodeId>,
    pub(crate) lightmap_templates: Ordered<String, NodeId>,
    /// Shader set name -> a stock material using it.
    pub(crate) material_templates: Ordered<String, NodeId>,

    pub(crate) done: HashSet<NodeId>,
    pub(crate) mip_drop: HashMap<String, i32>,
    pub(crate) techset_swaps: Ordered<String, String>,
    pub(crate) missing_images: Vec<MissingImage>,
    pub(crate) unreadable: Vec<String>,
    pub(crate) magenta_materials: Vec<String>,
    pub(crate) invisible_materials: Vec<String>,
    pub(crate) invisible_ts: HashSet<NodeId>,
    pub(crate) stock_fx_used: Vec<NodeId>,
    /// A model -> the asset list Entry it is swapped for.
    pub(crate) model_swap: HashMap<NodeId, NodeId>,
    pub(crate) swapped_rows: HashSet<NodeId>,
    pub(crate) pc_sort: HashMap<NodeId, i64>,
    pub(crate) pc_cull: HashMap<NodeId, u32>,
    /// A material being converted -> its template.
    pub(crate) material_template: HashMap<NodeId, NodeId>,
    pub(crate) invisible: HashSet<NodeId>,
    pub(crate) two_sided: i32,
    pub(crate) dxn_count: i32,
    pub(crate) streamed_stock: i32,
    /// Material (and texture scale) -> numbers.
    pub(crate) stock_glass: Option<BTreeMap<String, [u32; 2]>>,
    /// Sort key -> {lit: 1, unlit: 0, unknown: -1}.
    pub(crate) stock_sort_keys: Option<BTreeMap<i64, BTreeSet<i32>>>,
    pub(crate) sort_key_kind_swapped: Vec<SortKeySwap>,
    pub(crate) encoded: HashMap<String, EncodedSound>,
    /// The map's loaded sounds by name, lower case.
    pub(crate) pc_loaded: HashMap<String, NodeId>,
    pub(crate) sound_files: Option<ZipIndex>,
    /// Sound files with no audio found.
    pub(crate) no_audio: BTreeSet<String>,
    pub(crate) encoded_bytes: usize,
    pub(crate) encode_count: i32,
    pub(crate) encode_seconds: f64,
    /// Some aren't 16-bit PCM.
    pub(crate) compressed_sounds: bool,
    pub(crate) stock_techniques: Option<HashMap<String, Vec<Option<NodeId>>>>,
    pub(crate) techset_has_cache: BTreeMap<(String, i32), Option<bool>>,
    /// Asset list Entries made here.
    pub(crate) extra_assets: Vec<NodeId>,
    pub(crate) moved_images: Vec<NodeId>,
    /// By stock file id.
    pub(crate) picked: HashMap<usize, BTreeSet<usize>>,
    pub(crate) owners: HashMap<usize, HashMap<NodeId, usize>>,
    pub(crate) stock_entries: HashSet<NodeId>,
    pub(crate) remapped: HashSet<NodeId>,
    pub(crate) ambient_swaps: Ordered<String, String>,
    pub(crate) script_words: BTreeSet<String>,
    pub(crate) world_checksum: i64,
    pub(crate) bone_bounds: i32,
    pub(crate) ground_lit: i32,
    pub(crate) stock_anims: Option<StockAnims>,
}

impl AsMut<Store> for Porter<'_> {
    fn as_mut(&mut self) -> &mut Store {
        self.st
    }
}

// Every Struct, Array, Leaf and PtrList the writer will write: those the asset list holds
// and those reached only through a pointer back to earlier data, each once.
fn reach(st: &mut Store, roots: Vec<Option<NodeId>>) -> Result<Vec<NodeId>> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut stack = roots;
    let mut out = Vec::new();
    let follow = |st: &Store, x: Option<NodeId>, stack: &mut Vec<Option<NodeId>>| {
        if let Some(r) = st.as_ref(x) {
            let mut t = r.target;
            if let Some(s) = st.as_slot(t) {
                t = s.asset;
            } else if let Some(e) = st.as_entry(t) {
                t = e.asset;
            }
            if st.is_struct(t) || st.is_array(t) || st.is_ptr_list(t) {
                stack.push(t);
            }
        } else if st.is_struct(x) || st.is_array(x) || st.is_leaf(x) || st.is_ptr_list(x) {
            stack.push(x);
        }
    };
    while let Some(o) = stack.pop() {
        let Some(o) = o else { continue };
        if !seen.insert(o) {
            continue;
        }
        out.push(o);
        if st.is_struct(Some(o)) {
            let (ty, size) = {
                let s = st.st(o);
                (s.ty, s.size())
            };
            for m in ty.members() {
                if m.has_pointer() || (m.offset + m.size) as usize > size {
                    continue;
                }
                if let Some(late) = st.late(o, &m.name) {
                    stack.push(Some(late));
                    continue;
                }
                if !ty.of(m).unwrapped().is_record() {
                    continue;
                }
                if m.mods.is_empty() {
                    stack.push(Some(st.sub_m(o, m, -1)?));
                } else {
                    let n = m.mods.iter().fold(1usize, |n, &x| n.wrapping_mul(x as usize));
                    for k in 0..n {
                        stack.push(Some(st.sub_m(o, m, k as i64)?));
                    }
                }
            }
            if let Some(ch) = st.ch(o) {
                for (_, c) in ch.items.clone() {
                    for x in each_held(st, c) {
                        follow(st, x, &mut stack);
                    }
                    if st.is_ptr_list(c) {
                        stack.push(c);
                    }
                }
            }
        } else if let Some(a) = st.as_array(Some(o)) {
            for x in a.items.clone() {
                follow(st, x, &mut stack);
            }
        } else if let Some(pl) = st.as_ptr_list(Some(o)) {
            for x in pl.items.clone() {
                follow(st, x, &mut stack);
            }
        }
    }
    Ok(out)
}

// The images/*.iwi pictures of some .iwd files, by name in lower case (the game looks names
// up whatever their case or slashes); later files win.
fn picture_index(zips: &[Rc<Zip>]) -> ZipIndex {
    let mut found = ZipIndex::new();
    for z in zips {
        for (k, name) in z.names().iter().enumerate() {
            let low = name.to_ascii_lowercase().replace('\\', "/");
            if low.starts_with("images/") && low.len() > 11 && low.ends_with(".iwi") {
                found.insert(low[7..low.len() - 4].to_string(), ZipEntry { zip: z.clone(), index: k });
            }
        }
    }
    found
}

// common_mp's ambience tracks (all streamed from the 360's disc) and the words that pick
// each, first match wins: one word, or words that must all be there ("middle east").
const AMBIENCES: &[(&str, &[&str])] = &[
    ("ambient_mp_duststorm", &["duststorm", "sandstorm", "dust storm"]),
    ("ambient_mp_airport", &["airport", "terminal", "hangar"]),
    ("ambient_mp_estate", &["estate"]),
    ("embient_mp_highrise", &["highrise", "rooftop", "skyscraper"]), // (sic: the stock alias's spelling)
    ("ambient_mp_favela", &["favela", "jungle", "tropical"]),
    ("ambient_mp_rain", &["rain", "thunder", "storm"]),
    ("ambient_mp_snow", &["snow", "winter", "arctic", "blizzard", "frozen", "frost"]),
    (
        "ambient_mp_desert",
        &[
            "desert",
            "middle east",
            "mideast",
            "arab",
            "sand",
            "afghan",
            "quarry",
            "boneyard",
            "backlot",
            "bog",
            "broadcast",
            "citystreets",
            "district",
            "convoy",
            "ambush",
            "crash",
            "crossfire",
            "showdown",
            "strike",
            "toujane",
        ],
    ),
    (
        "ambient_mp_urban",
        &["urban", "city", "town", "street", "traffic", "industrial", "factory", "docks", "harbor", "harbour"],
    ),
    (
        "ambient_mp_rural",
        &[
            "rural", "farm", "forest", "country", "village", "field", "swamp", "creek", "russia", "birds", "crickets",
            "bloc", "pipeline", "vacant", "overgrown", "france", "carentan",
        ],
    ),
];
const AMBIENCE_DEFAULT: &str = "ambient_mp_rural";

// Whether every word of `key` ("middle east") is among `words`.
fn has_all(words: &BTreeSet<String>, key: &str) -> bool {
    key.split(' ').all(|w| words.contains(w))
}

// The runs of letters of a text, lower case.
fn words(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut w = String::new();
    for c in text.to_ascii_lowercase().chars() {
        if c.is_ascii_lowercase() {
            w.push(c);
        } else if !w.is_empty() {
            out.insert(std::mem::take(&mut w));
        }
    }
    if !w.is_empty() {
        out.insert(w);
    }
    out
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

/// A call `ambientPlay("track")` in a script: where it starts and ends, where the track's
/// name starts and ends, at or after `from`.
fn next_ambient_play(text: &str, from: usize) -> Option<(usize, usize, usize, usize)> {
    const CALL: &str = "ambientPlay";
    let b: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = b.partition_point(|&(at, _)| at < from);
    while i < b.len() {
        let at = b[i].0;
        if text[at..].starts_with(CALL) && (i == 0 || !word_char(b[i - 1].1)) {
            let mut j = i + CALL.chars().count();
            while j < b.len() && regex_space(b[j].1) {
                j += 1;
            }
            if j < b.len() && b[j].1 == '(' {
                j += 1;
                while j < b.len() && regex_space(b[j].1) {
                    j += 1;
                }
                if j < b.len() && b[j].1 == '"' {
                    let name_start = j + 1;
                    let mut k = name_start;
                    while k < b.len() && b[k].1 != '"' {
                        k += 1;
                    }
                    if k < b.len() && k > name_start {
                        let end = b.get(k + 1).map_or(text.len(), |c| c.0);
                        return Some((at, b[name_start].0, b[k].0, end));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

// Sets the pointer that held child `k` of `o`, which now holds `value`: in a union, its
// first word when it said the data followed; else the member's, when it is a pointer and
// said so.
fn mark_held(st: &mut Store, o: NodeId, member: &str, value: u32, only_if_following: bool) -> Result<()> {
    let (union, big) = {
        let s = st.st(o);
        (s.ty.kind() == Kind::Union, s.big)
    };
    if union {
        let Some(d) = st.data_mut(o).get_mut(..4) else { return Ok(()) };
        let cur = load(d, 4, big) as u32;
        if !only_if_following || cur == FOLLOWING || cur == INSERT {
            store(d, 4, u64::from(value), big);
        }
    } else if let Some(m) = st.find(o, member)
        && m.is_pointer()
    {
        let cur = st.ptr(o, member)?;
        if !only_if_following || cur == FOLLOWING || cur == INSERT {
            st.set_ptr(o, member, value)?;
        }
    }
    Ok(())
}

impl<'a> Porter<'a> {
    /// `iwds`: the map's own .iwd files; `game_iwds`: the PC game's, in the order it loads
    /// them, for pictures and sounds the map's lack.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        st: &'a mut Store,
        root: &'a mut Tree,
        refs: Vec<Rc<StockFile>>,
        iwds: Vec<Rc<Zip>>,
        game_iwds: &[Rc<Zip>],
        log: Log,
        texture_budget: i32,
    ) -> Result<Porter<'a>> {
        let map_pictures = picture_index(&iwds);
        let game_pictures = picture_index(game_iwds);
        let mut p = Porter {
            st,
            breakables: Vec::new(),
            destructible_aliases: Vec::new(),
            map_name: String::new(),
            warnings: Vec::new(),
            p: rules::pc()?,
            x: rules::xbox()?,
            root,
            refs,
            log,
            texture_budget,
            iwds,
            game_iwds: game_iwds.to_vec(),
            map_pictures,
            game_pictures,
            from_game: 0,
            resident: AssetIndex::new(),
            library: AssetIndex::new(),
            common_materials: AssetIndex::new(),
            named: BTreeSet::new(),
            loaded: BTreeSet::new(),
            common_names: BTreeSet::new(),
            stock_fx: Ordered::new(),
            stock_models: Ordered::new(),
            stock_aliases: Ordered::new(),
            stock_streamed: Ordered::new(),
            templates: BTreeMap::new(),
            lightmap_templates: Ordered::new(),
            material_templates: Ordered::new(),
            done: HashSet::new(),
            mip_drop: HashMap::new(),
            techset_swaps: Ordered::new(),
            missing_images: Vec::new(),
            unreadable: Vec::new(),
            magenta_materials: Vec::new(),
            invisible_materials: Vec::new(),
            invisible_ts: HashSet::new(),
            stock_fx_used: Vec::new(),
            model_swap: HashMap::new(),
            swapped_rows: HashSet::new(),
            pc_sort: HashMap::new(),
            pc_cull: HashMap::new(),
            material_template: HashMap::new(),
            invisible: HashSet::new(),
            two_sided: 0,
            dxn_count: 0,
            streamed_stock: 0,
            stock_glass: None,
            stock_sort_keys: None,
            sort_key_kind_swapped: Vec::new(),
            encoded: HashMap::new(),
            pc_loaded: HashMap::new(),
            sound_files: None,
            no_audio: BTreeSet::new(),
            encoded_bytes: 0,
            encode_count: 0,
            encode_seconds: 0.0,
            compressed_sounds: false,
            stock_techniques: None,
            techset_has_cache: BTreeMap::new(),
            extra_assets: Vec::new(),
            moved_images: Vec::new(),
            picked: HashMap::new(),
            owners: HashMap::new(),
            stock_entries: HashSet::new(),
            remapped: HashSet::new(),
            ambient_swaps: Ordered::new(),
            script_words: BTreeSet::new(),
            world_checksum: 0,
            bone_bounds: 0,
            ground_lit: 0,
            stock_anims: None,
        };
        p.index_stock()?;
        Ok(p)
    }

    // ---- the stock files' indexes -----------------------------------------------------

    fn index_stock(&mut self) -> Result<()> {
        for f in self.refs.clone() {
            let mut idx: AssetIndex<NodeId> = AssetIndex::new();
            let mut aliases: Ordered<String, NodeId> = Ordered::new();
            for_each(self.st, Roots::Entries(&f.tree.assets), |st, n| {
                let Some(o) = st.as_struct(Some(n)).filter(|o| o.is_asset()) else { return Ok(()) };
                let (asset, sound) = (o.asset, o.is_asset_named("snd_alias_list_t"));
                let mut name = asset_name(st, n);
                if !named(&name) {
                    name = query::name(st, n);
                }
                if let Some(name) = name {
                    idx.find_or_add(AssetKey::new(asset, name), n);
                }
                if sound && let Some(alias) = alias_name(st, n) {
                    aliases.find_or_add(alias, n);
                }
                Ok(())
            })?;
            if f.resident {
                for (ty, names) in pool_names(self.st, &f.tree.assets)? {
                    for n in names {
                        self.loaded.insert((ty.clone(), n.clone()));
                        if f.base == "common_mp" {
                            self.common_names.insert((ty.clone(), n));
                        }
                    }
                }
            }
            let mut order: HashMap<Option<NodeId>, i64> = HashMap::new();
            for (i, &e) in f.tree.assets.iter().enumerate() {
                order.entry(self.st.entry(e).asset).or_insert(i as i64);
            }
            for (k, &v) in aliases.iter() {
                if k.starts_with(',') {
                    continue;
                }
                let replace = match self.stock_aliases.find(k) {
                    None => true,
                    Some(old) => old.resident && !f.resident,
                };
                if replace {
                    let at = order.get(&Some(v)).copied().unwrap_or(-1);
                    self.stock_aliases
                        .set(k.clone(), StockAlias { alias: v, resident: f.resident, file: f.clone(), order: at });
                }
            }
            if !f.resident {
                for &e in &f.tree.assets {
                    let entry = self.st.entry(e);
                    let (ty, asset) = (entry.ty.clone(), entry.asset);
                    if ty == "fx"
                        && let Some(a) = asset.filter(|&a| self.st.is_struct(Some(a)))
                        && let Some(n) = query::name(self.st, a)
                        && !n.starts_with(',')
                    {
                        self.stock_fx.find_or_add(n, StockAsset { asset: a, file: f.clone() });
                    }
                }
                for (k, &v) in idx.iter() {
                    if k.ty == "XModel" && !k.name.starts_with(',') {
                        self.stock_models.find_or_add(k.name.clone(), StockAsset { asset: v, file: f.clone() });
                    }
                }
            }
            for (k, &v) in idx.iter() {
                if let Some(rest) = k.name.strip_prefix(',') {
                    self.named.insert((k.ty.clone(), rest.to_string()));
                    continue;
                }
                if f.base == "common_mp" {
                    // Not used by name: only what a map's own materials (heat distortion,
                    // ...) need a stock material of the same shader set for.
                    if k.ty == "Material" {
                        self.common_materials.find_or_add(k.clone(), v);
                    }
                    continue;
                }
                if f.resident {
                    self.resident.find_or_add(k.clone(), v);
                } else {
                    self.library.find_or_add(k.clone(), v);
                }
            }
        }
        // A stock picture held in its file, of each format: the header to copy.
        let mut stock: Vec<(AssetKey, NodeId)> = self.resident.iter().map(|(k, &v)| (k.clone(), v)).collect();
        stock.extend(self.library.iter().map(|(k, &v)| (k.clone(), v)));
        for (k, v) in &stock {
            if k.ty != "GfxImage" || self.st.ptr(*v, "pixels")? != FOLLOWING || self.st.int(*v, "streaming")? != 0 {
                continue;
            }
            let texture = self.st.bytes(*v, "texture")?;
            let format = i32::from(*texture.get(32).ok_or_else(|| Error::new("a picture's texture is cut short"))? & 0x3F);
            let cube = self.st.int(*v, "mapType")? == 5;
            self.templates.entry((format, cube)).or_insert(*v);
        }
        // Stock lightmaps: their channel order differs from other pictures'.
        let library: Vec<(AssetKey, NodeId)> = self.library.iter().map(|(k, &v)| (k.clone(), v)).collect();
        for (k, v) in &library {
            if k.ty == "GfxImage" && k.name.starts_with("*lightmap") && self.st.ptr(*v, "pixels")? == FOLLOWING {
                let suffix = &k.name[k.name.rfind('_').map_or(0, |i| i + 1)..];
                self.lightmap_templates.find_or_add(suffix.to_string(), *v);
            }
        }
        // Stock 360 materials by the shader set they use (render state templates).
        let mut materials = library;
        materials.extend(self.resident.iter().map(|(k, &v)| (k.clone(), v)));
        materials.extend(self.common_materials.iter().map(|(k, &v)| (k.clone(), v)));
        for (k, v) in materials {
            if k.ty != "Material" {
                continue;
            }
            let ts = self.st.child(v, "techniqueSet");
            if let Some(ts) = deref_struct(self.st, ts)?
                && let Some(tn) = asset_name(self.st, ts)
            {
                self.material_templates.find_or_add(without_commas(&tn), v);
            }
        }
        Ok(())
    }

    // ---- the picture budget -----------------------------------------------------------

    // Which pictures lose top mip levels so the map's pictures fit the budget: the largest
    // picture (shine maps counted double) goes down a level at a time. Lightmaps,
    // reflection probes and loading screens stay as they are.
    fn plan_texture_budget(&mut self) -> Result<()> {
        // The 360's tiling: the block comes to about this much more.
        const TILING_PAD: f64 = 1.28;
        let budget = truncate(f64::from(self.texture_budget.wrapping_mul(1048576)) / TILING_PAD);
        let mut fixed: i64 = 0;
        struct Item {
            sizes: Vec<usize>,
            w: i32,
            h: i32,
        }
        let mut items: Ordered<String, Item> = Ordered::new();
        let ents = self.root.assets.clone();
        for_each(self, Roots::Entries(&ents), |p, n| {
            if !p.st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("GfxImage")) {
                return Ok(());
            }
            let name = asset_name(p.st, n);
            if !named(&name) || only_named(&name) {
                return Ok(());
            }
            let name = name.unwrap();
            if name.starts_with("loadscreen") {
                return Ok(());
            }
            if p.resident.has(&AssetKey::new("GfxImage", name.clone())) && (name.starts_with('$') || !p.in_iwd(&name)) {
                return Ok(());
            }
            if p.streams_from_stock(&name) {
                return Ok(()); // copied from stock, streamed
            }
            let tex = embedded(p.st, n, "texture");
            if let Some(ld) = tex.and_then(|t| p.st.child(t, "loadDef")).filter(|&l| p.st.is_struct(Some(l))) {
                let size = p.st.int(ld, "resourceSize")?;
                if size != 0 {
                    fixed += size;
                    return Ok(());
                }
            }
            let low = name.to_ascii_lowercase();
            let Some(found) = p.map_pictures.get(&low).or_else(|| p.game_pictures.get(&low)).cloned() else {
                return Ok(());
            };
            let pic = match found.zip.read(found.index).and_then(|data| read_iwi(&data)) {
                Ok(pic) => pic,
                Err(e) if e.is_error() => return Ok(()),
                Err(e) => return Err(e),
            };
            let total: usize = pic.mips.iter().map(Vec::len).sum();
            if pic.cube || pic.mips.len() < 2 {
                fixed += total as i64;
            } else {
                items.set(name, Item { sizes: pic.mips.iter().map(Vec::len).collect(), w: pic.width, h: pic.height });
            }
            Ok(())
        })?;
        let mut total = fixed;
        for (_, it) in items.iter() {
            total += it.sizes.iter().map(|&s| s as i64).sum::<i64>();
        }
        let before = total;
        // Specular (shine) maps count double: they go down before colour and normal maps of
        // the same size, whose loss shows far more.
        let weight = |n: &str, size: i64| {
            let low = n.to_ascii_lowercase();
            let spec = low.starts_with('~')
                || low.contains("_spc")
                || low.contains("_spec")
                || low.contains("_cos")
                || low.contains("_gloss");
            -size * if spec { 2 } else { 1 }
        };
        // The smallest (weight, name) first.
        let mut heap: BinaryHeap<Reverse<(i64, String)>> = BinaryHeap::new();
        for (n, it) in items.iter() {
            let sum: i64 = it.sizes.iter().map(|&s| s as i64).sum();
            heap.push(Reverse((weight(n, sum), n.clone())));
        }
        while total > budget {
            let Some(Reverse((_, n))) = heap.pop() else { break };
            let it = items.find(&n).unwrap();
            let k = self.mip_drop.get(&n).copied().unwrap_or(0);
            if (k + 1) as usize >= it.sizes.len() || it.w.min(it.h) >> (k + 1) < 32 {
                continue;
            }
            total -= it.sizes[k as usize] as i64;
            self.mip_drop.insert(n.clone(), k + 1);
            let rest: i64 = it.sizes[(k + 1) as usize..].iter().map(|&s| s as i64).sum();
            heap.push(Reverse((weight(&n, rest), n)));
        }
        if !self.mip_drop.is_empty() {
            self.say(&format!(
                "  pictures: about {:.0} MB, over the {} MB budget, so {} pictures lose their top mip levels (about {:.0} MB left)",
                before as f64 * TILING_PAD / 1048576.0,
                self.texture_budget,
                self.mip_drop.len(),
                total as f64 * TILING_PAD / 1048576.0
            ))?;
            if total > budget {
                let line = format!(
                    "the pictures still come to about {:.0} MB (budget {} MB): the rest are lightmaps, probes and pictures that are already small",
                    total as f64 * TILING_PAD / 1048576.0,
                    self.texture_budget
                );
                self.warn(&line)?;
            }
        }
        Ok(())
    }

    // ---- the conversion ---------------------------------------------------------------

    /// Turns the tree into the 360's.
    pub fn convert(&mut self) -> Result<()> {
        if self.texture_budget != 0 {
            self.plan_texture_budget()?;
        }
        let mut from_stock: Vec<String> = Vec::new();
        for e in self.root.assets.clone() {
            let entry = self.st.entry(e);
            let Some(d) = entry.asset.filter(|_| entry.ty == "rawfile").filter(|&d| self.st.is_struct(Some(d))) else {
                continue;
            };
            // IW4x's ZoneBuilder signs a file with a raw file stored as it is but marked
            // compressed: the 360 would take it for a zlib stream.
            if is_builder_signature(self.st, d)? {
                set_rawfile_text(self.st, d, "")?;
                continue;
            }
            // The stock 360 map's own script: PC scripts can call what only later PC
            // patches have (PC mp_rust's killTrigger).
            let name = query::name(self.st, d).unwrap_or_default();
            if let Some(&src) = self.library.find(&AssetKey::new("RawFile", name)) {
                let text = match rawfile_text(self.st, src) {
                    Ok(t) => Some(t),
                    Err(e) if e.is_error() => None,
                    Err(e) => return Err(e),
                };
                if let Some(text) = text
                    && text != rawfile_text(self.st, d)?
                {
                    set_rawfile_text(self.st, d, &text)?;
                    from_stock.push(query::name(self.st, d).unwrap_or_default());
                }
            } else {
                self.edit_script(d)?;
            }
        }
        if !from_stock.is_empty() {
            self.say(&format!(
                "  {} script{} from the stock 360 files: {}",
                from_stock.len(),
                if from_stock.len() > 1 { "s" } else { "" },
                from_stock.join(", ")
            ))?;
        }
        let gfx = self.root.assets.iter().find_map(|&e| {
            let entry = self.st.entry(e);
            entry.asset.filter(|&a| entry.ty == "gfx_map" && self.st.is_struct(Some(a)))
        });
        self.map_name = match gfx {
            Some(g) => {
                let n = query::name(self.st, g).unwrap_or_default();
                let n = n.strip_prefix("maps/mp/").unwrap_or(&n);
                n.strip_suffix(".d3dbsp").unwrap_or(n).to_string()
            }
            None => String::new(),
        };
        self.world_checksum = match gfx {
            Some(g) => self.st.int(g, "checksum")?,
            None => 0,
        };
        // Some PC files list the pieces of a model or shader set as assets of their own;
        // stock 360 files never do. The PC shaders go; model surfaces are written where the
        // model that uses them points at them.
        let mut gone: HashSet<NodeId> = HashSet::new();
        let mut slots: HashMap<NodeId, NodeId> = HashMap::new();
        for e in self.root.assets.clone() {
            let entry = self.st.entry(e);
            let ty = entry.ty.clone();
            let Some(a) = entry.asset.filter(|&a| self.st.is_struct(Some(a))) else { continue };
            if ty == "pixelshader" || ty == "vertexshader" || ty == "vertexdecl" {
                gone.insert(a);
            }
            if ty == "xmodelsurfs" {
                let slot = match self.st.st(a).slot {
                    Some(s) => s,
                    None => {
                        let s = self.st.make_slot(Some(a));
                        self.st.st_mut(a).slot = Some(s);
                        s
                    }
                };
                slots.insert(e, slot);
            }
        }
        // Pointers to a model surfaces asset's entry become pointers to its slot.
        if !slots.is_empty() {
            let ents = self.root.assets.clone();
            for_each(self.st, Roots::Entries(&ents), |st, n| {
                let Some(ch) = st.as_struct(Some(n)).and_then(|o| o.ch()) else { return Ok(()) };
                for (_, c) in st.children(ch).items.clone() {
                    for x in each_held(st, c) {
                        let Some(r) = st.as_ref_mut(x) else { continue };
                        if r.rel != 4 {
                            continue;
                        }
                        let Some(&slot) = r.target.and_then(|t| slots.get(&t)) else { continue };
                        r.target = Some(slot);
                        r.rel = 0;
                        r.t = ElemType::default();
                    }
                }
                Ok(())
            })?;
        }
        let st = &*self.st;
        self.root.assets.retain(|&e| {
            !matches!(st.entry(e).ty.as_str(), "pixelshader" | "vertexshader" | "vertexdecl" | "xmodelsurfs")
        });
        self.stock_effects()?;
        self.add_destructible_aliases()?;
        self.stock_sounds()?;
        self.stock_anims()?;
        self.stock_model_swap()?;
        self.name_common_assets()?;
        self.effect_sister_materials()?;
        let orphans = self.mark_orphans(&gone, false)?;
        for e in self.root.assets.clone() {
            let entry = self.st.entry(e);
            if entry.ty == "vertexshader" || entry.ty == "vertexdecl" {
                return Err(port_error(format!("top-level {} assets can't be converted", entry.ty)));
            }
            if let Some(a) = entry.asset.filter(|&a| self.st.is_struct(Some(a))) {
                self.convert_asset(a)?;
            }
        }
        for a in orphans {
            self.convert_asset(a)?;
            self.st.st_mut(a).forward = true;
        }
        if !self.extra_assets.is_empty() {
            // Assets made here (merged decal layers' composite materials): before the
            // world, whose pointers to them must point back; shader sets first, the
            // materials point at them.
            let ents = &self.root.assets;
            let at = ents.iter().position(|&e| self.st.entry(e).ty == "gfx_map").unwrap_or(ents.len());
            let mut made = self.extra_assets.clone();
            made.sort_by_key(|&e| self.st.entry(e).ty != "techset");
            let count = made.len();
            self.root.assets.splice(at..at, made);
            self.pictures_before_composites(at, at + count)?;
        }
        if !self.breakables.is_empty() {
            self.add_breakables()?;
        }
        if self.from_game != 0 {
            self.say(&format!("  {} pictures come from the PC game's own .iwd files", self.from_game))?;
        }
        if !self.sort_key_kind_swapped.is_empty() {
            let swaps: Vec<String> =
                self.sort_key_kind_swapped.iter().map(|s| format!("{} {}->{}", s.material, s.pc, s.stock)).collect();
            self.say(&format!(
                "  {} PC sort keys stock uses only for the other kind of material replaced by the template's ({})",
                swaps.len(),
                joined(&swaps, 5, ", ...")
            ))?;
        }
        if self.two_sided != 0 {
            self.say(&format!(
                "  {} materials drawn two-sided as on the PC (the stock render state they take culls back faces)",
                self.two_sided
            ))?;
        }
        if self.dxn_count != 0 {
            self.say(&format!("  {} normal maps converted to DXN, as the 360 keeps them", self.dxn_count))?;
        }
        self.say(&format!(
            "  {} pictures stream from the 360's own picture packs, as stock maps have them ({} stock streamed pictures known)",
            self.streamed_stock,
            self.stock_streamed.len()
        ))?;
        // Merging decal layers drops the decal materials it drew into composites: what they
        // held first is only pointed at from then on.
        self.own_reference_copies()?;
        let late = self.mark_orphans(&gone, true)?;
        if !late.is_empty() {
            self.say(&format!(
                "  {} assets the merged decal materials held written where their next pointer is",
                late.len()
            ))?;
        }
        for i in 0..self.unreadable.len().min(5) {
            let line = format!("picture {} can't be read, so it's treated as missing", self.unreadable[i]);
            self.warn(&line)?;
        }
        if self.unreadable.len() > 5 {
            let line = format!("... and {} more pictures that can't be read", self.unreadable.len() - 5);
            self.warn(&line)?;
        }
        // The first few of a list, for a note.
        let list = |v: &[String]| joined(v, 5, ", ...");
        if !self.missing_images.is_empty() {
            let place =
                if self.game_pictures.is_empty() { "the map's .iwd" } else { "the map's .iwd or the PC game's files" };
            let (mut magenta, mut flat) = (Vec::new(), Vec::new());
            for m in &self.missing_images {
                if m.stand_in.is_some() { flat.push(m.name.clone()) } else { magenta.push(m.name.clone()) }
            }
            if !magenta.is_empty() {
                let line = format!(
                    "{} picture{} in {place}, so magenta stands in ({})",
                    magenta.len(),
                    if magenta.len() > 1 { "s aren't" } else { " isn't" },
                    list(&magenta)
                );
                self.warn(&line)?;
            }
            if !flat.is_empty() {
                let line = format!(
                    "{} normal/specular map{} in {place}, so flat / no-shine ones stand in ({})",
                    flat.len(),
                    if flat.len() > 1 { "s aren't" } else { " isn't" },
                    list(&flat)
                );
                self.warn(&line)?;
            }
        }
        let n = self.magenta_materials.len();
        if n != 0 {
            let line = format!(
                "{n}{} a picture the shader set used needs, so {} magenta ({})",
                if n > 1 { " materials lack" } else { " material lacks" },
                if n > 1 { "they show" } else { "it shows" },
                list(&self.magenta_materials)
            );
            self.warn(&line)?;
        }
        let n = self.invisible_materials.len();
        if n != 0 {
            let line = format!(
                "{n}{} with a tool or distance-fade shader set draw{} nothing ({})",
                if n > 1 { " materials" } else { " material" },
                if n > 1 { "" } else { "s" },
                list(&self.invisible_materials)
            );
            self.warn(&line)?;
        }
        let mut swaps: Vec<(String, String)> = self.techset_swaps.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
        swaps.sort();
        for (a, b) in swaps {
            if self.is_tools_techset(&a)? {
                if self.hides(&a, &b)? {
                    continue;
                }
                let line = format!(
                    "shader set {a} isn't in the stock files given and no alpha tested one is either, so {b} is used and its surfaces show"
                );
                self.warn(&line)?;
                continue;
            }
            self.warn(&format!("shader set {a} isn't in the stock files given, so {b} is used"))?;
        }
        self.rename_clashes()?;
        if self.bone_bounds != 0 {
            self.say(&format!(
                "  {} models' bone boxes rebuilt from their vertices (as stock 360 models have them)",
                self.bone_bounds
            ))?;
        }
        if self.ground_lit != 0 {
            self.say(&format!("  {} placed models with a ground colour given the ground-lit flag", self.ground_lit))?;
        }
        self.root.platform = "xbox".into();
        Ok(())
    }

    /// A picture a stock map streams from the console's picture packs, by its name in
    /// lower case (the first one given stays).
    pub fn know_streamed(&mut self, name: String, image: NodeId) {
        self.stock_streamed.find_or_add(name, image);
    }

    // ---- scripts ----------------------------------------------------------------------

    // A map script's ambience track (ambientPlay("name")) that no stock file has becomes
    // the closest track the 360 has loaded.
    fn edit_script(&mut self, d: NodeId) -> Result<()> {
        let name = query::name(self.st, d).unwrap_or_default();
        if !name.ends_with(".gsc") {
            return Ok(());
        }
        let text = rawfile_text(self.st, d)?;
        self.script_words = words(&name);
        let mut out = String::new();
        let mut at = 0;
        while let Some((start, track_start, track_end, end)) = next_ambient_play(&text, at) {
            out += &text[at..start];
            let pick = self.ambient_track(&text[track_start..track_end])?;
            if pick.is_empty() {
                out += &text[start..end];
            } else {
                out += &text[start..track_start - 1];
                out += &format!("\"{pick}\"");
            }
            at = end;
        }
        out += &text[at..];
        if out != text {
            set_rawfile_text(self.st, d, &out)?;
        }
        Ok(())
    }

    // The track to play in place of `name`, or "" to keep it.
    fn ambient_track(&mut self, name: &str) -> Result<String> {
        if self.stock_aliases.has(&name.to_string()) {
            return Ok(String::new());
        }
        let have: Vec<&str> =
            AMBIENCES.iter().map(|a| a.0).filter(|t| self.stock_aliases.has(&t.to_string())).collect();
        if have.is_empty() {
            return Ok(String::new());
        }
        // The words of its name and of everything its alias names (sound files, ...).
        let mut ws = words(name);
        for e in self.root.assets.clone() {
            let entry = self.st.entry(e);
            let Some(a) = entry.asset.filter(|_| entry.ty == "sound").filter(|&a| self.st.is_struct(Some(a))) else {
                continue;
            };
            if alias_name(self.st, a).as_deref() != Some(name) {
                continue;
            }
            for_each(self.st, Roots::One(Some(a)), |st, n| {
                if let Some(ch) = st.as_struct(Some(n)).and_then(|o| o.ch()) {
                    for (_, c) in &st.children(ch).items {
                        if let Some(s) = st.as_str(*c) {
                            ws.extend(words(&s.b));
                        }
                    }
                }
                Ok(())
            })?;
            break;
        }
        // The track's own words first (what the PC plays), then the map's name.
        let mut pick = "";
        for set in [&ws, &self.script_words] {
            if let Some(a) =
                AMBIENCES.iter().find(|a| have.contains(&a.0) && a.1.iter().any(|key| has_all(set, key)))
            {
                pick = a.0;
                break;
            }
        }
        if pick.is_empty() {
            pick = if have.contains(&AMBIENCE_DEFAULT) { AMBIENCE_DEFAULT } else { have[0] };
        }
        self.ambient_swaps.set(name.to_string(), pick.to_string());
        self.say(&format!("  ambience track {name} plays the 360's {pick} (it plays ambience only streamed from its disc)"))?;
        Ok(pick.to_string())
    }

    // ---- orphans: what only pointers hold any more ------------------------------------

    // Materials and pictures a left-out asset brought in first are only pointed at from
    // then on: the writer puts each where its first remaining pointer is. Returns them
    // (`new_only`: those not marked already).
    fn mark_orphans(&mut self, gone: &HashSet<NodeId>, new_only: bool) -> Result<Vec<NodeId>> {
        let ents = self.root.assets.clone();
        let all = collect(self.st, Roots::Entries(&ents))?;
        let reached: HashSet<NodeId> = all.iter().copied().collect();
        let mut orphans: Ordered<NodeId, NodeId> = Ordered::new();
        for &n in &all {
            let Some(ch) = self.st.as_struct(Some(n)).and_then(|o| o.ch()) else { continue };
            for (_, c) in self.st.children(ch).items.clone() {
                for x in each_held(self.st, c) {
                    let a = self
                        .st
                        .as_ref(x)
                        .and_then(|r| self.st.as_slot(r.target))
                        .and_then(|s| s.asset)
                        .filter(|&a| self.st.is_struct(Some(a)));
                    if let Some(a) = a
                        && !reached.contains(&a)
                        && !gone.contains(&a)
                    {
                        orphans.find_or_add(a, a);
                    }
                }
            }
        }
        let mut inner: HashSet<NodeId> = HashSet::new();
        for (_, &a) in orphans.iter() {
            for_each(self.st, Roots::One(Some(a)), |_, n| {
                if n != a {
                    inner.insert(n);
                }
                Ok(())
            })?;
        }
        let mut found = Vec::new();
        for (&k, &a) in orphans.iter() {
            if inner.contains(&k) || (new_only && self.st.st(a).forward) {
                continue;
            }
            self.st.st_mut(a).forward = true;
            found.push(a);
        }
        Ok(found)
    }

    // A pointer to the slot of a picture that is only a name (",$white") gets its own copy
    // of that name, as stock materials repeat such names inline: the 360 orders some
    // structs' members otherwise than the PC, so the pointer can come before the asset it
    // points into.
    fn own_reference_copies(&mut self) -> Result<()> {
        // A pointer into another element's pointer array (effects share a mark material):
        // pointed at the material itself, or a copy of a name-only one.
        let mut shared = 0;
        let point = |p: &mut Porter, held: Option<NodeId>| -> Result<Option<NodeId>> {
            let Some(c) = held.filter(|&c| p.st.is_ref(Some(c))) else { return Ok(None) };
            let Some(v) = ptr_array_value(p.st, c) else { return Ok(None) };
            let slot = p.st.as_struct(Some(v)).and_then(|s| s.slot);
            if let Some(slot) = slot {
                let r = p.st.make_ref(0);
                p.st.reference_mut(r).target = Some(slot);
                return Ok(Some(r));
            }
            if p.st.is_struct(Some(v)) && only_named(&query::name(p.st, v)) {
                let copy = p.st.copy(v)?;
                let s = p.st.st_mut(copy);
                s.slot = None;
                s.forward = false;
                p.done.insert(copy);
                return Ok(Some(copy));
            }
            Ok(Some(v))
        };
        let ents = self.root.assets.clone();
        for n in collect(self.st, Roots::Entries(&ents))? {
            if self.st.is_struct(Some(n)) {
                let Some(ch) = self.st.st(n).ch() else { continue };
                for i in 0..self.st.children(ch).items.len() {
                    let held = self.st.children(ch).items[i].1;
                    if let Some(v) = point(self, held)? {
                        self.st.children_mut(ch).items[i].1 = Some(v);
                        shared += 1;
                    }
                }
            } else if self.st.is_ptr_list(Some(n)) {
                for i in 0..self.st.ptr_list(n).items.len() {
                    let held = self.st.ptr_list(n).items[i];
                    if let Some(v) = point(self, held)? {
                        self.st.ptr_list_mut(n).items[i] = Some(v);
                        shared += 1;
                    }
                }
            }
        }
        if shared != 0 {
            self.say(&format!("  {shared} pointers into another element's material list pointed at the material"))?;
        }
        // Name-only assets of other kinds too once nothing holds them any more.
        let objs = collect(self.st, Roots::Entries(&ents))?;
        let held: HashSet<NodeId> = objs.iter().copied().collect();
        let mut types: BTreeMap<String, i32> = BTreeMap::new();
        let mut count = 0;
        for &o in &objs {
            let Some(ch) = self.st.as_struct(Some(o)).and_then(|s| s.ch()) else { continue };
            for i in 0..self.st.children(ch).items.len() {
                let (key, c) = self.st.children(ch).items[i].clone();
                let a = self
                    .st
                    .as_ref(c)
                    .filter(|r| r.rel == 0)
                    .and_then(|r| self.st.as_slot(r.target))
                    .and_then(|s| s.asset)
                    .filter(|&a| self.st.is_struct(Some(a)));
                let Some(a) = a else { continue };
                if !only_named(&query::name(self.st, a))
                    || !(self.st.st(a).is_asset_named("GfxImage") || !held.contains(&a))
                {
                    continue;
                }
                let copy = self.st.copy(a)?;
                {
                    let s = self.st.st_mut(copy);
                    s.slot = None;
                    s.forward = false;
                }
                if let Some(ach) = self.st.st(a).ch() {
                    let own = self.st.make_children();
                    *self.st.children_mut(own) = self.st.children(ach).clone();
                    self.st.st_mut(copy).set_ch(Some(own));
                }
                // (its info, a material's, keeps sharing the name's: a copy of the struct,
                // not of what it holds)
                self.st.children_mut(ch).items[i].1 = Some(copy);
                let union = self.st.st(o).ty.kind() == Kind::Union;
                if union && key.idx.is_empty() {
                    let big = self.st.st(o).big;
                    if let Some(d) = self.st.data_mut(o).get_mut(..4) {
                        store(d, 4, u64::from(FOLLOWING), big);
                    }
                } else if let Some(m) = self.st.find(o, key.member)
                    && m.is_pointer()
                {
                    self.st.set_ptr(o, key.member, FOLLOWING)?;
                }
                self.done.insert(copy);
                count += 1;
                let asset = self.st.st(a).asset;
                *types.entry(if asset.is_empty() { "other".into() } else { asset.to_string() }).or_default() += 1;
            }
        }
        if count != 0 {
            let list: Vec<String> = types.iter().map(|(t, n)| format!("{t} {n}")).collect();
            self.say(&format!(
                "  {count} pointers to a name-only asset given their own copy of the name ({})",
                list.join(", ")
            ))?;
        }
        Ok(())
    }

    // The composite materials of merged decal layers point at the pictures of the ground
    // and decal materials; one first written further on would be pointed at before it is
    // written: it is written at the composite's pointer instead, and the places that held
    // it point back at it.
    fn pictures_before_composites(&mut self, start: usize, end: usize) -> Result<()> {
        let ents = self.root.assets.clone();
        let before: HashSet<NodeId> = collect(self.st, Roots::Entries(&ents[..start]))?.into_iter().collect();
        let mut late: Ordered<NodeId, NodeId> = Ordered::new();
        for n in collect(self.st, Roots::Entries(&ents[start..end]))? {
            let Some(ch) = self.st.as_struct(Some(n)).and_then(|o| o.ch()) else { continue };
            for (_, c) in self.st.children(ch).items.clone() {
                for x in each_held(self.st, c) {
                    let a = self
                        .st
                        .as_ref(x)
                        .and_then(|r| self.st.as_slot(r.target))
                        .and_then(|s| s.asset)
                        .filter(|&a| self.st.as_struct(Some(a)).is_some_and(|s| s.is_asset_named("GfxImage")));
                    if let Some(a) = a
                        && !before.contains(&a)
                    {
                        late.find_or_add(a, a);
                    }
                }
            }
        }
        if late.is_empty() {
            return Ok(());
        }
        for o in collect(self.st, Roots::Entries(&ents[end..]))? {
            let Some(ch) = self.st.as_struct(Some(o)).and_then(|s| s.ch()) else { continue };
            for i in 0..self.st.children(ch).items.len() {
                let (key, v) = self.st.children(ch).items[i].clone();
                let Some(v) = v.filter(|&v| self.st.is_struct(Some(v)) && late.has(&v)) else { continue };
                let Some(slot) = self.st.st(v).slot else { continue };
                // (Not null until the writer places the picture.)
                let r = self.st.make_ref(1);
                self.st.reference_mut(r).target = Some(slot);
                self.st.children_mut(ch).items[i].1 = Some(r);
                mark_held(self.st, o, key.member, 1, true)?;
            }
        }
        for (_, &a) in late.iter() {
            self.st.st_mut(a).forward = true;
        }
        // Every pointer to such a picture points at the picture itself, not its slot: the
        // first one writes it there.
        for_each(self.st, Roots::Entries(&ents), |st, n| {
            let Some(ch) = st.as_struct(Some(n)).and_then(|o| o.ch()) else { return Ok(()) };
            for (_, c) in st.children(ch).items.clone() {
                for x in each_held(st, c) {
                    let Some(asset) = st.as_ref(x).and_then(|r| st.as_slot(r.target)).map(|s| s.asset) else {
                        continue;
                    };
                    if asset.is_some_and(|a| late.has(&a)) {
                        let r = st.as_ref_mut(x).unwrap();
                        r.target = asset;
                        r.rel = 0;
                        r.t = ElemType::default();
                    }
                }
            }
            Ok(())
        })?;
        self.say(&format!(
            "  decal layers: {} pictures written with the composite materials that use them (first written further on before)",
            late.len()
        ))
    }

    // ---- names ------------------------------------------------------------------------

    // The map's own materials and pictures named as one a stock effect brings get a name
    // of their own: the game keeps one asset per name.
    fn rename_clashes(&mut self) -> Result<()> {
        let mut ours: HashSet<NodeId> = HashSet::new();
        let mut names: BTreeSet<(String, String)> = BTreeSet::new();
        for fx in self.stock_fx_used.clone() {
            for_each(self.st, Roots::One(Some(fx)), |st, n| {
                ours.insert(n);
                if let Some(o) = st.as_struct(Some(n))
                    && (o.is_asset_named("Material") || o.is_asset_named("GfxImage"))
                {
                    let asset = o.asset;
                    if let Some(nm) = query::name(st, n)
                        && !nm.starts_with(',')
                    {
                        names.insert((asset.to_string(), nm));
                    }
                }
                Ok(())
            })?;
        }
        self.share_effect_pictures(&ours)?;
        let ents = self.root.assets.clone();
        for_each(self.st, Roots::Entries(&ents), |st, n| {
            let Some(o) = st.as_struct(Some(n)) else { return Ok(()) };
            if ours.contains(&n) || !(o.is_asset_named("Material") || o.is_asset_named("GfxImage")) {
                return Ok(());
            }
            let asset = o.asset.to_string();
            if let Some(nm) = query::name(st, n)
                && names.contains(&(asset, nm.clone()))
            {
                set_name(st, n, format!("~pc/{nm}"));
            }
            Ok(())
        })
    }

    // A picture of the map's with the name of one a stock effect brings, when it comes
    // from the PC game's files rather than the map's .iwd, is the same picture: the map's
    // materials point at the stock effect's copy.
    fn share_effect_pictures(&mut self, ours: &HashSet<NodeId>) -> Result<()> {
        let mut stock: Ordered<String, NodeId> = Ordered::new();
        for fx in self.stock_fx_used.clone() {
            for_each(self.st, Roots::One(Some(fx)), |st, n| {
                if st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("GfxImage")) {
                    let nm = query::name(st, n).unwrap_or_else(|| ",".into());
                    if !nm.starts_with(',') {
                        stock.find_or_add(nm, n);
                    }
                }
                Ok(())
            })?;
        }
        if stock.is_empty() {
            return Ok(());
        }
        let mut swap: HashMap<NodeId, NodeId> = HashMap::new();
        let ents = self.root.assets.clone();
        let map_pictures = &self.map_pictures;
        for_each(self.st, Roots::Entries(&ents), |st, n| {
            if ours.contains(&n) || !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("GfxImage")) {
                return Ok(());
            }
            let nm = query::name(st, n).unwrap_or_default();
            if let Some(&s) = stock.find(&nm)
                && !map_pictures.contains_key(&nm.to_ascii_lowercase())
            {
                swap.insert(n, s);
                if let Some(slot) = st.st(n).slot {
                    swap.insert(slot, s);
                }
            }
            Ok(())
        })?;
        if swap.is_empty() {
            return Ok(());
        }
        for &v in swap.values() {
            // Stock effects hold their pictures inline: written with a slot of its own at
            // its first pointer, which the map's pointers then name.
            if self.st.st(v).slot.is_none() {
                let s = self.st.make_slot(Some(v));
                self.st.st_mut(v).slot = Some(s);
            }
            self.st.st_mut(v).forward = true;
        }
        let mut moved = 0;
        for_each(self.st, Roots::Entries(&ents), |st, o| {
            if ours.contains(&o) {
                return Ok(());
            }
            let Some(ch) = st.as_struct(Some(o)).and_then(|s| s.ch()) else { return Ok(()) };
            for i in 0..st.children(ch).items.len() {
                let (key, c) = st.children(ch).items[i].clone();
                let mut target = None;
                if let Some(&t) = c.and_then(|c| swap.get(&c))
                    && st.is_struct(c)
                {
                    target = Some(t);
                } else if let Some(r) = st.as_ref(c)
                    && r.rel == 0
                    && let Some(&t) = r.target.and_then(|rt| swap.get(&rt))
                {
                    target = Some(t);
                }
                let Some(target) = target else { continue };
                let r = st.make_ref(1);
                st.reference_mut(r).target = st.st(target).slot;
                st.children_mut(ch).items[i].1 = Some(r);
                mark_held(st, o, key.member, 1, true)?;
                moved += 1;
            }
            Ok(())
        })?;
        let st = &*self.st;
        self.root.assets.retain(|&e| {
            let a = st.entry(e).asset;
            !(st.is_struct(a) && swap.contains_key(&a.unwrap()))
        });
        let distinct: HashSet<NodeId> = swap.values().copied().collect();
        self.say(&format!(
            "  {} pictures the map shares with stock effects it uses taken from them ({moved} pointers), not kept twice",
            distinct.len()
        ))
    }

    // A full asset under a name common_mp has (always loaded) becomes a reference to the
    // loaded one, as stock maps name them. One that other assets point into stays, as the
    // parts would be written nowhere.
    fn name_common_assets(&mut self) -> Result<()> {
        // A raw file common_mp has goes: the game loads raw files by name.
        let mut dropped: Vec<String> = Vec::new();
        let mut keep = Vec::with_capacity(self.root.assets.len());
        for e in self.root.assets.clone() {
            let entry = self.st.entry(e);
            let (ty, a) = (entry.ty.clone(), entry.asset);
            if ty == "rawfile"
                && let Some(a) = a.filter(|&a| self.st.is_struct(Some(a)))
            {
                let name = query::name(self.st, a);
                if self.common_names.contains(&("RawFile".to_string(), name.clone().unwrap_or_default().to_ascii_lowercase())) {
                    dropped.push(name.unwrap_or_default());
                    continue;
                }
            }
            keep.push(e);
        }
        self.root.assets = keep;
        if !dropped.is_empty() {
            self.say(&format!("  {} raw files common_mp has left out ({})", dropped.len(), joined(&dropped, 4, "")))?;
        }
        const NAMED: [&str; 6] = ["GfxImage", "Material", "XModel", "FxEffectDef", "MaterialTechniqueSet", "PhysPreset"];
        let mut found: Vec<NodeId> = Vec::new();
        let ents = self.root.assets.clone();
        for_each(self, Roots::Entries(&ents), |p, n| {
            let Some(o) = p.st.as_struct(Some(n)) else { return Ok(()) };
            if p.done.contains(&n) || !o.is_asset() || !NAMED.contains(&o.asset) {
                return Ok(());
            }
            let asset = o.asset.to_string();
            let Some(nm) = asset_name(p.st, n) else { return Ok(()) };
            if nm.starts_with(',') {
                return Ok(());
            }
            if p.common_names.contains(&(asset, nm.to_ascii_lowercase())) {
                found.push(n);
            }
            Ok(())
        })?;
        if found.is_empty() {
            return Ok(());
        }
        let mut inside: HashMap<NodeId, NodeId> = HashMap::new();
        for &o in &found {
            for_each(self.st, Roots::One(Some(o)), |_, x| {
                if x != o {
                    inside.insert(x, o);
                }
                Ok(())
            })?;
            if let Some(slot) = self.st.st(o).slot {
                inside.remove(&slot);
            }
        }
        let held: HashSet<NodeId> = found.iter().copied().collect();
        let mut pointed_into: HashSet<NodeId> = HashSet::new();
        for_each(self.st, Roots::Entries(&ents), |st, n| {
            if inside.contains_key(&n) || held.contains(&n) {
                return Ok(());
            }
            let Some(ch) = st.as_struct(Some(n)).and_then(|o| o.ch()) else { return Ok(()) };
            for (_, c) in st.children(ch).items.clone() {
                let check = |st: &Store, x: Option<NodeId>, pointed_into: &mut HashSet<NodeId>| {
                    let Some(r) = st.as_ref(x) else { return };
                    let mut t = r.target;
                    if let Some(s) = st.as_slot(t) {
                        t = s.asset;
                    }
                    if let Some(&owner) = t.and_then(|t| inside.get(&t))
                        && (r.rel != 0 || !st.as_struct(t).is_some_and(|s| s.is_asset()))
                    {
                        pointed_into.insert(owner);
                    }
                };
                if st.is_array(c) || st.is_ptr_list(c) {
                    for x in each_held(st, c) {
                        check(st, x, &mut pointed_into);
                    }
                } else {
                    check(st, c, &mut pointed_into);
                }
            }
            Ok(())
        })?;
        let mut by_type: BTreeMap<String, i32> = BTreeMap::new();
        let mut kept: Vec<String> = Vec::new();
        for o in found {
            let name = asset_name(self.st, o).unwrap_or_default();
            if pointed_into.contains(&o) {
                kept.push(name);
                continue;
            }
            let asset = self.st.st(o).asset.to_string();
            *by_type.entry(asset.clone()).or_default() += 1;
            let made = self.reference(&asset, &name)?;
            self.replace(o, made)?;
        }
        if !by_type.is_empty() {
            let total: i32 = by_type.values().sum();
            let list: Vec<String> = by_type.iter().map(|(t, c)| format!("{t} {c}")).collect();
            self.say(&format!("  {total} assets common_mp has named instead of copied ({})", list.join(", ")))?;
        }
        if !kept.is_empty() {
            self.say(&format!(
                "  note: {} kept as copies (other assets point inside them: {})",
                kept.len(),
                joined(&kept, 5, "")
            ))?;
        }
        Ok(())
    }

    // The lower-case name of a material's colour picture, if it has one.
    fn color_image(&mut self, m: NodeId) -> Result<Option<String>> {
        let tt = self.st.child(m, "textureTable");
        let Some(tt) = deref(self.st, tt)?.filter(|&t| self.st.is_array(Some(t))) else { return Ok(None) };
        for n in self.st.array(tt).items.clone() {
            let Some(td) = n.filter(|&t| self.st.is_struct(Some(t))) else { continue };
            if self.st.u(td, "nameHash")? != u64::from(COLOR_MAP) {
                continue;
            }
            let u = embedded(self.st, td, "u");
            let image = u.and_then(|u| self.st.child(u, "image"));
            let Some(im) = deref_struct(self.st, image)? else { return Ok(None) };
            let nm = without_commas(&query::name(self.st, im).unwrap_or_default());
            return Ok(Some(nm.to_ascii_lowercase()));
        }
        Ok(None)
    }

    // Effect materials named as one of common_mp's with an "_eye<n>" suffix and the same
    // render state and colour picture are drawn with common_mp's.
    fn effect_sister_materials(&mut self) -> Result<()> {
        let mut common: Ordered<String, NodeId> = Ordered::new();
        for (k, &v) in self.common_materials.iter() {
            if k.ty == "Material" {
                common.find_or_add(k.name.to_ascii_lowercase(), v);
            }
        }
        let common_ts: BTreeSet<String> = self
            .common_names
            .iter()
            .filter(|(t, _)| t == "MaterialTechniqueSet")
            .map(|(_, n)| n.clone())
            .collect();
        let fx: Vec<Option<NodeId>> = self
            .root
            .assets
            .iter()
            .filter(|&&e| self.st.entry(e).ty == "fx")
            .map(|&e| self.st.entry(e).asset)
            .collect();
        let mut swap: HashMap<NodeId, String> = HashMap::new();
        for o in reach(self.st, fx.clone())? {
            if !self.st.as_struct(Some(o)).is_some_and(|s| s.is_asset_named("Material")) || swap.contains_key(&o) {
                continue;
            }
            let name = asset_name(self.st, o).unwrap_or_default();
            let sister = without_eye(&name).to_string();
            if name.starts_with(',') || sister == name || !common.has(&sister.to_ascii_lowercase()) {
                continue;
            }
            let ts = self.st.child(o, "techniqueSet");
            let tsn = match deref_struct(self.st, ts)? {
                Some(ts) => query::name(self.st, ts).unwrap_or_default(),
                None => String::new(),
            };
            if tsn.is_empty() || tsn.starts_with(',') || common_ts.contains(&tsn.to_ascii_lowercase()) {
                continue;
            }
            let s = *common.find(&sister.to_ascii_lowercase()).unwrap();
            if self.st.bytes(s, "stateBitsEntry")? != self.st.bytes(o, "stateBitsEntry")?
                || self.color_image(s)? != self.color_image(o)?
            {
                continue;
            }
            swap.insert(o, sister);
        }
        if swap.is_empty() {
            return Ok(());
        }
        let mut count = 0;
        for o in reach(self.st, fx)? {
            let Some(ch) = self.st.as_struct(Some(o)).and_then(|s| s.ch()) else { continue };
            for i in 0..self.st.children(ch).items.len() {
                let (key, v) = self.st.children(ch).items[i].clone();
                let m = if self.st.is_ref(v) {
                    deref_struct(self.st, v)?
                } else {
                    v.filter(|&v| self.st.is_struct(Some(v)))
                };
                let Some(sister) = m.and_then(|m| swap.get(&m)).cloned() else { continue };
                let made = self.reference("Material", &sister)?;
                self.st.children_mut(ch).items[i].1 = Some(made);
                if let Some(mem) = self.st.find(o, key.member)
                    && mem.is_pointer()
                {
                    self.st.set_ptr(o, key.member, FOLLOWING)?;
                }
                count += 1;
            }
        }
        let names: BTreeSet<&String> = swap.values().collect();
        let list: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        self.say(&format!("  {count} effect material pointers drawn with common_mp's own material ({})", list.join(", ")))
    }
}

// A name without its "_eye<digits>" ending.
fn without_eye(name: &str) -> &str {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return name;
    }
    name[..name.len() - digits].strip_suffix("_eye").unwrap_or(name)
}


impl Porter<'_> {
    /// Logs a line of the conversion's log.
    pub(crate) fn say(&self, line: &str) -> Result<()> {
        (self.log)(line)
    }

    /// A note of what the conversion could not do as the PC map does, once each, also in
    /// the log.
    pub fn warn(&mut self, msg: &str) -> Result<()> {
        if self.warnings.iter().any(|w| w == msg) {
            return Ok(());
        }
        self.warnings.push(msg.to_string());
        self.say(&format!("  note: {msg}"))
    }
}

/// A PortError of the text formatted.
pub fn port_error(text: impl Into<String>) -> Error {
    Error::port(text)
}
