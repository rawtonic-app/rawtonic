//! Auto white balance (Lightroom's and Capture One's Auto).
//!
//! Finds the light (`rawtonic_engine::white_balance_estimate`, about twice
//! as accurate as Camera Raw's Auto on scenes with a measured grey; notes:
//! reference/white-balance.md) and corrects it the way a photographer
//! would: fully in ordinary daylight, only partly in warmer or cooler
//! light, so golden hour, sunsets and lamplight keep their warmth and shade
//! and blue hour some of their blue. Tint (green/magenta casts) is corrected within 0 to +30, the range
//! of real light.

use image::DynamicImage;

/// The estimate's small cool bias (3 mireds on scenes with a measured grey).
const OFFSET_MIRED: f64 = -3.0;
/// Daylight between these (D50 to D65) is corrected fully.
const FULL_K: (f64, f64) = (5000.0, 6500.0);
/// Share of the light's distance beyond those that is corrected: as much
/// warmth stays as with Camera Raw's Auto on average, but smoothly instead
/// of at a hard limit.
const BEYOND: f64 = 0.7;
const RANGE_K: (f64, f64) = (2000.0, 12000.0);
const RANGE_TINT: (f64, f64) = (0.0, 30.0);

/// Kelvin and tint of the Auto balance for the RAW at `path`, from its
/// linear decode.
pub fn auto_white_balance(path: &str, decode: &DynamicImage) -> Option<(f64, f64)> {
    let neutral = rawtonic_engine::white_balance_estimate::neutral(decode)?;
    let (kelvin, tint) = crate::camera_profile::white_balance_for_sample(path, neutral)?;
    to_auto(kelvin, tint)
}

/// The light's Kelvin and tint as Auto's setting.
fn to_auto(kelvin: f64, tint: f64) -> Option<(f64, f64)> {
    let mired = 1e6 / kelvin + OFFSET_MIRED;
    let (cool, warm) = (1e6 / FULL_K.1, 1e6 / FULL_K.0);
    let mired = if mired > warm {
        warm + (mired - warm) * BEYOND
    } else if mired < cool {
        cool - (cool - mired) * BEYOND
    } else {
        mired
    };
    let mired = mired.max(1e6 / RANGE_K.1);
    Some((
        (1e6 / mired).clamp(RANGE_K.0, RANGE_K.1),
        tint.clamp(RANGE_TINT.0, RANGE_TINT.1),
    ))
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
    use super::to_auto;

    fn mired(kelvin: f64) -> f64 {
        1e6 / kelvin
    }

    #[test]
    fn daylight_is_corrected_fully() {
        for k in [5100.0, 5500.0, 6300.0] {
            let (set, _) = to_auto(k, 10.0).unwrap();
            assert!((mired(set) - (mired(k) - 3.0)).abs() < 1e-9, "{k} -> {set}");
        }
    }

    #[test]
    fn warm_and_cool_light_keep_part_of_their_colour() {
        let (sunset, _) = to_auto(2200.0, 5.0).unwrap();
        assert!(sunset > 2400.0 && sunset < 3500.0, "{sunset}");
        let (shade, _) = to_auto(12000.0, 5.0).unwrap();
        assert!(shade > 6500.0 && shade < 10000.0, "{shade}");
    }

    #[test]
    fn no_jump_at_the_knees() {
        for knee in [5000.0, 6500.0] {
            let below = to_auto(1e6 / (mired(knee) + 3.0 - 0.01), 0.0).unwrap().0;
            let above = to_auto(1e6 / (mired(knee) + 3.0 + 0.01), 0.0).unwrap().0;
            assert!(
                (mired(below) - mired(above)).abs() < 0.05,
                "{knee}: {below} {above}"
            );
        }
    }

    #[test]
    fn tint_stays_in_the_range_of_real_light() {
        assert_eq!(to_auto(5000.0, -40.0).unwrap().1, 0.0);
        assert_eq!(to_auto(5000.0, 55.0).unwrap().1, 30.0);
    }

    /// Prints one JSON line per RAW in `$AWB_FILES` (`;`-separated): As
    /// Shot, the estimated neutral and Auto as Kelvin and tint
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
            // Optional region to leave out: `$AWB_EXCLUDE` lines
            // "path\tx0 y0 x1 y1" (0..1), e.g. a grey reference target.
            let exclude: Option<[f32; 4]> = std::env::var("AWB_EXCLUDE").ok().and_then(|f| {
                std::fs::read_to_string(f).ok()?.lines().find_map(|l| {
                    let (p, r) = l.split_once('\t')?;
                    let v: Vec<f32> = r.split(' ').filter_map(|x| x.parse().ok()).collect();
                    (p == path && v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
                })
            });
            let keep = |x: f32, y: f32| {
                exclude.is_none_or(|[x0, y0, x1, y1]| !(x >= x0 && x <= x1 && y >= y0 && y <= y1))
            };
            let neutral = rawtonic_engine::white_balance_estimate::neutral_where(&decode, keep);
            let grey =
                neutral.and_then(|rgb| crate::camera_profile::white_balance_for_sample(path, rgb));
            println!(
                "{{\"source\":{path:?},\"as_shot\":{:?},\"neutral\":{grey:?},\"auto\":{:?}}}",
                crate::camera_profile::as_shot_white_balance(path),
                grey.and_then(|(k, t)| super::to_auto(k, t)),
            );
        }
    }

    /// Writes, for each RAW in `$AWB_FILES`, `$AWB_OUT/<n>.bin`: width and
    /// height (u32), the area-average thumbnail (long edge 512) of the
    /// linear decode as f32 RGB, then the decode's camera_to_srgb (9 f32)
    /// and white-balance multipliers (3 f32), so estimators can be tried
    /// outside the app and their neutrals taken back to camera RGB.
    #[test]
    #[ignore = "tool: needs AWB_FILES and AWB_OUT"]
    fn dump_decode_thumbnails() {
        let out = std::path::PathBuf::from(std::env::var("AWB_OUT").unwrap());
        for (i, path) in std::env::var("AWB_FILES").unwrap().split(';').enumerate() {
            let bytes = std::fs::read(path).unwrap();
            let (Ok(decode), Some(colour)) = (
                crate::raw_processing::develop_raw_image(&bytes, true, 2.5, "auto".into(), None),
                crate::camera_profile::image_colour(path, None),
            ) else {
                println!("{i}: decode failed");
                continue;
            };
            let small = rawtonic_engine::image_processing::thumbnail(&decode, 512, 512).to_rgb32f();
            let mut data = Vec::new();
            data.extend_from_slice(&small.width().to_le_bytes());
            data.extend_from_slice(&small.height().to_le_bytes());
            let floats = small
                .as_raw()
                .iter()
                .chain(colour.camera_to_srgb.iter().flatten())
                .chain(&colour.wb[..3]);
            for v in floats {
                data.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(out.join(format!("{i:04}.bin")), data).unwrap();
        }
    }

    /// Prints the camera-RGB neutral (inverse multipliers, green 1) for
    /// `$AWB_KT` lines "path\tkelvin\ttint".
    #[test]
    #[ignore = "tool: needs AWB_KT"]
    fn print_camera_neutral_for_kelvin() {
        for line in std::fs::read_to_string(std::env::var("AWB_KT").unwrap())
            .unwrap()
            .lines()
        {
            let v: Vec<&str> = line.split('\t').collect();
            let neutral = crate::camera_profile::image_colour(v[0], None).and_then(|colour| {
                rawtonic_rawdecode::color_calibration::white_balance_multipliers(
                    &colour.calibrations,
                    colour.unit_calibration.as_ref(),
                    v[1].parse().unwrap(),
                    v[2].parse().unwrap(),
                    1.0,
                )
            });
            println!(
                "{:?}",
                neutral.map(|m| [1.0 / m[0], 1.0 / m[1], 1.0 / m[2]])
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
