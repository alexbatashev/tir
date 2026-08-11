//! Semantic seeding: an op's declared semantics, replayed as `Kind::Sym` terms
//! over the classes the view already gave its operands.
//!
//! A view seeds every pure simple node as its operator identity, which is what
//! the peephole rules match. The selection theory is written in the *semantic*
//! vocabulary instead, so a class holding only an op identity binds none of its
//! axioms. Replaying [`crate::Operation::semantic_expr`] into the same e-graph
//! and unioning its root with the identity's class puts both vocabularies on one
//! class: a rule keyed on either fires on the same value.
//!
//! The expansion is built over the *operand classes themselves* — a
//! `SymbolId(i)` leaf resolves to the class the view holds for input `i` — so a
//! shared operand is one class for both vocabularies, and an operand's own
//! expansion is reached through it rather than rebuilt.
//!
//! Nothing partial is seeded. An expansion the view cannot spell whole — an
//! operand index the op does not have, a payload the vocabulary does not carry,
//! a node at an arity its kind rejects — reports `None` and leaves the node with
//! its identity alone, rather than minting an opaque leaf that would assert an
//! equality with an unknown.

use std::sync::Arc;

use tir_symbolic::egraph::{EGraph, Id};

use crate::backend::isel::node::{ir_type, semantic_type};
use crate::graph::{Dag, MetaDag, NodeId};
use crate::sem::{
    SemGraph, SemNode as Node, SemType, SymKind, SymPayload, infer_types, template_node,
};
use crate::{Context, OpInstance, TypeId};

/// Replay `probe`'s declared semantics over `operands`, and report the class of
/// the expansion's root. `None` for an op that declares no semantics, or one
/// whose expansion the view cannot spell whole.
pub(super) fn expand(
    context: &Context,
    eg: &mut EGraph<Node>,
    probe: &Arc<OpInstance>,
    operands: &[Id],
) -> Option<Id> {
    let mut graph = SemGraph::new();
    let root = probe.clone().as_dyn_op().semantic_expr(&mut graph)?;
    let types = infer_local_types(context, eg, &graph, operands);
    lower(context, eg, &graph, root, operands, types.as_deref())
}

/// The IR type recorded on a class, from whichever member carries one — the
/// same reading the width-dependent axioms take a class's width from.
fn class_type(eg: &EGraph<Node>, class: Id) -> Option<TypeId> {
    eg.nodes(class).iter().find_map(|node| node.ty)
}

/// A type for every node of the expansion: the op's own annotations where it
/// made them, and the operand classes' types under its symbol leaves.
fn infer_local_types(
    context: &Context,
    eg: &EGraph<Node>,
    graph: &SemGraph,
    operands: &[Id],
) -> Option<Vec<SemType>> {
    infer_types(graph, |node| {
        graph
            .get_actual_type(node)
            .and_then(|ty| semantic_type(context, ty))
            .or_else(|| match graph.get_leaf_data(node) {
                Some(SymPayload::SymbolId(id)) => operands
                    .get(*id as usize)
                    .and_then(|&class| class_type(eg, class))
                    .and_then(|ty| semantic_type(context, ty)),
                _ => None,
            })
    })
    .ok()
}

fn lower(
    context: &Context,
    eg: &mut EGraph<Node>,
    graph: &SemGraph,
    node: NodeId,
    operands: &[Id],
    types: Option<&[SemType]>,
) -> Option<Id> {
    let ty = graph
        .get_actual_type(node)
        .or_else(|| types.and_then(|types| ir_type(context, &types[node.index()])));
    match graph.get_node(node) {
        SymKind::Symbol => match graph.get_leaf_data(node) {
            Some(SymPayload::SymbolId(id)) => operands.get(*id as usize).copied(),
            _ => None,
        },
        SymKind::Constant => match graph.get_leaf_data(node) {
            Some(payload @ SymPayload::Int(_)) => {
                Some(eg.add(template_node(SymKind::Constant, Some(payload.clone()), ty)))
            }
            _ => None,
        },
        &kind => {
            let mut children = graph
                .children(node)
                .map(|child| lower(context, eg, graph, child, operands, types))
                .collect::<Option<Vec<_>>>()?;
            if !kind.accepts_arity(children.len()) {
                return None;
            }
            if kind.is_commutative() {
                children.sort();
            }
            let mut term = template_node(kind, None, ty);
            term.children = children;
            Some(eg.add(term))
        }
    }
}
