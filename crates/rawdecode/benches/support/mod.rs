//! Inputs shared by the hot-path benchmarks (crates/rawdecode/benches,
//! crates/engine/benches, crates/export/benches; included by `#[path]`).
//!
//! Public RAW fixtures come from `$RAWTONIC_BENCH_FIXTURES`
//! (scripts/bench/fetch_fixtures.sh, raw.pixls.us, CC0). Without them every
//! benchmark falls back to a synthetic input of a similar size, and its id
//! says so (`synthetic_…`), so an A/B comparison never mixes the two.
#![allow(dead_code)]

use image::Rgb32FImage;
use rawtonic_rawdecode::demosaic::CfaPattern;
use rayon::prelude::*;
use std::path::PathBuf;

/// The fixture set (file names written by scripts/bench/fetch_fixtures.sh).
pub const BAYER_FIXTURES: [(&str, &str); 2] = [
    ("nef_d7200_24mp", "bayer-nef.NEF"),
    ("cr3_r10_24mp", "bayer-cr3.CR3"),
];
pub const XTRANS_FIXTURE: (&str, &str) = ("raf_xt3_26mp", "xtrans-raf.RAF");
/// The photo the develop and export benchmarks work on.
pub const PHOTO_FIXTURE: (&str, &str) = BAYER_FIXTURES[0];

/// Size of the synthetic stand-ins (24 MP like the fixtures).
pub const SYNTHETIC_SIZE: (u32, u32) = (6000, 4000);

/// The X-Trans layout (0 red, 1 green, 2 blue).
pub const XTRANS: [u8; 36] = [
    1, 1, 0, 1, 1, 2, //
    1, 1, 2, 1, 1, 0, //
    2, 0, 1, 0, 2, 1, //
    1, 1, 2, 1, 1, 0, //
    1, 1, 0, 1, 1, 2, //
    0, 2, 1, 2, 0, 1,
];
pub const BAYER: [u8; 4] = [0, 1, 1, 2];

pub fn bayer() -> CfaPattern {
    CfaPattern::new(2, 2, BAYER.to_vec()).unwrap()
}

pub fn xtrans() -> CfaPattern {
    CfaPattern::new(6, 6, XTRANS.to_vec()).unwrap()
}

fn fixture(name: &str) -> Option<Vec<u8>> {
    let dir = PathBuf::from(std::env::var_os("RAWTONIC_BENCH_FIXTURES")?);
    std::fs::read(dir.join(name)).ok()
}

/// RAW files to decode: (family, id, bytes). Each family uses its fixtures,
/// or a synthetic uncompressed DNG when none is present.
pub fn raw_files() -> Vec<(&'static str, String, Vec<u8>)> {
    let mut files: Vec<_> = BAYER_FIXTURES
        .iter()
        .filter_map(|(id, name)| Some(("bayer", id.to_string(), fixture(name)?)))
        .collect();
    let (w, h) = SYNTHETIC_SIZE;
    if files.is_empty() {
        files.push((
            "bayer",
            synthetic_id("dng"),
            synthetic_dng(w, h, (2, 2), &BAYER),
        ));
    }
    match fixture(XTRANS_FIXTURE.1) {
        Some(bytes) => files.push(("xtrans", XTRANS_FIXTURE.0.to_string(), bytes)),
        None => files.push((
            "xtrans",
            synthetic_id("dng"),
            synthetic_dng(w, h, (6, 6), &XTRANS),
        )),
    }
    files
}

fn synthetic_id(kind: &str) -> String {
    format!("synthetic_{kind}_{}x{}", SYNTHETIC_SIZE.0, SYNTHETIC_SIZE.1)
}

/// A full development as the app's decode does it (CPU demosaic: the
/// benchmarks never register a GPU), linear RGB.
pub fn develop(bytes: &[u8]) -> Rgb32FImage {
    rawtonic_rawdecode::raw_processing::develop_raw_image(bytes, false, 2.5, "auto".into(), None)
        .expect("decode")
        .into_rgb32f()
}

/// The photo the develop and export stages run on: the developed fixture,
/// or the synthetic scene. (id, linear RGB)
pub fn photo() -> (String, Rgb32FImage) {
    match fixture(PHOTO_FIXTURE.1) {
        Some(bytes) => (PHOTO_FIXTURE.0.to_string(), develop(&bytes)),
        None => (
            synthetic_id("scene"),
            scene(SYNTHETIC_SIZE.0, SYNTHETIC_SIZE.1),
        ),
    }
}

/// A box-filtered copy at about `width` pixels wide (previews).
pub fn reduced(image: &Rgb32FImage, width: u32) -> Rgb32FImage {
    let n = (image.width() / width).max(1);
    let (w, h) = (image.width() / n, image.height() / n);
    let src = image.as_raw();
    let stride = image.width() as usize * 3;
    let mut out = vec![0.0f32; (w * h * 3) as usize];
    out.par_chunks_mut(w as usize * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w as usize {
                let mut sum = [0.0f32; 3];
                for dy in 0..n as usize {
                    let line = (y * n as usize + dy) * stride;
                    for dx in 0..n as usize {
                        let i = line + (x * n as usize + dx) * 3;
                        for c in 0..3 {
                            sum[c] += src[i + c];
                        }
                    }
                }
                for c in 0..3 {
                    row[x * 3 + c] = sum[c] / (n * n) as f32;
                }
            }
        });
    Rgb32FImage::from_raw(w, h, out).unwrap()
}

/// The colour each CFA position of `image` would have recorded.
pub fn mosaic(image: &Rgb32FImage, cfa: &CfaPattern) -> Vec<f32> {
    let w = image.width() as usize;
    let src = image.as_raw();
    let mut out = vec![0.0f32; w * image.height() as usize];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = src[(y * w + x) * 3 + cfa.color(y as isize, x as isize)];
        }
    });
    out
}

/// Deterministic noise in [-0.5, 0.5) per index (splitmix64).
fn noise(i: u64) -> f32 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32 - 0.5
}

/// A linear-light test scene: a lit gradient, coloured discs and bars with
/// sharp edges, fine stripes and photon-like noise (about ISO 1600), so
/// edge-, noise- and colour-adaptive stages take their usual paths.
pub fn scene(width: u32, height: u32) -> Rgb32FImage {
    let (w, h) = (width as usize, height as usize);
    let s = h as f32;
    let discs: [([f32; 2], f32, [f32; 3]); 6] = [
        ([0.25, 0.3], 0.18, [0.55, 0.12, 0.08]),
        ([0.6, 0.35], 0.22, [0.08, 0.3, 0.6]),
        ([1.05, 0.7], 0.2, [0.1, 0.45, 0.12]),
        ([0.4, 0.75], 0.14, [0.7, 0.6, 0.1]),
        ([1.3, 0.25], 0.12, [0.9, 0.9, 0.88]),
        ([0.85, 0.15], 0.08, [0.02, 0.02, 0.03]),
    ];
    let mut out = vec![0.0f32; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let fy = y as f32 / s;
        for x in 0..w {
            let fx = x as f32 / s;
            let light = 0.05 + 0.25 * fx / (w as f32 / s);
            let mut rgb = [light * 0.9, light, light * 1.1];
            for (centre, radius, colour) in discs {
                let d = ((fx - centre[0]).powi(2) + (fy - centre[1]).powi(2)).sqrt();
                if d < radius {
                    rgb = colour;
                }
            }
            // Fine stripes (aliasing-prone detail) in one band.
            if (0.88..0.95).contains(&fy) {
                let v = 0.2 + 0.15 * ((x as f32 * 1.7).sin() * (y as f32 * 0.9).cos());
                rgb = [v, v, v];
            }
            let i = (y * w + x) as u64 * 3;
            for c in 0..3 {
                let v = rgb[c];
                row[x * 3 + c] = (v + noise(i + c as u64) * 0.04 * (v + 0.01).sqrt()).max(0.0);
            }
        }
    });
    Rgb32FImage::from_raw(width, height, out).unwrap()
}

/// The synthetic scene as a 12-bit uncompressed DNG with this CFA.
pub fn synthetic_dng(width: u32, height: u32, cfa_size: (u16, u16), cfa: &[u8]) -> Vec<u8> {
    let pattern = CfaPattern::new(cfa_size.0 as usize, cfa_size.1 as usize, cfa.to_vec()).unwrap();
    let values = mosaic(&scene(width, height), &pattern);
    let samples: Vec<u16> = values
        .iter()
        .map(|v| (256.0 + v.clamp(0.0, 1.0) * 3839.0).round() as u16)
        .collect();
    rawtonic_rawdecode::fingerprint::synthetic::mosaic_dng(width, height, cfa_size, cfa, &samples)
}
