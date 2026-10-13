// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-only diagnostic; never commit the photograph or inferred geometry.
#[test]
#[ignore = "requires FACE_MODELS, RELIGHT_PHOTO, RELIGHT_OUT and ONNX Runtime"]
fn estimates_portrait_relief() {
    let dir = std::path::PathBuf::from(std::env::var("FACE_MODELS").expect("FACE_MODELS"));
    let models = rawtonic_faces::FaceModels::load(&dir).unwrap();
    let image = image::open(std::env::var("RELIGHT_PHOTO").expect("RELIGHT_PHOTO"))
        .unwrap()
        .resize(1600, 1600, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let start = std::time::Instant::now();
    let faces = rawtonic_faces::relight::analyze(&image, &models).unwrap();
    assert!(!faces.is_empty(), "portrait fixture must produce a face");
    println!(
        "Facial relief: {} faces, {:.1} ms",
        faces.len(),
        start.elapsed().as_secs_f64() * 1000.
    );
    for face in &faces {
        assert_eq!(face.len(), 468);
        assert!(face.iter().flatten().all(|v| v.is_finite()));
        assert!(face.iter().filter(|p| p[3] > 0.8).count() > 100);
    }
    let out = std::path::PathBuf::from(std::env::var("RELIGHT_OUT").expect("RELIGHT_OUT"));
    std::fs::write(out.join("faces.json"), serde_json::to_vec(&faces).unwrap()).unwrap();
}
