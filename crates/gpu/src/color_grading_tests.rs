//! `colorGradingVersion` 3 and 4 in the shader (`apply_grading`) against
//! the same steps computed here from the neutral render and the CPU's
//! table: the Luminance sliders' curve per channel by its cube root, the
//! tint as an Oklab shift by the lightness before the curve (with 4 mixed
//! with the tint each channel takes as a grey of its own level), the result
//! limited per channel to the sRGB cube. The fits of the tables ran on these
//! steps (notes: benchmark-results/cg2-colour-grading-2026-11-15,
//! benchmark-results/cg3-2026-11-17), so the shader has to do the same.
//!
//! Run: `cargo test -p rawtonic-gpu -- --ignored colour_grading_3`.

use super::*;
use rawtonic_engine::color_grading::{GRADING_CHANNEL_WEIGHT, GRADING_LUT_SIZE, Grading, lut};
use rawtonic_engine::recipe_versions::ColorGradingVersion;

pub(super) fn to_oklab(c: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * c[0] + 0.5363325363 * c[1] + 0.0514459929 * c[2]).cbrt();
    let m = (0.2119034982 * c[0] + 0.6806995451 * c[1] + 0.1073969566 * c[2]).cbrt();
    let s = (0.0883024619 * c[0] + 0.2817188376 * c[1] + 0.6299787005 * c[2]).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

pub(super) fn from_oklab(lab: [f64; 3]) -> [f64; 3] {
    let l = (lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2]).powi(3);
    let m = (lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2]).powi(3);
    let s = (lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2]).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

pub(super) fn decode(e: f64) -> f64 {
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn encode(c: f64) -> f64 {
    let a = c.abs();
    c.signum()
        * if a <= 0.0031308 {
            a * 12.92
        } else {
            1.055 * a.powf(1.0 / 2.4) - 0.055
        }
}

/// The table's entry at a lightness, as the shader interpolates it.
fn entry(table: &[[f32; 4]; GRADING_LUT_SIZE], lightness: f64) -> [f64; 4] {
    let x = lightness.clamp(0.0, 1.0) * (GRADING_LUT_SIZE - 1) as f64;
    let i = (x as usize).min(GRADING_LUT_SIZE - 2);
    let f = x - i as f64;
    std::array::from_fn(|k| table[i][k] as f64 * (1.0 - f) + table[i + 1][k] as f64 * f)
}

/// Version 3's grading of an sRGB colour inside the cube, with version 4's
/// tint per channel mixed in by `weight` (0 for version 3).
fn graded(pixel: [f32; 3], table: &[[f32; 4]; GRADING_LUT_SIZE], weight: f64) -> [f64; 3] {
    let linear = pixel.map(|e| decode(e as f64));
    let lab = to_oklab(linear);
    let mut base = lab;
    if table.iter().any(|row| row[3] != 0.0) {
        base = to_oklab(linear.map(|c| (c.cbrt() + entry(table, c.cbrt())[3]).powi(3)));
    }
    let tint = entry(table, lab[0]);
    let shifted = |lab: [f64; 3], tint: [f64; 4]| {
        from_oklab([lab[0] + tint[0], lab[1] + tint[1], lab[2] + tint[2]])
    };
    let graded = shifted(base, tint);
    let whole = shifted(lab, tint);
    std::array::from_fn(|k| {
        let root = linear[k].cbrt();
        let per_channel = shifted([root, 0.0, 0.0], entry(table, root))[k];
        encode(graded[k] + weight * (per_channel - whole[k])).clamp(0.0, 1.0)
    })
}

#[test]
#[ignore = "requires a native GPU; colorGradingVersion 3's shader path against the same steps on the CPU"]
fn colour_grading_3_limits_per_channel_and_curves_per_channel() {
    let context = context();
    let slot = GpuProcessorSlot::new(None);
    let charts = Charts::new();
    let gradings = [
        // Strong tints that leave sRGB in the dark and the light tones.
        json!({"shadows": {"hue": 120, "saturation": 100}, "highlights": {"hue": 300, "saturation": 100}}),
        // Luminance alone: the curve per channel.
        json!({"shadows": {"luminance": -40}, "midtones": {"luminance": 60}}),
        // Both.
        json!({"global": {"hue": 30, "saturation": 60, "luminance": -30},
            "highlights": {"hue": 210, "saturation": 80, "luminance": 40}, "balance": 30}),
    ];
    for (number, (version, grading)) in [3u64, 4]
        .into_iter()
        .flat_map(|version| gradings.iter().map(move |grading| (version, grading)))
        .enumerate()
    {
        let table = lut(
            &Grading::from_json(grading),
            ColorGradingVersion::of(&json!({ "colorGradingVersion": version })),
        );
        let weight = if version >= 4 {
            f64::from(GRADING_CHANNEL_WEIGHT)
        } else {
            0.0
        };
        // Ramp (greys) and hue chart (colours).
        for (chart, colours) in [(&charts.images[0], false), (&charts.images[2], true)] {
            let neutral = render_srgb(&context, &slot, chart, &base_recipe(), true, &[]);
            let mut recipe = base_recipe();
            recipe["colorGradingVersion"] = json!(version);
            recipe["colorGrading"] = grading.clone();
            let rendered = render_srgb(&context, &slot, chart, &recipe, true, &[]);
            // Colours the limit leaves alone, and those it puts on the
            // cube's surface. The output's knee eases colours on and
            // within 5 % of the gamut's edge a little (ease_into_gamut),
            // so the largest difference is that easing; the median and the
            // 90th percentile of the colours left alone show the path
            // itself.
            let mut errors = [Vec::new(), Vec::new()];
            for (before, after) in neutral.iter().zip(&rendered) {
                // A colour on the cube's surface may lie beyond it in the
                // working gamut, where this test does not see it; so may a
                // saturated colour of the hue chart, which the output
                // mapping brings inside the cube.
                let (low, high) = before
                    .iter()
                    .fold((1.0f32, 0.0f32), |(l, h), &c| (l.min(c), h.max(c)));
                if low <= 0.002 || high >= 0.998 {
                    continue;
                }
                if colours && (low < 0.08 || high > 0.92 || high - low > 0.45) {
                    continue;
                }
                let wanted = graded(*before, &table, weight);
                let limited = wanted.iter().any(|&c| c <= 0.001 || c >= 0.999);
                errors[usize::from(limited)].push(
                    (0..3)
                        .map(|k| (wanted[k] - after[k] as f64).abs())
                        .fold(0.0, f64::max),
                );
            }
            let limited = errors[1].len();
            for (kind, errors) in errors.iter_mut().enumerate() {
                if errors.is_empty() {
                    continue;
                }
                errors.sort_by(f64::total_cmp);
                let (median, p90, max) = (
                    errors[errors.len() / 2],
                    errors[errors.len() * 9 / 10],
                    errors[errors.len() - 1],
                );
                eprintln!(
                    "grading {number}, {} {} pixels: |shader − CPU| median {median:.5} p90 {p90:.5} max {max:.5}",
                    errors.len(),
                    ["free", "limited"][kind],
                );
                assert!(max < 0.03, "grading {number}");
                if kind == 0 {
                    assert!(median < 0.0005 && p90 < 0.002, "grading {number}");
                }
            }
            if number % gradings.len() == 0 && !colours {
                assert!(limited > 0, "the tints leave sRGB somewhere");
            }
        }
    }
}
