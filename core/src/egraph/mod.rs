//! A simplified e-graph: enodes live in a [`GenericDag`], equivalence classes in
//! a [`DisjointMap`] of dag [`NodeId`]s. Congruence is restored by
//! [`EGraph::rebuild`].

use std::collections::HashMap;

use crate::graph::{Dag, GenericDag, MutDag, Node, NodeId};
use crate::utils::DisjointMap;

mod ematch;
mod rewrite;

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

fn merge_members(mut a: Vec<NodeId>, mut b: Vec<NodeId>) -> Vec<NodeId> {
    a.append(&mut b);
    a
}

type EClassMap = DisjointMap<Vec<NodeId>, fn(Vec<NodeId>, Vec<NodeId>) -> Vec<NodeId>>;

pub struct EGraph<N: Node, L> {
    dag: GenericDag<N, L>,
    node_class: Vec<u32>,
    eclass: EClassMap,
}

impl<N: Node, L> Dag for EGraph<N, L> {
    type Node = N;
    type Leaf = L;

    fn len(&self) -> usize {
        self.dag.len()
    }

    fn get_node(&self, id: NodeId) -> &Self::Node {
        self.dag.get_node(id)
    }

    fn get_leaf_data(&self, id: NodeId) -> Option<&Self::Leaf> {
        self.dag.get_leaf_data(id)
    }

    fn get_original_op(&self, id: NodeId) -> Option<crate::OpId> {
        self.dag.get_original_op(id)
    }

    fn get_actual_type(&self, id: NodeId) -> Option<crate::TypeId> {
        self.dag.get_actual_type(id)
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

impl<N: Node + Clone + Eq, L: Clone + Eq> Default for EGraph<N, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Node + Clone + Eq, L: Clone + Eq> EGraph<N, L> {
    pub fn new() -> Self {
        Self {
            dag: GenericDag::new(),
            node_class: Vec::new(),
            eclass: DisjointMap::new(merge_members),
        }
    }

    pub fn dag(&self) -> &GenericDag<N, L> {
        &self.dag
    }

    pub fn find(&self, id: EClassId) -> EClassId {
        EClassId::from_raw(self.eclass.find_root(id.0))
    }

    pub fn class_of(&self, node: NodeId) -> EClassId {
        self.find(EClassId::from_raw(self.node_class[node.index()]))
    }

    pub fn num_classes(&self) -> usize {
        self.eclass.roots().count()
    }

    pub fn classes(&self) -> impl Iterator<Item = EClassId> + '_ {
        self.eclass.roots().map(|(id, _)| EClassId::from_raw(id))
    }

    pub fn nodes(&self, class: EClassId) -> &[NodeId] {
        self.eclass.get(class.0)
    }

    fn representative(&self, class: EClassId) -> NodeId {
        self.eclass.get(self.find(class).0)[0]
    }

    pub(crate) fn child_classes(&self, id: NodeId) -> Vec<EClassId> {
        self.dag
            .children(id)
            .map(|child| self.class_of(child))
            .map(|class| self.find(class))
            .collect()
    }

    fn same_shape(&self, a: NodeId, b: NodeId) -> bool {
        self.dag.get_node(a) == self.dag.get_node(b)
            && self.dag.get_leaf_data(a) == self.dag.get_leaf_data(b)
            && self.child_classes(a) == self.child_classes(b)
    }

    fn find_matching(&self, node: &N, children: &[EClassId], leaf: Option<&L>) -> Option<NodeId> {
        for index in 0..self.dag.len() {
            let id = NodeId::from_index(index);
            if self.dag.get_node(id) == node
                && self.dag.get_leaf_data(id) == leaf
                && self.child_classes(id) == children
            {
                return Some(id);
            }
        }
        None
    }

    pub fn add(&mut self, node: N, children: &[EClassId], leaf: Option<L>) -> EClassId {
        let children: Vec<EClassId> = children.iter().map(|&c| self.find(c)).collect();
        if let Some(existing) = self.find_matching(&node, &children, leaf.as_ref()) {
            return self.class_of(existing);
        }

        let id = self.dag.add_node(node);
        if let Some(data) = leaf {
            self.dag.set_leaf_data(id, data);
        }
        for &child_class in &children {
            self.dag.add_edge(id, self.representative(child_class));
        }

        let class = EClassId::from_raw(self.eclass.push(vec![id]));
        self.node_class.push(class.0);
        class
    }

    pub fn add_dag<D: Dag<Node = N, Leaf = L>>(&mut self, dag: &D, root: NodeId) -> EClassId {
        let mut memo = HashMap::new();
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
            .map(|child| self.add_dag_node(dag, child, memo))
            .collect();
        let class = self.add(
            dag.get_node(node).clone(),
            &children,
            dag.get_leaf_data(node).cloned(),
        );
        memo.insert(node.index(), class);
        class
    }

    pub fn union(&mut self, a: EClassId, b: EClassId) -> EClassId {
        let merged = EClassId::from_raw(self.eclass.union(a.0, b.0));
        for &node in self.eclass.get(merged.0) {
            self.node_class[node.index()] = merged.0;
        }
        merged
    }

    pub fn congruences(&self) -> Vec<(EClassId, EClassId)> {
        let mut out = Vec::new();
        for index in 0..self.dag.len() {
            let left = NodeId::from_index(index);
            let left_class = self.class_of(left);
            for other in index + 1..self.dag.len() {
                let right = NodeId::from_index(other);
                if self.same_shape(left, right) {
                    let right_class = self.class_of(right);
                    let (mut a, mut b) = (self.find(left_class), self.find(right_class));
                    if a == b {
                        continue;
                    }
                    if a.0 > b.0 {
                        std::mem::swap(&mut a, &mut b);
                    }
                    if !out.contains(&(a, b)) {
                        out.push((a, b));
                    }
                }
            }
        }
        out
    }

    pub fn rebuild(&mut self) {
        loop {
            let congruences = self.congruences();
            if congruences.is_empty() {
                break;
            }
            for (a, b) in congruences {
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
        g.union(a, b);
        assert_eq!(g.congruences(), vec![(fa, fb)]);
        g.union(fa, fb);
        assert_eq!(g.congruences(), vec![]);
        g.union(a, c);
        assert_eq!(g.congruences(), vec![(fb, fc)]);
        g.rebuild();
        assert_eq!(g.congruences(), vec![]);
        assert_eq!(g.find(fa), g.find(fb));
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
        let mut dag = GenericDag::<ExprKind, ()>::new();
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
    fn dag_trait_delegates_to_storage() {
        let mut g = EGraph::<ExprKind, ()>::new();
        let a = sym(&mut g);
        let node = g.nodes(a)[0];
        assert_eq!(g.get_node(node), &ExprKind::Symbol);
        assert_eq!(g.len(), 1);
    }
}
