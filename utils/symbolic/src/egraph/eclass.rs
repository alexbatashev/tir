use super::{ENode, Id};

/// An equivalence class: the e-nodes proven equal, plus back-edges to the e-nodes
/// that reference this class as an operand (used to repair congruence after unions).
pub struct EClass<L: ENode> {
    pub(super) id: Id,
    pub(super) nodes: Vec<L>,
    /// `(parent enode, the parent's own class)` for every e-node with a child in
    /// this class.
    pub(super) parents: Vec<(L, Id)>,
}

impl<L: ENode> EClass<L> {
    pub(super) fn new(id: Id, node: L) -> Self {
        Self {
            id,
            nodes: vec![node],
            parents: Vec::new(),
        }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn nodes(&self) -> &[L] {
        &self.nodes
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
