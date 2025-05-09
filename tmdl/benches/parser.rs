use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn large_instr_template(c: &mut Criterion) {}

criterion_group!(benches, large_instr_template);
criterion_main!(benches);
