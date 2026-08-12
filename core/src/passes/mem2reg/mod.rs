use crate::analysis::DominatorTree;
use crate::{
    AnalysisManager, Context, OperationRef, Pass, PassError, PassTarget, Rewriter, builtin::FuncOp,
};

mod structured;
mod unstructured;

#[derive(Default)]
pub struct Mem2RegPass;

impl Mem2RegPass {
    pub fn new() -> Self {
        Self
    }
}

tir::register_pass!(Mem2RegPass, "mem2reg");

impl Pass for Mem2RegPass {
    fn name(&self) -> &'static str {
        "mem2reg"
    }

    fn target(&self) -> PassTarget {
        PassTarget::operation::<FuncOp>()
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        analyses: &AnalysisManager,
    ) -> Result<(), PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(());
        }
        let Some(&body) = op.op().regions.first() else {
            return Ok(());
        };

        // A structured body is one region tree with `scf` gates at its joins, where
        // SSA construction needs no dominance; anything else is the unraised `goto`
        // remainder, promoted by the dominance-based path below.
        if context.get_region(body).iter(context.clone()).count() == 1 {
            structured::run(context, rewriter, body)?;
            return Ok(());
        }

        let dom_tree = analyses.get::<DominatorTree>(context, op.op().id);
        unstructured::run(context, rewriter, &dom_tree)?;

        // Promotion only erases loads/stores/allocas — never terminators or
        // blocks — so block-level dominance survives.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, OpId, Operation, PassManager,
        builtin::{IntegerType, ops as b},
        ptr::{PtrType, ops as p},
    };

    use super::Mem2RegPass;

    fn run_mem2reg(context: &Context, func: OpId) {
        let mut pm = PassManager::new();
        pm.add_pass(Mem2RegPass::new());
        pm.run(context, context.get_op(func)).expect("mem2reg");
    }

    // Linear stack-slot promotion is covered by the FileCheck suite at
    // core/checks/Mem2Reg/basic.tir.

    /// A store in the entry block dominates a load in a successor block, so the
    /// value is forwarded across the branch.
    #[test]
    fn promotes_across_unstructured_branch() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let param = context.create_value(i32_ty, None);
        let param_id = param.id();

        let region = context.create_region();
        let entry = context.create_block(vec![param]);
        let next = context.create_block(vec![]);
        region.add_block(entry.id());
        region.add_block(next.id());
        let func = b::func(&context, "fwd", i32_ty, Some(region.id())).build();

        let slot = entry
            .append_op(p::alloca(&context, 4u64, 4u64, PtrType::typed(&context, i32_ty)).build());
        let slot_ptr = slot.result();
        let alloca_id = slot.id();
        let store_id = entry
            .append_op(p::store(&context, param_id, slot_ptr).build())
            .id();
        entry.append_op(b::br(&context, vec![], next.id()).build());

        let next_b = next.clone();
        let load = next_b.append_op(p::load(&context, slot_ptr, i32_ty).build());
        let load_id = load.id();
        let ret_id = next_b
            .append_op(b::r#return(&context, load.result()).build())
            .id();

        run_mem2reg(&context, func.id());

        assert!(!context.has_operation(alloca_id));
        assert!(!context.has_operation(store_id));
        assert!(!context.has_operation(load_id));
        // The load's consumer now reads the stored value directly.
        assert_eq!(context.get_op(ret_id).operands, vec![param_id]);
    }

    /// A store on only one side of a branch does not dominate a load placed after
    /// the join, so the slot must be left in memory.
    #[test]
    fn keeps_slot_when_store_does_not_dominate_load() {
        let context = Context::with_default_dialects();
        let i1_ty = IntegerType::new(&context, 1);
        let i32_ty = IntegerType::new(&context, 32);
        let cond = context.create_value(i1_ty, None);
        let param = context.create_value(i32_ty, None);
        let cond_id = cond.id();
        let param_id = param.id();

        let region = context.create_region();
        let entry = context.create_block(vec![cond, param]);
        let then = context.create_block(vec![]);
        let join = context.create_block(vec![]);
        for block in [&entry, &then, &join] {
            region.add_block(block.id());
        }
        let func = b::func(&context, "maybe", i32_ty, Some(region.id())).build();

        let slot = entry
            .append_op(p::alloca(&context, 4u64, 4u64, PtrType::typed(&context, i32_ty)).build());
        let slot_ptr = slot.result();
        let alloca_id = slot.id();
        entry
            .append_op(b::cond_br(&context, cond_id, vec![], vec![], then.id(), join.id()).build());

        let then_b = then.clone();
        let store_id = then_b
            .append_op(p::store(&context, param_id, slot_ptr).build())
            .id();
        then_b.append_op(b::br(&context, vec![], join.id()).build());

        let join_b = join.clone();
        let load_id = join_b
            .append_op(p::load(&context, slot_ptr, i32_ty).build())
            .id();
        join_b.append_op(b::r#return(&context, context.get_op(load_id).results[0]).build());

        run_mem2reg(&context, func.id());

        assert!(context.has_operation(alloca_id));
        assert!(context.has_operation(store_id));
        assert!(context.has_operation(load_id));
    }

    // The multiple-stores case, the dead-alloca erase case and structured
    // promotion are covered by the FileCheck suite under core/checks/Mem2Reg.
}
