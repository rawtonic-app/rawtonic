// SPDX-License-Identifier: AGPL-3.0-or-later
//! The installed model on a 24 MP image (ignored; needs the model files):
//! `SUPERRES_MODELS=<model folder> cargo test -p rawtonic-superres --release
//! --test real_model -- --ignored --nocapture` (on ONNX Runtime also
//! `ORT_DYLIB_PATH`). Prints the load, first-tile and whole-image times and
//! checks that tiles join into the model's own whole-tile result.

use std::time::Instant;

use image::{DynamicImage, ImageBuffer, Rgb};
use rawtonic_superres::{MARGIN, Model, SCALE, TILE};

fn models() -> Option<std::path::PathBuf> {
    std::env::var_os("SUPERRES_MODELS").map(Into::into)
}

/// A photo-like test pattern: smooth gradients, fine lines and a little noise.
fn pattern(width: u32, height: u32) -> image::Rgb32FImage {
    let mut seed = 1u32;
    ImageBuffer::from_fn(width, height, |x, y| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 0.02;
        let lines = if (x / 3 + y / 7) % 5 == 0 { 0.3 } else { 0.0 };
        Rgb([
            (x as f32 / width as f32 * 0.7 + lines + noise).min(1.0),
            (y as f32 / height as f32 * 0.6 + noise).min(1.0),
            (0.4 + ((x ^ y) % 31) as f32 / 120.0 + noise).min(1.0),
        ])
    })
}

#[test]
#[ignore = "needs the Super Resolution model (SUPERRES_MODELS)"]
fn enlarges_24_mp_and_tiles_join() {
    let Some(dir) = models() else {
        eprintln!("SKIP: SUPERRES_MODELS not set");
        return;
    };
    let load = Instant::now();
    let model = Model::from_dir(&dir).expect("model");
    println!(
        "loaded on {} in {:.2} s",
        model.backend_name(),
        load.elapsed().as_secs_f64()
    );

    // One whole tile against the same pixels enlarged through the tiler:
    // the centre the tiler keeps must be the model's own result.
    let source = pattern(TILE as u32, TILE as u32);
    let mut whole = vec![0.0f32; 3 * TILE * TILE];
    for (x, y, p) in source.enumerate_pixels() {
        for c in 0..3 {
            whole[c * TILE * TILE + y as usize * TILE + x as usize] = p[c];
        }
    }
    let first = Instant::now();
    let reference = model.enlarge_tile(&whole).expect("tile");
    println!("first tile {:.3} s", first.elapsed().as_secs_f64());
    let tiled = model
        .enlarge(DynamicImage::ImageRgb32F(source), &|| Ok(()))
        .expect("enlarge")
        .into_rgb32f();
    let out = TILE * SCALE;
    let mut worst = 0.0f32;
    // Away from the borders (where the tiler repeats edge pixels and the
    // whole tile sees the image edge too, both agree anyway).
    let inner = (MARGIN * SCALE * 2)..(out - MARGIN * SCALE * 2);
    for y in inner.clone() {
        for x in inner.clone() {
            for c in 0..3 {
                let a = reference[c * out * out + y * out + x];
                let b = tiled.get_pixel(x as u32, y as u32)[c];
                worst = worst.max((a - b).abs());
            }
        }
    }
    println!("tiled vs whole tile: max |difference| {worst:.5}");
    assert!(worst < 0.01, "tiles do not join: {worst}");

    let image = DynamicImage::ImageRgb32F(pattern(6000, 4000));
    let start = Instant::now();
    let mut in_model = std::time::Duration::ZERO;
    let mut tiles = 0;
    let enlarged = rawtonic_superres::enlarge_with(
        image,
        &mut |tile| {
            let t = Instant::now();
            let out = model.enlarge_tile(tile);
            in_model += t.elapsed();
            tiles += 1;
            out
        },
        &|| Ok(()),
    )
    .expect("enlarge");
    println!(
        "24 MP -> {}x{} in {:.2} s ({tiles} tiles, {:.2} s in the model)",
        enlarged.width(),
        enlarged.height(),
        start.elapsed().as_secs_f64(),
        in_model.as_secs_f64()
    );
    let start = Instant::now();
    let image =
        DynamicImage::ImageRgb16(DynamicImage::ImageRgb32F(pattern(6000, 4000)).into_rgb16());
    let enlarged = model.enlarge(image, &|| Ok(())).expect("enlarge");
    assert!(matches!(enlarged, DynamicImage::ImageRgb16(_)));
    println!("24 MP 16-bit in {:.2} s", start.elapsed().as_secs_f64());
}
