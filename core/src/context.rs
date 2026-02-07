use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, Weak, atomic::AtomicU32},
};

use parking_lot::RwLock;

use crate::{
    Block, Dialect, Error, OpId, OpInstance, Operation, Region, Type,
    block::BlockId,
    builtin::BuiltinDialect,
    parse::Span,
    parse::text::Parser as IRParser,
    region::RegionId,
    value::{Value, ValueId},
};

/// Central hub for managing all IR entities and state.
///
/// The `Context` serves as the global owner and access point for all
/// intermediate representation (IR) objects such as operations, values,
/// regions, and blocks. It orchestrates allocation, registration, lookup,
/// and mutation of these entities, providing a reliable foundation for
/// all transformation passes and analyses.
///
/// All IR objects in TIR are uniquely identified and stored within the
/// context, which enables:
/// - **Uniqueness and lifetime management:** Ensures that all IR nodes are
///   consistently referenced by identifier and have stable lifetimes throughout
///   graph construction and rewriting.
/// - **Thread safety:** Allows safe concurrent access to the IR graph, supporting
///   lock-free reads and coordinated mutation via interior mutability primitives.
/// - **Dialect and operation extensibility:** Registers and manages dialects and
///   operation kinds, enabling the IR to be extended with new languages or
///   target-specific features.
/// - **Forking and analysis:** Supports speculative graph forking, cloning, or
///   cost-based variant analysis by encapsulating IR state in a single location.
///
/// The `Context` enforces the design principle that individual IR objects
/// (like operations or blocks) do not exist in isolation; instead, they
/// are always part of a coherent context-managed graph.
///
/// # Example
///
/// ```rust
/// let context = tir::Context::with_default_dialects();
/// ```
///
/// The context is typically shared (via reference or smart pointer) throughout
/// the compiler pipeline, ensuring consistent access to all ongoing IR state
/// and registered dialects.
#[derive(Clone)]
pub struct Context(Arc<RwLock<ContextInstance>>);

#[derive(Debug, Clone)]
pub struct ContextRef(Weak<RwLock<ContextInstance>>);

pub struct ContextIterator<I: GetFromContext> {
    context: Context,
    elements: Vec<I>,
    current_front: usize,
    current_back: usize,
}

pub trait GetFromContext {
    type Item;

    fn get_from_context(&self, context: &Context) -> Self::Item;
}

struct ContextInstance {
    // None for root context itself, reference to a root context if this is a forked Region.
    root_context: Option<Context>,
    operations: HashMap<OpId, Arc<OpInstance>>,
    last_op_id: AtomicU32,
    values: HashMap<ValueId, Arc<Value>>,
    last_value_id: AtomicU32,
    regions: HashMap<RegionId, Arc<Region>>,
    last_region_id: AtomicU32,
    blocks: HashMap<BlockId, Arc<Block>>,
    last_block_id: AtomicU32,
    dialects: HashMap<&'static str, Arc<dyn Dialect>>,
}

impl Context {
    /// Create a new empty context with no registered dialects.
    pub fn new() -> Self {
        Context(Arc::new(RwLock::new(ContextInstance {
            root_context: None,
            operations: HashMap::new(),
            last_op_id: AtomicU32::new(0),
            values: HashMap::new(),
            last_value_id: AtomicU32::new(0),
            regions: HashMap::new(),
            last_region_id: AtomicU32::new(0),
            blocks: HashMap::new(),
            last_block_id: AtomicU32::new(0),
            dialects: HashMap::new(),
        })))
    }

    /// Create a new context with default dialects.
    pub fn with_default_dialects() -> Self {
        let context = Context::new();

        context.register_dialect::<BuiltinDialect>();

        context
    }

    pub fn as_context_ref(&self) -> ContextRef {
        ContextRef(Arc::downgrade(&self.0))
    }

    /// Register a dialect with context.
    pub fn register_dialect<D: Dialect>(&self) {
        let mut dialect = D::new();
        Arc::<dyn Dialect>::get_mut(&mut dialect)
            .unwrap()
            .register_operations(self);
        self.0.write().dialects.insert(D::name(), dialect);
    }

    pub fn find_dialect<D: Dialect>(&self) -> Option<Arc<D>> {
        self.0
            .read()
            .dialects
            .get(D::name())
            .cloned()
            .map(|d| {
                let d: Arc<dyn Any + Send + Sync> = d;
                d.downcast::<D>().ok()
            })
            .flatten()
    }

    pub fn add_operation(&self, mut instance: OpInstance) -> Arc<OpInstance> {
        let mut inner = self.0.write();

        let op_id = OpId::new(
            inner
                .last_op_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        instance.id = op_id;

        for r in &instance.regions {
            inner.regions.get(&r).unwrap().set_parent_op(op_id);
        }

        let instance = Arc::new(instance);

        inner.operations.insert(op_id, instance.clone());

        instance
    }

    pub fn create_value(&self, ty: Type, defining_op: Option<OpId>) -> Value {
        let mut inner = self.0.write();

        let value_id = ValueId::new(
            inner
                .last_value_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        let value = Value::new(value_id, ty, defining_op);
        inner.values.insert(value_id, Arc::new(value.clone()));

        value
    }

    pub fn get_value(&self, id: ValueId) -> Arc<Value> {
        let inner = self.0.read();
        inner.values.get(&id).unwrap().clone()
    }

    pub fn create_region(&self) -> Arc<Region> {
        let mut inner = self.0.write();

        let region_id = RegionId::new(
            inner
                .last_region_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        let region = Arc::new(Region::new(region_id));
        inner.regions.insert(region_id, region.clone());

        region
    }

    pub fn create_block(&self, arguments: Vec<Value>) -> Arc<Block> {
        let mut inner = self.0.write();

        let block_id = BlockId::new(
            inner
                .last_block_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        let block = Arc::new(Block::new(block_id, arguments));
        inner.blocks.insert(block_id, block.clone());

        block
    }

    pub fn get_block(&self, id: BlockId) -> Arc<Block> {
        let inner = self.0.read();

        inner.blocks.get(&id).unwrap().clone()
    }

    pub fn get_region(&self, id: RegionId) -> Arc<Region> {
        let inner = self.0.read();

        inner.regions.get(&id).unwrap().clone()
    }

    pub fn get_op(&self, id: OpId) -> Arc<OpInstance> {
        let inner = self.0.read();

        inner.operations.get(&id).unwrap().clone()
    }

    pub(crate) fn get_dyn_op(&self, op: Arc<OpInstance>) -> Box<dyn Operation> {
        let inner = self.0.read();

        let dialect = inner.dialects.get(op.dialect()).unwrap();

        dialect.get_dyn_op(op)
    }

    pub fn get_parser(
        &self,
        dialect: &str,
        name: &str,
    ) -> Result<fn(&mut IRParser, &Context) -> Result<Box<dyn Operation>, (Span, Error)>, Error>
    {
        let inner = self.0.read();

        let dialect = inner
            .dialects
            .get(dialect)
            .ok_or(Error::UnknownDialect(dialect.to_string()))?;

        dialect.get_parser(name)
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::with_default_dialects()
    }
}

impl ContextRef {
    pub fn upgrade(&self) -> Context {
        Context(self.0.upgrade().unwrap())
    }
}

impl<I: GetFromContext> ContextIterator<I> {
    pub fn new(context: Context, elements: Vec<I>) -> Self {
        let current_back = elements.len();
        Self {
            context,
            elements,
            current_front: 0,
            current_back,
        }
    }
}

impl<I: GetFromContext> Iterator for ContextIterator<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_front == self.elements.len() {
            None
        } else {
            let element = self.elements[self.current_front].get_from_context(&self.context);
            self.current_front += 1;
            Some(element)
        }
    }
}

impl<I: GetFromContext> ExactSizeIterator for ContextIterator<I> {
    fn len(&self) -> usize {
        self.elements.len()
    }
}

impl<I: GetFromContext> DoubleEndedIterator for ContextIterator<I> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.current_back == 0 {
            None
        } else {
            self.current_back -= 1;
            let element = self.elements[self.current_back].get_from_context(&self.context);
            Some(element)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Context;

    #[test]
    fn default_context() {
        let _ = Context::with_default_dialects();
    }
}
