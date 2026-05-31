use std::collections::{HashMap, HashSet};

use tir::{
    Block, BlockId, Context, OpId, OpInstance, Operation, OperationRef, Pass, PassError,
    PassTarget, Rewriter, ValueId,
    attributes::AttributeValue,
    graph::{CoverLegality, Dag, Node, NodeId, Pattern, PatternExpr, VF2CoverDriver},
    pbqp::{self, INF_COST, PbqpAlternative, PbqpMatrix, PbqpProblem},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
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
}

impl PartialEq for SemNode {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.payload == other.payload
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

    fn matches_pattern(&self, pattern: &Self, _ctx: &Context) -> bool {
        if self.kind != pattern.kind {
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
}

pub struct DefaultTargetIselModel;

impl TargetIselModel for DefaultTargetIselModel {}

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
            legality_fn: default_legality,
            dynamic_cost_fn: default_dynamic_cost,
            emit_plan_fn,
        }
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
            match dag.node(*node_id).payload.as_ref() {
                Some(ExprPayload::Int(v)) => int_bindings.push((*sym, v.clone())),
                Some(ExprPayload::Value(v)) => value_bindings.push((*sym, *v)),
                _ => {}
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
    },
    Input(u32),
    UnknownSymbol(u32),
    Op {
        kind: ExprKind,
        inputs: Vec<SemNodeId>,
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

    fn intern_int(&mut self, value: APInt) -> SemNodeId {
        self.intern_with_key(
            SemKey::Int {
                width: value.width(),
                signed: value.is_signed(),
                value: value.to_u64(),
            },
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Constant,
                inputs: Vec::new(),
                payload: Some(ExprPayload::Int(value)),
            },
        )
    }

    fn intern_input_value(&mut self, value: ValueId) -> SemNodeId {
        self.intern_with_key(
            SemKey::Input(value.number()),
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Symbol,
                inputs: Vec::new(),
                payload: Some(ExprPayload::Value(value)),
            },
        )
    }

    fn intern_unknown_symbol(&mut self, symbol: u32) -> SemNodeId {
        self.intern_with_key(
            SemKey::UnknownSymbol(symbol),
            SemNode {
                id: SemNodeId(0),
                kind: ExprKind::Symbol,
                inputs: Vec::new(),
                payload: Some(ExprPayload::SymbolId(symbol)),
            },
        )
    }

    fn intern_op(&mut self, kind: ExprKind, mut inputs: Vec<SemNodeId>) -> SemNodeId {
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
            },
            SemNode {
                id: SemNodeId(0),
                kind,
                inputs,
                payload: None,
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
            },
        )
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

    fn get_actual_type(&self, _id: NodeId) -> Option<tir::TypeId> {
        None
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
        Some(self.lower_graph_node(&graph, root, &operands))
    }

    fn build_from_value(&mut self, value: ValueId) -> SemNodeId {
        if let Some(existing) = self.value_to_node.get(&value) {
            return *existing;
        }

        let node = if let Some(def_op_id) = self.value_to_def.get(&value) {
            let def = self.context.get_op(*def_op_id);
            if def.name == "constant" {
                if let Some(attr) = def.attributes.iter().find(|a| a.name == "value") {
                    if let AttributeValue::Int(v) = &attr.value {
                        self.arena.intern_int(APInt::new_signed(64, *v))
                    } else {
                        self.arena.intern_input_value(value)
                    }
                } else {
                    self.arena.intern_input_value(value)
                }
            } else {
                let mut graph = ExprPostGraph::new();
                if let Some(root) = def.clone().as_dyn_op().semantic_expr(&mut graph) {
                    let mut operands = Vec::with_capacity(def.operands.len());
                    for operand in &def.operands {
                        operands.push(self.build_from_value(*operand));
                    }
                    self.lower_graph_node(&graph, root, &operands)
                } else {
                    self.arena.intern_input_value(value)
                }
            }
        } else {
            self.arena.intern_input_value(value)
        };

        self.value_to_node.insert(value, node);
        node
    }

    fn lower_graph_node(
        &mut self,
        graph: &ExprPostGraph,
        node: NodeId,
        operands: &[SemNodeId],
    ) -> SemNodeId {
        match graph.get_node(node) {
            ExprKind::Symbol => match graph.get_leaf_data(node) {
                Some(ExprPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .copied()
                    .unwrap_or_else(|| self.arena.intern_unknown_symbol(*id)),
                _ => self.arena.intern_opaque(),
            },
            ExprKind::Constant => match graph.get_leaf_data(node) {
                Some(ExprPayload::Int(v)) => self.arena.intern_int(v.clone()),
                _ => self.arena.intern_opaque(),
            },
            kind => {
                let children: Vec<SemNodeId> = graph
                    .children(node)
                    .map(|child| self.lower_graph_node(graph, child, operands))
                    .collect();
                if kind.num_children(self.context) == children.len() {
                    self.arena.intern_op(*kind, children)
                } else {
                    self.arena.intern_opaque()
                }
            }
        }
    }
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
}

fn compile_isel_pattern(rule_index: usize, expr: &ExprPostGraph) -> Option<CompiledIselPattern> {
    let root = expr.root()?;
    let mut pattern = Pattern::new(rule_index);
    let mut boundary_symbols = HashMap::new();
    let mut memo = HashMap::new();
    let pattern_root =
        compile_isel_pattern_node(expr, root, &mut pattern, &mut boundary_symbols, &mut memo)?;
    pattern.set_root(pattern_root);

    Some(CompiledIselPattern {
        rule_index,
        pattern,
        boundary_symbols,
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
            ))),
            _ => return None,
        },
        kind => {
            let compiled = pattern.add_node(PatternExpr::Node(template_node(*kind, None)));
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

fn template_node(kind: ExprKind, payload: Option<ExprPayload>) -> SemNode {
    SemNode {
        id: SemNodeId(0),
        kind,
        inputs: Vec::new(),
        payload,
    }
}

fn validate_block_rule_set(
    patterns: &[CompiledIselPattern],
    cache: &BlockSelectionCache,
) -> Vec<String> {
    let atomics = atomic_materializers(patterns);
    let mut errors = Vec::new();

    let mut required = Vec::new();
    for root in cache.op_by_root.keys().copied() {
        let sem_node = cache.dag.node(root);
        if sem_node.is_terminal() {
            continue;
        }
        if !required.contains(&sem_node.kind) {
            required.push(sem_node.kind);
        }
    }
    required.sort();

    for kind in required {
        if !atomics.contains(&kind) {
            errors.push(format!(
                "missing atomic materializer rule for semantic kind {kind:?}"
            ));
        }
    }

    errors
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

pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    compiled_patterns: Vec<CompiledIselPattern>,
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
            .filter_map(|(rule_index, rule)| compile_isel_pattern(rule_index, &rule.pattern))
            .collect();
        Self {
            rules,
            compiled_patterns,
            target_model: Box::new(DefaultTargetIselModel),
            op_lowerings: vec![],
            block_cache: HashMap::new(),
            emitted_blocks: HashSet::new(),
        }
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
        if let Some(cache) = self.block_cache.get(&block.id()) {
            let validation_errors = validate_block_rule_set(&self.compiled_patterns, cache);
            if !validation_errors.is_empty() {
                return Err(PassError::InvalidRuleSet(validation_errors.join("; ")));
            }
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

        let mut alternatives_by_node = vec![Vec::<PbqpIselAlternative>::new(); cache.dag.len()];
        for (node, sem_node) in cache.dag.iter() {
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
                alternatives_by_node[binding.sem_node.index()].push(
                    PbqpIselAlternative::Internal {
                        match_id,
                        pattern_node: binding.pattern_node,
                    },
                );
            }
        }

        for (node, alternatives) in alternatives_by_node.iter_mut().enumerate() {
            if alternatives.is_empty() {
                let sem_node = cache.dag.node(SemNodeId(node as u32));
                if sem_node.is_terminal() || !cache.op_by_root.contains_key(&SemNodeId(node as u32))
                {
                    alternatives.push(PbqpIselAlternative::External);
                }
            }
        }

        if alternatives_by_node.iter().any(Vec::is_empty) {
            return HashMap::new();
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

        for (parent_id, parent) in cache.dag.iter() {
            for &child_id in &parent.inputs {
                let parent_alts = &alternatives_by_node[parent_id.index()];
                let child_alts = &alternatives_by_node[child_id.index()];
                let mut matrix = PbqpMatrix::zero(parent_alts.len(), child_alts.len());

                for (parent_alt_idx, parent_alt) in parent_alts.iter().enumerate() {
                    for (child_alt_idx, child_alt) in child_alts.iter().enumerate() {
                        if !self.alternatives_compatible(
                            parent_id, child_id, parent_alt, child_alt, &matches,
                        ) {
                            matrix.set(parent_alt_idx, child_alt_idx, INF_COST);
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

        let Ok(solution) = pbqp::solve(&problem) else {
            return HashMap::new();
        };

        let mut decisions = HashMap::new();
        for (node, choice) in solution.choices.iter().copied().enumerate() {
            let node_id = SemNodeId(node as u32);
            let Some(op_id) = cache.op_by_root.get(&node_id).copied() else {
                continue;
            };
            match &alternatives_by_node[node][choice] {
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
            let rule = &self.rules[compiled.rule_index];
            if !self.target_model.supports_rule(rule.compiled_rule_id) {
                continue;
            }

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
                if !(rule.legality_fn)(context, op_ref, &rule_match, self.target_model.as_ref()) {
                    continue;
                }

                let pressure = SelectionPressure {
                    estimated_live_operands: op_ref.op().operands.len() as u32,
                    estimated_register_pressure: self
                        .target_model
                        .estimate_register_pressure(op_ref),
                };
                let cost = rule.base_cost as u64
                    + (rule.dynamic_cost_fn)(
                        context,
                        op_ref,
                        &rule_match,
                        &pressure,
                        self.target_model.as_ref(),
                    ) as u64;

                matches.push(PbqpIselMatch {
                    pattern_index,
                    rule_index: compiled.rule_index,
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

    fn alternatives_compatible(
        &self,
        parent: SemNodeId,
        child: SemNodeId,
        parent_alt: &PbqpIselAlternative,
        child_alt: &PbqpIselAlternative,
        matches: &[PbqpIselMatch],
    ) -> bool {
        if let Some(requirement) = self.child_requirement(parent, child, parent_alt, matches) {
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
            return self.parent_satisfies_internal_child(
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
        &self,
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
        let pattern = &self.compiled_patterns[m.pattern_index].pattern;
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

    fn parent_satisfies_internal_child(
        &self,
        parent: SemNodeId,
        child: SemNodeId,
        parent_alt: &PbqpIselAlternative,
        child_match_id: usize,
        child_pattern_node: NodeId,
        matches: &[PbqpIselMatch],
    ) -> bool {
        let m = &matches[child_match_id];
        let pattern = &self.compiled_patterns[m.pattern_index].pattern;
        for pattern_parent in (0..pattern.len()).map(NodeId::from_index) {
            if !pattern
                .children(pattern_parent)
                .iter()
                .any(|&pattern_child| pattern_child == child_pattern_node)
            {
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
        Context, IRBuilder, IRFormatter, Operation, PassManager,
        builtin::{FuncOp, IntegerType, ops},
        graph::MutDag,
        sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
    };

    use super::{EmitPlan, InstructionSelectPass, Rule, RuleMatch};

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
