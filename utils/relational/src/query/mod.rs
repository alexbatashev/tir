//! Conjunctive queries over a [`Database`](crate::Database): atoms over its
//! tables and lattice columns, guards over the words a match binds, and an
//! evaluator that answers with loops over the tables rather than a walk over a
//! goal stack.
//!
//! A [`Query`] is the IR; a [`Plan`] is that query with an evaluation order
//! chosen once. The order a plan produces matches in is part of the contract:
//! a client applies heads in match order, so the ids a saturation mints depend
//! on it.
//!
//! Where consecutive row atoms share a variable none of them has bound yet,
//! the evaluator joins them over tries of their tables, intersecting the
//! cells each atom allows for that variable, and then emits the matches in
//! the order the loop nest would have found them.

mod eval;
mod ir;
mod join;
mod plan;
mod set;
pub(crate) mod trie;

pub(crate) use eval::Unchecked;
pub use eval::{Allow, Matches, PlanCache};
pub use ir::{
    Atom, Cmp, Expr, ExternId, Guard, Host, Lattice, Nested, Out, Param, Query, Resolution, Rows,
    Scalar, Select, Swap, Term, Var,
};
pub use plan::{Plan, Step};
