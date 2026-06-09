//! Discovery of the proved algebraic rewrites used to saturate the program
//! e-graph before covering.

use tir::{
    Context,
    egraph::{EMatch, Rewrite},
    graph::{Pattern, PatternExpr},
    sem_expr::{ExprKind, ExprPayload, FuzzOracle, confirm_extension_via_shifts},
    utils::APInt,
};

use super::node::{SemEGraph, SemNode, class_width, template_node};
use super::pattern::{CompiledIselPattern, atomic_kinds};

/// Discover the algebraic bridges the rule set needs to cover sub-word extensions.
/// If the target has `slli` plus the matching right shift, confirm the standard
/// shift-pair identity against the [`FuzzOracle`] and, on success, emit a
/// width-parameterized rewrite. No hand-written selection rule is involved — only a
/// proved bit-vector lemma the target's own instructions happen to realize.
pub(crate) fn discover_rewrites(patterns: &[CompiledIselPattern]) -> Vec<Rewrite<SemNode, ()>> {
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
pub(crate) fn extension_rewrite(ext_kind: ExprKind, shr_kind: ExprKind) -> Rewrite<SemNode, ()> {
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
            let shift_amount = egraph.add(amount, &[], None);
            let shl = egraph.add(
                template_node(ExprKind::ShiftLeft, None, None),
                &[value_class, shift_amount],
                None,
            );
            let shr = egraph.add(
                template_node(shr_kind, None, None),
                &[shl, shift_amount],
                None,
            );
            egraph.union(root_class, shr);
        }),
    }
}
