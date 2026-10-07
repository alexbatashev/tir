//! The query IR: atoms over a database's tables and lattice columns, guards
//! over the words a match binds, and the host a search asks for what only the
//! client knows.

use smallvec::SmallVec;

use crate::Ref;
use crate::db::{LatticeId, SpaceId};

/// A variable of a [`Query`], numbered from zero. It binds a reference: an id
/// and an offset. A variable bound twice must bind the same reference both
/// times.
pub type Var = u32;

/// A scalar variable: one word, read off a row or computed by a guard.
pub type Scalar = u32;

/// A set of cells the host resolves per database: which cells a selected
/// column may hold, and in which tables. Shared by a query and its nots.
pub type Param = u32;

/// One of the host's functions over words.
pub type ExternId = u32;

/// An integer expression over bound scalars.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Expr {
    Lit(i64),
    Scalar(Scalar),
    Sub(Box<Expr>, Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    /// `2^e - 1`, the all-ones value of `e` bits.
    Ones(Box<Expr>),
    /// Bitwise conjunction, for masking a literal to the width the class spells
    /// it at.
    And(Box<Expr>, Box<Expr>),
    /// One when the operand is zero, zero otherwise — a comparison's negation,
    /// which is what a rule proving the complement of a settled comparison
    /// needs to spell.
    IsZero(Box<Expr>),
}

impl Expr {
    pub(crate) fn eval(&self, scalars: &[u64]) -> Option<i64> {
        Some(match self {
            Expr::Lit(value) => *value,
            Expr::Scalar(slot) => scalars[*slot as usize] as i64,
            Expr::Sub(a, b) => a.eval(scalars)?.checked_sub(b.eval(scalars)?)?,
            Expr::Add(a, b) => a.eval(scalars)?.checked_add(b.eval(scalars)?)?,
            Expr::Ones(e) => match e.eval(scalars)? {
                64 => u64::MAX as i64,
                bits @ 0..64 => ((1u64 << bits) - 1) as i64,
                _ => return None,
            },
            Expr::And(a, b) => a.eval(scalars)? & b.eval(scalars)?,
            Expr::IsZero(e) => i64::from(e.eval(scalars)? == 0),
        })
    }

    /// Whether every scalar the expression reads is `known`.
    fn ready(&self, known: &[bool]) -> bool {
        match self {
            Expr::Lit(_) => true,
            Expr::Scalar(slot) => known[*slot as usize],
            Expr::Sub(a, b) | Expr::Add(a, b) | Expr::And(a, b) => a.ready(known) && b.ready(known),
            Expr::Ones(e) | Expr::IsZero(e) => e.ready(known),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ne,
}

/// A conjunctive query. Every atom's key variable is reachable from
/// [`Self::root`] through the variables earlier atoms bind, which is what
/// lets a plan bind them in one pass.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    /// The id space every variable ranges over.
    pub space: SpaceId,
    pub vars: u32,
    pub scalars: u32,
    pub params: u32,
    pub root: Var,
    pub atoms: Vec<Atom>,
    pub guards: Vec<Guard>,
    /// Sub-conjunctions that must have no solution. Placed once everything they
    /// read is bound.
    pub nots: Vec<Nested>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Atom {
    Rows(Rows),
    Lattice(Lattice),
    Place(Place),
}

impl Atom {
    /// The variable this atom is looked up under: a row atom's value, a
    /// lattice or place atom's key.
    pub fn key(&self) -> Var {
        match self {
            Atom::Rows(rows) => rows.value(),
            Atom::Lattice(lattice) => lattice.key,
            Atom::Place(place) => place.key,
        }
    }

    /// The scalar this atom binds.
    pub(crate) fn writes(&self) -> Option<Scalar> {
        match self {
            Atom::Rows(rows) => rows.tag,
            Atom::Lattice(lattice) => match lattice.out {
                Out::Word(slot) => Some(slot),
                Out::In(_) | Out::Host(_) => None,
            },
            Atom::Place(place) => match place.read {
                Read::Offset { offset, .. } | Read::Pinned { offset } => Some(offset),
                Read::Width { width } => Some(width),
                Read::Conflicted => None,
            },
        }
    }
}

/// Rows of the tables `select.param` names. In each table, column
/// `select.column` must hold one of the cells listed for that table. A cell
/// binds its variable to the reference it and its weight are.
///
/// The variable a search starts at, before an atom fixes its offset, stands
/// for its id at whatever offset the row it meets is at, so a search from an
/// id meets every row whose value is in its set.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rows {
    pub select: Select,
    /// One per column, the value column last, which must be a variable.
    /// `terms[select.column]` is [`Term::Any`].
    pub terms: SmallVec<[Term; 6]>,
    /// Binds the row's tag.
    pub tag: Option<Scalar>,
    pub swap: Option<Swap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Select {
    pub column: u8,
    pub param: Param,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Term {
    Any,
    Var(Var),
}

/// The atom also matches a row with columns `columns[0]` and `columns[1]`
/// exchanged, if the row's selected cell is among `when`'s cells for its
/// table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Swap {
    pub columns: [u8; 2],
    pub when: Param,
}

impl Rows {
    /// The variable the value column binds.
    pub fn value(&self) -> Var {
        match self.terms.last() {
            Some(Term::Var(var)) => *var,
            _ => panic!("a row atom binds its value column"),
        }
    }

    /// The key columns holding a variable, in column order: the atom's
    /// arguments, which a sideways step numbers.
    pub(crate) fn args(&self) -> impl Iterator<Item = (usize, Var)> + '_ {
        let keys = &self.terms[..self.terms.len() - 1];
        keys.iter()
            .enumerate()
            .filter_map(|(column, term)| match term {
                Term::Var(var) => Some((column, *var)),
                Term::Any => None,
            })
    }

    /// The column whose cell sits in `column` when the row is read swapped.
    pub(crate) fn swapped(&self, column: usize) -> usize {
        match self.swap {
            Some(Swap {
                columns: [a, b], ..
            }) if column == a as usize => b as usize,
            Some(Swap {
                columns: [a, b], ..
            }) if column == b as usize => a as usize,
            _ => column,
        }
    }
}

/// The value of `key`'s id in a lattice column.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lattice {
    pub column: LatticeId,
    pub key: Var,
    pub out: Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Out {
    /// The value is written to the scalar.
    Word(Scalar),
    /// The value must be one of the parameter's cells.
    In(Param),
    /// The value is not read, and need not exist: [`Host::accepts`] decides
    /// from `key`'s reference. The column still says which epoch the atom is
    /// from.
    Host(ExternId),
}

/// What the id space knows of `key`'s reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Place {
    pub key: Var,
    pub read: Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Read {
    /// `key` is `base`, its id at offset zero, plus the scalar `offset`.
    Offset { base: Var, offset: Scalar },
    /// `key`'s id is pinned: bind the offset `key` sits at from it, which is
    /// the constant `key` is.
    Pinned { offset: Scalar },
    /// `key`'s id has a carrier: bind its width in bits.
    Width { width: Scalar },
    /// An equate tried to put `key`'s id at a non-zero offset from itself: it
    /// is placed two ways at once, so nowhere.
    Conflicted,
}

/// A predicate over bound scalars.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Guard {
    Cmp(Cmp, Expr, Expr),
    /// Bind `out` to what `value` evaluates to.
    Let {
        out: Scalar,
        value: Expr,
    },
    /// The pairs are not all bound to the same reference. One pair is plain
    /// disequality; several say "these two terms are not the same term".
    Distinct(SmallVec<[(Var, Var); 4]>),
    /// A host function over the words `args` evaluate to; it binds `out`, and
    /// failing fails the match.
    Extern {
        call: ExternId,
        args: SmallVec<[Expr; 4]>,
        out: SmallVec<[Scalar; 2]>,
    },
}

impl Guard {
    /// Whether every scalar the guard reads is `known` and every variable it
    /// reads (only a disequality has any) is `bound`.
    pub(crate) fn ready(&self, known: &[bool], bound: &[bool]) -> bool {
        match self {
            Guard::Cmp(_, a, b) => a.ready(known) && b.ready(known),
            Guard::Let { value, .. } => value.ready(known),
            Guard::Distinct(pairs) => pairs
                .iter()
                .all(|&(a, b)| bound[a as usize] && bound[b as usize]),
            Guard::Extern { args, .. } => args.iter().all(|arg| arg.ready(known)),
        }
    }

    #[inline]
    pub(crate) fn writes(&self) -> &[Scalar] {
        match self {
            Guard::Cmp(..) | Guard::Distinct(..) => &[],
            Guard::Let { out, .. } => std::slice::from_ref(out),
            Guard::Extern { out, .. } => out,
        }
    }
}

/// A negated sub-conjunction: no solution of it exists, given what the outer
/// match bound.
///
/// Evaluated against the whole database, never a delta. Rows and lattice
/// values only accumulate and ids only merge, so the conjunction can only go
/// from unsatisfied to satisfied over epochs: a match blocked once stays
/// blocked and needs no re-check, and a match that fired cannot be un-fired.
/// An id placed consistently in one epoch can be [`Read::Conflicted`] in the
/// next without anything in the match moving at all.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Nested {
    pub atoms: Vec<Atom>,
    pub guards: Vec<Guard>,
}

/// What a parameter resolves to: for a row atom's selection, tables in the
/// order first met and each table's cells ascending; for a lattice atom's
/// [`Out::In`], cells ascending. Resolutions only grow.
#[derive(Clone, Debug, Default)]
pub struct Resolution {
    pub tables: SmallVec<[(u32, SmallVec<[u32; 4]>); 2]>,
    pub cells: SmallVec<[u32; 4]>,
}

impl Resolution {
    /// The cells listed for `table`.
    pub(crate) fn cells_of(&self, table: u32) -> &[u32] {
        let found = self.tables.iter().find(|(known, _)| *known == table);
        found.map_or(&[], |(_, cells)| cells.as_slice())
    }
}

/// What a search asks its client.
pub trait Host {
    /// Grows whenever some parameter's resolution may have grown.
    fn version(&self) -> u64;
    /// Extend `out` by what `param` resolves to among what appeared since
    /// version `since` (`None`: from scratch, into an empty `out`). Appending
    /// must keep the order a from-scratch resolution would give.
    fn resolve(&self, param: Param, since: Option<u64>, out: &mut Resolution);
    /// A guard's host function over words. Failing fails the match.
    fn call(&self, id: ExternId, args: &[u64], out: &mut [u64]) -> bool;
    /// Whether an [`Out::Host`] atom holds of `reference`, which is canonical.
    fn accepts(&self, _id: ExternId, _reference: Ref) -> bool {
        false
    }
    /// Whether `param` resolves to nothing, when the host can tell without
    /// resolving it: a plan reading no table is settled before it gets a
    /// cache.
    fn empty(&self, _param: Param) -> bool {
        false
    }
}
