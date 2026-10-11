//! Single-channel and planar RGB float images with the filters and
//! pyramids the merges share. Moved from the app's focus_stacking.rs
//! (RapidRAW, Timon Käch; AGPL-3.0).
use image::Rgb32FImage;
use rayon::prelude::*;

#[derive(Clone, Debug)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl Plane {
    pub fn new(w: usize, h: usize) -> Self {
        Plane {
            w,
            h,
            data: vec![0.0; w * h],
        }
    }

    pub fn filled(w: usize, h: usize, v: f32) -> Self {
        Plane {
            w,
            h,
            data: vec![v; w * h],
        }
    }

    #[inline(always)]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }

    #[inline(always)]
    pub fn clamped(&self, x: i64, y: i64) -> f32 {
        let xc = x.clamp(0, self.w as i64 - 1) as usize;
        let yc = y.clamp(0, self.h as i64 - 1) as usize;
        self.data[yc * self.w + xc]
    }

    #[inline]
    pub fn sample_bilinear(&self, x: f32, y: f32) -> f32 {
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let x0 = x0 as i64;
        let y0 = y0 as i64;
        let p00 = self.clamped(x0, y0);
        let p10 = self.clamped(x0 + 1, y0);
        let p01 = self.clamped(x0, y0 + 1);
        let p11 = self.clamped(x0 + 1, y0 + 1);
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        top + (bot - top) * fy
    }

    #[inline]
    pub fn sample_catmull_rom(&self, x: f32, y: f32) -> f32 {
        let xf = x.floor();
        let yf = y.floor();
        let tx = x - xf;
        let ty = y - yf;
        let ix = xf as i64;
        let iy = yf as i64;

        let wx = catmull_weights(tx);
        let wy = catmull_weights(ty);

        let mut acc = 0.0f32;
        for (j, &wy_j) in wy.iter().enumerate() {
            let mut row = 0.0f32;
            for (i, &wx_i) in wx.iter().enumerate() {
                row += wx_i * self.clamped(ix - 1 + i as i64, iy - 1 + j as i64);
            }
            acc += wy_j * row;
        }
        acc
    }

    pub fn map<F: Fn(f32) -> f32 + Sync + Send>(&self, f: F) -> Plane {
        let mut out = Plane::new(self.w, self.h);
        out.data
            .par_iter_mut()
            .zip(self.data.par_iter())
            .for_each(|(o, &v)| *o = f(v));
        out
    }

    pub fn mean(&self) -> f64 {
        let s: f64 = self.data.par_iter().map(|&v| v as f64).sum();
        s / self.data.len().max(1) as f64
    }

    pub fn gradients(&self) -> (Plane, Plane) {
        let mut gx = Plane::new(self.w, self.h);
        let mut gy = Plane::new(self.w, self.h);
        let (w, h) = (self.w, self.h);
        gx.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out_val) in row.iter_mut().enumerate() {
                let a = self.clamped(x as i64 - 1, y as i64);
                let b = self.clamped(x as i64 + 1, y as i64);
                *out_val = 0.5 * (b - a);
            }
        });
        gy.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out_val) in row.iter_mut().enumerate() {
                let a = self.clamped(x as i64, y as i64 - 1);
                let b = self.clamped(x as i64, y as i64 + 1);
                *out_val = 0.5 * (b - a);
            }
        });
        let _ = h;
        (gx, gy)
    }
}

#[inline(always)]
fn catmull_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(1e-3);
    let radius = (sigma * 3.0).ceil().max(1.0) as usize;
    let mut k = Vec::with_capacity(2 * radius + 1);
    let denom = 2.0 * sigma * sigma;
    for i in 0..(2 * radius + 1) {
        let d = i as f32 - radius as f32;
        k.push((-d * d / denom).exp());
    }
    let sum: f32 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= sum;
    }
    k
}

pub fn convolve_separable(src: &Plane, kernel: &[f32]) -> Plane {
    let (w, h) = (src.w, src.h);
    if w == 0 || h == 0 {
        return src.clone();
    }
    let r_kern = (kernel.len() / 2) as i64;

    let mut tmp = Plane::new(w, h);
    tmp.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out_val) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (i, &k) in kernel.iter().enumerate() {
                let sx = (x as i64 + i as i64 - r_kern).clamp(0, w as i64 - 1) as usize;
                acc += k * src.data[y * w + sx];
            }
            *out_val = acc;
        }
    });

    let mut out = Plane::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out_val) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (i, &k) in kernel.iter().enumerate() {
                let sy = (y as i64 + i as i64 - r_kern).clamp(0, h as i64 - 1) as usize;
                acc += k * tmp.data[sy * w + x];
            }
            *out_val = acc;
        }
    });
    out
}

pub fn gaussian_blur(src: &Plane, sigma: f32) -> Plane {
    convolve_separable(src, &gaussian_kernel(sigma))
}

pub fn box_filter(src: &Plane, radius: usize) -> Plane {
    let (w, h) = (src.w, src.h);
    if w == 0 || h == 0 {
        return src.clone();
    }
    let stride = w + 1;
    let mut sat = vec![0f64; stride * (h + 1)];
    for y in 0..h {
        let mut row_sum = 0f64;
        for x in 0..w {
            row_sum += src.data[y * w + x] as f64;
            sat[(y + 1) * stride + (x + 1)] = sat[y * stride + (x + 1)] + row_sum;
        }
    }

    let mut out = Plane::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(h);
        for (x, out_val) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(w);
            let s = sat[y1 * stride + x1] - sat[y0 * stride + x1] - sat[y1 * stride + x0]
                + sat[y0 * stride + x0];
            let n = ((y1 - y0) * (x1 - x0)) as f64;
            *out_val = (s / n) as f32;
        }
    });
    out
}

pub fn downsample(src: &Plane) -> Plane {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let blurred = convolve_separable(src, &K);
    let w2 = src.w.div_ceil(2).max(1);
    let h2 = src.h.div_ceil(2).max(1);
    let mut out = Plane::new(w2, h2);
    out.data
        .par_chunks_mut(w2)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = (2 * y).min(src.h.saturating_sub(1));
            for (x, out_val) in row.iter_mut().enumerate() {
                let sx = (2 * x).min(src.w.saturating_sub(1));
                *out_val = blurred.data[sy * src.w + sx];
            }
        });
    out
}

pub fn upsample_to(src: &Plane, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    if src.w == 0 || src.h == 0 || w == 0 || h == 0 {
        return out;
    }
    let sx_ratio = src.w as f32 / w as f32;
    let sy_ratio = src.h as f32 / h as f32;
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = ((y as f32 + 0.5) * sy_ratio - 0.5).max(0.0);
        for (x, out_val) in row.iter_mut().enumerate() {
            let sx = ((x as f32 + 0.5) * sx_ratio - 0.5).max(0.0);
            *out_val = src.sample_bilinear(sx, sy);
        }
    });
    out
}

pub fn resize_to(src: &Plane, w: usize, h: usize) -> Plane {
    if src.w == w && src.h == h {
        return src.clone();
    }
    let mut cur = src.clone();
    while cur.w >= 2 * w && cur.h >= 2 * h && cur.w > 2 && cur.h > 2 {
        cur = downsample(&cur);
    }
    if cur.w == w && cur.h == h {
        cur
    } else {
        upsample_to(&cur, w, h)
    }
}

pub fn plane_sub(a: &Plane, b: &Plane) -> Plane {
    let mut out = Plane::new(a.w, a.h);
    out.data
        .par_iter_mut()
        .zip(a.data.par_iter().zip(b.data.par_iter()))
        .for_each(|(o, (&x, &y))| *o = x - y);
    out
}

#[derive(Clone, Debug)]
pub struct PlanarRgb {
    pub w: usize,
    pub h: usize,
    pub c: [Plane; 3],
}

impl PlanarRgb {
    pub fn new(w: usize, h: usize) -> Self {
        PlanarRgb {
            w,
            h,
            c: [Plane::new(w, h), Plane::new(w, h), Plane::new(w, h)],
        }
    }

    pub fn from_rgb32f(img: &Rgb32FImage) -> Self {
        let w = img.width() as usize;
        let h = img.height() as usize;
        let raw = img.as_raw();
        let mut out = PlanarRgb::new(w, h);
        for ch in 0..3 {
            out.c[ch]
                .data
                .par_iter_mut()
                .enumerate()
                .for_each(|(i, v)| *v = raw[i * 3 + ch]);
        }
        out
    }

    pub fn to_rgb32f(&self) -> Rgb32FImage {
        let mut img = Rgb32FImage::new(self.w as u32, self.h as u32);
        {
            let raw = img.as_mut();
            raw.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
                px[0] = self.c[0].data[i];
                px[1] = self.c[1].data[i];
                px[2] = self.c[2].data[i];
            });
        }
        img
    }

    pub fn luma(&self) -> Plane {
        let mut out = Plane::new(self.w, self.h);
        out.data.par_iter_mut().enumerate().for_each(|(i, v)| {
            *v = 0.2126 * self.c[0].data[i]
                + 0.7152 * self.c[1].data[i]
                + 0.0722 * self.c[2].data[i];
        });
        out
    }

    pub fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> PlanarRgb {
        let mut out = PlanarRgb::new(w, h);
        for ch in 0..3 {
            let src = &self.c[ch];
            out.c[ch]
                .data
                .par_chunks_mut(w)
                .enumerate()
                .for_each(|(y, row)| {
                    let sy = y + y0;
                    row.copy_from_slice(&src.data[sy * src.w + x0..sy * src.w + x0 + w]);
                });
        }
        out
    }
}

pub fn level_count(w: usize, h: usize, max_levels: usize) -> usize {
    let mut n = 1usize;
    let (mut cw, mut ch) = (w, h);
    while n < max_levels && cw.min(ch) > 16 {
        cw = cw.div_ceil(2);
        ch = ch.div_ceil(2);
        n += 1;
    }
    n
}

pub fn gaussian_pyramid(base: &Plane, levels: usize) -> Vec<Plane> {
    let mut v = Vec::with_capacity(levels);
    v.push(base.clone());
    for _ in 1..levels {
        let next = downsample(v.last().unwrap());
        v.push(next);
    }
    v
}

#[derive(Clone, Debug)]
pub struct LaplacianPyramid {
    pub details: Vec<Plane>,
    pub residual: Plane,
}

pub fn laplacian_pyramid(base: &Plane, levels: usize) -> LaplacianPyramid {
    let g = gaussian_pyramid(base, levels);
    let mut details = Vec::with_capacity(g.len().saturating_sub(1));
    for i in 0..g.len() - 1 {
        let up = upsample_to(&g[i + 1], g[i].w, g[i].h);
        details.push(plane_sub(&g[i], &up));
    }
    LaplacianPyramid {
        residual: g.last().unwrap().clone(),
        details,
    }
}

pub fn collapse_pyramid(lp: &LaplacianPyramid) -> Plane {
    let mut cur = lp.residual.clone();
    for d in lp.details.iter().rev() {
        let up = upsample_to(&cur, d.w, d.h);
        let mut next = up;
        next.data
            .iter_mut()
            .zip(d.data.iter())
            .for_each(|(o, &v)| *o += v);
        cur = next;
    }
    cur
}
