use std::{collections::HashMap, hash::Hash};

use tir_adt::DisjointSet;

#[derive(Clone, Copy, Hash, Debug)]
pub struct Id(u32);

pub struct EGraph<N: Hash + Eq> {
    ds: DisjointSet,
    nodes: Vec<N>,
    memo: HashMap<N, Id>,
    classes: HashMap<Id, EClass<N>>,
}

pub struct EClass<N: Hash + Eq> {
    nodes: Vec<N>,
    parents: Vec<(N, Id)>,
}

impl<N: Hash + Eq> EGraph<N> {
    pub fn nodes(&self) -> &[N] {
        &self.nodes
    }

    pub fn is_empty(&self) -> bool {
        self.memo.is_empty()
    }

    pub fn total_size(&self) -> usize {
        self.memo.len()
    }

    pub fn find_root(&self, id: Id) -> Id {
        Id(self.ds.find_root(id.0))
    }

    pub fn connected(&self, x: N, y: N) -> bool {
        // FIXME what if nodes don't exist?
        self.ds
            .connected(self.memo.get(&x).unwrap().0, self.memo.get(&y).unwrap().0)
    }

    pub fn union(&mut self, x: Id, y: Id) -> bool {
        todo!()
    }
}
