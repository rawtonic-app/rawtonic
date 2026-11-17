//! Lightroom's and Camera Raw's Guided Upright (`crs:PerspectiveUpright` 5)
//! as Rawtonic's guided perspective lines.
//!
//! Lightroom stores each guide as `crs:UprightFourSegments_N="x1,y1,x2,y2"`
//! (`UprightFourSegmentsCount` of them), normalised to Lightroom's whole
//! image in the raw's own, unoriented frame (a guide drawn on a photo shown
//! turned is stored unturned): the sensor image before the camera's aspect
//! crop, as its crop (a Panasonic 1:1 photo is a crop of the 4:3 image; a
//! Sony ARW set to another aspect ratio is not settled and left out). The
//! guides lie *before* the lens profile (`LensProfileEnable`,
//! an Adobe or the camera's "Camera Settings" profile): a guide drawn on the
//! corrected photo is stored at the uncorrected position of its ends.
//! Without a profile the stored ends are where they were drawn, on the photo
//! with only the corrections Lightroom applies to some cameras regardless
//! (Fujifilm, Panasonic), which Rawtonic's import keeps as the camera's
//! correction (`library_import::lightroom_lens`). Camera Raw 18.7, 24
//! photos: stored and drawn ends agree without a profile and differ by the
//! profile's distortion with one, up to 9 % of the radius (notes:
//! benchmark-results/guq1-2026-11-17). Rawtonic's guides lie on the
//! corrected photo, so with a profile each end goes through the photo's own
//! lens correction (`WarpPoints::to_output`). The guide's kind follows its
//! direction, as in Lightroom (which stores none).

use std::path::Path;

use serde_json::Value;

use crate::guided_perspective::{GuideLine, GuideOrientation, GuidePoint};
use crate::image_processing::{GeometryParams, WarpPoints, get_geometry_params_from_json};

/// Lightroom's Guided Upright mode (`crs:PerspectiveUpright`).
pub const GUIDED: f64 = 5.0;

/// The guides of a Guided Upright edit, as stored (x1, y1, x2, y2,
/// normalised to the sensor image), from Lightroom's XMP; none for other
/// Upright modes, no Upright, or no guides. Reads both the attribute and the
/// element form of the keys.
pub fn segments(xmp: &str) -> Vec<[f64; 4]> {
    let value = |key: &str| -> Option<String> {
        let attribute = regex::Regex::new(&format!(r#"crs:{key}\s*=\s*"([^"]*)""#)).ok()?;
        let element = regex::Regex::new(&format!(r"<crs:{key}>([^<]*)</crs:{key}>")).ok()?;
        attribute
            .captures(xmp)
            .or_else(|| element.captures(xmp))
            .map(|c| c[1].trim().to_string())
    };
    let number = |key: &str| value(key).and_then(|v| v.trim_start_matches('+').parse::<f64>().ok());
    if number("PerspectiveUpright") != Some(GUIDED) {
        return Vec::new();
    }
    let count = number("UprightFourSegmentsCount").unwrap_or(0.0);
    if !(0.0..=16.0).contains(&count) {
        return Vec::new();
    }
    (0..count as usize)
        .filter_map(|i| {
            let text = value(&format!("UprightFourSegments_{i}"))?;
            let v: Vec<f64> = text
                .split(',')
                .filter_map(|n| n.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .collect();
            (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
        })
        .collect()
}

/// Rawtonic guide lines for Lightroom's `segments` normalised to `whole`, on
/// a photo whose developed frame is `frame` (both x, y, width, height in the
/// raw's unoriented buffer pixels) shown with EXIF `orientation`, through
/// `warp` (the photo's lens correction only, built for the oriented frame;
/// None: stored on the corrected photo). Rawtonic's guides are normalised to
/// the photo as decoded (oriented). Ends the correction cannot place drop
/// their guide.
pub fn guide_lines(
    segments: &[[f64; 4]],
    whole: [f64; 4],
    frame: [f64; 4],
    orientation: u16,
    warp: Option<&WarpPoints>,
) -> Vec<GuideLine> {
    let [sx, sy, sw, sh] = whole;
    let [fx, fy, fw, fh] = frame;
    if !(sw > 0.0 && sh > 0.0 && fw > 0.0 && fh > 0.0) {
        return Vec::new();
    }
    // The oriented frame's size.
    let (fw, fh) = if (5..=8).contains(&orientation) {
        (fh, fw)
    } else {
        (fw, fh)
    };
    let place = |x: f64, y: f64| -> Option<GuidePoint> {
        let (ux, uy) = ((sx + x * sw - fx) / frame[2], (sy + y * sh - fy) / frame[3]);
        let (nx, ny) = crate::lightroom_retouch::orient_point(orientation, ux, uy);
        let (px, py) = (nx * fw, ny * fh);
        let (ox, oy) = match warp {
            Some(warp) => warp
                .to_output(px as f32, py as f32)
                .map(|(x, y)| (f64::from(x), f64::from(y)))?,
            None => (px, py),
        };
        let point = GuidePoint {
            x: ox / fw,
            y: oy / fh,
        };
        (point.x.is_finite() && point.y.is_finite()).then_some(point)
    };
    segments
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let p1 = place(s[0], s[1])?;
            let p2 = place(s[2], s[3])?;
            let vertical = ((p2.y - p1.y) * fh).abs() >= ((p2.x - p1.x) * fw).abs();
            Some(GuideLine {
                id: format!("lightroom-{i}"),
                orientation: if vertical {
                    GuideOrientation::Vertical
                } else {
                    GuideOrientation::Horizontal
                },
                p1,
                p2,
            })
        })
        .collect()
}

/// The photo's lens correction alone (no transform, no guides) as the
/// recipe `adjustments` renders it: the stored lens choice, or for a recipe
/// without one the default a never-edited photo gets.
fn lens_warp(adjustments: &Value, path: &Path, width: u32, height: u32) -> WarpPoints {
    let mut probe = adjustments.clone();
    crate::embedded_lens::apply_default(&mut probe, path);
    let lens_only = GeometryParams {
        vertical: 0.0,
        horizontal: 0.0,
        rotate: 0.0,
        aspect: 0.0,
        scale: 100.0,
        x_offset: 0.0,
        y_offset: 0.0,
        guided_lines: Vec::new(),
        guided_perspective_enabled: false,
        guided_fill: false,
        ..get_geometry_params_from_json(&probe)
    };
    WarpPoints::new(&lens_only, width, height)
}

/// Lightroom's Guided Upright guides in `xmp` as Rawtonic guide lines for the
/// RAW at `path` whose translated recipe is `adjustments` (`profile`:
/// Lightroom's lens profile was on); none when the edit has no Guided
/// Upright, fewer than two guides or the RAW cannot be read.
pub fn lines_for_file(
    xmp: &str,
    path: &Path,
    adjustments: &Value,
    profile: bool,
) -> Vec<GuideLine> {
    let segments = segments(xmp);
    if segments.len() < 2 {
        return Vec::new();
    }
    let Ok(source) = rawler::rawsource::RawSource::new(path) else {
        return Vec::new();
    };
    let Some((frame, sensor)) = rawtonic_rawdecode::source_frame::frame_and_sensor(
        &source,
        rawtonic_rawdecode::source_frame::VERSION,
    ) else {
        return Vec::new();
    };
    // A Sony ARW set to another aspect ratio in the camera (16:9 on a 3:2
    // sensor): which image Lightroom's numbers refer to is not settled (on
    // B488 neither the sensor nor the camera's frame puts both guides on
    // their edges; notes: benchmark-results/guq1-2026-11-17), so its guides
    // are left out rather than placed on other edges.
    let sony =
        crate::source_frame::extension_of(&path.to_string_lossy()).eq_ignore_ascii_case("arw");
    if sony
        && rawtonic_rawdecode::source_frame::frame(&source, 1)
            .is_some_and(|aspect| aspect.rect != sensor)
    {
        return Vec::new();
    }
    let [_, _, w, h] = frame.rect;
    let (w, h) = if (5..=8).contains(&frame.orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let warp = profile.then(|| lens_warp(adjustments, path, w as u32, h as u32));
    let f = |r: [usize; 4]| r.map(|v| v as f64);
    guide_lines(
        &segments,
        f(sensor),
        f(frame.rect),
        frame.orientation,
        warp.as_ref(),
    )
}

/// Sets `guidedPerspective` from Lightroom's Guided Upright, with Rawtonic's
/// Constrain crop unless Lightroom's own crop was imported (`has_crop`: the
/// photographer framed the corrected photo there). False when there was
/// nothing to import.
pub fn apply(
    adjustments: &mut Value,
    xmp: &str,
    path: &Path,
    profile: bool,
    has_crop: bool,
) -> bool {
    let lines = lines_for_file(xmp, path, adjustments, profile);
    if lines.len() < 2 {
        return false;
    }
    let Some(map) = adjustments.as_object_mut() else {
        return false;
    };
    map.insert(
        rawtonic_recipe::geometry::GUIDED_PERSPECTIVE.name().into(),
        serde_json::json!({
            "enabled": true,
            "lines": lines,
            "upright": "guided",
            "autoCrop": !has_crop,
        }),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const XMP: &str = r#"<rdf:Description crs:PerspectiveUpright="5"
        crs:UprightVersion="151388160" crs:UprightTransformCount="6"
        crs:UprightFourSegmentsCount="2"
        crs:UprightFourSegments_0="0.200000000,0.100000000,0.250000000,0.900000000"
        crs:UprightFourSegments_1="0.800000000,0.100000000,0.750000000,0.900000000"/>"#;

    #[test]
    fn reads_the_guides_of_guided_upright_only() {
        assert_eq!(
            segments(XMP),
            vec![[0.2, 0.1, 0.25, 0.9], [0.8, 0.1, 0.75, 0.9]]
        );
        // Element form (Lightroom's catalog through `lightroom_settings::to_xmp`
        // or a sidecar written so).
        let element = "<crs:PerspectiveUpright>5</crs:PerspectiveUpright>\
            <crs:UprightFourSegmentsCount>1</crs:UprightFourSegmentsCount>\
            <crs:UprightFourSegments_0>0.1, 0.2, 0.3, 0.4</crs:UprightFourSegments_0>";
        assert_eq!(segments(element), vec![[0.1, 0.2, 0.3, 0.4]]);
        // Other Upright modes keep no guides (Full = 2).
        assert!(
            segments(&XMP.replace(r#"PerspectiveUpright="5""#, r#"PerspectiveUpright="2""#))
                .is_empty()
        );
        assert!(segments(r#"<rdf:Description crs:Exposure2012="+0.5"/>"#).is_empty());
        // A malformed guide is skipped, the others kept.
        let broken = XMP.replace("0.800000000,0.100000000,", "0.8,x,");
        assert_eq!(segments(&broken), vec![[0.2, 0.1, 0.25, 0.9]]);
    }

    /// Without Lightroom's lens profile a guide keeps its place: the sensor's
    /// normalised position becomes the developed frame's (an aspect crop
    /// moves and scales it), and its kind follows its direction.
    #[test]
    fn guides_land_on_the_same_pixels_of_the_developed_frame() {
        let sensor = [0.0, 0.0, 4000.0, 3000.0];
        // A 3:2 aspect crop of the 4:3 sensor.
        let frame = [0.0, 167.0, 4000.0, 2666.0];
        let lines = guide_lines(&segments(XMP), sensor, frame, 1, None);
        assert_eq!(lines.len(), 2);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-4;
        let l = &lines[0];
        assert_eq!(l.orientation, GuideOrientation::Vertical);
        assert!(
            near(l.p1.x, 0.2) && near(l.p1.y, (300.0 - 167.0) / 2666.0),
            "{l:?}"
        );
        assert!(
            near(l.p2.x, 0.25) && near(l.p2.y, (2700.0 - 167.0) / 2666.0),
            "{l:?}"
        );
        let flat = [[0.1, 0.5, 0.9, 0.52]];
        let lines = guide_lines(&flat, sensor, sensor, 1, None);
        assert_eq!(lines[0].orientation, GuideOrientation::Horizontal);
    }

    /// A photo shown turned (EXIF 6): Lightroom keeps the guide in the raw's
    /// unturned frame, Rawtonic in the photo as decoded, where a building's
    /// edge along the sensor's long side stands upright.
    #[test]
    fn guides_of_a_turned_photo_land_in_the_decoded_photo() {
        let sensor = [0.0, 0.0, 4000.0, 3000.0];
        let lines = guide_lines(&[[0.1, 0.5, 0.9, 0.52]], sensor, sensor, 6, None);
        let l = &lines[0];
        assert_eq!(l.orientation, GuideOrientation::Vertical, "{l:?}");
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(l.p1.x, 0.5) && near(l.p1.y, 0.1), "{l:?}");
        assert!(near(l.p2.x, 0.48) && near(l.p2.y, 0.9), "{l:?}");
        // Turned the other way (EXIF 8).
        let l = &guide_lines(&[[0.1, 0.5, 0.9, 0.52]], sensor, sensor, 8, None)[0];
        assert!(near(l.p1.x, 0.5) && near(l.p1.y, 0.9), "{l:?}");
    }

    /// With the profile an end stored at its uncorrected place lands
    /// where the correction puts that point: the warp samples it back there.
    #[test]
    fn guides_follow_the_lens_correction() {
        let (w, h) = (6000u32, 4000u32);
        let recipe = serde_json::json!({
            "lensDistortionParams": {"k1": -0.08, "k2": 0.01, "k3": 0.0, "model": 1,
                "tca_vr": 1.0, "tca_vb": 1.0, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0},
        });
        let params = get_geometry_params_from_json(&recipe);
        let warp = WarpPoints::new(&params, w, h);
        let sensor = [0.0, 0.0, f64::from(w), f64::from(h)];
        let lines = guide_lines(&[[0.1, 0.1, 0.12, 0.9]], sensor, sensor, 1, Some(&warp));
        assert_eq!(lines.len(), 1);
        for (p, (x, y)) in [(lines[0].p1, (0.1, 0.1)), (lines[0].p2, (0.12, 0.9))] {
            let (sx, sy) = warp
                .to_source((p.x * f64::from(w)) as f32, (p.y * f64::from(h)) as f32)
                .unwrap();
            assert!(
                (f64::from(sx) - x * f64::from(w)).abs() < 0.5
                    && (f64::from(sy) - y * f64::from(h)).abs() < 0.5,
                "{p:?} samples {sx} {sy}"
            );
        }
        // And the correction moved it (the test would pass without one).
        assert!((lines[0].p1.x - 0.1).abs() > 1e-3, "{:?}", lines[0].p1);
    }

    /// Measurement tool (notes: benchmark-results/guq1-2026-11-17): prints
    /// the guide lines for each `raw<TAB>xmp` line of the file `GUQ_PAIRS`
    /// as JSON (`{"<raw>": [lines]}`), with a new edit's lens correction.
    #[test]
    #[ignore]
    fn print_lightroom_guides() {
        let Ok(pairs) = std::env::var("GUQ_PAIRS") else {
            return;
        };
        if let Ok(dir) = std::env::var("RAWTONIC_LENSFUN_DB") {
            let db = crate::lens_correction::load_lensfun_dir(Path::new(&dir));
            crate::lens_correction::set_global_lens_db(std::sync::Arc::new(db));
        }
        let mut out = serde_json::Map::new();
        for line in std::fs::read_to_string(pairs).unwrap().lines() {
            let Some((raw, xmp)) = line.split_once('\t') else {
                continue;
            };
            let xmp = std::fs::read_to_string(xmp).unwrap();
            let profile = xmp.contains(r#"crs:LensProfileEnable="1""#);
            let lines = lines_for_file(&xmp, Path::new(raw), &serde_json::json!({}), profile);
            let source = rawler::rawsource::RawSource::new(Path::new(raw)).unwrap();
            let rects = rawtonic_rawdecode::source_frame::frame_and_sensor(
                &source,
                rawtonic_rawdecode::source_frame::VERSION,
            )
            .map(|(frame, sensor)| (frame.rect, sensor));
            out.insert(
                raw.to_string(),
                serde_json::json!({"lines": lines, "frame_sensor": rects}),
            );
        }
        println!("GUQ_LINES {}", Value::Object(out));
    }
}
