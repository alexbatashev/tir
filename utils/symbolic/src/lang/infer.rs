use tir_graph::{Dag, NodeId};

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
            | SymKind::Gt
            | SymKind::Ge
            | SymKind::ULt
            | SymKind::ULe
            | SymKind::UGt
            | SymKind::UGe => Some(1),

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
        };

        widths[index] = width;
    }

    widths
}
