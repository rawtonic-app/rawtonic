// SPDX-License-Identifier: AGPL-3.0-or-later
//! ONNX Runtime sessions for Rawtonic's AI models, on a GPU when one is
//! usable and on the CPU otherwise (docs/decisions/0011-gpu-ai.md).
//!
//! GPU execution providers are ONNX Runtime *plugin libraries* registered
//! at run time beside the runtime itself: WebGPU (Dawn: Direct3D 12 on
//! Windows, Vulkan on Linux, any GPU vendor) and, when its plugin and the
//! user's own CUDA libraries are installed, NVIDIA's CUDA. A model is loaded
//! on the first provider that accepts it and whose output matched the CPU's
//! on that device (checked once per model and device, `check`); if a run on
//! the GPU later fails or returns non-finite values (`ensure_finite`), the
//! model is loaded again on the CPU and the run repeated, so a broken driver
//! costs time, not the tool. The CPU is always last.
//!
//! The app (or the CLI) calls `configure` with the user's preference and the
//! folders that may hold plugin libraries; `RAWTONIC_AI_PROVIDER`
//! (`cpu`, `webgpu`, `cuda`) overrides the choice for tests and benchmarks.
//!
//! The runtime library itself is loaded by `ensure_runtime`, which every
//! model load calls first: a missing or broken library is an error, never a
//! panic (`runtime`).

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use ort::AsPointer;
use ort::session::Session;
use ort::session::builder::SessionBuilder;

pub mod bench;
mod check;
mod runtime;

pub use runtime::{
    RUNTIME_UNAVAILABLE, RuntimeError, RuntimeErrorKind, ensure_runtime, library_path,
};

/// Where a model runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// NVIDIA CUDA (ONNX Runtime's CUDA plugin; needs the user's CUDA and cuDNN).
    Cuda,
    /// WebGPU (ONNX Runtime's WebGPU plugin on Dawn: Direct3D 12 or Vulkan).
    WebGpu,
    Cpu,
}

/// GPU providers in the order they are tried.
pub const GPU_PROVIDERS: [Provider; 2] = [Provider::Cuda, Provider::WebGpu];

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Cuda => "CUDA",
            Provider::WebGpu => "WebGPU",
            Provider::Cpu => "CPU",
        }
    }

    /// `cpu`, `webgpu` or `cuda` (any case).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(Provider::Cpu),
            "webgpu" => Some(Provider::WebGpu),
            "cuda" => Some(Provider::Cuda),
            _ => None,
        }
    }

    /// ONNX Runtime's name of the execution provider.
    pub fn ep_name(self) -> &'static str {
        match self {
            Provider::Cuda => "CUDAExecutionProvider",
            Provider::WebGpu => "WebGpuExecutionProvider",
            Provider::Cpu => "CPUExecutionProvider",
        }
    }

    /// File name of the plugin library on this platform (`None` for the CPU).
    pub fn library_file(self) -> Option<&'static str> {
        let stem = match self {
            Provider::Cuda => "onnxruntime_providers_cuda",
            Provider::WebGpu => "onnxruntime_providers_webgpu",
            Provider::Cpu => return None,
        };
        Some(match (stem, std::env::consts::OS) {
            ("onnxruntime_providers_cuda", "windows") => "onnxruntime_providers_cuda.dll",
            ("onnxruntime_providers_cuda", "macos") => "libonnxruntime_providers_cuda.dylib",
            ("onnxruntime_providers_cuda", _) => "libonnxruntime_providers_cuda.so",
            (_, "windows") => "onnxruntime_providers_webgpu.dll",
            (_, "macos") => "libonnxruntime_providers_webgpu.dylib",
            _ => "libonnxruntime_providers_webgpu.so",
        })
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The models, for logging and for the measured per-model exceptions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelKind {
    /// BiRefNet (Subject and Background masks).
    Subject,
    /// SAM 2.1 image encoder (object masks, Quick Erase).
    SamEncoder,
    /// SAM 2.1 prompt decoder.
    SamDecoder,
    /// UperNet + ConvNeXt-T sky segmentation.
    Sky,
    /// Depth Anything V2 Small.
    Depth,
    /// Camera-aware relative geometry for Relight (DA3 Base).
    Relight,
    /// Surface normals of people for Relight (DAViD Base).
    PeopleNormals,
    /// SigLIP 2 image encoder (automatic tags).
    TagVision,
    /// SigLIP 2 text encoder (the user's own tags).
    TagText,
    /// MI-GAN object removal.
    Inpaint,
    /// Nonlocal MatchFilter (AI Denoise).
    Denoise,
    /// YuNet face detection.
    FaceDetector,
    /// MediaPipe Face Mesh V2.
    FaceMesh,
    /// MediaPipe multiclass selfie segmentation.
    FaceParser,
}

impl ModelKind {
    pub const ALL: [ModelKind; 14] = [
        ModelKind::Subject,
        ModelKind::SamEncoder,
        ModelKind::SamDecoder,
        ModelKind::Sky,
        ModelKind::Depth,
        ModelKind::Relight,
        ModelKind::PeopleNormals,
        ModelKind::TagVision,
        ModelKind::TagText,
        ModelKind::Inpaint,
        ModelKind::Denoise,
        ModelKind::FaceDetector,
        ModelKind::FaceMesh,
        ModelKind::FaceParser,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ModelKind::Subject => "subject",
            ModelKind::SamEncoder => "sam-encoder",
            ModelKind::SamDecoder => "sam-decoder",
            ModelKind::Sky => "sky",
            ModelKind::Depth => "depth",
            ModelKind::Relight => "relight",
            ModelKind::PeopleNormals => "people-normals",
            ModelKind::TagVision => "tags-vision",
            ModelKind::TagText => "tags-text",
            ModelKind::Inpaint => "inpaint",
            ModelKind::Denoise => "denoise",
            ModelKind::FaceDetector => "face-detector",
            ModelKind::FaceMesh => "face-mesh",
            ModelKind::FaceParser => "face-parser",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == text.trim())
    }

    /// The model a file of the model pack holds, by its name.
    pub fn from_file_name(name: &str) -> Option<Self> {
        const PREFIXES: [(&str, ModelKind); 14] = [
            ("birefnet", ModelKind::Subject),
            ("sam2.1_tiny_encoder", ModelKind::SamEncoder),
            ("sam2.1_tiny_decoder", ModelKind::SamDecoder),
            ("skyseg", ModelKind::Sky),
            ("depth_anything", ModelKind::Depth),
            ("relight_geometry", ModelKind::Relight),
            ("relight_people_normals", ModelKind::PeopleNormals),
            ("siglip2_vision", ModelKind::TagVision),
            ("siglip2_text", ModelKind::TagText),
            ("migan", ModelKind::Inpaint),
            ("nlmf", ModelKind::Denoise),
            ("face_detection", ModelKind::FaceDetector),
            ("face_mesh", ModelKind::FaceMesh),
            ("selfie_multiclass", ModelKind::FaceParser),
        ];
        PREFIXES
            .into_iter()
            .find(|(prefix, _)| name.starts_with(prefix) && name.ends_with(".onnx"))
            .map(|(_, kind)| kind)
    }

    /// Why this model stays on the CPU with `provider`, if it does. Each
    /// entry is a measurement (docs/decisions/0011-gpu-ai.md): models too
    /// small to gain from a GPU, which run many times per photo. Wrong
    /// output on a device is caught by the check (`check`), not here.
    pub fn gpu_exception(self, provider: Provider) -> Option<&'static str> {
        match (self, provider) {
            (_, Provider::Cpu) => None,
            #[cfg(target_os = "linux")]
            (ModelKind::FaceParser, Provider::WebGpu) => Some(
                "WebGPU face parser crashes inside the provider in the Linux GTK app; use CPU until the provider is fixed",
            ),
            (ModelKind::FaceDetector | ModelKind::FaceMesh, _) => Some(
                "a few milliseconds on the CPU; the GPU's dispatch and upload cost more (WebGPU on Metal: 7-8 ms against 3-5 ms)",
            ),
            (ModelKind::SamDecoder, _) => Some(
                "runs twice per click and in tens of milliseconds on the CPU; kept there for the click's latency",
            ),
            _ => None,
        }
    }

    /// Options for `provider` when it runs this model, each a measurement.
    pub fn provider_options(self, provider: Provider) -> &'static [(&'static str, &'static str)] {
        match (self, provider) {
            // BiRefNet at 1024 × 1024 with WebGPU's default bucket cache of
            // freed buffers ran out of memory on a 10 GB RTX 3080 (Vulkan:
            // VK_ERROR_OUT_OF_DEVICE_MEMORY, then an invalid buffer); without
            // the cache it runs in 0.73 s (CPU 9.5 s), mask IoU 1.0 to the
            // CPU. Other models keep the cache: without it MI-GAN took 187
            // instead of 59 ms and the SAM encoder 147 instead of 71 ms.
            (ModelKind::Subject, Provider::WebGpu) => &[("storageBufferCacheMode", "disabled")],
            _ => &[],
        }
    }
}

/// The user's choice and where plugin libraries may be.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Use a GPU provider when one works (the preference; on by default).
    pub gpu: bool,
    /// Folders searched for plugin libraries, in order.
    pub plugin_dirs: Vec<PathBuf>,
    /// Where the GPU checks' verdicts are kept (`None`: for this process).
    pub checks_file: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gpu: true,
            plugin_dirs: default_plugin_dirs(),
            checks_file: None,
        }
    }
}

/// The folder of the ONNX Runtime library (`ORT_DYLIB_PATH`), where the
/// app bundles the plugins, and `RAWTONIC_ORT_PLUGIN_DIR`.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("RAWTONIC_ORT_PLUGIN_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(lib) = std::env::var_os("ORT_DYLIB_PATH")
        && let Some(dir) = Path::new(&lib).parent()
    {
        dirs.push(dir.to_path_buf());
    }
    dirs
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

/// Sets the preference and plugin folders for models loaded from now on
/// (models already loaded keep their provider).
pub fn configure(settings: Settings) {
    *SETTINGS.write().unwrap() = Some(settings);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap().clone().unwrap_or_default()
}

/// The providers to try for `kind`, best first, the CPU last.
pub fn candidates(kind: ModelKind, settings: &Settings, forced: Option<Provider>) -> Vec<Provider> {
    let mut list = Vec::new();
    match forced {
        Some(Provider::Cpu) => {}
        // A forced provider ignores the preference and the exceptions, so
        // they can be measured.
        Some(provider) => list.push(provider),
        None if settings.gpu => list.extend(
            GPU_PROVIDERS
                .into_iter()
                .filter(|p| kind.gpu_exception(*p).is_none()),
        ),
        None => {}
    }
    list.push(Provider::Cpu);
    list
}

/// `RAWTONIC_AI_PROVIDER`, if set to a provider's name.
pub fn forced_provider() -> Option<Provider> {
    std::env::var("RAWTONIC_AI_PROVIDER")
        .ok()
        .and_then(|v| Provider::parse(&v))
}

/// A GPU device a plugin offers.
#[derive(Clone, Debug)]
pub struct Device {
    pub provider: Provider,
    /// Vendor and device as ONNX Runtime reports them.
    pub label: String,
    /// What ONNX Runtime reports about the device and its provider (`k=v`,
    /// sorted; e.g. the plugin's version and the PCI slot; on Linux with
    /// the WebGPU plugin 0.4.0 no driver version).
    pub identity: String,
    pub library: PathBuf,
    ep_device: EpDevice,
    environment: Arc<ort::environment::Environment>,
}

#[derive(Clone, Copy, Debug)]
struct EpDevice(*const ort::sys::OrtEpDevice);
// Device retains its environment, so the read-only pointer remains valid.
unsafe impl Send for EpDevice {}
unsafe impl Sync for EpDevice {}

enum Registration {
    Ready(Weak<Device>),
    /// Registered or tried without a usable device; not tried again.
    Unusable(String),
}

static REGISTRY: Mutex<Vec<(Provider, Registration)>> = Mutex::new(Vec::new());

/// The device of `provider`, registering its plugin library on first use.
/// `Err` says why it is not usable (no library, no device, a failed
/// registration).
pub fn device(provider: Provider, dirs: &[PathBuf]) -> Result<Arc<Device>> {
    let file = provider
        .library_file()
        .ok_or_else(|| anyhow!("the CPU needs no plugin"))?;
    let mut registry = REGISTRY.lock().unwrap();
    if let Some(index) = registry.iter().position(|(p, _)| *p == provider) {
        match &registry[index].1 {
            Registration::Ready(device) => {
                if let Some(device) = device.upgrade() {
                    return Ok(device);
                }
            }
            Registration::Unusable(why) => return Err(anyhow!("{why}")),
        }
        registry.remove(index);
    }
    // Not registered yet: a missing library may still be installed later.
    let Some(library) = dirs.iter().map(|d| d.join(file)).find(|p| p.is_file()) else {
        return Err(anyhow!("{file} is not installed"));
    };
    let registration = match register(provider, &library) {
        Ok(device) => {
            log::info!(
                "AI: {} provider registered ({}), device {}",
                provider,
                library.display(),
                device.label
            );
            let device = Arc::new(device);
            registry.push((provider, Registration::Ready(Arc::downgrade(&device))));
            return Ok(device);
        }
        Err(e) => {
            log::warn!(
                "AI: {} provider not usable ({}): {e:#}",
                provider,
                library.display()
            );
            Registration::Unusable(format!("{e:#}"))
        }
    };
    let result = match &registration {
        Registration::Ready(_) => unreachable!(),
        Registration::Unusable(why) => Err(anyhow!("{why}")),
    };
    registry.push((provider, registration));
    result
}

fn register(provider: Provider, library: &Path) -> Result<Device> {
    ensure_runtime()?;
    let api = ort::api();
    let env = ort::environment::Environment::current()
        .context("creating the ONNX Runtime environment")?;
    let name = CString::new(provider.ep_name())?;
    let path = ort_path(library);
    unsafe {
        let registered = check((api.RegisterExecutionProviderLibrary)(
            env.ptr().cast_mut(),
            name.as_ptr(),
            path.as_ptr(),
        ));
        // Newer runtimes register plugins found beside themselves when the
        // environment is created; then the devices are there already.
        if let Err(e) = registered
            && !e.to_string().contains("already registered")
        {
            return Err(e.context("registering the plugin library"));
        }
        let mut devices: *const *const ort::sys::OrtEpDevice = std::ptr::null();
        let mut count = 0usize;
        check((api.GetEpDevices)(env.ptr(), &mut devices, &mut count))
            .context("listing devices")?;
        let all = if devices.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(devices, count)
        };
        for &device in all {
            if c_str((api.EpDevice_EpName)(device)) != provider.ep_name() {
                continue;
            }
            let hardware = (api.EpDevice_Device)(device);
            let label = if hardware.is_null() {
                "unknown device".to_string()
            } else {
                let kind = match (api.HardwareDevice_Type)(hardware) {
                    ort::sys::OrtHardwareDeviceType::OrtHardwareDeviceType_GPU => "GPU",
                    ort::sys::OrtHardwareDeviceType::OrtHardwareDeviceType_CPU => "CPU",
                    ort::sys::OrtHardwareDeviceType::OrtHardwareDeviceType_NPU => "NPU",
                };
                let description = key_value((api.HardwareDevice_Metadata)(hardware), "Description");
                format!(
                    "{} {kind} {:04x}:{:04x}{}",
                    c_str((api.HardwareDevice_Vendor)(hardware)),
                    (api.HardwareDevice_VendorId)(hardware),
                    (api.HardwareDevice_DeviceId)(hardware),
                    description.map(|d| format!(" ({d})")).unwrap_or_default()
                )
            };
            let mut identity = key_values((api.EpDevice_EpMetadata)(device));
            if !hardware.is_null() {
                identity.extend(key_values((api.HardwareDevice_Metadata)(hardware)));
            }
            // Not where the plugin lies: an AppImage mounts it at a new path
            // each launch (the check key has the plugin's name and size).
            identity.retain(|pair| !pair.starts_with("library_path="));
            identity.sort();
            return Ok(Device {
                provider,
                label,
                identity: identity.join(" "),
                library: library.to_path_buf(),
                ep_device: EpDevice(device),
                environment: env,
            });
        }
    }
    Err(anyhow!("the plugin found no device"))
}

/// Adds `device`'s provider, with `kind`'s options for it, to a session's
/// options.
fn append(builder: &mut SessionBuilder, device: &Device, kind: ModelKind) -> Result<()> {
    let api = ort::api();
    let env = &device.environment;
    let devices = [device.ep_device.0];
    let options = kind.provider_options(device.provider);
    let text = |s: &str| CString::new(s).map_err(|e| anyhow!("provider option: {e}"));
    let keys = options
        .iter()
        .map(|(k, _)| text(k))
        .collect::<Result<Vec<_>>>()?;
    let values = options
        .iter()
        .map(|(_, v)| text(v))
        .collect::<Result<Vec<_>>>()?;
    let key_ptrs: Vec<*const c_char> = keys.iter().map(|k| k.as_ptr()).collect();
    let value_ptrs: Vec<*const c_char> = values.iter().map(|v| v.as_ptr()).collect();
    unsafe {
        check((api.SessionOptionsAppendExecutionProvider_V2)(
            builder.ptr_mut(),
            env.ptr().cast_mut(),
            devices.as_ptr(),
            devices.len(),
            key_ptrs.as_ptr(),
            value_ptrs.as_ptr(),
            key_ptrs.len(),
        ))
    }
}

/// Turns an `OrtStatus` into a `Result`, releasing it.
unsafe fn check(status: ort::sys::OrtStatusPtr) -> Result<()> {
    if status.0.is_null() {
        return Ok(());
    }
    let api = ort::api();
    let message = unsafe { c_str((api.GetErrorMessage)(status.0)) };
    unsafe { (api.ReleaseStatus)(status.0) };
    Err(anyhow!("{message}"))
}

unsafe fn c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

unsafe fn key_value(pairs: *const ort::sys::OrtKeyValuePairs, key: &str) -> Option<String> {
    if pairs.is_null() {
        return None;
    }
    let key = CString::new(key).ok()?;
    let value = unsafe { (ort::api().GetKeyValue)(pairs, key.as_ptr()) };
    (!value.is_null()).then(|| unsafe { c_str(value) })
}

/// All pairs of `pairs` as `key=value`.
unsafe fn key_values(pairs: *const ort::sys::OrtKeyValuePairs) -> Vec<String> {
    if pairs.is_null() {
        return Vec::new();
    }
    let mut keys: *const *const c_char = std::ptr::null();
    let mut values: *const *const c_char = std::ptr::null();
    let mut count = 0usize;
    unsafe {
        (ort::api().GetKeyValuePairs)(pairs, &mut keys, &mut values, &mut count);
        if keys.is_null() || values.is_null() {
            return Vec::new();
        }
        (0..count)
            .map(|i| format!("{}={}", c_str(*keys.add(i)), c_str(*values.add(i))))
            .collect()
    }
}

#[cfg(windows)]
fn ort_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(not(windows))]
fn ort_path(path: &Path) -> Vec<c_char> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|&b| b as c_char)
        .chain(Some(0))
        .collect()
}

/// Creates a session for `path` (a `kind` model) on `device` (`None` = the
/// CPU).
pub(crate) fn session(path: &Path, kind: ModelKind, device: Option<&Device>) -> Result<Session> {
    let mut builder = Session::builder()?;
    if let Some(device) = device {
        append(&mut builder, device, kind)?;
    } else {
        // Explicit CPU registration bypasses ort's default NPU discovery.
        // Runtime 1.22 has no discoverable CPU device on macOS.
        unsafe {
            let library = libloading::Library::new(runtime::library_path())?;
            let append_cpu: libloading::Symbol<
                unsafe extern "system" fn(
                    *mut ort::sys::OrtSessionOptions,
                    i32,
                ) -> ort::sys::OrtStatusPtr,
            > = library.get(b"OrtSessionOptionsAppendExecutionProvider_CPU\0")?;
            check(append_cpu(builder.ptr_mut(), 1))?;
        }
    }
    Ok(builder.commit_from_file(path)?)
}

/// An error if any value is NaN or infinite: a GPU kernel that went wrong
/// (for example an fp16 overflow) makes `Model::run` repeat on the CPU.
pub fn ensure_finite<'a>(values: impl IntoIterator<Item = &'a f32>) -> Result<()> {
    if values.into_iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(anyhow!("the output has non-finite values"))
    }
}

/// True for ONNX Runtime's error after a run was stopped on purpose
/// (`RunOptions::terminate`): not a reason to leave the GPU.
fn is_termination(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("terminate flag")
}

struct Loaded {
    session: Session,
    provider: Provider,
    runs: u32,
}

/// A loaded model: its session and the provider it runs on.
pub struct Model {
    kind: ModelKind,
    path: PathBuf,
    state: Mutex<Loaded>,
}

impl Model {
    /// Loads `path` on the first provider of `candidates(kind, …)` that
    /// accepts it.
    pub fn load(path: &Path, kind: ModelKind) -> Result<Self> {
        let settings = settings();
        Self::load_on(
            path,
            kind,
            &candidates(kind, &settings, forced_provider()),
            &settings.plugin_dirs,
        )
    }

    /// `load` for a file of the model pack, the model known by its name.
    pub fn load_file(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let kind = ModelKind::from_file_name(name)
            .ok_or_else(|| anyhow!("{} is not a file of the model pack", path.display()))?;
        Self::load(path, kind)
    }

    /// Loads `path` on the first of `providers` that works and, for a GPU,
    /// whose output matched the CPU's (`check`; the CPU only if listed).
    pub fn load_on(
        path: &Path,
        kind: ModelKind,
        providers: &[Provider],
        dirs: &[PathBuf],
    ) -> Result<Self> {
        Self::load_checked(path, kind, providers, dirs, true)
    }

    /// `load_on`, with the GPU check only when `checked` (benchmarks report
    /// the GPU's output as it is).
    pub(crate) fn load_checked(
        path: &Path,
        kind: ModelKind,
        providers: &[Provider],
        dirs: &[PathBuf],
        checked: bool,
    ) -> Result<Self> {
        // ONNX Runtime missing or broken: an error saying so, not a panic.
        ensure_runtime()?;
        let checks_file = settings().checks_file;
        let mut last_error = anyhow!("no provider to try");
        for &provider in providers {
            let started = Instant::now();
            let attempt = if provider == Provider::Cpu {
                session(path, kind, None).map(|s| (s, "CPU".to_string()))
            } else {
                device(provider, dirs).and_then(|device| {
                    let mut gpu = session(path, kind, Some(&device))?;
                    if checked {
                        check::check(path, kind, &mut gpu, &device, checks_file.as_ref())?;
                    }
                    Ok((gpu, device.label.clone()))
                })
            };
            match attempt {
                Ok((session, label)) => {
                    log::info!(
                        "AI {}: loaded on {provider} ({label}) in {} ms",
                        kind.name(),
                        started.elapsed().as_millis()
                    );
                    return Ok(Self {
                        kind,
                        path: path.to_path_buf(),
                        state: Mutex::new(Loaded {
                            session,
                            provider,
                            runs: 0,
                        }),
                    });
                }
                Err(e) => {
                    if provider != Provider::Cpu {
                        log::info!("AI {}: not on {provider}: {e:#}", kind.name());
                    }
                    last_error = e.context(format!("loading {} on {provider}", path.display()));
                }
            }
        }
        Err(last_error)
    }

    pub fn kind(&self) -> ModelKind {
        self.kind
    }

    /// The provider the model runs on now.
    pub fn provider(&self) -> Provider {
        self.state.lock().unwrap().provider
    }

    /// Runs `f` on the session where it is, never moving to the CPU (for
    /// benchmarks and tests).
    pub fn run_without_fallback<T>(&self, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        f(&mut self.state.lock().unwrap().session)
    }

    /// Runs `f` on the session. When it fails on a GPU (other than being
    /// stopped on purpose), the model moves to the CPU for good and `f`
    /// runs again there; `f` should return an error for unusable output
    /// (`ensure_finite`).
    pub fn run<T>(&self, mut f: impl FnMut(&mut Session) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().unwrap();
        let started = Instant::now();
        let result = f(&mut state.session);
        state.runs += 1;
        let provider = state.provider;
        match result {
            Ok(value) => {
                if state.runs == 1 {
                    log::info!(
                        "AI {}: first run on {provider} in {} ms",
                        self.kind.name(),
                        started.elapsed().as_millis()
                    );
                }
                Ok(value)
            }
            Err(e) if provider != Provider::Cpu && !is_termination(&e) => {
                log::warn!(
                    "AI {}: run on {provider} failed ({e:#}); using the CPU from now on",
                    self.kind.name()
                );
                state.session = session(&self.path, self.kind, None)?;
                state.provider = Provider::Cpu;
                state.runs = 0;
                let started = Instant::now();
                let value = f(&mut state.session)?;
                state.runs = 1;
                log::info!(
                    "AI {}: first run on CPU in {} ms",
                    self.kind.name(),
                    started.elapsed().as_millis()
                );
                Ok(value)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(gpu: bool) -> Settings {
        Settings {
            gpu,
            plugin_dirs: Vec::new(),
            checks_file: None,
        }
    }

    #[test]
    fn the_cpu_is_always_last() {
        for kind in ModelKind::ALL {
            for gpu in [false, true] {
                for forced in [
                    None,
                    Some(Provider::Cpu),
                    Some(Provider::WebGpu),
                    Some(Provider::Cuda),
                ] {
                    let list = candidates(kind, &settings(gpu), forced);
                    assert_eq!(
                        list.last(),
                        Some(&Provider::Cpu),
                        "{kind:?} {gpu} {forced:?}"
                    );
                    assert_eq!(list.iter().filter(|p| **p == Provider::Cpu).count(), 1);
                }
            }
        }
    }

    #[test]
    fn the_preference_turns_the_gpu_off() {
        assert_eq!(
            candidates(ModelKind::Subject, &settings(false), None),
            vec![Provider::Cpu]
        );
        assert_eq!(
            candidates(ModelKind::Subject, &settings(true), None),
            vec![Provider::Cuda, Provider::WebGpu, Provider::Cpu]
        );
    }

    #[test]
    fn measured_exceptions_stay_on_the_cpu_unless_forced() {
        assert_eq!(
            candidates(ModelKind::FaceMesh, &settings(true), None),
            vec![Provider::Cpu]
        );
        assert_eq!(
            candidates(ModelKind::FaceMesh, &settings(true), Some(Provider::WebGpu)),
            vec![Provider::WebGpu, Provider::Cpu]
        );
        assert_eq!(
            candidates(ModelKind::Subject, &settings(false), Some(Provider::Cpu)),
            vec![Provider::Cpu]
        );
    }

    #[test]
    fn only_subject_on_webgpu_runs_without_the_buffer_cache() {
        for kind in ModelKind::ALL {
            for provider in [Provider::Cpu, Provider::WebGpu, Provider::Cuda] {
                let options = kind.provider_options(provider);
                if (kind, provider) == (ModelKind::Subject, Provider::WebGpu) {
                    assert_eq!(options, &[("storageBufferCacheMode", "disabled")]);
                } else {
                    assert!(options.is_empty(), "{kind:?} {provider}");
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_face_parser_avoids_the_crashing_webgpu_provider() {
        assert_eq!(
            candidates(ModelKind::FaceParser, &settings(true), None),
            vec![Provider::Cuda, Provider::Cpu]
        );
        // Explicit benchmarks may still measure the provider.
        assert_eq!(
            candidates(
                ModelKind::FaceParser,
                &settings(true),
                Some(Provider::WebGpu)
            ),
            vec![Provider::WebGpu, Provider::Cpu]
        );
    }

    #[test]
    fn names_round_trip() {
        for kind in ModelKind::ALL {
            assert_eq!(ModelKind::parse(kind.name()), Some(kind));
        }
        for provider in [Provider::Cpu, Provider::WebGpu, Provider::Cuda] {
            assert_eq!(Provider::parse(provider.name()), Some(provider));
        }
        assert_eq!(Provider::parse("metal"), None);
    }

    #[test]
    fn a_missing_plugin_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join("rawtonic-onnx-no-plugins");
        let error = device(Provider::WebGpu, &[dir]).unwrap_err();
        assert!(error.to_string().contains("not installed"), "{error}");
    }

    #[test]
    fn non_finite_output_is_an_error() {
        assert!(ensure_finite(&[0.0, 1.0, -2.5]).is_ok());
        assert!(ensure_finite(&[0.0, f32::NAN]).is_err());
        assert!(ensure_finite(&[f32::INFINITY]).is_err());
    }

    #[test]
    fn only_a_stopped_run_counts_as_termination() {
        assert!(is_termination(&anyhow!(
            "Exiting due to terminate flag being set to true."
        )));
        assert!(!is_termination(&anyhow!("D3D12 device lost")));
    }
}
