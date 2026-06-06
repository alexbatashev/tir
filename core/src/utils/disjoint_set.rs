pub struct DisjointSet {
    parents: Vec<i32>,
}

impl DisjointSet {
    pub fn new(size: usize) -> Self {
        DisjointSet {
            parents: vec![-1; size],
        }
    }

    pub fn find_root(&self, i: u32) -> u32 {
        assert!(i <= i32::MAX as u32);

        let mut i = i as i32;

        while self.parents[i as usize] >= 0 {
            i = self.parents[i as usize]
        }

        i as u32
    }

    pub fn union(&mut self, i: u32, j: u32) -> u32 {
        let mut root_i = self.find_root(i);
        let mut root_j = self.find_root(j);

        if root_i != root_j {
            let mut i_size = -self.parents[root_i as usize];
            let mut j_size = -self.parents[root_j as usize];

            if i_size > j_size {
                std::mem::swap(&mut root_i, &mut root_j);
                std::mem::swap(&mut i_size, &mut j_size);
            }

            self.parents[root_j as usize] -= i_size;
            self.parents[root_i as usize] = root_j as i32;
        }

        root_j
    }

    pub fn connected(&self, i: u32, j: u32) -> bool {
        self.find_root(i) == self.find_root(j)
    }
}

pub struct DisjointMap<V, F> {
    parents: Vec<i32>,
    values: Vec<Option<V>>,
    merge: F,
}

impl<V, F> DisjointMap<V, F>
where
    F: Fn(V, V) -> V,
{
    pub fn new(merge: F) -> Self {
        Self {
            parents: Vec::new(),
            values: Vec::new(),
            merge,
        }
    }

    pub fn len(&self) -> usize {
        self.parents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    pub fn push(&mut self, value: V) -> u32 {
        let id = self.parents.len() as u32;
        self.parents.push(-1);
        self.values.push(Some(value));
        id
    }

    pub fn find_root(&self, i: u32) -> u32 {
        assert!(i <= i32::MAX as u32);

        let mut i = i as i32;

        while self.parents[i as usize] >= 0 {
            i = self.parents[i as usize]
        }

        i as u32
    }

    pub fn get(&self, i: u32) -> &V {
        self.values[self.find_root(i) as usize]
            .as_ref()
            .expect("root value")
    }

    pub fn set(&mut self, i: u32, value: V) {
        let root = self.find_root(i);
        self.values[root as usize] = Some(value);
    }

    pub fn union(&mut self, i: u32, j: u32) -> u32 {
        let mut root_i = self.find_root(i);
        let mut root_j = self.find_root(j);

        if root_i != root_j {
            let mut i_size = -self.parents[root_i as usize];
            let mut j_size = -self.parents[root_j as usize];

            if i_size > j_size {
                std::mem::swap(&mut root_i, &mut root_j);
                std::mem::swap(&mut i_size, &mut j_size);
            }

            self.parents[root_j as usize] -= i_size;

            let val_i = self.values[root_i as usize].take().expect("root value");
            let val_j = self.values[root_j as usize].take().expect("root value");
            self.values[root_j as usize] = Some((self.merge)(val_j, val_i));
            self.values[root_i as usize] = None;

            self.parents[root_i as usize] = root_j as i32;
        }

        root_j
    }

    pub fn connected(&self, i: u32, j: u32) -> bool {
        self.find_root(i) == self.find_root(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_elements_are_disjoint() {
        let ds = DisjointSet::new(4);
        for i in 0..4 {
            assert_eq!(ds.find_root(i), i);
        }
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert!(!ds.connected(i, j));
            }
        }
    }

    #[test]
    fn union_merges_two_sets() {
        let mut ds = DisjointSet::new(4);
        let root = ds.union(0, 2);
        assert!(ds.connected(0, 2));
        assert!(!ds.connected(0, 1));
        assert_eq!(ds.find_root(0), root);
        assert_eq!(ds.find_root(2), root);
    }

    #[test]
    fn union_is_transitive() {
        let mut ds = DisjointSet::new(5);
        ds.union(0, 1);
        ds.union(1, 2);
        assert!(ds.connected(0, 2));
        assert!(!ds.connected(0, 3));
    }

    #[test]
    fn union_same_element_is_noop() {
        let mut ds = DisjointSet::new(3);
        assert_eq!(ds.union(1, 1), 1);
        assert_eq!(ds.find_root(1), 1);
        assert!(!ds.connected(0, 1));
    }

    #[test]
    fn repeated_union_is_idempotent() {
        let mut ds = DisjointSet::new(3);
        let first = ds.union(0, 1);
        let second = ds.union(0, 1);
        assert_eq!(first, second);
        assert!(ds.connected(0, 1));
    }

    #[test]
    fn union_links_through_parent_chain() {
        let mut ds = DisjointSet::new(4);
        ds.union(0, 1);
        ds.union(2, 3);
        let root = ds.union(1, 2);
        for i in 0..4 {
            assert_eq!(ds.find_root(i), root);
        }
    }

    #[test]
    fn map_push_and_get() {
        let mut map = DisjointMap::new(|a: i32, b| a + b);
        let a = map.push(1);
        let b = map.push(14);
        assert_eq!(map.get(a), &1);
        assert_eq!(map.get(b), &14);
    }

    #[test]
    fn map_union_merges_values() {
        let mut map = DisjointMap::new(|a: i32, b| a + b);
        let a = map.push(1);
        let b = map.push(14);
        map.union(a, b);
        assert_eq!(map.get(a), &15);
        assert_eq!(map.get(b), &15);
    }

    #[test]
    fn map_union_merges_transitively() {
        let mut map = DisjointMap::new(|a: i32, b| a + b);
        let a = map.push(1);
        let b = map.push(14);
        let c = map.push(4);
        map.union(a, b);
        map.union(a, c);
        assert_eq!(map.get(c), &19);
        assert_eq!(map.find_root(c), map.find_root(a));
    }

    #[test]
    fn map_set_updates_class() {
        let mut map = DisjointMap::new(|a: i32, b| a + b);
        let a = map.push(1);
        let b = map.push(14);
        let c = map.push(4);
        map.union(a, b);
        map.union(a, c);
        map.set(c, 42);
        assert_eq!(map.get(b), &42);
    }
}
