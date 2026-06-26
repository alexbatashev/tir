//! A hash-consing e-graph with congruence closure
//! Mostly based on egg (<https://github.com/egraphs-good/egg>,
//! MIT License, Copyright Max Willsey) and <https://www.philipzucker.com/egraph-1/>.
//!
//! E-nodes ([`ENode`]) carry their operands as child [`Id`]s; the e-graph interns
//! them ([`EGraph::add`]) so structurally identical nodes share an e-class, and
//! restores congruence after [`EGraph::union`] via deferred [`EGraph::rebuild`].

mod eclass;
mod enode;
mod pattern;
mod rewrite;
mod runner;
#[cfg(test)]
mod test_lang;

use std::collections::HashMap;

use tir_adt::DisjointSet;

pub use eclass::*;
pub use enode::*;
pub use pattern::*;
pub use rewrite::*;
pub use runner::*;

/// Identifier of an e-class, and how children reference one. May be non-canonical
/// after unions — pass through [`EGraph::find`] before comparing.
#[derive(Clone, Copy, Hash, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Id(u32);

impl Id {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    fn from_raw(raw: u32) -> Self {
        Id(raw)
    }
}

pub struct EGraph<L: ENode> {
    /// Equivalence of class ids. The sole authority on what is equal — a future
    /// colored e-graph layers per-color refinements here without touching `memo`
    /// or `classes`, since every comparison flows through [`Self::find`].
    unionfind: DisjointSet,
    /// Hash-cons index: [`ENode::hash_cons`] bucket -> `[(canonical node, class)]`.
    /// A node is present iff some entry has `matches` + equal canonical children,
    /// so collisions only share a bucket and never merge distinct nodes.
    memo: HashMap<u64, Vec<(L, Id)>>,
    /// Canonical class id -> its e-class. Absorbed ids are removed on `union`.
    classes: HashMap<Id, EClass<L>>,
    /// Classes touched by a `union` since the last `rebuild`, awaiting congruence
    /// repair.
    pending: Vec<Id>,
}

impl<L: ENode> Default for EGraph<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: ENode> EGraph<L> {
    pub fn new() -> Self {
        Self {
            unionfind: DisjointSet::empty(),
            memo: HashMap::new(),
            classes: HashMap::new(),
            pending: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Total number of e-nodes across all classes.
    pub fn total_size(&self) -> usize {
        self.classes.values().map(EClass::len).sum()
    }

    pub fn num_classes(&self) -> usize {
        self.classes.len()
    }

    /// Canonicalize `id` to its class root.
    pub fn find(&self, id: Id) -> Id {
        Id::from_raw(self.unionfind.find_root(id.0))
    }

    pub fn connected(&self, a: Id, b: Id) -> bool {
        self.find(a) == self.find(b)
    }

    pub fn class(&self, id: Id) -> &EClass<L> {
        self.classes.get(&self.find(id)).expect("live e-class")
    }

    pub fn classes(&self) -> impl Iterator<Item = &EClass<L>> + '_ {
        self.classes.values()
    }

    /// The e-nodes of `id`'s class. Their child ids may be non-canonical after
    /// unions — resolve with [`Self::find`].
    pub fn nodes(&self, id: Id) -> &[L] {
        self.class(id).nodes()
    }

    /// Intern `node`, returning its e-class. Canonicalizes children, then
    /// hash-conses: a non-unique node structurally equal to an existing one shares
    /// its class; otherwise (and always for [`ENode::is_unique`] nodes) a fresh
    /// class is made.
    pub fn add(&mut self, mut node: L) -> Id {
        self.canonicalize(&mut node);
        if !node.is_unique()
            && let Some(existing) = self.memo_find(&node)
        {
            return existing;
        }
        self.make_class(node)
    }

    /// The class of an already-interned `node`, or `None`. Never inserts; always
    /// `None` for a unique node.
    pub fn lookup(&self, node: &L) -> Option<Id> {
        if node.is_unique() {
            return None;
        }
        let mut node = node.clone();
        self.canonicalize(&mut node);
        self.memo_find(&node)
    }

    /// Merge the classes of `a` and `b`, returning the surviving canonical id.
    /// Congruence repair is deferred to [`Self::rebuild`].
    pub fn union(&mut self, a: Id, b: Id) -> Id {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return ra;
        }
        let survivor = Id::from_raw(self.unionfind.union(ra.0, rb.0));
        let absorbed = if survivor == ra { rb } else { ra };
        let mut taken = self.classes.remove(&absorbed).expect("absorbed e-class");
        let surv = self.classes.get_mut(&survivor).expect("surviving e-class");
        surv.nodes.append(&mut taken.nodes);
        surv.parents.append(&mut taken.parents);
        self.pending.push(survivor);
        survivor
    }

    /// Restore the congruence invariant to a fixpoint after a batch of unions, and
    /// re-canonicalize the hash-cons.
    pub fn rebuild(&mut self) {
        while let Some(id) = self.pending.pop() {
            let id = self.find(id);
            self.repair(id);
        }
    }

    fn class_mut(&mut self, id: Id) -> &mut EClass<L> {
        let root = self.find(id);
        self.classes.get_mut(&root).expect("live e-class")
    }

    /// Rewrite a node's children to their roots; returns whether any changed.
    fn canonicalize(&self, node: &mut L) -> bool {
        let mut changed = false;
        for child in node.children_mut() {
            let root = self.find(*child);
            if root != *child {
                *child = root;
                changed = true;
            }
        }
        changed
    }

    /// The class of a canonical `node` already in the memo, or `None`.
    fn memo_find(&self, node: &L) -> Option<Id> {
        let bucket = self.memo.get(&node.hash_cons())?;
        bucket
            .iter()
            .find(|(stored, _)| stored.matches(node) && stored.children() == node.children())
            .map(|&(_, id)| self.find(id))
    }

    /// Insert or update the memo entry for a canonical `node`.
    fn memo_insert(&mut self, node: L, id: Id) {
        let bucket = self.memo.entry(node.hash_cons()).or_default();
        match bucket
            .iter_mut()
            .find(|(stored, _)| stored.matches(&node) && stored.children() == node.children())
        {
            Some(slot) => slot.1 = id,
            None => bucket.push((node, id)),
        }
    }

    /// Drop the memo entry for a (possibly stale) `node`, if present.
    fn memo_remove(&mut self, node: &L) {
        let key = node.hash_cons();
        let Some(bucket) = self.memo.get_mut(&key) else {
            return;
        };
        if let Some(pos) = bucket
            .iter()
            .position(|(stored, _)| stored.matches(node) && stored.children() == node.children())
        {
            bucket.swap_remove(pos);
        }
        if bucket.is_empty() {
            self.memo.remove(&key);
        }
    }

    /// Make a fresh singleton class for an already-canonical `node`: register it as
    /// a parent of each distinct child class and (unless unique) memoize it.
    fn make_class(&mut self, node: L) -> Id {
        let id = Id::from_raw(self.unionfind.push());
        let mut seen: Vec<Id> = Vec::new();
        for &child in node.children() {
            let child = self.find(child);
            if !seen.contains(&child) {
                seen.push(child);
                self.classes
                    .get_mut(&child)
                    .expect("child e-class")
                    .parents
                    .push((node.clone(), id));
            }
        }
        if !node.is_unique() {
            self.memo_insert(node.clone(), id);
        }
        self.classes.insert(id, EClass::new(id, node));
        id
    }

    /// Congruence repair for one class: re-canonicalize the e-nodes that reference
    /// it (its `parents`), refresh their memo entries, and union any that have
    /// become structurally equal (which queues more work via `union`).
    fn repair(&mut self, id: Id) {
        let id = self.find(id);
        let parents = std::mem::take(&mut self.class_mut(id).parents);

        for (p_node, _) in &parents {
            if !p_node.is_unique() {
                self.memo_remove(p_node);
            }
        }

        let mut new_parents: Vec<(L, Id)> = Vec::new();
        let mut index: HashMap<u64, Vec<usize>> = HashMap::new();
        for (mut p_node, p_class) in parents {
            self.canonicalize(&mut p_node);
            let p_class = self.find(p_class);
            if p_node.is_unique() {
                new_parents.push((p_node, p_class));
                continue;
            }
            let slot = index.entry(p_node.hash_cons()).or_default();
            let congruent = slot.iter().copied().find(|&i| {
                new_parents[i].0.matches(&p_node)
                    && new_parents[i].0.children() == p_node.children()
            });
            match congruent {
                Some(i) => {
                    let kept = new_parents[i].1;
                    self.union(kept, p_class);
                }
                None => {
                    slot.push(new_parents.len());
                    self.memo_insert(p_node.clone(), p_class);
                    new_parents.push((p_node, p_class));
                }
            }
        }

        // Extend rather than assign: a `union` above may merge into this very
        // class and append parents to it; those would be lost by an assignment.
        // The merge re-queued this class, so the duplicates dedup on the next pass.
        let root = self.find(id);
        self.class_mut(root).parents.extend(new_parents);
    }
}

#[cfg(test)]
mod tests {
    use super::test_lang::*;
    use super::*;

    #[test]
    fn hash_consing_shares_identical_expressions() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let e1 = add(&mut g, a, b);
        let e2 = add(&mut g, a, b);
        assert_eq!(g.find(e1), g.find(e2));
        assert_eq!(g.nodes(e1).len(), 1);
        assert_eq!(g.total_size(), 3);
        assert_eq!(g.num_classes(), 3);
    }

    #[test]
    fn lookup_probes_without_inserting() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        assert!(g.lookup(&Math::Add([a, b])).is_none());
        assert_eq!(g.num_classes(), 2);
        let e = add(&mut g, a, b);
        assert_eq!(g.lookup(&Math::Add([a, b])), Some(g.find(e)));
    }

    #[test]
    fn union_merges_classes() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = num(&mut g, 7);
        let c = num(&mut g, 9);
        assert_eq!(g.num_classes(), 3);
        g.union(a, b);
        assert!(g.connected(a, b));
        assert!(!g.connected(a, c));
        assert_eq!(g.num_classes(), 2);
    }

    #[test]
    fn congruence_merges_function_applications() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        let fa = neg(&mut g, a);
        let fb = neg(&mut g, b);
        let fc = neg(&mut g, c);

        assert_ne!(g.find(fa), g.find(fb));
        g.union(a, b);
        g.rebuild();
        assert_eq!(g.find(fa), g.find(fb));
        assert_ne!(g.find(fb), g.find(fc));

        g.union(a, c);
        g.rebuild();
        assert_eq!(g.find(fc), g.find(fb));
    }

    #[test]
    fn rebuild_propagates_congruence_to_fixpoint() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let mut cur = a;
        for _ in 0..5 {
            cur = neg(&mut g, cur);
        }
        let fa = neg(&mut g, a);
        assert_eq!(g.num_classes(), 6);
        g.union(fa, a);
        g.rebuild();
        assert_eq!(g.num_classes(), 1);
    }

    #[test]
    fn hash_collision_keeps_distinct_nodes_separate() {
        // Num(1) and Num(2) share a hash_cons bucket but must not merge.
        let mut g = EGraph::new();
        let n1 = num(&mut g, 1);
        let n2 = num(&mut g, 2);
        let n1b = num(&mut g, 1);
        assert_eq!(g.find(n1), g.find(n1b));
        assert_ne!(g.find(n1), g.find(n2));
        assert_eq!(g.num_classes(), 2);
    }

    #[test]
    fn unique_nodes_never_share_or_merge() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let e1 = g.add(Math::Effect(0, [a]));
        let e2 = g.add(Math::Effect(0, [a]));
        assert_ne!(g.find(e1), g.find(e2));
        assert_eq!(g.num_classes(), 3);

        // Effects over operands that later merge still do not congruence-merge,
        // but their operand ids resolve through `find`.
        let b = sym(&mut g, 1);
        let ua = g.add(Math::Effect(1, [a]));
        let ub = g.add(Math::Effect(1, [b]));
        g.union(a, b);
        g.rebuild();
        assert_ne!(g.find(ua), g.find(ub));
        let child = g.nodes(ua)[0].children()[0];
        assert!(g.connected(child, a));
    }
}
