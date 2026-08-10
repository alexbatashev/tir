use std::collections::BTreeMap;

use crate::analysis::DominatorTree;
use crate::{
    AnalysisManager, Context, MemoryRead, MemoryWrite, OpId, OperationRef, Pass, PassError,
    PassTarget, PreservedAnalyses, PromotableAllocation, Rewriter, ValueId, builtin::FuncOp,
};

mod structured;
mod unstructured;

#[derive(Default)]
pub struct Mem2RegPass;

/// What we know about one allocated stack slot across the whole function.
#[derive(Default)]
struct SlotState {
    alloca: Option<OpId>,
    stores: Vec<OpId>,
    loads: Vec<OpId>,
    /// The slot's pointer is used somewhere other than a load/store, so its
    /// contents may be observed indirectly and it cannot be promoted.
    escapes: bool,
}

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
    ) -> Result<PreservedAnalyses, PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(PreservedAnalyses::all());
        }
        let Some(&body) = op.op().regions.first() else {
            return Ok(PreservedAnalyses::all());
        };

        // A structured body is one region tree with `scf` gates at its joins, where
        // SSA construction needs no dominance; anything else is the unraised `goto`
        // remainder, promoted by the dominance-based path below.
        if context.get_region(body).iter(context.clone()).count() == 1 {
            structured::run(context, rewriter, body)?;
            return Ok(PreservedAnalyses::none());
        }

        let dom_tree = analyses.get::<DominatorTree>(context, op.op().id);
        unstructured::run(context, rewriter, &dom_tree)?;

        // Promotion only erases loads/stores/allocas — never terminators or
        // blocks — so block-level dominance survives.
        Ok(PreservedAnalyses::none().preserve::<DominatorTree>())
    }
}

/// Classify every load/store/escape against the slots opened by `alloca`s.
fn collect_slots(context: &Context, op_ids: &[OpId]) -> BTreeMap<ValueId, SlotState> {
    let mut slots: BTreeMap<ValueId, SlotState> = BTreeMap::new();

    for &op_id in op_ids {
        if let Some(alloca) = context
            .get_op(op_id)
            .as_interface::<dyn PromotableAllocation>()
        {
            slots.entry(alloca.promoted_location()).or_default().alloca = Some(op_id);
        }
    }

    for &op_id in op_ids {
        let instance = context.get_op(op_id);
        if instance
            .clone()
            .as_interface::<dyn PromotableAllocation>()
            .is_some()
        {
            continue;
        }

        let read_location = instance
            .clone()
            .as_interface::<dyn MemoryRead>()
            .map(|read| read.read_location());
        if let Some(location) = read_location
            && let Some(slot) = slots.get_mut(&location)
        {
            slot.loads.push(op_id);
        }

        let write = instance.clone().as_interface::<dyn MemoryWrite>();
        let write_location = write.as_ref().map(|write| write.write_location());
        if let Some(location) = write_location
            && let Some(slot) = slots.get_mut(&location)
        {
            slot.stores.push(op_id);
        }
        if let Some(value) = write.map(|write| write.written_value())
            && let Some(slot) = slots.get_mut(&value)
        {
            slot.escapes = true;
        }

        for operand in &instance.operands {
            if Some(*operand) == read_location || Some(*operand) == write_location {
                continue;
            }
            if let Some(slot) = slots.get_mut(operand) {
                slot.escapes = true;
            }
        }
    }

    slots
}

fn store_value(context: &Context, store: OpId) -> ValueId {
    context
        .get_op(store)
        .as_interface::<dyn MemoryWrite>()
        .expect("store op implements MemoryWrite")
        .written_value()
}

fn load_result(context: &Context, load: OpId) -> ValueId {
    context
        .get_op(load)
        .as_interface::<dyn MemoryRead>()
        .expect("load op implements MemoryRead")
        .read_value()
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, OpId, Operation, PassManager,
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

        let mut entry_b = IRBuilder::new(entry.clone());
        let slot = entry_b
            .insert(p::alloca(&context, 4u64, 4u64, PtrType::typed(&context, i32_ty)).build());
        let slot_ptr = slot.result();
        let alloca_id = slot.id();
        let store_id = entry_b
            .insert(p::store(&context, param_id, slot_ptr).build())
            .id();
        entry_b.insert(b::br(&context, vec![], next.id()).build());

        let mut next_b = IRBuilder::new(next.clone());
        let load = next_b.insert(p::load(&context, slot_ptr, i32_ty).build());
        let load_id = load.id();
        let ret_id = next_b
            .insert(b::r#return(&context, load.result()).build())
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

        let mut entry_b = IRBuilder::new(entry.clone());
        let slot = entry_b
            .insert(p::alloca(&context, 4u64, 4u64, PtrType::typed(&context, i32_ty)).build());
        let slot_ptr = slot.result();
        let alloca_id = slot.id();
        entry_b.insert(b::cond_br(&context, cond_id, vec![], vec![], then.id(), join.id()).build());

        let mut then_b = IRBuilder::new(then.clone());
        let store_id = then_b
            .insert(p::store(&context, param_id, slot_ptr).build())
            .id();
        then_b.insert(b::br(&context, vec![], join.id()).build());

        let mut join_b = IRBuilder::new(join.clone());
        let load_id = join_b
            .insert(p::load(&context, slot_ptr, i32_ty).build())
            .id();
        join_b.insert(b::r#return(&context, context.get_op(load_id).results[0]).build());

        run_mem2reg(&context, func.id());

        assert!(context.has_operation(alloca_id));
        assert!(context.has_operation(store_id));
        assert!(context.has_operation(load_id));
    }

    // The multiple-stores case, the dead-alloca erase case and structured
    // promotion are covered by the FileCheck suite under core/checks/Mem2Reg.
}
