//! Canonicalize a function through the sea: raise, view, saturate, commit,
//! destruct.
//!
//! The validation harness for the red layer, as [`super::sea_roundtrip`] is for
//! the green one. It is not the optimizer yet — `instcombine` still runs — so
//! what it proves is that a body rebuilt from an e-graph extraction is valid IR
//! that means the same thing, on every program the end-to-end suite compiles. A
//! function whose shape does not raise, or whose commit does not stage, is left
//! exactly as it was.

use crate::{
    AnalysisManager, Context, OperationRef, Pass, PassError, PassTarget, PreservedAnalyses,
    Rewriter, builtin::FuncOp, sea,
};

#[derive(Default)]
pub struct SeaCanonicalizePass;

impl SeaCanonicalizePass {
    pub fn new() -> Self {
        Self
    }
}

crate::register_pass!(SeaCanonicalizePass, "sea-canonicalize");

impl Pass for SeaCanonicalizePass {
    fn name(&self) -> &'static str {
        "sea-canonicalize"
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
        let Ok(mut raised) = sea::raise_function(context, &func) else {
            return Ok(PreservedAnalyses::all());
        };
        raised
            .graph
            .verify()
            .map_err(|error| PassError::InvalidRuleSet(error.to_string()))?;

        let body = raised.graph.subregions(raised.lambda)[0];
        let mut views = sea::ViewCache::default();
        let view = views.view(context, &raised.graph, body);
        view.saturate(context);
        // A commit that cannot stage leaves a region no node owns, so the graph
        // it staged into is unusable and the function keeps the IR it has.
        match sea::commit(&mut raised.graph, view, raised.lambda) {
            Ok(Some(lambda)) => raised.lambda = lambda,
            Ok(None) => {}
            Err(_) => return Ok(PreservedAnalyses::all()),
        }
        raised
            .graph
            .verify()
            .map_err(|error| PassError::InvalidRuleSet(error.to_string()))?;

        let block = func.body();
        for id in block.op_ids() {
            super::sea_roundtrip::erase(context, &block, id);
        }
        sea::lower_function(context, &raised, &block)
            .map_err(|error| PassError::InvalidRuleSet(error.to_string()))?;
        Ok(PreservedAnalyses::none())
    }
}
