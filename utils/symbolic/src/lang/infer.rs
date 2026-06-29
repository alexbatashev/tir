use std::collections::{HashMap, HashSet};

use tir_adt::APInt;
use tir_graph::{Dag, GenericDag, MutDag, NodeId};

use crate::lang::{SymKind, SymPayload};

/// Infer the integer bit-width of every node of a semantic-expression graph,
/// bottom-up, from the widths of its leaf operands.
///
/// `leaf_width(node)` supplies the width of a `Symbol` leaf (an operand — e.g. a
/// register is XLEN-wide, an immediate is its encoded width). `Constant` widths
/// come from the literal itself. Every internal node follows its kind's width
/// rule. `None` means "unknown" and propagates, so a partially-known graph still
/// infers everything it can.
///
/// This is the single shared width rule used by both the program-graph builder
/// (so the program is fully typed) and TMDL pattern generation (so rules carry
/// type constraints) — keeping the two sides consistent is what lets typed
/// patterns match.
///
/// The result is indexed by node index; it relies on children having lower
/// indices than their parent, which holds for the post-order graphs used here.
pub fn infer_widths<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    leaf_width: impl Fn(NodeId) -> Option<u32>,
) -> Vec<Option<u32>> {
    let count = graph.len();
    let mut widths = vec![None; count];

    let const_value = |id: NodeId| -> Option<u64> {
        match graph.get_leaf_data(id)? {
            SymPayload::<V>::Int(value) => Some(value.to_u64()),
            _ => None,
        }
    };

    for index in 0..count {
        let id = NodeId::from_index(index);
        let children: Vec<NodeId> = graph.children(id).collect();
        let child_width = |slot: usize| children.get(slot).and_then(|c| widths[c.index()]);

        let width = match *graph.get_kind(id) {
            SymKind::Symbol => leaf_width(id),
            SymKind::Constant => match graph.get_leaf_data(id) {
                Some(SymPayload::<V>::Int(value)) => Some(value.width()),
                _ => None,
            },

            // Arithmetic / logic / shifts produce a value as wide as their (left) input.
            SymKind::Add
            | SymKind::Sub
            | SymKind::Mul
            | SymKind::Div
            | SymKind::UDiv
            | SymKind::SRem
            | SymKind::URem
            | SymKind::Neg
            | SymKind::And
            | SymKind::Or
            | SymKind::Xor
            | SymKind::ShiftLeft
            | SymKind::ShiftRightLogic
            | SymKind::ShiftRightArithmetic
            | SymKind::Not
            | SymKind::Clamp
            | SymKind::Log2Ceil
            | SymKind::Sqrt
            | SymKind::Fma => child_width(0),

            // Comparisons produce a 1-bit boolean.
            SymKind::Eq
            | SymKind::Ne
            | SymKind::Lt
            | SymKind::Le
            | SymKind::Gt
            | SymKind::Ge
            | SymKind::ULt
            | SymKind::ULe
            | SymKind::UGt
            | SymKind::UGe => Some(1),

            // Concatenation sums the operand widths.
            SymKind::Concat => match (child_width(0), child_width(1)) {
                (Some(hi), Some(lo)) => Some(hi + lo),
                _ => None,
            },

            // `If(cond, then, else)` is as wide as its arms.
            SymKind::If => child_width(1),

            // `Extract(value, high, low)` yields `high - low + 1` bits.
            SymKind::Extract => {
                match (
                    children.get(1).and_then(|&c| const_value(c)),
                    children.get(2).and_then(|&c| const_value(c)),
                ) {
                    (Some(high), Some(low)) if high >= low => Some((high - low + 1) as u32),
                    _ => None,
                }
            }

            // Extensions widen to their target-width argument.
            SymKind::SExt | SymKind::ZExt => children
                .get(1)
                .and_then(|&c| const_value(c))
                .map(|w| w as u32),

            SymKind::LoadMemory => children
                .get(1)
                .and_then(|&c| const_value(c))
                .map(|bytes| (bytes as u32) * 8),
            SymKind::StoreMemory => None,

            // Iterator-valued nodes and lambda arguments carry no scalar width:
            // their element widths come from the runtime value, not the structure.
            SymKind::Map
            | SymKind::Zip
            | SymKind::IterConcat
            | SymKind::Split
            | SymKind::Reduce
            | SymKind::Arg => None,
        };

        widths[index] = width;
    }

    widths
}

/// Rewrite a behavior-derived pattern into the form instruction selection should
/// match against, returning the new graph, its root, and forced node widths
/// (indexed by the new node's index). `immediate_symbols` names the operand
/// symbols that are encoded immediates rather than registers.
///
/// The rewrites bridge "how the hardware computes" to "what the IR looks like":
/// - **Result-extension collapse:** `SExt(Extract(x, hi, 0), _)` becomes `x`, with
///   `x` forced to width `hi + 1`. So `addw`'s `sext(extract(a+b, 31, 0), XLEN)` is
///   an i32 `Add`, not a literal structure to match.
/// - **Shift-amount mask strip:** a shift whose amount is `Extract(amt, k, 0)` (the
///   encoding's 5/6-bit field) matches the plain `amt`.
/// - **Extension-of-load collapse:** `sext/zext(load(...), XLEN)` becomes the typed
///   load itself, since source IR types the load result instead of wrapping it.
/// - **Immediate-extension collapse:** `sext/zext(imm, XLEN)` becomes the bare
///   `imm` symbol, since source IR carries constants at their use width.
///
/// Execution semantics are untouched — only the selection pattern is simplified.
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

fn canon_const_u64<V>(
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

fn extract_from_zero_hi<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
) -> Option<(NodeId, u64)> {
    if *graph.get_node(node) != SymKind::Extract {
        return None;
    }
    let children: Vec<NodeId> = graph.children(node).collect();
    if children.len() != 3 || canon_const_u64(graph, children[2]) != Some(0) {
        return None;
    }
    canon_const_u64(graph, children[1]).map(|hi| (children[0], hi))
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

    // Extension-of-load collapse: lb/lh/lw-style behaviors extend the raw memory
    // bytes to XLEN (`sext(load(addr, 4, _), XLEN)`), but source IR types the
    // load result instead of wrapping it. Match the typed `LoadMemory` itself —
    // its width is inferred from the bytes operand.
    //
    // Immediate-extension collapse: behaviors widen an encoded immediate to the
    // computation width (`sext(imm, XLEN)`), but source IR carries constants at
    // their use width, so the canonical pattern binds the bare immediate.
    if matches!(kind, SymKind::SExt | SymKind::ZExt)
        && children.len() == 2
        && (*graph.get_node(children[0]) == SymKind::LoadMemory
            || is_immediate_leaf(graph, children[0], immediate_symbols))
    {
        let inner = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        memo.insert(node.index(), inner);
        return inner;
    }

    // Result-extension collapse: SExt(Extract(inner, hi, 0), _) -> inner @ width hi+1.
    if kind == SymKind::SExt
        && children.len() == 2
        && let Some((source, hi)) = extract_from_zero_hi(graph, children[0])
    {
        let inner = canon_rebuild(graph, source, immediate_symbols, out, memo, forced);
        forced.insert(inner.index(), (hi + 1) as u32);
        memo.insert(node.index(), inner);
        return inner;
    }

    // Shift-amount mask strip: the encoding's amount mask is implicit, so
    //   Shift(value, Extract(amt, k, 0))  -> Shift(value, amt)
    //   Shift(value, Clamp(amt, _, _))    -> Shift(value, amt)
    if is_shift(kind) && children.len() == 2 {
        let value = canon_rebuild(graph, children[0], immediate_symbols, out, memo, forced);
        let amount = {
            let src = children[1];
            let stripped = match *graph.get_node(src) {
                SymKind::Extract => {
                    let ec: Vec<NodeId> = graph.children(src).collect();
                    (ec.len() == 3 && canon_const_u64(graph, ec[2]) == Some(0)).then_some(ec[0])
                }
                SymKind::Clamp => graph.children(src).next(),
                _ => None,
            };
            canon_rebuild(
                graph,
                stripped.unwrap_or(src),
                immediate_symbols,
                out,
                memo,
                forced,
            )
        };
        let new_node = out.add_node(kind);
        out.add_edge(new_node, value);
        out.add_edge(new_node, amount);
        memo.insert(node.index(), new_node);
        return new_node;
    }

    // The third `load` operand records signedness/address-space metadata. The raw
    // memory value is unsigned bytes; explicit `SExt`/`ZExt` nodes carry
    // signedness for both isel and execution. Normalize the metadata child so
    // source IR loads can match signed and unsigned target load forms by their
    // surrounding extension.
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

    // Stores usually truncate the source register explicitly:
    // `store(addr, 4, extract(rs, 31, 0))`. Source IR already carries the stored
    // value's width, so canonicalize that extract to the inner value and force the
    // width on the matched operand.
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

    // Default: copy leaves verbatim, rebuild operations from canonicalized children.
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
