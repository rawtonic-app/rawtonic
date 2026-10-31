//! Find Dust: sensor dust found on the photo's base image and placed as
//! ordinary heal spots (`spot_removal`), Lightroom's Dust > Apply flow
//! with our own detector.
//!
//! Dust on the sensor shows as a soft, round, slightly darker disc that is
//! only visible on flat areas (sky, studio backdrops). The detector works on
//! a reduced copy of the base image (the photo as decoded, so positions are
//! sensor positions): square root of the linear luminance, where photon
//! noise is about even across brightness. At several sizes it compares the
//! mean of a box with the two middle sides of the ring around it (a smooth
//! gradient cancels, and the edge of a cloud or a branch on one side does
//! not answer); the strongest size per pixel and the local maxima over the
//! photo are the candidates. Each is judged against a plane fitted robustly
//! to its wider surroundings (other dust, a cloud's edge or a branch in a
//! part of them are left out of the fit): the surroundings must be flat
//! (their robust spread at most a share of the darkening), the spot round
//! (darker in each quarter), its rim even (no line or edge crossing it) and
//! its shadow soft. Specks around it that are not dust (darker ones that
//! fail these tests, lighter ones: grain and mottled surfaces have both)
//! may be few, while dust among other dust is kept. Very dark or bright
//! surroundings (dust invisible there, clipped values), strong contrast
//! (objects, not dust) and every face box the caller gives are left out.
//! The heal circle reaches as far as the shadow's soft rim, measured.
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
/// Inner half-sizes (work pixels) of the box sizes searched: up to discs
/// of about 1.5 % of the long edge (dust far from focus, wide apertures).
const SCALES: [usize; 10] = [1, 2, 3, 4, 6, 8, 11, 15, 20, 27];
/// Relative darkening (in square-root space) needed at sensitivity 50; the
/// slider scales it as Visualize Spots scales its threshold.
const CONTRAST_AT_50: f32 = 0.006;
/// Smallest half-size of a dust shadow's box, as a share of the long edge
/// (about 4 sensor pixels at 6000).
const MIN_HALF_SIZE: f64 = 0.0007;
/// At most this many spots per photo.
const MAX_SPOTS: usize = 400;
/// Heal circle radius over the reach of the shadow (`shadow_reach`).
const HEAL_MARGIN: f64 = 1.2;
/// Weight of a heal source's uneven inside in its score
/// (`spot_removal::best_source_with`).
const SOURCE_UNEVEN: f64 = 16.0;

/// The detector's gates (`Gates::DEFAULT`; the measurement harness tries
/// others on the development set only).
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Gates {
    /// Darker than this share is an object (a bird, a letter), not dust.
    max_contrast: f32,
    /// Minimum significance over photon noise.
    min_significance: f32,
    /// Texture of the wider surroundings (their robust spread around a
    /// plane, photon noise taken out) at most this share of the spot's
    /// darkening …
    texture_per_contrast: f32,
    /// … and at most this share of their brightness.
    max_texture: f32,
    /// The lightest and the darkest of the rim's eight sectors (the ring
    /// just outside the spot, against the plane) may differ by at most this
    /// share of the darkening: a line or an edge crossing the spot makes
    /// one side darker or lighter, a dust shadow does not.
    rim_spread: f32,
    /// Each quarter of the spot is at least this share of its darkening
    /// darker than the plane: a dust shadow is round, the end of a branch
    /// or the corner of a shape is not.
    min_quarter: f32,
    /// Surroundings brightness range (square root of linear luminance).
    min_level: f32,
    max_level: f32,
    /// Isolation: specks of at least `neighbour_share` of a spot's contrast
    /// within `isolation_reach` × (size + 2) work pixels that are not dust
    /// (darker ones that fail the dust test, lighter ones not larger than
    /// the spot) may be at most `max_neighbours`: dust may lie in dense
    /// groups, grain and mottled surfaces have such specks everywhere.
    neighbour_share: f32,
    isolation_reach: usize,
    max_neighbours: usize,
    /// A spot's darkest pixel may be at most this many times as dark as its
    /// box: dust shadows are soft.
    max_peak: f32,
}

impl Gates {
    const DEFAULT: Gates = Gates {
        max_contrast: 0.18,
        min_significance: 5.0,
        texture_per_contrast: 0.3,
        max_texture: 0.012,
        rim_spread: 0.6,
        min_quarter: 0.5,
        min_level: 0.12,
        max_level: 0.97,
        neighbour_share: 0.5,
        isolation_reach: 8,
        max_neighbours: 8,
        max_peak: 2.5,
    };
}

impl Default for Gates {
    fn default() -> Self {
        Self::DEFAULT
    }
}

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
    analyse_with(work, size, faces, &Gates::DEFAULT, None)
}

/// `analyse_work` with `gates`; `dump` (the measurement harness) collects
/// every candidate's measures before any gate.
fn analyse_with(
    work: &WorkCopy,
    size: (u32, u32),
    faces: &[[f64; 4]],
    gates: &Gates,
    dump: Option<&std::sync::Mutex<Vec<Value>>>,
) -> DustAnalysis {
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

    // Per pixel: the size with the strongest significant darkening, and
    // the same for brightening (specks lighter than their ring: textures
    // and noise have them as often as darker ones, dust never).
    let best: Vec<[(f32, u8); 2]> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let integral = &integral;
            (0..w).map(move |x| {
                let (x, y) = (x as isize, y as isize);
                let mut found = [(0.0f32, 0u8); 2];
                for (k, &a) in SCALES.iter().enumerate() {
                    if a < min_half {
                        continue;
                    }
                    let a = a as isize;
                    let inner = integral.square(x, y, a);
                    let around = ring(integral, x, y, a, 2 * a + 1);
                    let (mi, mr) = (mean(inner), mean(around));
                    if mr < gates.min_level as f64 {
                        continue;
                    }
                    // Against the ring's four sides without the lightest and
                    // the darkest: a smooth gradient cancels, and the edge of
                    // a bright cloud or a dark branch on one side does not
                    // make the pixels beside it a speck.
                    let m = 2 * a + 1;
                    let mut sides = [
                        integral.boxed(x - m, y - m, x - a - 1, y + m),
                        integral.boxed(x + a + 1, y - m, x + m, y + m),
                        integral.boxed(x - a, y - m, x + a, y - a - 1),
                        integral.boxed(x - a, y + a + 1, x + a, y + m),
                    ];
                    sides.sort_by(|p, q| mean(*p).total_cmp(&mean(*q)));
                    let middle = (sides[1].0 + sides[2].0, sides[1].2 + sides[2].2);
                    let d = (middle.0 / middle.1.max(1.0) - mi) as f32;
                    let spread = sigma * ((1.0 / inner.2 + 1.0 / middle.1.max(1.0)) as f32).sqrt();
                    let contrast = d.abs() / mr as f32;
                    let side = usize::from(d < 0.0);
                    if d.abs() > gates.min_significance * spread && contrast > found[side].0 {
                        found[side] = (contrast, k as u8);
                    }
                }
                found
            })
        })
        .collect();

    // Local maxima of the darkening and of the brightening (ties go to the
    // first in reading order), down to a share of the floor: the
    // neighbours the isolation test counts.
    let maxima = |side: usize| -> Vec<(usize, usize, f32, usize)> {
        (2..h - 2)
            .into_par_iter()
            .flat_map_iter(|y| {
                let best = &best;
                (2..w - 2).filter_map(move |x| {
                    let (c, k) = best[y * w + x][side];
                    if c < gates.neighbour_share * floor {
                        return None;
                    }
                    for dy in -1isize..=1 {
                        for dx in -1isize..=1 {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            let o = best[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
                                [side]
                                .0;
                            let earlier = dy < 0 || (dy == 0 && dx < 0);
                            if o > c || (earlier && o == c) {
                                return None;
                            }
                        }
                    }
                    Some((x, y, c, SCALES[k as usize]))
                })
            })
            .collect()
    };
    let (peaks, light) = (maxima(0), maxima(1));
    let cell = 32usize;
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let gridded = |specks: &[(usize, usize, f32, usize)]| {
        let mut grid: Vec<Vec<usize>> = vec![Vec::new(); gw * gh];
        for (i, p) in specks.iter().enumerate() {
            grid[(p.1 / cell) * gw + p.0 / cell].push(i);
        }
        grid
    };
    let (grid, light_grid) = (gridded(&peaks), gridded(&light));

    // Every darker speck judged as dust on its own (its surroundings, its
    // shape), before isolation: dust among other dust is not texture.
    let measured: Vec<Option<Measures>> = peaks
        .par_iter()
        .map(|&(x, y, c, a)| {
            // The box-against-ring darkening is a first sieve; the contrast
            // that counts is measured against the surroundings' plane (other
            // dust or an edge in the ring lowers or raises the first).
            if c < 0.5 * floor {
                return None;
            }
            measure(values, &integral, (x as isize, y as isize), a as isize, sigma)
        })
        .collect();
    let dusty: Vec<bool> = measured
        .iter()
        .map(|m| {
            m.is_some_and(|m| {
                m.passes(gates) && (floor..=gates.max_contrast).contains(&m.contrast())
            })
        })
        .collect();

    let mut candidates: Vec<(Candidate, f32)> = peaks
        .par_iter()
        .enumerate()
        .filter_map(|(i, &(x, y, c, a))| {
            let measures = measured[i]?;
            // Dust stands alone or among other dust on a flat area; mottled
            // or textured surfaces (floors, roofs, foliage, grain) have
            // specks around each one that are not dust: lighter ones, or
            // darker ones that fail the dust test.
            let count = |specks: &[(usize, usize, f32, usize)],
                         grid: &[Vec<usize>],
                         reach: usize,
                         counted: &dyn Fn(usize) -> bool| {
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
                            let (ox, oy, oc, _) = specks[j];
                            let d = (ox as f64 - x as f64).hypot(oy as f64 - y as f64);
                            if d > own
                                && d <= reach as f64
                                && oc >= gates.neighbour_share * c
                                && counted(j)
                            {
                                neighbours += 1;
                            }
                        }
                    }
                }
                neighbours
            };
            let all = |_: usize| true;
            let not_dust = |j: usize| !dusty[j];
            let small_dark = |j: usize| !dusty[j] && peaks[j].3 <= a;
            let small_light = |j: usize| light[j].3 <= a;
            if let Some(dump) = dump {
                let reaches = [4, 8, 12, 16, 24].map(|k| k * (a + 2));
                let n = |specks: &[(usize, usize, f32, usize)],
                         grid: &[Vec<usize>],
                         counted: &dyn Fn(usize) -> bool| {
                    reaches.map(|r| count(specks, grid, r, counted))
                };
                dump.lock().unwrap().push(json!({
                    "x": x, "y": y, "a": a, "c": c, "m": measures,
                    "n": n(&peaks, &grid, &all),
                    "nd": n(&peaks, &grid, &not_dust),
                    "nds": n(&peaks, &grid, &small_dark),
                    "nb": n(&light, &light_grid, &all),
                    "nbs": n(&light, &light_grid, &small_light),
                }));
            }
            let reach = gates.isolation_reach * (a + 2);
            let neighbours = count(&peaks, &grid, reach, &not_dust)
                + count(&light, &light_grid, reach, &small_light);
            if neighbours > gates.max_neighbours || !dusty[i] {
                return None;
            }
            let (contrast, darkening) = (measures.contrast(), measures.darkening);
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
            // The heal circle covers the disc and its soft rim: out to where
            // the darkening has faded, with a margin for the faint tail (a
            // heal circle that leaves part of the rim outside carries its
            // darkening into the healed disc).
            let reach = shadow_reach(values, (w, h), (x, y), a, measures.level);
            let radius = (HEAL_MARGIN * (reach + 1.0) * factor as f64).max(4.0);
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

/// How far (work pixels) the shadow of a spot of size `a` at `at` reaches
/// from its centre: the first ring (one pixel wide) whose mean is less than
/// a tenth of the centre's darkening below `level`, kept between the box
/// and three times its size. A ring's mean is the plane's value at the
/// centre, so a smooth gradient does not shift it.
fn shadow_reach(
    values: &[f32],
    (w, h): (usize, usize),
    (x, y): (usize, usize),
    a: usize,
    level: f32,
) -> f64 {
    let limit = 3 * a + 3;
    let mut rings = vec![(0.0f32, 0u32); limit + 1];
    let (x0, x1) = (x.saturating_sub(limit), (x + limit).min(w - 1));
    let (y0, y1) = (y.saturating_sub(limit), (y + limit).min(h - 1));
    for yy in y0..=y1 {
        for xx in x0..=x1 {
            let d = (xx as f64 - x as f64).hypot(yy as f64 - y as f64).round() as usize;
            if d <= limit {
                rings[d].0 += values[yy * w + xx];
                rings[d].1 += 1;
            }
        }
    }
    let dark = |k: usize| level - rings[k].0 / rings[k].1.max(1) as f32;
    let centre = (0..=a.div_ceil(2)).map(dark).sum::<f32>() / (a.div_ceil(2) + 1) as f32;
    (a.max(1)..=limit)
        .find(|&k| rings[k].1 > 0 && dark(k) < 0.1 * centre)
        .unwrap_or(limit) as f64
}

/// Least-squares plane v = gx·dx + gy·dy + c through `samples` (dx, dy, v).
fn plane(samples: &[(f32, f32, f32)]) -> Option<(f64, f64, f64)> {
    let (mut sxx, mut sxy, mut syy, mut sx, mut sy, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut sxv, mut syv, mut sv) = (0.0, 0.0, 0.0);
    for &(dx, dy, v) in samples {
        let (dx, dy, v) = (f64::from(dx), f64::from(dy), f64::from(v));
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
        sx += dx;
        sy += dy;
        n += 1.0;
        sxv += dx * v;
        syv += dy * v;
        sv += v;
    }
    // Normal equations [[sxx sxy sx] [sxy syy sy] [sx sy n]] · (gx gy c) = (sxv syv sv).
    let det = sxx * (syy * n - sy * sy) - sxy * (sxy * n - sy * sx) + sx * (sxy * sy - syy * sx);
    if det.abs() < 1e-9 {
        return None;
    }
    let solve = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
        a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0])
    };
    let (r0, r1, r2) = ([sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n]);
    let rhs = [sxv, syv, sv];
    let col = |k: usize| {
        let mut m = [r0, r1, r2];
        for (row, value) in m.iter_mut().zip(rhs) {
            row[k] = value;
        }
        solve(m[0], m[1], m[2]) / det
    };
    Some((col(0), col(1), col(2)))
}

/// Median of `v` (reordered).
fn median(v: &mut [f32]) -> f32 {
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// What the gates judge of a candidate spot at `at` of size `a`, all in
/// square-root luminance against its surroundings: a plane fitted robustly
/// to the wider ring (other dust, a cloud's edge or a branch in part of it
/// are left out of the fit and of the texture), so dust among other dust or
/// near an edge is judged by what is around it.
#[derive(Debug, Clone, Copy, Serialize)]
struct Measures {
    /// The plane at the spot (the surroundings' brightness).
    level: f32,
    /// Plane minus the spot's box mean.
    darkening: f32,
    /// Robust spread of the wider ring around the plane, photon noise out.
    texture: f32,
    /// Lightest minus darkest of the rim's eight sectors.
    rim: f32,
    /// The least dark of the spot's four quarters (darkening, positive).
    quarter: f32,
    /// The darkest pixel of the box below the plane (positive).
    peak: f32,
    /// Share of the wider ring the plane fit left out (not plane-like).
    outliers: f32,
    /// Standard deviation of the whole wider ring around the plane, photon
    /// noise out (what the robust spread ignores).
    rough: f32,
}

impl Measures {
    fn contrast(&self) -> f32 {
        self.darkening / self.level
    }

    fn passes(&self, gates: &Gates) -> bool {
        let d = self.darkening;
        (gates.min_level..=gates.max_level).contains(&self.level)
            && d > 0.0
            && self.texture <= gates.texture_per_contrast * d
            && self.texture <= gates.max_texture * self.level
            && self.rim <= gates.rim_spread * d
            && self.quarter >= gates.min_quarter * d
            && self.peak <= gates.max_peak * d
    }
}

fn measure(
    values: &[f32],
    i: &Integral,
    at: (isize, isize),
    a: isize,
    sigma: f32,
) -> Option<Measures> {
    let (x, y) = at;
    let (w, h) = (i.w as isize, i.h as isize);
    let value = |xx: isize, yy: isize| {
        (xx >= 0 && yy >= 0 && xx < w && yy < h).then(|| values[(yy * w + xx) as usize])
    };
    // The wider surroundings, outside the spot and its rim, sampled on a
    // grid of about 2500 points at most.
    let m = 2 * a + 1;
    let b = (5 * a + 3).max(m + 4);
    let stride = ((2 * b + 1) / 50).max(1);
    let mut wide: Vec<(f32, f32, f32)> = Vec::new();
    let mut yy = -b;
    while yy <= b {
        let mut xx = -b;
        while xx <= b {
            if xx.abs().max(yy.abs()) > m
                && let Some(v) = value(x + xx, y + yy)
            {
                wide.push((xx as f32, yy as f32, v));
            }
            xx += stride;
        }
        yy += stride;
    }
    if wide.len() < 16 {
        return None;
    }
    let residuals = |p: (f64, f64, f64), s: &[(f32, f32, f32)]| -> Vec<f32> {
        s.iter()
            .map(|&(dx, dy, v)| v - (p.0 * f64::from(dx) + p.1 * f64::from(dy) + p.2) as f32)
            .collect()
    };
    let spread = |r: &[f32]| -> (f32, f32) {
        let mut r = r.to_vec();
        let mid = median(&mut r);
        let mut dev: Vec<f32> = r.iter().map(|v| (v - mid).abs()).collect();
        (mid, 1.4826 * median(&mut dev))
    };
    // Fit, leave out what does not belong to the plane, fit again.
    let first = plane(&wide)?;
    let (mid, s1) = spread(&residuals(first, &wide));
    let limit = 3.0 * s1.max(sigma);
    let kept: Vec<(f32, f32, f32)> = wide
        .iter()
        .zip(residuals(first, &wide))
        .filter(|(_, r)| (r - mid).abs() <= limit)
        .map(|(s, _)| *s)
        .collect();
    let outliers = 1.0 - kept.len() as f32 / wide.len() as f32;
    let fit = if kept.len() >= 16 { plane(&kept)? } else { first };
    let all = residuals(fit, &wide);
    let (_, s) = spread(&all);
    let variance = all.iter().map(|r| r * r).sum::<f32>() / all.len() as f32;
    let rough = (variance - sigma * sigma).max(0.0).sqrt();
    let level = fit.2 as f32;
    let darkening = level - mean(i.square(x, y, a)) as f32;
    if darkening <= 0.0 || level <= 0.0 {
        return None;
    }
    let texture = (s * s - sigma * sigma).max(0.0).sqrt();
    let at_plane = |dx: isize, dy: isize| (fit.0 * dx as f64 + fit.1 * dy as f64 + fit.2) as f32;
    // The rim in eight sectors and the spot in four quarters, against the
    // plane; and the darkest pixel (a speck in focus is far darker in its
    // middle than the box: dirt, a nail head).
    let mut sectors = [(0.0f32, 0u32); 8];
    let mut quarters = [(0.0f32, 0u32); 4];
    let mut darkest = f32::INFINITY;
    for dy in -m..=m {
        for dx in -m..=m {
            let Some(v) = value(x + dx, y + dy) else {
                continue;
            };
            let r = v - at_plane(dx, dy);
            if dx.abs().max(dy.abs()) > a {
                let angle = (dy as f32).atan2(dx as f32);
                let k = (((angle / std::f32::consts::TAU + 1.0) * 8.0) as usize) % 8;
                sectors[k].0 += r;
                sectors[k].1 += 1;
            } else {
                darkest = darkest.min(r);
                if dx != 0 && dy != 0 {
                    let q = usize::from(dx > 0) + 2 * usize::from(dy > 0);
                    quarters[q].0 += r;
                    quarters[q].1 += 1;
                }
            }
        }
    }
    let means = |parts: &[(f32, u32)]| -> Vec<f32> {
        parts
            .iter()
            .filter(|p| p.1 > 0)
            .map(|p| p.0 / p.1 as f32)
            .collect()
    };
    let rim = means(&sectors);
    let (lo, hi) = rim
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(l, h), &v| {
            (l.min(v), h.max(v))
        });
    if rim.len() < 5 {
        return None;
    }
    // Quarters exist from a = 1 on (the centre row and column are left out).
    let quarter = means(&quarters)
        .iter()
        .fold(f32::INFINITY, |q, &v| q.min(-v));
    Some(Measures {
        level,
        darkening,
        texture,
        rim: hi - lo,
        quarter,
        peak: -darkest,
        outliers,
        rough,
    })
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
/// and a later run use to recognise the same dust. A source stays clear
/// of `analysis`'s candidates (other dust, down to the most sensitive
/// setting) where it can and prefers an even inside. Spots without a
/// source (no room in the photo) are left out.
pub fn heal_spots(
    base: &DynamicImage,
    adjustments: &Value,
    analysis: &DustAnalysis,
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
            let avoid: Vec<((f64, f64), f64)> = analysis
                .candidates
                .iter()
                .filter(|o| o.centre != c.centre)
                .map(|o| (o.centre, o.radius))
                .collect();
            // Among very dense dust no source may stay clear: then the
            // nearest dust is allowed rather than leaving the spot out.
            let source = crate::spot_removal::best_source_with(
                &image,
                c.centre,
                c.radius,
                &avoid,
                SOURCE_UNEVEN,
            )
            .or_else(|| {
                crate::spot_removal::best_source_with(&image, c.centre, c.radius, &[], SOURCE_UNEVEN)
            })?;
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
    fn finds_dust_among_other_dust_and_beside_clouds() {
        // A dense group of dust (each disc has six others within ten radii)
        // and two discs beside a bright cloud: the old flatness and
        // isolation tests left all of them out.
        let mut spots: Vec<(f32, f32, f32, f32)> = (0..8)
            .map(|k| {
                let (col, row) = ((k % 4) as f32, (k / 4) as f32);
                (600.0 + 70.0 * col, 400.0 + 70.0 * row, 12.0, 0.06)
            })
            .collect();
        spots.push((2050.0, 1050.0, 12.0, 0.06));
        spots.push((2000.0, 1400.0, 12.0, 0.06));
        let image = photo(3000, 2000, &spots, |x, y, v| {
            // A soft bright cloud left of the two discs, 2.5 radii away.
            let d = ((x as f32 - 2350.0) / 250.0).hypot((y as f32 - 1230.0) / 150.0);
            v * (1.0 + 0.6 * ((1.4 - d) / 0.4).clamp(0.0, 1.0))
        });
        let found = select(&analyse(&image, false, &[]), 50.0);
        for &(x, y, ..) in &spots {
            assert!(
                near(&found, x as f64, y as f64, 15.0),
                "missed ({x}, {y}): {found:?}"
            );
        }
        assert_eq!(found.len(), spots.len(), "{found:?}");
    }

    #[test]
    fn leaves_out_speckled_texture() {
        // Grain-like specks, darker and lighter ones alike (a rough wall, a
        // noisy floor), at dust's size and contrast; one real dust disc.
        let speck = |x: u32, y: u32| -> f32 {
            let (cx, cy) = (x / 16, y / 16);
            let h = (cx.wrapping_mul(2_654_435_761) ^ cy.wrapping_mul(40_503)).wrapping_mul(97);
            let (ox, oy) = ((h % 5) as f32 + 6.0, ((h >> 8) % 5) as f32 + 6.0);
            let d = ((x % 16) as f32 - ox).hypot((y % 16) as f32 - oy) / 4.0;
            let sign = if (h >> 16) % 2 == 0 { -1.0 } else { 1.0 };
            1.0 + sign * 0.07 * ((1.3 - d) / 0.6).clamp(0.0, 1.0)
        };
        let image = photo(3000, 2000, &[(400.0, 300.0, 12.0, 0.06)], |x, y, v| {
            if x > 1200 && y > 600 {
                v * speck(x, y)
            } else {
                v
            }
        });
        let found = select(&analyse(&image, false, &[]), 50.0);
        assert!(near(&found, 400.0, 300.0, 15.0), "{found:?}");
        // Nothing inside the texture (a speck on its border, with flat sky
        // on one side, may look like dust).
        assert!(
            found
                .iter()
                .all(|c| c.centre.0 < 1250.0 || c.centre.1 < 650.0),
            "{found:?}"
        );
    }

    #[test]
    fn heal_circles_cover_the_rim_and_sources_avoid_other_dust() {
        // Dust is darker out to 1.3 radii (`dust`); a heal circle that
        // stops short carries the rim's darkening into the healed disc.
        let spots = [
            (800.0, 600.0, 14.0, 0.06),
            (860.0, 600.0, 14.0, 0.06),
            (800.0, 660.0, 14.0, 0.06),
            (1800.0, 1200.0, 20.0, 0.05),
        ];
        let image = photo(2600, 1800, &spots, |_, _, v| v);
        let analysis = analyse(&image, false, &[]);
        let found = placed(&analysis, 50.0, &json!({}));
        assert_eq!(found.len(), spots.len(), "{found:?}");
        for f in &found {
            let (_, _, r, _) = spots
                .iter()
                .find(|s| (s.0 as f64 - f.x).hypot(s.1 as f64 - f.y) < 15.0)
                .unwrap();
            assert!(
                f.radius >= 1.3 * *r as f64 && f.radius <= 3.0 * *r as f64,
                "{f:?}"
            );
        }
        for s in heal_spots(&image, &json!({}), &analysis, &found, 1) {
            let n = |k: &str| s[k].as_f64().unwrap();
            let (sx, sy, r) = (n("sourceX"), n("sourceY"), n("radius"));
            for &(x, y, dr, _) in &spots {
                let d = (sx - x as f64).hypot(sy - y as f64);
                assert!(d >= r + 1.3 * dr as f64, "source {s} on dust ({x}, {y})");
            }
        }
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
        let spots = heal_spots(&image, &adjustments, &analysis, &found, 1);
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
    /// STEM.found.json beside each with every candidate (`$DUST_TAG`:
    /// STEM.TAG.json). `$DUST_GATES`: JSON overriding gates (camelCase
    /// names of `Gates`), for trying them on the development set.
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
        let gates: Gates = std::env::var("DUST_GATES")
            .ok()
            .map(|g| serde_json::from_str(&g).unwrap())
            .unwrap_or_default();
        let tag = std::env::var("DUST_TAG").unwrap_or_else(|_| "found".into());
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
            let dump = std::sync::Mutex::new(Vec::new());
            let dumping = std::env::var("DUST_DUMP").is_ok();
            let analysis = analyse_with(&work, (bw, bh), &[], &gates, dumping.then_some(&dump));
            if dumping {
                let integral = Integral::new(&work.values, w, h);
                std::fs::write(
                    path.with_extension("measures.json"),
                    serde_json::to_string(&json!({
                        "sigma": noise(&work.values, w, h, &integral),
                        "factor": factor, "w": w, "h": h, "bw": bw, "bh": bh,
                        "peaks": dump.into_inner().unwrap(),
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            let candidates: Vec<Value> = analysis
                .candidates
                .iter()
                .map(|c| json!({"x": c.centre.0, "y": c.centre.1, "radius": c.radius, "contrast": c.contrast}))
                .collect();
            std::fs::write(
                path.with_extension(format!("{tag}.json")),
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

    /// Measurement harness (notes: benchmark-results/fd2-dust-recall-2026-11-01):
    /// finds and heals the dust of each STEM.base in `$DUST_HEAL` (written by
    /// the app's `print_dust` with `$DUST_BASE`) as Find dust's Apply does
    /// (feather 0, opacity 100) and writes STEM.heal.json: per spot the
    /// residual darkening before and after, (ring − disc) / ring of the
    /// sRGB-encoded mean in % (disc within 0.6 radius, ring 1.3–2.2 radii);
    /// with `$DUST_CLEAN` (the same photos without the painted discs) also the
    /// mean deviation from the clean photo inside the spot, in % of its ring.
    #[test]
    #[ignore = "measurement: needs DUST_HEAL"]
    fn heal_bases() {
        let dir = std::path::PathBuf::from(std::env::var("DUST_HEAL").unwrap());
        let clean_dir = std::env::var("DUST_CLEAN").ok().map(std::path::PathBuf::from);
        let load = |path: &std::path::Path| -> Option<DynamicImage> {
            let bytes = std::fs::read(path).ok()?;
            let u = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
            let values: Vec<f32> = bytes[8..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            Some(DynamicImage::ImageRgb32F(
                Rgb32FImage::from_raw(u(0), u(1), values).unwrap(),
            ))
        };
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "base"))
            .collect();
        entries.sort();
        let encode = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
        };
        for path in entries {
            let image = load(&path).unwrap();
            let clean = clean_dir
                .as_ref()
                .and_then(|d| load(&d.join(path.file_name().unwrap())))
                .map(|c| c.to_rgb32f());
            let analysis = analyse(&image, false, &[]);
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            // `$DUST_TARGETS`: {stem: [{x, y, r}]} heals these instead (r 0:
            // the radius of the candidate found there, else of the target).
            let targets: Option<Vec<FoundSpot>> = std::env::var("DUST_TARGETS").ok().map(|t| {
                let t: Value = serde_json::from_str(&std::fs::read_to_string(t).unwrap()).unwrap();
                t[&stem]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        let (x, y, r) = (
                            p["x"].as_f64().unwrap(),
                            p["y"].as_f64().unwrap(),
                            p["r"].as_f64().unwrap(),
                        );
                        let near = analysis.candidates.iter().find(|c| {
                            (c.centre.0 - x).hypot(c.centre.1 - y) < 1.2 * c.radius.max(r) + 4.0
                        });
                        let radius = if p["fixed"].as_bool() == Some(true) {
                            r
                        } else {
                            near.map_or(r, |c| c.radius)
                        };
                        let base = Candidate {
                            centre: near.map_or((x, y), |c| c.centre),
                            radius,
                            contrast: 0.0,
                        };
                        FoundSpot {
                            x: base.centre.0,
                            y: base.centre.1,
                            radius,
                            base,
                        }
                    })
                    .collect()
            });
            let mut found = targets.unwrap_or_else(|| placed(&analysis, 50.0, &json!({})));
            if let Some(k) = std::env::var("DUST_RADIUS").ok().and_then(|k| k.parse::<f64>().ok()) {
                for f in &mut found {
                    f.radius *= k;
                    f.base.radius *= k;
                }
            }
            // `$DUST_PLAIN_SOURCE`: the sources as before FD2 (the manual
            // spot's search), for the A/B.
            // `$DUST_SOURCE` = "avoid,uneven" (e.g. "0,0"): the source search
            // with these settings, for trying them on the development set.
            let variant: Option<(bool, f64)> = std::env::var("DUST_SOURCE").ok().map(|v| {
                let (a, u) = v.split_once(',').unwrap();
                (a == "1", u.parse().unwrap())
            });
            let healing = if let Some((avoiding, uneven)) = variant {
                let rgb = image.to_rgb32f();
                found
                    .iter()
                    .filter_map(|f| {
                        let avoid: Vec<((f64, f64), f64)> = analysis
                            .candidates
                            .iter()
                            .filter(|o| avoiding && o.centre != f.base.centre)
                            .map(|o| (o.centre, o.radius))
                            .collect();
                        let s = crate::spot_removal::best_source_with(
                            &rgb,
                            f.base.centre,
                            f.radius,
                            &avoid,
                            uneven,
                        )?;
                        Some(json!({"x": f.x, "y": f.y, "radius": f.radius,
                                    "sourceX": s.0, "sourceY": s.1, "visible": true}))
                    })
                    .collect()
            } else if std::env::var("DUST_PLAIN_SOURCE").is_ok() {
                let rgb = image.to_rgb32f();
                found
                    .iter()
                    .filter_map(|f| {
                        let s = crate::spot_removal::best_source(&rgb, f.base.centre, f.radius)?;
                        Some(json!({"x": f.x, "y": f.y, "radius": f.radius,
                                    "sourceX": s.0, "sourceY": s.1, "visible": true}))
                    })
                    .collect()
            } else {
                heal_spots(&image, &json!({}), &analysis, &found, 1)
            };
            let spots: Vec<Value> = healing
                .into_iter()
                .enumerate()
                .map(|(k, mut s)| {
                    s["id"] = json!(k.to_string());
                    s["mode"] = json!("heal");
                    s["feather"] = json!(0.0);
                    s["opacity"] = json!(100.0);
                    s
                })
                .collect();
            let healed = crate::spot_removal::apply_spots(&image, &json!({"spots": spots}))
                .unwrap()
                .to_rgb32f();
            let original = image.to_rgb32f();
            let (w, h) = (original.width() as i64, original.height() as i64);
            let luma = |im: &Rgb32FImage, x: i64, y: i64| {
                let p = im.get_pixel(x as u32, y as u32);
                (encode(p[0]) + encode(p[1]) + encode(p[2])) / 3.0
            };
            let residual = |im: &Rgb32FImage, cx: f64, cy: f64, r: f64| {
                let (mut inner, mut ni, mut ring, mut nr) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                let reach = (2.2 * r).ceil() as i64;
                for y in (cy as i64 - reach).max(0)..(cy as i64 + reach).min(h) {
                    for x in (cx as i64 - reach).max(0)..(cx as i64 + reach).min(w) {
                        let d = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy);
                        if d <= 0.6 * r {
                            inner += f64::from(luma(im, x, y));
                            ni += 1.0;
                        } else if d > 1.3 * r && d < 2.2 * r {
                            ring += f64::from(luma(im, x, y));
                            nr += 1.0;
                        }
                    }
                }
                let (inner, ring) = (inner / ni.max(1.0), ring / nr.max(1.0));
                (ring - inner) / ring.max(1e-6) * 100.0
            };
            let rows: Vec<Value> = spots
                .iter()
                .map(|s| {
                    let n = |k: &str| s[k].as_f64().unwrap();
                    let (x, y, r) = (n("x"), n("y"), n("radius"));
                    let bias = clean.as_ref().map(|c| {
                        let (mut sum, mut count, mut ring, mut nr) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                        let reach = (2.2 * r).ceil() as i64;
                        for yy in (y as i64 - reach).max(0)..(y as i64 + reach).min(h) {
                            for xx in (x as i64 - reach).max(0)..(x as i64 + reach).min(w) {
                                let d = (xx as f64 + 0.5 - x).hypot(yy as f64 + 0.5 - y);
                                if d <= 1.5 * r {
                                    sum += f64::from(luma(&healed, xx, yy) - luma(c, xx, yy));
                                    count += 1.0;
                                } else if d > 1.5 * r && d < 2.2 * r {
                                    ring += f64::from(luma(c, xx, yy));
                                    nr += 1.0;
                                }
                            }
                        }
                        sum / count.max(1.0) / (ring / nr.max(1.0)).max(1e-6) * 100.0
                    });
                    let error = clean.as_ref().map(|c| {
                        let (mut sum, mut count, mut ring, mut nr) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                        let reach = (2.2 * r).ceil() as i64;
                        for yy in (y as i64 - reach).max(0)..(y as i64 + reach).min(h) {
                            for xx in (x as i64 - reach).max(0)..(x as i64 + reach).min(w) {
                                let d = (xx as f64 + 0.5 - x).hypot(yy as f64 + 0.5 - y);
                                if d <= r {
                                    sum += f64::from(
                                        (luma(&healed, xx, yy) - luma(c, xx, yy)).abs(),
                                    );
                                    count += 1.0;
                                } else if d > 1.3 * r && d < 2.2 * r {
                                    ring += f64::from(luma(c, xx, yy));
                                    nr += 1.0;
                                }
                            }
                        }
                        sum / count.max(1.0) / (ring / nr.max(1.0)).max(1e-6) * 100.0
                    });
                    json!({
                        "x": x, "y": y, "r": r, "sx": n("sourceX"), "sy": n("sourceY"),
                        "before": residual(&original, x, y, r),
                        "after": residual(&healed, x, y, r),
                        "error": error, "bias": bias,
                    })
                })
                .collect();
            std::fs::write(
                path.with_extension("heal.json"),
                serde_json::to_string(&rows).unwrap(),
            )
            .unwrap();
            std::fs::write(
                path.with_extension("spots.json"),
                serde_json::to_string(&json!({"spots": spots})).unwrap(),
            )
            .unwrap();
            println!(
                "{}: {} spots",
                path.file_stem().unwrap().to_string_lossy(),
                rows.len()
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
