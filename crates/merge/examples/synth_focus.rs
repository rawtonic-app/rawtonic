//! Test focus stacks with a known answer: one lens-corrected RAW is the
//! all-in-focus scene with a depth that runs from top (far) to bottom
//! (near); each frame is sharp at one depth and blurred elsewhere in
//! proportion to the distance, magnified a little more per frame (focus
//! breathing), with noise. A stack should reproduce `truth.dng`.
//!
//! cargo run --release -p rawtonic-merge --example synth_focus -- RAW OUTDIR FRAMES
use anyhow::{Context, Result};
use image::Rgb32FImage;
use rawtonic_merge::dng::{DngMeta, encode_linear_dng};
use rawtonic_merge::frame::load_frame;
use rawtonic_merge::hdr::working_to_camera;
use rawtonic_merge::planes::{Plane, gaussian_blur};
use std::path::PathBuf;

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
    anyhow::ensure!(args.len() >= 4, "usage: RAW OUTDIR FRAMES");
    let raw = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let n: usize = args[3].parse()?;
    std::fs::create_dir_all(&out)?;
    let mut frame = load_frame(&raw, &Default::default())?;
    let adjustments = rawtonic_merge::initial_adjustments(&raw, false);
    frame.image = rawtonic_engine::image_processing::apply_geometry_warp(
        image::DynamicImage::ImageRgb32F(std::mem::take(&mut frame.image)),
        &adjustments,
    )
    .into_owned()
    .into_rgb32f();
    let to_camera = working_to_camera(frame.color.as_ref());
    let from_camera = to_camera.try_inverse().context("singular colour matrix")?;
    let (w, h) = frame.image.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let write = |name: &str, image: &Rgb32FImage| -> Result<()> {
        let bytes = encode_linear_dng(
            image,
            &DngMeta {
                color: frame.color.as_ref(),
                metadata: frame.metadata.as_ref(),
                keep_lens: false,
                baseline_exposure: Some(
                    rawtonic_engine::baseline_exposure::dng_baseline_equivalent(
                        &raw.to_string_lossy(),
                    ) as f64,
                ),
                preview: None,
                software: "Rawtonic test focus stack",
            },
        )?;
        std::fs::write(out.join(name), bytes)?;
        Ok(())
    };
    write("truth.dng", &frame.image)?;

    // Planar copy and blur levels (sigma in pixels).
    let src: &[f32] = &frame.image;
    let planes: Vec<Plane> = (0..3)
        .map(|c| Plane {
            w: wu,
            h: hu,
            data: src.iter().skip(c).step_by(3).copied().collect(),
        })
        .collect();
    let sigmas = [0.0f32, 1.5, 3.0, 6.0, 12.0];
    let levels: Vec<Vec<Plane>> = sigmas
        .iter()
        .map(|&s| {
            planes
                .iter()
                .map(|p| {
                    if s == 0.0 {
                        p.clone()
                    } else {
                        gaussian_blur(p, s)
                    }
                })
                .collect()
        })
        .collect();
    let blur_at = |depth_gap: f32| -> (usize, f32) {
        // Blur grows by 12 px per full depth range of distance.
        let s = (depth_gap * 24.0).min(12.0);
        let mut i = 0;
        while i + 1 < sigmas.len() && sigmas[i + 1] < s {
            i += 1;
        }
        if i + 1 == sigmas.len() {
            return (i, 0.0);
        }
        (i, (s - sigmas[i]) / (sigmas[i + 1] - sigmas[i]))
    };
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    for k in 0..n {
        let focus_depth = k as f32 / (n - 1) as f32;
        let mag = 1.0 + 0.002 * (k as f64 - (n as f64 - 1.0) / 2.0);
        let mut noise = Noise(0x1357_9BDF_2468_ACE0 ^ ((k as u64 + 1) * 0x9E37_79B9));
        let (a, b) = (1.0 / 8000.0f32, 0.0005f32);
        let mut img = Rgb32FImage::new(w, h);
        for y in 0..hu {
            for x in 0..wu {
                // Magnified source position.
                let sx = ((x as f64 - cx) / mag + cx).clamp(0.0, (w - 1) as f64 - 0.001);
                let sy = ((y as f64 - cy) / mag + cy).clamp(0.0, (h - 1) as f64 - 0.001);
                let depth = (sy / (h - 1) as f64) as f32;
                let (li, lt) = blur_at((depth - focus_depth).abs());
                let mut p = [0.0f32; 3];
                for (c, pc) in p.iter_mut().enumerate() {
                    let a0 = levels[li][c].sample_bilinear(sx as f32, sy as f32);
                    let a1 = if lt > 0.0 {
                        levels[li + 1][c].sample_bilinear(sx as f32, sy as f32)
                    } else {
                        a0
                    };
                    *pc = a0 * (1.0 - lt) + a1 * lt;
                }
                let mut cam = to_camera * nalgebra::Vector3::new(p[0], p[1], p[2]);
                for c in cam.iter_mut() {
                    let sigma = (a * c.max(0.0) + b * b).sqrt();
                    *c = (*c + sigma * noise.gauss()).min(1.0);
                }
                let back = from_camera * cam;
                img.put_pixel(x as u32, y as u32, image::Rgb([back[0], back[1], back[2]]));
            }
        }
        write(&format!("frame_{k}.dng"), &img)?;
        eprintln!("wrote frame {k}: focus depth {focus_depth:.2}, magnification {mag:.4}");
    }
    Ok(())
}
