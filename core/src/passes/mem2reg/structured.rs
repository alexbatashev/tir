//! Region-structured SSA construction.
//!
//! A structured body is a tree of linear operation sequences and `scf` gates, so
//! the join points are syntactic: no dominance frontiers are needed. Walking the
//! tree while tracking the value reaching a slot promotes it outright — a
//! conditional grows one result per promoted slot, a loop one carried port.

use std::sync::Arc;

use crate::attributes::AttributeValue;
use crate::builtin::TokenType;
use crate::{
    Block, Conditional, Context, LoopLike, MemoryRead, MemoryWrite, OpId, OpInstance, Operation,
    OperationRef, PassError, PromotableAllocation, RegionId, Rewriter, TypeId, ValueId, scf,
};

use crate::analysis::slots::{collect_slots, values_agree_on_type};

pub(super) fn run(
    context: &Context,
    rewriter: &mut Rewriter,
    body: RegionId,
) -> Result<(), PassError> {
    let slots = collect_slots(context, &region_ops(context, body));

    for (&slot, state) in &slots {
        let Some(alloca) = state.alloca else { continue };
        if state.escapes {
            continue;
        }

        // A slot that is never read has no reaching value to reconstruct: its
        // stores are dead, since nothing outside can observe the location.
        if state.loads.is_empty() {
            erase_all(context, rewriter, state.stores.iter().copied())?;
            erase_all(context, rewriter, [alloca])?;
            continue;
        }

        if !values_agree_on_type(context, state) {
            continue;
        }

        // The slot's pointer is only visible inside the block that allocates it, so
        // that block — not the function body — bounds the promotion: gates enclosing
        // the allocation never see the slot and never grow a port for it.
        let home = context.get_block(context.parent_block(alloca).expect("live alloca"));
        if !supported(context, &home, slot) {
            continue;
        }

        let mut promoter = Promoter {
            context,
            slot,
            alloca,
            uninitialized: None,
            scopes: vec![],
            erase: vec![],
        };
        promoter.walk(&home, None);
        let Promoter {
            uninitialized,
            erase,
            ..
        } = promoter;
        erase_all(context, rewriter, erase)?;
        if uninitialized.is_none() {
            erase_all(context, rewriter, [alloca])?;
        }
    }

    Ok(())
}

/// Reconstructs the value reaching each use of one slot, rewriting the region
/// tree as it goes.
struct Promoter<'a> {
    context: &'a Context,
    slot: ValueId,
    alloca: OpId,
    /// The materialized stand-in for the slot's value before anything stores to
    /// it: a re-read of the still-uninitialized location. Created on demand, and
    /// its presence is what keeps the allocation alive.
    uninitialized: Option<ValueId>,
    /// The token scopes of the loops whose carried port is being grown, innermost last.
    /// An `scf.break`/`scf.continue` naming one of them leaves through that port and so
    /// carries the value reaching it.
    scopes: Vec<ValueId>,
    erase: Vec<OpId>,
}

/// How one operation relates to the slot being promoted.
enum SlotUse {
    Read(ValueId),
    Write(ValueId),
    Nested,
    Unrelated,
}

impl Promoter<'_> {
    /// Walk `block` with `incoming` reaching it, returning the value that reaches
    /// its end.
    fn walk(&mut self, block: &Arc<Block>, incoming: Option<ValueId>) -> Option<ValueId> {
        let mut current = incoming;
        for op_id in block.op_ids() {
            let op = self.context.get_op(op_id);
            match classify(&op, self.slot) {
                SlotUse::Read(result) => {
                    let value = self.reaching(current);
                    current = Some(value);
                    self.context.replace_value_uses(result, value);
                    self.erase.push(op_id);
                }
                SlotUse::Write(value) => {
                    current = Some(value);
                    self.erase.push(op_id);
                }
                SlotUse::Nested
                    if references_slot(self.context, &op, self.slot)
                        || exits_scopes(self.context, &op, &self.scopes) =>
                {
                    current = self.walk_nested(block, op, current);
                }
                _ if exits_scope(&op, &self.scopes) => {
                    let value = self.reaching(current);
                    self.append_operand(op_id, value);
                }
                _ => {}
            }
        }
        current
    }

    /// Promote the slot through one structured operation, growing a result (a
    /// conditional) or a carried port (a loop) when the region tree redefines it.
    fn walk_nested(
        &mut self,
        block: &Arc<Block>,
        op: Arc<OpInstance>,
        incoming: Option<ValueId>,
    ) -> Option<ValueId> {
        if !stores_slot(self.context, &op, self.slot) {
            // Read-only inside: the incoming value already dominates every use,
            // so the loads rewrite against it and no port is needed.
            for region in op.regions.clone() {
                let block = single_block(self.context, region).expect("checked by `supported`");
                self.walk(&block, incoming);
            }
            return incoming;
        }

        if op.clone().as_interface::<dyn Conditional>().is_some() {
            Some(self.grow_conditional(block, op, incoming))
        } else {
            Some(self.grow_loop(block, op, incoming))
        }
    }

    fn grow_conditional(
        &mut self,
        block: &Arc<Block>,
        op: Arc<OpInstance>,
        incoming: Option<ValueId>,
    ) -> ValueId {
        let arm_values: Vec<ValueId> = op
            .regions
            .iter()
            .map(|&region| {
                let arm = single_block(self.context, region).expect("checked by `supported`");
                let value = self.walk(&arm, incoming);
                self.reaching(value)
            })
            .collect();

        for (&region, &value) in op.regions.iter().zip(&arm_values) {
            self.append_yielded(region, value);
        }

        let ty = self.context.get_value(arm_values[0]).ty();
        let grown = scf::IfOpBuilder::new(self.context)
            .condition(op.operands[0])
            .then_body(op.regions[0])
            .else_body(op.regions[1])
            .result_types(result_types(self.context, &op, ty))
            .build();
        self.replace(block, op, &grown)
    }

    fn grow_loop(
        &mut self,
        block: &Arc<Block>,
        op: Arc<OpInstance>,
        incoming: Option<ValueId>,
    ) -> ValueId {
        let init = self.reaching(incoming);
        let ty = self.context.get_value(init).ty();
        let is_while = op.is::<scf::WhileOp>();

        let body_region = *op.regions.last().expect("a loop has a body");
        let scope = loop_scope(self.context, body_region);
        self.scopes.extend(scope);

        // `scf.while` observes the carried value first in its condition region,
        // which forwards what the body sees; `scf.for` carries straight into the
        // body.
        let carried = if is_while {
            let condition = self.carry_into(op.regions[0], ty);
            let forwarded = self.reaching(condition);
            // `scf.condition` forwards the deciding boolean, then one value per port.
            self.append_yielded(op.regions[0], forwarded);
            self.carry_into(op.regions[1], ty)
        } else {
            self.carry_into(op.regions[0], ty)
        };
        let latched = self.reaching(carried);
        self.append_yielded(body_region, latched);
        if scope.is_some() {
            self.scopes.pop();
        }

        let mut inits = op.operands.clone();
        inits.push(init);
        let result_types = result_types(self.context, &op, ty);
        let grown: Box<dyn Operation> = if is_while {
            Box::new(
                scf::WhileOpBuilder::new(self.context)
                    .inits(inits)
                    .condition_region(op.regions[0])
                    .body(op.regions[1])
                    .result_types(result_types)
                    .build(),
            )
        } else {
            let carried_inits = inits.split_off(3);
            Box::new(
                scf::ForOpBuilder::new(self.context)
                    .lower_bound(inits[0])
                    .upper_bound(inits[1])
                    .step(inits[2])
                    .inits(carried_inits)
                    .body(op.regions[0])
                    .result_types(result_types)
                    .build(),
            )
        };
        self.replace(block, op, grown.as_ref())
    }

    /// Give `region`'s block one more entry argument for the slot and walk it with
    /// that argument reaching its start.
    fn carry_into(&mut self, region: RegionId, ty: TypeId) -> Option<ValueId> {
        let block = single_block(self.context, region).expect("checked by `supported`");
        let carried = self.context.append_block_argument(block.id(), ty).id();
        self.walk(&block, Some(carried))
    }

    /// Swap `op` for the grown operation, keeping the values its old results fed.
    fn replace(
        &mut self,
        block: &Arc<Block>,
        op: Arc<OpInstance>,
        grown: &dyn Operation,
    ) -> ValueId {
        let results = self.context.get_op(grown.id()).results.clone();
        for (&old, &new) in op.results.iter().zip(&results) {
            self.context.replace_value_uses(old, new);
        }
        block.replace_op(op.id, grown.id());
        self.context.detach_op_uses(&op);
        self.context.remove_operation(op.id);
        *results.last().expect("the grown op carries the slot")
    }

    /// Append `value` to what a structured region yields on its back or exit edge. A
    /// region leaving through an early exit took the value there instead, on the edge
    /// that exit branches along.
    fn append_yielded(&self, region: RegionId, value: ValueId) {
        let block = single_block(self.context, region).expect("checked by `supported`");
        let terminator = *block.op_ids().last().expect("regions are terminated");
        let op = self.context.get_op(terminator);
        if op.is::<scf::BreakOp>() || op.is::<scf::ContinueOp>() {
            return;
        }
        self.append_operand(terminator, value);
    }

    /// Append `value` to a terminator's trailing variadic group, keeping the
    /// operand segment sizes that describe that grouping in step.
    fn append_operand(&self, op_id: OpId, value: ValueId) {
        let op = self.context.get_op(op_id);
        let mut operands = op.operands.clone();
        operands.push(value);
        self.context.set_op_operands(op_id, operands);

        let mut attributes = op.attributes.clone();
        if let Some(attribute) = attributes
            .iter_mut()
            .find(|attribute| attribute.name == "operand_segment_sizes")
            && let AttributeValue::Array(sizes) = &mut attribute.value
            && let Some(AttributeValue::UInt(last)) = sizes.last_mut()
        {
            *last += 1;
            self.context.set_op_attributes(op_id, attributes);
        }
    }

    /// The value reaching this point, materializing a read of the untouched slot
    /// when nothing has defined it yet.
    fn reaching(&mut self, current: Option<ValueId>) -> ValueId {
        if let Some(value) = current {
            return value;
        }
        if let Some(value) = self.uninitialized {
            return value;
        }
        let value = self.materialize_uninitialized();
        self.uninitialized = Some(value);
        value
    }

    /// Reading a slot before anything stores to it yields an indeterminate value.
    /// With no `undef` operation to name it, the read itself is the value: a copy
    /// of one of the slot's own loads, placed right after the allocation.
    fn materialize_uninitialized(&self) -> ValueId {
        let block = self
            .context
            .get_block(self.context.parent_block(self.alloca).expect("live alloca"));
        let template = first_load(self.context, &block, self.slot).expect("the slot is read");
        let results = template
            .results
            .iter()
            .map(|&result| {
                self.context
                    .create_value(self.context.get_value(result).ty(), None)
                    .id()
            })
            .collect::<Vec<_>>();
        let read = self.context.add_operation(OpInstance::new_dynamic(
            (template.dialect().as_str(), template.name().as_str()),
            self.context.as_context_ref(),
            template.operands.clone(),
            results.clone(),
            vec![],
            template.attributes.clone(),
        ));
        let position = block
            .op_ids()
            .iter()
            .position(|&id| id == self.alloca)
            .expect("the alloca is in its block");
        block.insert(position + 1, read.id);
        results[0]
    }
}

/// The loop body's token scope: the value its `scf.break`/`scf.continue` name.
fn loop_scope(context: &Context, body: RegionId) -> Option<ValueId> {
    let token = TokenType::new(context);
    single_block(context, body)?
        .arguments()
        .iter()
        .find(|argument| argument.ty() == token)
        .map(|argument| argument.id())
}

/// Whether `op` leaves one of the loops whose port is being grown.
fn exits_scope(op: &Arc<OpInstance>, scopes: &[ValueId]) -> bool {
    (op.is::<scf::BreakOp>() || op.is::<scf::ContinueOp>()) && scopes.contains(&op.operands[0])
}

/// Whether anything inside `op`'s regions leaves one of those loops.
fn exits_scopes(context: &Context, op: &Arc<OpInstance>, scopes: &[ValueId]) -> bool {
    nested_ops(context, op)
        .iter()
        .any(|&op_id| exits_scope(&context.get_op(op_id), scopes))
}

/// Whether the slot can be promoted through every structured operation that
/// mentions it: only conditionals and loops built from single-block regions whose
/// every exit carries a value — a yield, a condition for a loop's condition region,
/// or a `break`/`continue` out of a loop the walk itself grows. Anything else — an
/// unknown region, an exit out of a loop enclosing the allocation — is left in
/// memory rather than promoted in part.
fn supported(context: &Context, home: &Arc<Block>, slot: ValueId) -> bool {
    // The stand-in for an undefined value copies one of the slot's own reads, so
    // that read must be reproducible right after the allocation.
    let reads_reproducible = block_ops(context, home)
        .iter()
        .map(|&op_id| context.get_op(op_id))
        .filter(|op| reads_slot(op, slot))
        .all(|op| op.operands == [slot] && op.regions.is_empty());
    reads_reproducible && gates_carry_values(context, home, slot, &mut vec![])
}

fn gates_carry_values(
    context: &Context,
    block: &Arc<Block>,
    slot: ValueId,
    scopes: &mut Vec<ValueId>,
) -> bool {
    for op_id in block.op_ids() {
        let op = context.get_op(op_id);
        if op.regions.is_empty()
            || (!references_slot(context, &op, slot) && !exits_scopes(context, &op, scopes))
        {
            continue;
        }

        // The gate is recognized through its interface, but growing a port has to
        // rebuild it, which only the concrete `scf` builders can do.
        let is_conditional =
            op.clone().as_interface::<dyn Conditional>().is_some() && op.is::<scf::IfOp>();
        let is_loop = op.clone().as_interface::<dyn LoopLike>().is_some()
            && (op.is::<scf::ForOp>() || op.is::<scf::WhileOp>());
        if !is_conditional && !is_loop {
            return false;
        }
        // A loop only grows a port — and so only carries values on its exits — when
        // something inside it writes the slot.
        let scope = if is_loop && stores_slot(context, &op, slot) {
            loop_scope(context, *op.regions.last().expect("a loop has a body"))
        } else {
            None
        };
        scopes.extend(scope);

        let carried = op.regions.iter().enumerate().all(|(index, &nested)| {
            let Some(nested_block) = single_block(context, nested) else {
                return false;
            };
            // A loop's condition region ends in a condition; every other structured
            // region must yield or leave through a loop exit. An exit out of a loop
            // that grows no port needs no value: the slot dies with that loop's body.
            let terminator = context.get_op(*nested_block.op_ids().last().expect("terminated"));
            let expects_condition = op.is::<scf::WhileOp>() && index == 0;
            if terminator.is::<scf::ConditionOp>() != expects_condition {
                return false;
            }
            let exits = terminator.is::<scf::BreakOp>() || terminator.is::<scf::ContinueOp>();
            if !expects_condition && !terminator.is::<scf::YieldOp>() && !exits {
                return false;
            }
            gates_carry_values(context, &nested_block, slot, scopes)
        });

        if scope.is_some() {
            scopes.pop();
        }
        if !carried {
            return false;
        }
    }

    true
}

fn classify(op: &Arc<OpInstance>, slot: ValueId) -> SlotUse {
    if op
        .clone()
        .as_interface::<dyn PromotableAllocation>()
        .is_some()
    {
        return SlotUse::Unrelated;
    }
    if let Some(read) = op.clone().as_interface::<dyn MemoryRead>()
        && read.read_location() == slot
    {
        return SlotUse::Read(read.read_value());
    }
    if let Some(write) = op.clone().as_interface::<dyn MemoryWrite>()
        && write.write_location() == slot
    {
        return SlotUse::Write(write.written_value());
    }
    if op.regions.is_empty() {
        SlotUse::Unrelated
    } else {
        SlotUse::Nested
    }
}

fn reads_slot(op: &Arc<OpInstance>, slot: ValueId) -> bool {
    matches!(classify(op, slot), SlotUse::Read(_))
}

fn first_load(context: &Context, block: &Arc<Block>, slot: ValueId) -> Option<Arc<OpInstance>> {
    block_ops(context, block)
        .into_iter()
        .map(|op_id| context.get_op(op_id))
        .find(|op| reads_slot(op, slot))
}

/// Whether anything inside `op`'s regions names the slot.
fn references_slot(context: &Context, op: &Arc<OpInstance>, slot: ValueId) -> bool {
    nested_ops(context, op)
        .iter()
        .any(|op_id| context.get_op(*op_id).operands.contains(&slot))
}

/// Whether anything inside `op`'s regions writes the slot, which is what forces a
/// new result or carried port.
fn stores_slot(context: &Context, op: &Arc<OpInstance>, slot: ValueId) -> bool {
    nested_ops(context, op)
        .iter()
        .any(|&op_id| matches!(classify(&context.get_op(op_id), slot), SlotUse::Write(_)))
}

fn nested_ops(context: &Context, op: &Arc<OpInstance>) -> Vec<OpId> {
    op.regions
        .iter()
        .flat_map(|&region| region_ops(context, region))
        .collect()
}

/// Every operation in a region tree, outermost first.
fn region_ops(context: &Context, region: RegionId) -> Vec<OpId> {
    context
        .get_region(region)
        .iter(context.clone())
        .flat_map(|block| block_ops(context, &block))
        .collect()
}

/// Every operation in a block and its nested regions, outermost first.
fn block_ops(context: &Context, block: &Arc<Block>) -> Vec<OpId> {
    let mut ops = vec![];
    for op_id in block.op_ids() {
        ops.push(op_id);
        ops.extend(nested_ops(context, &context.get_op(op_id)));
    }
    ops
}

fn single_block(context: &Context, region: RegionId) -> Option<Arc<Block>> {
    let mut blocks = context.get_region(region).iter(context.clone());
    let block = blocks.next()?;
    blocks.next().is_none().then_some(block)
}

/// The grown operation's result types: the ones it already had, plus the slot's.
fn result_types(context: &Context, op: &Arc<OpInstance>, ty: TypeId) -> Vec<TypeId> {
    op.results
        .iter()
        .map(|&result| context.get_value(result).ty())
        .chain(std::iter::once(ty))
        .collect()
}

fn erase_all(
    context: &Context,
    rewriter: &mut Rewriter,
    ops: impl IntoIterator<Item = OpId>,
) -> Result<(), PassError> {
    for op_id in ops {
        if !context.has_operation(op_id) {
            continue;
        }
        let block = context.parent_block(op_id).map(|id| context.get_block(id));
        rewriter.erase_op(&OperationRef::new(context.get_op(op_id), block, None))?;
    }
    Ok(())
}
