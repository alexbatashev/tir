//! Landing a saturated view back on the green layer.
//!
//! A commit is one edit. The extraction names, per class, the cheapest term the
//! view proved equal to what the region held; this module materializes those
//! terms as a fresh λ region and lands it with a single
//! [`mutate::replace_subtree`] of the λ node. Either the whole body is the
//! extraction's or the graph is exactly as it was — mid-materialization there is
//! nothing for another reader to see, because the staged nodes are not reachable
//! from ω until the replacement.
//!
//! What the view anchored is staged verbatim: the state-carrying nodes in their
//! original order, and the subregions of the structural ones copied node for
//! node. Only the pure value terms come from the extraction, and only in the
//! region the view rendered — a nested region is a view of its own, for a commit
//! of its own.

use std::collections::{HashMap, HashSet};

use tir_symbolic::egraph::{Extraction, Id};

use crate::TypeId;
use crate::attributes::{AttributeValue, NamedAttribute};
use crate::sem::node::cost;
use crate::sem::{Kind, SemNode as Node};

use super::Error;
use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::mutate;
use super::view::View;

/// Extract `view` and land it as `lambda`'s new body. Reports the λ that took
/// its place, or `None` when the extraction chose the terms the region already
/// held. A failed commit leaves a staged region that no node owns, so its graph
/// must be discarded rather than used.
pub fn commit(graph: &mut Graph, view: &View, lambda: NodeId) -> Result<Option<NodeId>, Error> {
    let extraction = view.egraph().extract_best(materializable_cost);
    if !view.improved(&extraction) {
        return Ok(None);
    }
    let mut stager = Stager {
        view,
        extraction: &extraction,
        types: view.class_types(),
        moved: HashMap::new(),
        materialized: HashMap::new(),
        active: HashSet::new(),
    };
    stager.stage(graph, lambda).map(Some)
}

/// The extraction cost a commit reads: [`cost`], except that a bare semantic
/// term is unselectable.
///
/// A region holds ops, so [`Stager::build`] rebuilds an op identity, folds a
/// constant, and reads a gate or a leaf back from the origin it stands for —
/// there is nothing it can make of an `Add` or an `Extract`. The view seeds
/// those anyway, unioned onto the very classes the ops occupy, because that is
/// what puts the semantic rulesets on the program's values; here they are
/// equalities to discover through, never a form to land. Selection consumes
/// them directly, so this constraint is a property of *materializing into scf*
/// and lifts where isel emission takes over.
fn materializable_cost(node: &Node) -> u64 {
    match &node.kind {
        // A gate or a leaf names the origin it was seeded from; a literal folds
        // to a `constant`.
        Kind::Sym(_) | Kind::Merge(_) if node.value().is_none() && node.int().is_none() => u64::MAX,
        _ => cost(node),
    }
}

struct Stager<'a> {
    view: &'a View,
    extraction: &'a Extraction<Node>,
    /// The type the region seeded each class at.
    types: HashMap<Id, TypeId>,
    /// Where each origin of the old graph now lives.
    moved: HashMap<Origin, Origin>,
    /// A class is materialized once per type it is read at: a constant carries
    /// no type of its own, so one class can spell values of two widths and
    /// reusing the first origin for the second would retype a value.
    materialized: HashMap<(Id, PortType), Origin>,
    active: HashSet<Id>,
}

impl Stager<'_> {
    fn stage(&mut self, graph: &mut Graph, lambda: NodeId) -> Result<NodeId, Error> {
        let staged = self.stage_body(graph, self.view.region())?;

        let inputs = graph.inputs(lambda).to_vec();
        let outputs = graph.outputs(lambda).to_vec();
        let attributes = graph.attributes(lambda).to_vec();
        let region = graph
            .parent_region(lambda)
            .ok_or_else(|| Error::new("a λ to commit into must belong to a region"))?;
        let position = graph.position(lambda).expect("scheduled node") + 1;
        let replacement = graph.insert_node(
            region,
            position,
            super::kinds::LAMBDA,
            &inputs,
            &outputs,
            &[staged],
            attributes,
        )?;
        mutate::replace_subtree(graph, lambda, replacement)?;
        Ok(replacement)
    }

    /// The viewed region, rebuilt: anchors in their original order, pure terms
    /// from the extraction, materialized just before whatever reads them.
    fn stage_body(&mut self, graph: &mut Graph, old: RegionId) -> Result<RegionId, Error> {
        let arguments = graph.region_arguments(old).to_vec();
        let staged = graph.open_region(&arguments);
        self.map_arguments(old, staged, arguments.len());

        for node in graph.region_nodes(old).to_vec() {
            if self.is_term(graph, node) {
                continue;
            }
            self.stage_node(graph, node, true)?;
        }

        let results = graph
            .region_results(old)
            .to_vec()
            .into_iter()
            .map(|origin| self.resolve(graph, origin))
            .collect::<Result<Vec<_>, _>>()?;
        graph.close_region(staged, &results)?;
        Ok(staged)
    }

    /// Whether the view models everything `node` produces, so the extraction —
    /// not the copy — is what rebuilds it.
    fn is_term(&self, graph: &Graph, node: NodeId) -> bool {
        graph.subregions(node).is_empty()
            && !graph.outputs(node).is_empty()
            && (0..graph.outputs(node).len())
                .all(|port| self.view.models(Origin::output(node, port as u32)))
    }

    fn map_arguments(&mut self, old: RegionId, staged: RegionId, count: usize) {
        for index in 0..count as u32 {
            self.moved.insert(
                Origin::argument(old, index),
                Origin::argument(staged, index),
            );
        }
    }

    /// Re-create one node the view did not model, and every region it owns. Its
    /// inputs come from the extraction only in the region the view rendered;
    /// inside a copied region every origin is read from the copy.
    fn stage_node(&mut self, graph: &mut Graph, node: NodeId, viewed: bool) -> Result<(), Error> {
        let inputs = graph
            .inputs(node)
            .to_vec()
            .into_iter()
            .map(|origin| match viewed {
                true => self.resolve(graph, origin),
                false => self.moved_origin(origin),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let regions = graph
            .subregions(node)
            .to_vec()
            .into_iter()
            .map(|region| self.copy_region(graph, region))
            .collect::<Result<Vec<_>, _>>()?;
        let outputs = graph.outputs(node).to_vec();
        let attributes = graph.attributes(node).to_vec();
        let staged = graph.add_node(graph.op_of(node), &inputs, &outputs, &regions, attributes)?;
        for port in 0..outputs.len() as u32 {
            self.moved
                .insert(Origin::output(node, port), Origin::output(staged, port));
        }
        Ok(())
    }

    /// Copy a region the view did not render, node for node.
    fn copy_region(&mut self, graph: &mut Graph, old: RegionId) -> Result<RegionId, Error> {
        let arguments = graph.region_arguments(old).to_vec();
        let staged = graph.open_region(&arguments);
        self.map_arguments(old, staged, arguments.len());
        for node in graph.region_nodes(old).to_vec() {
            self.stage_node(graph, node, false)?;
        }
        let results = graph
            .region_results(old)
            .to_vec()
            .into_iter()
            .map(|origin| self.moved_origin(origin))
            .collect::<Result<Vec<_>, _>>()?;
        graph.close_region(staged, &results)?;
        Ok(staged)
    }

    /// Where an origin the staged graph must read now lives: a modeled value
    /// comes from the extraction, anything else from the copy.
    fn resolve(&mut self, graph: &mut Graph, origin: Origin) -> Result<Origin, Error> {
        match self.view.class(origin) {
            Some(class) => {
                let ty = graph.origin_type(origin);
                self.materialize(graph, class, ty)
            }
            None => self.moved_origin(origin),
        }
    }

    fn moved_origin(&self, origin: Origin) -> Result<Origin, Error> {
        self.moved
            .get(&origin)
            .copied()
            .ok_or_else(|| Error::new(format!("origin {origin:?} was not staged before its use")))
    }

    /// Build the extraction's choice for `class`, memoized per class so a shared
    /// subterm is built once. `expected` types a constant, which carries none:
    /// the type the region seeded the class at when it has one, since an
    /// operation's operands need not share its result type, and the reader's own
    /// type otherwise.
    fn materialize(
        &mut self,
        graph: &mut Graph,
        class: Id,
        expected: PortType,
    ) -> Result<Origin, Error> {
        let class = self.view.egraph().find(class);
        let expected = self
            .types
            .get(&class)
            .map_or(expected, |&ty| PortType::Value(ty));
        if let Some(&origin) = self.materialized.get(&(class, expected)) {
            return Ok(origin);
        }
        if !self.active.insert(class) {
            return Err(Error::new(
                "the extracted term references itself; no finite subtree spells it",
            ));
        }
        let node = self
            .extraction
            .node(class)
            .ok_or_else(|| Error::new("an extracted class has no representative node"))?
            .clone();
        let origin = self.build(graph, &node, expected)?;
        self.active.remove(&class);
        self.materialized.insert((class, expected), origin);
        Ok(origin)
    }

    fn build(
        &mut self,
        graph: &mut Graph,
        node: &Node,
        expected: PortType,
    ) -> Result<Origin, Error> {
        // A gate or a leaf is never rebuilt: it stands for the origin it was
        // seeded from, which the staged body already holds.
        if let Some(value) = node.value() {
            let anchor = self.view.anchor(value).ok_or_else(|| {
                Error::new("an extracted leaf stands for no origin of the region")
            })?;
            return self.moved_origin(anchor);
        }
        match &node.kind {
            Kind::Ir(op) => {
                let ty = node.ty.expect("an op term carries its result type");
                let output = PortType::Value(ty);
                let inputs = node
                    .children
                    .iter()
                    .map(|&arg| self.materialize(graph, arg, output))
                    .collect::<Result<Vec<_>, _>>()?;
                let op_type = graph.op_type(op.dialect, op.name);
                let node = graph.add_node(op_type, &inputs, &[output], &[], op.attrs.clone())?;
                Ok(Origin::output(node, 0))
            }
            Kind::Sym(_) | Kind::Merge(_) => {
                let literal = node
                    .int()
                    .ok_or_else(|| Error::new("an extracted term stands for no origin"))?;
                let op = graph.op_type("builtin", "constant");
                let attributes = vec![NamedAttribute::new(
                    "value",
                    AttributeValue::Int(literal.to_i64()),
                )];
                let node = graph.add_node(op, &[], &[expected], &[], attributes)?;
                Ok(Origin::output(node, 0))
            }
        }
    }
}
