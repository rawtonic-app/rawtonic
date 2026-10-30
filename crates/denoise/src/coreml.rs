// SPDX-License-Identifier: AGPL-3.0-or-later
//! Core ML models (macOS): a `.mlpackage` is compiled once into a
//! `.mlmodelc` beside it, which later loads use. `CoreMlModel` runs any
//! model with float inputs and one float output (MatchFilter here, the
//! app's BiRefNet subject, sky and Moebius removal models too); `CoreMl` is
//! MatchFilter's backend.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail, ensure};
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use crate::Backend;

const INPUT: &str = "raw_with_noise";
const OUTPUT: &str = "denoised_raw";

/// The processors Core ML may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeUnits {
    /// CPU, GPU and Neural Engine.
    All,
    CpuAndGpu,
    CpuAndNeuralEngine,
    CpuOnly,
}

impl ComputeUnits {
    fn ml(self) -> MLComputeUnits {
        match self {
            Self::All => MLComputeUnits::All,
            Self::CpuAndGpu => MLComputeUnits::CPUAndGPU,
            Self::CpuAndNeuralEngine => MLComputeUnits::CPUAndNeuralEngine,
            Self::CpuOnly => MLComputeUnits::CPUOnly,
        }
    }
}

struct Model(Retained<MLModel>);
// Predictions are serialised by the mutex below; the model is not touched
// from two threads at once.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

/// A loaded Core ML model with float inputs and one float output.
pub struct CoreMlModel {
    model: Mutex<Model>,
    /// Set by the first run that succeeds (`with_first_run_marker`).
    first_run: Option<FirstRunMarker>,
    marked: std::sync::atomic::AtomicBool,
}

/// Records that a model has run once in this app on this macOS build.
///
/// A model's first run in an app identifier Core ML has not run it in
/// before is slow: Metal compiles the model's GPU kernels and keeps them in
/// the app's Metal cache (per identifier and OS build), which later launches
/// reuse. BiRefNet fp16 on an M5 Pro: first run 11-14 s on a fresh
/// identifier against 1.1 s in the next launch; the sky model 6.8 s against
/// 0.2 s (notes: reference/subject-first-use.md). The marker is an empty
/// file named after the model and the OS build in the app's cache folder,
/// so an app that never ran a model can build its kernels ahead
/// (`is_set` false), and a macOS update, which brings a new Metal compiler,
/// asks again.
#[derive(Clone, Debug)]
pub struct FirstRunMarker(PathBuf);

impl FirstRunMarker {
    /// The marker for model `key` (e.g. its name and a hash of its files) in
    /// `dir`; `None` when the OS build is unknown.
    pub fn new(dir: &Path, key: &str) -> Option<Self> {
        Self::for_build(dir, key, &os_build()?)
    }

    fn for_build(dir: &Path, key: &str, build: &str) -> Option<Self> {
        Some(Self(dir.join(format!("{key}-{build}"))))
    }

    /// Whether the model has run once here.
    pub fn is_set(&self) -> bool {
        self.0.is_file()
    }

    /// Notes the first run (an empty file, put in place by a rename).
    pub fn set(&self) -> Result<()> {
        let dir = self.0.parent().context("marker without a folder")?;
        std::fs::create_dir_all(dir)?;
        let file = tempfile::NamedTempFile::new_in(dir)?;
        file.persist(&self.0)?;
        Ok(())
    }
}

/// The compiled model beside the package (`x.mlpackage` → `x.mlmodelc`).
pub fn compiled_path(package: &Path) -> PathBuf {
    package.with_extension("mlmodelc")
}

impl CoreMlModel {
    /// Loads `package`, compiling it first unless its `.mlmodelc` exists.
    pub fn load(package: &Path, units: ComputeUnits) -> Result<Self> {
        Self::load_with(package, units, false)
    }

    /// `load`; `low_precision_accumulation` lets the GPU sum in float16
    /// (`MLModelConfiguration.allowLowPrecisionAccumulationOnGPU`).
    pub fn load_with(
        package: &Path,
        units: ComputeUnits,
        low_precision_accumulation: bool,
    ) -> Result<Self> {
        let compiled = compiled_path(package);
        if !is_compiled(&compiled) {
            compile(package, &compiled)?;
        }
        let url = file_url(&compiled)?;
        let model = autoreleasepool(|_| unsafe {
            let config = MLModelConfiguration::new();
            config.setComputeUnits(units.ml());
            config.setAllowLowPrecisionAccumulationOnGPU(low_precision_accumulation);
            MLModel::modelWithContentsOfURL_configuration_error(&url, &config)
        })
        .map_err(|e| anyhow!("Core ML could not load {}: {e:?}", compiled.display()))?;
        Ok(Self {
            model: Mutex::new(Model(model)),
            first_run: None,
            marked: Default::default(),
        })
    }

    /// Sets `marker` after the first run that succeeds.
    pub fn with_first_run_marker(mut self, marker: Option<FirstRunMarker>) -> Self {
        self.first_run = marker;
        self
    }

    /// Runs the model once on a constant input of `shape` (Core ML builds
    /// its GPU programs on the first run; see `FirstRunMarker`).
    pub fn warm_up(&self, input: &str, shape: &[usize], output: &str) -> Result<()> {
        let data = vec![0.0; shape.iter().product()];
        self.predict(input, shape, &data, output).map(|_| ())
    }

    /// The shape of float input `name` as the model declares it.
    pub fn input_shape(&self, name: &str) -> Result<Vec<usize>> {
        let model = self.model.lock().unwrap();
        autoreleasepool(|_| unsafe {
            let inputs = model.0.modelDescription().inputDescriptionsByName();
            let description = inputs
                .objectForKey(&NSString::from_str(name))
                .with_context(|| format!("no input {name}"))?;
            let constraint = description
                .multiArrayConstraint()
                .with_context(|| format!("input {name} is not an array"))?;
            Ok(constraint
                .shape()
                .iter()
                .map(|n| n.as_isize() as usize)
                .collect())
        })
    }

    /// Runs the model on `data` (`input` of `shape`, float32, first-major)
    /// and returns `output`'s shape and values (first-major).
    pub fn predict(
        &self,
        input: &str,
        shape: &[usize],
        data: &[f32],
        output: &str,
    ) -> Result<(Vec<usize>, Vec<f32>)> {
        self.predict_inputs(&[(input, shape, data)], output)
    }

    /// `predict` for a model with several float inputs (name, shape, data).
    pub fn predict_inputs(
        &self,
        inputs: &[(&str, &[usize], &[f32])],
        output: &str,
    ) -> Result<(Vec<usize>, Vec<f32>)> {
        for &(name, shape, data) in inputs {
            ensure!(
                data.len() == shape.iter().product::<usize>(),
                "input {name} of {} values for shape {shape:?}",
                data.len()
            );
        }
        let model = self.model.lock().unwrap();
        autoreleasepool(|_| unsafe {
            let mut keys = Vec::with_capacity(inputs.len());
            let mut values: Vec<Retained<AnyObject>> = Vec::with_capacity(inputs.len());
            for &(name, shape, data) in inputs {
                let array = MLMultiArray::initWithShape_dataType_error(
                    MLMultiArray::alloc(),
                    &ns_shape(shape),
                    MLMultiArrayDataType::Float32,
                )
                .map_err(|e| anyhow!("MLMultiArray: {e:?}"))?;
                #[allow(deprecated)]
                let ptr = array.dataPointer().as_ptr() as *mut f32;
                // initWithShape:dataType: lays the scalars out first-major and contiguous.
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
                let value = MLFeatureValue::featureValueWithMultiArray(&array);
                keys.push(NSString::from_str(name));
                values.push(Retained::into_super(Retained::into_super(value)));
            }
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let dict = NSDictionary::<NSString, AnyObject>::from_retained_objects(&keys, &values);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                MLDictionaryFeatureProvider::alloc(),
                &dict,
            )
            .map_err(|e| anyhow!("feature provider: {e:?}"))?;
            let provider = ProtocolObject::<dyn MLFeatureProvider>::from_ref(&*provider);
            let result = model
                .0
                .predictionFromFeatures_error(provider)
                .map_err(|e| anyhow!("Core ML prediction failed: {e:?}"))?;
            let out = result
                .featureValueForName(&NSString::from_str(output))
                .and_then(|v| v.multiArrayValue())
                .with_context(|| format!("no {output} output"))?;
            read_array(&out)
        })
        .inspect(|_| self.note_first_run())
    }

    fn note_first_run(&self) {
        if let Some(marker) = &self.first_run
            && !self.marked.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            // Without the marker the next launch only builds ahead again.
            let _ = marker.set();
        }
    }
}

/// MatchFilter on Core ML; the tile edge is the package's input size.
pub struct CoreMl {
    model: CoreMlModel,
    tile: usize,
    tag: &'static str,
}

impl CoreMl {
    /// On all compute units (GPU / Neural Engine).
    pub fn load(package: &Path) -> Result<Self> {
        Self::load_tagged(package, ComputeUnits::All, "")
    }

    /// On `units`; `tag` keeps this package's results apart in the cache
    /// (`Backend::cache_tag`).
    pub fn load_tagged(package: &Path, units: ComputeUnits, tag: &'static str) -> Result<Self> {
        let model = CoreMlModel::load(package, units)?;
        let shape = model.input_shape(INPUT)?;
        ensure!(
            shape.len() == 4 && shape[..2] == [1, 8] && shape[2] == shape[3],
            "unexpected input shape {shape:?}"
        );
        Ok(Self {
            tile: shape[2],
            model,
            tag,
        })
    }
}

/// Reads the GPU program Core ML keeps for `package` in its device cache
/// into the page cache, on a thread of its own, so a load that follows soon
/// finds the weights in memory.
///
/// Core ML (E5 runtime) builds each model's GPU program once per app and OS
/// build and keeps it with a copy of the weights in
/// `~/Library/Caches/<app>/com.apple.e5rt.e5bundlecache/<OS build>/…`. A load
/// copies those weights out of a memory-mapped file page by page: with the
/// file in the page cache BiRefNet fp16 (464 MB) loads in 0.26 s, with its
/// pages dropped (an app launched a while after the last one) in 0.53-0.67 s
/// on an M5 Pro. Reading the file ahead with large reads takes a few tens of
/// milliseconds and runs while Core ML parses the program (~0.17 s), so the
/// load no longer waits for it (notes: reference/birefnet-fp16.md).
///
/// The cache's layout is Core ML's own: the files are found by their size
/// (the program's weights are the package's weights, within 1 %). Nothing is
/// read when the layout differs; the load works as before.
pub fn prefetch_device_cache(package: &Path) -> Option<std::thread::JoinHandle<(u64, f64)>> {
    let weights = std::fs::metadata(compiled_path(package).join("weights/weight.bin"))
        .or_else(|_| std::fs::metadata(package.join("Data/com.apple.CoreML/weights/weight.bin")))
        .ok()?
        .len();
    let dir = device_cache_dir()?;
    Some(std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut files = Vec::new();
        find_sized(&dir, weights, 8, &mut files);
        let bytes = files.iter().map(|f| read_through(f)).sum();
        (bytes, start.elapsed().as_secs_f64())
    }))
}

/// Core ML's device cache for this app and OS build.
fn device_cache_dir() -> Option<PathBuf> {
    let app = objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .file_name()?
                .to_str()
                .map(str::to_string)
        })?;
    let dir = PathBuf::from(std::env::var_os("HOME")?)
        .join("Library/Caches")
        .join(app)
        .join("com.apple.e5rt.e5bundlecache")
        .join(os_build()?);
    dir.is_dir().then_some(dir)
}

/// The OS build (`kern.osversion`, e.g. 25G83).
fn os_build() -> Option<String> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            old: *mut std::ffi::c_void,
            old_len: *mut usize,
            new: *mut std::ffi::c_void,
            new_len: usize,
        ) -> std::ffi::c_int;
    }
    let mut buffer = [0u8; 64];
    let mut len = buffer.len();
    let ok = unsafe {
        sysctlbyname(
            c"kern.osversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    let text = std::ffi::CStr::from_bytes_until_nul(&buffer[..len.min(buffer.len())]).ok()?;
    ok.then(|| text.to_str().ok().map(str::to_string))
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Files under `dir` (to `depth` levels) whose size is within 1 % of `len`.
fn find_sized(dir: &Path, len: u64, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth > 0 {
            find_sized(&entry.path(), len, depth - 1, out);
        } else if kind.is_file()
            && entry
                .metadata()
                .is_ok_and(|m| m.len().abs_diff(len) <= len / 100)
        {
            out.push(entry.path());
        }
    }
}

/// Reads a file with four threads in 8 MB pieces (into the page cache);
/// returns the bytes read.
fn read_through(path: &Path) -> u64 {
    use std::os::unix::fs::FileExt;
    const PIECE: u64 = 8 << 20;
    const THREADS: u64 = 4;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return 0;
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..THREADS)
            .map(|k| {
                let file = &file;
                scope.spawn(move || {
                    let mut buffer = vec![0u8; PIECE as usize];
                    let mut read = 0;
                    let mut offset = k * PIECE;
                    while offset < len {
                        read += file.read_at(&mut buffer, offset).unwrap_or(0) as u64;
                        offset += THREADS * PIECE;
                    }
                    read
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or(0)).sum()
    })
}

fn file_url(path: &Path) -> Result<Retained<NSURL>> {
    let s = path.to_str().context("model path is not UTF-8")?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(s)))
}

/// Compiles `package` into `compiled` (`publish::publish_dir`: the app and
/// the CLI may compile at once; the second uses the first one's result).
fn compile(package: &Path, compiled: &Path) -> Result<()> {
    ensure!(
        package.exists(),
        "model package {} missing",
        package.display()
    );
    crate::publish::publish_dir(compiled, is_compiled, |staging| {
        let url = file_url(package)?;
        #[allow(deprecated)]
        let out = autoreleasepool(|_| unsafe { MLModel::compileModelAtURL_error(&url) })
            .map_err(|e| anyhow!("Core ML could not compile {}: {e:?}", package.display()))?;
        let out_path = PathBuf::from(
            out.path()
                .context("compiled model without a path")?
                .to_string(),
        );
        if std::fs::rename(&out_path, staging).is_err() {
            copy_dir(&out_path, staging)?;
            let _ = std::fs::remove_dir_all(&out_path);
        }
        Ok(())
    })
}

/// Whether `compiled` holds a compiled model.
fn is_compiled(compiled: &Path) -> bool {
    compiled.join("coremldata.bin").exists()
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn ns_shape(dims: &[usize]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = dims
        .iter()
        .map(|&d| NSNumber::new_isize(d as isize))
        .collect();
    NSArray::from_retained_slice(&numbers)
}

impl Backend for CoreMl {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
        let (shape, out) =
            self.model
                .predict(INPUT, &[1, 8, self.tile, self.tile], input, OUTPUT)?;
        ensure!(
            shape == [1, 4, self.tile, self.tile],
            "output shape {shape:?}"
        );
        Ok(out)
    }

    fn tile(&self) -> usize {
        self.tile
    }

    fn cache_tag(&self) -> String {
        if self.tag.is_empty() {
            String::new()
        } else {
            format!("{}-{}", self.tag, self.tile)
        }
    }
}

/// An array's shape and values (first-major), honouring its strides.
unsafe fn read_array(array: &MLMultiArray) -> Result<(Vec<usize>, Vec<f32>)> {
    let shape: Vec<usize> = unsafe { array.shape() }
        .iter()
        .map(|n| n.as_isize() as usize)
        .collect();
    let strides: Vec<usize> = unsafe { array.strides() }
        .iter()
        .map(|n| n.as_isize() as usize)
        .collect();
    ensure!(
        shape.len() == strides.len(),
        "shape {shape:?}, strides {strides:?}"
    );
    let data_type = unsafe { array.dataType() };
    let half = if data_type == MLMultiArrayDataType::Float32 {
        false
    } else if data_type == MLMultiArrayDataType::Float16 {
        true
    } else {
        bail!("output data type {:?}", data_type.0);
    };
    #[allow(deprecated)]
    let base = unsafe { array.dataPointer() }.as_ptr();
    let count: usize = shape.iter().product();
    let contiguous =
        (0..shape.len()).all(|d| strides[d] == shape[d + 1..].iter().product::<usize>());
    if contiguous {
        let out = if half {
            let values = unsafe { std::slice::from_raw_parts(base as *const u16, count) };
            values
                .iter()
                .map(|&b| half::f16::from_bits(b).to_f32())
                .collect()
        } else {
            unsafe { std::slice::from_raw_parts(base as *const f32, count) }.to_vec()
        };
        return Ok((shape, out));
    }
    let mut out = Vec::with_capacity(count);
    let mut index = vec![0usize; shape.len()];
    for _ in 0..count {
        let offset: usize = index.iter().zip(&strides).map(|(i, s)| i * s).sum();
        out.push(if half {
            half::f16::from_bits(unsafe { *(base as *const u16).add(offset) }).to_f32()
        } else {
            unsafe { *(base as *const f32).add(offset) }
        });
        // Next index, last dimension fastest.
        for d in (0..shape.len()).rev() {
            index[d] += 1;
            if index[d] < shape[d] {
                break;
            }
            index[d] = 0;
        }
    }
    Ok((shape, out))
}

#[cfg(test)]
mod tests {
    use super::FirstRunMarker;

    #[test]
    fn first_run_marker_is_per_model_and_os_build() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("first-runs");
        let subject = FirstRunMarker::for_build(&cache, "subject-abc", "25G83").unwrap();
        let sky = FirstRunMarker::for_build(&cache, "sky-def", "25G83").unwrap();
        let updated = FirstRunMarker::for_build(&cache, "subject-abc", "26A434").unwrap();
        assert!(!subject.is_set());
        subject.set().unwrap();
        subject.set().unwrap();
        assert!(subject.is_set());
        assert!(!sky.is_set() && !updated.is_set());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    }

    #[test]
    fn the_running_os_build_is_known() {
        let dir = tempfile::tempdir().unwrap();
        assert!(FirstRunMarker::new(dir.path(), "x").is_some());
    }
}
