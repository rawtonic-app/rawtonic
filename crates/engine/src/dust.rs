//! Find Dust: sensor dust found on the photo's base image and placed as
//! ordinary heal spots (`spot_removal`), Lightroom's Dust > Apply flow
//! with our own detector.
//!
//! Dust on the sensor shows as a soft, round, slightly darker disc that is
//! only visible on flat areas (sky, studio backdrops). The detector works on
//! a reduced copy of the base image (the photo as decoded, so positions are
//! sensor positions): square root of the linear luminance, where photon
//! noise is about even across brightness. At several sizes it compares the
//! mean of a box with the ring around it (a centre-surround filter that a
//! smooth gradient does not answer); the strongest size per pixel and the
//! local maxima over the photo are the candidates. A candidate is kept only
//! where the wider surroundings are flat: what is left of their variance
//! after a plane fit (a sky gradient) is at most photon noise plus a small
//! share of the spot's own contrast, so edges, lines, text and fine texture
//! are left out; very dark or bright surroundings (dust invisible there,
//! clipped values) and strong contrast (objects, not dust) too, and every
//! face box the caller gives.
//!
//! The analysis is independent of the sensitivity: it keeps every candidate
//! down to the most sensitive setting with its contrast, so the sensitivity
//! slider only filters (`select`), live. The slider is Visualize Spots'
//! (`spot_removal::sensitivity_factor`): the same 0–100 scale and curve.

use image::DynamicImage;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};

/// Long edge of the copy the detector works on (about 2.5 sensor pixels
/// per work pixel on 24 MP, 3.5 on 45 MP: the smallest dust a pro sees at
/// 100 % spans several work pixels).
const WORK_EDGE: usize = 2400;
/// Inner half-sizes (work pixels) of the box sizes searched.
const SCALES: [usize; 8] = [1, 2, 3, 4, 6, 8, 11, 15];
/// Relative darkening (in square-root space) needed at sensitivity 50; the
/// slider scales it as Visualize Spots scales its threshold.
const CONTRAST_AT_50: f32 = 0.006;
/// Darker than this share is an object (a bird, a letter), not dust.
const MAX_CONTRAST: f32 = 0.18;
/// Minimum significance over photon noise.
const MIN_SIGNIFICANCE: f32 = 5.0;
/// Surroundings' texture (left after the plane fit and photon noise: an
/// edge, a line, fine detail) may be at most this share of the spot's
/// darkening.
const TEXTURE_PER_CONTRAST: f32 = 0.5;
/// … and at most this share of their brightness.
const MAX_TEXTURE: f32 = 0.012;
/// The surroundings' change across may be at most this many times the
/// spot's darkening or this share of their brightness (an edge).
const STEP_PER_CONTRAST: f32 = 3.0;
const MAX_STEP: f32 = 0.05;
/// Surroundings brightness range (square root of linear luminance).
const MIN_LEVEL: f32 = 0.12;
const MAX_LEVEL: f32 = 0.97;
/// Isolation: other darker specks of at least this share of a spot's
/// contrast within `ISOLATION_REACH` × (size + 2) work pixels …
const NEIGHBOUR_SHARE: f32 = 0.5;
const ISOLATION_REACH: usize = 8;
/// … may be at most this many (dust may lie in small groups).
const MAX_NEIGHBOURS: usize = 2;
/// A spot's darkest pixel may be at most this many times as dark as its
/// box: dust shadows are soft.
const MAX_PEAK: f32 = 3.0;
/// Smallest half-size of a dust shadow's box, as a share of the long edge
/// (about 4 sensor pixels at 6000).
const MIN_HALF_SIZE: f64 = 0.0007;
/// At most this many spots per photo.
const MAX_SPOTS: usize = 400;

/// One dust candidate in base-image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Candidate {
    pub centre: (f64, f64),
    pub radius: f64,
    /// Relative darkening in square-root space (what the sensitivity gates).
    pub contrast: f32,
}

/// Every candidate of a base image down to sensitivity 100.
#[derive(Debug, Clone, Default)]
pub struct DustAnalysis {
    pub width: u32,
    pub height: u32,
    pub candidates: Vec<Candidate>,
}

/// The darkening a spot needs at `sensitivity` (0–100, more finds more).
pub fn min_contrast(sensitivity: f64) -> f32 {
    CONTRAST_AT_50 * crate::spot_removal::sensitivity_factor(sensitivity)
        / crate::spot_removal::sensitivity_factor(50.0)
}

/// The candidates found at `sensitivity`.
pub fn select(analysis: &DustAnalysis, sensitivity: f64) -> Vec<Candidate> {
    let needed = min_contrast(sensitivity);
    analysis
        .candidates
        .iter()
        .filter(|c| c.contrast >= needed)
        .copied()
        .collect()
}

/// Summed-area tables of values and their squares, (w + 1) × (h + 1).
struct Integral {
    w: usize,
    h: usize,
    sum: Vec<f64>,
    sum2: Vec<f64>,
}

impl Integral {
    fn new(values: &[f32], w: usize, h: usize) -> Self {
        let mut sum = vec![0.0f64; (w + 1) * (h + 1)];
        let mut sum2 = vec![0.0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let (mut row, mut row2) = (0.0f64, 0.0f64);
            for x in 0..w {
                let v = values[y * w + x] as f64;
                row += v;
                row2 += v * v;
                sum[(y + 1) * (w + 1) + x + 1] = sum[y * (w + 1) + x + 1] + row;
                sum2[(y + 1) * (w + 1) + x + 1] = sum2[y * (w + 1) + x + 1] + row2;
            }
        }
        Self { w, h, sum, sum2 }
    }

    /// Sum, sum of squares and count of the box x0..=x1, y0..=y1, clipped
    /// to the image.
    fn boxed(&self, x0: isize, y0: isize, x1: isize, y1: isize) -> (f64, f64, f64) {
        let cx = |v: isize| v.clamp(0, self.w as isize) as usize;
        let cy = |v: isize| v.clamp(0, self.h as isize) as usize;
        let (a, b, c, d) = (cx(x0), cy(y0), cx(x1 + 1), cy(y1 + 1));
        if c <= a || d <= b {
            return (0.0, 0.0, 0.0);
        }
        let at = |t: &[f64], x: usize, y: usize| t[y * (self.w + 1) + x];
        let s = |t: &[f64]| at(t, c, d) - at(t, a, d) - at(t, c, b) + at(t, a, b);
        (s(&self.sum), s(&self.sum2), ((c - a) * (d - b)) as f64)
    }

    /// The square of half-size `r` around (x, y).
    fn square(&self, x: isize, y: isize, r: isize) -> (f64, f64, f64) {
        self.boxed(x - r, y - r, x + r, y + r)
    }
}

/// The ring between the squares of half-sizes `inner` and `outer`.
fn ring(i: &Integral, x: isize, y: isize, inner: isize, outer: isize) -> (f64, f64, f64) {
    let o = i.square(x, y, outer);
    let n = i.square(x, y, inner);
    (o.0 - n.0, o.1 - n.1, o.2 - n.2)
}

fn mean(s: (f64, f64, f64)) -> f64 {
    if s.2 > 0.0 { s.0 / s.2 } else { 0.0 }
}

fn variance(s: (f64, f64, f64)) -> f64 {
    if s.2 > 1.0 {
        (s.1 / s.2 - (s.0 / s.2).powi(2)).max(0.0)
    } else {
        0.0
    }
}

/// What the detector looks at: square root of the linear luminance,
/// block-averaged by an integer `factor` (`width` × `height`).
#[derive(Debug, Clone)]
pub struct WorkCopy {
    pub values: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub factor: usize,
}

/// The work copy of a base image. `encoded`: the base holds sRGB-encoded
/// values (non-RAW files), linearised first.
pub fn work_copy(base: &DynamicImage, encoded: bool) -> WorkCopy {
    let rgb = base.to_rgb32f();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let factor = w.max(h).div_ceil(WORK_EDGE).max(1);
    let (ow, oh) = (w / factor, h / factor);
    let linear = |v: f32| {
        if encoded {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        } else {
            v
        }
    };
    let raw = rgb.as_raw();
    let values: Vec<f32> = (0..oh)
        .into_par_iter()
        .flat_map_iter(|oy| {
            (0..ow).map(move |ox| {
                let mut acc = 0.0f32;
                for y in oy * factor..(oy + 1) * factor {
                    for x in ox * factor..(ox + 1) * factor {
                        let p = &raw[(y * w + x) * 3..(y * w + x) * 3 + 3];
                        acc +=
                            0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2]);
                    }
                }
                (acc / (factor * factor) as f32).max(0.0).sqrt()
            })
        })
        .collect();
    WorkCopy {
        values,
        width: ow,
        height: oh,
        factor,
    }
}

/// Photon noise of one work pixel: from the residuals to the 3 × 3 mean
/// over pixels neither near black nor clipped, by their lower quartile (a
/// mostly textured photo does not inflate it).
fn noise(values: &[f32], w: usize, h: usize, integral: &Integral) -> f32 {
    let mut residuals: Vec<f32> = (1..h.saturating_sub(1))
        .step_by(3)
        .flat_map(|y| (1..w.saturating_sub(1)).step_by(3).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            let v = values[y * w + x];
            if !(0.05..=0.97).contains(&v) {
                return None;
            }
            let m = mean(integral.square(x as isize, y as isize, 1)) as f32;
            Some((v - m).abs())
        })
        .collect();
    if residuals.is_empty() {
        return 1e-3;
    }
    let k = residuals.len() / 4;
    let (_, q, _) = residuals.select_nth_unstable_by(k, f32::total_cmp);
    // |N(0, σ)| has its lower quartile at 0.3186 σ; the 3 × 3 mean holds
    // the pixel itself (residual σ · √(8/9)).
    (*q / 0.3186 / (8.0f32 / 9.0).sqrt()).max(1e-4)
}

/// Finds every dust candidate of `base` (the photo as decoded; `encoded`
/// for sRGB-encoded bases), leaving out the face boxes `faces` (base pixels,
/// x0, y0, x1, y1).
pub fn analyse(base: &DynamicImage, encoded: bool, faces: &[[f64; 4]]) -> DustAnalysis {
    analyse_work(
        &work_copy(base, encoded),
        (base.width(), base.height()),
        faces,
    )
}

/// `analyse` on a work copy of a base image of `size`.
pub fn analyse_work(work: &WorkCopy, size: (u32, u32), faces: &[[f64; 4]]) -> DustAnalysis {
    let (width, height) = size;
    let (values, w, h, factor) = (&work.values, work.width, work.height, work.factor);
    let mut analysis = DustAnalysis {
        width,
        height,
        candidates: Vec::new(),
    };
    if w < 32 || h < 32 {
        return analysis;
    }
    let integral = Integral::new(values, w, h);
    let sigma = noise(values, w, h, &integral);
    let floor = min_contrast(100.0);
    // A dust shadow is blurred by the glass in front of the sensor: even at
    // f/22 it spans some 15 sensor pixels, so specks in focus (dirt, texture)
    // that are smaller are not dust.
    let long = f64::from(width.max(height));
    let min_half = ((MIN_HALF_SIZE * long / factor as f64).ceil() as usize).max(1);

    // Per pixel: the size with the strongest significant darkening.
    let best: Vec<(f32, u8)> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let integral = &integral;
            (0..w).map(move |x| {
                let (x, y) = (x as isize, y as isize);
                let mut found = (0.0f32, 0u8);
                for (k, &a) in SCALES.iter().enumerate() {
                    if a < min_half {
                        continue;
                    }
                    let a = a as isize;
                    let inner = integral.square(x, y, a);
                    let around = ring(integral, x, y, a, 2 * a + 1);
                    let (mi, mr) = (mean(inner), mean(around));
                    if mr < MIN_LEVEL as f64 {
                        continue;
                    }
                    let d = (mr - mi) as f32;
                    let spread = sigma * ((1.0 / inner.2 + 1.0 / around.2.max(1.0)) as f32).sqrt();
                    let contrast = d / mr as f32;
                    if d > MIN_SIGNIFICANCE * spread && contrast > found.0 {
                        found = (contrast, k as u8);
                    }
                }
                found
            })
        })
        .collect();

    // Local maxima of the darkening (ties go to the first in reading
    // order), down to a share of the floor: the neighbours the isolation
    // test counts.
    let peaks: Vec<(usize, usize, f32, usize)> = (2..h - 2)
        .into_par_iter()
        .flat_map_iter(|y| {
            let best = &best;
            (2..w - 2).filter_map(move |x| {
                let (c, k) = best[y * w + x];
                if c < NEIGHBOUR_SHARE * floor {
                    return None;
                }
                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let o = best[(y as isize + dy) as usize * w + (x as isize + dx) as usize].0;
                        let earlier = dy < 0 || (dy == 0 && dx < 0);
                        if o > c || (earlier && o == c) {
                            return None;
                        }
                    }
                }
                Some((x, y, c, SCALES[k as usize]))
            })
        })
        .collect();
    let cell = 32usize;
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut grid: Vec<Vec<usize>> = vec![Vec::new(); gw * gh];
    for (i, p) in peaks.iter().enumerate() {
        grid[(p.1 / cell) * gw + p.0 / cell].push(i);
    }

    let mut candidates: Vec<(Candidate, f32)> = peaks
        .par_iter()
        .filter_map(|&(x, y, c, a)| {
            if c < floor {
                return None;
            }
            // Dust stands alone on a flat area; mottled or textured
            // surfaces (floors, roofs, out-of-focus foliage) have many
            // darker specks of the same kind around each one.
            let reach = ISOLATION_REACH * (a + 2);
            let (cx0, cx1) = (
                x.saturating_sub(reach) / cell,
                ((x + reach) / cell).min(gw - 1),
            );
            let (cy0, cy1) = (
                y.saturating_sub(reach) / cell,
                ((y + reach) / cell).min(gh - 1),
            );
            let own = (2 * a + 1) as f64;
            let mut neighbours = 0;
            for gy in cy0..=cy1 {
                for gx in cx0..=cx1 {
                    for &j in &grid[gy * gw + gx] {
                        let (ox, oy, oc, _) = peaks[j];
                        let d = (ox as f64 - x as f64).hypot(oy as f64 - y as f64);
                        if d > own && d <= reach as f64 && oc >= NEIGHBOUR_SHARE * c {
                            neighbours += 1;
                        }
                    }
                }
            }
            if neighbours > MAX_NEIGHBOURS {
                return None;
            }
            let a = a as isize;
            let (contrast, darkening) =
                flat_surroundings(values, &integral, x as isize, y as isize, a, sigma)?;
            if !(floor..=MAX_CONTRAST).contains(&contrast) {
                return None;
            }
            let centre = (
                (x as f64 + 0.5) * factor as f64,
                (y as f64 + 0.5) * factor as f64,
            );
            let in_face = faces.iter().any(|f| {
                let (mx, my) = ((f[2] - f[0]) * 0.25, (f[3] - f[1]) * 0.25);
                centre.0 >= f[0] - mx
                    && centre.0 <= f[2] + mx
                    && centre.1 >= f[1] - my
                    && centre.1 <= f[3] + my
            });
            if in_face {
                return None;
            }
            // The heal circle covers the disc and its soft rim.
            let radius = ((1.6 * a as f64 + 1.5) * factor as f64).max(4.0);
            Some((
                Candidate {
                    centre,
                    radius,
                    contrast,
                },
                darkening / sigma,
            ))
        })
        .collect();

    // One candidate per spot: the most significant.
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut kept: Vec<Candidate> = Vec::new();
    for (c, _) in candidates {
        let overlaps = kept.iter().any(|k| {
            (k.centre.0 - c.centre.0).hypot(k.centre.1 - c.centre.1) < k.radius.max(c.radius)
        });
        if !overlaps {
            kept.push(c);
        }
        if kept.len() >= MAX_SPOTS {
            break;
        }
    }
    analysis.candidates = kept;
    analysis
}

/// Contrast and darkening of a spot at (x, y) of size `a` when its
/// surroundings are flat, else None.
fn flat_surroundings(
    values: &[f32],
    i: &Integral,
    x: isize,
    y: isize,
    a: isize,
    sigma: f32,
) -> Option<(f32, f32)> {
    let inner = i.square(x, y, a);
    let near = ring(i, x, y, a, 2 * a + 1);
    let (mi, mr) = (mean(inner), mean(near));
    if !(MIN_LEVEL as f64..=MAX_LEVEL as f64).contains(&mr) {
        return None;
    }
    let darkening = (mr - mi) as f32;
    if darkening <= 0.0 {
        return None;
    }
    // The wider surroundings, outside the spot and its rim.
    let m = 2 * a + 1;
    let b = (5 * a + 3).max(m + 4);
    let wide = ring(i, x, y, m, b);
    let side = |x0, y0, x1, y1| mean(i.boxed(x0, y0, x1, y1));
    let (left, right) = (
        side(x - b, y - b, x - m - 1, y + b),
        side(x + m + 1, y - b, x + b, y + b),
    );
    let (top, bottom) = (
        side(x - b, y - b, x + b, y - m - 1),
        side(x - b, y + m + 1, x + b, y + b),
    );
    let span = (b + m + 1) as f64;
    let (gx, gy) = ((right - left) / span, (bottom - top) / span);
    let level = mean(wide);
    if level < MIN_LEVEL as f64 {
        return None;
    }
    // A plane's variance over the square of half-size b is g² b² / 3 per
    // axis (the hole changes it little).
    let plane = (gx * gx + gy * gy) * (b * b) as f64 / 3.0;
    let texture = ((variance(wide) - plane - (sigma as f64).powi(2)).max(0.0)).sqrt() as f32;
    // A dust shadow is soft: its darkest pixel is not much darker than its
    // disc. A speck in focus (dirt, a nail head) is far darker in its middle
    // than the box the darkening was measured over.
    let darkest = (y - a..=y + a)
        .filter(|&yy| yy >= 0 && (yy as usize) < i.h)
        .flat_map(|yy| {
            (x - a..=x + a)
                .filter(|&xx| xx >= 0 && (xx as usize) < i.w)
                .map(move |xx| values[yy as usize * i.w + xx as usize])
        })
        .fold(f32::INFINITY, f32::min);
    let peak = mr as f32 - darkest;
    // An edge beside the spot: a jump across the surroundings that is large
    // both against the spot and in itself (a smooth sky gradient is not).
    let step = ((right - left).abs().max((bottom - top).abs())) as f32;
    let edge = step > STEP_PER_CONTRAST * darkening && step > MAX_STEP * level as f32;
    if texture > TEXTURE_PER_CONTRAST * darkening
        || texture > MAX_TEXTURE * level as f32
        || edge
        || peak > MAX_PEAK * darkening
    {
        return None;
    }
    Some((darkening / mr as f32, darkening))
}

/// A found spot where the photo shows it: mask-frame centre and radius
/// (`spot_removal`'s frame), with its base pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundSpot {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    #[serde(skip)]
    pub base: Candidate,
}

/// The candidates of `analysis` at `sensitivity` where the photo
/// `adjustments` describe shows them: inside the crop, not on a spot the
/// recipe already has (shown or hidden), so running again adds nothing
/// twice.
pub fn placed(analysis: &DustAnalysis, sensitivity: f64, adjustments: &Value) -> Vec<FoundSpot> {
    let frame = crate::spot_removal::mask_frame(adjustments, analysis.width, analysis.height);
    let crop = crate::image_processing::crop_rect(
        &adjustments["crop"],
        frame.0.round() as u32,
        frame.1.round() as u32,
    )
    .map(|(x, y, w, h)| (x as f64, y as f64, w as f64, h as f64))
    .unwrap_or((0.0, 0.0, frame.0, frame.1));
    let existing = existing_spots(adjustments);
    select(analysis, sensitivity)
        .into_iter()
        .filter_map(|c| {
            let (x, y) = crate::red_eye::to_mask(adjustments, frame, c.centre)?;
            let radius = crate::lightroom_retouch::mask_radius(
                adjustments,
                frame,
                (x, y),
                c.centre,
                c.radius,
            )?;
            let inside = x >= crop.0 && y >= crop.1 && x < crop.0 + crop.2 && y < crop.1 + crop.3;
            let taken = existing
                .iter()
                .any(|(path, r)| distance_to(path, (x, y)) < r + 0.5 * radius);
            (inside && !taken).then_some(FoundSpot {
                x,
                y,
                radius,
                base: c,
            })
        })
        .collect()
}

/// The recipe's placed spots as (centre or path, radius), mask frame.
fn existing_spots(adjustments: &Value) -> Vec<(Vec<(f64, f64)>, f64)> {
    adjustments["spots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s.get(crate::lightroom_retouch::PENDING).is_none())
        .filter_map(|s| {
            let n = |k: &str| s[k].as_f64().filter(|v| v.is_finite());
            let mut path: Vec<(f64, f64)> = s["points"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| Some((p["x"].as_f64()?, p["y"].as_f64()?)))
                .collect();
            if path.len() < 2 {
                path = vec![(n("x")?, n("y")?)];
            }
            Some((path, n("radius")?))
        })
        .collect()
}

fn distance_to(path: &[(f64, f64)], p: (f64, f64)) -> f64 {
    if path.len() == 1 {
        return (path[0].0 - p.0).hypot(path[0].1 - p.1);
    }
    path.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let (vx, vy) = (b.0 - a.0, b.1 - a.1);
            let l2 = vx * vx + vy * vy;
            let t = if l2 > 0.0 {
                (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p.0 - a.0 - t * vx).hypot(p.1 - a.1 - t * vy)
        })
        .fold(f64::INFINITY, f64::min)
}

/// The found spots as heal spots of the recipe (`adjustments.spots` items
/// without `id`, `mode`, `feather`, `opacity`: the editor adds its
/// settings): each with its automatic source searched on `base` (the base
/// image with the recipe's patches and spots) and `dust`, the spot in the
/// unrotated raw of the photo decoded with EXIF `orientation` (normalised
/// centre, radius as a share of the unrotated width), which `sync_items`
/// and a later run use to recognise the same dust. Spots without a source
/// are left out.
pub fn heal_spots(
    base: &DynamicImage,
    adjustments: &Value,
    found: &[FoundSpot],
    orientation: u16,
) -> Vec<Value> {
    let image = base.to_rgb32f();
    let (w, h) = (image.width(), image.height());
    let frame = crate::spot_removal::mask_frame(adjustments, w, h);
    let round = |v: f64| (v * 100.0).round() / 100.0;
    found
        .par_iter()
        .filter_map(|spot| {
            let c = spot.base;
            let source = crate::spot_removal::best_source(&image, c.centre, c.radius)?;
            let (sx, sy) = crate::spot_removal::to_mask_point(
                adjustments,
                frame,
                (spot.x, spot.y),
                (source.0 - c.centre.0, source.1 - c.centre.1),
            )?;
            Some(json!({
                "x": round(spot.x),
                "y": round(spot.y),
                "radius": round(spot.radius),
                "sourceX": round(sx),
                "sourceY": round(sy),
                "visible": true,
                "dust": raw_key(orientation, w, h, c.centre),
            }))
        })
        .collect()
}

/// A base-pixel point of a photo decoded at `width` × `height` with EXIF
/// `orientation`, normalised in its unrotated raw (where the sensor's dust
/// sits): `lightroom_retouch::orient_point` undone.
pub fn unoriented(orientation: u16, width: u32, height: u32, p: (f64, f64)) -> (f64, f64) {
    let (u, v) = (p.0 / f64::from(width), p.1 / f64::from(height));
    // Every orientation is its own inverse but the two quarter turns.
    let inverse = match orientation {
        6 => 8,
        8 => 6,
        o => o,
    };
    crate::lightroom_retouch::orient_point(inverse, u, v)
}

fn raw_key(orientation: u16, width: u32, height: u32, p: (f64, f64)) -> Value {
    let (x, y) = unoriented(orientation, width, height, p);
    json!({"x": (x * 1e5).round() / 1e5, "y": (y * 1e5).round() / 1e5})
}

/// Width of the unrotated raw of a base image.
fn raw_width(orientation: u16, width: u32, height: u32) -> f64 {
    f64::from(if (5..=8).contains(&orientation) {
        height
    } else {
        width
    })
}

/// The recipe's dust spots (those Find Dust made, `dust`) as spots for
/// other photos of the same camera: pending items in the form
/// `lightroom_retouch` places on each photo (normalised in the unrotated
/// raw, without the orientation, which is the other photo's), from where
/// they stand now (moved or resized since included). `width` × `height`
/// and `orientation`: this photo's base image and EXIF orientation. Hidden
/// spots and strokes are left out.
pub fn sync_items(adjustments: &Value, width: u32, height: u32, orientation: u16) -> Vec<Value> {
    let frame = crate::spot_removal::mask_frame(adjustments, width, height);
    let to_base = |x: f64, y: f64| crate::spot_removal::to_base(adjustments, frame, x, y);
    let unit = raw_width(orientation, width, height);
    let round = |v: f64| (v * 1e6).round() / 1e6;
    adjustments["spots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("dust").is_some_and(Value::is_object)
                && s.get(crate::lightroom_retouch::PENDING).is_none()
                && s["visible"].as_bool().unwrap_or(true)
                && s["points"].as_array().is_none_or(|p| p.len() < 2)
        })
        .filter_map(|s| {
            let n = |k: &str| s[k].as_f64().filter(|v| v.is_finite());
            let (x, y, r) = (n("x")?, n("y")?, n("radius")?);
            let centre = to_base(x, y);
            let edge = to_base(x + r, y);
            let radius = (edge.0 - centre.0).hypot(edge.1 - centre.1);
            let source = to_base(n("sourceX")?, n("sourceY")?);
            let (cx, cy) = unoriented(orientation, width, height, centre);
            let (sx, sy) = unoriented(orientation, width, height, source);
            Some(json!({
                "mode": s["mode"].as_str().unwrap_or("heal"),
                "feather": n("feather").unwrap_or(0.0),
                "opacity": n("opacity").unwrap_or(100.0),
                "visible": true,
                "dust": {"x": round(cx), "y": round(cy)},
                crate::lightroom_retouch::PENDING: {
                    "x": round(cx),
                    "y": round(cy),
                    "radius": round(radius / unit),
                    "sourceX": round(sx),
                    "sourceY": round(sy),
                },
            }))
        })
        .collect()
}

/// Whether two `dust` keys name the same dust: within a thousandth of the
/// frame (a few pixels; a moved spot or another photo's rounding).
fn same_dust(a: &Value, b: &Value) -> bool {
    let p = |v: &Value| Some((v["x"].as_f64()?, v["y"].as_f64()?));
    match (p(a), p(b)) {
        (Some(a), Some(b)) => (a.0 - b.0).hypot(a.1 - b.1) < 1e-3,
        _ => false,
    }
}

/// `items` (`sync_items`) added to another photo's recipe with its EXIF
/// `orientation`, but for dust it already has (synced or found there):
/// returns how many were added.
pub fn add_synced(
    adjustments: &mut Value,
    items: &[Value],
    orientation: u16,
    ids: &mut dyn FnMut() -> String,
) -> usize {
    if !adjustments.is_object() {
        *adjustments = json!({});
    }
    let spots = adjustments
        .as_object_mut()
        .expect("object")
        .entry("spots")
        .or_insert_with(|| json!([]));
    if !spots.is_array() {
        *spots = json!([]);
    }
    let spots = spots.as_array_mut().expect("array");
    let mut added = 0;
    for item in items {
        let known = spots.iter().any(|s| same_dust(&s["dust"], &item["dust"]));
        if known {
            continue;
        }
        let mut item = item.clone();
        item["id"] = json!(ids());
        item[crate::lightroom_retouch::PENDING][crate::lightroom_retouch::ORIENTATION] =
            json!(orientation);
        spots.push(item);
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb32FImage;

    /// Deterministic grain of a given strength (uniform, mean 0).
    fn grain(x: u32, y: u32, strength: f32) -> f32 {
        let h =
            (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
        ((h >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * strength
    }

    /// A soft dust shadow: transmission 1 − depth in the disc, a smooth rim.
    fn dust(x: f32, y: f32, cx: f32, cy: f32, radius: f32, depth: f32) -> f32 {
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / radius;
        let t = ((1.3 - d) / 0.6).clamp(0.0, 1.0);
        1.0 - depth * t * t * (3.0 - 2.0 * t)
    }

    /// A sky-like gradient (linear values) with grain, `spots` of dust
    /// (x, y, radius, depth), and `extra` painted on top.
    fn photo(
        w: u32,
        h: u32,
        spots: &[(f32, f32, f32, f32)],
        extra: impl Fn(u32, u32, f32) -> f32,
    ) -> DynamicImage {
        DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(w, h, |x, y| {
            let base = 0.25 + 0.15 * y as f32 / h as f32 + 0.05 * x as f32 / w as f32;
            let mut v = base * (1.0 + grain(x, y, 0.02));
            for &(cx, cy, r, depth) in spots {
                v *= dust(x as f32, y as f32, cx, cy, r, depth);
            }
            let v = extra(x, y, v);
            image::Rgb([v * 0.9, v, v * 1.15])
        }))
    }

    fn near(found: &[Candidate], x: f64, y: f64, tolerance: f64) -> bool {
        found
            .iter()
            .any(|c| (c.centre.0 - x).hypot(c.centre.1 - y) < tolerance)
    }

    #[test]
    fn finds_faint_dust_on_a_flat_sky_and_nothing_else() {
        let spots = [
            (500.0, 300.0, 12.0, 0.06),
            (1500.0, 800.0, 20.0, 0.05),
            (2600.0, 1400.0, 8.0, 0.08),
        ];
        let image = photo(3000, 2000, &spots, |_, _, v| v);
        let analysis = analyse(&image, false, &[]);
        let found = select(&analysis, 50.0);
        for &(x, y, ..) in &spots {
            assert!(
                near(&found, x as f64, y as f64, 15.0),
                "missed ({x}, {y}): {found:?}"
            );
        }
        assert_eq!(found.len(), spots.len(), "{found:?}");
        // The heal circle covers the dust's disc.
        for c in &found {
            let (_, _, r, _) = spots
                .iter()
                .find(|s| (s.0 as f64 - c.centre.0).hypot(s.1 as f64 - c.centre.1) < 15.0)
                .unwrap();
            assert!(
                c.radius >= *r as f64 && c.radius <= 4.0 * *r as f64,
                "{c:?}"
            );
        }
    }

    #[test]
    fn sensitivity_filters_the_faint_ones_live() {
        let spots = [(600.0, 400.0, 14.0, 0.12), (1800.0, 900.0, 14.0, 0.016)];
        let image = photo(2400, 1600, &spots, |_, _, v| v);
        let analysis = analyse(&image, false, &[]);
        let low = select(&analysis, 0.0);
        let high = select(&analysis, 100.0);
        assert!(
            near(&low, 600.0, 400.0, 15.0) && !near(&low, 1800.0, 900.0, 15.0),
            "{low:?}"
        );
        assert!(near(&high, 1800.0, 900.0, 15.0), "{high:?}");
    }

    #[test]
    fn leaves_out_edges_lines_and_fine_texture() {
        // A dark dot sitting on a horizon edge, one on a thin line (a wire),
        // one in fine texture (foliage-like): none is dust on a flat area.
        let spots = [
            (800.0, 1000.0, 12.0, 0.08),
            (1600.0, 500.0, 12.0, 0.08),
            (2400.0, 1400.0, 12.0, 0.08),
            // The one real dust spot on flat sky.
            (400.0, 300.0, 12.0, 0.08),
        ];
        let image = photo(3000, 2000, &spots, |x, y, v| {
            let edge = if y > 1000 { 0.45 } else { 1.0 };
            let wire = if (x as i32 - 1600).abs() <= 1 {
                0.7
            } else {
                1.0
            };
            let texture = if x > 2100 && y > 1100 && y < 1700 {
                1.0 + 0.25 * ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos())
            } else {
                1.0
            };
            v * edge * wire * texture
        });
        let found = select(&analyse(&image, false, &[]), 100.0);
        assert!(near(&found, 400.0, 300.0, 15.0), "{found:?}");
        assert!(!near(&found, 800.0, 1000.0, 40.0), "edge: {found:?}");
        assert!(!near(&found, 1600.0, 500.0, 40.0), "wire: {found:?}");
        assert!(!near(&found, 2400.0, 1400.0, 40.0), "texture: {found:?}");
        // Nothing along the edge, the wire or in the texture either.
        assert!(
            found.iter().all(|c| (c.centre.1 - 1000.0).abs() > 30.0
                && (c.centre.0 - 1600.0).abs() > 30.0
                && !(c.centre.0 > 2080.0 && c.centre.1 > 1080.0 && c.centre.1 < 1720.0)),
            "{found:?}"
        );
    }

    #[test]
    fn leaves_out_faces_and_dark_objects() {
        // A freckle-like dot inside a face box, and a bird (a dark blob, far
        // darker than dust).
        let spots = [
            (1000.0, 600.0, 10.0, 0.08),
            (2000.0, 500.0, 10.0, 0.8),
            (400.0, 1500.0, 10.0, 0.08),
        ];
        let image = photo(3000, 2000, &spots, |_, _, v| v);
        let faces = [[900.0, 500.0, 1100.0, 750.0]];
        let found = select(&analyse(&image, false, &faces), 100.0);
        assert!(!near(&found, 1000.0, 600.0, 40.0), "face: {found:?}");
        assert!(!near(&found, 2000.0, 500.0, 40.0), "bird: {found:?}");
        assert!(near(&found, 400.0, 1500.0, 15.0), "{found:?}");
    }

    #[test]
    fn finds_dust_on_srgb_encoded_bases_too() {
        let spots = [(700.0, 500.0, 12.0, 0.07)];
        let linear = photo(2000, 1400, &spots, |_, _, v| v).to_rgb32f();
        let encoded = Rgb32FImage::from_fn(2000, 1400, |x, y| {
            let p = linear.get_pixel(x, y).0;
            image::Rgb(p.map(|v| {
                if v <= 0.003_130_8 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }))
        });
        let found = select(
            &analyse(&DynamicImage::ImageRgb32F(encoded), true, &[]),
            50.0,
        );
        assert!(near(&found, 700.0, 500.0, 15.0), "{found:?}");
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn placement_follows_the_geometry_crop_and_skips_existing_spots() {
        let spots = [
            (500.0, 300.0, 12.0, 0.08),
            (1500.0, 800.0, 12.0, 0.08),
            (1800.0, 1100.0, 12.0, 0.08),
        ];
        let image = photo(2000, 1400, &spots, |_, _, v| v);
        let analysis = analyse(&image, false, &[]);
        // A quarter turn: the base (500, 300) shows at (1400 − 300, 500)
        // in the turned frame; the crop leaves out the third spot; a spot
        // already on the second.
        let adjustments = json!({
            "orientationSteps": 1,
            "crop": {"x": 0.0, "y": 0.0, "width": 1400.0, "height": 1600.0},
            "spots": [{"x": 600.0, "y": 1500.0, "radius": 20.0, "sourceX": 600.0, "sourceY": 1400.0,
                       "visible": false}],
        });
        let found = placed(&analysis, 50.0, &adjustments);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            (found[0].x - 1100.0).abs() < 6.0 && (found[0].y - 500.0).abs() < 6.0,
            "{found:?}"
        );
        // The heal spots carry their raw position and a source off the dust.
        let spots = heal_spots(&image, &adjustments, &found, 1);
        assert_eq!(spots.len(), 1);
        let s = &spots[0];
        let d = (s["sourceX"].as_f64().unwrap() - s["x"].as_f64().unwrap())
            .hypot(s["sourceY"].as_f64().unwrap() - s["y"].as_f64().unwrap());
        assert!(d > 2.0 * s["radius"].as_f64().unwrap(), "{s}");
        assert!(
            (s["dust"]["x"].as_f64().unwrap() - 0.25).abs() < 0.004,
            "{s}"
        );
        assert!(
            (s["dust"]["y"].as_f64().unwrap() - 300.0 / 1400.0).abs() < 0.004,
            "{s}"
        );
    }

    /// Measurement harness (notes: benchmark-results/fd-find-dust-2026-10-31):
    /// runs the detector on the work copies `print_dust` cached
    /// (`STEM.work`: width, height, factor, base width, base height as
    /// little-endian u32, then the values as f32) in `$DUST_WORK` and writes
    /// STEM.json beside each with every candidate.
    /// `DUST_WORK=dir cargo test -p rawtonic-engine --lib dust::tests::work_copies -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement: needs DUST_WORK"]
    fn work_copies() {
        let dir = std::path::PathBuf::from(std::env::var("DUST_WORK").unwrap());
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "work"))
            .collect();
        entries.sort();
        for path in entries {
            let bytes = std::fs::read(&path).unwrap();
            let u = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
            let (w, h, factor, bw, bh) = (u(0) as usize, u(1) as usize, u(2) as usize, u(3), u(4));
            let values: Vec<f32> = bytes[20..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            assert_eq!(values.len(), w * h);
            let work = WorkCopy {
                values,
                width: w,
                height: h,
                factor,
            };
            let analysis = analyse_work(&work, (bw, bh), &[]);
            let candidates: Vec<Value> = analysis
                .candidates
                .iter()
                .map(|c| json!({"x": c.centre.0, "y": c.centre.1, "radius": c.radius, "contrast": c.contrast}))
                .collect();
            std::fs::write(
                path.with_extension("found.json"),
                serde_json::to_string(&json!({"candidates": candidates})).unwrap(),
            )
            .unwrap();
            println!(
                "{}: {} (≥50: {})",
                path.file_stem().unwrap().to_string_lossy(),
                analysis.candidates.len(),
                select(&analysis, 50.0).len()
            );
        }
    }

    #[test]
    fn unoriented_undoes_every_orientation() {
        for o in 1..=8u16 {
            let (w, h) = if (5..=8).contains(&o) {
                (400, 600)
            } else {
                (600, 400)
            };
            let (x, y) = (0.2, 0.7);
            let (u, v) = crate::lightroom_retouch::orient_point(o, x, y);
            let back = unoriented(o, w, h, (u * w as f64, v * h as f64));
            assert!(
                (back.0 - x).abs() < 1e-9 && (back.1 - y).abs() < 1e-9,
                "{o}: {back:?}"
            );
        }
    }

    #[test]
    fn synced_dust_lands_on_the_same_sensor_place_through_crop_and_turns() {
        // This photo: landscape (orientation 1), turned and cropped by the
        // user; a dust spot at base (900, 500) of a 2000 × 1400 base.
        let base_point = (900.0, 500.0);
        let here = json!({
            "orientationSteps": 1, "rotation": 3.0,
            "crop": {"x": 100.0, "y": 50.0, "width": 1000.0, "height": 1500.0},
        });
        let frame = crate::spot_removal::mask_frame(&here, 2000, 1400);
        let at = crate::red_eye::to_mask(&here, frame, base_point).unwrap();
        let src = crate::red_eye::to_mask(&here, frame, (900.0, 300.0)).unwrap();
        let radius =
            crate::lightroom_retouch::mask_radius(&here, frame, at, base_point, 15.0).unwrap();
        let recipe = json!({
            "orientationSteps": 1, "rotation": 3.0,
            "crop": here["crop"],
            "spots": [
                {"id": "a", "mode": "heal", "x": at.0, "y": at.1, "radius": radius,
                 "sourceX": src.0, "sourceY": src.1, "feather": 10.0, "opacity": 100.0,
                 "visible": true, "dust": {"x": 0.45, "y": 0.357}},
                // A manual spot is not dust: not synced.
                {"id": "b", "mode": "heal", "x": 10.0, "y": 10.0, "radius": 5.0,
                 "sourceX": 30.0, "sourceY": 10.0, "visible": true},
            ],
        });
        let items = sync_items(&recipe, 2000, 1400, 1);
        assert_eq!(items.len(), 1, "{items:?}");

        // Another photo of the camera held upright (EXIF 6: decoded
        // 1400 × 2000), straightened by the user.
        let mut other = json!({"rotation": -2.0, "spots": []});
        let mut n = 0;
        let mut ids = || {
            n += 1;
            format!("id{n}")
        };
        assert_eq!(add_synced(&mut other, &items, 6, &mut ids), 1);
        // Again: nothing twice.
        assert_eq!(add_synced(&mut other, &items, 6, &mut ids), 0);
        assert!(crate::lightroom_retouch::place(
            &mut other, 1400, 2000, None
        ));
        let placed = &other["spots"][0];
        let frame = crate::spot_removal::mask_frame(&other, 1400, 2000);
        let got = crate::spot_removal::to_base(
            &other,
            frame,
            placed["x"].as_f64().unwrap(),
            placed["y"].as_f64().unwrap(),
        );
        // EXIF 6 turns the raw's (x, y) into (1 − y, x) of the decode.
        let expected = (
            (1.0 - base_point.1 / 1400.0) * 1400.0,
            base_point.0 / 2000.0 * 2000.0,
        );
        assert!(
            (got.0 - expected.0).abs() < 0.5 && (got.1 - expected.1).abs() < 0.5,
            "{got:?} vs {expected:?}"
        );
        // Its size in sensor pixels is kept.
        let edge = crate::spot_removal::to_base(
            &other,
            frame,
            placed["x"].as_f64().unwrap() + placed["radius"].as_f64().unwrap(),
            placed["y"].as_f64().unwrap(),
        );
        assert!(((edge.0 - got.0).hypot(edge.1 - got.1) - 15.0).abs() < 0.2);
        assert_eq!(placed["dust"], items[0]["dust"]);
        assert_eq!(placed["feather"], 10.0);
    }
}
