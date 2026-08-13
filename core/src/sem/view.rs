//! The e-graph over a seeded term arena: build, saturate, and read the classes
//! back off it.
//!
//! The classes live in the arena the seeder filled — union-find is a flat
//! `Vec<u32>` over term indices, a term added during saturation is appended to
//! the same arena, and the reverse index naming every reader of a class is a
//! linked list over one shared edge pool. Nothing here owns a term.
//!
//! Congruence leaves two kinds alone. An [`TermKind::Opaque`] is a distinct
//! unknown by construction, and a [`SymKind::Theta`] carries no trip count, so
//! two loops that happen to spell one init and one latch alike are not the same
//! carried value.

#[cfg(test)]
mod tests;

use std::hash::{BuildHasher, Hash, Hasher};

use tir_adt::FxBuildHasher;

use crate::TypeId;
use crate::sem::SymKind;
use crate::sem::seed::{SeedGraph, TermId, TermKind, TermPayload};

/// How many terms one region's saturation may reach before it stops.
pub const DEFAULT_NODE_LIMIT: usize = 1 << 16;

/// An e-graph over a seeded term arena.
pub struct View {
    graph: SeedGraph,
    /// Union-find over term indices; a root points at itself.
    parent: Vec<u32>,
    /// Circular list of the terms of one class, threaded through every member.
    next_member: Vec<u32>,
    size: Vec<u32>,
    /// Head and tail of each class's reader list, `u32::MAX` when it has none.
    reader_head: Vec<u32>,
    reader_tail: Vec<u32>,
    /// The reader edge pool: the reading term, and the next edge of its class.
    reader_term: Vec<u32>,
    reader_next: Vec<u32>,
    node_limit: usize,
    hasher: FxBuildHasher,
}

impl View {
    pub fn new(graph: SeedGraph) -> Self {
        Self::with_node_limit(graph, DEFAULT_NODE_LIMIT)
    }

    pub fn with_node_limit(graph: SeedGraph, node_limit: usize) -> Self {
        let mut view = Self {
            graph,
            parent: Vec::new(),
            next_member: Vec::new(),
            size: Vec::new(),
            reader_head: Vec::new(),
            reader_tail: Vec::new(),
            reader_term: Vec::new(),
            reader_next: Vec::new(),
            node_limit,
            hasher: FxBuildHasher::default(),
        };
        view.absorb_new_terms();
        view
    }

    pub fn graph(&self) -> &SeedGraph {
        &self.graph
    }

    /// The class a term belongs to, named by its leading term.
    pub fn find(&self, term: TermId) -> TermId {
        let mut current = term.index() as u32;
        while self.parent[current as usize] != current {
            current = self.parent[current as usize];
        }
        TermId::from_index(current)
    }

    /// Merge two classes; whether they were apart.
    pub fn union(&mut self, left: TermId, right: TermId) -> bool {
        let (mut winner, mut loser) = (self.find(left), self.find(right));
        if winner == loser {
            return false;
        }
        let by_size = self.size[winner.index()].cmp(&self.size[loser.index()]);
        if by_size.then(loser.cmp(&winner)).is_lt() {
            std::mem::swap(&mut winner, &mut loser);
        }
        self.parent[loser.index()] = winner.index() as u32;
        self.size[winner.index()] += self.size[loser.index()];
        self.next_member.swap(winner.index(), loser.index());
        self.splice_readers(winner, loser);
        true
    }

    /// Add a term to the arena, or nothing once the node limit is reached.
    pub fn add(
        &mut self,
        kind: TermKind,
        payload: TermPayload,
        ty: Option<TypeId>,
        children: &[TermId],
    ) -> Option<TermId> {
        if self.graph.len() >= self.node_limit {
            return None;
        }
        let term = self.graph.intern(kind, payload, ty, children);
        self.absorb_new_terms();
        Some(term)
    }

    /// The terms of one class, in arena order.
    pub fn members(&self, class: TermId) -> Vec<TermId> {
        let class = self.find(class);
        let mut members = vec![class];
        let mut current = self.next_member[class.index()];
        while current != class.index() as u32 {
            members.push(TermId::from_index(current));
            current = self.next_member[current as usize];
        }
        members.sort();
        members
    }

    /// Every class of the graph, named by its leading term, in arena order.
    pub fn classes(&self) -> Vec<TermId> {
        (0..self.graph.len() as u32)
            .map(TermId::from_index)
            .filter(|&term| self.find(term) == term)
            .collect()
    }

    /// The terms that read a class as one of their children.
    pub fn readers(&self, class: TermId) -> Vec<TermId> {
        let class = self.find(class);
        let mut readers = Vec::new();
        let mut edge = self.reader_head[class.index()];
        while edge != u32::MAX {
            readers.push(TermId::from_index(self.reader_term[edge as usize]));
            edge = self.reader_next[edge as usize];
        }
        readers.sort();
        readers.dedup();
        readers
    }

    /// Saturate to a fixpoint, or to the node limit.
    pub fn saturate(&mut self) {
        while self.congruence() && self.graph.len() < self.node_limit {}
    }

    /// The classes and their members, one line each: two saturations of one
    /// graph print alike.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for class in self.classes() {
            out.push_str(&format!("c{}:", class.index()));
            for member in self.members(class) {
                out.push_str(&format!(" t{}", member.index()));
            }
            out.push('\n');
        }
        out
    }

    // ── congruence ──────────────────────────────────────────────────────────

    /// One closure pass over the arena: terms whose canonical readings agree
    /// merge. Whether anything merged.
    fn congruence(&mut self) -> bool {
        let mut table = vec![u32::MAX; (self.graph.len() * 2).next_power_of_two().max(64)];
        let mask = table.len() - 1;
        let mut merged = false;
        let (mut key, mut candidate_key) = (Vec::new(), Vec::new());
        for index in 0..self.graph.len() as u32 {
            let term = TermId::from_index(index);
            if !congruent(self.graph.kind(term)) {
                continue;
            }
            self.canonical_children(term, &mut key);
            let mut bucket = self.hash_of(term, &key) as usize & mask;
            loop {
                let slot = table[bucket];
                if slot == u32::MAX {
                    table[bucket] = index;
                    break;
                }
                let candidate = TermId::from_index(slot);
                self.canonical_children(candidate, &mut candidate_key);
                if self.same_head(term, candidate) && key == candidate_key {
                    merged |= self.union(term, candidate);
                    break;
                }
                bucket = (bucket + 1) & mask;
            }
        }
        merged
    }

    /// A term's children read as the classes they now name, ordered so a
    /// commutative operator reads its operands one way.
    fn canonical_children(&self, term: TermId, out: &mut Vec<TermId>) {
        out.clear();
        out.extend(self.graph.children(term).iter().map(|&c| self.find(c)));
        if matches!(self.graph.kind(term), TermKind::Op(kind) if kind.is_commutative()) {
            out.sort();
        }
    }

    fn same_head(&self, left: TermId, right: TermId) -> bool {
        self.graph.kind(left) == self.graph.kind(right)
            && self.graph.payload(left) == self.graph.payload(right)
            && self.graph.ty(left) == self.graph.ty(right)
    }

    fn hash_of(&self, term: TermId, children: &[TermId]) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        self.graph.kind(term).hash(&mut hasher);
        self.graph.payload(term).hash(&mut hasher);
        self.graph.ty(term).hash(&mut hasher);
        children.hash(&mut hasher);
        hasher.finish()
    }

    // ── bookkeeping ─────────────────────────────────────────────────────────

    /// Give every arena term the e-graph read the seeder left it without: its
    /// own class, and a reader edge on each of its children. The classes come
    /// first: a cyclic θ names a child the arena holds after it.
    fn absorb_new_terms(&mut self) {
        let first = self.parent.len();
        while self.parent.len() < self.graph.len() {
            let index = self.parent.len() as u32;
            self.parent.push(index);
            self.next_member.push(index);
            self.size.push(1);
            self.reader_head.push(u32::MAX);
            self.reader_tail.push(u32::MAX);
        }
        for index in first..self.graph.len() {
            let term = TermId::from_index(index as u32);
            for child in self.graph.children(term).to_vec() {
                self.record_reader(self.find(child), index as u32);
            }
        }
    }

    fn record_reader(&mut self, class: TermId, reader: u32) {
        let edge = self.reader_term.len() as u32;
        self.reader_term.push(reader);
        self.reader_next.push(u32::MAX);
        let tail = self.reader_tail[class.index()];
        if tail == u32::MAX {
            self.reader_head[class.index()] = edge;
        } else {
            self.reader_next[tail as usize] = edge;
        }
        self.reader_tail[class.index()] = edge;
    }

    fn splice_readers(&mut self, winner: TermId, loser: TermId) {
        let head = self.reader_head[loser.index()];
        if head == u32::MAX {
            return;
        }
        let tail = self.reader_tail[winner.index()];
        if tail == u32::MAX {
            self.reader_head[winner.index()] = head;
        } else {
            self.reader_next[tail as usize] = head;
        }
        self.reader_tail[winner.index()] = self.reader_tail[loser.index()];
        self.reader_head[loser.index()] = u32::MAX;
        self.reader_tail[loser.index()] = u32::MAX;
    }
}

fn congruent(kind: TermKind) -> bool {
    !matches!(kind, TermKind::Opaque | TermKind::Op(SymKind::Theta))
}
