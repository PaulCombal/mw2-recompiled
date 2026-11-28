// A map that keeps the order its keys were first added in. The converter keeps its
// indexes this way: which stock asset of a name it takes, and the order it goes through
// them, follow the order the stock files were read.

use std::collections::HashMap;
use std::hash::Hash;

#[derive(Clone, Debug)]
pub struct Ordered<K, V> {
    items: Vec<(K, V)>,
    index: HashMap<K, usize>,
}

impl<K, V> Default for Ordered<K, V> {
    fn default() -> Ordered<K, V> {
        Ordered { items: Vec::new(), index: HashMap::new() }
    }
}

impl<K: Hash + Eq + Clone, V> Ordered<K, V> {
    pub fn new() -> Ordered<K, V> {
        Ordered::default()
    }

    /// The value of a key, added (with `v`) when it isn't there yet.
    pub fn find_or_add(&mut self, k: K, v: V) -> &mut V {
        let at = match self.index.get(&k) {
            Some(&at) => at,
            None => {
                self.index.insert(k.clone(), self.items.len());
                self.items.push((k, v));
                self.items.len() - 1
            }
        };
        &mut self.items[at].1
    }

    /// The key's value set to `v`, keeping the key's place.
    pub fn set(&mut self, k: K, v: V) {
        match self.index.get(&k) {
            Some(&at) => self.items[at].1 = v,
            None => {
                self.index.insert(k.clone(), self.items.len());
                self.items.push((k, v));
            }
        }
    }

    pub fn find(&self, k: &K) -> Option<&V> {
        self.index.get(k).map(|&at| &self.items[at].1)
    }

    pub fn find_mut(&mut self, k: &K) -> Option<&mut V> {
        self.index.get(k).map(|&at| &mut self.items[at].1)
    }

    pub fn has(&self, k: &K) -> bool {
        self.index.contains_key(k)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The keys and values in the order the keys were added.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.items.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.items.iter_mut().map(|(k, v)| (&*k, v))
    }
}

/// An asset's place in an index: its struct and its name.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct AssetKey {
    pub ty: String,
    pub name: String,
}

impl AssetKey {
    pub fn new(ty: impl Into<String>, name: impl Into<String>) -> AssetKey {
        AssetKey { ty: ty.into(), name: name.into() }
    }
}

pub type AssetIndex<V> = Ordered<AssetKey, V>;
