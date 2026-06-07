//! The builtin dialect's rewrites. Each is an e-graph [`Rewrite`] (searcher +
//! applier that only *adds* equivalent forms during saturation) paired with an
//! optional `emit` that materializes the op the rewrite introduces. Construction is
//! owned by the rewrite, reached through the node's saturation provenance — the
//! engine never routes by op identity. Constant folding is just another rewrite,
//! generic over any op's [`ConstantFold`] interface.

use crate::egraph::{EClassId, EGraph, EMatch, Rewrite};
use crate::graph::{Dag, NodeId, Pattern, PatternExpr};
use crate::sem_expr::Value;
use crate::utils::APInt;
use crate::{
    ConstantFold, Context, Operation, OperationRef, PassError, Rewriter, TypeId, ValueId,
    builtin::{AddIOp, IntegerType, MulIOp, ShlIOp, SubIOp, ops},
};

use super::term::{Leaf, Term, op_term};

/// Builds the op a rewrite introduces, from its already-materialized operand values.
pub type EmitFn = Box<
    dyn Fn(&Context, &[ValueId], TypeId, &OperationRef, &mut Rewriter) -> Result<ValueId, PassError>
        + Send
        + Sync,
>;

/// The rewrites plus, per rewrite, how to build the op it introduces (`None` for
/// rewrites that only union existing classes or fold to a constant). The two vecs
/// share an index, which is the saturation provenance recorded on each node.
pub struct Ruleset {
    pub rewrites: Vec<Rewrite<Term, Leaf>>,
    pub emits: Vec<Option<EmitFn>>,
}

impl Ruleset {
    fn push(&mut self, rewrite: Rewrite<Term, Leaf>, emit: Option<EmitFn>) {
        self.rewrites.push(rewrite);
        self.emits.push(emit);
    }
}

pub fn builtin_ruleset() -> Ruleset {
    let mut rs = Ruleset {
        rewrites: Vec::new(),
        emits: Vec::new(),
    };
    rs.push(rule::<AddIOp>(add_zero), None);
    rs.push(rule::<MulIOp>(mul_identities), None);
    rs.push(rule::<MulIOp>(mul_pow2_to_shl), Some(emit_shl()));
    rs.push(rule::<SubIOp>(sub_self), None);
    rs.push(
        Rewrite::new("const-fold", any_searcher(), Box::new(const_fold)),
        None,
    );
    rs
}

type Applier = fn(&Context, &mut EGraph<Term, Leaf>, &EMatch);

fn rule<O: Operation>(apply: Applier) -> Rewrite<Term, Leaf> {
    Rewrite::new(O::name(), binop_searcher::<O>(), Box::new(apply))
}

/// A searcher matching `O(_, _)` with both operands left as wildcards.
fn binop_searcher<O: Operation>() -> Pattern<Term, ()> {
    let mut p = Pattern::new(());
    let lhs = p.add_node(PatternExpr::Boundary);
    let rhs = p.add_node(PatternExpr::Boundary);
    let root = p.add_node(PatternExpr::Node(op_term::<O>(false, 0)));
    p.add_edge(root, lhs);
    p.add_edge(root, rhs);
    p.set_root(root);
    p
}

/// Matches every class — the constant folder inspects the class's nodes itself.
fn any_searcher() -> Pattern<Term, ()> {
    let mut p = Pattern::new(());
    let root = p.add_node(PatternExpr::Any);
    p.set_root(root);
    p
}

fn operands(m: &EMatch) -> (EClassId, EClassId) {
    (
        m.binding(NodeId::from_index(0)),
        m.binding(NodeId::from_index(1)),
    )
}

/// The constant value carried by `class`, if any e-node in it is a constant.
fn const_value(g: &EGraph<Term, Leaf>, class: EClassId) -> Option<APInt> {
    g.nodes(g.find(class))
        .iter()
        .find_map(|&id| match (g.get_node(id), g.get_leaf_data(id)) {
            (Term::Const, Some(Leaf::Int(v))) => Some(v.clone()),
            _ => None,
        })
}

fn int_width(context: &Context, ty: TypeId) -> Option<u32> {
    (context.get_type_data(ty).as_ref() as &dyn std::any::Any)
        .downcast_ref::<IntegerType>()
        .map(IntegerType::width)
}

fn root_width(context: &Context, g: &EGraph<Term, Leaf>, m: &EMatch) -> Option<u32> {
    let ty = g
        .nodes(g.find(m.root()))
        .iter()
        .find_map(|&id| g.get_actual_type(id))?;
    int_width(context, ty)
}

// `x + 0 -> x`
fn add_zero(_context: &Context, g: &mut EGraph<Term, Leaf>, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, other) in [(lhs, rhs), (rhs, lhs)] {
        if const_value(g, c).is_some_and(|v| v.is_zero()) {
            g.union(m.root(), other);
            return;
        }
    }
}

// `x * 1 -> x` and `x * 0 -> 0`
fn mul_identities(_context: &Context, g: &mut EGraph<Term, Leaf>, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, other) in [(lhs, rhs), (rhs, lhs)] {
        match const_value(g, c) {
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

// `x * 2^k -> x << k`. Introduces the shift (built by `emit_shl`) and a constant
// shift amount (materialized as a constant).
fn mul_pow2_to_shl(_context: &Context, g: &mut EGraph<Term, Leaf>, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    for (c, x) in [(lhs, rhs), (rhs, lhs)] {
        let Some(v) = const_value(g, c) else { continue };
        if v.is_zero() || v.is_one() || v.count_ones() != 1 {
            continue;
        }
        let amount = APInt::new(v.width(), v.count_trailing_zeros() as u64);
        let amount = g.add(Term::Const, &[], Some(Leaf::Int(amount)));
        let shifted = g.add(op_term::<ShlIOp>(false, 1), &[x, amount], None);
        g.union(m.root(), shifted);
        return;
    }
}

fn emit_shl() -> EmitFn {
    Box::new(|context, operands, ty, target, rewriter| {
        let op = ops::shli(context, operands[0], operands[1], ty).build();
        rewriter.insert_op_before(target, &op)?;
        Ok(op.result())
    })
}

// `x - x -> 0`
fn sub_self(context: &Context, g: &mut EGraph<Term, Leaf>, m: &EMatch) {
    let (lhs, rhs) = operands(m);
    if g.find(lhs) != g.find(rhs) {
        return;
    }
    if let Some(width) = root_width(context, g, m) {
        let zero = g.add(Term::Const, &[], Some(Leaf::Int(APInt::zero(width))));
        g.union(m.root(), zero);
    }
}

// Generic constant folding: any op in the class whose operands are all constant is
// evaluated through its `ConstantFold` interface (derived from its `sem`).
fn const_fold(context: &Context, g: &mut EGraph<Term, Leaf>, m: &EMatch) {
    let class = g.find(m.root());
    let mut folded = None;
    for &node in g.nodes(class) {
        if !matches!(g.get_node(node), Term::Op { .. }) {
            continue;
        }
        let Some(op_id) = g.get_original_op(node) else {
            continue;
        };
        let operands: Option<Vec<Value>> = g
            .child_classes(node)
            .into_iter()
            .map(|c| const_value(g, c).map(Value::Int))
            .collect();
        let (Some(operands), Some(fold)) = (
            operands,
            context.get_op(op_id).as_interface::<dyn ConstantFold>(),
        ) else {
            continue;
        };
        if let Some(Value::Int(v)) = fold.fold(&operands) {
            folded = Some(v);
            break;
        }
    }
    if let Some(v) = folded {
        let constant = g.add(Term::Const, &[], Some(Leaf::Int(v)));
        g.union(class, constant);
    }
}
