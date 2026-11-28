// The map's two teams. IW4x loads team assets (soldier bodies, heads and arms, flag and
// crate models, team icons) from its own files; on the 360 each map carries its teams', so
// they are copied in from the stock maps that have them, and the map's script is set to use
// them.

use crate::error::{Error, Result};
use crate::paths;
use crate::porter::{Porter, StockFile, Teams};
use crate::query::{asset_in_slot, each_held, joined, name, rawfile_text, set_rawfile_text};
use crate::stock::{entry_asset, entry_type};
use crate::tree::{NodeId, Roots, for_each};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

struct TeamAssets {
    team: &'static str,
    models: &'static [&'static str],
    materials: &'static [&'static str],
}

// Per team (mp/factionTable.csv and the character scripts in common_mp): the models and
// icons a map carries for it.
#[rustfmt::skip]
const TEAMS: &[TeamAssets] = &[
    TeamAssets {
        team: "us_army",
        models: &[
            "head_allies_us_army_sniper",
            "head_us_army_a",
            "head_us_army_b",
            "head_us_army_c",
            "head_us_army_d",
            "head_us_army_e",
            "head_us_army_f",
            "mp_body_army_sniper",
            "mp_body_us_army_assault_a",
            "mp_body_us_army_assault_b",
            "mp_body_us_army_assault_c",
            "mp_body_us_army_lmg",
            "mp_body_us_army_lmg_b",
            "mp_body_us_army_lmg_c",
            "mp_body_us_army_riot",
            "mp_body_us_army_shotgun",
            "mp_body_us_army_shotgun_b",
            "mp_body_us_army_shotgun_c",
            "mp_body_us_army_smg",
            "mp_body_us_army_smg_b",
            "mp_body_us_army_smg_c",
            "viewhands_sniper_us_army",
            "viewhands_us_army",
            "prop_flag_ranger",
            "prop_flag_ranger_carry",
            "com_plasticcase_rangers",
        ],
        materials: &[
            "faction_128_rangers",
            "faction_128_rangers_fade",
            "objpoint_flag_rangers",
            "headicon_rangers",
        ],
    },
    TeamAssets {
        team: "opforce_composite",
        models: &[
            "head_op_arab_sniper",
            "head_opforce_arab_a",
            "head_opforce_arab_b",
            "head_opforce_arab_c",
            "head_opforce_arab_d_hat",
            "head_opforce_arab_e",
            "head_riot_op_arab",
            "mp_body_op_arab_sniper",
            "mp_body_opforce_arab_assault_a",
            "mp_body_opforce_arab_lmg_a",
            "mp_body_opforce_arab_shotgun_a",
            "mp_body_opforce_arab_smg_a",
            "mp_body_riot_op_arab",
            "viewhands_militia",
            "viewhands_sniper_op_arab",
            "prop_flag_opforce",
            "prop_flag_opforce_carry",
            "com_plasticcase_arab",
        ],
        materials: &[
            "faction_128_arab",
            "faction_128_arab_fade",
            "objpoint_flag_arab",
            "headicon_arab",
        ],
    },
    TeamAssets {
        team: "opforce_arctic",
        models: &[
            "head_op_arctic_sniper",
            "head_opforce_arctic_a",
            "head_opforce_arctic_b",
            "head_opforce_arctic_c",
            "head_opforce_arctic_d",
            "head_riot_op_arctic",
            "mp_body_op_arctic_sniper",
            "mp_body_opforce_arctic_assault_a",
            "mp_body_opforce_arctic_assault_b",
            "mp_body_opforce_arctic_assault_c",
            "mp_body_opforce_arctic_lmg",
            "mp_body_opforce_arctic_lmg_b",
            "mp_body_opforce_arctic_lmg_c",
            "mp_body_opforce_arctic_shotgun",
            "mp_body_opforce_arctic_shotgun_b",
            "mp_body_opforce_arctic_shotgun_c",
            "mp_body_opforce_arctic_smg",
            "mp_body_opforce_arctic_smg_b",
            "mp_body_opforce_arctic_smg_c",
            "mp_body_riot_op_arctic",
            "viewhands_arctic_opforce",
            "viewhands_sniper_op_arctic",
            "prop_flag_speznas",
            "prop_flag_speznas_carry",
            "com_plasticcase_ussr",
        ],
        materials: &[
            "faction_128_ussr",
            "faction_128_ussr_fade",
            "objpoint_flag_ussr",
            "headicon_ussr",
        ],
    },
    TeamAssets {
        team: "opforce_airborne",
        models: &[
            "head_airborne_a",
            "head_airborne_b",
            "head_airborne_c",
            "head_airborne_d",
            "head_airborne_e",
            "head_op_airborne_sniper",
            "head_riot_op_airborne",
            "mp_body_airborne_assault_a",
            "mp_body_airborne_assault_b",
            "mp_body_airborne_assault_c",
            "mp_body_airborne_lmg",
            "mp_body_airborne_lmg_b",
            "mp_body_airborne_lmg_c",
            "mp_body_airborne_shotgun",
            "mp_body_airborne_shotgun_b",
            "mp_body_airborne_shotgun_c",
            "mp_body_airborne_smg",
            "mp_body_airborne_smg_b",
            "mp_body_airborne_smg_c",
            "mp_body_op_airborne_sniper",
            "mp_body_riot_op_airborne",
            "viewhands_russian_airborne",
            "viewhands_sniper_op_airborne",
            "prop_flag_speznas",
            "prop_flag_speznas_carry",
            "com_plasticcase_ussr",
        ],
        materials: &[
            "faction_128_ussr",
            "faction_128_ussr_fade",
            "objpoint_flag_ussr",
            "headicon_ussr",
        ],
    },
    TeamAssets {
        team: "militia",
        models: &[
            "head_militia_a_wht",
            "head_militia_ba_blk",
            "head_militia_bb_blk_hat",
            "head_militia_bc_blk",
            "head_militia_bd_blk",
            "head_op_militia_sniper",
            "head_riot_op_militia",
            "mp_body_militia_assault_aa_blk",
            "mp_body_militia_assault_aa_wht",
            "mp_body_militia_assault_ab_blk",
            "mp_body_militia_assault_ac_blk",
            "mp_body_militia_lmg_aa_blk",
            "mp_body_militia_lmg_ab_blk",
            "mp_body_militia_lmg_ac_blk",
            "mp_body_militia_smg_aa_blk",
            "mp_body_militia_smg_aa_wht",
            "mp_body_militia_smg_ab_blk",
            "mp_body_militia_smg_ac_blk",
            "mp_body_op_miltia_sniper",
            "mp_body_riot_op_militia",
            "viewhands_militia",
            "prop_flag_militia",
            "prop_flag_militia_carry",
            "com_plasticcase_militia",
        ],
        materials: &[
            "faction_128_militia",
            "faction_128_militia_fade",
            "objpoint_flag_militia",
            "headicon_militia",
        ],
    },
    TeamAssets {
        team: "socom_141",
        models: &[
            "head_seal_soccom_a",
            "head_seal_soccom_ba",
            "head_seal_soccom_ca",
            "head_seal_soccom_da",
            "mp_body_seal_soccom_assault_a",
            "mp_body_seal_soccom_assault_b",
            "mp_body_seal_soccom_assault_b_blk",
            "mp_body_seal_soccom_assault_c",
            "mp_body_seal_soccom_assault_c_blk",
            "mp_body_seal_soccom_assault_d",
            "viewhands_us_army",
            "prop_flag_tf141",
            "prop_flag_tf141_carry",
            "com_plasticcase_taskforce141",
        ],
        materials: &[
            "faction_128_taskforce141",
            "faction_128_taskforce141_fade",
            "objpoint_flag_taskforce",
            "headicon_taskforce141",
        ],
    },
    TeamAssets {
        team: "socom_141_arctic",
        models: &[
            "head_allies_tf141_arctic_sniper",
            "head_riot_tf141_arctic",
            "head_tf141_arctic_a",
            "head_tf141_arctic_b",
            "head_tf141_arctic_c",
            "head_tf141_arctic_d",
            "mp_body_riot_tf141_arctic",
            "mp_body_tf141_arctic_sniper",
            "mp_body_tf141_assault_a",
            "mp_body_tf141_assault_b",
            "mp_body_tf141_lmg",
            "mp_body_tf141_shotgun",
            "mp_body_tf141_smg",
            "viewhands_arctic",
            "viewhands_sniper_tf141_arctic",
            "viewhands_tf141",
            "prop_flag_tf141",
            "prop_flag_tf141_carry",
            "com_plasticcase_taskforce141",
        ],
        materials: &[
            "faction_128_taskforce141",
            "faction_128_taskforce141_fade",
            "objpoint_flag_taskforce",
            "headicon_taskforce141",
        ],
    },
    TeamAssets {
        team: "socom_141_desert",
        models: &[
            "head_allies_tf141_desert_sniper",
            "head_riot_tf141_desert",
            "head_tf141_desert_a",
            "head_tf141_desert_b",
            "head_tf141_desert_c",
            "head_tf141_desert_d",
            "mp_body_desert_tf141_assault_a",
            "mp_body_desert_tf141_assault_b",
            "mp_body_desert_tf141_lmg",
            "mp_body_desert_tf141_shotgun",
            "mp_body_desert_tf141_smg",
            "mp_body_riot_tf141_desert",
            "mp_body_tf141_desert_sniper",
            "viewhands_sniper_tf141_desert",
            "viewhands_tf141",
            "prop_flag_tf141",
            "prop_flag_tf141_carry",
            "com_plasticcase_taskforce141",
        ],
        materials: &[
            "faction_128_taskforce141",
            "faction_128_taskforce141_fade",
            "objpoint_flag_taskforce",
            "headicon_taskforce141",
        ],
    },
    TeamAssets {
        team: "socom_141_forest",
        models: &[
            "head_allies_tf141_forest_sniper",
            "head_riot_tf141_forest",
            "head_tf141_forest_a",
            "head_tf141_forest_b",
            "head_tf141_forest_c",
            "head_tf141_forest_d",
            "mp_body_forest_tf141_assault_a",
            "mp_body_forest_tf141_assault_b",
            "mp_body_forest_tf141_lmg",
            "mp_body_forest_tf141_shotgun",
            "mp_body_forest_tf141_smg",
            "mp_body_riot_tf141_forest",
            "mp_body_tf141_forest_sniper",
            "viewhands_sniper_tf141_forest",
            "viewhands_tf141",
            "prop_flag_tf141",
            "prop_flag_tf141_carry",
            "com_plasticcase_taskforce141",
        ],
        materials: &[
            "faction_128_taskforce141",
            "faction_128_taskforce141_fade",
            "objpoint_flag_taskforce",
            "headicon_taskforce141",
        ],
    },
    TeamAssets {
        team: "seals_udt",
        models: &[
            "head_allies_seal_udt_sniper",
            "head_riot_udt",
            "head_seal_udt_a",
            "head_seal_udt_c",
            "head_seal_udt_d",
            "head_seal_udt_e",
            "mp_body_riot_udt",
            "mp_body_seal_udt_assault_a",
            "mp_body_seal_udt_assault_b",
            "mp_body_seal_udt_lmg",
            "mp_body_seal_udt_smg",
            "mp_body_seal_udt_sniper",
            "viewhands_sniper_udt",
            "viewhands_udt",
            "prop_flag_seal",
            "prop_flag_seal_carry",
            "com_plasticcase_seals",
        ],
        materials: &[
            "faction_128_seals",
            "faction_128_seals_fade",
            "objpoint_flag_seals",
            "headicon_seals",
        ],
    },
];

fn on_side(side: &str, team: &str) -> bool {
    const ALLIES: &[&str] =
        &["us_army", "seals_udt", "socom_141", "socom_141_desert", "socom_141_forest", "socom_141_arctic"];
    const AXIS: &[&str] = &["opforce_composite", "opforce_airborne", "opforce_arctic", "militia"];
    let list = if side == "allies" { ALLIES } else { AXIS };
    list.contains(&team)
}

// ---- arenas -------------------------------------------------------------------------------

/// A blank as an arena file's patterns take one: space, tab, newline, vertical tab, form
/// feed, carriage return.
fn blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The pairs of a block's body: a word, blanks, then a value up to a blank or a quote
/// (optionally in quotes). A word not followed by a blank starts none.
fn pairs(body: &str) -> BTreeMap<String, String> {
    let mut kv = BTreeMap::new();
    let b: Vec<(usize, char)> = body.char_indices().collect();
    let at = |i: usize| b.get(i).map_or(body.len(), |&(p, _)| p);
    let mut i = 0;
    while i < b.len() {
        if !word(b[i].1) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < b.len() && word(b[j].1) {
            j += 1;
        }
        if j >= b.len() || !blank(b[j].1) {
            // No pair starts anywhere in this word.
            i = j;
            continue;
        }
        let key = body[at(i)..at(j)].to_string();
        while j < b.len() && blank(b[j].1) {
            j += 1;
        }
        if j < b.len() && b[j].1 == '"' {
            j += 1;
        }
        let start = j;
        while j < b.len() && b[j].1 != '"' && !blank(b[j].1) {
            j += 1;
        }
        let value = body[at(start)..at(j)].to_string();
        if j < b.len() && b[j].1 == '"' {
            j += 1;
        }
        kv.insert(key, value);
        i = j;
    }
    kv
}

/// An arena file's { ... } blocks, each as its keys and values.
fn arena_blocks(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(open) = text[pos..].find('{').map(|k| pos + k) {
        let Some(close) = text[open + 1..].find('}').map(|k| open + 1 + k) else { break };
        out.push(pairs(&text[open + 1..close]));
        pos = close + 1;
    }
    out
}

/// The stock maps whose arena entry has the team on a side.
pub fn team_maps(arena: &str, team: &str) -> Vec<String> {
    let mut out = Vec::new();
    for kv in arena_blocks(arena) {
        let get = |k: &str| kv.get(k).map_or("", String::as_str);
        if get("allieschar") == team || get("axischar") == team {
            out.push(get("map").to_string());
        }
    }
    out
}

/// A map's teams in an arena file.
pub fn arena_teams(arena: &str, map: &str) -> Option<Teams> {
    for kv in arena_blocks(arena) {
        if kv.get("map").is_some_and(|m| m == map)
            && let (Some(allies), Some(axis)) = (kv.get("allieschar"), kv.get("axischar"))
        {
            return Some(Teams { allies: allies.clone(), axis: axis.clone() });
        }
    }
    None
}

/// Where the line after a script's `main() {` starts: past the first "main", blanks, "(",
/// blanks, ")", blanks, "{" and the rest of its line with its newline.
fn after_main(text: &str) -> Option<usize> {
    let skip = |mut i: usize| {
        while let Some(c) = text[i..].chars().next()
            && blank(c)
        {
            i += c.len_utf8();
        }
        i
    };
    let expect = |i: usize, c: char| text[i..].starts_with(c).then_some(i + 1);
    let mut from = 0;
    while let Some(at) = text[from..].find("main").map(|k| from + k) {
        from = at + 1;
        let Some(i) = expect(skip(at + 4), '(') else { continue };
        let Some(i) = expect(skip(i), ')') else { continue };
        let Some(i) = expect(skip(i), '{') else { continue };
        if let Some(nl) = text[i..].find('\n') {
            return Some(i + nl + 1);
        }
    }
    None
}

impl Porter<'_> {
    // ---- picking stock assets ---------------------------------------------------------------

    /// Picks a stock file's asset list entries named (type, name), with every entry they
    /// point into.
    pub(crate) fn pick(&mut self, src: &Rc<StockFile>, names: &BTreeSet<(String, String)>) -> Result<()> {
        let ents = &src.tree.assets;
        let mut pos: HashMap<NodeId, usize> = HashMap::new();
        for (i, &e) in ents.iter().enumerate() {
            pos.entry(e).or_insert(i);
        }
        let owner = match self.owners.remove(&src.id) {
            Some(owner) => owner,
            None => {
                // (Kept for the next pick from the file: both teams and the titles can come
                // from one.)
                let mut owner = HashMap::new();
                for (i, &e) in ents.iter().enumerate() {
                    let asset = entry_asset(self.st, e);
                    for_each(&mut *self.st, Roots::One(asset), |st, n| {
                        if st.as_struct(Some(n)).is_some_and(|o| o.is_asset()) {
                            owner.entry(n).or_insert(i);
                        }
                        Ok(())
                    })?;
                }
                owner
            }
        };
        let mut sel: BTreeSet<usize> = BTreeSet::new();
        for (i, &e) in ents.iter().enumerate() {
            if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)))
                && names.contains(&(entry_type(self.st, e).to_string(), name(self.st, a).unwrap_or_default()))
            {
                sel.insert(i);
            }
        }
        let mut todo: Vec<usize> = sel.iter().copied().collect();
        while let Some(at) = todo.pop() {
            let asset = entry_asset(self.st, ents[at]);
            for_each(&mut *self.st, Roots::One(asset), |st, n| {
                let Some(ch) = st.as_struct(Some(n)).and_then(|s| s.ch()) else { return Ok(()) };
                let items: Vec<Option<NodeId>> = st.children(ch).items.iter().map(|(_, c)| *c).collect();
                for c in items {
                    for x in each_held(st, c) {
                        let Some(r) = x.filter(|&r| st.is_ref(Some(r))) else { continue };
                        let target = st.reference(r).target;
                        let a = if st.is_entry(target) { target } else { asset_in_slot(st, r)? };
                        let mut j = None;
                        if let Some(a) = a.filter(|&a| st.is_entry(Some(a))) {
                            j = pos.get(&a).copied(); // (none: in a world let go)
                        } else if let Some(a) = a.filter(|&a| st.is_struct(Some(a))) {
                            j = owner.get(&a).copied();
                        }
                        if let Some(j) = j
                            && sel.insert(j)
                        {
                            todo.push(j);
                        }
                    }
                }
                Ok(())
            })?;
        }
        self.owners.insert(src.id, owner);
        self.picked.entry(src.id).or_default().extend(sel);
        Ok(())
    }

    /// Copies the entries picked from a file in their stock order (both teams of one file
    /// together: a later model can point into an earlier one's parts).
    pub(crate) fn copy_picked(&mut self, src: &Rc<StockFile>) -> Result<()> {
        let Some(sel) = self.picked.remove(&src.id) else { return Ok(()) };
        if sel.is_empty() {
            return Ok(());
        }
        self.owners.remove(&src.id); // copying changes the entries' pointers
        let mut made: Vec<Option<NodeId>> = Vec::new();
        let mut entries: Vec<NodeId> = Vec::new();
        for i in sel {
            let e = *src.tree.assets.get(i).ok_or_else(|| Error::new("a picked entry past the asset list"))?;
            entries.push(e);
            made.push(Some(e));
            self.stock_entries.insert(e);
        }
        self.localize(&made)?;
        self.remap_bones(&made, src.tree.script_strings)?;
        self.root.assets.splice(0..0, entries);
        Ok(())
    }

    /// The calling card titles and emblems a stock map carries: in a match the game draws
    /// them from the map's own copy, not ui_mp's. They come as stock maps have them, their
    /// pixels streamed from the console's picture packs.
    pub(crate) fn pick_card_pictures(&mut self) -> Result<()> {
        let mut have: BTreeSet<(String, String)> = BTreeSet::new();
        for &e in self.root.assets.iter() {
            if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a))) {
                have.insert((entry_type(self.st, e).to_string(), name(self.st, a).unwrap_or_default()));
            }
        }
        for f in self.refs.clone() {
            if !f.base.starts_with("mp_") || f.base.ends_with("_load") {
                continue;
            }
            let mut names: BTreeSet<(String, String)> = BTreeSet::new();
            for &e in &f.tree.assets {
                let a = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)));
                let n = match a {
                    Some(a) => name(self.st, a).unwrap_or_default(),
                    None => String::new(),
                };
                if entry_type(self.st, e) == "material"
                    && (n.starts_with("cardtitle_") || n.starts_with("cardicon_"))
                    && !have.contains(&("material".to_string(), n.clone()))
                {
                    names.insert(("material".to_string(), n));
                }
            }
            if names.is_empty() {
                continue;
            }
            self.pick(&f, &names)?;
            return self.say(&format!("  titles and emblems: {} from {}", names.len(), paths::file_name(&f.path)));
        }
        self.warn("no stock map given carries the calling card titles and emblems; they show as missing in matches")
    }

    /// Copies in what the map's two teams need and sets them in its script; returns the
    /// teams used.
    pub(crate) fn add_teams(&mut self, wanted: &Teams) -> Result<Teams> {
        let mut arena = String::new();
        for f in self.refs.clone() {
            for &e in &f.tree.assets {
                if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)))
                    && entry_type(self.st, e) == "rawfile"
                    && arena.is_empty()
                    && name(self.st, a).as_deref() == Some("mp/basemaps.arena")
                {
                    arena = rawfile_text(self.st, a)?;
                }
            }
        }
        struct Donor {
            file: Rc<StockFile>,
            team: &'static TeamAssets,
        }
        let mut donors: Vec<Donor> = Vec::new();
        for f in self.refs.clone() {
            let mut have: BTreeSet<(String, String)> = BTreeSet::new();
            for &e in &f.tree.assets {
                if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a))) {
                    have.insert((entry_type(self.st, e).to_string(), name(self.st, a).unwrap_or_default()));
                }
            }
            for t in TEAMS {
                let all = t.models.iter().all(|m| have.contains(&("xmodel".to_string(), m.to_string())))
                    && t.materials.iter().all(|m| have.contains(&("material".to_string(), m.to_string())));
                if all {
                    donors.push(Donor { file: f.clone(), team: t });
                }
            }
        }
        let mut used: Vec<&Donor> = Vec::new();
        for (side, want) in [("allies", wanted.allies.to_ascii_lowercase()), ("axis", wanted.axis.to_ascii_lowercase())]
        {
            let is_used = |used: &[&Donor], t: &TeamAssets| used.iter().any(|u| std::ptr::eq(u.team, t));
            let mut pick = donors.iter().find(|d| want == d.team.team);
            if pick.is_none() {
                pick = donors.iter().find(|d| on_side(side, d.team.team) && !is_used(&used, d.team));
                if pick.is_none() {
                    pick = donors.iter().find(|d| !is_used(&used, d.team));
                }
                let Some(p) = pick else {
                    return Err(Error::port(format!("no stock map given has the {want} team")));
                };
                let maps = team_maps(&arena, &want);
                self.warn(&format!(
                    "the map wants the {} team for {side}; using {} instead (for {want}, add one of these stock 360 maps: {})",
                    if want.is_empty() { "(none)" } else { &want },
                    p.team.team,
                    if maps.is_empty() { "?".to_string() } else { joined(&maps, usize::MAX, "") }
                ))?;
            }
            if let Some(p) = pick {
                used.push(p);
            }
        }
        self.pick_card_pictures()?;
        for d in &used {
            let mut names: BTreeSet<(String, String)> = BTreeSet::new();
            for m in d.team.models {
                names.insert(("xmodel".to_string(), m.to_string()));
            }
            for m in d.team.materials {
                names.insert(("material".to_string(), m.to_string()));
            }
            self.pick(&d.file, &names)?;
        }
        for d in &used {
            self.copy_picked(&d.file)?;
        }
        for f in self.refs.clone() {
            self.copy_picked(&f)?; // titles and emblems from a map no team came from
        }
        let (allies, axis) = (used[0], used[1]);
        // The 360 only knows the teams of its own maps (mp/basemaps.arena): they are set in
        // the map's script, so the game uses the assets copied in.
        let gsc = format!("maps/mp/{}.gsc", self.map_name);
        let mut set = false;
        for e in self.root.assets.clone() {
            let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a))) else { continue };
            if entry_type(self.st, e) != "rawfile" || name(self.st, a).as_deref() != Some(gsc.as_str()) {
                continue;
            }
            let text = rawfile_text(self.st, a)?;
            let Some(end) = after_main(&text) else { continue };
            let line = format!(
                "\tgame[ \"allies\" ] = \"{}\";\n\tgame[ \"axis\" ] = \"{}\";\n",
                allies.team.team, axis.team.team
            );
            set_rawfile_text(self.st, a, &format!("{}{line}{}", &text[..end], &text[end..]))?;
            set = true;
            break;
        }
        if !set {
            self.warn(&format!("couldn't set the teams in {gsc}"))?;
        }
        self.say(&format!(
            "  teams: {} (from {}) vs {} (from {})",
            allies.team.team,
            paths::file_name(&allies.file.path),
            axis.team.team,
            paths::file_name(&axis.file.path)
        ))?;
        Ok(Teams { allies: allies.team.team.to_string(), axis: axis.team.team.to_string() })
    }
}
