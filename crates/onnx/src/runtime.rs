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

static MISSING_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Opens `path` and checks it is a usable ONNX Runtime (the API version
/// `ort` needs), then has `ort` load it. Returns the runtime's version.
pub(crate) fn load(path: &Path) -> Result<String, RuntimeError> {
    let shown = path.display();
    if !is_bare_name(path) && !path.is_file() {
        return Err(RuntimeError::missing(format!(
            "ONNX Runtime is not installed (no library at {shown})"
        )));
    }
    let version = check_library(path)?;
    // `ort` loads this file from now on (its environment is created on
    // first use, as before).
    ort::init_from(path).map_err(|error| {
        RuntimeError::unusable(format!("ONNX Runtime at {shown} could not start: {error}"))
    })?;
    match std::panic::catch_unwind(|| {
        ort::api();
    }) {
        Ok(()) => Ok(version),
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
