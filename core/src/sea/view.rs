//! The e-graph view over one sea region.
//!
//! The view is red: it renders a region's *pure value terms* as an e-graph and
//! nothing else. What it does not model, it anchors — a state-typed edge, an
//! operation that touches one, a structural node it may not speculate through.
//! An anchor is an opaque leaf standing for one origin of the region, so the
//! schedule the state edges express is never something saturation can reorder.
//!
//! # What becomes a term
//!
//! * A **region argument** of value type is a leaf.
//! * A **simple node** with no state port is its operator over its operands'
//!   classes — a constant-like one is the constant it holds, so equal constants
//!   share a class whatever op spells them.
//! * A **γ** contributes one value-level `If(predicate, then, else)` per output
//!   port whose computing slice is pure. Purity is the speculation licence — the
//!   term evaluates both arms — and it is asked per port, not per γ: a port an
//!   arm computes off the state chain stays an anchor while its siblings gate.
//! * A **θ** with a pure body contributes the per-value `Theta(init, latch)`
//!   projection of one loop variable, built by the placeholder-union
//!   construction: a leaf stands for the loop-*carried* value while the body is
//!   seeded over it, then the real projection is unioned onto it. The value the
//!   loop leaves behind is a different value, so the θ's output anchors and never
//!   joins that class. The θ node itself is never a term; only its loop variables
//!   are.
//!
//! # Anchors are synthetic values
//!
//! A term's payloads name live IR, which the green layer has none of, so the view
//! mints one synthetic [`crate::Value`] per anchored origin: it carries the
//! origin's type — which is what the rules read a class's width from — and its
//! identity separates anchors that must not merge. [`View::anchor`] takes an
//! extracted leaf back to the origin it stands for.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tir_symbolic::egraph::{EGraph, Extraction, Id};

use crate::passes::instcombine::value_rewrites;
use crate::sem::{Prov, SemNode as Node};
use crate::{Commutative, ConstantLike, Context, OpCost, OpInstance, TypeId, ValueId};

use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::kinds;

const ITER_LIMIT: usize = 30;
const NODE_LIMIT: usize = 100_000;

/// The e-graph rendering of one region, valid for the version of that region's
/// owner it was built at.
pub struct View {
    region: RegionId,
    version: u32,
    eg: EGraph<Node>,
    class_of: HashMap<Origin, Id>,
    /// The origins the view gave up on, kept opaque so nothing merges through
    /// them.
    anchored: HashSet<Origin>,
    /// The origin every leaf stands for, keyed by its synthetic value.
    anchor_of: HashMap<ValueId, Origin>,
    /// What each origin of the viewed region itself seeded, so a commit can tell
    /// an extraction that changed something from one that chose the same terms.
    /// A subregion's origins are not listed: they are only there to spell the
    /// gates, and a commit rebuilds no region but the one it views.
    seeded: Vec<(Origin, Node)>,
    /// The type every seeding of a class read it at, before saturation moved the
    /// class ids around.
    types: Vec<(Id, TypeId)>,
}

impl View {
    /// Render `region` — the pure value terms of it — as an e-graph.
    pub fn build(context: &Context, graph: &Graph, region: RegionId) -> Self {
        let mut builder = Builder {
            context,
            graph,
            eg: EGraph::new(),
            class_of: HashMap::new(),
            anchored: HashSet::new(),
            anchor_of: HashMap::new(),
            seeded: Vec::new(),
            types: Vec::new(),
            nesting: 0,
        };
        builder.seed_arguments(region);
        for &node in graph.region_nodes(region) {
            builder.seed_node(node);
        }
        View {
            region,
            version: version_of(graph, region),
            eg: builder.eg,
            class_of: builder.class_of,
            anchored: builder.anchored,
            anchor_of: builder.anchor_of,
            seeded: builder.seeded,
            types: builder.types,
        }
    }

    pub fn region(&self) -> RegionId {
        self.region
    }

    /// The version of the region's owner this view renders.
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn egraph(&self) -> &EGraph<Node> {
        &self.eg
    }

    /// The class an origin of the viewed region belongs to, or `None` for the
    /// state edges, which are not terms.
    pub fn class(&self, origin: Origin) -> Option<Id> {
        self.class_of.get(&origin).map(|&id| self.eg.find(id))
    }

    /// The origin a leaf stands for.
    pub fn anchor(&self, value: ValueId) -> Option<Origin> {
        self.anchor_of.get(&value).copied()
    }

    /// The type each class was seeded at, canonicalized against the unions
    /// saturation made. A class every seeding agrees on types the values a commit
    /// materializes for it — a constant carries no type of its own, and the
    /// operation reading it need not have its operands' type. A class two
    /// seedings disagree on is left untyped rather than guessed at.
    pub fn class_types(&self) -> HashMap<Id, TypeId> {
        let mut types: HashMap<Id, Option<TypeId>> = HashMap::new();
        for &(id, ty) in &self.types {
            types
                .entry(self.eg.find(id))
                .and_modify(|known| {
                    if *known != Some(ty) {
                        *known = None;
                    }
                })
                .or_insert(Some(ty));
        }
        types
            .into_iter()
            .filter_map(|(id, ty)| ty.map(|ty| (id, ty)))
            .collect()
    }

    /// Whether the view renders `origin` as a term. An anchored origin has a
    /// class too — the opaque leaf standing for it — but the node producing it
    /// is the schedule, not something an extraction may rebuild.
    pub fn models(&self, origin: Origin) -> bool {
        self.class_of.contains_key(&origin) && !self.anchored.contains(&origin)
    }

    /// Saturate with the proved rewrites. Between this and a commit the view is
    /// not IR: a class holds every form its term was proved equal to.
    pub fn saturate(&mut self, context: &Context) {
        let rewrites = value_rewrites(context);
        self.eg.saturate(&rewrites, ITER_LIMIT, NODE_LIMIT);
    }

    /// Whether `extraction` chose, for any origin, a different term than the one
    /// the region seeded. A commit that would rebuild the region unchanged is
    /// not worth the edit.
    pub fn improved(&self, extraction: &Extraction<Node>) -> bool {
        self.seeded.iter().any(|(origin, seeded)| {
            let Some(class) = self.class(*origin) else {
                return false;
            };
            match extraction.node(class) {
                Some(chosen) => self.canonical_key(chosen) != self.canonical_key(seeded),
                None => false,
            }
        })
    }

    /// A node's hash with its children canonicalized, so a term seeded before a
    /// union compares equal to the same term read back after one.
    fn canonical_key(&self, node: &Node) -> u64 {
        use tir_symbolic::egraph::ENode;
        let mut node = node.clone();
        for child in node.children_mut() {
            *child = self.eg.find(*child);
        }
        node.hash_cons()
    }
}

/// A region's key: the version stamp of the structural node that owns it. Every
/// mutation bumps the owners on the spine from the region it edited up to ω, so
/// this changes exactly when the region — or something containing it — changed.
fn version_of(graph: &Graph, region: RegionId) -> u32 {
    graph.region_owner(region).map_or(0, |o| graph.version(o))
}

/// Views keyed by `(region, version)`. The key is region-local, so a cache
/// belongs to one graph: region ids are dense per graph and mean nothing across
/// two of them.
#[derive(Default)]
pub struct ViewCache {
    entries: HashMap<RegionId, View>,
}

impl ViewCache {
    /// The view of `region`, revalidated against its owner's version stamp: an
    /// edit elsewhere in the graph leaves it warm, an edit reaching this region
    /// rebuilds it.
    pub fn view(&mut self, context: &Context, graph: &Graph, region: RegionId) -> &mut View {
        let version = version_of(graph, region);
        let stale = self
            .entries
            .get(&region)
            .is_none_or(|view| view.version != version);
        if stale {
            self.entries
                .insert(region, View::build(context, graph, region));
        }
        self.entries.get_mut(&region).expect("just inserted")
    }
}

struct Builder<'a> {
    context: &'a Context,
    graph: &'a Graph,
    eg: EGraph<Node>,
    class_of: HashMap<Origin, Id>,
    anchored: HashSet<Origin>,
    anchor_of: HashMap<ValueId, Origin>,
    seeded: Vec<(Origin, Node)>,
    types: Vec<(Id, TypeId)>,
    /// How deep below the viewed region the seeding currently is.
    nesting: u32,
}

impl Builder<'_> {
    fn seed_arguments(&mut self, region: RegionId) {
        for (index, &port) in self.graph.region_arguments(region).iter().enumerate() {
            if let PortType::Value(ty) = port {
                self.anchor(Origin::argument(region, index as u32), ty);
            }
        }
    }

    fn seed_node(&mut self, node: NodeId) {
        let op = self.graph.op_of(node);
        if op == kinds::GAMMA && self.seed_gamma(node) {
            return;
        }
        if op == kinds::THETA && self.seed_theta(node) {
            return;
        }
        if !kinds::is_structural(op) && self.is_pure(node) {
            self.seed_simple(node);
            return;
        }
        self.anchor_outputs(node);
    }

    /// A node whose result the e-graph may reason about: one value output, no
    /// state port, no regions. State is what orders everything else, and the
    /// view never reorders it.
    fn is_pure(&self, node: NodeId) -> bool {
        self.graph.subregions(node).is_empty()
            && matches!(self.graph.outputs(node), [PortType::Value(_)])
            && self
                .graph
                .input_types(node)
                .iter()
                .all(|port| port.value_type().is_some())
    }

    /// The type of a pure node's single value output.
    fn output_type(&self, node: NodeId) -> TypeId {
        self.graph.outputs(node)[0]
            .value_type()
            .expect("a pure node produces one value")
    }

    fn anchor_outputs(&mut self, node: NodeId) {
        for (port, &output) in self.graph.outputs(node).to_vec().iter().enumerate() {
            if let PortType::Value(ty) = output {
                self.anchor(Origin::output(node, port as u32), ty);
            }
        }
    }

    /// Seed `origin` as an opaque leaf of type `ty`, and remember which origin it
    /// stands for.
    fn anchor(&mut self, origin: Origin, ty: TypeId) -> Id {
        let value = self.context.create_value(ty, None).id();
        let node = Node::input(value);
        let id = self.eg.add(node.clone());
        self.anchor_of.insert(value, origin);
        self.anchored.insert(origin);
        self.record(origin, id, ty, node);
        id
    }

    fn record(&mut self, origin: Origin, id: Id, ty: TypeId, node: Node) {
        self.class_of.insert(origin, id);
        self.types.push((id, ty));
        if self.nesting == 0 {
            self.seeded.push((origin, node));
        }
    }

    /// Seed the nodes of a subregion, whose origins spell the gates but are not
    /// the viewed region's own.
    fn seed_nested(&mut self, region: RegionId) {
        self.nesting += 1;
        for &node in self.graph.region_nodes(region) {
            self.seed_node(node);
        }
        self.nesting -= 1;
    }

    fn seed_simple(&mut self, node: NodeId) {
        let ty = self.output_type(node);
        let probe = self.probe(node, ty);
        let seeded = match probe.clone().as_interface::<dyn ConstantLike>() {
            Some(constant) => Node::constant(constant.constant_value(), Prov::None),
            None => {
                let Some(mut args) = self.operand_classes(node) else {
                    self.anchor_outputs(node);
                    return;
                };
                let commutative = probe.has_interface::<dyn Commutative>();
                if commutative {
                    args.sort_by_key(|id| id.index());
                }
                let cost = probe
                    .clone()
                    .as_interface::<dyn OpCost>()
                    .map_or(1, |c| c.cost());
                let (dialect, name) = self.graph.op_type_name(self.graph.op_of(node));
                Node::sea(
                    dialect,
                    name,
                    ty,
                    self.graph.attributes(node).to_vec(),
                    commutative,
                    cost,
                    args,
                )
            }
        };
        let id = self.eg.add(seeded.clone());
        self.record(Origin::output(node, 0), id, ty, seeded);
    }

    /// A detached instance of the node's op, carrying its attributes and a value
    /// of its result type. Interfaces are registered per `(dialect, name)`, so
    /// this is how the green layer asks an op what it is without an IR to ask in.
    fn probe(&self, node: NodeId, ty: TypeId) -> Arc<OpInstance> {
        let identity = self.graph.op_type_name(self.graph.op_of(node));
        let result = self.context.create_value(ty, None).id();
        Arc::new(OpInstance::new_dynamic(
            identity,
            self.context.as_context_ref(),
            Vec::new(),
            vec![result],
            Vec::new(),
            self.graph.attributes(node).to_vec(),
        ))
    }

    fn operand_classes(&self, node: NodeId) -> Option<Vec<Id>> {
        self.graph
            .inputs(node)
            .iter()
            .map(|origin| self.class_of.get(origin).copied())
            .collect()
    }

    /// Every node of `region` is a pure simple node, so a term over the region's
    /// arguments is a term the view may evaluate unconditionally.
    fn region_is_pure(&self, region: RegionId) -> bool {
        self.graph
            .region_nodes(region)
            .iter()
            .all(|&node| !kinds::is_structural(self.graph.op_of(node)) && self.is_pure(node))
    }

    /// Whether the slice of `arm` computing its `port`-th result is speculatable:
    /// every node that result reaches is a pure simple node, so evaluating it on
    /// the path the γ did not take costs a computation and nothing else. What the
    /// arm's other nodes do is not this port's business.
    fn slice_is_pure(&self, arm: RegionId, port: usize) -> bool {
        let mut pending = vec![self.graph.region_results(arm)[port]];
        let mut seen = HashSet::new();
        while let Some(origin) = pending.pop() {
            let Some(node) = origin.node() else {
                continue;
            };
            if !seen.insert(node) {
                continue;
            }
            if kinds::is_structural(self.graph.op_of(node)) || !self.is_pure(node) {
                return false;
            }
            pending.extend(self.graph.inputs(node).iter().copied());
        }
        true
    }

    /// Bind a subregion's arguments to the classes its owner's inputs carry, then
    /// seed its nodes into the same e-graph.
    fn inline(&mut self, region: RegionId, arguments: &[Origin]) {
        for (index, origin) in arguments.iter().enumerate() {
            if let Some(&class) = self.class_of.get(origin) {
                self.class_of
                    .insert(Origin::argument(region, index as u32), class);
            }
        }
        self.seed_nested(region);
    }

    /// The speculation licence is per output port: a port's term evaluates the
    /// slice of each arm that computes *that* result, so a γ whose other ports
    /// touch state still gates the ones that do not. Reports whether it was
    /// modeled; anything else falls back to an anchor.
    fn seed_gamma(&mut self, node: NodeId) -> bool {
        let regions = self.graph.subregions(node).to_vec();
        let [otherwise, taken] = regions[..] else {
            return false;
        };
        let inputs = self.graph.inputs(node).to_vec();
        let Some(&condition) = self.class_of.get(&inputs[0]) else {
            return false;
        };
        self.inline(taken, &inputs[1..]);
        self.inline(otherwise, &inputs[1..]);

        for (port, &output) in self.graph.outputs(node).to_vec().iter().enumerate() {
            let PortType::Value(ty) = output else {
                continue;
            };
            let origin = Origin::output(node, port as u32);
            let arms = [taken, otherwise].map(|arm| {
                let result = self.graph.region_results(arm)[port];
                self.slice_is_pure(arm, port)
                    .then(|| self.class_of.get(&result).copied())
                    .flatten()
            });
            let [Some(then), Some(otherwise)] = arms else {
                self.anchor(origin, ty);
                continue;
            };
            let value = self.context.create_value(ty, None).id();
            // The provenance names the origin the gate stands for; the condition
            // is child 0, which is what matching and extraction read.
            let gate = Node::gamma(value, vec![condition, then, otherwise]);
            let id = self.eg.add(gate.clone());
            self.anchor_of.insert(value, origin);
            self.record(origin, id, ty, gate);
        }
        true
    }

    /// A θ over a pure body projects each loop variable as `Theta(init, latch)`,
    /// built placeholder-first so the latch term may reference the class it
    /// defines. Reports whether it was modeled.
    ///
    /// The placeholder stands for the value the body *carries*, never for the one
    /// the loop leaves behind: the θ output is the last iteration's, so seeding
    /// both into one class would prove a final value congruent to a carried one.
    /// The output anchors instead, and no union ever brings the two together.
    fn seed_theta(&mut self, node: NodeId) -> bool {
        let regions = self.graph.subregions(node).to_vec();
        let [body] = regions[..] else {
            return false;
        };
        if !self.region_is_pure(body) {
            return false;
        }
        let outputs = self.graph.outputs(node).to_vec();
        let mut placeholders = Vec::new();
        for (port, &output) in outputs.iter().enumerate() {
            let PortType::Value(ty) = output else {
                continue;
            };
            self.anchor(Origin::output(node, port as u32), ty);
            let value = self.context.create_value(ty, None).id();
            let id = self.eg.add(Node::input(value));
            self.class_of
                .insert(Origin::argument(body, port as u32), id);
            self.types.push((id, ty));
            placeholders.push((port, value, id));
        }
        self.seed_nested(body);

        let inputs = self.graph.inputs(node).to_vec();
        let results = self.graph.region_results(body).to_vec();
        for (port, value, placeholder) in placeholders {
            // The θ region's first result is the continuation predicate, so the
            // latch of port `p` is result `p + 1`.
            let latched = self.class_of.get(&results[port + 1]).copied();
            let (Some(init), Some(latch)) = (self.class_of.get(&inputs[port]).copied(), latched)
            else {
                continue;
            };
            let mu = self.eg.add(Node::theta(value, init, latch));
            self.eg.union(placeholder, mu);
        }
        self.eg.rebuild();
        true
    }
}
