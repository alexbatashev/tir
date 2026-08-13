//! The laws of the state algebra the memory terms live in.
//!
//! They are definitional, not proved: the axiom prover is QF_BV and has no array
//! model to quantify a memory over, so a read and a write mean here what addition
//! means for `addi`. What keeps them honest is that they are narrow. Both read
//! the state operand the seeder threads, which is the whole of memory identity in
//! the term graph: a chain reaches an access only through the writes that
//! actually happened on it, so a law that fires has already been told the two
//! accesses alias exactly.
//!
//! * **S1, store-to-load forwarding.** `Load(a, n, m, Store(s, a, n, v))` is `v`.
//!   Address and byte count are one class on both sides, so the two accesses name
//!   the same extent of the same address. The two must also agree on an IR type:
//!   the vocabulary is bit-level, and a byte count alone would forward the float a
//!   slot was written with into the integer a reader spells it as.
//! * **S2, dead-store elimination.** `Store(a, n, v2, Store(s, a, n, v1))` leaves
//!   memory as `Store(s, a, n, v2)` does, so the inner store *is* the state `s` —
//!   provided nothing else observes it. That proviso is not algebra: a class is
//!   context-free, and two arms of a gate writing one value to one address are one
//!   term, so unioning a class an arm yields with the state before it would take
//!   that arm back before its write. The applier therefore fires only when the
//!   accesses reading the inner state are the overwriting store alone, and the
//!   seeder recorded no operation outside the term graph observing it.

use tir_symbolic::egraph::{EGraph, ENode, Id, Pattern, Rewrite, Rhs, Var};

use super::rules::{Rule, Sym, class_value_type, operand};
use crate::sem::{SemNode as Node, SymKind};
use crate::{Context, TypeId};

/// `Load(address, bytes, metadata, state)`.
const LOAD_ARITY: usize = 4;
const LOAD_STATE: usize = 3;
/// `Store(address, bytes, value, address_space, state)`.
const STORE_ARITY: usize = 5;
const STORE_VALUE: usize = 2;
const STORE_STATE: usize = 4;
const ADDRESS: usize = 0;
const BYTES: usize = 1;

/// S1: a load whose state a matching store left reads that store's value.
pub(crate) fn forward_load(context: Context) -> Rule {
    access_law(
        "store-to-load",
        SymKind::LoadMemory,
        LOAD_ARITY,
        move |eg, read, root| {
            let Some(ty) = loaded_type(eg, root, read) else {
                return;
            };
            let Some(value) = stored_value(&context, eg, read[LOAD_STATE], read, ty) else {
                return;
            };
            eg.union(root, value);
        },
    )
}

/// S2: a store the next one overwrites unobserved leaves the state it was handed.
pub(crate) fn eliminate_dead_store(exported: Vec<Id>) -> Rule {
    access_law(
        "dead-store",
        SymKind::StoreMemory,
        STORE_ARITY,
        move |eg, written, root| {
            let state = written[STORE_STATE];
            if exported.iter().any(|&class| eg.find(class) == state) {
                return;
            }
            if observers(eg, state) != [eg.find(root)] {
                return;
            }
            let Some(before) = overwritten_state(eg, state, written) else {
                return;
            };
            eg.union(state, before);
        },
    )
}

/// A law over every access of `kind`, handed the canonical classes of the term's
/// `arity` operands and the class the access itself stands for.
fn access_law(
    name: &'static str,
    kind: SymKind,
    arity: usize,
    fire: impl Fn(&mut EGraph<Node>, &[Id], Id) + Send + Sync + 'static,
) -> Rule {
    let mut lhs = Pattern::new();
    let args = (0..arity)
        .map(|index| lhs.var(Var::Symbol(index as Sym)))
        .collect();
    lhs.add(Node::sym_pattern(kind, args));
    Rewrite::new(
        name,
        lhs,
        Rhs::Apply(Box::new(move |eg, substitution, root| {
            let operands: Vec<Id> = (0..arity)
                .map(|index| eg.find(operand(substitution, index as Sym)))
                .collect();
            fire(eg, &operands, root);
        })),
    )
}

/// The state a store in `state` was handed, when it wrote the extent `written`
/// names.
fn overwritten_state(eg: &EGraph<Node>, state: Id, written: &[Id]) -> Option<Id> {
    eg.nodes(state)
        .iter()
        .filter(|node| node.sym() == Some(SymKind::StoreMemory))
        .find_map(|node| {
            let dead = canonical(eg, node);
            (dead[ADDRESS] == written[ADDRESS] && dead[BYTES] == written[BYTES])
                .then_some(dead[STORE_STATE])
        })
}

/// The classes of the accesses reading `state`, in the graph's own order.
fn observers(eg: &EGraph<Node>, state: Id) -> Vec<Id> {
    let mut classes = Vec::new();
    for (kind, slot) in [
        (SymKind::LoadMemory, LOAD_STATE),
        (SymKind::StoreMemory, STORE_STATE),
    ] {
        let key = Node::sym_pattern(kind, Vec::new()).op_key();
        for class in eg.classes_with_op(key) {
            let reads = eg
                .nodes(class)
                .iter()
                .any(|node| node.sym() == Some(kind) && eg.find(node.children[slot]) == state);
            if reads {
                classes.push(class);
            }
        }
    }
    classes
}

/// The type the load standing for `read` in `class` yields.
fn loaded_type(eg: &EGraph<Node>, class: Id, read: &[Id]) -> Option<TypeId> {
    eg.nodes(class)
        .iter()
        .find(|node| node.sym() == Some(SymKind::LoadMemory) && canonical(eg, node) == read)
        .and_then(|node| node.ty)
}

/// The class a store in `state` left at the extent `read` names, when it holds a
/// value of type `ty`.
fn stored_value(
    context: &Context,
    eg: &EGraph<Node>,
    state: Id,
    read: &[Id],
    ty: TypeId,
) -> Option<Id> {
    eg.nodes(state)
        .iter()
        .filter(|node| node.sym() == Some(SymKind::StoreMemory))
        .find_map(|node| {
            let written = canonical(eg, node);
            let value = written[STORE_VALUE];
            (written[ADDRESS] == read[ADDRESS]
                && written[BYTES] == read[BYTES]
                && class_value_type(context, eg, value) == Some(ty))
            .then_some(value)
        })
}

fn canonical(eg: &EGraph<Node>, node: &Node) -> Vec<Id> {
    node.children.iter().map(|&child| eg.find(child)).collect()
}
