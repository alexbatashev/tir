//! The region-tree seeder: reads a function's regions and builds the term graph
//! the e-graph is seeded from.
//!
//! The walk is one pass in program order. A value defined by an operation the
//! seeder has already read expands into that operation's term; anything else —
//! a function argument, a definition in another block, an operation with no
//! declared semantics — anchors as a leaf. Structured regions are the only
//! place the walk crosses a region boundary, and it crosses through the
//! [`Conditional`] and [`GuardedLoop`]/[`LoopLike`] readings alone: gates are
//! never derived from the control-flow graph.

mod terms;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use tir_adt::APInt;
use tir_symbolic::lang::infer_types;

use crate::attributes::AttributeValue;
use crate::builtin::{FloatType, IntegerType};
use crate::graph::{Dag, MetaDag, NodeId};
use crate::sem::egraph::{ir_type, minimal_unsigned_apint, semantic_type, type_width};
use crate::sem::{SemGraph, SemType, SymKind, SymPayload};
use crate::{
    Block, Conditional, Context, EntryGuard, GuardOrdering, GuardedLoop, LoopLike, MemoryRead,
    MemoryWrite, OpId, OpInstance, RegionId, TypeId, ValueId,
};

pub use terms::{SeedGraph, TermId, TermKind, TermPayload};

/// Seed the region tree of `root` — a function, or any operation with regions.
pub fn seed(context: &Context, root: OpId) -> SeedGraph {
    let pointer_width =
        crate::DataLayout::for_op(context, root).and_then(|layout| layout.pointer_size());
    let mut seeder = Seeder {
        context,
        graph: SeedGraph::new(),
        pointer_width,
        opaque_serial: 0,
        undo: Vec::new(),
        transient: 0,
    };
    for region in context.get_op(root).regions.clone() {
        seeder.seed_region(region);
    }
    seeder.graph
}

struct Seeder<'a> {
    context: &'a Context,
    graph: SeedGraph,
    pointer_width: Option<u32>,
    opaque_serial: u32,
    /// Terms recorded inside an open transient scope, restored when it closes.
    undo: Vec<(ValueId, Option<TermId>)>,
    /// Depth of open transient scopes; every recording inside one is undone.
    transient: usize,
}

impl Seeder<'_> {
    // ── region walk ─────────────────────────────────────────────────────────

    fn seed_region(&mut self, region: RegionId) {
        for block in self.context.get_region(region).iter(self.context.clone()) {
            self.seed_block(&block);
        }
    }

    fn entry_block(&self, region: RegionId) -> Option<Arc<Block>> {
        self.context
            .get_region(region)
            .iter(self.context.clone())
            .next()
    }

    fn seed_block(&mut self, block: &Arc<Block>) {
        for argument in block.arguments() {
            self.term_of(argument.id());
        }
        for op_id in block.op_ids() {
            let instance = self.context.get_op(op_id);
            self.seed_op(&instance);
        }
    }

    fn seed_op(&mut self, instance: &Arc<OpInstance>) {
        if !instance.regions.is_empty() {
            self.seed_structured(instance);
            return;
        }
        if self.seed_memory(instance) {
            return;
        }
        if let Some(term) = self.seed_pure(instance) {
            self.record(instance.results[0], term);
            return;
        }
        self.seed_unmodeled(instance);
    }

    /// A pure single-result operation: its declared semantic expression, lowered
    /// over the terms of its operands.
    fn seed_pure(&mut self, instance: &Arc<OpInstance>) -> Option<TermId> {
        if instance.results.len() != 1 {
            return None;
        }
        let result_ty = self.context.get_value(instance.results[0]).ty();
        if instance.is::<crate::builtin::ConstantOp>() {
            return Some(self.constant_term(instance, result_ty));
        }
        if instance.is::<crate::builtin::ConstantFOp>() {
            return self.float_constant_term(instance, result_ty);
        }
        let mut graph = SemGraph::new();
        let root = instance.clone().as_dyn_op().semantic_expr(&mut graph)?;
        let operands = self.operand_terms(&instance.operands);
        Some(self.lower(&graph, root, &operands))
    }

    /// An operation whose behaviour the seeder cannot spell: one opaque term for
    /// the operation, projected once per result so its results stay related.
    fn seed_unmodeled(&mut self, instance: &Arc<OpInstance>) {
        if instance.results.is_empty() {
            return;
        }
        let opaque = self.opaque();
        if let [result] = instance.results[..] {
            self.record(result, opaque);
            return;
        }
        for (index, &result) in instance.results.iter().enumerate() {
            let ty = self.context.get_value(result).ty();
            let term = self.graph.intern(
                TermKind::Project,
                TermPayload::Index(index as u32),
                Some(ty),
                &[opaque],
            );
            self.record(result, term);
        }
    }

    // ── memory ──────────────────────────────────────────────────────────────

    /// Seed a memory access, threading the state values it names. Returns
    /// whether the operation was one.
    fn seed_memory(&mut self, instance: &Arc<OpInstance>) -> bool {
        if let Some(read) = instance.clone().as_interface::<dyn MemoryRead>() {
            return self.seed_read(read.as_ref());
        }
        if let Some(write) = instance.clone().as_interface::<dyn MemoryWrite>() {
            return self.seed_write(write.as_ref());
        }
        false
    }

    /// A read: its value term, and the state it leaves as a projection of it.
    fn seed_read(&mut self, read: &dyn MemoryRead) -> bool {
        let value = read.read_value();
        let value_ty = self.context.get_value(value).ty();
        let Some(bytes) = self.byte_width(value_ty) else {
            return false;
        };
        let address = self.term_of(read.read_location());
        let bytes = self.byte_count(bytes);
        let metadata = self.int_term(minimal_unsigned_apint(0), None);
        let state = self.state_term(read.state_operand());
        let load = self.op_term(
            SymKind::LoadMemory,
            &[address, bytes, metadata, state],
            Some(value_ty),
        );
        self.record(value, load);
        if let Some(result) = read.state_result() {
            let ty = self.context.get_value(result).ty();
            let after =
                self.graph
                    .intern(TermKind::Project, TermPayload::Index(1), Some(ty), &[load]);
            self.record(result, after);
        }
        true
    }

    /// A write: the term *is* the state the accesses after it read.
    fn seed_write(&mut self, write: &dyn MemoryWrite) -> bool {
        let written = write.written_value();
        let written_ty = self.context.get_value(written).ty();
        let Some(bytes) = self.byte_width(written_ty) else {
            return false;
        };
        let address = self.term_of(write.write_location());
        let bytes = self.byte_count(bytes);
        let value = self.term_of(written);
        let address_space = self.int_term(minimal_unsigned_apint(0), None);
        let state = self.state_term(write.state_operand());
        let store = self.op_term(
            SymKind::StoreMemory,
            &[address, bytes, value, address_space, state],
            None,
        );
        if let Some(result) = write.state_result() {
            self.record(result, store);
        }
        true
    }

    /// The term of a state operand. An access naming no state reads an unknown
    /// one: a distinct opaque, so nothing merges through it.
    fn state_term(&mut self, state: Option<ValueId>) -> TermId {
        match state {
            Some(state) => self.term_of(state),
            None => self.opaque(),
        }
    }

    fn byte_width(&self, ty: TypeId) -> Option<u32> {
        let bits = type_width(self.context, ty).or_else(|| {
            let data = self.context.get_type_data(ty);
            (data.as_ref() as &dyn std::any::Any)
                .downcast_ref::<crate::ptr::PtrType>()
                .and(self.pointer_width)
        })?;
        Some(bits / 8)
    }

    fn byte_count(&mut self, bytes: u32) -> TermId {
        self.int_term(minimal_unsigned_apint(u64::from(bytes)), None)
    }

    // ── structured regions ──────────────────────────────────────────────────

    fn seed_structured(&mut self, instance: &Arc<OpInstance>) {
        if let Some(conditional) = instance.clone().as_interface::<dyn Conditional>() {
            self.seed_gamma(instance, conditional.as_ref());
            return;
        }
        if let Some(loop_like) = instance.clone().as_interface::<dyn LoopLike>() {
            let guard = instance
                .clone()
                .as_interface::<dyn GuardedLoop>()
                .map(|guarded| guarded.entry_guard());
            self.seed_theta(instance, loop_like.as_ref(), guard);
            return;
        }
        for region in instance.regions.clone() {
            self.seed_region(region);
        }
        self.seed_unmodeled(instance);
    }

    /// γ: each arm's entry arguments bind to the forwarded inputs, and result
    /// `i` is the choice between the arms' `i`-th yielded values.
    fn seed_gamma(&mut self, instance: &Arc<OpInstance>, conditional: &dyn Conditional) {
        let decision = self.term_of(conditional.decision());
        let arms: Vec<(RegionId, Option<i64>)> = conditional.case_values();
        let mut yields: Vec<Vec<TermId>> = Vec::with_capacity(arms.len());
        for &(region, _) in &arms {
            self.bind_entry_arguments(instance, region);
            self.seed_region(region);
            yields.push(
                conditional
                    .region_yields(region)
                    .iter()
                    .map(|&value| self.term_of(value))
                    .collect(),
            );
        }

        let boolean = boolean_arms(conditional);
        for (index, &result) in instance.results.iter().enumerate() {
            let ty = self.context.get_value(result).ty();
            let Some(term) = self.gamma_term(decision, &arms, &yields, index, boolean, ty) else {
                continue;
            };
            self.record(result, term);
        }
    }

    /// The choice term for one γ result: a boolean γ is one `If` on the decision
    /// itself; any other is the chain of case comparisons, default last.
    fn gamma_term(
        &mut self,
        decision: TermId,
        arms: &[(RegionId, Option<i64>)],
        yields: &[Vec<TermId>],
        index: usize,
        boolean: bool,
        ty: TypeId,
    ) -> Option<TermId> {
        let value = |arm: usize| yields.get(arm).and_then(|arm| arm.get(index).copied());
        if boolean {
            let (taken, not_taken) = (value(0)?, value(1)?);
            return Some(self.op_term(SymKind::If, &[decision, taken, not_taken], Some(ty)));
        }
        let decision_ty = self.graph.ty(decision);
        let width = decision_ty
            .and_then(|ty| integer_width(self.context, ty))
            .unwrap_or(64);
        let mut term = value(arms.len() - 1)?;
        for (arm, &(_, case)) in arms.iter().enumerate().rev() {
            let Some(case) = case else {
                continue;
            };
            let literal = self.int_term(APInt::new_signed(width, case), decision_ty);
            let matches = self.op_term(SymKind::Eq, &[decision, literal], None);
            term = self.op_term(SymKind::If, &[matches, value(arm)?, term], Some(ty));
        }
        Some(term)
    }

    /// Bind an arm's entry arguments to the inputs forwarded into it: the
    /// operation's trailing operands, one per argument.
    fn bind_entry_arguments(&mut self, instance: &Arc<OpInstance>, region: RegionId) {
        let Some(block) = self.entry_block(region) else {
            return;
        };
        let arguments: Vec<ValueId> = block.arguments().iter().map(|a| a.id()).collect();
        let first = instance.operands.len().saturating_sub(arguments.len());
        for (argument, &input) in arguments.iter().zip(&instance.operands[first..]) {
            let term = self.term_of(input);
            self.record(*argument, term);
        }
    }

    /// θ: one cyclic term per carried port, the body's reading; the loop's own
    /// results are exit projections of it, never the same term.
    fn seed_theta(
        &mut self,
        instance: &Arc<OpInstance>,
        loop_like: &dyn LoopLike,
        guard: Option<EntryGuard>,
    ) {
        let inits: Vec<TermId> = loop_like
            .inits()
            .iter()
            .map(|&value| self.term_of(value))
            .collect();

        let (entry, zero_trip) = self.entry_terms(guard.as_ref(), &inits);

        let ports: Vec<TermId> = inits
            .iter()
            .zip(loop_like.inits())
            .map(|(&init, value)| {
                let ty = self.context.get_value(value).ty();
                self.graph.mint(
                    TermKind::Op(SymKind::Theta),
                    TermPayload::None,
                    Some(ty),
                    &[init, init],
                )
            })
            .collect();

        let carried = self.forwarded_terms(guard.as_ref(), &ports);
        for (&argument, &term) in loop_like.carried_args().iter().zip(&carried) {
            self.record(argument, term);
        }
        for region in body_regions(instance, guard.as_ref()) {
            self.seed_region(region);
        }
        for (port, latch) in ports.iter().zip(loop_like.latched()) {
            let latch = self.term_of(latch);
            self.graph.set_child(*port, 1, latch);
        }
        let exits: Vec<TermId> = carried
            .iter()
            .map(|&term| {
                let ty = self.graph.ty(term);
                self.graph
                    .intern(TermKind::LoopExit, TermPayload::None, ty, &[term])
            })
            .collect();

        for (index, &result) in loop_like.finals().iter().enumerate() {
            let Some(&exit) = exits.get(index) else {
                continue;
            };
            let ty = self.context.get_value(result).ty();
            let zero_trip = zero_trip.get(index).copied().unwrap_or(inits[index]);
            let term = self.op_term(SymKind::If, &[entry, exit, zero_trip], Some(ty));
            self.record(result, term);
        }
    }

    /// The guard deciding whether the loop runs at all, and the value each port
    /// holds when it does not. A condition-region guard is read with the ports
    /// bound to the inits — the loop's first, pre-entry evaluation.
    fn entry_terms(
        &mut self,
        guard: Option<&EntryGuard>,
        inits: &[TermId],
    ) -> (TermId, Vec<TermId>) {
        match guard {
            Some(EntryGuard::Less { ordering, lhs, rhs }) => {
                let lhs = self.term_of(*lhs);
                let rhs = self.term_of(*rhs);
                let kind = match ordering {
                    GuardOrdering::Signed => SymKind::Lt,
                    GuardOrdering::Unsigned => SymKind::ULt,
                };
                (self.op_term(kind, &[lhs, rhs], None), inits.to_vec())
            }
            Some(EntryGuard::Region {
                region,
                arguments,
                condition,
            }) => {
                let watermark = self.open_transient();
                for (&argument, &init) in arguments.iter().zip(inits) {
                    self.record(argument, init);
                }
                self.seed_region(*region);
                let entry = self.term_of(*condition);
                let forwarded = self.condition_forwards(*region);
                self.close_transient(watermark);
                (entry, forwarded)
            }
            _ => (self.true_term(), inits.to_vec()),
        }
    }

    /// The port values a condition region forwards, read under whatever bindings
    /// are open. No interface exposes the forwarding: the terminator's operands
    /// carry the condition first and one port after it, in port order.
    fn condition_forwards(&mut self, region: RegionId) -> Vec<TermId> {
        let Some(block) = self.entry_block(region) else {
            return Vec::new();
        };
        let Some(&terminator) = block.op_ids().last() else {
            return Vec::new();
        };
        let operands = self.context.get_op(terminator).operands.clone();
        operands
            .iter()
            .skip(1)
            .map(|&value| self.term_of(value))
            .collect()
    }

    /// What each of the body's carried arguments holds: the port itself, or —
    /// for a condition-region loop — the value that region forwards from it.
    fn forwarded_terms(&mut self, guard: Option<&EntryGuard>, ports: &[TermId]) -> Vec<TermId> {
        let Some(EntryGuard::Region {
            region, arguments, ..
        }) = guard
        else {
            return ports.to_vec();
        };
        for (&argument, &port) in arguments.iter().zip(ports) {
            self.record(argument, port);
        }
        self.seed_region(*region);
        self.condition_forwards(*region)
    }

    // ── terms ───────────────────────────────────────────────────────────────

    /// The term of `value`: the one already seeded for it, or an anchor leaf.
    fn term_of(&mut self, value: ValueId) -> TermId {
        if let Some(term) = self.graph.term_of(value) {
            return term;
        }
        let ty = self
            .context
            .has_value(value)
            .then(|| self.context.get_value(value).ty());
        let term = self
            .graph
            .intern(TermKind::Anchor, TermPayload::Value(value), ty, &[]);
        self.record(value, term);
        term
    }

    fn operand_terms(&mut self, operands: &[ValueId]) -> Vec<TermId> {
        operands
            .iter()
            .map(|&operand| self.term_of(operand))
            .collect()
    }

    fn op_term(&mut self, kind: SymKind, children: &[TermId], ty: Option<TypeId>) -> TermId {
        let mut children = children.to_vec();
        if kind.is_commutative() {
            children.sort();
        }
        self.graph
            .intern(TermKind::Op(kind), TermPayload::None, ty, &children)
    }

    fn int_term(&mut self, value: APInt, ty: Option<TypeId>) -> TermId {
        self.graph.intern(
            TermKind::Const,
            TermPayload::Int {
                width: value.width(),
                bits: value.to_i64(),
            },
            ty,
            &[],
        )
    }

    fn true_term(&mut self) -> TermId {
        let ty = IntegerType::new(self.context, 1);
        self.int_term(APInt::new(1, 1), Some(ty))
    }

    fn opaque(&mut self) -> TermId {
        let serial = self.opaque_serial;
        self.opaque_serial += 1;
        self.graph
            .mint(TermKind::Opaque, TermPayload::Serial(serial), None, &[])
    }

    fn constant_term(&mut self, instance: &Arc<OpInstance>, ty: TypeId) -> TermId {
        match instance.attr("value") {
            Some(AttributeValue::Int(value)) => {
                let width = integer_width(self.context, ty).unwrap_or(64);
                self.int_term(APInt::new_signed(width, *value), Some(ty))
            }
            _ => self.opaque(),
        }
    }

    fn float_constant_term(&mut self, instance: &Arc<OpInstance>, ty: TypeId) -> Option<TermId> {
        let width = {
            let data = self.context.get_type_data(ty);
            (data.as_ref() as &dyn std::any::Any)
                .downcast_ref::<FloatType>()?
                .bit_width()
        };
        let AttributeValue::F64(value) = instance.attr("value")? else {
            return None;
        };
        let bits = match width {
            32 => (*value as f32).to_bits() as i32 as i64,
            64 => value.to_bits() as i64,
            _ => return None,
        };
        let int_ty = IntegerType::new(self.context, width);
        let bits = self.int_term(APInt::new_signed(64, bits), Some(int_ty));
        Some(self.op_term(SymKind::Bitcast, &[bits], Some(ty)))
    }

    // ── semantic-expression lowering ────────────────────────────────────────

    fn lower(&mut self, graph: &SemGraph, root: NodeId, operands: &[TermId]) -> TermId {
        let types = self.infer_local_types(graph, operands);
        self.lower_node(graph, root, operands, types.as_deref())
    }

    fn lower_node(
        &mut self,
        graph: &SemGraph,
        node: NodeId,
        operands: &[TermId],
        types: Option<&[SemType]>,
    ) -> TermId {
        let node_ty = graph
            .get_actual_type(node)
            .or_else(|| types.and_then(|types| ir_type(self.context, &types[node.index()])));
        match graph.get_node(node) {
            SymKind::Symbol => match graph.get_leaf_data(node) {
                Some(SymPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .copied()
                    .unwrap_or_else(|| self.opaque()),
                _ => self.opaque(),
            },
            SymKind::Constant => match graph.get_leaf_data(node) {
                Some(SymPayload::Int(value)) => self.int_term(value.clone(), node_ty),
                _ => self.opaque(),
            },
            kind => {
                let children: Vec<TermId> = graph
                    .children(node)
                    .map(|child| self.lower_node(graph, child, operands, types))
                    .collect();
                if kind.accepts_arity(children.len()) {
                    self.op_term(*kind, &children, node_ty)
                } else {
                    self.opaque()
                }
            }
        }
    }

    fn infer_local_types(&self, graph: &SemGraph, operands: &[TermId]) -> Option<Vec<SemType>> {
        infer_types(graph, |node| {
            graph
                .get_actual_type(node)
                .and_then(|ty| semantic_type(self.context, ty))
                .or_else(|| match graph.get_leaf_data(node) {
                    Some(SymPayload::SymbolId(id)) => operands
                        .get(*id as usize)
                        .and_then(|&term| self.graph.ty(term))
                        .and_then(|ty| semantic_type(self.context, ty)),
                    _ => None,
                })
        })
        .ok()
    }

    // ── binding scopes ──────────────────────────────────────────────────────

    /// Record the term of `value`. A recording made while a transient scope is
    /// open is logged, so closing the scope restores what the value held before.
    fn record(&mut self, value: ValueId, term: TermId) {
        let previous = self.graph.record_value(value, term);
        if self.transient > 0 {
            self.undo.push((value, previous));
        }
    }

    /// Open a reading whose terms are discarded: the same region read under
    /// other bindings keeps its own.
    fn open_transient(&mut self) -> usize {
        self.transient += 1;
        self.undo.len()
    }

    fn close_transient(&mut self, watermark: usize) {
        self.transient -= 1;
        while self.undo.len() > watermark {
            let (value, previous) = self.undo.pop().expect("the undo log holds the open scope");
            self.graph.forget_value(value, previous);
        }
    }
}

/// Whether the γ is the boolean one: two arms guarded by one value at opposite
/// polarities, so the decision itself selects between them.
fn boolean_arms(conditional: &dyn Conditional) -> bool {
    match conditional.guarded_regions()[..] {
        [(_, first, true), (_, second, false)] => first == second,
        _ => false,
    }
}

/// The regions a loop's body occupies: everything but the condition region a
/// guard reads, which the guard seeds itself.
fn body_regions(instance: &Arc<OpInstance>, guard: Option<&EntryGuard>) -> Vec<RegionId> {
    let condition = match guard {
        Some(EntryGuard::Region { region, .. }) => Some(*region),
        _ => None,
    };
    instance
        .regions
        .iter()
        .copied()
        .filter(|region| Some(*region) != condition)
        .collect()
}

fn integer_width(context: &Context, ty: TypeId) -> Option<u32> {
    let data = context.get_type_data(ty);
    (data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<IntegerType>()
        .map(IntegerType::width)
}
