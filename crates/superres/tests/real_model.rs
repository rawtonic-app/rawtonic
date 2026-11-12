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

/// The CPU conversion preserves the shipped four-rotation graph's output.
#[test]
#[ignore = "needs both installed ONNX models; run with RAWTONIC_AI_PROVIDER=cpu"]
fn sequential_cpu_matches_original_graph() {
    let dir = models().expect("SUPERRES_MODELS must name the installed models");
    assert!(dir.join(rawtonic_superres::CPU_ONNX_FILE).is_file());
    let original = rawtonic_onnx::Model::load_on(
        &dir.join(rawtonic_superres::ONNX_FILE),
        rawtonic_onnx::ModelKind::SuperResolution,
        &[rawtonic_onnx::Provider::Cpu],
        &[],
    )
    .unwrap();
    let sequential = Model::cpu(&dir.join(rawtonic_superres::CPU_ONNX_FILE)).unwrap();
    assert!(
        sequential.uses_cpu(),
        "force the CPU provider for this test"
    );
    let input = pattern(TILE as u32, TILE as u32);
    let tile: Vec<f32> = (0..3)
        .flat_map(|channel| input.pixels().map(move |p| p[channel]))
        .collect();
    let reference = original
        .run(|session| {
            let tensor = ort::value::Tensor::from_array(ndarray::Array4::from_shape_vec(
                (1, 3, TILE, TILE),
                tile.clone(),
            )?)?;
            let outputs = session.run(ort::inputs!["image" => tensor])?;
            Ok(outputs["enlarged"].try_extract_tensor::<f32>()?.1.to_vec())
        })
        .unwrap();
    let got = sequential.enlarge_tile(&tile).unwrap();
    assert_eq!(got.len(), reference.len());
    let worst = got
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!("sequential CPU vs original: max |difference| {worst:.8}; tolerance 0.00001");
    // Float reordering only: tighter than the existing tile-equivalence tolerance (0.01).
    assert!(worst < 1e-5, "CPU conversion changed the output: {worst}");
}

#[test]
#[ignore = "needs an old model pack (SUPERRES_OLD_MODELS); force RAWTONIC_AI_PROVIDER=cpu"]
fn cpu_legacy_pack_without_optional_conversion_still_runs() {
    let dir =
        std::path::PathBuf::from(std::env::var_os("SUPERRES_OLD_MODELS").expect("old pack path"));
    assert!(!dir.join(rawtonic_superres::CPU_ONNX_FILE).exists());
    let model = Model::onnx(&dir.join(rawtonic_superres::ONNX_FILE)).unwrap();
    assert!(model.uses_cpu());
    let output = model.enlarge_tile(&vec![0.0; 3 * TILE * TILE]).unwrap();
    assert_eq!(output.len(), 3 * (TILE * SCALE).pow(2));
    assert!(output.iter().all(|v| v.is_finite()));
}
