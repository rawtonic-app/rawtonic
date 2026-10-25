//! CPU develop stages and mask rasterisation timing (criterion). How to run
//! and compare against a base commit: docs/DEVELOPMENT.md "Benchmarks".
//! `cargo bench -p rawtonic-engine --features test-support --bench develop`

#[path = "../../rawdecode/benches/support/mod.rs"]
mod support;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::image_processing::{GeometryParams, warp_image_geometry};
use rawtonic_engine::mask_generation::{
    MaskDefinition, ParametricMaskSource, PhotoGeometry, clear_brush_prefix_cache,
    generate_layer_mask,
};
use rawtonic_engine::{color_noise, edge_fringe, lateral_ca};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

/// The colour noise versions these numbers are for; a newer version gets
/// its own entry beside the older ones.
const COLOR_NOISE_VERSIONS: [u64; 2] = [6, 7];
/// Preview width of the mask benchmarks (the Develop view's preview).
const PREVIEW_WIDTH: u32 = 1536;

/// The centre `w` x `h` of `image`.
fn centre(image: &Rgb32FImage, w: u32, h: u32) -> Rgb32FImage {
    let (w, h) = (w.min(image.width()), h.min(image.height()));
    let (x0, y0) = ((image.width() - w) / 2, (image.height() - h) / 2);
    image::imageops::crop_imm(image, x0, y0, w, h).to_image()
}

/// Colour NR on the CPU: the chroma pyramid the renderer builds for every
/// development, and the whole CPU filter (the reference the GPU path
/// matches) on a 6 MP centre.
fn color_noise(c: &mut Criterion, id: &str, photo: &Rgb32FImage, version: u64) {
    let tuning = color_noise::Tuning::for_version(version);
    let settings = color_noise::Settings {
        amount: 0.25,
        detail: 0.5,
        smoothness: 0.5,
    };
    let full = DynamicImage::ImageRgb32F(photo.clone());
    let crop = centre(photo, 3000, 2000);
    let mut group = c.benchmark_group(format!("color_nr_v{version}"));
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    group.bench_function(BenchmarkId::new("pyramid", id), |b| {
        b.iter(|| color_noise::ChromaPyramid::build_for(black_box(&full), true, &tuning))
    });
    group.bench_function(BenchmarkId::new("cpu_filter_6mp", id), |b| {
        b.iter(|| color_noise::apply_to_linear(black_box(&crop), settings, &tuning))
    });
    group.finish();
}

/// The edge-fringe pass (`fringeVersion` 2, 3 and 4, full resolution) and the
/// automatic lateral CA estimate (version 2, as without lens CA data).
fn fringe_and_ca(c: &mut Criterion, id: &str, photo: &Rgb32FImage) {
    let image = DynamicImage::ImageRgb32F(photo.clone());
    let mut group = c.benchmark_group("lens");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    for version in [2, 3, 4] {
        let recipe = json!({ (edge_fringe::VERSION_KEY): version });
        group.bench_function(
            BenchmarkId::new(format!("edge_fringe_v{version}"), id),
            |b| {
                b.iter_batched(
                    || image.clone(),
                    |image| {
                        edge_fringe::apply_for_recipe(Cow::Owned(image), &recipe, 1.0)
                            .unwrap()
                            .into_owned()
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    group.bench_function(BenchmarkId::new("lateral_ca_estimate_v2", id), |b| {
        b.iter(|| lateral_ca::estimate_v2(black_box(photo)))
    });
    // The full-resolution lens warp with a lens profile's distortion,
    // vignetting and lateral CA (every never-edited RAW with lens data).
    let params = GeometryParams {
        lens_dist_k1: -0.04,
        lens_dist_k2: 0.01,
        lens_distortion_amount: 1.0,
        tca_vr: 1.0004,
        tca_vb: 0.9997,
        vig_k1: -0.3,
        vig_k2: 0.05,
        lens_vignette_amount: 1.0,
        ..GeometryParams::default()
    };
    group.bench_function(BenchmarkId::new("warp_lens_ca", id), |b| {
        b.iter(|| warp_image_geometry(black_box(&image), params.clone()))
    });
    group.finish();
}

fn layer(sub: Value, refine_edge: Option<f32>) -> MaskDefinition {
    serde_json::from_value(json!({
        "id": "l", "name": "l", "visible": true, "invert": false, "opacity": 100.0,
        "adjustments": {}, "subMasks": [sub], "maskVersion": 2, "refineEdge": refine_edge,
    }))
    .unwrap()
}

/// A brush stroke across the middle of a `w` x `h` image.
fn stroke(w: u32, h: u32, size: f64, auto_mask: bool) -> Value {
    let points: Vec<Value> = (0..40)
        .map(|i| {
            let t = i as f64 / 39.0;
            json!({"x": w as f64 * (0.1 + 0.8 * t), "y": h as f64 * (0.5 + 0.1 * (t * 9.0).sin())})
        })
        .collect();
    json!({"id": "b", "type": "brush", "visible": true, "mode": "additive",
        "parameters": {"lines": [{"tool": "brush", "brushSize": size, "feather": 0.5,
            "hardness": 80.0, "points": points, "autoMask": auto_mask, "autoMaskVersion": 2}]}})
}

fn radial(w: u32, h: u32) -> Value {
    json!({"id": "r", "type": "radial", "visible": true, "mode": "additive",
        "parameters": {"centerX": w as f64 * 0.5, "centerY": h as f64 * 0.5,
            "radiusX": w as f64 * 0.3, "radiusY": h as f64 * 0.35, "rotation": 10.0, "feather": 0.2}})
}

/// Layer masks as Develop rasterises them: a whole Auto Mask v2 brush
/// stroke on the preview (the brush cache emptied before each sample, so
/// every stroke is drawn in full), and Refine edge on a radial layer at
/// preview and full resolution.
fn masks(c: &mut Criterion, id: &str, photo: &Rgb32FImage) {
    let preview = support::reduced(photo, PREVIEW_WIDTH);
    let geometry = PhotoGeometry::default();
    let source = |image: &Rgb32FImage| {
        ParametricMaskSource::new(
            Arc::new(DynamicImage::ImageRgb32F(image.clone())),
            (image.width() as f64, image.height() as f64),
        )
        .unwrap()
    };
    let (pw, ph) = preview.dimensions();
    let (fw, fh) = photo.dimensions();
    let preview_source = source(&preview);
    let full_source = source(photo);
    let mut group = c.benchmark_group("mask");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(8));
    for (name, auto_mask) in [
        ("brush_stroke_preview", false),
        ("auto_mask_v2_stroke_preview", true),
    ] {
        let def = layer(stroke(pw, ph, 60.0, auto_mask), None);
        group.bench_function(BenchmarkId::new(name, id), |b| {
            b.iter_batched(
                clear_brush_prefix_cache,
                |()| {
                    generate_layer_mask(
                        &def,
                        pw,
                        ph,
                        1.0,
                        (0.0, 0.0),
                        &geometry,
                        Some(&preview_source),
                    )
                },
                BatchSize::PerIteration,
            )
        });
    }
    let def = layer(radial(pw, ph), Some(10.0));
    group.bench_function(BenchmarkId::new("refine_edge_10_preview", id), |b| {
        b.iter(|| {
            generate_layer_mask(
                &def,
                pw,
                ph,
                1.0,
                (0.0, 0.0),
                &geometry,
                Some(&preview_source),
            )
        })
    });
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    let def = layer(radial(fw, fh), Some(40.0));
    group.bench_function(BenchmarkId::new("refine_edge_40_full", id), |b| {
        b.iter(|| generate_layer_mask(&def, fw, fh, 1.0, (0.0, 0.0), &geometry, Some(&full_source)))
    });
    group.finish();
}

fn develop(c: &mut Criterion) {
    let (id, photo) = support::photo();
    for version in COLOR_NOISE_VERSIONS {
        color_noise(c, &id, &photo, version);
    }
    fringe_and_ca(c, &id, &photo);
    masks(c, &id, &photo);
}

criterion_group!(benches, develop);
criterion_main!(benches);
