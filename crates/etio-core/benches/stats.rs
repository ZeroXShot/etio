//! Micro-benchmarks for the per-observation hot paths.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use etio_core::rng::Rng;
use etio_core::sketch::DDSketch;
use etio_core::stats::evt::{Spot, SpotConfig};
use etio_core::stats::window::SortedWindow;
use std::hint::black_box;

fn sketch(c: &mut Criterion) {
    let mut rng = Rng::seed_from_u64(1);
    let xs: Vec<f64> = (0..10_000).map(|_| rng.lognormal(15.0, 1.0)).collect();
    c.bench_function("ddsketch/add", |b| {
        let mut s = DDSketch::default();
        let mut i = 0;
        b.iter(|| {
            s.add(black_box(xs[i % xs.len()]));
            i += 1;
        });
    });
    let mut full = DDSketch::default();
    xs.iter().for_each(|&x| full.add(x));
    c.bench_function("ddsketch/merge", |b| {
        b.iter_batched(
            || full.clone(),
            |mut s| {
                s.merge(black_box(&full)).expect("same accuracy");
                s
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("ddsketch/p99", |b| b.iter(|| black_box(&full).quantile(0.99)));
}

fn window(c: &mut Criterion) {
    let mut rng = Rng::seed_from_u64(2);
    let xs: Vec<f64> = (0..10_000).map(|_| rng.normal(100.0, 10.0)).collect();
    let mut w = SortedWindow::new(360);
    xs.iter().take(360).for_each(|&x| {
        w.push(x);
    });
    let mut i = 0;
    c.bench_function("sorted_window/push+median+mad (n=360)", |b| {
        b.iter(|| {
            w.push(black_box(xs[i % xs.len()]));
            i += 1;
            black_box((w.median(), w.mad()))
        });
    });
}

fn spot(c: &mut Criterion) {
    let mut rng = Rng::seed_from_u64(3);
    let xs: Vec<f64> = (0..20_000).map(|_| rng.normal(0.0, 1.0)).collect();
    let cal = &xs[..2_000];
    let mut s = Spot::calibrated(SpotConfig::default(), cal);
    let mut i = 0;
    c.bench_function("spot/observe", |b| {
        b.iter(|| {
            let v = s.observe(black_box(xs[i % xs.len()]));
            i += 1;
            v
        });
    });
}

criterion_group!(benches, sketch, window, spot);
criterion_main!(benches);
