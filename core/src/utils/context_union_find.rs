use std::collections::HashMap;

// Contextual union-find (Philip Zucker, https://www.philipzucker.com/context_uf2/).
// A dense base union-find plus a stack of scoped layers. Unions go into the top
// layer (or the base when no layer is open) and are discarded by `pop_context`.
// Only the top layer may be unioned into; each enclosing layer stays frozen while
// its child is open (stack discipline), which is what makes the single-pass eqset
// `find` complete.
#[derive(Default)]
struct Layer {
    // Child-local parent pointers.
    uf: HashMap<u32, u32>,
    // Child-local representative -> the other members merged into it.
    ids: HashMap<u32, Vec<u32>>,
}

pub struct ContextUnionFind {
    base: Vec<u32>,
    layers: Vec<Layer>,
}

impl ContextUnionFind {
    pub fn new(n: usize) -> Self {
        Self {
            base: (0..n as u32).collect(),
            layers: Vec::new(),
        }
    }

    pub fn add(&mut self) -> u32 {
        let id = self.base.len() as u32;
        self.base.push(id);
        id
    }

    pub fn push_context(&mut self) {
        self.layers.push(Layer::default());
    }

    pub fn pop_context(&mut self) {
        self.layers.pop();
    }

    pub fn union(&mut self, x: u32, y: u32) -> u32 {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry {
            return rx;
        }
        let (lo, hi) = (rx.min(ry), rx.max(ry));
        match self.layers.last_mut() {
            None => self.base[hi as usize] = lo,
            Some(top) => {
                top.uf.insert(hi, lo);
                let hi_members = top.ids.remove(&hi).unwrap_or_default();
                let entry = top.ids.entry(lo).or_default();
                entry.extend(hi_members);
                entry.push(hi);
            }
        }
        lo
    }

    pub fn find(&self, x: u32) -> u32 {
        self.find_upto(x, self.layers.len())
    }

    pub fn connected(&self, x: u32, y: u32) -> bool {
        self.find(x) == self.find(y)
    }

    // Canonical of `x` against `base + layers[0..level]`.
    fn find_upto(&self, mut x: u32, level: usize) -> u32 {
        if level == 0 {
            return self.find_base(x);
        }
        let layer = &self.layers[level - 1];
        while let Some(&next) = layer.uf.get(&x) {
            x = next;
        }
        match layer.ids.get(&x) {
            None => self.find_upto(x, level - 1),
            Some(members) => {
                let mut best = self.find_upto(x, level - 1);
                for &y in members {
                    best = best.min(self.find_upto(y, level - 1));
                }
                best
            }
        }
    }

    fn find_base(&self, mut x: u32) -> u32 {
        while self.base[x as usize] != x {
            x = self.base[x as usize];
        }
        x
    }

    #[cfg(test)]
    fn union_base(&mut self, x: u32, y: u32) {
        let rx = self.find_base(x);
        let ry = self.find_base(y);
        if rx == ry {
            return;
        }
        self.base[rx.max(ry) as usize] = rx.min(ry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_uf_basics_and_transitivity() {
        let mut uf = ContextUnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        assert!(uf.connected(0, 2));
        assert!(!uf.connected(0, 3));
        assert_eq!(uf.find(0), uf.find(2));
    }

    #[test]
    fn scope_isolation() {
        let mut uf = ContextUnionFind::new(4);
        uf.push_context();
        uf.union(0, 1);
        assert!(uf.connected(0, 1));
        uf.pop_context();
        assert!(!uf.connected(0, 1));
    }

    #[test]
    fn base_then_child_transitivity() {
        let mut uf = ContextUnionFind::new(3);
        uf.union(0, 1);
        uf.push_context();
        uf.union(1, 2);
        assert_eq!(uf.find(2), uf.find(0));
        uf.pop_context();
        assert!(!uf.connected(0, 2));
        assert!(uf.connected(0, 1));
    }

    #[test]
    fn nested_contexts_do_not_leak() {
        let mut uf = ContextUnionFind::new(5);
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
    fn eqset_find_avoids_false_negative() {
        let mut uf = ContextUnionFind::new(4);
        uf.push_context();
        uf.union(1, 2);
        uf.union(2, 3);
        // Mutate the frozen base while the layer is open (test-only).
        uf.union_base(0, 2);
        assert!(uf.connected(3, 0));
        assert_eq!(uf.find(3), 0);
    }
}
