use std::{borrow::Borrow, collections::HashMap, hash::Hash, marker::PhantomData};

use crate::AsIndex;

#[derive(Clone, Debug, Default)]
pub struct IndexMap<K: Eq + Hash, V: Clone, I: AsIndex = u32> {
    indices: HashMap<K, I>,
    data: Vec<Option<V>>,
}

impl<K: Eq + Hash, V: Clone, I: AsIndex> IndexMap<K, V, I> {
    pub fn new() -> Self {
        Self {
            indices: HashMap::new(),
            data: Vec::new(),
        }
    }

    pub fn get<Q>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.indices
            .get(k)
            .and_then(|idx| self.data[idx.as_index()].as_ref())
    }

    pub fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.indices
            .get(k)
            .and_then(|idx| self.data[idx.as_index()].as_mut())
    }

    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        let old = self.get(&k).cloned();
        if old.is_some() {
            let old_idx = self.indices.get(&k).unwrap().as_index();
            self.data[old_idx] = Some(v);
        } else {
            self.data.push(Some(v));
            self.indices.insert(k, I::from_usize(self.data.len() - 1));
        }
        old
    }

    pub fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let old = self.get(k).cloned();
        if old.is_some() {
            let old_idx = self.indices.remove(k).unwrap().as_index();
            self.data[old_idx] = None;
        }

        old
    }

    pub fn contains_key<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.indices.contains_key(k)
    }

    pub fn entry(&mut self, k: K) -> Entry<'_, K, V, I> {
        match self.indices.get(&k) {
            Some(idx) => Entry::Occupied(OccupiedEntry {
                key: k,
                idx: idx.as_index(),
                data: &mut self.data,
                _marker: PhantomData,
            }),
            None => Entry::Vacant(VacantEntry {
                key: k,
                indices: &mut self.indices,
                data: &mut self.data,
                _marker: PhantomData,
            }),
        }
    }
}

pub enum Entry<'a, K: Eq + Hash, V: Clone, I: AsIndex> {
    Occupied(OccupiedEntry<'a, K, V, I>),
    Vacant(VacantEntry<'a, K, V, I>),
}

impl<'a, K: Eq + Hash, V: Clone, I: AsIndex> Entry<'a, K, V, I> {
    pub fn key(&self) -> &K {
        match self {
            Entry::Occupied(entry) => &entry.key,
            Entry::Vacant(entry) => &entry.key,
        }
    }

    pub fn and_modify<F>(self, f: F) -> Self
    where
        F: FnOnce(&mut V),
    {
        match self {
            Entry::Occupied(entry) => {
                let idx = entry.idx;
                let data = &mut *entry.data;
                if let Some(value) = data.get_mut(idx).and_then(Option::as_mut) {
                    f(value)
                }
                Entry::Occupied(entry)
            }
            Entry::Vacant(entry) => Entry::Vacant(entry),
        }
    }

    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Entry::Occupied(entry) => entry.or_insert(default),
            Entry::Vacant(entry) => entry.or_insert(default),
        }
    }

    pub fn or_insert_with<F>(self, default: F) -> &'a mut V
    where
        F: FnOnce() -> V,
    {
        match self {
            Entry::Occupied(entry) => entry.or_insert_with(default),
            Entry::Vacant(entry) => entry.or_insert_with(default),
        }
    }

    pub fn or_insert_with_key<F>(self, default: F) -> &'a mut V
    where
        F: FnOnce(&K) -> V,
    {
        match self {
            Entry::Occupied(entry) => entry.or_insert_with_key(default),
            Entry::Vacant(entry) => entry.or_insert_with_key(default),
        }
    }

    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }

    pub fn insert(self, value: V) -> Option<V> {
        match self {
            Entry::Occupied(entry) => entry.insert(value),
            Entry::Vacant(entry) => {
                entry.or_insert(value);
                None
            }
        }
    }
}

impl<K: Eq + Hash, V: Clone, I: AsIndex> IndexMap<K, V, I> {
    pub fn clear(&mut self) {
        self.indices.clear();
        self.data.clear();
    }
}

pub struct IntoIter<K: Eq + Hash, V: Clone, I: AsIndex> {
    indices: std::collections::hash_map::IntoIter<K, I>,
    data: Vec<Option<V>>,
    _marker: PhantomData<I>,
}

impl<K: Eq + Hash, V: Clone, I: AsIndex> Iterator for IntoIter<K, V, I> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        for (key, idx) in self.indices.by_ref() {
            if let Some(value) = self.data[idx.as_index()].take() {
                return Some((key, value));
            }
        }

        None
    }
}

impl<K: Eq + Hash, V: Clone, I: AsIndex> IntoIterator for IndexMap<K, V, I> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, I>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            indices: self.indices.into_iter(),
            data: self.data,
            _marker: PhantomData,
        }
    }
}

pub struct OccupiedEntry<'a, K, V, I: AsIndex> {
    key: K,
    idx: usize,
    data: &'a mut Vec<Option<V>>,
    _marker: PhantomData<(K, I)>,
}

impl<'a, K: Eq + Hash, V: Clone, I: AsIndex> OccupiedEntry<'a, K, V, I> {
    fn or_insert(self, _default: V) -> &'a mut V {
        let OccupiedEntry { idx, data, .. } = self;

        data[idx].as_mut().expect("index map has occupied entry")
    }

    fn or_insert_with<F>(self, _default: F) -> &'a mut V
    where
        F: FnOnce() -> V,
    {
        let OccupiedEntry { idx, data, .. } = self;

        data[idx].as_mut().expect("index map has occupied entry")
    }

    fn or_insert_with_key<F>(self, _default: F) -> &'a mut V
    where
        F: FnOnce(&K) -> V,
    {
        let OccupiedEntry { idx, data, .. } = self;

        data[idx].as_mut().expect("index map has occupied entry")
    }

    fn insert(self, value: V) -> Option<V> {
        let OccupiedEntry { idx, data, .. } = self;
        data[idx].replace(value)
    }
}

pub struct VacantEntry<'a, K, V, I: AsIndex> {
    key: K,
    indices: &'a mut HashMap<K, I>,
    data: &'a mut Vec<Option<V>>,
    _marker: PhantomData<I>,
}

impl<'a, K: Eq + Hash, V: Clone, I: AsIndex> VacantEntry<'a, K, V, I> {
    fn or_insert(self, value: V) -> &'a mut V {
        let idx = self.data.len();
        self.data.push(Some(value));
        self.indices.insert(self.key, I::from_usize(idx));
        self.data[idx].as_mut().expect("index map inserted value")
    }

    fn or_insert_with<F>(self, default: F) -> &'a mut V
    where
        F: FnOnce() -> V,
    {
        self.or_insert(default())
    }

    fn or_insert_with_key<F>(self, default: F) -> &'a mut V
    where
        F: FnOnce(&K) -> V,
    {
        let value = default(&self.key);
        self.or_insert(value)
    }
}

#[cfg(test)]
mod tests {
    use super::IndexMap;

    #[test]
    fn entry_or_default_inserts_if_missing() {
        let mut map: IndexMap<u8, Vec<u8>> = IndexMap::new();

        let default = map.entry(1).or_default();
        default.push(7);

        assert_eq!(map.get(&1), Some(&vec![7]));
    }

    #[test]
    fn entry_occupied_or_insert_keeps_value() {
        let mut map: IndexMap<String, Vec<u8>, u32> = IndexMap::new();
        map.insert("k".to_string(), vec![1]);

        let value = map.entry("k".to_string()).or_insert(vec![2]);
        value.push(3);

        assert_eq!(map.get("k"), Some(&vec![1, 3]));
    }

    #[test]
    fn entry_or_insert_replaces_only_when_vacant() {
        let mut map: IndexMap<&str, i32, u32> = IndexMap::new();
        map.insert("k", 10);

        let old = map.entry("k").or_insert(20);
        *old = 30;
        assert_eq!(map.get("k"), Some(&30));

        let inserted = map.entry("l").or_insert(40);
        assert_eq!(inserted, &40);
        assert_eq!(map.get("l"), Some(&40));
    }

    #[test]
    fn clear_removes_all_entries() {
        let mut map: IndexMap<&str, i32, u32> = IndexMap::new();
        map.insert("a", 1);
        map.insert("b", 2);

        map.clear();

        assert!(map.get(&"a").is_none());
        assert!(map.get(&"b").is_none());
    }

    #[test]
    fn into_iter_consumes_entries() {
        let mut map: IndexMap<&str, i32, u32> = IndexMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        map.remove(&"b");

        let mut entries: Vec<_> = map.into_iter().collect();
        entries.sort_by_key(|(k, _)| *k);

        assert_eq!(entries, vec![("a", 1)]);
    }

    #[test]
    fn contains_key_works_like_hashmap() {
        let mut map: IndexMap<&str, i32, u32> = IndexMap::new();
        map.insert("a", 1);

        assert!(map.contains_key("a"));
        assert!(!map.contains_key("b"));

        map.remove(&"a");
        assert!(!map.contains_key("a"));
    }
}
