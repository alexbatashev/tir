use crate::egraph::{EGraph, EMatch, ENode, Id, Pattern, Substitution};

/// Imperative RHS: given the e-graph, match bindings, and matched root, assert the equivalences the rewrite proves.
pub type Applier<N, S> = dyn Fn(&mut EGraph<N>, &Substitution<S>, Id) + Send + Sync;

/// The right-hand side of a [`Rewrite`].
pub enum Rhs<N: ENode, S> {
    /// A template instantiated from the match and unioned with the matched root.
    Pattern(Pattern<N, S>),
    /// An arbitrary applier for rewrites a template cannot express.
    Apply(Box<Applier<N, S>>),
}

/// Search the e-graph for `lhs`, then apply `rhs` to each match.
pub struct Rewrite<N: ENode, S> {
    pub name: String,
    pub lhs: Pattern<N, S>,
    pub rhs: Rhs<N, S>,
}

impl<N: ENode, S: Clone + PartialEq> Rewrite<N, S> {
    pub fn new(name: impl Into<String>, lhs: Pattern<N, S>, rhs: Rhs<N, S>) -> Self {
        Self {
            name: name.into(),
            lhs,
            rhs,
        }
    }

    /// Apply the right-hand side to a single match.
    pub fn apply_match(&self, eg: &mut EGraph<N>, m: &EMatch<S>) {
        match &self.rhs {
            Rhs::Pattern(p) => {
                let id = p.instantiate(eg, &m.subst);
                eg.union(m.root, id);
            }
            Rhs::Apply(f) => f(eg, &m.subst, m.root),
        }
    }

    /// One pass: apply the rewrite to every current match, then restore congruence.
    pub fn apply_all(&self, eg: &mut EGraph<N>) {
        for m in self.lhs.search(eg) {
            self.apply_match(eg, &m);
        }
        eg.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_lang::*;
    use super::*;
    use crate::egraph::Var;

    #[test]
    fn double_negation_eliminates_via_declarative_rhs() {
        // neg(neg(x)) => x
        let mut lhs: Pattern<Math, &'static str> = Pattern::new();
        let x = lhs.var(Var::Symbol("x"));
        let inner = lhs.add(Math::Neg([x]));
        lhs.add(Math::Neg([inner]));

        let mut rhs: Pattern<Math, &'static str> = Pattern::new();
        rhs.var(Var::Symbol("x"));

        let rule = Rewrite::new("double-neg", lhs, Rhs::Pattern(rhs));

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let nn = neg(&mut g, a);
        let nna = neg(&mut g, nn);
        assert!(!g.connected(nna, a));

        rule.apply_all(&mut g);
        assert!(g.connected(nna, a));
    }

    #[test]
    fn commutativity_unions_swapped_form() {
        // add(x, y) => add(y, x)
        let rule = comm_rule();

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let ab = add(&mut g, a, b);
        let ba = add(&mut g, b, a);
        assert!(!g.connected(ab, ba));

        rule.apply_all(&mut g);
        assert!(g.connected(ab, ba));
    }

    #[test]
    fn additive_identity_via_integer_literal() {
        // add(x, 0) => x
        let rule = add_zero_rule();

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let z = num(&mut g, 0);
        let root = add(&mut g, a, z);
        assert!(!g.connected(root, a));

        rule.apply_all(&mut g);
        assert!(g.connected(root, a));
    }

    #[test]
    fn imperative_applier_unions_root_with_binding() {
        // neg(x) => x via a closure (degenerate; just exercises the escape hatch).
        let mut lhs: Pattern<Math, &'static str> = Pattern::new();
        let x = lhs.var(Var::Symbol("x"));
        lhs.add(Math::Neg([x]));

        let rule = Rewrite::new(
            "neg-id",
            lhs,
            Rhs::Apply(Box::new(|eg, subst, root| {
                let x = subst.get(&Var::Symbol("x")).unwrap();
                eg.union(root, x);
            })),
        );

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let na = neg(&mut g, a);
        assert!(!g.connected(na, a));

        rule.apply_all(&mut g);
        assert!(g.connected(na, a));
    }
}
