//! The e-node label and leaf payload for the InstCombine e-graph.
//!
//! The label is dialect-agnostic. [`Term::Op`] carries an operation's *registered
//! identity* (dialect + name) plus the two properties the e-graph needs about a
//! node — commutativity and cost — both read generically from the op's interfaces
//! ([`Commutative`], [`OpCost`]) when the node is interned. The engine never
//! enumerates ops or matches on names; only dialect-bound rules name their own ops,
//! type-safely through [`op_term`].

use std::sync::Arc;

use crate::graph::Matchable;
use crate::utils::APInt;
use crate::{Commutative, Context, OpCost, OpInstance, Operation, ValueId};

/// An e-node label: a real operation by identity, a constant, or an opaque
/// external SSA value.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Term {
    /// A pure, single-result operation. `commutative` and `cost` are intrinsic to
    /// the op kind (read from its interfaces at intern time), so the label is
    /// self-describing for matching, hash-consing and cost extraction.
    Op {
        dialect: &'static str,
        name: &'static str,
        commutative: bool,
        cost: u32,
    },
    /// A compile-time constant; its value lives in [`Leaf::Int`].
    Const,
    /// An SSA value with no modeled defining op; the value is in [`Leaf::Value`].
    Opaque,
}

/// A leaf payload: a constant's value, or the IR value an opaque leaf stands for.
#[derive(Clone, PartialEq, Debug)]
pub enum Leaf {
    Int(APInt),
    Value(ValueId),
}

impl Term {
    /// The label for `instance`, reading identity, commutativity and cost from its
    /// interfaces. Generic over any dialect op.
    pub fn of_op(instance: &Arc<OpInstance>) -> Term {
        let commutative = instance
            .clone()
            .as_interface::<dyn Commutative>()
            .is_some_and(|c| c.is_commutative());
        let cost = instance
            .clone()
            .as_interface::<dyn OpCost>()
            .map(|c| c.cost())
            .unwrap_or(1);
        Term::Op {
            dialect: instance.dialect,
            name: instance.name,
            commutative,
            cost,
        }
    }
}

/// The label a rule uses for an op it introduces, naming the op type-safely via its
/// own `Operation::name`/`dialect`. `commutative`/`cost` describe the kind (rules
/// only emit cheap, non-expensive ops, so the cost is the op's modeled value).
pub fn op_term<O: Operation>(commutative: bool, cost: u32) -> Term {
    Term::Op {
        dialect: O::dialect(),
        name: O::name(),
        commutative,
        cost,
    }
}

impl Matchable for Term {
    fn is_leaf(&self, _ctx: &Context) -> bool {
        matches!(self, Term::Const | Term::Opaque)
    }

    fn num_children(&self, _ctx: &Context) -> usize {
        0
    }

    fn is_constant(&self) -> bool {
        matches!(self, Term::Const)
    }

    fn is_commutative(&self) -> bool {
        matches!(
            self,
            Term::Op {
                commutative: true,
                ..
            }
        )
    }

    /// Match on op *identity* only (dialect + name); commutativity and cost are
    /// derived properties, not part of what a searcher pattern selects.
    fn matches_pattern(&self, template: &Self, _ctx: &Context) -> bool {
        match (self, template) {
            (
                Term::Op {
                    dialect: d1,
                    name: n1,
                    ..
                },
                Term::Op {
                    dialect: d2,
                    name: n2,
                    ..
                },
            ) => d1 == d2 && n1 == n2,
            (Term::Const, Term::Const) => true,
            (Term::Opaque, Term::Opaque) => true,
            _ => false,
        }
    }
}
