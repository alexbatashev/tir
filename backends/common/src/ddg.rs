//! Data dependence graph over a single machine-IR block.
//!
//! In physical-register form an instruction's register operands live in op
//! *attributes*, not SSA `operands`, so there are no def-use chains to follow:
//! data flow has to be *reconstructed* by scanning instructions in program order
//! and tracking, per physical register, which instruction last wrote it. The
//! result is the dependence graph a scheduler (e.g. an llvm-mca-style analyzer)
//! consumes to know how instructions are ordered with respect to one another.
//!
//! Three kinds of register dependency are recorded (memory and control are out of
//! scope for this first cut):
//!
//! * [`DepKind::Raw`] — true / flow dependency (read-after-write). Carries the
//!   producer's result latency; this is the edge that drives scheduling timing.
//! * [`DepKind::War`] — anti dependency (write-after-read). Pure ordering.
//! * [`DepKind::Waw`] — output dependency (write-after-write). Pure ordering.
//!
//! Nodes and adjacency are stored in [`tir::graph::PostOrderDag`]: instructions
//! are added in program order, and every edge points from a *dependent*
//! instruction to the earlier instruction it depends on, so the DAG's strict
//! post-order invariant (`to < from`) holds and its transitive-reachability index
//! is available for free (useful for dependency height / critical path later).

use std::collections::{HashMap, HashSet};
use std::fmt;

use tir::graph::{Dag, MutDag, Node, NodeId, PostOrderDag};
use tir::{Context, OpId};

use crate::liveness::{RegRef, op_regs};
use crate::sched::{InstrSchedClass, MachineModel};

/// The kind of register dependency between two instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DepKind {
    /// Read-after-write (true dependency); carries the producer's latency.
    Raw,
    /// Write-after-read (anti dependency); ordering only.
    War,
    /// Write-after-write (output dependency); ordering only.
    Waw,
}

impl DepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::Raw => "raw",
            DepKind::War => "war",
            DepKind::Waw => "waw",
        }
    }
}

/// The register that ties two instructions together. Physical registers carry
/// their class and encoding index; virtual ids only survive on the handful of
/// builtin SSA ops (e.g. the block terminator) that name registers positionally.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RegKey {
    Physical { class: String, index: u16 },
    Virtual(u32),
}

impl RegKey {
    fn of(reg: &RegRef) -> RegKey {
        match reg {
            RegRef::Physical { class, index } => RegKey::Physical {
                class: class.clone(),
                index: *index,
            },
            RegRef::Virtual { id, .. } => RegKey::Virtual(*id),
        }
    }
}

impl fmt::Display for RegKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegKey::Physical { class, index } => write!(f, "{class}[{index}]"),
            RegKey::Virtual(id) => write!(f, "v{id}"),
        }
    }
}

/// One dependency edge: `from` (the later, dependent instruction) must observe
/// the effect of `on` (the earlier instruction) on register `reg`.
#[derive(Clone, Debug)]
pub struct DepEdge {
    pub from: NodeId,
    pub on: NodeId,
    pub kind: DepKind,
    pub reg: RegKey,
    /// Producer→consumer latency in cycles. Non-zero only for [`DepKind::Raw`].
    pub latency: u16,
}

/// A graph node: one machine instruction.
pub struct InstrNode {
    pub op: OpId,
}

impl Node for InstrNode {
    fn is_leaf(&self, _ctx: &Context) -> bool {
        false
    }

    fn num_children(&self, _ctx: &Context) -> usize {
        0
    }
}

/// The data dependence graph of one block: instruction nodes plus the register
/// dependency edges between them.
pub struct Ddg {
    dag: PostOrderDag<InstrNode, ()>,
    edges: Vec<DepEdge>,
}

impl Ddg {
    pub fn len(&self) -> usize {
        self.dag.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dag.len() == 0
    }

    /// The instruction at `node`.
    pub fn op(&self, node: NodeId) -> OpId {
        self.dag.get_node(node).op
    }

    /// All dependency edges, in the order they were discovered.
    pub fn edges(&self) -> &[DepEdge] {
        &self.edges
    }

    /// The dependencies of `node`: the edges from `node` to the instructions it
    /// must wait for.
    pub fn deps(&self, node: NodeId) -> impl Iterator<Item = &DepEdge> {
        self.edges.iter().filter(move |e| e.from == node)
    }

    /// The underlying DAG, exposing adjacency and transitive reachability.
    pub fn dag(&self) -> &PostOrderDag<InstrNode, ()> {
        &self.dag
    }
}

/// Build the data dependence graph for `ops` (one block's instructions, in
/// program order). `model`, when present, supplies producer latencies for RAW
/// edges; otherwise the single-cycle default is used.
pub fn build(context: &Context, ops: &[OpId], model: Option<&MachineModel>) -> Ddg {
    let mut dag = PostOrderDag::new();
    let nodes: Vec<NodeId> = ops
        .iter()
        .map(|&op| dag.add_node(InstrNode { op }))
        .collect();
    let mut edges = Vec::new();
    let mut seen: HashSet<(usize, usize, DepKind, RegKey)> = HashSet::new();

    // Most recent writer and the readers-since-that-write, per register.
    let mut last_def: HashMap<RegKey, usize> = HashMap::new();
    let mut readers: HashMap<RegKey, Vec<usize>> = HashMap::new();

    let latency_of = |idx: usize| -> u16 {
        let op = context.get_op(ops[idx]);
        let key = sched_key(op.name());
        model
            .map(|m| m.sched_class(key).latency)
            .unwrap_or(InstrSchedClass::DEFAULT.latency)
    };

    for (i, &op_id) in ops.iter().enumerate() {
        let op = context.get_op(op_id);
        let regs = op_regs(&op);

        // Uses read the value live on entry to this op: RAW on the last writer.
        for use_reg in &regs.uses {
            let key = RegKey::of(use_reg);
            if let Some(&producer) = last_def.get(&key) {
                push_edge(
                    &mut dag,
                    &mut edges,
                    &mut seen,
                    (nodes[i], i),
                    (nodes[producer], producer),
                    DepKind::Raw,
                    key.clone(),
                    latency_of(producer),
                );
            }
            readers.entry(key).or_default().push(i);
        }

        // Defs overwrite the register: WAW on the last writer, WAR on every reader
        // since that write.
        for def_reg in &regs.defs {
            let key = RegKey::of(def_reg);
            if let Some(&producer) = last_def.get(&key) {
                push_edge(
                    &mut dag,
                    &mut edges,
                    &mut seen,
                    (nodes[i], i),
                    (nodes[producer], producer),
                    DepKind::Waw,
                    key.clone(),
                    0,
                );
            }
            if let Some(reader_list) = readers.get(&key) {
                for &reader in reader_list {
                    if reader != i {
                        push_edge(
                            &mut dag,
                            &mut edges,
                            &mut seen,
                            (nodes[i], i),
                            (nodes[reader], reader),
                            DepKind::War,
                            key.clone(),
                            0,
                        );
                    }
                }
            }
            last_def.insert(key.clone(), i);
            readers.remove(&key);
        }
    }

    Ddg { dag, edges }
}

#[allow(clippy::too_many_arguments)]
fn push_edge(
    dag: &mut PostOrderDag<InstrNode, ()>,
    edges: &mut Vec<DepEdge>,
    seen: &mut HashSet<(usize, usize, DepKind, RegKey)>,
    (from, from_idx): (NodeId, usize),
    (on, on_idx): (NodeId, usize),
    kind: DepKind,
    reg: RegKey,
    latency: u16,
) {
    if from == on || !seen.insert((from_idx, on_idx, kind, reg.clone())) {
        return;
    }
    dag.add_edge(from, on);
    edges.push(DepEdge {
        from,
        on,
        kind,
        reg,
        latency,
    });
}

/// The scheduling-table key for an op: its mnemonic, stripped of any dialect
/// prefix (`riscv.add` → `add`).
fn sched_key(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}
