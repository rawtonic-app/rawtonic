// SPDX-License-Identifier: AGPL-3.0-or-later
//! One requested scene analysis; light edits use the persisted geometry only.
use super::*;
use rawtonic_engine::relight::Camera;

const FILE: &str = "relight_geometry_da3_base.onnx";
const SHA256: &str = "070a4ff34ed406d057044daf9b84fd6287e5a7cc66134060acf2ccfe5d5dea23";
type DepthImage = image::ImageBuffer<image::Luma<u16>, Vec<u16>>;

pub async fn get_or_init_relight_model(
    app: &tauri::AppHandle,
    state: &Mutex<Option<AiState>>,
    init: &TokioMutex<()>,
) -> Result<Arc<rawtonic_onnx::Model>> {
    ensure_local_runtime()?;
    let loaded = || {
        state
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.relight_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let _guard = init.lock().await;
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let dir = get_models_dir(app)?;
    download_and_verify_model(app, &dir, FILE, FILE, SHA256, "Relight geometry").await?;
    let path = dir.join(FILE);
    let model = tokio::task::spawn_blocking(move || {
        rawtonic_onnx::Model::load(&path, rawtonic_onnx::ModelKind::Relight)
    })
    .await??;
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(state, generation, |ai| {
        ai.relight_model = Some(model.clone())
    });
    Ok(model)
}

fn size(width: u32, height: u32) -> (u32, u32) {
    let scale = 518. / width.max(height).max(1) as f64;
    // A single patch along an axis makes the upstream camera head singular.
    let side = |n| ((n as f64 * scale / 14.).round() as u32).clamp(2, 37) * 14;
    (side(width), side(height))
}

fn encode(w: u32, h: u32, values: &[f32], intrinsics: &[f32]) -> Result<(DepthImage, Camera)> {
    anyhow::ensure!(
        values.len() == (w * h) as usize && intrinsics.len() == 9,
        "Invalid Relight geometry dimensions"
    );
    rawtonic_onnx::ensure_finite(values)?;
    rawtonic_onnx::ensure_finite(intrinsics)?;
    anyhow::ensure!(values.iter().all(|v| *v > 0.), "Invalid Relight distance");
    let mut ordered = values.to_vec();
    let middle = ordered.len() / 2;
    ordered.select_nth_unstable_by(middle, f32::total_cmp);
    let reference = ordered[middle];
    // Monocular distance is relative. Normalize once, retaining ratios, and
    // bound extreme sky/outlier distances before reciprocal GPU projection.
    let inverse: Vec<_> = values
        .iter()
        .map(|d| (reference / d).clamp(0.0001, 100.))
        .collect();
    let lo = inverse.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = inverse.iter().copied().fold(0., f32::max);
    let camera = Camera {
        fx: intrinsics[0] / w as f32,
        fy: intrinsics[4] / h as f32,
        cx: intrinsics[2] / w as f32,
        cy: intrinsics[5] / h as f32,
        inverse_min: lo,
        inverse_max: hi,
        reference: 1.,
    };
    anyhow::ensure!(
        Camera::from_recipe(&serde_json::json!({"relightDepth":{"version":4,"camera":camera}}))
            .is_some(),
        "Invalid Relight camera"
    );
    let range = hi - lo;
    let codes = inverse
        .iter()
        .map(|q| {
            if range > 0. {
                ((q - lo) / range * 65535.).round() as u16
            } else {
                0
            }
        })
        .collect();
    Ok((
        image::ImageBuffer::from_raw(w, h, codes)
            .ok_or_else(|| anyhow::anyhow!("Invalid Relight dimensions"))?,
        camera,
    ))
}

pub fn run_relight_model(
    image: &DynamicImage,
    model: &rawtonic_onnx::Model,
    linear: bool,
) -> Result<(DepthImage, Camera)> {
    let (w, h) = size(image.width(), image.height());
    let rgb = display_rgb8(image.resize_exact(w, h, FilterType::CatmullRom), linear);
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    let input =
        ndarray::Array5::from_shape_fn((1, 1, 3, h as usize, w as usize), |(_, _, c, y, x)| {
            (rgb.get_pixel(x as u32, y as u32)[c] as f32 / 255. - mean[c]) / std[c]
        });
    model.run(|session| {
        let output = session.run(ort::inputs!["image" => Tensor::from_array(input.clone())?])?;
        let (shape, values) = output["depth"].try_extract_tensor::<f32>()?;
        anyhow::ensure!(
            shape.as_ref() == [1, 1, h as i64, w as i64],
            "Invalid Relight output shape"
        );
        let (_, k) = output["intrinsics"].try_extract_tensor::<f32>()?;
        encode(w, h, values, k)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distance_ratios_and_camera_survive_the_recipe() {
        let (image, c) = encode(
            4,
            2,
            &[1., 2., 3., 4., 2., 3., 4., 5.],
            &[6., 0., 2., 0., 6., 1., 0., 0., 1.],
        )
        .unwrap();
        for (i, &d) in [1., 2., 3., 4., 2., 3., 4., 5.].iter().enumerate() {
            let decoded = c.inverse(image.as_raw()[i]).recip();
            assert!((decoded - d / 3.).abs() < 0.0001);
        }
        assert_eq!(c.fx, 1.5);
        assert_eq!(c.fy, 3.);
        assert_eq!(c.cx, 0.5);
        assert_eq!(c.cy, 0.5);
        let flat = encode(2, 2, &[4.; 4], &[6., 0., 1., 0., 6., 1., 0., 0., 1.]).unwrap();
        assert!(flat.0.as_raw().iter().all(|v| flat.1.inverse(*v) == 1.));
    }
    #[test]
    fn invalid_model_geometry_is_rejected() {
        for values in [vec![], vec![0.; 4], vec![f32::NAN; 4], vec![-1.; 4]] {
            assert!(encode(2, 2, &values, &[3., 0., 1., 0., 3., 1., 0., 0., 1.]).is_err());
        }
        assert!(encode(2, 2, &[1.; 4], &[0.; 9]).is_err());
        assert_eq!(size(6000, 4000), (518, 350));
        assert_eq!(size(4000, 6000), (350, 518));
        assert_eq!(size(20000, 1), (518, 28));
    }
    #[test]
    #[ignore = "requires RELIGHT_GEOMETRY_MODEL and ORT_DYLIB_PATH"]
    fn native_geometry_handles_portrait_landscape_and_narrow_frames() {
        let path = std::env::var("RELIGHT_GEOMETRY_MODEL").expect("RELIGHT_GEOMETRY_MODEL");
        let model = rawtonic_onnx::Model::load_on(
            Path::new(&path),
            rawtonic_onnx::ModelKind::Relight,
            &[rawtonic_onnx::Provider::Cpu],
            &[],
        )
        .unwrap();
        for (w, h) in [
            (518, 350),
            (350, 518),
            (518, 518),
            (518, 140),
            (518, 14),
            (14, 518),
        ] {
            let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([(255 * x / w) as u8, (255 * y / h) as u8, 100])
            }));
            let start = std::time::Instant::now();
            let (depth, camera) = run_relight_model(&image, &model, false).unwrap();
            assert_eq!(depth.dimensions(), size(w, h));
            assert!(
                depth
                    .as_raw()
                    .iter()
                    .all(|v| camera.inverse(*v).is_finite())
            );
            eprintln!("Native Relight {w}x{h}: {} ms", start.elapsed().as_millis());
        }
    }
}
