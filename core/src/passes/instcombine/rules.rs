//! The builtin dialect's rewrites. Each applier only *adds* equivalent forms (via
//! [`EGraph::union`]) during saturation; a rule that introduces an op pairs with an
//! `emit` that builds it at write-back, reached through the chosen node's [`OpProv`]
//! — the engine never routes by op identity. Constant folding is just another rule,
//! generic over any op's [`ConstantFold`] interface.

use tir_symbolic::egraph::{EGraph, Id, Pattern, Rewrite, Rhs, Substitution, Var};

use crate::utils::APInt;
use crate::{
    ConstantFold, Context, Operation, OperationRef, PassError, Rewriter, TypeId, ValueId,
    builtin::{AddIOp, IntegerType, MulIOp, ShlIOp, SubIOp, ops},
    sem::Value,
};

use crate::analysis::GateNode;

use super::node::{Node, OpProv};

/// Pattern-variable symbols are positional operand indices.
type Sym = u32;
type Rule = Rewrite<Node, Sym>;

/// Builds the op a rewrite introduces, from its already-materialized operands.
pub type EmitFn = Box<
    dyn Fn(&Context, &[ValueId], TypeId, &OperationRef, &mut Rewriter) -> Result<ValueId, PassError>
        + Send
        + Sync,
>;

/// The rewrites plus, per rewrite, how to build the op it introduces (`None` for
/// rewrites that only union existing classes or fold to a constant). The two vecs
/// share an index — the [`OpProv::Introduced`] tag a rule stamps on the node it adds.
pub struct Ruleset {
    pub rewrites: Vec<Rule>,
    pub emits: Vec<Option<EmitFn>>,
}

impl Ruleset {
    fn push(&mut self, rewrite: Rule, emit: Option<EmitFn>) {
        self.rewrites.push(rewrite);
        self.emits.push(emit);
    }
}

/// The ruleset, built per run so appliers can capture a [`Context`] clone (the
/// applier signature carries no context).
pub fn builtin_ruleset(context: &Context) -> Ruleset {
    let mut rs = Ruleset {
        rewrites: Vec::new(),
        emits: Vec::new(),
    };
    rs.push(add_zero(), None);
    rs.push(mul_identities(), None);
    let shl_idx = rs.rewrites.len();
    rs.push(mul_pow2_to_shl(shl_idx), Some(emit_shl()));
    rs.push(sub_self(context.clone()), None);
    rs.push(const_fold(context.clone()), None);
    rs.push(gamma_fold(), None);
    rs
}

/// A rewrite over `O(a, b)` whose applier receives the root and the two operand
/// classes.
fn binop<O: Operation>(
    name: &str,
    apply: impl Fn(&mut EGraph<Node>, Id, Id, Id) + Send + Sync + 'static,
) -> Rule {
    let mut lhs = Pattern::new();
    let a = lhs.var(Var::Symbol(0));
    let b = lhs.var(Var::Symbol(1));
    lhs.add(Node::pattern::<O>(vec![a, b]));
    Rewrite::new(
        name,
        lhs,
        Rhs::Apply(Box::new(move |eg, subst, root| {
            apply(eg, root, operand(subst, 0), operand(subst, 1));
        })),
    )
}

fn operand(subst: &Substitution<Sym>, i: u32) -> Id {
    subst.get(&Var::Symbol(i)).expect("bound operand")
}

/// The constant value carried by `class`, if any e-node in it is a constant.
fn const_value(eg: &EGraph<Node>, class: Id) -> Option<APInt> {
    eg.nodes(eg.find(class)).iter().find_map(|n| match n {
        Node::Const { value, .. } => Some(value.clone()),
        _ => None,
    })
}

/// The result type of `class`, read from any op e-node in it.
fn class_type(eg: &EGraph<Node>, class: Id) -> Option<TypeId> {
    eg.nodes(eg.find(class)).iter().find_map(Node::op_type)
}

fn int_width(context: &Context, ty: TypeId) -> Option<u32> {
    (context.get_type_data(ty).as_ref() as &dyn std::any::Any)
        .downcast_ref::<IntegerType>()
        .map(IntegerType::width)
}

// `x + 0 -> x`
fn add_zero() -> Rule {
    binop::<AddIOp>("add-zero", |eg, root, lhs, rhs| {
        for (c, other) in [(lhs, rhs), (rhs, lhs)] {
            if const_value(eg, c).is_some_and(|v| v.is_zero()) {
                eg.union(root, other);
                return;
            }
        }
    })
}

// `x * 1 -> x` and `x * 0 -> 0` (the zero operand's class is the result).
fn mul_identities() -> Rule {
    binop::<MulIOp>("mul-identities", |eg, root, lhs, rhs| {
        for (c, other) in [(lhs, rhs), (rhs, lhs)] {
            match const_value(eg, c) {
                Some(v) if v.is_one() => {
                    eg.union(root, other);
                    return;
                }
                Some(v) if v.is_zero() => {
                    eg.union(root, c);
                    return;
                }
                _ => {}
            }
        }
    })
}

// `x * 2^k -> x << k`. Adds the shift (built at write-back by `emit_shl`, tagged with
// this rule's index) and a constant shift amount.
fn mul_pow2_to_shl(idx: usize) -> Rule {
    binop::<MulIOp>("mul-pow2-to-shl", move |eg, root, lhs, rhs| {
        for (c, x) in [(lhs, rhs), (rhs, lhs)] {
            let Some(v) = const_value(eg, c) else {
                continue;
            };
            if v.is_zero() || v.is_one() || v.count_ones() != 1 {
                continue;
            }
            let Some(ty) = class_type(eg, root) else {
                continue;
            };
            let amount = eg.add(konst(APInt::new(
                v.width(),
                v.count_trailing_zeros() as u64,
            )));
            let shifted = eg.add(Node::introduced::<ShlIOp>(ty, 1, idx, vec![x, amount]));
            eg.union(root, shifted);
            return;
        }
    })
}

fn emit_shl() -> EmitFn {
    Box::new(|context, operands, ty, target, rewriter| {
        let op = ops::shli(context, operands[0], operands[1], ty).build();
        rewriter.insert_op_before(target, &op)?;
        Ok(op.result())
    })
}

// `x - x -> 0`
fn sub_self(context: Context) -> Rule {
    binop::<SubIOp>("sub-self", move |eg, root, lhs, rhs| {
        if eg.find(lhs) != eg.find(rhs) {
            return;
        }
        if let Some(width) = class_type(eg, root).and_then(|ty| int_width(&context, ty)) {
            let zero = eg.add(konst(APInt::zero(width)));
            eg.union(root, zero);
        }
    })
}

// `γ(c, x, x) -> x` (equal arms) and `γ(1, t, f) -> t` / `γ(0, t, f) -> f` (known
// condition): a merge that doesn't depend on the branch is that arm.
fn gamma_fold() -> Rule {
    let mut lhs = Pattern::new();
    let c = lhs.var(Var::Symbol(0));
    let t = lhs.var(Var::Symbol(1));
    let f = lhs.var(Var::Symbol(2));
    let any = ValueId::from_number(u32::MAX);
    lhs.add(Node::Gate(
        GateNode::Gamma {
            value: any,
            cond: any,
        },
        vec![c, t, f],
    ));
    Rewrite::new(
        "gamma-fold",
        lhs,
        Rhs::Apply(Box::new(|eg, subst, root| {
            let (c, t, f) = (operand(subst, 0), operand(subst, 1), operand(subst, 2));
            let arm = if eg.find(t) == eg.find(f) {
                Some(t)
            } else {
                match const_value(eg, c) {
                    Some(v) if v.is_zero() => Some(f),
                    Some(v) if !v.is_zero() => Some(t),
                    _ => None,
                }
            };
            if let Some(arm) = arm {
                eg.union(root, arm);
            }
        })),
    )
}

// Generic constant folding: any seeded op in the class whose operands are all
// constant is evaluated through its `ConstantFold` interface (derived from its
// `sem`). A bare-symbol LHS visits every class.
fn const_fold(context: Context) -> Rule {
    let mut lhs = Pattern::new();
    lhs.var(Var::Symbol(0));
    Rewrite::new(
        "const-fold",
        lhs,
        Rhs::Apply(Box::new(move |eg, _subst, root| {
            if let Some(v) = fold_class(&context, eg, eg.find(root)) {
                let folded = eg.add(konst(v));
                eg.union(root, folded);
            }
        })),
    )
}

/// The constant a class folds to, if some seeded op in it has all-constant operands.
fn fold_class(context: &Context, eg: &EGraph<Node>, class: Id) -> Option<APInt> {
    eg.nodes(class).iter().find_map(|node| {
        let Node::Op {
            prov: OpProv::Seeded(op),
            args,
            ..
        } = node
        else {
            return None;
        };
        if !context.has_operation(*op) {
            return None;
        }
        let operands: Vec<Value> = args
            .iter()
            .map(|&c| const_value(eg, c).map(Value::Int))
            .collect::<Option<_>>()?;
        match context
            .get_op(*op)
            .as_interface::<dyn ConstantFold>()?
            .fold(&operands)
        {
            Some(Value::Int(v)) => Some(v),
            _ => None,
        }
    })
}

fn konst(value: APInt) -> Node {
    Node::Const {
        value,
        origin: None,
    }
}
