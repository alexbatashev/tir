use std::sync::Arc;

use crate::{
    Context, ContextIterator, GetFromContext, OpId, Value,
    attributes::{AttributeValue, NamedAttribute},
    context::ContextRef,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u32);

/// A basic block: a plain value living in the context's block slab, edited in
/// place through [`Context`] under its write lock.
///
/// Like [`crate::OpInstance`], a handle handed out by [`Context::get_block`] is a
/// snapshot: reads answer from the copy that was current when the handle was
/// taken, while the mutators below always edit the live block. Re-read the block
/// from the context to observe an edit.
#[derive(Debug, Clone)]
pub struct Block {
    id: BlockId,
    arguments: Vec<Value>,
    operations: Vec<OpId>,
    /// Discardable metadata scoped to this block (e.g. `fpmath`), printed in the
    /// block label.
    attributes: Vec<NamedAttribute>,
    /// Handle back to the owning context, which stores this block and performs
    /// every edit below.
    context: ContextRef,
}

impl BlockId {
    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn number(&self) -> u32 {
        self.0
    }

    pub fn from_number(n: u32) -> Self {
        Self(n)
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

impl Block {
    pub(crate) fn new(id: BlockId, arguments: Vec<Value>, context: ContextRef) -> Self {
        Self {
            id,
            arguments,
            operations: vec![],
            attributes: vec![],
            context,
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.arguments.capacity() * std::mem::size_of::<Value>()
            + self.operations.capacity() * std::mem::size_of::<OpId>()
            + self.attributes.capacity() * std::mem::size_of::<NamedAttribute>()
    }

    pub(crate) fn operations_mut(&mut self) -> &mut Vec<OpId> {
        &mut self.operations
    }

    pub(crate) fn arguments_mut(&mut self) -> &mut Vec<Value> {
        &mut self.arguments
    }

    pub(crate) fn attributes_mut(&mut self) -> &mut Vec<NamedAttribute> {
        &mut self.attributes
    }

    pub fn attributes(&self) -> &[NamedAttribute] {
        &self.attributes
    }

    pub fn attr(&self, name: &str) -> Option<AttributeValue> {
        let name = self.context.upgrade().sym(name)?;
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.clone())
    }

    /// Set (or replace) a named attribute on this block.
    pub fn set_attr(&self, name: &str, value: AttributeValue) {
        self.context.upgrade().set_block_attr(self.id, name, value);
    }

    pub fn id(&self) -> BlockId {
        self.id
    }

    pub fn arguments(&self) -> &[Value] {
        &self.arguments
    }

    pub fn len(&self) -> usize {
        self.operations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    pub fn insert(&self, index: usize, id: OpId) {
        self.context.upgrade().insert_op(self.id, index, id);
    }

    /// Add `id` after every operation the block currently holds. Unlike
    /// [`Block::insert`] this asks the live block for its end, so it is correct to
    /// call in a loop through one handle.
    pub fn append(&self, id: OpId) {
        self.context.upgrade().append_op(self.id, id);
    }

    /// Append `op` and hand it back, for building a block in sequence.
    pub fn append_op<T: crate::Operation>(&self, op: T) -> T {
        self.append(op.id());
        op
    }

    pub fn op_ids(&self) -> Vec<OpId> {
        self.operations.clone()
    }

    pub fn replace_op(&self, old: OpId, new: OpId) -> bool {
        self.context
            .upgrade()
            .replace_op_in_block(self.id, old, new)
    }

    pub fn remove_op(&self, id: OpId) -> bool {
        self.context.upgrade().remove_op_from_block(self.id, id)
    }

    /// Returns true if a comes before b in the block, false otherwise
    pub fn is_before(&self, a: OpId, b: OpId) -> bool {
        let a_pos = self.operations.iter().position(|op_id| *op_id == a);
        let b_pos = self.operations.iter().position(|op_id| *op_id == b);

        if let (Some(a_pos), Some(b_pos)) = (a_pos, b_pos) {
            a_pos < b_pos
        } else {
            false
        }
    }

    pub fn iter(&self, context: Context) -> ContextIterator<OpId> {
        ContextIterator::new(context, self.operations.clone())
    }
}

impl GetFromContext for BlockId {
    type Item = Arc<Block>;

    fn get_from_context(&self, context: &crate::Context) -> Self::Item {
        context.get_block(*self)
    }
}
