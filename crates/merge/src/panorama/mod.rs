//! Panorama stitching of overlapping RAWs into one scene-linear image.
//!
//! - Frames are developed linear, lens-corrected with their own profile
//!   (distortion, vignetting, lateral CA) and brought to the reference
//!   frame's white balance.
//! - Keypoints (features.rs) are matched between frames, registered with
//!   RANSAC and solved globally as rotating pinhole cameras with one focal
//!   length (geometry.rs), then levelled.
//! - Exposure differences are solved globally from the matched points.
//! - The frames are projected (spherical, cylindrical, perspective or
//!   Panini; automatic by field of view) and blended in two bands: fine
//!   detail from the frame that owns a pixel (seams through the middle of
//!   overlaps), brightness and colour from all overlapping frames smoothly.
//! - Boundary warp stretches the irregular outline towards the rectangle;
//!   auto crop takes the largest rectangle inside.
pub mod features;
pub mod geometry;

use crate::frame::load_corrected;
use crate::planes::{Plane, gaussian_blur};
use anyhow::{Result, anyhow, ensure};
use geometry::{Camera, PairMatches};
use image::Rgb32FImage;
use nalgebra::{Vector2, Vector3};
use rawler::decoders::RawMetadata;
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_rawdecode::raw_processing::ImageColor;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Projection {
    #[default]
    Auto,
    Spherical,
    Cylindrical,
    Perspective,
    Panini,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoramaConfig {
    pub projection: Projection,
    /// 0–100: how far the outline is stretched towards the rectangle.
    pub boundary_warp: f32,
    pub auto_crop: bool,
}

impl Default for PanoramaConfig {
    fn default() -> Self {
        PanoramaConfig {
            projection: Projection::Auto,
            boundary_warp: 0.0,
            auto_crop: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoramaFrameReport {
    pub path: PathBuf,
    pub used: bool,
    /// Exposure correction applied (EV).
    pub gain_ev: f64,
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub roll_deg: f64,
}

pub struct PanoramaResult {
    /// The projected canvas before boundary warp and crop (`finish`), so
    /// that those can change without stitching again.
    pub image: Rgb32FImage,
    /// Canvas pixels some frame covers.
    pub covered: Vec<bool>,
    pub projection: Projection,
    pub reference_path: PathBuf,
    pub color: Option<ImageColor>,
    pub metadata: Option<RawMetadata>,
    pub frames: Vec<PanoramaFrameReport>,
    /// RMS registration error after bundle adjustment (pixels).
    pub rms_px: f64,
    /// Horizontal and vertical field of view (degrees).
    pub fov_deg: (f64, f64),
}

const FEATURE_DIM: usize = 1600;
const MAX_KEYPOINTS: usize = 2500;
/// Largest output (megapixels); larger panoramas are rendered smaller.
/// Low-band sums are kept at 1/LOW_Q size.
const LOW_Q: usize = 4;
const MAX_OUTPUT_MP: f64 = 400.0;

fn luma(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// Box-averaged luminance at most `dim` pixels on the long side, and the
/// factor to full size.
fn small_luma(image: &Rgb32FImage, dim: usize) -> (Plane, f64) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let f = w.max(h).div_ceil(dim).max(1);
    let (sw, sh) = (w / f, h / f);
    let raw: &[f32] = image;
    let mut out = Plane::new(sw, sh);
    out.data
        .par_chunks_mut(sw)
        .enumerate()
        .for_each(|(sy, row)| {
            for (sx, o) in row.iter_mut().enumerate() {
                let mut s = 0.0;
                for y in sy * f..(sy + 1) * f {
                    for x in sx * f..(sx + 1) * f {
                        s += luma(&raw[(y * w + x) * 3..][..3]);
                    }
                }
                *o = s / (f * f) as f32;
            }
        });
    (out, f as f64)
}

struct Registered {
    keypoints: Vec<features::Keypoint>,
    luma: Plane,
    scale: f64,
    size: (usize, usize),
    color: Option<ImageColor>,
    metadata: Option<RawMetadata>,
}

/// Projection of a world ray to panorama coordinates (focal-length units)
/// and back.
pub fn project(p: Projection, d: &Vector3<f64>) -> Option<(f64, f64)> {
    let (x, y, z) = (d.x, d.y, d.z);
    let r = (x * x + z * z).sqrt();
    match p {
        Projection::Spherical | Projection::Auto => Some((x.atan2(z), y.atan2(r))),
        Projection::Cylindrical => (r > 1e-9).then(|| (x.atan2(z), y / r)),
        Projection::Perspective => (z > 1e-6).then(|| (x / z, y / z)),
        Projection::Panini => {
            let t = x.atan2(z);
            (t.abs() < 0.97 * std::f64::consts::PI && r > 1e-9).then(|| {
                let s = 2.0 / (1.0 + t.cos());
                (s * t.sin(), s * y / r)
            })
        }
    }
}

pub fn unproject(p: Projection, u: f64, v: f64) -> Option<Vector3<f64>> {
    match p {
        Projection::Spherical | Projection::Auto => (v.abs() <= std::f64::consts::FRAC_PI_2)
            .then(|| Vector3::new(u.sin() * v.cos(), v.sin(), u.cos() * v.cos())),
        Projection::Cylindrical => Some(Vector3::new(u.sin(), v, u.cos())),
        Projection::Perspective => Some(Vector3::new(u, v, 1.0)),
        Projection::Panini => {
            let t = 2.0 * (u / 2.0).atan();
            let s = 2.0 / (1.0 + t.cos());
            Some(Vector3::new(t.sin(), v / s, t.cos()))
        }
    }
}

/// Bicubic sample of an interleaved RGB image; `None` outside.
fn sample_rgb(raw: &[f32], w: usize, h: usize, x: f64, y: f64) -> Option<[f32; 3]> {
    if x < 0.0 || y < 0.0 || x > (w - 1) as f64 || y > (h - 1) as f64 {
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
    for (j, wyj) in wy.iter().enumerate() {
        let yy = (y0 - 1 + j as i64).clamp(0, h as i64 - 1) as usize;
        for (i, wxi) in wx.iter().enumerate() {
            let xx = (x0 - 1 + i as i64).clamp(0, w as i64 - 1) as usize;
            let p = &raw[(yy * w + xx) * 3..][..3];
            let wgt = wyj * wxi;
            out[0] += p[0] * wgt;
            out[1] += p[1] * wgt;
            out[2] += p[2] * wgt;
        }
    }
    Some(out)
}

/// Feather weight of a frame pixel: 1 in the middle, 0 at the border.
fn feather(x: f64, y: f64, w: f64, h: f64) -> f64 {
    let dx = (x.min(w - 1.0 - x) / (0.5 * w)).clamp(0.0, 1.0);
    let dy = (y.min(h - 1.0 - y) / (0.5 * h)).clamp(0.0, 1.0);
    dx * dy
}

/// Blending domain: cube root keeps negative (wide-gamut) channels and
/// compresses highlights so seams in bright skies stay invisible.
#[inline]
fn to_blend(v: f32) -> f32 {
    v.cbrt()
}
#[inline]
fn from_blend(v: f32) -> f32 {
    v * v * v
}

/// Largest axis-aligned rectangle of `true` cells: (x, y, w, h).
fn largest_rectangle(mask: &[bool], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let mut heights = vec![0usize; w];
    let mut best = (0, 0, 0, 0, 0usize);
    for y in 0..h {
        for x in 0..w {
            heights[x] = if mask[y * w + x] { heights[x] + 1 } else { 0 };
        }
        let mut stack: Vec<usize> = Vec::new();
        for x in 0..=w {
            let cur = if x < w { heights[x] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= cur {
                    break;
                }
                stack.pop();
                let height = heights[top];
                let left = stack.last().map_or(0, |&l| l + 1);
                let area = height * (x - left);
                if area > best.4 {
                    best = (left, y + 1 - height, x - left, height, area);
                }
            }
            stack.push(x);
        }
    }
    (best.0, best.1, best.2, best.3)
}

/// Stitches overlapping photos (at least two).
pub fn stitch(
    paths: &[PathBuf],
    options: &BaseImageOptions,
    config: &PanoramaConfig,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<PanoramaResult> {
    let load =
        |i: usize, reference: Option<&ImageColor>| load_corrected(&paths[i], options, reference);
    stitch_frames(paths, &load, config, progress)
}

/// `stitch` over frames from any source: `load(i, reference)` returns frame
/// `i` lens-corrected and balanced to `reference` (it is called twice per
/// frame: small for registration, then full for rendering).
pub fn stitch_frames(
    paths: &[PathBuf],
    load: &(dyn Fn(usize, Option<&ImageColor>) -> Result<crate::frame::Frame> + Sync),
    config: &PanoramaConfig,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<PanoramaResult> {
    ensure!(paths.len() >= 2, "Select at least two photos to stitch.");
    let n = paths.len();
    let name = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };

    // Pass 1: keypoints on small copies. The first frame's balance is the
    // working balance until the reference is known.
    let mut first_color: Option<ImageColor> = None;
    let mut registered: Vec<Registered> = Vec::with_capacity(n);
    for (index, path) in paths.iter().enumerate() {
        progress(&format!("Analysing {}", name(path)));
        let frame = load(index, first_color.as_ref())?;
        if first_color.is_none() {
            first_color = frame.color.clone();
        }
        let (small, scale) = small_luma(&frame.image, FEATURE_DIM);
        let keypoints = features::detect(&small, MAX_KEYPOINTS)
            .into_iter()
            .map(|mut k| {
                k.x = ((k.x as f64 + 0.5) * scale - 0.5) as f32;
                k.y = ((k.y as f64 + 0.5) * scale - 0.5) as f32;
                k
            })
            .collect();
        registered.push(Registered {
            keypoints,
            luma: small,
            scale,
            size: (frame.image.width() as usize, frame.image.height() as usize),
            color: frame.color.clone(),
            metadata: frame.metadata.clone(),
        });
    }

    // Matching: all pairs for short series, neighbours plus wrap-around for
    // long ones.
    progress("Matching");
    let candidates: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .filter(|&(a, b)| n <= 16 || b - a <= 3 || (a == 0 && b == n - 1))
        .collect();
    let pairs: Vec<PairMatches> = candidates
        .par_iter()
        .filter_map(|&(a, b)| {
            let (ra, rb) = (&registered[a], &registered[b]);
            let m = features::match_keypoints(&ra.keypoints, &rb.keypoints, 0.8);
            let pts: Vec<(Vector2<f64>, Vector2<f64>)> = m
                .iter()
                .map(|&(i, j)| {
                    let (p, q) = (&ra.keypoints[i], &rb.keypoints[j]);
                    (
                        Vector2::new(p.x as f64, p.y as f64),
                        Vector2::new(q.x as f64, q.y as f64),
                    )
                })
                .collect();
            let threshold = 2.5 * ra.scale.max(rb.scale);
            let (h, inliers) = geometry::ransac_homography(&pts, threshold, 1500)?;
            // Brown & Lowe's acceptance test for a true overlap.
            if (inliers.len() as f64) < 8.0 + 0.3 * pts.len() as f64 || inliers.len() < 20 {
                return None;
            }
            Some(PairMatches {
                a,
                b,
                points: inliers.iter().map(|&i| pts[i]).collect(),
                homography: h,
            })
        })
        .collect();
    ensure!(
        !pairs.is_empty(),
        "The photos do not overlap enough to be stitched."
    );

    // The frame with the most matches roots the chain.
    let mut degree = vec![0usize; n];
    for p in &pairs {
        degree[p.a] += p.points.len();
        degree[p.b] += p.points.len();
    }
    let root = (0..n).max_by_key(|&i| degree[i]).unwrap_or(0);
    let centers: Vec<(f64, f64)> = registered
        .iter()
        .map(|r| ((r.size.0 as f64 - 1.0) / 2.0, (r.size.1 as f64 - 1.0) / 2.0))
        .collect();
    let diag = registered
        .iter()
        .map(|r| ((r.size.0 * r.size.0 + r.size.1 * r.size.1) as f64).sqrt())
        .fold(0.0, f64::max);
    let focal = geometry::estimate_focal(&pairs, &centers, diag);
    let rotations = geometry::chain_rotations(n, &pairs, focal, &centers, root);
    let active: Vec<bool> = rotations.iter().map(Option::is_some).collect();
    ensure!(
        active.iter().filter(|a| **a).count() >= 2,
        "The photos do not overlap enough to be stitched."
    );
    let mut cameras: Vec<Camera> = (0..n)
        .map(|i| Camera {
            rotation: rotations[i].unwrap_or_else(nalgebra::Rotation3::identity),
            focal,
            cx: centers[i].0,
            cy: centers[i].1,
        })
        .collect();
    progress("Aligning");
    let rms_px = geometry::bundle_adjust(&mut cameras, &pairs, root, &active);
    geometry::level(&mut cameras, &active);
    log::info!(
        "Panorama: focal {:.1} px (initial {:.1}), RMS {:.2} px, {} pairs",
        cameras[root].focal,
        focal,
        rms_px,
        pairs.len()
    );

    // Reference: the frame nearest the middle of the view.
    let reference = (0..n)
        .filter(|&i| active[i])
        .min_by(|&a, &b| {
            let fa = cameras[a].rotation.inverse() * Vector3::z();
            let fb = cameras[b].rotation.inverse() * Vector3::z();
            fb.z.total_cmp(&fa.z)
        })
        .unwrap_or(root);

    // Exposure: one gain per frame from the matched points' luminance.
    let gains = solve_gains(&pairs, &registered, &active, reference);

    // Field of view and projection.
    let all_rays: Vec<Vector3<f64>> = (0..n)
        .filter(|&i| active[i])
        .flat_map(|i| {
            let (w, h) = registered[i].size;
            let c = cameras[i];
            (0..=16).flat_map(move |k| {
                let t = k as f64 / 16.0;
                let (wf, hf) = ((w - 1) as f64, (h - 1) as f64);
                [
                    c.ray(t * wf, 0.0),
                    c.ray(t * wf, hf),
                    c.ray(0.0, t * hf),
                    c.ray(wf, t * hf),
                ]
            })
        })
        .collect();
    let extent = |p: Projection| -> Option<(f64, f64, f64, f64)> {
        let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for r in &all_rays {
            let (u, v) = project(p, r)?;
            b = (b.0.min(u), b.1.min(v), b.2.max(u), b.3.max(v));
        }
        Some(b)
    };
    let sph = extent(Projection::Spherical).ok_or_else(|| anyhow!("Panorama has no extent"))?;
    let fov = ((sph.2 - sph.0).to_degrees(), (sph.3 - sph.1).to_degrees());
    let projection = match config.projection {
        Projection::Auto => {
            if fov.0 < 100.0 && fov.1 < 75.0 {
                Projection::Perspective
            } else if fov.1 < 90.0 {
                Projection::Cylindrical
            } else {
                Projection::Spherical
            }
        }
        p => p,
    };
    let mut bounds = extent(projection).ok_or_else(|| {
        anyhow!("This projection cannot show the whole panorama; choose spherical or cylindrical.")
    })?;
    if projection == Projection::Perspective
        && ((bounds.2 - bounds.0) > 6.0 || (bounds.3 - bounds.1) > 6.0)
    {
        return Err(anyhow!(
            "The panorama is too wide for the perspective projection; choose spherical, cylindrical or Panini."
        ));
    }
    // Output scale: the frames' own resolution in the middle.
    let mut scale = cameras[reference].focal;
    let area = (bounds.2 - bounds.0) * (bounds.3 - bounds.1) * scale * scale / 1e6;
    if area > MAX_OUTPUT_MP {
        scale *= (MAX_OUTPUT_MP / area).sqrt();
    }
    bounds = (bounds.0, bounds.1, bounds.2, bounds.3);
    let out_w = ((bounds.2 - bounds.0) * scale).ceil() as usize + 1;
    let out_h = ((bounds.3 - bounds.1) * scale).ceil() as usize + 1;
    let to_uv = |x: f64, y: f64| (bounds.0 + x / scale, bounds.1 + y / scale);

    // Ownership (seams) and blend weights on a coarse canvas.
    let coarse_f = (out_w.max(out_h) / 1200).max(1);
    let (cw, ch) = (out_w.div_ceil(coarse_f), out_h.div_ceil(coarse_f));
    // Per frame on the coarse canvas: feather weight and log luminance
    // (exposure-matched) for the seams.
    let coarse: Vec<(Vec<f32>, Vec<f32>)> = (0..n)
        .into_par_iter()
        .map(|i| {
            let mut wts = vec![0.0f32; cw * ch];
            let mut lum = vec![f32::NAN; cw * ch];
            if !active[i] {
                return (wts, lum);
            }
            let r = &registered[i];
            let (w, h) = r.size;
            let cam = cameras[i];
            let gain = gains[i] as f32;
            for y in 0..ch {
                for x in 0..cw {
                    let (u, v) = to_uv(
                        ((x * coarse_f) as f64) + 0.5 * coarse_f as f64,
                        ((y * coarse_f) as f64) + 0.5 * coarse_f as f64,
                    );
                    if let Some(px) = unproject(projection, u, v).and_then(|r| cam.project(&r)) {
                        let k = y * cw + x;
                        wts[k] = feather(px.0, px.1, w as f64, h as f64) as f32;
                        if wts[k] > 0.0 {
                            let sx = ((px.0 + 0.5) / r.scale - 0.5) as f32;
                            let sy = ((px.1 + 0.5) / r.scale - 0.5) as f32;
                            lum[k] =
                                (r.luma.sample_bilinear(sx, sy).max(0.0) * gain + 0.002).log2();
                        }
                    }
                }
            }
            (wts, lum)
        })
        .collect();
    let (coarse_weights, coarse_luma): (Vec<Vec<f32>>, Vec<Vec<f32>>) = coarse.into_iter().unzip();
    let owner: Vec<u16> = (0..cw * ch)
        .into_par_iter()
        .map(|k| {
            let (best, w) = (0..n)
                .map(|i| (i, coarse_weights[i][k]))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            if w > 0.0 { best as u16 } else { u16::MAX }
        })
        .collect();
    let owner = optimize_seams(owner, &coarse_weights, &coarse_luma, cw, ch);
    // Smooth low-band weights: ownership blurred over ~1/10 of a frame, kept
    // inside each frame.
    let blur_sigma = (registered
        .iter()
        .map(|r| r.size.0.min(r.size.1))
        .min()
        .unwrap_or(1000) as f64
        * cameras[reference].focal.recip()
        * scale
        / coarse_f as f64
        * 0.08)
        .max(1.0) as f32;
    let low_weights: Vec<Plane> = (0..n)
        .into_par_iter()
        .map(|i| {
            let mut p = Plane::new(cw, ch);
            if !active[i] {
                return p;
            }
            for (d, o) in p.data.iter_mut().zip(&owner) {
                *d = if *o == i as u16 { 1.0 } else { 0.0 };
            }
            let mut b = gaussian_blur(&p, blur_sigma);
            // Inside the frame only, with a soft edge.
            for (d, w) in b.data.iter_mut().zip(&coarse_weights[i]) {
                *d *= (w * 8.0).min(1.0);
            }
            b
        })
        .collect();

    // Pass 2: render each frame into the canvas.
    let reference_color = registered[reference].color.clone();
    let px = out_w * out_h;
    // The low band is smooth (block means at 1/8, blurred): its weighted
    // sums are kept at 1/4 size and interpolated back (full-size sums were
    // 1.4 GB for a 90 MP canvas).
    let (qw, qh) = (out_w.div_ceil(LOW_Q), out_h.div_ceil(LOW_Q));
    let mut low_num = vec![0.0f32; qw * qh * 3];
    let mut low_w = vec![0.0f32; qw * qh];
    let mut high = vec![0.0f32; px * 3];
    // Detail weights: ownership interpolated between the coarse cells, so a
    // seam crosses over about one cell instead of stepping cell by cell (the
    // steps showed wherever parallax kept the frames from lining up); a
    // trace for frames covering pixels that no owner covers.
    let mut high_w = vec![0.0f32; px];
    let owner_weight = |i: usize, xf: f32, yf: f32| -> f32 {
        let (x0f, y0f) = (xf.floor(), yf.floor());
        let (tx, ty) = (xf - x0f, yf - y0f);
        let mut acc = 0.0f32;
        for (dy, wy) in [(0i64, 1.0 - ty), (1, ty)] {
            for (dx, wx) in [(0i64, 1.0 - tx), (1, tx)] {
                let cx = (x0f as i64 + dx).clamp(0, cw as i64 - 1) as usize;
                let cy = (y0f as i64 + dy).clamp(0, ch as i64 - 1) as usize;
                if owner[cy * cw + cx] == i as u16 {
                    acc += wx * wy;
                }
            }
        }
        acc
    };
    let low_f = 8usize;
    for i in 0..n {
        if !active[i] {
            continue;
        }
        progress(&format!("Rendering {}", name(&paths[i])));
        let frame = load(i, reference_color.as_ref())?;
        let (w, h) = (frame.image.width() as usize, frame.image.height() as usize);
        let src: &[f32] = &frame.image;
        let cam = cameras[i];
        let gain = gains[i] as f32;
        // Bounding box of the frame on the canvas.
        let mut bb = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..ch {
            for x in 0..cw {
                if coarse_weights[i][y * cw + x] > 0.0 {
                    bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
                }
            }
        }
        if bb.0 == usize::MAX {
            continue;
        }
        let x0 = (bb.0.saturating_sub(1)) * coarse_f;
        let y0 = (bb.1.saturating_sub(1)) * coarse_f;
        let x1 = ((bb.2 + 2) * coarse_f).min(out_w);
        let y1 = ((bb.3 + 2) * coarse_f).min(out_h);
        let (bw, bh) = (x1 - x0, y1 - y0);
        // Warped frame in the blend domain, NaN outside.
        let mut warped = vec![f32::NAN; bw * bh * 3];
        warped
            .par_chunks_mut(bw * 3)
            .enumerate()
            .for_each(|(by, row)| {
                for bx in 0..bw {
                    let (u, v) = to_uv((x0 + bx) as f64, (y0 + by) as f64);
                    let Some(p) = unproject(projection, u, v)
                        .and_then(|r| cam.project(&r))
                        .and_then(|(sx, sy)| sample_rgb(src, w, h, sx, sy))
                    else {
                        continue;
                    };
                    for c in 0..3 {
                        row[bx * 3 + c] = to_blend(p[c] * gain);
                    }
                }
            });
        drop(frame);
        // Low band: block means at 1/low_f, blurred, sampled back.
        let (lw, lh) = (bw.div_ceil(low_f), bh.div_ceil(low_f));
        let mut low_planes: Vec<Plane> = (0..3).map(|_| Plane::new(lw, lh)).collect();
        let mut low_cnt = Plane::new(lw, lh);
        for by in 0..bh {
            for bx in 0..bw {
                let v = &warped[(by * bw + bx) * 3..][..3];
                if v[0].is_nan() {
                    continue;
                }
                let k = (by / low_f) * lw + bx / low_f;
                for c in 0..3 {
                    low_planes[c].data[k] += v[c];
                }
                low_cnt.data[k] += 1.0;
            }
        }
        let cnt_blur = gaussian_blur(&low_cnt, 2.0);
        let low_planes: Vec<Plane> = low_planes
            .iter()
            .map(|p| {
                let b = gaussian_blur(p, 2.0);
                let mut o = Plane::new(lw, lh);
                for k in 0..lw * lh {
                    o.data[k] = if cnt_blur.data[k] > 1e-3 {
                        b.data[k] / cnt_blur.data[k]
                    } else {
                        0.0
                    };
                }
                o
            })
            .collect();
        let lwts = &low_weights[i];
        // Low band: weighted sums per LOW_Q×LOW_Q block.
        let (qy0, qy1) = (y0 / LOW_Q, y1.div_ceil(LOW_Q));
        low_num[qy0 * qw * 3..qy1 * qw * 3]
            .par_chunks_mut(qw * 3)
            .zip(low_w[qy0 * qw..qy1 * qw].par_chunks_mut(qw))
            .enumerate()
            .for_each(|(qi, (ln, lwr))| {
                let qy = qy0 + qi;
                for y in (qy * LOW_Q).max(y0)..((qy + 1) * LOW_Q).min(y1) {
                    let by = y - y0;
                    for bx in 0..bw {
                        let x = x0 + bx;
                        let v = &warped[(by * bw + bx) * 3..][..3];
                        if v[0].is_nan() {
                            continue;
                        }
                        let lx = (bx as f32 + 0.5) / low_f as f32 - 0.5;
                        let ly = (by as f32 + 0.5) / low_f as f32 - 0.5;
                        let cxf = (x as f32 + 0.5) / coarse_f as f32 - 0.5;
                        let cyf = (y as f32 + 0.5) / coarse_f as f32 - 0.5;
                        let wgt = lwts.sample_bilinear(cxf, cyf).max(1e-6);
                        let q = x / LOW_Q;
                        for c in 0..3 {
                            ln[q * 3 + c] += wgt * low_planes[c].sample_bilinear(lx, ly);
                        }
                        lwr[q] += wgt;
                    }
                }
            });
        // Detail: the frame minus its low band, weighted by ownership.
        let rows_high = high[y0 * out_w * 3..y1 * out_w * 3].par_chunks_mut(out_w * 3);
        let rows_cov = high_w[y0 * out_w..y1 * out_w].par_chunks_mut(out_w);
        rows_high
            .zip(rows_cov)
            .enumerate()
            .for_each(|(by, (hr, cr))| {
                let y = y0 + by;
                for bx in 0..bw {
                    let x = x0 + bx;
                    let v = &warped[(by * bw + bx) * 3..][..3];
                    if v[0].is_nan() {
                        continue;
                    }
                    let lx = (bx as f32 + 0.5) / low_f as f32 - 0.5;
                    let ly = (by as f32 + 0.5) / low_f as f32 - 0.5;
                    let cxf = (x as f32 + 0.5) / coarse_f as f32 - 0.5;
                    let cyf = (y as f32 + 0.5) / coarse_f as f32 - 0.5;
                    let hw = owner_weight(i, cxf, cyf) + 1e-4;
                    for c in 0..3 {
                        hr[x * 3 + c] += hw * (v[c] - low_planes[c].sample_bilinear(lx, ly));
                    }
                    cr[x] += hw;
                }
            });
    }

    progress("Finishing");
    // The result replaces the detail sums in place.
    let sample_q = |plane: &[f32], stride: usize, c: usize, xf: f32, yf: f32| -> f32 {
        let (x0f, y0f) = (xf.floor(), yf.floor());
        let (tx, ty) = (xf - x0f, yf - y0f);
        let mut acc = 0.0f32;
        for (dy, wy) in [(0i64, 1.0 - ty), (1, ty)] {
            for (dx, wx) in [(0i64, 1.0 - tx), (1, tx)] {
                let qx = (x0f as i64 + dx).clamp(0, qw as i64 - 1) as usize;
                let qy = (y0f as i64 + dy).clamp(0, qh as i64 - 1) as usize;
                acc += wx * wy * plane[(qy * qw + qx) * stride + c];
            }
        }
        acc
    };
    high.par_chunks_mut(out_w * 3)
        .zip(high_w.par_chunks(out_w))
        .enumerate()
        .for_each(|(y, (row, hw_row))| {
            let qyf = (y as f32 + 0.5) / LOW_Q as f32 - 0.5;
            for x in 0..out_w {
                let o = &mut row[x * 3..x * 3 + 3];
                let hw = hw_row[x];
                if hw <= 0.0 {
                    o.fill(0.0);
                    continue;
                }
                let qxf = (x as f32 + 0.5) / LOW_Q as f32 - 0.5;
                let lw = sample_q(&low_w, 1, 0, qxf, qyf);
                for (c, v) in o.iter_mut().enumerate() {
                    let low = if lw > 0.0 {
                        sample_q(&low_num, 3, c, qxf, qyf) / lw
                    } else {
                        0.0
                    };
                    *v = from_blend(low + *v / hw);
                }
            }
        });
    drop((low_num, low_w));
    let image = Rgb32FImage::from_raw(out_w as u32, out_h as u32, high)
        .ok_or_else(|| anyhow!("panorama buffer size"))?;
    let covered: Vec<bool> = high_w.par_iter().map(|w| *w > 0.0).collect();
    drop(high_w);

    let frames = (0..n)
        .map(|i| {
            let fwd = cameras[i].rotation.inverse() * Vector3::z();
            let right = cameras[i].rotation.inverse() * Vector3::x();
            PanoramaFrameReport {
                path: paths[i].clone(),
                used: active[i],
                gain_ev: gains[i].log2(),
                yaw_deg: fwd.x.atan2(fwd.z).to_degrees(),
                pitch_deg: -fwd.y.asin().to_degrees(),
                roll_deg: right.y.asin().to_degrees(),
            }
        })
        .collect();
    Ok(PanoramaResult {
        image,
        covered,
        projection,
        reference_path: paths[reference].clone(),
        color: registered[reference].color.clone(),
        metadata: registered[reference].metadata.clone(),
        frames,
        rms_px,
        fov_deg: fov,
    })
}

/// Moves the seams between frames to where they agree. For each pair of
/// frames that share an overlap, the seam through the pixels they own
/// there is the cheapest path (dynamic programming, one crossing per row
/// for frames side by side, per column for frames above each other), where
/// a pixel costs how much the two frames differ (moving people, parallax)
/// plus a little per seam length. Cutting around a moving object is a
/// global choice, which local label updates do not find.
fn optimize_seams(
    owner: Vec<u16>,
    weights: &[Vec<f32>],
    luma: &[Vec<f32>],
    w: usize,
    h: usize,
) -> Vec<u16> {
    const LENGTH: f32 = 0.05;
    const STEP: usize = 2;
    let n = weights.len();
    let mut labels = owner;
    // Frame centres on the canvas (weight-averaged) decide which side is which.
    let centre: Vec<Option<(f64, f64)>> = (0..n)
        .map(|i| {
            let (mut sx, mut sy, mut s) = (0.0, 0.0, 0.0);
            for (k, &wt) in weights[i].iter().enumerate() {
                if wt > 0.0 {
                    sx += (k % w) as f64 * wt as f64;
                    sy += (k / w) as f64 * wt as f64;
                    s += wt as f64;
                }
            }
            (s > 0.0).then(|| (sx / s, sy / s))
        })
        .collect();
    let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
    for a in 0..n {
        for b in a + 1..n {
            let overlap = (0..w * h)
                .filter(|&k| weights[a][k] > 0.0 && weights[b][k] > 0.0)
                .count();
            if overlap > 64 {
                pairs.push((a, b, overlap));
            }
        }
    }
    pairs.sort_by_key(|p| std::cmp::Reverse(p.2));
    for (a, b, _) in pairs {
        let (Some(ca), Some(cb)) = (centre[a], centre[b]) else {
            continue;
        };
        let vertical_seam = (cb.0 - ca.0).abs() >= (cb.1 - ca.1).abs();
        // In seam coordinates `line` runs along the seam and `pos` across it;
        // `first` is the frame on the low side of `pos`.
        let (lines, span) = if vertical_seam { (h, w) } else { (w, h) };
        let a_low = if vertical_seam {
            ca.0 <= cb.0
        } else {
            ca.1 <= cb.1
        };
        let (first, second) = if a_low { (a, b) } else { (b, a) };
        let index = |line: usize, pos: usize| {
            if vertical_seam {
                line * w + pos
            } else {
                pos * w + line
            }
        };
        let in_region = |labels: &[u16], k: usize| {
            weights[a][k] > 0.0
                && weights[b][k] > 0.0
                && (labels[k] == a as u16 || labels[k] == b as u16)
        };
        let ranges: Vec<Option<(usize, usize)>> = (0..lines)
            .map(|line| {
                let mut lo = None;
                let mut hi = None;
                for pos in 0..span {
                    if in_region(&labels, index(line, pos)) {
                        lo.get_or_insert(pos);
                        hi = Some(pos);
                    }
                }
                lo.zip(hi)
            })
            .collect();
        let cost = |line: usize, pos: usize| -> f32 {
            let k = index(line, pos);
            let (x, y) = (luma[a][k], luma[b][k]);
            if x.is_nan() || y.is_nan() {
                4.0
            } else {
                (x - y).abs().min(4.0) + LENGTH
            }
        };
        // Each run of consecutive lines with a region gets its own seam.
        let mut line = 0;
        while line < lines {
            if ranges[line].is_none() {
                line += 1;
                continue;
            }
            let start = line;
            while line < lines && ranges[line].is_some() {
                line += 1;
            }
            let end = line;
            let mut acc: Vec<f32> = vec![f32::INFINITY; span];
            let mut back: Vec<Vec<u32>> = Vec::with_capacity(end - start);
            let (lo, hi) = ranges[start].unwrap();
            for (pos, v) in acc.iter_mut().enumerate().take(hi + 1).skip(lo) {
                *v = cost(start, pos);
            }
            back.push(vec![0; span]);
            for (l, range) in ranges.iter().enumerate().take(end).skip(start + 1) {
                let (lo, hi) = range.unwrap();
                let mut next = vec![f32::INFINITY; span];
                let mut from = vec![0u32; span];
                for pos in lo..=hi {
                    let (p0, p1) = (pos.saturating_sub(STEP), (pos + STEP).min(span - 1));
                    let (mut best, mut arg) = (f32::INFINITY, pos);
                    for (q, &v) in acc.iter().enumerate().take(p1 + 1).skip(p0) {
                        let v = v + LENGTH * q.abs_diff(pos) as f32;
                        if v < best {
                            best = v;
                            arg = q;
                        }
                    }
                    if best.is_finite() {
                        next[pos] = best + cost(l, pos);
                        from[pos] = arg as u32;
                    }
                }
                acc = next;
                back.push(from);
            }
            let Some((mut pos, _)) = acc
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .min_by(|x, y| x.1.total_cmp(y.1))
            else {
                continue;
            };
            // Trace back; relabel the region on each side of the seam.
            for l in (start..end).rev() {
                let (lo, hi) = ranges[l].unwrap();
                for p in lo..=hi {
                    let k = index(l, p);
                    if in_region(&labels, k) {
                        labels[k] = if p < pos { first as u16 } else { second as u16 };
                    }
                }
                if l > start {
                    pos = back[l - start][pos] as usize;
                }
            }
        }
    }
    labels
}

/// Per-frame gains from the luminance at matched points (log least squares,
/// the reference fixed at 1).
fn solve_gains(
    pairs: &[PairMatches],
    registered: &[Registered],
    active: &[bool],
    reference: usize,
) -> Vec<f64> {
    let n = registered.len();
    let sample = |r: &Registered, p: &Vector2<f64>| -> Option<f32> {
        let (x, y) = (
            ((p.x + 0.5) / r.scale - 0.5) as i64,
            ((p.y + 0.5) / r.scale - 0.5) as i64,
        );
        let mut s = 0.0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                if x + dx < 0
                    || y + dy < 0
                    || x + dx >= r.luma.w as i64
                    || y + dy >= r.luma.h as i64
                {
                    return None;
                }
                s += r.luma.at((x + dx) as usize, (y + dy) as usize);
            }
        }
        let v = s / 9.0;
        (v > 0.01 && v < 0.6).then_some(v)
    };
    let mut rows: Vec<(usize, usize, f64, f64)> = Vec::new();
    for p in pairs.iter().filter(|p| active[p.a] && active[p.b]) {
        let mut ratios: Vec<f64> = p
            .points
            .iter()
            .filter_map(|(pa, pb)| {
                Some(
                    (sample(&registered[p.b], pb)? as f64 / sample(&registered[p.a], pa)? as f64)
                        .ln(),
                )
            })
            .collect();
        if ratios.len() < 10 {
            continue;
        }
        ratios.sort_by(|a, b| a.total_cmp(b));
        rows.push((p.a, p.b, ratios[ratios.len() / 2], ratios.len() as f64));
    }
    // log g_a + log Y_a = log g_b + log Y_b  →  log g_a − log g_b = r_ab
    let mut ata = nalgebra::DMatrix::<f64>::zeros(n, n);
    let mut atb = nalgebra::DVector::<f64>::zeros(n);
    for &(a, b, r, w) in &rows {
        let w = w.sqrt();
        ata[(a, a)] += w;
        ata[(b, b)] += w;
        ata[(a, b)] -= w;
        ata[(b, a)] -= w;
        atb[a] += w * r;
        atb[b] -= w * r;
    }
    let prior = 1e-3;
    for i in 0..n {
        ata[(i, i)] += if i == reference { 1e6 } else { prior };
    }
    match ata.lu().solve(&atb) {
        Some(x) => (0..n).map(|i| x[i].exp().clamp(0.25, 4.0)).collect(),
        None => vec![1.0; n],
    }
}

/// Boundary warp and auto crop of a stitched canvas (`PanoramaResult`), with
/// the pixels the result covers (empty edges are left where the crop is off).
pub fn finish(
    image: &Rgb32FImage,
    covered: &[bool],
    config: &PanoramaConfig,
) -> (Rgb32FImage, Vec<bool>) {
    finish_outline(Cow::Borrowed(image), covered, config)
}

/// `finish` on a copy reduced to at most `max_dim` pixels: the dialog's
/// preview when only the boundary warp or crop changes.
pub fn finish_preview(
    image: &Rgb32FImage,
    covered: &[bool],
    config: &PanoramaConfig,
    max_dim: u32,
) -> (Rgb32FImage, (u32, u32)) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let f = w.max(h).div_ceil(max_dim as usize).max(1);
    let (sw, sh) = ((w / f).max(1), (h / f).max(1));
    let raw: &[f32] = image;
    let mut small = Rgb32FImage::new(sw as u32, sh as u32);
    let mut small_cov = vec![false; sw * sh];
    small
        .as_mut()
        .par_chunks_mut(sw * 3)
        .zip(small_cov.par_chunks_mut(sw))
        .enumerate()
        .for_each(|(sy, (row, crow))| {
            for sx in 0..sw {
                let (mut sum, mut n, mut all) = ([0.0f32; 3], 0.0f32, true);
                for y in sy * f..(sy + 1) * f {
                    for x in sx * f..(sx + 1) * f {
                        let i = y * w + x;
                        all &= covered[i];
                        if covered[i] {
                            let p = &raw[i * 3..i * 3 + 3];
                            sum[0] += p[0];
                            sum[1] += p[1];
                            sum[2] += p[2];
                            n += 1.0;
                        }
                    }
                }
                crow[sx] = all;
                if n > 0.0 {
                    row[sx * 3..sx * 3 + 3].copy_from_slice(&sum.map(|v| v / n));
                }
            }
        });
    let (finished, _) = finish_outline(Cow::Owned(small), &small_cov, config);
    // The full-resolution size the same settings give.
    let full = (finished.width() * f as u32, finished.height() * f as u32);
    (finished, full)
}

/// Borrows the canvas until a step makes a new image (a copy of a 90 MP
/// canvas up front cost 1 GB).
fn finish_outline(
    image: Cow<'_, Rgb32FImage>,
    covered: &[bool],
    config: &PanoramaConfig,
) -> (Rgb32FImage, Vec<bool>) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut image = image;
    let mut covered = covered.to_vec();
    let amount = (config.boundary_warp / 100.0).clamp(0.0, 1.0) as f64;
    if amount > 0.0 {
        let (warped, cov) = boundary_warp(&image, &covered, amount);
        image = Cow::Owned(warped);
        covered = cov;
    }
    if !config.auto_crop {
        return (image.into_owned(), covered);
    }
    // Largest rectangle on a coarse mask (a cell counts when fully covered).
    let f = (w.max(h) / 1500).max(1);
    let (mw, mh) = (w / f, h / f);
    let mask: Vec<bool> = (0..mw * mh)
        .into_par_iter()
        .map(|k| {
            let (mx, my) = (k % mw, k / mw);
            (my * f..(my + 1) * f).all(|y| (mx * f..(mx + 1) * f).all(|x| covered[y * w + x]))
        })
        .collect();
    let (rx, ry, rw, rh) = largest_rectangle(&mask, mw, mh);
    if rw == 0 || rh == 0 {
        return (image.into_owned(), covered);
    }
    let (x0, y0, cw, ch) = (rx * f, ry * f, rw * f, rh * f);
    let cropped =
        image::imageops::crop_imm(&*image, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
    let cov = (0..cw * ch)
        .map(|k| covered[(y0 + k / cw) * w + x0 + k % cw])
        .collect();
    (cropped, cov)
}

/// A smooth outline for an irregular edge (per column or row; `None` where
/// nothing is covered): a degree-4 polynomial through it, moved inwards
/// (`inward` +1 for a top/left edge, −1 for bottom/right) until it lies
/// inside the covered area except near the ends, which the other pass and
/// the crop take care of. Following the jagged edge itself makes the
/// stretched picture ripple.
fn smooth_edge(edge: &[Option<f64>], inward: f64) -> Vec<f64> {
    let n = edge.len();
    let points: Vec<(f64, f64)> = edge
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.map(|v| (i as f64 / (n - 1).max(1) as f64, v)))
        .collect();
    if points.len() < 6 {
        let fallback = if inward > 0.0 { 0.0 } else { (n - 1) as f64 };
        return edge.iter().map(|e| e.unwrap_or(fallback)).collect();
    }
    const DEGREE: usize = 4;
    let mut ata = nalgebra::DMatrix::<f64>::zeros(DEGREE + 1, DEGREE + 1);
    let mut atb = nalgebra::DVector::<f64>::zeros(DEGREE + 1);
    for &(t, v) in &points {
        let basis: Vec<f64> = (0..=DEGREE)
            .map(|k| (2.0 * t - 1.0).powi(k as i32))
            .collect();
        for a in 0..=DEGREE {
            atb[a] += basis[a] * v;
            for b in 0..=DEGREE {
                ata[(a, b)] += basis[a] * basis[b];
            }
        }
    }
    let Some(coeffs) = ata.lu().solve(&atb) else {
        return edge.iter().map(|e| e.unwrap_or(0.0)).collect();
    };
    let fit = |t: f64| {
        (0..=DEGREE)
            .map(|k| coeffs[k] * (2.0 * t - 1.0).powi(k as i32))
            .sum::<f64>()
    };
    let margin = 0.03;
    let offset = points
        .iter()
        .filter(|(t, _)| *t > margin && *t < 1.0 - margin)
        .map(|&(t, v)| inward * (v - fit(t)))
        .fold(0.0f64, f64::max);
    (0..n)
        .map(|i| fit(i as f64 / (n - 1).max(1) as f64) + inward * offset)
        .collect()
}

/// Stretches each column between its covered top and bottom (and then each
/// row between its left and right) towards the full canvas, by `amount`.
fn boundary_warp(image: &Rgb32FImage, covered: &[bool], amount: f64) -> (Rgb32FImage, Vec<bool>) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    // Vertical pass.
    let tops: Vec<Option<f64>> = (0..w)
        .map(|x| (0..h).find(|&y| covered[y * w + x]).map(|y| y as f64))
        .collect();
    let bottoms: Vec<Option<f64>> = (0..w)
        .map(|x| (0..h).rev().find(|&y| covered[y * w + x]).map(|y| y as f64))
        .collect();
    let (tops, bottoms) = (smooth_edge(&tops, 1.0), smooth_edge(&bottoms, -1.0));
    let src: &[f32] = image;
    let mut mid = vec![0.0f32; w * h * 3];
    let mut mid_cov = vec![false; w * h];
    mid.par_chunks_mut(w * 3)
        .zip(mid_cov.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (row, crow))| {
            let t = y as f64 / (h - 1).max(1) as f64;
            for x in 0..w {
                let target = tops[x] + t * (bottoms[x] - tops[x]);
                let sy = (y as f64 + amount * (target - y as f64)).clamp(0.0, (h - 1) as f64);
                let (y0, fy) = (sy.floor() as usize, (sy - sy.floor()) as f32);
                let y1 = (y0 + 1).min(h - 1);
                crow[x] = covered[y0 * w + x] && covered[y1 * w + x];
                for c in 0..3 {
                    row[x * 3 + c] =
                        src[(y0 * w + x) * 3 + c] * (1.0 - fy) + src[(y1 * w + x) * 3 + c] * fy;
                }
            }
        });
    // Horizontal pass.
    let lefts: Vec<Option<f64>> = (0..h)
        .map(|y| (0..w).find(|&x| mid_cov[y * w + x]).map(|x| x as f64))
        .collect();
    let rights: Vec<Option<f64>> = (0..h)
        .map(|y| (0..w).rev().find(|&x| mid_cov[y * w + x]).map(|x| x as f64))
        .collect();
    let (lefts, rights) = (smooth_edge(&lefts, 1.0), smooth_edge(&rights, -1.0));
    let mut out = Rgb32FImage::new(w as u32, h as u32);
    let mut out_cov = vec![false; w * h];
    {
        let dst: &mut [f32] = &mut out;
        dst.par_chunks_mut(w * 3)
            .zip(out_cov.par_chunks_mut(w))
            .enumerate()
            .for_each(|(y, (row, crow))| {
                for x in 0..w {
                    let t = x as f64 / (w - 1).max(1) as f64;
                    let target = lefts[y] + t * (rights[y] - lefts[y]);
                    let sx = (x as f64 + amount * (target - x as f64)).clamp(0.0, (w - 1) as f64);
                    let (x0, fx) = (sx.floor() as usize, (sx - sx.floor()) as f32);
                    let x1 = (x0 + 1).min(w - 1);
                    crow[x] = mid_cov[y * w + x0] && mid_cov[y * w + x1];
                    for c in 0..3 {
                        row[x * 3 + c] =
                            mid[(y * w + x0) * 3 + c] * (1.0 - fx) + mid[(y * w + x1) * 3 + c] * fx;
                    }
                }
            });
    }
    (out, out_cov)
}
