//! HDR merge of bracketed exposures into one scene-linear image.
//!
//! - Every frame is brought to the reference frame's white balance (auto
//!   white balance may differ between brackets) and measured against it:
//!   the exposure ratios come from EXIF and are refined on the pixels both
//!   frames expose well (nominal shutter speeds are rounded).
//! - Hand-held brackets are aligned with the dense lens-aware alignment on
//!   exposure-matched luminance.
//! - Pixels merge as the photon-count estimate Σv / Σexposure over the
//!   frames that are not saturated in camera units (a soft ramp below the
//!   sensor white), which weighs each frame by its signal-to-noise ratio.
//! - Deghosting compares each frame with the consensus of the frames closer
//!   to the reference exposure and drops it where they disagree, so moving
//!   things come from one frame only. Where the reference itself is bright
//!   (clean), frames are also compared with it pixel by pixel.
//!
//! The result keeps the reference frame's exposure (its midtones render as
//! they do in the reference) with highlights above 1.0.
use crate::align::{AlignConfig, LensWarp, build_align_pyramids, solve_alignment};
pub use crate::frame::working_to_camera;
use crate::frame::{Capture, Frame, load_frame, read_capture, rebalance};
use crate::planes::{Plane, gaussian_blur};
use anyhow::{Result, anyhow, ensure};
use image::{Rgb32FImage, RgbImage};
use nalgebra::Matrix3;
use rawler::decoders::RawMetadata;
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_rawdecode::raw_processing::ImageColor;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deghost {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl Deghost {
    /// Disagreement (EV, after the noise floor) from which a frame counts as
    /// a ghost.
    fn threshold_ev(self) -> Option<f32> {
        match self {
            Deghost::Off => None,
            Deghost::Low => Some(0.7),
            Deghost::Medium => Some(0.4),
            Deghost::High => Some(0.22),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrConfig {
    pub align: bool,
    pub deghost: Deghost,
}

impl Default for HdrConfig {
    fn default() -> Self {
        HdrConfig {
            align: true,
            deghost: Deghost::Off,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrFrameReport {
    pub path: PathBuf,
    /// EXIF exposure (EV, larger = brighter).
    pub exif_ev: Option<f64>,
    /// Measured brightness relative to the reference frame (EV).
    pub relative_ev: f64,
    /// Largest alignment displacement (pixels); 0 when not warped.
    pub shift_px: f64,
    /// Alignment failed and the frame was merged unaligned.
    pub align_failed: bool,
    /// Share of the frame dropped as ghosts.
    pub ghost_fraction: f64,
}

pub struct HdrResult {
    /// Linear working RGB at the reference exposure (see `Frame::image`).
    pub image: Rgb32FImage,
    /// Index of the reference frame in the input order.
    pub reference: usize,
    pub reference_path: PathBuf,
    pub color: Option<ImageColor>,
    pub metadata: Option<RawMetadata>,
    pub frames: Vec<HdrFrameReport>,
    /// Where any frame was dropped as a ghost, at reduced size (0–1).
    pub ghost_mask: Option<Plane>,
}

/// Camera units below which a channel counts as fully usable, and where a
/// frame is no longer used (sensor white = 1).
const SATURATION_START: f32 = 0.80;
const SATURATION_END: f32 = 0.95;
/// Working-space floor for log comparisons, per unit exposure.
const NOISE_FLOOR: f32 = 0.004;
/// Share of a pixel's luminance below which a colour channel's difference
/// does not count as a ghost (`ghost_map`).
const CHANNEL_FLOOR: f32 = 0.1;
/// Working luminance of the reference from which every other frame must
/// agree with it pixel by pixel (about two stops below where the sensor
/// starts to saturate; the test fades in from half of it, so a smooth
/// reflection crossing it gets no contour): there the reference alone is clean, and a frame that
/// shows something else (a sun glint that moved on rippling water) left
/// dark rims and specks around the glints that the low-resolution ghost map
/// is too coarse to catch (notes: benchmark-results/mrg2-2026-11-23).
const BRIGHT_REFERENCE: f32 = 0.2;
const LOW_RES_TARGET: usize = 1500;
const ALIGN_TARGET: usize = 2400;

fn apply_matrix(image: &mut Rgb32FImage, m: &Matrix3<f32>) {
    let raw: &mut [f32] = image;
    raw.par_chunks_mut(3).for_each(|p| {
        let v = m * nalgebra::Vector3::new(p[0], p[1], p[2]);
        p.copy_from_slice(v.as_slice());
    });
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn luma(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// Weight of a pixel by its saturation in camera units.
fn usable(p: &[f32], to_camera: &Matrix3<f32>) -> f32 {
    let cam = to_camera * nalgebra::Vector3::new(p[0], p[1], p[2]);
    1.0 - smoothstep(SATURATION_START, SATURATION_END, cam.max())
}

/// Box averages at 1/`f` size: linear RGB, luminance and usability.
struct LowRes {
    rgb: [Plane; 3],
    y: Plane,
    u: Plane,
}

fn low_res(view: &View, to_camera: &Matrix3<f32>, f: usize) -> LowRes {
    let (w, h) = (view.w, view.h);
    let (lw, lh) = (w.div_ceil(f), h.div_ceil(f));
    let rows: Vec<Vec<[f32; 4]>> = (0..lh)
        .into_par_iter()
        .map(|ly| {
            (0..lw)
                .map(|lx| {
                    let mut sum = [0.0f32; 4];
                    let mut n = 0.0f32;
                    for y in ly * f..((ly + 1) * f).min(h) {
                        for x in lx * f..((lx + 1) * f).min(w) {
                            // Pixels without a source count as black, unusable.
                            if let Some(p) = view.at(x, y) {
                                sum[0] += p[0];
                                sum[1] += p[1];
                                sum[2] += p[2];
                                sum[3] += usable(&p, to_camera);
                            }
                            n += 1.0;
                        }
                    }
                    sum.map(|v| v / n)
                })
                .collect()
        })
        .collect();
    let mut planes: [Plane; 4] = std::array::from_fn(|_| Plane::new(lw, lh));
    for (ly, row) in rows.into_iter().enumerate() {
        for (lx, v) in row.into_iter().enumerate() {
            for (plane, value) in planes.iter_mut().zip(v) {
                plane.data[ly * lw + lx] = value;
            }
        }
    }
    let [r, g, b, u] = planes;
    let mut y = Plane::new(lw, lh);
    for (i, o) in y.data.iter_mut().enumerate() {
        *o = luma(&[r.data[i], g.data[i], b.data[i]]);
    }
    LowRes {
        rgb: [r, g, b],
        y,
        u,
    }
}

/// Where a frame shows something else than the consensus of the frames
/// merged before it (1 = a ghost, dropped): the largest disagreement of
/// any colour channel, in EV, through a smooth step from `threshold_ev` to
/// twice that. Each channel counts on its own (RapidRAW's ghost test,
/// Timon Käch, AGPL-3.0): a moving object of another colour but similar
/// brightness (a red coat on green grass) differs strongly in one channel
/// while luminance alone half-passed it as the same scene (notes:
/// benchmark-results/mrg1-2026-11-22). A channel near black in a saturated
/// colour (red in deep green foliage) is mostly noise, so each channel's
/// floor also includes a share of the pixel's luminance (`CHANNEL_FLOOR`):
/// a colour change counts once it is a sizeable part of the brightness.
/// The noise part of each channel's floor scales with how much the camera
/// matrix amplifies sensor noise into that channel against luminance
/// (`noise_gain`, from `channel_noise_gain`): with one floor for all, the
/// noisier channels of the darkest frame read as ghosts in deep shadows
/// (13–19 % of a static bracket's darkest frame at High).
/// `cons_rgb` are the consensus sums over `cons_den`; `ratio` is the
/// frame's exposure against the reference.
fn ghost_map(
    frame: &LowRes,
    cons_rgb: &[Plane; 3],
    cons_den: &Plane,
    ratio: f32,
    threshold_ev: f32,
    noise_gain: [f32; 3],
) -> Plane {
    let eps = NOISE_FLOOR * (1.0f32).max(1.0 / ratio);
    let mut g = Plane::new(frame.u.w, frame.u.h);
    g.data.par_iter_mut().enumerate().for_each(|(i, g)| {
        let cd = cons_den.data[i];
        if frame.u.data[i] < 0.9 || cd <= 0.0 {
            return;
        }
        let k: [f32; 3] = std::array::from_fn(|c| (cons_rgb[c].data[i] / cd).max(0.0));
        let share = CHANNEL_FLOOR * luma(&k);
        let e = (0..3)
            .map(|c| {
                let v = frame.rgb[c].data[i].max(0.0) / ratio;
                let floor = eps * noise_gain[c] + share;
                ((v + floor) / (k[c] + floor)).log2().abs()
            })
            .fold(0.0f32, f32::max);
        *g = smoothstep(threshold_ev, 2.0 * threshold_ev, e);
    });
    gaussian_blur(&max_filter(&g, 3), 3.0).map(|v| v.clamp(0.0, 1.0))
}

/// Noise of each working channel relative to the noise of luminance, for
/// equal noise in the camera channels: row norms of the camera → working
/// matrix over the norm of its luminance row.
fn channel_noise_gain(to_camera: &Matrix3<f32>) -> [f32; 3] {
    let m = to_camera.try_inverse().unwrap_or_else(Matrix3::identity);
    let l = nalgebra::RowVector3::new(0.2126f32, 0.7152, 0.0722) * m;
    let ln = l.norm().max(1e-6);
    std::array::from_fn(|c| m.row(c).norm() / ln)
}

/// The reference pixel's role for the frames merged after it: clipped or
/// nearly (`INFINITY`), bright enough to judge other frames by (its
/// luminance), or neither (0).
fn reference_state(p: &[f32], to_camera: &Matrix3<f32>) -> f32 {
    if usable(p, to_camera) < 1.0 {
        return f32::INFINITY;
    }
    let l = luma(p);
    if l > 0.5 * BRIGHT_REFERENCE { l } else { 0.0 }
}

/// How much of a frame's pixel is dropped because it disagrees with a
/// bright reference pixel (`ref_luma`, from `reference_state`; `luma` is
/// the frame's in reference units) by more than `threshold_ev`: soft in the
/// reference's brightness, whole in the disagreement, so it meets the
/// clipped-reference rule (a frame darker than the clipping point is
/// dropped whole) without a step where a reflection reaches saturation.
fn bright_rejection(ref_luma: f32, luma: f32, threshold_ev: f32) -> f32 {
    if ref_luma <= 0.0 || (luma.max(1e-6) / ref_luma).log2().abs() <= threshold_ev {
        return 0.0;
    }
    smoothstep(0.5 * BRIGHT_REFERENCE, BRIGHT_REFERENCE, ref_luma)
}

fn max_filter(p: &Plane, radius: usize) -> Plane {
    let pass = |src: &Plane, horizontal: bool| {
        let mut out = Plane::new(src.w, src.h);
        out.data
            .par_chunks_mut(src.w)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let mut m = 0.0f32;
                    for d in -(radius as i64)..=radius as i64 {
                        let v = if horizontal {
                            src.clamped(x as i64 + d, y as i64)
                        } else {
                            src.clamped(x as i64, y as i64 + d)
                        };
                        m = m.max(v);
                    }
                    *o = m;
                }
            });
        out
    };
    pass(&pass(p, true), false)
}

/// Bicubic (Catmull-Rom) sample of an interleaved RGB image; `None` outside.
fn sample_rgb(raw: &[f32], w: usize, h: usize, x: f64, y: f64) -> Option<[f32; 3]> {
    if x < 1.0 || y < 1.0 || x > (w - 2) as f64 || y > (h - 2) as f64 {
        return None;
    }
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (tx, ty) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let weights = |t: f32| {
        let t2 = t * t;
        let t3 = t2 * t;
        [
            -0.5 * t3 + t2 - 0.5 * t,
            1.5 * t3 - 2.5 * t2 + 1.0,
            -1.5 * t3 + 2.0 * t2 + 0.5 * t,
            0.5 * t3 - 0.5 * t2,
        ]
    };
    let (wx, wy) = (weights(tx), weights(ty));
    let mut out = [0.0f32; 3];
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for (j, wyj) in wy.iter().enumerate() {
        let yy = (y0 - 1 + j as i64).clamp(0, h as i64 - 1) as usize;
        for (i, wxi) in wx.iter().enumerate() {
            let xx = (x0 - 1 + i as i64).clamp(0, w as i64 - 1) as usize;
            let p = &raw[(yy * w + xx) * 3..][..3];
            let wgt = wyj * wxi;
            for c in 0..3 {
                out[c] += p[c] * wgt;
                if (1..=2).contains(&i) && (1..=2).contains(&j) {
                    lo[c] = lo[c].min(p[c]);
                    hi[c] = hi[c].max(p[c]);
                }
            }
        }
    }
    // Within the four nearest pixels: Catmull-Rom over- and undershoots at
    // sun glints, and a darker frame's undershoot, scaled up by the
    // exposure ratio, drew dark rims around them.
    for c in 0..3 {
        out[c] = out[c].clamp(lo[c], hi[c]);
    }
    Some(out)
}

/// A frame as the merge reads it: its own pixels, or resampled on the fly
/// so that it lines up with the reference (`warp` maps reference pixels to
/// frame pixels; `None` where there is no source). A warped copy held a
/// second full-size image per frame.
struct View<'a> {
    raw: &'a [f32],
    w: usize,
    h: usize,
    warp: Option<LensWarp>,
}

impl<'a> View<'a> {
    fn new(image: &'a Rgb32FImage, warp: Option<LensWarp>) -> Self {
        Self {
            raw: image,
            w: image.width() as usize,
            h: image.height() as usize,
            warp,
        }
    }

    #[inline]
    fn at(&self, x: usize, y: usize) -> Option<[f32; 3]> {
        match &self.warp {
            None => {
                let p = &self.raw[(y * self.w + x) * 3..][..3];
                Some([p[0], p[1], p[2]])
            }
            Some(warp) => {
                let (u, v) = warp.apply(x as f64, y as f64);
                sample_rgb(self.raw, self.w, self.h, u, v)
            }
        }
    }
}

/// Exposure-matched log luminance for the alignment, box-averaged over
/// `f`×`f` pixels. `ratio` = frame brightness / reference brightness.
fn align_plane(image: &Rgb32FImage, ratio: f32, f: usize) -> Plane {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let (aw, ah) = (w.div_ceil(f), h.div_ceil(f));
    let hi = 0.85 * (1.0f32).min(1.0 / ratio);
    let lo = 0.003 * (1.0f32).max(1.0 / ratio);
    let raw: &[f32] = image;
    let mut out = Plane::new(aw, ah);
    out.data
        .par_chunks_mut(aw)
        .enumerate()
        .for_each(|(ay, row)| {
            for (ax, o) in row.iter_mut().enumerate() {
                let (mut s, mut n) = (0.0f32, 0.0f32);
                for y in ay * f..((ay + 1) * f).min(h) {
                    for x in ax * f..((ax + 1) * f).min(w) {
                        s += luma(&raw[(y * w + x) * 3..][..3]);
                        n += 1.0;
                    }
                }
                let v = if n > 0.0 { s / n / ratio } else { lo };
                *o = v.clamp(lo, hi).log2();
            }
        });
    out
}

/// Refines a similarity `warp` at full resolution: the residual shifts of
/// 3×3 tiles (aligned at full size), fitted as a small similarity and
/// composed with `warp`. The coarse alignment runs at a fraction of the size,
/// where 0.1 px is half a pixel or more at full size.
const TILE: usize = 384;

/// Full-resolution luminance of the reference at the 3×3 tile positions
/// (top-left corner, values).
fn reference_tiles(reference: &Rgb32FImage) -> Vec<(usize, usize, Vec<f32>)> {
    let (w, h) = (reference.width() as usize, reference.height() as usize);
    if w < 3 * TILE || h < 3 * TILE {
        return Vec::new();
    }
    let raw: &[f32] = reference;
    (0..9)
        .map(|k| {
            let (x0, y0) = (
                w * ((k % 3) * 2 + 1) / 6 - TILE / 2,
                h * ((k / 3) * 2 + 1) / 6 - TILE / 2,
            );
            let mut v = Vec::with_capacity(TILE * TILE);
            for y in 0..TILE {
                for x in 0..TILE {
                    v.push(luma(&raw[((y0 + y) * w + x0 + x) * 3..][..3]));
                }
            }
            (x0, y0, v)
        })
        .collect()
}

fn refine_full_res(
    ref_tiles: &[(usize, usize, Vec<f32>)],
    frame: &Rgb32FImage,
    ratio: f32,
    warp: &LensWarp,
) -> Option<LensWarp> {
    const T: usize = TILE;
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if ref_tiles.len() < 9 || warp.k1 != 0.0 || warp.k2 != 0.0 {
        return None;
    }
    let hi = 0.85 * (1.0f32).min(1.0 / ratio);
    let lo = 0.003 * (1.0f32).max(1.0 / ratio);
    let tone = |y: f32| y.clamp(lo, hi).log2();
    let src_raw: &[f32] = frame;
    let (c0x, c0y) = (w as f64 / 2.0, h as f64 / 2.0);
    let tiles: Vec<(f64, f64, [f64; 2], f64)> = ref_tiles
        .par_iter()
        .filter_map(|(x0, y0, ref_luma)| {
            let (x0, y0) = (*x0, *y0);
            let (cx, cy) = (x0 + T / 2, y0 + T / 2);
            let mut tmpl = Plane::new(T, T);
            let mut src = Plane::new(T, T);
            for y in 0..T {
                for x in 0..T {
                    let (fx, fy) = (x0 + x, y0 + y);
                    tmpl.data[y * T + x] = tone(ref_luma[y * T + x]);
                    let (u, v) = warp.apply(fx as f64, fy as f64);
                    let p = sample_rgb(src_raw, w, h, u, v)?;
                    src.data[y * T + x] = tone(luma(&p) / ratio);
                }
            }
            let cfg = AlignConfig {
                finest_dim: T,
                coarsest_dim: 48,
                max_model: crate::align::WarpModel::Translation,
                polynomial_refinement: false,
                ..AlignConfig::default()
            };
            let pyramids = build_align_pyramids(&tmpl, &src, &cfg);
            let pose = solve_alignment(&pyramids, LensWarp::identity(T, T), &cfg);
            let t = pose.warp.t;
            (pose.quality > 0.5 && t[0].abs() < 4.0 && t[1].abs() < 4.0).then_some((
                cx as f64,
                cy as f64,
                t,
                pose.quality,
            ))
        })
        .collect();
    if tiles.len() < 4 {
        return None;
    }
    // d(x) = [s −r; r s](x − c0) + t, weighted least squares.
    let mut ata = nalgebra::Matrix4::<f64>::zeros();
    let mut atb = nalgebra::Vector4::<f64>::zeros();
    for (x, y, t, q) in &tiles {
        let (xx, yy) = ((x - c0x) / w as f64, (y - c0y) / w as f64);
        for (row, b) in [([xx, -yy, 1.0, 0.0], t[0]), ([yy, xx, 0.0, 1.0], t[1])] {
            let r = nalgebra::Vector4::from(row);
            ata += r * r.transpose() * *q;
            atb += r * (b * *q);
        }
    }
    let p = ata.try_inverse()? * atb;
    let (sc, rot) = (p[0] / w as f64, p[1] / w as f64);
    let d = nalgebra::Matrix2::new(sc, -rot, rot, sc);
    let td = nalgebra::Vector2::new(p[2], p[3]) - d * nalgebra::Vector2::new(c0x, c0y);
    let a = nalgebra::Matrix2::new(warp.a[0], warp.a[1], warp.a[2], warp.a[3]);
    let a2 = a * (nalgebra::Matrix2::identity() + d);
    let t2 = a * td + nalgebra::Vector2::new(warp.t[0], warp.t[1]);
    Some(LensWarp {
        a: [a2[(0, 0)], a2[(0, 1)], a2[(1, 0)], a2[(1, 1)]],
        t: [t2[0], t2[1]],
        ..*warp
    })
}

/// Median of frame/reference luminance where both are well exposed.
fn measure_ratio(y: &Plane, u: &Plane, ref_y: &Plane, ref_u: &Plane) -> Option<f32> {
    let mut ratios: Vec<f32> = y
        .data
        .iter()
        .zip(&u.data)
        .zip(ref_y.data.iter().zip(&ref_u.data))
        .filter(|((yv, uv), (ry, ru))| {
            **uv > 0.99 && **ru > 0.99 && **yv > 0.02 && **ry > 0.02 && **yv < 0.7 && **ry < 0.7
        })
        .map(|((yv, _), (ry, _))| yv / ry)
        .collect();
    if ratios.len() < 500 {
        return None;
    }
    let mid = ratios.len() / 2;
    let (_, m, _) = ratios.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    Some(*m)
}

/// The running merge: full-resolution sums (reference units), the low-res
/// consensus of the frames merged so far that the next frame is compared
/// with, and the ghost bookkeeping. Frames are added reference first, then
/// outwards by exposure (`merge_hdr`).
struct Accumulator {
    w: usize,
    h: usize,
    /// Low-res cell size (`low_res`).
    f: usize,
    to_camera: Matrix3<f32>,
    noise_gain: [f32; 3],
    /// Ghost removal's threshold; `None` = off.
    threshold_ev: Option<f32>,
    num: Vec<f32>,
    den: Vec<f32>,
    cons_rgb: [Plane; 3],
    cons_den: Plane,
    /// Cells where a frame had samples dropped at full resolution that
    /// would otherwise have merged (`update_consensus`).
    touched: Vec<bool>,
    ghost_union: Option<Plane>,
    /// With ghost removal (set after the reference is merged): where the
    /// reference is clipped, only frames that agree it is that bright are
    /// merged; where it is bright, only frames that agree with it per pixel
    /// (`reference_state`).
    ref_state: Option<Vec<f32>>,
}

impl Accumulator {
    fn new(
        w: usize,
        h: usize,
        f: usize,
        to_camera: Matrix3<f32>,
        threshold_ev: Option<f32>,
    ) -> Self {
        let (lw, lh) = (w.div_ceil(f), h.div_ceil(f));
        Self {
            w,
            h,
            f,
            to_camera,
            noise_gain: channel_noise_gain(&to_camera),
            threshold_ev,
            num: vec![0.0; w * h * 3],
            den: vec![0.0; w * h],
            cons_rgb: std::array::from_fn(|_| Plane::new(lw, lh)),
            cons_den: Plane::new(lw, lh),
            touched: vec![false; lw * lh],
            ghost_union: threshold_ev.map(|_| Plane::new(lw, lh)),
            ref_state: None,
        }
    }

    /// Merges one frame (`low` = `low_res` of `view`; `ratio` = its exposure
    /// against the reference) and returns the share of it dropped as ghosts.
    fn add(
        &mut self,
        view: &View,
        low: &LowRes,
        ratio: f32,
        is_ref: bool,
        darkest: bool,
        last: bool,
    ) -> f64 {
        let (wu, hu, f) = (self.w, self.h, self.f);
        let (lw, lh) = (self.cons_den.w, self.cons_den.h);
        let to_camera = self.to_camera;
        // Ghosts: disagreement with the consensus where both are usable.
        let ghost = match (self.threshold_ev, is_ref) {
            (Some(t), false) => Some(ghost_map(
                low,
                &self.cons_rgb,
                &self.cons_den,
                ratio,
                t,
                self.noise_gain,
            )),
            _ => None,
        };

        // Accumulate (reference units).
        let floor = if is_ref {
            1e-6
        } else if darkest {
            1e-4
        } else {
            0.0
        };
        let inv_ratio = 1.0 / ratio;
        let ghost_ev = self.threshold_ev.unwrap_or(f32::INFINITY);
        let fx = lw as f32 / wu as f32;
        let fy = lh as f32 / hu as f32;
        let ref_state = &self.ref_state;
        // Per row: the bright-reference rejections summed per low-res
        // column, for the ghost overlay and the frame's ghost share, and the
        // low-res columns where a sample that would have merged was dropped
        // (either test), for the consensus.
        let rows: Vec<(Vec<f32>, Vec<usize>)> = self
            .num
            .par_chunks_mut(wu * 3)
            .zip(self.den.par_chunks_mut(wu))
            .enumerate()
            .map(|(y, (nrow, drow))| {
                let mut rejected = Vec::new();
                let mut dropped: Vec<usize> = Vec::new();
                let mut drop_at = |x: usize| {
                    let lx = x / f;
                    if dropped.last() != Some(&lx) {
                        dropped.push(lx);
                    }
                };
                for x in 0..wu {
                    let Some(p) = view.at(x, y) else {
                        continue;
                    };
                    let mut wgt = usable(&p, &to_camera);
                    let ref_px = ref_state.as_ref().map_or(0.0, |s| s[y * wu + x]);
                    if ref_px == f32::INFINITY {
                        // A moving highlight (sun glints on water): a frame
                        // darker than the reference's clipping point there
                        // shows another moment, and with the reference
                        // left out it made dark holes in the glints. The
                        // ghost map does not apply: agreeing frames are the
                        // only data; with none, the clipped reference stays.
                        let cam = to_camera * nalgebra::Vector3::new(p[0], p[1], p[2]);
                        if cam.max() * inv_ratio < 0.9 * SATURATION_START {
                            if wgt > 0.0 {
                                drop_at(x);
                            }
                            continue;
                        }
                    } else {
                        let r = bright_rejection(ref_px, luma(&p) * inv_ratio, ghost_ev);
                        // Counted as far as the pixel would have merged
                        // (a clipped pixel of a brighter frame is no loss).
                        let coarse = ghost.as_ref().map_or(0.0, |g| {
                            g.sample_bilinear(
                                (x as f32 + 0.5) * fx - 0.5,
                                (y as f32 + 0.5) * fy - 0.5,
                            )
                        });
                        if r > 0.0 && wgt > 0.0 {
                            // Both tests together, for the overlay.
                            if rejected.is_empty() {
                                rejected = vec![0.0f32; lw];
                            }
                            rejected[(x * lw / wu).min(lw - 1)] +=
                                wgt * (1.0 - (1.0 - r) * (1.0 - coarse));
                            drop_at(x);
                        }
                        if r >= 1.0 {
                            continue;
                        }
                        wgt *= (1.0 - r) * (1.0 - coarse);
                    }
                    let wgt = ratio * wgt.max(floor);
                    if wgt <= 0.0 {
                        continue;
                    }
                    nrow[x * 3] += wgt * p[0] * inv_ratio;
                    nrow[x * 3 + 1] += wgt * p[1] * inv_ratio;
                    nrow[x * 3 + 2] += wgt * p[2] * inv_ratio;
                    drow[x] += wgt;
                }
                (rejected, dropped)
            })
            .collect();
        for (y, (_, dropped)) in rows.iter().enumerate() {
            let ly = y / f;
            for &lx in dropped {
                self.touched[ly * lw + lx] = true;
            }
        }
        let rejected: Vec<Vec<f32>> = rows.into_iter().map(|(r, _)| r).collect();
        // The ghost map as it acted: the coarse map, or the share of a
        // low-res cell's usable weight dropped where the bright reference
        // test took part (both tests combined there).
        let ghost = ghost.map(|mut g| {
            let mut sums = Plane::new(lw, lh);
            for (y, row) in rejected.iter().enumerate() {
                let ly = (y * lh / hu).min(lh - 1);
                for (lx, v) in row.iter().enumerate() {
                    sums.data[ly * lw + lx] += v;
                }
            }
            for ly in 0..lh {
                let rows = (ly * hu).div_ceil(lh)..((ly + 1) * hu).div_ceil(lh);
                for lx in 0..lw {
                    let cols = (lx * wu).div_ceil(lw)..((lx + 1) * wu).div_ceil(lw);
                    let n = (rows.len() * cols.len()).max(1) as f32;
                    let i = ly * lw + lx;
                    g.data[i] = g.data[i].max((sums.data[i] / n).min(1.0));
                }
            }
            g
        });
        drop(rejected);
        let ghost_fraction = ghost.as_ref().map(|g| g.mean()).unwrap_or(0.0);
        if let (Some(g), Some(union)) = (&ghost, &mut self.ghost_union) {
            for (a, b) in union.data.iter_mut().zip(&g.data) {
                *a = a.max(*b);
            }
        }
        // Consensus update at low resolution (none after the last frame).
        if !last {
            update_consensus(
                &mut self.cons_rgb,
                &mut self.cons_den,
                low,
                ghost.as_ref(),
                ratio,
                &self.touched,
                &self.num,
                &self.den,
                (wu, hu, f),
            );
        }
        if is_ref && self.threshold_ev.is_some() {
            self.ref_state = Some(
                view.raw
                    .par_chunks_exact(3)
                    .map(|p| reference_state(p, &to_camera))
                    .collect(),
            );
        }
        ghost_fraction
    }

    /// The merged image (interleaved RGB, replacing the sums in place) and
    /// where any frame was dropped as a ghost.
    fn finish(self) -> (Vec<f32>, Option<Plane>) {
        let mut num = self.num;
        num.par_chunks_mut(3)
            .zip(self.den.par_iter())
            .for_each(|(n, d)| {
                let d = d.max(1e-12);
                n[0] /= d;
                n[1] /= d;
                n[2] /= d;
            });
        (num, self.ghost_union)
    }
}

/// Adds a merged frame to the low-res consensus the next frames are compared
/// with. Each cell gains the frame's box average, weighted by its usable
/// share and by how much of it was not a ghost (`ratio` × u × (1 − g)).
/// In `touched` cells, where samples that would have merged were dropped at
/// full resolution (bright-reference test, clipped-reference rule), the
/// box average would still carry the dropped samples' colour (a glint that
/// moved, the wrong moment's water). There the consensus colour becomes the
/// box average of the full-resolution merge so far (`num`/`den`): every
/// sample counts with the weight it merged with, dropped ones not at all,
/// and the cell keeps its whole support, so the next frame's whole-cell
/// average is still compared like for like (excluding the dropped samples
/// from the average alone left the clipped or the dark half of a cell
/// standing for all of it: notes: benchmark-results/mrg3-2026-11-23,
/// mrg5-2026-11-24). Untouched cells, all of them in a bracket without
/// motion, are updated as before.
#[allow(clippy::too_many_arguments)]
fn update_consensus(
    cons_rgb: &mut [Plane; 3],
    cons_den: &mut Plane,
    low: &LowRes,
    ghost: Option<&Plane>,
    ratio: f32,
    touched: &[bool],
    num: &[f32],
    den: &[f32],
    (w, h, f): (usize, usize, usize),
) {
    let (lw, lh) = (cons_den.w, cons_den.h);
    let inv_ratio = 1.0 / ratio;
    for i in 0..lw * lh {
        let mut wgt = low.u.data[i];
        if let Some(g) = ghost {
            wgt *= 1.0 - g.data[i];
        }
        let wgt = ratio * wgt;
        for (cons, plane) in cons_rgb.iter_mut().zip(&low.rgb) {
            cons.data[i] += wgt * plane.data[i] * inv_ratio;
        }
        cons_den.data[i] += wgt;
    }
    // Same cells as `low_res` (f × f, the last ones cut).
    let means: Vec<(usize, [f32; 3])> = (0..lw * lh)
        .into_par_iter()
        .filter(|&i| touched[i])
        .filter_map(|i| {
            let (lx, ly) = (i % lw, i / lw);
            let mut sum = [0.0f32; 3];
            let mut n = 0.0f32;
            for y in ly * f..((ly + 1) * f).min(h) {
                for x in lx * f..((lx + 1) * f).min(w) {
                    let d = den[y * w + x];
                    if d > 0.0 {
                        for (c, s) in sum.iter_mut().enumerate() {
                            *s += num[(y * w + x) * 3 + c] / d;
                        }
                        n += 1.0;
                    }
                }
            }
            (n > 0.0).then(|| (i, sum.map(|v| v / n)))
        })
        .collect();
    for (i, mean) in means {
        for (cons, v) in cons_rgb.iter_mut().zip(mean) {
            cons.data[i] = v * cons_den.data[i];
        }
    }
}

/// Merges bracketed exposures (at least two, same camera orientation).
pub fn merge_hdr(
    paths: &[PathBuf],
    options: &BaseImageOptions,
    config: &HdrConfig,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<HdrResult> {
    ensure!(paths.len() >= 2, "Select at least two photos to merge.");
    let captures: Vec<Capture> = paths.iter().map(|p| read_capture(p)).collect();
    let evs: Vec<Option<f64>> = captures.iter().map(Capture::exposure_ev).collect();
    let mut by_ev: Vec<usize> = (0..paths.len()).collect();
    by_ev.sort_by(|&a, &b| evs[a].unwrap_or(0.0).total_cmp(&evs[b].unwrap_or(0.0)));
    let reference = by_ev[(by_ev.len() - 1) / 2];
    let ref_ev = evs[reference];
    let darkest = by_ev[0];
    // The reference first, then outwards by exposure.
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|&a, &b| {
        let d = |i: usize| match (evs[i], ref_ev) {
            (Some(e), Some(r)) => (e - r).abs(),
            _ => 0.0,
        };
        (a != reference)
            .cmp(&(b != reference))
            .then(d(a).total_cmp(&d(b)))
    });

    let name = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    progress(&format!("Loading {}", name(&paths[reference])));
    let ref_frame: Frame = load_frame(&paths[reference], options)?;
    let (w, h) = ref_frame.image.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let to_camera = working_to_camera(ref_frame.color.as_ref());
    let f = wu.max(hu).div_ceil(LOW_RES_TARGET).max(1);
    let align_f = wu.max(hu).div_ceil(ALIGN_TARGET).max(1);

    let ref_low = low_res(&View::new(&ref_frame.image, None), &to_camera, f);
    let (ref_y, ref_u) = (ref_low.y, ref_low.u);
    let ref_align = config
        .align
        .then(|| align_plane(&ref_frame.image, 1.0, align_f));
    let ref_tiles = if config.align {
        reference_tiles(&ref_frame.image)
    } else {
        Vec::new()
    };
    let mut acc = Accumulator::new(wu, hu, f, to_camera, config.deghost.threshold_ev());
    let mut reports: Vec<Option<HdrFrameReport>> = vec![None; paths.len()];

    let mut ref_frame = Some(ref_frame);
    let mut ref_meta = None;
    for (position, &index) in order.iter().enumerate() {
        let is_ref = index == reference;
        let is_last = position + 1 == order.len();
        let mut frame = if is_ref {
            ref_frame
                .take()
                .ok_or_else(|| anyhow!("reference frame used twice"))?
        } else {
            progress(&format!("Loading {}", name(&paths[index])));
            let frame = load_frame(&paths[index], options)?;
            ensure!(
                frame.image.dimensions() == (w, h),
                "{} is {}×{}, {} is {}×{}: bracketed photos must have the same size and orientation",
                name(&paths[reference]),
                w,
                h,
                name(&paths[index]),
                frame.image.width(),
                frame.image.height()
            );
            frame
        };
        let ref_color = ref_meta
            .as_ref()
            .map(|(c, _): &(Option<ImageColor>, _)| c.as_ref())
            .unwrap_or(frame.color.as_ref());
        if !is_ref && let Some(m) = rebalance(frame.color.as_ref(), ref_color) {
            apply_matrix(&mut frame.image, &m);
        }

        // Exposure ratio: EXIF first, measured on the aligned frame.
        let exif_ratio = match (evs[index], ref_ev) {
            (Some(e), Some(r)) => Some(2f64.powf(e - r) as f32),
            _ => None,
        };
        let mut ratio = 1.0f32;
        let mut shift_px = 0.0;
        let mut align_failed = false;
        let mut warp_used: Option<LensWarp> = None;
        if !is_ref {
            ratio = match exif_ratio {
                Some(r) => r,
                None => {
                    let low = low_res(&View::new(&frame.image, None), &to_camera, f);
                    measure_ratio(&low.y, &low.u, &ref_y, &ref_u).unwrap_or(1.0)
                }
            };
            if let Some(ref_align) = &ref_align {
                progress(&format!("Aligning {}", name(&paths[index])));
                let src = align_plane(&frame.image, ratio, align_f);
                // Brackets differ by camera motion only: rotation, shift and a
                // little perspective, no focus breathing or lens change.
                let cfg = AlignConfig {
                    finest_dim: ALIGN_TARGET,
                    max_model: crate::align::WarpModel::Similarity,
                    ..AlignConfig::default()
                };
                let pyramids = build_align_pyramids(ref_align, &src, &cfg);
                let pose = solve_alignment(
                    &pyramids,
                    LensWarp::identity(ref_align.w, ref_align.h),
                    &cfg,
                );
                let mut warp = pose.warp.rescaled(align_f as f64);
                if let Some(refined) = refine_full_res(&ref_tiles, &frame.image, ratio, &warp) {
                    warp = refined;
                }
                let displacement = warp.max_displacement(wu, hu);
                log::info!(
                    "HDR: {} aligned: a {:?} t {:?} k1 {:.2e} quality {:.3}",
                    name(&paths[index]),
                    warp.a,
                    warp.t,
                    warp.k1,
                    pose.quality
                );
                if pose.quality < cfg.min_quality
                    || !warp.is_plausible()
                    || displacement > 0.08 * wu.max(hu) as f64
                {
                    align_failed = true;
                    log::warn!(
                        "HDR: alignment of {} failed (quality {:.2}, {:.1} px)",
                        name(&paths[index]),
                        pose.quality,
                        displacement
                    );
                } else if displacement > 0.25 {
                    warp_used = Some(warp);
                    shift_px = displacement;
                }
            }
        }
        let view = View::new(&frame.image, warp_used);
        let low = low_res(&view, &to_camera, f);
        if !is_ref && let Some(measured) = measure_ratio(&low.y, &low.u, &ref_y, &ref_u) {
            ratio = measured;
        }

        progress(&format!("Merging {}", name(&paths[index])));
        let ghost_fraction = acc.add(&view, &low, ratio, is_ref, index == darkest, is_last);

        reports[index] = Some(HdrFrameReport {
            path: paths[index].clone(),
            exif_ev: evs[index],
            relative_ev: (ratio as f64).log2(),
            shift_px,
            align_failed,
            ghost_fraction,
        });
        if is_ref {
            ref_meta = Some((frame.color.clone(), frame.metadata.clone()));
        }
    }

    progress("Finishing");
    let (num, ghost_union) = acc.finish();
    let image = Rgb32FImage::from_raw(w, h, num).ok_or_else(|| anyhow!("merge buffer size"))?;
    let (color, metadata) = ref_meta.ok_or_else(|| anyhow!("reference frame missing"))?;
    Ok(HdrResult {
        image,
        reference,
        reference_path: paths[reference].clone(),
        color,
        metadata,
        frames: reports
            .into_iter()
            .map(|r| r.expect("every frame merged"))
            .collect(),
        ghost_mask: ghost_union,
    })
}

/// A small sRGB preview of a scene-linear image with a soft highlight
/// roll-off (the real rendering is the develop pipeline's).
pub fn preview(image: &Rgb32FImage, max_dim: u32) -> RgbImage {
    let (w, h) = image.dimensions();
    let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
    let (pw, ph) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let raw: &[f32] = image;
    let mut out = RgbImage::new(pw, ph);
    let step_x = w as f32 / pw as f32;
    let step_y = h as f32 / ph as f32;
    out.enumerate_pixels_mut().for_each(|(x, y, px)| {
        let (x0, y0) = ((x as f32 * step_x) as u32, (y as f32 * step_y) as u32);
        let (x1, y1) = (
            ((x + 1) as f32 * step_x) as u32,
            ((y + 1) as f32 * step_y) as u32,
        );
        let mut s = [0.0f32; 3];
        let mut n = 0.0;
        for yy in y0..y1.max(y0 + 1).min(h) {
            for xx in x0..x1.max(x0 + 1).min(w) {
                let p = &raw[((yy * w + xx) * 3) as usize..][..3];
                s[0] += p[0];
                s[1] += p[1];
                s[2] += p[2];
                n += 1.0;
            }
        }
        let tone = |v: f32| {
            let v = (v / n).max(0.0) * 1.4;
            let v = v / (1.0 + v / 2.5);
            let v = v.min(1.0);
            let e = if v <= 0.0031308 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (e * 255.0).round().clamp(0.0, 255.0) as u8
        };
        *px = image::Rgb([tone(s[0]), tone(s[1]), tone(s[2])]);
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A low-res frame of `w`×`h` filled with `rgb`, fully usable.
    fn flat(w: usize, h: usize, rgb: [f32; 3]) -> LowRes {
        frame_of(rgb.map(|v| Plane::filled(w, h, v)))
    }

    fn frame_of(rgb: [Plane; 3]) -> LowRes {
        let (w, h) = (rgb[0].w, rgb[0].h);
        let mut y = Plane::new(w, h);
        for (i, o) in y.data.iter_mut().enumerate() {
            *o = luma(&[rgb[0].data[i], rgb[1].data[i], rgb[2].data[i]]);
        }
        LowRes {
            rgb,
            y,
            u: Plane::filled(w, h, 1.0),
        }
    }

    /// A red object moving through a +2 EV frame over grey-green ground: in
    /// the reference units it is 4.5× brighter in red but only 1.5× in
    /// luminance (0.58 EV, between Medium's 0.4 and 0.8 EV steps). It must
    /// be dropped whole, and the matching ground kept.
    #[test]
    fn coloured_mover_is_a_ghost_in_its_own_channel() {
        let (w, h) = (48, 48);
        let ground = [0.067f32, 0.069, 0.078];
        let ratio = 4.0;
        let cons_rgb = ground.map(|v| Plane::filled(w, h, v));
        let cons_den = Plane::filled(w, h, 1.0);
        let mut rgb = ground.map(|v| Plane::filled(w, h, v * ratio));
        for y in 16..32 {
            for x in 16..32 {
                for (c, v) in [1.2f32, 0.2, 0.16].into_iter().enumerate() {
                    rgb[c].data[y * w + x] = v;
                }
            }
        }
        let frame = frame_of(rgb);
        let g = ghost_map(
            &frame,
            &cons_rgb,
            &cons_den,
            ratio,
            Deghost::Medium.threshold_ev().unwrap(),
            [1.0; 3],
        );
        assert!(g.at(24, 24) > 0.95, "mover kept: ghost {}", g.at(24, 24));
        assert!(g.at(2, 2) < 0.01, "ground dropped: ghost {}", g.at(2, 2));
    }

    /// A still, saturated subject (deep green leaves) whose weak red channel
    /// reads 0.002 in one frame and 0 in the other is no ghost, even at High.
    #[test]
    fn noise_in_a_weak_channel_is_no_ghost() {
        let (w, h) = (32, 32);
        let cons = [0.002f32, 0.18, 0.01];
        let cons_rgb = cons.map(|v| Plane::filled(w, h, v));
        let cons_den = Plane::filled(w, h, 1.0);
        let frame = flat(w, h, [0.0, 0.18, 0.012]);
        let g = ghost_map(
            &frame,
            &cons_rgb,
            &cons_den,
            1.0,
            Deghost::High.threshold_ev().unwrap(),
            [1.0; 3],
        );
        assert!(g.data.iter().all(|&v| v < 0.01), "ghost {}", g.at(16, 16));
    }

    /// The same scene seen at another exposure is no ghost in any channel.
    #[test]
    fn matching_frame_is_no_ghost() {
        let (w, h) = (32, 32);
        let scene = [0.30f32, 0.05, 0.04];
        let ratio = 0.25;
        let cons_rgb = scene.map(|v| Plane::filled(w, h, v));
        let cons_den = Plane::filled(w, h, 1.0);
        let frame = flat(w, h, scene.map(|v| v * ratio * 1.05));
        let g = ghost_map(&frame, &cons_rgb, &cons_den, ratio, 0.22, [1.0; 3]);
        assert!(g.data.iter().all(|&v| v < 0.01));
    }

    /// A channel the camera matrix amplifies more carries more noise.
    #[test]
    fn channel_noise_gain_follows_the_matrix() {
        let g = channel_noise_gain(&Matrix3::identity());
        assert!((g[0] - g[1]).abs() < 1e-6 && (g[1] - g[2]).abs() < 1e-6);
        // to_camera halves blue: camera → working doubles it.
        let g = channel_noise_gain(&Matrix3::from_diagonal(&nalgebra::Vector3::new(
            1.0, 1.0, 0.5,
        )));
        assert!((g[2] / g[0] - 2.0).abs() < 1e-5, "{g:?}");
    }

    /// Deep shadow of a static bracket's darkest frame (−2 EV): its blue
    /// channel is off by sensor noise that the matrix amplified. With one
    /// floor for every channel this is a ghost at High; with the channel's
    /// own noise floor it is not, while a real change of that size in
    /// luminance stays a ghost.
    #[test]
    fn amplified_channel_noise_is_no_ghost() {
        let (w, h) = (32, 32);
        let ratio = 0.25;
        let scene = [0.006f32, 0.006, 0.006];
        let cons_rgb = scene.map(|v| Plane::filled(w, h, v));
        let cons_den = Plane::filled(w, h, 1.0);
        let gain = [1.6, 1.4, 2.7];
        let noisy = flat(w, h, [0.006 * ratio, 0.006 * ratio, 0.0]);
        let high = Deghost::High.threshold_ev().unwrap();
        let single = ghost_map(&noisy, &cons_rgb, &cons_den, ratio, high, [1.0; 3]);
        assert!(single.at(16, 16) > 0.5, "premise: {}", single.at(16, 16));
        let own = ghost_map(&noisy, &cons_rgb, &cons_den, ratio, high, gain);
        assert!(
            own.at(16, 16) < 0.01,
            "noise is a ghost: {}",
            own.at(16, 16)
        );
        let moved = flat(w, h, [0.03 * ratio, 0.03 * ratio, 0.03 * ratio]);
        let g = ghost_map(&moved, &cons_rgb, &cons_den, ratio, high, gain);
        assert!(g.at(16, 16) > 0.95, "change kept: {}", g.at(16, 16));
    }

    /// Runs the merge's per-frame step on in-memory frames, the first one
    /// the reference, each with its exposure against it; identity camera
    /// matrix, sensor white at 1. Returns the accumulator before the last
    /// frame (`None` when there is one frame), its ghost shares and the
    /// merged image.
    fn run(
        frames: &[(Rgb32FImage, f32)],
        f: usize,
        deghost: Deghost,
    ) -> (Option<Accumulator>, Vec<f64>, Vec<f32>) {
        let (w, h) = frames[0].0.dimensions();
        let id = Matrix3::identity();
        let mut acc = Accumulator::new(w as usize, h as usize, f, id, deghost.threshold_ev());
        let darkest = (0..frames.len())
            .min_by(|&a, &b| frames[a].1.total_cmp(&frames[b].1))
            .unwrap();
        let mut ghosts = Vec::new();
        let mut before_last = None;
        for (k, (image, ratio)) in frames.iter().enumerate() {
            let last = k + 1 == frames.len();
            if last {
                before_last = Some(clone_acc(&acc));
            }
            let view = View::new(image, None);
            let low = low_res(&view, &id, f);
            ghosts.push(acc.add(&view, &low, *ratio, k == 0, k == darkest, last));
        }
        let (image, _) = acc.finish();
        (before_last.filter(|_| frames.len() > 1), ghosts, image)
    }

    fn clone_acc(a: &Accumulator) -> Accumulator {
        Accumulator {
            num: a.num.clone(),
            den: a.den.clone(),
            cons_rgb: a.cons_rgb.clone(),
            cons_den: a.cons_den.clone(),
            touched: a.touched.clone(),
            ghost_union: a.ghost_union.clone(),
            ref_state: a.ref_state.clone(),
            ..*a
        }
    }

    /// A scene as a frame `ratio` × as bright, clipped at the sensor white.
    fn exposed(w: u32, h: u32, scene: impl Fn(u32, u32) -> [f32; 3], ratio: f32) -> Rgb32FImage {
        Rgb32FImage::from_fn(w, h, |x, y| {
            image::Rgb(scene(x, y).map(|v| (v * ratio).min(1.0)))
        })
    }

    /// Sun glints on dark water that moved between a reference and a −1 EV
    /// frame. Where the reference glints, the frame shows dark water and is
    /// dropped pixel by pixel (bright-reference test); its glints moved to
    /// pixels the reference shows dark and the cell averages agree, so the
    /// coarse ghost map keeps the frame. The consensus the next frame is
    /// compared with must be what was merged — the dropped dark samples
    /// out, every pixel of the cell in — not the frame's whole-cell
    /// average, which still carried the dropped samples.
    #[test]
    fn dropped_samples_stay_out_of_the_consensus() {
        let (w, h, f) = (64u32, 64u32, 8usize);
        let water = [0.03f32, 0.05, 0.08];
        let glint = |x: u32, y: u32, phase: u32| (x * 7 + y * 3 + phase).is_multiple_of(11);
        let at =
            |phase: u32| move |x: u32, y: u32| if glint(x, y, phase) { [0.5; 3] } else { water };
        let frames = [
            (exposed(w, h, at(0), 1.0), 1.0),
            (exposed(w, h, at(5), 0.5), 0.5),
            (exposed(w, h, at(0), 2.0), 2.0),
        ];
        let (acc, _, _) = run(&frames, f, Deghost::Medium);
        let acc = acc.unwrap();
        let (lw, lh) = (acc.cons_den.w, acc.cons_den.h);
        let mut checked = 0;
        for ly in 1..lh - 1 {
            for lx in 1..lw - 1 {
                let i = ly * lw + lx;
                assert!(acc.touched[i], "cell {lx},{ly} not touched");
                // The full-resolution merge so far, averaged over the cell.
                let mut mean = [0.0f32; 3];
                for y in ly * f..(ly + 1) * f {
                    for x in lx * f..(lx + 1) * f {
                        let p = y * w as usize + x;
                        for (c, m) in mean.iter_mut().enumerate() {
                            *m += acc.num[p * 3 + c] / acc.den[p] / (f * f) as f32;
                        }
                    }
                }
                for (c, m) in mean.iter().enumerate() {
                    let k = acc.cons_rgb[c].data[i] / acc.cons_den.data[i];
                    assert!(
                        (k / m).log2().abs() < 0.01,
                        "cell {lx},{ly} channel {c}: consensus {k} vs merged {m}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    /// MRG3 review round 1: in a −2/0/+1 EV bracket without motion, the +1
    /// frame's bright half is clipped. Those samples are no rejection (they
    /// would not have merged), so the cells stay as before and the −2 frame
    /// finds no ghosts; the merge is the scene.
    #[test]
    fn clipped_half_of_a_still_cell_is_no_ghost() {
        let (w, h, f) = (64u32, 64u32, 8usize);
        let scene = |x: u32, _y: u32| {
            if x.is_multiple_of(2) {
                [0.6f32, 0.55, 0.5]
            } else {
                [0.02, 0.025, 0.03]
            }
        };
        let frames = [
            (exposed(w, h, scene, 1.0), 1.0),
            (exposed(w, h, scene, 2.0), 2.0),
            (exposed(w, h, scene, 0.25), 0.25),
        ];
        for deghost in [Deghost::Low, Deghost::Medium, Deghost::High] {
            let (acc, ghosts, image) = run(&frames, f, deghost);
            assert!(!acc.unwrap().touched.iter().any(|&t| t), "{deghost:?}");
            assert!(ghosts.iter().all(|&g| g < 1e-6), "{deghost:?}: {ghosts:?}");
            for (k, v) in image.iter().enumerate() {
                let p = k / 3;
                let truth = scene((p % w as usize) as u32, (p / w as usize) as u32)[k % 3];
                assert!(
                    (v / truth).log2().abs() < 1e-4,
                    "{deghost:?} px {p}: {v} vs {truth}"
                );
            }
        }
    }

    /// MRG3 review round 2: fine foliage over shadow, the +1 EV frame
    /// shifted by a pixel. Its dark samples fail the bright test where the
    /// reference is bright and its bright ones are clipped, so nothing of
    /// it merges in those cells. The −2 EV frame, which matches the
    /// reference, must not be taken for a ghost (55 % of such a patch was
    /// with the consensus colour cut to the surviving samples alone).
    #[test]
    fn shifted_foliage_does_not_ghost_the_matching_frame() {
        let (w, h, f) = (64u32, 64u32, 8usize);
        let leaf = |x: u32| {
            if x.is_multiple_of(2) {
                [0.45f32, 0.5, 0.3]
            } else {
                [0.02, 0.025, 0.015]
            }
        };
        let frames = [
            (exposed(w, h, |x, _| leaf(x), 1.0), 1.0),
            (exposed(w, h, |x, _| leaf(x + 1), 2.0), 2.0),
            (exposed(w, h, |x, _| leaf(x), 0.25), 0.25),
        ];
        for deghost in [Deghost::Low, Deghost::Medium, Deghost::High] {
            let (_, ghosts, _) = run(&frames, f, deghost);
            assert!(
                ghosts[2] < 0.01,
                "{deghost:?}: −2 EV frame ghost share {}",
                ghosts[2]
            );
        }
    }

    /// Clipped reference pixels, bright ones that judge other frames, and
    /// darker ones left to the ghost map.
    #[test]
    fn reference_state_classifies_pixels() {
        let id = Matrix3::identity();
        assert_eq!(reference_state(&[0.9, 0.3, 0.3], &id), f32::INFINITY);
        let l = reference_state(&[0.5, 0.5, 0.5], &id);
        assert!((l - 0.5).abs() < 1e-6);
        assert_eq!(reference_state(&[0.1, 0.1, 0.1], &id), 0.0);
    }

    /// The bright-reference test fades in with the reference's brightness
    /// (no step where a smooth reflection crosses `BRIGHT_REFERENCE`) and
    /// drops a frame that disagrees whole, as the clipped-reference rule
    /// does (no step where it reaches saturation).
    #[test]
    fn bright_rejection_is_continuous() {
        let t = Deghost::Medium.threshold_ev().unwrap();
        // 26 % darker (0.43 EV) just below and just above BRIGHT_REFERENCE.
        let below = bright_rejection(0.199, 0.199 * 0.74, t);
        let above = bright_rejection(0.201, 0.201 * 0.74, t);
        assert!((above - below).abs() < 0.01, "{below} {above}");
        assert!(
            above > 0.95,
            "a 26 % darker frame at a bright reference: {above}"
        );
        assert_eq!(bright_rejection(0.0, 0.05, t), 0.0);
        assert!(bright_rejection(0.11, 0.0, t) < 0.05);
        assert_eq!(bright_rejection(0.5, 0.05, t), 1.0);
        assert_eq!(bright_rejection(0.5, 0.5 * 1.05, t), 0.0);
        assert_eq!(bright_rejection(0.5, 0.05, f32::INFINITY), 0.0);
        // Near saturation it drops a darker frame whole, as the clipped-
        // reference rule does just above.
        assert_eq!(bright_rejection(0.799, 0.799 * 0.74, t), 1.0);
    }
}
