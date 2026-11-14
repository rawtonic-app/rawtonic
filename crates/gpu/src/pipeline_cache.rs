//! The app's own pipeline cache on disk (wgpu `PipelineCache`, Vulkan only):
//! compiled pipelines kept across launches in the app's cache folder, beside
//! the driver's own cache. It does not make the very first launch fast (the
//! file is empty then); it serves launches where the driver cache misses but
//! this file does not (NVIDIA evicting entries, its cache turned off). Notes:
//! benchmark-results/windows-gpu-vfio-2026-10-30/COLD-START-OPTIONS.md (A).
//!
//! A warm-up run (`rawtonic-app --warm-gpu-cache`, a QA and benchmark tool;
//! no installer runs it) fills the file before the first launch: the app reads it as
//! its own, so that launch does not wait for the compiles (notes:
//! COLD-START-OPTIONS.md, B1; W4 in RESULTS-2.md: variant 17 0.09 s instead
//! of 10–13 s cold on NVIDIA/Windows).
//!
//! One file per adapter, driver and build (`file_name`; the build is the
//! app's version, so an update starts a new file); files of other drivers or
//! builds in the folder are removed when a cache opens. A file
//! carries its length and checksum: a torn or foreign file is ignored, and
//! wgpu checks its own header (backend, adapter, driver cache UUID) before
//! the driver sees the data.
//!
//! An idle compile in a helper process (`idle_compile_child`) starts from
//! this cache's data (`seed`) and the app takes the helper's cache over as
//! its own afterwards (`adopt`), so a variant a helper compiled is in this
//! file too and the next launch finds it when the driver's cache is gone
//! (G76/HCF). The file holds each pipeline of this build once: it grows
//! only with the variants used, and a helper starts from at most
//! `MAX_SEED` bytes.

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
/// A helper starts from the app's cache data only up to this size; beyond
/// it the helper starts empty and its pipeline stays out of the file.
const MAX_SEED: usize = 64 << 20;

pub struct PipelineCache {
    /// Replaced by a helper's cache that started from it (`adopt`).
    cache: Mutex<wgpu::PipelineCache>,
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
/// so every pipeline of this crate created on `device` uses it. `build`
/// names the program's version (part of the file name). Does nothing where
/// the backend has no pipeline cache (Metal, DX12, GL).
pub fn open(device: &wgpu::Device, info: &wgpu::AdapterInfo, dir: &Path, build: &str) {
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
        return;
    }
    let Some(name) = file_name(info, build) else {
        return;
    };
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
        cache: Mutex::new(cache),
        path,
        state: Mutex::new((false, loaded)),
    });
    let mut caches = CACHES.lock().unwrap_or_else(|e| e.into_inner());
    caches.retain(|(registered, _)| registered != device);
    caches.push((device.clone(), store));
}

/// Writes the cache of `device` now (a warm-up run before it exits) and
/// returns its file and length, or `None` without a cache or data.
pub fn save_now(device: &wgpu::Device) -> Option<(PathBuf, usize)> {
    let cache = for_device(device)?;
    cache.save();
    let length = cache.state.lock().unwrap_or_else(|e| e.into_inner()).1;
    (length > 0).then(|| (cache.path.clone(), length))
}

/// The name of `device`'s cache file (`file_name`: adapter, driver, build),
/// if a cache is registered for it.
pub(crate) fn file_name_of(device: &wgpu::Device) -> Option<std::ffi::OsString> {
    for_device(device)?
        .path
        .file_name()
        .map(std::ffi::OsString::from)
}

/// A cache for `device` from the file `name` in `dir`, which another process
/// on the same adapter, driver and build wrote (`file_name` is the same
/// only then; wgpu checks the data's header against the device too), not
/// registered: what an idle compile in a helper process left
/// (`idle_compile_child`). `None` without such a valid file.
pub(crate) fn foreign(
    device: &wgpu::Device,
    dir: &Path,
    name: &std::ffi::OsStr,
) -> Option<wgpu::PipelineCache> {
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
        return None;
    }
    let data = read(&dir.join(name))?;
    // SAFETY: as in `open`: data `get_data` returned for an adapter with the
    // same name, driver and build (the file name), checked whole by our
    // checksum; wgpu validates its header against this adapter and the
    // driver's cache UUID, and `fallback` starts an empty cache instead.
    Some(unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("Rawtonic idle compile"),
            data: Some(&data),
            fallback: true,
        })
    })
}

/// Writes the data of `device`'s cache into `dir` under its file name
/// (`file_name_of`), for an idle compile helper to start from, so that the
/// cache it leaves holds the app's pipelines and its own (`adopt`). `None`
/// without a cache, or when the data is larger than `MAX_SEED` or could not
/// be written: the helper then starts empty, and its cache is used only to
/// take its pipeline over (`false`).
pub(crate) fn seed(device: &wgpu::Device, dir: &Path) -> bool {
    let Some(cache) = for_device(device) else {
        return false;
    };
    let Some(name) = cache.path.file_name() else {
        return false;
    };
    seed_file(&dir.join(name), cache.current().get_data().as_deref())
}

/// Writes `data` (a cache's data) to `path` for a helper: `false` when it
/// is not there or too large, or the write failed. Empty data needs no
/// file (the helper starts empty all the same).
fn seed_file(path: &Path, data: Option<&[u8]>) -> bool {
    match data {
        None => false,
        Some([]) => true,
        Some(data) if data.len() > MAX_SEED => {
            log::info!(
                "Pipeline cache of {} bytes: an idle compile helper starts empty",
                data.len()
            );
            false
        }
        Some(data) => match write(path, data) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("Pipeline cache not passed to the helper: {error}");
                false
            }
        },
    }
}

/// Makes `cache` (a helper's that started from `seed`'s data) `device`'s
/// own cache and writes it soon: it holds every pipeline the app's held
/// then and the helper's. Nothing compiled into the app's cache meanwhile
/// that is not in it, apart from small pipelines a new processor made:
/// under a compile limit (the only place helpers run) every shader variant
/// compiles with a compile slot, and a compile waiting for the slot stops
/// the helper (`slot_wanted`), so a helper that finished ran alone.
pub(crate) fn adopt(device: &wgpu::Device, cache: wgpu::PipelineCache) {
    let Some(store) = for_device(device) else {
        return;
    };
    *store.cache.lock().unwrap_or_else(|e| e.into_inner()) = cache;
    // Written even if its length is the one last written.
    store.state.lock().unwrap_or_else(|e| e.into_inner()).1 = 0;
    store.save_soon();
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
    /// The wgpu cache to create pipelines through now.
    pub(crate) fn current(&self) -> wgpu::PipelineCache {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
        let Some(data) = self.current().get_data() else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if data.len() == state.1 {
            return;
        }
        match write(&self.path, &data) {
            Ok(()) => state.1 = data.len(),
            Err(error) => log::warn!("Pipeline cache not written: {error}"),
        }
    }
}

/// Replaces the file atomically: a reader sees the old file or the new one,
/// never a torn one.
fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    rawtonic_fs::atomic_write(path, encode(data)).map_err(|e| format!("{e:#}"))
}

/// `wgpu-pipelines-<backend/vendor/device>-<driver and build hash>.bin`, or
/// `None` where wgpu keeps no cache data for the adapter.
fn file_name(info: &wgpu::AdapterInfo, build: &str) -> Option<String> {
    let adapter = wgpu::util::pipeline_cache_key(info)?;
    let adapter = adapter.trim_start_matches("wgpu_pipeline_cache_");
    let mut hash = Fnv::default();
    for part in [
        info.name.as_str(),
        info.driver.as_str(),
        info.driver_info.as_str(),
        env!("CARGO_PKG_VERSION"),
        build,
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
            transient_saves_memory: Some(false),
            limit_bucket: None,
        };
        let first = file_name(&info("617.42"), "0.1.0").unwrap();
        assert!(first.starts_with("wgpu-pipelines-vulkan_"), "{first}");
        assert_ne!(first, file_name(&info("617.57"), "0.1.0").unwrap());
        // An app update starts a new file.
        assert_ne!(first, file_name(&info("617.42"), "0.1.1").unwrap());
        assert_eq!(first, file_name(&info("617.42"), "0.1.0").unwrap());
        // No cache data on Metal.
        let metal = wgpu::AdapterInfo {
            backend: wgpu::Backend::Metal,
            ..info("x")
        };
        assert_eq!(file_name(&metal, "0.1.0"), None);
    }

    #[test]
    fn a_written_file_reads_back_and_a_damaged_one_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wgpu-pipelines-a.bin");
        // A damaged file (torn by a crash, or foreign) is not used...
        std::fs::write(&path, b"RTPLC001 torn").unwrap();
        assert_eq!(read(&path), None);
        // ...and the next write replaces it whole, leaving no temporary file.
        write(&path, b"pipelines").unwrap();
        assert_eq!(read(&path), Some(b"pipelines".to_vec()));
        write(&path, b"more pipelines").unwrap();
        assert_eq!(read(&path), Some(b"more pipelines".to_vec()));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["wgpu-pipelines-a.bin"]);
        assert_eq!(read(&dir.path().join("missing.bin")), None);
    }

    /// What an idle compile helper starts from (`seed`) is the app's data,
    /// read back whole by the helper's `open`; data the helper cannot use is
    /// not passed (G76/HCF).
    #[test]
    fn a_helper_starts_from_the_app_cache_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wgpu-pipelines-a.bin");
        assert!(seed_file(&path, Some(b"app pipelines")));
        assert_eq!(read(&path), Some(b"app pipelines".to_vec()));
        // A cache without data yet: the helper starts empty, which is the
        // app's cache all the same.
        let empty = dir.path().join("wgpu-pipelines-empty.bin");
        assert!(seed_file(&empty, Some(&[])));
        assert!(!empty.exists());
        // No data from the backend, or more than a helper starts from.
        let none = dir.path().join("wgpu-pipelines-none.bin");
        assert!(!seed_file(&none, None));
        assert!(!seed_file(&none, Some(&vec![7; MAX_SEED + 1])));
        assert!(!none.exists());
        // Into a folder that is gone: not seeded, no panic.
        assert!(!seed_file(
            &dir.path().join("gone").join("wgpu-pipelines-a.bin"),
            Some(b"x")
        ));
    }

    /// A file of another format version (an older or newer build's) or of
    /// another program is ignored, never handed to the driver.
    #[test]
    fn a_file_of_another_version_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wgpu-pipelines-a.bin");
        let mut other = encode(b"pipelines");
        other[..8].copy_from_slice(b"RTPLC000");
        std::fs::write(&path, &other).unwrap();
        assert_eq!(read(&path), None);
        other[..8].copy_from_slice(b"RTPLC002");
        std::fs::write(&path, &other).unwrap();
        assert_eq!(read(&path), None);
        std::fs::write(&path, encode(b"pipelines")).unwrap();
        assert_eq!(read(&path), Some(b"pipelines".to_vec()));
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
