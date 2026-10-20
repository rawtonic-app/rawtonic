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

use anyhow::{Context, Result, ensure};
use rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser;

pub use tiles::{HALO, TILE};

/// The model package's folder name in the app's model folder.
pub const CORE_ML_PACKAGE: &str = "nlmf.mlpackage";
/// The mixed-precision package of the same network with 640 px tiles (AI
/// Denoise version 3): convolutions in float16, the feature matching in
/// float32; 2.6× faster than `CORE_ML_PACKAGE` per tile and 1.4× fewer
/// samples through the halo (scripts/models/convert_nlmf_coreml.py).
pub const CORE_ML_MIXED_PACKAGE: &str = "nlmf_mixed640.mlpackage";
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
    fn cache_tag(&self) -> String {
        String::new()
    }

    /// Tile edge the network takes, in packed pixels.
    fn tile(&self) -> usize {
        TILE
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

    /// Core ML with the mixed-precision package at `package` (AI Denoise
    /// version 3): GPU only, where it is fastest (the Neural Engine runs it
    /// 4× slower); its results are cached apart from the first package's.
    #[cfg(target_os = "macos")]
    pub fn core_ml_mixed(package: &Path, cache_dir: Option<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            Box::new(coreml::CoreMl::load_tagged(
                package,
                coreml::ComputeUnits::CpuAndGpu,
                "coreml-mixed",
            )?),
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
        self.denoise_scaled(mosaic, width, height, pattern, 1.0)
    }

    fn denoise_scaled(
        &self,
        mosaic: &[f32],
        width: usize,
        height: usize,
        pattern: [u8; 4],
        noise_scale: f32,
    ) -> Result<Vec<f32>> {
        ensure!(
            noise_scale.is_finite() && noise_scale > 0.0,
            "noise scale {noise_scale}"
        );
        // Scale 1 keeps the keys results were cached under before scales.
        let backend_tag = self.backend.cache_tag();
        let mut tag = backend_tag.clone();
        if noise_scale != 1.0 {
            tag.push_str(&format!("|noise×{noise_scale}"));
        }
        let key = cache_key(&tag, mosaic, width, height, pattern);
        if let Some(path) = self.cache_path(key)
            && let Some(cached) = read_cache(&path, mosaic.len())
        {
            return Ok(cached);
        }
        let started = std::time::Instant::now();
        let planes = tiles::pack(mosaic, width, height, pattern);
        let profile = noise::estimate(&planes)?;
        let denoised = tiles::run(&planes, &profile, self.backend.as_ref(), noise_scale)?;
        let mut out = tiles::unpack(&denoised, width, height, pattern);
        for v in &mut out {
            *v = half::f16::from_f32(v.clamp(0.0, 1.0)).to_f32();
        }
        eprintln!(
            "MatchFilter: {width}x{height} in {:.1} s, noise (shot, read) {:?} ×{noise_scale}",
            started.elapsed().as_secs_f32(),
            profile.channels
        );
        // A backend that moved to the CPU during the run (a GPU fault) made
        // a mixed result, which is neither the GPU's nor the CPU's.
        let same_backend = self.backend.cache_tag() == backend_tag;
        if let Some(path) = self.cache_path(key).filter(|_| same_backend) {
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

/// A cached result, `None` when missing or unusable: of another length, or
/// with non-finite values (written before NaN tiles were refused).
fn read_cache(path: &Path, len: usize) -> Option<Vec<f32>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != len * 2 {
        return None;
    }
    let values: Vec<f32> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| half::f16::from_le_bytes(b).to_f32())
        .collect();
    values.iter().all(|v| v.is_finite()).then_some(values)
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

/// A temporary file of its own beside the target (the app and the CLI share
/// the cache), then a rename.
fn write_cache(path: &Path, values: &[f32]) -> Result<()> {
    let dir = path.parent().context("cache path without a folder")?;
    fs::create_dir_all(dir)?;
    let mut file = tempfile::Builder::new()
        .prefix(".denoise-")
        .suffix(".partial")
        .tempfile_in(dir)?;
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for &v in values {
        bytes.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
    }
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
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

    /// Returns NaN, as a faulting GPU can without an error.
    struct Faulty;

    impl Backend for Faulty {
        fn run(&self, _: &[f32]) -> Result<Vec<f32>> {
            Ok(vec![f32::NAN; 4 * TILE * TILE])
        }
    }

    #[test]
    fn non_finite_network_output_is_an_error_and_not_cached() {
        let (w, h) = (300, 200);
        let dir = tempfile::tempdir().unwrap();
        let filter = MatchFilter::new(Box::new(Faulty), Some(dir.path().to_path_buf()));
        let error = filter
            .denoise(&noisy_mosaic(w, h), w, h, [0, 1, 1, 2])
            .unwrap_err();
        assert!(format!("{error:#}").contains("non-finite"), "{error:#}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Identity on a "GPU" that moves to the CPU after its first tile, as
    /// rawtonic-onnx does after a failed run.
    struct FallsBack(std::sync::atomic::AtomicUsize);

    impl Backend for FallsBack {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(input[..4 * TILE * TILE].to_vec())
        }

        fn cache_tag(&self) -> String {
            match self.0.load(std::sync::atomic::Ordering::SeqCst) {
                0 => "webgpu".to_string(),
                _ => String::new(),
            }
        }
    }

    #[test]
    fn a_result_from_a_backend_that_changed_during_the_run_is_not_cached() {
        let (w, h) = (300, 200);
        let mosaic = noisy_mosaic(w, h);
        let dir = tempfile::tempdir().unwrap();
        let filter = MatchFilter::new(
            Box::new(FallsBack(Default::default())),
            Some(dir.path().to_path_buf()),
        );
        filter.denoise(&mosaic, w, h, [0, 1, 1, 2]).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        // Now on the CPU from the start: cached.
        filter.denoise(&mosaic, w, h, [0, 1, 1, 2]).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_cached_result_with_nan_is_computed_again() {
        let (w, h) = (300, 200);
        let mosaic = noisy_mosaic(w, h);
        let dir = tempfile::tempdir().unwrap();
        let filter = MatchFilter::new(Box::new(Identity), Some(dir.path().to_path_buf()));
        let first = filter.denoise(&mosaic, w, h, [0, 1, 1, 2]).unwrap();
        let cached = fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        // As an earlier build cached a faulty GPU's tile.
        let mut bytes = fs::read(&cached).unwrap();
        bytes[..2].copy_from_slice(&half::f16::NAN.to_le_bytes());
        fs::write(&cached, &bytes).unwrap();
        let again = filter.denoise(&mosaic, w, h, [0, 1, 1, 2]).unwrap();
        assert_eq!(again, first);
        assert_eq!(read_cache(&cached, mosaic.len()).unwrap(), first);
    }

    #[test]
    fn cache_writes_leave_other_writers_staging_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("00000000000000ab.f16");
        // The name every writer used before: another process mid-write.
        let other = path.with_extension("f16.partial");
        fs::write(&other, b"other writer").unwrap();
        write_cache(&path, &[0.25, 0.5]).unwrap();
        assert_eq!(read_cache(&path, 2).unwrap(), [0.25, 0.5]);
        assert_eq!(fs::read(&other).unwrap(), b"other writer");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
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

    /// `Identity` on 400 px tiles.
    struct WideIdentity;

    impl Backend for WideIdentity {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
            Ok(input[..4 * 400 * 400].to_vec())
        }

        fn tile(&self) -> usize {
            400
        }
    }

    #[test]
    fn the_tile_is_the_backends_and_noise_scales_cache_apart() {
        let (w, h) = (900, 620);
        let mosaic = noisy_mosaic(w, h);
        let dir = tempfile::tempdir().unwrap();
        let filter = MatchFilter::new(Box::new(WideIdentity), Some(dir.path().to_path_buf()));
        let plain = filter.denoise(&mosaic, w, h, [0, 1, 1, 2]).unwrap();
        for (a, b) in plain.iter().zip(&mosaic) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
        let scaled = filter
            .denoise_scaled(&mosaic, w, h, [0, 1, 1, 2], 0.8)
            .unwrap();
        assert_eq!(
            plain, scaled,
            "an identity network ignores the noise planes"
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            2,
            "one cache file per scale"
        );
        assert!(
            filter
                .denoise_scaled(&mosaic, w, h, [0, 1, 1, 2], 0.0)
                .is_err()
        );
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
