//! Auto for the black & white mix (Lightroom's Auto; Capture One has none).
//!
//! Camera Raw's Auto mixes span essentially two directions around one
//! typical mix (three principal components of 59 cameras' mixes leave
//! integer rounding): a warm/cool axis (red–yellow against aqua–purple) and
//! a common lift. On 73 photos the best point of that plane is 0.015
//! Oklab L × 100 from Lightroom's own Auto mix on average (at most 0.12),
//! so Auto only has to pick the point. It takes one step from the typical mix along the slopes of two
//! contrast measures of the grey image: the log of the standard deviation
//! of its linear luminance (Lightroom lifts the hues that fill and light the
//! frame: a tungsten-orange room gets Red and Orange raised, a sky Blue) and
//! the 5–95 % range of its Oklab lightness. The step is a fixed linear map
//! of the four slopes, fitted to Lightroom's Auto on 73 photos; on held-out
//! halves the grey differs from Lightroom's by 0.16 and 0.21 Oklab L × 100
//! on average, where the earlier range-maximising Auto gave 0.29 and 0.55,
//! and no photo got worse by more than 0.2 (notes:
//! benchmark-results/bwa-bw-auto-2026-11-03/REPORT.md).

/// A B&W mix response: channel windows and gains as the shader renders
/// them (`apply_black_and_white`, `apply_black_and_white_v2` in shader.wgsl,
/// checked by a test). A channel's gain grows with the slider by `*_q`.
struct Response {
    centres: [f32; 8],
    reach: [f32; 8],
    raise: [f32; 8],
    raise_q: [f32; 8],
    lower: [f32; 8],
    lower_q: [f32; 8],
}

/// `bwVersion` 1.
const V1: Response = Response {
    centres: [18.6, 55.2, 94.3, 134.9, 181.9, 236.0, 312.1, 348.0],
    reach: [33.9, 41.8, 38.7, 41.4, 42.1, 84.8, 60.6, 31.1],
    raise: [1.986, 3.3, 2.106, 2.203, 2.27, 4.216, 2.117, 1.248],
    raise_q: [0.0; 8],
    lower: [3.117, 5.741, 7.863, 4.676, 2.995, 6.053, 2.099, 1.677],
    lower_q: [0.0; 8],
};

/// `bwVersion` 2 (rawtonic_engine::image_processing::BW_VERSION).
const V2: Response = Response {
    centres: [17.8, 52.4, 93.3, 135.0, 182.9, 247.0, 310.9, 350.5],
    reach: [28.6, 39.4, 41.9, 38.1, 45.4, 77.4, 51.7, 25.3],
    raise: [2.386, 3.153, 3.062, 2.108, 1.883, 3.223, 2.463, 1.852],
    raise_q: [-0.53, -0.326, 0.446, -0.096, 0.184, 0.378, -0.083, 0.07],
    lower: [2.512, 4.448, 3.21, 2.751, 2.096, 3.468, 2.206, 1.747],
    lower_q: [1.442, 2.665, 5.972, 2.618, 2.0, 3.241, 1.35, 1.056],
};

fn response(version: u64) -> &'static Response {
    if version >= 2 { &V2 } else { &V1 }
}
const BASE: [f32; 24] = [
    -0.071, -0.13, -0.194, -0.144, -0.058, -0.006, 0.021, 0.004, 0.061, 0.115, 0.209, 0.238, 0.133,
    0.045, 0.031, 0.033, -0.042, 0.068, 0.169, 0.158, 0.153, 0.058, -0.119, -0.129,
];
const CHROMA_POWER: f32 = 1.077;
const MIN_LIGHTNESS: f32 = 0.075;

/// Camera Raw's typical Auto mix (median of 59 frames), slider units.
const TYPICAL: [f32; 8] = [-10.0, -19.0, -23.0, -27.0, -18.0, 10.0, 16.0, 4.0];
/// Warm/cool axis (positive raises blues) and common lift of the Auto mixes.
const AXIS_COOL: [f32; 8] = [
    -0.3655, -0.4103, -0.4314, -0.2608, 0.1751, 0.5322, 0.3590, -0.0338,
];
const AXIS_LIFT: [f32; 8] = [
    0.4508, 0.3049, 0.2391, 0.1188, 0.1101, 0.4719, 0.4676, 0.4232,
];
/// Half width of the central differences that measure the slopes, in steps
/// along the axes.
const SLOPE_SPAN: f32 = 10.0;
/// Step (cool, lift) per unit slope of, in this order: ln σ of the linear
/// grey luminance along cool and along lift, the 5–95 % Oklab grey range
/// along cool and along lift. Ridge fit (λ 0.03, no intercept: a grey image
/// keeps the typical mix) to Lightroom's Auto on 73 photos.
const STEP: [[f32; 2]; 4] = [
    [3430.4, 511.8],
    [-1621.0, 1832.7],
    [3437.4, 1558.7],
    [1833.5, 1416.5],
];
/// The steps measured on Lightroom's Auto mixes stay within these bounds.
const MAX_COOL: f32 = 60.0;
const MAX_LIFT: f32 = 30.0;
/// Floor of the luminance spread before the log (a flat image).
const MIN_SPREAD: f64 = 1e-4;
const MAX_SAMPLES: usize = 40_000;
const BINS: usize = 2048;

struct Sample {
    lightness: f32,
    neutral: f32,
    strength: f32,
    weights: [f32; 8],
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b)
        .max(0.0)
        .cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b)
        .max(0.0)
        .cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b)
        .max(0.0)
        .cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

impl Sample {
    fn new(display: [f32; 3], response: &Response) -> Self {
        let lab = oklab(display.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
        let chroma = lab[1].hypot(lab[2]);
        let hue = (lab[2].atan2(lab[1]).to_degrees() + 360.0) % 360.0;
        let position = hue / 15.0;
        let index = position as usize % 24;
        let neutral =
            BASE[index] + (BASE[(index + 1) % 24] - BASE[index]) * (position - position.floor());
        let weights = std::array::from_fn(|i| {
            let distance = (((hue - response.centres[i]) % 360.0 + 540.0) % 360.0 - 180.0).abs();
            0.5 + 0.5
                * ((distance / response.reach[i]).clamp(0.0, 1.0) * std::f32::consts::PI).cos()
        });
        Self {
            lightness: lab[0],
            neutral,
            strength: chroma.powf(CHROMA_POWER) / lab[0].max(MIN_LIGHTNESS),
            weights,
        }
    }

    /// Oklab lightness of the grey for a mix in slider units, as
    /// `apply_black_and_white(_v2)` in shader.wgsl computes it.
    fn grey(&self, mix: &[f32; 8], response: &Response) -> f32 {
        let mut k = self.neutral;
        for (i, slider) in mix.iter().enumerate() {
            let m = slider / 100.0;
            if m != 0.0 {
                let gain = if m > 0.0 {
                    response.raise[i] + response.raise_q[i] * m.abs()
                } else {
                    response.lower[i] + response.lower_q[i] * m.abs()
                };
                k += self.weights[i] * m * gain;
            }
        }
        (self.lightness * (k * self.strength).exp()).clamp(0.0, 1.0)
    }
}

/// Contrast of the grey image for a mix: ln σ of its linear luminance
/// (Oklab L³) and the 5–95 % range of its Oklab lightness.
fn contrast(samples: &[Sample], mix: &[f32; 8], response: &Response) -> [f32; 2] {
    let mut histogram = [0u32; BINS];
    let (mut sum, mut sum_sq) = (0.0f64, 0.0f64);
    for sample in samples {
        let grey = sample.grey(mix, response);
        let bin = (grey * (BINS - 1) as f32).round() as usize;
        histogram[bin.min(BINS - 1)] += 1;
        let luminance = f64::from(grey).powi(3);
        sum += luminance;
        sum_sq += luminance * luminance;
    }
    let n = samples.len() as f64;
    let variance = (sum_sq / n - (sum / n).powi(2)).max(0.0);
    let percentile = |p: f32| {
        let target = (p * samples.len() as f32).ceil().max(1.0) as u32;
        let mut seen = 0;
        for (bin, count) in histogram.iter().enumerate() {
            seen += count;
            if seen >= target {
                return bin as f32 / (BINS - 1) as f32;
            }
        }
        1.0
    };
    [
        variance.sqrt().max(MIN_SPREAD).ln() as f32,
        percentile(0.95) - percentile(0.05),
    ]
}

fn mix_at(cool: f32, lift: f32) -> [f32; 8] {
    std::array::from_fn(|i| TYPICAL[i] + cool * AXIS_COOL[i] + lift * AXIS_LIFT[i])
}

/// The Auto mix (slider units, red to magenta) for a developed colour image
/// in display sRGB, rendered with the B&W mix response `version`
/// (`bwVersion`), `None` without usable pixels.
pub fn auto_mix(pixels: &[[f32; 3]], version: u64) -> Option<[i32; 8]> {
    if pixels.is_empty() {
        return None;
    }
    let response = response(version);
    let stride = pixels.len().div_ceil(MAX_SAMPLES);
    let samples: Vec<Sample> = pixels
        .iter()
        .step_by(stride)
        .map(|p| Sample::new(*p, response))
        .collect();
    let at = |cool: f32, lift: f32| contrast(&samples, &mix_at(cool, lift), response);
    let slope =
        |plus: [f32; 2], minus: [f32; 2]| [0, 1].map(|i| (plus[i] - minus[i]) / (2.0 * SLOPE_SPAN));
    let along_cool = slope(at(SLOPE_SPAN, 0.0), at(-SLOPE_SPAN, 0.0));
    let along_lift = slope(at(0.0, SLOPE_SPAN), at(0.0, -SLOPE_SPAN));
    let slopes = [along_cool[0], along_lift[0], along_cool[1], along_lift[1]];
    let step = |axis: usize| -> f32 { (0..4).map(|i| slopes[i] * STEP[i][axis]).sum() };
    let cool = step(0).clamp(-MAX_COOL, MAX_COOL);
    let lift = step(1).clamp(-MAX_LIFT, MAX_LIFT);
    Some(mix_at(cool, lift).map(|v| v.round().clamp(-100.0, 100.0) as i32))
}

/// Auto for the photo `path` with its current recipe: the cached preview
/// base developed in colour (B&W off, layers left out: the mix is global)
/// and measured with [`auto_mix`]. `None` while no preview of that photo is
/// ready.
#[tauri::command]
pub async fn auto_gray_mix(
    path: String,
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, crate::app_state::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Option<[i32; 8]>, String> {
    use tauri::Manager;
    let context = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<crate::app_state::AppState>();
        let Some((source, is_raw, measures)) = state
            .original_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|image| image.path == path)
            .map(|image| {
                (
                    std::sync::Arc::clone(&image.image),
                    image.is_raw,
                    image.measures,
                )
            })
        else {
            return Ok(None);
        };
        let Some((base, transform_hash, scale)) = state
            .cached_preview
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|preview| preview.source.ptr_eq(&std::sync::Arc::downgrade(&source)))
            .map(|preview| {
                (
                    std::sync::Arc::clone(&preview.image),
                    preview.transform_hash,
                    preview.scale,
                )
            })
        else {
            return Ok(None);
        };
        let mut recipe = js_adjustments;
        let version = crate::image_processing::bw_version(&recipe);
        crate::adjustment_utils::hydrate_adjustments(&state, &mut recipe);
        recipe["blackAndWhite"] = serde_json::Value::Bool(false);
        recipe["masks"] = serde_json::json!([]);
        let tone_mapper =
            crate::image_processing::resolve_tonemapper_override_from_handle(&app_handle, is_raw);
        let mut adjustments =
            crate::image_processing::get_all_adjustments_from_json(&recipe, is_raw, tone_mapper);
        crate::baseline_exposure::apply_base_look(
            &mut adjustments.global,
            &path,
            measures.as_ref(),
        );
        adjustments.global.detail_scale = scale;
        adjustments.global.show_clipping = 0;
        let lut = recipe["lutPath"]
            .as_str()
            .and_then(|p| crate::lut_processing::get_or_load_lut(&state, p).ok());
        let developed = crate::gpu_processing::process_and_get_dynamic_image(
            &context,
            &state,
            &base,
            transform_hash,
            crate::gpu_processing::RenderRequest {
                adjustments,
                mask_bitmaps: &[],
                lut,
                roi: None,
            },
            "auto_gray_mix",
        )?;
        let pixels: Vec<[f32; 3]> = developed.to_rgb32f().pixels().map(|p| p.0).collect();
        Ok(auto_mix(&pixels, version))
    })
    .await
    .map_err(|e| format!("Auto black & white task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srgb(c: [u8; 3]) -> [f32; 3] {
        c.map(|v| v as f32 / 255.0)
    }

    fn image(parts: &[([u8; 3], usize)]) -> Vec<[f32; 3]> {
        parts
            .iter()
            .flat_map(|(c, n)| std::iter::repeat_n(srgb(*c), *n))
            .collect()
    }

    #[test]
    fn constants_match_the_shader() {
        let shader = include_str!("../../crates/gpu/src/shaders/shader.wgsl");
        let list = |name: &str| -> Vec<f32> {
            let start = shader.find(&format!("const {name} = ")).unwrap();
            let body = &shader[start..];
            let body = &body[body.find('(').unwrap() + 1..body.find(')').unwrap()];
            body.split(',').map(|v| v.trim().parse().unwrap()).collect()
        };
        let scalar = |name: &str| -> f32 {
            let start = shader.find(&format!("const {name}: f32 = ")).unwrap();
            let body = &shader[start..];
            body[body.find('=').unwrap() + 1..body.find(';').unwrap()]
                .trim()
                .parse()
                .unwrap()
        };
        assert_eq!(list("BW_CENTRES"), V1.centres);
        assert_eq!(list("BW_REACH"), V1.reach);
        assert_eq!(list("BW_RAISE"), V1.raise);
        assert_eq!(list("BW_LOWER"), V1.lower);
        assert_eq!(list("BW2_CENTRES"), V2.centres);
        assert_eq!(list("BW2_REACH"), V2.reach);
        assert_eq!(list("BW2_RAISE"), V2.raise);
        assert_eq!(list("BW2_RAISE_Q"), V2.raise_q);
        assert_eq!(list("BW2_LOWER"), V2.lower);
        assert_eq!(list("BW2_LOWER_Q"), V2.lower_q);
        assert_eq!(list("BW_BASE"), BASE);
        assert_eq!(scalar("BW_CHROMA_POWER"), CHROMA_POWER);
        assert_eq!(scalar("BW_MIN_LIGHTNESS"), MIN_LIGHTNESS);
    }

    /// Prints the Auto mix of each image (developed colour, display sRGB)
    /// in `$BW_AUTO_FILES` (`;`-separated).
    #[test]
    #[ignore = "tool: needs BW_AUTO_FILES"]
    fn print_auto_mix() {
        for path in std::env::var("BW_AUTO_FILES").unwrap().split(';') {
            let image = image::open(path).unwrap().to_rgb32f();
            let pixels: Vec<[f32; 3]> = image.pixels().map(|p| p.0).collect();
            println!("{path}\t{:?}", auto_mix(&pixels, 2).unwrap());
        }
    }

    #[test]
    fn grey_scenes_get_the_typical_mix() {
        let pixels = image(&[
            ([40, 40, 40], 500),
            ([128, 128, 128], 500),
            ([220, 220, 220], 500),
        ]);
        for version in [1, 2] {
            assert_eq!(
                auto_mix(&pixels, version).unwrap(),
                TYPICAL.map(|v| v as i32)
            );
            assert_eq!(auto_mix(&[], version), None);
        }
    }

    #[test]
    fn bright_blue_sky_over_dark_foliage_raises_blue_and_lowers_yellow() {
        let pixels = image(&[([110, 160, 225], 1000), ([150, 95, 35], 1000)]);
        for version in [1, 2] {
            let mix = auto_mix(&pixels, version).unwrap();
            assert!(mix[5] > TYPICAL[5] as i32 + 5, "{version} {mix:?}");
            assert!(mix[2] < TYPICAL[2] as i32 - 3, "{version} {mix:?}");
        }
    }

    /// A room lit by tungsten light: the cast fills the frame, so Auto lifts
    /// the warm hues instead of darkening them with the typical mix
    /// (Lightroom: Red +18, Orange +6 on such a frame).
    #[test]
    fn a_frame_filled_with_warm_light_raises_the_warm_hues() {
        let pixels = image(&[
            ([235, 160, 70], 700),
            ([180, 105, 40], 700),
            ([95, 55, 20], 600),
        ]);
        for version in [1, 2] {
            let mix = auto_mix(&pixels, version).unwrap();
            assert!(mix[0] > TYPICAL[0] as i32 + 10, "{version} {mix:?}");
            assert!(mix[1] > TYPICAL[1] as i32 + 10, "{version} {mix:?}");
        }
    }

    #[test]
    fn bright_orange_sky_over_a_dark_blue_foreground_raises_the_warm_hues() {
        let pixels = image(&[([245, 150, 70], 1000), ([30, 40, 70], 1000)]);
        for version in [1, 2] {
            let mix = auto_mix(&pixels, version).unwrap();
            assert!(mix[0] > TYPICAL[0] as i32 + 5, "{version} {mix:?}");
            assert!(mix[5] < TYPICAL[5] as i32, "{version} {mix:?}");
        }
    }
}
