use crate::analysis::{AnalysisManager, PreservedAnalyses};
use crate::attributes::AttributeValue;
use crate::builtin::{DeclareOp, IntegerType, ModuleOp, ops as b};
use crate::ptr::{MemcpyOp, PtrType};
use crate::{Context, OpId, Operation, OperationRef, Pass, PassError, PassTarget, Rewriter};

pub struct LowerMemoryIntrinsicsPass;

impl LowerMemoryIntrinsicsPass {
    pub fn new() -> Self {
        Self
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
        PassTarget::operation::<MemcpyOp>()
    }

    fn run(
        &mut self,
        operation: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<PreservedAnalyses, PassError> {
        let copy = operation
            .as_op::<MemcpyOp>()
            .expect("pass target guarantees ptr.memcpy");
        let module = enclosing_module(context, operation.op().id).ok_or_else(|| {
            PassError::InvalidRuleSet("ptr.memcpy must be nested in a module".to_string())
        })?;

        let pointer = PtrType::opaque(context);
        let size = IntegerType::new(context, 64);
        let signature = [pointer, pointer, size];
        let declaration = module.body().op_ids().into_iter().find_map(|operation| {
            context
                .get_op(operation)
                .as_op::<DeclareOp>()
                .filter(|declaration| declaration.sym_name() == "memcpy")
        });
        if let Some(declaration) = declaration {
            if declaration.ret_type() != pointer || declaration.arg_types() != signature {
                return Err(PassError::InvalidRuleSet(
                    "existing memcpy declaration has an incompatible type".to_string(),
                ));
            }
        } else {
            let declaration = b::declare_op(context, "memcpy", pointer, &signature);
            module.body().insert(0, declaration.id());
        }

        let call = b::CallOpBuilder::new(context)
            .args(copy.operands().to_vec())
            .attr("callee", AttributeValue::Str("memcpy".to_string()))
            .result_type(pointer)
            .build();
        rewriter.replace_op(operation, &call)?;
        Ok(PreservedAnalyses::none())
    }
}

fn enclosing_module(context: &Context, mut operation: OpId) -> Option<ModuleOp> {
    loop {
        let block = context.parent_block(operation)?;
        let region = context.parent_region(block)?;
        operation = context.get_region(region).parent_op()?;
        if let Some(module) = context.get_op(operation).as_op::<ModuleOp>() {
            return Some(module);
        }
    }
}
