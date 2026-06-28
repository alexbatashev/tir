//! A simplified e-graph: enodes live in a [`GenericDag`], equivalence classes in
//! a [`ContextUnionFind`] of dag [`NodeId`]s. Congruence is restored by
//! [`EGraph::rebuild`].
//!
//! The equality core is *contextual*: [`EGraph::push_context`] enters a scope,
//! unions made inside it (e.g. an assumed branch condition) are discarded by
//! [`EGraph::pop_context`], which re-derives the enclosing scope's congruence.
//! With no context open it is a plain union-find.

use std::collections::HashMap;
use std::hash::Hash;

use crate::graph::{Dag, GenericDag, Matchable, MetaDag, MetaMutDag, MutDag, NodeId, NodeMeta};
use crate::utils::ScopedDisjointSet;
use crate::{Context, OpId, TypeId};

mod ematch;
mod print;
mod rewrite;

pub use print::{DotLabel, EGPrinter};
pub use rewrite::{Applier, EMatch, Rewrite, SaturationLimits};

/// Identifier of an e-class. May be non-canonical after unions — pass through
/// [`EGraph::find`] before comparing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EClassId(u32);

impl EClassId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    fn from_raw(i: u32) -> Self {
        EClassId(i)
    }
}

/// Hash-cons index: `(label, canonical children) -> [(leaf, class)]`. The leaf is
/// part of e-node identity but `L` may carry floats, so it stays out of the hash
/// key and entries sharing a key are told apart by `PartialEq` on the leaf.
type ENodeMemo<N, L> = HashMap<(N, Vec<EClassId>), Vec<(Option<L>, EClassId)>>;

pub struct EGraph<N, L> {
    dag: GenericDag<N, L, NodeMeta>,
    node_class: Vec<u32>,
    uf: ScopedDisjointSet,
    /// Canonical class id -> its member e-nodes. Maintained on `add`/`union` and
    /// fully recomputed by `rebuild` (so it survives `pop_context`).
    members: HashMap<u32, Vec<NodeId>>,
    memo: ENodeMemo<N, L>,
    /// Provenance: which rewrite (its index in the saturation set) first introduced
    /// each node, or `None` for nodes that predate saturation (seeded directly).
    /// Set by [`Self::saturate`] and read back so a caller can ask the producing
    /// rewrite to materialize the node it chose.
    node_producer: Vec<Option<usize>>,
}

impl<N, L> Dag for EGraph<N, L> {
    type Node = N;
    type Leaf = L;
    type Annotation = NodeMeta;

    fn len(&self) -> usize {
        self.dag.len()
    }

    fn get_node(&self, id: NodeId) -> &Self::Node {
        self.dag.get_node(id)
    }

    fn get_leaf_data(&self, id: NodeId) -> Option<&Self::Leaf> {
        self.dag.get_leaf_data(id)
    }

    fn get_annotation(&self, id: NodeId) -> Option<&Self::Annotation> {
        self.dag.get_annotation(id)
    }

    fn root(&self) -> Option<NodeId> {
        self.dag.root()
    }

    fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.dag.children(id)
    }

    fn postorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        self.dag.postorder(start)
    }

    fn preorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        self.dag.preorder(start)
    }
}

impl<N: Matchable<Context> + Clone + Eq + Hash, L: Clone + PartialEq> Default for EGraph<N, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Matchable<Context> + Clone + Eq + Hash, L: Clone + PartialEq> EGraph<N, L> {
    pub fn new() -> Self {
        Self {
            dag: GenericDag::new(),
            node_class: Vec::new(),
            uf: ScopedDisjointSet::new(0),
            members: HashMap::new(),
            memo: HashMap::new(),
            node_producer: Vec::new(),
        }
    }

    /// The rewrite that first introduced `node`, or `None` if it was seeded.
    pub fn producer(&self, node: NodeId) -> Option<usize> {
        self.node_producer.get(node.index()).copied().flatten()
    }

    pub(crate) fn set_producer(&mut self, node: NodeId, producer: usize) {
        self.node_producer[node.index()] = Some(producer);
    }

    pub fn dag(&self) -> &GenericDag<N, L, NodeMeta> {
        &self.dag
    }

    /// Enter an assumption scope. Unions performed until the matching
    /// [`EGraph::pop_context`] are local to it.
    pub fn push_context(&mut self) {
        self.uf.push_context();
    }

    /// Leave the current assumption scope, dropping its unions and re-deriving the
    /// enclosing scope's congruence and hash-cons index.
    pub fn pop_context(&mut self) {
        self.uf.pop_context();
        self.rebuild();
    }

    pub fn find(&self, id: EClassId) -> EClassId {
        EClassId::from_raw(self.uf.find(id.0))
    }

    pub fn class_of(&self, node: NodeId) -> EClassId {
        self.find(EClassId::from_raw(self.node_class[node.index()]))
    }

    pub fn num_classes(&self) -> usize {
        self.members.len()
    }

    pub fn classes(&self) -> impl Iterator<Item = EClassId> + '_ {
        self.members.keys().map(|&id| EClassId::from_raw(id))
    }

    pub fn nodes(&self, class: EClassId) -> &[NodeId] {
        self.members
            .get(&self.find(class).0)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn representative(&self, class: EClassId) -> NodeId {
        self.nodes(class)[0]
    }

    pub(crate) fn child_classes(&self, id: NodeId) -> Vec<EClassId> {
        self.dag
            .children(id)
            .map(|child| self.class_of(child))
            .map(|class| self.find(class))
            .collect()
    }

    /// The canonical class of an already-interned e-node with these canonical
    /// `children`, or `None`. O(bucket) rather than O(graph): a hash lookup then a
    /// `PartialEq` scan over the few entries that share the `(label, children)` key.
    fn memo_get(&self, node: &N, children: &[EClassId], leaf: Option<&L>) -> Option<EClassId> {
        let bucket = self.memo.get(&(node.clone(), children.to_vec()))?;
        bucket
            .iter()
            .find(|(l, _)| l.as_ref() == leaf)
            .map(|&(_, class)| self.find(class))
    }

    fn memo_put(&mut self, node: &N, children: &[EClassId], leaf: Option<&L>, class: EClassId) {
        self.memo
            .entry((node.clone(), children.to_vec()))
            .or_default()
            .push((leaf.cloned(), class));
    }

    pub fn add(&mut self, node: N, children: &[EClassId], leaf: Option<L>) -> EClassId {
        self.add_inner(node, children, leaf, None)
    }

    /// Annotations a source [`Dag`] node may carry, preserved verbatim onto the
    /// enode. They live outside the leaf payload so they never affect e-node
    /// identity (two structurally equal nodes still hash-cons regardless of which
    /// op produced them), and survive on the stable [`NodeId`] through saturation.
    fn add_inner(
        &mut self,
        node: N,
        children: &[EClassId],
        leaf: Option<L>,
        meta: Option<(OpId, TypeId)>,
    ) -> EClassId {
        let children: Vec<EClassId> = children.iter().map(|&c| self.find(c)).collect();
        if let Some(existing) = self.memo_get(&node, &children, leaf.as_ref()) {
            return existing;
        }

        let id = self.dag.add_node(node.clone());
        if let Some(data) = leaf.clone() {
            self.dag.set_leaf_data(id, data);
        }
        for &child_class in &children {
            self.dag.add_edge(id, self.representative(child_class));
        }
        if let Some((original, ty)) = meta {
            self.dag.set_original_op(id, original);
            self.dag.set_actual_type(id, ty);
        }

        let elem = self.uf.add();
        self.node_class.push(elem);
        self.node_producer.push(None);
        self.members.insert(elem, vec![id]);
        let class = EClassId::from_raw(elem);
        self.memo_put(&node, &children, leaf.as_ref(), class);
        class
    }

    pub fn add_dag<D: Dag<Node = N, Leaf = L, Annotation = NodeMeta>>(
        &mut self,
        dag: &D,
        root: NodeId,
    ) -> EClassId {
        let mut memo = HashMap::new();
        self.add_dag_node(dag, root, &mut memo)
    }

    fn add_dag_node<D: Dag<Node = N, Leaf = L, Annotation = NodeMeta>>(
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
            .map(|child| self.add_dag_node(dag, child, memo))
            .collect();
        let meta = dag.get_original_op(node).zip(dag.get_actual_type(node));
        let class = self.add_inner(
            dag.get_node(node).clone(),
            &children,
            dag.get_leaf_data(node).cloned(),
            meta,
        );
        memo.insert(node.index(), class);
        class
    }

    pub fn union(&mut self, a: EClassId, b: EClassId) -> EClassId {
        let ra = self.find(a).0;
        let rb = self.find(b).0;
        if ra == rb {
            return EClassId::from_raw(ra);
        }
        let lo = self.uf.union(a.0, b.0);
        let hi = if lo == ra { rb } else { ra };
        if let Some(mut hi_members) = self.members.remove(&hi) {
            self.members.entry(lo).or_default().append(&mut hi_members);
        }
        EClassId::from_raw(lo)
    }

    /// Regroup every node under its current canonical class. The class ids change
    /// as the context stack and unions evolve, so `members` is rebuilt from
    /// `node_class` rather than maintained across context boundaries.
    fn recompute_members(&mut self) {
        self.members.clear();
        for index in 0..self.dag.len() {
            let id = NodeId::from_index(index);
            let class = self.find(self.class_of(id)).0;
            self.members.entry(class).or_default().push(id);
        }
    }

    /// Restore the congruence invariant after a batch of unions: regroup every
    /// e-node by `(label, canonical children, leaf)`, union classes that collide,
    /// and repeat to a fixpoint. Each sweep is O(n) through the `seen` index (the
    /// previous all-pairs scan was O(n²)); the final index becomes the memo so
    /// hash-consing stays canonical afterwards. Unions land in the active context,
    /// so a `pop_context` rebuild restores the enclosing scope's congruence.
    pub fn rebuild(&mut self) {
        loop {
            self.recompute_members();
            let mut seen: ENodeMemo<N, L> = HashMap::new();
            let mut merges: Vec<(EClassId, EClassId)> = Vec::new();

            for index in 0..self.dag.len() {
                let id = NodeId::from_index(index);
                let class = self.find(self.class_of(id));
                let key = (self.dag.get_node(id).clone(), self.child_classes(id));
                let leaf = self.dag.get_leaf_data(id).cloned();
                let bucket = seen.entry(key).or_default();
                let found = bucket.iter().find(|(l, _)| *l == leaf).map(|&(_, c)| c);
                match found {
                    Some(other) if other != class => merges.push((other, class)),
                    Some(_) => {}
                    None => bucket.push((leaf, class)),
                }
            }

            if merges.is_empty() {
                self.memo = seen;
                break;
            }
            for (a, b) in merges {
                self.union(a, b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sem_expr::ExprKind;

    fn sym(g: &mut EGraph<ExprKind, ()>) -> EClassId {
        g.add(ExprKind::Symbol, &[], None)
    }

    fn unary(g: &mut EGraph<ExprKind, ()>, k: ExprKind, a: EClassId) -> EClassId {
        g.add(k, &[a], None)
    }

    #[test]
    fn hash_consing_shares_identical_expressions() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let add1 = g.add(ExprKind::Add, &[a, b], None);
        let add2 = g.add(ExprKind::Add, &[a, b], None);
        assert_eq!(g.find(add1), g.find(add2));
        assert_eq!(g.nodes(add1).len(), 1);
    }

    #[test]
    fn union_merges_classes() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = g.add(ExprKind::Symbol, &[], None);
        let b = g.add(ExprKind::Constant, &[], None);
        let c = g.add(ExprKind::Sqrt, &[], None);
        g.union(a, b);
        assert_eq!(g.find(a), g.find(b));
        assert_ne!(g.find(a), g.find(c));
        assert_eq!(g.add(ExprKind::Symbol, &[], None), g.find(b));
    }

    #[test]
    fn congruence_merges_function_applications() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = g.add(ExprKind::Constant, &[], None);
        let c = g.add(ExprKind::Sqrt, &[], None);
        let fa = unary(&mut g, ExprKind::Sqrt, a);
        let fb = unary(&mut g, ExprKind::Sqrt, b);
        let fc = unary(&mut g, ExprKind::Sqrt, c);

        // Sqrt(a) and Sqrt(b) become congruent once a and b are merged.
        g.union(a, b);
        g.rebuild();
        assert_eq!(g.find(fa), g.find(fb));
        assert_ne!(g.find(fb), g.find(fc));

        // Folding c in too collapses all three Sqrt applications into one class.
        g.union(a, c);
        g.rebuild();
        assert_eq!(g.find(fc), g.find(fb));
    }

    #[test]
    fn union_merges_without_congruence_repair() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let mut cur = a;
        for _ in 0..5 {
            cur = unary(&mut g, ExprKind::Sqrt, cur);
        }
        let f5a = cur;
        let inner = unary(&mut g, ExprKind::Sqrt, a);
        let f2a = unary(&mut g, ExprKind::Sqrt, inner);
        assert_eq!(g.num_classes(), 6);
        g.union(f5a, f2a);
        assert_eq!(g.find(f5a), g.find(f2a));
        assert_eq!(g.num_classes(), 5);
    }

    #[test]
    fn rebuild_propagates_congruence_to_fixpoint() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let mut cur = a;
        for _ in 0..5 {
            cur = unary(&mut g, ExprKind::Sqrt, cur);
        }
        let _f5a = cur;
        let fa = unary(&mut g, ExprKind::Sqrt, a);
        assert_eq!(g.num_classes(), 6);
        g.union(fa, a);
        g.rebuild();
        assert_eq!(g.num_classes(), 1);
    }

    #[test]
    fn add_dag_seeds_expression_tree() {
        let ctx = crate::Context::default();
        let mut dag = GenericDag::<ExprKind, (), NodeMeta>::new();
        let a = dag.add_node(ExprKind::Symbol);
        let b = dag.add_node(ExprKind::Constant);
        let add = dag.add_node(ExprKind::Add);
        dag.add_edge(add, a);
        dag.add_edge(add, b);

        let mut g = EGraph::<ExprKind, ()>::new();
        let class = g.add_dag(&dag, add);

        assert_eq!(g.nodes(class).len(), 1);
        assert_eq!(g.dag().get_node(g.nodes(class)[0]), &ExprKind::Add);
        assert_eq!(g.dag().children(g.nodes(class)[0]).count(), 2);
        for child in g.dag().children(g.nodes(class)[0]) {
            assert!(g.dag().get_node(child).is_leaf(&ctx));
        }
    }

    #[test]
    fn add_dag_preserves_source_annotations() {
        use crate::Operation;
        let ctx = crate::Context::with_default_dialects();
        let ty = crate::builtin::IntegerType::new(&ctx, 32);
        let op = crate::builtin::ops::constant(&ctx, 1, ty).build();

        let mut dag = GenericDag::<ExprKind, (), NodeMeta>::new();
        let a = dag.add_node(ExprKind::Symbol);
        dag.set_original_op(a, op.id());
        dag.set_actual_type(a, ty);

        let mut g = EGraph::<ExprKind, ()>::new();
        let class = g.add_dag(&dag, a);
        let node = g.nodes(class)[0];
        assert_eq!(g.get_original_op(node), Some(op.id()));
        assert_eq!(g.get_actual_type(node), Some(ty));
    }

    #[test]
    fn ematch_finds_pattern_in_graph() {
        let ctx = crate::Context::default();
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let add = g.add(ExprKind::Add, &[a, b], None);

        let mut pattern = crate::graph::Pattern::<ExprKind, ()>::new(());
        let pl = pattern.add_node(crate::graph::PatternExpr::Leaf);
        let pr = pattern.add_node(crate::graph::PatternExpr::Leaf);
        let proot = pattern.add_node(crate::graph::PatternExpr::Node(ExprKind::Add));
        pattern.add_edge(proot, pl);
        pattern.add_edge(proot, pr);
        pattern.set_root(proot);

        let matches = g.ematch(&ctx, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(g.find(matches[0].root()), g.find(add));
    }

    #[test]
    fn saturation_adds_equivalent_form_and_extracts_cheapest() {
        let ctx = crate::Context::default();
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = sym(&mut g);
        let mul = g.add(ExprKind::Mul, &[a, b], None);

        let mut searcher = crate::graph::Pattern::<ExprKind, ()>::new(());
        let sl = searcher.add_node(crate::graph::PatternExpr::Boundary);
        let sr = searcher.add_node(crate::graph::PatternExpr::Boundary);
        let sroot = searcher.add_node(crate::graph::PatternExpr::Node(ExprKind::Mul));
        searcher.add_edge(sroot, sl);
        searcher.add_edge(sroot, sr);
        searcher.set_root(sroot);

        let rewrites = vec![Rewrite::new(
            "mul-to-add",
            searcher,
            Box::new(move |_ctx, g, m| {
                let l = m.binding(NodeId::from_index(0));
                let r = m.binding(NodeId::from_index(1));
                let added = g.add(ExprKind::Add, &[l, r], None);
                g.union(m.root(), added);
            }),
        )];

        g.saturate(&ctx, &rewrites, SaturationLimits::default());

        assert!(
            g.nodes(mul)
                .iter()
                .any(|&id| g.dag().get_node(id) == &ExprKind::Add)
        );

        let best = g.extract_best(|kind, _| match kind {
            ExprKind::Mul => 100,
            ExprKind::Add => 1,
            _ => 1,
        });
        assert_eq!(g.dag().get_node(best[&g.find(mul)].0), &ExprKind::Add);
    }

    #[test]
    fn context_union_is_scoped() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = g.add(ExprKind::Constant, &[], None);
        g.push_context();
        g.union(a, b);
        assert_eq!(g.find(a), g.find(b));
        g.pop_context();
        assert_ne!(g.find(a), g.find(b));
    }

    #[test]
    fn context_congruence_collapses_and_restores() {
        // f(a) and f(b) are distinct in the base scope; assuming a≡b inside a
        // context makes them congruent, and popping restores the distinction.
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = g.add(ExprKind::Constant, &[], None);
        let fa = unary(&mut g, ExprKind::Sqrt, a);
        let fb = unary(&mut g, ExprKind::Sqrt, b);
        g.rebuild();
        assert_ne!(g.find(fa), g.find(fb));

        g.push_context();
        g.union(a, b);
        g.rebuild();
        assert_eq!(g.find(fa), g.find(fb));

        g.pop_context();
        assert_ne!(g.find(a), g.find(b));
        assert_ne!(g.find(fa), g.find(fb));
    }

    impl DotLabel<()> for ExprKind {
        fn dot_label(&self, _leaf: Option<&()>) -> String {
            format!("{self:?}")
        }
    }

    #[test]
    fn printer_emits_cluster_per_class_with_child_edges() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let b = g.add(ExprKind::Constant, &[], None);
        let add = g.add(ExprKind::Add, &[a, b], None);

        let dot = EGPrinter::new(&g).to_dot();
        assert!(dot.starts_with("digraph egraph {"));
        // One cluster per class.
        assert_eq!(dot.matches("subgraph cluster_").count(), 3);
        assert!(dot.contains("label=\"Add\""));
        assert!(dot.contains("label=\"Symbol\""));
        // The Add node points at both operand clusters.
        let add_node = g.nodes(add)[0].index();
        assert_eq!(dot.matches(&format!("n{add_node} -> ")).count(), 2);
    }

    #[test]
    fn dag_trait_delegates_to_storage() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let node = g.nodes(a)[0];
        assert_eq!(g.get_node(node), &ExprKind::Symbol);
        assert_eq!(g.len(), 1);
    }
}
