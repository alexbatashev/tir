use std::collections::{HashMap, HashSet};

use crate::{
    Context,
    graph::{Dag, Node, NodeId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatternId(u32);

impl PatternId {
    pub fn from_index(i: usize) -> Self {
        Self(i as u32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternExpr<N: Node> {
    Any,
    Leaf,
    Node(N),
}

pub struct Pattern<N: Node, A> {
    nodes: Vec<PatternExpr<N>>,
    edges: HashMap<NodeId, Vec<NodeId>>,
    parents: HashMap<NodeId, Vec<NodeId>>,
    root: Option<NodeId>,
    applicator: A,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchBinding {
    pattern_root: NodeId,
    graph_root: NodeId,
    pattern_to_graph: Vec<NodeId>,
    covered_nodes: Vec<NodeId>,
}

impl MatchBinding {
    pub fn pattern_root(&self) -> NodeId {
        self.pattern_root
    }

    pub fn graph_root(&self) -> NodeId {
        self.graph_root
    }

    pub fn binding(&self, pattern_node: NodeId) -> NodeId {
        self.pattern_to_graph[pattern_node.index()]
    }

    pub fn pattern_to_graph(&self) -> &[NodeId] {
        &self.pattern_to_graph
    }

    pub fn covered_nodes(&self) -> &[NodeId] {
        &self.covered_nodes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverCandidate {
    pattern_id: PatternId,
    binding: MatchBinding,
}

impl CoverCandidate {
    pub fn pattern_id(&self) -> PatternId {
        self.pattern_id
    }

    pub fn binding(&self) -> &MatchBinding {
        &self.binding
    }
}

pub trait GraphCoverDriver<N: Node, L, A> {
    fn matches(
        ctx: &Context,
        g: &impl Dag<Node = N, Leaf = L>,
        pattern: &Pattern<N, A>,
    ) -> Vec<MatchBinding>;

    fn cover(
        ctx: &Context,
        g: &impl Dag<Node = N, Leaf = L>,
        patterns: &[Pattern<N, A>],
    ) -> Vec<CoverCandidate>;
}

pub struct VF2CoverDriver {}

impl<N: Node, A> Pattern<N, A> {
    pub fn new(a: A) -> Self {
        Self {
            nodes: vec![],
            edges: HashMap::new(),
            parents: HashMap::new(),
            root: None,
            applicator: a,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn applicator(&self) -> &A {
        &self.applicator
    }

    pub fn add_node(&mut self, node: PatternExpr<N>) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(node);
        id
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId) {
        self.edges.entry(from).or_default().push(to);
        self.parents.entry(to).or_default().push(from);
    }

    pub fn set_root(&mut self, root: NodeId) {
        self.root = Some(root);
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root.or_else(|| self.infer_root())
    }

    pub fn get_node(&self, id: NodeId) -> &PatternExpr<N> {
        &self.nodes[id.index()]
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.edges.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn parents(&self, id: NodeId) -> &[NodeId] {
        self.parents.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    fn infer_root(&self) -> Option<NodeId> {
        let mut roots = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, _)| NodeId::from_index(i))
            .filter(|&id| self.parents(id).is_empty());
        let root = roots.next()?;
        if roots.next().is_some() {
            return None;
        }
        Some(root)
    }

    fn preorder(&self, root: NodeId) -> Vec<NodeId> {
        fn visit<N: Node, A>(
            pattern: &Pattern<N, A>,
            node: NodeId,
            seen: &mut HashSet<NodeId>,
            out: &mut Vec<NodeId>,
        ) {
            if !seen.insert(node) {
                return;
            }

            out.push(node);
            for &child in pattern.children(node) {
                visit(pattern, child, seen, out);
            }
        }

        let mut seen = HashSet::new();
        let mut order = Vec::new();
        visit(self, root, &mut seen, &mut order);
        order
    }

    fn validate(&self, ctx: &Context) -> Result<NodeId, &'static str>
    where
        N: PartialEq,
    {
        let root = self.root().ok_or("pattern must have a unique root")?;

        fn dfs<N: Node, A>(
            pattern: &Pattern<N, A>,
            node: NodeId,
            visiting: &mut HashSet<NodeId>,
            visited: &mut HashSet<NodeId>,
        ) -> Result<(), &'static str> {
            if visited.contains(&node) {
                return Ok(());
            }
            if !visiting.insert(node) {
                return Err("pattern must be acyclic");
            }

            for &child in pattern.children(node) {
                dfs(pattern, child, visiting, visited)?;
            }

            visiting.remove(&node);
            visited.insert(node);
            Ok(())
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        dfs(self, root, &mut visiting, &mut visited)?;

        if visited.len() != self.len() {
            return Err("pattern must be connected from its root");
        }

        for (i, expr) in self.nodes.iter().enumerate() {
            let node = NodeId::from_index(i);
            let arity = self.children(node).len();
            match expr {
                PatternExpr::Any => {}
                PatternExpr::Leaf => {
                    if arity != 0 {
                        return Err("leaf pattern nodes cannot have children");
                    }
                }
                PatternExpr::Node(kind) => {
                    if kind.is_leaf(ctx) && arity != 0 {
                        return Err("leaf pattern nodes cannot have children");
                    }
                    if kind.num_children(ctx) != arity {
                        return Err("pattern node arity must match the node kind arity");
                    }
                }
            }
        }

        Ok(root)
    }
}

impl<N: Node + PartialEq, L, A> GraphCoverDriver<N, L, A> for VF2CoverDriver {
    fn matches(
        ctx: &Context,
        g: &impl Dag<Node = N, Leaf = L>,
        pattern: &Pattern<N, A>,
    ) -> Vec<MatchBinding> {
        let Ok(pattern_root) = pattern.validate(ctx) else {
            return Vec::new();
        };
        if pattern.is_empty() || g.len() == 0 {
            return Vec::new();
        }

        let pattern_order = pattern.preorder(pattern_root);
        let graph_children: Vec<Vec<NodeId>> = (0..g.len())
            .map(|i| g.children(NodeId::from_index(i)).collect())
            .collect();

        struct SearchState {
            pattern_to_graph: Vec<Option<NodeId>>,
            graph_to_pattern: HashMap<NodeId, NodeId>,
        }

        fn next_pattern_node<N: Node, A>(
            pattern: &Pattern<N, A>,
            order: &[NodeId],
            state: &SearchState,
            root: NodeId,
        ) -> Option<NodeId> {
            if state.pattern_to_graph[root.index()].is_none() {
                return Some(root);
            }

            order.iter().copied().find(|&node| {
                state.pattern_to_graph[node.index()].is_none()
                    && pattern
                        .parents(node)
                        .iter()
                        .any(|parent| state.pattern_to_graph[parent.index()].is_some())
            })
        }

        fn forced_graph_candidate<N: Node, A>(
            pattern: &Pattern<N, A>,
            graph_children: &[Vec<NodeId>],
            pattern_node: NodeId,
            state: &SearchState,
        ) -> Result<Option<NodeId>, ()> {
            let mut forced: Option<NodeId> = None;

            for &parent in pattern.parents(pattern_node) {
                let Some(graph_parent) = state.pattern_to_graph[parent.index()] else {
                    continue;
                };

                for (slot, &child) in pattern.children(parent).iter().enumerate() {
                    if child != pattern_node {
                        continue;
                    }

                    let Some(graph_child) = graph_children[graph_parent.index()].get(slot).copied()
                    else {
                        return Err(());
                    };
                    match forced {
                        Some(existing) if existing != graph_child => return Err(()),
                        Some(_) => {}
                        None => forced = Some(graph_child),
                    }
                }
            }

            Ok(forced)
        }

        fn node_compatible<N: Node + PartialEq, L, A>(
            ctx: &Context,
            pattern: &Pattern<N, A>,
            g: &impl Dag<Node = N, Leaf = L>,
            graph_children: &[Vec<NodeId>],
            pattern_node: NodeId,
            graph_node: NodeId,
        ) -> bool {
            let pattern_arity = pattern.children(pattern_node).len();
            if graph_children[graph_node.index()].len() != pattern_arity {
                return false;
            }

            match pattern.get_node(pattern_node) {
                PatternExpr::Any => true,
                PatternExpr::Leaf => g.get_kind(graph_node).is_leaf(ctx),
                PatternExpr::Node(kind) => g.get_kind(graph_node) == kind,
            }
        }

        fn feasible<N: Node + PartialEq, L, A>(
            ctx: &Context,
            pattern: &Pattern<N, A>,
            g: &impl Dag<Node = N, Leaf = L>,
            graph_children: &[Vec<NodeId>],
            state: &SearchState,
            pattern_node: NodeId,
            graph_node: NodeId,
        ) -> bool {
            if state.graph_to_pattern.contains_key(&graph_node) {
                return false;
            }

            if !node_compatible(ctx, pattern, g, graph_children, pattern_node, graph_node) {
                return false;
            }

            for &parent in pattern.parents(pattern_node) {
                let Some(graph_parent) = state.pattern_to_graph[parent.index()] else {
                    continue;
                };

                for (slot, &child) in pattern.children(parent).iter().enumerate() {
                    if child != pattern_node {
                        continue;
                    }
                    if graph_children[graph_parent.index()].get(slot).copied() != Some(graph_node) {
                        return false;
                    }
                }
            }

            for (slot, &pattern_child) in pattern.children(pattern_node).iter().enumerate() {
                let Some(graph_child) = state.pattern_to_graph[pattern_child.index()] else {
                    continue;
                };
                if graph_children[graph_node.index()].get(slot).copied() != Some(graph_child) {
                    return false;
                }
            }

            true
        }

        fn search<N: Node + PartialEq, L, A>(
            ctx: &Context,
            pattern: &Pattern<N, A>,
            g: &impl Dag<Node = N, Leaf = L>,
            graph_children: &[Vec<NodeId>],
            pattern_order: &[NodeId],
            pattern_root: NodeId,
            state: &mut SearchState,
            out: &mut Vec<MatchBinding>,
        ) {
            let Some(pattern_node) = next_pattern_node(pattern, pattern_order, state, pattern_root)
            else {
                let pattern_to_graph: Vec<_> = state
                    .pattern_to_graph
                    .iter()
                    .copied()
                    .map(Option::unwrap)
                    .collect();
                let mut covered_nodes = pattern_to_graph.clone();
                covered_nodes.sort_by_key(|node| node.index());

                out.push(MatchBinding {
                    pattern_root,
                    graph_root: pattern_to_graph[pattern_root.index()],
                    pattern_to_graph,
                    covered_nodes,
                });
                return;
            };

            let candidates: Vec<NodeId> =
                match forced_graph_candidate(pattern, graph_children, pattern_node, state) {
                    Ok(Some(candidate)) => vec![candidate],
                    Ok(None) => (0..g.len()).map(NodeId::from_index).collect(),
                    Err(()) => return,
                };

            for graph_node in candidates {
                if !feasible(
                    ctx,
                    pattern,
                    g,
                    graph_children,
                    state,
                    pattern_node,
                    graph_node,
                ) {
                    continue;
                }

                state.pattern_to_graph[pattern_node.index()] = Some(graph_node);
                state.graph_to_pattern.insert(graph_node, pattern_node);

                search(
                    ctx,
                    pattern,
                    g,
                    graph_children,
                    pattern_order,
                    pattern_root,
                    state,
                    out,
                );

                state.pattern_to_graph[pattern_node.index()] = None;
                state.graph_to_pattern.remove(&graph_node);
            }
        }

        let mut results = Vec::new();
        let mut state = SearchState {
            pattern_to_graph: vec![None; pattern.len()],
            graph_to_pattern: HashMap::new(),
        };

        search(
            ctx,
            pattern,
            g,
            &graph_children,
            &pattern_order,
            pattern_root,
            &mut state,
            &mut results,
        );

        results
    }

    fn cover(
        ctx: &Context,
        g: &impl Dag<Node = N, Leaf = L>,
        patterns: &[Pattern<N, A>],
    ) -> Vec<CoverCandidate> {
        let mut candidates = Vec::new();

        for (i, pattern) in patterns.iter().enumerate() {
            let pattern_id = PatternId::from_index(i);
            for binding in Self::matches(ctx, g, pattern) {
                candidates.push(CoverCandidate {
                    pattern_id,
                    binding,
                });
            }
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context,
        graph::{MutDag, NodeId, PostOrderDag},
        sem_expr::ExprKind,
    };

    use super::{GraphCoverDriver, Pattern, PatternExpr, VF2CoverDriver};

    fn graph_symbol(g: &mut PostOrderDag<ExprKind, ()>) -> NodeId {
        g.add_node(ExprKind::Symbol)
    }

    fn graph_binary(
        g: &mut PostOrderDag<ExprKind, ()>,
        kind: ExprKind,
        lhs: NodeId,
        rhs: NodeId,
    ) -> NodeId {
        let node = g.add_node(kind);
        g.add_edge(node, lhs);
        g.add_edge(node, rhs);
        node
    }

    #[test]
    fn exact_binary_match_finds_rooted_embedding() {
        let ctx = Context::default();
        let mut g = PostOrderDag::<ExprKind, ()>::new();
        let lhs = graph_symbol(&mut g);
        let rhs = graph_symbol(&mut g);
        let add = graph_binary(&mut g, ExprKind::Add, lhs, rhs);

        let mut pattern = Pattern::new("add");
        let p_lhs = pattern.add_node(PatternExpr::Leaf);
        let p_rhs = pattern.add_node(PatternExpr::Leaf);
        let p_add = pattern.add_node(PatternExpr::Node(ExprKind::Add));
        pattern.add_edge(p_add, p_lhs);
        pattern.add_edge(p_add, p_rhs);
        pattern.set_root(p_add);

        let matches = VF2CoverDriver::matches(&ctx, &g, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].graph_root(), add);
        assert_eq!(matches[0].binding(p_lhs), lhs);
        assert_eq!(matches[0].binding(p_rhs), rhs);
    }

    #[test]
    fn wildcard_root_matches_multiple_nodes() {
        let ctx = Context::default();
        let mut g = PostOrderDag::<ExprKind, ()>::new();
        let lhs = graph_symbol(&mut g);
        let rhs = graph_symbol(&mut g);
        let _add = graph_binary(&mut g, ExprKind::Add, lhs, rhs);
        let _mul = graph_binary(&mut g, ExprKind::Mul, lhs, rhs);

        let mut pattern = Pattern::new("binary");
        let p_lhs = pattern.add_node(PatternExpr::Leaf);
        let p_rhs = pattern.add_node(PatternExpr::Leaf);
        let p_root = pattern.add_node(PatternExpr::Any);
        pattern.add_edge(p_root, p_lhs);
        pattern.add_edge(p_root, p_rhs);
        pattern.set_root(p_root);

        let matches = VF2CoverDriver::matches(&ctx, &g, &pattern);
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn child_order_is_respected() {
        let ctx = Context::default();
        let mut g = PostOrderDag::<ExprKind, ()>::new();
        let lhs = graph_symbol(&mut g);
        let rhs = graph_symbol(&mut g);
        let add = graph_binary(&mut g, ExprKind::Add, lhs, rhs);

        let mut pattern = Pattern::new("ordered-add");
        let p_lhs = pattern.add_node(PatternExpr::Node(ExprKind::Symbol));
        let p_rhs = pattern.add_node(PatternExpr::Node(ExprKind::Symbol));
        let p_add = pattern.add_node(PatternExpr::Node(ExprKind::Add));
        pattern.add_edge(p_add, p_rhs);
        pattern.add_edge(p_add, p_lhs);
        pattern.set_root(p_add);

        let matches = VF2CoverDriver::matches(&ctx, &g, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].graph_root(), add);
        assert_eq!(matches[0].binding(p_rhs), lhs);
        assert_eq!(matches[0].binding(p_lhs), rhs);
    }

    #[test]
    fn shared_subdag_match_is_supported() {
        let ctx = Context::default();
        let mut g = PostOrderDag::<ExprKind, ()>::new();
        let x = graph_symbol(&mut g);
        let y = graph_symbol(&mut g);
        let add = graph_binary(&mut g, ExprKind::Add, x, y);
        let mul = graph_binary(&mut g, ExprKind::Mul, add, add);

        let mut pattern = Pattern::new("mul-add-add");
        let p_x = pattern.add_node(PatternExpr::Leaf);
        let p_y = pattern.add_node(PatternExpr::Leaf);
        let p_add = pattern.add_node(PatternExpr::Node(ExprKind::Add));
        let p_mul = pattern.add_node(PatternExpr::Node(ExprKind::Mul));
        pattern.add_edge(p_add, p_x);
        pattern.add_edge(p_add, p_y);
        pattern.add_edge(p_mul, p_add);
        pattern.add_edge(p_mul, p_add);
        pattern.set_root(p_mul);

        let matches = VF2CoverDriver::matches(&ctx, &g, &pattern);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].graph_root(), mul);
        assert_eq!(matches[0].binding(p_add), add);
    }

    #[test]
    fn cover_enumerates_candidates_across_patterns() {
        let ctx = Context::default();
        let mut g = PostOrderDag::<ExprKind, ()>::new();
        let lhs = graph_symbol(&mut g);
        let rhs = graph_symbol(&mut g);
        let _add = graph_binary(&mut g, ExprKind::Add, lhs, rhs);
        let _mul = graph_binary(&mut g, ExprKind::Mul, lhs, rhs);

        let mut add_pattern = Pattern::new("add");
        let add_lhs = add_pattern.add_node(PatternExpr::Leaf);
        let add_rhs = add_pattern.add_node(PatternExpr::Leaf);
        let add_root = add_pattern.add_node(PatternExpr::Node(ExprKind::Add));
        add_pattern.add_edge(add_root, add_lhs);
        add_pattern.add_edge(add_root, add_rhs);
        add_pattern.set_root(add_root);

        let mut mul_pattern = Pattern::new("mul");
        let mul_lhs = mul_pattern.add_node(PatternExpr::Leaf);
        let mul_rhs = mul_pattern.add_node(PatternExpr::Leaf);
        let mul_root = mul_pattern.add_node(PatternExpr::Node(ExprKind::Mul));
        mul_pattern.add_edge(mul_root, mul_lhs);
        mul_pattern.add_edge(mul_root, mul_rhs);
        mul_pattern.set_root(mul_root);

        let candidates = VF2CoverDriver::cover(&ctx, &g, &[add_pattern, mul_pattern]);
        assert_eq!(candidates.len(), 2);
        assert_ne!(
            candidates[0].binding().graph_root(),
            candidates[1].binding().graph_root()
        );
    }
}
