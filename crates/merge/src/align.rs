//! Dense photometric alignment of one frame to a reference: a lens-aware
//! warp (affine, radial, cubic polynomial) solved coarse to fine with gain
//! and bias. Moved from the app's focus_stacking.rs (RapidRAW, Timon Käch;
//! AGPL-3.0).
use crate::planes::*;
use nalgebra::{DMatrix, DVector};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WarpModel {
    Translation,
    Similarity,
    AffineRadial,
    AffineCubic,
}

#[inline(always)]
fn poly_basis(qx: f64, qy: f64) -> [f64; 7] {
    let (qx2, qy2) = (qx * qx, qy * qy);
    [qx2, qx * qy, qy2, qx2 * qx, qx2 * qy, qx * qy2, qy2 * qy]
}

impl WarpModel {
    pub fn n_params(&self) -> usize {
        match self {
            WarpModel::Translation => 2,
            WarpModel::Similarity => 4,
            WarpModel::AffineRadial => 7,
            WarpModel::AffineCubic => 21,
        }
    }

    fn prior(&self, i: usize) -> f64 {
        match self {
            WarpModel::AffineCubic => {
                if i < 6 {
                    0.0
                } else if i == 20 {
                    2e-3
                } else {
                    let m = (i - 6) % 7;
                    if m < 3 { 3e-4 } else { 1e-3 }
                }
            }
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LensWarp {
    pub a: [f64; 4],
    pub t: [f64; 2],
    pub k1: f64,
    pub k2: f64,
    pub pu: [f64; 7],
    pub pv: [f64; 7],
    pub cx: f64,
    pub cy: f64,
    pub norm: f64,
}

impl LensWarp {
    pub fn identity(w: usize, h: usize) -> Self {
        LensWarp {
            a: [1.0, 0.0, 0.0, 1.0],
            t: [0.0, 0.0],
            k1: 0.0,
            k2: 0.0,
            pu: [0.0; 7],
            pv: [0.0; 7],
            cx: w as f64 / 2.0,
            cy: h as f64 / 2.0,
            norm: (0.5 * ((w * w + h * h) as f64).sqrt()).max(1.0),
        }
    }

    pub fn rescaled(&self, factor: f64) -> Self {
        LensWarp {
            a: self.a,
            t: [self.t[0] * factor, self.t[1] * factor],
            k1: self.k1,
            k2: self.k2,
            pu: self.pu,
            pv: self.pv,
            cx: self.cx * factor,
            cy: self.cy * factor,
            norm: self.norm * factor,
        }
    }

    pub fn fold_radial_into_poly(&mut self) {
        if self.k1 == 0.0 {
            return;
        }
        self.pu[3] += self.k1;
        self.pu[5] += self.k1;
        self.pv[4] += self.k1;
        self.pv[6] += self.k1;
        self.k1 = 0.0;
    }

    #[inline(always)]
    fn predistort(&self, x: f64, y: f64) -> (f64, f64, f64, f64, [f64; 7]) {
        let qx = (x - self.cx) / self.norm;
        let qy = (y - self.cy) / self.norm;
        let basis = poly_basis(qx, qy);
        let r2 = qx * qx + qy * qy;
        let rad = self.k1 * r2 + self.k2 * r2 * r2;
        let mut du = qx * rad;
        let mut dv = qy * rad;
        for (m, &b) in basis.iter().enumerate() {
            du += self.pu[m] * b;
            dv += self.pv[m] * b;
        }
        (
            (qx + du) * self.norm + self.cx,
            (qy + dv) * self.norm + self.cy,
            qx,
            qy,
            basis,
        )
    }

    #[inline(always)]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py, _, _, _) = self.predistort(x, y);
        (
            self.a[0] * px + self.a[1] * py + self.t[0],
            self.a[2] * px + self.a[3] * py + self.t[1],
        )
    }

    pub fn max_displacement(&self, w: usize, h: usize) -> f64 {
        let mut m: f64 = 0.0;
        for gy in 0..5 {
            for gx in 0..5 {
                let x = gx as f64 * (w.max(2) - 1) as f64 / 4.0;
                let y = gy as f64 * (h.max(2) - 1) as f64 / 4.0;
                let (u, v) = self.apply(x, y);
                m = m.max(((u - x).powi(2) + (v - y).powi(2)).sqrt());
            }
        }
        m
    }

    #[inline]
    fn jacobian(&self, x: f64, y: f64, model: WarpModel, ju: &mut [f64; 21], jv: &mut [f64; 21]) {
        *ju = [0.0; 21];
        *jv = [0.0; 21];
        let (px, py, qx, qy, basis) = self.predistort(x, y);
        match model {
            WarpModel::Translation => {
                ju[0] = 1.0;
                jv[1] = 1.0;
            }
            WarpModel::Similarity => {
                ju[0] = px;
                ju[1] = -py;
                ju[2] = 1.0;
                jv[0] = py;
                jv[1] = px;
                jv[3] = 1.0;
            }
            WarpModel::AffineRadial => {
                ju[0] = px;
                ju[1] = py;
                ju[4] = 1.0;
                jv[2] = px;
                jv[3] = py;
                jv[5] = 1.0;
                let r2 = qx * qx + qy * qy;
                let dk_x = qx * r2 * self.norm;
                let dk_y = qy * r2 * self.norm;
                ju[6] = self.a[0] * dk_x + self.a[1] * dk_y;
                jv[6] = self.a[2] * dk_x + self.a[3] * dk_y;
            }
            WarpModel::AffineCubic => {
                ju[0] = px;
                ju[1] = py;
                ju[4] = 1.0;
                jv[2] = px;
                jv[3] = py;
                jv[5] = 1.0;
                for m in 0..7 {
                    let b = basis[m] * self.norm;
                    ju[6 + m] = self.a[0] * b;
                    jv[6 + m] = self.a[2] * b;
                    ju[13 + m] = self.a[1] * b;
                    jv[13 + m] = self.a[3] * b;
                }
                let r2 = qx * qx + qy * qy;
                let q4x = qx * r2 * r2 * self.norm;
                let q4y = qy * r2 * r2 * self.norm;
                ju[20] = self.a[0] * q4x + self.a[1] * q4y;
                jv[20] = self.a[2] * q4x + self.a[3] * q4y;
            }
        }
    }

    fn update(&mut self, model: WarpModel, d: &[f64]) {
        match model {
            WarpModel::Translation => {
                self.t[0] += d[0];
                self.t[1] += d[1];
            }
            WarpModel::Similarity => {
                let (da, db) = (d[0], d[1]);
                let a = self.a;
                let m = [1.0 + da, -db, db, 1.0 + da];
                self.a = [
                    m[0] * a[0] + m[1] * a[2],
                    m[0] * a[1] + m[1] * a[3],
                    m[2] * a[0] + m[3] * a[2],
                    m[2] * a[1] + m[3] * a[3],
                ];
                self.t[0] += d[2];
                self.t[1] += d[3];
            }
            WarpModel::AffineRadial => {
                self.a[0] += d[0];
                self.a[1] += d[1];
                self.a[2] += d[2];
                self.a[3] += d[3];
                self.t[0] += d[4];
                self.t[1] += d[5];
                self.k1 += d[6];
            }
            WarpModel::AffineCubic => {
                self.a[0] += d[0];
                self.a[1] += d[1];
                self.a[2] += d[2];
                self.a[3] += d[3];
                self.t[0] += d[4];
                self.t[1] += d[5];
                for m in 0..7 {
                    self.pu[m] += d[6 + m];
                    self.pv[m] += d[13 + m];
                }
                self.k2 += d[20];
            }
        }
    }

    pub fn is_plausible(&self) -> bool {
        let det = self.a[0] * self.a[3] - self.a[1] * self.a[2];
        if !det.is_finite() || det <= 0.5 || det >= 2.0 {
            return false;
        }
        if self.k1.abs() >= 0.5
            || self.k2.abs() >= 0.5
            || !self.t[0].is_finite()
            || !self.t[1].is_finite()
        {
            return false;
        }
        self.pu.iter().chain(self.pv.iter()).all(|c| c.abs() < 0.2)
    }

    pub fn to_vector(self) -> [f64; 22] {
        let mut v = [0f64; 22];
        v[0..4].copy_from_slice(&self.a);
        v[4] = self.t[0];
        v[5] = self.t[1];
        v[6] = self.k1;
        v[7..14].copy_from_slice(&self.pu);
        v[14..21].copy_from_slice(&self.pv);
        v[21] = self.k2;
        v
    }

    pub fn with_vector(mut self, v: &[f64; 22]) -> LensWarp {
        self.a.copy_from_slice(&v[0..4]);
        self.t = [v[4], v[5]];
        self.k1 = v[6];
        self.pu.copy_from_slice(&v[7..14]);
        self.pv.copy_from_slice(&v[14..21]);
        self.k2 = v[21];
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FramePose {
    pub warp: LensWarp,
    pub gain: f64,
    pub bias: f64,
    pub quality: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct AlignConfig {
    pub finest_dim: usize,
    pub coarsest_dim: usize,
    pub max_iterations: usize,
    pub target_samples: usize,
    pub min_quality: f64,
    pub polynomial_refinement: bool,
    /// The most flexible model used at the finest levels.
    pub max_model: WarpModel,
}

impl Default for AlignConfig {
    fn default() -> Self {
        AlignConfig {
            finest_dim: 2400,
            coarsest_dim: 80,
            max_iterations: 45,
            target_samples: 160_000,
            min_quality: 0.25,
            polynomial_refinement: true,
            max_model: WarpModel::AffineCubic,
        }
    }
}

struct LevelData {
    tmpl: Plane,
    src: Plane,
    gx: Plane,
    gy: Plane,
    scale: f64,
}

pub struct AlignPyramids {
    levels: Vec<LevelData>,
}

pub fn build_align_pyramids(
    tmpl_full: &Plane,
    src_full: &Plane,
    cfg: &AlignConfig,
) -> AlignPyramids {
    let mut start_scale = 1.0f64;
    let mut tmpl = tmpl_full.clone();
    let mut src = src_full.clone();
    while tmpl.w.max(tmpl.h) > cfg.finest_dim && tmpl.w.min(tmpl.h) > 32 {
        tmpl = downsample(&tmpl);
        src = downsample(&src);
        start_scale *= 0.5;
    }

    let mut n = 1usize;
    let (mut cw, mut ch) = (tmpl.w, tmpl.h);
    while cw.max(ch) > cfg.coarsest_dim && cw.min(ch) > 20 {
        cw = cw.div_ceil(2);
        ch = ch.div_ceil(2);
        n += 1;
    }

    let tmpl_s = gaussian_blur(&tmpl, 1.0);
    let src_s = gaussian_blur(&src, 1.0);

    let tp = gaussian_pyramid(&tmpl_s, n);
    let sp = gaussian_pyramid(&src_s, n);

    let mut levels = Vec::with_capacity(tp.len());
    for i in 0..tp.len() {
        let (gx, gy) = sp[i].gradients();
        levels.push(LevelData {
            tmpl: tp[i].clone(),
            src: sp[i].clone(),
            gx,
            gy,
            scale: start_scale * 0.5f64.powi(i as i32),
        });
    }
    AlignPyramids { levels }
}

struct Sample {
    ju: [f64; 21],
    jv: [f64; 21],
    gx: f64,
    gy: f64,
    val: f64,
    residual: f64,
}

fn model_for_level(short_side: usize, cfg: &AlignConfig) -> WarpModel {
    model_for_size(short_side, cfg).min(cfg.max_model)
}

fn model_for_size(short_side: usize, cfg: &AlignConfig) -> WarpModel {
    if short_side < 48 {
        WarpModel::Translation
    } else if short_side < 110 {
        WarpModel::Similarity
    } else if short_side < 300 || !cfg.polynomial_refinement {
        WarpModel::AffineRadial
    } else {
        WarpModel::AffineCubic
    }
}

pub fn solve_alignment(pyr: &AlignPyramids, init: LensWarp, cfg: &AlignConfig) -> FramePose {
    let mut warp_full = init;
    let mut gain = 1.0f64;
    let mut bias = 0.0f64;
    let mut quality = 0.0f64;

    for li in (0..pyr.levels.len()).rev() {
        let lvl = &pyr.levels[li];
        let model = model_for_level(lvl.tmpl.w.min(lvl.tmpl.h), cfg);

        let mut w = warp_full.rescaled(lvl.scale);
        if model == WarpModel::AffineCubic {
            w.fold_radial_into_poly();
        }
        let q = solve_level(lvl, &mut w, &mut gain, &mut bias, model, cfg);
        if w.is_plausible() {
            warp_full = w.rescaled(1.0 / lvl.scale);
            quality = q;
        }
    }

    FramePose {
        warp: warp_full,
        gain,
        bias,
        quality,
    }
}

fn solve_level(
    lvl: &LevelData,
    warp: &mut LensWarp,
    gain: &mut f64,
    bias: &mut f64,
    model: WarpModel,
    cfg: &AlignConfig,
) -> f64 {
    let np = model.n_params();
    let n = np + 2;
    let (w, h) = (lvl.tmpl.w, lvl.tmpl.h);
    if w < 8 || h < 8 {
        return 0.0;
    }

    let px_total = w * h;
    let stride = ((px_total as f64 / cfg.target_samples as f64).sqrt().ceil() as usize).max(1);

    let tmpl_mean = lvl.tmpl.mean();
    let tmpl_var: f64 = lvl
        .tmpl
        .data
        .par_iter()
        .map(|&v| {
            let d = v as f64 - tmpl_mean;
            d * d
        })
        .sum::<f64>()
        / px_total as f64;
    let tmpl_var = tmpl_var.max(1e-9);

    let mut lambda = 1e-3f64;
    let mut prev_cost = f64::INFINITY;

    for _iter in 0..cfg.max_iterations {
        let samples = collect_samples(lvl, warp, *gain, *bias, model, stride);
        if samples.len() < n * 8 {
            return 0.0;
        }

        let mut abs_res: Vec<f64> = samples.iter().map(|s| s.residual.abs()).collect();
        let mid = abs_res.len() / 2;
        abs_res.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap());
        let mad = abs_res[mid].max(1e-6);
        let huber = 1.5 * 1.4826 * mad;

        let (hess, grad, cost, wsum) = samples
            .par_iter()
            .fold(
                || (vec![0f64; n * n], vec![0f64; n], 0f64, 0f64),
                |mut acc, s| {
                    let r = s.residual;
                    let wgt = if r.abs() <= huber {
                        1.0
                    } else {
                        huber / r.abs()
                    };
                    let mut j = [0f64; 23];
                    for (p, jp) in j.iter_mut().enumerate().take(np) {
                        *jp = *gain * (s.gx * s.ju[p] + s.gy * s.jv[p]);
                    }
                    j[np] = s.val;
                    j[np + 1] = 1.0;
                    for a in 0..n {
                        acc.1[a] += wgt * j[a] * r;
                        for b in a..n {
                            acc.0[a * n + b] += wgt * j[a] * j[b];
                        }
                    }
                    acc.2 += wgt * r * r;
                    acc.3 += wgt;
                    acc
                },
            )
            .reduce(
                || (vec![0f64; n * n], vec![0f64; n], 0f64, 0f64),
                |mut a, b| {
                    for i in 0..n * n {
                        a.0[i] += b.0[i];
                    }
                    for i in 0..n {
                        a.1[i] += b.1[i];
                    }
                    a.2 += b.2;
                    a.3 += b.3;
                    a
                },
            );

        let cost = cost / wsum.max(1e-12);

        if cost > prev_cost {
            lambda *= 8.0;
            if lambda > 1e6 {
                break;
            }
        } else {
            lambda = (lambda * 0.5).max(1e-8);
        }
        prev_cost = prev_cost.min(cost);

        let mut hm = DMatrix::<f64>::zeros(n, n);
        for a in 0..n {
            for b in a..n {
                let v = hess[a * n + b];
                hm[(a, b)] = v;
                hm[(b, a)] = v;
            }
        }
        let trace: f64 = (0..n).map(|a| hm[(a, a)]).sum::<f64>() / n as f64;
        for a in 0..n {
            let d = hm[(a, a)];
            let prior = if a < np { model.prior(a) } else { 0.0 };
            hm[(a, a)] = d + lambda * d.max(1e-9) + prior * trace;
        }
        let gv = DVector::<f64>::from_iterator(n, grad.iter().map(|&v| -v));

        let delta = match hm.lu().solve(&gv) {
            Some(d) => d,
            None => break,
        };
        if delta.iter().any(|v| !v.is_finite()) {
            break;
        }

        let mut trial = *warp;
        trial.update(model, delta.as_slice());
        let trial_gain = *gain + delta[np];
        let trial_bias = *bias + delta[np + 1];

        if !trial.is_plausible() || trial_gain <= 0.1 || trial_gain > 10.0 {
            break;
        }
        *warp = trial;
        *gain = trial_gain;
        *bias = trial_bias;

        let shift = delta.iter().take(np).fold(0f64, |m, &v| m.max(v.abs()));
        if shift < 1e-6 {
            break;
        }
    }

    (1.0 - prev_cost / tmpl_var).clamp(0.0, 1.0)
}

fn collect_samples(
    lvl: &LevelData,
    warp: &LensWarp,
    gain: f64,
    bias: f64,
    model: WarpModel,
    stride: usize,
) -> Vec<Sample> {
    let (w, h) = (lvl.tmpl.w, lvl.tmpl.h);
    let rows: Vec<usize> = (0..h).step_by(stride).collect();
    rows.par_iter()
        .flat_map_iter(|&y| {
            let mut local = Vec::with_capacity(w / stride + 1);
            let mut ju = [0f64; 21];
            let mut jv = [0f64; 21];
            let mut x = 0usize;
            while x < w {
                let (u, v) = warp.apply(x as f64, y as f64);
                if u >= 1.0 && v >= 1.0 && u < (lvl.src.w - 2) as f64 && v < (lvl.src.h - 2) as f64
                {
                    let val = lvl.src.sample_bilinear(u as f32, v as f32) as f64;
                    let gx = lvl.gx.sample_bilinear(u as f32, v as f32) as f64;
                    let gy = lvl.gy.sample_bilinear(u as f32, v as f32) as f64;
                    if gx * gx + gy * gy > 1e-10 {
                        warp.jacobian(x as f64, y as f64, model, &mut ju, &mut jv);
                        let t = lvl.tmpl.at(x, y) as f64;
                        local.push(Sample {
                            ju,
                            jv,
                            gx,
                            gy,
                            val,
                            residual: gain * val + bias - t,
                        });
                    }
                }
                x += stride;
            }
            local.into_iter()
        })
        .collect()
}

fn erode_mask(mask: &Plane, radius: usize) -> Plane {
    if radius == 0 {
        return mask.clone();
    }
    let (w, h) = (mask.w, mask.h);
    let r = radius as i64;
    let mut tmp = Plane::new(w, h);
    tmp.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out_val) in row.iter_mut().enumerate() {
            let mut m = f32::INFINITY;
            for d in -r..=r {
                m = m.min(mask.clamped(x as i64 + d, y as i64));
            }
            *out_val = m;
        }
    });
    let mut out = Plane::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out_val) in row.iter_mut().enumerate() {
            let mut m = f32::INFINITY;
            for d in -r..=r {
                m = m.min(tmp.clamped(x as i64, y as i64 + d));
            }
            *out_val = m;
        }
    });
    out
}

pub fn warp_frame(
    src: &PlanarRgb,
    warp: &LensWarp,
    gain: f32,
    bias: f32,
    out_w: usize,
    out_h: usize,
) -> (PlanarRgb, Plane) {
    let mut out = PlanarRgb::new(out_w, out_h);
    let mut mask = Plane::new(out_w, out_h);

    let coords: Vec<(f32, f32, bool)> = {
        let mut v = vec![(0f32, 0f32, false); out_w * out_h];
        v.par_chunks_mut(out_w).enumerate().for_each(|(y, row)| {
            for (x, r) in row.iter_mut().enumerate() {
                let (u, vv) = warp.apply(x as f64, y as f64);
                let ok =
                    u >= 2.0 && vv >= 2.0 && u <= (src.w - 3) as f64 && vv <= (src.h - 3) as f64;
                *r = (u as f32, vv as f32, ok);
            }
        });
        v
    };

    mask.data.par_iter_mut().enumerate().for_each(|(i, m)| {
        *m = if coords[i].2 { 1.0 } else { 0.0 };
    });
    let mask = erode_mask(&mask, 2);

    for ch in 0..3 {
        let s = &src.c[ch];
        out.c[ch]
            .data
            .par_iter_mut()
            .enumerate()
            .for_each(|(i, o)| {
                let (u, v, _) = coords[i];
                *o = s.sample_catmull_rom(u, v) * gain + bias;
            });
    }

    (out, mask)
}
