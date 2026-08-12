//! Cleanup of the trivial blocks structured lowering leaves behind.
//!
//! Turning an `scf` region into a CFG splits every structured exit into its own
//! block, so a nest of loops and conditionals ends up threaded together by
//! blocks that do nothing but branch on. Two rewrites remove them, and nothing
//! more: an edge into a block whose only operation is an unconditional branch
//! is retargeted past it, and a block whose sole predecessor branches to it
//! unconditionally is absorbed into that predecessor.

use std::collections::{HashMap, HashSet};

use crate::analysis::DefUse;
use crate::builtin::ops as b;
use crate::builtin::{BranchOp, CondBranchOp};
use crate::{
    BlockId, Context, OperationRef, PassError, RegionId, Rewriter, Terminator, Value, ValueId,
};

/// Fold the jump-only blocks and single-predecessor chains in `region`, which
/// must be a function body (its first block is the entry).
pub(super) fn fold_trivial_blocks(
    context: &Context,
    rewriter: &mut Rewriter,
    region: RegionId,
) -> Result<(), PassError> {
    loop {
        let mut changed = thread_edges(context, rewriter, region)?;
        changed |= drop_unreachable_blocks(context, rewriter, region)?;
        changed |= merge_single_predecessor(context, rewriter, region)?;
        if !changed {
            return Ok(());
        }
    }
}

fn blocks(context: &Context, region: RegionId) -> Vec<BlockId> {
    context
        .get_region(region)
        .iter(context.clone())
        .map(|block| block.id())
        .collect()
}

fn terminator_of(context: &Context, block: BlockId) -> Option<crate::OpId> {
    context.get_block(block).op_ids().last().copied()
}

fn successors_of(context: &Context, block: BlockId) -> Vec<BlockId> {
    terminator_of(context, block)
        .and_then(|op_id| context.get_op(op_id).as_interface::<dyn Terminator>())
        .map(|terminator| terminator.successors())
        .unwrap_or_default()
}

/// The destination and forwarded arguments of a block whose only operation is
/// an unconditional branch, and whose parameters feed nothing but that branch.
/// A parameter read by a block the jump-only block dominates keeps that block
/// alive: folding the edges past it would strip the only definition of a value
/// still in use.
fn jump_only_target(
    context: &Context,
    def_use: &DefUse,
    block: BlockId,
) -> Option<(BlockId, Vec<ValueId>)> {
    let block = context.get_block(block);
    let [op_id] = block.op_ids()[..] else {
        return None;
    };
    let branch = context.get_op(op_id).as_op::<BranchOp>()?;
    let parameters_escape = block.arguments().iter().any(|parameter| {
        def_use
            .users_of(parameter.id().number())
            .iter()
            .any(|user| *user != op_id)
    });
    (!parameters_escape).then(|| (branch.dest(), branch.dest_args()))
}

/// Follow an edge through every jump-only block it lands in, rewriting the
/// forwarded arguments through each block's parameters. A value the chain
/// forwards but does not take from a parameter dominates every block on the
/// chain, hence also the predecessor the edge now leaves from.
fn resolve_edge(
    context: &Context,
    def_use: &DefUse,
    mut dest: BlockId,
    mut args: Vec<ValueId>,
) -> (BlockId, Vec<ValueId>) {
    let mut visited = HashSet::new();
    while visited.insert(dest) {
        let Some((next, next_args)) = jump_only_target(context, def_use, dest) else {
            break;
        };
        let substitution: HashMap<ValueId, ValueId> = context
            .get_block(dest)
            .arguments()
            .iter()
            .map(Value::id)
            .zip(args)
            .collect();
        args = next_args
            .into_iter()
            .map(|value| substitution.get(&value).copied().unwrap_or(value))
            .collect();
        dest = next;
    }
    (dest, args)
}

fn thread_edges(
    context: &Context,
    rewriter: &mut Rewriter,
    region: RegionId,
) -> Result<bool, PassError> {
    let root = context
        .get_region(region)
        .parent_op()
        .ok_or_else(|| PassError::InvalidRuleSet("cleanup needs an owned region".to_string()))?;
    // The index answers "does anything but this branch read the block's
    // parameters", so it has to describe the IR each rewrite leaves behind.
    let mut def_use = DefUse::new(context, root);
    let mut changed = false;
    for block_id in blocks(context, region) {
        let Some(op_id) = terminator_of(context, block_id) else {
            continue;
        };
        let op = context.get_op(op_id);
        let replacement = if let Some(branch) = op.clone().as_op::<BranchOp>() {
            let (dest, args) = resolve_edge(context, &def_use, branch.dest(), branch.dest_args());
            (dest != branch.dest())
                .then(|| Box::new(b::br(context, args, dest).build()) as Box<dyn crate::Operation>)
        } else if let Some(branch) = op.clone().as_op::<CondBranchOp>() {
            let (true_dest, true_args) =
                resolve_edge(context, &def_use, branch.true_dest(), branch.true_args());
            let (false_dest, false_args) =
                resolve_edge(context, &def_use, branch.false_dest(), branch.false_args());
            (true_dest != branch.true_dest() || false_dest != branch.false_dest()).then(|| {
                Box::new(
                    b::cond_br(
                        context,
                        branch.condition(),
                        true_args,
                        false_args,
                        true_dest,
                        false_dest,
                    )
                    .build(),
                ) as Box<dyn crate::Operation>
            })
        } else {
            None
        };
        if let Some(replacement) = replacement {
            let block = context.get_block(block_id);
            rewriter.replace_op(
                &OperationRef::new(op, Some(block), None),
                replacement.as_ref(),
            )?;
            def_use = DefUse::new(context, root);
            changed = true;
        }
    }
    Ok(changed)
}

/// How many edges enter each block.
fn entry_counts(context: &Context, region: RegionId) -> HashMap<BlockId, usize> {
    let mut counts = HashMap::new();
    for block_id in blocks(context, region) {
        for successor in successors_of(context, block_id) {
            *counts.entry(successor).or_default() += 1;
        }
    }
    counts
}

/// Absorb one block into its only predecessor, which must reach it through an
/// unconditional branch. Rewrites at most one pair: both the block list and the
/// edge counts it is chosen from go stale as soon as a block is removed.
fn merge_single_predecessor(
    context: &Context,
    rewriter: &mut Rewriter,
    region: RegionId,
) -> Result<bool, PassError> {
    let counts = entry_counts(context, region);
    let all = blocks(context, region);
    let entry = all.first().copied();
    for block_id in all {
        let Some(op_id) = terminator_of(context, block_id) else {
            continue;
        };
        let op = context.get_op(op_id);
        let Some(branch) = op.clone().as_op::<BranchOp>() else {
            continue;
        };
        let successor_id = branch.dest();
        if Some(successor_id) == entry
            || successor_id == block_id
            || counts.get(&successor_id) != Some(&1)
        {
            continue;
        }
        let block = context.get_block(block_id);
        let successor = context.get_block(successor_id);
        for (parameter, &argument) in successor.arguments().iter().zip(branch.dest_args().iter()) {
            context.replace_value_uses(parameter.id(), argument);
        }
        rewriter.erase_op(&OperationRef::new(op, Some(block.clone()), None))?;
        rewriter.splice_block(successor_id, block_id);
        context.get_region(region).remove_block(successor_id);
        return Ok(true);
    }
    Ok(false)
}

fn drop_unreachable_blocks(
    context: &Context,
    rewriter: &mut Rewriter,
    region: RegionId,
) -> Result<bool, PassError> {
    let all = blocks(context, region);
    let Some(&entry) = all.first() else {
        return Ok(false);
    };
    let mut reachable = HashSet::from([entry]);
    let mut worklist = vec![entry];
    while let Some(block_id) = worklist.pop() {
        for successor in successors_of(context, block_id) {
            if reachable.insert(successor) {
                worklist.push(successor);
            }
        }
    }
    let mut changed = false;
    for block_id in all {
        if reachable.contains(&block_id) {
            continue;
        }
        let block = context.get_block(block_id);
        for op_id in block.op_ids() {
            let op = context.get_op(op_id);
            rewriter.erase_op(&OperationRef::new(op, Some(block.clone()), None))?;
        }
        context.get_region(region).remove_block(block_id);
        changed = true;
    }
    Ok(changed)
}
