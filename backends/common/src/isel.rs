use std::collections::{HashMap, HashSet};

use tir::{
    Block, BlockId, Context, OpId, OpInstance, Operation, OperationRef, Pass, PassError,
    PassTarget, Rewriter, TypeId, ValueId,
    attributes::AttributeValue,
    graph::{
        CoverLegality, Dag, Node, NodeId, OperandConstraint, Pattern, PatternExpr, VF2CoverDriver,
    },
    builtin::IntegerType,
    pbqp::{self, INF_COST, PbqpAlternative, PbqpMatrix, PbqpProblem},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph, infer_widths},
    utils::APInt,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SemNodeId(u32);

impl SemNodeId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug)]
pub struct SemNode {
    pub id: SemNodeId,
    pub kind: ExprKind,
    pub inputs: Vec<SemNodeId>,
    pub payload: Option<ExprPayload>,
    /// The IR type of the value this node represents (the result type for an op
    /// node, the value type for a leaf). `None` on a *pattern* node means "match
    /// any type"; `None` on a *graph* node means the type is unknown (e.g. an
    /// intermediate node of a multi-node semantic expansion).
    ///
    /// The type is stored verbatim from the IR — no width is collapsed or
    /// normalized — so every target can constrain on exactly the widths/classes it
    /// distinguishes (x86/AArch64 8/16/32/64-bit forms, RISC-V word vs XLEN, vector
    /// element types, floats), and untyped rules stay width-agnostic.
    pub ty: Option<TypeId>,
    /// The IR value this node produces, if it corresponds to one (an op result or a
    /// block input). Lets an operand that resolves to an intermediate result be
    /// materialized as that register value at emit time. Metadata only — not part
    /// of equality or matching.
    pub value: Option<ValueId>,
}

impl PartialEq for SemNode {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.payload == other.payload && self.ty == other.ty
    }
}

impl tir::graph::Node for SemNode {
    fn is_leaf(&self, ctx: &Context) -> bool {
        self.kind.is_leaf(ctx)
    }

    fn num_children(&self, ctx: &Context) -> usize {
        self.kind.num_children(ctx)
    }

    fn is_commutative(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::Add | ExprKind::Mul | ExprKind::And | ExprKind::Or | ExprKind::Xor
        )
    }

    fn is_constant(&self) -> bool {
        self.kind == ExprKind::Constant
    }

    fn matches_pattern(&self, pattern: &Self, _ctx: &Context) -> bool {
        if self.kind != pattern.kind {
            return false;
        }

        // A typed pattern node only matches a graph node of exactly that type;
        // an untyped pattern node (`ty == None`) is a type wildcard.
        if pattern.ty.is_some() && self.ty != pattern.ty {
            return false;
        }

        match (&self.payload, &pattern.payload) {
            (_, None) => true,
            (Some(actual), Some(expected)) => actual == expected,
            (None, Some(_)) => false,
        }
    }
}

impl SemNode {
    fn is_terminal(&self) -> bool {
        self.inputs.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompiledRuleId(u32);

#[derive(Debug, Clone)]
pub struct RuleMatch {
    int_bindings: Vec<(u32, APInt)>,
    value_bindings: Vec<(u32, ValueId)>,
}

impl RuleMatch {
    fn new(mut int_bindings: Vec<(u32, APInt)>, mut value_bindings: Vec<(u32, ValueId)>) -> Self {
        int_bindings.sort_by_key(|(sym, _)| *sym);
        value_bindings.sort_by_key(|(sym, _)| *sym);
        Self {
            int_bindings,
            value_bindings,
        }
    }

    pub fn value_binding(&self, symbol: u32) -> Option<ValueId> {
        self.value_bindings
            .iter()
            .find(|(sym, _)| *sym == symbol)
            .map(|(_, v)| *v)
    }

    pub fn int_binding(&self, symbol: u32) -> Option<i64> {
        self.int_bindings
            .iter()
            .find(|(sym, _)| *sym == symbol)
            .map(|(_, v)| v.to_u64() as i64)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SelectionPressure {
    pub estimated_live_operands: u32,
    pub estimated_register_pressure: u32,
}

pub struct EmitPlan {
    op: Box<dyn Operation>,
}

impl EmitPlan {
    pub fn single(op: Box<dyn Operation>) -> Self {
        Self { op }
    }

    fn into_op(self) -> Box<dyn Operation> {
        self.op
    }
}

pub trait TargetIselModel: Send + Sync {
    fn subtarget(&self) -> &'static str {
        "generic"
    }

    fn is_pbqp_enabled(&self) -> bool {
        true
    }

    fn supports_rule(&self, _rule_id: CompiledRuleId) -> bool {
        true
    }

    fn estimate_register_pressure(&self, op: &OperationRef) -> u32 {
        op.op().operands.len() as u32
    }

    /// The objective consulted by the PBQP builder for node and edge costs.
    ///
    /// Targets that want to tune selection (subtarget-specific instruction
    /// costs, register-pressure weighting, materialization penalties) return a
    /// custom [`IselCostModel`]. The default reproduces the historical
    /// `base_cost + dynamic_cost_fn` behavior.
    fn cost_model(&self) -> &dyn IselCostModel {
        &DEFAULT_COST_MODEL
    }
}

pub struct DefaultTargetIselModel;

impl TargetIselModel for DefaultTargetIselModel {}

/// The optimization objective the PBQP builder minimizes.
///
/// `node_cost` is the full instruction cost placed on the *root* alternative of
/// a pattern match (non-root alternatives carry zero, per the paper). `edge_cost`
/// adds a compatibility cost to *finite* parent -> child edges, letting a target
/// price, e.g., materializing a value into a register. `pressure_weight` scales
/// the estimated register pressure into the node cost.
pub trait IselCostModel: Send + Sync {
    fn node_cost(
        &self,
        context: &Context,
        op: &OperationRef,
        rule: &Rule,
        m: &RuleMatch,
        pressure: &SelectionPressure,
        target: &dyn TargetIselModel,
    ) -> u64 {
        let dynamic = (rule.dynamic_cost_fn)(context, op, m, pressure, target) as u64;
        rule.base_cost as u64
            + dynamic
            + self.pressure_weight() * pressure.estimated_register_pressure as u64
    }

    /// Extra cost for a satisfied parent -> child compatibility edge. `materialized`
    /// is true when the parent reaches the child through an untyped boundary leaf,
    /// i.e. the child must be available as a register value.
    fn edge_cost(&self, _parent: ExprKind, _child: ExprKind, _materialized: bool) -> u64 {
        0
    }

    /// Weight applied to the estimated register pressure of a match. Zero (the
    /// default) ignores pressure entirely, matching historical behavior.
    fn pressure_weight(&self) -> u64 {
        0
    }
}

pub struct DefaultIselCostModel;

impl IselCostModel for DefaultIselCostModel {}

static DEFAULT_COST_MODEL: DefaultIselCostModel = DefaultIselCostModel;

pub type RuleLegalityFn = fn(&Context, &OperationRef, &RuleMatch, &dyn TargetIselModel) -> bool;
pub type RuleDynamicCostFn =
    fn(&Context, &OperationRef, &RuleMatch, &SelectionPressure, &dyn TargetIselModel) -> u32;
pub type RuleEmitPlanFn = fn(&Context, &OperationRef, &RuleMatch) -> Result<EmitPlan, PassError>;

fn default_legality(
    _context: &Context,
    _op: &OperationRef,
    _m: &RuleMatch,
    _target: &dyn TargetIselModel,
) -> bool {
    true
}

fn default_dynamic_cost(
    _context: &Context,
    _op: &OperationRef,
    _m: &RuleMatch,
    _pressure: &SelectionPressure,
    _target: &dyn TargetIselModel,
) -> u32 {
    0
}

pub struct Rule {
    pub name: &'static str,
    pub pattern: ExprPostGraph,
    pub compiled_rule_id: CompiledRuleId,
    pub base_cost: u32,
    /// Per-operand-symbol constraint (register vs immediate). Symbols absent here
    /// are unconstrained, so hand-written and synthesized rules keep matching any
    /// value.
    pub operand_constraints: Vec<(u32, OperandConstraint)>,
    pub legality_fn: RuleLegalityFn,
    pub dynamic_cost_fn: RuleDynamicCostFn,
    pub emit_plan_fn: RuleEmitPlanFn,
}

impl Rule {
    pub fn new(
        name: &'static str,
        pattern: ExprPostGraph,
        base_cost: u32,
        emit_plan_fn: RuleEmitPlanFn,
    ) -> Self {
        Self {
            name,
            pattern,
            compiled_rule_id: CompiledRuleId(0),
            base_cost,
            operand_constraints: Vec::new(),
            legality_fn: default_legality,
            dynamic_cost_fn: default_dynamic_cost,
            emit_plan_fn,
        }
    }

    /// Constrain operand symbols to register or immediate operands, so e.g. an
    /// immediate-shift pattern only matches a constant shift amount.
    pub fn with_operand_constraints(
        mut self,
        constraints: Vec<(u32, OperandConstraint)>,
    ) -> Self {
        self.operand_constraints = constraints;
        self
    }

    pub fn with_legality(mut self, legality_fn: RuleLegalityFn) -> Self {
        self.legality_fn = legality_fn;
        self
    }

    pub fn with_dynamic_cost(mut self, dynamic_cost_fn: RuleDynamicCostFn) -> Self {
        self.dynamic_cost_fn = dynamic_cost_fn;
        self
    }
}

#[derive(Clone, Debug)]
struct CaptureBindings {
    entries: Vec<(u32, SemNodeId)>,
}

impl CaptureBindings {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn bind(&mut self, symbol: u32, node: SemNodeId) -> bool {
        if let Some((_, existing)) = self.entries.iter().find(|(sym, _)| *sym == symbol) {
            *existing == node
        } else {
            self.entries.push((symbol, node));
            true
        }
    }

    fn to_rule_match(&self, dag: &SemDagArena) -> RuleMatch {
        let mut int_bindings = Vec::new();
        let mut value_bindings = Vec::new();
        for (sym, node_id) in &self.entries {
            let node = dag.node(*node_id);
            match node.payload.as_ref() {
                Some(ExprPayload::Int(v)) => int_bindings.push((*sym, v.clone())),
                Some(ExprPayload::Value(v)) => value_bindings.push((*sym, *v)),
                // An operand that resolves to an intermediate result: bind it to the
                // value that result produces, so emit can read it as a register.
                _ => {
                    if let Some(v) = node.value {
                        value_bindings.push((*sym, v));
                    }
                }
            }
        }
        RuleMatch::new(int_bindings, value_bindings)
    }
}

#[derive(Clone, Debug)]
struct PatternNodeBinding {
    pattern_node: NodeId,
    sem_node: SemNodeId,
    is_boundary: bool,
}

#[derive(Clone, Debug)]
struct FullMatchBindings {
    captures: CaptureBindings,
    pattern_nodes: Vec<PatternNodeBinding>,
}

impl FullMatchBindings {
    fn sem_node_for_pattern(&self, pattern_node: NodeId) -> Option<SemNodeId> {
        self.pattern_nodes
            .iter()
            .find(|binding| binding.pattern_node == pattern_node)
            .map(|binding| binding.sem_node)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SemKey {
    Int {
        width: u32,
        signed: bool,
        value: u64,
        ty: Option<TypeId>,
    },
    Input(u32),
    UnknownSymbol(u32),
    Op {
        kind: ExprKind,
        inputs: Vec<SemNodeId>,
        ty: Option<TypeId>,
    },
    Opaque,
}

pub struct SemDagArena {
    nodes: Vec<SemNode>,
    interner: HashMap<SemKey, SemNodeId>,
}

impl SemDagArena {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            interner: HashMap::new(),
        }
    }

    pub fn node(&self, id: SemNodeId) -> &SemNode {
        &self.nodes[id.0 as usize]
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn iter(&self) -> impl Iterator<Item = (SemNodeId, &SemNode)> {
        self.nodes.iter().enumerate().map(|(idx, node)| {
            let id = SemNodeId(idx as u32);
            (id, node)
        })
    }

    fn intern_with_key(&mut self, key: SemKey, mut node: SemNode) -> SemNodeId {
        if let Some(id) = self.interner.get(&key) {
            return *id;
        }
        let id = SemNodeId(self.nodes.len() as u32);
        node.id = id;
        self.nodes.push(node);
        self.interner.insert(key, id);
        id
    }

    fn intern_int(&mut self, value: APInt, ty: Option<TypeId>) -> SemNodeId {
        self.intern_with_key(
            SemKey::Int {
                width: value.width(),
                signed: value.is_signed(),
                value: value.to_u64(),
                ty,
            },
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Constant,
                inputs: Vec::new(),
                payload: Some(ExprPayload::Int(value)),
                ty,
                value: None,
            },
        )
    }

    fn intern_input_value(&mut self, value: ValueId, ty: Option<TypeId>) -> SemNodeId {
        self.intern_with_key(
            SemKey::Input(value.number()),
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Symbol,
                inputs: Vec::new(),
                payload: Some(ExprPayload::Value(value)),
                ty,
                value: Some(value),
            },
        )
    }

    fn intern_unknown_symbol(&mut self, symbol: u32, ty: Option<TypeId>) -> SemNodeId {
        self.intern_with_key(
            SemKey::UnknownSymbol(symbol),
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Symbol,
                inputs: Vec::new(),
                payload: Some(ExprPayload::SymbolId(symbol)),
                ty,
                value: None,
            },
        )
    }

    fn intern_op(&mut self, kind: ExprKind, mut inputs: Vec<SemNodeId>, ty: Option<TypeId>) -> SemNodeId {
        if matches!(
            kind,
            ExprKind::Add | ExprKind::Mul | ExprKind::And | ExprKind::Or | ExprKind::Xor
        ) {
            inputs.sort();
        }

        self.intern_with_key(
            SemKey::Op {
                kind,
                inputs: inputs.clone(),
                ty,
            },
            SemNode {
                id: SemNodeId(0),
                kind,
                inputs,
                payload: None,
                ty,
                value: None,
            },
        )
    }

    fn intern_opaque(&mut self) -> SemNodeId {
        self.intern_with_key(
            SemKey::Opaque,
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Symbol,
                inputs: Vec::new(),
                payload: Some(ExprPayload::SymbolId(u32::MAX)),
                ty: None,
                value: None,
            },
        )
    }

    /// Record that `node` produces IR `value` (idempotent; first writer wins, which
    /// is correct since identical computations are the same value under CSE).
    fn set_value(&mut self, node: SemNodeId, value: ValueId) {
        let slot = &mut self.nodes[node.0 as usize].value;
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

impl Dag for SemDagArena {
    type Node = SemNode;
    type Leaf = ();

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn get_node(&self, id: NodeId) -> &Self::Node {
        &self.nodes[id.index()]
    }

    fn get_leaf_data(&self, _id: NodeId) -> Option<&Self::Leaf> {
        None
    }

    fn get_original_op(&self, _id: NodeId) -> Option<OpId> {
        None
    }

    fn get_actual_type(&self, id: NodeId) -> Option<tir::TypeId> {
        self.nodes[id.index()].ty
    }

    fn root(&self) -> Option<NodeId> {
        self.nodes.len().checked_sub(1).map(NodeId::from_index)
    }

    fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.nodes[id.index()]
            .inputs
            .iter()
            .map(|input| NodeId::from_index(input.index()))
    }

    fn postorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        (0..=start.index()).map(NodeId::from_index)
    }

    fn preorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        std::iter::once(start)
    }
}

struct IselCoverLegality<'a> {
    shared_roots: &'a HashSet<SemNodeId>,
}

impl CoverLegality<SemNode, (), usize> for IselCoverLegality<'_> {
    fn binding_allowed(
        &self,
        _ctx: &Context,
        _g: &impl Dag<Node = SemNode, Leaf = ()>,
        pattern: &Pattern<SemNode, usize>,
        pattern_node: NodeId,
        graph_node: NodeId,
    ) -> bool {
        pattern.is_duplicable(pattern_node)
            || !self
                .shared_roots
                .contains(&SemNodeId(graph_node.index() as u32))
    }
}

struct SemDagBuilder<'a> {
    context: &'a Context,
    value_to_def: &'a HashMap<ValueId, OpId>,
    arena: SemDagArena,
    value_to_node: HashMap<ValueId, SemNodeId>,
}

impl<'a> SemDagBuilder<'a> {
    fn new(context: &'a Context, value_to_def: &'a HashMap<ValueId, OpId>) -> Self {
        Self {
            context,
            value_to_def,
            arena: SemDagArena::new(),
            value_to_node: HashMap::new(),
        }
    }

    fn build_for_op(&mut self, op: &std::sync::Arc<OpInstance>) -> Option<SemNodeId> {
        let mut operands = Vec::with_capacity(op.operands.len());
        for operand in &op.operands {
            operands.push(self.build_from_value(*operand));
        }
        let mut graph = ExprPostGraph::new();
        let root = op.clone().as_dyn_op().semantic_expr(&mut graph)?;
        let widths = self.infer_local_widths(&graph, &operands);
        let node = self.lower_graph_node(&graph, root, &operands, &widths);
        if let Some(result) = op.results.first() {
            self.arena.set_value(node, *result);
        }
        Some(node)
    }

    fn build_from_value(&mut self, value: ValueId) -> SemNodeId {
        if let Some(existing) = self.value_to_node.get(&value) {
            return *existing;
        }

        let value_ty = Some(self.context.get_value(value).ty());
        let node = if let Some(def_op_id) = self.value_to_def.get(&value) {
            let def = self.context.get_op(*def_op_id);
            if def.name == "constant" {
                if let Some(attr) = def.attributes.iter().find(|a| a.name == "value") {
                    if let AttributeValue::Int(v) = &attr.value {
                        self.arena.intern_int(APInt::new_signed(64, *v), value_ty)
                    } else {
                        self.arena.intern_input_value(value, value_ty)
                    }
                } else {
                    self.arena.intern_input_value(value, value_ty)
                }
            } else {
                let mut graph = ExprPostGraph::new();
                if let Some(root) = def.clone().as_dyn_op().semantic_expr(&mut graph) {
                    let mut operands = Vec::with_capacity(def.operands.len());
                    for operand in &def.operands {
                        operands.push(self.build_from_value(*operand));
                    }
                    let widths = self.infer_local_widths(&graph, &operands);
                    let node = self.lower_graph_node(&graph, root, &operands, &widths);
                    self.arena.set_value(node, value);
                    node
                } else {
                    self.arena.intern_input_value(value, value_ty)
                }
            }
        } else {
            self.arena.intern_input_value(value, value_ty)
        };

        self.value_to_node.insert(value, node);
        node
    }

    /// Infer the width of every node of `graph` from the IR types of the operands
    /// it references, then resolve those widths against the live context. This is
    /// the same width rule TMDL uses for patterns, so the program graph and the
    /// rule patterns end up typed consistently.
    fn infer_local_widths(
        &self,
        graph: &ExprPostGraph,
        operands: &[SemNodeId],
    ) -> Vec<Option<u32>> {
        infer_widths(graph, |node| match graph.get_leaf_data(node) {
            Some(ExprPayload::SymbolId(id)) => operands
                .get(*id as usize)
                .and_then(|&sem| self.arena.node(sem).ty)
                .and_then(|ty| type_width(self.context, ty)),
            _ => None,
        })
    }

    /// Lower one node of a semantic-expression graph, typing each node from its
    /// inferred width. Operand leaves keep the IR type they were built with;
    /// internal nodes (and the root) take their inferred width resolved to a type.
    fn lower_graph_node(
        &mut self,
        graph: &ExprPostGraph,
        node: NodeId,
        operands: &[SemNodeId],
        widths: &[Option<u32>],
    ) -> SemNodeId {
        let node_ty = widths[node.index()].map(|width| IntegerType::new(self.context, width));
        match graph.get_node(node) {
            ExprKind::Symbol => match graph.get_leaf_data(node) {
                Some(ExprPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .copied()
                    .unwrap_or_else(|| self.arena.intern_unknown_symbol(*id, node_ty)),
                _ => self.arena.intern_opaque(),
            },
            ExprKind::Constant => match graph.get_leaf_data(node) {
                Some(ExprPayload::Int(v)) => self.arena.intern_int(v.clone(), node_ty),
                _ => self.arena.intern_opaque(),
            },
            kind => {
                let children: Vec<SemNodeId> = graph
                    .children(node)
                    .map(|child| self.lower_graph_node(graph, child, operands, widths))
                    .collect();
                if kind.num_children(self.context) == children.len() {
                    self.arena.intern_op(*kind, children, node_ty)
                } else {
                    self.arena.intern_opaque()
                }
            }
        }
    }
}

/// The integer bit-width of an IR type, or `None` if it is not an integer type.
fn type_width(context: &Context, ty: TypeId) -> Option<u32> {
    let data = context.get_type_data(ty);
    (data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<IntegerType>()
        .map(IntegerType::width)
}

/// Headroom factor that lets pattern specificity break ties between equal-cost
/// matches without ever overriding a genuine instruction-cost difference.
const SPECIFICITY_SCALE: u64 = 8;

/// Fold a match's specificity into its cost: scale the instruction cost, then give
/// a small discount for each type-constrained pattern node. So among equally cheap
/// matches the most specific (e.g. i32 `addw` over untyped `add`) wins, while a
/// cheaper instruction still wins outright.
fn specificity_adjusted_cost(cost: u64, specificity: usize) -> u64 {
    cost.saturating_mul(SPECIFICITY_SCALE)
        .saturating_sub((specificity as u64).min(SPECIFICITY_SCALE - 1))
}

#[derive(Clone, Debug)]
enum BlockDecision {
    Emit { rule_index: usize, m: RuleMatch },
    Consume,
}

#[derive(Clone, Debug)]
enum PbqpIselAlternative {
    External,
    Root {
        match_id: usize,
    },
    Internal {
        match_id: usize,
        pattern_node: NodeId,
    },
}

#[derive(Clone, Debug)]
struct PbqpIselMatch {
    pattern_index: usize,
    rule_index: usize,
    root: SemNodeId,
    pattern_root: NodeId,
    bindings: FullMatchBindings,
    cost: u64,
}

struct BlockSelectionCache {
    dag: SemDagArena,
    op_by_root: HashMap<SemNodeId, OpId>,
    decisions_by_op: Option<HashMap<OpId, BlockDecision>>,
}

struct CompiledIselPattern {
    rule_index: usize,
    pattern: Pattern<SemNode, usize>,
    boundary_symbols: HashMap<NodeId, u32>,
    /// Number of type-constrained pattern nodes — how "specific" this pattern is.
    /// At equal instruction cost, a more specific match is preferred, so an i32
    /// `addw` (one typed node) beats the untyped `add` for an i32 value, while the
    /// untyped `add`/`and` still match every other width.
    specificity: usize,
    /// `Some` for patterns synthesized to complete the rule set (paper §4). A
    /// synthesized pattern has no emitter of its own; selecting it expands into
    /// the constituent per-op decisions recorded in the cover.
    synthesis: Option<SynthesizedCover>,
}

/// A cover of a synthesized pattern by existing rules (paper §4). Selecting the
/// synthesized pattern expands into these per-op constituent emissions.
#[derive(Clone, Debug)]
struct SynthesizedCover {
    cost: u64,
    parts: Vec<CoverPart>,
}

/// One constituent of a [`SynthesizedCover`]: an existing rule emitted at the
/// synthesized-pattern node it roots, plus the set of synthesized-pattern nodes
/// it consumes (which are erased at emit time).
#[derive(Clone, Debug)]
struct CoverPart {
    /// Node of the synthesized pattern this constituent rule is rooted at.
    root_pattern_node: NodeId,
    rule_index: usize,
    /// Synthesized-pattern nodes covered (and thus consumed) by this constituent,
    /// excluding its own root.
    consumed_pattern_nodes: Vec<NodeId>,
    /// Maps the constituent rule's capture symbols to the synthesized pattern node
    /// supplying that operand.
    capture_sources: Vec<(u32, NodeId)>,
}

fn compile_isel_pattern(
    rule_index: usize,
    expr: &ExprPostGraph,
    operand_constraints: &[(u32, OperandConstraint)],
) -> Option<CompiledIselPattern> {
    let root = expr.root()?;
    let mut pattern = Pattern::new(rule_index);
    let mut boundary_symbols = HashMap::new();
    let mut memo = HashMap::new();
    let pattern_root =
        compile_isel_pattern_node(expr, root, &mut pattern, &mut boundary_symbols, &mut memo)?;
    pattern.set_root(pattern_root);

    // Constrain each operand boundary to register or immediate, so e.g. an
    // immediate-shift pattern never matches a register shift amount.
    for (&pattern_node, &symbol) in &boundary_symbols {
        if let Some((_, constraint)) = operand_constraints.iter().find(|(s, _)| *s == symbol) {
            pattern.set_operand_constraint(pattern_node, *constraint);
        }
    }

    let specificity = (0..pattern.len())
        .map(NodeId::from_index)
        .filter(|&n| matches!(pattern.get_node(n), PatternExpr::Node(node) if node.ty.is_some()))
        .count();

    Some(CompiledIselPattern {
        rule_index,
        pattern,
        boundary_symbols,
        specificity,
        synthesis: None,
    })
}

fn compile_isel_pattern_node(
    expr: &ExprPostGraph,
    node: NodeId,
    pattern: &mut Pattern<SemNode, usize>,
    boundary_symbols: &mut HashMap<NodeId, u32>,
    memo: &mut HashMap<NodeId, NodeId>,
) -> Option<NodeId> {
    if let Some(compiled) = memo.get(&node).copied() {
        return Some(compiled);
    }

    let compiled = match expr.get_node(node) {
        ExprKind::Symbol => {
            let Some(ExprPayload::SymbolId(symbol)) = expr.get_leaf_data(node) else {
                return None;
            };
            let compiled = pattern.add_node(PatternExpr::Boundary);
            pattern.set_duplicable(compiled, true);
            boundary_symbols.insert(compiled, *symbol);
            compiled
        }
        ExprKind::Constant => match expr.get_leaf_data(node) {
            Some(ExprPayload::Int(value)) => pattern.add_node(PatternExpr::Node(template_node(
                ExprKind::Constant,
                Some(ExprPayload::Int(value.clone())),
                expr.get_actual_type(node),
            ))),
            _ => return None,
        },
        kind => {
            let compiled = pattern.add_node(PatternExpr::Node(template_node(
                *kind,
                None,
                expr.get_actual_type(node),
            )));
            memo.insert(node, compiled);
            for child in expr.children(node) {
                let compiled_child =
                    compile_isel_pattern_node(expr, child, pattern, boundary_symbols, memo)?;
                pattern.add_edge(compiled, compiled_child);
            }
            return Some(compiled);
        }
    };

    memo.insert(node, compiled);
    Some(compiled)
}

fn template_node(kind: ExprKind, payload: Option<ExprPayload>, ty: Option<TypeId>) -> SemNode {
    SemNode {
        id: SemNodeId(0),
        kind,
        inputs: Vec::new(),
        payload,
        ty,
        value: None,
    }
}

fn atomic_materializers(patterns: &[CompiledIselPattern]) -> HashSet<ExprKind> {
    let mut atomics = HashSet::new();
    for compiled in patterns {
        let Some(root) = compiled.pattern.root() else {
            continue;
        };
        let PatternExpr::Node(root_node) = compiled.pattern.get_node(root) else {
            continue;
        };
        if root_node.kind.num_children(&Context::default()) == 0 {
            continue;
        }

        let children = compiled.pattern.children(root);
        if !children.is_empty()
            && children
                .iter()
                .all(|&child| matches!(compiled.pattern.get_node(child), PatternExpr::Boundary))
        {
            atomics.insert(root_node.kind);
        }
    }
    atomics
}

/// The typed-node skeleton of a pattern. Untyped boundary/leaf/wildcard nodes
/// collapse to [`PatternShape::Boundary`]; typed nodes keep their kind, payload,
/// and child structure. Two patterns are "shape-equal" (hence the same rule for
/// availability purposes) iff their shapes are equal.
#[derive(Clone, Debug, PartialEq)]
enum PatternShape {
    Boundary,
    Node {
        kind: ExprKind,
        payload: Option<ExprPayload>,
        children: Vec<PatternShape>,
    },
}

impl PatternShape {
    fn is_typed_op(&self) -> bool {
        matches!(self, PatternShape::Node { children, .. } if !children.is_empty())
    }

    fn with_child_boundary(&self, index: usize) -> PatternShape {
        let PatternShape::Node {
            kind,
            payload,
            children,
        } = self
        else {
            return self.clone();
        };
        let mut children = children.clone();
        children[index] = PatternShape::Boundary;
        PatternShape::Node {
            kind: *kind,
            payload: payload.clone(),
            children,
        }
    }

    fn describe(&self) -> String {
        match self {
            PatternShape::Boundary => "_".to_string(),
            PatternShape::Node { kind, children, .. } if children.is_empty() => {
                format!("{kind:?}")
            }
            PatternShape::Node { kind, children, .. } => {
                let inner: Vec<_> = children.iter().map(PatternShape::describe).collect();
                format!("{kind:?}({})", inner.join(", "))
            }
        }
    }
}

fn pattern_shape(pattern: &Pattern<SemNode, usize>, node: NodeId) -> PatternShape {
    match pattern.get_node(node) {
        PatternExpr::Boundary | PatternExpr::Leaf | PatternExpr::Any => PatternShape::Boundary,
        PatternExpr::Node(n) => PatternShape::Node {
            kind: n.kind,
            payload: n.payload.clone(),
            children: pattern
                .children(node)
                .iter()
                .map(|&child| pattern_shape(pattern, child))
                .collect(),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredKind {
    /// The typed subtree rooted at an internal node must be matchable on its own.
    Subpattern,
    /// The pattern with a direct-successor subtree cut away (materialized).
    CutPattern,
}

/// A pattern shape the rule set must contain for composability (paper §4).
#[derive(Clone, Debug)]
pub struct RequiredPattern {
    kind: RequiredKind,
    shape: PatternShape,
    source_rule: usize,
}

impl RequiredPattern {
    pub fn kind(&self) -> RequiredKind {
        self.kind
    }

    pub fn source_rule(&self) -> usize {
        self.source_rule
    }

    pub fn describe(&self) -> String {
        format!("{:?} {}", self.kind, self.shape.describe())
    }
}

/// Input-independent assessment of whether a rule set can always produce a valid
/// cover. `missing_atomic` is fatal (the engine cannot emit an opcode it has no
/// rule for); `missing_composable` is the synthesis worklist.
#[derive(Clone, Debug, Default)]
pub struct RuleSetAnalysis {
    missing_atomic: Vec<ExprKind>,
    missing_composable: Vec<RequiredPattern>,
}

impl RuleSetAnalysis {
    pub fn missing_atomic(&self) -> &[ExprKind] {
        &self.missing_atomic
    }

    pub fn missing_composable(&self) -> &[RequiredPattern] {
        &self.missing_composable
    }

    pub fn is_atomically_complete(&self) -> bool {
        self.missing_atomic.is_empty()
    }
}

fn enumerate_required(shape: &PatternShape, source_rule: usize, out: &mut Vec<RequiredPattern>) {
    let PatternShape::Node { children, .. } = shape else {
        return;
    };
    for (index, child) in children.iter().enumerate() {
        if !child.is_typed_op() {
            continue;
        }
        out.push(RequiredPattern {
            kind: RequiredKind::CutPattern,
            shape: shape.with_child_boundary(index),
            source_rule,
        });
        out.push(RequiredPattern {
            kind: RequiredKind::Subpattern,
            shape: child.clone(),
            source_rule,
        });
        enumerate_required(child, source_rule, out);
    }
}

fn analyze_rule_set(patterns: &[CompiledIselPattern]) -> RuleSetAnalysis {
    let ctx = Context::default();
    let atomics = atomic_materializers(patterns);

    let available: Vec<PatternShape> = patterns
        .iter()
        .filter_map(|p| p.pattern.root().map(|root| pattern_shape(&p.pattern, root)))
        .collect();

    // Atomic completeness: every non-leaf kind used as a typed node anywhere in
    // the rule set must have an atomic materializer.
    let mut required_kinds: Vec<ExprKind> = Vec::new();
    for p in patterns {
        for i in 0..p.pattern.len() {
            let PatternExpr::Node(n) = p.pattern.get_node(NodeId::from_index(i)) else {
                continue;
            };
            if n.kind.num_children(&ctx) > 0 && !required_kinds.contains(&n.kind) {
                required_kinds.push(n.kind);
            }
        }
    }
    required_kinds.sort();
    let missing_atomic: Vec<ExprKind> = required_kinds
        .into_iter()
        .filter(|kind| !atomics.contains(kind))
        .collect();

    // Composability: enumerate the subpattern/cut-pattern shapes each complex
    // pattern depends on, and report the ones no existing pattern provides.
    let mut missing_composable: Vec<RequiredPattern> = Vec::new();
    for (idx, p) in patterns.iter().enumerate() {
        let Some(root) = p.pattern.root() else {
            continue;
        };
        let shape = pattern_shape(&p.pattern, root);
        let mut required = Vec::new();
        enumerate_required(&shape, idx, &mut required);
        for req in required {
            if available.contains(&req.shape) {
                continue;
            }
            if missing_composable.iter().any(|m| m.shape == req.shape) {
                continue;
            }
            missing_composable.push(req);
        }
    }

    RuleSetAnalysis {
        missing_atomic,
        missing_composable,
    }
}

/// A selected alternative per node of a covered DAG, with the achieved cost.
struct DagCover {
    choices: Vec<PbqpIselAlternative>,
    total_cost: u64,
}

/// Build and solve the PBQP cover for `dag` given a fixed set of `matches`. The
/// `edge_cost` closure prices satisfied parent -> child edges (zero for synthesis).
/// Returns `None` if the instance is infeasible (a node with no valid alternative).
fn build_dag_cover(
    dag: &SemDagArena,
    op_by_root: &HashMap<SemNodeId, OpId>,
    patterns: &[CompiledIselPattern],
    matches: &[PbqpIselMatch],
    edge_cost: impl Fn(SemNodeId, SemNodeId, &PbqpIselAlternative) -> u64,
) -> Option<DagCover> {
    let mut alternatives_by_node = vec![Vec::<PbqpIselAlternative>::new(); dag.len()];
    for (node, sem_node) in dag.iter() {
        if sem_node.is_terminal() {
            alternatives_by_node[node.index()].push(PbqpIselAlternative::External);
        }
    }

    for (match_id, m) in matches.iter().enumerate() {
        alternatives_by_node[m.root.index()].push(PbqpIselAlternative::Root { match_id });
        for binding in &m.bindings.pattern_nodes {
            if binding.is_boundary || binding.pattern_node == m.pattern_root {
                continue;
            }
            alternatives_by_node[binding.sem_node.index()].push(PbqpIselAlternative::Internal {
                match_id,
                pattern_node: binding.pattern_node,
            });
        }
    }

    for (node, alternatives) in alternatives_by_node.iter_mut().enumerate() {
        if alternatives.is_empty() {
            let sem_node = dag.node(SemNodeId(node as u32));
            if sem_node.is_terminal() || !op_by_root.contains_key(&SemNodeId(node as u32)) {
                alternatives.push(PbqpIselAlternative::External);
            }
        }
    }

    if alternatives_by_node.iter().any(Vec::is_empty) {
        return None;
    }

    let mut problem = PbqpProblem::new();
    for alternatives in &alternatives_by_node {
        let costs = alternatives
            .iter()
            .map(|alternative| match alternative {
                PbqpIselAlternative::Root { match_id } => matches[*match_id].cost,
                PbqpIselAlternative::External | PbqpIselAlternative::Internal { .. } => 0,
            })
            .collect();
        problem.add_node(costs);
    }

    for (match_id, m) in matches.iter().enumerate() {
        let mut coherent = Vec::new();
        for (node, alternatives) in alternatives_by_node.iter().enumerate() {
            for (alternative, pbqp_alt) in alternatives.iter().enumerate() {
                let belongs_to_match = match pbqp_alt {
                    PbqpIselAlternative::Root { match_id: alt_match }
                    | PbqpIselAlternative::Internal { match_id: alt_match, .. } => {
                        *alt_match == match_id
                    }
                    PbqpIselAlternative::External => false,
                };
                if belongs_to_match {
                    coherent.push(PbqpAlternative {
                        node: pbqp::PbqpNodeId::from_index(node),
                        alternative,
                    });
                }
            }
        }
        if m.bindings.pattern_nodes.len() > 1 {
            problem.add_coherence_set(coherent);
        }
    }

    for (parent_id, parent) in dag.iter() {
        for &child_id in &parent.inputs {
            let parent_alts = &alternatives_by_node[parent_id.index()];
            let child_alts = &alternatives_by_node[child_id.index()];
            let mut matrix = PbqpMatrix::zero(parent_alts.len(), child_alts.len());

            for (parent_alt_idx, parent_alt) in parent_alts.iter().enumerate() {
                for (child_alt_idx, child_alt) in child_alts.iter().enumerate() {
                    if !alternatives_compatible(
                        patterns, parent_id, child_id, parent_alt, child_alt, matches,
                    ) {
                        matrix.set(parent_alt_idx, child_alt_idx, INF_COST);
                        continue;
                    }
                    let cost = edge_cost(parent_id, child_id, parent_alt);
                    if cost != 0 {
                        matrix.set(parent_alt_idx, child_alt_idx, cost);
                    }
                }
            }

            problem.add_edge(
                pbqp::PbqpNodeId::from_index(parent_id.index()),
                pbqp::PbqpNodeId::from_index(child_id.index()),
                matrix,
            );
        }
    }

    let solution = pbqp::solve(&problem).ok()?;
    let choices = solution
        .choices
        .iter()
        .copied()
        .enumerate()
        .map(|(node, choice)| alternatives_by_node[node][choice].clone())
        .collect();
    Some(DagCover {
        choices,
        total_cost: solution.total_cost,
    })
}

/// A required pattern shape realized as both a matchable [`Pattern`] and a
/// standalone semantic DAG, kept node-aligned so a cover of the DAG maps straight
/// back onto the pattern.
struct MaterializedShape {
    pattern: Pattern<SemNode, usize>,
    boundary_symbols: HashMap<NodeId, u32>,
    dag: SemDagArena,
    op_by_root: HashMap<SemNodeId, OpId>,
    sem_to_pattern: HashMap<SemNodeId, NodeId>,
}

fn materialize_shape(shape: &PatternShape) -> Option<MaterializedShape> {
    let mut out = MaterializedShape {
        pattern: Pattern::new(0usize),
        boundary_symbols: HashMap::new(),
        dag: SemDagArena::new(),
        op_by_root: HashMap::new(),
        sem_to_pattern: HashMap::new(),
    };
    let mut next_symbol = 0u32;
    let (root, _) = build_materialized_node(shape, &mut out, &mut next_symbol)?;
    out.pattern.set_root(root);
    Some(out)
}

fn build_materialized_node(
    shape: &PatternShape,
    out: &mut MaterializedShape,
    next_symbol: &mut u32,
) -> Option<(NodeId, SemNodeId)> {
    let (pattern_node, sem_node) = match shape {
        PatternShape::Boundary => {
            let pattern_node = out.pattern.add_node(PatternExpr::Boundary);
            out.pattern.set_duplicable(pattern_node, true);
            let symbol = *next_symbol;
            *next_symbol += 1;
            out.boundary_symbols.insert(pattern_node, symbol);
            // Synthetic nodes are type-agnostic: synthesis covers by structure, so
            // both the synthesized pattern and the DAG it is covered against are
            // left untyped (type wildcards).
            let sem_node = out.dag.intern_unknown_symbol(symbol, None);
            (pattern_node, sem_node)
        }
        PatternShape::Node {
            kind,
            payload,
            children,
        } if children.is_empty() => {
            let pattern_node = out
                .pattern
                .add_node(PatternExpr::Node(template_node(*kind, payload.clone(), None)));
            let sem_node = match payload {
                Some(ExprPayload::Int(value)) => out.dag.intern_int(value.clone(), None),
                _ => out.dag.intern_opaque(),
            };
            (pattern_node, sem_node)
        }
        PatternShape::Node {
            kind,
            payload,
            children,
        } => {
            let pattern_node = out
                .pattern
                .add_node(PatternExpr::Node(template_node(*kind, payload.clone(), None)));
            let mut sem_children = Vec::with_capacity(children.len());
            for child in children {
                let (child_pattern, child_sem) =
                    build_materialized_node(child, out, next_symbol)?;
                out.pattern.add_edge(pattern_node, child_pattern);
                sem_children.push(child_sem);
            }
            let sem_node = out.dag.intern_op(*kind, sem_children, None);
            out.op_by_root.insert(sem_node, OpId::invalid());
            (pattern_node, sem_node)
        }
    };
    out.sem_to_pattern.insert(sem_node, pattern_node);
    Some((pattern_node, sem_node))
}

/// Match the existing rules against a synthetic DAG, costing each match by its
/// rule's static `base_cost` (synthesis is target-independent).
fn collect_synthetic_matches(
    context: &Context,
    dag: &SemDagArena,
    op_by_root: &HashMap<SemNodeId, OpId>,
    existing: &[CompiledIselPattern],
    rules: &[Rule],
) -> Vec<PbqpIselMatch> {
    let shared_roots: HashSet<SemNodeId> = HashSet::new();
    let legality = IselCoverLegality {
        shared_roots: &shared_roots,
    };
    let mut matches = Vec::new();
    for (pattern_index, compiled) in existing.iter().enumerate() {
        if compiled.synthesis.is_some() {
            continue;
        }
        let Some(pattern_root) = compiled.pattern.root() else {
            continue;
        };
        for binding in
            VF2CoverDriver::matches_with_legality(context, dag, &compiled.pattern, &legality)
        {
            let root = SemNodeId(binding.graph_root().index() as u32);
            if !op_by_root.contains_key(&root) {
                continue;
            }

            let mut captures = CaptureBindings::new();
            for (pattern_node, symbol) in &compiled.boundary_symbols {
                captures.bind(
                    *symbol,
                    SemNodeId(binding.binding(*pattern_node).index() as u32),
                );
            }
            let pattern_nodes = (0..compiled.pattern.len())
                .map(NodeId::from_index)
                .map(|pattern_node| PatternNodeBinding {
                    pattern_node,
                    sem_node: SemNodeId(binding.binding(pattern_node).index() as u32),
                    is_boundary: matches!(
                        compiled.pattern.get_node(pattern_node),
                        PatternExpr::Boundary
                    ),
                })
                .collect();

            matches.push(PbqpIselMatch {
                pattern_index,
                rule_index: compiled.rule_index,
                root,
                pattern_root,
                bindings: FullMatchBindings {
                    captures,
                    pattern_nodes,
                },
                cost: specificity_adjusted_cost(
                    rules[compiled.rule_index].base_cost as u64,
                    compiled.specificity,
                ),
            });
        }
    }
    matches
}

/// Translate a solved synthetic cover into a [`SynthesizedCover`] expressed over
/// the synthesized pattern's nodes (via the sem -> pattern alignment).
fn extract_cover(
    cover: &DagCover,
    matches: &[PbqpIselMatch],
    existing: &[CompiledIselPattern],
    sem_to_pattern: &HashMap<SemNodeId, NodeId>,
) -> Option<SynthesizedCover> {
    let mut parts = Vec::new();
    for choice in &cover.choices {
        let PbqpIselAlternative::Root { match_id } = choice else {
            continue;
        };
        let m = &matches[*match_id];
        let compiled = &existing[m.pattern_index];
        let root_pattern_node = *sem_to_pattern.get(&m.root)?;

        let mut consumed_pattern_nodes = Vec::new();
        for binding in &m.bindings.pattern_nodes {
            if binding.pattern_node == m.pattern_root || binding.is_boundary {
                continue;
            }
            if let Some(node) = sem_to_pattern.get(&binding.sem_node) {
                consumed_pattern_nodes.push(*node);
            }
        }

        let mut capture_sources = Vec::new();
        for (pattern_node, symbol) in &compiled.boundary_symbols {
            if let Some(node) = m
                .bindings
                .sem_node_for_pattern(*pattern_node)
                .and_then(|sem| sem_to_pattern.get(&sem))
            {
                capture_sources.push((*symbol, *node));
            }
        }

        parts.push(CoverPart {
            root_pattern_node,
            rule_index: m.rule_index,
            consumed_pattern_nodes,
            capture_sources,
        });
    }

    if parts.is_empty() {
        return None;
    }
    Some(SynthesizedCover {
        cost: cover.total_cost,
        parts,
    })
}

/// Auto-complete the rule set (paper §4): for every missing subpattern/cut-pattern
/// that can be covered by the existing rules, synthesize a matchable pattern whose
/// cost is the summed cover cost and whose emission expands into that cover.
///
/// Best-effort: a required shape that the existing rules cannot cover (because it
/// bottoms out at a kind with no atomic materializer) is simply skipped. Real input
/// that needs such a kind standalone is still caught by the per-block atomic check.
fn synthesize_missing_patterns(
    analysis: &RuleSetAnalysis,
    rules: &[Rule],
    existing: &[CompiledIselPattern],
) -> Vec<CompiledIselPattern> {
    let context = Context::default();
    let mut synthesized = Vec::new();

    for required in &analysis.missing_composable {
        let Some(material) = materialize_shape(&required.shape) else {
            continue;
        };

        let matches = collect_synthetic_matches(
            &context,
            &material.dag,
            &material.op_by_root,
            existing,
            rules,
        );

        let Some(cover) = build_dag_cover(
            &material.dag,
            &material.op_by_root,
            existing,
            &matches,
            |_, _, _| 0,
        )
        .filter(|cover| cover.total_cost < INF_COST)
        .and_then(|cover| extract_cover(&cover, &matches, existing, &material.sem_to_pattern))
        else {
            continue;
        };

        synthesized.push(CompiledIselPattern {
            rule_index: 0,
            pattern: material.pattern,
            boundary_symbols: material.boundary_symbols,
            specificity: 0,
            synthesis: Some(cover),
        });
    }

    synthesized
}

pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    compiled_patterns: Vec<CompiledIselPattern>,
    /// Composability assessment of the original rule set (before synthesis).
    analysis: RuleSetAnalysis,
    /// Atomic materializer kinds provided by the rule set. A block whose op roots
    /// include a non-leaf kind outside this set cannot be selected (paper's
    /// atomic-completeness guarantee), and is rejected per-block.
    atomics: HashSet<ExprKind>,
    target_model: Box<dyn TargetIselModel>,
    op_lowerings: Vec<OpLowering>,
    block_cache: HashMap<BlockId, BlockSelectionCache>,
    emitted_blocks: HashSet<BlockId>,
}

impl InstructionSelectPass {
    pub fn new(mut rules: Vec<Rule>) -> Self {
        for (idx, rule) in rules.iter_mut().enumerate() {
            rule.compiled_rule_id = CompiledRuleId(idx as u32);
        }
        let mut compiled_patterns: Vec<_> = rules
            .iter()
            .enumerate()
            .filter_map(|(rule_index, rule)| {
                compile_isel_pattern(rule_index, &rule.pattern, &rule.operand_constraints)
            })
            .collect();

        let atomics = atomic_materializers(&compiled_patterns);
        let analysis = analyze_rule_set(&compiled_patterns);

        // Auto-complete the rule set (paper §4): append a matchable pattern for
        // every missing subpattern/cut-pattern the existing rules can cover.
        let synthesized = synthesize_missing_patterns(&analysis, &rules, &compiled_patterns);
        compiled_patterns.extend(synthesized);

        Self {
            rules,
            compiled_patterns,
            analysis,
            atomics,
            target_model: Box::new(DefaultTargetIselModel),
            op_lowerings: vec![],
            block_cache: HashMap::new(),
            emitted_blocks: HashSet::new(),
        }
    }

    /// Composability assessment of the rule set as supplied (before synthesis).
    pub fn rule_set_analysis(&self) -> &RuleSetAnalysis {
        &self.analysis
    }

    pub fn with_target_model(mut self, target_model: Box<dyn TargetIselModel>) -> Self {
        self.target_model = target_model;
        self
    }

    pub fn with_op_lowering(mut self, lowering: OpLowering) -> Self {
        self.op_lowerings.push(lowering);
        self
    }

    fn ensure_block_cache(&mut self, context: &Context, block: &Block) {
        if self.block_cache.contains_key(&block.id()) {
            return;
        }

        let mut value_to_def = HashMap::new();
        for op_id in block.op_ids() {
            let op = context.get_op(op_id);
            for result in &op.results {
                value_to_def.insert(*result, op_id);
            }
        }

        let mut builder = SemDagBuilder::new(context, &value_to_def);
        let mut roots_by_op = HashMap::new();

        let op_ids = block.op_ids();
        for op_id in &op_ids {
            let op = context.get_op(*op_id);
            if op.results.is_empty() {
                continue;
            }

            if let Some(root) = builder.build_for_op(&op) {
                roots_by_op.insert(*op_id, root);
            }
        }

        self.block_cache.insert(
            block.id(),
            BlockSelectionCache {
                dag: builder.arena,
                op_by_root: roots_by_op.iter().map(|(op, root)| (*root, *op)).collect(),
                decisions_by_op: None,
            },
        );
    }

    fn ensure_block_solution(&mut self, context: &Context, block: &Block) {
        self.ensure_block_cache(context, block);
        let Some(cache) = self.block_cache.get(&block.id()) else {
            return;
        };
        if cache.decisions_by_op.is_some() {
            return;
        }

        let decisions = self.solve_block(context, block, cache);
        if let Some(cache) = self.block_cache.get_mut(&block.id()) {
            cache.decisions_by_op = Some(decisions);
        }
    }

    fn commit_block_solution(
        &mut self,
        context: &Context,
        block: &Block,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        if !self.emitted_blocks.insert(block.id()) {
            return Ok(());
        }

        self.ensure_block_cache(context, block);
        if let Some(message) = self
            .block_cache
            .get(&block.id())
            .and_then(|cache| self.block_atomic_completeness_error(cache))
        {
            return Err(PassError::InvalidRuleSet(message));
        }

        self.ensure_block_solution(context, block);
        let decisions = self
            .block_cache
            .get(&block.id())
            .and_then(|cache| cache.decisions_by_op.as_ref())
            .cloned()
            .unwrap_or_default();

        if decisions.is_empty() {
            return Ok(());
        }

        let op_ids = block.op_ids();
        for (position, op_id) in op_ids.into_iter().enumerate() {
            let Some(decision) = decisions.get(&op_id).cloned() else {
                continue;
            };
            let op_ref = OperationRef::new(
                context.get_op(op_id),
                Some(context.get_block(block.id())),
                Some(position),
            );
            match decision {
                BlockDecision::Emit { rule_index, m } => {
                    let rule = &self.rules[rule_index];
                    let plan = (rule.emit_plan_fn)(context, &op_ref, &m)?;
                    let new_op = plan.into_op();
                    rewriter.replace_op(&op_ref, new_op.as_ref())?;
                }
                BlockDecision::Consume => {
                    rewriter.erase_op(&op_ref)?;
                }
            }
        }

        Ok(())
    }

    /// Atomic-completeness check for a concrete block: every op root whose kind is
    /// not an atomic materializer makes selection impossible (the engine has no
    /// fallback to emit it). Input-driven because the universe of opcodes that can
    /// appear standalone is not known statically.
    fn block_atomic_completeness_error(&self, cache: &BlockSelectionCache) -> Option<String> {
        let mut missing: Vec<ExprKind> = Vec::new();
        for root in cache.op_by_root.keys().copied() {
            let sem_node = cache.dag.node(root);
            if sem_node.is_terminal() {
                continue;
            }
            if !self.atomics.contains(&sem_node.kind) && !missing.contains(&sem_node.kind) {
                missing.push(sem_node.kind);
            }
        }
        if missing.is_empty() {
            return None;
        }
        missing.sort();
        Some(
            missing
                .iter()
                .map(|kind| format!("missing atomic materializer rule for semantic kind {kind:?}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    fn solve_block(
        &self,
        context: &Context,
        block: &Block,
        cache: &BlockSelectionCache,
    ) -> HashMap<OpId, BlockDecision> {
        let mut op_refs = HashMap::new();
        for (position, op_id) in block.op_ids().into_iter().enumerate() {
            let op = context.get_op(op_id);
            op_refs.insert(
                op_id,
                OperationRef::new(op, Some(context.get_block(block.id())), Some(position)),
            );
        }

        let matches = self.collect_block_matches(context, cache, &op_refs);
        if matches.is_empty() {
            return HashMap::new();
        }

        let cost_model = self.target_model.cost_model();
        let Some(cover) = build_dag_cover(
            &cache.dag,
            &cache.op_by_root,
            &self.compiled_patterns,
            &matches,
            |parent, child, parent_alt| {
                materialization_edge_cost(
                    &self.compiled_patterns,
                    &cache.dag,
                    parent,
                    child,
                    parent_alt,
                    &matches,
                    cost_model,
                )
            },
        ) else {
            return HashMap::new();
        };

        let mut decisions = HashMap::new();
        // Synthesized matches are authoritative over the nodes they cover: expand
        // them into the constituent per-op decisions first, then fill in the rest.
        let mut covered_by_synth: HashSet<SemNodeId> = HashSet::new();
        for choice in &cover.choices {
            if let PbqpIselAlternative::Root { match_id } = choice {
                let m = &matches[*match_id];
                if let Some(synth) = &self.compiled_patterns[m.pattern_index].synthesis {
                    self.expand_synthesized(cache, m, synth, &mut decisions, &mut covered_by_synth);
                }
            }
        }

        for (node, choice) in cover.choices.iter().enumerate() {
            let node_id = SemNodeId(node as u32);
            if covered_by_synth.contains(&node_id) {
                continue;
            }
            let Some(op_id) = cache.op_by_root.get(&node_id).copied() else {
                continue;
            };
            match choice {
                PbqpIselAlternative::Root { match_id } => {
                    let m = &matches[*match_id];
                    decisions.insert(
                        op_id,
                        BlockDecision::Emit {
                            rule_index: m.rule_index,
                            m: m.bindings.captures.to_rule_match(&cache.dag),
                        },
                    );
                }
                PbqpIselAlternative::Internal { .. } => {
                    decisions.insert(op_id, BlockDecision::Consume);
                }
                PbqpIselAlternative::External => {}
            }
        }

        decisions
    }

    /// Expand a selected synthesized pattern into the constituent per-op decisions
    /// recorded in its cover: each part emits an existing rule at the real op it
    /// roots, and every node the part consumes is erased.
    fn expand_synthesized(
        &self,
        cache: &BlockSelectionCache,
        m: &PbqpIselMatch,
        synth: &SynthesizedCover,
        decisions: &mut HashMap<OpId, BlockDecision>,
        covered: &mut HashSet<SemNodeId>,
    ) {
        for part in &synth.parts {
            let Some(real_root) = m.bindings.sem_node_for_pattern(part.root_pattern_node) else {
                continue;
            };
            covered.insert(real_root);

            let mut captures = CaptureBindings::new();
            for (symbol, source_pattern_node) in &part.capture_sources {
                if let Some(real_sem) = m.bindings.sem_node_for_pattern(*source_pattern_node) {
                    captures.bind(*symbol, real_sem);
                }
            }
            let rule_match = captures.to_rule_match(&cache.dag);

            if let Some(op_id) = cache.op_by_root.get(&real_root).copied() {
                decisions.insert(
                    op_id,
                    BlockDecision::Emit {
                        rule_index: part.rule_index,
                        m: rule_match,
                    },
                );
            }

            for consumed in &part.consumed_pattern_nodes {
                if let Some(real_sem) = m.bindings.sem_node_for_pattern(*consumed) {
                    covered.insert(real_sem);
                    if let Some(op_id) = cache.op_by_root.get(&real_sem).copied() {
                        decisions.insert(op_id, BlockDecision::Consume);
                    }
                }
            }
        }
    }

    fn collect_block_matches(
        &self,
        context: &Context,
        cache: &BlockSelectionCache,
        op_refs: &HashMap<OpId, OperationRef>,
    ) -> Vec<PbqpIselMatch> {
        let mut matches = Vec::new();
        let shared_roots = self.shared_semantic_roots(context, cache);
        let legality = IselCoverLegality {
            shared_roots: &shared_roots,
        };
        for (pattern_index, compiled) in self.compiled_patterns.iter().enumerate() {
            let synth = compiled.synthesis.as_ref();
            let rule = if synth.is_some() {
                None
            } else {
                let rule = &self.rules[compiled.rule_index];
                if !self.target_model.supports_rule(rule.compiled_rule_id) {
                    continue;
                }
                Some(rule)
            };

            let Some(pattern_root) = compiled.pattern.root() else {
                continue;
            };

            for binding in VF2CoverDriver::matches_with_legality(
                context,
                &cache.dag,
                &compiled.pattern,
                &legality,
            ) {
                let root = SemNodeId(binding.graph_root().index() as u32);
                let Some(op_id) = cache.op_by_root.get(&root) else {
                    continue;
                };
                let Some(op_ref) = op_refs.get(op_id) else {
                    continue;
                };

                let mut captures = CaptureBindings::new();
                for (pattern_node, symbol) in &compiled.boundary_symbols {
                    captures.bind(
                        *symbol,
                        SemNodeId(binding.binding(*pattern_node).index() as u32),
                    );
                }

                let pattern_nodes = (0..compiled.pattern.len())
                    .map(NodeId::from_index)
                    .map(|pattern_node| PatternNodeBinding {
                        pattern_node,
                        sem_node: SemNodeId(binding.binding(pattern_node).index() as u32),
                        is_boundary: matches!(
                            compiled.pattern.get_node(pattern_node),
                            PatternExpr::Boundary
                        ),
                    })
                    .collect();
                let bindings = FullMatchBindings {
                    captures,
                    pattern_nodes,
                };

                let rule_match = bindings.captures.to_rule_match(&cache.dag);
                let (rule_index, cost) = if let Some(synth) = synth {
                    (0, synth.cost)
                } else {
                    let rule = rule.expect("non-synthesized pattern has a rule");
                    if !(rule.legality_fn)(context, op_ref, &rule_match, self.target_model.as_ref())
                    {
                        continue;
                    }
                    let pressure = SelectionPressure {
                        estimated_live_operands: op_ref.op().operands.len() as u32,
                        estimated_register_pressure: self
                            .target_model
                            .estimate_register_pressure(op_ref),
                    };
                    let cost = self.target_model.cost_model().node_cost(
                        context,
                        op_ref,
                        rule,
                        &rule_match,
                        &pressure,
                        self.target_model.as_ref(),
                    );
                    (
                        compiled.rule_index,
                        specificity_adjusted_cost(cost, compiled.specificity),
                    )
                };

                matches.push(PbqpIselMatch {
                    pattern_index,
                    rule_index,
                    root,
                    pattern_root,
                    bindings,
                    cost,
                });
            }
        }
        matches
    }

    fn shared_semantic_roots(
        &self,
        context: &Context,
        cache: &BlockSelectionCache,
    ) -> HashSet<SemNodeId> {
        let mut operand_uses = HashMap::<ValueId, usize>::new();
        for op_id in cache.op_by_root.values().copied() {
            let op = context.get_op(op_id);
            for operand in &op.operands {
                *operand_uses.entry(*operand).or_insert(0) += 1;
            }
        }

        let mut shared = HashSet::new();
        for (root, op_id) in &cache.op_by_root {
            let op = context.get_op(*op_id);
            if op
                .results
                .iter()
                .any(|result| operand_uses.get(result).copied().unwrap_or_default() > 1)
            {
                shared.insert(*root);
            }
        }
        shared
    }

}

fn alternatives_compatible(
    patterns: &[CompiledIselPattern],
    parent: SemNodeId,
    child: SemNodeId,
    parent_alt: &PbqpIselAlternative,
    child_alt: &PbqpIselAlternative,
    matches: &[PbqpIselMatch],
) -> bool {
    if let Some(requirement) = child_requirement(patterns, parent, child, parent_alt, matches) {
        return match requirement {
            ChildRequirement::Materialized => matches!(
                child_alt,
                PbqpIselAlternative::Root { .. } | PbqpIselAlternative::External
            ),
            ChildRequirement::SameMatch {
                match_id,
                pattern_node,
            } => matches!(
                child_alt,
                PbqpIselAlternative::Internal {
                    match_id: child_match,
                    pattern_node: child_pattern_node,
                } if *child_match == match_id && *child_pattern_node == pattern_node
            ),
        };
    }

    if let PbqpIselAlternative::Internal {
        match_id,
        pattern_node,
    } = child_alt
    {
        return parent_satisfies_internal_child(
            patterns,
            parent,
            child,
            parent_alt,
            *match_id,
            *pattern_node,
            matches,
        );
    }

    true
}

fn child_requirement(
    patterns: &[CompiledIselPattern],
    _parent: SemNodeId,
    child: SemNodeId,
    parent_alt: &PbqpIselAlternative,
    matches: &[PbqpIselMatch],
) -> Option<ChildRequirement> {
    let (match_id, parent_pattern_node) = match parent_alt {
        PbqpIselAlternative::Root { match_id } => {
            let m = &matches[*match_id];
            (*match_id, m.pattern_root)
        }
        PbqpIselAlternative::Internal {
            match_id,
            pattern_node,
        } => (*match_id, *pattern_node),
        PbqpIselAlternative::External => return None,
    };

    let m = &matches[match_id];
    let pattern = &patterns[m.pattern_index].pattern;
    for &pattern_child in pattern.children(parent_pattern_node) {
        if m.bindings.sem_node_for_pattern(pattern_child) != Some(child) {
            continue;
        }
        let is_boundary = m
            .bindings
            .pattern_nodes
            .iter()
            .find(|binding| binding.pattern_node == pattern_child)
            .is_some_and(|binding| binding.is_boundary);
        return if is_boundary {
            Some(ChildRequirement::Materialized)
        } else {
            Some(ChildRequirement::SameMatch {
                match_id,
                pattern_node: pattern_child,
            })
        };
    }

    None
}

/// Cost added to a *finite* parent -> child edge by the target objective. Only
/// materialization edges (parent reaches the child through an untyped boundary)
/// are priced; structural same-match edges stay at zero.
fn materialization_edge_cost(
    patterns: &[CompiledIselPattern],
    dag: &SemDagArena,
    parent: SemNodeId,
    child: SemNodeId,
    parent_alt: &PbqpIselAlternative,
    matches: &[PbqpIselMatch],
    cost_model: &dyn IselCostModel,
) -> u64 {
    let materialized = matches!(
        child_requirement(patterns, parent, child, parent_alt, matches),
        Some(ChildRequirement::Materialized)
    );
    if !materialized {
        return 0;
    }
    cost_model.edge_cost(dag.node(parent).kind, dag.node(child).kind, true)
}

fn parent_satisfies_internal_child(
    patterns: &[CompiledIselPattern],
    parent: SemNodeId,
    child: SemNodeId,
    parent_alt: &PbqpIselAlternative,
    child_match_id: usize,
    child_pattern_node: NodeId,
    matches: &[PbqpIselMatch],
) -> bool {
    let m = &matches[child_match_id];
    let pattern = &patterns[m.pattern_index].pattern;
    for pattern_parent in (0..pattern.len()).map(NodeId::from_index) {
        if !pattern.children(pattern_parent).contains(&child_pattern_node) {
            continue;
        }
        if m.bindings.sem_node_for_pattern(pattern_parent) != Some(parent) {
            continue;
        }
        if m.bindings.sem_node_for_pattern(child_pattern_node) != Some(child) {
            continue;
        }
        return match parent_alt {
            PbqpIselAlternative::Root { match_id } => {
                *match_id == child_match_id && pattern_parent == m.pattern_root
            }
            PbqpIselAlternative::Internal {
                match_id,
                pattern_node,
            } => *match_id == child_match_id && *pattern_node == pattern_parent,
            PbqpIselAlternative::External => false,
        };
    }

    false
}

enum ChildRequirement {
    Materialized,
    SameMatch {
        match_id: usize,
        pattern_node: NodeId,
    },
}

#[cfg(test)]
mod tests {
    use tir::{
        Context, IRBuilder, IRFormatter, Operation, PassManager, TypeId,
        builtin::{FuncOp, IntegerType, ops},
        graph::MutDag,
        sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
    };

    use super::{
        EmitPlan, InstructionSelectPass, IselCostModel, Rule, RuleMatch, SelectionPressure,
        TargetIselModel,
    };

    fn symbol(g: &mut ExprPostGraph, id: u32) -> tir::graph::NodeId {
        let node = g.add_node(ExprKind::Symbol);
        g.set_leaf_data(node, ExprPayload::SymbolId(id));
        node
    }

    fn binary(
        g: &mut ExprPostGraph,
        kind: ExprKind,
        lhs: tir::graph::NodeId,
        rhs: tir::graph::NodeId,
    ) -> tir::graph::NodeId {
        let node = g.add_node(kind);
        g.add_edge(node, lhs);
        g.add_edge(node, rhs);
        node
    }

    fn atomic_pattern(kind: ExprKind) -> ExprPostGraph {
        let mut g = ExprPostGraph::new();
        let lhs = symbol(&mut g, 0);
        let rhs = symbol(&mut g, 1);
        binary(&mut g, kind, lhs, rhs);
        g
    }

    fn add_mul_pattern() -> ExprPostGraph {
        let mut g = ExprPostGraph::new();
        let x = symbol(&mut g, 0);
        let y = symbol(&mut g, 1);
        let mul = binary(&mut g, ExprKind::Mul, x, y);
        let z = symbol(&mut g, 2);
        binary(&mut g, ExprKind::Add, mul, z);
        g
    }

    fn emit_add(
        context: &Context,
        op: &tir::OperationRef,
        m: &RuleMatch,
    ) -> Result<EmitPlan, tir::PassError> {
        let lhs = m
            .value_binding(0)
            .unwrap_or_else(|| op.op().operands.first().copied().unwrap());
        let rhs = m
            .value_binding(2)
            .or_else(|| m.value_binding(1))
            .unwrap_or_else(|| op.op().operands[1]);
        let result_ty = context.get_value(op.op().results[0]).ty();
        Ok(EmitPlan::single(Box::new(
            ops::addi(context, lhs, rhs, result_ty).build(),
        )))
    }

    fn emit_mul(
        context: &Context,
        op: &tir::OperationRef,
        _m: &RuleMatch,
    ) -> Result<EmitPlan, tir::PassError> {
        let result_ty = context.get_value(op.op().results[0]).ty();
        Ok(EmitPlan::single(Box::new(
            ops::muli(context, op.op().operands[0], op.op().operands[1], result_ty).build(),
        )))
    }

    #[test]
    fn pbqp_selector_consumes_internal_nodes_of_selected_pattern() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let x = context.create_value(i32_ty, None);
        let y = context.create_value(i32_ty, None);
        let z = context.create_value(i32_ty, None);
        let x_id = x.id();
        let y_id = y.id();
        let z_id = z.id();
        let region = context.create_region();
        let block = context.create_block(vec![x, y, z]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        let add = ops::addi(&context, mul_result, z_id, i32_ty).build();
        let add_result = add.result();
        fb.insert(add);
        fb.insert(ops::r#return(&context, add_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![
            Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
            Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        assert_eq!(body_ops, vec!["addi", "return"]);

        let mut buf = String::new();
        let mut fmt = IRFormatter::new(&mut buf);
        module.print(&mut fmt).expect("print lowered module");
        assert!(!buf.contains("muli"));
    }

    #[test]
    fn rule_validation_rejects_missing_atomic_materializer() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let x = context.create_value(i32_ty, None);
        let y = context.create_value(i32_ty, None);
        let z = context.create_value(i32_ty, None);
        let x_id = x.id();
        let y_id = y.id();
        let z_id = z.id();
        let region = context.create_region();
        let block = context.create_block(vec![x, y, z]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        let add = ops::addi(&context, mul_result, z_id, i32_ty).build();
        let add_result = add.result();
        fb.insert(add);
        fb.insert(ops::r#return(&context, add_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![
            Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        let err = pm
            .run(&context, context.get_op(module.id()))
            .expect_err("incomplete rule set should be rejected");
        assert!(err.to_string().contains("Mul"));
    }

    #[test]
    fn pbqp_selector_does_not_consume_shared_internal_nodes() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let x = context.create_value(i32_ty, None);
        let y = context.create_value(i32_ty, None);
        let z = context.create_value(i32_ty, None);
        let x_id = x.id();
        let y_id = y.id();
        let z_id = z.id();
        let region = context.create_region();
        let block = context.create_block(vec![x, y, z]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        let add0 = ops::addi(&context, mul_result, z_id, i32_ty).build();
        let add0_result = add0.result();
        fb.insert(add0);
        let add1 = ops::addi(&context, mul_result, add0_result, i32_ty).build();
        let add1_result = add1.result();
        fb.insert(add1);
        fb.insert(ops::r#return(&context, add1_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![
            Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
            Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        assert_eq!(body_ops, vec!["muli", "addi", "addi", "return"]);
    }

    fn add_mul_add_pattern() -> ExprPostGraph {
        let mut g = ExprPostGraph::new();
        let a = symbol(&mut g, 0);
        let b = symbol(&mut g, 1);
        let inner = binary(&mut g, ExprKind::Add, a, b);
        let c = symbol(&mut g, 2);
        let mul = binary(&mut g, ExprKind::Mul, inner, c);
        let d = symbol(&mut g, 3);
        binary(&mut g, ExprKind::Add, mul, d);
        g
    }

    /// A cost model that makes the fused `add-mul` rule prohibitively expensive,
    /// so selection must fall back to the atomic `mul` + `add` cover.
    struct NoFusionCostModel;

    impl IselCostModel for NoFusionCostModel {
        fn node_cost(
            &self,
            _context: &Context,
            _op: &tir::OperationRef,
            rule: &Rule,
            _m: &RuleMatch,
            _pressure: &SelectionPressure,
            _target: &dyn TargetIselModel,
        ) -> u64 {
            if rule.name == "add-mul" {
                1000
            } else {
                rule.base_cost as u64
            }
        }
    }

    struct NoFusionTarget {
        cost: NoFusionCostModel,
    }

    impl TargetIselModel for NoFusionTarget {
        fn cost_model(&self) -> &dyn IselCostModel {
            &self.cost
        }
    }

    #[test]
    fn cost_model_override_changes_selection() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let x = context.create_value(i32_ty, None);
        let y = context.create_value(i32_ty, None);
        let z = context.create_value(i32_ty, None);
        let (x_id, y_id, z_id) = (x.id(), y.id(), z.id());
        let region = context.create_region();
        let block = context.create_block(vec![x, y, z]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        let add = ops::addi(&context, mul_result, z_id, i32_ty).build();
        let add_result = add.result();
        fb.insert(add);
        fb.insert(ops::r#return(&context, add_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![
            Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
            Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name()).add_pass(
            InstructionSelectPass::new(rules).with_target_model(Box::new(NoFusionTarget {
                cost: NoFusionCostModel,
            })),
        );
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        // With fusion priced out, the default add-mul cost-1 win is overridden.
        assert_eq!(body_ops, vec!["muli", "addi", "return"]);
    }

    #[test]
    fn analysis_flags_missing_subpattern() {
        let rules = vec![
            Rule::new("add-mul-add", add_mul_add_pattern(), 100, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
            Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
        ];

        let pass = InstructionSelectPass::new(rules);
        let missing: Vec<String> = pass
            .rule_set_analysis()
            .missing_composable()
            .iter()
            .map(|required| required.describe())
            .collect();

        assert!(
            missing.iter().any(|d| d.contains("Mul(Add")),
            "expected a missing Mul(Add(...)) subpattern, got {missing:?}"
        );
    }

    #[test]
    fn synthesis_keeps_selection_valid() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let a = context.create_value(i32_ty, None);
        let b = context.create_value(i32_ty, None);
        let c = context.create_value(i32_ty, None);
        let d = context.create_value(i32_ty, None);
        let (a_id, b_id, c_id, d_id) = (a.id(), b.id(), c.id(), d.id());
        let region = context.create_region();
        let block = context.create_block(vec![a, b, c, d]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let add0 = ops::addi(&context, a_id, b_id, i32_ty).build();
        let add0_result = add0.result();
        fb.insert(add0);
        let mul = ops::muli(&context, add0_result, c_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        let add1 = ops::addi(&context, mul_result, d_id, i32_ty).build();
        let add1_result = add1.result();
        fb.insert(add1);
        fb.insert(ops::r#return(&context, add1_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        // `add-mul-add` requires a `Mul(Add(_,_),_)` subpattern that no rule
        // provides; the pass synthesizes it. Selection must remain valid and, with
        // fusion priced high, fall back to the atomic cover.
        let rules = vec![
            Rule::new("add-mul-add", add_mul_add_pattern(), 100, emit_add),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
            Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        assert_eq!(body_ops, vec!["addi", "muli", "addi", "return"]);
    }

    /// A binary pattern constrained to a specific result type via the pattern
    /// graph's actual-type annotation (the channel a typed rule would use).
    fn typed_binary_pattern(kind: ExprKind, ty: TypeId) -> ExprPostGraph {
        let mut g = ExprPostGraph::new();
        let lhs = symbol(&mut g, 0);
        let rhs = symbol(&mut g, 1);
        let root = binary(&mut g, kind, lhs, rhs);
        g.set_actual_type(root, ty);
        g
    }

    fn emit_sub(
        context: &Context,
        op: &tir::OperationRef,
        _m: &RuleMatch,
    ) -> Result<EmitPlan, tir::PassError> {
        let result_ty = context.get_value(op.op().results[0]).ty();
        Ok(EmitPlan::single(Box::new(
            ops::subi(context, op.op().operands[0], op.op().operands[1], result_ty).build(),
        )))
    }

    #[test]
    fn selection_is_type_aware() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let i64_ty = IntegerType::new(&context, 64);
        let a32v = context.create_value(i32_ty, None);
        let b32v = context.create_value(i32_ty, None);
        let a64v = context.create_value(i64_ty, None);
        let b64v = context.create_value(i64_ty, None);
        let (a32, b32, a64, b64) = (a32v.id(), b32v.id(), a64v.id(), b64v.id());
        let region = context.create_region();
        let block = context.create_block(vec![a32v, b32v, a64v, b64v]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i64_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let add32 = ops::addi(&context, a32, b32, i32_ty).build();
        fb.insert(add32);
        let add64 = ops::addi(&context, a64, b64, i64_ty).build();
        let add64_result = add64.result();
        fb.insert(add64);
        fb.insert(ops::r#return(&context, add64_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        // Same opcode (Add), two result widths. The i32-constrained rule must only
        // fire on the i32 add; the i64 add falls back to the width-agnostic rule.
        let rules = vec![
            Rule::new("add.i32", typed_binary_pattern(ExprKind::Add, i32_ty), 1, emit_sub),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        // i32 add -> the type-constrained rule (subi stand-in); i64 add -> fallback addi.
        assert_eq!(body_ops, vec!["subi", "addi", "return"]);
    }

    /// Build `add(add(a,b), c)` over i32 values and select it with a fused
    /// `Add(Add(_,_),_)` rule whose *internal* node carries `inner_width` as a type
    /// constraint (plus an untyped atomic `add` fallback). Returns the lowered op
    /// names. Fusion (the `subi` marker) only happens when the inner constraint
    /// agrees with the inferred i32 type of the inner add.
    fn run_inner_typed_fusion(inner_width: Option<u32>) -> Vec<&'static str> {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i32_ty = IntegerType::new(&context, 32);
        let a = context.create_value(i32_ty, None);
        let b = context.create_value(i32_ty, None);
        let c = context.create_value(i32_ty, None);
        let (a_id, b_id, c_id) = (a.id(), b.id(), c.id());
        let region = context.create_region();
        let block = context.create_block(vec![a, b, c]);
        region.add_block(block.id());

        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let add0 = ops::addi(&context, a_id, b_id, i32_ty).build();
        let add0_result = add0.result();
        fb.insert(add0);
        let add1 = ops::addi(&context, add0_result, c_id, i32_ty).build();
        let add1_result = add1.result();
        fb.insert(add1);
        fb.insert(ops::r#return(&context, add1_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        // Fused pattern Add(Add(s0, s1), s2); optionally constrain the inner Add.
        let mut pattern = ExprPostGraph::new();
        let s0 = symbol(&mut pattern, 0);
        let s1 = symbol(&mut pattern, 1);
        let inner = binary(&mut pattern, ExprKind::Add, s0, s1);
        let s2 = symbol(&mut pattern, 2);
        binary(&mut pattern, ExprKind::Add, inner, s2);
        if let Some(width) = inner_width {
            pattern.set_actual_type(inner, IntegerType::new(&context, width));
        }

        let rules = vec![
            Rule::new("add-add", pattern, 1, emit_sub),
            Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect()
    }

    #[test]
    fn internal_node_type_constraint_is_enforced() {
        // Inner add inferred as i32 from i32 operands. A matching i32 constraint
        // (or no constraint) lets the fused rule consume it; an i64 constraint
        // forbids the match, falling back to two atomic adds.
        assert_eq!(run_inner_typed_fusion(Some(32)), vec!["subi", "return"]);
        assert_eq!(run_inner_typed_fusion(None), vec!["subi", "return"]);
        assert_eq!(
            run_inner_typed_fusion(Some(64)),
            vec!["addi", "addi", "return"]
        );
    }
}

impl Pass for InstructionSelectPass {
    fn name(&self) -> &'static str {
        "instruction-select"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Any
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        for lowering in &self.op_lowerings {
            if lowering(context, op, rewriter)? {
                return Ok(());
            }
        }

        if op.op().results.is_empty() {
            return Ok(());
        }

        let Some(block) = op.block() else {
            return Ok(());
        };

        if self.target_model.is_pbqp_enabled() {
            self.commit_block_solution(context, block, rewriter)?;
        }

        Ok(())
    }
}
