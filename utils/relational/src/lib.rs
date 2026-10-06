//! The relational engine behind TIR's e-graph, in three layers.
//!
//! [`store`] holds tables of `u32` cells whose key columns determine a value
//! column, with the whole-column loops done by [`tir_adt::simd`]. A table holds
//! any relation and knows nothing about what its cells name.
//!
//! [`Database`] puts tables together with what their cells name: id spaces,
//! each partitioned by a union-find, whose equalities it pushes into the tables
//! on [`Database::canonicalize`], reporting the rows that came to share a key.
//! It also keeps lattice columns of what an id is known to be, the epoch
//! stamps and change log a semi-naive fixpoint reads, the indexes (rows grouped
//! by a column, ids to the rows naming them) built once per change, and
//! assumption scopes that snapshot all of it along with the client's own state.
//!
//! [`Engine`] is the e-graph, a client of the database. Classes are one id
//! space; each label is a table from child classes to the class the node
//! belongs to, so hash-consing is a key lookup, congruence is the table's
//! functional dependency, and a rewrite's left-hand side is a join. Rewrites
//! are conjunctive queries; saturation is the least fixpoint of the rule set.

mod column;
mod csr;
mod db;
mod engine;
mod extract;
mod label;
mod query;
mod rule;
mod saturate;
pub mod store;
mod telemetry;
mod unionfind;

#[cfg(test)]
mod testing;

pub use column::{Column, Fact, Join};
pub use csr::Csr;
pub use db::{Database, Groups, LatticeId, Marks, SpaceId};
pub use engine::{ClassRef, Engine, Rows, Stats};
pub use extract::Extraction;
pub use label::Label;
pub use query::{
    Atom, Cmp, ColumnId, Expr, Externs, Field, Guard, Match, Nested, NoExterns, Plan, Query,
    Scalar, Source, Step, Var,
};
pub use rule::{HeadOp, LabelFill, Rule};
pub use saturate::{Delta, round_roots};
pub use telemetry::{RoundStats, Timer, report_saturation};

/// Whether `TIR_SAT_TRACE` asked for a saturation trace on stderr: every class
/// minted (`A id node children`) and every merge (`U a b -> survivor`), plus
/// whatever the drivers add.
///
/// The trace is how an engine change is localized. Two compilers built from
/// different commits assign the same ids for as long as they agree, so the first
/// differing line of the diff is the coordinates of the divergence — the round,
/// the rule and the match that caused it — rather than an object file that
/// merely came out different.
pub fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TIR_SAT_TRACE").is_some_and(|value| value != "0"))
}

/// An e-class. Only [`UnionFind::find`] turns one into its canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClassId(pub u32);

impl ClassId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn from_raw(raw: u32) -> Self {
        ClassId(raw)
    }
}

/// A row of one relation: an e-node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(pub u32);

impl RowId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// An interned label: an e-node stripped of its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LabelId(pub u32);

impl LabelId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}
