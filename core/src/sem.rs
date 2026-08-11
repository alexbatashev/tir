//! Semantic-expression substrate.
//!
//! The semantic graph, its interpreter, width inference and selection
//! canonicalization all live in the `tir-symbolic` crate's `lang` module now;
//! this module re-exports them and adds the core-specific pieces: the post-order
//! graph alias core builds into, the `AsSemExpr` trait, and constant folding via
//! [`Operation::semantic_expr`].

use crate::graph::{Dag, MutDag, NodeId, NodeMeta, PostOrderDag};
use crate::{Operation, ValueId};

pub use tir_symbolic::lang::{
    AtomicRmwOp, BuildError, FloatFormat, MemOrdering, Memory, SCALAR_OPS, ScalarOp,
    SemBuilderHooks, SemExpr, SemType, SmtTemplate, SymKind, SymPayload, TypeError, TypeUnifier,
    TypeVar, Value, Width, WidthRule, WidthVar, build, canonicalize_for_selection, execute,
    execute_with_memory, infer_types, infer_widths, op_kind, op_name, parse, scalar_op,
    scalar_op_named,
};

pub(crate) mod axioms;
mod discover;
pub(crate) mod egraph;
pub mod node;
pub(crate) mod rewrites;
pub(crate) mod theory;
pub use egraph::SemEGraph;
pub use node::{IrOp, Kind, MergeKind, Prov, SemNode, SemPayload, template_node};
pub use rewrites::{IselRewrite, SaturationLimits};

pub(crate) use discover::sym;
pub use discover::{
    EquivalenceOracle, FuzzOracle, SmtOracle, confirm_bool_via_if, confirm_extension_via_shifts,
};
pub(crate) use discover::{con, op};

/// The post-order graph core builds semantic expressions into: [`SymKind`] nodes
/// over `SymPayload<ValueId>` leaves, annotated with [`NodeMeta`] so a node can
/// carry its originating op and inferred type.
pub type SemGraph = PostOrderDag<SymKind, SymPayload<ValueId>, NodeMeta>;

/// A payload literal in a serialized [`SemOp`] program; decoded to
/// [`SymPayload`] on replay.
#[derive(Clone, Copy, Debug)]
pub enum SemPayloadDesc {
    SymbolId(u32),
    Value(u32),
    Int {
        width: u32,
        value: u64,
        signed: bool,
    },
    Float(f64),
}

impl SemPayloadDesc {
    fn decode(self) -> SymPayload<ValueId> {
        match self {
            SemPayloadDesc::SymbolId(id) => SymPayload::SymbolId(id),
            SemPayloadDesc::Value(number) => SymPayload::Value(ValueId::from_number(number)),
            SemPayloadDesc::Int {
                width,
                value,
                signed,
            } => int_payload(width, value, signed),
            SemPayloadDesc::Float(value) => float_payload(value),
        }
    }
}

/// One step of a serialized semantic-graph construction. TMDL-generated
/// backends used to build every graph with one `add_node`/`add_edge` call per
/// statement; hundreds of thousands of such statements dominated rustc time on
/// the generated crates. Codegen therefore serializes these steps into a binary
/// blob ([`SemBlobBuilder`]) the generated crate embeds with `include_bytes!`,
/// so rustc sees one constant per crate instead of one per step.
/// `Payload`/`Typed` apply to the most recently added node; `Edge` indices count
/// nodes of the current program from 0.
#[derive(Clone, Copy, Debug)]
pub enum SemOp {
    Node(SymKind),
    Payload(SemPayloadDesc),
    Typed(u32),
    Edge(u32, u32),
}

fn replay_sem_ops<G, F>(g: &mut G, ops: &[SemOp], mut set_type: F) -> NodeId
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<ValueId>>,
    F: FnMut(&mut G, NodeId, u32),
{
    let mut nodes: Vec<NodeId> = Vec::new();
    for op in ops {
        match *op {
            SemOp::Node(kind) => nodes.push(g.add_node(kind)),
            SemOp::Payload(desc) => {
                let node = *nodes.last().expect("payload before any node");
                g.set_leaf_data(node, desc.decode());
            }
            SemOp::Typed(width) => {
                let node = *nodes.last().expect("type before any node");
                set_type(g, node, width);
            }
            SemOp::Edge(parent, child) => {
                g.add_edge(nodes[parent as usize], nodes[child as usize]);
            }
        }
    }
    *nodes.last().expect("empty sem op array")
}

// ── Serialized sem programs ─────────────────────────────────────────────────

const SEM_BLOB_MAGIC: [u8; 4] = *b"TSEM";
const SEM_BLOB_VERSION: u8 = 1;

const TAG_NODE: u8 = 0;
const TAG_SYMBOL_ID: u8 = 1;
const TAG_VALUE: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_TYPED: u8 = 5;
const TAG_EDGE: u8 = 6;

/// Serializes sem programs into one blob: a `TSEM` header followed by records,
/// each a byte length and that many bytes of tagged [`SemOp`]s. [`Self::intern`]
/// returns the offset a program was placed at, which is all the generated crate
/// embeds per program. Node kinds are indices into the [`Self::finish`] kind
/// table rather than raw discriminants, so the blob needs no stable numbering
/// for [`SymKind`].
///
/// Output is a pure function of the interned programs and their order: the maps
/// are only ever looked up in, never iterated.
pub struct SemBlobBuilder {
    bytes: Vec<u8>,
    kinds: Vec<SymKind>,
    records: std::collections::HashMap<Vec<u8>, u32>,
}

impl Default for SemBlobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SemBlobBuilder {
    pub fn new() -> Self {
        let mut bytes = SEM_BLOB_MAGIC.to_vec();
        bytes.push(SEM_BLOB_VERSION);
        Self {
            bytes,
            kinds: Vec::new(),
            records: std::collections::HashMap::new(),
        }
    }

    /// Appends `ops` as a record, reusing an identical earlier one; returns the
    /// record's offset.
    pub fn intern(&mut self, ops: &[SemOp]) -> u32 {
        let mut body = Vec::new();
        for op in ops {
            self.encode_op(*op, &mut body);
        }
        if let Some(&existing) = self.records.get(&body) {
            return existing;
        }
        let offset = self.bytes.len() as u32;
        self.bytes
            .extend_from_slice(&(body.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(&body);
        self.records.insert(body, offset);
        offset
    }

    /// The blob and the kind table its node codes index.
    pub fn finish(self) -> (Vec<u8>, Vec<SymKind>) {
        (self.bytes, self.kinds)
    }

    fn encode_op(&mut self, op: SemOp, out: &mut Vec<u8>) {
        match op {
            SemOp::Node(kind) => {
                let code = self.kind_code(kind);
                out.push(TAG_NODE);
                out.extend_from_slice(&code.to_le_bytes());
            }
            SemOp::Payload(SemPayloadDesc::SymbolId(id)) => {
                out.push(TAG_SYMBOL_ID);
                out.extend_from_slice(&id.to_le_bytes());
            }
            SemOp::Payload(SemPayloadDesc::Value(number)) => {
                out.push(TAG_VALUE);
                out.extend_from_slice(&number.to_le_bytes());
            }
            SemOp::Payload(SemPayloadDesc::Int {
                width,
                value,
                signed,
            }) => {
                out.push(TAG_INT);
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
                out.push(u8::from(signed));
            }
            SemOp::Payload(SemPayloadDesc::Float(value)) => {
                out.push(TAG_FLOAT);
                out.extend_from_slice(&value.to_bits().to_le_bytes());
            }
            SemOp::Typed(width) => {
                out.push(TAG_TYPED);
                out.extend_from_slice(&width.to_le_bytes());
            }
            SemOp::Edge(parent, child) => {
                out.push(TAG_EDGE);
                out.extend_from_slice(&parent.to_le_bytes());
                out.extend_from_slice(&child.to_le_bytes());
            }
        }
    }

    fn kind_code(&mut self, kind: SymKind) -> u16 {
        match self.kinds.iter().position(|known| *known == kind) {
            Some(code) => code as u16,
            None => {
                self.kinds.push(kind);
                (self.kinds.len() - 1) as u16
            }
        }
    }
}

struct SemBlobReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl SemBlobReader<'_> {
    fn u8(&mut self) -> u8 {
        let value = self.bytes[self.pos];
        self.pos += 1;
        value
    }

    fn u16(&mut self) -> u16 {
        let value = u16::from_le_bytes(self.bytes[self.pos..self.pos + 2].try_into().unwrap());
        self.pos += 2;
        value
    }

    fn u32(&mut self) -> u32 {
        let value = u32::from_le_bytes(self.bytes[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        value
    }

    fn u64(&mut self) -> u64 {
        let value = u64::from_le_bytes(self.bytes[self.pos..self.pos + 8].try_into().unwrap());
        self.pos += 8;
        value
    }
}

/// Decodes the program at `offset` of a [`SemBlobBuilder`] blob; `kinds` is the
/// table returned alongside it.
pub fn decode_sem_ops(blob: &[u8], offset: u32, kinds: &[SymKind]) -> Vec<SemOp> {
    assert_eq!(blob[..4], SEM_BLOB_MAGIC, "not a sem blob");
    assert_eq!(blob[4], SEM_BLOB_VERSION, "unsupported sem blob version");
    let mut reader = SemBlobReader {
        bytes: blob,
        pos: offset as usize,
    };
    let end = reader.pos + 4 + reader.u32() as usize;
    let mut ops = Vec::new();
    while reader.pos < end {
        ops.push(match reader.u8() {
            TAG_NODE => SemOp::Node(kinds[reader.u16() as usize]),
            TAG_SYMBOL_ID => SemOp::Payload(SemPayloadDesc::SymbolId(reader.u32())),
            TAG_VALUE => SemOp::Payload(SemPayloadDesc::Value(reader.u32())),
            TAG_INT => SemOp::Payload(SemPayloadDesc::Int {
                width: reader.u32(),
                value: reader.u64(),
                signed: reader.u8() != 0,
            }),
            TAG_FLOAT => SemOp::Payload(SemPayloadDesc::Float(f64::from_bits(reader.u64()))),
            TAG_TYPED => SemOp::Typed(reader.u32()),
            TAG_EDGE => SemOp::Edge(reader.u32(), reader.u32()),
            tag => unreachable!("unknown sem op tag {tag}"),
        });
    }
    ops
}

/// Replays the program at `offset` into any semantic-graph sink; returns the
/// root (the last node, as programs are serialized in post order).
pub trait ExtendSemBytes: MutDag<Node = SymKind, Leaf = SymPayload<ValueId>> + Sized {
    fn extend_sem_bytes(&mut self, kinds: &[SymKind], blob: &[u8], offset: u32) -> NodeId {
        replay_sem_ops(self, &decode_sem_ops(blob, offset, kinds), |_, _, _| {
            unreachable!("SemOp::Typed requires extend_sem_bytes_typed")
        })
    }
}

impl<G: MutDag<Node = SymKind, Leaf = SymPayload<ValueId>>> ExtendSemBytes for G {}

/// [`ExtendSemBytes`] for graphs that carry [`NodeMeta`], resolving
/// [`SemOp::Typed`] widths against the context's interned integer types.
pub trait ExtendSemBytesTyped:
    MutDag<Node = SymKind, Leaf = SymPayload<ValueId>, Annotation = NodeMeta> + Sized
{
    fn extend_sem_bytes_typed(
        &mut self,
        context: &crate::Context,
        kinds: &[SymKind],
        blob: &[u8],
        offset: u32,
    ) -> NodeId {
        use crate::graph::MetaMutDag as _;
        replay_sem_ops(
            self,
            &decode_sem_ops(blob, offset, kinds),
            |g, node, width| {
                g.set_actual_type(node, crate::builtin::IntegerType::new(context, width));
            },
        )
    }
}

impl<G: MutDag<Node = SymKind, Leaf = SymPayload<ValueId>, Annotation = NodeMeta>>
    ExtendSemBytesTyped for G
{
}

/// The definedness condition of a partial integer operation, materialized as new
/// nodes in `g` over the operation's own operands. The partial kinds are the
/// IR-level division/remainder ops, undefined at a zero divisor (and, for the
/// signed forms, at the `MIN / -1` overflow point). Returns the condition's root,
/// or `None` when `node`'s kind is total. `widths` must give every existing
/// node's bit width (e.g. from [`infer_widths`]); the operand width sizes the
/// emitted constants.
///
/// This is the metadata the guard-relaxation gate consumes: a guarded target
/// instruction may be relaxed to the pure partial op exactly where the op is
/// defined, so `D(pattern)` is the antecedent of the relaxation obligation.
pub fn definedness_condition(
    g: &mut SemGraph,
    node: NodeId,
    widths: &[Option<u32>],
) -> Option<NodeId> {
    let signed = match *g.get_node(node) {
        SymKind::Div | SymKind::SRem => true,
        SymKind::UDiv | SymKind::URem => false,
        _ => return None,
    };
    let operands: Vec<NodeId> = g.children(node).collect();
    let [lhs, rhs] = operands.as_slice() else {
        return None;
    };
    let (lhs, rhs) = (*lhs, *rhs);
    let width = widths.get(rhs.index()).copied().flatten()?;

    let zero = con(g, 0, width);
    let nonzero = op(g, SymKind::Ne, &[rhs, zero]);
    if !signed {
        return Some(nonzero);
    }
    let min = con(g, 1u64 << (width - 1), width);
    let all_ones = con(g, ones_mask(width), width);
    let is_min = op(g, SymKind::Eq, &[lhs, min]);
    let is_neg_one = op(g, SymKind::Eq, &[rhs, all_ones]);
    let overflow = op(g, SymKind::And, &[is_min, is_neg_one]);
    let no_overflow = op(g, SymKind::Not, &[overflow]);
    Some(op(g, SymKind::And, &[nonzero, no_overflow]))
}

fn ones_mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Build an operation's semantic expression into any graph backend. Unlike
/// [`Operation::semantic_expr`] (nailed to [`SemGraph`] so it stays `dyn`-callable),
/// this is generic, so isel and TMDL can lower into their own graph stores.
pub trait AsSemExpr: Operation {
    fn convert(
        &self,
        g: &mut impl MutDag<Node = SymKind, Leaf = SymPayload<ValueId>, Annotation = NodeMeta>,
    ) -> NodeId;
}

/// Fold an operation over constant operand `values` by evaluating its declared
/// semantic expression. Returns `None` for ops without one. This backs the
/// `ConstantFold` impl the `operation!` macro derives from `sem`.
pub fn fold_with_sem(op: &dyn Operation, values: &[Value]) -> Option<Value> {
    let mut graph = SemGraph::new();
    op.semantic_expr(&mut graph)?;
    Some(execute(&graph, values))
}

// ── APInt boundary helpers ──────────────────────────────────────────────────
//
// These let TMDL-generated backend code construct and consume sem values without
// naming `tir-adt` directly.

/// An integer payload literal for graph construction (`signed` picks the
/// constructor); used by TMDL codegen in place of a raw `APInt`.
pub fn int_payload(width: u32, value: u64, signed: bool) -> SymPayload<ValueId> {
    let v = if signed {
        tir_adt::APInt::new_signed(width, value as i64)
    } else {
        tir_adt::APInt::new(width, value)
    };
    SymPayload::Int(v)
}

/// A float payload literal for graph construction.
pub fn float_payload(value: f64) -> SymPayload<ValueId> {
    SymPayload::Float(tir_adt::APFloat::from_f64(value))
}

/// A signed integer interpreter value of the given width.
pub fn int_value_signed(width: u32, value: i64) -> Value {
    Value::Int(tir_adt::APInt::new_signed(width, value))
}

/// An unsigned integer interpreter value of the given width.
pub fn int_value(width: u32, value: u64) -> Value {
    Value::Int(tir_adt::APInt::new(width, value))
}

/// Wrap a machine-register `APInt` (e.g. from `MachineContext::read_register`) as
/// an interpreter value.
pub fn value_from_register(v: tir_adt::APInt) -> Value {
    Value::Int(v)
}

/// Wrap raw register byte lanes (e.g. a vector register from
/// `MachineContext::read_register_bits`) as an interpreter value; behaviors then
/// split it into lanes.
pub fn value_from_raw_bits(v: tir_adt::RawBits) -> Value {
    Value::RawBits(v)
}

/// Convert an interpreter integer back to a machine-register `APInt` for write-back.
pub fn register_from_int(v: tir_adt::APInt) -> tir_adt::APInt {
    v
}
