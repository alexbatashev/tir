use std::sync::Arc;

use parking_lot::RwLock;

use crate::{BlockId, Context, ContextIterator, GetFromContext, OpId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(u32);

#[derive(Debug)]
pub struct Region {
    id: RegionId,
    blocks: RwLock<Vec<BlockId>>,
    parent_op: RwLock<OpId>,
}

impl Region {
    pub fn id(&self) -> RegionId {
        self.id
    }

    pub(crate) fn new(id: RegionId) -> Region {
        Region {
            id,
            blocks: RwLock::new(vec![]),
            parent_op: RwLock::new(OpId::invalid()),
        }
    }

    pub(crate) fn set_parent_op(&self, op: OpId) {
        *self.parent_op.write() = op;
    }

    pub fn add_block(&self, id: BlockId) {
        self.blocks.write().push(id);
    }

    pub fn iter(&self, context: Context) -> ContextIterator<BlockId> {
        ContextIterator::new(context, self.blocks.read().clone())
    }
}

impl RegionId {
    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }
}

impl GetFromContext for RegionId {
    type Item = Arc<Region>;

    fn get_from_context(&self, context: &Context) -> Self::Item {
        context.get_region(*self)
    }
}
