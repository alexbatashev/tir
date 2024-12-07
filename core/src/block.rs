use std::sync::Arc;

use crate::{
    green::{NodeId, NodeKind},
    ContextRef, ContextWRef, OpRef,
};

#[derive(Clone)]
#[repr(transparent)]
pub struct Block(BlockInner);

#[derive(Clone)]
enum BlockInner {
    Imm(ContextWRef, NodeId),
    Mut(ContextWRef, Vec<OpRef>),
}

impl Block {
    pub fn new(context: &ContextRef) -> Self {
        let context = Arc::downgrade(context);

        Self(BlockInner::Mut(context, vec![]))
    }

    pub fn from_node_id(context: &ContextRef, id: NodeId) -> Self {
        let node = context.get_node(id);
        assert!(node.is_some());
        assert_eq!(node.unwrap().kind(), NodeKind::Block);

        let context = Arc::downgrade(context);

        Self(BlockInner::Imm(context, id))
    }
}
