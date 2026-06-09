//! PBQP cover construction over the saturated e-graph: match bindings, the
//! alternative/compatibility model, and the solved cover.

use std::collections::{HashMap, HashSet};

use tir::{
    OpId, ValueId,
    egraph::EClassId,
    graph::{Dag, NodeId},
    pbqp::{self, INF_COST, PbqpAlternative, PbqpMatrix, PbqpProblem},
    sem_expr::ExprKind,
};

use super::node::{Binding, SemEGraph, class_binding};
use super::pattern::CompiledIselPattern;
use super::{IselCostModel, RuleMatch};

#[derive(Clone, Debug)]
pub(crate) struct CaptureBindings {
    pub(crate) entries: Vec<(u32, EClassId)>,
}

impl CaptureBindings {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub(crate) fn bind(&mut self, symbol: u32, class: EClassId) -> bool {
        if let Some((_, existing)) = self.entries.iter().find(|(sym, _)| *sym == symbol) {
            *existing == class
        } else {
            self.entries.push((symbol, class));
            true
        }
    }

    pub(crate) fn to_rule_match(
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
pub(crate) struct PatternNodeBinding {
    pub(crate) pattern_node: NodeId,
    pub(crate) class: EClassId,
    pub(crate) is_boundary: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FullMatchBindings {
    pub(crate) captures: CaptureBindings,
    pub(crate) pattern_nodes: Vec<PatternNodeBinding>,
}

impl FullMatchBindings {
    pub(crate) fn class_for_pattern(&self, pattern_node: NodeId) -> Option<EClassId> {
        self.pattern_nodes
            .iter()
            .find(|binding| binding.pattern_node == pattern_node)
            .map(|binding| binding.class)
    }
}
#[derive(Clone, Debug)]
pub(crate) enum PbqpIselAlternative {
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
pub(crate) struct PbqpIselMatch {
    pub(crate) pattern_index: usize,
    pub(crate) rule_index: usize,
    pub(crate) root: EClassId,
    pub(crate) pattern_root: NodeId,
    pub(crate) bindings: FullMatchBindings,
    pub(crate) cost: u64,
}
/// A solved cover: the chosen alternative for every PBQP node, the e-class each
/// PBQP node stands for (same index), and the achieved cost.
pub(crate) struct DagCover {
    pub(crate) choices: Vec<PbqpIselAlternative>,
    pub(crate) classes: Vec<EClassId>,
}

/// Build and solve the PBQP cover over the e-graph: one PBQP node per e-class,
/// alternatives drawn from the instruction-pattern `matches`, and parent -> child
/// compatibility derived from each match's pattern structure (not a single DAG
/// shape, since a class may be realized by several equivalent e-nodes). The
/// `edge_cost` closure prices satisfied materialization edges. Returns `None` if
/// the instance is infeasible (a class with no valid alternative).
pub(crate) fn build_eclass_cover(
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

    let is_terminal = |c: EClassId| {
        egraph
            .nodes(c)
            .iter()
            .any(|&id| egraph.children(id).next().is_none())
    };

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
/// Coverage completeness: every op-root e-class must be emittable as an instruction
/// (it roots some match) or consumable by a parent match (it is an interior node of
/// some match). A non-terminal op-root that is neither cannot be selected by this
/// rule set — even after saturation — so selection fails with a diagnostic.
pub(crate) fn completeness_error(
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
        if egraph
            .nodes(class)
            .iter()
            .any(|&id| egraph.children(id).next().is_none())
        {
            continue;
        }
        if has_root.contains(&class) || has_internal.contains(&class) {
            continue;
        }
        if let Some(kind) = egraph
            .nodes(class)
            .first()
            .map(|&id| egraph.get_node(id).kind)
            && !missing.contains(&kind)
        {
            missing.push(kind);
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
pub(crate) fn alternatives_compatible(
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

pub(crate) fn child_requirement(
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
pub(crate) fn materialization_edge_cost(
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
        egraph
            .nodes(parent)
            .first()
            .map(|&id| egraph.get_node(id).kind),
        egraph
            .nodes(child)
            .first()
            .map(|&id| egraph.get_node(id).kind),
    ) else {
        return 0;
    };
    cost_model.edge_cost(parent_kind, child_kind, true)
}

pub(crate) fn parent_satisfies_internal_child(
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

pub(crate) enum ChildRequirement {
    Materialized,
    SameMatch {
        match_id: usize,
        pattern_node: NodeId,
    },
}
