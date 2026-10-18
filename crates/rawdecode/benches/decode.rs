//! RAW decode and demosaic timing (criterion). How to run and compare
//! against a base commit: docs/DEVELOPMENT.md "Benchmarks".
//! `cargo bench -p rawtonic-rawdecode --bench decode`

#[path = "support/mod.rs"]
mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rawtonic_rawdecode::{demosaic, raw_processing};
use std::hint::black_box;
use std::time::Duration;

/// File bytes in memory to oriented linear RGB, per format family.
fn raw_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_decode");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(12));
    for (family, id, bytes) in support::raw_files() {
        group.bench_function(BenchmarkId::new(family, id), |b| {
            b.iter(|| {
                raw_processing::develop_raw_image(
                    black_box(&bytes),
                    false,
                    2.5,
                    "auto".into(),
                    None,
                )
                .unwrap()
            })
        });
    }
    group.finish();
}

/// The CPU demosaic alone, on the photo sampled back into each mosaic
/// (X-Trans includes its fine-grain smoothing).
fn demosaic(c: &mut Criterion) {
    let (id, photo) = support::photo();
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let mut group = c.benchmark_group("demosaic");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    for (family, cfa) in [("bayer", support::bayer()), ("xtrans", support::xtrans())] {
        let mosaic = support::mosaic(&photo, &cfa);
        group.bench_function(BenchmarkId::new(family, &id), |b| {
            b.iter(|| demosaic::demosaic(black_box(&mosaic), w, h, &cfa))
        });
    }
    group.finish();
}

criterion_group!(benches, raw_decode, demosaic);
criterion_main!(benches);
