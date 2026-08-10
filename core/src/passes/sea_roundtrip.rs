//! Raise a function onto the sea and destruct it straight back.
//!
//! The identity transformation through the green layer: it is the correctness
//! harness for [`crate::sea`], not an optimization. A function whose shape does
//! not map is left exactly as it was — the raiser reports why rather than
//! producing something unsound.

use std::sync::Arc;

use crate::{
    AnalysisManager, Context, OpInstance, OperationRef, Pass, PassError, PassTarget,
    PreservedAnalyses, Rewriter, builtin::FuncOp, sea,
};

#[derive(Default)]
pub struct SeaRoundTripPass;

impl SeaRoundTripPass {
    pub fn new() -> Self {
        Self
    }
}

crate::register_pass!(SeaRoundTripPass, "sea-roundtrip");

impl Pass for SeaRoundTripPass {
    fn name(&self) -> &'static str {
        "sea-roundtrip"
    }

    fn target(&self) -> PassTarget {
        PassTarget::operation::<FuncOp>()
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        _rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<PreservedAnalyses, PassError> {
        let Some(func) = op.as_op::<FuncOp>() else {
            return Ok(PreservedAnalyses::all());
        };
        let Ok(raised) = sea::raise_function(context, &func) else {
            return Ok(PreservedAnalyses::all());
        };
        raised
            .graph
            .verify()
            .map_err(|error| PassError::InvalidRuleSet(error.to_string()))?;

        let block = func.body();
        for id in block.op_ids() {
            erase(context, &block, id);
        }
        sea::lower_function(context, &raised, &block)
            .map_err(|error| PassError::InvalidRuleSet(error.to_string()))?;
        Ok(PreservedAnalyses::none())
    }
}

/// Drop `id` and everything nested inside it from the live IR.
fn erase(context: &Context, block: &Arc<crate::Block>, id: crate::OpId) {
    let op = context.get_op(id);
    erase_nested(context, &op);
    block.remove_op(id);
    context.detach_op_uses(&op);
    context.remove_operation(id);
}

fn erase_nested(context: &Context, op: &Arc<OpInstance>) {
    for &region in &op.regions {
        for nested in context.get_region(region).iter(context.clone()) {
            for id in nested.op_ids() {
                erase(context, &nested, id);
            }
        }
    }
}
