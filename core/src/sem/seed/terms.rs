//! The seeded term graph: a data-oriented arena of hash-consed terms.

use tir_adt::FxBuildHasher;

use crate::sem::SymKind;
use crate::{Context, TypeId, ValueId};

use std::hash::{BuildHasher, Hash, Hasher};

/// Index of a term in the arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct TermId(u32);

impl TermId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(index: u32) -> Self {
        Self(index)
    }
}

/// What a term denotes. Operators carry the shared semantic vocabulary; the
/// remaining kinds are the seeder's own structure — leaves it cannot look
/// through, and the projections that keep a loop's readings apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermKind {
    /// A semantic operator over its children.
    Op(SymKind),
    /// An integer literal.
    Const,
    /// A value the seeder does not look through: a function argument, a
    /// definition outside the block being seeded, or an unmodeled operation.
    Anchor,
    /// A distinct unknown; two opaque terms are never equal.
    Opaque,
    /// Result `index` of a multi-result term.
    Project,
    /// The value a loop port holds once the loop is left. Distinct from the
    /// [`SymKind::Theta`] it projects: that term is the value *inside* the body.
    LoopExit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermPayload {
    None,
    Value(ValueId),
    Int { width: u32, bits: i64 },
    Serial(u32),
    Index(u32),
}

/// The term graph seeded from a region tree. Structure of arrays: one entry per
/// term in each column, children in a shared CSR pool, and an index-only
/// hash-cons table over the arena.
pub struct SeedGraph {
    kinds: Vec<TermKind>,
    payloads: Vec<TermPayload>,
    types: Vec<Option<TypeId>>,
    /// CSR offsets into `children`, one more entry than there are terms.
    child_offsets: Vec<u32>,
    children: Vec<TermId>,
    /// Open-addressing hash-cons: bucket -> term index, `u32::MAX` when empty.
    table: Vec<u32>,
    /// The hash-consed terms, in insertion order, so a table growth re-inserts
    /// exactly them and never a minted one.
    consed: Vec<TermId>,
    hasher: FxBuildHasher,
    /// Term seeded for each value, indexed by value id; `u32::MAX` when the
    /// value carries no term.
    value_terms: Vec<u32>,
}

impl SeedGraph {
    pub(super) fn new() -> Self {
        Self {
            kinds: Vec::new(),
            payloads: Vec::new(),
            types: Vec::new(),
            child_offsets: vec![0],
            children: Vec::new(),
            table: vec![u32::MAX; 64],
            consed: Vec::new(),
            hasher: FxBuildHasher::default(),
            value_terms: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn kind(&self, term: TermId) -> TermKind {
        self.kinds[term.index()]
    }

    pub fn payload(&self, term: TermId) -> TermPayload {
        self.payloads[term.index()]
    }

    pub fn ty(&self, term: TermId) -> Option<TypeId> {
        self.types[term.index()]
    }

    pub fn children(&self, term: TermId) -> &[TermId] {
        let start = self.child_offsets[term.index()] as usize;
        let end = self.child_offsets[term.index() + 1] as usize;
        &self.children[start..end]
    }

    /// The term seeded for `value`, if the seeder reached it.
    pub fn term_of(&self, value: ValueId) -> Option<TermId> {
        match self.value_terms.get(value.index()).copied() {
            Some(u32::MAX) | None => None,
            Some(term) => Some(TermId(term)),
        }
    }

    /// The term at an arena index.
    pub fn term_at(&self, index: usize) -> TermId {
        assert!(index < self.len(), "the arena holds the term");
        TermId(index as u32)
    }

    /// Hash-cons a term: an equal one already in the arena is reused.
    pub(crate) fn intern(
        &mut self,
        kind: TermKind,
        payload: TermPayload,
        ty: Option<TypeId>,
        children: &[TermId],
    ) -> TermId {
        let hash = self.hash_of(kind, payload, ty, children);
        let mut bucket = self.bucket(hash);
        loop {
            let slot = self.table[bucket];
            if slot == u32::MAX {
                break;
            }
            let candidate = TermId(slot);
            if self.kinds[candidate.index()] == kind
                && self.payloads[candidate.index()] == payload
                && self.types[candidate.index()] == ty
                && self.children(candidate) == children
            {
                return candidate;
            }
            bucket = (bucket + 1) & (self.table.len() - 1);
        }

        let term = self.push(kind, payload, ty, children);
        self.table[bucket] = term.0;
        self.consed.push(term);
        if self.consed.len() * 10 >= self.table.len() * 7 {
            self.grow();
        }
        term
    }

    /// Add a term that never hash-conses: a distinct unknown, or a cyclic
    /// [`SymKind::Theta`] whose latch child is patched once its body is seeded.
    pub(super) fn mint(
        &mut self,
        kind: TermKind,
        payload: TermPayload,
        ty: Option<TypeId>,
        children: &[TermId],
    ) -> TermId {
        self.push(kind, payload, ty, children)
    }

    pub(super) fn set_child(&mut self, term: TermId, index: usize, child: TermId) {
        let start = self.child_offsets[term.index()] as usize;
        self.children[start + index] = child;
    }

    pub(super) fn record_value(&mut self, value: ValueId, term: TermId) -> Option<TermId> {
        let previous = self.term_of(value);
        if self.value_terms.len() <= value.index() {
            self.value_terms.resize(value.index() + 1, u32::MAX);
        }
        self.value_terms[value.index()] = term.0;
        previous
    }

    pub(super) fn forget_value(&mut self, value: ValueId, previous: Option<TermId>) {
        self.value_terms[value.index()] = previous.map_or(u32::MAX, |term| term.0);
    }

    fn push(
        &mut self,
        kind: TermKind,
        payload: TermPayload,
        ty: Option<TypeId>,
        children: &[TermId],
    ) -> TermId {
        let term = TermId(self.kinds.len() as u32);
        self.kinds.push(kind);
        self.payloads.push(payload);
        self.types.push(ty);
        self.children.extend_from_slice(children);
        self.child_offsets.push(self.children.len() as u32);
        term
    }

    fn grow(&mut self) {
        let mut table = vec![u32::MAX; self.table.len() * 2];
        let mask = table.len() - 1;
        for &term in &self.consed {
            let index = term.index();
            let hash = self.hash_of(
                self.kinds[index],
                self.payloads[index],
                self.types[index],
                self.children(term),
            );
            let mut bucket = hash as usize & mask;
            while table[bucket] != u32::MAX {
                bucket = (bucket + 1) & mask;
            }
            table[bucket] = term.0;
        }
        self.table = table;
    }

    fn bucket(&self, hash: u64) -> usize {
        hash as usize & (self.table.len() - 1)
    }

    fn hash_of(
        &self,
        kind: TermKind,
        payload: TermPayload,
        ty: Option<TypeId>,
        children: &[TermId],
    ) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        kind.hash(&mut hasher);
        payload.hash(&mut hasher);
        ty.hash(&mut hasher);
        children.hash(&mut hasher);
        hasher.finish()
    }

    /// A textual rendering of the whole arena, term order first and the value
    /// bindings after, sorted so two seedings of one program print alike.
    pub fn dump(&self, context: &Context) -> String {
        let mut out = String::new();
        for index in 0..self.len() {
            let term = TermId(index as u32);
            out.push_str(&format!("t{index} = {}", self.head(term)));
            for child in self.children(term) {
                out.push_str(&format!(" t{}", child.index()));
            }
            if let Some(ty) = self.ty(term).filter(|_| self.kind(term) != TermKind::Const) {
                out.push_str(&format!(" : {}", context.type_to_string(ty)));
            }
            out.push('\n');
        }
        out.push_str("values:\n");
        for (value, &term) in self.value_terms.iter().enumerate() {
            if term != u32::MAX {
                out.push_str(&format!("  %{value} -> t{term}\n"));
            }
        }
        out
    }

    fn head(&self, term: TermId) -> String {
        match (self.kind(term), self.payload(term)) {
            (TermKind::Op(kind), _) => format!("{kind:?}"),
            (TermKind::Const, TermPayload::Int { width, bits }) => format!("const {bits}:i{width}"),
            (TermKind::Anchor, TermPayload::Value(value)) => format!("anchor %{}", value.number()),
            (TermKind::Opaque, TermPayload::Serial(serial)) => format!("opaque #{serial}"),
            (TermKind::Project, TermPayload::Index(index)) => format!("project {index}"),
            (TermKind::LoopExit, _) => "loop_exit".to_string(),
            (kind, _) => format!("{kind:?}"),
        }
    }
}
