/// One partition over the ids of a space.
///
/// The survivor of a merge is the smaller id, so a set's representative is the
/// minimum of its members — a function of the set, not of the order the merges
/// happened in. It also means a parent is never above its child, which is what
/// lets [`Self::flatten`] finish in one ascending pass and hand the parent
/// array out as a map from every class to its representative.
#[derive(Clone, Default)]
pub struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint an id in a set of its own.
    pub fn push(&mut self) -> u32 {
        let id = self.parent.len() as u32;
        self.parent.push(id);
        id
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    /// Grow to `len` ids, each new one in a set of its own.
    pub fn extend_to(&mut self, len: usize) {
        let from = self.parent.len() as u32;
        self.parent.extend(from..len as u32);
    }

    pub fn find(&self, id: u32) -> u32 {
        let mut cur = id;
        while self.parent[cur as usize] != cur {
            cur = self.parent[cur as usize];
        }
        cur
    }

    /// Merge the sets of `a` and `b`, returning the representative.
    pub fn union(&mut self, a: u32, b: u32) -> u32 {
        let (a, b) = (self.find(a), self.find(b));
        let (survivor, absorbed) = if a < b { (a, b) } else { (b, a) };
        self.parent[absorbed as usize] = survivor;
        survivor
    }

    /// Point every id straight at its representative.
    pub fn flatten(&mut self) {
        for i in 0..self.parent.len() {
            self.parent[i] = self.parent[self.parent[i] as usize];
        }
    }

    /// Id -> parent. After [`Self::flatten`], id -> representative.
    pub fn parents(&self) -> &[u32] {
        &self.parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn seeded(classes: u32) -> UnionFind {
        let mut uf = UnionFind::new();
        for _ in 0..classes {
            uf.push();
        }
        uf
    }

    #[test]
    fn a_set_is_represented_by_its_smallest_member() {
        let mut uf = seeded(4);
        assert_eq!(uf.union(3, 1), 1);
        assert_eq!(uf.union(2, 0), 0);
        assert_eq!(uf.union(3, 2), 0);
        for i in 0..4 {
            assert_eq!(uf.find(i), 0);
        }
    }

    #[test]
    fn the_representative_does_not_depend_on_merge_order() {
        let orders: [[(u32, u32); 3]; 2] = [[(3, 1), (2, 0), (3, 2)], [(2, 3), (0, 1), (1, 2)]];
        let roots: Vec<Vec<u32>> = orders
            .iter()
            .map(|order| {
                let mut uf = seeded(4);
                for &(a, b) in order {
                    uf.union(a, b);
                }
                (0..4).map(|i| uf.find(i)).collect()
            })
            .collect();
        assert_eq!(roots[0], roots[1]);
    }

    proptest! {
        #[test]
        fn flatten_maps_every_class_to_its_representative(pairs in prop::collection::vec((0u32..16, 0u32..16), 0..64)) {
            let mut uf = seeded(16);
            for (a, b) in pairs {
                uf.union(a, b);
            }
            let before: Vec<u32> = (0..16).map(|i| uf.find(i)).collect();
            uf.flatten();
            prop_assert_eq!(uf.parents(), &before[..]);
        }
    }
}
