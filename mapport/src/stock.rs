// What the map takes from the stock 360 files: copies of their assets made to stand alone,
// references to what the game has loaded, and the effects, models, animations and sounds
// the 360 can't run from the PC's.

use crate::error::{Error, Result};
use crate::num::{load, store};
use crate::ordered::Ordered;
use crate::porter::{ColourSwap, Porter};
use crate::query::{
    Holder, asset_in_slot, asset_name, deref_struct, each_held, holder_of, joined, name, ptr_array_value, set_holder,
    set_name,
};
use crate::tree::{Array, ElemType, FOLLOWING, INSERT, Memo, Node, NodeId, Roots, Store, Ty, collect, for_each};
use mw2_zone::defs::{Kind, POINTER};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Every pointer a node holds: a struct's children (each item of a list), or a pointer
/// list's items.
fn each_pointer(st: &Store, o: NodeId) -> Vec<Option<NodeId>> {
    let mut out = Vec::new();
    match st.node(o) {
        Node::Struct(_) => {
            if let Some(ch) = st.ch(o) {
                for (_, c) in &ch.items {
                    out.extend(each_held(st, *c));
                }
            }
        }
        Node::PtrList(pl) => {
            for &x in &pl.items {
                out.extend(each_held(st, x));
            }
        }
        _ => {}
    }
    out
}

/// What a pointer to a node holds: INSERT for a struct with a slot, else FOLLOWING.
fn marker(st: &Store, n: NodeId) -> u32 {
    if st.as_struct(Some(n)).is_some_and(|s| s.slot.is_some()) { INSERT } else { FOLLOWING }
}

/// An asset list entry's asset.
pub(crate) fn entry_asset(st: &Store, e: NodeId) -> Option<NodeId> {
    st.as_entry(Some(e)).and_then(|x| x.asset)
}

/// An asset list entry's type.
pub(crate) fn entry_type(st: &Store, e: NodeId) -> &str {
    st.as_entry(Some(e)).map_or("", |x| x.ty.as_str())
}

/// Every node reachable from `n` counts as inside.
fn take(st: &mut Store, inside: &mut HashSet<NodeId>, n: Option<NodeId>) -> Result<()> {
    for_each(st, Roots::One(n), |_, x| {
        inside.insert(x);
        Ok(())
    })
}

/// What a pointer back to earlier data `o` of a stock asset becomes once the asset stands
/// alone: itself, when it points inside, else what it points at.
fn localized(st: &mut Store, inside: &mut HashSet<NodeId>, o: NodeId) -> Result<NodeId> {
    let (target, rel, t) = {
        let r = st.reference(o);
        (r.target, r.rel, r.t.t)
    };
    if let Some(tg) = target
        && inside.contains(&tg)
    {
        // A pointer into a pointer array of an element ("the same as that one") is
        // replaced by what it points at even inside: the writer would take it for the
        // element itself. So is an asset pointer meaning "the asset that member holds",
        // which this file's order can put after the pointer.
        if let Some(v) = ptr_array_value(st, o) {
            return Ok(v);
        }
        let a = asset_in_slot(st, o)?;
        if let Some(a) = a
            && st.as_struct(Some(a)).is_some_and(|s| s.slot.is_none())
        {
            return Ok(a);
        }
        return Ok(o);
    }
    if let Some(asset) = asset_in_slot(st, o)? {
        // A pointer to another struct's pointer to the asset (how zones refer to an asset
        // loaded earlier): the asset itself.
        if st.is_ref(Some(asset)) {
            return localized(st, inside, asset);
        }
        if !inside.contains(&asset) {
            take(st, inside, Some(asset))?;
        }
        return Ok(asset);
    }
    if let Some(t) = t
        && t.compound()
        && t.size() != 0
        && let Some(list) = st.as_array(target)
    {
        let size = i64::from(t.size());
        let i = (rel / size) as usize;
        let off = rel % size;
        if off == 0 {
            // A later element of another array (stock files share the tail of one
            // argument list with another): a copy of that tail.
            let items = list.items.get(i..).map(<[_]>::to_vec).unwrap_or_default();
            let tail = st.make(Node::Array(Array { items: items.clone(), tail: true }));
            for x in items {
                take(st, inside, x)?;
            }
            inside.insert(tail);
            return Ok(tail);
        }
        // A pointer member of another array's element (stock aliases share a sound file
        // that way), or one pointer of an array of them: what that pointer holds.
        let el = list.items.get(i).copied().flatten().filter(|&e| st.is_struct(Some(e)));
        let m = t.members().iter().find(|x| {
            let at = i64::from(x.offset);
            x.mods.last() == Some(&POINTER) && at <= off && off < at + i64::from(x.size) && (off - at) % 4 == 0
        });
        if let (Some(m), Some(el)) = (m, el) {
            let mut idx: Vec<i32> = Vec::new();
            let mut flat = (off - i64::from(m.offset)) / 4;
            for d in (0..m.mods.len() - 1).rev() {
                let dim = m.mods[d];
                if dim == POINTER {
                    continue;
                }
                // (A dimension of 0 leaves the member no bytes for `off` to fall in.)
                let dim = i64::from(dim);
                let (Some(r), Some(q)) = (flat.checked_rem(dim), flat.checked_div(dim)) else {
                    return Err(Error::port("a stock asset points outside itself"));
                };
                idx.insert(0, r as i32);
                flat = q;
            }
            let mut c = st.child_at(el, &m.name, &idx);
            if c.is_none() && !idx.is_empty() {
                // (arrays of pointers are kept as one list under the member's name)
                let k = ((off - i64::from(m.offset)) / 4) as usize;
                let all = st.child(el, &m.name);
                if let Some(pl) = st.as_ptr_list(all) {
                    c = pl.items.get(k).copied().flatten();
                } else if let Some(a) = st.as_array(all) {
                    c = a.items.get(k).copied().flatten();
                }
            }
            if let Some(r) = c.filter(|&r| st.is_ref(Some(r))) {
                return localized(st, inside, r);
            }
            if let Some(s) = c.filter(|&s| st.is_struct(Some(s))) {
                take(st, inside, Some(s))?;
                return Ok(s);
            }
        }
    }
    if let Some(tg) = target
        && rel == 0
    {
        inside.insert(tg);
        take(st, inside, Some(tg))?;
        return Ok(tg);
    }
    Err(Error::port("a stock asset points outside itself"))
}

/// A material name with the stock part's colour in the map's: each `_<stock>` followed by
/// `_` or the end becomes `_<map>`.
fn recoloured(name: &str, swap: &ColourSwap) -> String {
    let from = format!("_{}", swap.stock);
    let to = format!("_{}", swap.map);
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < name.len() {
        let rest = &name[i..];
        if rest.starts_with(&from) && (rest.len() == from.len() || rest[from.len()..].starts_with('_')) {
            out += &to;
            i += from.len();
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

impl Porter<'_> {
    // ---- copies -----------------------------------------------------------------------------

    /// A copy of a stock 360 asset, safe to write into another file: pointers back to things
    /// outside it are replaced by the things themselves.
    pub(crate) fn copy_in(&mut self, src: NodeId) -> Result<NodeId> {
        self.localize(&[Some(src)])?;
        let copy = self.st.copy(src)?;
        self.st.st_mut(copy).slot = None;
        Ok(copy)
    }

    /// Makes stock assets (or asset list entries) stand alone, in place.
    pub(crate) fn localize(&mut self, roots: &[Option<NodeId>]) -> Result<()> {
        let st = &mut *self.st;
        let mut inside: HashSet<NodeId> = HashSet::new();
        for_each(st, Roots::Many(roots), |st, x| {
            inside.insert(x);
            // The slot a temp-block asset reserves counts as part of it.
            if let Some(slot) = st.as_struct(Some(x)).and_then(|s| s.slot) {
                inside.insert(slot);
            }
            Ok(())
        })?;
        for_each(st, Roots::Many(roots), |st, n| {
            let Some(ch) = st.as_struct(Some(n)).and_then(|s| s.ch()) else { return Ok(()) };
            let mut i = 0;
            while i < st.children(ch).items.len() {
                let (key, c) = st.children(ch).items[i].clone();
                let at = i;
                i += 1;
                if let Some(r) = c.filter(|&r| st.is_ref(Some(r))) {
                    let made = localized(st, &mut inside, r)?;
                    if made == r {
                        continue;
                    }
                    if let Some(slot) = st.children_mut(ch).items.get_mut(at) {
                        slot.1 = Some(made);
                    }
                    let mark = marker(st, made);
                    if key.idx.is_empty() && holder_of(st, n, key.member) != Holder::Array {
                        set_holder(st, n, key.member, mark, 0)?;
                    } else if !key.idx.is_empty() {
                        set_holder(st, n, key.member, mark, key.idx[0] as usize)?;
                    }
                } else if let Some(pl) = c.filter(|&p| st.is_ptr_list(Some(p))) {
                    let mut k = 0;
                    while k < st.ptr_list(pl).items.len() {
                        let item = st.ptr_list(pl).items[k];
                        let at = k;
                        k += 1;
                        let Some(item) = item.filter(|&r| st.is_ref(Some(r))) else { continue };
                        let made = localized(st, &mut inside, item)?;
                        if made == item {
                            continue;
                        }
                        let mark = marker(st, made);
                        let list = st.ptr_list_mut(pl);
                        list.items[at] = Some(made);
                        if let Some(raw) = &mut list.raw {
                            *raw.get_mut(at).ok_or_else(|| Error::new("a pointer list longer than its pointers"))? =
                                mark;
                        }
                        if holder_of(st, n, key.member) == Holder::Array {
                            set_holder(st, n, key.member, mark, at)?;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    /// A copy of a stock asset with what it points at, left as it is.
    pub(crate) fn stock_copy(&mut self, src: NodeId) -> Result<NodeId> {
        let made = self.copy_in(src)?;
        let done = &mut self.done;
        for_each(&mut *self.st, Roots::One(Some(made)), |st, n| {
            if st.is_struct(Some(n)) {
                done.insert(n);
            }
            Ok(())
        })?;
        Ok(made)
    }

    /// `d` becomes `made`, keeping its identity, slot and placement.
    pub(crate) fn replace(&mut self, d: NodeId, made: NodeId) -> Result<()> {
        let (slot, forward) = {
            let s = self.st.st(d);
            (s.slot, s.forward)
        };
        self.st.assign(d, made)?;
        if slot.is_some() {
            self.st.st_mut(d).slot = slot;
        }
        if forward {
            self.st.st_mut(d).forward = true;
        }
        self.done.insert(d);
        Ok(())
    }

    /// A 360 asset that only names one the game has loaded (",name").
    pub(crate) fn reference(&mut self, ty: &str, name: &str) -> Result<NodeId> {
        let x = self.x;
        let info = x.infos.get(ty).map(|&id| &x.structs[id]).ok_or_else(|| Error::new(format!("no type {ty}")))?;
        let d = self.st.zero(Ty::new(x, info.ctype), true);
        let mut holder = d;
        let mut member = "name";
        if ty == "Material" {
            holder = self.st.sub(d, "info")?;
        } else if self.st.find(d, "name").is_none() {
            member = "filename"; // (SndCurve)
        }
        self.st.set_ptr(holder, member, FOLLOWING)?;
        let s = self.st.make_str(format!(",{name}"));
        self.st.set_child(holder, member, Some(s));
        self.st.st_mut(d).asset = info.name.as_str();
        Ok(d)
    }

    /// Bone names (and an animation's notify names) are indexes into the file's script
    /// strings: moved from the stock file's (`theirs`) to ours. Left as they were they named
    /// whatever string had that number here.
    pub(crate) fn remap_bones(&mut self, made: &[Option<NodeId>], theirs: Option<NodeId>) -> Result<()> {
        let ss = match self.root.script_strings {
            Some(ss) => ss,
            None => {
                let ss = self.st.make_ptr_list(vec![None], None);
                self.root.script_strings = Some(ss);
                ss
            }
        };
        let st = &mut *self.st;
        if !st.is_ptr_list(Some(ss)) {
            return Err(Error::new("script strings that are not a list"));
        }
        let mut place: BTreeMap<Option<String>, usize> = BTreeMap::new();
        for (k, s) in st.ptr_list(ss).items.iter().enumerate() {
            place.insert(st.as_str(*s).map(|s| s.b.clone()), k);
        }
        let mut ours = |st: &mut Store, v: u64| -> usize {
            let s = theirs
                .and_then(|t| st.as_ptr_list(Some(t)))
                .and_then(|pl| usize::try_from(v).ok().and_then(|v| pl.items.get(v).copied().flatten()));
            let b = st.as_str(s).map(|s| s.b.clone());
            if let Some(&k) = place.get(&b) {
                return k;
            }
            let k = st.ptr_list(ss).items.len();
            place.insert(b.clone(), k);
            let item = b.map(|b| st.make_str(b));
            st.ptr_list_mut(ss).items.push(item);
            k
        };
        let remapped = &mut self.remapped;
        for_each(st, Roots::Many(made), |st, n| {
            let Some(o) = st.as_struct(Some(n)) else { return Ok(()) };
            let key = if o.is_asset_named("XModel") {
                "boneNames"
            } else if o.is_asset_named("XAnimParts") {
                "names"
            } else {
                // XAnimNotifyInfo: {name (a script string), time}
                let name = o.find("name");
                if o.find("time").is_some()
                    && let Some(name) = name
                    && name.mods.is_empty()
                    && !o.ty.of(name).compound()
                    && !remapped.contains(&n)
                {
                    remapped.insert(n);
                    let v = st.u(n, "name")?;
                    let k = ours(st, v);
                    st.set(n, "name", k as i64)?;
                }
                return Ok(());
            };
            let mut c = st.child(n, key);
            if let Some(r) = st.as_ref(c) {
                c = r.target;
            }
            let Some(lf) = c.filter(|&l| st.is_leaf(Some(l))) else { return Ok(()) };
            if remapped.contains(&lf) {
                return Ok(());
            }
            remapped.insert(lf);
            let (count, big) = {
                let l = st.leaf(lf);
                (l.n, l.big)
            };
            for i in 0..count {
                let at = (2 * i) as usize;
                let past = || Error::new("a list of bone names past its data");
                let v = load(st.leaf(lf).raw.get(at..at + 2).ok_or_else(past)?, 2, big);
                let k = ours(st, v);
                store(&mut st.leaf_mut(lf).raw[at..at + 2], 2, k as u64, big);
            }
            Ok(())
        })
    }

    // ---- effects --------------------------------------------------------------------------

    /// Effects a stock 360 file has under the same name come from there (with their
    /// materials) instead of being converted: converted ones can draw far stronger than on
    /// the PC (mp_backlot's dust filled the map with a yellow haze). Scripts and other assets
    /// name effects, so the stock one goes in under the name and the converted one is renamed
    /// out of the way, then left out unless the map's other assets point into it.
    pub(crate) fn stock_effects(&mut self) -> Result<()> {
        let mut made: Vec<NodeId> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut swapped: Vec<(NodeId, NodeId)> = Vec::new();
        for e in self.root.assets.clone() {
            let ty = entry_type(self.st, e).to_string();
            let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a))) else { continue };
            if ty != "fx" {
                continue;
            }
            let Some(nm) = name(self.st, a) else { continue };
            let Some(found) = self.stock_fx.find(&nm).cloned() else { continue };
            let s = self.st.make_str(format!("~pc/{nm}"));
            self.st.set_child(a, "name", Some(s));
            let fx = self.copy_in(found.asset)?;
            self.remap_bones(&[Some(fx)], found.file.tree.script_strings)?;
            self.done.insert(fx);
            made.push(self.st.make_entry(ty, Some(fx)));
            swapped.push((e, *made.last().unwrap_or(&fx)));
            self.stock_fx_used.push(fx);
            names.push(nm);
        }
        if !swapped.is_empty() {
            self.drop_swapped_effects(swapped)?;
        }
        // First in the list: the shader sets they bring are the stock ones the map's own
        // materials of those sets point at, written before them.
        self.root.assets.splice(0..0, made);
        if !names.is_empty() {
            self.say(&format!(
                "  {} effect{} from the stock 360 files: {}",
                names.len(),
                if names.len() > 1 { "s" } else { "" },
                joined(&names, usize::MAX, "")
            ))?;
        }
        Ok(())
    }

    /// The PC copies of the effects taken from stock go: each took one of the game's 600
    /// effect places next to the stock one. A pointer to one goes to the stock copy; what it
    /// brought is written where it is pointed at next. One the map's other assets point into
    /// stays (renamed).
    pub(crate) fn drop_swapped_effects(&mut self, swapped: Vec<(NodeId, NodeId)>) -> Result<()> {
        let count = swapped.len();
        let st = &mut *self.st;
        // node -> the swapped entry it is in
        let mut inside: HashMap<NodeId, usize> = HashMap::new();
        for (k, &(e, _)) in swapped.iter().enumerate() {
            let asset = entry_asset(st, e);
            for_each(st, Roots::One(asset), |st, o| {
                if Some(o) == asset {
                    return Ok(());
                }
                inside.insert(o, k);
                if let Some(slot) = st.as_struct(Some(o)).and_then(|s| s.slot) {
                    inside.insert(slot, k);
                }
                Ok(())
            })?;
        }
        let mut live = vec![true; swapped.len()];
        loop {
            let mut keep: BTreeSet<usize> = BTreeSet::new();
            for &e in self.root.assets.iter() {
                let is_swapped = swapped.iter().enumerate().any(|(k, s)| live[k] && s.0 == e);
                if is_swapped {
                    continue;
                }
                let asset = entry_asset(st, e);
                for_each(st, Roots::One(asset), |st, o| {
                    for x in each_pointer(st, o) {
                        if let Some(r) = st.as_ref(x)
                            && let Some(&k) = r.target.and_then(|t| inside.get(&t))
                            && live[k]
                        {
                            keep.insert(k);
                        }
                    }
                    Ok(())
                })?;
            }
            if keep.is_empty() {
                break;
            }
            for k in keep {
                live[k] = false;
            }
        }
        let gone: Vec<(NodeId, NodeId)> =
            swapped.iter().enumerate().filter(|(k, _)| live[*k]).map(|(_, &s)| s).collect();
        let kept = count - gone.len();
        if kept != 0 {
            self.say(&format!(
                "  {kept} PC copies of stock effects kept (renamed ~pc/...): the map's other assets point into them"
            ))?;
        }
        if gone.is_empty() {
            return Ok(());
        }
        let st = &mut *self.st;
        let mut by_obj: HashMap<NodeId, NodeId> = HashMap::new();
        for &(e, stock) in &gone {
            by_obj.insert(e, stock);
            let asset = entry_asset(st, e);
            if let Some(a) = asset {
                by_obj.insert(a, stock);
            }
            if let Some(slot) = st.as_struct(asset).and_then(|s| s.slot) {
                by_obj.insert(slot, stock);
            }
        }
        let mut moved = 0;
        // A pointer to a node going: a Ref is pointed at the stock entry in place; a struct
        // is replaced by a Ref to it (returned).
        let mut visit = |st: &mut Store, x: Option<NodeId>| -> Option<NodeId> {
            let x = x?;
            match st.node(x) {
                Node::Ref(r) => {
                    let to = *r.target.and_then(|t| by_obj.get(&t))?;
                    if r.rel != 0 && r.rel != 4 {
                        return None;
                    }
                    let r = st.reference_mut(x);
                    r.target = Some(to);
                    r.rel = 4;
                    r.t = ElemType::default();
                    moved += 1;
                    None
                }
                Node::Struct(_) => {
                    let to = *by_obj.get(&x)?;
                    let r = st.make_ref(1);
                    let rr = st.reference_mut(r);
                    rr.target = Some(to);
                    rr.rel = 4;
                    moved += 1;
                    Some(r)
                }
                _ => None,
            }
        };
        let assets = self.root.assets.clone();
        for_each(st, Roots::Entries(&assets), |st, n| {
            if let Some(ch) = st.as_struct(Some(n)).and_then(|s| s.ch()) {
                let mut i = 0;
                while i < st.children(ch).items.len() {
                    let (key, c) = st.children(ch).items[i].clone();
                    let at = i;
                    i += 1;
                    if let Some(a) = c.filter(|&a| st.is_array(Some(a))) {
                        for j in 0..st.array(a).items.len() {
                            let x = st.array(a).items[j];
                            if let Some(to) = visit(st, x) {
                                st.array_mut(a).items[j] = Some(to);
                            }
                        }
                    } else if let Some(pl) = c.filter(|&p| st.is_ptr_list(Some(p))) {
                        for j in 0..st.ptr_list(pl).items.len() {
                            let x = st.ptr_list(pl).items[j];
                            if let Some(to) = visit(st, x) {
                                st.ptr_list_mut(pl).items[j] = Some(to);
                            }
                        }
                    } else if let Some(to) = visit(st, c) {
                        st.children_mut(ch).items[at].1 = Some(to);
                        if holder_of(st, n, key.member) == Holder::One || st.st(n).ty.kind() == Kind::Union {
                            set_holder(st, n, key.member, 1, 0)?;
                        }
                    }
                }
            } else if st.is_ptr_list(Some(n)) {
                for j in 0..st.ptr_list(n).items.len() {
                    let x = st.ptr_list(n).items[j];
                    if let Some(to) = visit(st, x) {
                        st.ptr_list_mut(n).items[j] = Some(to);
                    }
                }
            }
            Ok(())
        })?;
        self.root.assets.retain(|e| !gone.iter().any(|g| g.0 == *e));
        self.say(&format!(
            "  {} PC copies of those effects left out ({moved} pointers to them now name the stock copy)",
            gone.len()
        ))
    }

    // ---- models -----------------------------------------------------------------------------

    /// Placed models a stock 360 file also has are drawn with the stock copy: it goes in
    /// first, and each placed copy points at it. The converted model is renamed and left out
    /// unless other assets point into it; a model anything but placed copies holds stays as
    /// it is.
    pub(crate) fn stock_model_swap(&mut self) -> Result<()> {
        let mut top: HashSet<NodeId> = HashSet::new();
        for &e in self.root.assets.iter() {
            if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a))) {
                top.insert(a);
            }
        }
        let mut holders: HashMap<NodeId, BTreeSet<bool>> = HashMap::new();
        let assets = self.root.assets.clone();
        for_each(&mut *self.st, Roots::Entries(&assets), |st, n| {
            let Some(ch) = st.as_struct(Some(n)).and_then(|s| s.ch()) else { return Ok(()) };
            let placement = st.find(n, "placement").is_some();
            let items: Vec<Option<NodeId>> = st.children(ch).items.iter().map(|(_, c)| *c).collect();
            for c in items {
                for x in each_held(st, c) {
                    let t = if st.is_struct(x) {
                        x
                    } else if st.is_ref(x) {
                        deref_struct(st, x)?
                    } else {
                        None
                    };
                    if let Some(t) = t
                        && st.st(t).is_asset_named("XModel")
                    {
                        holders.entry(t).or_default().insert(placement);
                    }
                }
            }
            Ok(())
        })?;
        let mut front: Vec<NodeId> = Vec::new();
        let mut moved: Vec<NodeId> = Vec::new();
        let mut swapped_models: Vec<NodeId> = Vec::new();
        let mut kinds = 0usize;
        let only_placed = BTreeSet::from([true]);
        for o in collect(self.st, Roots::Entries(&assets))? {
            if !self.st.as_struct(Some(o)).is_some_and(|s| s.is_asset_named("XModel")) || self.done.contains(&o) {
                continue;
            }
            let Some(nm) = name(self.st, o) else { continue };
            if nm.starts_with(',') {
                continue;
            }
            let Some(found) = self.stock_models.find(&nm).cloned() else { continue };
            match holders.get(&o) {
                Some(h) if *h == only_placed => {}
                _ => continue, // held by more than placed copies
            }
            let m = self.copy_in(found.asset)?;
            self.remap_bones(&[Some(m)], found.file.tree.script_strings)?;
            self.done.insert(m);
            let entry = self.st.make_entry("xmodel", Some(m));
            front.push(entry);
            self.model_swap.insert(o, entry);
            let s = self.st.make_str(format!("~pc/{nm}"));
            self.st.set_child(o, "name", Some(s));
            if !top.contains(&o) {
                moved.push(self.st.make_entry("xmodel", Some(o)));
            }
            swapped_models.push(o);
            kinds += 1;
        }
        if !swapped_models.is_empty() {
            // The converted copy goes too when nothing but placed copies points at it: each
            // took one of the game's 1,536 model places.
            let st = &mut *self.st;
            let mut inside: HashMap<NodeId, usize> = HashMap::new();
            let mut own: HashSet<NodeId> = HashSet::new();
            for (k, &sm) in swapped_models.iter().enumerate() {
                for_each(st, Roots::One(Some(sm)), |st, x| {
                    own.insert(x);
                    if x == sm {
                        return Ok(());
                    }
                    inside.insert(x, k);
                    if let Some(slot) = st.as_struct(Some(x)).and_then(|s| s.slot) {
                        inside.insert(slot, k);
                    }
                    Ok(())
                })?;
            }
            let mut keep: BTreeSet<usize> = BTreeSet::new();
            let assets = self.root.assets.clone();
            for_each(st, Roots::Entries(&assets), |st, x| {
                if own.contains(&x) {
                    return Ok(());
                }
                for y in each_pointer(st, x) {
                    if let Some(r) = st.as_ref(y)
                        && let Some(&k) = r.target.and_then(|t| inside.get(&t))
                    {
                        keep.insert(k);
                    }
                }
                Ok(())
            })?;
            let gone: HashSet<NodeId> =
                swapped_models.iter().enumerate().filter(|(k, _)| !keep.contains(k)).map(|(_, &m)| m).collect();
            moved.retain(|&e| !entry_asset(st, e).is_some_and(|a| gone.contains(&a)));
            self.root
                .assets
                .retain(|&e| !entry_asset(st, e).is_some_and(|a| st.is_struct(Some(a)) && gone.contains(&a)));
            self.say(&format!(
                "  {} converted copies of those models left out ({} kept: other assets point into them)",
                gone.len(),
                keep.len()
            ))?;
        }
        front.extend(moved);
        self.root.assets.splice(0..0, front);
        self.say(&format!("  {kinds} kinds of static model drawn with the stock 360 copy"))
    }

    // ---- animations -------------------------------------------------------------------------

    /// The 360 packs animation rotations otherwise (a quaternion in 32 bits) and has two more
    /// part types, so PC animations can't be carried over: those a stock 360 file has come
    /// from it, the rest are left out (scripts and map entities name animations; nothing
    /// points at them).
    pub(crate) fn stock_anims(&mut self) -> Result<()> {
        let mut took: Vec<String> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        let mut keep: Vec<NodeId> = Vec::new();
        for e in self.root.assets.clone() {
            let a = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)));
            let Some(a) = a.filter(|_| entry_type(self.st, e) == "xanim") else {
                keep.push(e);
                continue;
            };
            let nm = name(self.st, a).unwrap_or_default();
            if nm.starts_with(',') {
                keep.push(e);
                continue;
            }
            if self.stock_anims.is_none() {
                // By resolved name: stock files name animations through a shared string.
                let mut first: Ordered<String, NodeId> = Ordered::new();
                let mut second = HashMap::new();
                for f in self.refs.clone() {
                    for_each(&mut *self.st, Roots::Entries(&f.tree.assets), |st, n| {
                        if !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("XAnimParts")) {
                            return Ok(());
                        }
                        let nm = name(st, n).unwrap_or_default();
                        if nm.is_empty() || nm.starts_with(',') || (first.has(&nm) && f.resident) {
                            return Ok(());
                        }
                        first.find_or_add(nm, n);
                        second.entry(n).or_insert_with(|| f.clone());
                        Ok(())
                    })?;
                }
                self.stock_anims = Some((first, second));
            }
            let Some((first, second)) = &self.stock_anims else { continue };
            let Some(&src) = first.find(&nm) else {
                dropped.push(nm);
                continue;
            };
            let theirs = second
                .get(&src)
                .map(|f| f.tree.script_strings)
                .ok_or_else(|| Error::new("a stock animation without its file"))?;
            let made = self.copy_in(src)?;
            self.remap_bones(&[Some(made)], theirs)?;
            self.replace(a, made)?;
            let done = &mut self.done;
            for_each(&mut *self.st, Roots::One(Some(a)), |st, n| {
                if st.is_struct(Some(n)) {
                    done.insert(n);
                }
                Ok(())
            })?;
            keep.push(e);
            took.push(nm);
        }
        self.root.assets.clone_from(&keep);
        if !took.is_empty() {
            self.say(&format!("  {} animations from the stock 360 files", took.len()))?;
        }
        if !dropped.is_empty() {
            self.warn(&format!(
                "{} animations left out (no stock 360 copy): {}",
                dropped.len(),
                joined(&dropped, 8, " ...")
            ))?;
        }
        Ok(())
    }

    // ---- breakable parts ------------------------------------------------------------------------

    /// The models the map's destructible entities need: each copied in after the map's own
    /// assets under the name the 360's script asks for, a part taken in another colour
    /// painted with the map's material for its own colour, a material the map has under the
    /// same name taken from the map. Copies of one stock model share its materials and
    /// geometry.
    pub(crate) fn add_breakables(&mut self) -> Result<()> {
        let mut ours: Ordered<String, NodeId> = Ordered::new();
        for e in self.root.assets.clone() {
            let asset = entry_asset(self.st, e);
            for_each(&mut *self.st, Roots::One(asset), |st, n| {
                if !st.as_struct(Some(n)).is_some_and(|o| o.is_asset_named("Material")) {
                    return Ok(());
                }
                if let Some(nm) = asset_name(st, n)
                    && !nm.starts_with(',')
                {
                    ours.find_or_add(nm.to_ascii_lowercase(), n);
                }
                Ok(())
            })?;
        }
        let mut made: Vec<NodeId> = Vec::new();
        let mut painted = 0;
        for b in self.breakables.clone() {
            let file = self.refs.iter().rfind(|f| f.path == b.path).cloned();
            let mut hit = None;
            if let Some(file) = &file {
                for &e in &file.tree.assets {
                    if let Some(a) = entry_asset(self.st, e).filter(|&a| self.st.is_struct(Some(a)))
                        && entry_type(self.st, e) == "xmodel"
                        && name(self.st, a).unwrap_or_default().to_ascii_lowercase() == b.src
                    {
                        hit = Some(a);
                        break;
                    }
                }
            }
            let (Some(hit), Some(file)) = (hit, file) else { continue };
            let base = self.copy_in(hit)?;
            // A material named through another model's list ("the same as its second")
            // becomes the material itself, so the copy stands alone.
            let st = &mut *self.st;
            if let Some(ml) = st.child(base, "materialHandles").filter(|&l| st.is_ptr_list(Some(l))) {
                for i in 0..st.ptr_list(ml).items.len() {
                    let mut v = st.ptr_list(ml).items[i];
                    while let Some(r) = v.filter(|&r| st.is_ref(Some(r))) {
                        if !st.is_ptr_list(st.reference(r).target) {
                            break;
                        }
                        v = asset_in_slot(st, r)?;
                    }
                    if st.is_struct(v) {
                        st.ptr_list_mut(ml).items[i] = v;
                    }
                }
            }
            let mut shared = Memo::new();
            for_each(st, Roots::One(Some(base)), |st, n| {
                if !st.as_struct(Some(n)).is_some_and(|o| o.is_asset() && !o.is_asset_named("XModel")) {
                    return Ok(());
                }
                for_each(st, Roots::One(Some(n)), |st, x| {
                    shared.insert(x, Some(x));
                    if let Some(slot) = st.as_struct(Some(x)).and_then(|s| s.slot) {
                        shared.insert(slot, Some(slot));
                    }
                    Ok(())
                })
            })?;
            let m = st.deep_copy_struct(base, Some(&mut shared))?;
            st.st_mut(m).slot = None;
            self.remap_bones(&[Some(m)], file.tree.script_strings)?;
            set_name(self.st, m, b.need.clone());
            painted += self.paint_breakable(m, &b.swap, &ours)?;
            self.done.insert(m);
            made.push(self.st.make_entry("xmodel", Some(m)));
        }
        let at = self
            .root
            .assets
            .iter()
            .position(|&e| entry_type(self.st, e) == "gfx_map")
            .unwrap_or(self.root.assets.len());
        let count = made.len();
        self.root.assets.splice(at..at, made);
        if count != 0 {
            self.say(&format!("  {count} breakable parts copied in ({painted} paint materials the map's own)"))?;
        }
        Ok(())
    }

    /// Points a model's materials at the map's own: the one in the map's colour (swap:
    /// stock colour, map colour), else the same-named one. Returns how many changed.
    pub(crate) fn paint_breakable(
        &mut self,
        model: NodeId,
        swap: &Option<ColourSwap>,
        ours: &Ordered<String, NodeId>,
    ) -> Result<i32> {
        let st = &mut *self.st;
        let mut c = st.child(model, "materialHandles");
        if let Some(r) = st.as_ref(c) {
            c = r.target;
        }
        let Some(ml) = c.filter(|&l| st.is_ptr_list(Some(l))) else { return Ok(0) };
        let mut count = 0;
        for i in 0..st.ptr_list(ml).items.len() {
            let mut m = st.ptr_list(ml).items[i];
            if let Some(r) = m.filter(|&r| st.is_ref(Some(r))) {
                m = ptr_array_value(st, r);
                if m.is_none() {
                    let target = st.reference(r).target;
                    m = if st.is_entry(target) { target } else { asset_in_slot(st, r)? };
                }
                if let Some(e) = st.as_entry(m) {
                    m = e.asset;
                }
            }
            let ms = m.filter(|&s| st.is_struct(Some(s)));
            let n = match ms {
                Some(s) => asset_name(st, s).unwrap_or_default().to_ascii_lowercase(),
                None => String::new(),
            };
            if n.is_empty() {
                continue;
            }
            let mut mine = None;
            if let Some(swap) = swap {
                mine = ours.find(&recoloured(&n, swap)).copied();
            }
            if mine.is_none() {
                mine = ours.find(&n).copied();
            }
            if let Some(mine) = mine
                && Some(mine) != ms
            {
                st.ptr_list_mut(ml).items[i] = Some(mine);
                count += 1;
            }
        }
        Ok(count)
    }
}
