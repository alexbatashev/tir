use std::sync::Arc;

use crate::analysis::{AnalysisManager, PreservedAnalyses};
use crate::attributes::AttributeValue;
use crate::builtin::{DeclareOp, IntegerType, ModuleOp, ops as b};
use crate::ptr::{MemcpyOp, PtrType};
use crate::{Context, OpInstance, Operation, OperationRef, Pass, PassError, PassTarget, Rewriter};

pub struct LowerMemoryIntrinsicsPass;

impl LowerMemoryIntrinsicsPass {
    pub fn new() -> Self {
        Self
    }

    fn descendants(context: &Context, root: &Arc<OpInstance>) -> Vec<OperationRef> {
        fn visit(context: &Context, operation: &Arc<OpInstance>, result: &mut Vec<OperationRef>) {
            for region in &operation.regions {
                for block in context.get_region(*region).iter(context.clone()) {
                    for operation in block.op_ids() {
                        let operation = context.get_op(operation);
                        result.push(OperationRef::new(
                            operation.clone(),
                            Some(block.clone()),
                            None,
                        ));
                        visit(context, &operation, result);
                    }
                }
            }
        }

        let mut result = Vec::new();
        visit(context, root, &mut result);
        result
    }
}

impl Default for LowerMemoryIntrinsicsPass {
    fn default() -> Self {
        Self::new()
    }
}

crate::register_pass!(LowerMemoryIntrinsicsPass, "lower-memory-intrinsics");

impl Pass for LowerMemoryIntrinsicsPass {
    fn name(&self) -> &'static str {
        "lower-memory-intrinsics"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Operation(ModuleOp::name())
    }

    fn run(
        &mut self,
        operation: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<PreservedAnalyses, PassError> {
        let Some(module) = operation.as_op::<ModuleOp>() else {
            return Ok(PreservedAnalyses::all());
        };
        let copies: Vec<_> = Self::descendants(context, operation.op())
            .into_iter()
            .filter(|operation| operation.as_op::<MemcpyOp>().is_some())
            .collect();
        if copies.is_empty() {
            return Ok(PreservedAnalyses::all());
        }

        let pointer = PtrType::opaque(context);
        let size = IntegerType::new(context, 64);
        let declaration = module.body().op_ids().into_iter().any(|operation| {
            context
                .get_op(operation)
                .as_op::<DeclareOp>()
                .is_some_and(|declaration| declaration.sym_name() == "memcpy")
        });
        if !declaration {
            let declaration = b::declare_op(context, "memcpy", pointer, &[pointer, pointer, size]);
            module.body().insert(0, declaration.id());
        }

        for operation in copies {
            let copy = operation.as_op::<MemcpyOp>().unwrap();
            let call = b::CallOpBuilder::new(context)
                .args(copy.operands().to_vec())
                .attr("callee", AttributeValue::Str("memcpy".to_string()))
                .result_type(pointer)
                .build();
            rewriter.replace_op(&operation, &call)?;
        }

        Ok(PreservedAnalyses::none())
    }
}
