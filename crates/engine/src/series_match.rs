//! Match a series: each photo's light normalised to reference photos.
//!
//! A series shot under changing light (a wedding moving from window light
//! to a tungsten-lit church, flash, evening) gets per photo the Exposure and
//! white balance that make it look lit like its reference, so one look can
//! then be synced across it (Copy/Sync settings without Exposure and White
//! Balance). It only normalises light; it never transfers colour from
//! another photo (notes: parity-gaps/G27-copy-sync.md).
//!
//! - Exposure: the shift that best overlaps the photo's log-luminance
//!   histogram with the reference's (the share of pixels both have in
//!   common is largest, so a new subject or framing moves only part of the
//!   mass and does not pull the match), and the shift that gives faces'
//!   skin the reference's lightness where both show faces.
//! - White balance: the reference's setting relative to its light, carried
//!   over to each photo's light (in mireds and tint): the light is the
//!   scene's estimated neutral (`white_balance_estimate`) and, where faces
//!   are seen, the colour of their skin (skin under the same light has much
//!   the same hue across people, unlike its lightness).
//! - Kept: frames of an exposure bracket, silhouettes against a bright
//!   background (also with a face in it) and, for white balance only,
//!   coloured (stage) light, each with its reason; a match with little
//!   evidence is proposed for review.
//! - Faces lead the exposure where they are seen: a subject's skin under
//!   the same light keeps its lightness while the background changes with
//!   framing (measured on a portrait series moving between sunlit walls and
//!   shade: the scene histogram missed the face by 1.5–3 EV). Skin
//!   lightness differs between people, though: a frame of other people than
//!   the reference's is matched on their skin too (notes:
//!   parity-gaps/G27-copy-sync.md).
//!
//! Every photo gets a confidence (0..1). Pure maths on per-photo statistics;
//! decoding and face detection are the caller's.
use serde::{Deserialize, Serialize};

/// Histogram range of linear luminance, in EV (1.0 = the decode's white).
const HIST_MIN_EV: f32 = -16.0;
const HIST_MAX_EV: f32 = 0.0;
const BINS_PER_EV: f32 = 8.0;
pub const HIST_BINS: usize = ((HIST_MAX_EV - HIST_MIN_EV) * BINS_PER_EV) as usize;
/// Long edge of the image the statistics are made from.
pub const ANALYSIS_SIZE: u32 = 512;
/// Linear value counted as clipped.
const CLIPPED: f32 = 0.98;
/// Histogram smoothing before matching (EV).
const SMOOTH_EV: f32 = 0.25;
/// Largest exposure change proposed (EV).
const MAX_CHANGE_EV: f32 = 4.0;
const EXPOSURE_RANGE: f32 = 5.0;
/// Least skin pixels (at `ANALYSIS_SIZE`) for a full-weight skin anchor.
const SKIN_PIXELS_FULL: f32 = 1500.0;
const SKIN_PIXELS_MIN: u32 = 40;
/// Skin and scene exposure anchors further apart than this (EV) need review.
const ANCHORS_DISAGREE_EV: f32 = 2.5;
/// Weight of a full skin anchor against a fully confident scene match.
const SKIN_EXPOSURE_WEIGHT: f32 = 9.0;
/// Exposure changes larger than this (EV) are only proposed for review: a
/// backlit face against the sun asked for +3.7 EV on the QA series.
const REVIEW_CHANGE_EV: f32 = 2.5;
/// Below this confidence a match is only proposed for review.
pub const REVIEW_BELOW: f32 = 0.4;
/// Frames this close (s) with exposures this far apart (EV) are a bracket.
const BRACKET_GAP_S: f64 = 2.0;
const BRACKET_SPREAD_EV: f32 = 0.9;
/// A light estimate outside these is not white light but coloured (stage)
/// light: tint (green/magenta) and colour temperature in Kelvin. Fluorescent
/// and LED casts within them are corrected.
const WHITE_LIGHT_TINT: f64 = 60.0;
const WHITE_LIGHT_K: (f64, f64) = (2000.0, 12000.0);
/// Skin whose light's tint differs this much from the reference skin's is
/// lit by coloured light (a change in colour temperature is the ordinary
/// change from tungsten to daylight the match corrects).
const SKIN_LIGHT_TINT: f64 = 45.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalance {
    pub temperature: f64,
    pub tint: f64,
}

fn mired(kelvin: f64) -> f64 {
    1e6 / kelvin.max(1.0)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkinSample {
    /// Median log2 linear luminance of the skin pixels (decode units).
    pub log_luma: f32,
    /// Temperature/tint that would render the skin's mean colour neutral.
    pub light: Option<WhiteBalance>,
    pub pixels: u32,
    pub faces: u32,
}

/// What the match needs from one photo's linear decode.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PhotoStats {
    /// Share of the analysed pixels per 1/8 EV of log2 luminance from
    /// `HIST_MIN_EV`, unclipped pixels only.
    pub histogram: Vec<f32>,
    pub clipped_share: f32,
    /// Temperature/tint that render the scene's estimated neutral neutral.
    pub light: Option<WhiteBalance>,
    pub skin: Option<SkinSample>,
}

/// Capture facts from EXIF.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    /// Seconds (any epoch) of capture.
    pub time: Option<f64>,
    /// Camera exposure in EV: log2(N² / t) − log2(ISO / 100).
    pub exposure_value: Option<f32>,
    /// Exposure compensation set in the camera (EV).
    pub bias: Option<f32>,
    /// The camera marked the frame as part of an auto bracket.
    pub auto_bracket: bool,
    pub camera: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPhoto {
    pub path: String,
    pub stats: Option<PhotoStats>,
    pub capture: Capture,
    /// The base look's baseline exposure (EV before the tone curve).
    pub baseline_ev: f32,
    /// The recipe's Exposure.
    pub exposure: f32,
    /// The recipe's white balance (As Shot resolved), when known.
    pub white_balance: Option<WhiteBalance>,
    pub reference: bool,
    /// Why there are no statistics (decode failed), shown as the reason.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatchOptions {
    pub exposure: bool,
    pub white_balance: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Reference,
    /// Proposed and ticked.
    Match,
    /// Proposed, not ticked: little evidence or a possible intent.
    Review,
    /// Kept as it is.
    Keep,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// Part of an exposure bracket (merged later, not matched).
    Bracket,
    /// Dark subject against a bright background, left dark.
    Silhouette,
    /// Exposure compensation set in the camera for a darker or brighter
    /// picture than the match would make.
    CameraCompensation,
    /// Coloured (stage) light: white balance kept.
    ColouredLight,
    /// Little in common with the reference.
    LittleInCommon,
    /// Faces and scene call for different exposures.
    AnchorsDisagree,
    /// No white balance for this file (not a RAW or no camera calibration).
    NoWhiteBalance,
    /// A large exposure change, proposed for review.
    LargeChange,
    /// Already matches the reference.
    AlreadyMatches,
    /// Could not be analysed.
    Unreadable,
    /// No reference photo.
    NoReference,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Anchor {
    Scene,
    Skin,
    SkinAndScene,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub path: String,
    pub status: Status,
    pub reasons: Vec<Reason>,
    /// New Exposure, when it changes.
    pub exposure: Option<f32>,
    /// New white balance, when it changes.
    pub white_balance: Option<WhiteBalance>,
    pub confidence: f32,
    pub exposure_anchor: Option<Anchor>,
    pub white_balance_anchor: Option<Anchor>,
    /// The photo's Exposure and white balance now (for the result list).
    pub current_exposure: f32,
    pub current_white_balance: Option<WhiteBalance>,
}

// --- statistics -------------------------------------------------------------

fn bin_of(ev: f32) -> Option<usize> {
    let bin = ((ev - HIST_MIN_EV) * BINS_PER_EV).floor();
    (bin >= 0.0 && (bin as usize) < HIST_BINS).then_some(bin as usize)
}

fn luma(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// Face-skin probability (0..1) at a position (0..1 across the image), and
/// the number of faces.
pub type SkinMap<'a> = (&'a dyn Fn(f32, f32) -> f32, u32);

/// Statistics of a linear decode. `skin(x, y)` is the probability (0..1)
/// that the pixel at `x, y` (0..1 across the image) is face skin; `light`
/// turns a linear colour of this decode into the temperature/tint that
/// renders it neutral (`camera_profile::white_balance_for_sample`).
pub fn photo_stats(
    image: &image::DynamicImage,
    skin: Option<SkinMap<'_>>,
    light: &dyn Fn([f32; 3]) -> Option<WhiteBalance>,
) -> Option<PhotoStats> {
    let small = crate::image_processing::thumbnail(image, ANALYSIS_SIZE, ANALYSIS_SIZE).to_rgb32f();
    let (w, h) = (small.width() as f32, small.height() as f32);
    let mut histogram = vec![0f32; HIST_BINS];
    let (mut total, mut clipped) = (0usize, 0usize);
    let mut skin_lumas = Vec::new();
    let mut skin_sum = [0f64; 3];
    for (x, y, p) in small.enumerate_pixels() {
        let p = p.0;
        if !p.iter().all(|v| v.is_finite()) {
            continue;
        }
        total += 1;
        if p.iter().any(|v| *v >= CLIPPED) {
            clipped += 1;
            continue;
        }
        let l = luma(p).max(1e-7);
        if let Some(bin) = bin_of(l.log2()) {
            histogram[bin] += 1.0;
        }
        if let Some((skin, _)) = skin
            && l > 1e-4
            && skin((x as f32 + 0.5) / w, (y as f32 + 0.5) / h) >= 0.6
        {
            skin_lumas.push(l.log2());
            for c in 0..3 {
                skin_sum[c] += p[c].max(0.0) as f64;
            }
        }
    }
    if total < 64 {
        return None;
    }
    histogram.iter_mut().for_each(|v| *v /= total as f32);
    let neutral = crate::white_balance_estimate::neutral(image);
    let skin = skin.and_then(|(_, faces)| {
        if skin_lumas.len() < SKIN_PIXELS_MIN as usize {
            return None;
        }
        skin_lumas.sort_unstable_by(f32::total_cmp);
        let n = skin_lumas.len();
        let mean = skin_sum.map(|v| (v / n as f64) as f32);
        Some(SkinSample {
            log_luma: skin_lumas[n / 2],
            light: light(mean),
            pixels: n as u32,
            faces,
        })
    });
    Some(PhotoStats {
        histogram,
        clipped_share: clipped as f32 / total as f32,
        light: neutral.and_then(light),
        skin,
    })
}

// --- exposure -----------------------------------------------------------------

fn smoothed(histogram: &[f32]) -> Vec<f32> {
    let sigma = SMOOTH_EV * BINS_PER_EV;
    let radius = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    (0..histogram.len() as isize)
        .map(|i| {
            kernel
                .iter()
                .enumerate()
                .map(|(k, weight)| {
                    let j = i + k as isize - radius;
                    if (0..histogram.len() as isize).contains(&j) {
                        histogram[j as usize] * weight
                    } else {
                        0.0
                    }
                })
                .sum::<f32>()
                / sum
        })
        .collect()
}

/// Shared mass of `a` and of `b` shifted by `shift` bins (fractional).
fn overlap(a: &[f32], b: &[f32], shift: f32) -> f32 {
    let at = |i: f32| {
        let (lo, t) = (i.floor(), i - i.floor());
        let get = |j: f32| {
            if j >= 0.0 && (j as usize) < b.len() {
                b[j as usize]
            } else {
                0.0
            }
        };
        get(lo) * (1.0 - t) + get(lo + 1.0) * t
    };
    a.iter()
        .enumerate()
        .map(|(i, va)| va.min(at(i as f32 - shift)))
        .sum()
}

/// The EV to add to `photo` (histogram already in rendered EV via
/// `photo_shift`) so its histogram overlaps the reference's most, and the
/// confidence of that match.
fn histogram_match(
    reference: &[f32],
    ref_shift: f32,
    photo: &[f32],
    photo_shift: f32,
) -> (f32, f32) {
    let (a, b) = (smoothed(reference), smoothed(photo));
    let total = a.iter().sum::<f32>().min(b.iter().sum()).max(1e-6);
    // Shift in bins that brings the photo onto the reference.
    let base = (photo_shift - ref_shift) * BINS_PER_EV;
    let limit = MAX_CHANGE_EV * BINS_PER_EV;
    let step = 0.5;
    let scores: Vec<(f32, f32)> = (0..=((2.0 * limit / step) as usize))
        .map(|k| {
            let d = -limit + k as f32 * step;
            (d, overlap(&a, &b, base + d))
        })
        .collect();
    let (best_index, &(best_d, best)) = scores
        .iter()
        .enumerate()
        .max_by(|x, y| x.1.1.total_cmp(&y.1.1))
        .unwrap();
    // Parabolic refinement.
    let refined = if best_index > 0 && best_index + 1 < scores.len() {
        let (l, r) = (scores[best_index - 1].1, scores[best_index + 1].1);
        let denom = l - 2.0 * best + r;
        if denom.abs() > 1e-9 {
            best_d + step * (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            best_d
        }
    } else {
        best_d
    };
    // Distinct: the best alternative at least 1 EV away is clearly lower.
    let rival = scores
        .iter()
        .filter(|(d, _)| (d - best_d).abs() >= BINS_PER_EV)
        .map(|(_, s)| *s)
        .fold(0.0, f32::max);
    let shared = best / total;
    let distinct = if best > 0.0 {
        (1.0 - rival / best).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let confidence = (((shared - 0.25) / 0.5).clamp(0.0, 1.0)) * (0.5 + distinct).min(1.0);
    (refined / BINS_PER_EV, confidence)
}

/// Pixels of a skin sample as a weight 0..1.
fn skin_weight(skin: &SkinSample) -> f32 {
    (skin.pixels as f32 / SKIN_PIXELS_FULL).min(1.0)
}

struct Estimate {
    exposure_change: Option<f32>,
    exposure_anchor: Option<Anchor>,
    white_balance: Option<WhiteBalance>,
    white_balance_anchor: Option<Anchor>,
    confidence: f32,
    reasons: Vec<Reason>,
}

/// Rendered EV offset of a photo's decode (baseline + Exposure).
fn rendered_shift(photo: &SeriesPhoto) -> f32 {
    photo.baseline_ev + photo.exposure
}

fn estimate(reference: &SeriesPhoto, photo: &SeriesPhoto, options: MatchOptions) -> Estimate {
    let (Some(rs), Some(ps)) = (&reference.stats, &photo.stats) else {
        return Estimate {
            exposure_change: None,
            exposure_anchor: None,
            white_balance: None,
            white_balance_anchor: None,
            confidence: 0.0,
            reasons: vec![Reason::Unreadable],
        };
    };
    let mut reasons = Vec::new();
    let (mut exposure_change, mut exposure_anchor, mut confidence) = (None, None, 1.0f32);
    let both_skin = match (&rs.skin, &ps.skin) {
        (Some(a), Some(b)) => Some((a, b, skin_weight(a).min(skin_weight(b)))),
        _ => None,
    };
    if options.exposure {
        let (scene, scene_confidence) = histogram_match(
            &rs.histogram,
            rendered_shift(reference),
            &ps.histogram,
            rendered_shift(photo),
        );
        let skin = both_skin.map(|(a, b, weight)| {
            (
                (a.log_luma + rendered_shift(reference)) - (b.log_luma + rendered_shift(photo)),
                weight,
            )
        });
        let (change, anchor, c) = match skin {
            // Skin lightness differs between people, so faces weigh in
            // beside the scene but do not overrule it.
            Some((skin, weight)) => {
                let (ws, wf) = (scene_confidence.max(0.05), SKIN_EXPOSURE_WEIGHT * weight);
                let change = (scene * ws + skin * wf) / (ws + wf);
                let mut c = scene_confidence.max(weight);
                if (skin - scene).abs() > ANCHORS_DISAGREE_EV {
                    reasons.push(Reason::AnchorsDisagree);
                    c = c.min(REVIEW_BELOW - 0.05);
                }
                (change, Anchor::SkinAndScene, c)
            }
            None => (scene, Anchor::Scene, scene_confidence),
        };
        if c < REVIEW_BELOW && !reasons.contains(&Reason::AnchorsDisagree) {
            reasons.push(Reason::LittleInCommon);
        }
        exposure_change = Some(change.clamp(-MAX_CHANGE_EV, MAX_CHANGE_EV));
        exposure_anchor = Some(anchor);
        confidence = c;
    }
    let (mut white_balance, mut white_balance_anchor) = (None, None);
    if options.white_balance {
        match (reference.white_balance, photo.white_balance) {
            (Some(setting), Some(_)) => {
                let carried = |from: WhiteBalance, to: WhiteBalance| WhiteBalance {
                    temperature: mired(setting.temperature) - mired(from.temperature)
                        + mired(to.temperature),
                    tint: setting.tint - from.tint + to.tint,
                };
                let scene = rs.light.zip(ps.light).map(|(a, b)| carried(a, b));
                let skin = both_skin.and_then(|(a, b, weight)| {
                    a.light.zip(b.light).map(|(a, b)| (carried(a, b), weight))
                });
                let not_white = |l: &WhiteBalance| {
                    l.tint.abs() > WHITE_LIGHT_TINT
                        || !(WHITE_LIGHT_K.0..=WHITE_LIGHT_K.1).contains(&l.temperature)
                };
                let skin_tinted = both_skin
                    .and_then(|(a, b, _)| a.light.zip(b.light))
                    .is_some_and(|(a, b)| (a.tint - b.tint).abs() > SKIN_LIGHT_TINT);
                let coloured = ps.light.as_ref().is_some_and(not_white) || skin_tinted;
                let combined = match (scene, skin) {
                    _ if coloured => None,
                    (Some(scene), Some((skin, weight))) => {
                        let wf = 0.75 * weight as f64;
                        Some((
                            WhiteBalance {
                                temperature: scene.temperature * (1.0 - wf) + skin.temperature * wf,
                                tint: scene.tint * (1.0 - wf) + skin.tint * wf,
                            },
                            Anchor::SkinAndScene,
                        ))
                    }
                    (None, Some((skin, _))) => Some((skin, Anchor::Skin)),
                    (Some(scene), None) => Some((scene, Anchor::Scene)),
                    (None, None) => None,
                };
                if coloured {
                    reasons.push(Reason::ColouredLight);
                }
                if let Some((m, anchor)) = combined {
                    white_balance = Some(WhiteBalance {
                        temperature: (1e6 / m.temperature.max(1e6 / 50_000.0))
                            .clamp(2000.0, 50_000.0),
                        tint: m.tint.clamp(-150.0, 150.0),
                    });
                    white_balance_anchor = Some(anchor);
                } else if !coloured {
                    reasons.push(Reason::NoWhiteBalance);
                }
            }
            _ => reasons.push(Reason::NoWhiteBalance),
        }
    }
    Estimate {
        exposure_change,
        exposure_anchor,
        white_balance,
        white_balance_anchor,
        confidence,
        reasons,
    }
}

// --- intent -------------------------------------------------------------------

/// Indices of frames that belong to an exposure bracket.
fn brackets(photos: &[SeriesPhoto]) -> Vec<bool> {
    let mut is_bracket: Vec<bool> = photos.iter().map(|p| p.capture.auto_bracket).collect();
    let mut order: Vec<usize> = (0..photos.len())
        .filter(|&i| photos[i].capture.time.is_some())
        .collect();
    order.sort_by(|&a, &b| {
        (&photos[a].capture.camera, photos[a].capture.time.unwrap())
            .partial_cmp(&(&photos[b].capture.camera, photos[b].capture.time.unwrap()))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() {
            let (prev, next) = (&photos[order[end - 1]].capture, &photos[order[end]].capture);
            if prev.camera != next.camera || next.time.unwrap() - prev.time.unwrap() > BRACKET_GAP_S
            {
                break;
            }
            end += 1;
        }
        let group = &order[start..end];
        if group.len() >= 2 {
            let spread = |values: Vec<f32>| {
                values.iter().copied().fold(f32::MIN, f32::max)
                    - values.iter().copied().fold(f32::MAX, f32::min)
            };
            let evs: Vec<f32> = group
                .iter()
                .filter_map(|&i| photos[i].capture.exposure_value)
                .collect();
            let biases: Vec<f32> = group
                .iter()
                .filter_map(|&i| photos[i].capture.bias)
                .collect();
            if (evs.len() == group.len() && spread(evs) >= BRACKET_SPREAD_EV)
                || (biases.len() == group.len() && spread(biases) >= BRACKET_SPREAD_EV)
            {
                group.iter().for_each(|&i| is_bracket[i] = true);
            }
        }
        start = end;
    }
    is_bracket
}

/// A dark subject against a bright background with no lit face: the match
/// would brighten what the photographer left dark.
fn is_silhouette(stats: &PhotoStats) -> bool {
    let ev_of = |bin: usize| HIST_MIN_EV + (bin as f32 + 0.5) / BINS_PER_EV;
    let bright: f32 = stats.clipped_share
        + stats
            .histogram
            .iter()
            .enumerate()
            .filter(|(i, _)| ev_of(*i) >= -2.5)
            .map(|(_, v)| v)
            .sum::<f32>();
    let dark: f32 = stats
        .histogram
        .iter()
        .enumerate()
        .filter(|(i, _)| ev_of(*i) < -7.5)
        .map(|(_, v)| v)
        .sum();
    bright >= 0.15 && dark >= 0.25 && bright + dark >= 0.6
}

// --- plan ---------------------------------------------------------------------

/// Proposals for every photo of `photos` against the references among them
/// (several references: each photo follows the ones before and after it in
/// capture time, blended by distance; without times, by position).
pub fn plan(photos: &[SeriesPhoto], options: MatchOptions) -> Vec<Proposal> {
    let refs: Vec<usize> = (0..photos.len()).filter(|&i| photos[i].reference).collect();
    let bracket = brackets(photos);
    let all_timed = photos.iter().all(|p| p.capture.time.is_some());
    let when = |i: usize| -> f64 {
        if all_timed {
            photos[i].capture.time.unwrap()
        } else {
            i as f64
        }
    };
    photos
        .iter()
        .enumerate()
        .map(|(i, photo)| {
            let keep = |reasons: Vec<Reason>| Proposal {
                path: photo.path.clone(),
                status: Status::Keep,
                reasons,
                exposure: None,
                white_balance: None,
                confidence: 0.0,
                exposure_anchor: None,
                white_balance_anchor: None,
                current_exposure: photo.exposure,
                current_white_balance: photo.white_balance,
            };
            if photo.reference {
                return Proposal {
                    status: Status::Reference,
                    confidence: 1.0,
                    ..keep(Vec::new())
                };
            }
            if refs.is_empty() {
                return keep(vec![Reason::NoReference]);
            }
            if photo.stats.is_none() {
                return keep(vec![Reason::Unreadable]);
            }
            if bracket[i] {
                return keep(vec![Reason::Bracket]);
            }
            // The references around this photo in time, with weights.
            let t = when(i);
            let before = refs
                .iter()
                .copied()
                .filter(|&r| when(r) <= t)
                .max_by(|&a, &b| when(a).total_cmp(&when(b)));
            let after = refs
                .iter()
                .copied()
                .filter(|&r| when(r) > t)
                .min_by(|&a, &b| when(a).total_cmp(&when(b)));
            let around: Vec<(usize, f64)> = match (before, after) {
                (Some(a), Some(b)) => {
                    let span = (when(b) - when(a)).max(1e-9);
                    vec![(a, (when(b) - t) / span), (b, (t - when(a)) / span)]
                }
                (Some(a), None) | (None, Some(a)) => vec![(a, 1.0)],
                (None, None) => unreachable!("refs is not empty"),
            };
            let estimates: Vec<(Estimate, f64)> = around
                .iter()
                .map(|&(r, w)| (estimate(&photos[r], photo, options), w))
                .collect();
            let blend = |value: &dyn Fn(&Estimate) -> Option<f64>| -> Option<f64> {
                let parts: Vec<(f64, f64)> = estimates
                    .iter()
                    .filter_map(|(e, w)| value(e).map(|v| (v, *w)))
                    .collect();
                let weight: f64 = parts.iter().map(|(_, w)| w).sum();
                (weight > 0.0).then(|| parts.iter().map(|(v, w)| v * w).sum::<f64>() / weight)
            };
            let exposure_change = blend(&|e| e.exposure_change.map(f64::from)).map(|v| v as f32);
            let wb_mired = blend(&|e| e.white_balance.map(|wb| mired(wb.temperature)));
            let wb_tint = blend(&|e| e.white_balance.map(|wb| wb.tint));
            let mut reasons: Vec<Reason> = Vec::new();
            for (e, _) in &estimates {
                for reason in &e.reasons {
                    if !reasons.contains(reason) {
                        reasons.push(*reason);
                    }
                }
            }
            let confidence = estimates
                .iter()
                .map(|(e, _)| e.confidence)
                .fold(1.0, f32::min);
            let stats = photo.stats.as_ref().unwrap();
            let mut exposure = exposure_change
                .map(|d| (photo.exposure + d).clamp(-EXPOSURE_RANGE, EXPOSURE_RANGE));
            let mut status = if confidence < REVIEW_BELOW {
                Status::Review
            } else {
                Status::Match
            };
            if let Some(d) = exposure_change {
                // A face against the light may be meant dark too, but a
                // moderate lift of a visible face is the match's job.
                let threshold = if stats.skin.is_some() { 1.5 } else { 0.3 };
                if d.abs() > threshold && is_silhouette(stats) {
                    exposure = None;
                    reasons.push(Reason::Silhouette);
                } else if photo
                    .capture
                    .bias
                    .is_some_and(|b| (b <= -1.7 && d > 1.0) || (b >= 1.7 && d < -1.0))
                {
                    reasons.push(Reason::CameraCompensation);
                    status = Status::Review;
                } else if d.abs() > REVIEW_CHANGE_EV {
                    reasons.push(Reason::LargeChange);
                    status = Status::Review;
                }
            }
            let white_balance = wb_mired.zip(wb_tint).map(|(m, tint)| WhiteBalance {
                temperature: (1e6 / m).round(),
                tint: tint.round(),
            });
            // Changes too small to see are left out.
            let exposure = exposure.filter(|e| (e - photo.exposure).abs() >= 0.05);
            let white_balance = white_balance.filter(|wb| {
                photo.white_balance.is_none_or(|old| {
                    (mired(wb.temperature) - mired(old.temperature)).abs() >= 3.0
                        || (wb.tint - old.tint).abs() >= 2.0
                })
            });
            if exposure.is_none() && white_balance.is_none() {
                if !reasons.iter().any(|r| {
                    matches!(
                        r,
                        Reason::Silhouette | Reason::ColouredLight | Reason::NoWhiteBalance
                    )
                }) {
                    reasons.push(Reason::AlreadyMatches);
                }
                status = Status::Keep;
            }
            let first = &estimates[0].0;
            Proposal {
                path: photo.path.clone(),
                status,
                reasons,
                exposure,
                white_balance,
                confidence,
                exposure_anchor: first.exposure_anchor,
                white_balance_anchor: first.white_balance_anchor,
                current_exposure: photo.exposure,
                current_white_balance: photo.white_balance,
            }
        })
        .collect()
}

/// Rendered log2 luminance key (median of the analysed pixels) of a photo
/// at an Exposure: the evaluation's measure of consistency.
pub fn rendered_median(stats: &PhotoStats, baseline_ev: f32, exposure: f32) -> Option<f32> {
    let total: f32 = stats.histogram.iter().sum::<f32>() + stats.clipped_share;
    let mut seen = 0.0;
    for (i, v) in stats.histogram.iter().enumerate() {
        seen += v;
        if seen >= 0.5 * total {
            return Some(HIST_MIN_EV + (i as f32 + 0.5) / BINS_PER_EV + baseline_ev + exposure);
        }
    }
    (stats.clipped_share > 0.0).then_some(HIST_MAX_EV + baseline_ev + exposure)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A histogram with Gaussian modes (centre EV, sigma EV, mass).
    fn hist(modes: &[(f32, f32, f32)]) -> Vec<f32> {
        let mut h = vec![0f32; HIST_BINS];
        for (i, v) in h.iter_mut().enumerate() {
            let ev = HIST_MIN_EV + (i as f32 + 0.5) / BINS_PER_EV;
            *v = modes
                .iter()
                .map(|(c, s, m)| m * (-(ev - c).powi(2) / (2.0 * s * s)).exp())
                .sum();
        }
        let sum: f32 = h.iter().sum();
        let mass: f32 = modes.iter().map(|m| m.2).sum();
        h.iter_mut().for_each(|v| *v *= mass / sum);
        h
    }

    fn stats(histogram: Vec<f32>) -> PhotoStats {
        PhotoStats {
            histogram,
            clipped_share: 0.0,
            light: Some(WhiteBalance {
                temperature: 5500.0,
                tint: 5.0,
            }),
            skin: None,
        }
    }

    fn photo(path: &str, stats: PhotoStats, time: f64) -> SeriesPhoto {
        SeriesPhoto {
            path: path.into(),
            stats: Some(stats),
            capture: Capture {
                time: Some(time),
                camera: "A".into(),
                ..Default::default()
            },
            baseline_ev: 0.0,
            exposure: 0.0,
            white_balance: Some(WhiteBalance {
                temperature: 5500.0,
                tint: 5.0,
            }),
            reference: false,
            error: None,
        }
    }

    const BOTH: MatchOptions = MatchOptions {
        exposure: true,
        white_balance: true,
    };

    #[test]
    fn a_darker_copy_of_the_scene_is_brightened_by_its_difference() {
        let reference = photo("r", stats(hist(&[(-5.0, 1.0, 0.6), (-2.0, 0.5, 0.4)])), 0.0);
        let dark = photo(
            "d",
            stats(hist(&[(-6.3, 1.0, 0.6), (-3.3, 0.5, 0.4)])),
            60.0,
        );
        let p = plan(
            &[
                SeriesPhoto {
                    reference: true,
                    ..reference
                },
                dark,
            ],
            BOTH,
        );
        let change = p[1].exposure.unwrap();
        assert!((change - 1.3).abs() < 0.06, "{change}");
        assert_eq!(p[1].status, Status::Match);
        assert!(p[1].confidence > 0.8, "{}", p[1].confidence);
    }

    #[test]
    fn the_recipe_exposure_and_baseline_count() {
        let mut reference = photo("r", stats(hist(&[(-5.0, 1.0, 1.0)])), 0.0);
        reference.reference = true;
        reference.exposure = 0.5;
        let mut other = photo("o", stats(hist(&[(-5.0, 1.0, 1.0)])), 1.0);
        other.baseline_ev = 0.3;
        let p = plan(&[reference, other], BOTH);
        assert!(
            (p[1].exposure.unwrap() - 0.2).abs() < 0.06,
            "{:?}",
            p[1].exposure
        );
    }

    #[test]
    fn a_new_dark_subject_does_not_pull_the_match() {
        // Same light; half the frame now a dark coat. The median moves by
        // about 2 EV, the histogram match stays near 0.
        let reference = photo("r", stats(hist(&[(-4.0, 0.8, 0.7), (-1.5, 0.4, 0.3)])), 0.0);
        let coat = photo(
            "c",
            stats(hist(&[
                (-4.0, 0.8, 0.35),
                (-1.5, 0.4, 0.15),
                (-9.0, 0.6, 0.5),
            ])),
            10.0,
        );
        let p = plan(
            &[
                SeriesPhoto {
                    reference: true,
                    ..reference.clone()
                },
                coat.clone(),
            ],
            BOTH,
        );
        let change = p[1].exposure.unwrap_or(0.0);
        assert!(change.abs() < 0.2, "{change}");
        let medians = [reference, coat]
            .map(|p| rendered_median(p.stats.as_ref().unwrap(), 0.0, 0.0).unwrap());
        assert!(medians[0] - medians[1] > 1.0, "{medians:?}");
    }

    #[test]
    fn a_bracket_is_kept() {
        let mut photos = vec![SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-5.0, 1.0, 1.0)])), 0.0)
        }];
        for (i, ev) in [10.0f32, 12.0, 8.0].iter().enumerate() {
            let mut p = photo(
                &format!("b{i}"),
                stats(hist(&[(-5.0 - (ev - 10.0), 1.0, 1.0)])),
                100.0 + i as f64 * 0.4,
            );
            p.capture.exposure_value = Some(*ev);
            photos.push(p);
        }
        let p = plan(&photos, BOTH);
        for proposal in &p[1..] {
            assert_eq!(proposal.status, Status::Keep);
            assert_eq!(proposal.reasons, vec![Reason::Bracket]);
        }
    }

    #[test]
    fn a_burst_with_one_exposure_is_not_a_bracket() {
        let mut photos = vec![SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-5.0, 1.0, 1.0)])), 0.0)
        }];
        for i in 0..3 {
            let mut p = photo(
                &format!("b{i}"),
                stats(hist(&[(-6.0, 1.0, 1.0)])),
                100.0 + i as f64 * 0.2,
            );
            p.capture.exposure_value = Some(10.0);
            photos.push(p);
        }
        assert!(
            plan(&photos, BOTH)[1..]
                .iter()
                .all(|p| p.status == Status::Match)
        );
    }

    #[test]
    fn a_silhouette_stays_dark() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0)
        };
        let mut s = stats(hist(&[(-11.0, 0.6, 0.45), (-1.0, 0.4, 0.35)]));
        s.clipped_share = 0.2;
        let p = plan(&[reference, photo("s", s, 5.0)], BOTH);
        assert!(p[1].exposure.is_none(), "{:?}", p[1]);
        assert!(p[1].reasons.contains(&Reason::Silhouette));
    }

    #[test]
    fn white_balance_follows_the_light_relative_to_the_reference_setting() {
        // Reference: daylight 5500 K rendered at 6000 K (a little warmer).
        let mut reference = photo("r", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0);
        reference.reference = true;
        reference.white_balance = Some(WhiteBalance {
            temperature: 6000.0,
            tint: 8.0,
        });
        // Tungsten light 3000 K.
        let mut s = stats(hist(&[(-4.0, 1.0, 1.0)]));
        s.light = Some(WhiteBalance {
            temperature: 3000.0,
            tint: 2.0,
        });
        let p = plan(&[reference, photo("t", s, 1.0)], BOTH);
        let wb = p[1].white_balance.unwrap();
        let expected = 1e6 / (mired(6000.0) - mired(5500.0) + mired(3000.0));
        assert!(
            (wb.temperature - expected).abs() < 2.0,
            "{wb:?} vs {expected}"
        );
        assert_eq!(wb.tint, 5.0);
    }

    #[test]
    fn skin_anchors_white_balance_under_mixed_light() {
        let skin = |light: f64| SkinSample {
            log_luma: -3.0,
            light: Some(WhiteBalance {
                temperature: light,
                tint: 10.0,
            }),
            pixels: 2000,
            faces: 1,
        };
        let mut rs = stats(hist(&[(-4.0, 1.0, 1.0)]));
        rs.skin = Some(skin(3500.0));
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", rs, 0.0)
        };
        // The scene estimate sees tungsten background, the face is lit by
        // flash: the skin anchor dominates.
        let mut s = stats(hist(&[(-4.0, 1.0, 1.0)]));
        s.light = Some(WhiteBalance {
            temperature: 3000.0,
            tint: 5.0,
        });
        s.skin = Some(skin(3500.0));
        let p = plan(&[reference, photo("f", s, 1.0)], BOTH);
        let wb = p[1].white_balance.map_or(5500.0, |wb| wb.temperature);
        assert!(
            (mired(wb) - mired(5500.0)).abs() < 0.3 * (mired(3000.0) - mired(5500.0)).abs(),
            "{wb}"
        );
        assert_eq!(p[1].white_balance_anchor, Some(Anchor::SkinAndScene));
    }

    #[test]
    fn coloured_light_keeps_its_white_balance() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0)
        };
        let mut s = stats(hist(&[(-5.0, 1.0, 1.0)]));
        s.light = Some(WhiteBalance {
            temperature: 9000.0,
            tint: 70.0,
        });
        let p = plan(&[reference, photo("stage", s, 1.0)], BOTH);
        assert!(p[1].white_balance.is_none());
        assert!(p[1].exposure.is_some());
        assert!(p[1].reasons.contains(&Reason::ColouredLight));
    }

    fn with_skin(mut s: PhotoStats, log_luma: f32) -> PhotoStats {
        s.skin = Some(SkinSample {
            log_luma,
            light: Some(WhiteBalance {
                temperature: 3200.0,
                tint: -12.0,
            }),
            pixels: 5000,
            faces: 1,
        });
        s
    }

    #[test]
    fn faces_lead_the_exposure_when_the_background_changes() {
        // Same light on the face; the photo is framed against a bright
        // wall, which pulls the histogram match towards darkening.
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", with_skin(stats(hist(&[(-5.0, 1.0, 1.0)])), -3.0), 0.0)
        };
        let wall = photo("w", with_skin(stats(hist(&[(-4.5, 1.0, 1.0)])), -5.0), 1.0);
        let p = plan(&[reference, wall], BOTH);
        let change = p[1].exposure.unwrap();
        assert!(change > 1.6 && change < 2.1, "{change}");
        assert_eq!(p[1].status, Status::Match, "{:?}", p[1]);
    }

    #[test]
    fn a_fluorescent_cast_is_corrected_but_stage_light_is_kept() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0)
        };
        let mut green = stats(hist(&[(-4.0, 1.0, 1.0)]));
        green.light = Some(WhiteBalance {
            temperature: 3300.0,
            tint: -45.0,
        });
        let mut magenta = stats(hist(&[(-4.0, 1.0, 1.0)]));
        magenta.light = Some(WhiteBalance {
            temperature: 4000.0,
            tint: 90.0,
        });
        let p = plan(
            &[reference, photo("g", green, 1.0), photo("m", magenta, 2.0)],
            BOTH,
        );
        assert!(p[1].white_balance.is_some(), "{:?}", p[1]);
        assert!(!p[1].reasons.contains(&Reason::ColouredLight));
        assert!(p[2].white_balance.is_none());
        assert!(p[2].reasons.contains(&Reason::ColouredLight));
    }

    #[test]
    fn a_face_against_the_sun_is_not_lifted_by_stops() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", with_skin(stats(hist(&[(-4.0, 1.0, 1.0)])), -3.0), 0.0)
        };
        let mut backlit = with_skin(stats(hist(&[(-11.0, 0.6, 0.45), (-1.0, 0.4, 0.35)])), -8.0);
        backlit.clipped_share = 0.2;
        let p = plan(&[reference, photo("b", backlit, 1.0)], BOTH);
        assert!(p[1].exposure.is_none(), "{:?}", p[1]);
        assert!(p[1].reasons.contains(&Reason::Silhouette));
    }

    #[test]
    fn a_change_of_several_stops_is_only_proposed() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0)
        };
        let p = plan(
            &[reference, photo("d", stats(hist(&[(-7.2, 1.0, 1.0)])), 1.0)],
            BOTH,
        );
        assert!((p[1].exposure.unwrap() - 3.2).abs() < 0.1, "{:?}", p[1]);
        assert_eq!(p[1].status, Status::Review);
        assert_eq!(p[1].reasons, vec![Reason::LargeChange]);
    }

    #[test]
    fn two_references_are_blended_by_capture_time() {
        let mut first = photo("a", stats(hist(&[(-4.0, 1.0, 1.0)])), 0.0);
        first.reference = true;
        let mut last = photo("b", stats(hist(&[(-4.0, 1.0, 1.0)])), 100.0);
        last.reference = true;
        last.exposure = 1.0;
        let middle = photo("m", stats(hist(&[(-4.0, 1.0, 1.0)])), 25.0);
        let p = plan(&[first, middle, last], BOTH);
        assert!(
            (p[1].exposure.unwrap() - 0.25).abs() < 0.06,
            "{:?}",
            p[1].exposure
        );
    }

    #[test]
    fn unrelated_content_is_only_proposed_for_review() {
        let reference = SeriesPhoto {
            reference: true,
            ..photo("r", stats(hist(&[(-3.0, 0.3, 1.0)])), 0.0)
        };
        // Spread flat over 12 EV: no clear alignment.
        let flat = stats(hist(&[(-9.0, 4.0, 1.0)]));
        let p = plan(
            &[reference, photo("f", flat, 1.0)],
            MatchOptions {
                exposure: true,
                white_balance: false,
            },
        );
        assert_eq!(p[1].status, Status::Review, "{:?}", p[1]);
        assert!(p[1].reasons.contains(&Reason::LittleInCommon));
    }

    #[test]
    fn stats_of_a_uniform_image() {
        let img = image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(
            64,
            64,
            image::Rgb([0.18f32, 0.18, 0.18]),
        ));
        let s = photo_stats(&img, None, &|_| None).unwrap();
        let median = rendered_median(&s, 0.0, 0.0).unwrap();
        assert!((median - 0.18f32.log2()).abs() < 0.13, "{median}");
        assert_eq!(s.clipped_share, 0.0);
    }
}
