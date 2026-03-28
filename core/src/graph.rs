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
