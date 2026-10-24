//! Lightroom's Heal/Clone spots and Red Eye corrections as imported, kept as
//! Lightroom stores them (`lrNormalized`: normalised in the unrotated raw)
//! until they are placed, and rendered from that form wherever the photo's
//! decoded size is known.
//!
//! An import knows the photo's EXIF orientation (a fact of the file) but
//! not its decoded size, which only a decode gives. The importer records
//! the orientation in the item (`orientation` inside `lrNormalized`); the
//! base retouch (`spot_removal::apply_spots`, `red_eye::apply_red_eyes`)
//! then places such an item on the base image it is given, so a photo
//! exported (app, batch, CLI) before it is ever opened renders them as the
//! editor does. The editor turns them into ordinary spots and corrections
//! when the photo is open (`placed_spot`, `placed_red_eye`) so they can be
//! edited; both start from the same base pixels.

use serde_json::{Value, json};

/// The key of a Lightroom item not yet placed.
pub const PENDING: &str = "lrNormalized";
/// The EXIF orientation the item's photo is decoded with, inside `PENDING`.
pub const ORIENTATION: &str = "orientation";

/// EXIF orientation applied to a point in normalised coordinates of the
/// unrotated raw (where Lightroom keeps its geometry): the point in the
/// photo as decoded.
pub fn orient_point(orientation: u16, x: f64, y: f64) -> (f64, f64) {
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
pub fn orient_direction(orientation: u16, dx: f64, dy: f64) -> (f64, f64) {
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

/// Width and height of the unrotated raw of a photo decoded at
/// `width` × `height` with `orientation`.
fn unrotated(orientation: u16, width: f64, height: f64) -> (f64, f64) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

/// The items under `key` ("spots", "redEyes") still in Lightroom's form.
fn pending_items<'a>(adjustments: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    adjustments
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get(PENDING).is_some_and(Value::is_object))
}

/// The orientation recorded in a pending item.
fn orientation_of(item: &Value) -> Option<u16> {
    item[PENDING][ORIENTATION]
        .as_u64()
        .and_then(|o| u16::try_from(o).ok())
        .filter(|o| (1..=8).contains(o))
}

/// Whether a pending item lacks its photo's orientation (imported before it
/// was recorded), so it cannot render until it is given one.
pub fn needs_orientation(adjustments: &Value) -> bool {
    ["spots", "redEyes"]
        .iter()
        .any(|key| pending_items(adjustments, key).any(|item| orientation_of(item).is_none()))
}

/// Records `orientation` (the photo's, `image_loader::source_orientation`)
/// in the pending items that lack one.
pub fn set_orientation(adjustments: &mut Value, orientation: u16) {
    for key in ["spots", "redEyes"] {
        let Some(items) = adjustments.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        for item in items {
            if item.get(PENDING).is_some_and(Value::is_object) && orientation_of(item).is_none() {
                item[PENDING][ORIENTATION] = json!(orientation);
            }
        }
    }
}

/// Whether the recipe holds a visible pending item under `key` that can
/// render (its orientation recorded).
pub fn has_renderable(adjustments: &Value, key: &str) -> bool {
    pending_items(adjustments, key)
        .any(|item| item["visible"].as_bool().unwrap_or(true) && orientation_of(item).is_some())
}

/// A spot in base-image pixels, as it is applied: centre, radius, source
/// centre and, for a stroke, its path.
pub(crate) struct BaseSpot {
    pub centre: (f64, f64),
    pub radius: f64,
    pub source: (f64, f64),
    pub path: Vec<(f64, f64)>,
}

/// A pending spot in pixels of a base image of `width` × `height`
/// (Lightroom's radius is a share of the unrotated width, its points
/// normalised in the unrotated raw). `orientation` applies when the item
/// records none. Rounded to hundredths of a pixel, as placed spots are
/// stored.
pub(crate) fn base_spot(
    item: &Value,
    width: f64,
    height: f64,
    orientation: Option<u16>,
) -> Option<BaseSpot> {
    let orientation = orientation_of(item).or(orientation)?;
    let g = &item[PENDING];
    let n = |k: &str| g[k].as_f64().filter(|v| v.is_finite());
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let pixel = |x: f64, y: f64| {
        let (u, v) = orient_point(orientation, x, y);
        (round(u * width), round(v * height))
    };
    let path = g["points"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| Some(pixel(p[0].as_f64()?, p[1].as_f64()?)))
        .collect();
    Some(BaseSpot {
        centre: pixel(n("x")?, n("y")?),
        radius: round(n("radius")? * unrotated(orientation, width, height).0),
        source: pixel(n("sourceX")?, n("sourceY")?),
        path,
    })
}

/// A pending spot as an ordinary spot of the photo `adjustments` describe
/// (mask-frame pixels, `spot_removal`), whose base image is `width` ×
/// `height`: its base pixels carried through the photo's geometry, the
/// radius through its local scale. Without an `id` (the caller gives one).
/// None when the geometry cannot show it.
pub fn placed_spot(
    item: &Value,
    adjustments: &Value,
    width: u32,
    height: u32,
    orientation: Option<u16>,
) -> Option<Value> {
    let spot = base_spot(item, f64::from(width), f64::from(height), orientation)?;
    let frame = crate::spot_removal::mask_frame(adjustments, width, height);
    let to_mask = |p: (f64, f64)| crate::red_eye::to_mask(adjustments, frame, p);
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let centre = to_mask(spot.centre)?;
    let radius = mask_radius(adjustments, frame, centre, spot.centre, spot.radius)?;
    let source = to_mask(spot.source)?;
    let mut placed = item.as_object()?.clone();
    placed.remove(PENDING);
    placed.insert("x".into(), json!(round(centre.0)));
    placed.insert("y".into(), json!(round(centre.1)));
    placed.insert("radius".into(), json!(round(radius)));
    placed.insert("sourceX".into(), json!(round(source.0)));
    placed.insert("sourceY".into(), json!(round(source.1)));
    placed.entry("visible").or_insert(json!(true));
    if spot.path.len() > 1 {
        let points: Option<Vec<Value>> = spot
            .path
            .iter()
            .map(|&p| to_mask(p).map(|(x, y)| json!({"x": round(x), "y": round(y)})))
            .collect();
        placed.insert("points".into(), Value::Array(points?));
    }
    Some(Value::Object(placed))
}

/// The mask-frame radius a spot centred at mask-frame `centre` (base pixel
/// `base_centre`) needs to render with `radius` base pixels:
/// `spot_removal::apply_spots` measures it from the centre to the point a
/// radius along the mask frame's x, carried to the base, so it is solved
/// for that (a stretching or distorting geometry scales the two frames
/// unequally along different directions). None when it does not converge.
fn mask_radius(
    adjustments: &Value,
    frame: (f64, f64),
    centre: (f64, f64),
    base_centre: (f64, f64),
    radius: f64,
) -> Option<f64> {
    let rendered = |r: f64| {
        let edge = crate::spot_removal::to_base(adjustments, frame, centre.0 + r, centre.1);
        (edge.0 - base_centre.0).hypot(edge.1 - base_centre.1)
    };
    let mut r = radius;
    for _ in 0..20 {
        let got = rendered(r);
        if !(got.is_finite() && got > 0.0) {
            return None;
        }
        if (got - radius).abs() < 1e-3 {
            return Some(r);
        }
        r *= radius / got;
    }
    None
}

/// A pending red eye correction (Lightroom's `crs:RedEyeInfo`, see
/// `RedEyeInfo` in the app's preset converter) as an ordinary one
/// (`red_eye::RedEye`: the photo as decoded, long-edge units) for a base
/// image of `width` × `height` (any copy of it: only its proportions
/// count). Lightroom's ellipse is a centre with the semi-extents along the
/// unrotated raw's x and y as shares of its width and height, and their
/// correlation `alpha` (its tilt); its highlight sits at 0.5 for the
/// centre and 0 or 1 a semi-extent away. `orientation` applies when the
/// item records none. Without an `id` (the caller gives one).
pub fn placed_red_eye(
    item: &Value,
    width: f64,
    height: f64,
    orientation: Option<u16>,
) -> Option<Value> {
    let orientation = orientation_of(item).or(orientation)?;
    let g = &item[PENDING];
    let n = |k: &str| g[k].as_f64().filter(|v| v.is_finite());
    let unit = width.max(height);
    let (raw_w, raw_h) = unrotated(orientation, width, height);
    let (u, v) = orient_point(orientation, n("x")?, n("y")?);
    let (sx, sy) = (n("width")?.abs() * raw_w, n("height")?.abs() * raw_h);
    let alpha = n("alpha").unwrap_or(0.0).clamp(-0.99, 0.99);
    // Its matrix in the raw (semi-extents² on the diagonal), turned as the
    // decode turns the raw: P·Σ·Pᵀ with P the orientation's signed swap.
    let raw = [[sx * sx, alpha * sx * sy], [alpha * sx * sy, sy * sy]];
    let columns = [
        orient_direction(orientation, 1.0, 0.0),
        orient_direction(orientation, 0.0, 1.0),
    ];
    let p = [[columns[0].0, columns[1].0], [columns[0].1, columns[1].1]];
    let m: [[f64; 2]; 2] = std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            (0..2)
                .map(|i| (0..2).map(|j| p[r][i] * raw[i][j] * p[c][j]).sum::<f64>())
                .sum()
        })
    });
    let (radius_x, radius_y, angle) = crate::red_eye::ellipse_of(m);
    let (radius_x, radius_y) = (radius_x / unit, radius_y / unit);
    if !(radius_x > 0.0 && radius_y > 0.0 && radius_x.is_finite() && radius_y.is_finite()) {
        return None;
    }
    let mut placed = item.as_object()?.clone();
    placed.remove(PENDING);
    placed.insert("version".into(), json!(crate::red_eye::VERSION));
    placed.insert("x".into(), json!(u * width / unit));
    placed.insert("y".into(), json!(v * height / unit));
    placed.insert("radiusX".into(), json!(radius_x));
    placed.insert("radiusY".into(), json!(radius_y));
    placed.insert("angle".into(), json!(angle));
    if placed.get("catchlight").and_then(Value::as_bool) == Some(true) {
        // Lightroom's highlight offset in the raw, as the catchlight's: in
        // units of the corrected pupil's mean radius along the decoded x, y.
        let offset = |k: &str| (n(k).unwrap_or(0.5) - 0.5) * 2.0;
        let (dx, dy) = orient_direction(
            orientation,
            offset("highlightX") * sx,
            offset("highlightY") * sy,
        );
        let pupil_size = placed
            .get("pupilSize")
            .and_then(Value::as_f64)
            .unwrap_or(50.0)
            .clamp(0.0, 100.0);
        let mean = (radius_x * radius_y).sqrt() * (0.6 + 0.8 * pupil_size / 100.0) * unit;
        placed.insert("catchlightX".into(), json!(dx / mean));
        placed.insert("catchlightY".into(), json!(dy / mean));
    }
    Some(Value::Object(placed))
}

/// Places the recipe's pending spots and red eye corrections for its photo
/// (base image `width` × `height`, `orientation` for items that record
/// none), each where it stands in its list; items the geometry cannot show
/// stay pending. Returns whether any was placed. Placed items have no `id`.
pub fn place(adjustments: &mut Value, width: u32, height: u32, orientation: Option<u16>) -> bool {
    let geometry = adjustments.clone();
    let mut placed = false;
    for key in ["spots", "redEyes"] {
        let Some(items) = adjustments.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        for item in items.iter_mut() {
            if !item.get(PENDING).is_some_and(Value::is_object) {
                continue;
            }
            let resolved = if key == "spots" {
                placed_spot(item, &geometry, width, height, orientation)
            } else {
                placed_red_eye(item, f64::from(width), f64::from(height), orientation)
            };
            if let Some(resolved) = resolved {
                *item = resolved;
                placed = true;
            }
        }
    }
    placed
}

/// `adjustments` without its pending spots and red eye corrections (for
/// reduced copies of the base, `image_loader::composite_patches_on_scaled_image`),
/// borrowed when it has none.
pub fn without_pending(adjustments: &Value) -> std::borrow::Cow<'_, Value> {
    if ["spots", "redEyes"]
        .iter()
        .all(|key| pending_items(adjustments, key).next().is_none())
    {
        return std::borrow::Cow::Borrowed(adjustments);
    }
    let mut kept = adjustments.clone();
    for key in ["spots", "redEyes"] {
        if let Some(items) = kept.get_mut(key).and_then(Value::as_array_mut) {
            items.retain(|item| !item.get(PENDING).is_some_and(Value::is_object));
        }
    }
    std::borrow::Cow::Owned(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_spot(orientation: Option<u16>) -> Value {
        let mut g = json!({"x": 0.25, "y": 0.5, "radius": 0.01, "sourceX": 0.3,
            "sourceY": 0.5, "points": []});
        if let Some(o) = orientation {
            g[ORIENTATION] = json!(o);
        }
        json!({"mode": "heal", "opacity": 100.0, "feather": 0.0, PENDING: g})
    }

    #[test]
    fn orientation_is_recorded_once_and_only_where_missing() {
        let mut recipe = json!({"spots": [pending_spot(None), pending_spot(Some(8)),
            {"x": 1.0, "y": 1.0, "radius": 4.0, "sourceX": 9.0, "sourceY": 9.0}]});
        assert!(needs_orientation(&recipe));
        assert!(!has_renderable(
            &json!({"spots": [pending_spot(None)]}),
            "spots"
        ));
        set_orientation(&mut recipe, 6);
        assert!(!needs_orientation(&recipe));
        assert_eq!(recipe["spots"][0][PENDING][ORIENTATION], 6);
        assert_eq!(recipe["spots"][1][PENDING][ORIENTATION], 8);
        assert!(recipe["spots"][2].get(PENDING).is_none());
        assert!(has_renderable(&recipe, "spots"));
    }

    #[test]
    fn a_spot_lands_where_the_decode_puts_the_raw_point() {
        // Orientation 6 (turned a quarter clockwise): raw (0.25, 0.5) of a
        // 6000 × 4000 raw is decoded at (0.5, 0.25) of a 4000 × 6000 photo;
        // the radius is a share of the raw's width (6000).
        let spot = base_spot(&pending_spot(Some(6)), 4000.0, 6000.0, None).unwrap();
        assert_eq!(spot.centre, (2000.0, 1500.0));
        assert_eq!(spot.radius, 60.0);
        assert_eq!(spot.source, (2000.0, 1800.0));
        assert!(base_spot(&pending_spot(None), 4000.0, 6000.0, None).is_none());
    }

    #[test]
    fn placed_spots_follow_the_photos_turns_and_straightening() {
        let item = pending_spot(Some(1));
        let plain = placed_spot(&item, &json!({}), 6000, 4000, None).unwrap();
        assert_eq!(
            (plain["x"].as_f64(), plain["y"].as_f64()),
            (Some(1500.0), Some(2000.0))
        );
        assert_eq!(plain["radius"], 60.0);
        assert!(plain.get(PENDING).is_none());
        // Turned a quarter by the user: the mask frame is 4000 × 6000, and
        // the spot's mask-frame point goes back to the same base pixel.
        for geometry in [
            json!({"orientationSteps": 1}),
            json!({"rotation": 7.5}),
            json!({"orientationSteps": 3, "flipHorizontal": true, "rotation": -3.0}),
            // Stretching and distorting: the two frames' scales differ by
            // direction, the rendered radius must still be Lightroom's.
            json!({"orientationSteps": 1, "transformAspect": 100.0}),
            json!({"transformDistortion": 35.0, "transformAspect": -40.0, "rotation": 2.0}),
        ] {
            let placed = placed_spot(&item, &geometry, 6000, 4000, None).unwrap();
            let frame = crate::spot_removal::mask_frame(&geometry, 6000, 4000);
            let n = |k: &str| placed[k].as_f64().unwrap();
            let base = crate::spot_removal::to_base(&geometry, frame, n("x"), n("y"));
            assert!(
                (base.0 - 1500.0).abs() < 0.02 && (base.1 - 2000.0).abs() < 0.02,
                "{geometry} {base:?}"
            );
            let source = crate::spot_removal::to_base(&geometry, frame, n("sourceX"), n("sourceY"));
            assert!((source.0 - 1800.0).abs() < 0.02 && (source.1 - 2000.0).abs() < 0.02);
            // The radius as `apply_spots` renders it.
            let edge = crate::spot_removal::to_base(&geometry, frame, n("x") + n("radius"), n("y"));
            let rendered = (edge.0 - base.0).hypot(edge.1 - base.1);
            assert!((rendered - 60.0).abs() < 0.05, "{geometry}: {rendered}");
        }
    }

    #[test]
    fn a_red_eye_becomes_an_ellipse_of_the_decoded_photo() {
        // A 6000 × 4000 raw, decoded with orientation 8 (turned a quarter
        // counter-clockwise) as 4000 × 6000: raw (0.5, 0.25) lies at
        // decoded (0.25, 0.5). Semi-extents 60 px along the raw's x and
        // 20 px along its y become 60 px along the decoded y and 20 px
        // along its x.
        let item = json!({"kind": "red", "pupilSize": 50.0, "darken": 50.0, PENDING: {
            "x": 0.5, "y": 0.25, "width": 0.01, "height": 0.005, "alpha": 0.0,
            ORIENTATION: 8}});
        let eye = placed_red_eye(&item, 4000.0, 6000.0, None).unwrap();
        let n = |k: &str| eye[k].as_f64().unwrap();
        assert!((n("x") * 6000.0 - 1000.0).abs() < 1e-9);
        assert!((n("y") * 6000.0 - 3000.0).abs() < 1e-9);
        let (a, b) = (n("radiusX") * 6000.0, n("radiusY") * 6000.0);
        // The long axis runs along the decoded y.
        assert!((a - 60.0).abs() < 1e-6 && (b - 20.0).abs() < 1e-6, "{eye}");
        assert!((n("angle").abs() - 90.0).abs() < 1e-6, "{eye}");
        assert_eq!(eye["version"], crate::red_eye::VERSION);
        // A reduced copy places it on the same spot of the photo.
        let half = placed_red_eye(&item, 2000.0, 3000.0, None).unwrap();
        assert!((half["x"].as_f64().unwrap() - n("x")).abs() < 1e-12);
        assert!((half["radiusX"].as_f64().unwrap() - n("radiusX")).abs() < 1e-12);
    }

    #[test]
    fn a_tilted_red_eye_keeps_its_extents_and_its_highlight() {
        // Correlation 0.6 between equal extents: the ellipse's axes lie on
        // the diagonals, and its bounding box keeps the semi-extents.
        let item = json!({"kind": "pet", "pupilSize": 50.0, "catchlight": true, PENDING: {
            "x": 0.5, "y": 0.5, "width": 0.01, "height": 0.015, "alpha": 0.6,
            "highlightX": 1.0, "highlightY": 0.5, ORIENTATION: 1}});
        let eye = placed_red_eye(&item, 6000.0, 4000.0, None).unwrap();
        let n = |k: &str| eye[k].as_f64().unwrap();
        let (a, b, t) = (
            n("radiusX") * 6000.0,
            n("radiusY") * 6000.0,
            n("angle").to_radians(),
        );
        let extent_x = ((a * t.cos()).powi(2) + (b * t.sin()).powi(2)).sqrt();
        let extent_y = ((a * t.sin()).powi(2) + (b * t.cos()).powi(2)).sqrt();
        assert!(
            (extent_x - 60.0).abs() < 1e-6 && (extent_y - 60.0).abs() < 1e-6,
            "{eye}"
        );
        assert!(t.sin() * t.cos() > 0.0, "tilted down to the right: {eye}");
        // Highlight a semi-extent to the right: 60 px.
        let mean = (n("radiusX") * n("radiusY")).sqrt() * 6000.0;
        assert!((n("catchlightX") * mean - 60.0).abs() < 1e-6, "{eye}");
        assert!(n("catchlightY").abs() < 1e-9);
    }

    #[test]
    fn place_turns_pending_items_into_ordinary_ones_in_their_order() {
        let mut recipe = json!({
            "spots": [{"x": 1.0, "y": 1.0, "radius": 4.0, "sourceX": 9.0, "sourceY": 9.0},
                pending_spot(None)],
            "redEyes": [{"kind": "red", PENDING: {"x": 0.5, "y": 0.5, "width": 0.01,
                "height": 0.01}}],
        });
        assert!(place(&mut recipe, 6000, 4000, Some(1)));
        assert_eq!(recipe["spots"][0]["x"], 1.0);
        assert_eq!(recipe["spots"][1]["x"], 1500.0);
        assert!(recipe["redEyes"][0].get(PENDING).is_none());
        assert_eq!(crate::red_eye::red_eyes(&recipe).len(), 1);
        assert!(!place(&mut recipe, 6000, 4000, Some(1)));
    }

    /// A grey photo of `width` × `height` with texture, a dark speck at
    /// (`speck`) and a red pupil at (`eye`) (pixels).
    fn photo(width: u32, height: u32, speck: (f64, f64), eye: (f64, f64)) -> image::DynamicImage {
        image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |x, y| {
            let (fx, fy) = (f64::from(x), f64::from(y));
            let grain = (((x * 7 + y * 13) % 11) as f32) * 0.004;
            if (fx - speck.0).hypot(fy - speck.1) < 3.0 {
                image::Rgb([0.02, 0.02, 0.02])
            } else if (fx - eye.0).hypot(fy - eye.1) < 4.0 {
                image::Rgb([0.6, 0.04, 0.03])
            } else {
                image::Rgb([0.3 + grain, 0.28 + grain, 0.25 + grain])
            }
        }))
    }

    fn rendered(base: &image::DynamicImage, recipe: &Value) -> Vec<f32> {
        crate::image_loader::composite_patches_on_image(base, recipe)
            .unwrap()
            .to_rgb32f()
            .into_raw()
    }

    #[test]
    fn pending_items_render_as_their_placed_form_does() {
        // A 120 × 160 portrait decoded from a 160 × 120 raw with
        // orientation 6: raw (0.25, 0.5) is decoded at (60, 40), raw
        // (0.75, 0.25) at (90, 120).
        let base = photo(120, 160, (60.0, 40.0), (90.0, 120.0));
        let pending = json!({
            "spots": [{"mode": "heal", "opacity": 100.0, "feather": 0.0, PENDING: {
                "x": 0.25, "y": 0.5, "radius": 0.04, "sourceX": 0.25, "sourceY": 0.25,
                "points": [], ORIENTATION: 6}}],
            "redEyes": [{"kind": "red", "pupilSize": 50.0, "darken": 50.0, PENDING: {
                "x": 0.75, "y": 0.25, "width": 0.03, "height": 0.04, "alpha": 0.0,
                ORIENTATION: 6}}],
        });
        let plain = rendered(&base, &json!({}));
        let imported = rendered(&base, &pending);
        let at = |pixels: &[f32], x: usize, y: usize| pixels[(y * 120 + x) * 3];
        // The speck is healed and the pupil no longer red.
        assert!(at(&plain, 60, 40) < 0.05 && at(&imported, 60, 40) > 0.2);
        assert!(at(&plain, 90, 120) > 0.5 && at(&imported, 90, 120) < 0.2);
        assert!(crate::image_loader::has_base_retouch(&pending));
        // Placed for editing, they render alike, also turned and
        // straightened by the user afterwards (their place in the photo
        // does not change).
        for geometry in [
            json!({}),
            json!({"orientationSteps": 1}),
            json!({"rotation": 4.0}),
            json!({"orientationSteps": 1, "transformAspect": 100.0}),
        ] {
            let mut recipe = pending.clone();
            for (k, v) in geometry.as_object().unwrap() {
                recipe[k] = v.clone();
            }
            let mut placed = recipe.clone();
            assert!(place(&mut placed, 120, 160, None));
            assert!(placed["spots"][0].get(PENDING).is_none(), "{placed}");
            let a = rendered(&base, &recipe);
            // On the same pixels of the photo, and opening the photo
            // changes none.
            assert!(at(&a, 60, 40) > 0.2 && at(&a, 90, 120) < 0.2, "{geometry}");
            assert_eq!(a, rendered(&base, &placed), "{geometry}");
        }
        // Without its orientation an item cannot be placed and renders
        // nothing, as before imports recorded it.
        let mut unknown = pending.clone();
        unknown["spots"][0][PENDING]
            .as_object_mut()
            .unwrap()
            .remove(ORIENTATION);
        unknown["redEyes"][0][PENDING]
            .as_object_mut()
            .unwrap()
            .remove(ORIENTATION);
        assert!(!crate::image_loader::has_base_retouch(&unknown));
    }

    #[test]
    fn a_clone_copies_the_same_pixels_before_and_after_placement() {
        // Base x 1000.1 and source 1500.6 on a 6000-pixel-wide photo: an
        // offset of 500.5 in f64, which the placed spot's f32 geometry may
        // round to the other whole pixel. The pending spot renders as placed.
        let base =
            image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(6000, 40, |x, y| {
                let v = ((x * 31 + y * 17) % 97) as f32 / 97.0;
                image::Rgb([v, 1.0 - v, 0.5])
            }));
        let pending = json!({"spots": [{"mode": "clone", PENDING: {"x": 0.166683, "y": 0.5,
            "radius": 0.002, "sourceX": 0.2501, "sourceY": 0.5, ORIENTATION: 1}}]});
        for geometry in [
            json!({}),
            json!({"rotation": 0.3}),
            json!({"transformAspect": 30.0}),
        ] {
            let mut recipe = pending.clone();
            for (k, v) in geometry.as_object().unwrap() {
                recipe[k] = v.clone();
            }
            let mut placed = recipe.clone();
            assert!(place(&mut placed, 6000, 40, None), "{geometry}");
            assert_eq!(
                rendered(&base, &recipe),
                rendered(&base, &placed),
                "{geometry}"
            );
        }
    }

    #[test]
    fn a_hidden_pending_spot_stays_hidden_rendered_and_placed() {
        let base = photo(120, 160, (60.0, 40.0), (90.0, 120.0));
        let spot = |x: f64, visible: bool| {
            json!({"mode": "heal", "visible": visible, PENDING: {"x": x, "y": 0.5,
                "radius": 0.04, "sourceX": 0.25, "sourceY": 0.25, ORIENTATION: 6}})
        };
        let shown = json!({"spots": [spot(0.7, true)]});
        let both = json!({"spots": [spot(0.25, false), spot(0.7, true)]});
        assert_eq!(rendered(&base, &both), rendered(&base, &shown));
        let mut placed = both.clone();
        assert!(place(&mut placed, 120, 160, None));
        assert_eq!(placed["spots"][0]["visible"], false);
        assert_eq!(rendered(&base, &placed), rendered(&base, &shown));
    }

    #[test]
    fn reduced_copies_leave_pending_items_out() {
        // Only full-resolution bases place them exactly as the editor will;
        // a reduced copy shows them once they are placed.
        let pending = json!({
            "spots": [{"mode": "heal", PENDING: {"x": 0.25, "y": 0.5, "radius": 0.08,
                "sourceX": 0.25, "sourceY": 0.25, ORIENTATION: 6}}],
            "redEyes": [{"kind": "red", PENDING: {"x": 0.75, "y": 0.25, "width": 0.03,
                "height": 0.04, ORIENTATION: 6}}],
        });
        let half = photo(60, 80, (30.0, 20.0), (45.0, 60.0));
        let reduced = crate::image_loader::composite_patches_on_scaled_image(&half, &pending, 0.5)
            .unwrap()
            .to_rgb32f();
        assert_eq!(reduced.into_raw(), half.to_rgb32f().into_raw());
        assert!(crate::image_loader::retouch_needs_full_resolution(&pending));
    }
}
