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
//!   things come from one frame only.
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

/// Box-averaged luminance and usability at 1/`f` size.
fn low_res(view: &View, to_camera: &Matrix3<f32>, f: usize) -> (Plane, Plane) {
    let (w, h) = (view.w, view.h);
    let (lw, lh) = (w.div_ceil(f), h.div_ceil(f));
    let rows: Vec<(Vec<f32>, Vec<f32>)> = (0..lh)
        .into_par_iter()
        .map(|ly| {
            let mut y_row = vec![0.0f32; lw];
            let mut u_row = vec![0.0f32; lw];
            for (lx, (yo, uo)) in y_row.iter_mut().zip(u_row.iter_mut()).enumerate() {
                let (mut sy, mut su, mut n) = (0.0f32, 0.0f32, 0.0f32);
                for y in ly * f..((ly + 1) * f).min(h) {
                    for x in lx * f..((lx + 1) * f).min(w) {
                        // Pixels without a source count as black, unusable.
                        if let Some(p) = view.at(x, y) {
                            sy += luma(&p);
                            su += usable(&p, to_camera);
                        }
                        n += 1.0;
                    }
                }
                *yo = sy / n;
                *uo = su / n;
            }
            (y_row, u_row)
        })
        .collect();
    let mut y = Plane::new(lw, lh);
    let mut u = Plane::new(lw, lh);
    for (ly, (yr, ur)) in rows.into_iter().enumerate() {
        y.data[ly * lw..(ly + 1) * lw].copy_from_slice(&yr);
        u.data[ly * lw..(ly + 1) * lw].copy_from_slice(&ur);
    }
    (y, u)
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

    let (ref_y, ref_u) = low_res(&View::new(&ref_frame.image, None), &to_camera, f);
    let ref_align = config
        .align
        .then(|| align_plane(&ref_frame.image, 1.0, align_f));
    let ref_tiles = if config.align {
        reference_tiles(&ref_frame.image)
    } else {
        Vec::new()
    };
    let (lw, lh) = (ref_y.w, ref_y.h);

    let mut num = vec![0.0f32; wu * hu * 3];
    let mut den = vec![0.0f32; wu * hu];
    // Low-res consensus luminance of the frames merged so far.
    let mut cons_num = Plane::new(lw, lh);
    let mut cons_den = Plane::new(lw, lh);
    let mut ghost_union = config.deghost.threshold_ev().map(|_| Plane::new(lw, lh));
    // With ghost removal: where the reference is clipped (set after it is
    // merged), only frames that agree it is that bright are merged.
    let mut ref_clipped: Option<Vec<bool>> = None;
    let mut reports: Vec<Option<HdrFrameReport>> = vec![None; paths.len()];

    let mut ref_frame = Some(ref_frame);
    let mut ref_meta = None;
    for &index in &order {
        let is_ref = index == reference;
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
                    let (y, u) = low_res(&View::new(&frame.image, None), &to_camera, f);
                    measure_ratio(&y, &u, &ref_y, &ref_u).unwrap_or(1.0)
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
        let (low_y, low_u) = low_res(&view, &to_camera, f);
        if !is_ref && let Some(measured) = measure_ratio(&low_y, &low_u, &ref_y, &ref_u) {
            ratio = measured;
        }

        // Ghosts: disagreement with the consensus where both are usable.
        let ghost = match (config.deghost.threshold_ev(), is_ref) {
            (Some(t), false) => {
                let (y, u) = (&low_y, &low_u);
                let eps = NOISE_FLOOR * (1.0f32).max(1.0 / ratio);
                let mut g = Plane::new(lw, lh);
                g.data.par_iter_mut().enumerate().for_each(|(i, g)| {
                    let cd = cons_den.data[i];
                    if u.data[i] < 0.9 || cd <= 0.0 {
                        return;
                    }
                    let c = cons_num.data[i] / cd;
                    let e = ((y.data[i] / ratio + eps) / (c + eps)).log2().abs();
                    *g = smoothstep(t, 2.0 * t, e);
                });
                let g = gaussian_blur(&max_filter(&g, 3), 3.0).map(|v| v.clamp(0.0, 1.0));
                Some(g)
            }
            _ => None,
        };
        let ghost_fraction = ghost.as_ref().map(|g| g.mean()).unwrap_or(0.0);
        if let (Some(g), Some(union)) = (&ghost, &mut ghost_union) {
            for (a, b) in union.data.iter_mut().zip(&g.data) {
                *a = a.max(*b);
            }
        }

        // Accumulate (reference units).
        progress(&format!("Merging {}", name(&paths[index])));
        let floor = if is_ref {
            1e-6
        } else if index == darkest {
            1e-4
        } else {
            0.0
        };
        let inv_ratio = 1.0 / ratio;
        let fx = lw as f32 / wu as f32;
        let fy = lh as f32 / hu as f32;
        num.par_chunks_mut(wu * 3)
            .zip(den.par_chunks_mut(wu))
            .enumerate()
            .for_each(|(y, (nrow, drow))| {
                for x in 0..wu {
                    let Some(p) = view.at(x, y) else {
                        continue;
                    };
                    let mut wgt = usable(&p, &to_camera);
                    let clipped_ref = ref_clipped.as_ref().is_some_and(|c| c[y * wu + x]);
                    if clipped_ref {
                        // A moving highlight (sun glints on water): a frame
                        // darker than the reference's clipping point there
                        // shows another moment, and with the reference
                        // left out it made dark holes in the glints. The
                        // ghost map does not apply: agreeing frames are the
                        // only data; with none, the clipped reference stays.
                        let cam = to_camera * nalgebra::Vector3::new(p[0], p[1], p[2]);
                        if cam.max() * inv_ratio < 0.9 * SATURATION_START {
                            continue;
                        }
                    } else if let Some(g) = &ghost {
                        wgt *= 1.0
                            - g.sample_bilinear(
                                (x as f32 + 0.5) * fx - 0.5,
                                (y as f32 + 0.5) * fy - 0.5,
                            );
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
            });
        // Consensus update at low resolution.
        let (y, u) = (&low_y, &low_u);
        for i in 0..lw * lh {
            let mut wgt = u.data[i];
            if let Some(g) = &ghost {
                wgt *= 1.0 - g.data[i];
            }
            let wgt = ratio * wgt;
            cons_num.data[i] += wgt * y.data[i] * inv_ratio;
            cons_den.data[i] += wgt;
        }

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
            if config.deghost.threshold_ev().is_some() {
                // Clipped or nearly.
                let raw: &[f32] = &frame.image;
                ref_clipped = Some(
                    raw.par_chunks_exact(3)
                        .map(|p| usable(p, &to_camera) < 1.0)
                        .collect(),
                );
            }
        }
    }

    progress("Finishing");
    // The result replaces the sums in place.
    num.par_chunks_mut(3)
        .zip(den.par_iter())
        .for_each(|(n, d)| {
            let d = d.max(1e-12);
            n[0] /= d;
            n[1] /= d;
            n[2] /= d;
        });
    drop(den);
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
