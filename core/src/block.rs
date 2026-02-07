use std::sync::Arc;

use parking_lot::RwLock;

use crate::{Context, ContextIterator, GetFromContext, OpId, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u32);

#[derive(Debug)]
pub struct Block {
    id: BlockId,
    arguments: Vec<Value>,
    operations: RwLock<Vec<OpId>>,
    successors: RwLock<Vec<BlockId>>,
    predecessors: RwLock<Vec<BlockId>>,
}

impl BlockId {
    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }
}

impl Block {
    pub(crate) fn new(id: BlockId, arguments: Vec<Value>) -> Self {
        Self {
            id,
            arguments,
            operations: RwLock::new(vec![]),
            successors: RwLock::new(vec![]),
            predecessors: RwLock::new(vec![]),
        }
    }

    pub fn id(&self) -> BlockId {
        self.id
    }

    pub fn arguments(&self) -> &[Value] {
        &self.arguments
    }

    pub fn len(&self) -> usize {
        self.operations.read().len()
    }

    pub(crate) fn insert(&self, index: usize, id: OpId) {
        self.operations.write().insert(index, id);
    }

    pub fn iter(&self, context: Context) -> ContextIterator<OpId> {
        ContextIterator::new(context, self.operations.read().clone())
    }
}

impl GetFromContext for BlockId {
    type Item = Arc<Block>;

    fn get_from_context(&self, context: &crate::Context) -> Self::Item {
        context.get_block(*self)
    }
}
