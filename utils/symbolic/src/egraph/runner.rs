use crate::egraph::{EGraph, ENode, Id, Rewrite};

/// Drives equality saturation: repeatedly applies a rule set to the e-graph until
/// it reaches a fixpoint or a configured limit, then hands the saturated
/// [`EGraph`] and canonical roots back for the caller to extract from.
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

    /// Saturate the e-graph with `rules`. Each iteration searches every rule
    /// against the same snapshot, then applies all matches and rebuilds — so a
    /// node born this iteration is only visible to the next one. Stops at a
    /// fixpoint (an iteration that changes neither the class nor the node count),
    /// or once the iteration or node limit is reached.
    pub fn run<'a, S>(&mut self, rules: impl IntoIterator<Item = &'a Rewrite<N, S>>)
    where
        N: 'a,
        S: Clone + PartialEq + 'a,
    {
        let rules: Vec<&Rewrite<N, S>> = rules.into_iter().collect();
        let mut iters = 0;
        loop {
            if iters >= self.iter_limit || self.egraph.total_size() >= self.node_limit {
                break;
            }
            let before = (self.egraph.num_classes(), self.egraph.total_size());

            let searched: Vec<_> = rules
                .iter()
                .map(|rule| (*rule, rule.lhs.search(&self.egraph)))
                .collect();
            for (rule, matches) in &searched {
                for m in matches {
                    rule.apply_match(&mut self.egraph, m);
                }
            }
            self.egraph.rebuild();

            iters += 1;
            if (self.egraph.num_classes(), self.egraph.total_size()) == before {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tir_adt::APInt;

    use super::super::test_lang::*;
    use super::Runner;
    use crate::egraph::{EGraph, Pattern, Rewrite, Rhs, Var};

    /// `add(x, y) => add(y, x)`.
    fn comm() -> Rewrite<Math, &'static str> {
        let mut lhs: Pattern<Math, &'static str> = Pattern::new();
        let x = lhs.var(Var::Symbol("x"));
        let y = lhs.var(Var::Symbol("y"));
        lhs.add(Math::Add([x, y]));

        let mut rhs: Pattern<Math, &'static str> = Pattern::new();
        let rx = rhs.var(Var::Symbol("x"));
        let ry = rhs.var(Var::Symbol("y"));
        rhs.add(Math::Add([ry, rx]));

        Rewrite::new("add-comm", lhs, Rhs::Pattern(rhs))
    }

    /// `add(x, 0) => x`.
    fn add_zero() -> Rewrite<Math, &'static str> {
        let mut lhs: Pattern<Math, &'static str> = Pattern::new();
        let x = lhs.var(Var::Symbol("x"));
        let zero = lhs.var(Var::Int(APInt::from_i64(0)));
        lhs.add(Math::Add([x, zero]));

        let mut rhs: Pattern<Math, &'static str> = Pattern::new();
        rhs.var(Var::Symbol("x"));

        Rewrite::new("add-zero", lhs, Rhs::Pattern(rhs))
    }

    #[test]
    fn saturates_and_applies_a_rule() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);
        assert!(!g.connected(ab, ba));

        let mut runner = Runner::new(g, vec![]);
        runner.run(&[comm()]);
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
        runner.run(&[comm(), add_zero()]);
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
        runner.run(&[comm()]);
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
        runner.run(&[comm()]);
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
        runner.run(&[comm()]);
        let roots = runner.roots();
        assert_eq!(roots[0], roots[1]);
    }
}
