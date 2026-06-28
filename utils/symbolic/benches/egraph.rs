//! Symbolic-math equality-saturation benchmark for the `tir-symbolic` e-graph,
//! modelled on egg's `tests/math.rs`. The `egg_math` bench runs the identical
//! workload on egg for comparison; both build their rules from [`shared::RULES`]
//! and seed the same [`shared::SEED_EXPRS`].

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use tir_adt::APInt;
use tir_symbolic::egraph::{EGraph, ENode, Id, Pattern, Rewrite, Rhs, Substitution, Var};

#[path = "math_shared.rs"]
mod shared;
use shared::{Cond, PRE_SAT_ITERS, RULES, RuleSpec, SAT_ITERS, SEED_EXPRS};

const NODE_LIMIT: usize = 1_000_000;

/// The math language label. Operands are child e-class [`Id`]s carried inline;
/// constants and symbols carry their value/name so hash-consing and matching tell
/// `0` from `2` and `x` from `y` by ordinary label equality.
#[derive(Clone, Debug)]
enum Math {
    Diff([Id; 2]),
    Integral([Id; 2]),
    Add([Id; 2]),
    Sub([Id; 2]),
    Mul([Id; 2]),
    Div([Id; 2]),
    Pow([Id; 2]),
    Ln([Id; 1]),
    Sqrt([Id; 1]),
    Sin([Id; 1]),
    Cos([Id; 1]),
    Constant(i64),
    Symbol(String),
}

impl ENode for Math {
    fn children(&self) -> &[Id] {
        match self {
            Math::Diff(a)
            | Math::Integral(a)
            | Math::Add(a)
            | Math::Sub(a)
            | Math::Mul(a)
            | Math::Div(a)
            | Math::Pow(a) => a,
            Math::Ln(a) | Math::Sqrt(a) | Math::Sin(a) | Math::Cos(a) => a,
            Math::Constant(_) | Math::Symbol(_) => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Id] {
        match self {
            Math::Diff(a)
            | Math::Integral(a)
            | Math::Add(a)
            | Math::Sub(a)
            | Math::Mul(a)
            | Math::Div(a)
            | Math::Pow(a) => a,
            Math::Ln(a) | Math::Sqrt(a) | Math::Sin(a) | Math::Cos(a) => a,
            Math::Constant(_) | Math::Symbol(_) => &mut [],
        }
    }

    fn hash_cons(&self) -> u64 {
        let mut h = DefaultHasher::new();
        std::mem::discriminant(self).hash(&mut h);
        match self {
            Math::Constant(n) => n.hash(&mut h),
            Math::Symbol(s) => s.hash(&mut h),
            _ => {}
        }
        h.finish()
    }

    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Math::Constant(a), Math::Constant(b)) => a == b,
            (Math::Symbol(a), Math::Symbol(b)) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }

    fn from_int(value: APInt) -> Option<Self> {
        Some(Math::Constant(value.to_i64()))
    }
}

/// Build the operator label for `head` over already-built `children`.
fn make_node(head: &str, children: &[Id]) -> Math {
    let two = || [children[0], children[1]];
    let one = || [children[0]];
    match head {
        "+" => Math::Add(two()),
        "-" => Math::Sub(two()),
        "*" => Math::Mul(two()),
        "/" => Math::Div(two()),
        "pow" => Math::Pow(two()),
        "ln" => Math::Ln(one()),
        "sqrt" => Math::Sqrt(one()),
        "sin" => Math::Sin(one()),
        "cos" => Math::Cos(one()),
        "d" => Math::Diff(two()),
        "i" => Math::Integral(two()),
        other => panic!("unknown operator {other}"),
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

fn add_expr(g: &mut EGraph<Math>, e: &Sexp) -> Id {
    match e {
        Sexp::Atom(a) => {
            if let Ok(n) = a.parse::<i64>() {
                g.add(Math::Constant(n))
            } else {
                g.add(Math::Symbol(a.clone()))
            }
        }
        Sexp::List(items) => {
            let children: Vec<Id> = items[1..].iter().map(|c| add_expr(g, c)).collect();
            g.add(make_node(atom_str(&items[0]), &children))
        }
    }
}

/// Build a pattern from `e`, reusing one hole per `?var` name.
fn build_pattern(e: &Sexp) -> Pattern<Math, String> {
    let mut p = Pattern::new();
    let mut vars = HashMap::new();
    let root = add_pat(&mut p, e, &mut vars);
    p.set_root(root);
    p
}

fn add_pat(p: &mut Pattern<Math, String>, e: &Sexp, vars: &mut HashMap<String, Id>) -> Id {
    match e {
        Sexp::Atom(a) => {
            if is_var(a) {
                *vars
                    .entry(a.clone())
                    .or_insert_with(|| p.var(Var::Symbol(a.clone())))
            } else if let Ok(n) = a.parse::<i64>() {
                p.var(Var::Int(APInt::from_i64(n)))
            } else {
                p.add(Math::Symbol(a.clone()))
            }
        }
        Sexp::List(items) => {
            let children: Vec<Id> = items[1..].iter().map(|c| add_pat(p, c, vars)).collect();
            p.add(make_node(atom_str(&items[0]), &children))
        }
    }
}

fn class_has(g: &EGraph<Math>, class: Id, pred: impl Fn(&Math) -> bool) -> bool {
    g.nodes(class).iter().any(pred)
}

fn var_class(subst: &Substitution<String>, v: &str) -> Id {
    subst
        .get(&Var::Symbol(v.to_string()))
        .expect("condition variable is bound by the searcher")
}

fn eval_cond(c: &Cond, g: &EGraph<Math>, subst: &Substitution<String>) -> bool {
    match *c {
        Cond::NotZero(v) => !class_has(g, var_class(subst, v), |n| matches!(n, Math::Constant(0))),
        Cond::Sym(v) => class_has(g, var_class(subst, v), |n| matches!(n, Math::Symbol(_))),
        Cond::Const(v) => class_has(g, var_class(subst, v), |n| matches!(n, Math::Constant(_))),
        Cond::ConstOrDistinct(cv, xv) => {
            let cc = var_class(subst, cv);
            g.find(cc) != g.find(var_class(subst, xv))
                && (class_has(g, cc, |n| matches!(n, Math::Constant(_)))
                    || class_has(g, cc, |n| matches!(n, Math::Symbol(_))))
        }
    }
}

fn build_rule(spec: &RuleSpec) -> Rewrite<Math, String> {
    let lhs = build_pattern(&parse_sexp(spec.lhs));
    let rhs = build_pattern(&parse_sexp(spec.rhs));
    let conds = spec.conds;
    let apply = Box::new(
        move |g: &mut EGraph<Math>, subst: &Substitution<String>, root: Id| {
            if !conds.iter().all(|c| eval_cond(c, g, subst)) {
                return;
            }
            let new = rhs.instantiate(g, subst);
            g.union(root, new);
        },
    );
    Rewrite::new(spec.name, lhs, Rhs::Apply(apply))
}

fn build_rules() -> Vec<Rewrite<Math, String>> {
    RULES.iter().map(build_rule).collect()
}

fn seed_all() -> EGraph<Math> {
    let mut g = EGraph::new();
    for s in SEED_EXPRS {
        add_expr(&mut g, &parse_sexp(s));
    }
    g
}

fn extract_cost(node: &Math) -> u64 {
    match node {
        Math::Diff(_) | Math::Integral(_) => 100,
        _ => 1,
    }
}

fn bench_saturate(c: &mut Criterion) {
    let rules = build_rules();
    let mut group = c.benchmark_group("tir_math/saturate");
    for &iters in SAT_ITERS {
        group.bench_with_input(BenchmarkId::from_parameter(iters), &iters, |b, &iters| {
            b.iter_batched(
                seed_all,
                |mut g| {
                    g.saturate(&rules, iters, NODE_LIMIT);
                    g
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_ematch(c: &mut Criterion) {
    let rules = build_rules();
    let mut g = seed_all();
    g.saturate(&rules, PRE_SAT_ITERS, NODE_LIMIT);
    let mut group = c.benchmark_group("tir_math/ematch");
    group.bench_function("all_rules", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for rule in &rules {
                total += black_box(rule.lhs.search(&g)).len();
            }
            total
        });
    });
    group.finish();
}

fn bench_extract(c: &mut Criterion) {
    let rules = build_rules();
    let mut g = seed_all();
    g.saturate(&rules, PRE_SAT_ITERS, NODE_LIMIT);
    let mut group = c.benchmark_group("tir_math/extract");
    group.bench_function("best", |b| {
        b.iter(|| black_box(g.extract_best(extract_cost)));
    });
    group.finish();
}

criterion_group!(benches, bench_saturate, bench_ematch, bench_extract);
criterion_main!(benches);
