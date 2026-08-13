//! The laws of the state algebra the memory terms live in.
//!
//! They are definitional, not proved: the axiom prover is QF_BV and has no
//! array model to quantify a memory over, so a read and a write mean here what
//! addition means for `Add`. What keeps them honest is that they are narrow.
//! Both read the state operand the seeder threads, which is the whole of memory
//! identity in the term graph: a chain reaches an access only through the writes
//! that actually happened on it, so a law that fires has already been told the
//! two accesses alias exactly.
//!
//! * **S1, load-over-store forwarding.** `Load(Store(s, a, n, v), a, n)` is `v`.
//!   Address and byte count are one class on both sides, so the two accesses are
//!   the same extent of the same address, and the store proves that extent
//!   dereferenceable. The two must also agree on an IR type: the vocabulary is
//!   bit-level, and a byte count alone would forward the integer a slot was
//!   written with into the float a reader spells it as.
//! * **S2, dead-store elimination.** `Store(Store(s, a, n, v1), a, n, v2)`
//!   leaves memory as `Store(s, a, n, v2)` does, so the inner store *is* the
//!   state `s` — provided nothing else reads it. That proviso is not algebra: a
//!   class is context-free, and unioning the inner state with `s` where a load
//!   also reads it would take that load back before the write. So the applier
//!   checks it against the reader index: the inner class must be read by the
//!   outer store alone, and must not be a state the region exports.

use super::View;
use crate::TypeId;
use crate::sem::SymKind;
use crate::sem::seed::{TermId, TermKind};

/// `Load(address, bytes, metadata, state)`.
const LOAD_STATE: usize = 3;
/// `Store(address, bytes, value, address_space, state)`.
const STORE_VALUE: usize = 2;
const STORE_STATE: usize = 4;
const ADDRESS: usize = 0;
const BYTES: usize = 1;

impl View {
    /// One pass of the state laws over the arena. Whether anything merged.
    pub(super) fn state_laws(&mut self) -> bool {
        let mut merged = false;
        for index in 0..self.graph().len() as u32 {
            let term = TermId::from_index(index);
            match self.graph().kind(term) {
                TermKind::Op(SymKind::LoadMemory) => merged |= self.forward_load(term),
                TermKind::Op(SymKind::StoreMemory) => merged |= self.eliminate_dead_store(term),
                _ => {}
            }
        }
        merged
    }

    /// S1: a load whose state a matching store left reads that store's value.
    fn forward_load(&mut self, load: TermId) -> bool {
        let state = self.child(load, LOAD_STATE);
        for store in self.stores_in(state) {
            if !self.same_access(load, store) {
                continue;
            }
            let value = self.child(store, STORE_VALUE);
            if !self.types_agree(load, value) {
                continue;
            }
            return self.union(load, value);
        }
        false
    }

    /// S2: the store an unread store immediately precedes overwrites it, so that
    /// inner store leaves the state it was handed.
    fn eliminate_dead_store(&mut self, store: TermId) -> bool {
        let state = self.child(store, STORE_STATE);
        for dead in self.stores_in(state) {
            if !self.same_access(store, dead) || !self.only_read_by(state, store) {
                continue;
            }
            let before = self.child(dead, STORE_STATE);
            return self.union(state, before);
        }
        false
    }

    /// The stores a class holds, in arena order.
    fn stores_in(&self, class: TermId) -> Vec<TermId> {
        self.members(class)
            .into_iter()
            .filter(|&member| self.graph().kind(member) == TermKind::Op(SymKind::StoreMemory))
            .collect()
    }

    /// Whether two accesses name one extent of one address.
    fn same_access(&self, left: TermId, right: TermId) -> bool {
        self.child(left, ADDRESS) == self.child(right, ADDRESS)
            && self.child(left, BYTES) == self.child(right, BYTES)
    }

    /// Whether `reader` is the only term reading `class`, and the region does not
    /// export it.
    fn only_read_by(&self, class: TermId, reader: TermId) -> bool {
        if self.is_exported(class) {
            return false;
        }
        match self.readers(class)[..] {
            [only] => self.find(only) == self.find(reader),
            _ => false,
        }
    }

    /// Whether the two classes name a value of the same IR type. A class carries
    /// a type on whichever members were seeded with one; the loaded and the
    /// stored value must agree on one of them for the load to *be* the store's
    /// value rather than a reinterpretation of its bits.
    fn types_agree(&self, left: TermId, right: TermId) -> bool {
        let types = |class| -> Vec<TypeId> {
            self.members(class)
                .into_iter()
                .filter_map(|member| self.graph().ty(member))
                .collect()
        };
        let right = types(right);
        types(left).iter().any(|ty| right.contains(ty))
    }

    /// The class of a term's child.
    fn child(&self, term: TermId, index: usize) -> TermId {
        self.find(self.graph().children(term)[index])
    }
}
