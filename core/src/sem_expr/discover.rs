//! Discovery of algebraic identities by *testing*, not hand-authoring.
//!
//! Instruction selection needs target-independent bit-vector lemmas to bridge IR
//! operators that no single instruction implements (e.g. a sub-word sign extension)
//! to sequences that the target *does* have. Rather than writing those lemmas by
//! hand, we propose a candidate shape and confirm it against an
//! [`EquivalenceOracle`]. The default oracle evaluates both sides on many inputs
//! with the reference interpreter ([`super::execute`]); an SMT-backed oracle can be
//! slotted in later for a soundness proof. Confirmed shapes become e-graph rewrites
//! at the call site.

use crate::{
    graph::{Dag, MutDag, NodeId},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph, Value, execute},
    utils::APInt,
};

/// Decides whether two single-output expression graphs (over the same symbols)
/// compute the same value for every input of the given symbol widths.
pub trait EquivalenceOracle {
    fn equivalent(
        &self,
        lhs: &ExprPostGraph,
        rhs: &ExprPostGraph,
        symbol_widths: &[u32],
    ) -> bool;
}

/// Property-testing oracle: evaluates both graphs on boundary values plus a
/// deterministic pseudo-random spread per symbol. Sound enough to bootstrap the
/// standard bit-vector idioms; not a proof. Deterministic, so discovery is stable.
pub struct FuzzOracle {
    pub samples_per_symbol: usize,
}

impl Default for FuzzOracle {
    fn default() -> Self {
        Self {
            samples_per_symbol: 16,
        }
    }
}

impl EquivalenceOracle for FuzzOracle {
    fn equivalent(
        &self,
        lhs: &ExprPostGraph,
        rhs: &ExprPostGraph,
        symbol_widths: &[u32],
    ) -> bool {
        let value_sets: Vec<Vec<APInt>> = symbol_widths
            .iter()
            .map(|&w| sample_values(w, self.samples_per_symbol))
            .collect();

        let mut assignment = vec![0usize; symbol_widths.len()];
        loop {
            let inputs: Vec<Value> = assignment
                .iter()
                .enumerate()
                .map(|(i, &j)| Value::Int(value_sets[i][j].clone()))
                .collect();
            if !values_bit_eq(&execute(lhs, &inputs), &execute(rhs, &inputs)) {
                return false;
            }
            if !advance(&mut assignment, &value_sets) {
                return true;
            }
        }
    }
}

/// Mixed-radix odometer over the per-symbol value sets; returns false when wrapped.
fn advance(assignment: &mut [usize], value_sets: &[Vec<APInt>]) -> bool {
    for (slot, set) in assignment.iter_mut().zip(value_sets.iter()) {
        *slot += 1;
        if *slot < set.len() {
            return true;
        }
        *slot = 0;
    }
    false
}

fn values_bit_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            // Compare bit patterns over the common width, ignoring how each side's
            // signedness flag would sign- vs zero-extend `to_u64` past that width.
            let width = a.width();
            let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
            width == b.width() && (a.to_u64() & mask) == (b.to_u64() & mask)
        }
        _ => false,
    }
}

/// Boundary values (0, 1, all-ones, sign bit, alternating patterns) plus a small
/// deterministic LCG spread, all masked to `width` bits.
fn sample_values(width: u32, extra: usize) -> Vec<APInt> {
    let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
    let mut raw = vec![
        0u64,
        1,
        mask,
        1u64 << (width - 1),
        0x5555_5555_5555_5555 & mask,
        0xAAAA_AAAA_AAAA_AAAA & mask,
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..extra {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        raw.push(state & mask);
    }
    raw.sort_unstable();
    raw.dedup();
    // Flag the samples signed so the interpreter's arithmetic right shift performs a
    // true (sign-extending) shift, matching hardware `sra`; the logical shifts
    // (`srl`) and the masked bit-pattern comparison are unaffected by the flag.
    raw.into_iter()
        .map(|v| APInt::new(width, v).with_signed(true))
        .collect()
}

fn sym(g: &mut ExprPostGraph, id: u32) -> NodeId {
    let node = g.add_node(ExprKind::Symbol);
    g.set_leaf_data(node, ExprPayload::SymbolId(id));
    node
}

fn con(g: &mut ExprPostGraph, value: u64, width: u32) -> NodeId {
    let node = g.add_node(ExprKind::Constant);
    g.set_leaf_data(node, ExprPayload::Int(APInt::new(width, value)));
    node
}

fn op(g: &mut ExprPostGraph, kind: ExprKind, children: &[NodeId]) -> NodeId {
    let node = g.add_node(kind);
    for &child in children {
        g.add_edge(node, child);
    }
    node
}

/// The candidate realization of an extension as a shift pair, parameterized by the
/// source width `n` and register width `w`: `ext_kind(extract(x, n-1, 0), w)`.
fn ext_of_low_bits(ext_kind: ExprKind, n: u32, w: u32) -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let x = sym(&mut g, 0);
    let hi = con(&mut g, (n - 1) as u64, 16);
    let lo = con(&mut g, 0, 16);
    let extract = op(&mut g, ExprKind::Extract, &[x, hi, lo]);
    let width = con(&mut g, w as u64, 16);
    op(&mut g, ext_kind, &[extract, width]);
    g
}

/// `shr_kind(shl(x, k), k)` over `w`-bit values.
fn shift_pair(shr_kind: ExprKind, k: u32, w: u32) -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let x = sym(&mut g, 0);
    let amount = con(&mut g, k as u64, w);
    let shl = op(&mut g, ExprKind::ShiftLeft, &[x, amount]);
    let amount2 = con(&mut g, k as u64, w);
    op(&mut g, shr_kind, &[shl, amount2]);
    g
}

/// Representative `(source_width, register_width)` pairs the extension bridge is
/// confirmed against. Covering several widths is what justifies generalizing the
/// shift amount to the symbolic `w - n`.
const EXT_WIDTH_SAMPLES: &[(u32, u32)] = &[(8, 32), (16, 32), (8, 64), (16, 64), (32, 64)];

/// Confirm that extending the low `n` bits of a register (`ext_kind` ∈ {`SExt`,
/// `ZExt`}) equals `shr_kind(shl(x, w - n), w - n)` for every sampled width pair.
/// On success the caller may emit a width-parameterized rewrite
/// `ext_kind(v, w) -> shr_kind(shl(v, w - n), w - n)` with `n = width(v)`.
pub fn confirm_extension_via_shifts(
    ext_kind: ExprKind,
    shr_kind: ExprKind,
    oracle: &dyn EquivalenceOracle,
) -> bool {
    EXT_WIDTH_SAMPLES.iter().all(|&(n, w)| {
        n < w
            && oracle.equivalent(
                &ext_of_low_bits(ext_kind, n, w),
                &shift_pair(shr_kind, w - n, w),
                &[w],
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extension_is_a_left_then_arithmetic_right_shift() {
        assert!(confirm_extension_via_shifts(
            ExprKind::SExt,
            ExprKind::ShiftRightArithmetic,
            &FuzzOracle::default(),
        ));
    }

    #[test]
    fn zero_extension_is_a_left_then_logical_right_shift() {
        assert!(confirm_extension_via_shifts(
            ExprKind::ZExt,
            ExprKind::ShiftRightLogic,
            &FuzzOracle::default(),
        ));
    }

    #[test]
    fn sign_extension_is_not_a_logical_right_shift() {
        // The oracle must reject the wrong pairing (srl can't sign-extend).
        assert!(!confirm_extension_via_shifts(
            ExprKind::SExt,
            ExprKind::ShiftRightLogic,
            &FuzzOracle::default(),
        ));
    }
}
