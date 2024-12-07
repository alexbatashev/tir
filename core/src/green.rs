use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NodeId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum NodeKind {
    Region,
    Block,
    Operation,
}

pub type NodeCaster = fn(*mut ()) -> ();

/// A green node is a type-erased immutable storage that holds the internal data of each operation
#[derive(Debug, Clone)]
pub struct Node {
    id: NodeId,
    kind: NodeKind,
    // Data and caster can only be set for NodeKind::Operation
    data: Option<*mut ()>,
    caster: Option<NodeCaster>,
    // Used to hold children IDs for regions and blocks
    children: SmallVec<[NodeId; 8]>,
}

impl NodeId {
    fn invalid() -> Self {
        NodeId(u32::MAX)
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::invalid()
    }
}

impl Node {
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    pub fn children(&self) -> &[NodeId] {
        &self.children
    }
}
