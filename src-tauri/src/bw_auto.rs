//! Auto for the black & white mix (Lightroom's Auto; Capture One has none).
//!
//! Camera Raw's Auto mixes for 59 cameras' sample frames span essentially
//! two directions around one typical mix (three principal components leave
//! integer rounding): a warm/cool axis (red–yellow against aqua–purple) and
//! a common lift. Along the warm/cool axis the step follows the image: it
//! raises the hues of bright areas and lowers those of dark ones (blue sky
//! over foliage: Blue +27, Yellow −30; an orange sunset sky over a dark
//! foreground: Red +6), i.e. it widens the tonal range. Rawtonic therefore
//! picks the point of that plane that maximises the grey image's 5–95 %
//! range minus a penalty on the distance from the typical mix, evaluating
//! the mix exactly as the shader renders it. Against Camera Raw's own Auto
//! on held-out frames the grey differs by 0.21–0.38 Oklab L × 100 on
//! average, against 0.35–0.70 for the fixed typical mix
//! (notes: reference/black-and-white.md).

// `apply_black_and_white` in shader.wgsl (checked by a test).
const CENTRES: [f32; 8] = [18.6, 55.2, 94.3, 134.9, 181.9, 236.0, 312.1, 348.0];
const REACH: [f32; 8] = [33.9, 41.8, 38.7, 41.4, 42.1, 84.8, 60.6, 31.1];
const RAISE: [f32; 8] = [1.986, 3.3, 2.106, 2.203, 2.27, 4.216, 2.117, 1.248];
const LOWER: [f32; 8] = [3.117, 5.741, 7.863, 4.676, 2.995, 6.053, 2.099, 1.677];
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
/// Penalty per squared step against the gain in the 5–95 % grey range.
const PENALTY: f32 = 5.62e-5;
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
    fn new(display: [f32; 3]) -> Self {
        let lab = oklab(display.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
        let chroma = lab[1].hypot(lab[2]);
        let hue = (lab[2].atan2(lab[1]).to_degrees() + 360.0) % 360.0;
        let position = hue / 15.0;
        let index = position as usize % 24;
        let neutral =
            BASE[index] + (BASE[(index + 1) % 24] - BASE[index]) * (position - position.floor());
        let weights = std::array::from_fn(|i| {
            let distance = (((hue - CENTRES[i]) % 360.0 + 540.0) % 360.0 - 180.0).abs();
            0.5 + 0.5 * ((distance / REACH[i]).clamp(0.0, 1.0) * std::f32::consts::PI).cos()
        });
        Self {
            lightness: lab[0],
            neutral,
            strength: chroma.powf(CHROMA_POWER) / lab[0].max(MIN_LIGHTNESS),
            weights,
        }
    }

    /// Oklab lightness of the grey for a mix in slider units, as
    /// `apply_black_and_white` in shader.wgsl computes it.
    fn grey(&self, mix: &[f32; 8]) -> f32 {
        let mut k = self.neutral;
        for i in 0..8 {
            let m = mix[i] / 100.0;
            if m != 0.0 {
                k += self.weights[i] * m * if m > 0.0 { RAISE[i] } else { LOWER[i] };
            }
        }
        (self.lightness * (k * self.strength).exp()).clamp(0.0, 1.0)
    }
}

fn tonal_range(samples: &[Sample], mix: &[f32; 8]) -> f32 {
    let mut histogram = [0u32; BINS];
    for sample in samples {
        let bin = (sample.grey(mix) * (BINS - 1) as f32).round() as usize;
        histogram[bin.min(BINS - 1)] += 1;
    }
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
    percentile(0.95) - percentile(0.05)
}

fn mix_at(cool: f32, lift: f32) -> [f32; 8] {
    std::array::from_fn(|i| TYPICAL[i] + cool * AXIS_COOL[i] + lift * AXIS_LIFT[i])
}

/// The Auto mix (slider units, red to magenta) for a developed colour image
/// in display sRGB, `None` without usable pixels.
pub fn auto_mix(pixels: &[[f32; 3]]) -> Option<[i32; 8]> {
    if pixels.is_empty() {
        return None;
    }
    let stride = pixels.len().div_ceil(MAX_SAMPLES);
    let samples: Vec<Sample> = pixels
        .iter()
        .step_by(stride)
        .map(|p| Sample::new(*p))
        .collect();
    let score = |cool: f32, lift: f32| {
        tonal_range(&samples, &mix_at(cool, lift)) - PENALTY * (cool * cool + lift * lift)
    };
    // Coarse grid, then one-unit steps around the best point.
    let mut best = (f32::NEG_INFINITY, 0.0, 0.0);
    for cool in (-40..=40).step_by(5) {
        for lift in (-20..=20).step_by(5) {
            let (cool, lift) = (cool as f32, lift as f32);
            let value = score(cool, lift);
            if value > best.0 {
                best = (value, cool, lift);
            }
        }
    }
    let (_, centre_cool, centre_lift) = best;
    for dc in -5..=5 {
        for dl in -5..=5 {
            let (cool, lift) = (centre_cool + dc as f32, centre_lift + dl as f32);
            let value = score(cool, lift);
            if value > best.0 {
                best = (value, cool, lift);
            }
        }
    }
    Some(mix_at(best.1, best.2).map(|v| v.round().clamp(-100.0, 100.0) as i32))
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
        Ok(auto_mix(&pixels))
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
            let start = shader.find(&format!("const {name}")).unwrap();
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
        assert_eq!(list("BW_CENTRES"), CENTRES);
        assert_eq!(list("BW_REACH"), REACH);
        assert_eq!(list("BW_RAISE"), RAISE);
        assert_eq!(list("BW_LOWER"), LOWER);
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
            println!("{path}\t{:?}", auto_mix(&pixels).unwrap());
        }
    }

    #[test]
    fn grey_scenes_get_the_typical_mix() {
        let pixels = image(&[
            ([40, 40, 40], 500),
            ([128, 128, 128], 500),
            ([220, 220, 220], 500),
        ]);
        assert_eq!(auto_mix(&pixels).unwrap(), TYPICAL.map(|v| v as i32));
        assert_eq!(auto_mix(&[]), None);
    }

    #[test]
    fn bright_blue_sky_over_dark_foliage_raises_blue_and_lowers_yellow() {
        let pixels = image(&[([110, 160, 225], 1000), ([150, 95, 35], 1000)]);
        let mix = auto_mix(&pixels).unwrap();
        assert!(mix[5] > TYPICAL[5] as i32 + 5, "{mix:?}");
        assert!(mix[2] < TYPICAL[2] as i32 - 3, "{mix:?}");
    }

    #[test]
    fn bright_orange_sky_over_a_dark_blue_foreground_raises_the_warm_hues() {
        let pixels = image(&[([245, 150, 70], 1000), ([30, 40, 70], 1000)]);
        let mix = auto_mix(&pixels).unwrap();
        assert!(mix[0] > TYPICAL[0] as i32 + 5, "{mix:?}");
        assert!(mix[5] < TYPICAL[5] as i32, "{mix:?}");
    }
}
