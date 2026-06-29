use tir_adt::{APInt, RawBits};
use tir_graph::{Dag, NodeId};

use crate::lang::{SymKind, SymPayload, Value};

/// Memory backend used by semantic expressions containing `LoadMemory` or
/// `StoreMemory`.
pub trait Memory {
    type Error;

    fn read_memory(&mut self, address: u64, size: usize) -> Result<u64, Self::Error>;
    fn write_memory(&mut self, address: u64, size: usize, value: u64) -> Result<(), Self::Error>;
}

enum NoMemoryError {}

struct NoMemory;

impl Memory for NoMemory {
    type Error = NoMemoryError;

    fn read_memory(&mut self, _address: u64, _size: usize) -> Result<u64, Self::Error> {
        unimplemented!("memory operations are not supported by this interpreter")
    }

    fn write_memory(
        &mut self,
        _address: u64,
        _size: usize,
        _value: u64,
    ) -> Result<(), Self::Error> {
        unimplemented!("memory operations are not supported by this interpreter")
    }
}

/// Evaluate the expression DAG given concrete values for each symbol.
///
/// `symbols[i]` is the value for the operand with `SymbolId(i)`.
/// Returns the value of the root node.
pub fn execute<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    symbols: &[Value],
) -> Value {
    match execute_with_memory(graph, symbols, &mut NoMemory) {
        Ok(value) => value,
        Err(err) => match err {},
    }
}

/// Evaluate the expression DAG with a memory backend for load/store nodes.
///
/// Loads read little-endian byte sequences and produce an integer whose width is
/// `size * 8`. Stores write the low bytes of their value and return a dummy
/// 1-bit integer; callers normally ignore the result for store statements.
pub fn execute_with_memory<V, M: Memory>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    symbols: &[Value],
    memory: &mut M,
) -> Result<Value, M::Error> {
    let root = graph.root().expect("cannot execute empty graph");
    let mut cache = vec![None::<Value>; graph.len()];
    let mut args: Vec<Value> = Vec::new();
    eval_node(graph, root, symbols, &mut cache, &mut args, memory)
}

fn child_val<V>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
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
            Value::Iterator(_) => panic!("{} requires scalar operands", $op),
            Value::RawBits(_) => panic!("{} requires integer operands", $op),
        }
    };
}

macro_rules! as_float {
    ($v:expr, $op:literal) => {
        match $v {
            Value::Float(f) => f,
            Value::Int(_) => panic!("{} requires float operands", $op),
            Value::Iterator(_) => panic!("{} requires scalar operands", $op),
            Value::RawBits(_) => panic!("{} requires float operands", $op),
        }
    };
}

/// Widen `v` to `width`, sign-extending signed values and zero-extending unsigned
/// ones; a no-op when it is already at least that wide.
fn widen(v: APInt, width: u32) -> APInt {
    if v.width() >= width {
        v
    } else if v.is_signed() {
        v.sign_extend(width)
    } else {
        v.zero_extend(width)
    }
}

/// Bring two integers to a common width before a binary operation. Behavior
/// expressions freely mix a wide value (a register, `XLEN`) with a bare narrow
/// literal (`- 1`, `<< 2`, a `zext`-ed constant), so the interpreter extends the
/// narrower operand rather than requiring exactly matching widths. Equal-width
/// operands — the common case — pass through unchanged.
fn coerce_ints(a: APInt, b: APInt) -> (APInt, APInt) {
    let width = a.width().max(b.width());
    (widen(a, width), widen(b, width))
}

/// Equality of two integers independent of width and signedness: operands are
/// widened to a common width and compared by value, so e.g. a 64-bit register
/// equals a narrow literal of the same magnitude.
fn ints_equal(a: APInt, b: APInt) -> bool {
    let (a, b) = coerce_ints(a, b);
    a.with_signed(false) == b.with_signed(false)
}

/// Evaluate a `Map` node: apply `body` to each lane of `iter`, exposing the lane
/// (or its components, for a zipped pair) through the lambda-argument stack.
fn eval_map<V, M: Memory>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
    symbols: &[Value],
    cache: &mut Vec<Option<Value>>,
    args: &mut Vec<Value>,
    memory: &mut M,
) -> Result<Value, M::Error> {
    let children: Vec<NodeId> = graph.children(node).collect();
    let (iter_n, body_n) = (children[0], children[1]);

    let iter = eval_node(graph, iter_n, symbols, cache, args, memory)?;
    let Value::Iterator(elems) = iter else {
        panic!("map requires an iterator operand");
    };

    let mut out = Vec::with_capacity(elems.len());
    for elem in elems {
        args.push(elem);
        // `body` reads the per-lane argument, which changes each lane, so it
        // cannot share the surrounding cache: evaluate it fresh.
        let mut body_cache = vec![None::<Value>; graph.len()];
        let lane = eval_node(graph, body_n, symbols, &mut body_cache, args, memory);
        args.pop();
        out.push(lane?);
    }
    Ok(Value::Iterator(out))
}

/// Evaluate a `Reduce` node: left-fold `body` over the lanes of `iter`, binding
/// `Arg(0)` to the accumulator and `Arg(1)` to the current lane.
fn eval_reduce<V, M: Memory>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
    symbols: &[Value],
    cache: &mut Vec<Option<Value>>,
    args: &mut Vec<Value>,
    memory: &mut M,
) -> Result<Value, M::Error> {
    let children: Vec<NodeId> = graph.children(node).collect();
    let (iter_n, body_n) = (children[0], children[1]);

    let iter = eval_node(graph, iter_n, symbols, cache, args, memory)?;
    let Value::Iterator(elems) = iter else {
        panic!("reduce requires an iterator operand");
    };
    let mut elems = elems.into_iter();
    let mut acc = elems.next().expect("reduce requires a non-empty iterator");
    for elem in elems {
        // The accumulator and lane are read via `Arg(0)`/`Arg(1)`, packed as a
        // two-element binding on the argument stack.
        args.push(Value::Iterator(vec![acc, elem]));
        let mut body_cache = vec![None::<Value>; graph.len()];
        let next = eval_node(graph, body_n, symbols, &mut body_cache, args, memory);
        args.pop();
        acc = next?;
    }
    Ok(acc)
}

/// Evaluate a `Split` node: cut a raw-bits value into `n` equal-width lanes, lane
/// 0 from the low bits. Each lane is reinterpreted as an integer — the only
/// element kind the behavior language currently produces; float lanes would read
/// the raw lanes with [`RawBits::to_apfloat`] instead.
fn split_bits(value: Value, n: usize) -> Value {
    let Value::RawBits(bits) = value else {
        panic!("split requires a raw-bits operand");
    };
    let lanes = bits
        .split(n)
        .into_iter()
        .map(|lane| Value::Int(lane.to_apint()))
        .collect();
    Value::Iterator(lanes)
}

/// Evaluate an `IterConcat` node: join an iterator's lanes into one raw-bits
/// value, lane 0 in the low bits. The inverse of `Split`; each lane is taken back
/// to its raw bytes according to its runtime type.
fn concat_lanes(value: Value) -> Value {
    let Value::Iterator(lanes) = value else {
        panic!("concat requires an iterator operand");
    };
    let raw: Vec<RawBits> = lanes
        .into_iter()
        .map(|lane| match lane {
            Value::Int(i) => RawBits::from_apint(&i),
            Value::Float(f) => RawBits::from_apfloat(&f),
            Value::RawBits(b) => b,
            Value::Iterator(_) => panic!("concat lanes must be scalar"),
        })
        .collect();
    Value::RawBits(RawBits::concat(&raw))
}

fn eval_node<V, M: Memory>(
    graph: &impl Dag<Node = SymKind, Leaf = SymPayload<V>>,
    node: NodeId,
    symbols: &[Value],
    cache: &mut Vec<Option<Value>>,
    args: &mut Vec<Value>,
    memory: &mut M,
) -> Result<Value, M::Error> {
    if let Some(ref v) = cache[node.index()] {
        return Ok(v.clone());
    }

    // `Map` and `Reduce` re-evaluate a child once per lane with a fresh per-lane
    // lambda argument (read via `Arg`), so that child must not be pre-evaluated by
    // the generic pass. Intercept each before the generic child pre-evaluation.
    match *graph.get_kind(node) {
        SymKind::Map => {
            let result = eval_map(graph, node, symbols, cache, args, memory)?;
            cache[node.index()] = Some(result.clone());
            return Ok(result);
        }
        SymKind::Reduce => {
            let result = eval_reduce(graph, node, symbols, cache, args, memory)?;
            cache[node.index()] = Some(result.clone());
            return Ok(result);
        }
        _ => {}
    }

    for child_id in graph.children(node) {
        if cache[child_id.index()].is_none() {
            let v = eval_node(graph, child_id, symbols, cache, args, memory)?;
            cache[child_id.index()] = Some(v);
        }
    }

    let c = |idx: usize| child_val(graph, node, idx, cache);

    let result = match graph.get_kind(node) {
        SymKind::Map | SymKind::Reduce => {
            unreachable!("map/reduce handled before child pre-evaluation")
        }
        SymKind::Arg => {
            let SymPayload::Int(idx) = graph.get_leaf_data(node).unwrap() else {
                panic!("Arg node must have Int payload");
            };
            let idx = idx.to_u64() as usize;
            let binding = args.last().expect("Arg evaluated outside a lambda");
            match binding {
                // A pair binding (from `Zip` lanes or a `Reduce` acc/lane pack)
                // exposes its components positionally.
                Value::Iterator(parts) => parts[idx].clone(),
                // A scalar binding is the single argument of a unary lambda.
                scalar => {
                    assert!(idx == 0, "scalar lambda argument has only index 0");
                    scalar.clone()
                }
            }
        }
        SymKind::Zip => {
            let (Value::Iterator(lhs), Value::Iterator(rhs)) = (c(0), c(1)) else {
                panic!("zip requires iterator operands");
            };
            assert!(
                lhs.len() == rhs.len(),
                "zip requires equal-length iterators"
            );
            Value::Iterator(
                lhs.into_iter()
                    .zip(rhs)
                    .map(|(a, b)| Value::Iterator(vec![a, b]))
                    .collect(),
            )
        }
        SymKind::Split => split_bits(c(0), as_int!(c(1), "split").to_u64() as usize),
        SymKind::IterConcat => concat_lanes(c(0)),
        SymKind::Symbol => {
            let SymPayload::SymbolId(id) = graph.get_leaf_data(node).unwrap() else {
                panic!("Symbol node must have SymbolId payload");
            };
            symbols[*id as usize].clone()
        }
        SymKind::Constant => match graph.get_leaf_data(node).unwrap() {
            SymPayload::Int(v) => Value::Int(v.clone()),
            SymPayload::Float(v) => Value::Float(v.clone()),
            _ => panic!("Constant node must have Int or Float payload"),
        },

        // ── Arithmetic (int or float) ──────────────────────────────────────
        SymKind::Add => match c(0) {
            Value::Int(a) => {
                let (a, b) = coerce_ints(a, as_int!(c(1), "add"));
                Value::Int(a.add(&b))
            }
            Value::Float(a) => Value::Float(a.add(&as_float!(c(1), "add"))),
            Value::Iterator(_) | Value::RawBits(_) => panic!("add requires scalar operands"),
        },
        SymKind::Sub => match c(0) {
            Value::Int(a) => {
                let (a, b) = coerce_ints(a, as_int!(c(1), "sub"));
                Value::Int(a.sub(&b))
            }
            Value::Float(a) => Value::Float(a.sub(&as_float!(c(1), "sub"))),
            Value::Iterator(_) | Value::RawBits(_) => panic!("sub requires scalar operands"),
        },
        SymKind::Mul => match c(0) {
            Value::Int(a) => {
                let (a, b) = coerce_ints(a, as_int!(c(1), "mul"));
                Value::Int(a.mul(&b))
            }
            Value::Float(a) => Value::Float(a.mul(&as_float!(c(1), "mul"))),
            Value::Iterator(_) | Value::RawBits(_) => panic!("mul requires scalar operands"),
        },
        SymKind::Div => match c(0) {
            Value::Int(a) => {
                let (a, b) = coerce_ints(a, as_int!(c(1), "div"));
                Value::Int(a.sdiv(&b))
            }
            Value::Float(a) => Value::Float(a.div(&as_float!(c(1), "div"))),
            Value::Iterator(_) | Value::RawBits(_) => panic!("div requires scalar operands"),
        },
        SymKind::UDiv => {
            let (a, b) = coerce_ints(as_int!(c(0), "udiv"), as_int!(c(1), "udiv"));
            Value::Int(a.udiv(&b))
        }
        SymKind::SRem => {
            let (a, b) = coerce_ints(as_int!(c(0), "srem"), as_int!(c(1), "srem"));
            Value::Int(a.srem(&b))
        }
        SymKind::URem => {
            let (a, b) = coerce_ints(as_int!(c(0), "urem"), as_int!(c(1), "urem"));
            Value::Int(a.urem(&b))
        }
        SymKind::Neg => Value::Int(as_int!(c(0), "neg").neg()),

        // ── Bitwise (int only) ─────────────────────────────────────────────
        SymKind::And => {
            let (a, b) = coerce_ints(as_int!(c(0), "and"), as_int!(c(1), "and"));
            Value::Int(a.and(&b))
        }
        SymKind::Or => {
            let (a, b) = coerce_ints(as_int!(c(0), "or"), as_int!(c(1), "or"));
            Value::Int(a.or(&b))
        }
        SymKind::Xor => {
            let (a, b) = coerce_ints(as_int!(c(0), "xor"), as_int!(c(1), "xor"));
            Value::Int(a.xor(&b))
        }
        // Concatenation places the first operand in the high bits; the result is
        // as wide as the sum of the operand widths.
        SymKind::Concat => {
            let hi = as_int!(c(0), "concat");
            let lo = as_int!(c(1), "concat");
            let width = hi.width() + lo.width();
            let value = hi
                .zero_extend(width)
                .shl(lo.width())
                .or(&lo.zero_extend(width));
            Value::Int(value)
        }
        SymKind::ShiftLeft => {
            Value::Int(as_int!(c(0), "shl").shl(as_int!(c(1), "shl").to_u64() as u32))
        }
        SymKind::ShiftRightLogic => {
            Value::Int(as_int!(c(0), "lshr").lshr(as_int!(c(1), "lshr").to_u64() as u32))
        }
        SymKind::ShiftRightArithmetic => {
            // An arithmetic shift always treats its operand as signed (sign bit =
            // MSB of the operand width), regardless of the value's stored
            // signedness flag. Register values are stored unsigned, so without
            // forcing this `>>>` would silently degrade to a logical shift.
            let mut value = as_int!(c(0), "ashr");
            value.set_signed(true);
            Value::Int(value.ashr(as_int!(c(1), "ashr").to_u64() as u32))
        }
        SymKind::Not => Value::Int(as_int!(c(0), "not").not()),

        // ── Comparisons ────────────────────────────────────────────────────
        SymKind::Eq => {
            let eq = match (c(0), c(1)) {
                (Value::Int(a), Value::Int(b)) => ints_equal(a, b),
                (l, r) => l == r,
            };
            Value::Int(APInt::new(1, bool_result(eq)))
        }
        SymKind::Ne => {
            let ne = match (c(0), c(1)) {
                (Value::Int(a), Value::Int(b)) => !ints_equal(a, b),
                (l, r) => l != r,
            };
            Value::Int(APInt::new(1, bool_result(ne)))
        }
        SymKind::Lt => Value::Int(APInt::new(
            1,
            match c(0) {
                Value::Int(a) => {
                    let (a, b) = coerce_ints(a, as_int!(c(1), "lt"));
                    bool_result(a.slt(&b))
                }
                Value::Float(a) => bool_result(a.lt(&as_float!(c(1), "lt"))),
                Value::Iterator(_) | Value::RawBits(_) => panic!("lt requires scalar operands"),
            },
        )),
        SymKind::Le => Value::Int(APInt::new(
            1,
            match c(0) {
                Value::Int(a) => {
                    let (a, b) = coerce_ints(a, as_int!(c(1), "le"));
                    bool_result(a.sle(&b))
                }
                Value::Float(a) => bool_result(a.le(&as_float!(c(1), "le"))),
                Value::Iterator(_) | Value::RawBits(_) => panic!("le requires scalar operands"),
            },
        )),
        SymKind::Gt => Value::Int(APInt::new(
            1,
            match c(0) {
                Value::Int(a) => {
                    let (a, b) = coerce_ints(a, as_int!(c(1), "gt"));
                    bool_result(a.sgt(&b))
                }
                Value::Float(a) => bool_result(a.gt(&as_float!(c(1), "gt"))),
                Value::Iterator(_) | Value::RawBits(_) => panic!("gt requires scalar operands"),
            },
        )),
        SymKind::Ge => Value::Int(APInt::new(
            1,
            match c(0) {
                Value::Int(a) => {
                    let (a, b) = coerce_ints(a, as_int!(c(1), "ge"));
                    bool_result(a.sge(&b))
                }
                Value::Float(a) => bool_result(a.ge(&as_float!(c(1), "ge"))),
                Value::Iterator(_) | Value::RawBits(_) => panic!("ge requires scalar operands"),
            },
        )),
        SymKind::ULt => {
            let (a, b) = coerce_ints(as_int!(c(0), "ult"), as_int!(c(1), "ult"));
            Value::Int(APInt::new(1, bool_result(a.ult(&b))))
        }
        SymKind::ULe => {
            let (a, b) = coerce_ints(as_int!(c(0), "ule"), as_int!(c(1), "ule"));
            Value::Int(APInt::new(1, bool_result(a.ule(&b))))
        }
        SymKind::UGt => {
            let (a, b) = coerce_ints(as_int!(c(0), "ugt"), as_int!(c(1), "ugt"));
            Value::Int(APInt::new(1, bool_result(a.ugt(&b))))
        }
        SymKind::UGe => {
            let (a, b) = coerce_ints(as_int!(c(0), "uge"), as_int!(c(1), "uge"));
            Value::Int(APInt::new(1, bool_result(a.uge(&b))))
        }

        // ── Control ────────────────────────────────────────────────────────
        SymKind::If => {
            let cond_zero = match c(0) {
                Value::Int(i) => i.is_zero(),
                Value::Float(f) => f.is_zero(),
                Value::Iterator(_) | Value::RawBits(_) => panic!("if condition must be scalar"),
            };
            if cond_zero { c(2) } else { c(1) }
        }
        SymKind::Clamp => {
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
        SymKind::Fma => match c(0) {
            Value::Int(a) => {
                let (a, b) = coerce_ints(a, as_int!(c(1), "fma"));
                let (prod, addend) = coerce_ints(a.mul(&b), as_int!(c(2), "fma"));
                Value::Int(prod.add(&addend))
            }
            Value::Float(a) => {
                Value::Float(a.fma(&as_float!(c(1), "fma"), &as_float!(c(2), "fma")))
            }
            Value::Iterator(_) | Value::RawBits(_) => panic!("fma requires scalar operands"),
        },
        SymKind::Sqrt => match c(0) {
            Value::Int(a) => {
                let v = a.to_u64();
                Value::Int(APInt::new(a.width(), (v as f64).sqrt() as u64))
            }
            Value::Float(a) => Value::Float(a.sqrt()),
            Value::Iterator(_) | Value::RawBits(_) => panic!("sqrt requires a scalar operand"),
        },
        SymKind::Log2Ceil => {
            let a = as_int!(c(0), "log2ceil");
            let v = a.to_u64();
            let result = if v <= 1 {
                0u64
            } else {
                64 - (v - 1).leading_zeros() as u64
            };
            Value::Int(APInt::new(a.width(), result))
        }

        SymKind::Extract => {
            let value = as_int!(c(0), "extract");
            let high = as_int!(c(1), "extract").to_u64() as u32;
            let low = as_int!(c(2), "extract").to_u64() as u32;
            // `extract(a * b, 2N-1, N)` is the TMDL idiom for the high half of a
            // full multiply (e.g. RISC-V `mulh`). The `Mul` node itself only
            // keeps the low N bits, so when the slice lies entirely past the
            // product's width, recompute it from the multiply's operands as a
            // signed full-width product.
            let mul = graph.children(node).next().expect("extract has children");
            if low >= value.width() && matches!(graph.get_kind(mul), SymKind::Mul) {
                let (a, b) = coerce_ints(
                    as_int!(child_val(graph, mul, 0, cache), "extract"),
                    as_int!(child_val(graph, mul, 1, cache), "extract"),
                );
                let product_high = a.with_signed(true).mulh(&b.with_signed(true));
                Value::Int(product_high.extract_bits(high - a.width(), low - a.width()))
            } else {
                Value::Int(value.extract_bits(high, low))
            }
        }
        SymKind::ZExt => {
            let value = as_int!(c(0), "zext");
            let width = as_int!(c(1), "zext").to_u64() as u32;
            Value::Int(value.zero_extend(width))
        }
        SymKind::SExt => {
            let value = as_int!(c(0), "sext");
            let width = as_int!(c(1), "sext").to_u64() as u32;
            // Sign-extend from the value's current MSB regardless of how its
            // signedness flag happens to be set (e.g. `extract` yields unsigned).
            Value::Int(value.with_signed(true).sign_extend(width))
        }

        // ── Memory ─────────────────────────────────────────────────────────
        SymKind::LoadMemory => {
            let address = as_int!(c(0), "load").to_u64();
            let size = as_int!(c(1), "load").to_u64() as usize;
            let value = memory.read_memory(address, size)?;
            Value::Int(APInt::new((size as u32) * 8, value))
        }
        SymKind::StoreMemory => {
            let address = as_int!(c(0), "store").to_u64();
            let size = as_int!(c(1), "store").to_u64() as usize;
            let value = as_int!(c(2), "store").to_u64();
            memory.write_memory(address, size, value)?;
            Value::Int(APInt::new(1, 0))
        }
    };

    cache[node.index()] = Some(result.clone());
    Ok(result)
}

fn bool_result(b: bool) -> u64 {
    b as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tir_adt::APFloat;
    use tir_graph::{GenericDag, MutDag};

    type Graph = GenericDag<SymKind, SymPayload<()>>;

    fn sym(g: &mut Graph, id: u32) -> NodeId {
        let node = g.add_node(SymKind::Symbol);
        g.set_leaf_data(node, SymPayload::SymbolId(id));
        node
    }
    fn int_con(g: &mut Graph, v: i64) -> NodeId {
        let node = g.add_node(SymKind::Constant);
        g.set_leaf_data(node, SymPayload::Int(APInt::new_signed(64, v)));
        node
    }
    fn inner(g: &mut Graph, kind: SymKind, children: &[NodeId]) -> NodeId {
        let node = g.add_node(kind);
        for &child in children {
            g.add_edge(node, child);
        }
        node
    }
    fn arg(g: &mut Graph, k: u64) -> NodeId {
        let node = g.add_node(SymKind::Arg);
        g.set_leaf_data(node, SymPayload::Int(APInt::new(32, k)));
        node
    }

    fn iv(v: i64) -> Value {
        Value::Int(APInt::new_signed(32, v))
    }
    fn fv(v: f64) -> Value {
        Value::Float(APFloat::from_f64(v))
    }
    fn uv(v: u64) -> Value {
        Value::Int(APInt::new(32, v))
    }
    fn rb(bytes: &[u8]) -> Value {
        Value::RawBits(RawBits::from_bytes(bytes.to_vec()))
    }

    fn as_i64(v: Value) -> i64 {
        match v {
            Value::Int(i) => i.to_i64(),
            _ => panic!(),
        }
    }
    fn as_u64(v: Value) -> u64 {
        match v {
            Value::Int(i) => i.to_u64(),
            _ => panic!(),
        }
    }
    fn as_f64(v: Value) -> f64 {
        match v {
            Value::Float(f) => f.to_f64(),
            _ => panic!(),
        }
    }
    fn raw_bytes(v: Value) -> Vec<u8> {
        match v {
            Value::RawBits(b) => b.bytes().to_vec(),
            other => panic!("expected raw bits, got {other:?}"),
        }
    }
    fn int_lanes(v: Value) -> Vec<i64> {
        match v {
            Value::Iterator(xs) => xs.into_iter().map(as_i64).collect(),
            other => panic!("expected iterator, got {other:?}"),
        }
    }

    #[derive(Default)]
    struct TestMemory {
        bytes: Vec<u8>,
    }

    impl Memory for TestMemory {
        type Error = ();

        fn read_memory(&mut self, address: u64, size: usize) -> Result<u64, Self::Error> {
            let start = address as usize;
            let mut value = 0;
            for (offset, byte) in self.bytes[start..start + size].iter().enumerate() {
                value |= u64::from(*byte) << (offset * 8);
            }
            Ok(value)
        }

        fn write_memory(
            &mut self,
            address: u64,
            size: usize,
            value: u64,
        ) -> Result<(), Self::Error> {
            let start = address as usize;
            for offset in 0..size {
                self.bytes[start + offset] = ((value >> (offset * 8)) & 0xff) as u8;
            }
            Ok(())
        }
    }

    #[test]
    fn memory_load_and_store_execute_little_endian() {
        let mut g = Graph::new();
        let address = int_con(&mut g, 4);
        let bytes = int_con(&mut g, 4);
        let metadata = int_con(&mut g, 0);
        inner(&mut g, SymKind::LoadMemory, &[address, bytes, metadata]);

        let mut memory = TestMemory { bytes: vec![0; 16] };
        memory.bytes[4..8].copy_from_slice(&[0x78, 0x56, 0x34, 0x12]);
        let loaded = execute_with_memory(&g, &[], &mut memory).unwrap();
        assert_eq!(as_i64(loaded), 0x1234_5678);

        let mut g = Graph::new();
        let address = int_con(&mut g, 8);
        let bytes = int_con(&mut g, 2);
        let value = int_con(&mut g, 0xbeef);
        let address_space = int_con(&mut g, 0);
        inner(
            &mut g,
            SymKind::StoreMemory,
            &[address, bytes, value, address_space],
        );
        execute_with_memory(&g, &[], &mut memory).unwrap();
        assert_eq!(&memory.bytes[8..10], &[0xef, 0xbe]);
    }

    // ── Integer arithmetic ─────────────────────────────────────────────────

    #[test]
    fn int_add() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Add, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(3), iv(4)])), 7);
    }

    #[test]
    fn int_sub() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Sub, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(10), iv(3)])), 7);
    }

    #[test]
    fn int_mul() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Mul, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(6), iv(7)])), 42);
    }

    #[test]
    fn int_neg() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, SymKind::Neg, &[a]);
        assert_eq!(as_i64(execute(&g, &[iv(5)])), -5);
        assert_eq!(as_i64(execute(&g, &[iv(-3)])), 3);
    }

    #[test]
    fn int_srem_and_urem() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::SRem, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(-7), iv(3)])), -1);

        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::URem, &[a, b]);
        assert_eq!(as_u64(execute(&g, &[uv(7), uv(3)])), 1);
    }

    #[test]
    fn int_concat_places_first_operand_high() {
        // concat(0xAB @ 8, 0xCD @ 8) -> 0xABCD @ 16.
        let mut g = Graph::new();
        let hi = {
            let n = g.add_node(SymKind::Constant);
            g.set_leaf_data(n, SymPayload::Int(APInt::new(8, 0xAB)));
            n
        };
        let lo = {
            let n = g.add_node(SymKind::Constant);
            g.set_leaf_data(n, SymPayload::Int(APInt::new(8, 0xCD)));
            n
        };
        inner(&mut g, SymKind::Concat, &[hi, lo]);
        assert_eq!(as_u64(execute(&g, &[])), 0xABCD);
    }

    #[test]
    fn extract_above_mul_yields_signed_high_product() {
        // The RISC-V `mulh` semantics expressed the TMDL way:
        // extract(rs1 * rs2, 127, 64) on 64-bit operands.
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let mul = inner(&mut g, SymKind::Mul, &[a, b]);
        let hi = int_con(&mut g, 127);
        let lo = int_con(&mut g, 64);
        inner(&mut g, SymKind::Extract, &[mul, hi, lo]);

        // -3 * 7 = -21: the high half of the signed 128-bit product is -1.
        let inputs = [
            Value::Int(APInt::new(64, (-3i64) as u64)),
            Value::Int(APInt::new(64, 7)),
        ];
        assert_eq!(as_i64(execute(&g, &inputs)), -1);

        // 2^62 * 4 = 2^64: high half is 1.
        let inputs = [
            Value::Int(APInt::new(64, 1u64 << 62)),
            Value::Int(APInt::new(64, 4)),
        ];
        assert_eq!(as_i64(execute(&g, &inputs)), 1);
    }

    #[test]
    fn addw_tree_sign_extends_low_word() {
        // The RV64 `addw` semantics expressed directly in the graph, no extra
        // primitives: sext(extract(rs1 + rs2, 31, 0), 64).
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let add = inner(&mut g, SymKind::Add, &[a, b]);
        let hi = int_con(&mut g, 31);
        let lo = int_con(&mut g, 0);
        let ext = inner(&mut g, SymKind::Extract, &[add, hi, lo]);
        let width = int_con(&mut g, 64);
        inner(&mut g, SymKind::SExt, &[ext, width]);

        // 0x7FFF_FFFF + 1 = 0x8000_0000, whose low word is negative as i32 and
        // sign-extends to -2147483648 in 64 bits.
        let inputs = [
            Value::Int(APInt::new(64, 0x7FFF_FFFF)),
            Value::Int(APInt::new(64, 1)),
        ];
        assert_eq!(as_i64(execute(&g, &inputs)), -2_147_483_648);
    }

    #[test]
    fn int_and() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::And, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(0b1100), uv(0b1010)])), 0b1000);
    }

    #[test]
    fn int_not() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, SymKind::Not, &[a]);
        assert_eq!(as_u64(execute(&g, &[uv(0b1010)])), 0xFFFF_FFF5);
    }

    #[test]
    fn int_shl() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::ShiftLeft, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(1), uv(3)])), 8);
    }

    #[test]
    fn int_lshr() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::ShiftRightLogic, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[uv(16), uv(2)])), 4);
    }

    #[test]
    fn int_ashr_negative() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::ShiftRightArithmetic, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(-8), iv(1)])), -4);
    }

    #[test]
    fn int_constant() {
        let mut g = Graph::new();
        int_con(&mut g, 42);
        assert_eq!(as_i64(execute(&g, &[])), 42);
    }

    #[test]
    fn int_shared_node() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, SymKind::Add, &[a, a]);
        assert_eq!(as_i64(execute(&g, &[iv(5)])), 10);
    }

    #[test]
    fn int_fma() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        inner(&mut g, SymKind::Fma, &[a, b, c]);
        assert_eq!(as_i64(execute(&g, &[iv(3), iv(4), iv(5)])), 17);
    }

    // ── Comparisons ────────────────────────────────────────────────────────

    #[test]
    fn int_eq() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Eq, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[iv(5), iv(5)])), 1);
        assert_eq!(as_i64(execute(&g, &[iv(5), iv(6)])), 0);
    }

    #[test]
    fn int_if() {
        let mut g = Graph::new();
        let cond = sym(&mut g, 0);
        let t = sym(&mut g, 1);
        let e = sym(&mut g, 2);
        inner(&mut g, SymKind::If, &[cond, t, e]);
        assert_eq!(as_i64(execute(&g, &[iv(1), iv(42), iv(0)])), 42);
        assert_eq!(as_i64(execute(&g, &[iv(0), iv(42), iv(99)])), 99);
    }

    #[test]
    fn int_clamp() {
        let mut g = Graph::new();
        let input = sym(&mut g, 0);
        let min = {
            let node = g.add_node(SymKind::Constant);
            g.set_leaf_data(node, SymPayload::Int(APInt::new_signed(32, 3)));
            node
        };
        let max = {
            let node = g.add_node(SymKind::Constant);
            g.set_leaf_data(node, SymPayload::Int(APInt::new_signed(32, 10)));
            node
        };
        inner(&mut g, SymKind::Clamp, &[input, min, max]);
        assert_eq!(as_i64(execute(&g, &[iv(20)])), 10);
    }

    // ── Float arithmetic ───────────────────────────────────────────────────

    #[test]
    fn float_add() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Add, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(1.5), fv(2.5)])) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn float_div() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Div, &[a, b]);
        assert!((as_f64(execute(&g, &[fv(7.0), fv(2.0)])) - 3.5).abs() < 1e-9);
    }

    #[test]
    fn float_sqrt() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        inner(&mut g, SymKind::Sqrt, &[a]);
        assert!((as_f64(execute(&g, &[fv(9.0)])) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn float_fma() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let c = sym(&mut g, 2);
        inner(&mut g, SymKind::Fma, &[a, b, c]);
        assert!((as_f64(execute(&g, &[fv(2.0), fv(3.0), fv(1.0)])) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn float_lt() {
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        inner(&mut g, SymKind::Lt, &[a, b]);
        assert_eq!(as_i64(execute(&g, &[fv(1.0), fv(2.0)])), 1);
        assert_eq!(as_i64(execute(&g, &[fv(3.0), fv(2.0)])), 0);
    }

    // ── Iterator nodes ─────────────────────────────────────────────────────

    #[test]
    fn split_then_concat_roundtrips_raw_bits() {
        // split a 16-bit raw value 0xBA21 into two bytes, then concat them back.
        let mut g = Graph::new();
        let bits = sym(&mut g, 0);
        let n = int_con(&mut g, 2);
        let split = inner(&mut g, SymKind::Split, &[bits, n]);

        // The lanes are the two bytes read as integers.
        assert_eq!(
            int_lanes(execute(&g, &[rb(&[0x21, 0xBA])])),
            vec![0x21, 0xBA]
        );

        inner(&mut g, SymKind::IterConcat, &[split]);
        assert_eq!(
            raw_bytes(execute(&g, &[rb(&[0x21, 0xBA])])),
            vec![0x21, 0xBA]
        );
    }

    #[test]
    fn map_applies_unary_lambda_per_lane() {
        // map(split(0x0201, 2), |x| x + 1) -> [1+1, 2+1] = [2, 3].
        let mut g = Graph::new();
        let bits = sym(&mut g, 0);
        let n = int_con(&mut g, 2);
        let iter = inner(&mut g, SymKind::Split, &[bits, n]);
        let x = arg(&mut g, 0);
        let one = int_con(&mut g, 1);
        let body = inner(&mut g, SymKind::Add, &[x, one]);
        inner(&mut g, SymKind::Map, &[iter, body]);

        assert_eq!(int_lanes(execute(&g, &[rb(&[0x01, 0x02])])), vec![2, 3]);
    }

    #[test]
    fn zip_then_map_lane_wise_add_concats() {
        // concat(map(zip(split(a, 2), split(b, 2)), |x, y| x + y)) for
        // a=[1,2], b=[3,4] -> lanes [4, 6] -> raw bytes [0x04, 0x06].
        let mut g = Graph::new();
        let a = sym(&mut g, 0);
        let b = sym(&mut g, 1);
        let n = int_con(&mut g, 2);
        let split_a = inner(&mut g, SymKind::Split, &[a, n]);
        let split_b = inner(&mut g, SymKind::Split, &[b, n]);
        let zip = inner(&mut g, SymKind::Zip, &[split_a, split_b]);
        let x = arg(&mut g, 0);
        let y = arg(&mut g, 1);
        let body = inner(&mut g, SymKind::Add, &[x, y]);
        let map = inner(&mut g, SymKind::Map, &[zip, body]);
        inner(&mut g, SymKind::IterConcat, &[map]);

        let out = execute(&g, &[rb(&[0x01, 0x02]), rb(&[0x03, 0x04])]);
        assert_eq!(raw_bytes(out), vec![0x04, 0x06]);
    }

    #[test]
    fn reduce_folds_to_horizontal_sum() {
        // reduce(split(0x04030201, 4), |acc, x| acc + x) -> 1+2+3+4 = 10.
        let mut g = Graph::new();
        let bits = sym(&mut g, 0);
        let n = int_con(&mut g, 4);
        let iter = inner(&mut g, SymKind::Split, &[bits, n]);
        let acc = arg(&mut g, 0);
        let x = arg(&mut g, 1);
        let body = inner(&mut g, SymKind::Add, &[acc, x]);
        inner(&mut g, SymKind::Reduce, &[iter, body]);

        assert_eq!(as_i64(execute(&g, &[rb(&[0x01, 0x02, 0x03, 0x04])])), 10);
    }
}
