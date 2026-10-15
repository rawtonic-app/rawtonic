//! Auto white balance (Lightroom's and Capture One's Auto).
//!
//! Fitted to Camera Raw's Auto on one sample frame each of 59 cameras
//! (notes: reference/white-balance.md): the mean of the linear decode (grey
//! world) gives a neutral; its temperature moves the camera's As Shot one
//! by 1.5 times their difference in mireds, 15 mireds cooler-lit, and the
//! tint is grey world's. Leave-one-out against Camera Raw's Auto: 22.8
//! mireds mean (As Shot 35.0); Camera Raw stops at 7500 K, which counts as
//! "at least" there.

use image::DynamicImage;

const GAIN: f64 = 1.5;
const OFFSET_MIRED: f64 = -15.0;
const RANGE_K: (f64, f64) = (2000.0, 12000.0);

/// Grey world of a linear decode at the camera's balance. Deliberately the
/// image crate's thumbnail, which adds 0.5 to float values
/// (`image_processing::thumbnail`): GAIN and OFFSET_MIRED were fitted on
/// those values (`dump_linear_decodes`). Correcting it means refitting both
/// on the 59-camera set (notes: reference/white-balance.md, PARITY_GAPS G01).
fn grey_world(decode: &DynamicImage) -> Option<[f32; 3]> {
    let rgb = decode.thumbnail(512, 512).to_rgb32f();
    let mut sum = [0f64; 3];
    for p in rgb.pixels() {
        for (total, v) in sum.iter_mut().zip(p.0) {
            *total += v.max(0.0) as f64;
        }
    }
    let n = (rgb.width() * rgb.height()) as f64;
    (n > 0.0 && sum.iter().all(|v| *v > 0.0)).then(|| sum.map(|v| (v / n) as f32))
}

/// Kelvin and tint of the Auto balance for the RAW at `path`, from its
/// linear decode.
pub fn auto_white_balance(path: &str, decode: &DynamicImage) -> Option<(f64, f64)> {
    let (k_world, tint) =
        crate::camera_profile::white_balance_for_sample(path, grey_world(decode)?)?;
    let (k_shot, _) = crate::camera_profile::as_shot_white_balance(path)?;
    let mired = 1e6 / k_shot + GAIN * (1e6 / k_world - 1e6 / k_shot) + OFFSET_MIRED;
    let kelvin = (1e6 / mired.max(1e6 / RANGE_K.1)).clamp(RANGE_K.0, RANGE_K.1);
    Some((kelvin, tint.clamp(-150.0, 150.0)))
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
    /// Writes the linear decode (as-shot balance, sRGB primaries) of each
    /// RAW in `$AWB_FILES` (`;`-separated) to `$AWB_OUT/<index>.bin`:
    /// width and height as u32, then f32 RGB, long edge ≤ 384 px.
    #[test]
    #[ignore = "tool: needs AWB_FILES and AWB_OUT"]
    fn dump_linear_decodes() {
        let out = std::path::PathBuf::from(std::env::var("AWB_OUT").unwrap());
        for (i, path) in std::env::var("AWB_FILES").unwrap().split(';').enumerate() {
            let bytes = std::fs::read(path).unwrap();
            let Ok(image) =
                crate::raw_processing::develop_raw_image(&bytes, true, 2.5, "auto".into(), None)
            else {
                println!("{i}: decode failed");
                continue;
            };
            // The values Auto was fitted on (see `grey_world`).
            let small = image.thumbnail(384, 384).to_rgb32f();
            let mut data = Vec::new();
            data.extend_from_slice(&small.width().to_le_bytes());
            data.extend_from_slice(&small.height().to_le_bytes());
            for v in small.as_raw() {
                data.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(out.join(format!("{i:03}.bin")), data).unwrap();
        }
    }

    /// Prints the Auto balance of each RAW in `$AWB_FILES`.
    #[test]
    #[ignore = "tool: needs AWB_FILES"]
    fn print_auto_white_balance() {
        for path in std::env::var("AWB_FILES").unwrap().split(';') {
            let bytes = std::fs::read(path).unwrap();
            let decode =
                crate::raw_processing::develop_raw_image(&bytes, true, 2.5, "auto".into(), None)
                    .ok();
            println!(
                "{:?}",
                decode.and_then(|d| super::auto_white_balance(path, &d))
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
