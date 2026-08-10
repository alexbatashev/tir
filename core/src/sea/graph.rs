//! The green layer's storage: a region-hierarchical value+state graph.
//!
//! A region `R = (A, N, E, R')` owns an argument tuple `A`, nodes `N`, edges `E`
//! and the subregions `R'` reachable through its structural nodes. A node is
//! either *simple* (a dialect op) or *structural* (it owns regions). An edge
//! connects an **origin** — a node output or a region argument — to a **user** —
//! a node input or a region result.
//!
//! # Invariants
//!
//! * *Every input and every region result is the user of exactly one edge.* By
//!   construction: an input slot **is** the edge, stored as the [`Origin`] it
//!   reads. There is no way to express zero or two edges into a slot.
//! * *Acyclicity.* By construction: regions are built bottom-up into append-only
//!   arenas, so an origin can only name a node that already exists. The node
//!   order of a region is the graph's topological order, and every edge inside a
//!   region runs from a lower position to a higher one; [`Graph::add_node`]
//!   appends, so it cannot express anything else, and the mutation API
//!   re-establishes the order it edits (see [`super::mutate`]).
//! * *Scoping.* An origin used inside region `R` must be a node of `R` or an
//!   argument of `R`. Reaching into an enclosing region is not expressible;
//!   dependencies are routed through region arguments (γ/θ signatures, λ context
//!   variables), as the RVSDG specification requires.
//! * *Type match.* Structural nodes verify their signature laws when created
//!   (see [`super::kinds`]); [`Graph::verify`] re-checks the whole graph, which
//!   is what the mutation API leans on.
//!
//! # Storage
//!
//! Structure-of-arrays with dense `u32` ids: one column per node or region
//! property, and CSR segments (`start`, `len`) into shared pools for the
//! variable-length parts — inputs, outputs, subregions, attributes, region
//! arguments, region results. The columns are sized and ordered so a chunked,
//! 64-byte-aligned allocator can be dropped underneath them later; this
//! increment uses plain `Vec`s.
//!
//! The one column that is not a CSR pool is a region's node order: it is the
//! schedule, and the mutation API inserts into the middle of it, which a shared
//! contiguous pool cannot express.

use crate::TypeId;
use crate::attributes::NamedAttribute;

use super::Error;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RegionId(u32);

/// An interned `(dialect, name)` pair. Op types are interned per graph exactly
/// like the [`crate::Context`]'s op registry: dialects add their own, and the
/// core holds no closed enum of them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct OpTypeId(u32);

impl NodeId {
    pub fn number(self) -> u32 {
        self.0
    }
    fn index(self) -> usize {
        self.0 as usize
    }
}

impl RegionId {
    pub fn number(self) -> u32 {
        self.0
    }
    fn index(self) -> usize {
        self.0 as usize
    }
}

impl OpTypeId {
    pub(super) const fn new(id: u32) -> Self {
        OpTypeId(id)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// What flows along an edge: a typed value, or the state that orders the
/// operations touching memory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PortType {
    Value(TypeId),
    State,
}

impl PortType {
    pub fn value_type(self) -> Option<TypeId> {
        match self {
            PortType::Value(ty) => Some(ty),
            PortType::State => None,
        }
    }
}

/// The producing end of an edge: a node output, or an argument of the region the
/// user lives in. Packed into one word so the edge pools stay dense.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin(u64);

const ORIGIN_ARGUMENT: u64 = 1 << 63;

impl Origin {
    pub fn output(node: NodeId, port: u32) -> Self {
        Origin(((node.0 as u64) << 32) | port as u64)
    }

    pub fn argument(region: RegionId, port: u32) -> Self {
        Origin(ORIGIN_ARGUMENT | ((region.0 as u64) << 32) | port as u64)
    }

    pub fn port(self) -> u32 {
        self.0 as u32
    }

    fn owner(self) -> u32 {
        ((self.0 & !ORIGIN_ARGUMENT) >> 32) as u32
    }

    pub fn node(self) -> Option<NodeId> {
        (self.0 & ORIGIN_ARGUMENT == 0).then(|| NodeId(self.owner()))
    }

    pub fn region(self) -> Option<RegionId> {
        (self.0 & ORIGIN_ARGUMENT != 0).then(|| RegionId(self.owner()))
    }
}

impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.node() {
            Some(node) => write!(f, "n{}:{}", node.0, self.port()),
            None => write!(f, "a{}:{}", self.owner(), self.port()),
        }
    }
}

/// A CSR slice into one of the shared pools.
#[derive(Clone, Copy, Default, Debug)]
struct Segment {
    start: u32,
    len: u32,
}

impl Segment {
    fn range(self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

/// The interning table behind [`OpTypeId`].
#[derive(Default)]
pub(super) struct OpTypeTable {
    names: Vec<(&'static str, &'static str)>,
}

impl OpTypeTable {
    pub(super) fn intern(&mut self, dialect: &'static str, name: &'static str) -> OpTypeId {
        match self
            .names
            .iter()
            .position(|entry| *entry == (dialect, name))
        {
            Some(index) => OpTypeId(index as u32),
            None => {
                self.names.push((dialect, name));
                OpTypeId(self.names.len() as u32 - 1)
            }
        }
    }
}

pub struct Graph {
    op_types: OpTypeTable,
    /// The type a γ predicate and a θ continuation predicate carry.
    predicate_type: TypeId,

    // Node columns.
    node_op: Vec<OpTypeId>,
    node_region: Vec<RegionId>,
    node_version: Vec<u32>,
    node_inputs: Vec<Segment>,
    node_outputs: Vec<Segment>,
    node_regions: Vec<Segment>,
    node_attributes: Vec<Segment>,

    // Node pools.
    inputs: Vec<Origin>,
    outputs: Vec<PortType>,
    subregions: Vec<RegionId>,
    attributes: Vec<NamedAttribute>,

    // Region columns.
    region_owner: Vec<NodeId>,
    region_arguments: Vec<Segment>,
    region_results: Vec<Segment>,
    /// The schedule; see the module docs for why this one is not a CSR pool.
    region_order: Vec<Vec<NodeId>>,

    // Region pools.
    arguments: Vec<PortType>,
    results: Vec<Origin>,

    open: Vec<RegionId>,
    omega: Option<NodeId>,
}

const NO_NODE: NodeId = NodeId(u32::MAX);
const NO_REGION: RegionId = RegionId(u32::MAX);

impl Graph {
    /// Open a graph whose ω region takes `imports` as arguments. `predicate_type`
    /// is the integer type carrying γ and θ predicates (`i1` for a two-way γ).
    pub fn new(predicate_type: TypeId, imports: &[PortType]) -> Self {
        let mut graph = Graph {
            op_types: OpTypeTable::default(),
            predicate_type,
            node_op: Vec::new(),
            node_region: Vec::new(),
            node_version: Vec::new(),
            node_inputs: Vec::new(),
            node_outputs: Vec::new(),
            node_regions: Vec::new(),
            node_attributes: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            subregions: Vec::new(),
            attributes: Vec::new(),
            region_owner: Vec::new(),
            region_arguments: Vec::new(),
            region_results: Vec::new(),
            region_order: Vec::new(),
            arguments: Vec::new(),
            results: Vec::new(),
            open: Vec::new(),
            omega: None,
        };
        super::kinds::intern_all(&mut graph.op_types);
        graph.open_region(imports);
        graph
    }

    pub fn predicate_type(&self) -> TypeId {
        self.predicate_type
    }

    /// Intern a `(dialect, name)` op type.
    pub fn op_type(&mut self, dialect: &'static str, name: &'static str) -> OpTypeId {
        self.op_types.intern(dialect, name)
    }

    /// The `(dialect, name)` an op type spells.
    pub fn op_type_name(&self, op: OpTypeId) -> (&'static str, &'static str) {
        self.op_types.names[op.index()]
    }

    /// Open a region with the given argument tuple. Nodes created while it is the
    /// innermost open region belong to it; nested regions may be opened and closed
    /// in the meantime.
    pub fn open_region(&mut self, arguments: &[PortType]) -> RegionId {
        let region = RegionId(self.region_arguments.len() as u32);
        let segment = push_pool(&mut self.arguments, arguments.iter().copied());
        self.region_arguments.push(segment);
        self.region_results.push(Segment::default());
        self.region_order.push(Vec::new());
        self.region_owner.push(NO_NODE);
        self.open.push(region);
        region
    }

    /// Seal `region` with its result tuple. The region must be the innermost open
    /// one, and every result must name an origin visible inside it.
    pub fn close_region(&mut self, region: RegionId, results: &[Origin]) -> Result<(), Error> {
        if self.open.last() != Some(&region) {
            return Err(Error::new(format!(
                "region {} is not the innermost open region",
                region.0
            )));
        }
        for (index, &origin) in results.iter().enumerate() {
            self.check_visible(region, origin, usize::MAX)
                .map_err(|e| e.context(format!("region {} result {index}", region.0)))?;
        }
        self.open.pop();
        self.region_results[region.index()] = push_pool(&mut self.results, results.iter().copied());
        Ok(())
    }

    /// Create a node at the end of the innermost open region. Regions listed in
    /// `regions` must already be closed and unowned; they become its subregions.
    pub fn add_node(
        &mut self,
        op: OpTypeId,
        inputs: &[Origin],
        outputs: &[PortType],
        regions: &[RegionId],
        attributes: Vec<NamedAttribute>,
    ) -> Result<NodeId, Error> {
        let Some(&region) = self.open.last() else {
            return Err(Error::new("no open region to create a node in"));
        };
        self.insert_node(region, usize::MAX, op, inputs, outputs, regions, attributes)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_node(
        &mut self,
        region: RegionId,
        position: usize,
        op: OpTypeId,
        inputs: &[Origin],
        outputs: &[PortType],
        regions: &[RegionId],
        attributes: Vec<NamedAttribute>,
    ) -> Result<NodeId, Error> {
        let position = position.min(self.region_order[region.index()].len());
        for (index, &origin) in inputs.iter().enumerate() {
            self.check_visible(region, origin, position)
                .map_err(|e| e.context(format!("input {index}")))?;
        }
        for &subregion in regions {
            if self.open.contains(&subregion) {
                return Err(Error::new(format!("region {} is still open", subregion.0)));
            }
            if self.region_owner[subregion.index()] != NO_NODE {
                return Err(Error::new(format!(
                    "region {} already belongs to a node",
                    subregion.0
                )));
            }
        }

        let node = NodeId(self.node_op.len() as u32);
        self.node_op.push(op);
        self.node_region.push(region);
        self.node_version.push(0);
        let segment = push_pool(&mut self.inputs, inputs.iter().copied());
        self.node_inputs.push(segment);
        let segment = push_pool(&mut self.outputs, outputs.iter().copied());
        self.node_outputs.push(segment);
        let segment = push_pool(&mut self.subregions, regions.iter().copied());
        self.node_regions.push(segment);
        let segment = push_pool(&mut self.attributes, attributes.into_iter());
        self.node_attributes.push(segment);
        for &subregion in regions {
            self.region_owner[subregion.index()] = node;
        }

        if let Err(error) = super::kinds::verify_node(self, node) {
            self.pop_node(regions);
            return Err(error);
        }

        self.region_order[region.index()].insert(position, node);
        Ok(node)
    }

    /// Undo a partially created node after its signature laws failed, so a
    /// rejected `insert_node` leaves no trace.
    fn pop_node(&mut self, regions: &[RegionId]) {
        for &subregion in regions {
            self.region_owner[subregion.index()] = NO_NODE;
        }
        truncate_pool(&mut self.inputs, self.node_inputs.pop());
        truncate_pool(&mut self.outputs, self.node_outputs.pop());
        truncate_pool(&mut self.subregions, self.node_regions.pop());
        truncate_pool(&mut self.attributes, self.node_attributes.pop());
        self.node_op.pop();
        self.node_region.pop();
        self.node_version.pop();
    }

    /// Seal the ω region and create the ω node: the translation unit, with the
    /// region's arguments as its imports and `exports` as its results.
    pub fn finish(&mut self, exports: &[Origin]) -> Result<NodeId, Error> {
        let omega_region = self.omega_region();
        self.close_region(omega_region, exports)?;
        let node = NodeId(self.node_op.len() as u32);
        self.node_op.push(super::kinds::OMEGA);
        self.node_region.push(NO_REGION);
        self.node_version.push(0);
        self.node_inputs.push(Segment::default());
        self.node_outputs.push(Segment::default());
        let segment = push_pool(&mut self.subregions, std::iter::once(omega_region));
        self.node_regions.push(segment);
        self.node_attributes.push(Segment::default());
        self.region_owner[omega_region.index()] = node;
        self.omega = Some(node);
        super::kinds::verify_node(self, node)?;
        Ok(node)
    }

    pub fn omega(&self) -> Option<NodeId> {
        self.omega
    }

    pub fn omega_region(&self) -> RegionId {
        RegionId(0)
    }

    // ---- reads ----

    pub fn node_count(&self) -> usize {
        self.node_op.len()
    }

    pub fn region_count(&self) -> usize {
        self.region_arguments.len()
    }

    pub fn op_of(&self, node: NodeId) -> OpTypeId {
        self.node_op[node.index()]
    }

    pub fn version(&self, node: NodeId) -> u32 {
        self.node_version[node.index()]
    }

    pub fn inputs(&self, node: NodeId) -> &[Origin] {
        &self.inputs[self.node_inputs[node.index()].range()]
    }

    pub fn outputs(&self, node: NodeId) -> &[PortType] {
        &self.outputs[self.node_outputs[node.index()].range()]
    }

    pub fn subregions(&self, node: NodeId) -> &[RegionId] {
        &self.subregions[self.node_regions[node.index()].range()]
    }

    pub fn attributes(&self, node: NodeId) -> &[NamedAttribute] {
        &self.attributes[self.node_attributes[node.index()].range()]
    }

    pub fn region_arguments(&self, region: RegionId) -> &[PortType] {
        &self.arguments[self.region_arguments[region.index()].range()]
    }

    pub fn region_results(&self, region: RegionId) -> &[Origin] {
        &self.results[self.region_results[region.index()].range()]
    }

    /// The nodes of `region` in schedule order, which is a topological order.
    pub fn region_nodes(&self, region: RegionId) -> &[NodeId] {
        &self.region_order[region.index()]
    }

    pub fn parent_region(&self, node: NodeId) -> Option<RegionId> {
        let region = self.node_region[node.index()];
        (region != NO_REGION).then_some(region)
    }

    pub fn region_owner(&self, region: RegionId) -> Option<NodeId> {
        let owner = self.region_owner[region.index()];
        (owner != NO_NODE).then_some(owner)
    }

    /// The type an origin produces.
    pub fn origin_type(&self, origin: Origin) -> PortType {
        match origin.node() {
            Some(node) => self.outputs(node)[origin.port() as usize],
            None => self.region_arguments(origin.region().expect("argument origin"))
                [origin.port() as usize],
        }
    }

    /// The types a node's inputs read, in order.
    pub fn input_types(&self, node: NodeId) -> Vec<PortType> {
        self.inputs(node)
            .iter()
            .map(|&origin| self.origin_type(origin))
            .collect()
    }

    /// The types a region's results produce, in order.
    pub fn region_result_types(&self, region: RegionId) -> Vec<PortType> {
        self.region_results(region)
            .iter()
            .map(|&origin| self.origin_type(origin))
            .collect()
    }

    /// Where `node` sits in its region's schedule.
    pub fn position(&self, node: NodeId) -> Option<usize> {
        let region = self.parent_region(node)?;
        self.region_order[region.index()]
            .iter()
            .position(|&other| other == node)
    }

    // ---- writes used by the mutation API ----

    pub(super) fn set_input(&mut self, node: NodeId, index: usize, origin: Origin) {
        let start = self.node_inputs[node.index()].start as usize;
        self.inputs[start + index] = origin;
    }

    pub(super) fn set_region_result(&mut self, region: RegionId, index: usize, origin: Origin) {
        let start = self.region_results[region.index()].start as usize;
        self.results[start + index] = origin;
    }

    pub(super) fn bump(&mut self, node: NodeId) {
        self.node_version[node.index()] += 1;
    }

    /// Move `node` out of its region's schedule and back in at `position`.
    pub(super) fn reschedule(&mut self, node: NodeId, position: usize) {
        let region = self
            .parent_region(node)
            .expect("only scheduled nodes can move");
        let order = &mut self.region_order[region.index()];
        let from = order
            .iter()
            .position(|&other| other == node)
            .expect("node is in its region's schedule");
        order.remove(from);
        order.insert(position.min(order.len()), node);
    }

    /// Move `node` into `region`, whose owner must be created afterwards.
    pub(super) fn move_into(&mut self, node: NodeId, region: RegionId) {
        let old = self
            .parent_region(node)
            .expect("only scheduled nodes can move");
        let order = &mut self.region_order[old.index()];
        let from = order
            .iter()
            .position(|&other| other == node)
            .expect("node is in its region's schedule");
        order.remove(from);
        self.node_region[node.index()] = region;
        self.region_order[region.index()].push(node);
    }

    pub(super) fn is_open(&self) -> bool {
        !self.open.is_empty()
    }

    // ---- checks ----

    /// An origin is usable at `position` inside `region` when it is one of the
    /// region's own arguments, or the output of a node scheduled before that
    /// position. This is what makes the graph acyclic and keeps every
    /// cross-region dependency routed through a region argument.
    fn check_visible(
        &self,
        region: RegionId,
        origin: Origin,
        position: usize,
    ) -> Result<(), Error> {
        match origin.node() {
            Some(node) => {
                if node.index() >= self.node_op.len() {
                    return Err(Error::new(format!("origin names unknown node {}", node.0)));
                }
                if self.node_region[node.index()] != region {
                    return Err(Error::new(format!(
                        "origin {origin:?} is not visible in region {}: dependencies must be \
                         routed through region arguments",
                        region.0
                    )));
                }
                if origin.port() as usize >= self.outputs(node).len() {
                    return Err(Error::new(format!(
                        "origin {origin:?} names a port node {} does not have",
                        node.0
                    )));
                }
                let source = self.position(node).expect("node is scheduled");
                if source >= position {
                    return Err(Error::new(format!(
                        "origin {origin:?} is scheduled at {source}, not before its user at \
                         {position}"
                    )));
                }
                Ok(())
            }
            None => {
                let owner = origin.region().expect("argument origin");
                if owner != region {
                    return Err(Error::new(format!(
                        "origin {origin:?} is an argument of region {}, not of region {}",
                        owner.0, region.0
                    )));
                }
                if origin.port() as usize >= self.region_arguments(region).len() {
                    return Err(Error::new(format!(
                        "origin {origin:?} names an argument region {} does not have",
                        region.0
                    )));
                }
                Ok(())
            }
        }
    }

    /// Re-check every invariant that construction establishes. Cheap enough to
    /// run after a mutation, which is where it earns its keep.
    pub fn verify(&self) -> Result<(), Error> {
        if self.is_open() {
            return Err(Error::new("graph still has open regions"));
        }
        for index in 0..self.region_count() {
            let region = RegionId(index as u32);
            for (position, &node) in self.region_order[index].iter().enumerate() {
                if self.parent_region(node) != Some(region) {
                    return Err(Error::new(format!(
                        "node {} is listed in region {index} but belongs elsewhere",
                        node.0
                    )));
                }
                for (port, &origin) in self.inputs(node).iter().enumerate() {
                    self.check_visible(region, origin, position)
                        .map_err(|e| e.context(format!("node {} input {port}", node.0)))?;
                }
                super::kinds::verify_node(self, node)?;
            }
            for (port, &origin) in self.region_results(region).iter().enumerate() {
                self.check_visible(region, origin, usize::MAX)
                    .map_err(|e| e.context(format!("region {index} result {port}")))?;
            }
        }
        Ok(())
    }
}

fn push_pool<T>(pool: &mut Vec<T>, items: impl Iterator<Item = T>) -> Segment {
    let start = pool.len() as u32;
    pool.extend(items);
    Segment {
        start,
        len: pool.len() as u32 - start,
    }
}

fn truncate_pool<T>(pool: &mut Vec<T>, segment: Option<Segment>) {
    if let Some(segment) = segment {
        pool.truncate(segment.start as usize);
    }
}
