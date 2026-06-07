//! A generic e-graph with equality saturation, built on the same [`Node`] and
//! [`Pattern`] abstractions used by the DAG cover driver.
//!
//! An e-graph compactly represents a congruence over expressions: every *e-class*
//! is a set of equivalent *e-nodes*, and equivalent sub-expressions are shared.
//! Rewrites grow the e-graph with new, provably-equivalent forms instead of
//! destroying the old ones, so a later cost-driven extraction (here: the PBQP
//! instruction-selection cover) can pick the cheapest realization across *all*
//! equivalent forms at once.
//!
//! This is deliberately target-independent and reusable: instruction selection
//! seeds it from a program's semantic expressions and saturates with bit-vector
//! identities, but the same type is intended to back a future instcombine-style
//! mid-end. It is generic over the node label `N: Matchable` and a leaf payload `L`,
//! mirroring [`crate::graph::Dag`].

use std::collections::HashMap;
use std::hash::Hash;

use crate::Context;

use super::{Dag, Matchable, NodeId, OperandConstraint, Pattern, PatternExpr};

/// Identifier of an e-class. Stable as an arena index, but may be *non-canonical*
/// after unions — always pass through [`EGraph::find`] before comparing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EClassId(u32);

impl EClassId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    fn from_raw(i: usize) -> Self {
        EClassId(i as u32)
    }
}

/// A single e-node: an operator label plus the e-classes of its operands.
///
/// The label `node` carries the operator's identity (kind/payload/type); any
/// structural fields it happens to hold are ignored — operands live in
/// `children`. Equality and hashing therefore reduce to "same label + same child
/// classes + same leaf payload", which is exactly the congruence relation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ENode<N, L> {
    pub node: N,
    pub children: Vec<EClassId>,
    pub leaf: Option<L>,
}

impl<N, L> ENode<N, L> {
    pub fn leaf(node: N, leaf: Option<L>) -> Self {
        Self {
            node,
            children: Vec::new(),
            leaf,
        }
    }

    pub fn op(node: N, children: Vec<EClassId>) -> Self {
        Self {
            node,
            children,
            leaf: None,
        }
    }
}

/// One match of a [`Pattern`] against the e-graph: the e-class the pattern root
/// matched, and the e-class bound to every pattern node (indexed by the pattern
/// node's [`NodeId`]). All node ids reachable from the pattern root are bound.
#[derive(Clone, Debug)]
pub struct EMatch {
    root: EClassId,
    bindings: Vec<EClassId>,
}

impl EMatch {
    pub fn root(&self) -> EClassId {
        self.root
    }

    /// The e-class bound to `pattern_node`.
    pub fn binding(&self, pattern_node: NodeId) -> EClassId {
        self.bindings[pattern_node.index()]
    }
}

/// A rewrite: occurrences of `searcher` are found by e-matching, and `apply`
/// extends the e-graph (typically adding the right-hand side and unioning it with
/// the match root). The applier owns the algebraic content so a rule can compute
/// width-dependent constants, which is what bit-vector identities need.
pub type EGraphApplier<N, L> = dyn Fn(&Context, &mut EGraph<N, L>, &EMatch) + Send + Sync;

pub struct Rewrite<N: Matchable, L> {
    pub name: String,
    pub searcher: Pattern<N, ()>,
    /// Extends the e-graph with the rule's right-hand side for a given match. Gets
    /// the [`Context`] so width-dependent rules can resolve types and build typed
    /// nodes (e.g. the constant shift amount `W - n` of a sign-extension bridge).
    pub apply: Box<EGraphApplier<N, L>>,
}

/// Bounds on a saturation run, so a runaway rule set can never loop forever.
#[derive(Clone, Copy, Debug)]
pub struct SaturationLimits {
    pub max_iterations: usize,
    pub max_classes: usize,
}

impl Default for SaturationLimits {
    fn default() -> Self {
        Self {
            max_iterations: 30,
            max_classes: 10_000,
        }
    }
}

pub struct EGraph<N: Matchable, L> {
    /// Union-find parent pointers, indexed by raw e-class id.
    union_find: Vec<u32>,
    /// Canonical class id -> its e-nodes (children kept canonical after `rebuild`).
    classes: HashMap<EClassId, Vec<ENode<N, L>>>,
    /// Hash-cons: canonicalized e-node -> the class that owns it.
    memo: HashMap<ENode<N, L>, EClassId>,
    /// Set when a union has happened but congruence has not yet been repaired.
    dirty: bool,
}

impl<N: Matchable, L> Default for EGraph<N, L> {
    fn default() -> Self {
        Self {
            union_find: Vec::new(),
            classes: HashMap::new(),
            memo: HashMap::new(),
            dirty: false,
        }
    }
}

impl<N: Matchable + Clone + Eq + Hash, L: Clone + Eq + Hash> EGraph<N, L> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical representative of `id`.
    pub fn find(&self, id: EClassId) -> EClassId {
        let mut x = id.0;
        while self.union_find[x as usize] != x {
            x = self.union_find[x as usize];
        }
        EClassId(x)
    }

    /// Iterator over the canonical e-class ids.
    pub fn classes(&self) -> impl Iterator<Item = EClassId> + '_ {
        self.classes.keys().copied()
    }

    /// The e-nodes of (the canonical class of) `id`.
    pub fn nodes(&self, id: EClassId) -> &[ENode<N, L>] {
        self.classes
            .get(&self.find(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn canonicalize(&self, node: &ENode<N, L>) -> ENode<N, L> {
        ENode {
            node: node.node.clone(),
            children: node.children.iter().map(|&c| self.find(c)).collect(),
            leaf: node.leaf.clone(),
        }
    }

    /// Add an e-node, returning its (canonical) class. Hash-consing guarantees that
    /// structurally identical e-nodes share a class.
    pub fn add(&mut self, node: ENode<N, L>) -> EClassId {
        let node = self.canonicalize(&node);
        if let Some(&existing) = self.memo.get(&node) {
            return self.find(existing);
        }
        let id = EClassId::from_raw(self.union_find.len());
        self.union_find.push(id.0);
        self.memo.insert(node.clone(), id);
        self.classes.entry(id).or_default().push(node);
        id
    }

    /// Seed the e-graph from a [`Dag`], returning the class of `root`. Identical
    /// sub-expressions collapse to one class via hash-consing.
    pub fn add_dag<D: Dag<Node = N, Leaf = L>>(&mut self, dag: &D, root: NodeId) -> EClassId {
        let mut memo: HashMap<usize, EClassId> = HashMap::new();
        self.add_dag_node(dag, root, &mut memo)
    }

    fn add_dag_node<D: Dag<Node = N, Leaf = L>>(
        &mut self,
        dag: &D,
        node: NodeId,
        memo: &mut HashMap<usize, EClassId>,
    ) -> EClassId {
        if let Some(&existing) = memo.get(&node.index()) {
            return existing;
        }
        let children: Vec<EClassId> = dag
            .children(node)
            .collect::<Vec<_>>()
            .into_iter()
            .map(|child| self.add_dag_node(dag, child, memo))
            .collect();
        let enode = ENode {
            node: dag.get_node(node).clone(),
            children,
            leaf: dag.get_leaf_data(node).cloned(),
        };
        let id = self.add(enode);
        memo.insert(node.index(), id);
        id
    }

    /// Merge the classes of `a` and `b`. Congruence is *not* repaired until the
    /// next [`EGraph::rebuild`]; the saturation driver batches unions then rebuilds.
    pub fn union(&mut self, a: EClassId, b: EClassId) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return false;
        }
        self.union_find[b.0 as usize] = a.0;
        self.dirty = true;
        true
    }

    /// Restore the congruence invariant after a batch of unions: re-canonicalize
    /// every e-node, merge classes that become structurally identical, and repeat
    /// to a fixpoint. The graphs here are block-sized, so the simple O(n²) sweep is
    /// preferred over egg's incremental worklist for clarity.
    pub fn rebuild(&mut self) {
        if !self.dirty {
            return;
        }
        loop {
            self.recompact();

            let mut seen: HashMap<ENode<N, L>, EClassId> = HashMap::new();
            let mut merges: Vec<(EClassId, EClassId)> = Vec::new();
            for (&class, nodes) in &self.classes {
                for node in nodes {
                    match seen.get(node) {
                        Some(&other) if other != class => merges.push((other, class)),
                        Some(_) => {}
                        None => {
                            seen.insert(node.clone(), class);
                        }
                    }
                }
            }

            if merges.is_empty() {
                break;
            }
            for (a, b) in merges {
                self.union(a, b);
            }
        }

        self.memo.clear();
        for (&class, nodes) in &self.classes {
            for node in nodes {
                self.memo.insert(node.clone(), class);
            }
        }
        self.dirty = false;
    }

    /// Regroup every e-node under its canonical class with canonical children,
    /// deduplicating. Leaves `self.classes` keyed only by canonical ids.
    fn recompact(&mut self) {
        let old = std::mem::take(&mut self.classes);
        for (class, nodes) in old {
            let root = self.find(class);
            for node in nodes {
                let canon = self.canonicalize(&node);
                let entry = self.classes.entry(root).or_default();
                if !entry.contains(&canon) {
                    entry.push(canon);
                }
            }
        }
    }

    // ── E-matching ──────────────────────────────────────────────────────────

    /// All matches of `pattern` anywhere in the e-graph.
    pub fn ematch<A>(&self, ctx: &Context, pattern: &Pattern<N, A>) -> Vec<EMatch> {
        self.ematch_with_legality(ctx, pattern, &|_, _| true)
    }

    /// As [`EGraph::ematch`], but a binding of `pattern_node` to `class` is only
    /// kept when `allowed(pattern_node, class)` holds. Instruction selection uses
    /// this to forbid *consuming* (internalizing) an e-class that is shared by more
    /// than one consumer — such a value must be materialized into a register.
    pub fn ematch_with_legality<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<EMatch> {
        let Some(root) = pattern.root() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for class in self.classes.keys().copied() {
            for binding in self.solve(ctx, pattern, root, class, allowed) {
                out.push(EMatch {
                    root: class,
                    bindings: binding.into_iter().map(Option::unwrap).collect(),
                });
            }
        }
        out
    }

    /// Enumerate every way to match `pattern_node` against `class`. Each result is
    /// a full-length assignment (indexed by pattern node) with the subtree's nodes
    /// bound; merging keeps shared pattern nodes consistent.
    fn solve<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        pattern_node: NodeId,
        class: EClassId,
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<Vec<Option<EClassId>>> {
        let class = self.find(class);
        let empty = || vec![None; pattern.len()];

        if !allowed(pattern_node, class) {
            return Vec::new();
        }

        match pattern.get_node(pattern_node) {
            PatternExpr::Boundary => {
                if !self.boundary_ok(pattern, pattern_node, class) {
                    return Vec::new();
                }
                let mut b = empty();
                b[pattern_node.index()] = Some(class);
                vec![b]
            }
            PatternExpr::Any => {
                let mut b = empty();
                b[pattern_node.index()] = Some(class);
                vec![b]
            }
            PatternExpr::Leaf => {
                if self.class_has_leaf(ctx, class) {
                    let mut b = empty();
                    b[pattern_node.index()] = Some(class);
                    vec![b]
                } else {
                    Vec::new()
                }
            }
            PatternExpr::Node(template) => {
                let children = pattern.children(pattern_node).to_vec();
                let commutative = template.is_commutative() && children.len() == 2;
                let mut results = Vec::new();

                for node in self.nodes(class) {
                    if node.children.len() != children.len() {
                        continue;
                    }
                    if !node.node.matches_pattern(template, ctx) {
                        continue;
                    }

                    let orders: &[Vec<EClassId>] = &if commutative {
                        vec![
                            node.children.clone(),
                            vec![node.children[1], node.children[0]],
                        ]
                    } else {
                        vec![node.children.clone()]
                    };

                    for order in orders {
                        for combo in self.solve_children(ctx, pattern, &children, order, allowed) {
                            let mut b = combo;
                            // Record the parent binding; conflict means this e-node
                            // can't realize the pattern node consistently.
                            match b[pattern_node.index()] {
                                Some(existing) if existing != class => continue,
                                _ => b[pattern_node.index()] = Some(class),
                            }
                            results.push(b);
                        }
                    }
                }
                results
            }
        }
    }

    /// Cartesian product over the children, merging assignments and dropping any
    /// combination that binds a shared pattern node to two different classes.
    fn solve_children<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        pattern_children: &[NodeId],
        class_children: &[EClassId],
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<Vec<Option<EClassId>>> {
        let mut acc: Vec<Vec<Option<EClassId>>> = vec![vec![None; pattern.len()]];
        for (&pc, &cc) in pattern_children.iter().zip(class_children.iter()) {
            let child_solutions = self.solve(ctx, pattern, pc, cc, allowed);
            let mut next = Vec::new();
            for base in &acc {
                for sol in &child_solutions {
                    if let Some(merged) = merge_bindings(base, sol) {
                        next.push(merged);
                    }
                }
            }
            acc = next;
            if acc.is_empty() {
                break;
            }
        }
        acc
    }

    fn boundary_ok<A>(
        &self,
        pattern: &Pattern<N, A>,
        pattern_node: NodeId,
        class: EClassId,
    ) -> bool {
        match pattern.operand_constraint(pattern_node) {
            Some(OperandConstraint::Register) => {
                self.nodes(class).iter().any(|n| !n.node.is_constant())
            }
            Some(OperandConstraint::Immediate) => {
                self.nodes(class).iter().any(|n| n.node.is_constant())
            }
            None => true,
        }
    }

    fn class_has_leaf(&self, ctx: &Context, class: EClassId) -> bool {
        self.nodes(class).iter().any(|n| n.node.is_leaf(ctx))
    }

    // ── Saturation ────────────────────────────────────────────────────────────

    /// Apply `rewrites` to a fixpoint (or until a limit is hit). Each iteration
    /// collects all matches against the current graph, applies them, then rebuilds
    /// congruence — the standard equality-saturation schedule.
    pub fn saturate(
        &mut self,
        ctx: &Context,
        rewrites: &[Rewrite<N, L>],
        limits: SaturationLimits,
    ) {
        for _ in 0..limits.max_iterations {
            let mut found: Vec<(usize, EMatch)> = Vec::new();
            for (i, rw) in rewrites.iter().enumerate() {
                for m in self.ematch(ctx, &rw.searcher) {
                    found.push((i, m));
                }
            }
            if found.is_empty() {
                break;
            }

            let before = self.union_find.len();
            for (i, m) in found {
                (rewrites[i].apply)(ctx, self, &m);
            }
            self.rebuild();

            // Stop once the graph stops growing or we hit the size cap; either way
            // every match producible by these rules is already represented.
            if self.union_find.len() == before || self.union_find.len() >= limits.max_classes {
                break;
            }
        }
        self.rebuild();
    }

    // ── Extraction ──────────────────────────────────────────────────────────

    /// Greedy cost-minimizing extraction: returns, per canonical class, the cheapest
    /// e-node and its total cost. `cost(label, child_costs)` is the node's own cost
    /// given the already-chosen costs of its operands. Relaxed to a fixpoint, which
    /// converges for non-negative costs. Mainly for non-PBQP (instcombine) callers;
    /// instruction selection extracts via its PBQP cover instead.
    pub fn extract(
        &self,
        cost: impl Fn(&N, &[u64]) -> u64,
    ) -> HashMap<EClassId, (ENode<N, L>, u64)> {
        let mut best: HashMap<EClassId, (ENode<N, L>, u64)> = HashMap::new();
        loop {
            let mut changed = false;
            for (&class, nodes) in &self.classes {
                for node in nodes {
                    let child_costs: Option<Vec<u64>> = node
                        .children
                        .iter()
                        .map(|c| best.get(&self.find(*c)).map(|(_, cost)| *cost))
                        .collect();
                    let Some(child_costs) = child_costs else {
                        continue;
                    };
                    let total =
                        cost(&node.node, &child_costs).saturating_add(child_costs.iter().sum());
                    let improve = match best.get(&class) {
                        Some((_, existing)) => total < *existing,
                        None => true,
                    };
                    if improve {
                        best.insert(class, (node.clone(), total));
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        best
    }
}

/// Merge two partial assignments, failing if they bind the same pattern node to
/// different classes.
fn merge_bindings(a: &[Option<EClassId>], b: &[Option<EClassId>]) -> Option<Vec<Option<EClassId>>> {
    let mut out = a.to_vec();
    for (slot, &value) in out.iter_mut().zip(b.iter()) {
        match (*slot, value) {
            (Some(x), Some(y)) if x != y => return None,
            (None, Some(y)) => *slot = Some(y),
            _ => {}
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sem_expr::ExprKind;

    fn sym(g: &mut EGraph<ExprKind, ()>) -> EClassId {
        g.add(ENode::leaf(ExprKind::Symbol, None))
    }
    fn bin(g: &mut EGraph<ExprKind, ()>, k: ExprKind, a: EClassId, b: EClassId) -> EClassId {
        g.add(ENode::op(k, vec![a, b]))
    }

    #[test]
    fn hash_consing_shares_identical_expressions() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let add1 = bin(&mut g, ExprKind::Add, a, b);
        let add2 = bin(&mut g, ExprKind::Add, a, b);
        assert_eq!(add1, add2);
    }

    #[test]
    fn union_propagates_through_congruence() {
        // sqrt(a) and sqrt(b); unioning a,b must merge sqrt(a) with sqrt(b) after
        // rebuild. (a and b are distinct leaf *kinds* since L=() carries no payload.)
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = g.add(ENode::leaf(ExprKind::Symbol, None));
        let b = g.add(ENode::leaf(ExprKind::Constant, None));
        let fa = g.add(ENode::op(ExprKind::Sqrt, vec![a]));
        let fb = g.add(ENode::op(ExprKind::Sqrt, vec![b]));
        assert_ne!(g.find(fa), g.find(fb));
        g.union(a, b);
        g.rebuild();
        assert_eq!(g.find(fa), g.find(fb));
    }

    #[test]
    fn ematch_finds_pattern_in_every_class() {
        let ctx = Context::default();
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let _add = bin(&mut g, ExprKind::Add, a, b);
        let _mul = bin(&mut g, ExprKind::Mul, a, b);

        let mut pattern = Pattern::<ExprKind, ()>::new(());
        let pl = pattern.add_node(PatternExpr::Leaf);
        let pr = pattern.add_node(PatternExpr::Leaf);
        let proot = pattern.add_node(PatternExpr::Node(ExprKind::Add));
        pattern.add_edge(proot, pl);
        pattern.add_edge(proot, pr);
        pattern.set_root(proot);

        let matches = g.ematch(&ctx, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(g.find(matches[0].root()), g.find(_add));
        assert_eq!(g.find(matches[0].binding(pl)), g.find(a));
    }

    #[test]
    fn typed_node_and_leaf_children_match_in_order() {
        let ctx = Context::default();
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let one = g.add(ENode::leaf(ExprKind::Constant, None));
        // Add(const, a): a pattern with a Constant template then a Leaf must match
        // the structurally identical e-node. (Raw ExprKind is not commutative —
        // commutative ordering is covered through SemNode in the isel tests.)
        let _add = bin(&mut g, ExprKind::Add, one, a);

        let mut pattern = Pattern::<ExprKind, ()>::new(());
        let pc = pattern.add_node(PatternExpr::Node(ExprKind::Constant));
        let pl = pattern.add_node(PatternExpr::Leaf);
        let proot = pattern.add_node(PatternExpr::Node(ExprKind::Add));
        pattern.add_edge(proot, pc);
        pattern.add_edge(proot, pl);
        pattern.set_root(proot);

        let matches = g.ematch(&ctx, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(g.find(matches[0].binding(pc)), g.find(one));
        assert_eq!(g.find(matches[0].binding(pl)), g.find(a));
    }

    #[test]
    fn saturation_adds_equivalent_form_and_extracts_cheapest() {
        let ctx = Context::default();
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let mul = bin(&mut g, ExprKind::Mul, a, b);

        // Rewrite Mul(x, y) => Add(x, y) (nonsense algebra, but exercises the loop):
        // after saturation the Mul class must also contain an Add e-node.
        let mut searcher = Pattern::<ExprKind, ()>::new(());
        let sl = searcher.add_node(PatternExpr::Boundary);
        let sr = searcher.add_node(PatternExpr::Boundary);
        let sroot = searcher.add_node(PatternExpr::Node(ExprKind::Mul));
        searcher.add_edge(sroot, sl);
        searcher.add_edge(sroot, sr);
        searcher.set_root(sroot);

        let rewrites = vec![Rewrite {
            name: "mul-to-add".to_string(),
            searcher,
            apply: Box::new(
                move |_ctx: &Context, g: &mut EGraph<ExprKind, ()>, m: &EMatch| {
                    let l = m.binding(NodeId::from_index(0));
                    let r = m.binding(NodeId::from_index(1));
                    let added = g.add(ENode::op(ExprKind::Add, vec![l, r]));
                    g.union(m.root(), added);
                },
            ),
        }];

        g.saturate(&ctx, &rewrites, SaturationLimits::default());

        assert!(
            g.nodes(mul).iter().any(|n| n.node == ExprKind::Add),
            "saturated class should contain the rewritten Add form"
        );

        // Make Add cheap, Mul expensive: extraction must pick the Add e-node.
        let best = g.extract(|kind, _| match kind {
            ExprKind::Mul => 100,
            ExprKind::Add => 1,
            _ => 1,
        });
        assert_eq!(best[&g.find(mul)].0.node, ExprKind::Add);
    }
}
