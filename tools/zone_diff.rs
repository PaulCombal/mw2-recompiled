// tools/zone_diff.py's Rust side: the zone reader (zone/src/) asked what zone_diff.cpp
// asks the C++ it replaced, printing the same text for the same answer.
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use mw2_zone::defs::{Defs, Kind, TypeId};
use mw2_zone::expr::{self, Node, Parser};
use mw2_zone::schema::{BlockId, ExprId, MemberId, Schema};
use mw2_zone::shaders::{self, BindCounts, Shader};
use mw2_zone::vertex_binding::{self, BindTables};
use mw2_zone::walker::{Place, Visitor, Walker};
use mw2_zone::{Error, Result, fastfile, rules};

fn read_file(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|_| Error::new(format!("cannot open {path}")))
}

fn text(path: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_file(path)?).into_owned())
}

// FNV-1a 64, the digests both sides print.
struct Digest {
    h: u64,
    n: u64,
}

impl Digest {
    fn new() -> Digest {
        Digest { h: 1469598103934665603, n: 0 }
    }
    fn add(&mut self, data: &[u8]) {
        for &b in data {
            self.h = (self.h ^ u64::from(b)).wrapping_mul(1099511628211);
        }
    }
    fn line(&mut self, s: &str) {
        self.add(s.as_bytes());
        self.add(b"\n");
        self.n += 1;
    }
}

// ---- defs --------------------------------------------------------------------------
const KINDS: [&str; 5] = ["prim", "enum", "struct", "union", "typedef"];

fn kind_index(k: Kind) -> usize {
    match k {
        Kind::Prim => 0,
        Kind::Enum => 1,
        Kind::Struct => 2,
        Kind::Union => 3,
        Kind::Typedef => 4,
    }
}

struct DefsDump<'d> {
    defs: &'d Defs,
    labels: HashMap<TypeId, String>,
    anonymous: Vec<TypeId>,
}

impl DefsDump<'_> {
    fn label(&mut self, t: TypeId) -> String {
        let name = &self.defs.types[t].name;
        if !name.is_empty() {
            return name.clone();
        }
        if let Some(l) = self.labels.get(&t) {
            return l.clone();
        }
        let label = format!("<anon{}>", self.anonymous.len());
        self.labels.insert(t, label.clone());
        self.anonymous.push(t);
        label
    }

    fn ty(&mut self, t: TypeId, label: &str) {
        let ty = &self.defs.types[t];
        let parent = match ty.parent {
            Some(p) => self.label(p),
            None => "-".into(),
        };
        println!(
            "type {label} kind={} size={} align={} signed={} float={} alignas={} defined={} parent={parent}",
            KINDS[kind_index(ty.kind)],
            ty.size,
            ty.align,
            u8::from(ty.is_signed),
            u8::from(ty.is_float),
            ty.alignas,
            u8::from(ty.defined)
        );
        for m in &self.defs.types[t].members {
            let mods: String = m.mods.iter().map(|x| format!("{x},")).collect();
            let tl = self.label(m.ty);
            println!(
                "  member {} type={tl} mods={mods} bits={} offset={} bitpos={} size={} align={}",
                m.name, m.bits, m.offset, m.bitpos, m.size, m.align
            );
        }
    }
}

fn defs(path: &str) -> Result<()> {
    let defs = Defs::new(&text(path)?)?;
    let mut d = DefsDump { defs: &defs, labels: HashMap::new(), anonymous: Vec::new() };
    for &t in &defs.named {
        let l = d.label(t);
        d.ty(t, &l);
    }
    let mut k = 0;
    while k < d.anonymous.len() {
        let t = d.anonymous[k];
        let l = d.label(t);
        d.ty(t, &l);
        k += 1;
    }
    let named: Vec<String> = defs.named.iter().map(|&t| d.label(t)).collect();
    println!("named {}", named.join(" "));
    let consts: BTreeMap<_, _> = defs.consts.iter().collect();
    for (name, value) in consts {
        println!("const {name} {value}");
    }
    let names: BTreeMap<_, _> = defs.by_name.iter().collect();
    for (name, &t) in names {
        println!("name {name} {}", d.label(t));
    }
    Ok(())
}

// ---- schema ------------------------------------------------------------------------
fn member_label(s: &Schema, m: Option<MemberId>) -> String {
    let Some(m) = m else { return "-".into() };
    let mi = &s.members[m];
    let parent = &s.structs[mi.parent];
    let name = if mi.name.is_empty() {
        format!("#{}", parent.members.iter().position(|&x| x == m).unwrap_or(0))
    } else {
        mi.name.clone()
    };
    format!("{}::{name}", parent.name)
}

fn node_text(n: &Node) -> String {
    let mut s = format!("({} {}", n.op as i32, n.value);
    for k in &n.kids {
        s.push(' ');
        s += &node_text(k);
    }
    s + ")"
}

fn expr_text(s: &Schema, e: Option<ExprId>) -> String {
    let Some(e) = e else { return "-".into() };
    let e = &s.exprs[e];
    let mut t = node_text(&e.root);
    for r in &e.refs {
        let _ = write!(t, " [{}{}:", if r.is_index { "index " } else { "" }, s.structs[r.info].name);
        for n in &r.names {
            t.push(' ');
            t += n;
        }
        t += " /";
        for &m in &r.chain {
            t.push(' ');
            t += &member_label(s, Some(m));
        }
        t.push(']');
    }
    if let Some(c) = e.constant {
        let _ = write!(t, " ={c}");
    }
    t
}

fn block_text(s: &Schema, b: Option<BlockId>) -> String {
    b.map_or("-".into(), |b| s.blocks[b].name.clone())
}

fn schema(folder: &str) -> Result<()> {
    let s = rules::load(&|name| text(&format!("{folder}/{name}")))?;
    for b in &s.blocks {
        println!("block {} {} {}", b.kind, b.name, u8::from(b.is_default));
    }
    let assets: BTreeMap<_, _> = s.assets.iter().collect();
    for (name, &info) in assets {
        println!("asset {name} {}", s.structs[info].name);
    }
    let infos: BTreeMap<_, _> = s.infos.iter().collect();
    for (name, &id) in infos {
        let info = &s.structs[id];
        println!(
            "struct {name} as {} ctype={} block={} asset={} allocAlign={} leaf={} union={} dynamic={}",
            info.name,
            s.defs.types[info.ctype].name,
            block_text(&s, info.block),
            info.asset,
            info.alloc_align,
            u8::from(info.is_leaf),
            u8::from(info.is_union),
            member_label(&s, info.dynamic)
        );
        for (which, list) in [("members", &info.members), ("ordered", &info.ordered), ("used", &info.used)] {
            let labels: Vec<String> = list.iter().map(|&m| member_label(&s, Some(m))).collect();
            println!("  {which}{}", labels.iter().map(|l| format!(" {l}")).collect::<String>());
        }
        for &m in &info.members {
            let mi = &s.members[m];
            println!(
                "  member {} string={} script={} reusable={} block={} allocAlign={} type={} leaf={} treat={} after={} ignored={}",
                member_label(&s, Some(m)),
                u8::from(mi.is_string),
                u8::from(mi.is_script_string),
                u8::from(mi.is_reusable),
                block_text(&s, mi.block),
                mi.alloc_align,
                mi.ty.map_or("-".to_string(), |t| s.structs[t].name.clone()),
                u8::from(mi.is_leaf),
                u8::from(s.needs_treatment(m)),
                u8::from(s.after_partial_load(m)),
                u8::from(s.ignored(m))
            );
            println!("    condition {}", expr_text(&s, mi.condition));
            if let Some(bc) = &mi.block_cond {
                println!(
                    "    blockcond {} ? {} : {}",
                    expr_text(&s, Some(bc.cond)),
                    block_text(&s, Some(bc.if_true)),
                    block_text(&s, Some(bc.if_false))
                );
            }
            for (k, &e) in &mi.counts {
                println!("    count {k} {}", expr_text(&s, Some(e)));
            }
            for (k, by) in &mi.counts_by_index {
                for (i, &e) in by {
                    println!("    count {k} [{i}] {}", expr_text(&s, Some(e)));
                }
            }
            for (k, &e) in &mi.array_size {
                println!("    arraysize {k} {}", expr_text(&s, Some(e)));
            }
        }
    }
    Ok(())
}

// ---- the walk ----------------------------------------------------------------------
struct Events<'s> {
    s: &'s Schema,
    dump: bool,
    d: Digest,
}

impl Events<'_> {
    fn emit(&mut self, line: &str) {
        if self.dump {
            println!("{line}");
        }
        self.d.line(line);
    }
}

impl Visitor for Events<'_> {
    fn on_pointer(&mut self, m: MemberId, index: i64, value: u32) -> Result<()> {
        let line = format!("P {} {index} {value:08X}", member_label(self.s, Some(m)));
        self.emit(&line);
        Ok(())
    }
    fn on_data(&mut self, m: MemberId, p: &Place, bytes: usize) -> Result<()> {
        let line = format!(
            "D {} {} {} {} {} {bytes}",
            member_label(self.s, Some(m)),
            p.block,
            p.block_pos,
            p.zone_pos,
            u8::from(p.in_zone)
        );
        self.emit(&line);
        Ok(())
    }
}

fn walk(path: &str, dump: bool) -> Result<()> {
    let zone = read_file(path)?;
    let s = rules::xbox()?;
    let mut events = Events { s, dump, d: Digest::new() };
    let mut w = Walker::new(s, &zone, &mut events);
    let result = w.walk();
    let (position, assets) = (w.position(), w.asset_count());
    drop(w);
    match result {
        Ok(()) => println!("walked {} events {:016X}, to {position}, {assets} assets", events.d.n, events.d.h),
        Err(e) => println!("walked {} events {:016X}, error: {e}", events.d.n, events.d.h),
    }
    Ok(())
}

// ---- shaders -----------------------------------------------------------------------
fn print_shaders(shaders: &[Shader]) {
    for s in shaders {
        let mut code = Digest::new();
        let bytes: Vec<u8> = s.code.iter().flat_map(|w| w.to_ne_bytes()).collect();
        code.add(&bytes);
        println!("shader {} {:016X} {} {:016X}", u8::from(s.pixel), s.hash, s.code.len(), code.h);
    }
    println!("{} shaders", shaders.len());
}

// The C++ BindTables' bytes: types, usages, strides (the host's order), swizzles, formats.
fn read_tables(path: &str) -> Result<BindTables> {
    let data = read_file(path)?;
    if data.len() != 1080 + 28 + 120 + 16 + 64 {
        return Err(Error::new("tables of the wrong size"));
    }
    let mut t =
        BindTables { types: [0; 1080], usages: [0; 28], strides: [[0; 15]; 2], swizzles: [0; 16], formats: [0; 64] };
    t.types.copy_from_slice(&data[..1080]);
    t.usages.copy_from_slice(&data[1080..1108]);
    for s in 0..2 {
        for k in 0..15 {
            let at = 1108 + 60 * s + 4 * k;
            t.strides[s][k] = u32::from_ne_bytes(data[at..at + 4].try_into().unwrap());
        }
    }
    t.swizzles.copy_from_slice(&data[1228..1244]);
    t.formats.copy_from_slice(&data[1244..1308]);
    Ok(t)
}

fn image(path: &str) -> Result<Vec<u8>> {
    mw2_install::xex::image(&read_file(path)?).map_err(Error)
}

fn executable(path: &str, multiplayer: bool) -> Result<()> {
    let image = image(path)?;
    let t = BindTables::read(&image, multiplayer)?;
    let mut line = String::from("tables");
    for b in t.types.iter().chain(&t.usages) {
        let _ = write!(line, " {b:02X}");
    }
    for v in t.strides.iter().flatten() {
        let _ = write!(line, " {v:08X}");
    }
    for b in t.swizzles.iter().chain(&t.formats) {
        let _ = write!(line, " {b:02X}");
    }
    println!("{line}");
    let mv = vertex_binding::move_declaration(&image, multiplayer)?;
    println!("move{}", mv.iter().map(|b| format!(" {b:02X}")).collect::<String>());
    print_shaders(&shaders::executable_shaders(&image, multiplayer, &t)?);
    Ok(())
}

fn counts(c: &BindCounts) {
    println!("counts {} {} {} {}", c.bound, c.unbound, c.no_declaration, c.own_shader);
}

fn fastfile_shaders(tables: &str, path: &str) -> Result<()> {
    let (list, c) = shaders::shaders(&fastfile::read(Path::new(path))?, &read_tables(tables)?)?;
    print_shaders(&list);
    counts(&c);
    Ok(())
}

// ---- damage ------------------------------------------------------------------------
// splitmix64, as zone_diff.cpp has it.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

// zone_diff.cpp's Damage, change for change.
fn damage(zone: &mut Vec<u8>, seed: u64) {
    let mut r = Random(seed);
    let changes = 1 + r.below(4);
    let mut c = 0;
    while c < changes && zone.len() > 64 {
        let from = if r.below(16) == 0 { 0 } else { 56 };
        let reach = [4096, 65536, zone.len() as u64];
        let span = reach[r.below(3) as usize].min(zone.len() as u64 - from - 4);
        let at = (from + r.below(span)) as usize;
        match r.below(5) {
            0 => zone[at] ^= 1u8 << r.below(8),
            1 => zone[at] = r.next() as u8,
            2 => {
                const WORDS: [u32; 8] = [0, 0xFFFF_FFFF, 0xFFFF_FFFE, 1, 2, 0x7FFF_FFFF, 0x8000_0000, 0x1000_0000];
                let v = match r.below(3) {
                    0 => WORDS[r.below(8) as usize],
                    1 => r.below(300) as u32,
                    _ => r.next() as u32,
                };
                let w = at & !3;
                zone[w..w + 4].copy_from_slice(&v.to_be_bytes());
            }
            3 => zone[at] = zone[at].wrapping_add(1),
            _ => zone.truncate(at),
        }
        c += 1;
    }
}

fn mutate(path: &str, tables: &str, seed: u64) -> Result<()> {
    let mut zone = read_file(path)?;
    damage(&mut zone, seed);
    let mut z = Digest::new();
    z.add(&zone);
    println!("zone {} {:016X}", zone.len(), z.h);
    match shaders::shaders(&zone, &read_tables(tables)?) {
        Ok((list, c)) => {
            let mut all = Digest::new();
            for s in &list {
                all.add(&s.hash.to_ne_bytes());
                let bytes: Vec<u8> = s.code.iter().flat_map(|w| w.to_ne_bytes()).collect();
                all.add(&bytes);
            }
            println!("ok {} {:016X}", list.len(), all.h);
            counts(&c);
        }
        Err(e) => println!("error {e}"),
    }
    Ok(())
}

// ---- expressions -------------------------------------------------------------------
fn expressions(path: &str) -> Result<()> {
    let text = text(path)?;
    let names: HashMap<&str, i64> =
        [("a", 3), ("b", -7), ("c", 0), ("d", 1i64 << 40), ("e", i64::MAX), ("f", 1)].into();
    let lines: Vec<&str> = text.split('\n').collect();
    let count = if text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
    for line in &lines[..count] {
        for paths in [false, true] {
            let toks = expr::tokens(line, paths);
            println!("tokens{}{}", u8::from(paths), toks.iter().map(|t| format!(" «{t}»")).collect::<String>());
        }
        let toks = expr::tokens(line, true);
        for floor in [false, true] {
            let mut ident = |p: &mut Parser, at: &mut usize| -> Result<Node> {
                let name = &p.toks[*at];
                let value =
                    *names.get(name.as_str()).ok_or_else(|| Error::new(format!("unknown constant '{name}'")))?;
                *at += 1;
                Ok(Node::number(value))
            };
            let parsed = Parser::new(&toks).parse(0, toks.len(), &mut ident);
            match parsed.and_then(|root| Ok((root.eval(&|_, _| Ok(0), floor)?, root))) {
                Ok((value, root)) => println!("value{} {} = {value}", u8::from(floor), node_text(&root)),
                Err(e) => println!("value{} error {e}", u8::from(floor)),
            }
        }
    }
    Ok(())
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    let result = match a.as_slice() {
        ["defs", path] => defs(path),
        ["schema", folder] => schema(folder),
        ["walk", path] => walk(path, false),
        ["walk", path, _] => walk(path, true),
        ["fastfile", path] => {
            match fastfile::read(Path::new(path)) {
                Ok(zone) => {
                    let mut d = Digest::new();
                    d.add(&zone);
                    println!("zone {} {:016X}", zone.len(), d.h);
                }
                Err(e) => println!("error {e}"),
            }
            Ok(())
        }
        ["executable", path, mp] => executable(path, *mp == "1"),
        ["shaders", tables, path] => fastfile_shaders(tables, path),
        ["mutate", path, tables, seed] => mutate(path, tables, seed.parse().unwrap_or(0)),
        ["expr", path] => expressions(path),
        _ => {
            eprintln!("usage: see tools/zone_diff.cpp");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        println!("error {e}");
    }
}
