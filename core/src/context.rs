use std::{
    any::{Any, TypeId},
    collections::HashMap,
    sync::{Arc, Weak, atomic::AtomicU32},
};

use parking_lot::RwLock;

use crate::{
    Block, Dialect, Error, OpId, OpInstance, Operation, Region, Type,
    block::BlockId,
    builtin::BuiltinDialect,
    operation::{
        ImplementsOpInterface, OpInterfaceConverter, downcast_op_interface, op_interface_converter,
    },
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
    op_interface_converters: HashMap<(&'static str, &'static str, TypeId), OpInterfaceConverter>,
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
            op_interface_converters: HashMap::new(),
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

        // Results are created before op id assignment in builders; patch their def-site now.
        for result_id in &instance.results {
            if let Some(value) = inner.values.get(result_id).cloned() {
                inner.values.insert(
                    *result_id,
                    Arc::new((*value).clone().with_defining_op(op_id)),
                );
            }
        }

        for r in &instance.regions {
            inner.regions.get(&r).unwrap().set_parent_op(op_id);
        }

        let instance = Arc::new(instance);

        inner.operations.insert(op_id, instance.clone());

        instance
    }

    pub fn has_operation(&self, id: OpId) -> bool {
        self.0.read().operations.contains_key(&id)
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

    pub fn has_value(&self, id: ValueId) -> bool {
        self.0.read().values.contains_key(&id)
    }

    pub fn is_block_argument(&self, id: ValueId) -> bool {
        let inner = self.0.read();
        inner
            .blocks
            .values()
            .any(|block| block.arguments().iter().any(|arg| arg.id() == id))
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

    pub fn register_op_interface<I: ?Sized + 'static>(
        &self,
        dialect: &'static str,
        op_name: &'static str,
        converter: OpInterfaceConverter,
    ) {
        self.0
            .write()
            .op_interface_converters
            .insert((dialect, op_name, TypeId::of::<I>()), converter);
    }

    pub fn register_operation_interface<Op, I>(&self)
    where
        Op: ImplementsOpInterface<I>,
        I: ?Sized + 'static,
    {
        self.register_op_interface::<I>(Op::dialect(), Op::name(), op_interface_converter::<Op, I>);
    }

    pub(crate) fn get_dyn_op(&self, op: Arc<OpInstance>) -> Box<dyn Operation> {
        let inner = self.0.read();

        let dialect = inner.dialects.get(op.dialect()).unwrap();

        dialect.get_dyn_op(op)
    }

    pub(crate) fn get_op_interface<I: ?Sized + 'static>(
        &self,
        op: Arc<OpInstance>,
    ) -> Option<Box<I>> {
        let converter = {
            let inner = self.0.read();
            inner
                .op_interface_converters
                .get(&(op.dialect(), op.name(), TypeId::of::<I>()))
                .copied()
        }?;

        let erased = converter(op);
        downcast_op_interface::<I>(erased)
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
    use crate::{Commutative, Operation, Terminator, Type, builtin};

    #[test]
    fn default_context() {
        let _ = Context::with_default_dialects();
    }

    #[test]
    fn custom_interface_for_existing_op() {
        let context = Context::with_default_dialects();

        let lhs = context.create_value(Type::Integer { width: 32 }, None);
        let rhs = context.create_value(Type::Integer { width: 32 }, None);
        let add =
            builtin::ops::addi(&context, lhs.id(), rhs.id(), Type::Integer { width: 32 }).build();

        let iface = context
            .get_op(add.id())
            .as_interface::<dyn Commutative>()
            .expect("interface should be available");
        assert!(iface.is_commutative());
    }

    #[test]
    fn builtin_terminator_interface() {
        let context = Context::with_default_dialects();
        let value = context.create_value(Type::Integer { width: 32 }, None);
        let ret = builtin::ops::r#return(&context, value.id()).build();

        let iface = context
            .get_op(ret.id())
            .as_interface::<dyn Terminator>()
            .expect("terminator interface should be available");
        assert!(iface.is_terminator());
    }
}
