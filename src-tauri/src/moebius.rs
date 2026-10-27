//! Moebius (hustvl, Apache-2.0), the guide of Remove's fill for large holes
//! on macOS (docs/MODELS.md, "Object removal"): a 0.23 B distilled latent
//! diffusion inpainter at 512 × 512 with the SDXL VAE (fp16-fix, MIT), run
//! as three Core ML programs on the GPU
//! (scripts/models/convert_moebius_coreml.py). MI-GAN paints objects back
//! into holes that fill most of its view (a lemon's brown smear, a kitten's
//! ghost); Moebius fills them with background.
//!
//! The pipeline is the authors' (DDIM, eta 0, 10 steps from strength 0.99,
//! so 9 network runs; classifier-free guidance 2.0; their noise offset
//! 0.0357) with the VAE posterior's mean instead of a sample and a fixed
//! seed, so a removal fills the same way every time. It runs on a square
//! crop of 2.5 × the hole's longer side, scaled to 512 px; the result is
//! scaled back and guides the patch-based completion as MI-GAN's does
//! (`inpainting::fill_removal`).

// Only the macOS backend uses the sampler so far (`Networks`).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::Path;

use anyhow::{Context, Result, ensure};
use image::{GrayImage, RgbImage, RgbaImage};
use rayon::prelude::*;

#[cfg(target_os = "macos")]
pub mod coreml;

/// The networks' working size.
pub const SIZE: usize = 512;
pub(crate) const LATENT: usize = SIZE / 8;
/// The crop around the hole, as a multiple of its longer side (as MI-GAN's
/// context marks, `ai_processing::run_inpainting`).
pub const CONTEXT: f32 = 2.5;
const STEPS: usize = 10;
const STRENGTH: f32 = 0.99;
const GUIDANCE: f32 = 2.0;
const NOISE_OFFSET: f32 = 0.0357;
const SEED: u64 = 0;
/// The 20 learned embeddings that condition the network, in its batch
/// order (unconditional, then conditional), 10 × 3072 each.
pub(crate) const EMBEDDINGS_LEN: usize = 2 * 10 * 3072;

pub const EMBEDDINGS_FILE: (&str, &str) = (
    "moebius_embeddings.bin",
    "1b46b6251205fd7473f9edb834073848df00fe83e3224ee1f4a2f1bc4bb52c8e",
);
/// The 20 learned embeddings (`EMBEDDINGS_FILE`), float32 little-endian.
pub fn read_embeddings(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).context("Moebius embeddings")?;
    ensure!(
        bytes.len() == EMBEDDINGS_LEN * 4,
        "{} has {} bytes, expected {}",
        path.display(),
        bytes.len(),
        EMBEDDINGS_LEN * 4
    );
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect())
}

/// The networks the sampler runs: Core ML on macOS (`coreml`), a stand-in
/// in tests. This is the hook for other platforms: an implementation on
/// `rawtonic_onnx::Model` (an ONNX export of the same three networks with
/// the embeddings as an input, on the WebGPU or CUDA provider) would let
/// Remove use Moebius on Windows and Linux; until one is measured there
/// (docs/MODELS.md, "Moebius on other platforms"), they keep MI-GAN.
pub trait Networks {
    /// Photos [n, 3, 512, 512] in -1..1 → latents [n, 4, 64, 64].
    fn encode(&self, images: &[f32], n: usize) -> Result<Vec<f32>>;
    /// The CFG batch [2, 9, 64, 64] at timestep `t` → noise [2, 4, 64, 64].
    fn noise(&self, latent_in: &[f32], t: f32) -> Result<Vec<f32>>;
    /// A latent [1, 4, 64, 64] → a photo [1, 3, 512, 512] in -1..1.
    fn decode(&self, latent: &[f32]) -> Result<Vec<f32>>;
}

/// DDPM's scaled-linear schedule (betas 0.00085 .. 0.012 over 1000 steps),
/// cumulated: ᾱ_t.
fn alphas_cumprod() -> Vec<f64> {
    let (a, b) = (0.00085f64.sqrt(), 0.012f64.sqrt());
    let mut product = 1.0;
    (0..1000)
        .map(|i| {
            let beta = (a + (b - a) * i as f64 / 999.0).powi(2);
            product *= 1.0 - beta;
            product
        })
        .collect()
}

/// DDIM's "leading" timesteps for `steps` steps, from `strength`
/// (diffusers' `get_timesteps`): 10 steps at 0.99 → 800, 700, …, 0.
fn timesteps(steps: usize, strength: f32) -> Vec<usize> {
    let ratio = 1000 / steps;
    let run = ((steps as f32 * strength) as usize).min(steps);
    (0..steps)
        .rev()
        .map(|i| i * ratio)
        .skip(steps - run)
        .collect()
}

/// Standard normal samples from a fixed seed (SplitMix64, Box–Muller), the
/// same on every machine.
struct Normal(u64);

impl Normal {
    fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn sample(&mut self) -> f32 {
        let (u, v) = (self.uniform(), self.uniform());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }
}

/// The starting noise [1, 4, 64, 64] with the authors' per-channel offset.
pub(crate) fn start_noise() -> Vec<f32> {
    let mut normal = Normal(SEED);
    let mut noise: Vec<f32> = (0..4 * LATENT * LATENT).map(|_| normal.sample()).collect();
    for c in 0..4 {
        let offset = NOISE_OFFSET * normal.sample();
        for v in &mut noise[c * LATENT * LATENT..(c + 1) * LATENT * LATENT] {
            *v += offset;
        }
    }
    noise
}

/// Fills `hole` (512 × 512, true = fill) of `image` (512 × 512 RGB, 0..1,
/// first-major rows) and returns the network's whole 512 px result.
/// `progress(done, total)` follows the network runs; `None` when
/// `cancelled` turned true between them.
pub fn fill<N: Networks>(
    nets: &N,
    image: &[[f32; 3]],
    hole: &[bool],
    noise: &[f32],
    progress: &mut dyn FnMut(usize, usize),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Vec<[f32; 3]>>> {
    let n = SIZE * SIZE;
    ensure!(
        image.len() == n && hole.len() == n,
        "a 512 × 512 photo and hole"
    );
    let plane = LATENT * LATENT;
    // The photo and the photo with its hole blanked, planar, in -1..1.
    let mut photos = vec![0.0f32; 2 * 3 * n];
    for (i, (px, &h)) in image.iter().zip(hole).enumerate() {
        for c in 0..3 {
            let v = px[c] * 2.0 - 1.0;
            photos[c * n + i] = v;
            photos[3 * n + c * n + i] = if h { 0.0 } else { v };
        }
    }
    let ts = timesteps(STEPS, STRENGTH);
    let total = ts.len();
    progress(0, total);
    let latents = nets.encode(&photos, 2)?;
    let (latent, masked) = latents.split_at(4 * plane);
    // The hole at 64 × 64: torch's nearest-neighbour scaling takes pixel 8i.
    let mask64: Vec<f32> = (0..plane)
        .map(|i| {
            let (y, x) = (i / LATENT, i % LATENT);
            if hole[y * 8 * SIZE + x * 8] { 1.0 } else { 0.0 }
        })
        .collect();
    let acp = alphas_cumprod();
    let ratio = 1000 / STEPS;
    let a0 = acp[ts[0]];
    let mut z: Vec<f32> = latent
        .iter()
        .zip(noise)
        .map(|(&l, &e)| (a0.sqrt() * l as f64 + (1.0 - a0).sqrt() * e as f64) as f32)
        .collect();
    let mut batch = vec![0.0f32; 2 * 9 * plane];
    for (k, &t) in ts.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        for b in 0..2 {
            let item = &mut batch[b * 9 * plane..(b + 1) * 9 * plane];
            item[..4 * plane].copy_from_slice(&z);
            item[4 * plane..5 * plane].copy_from_slice(&mask64);
            item[5 * plane..].copy_from_slice(masked);
        }
        let eps2 = nets.noise(&batch, t as f32)?;
        ensure!(eps2.len() == 8 * plane, "noise of {} values", eps2.len());
        let (a, prev) = (acp[t], if t >= ratio { acp[t - ratio] } else { 1.0 });
        for (i, zi) in z.iter_mut().enumerate() {
            let (uncond, cond) = (eps2[i] as f64, eps2[4 * plane + i] as f64);
            let eps = uncond + GUIDANCE as f64 * (cond - uncond);
            let x0 = (*zi as f64 - (1.0 - a).sqrt() * eps) / a.sqrt();
            *zi = (prev.sqrt() * x0 + (1.0 - prev).sqrt() * eps) as f32;
        }
        progress(k + 1, total);
    }
    if cancelled() {
        return Ok(None);
    }
    let out = nets.decode(&z)?;
    Ok(Some(
        (0..n)
            .map(|i| std::array::from_fn(|c| ((out[c * n + i] + 1.0) / 2.0).clamp(0.0, 1.0)))
            .collect(),
    ))
}

/// The square crop the network sees: `CONTEXT` × the hole's longer side
/// (`bounds`: x0, y0, x1, y1 inclusive) around its centre, moved inside the
/// photo, at most the photo's shorter side. `None` when the hole does not
/// fit in such a square (a hole longer than the photo's shorter side).
pub fn crop_square(
    bounds: (u32, u32, u32, u32),
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32)> {
    let (x0, y0, x1, y1) = bounds;
    let (hw, hh) = (x1 - x0 + 1, y1 - y0 + 1);
    let side = ((hw.max(hh) as f32 * CONTEXT).round() as u32)
        .max(SIZE as u32 / 2)
        .min(width.min(height));
    if hw > side || hh > side {
        return None;
    }
    let place = |lo: u32, hi: u32, size: u32| {
        let centre = (lo + hi).div_ceil(2);
        centre.saturating_sub(side / 2).min(size - side)
    };
    Some((place(x0, x1, width), place(y0, y1, height), side))
}

/// Area-averages a `side` × `side` region into 512 × 512 (each output pixel
/// the mean of the source pixels it covers, partly covered ones weighted).
fn area_resample(side: usize, sample: impl Fn(usize, usize) -> [f32; 4] + Sync) -> Vec<[f32; 4]> {
    let scale = side as f64 / SIZE as f64;
    let spans: Vec<Vec<(usize, f32)>> = (0..SIZE)
        .map(|o| {
            let (lo, hi) = (o as f64 * scale, (o + 1) as f64 * scale);
            (lo.floor() as usize..(hi.ceil() as usize).min(side))
                .map(|s| {
                    let w = (hi.min(s as f64 + 1.0) - lo.max(s as f64)) / scale;
                    (s, w as f32)
                })
                .filter(|&(_, w)| w > 0.0)
                .collect()
        })
        .collect();
    let mut out = vec![[0.0f32; 4]; SIZE * SIZE];
    out.par_chunks_mut(SIZE).enumerate().for_each(|(oy, row)| {
        for (ox, xs) in spans.iter().enumerate() {
            let mut acc = [0.0f32; 4];
            for &(sy, wy) in &spans[oy] {
                for &(sx, wx) in xs {
                    let v = sample(sx, sy);
                    for c in 0..4 {
                        acc[c] += v[c] * wy * wx;
                    }
                }
            }
            row[ox] = acc;
        }
    });
    out
}

/// Catmull-Rom weights of the four taps around `t` in 0..1.
fn cubic_weights(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

/// The 512 px fill scaled to `side` px (Catmull-Rom, pixel centres
/// aligned) at pixel (`x`, `y`), in 0..1.
fn upscaled(fill: &[[f32; 3]], side: u32, x: u32, y: u32) -> [f32; 3] {
    let scale = SIZE as f32 / side as f32;
    let tap = |v: u32| {
        let u = (v as f32 + 0.5) * scale - 0.5;
        let base = u.floor();
        (base as i64, cubic_weights(u - base))
    };
    let ((bx, wx), (by, wy)) = (tap(x), tap(y));
    let at = |i: i64| i.clamp(0, SIZE as i64 - 1) as usize;
    let mut acc = [0.0f32; 3];
    for (j, wyj) in wy.iter().enumerate() {
        let row = at(by - 1 + j as i64) * SIZE;
        for (i, wxi) in wx.iter().enumerate() {
            let p = fill[row + at(bx - 1 + i as i64)];
            for c in 0..3 {
                acc[c] += p[c] * wyj * wxi;
            }
        }
    }
    acc.map(|v| v.clamp(0.0, 1.0))
}

/// The photo with Moebius's fill in the hole (`mask` > 0, blended by its
/// level as MI-GAN's result is), the guide of Remove's completion. `None`
/// when cancelled or when the hole does not fit the network's square crop
/// (the caller uses MI-GAN then).
pub fn guide<N: Networks>(
    nets: &N,
    source: &RgbImage,
    mask: &GrayImage,
    bounds: (u32, u32, u32, u32),
    progress: &mut dyn FnMut(usize, usize),
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<RgbaImage>> {
    let (w, h) = source.dimensions();
    let Some((cx, cy, side)) = crop_square(bounds, w, h) else {
        return Ok(None);
    };
    let small = area_resample(side as usize, |x, y| {
        let p = source.get_pixel(cx + x as u32, cy + y as u32).0;
        let hole = mask.get_pixel(cx + x as u32, cy + y as u32)[0] > 0;
        [
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
            if hole { 1.0 } else { 0.0 },
        ]
    });
    let image: Vec<[f32; 3]> = small.iter().map(|p| [p[0], p[1], p[2]]).collect();
    // A pixel any part of the hole covers is in the network's hole, so no
    // remnant of the object is left in its view.
    let hole: Vec<bool> = small.iter().map(|p| p[3] > 0.0).collect();
    let Some(filled) = fill(nets, &image, &hole, &start_noise(), progress, cancelled)? else {
        return Ok(None);
    };
    // The fill rounded to 8 bits as the network's image, then scaled back
    // only where the hole is.
    let filled: Vec<[f32; 3]> = filled
        .iter()
        .map(|p| p.map(|v| (v * 255.0).round() / 255.0))
        .collect();
    let mut out = vec![0u8; w as usize * h as usize * 4];
    out.par_chunks_mut(w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = x as u32;
                let o = source.get_pixel(x, y).0;
                let m = mask.get_pixel(x, y)[0];
                let inside = (cx..cx + side).contains(&x) && (cy..cy + side).contains(&y);
                if m > 0 && inside {
                    let alpha = m as f32 / 255.0;
                    let f = upscaled(&filled, side, x - cx, y - cy);
                    for c in 0..3 {
                        px[c] = (f[c] * 255.0 * alpha + o[c] as f32 * (1.0 - alpha)).round() as u8;
                    }
                } else {
                    px[..3].copy_from_slice(&o);
                }
                px[3] = 255;
            }
        });
    Ok(Some(
        RgbaImage::from_raw(w, h, out).expect("a buffer of the photo's size"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A network that predicts zero noise, an encoder that averages the
    /// photo into the first latent channel and a decoder that spreads it
    /// back: the sampler must then hand the (noised and denoised) photo
    /// through unchanged where the schedule says so.
    struct Echo;

    impl Networks for Echo {
        fn encode(&self, images: &[f32], n: usize) -> Result<Vec<f32>> {
            let plane = SIZE * SIZE;
            let mut out = vec![0.0; n * 4 * LATENT * LATENT];
            for b in 0..n {
                for i in 0..LATENT * LATENT {
                    let (y, x) = (i / LATENT, i % LATENT);
                    out[b * 4 * LATENT * LATENT + i] = images[b * 3 * plane + y * 8 * SIZE + x * 8];
                }
            }
            Ok(out)
        }

        fn noise(&self, latent_in: &[f32], _t: f32) -> Result<Vec<f32>> {
            assert_eq!(latent_in.len(), 2 * 9 * LATENT * LATENT);
            Ok(vec![0.0; 2 * 4 * LATENT * LATENT])
        }

        fn decode(&self, latent: &[f32]) -> Result<Vec<f32>> {
            let plane = SIZE * SIZE;
            let mut out = vec![0.0; 3 * plane];
            for i in 0..plane {
                let (y, x) = (i / SIZE, i % SIZE);
                let v = latent[(y / 8) * LATENT + x / 8];
                for c in 0..3 {
                    out[c * plane + i] = v;
                }
            }
            Ok(out)
        }
    }

    #[test]
    fn schedule_and_timesteps_are_the_authors() {
        let acp = alphas_cumprod();
        // diffusers' DDIMScheduler(scaled_linear, 0.00085, 0.012).
        assert!((acp[0] - 0.99915).abs() < 1e-9);
        assert!((acp[999] - 0.004_660_1).abs() < 1e-6);
        assert_eq!(
            timesteps(10, 0.99),
            [800, 700, 600, 500, 400, 300, 200, 100, 0]
        );
        assert_eq!(
            timesteps(20, 0.99),
            (0..19).rev().map(|i| i * 50).collect::<Vec<_>>()
        );
    }

    #[test]
    fn noise_is_standard_normal_and_fixed() {
        let a = start_noise();
        assert_eq!(a, start_noise());
        let n = a.len() as f64;
        let mean = a.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var = a.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n;
        assert!(
            mean.abs() < 0.05 && (var - 1.0).abs() < 0.05,
            "mean {mean}, variance {var}"
        );
        // The evaluation's copy (convert_moebius_coreml.py `start_noise`)
        // draws the same numbers.
        for (got, want) in [
            (a[0], -0.453_800_17),
            (a[1], 2.649_563_6),
            (a[2], -0.989_646_55),
            (a[a.len() - 1], 0.266_221_2),
        ] {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
    }

    #[test]
    fn sampler_with_zero_noise_denoises_to_the_photo() {
        // With ε = 0 every DDIM step predicts x0 = z / √ᾱ_t, so the noised
        // start √ᾱ·x0 + √(1-ᾱ)·noise ends at x0 + √((1-ᾱ)/ᾱ)·noise; with
        // zero noise exactly the photo's latent comes back.
        let image: Vec<[f32; 3]> = (0..SIZE * SIZE)
            .map(|i| [(i % 7) as f32 / 7.0; 3])
            .collect();
        let hole = vec![false; SIZE * SIZE];
        let mut calls = Vec::new();
        let out = fill(
            &Echo,
            &image,
            &hole,
            &vec![0.0; 4 * LATENT * LATENT],
            &mut |d, t| calls.push((d, t)),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls.first(), Some(&(0, 9)));
        assert_eq!(calls.last(), Some(&(9, 9)));
        for (i, px) in out.iter().enumerate() {
            let (y, x) = (i / SIZE, i % SIZE);
            let expect = image[(y / 8 * 8) * SIZE + x / 8 * 8][0];
            assert!((px[0] - expect).abs() < 1e-4, "{i}: {} vs {expect}", px[0]);
        }
    }

    #[test]
    fn cancelling_stops_between_network_runs() {
        let image = vec![[0.5f32; 3]; SIZE * SIZE];
        let hole = vec![true; SIZE * SIZE];
        let runs = std::cell::Cell::new(0);
        let out = fill(
            &Echo,
            &image,
            &hole,
            &start_noise(),
            &mut |d, _| runs.set(d),
            &|| runs.get() >= 3,
        )
        .unwrap();
        assert!(out.is_none());
        assert_eq!(runs.get(), 3);
    }

    #[test]
    fn crop_square_is_context_times_the_hole_inside_the_photo() {
        // A 400 px hole in the middle of a 6000 × 4000 photo: a 1000 px square around it.
        assert_eq!(
            crop_square((2800, 1800, 3199, 2199), 6000, 4000),
            Some((2500, 1500, 1000))
        );
        // Near the corner: moved inside.
        assert_eq!(crop_square((0, 0, 399, 99), 6000, 4000), Some((0, 0, 1000)));
        // A small hole: at least 256 px.
        assert_eq!(
            crop_square((100, 100, 109, 109), 6000, 4000),
            Some((0, 0, 256))
        );
        // Longer than 1 / 2.5 of the shorter side: the whole shorter side.
        assert_eq!(
            crop_square((1000, 0, 2999, 1999), 6000, 4000),
            Some((0, 0, 4000))
        );
        // Longer than the shorter side: does not fit.
        assert_eq!(crop_square((0, 100, 5999, 199), 6000, 4000), None);
    }

    #[test]
    fn upscaling_keeps_a_ramp_and_a_constant() {
        // A horizontal ramp stays a ramp at 2 × (Catmull-Rom is exact on lines).
        let ramp: Vec<[f32; 3]> = (0..SIZE * SIZE)
            .map(|i| [(i % SIZE) as f32 / 1024.0 + 0.25; 3])
            .collect();
        let side = 2 * SIZE as u32;
        for x in [10u32, 11, 500, 501] {
            let want = ((x as f32 + 0.5) / 2.0 - 0.5) / 1024.0 + 0.25;
            assert!((upscaled(&ramp, side, x, 300)[0] - want).abs() < 1e-5);
        }
        let flat = vec![[0.3f32; 3]; SIZE * SIZE];
        assert!((upscaled(&flat, 777, 0, 776)[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn area_resample_averages_and_keeps_partial_cover() {
        // 1024 → 512: each output pixel is the mean of a 2 × 2 block.
        let out = area_resample(1024, |x, y| {
            [
                (x % 2) as f32,
                (y % 2) as f32,
                1.0,
                if x == 7 && y == 0 { 1.0 } else { 0.0 },
            ]
        });
        assert!(out.iter().all(|p| (p[0] - 0.5).abs() < 1e-6
            && (p[1] - 0.5).abs() < 1e-6
            && (p[2] - 1.0).abs() < 1e-6));
        assert!(out[3][3] > 0.0 && out[2][3] == 0.0 && out[4][3] == 0.0);
        // 700 → 512: weights of each output pixel sum to one.
        let out = area_resample(700, |_, _| [1.0; 4]);
        assert!(out.iter().all(|p| (p[0] - 1.0).abs() < 1e-5));
    }
}
