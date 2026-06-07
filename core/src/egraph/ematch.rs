use crate::Context;
use crate::graph::{Dag, OperandConstraint, Pattern, PatternExpr};

use super::{EClassId, EGraph, EMatch, Matchable, NodeId};

impl<N: Matchable + Clone + Eq + std::hash::Hash, L: Clone + PartialEq> EGraph<N, L> {
    pub fn ematch<A>(&self, ctx: &Context, pattern: &Pattern<N, A>) -> Vec<EMatch> {
        self.ematch_with_legality(ctx, pattern, &|_, _| true)
    }

    pub fn ematch_with_legality<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<EMatch> {
        let Some(root) = pattern.root() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for class in self.classes() {
            for binding in self.solve(ctx, pattern, root, class, allowed) {
                out.push(EMatch::new(
                    class,
                    binding.into_iter().map(Option::unwrap).collect(),
                ));
            }
        }
        out
    }

    fn solve<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        pattern_node: NodeId,
        class: EClassId,
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<Vec<Option<EClassId>>> {
        let class = self.find(class);
        let empty = || vec![None; pattern.len()];

        if !allowed(pattern_node, class) {
            return Vec::new();
        }

        match pattern.get_node(pattern_node) {
            PatternExpr::Boundary => {
                if !self.boundary_ok(pattern, pattern_node, class) {
                    return Vec::new();
                }
                let mut b = empty();
                b[pattern_node.index()] = Some(class);
                vec![b]
            }
            PatternExpr::Any => {
                let mut b = empty();
                b[pattern_node.index()] = Some(class);
                vec![b]
            }
            PatternExpr::Leaf => {
                if self.class_has_leaf(ctx, class) {
                    let mut b = empty();
                    b[pattern_node.index()] = Some(class);
                    vec![b]
                } else {
                    Vec::new()
                }
            }
            PatternExpr::Node(template) => {
                let children = pattern.children(pattern_node).to_vec();
                let commutative = template.is_commutative() && children.len() == 2;
                let mut results = Vec::new();

                for &node_id in self.nodes(class) {
                    let node_children = self.child_classes(node_id);
                    if node_children.len() != children.len() {
                        continue;
                    }
                    if !self.dag.get_node(node_id).matches_pattern(template, ctx) {
                        continue;
                    }

                    let orders: &[Vec<EClassId>] = &if commutative {
                        vec![
                            node_children.clone(),
                            vec![node_children[1], node_children[0]],
                        ]
                    } else {
                        vec![node_children]
                    };

                    for order in orders {
                        for combo in self.solve_children(ctx, pattern, &children, order, allowed) {
                            let mut b = combo;
                            match b[pattern_node.index()] {
                                Some(existing) if existing != class => continue,
                                _ => b[pattern_node.index()] = Some(class),
                            }
                            results.push(b);
                        }
                    }
                }
                results
            }
        }
    }

    fn solve_children<A>(
        &self,
        ctx: &Context,
        pattern: &Pattern<N, A>,
        pattern_children: &[NodeId],
        class_children: &[EClassId],
        allowed: &dyn Fn(NodeId, EClassId) -> bool,
    ) -> Vec<Vec<Option<EClassId>>> {
        let mut acc: Vec<Vec<Option<EClassId>>> = vec![vec![None; pattern.len()]];
        for (&pc, &cc) in pattern_children.iter().zip(class_children.iter()) {
            let child_solutions = self.solve(ctx, pattern, pc, cc, allowed);
            let mut next = Vec::new();
            for base in &acc {
                for sol in &child_solutions {
                    if let Some(merged) = merge_bindings(base, sol) {
                        next.push(merged);
                    }
                }
            }
            acc = next;
            if acc.is_empty() {
                break;
            }
        }
        acc
    }

    fn boundary_ok<A>(
        &self,
        pattern: &Pattern<N, A>,
        pattern_node: NodeId,
        class: EClassId,
    ) -> bool {
        match pattern.operand_constraint(pattern_node) {
            Some(OperandConstraint::Register) => self
                .nodes(class)
                .iter()
                .any(|&id| !self.dag.get_node(id).is_constant()),
            Some(OperandConstraint::Immediate) => self
                .nodes(class)
                .iter()
                .any(|&id| self.dag.get_node(id).is_constant()),
            None => true,
        }
    }

    fn class_has_leaf(&self, ctx: &Context, class: EClassId) -> bool {
        self.nodes(class)
            .iter()
            .any(|&id| self.dag.get_node(id).is_leaf(ctx))
    }
}

fn merge_bindings(a: &[Option<EClassId>], b: &[Option<EClassId>]) -> Option<Vec<Option<EClassId>>> {
    let mut out = a.to_vec();
    for (slot, &value) in out.iter_mut().zip(b.iter()) {
        match (*slot, value) {
            (Some(x), Some(y)) if x != y => return None,
            (None, Some(y)) => *slot = Some(y),
            _ => {}
        }
    }
    Some(out)
}
