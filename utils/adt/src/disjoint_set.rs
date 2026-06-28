use std::cell::UnsafeCell;

// Some DisjoinSet implementations chose to perform path compression as part
// of recursive call chain, which allows them to always keep the main set tidy
// without really spending much time or memory. Recursion, however, has a hidden
// cost: if the depth of the node is too large, the recursion will overflow and
// the program will crash. Another option would be to use a queue or a vector
// and collect all items in a loop. But in case of large chains re-allocation
// times would dominate. So, instead we opt in for a partial path compression:
// only compress path for the first 16 nodes + the node we're looking for.
// In practice this should be indistinguishable from the recursion path.
const AUTOCOMPRESS_DEPTH: usize = 16;

struct DisjointSetImpl {
    parents: UnsafeCell<Vec<i32>>,
}

impl DisjointSetImpl {
    fn with_size(size: usize) -> Self {
        Self {
            parents: UnsafeCell::new(vec![-1; size]),
        }
    }

    fn new() -> Self {
        Self {
            parents: UnsafeCell::new(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        unsafe { &*self.parents.get() }.len()
    }

    fn push(&mut self) -> u32 {
        let p = unsafe { &mut *self.parents.get() };
        let id = p.len() as u32;
        p.push(-1);
        id
    }

    fn find_root(&self, i: u32) -> u32 {
        assert!(i <= i32::MAX as u32);

        let p = unsafe { &mut *self.parents.get() };

        let mut curr = i as usize;

        if p[i as usize] >= 0 {
            let mut backprop = [0; AUTOCOMPRESS_DEPTH];
            let mut ptr = 0;

            while p[curr] >= 0 {
                if ptr < backprop.len() {
                    backprop[ptr] = curr;
                    ptr += 1;
                }
                curr = p[curr] as usize;
            }

            for x in 0..ptr {
                p[backprop[x]] = curr as i32;
            }

            p[i as usize] = curr as i32;
        }

        curr as u32
    }

    fn connected(&self, i: u32, j: u32) -> bool {
        self.find_root(i) == self.find_root(j)
    }

    fn union(&mut self, i: u32, j: u32) -> (u32, u32, bool) {
        let mut root_i = self.find_root(i);
        let mut root_j = self.find_root(j);

        if root_i == root_j {
            return (root_i, root_j, false);
        }

        let p = unsafe { &mut *self.parents.get() };
        let mut i_size = -p[root_i as usize];
        let mut j_size = -p[root_j as usize];

        if i_size > j_size {
            std::mem::swap(&mut root_i, &mut root_j);
            std::mem::swap(&mut i_size, &mut j_size);
        }

        p[root_j as usize] -= i_size;
        p[root_i as usize] = root_j as i32;

        (root_i, root_j, true)
    }

    fn is_root(&self, i: u32) -> bool {
        let v = unsafe { (&*self.parents.get())[i as usize] };
        v < 0
    }
}

pub struct DisjointSet {
    uf: DisjointSetImpl,
}

impl Default for DisjointSet {
    fn default() -> Self {
        Self::empty()
    }
}

impl DisjointSet {
    pub fn new(size: usize) -> Self {
        Self {
            uf: DisjointSetImpl::with_size(size),
        }
    }

    /// An empty set that grows one element at a time via [`Self::push`].
    pub fn empty() -> Self {
        Self {
            uf: DisjointSetImpl::new(),
        }
    }

    /// Add a fresh singleton element, returning its id.
    pub fn push(&mut self) -> u32 {
        self.uf.push()
    }

    pub fn len(&self) -> usize {
        self.uf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uf.len() == 0
    }

    pub fn find_root(&self, i: u32) -> u32 {
        self.uf.find_root(i)
    }

    pub fn union(&mut self, i: u32, j: u32) -> u32 {
        self.uf.union(i, j).1
    }

    pub fn connected(&self, i: u32, j: u32) -> bool {
        self.uf.connected(i, j)
    }
}

/// A union-find with stack-disciplined assumption scopes. The base is the equality
/// that always holds; each open context layers extra unions on top that
/// [`Self::pop_context`] discards. With no context open it is exactly a
/// [`DisjointSet`].
///
/// A context relation only ever *coarsens* the base (adds unions, never splits), so
/// each layer is a [`DisjointSetImpl`] over the same id-space whose unions link the
/// canonical reps of the layer below it. [`Self::find`] is therefore bottom-up —
/// canonicalize through the base, then through each layer in turn. A top-down find
/// (descend to the base first, then re-apply layers) yields false negatives: an
/// element with no entry in a layer would return its base rep without seeing that
/// the layer redirected that rep. Every open layer is kept the same length as the
/// base (grown in lockstep by [`Self::push`]), so all indices stay in range.
pub struct ScopedDisjointSet {
    base: DisjointSetImpl,
    layers: Vec<DisjointSetImpl>,
}

impl Default for ScopedDisjointSet {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ScopedDisjointSet {
    pub fn new(size: usize) -> Self {
        Self {
            base: DisjointSetImpl::with_size(size),
            layers: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.base.len()
    }

    pub fn is_empty(&self) -> bool {
        self.base.len() == 0
    }

    /// Number of open contexts.
    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    /// Add a fresh singleton element, returning its id. Grows the base and every
    /// open layer so they stay index-aligned.
    pub fn push(&mut self) -> u32 {
        let id = self.base.push();
        for layer in &mut self.layers {
            layer.push();
        }
        id
    }

    /// Enter an assumption scope. Unions until the matching [`Self::pop_context`]
    /// are local to it.
    pub fn push_context(&mut self) {
        self.layers
            .push(DisjointSetImpl::with_size(self.base.len()));
    }

    /// Leave the current assumption scope, discarding its unions.
    pub fn pop_context(&mut self) {
        self.layers.pop();
    }

    /// Canonicalize `x` bottom-up: through the base, then each open layer in order.
    pub fn find(&self, x: u32) -> u32 {
        let mut root = self.base.find_root(x);
        for layer in &self.layers {
            root = layer.find_root(root);
        }
        root
    }

    /// Merge the classes of `x` and `y` in the innermost open scope (the base when
    /// none is open), returning the surviving root.
    pub fn union(&mut self, x: u32, y: u32) -> u32 {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry {
            return rx;
        }
        match self.layers.last_mut() {
            Some(top) => top.union(rx, ry).1,
            None => self.base.union(rx, ry).1,
        }
    }

    pub fn connected(&self, x: u32, y: u32) -> bool {
        self.find(x) == self.find(y)
    }
}

pub struct DisjointMap<V, F> {
    uf: DisjointSetImpl,
    values: Vec<Option<V>>,
    merge: F,
}

impl<V, F> DisjointMap<V, F>
where
    F: Fn(V, V) -> V,
{
    pub fn new(merge: F) -> Self {
        Self {
            uf: DisjointSetImpl::new(),
            values: Vec::new(),
            merge,
        }
    }

    pub fn len(&self) -> usize {
        self.uf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uf.len() == 0
    }

    pub fn push(&mut self, value: V) -> u32 {
        let id = self.uf.push();
        self.values.push(Some(value));
        id
    }

    pub fn find_root(&self, i: u32) -> u32 {
        self.uf.find_root(i)
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
        let (root_i, root_j, merged) = self.uf.union(i, j);
        if merged {
            let val_i = self.values[root_i as usize].take().expect("root value");
            let val_j = self.values[root_j as usize].take().expect("root value");
            self.values[root_j as usize] = Some((self.merge)(val_j, val_i));
            self.values[root_i as usize] = None;
        }
        root_j
    }

    pub fn connected(&self, i: u32, j: u32) -> bool {
        self.uf.connected(i, j)
    }

    pub fn roots(&self) -> impl Iterator<Item = (u32, &V)> {
        self.values.iter().enumerate().filter_map(|(i, value)| {
            if self.uf.is_root(i as u32) {
                Some((i as u32, value.as_ref().expect("root value")))
            } else {
                None
            }
        })
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

    #[test]
    fn scoped_with_no_context_is_plain_uf() {
        let mut uf = ScopedDisjointSet::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        assert!(uf.connected(0, 2));
        assert!(!uf.connected(0, 3));
        assert_eq!(uf.find(0), uf.find(2));
    }

    #[test]
    fn scoped_context_isolates_unions() {
        let mut uf = ScopedDisjointSet::new(4);
        uf.push_context();
        uf.union(0, 1);
        assert!(uf.connected(0, 1));
        uf.pop_context();
        assert!(!uf.connected(0, 1));
    }

    #[test]
    fn scoped_base_then_context_is_transitive() {
        let mut uf = ScopedDisjointSet::new(3);
        uf.union(0, 1);
        uf.push_context();
        uf.union(1, 2);
        assert_eq!(uf.find(2), uf.find(0));
        uf.pop_context();
        assert!(!uf.connected(0, 2));
        assert!(uf.connected(0, 1));
    }

    #[test]
    fn scoped_nested_contexts_do_not_leak() {
        let mut uf = ScopedDisjointSet::new(5);
        uf.push_context();
        uf.union(0, 1);
        uf.push_context();
        uf.union(2, 3);
        assert!(uf.connected(2, 3));
        uf.pop_context();
        assert!(!uf.connected(2, 3));
        assert!(uf.connected(0, 1));
        uf.pop_context();
        assert!(!uf.connected(0, 1));
    }

    #[test]
    fn scoped_context_union_sees_base_class_siblings() {
        // Regression for the top-down find false negative: a base class {2,3}, then
        // a context union 0≡2. find(3) must re-apply the layer to its base rep 2.
        let mut uf = ScopedDisjointSet::new(4);
        uf.union(2, 3);
        uf.push_context();
        uf.union(0, 2);
        assert!(uf.connected(0, 3));
        assert!(uf.connected(2, 3));
        assert_eq!(uf.find(0), uf.find(3));
        uf.pop_context();
        assert!(!uf.connected(0, 3));
        assert!(uf.connected(2, 3));
    }

    #[test]
    fn scoped_nested_context_closes_across_layers() {
        // L1: 1≡2. L2: 0≡1 and 2≡3 ⇒ {0,1,2,3} despite the 1≡2 step living a layer
        // below the unions that close the class.
        let mut uf = ScopedDisjointSet::new(5);
        uf.push_context();
        uf.union(1, 2);
        uf.push_context();
        uf.union(0, 1);
        uf.union(2, 3);
        let r = uf.find(0);
        assert_eq!(uf.find(1), r);
        assert_eq!(uf.find(2), r);
        assert_eq!(uf.find(3), r);
        assert!(!uf.connected(0, 4));
        uf.pop_context();
        assert!(uf.connected(1, 2));
        assert!(!uf.connected(0, 1));
        assert!(!uf.connected(2, 3));
    }

    #[test]
    fn scoped_push_grows_open_layers() {
        let mut uf = ScopedDisjointSet::new(2);
        uf.push_context();
        let c = uf.push();
        uf.union(0, c);
        assert!(uf.connected(0, c));
        uf.pop_context();
        assert!(!uf.connected(0, c));
    }
}
