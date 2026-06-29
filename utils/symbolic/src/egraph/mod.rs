//! A hash-consing e-graph with congruence closure
//! Mostly based on egg (<https://github.com/egraphs-good/egg>,
//! MIT License, Copyright Max Willsey) and <https://www.philipzucker.com/egraph-1/>.
//!
//! E-nodes ([`ENode`]) carry their operands as child [`Id`]s; the e-graph interns
//! them ([`EGraph::add`]) so structurally identical nodes share an e-class, and
//! restores congruence after [`EGraph::union`] via deferred [`EGraph::rebuild`].

mod eclass;
mod enode;
mod extract;
mod pattern;
mod rewrite;
mod runner;
#[cfg(test)]
mod test_lang;

use std::collections::HashMap;

use tir_adt::ScopedDisjointSet;

pub use eclass::*;
pub use enode::*;
pub use extract::*;
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
    /// Equivalence of class ids and the sole authority on what is equal: every
    /// comparison flows through [`Self::find`]. A [`Self::push_context`] scope
    /// layers extra unions here that [`Self::pop_context`] discards.
    unionfind: ScopedDisjointSet,
    /// Base-scope hash-cons index: [`ENode::hash_cons`] bucket ->
    /// `[(canonical node, class)]`. A node is present iff some entry has `matches`
    /// and equal canonical children, so collisions only share a bucket and never
    /// merge distinct nodes.
    memo: HashMap<u64, Vec<(L, Id)>>,
    /// Canonical base class id -> its e-class. Absorbed ids are removed on `union`.
    /// Scoped unions never touch it, so a `pop_context` restores it for free.
    classes: HashMap<Id, EClass<L>>,
    /// [`ENode::op_key`] bucket -> ids of classes that hold a node with that bucket,
    /// letting [`Self::classes_with_op`] skip classes a concrete-rooted pattern can
    /// never match. Append-only: a `union` leaves the absorbed id in place, where
    /// `find` resolves it to the survivor (which still holds the node), and the
    /// caller dedups — so the index over-approximates but never misses a live class
    /// and never needs repair.
    classes_by_op: HashMap<u64, Vec<Id>>,
    /// Classes touched by a `union` since the last `rebuild`, awaiting congruence
    /// repair. Base reps in the base scope, scope reps inside a scope.
    pending: Vec<Id>,
    /// Scope overlay, live only while a scope is open. `scope_members` and
    /// `scope_classes` are caches of the current scope partition, rebuilt from the
    /// base by [`Self::aggregate_scope`]; `scope_memo` is a stack of scope
    /// hash-cons tables, one per open context, so a nested `pop_context` restores
    /// the enclosing scope's table instead of discarding it. The base
    /// `classes`/`memo` stay immutable underneath all of them.
    scope_members: HashMap<Id, Vec<Id>>,
    scope_classes: HashMap<Id, EClass<L>>,
    scope_memo: Vec<HashMap<u64, Vec<(L, Id)>>>,
}

impl<L: ENode> Default for EGraph<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: ENode> EGraph<L> {
    pub fn new() -> Self {
        Self {
            unionfind: ScopedDisjointSet::new(0),
            memo: HashMap::new(),
            classes: HashMap::new(),
            classes_by_op: HashMap::new(),
            pending: Vec::new(),
            scope_members: HashMap::new(),
            scope_classes: HashMap::new(),
            scope_memo: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Total number of e-nodes across all (current-scope) classes.
    pub fn total_size(&self) -> usize {
        self.current_classes().values().map(EClass::len).sum()
    }

    pub fn num_classes(&self) -> usize {
        self.current_classes().len()
    }

    /// Whether an assumption scope is currently open.
    fn in_scope(&self) -> bool {
        self.unionfind.depth() > 0
    }

    /// The class table that reflects the current scope: the scope overlay while a
    /// scope is open, otherwise the base classes.
    fn current_classes(&self) -> &HashMap<Id, EClass<L>> {
        if self.in_scope() {
            &self.scope_classes
        } else {
            &self.classes
        }
    }

    /// Enter an assumption scope. Unions until the matching [`Self::pop_context`]
    /// are local to it; the base scope's classes and hash-cons stay untouched.
    pub fn push_context(&mut self) {
        self.unionfind.push_context();
        self.scope_memo.push(HashMap::new());
        self.aggregate_scope();
    }

    /// Leave the current assumption scope, discarding its unions and overlay. The
    /// enclosing scope (or the base) is restored without a rebuild.
    pub fn pop_context(&mut self) {
        self.unionfind.pop_context();
        self.scope_memo.pop();
        self.scope_members.clear();
        self.scope_classes.clear();
        if self.in_scope() {
            self.aggregate_scope();
        }
    }

    /// Canonicalize `id` to its class root.
    pub fn find(&self, id: Id) -> Id {
        Id::from_raw(self.unionfind.find(id.0))
    }

    pub fn connected(&self, a: Id, b: Id) -> bool {
        self.find(a) == self.find(b)
    }

    pub fn class(&self, id: Id) -> &EClass<L> {
        let root = self.find(id);
        // Fall back to the base class for a node added since the last scope rebuild
        // (not yet aggregated into `scope_classes`).
        self.current_classes()
            .get(&root)
            .or_else(|| self.classes.get(&root))
            .expect("live e-class")
    }

    pub fn classes(&self) -> impl Iterator<Item = &EClass<L>> + '_ {
        self.current_classes().values()
    }

    /// The canonical (current-scope) classes that hold a node in [`ENode::op_key`]
    /// bucket `op`, each once. Lets a concrete-rooted pattern visit only plausible
    /// roots instead of every class; the result still over-approximates, so callers
    /// confirm with [`ENode::matches`].
    pub fn classes_with_op(&self, op: u64) -> Vec<Id> {
        let Some(ids) = self.classes_by_op.get(&op) else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        ids.iter()
            .map(|&id| self.find(id))
            .filter(|&root| seen.insert(root))
            .collect()
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
        if self.in_scope() {
            // Record the merge in the scope overlay only; the base classes that
            // `survivor` and `absorbed` denote are left intact for `pop_context`.
            let taken = self
                .scope_members
                .remove(&absorbed)
                .unwrap_or_else(|| vec![absorbed]);
            self.scope_members
                .entry(survivor)
                .or_insert_with(|| vec![survivor])
                .extend(taken);
        } else {
            let mut taken = self.classes.remove(&absorbed).expect("absorbed e-class");
            let surv = self.classes.get_mut(&survivor).expect("surviving e-class");
            surv.nodes.append(&mut taken.nodes);
            surv.parents.append(&mut taken.parents);
        }
        self.pending.push(survivor);
        survivor
    }

    /// Saturate in place with `rules`. Each iteration searches every rule against the
    /// same snapshot, then applies all matches and rebuilds — so a node born this
    /// iteration is only visible to the next. Stops at a fixpoint (an iteration that
    /// changes neither the class nor the node count) or once a limit is reached.
    /// Borrows `self` so a caller can saturate across [`Self::push_context`] scopes;
    /// [`Runner::run`] delegates here.
    pub fn saturate<'a, S>(
        &mut self,
        rules: impl IntoIterator<Item = &'a Rewrite<L, S>>,
        iter_limit: usize,
        node_limit: usize,
    ) where
        L: 'a,
        S: Clone + PartialEq + 'a,
    {
        let rules: Vec<&Rewrite<L, S>> = rules.into_iter().collect();
        let mut iters = 0;
        loop {
            if iters >= iter_limit || self.total_size() >= node_limit {
                break;
            }
            let before = (self.num_classes(), self.total_size());

            let searched: Vec<_> = rules
                .iter()
                .map(|rule| (*rule, rule.lhs.search(self)))
                .collect();
            for (rule, matches) in &searched {
                for m in matches {
                    rule.apply_match(self, m);
                }
            }
            self.rebuild();

            iters += 1;
            if (self.num_classes(), self.total_size()) == before {
                break;
            }
        }
    }

    /// Restore the congruence invariant to a fixpoint after a batch of unions, and
    /// re-canonicalize the hash-cons.
    ///
    /// Processes the pending classes in rounds, deduplicating each round to its
    /// canonical reps first. A `union` may queue the same survivor thousands of
    /// times in one batch; without the dedup each duplicate would re-`repair` the
    /// class — reprocessing its whole (growing) parent list — making rebuild
    /// quadratic in the number of unions. With it, every class is repaired once per
    /// round, and rounds continue until a round adds nothing (congruence fixpoint).
    pub fn rebuild(&mut self) {
        if self.in_scope() {
            self.rebuild_scope();
            return;
        }
        while !self.pending.is_empty() {
            let mut todo = std::mem::take(&mut self.pending);
            for id in &mut todo {
                *id = self.find(*id);
            }
            todo.sort_unstable();
            todo.dedup();
            for id in todo {
                self.repair(id);
            }
        }
    }

    /// Congruence repair inside a scope. The base `classes`/`memo` are read-only:
    /// for every scope class touched since the last rebuild, walk the base parents
    /// of the base classes it covers, canonicalize them through the scope, and
    /// union any that collide in a fresh scope hash-cons (which re-queues work).
    /// Repeats to a fixpoint, then re-aggregates the scope view.
    fn rebuild_scope(&mut self) {
        // `memo` is the scope hash-cons being built; it accumulates across rounds.
        // Each round is deduplicated to its canonical reps, so a survivor queued many
        // times by `union` is processed once per round rather than reprocessing its
        // covered parents every time (the same quadratic the base path avoids).
        let mut memo: HashMap<u64, Vec<(L, Id)>> = HashMap::new();
        while !self.pending.is_empty() {
            let mut todo = std::mem::take(&mut self.pending);
            for rep in &mut todo {
                *rep = self.find(*rep);
            }
            todo.sort_unstable();
            todo.dedup();
            for rep in todo {
                let rep = self.find(rep);
                let members = self
                    .scope_members
                    .get(&rep)
                    .cloned()
                    .unwrap_or_else(|| vec![rep]);
                for base_rep in members {
                    let Some(class) = self.classes.get(&base_rep) else {
                        continue;
                    };
                    for (mut p_node, p_class) in class.parents.clone() {
                        if p_node.is_unique() {
                            continue;
                        }
                        self.canonicalize(&mut p_node);
                        let p_class = self.find(p_class);
                        let bucket = memo.entry(p_node.hash_cons()).or_default();
                        let congruent = bucket
                            .iter()
                            .find(|(stored, _)| {
                                stored.matches(&p_node) && stored.children() == p_node.children()
                            })
                            .map(|&(_, id)| id);
                        match congruent {
                            Some(other) => {
                                let other = self.find(other);
                                if other != p_class {
                                    self.union(other, p_class);
                                }
                            }
                            None => bucket.push((p_node, p_class)),
                        }
                    }
                }
            }
        }
        self.aggregate_scope();
    }

    /// Rebuild the scope view from the base classes and the current scope unions:
    /// `scope_members` groups the base reps under each scope rep, and
    /// `scope_classes` aggregates their e-nodes so the read API works in a scope.
    fn aggregate_scope(&mut self) {
        let mut members: HashMap<Id, Vec<Id>> = HashMap::new();
        let mut nodes: HashMap<Id, Vec<L>> = HashMap::new();
        for class in self.classes.values() {
            let root = self.find(class.id);
            members.entry(root).or_default().push(class.id);
            nodes
                .entry(root)
                .or_default()
                .extend(class.nodes.iter().cloned());
        }
        self.scope_members = members;
        self.scope_classes = nodes
            .into_iter()
            .map(|(id, nodes)| {
                (
                    id,
                    EClass {
                        id,
                        nodes,
                        parents: Vec::new(),
                    },
                )
            })
            .collect();
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

    /// The class of a canonical `node` already in the memo, or `None`. Open scopes
    /// are consulted innermost-first, then the base hash-cons.
    fn memo_find(&self, node: &L) -> Option<Id> {
        for memo in self.scope_memo.iter().rev() {
            if let Some(id) = Self::bucket_lookup(memo, node) {
                return Some(self.find(id));
            }
        }
        Self::bucket_lookup(&self.memo, node).map(|id| self.find(id))
    }

    fn bucket_lookup(memo: &HashMap<u64, Vec<(L, Id)>>, node: &L) -> Option<Id> {
        memo.get(&node.hash_cons())?
            .iter()
            .find(|(stored, _)| stored.matches(node) && stored.children() == node.children())
            .map(|&(_, id)| id)
    }

    /// Insert or update the memo entry for a canonical `node`, in the innermost
    /// open scope's hash-cons while a scope is open (leaving the base one
    /// untouched).
    fn memo_insert(&mut self, node: L, id: Id) {
        let memo = self.scope_memo.last_mut().unwrap_or(&mut self.memo);
        let bucket = memo.entry(node.hash_cons()).or_default();
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
        self.classes_by_op
            .entry(node.op_key())
            .or_default()
            .push(id);
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

    #[test]
    fn scope_union_is_discarded_on_pop() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = num(&mut g, 7);
        g.push_context();
        g.union(a, b);
        assert!(g.connected(a, b));
        g.pop_context();
        assert!(!g.connected(a, b));
    }

    #[test]
    fn scope_congruence_collapses_and_restores() {
        // neg(a) and neg(b) are distinct at base; assuming a≡b in a scope makes
        // them congruent, and popping restores the distinction.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let fa = neg(&mut g, a);
        let fb = neg(&mut g, b);
        g.rebuild();
        assert!(!g.connected(fa, fb));

        g.push_context();
        g.union(a, b);
        g.rebuild();
        assert!(g.connected(a, b));
        assert!(g.connected(fa, fb));

        g.pop_context();
        assert!(!g.connected(a, b));
        assert!(!g.connected(fa, fb));
    }

    #[test]
    fn scope_preserves_base_equalities() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        g.union(a, b);
        g.rebuild();

        g.push_context();
        assert!(g.connected(a, b));
        g.union(b, c);
        g.rebuild();
        assert!(g.connected(a, c));
        g.pop_context();

        assert!(g.connected(a, b));
        assert!(!g.connected(a, c));
    }

    #[test]
    fn scope_congruence_propagates_to_fixpoint() {
        // neg(neg(a)) ≡ a under a≡neg(a): assuming a≡neg(a) collapses the whole
        // tower of negations into one class.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let mut cur = a;
        for _ in 0..5 {
            cur = neg(&mut g, cur);
        }
        let fa = neg(&mut g, a);
        g.rebuild();
        let base_classes = g.num_classes();

        g.push_context();
        g.union(fa, a);
        g.rebuild();
        assert_eq!(g.num_classes(), 1);
        g.pop_context();
        assert_eq!(g.num_classes(), base_classes);
    }

    #[test]
    fn nested_scopes_isolate() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        g.push_context();
        g.union(a, b);
        g.push_context();
        g.union(b, c);
        g.rebuild();
        assert!(g.connected(a, c));
        g.pop_context();
        assert!(g.connected(a, b));
        assert!(!g.connected(a, c));
        g.pop_context();
        assert!(!g.connected(a, b));
    }

    #[test]
    fn scope_add_then_congruence() {
        // A node built inside a scope participates in scoped congruence.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let fa = neg(&mut g, a);
        g.rebuild();

        g.push_context();
        g.union(a, b);
        let fb = neg(&mut g, b);
        g.rebuild();
        assert!(g.connected(fa, fb));
        g.pop_context();
        // fb's base singleton lingers but is no longer equal to fa.
        assert!(!g.connected(fa, fb));
    }

    #[test]
    fn nested_pop_restores_outer_scope_hash_cons() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();

        g.push_context();
        let outer = add(&mut g, a, b); // interned in the outer scope's hash-cons
        g.push_context();
        let c = sym(&mut g, 2);
        g.union(a, c);
        g.rebuild();
        g.pop_context();

        // Back in the outer scope: re-adding the node must hit the same class, so
        // the outer scope's hash-cons survived the nested pop.
        let again = add(&mut g, a, b);
        assert_eq!(g.find(again), g.find(outer));
        assert_eq!(g.nodes(g.find(outer)).len(), 1);
    }

    #[test]
    fn rewrite_under_scope_is_discarded_on_pop() {
        // add(x, y) => add(y, x), applied only inside a scope.
        let mut lhs: Pattern<Math, &'static str> = Pattern::new();
        let x = lhs.var(Var::Symbol("x"));
        let y = lhs.var(Var::Symbol("y"));
        lhs.add(Math::Add([x, y]));
        let mut rhs: Pattern<Math, &'static str> = Pattern::new();
        let rx = rhs.var(Var::Symbol("x"));
        let ry = rhs.var(Var::Symbol("y"));
        rhs.add(Math::Add([ry, rx]));
        let comm = Rewrite::new("add-comm", lhs, Rhs::Pattern(rhs));

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);
        g.rebuild();
        assert!(!g.connected(ab, ba));

        g.push_context();
        comm.apply_all(&mut g);
        assert!(g.connected(ab, ba));
        g.pop_context();
        assert!(!g.connected(ab, ba));
    }
}
