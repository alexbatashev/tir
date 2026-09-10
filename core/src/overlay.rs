//! The overlay a context edits through: a delta over a frozen base, finished
//! into an owned batch and committed once every reader of the base is gone.
//!
//! Every edit is visible to the next read. A base entity is copied into the
//! delta on its first edit and read from there afterwards; an erased base
//! entity is absent from every lookup; a value replaced wholesale is resolved
//! on every operand read of an unedited base op. Use lists are the union of
//! the base list, minus the slots of edited or erased base ops, and the
//! delta's own list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::attributes::{AttributeValue, NamedAttribute};
use crate::run::NO_ENTRY;
use crate::store::{Frontier, Parent, Store, slab_get};
use crate::{Block, BlockId, OpId, OpInstance, Region, RegionId, Use, Value, ValueId};

/// An immutable base every reader of an epoch shares. Cloning hands out one
/// more reader; a commit needs them all gone.
#[derive(Clone)]
pub struct Frozen(pub(crate) Arc<Store>);

impl Frozen {
    pub(crate) fn empty() -> Self {
        Frozen(Arc::new(Store::base()))
    }

    /// An address identifying this base, so a batch can say which it edits.
    pub(crate) fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

/// The edits an overlay holds against its base.
#[derive(Clone)]
pub(crate) struct Delta {
    pub store: Store,
    pub frontier: Frontier,
    erased_ops: Vec<bool>,
    erased_values: Vec<bool>,
    erased_blocks: Vec<bool>,
    erased_regions: Vec<bool>,
    /// Base value → the value every remaining base read of it resolves to.
    /// Always one hop: a target is resolved when it is recorded.
    replaced: Vec<u32>,
    replaced_count: usize,
    /// The ids set in the tables above, so finishing reads what was touched
    /// and not the width of the module.
    touched: Touched,
    /// Value → the replaced values whose surviving base uses it owns now.
    sources: HashMap<ValueId, Vec<ValueId>>,
    /// Version bumps this overlay made, by op id.
    revision: Vec<u32>,
    /// Ops whose subtree an edit touched, innermost per edit.
    dirty: Vec<OpId>,
    /// Erased op → the op that took its place, so a pipeline can follow its
    /// root across a replacement.
    pub replaced_ops: HashMap<OpId, OpId>,
    /// `(block, old, new)`: a base block copied only to put `new` where `old`
    /// stood; see [`Delta::fold_swaps`].
    swaps: Vec<(BlockId, OpId, OpId)>,
}

/// Which entries of the delta's dense per-id tables were ever set.
#[derive(Clone, Default)]
struct Touched {
    erased_ops: Vec<u32>,
    erased_values: Vec<u32>,
    erased_blocks: Vec<u32>,
    erased_regions: Vec<u32>,
    replaced: Vec<u32>,
    revised: Vec<u32>,
}

/// What an overlay counted when it was measured.
#[derive(Debug, Default, Clone, Copy)]
pub struct OverlayCensus {
    /// Entities the overlay created.
    pub local: usize,
    /// Base entities the overlay copied to edit.
    pub shadows: usize,
    /// Values whose use list the overlay extended.
    pub use_deltas: usize,
    /// Base values the overlay replaced.
    pub replaced: usize,
    /// Bytes the overlay's hives and arenas hold.
    pub bytes: usize,
}

impl Delta {
    pub(crate) fn new(frontier: Frontier) -> Self {
        Delta {
            store: Store::delta(frontier),
            frontier,
            erased_ops: Vec::new(),
            erased_values: Vec::new(),
            erased_blocks: Vec::new(),
            erased_regions: Vec::new(),
            replaced: Vec::new(),
            replaced_count: 0,
            touched: Touched::default(),
            sources: HashMap::new(),
            revision: Vec::new(),
            dirty: Vec::new(),
            replaced_ops: HashMap::new(),
            swaps: Vec::new(),
        }
    }

    /// Drop the copy of a base block the overlay holds only because a
    /// replaced op stood in it, keeping the swap as a record instead. Two
    /// tasks replacing sibling callables of one module then write nothing in
    /// common: each swap names its own op, and commit applies them in turn.
    pub(crate) fn fold_swaps(&mut self, base: &Store) {
        let mut replaced: Vec<(OpId, OpId)> =
            self.replaced_ops.iter().map(|(o, n)| (*o, *n)).collect();
        replaced.sort_unstable();
        for (old, new) in replaced {
            let Some(Parent::Block(block)) = base.op_parent(old) else {
                continue;
            };
            if !self.store.shadows_block(block) {
                continue;
            }
            let (Some(copy), Some(original)) = (self.store.block(block), base.block(block)) else {
                continue;
            };
            let mut swapped = original.clone();
            for op in swapped.operations_mut() {
                if *op == old {
                    *op = new;
                }
            }
            if *copy != swapped || self.store.block_parent(block) != base.block_parent(block) {
                continue;
            }
            self.store.unshadow_block(block);
            self.swaps.push((block, old, new));
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.store.ops.is_empty()
            && self.store.values.is_empty()
            && self.store.blocks.is_empty()
            && self.store.regions.is_empty()
            && self.replaced_count == 0
            && !self.erased_ops.iter().any(|e| *e)
            && !self.erased_values.iter().any(|e| *e)
            && !self.erased_blocks.iter().any(|e| *e)
            && !self.erased_regions.iter().any(|e| *e)
    }

    pub(crate) fn census(&self) -> OverlayCensus {
        let shadows = self.store.shadowed_ops().len()
            + self.store.shadowed_values().len()
            + self.store.shadowed_blocks().len()
            + self.store.shadowed_regions().len();
        OverlayCensus {
            local: self.store.ops.len()
                + self.store.values.len()
                + self.store.blocks.len()
                + self.store.regions.len()
                - shadows,
            shadows,
            use_deltas: self
                .store
                .first_use
                .iter()
                .filter(|head| **head != NO_ENTRY)
                .count(),
            replaced: self.replaced_count,
            bytes: self.store.bytes(),
        }
    }

    fn flag(flags: &[bool], idx: usize) -> bool {
        flags.get(idx).copied().unwrap_or(false)
    }

    fn set_flag(flags: &mut Vec<bool>, touched: &mut Vec<u32>, idx: usize) {
        if idx >= flags.len() {
            flags.resize(idx + 1, false);
        }
        if !flags[idx] {
            touched.push(idx as u32);
        }
        flags[idx] = true;
    }

    pub(crate) fn erased_op(&self, id: OpId) -> bool {
        Self::flag(&self.erased_ops, id.index())
    }

    pub(crate) fn erased_value(&self, id: ValueId) -> bool {
        Self::flag(&self.erased_values, id.index())
    }

    pub(crate) fn erased_block(&self, id: BlockId) -> bool {
        Self::flag(&self.erased_blocks, id.index())
    }

    pub(crate) fn erased_region(&self, id: RegionId) -> bool {
        Self::flag(&self.erased_regions, id.index())
    }

    /// Whether a base op's slots in the base use lists no longer speak for
    /// it: the overlay holds its own copy, or erased it.
    fn suppressed(&self, base_op: OpId) -> bool {
        self.store.shadows_op(base_op) || self.erased_op(base_op)
    }

    // Reads: the delta's own copy first, then the base unless erased. Each
    // locates the record once and reads it by handle.

    fn locate_op<'a>(&'a self, base: &'a Store, id: OpId) -> Option<(&'a Store, u32)> {
        match self.store.op_handle(id) {
            Some(handle) => Some((&self.store, handle)),
            None if self.erased_op(id) => None,
            None => base.op_handle(id).map(|handle| (base, handle)),
        }
    }

    fn locate_value<'a>(&'a self, base: &'a Store, id: ValueId) -> Option<(&'a Store, u32)> {
        match self.store.value_handle(id) {
            Some(handle) => Some((&self.store, handle)),
            None if self.erased_value(id) => None,
            None => base.value_handle(id).map(|handle| (base, handle)),
        }
    }

    fn locate_block<'a>(&'a self, base: &'a Store, id: BlockId) -> Option<(&'a Store, u32)> {
        match self.store.block_handle(id) {
            Some(handle) => Some((&self.store, handle)),
            None if self.erased_block(id) => None,
            None => base.block_handle(id).map(|handle| (base, handle)),
        }
    }

    fn locate_region<'a>(&'a self, base: &'a Store, id: RegionId) -> Option<(&'a Store, u32)> {
        match self.store.region_handle(id) {
            Some(handle) => Some((&self.store, handle)),
            None if self.erased_region(id) => None,
            None => base.region_handle(id).map(|handle| (base, handle)),
        }
    }

    pub(crate) fn op<'a>(&'a self, base: &'a Store, id: OpId) -> Option<&'a OpInstance> {
        let (store, handle) = self.locate_op(base, id)?;
        store.ops.get(handle)
    }

    pub(crate) fn value<'a>(&'a self, base: &'a Store, id: ValueId) -> Option<&'a Value> {
        let (store, handle) = self.locate_value(base, id)?;
        store.values.get(handle)
    }

    pub(crate) fn block<'a>(&'a self, base: &'a Store, id: BlockId) -> Option<&'a Block> {
        let (store, handle) = self.locate_block(base, id)?;
        store.blocks.get(handle)
    }

    pub(crate) fn region<'a>(&'a self, base: &'a Store, id: RegionId) -> Option<&'a Region> {
        let (store, handle) = self.locate_region(base, id)?;
        store.regions.get(handle)
    }

    pub(crate) fn op_parent(&self, base: &Store, id: OpId) -> Option<Parent> {
        let (store, handle) = self.locate_op(base, id)?;
        store.ops.get(handle)?;
        slab_get(&store.op_parent, handle as usize).copied()
    }

    pub(crate) fn block_parent(&self, base: &Store, id: BlockId) -> Option<RegionId> {
        let (store, handle) = self.locate_block(base, id)?;
        store.blocks.get(handle)?;
        slab_get(&store.block_parent, handle as usize).copied()
    }

    pub(crate) fn value_block(&self, base: &Store, id: ValueId) -> Option<BlockId> {
        let (store, handle) = self.locate_value(base, id)?;
        store.values.get(handle)?;
        slab_get(&store.value_block, handle as usize).copied()
    }

    pub(crate) fn value_region(&self, base: &Store, id: ValueId) -> Option<RegionId> {
        let (store, handle) = self.locate_value(base, id)?;
        store.values.get(handle)?;
        slab_get(&store.value_region, handle as usize).copied()
    }

    /// What a read of `value` answers after the replacements so far.
    pub(crate) fn resolve(&self, value: ValueId) -> ValueId {
        if self.replaced_count == 0 {
            return value;
        }
        match self.replaced.get(value.index()) {
            Some(&target) if target != NO_ENTRY => ValueId::from_number(target),
            _ => value,
        }
    }

    pub(crate) fn op_operands(&self, base: &Store, id: OpId) -> crate::operation::ValueIds {
        let Some((store, handle)) = self.locate_op(base, id) else {
            return Default::default();
        };
        let Some(instance) = store.ops.get(handle) else {
            return Default::default();
        };
        let operands = store.operands_of(instance);
        if std::ptr::eq(store, &self.store) || self.replaced_count == 0 {
            return operands;
        }
        operands
            .into_iter()
            .map(|value| self.resolve(value))
            .collect()
    }

    pub(crate) fn op_results(&self, base: &Store, id: OpId) -> crate::operation::ValueIds {
        self.locate_op(base, id)
            .and_then(|(store, handle)| Some(store.results_of(store.ops.get(handle)?)))
            .unwrap_or_default()
    }

    pub(crate) fn op_regions(&self, base: &Store, id: OpId) -> crate::operation::RegionIds {
        self.locate_op(base, id)
            .and_then(|(store, handle)| Some(store.regions_of(store.ops.get(handle)?)))
            .unwrap_or_default()
    }

    pub(crate) fn op_attrs<'a>(
        &'a self,
        base: &'a Store,
        id: OpId,
    ) -> &'a [crate::attributes::NamedAttribute] {
        self.locate_op(base, id)
            .and_then(|(store, handle)| Some(store.attrs_of(store.ops.get(handle)?)))
            .unwrap_or(&[])
    }

    /// Every operand slot naming `value`: the base slots it and the values
    /// replaced by it still hold, minus those of base ops the overlay copied
    /// or erased, then the overlay's own slots. A replaced value's base slots
    /// belong to its replacement; only slots recorded after the replacement
    /// still name it.
    pub(crate) fn uses(&self, base: &Store, value: ValueId) -> Vec<Use> {
        let mut uses = Vec::new();
        let own = (self.resolve(value) == value).then_some(value);
        let sources = self.sources.get(&value);
        for source in own
            .into_iter()
            .chain(sources.into_iter().flatten().copied())
        {
            let mut held: Vec<Use> = base
                .use_entries(source)
                .map(|entry| base.locate(entry))
                .filter(|(owner, _)| !self.suppressed(*owner))
                .map(|(op, index)| Use::new(op, index))
                .collect();
            held.reverse();
            uses.extend(held);
        }
        uses.extend(self.store.uses(value));
        uses
    }

    pub(crate) fn is_used(&self, base: &Store, value: ValueId) -> bool {
        if self.store.use_head(value) != NO_ENTRY {
            return true;
        }
        let own = (self.resolve(value) == value).then_some(value);
        let sources = self.sources.get(&value);
        own.into_iter()
            .chain(sources.into_iter().flatten().copied())
            .any(|source| {
                base.use_entries(source)
                    .any(|entry| !self.suppressed(base.locate(entry).0))
            })
    }

    /// The op enclosing `op`, walking out through whatever holds it.
    pub(crate) fn enclosing_op_of(&self, base: &Store, op: OpId) -> Option<OpId> {
        match self.op_parent(base, op)? {
            Parent::Block(block) => self.enclosing_op(base, block),
            Parent::Region(region) => self.region(base, region)?.parent_op(),
        }
    }

    pub(crate) fn enclosing_op(&self, base: &Store, block: BlockId) -> Option<OpId> {
        let region = self.block_parent(base, block)?;
        self.region(base, region)?.parent_op()
    }

    // Versions.

    pub(crate) fn revision(&self, op: OpId) -> u32 {
        self.revision.get(op.index()).copied().unwrap_or(0)
    }

    pub(crate) fn bump(&mut self, op: OpId) {
        if op.index() >= self.revision.len() {
            self.revision.resize(op.index() + 1, 0);
        }
        let slot = &mut self.revision[op.index()];
        if *slot == 0 {
            self.touched.revised.push(op.index() as u32);
        }
        *slot = slot.checked_add(1).expect("a revision counter wrapped");
    }

    fn mark_dirty(&mut self, op: OpId) {
        if self.dirty.last() != Some(&op) {
            self.dirty.push(op);
        }
    }

    pub(crate) fn take_dirty(&mut self) -> Vec<OpId> {
        let mut dirty = std::mem::take(&mut self.dirty);
        dirty.sort_unstable();
        dirty.dedup();
        dirty
    }

    /// `op`'s subtree changed: bump it and every enclosing op.
    pub(crate) fn edit_subtree(&mut self, base: &Store, op: OpId) {
        self.mark_dirty(op);
        let mut current = Some(op);
        while let Some(op) = current {
            self.bump(op);
            current = self.enclosing_op_of(base, op);
        }
    }

    /// `op` itself changed (operands, attributes). The dirtied subtree is its
    /// owner's, so verification also sees the siblings the edit may have broken.
    pub(crate) fn edit_op(&mut self, base: &Store, op: OpId) {
        self.bump(op);
        match self.enclosing_op_of(base, op) {
            Some(parent) => self.edit_subtree(base, parent),
            None => self.mark_dirty(op),
        }
    }

    pub(crate) fn edit_block(&mut self, base: &Store, block: BlockId) {
        if let Some(op) = self.enclosing_op(base, block) {
            self.edit_subtree(base, op);
        }
    }

    pub(crate) fn edit_region(&mut self, base: &Store, region: RegionId) {
        if let Some(op) = self.region(base, region).and_then(Region::parent_op) {
            self.edit_subtree(base, op);
        }
    }

    // Edits: copy a base entity into the delta the first time it is touched.

    /// Make `id` editable in the delta, copying its record, run and attributes
    /// out of the base on the first edit. Answers whether the op is live.
    pub(crate) fn shadow_op(&mut self, base: &Store, id: OpId) -> bool {
        if self.store.owns_op(id) || self.store.shadows_op(id) {
            return self.store.op(id).is_some();
        }
        if self.erased_op(id) {
            return false;
        }
        let Some(instance) = base.op(id) else {
            return false;
        };
        let mut copy = instance.clone();
        let (operands, results, regions) = base.ports(id);
        let operands: Vec<u32> = operands
            .into_iter()
            .map(|raw| self.resolve(ValueId::from_number(raw)).number())
            .collect();
        let attrs = base.op_attrs(id).to_vec();
        let parent = base.op_parent(id);
        copy.run = crate::run::RunId::NONE;
        copy.attrs = crate::run::AttrRunId::NONE;
        copy.attr_count = 0;
        copy.operand_count = 0;
        copy.result_count = 0;
        copy.region_count = 0;
        self.store.shadow_op(copy);
        self.store.set_ports(id, &operands, &results, &regions);
        self.store.set_op_attrs(id, attrs);
        self.store.set_op_parent(id, parent);
        true
    }

    pub(crate) fn shadow_value(&mut self, base: &Store, id: ValueId) -> bool {
        if self.store.owns_value(id) || self.store.shadows_value(id) {
            return self.store.value(id).is_some();
        }
        if self.erased_value(id) {
            return false;
        }
        let Some(value) = base.value(id) else {
            return false;
        };
        let (block, region) = (base.value_block(id), base.value_region(id));
        self.store.shadow_value(value.clone());
        self.store.set_value_block(id, block);
        self.store.set_value_region(id, region);
        true
    }

    pub(crate) fn shadow_block(&mut self, base: &Store, id: BlockId) -> bool {
        if self.store.owns_block(id) || self.store.shadows_block(id) {
            return self.store.block(id).is_some();
        }
        if self.erased_block(id) {
            return false;
        }
        let Some(block) = base.block(id) else {
            return false;
        };
        let parent = base.block_parent(id);
        self.store.shadow_block(id, block.clone());
        self.store.set_block_parent(id, parent);
        true
    }

    pub(crate) fn shadow_region(&mut self, base: &Store, id: RegionId) -> bool {
        if self.store.owns_region(id) || self.store.shadows_region(id) {
            return self.store.region(id).is_some();
        }
        if self.erased_region(id) {
            return false;
        }
        let Some(region) = base.region(id) else {
            return false;
        };
        self.store.shadow_region(id, region.clone());
        true
    }

    /// Take `id` out of the visible IR. An op the overlay holds loses its
    /// storage; a base op is flagged, and its base slots stop counting.
    pub(crate) fn erase_op(&mut self, id: OpId) {
        if self.store.owns_op(id) || self.store.shadows_op(id) {
            self.store.unlink_operands(id);
            self.store.erase_op(id);
        }
        if !self.store.owns_op(id) {
            Self::set_flag(
                &mut self.erased_ops,
                &mut self.touched.erased_ops,
                id.index(),
            );
        }
    }

    pub(crate) fn erase_value(&mut self, id: ValueId) {
        if self.store.owns_value(id) || self.store.shadows_value(id) {
            self.store.erase_value(id);
        }
        if !self.store.owns_value(id) {
            Self::set_flag(
                &mut self.erased_values,
                &mut self.touched.erased_values,
                id.index(),
            );
        }
    }

    pub(crate) fn erase_block(&mut self, id: BlockId) {
        if self.store.owns_block(id) || self.store.shadows_block(id) {
            self.store.erase_block(id);
        }
        if !self.store.owns_block(id) {
            Self::set_flag(
                &mut self.erased_blocks,
                &mut self.touched.erased_blocks,
                id.index(),
            );
        }
    }

    pub(crate) fn erase_region(&mut self, id: RegionId) {
        if self.store.owns_region(id) || self.store.shadows_region(id) {
            self.store.erase_region(id);
        }
        if !self.store.owns_region(id) {
            Self::set_flag(
                &mut self.erased_regions,
                &mut self.touched.erased_regions,
                id.index(),
            );
        }
    }

    /// Every slot naming `old` names `new` from here on. The overlay's own
    /// slots are rewritten now; base slots are resolved on read and rewritten
    /// at commit. A slot recorded later may name `old` again, and a second
    /// replacement of `old` moves only those. Answers the ops whose operands
    /// changed.
    pub(crate) fn replace_value_uses(
        &mut self,
        base: &Store,
        old: ValueId,
        new: ValueId,
    ) -> Vec<OpId> {
        let new = self.resolve(new);
        assert_ne!(new, old, "a value cannot replace itself through a chain");
        let mut edited = self.store.move_uses(old, new);
        if self.resolve(old) != old {
            return edited;
        }
        // Base slots attributed to `old` by earlier replacements follow it.
        let mut moved = self.sources.remove(&old).unwrap_or_default();
        for &source in &moved {
            self.replaced[source.index()] = new.number();
        }
        if !self.store.owns_value(old) {
            edited.extend(
                base.use_entries(old)
                    .map(|entry| base.locate(entry).0)
                    .filter(|owner| !self.suppressed(*owner)),
            );
            if old.index() >= self.replaced.len() {
                self.replaced.resize(old.index() + 1, NO_ENTRY);
            }
            self.replaced[old.index()] = new.number();
            self.replaced_count += 1;
            self.touched.replaced.push(old.number());
            moved.push(old);
        }
        if !moved.is_empty() {
            self.sources.entry(new).or_default().append(&mut moved);
        }
        edited
    }
}

/// An overlay's edits, owned outright: no reference to the base it was built
/// over survives, only the identity a commit checks. The dense per-id tables
/// the overlay read through are folded to what they name, so a batch held
/// until commit costs what it edited, not the width of the module.
pub(crate) struct EditBatch {
    epoch: u32,
    base: usize,
    frontier: Frontier,
    store: Store,
    erased_ops: Vec<u32>,
    erased_values: Vec<u32>,
    erased_blocks: Vec<u32>,
    erased_regions: Vec<u32>,
    /// `(base value, replacement)`.
    replaced: Vec<(u32, u32)>,
    /// `(op, bump)` for every op whose version the overlay advanced.
    revision: Vec<(u32, u32)>,
    replaced_ops: Vec<(OpId, OpId)>,
    swaps: Vec<(BlockId, OpId, OpId)>,
}

fn sorted(mut ids: Vec<u32>) -> Vec<u32> {
    ids.sort_unstable();
    ids
}

impl EditBatch {
    pub(crate) fn new(epoch: u32, base: usize, delta: Delta) -> Self {
        let mut replaced_ops: Vec<(OpId, OpId)> = delta.replaced_ops.into_iter().collect();
        replaced_ops.sort_unstable();
        let mut store = delta.store;
        store.compact();
        let touched = delta.touched;
        EditBatch {
            epoch,
            base,
            frontier: delta.frontier,
            store,
            erased_ops: sorted(touched.erased_ops),
            erased_values: sorted(touched.erased_values),
            erased_blocks: sorted(touched.erased_blocks),
            erased_regions: sorted(touched.erased_regions),
            replaced: sorted(touched.replaced)
                .into_iter()
                .map(|idx| (idx, delta.replaced[idx as usize]))
                .collect(),
            revision: sorted(touched.revised)
                .into_iter()
                .map(|idx| (idx, delta.revision[idx as usize]))
                .collect(),
            replaced_ops,
            swaps: delta.swaps,
        }
    }

    pub(crate) fn revision(&self) -> &[(u32, u32)] {
        &self.revision
    }

    /// Bytes the batch holds while it waits for commit.
    pub(crate) fn bytes(&self) -> usize {
        self.store.bytes()
            + self.store.tables_bytes()
            + (self.erased_ops.capacity()
                + self.erased_values.capacity()
                + self.erased_blocks.capacity()
                + self.erased_regions.capacity())
                * size_of::<u32>()
            + (self.replaced.capacity() + self.revision.capacity()) * size_of::<(u32, u32)>()
            + self.replaced_ops.capacity() * size_of::<(OpId, OpId)>()
            + self.swaps.capacity() * size_of::<(BlockId, OpId, OpId)>()
    }

    /// The base ids this batch writes: the entities it copied, erased or
    /// replaced. Two batches of one commit may not share any.
    fn write_set(&self) -> WriteSet {
        let mut set = WriteSet::default();
        set.ops.extend(
            self.store
                .shadowed_ops()
                .into_iter()
                .map(|(id, _)| id.raw()),
        );
        set.ops.extend(self.erased_ops.iter().copied());
        set.values.extend(
            self.store
                .shadowed_values()
                .into_iter()
                .map(|(id, _)| id.number()),
        );
        set.values.extend(self.erased_values.iter().copied());
        set.values.extend(self.replaced.iter().map(|(old, _)| *old));
        set.blocks.extend(
            self.store
                .shadowed_blocks()
                .into_iter()
                .map(|(id, _)| id.number()),
        );
        set.blocks.extend(self.erased_blocks.iter().copied());
        set.regions.extend(
            self.store
                .shadowed_regions()
                .into_iter()
                .map(|(id, _)| id.number()),
        );
        set.regions.extend(self.erased_regions.iter().copied());
        set
    }
}

#[derive(Default)]
struct WriteSet {
    ops: HashSet<u32>,
    values: HashSet<u32>,
    blocks: HashSet<u32>,
    regions: HashSet<u32>,
}

impl WriteSet {
    /// Add `other`, or name the first id both hold.
    fn merge(&mut self, other: WriteSet) -> Result<(), String> {
        let kinds: [(&mut HashSet<u32>, HashSet<u32>, &str); 4] = [
            (&mut self.ops, other.ops, "op"),
            (&mut self.values, other.values, "value"),
            (&mut self.blocks, other.blocks, "block"),
            (&mut self.regions, other.regions, "region"),
        ];
        for (held, incoming, kind) in kinds {
            if let Some(id) = incoming.iter().filter(|id| held.contains(*id)).min() {
                return Err(format!("{kind} {id}"));
            }
            held.extend(incoming);
        }
        Ok(())
    }
}

/// The first base entity two of `batches` both write, if any.
pub(crate) fn conflict(batches: &[EditBatch]) -> Option<String> {
    let mut written = WriteSet::default();
    for (index, batch) in batches.iter().enumerate() {
        if let Err(entity) = written.merge(batch.write_set()) {
            return Some(format!(
                "batch {index} edits {entity}, which an earlier batch of the same epoch edits too"
            ));
        }
    }
    None
}

/// Where a batch's local ids land: each kind moves up by however many ids the
/// batches committed before it added. Base ids and sentinels do not move.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shift {
    frontier: Frontier,
    counts: Frontier,
    by: Frontier,
}

impl Shift {
    fn is_none(&self) -> bool {
        self.by == Frontier::default()
    }

    fn raw(raw: u32, frontier: u32, count: u32, by: u32) -> u32 {
        if raw >= frontier && raw - frontier < count {
            raw + by
        } else {
            raw
        }
    }

    pub(crate) fn op(&self, id: OpId) -> OpId {
        OpId::new(Self::raw(
            id.raw(),
            self.frontier.ops,
            self.counts.ops,
            self.by.ops,
        ))
    }

    pub(crate) fn value(&self, id: ValueId) -> ValueId {
        ValueId::from_number(Self::raw(
            id.number(),
            self.frontier.values,
            self.counts.values,
            self.by.values,
        ))
    }

    pub(crate) fn block(&self, id: BlockId) -> BlockId {
        BlockId::new(Self::raw(
            id.raw(),
            self.frontier.blocks,
            self.counts.blocks,
            self.by.blocks,
        ))
    }

    pub(crate) fn region(&self, id: RegionId) -> RegionId {
        RegionId::new(Self::raw(
            id.raw(),
            self.frontier.regions,
            self.counts.regions,
            self.by.regions,
        ))
    }

    fn parent(&self, parent: Option<Parent>) -> Option<Parent> {
        parent.map(|parent| match parent {
            Parent::Block(block) => Parent::Block(self.block(block)),
            Parent::Region(region) => Parent::Region(self.region(region)),
        })
    }

    pub(crate) fn attrs(&self, attrs: &mut [NamedAttribute]) {
        if self.is_none() {
            return;
        }
        for attr in attrs {
            self.attr(&mut attr.value);
        }
    }

    fn attr(&self, value: &mut AttributeValue) {
        match value {
            AttributeValue::Value(id) => *id = self.value(*id),
            AttributeValue::Block(id) => *id = self.block(*id),
            AttributeValue::Array(items) => {
                for item in items.iter_mut() {
                    self.attr(item);
                }
            }
            AttributeValue::Dict(dict) => {
                for item in dict.values_mut() {
                    self.attr(item);
                }
            }
            _ => {}
        }
    }
}

/// What a commit hands back: the next epoch's base, the version bumps of
/// every batch with their ids as committed, and the op replacements a
/// pipeline follows its roots through.
pub(crate) struct Committed {
    pub base: Frozen,
    pub revisions: Vec<(u32, u32)>,
    pub replaced_ops: Vec<(OpId, OpId)>,
}

/// Apply `batches` to `base`, which no reader may still hold, in the order
/// given. Every batch was finished over `base` as it stands; the first keeps
/// the ids it minted and each later one moves up past what the earlier ones
/// added, which is the numbering one overlay running them in turn would have
/// given. Check [`conflict`] first: this applies without looking.
pub(crate) fn commit_epoch(base: Frozen, batches: Vec<EditBatch>) -> Committed {
    let identity = base.identity();
    let mut store = Arc::try_unwrap(base.0)
        .unwrap_or_else(|_| panic!("commit while a reader still holds the base"));
    let frontier = store.frontier();
    let mut revisions = Vec::new();
    let mut replaced_ops = Vec::new();
    for batch in batches {
        assert_eq!(
            batch.base, identity,
            "a batch commits to the base it edited"
        );
        assert_eq!(
            batch.frontier, frontier,
            "a batch commits to the base as it stood when the overlay opened"
        );
        let now = store.frontier();
        let shift = Shift {
            frontier,
            counts: Frontier {
                ops: batch.store.local_ops().len() as u32,
                values: batch.store.local_values().len() as u32,
                blocks: batch.store.local_blocks().len() as u32,
                regions: batch.store.local_regions().len() as u32,
            },
            by: Frontier {
                ops: now.ops - frontier.ops,
                values: now.values - frontier.values,
                blocks: now.blocks - frontier.blocks,
                regions: now.regions - frontier.regions,
            },
        };
        revisions.extend(
            batch
                .revision
                .iter()
                .map(|(op, bump)| (shift.op(OpId::new(*op)).raw(), *bump)),
        );
        replaced_ops.extend(
            batch
                .replaced_ops
                .iter()
                .map(|(old, new)| (shift.op(*old), shift.op(*new))),
        );
        apply(&mut store, batch, &shift);
    }
    store.recycle();
    Committed {
        base: Frozen(Arc::new(store)),
        revisions,
        replaced_ops,
    }
}

fn apply(base: &mut Store, batch: EditBatch, shift: &Shift) {
    for idx in &batch.erased_ops {
        let id = OpId::new(*idx);
        base.unlink_operands(id);
        base.erase_op(id);
    }
    for idx in &batch.erased_values {
        base.erase_value(ValueId::from_number(*idx));
    }
    for idx in &batch.erased_blocks {
        base.erase_block(BlockId::new(*idx));
    }
    for idx in &batch.erased_regions {
        base.erase_region(RegionId::new(*idx));
    }

    // Base slots move to their replacement while the base lists hold nothing
    // but surviving base slots: a slot the overlay recorded naming a replaced
    // value again is meant to.
    for (old, target) in &batch.replaced {
        base.move_uses(
            ValueId::from_number(*old),
            shift.value(ValueId::from_number(*target)),
        );
    }

    let store = &batch.store;
    for (id, _) in store.shadowed_ops() {
        write_op(base, store, id, id, shift);
    }
    for (id, _) in store.shadowed_values() {
        let mut value = store.value(id).expect("shadow").clone();
        value.shift(shift);
        *base.value_mut(id).expect("shadowed value is live") = value;
        base.set_value_block(id, store.value_block(id).map(|b| shift.block(b)));
        base.set_value_region(id, store.value_region(id).map(|r| shift.region(r)));
    }
    for (id, _) in store.shadowed_blocks() {
        let mut block = store.block(id).expect("shadow").clone();
        block.shift(shift);
        *base.block_mut(id).expect("shadowed block is live") = block;
        base.set_block_parent(id, store.block_parent(id).map(|r| shift.region(r)));
    }
    for (id, _) in store.shadowed_regions() {
        let mut region = store.region(id).expect("shadow").clone();
        region.shift(shift);
        *base.region_mut(id).expect("shadowed region is live") = region;
    }

    for id in store.local_values() {
        let Some(value) = store.value(id) else {
            base.skip_value();
            continue;
        };
        let mut value = value.clone();
        value.shift(shift);
        let got = base.insert_value(|_| value);
        assert_eq!(got, shift.value(id), "a committed value keeps its id");
        base.set_value_block(got, store.value_block(id).map(|b| shift.block(b)));
        base.set_value_region(got, store.value_region(id).map(|r| shift.region(r)));
    }
    for id in store.local_blocks() {
        let Some(block) = store.block(id) else {
            base.skip_block();
            continue;
        };
        let mut block = block.clone();
        block.shift(shift);
        let got = base.insert_block(block);
        assert_eq!(got, shift.block(id), "a committed block keeps its id");
        base.set_block_parent(got, store.block_parent(id).map(|r| shift.region(r)));
        base.stamp_block(got, batch.epoch);
    }
    for id in store.local_regions() {
        let Some(region) = store.region(id) else {
            base.skip_region();
            continue;
        };
        let mut region = region.clone();
        region.shift(shift);
        let got = base.insert_region(region);
        assert_eq!(got, shift.region(id), "a committed region keeps its id");
        base.stamp_region(got, batch.epoch);
    }
    for id in store.local_ops() {
        let Some(instance) = store.op(id) else {
            base.skip_op();
            continue;
        };
        let mut blank = instance.clone();
        blank.id = shift.op(id);
        blank.run = crate::run::RunId::NONE;
        blank.attrs = crate::run::AttrRunId::NONE;
        blank.attr_count = 0;
        blank.operand_count = 0;
        blank.result_count = 0;
        blank.region_count = 0;
        let got = base.insert_op(|_| blank);
        assert_eq!(got, shift.op(id), "a committed op keeps its id");
        write_op(base, store, id, got, shift);
        base.stamp_op(got, batch.epoch);
    }
    for (block, old, new) in &batch.swaps {
        let block = base.block_mut(*block).expect("a swapped block is live");
        for op in block.operations_mut() {
            if *op == *old {
                *op = shift.op(*new);
            }
        }
    }
}

/// Give the base op `into` the ports, attributes and parent the delta copy
/// `from` has, with every id moved by `shift`.
fn write_op(base: &mut Store, store: &Store, from: OpId, into: OpId, shift: &Shift) {
    let (mut operands, mut results, mut regions) = store.ports(from);
    if !shift.is_none() {
        for raw in operands.iter_mut().chain(results.iter_mut()) {
            *raw = shift.value(ValueId::from_number(*raw)).number();
        }
        for raw in regions.iter_mut() {
            *raw = shift.region(RegionId::new(*raw)).raw();
        }
    }
    base.set_ports(into, &operands, &results, &regions);
    let mut attrs = store.op_attrs(from).to_vec();
    shift.attrs(&mut attrs);
    base.set_op_attrs(into, attrs);
    base.set_op_parent(into, shift.parent(store.op_parent(from)));
}
