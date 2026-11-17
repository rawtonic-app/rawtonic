//! Capture One's local adjustment layers as Rawtonic layers (G50; the
//! measurements behind the choices: notes: benchmark-results/c1l1-2026-11-17/).
//!
//! A layer's settings translate as the photo's do (`costyle`; Capture One
//! renders a filled layer exactly as the same global slider, ATL5), limited
//! to what a Rawtonic layer adjusts. Its mask becomes:
//!
//! - a linear gradient: kept normalised in the frame Capture One draws it in
//!   (`lrNormalized`, placed like a Lightroom gradient), full effect on the
//!   start line, none on the end line, the eased ramp between (Capture One's
//!   smoothstep, `gradientVersion` 2);
//! - a radial gradient: the ellipse with its inner half at full effect and
//!   the smoothstep out to the ellipse (`gradientVersion` 3), inverted when
//!   Capture One applies it outside (as drawn by default);
//! - a filled (or cleared) mask: the whole photo at that value;
//! - a Luma Range on a filled mask: the whole photo limited by a luma range
//!   with Capture One's own falloff below and above.
//!
//! Painted, colour range and AI masks are compressed rasters Rawtonic does
//! not read yet: such a layer is left out and named in the import report,
//! never dropped silently, as are settings without a layer counterpart.
use rawtonic_interop::capture_one::Layer;
use rawtonic_interop::capture_one_masks::Mask;
use rawtonic_recipe::color as recipe_color;
use rawtonic_recipe::detail;
use rawtonic_recipe::masks;
use rawtonic_recipe::tone;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Keys of the translated settings a Rawtonic layer applies
/// (`rawtonic_recipe::masks::LocalAdjustments`).
fn local_key(key: &str) -> bool {
    [
        tone::EXPOSURE.name(),
        tone::BRIGHTNESS.name(),
        tone::CONTRAST.name(),
        tone::HIGHLIGHTS.name(),
        tone::SHADOWS.name(),
        tone::WHITES.name(),
        tone::BLACKS.name(),
        recipe_color::TEMPERATURE.name(),
        recipe_color::TINT.name(),
        recipe_color::VIBRANCE.name(),
        recipe_color::SATURATION.name(),
        recipe_color::HUE.name(),
        recipe_color::COLOR_GRADING.name(),
        recipe_color::HSL.name(),
        recipe_color::COLOR_RANGES.name(),
        detail::SHARPNESS.name(),
        detail::LUMA_NOISE_REDUCTION.name(),
        detail::COLOR_NOISE_REDUCTION.name(),
        detail::CLARITY.name(),
        detail::DEHAZE.name(),
        detail::STRUCTURE.name(),
        detail::MOIRE_AMOUNT.name(),
        detail::MOIRE_PATTERN.name(),
        masks::CURVES.name(),
        masks::DEFRINGE.name(),
    ]
    .contains(&key)
}

/// The layer's settings as Rawtonic layer adjustments, and the names of
/// those that have no layer counterpart.
fn adjustments(layer: &Layer) -> (Map<String, Value>, Vec<String>) {
    if layer.settings.is_empty() {
        return (Map::new(), Vec::new());
    }
    let style: String = layer
        .settings
        .iter()
        .map(|(k, v)| format!(r#"<E K="{}" V="{}"/>"#, escape(k), escape(v)))
        .collect();
    let style = format!("<SL>{style}</SL>");
    let mut left_out: Vec<String> = crate::costyle::untranslated_settings(&style)
        .into_iter()
        .map(str::to_string)
        .collect();
    let translated = crate::costyle::convert_costyle_to_preset(&style, "Layer")
        .ok()
        .and_then(|p| p.adjustments.as_object().cloned())
        .unwrap_or_default();
    let mut out = Map::new();
    for (key, value) in translated {
        if local_key(&key) {
            out.insert(key, value);
        } else if !is_neutral(&value) {
            left_out.push(key);
        }
    }
    (out, left_out)
}

fn is_neutral(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sub_mask(kind: &str, invert: bool, opacity: f64, mode: &str, parameters: Value) -> Value {
    json!({
        "id": format!("c1-{kind}-{}", Uuid::new_v4()),
        "type": kind,
        masks::VISIBLE.name(): true,
        masks::INVERT.name(): invert,
        masks::OPACITY.name(): opacity.clamp(0.0, 100.0),
        "mode": mode,
        masks::PARAMETERS.name(): parameters,
    })
}

/// The components of a mask, or why it is not imported. Approximations
/// that still import go to `notes`.
fn components(layer: &Layer, notes: &mut Vec<String>) -> Result<Vec<Value>, String> {
    use rawtonic_engine::recipe_versions::gradient;
    let (width, height) = layer.frame;
    let normal = |(x, y): (f64, f64)| (x / width, y / height);
    let framed = width > 0.0 && height > 0.0;
    match &layer.mask {
        Mask::Uniform { value } => Ok(vec![sub_mask(
            "all",
            false,
            *value as f64 / 2.55,
            "additive",
            json!({}),
        )]),
        Mask::Linear {
            start,
            end,
            midpoint,
        } if framed => {
            if (midpoint - 0.5).abs() > 1e-3 {
                notes.push(format!(
                    "linear gradient midpoint {midpoint:.2} imported as 0.5"
                ));
            }
            let (fx, fy) = normal(*start);
            let (zx, zy) = normal(*end);
            Ok(vec![sub_mask(
                "linear",
                false,
                100.0,
                "additive",
                json!({"lrNormalized": {"zeroX": zx, "zeroY": zy, "fullX": fx, "fullY": fy,
                    gradient::KEY: gradient::BELL}}),
            )])
        }
        Mask::Radial {
            center,
            rotation,
            radius_x,
            radius_y,
            reserved,
            inner,
            outside,
        } if framed => {
            if *rotation != 0.0 {
                notes.push(format!(
                    "radial gradient rotated by {rotation:.1}° (rotation direction not verified)"
                ));
            }
            if *reserved != 0.0 {
                notes.push(format!("radial gradient value {reserved} not read"));
            }
            let (left, top) = normal((center.0 - radius_x, center.1 - radius_y));
            let (right, bottom) = normal((center.0 + radius_x, center.1 + radius_y));
            Ok(vec![sub_mask(
                "radial",
                *outside,
                100.0,
                "additive",
                json!({"lrNormalized": {
                    "top": top, "left": left, "bottom": bottom, "right": right,
                    "angle": rotation,
                    "feather": ((1.0 - inner) * 100.0).clamp(0.0, 100.0),
                    gradient::KEY: gradient::SMOOTH,
                }}),
            )])
        }
        Mask::LumaRange {
            base,
            low,
            high,
            falloff_low,
            falloff_high,
            radius,
            invert,
            ..
        } => {
            if *radius > 0.0 {
                notes.push(format!("luma range edge radius {radius:.0} not applied"));
            }
            let percent = |v: f64| (v / 2.55).clamp(0.0, 100.0);
            let (low, high) = (low.min(*high), low.max(*high));
            let below = percent(low - falloff_low.min(low));
            let above = percent(falloff_high.max(high) - high);
            Ok(vec![
                sub_mask("all", false, *base as f64 / 2.55, "additive", json!({})),
                sub_mask(
                    "luma-range",
                    *invert,
                    100.0,
                    "intersect",
                    json!({"rangeLow": percent(low), "rangeHigh": percent(high),
                        "falloff": (below + above) / 2.0,
                        "falloffLow": below, "falloffHigh": above}),
                ),
            ])
        }
        Mask::Unsupported { what } => Err(what.clone()),
        _ => Err("gradient without a frame".into()),
    }
}

/// Rawtonic layers for Capture One's `layers` (bottom first), and the
/// report lines for what was left out or approximated.
pub fn layers(layers: &[Layer]) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let mut report = Vec::new();
    for layer in layers {
        let mut notes = Vec::new();
        match components(layer, &mut notes) {
            Ok(sub_masks) => {
                let (adjustments, left_out) = adjustments(layer);
                if !left_out.is_empty() {
                    notes.push(format!("settings not applied: {}", left_out.join(", ")));
                }
                out.push(json!({
                    "id": format!("c1-mask-{}", Uuid::new_v4()),
                    "name": layer.name,
                    masks::VISIBLE.name(): layer.enabled,
                    masks::INVERT.name(): false,
                    masks::OPACITY.name(): layer.opacity,
                    masks::ADJUSTMENTS.name(): Value::Object(adjustments),
                    masks::SUB_MASKS.name(): sub_masks,
                }));
            }
            Err(why) => notes.push(format!("not imported ({why})")),
        }
        report.extend(
            notes
                .into_iter()
                .map(|n| format!("Capture One layer “{}”: {n}", layer.name)),
        );
    }
    (out, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(mask: Mask, settings: &[(&str, &str)]) -> Layer {
        Layer {
            name: "L".into(),
            enabled: true,
            opacity: 80.0,
            settings: settings
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            mask,
            frame: (7360.0, 4912.0),
        }
    }

    #[test]
    fn gradients_keep_capture_ones_geometry_until_placed() {
        let (out, report) = layers(&[
            layer(
                Mask::Linear {
                    start: (3680.0, 491.2),
                    end: (3680.0, 2456.0),
                    midpoint: 0.5,
                },
                &[("Exposure", "-1.2"), ("Contrast", "30")],
            ),
            layer(
                Mask::Radial {
                    center: (3680.0, 2456.0),
                    rotation: 0.0,
                    radius_x: 736.0,
                    radius_y: 491.2,
                    reserved: 0.0,
                    inner: 0.5,
                    outside: true,
                },
                &[("Exposure", "1"), ("Clarity", "40")],
            ),
        ]);
        assert!(report.is_empty(), "{report:?}");
        let linear = &out[0]["subMasks"][0];
        assert_eq!(linear["type"], "linear");
        let g = &linear["parameters"]["lrNormalized"];
        let near = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 1e-9;
        assert!(near(&g["fullX"], 0.5) && near(&g["fullY"], 0.1));
        assert!(near(&g["zeroX"], 0.5) && near(&g["zeroY"], 0.5));
        assert_eq!(out[0]["opacity"], 80.0);
        assert_eq!(out[0]["adjustments"]["exposure"], -1.2);
        let radial = &out[1]["subMasks"][0];
        assert_eq!(
            (radial["type"].as_str(), radial["invert"].as_bool()),
            (Some("radial"), Some(true))
        );
        let g = &radial["parameters"]["lrNormalized"];
        assert!(near(&g["left"], 0.4) && near(&g["right"], 0.6));
        assert!(near(&g["top"], 0.4) && near(&g["bottom"], 0.6));
        assert_eq!(g["feather"], 50.0);
        assert_eq!(g["gradientVersion"], 3);
        assert!(out[1]["adjustments"]["clarity"].as_f64().is_some());
        // Placed on a photo as the editor places Lightroom's gradients.
        let mut adjustments = json!({"masks": out});
        assert!(rawtonic_engine::lightroom_masks::place(
            &mut adjustments,
            7360.0,
            4912.0,
            Some(1)
        ));
        let placed = &adjustments["masks"][1]["subMasks"][0]["parameters"];
        assert_eq!(
            (
                placed["centerX"].as_f64(),
                placed["radiusX"].as_f64(),
                placed["radiusY"].as_f64()
            ),
            (Some(3680.0), Some(736.0), Some(491.2))
        );
        assert_eq!(placed["gradientVersion"], 3);
    }

    #[test]
    fn filled_and_luma_range_masks_cover_the_photo() {
        let (out, report) = layers(&[
            layer(Mask::Uniform { value: 255 }, &[("Exposure", "1")]),
            layer(
                Mask::LumaRange {
                    base: 255,
                    low: 60.0,
                    high: 170.0,
                    falloff_low: 30.0,
                    falloff_high: 210.0,
                    radius: 0.0,
                    sensitivity: 50.0,
                    invert: false,
                },
                &[("Exposure", "-1")],
            ),
        ]);
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(out[0]["subMasks"][0]["type"], "all");
        assert_eq!(out[0]["subMasks"][0]["opacity"], 100.0);
        let range = &out[1]["subMasks"][1];
        assert_eq!(
            (range["type"].as_str(), range["mode"].as_str()),
            (Some("luma-range"), Some("intersect"))
        );
        let p = &range["parameters"];
        let near = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 1e-6;
        assert!(near(&p["rangeLow"], 60.0 / 2.55) && near(&p["rangeHigh"], 170.0 / 2.55));
        assert!(near(&p["falloffLow"], 30.0 / 2.55) && near(&p["falloffHigh"], 40.0 / 2.55));
    }

    #[test]
    fn unread_masks_and_settings_are_named() {
        let (out, report) = layers(&[
            layer(
                Mask::Unsupported {
                    what: "painted or colour range mask".into(),
                },
                &[("Exposure", "1")],
            ),
            layer(Mask::Uniform { value: 255 }, &[("FilmGrainAmount", "40")]),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(report.len(), 2, "{report:?}");
        assert!(report[0].contains("not imported (painted or colour range mask)"));
        assert!(report[1].contains("settings not applied"), "{report:?}");
    }
}
