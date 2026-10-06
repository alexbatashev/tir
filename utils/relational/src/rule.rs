//! Rule heads: what a match writes. A head only adds — a row, an equality, a
//! raised lattice value — so a match that fired cannot be un-fired, and a head
//! reads nothing beyond the key lookup an insert is. A match exists in epoch
//! `t` and not `t-1` exactly when one of its rows or values is from epoch
//! `t-1`, which is what lets a semi-naive driver search only the delta.

use smallvec::SmallVec;

use crate::db::{Database, LatticeId, SpaceId};
use crate::query::{Expr, Plan, Scalar, Var};

/// One write of a rule's head.
#[derive(Clone, Debug)]
pub enum Head {
    /// Bind `into` to the value of the row of `table` whose key is `key`. If no
    /// row has it, the client inserts one with a fresh value id.
    Insert {
        table: u32,
        key: SmallVec<[Key; 6]>,
        order: KeyOrder,
        into: Var,
    },
    Equate(Var, Var),
    /// Equate `var` with variable `offset + scalars[index]`: the one head that
    /// chooses among bound variables, for a rule that picks an argument by a
    /// guard's answer.
    EquateIndexed {
        var: Var,
        offset: Var,
        index: Scalar,
    },
    /// Raise `key`'s value in `column` to what `value` evaluates to, one word
    /// per value.
    Raise {
        column: LatticeId,
        key: Var,
        value: SmallVec<[Value; 2]>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Var(Var),
    /// The cell [`Client::cell`] gives this head op.
    Client,
}

/// How an insert's variable cells are put in order before the lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOrder {
    AsWritten,
    /// Canonicalize the cells, then sort them.
    Sorted,
    /// Sort the cells as bound, then canonicalize them.
    SortedRaw,
}

#[derive(Clone, Debug)]
pub enum Value {
    Var(Var),
    Expr(Expr),
}

/// A rule as a driver runs it: what to look for. What it proves, its head,
/// the driver asks the client for at the rule's first match.
#[derive(Clone, Copy)]
pub struct Rule<'a> {
    pub name: &'a str,
    pub plan: &'a Plan,
    /// Variables the head binds, numbered above the query's.
    pub head_vars: u32,
    /// Applied once after the fixpoint rather than in it, for a rule whose
    /// head would feed back through the others.
    pub post_saturation: bool,
}

/// What a head asks its client: the database, and the parts of a write only
/// the client can do.
pub trait Client<C, const S: usize> {
    fn db(&self) -> &Database<C, S>;
    fn db_mut(&mut self) -> &mut Database<C, S>;
    /// The cell [`Key::Client`] of head op `op` of rule `rule` stands for,
    /// given the match's scalars. `None`: the client cannot spell it, and the
    /// head stops.
    fn cell(&mut self, rule: usize, op: usize, scalars: &[u64]) -> Option<u32>;
    /// Insert `key` into `table` with a fresh value id, plus whatever the
    /// client keeps per row, returning the id.
    fn mint(&mut self, rule: usize, op: usize, table: u32, key: &[u32], scalars: &[u64]) -> u32;
    /// Equate two ids, plus whatever the client keeps per id.
    fn equate(&mut self, a: u32, b: u32);
}

/// Run `head` on one match of rule `rule`, whose variables range over
/// `space`. `bound` is scratch for the match's bindings and the head's own.
/// A missing binding or a cell the client cannot spell stops the rest of the
/// head; everything before it stands, which is sound because a head only adds.
#[allow(clippy::too_many_arguments)]
pub fn apply<C, const S: usize>(
    client: &mut impl Client<C, S>,
    space: SpaceId,
    rule: usize,
    head: &[Head],
    head_vars: u32,
    bindings: &[Option<u32>],
    scalars: &[u64],
    bound: &mut Vec<Option<u32>>,
) {
    bound.clear();
    bound.extend_from_slice(bindings);
    bound.resize(bindings.len() + head_vars as usize, None);
    for (op, write) in head.iter().enumerate() {
        match write {
            Head::Insert {
                table,
                key,
                order,
                into,
            } => {
                let Some(id) = insert(
                    client,
                    space,
                    (rule, op),
                    *table,
                    key,
                    *order,
                    bound,
                    scalars,
                ) else {
                    return;
                };
                bound[*into as usize] = Some(id);
            }
            Head::Equate(a, b) => {
                let (Some(a), Some(b)) = (bound[*a as usize], bound[*b as usize]) else {
                    return;
                };
                client.equate(a, b);
            }
            Head::EquateIndexed { var, offset, index } => {
                let chosen = *offset as usize + scalars[*index as usize] as usize;
                let (Some(a), Some(Some(b))) = (bound[*var as usize], bound.get(chosen)) else {
                    return;
                };
                client.equate(a, *b);
            }
            Head::Raise { column, key, value } => {
                if raise(client.db_mut(), *column, *key, value, bound, scalars).is_none() {
                    return;
                }
            }
        }
    }
}

/// Raise `key`'s value in `column` to what `value` evaluates to. Kept out of
/// line: most heads insert and equate, and this one's words would crowd them.
#[inline(never)]
fn raise<C, const S: usize>(
    db: &mut Database<C, S>,
    column: LatticeId,
    key: Var,
    value: &[Value],
    bound: &[Option<u32>],
    scalars: &[u64],
) -> Option<()> {
    let words: SmallVec<[u64; 2]> = value
        .iter()
        .map(|value| match value {
            Value::Var(var) => bound[*var as usize].map(u64::from),
            Value::Expr(expr) => expr.eval(scalars).map(|word| word as u64),
        })
        .collect::<Option<_>>()?;
    db.raise_value(column, bound[key as usize]?, &words);
    Some(())
}

/// The value of `table`'s row at the key an insert spells, minted if no row
/// has it.
#[allow(clippy::too_many_arguments)]
fn insert<C, const S: usize>(
    client: &mut impl Client<C, S>,
    space: SpaceId,
    (rule, op): (usize, usize),
    table: u32,
    key: &[Key],
    order: KeyOrder,
    bound: &[Option<u32>],
    scalars: &[u64],
) -> Option<u32> {
    debug_assert!(
        key.iter().all(
            |key| matches!(key, Key::Var(var) if bound[*var as usize].is_some())
                || *key == Key::Client
        ),
        "a head reads only what its match and its earlier inserts bound"
    );
    // All but the widest keys fit on the stack.
    let mut inline = [0u32; 8];
    let mut spilled = Vec::new();
    let cells = match inline.get_mut(..key.len()) {
        Some(cells) => cells,
        None => {
            spilled.resize(key.len(), 0);
            &mut spilled[..]
        }
    };
    // In key order, stopping at a cell the client cannot spell. A variable's
    // cell is canonical but for the order that sorts the cells as bound.
    let raw = order == KeyOrder::SortedRaw;
    for (cell, key) in cells.iter_mut().zip(key) {
        *cell = match *key {
            Key::Client => client.cell(rule, op, scalars)?,
            Key::Var(var) if raw => bound[var as usize]?,
            Key::Var(var) => client.db().find(space, bound[var as usize]?),
        };
    }
    let db = client.db();
    if order != KeyOrder::AsWritten {
        sort(db, space, cells, key, order);
    }
    let cells = &*cells;
    if let Some(table) = db.table(table)
        && let Some(row) = table.get(cells)
    {
        return Some(db.find(space, table.values()[row as usize]));
    }
    Some(client.mint(rule, op, table, cells, scalars))
}

/// Sort the variables' cells of `cells` among themselves, then canonicalize
/// the raw ones. Kept out of line, like [`raise`].
#[inline(never)]
fn sort<C, const S: usize>(
    db: &Database<C, S>,
    space: SpaceId,
    cells: &mut [u32],
    key: &[Key],
    order: KeyOrder,
) {
    let mut sorted: SmallVec<[u32; 8]> = vars(cells, key).map(|cell| *cell).collect();
    sorted.sort_unstable();
    for (cell, id) in vars(cells, key).zip(sorted) {
        *cell = match order {
            KeyOrder::SortedRaw => db.find(space, id),
            _ => id,
        };
    }
}

/// The cells of `key`'s variables.
fn vars<'a>(cells: &'a mut [u32], key: &'a [Key]) -> impl Iterator<Item = &'a mut u32> {
    let cells = cells.iter_mut().zip(key);
    cells.filter_map(|(cell, key)| (*key != Key::Client).then_some(cell))
}
