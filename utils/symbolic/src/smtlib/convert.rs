//! Conversion between the SMT-LIB AST and the evaluatable [`crate::lang`] graph.
//!
//! Scope is the Core (Bool) and FixedSizeBitVectors theories: bit-vectors map to
//! [`tir_adt::APInt`]-backed `Constant`/`Symbol` nodes, booleans are treated as
//! 1-bit values. Quantifiers, `match`, uninterpreted functions and any other
//! theory are rejected — they have no single-constant evaluation.

mod lift;
mod lower;

pub use lift::{lift_script, lift_term};
pub use lower::{Lowered, lower_script};

use std::fmt::{self, Display, Formatter};

/// A free variable in the lowered graph, indexed by its `SymbolId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    pub width: Option<u32>,
    /// True when the symbol's sort was `Bool` rather than a bit-vector, so the
    /// reverse direction can re-emit `Bool` and treat it as a boolean operand.
    pub is_bool: bool,
}

/// Why a term or graph could not be converted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvertError {
    /// `forall`/`exists` cannot be reduced to a constant by substitution.
    Quantifier,
    /// A free symbol with no declaration in scope.
    UnknownSymbol(String),
    /// An operator, sort or construct outside the Core + BitVec subset.
    Unsupported(String),
    /// An operator applied to the wrong number of arguments.
    BadArity {
        op: String,
        expected: String,
        got: usize,
    },
    /// A literal that does not fit the backing representation.
    BadLiteral(String),
    /// An operation needed an operand width that could not be determined.
    UnknownWidth(String),
}

impl Display for ConvertError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::Quantifier => {
                f.write_str("quantifiers are not evaluatable and cannot be lowered")
            }
            ConvertError::UnknownSymbol(s) => write!(f, "unknown symbol `{s}`"),
            ConvertError::Unsupported(s) => write!(f, "unsupported construct: {s}"),
            ConvertError::BadArity { op, expected, got } => {
                write!(f, "`{op}` expects {expected} arguments, got {got}")
            }
            ConvertError::BadLiteral(s) => write!(f, "invalid literal: {s}"),
            ConvertError::UnknownWidth(s) => write!(f, "could not determine width for {s}"),
        }
    }
}

impl std::error::Error for ConvertError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{SymKind, SymPayload};
    use crate::smtlib::parser::parse_script;
    use tir_graph::{Dag, GenericDag, NodeId};

    type Graph = GenericDag<SymKind, SymPayload<()>>;

    fn lower(src: &str) -> Lowered<()> {
        lower_script::<()>(&parse_script(src).unwrap()).unwrap()
    }

    /// Structural isomorphism, ignoring node ordering, sharing and `SymbolId`
    /// numbering (symbols compared by name).
    fn iso(
        g1: &Graph,
        n1: NodeId,
        s1: &[SymbolInfo],
        g2: &Graph,
        n2: NodeId,
        s2: &[SymbolInfo],
    ) -> bool {
        let k1 = *g1.get_kind(n1);
        if k1 != *g2.get_kind(n2) {
            return false;
        }
        match k1 {
            SymKind::Symbol => {
                let name = |g: &Graph, n: NodeId, s: &[SymbolInfo]| match g.get_leaf_data(n) {
                    Some(SymPayload::SymbolId(id)) => s[*id as usize].name.clone(),
                    _ => unreachable!(),
                };
                name(g1, n1, s1) == name(g2, n2, s2)
            }
            SymKind::Constant => g1.get_leaf_data(n1) == g2.get_leaf_data(n2),
            _ => {
                let c1: Vec<_> = g1.children(n1).collect();
                let c2: Vec<_> = g2.children(n2).collect();
                c1.len() == c2.len() && c1.iter().zip(&c2).all(|(&a, &b)| iso(g1, a, s1, g2, b, s2))
            }
        }
    }

    /// SMT -> graph -> SMT -> graph must produce isomorphic graphs.
    fn roundtrips(src: &str) {
        let a = lower(src);
        let script = lift_script(&a.graph, a.root, &a.symbols).unwrap();
        let b = lower_script::<()>(&parse_script(&script.to_string()).unwrap()).unwrap();
        assert!(
            iso(&a.graph, a.root, &a.symbols, &b.graph, b.root, &b.symbols),
            "round-trip diverged for `{src}`\nlifted to:\n{script}"
        );
    }

    #[test]
    fn lowers_structure_and_sharing() {
        let lo = lower(
            "(declare-const x (_ BitVec 8))\
             (declare-const y (_ BitVec 8))\
             (assert (= (bvadd x y) x))",
        );
        let g = &lo.graph;
        assert_eq!(*g.get_kind(lo.root), SymKind::Eq);
        let rc: Vec<_> = g.children(lo.root).collect();
        assert_eq!(*g.get_kind(rc[0]), SymKind::Add);
        assert_eq!(*g.get_kind(rc[1]), SymKind::Symbol);
        let add: Vec<_> = g.children(rc[0]).collect();
        // Both occurrences of `x` share one node.
        assert_eq!(add[0], rc[1]);
        assert_eq!(lo.widths[lo.root.index()], Some(1));
        assert_eq!(lo.widths[rc[0].index()], Some(8));
    }

    #[test]
    fn lowers_extract_and_literal() {
        let lo = lower(
            "(declare-const x (_ BitVec 8))\
             (assert (= ((_ extract 3 0) x) #x5))",
        );
        let g = &lo.graph;
        let rc: Vec<_> = g.children(lo.root).collect();
        assert_eq!(*g.get_kind(rc[0]), SymKind::Extract);
        assert_eq!(lo.widths[rc[0].index()], Some(4));
        // `#x5` is a 4-bit constant.
        match g.get_leaf_data(rc[1]) {
            Some(SymPayload::Int(v)) => {
                assert_eq!(v.width(), 4);
                assert_eq!(v.to_u64(), 5);
            }
            other => panic!("expected constant, got {other:?}"),
        }
    }

    #[test]
    fn empty_assertions_lower_to_true() {
        let lo = lower("(declare-const x (_ BitVec 8))");
        assert_eq!(*lo.graph.get_kind(lo.root), SymKind::Constant);
    }

    #[test]
    fn inlines_define_fun() {
        let lo = lower(
            "(declare-const x (_ BitVec 8))\
             (define-fun dbl ((a (_ BitVec 8))) (_ BitVec 8) (bvadd a a))\
             (assert (= (dbl x) x))",
        );
        let g = &lo.graph;
        let rc: Vec<_> = g.children(lo.root).collect();
        assert_eq!(*g.get_kind(rc[0]), SymKind::Add);
        let add: Vec<_> = g.children(rc[0]).collect();
        assert_eq!(add[0], add[1]); // both `a` bind to the same `x` node
    }

    #[test]
    fn roundtrips_bitvec_terms() {
        roundtrips(
            "(declare-const x (_ BitVec 8))\
             (declare-const y (_ BitVec 8))\
             (assert (= (bvadd x y) x))",
        );
        roundtrips(
            "(declare-const x (_ BitVec 16))\
             (assert (bvule ((_ extract 7 0) x) #x0f))",
        );
        roundtrips(
            "(declare-const x (_ BitVec 8))\
             (assert (= ((_ zero_extend 4) x) ((_ sign_extend 4) x)))",
        );
        roundtrips(
            "(declare-const a (_ BitVec 4))\
             (declare-const b (_ BitVec 4))\
             (assert (= (concat a b) (concat b a)))",
        );
        // `let` sharing collapses to identical subtrees on re-lowering.
        roundtrips(
            "(declare-const x (_ BitVec 8))\
             (declare-const y (_ BitVec 8))\
             (assert (let ((z (bvand x y))) (= z z)))",
        );
        // Boolean structure: and/or/not over comparisons stays boolean.
        roundtrips(
            "(declare-const x (_ BitVec 8))\
             (assert (and (bvult x #x0a) (not (= x #x00))))",
        );
        // A boolean constant in boolean position survives as true/false.
        roundtrips(
            "(declare-const x (_ BitVec 8))\
             (assert (and true (= x #x00)))",
        );
        // Bool-sorted symbols re-declare as Bool and stay boolean operands.
        roundtrips(
            "(declare-const b Bool)\
             (declare-const x (_ BitVec 8))\
             (assert (and b (bvult x #x05)))",
        );
    }

    #[test]
    fn rejects_oversized_or_zero_widths_without_panicking() {
        // 17 hex digits = 68 bits, > the 64-bit APInt backing.
        let cases = [
            "(assert (= #x00000000000000000 #x00000000000000000))",
            "(declare-const x (_ BitVec 100)) (assert (= x x))",
            "(assert (= (_ bv0 0) (_ bv0 0)))",
        ];
        for src in cases {
            let script = parse_script(src).unwrap();
            assert!(
                lower_script::<()>(&script).is_err(),
                "expected error (not panic) for `{src}`"
            );
        }
    }

    #[test]
    fn rejects_quantifiers() {
        let script = parse_script(
            "(declare-const x (_ BitVec 8))\
             (assert (forall ((y (_ BitVec 8))) (= x y)))",
        )
        .unwrap();
        assert!(matches!(
            lower_script::<()>(&script),
            Err(ConvertError::Quantifier)
        ));
    }

    #[test]
    fn rejects_unknown_symbol() {
        let script = parse_script("(assert (= x x))").unwrap();
        match lower_script::<()>(&script) {
            Err(ConvertError::UnknownSymbol(s)) => assert_eq!(s, "x"),
            _ => panic!("expected unknown-symbol error"),
        }
    }

    #[test]
    fn rejects_bvsmod() {
        let script = parse_script(
            "(declare-const x (_ BitVec 8))\
             (declare-const y (_ BitVec 8))\
             (assert (= (bvsmod x y) x))",
        )
        .unwrap();
        assert!(matches!(
            lower_script::<()>(&script),
            Err(ConvertError::Unsupported(_))
        ));
    }
}
