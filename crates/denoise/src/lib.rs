// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI denoising of Bayer mosaics with Nonlocal MatchFilter (Sánchez-Beeckman
//! and Buades, MIT; github.com/MIA-UIB/nonlocal-matchfilter).
//!
//! The network sees the mosaic packed into R, G, B, G planes (half size) and
//! the noise standard deviation of each sample, `sqrt(shot * x + read)`, as
//! it was trained; shot and read are estimated from the photo itself
//! (`noise::estimate`). It runs on 320 px tiles with a 40 px halo, on
//! Core ML on macOS and ONNX Runtime elsewhere. Results are rounded to f16,
//! cached or not, so a cached and a fresh result are the same bytes.
//! docs/MODELS.md has the measurements.

pub mod noise;
mod tiles;

#[cfg(target_os = "macos")]
pub mod coreml;
mod onnx;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser;

pub use tiles::{HALO, TILE};

/// The model package's folder name in the app's model folder.
pub const CORE_ML_PACKAGE: &str = "nlmf.mlpackage";
/// The ONNX model's file name there.
pub const ONNX_MODEL: &str = "nlmf.onnx";

/// Identifies the network and the input it gets; part of the cache key.
const MODEL_ID: &str = "nonlocal-matchfilter-rawnoise_25_15_9nbr-v1";

/// One inference: `[8, TILE, TILE]` in (packed planes, then their noise
/// standard deviations), `[4, TILE, TILE]` out.
pub trait Backend: Send + Sync {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>>;

    /// Part of the cache key for backends whose results differ from the
    /// CPU's in the last bits (a GPU), so a cached result is the one this
    /// backend computes; empty for the CPU and Core ML (keys unchanged).
    fn cache_tag(&self) -> &'static str {
        ""
    }
}

/// The MatchFilter mosaic denoiser.
pub struct MatchFilter {
    backend: Box<dyn Backend>,
    cache_dir: Option<PathBuf>,
    /// Oldest cached results go once the cache is larger.
    cache_limit: Option<u64>,
}

impl MatchFilter {
    pub fn new(backend: Box<dyn Backend>, cache_dir: Option<PathBuf>) -> Self {
        Self {
            backend,
            cache_dir,
            cache_limit: None,
        }
    }

    /// Keeps the cache below `bytes`, dropping the least recently written.
    pub fn with_cache_limit(mut self, bytes: u64) -> Self {
        self.cache_limit = Some(bytes);
        self
    }

    /// Core ML with the model package at `package` (macOS).
    #[cfg(target_os = "macos")]
    pub fn core_ml(package: &Path, cache_dir: Option<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            Box::new(coreml::CoreMl::load(package)?),
            cache_dir,
        ))
    }

    /// ONNX Runtime with the model at `model` (the runtime must be loaded).
    pub fn onnx(model: &Path, cache_dir: Option<PathBuf>) -> Result<Self> {
        Ok(Self::new(Box::new(onnx::Onnx::load(model)?), cache_dir))
    }

    fn cache_path(&self, key: u64) -> Option<PathBuf> {
        self.cache_dir
            .as_ref()
            .map(|dir| dir.join(format!("{key:016x}.f16")))
    }
}

impl MosaicDenoiser for MatchFilter {
    fn denoise(
        &self,
        mosaic: &[f32],
        width: usize,
        height: usize,
        pattern: [u8; 4],
    ) -> Result<Vec<f32>> {
        let key = cache_key(self.backend.cache_tag(), mosaic, width, height, pattern);
        if let Some(path) = self.cache_path(key)
            && let Some(cached) = read_cache(&path, mosaic.len())
        {
            return Ok(cached);
        }
        let started = std::time::Instant::now();
        let planes = tiles::pack(mosaic, width, height, pattern);
        let profile = noise::estimate(&planes)?;
        let denoised = tiles::run(&planes, &profile, self.backend.as_ref())?;
        let mut out = tiles::unpack(&denoised, width, height, pattern);
        for v in &mut out {
            *v = half::f16::from_f32(v.clamp(0.0, 1.0)).to_f32();
        }
        eprintln!(
            "MatchFilter: {width}x{height} in {:.1} s, noise (shot, read) {:?}",
            started.elapsed().as_secs_f32(),
            profile.channels
        );
        if let Some(path) = self.cache_path(key) {
            // A cache that cannot be written only costs the next run time.
            let _ = write_cache(&path, &out);
            if let (Some(limit), Some(dir)) = (self.cache_limit, path.parent()) {
                prune(dir, limit);
            }
        }
        Ok(out)
    }
}

/// FNV-1a over the samples' bits and the layout (stable across builds).
fn cache_key(tag: &str, mosaic: &[f32], width: usize, height: usize, pattern: [u8; 4]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |word: u64| {
        hash ^= word;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for b in MODEL_ID.bytes().chain(tag.bytes()) {
        eat(b as u64);
    }
    eat(width as u64);
    eat(height as u64);
    eat(u32::from_le_bytes(pattern) as u64);
    for pair in mosaic.chunks(2) {
        let lo = pair[0].to_bits() as u64;
        let hi = pair.get(1).map_or(0, |v| v.to_bits() as u64);
        eat(lo | (hi << 32));
    }
    hash
}

fn read_cache(path: &Path, len: usize) -> Option<Vec<f32>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != len * 2 {
        return None;
    }
    Some(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| half::f16::from_le_bytes(b).to_f32())
            .collect(),
    )
}

/// Deletes the oldest `.f16` files until the folder holds at most `limit` bytes.
fn prune(dir: &Path, limit: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "f16"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Temporary file beside the target, then a rename.
fn write_cache(path: &Path, values: &[f32]) -> Result<()> {
    let dir = path.parent().context("cache path without a folder")?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("f16.partial");
    {
        let mut file = fs::File::create(&tmp)?;
        let mut bytes = Vec::with_capacity(values.len() * 2);
        for &v in values {
            bytes.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns each tile's first four planes unchanged.
    struct Identity;

    impl Backend for Identity {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
            Ok(input[..4 * TILE * TILE].to_vec())
        }
    }

    fn noisy_mosaic(width: usize, height: usize) -> Vec<f32> {
        let mut seed = 12345u64;
        (0..width * height)
            .map(|i| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = ((seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.02;
                0.1 + 0.3 * ((i % width) as f32 / width as f32) + noise
            })
            .collect()
    }

    #[test]
    fn an_identity_network_returns_the_mosaic_and_the_cache_matches() {
        let (w, h) = (700, 530); // tiles do not divide the planes evenly
        let mosaic = noisy_mosaic(w, h);
        let dir = tempfile::tempdir().unwrap();
        let filter = MatchFilter::new(Box::new(Identity), Some(dir.path().to_path_buf()));
        for pattern in [[0, 1, 1, 2], [1, 0, 2, 1], [2, 1, 1, 0], [1, 2, 0, 1]] {
            let out = filter.denoise(&mosaic, w, h, pattern).unwrap();
            for (a, b) in out.iter().zip(&mosaic) {
                assert!((a - b).abs() < 1e-3, "{a} vs {b} for {pattern:?}");
            }
            let again = filter.denoise(&mosaic, w, h, pattern).unwrap();
            assert_eq!(out, again, "the cached result equals the fresh one");
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod core_ml_tests {
    use super::*;

    fn read_f32(path: &Path) -> Vec<f32> {
        fs::read(path)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect()
    }

    /// The Core ML backend gives the reference output (coremltools in
    /// Python) for a reference tile: `$NLMF_PACKAGE` (the .mlpackage) and
    /// `$NLMF_TILE_DIR` with `input.f32` / `output.f32`.
    #[test]
    #[ignore = "needs NLMF_PACKAGE and NLMF_TILE_DIR"]
    fn core_ml_matches_the_reference_tile() {
        let package = PathBuf::from(std::env::var("NLMF_PACKAGE").unwrap());
        let dir = PathBuf::from(std::env::var("NLMF_TILE_DIR").unwrap());
        let backend = coreml::CoreMl::load(&package).unwrap();
        let input = read_f32(&dir.join("input.f32"));
        let expected = read_f32(&dir.join("output.f32"));
        let started = std::time::Instant::now();
        let got = backend.run(&input).unwrap();
        let t = started.elapsed();
        let worst = got
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        eprintln!("max |difference| {worst:.2e}, {t:?}");
        assert!(worst < 2e-3, "max difference {worst}");
    }
}
