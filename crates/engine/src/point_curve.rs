//! Point curves with more control points than the shader's 16 (Lightroom
//! curves imported with many points). The renderer then reads the global
//! curves from one sampled table (`curve_table`) instead of the uniform's
//! first 16 points; recipes whose curves fit stay on the exact evaluator.
use serde_json::Value;
use std::sync::Arc;

/// The curves' `version` from which curves with more than 16 points render
/// whole; before, the renderer used their first 16 points.
pub const LONG_CURVE_VERSION: u64 = 2;
/// The curves' `version` from which the curves act in the wide working space
/// (ProPhoto primaries with the sRGB tone curve, where Camera Raw evaluates
/// its point curves; notes: reference/point-curve.md) instead of on display
/// values clipped to sRGB.
pub const WIDE_CURVE_VERSION: u64 = 3;
/// Control points one curve channel holds in the uniforms (shader `Point` arrays).
pub const SHADER_CURVE_POINTS: usize = 16;
/// Samples per channel in a curve table (1,024 intervals, shader `apply_curve_lut`).
pub const CURVE_TABLE_SAMPLES: usize = 1025;

/// A curve channel's points as the uniforms read them: entries without a
/// numeric x or y count as (0, 0) (`convert_points_to_aligned`).
pub fn curve_points(points: &[Value]) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|p| match (p["x"].as_f64(), p["y"].as_f64()) {
            (Some(x), Some(y)) => (x as f32, y as f32),
            _ => (0.0, 0.0),
        })
        .collect()
}

fn hermite(x: f32, p1: (f32, f32), p2: (f32, f32), m1: f32, m2: f32) -> f32 {
    let dx = p2.0 - p1.0;
    if dx <= 0.0 {
        return p1.1;
    }
    let t = (x - p1.0) / dx;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * p1.1 + h10 * m1 * dx + h01 * p2.1 + h11 * m2 * dx
}

/// The shader's `apply_curve` (monotone cubic Hermite through the points,
/// flat beyond the ends) for any number of points; `value` in 0–1.
pub fn curve_value(points: &[(f32, f32)], value: f32) -> f32 {
    let count = points.len();
    if count < 2 {
        return value;
    }
    let x = value * 255.0;
    if x <= points[0].0 {
        return points[0].1 / 255.0;
    }
    if x >= points[count - 1].0 {
        return points[count - 1].1 / 255.0;
    }
    for i in 0..count - 1 {
        let p1 = points[i];
        let p2 = points[i + 1];
        if x <= p2.0 {
            let p0 = points[i.saturating_sub(1)];
            let p3 = points[(i + 2).min(count - 1)];
            let before = (p1.1 - p0.1) / (p1.0 - p0.0).max(0.001);
            let current = (p2.1 - p1.1) / (p2.0 - p1.0).max(0.001);
            let after = (p3.1 - p2.1) / (p3.0 - p2.0).max(0.001);
            let mut m1 = if i == 0 {
                current
            } else if before * current <= 0.0 {
                0.0
            } else {
                (before + current) / 2.0
            };
            let mut m2 = if i + 1 == count - 1 {
                current
            } else if current * after <= 0.0 {
                0.0
            } else {
                (current + after) / 2.0
            };
            if current != 0.0 {
                let alpha = m1 / current;
                let beta = m2 / current;
                if alpha * alpha + beta * beta > 9.0 {
                    let tau = 3.0 / (alpha * alpha + beta * beta).sqrt();
                    m1 *= tau;
                    m2 *= tau;
                }
            }
            return (hermite(x, p1, p2, m1, m2) / 255.0).clamp(0.0, 1.0);
        }
    }
    points[count - 1].1 / 255.0
}

/// The shader's `is_default_curve`: a channel curve that leaves values alone.
fn is_identity(points: &[(f32, f32)]) -> bool {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return false;
    };
    points.len() >= 2
        && points.iter().all(|p| (p.0 - p.1).abs() <= 0.5)
        && first.0.abs() < 0.1
        && first.1.abs() < 0.1
        && (last.0 - 255.0).abs() < 0.1
        && (last.1 - 255.0).abs() < 0.1
}

/// The uniforms' `curve_space` for a recipe's (or a layer's) `curves`: 1 when
/// they act in the wide working space, which needs `WIDE_CURVE_VERSION` and a
/// channel that changes something (neutral curves skip the conversion).
pub fn curve_space(curves: &Value) -> u32 {
    if curves["version"].as_u64().unwrap_or(1) < WIDE_CURVE_VERSION {
        return 0;
    }
    let changes = ["luma", "red", "green", "blue"].iter().any(|key| {
        let points = curve_points(curves[*key].as_array().map_or(&[], Vec::as_slice));
        points.len() >= 2 && !is_identity(&points)
    });
    u32::from(changes)
}

/// The points of the Luminance curve (`curves.luminance`) as the uniforms
/// take them; none when it is missing or changes nothing, so the shader
/// skips it.
pub fn luminance_points(curves: &Value) -> Vec<Value> {
    let Some(points) = curves["luminance"].as_array() else {
        return Vec::new();
    };
    let parsed = curve_points(points);
    if parsed.len() < 2 || is_identity(&parsed) {
        return Vec::new();
    }
    points.clone()
}

/// The recipe whose render shows, at every pixel, the colour the photo's
/// region and point curves receive (tone targets sample it): those curves,
/// and everything the shader applies after them (layer curves, a
/// display-referred LUT, grain, view states such as clipping warnings, soft
/// proofing, the colour-range view), left out. Steps before them stay.
pub fn curve_input_probe(adjustments: &Value) -> Value {
    let mut probe = adjustments.clone();
    let Some(recipe) = probe.as_object_mut() else {
        return probe;
    };
    for key in [
        "curves",
        "regionCurve",
        "grainAmount",
        "showClipping",
        "softProof",
        "softProofWarning",
        "colorRangeShow",
    ] {
        recipe.remove(key);
    }
    if !recipe
        .get("lutIsSceneReferred")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        recipe.remove("lutPath");
    }
    if let Some(masks) = recipe.get_mut("masks").and_then(Value::as_array_mut) {
        for mask in masks {
            if let Some(layer) = mask.get_mut("adjustments").and_then(Value::as_object_mut) {
                layer.remove("curves");
            }
        }
    }
    probe
}

/// The shader's `apply_all_curves` for one value in 0–1: the luma (RGB)
/// curve on every channel, then each channel's own curve.
pub fn curves_value(channels: &[Vec<(f32, f32)>; 4], value: f32) -> [f32; 3] {
    let luma = curve_value(&channels[0], value);
    std::array::from_fn(|c| {
        let own = &channels[c + 1];
        let v = if is_identity(own) {
            luma
        } else {
            curve_value(own, luma)
        };
        v.clamp(0.0, 1.0)
    })
}

/// The global curves sampled for the renderer when a channel has more points
/// than the uniforms hold (`None` otherwise): `channels` are the luma, red,
/// green and blue points as the recipe stores them. The table holds the exact
/// curves every 1/1024 of the input and is read linearly between, so a read
/// lies between the exact curves' values within a quarter input level. On
/// the 20-point test curves the reads stay within 0.01–0.11 of an output
/// level (under a quarter level, tested); a near-vertical segment (a jump
/// within one input level) is spread over that quarter input level.
pub fn curve_table(channels: &[Vec<(f32, f32)>; 4]) -> Option<Arc<Vec<[f32; 4]>>> {
    if channels.iter().all(|c| c.len() <= SHADER_CURVE_POINTS) {
        return None;
    }
    let table = (0..CURVE_TABLE_SAMPLES)
        .map(|i| {
            let [r, g, b] = curves_value(channels, i as f32 / (CURVE_TABLE_SAMPLES - 1) as f32);
            [r, g, b, 0.0]
        })
        .collect();
    Some(Arc::new(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Vec<(f32, f32)> {
        vec![(0.0, 0.0), (255.0, 255.0)]
    }

    /// The shader's linear table read (`apply_curve_lut`).
    fn read(table: &[[f32; 4]], value: f32, channel: usize) -> f32 {
        let position = value.clamp(0.0, 1.0) * (CURVE_TABLE_SAMPLES - 1) as f32;
        let index = (position as usize).min(CURVE_TABLE_SAMPLES - 2);
        let t = position - index as f32;
        table[index][channel] * (1.0 - t) + table[index + 1][channel] * t
    }

    // A Lightroom-style curve with 20 points: zig-zag of ±10 levels.
    fn long_curve() -> Vec<(f32, f32)> {
        let xs = [
            0, 13, 27, 40, 54, 67, 81, 94, 107, 121, 134, 148, 161, 174, 188, 201, 215, 228, 242,
            255,
        ];
        xs.iter()
            .enumerate()
            .map(|(i, &x)| {
                let y = if i == 0 || i == xs.len() - 1 {
                    x
                } else if i % 2 == 1 {
                    x + 10
                } else {
                    x - 10
                };
                (x as f32, y as f32)
            })
            .collect()
    }

    #[test]
    fn curves_act_in_the_wide_space_from_version_three_when_they_change_something() {
        let neutral = serde_json::json!([{"x": 0, "y": 0}, {"x": 255, "y": 255}]);
        let red = serde_json::json!([{"x": 0, "y": 0}, {"x": 128, "y": 160}, {"x": 255, "y": 255}]);
        let curves = |version: Option<u64>, red: &Value| {
            let mut c =
                serde_json::json!({"luma": neutral, "red": red, "green": neutral, "blue": neutral});
            if let Some(v) = version {
                c["version"] = v.into();
            }
            c
        };
        assert_eq!(curve_space(&curves(None, &red)), 0);
        assert_eq!(curve_space(&curves(Some(2), &red)), 0);
        assert_eq!(curve_space(&curves(Some(3), &red)), 1);
        assert_eq!(curve_space(&curves(Some(3), &neutral)), 0);
        assert_eq!(curve_space(&serde_json::json!({"version": 3})), 0);
    }

    #[test]
    fn the_curve_input_probe_leaves_out_the_curves_and_what_follows_them() {
        let curve =
            serde_json::json!([{"x": 0, "y": 0}, {"x": 128, "y": 160}, {"x": 255, "y": 255}]);
        let recipe = serde_json::json!({
            "exposure": 0.5, "levels": {"rgb": {"inBlack": 5}}, "curves": {"luma": curve},
            "regionCurve": {"darks": 30}, "grainAmount": 20, "showClipping": true,
            "softProof": "proPhoto", "colorRangeShow": "r1", "lutPath": "/look.cube",
            "masks": [{"id": "m", "adjustments": {"exposure": 0.3, "curves": {"red": curve}}}],
        });
        let probe = curve_input_probe(&recipe);
        assert_eq!(
            probe,
            serde_json::json!({
                "exposure": 0.5, "levels": {"rgb": {"inBlack": 5}},
                "masks": [{"id": "m", "adjustments": {"exposure": 0.3}}],
            })
        );
        // A scene-referred LUT acts before the curves and stays.
        let scene = serde_json::json!({"lutPath": "/log.cube", "lutIsSceneReferred": true});
        assert_eq!(curve_input_probe(&scene), scene);
    }

    #[test]
    fn the_luminance_curve_reaches_the_uniforms_only_when_it_changes_something() {
        let lift =
            serde_json::json!([{"x": 0, "y": 0}, {"x": 128, "y": 160}, {"x": 255, "y": 255}]);
        assert_eq!(
            luminance_points(&serde_json::json!({"luminance": lift})).len(),
            3
        );
        for curves in [
            serde_json::json!({}),
            serde_json::json!({"luminance": [{"x": 0, "y": 0}, {"x": 255, "y": 255}]}),
            serde_json::json!({"luminance": [{"x": 0, "y": 0}]}),
        ] {
            assert!(luminance_points(&curves).is_empty(), "{curves}");
        }
    }

    #[test]
    fn short_curves_keep_the_exact_evaluator() {
        let sixteen: Vec<_> = (0..16)
            .map(|i| (i as f32 * 17.0, i as f32 * 17.0))
            .collect();
        assert!(curve_table(&[sixteen, identity(), identity(), identity()]).is_none());
    }

    #[test]
    fn long_curves_pass_through_every_point() {
        let curve = long_curve();
        let table = curve_table(&[curve.clone(), identity(), identity(), identity()]).unwrap();
        for &(x, y) in &curve {
            for channel in 0..3 {
                let got = read(&table, x / 255.0, channel) * 255.0;
                assert!((got - y).abs() < 0.05, "x {x}: {got} vs {y}");
            }
        }
        // Past point 16 (x 201) the curve still follows its points; the
        // first 16 alone would hold 211 from there on.
        assert!((read(&table, 242.0 / 255.0, 0) * 255.0 - 232.0).abs() < 0.05);
    }

    #[test]
    fn table_reads_stay_within_a_quarter_level_of_the_curve() {
        let mut red = long_curve();
        red.reverse();
        let red: Vec<_> = red.iter().map(|&(x, y)| (255.0 - x, 255.0 - y)).collect();
        let channels = [long_curve(), red, identity(), long_curve()];
        let table = curve_table(&channels).unwrap();
        let mut worst = 0.0f32;
        for i in 0..=8160 {
            let v = i as f32 / 8160.0;
            let exact = curves_value(&channels, v);
            for (c, expected) in exact.iter().enumerate() {
                worst = worst.max((read(&table, v, c) - expected).abs() * 255.0);
            }
        }
        assert!(worst < 0.25, "worst {worst} levels");
    }

    #[test]
    fn steep_curves_stay_within_a_quarter_input_level() {
        // A jump from 0 to 255 between inputs 128 and 129, 20 points.
        let xs = [
            0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 128, 129, 150, 180, 210, 240, 255,
        ];
        let steep: Vec<_> = xs
            .iter()
            .map(|&x| (x as f32, if x <= 128 { 0.0 } else { 255.0 }))
            .collect();
        let channels = [steep, identity(), identity(), identity()];
        let table = curve_table(&channels).unwrap();
        let reach = 1.0 / (CURVE_TABLE_SAMPLES - 1) as f32;
        for i in 0..=65_280 {
            let v = i as f32 / 65_280.0;
            let got = read(&table, v, 0);
            let (lo, hi) = (
                curves_value(&channels, (v - reach).max(0.0))[0],
                curves_value(&channels, (v + reach).min(1.0))[0],
            );
            assert!(
                got >= lo.min(hi) - 1e-5 && got <= lo.max(hi) + 1e-5,
                "{v}: {got} not in {lo}..{hi}"
            );
        }
    }

    #[test]
    fn matches_the_sixteen_point_evaluation_on_short_curves() {
        // The same arithmetic as the shader: a 3-point curve's midpoint and
        // the flat extension beyond moved endpoints.
        let curve = vec![(32.0, 0.0), (128.0, 160.0), (224.0, 255.0)];
        assert_eq!(curve_value(&curve, 128.0 / 255.0), 160.0 / 255.0);
        assert_eq!(curve_value(&curve, 0.05), 0.0);
        assert_eq!(curve_value(&curve, 0.95), 1.0);
    }
}
