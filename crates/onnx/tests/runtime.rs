// SPDX-License-Identifier: AGPL-3.0-or-later
//! A broken ONNX Runtime library (`ORT_DYLIB_PATH` at a file that is not a
//! library) gives errors, never `ort`'s panic, for the runtime and for every
//! model load. One test: it sets the process environment.

use std::path::PathBuf;

use rawtonic_onnx::{Model, ModelKind, RUNTIME_UNAVAILABLE, RuntimeErrorKind, ensure_runtime};

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rawtonic-onnx-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_broken_runtime_is_an_error_not_a_panic() {
    let dir = scratch();
    let library = dir.join("libonnxruntime.so");
    std::fs::write(&library, b"this is not ONNX Runtime\n").unwrap();
    // SAFETY: the only test in this binary; nothing else reads the
    // environment meanwhile.
    unsafe { std::env::set_var("ORT_DYLIB_PATH", &library) };

    let error = ensure_runtime().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Unusable);
    assert!(
        error.to_string().starts_with(RUNTIME_UNAVAILABLE),
        "{error}"
    );

    // Every model load says the same, whatever the model.
    let model = dir.join("birefnet_subject.onnx");
    std::fs::write(&model, b"model").unwrap();
    for kind in [ModelKind::Subject, ModelKind::Sky, ModelKind::FaceDetector] {
        let error = Model::load(&model, kind)
            .err()
            .expect("loaded on a broken runtime");
        assert!(
            format!("{error:#}").starts_with(RUNTIME_UNAVAILABLE),
            "{kind:?}: {error:#}"
        );
    }

    // Asked again (a tool retrying): answered from the remembered failure.
    let started = std::time::Instant::now();
    for _ in 0..100 {
        assert!(ensure_runtime().is_err());
    }
    assert!(
        started.elapsed().as_millis() < 500,
        "{:?}",
        started.elapsed()
    );

    // Gone: reported as missing, and looked for again next time.
    std::fs::remove_file(&library).unwrap();
    let error = ensure_runtime().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Missing, "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}
