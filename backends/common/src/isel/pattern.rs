//! Compilation of rule semantic expressions into matchable patterns.

use std::collections::{HashMap, HashSet};

use tir::{
    Context,
    graph::{Dag, Matchable, NodeId, OperandConstraint, Pattern, PatternExpr},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
};

use super::node::{SemNode, template_node};

/// Headroom factor that lets pattern specificity break ties between equal-cost
/// matches without ever overriding a genuine instruction-cost difference.
pub(crate) const SPECIFICITY_SCALE: u64 = 8;

/// Fold a match's specificity into its cost: scale the instruction cost, then give
/// a small discount for each type-constrained pattern node. So among equally cheap
/// matches the most specific (e.g. i32 `addw` over untyped `add`) wins, while a
/// cheaper instruction still wins outright.
pub(crate) fn specificity_adjusted_cost(cost: u64, specificity: usize) -> u64 {
    cost.saturating_mul(SPECIFICITY_SCALE)
        .saturating_sub((specificity as u64).min(SPECIFICITY_SCALE - 1))
}
pub(crate) struct CompiledIselPattern {
    pub(crate) rule_index: usize,
    pub(crate) pattern: Pattern<SemNode, usize>,
    pub(crate) boundary_symbols: HashMap<NodeId, u32>,
    /// Number of type-constrained pattern nodes — how "specific" this pattern is.
    /// At equal instruction cost, a more specific match is preferred, so an i32
    /// `addw` (one typed node) beats the untyped `add` for an i32 value, while the
    /// untyped `add`/`and` still match every other width.
    pub(crate) specificity: usize,
}

pub(crate) fn compile_isel_pattern(
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

pub(crate) fn compile_isel_pattern_node(
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
/// The semantic kinds for which the rule set provides an atomic materializer (a
/// pattern whose root is that kind with only operand boundaries beneath it).
pub(crate) fn atomic_kinds(patterns: &[CompiledIselPattern]) -> HashSet<ExprKind> {
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
