//! The e-node label. Seeded inputs and control-flow gates reuse GSA's [`GateNode`]
//! verbatim; operations and constants are keyed by their *value-signature* (so two
//! congruent ops collapse under hash-consing) — `GateNode`'s bare `OpId` can't supply
//! that without the context, and rewrites also invent constants/ops that have no IR
//! until extraction. Matching/hash-consing key on identity, including result type and
//! value-affecting attributes; `cost`/`prov`/`origin` ride the node for extraction and
//! write-back only.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use tir_symbolic::egraph::{ENode, Id};

use crate::analysis::GateNode;
use crate::attributes::{AttributeValue, NamedAttribute};
use crate::utils::APInt;
use crate::{OpCost, OpId, OpInstance, Operation, TypeId};

/// The type sentinel an LHS template uses to mean "match any result type".
fn any_type() -> TypeId {
    TypeId::from_number(u32::MAX)
}

/// Write-back source for an op node: an existing IR op (reuse its result) or one a
/// rewrite invents (built by `Ruleset.emits[idx]` at extraction — never during
/// saturation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpProv {
    Seeded(OpId),
    Introduced(usize),
}

#[derive(Clone, Debug)]
pub enum Node {
    /// A GSA input or gate (never `GateNode::Op`), with its operand classes. Identity
    /// is the gate kind (and an input's `ValueId`); `args` disambiguate.
    Gate(GateNode, Vec<Id>),
    /// An operation, seeded or rewrite-introduced. Identity is `(dialect, name, result
    /// type, value attributes)` + `args`; `cost`/`prov` are payload.
    Op {
        dialect: &'static str,
        name: &'static str,
        ty: TypeId,
        attrs: Vec<NamedAttribute>,
        cost: u32,
        prov: OpProv,
        args: Vec<Id>,
    },
    /// A constant, seeded or folded. Identity is `(width, bits)`; `origin` reuses a
    /// seeded constant op rather than rebuilding it.
    Const { value: APInt, origin: Option<OpId> },
}

impl Node {
    /// A seeded op: identity, `ty`, and value attributes from `instance`; `cost` from
    /// its [`OpCost`] interface; provenance to reuse it at write-back.
    pub fn seeded(instance: &Arc<OpInstance>, ty: TypeId, args: Vec<Id>) -> Self {
        let cost = instance
            .clone()
            .as_interface::<dyn OpCost>()
            .map_or(1, |c| c.cost());
        Self::Op {
            dialect: instance.dialect,
            name: instance.name,
            ty,
            attrs: instance.attributes.clone(),
            cost,
            prov: OpProv::Seeded(instance.id),
            args,
        }
    }

    /// An op the rewrite at `idx` introduces (no attributes), with its result `ty` and
    /// modeled `cost`.
    pub fn introduced<O: Operation>(ty: TypeId, cost: u32, idx: usize, args: Vec<Id>) -> Self {
        Self::Op {
            dialect: O::dialect(),
            name: O::name(),
            ty,
            attrs: Vec::new(),
            cost,
            prov: OpProv::Introduced(idx),
            args,
        }
    }

    /// An LHS-pattern template for op `O`: matched on dialect+name and arity, with a
    /// wildcard type and no attributes.
    pub fn pattern<O: Operation>(args: Vec<Id>) -> Self {
        Self::introduced::<O>(any_type(), 0, usize::MAX, args)
    }

    pub fn op_type(&self) -> Option<TypeId> {
        match self {
            Node::Op { ty, .. } => Some(*ty),
            _ => None,
        }
    }
}

/// A gate's extraction cost is high, so whenever its class is proven equal to a
/// concrete value that value is preferred and the merge is eliminated; when no
/// equivalent exists the gate is the only node and is chosen regardless.
const GATE_COST: u64 = 1 << 20;

/// Extraction cost: an op's modeled cost, [`GATE_COST`] for gates, zero for constants.
pub fn cost(node: &Node) -> u64 {
    match node {
        Node::Op { cost, .. } => *cost as u64,
        Node::Gate(..) => GATE_COST,
        Node::Const { .. } => 0,
    }
}

impl ENode for Node {
    fn children(&self) -> &[Id] {
        match self {
            Node::Gate(_, args) | Node::Op { args, .. } => args,
            Node::Const { .. } => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Id] {
        match self {
            Node::Gate(_, args) | Node::Op { args, .. } => args,
            Node::Const { .. } => &mut [],
        }
    }

    fn hash_cons(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Node::Gate(gate, _) => {
                0u8.hash(&mut h);
                hash_gate(gate, &mut h);
            }
            Node::Op {
                dialect,
                name,
                ty,
                attrs,
                ..
            } => {
                1u8.hash(&mut h);
                dialect.hash(&mut h);
                name.hash(&mut h);
                ty.hash(&mut h);
                hash_attrs(attrs, &mut h);
            }
            Node::Const { value, .. } => {
                2u8.hash(&mut h);
                value.width().hash(&mut h);
                value.to_u64().hash(&mut h);
            }
        }
        h.finish()
    }

    /// Search-index bucket: like [`hash_cons`](Self::hash_cons) but omits an `Op`'s
    /// result type, because an LHS template wildcards it ([`any_type`]) yet must land
    /// in the same bucket as the concrete-typed ops it matches.
    fn op_key(&self) -> u64 {
        let Node::Op {
            dialect,
            name,
            attrs,
            ..
        } = self
        else {
            return self.hash_cons();
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        1u8.hash(&mut h);
        dialect.hash(&mut h);
        name.hash(&mut h);
        hash_attrs(attrs, &mut h);
        h.finish()
    }

    /// Operator identity only. `Op` keys on dialect+name, result type (a wildcard type
    /// in a pattern matches anything), and value attributes — so `cmpi slt` and `cmpi
    /// sgt` (different predicate attribute) or `trunci`s to different widths stay
    /// distinct. `Const` on width+bits; a gate on its kind, `args` disambiguating.
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Node::Gate(a, _), Node::Gate(b, _)) => gate_matches(a, b),
            (
                Node::Op {
                    dialect: d1,
                    name: n1,
                    ty: t1,
                    attrs: a1,
                    ..
                },
                Node::Op {
                    dialect: d2,
                    name: n2,
                    ty: t2,
                    attrs: a2,
                    ..
                },
            ) => d1 == d2 && n1 == n2 && type_matches(*t1, *t2) && a1 == a2,
            (Node::Const { value: a, .. }, Node::Const { value: b, .. }) => {
                a.width() == b.width() && a.to_u64() == b.to_u64()
            }
            _ => false,
        }
    }

    fn from_int(value: tir_adt::APInt) -> Option<Self> {
        Some(Node::Const {
            value: APInt::new(value.width(), value.to_u64()),
            origin: None,
        })
    }
}

/// Result types are equal, or one side is the pattern wildcard.
fn type_matches(a: TypeId, b: TypeId) -> bool {
    a == b || a == any_type() || b == any_type()
}

/// A gate matches another of its kind; an input also matches on its `ValueId`. The
/// `value`/`cond` payload is ignored — congruent gates over equal `args` are equal.
fn gate_matches(a: &GateNode, b: &GateNode) -> bool {
    match (a, b) {
        (GateNode::Input(x), GateNode::Input(y)) => x == y,
        (GateNode::Gamma { .. }, GateNode::Gamma { .. }) => true,
        (GateNode::Mu { .. }, GateNode::Mu { .. }) => true,
        (GateNode::Phi { .. }, GateNode::Phi { .. }) => true,
        _ => false,
    }
}

fn hash_gate(gate: &GateNode, h: &mut impl Hasher) {
    match gate {
        GateNode::Input(v) => {
            0u8.hash(h);
            v.hash(h);
        }
        GateNode::Gamma { .. } => 1u8.hash(h),
        GateNode::Mu { .. } => 2u8.hash(h),
        GateNode::Phi { .. } => 3u8.hash(h),
        GateNode::Op(_) => unreachable!("an op is a Node::Op, never a Node::Gate"),
    }
}

fn hash_attrs(attrs: &[NamedAttribute], h: &mut impl Hasher) {
    attrs.len().hash(h);
    for attr in attrs {
        attr.name.hash(h);
        hash_attr_value(&attr.value, h);
    }
}

fn hash_attr_value(value: &AttributeValue, h: &mut impl Hasher) {
    std::mem::discriminant(value).hash(h);
    match value {
        AttributeValue::Str(s) => s.hash(h),
        AttributeValue::Int(i) => i.hash(h),
        AttributeValue::UInt(u) => u.hash(h),
        AttributeValue::F32(f) => f.to_bits().hash(h),
        AttributeValue::F64(f) => f.to_bits().hash(h),
        AttributeValue::Bool(b) => b.hash(h),
        AttributeValue::Type(t) => t.hash(h),
        AttributeValue::Block(b) => b.hash(h),
        AttributeValue::Array(a) => a.iter().for_each(|v| hash_attr_value(v, h)),
        AttributeValue::Dict(d) => d.iter().for_each(|(k, v)| {
            k.hash(h);
            hash_attr_value(v, h);
        }),
        // Register attributes are machine IR; InstCombine never sees them.
        AttributeValue::Register(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use tir_symbolic::egraph::{EGraph, Pattern, Var};

    use super::*;

    fn konst(width: u32, value: u64) -> Node {
        Node::Const {
            value: APInt::new(width, value),
            origin: None,
        }
    }

    fn ty(n: u32) -> TypeId {
        TypeId::from_number(n)
    }

    // These exercise the `ENode` hash-cons paths that lit can't reach: `cmpi` (whose
    // predicate attribute distinguishes value identity) is not parseable in `.tir`, and
    // a constant's width/bits identity has no `.tir`-observable surface. Op result-type
    // identity *is* covered end-to-end by core/checks/InstCombine/type_in_key.tir.

    #[test]
    fn equal_constants_share_a_class_distinct_widths_do_not() {
        let mut g: EGraph<Node> = EGraph::new();
        let a = g.add(konst(32, 0));
        let b = g.add(konst(32, 0));
        let c = g.add(konst(64, 0));
        assert_eq!(g.find(a), g.find(b));
        assert_ne!(g.find(a), g.find(c));
    }

    // `cmpi slt` and `cmpi sgt` over the same operand differ only in the predicate
    // attribute, so they must not hash-cons together; identical predicates must.
    #[test]
    fn ops_differing_in_attributes_stay_distinct() {
        let mut g: EGraph<Node> = EGraph::new();
        let x = g.add(konst(32, 0));
        let cmpi = |pred: &str, args: Vec<Id>| Node::Op {
            dialect: "builtin",
            name: "cmpi",
            ty: ty(1),
            attrs: vec![NamedAttribute::new(
                "predicate",
                AttributeValue::Str(pred.to_string()),
            )],
            cost: 1,
            prov: OpProv::Introduced(0),
            args,
        };
        let slt = g.add(cmpi("slt", vec![x]));
        let sgt = g.add(cmpi("sgt", vec![x]));
        let slt2 = g.add(cmpi("slt", vec![x]));
        assert_ne!(g.find(slt), g.find(sgt));
        assert_eq!(g.find(slt), g.find(slt2));
    }

    // `op_key` drops the result type so a wildcard-typed LHS template buckets with
    // the concrete ops it matches. That coarser *search* key must never leak into
    // congruence: `addi i32` and `addi i64` over the same operands share an op_key
    // bucket (a wildcard search visits and matches both), yet must stay in distinct
    // classes — merging them would substitute a wrong-typed value (a miscompile).
    #[test]
    fn wildcard_search_groups_result_types_without_merging_them() {
        let mut g: EGraph<Node> = EGraph::new();
        let x = g.add(konst(32, 0));
        let addi = |t: TypeId, args: Vec<Id>| Node::Op {
            dialect: "builtin",
            name: "addi",
            ty: t,
            attrs: vec![],
            cost: 1,
            prov: OpProv::Introduced(0),
            args,
        };
        let a32 = g.add(addi(ty(32), vec![x, x]));
        let a64 = g.add(addi(ty(64), vec![x, x]));

        // hash_cons keeps the two result types in separate classes...
        assert_ne!(g.find(a32), g.find(a64));
        // ...even though they collide in the op_key search bucket.
        assert_eq!(addi(ty(32), vec![]).op_key(), addi(ty(64), vec![]).op_key());

        // A wildcard-typed template (`any_type`) matches both via the index.
        let mut p: Pattern<Node, u32> = Pattern::new();
        let v0 = p.var(Var::Symbol(0));
        let v1 = p.var(Var::Symbol(1));
        p.add(addi(any_type(), vec![v0, v1]));
        let roots: std::collections::HashSet<Id> =
            p.search(&g).iter().map(|m| g.find(m.root)).collect();
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&g.find(a32)) && roots.contains(&g.find(a64)));

        // Searching is read-only: the two classes remain distinct.
        assert_ne!(g.find(a32), g.find(a64));
    }
}
