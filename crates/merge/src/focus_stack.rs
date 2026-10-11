//! Focus stacking: per-pixel focus decisions on a pyramid, fused in
//! scene-linear RGB. Moved from the app's focus_stacking.rs (RapidRAW,
//! Timon Käch; AGPL-3.0).
use crate::align::*;
use crate::planes::*;
use rayon::prelude::*;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

pub const MEMORY_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct FusionConfig {
    pub decision_dim: usize,
    pub max_levels: usize,
    pub pool_sigma: f32,
    pub guided_radius: usize,
    pub guided_eps: f32,
    pub median_radius: usize,
    pub median_sigma: f32,
    pub consistency_threshold: f32,
    pub detail_gamma: f32,
}

impl Default for FusionConfig {
    fn default() -> Self {
        FusionConfig {
            decision_dim: 2048,
            max_levels: 9,
            pool_sigma: 2.0,
            guided_radius: 12,
            guided_eps: 1e-4,
            median_radius: 5,
            median_sigma: 0.08,
            consistency_threshold: 0.10,
            detail_gamma: 8.0,
        }
    }
}

pub fn guided_filter(guide: &Plane, input: &Plane, radius: usize, eps: f32) -> Plane {
    let mean_i = box_filter(guide, radius);
    let mean_p = box_filter(input, radius);

    let mut ii = Plane::new(guide.w, guide.h);
    let mut ip = Plane::new(guide.w, guide.h);
    ii.data
        .par_iter_mut()
        .zip(guide.data.par_iter())
        .for_each(|(o, &g)| *o = g * g);
    ip.data
        .par_iter_mut()
        .zip(guide.data.par_iter().zip(input.data.par_iter()))
        .for_each(|(o, (&g, &p))| *o = g * p);

    let corr_i = box_filter(&ii, radius);
    let corr_ip = box_filter(&ip, radius);

    let mut a = Plane::new(guide.w, guide.h);
    let mut b = Plane::new(guide.w, guide.h);
    for i in 0..a.data.len() {
        let var_i = corr_i.data[i] - mean_i.data[i] * mean_i.data[i];
        let cov_ip = corr_ip.data[i] - mean_i.data[i] * mean_p.data[i];
        let ai = cov_ip / (var_i + eps);
        a.data[i] = ai;
        b.data[i] = mean_p.data[i] - ai * mean_i.data[i];
    }

    let mean_a = box_filter(&a, radius);
    let mean_b = box_filter(&b, radius);

    let mut out = Plane::new(guide.w, guide.h);
    out.data.par_iter_mut().enumerate().for_each(|(i, o)| {
        *o = mean_a.data[i] * guide.data[i] + mean_b.data[i];
    });
    out
}

fn focus_domain(luma: &Plane) -> Plane {
    luma.map(|v| (v.max(0.0) * 8.0).ln_1p())
}

pub fn focus_scores(luma: &Plane, levels: usize, cfg: &FusionConfig) -> Vec<Plane> {
    let fd = focus_domain(luma);
    let lp = laplacian_pyramid(&fd, levels);
    let mut out = Vec::with_capacity(lp.details.len());
    for d in lp.details.iter() {
        let sq = d.map(|v| v * v);
        let pooled = gaussian_blur(&sq, cfg.pool_sigma);
        out.push(pooled.map(|v| v.max(0.0).sqrt()));
    }
    out
}

pub struct DecisionMaps {
    pub weights: Vec<Plane>,
    pub labels: Vec<u16>,
    pub w: usize,
    pub h: usize,
}

pub fn decide_labels(
    scores: &[Vec<Plane>],
    base: &[Plane],
    masks: &[Plane],
    cfg: &FusionConfig,
) -> DecisionMaps {
    let n = scores.len();
    assert!(n > 0);
    let (w, h) = (masks[0].w, masks[0].h);
    let npx = w * h;
    let n_levels = scores[0].len();

    let mut combined: Vec<Plane> = (0..n).map(|_| Plane::new(w, h)).collect();
    for (k, _) in scores[0].iter().enumerate().take(n_levels) {
        let level_weight = 1.0 / (1.0 + k as f32);
        let mut totals = vec![0f32; npx];
        for (i, score_frames) in scores.iter().enumerate().take(n) {
            let s = &score_frames[k];
            totals
                .par_iter_mut()
                .zip(s.data.par_iter().zip(masks[i].data.par_iter()))
                .for_each(|(t, (&v, &m))| *t += v * m);
        }
        for (i, score_frames) in scores.iter().enumerate().take(n) {
            let s = &score_frames[k];
            let m = &masks[i];
            combined[i]
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(p, c)| {
                    let denom = totals[p] + 1e-8;
                    *c += level_weight * (s.data[p] * m.data[p]) / denom;
                });
        }
    }

    {
        let mut median = vec![0f32; npx];
        let mut buf = vec![0f32; n];
        for (p, med) in median.iter_mut().enumerate() {
            let mut cnt = 0usize;
            for (i, mask_i) in masks.iter().enumerate().take(n) {
                if mask_i.data[p] > 0.5 {
                    buf[cnt] = base[i].data[p];
                    cnt += 1;
                }
            }
            if cnt == 0 {
                continue;
            }
            let slice = &mut buf[..cnt];
            slice.sort_by(|a, b| a.partial_cmp(b).unwrap());
            *med = slice[cnt / 2];
        }
        let thr = cfg.consistency_threshold.max(1e-4);
        for i in 0..n {
            let b = &base[i];
            combined[i]
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(p, c)| {
                    let scale = median[p].abs().max(0.02);
                    let dev = (b.data[p] - median[p]).abs() / scale;
                    let t = dev / thr;
                    *c *= (-t * t).exp();
                });
        }
    }

    let mut guide = Plane::new(w, h);
    guide.data.par_iter_mut().enumerate().for_each(|(p, g)| {
        let mut best = f32::NEG_INFINITY;
        let mut bi = 0usize;
        for (i, comb) in combined.iter().enumerate() {
            if comb.data[p] > best {
                best = comb.data[p];
                bi = i;
            }
        }
        *g = base[bi].data[p];
    });
    let guide = gaussian_blur(&guide, 0.8);

    let aggregated: Vec<Plane> = combined
        .par_iter()
        .map(|c| guided_filter(&guide, c, cfg.guided_radius, cfg.guided_eps))
        .collect();

    let mut labels = vec![0u16; npx];
    let mut any_valid = vec![false; npx];
    labels
        .par_iter_mut()
        .zip(any_valid.par_iter_mut())
        .enumerate()
        .for_each(|(p, (l, av))| {
            let mut best = f32::NEG_INFINITY;
            let mut bi = 0usize;
            let mut found = false;
            for i in 0..n {
                if masks[i].data[p] < 0.5 {
                    continue;
                }
                found = true;
                if aggregated[i].data[p] > best {
                    best = aggregated[i].data[p];
                    bi = i;
                }
            }
            *l = bi as u16;
            *av = found;
        });

    let labels = weighted_median_labels(
        &labels,
        &guide,
        n,
        cfg.median_radius,
        cfg.median_sigma,
        w,
        h,
    );

    let mut weights: Vec<Plane> = Vec::with_capacity(n);
    for (i, mask_i) in masks.iter().enumerate() {
        let mut ind = Plane::new(w, h);
        ind.data.par_iter_mut().enumerate().for_each(|(p, v)| {
            *v = if any_valid[p] && labels[p] as usize == i && mask_i.data[p] > 0.5 {
                1.0
            } else {
                0.0
            };
        });
        let soft = guided_filter(&guide, &ind, cfg.guided_radius / 3 + 1, cfg.guided_eps);
        let mut soft = soft.map(|v| v.max(0.0));
        soft.data
            .par_iter_mut()
            .zip(mask_i.data.par_iter())
            .for_each(|(v, &m)| *v *= m);
        weights.push(soft);
    }

    let mut totals = vec![0f32; npx];
    for wi in weights.iter() {
        totals
            .par_iter_mut()
            .zip(wi.data.par_iter())
            .for_each(|(t, &v)| *t += v);
    }
    for wi in weights.iter_mut() {
        wi.data.par_iter_mut().enumerate().for_each(|(p, v)| {
            *v = if totals[p] > 1e-6 {
                *v / totals[p]
            } else {
                0.0
            };
        });
    }

    DecisionMaps {
        weights,
        labels,
        w,
        h,
    }
}

fn weighted_median_labels(
    labels: &[u16],
    guide: &Plane,
    n_labels: usize,
    radius: usize,
    sigma: f32,
    w: usize,
    h: usize,
) -> Vec<u16> {
    if radius == 0 {
        return labels.to_vec();
    }
    let mut out = vec![0u16; w * h];
    let inv2s2 = 1.0 / (2.0 * sigma * sigma).max(1e-8);
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut hist = vec![0f32; n_labels];
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(h);
        for x in 0..w {
            for v in hist.iter_mut() {
                *v = 0.0;
            }
            let gp = guide.data[y * w + x];
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(w);
            let mut total = 0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let d = guide.data[yy * w + xx] - gp;
                    let wgt = (-d * d * inv2s2).exp();
                    hist[labels[yy * w + xx] as usize] += wgt;
                    total += wgt;
                }
            }
            let half = total * 0.5;
            let mut acc = 0f32;
            let mut chosen = labels[y * w + x];
            for (i, &v) in hist.iter().enumerate() {
                acc += v;
                if acc >= half {
                    chosen = i as u16;
                    break;
                }
            }
            row[x] = chosen;
        }
    });
    out
}

pub struct MergeAccumulator {
    levels: usize,
    dims: Vec<(usize, usize)>,
    num_details: [Vec<Plane>; 3],
    num_residual: [Plane; 3],
    den_details: Vec<Plane>,
    den_residual: Plane,
    gammas: Vec<f32>,
}

impl MergeAccumulator {
    pub fn new(w: usize, h: usize, cfg: &FusionConfig) -> Self {
        let levels = level_count(w, h, cfg.max_levels);
        let mut dims = Vec::with_capacity(levels);
        let (mut cw, mut ch) = (w, h);
        for _ in 0..levels {
            dims.push((cw, ch));
            cw = cw.div_ceil(2);
            ch = ch.div_ceil(2);
        }
        let mk = |from: usize| -> Vec<Plane> {
            (from..levels - 1)
                .map(|k| Plane::new(dims[k].0, dims[k].1))
                .collect()
        };
        let (rw, rh) = dims[levels - 1];
        let gammas = (0..levels - 1)
            .map(|k| 1.0 + (cfg.detail_gamma - 1.0) * 0.5f32.powi(k as i32))
            .collect();
        MergeAccumulator {
            levels,
            num_details: [mk(0), mk(0), mk(0)],
            num_residual: [Plane::new(rw, rh), Plane::new(rw, rh), Plane::new(rw, rh)],
            den_details: mk(0),
            den_residual: Plane::new(rw, rh),
            dims,
            gammas,
        }
    }

    pub fn add_frame(&mut self, frame: &PlanarRgb, weight_full: &Plane) {
        let wpyr = gaussian_pyramid(weight_full, self.levels);

        let mut wpow: Vec<Plane> = Vec::with_capacity(self.levels);
        for (k, wp) in wpyr.iter().enumerate().take(self.levels) {
            let g = if k < self.gammas.len() {
                self.gammas[k]
            } else {
                1.0
            };
            wpow.push(wp.map(move |v| v.max(0.0).powf(g)));
        }

        for (k, src) in wpow.iter().enumerate().take(self.levels - 1) {
            let dst = &mut self.den_details[k];
            dst.data
                .par_iter_mut()
                .zip(src.data.par_iter())
                .for_each(|(d, &s)| *d += s);
        }
        {
            let src = &wpow[self.levels - 1];
            self.den_residual
                .data
                .par_iter_mut()
                .zip(src.data.par_iter())
                .for_each(|(d, &s)| *d += s);
        }

        for ch in 0..3 {
            let lp = laplacian_pyramid(&frame.c[ch], self.levels);
            for (k, det) in lp.details.iter().enumerate() {
                let wk = &wpow[k];
                self.num_details[ch][k]
                    .data
                    .par_iter_mut()
                    .enumerate()
                    .for_each(|(p, o)| *o += wk.data[p] * det.data[p]);
            }
            let wk = &wpow[self.levels - 1];
            let res = &lp.residual;
            self.num_residual[ch]
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(p, o)| *o += wk.data[p] * res.data[p]);
        }
    }

    pub fn finish(self) -> PlanarRgb {
        let (w, h) = self.dims[0];
        let mut out = PlanarRgb::new(w, h);
        let MergeAccumulator {
            num_details,
            num_residual,
            den_details,
            den_residual,
            ..
        } = self;

        let mut num_details = num_details;
        let mut num_residual = num_residual;

        for ch in 0..3 {
            let mut details = Vec::with_capacity(den_details.len());
            for (k, den) in den_details.iter().enumerate() {
                let mut d = std::mem::replace(&mut num_details[ch][k], Plane::new(1, 1));
                d.data
                    .par_iter_mut()
                    .zip(den.data.par_iter())
                    .for_each(|(v, &dd)| *v = if dd > 1e-8 { *v / dd } else { 0.0 });
                details.push(d);
            }
            let mut residual = std::mem::replace(&mut num_residual[ch], Plane::new(1, 1));
            residual
                .data
                .par_iter_mut()
                .zip(den_residual.data.par_iter())
                .for_each(|(v, &dd)| *v = if dd > 1e-8 { *v / dd } else { 0.0 });

            let lp = LaplacianPyramid { details, residual };
            out.c[ch] = collapse_pyramid(&lp);
        }
        out
    }
}

pub fn weight_to_full(weight: &Plane, w: usize, h: usize) -> Plane {
    if weight.w == w && weight.h == h {
        weight.clone()
    } else {
        upsample_to(weight, w, h)
    }
}

pub fn valid_bounds(masks: &[Plane], min_coverage: f32) -> (usize, usize, usize, usize) {
    let (w, h) = (masks[0].w, masks[0].h);
    let n = masks.len() as f32;
    let mut cov = vec![0f32; w * h];
    for m in masks {
        cov.par_iter_mut()
            .zip(m.data.par_iter())
            .for_each(|(c, &v)| *c += v);
    }
    let need = (n * min_coverage).max(1.0);

    let mut x0 = w;
    let mut x1 = 0usize;
    let mut y0 = h;
    let mut y1 = 0usize;
    for y in 0..h {
        for x in 0..w {
            if cov[y * w + x] >= need {
                if x < x0 {
                    x0 = x;
                }
                if x > x1 {
                    x1 = x;
                }
                if y < y0 {
                    y0 = y;
                }
                if y > y1 {
                    y1 = y;
                }
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return (0, 0, w, h);
    }
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

pub trait FrameSource: Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn dims(&self) -> (usize, usize);
    fn get(&self, index: usize) -> Result<PlanarRgb, String>;
}

#[derive(Clone, Copy, Debug)]
pub struct StackConfig {
    pub align: AlignConfig,
    pub fusion: FusionConfig,
    pub align_dim: usize,
    pub align_enabled: bool,
    pub pose_smoothing: f64,
    pub crop_coverage: f32,
}

impl Default for StackConfig {
    fn default() -> Self {
        StackConfig {
            align: AlignConfig::default(),
            fusion: FusionConfig::default(),
            align_dim: 2400,
            align_enabled: true,
            pose_smoothing: 0.35,
            crop_coverage: 0.999,
        }
    }
}

pub struct StackResult {
    pub image: PlanarRgb,
    pub depth: Vec<u16>,
    pub depth_w: usize,
    pub depth_h: usize,
    pub poses: Vec<FramePose>,
    pub reference: usize,
}

fn fit_dims(w: usize, h: usize, max_dim: usize) -> (usize, usize) {
    let long = w.max(h);
    if long <= max_dim {
        return (w, h);
    }
    let s = max_dim as f64 / long as f64;
    (
        ((w as f64 * s).round() as usize).max(1),
        ((h as f64 * s).round() as usize).max(1),
    )
}

fn regularize_poses(poses: &mut [FramePose], cfg: &StackConfig, reference: usize) {
    let n = poses.len();
    if n < 4 || cfg.pose_smoothing <= 0.0 {
        return;
    }

    let good: Vec<usize> = (0..n)
        .filter(|&i| poses[i].quality >= cfg.align.min_quality && poses[i].warp.is_plausible())
        .collect();
    if good.len() < 4 {
        return;
    }

    let center = (n as f64 - 1.0) / 2.0;
    let mut ata = [[0f64; 3]; 3];
    for &i in &good {
        let t = (i as f64 - center) / (n as f64).max(1.0);
        let w = poses[i].quality * poses[i].quality;
        let basis = [1.0, t, t * t];
        for a in 0..3 {
            for b in 0..3 {
                ata[a][b] += w * basis[a] * basis[b];
            }
        }
    }
    let m = nalgebra::Matrix3::new(
        ata[0][0], ata[0][1], ata[0][2], ata[1][0], ata[1][1], ata[1][2], ata[2][0], ata[2][1],
        ata[2][2],
    );
    let minv = match m.try_inverse() {
        Some(v) => v,
        None => return,
    };

    let mut coeffs = [[0f64; 3]; 22];
    for (p, coeff) in coeffs.iter_mut().enumerate() {
        let mut atb = nalgebra::Vector3::zeros();
        for &i in &good {
            let t = (i as f64 - center) / (n as f64).max(1.0);
            let w = poses[i].quality * poses[i].quality;
            let val = poses[i].warp.to_vector()[p];
            atb += nalgebra::Vector3::new(w * val, w * val * t, w * val * t * t);
        }
        let c = minv * atb;
        *coeff = [c[0], c[1], c[2]];
    }

    for (i, pose) in poses.iter_mut().enumerate().take(n) {
        if i == reference {
            continue;
        }
        let t = (i as f64 - center) / (n as f64).max(1.0);
        let mut fitted = [0f64; 22];
        for (p, fit_val) in fitted.iter_mut().enumerate() {
            *fit_val = coeffs[p][0] + coeffs[p][1] * t + coeffs[p][2] * t * t;
        }

        let failed = pose.quality < cfg.align.min_quality || !pose.warp.is_plausible();
        let s = if failed { 1.0 } else { cfg.pose_smoothing };

        let cur = pose.warp.to_vector();
        let mut blended = [0f64; 22];
        for (p, bl) in blended.iter_mut().enumerate() {
            *bl = cur[p] * (1.0 - s) + fitted[p] * s;
        }
        let candidate = pose.warp.with_vector(&blended);
        if candidate.is_plausible() {
            pose.warp = candidate;
        }
    }
}

pub fn run_focus_stack<S: FrameSource + ?Sized>(
    src: &S,
    cfg: &StackConfig,
    progress: &dyn Fn(&str),
) -> Result<StackResult, String> {
    let n = src.len();
    if n < 2 {
        return Err("Focus stacking needs at least two frames.".into());
    }
    let (w, h) = src.dims();
    if w < 32 || h < 32 {
        return Err("Frames are too small to stack.".into());
    }

    let reference = n / 2;

    let (aw, ah) = fit_dims(w, h, cfg.align_dim);
    let align_scale = aw as f64 / w as f64;

    let mut poses: Vec<FramePose> = (0..n)
        .map(|_| FramePose {
            warp: LensWarp::identity(aw, ah),
            gain: 1.0,
            bias: 0.0,
            quality: 1.0,
        })
        .collect();

    if cfg.align_enabled {
        progress("Preparing alignment references...");
        let mut align_luma: Vec<Plane> = Vec::with_capacity(n);
        for i in 0..n {
            let f = src.get(i)?;
            if f.w != w || f.h != h {
                return Err(format!(
                    "Frame {} is {}x{} but the stack is {}x{}. All frames must share dimensions.",
                    i + 1,
                    f.w,
                    f.h,
                    w,
                    h
                ));
            }
            align_luma.push(resize_to(&f.luma(), aw, ah));
        }

        let mut acfg = cfg.align;
        acfg.finest_dim = acfg.finest_dim.max(aw.max(ah));

        let mut completed = 0;
        for pass in 0..2 {
            let order: Vec<usize> = if pass == 0 {
                (0..reference).rev().collect()
            } else {
                ((reference + 1)..n).collect()
            };
            let mut prev = LensWarp::identity(aw, ah);
            for &i in order.iter() {
                completed += 1;
                progress(&format!("Aligning frame {} of {}...", completed, n - 1));
                let pyr = build_align_pyramids(&align_luma[reference], &align_luma[i], &acfg);
                let pose = solve_alignment(&pyr, prev, &acfg);
                let pose = if pose.quality < acfg.min_quality || !pose.warp.is_plausible() {
                    let retry = solve_alignment(&pyr, LensWarp::identity(aw, ah), &acfg);
                    if retry.quality > pose.quality {
                        retry
                    } else {
                        pose
                    }
                } else {
                    pose
                };
                poses[i] = pose;
                if pose.warp.is_plausible() {
                    prev = pose.warp;
                }
            }
        }

        poses[reference] = FramePose {
            warp: LensWarp::identity(aw, ah),
            gain: 1.0,
            bias: 0.0,
            quality: 1.0,
        };

        progress("Regularising alignment across the stack...");
        regularize_poses(&mut poses, cfg, reference);
    }

    let full_poses: Vec<FramePose> = poses
        .iter()
        .map(|p| FramePose {
            warp: p.warp.rescaled(1.0 / align_scale),
            ..*p
        })
        .collect();

    let (dw, dh) = fit_dims(w, h, cfg.fusion.decision_dim);
    let levels = level_count(w, h, cfg.fusion.max_levels);

    let mut scores: Vec<Vec<Plane>> = Vec::with_capacity(n);
    let mut bases: Vec<Plane> = Vec::with_capacity(n);
    let mut masks: Vec<Plane> = Vec::with_capacity(n);

    for (i, full_pose) in full_poses.iter().enumerate().take(n) {
        progress(&format!("Measuring focus in frame {} of {}...", i + 1, n));
        let frame = src.get(i)?;
        let (warped, mask) = warp_or_copy(&frame, full_pose, cfg.align_enabled, w, h);
        drop(frame);

        let luma = warped.luma();
        let raw_scores = focus_scores(&luma, levels, &cfg.fusion);
        scores.push(raw_scores.iter().map(|s| resize_to(s, dw, dh)).collect());

        let compressed = luma.map(|v| (v.max(0.0) * 8.0).ln_1p());
        bases.push(gaussian_blur(&resize_to(&compressed, dw, dh), 3.0));

        masks.push(resize_to(&mask, dw, dh).map(|v| if v > 0.98 { 1.0 } else { 0.0 }));
    }

    progress("Solving depth labels...");
    let decision = decide_labels(&scores, &bases, &masks, &cfg.fusion);
    drop(scores);
    drop(bases);

    let mut acc = MergeAccumulator::new(w, h, &cfg.fusion);
    for (i, full_pose) in full_poses.iter().enumerate().take(n) {
        progress(&format!("Blending frame {} of {}...", i + 1, n));
        let frame = src.get(i)?;
        let (warped, _) = warp_or_copy(&frame, full_pose, cfg.align_enabled, w, h);
        drop(frame);
        let wf = weight_to_full(&decision.weights[i], w, h);
        acc.add_frame(&warped, &wf);
    }

    progress("Reconstructing...");
    let merged = acc.finish();

    let (cx, cy, cw, ch) = valid_bounds(&masks, cfg.crop_coverage);
    let sx = w as f64 / dw as f64;
    let sy = h as f64 / dh as f64;
    let x0 = ((cx as f64 * sx).ceil() as usize).min(w.saturating_sub(1));
    let y0 = ((cy as f64 * sy).ceil() as usize).min(h.saturating_sub(1));
    let cw = ((cw as f64 * sx).floor() as usize).min(w - x0).max(1);
    let ch = ((ch as f64 * sy).floor() as usize).min(h - y0).max(1);

    let image = if x0 == 0 && y0 == 0 && cw == w && ch == h {
        merged
    } else {
        merged.crop(x0, y0, cw, ch)
    };

    Ok(StackResult {
        image,
        depth: decision.labels,
        depth_w: decision.w,
        depth_h: decision.h,
        poses: full_poses,
        reference,
    })
}

fn warp_or_copy(
    frame: &PlanarRgb,
    pose: &FramePose,
    align_enabled: bool,
    w: usize,
    h: usize,
) -> (PlanarRgb, Plane) {
    if !align_enabled {
        return (frame.clone(), Plane::filled(w, h, 1.0));
    }
    warp_frame(frame, &pose.warp, pose.gain as f32, pose.bias as f32, w, h)
}

pub fn depth_preview_plane(result: &StackResult, n_frames: usize) -> Plane {
    let mut p = Plane::new(result.depth_w, result.depth_h);
    let denom = (n_frames.saturating_sub(1)).max(1) as f32;
    p.data
        .par_iter_mut()
        .zip(result.depth.par_iter())
        .for_each(|(o, &l)| *o = l as f32 / denom);
    p
}

pub struct MemorySource {
    pub frames: Vec<PlanarRgb>,
}

impl FrameSource for MemorySource {
    fn len(&self) -> usize {
        self.frames.len()
    }
    fn dims(&self) -> (usize, usize) {
        (self.frames[0].w, self.frames[0].h)
    }
    fn get(&self, index: usize) -> Result<PlanarRgb, String> {
        Ok(self.frames[index].clone())
    }
}

pub struct DiskSource {
    dir: PathBuf,
    n: usize,
    w: usize,
    h: usize,
}

impl DiskSource {
    pub fn create(n: usize, w: usize, h: usize) -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "rr-focusstack-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create scratch dir: {}", e))?;
        Ok(DiskSource { dir, n, w, h })
    }

    fn path(&self, i: usize) -> PathBuf {
        self.dir.join(format!("frame_{:05}.f32", i))
    }

    pub fn write(&self, i: usize, img: &PlanarRgb) -> Result<(), String> {
        let mut f =
            fs::File::create(self.path(i)).map_err(|e| format!("Scratch write failed: {}", e))?;
        for ch in 0..3 {
            f.write_all(as_bytes(&img.c[ch].data))
                .map_err(|e| format!("Scratch write failed: {}", e))?;
        }
        Ok(())
    }
}

impl FrameSource for DiskSource {
    fn len(&self) -> usize {
        self.n
    }
    fn dims(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn get(&self, index: usize) -> Result<PlanarRgb, String> {
        let mut f =
            fs::File::open(self.path(index)).map_err(|e| format!("Scratch read failed: {}", e))?;
        let mut out = PlanarRgb::new(self.w, self.h);
        for ch in 0..3 {
            f.read_exact(as_bytes_mut(&mut out.c[ch].data))
                .map_err(|e| format!("Scratch read failed: {}", e))?;
        }
        Ok(out)
    }
}

impl Drop for DiskSource {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn as_bytes(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}
fn as_bytes_mut(v: &mut [f32]) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(v)) }
}

/// Options of a focus stack.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusConfig {
    pub align: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        FocusConfig { align: true }
    }
}

pub struct FocusResult {
    /// Linear working RGB (see `Frame::image`), cropped to the area all
    /// aligned frames cover.
    pub image: image::Rgb32FImage,
    pub stack: StackResult,
    pub reference_path: PathBuf,
    pub color: Option<rawtonic_rawdecode::raw_processing::ImageColor>,
    pub metadata: Option<rawler::decoders::RawMetadata>,
}

/// Stacks photos focused at different distances (at least two, same size).
/// Frames are lens-corrected, balanced to the middle frame, and fused in a
/// cube-root domain (close to how the eye sees detail; negative wide-gamut
/// channels survive), then returned to linear.
pub fn stack(
    paths: &[PathBuf],
    options: &rawtonic_engine::image_loader::BaseImageOptions,
    config: &FocusConfig,
    progress: &dyn Fn(&str),
) -> Result<FocusResult, String> {
    if paths.len() < 2 {
        return Err("Select at least two photos to stack.".into());
    }
    let n = paths.len();
    let reference = n / 2;
    let name = |p: &std::path::Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    progress(&format!("Loading {}", name(&paths[reference])));
    let ref_frame = crate::frame::load_corrected(&paths[reference], options, None)
        .map_err(|e| e.to_string())?;
    let (w, h) = (
        ref_frame.image.width() as usize,
        ref_frame.image.height() as usize,
    );
    let color = ref_frame.color.clone();
    let metadata = ref_frame.metadata.clone();
    let to_planar = |img: &image::Rgb32FImage| {
        let mut p = PlanarRgb::new(img.width() as usize, img.height() as usize);
        let raw: &[f32] = img;
        for ch in 0..3 {
            p.c[ch]
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(i, o)| *o = raw[i * 3 + ch].cbrt());
        }
        p
    };
    let estimated = (n as u64) * (w as u64) * (h as u64) * 12;
    let mut ref_planar = Some(to_planar(&ref_frame.image));
    drop(ref_frame);
    let mut load = |i: usize| -> Result<PlanarRgb, String> {
        if i == reference {
            return ref_planar
                .take()
                .ok_or_else(|| "reference loaded twice".to_string());
        }
        progress(&format!("Loading {}", name(&paths[i])));
        let f = crate::frame::load_corrected(&paths[i], options, color.as_ref())
            .map_err(|e| e.to_string())?;
        if (f.image.width() as usize, f.image.height() as usize) != (w, h) {
            return Err(format!(
                "{} is {}×{}, {} is {}×{}: the photos of a focus stack must have the same size and orientation",
                name(&paths[reference]),
                w,
                h,
                name(&paths[i]),
                f.image.width(),
                f.image.height()
            ));
        }
        Ok(to_planar(&f.image))
    };
    let source: Box<dyn FrameSource> = if estimated <= MEMORY_BUDGET_BYTES {
        let frames = (0..n).map(&mut load).collect::<Result<Vec<_>, _>>()?;
        Box::new(MemorySource { frames })
    } else {
        let disk = DiskSource::create(n, w, h)?;
        for i in 0..n {
            let f = load(i)?;
            disk.write(i, &f)?;
        }
        Box::new(disk)
    };
    let cfg = StackConfig {
        align_enabled: config.align,
        ..StackConfig::default()
    };
    let mut result = run_focus_stack(source.as_ref(), &cfg, progress)?;
    drop(source);
    let planar = std::mem::replace(&mut result.image, PlanarRgb::new(0, 0));
    let mut image = image::Rgb32FImage::new(planar.w as u32, planar.h as u32);
    {
        let out: &mut [f32] = &mut image;
        out.par_chunks_mut(3).enumerate().for_each(|(i, o)| {
            for (ch, oc) in o.iter_mut().enumerate() {
                let v = planar.c[ch].data[i];
                *oc = v * v * v;
            }
        });
    }
    Ok(FocusResult {
        image,
        stack: result,
        reference_path: paths[reference].clone(),
        color,
        metadata,
    })
}
