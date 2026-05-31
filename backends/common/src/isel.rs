use std::collections::HashMap;

use tir::{
    Block, BlockId, Context, OpId, OpInstance, Operation, OperationRef, Pass, PassError,
    PassTarget, Rewriter, ValueId,
    attributes::AttributeValue,
    graph::{Dag, NodeId},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
    utils::APInt,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SemNodeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SemOpcode {
    IntConst,
    BoolConst,
    InputValue,
    UnknownSymbol,
    Add,
    Sub,
    Mul,
    Div,
    ShiftLeft,
    ShiftRightLogic,
    ShiftRightArithmetic,
    And,
    Or,
    Xor,
    Opaque,
}

#[derive(Clone, Debug)]
pub struct SemNode {
    pub id: SemNodeId,
    pub opcode: SemOpcode,
    pub inputs: Vec<SemNodeId>,
    pub int_value: Option<APInt>,
    pub bool_value: Option<bool>,
    pub leaf_value: Option<ValueId>,
    pub unknown_symbol: Option<u32>,
}

impl SemNode {
    fn is_terminal(&self) -> bool {
        self.leaf_value.is_some() || self.int_value.is_some() || self.bool_value.is_some()
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

pub struct Selection {
    pub rule_index: usize,
    pub m: RuleMatch,
    pub total_cost: u64,
}

pub struct SelectionInput<'a> {
    pub dag: &'a SemDagArena,
    pub root: SemNodeId,
    pub op: &'a OperationRef,
    pub rules: &'a [Rule],
    pub matcher: &'a MatcherAutomaton,
    pub pressure: SelectionPressure,
    pub target_model: &'a dyn TargetIselModel,
    pub context: &'a Context,
}

pub struct SelectionResult {
    pub selection: Option<Selection>,
}

pub trait IselAlgorithm: Send + Sync {
    fn select_function(&self, input: SelectionInput<'_>) -> SelectionResult;
}

pub struct GlobalDpSelector;

#[derive(Clone, Debug)]
struct CandidateScore {
    rule_index: usize,
    captures: CaptureBindings,
    total_cost: u64,
}

impl IselAlgorithm for GlobalDpSelector {
    fn select_function(&self, input: SelectionInput<'_>) -> SelectionResult {
        let mut memo: HashMap<SemNodeId, Option<u64>> = HashMap::new();
        let candidates = input.matcher.candidate_rules(input.dag.node(input.root));

        let mut best: Option<CandidateScore> = None;
        for rule_index in candidates {
            let rule = &input.rules[rule_index];
            if !input.target_model.supports_rule(rule.compiled_rule_id) {
                continue;
            }

            let mut captures = CaptureBindings::new();
            let Some(pattern_root) = rule.pattern.root() else {
                continue;
            };
            if !match_pattern(
                &rule.pattern,
                pattern_root,
                input.root,
                input.dag,
                &mut captures,
            ) {
                continue;
            }

            let rule_match = captures.to_rule_match(input.dag);
            if !(rule.legality_fn)(input.context, input.op, &rule_match, input.target_model) {
                continue;
            }

            let mut total = rule.base_cost as u64;
            total += (rule.dynamic_cost_fn)(
                input.context,
                input.op,
                &rule_match,
                &input.pressure,
                input.target_model,
            ) as u64;

            let boundaries = captures.boundary_nodes(input.root, input.dag);
            for boundary in boundaries {
                total +=
                    best_cost_for_node(boundary, input.dag, input.rules, input.matcher, &mut memo)
                        .unwrap_or(u64::MAX / 4);
            }

            let score = CandidateScore {
                rule_index,
                captures,
                total_cost: total,
            };

            match &best {
                Some(existing)
                    if existing.total_cost < score.total_cost
                        || (existing.total_cost == score.total_cost
                            && existing.rule_index <= score.rule_index) => {}
                _ => best = Some(score),
            }
        }

        let selection = best.map(|winner| Selection {
            rule_index: winner.rule_index,
            m: winner.captures.to_rule_match(input.dag),
            total_cost: winner.total_cost,
        });

        SelectionResult { selection }
    }
}

fn best_cost_for_node(
    node: SemNodeId,
    dag: &SemDagArena,
    rules: &[Rule],
    matcher: &MatcherAutomaton,
    memo: &mut HashMap<SemNodeId, Option<u64>>,
) -> Option<u64> {
    if let Some(cached) = memo.get(&node) {
        return *cached;
    }

    let sem_node = dag.node(node);
    if sem_node.is_terminal() {
        memo.insert(node, Some(0));
        return Some(0);
    }

    let mut best: Option<u64> = None;
    for rule_index in matcher.candidate_rules(sem_node) {
        let rule = &rules[rule_index];
        let mut captures = CaptureBindings::new();
        let Some(pattern_root) = rule.pattern.root() else {
            continue;
        };
        if !match_pattern(&rule.pattern, pattern_root, node, dag, &mut captures) {
            continue;
        }

        let mut total = rule.base_cost as u64;
        for boundary in captures.boundary_nodes(node, dag) {
            let Some(sub) = best_cost_for_node(boundary, dag, rules, matcher, memo) else {
                total = u64::MAX / 4;
                break;
            };
            total = total.saturating_add(sub);
        }

        best = Some(match best {
            Some(existing) if existing <= total => existing,
            _ => total,
        });
    }

    memo.insert(node, best);
    best
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

    fn mark(&self) -> usize {
        self.entries.len()
    }

    fn rollback(&mut self, mark: usize) {
        self.entries.truncate(mark);
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
            if let Some(v) = &dag.node(*node_id).int_value {
                int_bindings.push((*sym, v.clone()));
            }
            if let Some(v) = dag.node(*node_id).leaf_value {
                value_bindings.push((*sym, v));
            }
        }
        RuleMatch::new(int_bindings, value_bindings)
    }

    fn boundary_nodes(&self, root: SemNodeId, dag: &SemDagArena) -> Vec<SemNodeId> {
        let mut out = Vec::new();
        for (_, node) in &self.entries {
            if *node == root {
                continue;
            }
            let n = dag.node(*node);
            if n.is_terminal() {
                continue;
            }
            if !out.contains(node) {
                out.push(*node);
            }
        }
        out
    }
}

fn match_pattern(
    pattern: &ExprPostGraph,
    pattern_node: NodeId,
    candidate: SemNodeId,
    dag: &SemDagArena,
    captures: &mut CaptureBindings,
) -> bool {
    let node = dag.node(candidate);
    match pattern.get_node(pattern_node) {
        ExprKind::Symbol => {
            let Some(ExprPayload::SymbolId(id)) = pattern.get_leaf_data(pattern_node) else {
                return false;
            };
            captures.bind(*id, candidate)
        }
        ExprKind::Constant => match pattern.get_leaf_data(pattern_node) {
            Some(ExprPayload::Int(v)) => matches!(node.int_value.as_ref(), Some(cv) if cv == v),
            _ => false,
        },
        ExprKind::Add => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Add,
            true,
        ),
        ExprKind::Sub => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Sub,
            false,
        ),
        ExprKind::Mul => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Mul,
            true,
        ),
        ExprKind::Div => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Div,
            false,
        ),
        ExprKind::ShiftLeft => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::ShiftLeft,
            false,
        ),
        ExprKind::ShiftRightLogic => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::ShiftRightLogic,
            false,
        ),
        ExprKind::ShiftRightArithmetic => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::ShiftRightArithmetic,
            false,
        ),
        ExprKind::And => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::And,
            true,
        ),
        ExprKind::Or => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Or,
            true,
        ),
        ExprKind::Xor => match_binary(
            pattern,
            pattern_node,
            candidate,
            dag,
            captures,
            SemOpcode::Xor,
            true,
        ),
        _ => false,
    }
}

fn match_binary(
    pattern: &ExprPostGraph,
    pattern_node: NodeId,
    candidate: SemNodeId,
    dag: &SemDagArena,
    captures: &mut CaptureBindings,
    opcode: SemOpcode,
    commutative: bool,
) -> bool {
    let node = dag.node(candidate);
    if node.opcode != opcode || node.inputs.len() != 2 {
        return false;
    }
    let children: Vec<NodeId> = pattern.children(pattern_node).collect();
    if children.len() != 2 {
        return false;
    }

    let mark = captures.mark();
    if match_pattern(pattern, children[0], node.inputs[0], dag, captures)
        && match_pattern(pattern, children[1], node.inputs[1], dag, captures)
    {
        return true;
    }
    captures.rollback(mark);

    if commutative {
        let mark = captures.mark();
        if match_pattern(pattern, children[0], node.inputs[1], dag, captures)
            && match_pattern(pattern, children[1], node.inputs[0], dag, captures)
        {
            return true;
        }
        captures.rollback(mark);
    }

    false
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
    Bin {
        opcode: SemOpcode,
        lhs: SemNodeId,
        rhs: SemNodeId,
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
                opcode: SemOpcode::IntConst,
                inputs: Vec::new(),
                int_value: Some(value),
                bool_value: None,
                leaf_value: None,
                unknown_symbol: None,
            },
        )
    }

    fn intern_input_value(&mut self, value: ValueId) -> SemNodeId {
        self.intern_with_key(
            SemKey::Input(value.number()),
            SemNode {
                id: SemNodeId(0),
                opcode: SemOpcode::InputValue,
                inputs: Vec::new(),
                int_value: None,
                bool_value: None,
                leaf_value: Some(value),
                unknown_symbol: None,
            },
        )
    }

    fn intern_unknown_symbol(&mut self, symbol: u32) -> SemNodeId {
        self.intern_with_key(
            SemKey::UnknownSymbol(symbol),
            SemNode {
                id: SemNodeId(0),
                opcode: SemOpcode::UnknownSymbol,
                inputs: Vec::new(),
                int_value: None,
                bool_value: None,
                leaf_value: None,
                unknown_symbol: Some(symbol),
            },
        )
    }

    fn intern_binary(&mut self, opcode: SemOpcode, lhs: SemNodeId, rhs: SemNodeId) -> SemNodeId {
        let (lhs, rhs) = if matches!(
            opcode,
            SemOpcode::Add | SemOpcode::Mul | SemOpcode::And | SemOpcode::Or | SemOpcode::Xor
        ) && rhs < lhs
        {
            (rhs, lhs)
        } else {
            (lhs, rhs)
        };

        self.intern_with_key(
            SemKey::Bin { opcode, lhs, rhs },
            SemNode {
                id: SemNodeId(0),
                opcode,
                inputs: vec![lhs, rhs],
                int_value: None,
                bool_value: None,
                leaf_value: None,
                unknown_symbol: None,
            },
        )
    }

    fn intern_opaque(&mut self) -> SemNodeId {
        self.intern_with_key(
            SemKey::Opaque,
            SemNode {
                id: SemNodeId(0),
                opcode: SemOpcode::Opaque,
                inputs: Vec::new(),
                int_value: None,
                bool_value: None,
                leaf_value: None,
                unknown_symbol: None,
            },
        )
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
                let opcode = match kind {
                    ExprKind::Add => Some(SemOpcode::Add),
                    ExprKind::Sub => Some(SemOpcode::Sub),
                    ExprKind::Mul => Some(SemOpcode::Mul),
                    ExprKind::Div => Some(SemOpcode::Div),
                    ExprKind::ShiftLeft => Some(SemOpcode::ShiftLeft),
                    ExprKind::ShiftRightLogic => Some(SemOpcode::ShiftRightLogic),
                    ExprKind::ShiftRightArithmetic => Some(SemOpcode::ShiftRightArithmetic),
                    ExprKind::And => Some(SemOpcode::And),
                    ExprKind::Or => Some(SemOpcode::Or),
                    ExprKind::Xor => Some(SemOpcode::Xor),
                    _ => None,
                };
                if let (Some(opcode), [lhs, rhs]) = (opcode, children.as_slice()) {
                    self.arena.intern_binary(opcode, *lhs, *rhs)
                } else {
                    self.arena.intern_opaque()
                }
            }
        }
    }
}

pub struct MatcherAutomaton {
    by_opcode: HashMap<SemOpcode, Vec<usize>>,
    fallback_rules: Vec<usize>,
    all_rules: Vec<usize>,
}

impl MatcherAutomaton {
    fn compile(rules: &mut [Rule]) -> Self {
        let mut by_opcode: HashMap<SemOpcode, Vec<usize>> = HashMap::new();
        let mut fallback_rules = Vec::new();

        for (idx, rule) in rules.iter_mut().enumerate() {
            rule.compiled_rule_id = CompiledRuleId(idx as u32);
            if let Some(root_opcode) = pattern_root_opcode(&rule.pattern) {
                by_opcode.entry(root_opcode).or_default().push(idx);
            } else {
                fallback_rules.push(idx);
            }
        }

        Self {
            by_opcode,
            fallback_rules,
            all_rules: (0..rules.len()).collect(),
        }
    }

    fn candidate_rules(&self, node: &SemNode) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(specific) = self.by_opcode.get(&node.opcode) {
            out.extend(specific.iter().copied());
        }
        out.extend(self.fallback_rules.iter().copied());

        if out.is_empty() {
            out.extend(self.all_rules.iter().copied());
        }
        out
    }
}

fn pattern_root_opcode(pattern: &ExprPostGraph) -> Option<SemOpcode> {
    match pattern.get_node(pattern.root()?) {
        ExprKind::Constant => match pattern.get_leaf_data(pattern.root()?) {
            Some(ExprPayload::Int(_)) => Some(SemOpcode::IntConst),
            _ => None,
        },
        ExprKind::Add => Some(SemOpcode::Add),
        ExprKind::Sub => Some(SemOpcode::Sub),
        ExprKind::Mul => Some(SemOpcode::Mul),
        ExprKind::Div => Some(SemOpcode::Div),
        ExprKind::ShiftLeft => Some(SemOpcode::ShiftLeft),
        ExprKind::ShiftRightLogic => Some(SemOpcode::ShiftRightLogic),
        ExprKind::ShiftRightArithmetic => Some(SemOpcode::ShiftRightArithmetic),
        ExprKind::And => Some(SemOpcode::And),
        ExprKind::Or => Some(SemOpcode::Or),
        ExprKind::Xor => Some(SemOpcode::Xor),
        ExprKind::Symbol => None,
        _ => None,
    }
}

struct BlockSelectionCache {
    dag: SemDagArena,
    roots_by_op: HashMap<OpId, SemNodeId>,
}

pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    matcher: MatcherAutomaton,
    algorithm: Box<dyn IselAlgorithm>,
    target_model: Box<dyn TargetIselModel>,
    op_lowerings: Vec<OpLowering>,
    block_cache: HashMap<BlockId, BlockSelectionCache>,
}

impl InstructionSelectPass {
    pub fn new(mut rules: Vec<Rule>) -> Self {
        let matcher = MatcherAutomaton::compile(&mut rules);
        Self {
            rules,
            matcher,
            algorithm: Box::new(GlobalDpSelector),
            target_model: Box::new(DefaultTargetIselModel),
            op_lowerings: vec![],
            block_cache: HashMap::new(),
        }
    }

    pub fn with_algorithm(mut self, algorithm: Box<dyn IselAlgorithm>) -> Self {
        self.algorithm = algorithm;
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
                roots_by_op,
            },
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

        self.ensure_block_cache(context, block);

        let Some(cache) = self.block_cache.get(&block.id()) else {
            return Ok(());
        };

        let Some(root) = cache.roots_by_op.get(&op.op().id) else {
            return Ok(());
        };

        let pressure = SelectionPressure {
            estimated_live_operands: op.op().operands.len() as u32,
            estimated_register_pressure: self.target_model.estimate_register_pressure(op),
        };

        let result = self.algorithm.select_function(SelectionInput {
            dag: &cache.dag,
            root: *root,
            op,
            rules: &self.rules,
            matcher: &self.matcher,
            pressure,
            target_model: self.target_model.as_ref(),
            context,
        });

        if let Some(selection) = result.selection {
            let rule = &self.rules[selection.rule_index];
            let plan = (rule.emit_plan_fn)(context, op, &selection.m)?;
            let new_op = plan.into_op();
            rewriter.replace_op(op, new_op.as_ref())?;
        }

        Ok(())
    }
}
