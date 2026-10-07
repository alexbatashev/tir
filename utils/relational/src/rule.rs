//! Rule heads: what a match writes. A head only adds — a row, an equality —
//! so a match that fired cannot be un-fired, and a head reads nothing beyond
//! the key lookup an insert is. A match exists in epoch `t` and not `t-1`
//! exactly when one of its rows or values is from epoch `t-1`, which is what
//! lets a semi-naive driver search only the delta.

use smallvec::SmallVec;

use crate::Ref;
use crate::db::{Database, SpaceId};
use crate::query::{Expr, Plan, Scalar, Var, bound};

/// One write of a rule's head. Variables hold references.
#[derive(Clone, Debug)]
pub enum Head {
    /// Bind `into` to the value of the row the client keys by `args`. If no
    /// row has the key, the client inserts one with a fresh value id.
    Insert {
        args: SmallVec<[Var; 4]>,
        /// Whether the arguments are sorted, once canonical, before the
        /// client keys them.
        sorted: bool,
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
    /// Bind `into` to `base` plus what `offset` evaluates to.
    Offset {
        base: Var,
        offset: Expr,
        into: Var,
    },
}

/// The key of a row: its cells, and beside them the weights of the cells
/// whose offsets stay in the key.
#[derive(Default)]
pub struct Key {
    pub cells: SmallVec<[u32; 8]>,
    pub weights: SmallVec<[u64; 8]>,
    /// Whether some weight is non-zero.
    pub weighted: bool,
}

impl Key {
    /// The weights as a table takes them: none when every one is zero.
    pub fn weights(&self) -> &[u64] {
        match self.weighted {
            true => &self.weights,
            false => &[],
        }
    }
}

/// What an insert comes to before its table is probed.
pub enum Keyed {
    /// The client settles it without a row.
    Is(Ref),
    /// The key is built: the table its row lives in, and what the row's value
    /// is shifted by.
    At { table: u32, shift: u64 },
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
    /// What insert `op` of rule `rule` over `operands`, which are canonical,
    /// comes to given the match's scalars: a reference, or the key its row is
    /// stored under, written into `key`. `None`: the client cannot spell it,
    /// and the head stops.
    fn key(
        &mut self,
        rule: usize,
        op: usize,
        operands: &[Ref],
        scalars: &[u64],
        key: &mut Key,
    ) -> Option<Keyed>;
    /// Insert `key` into `table` with a fresh value id, plus whatever the
    /// client keeps per row, returning the id.
    fn mint(&mut self, rule: usize, op: usize, table: u32, key: &Key, scalars: &[u64]) -> u32;
    /// Equate two references, plus whatever the client keeps per id.
    fn equate(&mut self, a: Ref, b: Ref);
}

/// Run `head` on one match of rule `rule`, whose variables range over
/// `space`. `held` is scratch for the match's bindings and the head's own.
/// A missing binding or an insert the client cannot spell stops the rest of
/// the head; everything before it stands, which is sound because a head only
/// adds.
#[allow(clippy::too_many_arguments)]
pub fn apply<C, const S: usize>(
    client: &mut impl Client<C, S>,
    space: SpaceId,
    rule: usize,
    head: &[Head],
    head_vars: u32,
    bindings: &[Ref],
    scalars: &[u64],
    held: &mut Vec<Ref>,
) {
    held.clear();
    held.extend_from_slice(bindings);
    held.resize(bindings.len() + head_vars as usize, crate::query::UNBOUND);
    let get = |held: &[Ref], var: usize| bound(*held.get(var)?);
    for (op, write) in head.iter().enumerate() {
        match write {
            Head::Insert { args, sorted, into } => {
                let Some(mut operands) = args
                    .iter()
                    .map(|&arg| get(held, arg as usize))
                    .collect::<Option<SmallVec<[Ref; 4]>>>()
                else {
                    return;
                };
                let Some(value) =
                    insert(client, space, (rule, op), &mut operands, *sorted, scalars)
                else {
                    return;
                };
                held[*into as usize] = value;
            }
            Head::Equate(a, b) => {
                let (Some(a), Some(b)) = (get(held, *a as usize), get(held, *b as usize)) else {
                    return;
                };
                client.equate(a, b);
            }
            Head::EquateIndexed { var, offset, index } => {
                let chosen = *offset as usize + scalars[*index as usize] as usize;
                let (Some(a), Some(b)) = (get(held, *var as usize), get(held, chosen)) else {
                    return;
                };
                client.equate(a, b);
            }
            Head::Offset { base, offset, into } => {
                let (Some(base), Some(offset)) = (get(held, *base as usize), offset.eval(scalars))
                else {
                    return;
                };
                let shifted = Ref::new(base.id, base.offset.wrapping_add(offset as u64));
                held[*into as usize] = client.db().find(space, shifted);
            }
        }
    }
}

/// The value of the row an insert's client keys by `operands`, minted if no
/// row has the key.
fn insert<C, const S: usize>(
    client: &mut impl Client<C, S>,
    space: SpaceId,
    (rule, op): (usize, usize),
    operands: &mut [Ref],
    sorted: bool,
    scalars: &[u64],
) -> Option<Ref> {
    for operand in operands.iter_mut() {
        *operand = client.db().find(space, *operand);
    }
    if sorted {
        operands.sort_unstable();
    }
    let mut key = Key::default();
    let (table, shift) = match client.key(rule, op, operands, scalars, &mut key)? {
        Keyed::Is(found) => return Some(found),
        Keyed::At { table, shift } => (table, shift),
    };
    let db = client.db();
    if let Some(found) = db.table(table)
        && let Some(row) = found.get(&key.cells, key.weights())
    {
        let weight = found.weight(found.arity(), row);
        let value = Ref::new(found.values()[row as usize], weight.wrapping_add(shift));
        return Some(db.find(space, value));
    }
    Some(Ref::new(client.mint(rule, op, table, &key, scalars), shift))
}
