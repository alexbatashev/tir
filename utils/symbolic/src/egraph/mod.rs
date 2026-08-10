//! Hash-consing e-graph with congruence closure. Based on egg
//! (<https://github.com/egraphs-good/egg>, MIT License, Copyright Max Willsey).

mod eclass;
mod enode;
mod extract;
mod pattern;
mod rewrite;
mod runner;
#[cfg(test)]
mod test_lang;

use std::collections::{HashMap, HashSet};

use tir_adt::{FxBuildHasher, IndexMap, ScopedDisjointSet};

type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
/// Hash-cons table: [`ENode::hash_cons`] bucket -> `[(canonical node, class)]`.
/// Never iterated, so a hash map keeps it deterministic.
type Memo<L> = FxHashMap<u64, Vec<(L, Id)>>;

pub use eclass::*;
pub use enode::*;
pub use extract::*;
pub use pattern::*;
pub use rewrite::*;
pub use runner::*;

/// E-class id. Non-canonical after unions — pass through [`EGraph::find`] before comparing.
#[derive(Clone, Copy, Hash, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Id(u32);

impl Id {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn from_raw(raw: u32) -> Self {
        Id(raw)
    }
}

pub struct EGraph<L: ENode> {
    /// Class-id equivalence; the sole authority on equality (all comparison flows
    /// through [`Self::find`]). Scoped unions layer here, discarded by `pop_context`.
    unionfind: ScopedDisjointSet,
    /// Base hash-cons. Collisions only share a bucket; identity is `matches` + equal children.
    memo: Memo<L>,
    /// Canonical base class id -> e-class; absorbed ids removed on `union`. Scoped
    /// unions never touch it, so `pop_context` restores it for free. Insertion order
    /// is ascending id, which is the class iteration order callers observe.
    classes: IndexMap<Id, EClass<L>>,
    /// Running `classes` node total, so [`Self::total_size`] costs nothing.
    total_nodes: usize,
    /// [`ENode::op_key`] bucket -> class ids holding such a node, so
    /// [`Self::classes_with_op`] skips classes a concrete-rooted pattern can't match.
    /// Append-only, caller-dedup'd: over-approximates, never misses a live class.
    classes_by_op: IndexMap<u64, Vec<Id>>,
    /// Classes touched by a `union` since the last `rebuild`, awaiting repair.
    pending: Vec<Id>,
    /// Scope overlay, live only inside a scope. One `scope_members` frame per open
    /// context holds that scope's partition (base reps grouped under their scoped
    /// rep, merged groups only); `scope_memo` stacks one hash-cons per context so a
    /// nested `pop_context` restores the enclosing table. Base `classes`/`memo` stay
    /// immutable underneath.
    scope_members: Vec<FxHashMap<Id, Vec<Id>>>,
    /// Read-side aggregation of the innermost frame, refreshed by [`Self::refresh_view`].
    view: ScopeView<L>,
    scope_memo: Vec<Memo<L>>,
    /// Undo log of base insertions per open context: `make_class` still writes the
    /// new class into base `classes`/`classes_by_op`/children's `parents` while scoped
    /// (so the overlay, which reads base, sees it), but `pop_context` reverts exactly
    /// these so a popped scope leaves the base structurally identical. One frame per
    /// context; nested pops revert only the innermost.
    scope_created: Vec<Vec<ScopeCreated>>,
}

/// Aggregated read view of the innermost scope, as of the last refresh. Only merged
/// groups are materialized; every other class is read straight from the base.
struct ScopeView<L: ENode> {
    /// Scoped rep -> its aggregated e-class.
    classes: FxHashMap<Id, EClass<L>>,
    /// What [`EGraph::classes`] emits at a merged group's base class: `Some(rep)` at
    /// the group's first member, `None` (skip) at the rest.
    plan: FxHashMap<Id, Option<Id>>,
    /// Base classes present at the refresh. Classes minted afterwards stay invisible
    /// to the view until the next one, as the old full re-aggregation had them.
    watermark: usize,
    num_classes: usize,
    total_size: usize,
}

impl<L: ENode> ScopeView<L> {
    fn clear(&mut self) {
        self.classes.clear();
        self.plan.clear();
        self.watermark = 0;
        self.num_classes = 0;
        self.total_size = 0;
    }
}

impl<L: ENode> Default for ScopeView<L> {
    fn default() -> Self {
        Self {
            classes: FxHashMap::default(),
            plan: FxHashMap::default(),
            watermark: 0,
            num_classes: 0,
            total_size: 0,
        }
    }
}

/// One base class minted by [`EGraph::make_class`] inside a scope, with what its
/// insertion mutated so [`EGraph::pop_context`] can undo it.
struct ScopeCreated {
    id: Id,
    op_key: u64,
    /// Child classes that received a parent back-edge for this class.
    parents_on: Vec<Id>,
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
            memo: Memo::default(),
            classes: IndexMap::new(),
            total_nodes: 0,
            classes_by_op: IndexMap::new(),
            pending: Vec::new(),
            scope_members: Vec::new(),
            view: ScopeView::default(),
            scope_memo: Vec::new(),
            scope_created: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Total number of e-nodes across all (current-scope) classes.
    pub fn total_size(&self) -> usize {
        if self.in_scope() {
            self.view.total_size
        } else {
            self.total_nodes
        }
    }

    pub fn num_classes(&self) -> usize {
        if self.in_scope() {
            self.view.num_classes
        } else {
            self.classes.len()
        }
    }

    fn in_scope(&self) -> bool {
        self.unionfind.depth() > 0
    }

    /// Enter an assumption scope: unions until the matching `pop_context` are local;
    /// base classes and hash-cons stay untouched.
    pub fn push_context(&mut self) {
        let frame = self.scope_members.last().cloned().unwrap_or_default();
        self.unionfind.push_context();
        self.scope_memo.push(Memo::default());
        self.scope_created.push(Vec::new());
        self.scope_members.push(frame);
        self.refresh_view();
    }

    /// Leave the scope, discarding its unions, overlay, and any classes added inside
    /// it; the enclosing scope (or base) is restored without a rebuild.
    pub fn pop_context(&mut self) {
        if let Some(created) = self.scope_created.pop() {
            self.undo_created(created);
        }
        self.unionfind.pop_context();
        self.scope_memo.pop();
        self.scope_members.pop();
        // Drop union work queued against classes that vanished with the scope; a
        // survivor kept here would panic the next base `repair`.
        self.pending = self
            .pending
            .iter()
            .copied()
            .filter(|&id| self.classes.contains_key(&self.find(id)))
            .collect();
        if self.in_scope() {
            self.refresh_view();
        } else {
            self.view.clear();
        }
    }

    /// Revert the base insertions logged for a just-popped scope: remove each class's
    /// `classes_by_op` entry, the parent back-edges it pushed onto surviving children,
    /// and the class itself.
    fn undo_created(&mut self, created: Vec<ScopeCreated>) {
        for c in created {
            for child in c.parents_on {
                if let Some(class) = self.classes.get_mut(&child) {
                    class.parents.retain(|(_, pc)| *pc != c.id);
                }
            }
            if let Some(bucket) = self.classes_by_op.get_mut(&c.op_key) {
                bucket.retain(|&id| id != c.id);
                if bucket.is_empty() {
                    self.classes_by_op.remove(&c.op_key);
                }
            }
            if let Some(class) = self.classes.remove(&c.id) {
                self.total_nodes -= class.nodes.len();
            }
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
        // Fall back to base for a class the view does not merge, or one added since
        // the last scope rebuild.
        (!self.view.classes.is_empty())
            .then(|| self.view.classes.get(&root))
            .flatten()
            .or_else(|| self.classes.get(&root))
            .expect("live e-class")
    }

    pub fn classes(&self) -> impl Iterator<Item = &EClass<L>> + '_ {
        let scoped = self.in_scope();
        let base = (!scoped)
            .then(|| self.classes.values())
            .into_iter()
            .flatten();
        let view = scoped
            .then(|| {
                self.classes
                    .values()
                    .take(self.view.watermark)
                    .filter_map(|class| match self.view.plan.get(&class.id) {
                        Some(rep) => rep.map(|rep| &self.view.classes[&rep]),
                        None => Some(class),
                    })
            })
            .into_iter()
            .flatten();
        base.chain(view)
    }

    /// Canonical current-scope classes holding a node in `op` bucket, each once.
    /// Over-approximates — callers confirm with [`ENode::matches`].
    pub fn classes_with_op(&self, op: u64) -> Vec<Id> {
        let Some(ids) = self.classes_by_op.get(&op) else {
            return Vec::new();
        };
        let mut seen: HashSet<Id, FxBuildHasher> =
            HashSet::with_capacity_and_hasher(ids.len(), FxBuildHasher::default());
        ids.iter()
            .map(|&id| self.find(id))
            .filter(|&root| seen.insert(root))
            .collect()
    }

    /// E-nodes of `id`'s class; child ids may be non-canonical — resolve with [`Self::find`].
    pub fn nodes(&self, id: Id) -> &[L] {
        self.class(id).nodes()
    }

    /// The base class ids the scope partition groups under the scoped-canonical
    /// `id` (as [`Self::aggregate_scope`] builds it). Empty when no scope is open
    /// or `id` is not a scoped representative — then `id` is itself the base rep.
    /// Side tables built against the base graph are keyed by base reps, so a query
    /// made under a scope aggregates over this slice.
    pub fn scope_members(&self, id: Id) -> &[Id] {
        self.scope_members
            .last()
            .and_then(|frame| frame.get(&id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Intern `node`, returning its e-class. A non-unique node equal to an existing
    /// one shares its class; otherwise (always for unique nodes) a fresh class.
    pub fn add(&mut self, mut node: L) -> Id {
        self.canonicalize(&mut node);
        if !node.is_unique()
            && let Some(existing) = self.memo_find(&node)
        {
            return existing;
        }
        self.make_class(node)
    }

    /// Class of an already-interned `node`, or `None` (never inserts; always `None` for unique).
    pub fn lookup(&self, node: &L) -> Option<Id> {
        if node.is_unique() {
            return None;
        }
        let mut node = node.clone();
        self.canonicalize(&mut node);
        self.memo_find(&node)
    }

    /// Merge the classes of `a` and `b`, returning the survivor. Congruence repair
    /// deferred to [`Self::rebuild`].
    pub fn union(&mut self, a: Id, b: Id) -> Id {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return ra;
        }
        let survivor = Id::from_raw(self.unionfind.union(ra.0, rb.0));
        let absorbed = if survivor == ra { rb } else { ra };
        if let Some(frame) = self.scope_members.last_mut() {
            // Scope overlay only; base classes stay intact for `pop_context`.
            let taken = frame.remove(&absorbed).unwrap_or_else(|| vec![absorbed]);
            frame
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

    /// Saturate in place with `rules`. Each iteration searches all rules against one
    /// snapshot, then applies and rebuilds — a node born this iteration is visible
    /// only to the next. Stops at a fixpoint (no class/node-count change) or a limit.
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
            let size = self.total_size();
            if iters >= iter_limit || size >= node_limit {
                break;
            }
            let before = (self.num_classes(), size);

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

    /// Restore congruence to a fixpoint after a batch of unions, re-canonicalizing
    /// the hash-cons. Each round dedups pending to canonical reps first: without it a
    /// survivor queued many times by `union` would re-`repair` its growing parent
    /// list each time, making rebuild quadratic. Rounds run until one adds nothing.
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

    /// Congruence repair inside a scope, base `classes`/`memo` read-only: walk the
    /// base parents each touched scope class covers, canonicalize through the scope,
    /// and union collisions in a fresh scope hash-cons. Fixpoint, then re-aggregate.
    fn rebuild_scope(&mut self) {
        // Scope hash-cons accumulated across rounds; per-round dedup avoids the same
        // quadratic the base path avoids.
        let mut memo: Memo<L> = Memo::default();
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
                    .last()
                    .and_then(|frame| frame.get(&rep))
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
                            .find(|(stored, _)| is_congruent(stored, &p_node))
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
        self.refresh_view();
    }

    /// Re-aggregate the read view from the innermost scope frame. Only merged groups
    /// are materialized, so the cost is the size of the assumption's collapse, not of
    /// the graph. Members are sorted so a group's e-nodes concatenate in base order.
    fn refresh_view(&mut self) {
        let mut frame = self.scope_members.pop().expect("open scope");
        self.view.classes.clear();
        self.view.plan.clear();
        let mut absorbed = 0;
        for (&rep, members) in frame.iter_mut() {
            members.sort_unstable();
            members.dedup();
            let mut nodes = Vec::new();
            for member in members.iter() {
                if let Some(class) = self.classes.get(member) {
                    nodes.extend(class.nodes.iter().cloned());
                }
            }
            self.view.plan.insert(members[0], Some(rep));
            for &member in &members[1..] {
                self.view.plan.insert(member, None);
            }
            absorbed += members.len() - 1;
            self.view.classes.insert(
                rep,
                EClass {
                    id: rep,
                    nodes,
                    parents: Vec::new(),
                },
            );
        }
        self.view.watermark = self.classes.len();
        self.view.num_classes = self.view.watermark - absorbed;
        self.view.total_size = self.total_nodes;
        self.scope_members.push(frame);
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

    /// Class of a canonical `node` in the memo, or `None`. Scopes innermost-first,
    /// then the base hash-cons.
    fn memo_find(&self, node: &L) -> Option<Id> {
        for memo in self.scope_memo.iter().rev() {
            if let Some(id) = Self::bucket_lookup(memo, node) {
                return Some(self.find(id));
            }
        }
        Self::bucket_lookup(&self.memo, node).map(|id| self.find(id))
    }

    fn bucket_lookup(memo: &Memo<L>, node: &L) -> Option<Id> {
        memo.get(&node.hash_cons())?
            .iter()
            .find(|(stored, _)| is_congruent(stored, node))
            .map(|&(_, id)| id)
    }

    /// Insert/update the memo entry for a canonical `node`, in the innermost open
    /// scope's hash-cons while scoped (base untouched).
    fn memo_insert(&mut self, node: L, id: Id) {
        let memo = self.scope_memo.last_mut().unwrap_or(&mut self.memo);
        let bucket = memo.entry(node.hash_cons()).or_default();
        match bucket
            .iter_mut()
            .find(|(stored, _)| is_congruent(stored, &node))
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
            .position(|(stored, _)| is_congruent(stored, node))
        {
            bucket.swap_remove(pos);
        }
        if bucket.is_empty() {
            self.memo.remove(&key);
        }
    }

    /// Fresh singleton class for a canonical `node`: register it as a parent of each
    /// distinct child class and (unless unique) memoize it.
    fn make_class(&mut self, node: L) -> Id {
        let id = Id::from_raw(self.unionfind.push());
        let op_key = node.op_key();
        self.classes_by_op.entry(op_key).or_default().push(id);
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
        self.total_nodes += 1;
        // Inside a scope, log this base insertion so `pop_context` can revert it.
        if let Some(frame) = self.scope_created.last_mut() {
            frame.push(ScopeCreated {
                id,
                op_key,
                parents_on: seen,
            });
        }
        id
    }

    /// Congruence repair for one class: re-canonicalize its `parents`, refresh their
    /// memo entries, and union any now structurally equal (queuing more via `union`).
    fn repair(&mut self, id: Id) {
        let id = self.find(id);
        let parents = std::mem::take(&mut self.class_mut(id).parents);

        for (p_node, _) in &parents {
            if !p_node.is_unique() {
                self.memo_remove(p_node);
            }
        }

        let mut new_parents: Vec<(L, Id)> = Vec::with_capacity(parents.len());
        let mut index: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
        for (mut p_node, p_class) in parents {
            self.canonicalize(&mut p_node);
            let p_class = self.find(p_class);
            if p_node.is_unique() {
                new_parents.push((p_node, p_class));
                continue;
            }
            let slot = index.entry(p_node.hash_cons()).or_default();
            let congruent = slot
                .iter()
                .copied()
                .find(|&i| is_congruent(&new_parents[i].0, &p_node));
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

        // Extend, don't assign: a `union` above may have appended parents to this
        // class; an assignment would drop them. Duplicates dedup on the next pass.
        let root = self.find(id);
        self.class_mut(root).parents.extend(new_parents);
    }
}

/// Structural congruence: same operator ([`ENode::matches`]) and equal canonical children.
fn is_congruent<L: ENode>(stored: &L, probe: &L) -> bool {
    stored.matches(probe) && stored.children() == probe.children()
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
    fn hash_cons_includes_children() {
        let a = Id::from_raw(1);
        let b = Id::from_raw(2);
        let c = Id::from_raw(3);

        assert_eq!(Math::Add([a, b]).hash_cons(), Math::Add([a, b]).hash_cons());
        assert_ne!(Math::Add([a, b]).hash_cons(), Math::Add([a, c]).hash_cons());
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
        let comm = comm_rule();

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

    #[test]
    fn scope_add_then_pop_restores_class_count() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();
        let base = g.num_classes();

        g.push_context();
        neg(&mut g, a);
        add(&mut g, a, b);
        g.rebuild();
        assert_eq!(g.num_classes(), base + 2);
        g.pop_context();
        assert_eq!(g.num_classes(), base);
    }

    #[test]
    fn readd_after_pop_mints_one_class_no_accumulation() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();
        let base = g.num_classes();

        g.push_context();
        add(&mut g, a, b);
        g.pop_context();

        // The scope's node is gone from the base memo, so re-adding mints exactly one
        // fresh class; it is then interned, so a repeat shares it (no accumulation).
        let e1 = add(&mut g, a, b);
        assert_eq!(g.num_classes(), base + 1);
        let e2 = add(&mut g, a, b);
        assert_eq!(g.find(e1), g.find(e2));
        assert_eq!(g.num_classes(), base + 1);
    }

    #[test]
    fn scope_add_registers_then_reverts_child_parents() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        g.rebuild();
        let before = g.classes.get(&a).unwrap().parents.len();

        g.push_context();
        neg(&mut g, a);
        assert_eq!(g.classes.get(&a).unwrap().parents.len(), before + 1);
        g.pop_context();
        assert_eq!(g.classes.get(&a).unwrap().parents.len(), before);
    }

    #[test]
    fn nested_scope_pop_reverts_only_inner_adds() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();
        let base = g.num_classes();

        g.push_context();
        neg(&mut g, a);
        g.rebuild();
        assert_eq!(g.num_classes(), base + 1);
        g.push_context();
        add(&mut g, a, b);
        g.rebuild();
        assert_eq!(g.num_classes(), base + 2);
        g.pop_context();
        assert_eq!(g.num_classes(), base + 1);
        g.pop_context();
        assert_eq!(g.num_classes(), base);
    }

    #[test]
    fn scoped_saturate_leaves_base_identical() {
        // Commutativity introduces add(b, a) as a new node inside the scope; after pop
        // the base graph must be structurally identical.
        let comm = comm_rule();
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        add(&mut g, a, b);
        g.rebuild();
        let base_classes = g.num_classes();
        let base_size = g.total_size();
        let a_parents = g.classes.get(&a).unwrap().parents.len();

        g.push_context();
        g.saturate([&comm], 10, 1000);
        assert!(g.total_size() > base_size);
        g.pop_context();

        assert_eq!(g.num_classes(), base_classes);
        assert_eq!(g.total_size(), base_size);
        assert_eq!(g.classes.get(&a).unwrap().parents.len(), a_parents);
    }

    #[test]
    fn scope_merge_aggregates_nodes_in_base_order() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        sym(&mut g, 1);
        let c = sym(&mut g, 2);
        g.rebuild();

        g.push_context();
        g.union(c, a);
        g.rebuild();
        let root = g.find(a);
        assert_eq!(g.scope_members(root), &[a, c][..]);
        let nodes = g.nodes(root);
        assert!(matches!(nodes[0], Math::Sym(0)));
        assert!(matches!(nodes[1], Math::Sym(2)));
        assert_eq!(g.num_classes(), 2);
        assert_eq!(g.total_size(), 3);

        g.pop_context();
        assert_eq!(g.num_classes(), 3);
        assert!(g.scope_members(root).is_empty());
        assert_eq!(g.nodes(g.find(a)).len(), 1);
    }

    #[test]
    fn nested_pop_restores_outer_scope_partition() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        let d = sym(&mut g, 3);
        g.rebuild();

        g.push_context();
        g.union(a, b);
        g.rebuild();
        let outer = g.find(a);

        g.push_context();
        g.union(c, d);
        g.rebuild();
        assert_eq!(g.num_classes(), 2);
        g.pop_context();

        assert_eq!(g.num_classes(), 3);
        assert_eq!(g.total_size(), 4);
        assert_eq!(g.scope_members(outer), &[a, b][..]);
        assert_eq!(g.nodes(outer).len(), 2);
        assert!(!g.connected(c, d));

        g.pop_context();
        assert_eq!(g.num_classes(), 4);
    }

    #[test]
    fn scope_class_view_is_snapshot_until_rebuild() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();

        g.push_context();
        g.union(a, b);
        // No rebuild yet: the aggregated view still shows the pre-union partition.
        assert_eq!(g.num_classes(), 2);
        assert_eq!(g.total_size(), 2);
        g.rebuild();
        assert_eq!(g.num_classes(), 1);
        assert_eq!(g.total_size(), 2);
        g.pop_context();
    }

    #[test]
    fn classes_iterate_scope_roots_at_first_member_position() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        g.rebuild();

        g.push_context();
        g.union(a, c);
        g.rebuild();
        let seen: Vec<Id> = g.classes().map(|class| class.id()).collect();
        assert_eq!(seen, vec![g.find(a), b]);
        g.pop_context();
    }

    #[test]
    fn classes_by_op_does_not_accumulate_across_cycles() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        g.rebuild();
        let add_key = Math::Add([a, b]).op_key();
        assert!(!g.classes_by_op.contains_key(&add_key));

        for _ in 0..5 {
            g.push_context();
            add(&mut g, a, b);
            g.pop_context();
        }
        assert!(!g.classes_by_op.contains_key(&add_key));
    }
}
