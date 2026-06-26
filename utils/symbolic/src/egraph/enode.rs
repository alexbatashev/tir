use std::fmt::Debug;

use super::Id;

/// An e-graph node. Operands are child e-class [`Id`]s carried inline; the e-graph
/// canonicalizes them through [`children_mut`](ENode::children_mut).
///
/// Hash-consing is an e-graph operation, decoupled from any `Hash`/`Eq` the node
/// type may implement for its own use: [`hash_cons`](ENode::hash_cons) only buckets,
/// and node identity is decided by [`matches`](ENode::matches) plus equal canonical
/// children — so a hash collision is harmless and never merges distinct nodes.
pub trait ENode: Debug + Clone {
    fn children(&self) -> &[Id];
    fn children_mut(&mut self) -> &mut [Id];

    /// Bucket hash for hash-consing. Not required collision-free.
    fn hash_cons(&self) -> u64;

    /// Operator/label equality, ignoring children. Two nodes share an e-class iff
    /// `matches` holds and their canonical children are equal.
    fn matches(&self, other: &Self) -> bool;

    /// Whether copies of this node may be shared. A unique node gets a fresh
    /// e-class on every `add` and never hash-conses or congruence-merges (for
    /// effectful ops or genuinely distinct unknowns). Its operand ids still
    /// resolve through the e-graph's `find`, like any other node's.
    fn is_unique(&self) -> bool {
        false
    }
}
