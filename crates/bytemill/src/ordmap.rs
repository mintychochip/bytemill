//! Insertion-ordered string map, the shape Python `dict` gives us.
//!
//! The interpreter's environment, the resolved schema's declaration tables and
//! JSON objects all need "dict order" semantics: iteration in insertion order,
//! in-place update that keeps a key's original position, O(1) lookup.

use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
    index: HashMap<String, usize>,
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        OrderedMap { entries: Vec::new(), index: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        match self.index.get(key) {
            Some(&i) => Some(&mut self.entries[i].1),
            None => None,
        }
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Insert or update in place; an existing key keeps its original position.
    pub fn insert(&mut self, key: &str, val: V) {
        match self.index.get(key) {
            Some(&i) => self.entries[i].1 = val,
            None => {
                let i = self.entries.len();
                self.entries.push((key.to_string(), val));
                self.index.insert(key.to_string(), i);
            }
        }
    }

    /// Remove a key, keeping the order of what remains.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let i = *self.index.get(key)?;
        self.index.remove(key);
        let (_, val) = self.entries.remove(i);
        for (k, _) in &self.entries[i..] {
            if let Some(slot) = self.index.get_mut(k) {
                *slot -= 1;
            }
        }
        Some(val)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn into_values(self) -> impl Iterator<Item = V> {
        self.entries.into_iter().map(|(_, v)| v)
    }

    /// `{**base, **other}`: base's entries with `other` overwriting in place.
    pub fn merged_over(base: &OrderedMap<V>, other: &OrderedMap<V>) -> OrderedMap<V>
    where
        V: Clone,
    {
        let mut out = base.clone();
        for (k, v) in other.iter() {
            out.insert(k, v.clone());
        }
        out
    }
}

impl<V> FromIterator<(String, V)> for OrderedMap<V> {
    fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
        let mut map = OrderedMap::new();
        for (k, v) in iter {
            map.insert(&k, v);
        }
        map
    }
}

impl<V> IntoIterator for OrderedMap<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}
