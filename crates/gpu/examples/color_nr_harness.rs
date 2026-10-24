// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measurement harness for colour noise reduction.
//!
//! Synthetic scenes with a known clean image: flat patches (bright, mid and
//! dark), fine saturated lines and dots on two backgrounds, colour edges
//! between patches, iso-luminant colour edges and low-contrast colour
//! texture. Each scene is mosaicked (Bayer RGGB or X-Trans) through a
//! made-up camera, given shot and read noise at several ISO-equivalent
//! levels plus low-frequency colour blotches, then developed as the app
//! develops a RAW (white balance, Rawtonic's demosaic, camera matrix and the
//! decode-time chroma clean-up). The same development of the noise-free
//! mosaic is the ground truth. Both go through the export renderer (16-bit,
//! general shader); metrics compare the renders in Oklab (×100):
//!
//! - `noise`: RMS chroma error (a, b) on patch interiors, and per scale
//!   (`fine` < 1 px, `medium` 1–3 px, `coarse` 3–8 px, `blotch` > 8 px
//!   Gaussian bands of the error);
//! - `shift`: mean chroma error per patch (colour shift of large areas);
//! - `detail_de`: mean CIEDE2000 on fine coloured lines and dots;
//! - `texture_gain`: how much of the true low-contrast colour texture is
//!   kept (regression slope of the output's on the truth's chroma band);
//! - `bleed` / `bleed_iso`: chroma error (2 px blurred, so noise averages
//!   out) within 6 px of colour edges with / without a luminance step.
//!
//! `cargo run -p rawtonic-gpu --release --example color_nr_harness`
//! (`-- --raw FILE...` measures real RAWs without a ground truth: chroma
//! noise on the flattest fifth and colour detail on the most textured
//! twentieth, relative to the render without colour noise reduction).

// Oklab and CIELAB constants are quoted at their published precision.
#![allow(clippy::excessive_precision, clippy::needless_range_loop)]

use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::color_noise::{self, Settings, Tuning};
use rawtonic_engine::image_processing::{
    get_all_adjustments_from_json, remove_raw_artifacts_and_enhance,
};
use rawtonic_gpu::gpu_processing::{
    GpuContext, GpuImageCache, GpuProcessorSlot, ReadbackShader, RenderOutputPrecision,
    RenderRequest, RenderSource, process_and_get_dynamic_image_inner,
};
use rawtonic_rawdecode::demosaic::{CfaPattern, demosaic};
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE: usize = 768;

// ---------- scene ----------

struct Scene {
    rgb: Vec<[f32; 3]>,
    flat: Vec<u16>, // patch id + 1 for patch interiors
    detail: Vec<bool>,
    edge: Vec<bool>,
    edge_iso: Vec<bool>,
    texture: Vec<bool>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
    fn gaussian(&mut self) -> f32 {
        let u1 = self.next().max(1e-7);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

fn scene(seed: u64) -> Scene {
    let n = SIZE * SIZE;
    let mut s = Scene {
        rgb: vec![[0.0; 3]; n],
        flat: vec![0; n],
        detail: vec![false; n],
        edge: vec![false; n],
        edge_iso: vec![false; n],
        texture: vec![false; n],
    };
    let patches: [[f32; 3]; 8] = [
        [0.45, 0.30, 0.22],
        [0.18, 0.30, 0.55],
        [0.10, 0.18, 0.05],
        [0.18, 0.18, 0.18],
        [0.05, 0.012, 0.010],
        [0.010, 0.012, 0.030],
        [0.02, 0.02, 0.02],
        [0.55, 0.45, 0.08],
    ];
    let mut rng = Rng(seed);
    // Patch order differs per seed.
    let mut order: Vec<usize> = (0..8).collect();
    for i in (1..8).rev() {
        let j = (rng.next() * (i + 1) as f32) as usize % (i + 1);
        order.swap(i, j);
    }
    let p = 192;
    for y in 0..384 {
        for x in 0..SIZE {
            let (px, py) = (x / p, y / p);
            let id = order[py * 4 + px];
            let i = y * SIZE + x;
            s.rgb[i] = patches[id];
            let (lx, ly) = (x % p, y % p);
            if (20..p - 20).contains(&lx) && (20..p - 20).contains(&ly) {
                s.flat[i] = id as u16 + 1;
            }
            // Distance to an inner patch boundary.
            let dx = if px > 0 && lx < 6 {
                Some(lx)
            } else if px < 3 && lx >= p - 6 {
                Some(p - 1 - lx)
            } else {
                None
            };
            let dy = if py > 0 && ly < 6 {
                Some(ly)
            } else if py < 1 && ly >= p - 6 {
                Some(p - 1 - ly)
            } else {
                None
            };
            if dx.is_some() || dy.is_some() {
                s.edge[i] = true;
            }
        }
    }
    // Fine colour detail on grey and skin backgrounds.
    let colours: [[f32; 3]; 6] = [
        [0.6, 0.05, 0.04],
        [0.05, 0.35, 0.05],
        [0.04, 0.08, 0.6],
        [0.5, 0.06, 0.45],
        [0.65, 0.3, 0.02],
        [0.05, 0.4, 0.45],
    ];
    for y in 384..SIZE {
        for x in 0..384 {
            s.rgb[y * SIZE + x] = if x < 192 {
                [0.15, 0.15, 0.15]
            } else {
                [0.45, 0.30, 0.22]
            };
        }
    }
    let paint = |s: &mut Scene, x: i32, y: i32, c: [f32; 3]| {
        if (0..384).contains(&x) && (384..SIZE as i32).contains(&y) {
            let i = y as usize * SIZE + x as usize;
            s.rgb[i] = c;
            s.detail[i] = true;
        }
    };
    for _ in 0..60 {
        let x0 = (rng.next() * 340.0) as i32 + 20;
        let y0 = (rng.next() * 340.0) as i32 + 404;
        let c = colours[(rng.next() * 6.0) as usize % 6];
        let width = 1 + (rng.next() * 3.0) as i32 % 3;
        let len = 15 + (rng.next() * 40.0) as i32;
        let dir = (rng.next() * 4.0) as i32 % 4;
        for t in 0..len {
            for w in 0..width {
                let (x, y) = match dir {
                    0 => (x0 + t, y0 + w),
                    1 => (x0 + w, y0 + t),
                    2 => (x0 + t + w, y0 + t),
                    _ => (x0 + t + w, y0 - t),
                };
                paint(&mut s, x, y, c);
            }
        }
    }
    for _ in 0..60 {
        let cx = (rng.next() * 340.0) as i32 + 20;
        let cy = (rng.next() * 340.0) as i32 + 404;
        let r = 1 + (rng.next() * 3.0) as i32 % 3;
        let c = colours[(rng.next() * 6.0) as usize % 6];
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    paint(&mut s, cx + dx, cy + dy, c);
                }
            }
        }
    }
    // Iso-luminant stripes.
    let (a, b) = ([0.30, 0.16, 0.14], [0.12, 0.21, 0.17]);
    for y in 384..576 {
        for x in 384..SIZE {
            let stripe = (x - 384) / 32;
            let i = y * SIZE + x;
            s.rgb[i] = if stripe % 2 == 0 { a } else { b };
            let lx = (x - 384) % 32;
            if (390..570).contains(&y) && ((lx < 6 && stripe > 0) || lx >= 26 && x < SIZE - 6) {
                s.edge_iso[i] = true;
            }
        }
    }
    // Low-contrast colour texture at two periods (with a luminance part:
    // the decode-time chroma clean-up averages iso-luminant texture away).
    let u = [1.6, 0.35, 0.132];
    for y in 576..SIZE {
        for x in 384..SIZE {
            let period = if x < 576 { 6.0 } else { 14.0 };
            let t = 0.04
                * (std::f32::consts::TAU * x as f32 / period).sin()
                * (std::f32::consts::TAU * y as f32 / period).sin();
            let i = y * SIZE + x;
            s.rgb[i] = [0.25 + t * u[0], 0.22 + t * u[1], 0.20 + t * u[2]];
            if (582..SIZE - 6).contains(&y)
                && (390..SIZE - 6).contains(&x)
                && !(570..582).contains(&x)
            {
                s.texture[i] = true;
            }
        }
    }
    // Detail mask grown by a pixel.
    let grown: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = ((i % SIZE) as i32, (i / SIZE) as i32);
            (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (xx, yy) = (x + dx, y + dy);
                    xx >= 0
                        && yy >= 0
                        && (xx as usize) < SIZE
                        && (yy as usize) < SIZE
                        && s.detail[yy as usize * SIZE + xx as usize]
                })
            })
        })
        .collect();
    s.detail = grown;
    s
}

// ---------- camera ----------

/// Made-up camera → linear sRGB matrix (rows sum to 1) and white balance.
const CAMERA: [[f32; 3]; 3] = [
    [1.75, -0.60, -0.15],
    [-0.20, 1.55, -0.35],
    [0.05, -0.55, 1.50],
];
const WB: [f32; 3] = [2.0, 1.0, 1.6];

fn invert(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let [[a, b, c], [d, e, f], [g, h, i]] = m.map(|r| r.map(f64::from));
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let inv = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    inv.map(|r| r.map(|v| (v / det) as f32))
}

fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn cfa(xtrans: bool) -> CfaPattern {
    if xtrans {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let colors = rows
            .iter()
            .flat_map(|r| {
                r.chars().map(|c| match c {
                    'R' => 0,
                    'G' => 1,
                    _ => 2,
                })
            })
            .collect();
        CfaPattern::new(6, 6, colors).unwrap()
    } else {
        CfaPattern::new(2, 2, vec![0, 1, 1, 2]).unwrap()
    }
}

fn blur_field(field: &mut [f32], sigma: f32) {
    let r = (3.0 * sigma).ceil() as i32;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / sum).collect();
    let mut tmp = vec![0.0f32; field.len()];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut acc = 0.0;
            for (j, w) in k.iter().enumerate() {
                let xx = (x as i32 + j as i32 - r).clamp(0, SIZE as i32 - 1) as usize;
                acc += w * field[y * SIZE + xx];
            }
            tmp[y * SIZE + x] = acc;
        }
    }
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut acc = 0.0;
            for (j, w) in k.iter().enumerate() {
                let yy = (y as i32 + j as i32 - r).clamp(0, SIZE as i32 - 1) as usize;
                acc += w * tmp[yy * SIZE + x];
            }
            field[y * SIZE + x] = acc;
        }
    }
    let rms = (field.iter().map(|v| v * v).sum::<f32>() / field.len() as f32).sqrt();
    for v in field.iter_mut() {
        *v /= rms.max(1e-12);
    }
}

/// Develops the scene through the camera; `iso` 0 = no noise.
fn develop(scene: &Scene, xtrans: bool, iso: f32, seed: u64) -> Rgb32FImage {
    let to_camera = invert(CAMERA);
    let pattern = cfa(xtrans);
    let mut rng = Rng(seed);
    // Electrons at the camera's white (green channel), read noise.
    let full = 40_000.0 * 100.0 / iso.max(1.0);
    let read = 2.5;
    let mut blotch: [Vec<f32>; 3] =
        std::array::from_fn(|_| (0..SIZE * SIZE).map(|_| rng.gaussian()).collect());
    for field in &mut blotch {
        blur_field(field, 5.0);
    }
    let mut mosaic = vec![0.0f32; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = y * SIZE + x;
            let c = pattern.color(y as isize, x as isize);
            let cam = mul(&to_camera, scene.rgb[i]).map(|v| v.max(0.0));
            let raw = cam[c] / WB[c];
            let value = if iso > 0.0 {
                let electrons = raw * full;
                let sigma = (electrons + read * read).sqrt();
                (electrons + sigma * rng.gaussian() + 0.35 * sigma * blotch[c][i]) / full
            } else {
                raw
            };
            mosaic[i] = value * WB[c];
        }
    }
    let rgb = demosaic(&mosaic, SIZE, SIZE, &pattern);
    let data: Vec<f32> = rgb.iter().flat_map(|p| mul(&CAMERA, *p)).collect();
    let mut image =
        DynamicImage::ImageRgb32F(Rgb32FImage::from_raw(SIZE as u32, SIZE as u32, data).unwrap());
    // The app's decode-time preprocessing at its default preferences.
    let defaults = rawtonic_engine::image_loader::BaseImageOptions::default();
    remove_raw_artifacts_and_enhance(
        &mut image,
        defaults.color_nr_amount,
        defaults.sharpening_amount,
    );
    image.into_rgb32f()
}

// ---------- rendering ----------

struct Renderer {
    context: GpuContext,
    processor: GpuProcessorSlot,
    cache: Mutex<Option<GpuImageCache>>,
    serial: AtomicU64,
}

impl Renderer {
    fn new() -> Self {
        Self {
            context: GpuContext::headless().expect("GPU"),
            processor: GpuProcessorSlot::new(None),
            cache: Mutex::new(None),
            serial: AtomicU64::new(1),
        }
    }

    /// 16-bit export render of linear `image` with `recipe` → linear sRGB.
    fn render(
        &self,
        image: &Rgb32FImage,
        recipe: serde_json::Value,
        noise_level: f32,
    ) -> Vec<[f32; 3]> {
        let mut recipe = recipe;
        for key in ["sharpness", "lumaNoiseReduction", "colorNoiseReduction"] {
            recipe
                .as_object_mut()
                .unwrap()
                .entry(key)
                .or_insert(json!(0));
        }
        let mut adjustments = get_all_adjustments_from_json(&recipe, true, None);
        adjustments.uniforms.global.noise_level = noise_level;
        let dynamic = DynamicImage::ImageRgb32F(image.clone());
        let out = process_and_get_dynamic_image_inner(
            &self.context,
            &self.processor,
            &self.cache,
            RenderSource::Image {
                image: &dynamic,
                transform_hash: self.serial.fetch_add(1, Ordering::Relaxed),
            },
            RenderRequest {
                adjustments,
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            "color_nr_harness",
            RenderOutputPrecision::SixteenBit,
            false,
            None,
            None,
            ReadbackShader::Export,
        )
        .expect("render");
        out.to_rgb32f()
            .pixels()
            .map(|p| p.0.map(srgb_to_linear))
            .collect()
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

// ---------- metrics ----------

fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|v| v.max(0.0));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        100.0 * (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s),
        100.0 * (1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s),
        100.0 * (0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s),
    ]
}

fn cielab(rgb: [f32; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|v| f64::from(v.max(0.0)));
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn ciede2000(a: [f64; 3], b: [f64; 3]) -> f64 {
    use std::f64::consts::PI;
    let (l1, a1, b1) = (a[0], a[1], a[2]);
    let (l2, a2, b2) = (b[0], b[1], b[2]);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cm = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hue = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let h = b.atan2(a);
            if h < 0.0 { h + 2.0 * PI } else { h }
        }
    };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let mut dh = h2p - h1p;
    if c1p * c2p == 0.0 {
        dh = 0.0;
    } else if dh > PI {
        dh -= 2.0 * PI;
    } else if dh < -PI {
        dh += 2.0 * PI;
    }
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).sin();
    let lm = (l1 + l2) / 2.0;
    let cmp = (c1p + c2p) / 2.0;
    let hm = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= PI {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 2.0 * PI {
        (h1p + h2p + 2.0 * PI) / 2.0
    } else {
        (h1p + h2p - 2.0 * PI) / 2.0
    };
    let t = 1.0 - 0.17 * (hm - PI / 6.0).cos()
        + 0.24 * (2.0 * hm).cos()
        + 0.32 * (3.0 * hm + PI / 30.0).cos()
        - 0.20 * (4.0 * hm - 63.0 * PI / 180.0).cos();
    let dtheta = PI / 6.0 * (-((hm * 180.0 / PI - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cmp.powi(7) / (cmp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lm - 50.0).powi(2) / (20.0 + (lm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cmp;
    let sh = 1.0 + 0.015 * cmp * t;
    let rt = -(2.0 * dtheta).sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhh / sh).powi(2) + rt * (dc / sc) * (dhh / sh))
        .sqrt()
}

/// Gaussian blur of a two-channel plane.
fn gauss2(plane: &[[f32; 2]], w: usize, h: usize, sigma: f32) -> Vec<[f32; 2]> {
    if sigma <= 0.0 {
        return plane.to_vec();
    }
    let r = (3.0 * sigma).ceil() as i32;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / sum).collect();
    let mut tmp = vec![[0.0f32; 2]; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 2];
            for (j, kw) in k.iter().enumerate() {
                let xx = (x as i32 + j as i32 - r).clamp(0, w as i32 - 1) as usize;
                let v = plane[y * w + xx];
                acc[0] += kw * v[0];
                acc[1] += kw * v[1];
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![[0.0f32; 2]; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 2];
            for (j, kw) in k.iter().enumerate() {
                let yy = (y as i32 + j as i32 - r).clamp(0, h as i32 - 1) as usize;
                let v = tmp[yy * w + x];
                acc[0] += kw * v[0];
                acc[1] += kw * v[1];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

fn rms(values: impl Iterator<Item = [f32; 2]>) -> f64 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for v in values {
        sum += f64::from(v[0] * v[0] + v[1] * v[1]);
        n += 1;
    }
    (sum / n.max(1) as f64).sqrt()
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    noise: f64,
    fine: f64,
    medium: f64,
    coarse: f64,
    blotch: f64,
    shift: f64,
    detail_de: f64,
    texture_gain: f64,
    bleed: f64,
    bleed_iso: f64,
    /// The same filter on the noise-free development (with the noisy
    /// photo's settings): what it does to the scene itself.
    clean_detail_de: f64,
    clean_texture: f64,
}

impl Metrics {
    fn add(&mut self, o: &Metrics, k: f64) {
        self.noise += k * o.noise;
        self.fine += k * o.fine;
        self.medium += k * o.medium;
        self.coarse += k * o.coarse;
        self.blotch += k * o.blotch;
        self.shift += k * o.shift;
        self.detail_de += k * o.detail_de;
        self.texture_gain += k * o.texture_gain;
        self.bleed += k * o.bleed;
        self.bleed_iso += k * o.bleed_iso;
        self.clean_detail_de += k * o.clean_detail_de;
        self.clean_texture += k * o.clean_texture;
    }
}

fn metrics(scene: &Scene, out: &[[f32; 3]], clean: &[[f32; 3]], truth: &[[f32; 3]]) -> Metrics {
    let mut m = noisy_metrics(scene, out, truth);
    let c = noisy_metrics(scene, clean, truth);
    m.bleed = c.bleed;
    m.bleed_iso = c.bleed_iso;
    m.clean_detail_de = c.detail_de;
    m.clean_texture = c.texture_gain;
    m
}

fn noisy_metrics(scene: &Scene, out: &[[f32; 3]], truth: &[[f32; 3]]) -> Metrics {
    let n = SIZE * SIZE;
    let o: Vec<[f32; 3]> = out.iter().map(|p| oklab(*p)).collect();
    let t: Vec<[f32; 3]> = truth.iter().map(|p| oklab(*p)).collect();
    let err: Vec<[f32; 2]> = (0..n)
        .map(|i| [o[i][1] - t[i][1], o[i][2] - t[i][2]])
        .collect();
    let b1 = gauss2(&err, SIZE, SIZE, 1.0);
    let b3 = gauss2(&err, SIZE, SIZE, 3.0);
    let b8 = gauss2(&err, SIZE, SIZE, 8.0);
    let flat = |i: &usize| scene.flat[*i] > 0;
    let band = |a: &Vec<[f32; 2]>, b: &Vec<[f32; 2]>| {
        rms((0..n)
            .filter(flat)
            .map(|i| [a[i][0] - b[i][0], a[i][1] - b[i][1]]))
    };
    let mut m = Metrics {
        noise: rms((0..n).filter(flat).map(|i| err[i])),
        fine: band(&err, &b1),
        medium: band(&b1, &b3),
        coarse: band(&b3, &b8),
        blotch: rms((0..n).filter(flat).map(|i| b8[i])),
        ..Default::default()
    };
    // Mean colour shift per patch.
    let mut shifts = Vec::new();
    for id in 1..=8u16 {
        let (mut s, mut c) = ([0.0f64; 2], 0usize);
        for i in 0..n {
            if scene.flat[i] == id {
                s[0] += f64::from(err[i][0]);
                s[1] += f64::from(err[i][1]);
                c += 1;
            }
        }
        if c > 0 {
            shifts.push(((s[0] / c as f64).powi(2) + (s[1] / c as f64).powi(2)).sqrt());
        }
    }
    m.shift = shifts.iter().sum::<f64>() / shifts.len().max(1) as f64;
    // Fine colour detail.
    let (mut de, mut c) = (0.0f64, 0usize);
    for i in 0..n {
        if scene.detail[i] {
            de += ciede2000(cielab(out[i]), cielab(truth[i]));
            c += 1;
        }
    }
    m.detail_de = de / c.max(1) as f64;
    // Texture: slope of output chroma band on the truth's (band 0–3 px).
    let ob: Vec<[f32; 2]> = o.iter().map(|v| [v[1], v[2]]).collect();
    let tb: Vec<[f32; 2]> = t.iter().map(|v| [v[1], v[2]]).collect();
    let ob_l = gauss2(&ob, SIZE, SIZE, 6.0);
    let tb_l = gauss2(&tb, SIZE, SIZE, 6.0);
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for i in 0..n {
        if scene.texture[i] {
            for k in 0..2 {
                let tv = f64::from(tb[i][k] - tb_l[i][k]);
                let ov = f64::from(ob[i][k] - ob_l[i][k]);
                num += tv * ov;
                den += tv * tv;
            }
        }
    }
    m.texture_gain = num / den.max(1e-12);
    let b2 = gauss2(&err, SIZE, SIZE, 2.0);
    let mean_abs = |mask: &Vec<bool>| {
        let (mut s, mut c) = (0.0f64, 0usize);
        for i in 0..n {
            if mask[i] {
                s += f64::from((b2[i][0] * b2[i][0] + b2[i][1] * b2[i][1]).sqrt());
                c += 1;
            }
        }
        s / c.max(1) as f64
    };
    m.bleed = mean_abs(&scene.edge);
    m.bleed_iso = mean_abs(&scene.edge_iso);
    m
}

fn print(name: &str, m: &Metrics) {
    println!(
        "{name:<22} noise {:5.3} fine {:5.3} med {:5.3} coarse {:5.3} blotch {:5.3} shift {:5.3} | ΔE00 {:5.3} tex {:5.3} | clean: ΔE00 {:5.3} tex {:5.3} bleed {:5.3} iso {:5.3}",
        m.noise,
        m.fine,
        m.medium,
        m.coarse,
        m.blotch,
        m.shift,
        m.detail_de,
        m.texture_gain,
        m.clean_detail_de,
        m.clean_texture,
        m.bleed,
        m.bleed_iso
    );
}

/// The colour noise version measured (`NR_VERSION`, default the newest).
fn version_from_env() -> u64 {
    std::env::var("NR_VERSION")
        .ok()
        .map_or(color_noise::VERSION, |v| v.parse().unwrap())
}

fn tuning_from_env() -> Tuning {
    let mut tuning = Tuning::for_version(version_from_env());
    let list = |key: &str| -> Option<Vec<f32>> {
        std::env::var(key)
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().parse().unwrap()).collect())
    };
    if let Some(v) = list("NR_MULTIPLES") {
        tuning.multiples.copy_from_slice(&v);
    }
    if let Some(v) = list("NR_STEEPNESS") {
        tuning.steepness = v[0];
    }
    if let Some(v) = list("NR_EDGE") {
        tuning.edge_guard = v[0];
    }
    if let Some(v) = list("NR_LUMA") {
        tuning.luma_guard = v[0];
    }
    if let Some(v) = list("NR_GROWTH") {
        tuning.amount_growth = v[0];
    }
    tuning
}

fn synthetic() {
    let renderer = Renderer::new();
    let tuning = tuning_from_env();
    let amounts: Vec<f32> = std::env::var("NR_AMOUNTS")
        .ok()
        .map(|v| v.split(',').map(|s| s.parse().unwrap()).collect())
        .unwrap_or_else(|| vec![25.0, 50.0, 100.0]);
    let gpu_new = std::env::var("NR_GPU").is_ok();
    let isos = [800.0, 3200.0, 12800.0];
    let seeds: Vec<u64> = std::env::var("NR_SEEDS")
        .ok()
        .map(|v| v.split(',').map(|s| s.parse().unwrap()).collect())
        .unwrap_or_else(|| vec![1, 2]);
    let mut total: std::collections::BTreeMap<String, Metrics> = Default::default();
    let mut count = 0.0;
    for xtrans in [false, true] {
        for &seed in &seeds {
            let sc = scene(seed);
            let truth_lin = develop(&sc, xtrans, 0.0, 0);
            let truth = renderer.render(&truth_lin, json!({}), 0.0);
            for &iso in &isos {
                let noisy = develop(&sc, xtrans, iso, seed * 1000 + iso as u64);
                let level = rawtonic_rawdecode::noise_level::measure(&DynamicImage::ImageRgb32F(
                    noisy.clone(),
                ))
                .unwrap_or(0.0);
                let pyramid = color_noise::ChromaPyramid::build(
                    &DynamicImage::ImageRgb32F(noisy.clone()),
                    true,
                )
                .unwrap();
                println!(
                    "-- {} seed {seed} ISO {iso}: luma noise {level:.4}, colour noise per level {:?}",
                    if xtrans { "X-Trans" } else { "Bayer" },
                    pyramid.noise.map(|v| (v * 1e4).round() / 1e4)
                );
                let mut row = |name: String, m: Metrics| {
                    print(&name, &m);
                    let key = format!("{name:<10} ISO {iso:>6}");
                    total.entry(key).or_default().add(&m, 1.0);
                    total
                        .entry(format!("{name:<10} all"))
                        .or_default()
                        .add(&m, 1.0 / 3.0);
                };
                let off = renderer.render(&noisy, json!({}), level);
                row("off".into(), metrics(&sc, &off, &truth, &truth));
                let mut clean_pyramid = color_noise::ChromaPyramid::build(
                    &DynamicImage::ImageRgb32F(truth_lin.clone()),
                    true,
                )
                .unwrap();
                // The noisy photo's settings: its measured luminance noise too.
                clean_pyramid.luma_noise = pyramid.luma_noise;
                clean_pyramid.noise = pyramid.noise;
                clean_pyramid.profile = pyramid.profile;
                for &amount in &amounts {
                    let recipe = json!({"colorNoiseReduction": amount});
                    let old = renderer.render(&noisy, recipe.clone(), level);
                    let old_clean = renderer.render(&truth_lin, recipe, level);
                    row(
                        format!("old {amount}"),
                        metrics(&sc, &old, &old_clean, &truth),
                    );
                    let settings = Settings {
                        amount: amount / 100.0,
                        detail: 0.5,
                        smoothness: 0.5,
                    };
                    let thresholds = tuning.thresholds(settings, &pyramid.noise_for(&tuning));
                    if std::env::var("NR_DEBUG").is_ok() {
                        let filtered = color_noise::apply_with(
                            &truth_lin,
                            &clean_pyramid,
                            &thresholds,
                            tuning.base_sigma(settings),
                            &tuning,
                        );
                        let amp = |img: &Rgb32FImage| {
                            let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
                            for (i, p) in img.pixels().enumerate() {
                                if sc.texture[i] && (i % SIZE) >= 582 {
                                    let c = color_noise::chroma(p.0)[0] as f64;
                                    s += c;
                                    s2 += c * c;
                                    n += 1.0;
                                }
                            }
                            (s2 / n - (s / n).powi(2)).sqrt()
                        };
                        println!(
                            "thresholds {thresholds:?} texture amp truth {} filtered {} clean noise {:?}",
                            amp(&truth_lin),
                            amp(&filtered),
                            clean_pyramid.noise
                        );
                    }
                    let new_clean = renderer.render(
                        &color_noise::apply_with(
                            &truth_lin,
                            &clean_pyramid,
                            &thresholds,
                            tuning.base_sigma(settings),
                            &tuning,
                        ),
                        json!({}),
                        level,
                    );
                    let new = if gpu_new {
                        renderer.render(
                            &noisy,
                            json!({"colorNoiseReduction": amount,
                                "colorNoiseVersion": version_from_env()}),
                            level,
                        )
                    } else {
                        let filtered = color_noise::apply_with(
                            &noisy,
                            &pyramid,
                            &thresholds,
                            tuning.base_sigma(settings),
                            &tuning,
                        );
                        renderer.render(&filtered, json!({}), level)
                    };
                    row(
                        format!("new {amount}"),
                        metrics(&sc, &new, &new_clean, &truth),
                    );
                    if std::env::var("NR_PROFILE").is_ok() {
                        // Oklab a error across two stripe periods, averaged over rows.
                        for (label, img) in [("new", &new_clean), ("old", &old_clean)] {
                            let mut profile = vec![0.0f64; 128];
                            for y in 400..560 {
                                for x in 384..512 {
                                    let i = y * SIZE + x;
                                    profile[x - 384] +=
                                        f64::from(oklab(img[i])[1] - oklab(truth[i])[1]) / 160.0;
                                }
                            }
                            println!(
                                "{label} profile {:?}",
                                profile
                                    .iter()
                                    .map(|v| (v * 100.0).round() / 100.0)
                                    .collect::<Vec<_>>()
                            );
                        }
                    }
                }
            }
            count += 1.0;
        }
    }
    println!("== means over sensors and seeds ==");
    for (name, m) in &total {
        let mut mean = Metrics::default();
        mean.add(m, 1.0 / count);
        print(name, &mean);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--raw") {
        real(&args[1..]);
    } else {
        synthetic();
    }
}

// ---------- real RAWs ----------

fn real(paths: &[String]) {
    let renderer = Renderer::new();
    let tuning = tuning_from_env();
    for path in paths {
        let bytes = std::fs::read(path).expect("read");
        let base = rawtonic_engine::image_loader::load_base_image(
            &bytes,
            path,
            false,
            &rawtonic_engine::image_loader::BaseImageOptions::default(),
            None,
        )
        .expect("decode");
        let base = base.into_rgb32f();
        // Centre crop, 2400 × 1600 at most.
        let (w, h) = (base.width(), base.height());
        let (cw, ch) = (w.min(2400), h.min(1600));
        let crop = image::imageops::crop_imm(&base, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image();
        let level =
            rawtonic_rawdecode::noise_level::measure(&DynamicImage::ImageRgb32F(crop.clone()))
                .unwrap_or(0.0);
        let pyramid =
            color_noise::ChromaPyramid::build(&DynamicImage::ImageRgb32F(crop.clone()), true)
                .unwrap();
        let name = std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy();
        println!(
            "-- {name}: luma noise {level:.4}, colour noise {:?}",
            pyramid.noise.map(|v| (v * 1e4).round() / 1e4)
        );
        let (cw, ch) = (cw as usize, ch as usize);
        let off = renderer.render(&crop, json!({}), level);
        let lab = |img: &[[f32; 3]]| img.iter().map(|p| oklab(*p)).collect::<Vec<_>>();
        let off_lab = lab(&off);
        // Flattest fifth and most textured twentieth of the reference.
        let l: Vec<[f32; 2]> = off_lab.iter().map(|v| [v[0], 0.0]).collect();
        let l2 = gauss2(&l, cw, ch, 2.0);
        let grad: Vec<f32> = (0..cw * ch)
            .map(|i| {
                let (x, y) = (i % cw, i / cw);
                let xr = (x + 1).min(cw - 1);
                let yd = (y + 1).min(ch - 1);
                (l2[y * cw + xr][0] - l2[i][0]).abs() + (l2[yd * cw + x][0] - l2[i][0]).abs()
            })
            .collect();
        let mut sorted = grad.clone();
        sorted.sort_by(f32::total_cmp);
        let flat_max = sorted[sorted.len() / 5];
        let tex_min = sorted[sorted.len() * 19 / 20];
        let measure = |img: &[[f32; 3]]| {
            let o = lab(img);
            let ab: Vec<[f32; 2]> = o.iter().map(|v| [v[1], v[2]]).collect();
            let g2 = gauss2(&ab, cw, ch, 2.0);
            let g07 = gauss2(&ab, cw, ch, 0.7);
            let g8 = gauss2(&ab, cw, ch, 8.0);
            let chroma_noise = rms((0..cw * ch)
                .filter(|i| grad[*i] <= flat_max && (30.0..85.0).contains(&off_lab[*i][0]))
                .map(|i| [ab[i][0] - g2[i][0], ab[i][1] - g2[i][1]]));
            let colour_detail = rms((0..cw * ch)
                .filter(|i| grad[*i] >= tex_min)
                .map(|i| [g07[i][0] - g2[i][0], g07[i][1] - g2[i][1]]));
            (chroma_noise, colour_detail, g8)
        };
        // NR_SAVE=DIR: 8-bit PNG crops of the renders for a visual check.
        let save = |label: &str, img: &[[f32; 3]]| {
            if let Ok(dir) = std::env::var("NR_SAVE") {
                let (sw, sh) = (cw.min(900), ch.min(600));
                let (x0, y0) = ((cw - sw) / 2, (ch - sh) / 2);
                let out = image::RgbImage::from_fn(sw as u32, sh as u32, |x, y| {
                    let p = img[(y0 + y as usize) * cw + x0 + x as usize];
                    image::Rgb(p.map(|v| {
                        let e = if v <= 0.0031308 {
                            12.92 * v
                        } else {
                            1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055
                        };
                        (e.clamp(0.0, 1.0) * 255.0).round() as u8
                    }))
                });
                out.save(format!("{dir}/{name}.{label}.png")).unwrap();
            }
        };
        save("off", &off);
        let (n0, d0, g8_off) = measure(&off);
        println!(
            "{:<10} chroma noise {n0:6.3}  colour detail {d0:6.3}",
            "off"
        );
        for amount in [25.0f32, 50.0] {
            let old = renderer.render(&crop, json!({"colorNoiseReduction": amount}), level);
            let settings = Settings {
                amount: amount / 100.0,
                detail: 0.5,
                smoothness: 0.5,
            };
            let new = color_noise::apply_to_linear(&crop, settings, &tuning);
            let new = renderer.render(&new, json!({}), level);
            for (label, img) in [("old", &old), ("new", &new)] {
                save(&format!("{label}{amount}"), img);
                let (n, d, g8) = measure(img);
                let shift =
                    rms((0..cw * ch).map(|i| [g8[i][0] - g8_off[i][0], g8[i][1] - g8_off[i][1]]));
                println!(
                    "{label} {amount:<5} chroma noise {n:6.3} ({:.2}×)  colour detail {d:6.3} ({:.2}×)  large-area shift {shift:5.3}",
                    n / n0,
                    d / d0
                );
            }
        }
    }
}
