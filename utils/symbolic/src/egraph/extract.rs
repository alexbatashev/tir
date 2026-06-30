use std::collections::HashMap;

use crate::egraph::{EGraph, ENode, Id};

/// Cheapest representative e-node per e-class, chosen by [`EGraph::extract_best`].
pub struct Extraction<L: ENode> {
    best: HashMap<Id, L>,
}

impl<L: ENode> Extraction<L> {
    /// Chosen node for `id`'s class, or `None` if no node has finite cost. `id` must
    /// be canonical ([`EGraph::find`]).
    pub fn node(&self, id: Id) -> Option<&L> {
        self.best.get(&id)
    }
}

impl<L: ENode> EGraph<L> {
    /// Greedy bottom-up extraction: per class, the node minimizing `cost_of(node)`
    /// plus each child's chosen cost. Cycle-tolerant — a node with un-costed children
    /// is skipped and revisited to a fixpoint, so a cycle is costed through its
    /// non-cyclic input. Scope-aware via [`EGraph::classes`]/[`EGraph::find`].
    pub fn extract_best(&self, cost_of: impl Fn(&L) -> u64) -> Extraction<L> {
        let mut cost: HashMap<Id, u64> = HashMap::new();
        let mut best: HashMap<Id, L> = HashMap::new();
        loop {
            let mut changed = false;
            for class in self.classes() {
                let id = self.find(class.id());
                for node in class.nodes() {
                    let Some(total) = self.node_cost(node, &cost_of, &cost) else {
                        continue;
                    };
                    if cost.get(&id).is_none_or(|&best| total < best) {
                        cost.insert(id, total);
                        best.insert(id, node.clone());
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Extraction { best }
    }

    /// `cost_of(node)` plus each child class's best cost, or `None` if any is un-costed.
    fn node_cost(
        &self,
        node: &L,
        cost_of: &impl Fn(&L) -> u64,
        cost: &HashMap<Id, u64>,
    ) -> Option<u64> {
        let mut total = cost_of(node);
        for &child in node.children() {
            total = total.saturating_add(*cost.get(&self.find(child))?);
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_lang::*;
    use super::*;

    /// Unit cost for operators, zero for leaves.
    fn unit(node: &Math) -> u64 {
        match node {
            Math::Num(_) | Math::FNum(_) | Math::Sym(_) => 0,
            _ => 1,
        }
    }

    #[test]
    fn picks_cheaper_equivalent_form() {
        // neg(neg(a)) unioned with a: extraction prefers the bare a (cost 0).
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let inner = neg(&mut g, a);
        let nn = neg(&mut g, inner);
        g.union(nn, a);
        g.rebuild();

        let extraction = g.extract_best(unit);
        assert!(matches!(extraction.node(g.find(a)).unwrap(), Math::Sym(0)));
    }

    #[test]
    fn sums_children_costs() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let na = neg(&mut g, a);
        let extraction = g.extract_best(unit);
        // neg(a) costs 1 (op) + 0 (leaf) = 1; the chosen node is the neg.
        assert!(matches!(extraction.node(g.find(na)).unwrap(), Math::Neg(_)));
    }

    #[test]
    fn terminates_on_a_cycle() {
        // a ≡ neg(a): the class is a self-cycle, but extraction still terminates and
        // costs it through the symbol leaf.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let na = neg(&mut g, a);
        g.union(a, na);
        g.rebuild();
        let extraction = g.extract_best(unit);
        assert!(extraction.node(g.find(a)).is_some());
    }
}
