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
//! - a layer never painted (no mask content yet): the layer with its
//!   settings and no mask component, so no effect until it is painted, as
//!   in Capture One (notes: benchmark-results/c1q1-2026-11-21/); one whose
//!   mask record is missing the same, named in the import report;
//! - a painted, colour range or AI mask (Capture One stores each as a
//!   raster; notes: benchmark-results/c1l2-2026-11-17/): its pixels as a
//!   `raster` component, over the photo before its lens and transform
//!   corrections as Capture One paints them, named in the import report
//!   (the strokes or colour range themselves are not kept);
//! - a Luma Range on a filled or painted mask: that mask limited by a luma
//!   range with Capture One's own falloff below and above.
//!
//! Capture One draws gradients, like its painted masks, on the photo before
//! its lens correction (notes: benchmark-results/c1l3-2026-11-17/). A
//! gradient's points are moved once, at the import, to where the photo's
//! lens correction shows them (`Through`): an ordinary gradient on the
//! corrected photo then, edited and drawn as any (a straight Capture One
//! line stays straight rather than following the correction's bow).
//!
//! A mask Rawtonic cannot read is left out and named in the import report,
//! never dropped silently or guessed, as are settings without a layer
//! counterpart.
use rawtonic_interop::capture_one::Layer;
use rawtonic_interop::capture_one_masks::{Coverage, Mask, Raster};
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

/// The layer's settings as the Rawtonic layer adjustments they become.
pub fn translated(layer: &Layer) -> Map<String, Value> {
    adjustments(layer).0
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

/// The `raster` components' parameters in `adjustments` that lack the
/// photo's EXIF orientation.
fn rasters_without_orientation(adjustments: &mut Value) -> Vec<&mut Value> {
    let key = rawtonic_engine::mask_generation::SOURCE_ORIENTATION;
    adjustments[masks::MASKS.name()]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter_map(|layer| layer[masks::SUB_MASKS.name()].as_array_mut())
        .flatten()
        .filter(|sub| sub["type"] == "raster")
        .map(|sub| &mut sub[masks::PARAMETERS.name()])
        .filter(|p| p.get(key).is_none())
        .collect()
}

/// Whether imported raster masks still need the photo's EXIF orientation
/// (`set_orientation`).
pub fn needs_orientation(adjustments: &Value) -> bool {
    let key = rawtonic_engine::mask_generation::SOURCE_ORIENTATION;
    adjustments[masks::MASKS.name()]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|layer| layer[masks::SUB_MASKS.name()].as_array())
        .flatten()
        .any(|sub| sub["type"] == "raster" && sub[masks::PARAMETERS.name()].get(key).is_none())
}

/// Records the photo file's EXIF `orientation` in its imported raster masks:
/// Capture One paints in the unrotated sensor frame.
pub fn set_orientation(adjustments: &mut Value, orientation: u16) {
    let key = rawtonic_engine::mask_generation::SOURCE_ORIENTATION;
    for p in rasters_without_orientation(adjustments) {
        p[key] = json!(orientation);
    }
}

/// A `raster` component of Capture One's raster; its bitmap only when
/// `encode` (a report needs none).
fn raster_component(raster: &Raster, encode: bool) -> Result<Value, String> {
    use base64::Engine as _;
    let data = if encode {
        let values = raster.values().ok_or("mask tiles not readable")?;
        let image = image::GrayImage::from_raw(raster.width as u32, raster.height as u32, values)
            .ok_or("mask raster of another size")?;
        let mut png = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Value::String(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        ))
    } else {
        Value::Null
    };
    Ok(sub_mask(
        "raster",
        false,
        100.0,
        "additive",
        json!({"maskDataBase64": data}),
    ))
}

/// The components of a mask, or why it is not imported. Approximations
/// that still import go to `notes`; bitmaps are made only when `encode`.
fn components(
    layer: &Layer,
    notes: &mut Vec<String>,
    encode: bool,
    through: Through,
) -> Result<Vec<Value>, String> {
    use rawtonic_engine::recipe_versions::gradient;
    let (width, height) = layer.frame;
    // A point of Capture One's frame (pixels) where the photo's lens
    // correction shows it, normalised (as drawn where it cannot place it).
    let normal = |(x, y): (f64, f64)| {
        let n = (x / width, y / height);
        through(n.0, n.1).unwrap_or(n)
    };
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
            // The ellipse's centre and axis ends where the lens correction
            // shows them: its axes and turn there.
            let (sin, cos) = rotation.to_radians().sin_cos();
            let pixel = |p: (f64, f64)| {
                let (x, y) = normal(p);
                (x * width, y * height)
            };
            let axis = |(dx, dy): (f64, f64)| {
                let a = pixel((center.0 + dx, center.1 + dy));
                let b = pixel((center.0 - dx, center.1 - dy));
                ((a.0 - b.0) / 2.0, (a.1 - b.1) / 2.0)
            };
            let (ax, ay) = axis((radius_x * cos, radius_x * sin));
            let (bx, by) = axis((-radius_y * sin, radius_y * cos));
            let (radius_x, radius_y) = (ax.hypot(ay), bx.hypot(by));
            let turn = ay.atan2(ax).to_degrees();
            let center = pixel(*center);
            // Lightroom's form: opposite corners of the ellipse's rectangle
            // turned by the angle, which placement projects back onto the
            // turned axes (`lightroom_masks::placed_parameters`).
            let (sin, cos) = turn.to_radians().sin_cos();
            let (ox, oy) = (
                radius_x * cos - radius_y * sin,
                radius_x * sin + radius_y * cos,
            );
            let at = |(x, y): (f64, f64)| (x / width, y / height);
            let (left, top) = at((center.0 - ox, center.1 - oy));
            let (right, bottom) = at((center.0 + ox, center.1 + oy));
            Ok(vec![sub_mask(
                "radial",
                *outside,
                100.0,
                "additive",
                json!({"lrNormalized": {
                    "top": top, "left": left, "bottom": bottom, "right": right,
                    "angle": turn,
                    "feather": ((1.0 - inner) * 100.0).clamp(0.0, 100.0),
                    gradient::KEY: gradient::SMOOTH,
                }}),
            )])
        }
        Mask::Raster { raster, ai } => {
            notes.push(
                if *ai {
                    "AI mask imported as a bitmap"
                } else {
                    "painted or colour range mask imported as a bitmap"
                }
                .into(),
            );
            Ok(vec![raster_component(raster, encode)?])
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
            let base = match base {
                Coverage::Uniform { value } => {
                    sub_mask("all", false, *value as f64 / 2.55, "additive", json!({}))
                }
                Coverage::Raster { raster } => {
                    notes.push("painted mask imported as a bitmap".into());
                    raster_component(raster, encode)?
                }
            };
            Ok(vec![
                base,
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
        // A layer never painted keeps its settings with an empty mask (no
        // effect until painted, as in Capture One); one whose mask record
        // is not there is kept the same way and named (C1Q1).
        Mask::Empty => Ok(Vec::new()),
        Mask::Missing => {
            notes.push(
                "mask not found in Capture One's mask files, imported without a mask (paint it again)"
                    .into(),
            );
            Ok(Vec::new())
        }
        Mask::Unsupported { what } => Err(what.clone()),
        _ => Err("gradient without a frame".into()),
    }
}

/// Rawtonic layers for Capture One's `layers` (bottom first), and the
/// report lines for what was left out or approximated.
pub fn layers(layers: &[Layer], through: Through) -> (Vec<Value>, Vec<String>) {
    convert(layers, true, through)
}

/// Where the photo's lens correction shows a point of the photo before it
/// (both normalised to the unrotated frame; `lightroom_upright::through_lens`),
/// None where it cannot place it.
pub type Through<'a> = &'a dyn Fn(f64, f64) -> Option<(f64, f64)>;

/// No lens correction: points stay where they are drawn.
pub fn uncorrected(x: f64, y: f64) -> Option<(f64, f64)> {
    Some((x, y))
}

/// The report lines of `layers` alone, without making their bitmaps.
pub fn report(layers: &[Layer]) -> Vec<String> {
    convert(layers, false, &uncorrected).1
}

fn convert(layers: &[Layer], encode: bool, through: Through) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let mut report = Vec::new();
    for layer in layers {
        let mut notes = Vec::new();
        match components(layer, &mut notes, encode, through) {
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

    /// The conversion on a photo without lens correction.
    fn layers(layers: &[Layer]) -> (Vec<Value>, Vec<String>) {
        super::layers(layers, &uncorrected)
    }

    /// Gradients drawn before the lens correction arrive where the
    /// correction shows their points: here a correction that spreads the
    /// photo by 2 % about its centre.
    #[test]
    fn gradients_arrive_where_the_lens_correction_shows_them() {
        let spread = |x: f64, y: f64| Some((0.5 + (x - 0.5) * 1.02, 0.5 + (y - 0.5) * 1.02));
        let (out, _) = super::layers(
            &[
                layer(
                    Mask::Linear {
                        start: (3680.0, 491.2),
                        end: (3680.0, 2456.0),
                        midpoint: 0.5,
                    },
                    &[("Exposure", "-1")],
                ),
                layer(
                    Mask::Radial {
                        center: (1840.0, 1228.0),
                        rotation: 0.0,
                        radius_x: 736.0,
                        radius_y: 491.2,
                        reserved: 0.0,
                        inner: 0.5,
                        outside: false,
                    },
                    &[("Exposure", "1")],
                ),
            ],
            &spread,
        );
        let near = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 1e-9;
        let g = &out[0]["subMasks"][0]["parameters"]["lrNormalized"];
        assert!(near(&g["fullX"], 0.5) && near(&g["fullY"], 0.5 - 0.4 * 1.02));
        assert!(near(&g["zeroX"], 0.5) && near(&g["zeroY"], 0.5));
        let g = &out[1]["subMasks"][0]["parameters"]["lrNormalized"];
        // Centre 0.25 → 0.245, half-axes 0.1 → 0.102 of each side.
        assert!(near(&g["left"], 0.245 - 0.102) && near(&g["right"], 0.245 + 0.102));
        assert!(near(&g["top"], 0.245 - 0.102) && near(&g["bottom"], 0.245 + 0.102));
        assert!(g["angle"].as_f64().unwrap().abs() < 1e-9);
        assert!(g.get("uncorrected").is_none());
    }

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
    fn rotated_radials_keep_their_radii() {
        let (out, _) = layers(&[layer(
            Mask::Radial {
                center: (3000.0, 2000.0),
                rotation: 30.0,
                radius_x: 200.0,
                radius_y: 100.0,
                reserved: 0.0,
                inner: 0.5,
                outside: false,
            },
            &[("Exposure", "1")],
        )]);
        let mut adjustments = json!({"masks": out});
        assert!(rawtonic_engine::lightroom_masks::place(
            &mut adjustments,
            7360.0,
            4912.0,
            Some(1)
        ));
        let placed = &adjustments["masks"][0]["subMasks"][0]["parameters"];
        let near = |k: &str, want: f64| (placed[k].as_f64().unwrap() - want).abs() < 0.02;
        assert!(
            near("centerX", 3000.0) && near("centerY", 2000.0),
            "{placed}"
        );
        assert!(near("radiusX", 200.0) && near("radiusY", 100.0), "{placed}");
        assert!(near("rotation", 30.0), "{placed}");
    }

    #[test]
    fn filled_and_luma_range_masks_cover_the_photo() {
        let (out, report) = layers(&[
            layer(Mask::Uniform { value: 255 }, &[("Exposure", "1")]),
            layer(
                Mask::LumaRange {
                    base: Coverage::Uniform { value: 255 },
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

    /// A 130 × 70 raster: a soft disc of up to 200 around (40, 30).
    fn disc() -> Raster {
        let values: Vec<u8> = (0..130 * 70)
            .map(|i| {
                let (x, y) = ((i % 130) as f64 - 40.0, (i / 130) as f64 - 30.0);
                (200.0 * (1.0 - (x * x + y * y).sqrt() / 25.0)).clamp(0.0, 200.0) as u8
            })
            .collect();
        Raster::from_values(130, 70, &values)
    }

    fn bitmap(sub: &Value) -> image::GrayImage {
        use base64::Engine as _;
        let url = sub["parameters"]["maskDataBase64"].as_str().unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        image::load_from_memory(&png).unwrap().into_luma8()
    }

    #[test]
    fn painted_colour_range_and_ai_masks_come_over_as_bitmaps() {
        let raster = disc();
        let want = raster.values().unwrap();
        let painted = layer(
            Mask::Raster {
                raster: raster.clone(),
                ai: false,
            },
            &[("Exposure", "-2")],
        );
        let ai = layer(
            Mask::Raster {
                raster: raster.clone(),
                ai: true,
            },
            &[("Exposure", "1")],
        );
        let luma = layer(
            Mask::LumaRange {
                base: Coverage::Raster {
                    raster: raster.clone(),
                },
                low: 60.0,
                high: 170.0,
                falloff_low: 30.0,
                falloff_high: 210.0,
                radius: 0.0,
                sensitivity: 50.0,
                invert: false,
            },
            &[("Exposure", "-1")],
        );
        let all = [painted, ai, luma];
        let (out, report) = layers(&all);
        assert_eq!(out.len(), 3);
        for l in &out {
            let sub = &l["subMasks"][0];
            assert_eq!(
                (sub["type"].as_str(), sub["mode"].as_str()),
                (Some("raster"), Some("additive"))
            );
            let b = bitmap(sub);
            assert_eq!(b.dimensions(), (130, 70));
            assert_eq!(b.into_raw(), want);
        }
        assert_eq!(out[0]["adjustments"]["exposure"], -2.0);
        assert_eq!(out[2]["subMasks"][1]["type"], "luma-range");
        assert_eq!(out[2]["subMasks"][1]["mode"], "intersect");
        assert_eq!(
            report,
            [
                "Capture One layer “L”: painted or colour range mask imported as a bitmap",
                "Capture One layer “L”: AI mask imported as a bitmap",
                "Capture One layer “L”: painted mask imported as a bitmap",
            ]
        );
        // The report alone names the same, without making the bitmaps.
        assert_eq!(super::report(&all), report);
        // The file's EXIF orientation goes with each raster, once.
        let mut adjustments = json!({"masks": out});
        assert!(needs_orientation(&adjustments));
        set_orientation(&mut adjustments, 6);
        set_orientation(&mut adjustments, 8);
        assert!(!needs_orientation(&adjustments));
        assert_eq!(
            adjustments["masks"][2]["subMasks"][0]["parameters"]["sourceOrientation"],
            6
        );
        assert!(
            adjustments["masks"][2]["subMasks"][1]["parameters"]["sourceOrientation"].is_null()
        );
    }

    /// C1Q1: a layer made and never painted (or whose mask record is not
    /// in the mask files) keeps its name, switch and settings with an empty
    /// mask (no effect until painted, as in Capture One); a filled layer
    /// covers the photo. Only the missing record is named in the report.
    #[test]
    fn never_painted_layers_come_over_with_their_settings() {
        let mut never = layer(Mask::Empty, &[("Exposure", "-3")]);
        never.name = "Adjustment Layer 1".into();
        let mut gone = layer(Mask::Missing, &[("Exposure", "-2")]);
        gone.name = "Lost".into();
        gone.enabled = false;
        let full = layer(Mask::Uniform { value: 255 }, &[("Exposure", "-1")]);
        let (out, report) = layers(&[never, gone, full]);
        assert_eq!(out.len(), 3, "{report:?}");
        assert_eq!(out[0]["name"], "Adjustment Layer 1");
        assert_eq!(out[0]["adjustments"]["exposure"], -3.0);
        assert_eq!(out[0]["subMasks"], json!([]));
        assert_eq!(out[1]["visible"], false);
        assert_eq!(out[1]["adjustments"]["exposure"], -2.0);
        assert_eq!(out[1]["subMasks"], json!([]));
        assert_eq!(out[2]["subMasks"][0]["type"], "all");
        assert_eq!(out[2]["subMasks"][0]["opacity"], 100.0);
        assert_eq!(
            report,
            [
                "Capture One layer “Lost”: mask not found in Capture One's mask files, imported without a mask (paint it again)"
            ]
        );
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
