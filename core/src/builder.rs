use std::sync::Arc;

use crate::{Block, Operation};

pub struct IRBuilder {
    insertion_point: InsertionPoint,
}

pub struct InsertionPoint {
    block: Arc<Block>,
    /// Where the next operation lands, or `None` to append. Appending asks the
    /// live block for its end, so a builder over a block that other code also
    /// edits keeps writing after everything already there.
    position: Option<usize>,
}

impl IRBuilder {
    /// Create a new IRBuilder that inserts to the end of block
    pub fn new(block: Arc<Block>) -> IRBuilder {
        IRBuilder {
            insertion_point: InsertionPoint {
                block,
                position: None,
            },
        }
    }

    pub fn insert<T: Operation>(&mut self, op: T) -> T {
        let id = op.id();
        match &mut self.insertion_point.position {
            Some(position) => {
                self.insertion_point.block.insert(*position, id);
                *position += 1;
            }
            None => self.insertion_point.block.append(id),
        }
        op
    }

    pub fn set_insertion_point_to_start(&mut self, block: Arc<Block>) {
        self.insertion_point = InsertionPoint {
            block,
            position: Some(0),
        };
    }
}
