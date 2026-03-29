use crate::{
    graph::{Dag, NodeId},
    sem_expr2::{ExprKind, ExprPayload, Value},
    utils::{APFloat, APInt},
};

/// Evaluate the expression DAG given concrete values for each symbol.
///
/// `symbols[i]` is the value for the operand with `SymbolId(i)`.
/// Returns the value of the root node.
pub fn execute(
    graph: &impl Dag<Node = ExprKind, Leaf = ExprPayload>,
    symbols: &[Value],
) -> Value {
    let root = graph.root().expect("cannot execute empty graph");
    let mut cache = vec![None::<Value>; graph.len()];
    eval_node(graph, root, symbols, &mut cache)
}

fn child_val(
    graph: &impl Dag<Node = ExprKind, Leaf = ExprPayload>,
    node: NodeId,
    idx: usize,
    cache: &[Option<Value>],
) -> Value {
    let child = graph
        .children(node)
        .nth(idx)
        .expect("child index must be in bounds");
    cache[child.index()]
        .as_ref()
        .expect("child must be evaluated before parent in post-order")
        .clone()
}

macro_rules! as_int {
    ($v:expr, $op:literal) => {
        match $v {
            Value::Int(i) => i,
            Value::Float(_) => panic!("{} requires integer operands", $op),
        }
    };
}

macro_rules! as_float {
    ($v:expr, $op:literal) => {
        match $v {
            Value::Float(f) => f,
            Value::Int(_) => panic!("{} requires float operands", $op),
        }
    };
}

fn eval_node(
    graph: &impl Dag<Node = ExprKind, Leaf = ExprPayload>,
    node: NodeId,
    symbols: &[Value],
    cache: &mut Vec<Option<Value>>,
) -> Value {
    if let Some(ref v) = cache[node.index()] {
        return v.clone();
    }

    for child_id in graph.children(node) {
        if cache[child_id.index()].is_none() {
            let v = eval_node(graph, child_id, symbols, cache);
            cache[child_id.index()] = Some(v);
        }
    }

    let c = |idx: usize| child_val(graph, node, idx, cache);

    let result = match graph.get_kind(node) {
        ExprKind::Symbol => {
            let ExprPayload::SymbolId(id) = graph.get_leaf_data(node).unwrap() else {
                panic!("Symbol node must have SymbolId payload");
            };
            symbols[*id as usize].clone()
        }
        ExprKind::Constant => match graph.get_leaf_data(node).unwrap() {
            ExprPayload::Int(v) => Value::Int(v.clone()),
            ExprPayload::Float(v) => Value::Float(v.clone()),
            _ => panic!("Constant node must have Int or Float payload"),
        },

        // ── Arithmetic (int or float) ──────────────────────────────────────
        ExprKind::Add => match c(0) {
            Value::Int(a) => Value::Int(a.add(&as_int!(c(1), "add"))),
            Value::Float(a) => Value::Float(a.add(&as_float!(c(1), "add"))),
        },
        ExprKind::Sub => match c(0) {
            Value::Int(a) => Value::Int(a.sub(&as_int!(c(1), "sub"))),
            Value::Float(a) => Value::Float(a.sub(&as_float!(c(1), "sub"))),
        },
        ExprKind::Mul => match c(0) {
            Value::Int(a) => Value::Int(a.mul(&as_int!(c(1), "mul"))),
            Value::Float(a) => Value::Float(a.mul(&as_float!(c(1), "mul"))),
        },
        ExprKind::Div => match c(0) {
            Value::Int(a) => Value::Int(a.sdiv(&as_int!(c(1), "div"))),
            Value::Float(a) => Value::Float(a.div(&as_float!(c(1), "div"))),
        },
        ExprKind::UDiv => Value::Int(as_int!(c(0), "udiv").udiv(&as_int!(c(1), "udiv"))),

        // ── Bitwise (int only) ─────────────────────────────────────────────
        ExprKind::And => Value::Int(as_int!(c(0), "and").and(&as_int!(c(1), "and"))),
        ExprKind::Or => Value::Int(as_int!(c(0), "or").or(&as_int!(c(1), "or"))),
        ExprKind::Xor => Value::Int(as_int!(c(0), "xor").xor(&as_int!(c(1), "xor"))),
        ExprKind::ShiftLeft => {
            Value::Int(as_int!(c(0), "shl").shl(as_int!(c(1), "shl").to_u64() as u32))
        }
        ExprKind::ShiftRightLogic => {
            Value::Int(as_int!(c(0), "lshr").lshr(as_int!(c(1), "lshr").to_u64() as u32))
        }
        ExprKind::ShiftRightArithmetic => {
            Value::Int(as_int!(c(0), "ashr").ashr(as_int!(c(1), "ashr").to_u64() as u32))
        }

        // ── Comparisons ────────────────────────────────────────────────────
        ExprKind::Eq => Value::Int(APInt::new(1, bool_result(c(0) == c(1)))),
        ExprKind::Ne => Value::Int(APInt::new(1, bool_result(c(0) != c(1)))),
        ExprKind::Lt => Value::Int(APInt::new(1, match c(0) {
            Value::Int(a) => bool_result(a.slt(&as_int!(c(1), "lt"))),
            Value::Float(a) => bool_result(a.lt(&as_float!(c(1), "lt"))),
        })),
        ExprKind::Gt => Value::Int(APInt::new(1, match c(0) {
            Value::Int(a) => bool_result(a.sgt(&as_int!(c(1), "gt"))),
            Value::Float(a) => bool_result(a.gt(&as_float!(c(1), "gt"))),
        })),
        ExprKind::Ge => Value::Int(APInt::new(1, match c(0) {
            Value::Int(a) => bool_result(a.sge(&as_int!(c(1), "ge"))),
            Value::Float(a) => bool_result(a.ge(&as_float!(c(1), "ge"))),
        })),
        ExprKind::ULt => Value::Int(APInt::new(1, bool_result(as_int!(c(0), "ult").ult(&as_int!(c(1), "ult"))))),
        ExprKind::ULe => Value::Int(APInt::new(1, bool_result(as_int!(c(0), "ule").ule(&as_int!(c(1), "ule"))))),
        ExprKind::UGt => Value::Int(APInt::new(1, bool_result(as_int!(c(0), "ugt").ugt(&as_int!(c(1), "ugt"))))),
        ExprKind::UGe => Value::Int(APInt::new(1, bool_result(as_int!(c(0), "uge").uge(&as_int!(c(1), "uge"))))),

        // ── Control ────────────────────────────────────────────────────────
        ExprKind::If => {
            let cond_zero = match c(0) {
                Value::Int(i) => i.is_zero(),
                Value::Float(f) => f.is_zero(),
            };
            if cond_zero { c(2) } else { c(1) }
        }
        ExprKind::Clamp => {
            let input = as_int!(c(0), "clamp");
            let min = as_int!(c(1), "clamp");
            let max = as_int!(c(2), "clamp");

            let result = if input.is_signed() {
                if input.slt(&min) {
                    min
                } else if input.sgt(&max) {
                    max
                } else {
                    input
                }
            } else if input.ult(&min) {
                min
            } else if input.ugt(&max) {
                max
            } else {
                input
            };

            Value::Int(result)
        }

        // ── Math (int or float) ────────────────────────────────────────────
        ExprKind::Fma => match c(0) {
            Value::Int(a) => Value::Int(a.mul(&as_int!(c(1), "fma")).add(&as_int!(c(2), "fma"))),
            Value::Float(a) => Value::Float(a.fma(&as_float!(c(1), "fma"), &as_float!(c(2), "fma"))),
        },
        ExprKind::Sqrt => match c(0) {
            Value::Int(a) => {
                let v = a.to_u64();
                Value::Int(APInt::new(a.width(), (v as f64).sqrt() as u64))
            }
            Value::Float(a) => Value::Float(a.sqrt()),
        },
        ExprKind::Log2Ceil => {
            let a = as_int!(c(0), "log2ceil");
            let v = a.to_u64();
            let result = if v <= 1 { 0u64 } else { 64 - (v - 1).leading_zeros() as u64 };
            Value::Int(APInt::new(a.width(), result))
        }

        // ── Not yet supported ──────────────────────────────────────────────
        ExprKind::ZExt => todo!("ZExt requires a target-width payload"),
        ExprKind::SExt => todo!("SExt requires a target-width payload"),
        ExprKind::LoadMemory | ExprKind::StoreMemory => {
            unimplemented!("memory operations are not supported by this interpreter")
        }
    };

    cache[node.index()] = Some(result.clone());
    result
}

fn bool_result(b: bool) -> u64 {
    b as u64
}

// PartialEq for Value so comparisons work
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => APFloat::eq(a, b),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graph::{Dag, MutDag},
        sem_expr2::{ExprKind, ExprPayload, ExprPostGraph},
    };

    fn sym(g: &mut ExprPostGraph, id: u32) -> NodeId {
        let node = g.add_node(ExprKind::Symbol);
        g.set_leaf_data(node, ExprPayload::SymbolId(id));
        node
    }
    fn int_con(g: &mut ExprPostGraph, v: i64) -> NodeId {
        let node = g.add_node(ExprKind::Constant);
        g.set_leaf_data(node, ExprPayload::Int(APInt::new_signed(64, v)));
        node
    }
    fn flt_con(g: &mut ExprPostGraph, v: f64) -> NodeId {
        let node = g.add_node(ExprKind::Constant);
        g.set_leaf_data(node, ExprPayload::Float(APFloat::from_f64(v)));
        node
    }

    fn inner(g: &mut ExprPostGraph, kind: ExprKind, children: &[NodeId]) -> NodeId {
        let node = g.add_node(kind);
        for &child in children {
            g.add_edge(node, child);
        }
        node
    }

    fn iv(v: i64) -> Value { Value::Int(APInt::new_signed(32, v)) }
    fn fv(v: f64) -> Value { Value::Float(APFloat::from_f64(v)) }
    fn uv(v: u64) -> Value { Value::Int(APInt::new(32, v)) }

    fn as_i64(v: Value) -> i64 { match v { Value::Int(i) => i.to_i64(), Value::Float(_) => panic!() } }
    fn as_f64(v: Value) -> f64 { match v { Value::Float(f) => f.to_f64(), Value::Int(_) => panic!() } }

    // ── Integer arithmetic ─────────────────────────────────────────────────

    #[test]
    fn int_add() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Add, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(3), iv(4)])), 7);
    }

    #[test]
    fn int_sub() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Sub, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(10), iv(3)])), 7);
    }

    #[test]
    fn int_mul() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Mul, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(6), iv(7)])), 42);
    }

    #[test]
    fn int_and() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::And, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(0b1100), uv(0b1010)])), 0b1000);
    }

    #[test]
    fn int_shl() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::ShiftLeft, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(1), uv(3)])), 8);
    }

    #[test]
    fn int_lshr() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::ShiftRightLogic, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(16), uv(2)])), 4);
    }

    #[test]
    fn int_ashr_negative() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::ShiftRightArithmetic, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(-8), iv(1)])), -4);
    }

    #[test]
    fn int_constant() {
        let mut g = ExprPostGraph::new();
        int_con(&mut g, 42);
        assert_eq!(as_i64(execute(&g, &[])), 42);
    }

    #[test]
    fn int_shared_node() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, ExprKind::Add, &[a, a]);
        assert_eq!(as_i64(execute(&g, &[iv(5)])), 10);
    }

    #[test]
    fn int_fma() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1); let c = sym(&mut g, 2);
        inner(&mut g, ExprKind::Fma, &[a, b, c]);
        assert_eq!(as_i64(execute(&g, &[iv(3), iv(4), iv(5)])), 17);
    }

    // ── Comparisons ────────────────────────────────────────────────────────

    #[test]
    fn int_eq_true() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Eq, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(5), iv(5)])), 1);
    }

    #[test]
    fn int_eq_false() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Eq, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(5), iv(6)])), 0);
    }

    #[test]
    fn int_if_taken() {
        let mut g = ExprPostGraph::new();
        let cond = sym(&mut g, 0); let t = sym(&mut g, 1); let e = sym(&mut g, 2);
        inner(&mut g, ExprKind::If, &[cond, t, e]);
        assert_eq!(as_i64(execute(&g, &[iv(1), iv(42), iv(0)])), 42);
    }

    #[test]
    fn int_if_not_taken() {
        let mut g = ExprPostGraph::new();
        let cond = sym(&mut g, 0); let t = sym(&mut g, 1); let e = sym(&mut g, 2);
        inner(&mut g, ExprKind::If, &[cond, t, e]);
        assert_eq!(as_i64(execute(&g, &[iv(0), iv(42), iv(99)])), 99);
    }

    // ── Float arithmetic ───────────────────────────────────────────────────

    #[test]
    fn float_add() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Add, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(1.5), fv(2.5)]) ) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn float_sub() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Sub, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(5.0), fv(3.0)])) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn float_mul() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Mul, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(2.0), fv(3.5)])) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn float_div() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Div, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(7.0), fv(2.0)])) - 3.5).abs() < 1e-9);
    }

    #[test]
    fn float_sqrt() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, ExprKind::Sqrt, &[a]);
        assert!((as_f64(execute(&g, &[fv(9.0)])) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn float_fma() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1); let c = sym(&mut g, 2);
        inner(&mut g, ExprKind::Fma, &[a, b, c]);
        // 2.0 * 3.0 + 1.0 = 7.0
        assert!((as_f64(execute(&g, &[fv(2.0), fv(3.0), fv(1.0)])) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn int_clamp() {
        let mut g = ExprPostGraph::new();
        let input = sym(&mut g, 0);
        let min = int_con(&mut g, 3);
        let max = int_con(&mut g, 10);
        inner(&mut g, ExprKind::Clamp, &[input, min, max]);
        assert_eq!(as_i64(execute(&g, &[iv(20)])), 10);
    }

    #[test]
    fn float_constant() {
        let mut g = ExprPostGraph::new();
        flt_con(&mut g, 3.14);
        assert!((as_f64(execute(&g, &[])) - 3.14).abs() < 1e-9);
    }

    #[test]
    fn float_lt_true() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Lt, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[fv(1.0), fv(2.0)])), 1);
    }

    #[test]
    fn float_lt_false() {
        let mut g = ExprPostGraph::new();
        let a = sym(&mut g, 0); let b = sym(&mut g, 1);
        inner(&mut g, ExprKind::Lt, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[fv(3.0), fv(2.0)])), 0);
    }
}
