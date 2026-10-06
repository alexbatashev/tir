//! A relational engine: tables of ids, the equalities over them, and the
//! queries and fixpoints a compiler runs over both.
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
//! [`query`] is conjunctive queries over a database, with an evaluator that
//! answers by loops over the tables; [`rule`] is what a match writes;
//! [`saturate`] is the semi-naive fixpoint over a rule set; [`extract`] is the
//! least-cost fold a client reads a solution off with. They name tables,
//! columns and cells, and ask their client for whatever means more than that.
//!
//! The e-graph is one such client and lives in its own crate, `tir-egraph`.

mod column;
mod csr;
mod db;
pub mod extract;
pub mod query;
pub mod rule;
pub mod saturate;
pub mod store;
mod telemetry;
mod unionfind;

pub use column::{Column, Fact, Join};
pub use csr::Csr;
pub use db::{Database, Groups, LatticeId, Marks, SpaceId, Stats};
pub use query::{Cmp, Expr, Scalar, Step, Var};
pub use telemetry::{RoundStats, Timer, report_saturation};

pub(crate) type FxHashMap<K, V> = std::collections::HashMap<K, V, tir_adt::FxBuildHasher>;

/// Whether `TIR_SAT_TRACE` asked for a saturation trace on stderr: every id
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
