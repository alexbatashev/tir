use std::fmt;

use tir_adt::APInt;
use tir_graph::{MutDag, NodeId};

use crate::lang::{SymKind, SymPayload};

/// Parsed s-expression: surface syntax of an op's `sem = "..."`; [`build`] lowers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemExpr {
    Atom(String),
    List(Vec<SemExpr>),
}

impl SemExpr {
    /// Names of every `$splice` atom, in first-seen order.
    pub fn splice_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_splices(&mut out);
        out
    }

    fn collect_splices(&self, out: &mut Vec<String>) {
        match self {
            SemExpr::Atom(name) => {
                if let Some(method) = name.strip_prefix('$')
                    && !out.iter().any(|n| n == method)
                {
                    out.push(method.to_string());
                }
            }
            SemExpr::List(items) => {
                for item in items {
                    item.collect_splices(out);
                }
            }
        }
    }
}

/// Parse an s-expression; tokens are whitespace/paren delimited, no quotes or escapes.
pub fn parse(input: &str) -> Option<SemExpr> {
    fn parse_list(chars: &[char], pos: &mut usize) -> Option<SemExpr> {
        if *pos >= chars.len() || chars[*pos] != '(' {
            return None;
        }
        *pos += 1;
        let mut items = Vec::new();
        loop {
            while *pos < chars.len() && chars[*pos].is_whitespace() {
                *pos += 1;
            }
            if *pos >= chars.len() {
                return None;
            }
            if chars[*pos] == ')' {
                *pos += 1;
                break;
            }
            if chars[*pos] == '(' {
                items.push(parse_list(chars, pos)?);
                continue;
            }
            let start = *pos;
            while *pos < chars.len()
                && !chars[*pos].is_whitespace()
                && chars[*pos] != '('
                && chars[*pos] != ')'
            {
                *pos += 1;
            }
            items.push(SemExpr::Atom(chars[start..*pos].iter().collect()));
        }
        Some(SemExpr::List(items))
    }

    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    let expr = parse_list(&chars, &mut pos)?;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    if pos == chars.len() { Some(expr) } else { None }
}

/// Op-specific callbacks resolving context-dependent atoms: `$splice` subexprs and result width.
pub trait SemBuilderHooks<G> {
    /// Build the subexpr a `$name` atom stands for, or `None` if unprovided.
    fn splice(&self, name: &str, g: &mut G) -> Option<NodeId>;

    /// Width `sext`/`zext`/`trunc` extend to; `None` if op has no result width.
    fn result_width(&self) -> Option<u64>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    Parse,
    /// The top-level form was not `(set <dst> <rhs>)`.
    NotSet,
    UnknownAtom(String),
    BadForm(String),
    /// A `$name` atom had no matching [`SemBuilderHooks::splice`].
    MissingSplice(String),
    /// A width-changing op was used by an op with no result width.
    MissingWidth,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Parse => write!(f, "malformed s-expression"),
            BuildError::NotSet => write!(f, "expected a top-level (set <dst> <rhs>) form"),
            BuildError::UnknownAtom(a) => write!(f, "unknown atom `{a}`"),
            BuildError::BadForm(s) => write!(f, "malformed `{s}` form"),
            BuildError::MissingSplice(n) => write!(f, "no splice provided for `${n}`"),
            BuildError::MissingWidth => write!(f, "width-changing op needs a result width"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Lower a `sem = "(set <dst> <rhs>)"` declaration into a [`SymKind`] graph.
pub fn build<V, G, H>(
    g: &mut G,
    sem: &str,
    symbols: &[(&str, u32)],
    hooks: &H,
) -> Result<NodeId, BuildError>
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<V>>,
    H: SemBuilderHooks<G>,
{
    let parsed = parse(sem).ok_or(BuildError::Parse)?;
    let SemExpr::List(items) = &parsed else {
        return Err(BuildError::NotSet);
    };
    let [SemExpr::Atom(set_kw), SemExpr::Atom(_dst), rhs] = items.as_slice() else {
        return Err(BuildError::NotSet);
    };
    if set_kw != "set" {
        return Err(BuildError::NotSet);
    }
    let mut lambda_params: Vec<Vec<String>> = Vec::new();
    build_node(g, rhs, symbols, &mut lambda_params, hooks)
}

fn leaf<V, G>(g: &mut G, kind: SymKind, data: SymPayload<V>) -> NodeId
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<V>>,
{
    let n = g.add_node(kind);
    g.set_leaf_data(n, data);
    n
}

fn node<V, G>(g: &mut G, kind: SymKind, children: &[NodeId]) -> NodeId
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<V>>,
{
    let n = g.add_node(kind);
    for &child in children {
        g.add_edge(n, child);
    }
    n
}

fn build_node<V, G, H>(
    g: &mut G,
    expr: &SemExpr,
    symbols: &[(&str, u32)],
    lambda_params: &mut Vec<Vec<String>>,
    hooks: &H,
) -> Result<NodeId, BuildError>
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<V>>,
    H: SemBuilderHooks<G>,
{
    match expr {
        SemExpr::Atom(name) => {
            if let Some(method) = name.strip_prefix('$') {
                hooks
                    .splice(method, g)
                    .ok_or_else(|| BuildError::MissingSplice(method.to_string()))
            } else if let Some(idx) = lambda_params
                .last()
                .and_then(|ps| ps.iter().position(|p| p == name))
            {
                // Lambda param reference lowers to an `Arg` leaf carrying its position.
                Ok(leaf(
                    g,
                    SymKind::Arg,
                    SymPayload::Int(APInt::new(32, idx as u64)),
                ))
            } else if let Some(&(_, idx)) = symbols.iter().find(|(s, _)| *s == name) {
                Ok(leaf(g, SymKind::Symbol, SymPayload::SymbolId(idx)))
            } else if let Ok(i) = name.parse::<i64>() {
                Ok(leaf(
                    g,
                    SymKind::Constant,
                    SymPayload::Int(APInt::new_signed(64, i)),
                ))
            } else {
                Err(BuildError::UnknownAtom(name.clone()))
            }
        }
        SemExpr::List(items) => build_list(g, items, symbols, lambda_params, hooks),
    }
}

fn build_list<V, G, H>(
    g: &mut G,
    items: &[SemExpr],
    symbols: &[(&str, u32)],
    lambda_params: &mut Vec<Vec<String>>,
    hooks: &H,
) -> Result<NodeId, BuildError>
where
    G: MutDag<Node = SymKind, Leaf = SymPayload<V>>,
    H: SemBuilderHooks<G>,
{
    // `(concat iter)`: matched before width-changing ops to avoid the single-operand clash.
    if let [SemExpr::Atom(op), arg] = items
        && op == "concat"
    {
        let inner = build_node(g, arg, symbols, lambda_params, hooks)?;
        return Ok(node(g, SymKind::IterConcat, &[inner]));
    }

    // Unary width-changing ops take width from the result type, not an operand.
    if let [SemExpr::Atom(op), arg] = items {
        let kind = match op.as_str() {
            "sext" => Some(SymKind::SExt),
            "zext" => Some(SymKind::ZExt),
            "trunc" => None,
            other => return Err(BuildError::UnknownAtom(other.to_string())),
        };
        let inner = build_node(g, arg, symbols, lambda_params, hooks)?;
        let width = hooks.result_width().ok_or(BuildError::MissingWidth)?;
        return Ok(match kind {
            Some(kind) => {
                let w = leaf(g, SymKind::Constant, SymPayload::Int(APInt::new(16, width)));
                node(g, kind, &[inner, w])
            }
            None => {
                // trunc x == extract(x, result_width - 1, 0)
                let hi = leaf(
                    g,
                    SymKind::Constant,
                    SymPayload::Int(APInt::new(16, width.saturating_sub(1))),
                );
                let lo = leaf(g, SymKind::Constant, SymPayload::Int(APInt::new(16, 0)));
                node(g, SymKind::Extract, &[inner, hi, lo])
            }
        });
    }

    // `(map iter (lambda (x) body))` / `(reduce iter (lambda (acc x) body))`.
    if let [SemExpr::Atom(op), iter, lambda] = items
        && (op == "map" || op == "reduce")
    {
        let SemExpr::List(parts) = lambda else {
            return Err(BuildError::BadForm(op.clone()));
        };
        let [SemExpr::Atom(lam_kw), SemExpr::List(param_nodes), body] = parts.as_slice() else {
            return Err(BuildError::BadForm(op.clone()));
        };
        if lam_kw != "lambda" {
            return Err(BuildError::BadForm(op.clone()));
        }
        let mut params = Vec::with_capacity(param_nodes.len());
        for p in param_nodes {
            let SemExpr::Atom(p) = p else {
                return Err(BuildError::BadForm(op.clone()));
            };
            params.push(p.clone());
        }

        let iter_node = build_node(g, iter, symbols, lambda_params, hooks)?;
        lambda_params.push(params);
        let body_res = build_node(g, body, symbols, lambda_params, hooks);
        lambda_params.pop();
        let body_node = body_res?;

        let kind = if op == "map" {
            SymKind::Map
        } else {
            SymKind::Reduce
        };
        return Ok(node(g, kind, &[iter_node, body_node]));
    }

    let [SemExpr::Atom(op), lhs, rhs] = items else {
        return Err(BuildError::BadForm("expression".to_string()));
    };
    let kind = match op.as_str() {
        "add" => SymKind::Add,
        "sub" => SymKind::Sub,
        "mul" => SymKind::Mul,
        "div" => SymKind::Div,
        "and" => SymKind::And,
        "or" => SymKind::Or,
        "xor" => SymKind::Xor,
        "shl" => SymKind::ShiftLeft,
        "lshr" => SymKind::ShiftRightLogic,
        "ashr" => SymKind::ShiftRightArithmetic,
        // zip pairs iterators lane-wise; split cuts a bit value into n lanes.
        "zip" => SymKind::Zip,
        "split" => SymKind::Split,
        other => return Err(BuildError::UnknownAtom(other.to_string())),
    };
    let lhs_node = build_node(g, lhs, symbols, lambda_params, hooks)?;
    let rhs_node = build_node(g, rhs, symbols, lambda_params, hooks)?;
    Ok(node(g, kind, &[lhs_node, rhs_node]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{Value, execute};
    use tir_adt::RawBits;
    use tir_graph::{Dag, GenericDag};

    type Graph = GenericDag<SymKind, SymPayload<()>>;

    /// Hooks whose `$get_vlen` splices a constant lane count; mirrors the vector dialect.
    struct TestHooks {
        vlen: u64,
        width: Option<u64>,
    }

    impl SemBuilderHooks<Graph> for TestHooks {
        fn splice(&self, name: &str, g: &mut Graph) -> Option<NodeId> {
            match name {
                "get_vlen" => Some(leaf(
                    g,
                    SymKind::Constant,
                    SymPayload::Int(APInt::new(32, self.vlen)),
                )),
                _ => None,
            }
        }
        fn result_width(&self) -> Option<u64> {
            self.width
        }
    }

    fn no_hooks() -> TestHooks {
        TestHooks {
            vlen: 0,
            width: None,
        }
    }

    #[test]
    fn parses_nested_list() {
        let e = parse("(set r (add lhs rhs))").unwrap();
        assert_eq!(
            e,
            SemExpr::List(vec![
                SemExpr::Atom("set".into()),
                SemExpr::Atom("r".into()),
                SemExpr::List(vec![
                    SemExpr::Atom("add".into()),
                    SemExpr::Atom("lhs".into()),
                    SemExpr::Atom("rhs".into()),
                ]),
            ])
        );
    }

    #[test]
    fn collects_splice_names_uniquely() {
        let e = parse("(set r (add (split a $get_vlen) (split b $get_vlen)))").unwrap();
        assert_eq!(e.splice_names(), vec!["get_vlen".to_string()]);
    }

    #[test]
    fn builds_and_executes_binary_op() {
        let mut g = Graph::new();
        let root = build(
            &mut g,
            "(set result (add lhs rhs))",
            &[("lhs", 0), ("rhs", 1)],
            &no_hooks(),
        )
        .unwrap();
        assert_eq!(*g.get_kind(root), SymKind::Add);
        let out = execute(
            &g,
            &[
                Value::Int(APInt::new_signed(32, 3)),
                Value::Int(APInt::new_signed(32, 4)),
            ],
        );
        match out {
            Value::Int(v) => assert_eq!(v.to_i64(), 7),
            _ => panic!(),
        }
    }

    #[test]
    fn builds_sext_with_result_width() {
        let mut g = Graph::new();
        let root = build(
            &mut g,
            "(set result (sext input))",
            &[("input", 0)],
            &TestHooks {
                vlen: 0,
                width: Some(64),
            },
        )
        .unwrap();
        assert_eq!(*g.get_kind(root), SymKind::SExt);
        let out = execute(&g, &[Value::Int(APInt::new_signed(8, -5))]);
        match out {
            Value::Int(v) => assert_eq!(v.to_i64(), -5),
            _ => panic!(),
        }
    }

    #[test]
    fn builds_trunc_as_extract() {
        let mut g = Graph::new();
        let root = build(
            &mut g,
            "(set result (trunc input))",
            &[("input", 0)],
            &TestHooks {
                vlen: 0,
                width: Some(8),
            },
        )
        .unwrap();
        assert_eq!(*g.get_kind(root), SymKind::Extract);
        let out = execute(&g, &[Value::Int(APInt::new(32, 0x1234))]);
        match out {
            Value::Int(v) => assert_eq!(v.to_u64(), 0x34),
            _ => panic!(),
        }
    }

    #[test]
    fn builds_vector_elementwise_via_splice() {
        // The vector dialect's shape: concat(map(zip(split a, split b), |x,y| x+y)).
        let mut g = Graph::new();
        build(
            &mut g,
            "(set result (concat (map (zip (split lhs $get_vlen) (split rhs $get_vlen)) (lambda (a b) (add a b)))))",
            &[("lhs", 0), ("rhs", 1)],
            &TestHooks { vlen: 2, width: None },
        )
        .unwrap();
        let a = Value::RawBits(RawBits::from_bytes(vec![0x01, 0x02]));
        let b = Value::RawBits(RawBits::from_bytes(vec![0x03, 0x04]));
        match execute(&g, &[a, b]) {
            Value::RawBits(bits) => assert_eq!(bits.bytes(), &[0x04, 0x06]),
            other => panic!("expected raw bits, got {other:?}"),
        }
    }

    #[test]
    fn missing_splice_errors() {
        let mut g = Graph::new();
        let err = build(&mut g, "(set r $nope)", &[], &no_hooks()).unwrap_err();
        assert_eq!(err, BuildError::MissingSplice("nope".into()));
    }
}
