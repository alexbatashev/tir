//! Memory state erasure.
//!
//! The inverse of [threading](super::thread_state): every port a chain grew is
//! dropped and the states it named go with it, leaving the implicit memory order
//! the IR started from. State exists only between the two passes, so a function
//! that carries none is left alone.

use crate::analysis::AnalysisManager;
use crate::analysis::slots::collect_slots;
use crate::builtin::{EntryStateOp, FuncOp, StateType};
use crate::{
    BlockId, Context, OpId, OperationRef, Pass, PassError, PassTarget, RegionId, Rewriter, TypeId,
};

#[derive(Default)]
pub struct EraseStatePass;

impl EraseStatePass {
    pub fn new() -> Self {
        Self
    }
}

crate::register_pass!(EraseStatePass, "erase-state");

impl Pass for EraseStatePass {
    fn name(&self) -> &'static str {
        "erase-state"
    }

    fn target(&self) -> PassTarget {
        PassTarget::operation::<FuncOp>()
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<(), PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(());
        }
        let Some(&body) = op.op().regions.first() else {
            return Ok(());
        };
        let state = StateType::new(context);
        let blocks = blocks_under(context, body);

        // The states an op consumes go first, so nothing reads a definition by the
        // time the definition is dropped.
        for &block in &blocks {
            for op_id in context.get_block(block).op_ids() {
                drop_trailing_state_operands(context, op_id, state);
            }
        }

        let mut roots = Vec::new();
        for &block in &blocks {
            for op_id in context.get_block(block).op_ids() {
                if context.get_op(op_id).is::<EntryStateOp>() {
                    roots.push((block, op_id));
                    continue;
                }
                drop_trailing_state_results(context, op_id, state);
            }
            drop_trailing_state_arguments(context, block, state);
        }

        for (block, op_id) in roots {
            let block = context.get_block(block);
            rewriter.erase_op(&OperationRef::new(context.get_op(op_id), Some(block), None))?;
        }

        sweep_dead_slots(context, rewriter, &blocks)
    }
}

/// Erase the slots nothing reads. A slot whose address never leaves the function
/// is a memory of its own, so with no read left there is nothing to observe what
/// its stores left behind and they die with the allocation.
///
/// Erasing state is what makes the sweep possible: while the chain is threaded,
/// the stores define the states their successors observe.
fn sweep_dead_slots(
    context: &Context,
    rewriter: &mut Rewriter,
    blocks: &[BlockId],
) -> Result<(), PassError> {
    let ops = blocks
        .iter()
        .flat_map(|&block| context.get_block(block).op_ids())
        .collect::<Vec<_>>();
    for slot in collect_slots(context, &ops).into_values() {
        let Some(alloca) = slot.alloca else { continue };
        if slot.escapes || !slot.loads.is_empty() {
            continue;
        }
        for op_id in slot.stores.into_iter().chain([alloca]) {
            let block = context.parent_block(op_id).map(|id| context.get_block(id));
            rewriter.erase_op(&OperationRef::new(context.get_op(op_id), block, None))?;
        }
    }
    Ok(())
}

fn drop_trailing_state_operands(context: &Context, op: OpId, state: TypeId) {
    while context
        .get_op(op)
        .operands
        .last()
        .is_some_and(|&operand| context.get_value(operand).ty() == state)
    {
        context.pop_operand(op);
    }
}

fn drop_trailing_state_results(context: &Context, op: OpId, state: TypeId) {
    while context
        .get_op(op)
        .results
        .last()
        .is_some_and(|&result| context.get_value(result).ty() == state)
    {
        context.pop_result(op);
    }
}

fn drop_trailing_state_arguments(context: &Context, block: BlockId, state: TypeId) {
    while context
        .get_block(block)
        .arguments()
        .last()
        .is_some_and(|argument| argument.ty() == state)
    {
        let index = context.get_block(block).arguments().len() - 1;
        context.remove_block_argument(block, index);
    }
}

/// Every block in a region tree, innermost first.
fn blocks_under(context: &Context, region: RegionId) -> Vec<BlockId> {
    let mut blocks = Vec::new();
    for block in context.get_region(region).iter(context.clone()) {
        for op_id in block.op_ids() {
            for &nested in &context.get_op(op_id).regions {
                blocks.extend(blocks_under(context, nested));
            }
        }
        blocks.push(block.id());
    }
    blocks
}
