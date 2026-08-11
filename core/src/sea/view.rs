//! The e-graph view over one sea region.
//!
//! The view is red: it renders a region's value terms and its memory accesses as
//! an e-graph, and nothing else. What it does not model, it anchors — a chain it
//! cannot spell, an operation that consumes one opaquely, a structural node it
//! may not speculate through. An anchor is an opaque leaf standing for one origin
//! of the region, so nothing ever merges through it.
//!
//! # What becomes a term
//!
//! * A **region argument** is a leaf: of its type where it carries a value, and
//!   untyped where it carries a state chain.
//! * A **memory access** is its operator over its operands *and the chain it
//!   reads* ([`Builder::seed_memory`]), in both vocabularies at once — the
//!   `LoadMemory`/`StoreMemory` shape the state laws are written in
//!   ([`super::state`]) and the operation's own identity, which a commit
//!   rebuilds. The state operand is the whole of memory identity: two accesses
//!   merge exactly where the chain says they must.
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
//!   are. Both a γ and a θ hand back their chains opaque: what happened on them
//!   happened on one path, or some number of times, so the accesses *after* one
//!   are still terms — over something nothing forwards through.
//!
//! Every one of those terms is seeded *twice* where the op says what it
//! computes: once as the op's identity, which the peephole rules match, and
//! once as the semantic expansion the selection theory is written in, unioned
//! onto the same class ([`super::sym`]). A rule keyed on either vocabulary then
//! fires on the same value. Both carry the port's IR type verbatim — the
//! width-dependent axioms read a class's width off it — and so do the gates and
//! the leaves.
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

use crate::attributes::{AttributeValue, NamedAttribute};
use crate::builtin::UnitType;
use crate::passes::instcombine::value_rewrites;
use crate::scoped_attr::AttributeDict;
use crate::sem::egraph::{minimal_unsigned_apint, type_width};
use crate::sem::rewrites::{IselRewrite, SaturationLimits, discover_rewrites, saturate};
use crate::sem::{Prov, SemNode as Node, SymKind, template_node};
use crate::{
    Commutative, ConstantLike, Context, DATA_LAYOUT, MemoryRead, MemoryWrite, OpCost, OpInstance,
    TypeId, ValueId,
};

use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::{kinds, sym};

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
    /// The state classes something outside the modeled terms reads: the chains
    /// the region exports, and the ones a node the view did not model consumes.
    /// A law that would drop a write may not touch these ([`super::state`]).
    escaping: Vec<Id>,
}

impl View {
    /// Render `region` — the pure value terms of it — as an e-graph. `layout` is
    /// the data layout in scope where the region's λ came from: an op whose
    /// semantics are width-dependent reads it, and the green layer has no IR
    /// scope to resolve one from (see [`Builder::probe`]).
    pub fn build(
        context: &Context,
        graph: &Graph,
        region: RegionId,
        layout: Option<&AttributeDict>,
    ) -> Self {
        let mut builder = Builder {
            context,
            graph,
            layout,
            eg: EGraph::new(),
            class_of: HashMap::new(),
            anchored: HashSet::new(),
            anchor_of: HashMap::new(),
            seeded: Vec::new(),
            types: Vec::new(),
            escaping: Vec::new(),
            nesting: 0,
        };
        builder.seed_arguments(region);
        for &node in graph.region_nodes(region) {
            builder.seed_node(node);
        }
        builder.record_exports(region);
        View {
            region,
            version: version_of(graph, region),
            eg: builder.eg,
            class_of: builder.class_of,
            anchored: builder.anchored,
            anchor_of: builder.anchor_of,
            seeded: builder.seeded,
            types: builder.types,
            escaping: builder.escaping,
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

    /// Give up the view and keep its e-graph. A consumer that saturates with its
    /// own ruleset and never commits — instruction selection, which covers the
    /// classes rather than extracting one term per class — owns the graph from
    /// there on; the readings it needs first ([`View::class`], [`View::anchor`])
    /// are taken while the view is still whole.
    pub fn into_egraph(self) -> EGraph<Node> {
        self.eg
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
    ///
    /// Both rulesets run in one driver. The peephole rules are keyed on the op
    /// identity, the selection axioms on the semantic vocabulary, and the view
    /// seeds a term in both — so a form either discovers is one the other may
    /// match next iteration, which two sequential saturations would not give.
    pub fn saturate(&mut self, context: &Context) {
        let mut rewrites: Vec<IselRewrite> =
            value_rewrites(context).into_iter().map(peephole).collect();
        rewrites.extend(discover_rewrites());
        rewrites.extend(super::state::state_laws(self.escaping.clone()));
        saturate(
            context,
            &mut self.eg,
            &rewrites,
            SaturationLimits::default(),
        );
    }

    /// Every form the class was proved to hold, canonicalized. Extraction names
    /// one winner per class; a covering pass chooses among all of them, and this
    /// is what it reads.
    pub fn candidates(&self, class: Id) -> impl Iterator<Item = &Node> {
        self.eg.nodes(self.eg.find(class)).iter()
    }

    /// Whether `extraction` chose, for any origin, a different term than the one
    /// the region seeded. A commit that would rebuild the region unchanged is
    /// not worth the edit.
    pub fn improved(&self, extraction: &Extraction<Node>) -> bool {
        self.seeded
            .iter()
            .any(|(origin, _)| !self.rebuilt(*origin, extraction))
    }

    /// Whether `extraction` chose, for `origin`, the very term the region seeded
    /// it as. A commit rebuilds the operation behind an origin only where this
    /// holds: an origin the extraction reads back as something else has no
    /// operation left to land — the forwarded load, the overwritten store.
    pub fn rebuilt(&self, origin: Origin, extraction: &Extraction<Node>) -> bool {
        let Some((_, seeded)) = self.seeded.iter().find(|(seeded, _)| *seeded == origin) else {
            return false;
        };
        let Some(class) = self.class(origin) else {
            return false;
        };
        match extraction.node(class) {
            Some(chosen) => self.canonical_key(chosen) == self.canonical_key(seeded),
            None => true,
        }
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

/// A peephole rule as the saturation driver's rewrite: the same searcher over
/// the same vocabulary, applied by the engine's own right-hand side. The two
/// rulesets are declared in different shapes, not driven by different engines.
fn peephole(rule: crate::passes::instcombine::rules::Rule) -> IselRewrite {
    IselRewrite {
        name: rule.name.clone(),
        searcher: rule.lhs.clone(),
        apply: Box::new(move |_, eg, matched| rule.apply_match(eg, matched)),
        post_saturation: false,
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
    pub fn view(
        &mut self,
        context: &Context,
        graph: &Graph,
        region: RegionId,
        layout: Option<&AttributeDict>,
    ) -> &mut View {
        let version = version_of(graph, region);
        let stale = self
            .entries
            .get(&region)
            .is_none_or(|view| view.version != version);
        if stale {
            self.entries
                .insert(region, View::build(context, graph, region, layout));
        }
        self.entries.get_mut(&region).expect("just inserted")
    }
}

/// A stateful node's ports, split into what it computes and the chain it
/// threads ([`Builder::memory_shape`]).
struct MemoryShape {
    /// The index of the state operand in the node's input list.
    state_input: usize,
    /// The origin of the chain the node produces.
    state_output: Origin,
    /// The types of the values the node computes, in port order.
    results: Vec<TypeId>,
}

struct Builder<'a> {
    context: &'a Context,
    graph: &'a Graph,
    /// The data layout the probes declare, so a width-dependent op's semantics
    /// resolve one.
    layout: Option<&'a AttributeDict>,
    eg: EGraph<Node>,
    class_of: HashMap<Origin, Id>,
    anchored: HashSet<Origin>,
    anchor_of: HashMap<ValueId, Origin>,
    seeded: Vec<(Origin, Node)>,
    types: Vec<(Id, TypeId)>,
    escaping: Vec<Id>,
    /// How deep below the viewed region the seeding currently is.
    nesting: u32,
}

impl Builder<'_> {
    fn seed_arguments(&mut self, region: RegionId) {
        for (index, &port) in self.graph.region_arguments(region).iter().enumerate() {
            let origin = Origin::argument(region, index as u32);
            match port {
                PortType::Value(ty) => self.anchor(origin, ty),
                PortType::State => self.anchor_state(origin),
            };
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
        if !kinds::is_structural(op) {
            if self.is_pure(node) {
                self.seed_simple(node);
                return;
            }
            if self.seed_memory(node) {
                return;
            }
        }
        self.escape_states(node);
        self.anchor_outputs(node);
    }

    /// Every chain `node` reads escapes the terms: whatever the node does with
    /// it, the view does not model, so no law may drop a write it can see.
    fn escape_states(&mut self, node: NodeId) {
        for (port, origin) in self.graph.inputs(node).to_vec().into_iter().enumerate() {
            if self.graph.input_types(node)[port] == PortType::State
                && let Some(&class) = self.class_of.get(&origin)
            {
                self.escaping.push(class);
            }
        }
    }

    /// The chains the region hands back to its caller.
    fn record_exports(&mut self, region: RegionId) {
        for (port, &origin) in self
            .graph
            .region_results(region)
            .to_vec()
            .iter()
            .enumerate()
        {
            if self.graph.region_result_types(region)[port] == PortType::State
                && let Some(&class) = self.class_of.get(&origin)
            {
                self.escaping.push(class);
            }
        }
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
            let origin = Origin::output(node, port as u32);
            match output {
                PortType::Value(ty) => self.anchor(origin, ty),
                PortType::State => self.anchor_state(origin),
            };
        }
    }

    /// Seed a state edge as an opaque leaf. A chain the view cannot spell — the
    /// one a call or a `memset` produces — still has a class, so the operations
    /// downstream of it are terms over *something*; nothing merges through it,
    /// which is what makes the fallback conservative rather than wrong.
    ///
    /// A state edge carries no value, so the leaf is untyped: the width-dependent
    /// axioms read a class's type, and a chain has none to read.
    fn anchor_state(&mut self, origin: Origin) -> Id {
        let value = self
            .context
            .create_value(UnitType::new(self.context), None)
            .id();
        let node = Node::input(value);
        let id = self.eg.add(node.clone());
        self.anchor_of.insert(value, origin);
        self.anchored.insert(origin);
        self.class_of.insert(origin, id);
        if self.nesting == 0 {
            self.seeded.push((origin, node));
        }
        id
    }

    /// Seed `origin` as an opaque leaf of type `ty`, and remember which origin it
    /// stands for.
    fn anchor(&mut self, origin: Origin, ty: TypeId) -> Id {
        let value = self.context.create_value(ty, None).id();
        let node = Node::input(value).typed(ty);
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
        let probe = self.probe(node, &[ty]);
        let Some(operands) = self.operand_classes(node) else {
            self.anchor_outputs(node);
            return;
        };
        let seeded = match probe.clone().as_interface::<dyn ConstantLike>() {
            Some(constant) => Node::constant(constant.constant_value(), Prov::None),
            None => {
                let mut args = operands.clone();
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
        // The op's declared semantics name the same value, so both vocabularies
        // land on one class (see [`super::sym`]). The operands go in unsorted:
        // an expansion reads input `i` by position, whatever order the
        // commutative identity was spelled in.
        if let Some(expansion) = sym::expand(self.context, &mut self.eg, &probe, &operands) {
            self.eg.union(id, expansion);
        }
        self.record(Origin::output(node, 0), id, ty, seeded);
    }

    /// A detached instance of the node's op, carrying its attributes, a value of
    /// its result type and one of each input's. Interfaces are registered per
    /// `(dialect, name)`, so this is how the green layer asks an op what it is
    /// without an IR to ask in — and an op whose semantics are a function of its
    /// own signature (`vector.add`, whose lane count is a static constant or its
    /// `vl` operand depending on whether it has one) reads that signature off
    /// the instance, so the probe carries the node's whole port list, not only
    /// its result.
    ///
    /// A detached instance belongs to no scope, so the layout the viewed λ was
    /// raised under rides along as the probe's own `data_layout` — otherwise an
    /// op whose semantics are the pointer width (`ptr.null`, `ptr.cmp`) would
    /// declare none and seed nothing.
    fn probe(&self, node: NodeId, results: &[TypeId]) -> Arc<OpInstance> {
        let identity = self.graph.op_type_name(self.graph.op_of(node));
        let operands: Vec<ValueId> = self
            .graph
            .input_types(node)
            .into_iter()
            .filter_map(|port| port.value_type())
            .map(|ty| self.context.create_value(ty, None).id())
            .collect();
        let results: Vec<ValueId> = results
            .iter()
            .map(|&ty| self.context.create_value(ty, None).id())
            .collect();
        let mut attributes = self.graph.attributes(node).to_vec();
        if let Some(layout) = self.layout {
            attributes.push(NamedAttribute::new(
                DATA_LAYOUT,
                AttributeValue::Dict(layout.clone()),
            ));
        }
        Arc::new(OpInstance::new_dynamic(
            identity,
            self.context.as_context_ref(),
            operands,
            results,
            Vec::new(),
            attributes,
        ))
    }

    /// Seed a memory operation as a term over the state it reads.
    ///
    /// A load is `LoadMemory(address, bytes, metadata, state)` and a store
    /// `StoreMemory(address, bytes, value, space, state)` — the target
    /// vocabulary's shapes with the chain appended, which is the *whole* of
    /// memory identity here. Two loads of one address on one state class
    /// hash-cons, and that congruence is load-over-store forwarding's other
    /// half; two on different chains, or split by a store, never do. The
    /// operation's own identity is seeded over the same operands and unioned
    /// on, so a commit has an op to rebuild ([`super::commit`]).
    ///
    /// Which of a node's ports is which comes from the interfaces, not from a
    /// position: [`MemoryRead::read_location`] and [`MemoryWrite::write_location`]
    /// name operands of the probe, and the operand order is the node's.
    ///
    /// A read does not change memory, so a load's outgoing chain is the class of
    /// its incoming one — a store's forwarding reaches past an intervening load.
    /// Nothing reorders on that account: the schedule a commit lands is the
    /// region's own node order, never the state DAG's topology.
    ///
    /// Reports whether the node was modeled. A stateful operation the memory
    /// vocabulary cannot spell whole — a `memset`, whose written extent is an
    /// operand rather than its value's width — anchors, and the chain it
    /// produces blocks forwarding through it.
    fn seed_memory(&mut self, node: NodeId) -> bool {
        let Some(shape) = self.memory_shape(node) else {
            return false;
        };
        let probe = self.probe(node, &shape.results);
        let Some(operands) = self.operand_classes(node) else {
            return false;
        };
        let state = operands[shape.state_input];
        let values = &operands[..shape.state_input];

        let read = probe.clone().as_interface::<dyn MemoryRead>();
        let write = probe.clone().as_interface::<dyn MemoryWrite>();
        let (location, stored, accessed) = match (&read, &write) {
            (Some(read), _) => (read.read_location(), None, shape.results.first().copied()),
            (_, Some(write)) => (
                write.write_location(),
                Some(write.written_value()),
                Some(self.context.get_value(write.written_value()).ty()),
            ),
            _ => return false,
        };
        let (Some(address), Some(accessed)) = (
            self.operand_class(&probe, values, location),
            accessed.and_then(|ty| type_width(self.context, ty)),
        ) else {
            return false;
        };

        let bytes = self.constant(u64::from(accessed / 8));
        let (kind, mut children) = match stored {
            None => {
                let metadata = self.constant(0);
                (SymKind::LoadMemory, vec![address, bytes, metadata])
            }
            Some(stored) => {
                let Some(value) = self.operand_class(&probe, values, stored) else {
                    return false;
                };
                let space = self.constant(0);
                (SymKind::StoreMemory, vec![address, bytes, value, space])
            }
        };
        children.push(state);
        let mut term = template_node(kind, None, shape.results.first().copied());
        term.children = children;
        let semantics = self.eg.add(term);

        // The op identity reads the same operands plus the chain, so two
        // spellings of one access are one class exactly when the vocabulary says
        // they are.
        let cost = probe
            .clone()
            .as_interface::<dyn OpCost>()
            .map_or(1, |c| c.cost());
        let (dialect, name) = self.graph.op_type_name(self.graph.op_of(node));
        let ty = shape
            .results
            .first()
            .copied()
            .unwrap_or_else(|| UnitType::new(self.context));
        let identity = Node::sea(
            dialect,
            name,
            ty,
            self.graph.attributes(node).to_vec(),
            false,
            cost,
            operands,
        );
        let id = self.eg.add(identity.clone());
        self.eg.union(id, semantics);

        match shape.results.first() {
            // A read: its value is the term, and the chain it hands on is the one
            // it read.
            Some(&ty) => {
                self.record(Origin::output(node, 0), id, ty, identity);
                self.class_of.insert(shape.state_output, state);
            }
            // A write: the chain it produces *is* the term.
            None => {
                self.class_of.insert(shape.state_output, id);
                if self.nesting == 0 {
                    self.seeded.push((shape.state_output, identity));
                }
            }
        }
        true
    }

    /// The ports of a node the memory vocabulary can spell: every input a value
    /// but the last, which is the one chain [`super::raise`] threads, and one
    /// output at most — the value a read produces — before the chain. An access
    /// that computes more than that is not one the vocabulary names.
    fn memory_shape(&self, node: NodeId) -> Option<MemoryShape> {
        let inputs = self.graph.input_types(node);
        let outputs = self.graph.outputs(node);
        let (PortType::State, values) = inputs.split_last()? else {
            return None;
        };
        let (PortType::State, results) = outputs.split_last()? else {
            return None;
        };
        if results.len() > 1 || values.iter().any(|port| port.value_type().is_none()) {
            return None;
        }
        Some(MemoryShape {
            state_input: values.len(),
            state_output: Origin::output(node, results.len() as u32),
            results: results
                .iter()
                .map(|port| port.value_type())
                .collect::<Option<_>>()?,
        })
    }

    /// The class the node's operand list holds for the probe's `value` operand.
    fn operand_class(&self, probe: &Arc<OpInstance>, values: &[Id], value: ValueId) -> Option<Id> {
        let index = probe
            .operands
            .iter()
            .position(|&operand| operand == value)?;
        values.get(index).copied()
    }

    /// An unsigned literal of the memory vocabulary — a byte count, a metadata
    /// or address-space code — at the one width every seeding spells it.
    fn constant(&mut self, value: u64) -> Id {
        self.eg
            .add(Node::constant(minimal_unsigned_apint(value), Prov::None))
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
        // Whatever the arms do with the chains they were handed, the gates do not
        // model it, so a law may not drop a write the γ can see.
        self.escape_states(node);
        self.inline(taken, &inputs[1..]);
        self.inline(otherwise, &inputs[1..]);

        for (port, &output) in self.graph.outputs(node).to_vec().iter().enumerate() {
            let origin = Origin::output(node, port as u32);
            // A chain a γ hands back is opaque: the writes on it happened on one
            // path. Anchoring it rather than leaving it classless keeps the chain
            // spellable, so the accesses *after* the γ are still terms — over
            // something nothing forwards through.
            let PortType::Value(ty) = output else {
                self.anchor_state(origin);
                continue;
            };
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
            let gate = Node::gamma(value, vec![condition, then, otherwise]).typed(ty);
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
        // Like a γ's, the chains a θ hands back are opaque — the writes on them
        // happened some number of times ([`Self::seed_gamma`]).
        self.escape_states(node);
        let outputs = self.graph.outputs(node).to_vec();
        let mut placeholders = Vec::new();
        for (port, &output) in outputs.iter().enumerate() {
            let origin = Origin::output(node, port as u32);
            let PortType::Value(ty) = output else {
                self.anchor_state(origin);
                continue;
            };
            self.anchor(origin, ty);
            let value = self.context.create_value(ty, None).id();
            let id = self.eg.add(Node::input(value).typed(ty));
            let carried = Origin::argument(body, port as u32);
            self.class_of.insert(carried, id);
            // The placeholder is a leaf like any other, so an extraction that
            // chooses it must reach the origin it stands for: the body argument
            // carrying the loop variable.
            self.anchor_of.insert(value, carried);
            self.types.push((id, ty));
            placeholders.push((port, value, ty, id));
        }
        self.seed_nested(body);

        let inputs = self.graph.inputs(node).to_vec();
        let results = self.graph.region_results(body).to_vec();
        for (port, value, ty, placeholder) in placeholders {
            // The θ region's first result is the continuation predicate, so the
            // latch of port `p` is result `p + 1`.
            let latched = self.class_of.get(&results[port + 1]).copied();
            let (Some(init), Some(latch)) = (self.class_of.get(&inputs[port]).copied(), latched)
            else {
                continue;
            };
            let mu = self.eg.add(Node::theta(value, init, latch).typed(ty));
            self.eg.union(placeholder, mu);
        }
        self.eg.rebuild();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::{IntegerType, UnitType};
    use crate::sem::SymKind;
    use crate::sem::rewrites::{SaturationLimits, discover_rewrites, saturate};

    /// The selection theory is written in the semantic vocabulary and its
    /// axioms bind on class *widths*, so a view whose classes hold only the op
    /// identity binds none of them. Seeded semantically and typed, the class of
    /// an `extsi` binds `sext-bridge` and gains the shift pair the axiom proves
    /// it equal to.
    #[test]
    fn a_view_class_binds_a_width_dependent_selection_axiom() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let narrow = IntegerType::new(&context, 32);
        let wide = IntegerType::new(&context, 64);
        let mut graph = Graph::new(i1, &[]);

        let body = graph.open_region(&[PortType::Value(narrow)]);
        let extsi = graph.op_type("builtin", "extsi");
        let extended = graph
            .add_node(
                extsi,
                &[Origin::argument(body, 0)],
                &[PortType::Value(wide)],
                &[],
                Vec::new(),
            )
            .expect("the argument is visible in its own region");
        graph
            .close_region(body, &[Origin::output(extended, 0)])
            .expect("the extension is visible in its region");
        let function = PortType::Value(UnitType::new(&context));
        let lambda = graph
            .add_node(kinds::LAMBDA, &[], &[function], &[body], Vec::new())
            .expect("a λ over a closed region");
        graph
            .finish(&[Origin::output(lambda, 0)])
            .expect("the λ is the only export");

        let mut view = View::build(&context, &graph, body, None);
        saturate(
            &context,
            &mut view.eg,
            &discover_rewrites(),
            SaturationLimits::default(),
        );

        let class = view
            .class(Origin::output(extended, 0))
            .expect("the extension is a term");
        let kinds: Vec<SymKind> = view
            .eg
            .nodes(class)
            .iter()
            .filter_map(|n| n.sym())
            .collect();
        assert!(
            kinds.contains(&SymKind::ShiftRightArithmetic),
            "the sign extension must be proved equal to its shift pair, got {kinds:?}"
        );
    }
}
