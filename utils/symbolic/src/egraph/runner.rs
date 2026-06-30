use crate::egraph::{EGraph, ENode, Id, Rewrite};

/// Drives equality saturation to a fixpoint or limit, then hands back the saturated [`EGraph`] and canonical roots.
pub struct Runner<N: ENode> {
    egraph: EGraph<N>,
    roots: Vec<Id>,
    iter_limit: usize,
    node_limit: usize,
}

impl<N: ENode> Runner<N> {
    pub fn new(egraph: EGraph<N>, roots: Vec<Id>) -> Self {
        Self {
            egraph,
            roots,
            iter_limit: 30,
            node_limit: 100_000,
        }
    }

    pub fn with_iter_limit(mut self, limit: usize) -> Self {
        self.iter_limit = limit;
        self
    }

    pub fn with_node_limit(mut self, limit: usize) -> Self {
        self.node_limit = limit;
        self
    }

    pub fn egraph(&self) -> &EGraph<N> {
        &self.egraph
    }

    /// The construction-time roots, canonicalized to their current classes.
    pub fn roots(&self) -> Vec<Id> {
        self.roots.iter().map(|&r| self.egraph.find(r)).collect()
    }

    /// Saturate with `rules`; each iteration searches against one snapshot, so a node born this iteration is visible only to the next. Stops at a fixpoint or the iter/node limit.
    pub fn run<'a, S>(&mut self, rules: impl IntoIterator<Item = &'a Rewrite<N, S>>)
    where
        N: 'a,
        S: Clone + PartialEq + 'a,
    {
        self.egraph
            .saturate(rules, self.iter_limit, self.node_limit);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_lang::*;
    use super::Runner;
    use crate::egraph::EGraph;

    #[test]
    fn saturates_and_applies_a_rule() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);
        assert!(!g.connected(ab, ba));

        let mut runner = Runner::new(g, vec![]);
        runner.run(&[comm_rule()]);
        assert!(runner.egraph().connected(ab, ba));
    }

    #[test]
    fn combines_rules_across_iterations() {
        // add(0, a): commutativity exposes add(a, 0), then add-zero collapses it.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let z = num(&mut g, 0);
        let root = add(&mut g, z, a);
        assert!(!g.connected(root, a));

        let mut runner = Runner::new(g, vec![]);
        runner.run(&[comm_rule(), add_zero_rule()]);
        assert!(runner.egraph().connected(root, a));
    }

    #[test]
    fn iter_limit_zero_does_nothing() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);
        let classes = g.num_classes();

        let mut runner = Runner::new(g, vec![]).with_iter_limit(0);
        runner.run(&[comm_rule()]);
        assert!(!runner.egraph().connected(ab, ba));
        assert_eq!(runner.egraph().num_classes(), classes);
    }

    #[test]
    fn node_limit_halts_before_growth() {
        // comm must mint add(b, a); capping at the current size blocks it.
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        add(&mut g, a, b);
        let size = g.total_size();

        let mut runner = Runner::new(g, vec![]).with_node_limit(size);
        runner.run(&[comm_rule()]);
        assert_eq!(runner.egraph().total_size(), size);
    }

    #[test]
    fn roots_canonicalize_after_saturation() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);

        let mut runner = Runner::new(g, vec![ab, ba]);
        runner.run(&[comm_rule()]);
        let roots = runner.roots();
        assert_eq!(roots[0], roots[1]);
    }
}
