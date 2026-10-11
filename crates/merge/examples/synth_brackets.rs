//! Test brackets with a known answer: one RAW developed to scene-linear RGB
//! and written as linear DNGs at other exposures, each clipped at the sensor
//! white in camera units with shot and read noise, optionally shaken (a
//! small rotation and shift) and with a patch that moves between frames.
//! The scene itself (`truth.dng`, the unclipped noiseless frame) is the
//! answer a merge should approach.
//!
//! cargo run --release -p rawtonic-merge --example synth_brackets -- RAW OUTDIR EV[,EV…] [--shake] [--ghost]
use anyhow::{Context, Result};
use image::Rgb32FImage;
use rawtonic_merge::dng::{DngMeta, encode_linear_dng};
use rawtonic_merge::frame::load_frame;
use rawtonic_merge::hdr::working_to_camera;
use std::path::PathBuf;

/// Deterministic Gaussian noise (xorshift + Box-Muller).
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f32 {
        let (u, v) = (self.next().max(1e-12), self.next());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 4,
        "usage: RAW OUTDIR EV[,EV…] [--shake] [--ghost]"
    );
    let raw = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let evs: Vec<f32> = args[3]
        .split(',')
        .map(|s| s.parse())
        .collect::<Result<_, _>>()?;
    let shake = args.iter().any(|a| a == "--shake");
    // Whole-pixel shifts only: no resampling in the test frames.
    let shift = args.iter().any(|a| a == "--shift");
    let ghost = args.iter().any(|a| a == "--ghost");
    std::fs::create_dir_all(&out)?;

    let frame = load_frame(&raw, &Default::default())?;
    let to_camera = working_to_camera(frame.color.as_ref());
    let from_camera = to_camera.try_inverse().context("singular colour matrix")?;
    let (w, h) = frame.image.dimensions();
    let base_time = frame
        .metadata
        .as_ref()
        .and_then(|m| m.exif.exposure_time)
        .map(|r| r.n as f64 / r.d as f64)
        .unwrap_or(1.0 / 125.0);

    let write = |name: &str, image: &Rgb32FImage, time: f64| -> Result<()> {
        let mut metadata = frame.metadata.clone();
        if let Some(m) = metadata.as_mut() {
            let d = 1_000_000u32;
            m.exif.exposure_time = Some(rawler::formats::tiff::Rational::new(
                (time * d as f64).round() as u32,
                d,
            ));
        }
        let bytes = encode_linear_dng(
            image,
            &DngMeta {
                color: frame.color.as_ref(),
                metadata: metadata.as_ref(),
                keep_lens: true,
                baseline_exposure: Some(
                    rawtonic_engine::baseline_exposure::dng_baseline_equivalent(
                        &raw.to_string_lossy(),
                    ) as f64,
                ),
                preview: None,
                software: "Rawtonic test brackets",
            },
        )?;
        std::fs::write(out.join(name), bytes)?;
        Ok(())
    };
    write("truth.dng", &frame.image, base_time)?;

    let src: &[f32] = &frame.image;
    // The merge's reference: the middle exposure; it keeps the scene's
    // geometry and has no moving patch, so the truth is its answer.
    let mut sorted = evs.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted[(sorted.len() - 1) / 2];
    for (k, &ev) in evs.iter().enumerate() {
        let off = ev - mid;
        let gain = 2f32.powf(ev);
        // Shot noise: variance a·v (camera units), read noise b.
        let (a, b) = (1.0 / 6000.0f32, 0.0006f32);
        let mut noise = Noise(0x9E37_79B9_7F4A_7C15 ^ ((k as u64 + 1) * 0x1234_5678));
        // Shake: rotation (radians) and shift (pixels) about the centre.
        let (angle, dx, dy) = if shift && off != 0.0 {
            (0.0, 6.0 * off as f64, -4.0 * off as f64)
        } else if shake && off != 0.0 {
            (0.002 * off as f64, 6.0 * off as f64, -4.0 * off as f64)
        } else {
            (0.0, 0.0, 0.0)
        };
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        // Ghost: a square that moves right by 8 % of the width per frame.
        let gsize = (w.min(h) / 8) as i64;
        let gx0 = (w as i64 / 4) + (k as i64) * (w as i64 / 6);
        let gy0 = h as i64 / 2;
        let mut img = Rgb32FImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (xf, yf) = (x as f64 - cx, y as f64 - cy);
                let sx =
                    (angle.cos() * xf - angle.sin() * yf + cx + dx).clamp(0.0, w as f64 - 1.001);
                let sy =
                    (angle.sin() * xf + angle.cos() * yf + cy + dy).clamp(0.0, h as f64 - 1.001);
                let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
                let (fx, fy) = ((sx - x0 as f64) as f32, (sy - y0 as f64) as f32);
                let at = |xx: usize, yy: usize| &src[(yy * w as usize + xx) * 3..][..3];
                let mut p = [0.0f32; 3];
                for (c, pc) in p.iter_mut().enumerate() {
                    *pc = (at(x0, y0)[c] * (1.0 - fx) + at(x0 + 1, y0)[c] * fx) * (1.0 - fy)
                        + (at(x0, y0 + 1)[c] * (1.0 - fx) + at(x0 + 1, y0 + 1)[c] * fx) * fy;
                }
                let mut v = nalgebra::Vector3::new(p[0], p[1], p[2]);
                if ghost
                    && off != 0.0
                    && (x as i64) >= gx0
                    && (x as i64) < gx0 + gsize
                    && (y as i64) >= gy0
                    && (y as i64) < gy0 + gsize
                {
                    v = nalgebra::Vector3::new(0.30, 0.05, 0.04);
                }
                let mut cam = to_camera * (v * gain);
                for c in cam.iter_mut() {
                    let sigma = (a * c.max(0.0) + b * b).sqrt();
                    *c = (*c + sigma * noise.gauss()).min(1.0);
                }
                let back = from_camera * cam;
                img.put_pixel(x, y, image::Rgb([back[0], back[1], back[2]]));
            }
        }
        write(
            &format!("bracket_{k}_{ev:+.1}.dng"),
            &img,
            base_time * gain as f64,
        )?;
        eprintln!("wrote bracket {ev:+.1} EV");
    }
    Ok(())
}
