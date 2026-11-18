// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loading the ONNX Runtime library, as an error instead of a panic.
//!
//! `ort` (with `load-dynamic`) opens the library on first use and panics
//! when it cannot: a missing file, a file that is not a library or not ONNX
//! Runtime, a build for another architecture, a version older than `ort`
//! needs. A panic in an async command never answers the caller, so an AI
//! tool stayed busy for good. `ensure_runtime` opens and checks the library
//! itself first and lets `ort` load it only once that passed (any panic
//! left is caught), so every caller gets an `Err` that says why.
//!
//! A missing library is looked for again on every call (a stat), so one
//! installed later is used at once. A library that failed to load is not
//! tried again until its file changes (size or modification time), so
//! callers that ask repeatedly do not reload it in a loop. A library that
//! was opened but is unusable (not ONNX Runtime, too old) stays mapped:
//! unloading ONNX Runtime is not safe, and the system would hand the same
//! image back for that path, so replacing it needs a restart.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Start of every runtime error's message: the app's frontend recognises it
/// and shows its own translated text.
pub const RUNTIME_UNAVAILABLE: &str = "AI runtime unavailable";

/// Why the ONNX Runtime library cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    kind: RuntimeErrorKind,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    /// No library at the path (or none found by that name).
    Missing,
    /// A file is there but cannot be loaded or used.
    Unusable,
}

impl RuntimeError {
    fn missing(detail: String) -> Self {
        Self {
            kind: RuntimeErrorKind::Missing,
            detail,
        }
    }

    fn unusable(detail: String) -> Self {
        Self {
            kind: RuntimeErrorKind::Unusable,
            detail,
        }
    }

    pub fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    /// What went wrong, without the `RUNTIME_UNAVAILABLE` start.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{RUNTIME_UNAVAILABLE}: {}", self.detail)
    }
}

impl std::error::Error for RuntimeError {}

/// The library `ort` would open: `ORT_DYLIB_PATH`, else the platform's
/// file name; a relative path beside the executable when a file is there
/// (as `ort` resolves it).
pub fn library_path() -> PathBuf {
    let path = match std::env::var_os("ORT_DYLIB_PATH") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(default_library_name()),
    };
    if path.is_absolute() {
        return path;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&path)))
        .filter(|beside| beside.exists())
        .unwrap_or(path)
}

fn default_library_name() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_vendor = "apple") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// Identifies a library file's content well enough to notice a replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// A path the system would only search for (a bare file name).
fn is_bare_name(path: &Path) -> bool {
    path.parent().is_none_or(|p| p.as_os_str().is_empty())
}

enum State {
    Ready,
    /// The library at `path` (as `stamp`) could not be used.
    Failed {
        path: PathBuf,
        stamp: Option<Stamp>,
        error: RuntimeError,
    },
}

/// What has been tried, and the outcome kept.
#[derive(Default)]
pub(crate) struct Runtime {
    state: Option<State>,
}

impl Runtime {
    /// `load` the library at `path` unless it is loaded, or failed before
    /// and its file has not changed since.
    pub(crate) fn ensure(
        &mut self,
        path: &Path,
        load: impl FnOnce(&Path) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        match &self.state {
            Some(State::Ready) => return Ok(()),
            Some(State::Failed {
                path: failed,
                stamp: was,
                error,
            }) if failed == path && *was == stamp(path) => return Err(error.clone()),
            _ => {}
        }
        match load(path) {
            Ok(()) => {
                self.state = Some(State::Ready);
                Ok(())
            }
            Err(error) => {
                // A missing library is looked for again next time; anything
                // else waits for its file to change.
                self.state = (error.kind == RuntimeErrorKind::Unusable).then(|| State::Failed {
                    path: path.to_path_buf(),
                    stamp: stamp(path),
                    error: error.clone(),
                });
                Err(error)
            }
        }
    }
}

static RUNTIME: Mutex<Runtime> = Mutex::new(Runtime { state: None });

/// Loads ONNX Runtime (`library_path`) for `ort`, or says why it cannot: call before any other `ort` API (`Model::load` does).
/// Cheap after the first success; a failure is remembered as described in
/// the module documentation.
pub fn ensure_runtime() -> Result<(), RuntimeError> {
    let path = library_path();
    let mut runtime = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    runtime.ensure(&path, |path| {
        let result = load(path);
        match &result {
            Ok(version) => log::info!("AI: ONNX Runtime {version} loaded from {}", path.display()),
            // Looked for on every request: said once.
            Err(e) if e.kind == RuntimeErrorKind::Missing => {
                if !MISSING_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log::info!("AI: {e}");
                }
            }
            Err(e) => log::warn!("AI: {e}"),
        }
        result.map(|_| ())
    })
}

/// The process's ONNX Runtime environment, created when the library loads
/// and never released (the library is never unloaded either).
static ENVIRONMENT: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();

/// The ONNX Runtime environment every session and provider uses, after
/// `ensure_runtime`.
pub fn environment() -> Result<&'static ort::environment::Environment, RuntimeError> {
    ensure_runtime()?;
    ENVIRONMENT
        .get()
        .ok_or_else(|| RuntimeError::unusable("ONNX Runtime environment missing".to_string()))
}

static MISSING_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What keeps Microsoft's ONNX Runtime 1.30.0 libraries from reporting to
/// Microsoft (`DisableTelemetryEvents` does not: the first report is written
/// while the environment is created, and the uploader stays live).
/// `ORT_DISABLE_TELEMETRY`: the full opt-out of the 1DS uploader on macOS and
/// Linux, read when the first environment is created. `ORT_RUNNING_UNIT_TESTS`:
/// the only switch the Windows library reads; its event provider (in
/// Microsoft's telemetry group) is then never registered, read when the
/// library first uses its environment. The second is Microsoft's internal
/// test signal, used for nothing else in 1.30.0 (posix and windows
/// telemetry.cc): check both again when the runtime changes.
/// notes: benchmark-results/ortt2-2026-11-18/REPORT.md
pub const TELEMETRY_OPT_OUT: [(&str, &str); 2] = [
    ("ORT_DISABLE_TELEMETRY", "1"),
    ("ORT_RUNNING_UNIT_TESTS", "1"),
];

/// Sets `TELEMETRY_OPT_OUT` in the process environment. Call it first in
/// `main`, before any other thread exists; `load` sets it too, before the
/// library is opened, for tools and tests that start without it.
pub fn opt_out_of_runtime_telemetry() {
    for (name, value) in TELEMETRY_OPT_OUT {
        if std::env::var_os(name).is_some_and(|v| v == value) {
            continue;
        }
        // SAFETY: `main` calls it before other threads start. `load` only
        // sets it when a tool or test binary skipped that call; other
        // threads may exist then, and a native `getenv` of any name racing
        // with this change is undefined on Unix. Accepted for those
        // binaries: without it the runtime reports to Microsoft.
        unsafe { std::env::set_var(name, value) };
    }
}

/// Opens `path` and checks it is a usable ONNX Runtime (the API version
/// `ort` needs), then has `ort` load it. Returns the runtime's version.
pub(crate) fn load(path: &Path) -> Result<String, RuntimeError> {
    let shown = path.display();
    if !is_bare_name(path) && !path.is_file() {
        return Err(RuntimeError::missing(format!(
            "ONNX Runtime is not installed (no library at {shown})"
        )));
    }
    // Windows: the bundled VC++ runtime first, and none too old for the
    // library (gap G76; `vc_runtime`).
    #[cfg(windows)]
    match crate::vc_runtime::prepare(path) {
        Ok(in_use) => log::info!("AI: Visual C++ runtime {in_use}"),
        Err(why) => {
            return Err(RuntimeError::unusable(format!(
                "ONNX Runtime at {shown} not loaded: {why}"
            )));
        }
    }
    // Windows: the DirectX Shader Compiler for the WebGPU plugin beside it,
    // before the environment registers that plugin (gap G76; `dxc`).
    #[cfg(windows)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        crate::dxc::prepare(dir);
    }
    // No reports to Microsoft: read when the library first uses its
    // environment, which opening it may already do.
    opt_out_of_runtime_telemetry();
    let version = check_library(path)?;
    // `ort` loads this file from now on; its one environment is created
    // here and kept for the life of the process (`environment`).
    let builder = ort::init_from(path)
        .map_err(|error| {
            RuntimeError::unusable(format!("ONNX Runtime at {shown} could not start: {error}"))
        })?
        // Defence in depth: stops events after this point (Windows ETW too).
        .with_telemetry(false);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || builder.build())) {
        Ok(Ok(env)) => {
            let _ = ENVIRONMENT.set(env);
            Ok(version)
        }
        Ok(Err(error)) => Err(RuntimeError::unusable(format!(
            "ONNX Runtime {version} at {shown} could not start: {error}"
        ))),
        Err(panic) => Err(RuntimeError::unusable(format!(
            "ONNX Runtime {version} at {shown} could not start: {}",
            panic_message(panic.as_ref())
        ))),
    }
}

/// The library's version, after the checks `ort` would otherwise make with
/// a panic: it opens, has `OrtGetApiBase`, is new enough and offers the API
/// version `ort` uses.
fn check_library(path: &Path) -> Result<String, RuntimeError> {
    let shown = path.display();
    let library = match unsafe { libloading::Library::new(path) } {
        Ok(library) => library,
        Err(e) if is_bare_name(path) => {
            return Err(RuntimeError::missing(format!(
                "ONNX Runtime is not installed ({shown} not found: {e})"
            )));
        }
        Err(e) => {
            return Err(RuntimeError::unusable(format!(
                "{shown} could not be loaded: {e}"
            )));
        }
    };
    let result = unsafe { api_version(&library) };
    // Kept loaded whatever the outcome (see the module documentation);
    // `ort` opens the same image.
    std::mem::forget(library);
    let version = result.map_err(|why| RuntimeError::unusable(format!("{shown} {why}")))?;
    let minor = version
        .split('.')
        .nth(1)
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    let required_minor = if cfg!(target_os = "macos") {
        23
    } else {
        ort::MINOR_VERSION
    };
    if minor < required_minor {
        return Err(RuntimeError::unusable(format!(
            "ONNX Runtime {version} at {shown} is too old: 1.{}.0 or later is needed",
            required_minor
        )));
    }
    Ok(version)
}

/// `OrtGetApiBase`'s version string, if the API `ort` uses is offered.
unsafe fn api_version(library: &libloading::Library) -> Result<String, String> {
    type GetApiBase = unsafe extern "system" fn() -> *const ort::sys::OrtApiBase;
    let get_base = unsafe { library.get::<GetApiBase>(b"OrtGetApiBase\0") }
        .map_err(|_| "is not ONNX Runtime (no OrtGetApiBase)".to_string())?;
    let base = unsafe { get_base() };
    if base.is_null() {
        return Err("is not usable (OrtGetApiBase returned nothing)".to_string());
    }
    let base = unsafe { &*base };
    let version = unsafe { (base.GetVersionString)() };
    let version = if version.is_null() {
        String::from("of unknown version")
    } else {
        unsafe { CStr::from_ptr(version) }
            .to_string_lossy()
            .into_owned()
    };
    if unsafe { (base.GetApi)(ort::sys::ORT_API_VERSION) }.is_null() {
        return Err(format!(
            "(ONNX Runtime {version}) does not offer API version {}",
            ort::sys::ORT_API_VERSION
        ));
    }
    Ok(version)
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "it panicked".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn runtime_initialization_disables_telemetry() {
        // ort exposes no getter for this setting. Pin the opt-out on the
        // production builder so this can run without a native runtime.
        assert!(include_str!("runtime.rs").contains(concat!(".with_telemetry(", "false);")));
    }

    #[test]
    fn the_uploader_opt_out_comes_before_the_environment() {
        // ONNX Runtime 1.30.0's switches (posix/telemetry.cc Initialize,
        // windows/telemetry.cc WindowsTelemetry()).
        assert_eq!(
            TELEMETRY_OPT_OUT,
            [
                ("ORT_DISABLE_TELEMETRY", "1"),
                ("ORT_RUNNING_UNIT_TESTS", "1")
            ]
        );
        // `load` sets them before it opens the library, for tools and tests
        // whose `main` did not.
        let load = include_str!("runtime.rs")
            .split("fn check_library(path: &Path)")
            .next()
            .unwrap();
        let load = &load[load.rfind("pub(crate) fn load(").unwrap()..];
        let opt_out = load.find("opt_out_of_runtime_telemetry();").unwrap();
        assert!(opt_out < load.find("check_library(path)").unwrap());
        assert!(opt_out < load.find("ort::init_from(path)").unwrap());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rawtonic-onnx-runtime-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_library_is_an_error() {
        let path = scratch("missing").join(default_library_name());
        let error = load(&path).unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Missing);
        assert!(
            error.to_string().starts_with(RUNTIME_UNAVAILABLE),
            "{error}"
        );
        assert!(error.to_string().contains("not installed"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_a_library_is_an_error_not_a_panic() {
        let path = scratch("text").join(default_library_name());
        std::fs::write(&path, b"not a library\n").unwrap();
        let error = load(&path).unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Unusable);
        assert!(
            error.to_string().starts_with(RUNTIME_UNAVAILABLE),
            "{error}"
        );
        assert!(error.to_string().contains("could not be loaded"), "{error}");
    }

    #[test]
    fn a_folder_is_not_a_library() {
        let path = scratch("folder");
        let error = load(&path).unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Missing, "{error}");
    }

    fn unusable(_: &Path) -> Result<(), RuntimeError> {
        Err(RuntimeError::unusable("broken".into()))
    }

    #[test]
    fn a_failed_library_is_not_loaded_again_until_it_changes() {
        let path = scratch("retry").join(default_library_name());
        std::fs::write(&path, b"broken").unwrap();
        let mut runtime = Runtime::default();
        let calls = Cell::new(0);
        let counted = |p: &Path| {
            calls.set(calls.get() + 1);
            unusable(p)
        };
        for _ in 0..5 {
            assert!(runtime.ensure(&path, counted).is_err());
        }
        assert_eq!(calls.get(), 1, "loaded again while unchanged");
        // Replaced (another size): tried again, and it works now.
        std::fs::write(&path, b"a library at last").unwrap();
        assert!(runtime.ensure(&path, |_| Ok(())).is_ok());
        assert!(runtime.ensure(&path, |_| panic!("loaded twice")).is_ok());
    }

    #[test]
    fn a_missing_library_is_looked_for_again() {
        let path = scratch("later").join(default_library_name());
        let mut runtime = Runtime::default();
        let calls = Cell::new(0);
        for _ in 0..3 {
            let error = runtime
                .ensure(&path, |p| {
                    calls.set(calls.get() + 1);
                    load(p).map(|_| ())
                })
                .unwrap_err();
            assert_eq!(error.kind(), RuntimeErrorKind::Missing);
        }
        assert_eq!(calls.get(), 3);
        // Installed meanwhile: the next request uses it.
        std::fs::write(&path, b"installed").unwrap();
        assert!(runtime.ensure(&path, |_| Ok(())).is_ok());
    }
}
