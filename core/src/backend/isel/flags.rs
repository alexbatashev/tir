//! Whether a rule's instruction raises the floating-point flags an
//! operation's semantics say it raises.

use std::collections::HashMap;

use tir::{
    Context, HasResourceSemantics, NodeId, OpHandle,
    sem::{SemGraph, SemType, SymKind, SymPayload, Width, canonicalize_for_selection},
};

use super::{FpFlags, Rule};

pub(super) fn raises(
    context: &Context,
    op: &OpHandle,
    rule: &Rule,
    bound: &[(u32, usize)],
) -> bool {
    let Some(interface) = op.clone().as_interface::<dyn HasResourceSemantics>() else {
        return true;
    };
    let semantics = interface.resource_semantics();
    let Some(raised) = semantics.raised_flags else {
        return true;
    };
    let FpFlags::Exact(pattern) = &rule.fp_flags else {
        return false;
    };
    let operands = op.operands();
    // Both terms are compared over the operation's operands by number.
    let named = |payload: &SymPayload<tir::ValueId>| match payload {
        SymPayload::Value(value) => operands.iter().position(|operand| operand == value),
        SymPayload::SymbolId(operand) => Some(*operand as usize),
        _ => None,
    };
    let captured = |payload: &SymPayload<tir::ValueId>| match payload {
        SymPayload::SymbolId(symbol) => {
            let mut bound = bound.iter();
            bound
                .find(|(candidate, _)| candidate == symbol)
                .map(|&(_, operand)| operand)
        }
        _ => None,
    };
    let ty = |operand: u32| {
        let value = *operands.get(operand as usize)?;
        tir::sem::egraph::semantic_type(context, context.get_value(value).ty())
    };
    let normalize =
        |graph: &SemGraph, root, bind: &dyn Fn(&SymPayload<tir::ValueId>) -> Option<usize>| {
            let mut normalized = SemGraph::new();
            let bind =
                |payload: &SymPayload<tir::ValueId>| bind(payload).map(|operand| operand as u32);
            let root = normalize_node(graph, root, &mut normalized, &mut HashMap::new(), &bind)?;
            for node in normalized.preorder(root) {
                if let Some(SymPayload::SymbolId(operand)) = normalized.get_leaf_data(node)
                    && let Some(declared) = normalized
                        .get_annotation(node)
                        .and_then(|m| m.actual_type)
                        .and_then(|ty| tir::sem::egraph::semantic_type(context, ty))
                    && let Some(held) = ty(*operand)
                {
                    tir::sem::TypeUnifier::default()
                        .unify(&declared, &held)
                        .ok()?;
                }
            }
            let types = tir::sem::infer_types(&normalized, |node| {
                normalized
                    .get_annotation(node)
                    .and_then(|m| m.actual_type)
                    .and_then(|ty| tir::sem::egraph::semantic_type(context, ty))
                    .or_else(|| match normalized.get_leaf_data(node) {
                        Some(SymPayload::SymbolId(operand)) => ty(*operand),
                        _ => None,
                    })
            })
            .ok()?;
            for (index, ty) in types.iter().enumerate() {
                let node = NodeId::from_index(index);
                if let Some(SymPayload::Int(value)) = normalized.get_leaf_data(node) {
                    normalized.set_leaf_data(node, SymPayload::Int(widened(value, ty)));
                }
            }
            Some(canonicalize_for_selection(
                &normalized,
                root,
                &Default::default(),
            ))
        };
    let Some((source, source_root, _)) = normalize(&semantics.graph, raised, &named) else {
        return false;
    };
    let Some(root) = pattern.root() else {
        return false;
    };
    let Some((target, target_root, _)) = normalize(pattern, root, &captured) else {
        return false;
    };
    source.subgraph_eq(source_root, &target, target_root)
}

// A literal of a pattern at the width its place in the term has.
fn widened(value: &tir::utils::APInt, ty: &SemType) -> tir::utils::APInt {
    let width = match ty {
        SemType::Bits(Width::Const(width)) | SemType::RawBits(Width::Const(width)) => *width,
        _ => return value.clone(),
    };
    if width <= value.width() || width > 64 {
        return value.clone();
    }
    if value.is_signed() {
        value.sign_extend(width)
    } else {
        value.zero_extend(width)
    }
}

fn normalize_node(
    source: &SemGraph,
    node: NodeId,
    target: &mut SemGraph,
    memo: &mut HashMap<NodeId, NodeId>,
    bind: &impl Fn(&SymPayload<tir::ValueId>) -> Option<u32>,
) -> Option<NodeId> {
    if let Some(&copied) = memo.get(&node) {
        return Some(copied);
    }
    let kind = *source.get_node(node);
    let children: Vec<_> = source
        .children(node)
        .enumerate()
        .map(|(index, child)| {
            if kind == SymKind::StateRead && index == 0 {
                Some(target.add_node(SymKind::StateBlock))
            } else {
                normalize_node(source, child, target, memo, bind)
            }
        })
        .collect::<Option<Vec<_>>>()?;
    let copied = target.add_node(kind);
    if let Some(payload) = source.get_leaf_data(node) {
        target.set_leaf_data(
            copied,
            if kind == SymKind::Symbol {
                SymPayload::SymbolId(bind(payload)?)
            } else {
                payload.clone()
            },
        );
    }
    if let Some(ty) = source.get_annotation(node).and_then(|m| m.actual_type) {
        target.annotation_mut(copied).actual_type = Some(ty);
    }
    for child in children {
        target.add_edge(copied, child);
    }
    memo.insert(node, copied);
    Some(copied)
}
