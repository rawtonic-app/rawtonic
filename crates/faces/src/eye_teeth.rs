// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the face retouch's Eyes and Teeth act, made once from the analysis
//! and stored in the recipe as outlines, so the render (rawtonic-engine
//! `face_retouch::eye_teeth`) draws the masks at its own resolution from the
//! pixels it retouches and needs no model.
//!
//! - **Eyes**: per visible eye the lid opening (Face Mesh's 16-point eye
//!   contour) and the iris as a circle. Face Mesh's iris points place the
//!   circle within about a tenth of its radius; the circle is then fitted
//!   to the photo: the centre and radius whose rim inside the lid opening
//!   has the strongest outward dark-to-light step (iris to sclera), within
//!   a fifth of the radius of Face Mesh's. Eyes behind sunglasses (the
//!   parse's accessories class at the iris) are left out.
//! - **Mouth**: the inner lip outline when the mouth is open; the render
//!   finds the teeth inside it by their lightness and low colour, so gums,
//!   tongue and the dark of the mouth stay as they are.

use image::RgbImage;

use crate::Face;
use crate::geometry::{Polygon, luma};
use crate::landmarks::{LEFT_EYE, LEFT_IRIS, LIPS_INNER, RIGHT_EYE, RIGHT_IRIS};
use crate::parse::Class;

/// Accessories probability at the iris above which the eye is behind
/// sunglasses (eyes.rs: the top of its uncertain band).
const SUNGLASSES: f32 = 0.616;
/// Inner lip area over the mouth width squared below which the mouth is
/// closed.
const OPEN_MOUTH: f32 = 0.012;

#[derive(Clone, Debug, PartialEq)]
pub struct Eye {
    /// Iris centre and radius, photo pixels.
    pub iris: [f32; 3],
    /// Lid opening, photo pixels.
    pub lid: Vec<[f32; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EyeTeeth {
    /// The eyes on the left and on the right of the photo (in that order,
    /// both or neither); `None` for an eye behind sunglasses. Empty for a
    /// face without landmarks.
    pub eyes: Vec<Option<Eye>>,
    /// Inner lip outline of an open mouth.
    pub mouth: Option<Vec<[f32; 2]>>,
}

fn sample(image: &RgbImage, x: f32, y: f32) -> Option<f32> {
    let (w, h) = image.dimensions();
    let (x, y) = (x - 0.5, y - 0.5);
    if x < 0.0 || y < 0.0 || x >= (w - 1) as f32 || y >= (h - 1) as f32 {
        return None;
    }
    let (x0, y0) = (x as u32, y as u32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |a, b| luma(image.get_pixel(a, b).0);
    let top = p(x0, y0) * (1.0 - fx) + p(x0 + 1, y0) * fx;
    let bottom = p(x0, y0 + 1) * (1.0 - fx) + p(x0 + 1, y0 + 1) * fx;
    Some(top * (1.0 - fy) + bottom * fy)
}

/// Mean outward lightness step across the rim of the circle (cx, cy, r),
/// over the rim inside the lid opening; `None` when too little of the rim
/// is visible.
fn rim_step(image: &RgbImage, lid: &Polygon, cx: f32, cy: f32, r: f32) -> Option<f32> {
    const ANGLES: usize = 72;
    let delta = (r * 0.12).max(0.75);
    let (mut sum, mut n) = (0.0f32, 0usize);
    for k in 0..ANGLES {
        let a = k as f32 / ANGLES as f32 * std::f32::consts::TAU;
        let (dx, dy) = (a.cos(), a.sin());
        let (x, y) = (cx + r * dx, cy + r * dy);
        // Rim points hidden by the lids or at the lid edge say nothing.
        if lid.signed_distance(x, y) > -delta {
            continue;
        }
        let (Some(outer), Some(inner)) = (
            sample(image, x + delta * dx, y + delta * dy),
            sample(image, x - delta * dx, y - delta * dy),
        ) else {
            continue;
        };
        sum += outer - inner;
        n += 1;
    }
    (n >= ANGLES / 6).then(|| sum / n as f32)
}

/// The iris circle fitted to the photo near Face Mesh's (see the module
/// documentation); Face Mesh's own where the rim is not seen.
fn fit_iris(image: &RgbImage, lid: &Polygon, guess: [f32; 3]) -> [f32; 3] {
    let [cx, cy, r] = guess;
    if r < 2.0 {
        return guess;
    }
    let Some(mut best_score) = rim_step(image, lid, cx, cy, r) else {
        return guess;
    };
    let mut best = guess;
    // Coarse grid, then a finer one around the best.
    for (reach, step) in [(0.2f32, 0.05f32), (0.05, 0.0125)] {
        let [bx, by, br] = best;
        let n = (reach / step).round() as i32;
        for i in -n..=n {
            for j in -n..=n {
                for s in -n..=n {
                    let (x, y) = (bx + i as f32 * step * r, by + j as f32 * step * r);
                    let rr = br + s as f32 * step * r;
                    if (x - cx).hypot(y - cy) > 0.2 * r || (rr - r).abs() > 0.2 * r {
                        continue;
                    }
                    if let Some(score) = rim_step(image, lid, x, y, rr)
                        && score > best_score
                    {
                        best_score = score;
                        best = [x, y, rr];
                    }
                }
            }
        }
    }
    best
}

fn iris_guess(points: &[[f32; 2]], indices: &[usize; 5]) -> [f32; 3] {
    let c = points[indices[0]];
    let r = indices[1..]
        .iter()
        .map(|&i| (points[i][0] - c[0]).hypot(points[i][1] - c[1]))
        .sum::<f32>()
        / 4.0;
    [c[0], c[1], r]
}

fn area(points: &[[f32; 2]]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % n]);
            p[0] * q[1] - q[0] * p[1]
        })
        .sum::<f32>()
        .abs()
        / 2.0
}

/// The eyes and mouth of `face` in `image` (the photo the analysis was made
/// of); empty for faces without landmarks.
pub fn eye_teeth(image: &RgbImage, face: &Face) -> EyeTeeth {
    let Some(points) = face.landmarks.as_deref() else {
        return EyeTeeth::default();
    };
    let mut eyes: Vec<([f32; 3], Option<Eye>)> =
        [(&RIGHT_EYE, &RIGHT_IRIS), (&LEFT_EYE, &LEFT_IRIS)]
            .into_iter()
            .map(|(contour, iris)| {
                let lid: Vec<[f32; 2]> = contour.iter().map(|&i| points[i]).collect();
                let guess = iris_guess(points, iris);
                let (accessories, weight) =
                    face.parse.sample(Class::Accessories, guess[0], guess[1]);
                if weight > 0.0 && accessories > SUNGLASSES {
                    return (guess, None);
                }
                let outline = Polygon::new(lid.clone());
                let iris = fit_iris(image, &outline, guess);
                (guess, Some(Eye { iris, lid }))
            })
            .collect();
    // The subject's right eye is mostly on the photo's left, but not in a
    // mirrored photo: order by place.
    eyes.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
    let eyes = eyes.into_iter().map(|(_, eye)| eye).collect();
    let inner: Vec<[f32; 2]> = LIPS_INNER.iter().map(|&i| points[i]).collect();
    let (a, b) = (points[61], points[291]);
    let width = (b[0] - a[0]).hypot(b[1] - a[1]);
    let mouth = (width > 0.0 && area(&inner) > OPEN_MOUTH * width * width).then_some(inner);
    EyeTeeth { eyes, mouth }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// A white eye opening with a dark iris of radius 20 at (100, 100).
    fn eye_image() -> RgbImage {
        RgbImage::from_fn(200, 200, |x, y| {
            let d = (x as f32 + 0.5 - 100.0).hypot(y as f32 + 0.5 - 100.0);
            if d < 20.0 {
                Rgb([70, 50, 40])
            } else {
                Rgb([225, 220, 215])
            }
        })
    }

    fn lid() -> Polygon {
        Polygon::new(
            (0..16)
                .map(|k| {
                    let a = k as f32 / 16.0 * std::f32::consts::TAU;
                    [100.0 + 60.0 * a.cos(), 100.0 + 24.0 * a.sin()]
                })
                .collect(),
        )
    }

    #[test]
    fn iris_circle_snaps_to_the_rim() {
        let fitted = fit_iris(&eye_image(), &lid(), [103.0, 101.0, 17.5]);
        assert!((fitted[0] - 100.0).abs() < 1.0, "{fitted:?}");
        assert!((fitted[1] - 100.0).abs() < 1.5, "{fitted:?}");
        assert!((fitted[2] - 20.0).abs() < 1.0, "{fitted:?}");
    }

    #[test]
    fn iris_stays_put_without_a_rim() {
        let flat = RgbImage::from_pixel(200, 200, Rgb([128, 128, 128]));
        assert_eq!(
            fit_iris(&flat, &lid(), [103.0, 101.0, 17.5]),
            [103.0, 101.0, 17.5]
        );
    }

    #[test]
    fn mouth_area_is_the_polygon_area() {
        assert_eq!(area(&[[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]]), 8.0);
    }
}
