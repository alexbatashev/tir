//! Promotion for functions that still contain unstructured control flow (the
//! `goto` remainder a frontend could not raise to `scf`). Dominance replaces the
//! region tree here, so only a single store reaching every load is forwarded.

use std::collections::{BTreeSet, HashMap};

use crate::analysis::DominatorTree;
use crate::graph::{Dag, NodeId};
use crate::{BlockId, Context, OpId, OperationRef, PassError, Rewriter};

use super::{SlotState, collect_slots, load_result, store_value, values_agree_on_type};

pub(super) fn run(
    context: &Context,
    rewriter: &mut Rewriter,
    dom_tree: &DominatorTree,
) -> Result<(), PassError> {
    let layout = OpLayout::collect(context, dom_tree);
    let slots = collect_slots(context, &layout.op_ids(context));

    let mut erase: BTreeSet<OpId> = BTreeSet::new();
    for slot in slots.values() {
        if !is_promotable(context, slot, &layout, dom_tree) {
            continue;
        }

        if let Some(store) = slot.stores.first() {
            let value = store_value(context, *store);
            for load in &slot.loads {
                context.replace_value_uses(load_result(context, *load), value);
                erase.insert(*load);
            }
            erase.insert(*store);
        }
        if let Some(alloca) = slot.alloca {
            erase.insert(alloca);
        }
    }

    for op_id in erase {
        if !context.has_operation(op_id) {
            continue;
        }
        let block = layout.block_of(op_id).map(|id| context.get_block(id));
        let target = OperationRef::new(context.get_op(op_id), block, None);
        rewriter.erase_op(&target)?;
    }

    Ok(())
}

/// Where every operation lives, so dominance can be lifted to operations:
/// within a block, program order decides; across blocks, the dominator tree does.
struct OpLayout {
    position: HashMap<OpId, (BlockId, usize)>,
    blocks: Vec<BlockId>,
}

impl OpLayout {
    fn collect(context: &Context, dom_tree: &DominatorTree) -> Self {
        let mut blocks: Vec<BlockId> = (0..dom_tree.len())
            .map(NodeId::from_index)
            .filter_map(|node| dom_tree.block(node))
            .collect();
        blocks.sort_by_key(BlockId::number);

        let mut position = HashMap::new();
        for &block_id in &blocks {
            for (index, op_id) in context.get_block(block_id).op_ids().into_iter().enumerate() {
                position.insert(op_id, (block_id, index));
            }
        }

        Self { position, blocks }
    }

    fn block_of(&self, op: OpId) -> Option<BlockId> {
        self.position.get(&op).map(|(block, _)| *block)
    }

    /// Whether the operation `a` dominates `b`, reflexively.
    fn dominates(&self, dom_tree: &DominatorTree, a: OpId, b: OpId) -> bool {
        let (Some(&(a_block, a_index)), Some(&(b_block, b_index))) =
            (self.position.get(&a), self.position.get(&b))
        else {
            return false;
        };
        if a_block == b_block {
            a_index <= b_index
        } else {
            dom_tree.dominates(a_block, b_block)
        }
    }

    /// Every operation in dominator-tree block order.
    fn op_ids(&self, context: &Context) -> Vec<OpId> {
        self.blocks
            .iter()
            .flat_map(|&block_id| context.get_block(block_id).op_ids())
            .collect()
    }
}

/// A slot promotes only when its single (or absent) store is the unambiguous
/// definition reaching every load — exactly the question dominance answers.
fn is_promotable(
    context: &Context,
    slot: &SlotState,
    layout: &OpLayout,
    dom_tree: &DominatorTree,
) -> bool {
    if slot.escapes || slot.alloca.is_none() || slot.stores.len() > 1 {
        return false;
    }
    if !values_agree_on_type(context, slot) {
        return false;
    }

    match slot.stores.first() {
        // A lone store dominating every load gives each load a single reaching
        // value; with no other store there is nothing to merge, so no phi is
        // needed and forwarding is sound.
        Some(store) => slot
            .loads
            .iter()
            .all(|load| layout.dominates(dom_tree, *store, *load)),
        // No store: a load would read an undefined value, so only a store-less,
        // load-less (dead) slot is promotable.
        None => slot.loads.is_empty(),
    }
}
