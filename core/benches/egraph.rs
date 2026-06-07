use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tir::Context;
use tir::egraph::{EGraph, Rewrite, SaturationLimits};
use tir::graph::{Dag, GenericDag, MutDag, NodeId, Pattern, PatternExpr};
use tir::sem_expr::ExprKind;

const SIZES: [usize; 3] = [100, 1_000, 5_000];
const DEPTHS: [usize; 3] = [10, 50, 200];
const CONGRUENCE_SIZES: [usize; 3] = [50, 200, 500];
const TREE_DEPTHS: [usize; 3] = [4, 6, 8];

fn sym(g: &mut EGraph<ExprKind, ()>) -> tir::egraph::EClassId {
    g.add(ExprKind::Symbol, &[], None)
}

fn unary(
    g: &mut EGraph<ExprKind, ()>,
    k: ExprKind,
    a: tir::egraph::EClassId,
) -> tir::egraph::EClassId {
    g.add(k, &[a], None)
}

fn build_sqrt_chain(depth: usize) -> EGraph<ExprKind, ()> {
    let mut g = EGraph::new();
    let mut cur = sym(&mut g);
    for _ in 0..depth {
        cur = unary(&mut g, ExprKind::Sqrt, cur);
    }
    g
}

fn build_shared_adds(n: usize) -> (EGraph<ExprKind, ()>, tir::egraph::EClassId) {
    let mut g = EGraph::new();
    let a = sym(&mut g);
    let b = sym(&mut g);
    let mut last = g.add(ExprKind::Add, &[a, b], None);
    for _ in 1..n {
        last = g.add(ExprKind::Add, &[a, b], None);
    }
    (g, last)
}

fn build_many_adds(n: usize) -> EGraph<ExprKind, ()> {
    let mut g = EGraph::new();
    for i in 0..n {
        let a = sym(&mut g);
        let b = sym(&mut g);
        black_box(g.add(ExprKind::Add, &[a, b], None));
        black_box(i);
    }
    g
}

fn build_union_chain(n: usize) -> EGraph<ExprKind, ()> {
    let mut g = EGraph::new();
    let mut classes: Vec<tir::egraph::EClassId> = (0..n).map(|_| sym(&mut g)).collect();
    for i in 1..n {
        let merged = g.union(classes[i - 1], classes[i]);
        classes[i] = merged;
    }
    g
}

/// `n` symbols each wrapped in a distinct `Sqrt`, then every symbol unioned into
/// the first. The unions are left unrepaired so a following `rebuild` must collapse
/// all `n` `Sqrt` applications into one class — the congruence-heavy case.
fn build_congruent_graph(n: usize) -> EGraph<ExprKind, ()> {
    let mut g = EGraph::new();
    let symbols: Vec<_> = (0..n).map(|_| sym(&mut g)).collect();
    let sqrts: Vec<_> = symbols
        .iter()
        .map(|&s| unary(&mut g, ExprKind::Sqrt, s))
        .collect();
    black_box(&sqrts);
    for i in 1..n {
        g.union(symbols[0], symbols[i]);
    }
    g
}

fn build_binary_tree_dag(depth: usize) -> GenericDag<ExprKind, ()> {
    let mut dag = GenericDag::new();
    let leaf_count = 1usize << (depth - 1);
    let mut level: Vec<NodeId> = (0..leaf_count)
        .map(|_| dag.add_node(ExprKind::Symbol))
        .collect();
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in level.chunks(2) {
            let add = dag.add_node(ExprKind::Add);
            dag.add_edge(add, chunk[0]);
            dag.add_edge(add, chunk[1]);
            next.push(add);
        }
        level = next;
    }
    dag
}

fn add_pattern() -> Pattern<ExprKind, ()> {
    let mut pattern = Pattern::new(());
    let pl = pattern.add_node(PatternExpr::Leaf);
    let pr = pattern.add_node(PatternExpr::Leaf);
    let proot = pattern.add_node(PatternExpr::Node(ExprKind::Add));
    pattern.add_edge(proot, pl);
    pattern.add_edge(proot, pr);
    pattern.set_root(proot);
    pattern
}

fn mul_to_add_rewrite() -> Rewrite<ExprKind, ()> {
    let mut searcher = Pattern::new(());
    let sl = searcher.add_node(PatternExpr::Boundary);
    let sr = searcher.add_node(PatternExpr::Boundary);
    let sroot = searcher.add_node(PatternExpr::Node(ExprKind::Mul));
    searcher.add_edge(sroot, sl);
    searcher.add_edge(sroot, sr);
    searcher.set_root(sroot);

    Rewrite::new(
        "mul-to-add",
        searcher,
        Box::new(|_ctx, g, m| {
            let l = m.binding(NodeId::from_index(0));
            let r = m.binding(NodeId::from_index(1));
            let added = g.add(ExprKind::Add, &[l, r], None);
            g.union(m.root(), added);
        }),
    )
}

fn egraph_add_unique(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/add_unique");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut g = EGraph::new();
                for _ in 0..size {
                    black_box(sym(&mut g));
                }
                g
            });
        });
    }
    group.finish();
}

fn egraph_add_shared(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/add_shared");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut g = EGraph::new();
                let a = sym(&mut g);
                let b = sym(&mut g);
                for _ in 0..size {
                    black_box(g.add(ExprKind::Add, &[a, b], None));
                }
                g
            });
        });
    }
    group.finish();
}

fn egraph_add_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/add_chain");
    for depth in DEPTHS {
        group.throughput(Throughput::Elements(depth as u64));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            b.iter(|| build_sqrt_chain(depth));
        });
    }
    group.finish();
}

fn egraph_union(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/union");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut g = EGraph::new();
                let mut classes: Vec<_> = (0..size).map(|_| sym(&mut g)).collect();
                for i in 1..size {
                    classes[i] = black_box(g.union(classes[i - 1], classes[i]));
                }
                g
            });
        });
    }
    group.finish();
}

fn egraph_find(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/find");
    for size in SIZES {
        let g = build_union_chain(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &g, |b, g| {
            let class = g.classes().next().unwrap();
            b.iter(|| black_box(g.find(class)));
        });
    }
    group.finish();
}

fn egraph_class_of(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/class_of");
    for size in SIZES {
        let g = build_sqrt_chain(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &g, |b, g| {
            let node = NodeId::from_index(g.len() - 1);
            b.iter(|| black_box(g.class_of(node)));
        });
    }
    group.finish();
}

fn egraph_rebuild_congruent(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/rebuild_congruent");
    for size in CONGRUENCE_SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || build_congruent_graph(size),
                |mut g| g.rebuild(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn egraph_rebuild(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/rebuild");
    for depth in DEPTHS {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            b.iter_batched(
                || {
                    let mut g = build_sqrt_chain(depth);
                    let a = sym(&mut g);
                    let fa = unary(&mut g, ExprKind::Sqrt, a);
                    g.union(fa, a);
                    g
                },
                |mut g| g.rebuild(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn egraph_add_dag(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/add_dag");
    for depth in TREE_DEPTHS {
        let dag = build_binary_tree_dag(depth);
        let root = dag.root().unwrap();
        let nodes = 1usize << (depth - 1);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &(dag, root),
            |b, (dag, root)| {
                b.iter(|| {
                    let mut g = EGraph::new();
                    black_box(g.add_dag(dag, *root));
                    g
                });
            },
        );
    }
    group.finish();
}

fn egraph_ematch(c: &mut Criterion) {
    let ctx = Context::default();
    let pattern = add_pattern();
    let mut group = c.benchmark_group("egraph/ematch");
    for size in SIZES {
        let g = build_many_adds(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &g, |b, g| {
            b.iter(|| black_box(g.ematch(&ctx, &pattern)));
        });
    }
    group.finish();
}

fn egraph_saturate(c: &mut Criterion) {
    let ctx = Context::default();
    let rewrite = mul_to_add_rewrite();
    let rewrites = [rewrite];
    let limits = SaturationLimits {
        max_iterations: 30,
        max_classes: 10_000,
    };
    let mut group = c.benchmark_group("egraph/saturate");
    for size in [10, 50, 100] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let mut g = EGraph::new();
                    for _ in 0..size {
                        let a = sym(&mut g);
                        let b = sym(&mut g);
                        g.add(ExprKind::Mul, &[a, b], None);
                    }
                    g
                },
                |mut g| g.saturate(&ctx, &rewrites, limits),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn egraph_extract_best(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/extract_best");
    for depth in DEPTHS {
        let g = build_sqrt_chain(depth);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &g, |b, g| {
            b.iter(|| {
                black_box(g.extract_best(|kind, _| match kind {
                    ExprKind::Mul => 100,
                    ExprKind::Add => 1,
                    _ => 1,
                }))
            });
        });
    }
    group.finish();
}

fn egraph_nodes_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("egraph/nodes_lookup");
    for size in SIZES {
        let (g, class) = build_shared_adds(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &(g, class),
            |b, (g, class)| {
                b.iter(|| black_box(g.nodes(*class)));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    egraph_add_unique,
    egraph_add_shared,
    egraph_add_chain,
    egraph_union,
    egraph_find,
    egraph_class_of,
    egraph_rebuild_congruent,
    egraph_rebuild,
    egraph_add_dag,
    egraph_ematch,
    egraph_saturate,
    egraph_extract_best,
    egraph_nodes_lookup,
);
criterion_main!(benches);
