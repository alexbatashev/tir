//! A pass that applies a list of per-op lowering functions, reusing the same
//! [`OpLowering`] shape instruction selection uses for its structural
//! lowerings. Targets contribute lowerings for the virtual ops that survive
//! earlier stages (wide constants before register allocation; `vret`/`vbr`
//! after).

use std::collections::HashMap;

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
            .is_none_or(|id| context.get_op(*id).name != super::SymbolEndOp::name())
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
        if let Some(value) = ret.operands().first().copied() {
            let ty = context.get_type_data(context.get_value(value).ty());
            if (ty.as_ref() as &dyn std::any::Any)
                .downcast_ref::<TupleType>()
                .is_some()
            {
                let defining_op = context.get_value(value).defining_op().ok_or_else(|| {
                    PassError::InvalidRuleSet(
                        "returned tuple must be produced by make_tuple".to_string(),
                    )
                })?;
                let tuple_instance = context.get_op(defining_op);
                let tuple = tuple_instance
                    .clone()
                    .as_op::<MakeTupleOp>()
                    .ok_or_else(|| {
                        PassError::InvalidRuleSet(
                            "returned tuple must be produced by make_tuple".to_string(),
                        )
                    })?;
                let uses = context.value_uses(value);
                if uses.len() != 1 || uses[0].op() != ret.id() {
                    return Err(PassError::InvalidRuleSet(
                        "returned tuple must only be consumed by return".to_string(),
                    ));
                }

                let mut next_slot = HashMap::new();
                for &element in tuple.operands() {
                    let kind = crate::backend::abi::value_kind(context, element);
                    let slot = next_slot.entry(kind).or_insert(0usize);
                    let marker = super::VirtualReturnValueOpBuilder::new(context)
                        .value(element)
                        .attr("slot", AttributeValue::UInt(*slot as u64))
                        .build();
                    *slot += 1;
                    rewriter.insert_op_before(op, &marker)?;
                }

                rewriter.replace_op(op, &super::VirtualReturnOpBuilder::new(context).build())?;
                let tuple_block = context.parent_block(defining_op).ok_or_else(|| {
                    PassError::InvalidRuleSet("make_tuple has no parent block".to_string())
                })?;
                let tuple_ref =
                    OperationRef::new(tuple_instance, Some(context.get_block(tuple_block)), None);
                rewriter.erase_op(&tuple_ref)?;
                return Ok(true);
            }
        }

        let mut builder = super::VirtualReturnOpBuilder::new(context);
        if let Some(value) = ret.operands().first().copied() {
            builder = builder.value(value);
        }
        rewriter.replace_op(op, &builder.build())?;
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
