use std::collections::HashMap;

use crate::Context;

mod pattern;
mod postorder;

pub use pattern::{
    CoverCandidate, GraphCoverDriver, MatchBinding, Pattern, PatternExpr, PatternId,
    VF2CoverDriver,
};
pub use postorder::PostOrderDag;

pub(crate) static EMPTY_CHILDREN: [NodeId; 0] = [];

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

pub trait Dag {
    type Node: Node;
    type Leaf;

    fn len(&self) -> usize;

    fn get_node(&self, id: NodeId) -> &Self::Node;
    fn get_kind(&self, id: NodeId) -> &Self::Node {
        self.get_node(id)
    }
    fn get_leaf_data(&self, id: NodeId) -> Option<&Self::Leaf>;

    fn root(&self) -> Option<NodeId>;
    fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId>;

    fn postorder(&self, start: NodeId) -> impl Iterator<Item = NodeId>;
    fn preorder(&self, start: NodeId) -> impl Iterator<Item = NodeId>;
}

pub trait MutDag: Dag {
    fn add_node(&mut self, n: Self::Node) -> NodeId;
    fn add_edge(&mut self, from: NodeId, to: NodeId);
    fn set_leaf_data(&mut self, n: NodeId, d: Self::Leaf);
}

pub struct GenericDag<N: Node, L> {
    nodes: Vec<N>,
    edges: HashMap<NodeId, Vec<NodeId>>,
    data: HashMap<NodeId, L>,
}

impl<N: Node, L> GenericDag<N, L> {
    fn contains_descendant(&self, root: NodeId, target: NodeId) -> bool {
        if root == target {
            return true;
        }

        self.edges.get(&root).is_some_and(|children| {
            children
                .iter()
                .any(|&child| self.contains_descendant(child, target))
        })
    }

    fn nth_preorder(&self, node: NodeId, remaining: &mut usize) -> Option<NodeId> {
        if *remaining == 0 {
            return Some(node);
        }
        *remaining -= 1;

        self.edges.get(&node).and_then(|children| {
            for &child in children {
                if let Some(found) = self.nth_preorder(child, remaining) {
                    return Some(found);
                }
            }
            None
        })
    }
}

pub struct GenericDagPostorderIter<'a, N: Node, L> {
    dag: &'a GenericDag<N, L>,
    start: NodeId,
    next_index: usize,
}

impl<N: Node, L> Iterator for GenericDagPostorderIter<'_, N, L> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_index <= self.start.index() {
            let candidate = NodeId::from_index(self.next_index);
            self.next_index += 1;

            if self.dag.contains_descendant(self.start, candidate) {
                return Some(candidate);
            }
        }

        None
    }
}

pub struct GenericDagPreorderIter<'a, N: Node, L> {
    dag: &'a GenericDag<N, L>,
    start: NodeId,
    next_ordinal: usize,
}

impl<N: Node, L> Iterator for GenericDagPreorderIter<'_, N, L> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let mut remaining = self.next_ordinal;
        let next = self.dag.nth_preorder(self.start, &mut remaining)?;
        self.next_ordinal += 1;
        Some(next)
    }
}

impl<N: Node, L> Dag for GenericDag<N, L> {
    type Node = N;

    type Leaf = L;

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn get_node(&self, id: NodeId) -> &Self::Node {
        &self.nodes[id.index()]
    }

    fn get_leaf_data(&self, id: NodeId) -> Option<&Self::Leaf> {
        self.data.get(&id)
    }

    fn root(&self) -> Option<NodeId> {
        self.nodes.len().checked_sub(1).map(NodeId::from_index)
    }

    fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.edges
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&EMPTY_CHILDREN)
            .iter()
            .copied()
    }

    fn postorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        GenericDagPostorderIter {
            dag: self,
            start,
            next_index: 0,
        }
    }

    fn preorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        GenericDagPreorderIter {
            dag: self,
            start,
            next_ordinal: 0,
        }
    }
}

impl<N: Node, L> MutDag for GenericDag<N, L> {
    fn add_node(&mut self, n: Self::Node) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(n);
        id
    }

    fn add_edge(&mut self, from: NodeId, to: NodeId) {
        self.edges.entry(from).or_default().push(to);
    }

    fn set_leaf_data(&mut self, n: NodeId, d: Self::Leaf) {
        self.data.insert(n, d);
    }
}
