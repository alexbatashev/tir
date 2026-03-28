use crate::Context;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn from_index(i: usize) -> Self {
        NodeId(i as u32)
    }
}

pub trait Node {
    fn is_leaf(&self, ctx: &Context) -> bool;

    fn num_children(&self, ctx: &Context) -> usize;
}

pub trait Dag<N: Node, L> {
    fn children(&self, node: NodeId) -> &[NodeId];

    fn get_kind(&self, node: NodeId) -> &N;

    fn get_leaf_data(&self, node: NodeId) -> Option<&L>;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The root is the last node added (highest post-order index).
    fn root(&self) -> Option<NodeId>;

    /// Add a leaf node with an associated payload. Returns its `NodeId`.
    fn add_leaf(&mut self, kind: N, data: L) -> NodeId;

    /// Add an interior node with the given children. All children must already
    /// be present in the DAG (enforcing post-order). Returns its `NodeId`.
    fn add_inner(&mut self, kind: N, children: &[NodeId]) -> NodeId;
}

/// A DAG whose nodes are stored in post-order: every child appears before its
/// parent. Children are stored in CSR (compressed sparse row) format for
/// cache-efficient traversal.
pub struct PostOrderDag<N: Node, L> {
    /// Node kinds in post-order.
    nodes: Vec<N>,
    /// subtree_size[i] = number of nodes in the subtree rooted at node i.
    subtree_size: Vec<u32>,
    /// Leaf payloads in insertion order.
    leaf_data: Vec<L>,
    /// leaf_data_idx[i] = index into leaf_data for node i, or u32::MAX for
    /// interior nodes.
    leaf_data_idx: Vec<u32>,
    /// Flat child list (CSR values).
    child_buf: Vec<NodeId>,
    /// child_buf[child_start[i]..child_start[i+1]] are the children of node i.
    /// Length is always nodes.len() + 1.
    child_start: Vec<u32>,
}

impl<N: Node, L> PostOrderDag<N, L> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            subtree_size: Vec::new(),
            leaf_data: Vec::new(),
            leaf_data_idx: Vec::new(),
            child_buf: Vec::new(),
            child_start: vec![0],
        }
    }

    /// Number of nodes in the subtree rooted at `node`.
    pub fn subtree_size(&self, node: NodeId) -> u32 {
        self.subtree_size[node.0 as usize]
    }
}

impl<NK: Node, L> Default for PostOrderDag<NK, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<NK: Node, L> Dag<NK, L> for PostOrderDag<NK, L> {
    fn children(&self, node: NodeId) -> &[NodeId] {
        let i = node.0 as usize;
        let start = self.child_start[i] as usize;
        let end = self.child_start[i + 1] as usize;
        &self.child_buf[start..end]
    }

    fn get_kind(&self, node: NodeId) -> &NK {
        &self.nodes[node.0 as usize]
    }

    fn get_leaf_data(&self, node: NodeId) -> Option<&L> {
        let idx = self.leaf_data_idx[node.0 as usize];
        if idx == u32::MAX {
            None
        } else {
            Some(&self.leaf_data[idx as usize])
        }
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn root(&self) -> Option<NodeId> {
        if self.nodes.is_empty() {
            None
        } else {
            Some(NodeId(self.nodes.len() as u32 - 1))
        }
    }

    fn add_leaf(&mut self, kind: NK, data: L) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        let leaf_idx = self.leaf_data.len() as u32;
        self.nodes.push(kind);
        self.subtree_size.push(1);
        self.leaf_data.push(data);
        self.leaf_data_idx.push(leaf_idx);
        self.child_start.push(self.child_buf.len() as u32);
        id
    }

    fn add_inner(&mut self, kind: NK, children: &[NodeId]) -> NodeId {
        debug_assert!(
            children.iter().all(|c| c.0 < self.nodes.len() as u32),
            "all children must be inserted before their parent"
        );
        let id = NodeId(self.nodes.len() as u32);
        let subtree_size: u32 = 1 + children
            .iter()
            .map(|c| self.subtree_size[c.0 as usize])
            .sum::<u32>();
        self.nodes.push(kind);
        self.subtree_size.push(subtree_size);
        self.leaf_data_idx.push(u32::MAX);
        for &c in children {
            self.child_buf.push(c);
        }
        self.child_start.push(self.child_buf.len() as u32);
        id
    }
}

// ─── Pattern matching ────────────────────────────────────────────────────────

/// Kind of a node inside a pattern DAG.
///
/// `Wildcard` matches any target subtree and captures its root.
/// `Exact(k)` requires the target node's kind to equal `k`, and the pattern's
/// children (if any) are matched recursively against the target's children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternNodeKind<N> {
    /// Matches any subtree. Must be a leaf in the pattern DAG (no children).
    Wildcard,
    /// Matches a node whose kind equals the wrapped value.
    Exact(N),
}

impl<N: Node> Node for PatternNodeKind<N> {
    fn is_leaf(&self, ctx: &Context) -> bool {
        match self {
            PatternNodeKind::Wildcard => true,
            PatternNodeKind::Exact(n) => n.is_leaf(ctx),
        }
    }

    fn num_children(&self, ctx: &Context) -> usize {
        match self {
            PatternNodeKind::Wildcard => 0,
            PatternNodeKind::Exact(n) => n.num_children(ctx),
        }
    }
}

/// A pattern DAG: a [`PostOrderDag`] whose nodes are [`PatternNodeKind<N>`].
///
/// # Building a pattern
/// ```ignore
/// let mut pat: PatternDag<ExprKind> = PatternDag::new();
/// let a = pat.add_leaf(PatternNodeKind::Wildcard, ());
/// let b = pat.add_leaf(PatternNodeKind::Wildcard, ());
/// pat.add_inner(PatternNodeKind::Exact(ExprKind::Add), &[a, b]);
/// ```
pub type PatternDag<N, L = ()> = PostOrderDag<PatternNodeKind<N>, L>;

/// One successful match of a pattern against a region of the target DAG.
///
/// `mapping[i]` is the `NodeId` in the *target* that pattern node
/// `NodeId::from_index(i)` was matched to.
#[derive(Clone, Debug)]
pub struct Match {
    pub mapping: Vec<NodeId>,
}

impl Match {
    /// Returns the target node that the given pattern node was matched to.
    #[inline]
    pub fn target_of(&self, p: NodeId) -> NodeId {
        self.mapping[p.index()]
    }
}

/// All matches collected by [`PatternMatchDriver::run`].
#[derive(Default)]
pub struct MatchSet {
    /// `(pattern_index, match)` pairs, in discovery order.
    pub matches: Vec<(usize, Match)>,
}

impl MatchSet {
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }
}

/// Matches zero or more patterns against a target DAG using a VF2-style
/// recursive subgraph-isomorphism search adapted for ordered, rooted DAGs.
///
/// The pattern root is anchored at every target node in turn; each anchor
/// attempt is independent. All successful matches are collected so a
/// downstream cost function can choose the best covering.
///
/// # DAG patterns
/// Pattern nodes may be shared (appear as children of multiple parents),
/// mirroring expressions like `Add(Mul(a,b), Mul(a,c))` where `a` is
/// evaluated once. The matching algorithm enforces that such shared pattern
/// nodes map to the *same* target node (consistency check).
pub struct PatternMatchDriver<N: Node, L = ()> {
    patterns: Vec<PatternDag<N, L>>,
}

impl<N: Node + PartialEq, L> PatternMatchDriver<N, L> {
    pub fn new(patterns: Vec<PatternDag<N, L>>) -> Self {
        Self { patterns }
    }

    /// Match every pattern at every node of `target` and return all results.
    ///
    /// The same target subtree may appear in multiple entries of the returned
    /// [`MatchSet`] (different patterns, or different anchor positions).
    pub fn run<TL>(&self, target: &impl Dag<N, TL>) -> MatchSet {
        let mut set = MatchSet::default();

        for (pat_idx, pattern) in self.patterns.iter().enumerate() {
            let Some(pat_root) = pattern.root() else {
                continue;
            };

            for t_idx in 0..target.len() {
                let t_root = NodeId::from_index(t_idx);

                // Fresh VF2 state per (pattern, anchor) attempt so that a
                // failed attempt never pollutes the next one.
                let mut core_p = vec![UNMATCHED; pattern.len()];
                let mut core_t = vec![UNMATCHED; target.len()];

                if vf2_match(pattern, target, pat_root, t_root, &mut core_p, &mut core_t) {
                    let mapping = core_p
                        .iter()
                        .map(|&ti| NodeId::from_index(ti))
                        .collect();
                    set.matches.push((pat_idx, Match { mapping }));
                }
            }
        }

        set
    }
}

// ─── VF2 internals ───────────────────────────────────────────────────────────

const UNMATCHED: usize = usize::MAX;

/// Recursively attempt to match pattern node `p` to target node `t`.
///
/// ## Invariant
/// * **Success** (`true`): every node reachable from `p` in the pattern has
///   its entry set in `core_p`/`core_t`.
/// * **Failure** (`false`): `core_p`/`core_t` are restored to their state
///   *before* this call (full backtracking).
///
/// ## Ordered-DAG VF2
/// Because children are ordered, the candidate for pattern child `i` is
/// always target child `i` — so no candidate enumeration loop is needed.
/// The VF2 contribution is the consistency check for shared pattern nodes
/// and the clean backtracking discipline.
fn vf2_match<N, PL, TL>(
    pattern: &impl Dag<PatternNodeKind<N>, PL>,
    target: &impl Dag<N, TL>,
    p: NodeId,
    t: NodeId,
    core_p: &mut Vec<usize>,
    core_t: &mut Vec<usize>,
) -> bool
where
    N: Node + PartialEq,
{
    // ── Consistency checks (read-only) ───────────────────────────────────────
    // If `p` is already mapped, the only valid candidate is the same `t`.
    let cp = core_p[p.index()];
    if cp != UNMATCHED {
        return cp == t.index();
    }
    // If `t` is already claimed by a different pattern node, reject.
    let ct = core_t[t.index()];
    if ct != UNMATCHED {
        return ct == p.index();
    }

    // ── Feasibility ─────────────────────────────────────────────────────────
    let p_kind = pattern.get_kind(p);
    let is_wildcard = matches!(p_kind, PatternNodeKind::Wildcard);

    if !is_wildcard {
        let PatternNodeKind::Exact(required) = p_kind else {
            unreachable!()
        };
        if required != target.get_kind(t) {
            return false;
        }
        // Arity must match for ordered children to align.
        if pattern.children(p).len() != target.children(t).len() {
            return false;
        }
    }

    // ── Extend mapping ───────────────────────────────────────────────────────
    core_p[p.index()] = t.index();
    core_t[t.index()] = p.index();

    // Wildcards capture the subtree root without recursing into it.
    if is_wildcard {
        return true;
    }

    // ── Recurse into ordered children ───────────────────────────────────────
    let p_children: Vec<NodeId> = pattern.children(p).to_vec();
    let t_children: Vec<NodeId> = target.children(t).to_vec();

    for (i, (&pc, &tc)) in p_children.iter().zip(t_children.iter()).enumerate() {
        if !vf2_match(pattern, target, pc, tc, core_p, core_t) {
            // Undo the siblings that already succeeded (reverse order).
            for j in (0..i).rev() {
                vf2_undo(pattern, p_children[j], core_p, core_t);
            }
            // Undo our own pair and propagate failure.
            core_p[p.index()] = UNMATCHED;
            core_t[t.index()] = UNMATCHED;
            return false;
        }
    }

    true
}

/// Recursively clear the mapping for the subtree rooted at pattern node `p`.
///
/// No-ops if `p` is already unmapped, so it is safe to call on shared pattern
/// nodes that may have been unset by an earlier sibling's undo pass.
fn vf2_undo<N: Node, PL>(
    pattern: &impl Dag<PatternNodeKind<N>, PL>,
    p: NodeId,
    core_p: &mut Vec<usize>,
    core_t: &mut Vec<usize>,
) {
    let t_idx = core_p[p.index()];
    if t_idx == UNMATCHED {
        return; // already undone (e.g. shared node cleared by a sibling)
    }
    core_p[p.index()] = UNMATCHED;
    core_t[t_idx] = UNMATCHED;

    if !matches!(pattern.get_kind(p), PatternNodeKind::Wildcard) {
        for &pc in pattern.children(p) {
            vf2_undo(pattern, pc, core_p, core_t);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal node kind for tests.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum K {
        Add,
        Mul,
        Sub,
        Leaf,
    }

    impl Node for K {
        fn is_leaf(&self, _: &Context) -> bool {
            matches!(self, K::Leaf)
        }
        fn num_children(&self, _: &Context) -> usize {
            match self {
                K::Leaf => 0,
                _ => 2,
            }
        }
    }

    fn leaf(g: &mut PostOrderDag<K, ()>) -> NodeId {
        g.add_leaf(K::Leaf, ())
    }

    fn wc(p: &mut PatternDag<K>) -> NodeId {
        p.add_leaf(PatternNodeKind::Wildcard, ())
    }

    fn exact_leaf(p: &mut PatternDag<K>) -> NodeId {
        p.add_leaf(PatternNodeKind::Exact(K::Leaf), ())
    }

    // ── Basic matching ───────────────────────────────────────────────────────

    #[test]
    fn wildcard_matches_leaf() {
        // Pattern: *   Target: leaf
        let mut pat: PatternDag<K> = PatternDag::new();
        wc(&mut pat);

        let mut tgt = PostOrderDag::new();
        leaf(&mut tgt);

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);
        assert_eq!(ms.len(), 1);
    }

    #[test]
    fn exact_matches_same_kind() {
        // Pattern: Exact(Add)(*, *)   Target: Add(leaf, leaf)
        let mut pat: PatternDag<K> = PatternDag::new();
        let a = wc(&mut pat);
        let b = wc(&mut pat);
        pat.add_inner(PatternNodeKind::Exact(K::Add), &[a, b]);

        let mut tgt = PostOrderDag::new();
        let x = leaf(&mut tgt);
        let y = leaf(&mut tgt);
        tgt.add_inner(K::Add, &[x, y]);

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);
        // Should match: pattern root (Add) at target root (Add).
        // Wildcards also match the individual leaves.
        // pattern root: idx 2, target root: idx 2.
        let add_matches: Vec<_> = ms
            .matches
            .iter()
            .filter(|(_, m)| m.target_of(NodeId::from_index(2)) == NodeId::from_index(2))
            .collect();
        assert!(!add_matches.is_empty());
    }

    #[test]
    fn wrong_kind_does_not_match() {
        // Pattern: Exact(Mul)(*, *)   Target: Add(leaf, leaf)
        let mut pat: PatternDag<K> = PatternDag::new();
        let a = wc(&mut pat);
        let b = wc(&mut pat);
        pat.add_inner(PatternNodeKind::Exact(K::Mul), &[a, b]);

        let mut tgt = PostOrderDag::new();
        let x = leaf(&mut tgt);
        let y = leaf(&mut tgt);
        tgt.add_inner(K::Add, &[x, y]);

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);
        // Mul pattern should never match an Add target root.
        // Wildcards may still match the leaf nodes.
        let root_matches: Vec<_> = ms
            .matches
            .iter()
            .filter(|(_, m)| {
                let pat_root = NodeId::from_index(2); // Mul node in pattern
                let tgt_root = NodeId::from_index(2); // Add node in target
                m.target_of(pat_root) == tgt_root
            })
            .collect();
        assert!(root_matches.is_empty());
    }

    // ── Nested pattern ───────────────────────────────────────────────────────

    #[test]
    fn nested_pattern_matches() {
        // Pattern: Add(Mul(*, *), *)   Target: Add(Mul(l, l), l)
        let mut pat: PatternDag<K> = PatternDag::new();
        let wa = wc(&mut pat); // 0
        let wb = wc(&mut pat); // 1
        let mul = pat.add_inner(PatternNodeKind::Exact(K::Mul), &[wa, wb]); // 2
        let wc_ = wc(&mut pat); // 3
        pat.add_inner(PatternNodeKind::Exact(K::Add), &[mul, wc_]); // 4

        let mut tgt = PostOrderDag::new();
        let l0 = leaf(&mut tgt); // 0
        let l1 = leaf(&mut tgt); // 1
        let m = tgt.add_inner(K::Mul, &[l0, l1]); // 2
        let l2 = leaf(&mut tgt); // 3
        tgt.add_inner(K::Add, &[m, l2]); // 4

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);

        // The Add pattern must match Add target (root-to-root).
        let root_match = ms.matches.iter().find(|(_, m)| {
            m.target_of(NodeId::from_index(4)) == NodeId::from_index(4)
        });
        assert!(root_match.is_some());
        let (_, m) = root_match.unwrap();
        // Wildcards wa, wb captured the two leaves inside Mul.
        assert_eq!(m.target_of(NodeId::from_index(0)), NodeId::from_index(0));
        assert_eq!(m.target_of(NodeId::from_index(1)), NodeId::from_index(1));
        // wc_ captured the third leaf.
        assert_eq!(m.target_of(NodeId::from_index(3)), NodeId::from_index(3));
    }

    // ── Multiple patterns ────────────────────────────────────────────────────

    #[test]
    fn multiple_patterns_both_match() {
        // Target: Add(leaf, leaf)
        let mut tgt = PostOrderDag::new();
        let x = leaf(&mut tgt);
        let y = leaf(&mut tgt);
        tgt.add_inner(K::Add, &[x, y]);

        // Pattern 0: Add(*, *)
        let mut p0: PatternDag<K> = PatternDag::new();
        let a = wc(&mut p0);
        let b = wc(&mut p0);
        p0.add_inner(PatternNodeKind::Exact(K::Add), &[a, b]);

        // Pattern 1: wildcard (matches everything)
        let mut p1: PatternDag<K> = PatternDag::new();
        wc(&mut p1);

        let driver = PatternMatchDriver::new(vec![p0, p1]);
        let ms = driver.run(&tgt);

        let p0_count = ms.matches.iter().filter(|(i, _)| *i == 0).count();
        let p1_count = ms.matches.iter().filter(|(i, _)| *i == 1).count();
        assert!(p0_count >= 1, "pattern 0 should match");
        assert!(p1_count >= 1, "pattern 1 should match");
    }

    // ── DAG pattern (shared node) ────────────────────────────────────────────

    #[test]
    fn shared_pattern_node_consistent_match() {
        // Pattern: Add(*, *) where BOTH wildcards are the SAME node `a`.
        // This means the pattern requires both children to map to the same target.
        let mut pat: PatternDag<K> = PatternDag::new();
        let a = wc(&mut pat); // 0 — shared
        pat.add_inner(PatternNodeKind::Exact(K::Add), &[a, a]); // 1

        // Target A: Add(leaf0, leaf0) — same child on both sides
        let mut tgt_same = PostOrderDag::new();
        let l0 = leaf(&mut tgt_same); // 0
        tgt_same.add_inner(K::Add, &[l0, l0]); // 1

        // Target B: Add(leaf0, leaf1) — different children
        let mut tgt_diff = PostOrderDag::new();
        let l0 = leaf(&mut tgt_diff); // 0
        let l1 = leaf(&mut tgt_diff); // 1
        tgt_diff.add_inner(K::Add, &[l0, l1]); // 2

        let driver = PatternMatchDriver::new(vec![pat]);

        let ms_same = driver.run(&tgt_same);
        let ms_diff = driver.run(&tgt_diff);

        // Same child: the shared wildcard can consistently map to leaf0.
        let root_match_same = ms_same.matches.iter().any(|(_, m)| {
            m.target_of(NodeId::from_index(1)) == NodeId::from_index(1)
        });
        assert!(root_match_same, "shared wildcard should match when children are identical");

        // Different children: shared wildcard cannot map to two different nodes.
        let root_match_diff = ms_diff.matches.iter().any(|(_, m)| {
            m.target_of(NodeId::from_index(1)) == NodeId::from_index(2)
        });
        assert!(!root_match_diff, "shared wildcard must not match when children differ");
    }

    // ── All matches collected ────────────────────────────────────────────────

    #[test]
    fn wildcard_matches_every_node() {
        // A single wildcard pattern should match every node in the target.
        let mut pat: PatternDag<K> = PatternDag::new();
        wc(&mut pat);

        let mut tgt = PostOrderDag::new();
        let x = leaf(&mut tgt);
        let y = leaf(&mut tgt);
        tgt.add_inner(K::Add, &[x, y]);

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);
        // 3 nodes in target → 3 matches
        assert_eq!(ms.len(), 3);
    }

    #[test]
    fn exact_leaf_matches_only_leaves() {
        // Pattern: Exact(Leaf)  — should match only leaf nodes, not Add.
        let mut pat: PatternDag<K> = PatternDag::new();
        exact_leaf(&mut pat);

        let mut tgt = PostOrderDag::new();
        let x = leaf(&mut tgt);
        let y = leaf(&mut tgt);
        tgt.add_inner(K::Add, &[x, y]);

        let driver = PatternMatchDriver::new(vec![pat]);
        let ms = driver.run(&tgt);
        // 2 leaf nodes, 1 Add — only leaves match Exact(Leaf)
        assert_eq!(ms.len(), 2);
        for (_, m) in &ms.matches {
            let matched = m.target_of(NodeId::from_index(0));
            assert!(matched == x || matched == y, "must match a leaf");
        }
    }
}
