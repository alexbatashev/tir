use std::sync::Arc;

use crate::{Block, Operation};

pub struct IRBuilder {
    insertion_point: InsertionPoint,
}

pub struct InsertionPoint {
    block: Arc<Block>,
    position: usize,
}

impl IRBuilder {
    /// Create a new IRBuilder that inserts to the end of block
    pub fn new(block: Arc<Block>) -> IRBuilder {
        let position = block.len();

        let insertion_point = InsertionPoint { block, position };

        IRBuilder { insertion_point }
    }

    pub fn insert<T: Operation>(&mut self, op: T) -> T {
        let id = op.id();
        self.insertion_point
            .block
            .insert(self.insertion_point.position, id);
        self.insertion_point.position += 1;
        op
    }
}
