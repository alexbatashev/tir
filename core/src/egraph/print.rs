//! DOT rendering of an [`EGraph`]: each e-class becomes a dotted cluster holding its
//! member e-nodes, and every e-node edge points at the cluster of the child class it
//! references. Useful for diffing the graph before and after saturation.

use std::fmt::Write;
use std::hash::Hash;

use crate::Context;
use crate::graph::{Dag, Matchable};

use super::EGraph;

/// Renders an e-node's display label. Implemented per label type so [`EGPrinter`]
/// stays generic over the graph's `N`/`L`.
pub trait DotLabel<L> {
    fn dot_label(&self, leaf: Option<&L>) -> String;
}

pub struct EGPrinter<'a, N, L> {
    eg: &'a EGraph<N, L>,
}

impl<'a, N: Matchable<Context> + Clone + Eq + Hash + DotLabel<L>, L: Clone + PartialEq>
    EGPrinter<'a, N, L>
{
    pub fn new(eg: &'a EGraph<N, L>) -> Self {
        Self { eg }
    }

    pub fn to_dot(&self) -> String {
        let mut classes: Vec<_> = self.eg.classes().map(|c| self.eg.find(c)).collect();
        classes.sort();
        classes.dedup();

        let mut out = String::new();
        writeln!(out, "digraph egraph {{").unwrap();
        writeln!(out, "  compound=true;").unwrap();
        writeln!(out, "  node [shape=box, style=rounded];").unwrap();

        for &class in &classes {
            writeln!(out, "  subgraph cluster_{} {{", class.index()).unwrap();
            writeln!(out, "    style=dotted;").unwrap();
            let mut nodes = self.eg.nodes(class).to_vec();
            nodes.sort_by_key(|n| n.index());
            for node in nodes {
                let label = self
                    .eg
                    .get_node(node)
                    .dot_label(self.eg.get_leaf_data(node));
                writeln!(out, "    n{} [label={label:?}];", node.index()).unwrap();
            }
            writeln!(out, "  }}").unwrap();
        }

        for &class in &classes {
            for &node in self.eg.nodes(class) {
                for child in self.eg.child_classes(node) {
                    let child = self.eg.find(child);
                    let rep = self.eg.nodes(child)[0];
                    writeln!(
                        out,
                        "  n{} -> n{} [lhead=cluster_{}];",
                        node.index(),
                        rep.index(),
                        child.index()
                    )
                    .unwrap();
                }
            }
        }

        writeln!(out, "}}").unwrap();
        out
    }
}
