mod eclass;
mod enode;

use std::{collections::HashMap, hash::Hash};

use tir_adt::DisjointSet;

pub use eclass::*;
pub use enode::*;

#[derive(Clone, Copy, Hash, Debug, Eq, PartialEq)]
pub struct Id(u32);

pub struct EGraph<N: ENode> {
    ds: DisjointSet,
    nodes: Vec<N>,
    memo: HashMap<HashConsed<N>, Id>,
    classes: HashMap<Id, EClass<N>>,
}

impl<N: ENode> EGraph<N> {
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
        self.ds.connected(
            self.memo.get(&x.into()).unwrap().0,
            self.memo.get(&y.into()).unwrap().0,
        )
    }

    pub fn push(&self, n: N) {}

    pub fn push_uncanonical(&mut self, mut n: N) -> Id {
        let original = n.clone();
        todo!()
    }

    pub fn union(&mut self, x: Id, y: Id) -> bool {
        let root_x = self.find_root(x);
        let root_y = self.find_root(y);

        if root_x != root_y {
            let z = Id(self.ds.union(root_x.0, root_y.0));
            let (to, from) = if root_x == z {
                (root_x, root_y)
            } else if root_y == z {
                (root_y, root_x)
            } else {
                unreachable!()
            };
        }
        todo!()
    }

    fn find_internal<B>(&mut self, mut n: B) -> Option<Id>
    where
        B: std::borrow::BorrowMut<N>,
    {
        let n = n.borrow_mut();
        self.memo.get(&HashConsed(n.clone())).copied()
    }
}
