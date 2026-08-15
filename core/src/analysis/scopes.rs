//! Loop token scopes: the edges a structured loop is left through.
//!
//! A loop body binds a token naming its iteration, and an `scf.break`/`scf.continue`
//! names that token to leave through it. A transform growing a carried port has to
//! feed every such edge, so it tracks the scopes it is walking inside.

use std::sync::Arc;

use crate::builtin::TokenType;
use crate::{Block, Context, OpId, OpInstance, RegionId, ValueId, scf};

/// The loop body's token scope: the value its `scf.break`/`scf.continue` name.
pub fn loop_scope(context: &Context, body: RegionId) -> Option<ValueId> {
    let token = TokenType::new(context);
    let mut blocks = context.get_region(body).iter(context.clone());
    let block = blocks.next()?;
    blocks.next().is_none().then_some(())?;
    block
        .arguments()
        .iter()
        .find(|argument| argument.ty() == token)
        .map(|argument| argument.id())
}

/// The scope `op` leaves through, if it is an exit at all.
pub fn exit_scope(op: &Arc<OpInstance>) -> Option<ValueId> {
    (op.is::<scf::BreakOp>() || op.is::<scf::ContinueOp>()).then(|| op.operands[0])
}

/// The scope `region`'s terminator leaves through, if it ends in an exit at all.
pub fn region_exit(context: &Context, region: RegionId) -> Option<ValueId> {
    let block = context.get_region(region).iter(context.clone()).next()?;
    exit_scope(&context.get_op(*block.op_ids().last()?))
}

/// What a structured region's terminator does with control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionExit {
    /// Falls out of the region, carrying its operands to whatever follows it.
    Yield,
    /// Leaves the loop naming its scope.
    Break,
    /// Starts the next iteration of the loop naming its scope.
    Continue,
}

/// How `op` leaves the region it terminates, or `None` where it does not leave one
/// — a `return`, or a terminator emission has already lowered.
pub fn region_exit_kind(op: &Arc<OpInstance>) -> Option<RegionExit> {
    if op.is::<scf::BreakOp>() {
        Some(RegionExit::Break)
    } else if op.is::<scf::ContinueOp>() {
        Some(RegionExit::Continue)
    } else if op.is::<scf::YieldOp>() {
        Some(RegionExit::Yield)
    } else {
        None
    }
}

/// Every edge feeding a loop's carried ports from inside `body`: the exits leaving
/// its scope, in the order the body tree holds them, and the body's own terminator
/// when it is not one of them. What a transform growing a port has to carry a value
/// on, and the order it reads them in.
pub fn port_edges(context: &Context, body: RegionId) -> Vec<OpId> {
    let Some(block) = context.get_region(body).iter(context.clone()).next() else {
        return Vec::new();
    };
    let mut edges = match loop_scope(context, body) {
        Some(scope) => scope_exits(context, &block, scope),
        None => Vec::new(),
    };
    let Some(&terminator) = block.op_ids().last() else {
        return edges;
    };
    if !edges.contains(&terminator) {
        edges.push(terminator);
    }
    edges
}

/// The exits leaving `scope` from `block`'s region tree, outermost first.
fn scope_exits(context: &Context, block: &Arc<Block>, scope: ValueId) -> Vec<OpId> {
    let mut exits = Vec::new();
    for op_id in block.op_ids() {
        let op = context.get_op(op_id);
        if exit_scope(&op) == Some(scope) {
            exits.push(op_id);
        }
        for &region in &op.regions {
            for nested in context.get_region(region).iter(context.clone()) {
                exits.extend(scope_exits(context, &nested, scope));
            }
        }
    }
    exits
}

/// The values an edge carries into the ports it feeds: an exit's operands past its
/// scope token, any other terminator's operands.
pub fn carried_operands(op: &Arc<OpInstance>) -> &[ValueId] {
    match exit_scope(op) {
        Some(_) => &op.operands[1..],
        None => &op.operands,
    }
}

/// The scopes every exit inside `op`'s regions leaves through.
pub fn nested_exit_scopes(context: &Context, op: &Arc<OpInstance>) -> Vec<ValueId> {
    let mut scopes = Vec::new();
    for &region in &op.regions {
        for block in context.get_region(region).iter(context.clone()) {
            for op_id in block.op_ids() {
                let nested = context.get_op(op_id);
                scopes.extend(exit_scope(&nested));
                scopes.extend(nested_exit_scopes(context, &nested));
            }
        }
    }
    scopes
}
