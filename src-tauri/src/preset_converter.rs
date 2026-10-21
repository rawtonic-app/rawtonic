use regex::Regex;
use regex::regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use uuid::Uuid;

use crate::file_management::Preset;

#[derive(Copy, Clone, Debug)]
enum Num {
    I(i64),
    F(f64),
}

fn parse_num(s: &str) -> Option<Num> {
    if let Ok(i) = s.parse::<i64>() {
        Some(Num::I(i))
    } else if let Ok(f) = s.parse::<f64>() {
        Some(Num::F(f))
    } else {
        None
    }
}

fn num_to_json(num: Num) -> Option<Value> {
    match num {
        Num::I(i) => Some(Value::Number(i.into())),
        Num::F(f) => serde_json::Number::from_f64(f).map(Value::Number),
    }
}

fn get_attr_as_f64(attrs: &HashMap<String, String>, key: &str) -> Option<f64> {
    attrs
        .get(key)
        .and_then(|s| s.trim_start_matches('+').parse::<f64>().ok())
}

fn extract_xmp_name(xmp_content: &str) -> Option<String> {
    regex!(r#"(?s)<crs:Name>.*?<rdf:Alt>.*?<rdf:li[^>]*>([^<]+)</rdf:li>.*?</crs:Name>"#)
        .captures(xmp_content)
        .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
}

fn extract_tone_curve_points(xmp_str: &str, curve_name: &str) -> Option<Vec<Value>> {
    let pattern = format!(
        r"(?s)<crs:{}>\s*<rdf:Seq>(.*?)</rdf:Seq>\s*</crs:{}>",
        curve_name, curve_name
    );
    let re = Regex::new(&pattern).ok()?;
    let captures = re.captures(xmp_str)?;
    let seq_content = captures.get(1)?.as_str();

    let point_re = regex!(r"<rdf:li>(\d+),\s*(\d+)</rdf:li>");
    let mut points = Vec::new();

    for point_cap in point_re.captures_iter(seq_content) {
        let x: u32 = point_cap.get(1)?.as_str().parse().ok()?;
        let y: u32 = point_cap.get(2)?.as_str().parse().ok()?;

        let mut point = Map::new();
        point.insert("x".to_string(), Value::Number(x.into()));
        point.insert("y".to_string(), Value::Number(y.into()));
        points.push(Value::Object(point));
    }

    if points.is_empty() {
        None
    } else {
        Some(lightroom_curve(&points))
    }
}

/// Natural cubic spline through `knots` (sorted, distinct x), flat beyond
/// the ends: Camera Raw's point curve on display values, measured
/// 2026-10-21 (notes: reference/point-curve.md; 3-point curve 0.11 levels
/// mean, overshooting curve 0.21, where the renderer's Hermite through the
/// same points is 1.37 and 5.14 levels off).
fn natural_spline(knots: &[(f64, f64)], x: f64) -> f64 {
    let n = knots.len();
    if x <= knots[0].0 {
        return knots[0].1;
    }
    if x >= knots[n - 1].0 {
        return knots[n - 1].1;
    }
    let h: Vec<f64> = knots.windows(2).map(|w| w[1].0 - w[0].0).collect();
    // Second derivatives (zero at both ends), tridiagonal solve.
    let mut m = vec![0.0; n];
    if n > 2 {
        let mut diag = vec![0.0; n];
        let mut rhs = vec![0.0; n];
        for i in 1..n - 1 {
            diag[i] = 2.0 * (h[i - 1] + h[i]);
            rhs[i] = 6.0
                * ((knots[i + 1].1 - knots[i].1) / h[i] - (knots[i].1 - knots[i - 1].1) / h[i - 1]);
        }
        for i in 2..n - 1 {
            let f = h[i - 1] / diag[i - 1];
            diag[i] -= f * h[i - 1];
            rhs[i] -= f * rhs[i - 1];
        }
        for i in (1..n - 1).rev() {
            m[i] = (rhs[i] - h[i] * m[i + 1]) / diag[i];
        }
    }
    let i = knots.windows(2).position(|w| x <= w[1].0).unwrap_or(n - 2);
    let (x0, y0) = knots[i];
    let (x1, y1) = knots[i + 1];
    let (a, b) = ((x1 - x) / h[i], (x - x0) / h[i]);
    a * y0 + b * y1 + ((a * a * a - a) * m[i] + (b * b * b - b) * m[i + 1]) * h[i] * h[i] / 6.0
}

/// A Lightroom point curve as Rawtonic's: its points, plus knots on
/// Lightroom's spline wherever the renderer's curve through the points
/// would leave it by more than a quarter level (checked every eighth of an
/// input level), on whole levels: a jump within one level cannot be
/// followed closer, and the remaining error is logged.
fn lightroom_curve(points: &[Value]) -> Vec<Value> {
    let mut knots: Vec<(f64, f64)> = points
        .iter()
        .filter_map(|p| Some((p["x"].as_f64()?, p["y"].as_f64()?)))
        .collect();
    knots.sort_by(|a, b| a.0.total_cmp(&b.0));
    knots.dedup_by(|a, b| a.0 == b.0);
    if knots.len() < 2 {
        return points.to_vec();
    }
    let lightroom = knots.clone();
    let target = |x: f64| natural_spline(&lightroom, x).clamp(0.0, 255.0);
    // Errors every eighth of a level (the renderer's input is continuous),
    // largest first; new knots sit on whole levels, at least one apart, as
    // the curve editor keeps its points.
    let errors = |knots: &[(f64, f64)]| {
        let ours: Vec<(f32, f32)> = knots.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
        let mut errors: Vec<(f64, f64)> = (0..=255 * 8)
            .map(|i| {
                let x = i as f64 / 8.0;
                let got = rawtonic_engine::point_curve::curve_value(&ours, (x / 255.0) as f32);
                (x, (got as f64 * 255.0 - target(x)).abs())
            })
            .filter(|&(_, error)| error > 0.25)
            .collect();
        errors.sort_by(|a, b| b.1.total_cmp(&a.1));
        errors
    };
    loop {
        let free = |x: f64| knots.iter().all(|k| (k.0 - x).abs() >= 1.0);
        let next = errors(&knots)
            .into_iter()
            .flat_map(|(x, _)| [x.round(), x.floor(), x.ceil()])
            .find(|&x| free(x));
        let Some(x) = next else { break };
        let at = knots.partition_point(|k| k.0 < x);
        knots.insert(at, (x, (target(x) * 100.0).round() / 100.0));
    }
    if let Some(&(x, error)) = errors(&knots).first() {
        log::info!(
            "Lightroom curve import: {error:.2} levels from Lightroom's curve at input {x} (no whole-level point left to add)"
        );
    }
    knots
        .into_iter()
        .map(|(x, y)| json!({"x": x, "y": y}))
        .collect()
}

/// Linear ProPhoto (D50) to linear sRGB (D65, Bradford).
const PROPHOTO_TO_SRGB: [[f64; 3]; 3] = [
    [2.0341926, -0.7274817, -0.3067109],
    [-0.2288247, 1.2317265, -0.0029018],
    [-0.0085555, -0.1532680, 1.1618235],
];

/// Oklab (L, chroma, hue in degrees) of a colour given as HSV of linear
/// ProPhoto, as Rawtonic's colour editor sees it after the display clip.
fn prophoto_hsv_to_oklab(hue_sixths: f64, saturation: f64, value: f64) -> (f64, f64, f64) {
    let h = hue_sixths.rem_euclid(6.0);
    let (s, v) = (saturation.clamp(0.0, 1.0), value.clamp(0.0, 1.0));
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let rgb = match h.floor() as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    };
    let srgb: [f64; 3] = std::array::from_fn(|i| {
        (0..3)
            .map(|k| PROPHOTO_TO_SRGB[i][k] * rgb[k])
            .sum::<f64>()
            .clamp(0.0, 1.0)
    });
    let [r, g, b] = srgb;
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let lab = [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ];
    (
        lab[0],
        lab[1].hypot(lab[2]),
        lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0),
    )
}

/// Camera Raw's hue weights fall off more gently than the colour editor's
/// window of the same half-width (measured, notes: reference/point-color.md).
const HUE_WINDOW_WIDENING: f64 = 1.15;

fn hue_difference(a: f64, b: f64) -> f64 {
    (a - b + 540.0).rem_euclid(360.0) - 180.0
}

/// Lightroom's Point Color (`crs:PointColors`, one swatch of 19 numbers
/// each, and `crs:ColorVariance`) as colour editor ranges. A swatch holds
/// its colour as HSV of linear ProPhoto (hue in sixths of a turn), the hue,
/// saturation and luminance shifts (−1…1), Range (0–1) and three
/// trapezoids (zero, full, full, zero): hue relative to the swatch (1 = 60°
/// of HSV hue), saturation, and luminance on the value with gamma 2.2.
/// Measured in Camera Raw (notes: reference/point-color.md): a swatch whose
/// own saturation or luminance lies outside its trapezoid's plateau has no
/// effect; hue ±1 turns HSV hue by about ±33°, saturation −1 scales it by
/// 0.53, luminance −1 / +1 scales the value by 0.48 / 2.17; Range widens the
/// hue window (×0.72 / ×1 / ×1.06 for Range 0 / 0.5 / 1).
fn point_color_ranges(xmp_one_line: &str) -> Vec<Value> {
    let seq = |name: &str| -> Vec<String> {
        Regex::new(&format!(
            r"<crs:{name}>\s*<rdf:Seq>(.*?)</rdf:Seq>\s*</crs:{name}>"
        ))
        .ok()
        .and_then(|re| re.captures(xmp_one_line))
        .map(|c| {
            regex!(r"<rdf:li>([^<]*)</rdf:li>")
                .captures_iter(&c[1])
                .map(|li| li[1].to_string())
                .collect()
        })
        .unwrap_or_default()
    };
    point_color_ranges_from(&seq("PointColors"), &seq("ColorVariance"), 8)
}

/// Colour editor ranges for Point Color swatches (19 numbers each), at most
/// `limit` (8 for the photo, 4 per layer).
fn point_color_ranges_from(swatches: &[String], variances: &[String], limit: usize) -> Vec<Value> {
    let mut ranges = Vec::new();
    for (index, swatch) in swatches.iter().enumerate() {
        let v: Vec<f64> = swatch
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        if v.len() < 19 || ranges.len() >= limit {
            continue;
        }
        let (h, s, value) = (v[0], v[1], v[2]);
        let (hue_shift, sat_shift, lum_shift, range) = (v[3], v[4], v[5], v[6]);
        let (hue_t, sat_t, lum_t) = (&v[7..11], &v[11..15], &v[15..19]);
        let lum = value.max(0.0).powf(1.0 / 2.2);
        if s < sat_t[1] - 1.0e-4
            || s > sat_t[2] + 1.0e-4
            || lum < lum_t[1] - 1.0e-4
            || lum > lum_t[2] + 1.0e-4
        {
            continue;
        }
        let (l0, c0, h0) = prophoto_hsv_to_oklab(h, s, value);
        // Hue window: half-weight points of the trapezoid in HSV degrees,
        // as Oklab half-width (widened to match Camera Raw's smooth edges).
        // Range: half-width ×0.72 at 0 and ×1.06 at 1 of that at 0.5.
        let range = range.clamp(0.0, 1.0);
        let range_scale = if range <= 0.5 {
            0.72 + 0.56 * range
        } else {
            1.0 + 0.12 * (range - 0.5)
        };
        let scale = 63.0 * range_scale;
        let below = (0.5 - 0.5 * (hue_t[0] + hue_t[1])) * scale;
        let above = (0.5 * (hue_t[2] + hue_t[3]) - 0.5) * scale;
        let at = |degrees: f64| prophoto_hsv_to_oklab(h + degrees / 60.0, s, value).2;
        let half_width = HUE_WINDOW_WIDENING
            * 0.5
            * (hue_difference(h0, at(-below)).abs() + hue_difference(at(above), h0).abs());
        // Saturation: the lower half-weight point as a chroma fraction.
        let s_low = 0.5 * (sat_t[0] + sat_t[1]);
        let chroma_range = if s_low <= 0.0 || c0 <= 1.0e-4 {
            100.0
        } else {
            (1.0 - prophoto_hsv_to_oklab(h, s_low, value).1 / c0) * 100.0
        };
        // Luminance band as Oklab lightness; an edge at the end of the
        // value axis leaves that side open (lighter or darker colours of
        // the hue have other saturations, so converting it would cut them).
        let l_at = |lum: f64| prophoto_hsv_to_oklab(h, s, lum.clamp(0.0, 1.0).powf(2.2)).0;
        let (lum_low, lum_high) = (0.5 * (lum_t[0] + lum_t[1]), 0.5 * (lum_t[2] + lum_t[3]));
        let l_low = if lum_low <= 0.05 { -0.1 } else { l_at(lum_low) };
        let l_high = if lum_high >= 0.9 { 1.1 } else { l_at(lum_high) };
        // Shifts act in HSV at constant value: the swatch's shifted colour
        // gives Oklab hue, chroma and lightness changes (turning red towards
        // orange or desaturating it also lightens it, as in Camera Raw).
        let shifted_h = h + 33.0 * hue_shift.clamp(-1.0, 1.0) / 60.0;
        let shifted_s = if sat_shift < 0.0 {
            s * 0.53f64.powf(-sat_shift.max(-1.0))
        } else {
            s + (1.0 - s) * (1.0 - 0.33f64.powf(sat_shift.min(1.0)))
        };
        let value_ratio = if lum_shift < 0.0 {
            0.48f64.powf(-lum_shift.max(-1.0))
        } else {
            2.17f64.powf(lum_shift.min(1.0))
        };
        let (l1, c1, h1) =
            prophoto_hsv_to_oklab(shifted_h, shifted_s, (value * value_ratio).min(1.0));
        let hue_degrees = hue_difference(h1, h0);
        let saturation = if c0 > 1.0e-4 {
            (c1 / c0 - 1.0) * 100.0
        } else {
            0.0
        };
        // The colour editor's lightness scales: ±100 = ×1.4 / ×0.6.
        let lightness_shift = if l0 > 1.0e-3 {
            (l1 / l0 - 1.0) / 0.4 * 100.0
        } else {
            0.0
        };
        // Negative variance evens the range out (Capture One's uniformity).
        let uniformity = variances
            .get(index)
            .and_then(|x| x.trim().parse::<f64>().ok())
            .map_or(0.0, |x| {
                if x < 0.0 {
                    (-x * 100.0).min(100.0)
                } else {
                    0.0
                }
            });
        let round = |x: f64| (x * 1000.0).round() / 1000.0;
        ranges.push(json!({
            "id": Uuid::new_v4().to_string(),
            "hue": round(h0),
            "chroma": round(c0),
            "lightness": round(0.5 * (l_low + l_high)),
            "hueRange": round(half_width.clamp(5.0, 90.0)),
            "chromaRange": round(chroma_range.clamp(0.0, 100.0)),
            "lightnessRange": round(((l_high - l_low) * 50.0 - 2.0).clamp(0.0, 100.0)),
            "smoothness": 55,
            "hueShift": round((hue_degrees / 0.5).clamp(-100.0, 100.0)),
            "saturation": round(saturation.clamp(-100.0, 100.0)),
            "lightnessShift": round(lightness_shift.clamp(-100.0, 100.0)),
            "hueUniformity": round(uniformity),
            "saturationUniformity": round(uniformity),
            "lightnessUniformity": round(uniformity),
        }));
    }
    ranges
}

pub fn convert_xmp_to_preset(xmp_content: &str) -> Result<Preset, String> {
    let xmp_one_line = xmp_content.split('\n').collect::<Vec<_>>().join(" ");

    let attr_re = regex!(r#"crs:([A-Za-z0-9]+)="([^"]*)""#);
    let mut attrs: HashMap<String, String> = HashMap::new();
    for cap in attr_re.captures_iter(&xmp_one_line) {
        attrs.insert(cap[1].to_string(), cap[2].to_string());
    }

    let mut adjustments = Map::new();
    let mut hsl_map = Map::new();
    let mut color_grading_map = Map::new();
    let mut curves_map = Map::new();

    let mappings = vec![
        ("Exposure2012", "exposure"),
        ("Contrast2012", "contrast"),
        ("Highlights2012", "highlights"),
        ("Whites2012", "whites"),
        ("Blacks2012", "blacks"),
        ("Clarity2012", "clarity"),
        ("Dehaze", "dehaze"),
        ("Vibrance", "vibrance"),
        ("Saturation", "saturation"),
        ("Texture", "structure"),
        ("SharpenRadius", "sharpenRadius"),
        ("SharpenDetail", "sharpenDetail"),
        ("SharpenEdgeMasking", "sharpenMasking"),
        ("LuminanceSmoothing", "lumaNoiseReduction"),
        ("LuminanceNoiseReductionDetail", "lumaNoiseDetail"),
        ("LuminanceNoiseReductionContrast", "lumaNoiseContrast"),
        ("ColorNoiseReduction", "colorNoiseReduction"),
        ("ColorNoiseReductionDetail", "colorNoiseDetail"),
        ("ColorNoiseReductionSmoothness", "colorNoiseSmoothness"),
        ("ChromaticAberrationRedCyan", "chromaticAberrationRedCyan"),
        (
            "ChromaticAberrationBlueYellow",
            "chromaticAberrationBlueYellow",
        ),
        ("PostCropVignetteAmount", "vignetteAmount"),
        ("PostCropVignetteMidpoint", "vignetteMidpoint"),
        ("PostCropVignetteFeather", "vignetteFeather"),
        ("PostCropVignetteRoundness", "vignetteRoundness"),
        ("GrainAmount", "grainAmount"),
        ("GrainSize", "grainSize"),
        ("GrainFrequency", "grainRoughness"),
        ("ColorGradeBlending", "blending"),
        ("DefringePurpleAmount", "defringePurpleAmount"),
        ("DefringePurpleHueLo", "defringePurpleHueLo"),
        ("DefringePurpleHueHi", "defringePurpleHueHi"),
        ("DefringeGreenAmount", "defringeGreenAmount"),
        ("DefringeGreenHueLo", "defringeGreenHueLo"),
        ("DefringeGreenHueHi", "defringeGreenHueHi"),
        // Same scale and response as Camera Raw (notes:
        // reference/manual-distortion.md).
        ("LensManualDistortionAmount", "transformDistortion"),
        // The manual Transform sliders, measured to Camera Raw's model
        // (notes: reference/transform.md).
        ("PerspectiveVertical", "transformVertical"),
        ("PerspectiveHorizontal", "transformHorizontal"),
        ("PerspectiveRotate", "transformRotate"),
        ("PerspectiveAspect", "transformAspect"),
        ("PerspectiveScale", "transformScale"),
        ("PerspectiveX", "transformXOffset"),
        ("PerspectiveY", "transformYOffset"),
    ];

    for (xmp_key, rr_key) in mappings {
        if let Some(raw_val) = attrs.get(xmp_key)
            && let Some(num) = parse_num(raw_val.trim_start_matches('+'))
            && let Some(json_val) = num_to_json(num)
        {
            if rr_key == "blending" {
                color_grading_map.insert(rr_key.to_string(), json_val);
            } else {
                adjustments.insert(rr_key.to_string(), json_val);
            }
        }
    }

    // Lightroom's grain gathers into clumps as Roughness rises; Clumped is
    // the structure that does the same (notes: reference/grain.md), and the
    // one new edits start with (user decision 2026-10-12).
    if adjustments
        .get("grainAmount")
        .and_then(Value::as_f64)
        .is_some_and(|amount| amount > 0.0)
    {
        adjustments.insert("grainStructure".to_string(), json!("clumped"));
    }

    // Shadows is calibrated to Lightroom's response (notes: reference), so the
    // former x1.5 compensation for a weak lift would now overshoot.
    if let Some(shadows_val) = get_attr_as_f64(&attrs, "Shadows2012") {
        adjustments.insert(
            "shadows".to_string(),
            json!(shadows_val.clamp(-100.0, 100.0)),
        );
    }

    // Rawtonic's Amount is calibrated to Lightroom's on the same scale
    // (notes: reference/detail.md: 40 = Lightroom's 40), so it maps 1:1 up to
    // the slider's 100 (the inherited x100/150 turned Lightroom's 40 into 27).
    if let Some(sharpness_val) = get_attr_as_f64(&attrs, "Sharpness") {
        adjustments.insert(
            "sharpness".to_string(),
            json!(sharpness_val.clamp(0.0, 100.0)),
        );
    }

    if let Some(adjusted_k) = get_attr_as_f64(&attrs, "Temperature") {
        const AS_SHOT_DEFAULT: f64 = 5500.0;
        const MAX_MIRED_SHIFT: f64 = 150.0;
        let as_shot_k = get_attr_as_f64(&attrs, "AsShotTemperature").unwrap_or(AS_SHOT_DEFAULT);
        let mired_adjusted = 1_000_000.0 / adjusted_k;
        let mired_as_shot = 1_000_000.0 / as_shot_k;
        let mired_delta = mired_adjusted - mired_as_shot;
        let temp_value = (-mired_delta / MAX_MIRED_SHIFT) * 100.0;
        adjustments.insert(
            "temperature".to_string(),
            json!(temp_value.clamp(-100.0, 100.0)),
        );
    }

    if let Some(tint_val) = get_attr_as_f64(&attrs, "Tint") {
        let scaled_tint = (tint_val / 150.0) * 100.0;
        adjustments.insert("tint".to_string(), json!(scaled_tint.clamp(-100.0, 100.0)));
    }

    let colors = [
        ("Red", "reds"),
        ("Orange", "oranges"),
        ("Yellow", "yellows"),
        ("Green", "greens"),
        ("Aqua", "aquas"),
        ("Blue", "blues"),
        ("Purple", "purples"),
        ("Magenta", "magentas"),
    ];
    for (src, dst) in colors {
        let mut color_map = Map::new();
        if let Some(raw) = attrs.get(&format!("HueAdjustment{}", src))
            && let Some(num) = parse_num(raw.trim_start_matches('+'))
            && let Some(Value::Number(n)) = num_to_json(num)
            && let Some(val_f64) = n.as_f64()
        {
            let adjusted_hue = val_f64 * 0.75;
            color_map.insert("hue".to_string(), json!(adjusted_hue));
        }
        if let Some(raw) = attrs.get(&format!("SaturationAdjustment{}", src))
            && let Some(num) = parse_num(raw.trim_start_matches('+'))
            && let Some(json_val) = num_to_json(num)
        {
            color_map.insert("saturation".to_string(), json_val);
        }
        if let Some(raw) = attrs.get(&format!("LuminanceAdjustment{}", src))
            && let Some(num) = parse_num(raw.trim_start_matches('+'))
            && let Some(json_val) = num_to_json(num)
        {
            color_map.insert("luminance".to_string(), json_val);
        }
        if !color_map.is_empty() {
            hsl_map.insert(dst.to_string(), Value::Object(color_map));
        }
    }
    if !hsl_map.is_empty() {
        adjustments.insert("hsl".to_string(), Value::Object(hsl_map));
    }

    let mut shadows_map = Map::new();
    let mut midtones_map = Map::new();
    let mut highlights_map = Map::new();
    let mut global_map = Map::new();
    if let Some(raw) = attrs.get("SplitToningShadowHue")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        shadows_map.insert("hue".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeMidtoneHue")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        midtones_map.insert("hue".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("SplitToningHighlightHue")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        highlights_map.insert("hue".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("SplitToningShadowSaturation")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        shadows_map.insert("saturation".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeMidtoneSat")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        midtones_map.insert("saturation".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("SplitToningHighlightSaturation")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        highlights_map.insert("saturation".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeShadowLum")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        shadows_map.insert("luminance".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeMidtoneLum")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        midtones_map.insert("luminance".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeHighlightLum")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        highlights_map.insert("luminance".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeGlobalHue")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        global_map.insert("hue".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeGlobalSat")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        global_map.insert("saturation".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("ColorGradeGlobalLum")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        global_map.insert("luminance".to_string(), json_val);
    }
    if let Some(raw) = attrs.get("SplitToningBalance")
        && let Some(num) = parse_num(raw)
        && let Some(json_val) = num_to_json(num)
    {
        color_grading_map.insert("balance".to_string(), json_val);
    }
    if !shadows_map.is_empty() {
        color_grading_map.insert("shadows".to_string(), Value::Object(shadows_map));
    }
    if !midtones_map.is_empty() {
        color_grading_map.insert("midtones".to_string(), Value::Object(midtones_map));
    }
    if !highlights_map.is_empty() {
        color_grading_map.insert("highlights".to_string(), Value::Object(highlights_map));
    }
    if !global_map.is_empty() {
        color_grading_map.insert("global".to_string(), Value::Object(global_map));
    }
    // Camera Raw renders split toning without a ColorGradeBlending value
    // (presets from before colour grading) at blending 100, not at the 50
    // new edits start from (measured, notes: reference/color-grading.md).
    if !color_grading_map.is_empty() && !color_grading_map.contains_key("blending") {
        color_grading_map.insert("blending".to_string(), json!(100));
    }
    if !color_grading_map.is_empty() {
        adjustments.insert("colorGrading".to_string(), Value::Object(color_grading_map));
    }

    // Post-crop vignette style (1 highlight priority, 2 colour priority,
    // 3 paint overlay) and Highlights.
    if let Some(style) = get_attr_as_f64(&attrs, "PostCropVignetteStyle") {
        let name = match style as i64 {
            2 => "colorPriority",
            3 => "paintOverlay",
            _ => "highlightPriority",
        };
        adjustments.insert("vignetteStyle".to_string(), json!(name));
    }
    if let Some(value) = get_attr_as_f64(&attrs, "PostCropVignetteHighlightContrast") {
        adjustments.insert(
            "vignetteHighlights".to_string(),
            json!(value.clamp(0.0, 100.0)),
        );
    }

    // Lightroom's region curve (parametric tone curve) maps 1:1.
    let mut region = Map::new();
    for (src, dst) in [
        ("ParametricShadows", "shadows"),
        ("ParametricDarks", "darks"),
        ("ParametricLights", "lights"),
        ("ParametricHighlights", "highlights"),
        ("ParametricShadowSplit", "shadowSplit"),
        ("ParametricMidtoneSplit", "midtoneSplit"),
        ("ParametricHighlightSplit", "highlightSplit"),
    ] {
        if let Some(value) = get_attr_as_f64(&attrs, src) {
            region.insert(dst.to_string(), json!(value));
        }
    }
    if !region.is_empty() {
        adjustments.insert("regionCurve".to_string(), Value::Object(region));
    }

    // Black & white: Lightroom's treatment and B&W mix map 1:1.
    if attrs
        .get("ConvertToGrayscale")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        adjustments.insert("blackAndWhite".to_string(), json!(true));
    }
    // Lightroom's Remove Chromatic Aberration: Rawtonic's measured one.
    if let Some(v) = attrs.get("AutoLateralCA") {
        adjustments.insert(
            "autoChromaticAberration".to_string(),
            json!(v.trim() == "1"),
        );
    }
    let mut gray_mixer = Map::new();
    for (src, _) in colors {
        if let Some(value) = get_attr_as_f64(&attrs, &format!("GrayMixer{}", src)) {
            gray_mixer.insert(src.to_lowercase(), json!(value.clamp(-100.0, 100.0)));
        }
    }
    if !gray_mixer.is_empty() {
        adjustments.insert("grayMixer".to_string(), Value::Object(gray_mixer));
    }

    let curve_mappings = [
        ("ToneCurvePV2012", "luma"),
        ("ToneCurvePV2012Red", "red"),
        ("ToneCurvePV2012Green", "green"),
        ("ToneCurvePV2012Blue", "blue"),
    ];
    for (xmp_curve, rr_curve) in curve_mappings {
        if let Some(points) = extract_tone_curve_points(xmp_content, xmp_curve) {
            curves_map.insert(rr_curve.to_string(), Value::Array(points));
        }
    }
    if curves_map.values().any(|points| {
        points
            .as_array()
            .is_some_and(|p| p.len() > rawtonic_engine::point_curve::SHADER_CURVE_POINTS)
    }) {
        adjustments.insert(
            "curveVersion".to_string(),
            json!(rawtonic_engine::point_curve::CURVE_VERSION),
        );
    }
    if !curves_map.is_empty() {
        adjustments.insert("curves".to_string(), Value::Object(curves_map));
    }

    let ranges = point_color_ranges(&xmp_one_line);
    if !ranges.is_empty() {
        adjustments.insert("colorRanges".to_string(), Value::Array(ranges));
    }

    let (layers, skipped) = lightroom_masks(xmp_content);
    if !skipped.is_empty() {
        log::info!("Lightroom preset import: masks not translated {skipped:?}");
    }
    let (spots, removals) = lightroom_spots(xmp_content);
    if removals > 0 {
        log::info!(
            "Lightroom import: {removals} Remove areas not translated (Lightroom's own fill)"
        );
    }
    if !spots.is_empty() {
        adjustments.insert("spots".to_string(), Value::Array(spots));
    }
    let has_masks = !layers.is_empty();
    if has_masks {
        adjustments.insert("masks".to_string(), Value::Array(layers));
    }

    // Lightroom's sharpening sliders map 1:1 onto the response fitted to
    // Camera Raw at every amount and lightness (notes: reference/detail.md
    // "Sharpening response 3"); without the key the photo would read them
    // with response 1.
    // Layers' sharpening adds to the photo's amount, so it counts too.
    let layer_sharpens = adjustments
        .get("masks")
        .and_then(Value::as_array)
        .is_some_and(|layers| {
            layers.iter().any(|layer| {
                layer["adjustments"]["sharpness"]
                    .as_f64()
                    .is_some_and(|v| v != 0.0)
            })
        });
    if layer_sharpens
        || [
            "sharpness",
            "sharpenRadius",
            "sharpenDetail",
            "sharpenMasking",
        ]
        .iter()
        .any(|key| adjustments.contains_key(*key))
    {
        adjustments.insert(
            rawtonic_engine::image_processing::SHARPEN_VERSION_KEY.to_string(),
            json!(rawtonic_engine::image_processing::SHARPEN_VERSION),
        );
    }

    let preset_name =
        extract_xmp_name(xmp_content).unwrap_or_else(|| "Imported Preset".to_string());

    Ok(Preset {
        id: Uuid::new_v4().to_string(),
        name: preset_name,
        adjustments: Value::Object(adjustments),
        include_masks: Some(has_masks),
        include_crop_transform: Some(false),
        preset_type: Some("style".to_string()),
    })
}

/// Lightroom's Heal and Clone spots and strokes (`crs:RetouchAreas`, as
/// Lightroom Classic 15.6 writes them; older files only have the
/// `crs:RetouchInfo` strings) as Rawtonic spots, kept normalised
/// (`lrNormalized`) until they are placed in a photo. A spot is a
/// `Mask/Ellipse` (centre X/Y, radius SizeX as a share of the unrotated
/// width) or a `Mask/Paint` stroke (dabs, Radius); `SourceX`/`OffsetY` is
/// where its centre or first dab copies from. Remove areas
/// (`heal_patchmatch`) carry Lightroom's own fill and are counted, not
/// translated.
pub fn lightroom_spots(xmp: &str) -> (Vec<Value>, usize) {
    let Ok(doc) = roxmltree::Document::parse(xmp) else {
        return (Vec::new(), 0);
    };
    let mut spots = Vec::new();
    let mut removals = 0;
    if let Some(areas) = doc
        .descendants()
        .find(|n| n.has_tag_name((CRS_NS, "RetouchAreas")))
    {
        for li in seq_items_of(areas) {
            let area = described(li);
            let attr = |n: &str| area.attribute((CRS_NS, n));
            let num = |n: &str| attr(n).and_then(|v| v.parse::<f64>().ok());
            let mode = match attr("SpotType") {
                Some("heal") => "heal",
                Some("clone") => "clone",
                Some(_) => {
                    removals += 1;
                    continue;
                }
                None => continue,
            };
            let (Some(source_x), Some(source_y)) = (num("SourceX"), num("OffsetY")) else {
                continue;
            };
            let Some(mask) = seq_items(area, "Masks").first().map(|li| described(*li)) else {
                continue;
            };
            let mnum = |n: &str| {
                mask.attribute((CRS_NS, n))
                    .and_then(|v| v.parse::<f64>().ok())
            };
            let (x, y, radius, points) = match mask.attribute((CRS_NS, "What")) {
                Some("Mask/Ellipse") => match (mnum("X"), mnum("Y"), mnum("SizeX")) {
                    (Some(x), Some(y), Some(r)) => (x, y, r, Vec::new()),
                    _ => continue,
                },
                Some("Mask/Paint") => {
                    let dabs: Vec<[f64; 2]> = seq_items(mask, "Dabs")
                        .iter()
                        .filter_map(|li| {
                            let mut parts = li.text()?.split_whitespace();
                            matches!(parts.next(), Some("d" | "M")).then_some(())?;
                            Some([parts.next()?.parse().ok()?, parts.next()?.parse().ok()?])
                        })
                        .collect();
                    let (Some(first), Some(r)) = (dabs.first().copied(), mnum("Radius")) else {
                        continue;
                    };
                    (first[0], first[1], r, dabs)
                }
                _ => continue,
            };
            spots.push(json!({
                "mode": mode,
                "opacity": (num("Opacity").unwrap_or(1.0) * 100.0).clamp(0.0, 100.0),
                "feather": (num("Feather").unwrap_or(0.0) * 100.0).clamp(0.0, 100.0),
                "lrNormalized": {"x": x, "y": y, "radius": radius, "sourceX": source_x,
                    "sourceY": source_y, "points": points},
            }));
        }
    } else if let Some(info) = doc
        .descendants()
        .find(|n| n.has_tag_name((CRS_NS, "RetouchInfo")))
    {
        // "centerX = 0.2, centerY = 0.8, radius = 0.017, sourceState = …,
        // sourceX = 0.2, sourceY = 0.7, spotType = clone"
        for li in seq_items_of(info) {
            let text = li.text().unwrap_or("");
            let field = |key: &str| {
                text.split(',').find_map(|part| {
                    let (k, v) = part.split_once('=')?;
                    (k.trim() == key).then(|| v.trim().to_string())
                })
            };
            let num = |key: &str| field(key).and_then(|v| v.parse::<f64>().ok());
            let mode = match field("spotType").as_deref() {
                Some("clone") => "clone",
                Some("heal") => "heal",
                Some(_) => {
                    removals += 1;
                    continue;
                }
                None => continue,
            };
            if let (Some(x), Some(y), Some(r), Some(sx), Some(sy)) = (
                num("centerX"),
                num("centerY"),
                num("radius"),
                num("sourceX"),
                num("sourceY"),
            ) {
                spots.push(json!({"mode": mode, "opacity": 100.0, "feather": 0.0,
                    "lrNormalized": {"x": x, "y": y, "radius": r, "sourceX": sx, "sourceY": sy,
                        "points": []}}));
            }
        }
    }
    (spots, removals)
}

/// The rdf:li items of the rdf:Seq directly inside `parent`.
fn seq_items_of<'a, 'i>(parent: roxmltree::Node<'a, 'i>) -> Vec<roxmltree::Node<'a, 'i>> {
    parent
        .children()
        .find(|c| c.has_tag_name((RDF_NS, "Seq")))
        .map(|seq| {
            seq.children()
                .filter(|c| c.has_tag_name((RDF_NS, "li")))
                .collect()
        })
        .unwrap_or_default()
}

/// EXIF orientation applied to a point in normalised coordinates of the
/// unrotated raw (where Lightroom keeps its mask geometry): the point in
/// the displayed photo, which Rawtonic's masks use.
fn orient_point(orientation: u16, x: f64, y: f64) -> (f64, f64) {
    match orientation {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (1.0 - y, x),
        7 => (1.0 - y, 1.0 - x),
        8 => (y, 1.0 - x),
        _ => (x, y),
    }
}

/// `orient_point` for a direction (pixel space).
fn orient_direction(orientation: u16, dx: f64, dy: f64) -> (f64, f64) {
    match orientation {
        2 => (-dx, dy),
        3 => (-dx, -dy),
        4 => (dx, -dy),
        5 => (dy, dx),
        6 => (-dy, dx),
        7 => (-dy, -dx),
        8 => (dy, -dx),
        _ => (dx, dy),
    }
}

/// Lightroom's brush flow as the coverage of one dab: Camera Raw renders a
/// single hard dab at Flow 0.5 with 0.105 and at 0.9 with 0.53, which
/// `f / (8.12 − 7.12 f)` matches within 0.004 over 0.05–1.
fn lightroom_dab_coverage(flow: f64) -> f64 {
    let f = flow.clamp(0.0, 1.0);
    f / (8.12 - 7.12 * f)
}

/// Places Lightroom mask geometry kept normalised (`lrNormalized`, see
/// `lightroom_masks`) in a photo of `width` × `height` coordinate pixels
/// (the displayed orientation) whose EXIF orientation is `orientation`.
/// Lightroom keeps its geometry in the unrotated raw (Camera Raw puts a dab
/// at raw (0.3, 0.5) of an orientation-8 photo at (0.5, 0.7)). Returns
/// whether anything was placed.
pub fn place_lightroom_geometry(
    adjustments: &mut Value,
    width: f64,
    height: f64,
    orientation: u16,
) -> bool {
    if !(width > 0.0 && height > 0.0) {
        return false;
    }
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let pixel = |x: f64, y: f64| {
        let (u, v) = orient_point(orientation, x, y);
        (u * width, v * height)
    };
    let mut placed = place_lightroom_spots(adjustments, width, height, orientation);
    // Lightroom's geometry belongs to the photo, not to how it is shown: the
    // placed shapes record the photo's own frame (`mask_generation`'s
    // `ShapeFrame`), so they are drawn on the same spot of the photo however
    // it is turned, flipped or straightened.
    let frame = json!({"width": width, "height": height, "rotation": 0.0,
        "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0});
    let Some(masks) = adjustments.get_mut("masks").and_then(Value::as_array_mut) else {
        return placed;
    };
    for sub in masks
        .iter_mut()
        .filter_map(|m| m.get_mut("subMasks").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(g) = sub["parameters"].get("lrNormalized").cloned() else {
            continue;
        };
        let n = |k: &str| g[k].as_f64().unwrap_or(0.0);
        let parameters = match sub["type"].as_str() {
            Some("linear") => {
                let (zx, zy) = pixel(n("zeroX"), n("zeroY"));
                let (fx, fy) = pixel(n("fullX"), n("fullY"));
                let (dx, dy) = (fx - zx, fy - zy);
                let length = dx.hypot(dy);
                if length <= 1.0 {
                    continue;
                }
                let (mx, my) = ((zx + fx) / 2.0, (zy + fy) / 2.0);
                let (lx, ly) = (-dy / 2.0, dx / 2.0);
                json!({"startX": round(mx - lx), "startY": round(my - ly),
                    "endX": round(mx + lx), "endY": round(my + ly), "range": round(length / 2.0),
                    "frame": frame})
            }
            Some("radial") => {
                // Top/Left and Bottom/Right are opposite corners of the
                // ellipse's rectangle turned by Angle (clockwise): the
                // semi-axes are the diagonal's projections on its axes
                // (Camera Raw at 15°/30°/45° matches this in pixels).
                let (x1, y1) = pixel(n("left"), n("top"));
                let (x2, y2) = pixel(n("right"), n("bottom"));
                let angle = g["angle"].as_f64().unwrap_or(0.0).to_radians();
                let (ux, uy) = orient_direction(orientation, angle.cos(), angle.sin());
                let (dx, dy) = (x2 - x1, y2 - y1);
                json!({
                    "centerX": round((x1 + x2) / 2.0),
                    "centerY": round((y1 + y2) / 2.0),
                    "radiusX": round((dx * ux + dy * uy).abs() / 2.0),
                    "radiusY": round((-dx * uy + dy * ux).abs() / 2.0),
                    "rotation": round(uy.atan2(ux).to_degrees()),
                    "feather": (g["feather"].as_f64().unwrap_or(50.0) / 100.0).clamp(0.0, 1.0),
                    "frame": frame,
                })
            }
            Some("brush") => {
                // The radius is a share of the long side (Camera Raw: 0.05 →
                // 60 px on a 1200 px long side, in either orientation).
                let long = width.max(height);
                let lines: Vec<Value> = g["strokes"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|stroke| {
                        let points: Vec<Value> = stroke["dabs"]
                            .as_array()?
                            .iter()
                            .filter_map(|d| {
                                let (x, y) = pixel(d[0].as_f64()?, d[1].as_f64()?);
                                Some(json!({"x": round(x), "y": round(y)}))
                            })
                            .collect();
                        let radius = stroke["radius"].as_f64()? * long;
                        (!points.is_empty() && radius > 0.0).then(|| {
                            json!({
                                "tool": "brush",
                                "brushSize": round(2.0 * radius),
                                "points": points,
                                "feather": 0.5,
                                "flow": round(
                                    lightroom_dab_coverage(stroke["flow"].as_f64().unwrap_or(1.0)) * 100.0,
                                ),
                                "lrCenterWeight": stroke["centerWeight"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0),
                                "autoMask": stroke["autoMask"].as_bool().unwrap_or(false),
                                "frame": frame,
                            })
                        })
                    })
                    .collect();
                json!({"lines": lines})
            }
            Some("color") => {
                let mut samples: Vec<Value> = g["points"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|p| {
                        let (x, y) = pixel(p[0].as_f64()?, p[1].as_f64()?);
                        Some(json!({"x": round(x), "y": round(y)}))
                    })
                    .collect();
                for corners in g["areas"].as_array().map(Vec::as_slice).unwrap_or_default() {
                    let placed: Vec<(f64, f64)> = (0..4)
                        .filter_map(|i| {
                            Some(pixel(
                                corners[2 * i].as_f64()?,
                                corners[2 * i + 1].as_f64()?,
                            ))
                        })
                        .collect();
                    if placed.len() == 4 {
                        let (x0, x1) = placed
                            .iter()
                            .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
                        let (y0, y1) = placed
                            .iter()
                            .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
                        samples.push(json!({"x": round(x0), "y": round(y0),
                            "width": round(x1 - x0), "height": round(y1 - y0)}));
                    }
                }
                samples.truncate(5);
                let Some(first) = samples.first().cloned() else {
                    continue;
                };
                let center = |s: &Value, k: &str, size: &str| {
                    s[k].as_f64().unwrap_or(0.0) + s[size].as_f64().unwrap_or(0.0) / 2.0
                };
                json!({
                    "targetX": round(center(&first, "x", "width")),
                    "targetY": round(center(&first, "y", "height")),
                    "samples": samples,
                    "refine": g["refine"].as_f64().unwrap_or(50.0),
                    "feather": 0.0,
                })
            }
            _ => continue,
        };
        sub["parameters"] = parameters;
        placed = true;
    }
    placed
}

/// `place_lightroom_geometry` for spots: Lightroom's radius is a share of
/// the unrotated width, its points normalised in the unrotated raw.
fn place_lightroom_spots(
    adjustments: &mut Value,
    width: f64,
    height: f64,
    orientation: u16,
) -> bool {
    let Some(spots) = adjustments.get_mut("spots").and_then(Value::as_array_mut) else {
        return false;
    };
    let unrotated_width = if (5..=8).contains(&orientation) {
        height
    } else {
        width
    };
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let pixel = |x: f64, y: f64| {
        let (u, v) = orient_point(orientation, x, y);
        (round(u * width), round(v * height))
    };
    let mut placed = false;
    for spot in spots.iter_mut() {
        let Some(g) = spot.get("lrNormalized").cloned() else {
            continue;
        };
        let n = |k: &str| g[k].as_f64().unwrap_or(0.0);
        let (x, y) = pixel(n("x"), n("y"));
        let (source_x, source_y) = pixel(n("sourceX"), n("sourceY"));
        let points: Vec<Value> = g["points"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                let (px, py) = pixel(p[0].as_f64()?, p[1].as_f64()?);
                Some(json!({"x": px, "y": py}))
            })
            .collect();
        let object = spot.as_object_mut().expect("spots are objects");
        object.remove("lrNormalized");
        object.insert("id".into(), json!(Uuid::new_v4().to_string()));
        object.insert("x".into(), json!(x));
        object.insert("y".into(), json!(y));
        object.insert("radius".into(), json!(round(n("radius") * unrotated_width)));
        object.insert("sourceX".into(), json!(source_x));
        object.insert("sourceY".into(), json!(source_y));
        object.insert("visible".into(), json!(true));
        if points.len() > 1 {
            object.insert("points".into(), Value::Array(points));
        }
        placed = true;
    }
    placed
}

/// Places an imported preset's Lightroom geometry in the open photo
/// (`width` × `height` coordinate pixels) when it is shown, with the
/// photo's EXIF orientation read from the file.
#[tauri::command]
pub async fn place_lightroom_masks(
    path: String,
    mut adjustments: Value,
    width: f64,
    height: f64,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        let bytes = std::fs::read(&source).map_err(|e| e.to_string())?;
        let orientation =
            crate::image_loader::source_orientation(&bytes, &source.to_string_lossy());
        place_lightroom_geometry(&mut adjustments, width, height, orientation);
        Ok(adjustments)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Lightroom's brush strokes of a `Mask/Paint` component: its Dabs list
/// ("d x y" dab positions in the unrotated raw, normalised; "M x y" starts a
/// stroke; "r", "f", "h" change radius, flow and CenterWeight, the names
/// Camera Raw's reader uses) grouped into strokes of equal settings.
fn lightroom_strokes(mask: roxmltree::Node) -> Vec<Value> {
    let attr = |n: &str| {
        mask.attribute((CRS_NS, n))
            .and_then(|v| v.parse::<f64>().ok())
    };
    let (mut radius, mut flow, mut center_weight) = (
        attr("Radius").unwrap_or(0.05),
        attr("Flow").unwrap_or(1.0),
        attr("CenterWeight").unwrap_or(0.0),
    );
    let mut strokes = Vec::new();
    let mut dabs: Vec<Value> = Vec::new();
    // "M x y" is a dab with Auto Mask ("d" without): Lightroom stores only
    // the positions and Camera Raw limits each such dab to colours like the
    // one under its centre (notes: reference/auto-mask.md).
    let mut auto_mask = false;
    let flush =
        |dabs: &mut Vec<Value>, strokes: &mut Vec<Value>, r: f64, f: f64, c: f64, a: bool| {
            if !dabs.is_empty() {
                strokes.push(json!({"dabs": std::mem::take(dabs), "radius": r, "flow": f,
                "centerWeight": c, "autoMask": a}));
            }
        };
    for item in seq_items(mask, "Dabs") {
        let text = item.text().unwrap_or("");
        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or("");
        let values: Vec<f64> = parts.filter_map(|v| v.parse().ok()).collect();
        match (command, values.as_slice()) {
            ("d" | "M", [x, y]) => {
                let masked = command == "M";
                if masked != auto_mask {
                    flush(
                        &mut dabs,
                        &mut strokes,
                        radius,
                        flow,
                        center_weight,
                        auto_mask,
                    );
                    auto_mask = masked;
                }
                dabs.push(json!([x, y]));
            }
            ("r", [v]) | ("f", [v]) | ("h", [v]) => {
                flush(
                    &mut dabs,
                    &mut strokes,
                    radius,
                    flow,
                    center_weight,
                    auto_mask,
                );
                match command {
                    "r" => radius = *v,
                    "f" => flow = *v,
                    _ => center_weight = *v,
                }
            }
            _ => {}
        }
    }
    flush(
        &mut dabs,
        &mut strokes,
        radius,
        flow,
        center_weight,
        auto_mask,
    );
    strokes
}

/// Lightroom's Luminance Range of a `Mask/RangeMask` component (Type 2) as
/// its zero, full, full, zero points (0–1); `None` for colour and depth
/// ranges.
fn lightroom_luminance_range(mask: roxmltree::Node) -> Option<[f64; 4]> {
    // Lightroom writes the attributes on an rdf:Description inside.
    let range = described(
        mask.children()
            .find(|c| c.has_tag_name((CRS_NS, "CorrectionRangeMask")))?,
    );
    let attr = |n: &str| range.attribute((CRS_NS, n));
    if attr("Type") != Some("2") {
        return None;
    }
    if let Some(points) = attr("LumRange") {
        let v: Vec<f64> = points
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect();
        if let [a, b, c, d] = v.as_slice() {
            return Some([*a, *b, *c, *d]);
        }
    }
    // Older files: a range and its smoothness (not measured here; the
    // smoothness widens the range by half of itself on either side).
    let num = |n: &str| attr(n).and_then(|v| v.parse::<f64>().ok());
    let (low, high) = (num("LumMin")?, num("LumMax")?);
    let soft = num("LumFeather").unwrap_or(0.5) * (high - low) / 2.0;
    Some([(low - soft).max(0.0), low, high, (high + soft).min(1.0)])
}

/// Lightroom's Color Range of a `Mask/RangeMask` component (Type 1): its
/// samples' positions in the unrotated raw (normalised; `PointModels`
/// "L a b x y flag", whose colour is relative to the photo's own spread
/// and is sampled again from Rawtonic's rendering instead), area samples as
/// their rectangles (`ColorRangeMaskAreaSampleInfo` "cx cy" then four
/// corners), and Refine from `ColorAmount` = (Refine − 1) / 99 (Lightroom
/// writes 0.4949 for 50; notes: reference/color-range.md).
fn lightroom_color_range(mask: roxmltree::Node) -> Option<Value> {
    let range = mask
        .children()
        .find(|c| c.has_tag_name((CRS_NS, "CorrectionRangeMask")))?;
    let range = described(range);
    let attr = |n: &str| range.attribute((CRS_NS, n));
    if attr("Type") != Some("1") {
        return None;
    }
    let numbers = |text: &str| -> Vec<f64> {
        text.split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let points: Vec<Value> = seq_items(range, "PointModels")
        .iter()
        .filter_map(|li| {
            let v = numbers(li.text()?);
            (v.len() >= 5).then(|| json!([v[3], v[4]]))
        })
        .collect();
    let areas: Vec<Value> = seq_items(range, "AreaModels")
        .iter()
        .filter_map(|li| {
            let v = numbers(described(*li).attribute((CRS_NS, "ColorRangeMaskAreaSampleInfo"))?);
            (v.len() >= 10).then(|| json!(v[2..10].to_vec()))
        })
        .collect();
    if points.is_empty() && areas.is_empty() {
        return None;
    }
    let amount = attr("ColorAmount")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    Some(json!({"points": points, "areas": areas,
        "refine": ((1.0 + 99.0 * amount) * 100.0).round() / 100.0}))
}

const CRS_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The rdf:Description inside an rdf:li, or the li when it carries the
/// settings itself.
fn described<'a, 'i>(li: roxmltree::Node<'a, 'i>) -> roxmltree::Node<'a, 'i> {
    li.children()
        .find(|c| c.has_tag_name((RDF_NS, "Description")))
        .unwrap_or(li)
}

/// The rdf:li items of a crs:NAME/rdf:Seq child.
fn seq_items<'a, 'i>(parent: roxmltree::Node<'a, 'i>, name: &str) -> Vec<roxmltree::Node<'a, 'i>> {
    parent
        .children()
        .find(|c| c.has_tag_name((CRS_NS, name)))
        .and_then(|n| n.children().find(|c| c.has_tag_name((RDF_NS, "Seq"))))
        .map(|seq| {
            seq.children()
                .filter(|c| c.has_tag_name((RDF_NS, "li")))
                .collect()
        })
        .unwrap_or_default()
}

/// Lightroom's masks (`crs:MaskGroupBasedCorrections`) as Rawtonic layers.
/// Supported: Select Subject (`Mask/Image` subtype 1, Rawtonic's automatic
/// subject, `ai-foreground`) and Select Sky (subtype 2, `ai-sky`), whose
/// mask data is computed when a photo with them opens, and linear and
/// radial gradients and brush strokes, kept normalised (`lrNormalized`)
/// until they are placed in a photo's pixels, and Luminance Ranges (in the
/// photo's luminance percentiles, as Camera Raw selects). The local
/// settings map as Lightroom's global ones do, scaled by the mask's Amount;
/// local tone curves and Point Color come along. Corrections with other
/// masks (people parts, colour and depth ranges) are left out and named in
/// the second result.
pub fn lightroom_masks(xmp: &str) -> (Vec<Value>, Vec<String>) {
    let mut layers = Vec::new();
    let mut skipped = Vec::new();
    let Ok(doc) = roxmltree::Document::parse(xmp) else {
        return (layers, skipped);
    };
    let Some(group) = doc
        .descendants()
        .find(|n| n.has_tag_name((CRS_NS, "MaskGroupBasedCorrections")))
    else {
        return (layers, skipped);
    };
    let Some(seq) = group.children().find(|c| c.has_tag_name((RDF_NS, "Seq"))) else {
        return (layers, skipped);
    };
    for (index, li) in seq
        .children()
        .filter(|c| c.has_tag_name((RDF_NS, "li")))
        .enumerate()
    {
        let correction = described(li);
        let attr = |name: &str| correction.attribute((CRS_NS, name));
        let num =
            |name: &str| attr(name).and_then(|v| v.trim_start_matches('+').parse::<f64>().ok());
        let name = attr("CorrectionName")
            .map(str::to_string)
            .unwrap_or_else(|| format!("Mask {}", index + 1));
        let mut sub_masks = Vec::new();
        let mut supported = true;
        for (m_index, mask_li) in seq_items(correction, "CorrectionMasks")
            .into_iter()
            .enumerate()
        {
            let mask = described(mask_li);
            let mattr = |n: &str| mask.attribute((CRS_NS, n));
            let mnum = |n: &str| mattr(n).and_then(|v| v.parse::<f64>().ok());
            // Geometric masks keep Lightroom's normalised geometry until a
            // photo opens (Rawtonic's mask coordinates are its pixels).
            let (kind, parameters) = match (mattr("What"), mattr("MaskSubType")) {
                (Some("Mask/Image"), Some("1")) => (
                    "ai-foreground",
                    json!({"maskDataBase64": null, "grow": 0, "feather": 0}),
                ),
                (Some("Mask/Image"), Some("2")) => (
                    "ai-sky",
                    json!({"maskDataBase64": null, "grow": 0, "feather": 0}),
                ),
                (Some("Mask/Gradient"), _) => {
                    match (mnum("ZeroX"), mnum("ZeroY"), mnum("FullX"), mnum("FullY")) {
                        (Some(zx), Some(zy), Some(fx), Some(fy)) => (
                            "linear",
                            json!({"lrNormalized": {"zeroX": zx, "zeroY": zy, "fullX": fx, "fullY": fy}}),
                        ),
                        _ => {
                            supported = false;
                            break;
                        }
                    }
                }
                (Some("Mask/Paint"), _) => {
                    let strokes = lightroom_strokes(mask);
                    if strokes.is_empty() {
                        supported = false;
                        break;
                    }
                    ("brush", json!({"lrNormalized": {"strokes": strokes}}))
                }
                (Some("Mask/RangeMask"), _) => match lightroom_luminance_range(mask) {
                    Some(range) => (
                        "luma-range",
                        json!({"rangeLow": range[1] * 100.0, "rangeHigh": range[2] * 100.0,
                            "falloff": 10.0, "lrRange": range}),
                    ),
                    None => match lightroom_color_range(mask) {
                        Some(color) => ("color", json!({"lrNormalized": color})),
                        None => {
                            supported = false;
                            break;
                        }
                    },
                },
                (Some("Mask/CircularGradient"), _) => {
                    match (mnum("Top"), mnum("Left"), mnum("Bottom"), mnum("Right")) {
                        (Some(top), Some(left), Some(bottom), Some(right)) => (
                            "radial",
                            json!({"lrNormalized": {
                                "top": top, "left": left, "bottom": bottom, "right": right,
                                "angle": mnum("Angle").unwrap_or(0.0),
                                "feather": mnum("Feather").unwrap_or(50.0),
                            }}),
                        ),
                        _ => {
                            supported = false;
                            break;
                        }
                    }
                }
                _ => {
                    supported = false;
                    break;
                }
            };
            let mode = match mattr("MaskBlendMode") {
                Some("1") => "subtractive",
                Some("2") => "intersect",
                _ => "additive",
            };
            let mut value = mattr("MaskValue")
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            // Lightroom's eraser paints MaskValue 0 strokes.
            let mode = if kind == "brush" && value <= 0.0 {
                value = 1.0;
                "subtractive"
            } else {
                mode
            };
            sub_masks.push(json!({
                "id": format!("lr-{index}-{m_index}-{}", Uuid::new_v4()),
                "type": kind,
                "visible": mattr("MaskActive") != Some("false"),
                "invert": mattr("MaskInverted") == Some("true"),
                "opacity": (value * 100.0).clamp(0.0, 100.0),
                "mode": mode,
                // AI masks are computed, geometry placed, when a photo opens.
                "parameters": parameters,
            }));
        }
        if !supported || sub_masks.is_empty() {
            skipped.push(name);
            continue;
        }
        // Lightroom's Amount scales every local setting.
        let amount = num("CorrectionAmount").unwrap_or(1.0).clamp(0.0, 2.0);
        let mut adjustments = Map::new();
        for (xmp_key, key, scale) in [
            // Stored as EV / 4 (the ±4 EV slider as ±1): Camera Raw renders
            // 0.5 as exactly 2 EV, and Adobe's presets hold 0.0875 for 0.35 EV.
            ("LocalExposure2012", "exposure", 4.0),
            ("LocalContrast2012", "contrast", 100.0),
            ("LocalHighlights2012", "highlights", 100.0),
            ("LocalShadows2012", "shadows", 100.0),
            ("LocalWhites2012", "whites", 100.0),
            ("LocalBlacks2012", "blacks", 100.0),
            ("LocalClarity2012", "clarity", 100.0),
            ("LocalDehaze", "dehaze", 100.0),
            ("LocalTexture", "structure", 100.0),
            ("LocalSaturation", "saturation", 100.0),
            ("LocalTemperature", "temperature", 100.0),
            ("LocalTint", "tint", 100.0),
            // ±1 is Lightroom's ±180° slider (Camera Raw turns 1.0 by ~172°
            // in Oklab; notes: reference/local-hue.md).
            ("LocalHue", "hue", 180.0),
            ("LocalSharpness", "sharpness", 100.0),
            ("LocalLuminanceNoise", "lumaNoiseReduction", 100.0),
            ("LocalMoire", "moireAmount", 100.0),
            // ±1 = ±100; above 0 fringes go, below 0 the photo's Defringe
            // is held back (notes: reference/local-defringe.md).
            ("LocalDefringe", "defringe", 100.0),
        ] {
            if let Some(v) = num(xmp_key).filter(|v| *v != 0.0) {
                let limit = match key {
                    "exposure" => 5.0,
                    "hue" => 180.0,
                    _ => 100.0,
                };
                let value = (v * scale * amount).clamp(-limit, limit);
                adjustments.insert(key.to_string(), json!((value * 100.0).round() / 100.0));
            }
        }
        if let Some(sat) = num("LocalToningSaturation").filter(|v| *v > 0.0) {
            adjustments.insert(
                "colorGrading".into(),
                json!({"global": {
                    "hue": num("LocalToningHue").unwrap_or(0.0).rem_euclid(360.0),
                    "saturation": (sat * 100.0 * amount).clamp(0.0, 100.0).round(),
                    "luminance": 0.0,
                }}),
            );
        }
        let mut curves = Map::new();
        for (xmp_curve, channel) in [
            ("MainCurve", "luma"),
            ("RedCurve", "red"),
            ("GreenCurve", "green"),
            ("BlueCurve", "blue"),
        ] {
            let points: Vec<Value> = seq_items(correction, xmp_curve)
                .into_iter()
                .filter_map(|p| {
                    let (x, y) = p.text()?.split_once(',')?;
                    let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
                    Some(json!({"x": x, "y": (x + amount * (y - x)).clamp(0.0, 255.0).round()}))
                })
                .collect();
            let identity = points.len() == 2
                && points[0] == json!({"x": 0.0, "y": 0.0})
                && points[1] == json!({"x": 255.0, "y": 255.0});
            if points.len() >= 2 && !identity {
                curves.insert(channel.into(), Value::Array(points));
            }
        }
        if !curves.is_empty() {
            let identity = json!([{"x": 0, "y": 0}, {"x": 255, "y": 255}]);
            let mut full =
                json!({"luma": identity, "red": identity, "green": identity, "blue": identity});
            for (k, v) in curves {
                full[k] = v;
            }
            adjustments.insert("curves".into(), full);
        }
        let swatches: Vec<String> = seq_items(correction, "LocalPointColors")
            .into_iter()
            .filter_map(|p| p.text().map(str::to_string))
            .collect();
        let ranges = point_color_ranges_from(&swatches, &[], 4);
        if !ranges.is_empty() {
            adjustments.insert("colorRanges".into(), Value::Array(ranges));
        }
        layers.push(json!({
            "id": format!("lr-mask-{}", Uuid::new_v4()),
            "name": name,
            "visible": attr("CorrectionActive") != Some("false"),
            "invert": false,
            "opacity": 100.0,
            "adjustments": Value::Object(adjustments),
            "subMasks": sub_masks,
        }));
    }
    (layers, skipped)
}

/// The profile or creative look a preset sets (`crs:Look`, else
/// `crs:CameraProfile`), unless it is Camera Raw's default rendering, which
/// Rawtonic's own base rendering stands in for. Only the name is read:
/// Adobe's profiles are Adobe's content and are not reproduced.
fn untranslated_profile(xmp: &str) -> Option<String> {
    let look = regex!(r#"<crs:Look\s[^>]*?crs:Name="([^"]*)""#)
        .captures(xmp)
        .or_else(|| {
            regex!(r#"(?s)<crs:Look>\s*<rdf:Description\s[^>]*?crs:Name="([^"]*)""#).captures(xmp)
        })
        .or_else(|| regex!(r#"crs:CameraProfile="([^"]*)""#).captures(xmp))
        .map(|c| {
            c[1].trim()
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        })?;
    let default = ["", "Adobe Color", "Adobe Standard", "Embedded"]
        .iter()
        .any(|d| look.eq_ignore_ascii_case(d));
    (!default).then_some(look)
}

/// Settings in a Lightroom preset that `convert_xmp_to_preset` does not
/// translate, as codes the import report names (`profile`, `calibration`,
/// `lensProfile`, `crop`, `upright`), and the profile's name. Settings left
/// at their defaults are not reported.
pub fn untranslated_settings(xmp_content: &str) -> (Vec<&'static str>, Option<String>) {
    let attrs: HashMap<String, String> = regex!(r#"crs:([A-Za-z0-9]+)="([^"]*)""#)
        .captures_iter(xmp_content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    let nonzero = |key: &str| get_attr_as_f64(&attrs, key).is_some_and(|v| v != 0.0);
    let is = |key: &str, value: &str| {
        attrs
            .get(key)
            .is_some_and(|v| v.eq_ignore_ascii_case(value))
    };
    let profile = untranslated_profile(xmp_content);
    let mut out = Vec::new();
    if profile.is_some() {
        out.push("profile");
    }
    if [
        "ShadowTint",
        "RedHue",
        "RedSaturation",
        "GreenHue",
        "GreenSaturation",
        "BlueHue",
        "BlueSaturation",
    ]
    .iter()
    .any(|k| nonzero(k))
    {
        out.push("calibration");
    }
    if is("LensProfileEnable", "1") {
        out.push("lensProfile");
    }
    if is("HasCrop", "True") {
        out.push("crop");
    }
    if nonzero("PerspectiveUpright") {
        out.push("upright");
    }
    (out, profile)
}

#[cfg(test)]
mod tests {
    /// Converts every preset listed in `XMP_PRESET_LIST` (one path per line)
    /// and counts what the import report names; checks that real presets
    /// convert. `cargo test -p rawtonic-app --lib converts_real_presets --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn converts_real_presets() {
        let list = std::fs::read_to_string(std::env::var("XMP_PRESET_LIST").unwrap()).unwrap();
        let (mut ok, mut failed) = (0, 0);
        let mut reports = std::collections::BTreeMap::<String, usize>::new();
        let mut profiles = std::collections::BTreeMap::<String, usize>::new();
        for path in list.lines().filter(|l| !l.is_empty()) {
            let Ok(xmp) = std::fs::read_to_string(path) else {
                continue;
            };
            match super::convert_xmp_to_preset(&xmp) {
                Ok(_) => ok += 1,
                Err(_) => failed += 1,
            }
            let (codes, profile) = super::untranslated_settings(&xmp);
            for code in codes {
                *reports.entry(code.to_string()).or_default() += 1;
            }
            if let Some(p) = profile {
                *profiles.entry(p).or_default() += 1;
            }
        }
        println!("converted {ok}, failed {failed}, untranslated {reports:?}");
        println!(
            "profiles {:?}",
            profiles.iter().take(12).collect::<Vec<_>>()
        );
    }

    fn rendered(curve: &[serde_json::Value], x: f64) -> f64 {
        let points: Vec<(f32, f32)> = curve
            .iter()
            .map(|p| {
                (
                    p["x"].as_f64().unwrap() as f32,
                    p["y"].as_f64().unwrap() as f32,
                )
            })
            .collect();
        rawtonic_engine::point_curve::curve_value(&points, (x / 255.0) as f32) as f64 * 255.0
    }

    #[test]
    fn lightroom_point_curves_follow_lightroom_spline() {
        let seq = |pts: &[(u32, u32)]| {
            let items: String = pts
                .iter()
                .map(|(x, y)| format!("<rdf:li>{x}, {y}</rdf:li>"))
                .collect();
            format!("<crs:ToneCurvePV2012><rdf:Seq>{items}</rdf:Seq></crs:ToneCurvePV2012>")
        };
        // Camera Raw, 2026-10-21 (neutral medians): 3-point 96 -> 125.3,
        // overshoot past (64,128)-(128,128): 96 -> 135.2, flat beyond moved ends.
        for (pts, x, camera_raw) in [
            (vec![(0, 0), (128, 160), (255, 255)], 96.0, 125.3),
            (vec![(0, 0), (64, 128), (128, 128), (255, 255)], 96.0, 135.2),
            (vec![(32, 0), (128, 128), (224, 255)], 16.0, 0.0),
            // A jump within one level: right at integers, off between them.
            (vec![(0, 0), (127, 0), (128, 255), (255, 255)], 64.0, -1.0),
        ] {
            let curve = super::extract_tone_curve_points(&seq(&pts), "ToneCurvePV2012").unwrap();
            for &(px, py) in &pts {
                assert!(
                    curve
                        .iter()
                        .any(|p| p["x"] == px as f64 && p["y"] == py as f64),
                    "{pts:?}"
                );
            }
            // Whole-level knots, sorted and at least one level apart, as the
            // curve editor keeps them.
            let xs: Vec<f64> = curve.iter().map(|p| p["x"].as_f64().unwrap()).collect();
            assert!(xs.iter().all(|x| x.fract() == 0.0), "{xs:?}");
            assert!(xs.windows(2).all(|w| w[1] - w[0] >= 1.0), "{xs:?}");
            if camera_raw < 0.0 {
                continue; // a jump within one level: no closer fit exists
            }
            // Within a quarter level at every eighth input level.
            for k in 0..=255 * 8 {
                let x = k as f64 / 8.0;
                let spline = super::natural_spline(
                    &pts.iter()
                        .map(|&(a, b)| (a as f64, b as f64))
                        .collect::<Vec<_>>(),
                    x,
                )
                .clamp(0.0, 255.0);
                assert!(
                    (rendered(&curve, x) - spline).abs() <= 0.26,
                    "{pts:?} at {x}"
                );
            }
            assert!(
                (rendered(&curve, x) - camera_raw).abs() < 1.5,
                "{pts:?}: {}",
                rendered(&curve, x)
            );
            assert!(curve.len() <= 16, "{pts:?}: {} points", curve.len());
        }
    }

    #[test]
    fn reports_untranslated_settings_and_the_profile_name() {
        let plain = r#"<rdf:Description crs:Exposure2012="+0.50" crs:RedHue="0"
            crs:LensProfileEnable="0" crs:HasCrop="False" crs:PerspectiveUpright="0">
            <crs:Look crs:Name="Adobe Color" crs:Amount="1"/></rdf:Description>"#;
        assert_eq!(super::untranslated_settings(plain), (vec![], None));
        let preset = r#"<rdf:Description crs:CameraProfile="Adobe Standard" crs:RedHue="+12"
            crs:LensProfileEnable="1" crs:HasCrop="True" crs:PerspectiveUpright="2">
            <crs:Look><rdf:Description crs:Name="Vivid &amp; Blue" crs:Amount="1"/></crs:Look>
            </rdf:Description>"#;
        assert_eq!(
            super::untranslated_settings(preset),
            (
                vec!["profile", "calibration", "lensProfile", "crop", "upright"],
                Some("Vivid & Blue".to_string())
            )
        );
    }

    use super::convert_xmp_to_preset;
    use serde_json::json;

    #[test]
    fn lightroom_tone_and_presence_map_onto_calibrated_controls() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:Shadows2012="+60"
            crs:Contrast2012="-30" crs:Clarity2012="+25" crs:Texture="+40"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["shadows"], 60.0);
        assert_eq!(adjustments["contrast"], -30);
        assert_eq!(adjustments["clarity"], 25);
        // Lightroom's Texture is fine-detail contrast, Rawtonic's Structure.
        assert_eq!(adjustments["structure"], 40);
    }

    #[test]
    fn lightroom_sharpening_reads_with_the_current_response() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:Sharpness="80"
            crs:SharpenDetail="100"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["sharpness"], 80.0);
        assert_eq!(adjustments["sharpenDetail"], 100);
        assert_eq!(
            adjustments["sharpenVersion"],
            rawtonic_engine::image_processing::SHARPEN_VERSION
        );
        let none = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:Exposure2012="+0.5"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(none).unwrap().adjustments;
        assert!(adjustments.get("sharpenVersion").is_none());
        // A layer's sharpening alone counts too.
        let local = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
  <crs:MaskGroupBasedCorrections><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Sharper" crs:LocalSharpness="0.5">
     <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Gradient" crs:ZeroX="0.5" crs:ZeroY="0.4" crs:FullX="0.5" crs:FullY="0.1"/>
     </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
  </rdf:Seq></crs:MaskGroupBasedCorrections>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(local).unwrap().adjustments;
        assert!(adjustments.get("sharpness").is_none());
        assert_ne!(
            adjustments["masks"][0]["adjustments"]["sharpness"],
            json!(0)
        );
        assert_eq!(
            adjustments["sharpenVersion"],
            rawtonic_engine::image_processing::SHARPEN_VERSION
        );
    }

    #[test]
    fn lightroom_grain_becomes_clumped_grain() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:GrainAmount="35"
            crs:GrainSize="30" crs:GrainFrequency="60"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["grainAmount"], 35);
        assert_eq!(adjustments["grainRoughness"], 60);
        assert_eq!(adjustments["grainStructure"], "clumped");
        let none =
            r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:GrainAmount="0"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(none).unwrap().adjustments;
        assert!(adjustments.get("grainStructure").is_none());
    }

    #[test]
    fn manual_transform_sliders_map_one_to_one() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:PerspectiveVertical="-35"
            crs:PerspectiveHorizontal="+12" crs:PerspectiveRotate="-1.5" crs:PerspectiveAspect="+20"
            crs:PerspectiveScale="95" crs:PerspectiveX="4" crs:PerspectiveY="-6"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["transformVertical"], -35);
        assert_eq!(adjustments["transformHorizontal"], 12);
        assert_eq!(adjustments["transformRotate"], -1.5);
        assert_eq!(adjustments["transformAspect"], 20);
        assert_eq!(adjustments["transformScale"], 95);
        assert_eq!(adjustments["transformXOffset"], 4);
        assert_eq!(adjustments["transformYOffset"], -6);
    }

    #[test]
    fn noise_reduction_refinements_map_one_to_one() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:LuminanceSmoothing="30"
            crs:LuminanceNoiseReductionDetail="70" crs:LuminanceNoiseReductionContrast="20"
            crs:ColorNoiseReductionDetail="40" crs:ColorNoiseReductionSmoothness="60"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["lumaNoiseDetail"], 70);
        assert_eq!(adjustments["lumaNoiseContrast"], 20);
        assert_eq!(adjustments["colorNoiseDetail"], 40);
        assert_eq!(adjustments["colorNoiseSmoothness"], 60);
    }

    #[test]
    fn black_and_white_mix_and_defringe_map_one_to_one() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:ConvertToGrayscale="True"
            crs:GrayMixerRed="-20" crs:GrayMixerBlue="+35" crs:DefringePurpleAmount="5"
            crs:DefringePurpleHueLo="25" crs:AutoLateralCA="1"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["autoChromaticAberration"], true);
        assert_eq!(adjustments["blackAndWhite"], true);
        assert_eq!(adjustments["grayMixer"]["red"], -20.0);
        assert_eq!(adjustments["grayMixer"]["blue"], 35.0);
        assert!(adjustments["grayMixer"].get("green").is_none());
        assert_eq!(adjustments["defringePurpleAmount"], 5);
        assert_eq!(adjustments["defringePurpleHueLo"], 25);
    }

    #[test]
    fn region_curve_maps_one_to_one() {
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:ParametricShadows="-15"
            crs:ParametricLights="+20" crs:ParametricMidtoneSplit="45"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        assert_eq!(adjustments["regionCurve"]["shadows"], -15.0);
        assert_eq!(adjustments["regionCurve"]["lights"], 20.0);
        assert_eq!(adjustments["regionCurve"]["midtoneSplit"], 45.0);
    }

    /// Converts every `*.xmp` in `$PRESET_XMP_DIR` to `*.json` adjustments
    /// next to it (reference measurements, notes: reference/point-color.md).
    #[test]
    #[ignore = "tool: needs PRESET_XMP_DIR"]
    fn convert_xmp_directory() {
        let dir = std::path::PathBuf::from(std::env::var("PRESET_XMP_DIR").unwrap());
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "xmp") {
                let xmp = std::fs::read_to_string(&path).unwrap();
                let adjustments = convert_xmp_to_preset(&xmp).unwrap().adjustments;
                std::fs::write(path.with_extension("json"), adjustments.to_string()).unwrap();
            }
        }
    }

    #[test]
    fn adaptive_sky_and_subject_masks_become_layers() {
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.20">
  <crs:MaskGroupBasedCorrections><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="0.5" crs:CorrectionName="Sky darker"
       crs:LocalExposure2012="-0.6" crs:LocalSaturation="0.2" crs:LocalDehaze="0.3" crs:LocalHue="0.5" crs:LocalDefringe="-0.4">
     <crs:MainCurve><rdf:Seq><rdf:li>0,0</rdf:li><rdf:li>128,148</rdf:li><rdf:li>255,255</rdf:li></rdf:Seq></crs:MainCurve>
     <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskSubType="2" crs:MaskValue="1" crs:MaskInverted="false" crs:MaskBlendMode="0"/>
     </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Background" crs:LocalExposure2012="-0.3">
     <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskSubType="1" crs:MaskInverted="true"/>
     </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Graduated" crs:LocalExposure2012="-0.5">
     <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Gradient" crs:ZeroX="0.5" crs:ZeroY="0.4" crs:FullX="0.5" crs:FullY="0.1"/>
     </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Teeth" crs:LocalExposure2012="0.2">
     <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskSubType="3" crs:MaskSubCategoryID="7"/>
     </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
  </rdf:Seq></crs:MaskGroupBasedCorrections>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (layers, skipped) = super::lightroom_masks(xmp);
        assert_eq!(skipped, vec!["Teeth".to_string()]);
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[2]["subMasks"][0]["type"], "linear");
        assert_eq!(
            layers[2]["subMasks"][0]["parameters"]["lrNormalized"]["fullY"],
            json!(0.1)
        );
        let sky = &layers[0];
        assert_eq!(sky["name"], "Sky darker");
        assert_eq!(sky["subMasks"][0]["type"], "ai-sky");
        // Amount 0.5 halves every setting and the curve's lift; local
        // exposure is stored as EV / 4.
        assert_eq!(sky["adjustments"]["exposure"], json!(-1.2));
        assert_eq!(sky["adjustments"]["saturation"], json!(10.0));
        // Hue ±1 is ±180°, scaled by the Amount (0.5).
        assert_eq!(sky["adjustments"]["hue"], json!(45.0));
        assert_eq!(sky["adjustments"]["defringe"], json!(-20.0));
        assert_eq!(sky["adjustments"]["dehaze"], json!(15.0));
        assert_eq!(
            sky["adjustments"]["curves"]["luma"][1],
            json!({"x": 128.0, "y": 138.0})
        );
        let background = &layers[1];
        assert_eq!(background["subMasks"][0]["type"], "ai-foreground");
        assert_eq!(background["subMasks"][0]["invert"], true);
        let preset = convert_xmp_to_preset(xmp).unwrap();
        assert_eq!(preset.include_masks, Some(true));
        assert_eq!(preset.adjustments["masks"].as_array().unwrap().len(), 3);
    }

    /// Compares the coordinate size used to place gradients (fast decode
    /// scaled back) with the full decode for `$DIM_FILES` (`;`-separated).
    #[test]
    #[ignore = "tool: needs DIM_FILES"]
    fn fast_decode_size_matches_full_decode() {
        let settings = crate::app_settings::AppSettings::default();
        for path in std::env::var("DIM_FILES").unwrap().split(';') {
            let bytes = std::fs::read(path).unwrap();
            let fast = crate::image_loader::load_base_image_from_bytes(
                &bytes, path, true, &settings, None,
            )
            .unwrap();
            let factor = crate::raw_processing::get_fast_demosaic_scale_factor(
                &bytes,
                fast.width(),
                fast.height(),
            );
            let full = crate::image_loader::load_base_image_from_bytes(
                &bytes, path, false, &settings, None,
            )
            .unwrap();
            println!(
                "{}\tfast/{factor} {:.0}x{:.0}\tfull {}x{}",
                path.rsplit('/').next().unwrap(),
                fast.width() as f32 / factor,
                fast.height() as f32 / factor,
                full.width(),
                full.height()
            );
        }
    }

    /// Converts `$PLACE_XMP` and places it for `$PLACE_FILE` as
    /// `apply_adjustments_to_paths` does, writing the recipe to `$PLACE_OUT`.
    #[test]
    #[ignore = "tool: needs PLACE_XMP, PLACE_FILE and PLACE_OUT"]
    fn place_preset_for_file() {
        let var = |k: &str| std::env::var(k).unwrap();
        let mut adjustments =
            convert_xmp_to_preset(&std::fs::read_to_string(var("PLACE_XMP")).unwrap())
                .unwrap()
                .adjustments;
        let path = var("PLACE_FILE");
        let bytes = std::fs::read(&path).unwrap();
        let settings = crate::app_settings::AppSettings::default();
        let image =
            crate::image_loader::load_base_image_from_bytes(&bytes, &path, true, &settings, None)
                .unwrap();
        let factor = crate::raw_processing::get_fast_demosaic_scale_factor(
            &bytes,
            image.width(),
            image.height(),
        ) as f64;
        super::place_lightroom_geometry(
            &mut adjustments,
            image.width() as f64 / factor,
            image.height() as f64 / factor,
            crate::image_loader::source_orientation(&bytes, &path),
        );
        std::fs::write(var("PLACE_OUT"), adjustments.to_string()).unwrap();
    }

    #[test]
    fn lightroom_gradients_are_placed_in_photo_pixels() {
        let mut adjustments = json!({"masks": [{"subMasks": [
            {"type": "linear", "parameters": {"lrNormalized":
                {"zeroX": 0.55, "zeroY": 0.4, "fullX": 0.5, "fullY": 0.1}}},
            {"type": "radial", "parameters": {"lrNormalized":
                {"top": 0.2, "left": 0.3, "bottom": 0.6, "right": 0.7, "angle": 15.0, "feather": 80.0}}},
            {"type": "brush", "parameters": {"lines": []}}
        ]}]});
        assert!(super::place_lightroom_geometry(
            &mut adjustments,
            6000.0,
            4000.0,
            1
        ));
        let subs = &adjustments["masks"][0]["subMasks"];
        let p = &subs[0]["parameters"];
        let (sx, sy, ex, ey, range) = (
            p["startX"].as_f64().unwrap(),
            p["startY"].as_f64().unwrap(),
            p["endX"].as_f64().unwrap(),
            p["endY"].as_f64().unwrap(),
            p["range"].as_f64().unwrap(),
        );
        // Rawtonic's linear mask at Lightroom's 100 % point.
        let (lx, ly) = (ex - sx, ey - sy);
        let len = lx.hypot(ly);
        let dist = (3000.0 - sx) * (-ly / len) + (400.0 - sy) * (lx / len);
        assert!((0.5 - dist / range * 0.5 - 1.0).abs() < 1e-3);
        // The corners span the ellipse's rectangle turned by 15°: the
        // diagonal (2400, 1600) projects to 2732 and 924 on its axes.
        let r = &subs[1]["parameters"];
        assert_eq!(r["centerX"], json!(3000.0));
        assert_eq!(r["centerY"], json!(1600.0));
        assert!((r["radiusX"].as_f64().unwrap() - 1366.2).abs() < 0.1, "{r}");
        assert!((r["radiusY"].as_f64().unwrap() - 462.2).abs() < 0.1, "{r}");
        assert_eq!(r["rotation"], json!(15.0));
        // Placed on the photo's own frame, whatever its turns now.
        let own = json!({"width": 6000.0, "height": 4000.0, "rotation": 0.0,
            "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0});
        assert_eq!((&p["frame"], &r["frame"]), (&own, &own));
        assert_eq!(subs[2]["parameters"], json!({"lines": []}));
        assert!(!super::place_lightroom_geometry(
            &mut adjustments,
            6000.0,
            4000.0,
            1
        ));
    }

    #[test]
    fn lightroom_geometry_follows_the_photo_orientation() {
        // Orientation 8 (displayed turned 90° counter-clockwise): Camera Raw
        // shows a dab at raw (0.3, 0.5) at (0.5, 0.7) and a radial with raw
        // corners (0.1, 0.3)–(0.3, 0.5) over x 0.3–0.5, y 0.7–0.9.
        let mut adjustments = json!({"masks": [{"subMasks": [
            {"type": "radial", "parameters": {"lrNormalized":
                {"top": 0.3, "left": 0.1, "bottom": 0.5, "right": 0.3, "angle": 0.0, "feather": 0.0}}},
            {"type": "brush", "parameters": {"lrNormalized": {"strokes": [
                {"dabs": [[0.3, 0.5], [0.31, 0.5]], "radius": 0.05, "flow": 0.5, "centerWeight": 1.0}]}}}
        ]}]});
        assert!(super::place_lightroom_geometry(
            &mut adjustments,
            4000.0,
            6000.0,
            8
        ));
        let subs = &adjustments["masks"][0]["subMasks"];
        let r = &subs[0]["parameters"];
        assert_eq!(
            (r["centerX"].clone(), r["centerY"].clone()),
            (json!(1600.0), json!(4800.0))
        );
        // Raw width 0.2 of a 6000 px raw side: 600 px, now vertical.
        let (rx, ry, rot) = (
            r["radiusX"].as_f64().unwrap(),
            r["radiusY"].as_f64().unwrap(),
            r["rotation"].as_f64().unwrap(),
        );
        assert!((rx - 600.0).abs() < 0.1 && (ry - 400.0).abs() < 0.1, "{r}");
        assert!((rot.abs() - 90.0).abs() < 1e-6, "{r}");
        let line = &subs[1]["parameters"]["lines"][0];
        assert_eq!(line["points"][0], json!({"x": 2000.0, "y": 4200.0}));
        assert_eq!(line["points"][1], json!({"x": 2000.0, "y": 4140.0}));
        // Radius 0.05 of the 6000 px long side; Flow 0.5 → 0.105 per dab.
        assert_eq!(line["brushSize"], json!(600.0));
        assert!(
            (line["flow"].as_f64().unwrap() - 10.96).abs() < 0.01,
            "{line}"
        );
        assert_eq!(line["lrCenterWeight"], json!(1.0));
        assert_eq!(line["frame"]["width"], json!(4000.0));
        assert_eq!(line["frame"]["orientationSteps"], json!(0));
    }

    #[test]
    fn lightroom_brushes_and_luminance_ranges_become_masks() {
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
  <crs:MaskGroupBasedCorrections><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Dodge" crs:LocalExposure2012="0.25">
    <crs:CorrectionMasks><rdf:Seq>
     <rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="1" crs:Radius="0.05" crs:Flow="1" crs:CenterWeight="0">
      <crs:Dabs><rdf:Seq><rdf:li>d 0.300000 0.500000</rdf:li><rdf:li>d 0.310000 0.500000</rdf:li>
       <rdf:li>r 0.080000</rdf:li><rdf:li>d 0.600000 0.400000</rdf:li></rdf:Seq></crs:Dabs>
     </rdf:Description></rdf:li>
     <rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="0" crs:Radius="0.02" crs:Flow="1" crs:CenterWeight="1">
      <crs:Dabs><rdf:Seq><rdf:li>d 0.305000 0.500000</rdf:li></rdf:Seq></crs:Dabs>
     </rdf:Description></rdf:li>
     <rdf:li><rdf:Description crs:What="Mask/RangeMask" crs:MaskValue="1" crs:MaskBlendMode="2">
      <crs:CorrectionRangeMask crs:Version="3" crs:Type="2" crs:LumRange="0.000000 0.000000 0.250000 0.400000"/>
     </rdf:Description></rdf:li>
    </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Colour" crs:LocalExposure2012="0.1">
    <crs:CorrectionMasks><rdf:Seq>
     <rdf:li><rdf:Description crs:What="Mask/RangeMask" crs:MaskValue="1">
      <crs:CorrectionRangeMask crs:Version="3" crs:Type="1"/>
     </rdf:Description></rdf:li>
    </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
  </rdf:Seq></crs:MaskGroupBasedCorrections>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (layers, skipped) = super::lightroom_masks(xmp);
        assert_eq!(skipped, vec!["Colour".to_string()]);
        let subs = &layers[0]["subMasks"];
        assert_eq!(layers[0]["adjustments"]["exposure"], json!(1.0));
        let strokes = &subs[0]["parameters"]["lrNormalized"]["strokes"];
        assert_eq!(strokes.as_array().unwrap().len(), 2);
        assert_eq!(strokes[0]["dabs"], json!([[0.3, 0.5], [0.31, 0.5]]));
        assert_eq!(strokes[1]["radius"], json!(0.08));
        // The eraser stroke subtracts.
        assert_eq!(subs[1]["mode"], "subtractive");
        assert_eq!(subs[1]["opacity"], json!(100.0));
        assert_eq!(subs[2]["type"], "luma-range");
        assert_eq!(subs[2]["mode"], "intersect");
        assert_eq!(
            subs[2]["parameters"]["lrRange"],
            json!([0.0, 0.0, 0.25, 0.4])
        );
    }

    #[test]
    fn lightroom_per_dab_radius_flow_and_hardness_are_kept() {
        // A pen stroke in Lightroom changes radius, flow and CenterWeight
        // between dabs ("r"/"f"/"h"); every dab keeps the settings it was
        // painted with (a stroke per run of equal settings, exact for
        // Lightroom's tip: a dab at every point, accumulated 1 − Π(1 − a)).
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
  <crs:MaskGroupBasedCorrections><rdf:Seq>
   <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Pen" crs:LocalExposure2012="0.25">
    <crs:CorrectionMasks><rdf:Seq>
     <rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="1" crs:Radius="0.05" crs:Flow="1" crs:CenterWeight="0">
      <crs:Dabs><rdf:Seq><rdf:li>d 0.300000 0.500000</rdf:li><rdf:li>f 0.400000</rdf:li>
       <rdf:li>d 0.310000 0.500000</rdf:li><rdf:li>r 0.030000</rdf:li><rdf:li>f 0.700000</rdf:li>
       <rdf:li>d 0.320000 0.500000</rdf:li><rdf:li>h 0.500000</rdf:li><rdf:li>d 0.330000 0.500000</rdf:li>
       <rdf:li>d 0.340000 0.500000</rdf:li></rdf:Seq></crs:Dabs>
     </rdf:Description></rdf:li>
    </rdf:Seq></crs:CorrectionMasks>
   </rdf:Description></rdf:li>
  </rdf:Seq></crs:MaskGroupBasedCorrections>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (layers, _) = super::lightroom_masks(xmp);
        let strokes = layers[0]["subMasks"][0]["parameters"]["lrNormalized"]["strokes"]
            .as_array()
            .unwrap()
            .clone();
        let settings: Vec<(usize, f64, f64, f64)> = strokes
            .iter()
            .map(|s| {
                (
                    s["dabs"].as_array().unwrap().len(),
                    s["radius"].as_f64().unwrap(),
                    s["flow"].as_f64().unwrap(),
                    s["centerWeight"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            settings,
            vec![
                (1, 0.05, 1.0, 0.0),
                (1, 0.05, 0.4, 0.0),
                (1, 0.03, 0.7, 0.0),
                (2, 0.03, 0.7, 0.5)
            ]
        );
    }

    #[test]
    fn lightroom_colour_ranges_keep_their_samples_and_refine() {
        // As Lightroom Classic 15.6 writes them (attributes on an inner
        // rdf:Description): two points and one area at Refine 25.
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Maske 1" crs:CorrectionAmount="1" crs:LocalExposure2012="0.25">
   <crs:CorrectionMasks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/RangeMask" crs:MaskValue="1">
    <crs:CorrectionRangeMask><rdf:Description crs:Version="3" crs:Type="1" crs:ColorAmount="0.242424" crs:SampleType="1">
     <crs:PointModels><rdf:Seq>
      <rdf:li>0.383628 0.738703 0.775121 0.634097 0.622801 0</rdf:li>
      <rdf:li>0.772694 0.268705 0.405694 0.155845 0.146252 1</rdf:li>
     </rdf:Seq></crs:PointModels>
     <crs:AreaModels><rdf:Seq><rdf:li><rdf:Description
      crs:ColorRangeMaskAreaSampleInfo="0.877884 0.449306 0.838344 0.412638 0.916288 0.413707 0.838554 0.485430 0.916558 0.485595 2"/>
     </rdf:li></rdf:Seq></crs:AreaModels>
    </rdf:Description></crs:CorrectionRangeMask>
   </rdf:Description></rdf:li></rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (layers, skipped) = super::lightroom_masks(xmp);
        assert!(skipped.is_empty(), "{skipped:?}");
        let mut adjustments = json!({ "masks": layers });
        assert!(super::place_lightroom_geometry(
            &mut adjustments,
            7728.0,
            5152.0,
            1
        ));
        let p = &adjustments["masks"][0]["subMasks"][0]["parameters"];
        assert_eq!(adjustments["masks"][0]["subMasks"][0]["type"], "color");
        assert_eq!(p["refine"], json!(25.0));
        let samples = p["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0], json!({"x": 4900.3, "y": 3208.67}));
        assert_eq!(p["targetX"], json!(4900.3));
        // The area: its corners' bounding box.
        assert_eq!(samples[2]["x"], json!(6478.72));
        assert!((samples[2]["width"].as_f64().unwrap() - 0.078214 * 7728.0).abs() < 0.1);
    }

    #[test]
    fn auto_mask_dabs_keep_their_auto_mask() {
        // Lightroom writes Auto Mask dabs as "M x y" (Classic 15.6).
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionName="Maske 2" crs:CorrectionAmount="1" crs:LocalExposure2012="0.25">
   <crs:CorrectionMasks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="1"
    crs:Radius="0.117352" crs:Flow="1" crs:CenterWeight="0">
    <crs:Dabs><rdf:Seq><rdf:li>M 0.547222 0.485417</rdf:li><rdf:li>M 0.570680 0.486590</rdf:li>
     <rdf:li>d 0.600000 0.486590</rdf:li></rdf:Seq></crs:Dabs>
   </rdf:Description></rdf:li></rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (layers, skipped) = super::lightroom_masks(xmp);
        assert!(skipped.is_empty(), "{skipped:?}");
        let strokes = &layers[0]["subMasks"][0]["parameters"]["lrNormalized"]["strokes"];
        assert_eq!(strokes.as_array().unwrap().len(), 2);
        assert_eq!(strokes[0]["autoMask"], true);
        assert_eq!(strokes[0]["dabs"].as_array().unwrap().len(), 2);
        assert_eq!(strokes[1]["autoMask"], false);
        let mut adjustments = json!({ "masks": layers });
        assert!(super::place_lightroom_geometry(
            &mut adjustments,
            6240.0,
            4160.0,
            1
        ));
        let lines = &adjustments["masks"][0]["subMasks"][0]["parameters"]["lines"];
        assert_eq!(lines[0]["autoMask"], true);
        assert_eq!(lines[1]["autoMask"], false);
    }

    #[test]
    fn lightroom_heal_and_clone_spots_are_placed_in_the_photo() {
        // As Lightroom Classic 15.6 writes them: a clone spot, a Remove area
        // (its own fill, not translated) and a painted heal stroke.
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:RetouchAreas><rdf:Seq>
  <rdf:li><rdf:Description crs:SpotType="clone" crs:SourceState="sourceAutoComputed" crs:Method="gaussian"
    crs:SourceX="0.218483" crs:OffsetY="0.777244" crs:Opacity="1" crs:Feather="0">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Ellipse" crs:MaskValue="1" crs:X="0.227778" crs:Y="0.829167"
    crs:SizeX="0.017424" crs:SizeY="0.017424"/></rdf:Seq></crs:Masks></rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="heal_patchmatch" crs:Opacity="1">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Ellipse" crs:X="0.17" crs:Y="0.12" crs:SizeX="0.017"/></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="heal" crs:SourceX="0.408333" crs:OffsetY="0.091587" crs:Opacity="0.5" crs:Feather="0.2">
   <crs:Masks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/Paint" crs:Radius="0.017424" crs:Flow="1" crs:CenterWeight="1">
    <crs:Dabs><rdf:Seq><rdf:li>d 0.408333 0.162500</rdf:li><rdf:li>d 0.475243 0.206812</rdf:li></rdf:Seq></crs:Dabs>
   </rdf:Description></rdf:li></rdf:Seq></crs:Masks></rdf:Description></rdf:li>
 </rdf:Seq></crs:RetouchAreas></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (spots, removals) = super::lightroom_spots(xmp);
        assert_eq!((spots.len(), removals), (2, 1));
        let mut adjustments = json!({ "spots": spots });
        assert!(super::place_lightroom_geometry(
            &mut adjustments,
            6240.0,
            4160.0,
            1
        ));
        let placed = adjustments["spots"].as_array().unwrap();
        let clone = &placed[0];
        assert_eq!(clone["mode"], "clone");
        assert_eq!(
            (clone["x"].clone(), clone["y"].clone()),
            (json!(1421.33), json!(3449.33))
        );
        assert_eq!(clone["sourceX"], json!(1363.33));
        assert_eq!(clone["radius"], json!(108.73));
        assert!(clone.get("lrNormalized").is_none() && clone["visible"] == true);
        let stroke = &placed[1];
        assert_eq!(
            (
                stroke["mode"].clone(),
                stroke["opacity"].clone(),
                stroke["feather"].clone()
            ),
            (json!("heal"), json!(50.0), json!(20.0))
        );
        assert_eq!(stroke["points"].as_array().unwrap().len(), 2);
        assert_eq!(stroke["x"], stroke["points"][0]["x"]);
        // A portrait (orientation 6): the radius is a share of the unrotated width.
        let mut turned = json!({ "spots": super::lightroom_spots(xmp).0 });
        super::place_lightroom_geometry(&mut turned, 4160.0, 6240.0, 6);
        assert_eq!(turned["spots"][0]["radius"], json!(108.73));
        // Older files: the RetouchInfo strings.
        let legacy = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:RetouchInfo><rdf:Seq>
  <rdf:li>centerX = 0.227778, centerY = 0.829167, radius = 0.017424, sourceState = sourceAutoComputed, sourceX = 0.218483, sourceY = 0.777244, spotType = clone</rdf:li>
 </rdf:Seq></crs:RetouchInfo></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (old, _) = super::lightroom_spots(legacy);
        assert_eq!(old.len(), 1);
        assert_eq!(old[0]["lrNormalized"]["sourceY"], json!(0.777244));
    }

    #[test]
    fn point_colors_become_colour_ranges() {
        // Adobe's own preset format: a default-range orange swatch with hue
        // +15 and saturation −13, and one whose saturation lies outside its
        // plateau (Camera Raw ignores such swatches).
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description>
            <crs:PointColors><rdf:Seq>
            <rdf:li>0.500000, 0.700000, 0.300000, 0.150000, -0.130000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.000000, 0.520000, 1.000000, 1.000000, 0.000000, 0.400000, 0.760000, 1.000000</rdf:li>
            <rdf:li>3.700000, 0.300000, 0.300000, 0.000000, -1.000000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.500000, 0.600000, 1.000000, 1.000000, 0.000000, 0.400000, 0.760000, 1.000000</rdf:li>
            </rdf:Seq></crs:PointColors>
            <crs:ColorVariance><rdf:Seq><rdf:li>0.000000</rdf:li><rdf:li>0.000000</rdf:li></rdf:Seq></crs:ColorVariance>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(xmp).unwrap().adjustments;
        let ranges = adjustments["colorRanges"].as_array().unwrap();
        assert_eq!(ranges.len(), 1);
        let range = &ranges[0];
        let hue = range["hue"].as_f64().unwrap();
        assert!((30.0..90.0).contains(&hue), "orange: {hue}");
        let shift = range["hueShift"].as_f64().unwrap();
        assert!((5.0..30.0).contains(&shift), "{shift}");
        let saturation = range["saturation"].as_f64().unwrap();
        assert!((-20.0..-3.0).contains(&saturation), "{saturation}");
        let width = range["hueRange"].as_f64().unwrap();
        assert!((10.0..60.0).contains(&width), "{width}");
    }

    #[test]
    fn split_toning_without_blending_renders_at_blending_100() {
        let legacy = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:SplitToningShadowHue="220"
            crs:SplitToningShadowSaturation="30"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(legacy).unwrap().adjustments;
        assert_eq!(adjustments["colorGrading"]["blending"], 100);
        let modern = r#"<x:xmpmeta><rdf:RDF><rdf:Description crs:SplitToningShadowHue="220"
            crs:SplitToningShadowSaturation="30" crs:ColorGradeBlending="50"/></rdf:RDF></x:xmpmeta>"#;
        let adjustments = convert_xmp_to_preset(modern).unwrap().adjustments;
        assert_eq!(adjustments["colorGrading"]["blending"], 50);
    }
}
