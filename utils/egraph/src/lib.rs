//! The e-graph, a client of the [`tir_relational`] engine that uses only its
//! public API. Classes are one id space of the [`Database`], and a value is a
//! [`Ref`]: a class plus a constant offset, kept by the database's union-find,
//! which places each class at an offset from its representative. Each label is
//! a table from operands to the reference the node is, with the offsets the
//! label's laws move to the result factored out of the key, so hash-consing is
//! a key lookup, congruence is the table's functional dependency, and a
//! rewrite's left-hand side is a join. Rewrites are conjunctive queries compiled
//! onto the database's query IR; saturation is the database's semi-naive
//! fixpoint over them, and extraction its least-cost fold.
//!
//! [`Engine`] is the e-graph, [`Query`] and [`Rule`] the rule front end, and
//! [`Extraction`] the cheapest node per class.
//!
//! [`Database`]: tir_relational::Database

mod engine;
mod extract;
mod label;
mod query;
mod rule;
mod saturate;

#[cfg(test)]
mod testing;

pub use engine::{ClassRef, Contradiction, Engine, Rows};
pub use extract::Extraction;
pub use label::{Carrier, Label};
pub use query::{
    Atom, ColumnId, Externs, Field, Guard, Match, Nested, NoExterns, Plan, Query, Source,
};
pub use rule::{HeadOp, LabelFill, Rule};

type FxHashMap<K, V> = std::collections::HashMap<K, V, tir_adt::FxBuildHasher>;

/// An e-class. Only [`Engine::root`] turns one into its canonical form.
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

/// A value: a class plus a constant, read as "the class's value plus
/// `offset`" modulo 2^W, W being the width of the class's carrier. A class
/// whose label declares no carrier is only ever referred to at offset zero.
/// Only [`Engine::find`] turns one into its canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    pub class: ClassId,
    pub offset: u64,
}

/// Most references sit at offset zero, and those hash as their class alone:
/// a hashed set of references costs what one of classes does.
impl std::hash::Hash for Ref {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.class.hash(state);
        if self.offset != 0 {
            self.offset.hash(state);
        }
    }
}

impl Ref {
    pub fn new(class: ClassId, offset: u64) -> Self {
        Self { class, offset }
    }
}

impl From<ClassId> for Ref {
    fn from(class: ClassId) -> Self {
        Self { class, offset: 0 }
    }
}

impl From<tir_relational::Ref> for Ref {
    fn from(reference: tir_relational::Ref) -> Self {
        Self::new(ClassId(reference.id), reference.offset)
    }
}

impl From<Ref> for tir_relational::Ref {
    fn from(reference: Ref) -> Self {
        Self::new(reference.class.0, reference.offset)
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
