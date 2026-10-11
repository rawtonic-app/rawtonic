// SPDX-License-Identifier: AGPL-3.0-or-later
//! ONNX Runtime installed while the process runs: the request before it
//! fails as missing, the one after it loads the runtime. Needs a real
//! runtime library in `RAWTONIC_TEST_ORT` (copied, never changed).

use rawtonic_onnx::{RuntimeErrorKind, ensure_runtime};

#[test]
#[ignore = "needs RAWTONIC_TEST_ORT (an ONNX Runtime library)"]
fn a_runtime_installed_later_is_used() {
    let Some(source) = std::env::var_os("RAWTONIC_TEST_ORT") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("rawtonic-onnx-later-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let library = dir.join(std::path::Path::new(&source).file_name().unwrap());
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("ORT_DYLIB_PATH", &library) };

    let error = ensure_runtime().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Missing, "{error}");

    std::fs::copy(&source, &library).unwrap();
    ensure_runtime().unwrap();
    // `ort` works from here on.
    assert!(!ort::info().is_empty());
    ensure_runtime().unwrap();
}
