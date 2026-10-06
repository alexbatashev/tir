//! Conjunctive queries over a [`Database`](crate::Database): atoms over its
//! tables and lattice columns, guards over the words a match binds, and an
//! evaluator that answers with loops over the tables rather than a walk over a
//! goal stack.
//!
//! A [`Query`] is the IR; a [`Plan`] is that query with an evaluation order
//! chosen once. The order a plan produces matches in is part of the contract:
//! a client applies heads in match order, so the ids a saturation mints depend
//! on it.

mod eval;
mod ir;
mod plan;

pub(crate) use eval::Unchecked;
pub use eval::{Allow, Matches, PlanCache};
pub use ir::{
    Atom, Cmp, Expr, ExternId, Guard, Host, Lattice, Nested, Out, Param, Query, Resolution, Rows,
    Scalar, Select, Swap, Term, Var,
};
pub use plan::{Plan, Step};
