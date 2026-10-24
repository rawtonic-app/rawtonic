// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measurement harness for speckles near bright objects at high ISO.
//!
//! A synthetic scene with a known clean image: a dark background with
//! bright discs and clusters of point lights (blurred by a lens-like point
//! spread with a faint glow), bright saturated patches with fine texture,
//! low-contrast grey texture and flat dark control areas. It is mosaicked
//! through a made-up camera (Bayer RGGB), given shot and read noise at
//! ISO-equivalent levels, developed as the app develops a RAW (white
//! balance, Rawtonic's demosaic, camera matrix, decode-time preprocessing)
//! and rendered by the export renderer at the RAW defaults (Sharpness 40,
//! Luminance 15, Color 25). The same development of the noise-free mosaic
//! is the ground truth. The canvas is 4320 px square so the renderer's
//! blurs have the radii of a 24 MP export.
//!
//! Metrics in Oklab ×100 per zone (`near`: dark pixels 3–24 px from a
//! bright object or patch; `sat`: saturated patches; `dark`: flat dark background;
//! `tex`: low-contrast texture), from the error e = L(render) − L(truth):
//!
//! - `noise`: RMS of e minus its 8 px Gaussian (fine error);
//! - `speck`: isolated outliers per 1000 pixels, |e − median5(e)| > 8
//!   (`dark`: the negative ones), and colour outliers |Δab − median5| > 8;
//! - `edge`: mean 1 px lightness gradient of the render on the edges of the
//!   bright objects ÷ the truth's (sharpness kept);
//! - `texture`: regression slope of the render's fine lightness band
//!   (minus its 8 px Gaussian) on the truth's in `tex` and `sat`.
//!
//! `cargo run -p rawtonic-gpu --release --example speckle_harness`
//! (`SPECKLE_SEEDS=1,2`, `SPECKLE_ISOS=6400,12800`, `SPECKLE_RECIPE='{...}'`;
//! `SPECKLE_DUMP=DIR` writes a crop of each render beside the truth,
//! `SPECKLE_HASH=1` prints a hash of each render instead of the metrics
//! (with `SPECKLE_RAW=PREFIX` also the float pixels) to compare builds, and
//! `SPECKLE_TIME=1` times renders with and without the luminance noise
//! reduction's edge-preserving pass).

#![allow(clippy::excessive_precision, clippy::needless_range_loop)]

use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::image_processing::{
    get_all_adjustments_from_json, remove_raw_artifacts_and_enhance,
};
use rawtonic_gpu::gpu_processing::{
    GpuContext, GpuImageCache, GpuProcessorSlot, ReadbackShader, RenderOutputPrecision,
    RenderRequest, RenderSource, process_and_get_dynamic_image_inner,
};
use rawtonic_rawdecode::demosaic::{CfaPattern, demosaic};
use rayon::prelude::*;
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE: usize = 4320;
const CELL: usize = 270;

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

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    None,
    Near,
    Sat,
    Dark,
    Tex,
}

struct Scene {
    rgb: Vec<[f32; 3]>,
    zone: Vec<Zone>,
    edge: Vec<bool>,
}

/// Separable Gaussian blur of an RGB plane.
fn blur_rgb(plane: &mut [[f32; 3]], sigma: f32) {
    let r = (3.0 * sigma).ceil() as i32;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / sum).collect();
    let mut tmp = vec![[0.0f32; 3]; plane.len()];
    tmp.par_chunks_mut(SIZE).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut acc = [0.0f32; 3];
            for (j, w) in k.iter().enumerate() {
                let xx = (x as i32 + j as i32 - r).clamp(0, SIZE as i32 - 1) as usize;
                let v = plane[y * SIZE + xx];
                for c in 0..3 {
                    acc[c] += w * v[c];
                }
            }
            *out = acc;
        }
    });
    plane.par_chunks_mut(SIZE).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut acc = [0.0f32; 3];
            for (j, w) in k.iter().enumerate() {
                let yy = (y as i32 + j as i32 - r).clamp(0, SIZE as i32 - 1) as usize;
                let v = tmp[yy * SIZE + x];
                for c in 0..3 {
                    acc[c] += w * v[c];
                }
            }
            *out = acc;
        }
    });
}

fn blur_plane(plane: &[f32], sigma: f32) -> Vec<f32> {
    let mut rgb: Vec<[f32; 3]> = plane.iter().map(|&v| [v, 0.0, 0.0]).collect();
    blur_rgb(&mut rgb, sigma);
    rgb.iter().map(|v| v[0]).collect()
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn scene(seed: u64) -> Scene {
    let n = SIZE * SIZE;
    let mut rng = Rng(seed);
    // Dark background with a gentle gradient (0.004 … 0.03 linear).
    let mut background = vec![[0.0f32; 3]; n];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let t = 0.004 + 0.026 * (y as f32 / SIZE as f32) * (0.6 + 0.4 * x as f32 / SIZE as f32);
            background[y * SIZE + x] = [t * 1.05, t, t * 0.9];
        }
    }
    let mut lights = vec![[0.0f32; 3]; n];
    let mut patch = vec![None::<[f32; 3]>; n];
    let mut zone = vec![Zone::None; n];
    let cells = SIZE / CELL;
    let saturated: [[f32; 3]; 6] = [
        [0.75, 0.04, 0.03],
        [0.06, 0.55, 0.05],
        [0.04, 0.10, 0.80],
        [0.85, 0.35, 0.03],
        [0.60, 0.05, 0.55],
        [0.05, 0.50, 0.60],
    ];
    for cy in 0..cells {
        for cx in 0..cells {
            let kind = (cy * cells + cx + seed as usize) % 5;
            let ox = cx * CELL;
            let oy = cy * CELL;
            let jitter = |rng: &mut Rng| (rng.next() * 40.0) as usize;
            match kind {
                0 => {
                    // Bright disc, neutral or coloured, below sensor white.
                    let r = 8.0 + rng.next() * 30.0;
                    let level = 0.5 + rng.next() * 0.4;
                    let c = if rng.next() < 0.5 {
                        [level; 3]
                    } else {
                        saturated[(rng.next() * 6.0) as usize % 6].map(|v| v * level / 0.8)
                    };
                    let (px, py) = (
                        ox + 135 + jitter(&mut rng) - 20,
                        oy + 135 + jitter(&mut rng) - 20,
                    );
                    let ri = r.ceil() as i32 + 1;
                    for dy in -ri..=ri {
                        for dx in -ri..=ri {
                            let d = ((dx * dx + dy * dy) as f32).sqrt();
                            if d <= r {
                                let i =
                                    (py as i32 + dy) as usize * SIZE + (px as i32 + dx) as usize;
                                lights[i] = c;
                            }
                        }
                    }
                }
                1 => {
                    // Point lights.
                    for _ in 0..6 {
                        let px = ox + 40 + (rng.next() * 190.0) as usize;
                        let py = oy + 40 + (rng.next() * 190.0) as usize;
                        let peak = 2.0 + rng.next() * 6.0;
                        let tint = if rng.next() < 0.5 {
                            [1.0, 0.85, 0.6]
                        } else {
                            [1.0, 1.0, 1.0]
                        };
                        for dy in -1i32..=1 {
                            for dx in -1i32..=1 {
                                let w = if dx == 0 && dy == 0 { 1.0 } else { 0.35 };
                                let i =
                                    (py as i32 + dy) as usize * SIZE + (px as i32 + dx) as usize;
                                lights[i] = tint.map(|t| t * peak * w * 0.25);
                            }
                        }
                    }
                }
                2 => {
                    // Bright saturated patch with fine texture.
                    let c = saturated[(rng.next() * 6.0) as usize % 6];
                    let period = 5.0 + rng.next() * 6.0;
                    for y in oy + 60..oy + 210 {
                        for x in ox + 60..ox + 210 {
                            let t = 1.0
                                + 0.08
                                    * (std::f32::consts::TAU * x as f32 / period).sin()
                                    * (std::f32::consts::TAU * y as f32 / (period * 1.3)).sin();
                            patch[y * SIZE + x] = Some(c.map(|v| v * t));
                            if (oy + 70..oy + 200).contains(&y) && (ox + 70..ox + 200).contains(&x)
                            {
                                zone[y * SIZE + x] = Zone::Sat;
                            }
                        }
                    }
                }
                3 => {
                    // Low-contrast grey texture.
                    let period = 4.0 + rng.next() * 8.0;
                    let amp = 0.03 + rng.next() * 0.04;
                    for y in oy + 60..oy + 210 {
                        for x in ox + 60..ox + 210 {
                            let t = 1.0
                                + amp
                                    * (std::f32::consts::TAU * (x as f32 + 0.5 * y as f32)
                                        / period)
                                        .sin();
                            patch[y * SIZE + x] = Some([0.12 * t, 0.12 * t, 0.12 * t]);
                            if (oy + 70..oy + 200).contains(&y) && (ox + 70..ox + 200).contains(&x)
                            {
                                zone[y * SIZE + x] = Zone::Tex;
                            }
                        }
                    }
                }
                _ => {
                    for y in oy + 40..oy + 230 {
                        for x in ox + 40..ox + 230 {
                            zone[y * SIZE + x] = Zone::Dark;
                        }
                    }
                }
            }
        }
    }
    // Lens: a tight core and a faint wide glow around the lights.
    let mut glow = lights.clone();
    blur_rgb(&mut lights, 0.7);
    blur_rgb(&mut glow, 10.0);
    let mut rgb = vec![[0.0f32; 3]; n];
    for i in 0..n {
        rgb[i] = match patch[i] {
            Some(c) => c,
            None => std::array::from_fn(|c| background[i][c] + lights[i][c] + 0.02 * glow[i][c]),
        };
    }
    // Zones around the bright objects: dark pixels 3..24 px away.
    let bright: Vec<f32> = rgb
        .iter()
        .map(|c| if luma(*c) > 0.08 { 1.0 } else { 0.0 })
        .collect();
    let near_mask = blur_plane(&bright, 8.0);
    let core_mask = blur_plane(&bright, 1.2);
    let mut edge = vec![false; n];
    for i in 0..n {
        let x = i % SIZE;
        let y = i / SIZE;
        if x < 2 || y < 2 || x >= SIZE - 2 || y >= SIZE - 2 {
            continue;
        }
        let l = luma(rgb[i]);
        if patch[i].is_none() && l < 0.05 && near_mask[i] > 0.002 && core_mask[i] < 1e-3 {
            zone[i] = Zone::Near;
        }
        if core_mask[i] > 0.2 && core_mask[i] < 0.8 {
            edge[i] = true;
        }
    }
    Scene { rgb, zone, edge }
}

// ---------- camera ----------

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

/// Develops the scene; `iso` 0 = no noise.
fn develop(scene: &Scene, iso: f32, seed: u64) -> Rgb32FImage {
    let to_camera = invert(CAMERA);
    let pattern = CfaPattern::new(2, 2, vec![0, 1, 1, 2]).unwrap();
    let full = 40_000.0 * 100.0 / iso.max(1.0);
    let read = 3.0;
    let mut mosaic = vec![0.0f32; SIZE * SIZE];
    mosaic
        .par_chunks_mut(SIZE)
        .enumerate()
        .for_each(|(y, row)| {
            let mut rng = Rng(seed.wrapping_mul(1_000_003).wrapping_add(y as u64 * 7919));
            for (x, out) in row.iter_mut().enumerate() {
                let c = pattern.color(y as isize, x as isize);
                let cam = mul(&to_camera, scene.rgb[y * SIZE + x]).map(|v| v.max(0.0));
                // Sensor white clips the raw value.
                let raw = (cam[c] / WB[c]).min(1.0);
                let value = if iso > 0.0 {
                    let electrons = raw * full;
                    let sigma = (electrons + read * read).sqrt();
                    ((electrons + sigma * rng.gaussian()) / full).min(1.0)
                } else {
                    raw
                };
                *out = value * WB[c];
            }
        });
    let rgb = demosaic(&mosaic, SIZE, SIZE, &pattern);
    let data: Vec<f32> = rgb.iter().flat_map(|p| mul(&CAMERA, *p)).collect();
    let mut image =
        DynamicImage::ImageRgb32F(Rgb32FImage::from_raw(SIZE as u32, SIZE as u32, data).unwrap());
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

    fn render(&self, image: &Rgb32FImage, recipe: &serde_json::Value, noise: f32) -> Vec<[f32; 3]> {
        let mut adjustments = get_all_adjustments_from_json(recipe, true, None);
        adjustments.uniforms.global.noise_level = noise;
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
            "speckle_harness",
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

// ---------- metrics ----------

fn median5(plane: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; plane.len()];
    out.par_chunks_mut(SIZE).enumerate().for_each(|(y, row)| {
        let mut v = [0.0f32; 25];
        for (x, o) in row.iter_mut().enumerate() {
            let mut k = 0;
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let yy = (y as i32 + dy).clamp(0, SIZE as i32 - 1) as usize;
                    let xx = (x as i32 + dx).clamp(0, SIZE as i32 - 1) as usize;
                    v[k] = plane[yy * SIZE + xx];
                    k += 1;
                }
            }
            v.sort_unstable_by(f32::total_cmp);
            *o = v[12];
        }
    });
    out
}

#[derive(Default, Clone, Copy)]
struct ZoneStats {
    noise: f64,
    speck: f64,
    dark: f64,
    colour: f64,
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    zones: [ZoneStats; 4],
    edge: f64,
    texture: f64,
    sat_texture: f64,
}

impl Metrics {
    fn add(&mut self, o: &Metrics, k: f64) {
        for z in 0..4 {
            self.zones[z].noise += k * o.zones[z].noise;
            self.zones[z].speck += k * o.zones[z].speck;
            self.zones[z].dark += k * o.zones[z].dark;
            self.zones[z].colour += k * o.zones[z].colour;
        }
        self.edge += k * o.edge;
        self.texture += k * o.texture;
        self.sat_texture += k * o.sat_texture;
    }
}

const ZONES: [(Zone, &str); 4] = [
    (Zone::Near, "near"),
    (Zone::Sat, "sat"),
    (Zone::Dark, "dark"),
    (Zone::Tex, "tex"),
];

fn metrics(scene: &Scene, out: &[[f32; 3]], truth: &[[f32; 3]]) -> Metrics {
    let n = SIZE * SIZE;
    let o: Vec<[f32; 3]> = out.par_iter().map(|p| oklab(*p)).collect();
    let t: Vec<[f32; 3]> = truth.par_iter().map(|p| oklab(*p)).collect();
    let e: Vec<f32> = (0..n).map(|i| o[i][0] - t[i][0]).collect();
    let ea: Vec<f32> = (0..n).map(|i| o[i][1] - t[i][1]).collect();
    let eb: Vec<f32> = (0..n).map(|i| o[i][2] - t[i][2]).collect();
    let e_low = blur_plane(&e, 8.0);
    let (me, ma, mb) = (median5(&e), median5(&ea), median5(&eb));
    let mut m = Metrics::default();
    for (z, (zone, _)) in ZONES.iter().enumerate() {
        let (mut sq, mut sp, mut dk, mut co, mut cnt) = (0.0f64, 0usize, 0usize, 0usize, 0usize);
        for i in 0..n {
            if scene.zone[i] != *zone {
                continue;
            }
            let f = f64::from(e[i] - e_low[i]);
            sq += f * f;
            let r = e[i] - me[i];
            if r.abs() > 8.0 {
                sp += 1;
            }
            if r < -8.0 {
                dk += 1;
            }
            if ((ea[i] - ma[i]).powi(2) + (eb[i] - mb[i]).powi(2)).sqrt() > 8.0 {
                co += 1;
            }
            cnt += 1;
        }
        let c = cnt.max(1) as f64;
        m.zones[z] = ZoneStats {
            noise: (sq / c).sqrt(),
            speck: 1000.0 * sp as f64 / c,
            dark: 1000.0 * dk as f64 / c,
            colour: 1000.0 * co as f64 / c,
        };
    }
    // Edge sharpness of the bright objects.
    let grad = |p: &[[f32; 3]], i: usize| {
        let gx = p[i + 1][0] - p[i - 1][0];
        let gy = p[i + SIZE][0] - p[i - SIZE][0];
        f64::from((gx * gx + gy * gy).sqrt())
    };
    let (mut go, mut gt) = (0.0f64, 0.0f64);
    for i in 0..n {
        if scene.edge[i] {
            go += grad(&o, i);
            gt += grad(&t, i);
        }
    }
    m.edge = go / gt.max(1e-9);
    // Texture kept: slope of the fine band on the truth's.
    let lo: Vec<f32> = o.iter().map(|v| v[0]).collect();
    let lt: Vec<f32> = t.iter().map(|v| v[0]).collect();
    let (bo, bt) = (blur_plane(&lo, 8.0), blur_plane(&lt, 8.0));
    for (zone, slot) in [(Zone::Tex, 0), (Zone::Sat, 1)] {
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for i in 0..n {
            if scene.zone[i] == zone {
                let tv = f64::from(lt[i] - bt[i]);
                let ov = f64::from(lo[i] - bo[i]);
                num += tv * ov;
                den += tv * tv;
            }
        }
        let slope = num / den.max(1e-12);
        if slot == 0 {
            m.texture = slope;
        } else {
            m.sat_texture = slope;
        }
    }
    m
}

fn print(name: &str, m: &Metrics) {
    let mut line = format!("{name:<22}");
    for (z, (_, label)) in ZONES.iter().enumerate() {
        let s = m.zones[z];
        line += &format!(
            " {label}: n {:5.2} sp {:6.2} dk {:6.2} c {:5.2} |",
            s.noise, s.speck, s.dark, s.colour
        );
    }
    line += &format!(
        " edge {:.3} tex {:.3} satTex {:.3}",
        m.edge, m.texture, m.sat_texture
    );
    println!("{line}");
}

/// The top-left 1080 px of the render beside the truth (8-bit sRGB).
fn dump(path: &str, out: &[[f32; 3]], truth: &[[f32; 3]]) {
    let w = 1080usize;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    };
    let img = image::RgbImage::from_fn(2 * w as u32, w as u32, |x, y| {
        let (x, y) = (x as usize, y as usize);
        let p = if x < w {
            out[y * SIZE + x]
        } else {
            truth[y * SIZE + x - w]
        };
        image::Rgb(p.map(enc))
    });
    img.save(path).expect("dump");
}

fn main() {
    let list = |key: &str, default: &str| -> Vec<f32> {
        std::env::var(key)
            .unwrap_or_else(|_| default.to_string())
            .split(',')
            .map(|s| s.trim().parse().unwrap())
            .collect()
    };
    let seeds = list("SPECKLE_SEEDS", "1,2");
    let isos = list("SPECKLE_ISOS", "3200,6400,12800,25600");
    let recipe: serde_json::Value = std::env::var("SPECKLE_RECIPE")
        .ok()
        .map(|s| serde_json::from_str(&s).expect("SPECKLE_RECIPE"))
        .unwrap_or_else(|| json!({}));
    let renderer = Renderer::new();
    let mut total = Metrics::default();
    let mut count = 0.0;
    for &seed in &seeds {
        let sc = scene(seed as u64);
        let clean = develop(&sc, 0.0, 0);
        let truth = renderer.render(
            &clean,
            &json!({"sharpness": 0, "lumaNoiseReduction": 0, "colorNoiseReduction": 0}),
            0.0,
        );
        for &iso in &isos {
            let noisy = develop(&sc, iso, seed as u64 * 1000 + iso as u64);
            let level =
                rawtonic_rawdecode::noise_level::measure(&DynamicImage::ImageRgb32F(noisy.clone()))
                    .unwrap_or(0.0);
            if std::env::var("SPECKLE_TIME").is_ok() {
                // Render time with the edge-preserving pass (measured level)
                // and without it (a level just below its start); medians of 5.
                for (label, noise) in [("with", level), ("without", 0.0049)] {
                    let _ = renderer.render(&noisy, &recipe, noise);
                    let mut times: Vec<f64> = (0..5)
                        .map(|_| {
                            let start = std::time::Instant::now();
                            let _ = renderer.render(&noisy, &recipe, noise);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    times.sort_by(f64::total_cmp);
                    println!(
                        "ISO {iso} {label} edge pass: {:.3} s ({}x{})",
                        times[2], SIZE, SIZE
                    );
                }
                continue;
            }
            let out = renderer.render(&noisy, &recipe, level);
            if std::env::var("SPECKLE_HASH").is_ok() {
                let mut h = 0xcbf29ce484222325u64;
                for v in out.iter().flatten() {
                    h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x100000001b3);
                }
                println!("seed {seed} ISO {iso} ({level:.4}) hash {h:016x}");
                if let Ok(path) = std::env::var("SPECKLE_RAW") {
                    let bytes: Vec<u8> =
                        out.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
                    std::fs::write(format!("{path}_{iso}.f32"), bytes).unwrap();
                }
                continue;
            }
            if let Ok(dir) = std::env::var("SPECKLE_DUMP") {
                dump(&format!("{dir}/seed{seed}_iso{iso}.png"), &out, &truth);
            }
            let m = metrics(&sc, &out, &truth);
            print(&format!("seed {seed} ISO {iso} ({level:.4})"), &m);
            total.add(&m, 1.0);
            count += 1.0;
        }
    }
    let mut mean = Metrics::default();
    mean.add(&total, 1.0 / count);
    print("mean", &mean);
}
