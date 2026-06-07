use std::collections::HashMap;

use crate::Context;
use crate::graph::{Dag, Matchable, NodeId, Pattern};

use super::{EClassId, EGraph};

/// One match of a [`Pattern`] against the e-graph.
#[derive(Clone, Debug)]
pub struct EMatch {
    root: EClassId,
    bindings: Vec<EClassId>,
}

impl EMatch {
    pub fn new(root: EClassId, bindings: Vec<EClassId>) -> Self {
        Self { root, bindings }
    }

    pub fn root(&self) -> EClassId {
        self.root
    }

    pub fn binding(&self, pattern_node: NodeId) -> EClassId {
        self.bindings[pattern_node.index()]
    }
}

pub type Applier<N, L> = dyn Fn(&Context, &mut EGraph<N, L>, &EMatch) + Send + Sync;

/// A rewrite: e-match `lhs`, then call `apply` for each match.
pub struct Rewrite<N, L> {
    pub name: String,
    pub searcher: Pattern<N, ()>,
    pub apply: Box<Applier<N, L>>,
}

impl<N, L> Rewrite<N, L> {
    pub fn new(
        name: impl Into<String>,
        searcher: Pattern<N, ()>,
        apply: Box<Applier<N, L>>,
    ) -> Self {
        Self {
            name: name.into(),
            searcher,
            apply,
        }
    }

    pub fn lhs(&self) -> &Pattern<N, ()> {
        &self.searcher
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SaturationLimits {
    pub max_iterations: usize,
    pub max_classes: usize,
}

impl Default for SaturationLimits {
    fn default() -> Self {
        Self {
            max_iterations: 30,
            max_classes: 10_000,
        }
    }
}

impl<N: Matchable + Clone + Eq + std::hash::Hash, L: Clone + PartialEq> EGraph<N, L> {
    pub fn saturate(
        &mut self,
        ctx: &Context,
        rewrites: &[Rewrite<N, L>],
        limits: SaturationLimits,
    ) {
        for _ in 0..limits.max_iterations {
            let mut matches = Vec::new();
            for (index, rw) in rewrites.iter().enumerate() {
                for m in self.ematch(ctx, rw.lhs()) {
                    matches.push((index, m));
                }
            }
            if matches.is_empty() {
                break;
            }

            let before = self.eclass.len();
            for (index, m) in matches {
                (rewrites[index].apply)(ctx, self, &m);
            }
            self.rebuild();

            if self.eclass.len() == before || self.num_classes() >= limits.max_classes {
                break;
            }
        }
        self.rebuild();
    }

    pub fn extract_best(
        &self,
        cost: impl Fn(&N, &[u64]) -> u64,
    ) -> HashMap<EClassId, (NodeId, u64)> {
        let mut best = HashMap::new();
        loop {
            let mut changed = false;
            for class in self.classes() {
                for &node_id in self.nodes(class) {
                    let child_costs: Option<Vec<u64>> = self
                        .child_classes(node_id)
                        .iter()
                        .map(|c| best.get(&self.find(*c)).map(|(_, cost)| *cost))
                        .collect();
                    let Some(child_costs) = child_costs else {
                        continue;
                    };
                    let label = self.dag.get_node(node_id);
                    let total = cost(label, &child_costs).saturating_add(child_costs.iter().sum());
                    let class = self.find(class);
                    if best
                        .get(&class)
                        .is_none_or(|(_, existing)| total < *existing)
                    {
                        best.insert(class, (node_id, total));
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        best
    }
}
