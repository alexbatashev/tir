//! A pass that applies a list of per-op lowering functions, reusing the same
//! [`OpLowering`] shape instruction selection uses for its structural
//! lowerings. Targets contribute lowerings for the virtual ops that survive
//! earlier stages (wide constants before register allocation; `vret`/`vbr`
//! after).

use tir::{
    AnalysisManager, Context, OperationRef, Pass, PassError, PassTarget, PreservedAnalyses,
    Rewriter, TypeId,
};

use crate::backend::isel::OpLowering;
use crate::backend::regalloc::RegClassId;

pub fn lower_function_and_return(
    context: &Context,
    op: &OperationRef,
    rewriter: &mut Rewriter,
    argument_class: impl Fn(TypeId) -> Result<RegClassId, PassError>,
) -> Result<bool, PassError> {
    use tir::Operation;
    use tir::attributes::{AttributeValue, RegisterAttr};
    use tir::builtin::{FuncOp, MakeTupleOp, ReturnOp, TupleType};

    if let Some(func) = op.as_op::<FuncOp>() {
        let body = func.body();
        if body
            .op_ids()
            .last()
            .is_none_or(|id| !context.get_op(*id).is::<super::SymbolEndOp>())
        {
            tir::IRBuilder::new(body).insert(super::SymbolEndOpBuilder::new(context).build());
        }
        let name = func
            .attributes()
            .iter()
            .find_map(|attr| match &attr.value {
                AttributeValue::Str(name) if attr.name == "sym_name" => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string());
        let arguments = func
            .body()
            .arguments()
            .iter()
            .map(|argument| {
                Ok(AttributeValue::Register(RegisterAttr::Virtual {
                    id: argument.id().number(),
                    class: Some(argument_class(argument.ty())?),
                }))
            })
            .collect::<Result<Vec<_>, PassError>>()?;
        let symbol = super::SymbolOpBuilder::new(context)
            .body(op.op().regions[0])
            .attr("name", AttributeValue::Str(name))
            .attr("arg_regs", AttributeValue::Array(arguments))
            .build();
        rewriter.replace_op(op, &symbol)?;
        return Ok(true);
    }

    if let Some(ret) = op.as_op::<ReturnOp>() {
        let mut tuple_source = None;
        let values = match ret.operands().first().copied() {
            None => Vec::new(),
            Some(value) => {
                let ty = context.get_type_data(context.get_value(value).ty());
                if (ty.as_ref() as &dyn std::any::Any)
                    .downcast_ref::<TupleType>()
                    .is_none()
                {
                    vec![value]
                } else {
                    let defining_op = context.get_value(value).defining_op().ok_or_else(|| {
                        PassError::InvalidRuleSet(
                            "returned tuple has no defining operation".to_string(),
                        )
                    })?;
                    let tuple = context
                        .get_op(defining_op)
                        .clone()
                        .as_op::<MakeTupleOp>()
                        .ok_or_else(|| {
                            PassError::InvalidRuleSet(
                                "returned tuple must be assembled from scalar values".to_string(),
                            )
                        })?;
                    tuple_source = Some((value, defining_op));
                    tuple.operands().to_vec()
                }
            }
        };
        rewriter.replace_op(
            op,
            &super::VirtualReturnOpBuilder::new(context)
                .values(values)
                .build(),
        )?;
        if let Some((value, defining_op)) = tuple_source
            && context.value_uses(value).is_empty()
        {
            let block = context.parent_block(defining_op).ok_or_else(|| {
                PassError::InvalidRuleSet("tuple construction has no parent block".to_string())
            })?;
            rewriter.erase_op(&OperationRef::new(
                context.get_op(defining_op),
                Some(context.get_block(block)),
                None,
            ))?;
        }
        return Ok(true);
    }

    Ok(false)
}

pub struct OpLoweringPass {
    name: &'static str,
    lowerings: Vec<OpLowering>,
}

impl OpLoweringPass {
    pub fn new(name: &'static str, lowerings: Vec<OpLowering>) -> Self {
        Self { name, lowerings }
    }
}

impl Pass for OpLoweringPass {
    fn name(&self) -> &'static str {
        self.name
    }

    fn target(&self) -> PassTarget {
        PassTarget::Any
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<PreservedAnalyses, PassError> {
        for lowering in &self.lowerings {
            if lowering(context, op, rewriter)? {
                return Ok(PreservedAnalyses::none());
            }
        }
        Ok(PreservedAnalyses::all())
    }
}
