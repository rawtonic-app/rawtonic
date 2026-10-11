//! Auto adjust for the Standard base look, after Capture One's Auto.
//!
//! The prediction uses the tone curve the file actually renders with: the
//! Camera Raw curve (with its black ramp) or the file's own curve for RAWs
//! since the switch to the Lightroom-like look, the Standard curve otherwise.
//!
//! Capture One's Auto (exposure, contrast/brightness, HDR, levels; no white
//! balance) was run on 17 cameras. Whatever the source, its results converge
//! on one tonal distribution: luminance percentiles p1 / p5 / p25 / p50 /
//! p75 / p95 / p99 of −8.9 / −6.8 / −4.2 / −2.4 / −1.2 / −0.3 / −0.1 EV
//! (spread 0.1–0.8 EV; notes: reference/auto-adjust.md). This module predicts
//! the Standard rendering from the scene histogram (the curve is monotonic,
//! so display percentiles are the curve applied to scene percentiles) and
//! chooses Exposure for the midtones, Highlights or Whites for the top,
//! Shadows for the deep tones and a Levels black point (the luma curve's
//! black input, as Capture One's Auto uses Levels) for the black end.
//! Unlike the inherited auto it leaves creative controls (Clarity, Dehaze,
//! Vibrance, vignetting) alone, as Capture One does.
use crate::image_processing::{AutoAdjustmentResults, downscale_f32_image};
use image::DynamicImage;

const ANALYSIS_MAX_DIM: u32 = 1024;
/// Display EV below white that counts as blown.
const WHITE_MARGIN_EV: f32 = 0.05;
/// Above this blown share Auto does not raise Exposure.
const BLOWN_NO_BRIGHTEN_SHARE: f32 = 0.05;
/// Below this blown share (and without overshoot) Highlights stay at 0.
const BLOWN_MIN_SHARE: f32 = 0.01;
/// Highlights amount per blown share (25 % blown → −62, 32 % → −80).
const HIGHLIGHTS_PER_BLOWN_SHARE: f32 = 250.0;

/// Rawtonic's default tone curve: display EV every 0.25 scene EV from −12 EV
/// (`DEFAULT_TONE_CURVE` in shaders/shader.wgsl; a test rebuilds both from
/// `DEFAULT_TONE_CURVE_POINTS`, a monotone cubic through those points in
/// log-log; notes: reference/own-base-look.md).
const DEFAULT_TONE_CURVE_START_EV: f32 = -12.0;
const DEFAULT_TONE_CURVE: [f32; 49] = [
    -12.6791, -12.4256, -12.1734, -11.9225, -11.6728, -11.4243, -11.1769, -10.9307, -10.6855,
    -10.4413, -10.1982, -9.9560, -9.7147, -9.4743, -9.2347, -8.9960, -8.7580, -8.5290, -8.3113,
    -8.0961, -7.8747, -7.6383, -7.3780, -7.0836, -6.7571, -6.4083, -6.0468, -5.6825, -5.3250,
    -4.9658, -4.5939, -4.2154, -3.8364, -3.4631, -3.1015, -2.7578, -2.4380, -2.1334, -1.8352,
    -1.5494, -1.2821, -1.0392, -0.8270, -0.6413, -0.4743, -0.3264, -0.1980, -0.0893, 0.0000,
];
/// (scene EV, display EV) points of the default tone curve, white at 0 EV.
#[cfg(test)]
const DEFAULT_TONE_CURVE_POINTS: [(f32, f32); 6] = [
    (-8.0, -8.758),
    (-6.5, -7.378),
    (-5.0, -5.325),
    (-3.0, -2.438),
    (-1.5, -0.827),
    (-0.5, -0.198),
];
const CAMERA_RAW_BLACK: f32 = 0.005;
const CAMERA_RAW_BLACK_NEUTRAL: f32 = 0.006;
/// Highlight roll-off of the neutral look (shader `CR_ROLLOFF_*`).
const CR_ROLLOFF_KNEE: f32 = 0.55;
const CR_ROLLOFF_LIMIT: f32 = 1.02;

/// The base tone curve a file renders with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayCurve {
    /// The default curve without a base look (no baseline from the file).
    Standard,
    /// The default tone curve; `neutral` selects the black ramp of the
    /// neutral-colour look.
    CameraRaw { neutral: bool },
    /// A file's own curve (display EV every 0.25 scene EV from −12 EV);
    /// `black` is false for DNGs with DefaultBlackRender = None.
    Profile { curve: [f32; 49], black: bool },
}

impl DisplayCurve {
    pub fn for_look(look: &crate::baseline_exposure::BaseLook) -> Self {
        match (look.camera_raw, look.profile_curve) {
            (true, Some(curve)) => Self::Profile {
                curve,
                black: !look.no_black_render,
            },
            (true, None) => Self::CameraRaw {
                neutral: look.neutral_colour,
            },
            _ => Self::Standard,
        }
    }

    /// Display EV for a scene EV (as the shader's tone curves).
    fn display_ev(&self, scene_ev: f32) -> f32 {
        let (table, black) = match self {
            Self::Standard => return standard_display_ev(scene_ev),
            Self::CameraRaw { neutral: false } => (&DEFAULT_TONE_CURVE, CAMERA_RAW_BLACK),
            Self::CameraRaw { neutral: true } => (&DEFAULT_TONE_CURVE, CAMERA_RAW_BLACK_NEUTRAL),
            Self::Profile { curve, black } => (curve, if *black { CAMERA_RAW_BLACK } else { 0.0 }),
        };
        let x = camera_raw_black_ramp(2f32.powf(scene_ev), black);
        if x >= 1.0 {
            return 0.0;
        }
        curve_only(table, x.max(1e-9).log2())
    }

    /// Display EV of a scene EV with an Exposure. The neutral Camera Raw
    /// look scales its black point with Exposure (shader
    /// `camera_raw_black`), so Exposure acts after its black ramp there.
    pub fn display_ev_exposed(&self, scene_ev: f32, exposure: f32) -> f32 {
        let table = match self {
            Self::CameraRaw { neutral: true } => &DEFAULT_TONE_CURVE,
            _ => return self.display_ev(scene_ev + exposure),
        };
        let mut ramped = camera_raw_black_ramp(2f32.powf(scene_ev), CAMERA_RAW_BLACK_NEUTRAL)
            * 2f32.powf(exposure);
        if ramped > CR_ROLLOFF_KNEE {
            let span = CR_ROLLOFF_LIMIT - CR_ROLLOFF_KNEE;
            ramped = CR_ROLLOFF_KNEE + span * (1.0 - (-(ramped - CR_ROLLOFF_KNEE) / span).exp());
        }
        curve_only(table, ramped.max(1e-9).log2())
    }
}

/// A Camera Raw style curve table without its black ramp.
fn curve_only(table: &[f32; 49], ev: f32) -> f32 {
    if ev >= 0.0 {
        return 0.0;
    }
    let position = (ev - DEFAULT_TONE_CURVE_START_EV) * 4.0;
    if position <= 0.0 {
        return ev + table[0] - DEFAULT_TONE_CURVE_START_EV;
    }
    let index = (position as usize).min(47);
    let t = position - index as f32;
    table[index] + (table[index + 1] - table[index]) * t
}

/// Curve choices on top of the base look (shader `base_curve_lookup`):
/// 1 High contrast, 2 Extra shadow, 3 Linear response (the default curve
/// undone). Display-EV change by display level.
const CURVE_CHOICE_CONTRAST: f32 = 0.15;
const CURVE_CHOICE_PIVOT_EV: f32 = -2.5;
const CURVE_CHOICE_SHADOW_LIFT: f32 = 0.8;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Scene EV that the default tone curve maps to display EV `level`.
fn default_tone_curve_inverse(level: f32) -> f32 {
    let table = &DEFAULT_TONE_CURVE;
    if level >= 0.0 {
        return 0.0;
    }
    if level <= table[0] {
        return level - table[0] + DEFAULT_TONE_CURVE_START_EV;
    }
    let i = (0..48).rev().find(|&k| table[k] <= level).unwrap_or(0);
    let t = ((level - table[i]) / (table[i + 1] - table[i]).max(1e-6)).clamp(0.0, 1.0);
    DEFAULT_TONE_CURVE_START_EV + (i as f32 + t) * 0.25
}

/// Display EV after the chosen base curve (0 = Standard: unchanged), the
/// shader's `apply_base_curve` without its chroma term.
pub fn with_base_curve(display_ev: f32, base_curve: u32) -> f32 {
    let change = match base_curve {
        1 => {
            let w = smoothstep(-10.0, -6.0, display_ev) * (1.0 - smoothstep(-1.5, 0.0, display_ev));
            CURVE_CHOICE_CONTRAST * (display_ev - CURVE_CHOICE_PIVOT_EV) * w
        }
        2 => CURVE_CHOICE_SHADOW_LIFT * (1.0 - smoothstep(-6.5, -1.5, display_ev)),
        3 => default_tone_curve_inverse(display_ev) - display_ev,
        _ => return display_ev,
    };
    (display_ev + change).min(0.0)
}

fn camera_raw_black_ramp(x: f32, black: f32) -> f32 {
    let slope = 1.0 / (1.0 - black);
    let radius = (0.5 * black).min(0.0625 / slope);
    if x <= black - radius {
        0.0
    } else if x >= black + radius {
        (x - black) * slope
    } else {
        let y = x - (black - radius);
        slope / (4.0 * radius) * y * y
    }
}

/// Capture One Auto's mean result (display EV).
const TARGET_MIDTONES: [f32; 3] = [-4.2, -2.4, -1.2];
const TARGET_P99: f32 = -0.1;
const TARGET_P5: f32 = -6.8;
const TARGET_P1: f32 = -8.9;
const EXPOSURE_LIMIT: f32 = 3.0;

/// Display-EV change at +100 of Shadows under Standard,
/// by display level (notes: benchmark-results/reference-tone-sweep-standard.json).
const RESPONSE_LEVELS: [f32; 7] = [-9.0, -7.0, -5.0, -4.0, -3.0, -2.0, -1.0];
const SHADOWS_RAISE_100: [f32; 7] = [3.23, 2.90, 2.20, 1.68, 1.12, 0.60, 0.11];
/// +50 gives about 55% of the +100 change.
const SHADOWS_EXPONENT: f32 = 0.86;
const SHADOWS_MAX: f32 = 25.0;
/// Display-EV lowering at +100 Contrast under Standard (deep tones darken;
/// upper midtones rise a little, ignored here); +50 gives about half.
const CONTRAST_LOWER_100: [f32; 7] = [3.70, 3.17, 1.65, 0.85, 0.03, -0.50, -0.33];
const CONTRAST_EXPONENT: f32 = 1.0;
/// The same under the Camera Raw look, whose sliders follow Lightroom's
/// measured responses (shader `CR_SHADOWS_RAISE`, `CR_CONTRAST_RAISE_*`).
const CR_SHADOWS_RAISE_100: [f32; 7] = [1.16, 2.46, 1.34, 0.89, 0.44, 0.12, 0.03];
const CR_SHADOWS_EXPONENT: f32 = 1.1;
const CR_CONTRAST_LOWER_100: [f32; 7] = [3.42, 2.73, 1.71, 1.08, 0.42, -0.23, -0.35];
const CR_CONTRAST_EXPONENT: f32 = 1.2;

/// A slider's display-EV response at +100 by level, and the exponent for
/// smaller amounts.
type Response = (&'static [f32; 7], f32);

/// Slider response tables (Contrast lowering, Shadows raising) for a curve.
fn response_tables(curve: DisplayCurve) -> (Response, Response) {
    match curve {
        DisplayCurve::Standard => (
            (&CONTRAST_LOWER_100, CONTRAST_EXPONENT),
            (&SHADOWS_RAISE_100, SHADOWS_EXPONENT),
        ),
        _ => (
            (&CR_CONTRAST_LOWER_100, CR_CONTRAST_EXPONENT),
            (&CR_SHADOWS_RAISE_100, CR_SHADOWS_EXPONENT),
        ),
    }
}
/// Capture One's Auto used Contrast −8…+24.
const CONTRAST_MAX: f32 = 30.0;

fn interpolate(levels: &[f32; 7], values: &[f32; 7], level: f32) -> f32 {
    if level <= levels[0] {
        return values[0];
    }
    for i in 1..levels.len() {
        if level <= levels[i] {
            let t = (level - levels[i - 1]) / (levels[i] - levels[i - 1]);
            return values[i - 1] + (values[i] - values[i - 1]) * t;
        }
    }
    values[levels.len() - 1]
}

/// Display-EV change of a tone at `level` for a slider amount (0..100).
fn response(table: &[f32; 7], exponent: f32, level: f32, amount: f32) -> f32 {
    interpolate(&RESPONSE_LEVELS, table, level) * (amount / 100.0).clamp(0.0, 1.0).powf(exponent)
}

/// Smallest amount in 0..max whose response reaches `wanted` (EV).
fn solve_amount(table: &[f32; 7], exponent: f32, level: f32, wanted: f32, max: f32) -> f32 {
    if response(table, exponent, level, max) <= wanted {
        return max;
    }
    let (mut lo, mut hi) = (0.0, max);
    for _ in 0..30 {
        let mid = 0.5 * (lo + hi);
        if response(table, exponent, level, mid) < wanted {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// Capture One Auto sets its black point with Levels (1.5–21 of 255).
const BLACK_POINT_MAX: f32 = 23.0 / 255.0;

fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Display EV after a Levels black point `black` (encoded 0..1), which the
/// luma curve applies to the encoded display value: (e − black) / (1 − black).
fn after_black_point(display_ev: f32, black: f32) -> f32 {
    let encoded = srgb_encode(2f32.powf(display_ev));
    let leveled = ((encoded - black) / (1.0 - black)).max(1e-6);
    srgb_decode(leveled).max(1e-9).log2()
}

/// Display EV of the default curve (neutral black ramp) for a scene EV.
pub fn standard_display_ev(scene_ev: f32) -> f32 {
    DisplayCurve::CameraRaw { neutral: true }.display_ev(scene_ev)
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    let index = ((sorted.len() - 1) as f32 * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Scene EV of linear luminance per analysed pixel, sorted.
fn scene_evs(image: &DynamicImage, baseline_ev: f32) -> Option<Vec<f32>> {
    let preview = downscale_f32_image(image, ANALYSIS_MAX_DIM, ANALYSIS_MAX_DIM).to_rgb32f();
    let mut ev: Vec<f32> = preview
        .pixels()
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .filter(|luma| luma.is_finite())
        .map(|luma| luma.max(1e-6).log2() + baseline_ev)
        .collect();
    if ev.len() < 64 {
        return None;
    }
    ev.sort_unstable_by(f32::total_cmp);
    Some(ev)
}

/// Share of sorted scene EVs at or above `level`.
fn share_at_or_above(sorted: &[f32], level: f32) -> f32 {
    let below = sorted.partition_point(|ev| *ev < level);
    (sorted.len() - below) as f32 / sorted.len() as f32
}

/// Scene EV percentiles (p1, p5, p25, p50, p75, p99) of linear luminance.
pub fn scene_percentiles(image: &DynamicImage, baseline_ev: f32) -> Option<[f32; 6]> {
    let ev = scene_evs(image, baseline_ev)?;
    Some([0.01, 0.05, 0.25, 0.5, 0.75, 0.99].map(|p| percentile(&ev, p)))
}

/// Predicted Standard display EV percentiles (p1, p5, p25, p50, p75, p99)
/// for an exposure, before the other sliders (used by the evaluation tool).
pub fn predicted_display_percentiles(
    image: &DynamicImage,
    baseline_ev: f32,
    curve: DisplayCurve,
    exposure: f32,
) -> Option<[f32; 6]> {
    scene_percentiles(image, baseline_ev).map(|s| s.map(|v| curve.display_ev_exposed(v, exposure)))
}

/// Auto adjustments for the Standard (and Camera Raw) base look.
/// `baseline_ev` is the base look's baseline exposure for this file,
/// `curve` the tone curve it renders with and `base_curve` the recipe's
/// Capture One-style curve choice (0 Standard, 1 High contrast, 2 Extra
/// shadow, 3 Linear response), which the prediction renders through too.
pub fn standard_auto_adjustments(
    image: &DynamicImage,
    baseline_ev: f32,
    curve: DisplayCurve,
    base_curve: u32,
) -> AutoAdjustmentResults {
    let display = |scene: f32, exposure: f32| {
        with_base_curve(curve.display_ev_exposed(scene, exposure), base_curve)
    };
    let ((contrast_table, contrast_exponent), (shadows_table, shadows_exponent)) =
        response_tables(curve);
    let mut results = AutoAdjustmentResults::default();
    let Some(evs) = scene_evs(image, baseline_ev) else {
        return results;
    };
    let [s1, s5, s25, s50, s75, s99] =
        [0.01, 0.05, 0.25, 0.5, 0.75, 0.99].map(|p| percentile(&evs, p));
    // Scene EV that renders at display white for an exposure (the display
    // curve rises monotonically, so bisect).
    let white_scene_ev = |exposure: f32| {
        let (mut lo, mut hi) = (-20.0f32, 12.0f32);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if display(mid, exposure) >= -WHITE_MARGIN_EV {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    };
    let blown_share = |exposure: f32| share_at_or_above(&evs, white_scene_ev(exposure));
    // Rendered display EV of a scene EV with the chosen tone controls.
    let render = |scene: f32, exposure: f32, contrast: f32, shadows: f32, black: f32| {
        let base = display(scene, exposure);
        let toned = base - response(contrast_table, contrast_exponent, base, contrast)
            + response(shadows_table, shadows_exponent, base, shadows);
        after_black_point(toned, black)
    };
    let solve_exposure = |contrast: f32, shadows: f32, black: f32| {
        let midtone_error = |exposure: f32| {
            [s25, s50, s75]
                .iter()
                .zip(TARGET_MIDTONES)
                .map(|(s, target)| render(*s, exposure, contrast, shadows, black) - target)
                .sum::<f32>()
                / 3.0
        };
        // Bisection: the midtone error rises monotonically with exposure.
        let (mut lo, mut hi) = (-EXPOSURE_LIMIT, EXPOSURE_LIMIT);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if midtone_error(mid) > 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        0.5 * (lo + hi)
    };
    let black_point_for = |p1: f32| {
        let (encoded, target) = (
            srgb_encode(2f32.powf(p1)),
            srgb_encode(2f32.powf(TARGET_P1)),
        );
        ((encoded - target) / (1.0 - target)).clamp(0.0, BLACK_POINT_MAX)
    };

    // Deep tones: Contrast darkens a lifted deep end toward Capture One's p5
    // (as its Auto does), Shadows open a too dark one; then a Levels black
    // point (the luma curve's black input) sets p1. Exposure is solved again
    // with these so the midtones stay on target.
    let (mut contrast, mut shadows, mut black) = (0.0, 0.0, 0.0);
    let mut exposure = solve_exposure(0.0, 0.0, 0.0);
    for _ in 0..3 {
        let p5 = display(s5, exposure);
        (contrast, shadows) = (0.0, 0.0);
        if p5 > TARGET_P5 + 0.3 {
            contrast = solve_amount(
                contrast_table,
                contrast_exponent,
                p5,
                p5 - TARGET_P5,
                CONTRAST_MAX,
            );
        } else if p5 < TARGET_P5 - 0.3 {
            shadows = solve_amount(
                shadows_table,
                shadows_exponent,
                p5,
                TARGET_P5 - p5,
                SHADOWS_MAX,
            );
        }
        black = black_point_for(render(s1, exposure, contrast, shadows, 0.0));
        exposure = solve_exposure(contrast, shadows, black);
    }
    // Large blown areas (a bright sky): like Capture One's Auto, do not
    // brighten the photo further; Shadows open the subject instead.
    if blown_share(0.0) > BLOWN_NO_BRIGHTEN_SHARE && exposure > 0.0 {
        exposure = 0.0;
        black = black_point_for(render(s1, exposure, contrast, shadows, 0.0));
    }
    results.contrast = contrast.round() as f64;
    results.shadows = shadows.round() as f64;

    // Highlights from the share of the frame that renders at white (Capture
    // One's Auto on nine clipped GFX 50R frames: −74…−80 with a quarter to a
    // third of the frame blown, −16…−23 with lamps and windows of ~10 %),
    // at least the overshoot of the 99th percentile.
    let blown = blown_share(exposure);
    let overshoot = s99 + exposure;
    if blown > BLOWN_MIN_SHARE || overshoot > -0.1 {
        let by_area = blown * HIGHLIGHTS_PER_BLOWN_SHARE;
        let by_overshoot = (overshoot + 0.1) * 16.0;
        results.highlights = -by_area.max(by_overshoot).clamp(0.0, 80.0) as f64;
    } else {
        // Whites: dull tops are lifted; +100 raises −1 EV by about 0.45 EV.
        let top = display(s99, exposure);
        if top < TARGET_P99 - 0.25 {
            results.whites = ((TARGET_P99 - top) / 0.0045).clamp(0.0, 60.0) as f64;
        }
    }
    results.exposure = exposure as f64;
    results.black_point = (black * 255.0) as f64;
    results
}

/// JSON for the frontend: only the controls this auto sets.
pub fn standard_auto_to_json(results: &AutoAdjustmentResults) -> serde_json::Value {
    let mut json = serde_json::json!({
        "exposure": (results.exposure * 100.0).round() / 100.0,
        "highlights": results.highlights.round(),
        "whites": results.whites.round(),
        "shadows": results.shadows.round(),
        "contrast": results.contrast.round(),
        "blacks": 0,
        "sectionVisibility": { "basic": true },
    });
    if results.black_point >= 0.5 {
        // Applied to the luma curve only while it is still the identity
        // (frontend and batch merge); x is the input level on 0..255.
        json["autoBlackPoint"] = serde_json::json!((results.black_point * 10.0).round() / 10.0);
    }
    json
}

fn is_identity_curve(points: Option<&serde_json::Value>) -> bool {
    let Some(points) = points else {
        return true;
    };
    let Some(points) = points.as_array() else {
        return false;
    };
    let at = |p: &serde_json::Value, key: &str| p.get(key).and_then(|v| v.as_f64());
    points.len() == 2
        && at(&points[0], "x") == Some(0.0)
        && at(&points[0], "y") == Some(0.0)
        && at(&points[1], "x") == Some(255.0)
        && at(&points[1], "y") == Some(255.0)
}

/// Merges an auto result into a recipe: values replace, section visibility
/// merges, and `autoBlackPoint` becomes the luma curve's black input only
/// while that curve is still the identity (a user curve is never replaced).
/// Mirrors `handleAutoAdjustments` in src/hooks/useEditorActions.ts.
pub fn merge_auto_adjustments(adjustments: &mut serde_json::Value, auto: &serde_json::Value) {
    if !adjustments.is_object() {
        *adjustments = serde_json::json!({});
    }
    let (Some(existing), Some(auto)) = (adjustments.as_object_mut(), auto.as_object()) else {
        return;
    };
    for (key, value) in auto {
        match key.as_str() {
            "autoBlackPoint" => {
                let Some(black) = value.as_f64() else {
                    continue;
                };
                let curves = existing
                    .entry("curves")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(curves) = curves.as_object_mut()
                    && is_identity_curve(curves.get("luma"))
                {
                    curves.insert(
                        "luma".to_string(),
                        serde_json::json!([{"x": black, "y": 0}, {"x": 255, "y": 255}]),
                    );
                }
            }
            "sectionVisibility" => match (existing.get_mut(key), value.as_object()) {
                (Some(serde_json::Value::Object(visible)), Some(auto_visible)) => {
                    for (section, flag) in auto_visible {
                        visible.insert(section.clone(), flag.clone());
                    }
                }
                _ => {
                    existing.insert(key.clone(), value.clone());
                }
            },
            _ => {
                existing.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn shader_array(shader: &str, name: &str) -> Vec<f32> {
        let start = shader.find(&format!("const {name} = array<f32, ")).unwrap();
        let body = &shader[start..];
        let open = body.find(">(").unwrap() + 2;
        body[open..body.find(");").unwrap()]
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.parse().unwrap())
            .collect()
    }

    /// Monotone cubic (Fritsch-Carlson, as SciPy's PCHIP) through `points`.
    fn pchip(points: &[(f64, f64)], x: f64) -> f64 {
        let n = points.len();
        let h: Vec<f64> = (0..n - 1).map(|i| points[i + 1].0 - points[i].0).collect();
        let m: Vec<f64> = (0..n - 1)
            .map(|i| (points[i + 1].1 - points[i].1) / h[i])
            .collect();
        let mut d = vec![0.0; n];
        for k in 1..n - 1 {
            if m[k - 1] * m[k] > 0.0 {
                let w1 = 2.0 * h[k] + h[k - 1];
                let w2 = h[k] + 2.0 * h[k - 1];
                d[k] = (w1 + w2) / (w1 / m[k - 1] + w2 / m[k]);
            }
        }
        let edge = |h0: f64, h1: f64, m0: f64, m1: f64| {
            let d = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
            if d.signum() != m0.signum() {
                0.0
            } else if m0.signum() != m1.signum() && d.abs() > 3.0 * m0.abs() {
                3.0 * m0
            } else {
                d
            }
        };
        d[0] = edge(h[0], h[1], m[0], m[1]);
        d[n - 1] = edge(h[n - 2], h[n - 3], m[n - 2], m[n - 3]);
        let i = (0..n - 1).rev().find(|&i| points[i].0 <= x).unwrap_or(0);
        let t = (x - points[i].0) / h[i];
        let (y0, y1) = (points[i].1, points[i + 1].1);
        let (h00, h10, h01, h11) = (
            2.0 * t.powi(3) - 3.0 * t * t + 1.0,
            t.powi(3) - 2.0 * t * t + t,
            -2.0 * t.powi(3) + 3.0 * t * t,
            t.powi(3) - t * t,
        );
        h00 * y0 + h10 * h[i] * d[i] + h01 * y1 + h11 * h[i] * d[i + 1]
    }

    #[test]
    fn default_curve_is_built_from_its_points_and_matches_the_shader() {
        // The five points, a sixth 6 EV below the first (slope 1 there) and
        // white at 0 EV.
        let (first_ev, first_display) = DEFAULT_TONE_CURVE_POINTS[0];
        let mut points = vec![(f64::from(first_ev) - 6.0, f64::from(first_display) - 6.0)];
        points.extend(
            DEFAULT_TONE_CURVE_POINTS
                .iter()
                .map(|&(e, d)| (f64::from(e), f64::from(d))),
        );
        points.push((0.0, 0.0));
        for (i, value) in DEFAULT_TONE_CURVE.iter().enumerate() {
            let scene = -12.0 + 0.25 * i as f64;
            assert!(
                (f64::from(*value) - pchip(&points, scene)).abs() < 2e-4,
                "{scene}"
            );
        }
        assert!(DEFAULT_TONE_CURVE.windows(2).all(|w| w[1] > w[0]));
        let shader = include_str!("../../gpu/src/shaders/shader.wgsl");
        assert_eq!(
            shader_array(shader, "DEFAULT_TONE_CURVE"),
            DEFAULT_TONE_CURVE.to_vec()
        );
        assert!(shader.contains("const DEFAULT_TONE_CURVE_START_EV: f32 = -12.0;"));
        assert!(shader.contains("const CAMERA_RAW_BLACK: f32 = 0.005;"));
        assert!(shader.contains("const CAMERA_RAW_BLACK_NEUTRAL: f32 = 0.006;"));
        assert!(shader.contains("const CR_ROLLOFF_KNEE: f32 = 0.55;"));
        assert!(shader.contains("const CR_ROLLOFF_LIMIT: f32 = 1.02;"));
    }

    #[test]
    fn curve_choices_match_the_shader_and_change_auto() {
        let shader = include_str!("../../gpu/src/shaders/shader.wgsl");
        assert!(shader.contains(&format!(
            "const CURVE_CHOICE_CONTRAST: f32 = {CURVE_CHOICE_CONTRAST};"
        )));
        assert!(shader.contains(&format!(
            "const CURVE_CHOICE_PIVOT_EV: f32 = {CURVE_CHOICE_PIVOT_EV:.1};"
        )));
        assert!(shader.contains(&format!(
            "const CURVE_CHOICE_SHADOW_LIFT: f32 = {CURVE_CHOICE_SHADOW_LIFT};"
        )));
        // Standard: unchanged. Linear response undoes the default curve:
        // the result is the scene EV itself.
        assert_eq!(with_base_curve(-3.0, 0), -3.0);
        for scene in [-9.0f32, -6.0, -3.0, -1.0] {
            let display = curve_only(&DEFAULT_TONE_CURVE, scene);
            assert!(
                (with_base_curve(display, 3) - scene).abs() < 1e-3,
                "{scene}"
            );
        }
        // High contrast: darker below mid grey, brighter above, white kept.
        assert!(with_base_curve(-5.0, 1) < -5.0);
        assert!(with_base_curve(-1.5, 1) > -1.5);
        assert_eq!(with_base_curve(0.0, 1), 0.0);
        // Extra shadow lifts the shadows, never above white.
        assert!(with_base_curve(-7.0, 2) > -7.0 + 0.7);
        assert!(with_base_curve(0.0, 2) <= 0.0);
        // Auto on the Linear response curve raises Exposure more than on Standard.
        let standard = standard_auto_adjustments(
            &uniform(0.05),
            0.72,
            DisplayCurve::CameraRaw { neutral: true },
            0,
        );
        let linear = standard_auto_adjustments(
            &uniform(0.05),
            0.72,
            DisplayCurve::CameraRaw { neutral: true },
            3,
        );
        assert!(
            linear.exposure > standard.exposure,
            "{} vs {}",
            linear.exposure,
            standard.exposure
        );
    }

    #[test]
    fn camera_raw_auto_targets_its_own_curve() {
        for level in [0.02f32, 0.06, 0.2] {
            let curve = DisplayCurve::CameraRaw { neutral: true };
            let auto = standard_auto_adjustments(&uniform(level), 0.72, curve, 0);
            let mid = curve.display_ev_exposed(level.log2() + 0.72, auto.exposure as f32);
            assert!(
                (mid - TARGET_MIDTONES[1]).abs() < 0.6,
                "level {level}: {mid}"
            );
        }
        // With its +0.72 EV baseline the Camera Raw look is brighter in the
        // midtones than Standard, so the same image needs less exposure.
        let standard = standard_auto_adjustments(&uniform(0.05), 0.0, DisplayCurve::Standard, 0);
        let camera_raw = standard_auto_adjustments(
            &uniform(0.05),
            0.72,
            DisplayCurve::CameraRaw { neutral: true },
            0,
        );
        assert!(camera_raw.exposure < standard.exposure);
    }

    fn uniform(level: f32) -> DynamicImage {
        DynamicImage::ImageRgb32F(ImageBuffer::from_fn(64, 64, |x, y| {
            // A spread of tones around `level` (linear).
            let v = level * 2f32.powf((x as f32 / 63.0 - 0.5) * 6.0 + (y as f32 / 63.0 - 0.5));
            Rgb([v, v, v])
        }))
    }

    #[test]
    fn exposure_brings_midtones_to_capture_one_auto_targets() {
        for level in [0.02f32, 0.06, 0.2] {
            let auto = standard_auto_adjustments(&uniform(level), 0.0, DisplayCurve::Standard, 0);
            let mid = standard_display_ev(level.log2() + auto.exposure as f32);
            assert!(
                (mid - TARGET_MIDTONES[1]).abs() < 0.6,
                "level {level}: {mid}"
            );
        }
        // Darker images get more exposure, never beyond the limit.
        let dark =
            standard_auto_adjustments(&uniform(0.002), 0.0, DisplayCurve::Standard, 0).exposure;
        let bright =
            standard_auto_adjustments(&uniform(0.2), 0.0, DisplayCurve::Standard, 0).exposure;
        assert!(dark > bright && dark <= EXPOSURE_LIMIT as f64);
        // Baseline exposure is part of the scene value.
        let shifted = standard_auto_adjustments(
            &uniform(0.02),
            1.0,
            DisplayCurve::CameraRaw { neutral: true },
            0,
        )
        .exposure;
        let plain = standard_auto_adjustments(
            &uniform(0.02),
            0.0,
            DisplayCurve::CameraRaw { neutral: true },
            0,
        )
        .exposure;
        // The black point follows Exposure, so the larger Exposure also
        // deepens the toe a little (0.90 EV apart with Adobe's 0.5 % ramp).
        assert!(
            (plain - shifted - 1.0).abs() < 0.15,
            "plain {plain} shifted {shifted}"
        );
    }

    #[test]
    fn black_point_goes_into_an_identity_luma_curve_only() {
        let auto = serde_json::json!({"exposure": 0.5, "autoBlackPoint": 7.5,
            "sectionVisibility": {"basic": true}});
        let mut fresh = serde_json::json!({"sectionVisibility": {"color": false},
            "curves": {"luma": [{"x":0,"y":0},{"x":255,"y":255}], "red": [{"x":0,"y":0},{"x":200,"y":255}]}});
        merge_auto_adjustments(&mut fresh, &auto);
        assert_eq!(fresh["exposure"], 0.5);
        assert_eq!(fresh["curves"]["luma"][0]["x"], 7.5);
        assert_eq!(fresh["curves"]["red"][1]["x"], 200);
        assert_eq!(fresh["sectionVisibility"]["color"], false);
        assert!(fresh.get("autoBlackPoint").is_none());
        let user_curve = serde_json::json!([{"x":0,"y":10},{"x":128,"y":140},{"x":255,"y":255}]);
        let mut edited = serde_json::json!({"curves": {"luma": user_curve.clone()}});
        merge_auto_adjustments(&mut edited, &auto);
        assert_eq!(edited["curves"]["luma"], user_curve);
        let mut empty = serde_json::Value::Null;
        merge_auto_adjustments(&mut empty, &auto);
        assert_eq!(empty["curves"]["luma"][0]["x"], 7.5);
    }

    #[test]
    fn only_tonal_controls_are_set() {
        let json = standard_auto_to_json(&standard_auto_adjustments(
            &uniform(0.01),
            0.0,
            DisplayCurve::CameraRaw { neutral: true },
            0,
        ));
        for key in ["vibrance", "clarity", "dehaze", "vignetteAmount", "centré"] {
            assert!(json.get(key).is_none(), "{key}");
        }
    }
}
