//! Static algebraic rewrites over the arith subset of [`ExprKind`], expressed as
//! e-graph [`Rewrite`]s. Each searcher matches an operator shape with opaque
//! operands; the applier inspects the bound e-classes for constants and unions in
//! the equivalent (and usually cheaper) form. Cost-driven extraction then keeps
//! whichever representation is cheapest — e.g. a multiply by a power of two stays
//! as a shift only because [`crate::builtin::InstCost`] makes the multiply dear.

use crate::Context;
use crate::egraph::{EClassId, EGraph, EMatch, Rewrite};
use crate::graph::{Dag, NodeId, Pattern, PatternExpr};
use crate::sem_expr::{ExprKind, ExprPayload};
use crate::utils::APInt;

type ArithEGraph = EGraph<ExprKind, ExprPayload>;

pub fn arith_rules() -> Vec<Rewrite<ExprKind, ExprPayload>> {
    let mut rules = vec![
        rule("add-zero", ExprKind::Add, add_zero),
        rule("mul-identities", ExprKind::Mul, mul_identities),
        rule("mul-pow2-to-shl", ExprKind::Mul, mul_pow2_to_shl),
        rule("sub-self", ExprKind::Sub, sub_self),
    ];
    rules.extend(
        [
            ExprKind::Add,
            ExprKind::Sub,
            ExprKind::Mul,
            ExprKind::And,
            ExprKind::Or,
            ExprKind::Xor,
        ]
        .map(|kind| {
            rule("const-fold", kind, move |ctx, g, m| {
                const_fold(ctx, g, m, kind)
            })
        }),
    );
    rules
}

/// A searcher matching `kind(_, _)` with both operands left opaque. The two
/// operand pattern nodes are [`NodeId`] 0 and 1, read back in the applier via
/// [`EMatch::binding`].
fn binop_searcher(kind: ExprKind) -> Pattern<ExprKind, ()> {
    let mut p = Pattern::new(());
    let lhs = p.add_node(PatternExpr::Boundary);
    let rhs = p.add_node(PatternExpr::Boundary);
    let root = p.add_node(PatternExpr::Node(kind));
    p.add_edge(root, lhs);
    p.add_edge(root, rhs);
    p.set_root(root);
    p
}

fn rule(
    name: &'static str,
    kind: ExprKind,
    apply: impl Fn(&Context, &mut ArithEGraph, &EMatch) + Send + Sync + 'static,
) -> Rewrite<ExprKind, ExprPayload> {
    Rewrite::new(name, binop_searcher(kind), Box::new(apply))
}

fn operands(m: &EMatch) -> (EClassId, EClassId) {
    (
        m.binding(NodeId::from_index(0)),
        m.binding(NodeId::from_index(1)),
    )
}

/// The constant value of `class`, if any e-node in it is a constant leaf.
fn const_of(g: &ArithEGraph, class: EClassId) -> Option<APInt> {
    g.nodes(g.find(class))
        .iter()
        .find_map(|&id| match (g.get_node(id), g.get_leaf_data(id)) {
            (ExprKind::Constant, Some(ExprPayload::Int(v))) => Some(v.clone()),
            _ => None,
        })
}

/// The result type width of the matched root, read from any annotated e-node.
fn root_width(ctx: &Context, g: &ArithEGraph, m: &EMatch) -> Option<u32> {
    let ty = g
        .nodes(g.find(m.root()))
        .iter()
        .find_map(|&id| g.get_actual_type(id))?;
    let data = ctx.get_type_data(ty);
    (data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<crate::builtin::IntegerType>()
        .map(|t| t.width())
}

// `x + 0 -> x`
fn add_zero(_ctx: &Context, g: &mut ArithEGraph, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, other) in [(lhs, rhs), (rhs, lhs)] {
        if const_of(g, c).is_some_and(|v| v.is_zero()) {
            g.union(m.root(), other);
            return;
        }
    }
}

// `x * 1 -> x` and `x * 0 -> 0`
fn mul_identities(_ctx: &Context, g: &mut ArithEGraph, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, other) in [(lhs, rhs), (rhs, lhs)] {
        match const_of(g, c) {
            Some(v) if v.is_one() => {
                g.union(m.root(), other);
                return;
            }
            // `c` already names the zero constant's class; reuse it as the result.
            Some(v) if v.is_zero() => {
                g.union(m.root(), c);
                return;
            }
            _ => {}
        }
    }
}

// `x * 2^k -> x << k`
fn mul_pow2_to_shl(_ctx: &Context, g: &mut ArithEGraph, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, x) in [(lhs, rhs), (rhs, lhs)] {
        let Some(v) = const_of(g, c) else { continue };
        if v.is_zero() || v.is_one() || v.count_ones() != 1 {
            continue;
        }
        let amount = APInt::new(v.width(), v.count_trailing_zeros() as u64);
        let amount = g.add(ExprKind::Constant, &[], Some(ExprPayload::Int(amount)));
        let shifted = g.add(ExprKind::ShiftLeft, &[x, amount], None);
        g.union(m.root(), shifted);
        return;
    }
}

// `x - x -> 0`
fn sub_self(ctx: &Context, g: &mut ArithEGraph, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    if g.find(lhs) != g.find(rhs) {
        return;
    }
    if let Some(width) = root_width(ctx, g, m) {
        let zero = g.add(
            ExprKind::Constant,
            &[],
            Some(ExprPayload::Int(APInt::zero(width))),
        );
        g.union(m.root(), zero);
    }
}

// Fold a binary operator over two constant operands.
fn const_fold(_ctx: &Context, g: &mut ArithEGraph, m: &EMatch, kind: ExprKind) {
    let (lhs, rhs) = operands(m);
    let (Some(a), Some(b)) = (const_of(g, lhs), const_of(g, rhs)) else {
        return;
    };
    let folded = match kind {
        ExprKind::Add => a.add(&b),
        ExprKind::Sub => a.sub(&b),
        ExprKind::Mul => a.mul(&b),
        ExprKind::And => a.and(&b),
        ExprKind::Or => a.or(&b),
        ExprKind::Xor => a.xor(&b),
        _ => return,
    };
    let folded = g.add(ExprKind::Constant, &[], Some(ExprPayload::Int(folded)));
    g.union(m.root(), folded);
}
