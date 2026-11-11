//! Pasted version keys (`rawtonic_engine::pasted_versions`) rendered on the
//! slider-response charts:
//!
//! - a global tool pasted from a newer edit onto an older one whose layer
//!   uses the tool renders exactly as if the user had set the photo's own
//!   slider to the translated value (the layer keeps its version and its
//!   value), where carrying the source's key would change the layer;
//! - the translation between versions: exact pairs (Whites 16 / 17, Blacks
//!   17 → 18, Structure 1 / 2) render byte-identical to the source; for the others
//!   the remaining error (CIELAB ΔE76 over the four charts, Camera Raw look)
//!   is measured and printed, with the error of pasting the value untranslated.
//!
//! Run: `cargo test -p rawtonic-gpu -- --ignored pasted_versions`;
//! `PASTED_VERSIONS_OUT=<file>` writes the measurement as JSON.

use super::*;
use rawtonic_engine::pasted_versions::{PasteTarget, express_pasted, translate};

/// Per-pixel ΔE76 between two renders of the same chart.
fn delta_e(a: &Lab, b: &Lab) -> Vec<f64> {
    a.0.iter()
        .zip(&b.0)
        .map(|(p, q)| {
            ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
        })
        .collect()
}

/// The four charts rendered with `recipe`.
fn render_all(
    context: &GpuContext,
    slot: &GpuProcessorSlot,
    charts: &Charts,
    recipe: &Value,
) -> Vec<Lab> {
    charts
        .images
        .iter()
        .map(|chart| render(context, slot, chart, recipe, true))
        .collect()
}

/// Mean and 95th percentile ΔE76 between two sets of chart renders.
fn error(a: &[Lab], b: &[Lab]) -> (f64, f64) {
    let mut all: Vec<f64> = a.iter().zip(b).flat_map(|(x, y)| delta_e(x, y)).collect();
    let mean = all.iter().sum::<f64>() / all.len() as f64;
    all.sort_by(f64::total_cmp);
    (mean, all[(all.len() as f64 * 0.95) as usize])
}

/// A recipe's slider: a top-level key, or `hsl.<band>.<property>`.
fn set(recipe: &mut Value, slider: &str, value: f64) {
    if let Some(rest) = slider.strip_prefix("hsl.") {
        let (band, property) = rest.split_once('.').unwrap();
        recipe["hsl"][band][property] = json!(value);
    } else {
        recipe[slider] = json!(value);
    }
}

struct Case {
    key: &'static str,
    slider: &'static str,
    pairs: &'static [(u64, u64)],
    values: &'static [f64],
    /// Pairs that render byte-identical once translated.
    exact: &'static [(u64, u64)],
}

const SIGNED: &[f64] = &[-100.0, -50.0, 50.0, 100.0];

const CASES: &[Case] = &[
    Case {
        key: "toneVersion",
        slider: "whites",
        pairs: &[(17, 16), (16, 17), (17, 12), (17, 5)],
        values: SIGNED,
        exact: &[(17, 16), (16, 17)],
    },
    Case {
        key: "toneVersion",
        slider: "shadows",
        pairs: &[(17, 12), (17, 5), (5, 17)],
        values: SIGNED,
        exact: &[],
    },
    Case {
        key: "toneVersion",
        slider: "highlights",
        pairs: &[(17, 12), (17, 5), (5, 17)],
        values: SIGNED,
        exact: &[],
    },
    Case {
        key: "toneVersion",
        slider: "blacks",
        pairs: &[(17, 12), (17, 5), (5, 17)],
        values: SIGNED,
        exact: &[],
    },
    // Lowered Blacks' travel (version 18): 17's values are 18's exactly; 18's
    // −100 is deeper than 17's reach.
    Case {
        key: "toneVersion",
        slider: "blacks",
        pairs: &[(17, 18), (18, 17), (18, 16)],
        values: &[-100.0, -75.0, -50.0, 50.0],
        exact: &[(17, 18)],
    },
    Case {
        key: "clarityVersion",
        slider: "clarity",
        pairs: &[(3, 1), (3, 2), (1, 3)],
        values: SIGNED,
        exact: &[],
    },
    Case {
        key: "structureVersion",
        slider: "structure",
        pairs: &[(2, 1), (3, 1), (3, 2), (1, 3)],
        values: SIGNED,
        exact: &[(2, 1)],
    },
    // Version 1 values within version 2's reach (+29.5 / −41).
    Case {
        key: "structureVersion",
        slider: "structure",
        pairs: &[(1, 2)],
        values: &[-40.0, -20.0, 20.0, 29.0],
        exact: &[(1, 2)],
    },
    Case {
        key: "vibranceVersion",
        slider: "vibrance",
        pairs: &[(2, 1), (1, 2)],
        values: SIGNED,
        exact: &[],
    },
    // Raising Dehaze is the same in both versions.
    Case {
        key: "dehazeVersion",
        slider: "dehaze",
        pairs: &[(2, 1), (1, 2)],
        values: &[-100.0, -50.0],
        exact: &[],
    },
    Case {
        key: "dehazeVersion",
        slider: "dehaze",
        pairs: &[(2, 1), (1, 2)],
        values: &[50.0, 100.0],
        exact: &[(2, 1), (1, 2)],
    },
    Case {
        key: "temperatureVersion",
        slider: "temperature",
        pairs: &[(2, 1), (1, 2)],
        values: SIGNED,
        exact: &[],
    },
    Case {
        key: "tintVersion",
        slider: "tint",
        pairs: &[(2, 1), (1, 2)],
        values: SIGNED,
        exact: &[],
    },
    Case {
        key: "hslVersion",
        slider: "hsl.reds.saturation",
        pairs: &[(2, 1), (1, 2)],
        values: &[-50.0, 50.0],
        exact: &[],
    },
    Case {
        key: "hslVersion",
        slider: "hsl.greens.hue",
        pairs: &[(2, 1), (1, 2)],
        values: &[-50.0, 50.0],
        exact: &[],
    },
    Case {
        key: "hslVersion",
        slider: "hsl.blues.luminance",
        pairs: &[(2, 1), (1, 2)],
        values: &[-50.0, 50.0],
        exact: &[],
    },
    Case {
        key: "sharpenVersion",
        slider: "sharpness",
        pairs: &[(8, 1), (1, 8)],
        values: &[40.0, 80.0],
        exact: &[],
    },
    Case {
        key: "lumaNoiseVersion",
        slider: "lumaNoiseReduction",
        pairs: &[(2, 1), (1, 2)],
        values: &[30.0, 60.0],
        exact: &[],
    },
];

#[test]
#[ignore = "requires a native GPU; the error of pasted values translated between version keys on generated charts"]
fn pasted_versions_translation_error() {
    let context = context();
    let slot = GpuProcessorSlot::new(None);
    let charts = Charts::new();
    let mut rows = Vec::new();
    let mut report = String::new();
    for case in CASES {
        for &(from, to) in case.pairs {
            for &value in case.values {
                let mut source = base_recipe();
                source[case.key] = json!(from);
                set(&mut source, case.slider, value);
                let translated = translate(
                    case.key,
                    case.slider.split('.').next().unwrap(),
                    value,
                    from,
                    to,
                );
                let mut pasted = base_recipe();
                pasted[case.key] = json!(to);
                set(&mut pasted, case.slider, translated);
                let mut untranslated = pasted.clone();
                set(&mut untranslated, case.slider, value);
                let wanted = render_all(&context, &slot, &charts, &source);
                let got = render_all(&context, &slot, &charts, &pasted);
                let (mean, p95) = error(&wanted, &got);
                let (plain_mean, plain_p95) = if translated == value {
                    (mean, p95)
                } else {
                    error(
                        &wanted,
                        &render_all(&context, &slot, &charts, &untranslated),
                    )
                };
                let exact = case.exact.contains(&(from, to));
                if exact {
                    assert!(
                        wanted.iter().zip(&got).all(|(a, b)| a.0 == b.0),
                        "{} {} {from}→{to} at {value}: translated pasted value must render exactly (ΔE mean {mean:.4})",
                        case.key,
                        case.slider
                    );
                }
                writeln!(
                    report,
                    "{:<18} {:<20} {from:>2}→{to:<2} {value:>6} → {translated:>8.3}  ΔE mean {mean:.3} p95 {p95:.3}  (untranslated {plain_mean:.3} / {plain_p95:.3}){}",
                    case.key,
                    case.slider,
                    if exact { "  exact" } else { "" }
                )
                .unwrap();
                rows.push(json!({
                    "key": case.key, "slider": case.slider, "from": from, "to": to,
                    "value": value, "translated": translated, "exact": exact,
                    "mean": mean, "p95": p95, "untranslatedMean": plain_mean, "untranslatedP95": plain_p95,
                }));
            }
        }
    }
    println!("{report}");
    if let Ok(path) = std::env::var("PASTED_VERSIONS_OUT") {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&json!({ "adapter": adapter_name(), "rows": rows }))
                .unwrap(),
        )
        .unwrap();
    }
}

/// An older edit with a layer using the tool, then the photo's own slider
/// pasted from a newer edit: the layer's half renders as the photo with the
/// user's own slider moved to the translated value (the paste changes
/// nothing else), where carrying the source's key would change it.
#[test]
#[ignore = "requires a native GPU; a pasted global tool leaves the destination's layers as they were"]
fn pasted_global_tool_leaves_the_layers_as_they_were() {
    let context = context();
    let slot = GpuProcessorSlot::new(None);
    let charts = Charts::new();
    // The layer covers the left half.
    let coverage: Vec<f32> = (0..WIDTH * HEIGHT)
        .map(|i| if i % WIDTH < WIDTH / 2 { 1.0 } else { 0.0 })
        .collect();
    let masks = [Arc::new(MaskBitmap::from_coverage(
        WIDTH, HEIGHT, &coverage,
    ))];
    // (key, slider, destination's version, source's, layer value, pasted value)
    let cases: &[(&str, &str, u64, u64, f64, f64)] = &[
        ("structureVersion", "structure", 1, 3, 40.0, 50.0),
        ("clarityVersion", "clarity", 1, 3, 40.0, 50.0),
        ("vibranceVersion", "vibrance", 1, 2, 40.0, 50.0),
        ("dehazeVersion", "dehaze", 1, 2, -30.0, -40.0),
        ("temperatureVersion", "temperature", 1, 2, -30.0, 60.0),
        ("tintVersion", "tint", 1, 2, 30.0, 40.0),
        // (Raised Whites add up to 100 at most: values whose sums stay below.)
        ("toneVersion", "whites", 16, 17, 20.0, 30.0),
        ("toneVersion", "shadows", 12, 17, 40.0, 50.0),
    ];
    for &(key, slider, old, new, layer_value, pasted_value) in cases {
        let mut destination = base_recipe();
        destination[key] = json!(old);
        let mut layer = json!({"id": "dodge", "name": "dodge", "visible": true, "invert": false, "opacity": 100,
            "subMasks": [{"id": "all", "type": "all", "visible": true, "mode": "additive", "parameters": {}}],
            "adjustments": {}});
        layer["adjustments"][slider] = json!(layer_value);
        destination["masks"] = json!([layer]);
        let pasted = json!({ slider: pasted_value, key: new });
        let mut expressed = pasted.clone();
        express_pasted(
            &mut expressed,
            &PasteTarget {
                recipe: &destination,
                is_new: false,
                is_raw: true,
                replace_layers: false,
                merge_groups: false,
            },
        );
        let merged = |pasted: &Value| {
            let mut recipe = destination.clone();
            for (k, v) in pasted.as_object().unwrap() {
                recipe[k] = v.clone();
            }
            recipe
        };
        // The user's own move of the photo's slider, at its version.
        let mut in_place = destination.clone();
        in_place[slider] = json!(translate(key, slider, pasted_value, new, old));
        assert_eq!(merged(&expressed), in_place, "{key} {slider}");
        let render_left = |recipe: &Value| -> Vec<Lab> {
            charts
                .images
                .iter()
                .map(|chart| {
                    let lab = render_with_masks(&context, &slot, chart, recipe, true, &masks);
                    Lab(lab
                        .0
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| (*i as u32) % WIDTH < WIDTH / 2)
                        .map(|(_, p)| *p)
                        .collect())
                })
                .collect()
        };
        let kept = render_left(&merged(&expressed));
        let own = render_left(&in_place);
        // The layer renders in its half.
        let mut without_layer = in_place.clone();
        without_layer["masks"][0]["adjustments"][slider] = json!(0.0);
        assert!(
            render_left(&without_layer)
                .iter()
                .zip(&own)
                .any(|(a, b)| a.0 != b.0),
            "{key} {slider}: the layer renders nothing"
        );
        assert!(
            kept.iter().zip(&own).all(|(a, b)| a.0 == b.0),
            "{key} {slider}: the layer's half changed"
        );
        // Carrying the source's key would have changed the layer's half.
        let carried = render_left(&merged(&pasted));
        let (mean, p95) = error(&own, &carried);
        println!(
            "{key:<18} {slider:<12} {old}→{new}: with the key carried the left half differs by ΔE mean {mean:.3}, p95 {p95:.3}"
        );
    }
}
