use tir_adt::{APFloat, APInt};

use crate::egraph::{EGraph, ENode, Id};

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum Var<S> {
    Symbol(S),
    Int(APInt),
    Float(APFloat),
}

/// A mapping from pattern variables to the e-classes they bound to during a match.
#[derive(Debug, Clone, Eq, PartialEq, Ord, Hash, PartialOrd)]
pub struct Substitution<S> {
    pub(crate) vec: Vec<(Var<S>, Id)>,
}

impl<S> Default for Substitution<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Substitution<S> {
    pub fn new() -> Self {
        Self { vec: Vec::new() }
    }
}

impl<S: PartialEq> Substitution<S> {
    pub fn insert(&mut self, var: Var<S>, id: Id) -> Option<Id> {
        for pair in &mut self.vec {
            if var == pair.0 {
                return Some(core::mem::replace(&mut pair.1, id));
            }
        }
        self.vec.push((var, id));
        None
    }

    pub fn get(&self, var: &Var<S>) -> Option<Id> {
        self.vec
            .iter()
            .find(|pair| &pair.0 == var)
            .map(|pair| pair.1)
    }
}

/// One node of a [`Pattern`]: either a template operator or a hole.
#[derive(Debug, Clone)]
pub enum PatternNode<N: ENode, S> {
    /// A template e-node. Its [`ENode::children`] ids are *pattern-local indices*
    /// into the owning pattern's `nodes`, not e-class ids.
    Node(N),
    /// A hole. A [`Var::Symbol`] matches any e-class and binds it.
    Var(Var<S>),
}

/// A structural pattern over a language `N`, used both as a search template (LHS)
/// and, via [`Self::instantiate`], to build the right-hand side of a rewrite.
///
/// Nodes are stored bottom-up: every node's children were added before it, so a
/// child's index is always smaller than its parent's. The builder enforces this by
/// returning the [`Id`] of each added node for the caller to wire as a child.
#[derive(Debug, Clone)]
pub struct Pattern<N: ENode, S> {
    nodes: Vec<PatternNode<N, S>>,
    root: Id,
}

/// One match of a [`Pattern`] against an e-graph: the matched e-class and the
/// variable bindings that made it match.
#[derive(Debug, Clone)]
pub struct EMatch<S> {
    pub root: Id,
    pub subst: Substitution<S>,
}

impl<N: ENode, S> Default for Pattern<N, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: ENode, S> Pattern<N, S> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            root: Id::from_raw(0),
        }
    }

    /// Add a hole; the returned id is the root until a later `add`/`var` or an
    /// explicit [`Self::set_root`].
    pub fn var(&mut self, var: Var<S>) -> Id {
        self.push(PatternNode::Var(var))
    }

    /// Add a template node. Wire its children to ids returned by earlier calls.
    pub fn add(&mut self, node: N) -> Id {
        self.push(PatternNode::Node(node))
    }

    pub fn set_root(&mut self, root: Id) {
        self.root = root;
    }

    fn push(&mut self, node: PatternNode<N, S>) -> Id {
        let id = Id::from_raw(self.nodes.len() as u32);
        self.nodes.push(node);
        self.root = id;
        id
    }
}

impl<N: ENode, S: Clone + PartialEq> Pattern<N, S> {
    /// Every match of this pattern across the whole e-graph.
    pub fn search(&self, eg: &EGraph<N>) -> Vec<EMatch<S>> {
        let mut out = Vec::new();
        for class in eg.classes() {
            let root = class.id();
            for subst in self.match_node(eg, self.root, root, Substitution::new()) {
                out.push(EMatch { root, subst });
            }
        }
        out
    }

    /// Every substitution under which pattern node `pat` matches e-class `class`,
    /// extending `partial`.
    fn match_node(
        &self,
        eg: &EGraph<N>,
        pat: Id,
        class: Id,
        partial: Substitution<S>,
    ) -> Vec<Substitution<S>> {
        match &self.nodes[pat.index()] {
            PatternNode::Var(var @ Var::Symbol(_)) => {
                let mut subst = partial;
                match subst.get(var) {
                    Some(bound) if eg.find(bound) != eg.find(class) => Vec::new(),
                    Some(_) => vec![subst],
                    None => {
                        subst.insert(var.clone(), class);
                        vec![subst]
                    }
                }
            }
            // Literal leaves need a per-language constant bridge; not yet supported.
            PatternNode::Var(Var::Int(_) | Var::Float(_)) => {
                unimplemented!("literal pattern leaves are not yet supported")
            }
            PatternNode::Node(template) => {
                let mut out = Vec::new();
                for enode in eg.nodes(class) {
                    if !template.matches(enode)
                        || template.children().len() != enode.children().len()
                    {
                        continue;
                    }
                    let mut partials = vec![partial.clone()];
                    for (pc, ec) in template.children().iter().zip(enode.children()) {
                        let child = eg.find(*ec);
                        let mut next = Vec::new();
                        for p in partials {
                            next.extend(self.match_node(eg, *pc, child, p));
                        }
                        partials = next;
                    }
                    out.extend(partials);
                }
                out
            }
        }
    }

    /// Build this pattern into `eg` under `subst`, returning the root e-class.
    pub fn instantiate(&self, eg: &mut EGraph<N>, subst: &Substitution<S>) -> Id {
        let mut ids: Vec<Id> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let id = match node {
                PatternNode::Var(var @ Var::Symbol(_)) => {
                    subst.get(var).expect("unbound pattern variable")
                }
                PatternNode::Var(Var::Int(_) | Var::Float(_)) => {
                    unimplemented!("literal pattern leaves are not yet supported")
                }
                PatternNode::Node(template) => {
                    let mut node = template.clone();
                    for child in node.children_mut() {
                        *child = ids[child.index()];
                    }
                    eg.add(node)
                }
            };
            ids.push(id);
        }
        ids[self.root.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_lang::*;
    use super::*;

    /// `add(x, y)` with `x`, `y` symbol holes.
    fn add_pattern() -> Pattern<Math, &'static str> {
        let mut p = Pattern::new();
        let x = p.var(Var::Symbol("x"));
        let y = p.var(Var::Symbol("y"));
        p.add(Math::Add([x, y]));
        p
    }

    #[test]
    fn search_binds_operands() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let root = add(&mut g, a, b);

        let matches = add_pattern().search(&g);
        assert_eq!(matches.len(), 1);
        let m = &matches[0];
        assert_eq!(g.find(m.root), g.find(root));
        assert_eq!(m.subst.get(&Var::Symbol("x")), Some(g.find(a)));
        assert_eq!(m.subst.get(&Var::Symbol("y")), Some(g.find(b)));
    }

    #[test]
    fn search_rejects_wrong_operator_and_arity() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        neg(&mut g, a);

        // Add wants two children; Neg has one and a different operator.
        assert!(add_pattern().search(&g).is_empty());
    }

    #[test]
    fn nonlinear_pattern_requires_equal_operands() {
        // add(x, x) matches add(a, a) but not add(a, b).
        let mut p: Pattern<Math, &'static str> = Pattern::new();
        let x = p.var(Var::Symbol("x"));
        p.add(Math::Add([x, x]));

        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        add(&mut g, a, b);
        assert!(p.search(&g).is_empty());

        add(&mut g, a, a);
        let matches = p.search(&g);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].subst.get(&Var::Symbol("x")), Some(g.find(a)));
    }

    #[test]
    fn instantiate_builds_under_substitution() {
        let mut g = EGraph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);

        let mut subst = Substitution::new();
        subst.insert(Var::Symbol("x"), a);
        subst.insert(Var::Symbol("y"), b);

        let built = add_pattern().instantiate(&mut g, &subst);
        // Hash-consing means rebuilding add(a, b) lands on the original class.
        let original = add(&mut g, a, b);
        assert_eq!(g.find(built), g.find(original));
    }
}
