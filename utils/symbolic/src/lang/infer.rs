use std::collections::{HashMap, HashSet};

use tir_adt::APInt;
use tir_graph::{Dag, GenericDag, MutDag, NodeId};

use crate::lang::{SymKind, SymPayload, WidthRule, scalar_op};

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

                SymKind::Clamp
                | SymKind::Log2Ceil
                | SymKind::Sqrt
                | SymKind::Fma
                | SymKind::FAdd
                | SymKind::FSub
                | SymKind::FMul
                | SymKind::FDiv => child_width(0),

                // As wide as its arms (the then-branch).
                SymKind::If => child_width(1),

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

                // No scalar width: element widths come from the runtime value, not structure.
                SymKind::Map
                | SymKind::Zip
                | SymKind::IterConcat
                | SymKind::Split
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
    let new_root = canon_rebuild(
        graph,
        root,
        immediate_symbols,
        &mut out,
        &mut memo,
        &mut forced,
    );

    let mut widths = vec![None; out.len()];
    for (index, width) in forced {
        if let Some(slot) = widths.get_mut(index) {
            *slot = Some(width);
        }
    }
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
