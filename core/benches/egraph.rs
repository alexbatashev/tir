//! Symbolic-math equality-saturation benchmark for TIR's e-graph, modelled on
//! egg's `tests/math.rs`. The `egg_math` bench runs the identical workload on egg
//! for comparison; both build their rules from [`shared::RULES`] and seed the same
//! [`shared::SEED_EXPRS`].

use std::collections::HashMap;
use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use tir::Context;
use tir::egraph::{EClassId, EGraph, EMatch, Rewrite, SaturationLimits};
use tir::graph::{Dag, Matchable, NodeId, Pattern, PatternExpr};

#[path = "math_shared.rs"]
mod shared;
use shared::{Cond, PRE_SAT_ITERS, RULES, RuleSpec, SAT_ITERS, SEED_EXPRS};

/// The math language label. Constants and symbols carry their value/name in the
/// label (not the leaf payload) so e-matching distinguishes `0` from `2` and `x`
/// from `y` by ordinary label equality.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Math {
    Diff,
    Integral,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Ln,
    Sqrt,
    Sin,
    Cos,
    Constant(i64),
    Symbol(String),
}

impl Matchable for Math {
    fn is_leaf(&self, _: &Context) -> bool {
        matches!(self, Math::Constant(_) | Math::Symbol(_))
    }

    fn num_children(&self, _: &Context) -> usize {
        match self {
            Math::Constant(_) | Math::Symbol(_) => 0,
            Math::Ln | Math::Sqrt | Math::Sin | Math::Cos => 1,
            _ => 2,
        }
    }

    // Commutativity is expressed only by the comm-add / comm-mul rules (as in egg),
    // never by the matcher, so both engines explore it the same way.
    fn is_commutative(&self) -> bool {
        false
    }

    fn is_constant(&self) -> bool {
        matches!(self, Math::Constant(_))
    }
}

enum Sexp {
    Atom(String),
    List(Vec<Sexp>),
}

fn tokenize(s: &str) -> Vec<String> {
    s.replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn parse_tokens(toks: &[String], pos: &mut usize) -> Sexp {
    let tok = toks[*pos].clone();
    *pos += 1;
    if tok == "(" {
        let mut items = Vec::new();
        while toks[*pos] != ")" {
            items.push(parse_tokens(toks, pos));
        }
        *pos += 1;
        Sexp::List(items)
    } else {
        Sexp::Atom(tok)
    }
}

fn parse_sexp(s: &str) -> Sexp {
    let toks = tokenize(s);
    let mut pos = 0;
    parse_tokens(&toks, &mut pos)
}

fn is_var(a: &str) -> bool {
    a.starts_with('?')
}

fn atom_str(e: &Sexp) -> &str {
    match e {
        Sexp::Atom(a) => a,
        Sexp::List(_) => panic!("expected operator atom"),
    }
}

fn op_label(head: &str) -> Math {
    match head {
        "+" => Math::Add,
        "-" => Math::Sub,
        "*" => Math::Mul,
        "/" => Math::Div,
        "pow" => Math::Pow,
        "ln" => Math::Ln,
        "sqrt" => Math::Sqrt,
        "sin" => Math::Sin,
        "cos" => Math::Cos,
        "d" => Math::Diff,
        "i" => Math::Integral,
        other => panic!("unknown operator {other}"),
    }
}

fn add_expr(g: &mut EGraph<Math, ()>, e: &Sexp) -> EClassId {
    match e {
        Sexp::Atom(a) => {
            if let Ok(n) = a.parse::<i64>() {
                g.add(Math::Constant(n), &[], None)
            } else {
                g.add(Math::Symbol(a.clone()), &[], None)
            }
        }
        Sexp::List(items) => {
            let children: Vec<EClassId> = items[1..].iter().map(|c| add_expr(g, c)).collect();
            g.add(op_label(atom_str(&items[0])), &children, None)
        }
    }
}

fn build_searcher(e: &Sexp) -> (Pattern<Math, ()>, HashMap<String, NodeId>) {
    let mut p = Pattern::new(());
    let mut vars = HashMap::new();
    let root = add_pat(&mut p, e, &mut vars);
    p.set_root(root);
    (p, vars)
}

fn add_pat(p: &mut Pattern<Math, ()>, e: &Sexp, vars: &mut HashMap<String, NodeId>) -> NodeId {
    match e {
        Sexp::Atom(a) => {
            if is_var(a) {
                *vars
                    .entry(a.clone())
                    .or_insert_with(|| p.add_node(PatternExpr::Boundary))
            } else if let Ok(n) = a.parse::<i64>() {
                p.add_node(PatternExpr::Node(Math::Constant(n)))
            } else {
                p.add_node(PatternExpr::Node(Math::Symbol(a.clone())))
            }
        }
        Sexp::List(items) => {
            let node = p.add_node(PatternExpr::Node(op_label(atom_str(&items[0]))));
            for c in &items[1..] {
                let cn = add_pat(p, c, vars);
                p.add_edge(node, cn);
            }
            node
        }
    }
}

fn instantiate(g: &mut EGraph<Math, ()>, e: &Sexp, vars: &HashMap<String, EClassId>) -> EClassId {
    match e {
        Sexp::Atom(a) => {
            if is_var(a) {
                vars[a]
            } else if let Ok(n) = a.parse::<i64>() {
                g.add(Math::Constant(n), &[], None)
            } else {
                g.add(Math::Symbol(a.clone()), &[], None)
            }
        }
        Sexp::List(items) => {
            let children: Vec<EClassId> =
                items[1..].iter().map(|c| instantiate(g, c, vars)).collect();
            g.add(op_label(atom_str(&items[0])), &children, None)
        }
    }
}

fn class_has(g: &EGraph<Math, ()>, class: EClassId, pred: impl Fn(&Math) -> bool) -> bool {
    g.nodes(class).iter().any(|&id| pred(g.get_node(id)))
}

fn eval_cond(c: &Cond, g: &EGraph<Math, ()>, vars: &HashMap<String, EClassId>) -> bool {
    match *c {
        Cond::NotZero(v) => !class_has(g, vars[v], |n| matches!(n, Math::Constant(0))),
        Cond::Sym(v) => class_has(g, vars[v], |n| matches!(n, Math::Symbol(_))),
        Cond::Const(v) => class_has(g, vars[v], |n| matches!(n, Math::Constant(_))),
        Cond::ConstOrDistinct(cv, xv) => {
            g.find(vars[cv]) != g.find(vars[xv])
                && (class_has(g, vars[cv], |n| matches!(n, Math::Constant(_)))
                    || class_has(g, vars[cv], |n| matches!(n, Math::Symbol(_))))
        }
    }
}

fn build_rule(spec: &RuleSpec) -> Rewrite<Math, ()> {
    let (searcher, var_nodes) = build_searcher(&parse_sexp(spec.lhs));
    let rhs = parse_sexp(spec.rhs);
    let conds = spec.conds;
    let apply = Box::new(
        move |_ctx: &Context, g: &mut EGraph<Math, ()>, m: &EMatch| {
            let vars: HashMap<String, EClassId> = var_nodes
                .iter()
                .map(|(name, &node)| (name.clone(), m.binding(node)))
                .collect();
            if !conds.iter().all(|c| eval_cond(c, g, &vars)) {
                return;
            }
            let new = instantiate(g, &rhs, &vars);
            g.union(m.root(), new);
        },
    );
    Rewrite::new(spec.name, searcher, apply)
}

fn build_rules() -> Vec<Rewrite<Math, ()>> {
    RULES.iter().map(build_rule).collect()
}

fn seed_all() -> EGraph<Math, ()> {
    let mut g = EGraph::new();
    for s in SEED_EXPRS {
        add_expr(&mut g, &parse_sexp(s));
    }
    g
}

fn limits(iters: usize) -> SaturationLimits {
    SaturationLimits {
        max_iterations: iters,
        max_classes: 1_000_000,
    }
}

fn extract_cost(node: &Math, _: &[u64]) -> u64 {
    match node {
        Math::Diff | Math::Integral => 100,
        _ => 1,
    }
}

fn bench_saturate(c: &mut Criterion) {
    let ctx = Context::default();
    let rules = build_rules();
    let mut group = c.benchmark_group("tir_math/saturate");
    for &iters in SAT_ITERS {
        group.bench_with_input(BenchmarkId::from_parameter(iters), &iters, |b, &iters| {
            b.iter_batched(
                seed_all,
                |mut g| {
                    g.saturate(&ctx, &rules, limits(iters));
                    g
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_ematch(c: &mut Criterion) {
    let ctx = Context::default();
    let rules = build_rules();
    let mut g = seed_all();
    g.saturate(&ctx, &rules, limits(PRE_SAT_ITERS));
    let mut group = c.benchmark_group("tir_math/ematch");
    group.bench_function("all_rules", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for rule in &rules {
                total += black_box(g.ematch(&ctx, rule.lhs())).len();
            }
            total
        });
    });
    group.finish();
}

fn bench_extract(c: &mut Criterion) {
    let ctx = Context::default();
    let rules = build_rules();
    let mut g = seed_all();
    g.saturate(&ctx, &rules, limits(PRE_SAT_ITERS));
    let mut group = c.benchmark_group("tir_math/extract");
    group.bench_function("best", |b| {
        b.iter(|| black_box(g.extract_best(extract_cost)));
    });
    group.finish();
}

criterion_group!(benches, bench_saturate, bench_ematch, bench_extract);
criterion_main!(benches);
