//! Lightroom's crop and straighten (`crs:CropLeft/Top/Right/Bottom`,
//! `crs:CropAngle`) as Rawtonic's `rotation` and pixel `crop`.
//!
//! Lightroom's four numbers are two corners of the crop: (CropLeft, CropTop)
//! is its top-left and (CropRight, CropBottom) its bottom-right corner,
//! normalised to the whole sensor image (before the camera's aspect crop:
//! Lightroom shows an Olympus 16:9 or 3:2 frame as a crop of the 4:3
//! sensor) in the raw's own, unoriented frame. The crop's edges are tilted
//! by CropAngle degrees (positive: the top edge runs down to the right), and
//! the photo is straightened by that angle so the crop comes out upright.
//! Measured on Lightroom Classic 15.6 exports (SIFT homography of each
//! export onto Rawtonic's uncropped frame, orientations 1, 3, 6, 8, angles
//! 0, 5, −8, 4, −3; notes: benchmark-results/lc2-lr-render-match-2026-11-01).
//!
//! Rawtonic straightens the oriented frame about its centre (keeping its
//! size) and then crops in pixels of that frame (`image_processing`
//! `fused_spatial_transform`), so the crop's corners are oriented, turned by
//! the straightening and bounded.

use serde_json::{Value, json};

use crate::lightroom_retouch::{orient_direction, orient_point};

/// Lightroom's crop settings of one photo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightroomCrop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// Degrees.
    pub angle: f64,
}

impl LightroomCrop {
    /// The whole image, not straightened: no crop.
    pub fn is_whole(&self) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
        near(self.left, 0.0)
            && near(self.top, 0.0)
            && near(self.right, 1.0)
            && near(self.bottom, 1.0)
            && near(self.angle, 0.0)
    }
}

/// The recipe values of a Lightroom crop.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// `crop` in pixels of the straightened, oriented frame.
    pub crop: Value,
    /// `rotation` in degrees (Rawtonic's straighten).
    pub rotation: f64,
    /// The crop's own aspect ratio (width / height), locked as in
    /// Lightroom.
    pub aspect_ratio: f64,
    /// Lightroom's crop reaches beyond the developed frame (it shows sensor
    /// area that Rawtonic's frame leaves out, such as outside an Olympus
    /// aspect ratio): the crop keeps its centre and aspect ratio and is
    /// shrunk until all of it shows the photo.
    pub clipped: bool,
}

/// Places `crop` for a photo whose developed frame is `frame` within the
/// whole sensor image `sensor` (both x, y, width, height in the raw's
/// unoriented buffer pixels), shown with EXIF `orientation`. None for the
/// whole image (no crop) and for degenerate crops.
pub fn place(
    crop: &LightroomCrop,
    sensor: [f64; 4],
    frame: [f64; 4],
    orientation: u16,
) -> Option<Placed> {
    if crop.is_whole() || !crop.angle.is_finite() {
        return None;
    }
    let [sx, sy, sw, sh] = sensor;
    let [fx, fy, fw, fh] = frame;
    if !(sw > 0.0 && sh > 0.0 && fw > 0.0 && fh > 0.0) {
        return None;
    }
    let theta = crop.angle.to_radians();
    let u = (theta.cos(), theta.sin());
    let v = (-theta.sin(), theta.cos());
    let tl = (sx + crop.left * sw, sy + crop.top * sh);
    let br = (sx + crop.right * sw, sy + crop.bottom * sh);
    let d = (br.0 - tl.0, br.1 - tl.1);
    let width = d.0 * u.0 + d.1 * u.1;
    let height = d.0 * v.0 + d.1 * v.1;
    if !(width > 1.0 && height > 1.0) {
        return None;
    }
    let corners = [
        tl,
        (tl.0 + width * u.0, tl.1 + width * u.1),
        br,
        (tl.0 + height * v.0, tl.1 + height * v.1),
    ];
    // Oriented frame.
    let (w, h) = if (5..=8).contains(&orientation) {
        (fh, fw)
    } else {
        (fw, fh)
    };
    let oriented = corners.map(|(x, y)| {
        let (ox, oy) = orient_point(orientation, (x - fx) / fw, (y - fy) / fh);
        (ox * w, oy * h)
    });
    // The top edge's angle in the oriented frame, within ±45°: the
    // straightening that makes the crop upright.
    let (dx, dy) = orient_direction(orientation, u.0, u.1);
    let mut tilt = dy.atan2(dx).to_degrees();
    tilt = (tilt + 45.0).rem_euclid(90.0) - 45.0;
    let rotation = -tilt;
    let (sin, cos) = rotation.to_radians().sin_cos();
    let (cx, cy) = (w / 2.0, h / 2.0);
    let turned = oriented.map(|(x, y)| {
        let (px, py) = (x - cx, y - cy);
        (cx + cos * px - sin * py, cy + sin * px + cos * py)
    });
    let x0 = turned.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x1 = turned.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y0 = turned.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let y1 = turned.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    // Whether a point of the straightened frame shows the photo (half a
    // pixel of slack for rounding): turned back, it lies in the frame.
    let on_photo = |(x, y): (f64, f64)| {
        let (px, py) = (x - cx, y - cy);
        let (ux, uy) = (cx + cos * px + sin * py, cy - sin * px + cos * py);
        (-0.5..=w + 0.5).contains(&ux) && (-0.5..=h + 0.5).contains(&uy)
    };
    let fits = |c: (f64, f64), half: (f64, f64)| {
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .iter()
            .all(|(sx, sy)| on_photo((c.0 + sx * half.0, c.1 + sy * half.1)))
    };
    let centre = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let half = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let clipped = !fits(centre, half);
    let (centre, half) = if clipped {
        // Lightroom shows sensor area the developed frame leaves out (an
        // Olympus aspect ratio): the crop keeps its centre and aspect ratio
        // (the format the photographer chose) and shrinks until all of it
        // shows the photo, never an empty corner. A centre off the photo
        // cannot be kept: it moves towards the frame's centre to where the
        // crop can be largest.
        let largest = |c: (f64, f64)| {
            if !on_photo(c) {
                return 0.0;
            }
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..40 {
                let mid = (lo + hi) / 2.0;
                if fits(c, (half.0 * mid, half.1 * mid)) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let mut best = (centre, largest(centre));
        if !on_photo(centre) {
            for step in 1..=32 {
                let t = f64::from(step) / 32.0;
                let c = (
                    centre.0 + t * (cx - centre.0),
                    centre.1 + t * (cy - centre.1),
                );
                let scale = largest(c);
                if scale > best.1 {
                    best = (c, scale);
                }
            }
        }
        let (c, scale) = best;
        (c, (half.0 * scale, half.1 * scale))
    } else {
        (centre, half)
    };
    let (x0, y0) = ((centre.0 - half.0).max(0.0), (centre.1 - half.1).max(0.0));
    let (x1, y1) = ((centre.0 + half.0).min(w), (centre.1 + half.1).min(h));
    if !(x1 - x0 >= 1.0 && y1 - y0 >= 1.0) {
        return None;
    }
    let round = |v: f64| (v * 100.0).round() / 100.0;
    Some(Placed {
        crop: json!({
            "unit": "px",
            "x": round(x0),
            "y": round(y0),
            "width": round(x1 - x0),
            "height": round(y1 - y0),
        }),
        rotation: round(rotation),
        aspect_ratio: (x1 - x0) / (y1 - y0),
        clipped,
    })
}

/// [`place`] for the RAW at `path`, on the newest frame version (the frame
/// a new edit, and the catalog import, develops). None when the file
/// cannot be read as a RAW.
pub fn place_for_file(path: &std::path::Path, crop: &LightroomCrop) -> Option<Placed> {
    if crop.is_whole() {
        return None;
    }
    let source = rawler::rawsource::RawSource::new(path).ok()?;
    let (frame, sensor) = rawtonic_rawdecode::source_frame::frame_and_sensor(
        &source,
        rawtonic_rawdecode::source_frame::VERSION,
    )?;
    let f = |r: [usize; 4]| r.map(|v| v as f64);
    place(crop, f(sensor), f(frame.rect), frame.orientation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop_of(placed: &Placed) -> [f64; 4] {
        ["x", "y", "width", "height"].map(|k| placed.crop[k].as_f64().unwrap())
    }

    /// The straightening undoes the tilt exactly: the bounded crop is the
    /// tilted rectangle's own size (a wrong sign would bound a larger one).
    #[test]
    fn a_straightened_crop_keeps_its_size() {
        let crop = LightroomCrop {
            left: 0.10,
            top: 0.08,
            right: 0.85,
            bottom: 0.90,
            angle: 5.0,
        };
        let sensor = [0.0, 0.0, 5184.0, 3888.0];
        let placed = place(&crop, sensor, sensor, 1).unwrap();
        // Sizes measured on Lightroom's export (c2): 4151.5 × 2837.6 px.
        let [_, _, w, h] = crop_of(&placed);
        assert!(
            (w - 4151.5).abs() < 1.0 && (h - 2837.6).abs() < 1.0,
            "{w} {h}"
        );
        assert_eq!(placed.rotation, -5.0);
        assert!(!placed.clipped);
    }

    /// Without an angle the crop is Lightroom's rectangle, moved into the
    /// developed frame (an Olympus 16:9 frame centred in its 4:3 sensor);
    /// one reaching beyond it keeps its centre and aspect ratio.
    #[test]
    fn an_aspect_frame_clips_a_wider_crop() {
        let crop = LightroomCrop {
            left: 0.1,
            top: 0.05,
            right: 0.8,
            bottom: 0.9,
            angle: 0.0,
        };
        let placed = place(
            &crop,
            [0.0, 0.0, 5184.0, 3888.0],
            [0.0, 484.0, 5184.0, 2920.0],
            1,
        )
        .unwrap();
        // 3628.8 × 3304.8 px in Lightroom, 2920 px of height here: the
        // crop keeps its centre (y 1362.4 → clamped by the frame) and its
        // aspect ratio.
        assert!(placed.clipped);
        let [x, y, w, h] = crop_of(&placed);
        assert!((w / h - 3628.8 / 3304.8).abs() < 1e-3, "{w} {h}");
        assert!(x >= 0.0 && y >= 0.0 && y + h <= 2920.01 && x + w <= 5184.01);
        assert!((x + w / 2.0 - 2332.8).abs() < 0.1, "{x} {w}");
        let inside = LightroomCrop {
            top: 0.2,
            bottom: 0.8,
            ..crop
        };
        let placed = place(
            &inside,
            [0.0, 0.0, 5184.0, 3888.0],
            [0.0, 484.0, 5184.0, 2920.0],
            1,
        )
        .unwrap();
        assert_eq!(crop_of(&placed), [518.4, 293.6, 3628.8, 2332.8]);
        assert!(!placed.clipped);
    }

    /// Orientation 6 (c4): the corners Lightroom's export showed in
    /// Rawtonic's oriented, unstraightened frame, within 2 px. Measured on
    /// the 16:9 frame (2920 × 5184) of a 4:3 sensor; here on the whole
    /// sensor (3888 × 5184), 484 px further right.
    #[test]
    fn an_oriented_crop_lands_where_lightroom_shows_it() {
        let crop = LightroomCrop {
            left: 0.10,
            top: 0.05,
            right: 0.80,
            bottom: 0.90,
            angle: 4.0,
        };
        let sensor = [0.0, 0.0, 5184.0, 3888.0];
        let placed = place(&crop, sensor, sensor, 6).unwrap();
        assert_eq!(placed.rotation, -4.0);
        assert!(!placed.clipped);
        let measured = [
            (173.2, 305.9),
            (3209.6, 518.3),
            (2941.1, 4359.4),
            (-95.6, 4147.3),
        ]
        .map(|(x, y)| (x + 484.0, y));
        let centre = measured
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0 / 4.0, a.1 + p.1 / 4.0));
        // The crop's centre turns about the frame's centre with the photo;
        // turn it back.
        let [x, y, w, h] = crop_of(&placed);
        let (cx, cy) = (1944.0, 2592.0);
        let (px, py) = (x + w / 2.0 - cx, y + h / 2.0 - cy);
        let (sin, cos) = 4.0f64.to_radians().sin_cos();
        let back = (cx + cos * px - sin * py, cy + sin * px + cos * py);
        assert!(
            (back.0 - centre.0).abs() < 2.0 && (back.1 - centre.1).abs() < 2.0,
            "{back:?} {centre:?}"
        );
        // Measured 3043.8 × 3850.8 (portrait).
        assert!(
            (w - 3043.8).abs() < 2.0 && (h - 3850.8).abs() < 2.0,
            "{w} {h}"
        );
        // On the 16:9 frame it reaches beyond the left edge (x −95.6).
        let frame = [0.0, 484.0, 5184.0, 2920.0];
        assert!(place(&crop, sensor, frame, 6).unwrap().clipped);
    }

    /// A straightened crop reaching beyond the frame (c2 on a 16:9 frame:
    /// Lightroom shows the 4:3 sensor) never shows an empty corner: every
    /// corner of the crop, turned back, lies on the photo.
    #[test]
    fn a_clipped_straightened_crop_has_no_empty_corner() {
        let crop = LightroomCrop {
            left: 0.10,
            top: 0.08,
            right: 0.85,
            bottom: 0.90,
            angle: 5.0,
        };
        let (w, h) = (5184.0, 2920.0);
        let placed = place(&crop, [0.0, 0.0, 5184.0, 3888.0], [0.0, 484.0, w, h], 1).unwrap();
        assert!(placed.clipped);
        let [x, y, cw, ch] = crop_of(&placed);
        assert!((cw / ch - 4151.5 / 2837.6).abs() < 2e-3, "{cw} {ch}");
        let (sin, cos) = placed.rotation.to_radians().sin_cos();
        for (px, py) in [(x, y), (x + cw, y), (x + cw, y + ch), (x, y + ch)] {
            let (dx, dy) = (px - w / 2.0, py - h / 2.0);
            let (ux, uy) = (w / 2.0 + cos * dx + sin * dy, h / 2.0 - sin * dx + cos * dy);
            assert!(
                (-0.6..=w + 0.6).contains(&ux) && (-0.6..=h + 0.6).contains(&uy),
                "{ux} {uy}"
            );
        }
    }

    /// A crop whose centre lies outside the developed frame (above the
    /// 16:9 frame of a 4:3 sensor) still arrives, trimmed, with its aspect
    /// ratio: never dropped as if there were no crop.
    #[test]
    fn a_crop_centred_outside_the_frame_is_kept() {
        let crop = LightroomCrop {
            left: 0.1,
            top: 0.0,
            right: 0.3,
            bottom: 0.1,
            angle: 0.0,
        };
        let placed = place(
            &crop,
            [0.0, 0.0, 5184.0, 3888.0],
            [0.0, 484.0, 5184.0, 2920.0],
            1,
        )
        .unwrap();
        assert!(placed.clipped);
        let [x, y, w, h] = crop_of(&placed);
        assert!((w / h - 1036.8 / 388.8).abs() < 1e-3, "{w} {h}");
        assert!(
            w > 100.0 && y >= 0.0 && y + h <= 2920.01 && x >= 0.0,
            "{x} {y} {w} {h}"
        );
    }

    #[test]
    fn the_whole_image_is_no_crop() {
        let whole = LightroomCrop {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
            angle: 0.0,
        };
        let s = [0.0, 0.0, 100.0, 80.0];
        assert!(place(&whole, s, s, 1).is_none());
        let turned = LightroomCrop {
            angle: 2.0,
            left: 0.05,
            top: 0.05,
            right: 0.95,
            bottom: 0.95,
        };
        assert!(place(&turned, s, s, 1).is_some());
    }
}
