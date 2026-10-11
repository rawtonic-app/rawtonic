//! Test panoramas with a known answer: one lens-corrected RAW is the scene;
//! views of a camera that rotates about its centre (same focal length) are
//! rendered from it with exposure differences and noise and written as
//! linear DNGs without lens data. A stitch should reproduce the scene
//! (`truth.dng`) up to its own projection.
//!
//! cargo run --release -p rawtonic-merge --example synth_pano -- RAW OUTDIR FOCAL_PX YAW[,YAW…] [PITCH,…]
use anyhow::{Context, Result};
use image::Rgb32FImage;
use nalgebra::{Rotation3, Vector3};
use rawtonic_merge::dng::{DngMeta, encode_linear_dng};
use rawtonic_merge::frame::load_frame;
use rawtonic_merge::hdr::working_to_camera;
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
    anyhow::ensure!(
        args.len() >= 5,
        "usage: RAW OUTDIR FOCAL_PX YAW[,YAW…] [PITCH,…]"
    );
    let raw = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let focal: f64 = args[3].parse()?;
    let yaws: Vec<f64> = args[4]
        .split(',')
        .map(|s| s.parse())
        .collect::<Result<_, _>>()?;
    let pitches: Vec<f64> = match args.get(5).filter(|a| !a.starts_with("--")) {
        Some(p) => p.split(',').map(|s| s.parse()).collect::<Result<_, _>>()?,
        None => vec![0.0; yaws.len()],
    };
    let ghost = args.iter().any(|a| a == "--ghost");
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
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let (vw, vh) = ((w as f64 * 0.4) as u32, (h as f64 * 0.6) as u32);
    let (vcx, vcy) = ((vw as f64 - 1.0) / 2.0, (vh as f64 - 1.0) / 2.0);

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
                keep_lens: false,
                baseline_exposure: Some(
                    rawtonic_engine::baseline_exposure::dng_baseline_equivalent(
                        &raw.to_string_lossy(),
                    ) as f64,
                ),
                preview: None,
                software: "Rawtonic test panorama",
            },
        )?;
        std::fs::write(out.join(name), bytes)?;
        Ok(())
    };
    write("truth.dng", &frame.image, base_time)?;
    // --brackets EV,EV,…: each view at these exposures (clipped at the
    // sensor white), written in shooting order view_k_e.
    let brackets: Option<Vec<f32>> = args
        .iter()
        .position(|a| a == "--brackets")
        .and_then(|i| args.get(i + 1))
        .map(|v| v.split(',').map(|e| e.parse().unwrap_or(0.0)).collect());

    let src: &[f32] = &frame.image;
    let sample = |x: f64, y: f64| -> Option<[f32; 3]> {
        if x < 0.0 || y < 0.0 || x >= (w - 1) as f64 || y >= (h - 1) as f64 {
            return None;
        }
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let at = |xx: usize, yy: usize| &src[(yy * w as usize + xx) * 3..][..3];
        Some(std::array::from_fn(|c| {
            (at(x0, y0)[c] * (1.0 - fx) + at(x0 + 1, y0)[c] * fx) * (1.0 - fy)
                + (at(x0, y0 + 1)[c] * (1.0 - fx) + at(x0 + 1, y0 + 1)[c] * fx) * fy
        }))
    };
    for (k, (&yaw, &pitch)) in yaws.iter().zip(&pitches).enumerate() {
        let roll = 0.6 * (k as f64 - 1.0);
        // Camera → world rotation.
        let r =
            Rotation3::from_euler_angles(pitch.to_radians(), yaw.to_radians(), roll.to_radians());
        let gain = 2f32.powf(0.3 * ((k as f32 * 1.7).sin()));
        let mut noise = Noise(0xA5A5_5A5A_1234_5678 ^ ((k as u64 + 1) * 0x9E37_79B9));
        let (a, b) = (1.0 / 8000.0f32, 0.0005f32);
        let mut scene = vec![[0.0f32; 3]; (vw * vh) as usize];
        let mut missing = 0usize;
        for y in 0..vh {
            for x in 0..vw {
                let ray = r * Vector3::new((x as f64 - vcx) / focal, (y as f64 - vcy) / focal, 1.0);
                let p = if ray.z > 0.0 {
                    sample(focal * ray.x / ray.z + cx, focal * ray.y / ray.z + cy)
                } else {
                    None
                };
                let mut p = p.unwrap_or_else(|| {
                    missing += 1;
                    [0.0; 3]
                });
                // --ghost: someone who stands in the middle view only, in its
                // overlap with the view to its left.
                if ghost && k == 1 {
                    let (fx, fy) = (x as f64 / vw as f64, y as f64 / vh as f64);
                    if (0.10..0.30).contains(&fx) && (0.35..0.75).contains(&fy) {
                        p = [0.25, 0.04, 0.03];
                    }
                }
                scene[(y * vw + x) as usize] = p;
            }
        }
        anyhow::ensure!(
            missing < (vw * vh / 100) as usize,
            "view {k} leaves the scene ({missing} px)"
        );
        for (e, &ev) in brackets.clone().unwrap_or(vec![0.0]).iter().enumerate() {
            let g = gain * 2f32.powf(ev);
            let mut img = Rgb32FImage::new(vw, vh);
            for (i, px) in img.pixels_mut().enumerate() {
                let p = scene[i];
                let mut cam = to_camera * (nalgebra::Vector3::new(p[0], p[1], p[2]) * g);
                for c in cam.iter_mut() {
                    let sigma = (a * c.max(0.0) + b * b).sqrt();
                    *c = (*c + sigma * noise.gauss()).min(1.0);
                }
                let back = from_camera * cam;
                *px = image::Rgb([back[0], back[1], back[2]]);
            }
            let name = if brackets.is_some() {
                format!("view_{k}_{e}.dng")
            } else {
                format!("view_{k}.dng")
            };
            write(&name, &img, base_time * 2f64.powf(ev as f64))?;
        }
        eprintln!("wrote view {k}: yaw {yaw} pitch {pitch} roll {roll:.1} gain {gain:.3}");
    }
    Ok(())
}
