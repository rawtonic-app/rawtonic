//! Export encoder timing (criterion). How to run and compare against a
//! base commit: docs/DEVELOPMENT.md "Benchmarks".
//! `cargo bench -p rawtonic-export --bench encode`

#[path = "../../rawdecode/benches/support/mod.rs"]
mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use image::{DynamicImage, ImageBuffer, Rgb, Rgb32FImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{TiffBitDepth, encode_image_to_bytes};
use std::hint::black_box;
use std::time::Duration;

/// The photo display-encoded (sRGB curve) at `max` levels, as the render
/// hands it to the encoder.
fn encoded(photo: &Rgb32FImage, max: f32) -> Vec<f32> {
    photo
        .as_raw()
        .iter()
        .map(|&v| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (e * max).round()
        })
        .collect()
}

/// The full-resolution photo encoded as the export writes it: JPEG at
/// quality 90 and 100 (8-bit) and 16-bit TIFF, sRGB with its ICC profile.
fn encode(c: &mut Criterion) {
    let (id, photo) = support::photo();
    let (w, h) = photo.dimensions();
    let rgb8: Vec<u8> = encoded(&photo, 255.0)
        .into_iter()
        .map(|v| v as u8)
        .collect();
    let rgb16: Vec<u16> = encoded(&photo, 65535.0)
        .into_iter()
        .map(|v| v as u16)
        .collect();
    let eight = DynamicImage::ImageRgb8(ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, rgb8).unwrap());
    let sixteen =
        DynamicImage::ImageRgb16(ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, rgb16).unwrap());
    let mut group = c.benchmark_group("export_encode");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    for (name, image, format, quality, depth) in [
        ("jpeg_q90", &eight, "jpeg", 90, TiffBitDepth::Eight),
        ("jpeg_q100", &eight, "jpeg", 100, TiffBitDepth::Eight),
        ("tiff_16bit", &sixteen, "tiff", 100, TiffBitDepth::Sixteen),
    ] {
        group.bench_function(BenchmarkId::new(name, &id), |b| {
            b.iter(|| {
                encode_image_to_bytes(
                    black_box(image),
                    format,
                    quality,
                    depth,
                    OutputColorSpace::Srgb,
                    None,
                )
                .unwrap()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, encode);
criterion_main!(benches);
