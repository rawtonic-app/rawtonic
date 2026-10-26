//! Lightroom's mask geometry (linear and radial gradients, brush strokes,
//! colour range samples) as imported, kept as Lightroom stores it
//! (`lrNormalized`: normalised in the unrotated raw) until it is placed in
//! a photo's pixels, and rendered from that form wherever the photo's full
//! decoded size is known.
//!
//! As for spots (`lightroom_retouch`), an import knows the photo's EXIF
//! orientation but not its decoded size. The catalog import records the
//! orientation in each pending component (`orientation` inside
//! `lrNormalized`, [`set_orientation`]); a render that knows the full
//! decode's size (`PhotoGeometry::with_decoded_size`) then places such a
//! layer with [`placed_parameters`] before drawing it, so a photo exported
//! (app, batch, CLI) before it is ever opened renders its masks as the
//! editor does once it places them (`place`): both run the same placement
//! on the same size. The recorded orientation is also what gates this:
//! components imported before it was recorded render as they always did
//! (left out) until the editor places them, or until the catalog is
//! imported again. Select Subject and Select Sky need a model and keep
//! waiting for the open photo.

use serde_json::{Value, json};

use crate::lightroom_retouch::{ORIENTATION, PENDING, orient_direction, orient_point};
use crate::masks::MaskDefinition;

/// Lightroom's brush flow as the coverage of one dab: Camera Raw renders a
/// single hard dab at Flow 0.5 with 0.105 and at 0.9 with 0.53, which
/// `f / (8.12 − 7.12 f)` matches within 0.004 over 0.05–1.
fn dab_coverage(flow: f64) -> f64 {
    let f = flow.clamp(0.0, 1.0);
    f / (8.12 - 7.12 * f)
}

/// The orientation recorded in a pending component's `parameters`.
fn orientation_of(parameters: &Value) -> Option<u16> {
    parameters[PENDING][ORIENTATION]
        .as_u64()
        .and_then(|o| u16::try_from(o).ok())
        .filter(|o| (1..=8).contains(o))
}

/// The `parameters` of the recipe's mask components still in Lightroom's
/// form.
fn pending_parameters_mut(adjustments: &mut Value) -> impl Iterator<Item = &mut Value> {
    adjustments
        .get_mut("masks")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get_mut("subMasks").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(|sub| sub.get_mut("parameters"))
        .filter(|p| p.get(PENDING).is_some_and(Value::is_object))
}

/// Whether a pending mask component of the recipe lacks its photo's
/// orientation.
pub fn needs_orientation(adjustments: &Value) -> bool {
    adjustments
        .get("masks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("subMasks").and_then(Value::as_array))
        .flatten()
        .map(|sub| &sub["parameters"])
        .any(|p| p.get(PENDING).is_some_and(Value::is_object) && orientation_of(p).is_none())
}

/// Records `orientation` (the photo's, `image_loader::source_orientation`)
/// in the pending mask components that lack one: done by the catalog
/// import, which makes renders place them (see the module comment).
pub fn set_orientation(adjustments: &mut Value, orientation: u16) {
    for parameters in pending_parameters_mut(adjustments) {
        if orientation_of(parameters).is_none() {
            parameters[PENDING][ORIENTATION] = json!(orientation);
        }
    }
}

/// Lightroom's geometry `g` of a component of kind `kind` placed in a photo
/// of `width` × `height` coordinate pixels (as decoded, EXIF orientation
/// applied) whose orientation is `orientation`: Rawtonic's parameters for
/// it, on the photo's own frame. Lightroom keeps its geometry in the
/// unrotated raw (Camera Raw puts a dab at raw (0.3, 0.5) of an
/// orientation-8 photo at (0.5, 0.7)). None when it has no shape there.
pub fn placed_parameters(
    kind: &str,
    g: &Value,
    width: f64,
    height: f64,
    orientation: u16,
) -> Option<Value> {
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let pixel = |x: f64, y: f64| {
        let (u, v) = orient_point(orientation, x, y);
        (u * width, v * height)
    };
    let n = |k: &str| g[k].as_f64().unwrap_or(0.0);
    // Lightroom's geometry belongs to the photo, not to how it is shown: the
    // placed shapes record the photo's own frame (`mask_generation`'s
    // `ShapeFrame`), so they are drawn on the same spot of the photo however
    // it is turned, flipped or straightened.
    let frame = json!({"width": width, "height": height, "rotation": 0.0,
        "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0});
    Some(match kind {
        "linear" => {
            let (zx, zy) = pixel(n("zeroX"), n("zeroY"));
            let (fx, fy) = pixel(n("fullX"), n("fullY"));
            let (dx, dy) = (fx - zx, fy - zy);
            let length = dx.hypot(dy);
            if length <= 1.0 {
                return None;
            }
            let (mx, my) = ((zx + fx) / 2.0, (zy + fy) / 2.0);
            let (lx, ly) = (-dy / 2.0, dx / 2.0);
            json!({"startX": round(mx - lx), "startY": round(my - ly),
                "endX": round(mx + lx), "endY": round(my + ly), "range": round(length / 2.0),
                "frame": frame})
        }
        "radial" => {
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
        "brush" => {
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
                            "flow": round(dab_coverage(stroke["flow"].as_f64().unwrap_or(1.0)) * 100.0),
                            "lrCenterWeight": stroke["centerWeight"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0),
                            "autoMask": stroke["autoMask"].as_bool().unwrap_or(false),
                            "frame": frame,
                        })
                    })
                })
                .collect();
            json!({"lines": lines})
        }
        "color" => {
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
            let first = samples.first().cloned()?;
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
        _ => return None,
    })
}

/// Places the recipe's pending mask components in a photo of `width` ×
/// `height` coordinate pixels (`orientation` for components that record
/// none); components without a shape there stay pending. Returns whether
/// any was placed.
pub fn place(adjustments: &mut Value, width: f64, height: f64, orientation: Option<u16>) -> bool {
    let mut placed = false;
    let Some(masks) = adjustments.get_mut("masks").and_then(Value::as_array_mut) else {
        return false;
    };
    for sub in masks
        .iter_mut()
        .filter_map(|m| m.get_mut("subMasks").and_then(Value::as_array_mut))
        .flatten()
    {
        let parameters = &sub["parameters"];
        let Some(g) = parameters.get(PENDING) else {
            continue;
        };
        let Some(o) = orientation_of(parameters).or(orientation) else {
            continue;
        };
        if let Some(resolved) = sub["type"]
            .as_str()
            .and_then(|kind| placed_parameters(kind, g, width, height, o))
        {
            sub["parameters"] = resolved;
            placed = true;
        }
    }
    placed
}

/// Moves the pending mask components onto the same content of another
/// frame of the raw (`source_frame::migrate`): their normalised points
/// through the unrotated frames' offset, brush radii (shares of the long
/// side) with it.
pub fn move_pending(adjustments: &mut Value, change: &crate::source_frame::FrameChange) {
    let frames = crate::lightroom_retouch::UnrotatedFrames::of(change);
    for parameters in pending_parameters_mut(adjustments) {
        let g = &mut parameters[PENDING];
        frames.move_point(g, "zeroX", "zeroY");
        frames.move_point(g, "fullX", "fullY");
        frames.move_point(g, "left", "top");
        frames.move_point(g, "right", "bottom");
        if let Some(strokes) = g.get_mut("strokes").and_then(Value::as_array_mut) {
            for stroke in strokes {
                frames.scale(stroke, "radius", frames.long_scale());
                if let Some(dabs) = stroke.get_mut("dabs").and_then(Value::as_array_mut) {
                    for dab in dabs {
                        frames.move_pair(dab);
                    }
                }
            }
        }
        if let Some(points) = g.get_mut("points").and_then(Value::as_array_mut) {
            for point in points {
                frames.move_pair(point);
            }
        }
        if let Some(areas) = g.get_mut("areas").and_then(Value::as_array_mut) {
            for corners in areas.iter_mut().filter_map(Value::as_array_mut) {
                for i in 0..corners.len() / 2 {
                    if let (Some(x), Some(y)) =
                        (corners[2 * i].as_f64(), corners[2 * i + 1].as_f64())
                    {
                        let (x, y) = frames.point(x, y);
                        corners[2 * i] = json!(x);
                        corners[2 * i + 1] = json!(y);
                    }
                }
            }
        }
    }
}

/// `def` with its pending components placed for a render of a photo whose
/// full decode is `width` × `height` (see the module comment); None when it
/// has none that can be placed (no orientation recorded, or no shape).
pub(crate) fn placed_layer(
    def: &MaskDefinition,
    (width, height): (f64, f64),
) -> Option<MaskDefinition> {
    let mut placed = None::<MaskDefinition>;
    for (i, sub) in def.sub_masks.iter().enumerate() {
        let Some(g) = sub.parameters.get(PENDING) else {
            continue;
        };
        let Some(parameters) = orientation_of(&sub.parameters)
            .and_then(|o| placed_parameters(&sub.mask_type, g, width, height, o))
        else {
            continue;
        };
        placed.get_or_insert_with(|| def.clone()).sub_masks[i].parameters = parameters;
    }
    placed
}

/// Whether a pending brush component holds an Auto Mask stroke (its layer
/// needs the photo's pixels, as a placed one does).
pub(crate) fn has_auto_mask_stroke(parameters: &Value) -> bool {
    parameters[PENDING]["strokes"]
        .as_array()
        .is_some_and(|strokes| strokes.iter().any(|s| s["autoMask"] == true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask_generation::{
        PhotoGeometry, generate_layer_mask, prepare_parametric_mask_source,
    };
    use image::{DynamicImage, Rgb32FImage};

    /// A catalog import's layers (as `preset_converter::lightroom_masks`
    /// writes them), the orientation recorded when `orientation` is given.
    fn imported(orientation: Option<u16>) -> Value {
        let sub = |kind: &str, mode: &str, mut g: Value| {
            if let Some(o) = orientation {
                g[ORIENTATION] = json!(o);
            }
            json!({"id": format!("lr-{kind}-{mode}"), "type": kind, "visible": true,
                "invert": false, "opacity": 100.0, "mode": mode,
                "parameters": {PENDING: g}})
        };
        let layer = |id: &str, version: Option<u32>, subs: Vec<Value>| {
            let mut layer = json!({"id": id, "name": id, "visible": true, "invert": false,
                "opacity": 100.0, "adjustments": {"exposure": 1.0}, "subMasks": subs});
            if let Some(v) = version {
                layer["maskVersion"] = json!(v);
            }
            layer
        };
        json!({"masks": [
            layer("gradients", None, vec![
                sub("linear", "additive", json!({"zeroX": 0.55, "zeroY": 0.4, "fullX": 0.5, "fullY": 0.1})),
                sub("radial", "subtractive", json!({"top": 0.2, "left": 0.3, "bottom": 0.6,
                    "right": 0.7, "angle": 15.0, "feather": 80.0})),
            ]),
            layer("precise", Some(2), vec![
                sub("radial", "additive", json!({"top": 0.1, "left": 0.1, "bottom": 0.5,
                    "right": 0.6, "angle": -30.0, "feather": 40.0})),
            ]),
            layer("brush", None, vec![
                sub("brush", "additive", json!({"strokes": [
                    {"dabs": [[0.3, 0.5], [0.35, 0.52], [0.4, 0.55]], "radius": 0.05,
                        "flow": 0.7, "centerWeight": 0.5, "autoMask": false},
                    {"dabs": [[0.6, 0.3], [0.62, 0.35]], "radius": 0.04,
                        "flow": 1.0, "centerWeight": 0.0, "autoMask": true}]})),
            ]),
            layer("color", None, vec![
                sub("color", "additive", json!({"points": [[0.25, 0.75]],
                    "areas": [[0.5, 0.5, 0.7, 0.5, 0.7, 0.6, 0.5, 0.6]], "refine": 40.0})),
            ]),
        ]})
    }

    /// The photo as decoded: 300 × 200 with colours varying across it.
    fn photo() -> DynamicImage {
        DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(300, 200, |x, y| {
            image::Rgb([
                x as f32 / 300.0,
                y as f32 / 200.0,
                ((x + y) % 37) as f32 / 37.0,
            ])
        }))
    }

    /// Each layer's mask of `adjustments` drawn as the editor's or an
    /// export's render draws it, at half size.
    fn render(adjustments: &Value, geometry: &PhotoGeometry) -> Vec<(Vec<u8>, Option<Vec<u16>>)> {
        let defs = crate::image_processing::mask_definitions_from_json(adjustments);
        let source = defs
            .iter()
            .any(MaskDefinition::requires_warped_image)
            .then(|| prepare_parametric_mask_source(&photo(), false, 1.0, adjustments).unwrap());
        let steps = adjustments["orientationSteps"].as_u64().unwrap_or(0);
        let (w, h) = if steps % 2 == 1 {
            (100, 150)
        } else {
            (150, 100)
        };
        defs.iter()
            .map(|def| {
                let mask =
                    generate_layer_mask(def, w, h, 0.5, (1.5, 2.0), geometry, source.as_ref())
                        .unwrap();
                (mask.as_raw().clone(), mask.wide().map(<[u16]>::to_vec))
            })
            .collect()
    }

    #[test]
    fn pending_masks_render_as_the_editor_places_them() {
        for (orientation, shown) in [
            (1, json!({})),
            (6, json!({})),
            (8, json!({"orientationSteps": 1, "rotation": 4.5})),
            (
                3,
                json!({"flipHorizontal": true, "orientationSteps": 3, "rotation": -2.0}),
            ),
        ] {
            let mut pending = imported(Some(orientation));
            for (k, v) in shown.as_object().unwrap() {
                pending[k] = v.clone();
            }
            let mut placed = pending.clone();
            assert!(place(&mut placed, 300.0, 200.0, None));
            assert!(!placed.to_string().contains(PENDING));
            let decoded = PhotoGeometry::from_adjustments(&pending).with_decoded_size(300, 200);
            let rendered = render(&placed, &decoded);
            assert!(
                rendered
                    .iter()
                    .all(|(bytes, _)| bytes.iter().any(|&v| v > 0))
            );
            assert_eq!(
                render(&pending, &decoded),
                rendered,
                "{orientation} {shown}"
            );
            // A reduced copy, which knows the photo's size only roughly,
            // leaves them out as before.
            let reduced = PhotoGeometry::from_adjustments(&pending).with_photo_size(300.0, 200.0);
            let left_out = render(&pending, &reduced);
            assert_ne!(left_out, rendered);
            assert_eq!(left_out, render(&imported(None), &reduced));
        }
    }

    #[test]
    fn masks_imported_without_their_photos_orientation_render_as_before() {
        // Imported before imports recorded it: left out as they always
        // were, whatever the render knows, until the editor places them.
        let mut recipe = imported(None);
        assert!(needs_orientation(&recipe));
        let decoded = PhotoGeometry::default().with_decoded_size(300, 200);
        let reduced = PhotoGeometry::default().with_photo_size(300.0, 200.0);
        assert_eq!(render(&recipe, &decoded), render(&recipe, &reduced));
        // Spots' orientation (recorded for every render of an older import)
        // leaves masks alone.
        crate::lightroom_retouch::set_orientation(&mut recipe, 6);
        assert!(needs_orientation(&recipe));
        assert_eq!(render(&recipe, &decoded), render(&recipe, &reduced));
        // The import records it.
        set_orientation(&mut recipe, 6);
        assert!(!needs_orientation(&recipe));
        assert_eq!(
            recipe["masks"][2]["subMasks"][0]["parameters"][PENDING][ORIENTATION],
            6
        );
        let mut placed = recipe.clone();
        assert!(place(&mut placed, 300.0, 200.0, None));
        assert_eq!(render(&recipe, &decoded), render(&placed, &decoded));
        assert_ne!(render(&recipe, &decoded), render(&recipe, &reduced));
    }

    #[test]
    fn a_pending_auto_mask_stroke_asks_for_the_photo() {
        let recipe = imported(Some(1));
        let defs = crate::image_processing::mask_definitions_from_json(&recipe);
        assert!(defs[2].requires_warped_image());
        assert!(!defs[0].requires_warped_image());
    }
}
