use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tir::utils::{DisjointMap, DisjointSet};

const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

fn merge_u64(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}

fn build_chain(n: usize) -> DisjointSet {
    let mut ds = DisjointSet::new(n);
    for i in 0..(n - 1) {
        ds.union(i as u32, (i + 1) as u32);
    }
    ds
}

fn build_chain_map(n: usize) -> DisjointMap<u64, fn(u64, u64) -> u64> {
    let mut map = DisjointMap::new(merge_u64 as fn(u64, u64) -> u64);
    for _ in 0..n {
        map.push(1);
    }
    for i in 0..(n - 1) {
        map.union(i as u32, (i + 1) as u32);
    }
    map
}

fn disjoint_set_find_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_set/find_root");
    for size in SIZES {
        let ds = build_chain(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &ds, |b, ds| {
            b.iter(|| black_box(ds.find_root(0)));
        });
    }
    group.finish();
}

fn disjoint_set_union(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_set/union");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut ds = DisjointSet::new(size);
                for i in 0..(size - 1) {
                    black_box(ds.union(i as u32, (i + 1) as u32));
                }
                ds
            });
        });
    }
    group.finish();
}

fn disjoint_set_connected(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_set/connected");
    for size in SIZES {
        let ds = build_chain(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &ds, |b, ds| {
            b.iter(|| black_box(ds.connected(0, (size - 1) as u32)));
        });
    }
    group.finish();
}

fn disjoint_map_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_map/push");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut map = DisjointMap::new(merge_u64 as fn(u64, u64) -> u64);
                for i in 0..size {
                    black_box(map.push(i as u64));
                }
                map
            });
        });
    }
    group.finish();
}

fn disjoint_map_union(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_map/union");
    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut map = DisjointMap::new(merge_u64 as fn(u64, u64) -> u64);
                for _ in 0..size {
                    map.push(1);
                }
                for i in 0..(size - 1) {
                    black_box(map.union(i as u32, (i + 1) as u32));
                }
                map
            });
        });
    }
    group.finish();
}

fn disjoint_map_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_map/get");
    for size in SIZES {
        let map = build_chain_map(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &map, |b, map| {
            b.iter(|| black_box(map.get((size - 1) as u32)));
        });
    }
    group.finish();
}

fn disjoint_map_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("disjoint_map/set");
    for size in SIZES {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || build_chain_map(size),
                |mut map| map.set((size - 1) as u32, black_box(42)),
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    disjoint_set_find_root,
    disjoint_set_union,
    disjoint_set_connected,
    disjoint_map_push,
    disjoint_map_union,
    disjoint_map_get,
    disjoint_map_set,
);
criterion_main!(benches);
