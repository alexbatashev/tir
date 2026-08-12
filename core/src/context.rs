use std::{
    any::Any,
    collections::HashMap,
    hash::{DefaultHasher, Hasher},
    sync::{Arc, Weak, atomic::AtomicU32},
};

use parking_lot::RwLock;

use crate::{
    Block, Dialect, Error, OpId, OpInstance, Operation, OperationParser, Region, TypeId,
    block::BlockId,
    builtin::BuiltinDialect,
    dialects::scf::ScfDialect,
    ir_formatter::IRFormatter,
    operation::{
        ImplementsOpInterface, OpInterfaceConverter, downcast_op_interface, op_interface_converter,
    },
    parse::text::Parser as IRParser,
    ptr::PtrDialect,
    region::RegionId,
    ty::{Type, TypeParser},
    value::{Value, ValueId},
    vector::VectorDialect,
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

/// Read an entry from a slab arena indexed by a dense id, or `None` if the id was
/// never inserted or has been removed.
fn slab_get<T>(slab: &[Option<T>], idx: usize) -> Option<&T> {
    slab.get(idx).and_then(Option::as_ref)
}

/// Mutable counterpart of [`slab_get`], for in-place edits via `Arc::make_mut`.
fn slab_get_mut<T>(slab: &mut [Option<T>], idx: usize) -> Option<&mut T> {
    slab.get_mut(idx).and_then(Option::as_mut)
}

/// Insert into a slab arena at a dense id, growing the backing vector as needed.
/// Ids come from per-context monotonic counters, so the vector stays dense.
fn slab_put<T>(slab: &mut Vec<Option<T>>, idx: usize, val: T) {
    if idx >= slab.len() {
        slab.resize_with(idx + 1, || None);
    }
    slab[idx] = Some(val);
}

struct ContextInstance {
    // Arenas are slabs indexed by the dense, monotonic id counters below; see `slab_get`.
    operations: Vec<Option<Arc<OpInstance>>>,
    last_op_id: AtomicU32,
    values: Vec<Option<Arc<Value>>>,
    last_value_id: AtomicU32,
    regions: Vec<Option<Arc<Region>>>,
    last_region_id: AtomicU32,
    blocks: Vec<Option<Arc<Block>>>,
    last_block_id: AtomicU32,
    /// Reverse index from an operation to the block that holds it, maintained by
    /// `Block`'s membership mutators. Lets `parent_block` answer in O(1) instead of
    /// scanning every block's operation list.
    op_parent: Vec<Option<BlockId>>,
    /// Reverse index from a block to the region that holds it, maintained by
    /// [`Region::add_block`]. Together with [`Region::parent_op`] it lets walks
    /// climb from an op to its enclosing ops.
    block_parent: Vec<Option<RegionId>>,
    /// Def-site index for block arguments: the block whose argument list a value
    /// entered. The counterpart of [`Value::defining_op`] for values no operation
    /// defines, and what bounds the scope a use of such a value can sit in.
    value_block: Vec<Option<BlockId>>,
    /// Structural version per op, bumped along the spine root-ward by every
    /// tree edit; see [`Context::op_version`].
    op_version: Vec<u32>,
    /// Ops whose own subtree an edit touched since the last
    /// [`Context::take_dirty_ops`], for scoping post-pass verification.
    dirty_ops: Vec<OpId>,
    dialects: HashMap<&'static str, Arc<dyn Dialect>>,
    /// Register-class names of registered targets, for resolving parsed
    /// `%virtN:CLASS` operands back to a [`RegClassId`].
    reg_classes: HashMap<&'static str, crate::backend::regalloc::RegClassId>,
    op_interface_converters:
        HashMap<(&'static str, &'static str, std::any::TypeId), OpInterfaceConverter>,
    type_cache: Vec<Arc<dyn Type>>,
    /// Interned type ids bucketed by [`Type::hash`], so [`Context::get_type_id`]
    /// only runs [`Type::eq`] against colliding candidates.
    type_lookup: HashMap<u64, Vec<TypeId>>,
}

impl ContextInstance {
    /// The op enclosing `block`, if the block sits in a region owned by one.
    fn enclosing_op(&self, block: BlockId) -> Option<OpId> {
        let region = *slab_get(&self.block_parent, block.index())?;
        slab_get(&self.regions, region.index())?.parent_op()
    }

    /// The op enclosing `op`, walking out through its block and region.
    fn enclosing_op_of(&self, op: OpId) -> Option<OpId> {
        let block = *slab_get(&self.op_parent, op.index())?;
        self.enclosing_op(block)
    }

    fn bump_version(&mut self, op: OpId) {
        if op.index() >= self.op_version.len() {
            self.op_version.resize(op.index() + 1, 0);
        }
        self.op_version[op.index()] += 1;
    }

    /// Record that `op`'s subtree changed. Consecutive edits to the same subtree
    /// collapse; [`Context::take_dirty_ops`] removes the rest of the duplicates.
    fn mark_dirty(&mut self, op: OpId) {
        if self.dirty_ops.last() != Some(&op) {
            self.dirty_ops.push(op);
        }
    }

    /// `op`'s subtree changed: bump it and every enclosing op, so an analysis
    /// keyed on any ancestor (the function root, typically) sees the edit.
    fn edit_subtree(&mut self, op: OpId) {
        self.mark_dirty(op);
        let mut current = Some(op);
        while let Some(op) = current {
            self.bump_version(op);
            current = self.enclosing_op_of(op);
        }
    }

    /// `op` itself changed (operands, attributes). The dirtied subtree is its
    /// owner's, so verification also sees the siblings the edit may have broken.
    fn edit_op(&mut self, op: OpId) {
        self.bump_version(op);
        match self.enclosing_op_of(op) {
            Some(parent) => self.edit_subtree(parent),
            None => self.mark_dirty(op),
        }
    }

    /// `block`'s contents changed.
    fn edit_block(&mut self, block: BlockId) {
        if let Some(op) = self.enclosing_op(block) {
            self.edit_subtree(op);
        }
    }

    /// `region`'s block list changed.
    fn edit_region(&mut self, region: RegionId) {
        if let Some(op) = slab_get(&self.regions, region.index()).and_then(|r| r.parent_op()) {
            self.edit_subtree(op);
        }
    }
}

fn type_hash(ty: &dyn Type) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(ty.dialect().as_bytes());
    ty.hash(&mut hasher);
    hasher.finish()
}

impl Context {
    /// Create a new empty context with no registered dialects.
    pub fn new() -> Self {
        Context(Arc::new(RwLock::new(ContextInstance {
            operations: Vec::new(),
            last_op_id: AtomicU32::new(0),
            values: Vec::new(),
            last_value_id: AtomicU32::new(0),
            regions: Vec::new(),
            last_region_id: AtomicU32::new(0),
            blocks: Vec::new(),
            last_block_id: AtomicU32::new(0),
            op_parent: Vec::new(),
            block_parent: Vec::new(),
            value_block: Vec::new(),
            op_version: Vec::new(),
            dirty_ops: Vec::new(),
            dialects: HashMap::new(),
            reg_classes: HashMap::new(),
            op_interface_converters: HashMap::new(),
            type_cache: vec![],
            type_lookup: HashMap::new(),
        })))
    }

    /// Create a new context with default dialects.
    pub fn with_default_dialects() -> Self {
        let context = Context::new();

        context.register_dialect::<BuiltinDialect>();
        context.register_dialect::<PtrDialect>();
        context.register_dialect::<ScfDialect>();
        context.register_dialect::<VectorDialect>();

        context
    }

    /// Slab capacities against live-entity counts, for the `TIR_MEM_STATS`
    /// census (see [`crate::memstats`]).
    pub fn slab_census(&self) -> crate::memstats::SlabCensus {
        let inner = self.0.read();
        fn live<T>(slab: &[Option<T>]) -> usize {
            slab.iter().filter(|slot| slot.is_some()).count()
        }
        crate::memstats::SlabCensus {
            ops_slab: inner.operations.len(),
            ops_live: live(&inner.operations),
            values_slab: inner.values.len(),
            values_live: live(&inner.values),
            blocks_slab: inner.blocks.len(),
            blocks_live: live(&inner.blocks),
            regions_slab: inner.regions.len(),
            regions_live: live(&inner.regions),
        }
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
        Arc::<dyn Dialect>::get_mut(&mut dialect)
            .unwrap()
            .register_types(self);
        self.0.write().dialects.insert(D::name(), dialect);
    }

    /// Register a target's register classes so the generic op parser can resolve a
    /// `%virtN:CLASS` operand's class name back to its [`RegClassId`]. Backends call
    /// this from `register_dialects` with their generated `register_info().classes`.
    pub fn register_reg_classes(&self, classes: &'static [crate::backend::regalloc::RegClassInfo]) {
        let mut inner = self.0.write();
        for class in classes {
            inner
                .reg_classes
                .insert(class.name, crate::backend::regalloc::RegClassId::new(class));
        }
    }

    /// Resolve a register-class name to its [`RegClassId`], if a target that defines
    /// it has been registered (see [`Context::register_reg_classes`]).
    pub fn resolve_reg_class(&self, name: &str) -> Option<crate::backend::regalloc::RegClassId> {
        self.0.read().reg_classes.get(name).copied()
    }

    pub fn find_dialect<D: Dialect>(&self) -> Option<Arc<D>> {
        self.0
            .read()
            .dialects
            .get(D::name())
            .cloned()
            .and_then(|d| {
                let d: Arc<dyn Any + Send + Sync> = d;
                d.downcast::<D>().ok()
            })
    }

    pub fn add_operation(&self, instance: OpInstance) -> Arc<OpInstance> {
        // Machine ops carry their register operands in role-tagged attributes;
        // resolve the opcode's roles through its RegisterSemantics interface
        // before taking the write lock (the tables are `'static`, so they
        // outlive the probe).
        let probe = Arc::new(instance);
        let attribute_roles = self
            .get_op_interface::<dyn crate::attributes::RegisterSemantics>(probe.clone())
            .map(|semantics| semantics.attribute_roles())
            .unwrap_or_default();
        let mut instance = Arc::try_unwrap(probe).expect("probe interface dropped");

        let mut inner = self.0.write();

        let op_id = OpId::new(
            inner
                .last_op_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        instance.id = op_id;

        // Results are created before op id assignment in builders; patch their def-site now.
        for result_id in &instance.results {
            if let Some(value) = slab_get_mut(&mut inner.values, result_id.index()) {
                Arc::make_mut(value).set_defining_op(op_id);
            }
        }

        // A `Def`-role register attribute is the def-site of its virtual value, the
        // machine-IR spelling of an SSA result. Virtual register ids are value
        // numbers; physical registers have none and are skipped — they are not SSA.
        // ReadWrite defines too.
        for (attr_name, role) in attribute_roles {
            use crate::attributes::{AttributeRole, AttributeValue, RegisterAttr};
            if !matches!(role, AttributeRole::Def | AttributeRole::ReadWrite) {
                continue;
            }
            let Some(attr) = instance.attributes.iter().find(|a| a.name == *attr_name) else {
                continue;
            };
            let AttributeValue::Register(register) = &attr.value else {
                continue;
            };
            let id = match register {
                RegisterAttr::Virtual { id, .. }
                | RegisterAttr::FixedUse { id, .. }
                | RegisterAttr::FixedDef { id, .. } => id,
                RegisterAttr::Physical { .. } => continue,
            };
            let value_id = ValueId::from_number(*id);
            if let Some(value) = slab_get_mut(&mut inner.values, value_id.index()) {
                Arc::make_mut(value).set_defining_op(op_id);
            }
        }

        for r in &instance.regions {
            slab_get(&inner.regions, r.index())
                .unwrap()
                .set_parent_op(op_id);
        }

        let instance = Arc::new(instance);

        slab_put(&mut inner.operations, op_id.index(), instance.clone());

        instance
    }

    pub fn has_operation(&self, id: OpId) -> bool {
        slab_get(&self.0.read().operations, id.index()).is_some()
    }

    /// Replace an operation's attributes in place, keeping its id, position, and
    /// regions. Register allocation uses this to rewrite virtual register operands
    /// to physical ones once the def-use chain is no longer needed; it deliberately
    /// does not update `Value::uses`, since physical registers are not SSA values.
    pub fn set_op_attributes(&self, id: OpId, attributes: Vec<crate::attributes::NamedAttribute>) {
        let mut inner = self.0.write();
        if let Some(existing) = slab_get_mut(&mut inner.operations, id.index()) {
            Arc::make_mut(existing).attributes = attributes;
            inner.edit_op(id);
        }
    }

    /// The structural version of `op`: a counter bumped by every edit to `op` or
    /// to anything under it. Analyses cached against a version are stale as soon
    /// as it moves; see [`crate::analysis::AnalysisManager`].
    pub fn op_version(&self, op: OpId) -> u32 {
        self.0
            .read()
            .op_version
            .get(op.index())
            .copied()
            .unwrap_or(0)
    }

    /// The subtrees edited since the last call, innermost-dirtied op per edit and
    /// deduplicated. The pass manager drains this to scope post-pass verification.
    pub(crate) fn take_dirty_ops(&self) -> Vec<OpId> {
        let mut dirty = std::mem::take(&mut self.0.write().dirty_ops);
        dirty.sort_unstable();
        dirty.dedup();
        dirty
    }

    /// Remove an op from the operation arena. Called by `Rewriter::erase_op`/
    /// `replace_op` once the op has left its block, so the arena tracks the *live*
    /// IR rather than accumulating detached ops (which otherwise show up as phantom
    /// references to any whole-program scan). Existing `Arc<OpInstance>` handles
    /// (e.g. inside an `OperationRef`) keep the instance alive after removal.
    pub(crate) fn remove_operation(&self, id: OpId) {
        let mut inner = self.0.write();
        if let Some(slot) = inner.operations.get_mut(id.index()) {
            *slot = None;
        }
    }

    /// Replace a single operation's SSA operand at `index`. Used by register
    /// allocation to retarget a terminator's return value onto a freshly copied
    /// register.
    pub fn set_op_operand(&self, id: OpId, index: usize, new: ValueId) {
        let mut inner = self.0.write();
        match slab_get(&inner.operations, id.index()).and_then(|op| op.operands.get(index).copied())
        {
            Some(old) if old != new => {}
            _ => return,
        }
        if let Some(op) = slab_get_mut(&mut inner.operations, id.index()) {
            Arc::make_mut(op).operands[index] = new;
        }
        inner.edit_op(id);
    }

    /// Replace all of an operation's SSA operands. Register allocation uses this
    /// to clear a branch's forwarded block arguments once they have been lowered
    /// to explicit copies.
    pub fn set_op_operands(&self, id: OpId, operands: Vec<ValueId>) {
        let mut inner = self.0.write();
        if let Some(op) = slab_get_mut(&mut inner.operations, id.index()) {
            Arc::make_mut(op).operands = operands;
            inner.edit_op(id);
        }
    }

    pub fn create_value(&self, ty: TypeId, defining_op: Option<OpId>) -> Value {
        let mut inner = self.0.write();

        let value_id = ValueId::new(
            inner
                .last_value_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        let value = Value::new(value_id, ty, defining_op);
        slab_put(&mut inner.values, value_id.index(), Arc::new(value.clone()));

        value
    }

    pub fn get_value(&self, id: ValueId) -> Arc<Value> {
        let inner = self.0.read();
        slab_get(&inner.values, id.index()).unwrap().clone()
    }

    /// Replace every SSA operand use of `old` with `new`.
    ///
    /// The green core keeps no use lists, so the uses are found by walking the
    /// scope a use of `old` can sit in (see [`Context::use_scope`]); the
    /// [`DefUse`](crate::analysis::DefUse) analysis is the cached index passes
    /// query. Register-attribute uses are intentionally left untouched: they are
    /// not SSA operands and belong to machine IR.
    pub fn replace_value_uses(&self, old: ValueId, new: ValueId) {
        if old == new {
            return;
        }

        let ops = match self.use_scope(old) {
            Some(root) => self.ops_under(root),
            None => self.live_ops(),
        };

        let mut inner = self.0.write();
        for op in ops {
            let uses_old = slab_get(&inner.operations, op.index())
                .is_some_and(|instance| instance.operands.contains(&old));
            if !uses_old {
                continue;
            }
            if let Some(instance) = slab_get_mut(&mut inner.operations, op.index()) {
                for operand in Arc::make_mut(instance).operands.iter_mut() {
                    if *operand == old {
                        *operand = new;
                    }
                }
            }
            inner.edit_op(op);
        }
    }

    /// The operation whose subtree holds every use of `value`: the one enclosing
    /// its definition, since SSA confines a use to the region tree the definition
    /// sits in. `None` for a value whose def-site left the tree, which forces a
    /// scan of everything live.
    fn use_scope(&self, value: ValueId) -> Option<OpId> {
        let inner = self.0.read();
        match slab_get(&inner.values, value.index())?.defining_op() {
            Some(op) => inner.enclosing_op_of(op),
            None => inner.enclosing_op(*slab_get(&inner.value_block, value.index())?),
        }
    }

    /// Every operation nested under `root`, at any depth.
    fn ops_under(&self, root: OpId) -> Vec<OpId> {
        let blocks: Vec<BlockId> = self
            .get_op(root)
            .regions
            .iter()
            .flat_map(|region| self.get_region(*region).block_ids())
            .collect();
        self.subtree(&blocks).0
    }

    fn live_ops(&self) -> Vec<OpId> {
        let inner = self.0.read();
        inner
            .operations
            .iter()
            .flatten()
            .map(|instance| instance.id)
            .collect()
    }

    pub fn has_value(&self, id: ValueId) -> bool {
        slab_get(&self.0.read().values, id.index()).is_some()
    }

    pub fn has_region(&self, id: RegionId) -> bool {
        slab_get(&self.0.read().regions, id.index()).is_some()
    }

    pub fn has_block(&self, id: BlockId) -> bool {
        slab_get(&self.0.read().blocks, id.index()).is_some()
    }

    pub fn is_block_argument(&self, id: ValueId) -> bool {
        let inner = self.0.read();
        slab_get(&inner.value_block, id.index()).is_some()
    }

    pub fn create_region(&self) -> Arc<Region> {
        let mut inner = self.0.write();

        let region_id = RegionId::new(
            inner
                .last_region_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        let region = Arc::new(Region::new(region_id, self.as_context_ref()));
        slab_put(&mut inner.regions, region_id.index(), region.clone());

        region
    }

    pub fn create_block(&self, arguments: Vec<Value>) -> Arc<Block> {
        let context = self.as_context_ref();
        let mut inner = self.0.write();

        let block_id = BlockId::new(
            inner
                .last_block_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        for argument in &arguments {
            slab_put(&mut inner.value_block, argument.id().index(), block_id);
        }
        let block = Arc::new(Block::new(block_id, arguments, context));
        slab_put(&mut inner.blocks, block_id.index(), block.clone());

        block
    }

    /// Append `value`'s type as a new entry argument of `block` and return the
    /// argument. Block ids are stable across the edit, so branches naming this
    /// block keep pointing at it.
    pub fn append_block_argument(&self, block: BlockId, ty: TypeId) -> Value {
        let value = self.create_value(ty, None);
        let mut inner = self.0.write();
        if let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) {
            Arc::make_mut(entry).arguments_mut().push(value.clone());
            slab_put(&mut inner.value_block, value.id().index(), block);
            inner.edit_block(block);
        }
        value
    }

    /// Begin building a region body off to the side of the live IR.
    ///
    /// The staged blocks belong to no region, so building them bumps no version
    /// and dirties no subtree. Hand the result to
    /// [`Context::replace_region_contents`] to swap it in, or drop it to discard.
    pub fn stage_region(&self) -> StagedRegion {
        StagedRegion {
            context: self.clone(),
            blocks: Vec::new(),
            remap: Vec::new(),
            discard: true,
        }
    }

    /// Swap `staged` in as `region`'s contents, in one edit.
    ///
    /// The old contents leave the tree: their ops are removed from the arena, the
    /// uses they held on surviving values are dropped, and their parent links are
    /// cleared, so no walk reaches into them. Uses of old values recorded with
    /// [`StagedRegion::replace_value`] are then retargeted to their staged
    /// replacements. The swap itself bumps the spine exactly once, at `region`'s
    /// owner, and dirties that one subtree.
    pub fn replace_region_contents(&self, region: RegionId, mut staged: StagedRegion) {
        staged.discard = false;
        let handle = self.get_region(region);
        let owner = handle.parent_op();

        self.detach_subtree(&handle.block_ids());
        handle.set_blocks(staged.blocks.clone());

        {
            let mut inner = self.0.write();
            for &block in &staged.blocks {
                slab_put(&mut inner.block_parent, block.index(), region);
            }
            if let Some(owner) = owner {
                inner.edit_subtree(owner);
            }
        }

        for &(old, new) in &staged.remap {
            self.replace_value_uses(old, new);
        }
    }

    /// Every op and block under `blocks`, transitively through nested regions.
    /// Collected without the context lock held, so no region lock is ever taken
    /// under it.
    fn subtree(&self, blocks: &[BlockId]) -> (Vec<OpId>, Vec<BlockId>) {
        let mut pending = blocks.to_vec();
        let mut visited_blocks = Vec::new();
        let mut ops = Vec::new();
        while let Some(block) = pending.pop() {
            visited_blocks.push(block);
            for op in self.get_block(block).op_ids() {
                ops.push(op);
                for region in &self.get_op(op).regions {
                    pending.extend(self.get_region(*region).block_ids());
                }
            }
        }
        (ops, visited_blocks)
    }

    /// Take the subtree under `blocks` out of the live IR: clear the parent links
    /// a walk would follow and remove the ops from the arena. Bumps no version —
    /// the caller reports the edit.
    fn detach_subtree(&self, blocks: &[BlockId]) {
        let (ops, blocks) = self.subtree(blocks);
        let mut inner = self.0.write();
        for op in ops {
            if let Some(slot) = inner.op_parent.get_mut(op.index()) {
                *slot = None;
            }
            if let Some(slot) = inner.operations.get_mut(op.index()) {
                *slot = None;
            }
        }
        for block in blocks {
            if let Some(slot) = inner.block_parent.get_mut(block.index()) {
                *slot = None;
            }
        }
    }

    /// Insert `op` into `block` at `index`, recording the new parent.
    pub(crate) fn insert_op(&self, block: BlockId, index: usize, op: OpId) {
        let mut inner = self.0.write();
        if let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) {
            Arc::make_mut(entry).operations_mut().insert(index, op);
        }
        slab_put(&mut inner.op_parent, op.index(), block);
        inner.edit_block(block);
    }

    /// Insert `op` after everything `block` currently holds.
    pub(crate) fn append_op(&self, block: BlockId, op: OpId) {
        let mut inner = self.0.write();
        if let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) {
            Arc::make_mut(entry).operations_mut().push(op);
        }
        slab_put(&mut inner.op_parent, op.index(), block);
        inner.edit_block(block);
    }

    pub(crate) fn replace_op_in_block(&self, block: BlockId, old: OpId, new: OpId) -> bool {
        let mut inner = self.0.write();
        let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) else {
            return false;
        };
        let operations = Arc::make_mut(entry).operations_mut();
        let Some(position) = operations.iter().position(|id| *id == old) else {
            return false;
        };
        operations[position] = new;
        if let Some(slot) = inner.op_parent.get_mut(old.index()) {
            *slot = None;
        }
        slab_put(&mut inner.op_parent, new.index(), block);
        inner.edit_block(block);
        true
    }

    pub(crate) fn remove_op_from_block(&self, block: BlockId, op: OpId) -> bool {
        let mut inner = self.0.write();
        let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) else {
            return false;
        };
        let operations = Arc::make_mut(entry).operations_mut();
        let Some(position) = operations.iter().position(|id| *id == op) else {
            return false;
        };
        operations.remove(position);
        if let Some(slot) = inner.op_parent.get_mut(op.index()) {
            *slot = None;
        }
        inner.edit_block(block);
        true
    }

    pub(crate) fn set_block_attr(
        &self,
        block: BlockId,
        name: &str,
        value: crate::attributes::AttributeValue,
    ) {
        let mut inner = self.0.write();
        if let Some(entry) = slab_get_mut(&mut inner.blocks, block.index()) {
            let attributes = Arc::make_mut(entry).attributes_mut();
            match attributes.iter_mut().find(|a| a.name == name) {
                Some(attribute) => attribute.value = value,
                None => attributes.push(crate::attributes::NamedAttribute::new(name, value)),
            }
            inner.edit_block(block);
        }
    }

    /// The block currently holding `op`, or `None` for an op not in any block (the
    /// root op, or one detached by a rewrite). Maintained by `Block`'s membership
    /// mutators; see [`ContextInstance::op_parent`].
    pub fn parent_block(&self, op: OpId) -> Option<BlockId> {
        slab_get(&self.0.read().op_parent, op.index()).copied()
    }

    /// The operation enclosing `op`: the owner of the region holding `op`'s
    /// block. `None` for a root op or one detached by a rewrite.
    pub fn parent_op(&self, op: OpId) -> Option<OpId> {
        self.0.read().enclosing_op_of(op)
    }

    /// The region currently holding `block`, or `None` for a detached block.
    /// Maintained by [`Region::add_block`]; see [`ContextInstance::block_parent`].
    pub fn parent_region(&self, block: BlockId) -> Option<RegionId> {
        slab_get(&self.0.read().block_parent, block.index()).copied()
    }

    pub(crate) fn set_block_parent(&self, block: BlockId, region: RegionId) {
        let mut inner = self.0.write();
        slab_put(&mut inner.block_parent, block.index(), region);
        inner.edit_region(region);
    }

    pub(crate) fn clear_block_parent(&self, block: BlockId) {
        let mut inner = self.0.write();
        let region = slab_get(&inner.block_parent, block.index()).copied();
        if let Some(slot) = inner.block_parent.get_mut(block.index()) {
            *slot = None;
        }
        if let Some(region) = region {
            inner.edit_region(region);
        }
    }

    pub fn get_block(&self, id: BlockId) -> Arc<Block> {
        let inner = self.0.read();

        slab_get(&inner.blocks, id.index()).unwrap().clone()
    }

    pub fn get_region(&self, id: RegionId) -> Arc<Region> {
        let inner = self.0.read();

        slab_get(&inner.regions, id.index()).unwrap().clone()
    }

    pub fn get_op(&self, id: OpId) -> Arc<OpInstance> {
        let inner = self.0.read();

        slab_get(&inner.operations, id.index()).unwrap().clone()
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
            .insert((dialect, op_name, std::any::TypeId::of::<I>()), converter);
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

        let dialect = inner.dialects.get(op.dialect().as_str()).unwrap();

        dialect.get_dyn_op(op)
    }

    pub(crate) fn get_op_interface<I: ?Sized + 'static>(
        &self,
        op: Arc<OpInstance>,
    ) -> Option<Box<I>> {
        let converter = self.find_op_interface::<I>(&op)?;
        let erased = converter(op);
        downcast_op_interface::<I>(erased)
    }

    pub(crate) fn find_op_interface<I: ?Sized + 'static>(
        &self,
        op: &OpInstance,
    ) -> Option<OpInterfaceConverter> {
        self.0
            .read()
            .op_interface_converters
            .get(&(
                op.dialect().as_str(),
                op.name().as_str(),
                std::any::TypeId::of::<I>(),
            ))
            .copied()
    }

    pub fn get_parser(&self, dialect: &str, name: &str) -> Result<OperationParser, Error> {
        let inner = self.0.read();

        let dialect = inner
            .dialects
            .get(dialect)
            .ok_or(Error::UnknownDialect(dialect.to_string()))?;

        dialect.get_parser(name)
    }

    pub fn get_type_parser(&self, dialect: &str, name: &str) -> Result<TypeParser, Error> {
        let inner = self.0.read();

        let dialect_impl = inner
            .dialects
            .get(dialect)
            .ok_or(Error::UnknownDialect(dialect.to_string()))?;

        if let Ok(parser) = dialect_impl.get_type_parser(name) {
            return Ok(parser);
        }

        let prefix: String = name
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
            .collect();

        if prefix.is_empty() || prefix == name {
            return Err(Error::UnknownType(dialect.to_string(), name.to_string()));
        }

        dialect_impl.get_type_parser(&prefix)
    }

    pub fn parse_type_mnemonic(&self, dialect: &str, name: &str) -> Result<TypeId, Error> {
        let parser = self.get_type_parser(dialect, name)?;
        let mut p = IRParser::new("");
        parser(name, &mut p, self).map_err(|(_, err)| err)
    }

    pub fn parse_type_spec(&self, spec: &str) -> Result<TypeId, Error> {
        let spec = spec.strip_prefix('!').unwrap_or(spec);
        if let Some((dialect, name)) = spec.split_once('.') {
            self.parse_type_mnemonic(dialect, name)
        } else {
            self.parse_type_mnemonic("builtin", spec)
        }
    }

    pub fn get_type_id(&self, ty: Arc<dyn Type>) -> TypeId {
        let hash = type_hash(&*ty);
        let mut inner = self.0.upgradable_read();
        if let Some(candidates) = inner.type_lookup.get(&hash) {
            for &id in candidates {
                if inner.type_cache[id.as_index()].eq(&*ty) {
                    return id;
                }
            }
        }

        inner.with_upgraded(|inner| {
            let id = TypeId::from_number(inner.type_cache.len() as u32);
            inner.type_cache.push(ty);
            inner.type_lookup.entry(hash).or_default().push(id);
            id
        })
    }

    pub fn get_type_data(&self, ty: TypeId) -> Arc<dyn Type> {
        self.0
            .read()
            .type_cache
            .get(ty.as_index())
            .cloned()
            .expect("unknown type id")
    }

    pub fn type_to_string(&self, ty: TypeId) -> String {
        let mut out = String::new();
        {
            let mut fmt = IRFormatter::new(&mut out);
            self.print_type(ty, &mut fmt)
                .expect("type print must succeed");
        }
        out
    }

    pub fn print_type(&self, ty: TypeId, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        let ty_data = self.get_type_data(ty);
        fmt.write("!")?;
        if ty_data.dialect() != "builtin" {
            fmt.write(format!("{}.", ty_data.dialect()))?;
        }
        ty_data.print(fmt)
    }
}

/// A region body under construction, detached from the live IR.
///
/// Its blocks, values and ops live in the context's arenas from the moment they
/// are built, but they belong to no region until the staging is committed: they
/// print nowhere, bump no version, and dirty no subtree. Staged ops may take
/// values defined outside the region as operands; those uses become live with the
/// commit and are dropped again if the staging is discarded.
///
/// Created by [`Context::stage_region`]; committed by
/// [`Context::replace_region_contents`], discarded by dropping it.
pub struct StagedRegion {
    context: Context,
    blocks: Vec<BlockId>,
    remap: Vec<(ValueId, ValueId)>,
    discard: bool,
}

impl StagedRegion {
    /// Append a block carrying one argument per entry of `argument_types`. The
    /// first staged block becomes the region's entry.
    pub fn append_block(&mut self, argument_types: &[TypeId]) -> BlockId {
        let arguments = argument_types
            .iter()
            .map(|&ty| self.context.create_value(ty, None))
            .collect();
        let block = self.context.create_block(arguments);
        self.blocks.push(block.id());
        block.id()
    }

    /// The `index`-th argument of a staged block, for staged ops to consume.
    pub fn block_argument(&self, block: BlockId, index: usize) -> Value {
        self.context.get_block(block).arguments()[index].clone()
    }

    /// Append `op` after everything the staged block holds.
    pub fn append_op(&self, block: BlockId, op: OpId) {
        self.context.append_op(block, op);
    }

    /// On commit, retarget every use of `old` that outlives the swap to `new`.
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) {
        self.remap.push((old, new));
    }
}

impl Drop for StagedRegion {
    fn drop(&mut self) {
        if self.discard {
            self.context.detach_subtree(&self.blocks);
        }
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
mod staging_tests {
    use super::Context;
    use crate::{
        Block, BlockId, IRFormatter, OpId, Operand, Operation, RegionId, ValueId, builtin, scf,
    };
    use std::sync::Arc;

    /// `module { func demo(%cond) { %c = 1; scf.if %cond { %old = 7; scf.yield }; return } }`
    /// — a region owned by an op nested inside a function, so a commit to it has a
    /// spine to bump and live values around it to reference.
    struct Fixture {
        module: OpId,
        func: OpId,
        if_op: OpId,
        then_region: RegionId,
        then_block: BlockId,
        /// `%old`, defined inside the region a commit replaces.
        old: ValueId,
        /// `%c`, defined outside it and still live after a commit.
        constant: ValueId,
        module_body: Arc<Block>,
    }

    fn fixture(context: &Context) -> Fixture {
        let i1 = builtin::IntegerType::new(context, 1);
        let i32_ty = builtin::IntegerType::new(context, 32);
        let unit = builtin::UnitType::new(context);

        let then_region = context.create_region();
        let then_block = context.create_block(vec![]);
        then_region.add_block(then_block.id());
        let old = builtin::ops::constant(context, 7, i32_ty).build();
        then_block.append(old.id());
        then_block.append(scf::ops::r#yield(context, vec![]).build().id());

        let else_region = context.create_region();
        let else_block = context.create_block(vec![]);
        else_region.add_block(else_block.id());
        else_block.append(scf::ops::r#yield(context, vec![]).build().id());

        let body = context.create_region();
        let cond = context.create_value(i1, None);
        let entry = context.create_block(vec![cond.clone()]);
        body.add_block(entry.id());
        let constant = builtin::ops::constant(context, 1, i32_ty).build();
        entry.append(constant.id());
        let if_op = scf::ops::r#if(
            context,
            cond.id(),
            vec![],
            Some(then_region.id()),
            Some(else_region.id()),
        )
        .build();
        entry.append(if_op.id());
        entry.append(
            builtin::ops::r#return(context, Operand::none())
                .build()
                .id(),
        );

        let func = builtin::ops::func(context, "demo", unit, Some(body.id())).build();
        let module = builtin::ops::module(context, None).build();
        module.body().append(func.id());

        Fixture {
            module: module.id(),
            func: func.id(),
            if_op: if_op.id(),
            then_region: then_region.id(),
            then_block: then_block.id(),
            old: old.result(),
            constant: constant.result(),
            module_body: context.get_block(module.body().id()),
        }
    }

    fn printed(context: &Context, module: OpId) -> String {
        let mut out = String::new();
        let mut fmt = IRFormatter::new(&mut out);
        let op = context.get_dyn_op(context.get_op(module));
        crate::print_ir(op.as_ref(), context, &mut fmt).expect("print must succeed");
        out
    }

    /// A staged `^block: scf.yield` body, ready to swap into an `scf.if` region.
    fn staged_yield(context: &Context) -> super::StagedRegion {
        let mut staged = context.stage_region();
        let block = staged.append_block(&[]);
        staged.append_op(block, scf::ops::r#yield(context, vec![]).build().id());
        staged
    }

    #[test]
    fn a_discarded_staging_leaves_the_tree_untouched() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let i32_ty = builtin::IntegerType::new(&context, 32);
        let before = printed(&context, f.module);
        let module_version = context.op_version(f.module);
        let if_version = context.op_version(f.if_op);
        context.take_dirty_ops();

        let staged_op = {
            let mut staged = context.stage_region();
            let block = staged.append_block(&[]);
            let add = builtin::ops::addi(&context, f.constant, f.constant, i32_ty).build();
            staged.append_op(block, add.id());
            add.id()
        };

        assert_eq!(printed(&context, f.module), before, "the IR is unchanged");
        assert_eq!(context.op_version(f.module), module_version);
        assert_eq!(context.op_version(f.if_op), if_version);
        assert!(context.take_dirty_ops().is_empty());
        assert!(!context.has_operation(staged_op), "staged ops are dropped");
        assert!(
            !crate::analysis::DefUse::new(&context, f.func).is_used(f.constant.number()),
            "a discarded staging leaves no uses of live values behind"
        );
    }

    #[test]
    fn a_commit_bumps_the_spine_once() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let module_version = context.op_version(f.module);
        let func_version = context.op_version(f.func);
        let if_version = context.op_version(f.if_op);
        context.take_dirty_ops();

        context.replace_region_contents(f.then_region, staged_yield(&context));

        assert_eq!(context.op_version(f.if_op), if_version + 1);
        assert_eq!(context.op_version(f.func), func_version + 1);
        assert_eq!(context.op_version(f.module), module_version + 1);
        assert_eq!(
            context.take_dirty_ops(),
            vec![f.if_op],
            "the region's owner is the one dirtied subtree"
        );
    }

    #[test]
    fn a_commit_detaches_the_old_subtree() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let old_op = context.get_value(f.old).defining_op().unwrap();

        context.replace_region_contents(f.then_region, staged_yield(&context));

        assert!(!context.has_operation(old_op));
        assert_eq!(context.parent_block(old_op), None);
        assert_eq!(context.parent_op(old_op), None);
        assert_eq!(context.parent_region(f.then_block), None);
        assert!(!printed(&context, f.module).contains("7"));
        // Nothing dirtied walks into the detached subtree.
        crate::verify_op_tree(&context, f.if_op).expect("the committed tree verifies");
    }

    #[test]
    fn staged_ops_keep_their_live_operands() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let i32_ty = builtin::IntegerType::new(&context, 32);

        let mut staged = context.stage_region();
        let block = staged.append_block(&[]);
        let add = builtin::ops::addi(&context, f.constant, f.constant, i32_ty).build();
        staged.append_op(block, add.id());
        staged.append_op(block, scf::ops::r#yield(&context, vec![]).build().id());
        context.replace_region_contents(f.then_region, staged);

        assert_eq!(context.get_op(add.id()).operands, vec![f.constant; 2]);
        assert_eq!(context.parent_block(add.id()), Some(block));
        assert_eq!(context.parent_region(block), Some(f.then_region));
        assert_eq!(
            crate::analysis::DefUse::new(&context, f.func).users_of(f.constant.number()),
            [add.id(); 2]
        );
        crate::verify_op_tree(&context, f.func).expect("the committed tree verifies");
    }

    #[test]
    fn staged_blocks_carry_their_arguments() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let i32_ty = builtin::IntegerType::new(&context, 32);

        let mut staged = context.stage_region();
        let block = staged.append_block(&[i32_ty]);
        let argument = staged.block_argument(block, 0).id();
        let add = builtin::ops::addi(&context, argument, f.constant, i32_ty).build();
        staged.append_op(block, add.id());
        staged.append_op(block, scf::ops::r#yield(&context, vec![]).build().id());
        context.replace_region_contents(f.then_region, staged);

        let committed = context.get_block(block);
        assert_eq!(committed.arguments().len(), 1);
        assert_eq!(committed.arguments()[0].id(), argument);
        assert_eq!(context.get_op(add.id()).operands[0], argument);
    }

    #[test]
    fn a_commit_remaps_uses_of_replaced_values() {
        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let i32_ty = builtin::IntegerType::new(&context, 32);
        // A use of the old region's value that outlives the swap.
        let user = builtin::ops::addi(&context, f.old, f.old, i32_ty).build();
        f.module_body.append(user.id());

        let mut staged = context.stage_region();
        let block = staged.append_block(&[]);
        let fresh = builtin::ops::constant(&context, 9, i32_ty).build();
        staged.append_op(block, fresh.id());
        staged.append_op(block, scf::ops::r#yield(&context, vec![]).build().id());
        staged.replace_value(f.old, fresh.result());
        context.replace_region_contents(f.then_region, staged);

        assert_eq!(
            context.get_op(user.id()).operands,
            vec![fresh.result(); 2],
            "surviving uses read the staged replacement"
        );
        assert!(!crate::analysis::DefUse::new(&context, f.module).is_used(f.old.number()));
    }

    #[test]
    fn a_commit_keeps_analyses_of_untouched_functions() {
        use crate::{Analysis, AnalysisManager, OpId as Id};
        struct Probe;
        impl Analysis for Probe {
            fn build(_: &AnalysisManager, _: &Context, _: Id) -> Self {
                Probe
            }
        }

        let context = Context::with_default_dialects();
        let f = fixture(&context);
        let unit = builtin::UnitType::new(&context);
        let sibling_body = context.create_region();
        let sibling_entry = context.create_block(vec![]);
        sibling_body.add_block(sibling_entry.id());
        sibling_entry.append(
            builtin::ops::r#return(&context, Operand::none())
                .build()
                .id(),
        );
        let sibling = builtin::ops::func(&context, "sib", unit, Some(sibling_body.id())).build();
        f.module_body.append(sibling.id());

        let analyses = AnalysisManager::new();
        analyses.get::<Probe>(&context, sibling.id());
        analyses.get::<Probe>(&context, f.func);

        context.replace_region_contents(f.then_region, staged_yield(&context));

        assert!(
            analyses
                .get_cached::<Probe>(&context, sibling.id())
                .is_some(),
            "a sibling function's analyses survive a commit elsewhere"
        );
        assert!(analyses.get_cached::<Probe>(&context, f.func).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::Context;
    use crate::{Block, Commutative, OpId, Operand, Operation, Terminator, builtin};
    use std::sync::Arc;

    #[test]
    fn default_context() {
        let _ = Context::with_default_dialects();
    }

    /// `module { func demo { ^entry: } }` — the func body sits two regions deep,
    /// so an edit there must reach the module to prove root-ward propagation.
    fn module_with_function(context: &Context) -> (OpId, OpId, Arc<Block>) {
        let i32 = builtin::IntegerType::new(context, 32);
        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        let func = builtin::ops::func(context, "demo", i32, Some(region.id())).build();
        let module = builtin::ops::module(context, None).build();
        module.body().append(func.id());
        (module.id(), func.id(), context.get_block(block.id()))
    }

    /// Runs `edit` and asserts it bumped the versions of both enclosing ops.
    fn assert_bumps_spine(context: &Context, module: OpId, func: OpId, edit: impl FnOnce()) {
        let module_before = context.op_version(module);
        let func_before = context.op_version(func);
        edit();
        assert!(
            context.op_version(func) > func_before,
            "the edited op's owner must be dirtied"
        );
        assert!(
            context.op_version(module) > module_before,
            "the bump must propagate root-ward"
        );
    }

    #[test]
    fn appending_an_op_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        assert_bumps_spine(&context, module, func, || {
            body.append(
                builtin::ops::r#return(&context, Operand::none())
                    .build()
                    .id(),
            );
        });
    }

    #[test]
    fn inserting_an_op_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        assert_bumps_spine(&context, module, func, || {
            body.insert(
                0,
                builtin::ops::r#return(&context, Operand::none())
                    .build()
                    .id(),
            );
        });
    }

    #[test]
    fn removing_an_op_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let ret = builtin::ops::r#return(&context, Operand::none()).build();
        body.append(ret.id());
        assert_bumps_spine(&context, module, func, || {
            assert!(body.remove_op(ret.id()));
        });
    }

    #[test]
    fn replacing_an_op_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let old = builtin::ops::r#return(&context, Operand::none()).build();
        body.append(old.id());
        let new = builtin::ops::r#return(&context, Operand::none()).build();
        assert_bumps_spine(&context, module, func, || {
            assert!(body.replace_op(old.id(), new.id()));
        });
    }

    #[test]
    fn appending_a_block_argument_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let i32 = builtin::IntegerType::new(&context, 32);
        assert_bumps_spine(&context, module, func, || {
            context.append_block_argument(body.id(), i32);
        });
    }

    #[test]
    fn setting_a_block_attribute_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        assert_bumps_spine(&context, module, func, || {
            body.set_attr("fpmath", crate::attributes::AttributeValue::Bool(true));
        });
    }

    #[test]
    fn adding_a_block_to_a_region_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, _) = module_with_function(&context);
        let region = context.get_region(context.get_op(func).regions[0]);
        let extra = context.create_block(vec![]);
        assert_bumps_spine(&context, module, func, || {
            region.add_block(extra.id());
        });
        assert_bumps_spine(&context, module, func, || {
            assert!(region.remove_block(extra.id()));
        });
    }

    #[test]
    fn setting_op_attributes_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let ret = builtin::ops::r#return(&context, Operand::none()).build();
        body.append(ret.id());
        assert_bumps_spine(&context, module, func, || {
            context.set_op_attributes(ret.id(), vec![]);
        });
    }

    #[test]
    fn setting_an_operand_bumps_the_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let i32 = builtin::IntegerType::new(&context, 32);
        let a = context.create_value(i32, None);
        let b = context.create_value(i32, None);
        let add = builtin::ops::addi(&context, a.id(), a.id(), i32).build();
        body.append(add.id());
        assert_bumps_spine(&context, module, func, || {
            context.set_op_operand(add.id(), 1, b.id());
        });
        assert_bumps_spine(&context, module, func, || {
            context.set_op_operands(add.id(), vec![b.id(), b.id()]);
        });
    }

    #[test]
    fn replacing_value_uses_bumps_the_users_spine() {
        let context = Context::with_default_dialects();
        let (module, func, body) = module_with_function(&context);
        let i32 = builtin::IntegerType::new(&context, 32);
        let a = context.create_value(i32, None);
        let b = context.create_value(i32, None);
        let add = builtin::ops::addi(&context, a.id(), a.id(), i32).build();
        body.append(add.id());
        assert_bumps_spine(&context, module, func, || {
            context.replace_value_uses(a.id(), b.id());
        });
    }

    #[test]
    fn an_edit_dirties_only_its_own_subtree() {
        let context = Context::with_default_dialects();
        let (_, func, body) = module_with_function(&context);
        let (_, untouched, _) = module_with_function(&context);
        context.take_dirty_ops();

        body.append(
            builtin::ops::r#return(&context, Operand::none())
                .build()
                .id(),
        );

        let dirty = context.take_dirty_ops();
        assert_eq!(dirty, vec![func], "only the edited subtree is verified");
        assert!(!dirty.contains(&untouched));
        assert!(
            context.take_dirty_ops().is_empty(),
            "draining leaves nothing behind"
        );
    }

    #[test]
    fn an_untouched_function_keeps_its_version() {
        let context = Context::with_default_dialects();
        let (_, edited, body) = module_with_function(&context);
        let (_, untouched, _) = module_with_function(&context);
        let before = context.op_version(untouched);

        body.append(
            builtin::ops::r#return(&context, Operand::none())
                .build()
                .id(),
        );

        assert!(context.op_version(edited) > 0);
        assert_eq!(context.op_version(untouched), before);
    }

    #[test]
    fn parent_block_tracks_membership() {
        use crate::IRBuilder;
        let context = Context::with_default_dialects();
        let i32 = builtin::IntegerType::new(&context, 32);
        let a = context.create_value(i32, None);
        let b = context.create_value(i32, None);

        let block = context.create_block(vec![]);
        let mut builder = IRBuilder::new(block.clone());
        let add = builder.insert(builtin::ops::addi(&context, a.id(), b.id(), i32).build());

        // Inserting into a block records the parent, reachable from just the op.
        assert_eq!(context.parent_block(add.id()), Some(block.id()));
        assert_eq!(context.get_op(add.id()).parent_block(), Some(block.id()));

        // Replacing swaps the parent over to the new op; the old op is detached.
        let sub = builtin::ops::subi(&context, a.id(), b.id(), i32).build();
        assert!(block.replace_op(add.id(), sub.id()));
        assert_eq!(context.parent_block(add.id()), None);
        assert_eq!(context.parent_block(sub.id()), Some(block.id()));

        // Removing clears it.
        assert!(block.remove_op(sub.id()));
        assert_eq!(context.parent_block(sub.id()), None);
    }

    #[test]
    fn parent_region_tracks_membership() {
        let context = Context::with_default_dialects();
        let region = context.create_region();
        let block = context.create_block(vec![]);

        region.add_block(block.id());
        assert_eq!(context.parent_region(block.id()), Some(region.id()));

        assert!(region.remove_block(block.id()));
        assert_eq!(context.parent_region(block.id()), None);
    }

    #[test]
    fn replacing_value_uses_reaches_a_nested_region() {
        let context = Context::with_default_dialects();
        let (_, _, body) = module_with_function(&context);
        let i32 = builtin::IntegerType::new(&context, 32);
        let i1 = builtin::IntegerType::new(&context, 1);
        let a = context.create_value(i32, None);
        let b = context.create_value(i32, None);
        let cond = context.create_value(i1, None);

        let then_region = context.create_region();
        let then_block = context.create_block(vec![]);
        then_region.add_block(then_block.id());
        let nested = builtin::ops::addi(&context, a.id(), a.id(), i32).build();
        then_block.append(nested.id());
        then_block.append(crate::scf::ops::r#yield(&context, vec![]).build().id());
        let else_region = context.create_region();
        let else_block = context.create_block(vec![]);
        else_region.add_block(else_block.id());
        else_block.append(crate::scf::ops::r#yield(&context, vec![]).build().id());
        body.append(
            crate::scf::ops::r#if(
                &context,
                cond.id(),
                vec![],
                Some(then_region.id()),
                Some(else_region.id()),
            )
            .build()
            .id(),
        );

        context.replace_value_uses(a.id(), b.id());

        assert_eq!(context.get_op(nested.id()).operands, vec![b.id(); 2]);
    }

    #[test]
    fn replacing_uses_of_a_block_argument_rewrites_its_readers() {
        let context = Context::with_default_dialects();
        let context_ref = &context;
        let i32 = builtin::IntegerType::new(context_ref, 32);
        let region = context.create_region();
        let argument = context.create_value(i32, None);
        let block = context.create_block(vec![argument.clone()]);
        region.add_block(block.id());
        let func = builtin::ops::func(&context, "demo", i32, Some(region.id())).build();
        let module = builtin::ops::module(&context, None).build();
        module.body().append(func.id());
        let reader = builtin::ops::addi(&context, argument.id(), argument.id(), i32).build();
        block.append(reader.id());
        let replacement = context.create_value(i32, None);

        context.replace_value_uses(argument.id(), replacement.id());

        assert_eq!(
            context.get_op(reader.id()).operands,
            vec![replacement.id(); 2]
        );
    }

    #[test]
    fn custom_interface_for_existing_op() {
        let context = Context::with_default_dialects();

        let lhs = context.create_value(builtin::IntegerType::new(&context, 32), None);
        let rhs = context.create_value(builtin::IntegerType::new(&context, 32), None);
        let add = builtin::ops::addi(
            &context,
            lhs.id(),
            rhs.id(),
            builtin::IntegerType::new(&context, 32),
        )
        .build();

        assert!(context.get_op(add.id()).has_interface::<dyn Commutative>());
        let iface = context
            .get_op(add.id())
            .as_interface::<dyn Commutative>()
            .expect("interface should be available");
        assert!(iface.is_commutative());
    }

    #[test]
    fn builtin_terminator_interface() {
        let context = Context::with_default_dialects();
        let value = context.create_value(builtin::IntegerType::new(&context, 32), None);
        let ret = builtin::ops::r#return(&context, value.id()).build();

        let iface = context
            .get_op(ret.id())
            .as_interface::<dyn Terminator>()
            .expect("terminator interface should be available");
        assert!(iface.is_terminator());
    }
}
