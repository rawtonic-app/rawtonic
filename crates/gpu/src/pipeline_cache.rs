//! The app's own pipeline cache on disk (wgpu `PipelineCache`, Vulkan only):
//! compiled pipelines kept across launches in the app's cache folder, beside
//! the driver's own cache. It does not make the very first launch fast (the
//! file is empty then); it serves launches where the driver cache misses but
//! this file does not (NVIDIA evicting entries, its cache turned off). Notes:
//! benchmark-results/windows-gpu-vfio-2026-10-30/COLD-START-OPTIONS.md (A).
//!
//! One file per adapter, driver and build (`file_name`); files of other
//! drivers or builds in the folder are removed when a cache opens. A file
//! carries its length and checksum: a torn or foreign file is ignored, and
//! wgpu checks its own header (backend, adapter, driver cache UUID) before
//! the driver sees the data.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAGIC: &[u8; 8] = b"RTPLC001";
const HEADER: usize = 8 + 8 + 8;
/// Larger files are not read (a driver's cache of all our pipelines is a
/// few MB).
const MAX_BYTES: u64 = 512 << 20;
/// Compiles finishing within this time are written together.
const SAVE_DELAY: Duration = Duration::from_secs(2);
const PREFIX: &str = "wgpu-pipelines-";

pub struct PipelineCache {
    cache: wgpu::PipelineCache,
    path: PathBuf,
    /// A save is scheduled; the length last written.
    state: Mutex<(bool, usize)>,
}

static CACHES: Mutex<Vec<(wgpu::Device, Arc<PipelineCache>)>> = Mutex::new(Vec::new());

/// The device features a pipeline cache needs, where `adapter` has them.
pub fn device_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & wgpu::Features::PIPELINE_CACHE
}

/// Opens (or starts) the cache file for `device` in `dir` and registers it,
/// so every pipeline of this crate created on `device` uses it. Does nothing
/// where the backend has no pipeline cache (Metal, DX12, GL).
pub fn open(device: &wgpu::Device, info: &wgpu::AdapterInfo, dir: &Path) {
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
        return;
    }
    let Some(name) = file_name(info) else { return };
    if let Err(error) = std::fs::create_dir_all(dir) {
        log::warn!("Pipeline cache folder unavailable: {error}");
        return;
    }
    remove_others(dir, &name);
    let path = dir.join(&name);
    let data = read(&path);
    let loaded = data.as_ref().map_or(0, Vec::len);
    // SAFETY: the data is what `get_data` returned for an adapter with the
    // same name, driver and build (the file name), checked whole by our
    // checksum; wgpu validates its header against this adapter and the
    // driver's cache UUID, and `fallback` starts an empty cache instead of
    // failing when it does not match.
    let cache = unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("Rawtonic pipelines"),
            data: data.as_deref(),
            fallback: true,
        })
    };
    log::info!(
        "GPU pipeline cache {}: {} bytes loaded",
        path.display(),
        loaded
    );
    let store = Arc::new(PipelineCache {
        cache,
        path,
        state: Mutex::new((false, loaded)),
    });
    let mut caches = CACHES.lock().unwrap_or_else(|e| e.into_inner());
    caches.retain(|(registered, _)| registered != device);
    caches.push((device.clone(), store));
}

/// The cache registered for `device`, if any.
pub(crate) fn for_device(device: &wgpu::Device) -> Option<Arc<PipelineCache>> {
    let caches = CACHES.lock().unwrap_or_else(|e| e.into_inner());
    caches
        .iter()
        .find(|(registered, _)| registered == device)
        .map(|(_, cache)| Arc::clone(cache))
}

impl PipelineCache {
    pub(crate) fn wgpu(&self) -> &wgpu::PipelineCache {
        &self.cache
    }

    /// Writes the cache shortly after a compile (several compiles finishing
    /// together are written once), on its own thread.
    pub(crate) fn save_soon(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.0 {
                return;
            }
            state.0 = true;
        }
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(SAVE_DELAY);
            this.state.lock().unwrap_or_else(|e| e.into_inner()).0 = false;
            this.save();
        });
    }

    fn save(&self) {
        let Some(data) = self.cache.get_data() else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if data.len() == state.1 {
            return;
        }
        match rawtonic_metadata::atomic_write(&self.path, encode(&data)) {
            Ok(()) => state.1 = data.len(),
            Err(error) => log::warn!("Pipeline cache not written: {error}"),
        }
    }
}

/// `wgpu-pipelines-<backend/vendor/device>-<driver and build hash>.bin`, or
/// `None` where wgpu keeps no cache data for the adapter.
fn file_name(info: &wgpu::AdapterInfo) -> Option<String> {
    let adapter = wgpu::util::pipeline_cache_key(info)?;
    let adapter = adapter.trim_start_matches("wgpu_pipeline_cache_");
    let mut hash = Fnv::default();
    for part in [
        info.name.as_str(),
        info.driver.as_str(),
        info.driver_info.as_str(),
        env!("CARGO_PKG_VERSION"),
        include_str!("shaders/shader.wgsl"),
    ] {
        hash.write(part.as_bytes());
        hash.write(&[0]);
    }
    Some(format!("{PREFIX}{adapter}-{:016x}.bin", hash.0))
}

/// Removes the cache files of other adapters, drivers or builds.
fn remove_others(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(PREFIX)
            && name != keep
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn read(path: &Path) -> Option<Vec<u8>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_BYTES {
        log::warn!("Pipeline cache {} ignored: {size} bytes", path.display());
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let data = decode(&bytes);
    if data.is_none() {
        log::warn!("Pipeline cache {} ignored: damaged", path.display());
    }
    data
}

fn encode(data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER + data.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(data.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&checksum(data).to_le_bytes());
    bytes.extend_from_slice(data);
    bytes
}

fn decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let header = bytes.get(..HEADER)?;
    let data = &bytes[HEADER..];
    let length = u64::from_le_bytes(header[8..16].try_into().ok()?);
    let sum = u64::from_le_bytes(header[16..24].try_into().ok()?);
    (&header[..8] == MAGIC
        && length == data.len() as u64
        && sum == checksum(data)
        && !data.is_empty())
    .then(|| data.to_vec())
}

fn checksum(data: &[u8]) -> u64 {
    let mut hash = Fnv::default();
    hash.write(data);
    hash.0
}

/// FNV-1a, 64 bit: stable across builds and platforms (unlike std's hasher).
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_round_trips_and_damage_is_rejected() {
        let data = b"driver cache bytes".to_vec();
        let bytes = encode(&data);
        assert_eq!(decode(&bytes), Some(data.clone()));
        // Torn write, flipped byte, foreign file, empty data.
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        let mut flipped = bytes.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(decode(&flipped), None);
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(decode(&longer), None);
        assert_eq!(decode(b"not a cache"), None);
        assert_eq!(decode(&encode(&[])), None);
    }

    #[test]
    fn the_file_name_changes_with_the_driver() {
        let info = |driver_info: &str| wgpu::AdapterInfo {
            name: "GPU".into(),
            vendor: 0x10de,
            device: 0x2206,
            device_type: wgpu::DeviceType::DiscreteGpu,
            device_pci_bus_id: String::new(),
            driver: "NVIDIA".into(),
            driver_info: driver_info.into(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 32,
            subgroup_max_size: 32,
            transient_saves_memory: false,
        };
        let first = file_name(&info("617.42")).unwrap();
        assert!(first.starts_with("wgpu-pipelines-vulkan_"), "{first}");
        assert_ne!(first, file_name(&info("617.57")).unwrap());
        // No cache data on Metal.
        let metal = wgpu::AdapterInfo {
            backend: wgpu::Backend::Metal,
            ..info("x")
        };
        assert_eq!(file_name(&metal), None);
    }

    #[test]
    fn only_other_cache_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["wgpu-pipelines-a.bin", "wgpu-pipelines-b.bin", "other.bin"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        remove_others(dir.path(), "wgpu-pipelines-b.bin");
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["other.bin", "wgpu-pipelines-b.bin"]);
    }
}
