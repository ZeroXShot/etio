//! Benchmarks for trace analysis and log template mining.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use etio_core::rng::Rng;
use etio_core::{Interner, Sym};
use etio_pipeline::logs::{Drain, DrainConfig};
use etio_pipeline::trace::analyze;
use etio_pipeline::{Span, SpanKind, SpanStatus};
use std::hint::black_box;

/// A synthetic trace: a fan-out tree over `services` services.
fn trace(rng: &mut Rng, interner: &Interner, n: usize, services: usize) -> Vec<Span> {
    let names: Vec<Sym> = (0..services).map(|i| interner.intern(&format!("svc-{i}"))).collect();
    let mut spans = Vec::with_capacity(n);
    for i in 0..n {
        let parent = if i == 0 { 0 } else { rng.below(i as u64) + 1 };
        let start = i64::try_from(i).unwrap_or(0) * 1_000;
        spans.push(Span {
            trace_id: 7,
            span_id: i as u64 + 1,
            parent_id: parent,
            service: names[rng.index(services)],
            operation: Sym::EMPTY,
            kind: SpanKind::Unspecified,
            start,
            end: start + 50_000 + i64::try_from(rng.below(100_000)).unwrap_or(0),
            status: if rng.chance(0.01) { SpanStatus::Error } else { SpanStatus::Unset },
            peer: Sym::EMPTY,
        });
    }
    spans
}

fn traces(c: &mut Criterion) {
    let interner = Interner::default();
    let mut rng = Rng::seed_from_u64(1);
    let mut g = c.benchmark_group("trace/analyze");
    for n in [8usize, 64, 512] {
        let t = trace(&mut rng, &interner, n, 12);
        g.throughput(Throughput::Elements(n as u64));
        g.bench_function(format!("{n} spans"), |b| b.iter(|| analyze(black_box(&t))));
    }
    g.finish();
}

fn drain(c: &mut Criterion) {
    let mut rng = Rng::seed_from_u64(2);
    let verbs = ["served", "rejected", "queued", "retried"];
    let lines: Vec<String> = (0..10_000)
        .map(|i| format!("request {i} {} in {}ms by worker-{}", verbs[rng.index(4)], rng.below(900), rng.below(16)))
        .collect();
    let mut g = c.benchmark_group("drain");
    g.throughput(Throughput::Elements(1));
    g.bench_function("add", |b| {
        let mut d = Drain::new(DrainConfig::default());
        let mut i = 0;
        b.iter(|| {
            let m = d.add(black_box(&lines[i % lines.len()]), 0);
            i += 1;
            m
        });
    });
    g.finish();
}

criterion_group!(benches, traces, drain);
criterion_main!(benches);
