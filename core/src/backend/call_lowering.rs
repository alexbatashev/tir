use std::collections::{HashMap, HashSet};

use tir::attributes::{AttributeValue, RegisterAttr};
use tir::builtin::{
    CallIndirectResultOp, CallOp, IndirectCallOp, IndirectResultOp, MakeTupleOp, TupleGetOp,
    TupleType, UnitType,
};
use tir::{Context, OpId, Operation, OperationRef, PassError, Rewriter, ValueId};

use crate::backend::abi::{
    AbiInfo, GroupRollback, Overflow, ValueKind, exhaust_argument_registers, value_kind,
};
use crate::backend::liveness::PhysReg;

pub trait CallEmitter: Send + Sync {
    fn copy(
        &self,
        context: &Context,
        dst: AttributeValue,
        src: AttributeValue,
    ) -> Box<dyn Operation>;

    fn stack_arg_store(
        &self,
        _context: &Context,
        _abi: &AbiInfo,
        _value: AttributeValue,
        _outgoing_size: u32,
        _offset: i64,
    ) -> Result<Box<dyn Operation>, PassError> {
        Err(PassError::InvalidRuleSet(
            "stack-passed call arguments are not supported by this target".to_string(),
        ))
    }

    fn call_prefix(
        &self,
        _context: &Context,
        _abi: &AbiInfo,
        _outgoing_size: u32,
    ) -> Vec<Box<dyn Operation>> {
        Vec::new()
    }

    fn call_suffix(
        &self,
        _context: &Context,
        _abi: &AbiInfo,
        _outgoing_size: u32,
    ) -> Vec<Box<dyn Operation>> {
        Vec::new()
    }
}

pub struct CallLowering {
    abi: &'static AbiInfo,
    emitter: Box<dyn CallEmitter>,
}

impl CallLowering {
    pub fn new(abi: &'static AbiInfo, emitter: Box<dyn CallEmitter>) -> Self {
        Self { abi, emitter }
    }

    pub fn lower(
        &self,
        context: &Context,
        op: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<bool, PassError> {
        if let Some(result) = op.as_op::<IndirectResultOp>() {
            let register = self.abi.indirect_result.ok_or_else(|| {
                PassError::InvalidRuleSet("ABI has no indirect result register".to_string())
            })?;
            let copy = self.emitter.copy(
                context,
                virtual_reg(result.result().number(), register.0),
                physical_reg(register),
            );
            rewriter.replace_op(op, copy.as_ref())?;
            return Ok(true);
        }

        let (callee, args, result, indirect_result) = if let Some(call) = op.as_op::<CallOp>() {
            (
                Callee::Direct(call.callee()),
                call.args(),
                call.result(),
                None,
            )
        } else if let Some(call) = op.as_op::<IndirectCallOp>() {
            (
                Callee::Indirect(call.callee()),
                call.args(),
                call.result(),
                None,
            )
        } else if let Some(call) = op.as_op::<CallIndirectResultOp>() {
            (
                Callee::Direct(call.callee()),
                call.args(),
                call.result(),
                Some(call.destination()),
            )
        } else {
            return Ok(false);
        };

        let mut tuple_arguments = Vec::new();
        let mut lowered_arguments = Vec::with_capacity(args.len());
        for arg in args {
            let ty = context.get_type_data(context.get_value(arg).ty());
            if (ty.as_ref() as &dyn std::any::Any)
                .downcast_ref::<TupleType>()
                .is_none()
            {
                lowered_arguments.push(vec![arg]);
                continue;
            }
            let defining_op = context.get_value(arg).defining_op().ok_or_else(|| {
                PassError::InvalidRuleSet(
                    "tuple call argument must be produced by make_tuple".to_string(),
                )
            })?;
            let tuple_instance = context.get_op(defining_op);
            let tuple = tuple_instance
                .clone()
                .as_op::<MakeTupleOp>()
                .ok_or_else(|| {
                    PassError::InvalidRuleSet(
                        "tuple call argument must be produced by make_tuple".to_string(),
                    )
                })?;
            let uses = context.value_uses(arg);
            if uses.len() != 1 || uses[0].op() != op.op().id {
                return Err(PassError::InvalidRuleSet(
                    "tuple call argument must only be consumed by its call".to_string(),
                ));
            }
            lowered_arguments.push(tuple.operands().to_vec());
            tuple_arguments.push(defining_op);
        }

        let mut next_slot = HashMap::new();
        let mut argument_values = Vec::new();
        let mut argument_locations = Vec::new();
        let mut stack_args = 0u32;
        for values in lowered_arguments {
            let mut trial_slots = next_slot.clone();
            let direct = if self.abi.argument_group_fits_register_limit(values.len()) {
                values
                    .iter()
                    .map(|&value| {
                        next_register(self.abi, value_kind(context, value), &mut trial_slots)
                    })
                    .collect::<Option<Vec<_>>>()
            } else {
                None
            };
            if let Some(registers) = direct {
                next_slot = trial_slots;
                argument_values.extend(values);
                argument_locations.extend(registers.into_iter().map(ArgumentLocation::Register));
                continue;
            }

            for &value in &values {
                if self.abi.argument_group_rollback == GroupRollback::Exhaust {
                    exhaust_argument_registers(
                        self.abi,
                        value_kind(context, value),
                        &mut next_slot,
                    );
                }
                let class = stack_class(self.abi, value_kind(context, value)).ok_or_else(|| {
                    PassError::InvalidRuleSet("ABI has no argument sequence".to_string())
                })?;
                argument_values.push(value);
                argument_locations.push(ArgumentLocation::Stack {
                    class,
                    offset: i64::from(stack_args * self.abi.stack.slot_size),
                });
                stack_args += 1;
            }
        }
        let outgoing_size = if stack_args == 0 {
            0
        } else {
            let bytes = stack_args * self.abi.stack.slot_size;
            bytes.div_ceil(self.abi.stack.align) * self.abi.stack.align
        };

        let indirect_class = self
            .abi
            .args
            .iter()
            .find(|sequence| sequence.kind == ValueKind::Int)
            .and_then(|sequence| sequence.regs.first())
            .map(|register| register.0)
            .ok_or_else(|| {
                PassError::InvalidRuleSet("ABI has no integer argument registers".to_string())
            })?;

        let detach = |rewriter: &mut Rewriter, value: ValueId, class| {
            let ty = context.get_value(value).ty();
            let fresh = context.create_value(ty, None).id().number();
            let copy = self.emitter.copy(
                context,
                virtual_reg(fresh, class),
                virtual_reg(value.number(), class),
            );
            rewriter.insert_op_before(op, copy.as_ref()).map(|()| fresh)
        };

        let fresh_callee = match callee {
            Callee::Direct(_) => None,
            Callee::Indirect(value) => Some(detach(rewriter, value, indirect_class)?),
        };
        let fresh_indirect_result = indirect_result
            .map(|value| {
                let register = self.abi.indirect_result.ok_or_else(|| {
                    PassError::InvalidRuleSet("ABI has no indirect result register".to_string())
                })?;
                detach(rewriter, value, register.0).map(|fresh| (fresh, register))
            })
            .transpose()?;
        let mut fresh_args = Vec::with_capacity(argument_values.len());
        for (&arg, location) in argument_values.iter().zip(&argument_locations) {
            fresh_args.push(detach(rewriter, arg, location.class())?);
        }

        for (&fresh, location) in fresh_args.iter().zip(&argument_locations) {
            match *location {
                ArgumentLocation::Register(register) => {
                    let copy = self.emitter.copy(
                        context,
                        physical_reg(register),
                        virtual_reg(fresh, register.0),
                    );
                    rewriter.insert_op_before(op, copy.as_ref())?;
                }
                ArgumentLocation::Stack { class, offset } => {
                    let store = self.emitter.stack_arg_store(
                        context,
                        self.abi,
                        virtual_reg(fresh, class),
                        outgoing_size,
                        offset,
                    )?;
                    rewriter.insert_op_before(op, store.as_ref())?;
                }
            }
        }
        if let Some((fresh, register)) = fresh_indirect_result {
            let copy = self.emitter.copy(
                context,
                physical_reg(register),
                virtual_reg(fresh, register.0),
            );
            rewriter.insert_op_before(op, copy.as_ref())?;
        }

        let saved_ra = if let Some(ra) = self.abi.ra {
            let ty = tir::builtin::IntegerType::new(context, self.abi.stack.slot_size * 8);
            let saved = context.create_value(ty, None).id().number();
            let copy = self
                .emitter
                .copy(context, virtual_reg(saved, ra.0), physical_reg(ra));
            rewriter.insert_op_before(op, copy.as_ref())?;
            Some((saved, ra))
        } else {
            None
        };

        for prefix in self.emitter.call_prefix(context, self.abi, outgoing_size) {
            rewriter.insert_op_before(op, prefix.as_ref())?;
        }

        let clobbers = AttributeValue::Array(
            self.abi
                .caller_saved
                .iter()
                .copied()
                .map(physical_reg)
                .collect(),
        );
        let call: Box<dyn Operation> = match callee {
            Callee::Direct(name) => Box::new(
                super::VirtualCallOpBuilder::new(context)
                    .attr("callee", AttributeValue::Str(name))
                    .attr("clobbers", clobbers)
                    .build(),
            ),
            Callee::Indirect(_) => Box::new(
                super::VirtualIndirectCallOpBuilder::new(context)
                    .attr(
                        "callee_reg",
                        virtual_reg(
                            fresh_callee.expect("indirect callee was detached"),
                            indirect_class,
                        ),
                    )
                    .attr("clobbers", clobbers)
                    .build(),
            ),
        };
        rewriter.insert_op_before(op, call.as_ref())?;
        for suffix in self.emitter.call_suffix(context, self.abi, outgoing_size) {
            rewriter.insert_op_before(op, suffix.as_ref())?;
        }

        let restore = saved_ra.map(|(saved, ra)| {
            self.emitter
                .copy(context, physical_reg(ra), virtual_reg(saved, ra.0))
        });
        if let Some(restore) = &restore {
            rewriter.insert_op_before(op, restore.as_ref())?;
        }

        let result_type = context.get_type_data(context.get_value(result).ty());
        if let Some(tuple) =
            (result_type.as_ref() as &dyn std::any::Any).downcast_ref::<TupleType>()
        {
            let element_types = tuple.elements(context);
            let mut next_slot = HashMap::new();
            let mut registers = Vec::with_capacity(element_types.len());
            for element_type in element_types {
                let kind = crate::backend::abi::type_kind(context, element_type);
                let slot = next_slot.entry(kind).or_insert(0usize);
                let register = self
                    .abi
                    .rets
                    .iter()
                    .find(|sequence| sequence.kind == kind)
                    .and_then(|sequence| sequence.regs.get(*slot))
                    .copied()
                    .ok_or_else(|| {
                        PassError::InvalidRuleSet(format!(
                            "ABI has no return register for tuple element {}",
                            registers.len()
                        ))
                    })?;
                *slot += 1;
                registers.push(register);
            }

            let mut extracts = vec![];
            for usage in context.value_uses(result) {
                if usage.operand_index() != Some(0) {
                    return Err(PassError::InvalidRuleSet(
                        "tuple call result must only be consumed by tuple_get".to_string(),
                    ));
                }
                let extract_instance = context.get_op(usage.op());
                let extract = extract_instance
                    .clone()
                    .as_op::<TupleGetOp>()
                    .ok_or_else(|| {
                        PassError::InvalidRuleSet(
                            "tuple call result must only be consumed by tuple_get".to_string(),
                        )
                    })?;
                let register = registers.get(extract.index()).copied().ok_or_else(|| {
                    PassError::InvalidRuleSet("tuple_get index is out of bounds".to_string())
                })?;
                let block = context.parent_block(usage.op()).ok_or_else(|| {
                    PassError::InvalidRuleSet("tuple_get has no parent block".to_string())
                })?;
                extracts.push((
                    extract.index(),
                    extract.result(),
                    register,
                    usage.op(),
                    block,
                ));
            }
            extracts.sort_by_key(|(index, result, ..)| (*index, result.number()));

            for &(_, extracted, register, _, _) in &extracts {
                let copy = self.emitter.copy(
                    context,
                    virtual_reg(extracted.number(), register.0),
                    physical_reg(register),
                );
                rewriter.insert_op_before(op, copy.as_ref())?;
            }
            for &(_, _, _, extract, block) in &extracts {
                rewriter.erase_op(&OperationRef::new(
                    context.get_op(extract),
                    Some(context.get_block(block)),
                    None,
                ))?;
            }
            rewriter.erase_op(op)?;
            erase_tuple_arguments(context, rewriter, &tuple_arguments)?;
            return Ok(true);
        }

        if context.get_value(result).ty() == UnitType::new(context) {
            rewriter.erase_op(op)?;
            erase_tuple_arguments(context, rewriter, &tuple_arguments)?;
            return Ok(true);
        }

        let kind = value_kind(context, result);
        let return_reg = self
            .abi
            .rets
            .iter()
            .find(|sequence| sequence.kind == kind)
            .or_else(|| {
                (kind != ValueKind::Int).then(|| {
                    self.abi
                        .rets
                        .iter()
                        .find(|sequence| sequence.kind == ValueKind::Int)
                })?
            })
            .and_then(|sequence| sequence.regs.first())
            .copied()
            .ok_or_else(|| PassError::InvalidRuleSet("ABI has no return register".to_string()))?;
        let copy = self.emitter.copy(
            context,
            virtual_reg(result.number(), return_reg.0),
            physical_reg(return_reg),
        );
        rewriter.replace_op(op, copy.as_ref())?;
        erase_tuple_arguments(context, rewriter, &tuple_arguments)?;
        Ok(true)
    }
}

fn erase_tuple_arguments(
    context: &Context,
    rewriter: &mut Rewriter,
    tuple_arguments: &[OpId],
) -> Result<(), PassError> {
    for &tuple in tuple_arguments {
        let block = context.parent_block(tuple).ok_or_else(|| {
            PassError::InvalidRuleSet("make_tuple has no parent block".to_string())
        })?;
        rewriter.erase_op(&OperationRef::new(
            context.get_op(tuple),
            Some(context.get_block(block)),
            None,
        ))?;
    }
    Ok(())
}

enum Callee {
    Direct(String),
    Indirect(ValueId),
}

#[derive(Clone, Copy)]
enum ArgumentLocation {
    Register(PhysReg),
    Stack {
        class: crate::backend::regalloc::RegClassId,
        offset: i64,
    },
}

impl ArgumentLocation {
    fn class(self) -> crate::backend::regalloc::RegClassId {
        match self {
            ArgumentLocation::Register(register) => register.0,
            ArgumentLocation::Stack { class, .. } => class,
        }
    }
}

fn next_register(
    abi: &AbiInfo,
    mut kind: ValueKind,
    next_slot: &mut HashMap<ValueKind, usize>,
) -> Option<PhysReg> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(kind) {
            return None;
        }
        let sequence = match abi.args.iter().find(|sequence| sequence.kind == kind) {
            Some(sequence) => sequence,
            None if kind != ValueKind::Int => {
                kind = ValueKind::Int;
                continue;
            }
            None => return None,
        };
        let slot = next_slot.entry(kind).or_insert(0);
        if let Some(&register) = sequence.regs.get(*slot) {
            *slot += 1;
            return Some(register);
        }
        match sequence.overflow {
            Overflow::Chain(next) => kind = next,
            Overflow::Stack => return None,
        }
    }
}

fn stack_class(abi: &AbiInfo, mut kind: ValueKind) -> Option<crate::backend::regalloc::RegClassId> {
    let mut visited = HashSet::new();
    let mut value_class = None;
    loop {
        if !visited.insert(kind) {
            return None;
        }
        let sequence = match abi.args.iter().find(|sequence| sequence.kind == kind) {
            Some(sequence) => sequence,
            None if kind != ValueKind::Int => {
                kind = ValueKind::Int;
                continue;
            }
            None => return None,
        };
        value_class.get_or_insert(sequence.regs.first()?.0);
        match sequence.overflow {
            Overflow::Chain(next) => kind = next,
            Overflow::Stack => return value_class,
        }
    }
}

fn physical_reg(reg: PhysReg) -> AttributeValue {
    AttributeValue::Register(RegisterAttr::Physical {
        class: reg.0,
        index: reg.1,
    })
}

fn virtual_reg(id: u32, class: crate::backend::regalloc::RegClassId) -> AttributeValue {
    AttributeValue::Register(RegisterAttr::Virtual {
        id,
        class: Some(class),
    })
}
