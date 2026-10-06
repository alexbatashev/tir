//! The e-graph, a client of the [`Database`](crate::Database). Classes are one
//! id space; each label is a table from child classes to the class the node
//! belongs to, so hash-consing is a key lookup, congruence is the table's
//! functional dependency, and a rewrite's left-hand side is a join. Rewrites
//! are conjunctive queries compiled onto the database's query IR; saturation
//! is the database's semi-naive fixpoint over them.

mod engine;
mod extract;
mod label;
mod query;
mod rule;
mod saturate;

#[cfg(test)]
mod testing;

pub use engine::{ClassRef, Engine, Rows};
pub use extract::Extraction;
pub use label::Label;
pub use query::{
    Atom, ColumnId, Externs, Field, Guard, Match, Nested, NoExterns, Plan, Query, Source,
};
pub use rule::{HeadOp, LabelFill, Rule};

/// An e-class. Only [`Engine::find`] turns one into its canonical form.
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
