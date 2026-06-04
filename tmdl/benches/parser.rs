use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use tmdl::{lex, parse};

const LARGE_INSTR_TEMPLATE_INPUT: &str = include_str!("./Inputs/large_instr_template.tmdl");

fn large_instr_template(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_instr_template");
    group.throughput(Throughput::Bytes(LARGE_INSTR_TEMPLATE_INPUT.len() as u64));
    group.bench_function("parse", |b| {
        b.iter(|| {
            let (tokens, errs) = lex(LARGE_INSTR_TEMPLATE_INPUT);
            assert!(errs.is_empty());
            let _ = parse(LARGE_INSTR_TEMPLATE_INPUT, &tokens, "<bench>");
        })
    });
    group.finish()
}

criterion_group!(benches, large_instr_template);
criterion_main!(benches);
