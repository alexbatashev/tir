use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use tir::{
    Block, BlockId, Context, OpId, OpInstance, Operation, OperationRef, Pass, PassError,
    PassTarget, Rewriter, TypeId, ValueId,
    attributes::AttributeValue,
    builtin::IntegerType,
    graph::{
        Dag, EClassId, EGraph, EMatch, ENode, Node, NodeId, OperandConstraint, Pattern,
        PatternExpr, Rewrite,
    },
    pbqp::{self, INF_COST, PbqpAlternative, PbqpMatrix, PbqpProblem},
    sem_expr::{
        ExprKind, ExprPayload, ExprPostGraph, FuzzOracle, confirm_extension_via_shifts,
        infer_widths,
    },
    utils::APInt,
};

/// The semantic e-graph instruction selection operates over: e-classes of
/// equivalent semantic expressions for the values computed in a block.
pub type SemEGraph = EGraph<SemNode, ()>;

/// An e-graph node label: the operator identity (kind/payload) plus the IR type of
/// the value it represents. Structure (operands) lives in the enclosing
/// [`ENode::children`], never here — so a label is exactly what hash-consing and
/// pattern matching compare.
///
/// `ty` is the result type for an op node, the value type for a leaf. `None` on a
/// *pattern* node means "match any type"; `None` on a *graph* node means the type
/// is unknown (e.g. an intermediate node of a multi-node semantic expansion). The
/// type is stored verbatim from the IR — no width is collapsed or normalized — so
/// every target can constrain on exactly the widths/classes it distinguishes
/// (x86/AArch64 8/16/32/64-bit forms, RISC-V word vs XLEN, vector element types,
/// floats), and untyped rules stay width-agnostic.
#[derive(Clone, Debug)]
pub struct SemNode {
    pub kind: ExprKind,
    pub payload: Option<ExprPayload>,
    pub ty: Option<TypeId>,
}

impl PartialEq for SemNode {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.payload == other.payload && self.ty == other.ty
    }
}

impl Eq for SemNode {}

/// Hashes exactly the fields compared by [`PartialEq`] — the operator label
/// (kind, payload, type) — so the e-graph hash-cons treats two e-nodes as congruent
/// iff they have the same label and the same operand classes.
impl Hash for SemNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.ty.hash(state);
        match &self.payload {
            None => 0u8.hash(state),
            Some(ExprPayload::SymbolId(s)) => {
                1u8.hash(state);
                s.hash(state);
            }
            Some(ExprPayload::Value(v)) => {
                2u8.hash(state);
                v.number().hash(state);
            }
            Some(ExprPayload::Int(i)) => {
                3u8.hash(state);
                i.width().hash(state);
                i.is_signed().hash(state);
                i.to_u64().hash(state);
            }
            Some(ExprPayload::Float(f)) => {
                4u8.hash(state);
                f.to_f64().to_bits().hash(state);
            }
        }
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
        self.kind.is_commutative()
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

/// The concrete operand a capture e-class resolves to.
enum Binding {
    Int(APInt),
    Value(ValueId),
}

/// Resolve one capture e-class to its operand binding: a constant immediate, then
/// an input value, then the IR value an intermediate result produces (looked up in
/// `class_value`, the map recording which class computes which op result). `None` if
/// the class carries no materializable operand. This is the single resolution rule
/// used by both match collection and emission.
fn class_binding(
    egraph: &SemEGraph,
    class_value: &HashMap<EClassId, ValueId>,
    class: EClassId,
) -> Option<Binding> {
    let nodes = egraph.nodes(class);
    if let Some(ExprPayload::Int(v)) = nodes.iter().find_map(|n| {
        n.node
            .payload
            .as_ref()
            .filter(|p| matches!(p, ExprPayload::Int(_)))
    }) {
        Some(Binding::Int(v.clone()))
    } else if let Some(v) = nodes.iter().find_map(|n| match n.node.payload.as_ref() {
        Some(ExprPayload::Value(v)) => Some(*v),
        _ => None,
    }) {
        Some(Binding::Value(v))
    } else {
        class_value
            .get(&egraph.find(class))
            .copied()
            .map(Binding::Value)
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
    pub fn with_operand_constraints(mut self, constraints: Vec<(u32, OperandConstraint)>) -> Self {
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
    entries: Vec<(u32, EClassId)>,
}

impl CaptureBindings {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn bind(&mut self, symbol: u32, class: EClassId) -> bool {
        if let Some((_, existing)) = self.entries.iter().find(|(sym, _)| *sym == symbol) {
            *existing == class
        } else {
            self.entries.push((symbol, class));
            true
        }
    }

    fn to_rule_match(
        &self,
        egraph: &SemEGraph,
        class_value: &HashMap<EClassId, ValueId>,
    ) -> RuleMatch {
        let mut int_bindings = Vec::new();
        let mut value_bindings = Vec::new();
        for (sym, class) in &self.entries {
            match class_binding(egraph, class_value, *class) {
                Some(Binding::Int(v)) => int_bindings.push((*sym, v)),
                Some(Binding::Value(v)) => value_bindings.push((*sym, v)),
                None => {}
            }
        }
        RuleMatch::new(int_bindings, value_bindings)
    }
}

#[derive(Clone, Debug)]
struct PatternNodeBinding {
    pattern_node: NodeId,
    class: EClassId,
    is_boundary: bool,
}

#[derive(Clone, Debug)]
struct FullMatchBindings {
    captures: CaptureBindings,
    pattern_nodes: Vec<PatternNodeBinding>,
}

impl FullMatchBindings {
    fn class_for_pattern(&self, pattern_node: NodeId) -> Option<EClassId> {
        self.pattern_nodes
            .iter()
            .find(|binding| binding.pattern_node == pattern_node)
            .map(|binding| binding.class)
    }
}

/// Builds a block's semantic expressions straight into the e-graph: every lowered
/// node is hash-consed by [`SemEGraph::add`], so the e-graph *is* the interned DAG
/// (no separate arena). Returns [`EClassId`]s and records, in `class_value`, which
/// class computes which op result so an intermediate can later be materialized as a
/// register value.
struct SemDagBuilder<'a> {
    context: &'a Context,
    value_to_def: &'a HashMap<ValueId, OpId>,
    egraph: &'a mut SemEGraph,
    /// The e-class built for each already-lowered IR value (operand sharing / CSE).
    value_to_class: HashMap<ValueId, EClassId>,
    /// First class found to compute each op result (first writer wins, matching CSE).
    class_value: HashMap<EClassId, ValueId>,
}

impl<'a> SemDagBuilder<'a> {
    fn new(
        context: &'a Context,
        value_to_def: &'a HashMap<ValueId, OpId>,
        egraph: &'a mut SemEGraph,
    ) -> Self {
        Self {
            context,
            value_to_def,
            egraph,
            value_to_class: HashMap::new(),
            class_value: HashMap::new(),
        }
    }

    fn add_leaf(
        &mut self,
        kind: ExprKind,
        payload: Option<ExprPayload>,
        ty: Option<TypeId>,
    ) -> EClassId {
        self.egraph
            .add(ENode::leaf(SemNode { kind, payload, ty }, None))
    }

    fn add_int(&mut self, value: APInt, ty: Option<TypeId>) -> EClassId {
        self.add_leaf(ExprKind::Constant, Some(ExprPayload::Int(value)), ty)
    }

    fn add_input_value(&mut self, value: ValueId, ty: Option<TypeId>) -> EClassId {
        self.add_leaf(ExprKind::Symbol, Some(ExprPayload::Value(value)), ty)
    }

    fn add_unknown_symbol(&mut self, symbol: u32, ty: Option<TypeId>) -> EClassId {
        self.add_leaf(ExprKind::Symbol, Some(ExprPayload::SymbolId(symbol)), ty)
    }

    /// A leaf that nothing materializes — the placeholder for an un-lowerable node,
    /// so a partial semantic expansion still yields a well-formed graph.
    fn add_opaque(&mut self) -> EClassId {
        self.add_leaf(
            ExprKind::Symbol,
            Some(ExprPayload::SymbolId(u32::MAX)),
            None,
        )
    }

    fn add_op(
        &mut self,
        kind: ExprKind,
        mut children: Vec<EClassId>,
        ty: Option<TypeId>,
    ) -> EClassId {
        // Canonicalize commutative operands so `a op b` and `b op a` hash-cons to the
        // same e-node, mirroring the program's CSE.
        if kind.is_commutative() {
            children.sort();
        }
        self.egraph.add(ENode::op(
            SemNode {
                kind,
                payload: None,
                ty,
            },
            children,
        ))
    }

    /// Record that `class` computes IR `value` (idempotent; first writer wins, which
    /// is correct since identical computations are the same value under CSE).
    fn set_value(&mut self, class: EClassId, value: ValueId) {
        self.class_value
            .entry(self.egraph.find(class))
            .or_insert(value);
    }

    fn build_for_op(&mut self, op: &std::sync::Arc<OpInstance>) -> Option<EClassId> {
        let mut operands = Vec::with_capacity(op.operands.len());
        for operand in &op.operands {
            operands.push(self.build_from_value(*operand));
        }
        let mut graph = ExprPostGraph::new();
        let root = op.clone().as_dyn_op().semantic_expr(&mut graph)?;
        let widths = self.infer_local_widths(&graph, &operands);
        let class = self.lower_graph_node(&graph, root, &operands, &widths);
        if let Some(result) = op.results.first() {
            self.set_value(class, *result);
        }
        Some(class)
    }

    fn build_from_value(&mut self, value: ValueId) -> EClassId {
        if let Some(existing) = self.value_to_class.get(&value) {
            return *existing;
        }

        let value_ty = Some(self.context.get_value(value).ty());
        let class = if let Some(def_op_id) = self.value_to_def.get(&value) {
            let def = self.context.get_op(*def_op_id);
            if def.name == "constant" {
                match def.attributes.iter().find(|a| a.name == "value") {
                    Some(attr) => match &attr.value {
                        AttributeValue::Int(v) => self.add_int(APInt::new_signed(64, *v), value_ty),
                        _ => self.add_input_value(value, value_ty),
                    },
                    None => self.add_input_value(value, value_ty),
                }
            } else {
                let mut graph = ExprPostGraph::new();
                if let Some(root) = def.clone().as_dyn_op().semantic_expr(&mut graph) {
                    let mut operands = Vec::with_capacity(def.operands.len());
                    for operand in &def.operands {
                        operands.push(self.build_from_value(*operand));
                    }
                    let widths = self.infer_local_widths(&graph, &operands);
                    let class = self.lower_graph_node(&graph, root, &operands, &widths);
                    self.set_value(class, value);
                    class
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

    /// Infer the width of every node of `graph` from the IR types of the operands
    /// it references, then resolve those widths against the live context. This is
    /// the same width rule TMDL uses for patterns, so the program graph and the
    /// rule patterns end up typed consistently.
    fn infer_local_widths(&self, graph: &ExprPostGraph, operands: &[EClassId]) -> Vec<Option<u32>> {
        infer_widths(graph, |node| match graph.get_leaf_data(node) {
            Some(ExprPayload::SymbolId(id)) => operands
                .get(*id as usize)
                .and_then(|&class| self.class_ty(class))
                .and_then(|ty| type_width(self.context, ty)),
            _ => None,
        })
    }

    /// The IR type recorded on an operand class (taken from any member carrying one).
    fn class_ty(&self, class: EClassId) -> Option<TypeId> {
        self.egraph.nodes(class).iter().find_map(|n| n.node.ty)
    }

    /// Lower one node of a semantic-expression graph, typing each node from its
    /// inferred width. Operand leaves keep the IR type they were built with;
    /// internal nodes (and the root) take their inferred width resolved to a type.
    fn lower_graph_node(
        &mut self,
        graph: &ExprPostGraph,
        node: NodeId,
        operands: &[EClassId],
        widths: &[Option<u32>],
    ) -> EClassId {
        let node_ty = widths[node.index()].map(|width| IntegerType::new(self.context, width));
        match graph.get_node(node) {
            ExprKind::Symbol => match graph.get_leaf_data(node) {
                Some(ExprPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .copied()
                    .unwrap_or_else(|| self.add_unknown_symbol(*id, node_ty)),
                _ => self.add_opaque(),
            },
            ExprKind::Constant => match graph.get_leaf_data(node) {
                Some(ExprPayload::Int(v)) => self.add_int(v.clone(), node_ty),
                _ => self.add_opaque(),
            },
            kind => {
                let children: Vec<EClassId> = graph
                    .children(node)
                    .map(|child| self.lower_graph_node(graph, child, operands, widths))
                    .collect();
                if kind.num_children(self.context) == children.len() {
                    self.add_op(*kind, children, node_ty)
                } else {
                    self.add_opaque()
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
    root: EClassId,
    pattern_root: NodeId,
    bindings: FullMatchBindings,
    cost: u64,
}

struct BlockSelectionCache {
    egraph: SemEGraph,
    /// The e-class that produces each op's result value.
    op_by_root: HashMap<EClassId, OpId>,
    /// The IR value each (canonical) e-class computes, so an operand resolving to an
    /// intermediate result can be materialized as that register value at emit time.
    class_value: HashMap<EClassId, ValueId>,
    /// E-classes used as an operand by more than one consumer; such a value must be
    /// materialized into a register and cannot be internalized into a match.
    shared_classes: HashSet<EClassId>,
    /// The solved emission plan, or the completeness error explaining why the block
    /// cannot be selected with this rule set.
    plan: Option<Result<BlockPlan, String>>,
}

/// The emission plan for a block: how each original op is rewritten, plus the extra
/// instructions to insert for rewrite-introduced e-classes that have no original op
/// (the `slli` of a `slli`/`srai` sign-extension expansion).
#[derive(Clone, Debug, Default)]
struct BlockPlan {
    op_decisions: HashMap<OpId, BlockDecision>,
    introduced: Vec<IntroducedEmit>,
}

/// An instruction to materialize for an introduced e-class: emitted with a fresh
/// destination value and inserted just before `anchor` (the source op whose
/// expansion produced it). Operands precede consumers in `BlockPlan::introduced`.
#[derive(Clone, Debug)]
struct IntroducedEmit {
    rule_index: usize,
    m: RuleMatch,
    dest: ValueId,
    dest_ty: TypeId,
    anchor: OpId,
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
    SemNode { kind, payload, ty }
}

/// A solved cover: the chosen alternative for every PBQP node, the e-class each
/// PBQP node stands for (same index), and the achieved cost.
struct DagCover {
    choices: Vec<PbqpIselAlternative>,
    classes: Vec<EClassId>,
}

/// Build and solve the PBQP cover over the e-graph: one PBQP node per e-class,
/// alternatives drawn from the instruction-pattern `matches`, and parent -> child
/// compatibility derived from each match's pattern structure (not a single DAG
/// shape, since a class may be realized by several equivalent e-nodes). The
/// `edge_cost` closure prices satisfied materialization edges. Returns `None` if
/// the instance is infeasible (a class with no valid alternative).
fn build_eclass_cover(
    egraph: &SemEGraph,
    op_by_root: &HashMap<EClassId, OpId>,
    patterns: &[CompiledIselPattern],
    matches: &[PbqpIselMatch],
    edge_cost: impl Fn(EClassId, EClassId, &PbqpIselAlternative) -> u64,
) -> Option<DagCover> {
    let classes: Vec<EClassId> = egraph.classes().map(|c| egraph.find(c)).collect();
    let index: HashMap<EClassId, usize> =
        classes.iter().enumerate().map(|(i, &c)| (c, i)).collect();
    let class_index = |c: EClassId| index[&egraph.find(c)];

    let is_terminal = |c: EClassId| egraph.nodes(c).iter().any(|n| n.children.is_empty());

    let mut alternatives_by_node = vec![Vec::<PbqpIselAlternative>::new(); classes.len()];
    for (i, &c) in classes.iter().enumerate() {
        if is_terminal(c) {
            alternatives_by_node[i].push(PbqpIselAlternative::External);
        }
    }

    for (match_id, m) in matches.iter().enumerate() {
        alternatives_by_node[class_index(m.root)].push(PbqpIselAlternative::Root { match_id });
        for binding in &m.bindings.pattern_nodes {
            if binding.is_boundary || binding.pattern_node == m.pattern_root {
                continue;
            }
            alternatives_by_node[class_index(binding.class)].push(PbqpIselAlternative::Internal {
                match_id,
                pattern_node: binding.pattern_node,
            });
        }
    }

    for (i, &c) in classes.iter().enumerate() {
        if alternatives_by_node[i].is_empty() && (is_terminal(c) || !op_by_root.contains_key(&c)) {
            alternatives_by_node[i].push(PbqpIselAlternative::External);
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
                    PbqpIselAlternative::Root {
                        match_id: alt_match,
                    }
                    | PbqpIselAlternative::Internal {
                        match_id: alt_match,
                        ..
                    } => *alt_match == match_id,
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

    // Edges follow each match's pattern structure: a (parent class -> operand
    // class) relation for every pattern edge of every match. Deduplicated so each
    // ordered class pair is priced once.
    let mut edge_pairs: HashSet<(usize, usize)> = HashSet::new();
    for m in matches {
        let pattern = &patterns[m.pattern_index].pattern;
        for pp in (0..pattern.len()).map(NodeId::from_index) {
            let Some(parent_class) = m.bindings.class_for_pattern(pp) else {
                continue;
            };
            for &pc in pattern.children(pp) {
                let Some(child_class) = m.bindings.class_for_pattern(pc) else {
                    continue;
                };
                let pi = class_index(parent_class);
                let ci = class_index(child_class);
                if pi != ci {
                    edge_pairs.insert((pi, ci));
                }
            }
        }
    }

    for (pi, ci) in edge_pairs {
        let (parent_class, child_class) = (classes[pi], classes[ci]);
        let parent_alts = &alternatives_by_node[pi];
        let child_alts = &alternatives_by_node[ci];
        let mut matrix = PbqpMatrix::zero(parent_alts.len(), child_alts.len());

        for (parent_alt_idx, parent_alt) in parent_alts.iter().enumerate() {
            for (child_alt_idx, child_alt) in child_alts.iter().enumerate() {
                if !alternatives_compatible(
                    patterns,
                    parent_class,
                    child_class,
                    parent_alt,
                    child_alt,
                    matches,
                ) {
                    matrix.set(parent_alt_idx, child_alt_idx, INF_COST);
                    continue;
                }
                let cost = edge_cost(parent_class, child_class, parent_alt);
                if cost != 0 {
                    matrix.set(parent_alt_idx, child_alt_idx, cost);
                }
            }
        }

        problem.add_edge(
            pbqp::PbqpNodeId::from_index(pi),
            pbqp::PbqpNodeId::from_index(ci),
            matrix,
        );
    }

    let solution = pbqp::solve(&problem).ok()?;
    let choices = solution
        .choices
        .iter()
        .copied()
        .enumerate()
        .map(|(node, choice)| alternatives_by_node[node][choice].clone())
        .collect();
    Some(DagCover { choices, classes })
}

/// The semantic kinds for which the rule set provides an atomic materializer (a
/// pattern whose root is that kind with only operand boundaries beneath it).
fn atomic_kinds(patterns: &[CompiledIselPattern]) -> HashSet<ExprKind> {
    let ctx = Context::default();
    let mut kinds = HashSet::new();
    for compiled in patterns {
        let Some(root) = compiled.pattern.root() else {
            continue;
        };
        let PatternExpr::Node(root_node) = compiled.pattern.get_node(root) else {
            continue;
        };
        if root_node.kind.num_children(&ctx) == 0 {
            continue;
        }
        let children = compiled.pattern.children(root);
        if !children.is_empty()
            && children
                .iter()
                .all(|&child| matches!(compiled.pattern.get_node(child), PatternExpr::Boundary))
        {
            kinds.insert(root_node.kind);
        }
    }
    kinds
}

/// The integer width of an e-class, taken from whichever member carries a known
/// integer type (the original IR node keeps its type; rewrite-introduced nodes are
/// left untyped).
fn class_width(ctx: &Context, egraph: &SemEGraph, class: EClassId) -> Option<u32> {
    egraph
        .nodes(class)
        .iter()
        .find_map(|n| n.node.ty.and_then(|ty| type_width(ctx, ty)))
}

/// Discover the algebraic bridges the rule set needs to cover sub-word extensions.
/// If the target has `slli` plus the matching right shift, confirm the standard
/// shift-pair identity against the [`FuzzOracle`] and, on success, emit a
/// width-parameterized rewrite. No hand-written selection rule is involved — only a
/// proved bit-vector lemma the target's own instructions happen to realize.
fn discover_rewrites(patterns: &[CompiledIselPattern]) -> Vec<Rewrite<SemNode, ()>> {
    let atomics = atomic_kinds(patterns);
    if !atomics.contains(&ExprKind::ShiftLeft) {
        return Vec::new();
    }
    let oracle = FuzzOracle::default();
    let mut rewrites = Vec::new();
    for (ext_kind, shr_kind) in [
        (ExprKind::SExt, ExprKind::ShiftRightArithmetic),
        (ExprKind::ZExt, ExprKind::ShiftRightLogic),
    ] {
        if atomics.contains(&shr_kind) && confirm_extension_via_shifts(ext_kind, shr_kind, &oracle)
        {
            rewrites.push(extension_rewrite(ext_kind, shr_kind));
        }
    }
    rewrites
}

/// Build the rewrite `ext_kind(v, W) -> shr_kind(shl(v, W - n), W - n)` with
/// `n = width(v)`. The introduced shift nodes are left untyped so they match the
/// target's width-agnostic shift patterns, and the shift amount is a fresh constant.
fn extension_rewrite(ext_kind: ExprKind, shr_kind: ExprKind) -> Rewrite<SemNode, ()> {
    let mut searcher = Pattern::<SemNode, ()>::new(());
    let value = searcher.add_node(PatternExpr::Boundary);
    searcher.set_duplicable(value, true);
    let width = searcher.add_node(PatternExpr::Boundary);
    searcher.set_duplicable(width, true);
    let root = searcher.add_node(PatternExpr::Node(template_node(ext_kind, None, None)));
    searcher.add_edge(root, value);
    searcher.add_edge(root, width);
    searcher.set_root(root);

    Rewrite {
        name: format!("{ext_kind:?}-via-shifts"),
        searcher,
        apply: Box::new(move |ctx: &Context, egraph: &mut SemEGraph, m: &EMatch| {
            let root_class = m.root();
            let value_class = m.binding(value);
            let (Some(w), Some(n)) = (
                class_width(ctx, egraph, root_class),
                class_width(ctx, egraph, value_class),
            ) else {
                return;
            };
            if n >= w {
                return;
            }
            let amount = template_node(
                ExprKind::Constant,
                Some(ExprPayload::Int(APInt::new(64, (w - n) as u64))),
                None,
            );
            let shift_amount = egraph.add(ENode::leaf(amount, None));
            let shl = egraph.add(ENode::op(
                template_node(ExprKind::ShiftLeft, None, None),
                vec![value_class, shift_amount],
            ));
            let shr = egraph.add(ENode::op(
                template_node(shr_kind, None, None),
                vec![shl, shift_amount],
            ));
            egraph.union(root_class, shr);
        }),
    }
}

pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    compiled_patterns: Vec<CompiledIselPattern>,
    /// Target-independent algebraic identities the program e-graph is saturated
    /// with before covering (e.g. discovered `sext`/shift bridges). Populated by
    /// rewrite discovery; empty means selection is purely syntactic tiling.
    rewrites: Vec<tir::graph::Rewrite<SemNode, ()>>,
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
        let compiled_patterns: Vec<_> = rules
            .iter()
            .enumerate()
            .filter_map(|(rule_index, rule)| {
                compile_isel_pattern(rule_index, &rule.pattern, &rule.operand_constraints)
            })
            .collect();

        let rewrites = discover_rewrites(&compiled_patterns);

        Self {
            rules,
            compiled_patterns,
            rewrites,
            target_model: Box::new(DefaultTargetIselModel),
            op_lowerings: vec![],
            block_cache: HashMap::new(),
            emitted_blocks: HashSet::new(),
        }
    }

    /// Install the algebraic identities used to saturate the program e-graph before
    /// covering. These are proved equivalences (target-independent bit-vector
    /// lemmas, or sequences discovered against the target's own instructions), so
    /// the rule set stays free of hand-written selection rules.
    pub fn with_rewrites(mut self, rewrites: Vec<tir::graph::Rewrite<SemNode, ()>>) -> Self {
        self.rewrites = rewrites;
        self
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

        // Build every op's semantic expression directly into the e-graph (it
        // hash-conses, so it is itself the interned DAG), then saturate with the
        // algebraic identities. Class ids are resolved through `find` afterwards
        // because saturation may have merged classes.
        let mut egraph = SemEGraph::new();
        let mut roots_by_op = HashMap::new();
        let op_ids = block.op_ids();
        let class_value = {
            let mut builder = SemDagBuilder::new(context, &value_to_def, &mut egraph);
            for op_id in &op_ids {
                let op = context.get_op(*op_id);
                if op.results.is_empty() {
                    continue;
                }
                if let Some(root) = builder.build_for_op(&op) {
                    roots_by_op.insert(*op_id, root);
                }
            }
            builder.class_value
        };
        egraph.saturate(context, &self.rewrites, Default::default());

        let op_by_root: HashMap<EClassId, OpId> = roots_by_op
            .iter()
            .map(|(op, root)| (egraph.find(*root), *op))
            .collect();

        // Re-canonicalize the class -> value map through `find`, since saturation may
        // have merged classes. No two value-carrying classes merge under the current
        // rewrites, so first-writer-wins keeps it unambiguous.
        let mut canon_class_value: HashMap<EClassId, ValueId> = HashMap::new();
        for (class, value) in class_value {
            canon_class_value.entry(egraph.find(class)).or_insert(value);
        }

        // A value used as an operand by more than one consumer must stay a register.
        let mut operand_uses: HashMap<ValueId, usize> = HashMap::new();
        for op_id in &op_ids {
            for operand in &context.get_op(*op_id).operands {
                *operand_uses.entry(*operand).or_insert(0) += 1;
            }
        }
        let mut shared_classes = HashSet::new();
        for (op_id, root) in &roots_by_op {
            let op = context.get_op(*op_id);
            if op
                .results
                .iter()
                .any(|r| operand_uses.get(r).copied().unwrap_or(0) > 1)
            {
                shared_classes.insert(egraph.find(*root));
            }
        }

        self.block_cache.insert(
            block.id(),
            BlockSelectionCache {
                egraph,
                op_by_root,
                class_value: canon_class_value,
                shared_classes,
                plan: None,
            },
        );
    }

    fn ensure_block_solution(&mut self, context: &Context, block: &Block) {
        self.ensure_block_cache(context, block);
        let Some(cache) = self.block_cache.get(&block.id()) else {
            return;
        };
        if cache.plan.is_some() {
            return;
        }

        let plan = self.solve_block(context, block, cache);
        if let Some(cache) = self.block_cache.get_mut(&block.id()) {
            cache.plan = Some(plan);
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

        self.ensure_block_solution(context, block);
        let plan = match self
            .block_cache
            .get(&block.id())
            .and_then(|cache| cache.plan.clone())
        {
            Some(Ok(plan)) => plan,
            Some(Err(message)) => return Err(PassError::InvalidRuleSet(message)),
            None => return Ok(()),
        };

        let block_arc = context.get_block(block.id());

        // Insert the rewrite-introduced instructions first, in operand-first order,
        // each ahead of its anchor op. A synthetic op-ref carries the fresh
        // destination value so the emitter reads it as the result register.
        for intro in &plan.introduced {
            let synthetic = synthetic_op_ref(context, &block_arc, intro.dest, intro.dest_ty);
            let rule = &self.rules[intro.rule_index];
            let new_op = (rule.emit_plan_fn)(context, &synthetic, &intro.m)?.into_op();
            let anchor =
                OperationRef::new(context.get_op(intro.anchor), Some(block_arc.clone()), None);
            rewriter.insert_op_before(&anchor, new_op.as_ref())?;
        }

        // Rewrite the original ops (positions are resolved by id, so insertions
        // above do not invalidate this).
        for (op_id, decision) in &plan.op_decisions {
            let op_ref = OperationRef::new(context.get_op(*op_id), Some(block_arc.clone()), None);
            match decision {
                BlockDecision::Emit { rule_index, m } => {
                    let rule = &self.rules[*rule_index];
                    let new_op = (rule.emit_plan_fn)(context, &op_ref, m)?.into_op();
                    rewriter.replace_op(&op_ref, new_op.as_ref())?;
                }
                BlockDecision::Consume => {
                    rewriter.erase_op(&op_ref)?;
                }
            }
        }

        // Drop constants left dead by selection: an immediate operand folds its
        // constant into the instruction's attribute (e.g. `slliw`'s `imm`), so the
        // defining `constant` op no longer feeds anything. It binds to an *immediate
        // boundary*, never an interior node, so the cover gives it neither Emit nor
        // Consume and it lingers as dead code. Replacing the consumer detached the
        // constant's operand use, and the folded immediate is an `Int` attribute (not
        // a register use), so the maintained def-use chain now reports zero uses.
        for op_id in block_arc.op_ids() {
            let op = context.get_op(op_id);
            if op.name != "constant" {
                continue;
            }
            if op.results.iter().all(|v| !context.is_value_used(*v)) {
                let op_ref = OperationRef::new(op, Some(block_arc.clone()), None);
                rewriter.erase_op(&op_ref)?;
            }
        }

        Ok(())
    }

    fn solve_block(
        &self,
        context: &Context,
        block: &Block,
        cache: &BlockSelectionCache,
    ) -> Result<BlockPlan, String> {
        let mut op_refs = HashMap::new();
        for (position, op_id) in block.op_ids().into_iter().enumerate() {
            let op = context.get_op(op_id);
            op_refs.insert(
                op_id,
                OperationRef::new(op, Some(context.get_block(block.id())), Some(position)),
            );
        }

        let matches = self.collect_block_matches(context, cache, &op_refs);

        if let Some(message) = completeness_error(&cache.egraph, &cache.op_by_root, &matches) {
            return Err(message);
        }
        if matches.is_empty() {
            return Ok(BlockPlan::default());
        }

        let cost_model = self.target_model.cost_model();
        let Some(cover) = build_eclass_cover(
            &cache.egraph,
            &cache.op_by_root,
            &self.compiled_patterns,
            &matches,
            |parent, child, parent_alt| {
                materialization_edge_cost(
                    &self.compiled_patterns,
                    &cache.egraph,
                    parent,
                    child,
                    parent_alt,
                    &matches,
                    cost_model,
                )
            },
        ) else {
            return Ok(BlockPlan::default());
        };

        // The match chosen as Root for each e-class, and the classes consumed as an
        // interior node of some selected match.
        let mut root_match: HashMap<EClassId, usize> = HashMap::new();
        let mut internal_classes: HashSet<EClassId> = HashSet::new();
        for (node, choice) in cover.choices.iter().enumerate() {
            match choice {
                PbqpIselAlternative::Root { match_id } => {
                    root_match.insert(cover.classes[node], *match_id);
                }
                PbqpIselAlternative::Internal { .. } => {
                    internal_classes.insert(cover.classes[node]);
                }
                PbqpIselAlternative::External => {}
            }
        }

        let mut emit = EmissionBuilder {
            egraph: &cache.egraph,
            class_value: &cache.class_value,
            op_by_root: &cache.op_by_root,
            matches: &matches,
            root_match: &root_match,
            context,
            introduced_dest: HashMap::new(),
            introduced: Vec::new(),
        };

        // Inverse of op_by_root for this block's ops.
        let class_of_op: HashMap<OpId, EClassId> = cache
            .op_by_root
            .iter()
            .map(|(class, op)| (*op, *class))
            .collect();

        let mut op_decisions = HashMap::new();
        for op_id in block.op_ids() {
            let Some(class) = class_of_op.get(&op_id).map(|c| cache.egraph.find(*c)) else {
                continue;
            };
            if let Some(&match_id) = root_match.get(&class) {
                let result_ty = context
                    .get_op(op_id)
                    .results
                    .first()
                    .map(|v| context.get_value(*v).ty());
                let m = emit.resolve_match(match_id, op_id, result_ty);
                op_decisions.insert(
                    op_id,
                    BlockDecision::Emit {
                        rule_index: matches[match_id].rule_index,
                        m,
                    },
                );
            } else if internal_classes.contains(&class) {
                op_decisions.insert(op_id, BlockDecision::Consume);
            }
        }

        Ok(BlockPlan {
            op_decisions,
            introduced: emit.introduced,
        })
    }

    fn collect_block_matches(
        &self,
        context: &Context,
        cache: &BlockSelectionCache,
        op_refs: &HashMap<OpId, OperationRef>,
    ) -> Vec<PbqpIselMatch> {
        let mut matches = Vec::new();
        for (pattern_index, compiled) in self.compiled_patterns.iter().enumerate() {
            let rule = &self.rules[compiled.rule_index];
            if !self.target_model.supports_rule(rule.compiled_rule_id) {
                continue;
            }
            let Some(pattern_root) = compiled.pattern.root() else {
                continue;
            };
            let pattern = &compiled.pattern;

            // A class shared by several consumers must be materialized into a
            // register: it may be a match *root* (the instruction that produces it)
            // or a boundary operand, but never an *interior* node that some larger
            // match would consume and erase.
            let allowed = |pattern_node: NodeId, class: EClassId| {
                pattern_node == pattern_root
                    || pattern.is_duplicable(pattern_node)
                    || !cache.shared_classes.contains(&cache.egraph.find(class))
            };

            for m in cache
                .egraph
                .ematch_with_legality(context, pattern, &allowed)
            {
                let root = cache.egraph.find(m.root());
                let op_id = cache.op_by_root.get(&root).copied();
                // Instructions root at computed values: an original op result, or a
                // rewrite-introduced intermediate (which has no op). Matches rooted at
                // a pure operand (leaf/constant) are not instruction candidates.
                let is_computed = cache
                    .egraph
                    .nodes(root)
                    .iter()
                    .any(|n| !n.children.is_empty());
                if op_id.is_none() && !is_computed {
                    continue;
                }

                let mut captures = CaptureBindings::new();
                for (pattern_node, symbol) in &compiled.boundary_symbols {
                    captures.bind(*symbol, cache.egraph.find(m.binding(*pattern_node)));
                }

                let pattern_nodes = (0..pattern.len())
                    .map(NodeId::from_index)
                    .map(|pattern_node| PatternNodeBinding {
                        pattern_node,
                        class: cache.egraph.find(m.binding(pattern_node)),
                        is_boundary: matches!(
                            pattern.get_node(pattern_node),
                            PatternExpr::Boundary
                        ),
                    })
                    .collect();
                let bindings = FullMatchBindings {
                    captures,
                    pattern_nodes,
                };

                // Cost and legality are op-relative when there is a backing op;
                // a rewrite-introduced root has no op, so it takes the rule's
                // target-independent base cost and skips op legality.
                let rule_match = bindings
                    .captures
                    .to_rule_match(&cache.egraph, &cache.class_value);
                let cost = if let Some(op_ref) = op_id.and_then(|id| op_refs.get(&id)) {
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
                    self.target_model.cost_model().node_cost(
                        context,
                        op_ref,
                        rule,
                        &rule_match,
                        &pressure,
                        self.target_model.as_ref(),
                    )
                } else {
                    rule.base_cost as u64
                };

                matches.push(PbqpIselMatch {
                    pattern_index,
                    rule_index: compiled.rule_index,
                    root,
                    pattern_root,
                    bindings,
                    cost: specificity_adjusted_cost(cost, compiled.specificity),
                });
            }
        }
        matches
    }
}

/// Coverage completeness: every op-root e-class must be emittable as an instruction
/// (it roots some match) or consumable by a parent match (it is an interior node of
/// some match). A non-terminal op-root that is neither cannot be selected by this
/// rule set — even after saturation — so selection fails with a diagnostic.
fn completeness_error(
    egraph: &SemEGraph,
    op_by_root: &HashMap<EClassId, OpId>,
    matches: &[PbqpIselMatch],
) -> Option<String> {
    let mut has_root: HashSet<EClassId> = HashSet::new();
    let mut has_internal: HashSet<EClassId> = HashSet::new();
    for m in matches {
        has_root.insert(egraph.find(m.root));
        for binding in &m.bindings.pattern_nodes {
            if !binding.is_boundary && binding.pattern_node != m.pattern_root {
                has_internal.insert(egraph.find(binding.class));
            }
        }
    }

    let mut missing: Vec<ExprKind> = Vec::new();
    for &class in op_by_root.keys() {
        let class = egraph.find(class);
        if egraph.nodes(class).iter().any(|n| n.children.is_empty()) {
            continue;
        }
        if has_root.contains(&class) || has_internal.contains(&class) {
            continue;
        }
        if let Some(kind) = egraph.nodes(class).first().map(|n| n.node.kind) {
            if !missing.contains(&kind) {
                missing.push(kind);
            }
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

/// Turns a solved cover into concrete per-instruction `RuleMatch`es, materializing
/// rewrite-introduced e-classes (those covered by a Root match but with no original
/// IR op) as fresh-valued instructions threaded into their consumers' operands.
struct EmissionBuilder<'a> {
    egraph: &'a SemEGraph,
    class_value: &'a HashMap<EClassId, ValueId>,
    op_by_root: &'a HashMap<EClassId, OpId>,
    matches: &'a [PbqpIselMatch],
    root_match: &'a HashMap<EClassId, usize>,
    context: &'a Context,
    /// Fresh destination value assigned to each introduced class.
    introduced_dest: HashMap<EClassId, ValueId>,
    introduced: Vec<IntroducedEmit>,
}

impl EmissionBuilder<'_> {
    /// A Root-covered class with no original op is one the rewrites introduced.
    fn is_introduced(&self, class: EClassId) -> bool {
        self.root_match.contains_key(&class) && !self.op_by_root.contains_key(&class)
    }

    /// Build the operand bindings for a match, first materializing any introduced
    /// operand instructions (anchored before `anchor`).
    fn resolve_match(
        &mut self,
        match_id: usize,
        anchor: OpId,
        anchor_ty: Option<TypeId>,
    ) -> RuleMatch {
        let operand_classes: Vec<EClassId> = self.matches[match_id]
            .bindings
            .captures
            .entries
            .iter()
            .map(|(_, class)| self.egraph.find(*class))
            .collect();
        for class in operand_classes {
            if self.is_introduced(class) {
                self.emit_introduced(class, anchor, anchor_ty);
            }
        }
        self.build_rule_match(match_id)
    }

    /// Ensure an introduced class is emitted (operands first), returning its fresh
    /// destination value.
    fn emit_introduced(
        &mut self,
        class: EClassId,
        anchor: OpId,
        anchor_ty: Option<TypeId>,
    ) -> ValueId {
        if let Some(&dest) = self.introduced_dest.get(&class) {
            return dest;
        }
        let match_id = self.root_match[&class];
        let operand_classes: Vec<EClassId> = self.matches[match_id]
            .bindings
            .captures
            .entries
            .iter()
            .map(|(_, c)| self.egraph.find(*c))
            .collect();
        for c in operand_classes {
            if self.is_introduced(c) {
                self.emit_introduced(c, anchor, anchor_ty);
            }
        }

        let dest_ty = anchor_ty
            .or_else(|| {
                class_width(self.context, self.egraph, class)
                    .map(|w| IntegerType::new(self.context, w))
            })
            .unwrap_or_else(|| IntegerType::new(self.context, 64));
        let dest = self.context.create_value(dest_ty, None).id();
        self.introduced_dest.insert(class, dest);

        let m = self.build_rule_match(match_id);
        self.introduced.push(IntroducedEmit {
            rule_index: self.matches[match_id].rule_index,
            m,
            dest,
            dest_ty,
            anchor,
        });
        dest
    }

    /// Resolve each capture symbol to a concrete operand: an introduced operand's
    /// fresh value, then a constant immediate, then an input value, then the value
    /// an intermediate result produces.
    fn build_rule_match(&self, match_id: usize) -> RuleMatch {
        let mut int_bindings = Vec::new();
        let mut value_bindings = Vec::new();
        for (sym, class) in &self.matches[match_id].bindings.captures.entries {
            let class = self.egraph.find(*class);
            // An introduced operand's fresh value takes priority; otherwise resolve
            // the class to its constant/input/intermediate operand as usual.
            if let Some(&dest) = self.introduced_dest.get(&class) {
                value_bindings.push((*sym, dest));
                continue;
            }
            match class_binding(self.egraph, self.class_value, class) {
                Some(Binding::Int(v)) => int_bindings.push((*sym, v)),
                Some(Binding::Value(v)) => value_bindings.push((*sym, v)),
                None => {}
            }
        }
        RuleMatch::new(int_bindings, value_bindings)
    }
}

/// A throwaway op-ref carrying only `dest` as its result, so an introduced
/// instruction's emitter (which reads the destination from the op's result) emits
/// into a fresh register without a backing IR op.
fn synthetic_op_ref(
    context: &Context,
    block: &std::sync::Arc<Block>,
    dest: ValueId,
    _dest_ty: TypeId,
) -> OperationRef {
    let instance = std::sync::Arc::new(OpInstance {
        id: OpId::invalid(),
        name: "isel.introduced",
        dialect: "isel",
        context: context.as_context_ref(),
        operands: Vec::new(),
        results: vec![dest],
        regions: Vec::new(),
        attributes: Vec::new(),
        attribute_roles: &[],
    });
    OperationRef::new(instance, Some(block.clone()), None)
}

fn alternatives_compatible(
    patterns: &[CompiledIselPattern],
    parent: EClassId,
    child: EClassId,
    parent_alt: &PbqpIselAlternative,
    child_alt: &PbqpIselAlternative,
    matches: &[PbqpIselMatch],
) -> bool {
    if let Some(requirement) = child_requirement(patterns, child, parent_alt, matches) {
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
    child: EClassId,
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
        if m.bindings.class_for_pattern(pattern_child) != Some(child) {
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
    egraph: &SemEGraph,
    parent: EClassId,
    child: EClassId,
    parent_alt: &PbqpIselAlternative,
    matches: &[PbqpIselMatch],
    cost_model: &dyn IselCostModel,
) -> u64 {
    let materialized = matches!(
        child_requirement(patterns, child, parent_alt, matches),
        Some(ChildRequirement::Materialized)
    );
    if !materialized {
        return 0;
    }
    let (Some(parent_kind), Some(child_kind)) = (
        egraph.nodes(parent).first().map(|n| n.node.kind),
        egraph.nodes(child).first().map(|n| n.node.kind),
    ) else {
        return 0;
    };
    cost_model.edge_cost(parent_kind, child_kind, true)
}

fn parent_satisfies_internal_child(
    patterns: &[CompiledIselPattern],
    parent: EClassId,
    child: EClassId,
    parent_alt: &PbqpIselAlternative,
    child_match_id: usize,
    child_pattern_node: NodeId,
    matches: &[PbqpIselMatch],
) -> bool {
    let m = &matches[child_match_id];
    let pattern = &patterns[m.pattern_index].pattern;
    for pattern_parent in (0..pattern.len()).map(NodeId::from_index) {
        if !pattern
            .children(pattern_parent)
            .contains(&child_pattern_node)
        {
            continue;
        }
        if m.bindings.class_for_pattern(pattern_parent) != Some(parent) {
            continue;
        }
        if m.bindings.class_for_pattern(child_pattern_node) != Some(child) {
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
        graph::{MutDag, OperandConstraint},
        sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
    };

    use super::{
        EmitPlan, InstructionSelectPass, IselCostModel, Rule, RuleMatch, SelectionPressure,
        SemEGraph, SemNode, TargetIselModel, extension_rewrite, template_node,
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

        // A standalone Mul that no rule can root and no parent match can consume:
        // the e-graph cover is infeasible, so selection fails naming the kind.
        let _ = z_id;
        let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
        let mul_result = mul.result();
        fb.insert(mul);
        fb.insert(ops::r#return(&context, mul_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![Rule::new(
            "add",
            atomic_pattern(ExprKind::Add),
            10,
            emit_add,
        )];

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
        pm.nest(FuncOp::name())
            .add_pass(
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
    fn composite_rule_falls_back_to_atomic_cover() {
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
            Rule::new(
                "add.i32",
                typed_binary_pattern(ExprKind::Add, i32_ty),
                1,
                emit_sub,
            ),
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

    /// The square problem: a sub-word sign extension has no single RISC-V base
    /// instruction. Equality saturation with the discovered `SExt -> slli/srai`
    /// bridge must make the `SExt(v@i16, 64)` class selectable as an arithmetic
    /// shift by `W - n = 48`, exactly the `srai` of the `add, slli, srai` idiom.
    #[test]
    fn saturation_bridges_sign_extension_to_shift_pair() {
        use tir::graph::{ENode, OperandConstraint, Pattern, PatternExpr, SaturationLimits};
        use tir::sem_expr::{ExprKind, ExprPayload};
        use tir::utils::APInt;

        let ctx = Context::with_default_dialects();
        let i16 = IntegerType::new(&ctx, 16);
        let i64 = IntegerType::new(&ctx, 64);

        // SExt(v @ i16, 64), typed i64 — the program graph node no RV64 base
        // instruction can root.
        let mut egraph = SemEGraph::new();
        let v = egraph.add(ENode::leaf(
            template_node(ExprKind::Symbol, Some(ExprPayload::SymbolId(0)), Some(i16)),
            None,
        ));
        let width = egraph.add(ENode::leaf(
            template_node(
                ExprKind::Constant,
                Some(ExprPayload::Int(APInt::new(64, 64))),
                None,
            ),
            None,
        ));
        let sext = egraph.add(ENode::op(
            template_node(ExprKind::SExt, None, Some(i64)),
            vec![v, width],
        ));

        let rewrite = extension_rewrite(ExprKind::SExt, ExprKind::ShiftRightArithmetic);
        egraph.saturate(
            &ctx,
            std::slice::from_ref(&rewrite),
            SaturationLimits::default(),
        );

        // The sext class now also contains the shift-pair realization.
        assert!(
            egraph
                .nodes(sext)
                .iter()
                .any(|n| n.node.kind == ExprKind::ShiftRightArithmetic),
            "saturation should add the arithmetic-shift bridge to the sext class"
        );

        // An immediate `srai` pattern matches the class, with shift amount 64-16=48.
        let mut srai = Pattern::<SemNode, ()>::new(());
        let rs1 = srai.add_node(PatternExpr::Boundary);
        srai.set_duplicable(rs1, true);
        let imm = srai.add_node(PatternExpr::Boundary);
        srai.set_duplicable(imm, true);
        srai.set_operand_constraint(imm, OperandConstraint::Immediate);
        let root = srai.add_node(PatternExpr::Node(template_node(
            ExprKind::ShiftRightArithmetic,
            None,
            None,
        )));
        srai.add_edge(root, rs1);
        srai.add_edge(root, imm);
        srai.set_root(root);

        let matches = egraph.ematch(&ctx, &srai);
        let m = matches
            .iter()
            .find(|m| egraph.find(m.root()) == egraph.find(sext))
            .expect("an immediate srai must match the sext class after saturation");
        let shift_amount = egraph
            .nodes(m.binding(imm))
            .iter()
            .find_map(|n| match n.node.payload.as_ref() {
                Some(ExprPayload::Int(v)) => Some(v.to_u64()),
                _ => None,
            })
            .expect("the srai shift amount must be a constant");
        assert_eq!(shift_amount, 48);
    }

    fn shift_imm_pattern(kind: ExprKind) -> ExprPostGraph {
        let mut g = ExprPostGraph::new();
        let rs1 = symbol(&mut g, 0);
        let imm = symbol(&mut g, 1);
        binary(&mut g, kind, rs1, imm);
        g
    }

    fn emit_shift_marker(
        marker: ExprKind,
    ) -> impl Fn(&Context, &tir::OperationRef, &RuleMatch) -> Result<EmitPlan, tir::PassError> {
        move |context, op, m| {
            let rs1 = m
                .value_binding(0)
                .ok_or(tir::PassError::RewriteFailed(op.op().id))?;
            let result_ty = context.get_value(op.op().results[0]).ty();
            // The shift amount is an immediate (m.int_binding(1)); operands beyond the
            // mnemonic don't matter for this test, so the source register is reused.
            let built: Box<dyn Operation> = match marker {
                ExprKind::ShiftLeft => Box::new(ops::shli(context, rs1, rs1, result_ty).build()),
                _ => Box::new(ops::shrsi(context, rs1, rs1, result_ty).build()),
            };
            Ok(EmitPlan::single(built))
        }
    }

    fn emit_slli(
        context: &Context,
        op: &tir::OperationRef,
        m: &RuleMatch,
    ) -> Result<EmitPlan, tir::PassError> {
        emit_shift_marker(ExprKind::ShiftLeft)(context, op, m)
    }

    fn emit_srai(
        context: &Context,
        op: &tir::OperationRef,
        m: &RuleMatch,
    ) -> Result<EmitPlan, tir::PassError> {
        emit_shift_marker(ExprKind::ShiftRightArithmetic)(context, op, m)
    }

    /// End-to-end square: `extsi(addi(a, b) : i16) : i64` lowers to `add, slli, srai`.
    /// The `add` covers the addi; saturation bridges the un-selectable sign extension
    /// into a `slli`/`srai` pair, and multi-instruction emission materializes the
    /// introduced `slli` (an e-class with no original op) before the `srai`.
    #[test]
    fn square_sign_extension_lowers_to_shift_pair() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let i16_ty = IntegerType::new(&context, 16);
        let i64_ty = IntegerType::new(&context, 64);
        let a = context.create_value(i16_ty, None);
        let b = context.create_value(i16_ty, None);
        let (a_id, b_id) = (a.id(), b.id());
        let region = context.create_region();
        let block = context.create_block(vec![a, b]);
        region.add_block(block.id());

        let func = ops::func(&context, "square", i64_ty, Some(region.id())).build();
        let mut fb = IRBuilder::new(func.body());
        let add = ops::addi(&context, a_id, b_id, i16_ty).build();
        let add_result = add.result();
        fb.insert(add);
        let ext = ops::extsi(&context, add_result, i64_ty).build();
        let ext_result = ext.result();
        fb.insert(ext);
        fb.insert(ops::r#return(&context, ext_result).build());

        let mut mb = IRBuilder::new(module.body());
        mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        let rules = vec![
            Rule::new("add", atomic_pattern(ExprKind::Add), 1, emit_add),
            Rule::new("slli", shift_imm_pattern(ExprKind::ShiftLeft), 1, emit_slli)
                .with_operand_constraints(vec![(1, OperandConstraint::Immediate)]),
            Rule::new(
                "srai",
                shift_imm_pattern(ExprKind::ShiftRightArithmetic),
                1,
                emit_srai,
            )
            .with_operand_constraints(vec![(1, OperandConstraint::Immediate)]),
        ];

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name())
            .add_pass(InstructionSelectPass::new(rules));
        pm.run(&context, context.get_op(module.id()))
            .expect("square should select");

        let body_ops: Vec<_> = context
            .get_region(region.id())
            .iter(context.clone())
            .next()
            .unwrap()
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();
        // add (from the addi), then the slli/srai sign-extension idiom, then return.
        assert_eq!(body_ops, vec!["addi", "shli", "shrsi", "return"]);
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
