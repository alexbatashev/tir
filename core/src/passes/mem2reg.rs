use std::collections::{BTreeMap, BTreeSet};

use crate::Operation;
use crate::{
    Context, MemoryRead, MemoryWrite, OpId, OperationRef, Pass, PassError, PassTarget,
    PromotableAllocation, Rewriter, ValueId, builtin::FuncOp,
};

#[derive(Default)]
pub struct Mem2RegPass;

#[derive(Default)]
struct SlotState {
    alloca: Option<OpId>,
    current_value: Option<ValueId>,
    replacements: Vec<(ValueId, ValueId)>,
    erase_ops: BTreeSet<OpId>,
    promotable: bool,
}

impl Mem2RegPass {
    pub fn new() -> Self {
        Self
    }
}

impl Pass for Mem2RegPass {
    fn name(&self) -> &'static str {
        "mem2reg"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Operation(FuncOp::name())
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        let Some(func) = op.as_op::<FuncOp>() else {
            return Ok(());
        };
        let block = func.body();
        let op_ids = block.op_ids();

        let mut slots = BTreeMap::<ValueId, SlotState>::new();

        for op_id in &op_ids {
            let instance = context.get_op(*op_id);
            if let Some(alloc) = instance.clone().as_interface::<dyn PromotableAllocation>() {
                let location = alloc.promoted_location();
                slots.insert(
                    location,
                    SlotState {
                        alloca: Some(*op_id),
                        current_value: None,
                        replacements: Vec::new(),
                        erase_ops: BTreeSet::new(),
                        promotable: true,
                    },
                );
            }
        }

        for op_id in &op_ids {
            let instance = context.get_op(*op_id);
            if instance
                .clone()
                .as_interface::<dyn PromotableAllocation>()
                .is_some()
            {
                continue;
            }

            if let Some(read) = instance.clone().as_interface::<dyn MemoryRead>() {
                let location = read.read_location();
                if let Some(slot) = slots.get_mut(&location) {
                    if let Some(value) = slot.current_value {
                        slot.replacements.push((read.read_value(), value));
                        slot.erase_ops.insert(*op_id);
                    } else {
                        slot.promotable = false;
                    }
                    continue;
                }
            }

            if let Some(write) = instance.clone().as_interface::<dyn MemoryWrite>() {
                let location = write.write_location();
                if let Some(slot) = slots.get_mut(&location) {
                    slot.current_value = Some(write.written_value());
                    slot.erase_ops.insert(*op_id);
                    continue;
                }
            }

            for operand in &instance.operands {
                if let Some(slot) = slots.get_mut(operand) {
                    slot.promotable = false;
                }
            }
        }

        for slot in slots.values().filter(|slot| slot.promotable) {
            for (old, new) in &slot.replacements {
                context.replace_value_uses(*old, *new);
            }
        }

        let mut erase_ops = BTreeSet::new();
        for slot in slots.values().filter(|slot| slot.promotable) {
            if let Some(alloca) = slot.alloca {
                erase_ops.insert(alloca);
            }
            erase_ops.extend(slot.erase_ops.iter().copied());
        }

        for op_id in erase_ops {
            if !context.has_operation(op_id) {
                continue;
            }
            let target = OperationRef::new(context.get_op(op_id), Some(block.clone()), None);
            rewriter.erase_op(&target)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, Operation, PassManager,
        builtin::{IntegerType, ops as b},
        ptr::{PtrType, ops as p},
    };

    use super::Mem2RegPass;

    #[test]
    fn promotes_linear_stack_slot() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let param = context.create_value(i32_ty, None);
        let param_id = param.id();
        let region = context.create_region();
        let block = context.create_block(vec![param]);
        region.add_block(block.id());
        let func = b::func(&context, "id", i32_ty, Some(region.id())).build();

        let mut builder = IRBuilder::new(func.body());
        let slot = builder.insert(p::alloca(&context, PtrType::typed(&context, i32_ty)).build());
        builder.insert(p::store(&context, param_id, slot.result()).build());
        let loaded = builder
            .insert(p::load(&context, slot.result(), i32_ty).build())
            .result();
        builder.insert(b::r#return(&context, loaded).build());

        let mut pm = PassManager::new();
        pm.add_pass(Mem2RegPass::new());
        pm.run(&context, context.get_op(func.id()))
            .expect("mem2reg");

        let mut out = String::new();
        let mut fmt = IRFormatter::new(&mut out);
        func.print(&mut fmt).expect("print");

        assert!(!out.contains("ptr.alloca"));
        assert!(!out.contains("ptr.store"));
        assert!(!out.contains("ptr.load"));
        assert!(out.contains(&format!("return %{}", param_id.number())));
    }
}
