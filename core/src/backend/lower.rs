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
    use tir::builtin::{FuncOp, MakeTupleOp, ReturnOp, TupleGetOp, TupleType};

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
        let mut tuple_extracts = Vec::new();
        let mut arguments = Vec::new();
        for argument in func.body().arguments() {
            let ty = context.get_type_data(argument.ty());
            let Some(tuple) = (ty.as_ref() as &dyn std::any::Any).downcast_ref::<TupleType>()
            else {
                arguments.push(AttributeValue::Register(RegisterAttr::Virtual {
                    id: argument.id().number(),
                    class: Some(argument_class(argument.ty())?),
                }));
                continue;
            };

            let element_types = tuple.elements(context);
            let mut elements = vec![None; element_types.len()];
            for usage in context.value_uses(argument.id()) {
                if usage.operand_index() != Some(0) {
                    continue;
                }
                let extract_instance = context.get_op(usage.op());
                let Some(extract) = extract_instance.clone().as_op::<TupleGetOp>() else {
                    continue;
                };
                let Some(element) = elements.get_mut(extract.index()) else {
                    return Err(PassError::InvalidRuleSet(
                        "tuple_get index is out of bounds".to_string(),
                    ));
                };
                match *element {
                    Some(canonical) => {
                        context.replace_value_uses(extract.result(), canonical);
                    }
                    None => *element = Some(extract.result()),
                }
                let block = context.parent_block(usage.op()).ok_or_else(|| {
                    PassError::InvalidRuleSet("tuple_get has no parent block".to_string())
                })?;
                tuple_extracts.push((usage.op(), block));
            }

            let group = element_types
                .into_iter()
                .zip(elements)
                .map(|(ty, value)| {
                    let value = value.unwrap_or_else(|| context.create_value(ty, None).id());
                    Ok(AttributeValue::Register(RegisterAttr::Virtual {
                        id: value.number(),
                        class: Some(argument_class(ty)?),
                    }))
                })
                .collect::<Result<Vec<_>, PassError>>()?;
            arguments.push(AttributeValue::Array(group));
        }
        let symbol = super::SymbolOpBuilder::new(context)
            .body(op.op().regions[0])
            .attr("name", AttributeValue::Str(name))
            .attr("arg_regs", AttributeValue::Array(arguments))
            .build();
        rewriter.replace_op(op, &symbol)?;
        for (extract, block) in tuple_extracts {
            rewriter.erase_op(&OperationRef::new(
                context.get_op(extract),
                Some(context.get_block(block)),
                None,
            ))?;
        }
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
