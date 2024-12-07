use std::{cell::RefCell, sync::Arc};

use smallvec::{smallvec, SmallVec};

use crate::{
    green::{NodeId, NodeKind},
    ContextRef, ContextWRef, OpRef, Type,
};

#[derive(Clone)]
#[repr(transparent)]
pub struct Block(RefCell<BlockInner>);

#[derive(Clone)]
struct BlockData {
    name: Arc<String>,
    arguments: SmallVec<[Type; 4]>,
}

#[derive(Clone)]
enum BlockInner {
    Imm(ContextWRef, NodeId),
    Mut(ContextWRef, BlockData, Vec<OpRef>),
}

impl Block {
    /// Creates an empty mutable basic block
    pub fn new(context: &ContextRef) -> Self {
        let context = Arc::downgrade(context);

        Self(RefCell::new(BlockInner::Mut(
            context,
            BlockData {
                name: Arc::new("".to_owned()),
                arguments: smallvec![],
            },
            vec![],
        )))
    }

    /// Casts a green Node into a basic block
    pub fn from_node_id(context: &ContextRef, id: NodeId) -> Self {
        let node = context.get_node(id);
        assert!(node.is_some());
        assert_eq!(node.unwrap().kind(), NodeKind::Block);

        let context = Arc::downgrade(context);

        Self(RefCell::new(BlockInner::Imm(context, id)))
    }

    /// Appends operation to the end of basic block
    pub fn push(&mut self, op: &OpRef) {
        self.ensure_mutable();
        self.0.borrow_mut().push(op);
    }

    /// Insert operation at specific index in basic block
    pub fn insert(&mut self, index: usize, op: &OpRef) {
        self.ensure_mutable();
        self.0.borrow_mut().insert(index, op);
    }

    fn ensure_mutable(&self) {
        if let BlockInner::Imm(ctx, id) = &*self.0.borrow() {
            let ctx = ctx.upgrade().unwrap();
            let id = *id;

            let node = ctx.get_node(id).unwrap();
            let children = node
                .children()
                .iter()
                .map(|id| {
                    let node = ctx.get_node(*id).unwrap();
                    todo!()
                })
                .collect();

            self.0.replace(BlockInner::Mut(
                Arc::downgrade(&ctx),
                BlockData {
                    name: Arc::new("".to_owned()),
                    arguments: smallvec![],
                },
                children,
            ));
        }
    }
}

impl BlockInner {
    fn push(&mut self, op: &OpRef) {
        match self {
            Self::Mut(_, _, ref mut ops) => ops.push(op.clone()),
            Self::Imm(_, _) => unreachable!(),
        }
    }

    fn insert(&mut self, index: usize, op: &OpRef) {
        match self {
            Self::Mut(_, _, ref mut ops) => ops.insert(index, op.clone()),
            Self::Imm(_, _) => unreachable!(),
        }
    }
}
