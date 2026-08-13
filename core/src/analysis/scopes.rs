//! Loop token scopes: the edges a structured loop is left through.
//!
//! A loop body binds a token naming its iteration, and an `scf.break`/`scf.continue`
//! names that token to leave through it. A transform growing a carried port has to
//! feed every such edge, so it tracks the scopes it is walking inside.

use std::sync::Arc;

use crate::builtin::TokenType;
use crate::{Context, OpInstance, RegionId, ValueId, scf};

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

/// Whether `op` leaves one of the loops being walked.
pub fn exits_scope(op: &Arc<OpInstance>, scopes: &[ValueId]) -> bool {
    exit_scope(op).is_some_and(|scope| scopes.contains(&scope))
}

/// Whether anything inside `op`'s regions leaves one of those loops.
pub fn exits_scopes(context: &Context, op: &Arc<OpInstance>, scopes: &[ValueId]) -> bool {
    nested_exit_scopes(context, op)
        .iter()
        .any(|scope| scopes.contains(scope))
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
