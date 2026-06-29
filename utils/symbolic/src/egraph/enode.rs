use std::fmt::Debug;

use tir_adt::{APFloat, APInt};

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

    /// Bucket key for the operator index that pattern search uses to skip classes a
    /// concrete-rooted pattern cannot match ([`EGraph::classes_with_op`]).
    ///
    /// Contract: if `a.matches(b)` then `a.op_key() == b.op_key()` — *including* when
    /// `a` is a pattern template that matches loosely (e.g. a wildcard result type).
    /// So the key must depend only on the fields `matches` compares for strict
    /// equality, never on a field a template can leave wildcarded. The default is
    /// [`hash_cons`](ENode::hash_cons), correct whenever `matches` implies equal
    /// `hash_cons` (no wildcard fields); override it when a template matches more
    /// nodes than its own `hash_cons` bucket holds.
    fn op_key(&self) -> u64 {
        self.hash_cons()
    }

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

    /// The canonical node for an integer constant, if the language has one.
    /// Backs [`Var::Int`](super::Var::Int) pattern leaves for both matching and
    /// instantiation; matching reuses [`matches`](ENode::matches), so this must
    /// produce the same node the language interns for that constant.
    fn from_int(_value: APInt) -> Option<Self> {
        None
    }

    /// The canonical node for a float constant, if the language has one. Backs
    /// [`Var::Float`](super::Var::Float) pattern leaves; see [`from_int`](ENode::from_int).
    fn from_float(_value: APFloat) -> Option<Self> {
        None
    }
}
