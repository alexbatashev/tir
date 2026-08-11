//! The laws of the state algebra the view's memory terms live in.
//!
//! # Why these are definitional, not proved
//!
//! Every other equality the view saturates with is an *obligation*: an axiom of
//! [`crate::sem::theory`] states a bit-vector identity and the [`SmtOracle`]
//! discharges it by bit-blasting `lhs != rhs` to unsat. That pipeline is QF_BV
//! and nothing more — [`tir_symbolic::bitblast`] reports `LoadMemory` and
//! `StoreMemory` as unsupported kinds, and the reference interpreter's memory
//! ([`tir_symbolic::lang::exec::Memory`]) is a concrete byte array, not an array
//! model a solver could quantify over. There is no oracle here to ask.
//!
//! So they are stated as what they are: the *meaning* of a read and a write over
//! the state algebra, in the sense that `Add` means addition. A store takes a
//! state to the state that differs from it at one address; a load of that
//! address on that state is the value written. Nothing about the axiom prover is
//! weakened to accommodate that, and no obligation is asserted that the oracle
//! did not discharge — the alternative, restructuring the laws as miters an
//! array-free bit-blaster could chew on, is the shape of proof this project has
//! already found intractable and forbidden.
//!
//! What keeps them honest is that they are *narrow*. Both read the state operand
//! the view seeds ([`super::view::Builder::seed_memory`]), which is the whole of
//! memory identity there: a chain reaches a load only through the writes that
//! actually happened on it, so a law that fires has already been told the two
//! accesses alias exactly.
//!
//! # The laws
//!
//! * **S1, load-over-store forwarding.** `Load(Store(s, a, n, v), a, n)` is `v`,
//!   where the two read the same IR type. Address and byte count are one class on
//!   both sides, so the two accesses are the same extent of the same address; the
//!   store proves that extent dereferenceable, so dropping the load is a no-op
//!   and not a new fault. The type agreement is the other half of "same access":
//!   the vocabulary is bit-level and a byte count alone would forward an integer
//!   a slot was written with into the float a reader spells it as, which is a
//!   value the region cannot hold.
//!   Congruence supplies the other half — two loads that agree on address and
//!   chain hash-cons, so the view never even spells them apart.
//! * **S2, dead-store elimination.** `Store(Store(s, a, n, v1), a, n, v2)` leaves
//!   memory as `Store(s, a, n, v2)` does, so the inner store is the state `s`
//!   *provided nothing else reads it*. That proviso is not algebra: an e-class is
//!   context-free, and unioning the inner state with `s` where a load also reads
//!   it would take that load back before the write. So the applier checks it —
//!   the inner class must be read by the outer store alone, and must not be a
//!   state the region exports.
//! * **S3, disjoint-chain commutation, needs no law.** [`super::raise`] threads
//!   one chain per non-escaping slot, so a store to one slot never appears under
//!   a load of another: the terms are already unrelated and S1 reaches through
//!   what is not there. It is asserted by test, not stated as a rewrite.
//!
//! [`SmtOracle`]: crate::sem::SmtOracle

use std::collections::HashSet;

use tir_symbolic::egraph::{EMatch, ENode, Id, Pattern, Var};

use crate::sem::egraph::SemEGraph;
use crate::sem::rewrites::IselRewrite;
use crate::sem::{SemNode, SymKind, template_node};
use crate::{Context, TypeId};

/// The state laws, over a view whose region exports `exported` as its outgoing
/// chains. A law that would drop a write needs to know which states leave the
/// region, and only the view knows that.
pub(super) fn state_laws(exported: Vec<Id>) -> Vec<IselRewrite> {
    vec![forward_load_over_store(), eliminate_dead_store(exported)]
}

/// The child index of the state operand: last on both shapes.
fn state_of(node: &SemNode) -> Id {
    *node.children.last().expect("a memory term carries a state")
}

/// `Load(Store(s, a, n, v), a, n) = v` (S1).
fn forward_load_over_store() -> IselRewrite {
    let mut searcher = Pattern::<SemNode, u32>::new();
    let address = searcher.var(Var::Symbol(0));
    let bytes = searcher.var(Var::Symbol(1));
    let metadata = searcher.var(Var::Symbol(2));
    let value = searcher.var(Var::Symbol(3));
    let space = searcher.var(Var::Symbol(4));
    let state = searcher.var(Var::Symbol(5));
    let store = node(
        SymKind::StoreMemory,
        vec![address, bytes, value, space, state],
    );
    let store = searcher.add(store);
    let load = node(SymKind::LoadMemory, vec![address, bytes, metadata, store]);
    let root = searcher.add(load);
    searcher.set_root(root);

    IselRewrite {
        name: "state-load-over-store".into(),
        searcher,
        post_saturation: false,
        apply: Box::new(move |_: &Context, eg: &mut SemEGraph, m: &EMatch<u32>| {
            let stored = m.binding(value);
            if !reads_alike(eg, m.root, stored) {
                return;
            }
            eg.union(m.root, stored);
        }),
    }
}

/// `Store(Store(s, a, n, v1), a, n, v2)` leaves the inner store's state equal to
/// `s`, where nothing but the outer store reads it (S2).
fn eliminate_dead_store(exported: Vec<Id>) -> IselRewrite {
    let mut searcher = Pattern::<SemNode, u32>::new();
    let address = searcher.var(Var::Symbol(0));
    let bytes = searcher.var(Var::Symbol(1));
    let overwritten = searcher.var(Var::Symbol(2));
    let space = searcher.var(Var::Symbol(3));
    let state = searcher.var(Var::Symbol(4));
    let value = searcher.var(Var::Symbol(5));
    let outer_space = searcher.var(Var::Symbol(6));
    let inner = node(
        SymKind::StoreMemory,
        vec![address, bytes, overwritten, space, state],
    );
    let inner = searcher.add(inner);
    let outer = node(
        SymKind::StoreMemory,
        vec![address, bytes, value, outer_space, inner],
    );
    let root = searcher.add(outer);
    searcher.set_root(root);

    IselRewrite {
        name: "state-dead-store".into(),
        searcher,
        post_saturation: true,
        apply: Box::new(move |_: &Context, eg: &mut SemEGraph, m: &EMatch<u32>| {
            let dead = eg.find(m.binding(inner));
            let before = m.binding(state);
            if exported.iter().any(|&state| eg.find(state) == dead) {
                return;
            }
            if readers(eg, dead) != 1 {
                return;
            }
            eg.union(dead, before);
        }),
    }
}

/// Whether the two classes name a value of the same IR type. A class carries a
/// type on whichever members were seeded with one; the loaded and the stored
/// value must agree on one of them for the load to *be* the store's value rather
/// than a reinterpretation of its bits.
fn reads_alike(eg: &SemEGraph, loaded: Id, stored: Id) -> bool {
    let types =
        |class| -> HashSet<TypeId> { eg.nodes(class).iter().filter_map(|n| n.ty).collect() };
    !types(loaded).is_disjoint(&types(stored))
}

/// How many terms of the whole graph read `state` as their state operand. A
/// write whose state nothing else reads is one the schedule can forget.
fn readers(eg: &SemEGraph, state: Id) -> usize {
    let mut seen = HashSet::new();
    for class in eg.classes() {
        for term in eg.nodes(class.id()) {
            let reads = match term.kind.sym() {
                Some(SymKind::LoadMemory | SymKind::StoreMemory) => {
                    eg.find(state_of(term)) == state
                }
                _ => term.children().iter().any(|&child| eg.find(child) == state),
            };
            if reads {
                seen.insert(eg.find(class.id()));
            }
        }
    }
    seen.len()
}

fn node(kind: SymKind, children: Vec<Id>) -> SemNode {
    let mut node = template_node(kind, None, None);
    node.children = children;
    node
}
