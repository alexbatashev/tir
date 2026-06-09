//! Instruction selection over semantic e-graphs.
//!
//! Each block's operations are lowered into an e-graph of semantic expressions
//! ([`builder`]), saturated with proved algebraic rewrites ([`rewrites`]), and
//! covered by the target's instruction patterns ([`pattern`]) via a PBQP
//! instance over e-classes ([`cover`]). The solved cover becomes an emission
//! plan ([`emit`]) the pass commits through the rewriter.

mod builder;
mod cover;
mod emit;
mod node;
mod pattern;
mod rewrites;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use tir::{
    Block, BlockId, Context, OpId, Operation, OperationRef, Pass, PassError, PassTarget, Rewriter,
    ValueId,
    egraph::{EClassId, Rewrite},
    graph::{Dag, NodeId, OperandConstraint, PatternExpr},
    sem_expr::{ExprKind, ExprPostGraph},
    utils::APInt,
};

pub use node::{SemEGraph, SemNode};

use builder::SemDagBuilder;
use cover::{
    CaptureBindings, FullMatchBindings, PatternNodeBinding, PbqpIselAlternative, PbqpIselMatch,
    build_eclass_cover, completeness_error, materialization_edge_cost,
};
use emit::{BlockDecision, BlockPlan, EmissionBuilder, synthetic_op_ref};
use pattern::{CompiledIselPattern, compile_isel_pattern, specificity_adjusted_cost};
use rewrites::discover_rewrites;
#[cfg(test)]
use {node::template_node, rewrites::extension_rewrite};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompiledRuleId(u32);

#[derive(Debug, Clone)]
pub struct RuleMatch {
    int_bindings: Vec<(u32, APInt)>,
    value_bindings: Vec<(u32, ValueId)>,
}

impl RuleMatch {
    pub(crate) fn new(
        mut int_bindings: Vec<(u32, APInt)>,
        mut value_bindings: Vec<(u32, ValueId)>,
    ) -> Self {
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
pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    compiled_patterns: Vec<CompiledIselPattern>,
    /// Target-independent algebraic identities the program e-graph is saturated
    /// with before covering (e.g. discovered `sext`/shift bridges). Populated by
    /// rewrite discovery; empty means selection is purely syntactic tiling.
    rewrites: Vec<Rewrite<SemNode, ()>>,
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
    pub fn with_rewrites(mut self, rewrites: Vec<Rewrite<SemNode, ()>>) -> Self {
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
                    .any(|&id| cache.egraph.children(id).next().is_some());
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
