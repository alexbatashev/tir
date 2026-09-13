use std::collections::{HashMap, HashSet};

use tir_adt::APInt;
use tir_graph::{Dag, GenericDag, MutDag, NodeId};

use crate::lang::types::TypeUnifier;
use crate::lang::{
    SemType, StateFieldKind, SymKind, SymPayload, TypeError, Width, WidthRule, scalar_op,
};

fn state_field_type(resource: u64, field: u64) -> Result<Option<SemType>, TypeError> {
    let schema = crate::lang::StateResourceKind::from_code(resource)
        .zip(StateFieldKind::from_code(field))
        .and_then(|(resource, field)| resource.field_schema(field))
        .ok_or(TypeError::InvalidStateField { resource, field })?;
    Ok(schema.bit_width.map(SemType::bits))
}

fn state_field_type_at<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    resource: NodeId,
    field: NodeId,
) -> Result<Option<SemType>, TypeError> {
    let (Some(resource), Some(field)) = (const_u64(graph, resource), const_u64(graph, field))
    else {
        return Ok(None);
    };
    state_field_type(resource, field)
}

/// Infer semantic value types by instantiating each operator's polymorphic
/// signature and unifying it with the types of its operands. `seed` supplies
/// externally known types, normally the IR types of symbol leaves and roots.
pub fn infer_types<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    seed: impl Fn(NodeId) -> Option<SemType>,
) -> Result<Vec<SemType>, TypeError> {
    let mut inference = TypeUnifier::default();
    let mut types: Vec<SemType> = Vec::with_capacity(graph.len());

    for index in 0..graph.len() {
        let node = NodeId::from_index(index);
        let children: Vec<NodeId> = graph.children(node).collect();
        let child = |slot: usize| types[children[slot].index()].clone();
        let kind = *graph.get_kind(node);
        if super::rounded::operation(kind).is_some() {
            inference.unify(&child(children.len() - 1), &SemType::bits(3))?;
        }
        let inferred = match kind {
            SymKind::Symbol | SymKind::Arg => inference.fresh_type(),
            SymKind::Constant => inference.fresh_bits(),
            SymKind::FAddRound | SymKind::FSubRound | SymKind::FMulRound | SymKind::FDivRound => {
                let ty = inference.fresh_float();
                inference.unify(&child(0), &ty)?;
                inference.unify(&child(1), &ty)?;
                SemType::Pair(Box::new(ty), Box::new(SemType::bits(5)))
            }
            SymKind::FAdd
            | SymKind::FSub
            | SymKind::FMul
            | SymKind::FDiv
            | SymKind::FMin
            | SymKind::FMax => {
                let ty = inference.fresh_float();
                inference.unify(&child(0), &ty)?;
                inference.unify(&child(1), &ty)?;
                ty
            }
            SymKind::SIToFP | SymKind::UIToFP | SymKind::SIToFPRound | SymKind::UIToFPRound => {
                let operand = inference.fresh_bits();
                inference.unify(&child(0), &operand)?;
                for slot in 1..3 {
                    let width = inference.fresh_bits();
                    inference.unify(&child(slot), &width)?;
                }
                let result = match (const_u64(graph, children[1]), const_u64(graph, children[2])) {
                    (Some(exponent), Some(mantissa)) => SemType::Float(
                        crate::lang::FloatFormat::new(exponent as u32, mantissa as u32),
                    ),
                    _ => inference.fresh_float(),
                };
                if matches!(kind, SymKind::SIToFPRound | SymKind::UIToFPRound) {
                    SemType::Pair(Box::new(result), Box::new(SemType::bits(5)))
                } else {
                    result
                }
            }
            SymKind::FPToSI | SymKind::FPToUI | SymKind::FPToSIRound | SymKind::FPToUIRound => {
                let operand = inference.fresh_float();
                inference.unify(&child(0), &operand)?;
                let width = inference.fresh_bits();
                inference.unify(&child(1), &width)?;
                let result = const_u64(graph, children[1])
                    .map(|width| SemType::bits(width as u32))
                    .unwrap_or_else(|| inference.fresh_bits());
                if matches!(kind, SymKind::FPToSIRound | SymKind::FPToUIRound) {
                    SemType::Pair(Box::new(result), Box::new(SemType::bits(5)))
                } else {
                    result
                }
            }
            SymKind::AsFloat => inference.fresh_float(),
            SymKind::FCvt | SymKind::FCvtRound => {
                let operand = inference.fresh_float();
                inference.unify(&child(0), &operand)?;
                for slot in 1..3 {
                    let width = inference.fresh_bits();
                    inference.unify(&child(slot), &width)?;
                }
                let result = match (const_u64(graph, children[1]), const_u64(graph, children[2])) {
                    (Some(exponent), Some(mantissa)) => SemType::Float(
                        crate::lang::FloatFormat::new(exponent as u32, mantissa as u32),
                    ),
                    _ => inference.fresh_float(),
                };
                if kind == SymKind::FCvtRound {
                    SemType::Pair(Box::new(result), Box::new(SemType::bits(5)))
                } else {
                    result
                }
            }
            SymKind::Eq | SymKind::Ne | SymKind::Lt | SymKind::Le | SymKind::Gt | SymKind::Ge => {
                let operand = inference.fresh_type();
                inference.unify(&child(0), &operand)?;
                inference.unify(&child(1), &operand)?;
                SemType::bits(1)
            }
            SymKind::ULt | SymKind::ULe | SymKind::UGt | SymKind::UGe => {
                let operand = inference.fresh_bits();
                inference.unify(&child(0), &operand)?;
                inference.unify(&child(1), &operand)?;
                SemType::bits(1)
            }
            SymKind::Add
            | SymKind::Sub
            | SymKind::Mul
            | SymKind::Div
            | SymKind::UDiv
            | SymKind::SRem
            | SymKind::URem
            | SymKind::Or
            | SymKind::And
            | SymKind::Xor
            | SymKind::Xnor => {
                let operand = inference.fresh_bits();
                inference.unify(&child(0), &operand)?;
                inference.unify(&child(1), &operand)?;
                operand
            }
            SymKind::Neg | SymKind::Not => {
                let operand = inference.fresh_bits();
                inference.unify(&child(0), &operand)?;
                operand
            }
            SymKind::ShiftLeft | SymKind::ShiftRightArithmetic | SymKind::ShiftRightLogic => {
                let value = inference.fresh_bits();
                let amount = inference.fresh_bits();
                inference.unify(&child(0), &value)?;
                inference.unify(&child(1), &amount)?;
                value
            }
            SymKind::Concat => {
                let lhs = inference.fresh_width();
                let rhs = inference.fresh_width();
                inference.unify(&child(0), &SemType::Bits(lhs.clone()))?;
                inference.unify(&child(1), &SemType::Bits(rhs.clone()))?;
                SemType::Bits(Width::Add(Box::new(lhs), Box::new(rhs)))
            }
            SymKind::Bitcast => {
                let width = inference.fresh_width();
                inference.unify(&child(0), &SemType::RawBits(width.clone()))?;
                SemType::RawBits(width)
            }
            SymKind::If => {
                inference.unify(&child(0), &SemType::bits(1))?;
                let result = inference.fresh_type();
                inference.unify(&child(1), &result)?;
                inference.unify(&child(2), &result)?;
                result
            }
            SymKind::Theta => {
                let result = inference.fresh_type();
                inference.unify(&child(0), &result)?;
                inference.unify(&child(1), &result)?;
                result
            }
            SymKind::Loop => {
                let result = inference.fresh_type();
                inference.unify(&child(0), &result)?;
                inference.unify(&child(1), &result)?;
                inference.unify(&child(2), &result)?;
                inference.unify(&child(3), &SemType::bits(1))?;
                result
            }
            SymKind::Port => inference.fresh_type(),
            SymKind::Switch => {
                let result = inference.fresh_type();
                for arm in 1..children.len() {
                    inference.unify(&child(arm), &result)?;
                }
                result
            }
            SymKind::SExt | SymKind::ZExt => {
                let value = inference.fresh_bits();
                let width = inference.fresh_bits();
                inference.unify(&child(0), &value)?;
                inference.unify(&child(1), &width)?;
                const_u64(graph, children[1])
                    .map(|width| SemType::bits(width as u32))
                    .unwrap_or_else(|| inference.fresh_bits())
            }
            SymKind::Extract => {
                let value = SemType::RawBits(inference.fresh_width());
                inference.unify(&child(0), &value)?;
                for slot in 1..3 {
                    let bound = inference.fresh_bits();
                    inference.unify(&child(slot), &bound)?;
                }
                match (const_u64(graph, children[1]), const_u64(graph, children[2])) {
                    (Some(high), Some(low)) if high >= low => {
                        SemType::bits((high - low + 1) as u32)
                    }
                    _ => inference.fresh_bits(),
                }
            }
            SymKind::Clamp | SymKind::Log2Ceil | SymKind::Sqrt => child(0),
            SymKind::SqrtRound => {
                let ty = inference.fresh_float();
                inference.unify(&child(0), &ty)?;
                SemType::Pair(Box::new(ty), Box::new(SemType::bits(5)))
            }
            SymKind::FPValue | SymKind::FPFlags => {
                let result = inference.fresh_type();
                inference.unify(
                    &child(0),
                    &SemType::Pair(Box::new(result.clone()), Box::new(SemType::bits(5))),
                )?;
                if kind == SymKind::FPValue {
                    result
                } else {
                    SemType::bits(5)
                }
            }
            SymKind::Fma => {
                let ty = inference.fresh_float();
                for slot in 0..3 {
                    inference.unify(&child(slot), &ty)?;
                }
                ty
            }
            SymKind::FmaRound => {
                let ty = inference.fresh_float();
                for slot in 0..3 {
                    inference.unify(&child(slot), &ty)?;
                }
                SemType::Pair(Box::new(ty), Box::new(SemType::bits(5)))
            }
            SymKind::LoadMemory | SymKind::LoadReserved => const_u64(graph, children[1])
                .map(|bytes| SemType::RawBits(Width::Const(bytes as u32 * 8)))
                .unwrap_or_else(|| SemType::RawBits(inference.fresh_width())),
            SymKind::StoreConditional => SemType::bits(1),
            SymKind::AtomicRmw => const_u64(graph, children[2])
                .map(|bytes| SemType::bits(bytes as u32 * 8))
                .unwrap_or_else(|| inference.fresh_bits()),
            SymKind::StoreMemory | SymKind::Fence => SemType::Unit,
            SymKind::Split => SemType::Iterator(Box::new(inference.fresh_type())),
            SymKind::Iota => {
                let lane = const_u64(graph, children[1])
                    .map(|w| SemType::bits(w as u32))
                    .unwrap_or_else(|| inference.fresh_bits());
                SemType::Iterator(Box::new(lane))
            }
            SymKind::Zip => {
                // An n-ary zip's element is its component types nested
                // right-associatively: zip(a, b, c) lanes are Pair(a, Pair(b, c)).
                let mut components = Vec::with_capacity(children.len());
                for slot in 0..children.len() {
                    let component = inference.fresh_type();
                    inference.unify(
                        &child(slot),
                        &SemType::Iterator(Box::new(component.clone())),
                    )?;
                    components.push(component);
                }
                let element = components
                    .into_iter()
                    .rev()
                    .reduce(|rest, component| SemType::Pair(Box::new(component), Box::new(rest)))
                    .expect("zip requires at least 2 operands");
                SemType::Iterator(Box::new(element))
            }
            SymKind::Map => {
                let element = inference.fresh_type();
                inference.unify(&child(0), &SemType::Iterator(Box::new(element)))?;
                SemType::Iterator(Box::new(child(1)))
            }
            SymKind::IterConcat => inference.fresh_bits(),
            SymKind::Reduce => child(1),
            SymKind::StateAssign => {
                if !children.is_empty() {
                    inference.unify(&child(0), &SemType::State)?;
                }
                if children.len() >= 5
                    && let Some(field_type) = state_field_type_at(graph, children[1], children[2])?
                {
                    inference.unify(&child(4), &field_type)?;
                }
                SemType::State
            }
            SymKind::StateStore
            | SymKind::StateStoreConditional
            | SymKind::StateFence
            | SymKind::StateTrap
            | SymKind::StateBlock
            | SymKind::StateIf
            | SymKind::StateTry
            | SymKind::StateHandler => SemType::State,
            SymKind::StateRead => {
                inference.unify(&child(0), &SemType::State)?;
                state_field_type_at(graph, children[1], children[2])?
                    .unwrap_or_else(|| inference.fresh_bits())
            }
            SymKind::StateResult => {
                for slot in 1..children.len() {
                    inference.unify(&child(slot), &SemType::State)?;
                }
                child(0)
            }
        };
        if let Some(expected) = seed(node) {
            inference.unify(&inferred, &expected)?;
        }
        types.push(inferred);
    }

    Ok(types.iter().map(|ty| inference.resolve(ty)).collect())
}

/// Infer each node's integer bit-width bottom-up; `leaf_width` supplies `Symbol`
/// widths, `None` means unknown and propagates. Relies on children having lower
/// indices than parents (holds for post-order graphs); result indexed by node index.
pub fn infer_widths<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    leaf_width: impl Fn(NodeId) -> Option<u32>,
) -> Vec<Option<u32>> {
    let count = graph.len();
    let mut widths = vec![None; count];

    for index in 0..count {
        let id = NodeId::from_index(index);
        let children: Vec<NodeId> = graph.children(id).collect();
        let child_width = |slot: usize| children.get(slot).and_then(|c| widths[c.index()]);

        let kind = *graph.get_kind(id);
        let width = if let Some(op) = scalar_op(kind) {
            match op.width {
                WidthRule::First => child_width(0),
                WidthRule::Bool => Some(1),
                WidthRule::Sum => match (child_width(0), child_width(1)) {
                    (Some(lhs), Some(rhs)) => Some(lhs + rhs),
                    _ => None,
                },
            }
        } else {
            match kind {
                SymKind::Symbol => leaf_width(id),
                SymKind::Constant => match graph.get_leaf_data(id) {
                    Some(SymPayload::<V>::Int(value)) => Some(value.width()),
                    _ => None,
                },

                SymKind::FPFlags => Some(5),
                SymKind::FPValue => child_width(0),
                SymKind::FAddRound
                | SymKind::FSubRound
                | SymKind::FMulRound
                | SymKind::FDivRound
                | SymKind::FmaRound
                | SymKind::SqrtRound
                | SymKind::Clamp
                | SymKind::Bitcast
                | SymKind::Log2Ceil
                | SymKind::Sqrt
                | SymKind::Fma
                | SymKind::FAdd
                | SymKind::FSub
                | SymKind::FMul
                | SymKind::FDiv
                | SymKind::FMin
                | SymKind::FMax => child_width(0),

                SymKind::SIToFP | SymKind::UIToFP | SymKind::SIToFPRound | SymKind::UIToFPRound => {
                    match (
                        children.get(1).and_then(|&c| const_u64(graph, c)),
                        children.get(2).and_then(|&c| const_u64(graph, c)),
                    ) {
                        (Some(exponent), Some(mantissa)) => {
                            Some(1 + exponent as u32 + mantissa as u32)
                        }
                        _ => None,
                    }
                }

                SymKind::AsFloat => child_width(0),

                SymKind::FCvt | SymKind::FCvtRound => match (
                    children.get(1).and_then(|&c| const_u64(graph, c)),
                    children.get(2).and_then(|&c| const_u64(graph, c)),
                ) {
                    (Some(exponent), Some(mantissa)) => Some(1 + exponent as u32 + mantissa as u32),
                    _ => None,
                },

                SymKind::FPToSI | SymKind::FPToUI | SymKind::FPToSIRound | SymKind::FPToUIRound => {
                    children
                        .get(1)
                        .and_then(|&c| const_u64(graph, c))
                        .map(|width| width as u32)
                }

                // As wide as its arms (the then-branch).
                SymKind::If | SymKind::Switch => child_width(1),
                SymKind::Theta | SymKind::Loop => child_width(0),
                SymKind::Port => None,

                SymKind::Extract => {
                    match (
                        children.get(1).and_then(|&c| const_u64(graph, c)),
                        children.get(2).and_then(|&c| const_u64(graph, c)),
                    ) {
                        (Some(high), Some(low)) if high >= low => Some((high - low + 1) as u32),
                        _ => None,
                    }
                }

                SymKind::SExt | SymKind::ZExt => children
                    .get(1)
                    .and_then(|&c| const_u64(graph, c))
                    .map(|w| w as u32),

                SymKind::LoadMemory => children
                    .get(1)
                    .and_then(|&c| const_u64(graph, c))
                    .map(|bytes| (bytes as u32) * 8),
                SymKind::StoreMemory => None,

                SymKind::LoadReserved => children
                    .get(1)
                    .and_then(|&c| const_u64(graph, c))
                    .map(|bytes| (bytes as u32) * 8),
                SymKind::AtomicRmw => children
                    .get(2)
                    .and_then(|&c| const_u64(graph, c))
                    .map(|bytes| (bytes as u32) * 8),
                SymKind::StoreConditional => Some(1),
                SymKind::Fence => None,

                SymKind::StateRead => children
                    .get(1)
                    .zip(children.get(2))
                    .and_then(|(&resource, &field)| {
                        state_field_type_at(graph, resource, field).ok().flatten()
                    })
                    .and_then(|ty| match ty {
                        SemType::Bits(Width::Const(width)) => Some(width),
                        _ => None,
                    }),
                SymKind::StateResult => child_width(0),
                SymKind::StateAssign
                | SymKind::StateStore
                | SymKind::StateStoreConditional
                | SymKind::StateFence
                | SymKind::StateTrap
                | SymKind::StateBlock
                | SymKind::StateIf
                | SymKind::StateTry
                | SymKind::StateHandler => None,

                // No scalar width: element widths come from the runtime value, not structure.
                SymKind::Map
                | SymKind::Zip
                | SymKind::IterConcat
                | SymKind::Split
                | SymKind::Iota
                | SymKind::Reduce
                | SymKind::Arg => None,
                _ => unreachable!("operator has no width rule"),
            }
        };

        widths[index] = width;
    }

    widths
}

/// Rewrite a behavior-derived pattern into the form isel matches against, returning
/// the new graph, root, and forced widths (indexed by new node index). The rewrites
/// (see `canon_rebuild`) only simplify the selection pattern, never execution semantics.
pub fn canonicalize_for_selection<V: Clone>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    root: NodeId,
    immediate_symbols: &HashSet<u32>,
) -> (GenericDag<SymKind, SymPayload<V>>, NodeId, Vec<Option<u32>>) {
    let mut out = GenericDag::new();
    let mut memo: HashMap<usize, NodeId> = HashMap::new();
    let mut forced: HashMap<usize, u32> = HashMap::new();
    let mut new_root = canon_rebuild(
        graph,
        root,
        immediate_symbols,
        &mut out,
        &mut memo,
        &mut forced,
    );

    // Propagate the forced leaf widths (from the extract/extension collapses) through
    // inference so interior narrow arithmetic is typed too: a collapsed extension pins
    // its whole sub-tree to a known width, and a node left untyped here would be read
    // as width-sensitive and demand full-width operands (e.g. the `Div` inside riscv
    // `remw`'s `Sub(x, Mul(Div(x,y), y))`). Forced widths still win over inference.
    let inferred = infer_widths(&out, |node| forced.get(&node.index()).copied());
    if *out.get_node(new_root) == SymKind::If {
        let children: Vec<_> = out.children(new_root).collect();
        if let [condition, then_value, else_value] = children.as_slice()
            && inferred[condition.index()] == Some(1)
            && inferred[then_value.index()].is_some()
            && inferred[then_value.index()] == inferred[else_value.index()]
            && const_u64(&out, *then_value) == Some(1)
            && const_u64(&out, *else_value) == Some(0)
        {
            // A register-wide 0/1 result observes exactly the logical low bit.
            new_root = *condition;
        }
    }
    let widths = (0..out.len())
        .map(|index| forced.get(&index).copied().or(inferred[index]))
        .collect();
    (out, new_root, widths)
}

fn const_u64<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
) -> Option<u64> {
    match graph.get_leaf_data(node)? {
        SymPayload::Int(v) => Some(v.to_u64()),
        _ => None,
    }
}

fn is_shift(kind: SymKind) -> bool {
    matches!(
        kind,
        SymKind::ShiftLeft | SymKind::ShiftRightLogic | SymKind::ShiftRightArithmetic
    )
}

/// The shift-amount operand's source with its implicit encoding mask stripped:
/// `Extract(amt, k, 0)` / `Clamp(amt, _, _)` -> `amt` (the shift encoding masks
/// the amount, so the mask is redundant for matching).
fn shift_amount_src<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    src: NodeId,
) -> NodeId {
    match *graph.get_node(src) {
        SymKind::Extract => {
            let ec: Vec<NodeId> = graph.children(src).collect();
            if ec.len() == 3 && const_u64(graph, ec[2]) == Some(0) {
                ec[0]
            } else {
                src
            }
        }
        SymKind::Clamp => graph.children(src).next().unwrap_or(src),
        _ => src,
    }
}

fn extract_from_zero_hi<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
) -> Option<(NodeId, u64)> {
    if *graph.get_node(node) != SymKind::Extract {
        return None;
    }
    let children: Vec<NodeId> = graph.children(node).collect();
    if children.len() != 3 || const_u64(graph, children[2]) != Some(0) {
        return None;
    }
    const_u64(graph, children[1]).map(|hi| (children[0], hi))
}

/// Whether `node` is a low slice `Extract(x, hi, 0)` (lo == 0), a re-view of the
/// low bits of `x`. The hi bound may be constant or symbolic (`XLEN - 1`).
fn is_low_extract<V>(graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>, node: NodeId) -> bool {
    *graph.get_node(node) == SymKind::Extract && {
        let children: Vec<NodeId> = graph.children(node).collect();
        children.len() == 3 && const_u64(graph, children[2]) == Some(0)
    }
}

fn is_immediate_leaf<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
    immediate_symbols: &HashSet<u32>,
) -> bool {
    *graph.get_node(node) == SymKind::Symbol
        && matches!(
            graph.get_leaf_data(node),
            Some(SymPayload::SymbolId(id)) if immediate_symbols.contains(id)
        )
}

fn is_extended_zero<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
) -> bool {
    if *graph.get_node(node) != SymKind::ZExt {
        return false;
    }
    let children: Vec<NodeId> = graph.children(node).collect();
    children.len() == 2 && const_u64(graph, children[0]) == Some(0)
}

fn canon_rebuild<V: Clone>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
    immediate_symbols: &HashSet<u32>,
    out: &mut GenericDag<SymKind, SymPayload<V>>,
    memo: &mut HashMap<usize, NodeId>,
    forced: &mut HashMap<usize, u32>,
) -> NodeId {
    if let Some(&existing) = memo.get(&node.index()) {
        return existing;
    }

    let kind = *graph.get_node(node);
    let children: Vec<NodeId> = graph.children(node).collect();

    if kind == SymKind::Add
        && let [lhs, rhs] = children.as_slice()
        && let Some(value) = if is_extended_zero(graph, *lhs) {
            Some(*rhs)
        } else if is_extended_zero(graph, *rhs) {
            Some(*lhs)
        } else {
            None
        }
    {
        let value = canon_rebuild(graph, value, immediate_symbols, out, memo, forced);
        memo.insert(node.index(), value);
        return value;
    }

    // Target behaviors spell width-specific literals as extended bitvectors;
    // source IR carries the same literal directly at its use width.
    if matches!(kind, SymKind::SExt | SymKind::ZExt)
        && children.len() == 2
        && let Some(SymPayload::Int(value)) = graph.get_leaf_data(children[0])
        && let Some(width) =
            const_u64(graph, children[1]).and_then(|width| u32::try_from(width).ok())
        && width >= value.width()
    {
        let value = if kind == SymKind::SExt {
            value.with_signed(true).sign_extend(width)
        } else {
            value.zero_extend(width)
        }
        .with_signed(true);
        let constant = out.add_node(SymKind::Constant);
        out.set_leaf_data(constant, SymPayload::Int(value));
        memo.insert(node.index(), constant);
        return constant;
    }

    // Collapse `sext/zext(load/imm, XLEN)` to the bare load or immediate: source IR
    // types the load result and carries constants at use width, rather than wrapping.
    if matches!(kind, SymKind::SExt | SymKind::ZExt)
        && children.len() == 2
        && (*graph.get_node(children[0]) == SymKind::LoadMemory
            || is_immediate_leaf(graph, children[0], immediate_symbols))
    {
        let inner = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        memo.insert(node.index(), inner);
        return inner;
    }

    // Collapse `sext/zext(e, w)` to the bare `e` forced to its inferred width `n`
    // when `e` roots pure integer arithmetic that inference pins to a known width:
    // selection canonicalization runs under the junk-upper Narrow contract, so a
    // pattern that materializes `ext(e_n, w)` provides at least the low `n` bits of
    // `e_n`, and consumers of a narrow class re-narrow through their own operand-width
    // constraints — the extension is a view, not a computation. This is what types
    // interior narrow arithmetic (e.g. the `Div` inside riscv `remw`'s Euclidean
    // `Sub(x, Mul(Div(x,y), y))`) and typed state fields. The allowlist excludes comparisons: `zext(x < y,
    // w)` is the register materialization of a 1-bit result (riscv `slt`), where the
    // extension is a computation, not a view, and collapsing it would drop the
    // materializer the rule set requires for that comparison kind.
    if matches!(kind, SymKind::SExt | SymKind::ZExt)
        && children.len() == 2
        && matches!(
            graph.get_node(children[0]),
            SymKind::Add
                | SymKind::Sub
                | SymKind::Mul
                | SymKind::Div
                | SymKind::UDiv
                | SymKind::SRem
                | SymKind::URem
                | SymKind::And
                | SymKind::Or
                | SymKind::Xor
                | SymKind::ShiftLeft
                | SymKind::ShiftRightLogic
                | SymKind::ShiftRightArithmetic
                | SymKind::FPToSI
                | SymKind::FPToUI
                | SymKind::StateRead
        )
        && let Some(width) = infer_widths(graph, |_| None)[children[0].index()]
    {
        let inner = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        forced.insert(inner.index(), width);
        memo.insert(node.index(), inner);
        return inner;
    }

    // SExt(Extract(inner, hi, 0), _) -> inner forced to width hi+1.
    if kind == SymKind::SExt
        && children.len() == 2
        && let Some((source, hi)) = extract_from_zero_hi(graph, children[0])
    {
        let inner = canon_rebuild(graph, source, immediate_symbols, out, memo, forced);
        forced.insert(inner.index(), (hi + 1) as u32);
        memo.insert(node.index(), inner);
        return inner;
    }

    // SExt(shr(Extract(v, hi, 0), amt), _) -> shr(v, amt) forced to width hi+1:
    // a word right shift computes the low hi+1 bits of its narrowed operand,
    // held sign-extended in the register (the register form), so matching sees
    // the narrow shift — the mirror of the `SExt(Extract(shl ...))` collapse
    // that covers the word left shift (`sllw`), whose extract sits outside.
    if kind == SymKind::SExt
        && children.len() == 2
        && matches!(
            *graph.get_node(children[0]),
            SymKind::ShiftRightLogic | SymKind::ShiftRightArithmetic
        )
    {
        let shift = children[0];
        let sc: Vec<NodeId> = graph.children(shift).collect();
        if sc.len() == 2
            && let Some((value_src, hi)) = extract_from_zero_hi(graph, sc[0])
        {
            let value = canon_rebuild(graph, value_src, immediate_symbols, out, memo, forced);
            forced.insert(value.index(), (hi + 1) as u32);
            let amount = canon_rebuild(
                graph,
                shift_amount_src(graph, sc[1]),
                immediate_symbols,
                out,
                memo,
                forced,
            );
            let new_node = out.add_node(*graph.get_node(shift));
            out.add_edge(new_node, value);
            out.add_edge(new_node, amount);
            forced.insert(new_node.index(), (hi + 1) as u32);
            memo.insert(node.index(), new_node);
            return new_node;
        }
    }

    // Extract(x, hi, 0) -> x (the value model: a low slice is the low hi+1 bits
    // of x, so the pattern matches the narrow value directly) — e.g. arm64 `mul`
    // = `extract(rn * rm, XLEN - 1, 0)` roots the same-width `Mul`. A constant hi
    // forces the width; a symbolic hi (`XLEN - 1`, a full-width identity) leaves
    // it to inference. A high slice (`lo > 0`, e.g. `smulh`) is left intact.
    //
    // Sound today because every symbolic-hi slice in the model is a full-width
    // identity (hi + 1 == the register width), so dropping it changes nothing.
    // It would be unsound for a symbolic *partial* slice — e.g. a hypothetical
    // `extract(x, vl - 1, 0)` with `vl` a dynamic sub-width — which this would
    // mis-collapse to the full value; add a width guard here if such a behavior
    // is introduced.
    if is_low_extract(graph, node) {
        let mut ch = graph.children(node);
        let source = ch.next().expect("extract has a value operand");
        let hi = const_u64(graph, ch.next().expect("extract has a hi operand"));
        let inner = canon_rebuild(graph, source, immediate_symbols, out, memo, forced);
        if let Some(hi) = hi {
            forced.insert(inner.index(), (hi + 1) as u32);
        }
        memo.insert(node.index(), inner);
        return inner;
    }

    // Shift-amount mask strip (mask is implicit in the encoding):
    //   Shift(v, Extract(amt, k, 0)) / Shift(v, Clamp(amt, _, _)) -> Shift(v, amt)
    if is_shift(kind) && children.len() == 2 {
        let value = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        let amount = canon_rebuild(
            graph,
            shift_amount_src(graph, children[1]),
            immediate_symbols,
            out,
            memo,
            forced,
        );
        let new_node = out.add_node(kind);
        out.add_edge(new_node, value);
        out.add_edge(new_node, amount);
        memo.insert(node.index(), new_node);
        return new_node;
    }

    // Normalize the load's metadata child to 0 so source IR loads match both signed
    // and unsigned target forms; signedness lives in the surrounding SExt/ZExt.
    if kind == SymKind::LoadMemory && children.len() == 3 {
        let address = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        let bytes = canon_rebuild(graph, children[1], immediate_symbols, out, memo, forced);
        let zero = out.add_node(SymKind::Constant);
        out.set_leaf_data(zero, SymPayload::Int(APInt::new(1, 0)));
        let new_node = out.add_node(kind);
        out.add_edge(new_node, address);
        out.add_edge(new_node, bytes);
        out.add_edge(new_node, zero);
        memo.insert(node.index(), new_node);
        return new_node;
    }

    // Collapse the explicit store truncation `extract(rs, 31, 0)` to the inner value,
    // forcing its width; source IR already carries the stored value's width.
    if kind == SymKind::StoreMemory && children.len() == 4 {
        let address = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        let bytes = canon_rebuild(graph, children[1], immediate_symbols, out, memo, forced);
        let value_src = children[2];
        let value = if let Some((source, hi)) = extract_from_zero_hi(graph, value_src) {
            let inner = canon_rebuild(graph, source, immediate_symbols, out, memo, forced);
            forced.insert(inner.index(), (hi + 1) as u32);
            inner
        } else {
            canon_rebuild(graph, value_src, immediate_symbols, out, memo, forced)
        };
        let address_space = canon_rebuild(graph, children[3], immediate_symbols, out, memo, forced);
        let new_node = out.add_node(kind);
        out.add_edge(new_node, address);
        out.add_edge(new_node, bytes);
        out.add_edge(new_node, value);
        out.add_edge(new_node, address_space);
        memo.insert(node.index(), new_node);
        return new_node;
    }

    let children = children.as_slice();

    // Default: copy leaves, rebuild operations from canonicalized children.
    let new_node = if children.is_empty() {
        let new_node = out.add_node(kind);
        if let Some(data) = graph.get_leaf_data(node) {
            out.set_leaf_data(new_node, data.clone());
        }
        new_node
    } else {
        let new_children: Vec<NodeId> = children
            .iter()
            .map(|&child| canon_rebuild(graph, child, immediate_symbols, out, memo, forced))
            .collect();
        let new_node = out.add_node(kind);
        for child in new_children {
            out.add_edge(new_node, child);
        }
        new_node
    };
    memo.insert(node.index(), new_node);
    new_node
}

fn selection_default_kind<A>(graph: &crate::sem::SemGraph<A>, node: NodeId) -> Option<SymKind> {
    let (kind, rounding) = match graph.get_kind(node) {
        SymKind::FCvtRound => (SymKind::FCvt, 0),
        SymKind::FAddRound => (SymKind::FAdd, 0),
        SymKind::FSubRound => (SymKind::FSub, 0),
        SymKind::FMulRound => (SymKind::FMul, 0),
        SymKind::FDivRound => (SymKind::FDiv, 0),
        SymKind::FmaRound => (SymKind::Fma, 0),
        SymKind::SqrtRound => (SymKind::Sqrt, 0),
        SymKind::SIToFPRound => (SymKind::SIToFP, 0),
        SymKind::UIToFPRound => (SymKind::UIToFP, 0),
        SymKind::FPToSIRound => (SymKind::FPToSI, 1),
        SymKind::FPToUIRound => (SymKind::FPToUI, 1),
        _ => return None,
    };
    (graph
        .children(node)
        .last()
        .and_then(|rm| const_u64(graph, rm))
        == Some(rounding))
    .then_some(kind)
}

fn selection_fallback_impl<A: Clone>(
    graph: &crate::sem::SemGraph<A>,
    root: NodeId,
    project_values: bool,
) -> Option<crate::sem::SemGraph<A>> {
    fn rounded_kind<A>(graph: &crate::sem::SemGraph<A>, node: NodeId) -> bool {
        matches!(
            graph.get_kind(node),
            SymKind::FAddRound
                | SymKind::FSubRound
                | SymKind::FMulRound
                | SymKind::FDivRound
                | SymKind::FmaRound
                | SymKind::SqrtRound
                | SymKind::FCvtRound
                | SymKind::SIToFPRound
                | SymKind::UIToFPRound
                | SymKind::FPToSIRound
                | SymKind::FPToUIRound
        )
    }
    fn quiet_nan_constant<A>(graph: &crate::sem::SemGraph<A>, node: NodeId) -> bool {
        if *graph.get_kind(node) != SymKind::AsFloat {
            return false;
        }
        if graph
            .preorder(node)
            .any(|candidate| *graph.get_kind(candidate) == SymKind::Symbol)
        {
            return false;
        }
        let mut constant = crate::sem::SemGraph::<()>::new();
        crate::sem::copy_subgraph(&mut constant, graph, node, &mut HashMap::new());
        let crate::lang::Value::Float(value) = crate::lang::execute(&constant, &[]) else {
            return false;
        };
        let bits = value.to_bits() as u64;
        match value.bit_width() {
            32 => bits & 0x7fc0_0000 == 0x7fc0_0000,
            64 => bits & 0x7ff8_0000_0000_0000 == 0x7ff8_0000_0000_0000,
            _ => false,
        }
    }
    fn rebuild<A: Clone>(
        graph: &crate::sem::SemGraph<A>,
        node: NodeId,
        out: &mut crate::sem::SemGraph<A>,
        memo: &mut HashMap<usize, NodeId>,
        changed: &mut bool,
        project_values: bool,
    ) -> NodeId {
        if let Some(&existing) = memo.get(&node.index()) {
            return existing;
        }
        let mut kind = *graph.get_kind(node);
        if kind == SymKind::FPFlags {
            let child = graph.children(node).next().expect("fp_flags has one child");
            let child = rebuild_outcome(graph, child, out, memo, changed, project_values);
            let result = out.add_node(SymKind::FPFlags);
            out.add_edge(result, child);
            memo.insert(node.index(), result);
            return result;
        }
        if kind == SymKind::FPValue {
            let outcome = graph.children(node).next().expect("fp_value has one child");
            if let Some(generic) = selection_default_kind(graph, outcome)
                && (project_values || matches!(generic, SymKind::FPToSI | SymKind::FPToUI))
            {
                *changed = true;
                let mut children: Vec<_> = graph.children(outcome).collect();
                children.pop();
                let children: Vec<_> = children
                    .into_iter()
                    .map(|child| rebuild(graph, child, out, memo, changed, project_values))
                    .collect();
                let result = out.add_node(generic);
                for child in children {
                    out.add_edge(result, child);
                }
                memo.insert(node.index(), result);
                return result;
            }
            let copied_outcome =
                rebuild_outcome(graph, outcome, out, memo, changed, project_values);
            let result = out.add_node(SymKind::FPValue);
            out.add_edge(result, copied_outcome);
            memo.insert(node.index(), result);
            return result;
        }
        if kind == SymKind::StateAssign {
            let result = crate::sem::copy_subgraph(out, graph, node, &mut HashMap::new());
            memo.insert(node.index(), result);
            return result;
        }
        let mut children: Vec<_> = graph.children(node).collect();
        if matches!(kind, SymKind::If | SymKind::StateIf) {
            let has_default = |root| {
                graph
                    .preorder(root)
                    .any(|node| selection_default_kind(graph, node).is_some())
            };
            let default_branch = if has_default(children[2]) {
                Some(children[2])
            } else if has_default(children[1]) {
                Some(children[1])
            } else {
                None
            };
            let rounded_nan_branch = if graph
                .preorder(children[1])
                .any(|node| rounded_kind(graph, node))
                && quiet_nan_constant(graph, children[2])
            {
                Some(children[1])
            } else if graph
                .preorder(children[2])
                .any(|node| rounded_kind(graph, node))
                && quiet_nan_constant(graph, children[1])
            {
                Some(children[2])
            } else {
                None
            };
            let branch = rounded_nan_branch
                .or(default_branch)
                .or_else(|| (kind == SymKind::StateIf).then_some(children[2]));
            if let Some(branch) = branch {
                *changed = true;
                let result = rebuild(graph, branch, out, memo, changed, project_values);
                memo.insert(node.index(), result);
                return result;
            }
        }
        if let Some(generic) = selection_default_kind(graph, node) {
            *changed = true;
            kind = generic;
            children.pop();
        }
        let children: Vec<_> = children
            .into_iter()
            .map(|child| rebuild(graph, child, out, memo, changed, project_values))
            .collect();
        let result = out.add_node(kind);
        if let Some(payload) = graph.get_leaf_data(node) {
            out.set_leaf_data(result, payload.clone());
        }
        for child in children {
            out.add_edge(result, child);
        }
        memo.insert(node.index(), result);
        result
    }

    fn rebuild_outcome<A: Clone>(
        graph: &crate::sem::SemGraph<A>,
        node: NodeId,
        out: &mut crate::sem::SemGraph<A>,
        memo: &mut HashMap<usize, NodeId>,
        changed: &mut bool,
        project_values: bool,
    ) -> NodeId {
        if let Some(&existing) = memo.get(&node.index()) {
            return existing;
        }
        let children = graph
            .children(node)
            .map(|child| rebuild(graph, child, out, memo, changed, project_values))
            .collect::<Vec<_>>();
        let result = out.add_node(*graph.get_kind(node));
        if let Some(payload) = graph.get_leaf_data(node) {
            out.set_leaf_data(result, payload.clone());
        }
        for child in children {
            out.add_edge(result, child);
        }
        memo.insert(node.index(), result);
        result
    }
    let mut out = crate::sem::SemGraph::new();
    let mut changed = false;
    rebuild(
        graph,
        root,
        &mut out,
        &mut HashMap::new(),
        &mut changed,
        project_values,
    );
    changed.then_some(out)
}

/// Propose an unguarded selection pattern. The caller must prove that the full
/// expression refines this candidate on the candidate's defined domain.
pub fn selection_fallback<A: Clone>(
    graph: &crate::sem::SemGraph<A>,
    root: NodeId,
) -> Option<crate::sem::SemGraph<A>> {
    selection_fallback_impl(graph, root, false)
}

/// Propose the numeric observation of a value or resource event. For an event,
/// the caller must separately prove that discarding its state outputs is sound.
/// Supported default-mode rounded outcomes collapse to the existing generic
/// scalar operator after value projection; other rounding modes remain explicit.
pub fn value_observation_fallback<A: Clone>(
    graph: &crate::sem::SemGraph<A>,
    root: NodeId,
) -> Option<crate::sem::SemGraph<A>> {
    let projected_state = *graph.get_kind(root) == SymKind::StateResult;
    let root = if projected_state {
        graph.children(root).next()?
    } else {
        root
    };
    if let Some(selected) = selection_fallback_impl(graph, root, true) {
        return Some(selected);
    }
    if !projected_state && *graph.get_kind(root) != SymKind::FPValue {
        return None;
    }
    let mut out = crate::sem::SemGraph::new();
    crate::sem::copy_subgraph(&mut out, graph, root, &mut HashMap::new());
    Some(out)
}
