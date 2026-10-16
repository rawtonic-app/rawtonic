//! Auto white balance (Lightroom's and Capture One's Auto).
//!
//! Matched to Camera Raw's Auto on one frame each of 232 in-scope cameras
//! (notes: reference/white-balance.md): the mean of the linear decode
//! without near-clipped pixels (grey world) gives a neutral, its
//! temperature 20 mireds warmer is Auto's, its tint is Auto's; Camera
//! Raw's limits (2850-7500 K, tint 0 to +30) keep scenes of one dominant
//! colour from swinging far. Against Camera Raw's Auto: median 0.5 mireds
//! (As Shot 24), 208 of 232 within 3 mireds, tint median 0.

use image::DynamicImage;

const OFFSET_MIRED: f64 = -20.0;
const RANGE_K: (f64, f64) = (2850.0, 7500.0);
const RANGE_TINT: (f64, f64) = (0.0, 30.0);
/// Pixels with a channel at or above this are left out of grey world.
const CLIPPED: f32 = 0.9;

/// Grey world of a linear decode at the camera's balance, without
/// near-clipped pixels.
fn grey_world(decode: &DynamicImage) -> Option<[f32; 3]> {
    let rgb = rawtonic_engine::image_processing::thumbnail(decode, 512, 512).to_rgb32f();
    let mut sum = [0f64; 3];
    let mut n = 0f64;
    for p in rgb.pixels().filter(|p| p.0.iter().all(|v| *v < CLIPPED)) {
        for (total, v) in sum.iter_mut().zip(p.0) {
            *total += v.max(0.0) as f64;
        }
        n += 1.0;
    }
    (n > 0.0 && sum.iter().all(|v| *v > 0.0)).then(|| sum.map(|v| (v / n) as f32))
}

/// Kelvin and tint of the Auto balance for the RAW at `path`, from its
/// linear decode.
pub fn auto_white_balance(path: &str, decode: &DynamicImage) -> Option<(f64, f64)> {
    let (k_world, tint) =
        crate::camera_profile::white_balance_for_sample(path, grey_world(decode)?)?;
    // A mean bluer than the offset can reach (negative mireds) gives the
    // upper limit; Camera Raw wraps such frames to its lower one.
    let mired = (1e6 / k_world + OFFSET_MIRED).max(1e6 / RANGE_K.1);
    let kelvin = (1e6 / mired).clamp(RANGE_K.0, RANGE_K.1);
    Some((kelvin, tint.clamp(RANGE_TINT.0, RANGE_TINT.1)))
}

/// Auto for the open photo, from its cached preview base; `None` while no
/// current preview exists or for files without a camera calibration.
#[tauri::command]
pub async fn auto_white_balance_for(
    path: String,
    app_handle: tauri::AppHandle,
) -> Result<Option<crate::camera_profile::WhiteBalanceInfo>, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<crate::app_state::AppState>();
        let Some(source) = state
            .original_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|image| image.path == path && image.is_raw)
            .map(|image| std::sync::Arc::clone(&image.image))
        else {
            return Ok(None);
        };
        let base = state
            .cached_preview
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|preview| preview.source.ptr_eq(&std::sync::Arc::downgrade(&source)))
            .map(|preview| std::sync::Arc::clone(&preview.image));
        let source_path = crate::file_management::parse_virtual_path(&path)
            .0
            .to_string_lossy()
            .to_string();
        Ok(base
            .as_deref()
            .and_then(|image| auto_white_balance(&source_path, image))
            .map(
                |(temperature, tint)| crate::camera_profile::WhiteBalanceInfo {
                    temperature: temperature.round(),
                    tint: tint.round(),
                },
            ))
    })
    .await
    .map_err(|e| format!("Auto white balance failed: {e}"))?
}

#[cfg(test)]
mod tests {
    /// Grey world is the plain mean of the unclipped pixels.
    #[test]
    fn grey_world_leaves_out_clipped_pixels() {
        let mut rgb = image::Rgb32FImage::from_pixel(4, 4, image::Rgb([0.2, 0.4, 0.1]));
        rgb.put_pixel(0, 0, image::Rgb([1.5, 0.95, 0.3]));
        let mean = super::grey_world(&image::DynamicImage::ImageRgb32F(rgb)).unwrap();
        for (got, want) in mean.iter().zip([0.2, 0.4, 0.1]) {
            assert!((got - want).abs() < 1e-6, "{mean:?}");
        }
    }

    /// Prints one JSON line per RAW in `$AWB_FILES` (`;`-separated): As
    /// Shot, the grey world neutral and Auto as Kelvin and tint
    /// (notes: scripts/reference/awb_fit.py compares them with Camera Raw's).
    #[test]
    #[ignore = "tool: needs AWB_FILES"]
    fn print_auto_white_balance() {
        for path in std::env::var("AWB_FILES").unwrap().split(';') {
            let bytes = std::fs::read(path).unwrap();
            let Ok(decode) =
                crate::raw_processing::develop_raw_image(&bytes, true, 2.5, "auto".into(), None)
            else {
                println!("{{\"source\":{path:?},\"error\":\"decode\"}}");
                continue;
            };
            let grey = super::grey_world(&decode)
                .and_then(|rgb| crate::camera_profile::white_balance_for_sample(path, rgb));
            println!(
                "{{\"source\":{path:?},\"as_shot\":{:?},\"grey_world\":{grey:?},\"auto\":{:?}}}",
                crate::camera_profile::as_shot_white_balance(path),
                super::auto_white_balance(path, &decode),
            );
        }
    }

    /// Prints Kelvin and tint for neutral samples: `$AWB_SAMPLES` lines
    /// "path\tr g b" (linear decode values).
    #[test]
    #[ignore = "tool: needs AWB_SAMPLES"]
    fn print_kelvin_for_samples() {
        for line in std::fs::read_to_string(std::env::var("AWB_SAMPLES").unwrap())
            .unwrap()
            .lines()
        {
            let (path, rgb) = line.split_once('\t').unwrap();
            let v: Vec<f32> = rgb.split(' ').map(|x| x.parse().unwrap()).collect();
            let wb = crate::camera_profile::white_balance_for_sample(path, [v[0], v[1], v[2]]);
            println!("{:?}", wb);
        }
    }
}
