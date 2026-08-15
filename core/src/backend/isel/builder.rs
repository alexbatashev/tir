//! Lowering of a function's IR operations into the semantic e-graph.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tir::{
    BlockId, Conditional, Context, EntryGuard, GuardedLoop, LoopLike, MemoryRead, MemoryWrite,
    OpId, OpInstance, RegionId, TokenScope, TypeId, ValueId,
    attributes::AttributeValue,
    builtin::{FloatType, IntegerType},
    graph::{Dag, MetaDag, NodeId},
    sem::{
        SemGraph, SemNode, SemPayload, SemType, SymKind, SymPayload,
        egraph::{SemEGraph, ir_type, minimal_unsigned_apint, semantic_type, type_width},
        infer_types, template_node,
    },
};
use tir_adt::APInt;
use tir_symbolic::egraph::Id;

use crate::analysis::scopes::{carried_operands, port_edges, region_exit};

use super::node::class_is_pure;

/// Whether the seeder reads a region-carrying operation's regions instead of
/// leaving it to seed a graph of its own. Bring-up flag for the structured-input
/// path: the CFG path is the default until the contract flips.
pub(crate) fn regions_enabled() -> bool {
    std::env::var_os("TIR_ISEL_REGIONS").is_some()
}

/// What a walk records for the cover: the class each operation is rooted at, and
/// the float constants a target materializer could build.
#[derive(Default)]
pub(crate) struct Seeds {
    pub(crate) roots_by_op: HashMap<OpId, Id>,
    pub(crate) constant_candidates: Vec<(OpId, Id)>,
}

/// Builds a block's semantic expressions straight into the e-graph: every lowered
/// node is hash-consed by [`SemEGraph::add`], so the e-graph *is* the interned DAG
/// (no separate arena). Returns e-class [`Id`]s and records, in `value_to_class`,
/// the class built for each IR value so operands share and cross-block uses expand.
pub(crate) struct SemDagBuilder<'a> {
    context: &'a Context,
    value_to_def: &'a HashMap<ValueId, OpId>,
    egraph: &'a mut SemEGraph,
    pointer_width: Option<u32>,
    /// The e-class built for each already-lowered IR value (operand sharing / CSE).
    pub(crate) value_to_class: HashMap<ValueId, Id>,
    /// Serial of the next opaque leaf; each un-lowerable node gets its own.
    opaque_serial: u32,
    /// The state chain the next memory access reads, or `None` where it is
    /// unknown and the next access mints a fresh one ([`Self::state`]).
    state: Option<Id>,
}

impl<'a> SemDagBuilder<'a> {
    pub(crate) fn new(
        context: &'a Context,
        value_to_def: &'a HashMap<ValueId, OpId>,
        egraph: &'a mut SemEGraph,
        pointer_width: Option<u32>,
    ) -> Self {
        Self {
            context,
            value_to_def,
            egraph,
            pointer_width,
            value_to_class: HashMap::new(),
            opaque_serial: 0,
            state: None,
        }
    }

    /// Lower `blocks` into the graph — and, under the region path, every region
    /// their operations carry.
    pub(crate) fn build_blocks(
        &mut self,
        blocks: &[BlockId],
        float_widths: &HashSet<u32>,
        regions: bool,
    ) -> Seeds {
        let mut seeds = Seeds::default();
        for &block in blocks {
            // Blocks are lowered in region order, which is not an execution
            // order: what ran before a block is a join the straight-line chain
            // does not model, so each block reads a fresh unknown state.
            self.break_state();
            self.build_block(block, float_widths, regions, &mut seeds);
        }
        seeds
    }

    fn build_block(
        &mut self,
        block: BlockId,
        float_widths: &HashSet<u32>,
        regions: bool,
        seeds: &mut Seeds,
    ) {
        for op_id in self.context.get_block(block).op_ids() {
            let op = self.context.get_op(op_id);
            if regions && !op.regions.is_empty() {
                self.build_region_op(&op, float_widths, seeds);
            } else {
                self.build_plain_op(op_id, &op, float_widths, seeds);
            }
        }
    }

    fn build_region(&mut self, region: RegionId, float_widths: &HashSet<u32>, seeds: &mut Seeds) {
        let blocks: Vec<BlockId> = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .map(|block| block.id())
            .collect();
        for block in blocks {
            self.break_state();
            self.build_block(block, float_widths, true, seeds);
        }
    }

    /// Lower one region-free operation, recording the class it is rooted at. A
    /// standalone constant has no semantic root of its own, so it is rooted at the
    /// class its value builds; a float one only where the target declares a
    /// materializer for its width.
    fn build_plain_op(
        &mut self,
        op_id: OpId,
        op: &Arc<OpInstance>,
        float_widths: &HashSet<u32>,
        seeds: &mut Seeds,
    ) {
        if let Some(root) = self.build_for_op(op).or_else(|| {
            op.is::<crate::builtin::ConstantOp>()
                .then(|| self.build_from_value(op.results[0]))
        }) {
            seeds.roots_by_op.insert(op_id, root);
        } else if op.is::<crate::builtin::ConstantFOp>()
            && let Some(&result) = op.results.first()
            && self
                .float_width(result)
                .is_some_and(|width| float_widths.contains(&width))
        {
            let class = self.build_from_value(result);
            seeds.constant_candidates.push((op_id, class));
        }
    }

    fn float_width(&self, value: ValueId) -> Option<u32> {
        let ty = self.context.get_value(value).ty();
        let data = self.context.get_type_data(ty);
        (data.as_ref() as &dyn std::any::Any)
            .downcast_ref::<FloatType>()
            .map(FloatType::bit_width)
    }

    /// A region-carrying operation is read through its own interfaces: its regions
    /// join this graph, and what it publishes is the γ over what its arms yield or
    /// the θ over what its edges carry. Its regions may have written anything the
    /// straight-line chain cannot spell, so the chain breaks across it.
    fn build_region_op(
        &mut self,
        op: &Arc<OpInstance>,
        float_widths: &HashSet<u32>,
        seeds: &mut Seeds,
    ) {
        if let Some(conditional) = op.clone().as_interface::<dyn Conditional>() {
            for region in op.regions.clone() {
                self.bind_region_arguments(op, region);
                self.build_region(region, float_widths, seeds);
            }
            self.break_state();
            self.seed_gamma(op, conditional.as_ref());
            return;
        }
        if let Some(loop_like) = op.clone().as_interface::<dyn LoopLike>() {
            self.seed_theta(op, loop_like.as_ref(), float_widths, seeds);
            return;
        }
        for region in op.regions.clone() {
            self.build_region(region, float_widths, seeds);
        }
        self.break_state();
    }

    /// γ: what a gate publishes is the choice between what its arms yield, one
    /// child per case in the order the interface reports them — the order a
    /// destruction maps back onto the regions.
    ///
    /// An arm leaving the enclosing loop never reaches what follows the gate, so a
    /// gate one arm leaves through publishes what the arm that stays yields, and
    /// one every arm leaves through publishes nothing the graph can name.
    fn seed_gamma(&mut self, op: &Arc<OpInstance>, conditional: &dyn Conditional) {
        let decision = self.build_from_value(conditional.decision());
        let cases = conditional.case_values();
        let arms: Vec<RegionId> = cases
            .iter()
            .map(|&(region, _)| region)
            .filter(|&region| region_exit(self.context, region).is_none())
            .collect();
        for (index, &result) in op.results.clone().iter().enumerate() {
            let yields: Option<Vec<ValueId>> = arms
                .iter()
                .map(|&region| conditional.region_yields(region).get(index).copied())
                .collect();
            let class = match yields.as_deref() {
                Some(&[value]) => self.build_from_value(value),
                Some(values) if values.len() == cases.len() => {
                    let mut args = vec![decision];
                    for &value in values {
                        args.push(self.build_from_value(value));
                    }
                    let ty = self.context.get_value(result).ty();
                    let gamma = self.egraph.add(SemNode::gamma(result, args).typed(ty));
                    // The gate's own value joins the choice: the cover may read the
                    // gate as the register its regions leave it in, whatever it can
                    // do with the arms' terms.
                    let anchor = self.add_input_value(result, Some(ty));
                    self.egraph.union(gamma, anchor)
                }
                _ => continue,
            };
            self.value_to_class.insert(result, class);
        }
    }

    /// An arm's entry arguments are the inputs the gate forwards into it: the
    /// operation's trailing operands, one per argument.
    fn bind_region_arguments(&mut self, op: &Arc<OpInstance>, region: RegionId) {
        let Some(block) = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .next()
        else {
            return;
        };
        let arguments: Vec<ValueId> = block.arguments().iter().map(|a| a.id()).collect();
        let first = op.operands.len().saturating_sub(arguments.len());
        for (&argument, &input) in arguments.iter().zip(&op.operands[first..]) {
            let class = self.build_from_value(input);
            self.value_to_class.insert(argument, class);
        }
    }

    /// θ: what a loop carries in a port is the value it was entered with and the
    /// one each edge back into the port carries — the body's latch, and every
    /// `break`/`continue` leaving its scope, in the one order `port_edges` reports.
    /// The port's argument anchors first, so the regions can be read on it, and the
    /// θ joins that class after: an edge is a term over the argument itself.
    ///
    /// A loop whose quad does not line the ports up — `scf.for`, whose body carries
    /// an induction variable no init names — anchors instead.
    fn seed_theta(
        &mut self,
        op: &Arc<OpInstance>,
        loop_like: &dyn LoopLike,
        float_widths: &HashSet<u32>,
        seeds: &mut Seeds,
    ) {
        let carried = loop_like.carried_args();
        let inits = loop_like.inits();
        let tested = self.tested_values(op, carried.len());
        let heads = match &tested {
            Some((_, arguments, _)) => arguments.clone(),
            None => carried.clone(),
        };
        for &head in &heads {
            self.build_from_value(head);
        }
        // The body reads what the test forwards into it, so the test's region is
        // read first and its forwarded values name the body's arguments.
        if let Some((region, _, forwarded)) = tested.clone() {
            self.build_region(region, float_widths, seeds);
            for (&argument, &value) in carried.iter().zip(&forwarded) {
                let class = self.build_from_value(value);
                self.value_to_class.insert(argument, class);
            }
        }
        for region in op.regions.clone() {
            if tested
                .as_ref()
                .is_some_and(|(tested, ..)| *tested == region)
            {
                continue;
            }
            self.build_region(region, float_widths, seeds);
        }
        self.break_state();

        let edges: Vec<Arc<OpInstance>> = op
            .clone()
            .as_interface::<dyn TokenScope>()
            .into_iter()
            .flat_map(|scope| scope.token_scope_regions())
            .flat_map(|body| port_edges(self.context, body))
            .map(|edge| self.context.get_op(edge))
            .collect();
        if edges.is_empty() || inits.len() != heads.len() {
            return;
        }
        for port in 0..heads.len() {
            let Some(latched) = self.latched_values(&edges, port, carried.len()) else {
                continue;
            };
            let init = self.build_from_value(inits[port]);
            let mut args = vec![init];
            for value in latched {
                let class = self.build_from_value(value);
                args.push(class);
            }
            let head = self.build_from_value(heads[port]);
            let theta = self.egraph.add(SemNode::theta(heads[port], args));
            self.egraph.union(theta, head);
        }
        self.egraph.rebuild();
    }

    /// The value each edge carries into the `port`-th of a loop's `ports` carried
    /// ports. `None` where an edge carries too few to name it.
    fn latched_values(
        &self,
        edges: &[Arc<OpInstance>],
        port: usize,
        ports: usize,
    ) -> Option<Vec<ValueId>> {
        edges
            .iter()
            .map(|edge| {
                let carried = carried_operands(edge);
                (carried.len() == ports).then(|| carried[port])
            })
            .collect()
    }

    /// The region a loop evaluates before each iteration, the arguments it reads
    /// the carried values as, and the values it forwards into the body — its
    /// terminator's trailing operands, one per port. `None` for a loop that tests
    /// nothing it carries.
    ///
    /// Read through the interfaces rather than through an `scf.while` accessor: the
    /// mapping body argument ↔ condition operand ↔ result is what `GuardedLoop`
    /// and the terminator already say between them, and an accessor would be a
    /// second source of truth for it.
    fn tested_values(
        &self,
        op: &Arc<OpInstance>,
        ports: usize,
    ) -> Option<(RegionId, Vec<ValueId>, Vec<ValueId>)> {
        let guard = op.clone().as_interface::<dyn GuardedLoop>()?;
        let EntryGuard::Region {
            region, arguments, ..
        } = guard.entry_guard()
        else {
            return None;
        };
        if arguments.len() != ports {
            return None;
        }
        let block = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .next()?;
        let operands = &self.context.get_op(*block.op_ids().last()?).operands;
        let first = operands.len().checked_sub(ports)?;
        Some((region, arguments, operands[first..].to_vec()))
    }

    /// The chain the next memory access reads. An unknown chain is an opaque
    /// leaf: nothing merges through it, so the accesses after one are still
    /// terms over *something*.
    fn state(&mut self) -> Id {
        match self.state {
            Some(state) => state,
            None => {
                let state = self.add_opaque();
                self.state = Some(state);
                state
            }
        }
    }

    /// Forget the current chain, so the next access reads a fresh unknown one.
    /// The state edges this builder threads are straight-line: a block boundary
    /// is a join the linear order does not model, and an operation whose effect
    /// the builder cannot spell may have written anything.
    pub(crate) fn break_state(&mut self) {
        self.state = None;
    }

    fn add_leaf(
        &mut self,
        kind: SymKind,
        payload: Option<SymPayload<ValueId>>,
        ty: Option<TypeId>,
    ) -> Id {
        self.egraph.add(template_node(kind, payload, ty))
    }

    fn next_opaque_serial(&mut self) -> u32 {
        let serial = self.opaque_serial;
        self.opaque_serial += 1;
        serial
    }

    fn add_int(&mut self, value: APInt, ty: Option<TypeId>) -> Id {
        self.add_leaf(SymKind::Constant, Some(SymPayload::Int(value)), ty)
    }

    fn add_u64_const(&mut self, value: u64) -> Id {
        self.add_int(minimal_unsigned_apint(value), None)
    }

    /// Add the `addr + 0` form used by base+offset addressing patterns and record
    /// its exact equality with the bare address used by direct-base patterns.
    ///
    /// The equality is recorded only for pure addresses: an effectful address
    /// (e.g. a loaded pointer) must keep its effect node as the class's sole
    /// materialization — unioning in an arithmetic view would let the cover pick
    /// that view and leave the effect with no rule to materialize it.
    fn zero_offset_address(&mut self, address: Id) -> Id {
        let zero = self.add_u64_const(0);
        let with_zero = self.add_op(SymKind::Add, vec![address, zero], None);
        if class_is_pure(self.egraph, address) {
            self.egraph.union(with_zero, address)
        } else {
            with_zero
        }
    }

    fn add_input_value(&mut self, value: ValueId, ty: Option<TypeId>) -> Id {
        self.add_leaf(SymKind::Symbol, Some(SymPayload::Value(value)), ty)
    }

    fn add_unknown_symbol(&mut self, symbol: u32, ty: Option<TypeId>) -> Id {
        self.add_leaf(SymKind::Symbol, Some(SymPayload::SymbolId(symbol)), ty)
    }

    /// A leaf that nothing materializes — the placeholder for an un-lowerable node,
    /// so a partial semantic expansion still yields a well-formed graph. Each call
    /// mints a distinct leaf: two unknown computations are never assumed equal.
    pub(crate) fn add_opaque(&mut self) -> Id {
        let serial = self.next_opaque_serial();
        let mut node = template_node(SymKind::Symbol, None, None);
        node.payload = Some(SemPayload::Opaque(serial));
        self.egraph.add(node)
    }

    /// Build an operator node, canonicalizing commutative operands so `a op b` and
    /// `b op a` hash-cons to the same e-node (mirroring the program's CSE).
    fn add_op(&mut self, kind: SymKind, mut children: Vec<Id>, ty: Option<TypeId>) -> Id {
        if kind.is_commutative() {
            children.sort();
        }
        let mut node = template_node(kind, None, ty);
        node.children = children;
        self.egraph.add(node)
    }

    pub(crate) fn build_for_op(&mut self, op: &std::sync::Arc<OpInstance>) -> Option<Id> {
        // A standalone `constantf` is left for the target's pre-RA hook, like a
        // bare integer `constant`; only as an operand (see `build_from_value`)
        // does it fold into a consumer via `float_constant_class`.
        if op.is::<crate::builtin::ConstantFOp>() {
            return None;
        }
        let class = if let Some(class) = self.build_memory_effect(op) {
            class
        } else {
            let operands = self.build_operands(&op.operands);
            let mut graph = SemGraph::new();
            let Some(root) = op.clone().as_dyn_op().semantic_expr(&mut graph) else {
                // Nothing says what this operation does, so nothing says it left
                // memory alone: the chain it may have written is not one the
                // accesses after it may read through.
                if !op.is::<crate::builtin::ConstantOp>() {
                    self.break_state();
                }
                return None;
            };
            let class = self.lower_typed(&graph, root, &operands);
            if !class_is_pure(self.egraph, class) {
                self.break_state();
            }
            class
        };
        for result in &op.results {
            self.value_to_class.insert(*result, class);
        }
        Some(class)
    }

    fn build_operands(&mut self, operands: &[ValueId]) -> Vec<Id> {
        operands
            .iter()
            .map(|&operand| self.build_from_value(operand))
            .collect()
    }

    fn lower_typed(&mut self, graph: &SemGraph, root: NodeId, operands: &[Id]) -> Id {
        let types = self.infer_local_types(graph, operands);
        self.lower_graph_node(graph, root, operands, types.as_deref())
    }

    fn build_memory_effect(&mut self, op: &std::sync::Arc<OpInstance>) -> Option<Id> {
        let read_parts = op
            .clone()
            .as_interface::<dyn MemoryRead>()
            .map(|read| (read.read_location(), read.read_value()));

        if let Some((location, result)) = read_parts {
            let result_ty = self.context.get_value(result).ty();
            let bytes = self.type_width(result_ty)? / 8;
            let address = self.build_from_value(location);
            let address = self.zero_offset_address(address);
            let bytes = self.add_u64_const(u64::from(bytes));
            let metadata = self.add_u64_const(0);
            let state = self.state();
            // A read leaves memory as it found it, so the chain runs on
            // unchanged; two reads of one address on one chain are one term.
            return Some(self.add_op(
                SymKind::LoadMemory,
                vec![address, bytes, metadata, state],
                Some(result_ty),
            ));
        }

        let write_parts = op
            .clone()
            .as_interface::<dyn MemoryWrite>()
            .map(|write| (write.write_location(), write.written_value()));

        if let Some((location, value)) = write_parts {
            let value_ty = self.context.get_value(value).ty();
            let bytes = self.type_width(value_ty)? / 8;
            let address = self.build_from_value(location);
            let address = self.zero_offset_address(address);
            let bytes = self.add_u64_const(u64::from(bytes));
            let value = self.build_from_value(value);
            let address_space = self.add_u64_const(0);
            let state = self.state();
            let store = self.add_op(
                SymKind::StoreMemory,
                vec![address, bytes, value, address_space, state],
                None,
            );
            // The write takes memory to a state nothing before it names, so the
            // term *is* the chain the accesses after it read.
            self.state = Some(store);
            return Some(store);
        }

        None
    }

    fn type_width(&self, ty: TypeId) -> Option<u32> {
        type_width(self.context, ty).or_else(|| {
            let data = self.context.get_type_data(ty);
            (data.as_ref() as &dyn std::any::Any)
                .downcast_ref::<tir::ptr::PtrType>()
                .and(self.pointer_width)
        })
    }

    /// The canonical comparison the (possibly cross-block) definer of `value`
    /// computes, lowered into this graph: `(class, kind, lhs class, rhs class)`.
    /// `None` when the definer is unknown, effectful, or not a comparison. Used
    /// by dominating-edge assumptions to relate a guard condition to the values
    /// it compares.
    pub(crate) fn build_defining_compare(
        &mut self,
        value: ValueId,
    ) -> Option<(Id, SymKind, Id, Id)> {
        let def_id = self.context.get_value(value).defining_op()?;
        if !self.context.has_operation(def_id) {
            return None;
        }
        let def = self.context.get_op(def_id);
        if def.results.len() != 1
            || def.clone().as_interface::<dyn MemoryRead>().is_some()
            || def.clone().as_interface::<dyn MemoryWrite>().is_some()
        {
            return None;
        }
        let mut graph = SemGraph::new();
        let root = def.clone().as_dyn_op().semantic_expr(&mut graph)?;
        let operands = self.build_operands(&def.operands);
        let class = self.lower_typed(&graph, root, &operands);
        let comparison = self
            .egraph
            .nodes(class)
            .iter()
            .find(|n| n.sym().is_some_and(tir::sem::egraph::is_comparison))?
            .clone();
        Some((
            class,
            comparison
                .sym()
                .expect("a comparison node is a semantic operator"),
            comparison.children[0],
            comparison.children[1],
        ))
    }

    pub(crate) fn build_from_value(&mut self, value: ValueId) -> Id {
        if let Some(existing) = self.value_to_class.get(&value) {
            return *existing;
        }

        let value_ty = Some(self.context.get_value(value).ty());
        let class = if let Some(def_op_id) = self.value_to_def.get(&value).copied() {
            let def = self.context.get_op(def_op_id);
            if def.is::<crate::builtin::ConstantOp>() {
                self.constant_class(&def, value, value_ty)
            } else if def.is::<crate::builtin::ConstantFOp>() {
                self.float_constant_class(&def)
                    .unwrap_or_else(|| self.add_input_value(value, value_ty))
            } else {
                let mut graph = SemGraph::new();
                if let Some(root) = def.clone().as_dyn_op().semantic_expr(&mut graph) {
                    let operands = self.build_operands(&def.operands);
                    self.lower_typed(&graph, root, &operands)
                } else {
                    self.add_input_value(value, value_ty)
                }
            }
        } else {
            self.add_input_value(value, value_ty)
        };

        self.value_to_class.insert(value, class);
        class
    }

    /// Lower a `constant` op to an integer-literal leaf, or to an input value when its
    /// payload is not an integer.
    fn constant_class(
        &mut self,
        def: &std::sync::Arc<OpInstance>,
        value: ValueId,
        value_ty: Option<TypeId>,
    ) -> Id {
        match def.attr("value") {
            Some(AttributeValue::Int(v)) => {
                let width = value_ty
                    .and_then(|ty| {
                        let ty = self.context.get_type_data(ty);
                        (ty.as_ref() as &dyn std::any::Any)
                            .downcast_ref::<tir::builtin::IntegerType>()
                            .map(tir::builtin::IntegerType::width)
                    })
                    .unwrap_or(64);
                self.add_int(APInt::new_signed(width, *v), value_ty)
            }
            _ => self.add_input_value(value, value_ty),
        }
    }

    fn float_constant_class(&mut self, def: &std::sync::Arc<OpInstance>) -> Option<Id> {
        let &result = def.results.first()?;
        let result_ty = self.context.get_value(result).ty();
        let width = {
            let data = self.context.get_type_data(result_ty);
            (data.as_ref() as &dyn std::any::Any)
                .downcast_ref::<FloatType>()?
                .bit_width()
        };
        let value = match def.attr("value")? {
            AttributeValue::F64(value) => *value,
            _ => return None,
        };
        let bits = match width {
            32 => (value as f32).to_bits() as i32 as i64,
            64 => value.to_bits() as i64,
            _ => return None,
        };
        let int_ty = IntegerType::new(self.context, width);
        let bits = self.add_int(APInt::new_signed(64, bits), Some(int_ty));
        Some(self.add_op(SymKind::Bitcast, vec![bits], Some(result_ty)))
    }

    fn infer_local_types(&self, graph: &SemGraph, operands: &[Id]) -> Option<Vec<SemType>> {
        infer_types(graph, |node| {
            graph
                .get_actual_type(node)
                .and_then(|ty| semantic_type(self.context, ty))
                .or_else(|| match graph.get_leaf_data(node) {
                    Some(SymPayload::SymbolId(id)) => operands
                        .get(*id as usize)
                        .and_then(|&class| self.class_ty(class))
                        .and_then(|ty| semantic_type(self.context, ty)),
                    _ => None,
                })
        })
        .ok()
    }

    /// The IR type recorded on an operand class (taken from any member carrying one).
    fn class_ty(&self, class: Id) -> Option<TypeId> {
        self.egraph.nodes(class).iter().find_map(|n| n.ty)
    }

    fn lower_graph_node(
        &mut self,
        graph: &SemGraph,
        node: NodeId,
        operands: &[Id],
        types: Option<&[SemType]>,
    ) -> Id {
        let node_ty = graph
            .get_actual_type(node)
            .or_else(|| types.and_then(|types| ir_type(self.context, &types[node.index()])));
        match graph.get_node(node) {
            SymKind::Symbol => match graph.get_leaf_data(node) {
                Some(SymPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .copied()
                    .unwrap_or_else(|| self.add_unknown_symbol(*id, node_ty)),
                _ => self.add_opaque(),
            },
            SymKind::Constant => match graph.get_leaf_data(node) {
                Some(SymPayload::Int(v)) => self.add_int(v.clone(), node_ty),
                _ => self.add_opaque(),
            },
            kind => {
                let children: Vec<Id> = graph
                    .children(node)
                    .map(|child| self.lower_graph_node(graph, child, operands, types))
                    .collect();
                if kind.accepts_arity(children.len()) {
                    self.add_op(*kind, children, node_ty)
                } else {
                    self.add_opaque()
                }
            }
        }
    }
}
