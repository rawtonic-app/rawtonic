// SPDX-License-Identifier: AGPL-3.0-or-later
//! Writes what the face retouch's Eyes and Teeth act on in real photos: per
//! photo a JSON file with each face's eyes (lid opening, Face Mesh's iris
//! circle and the fitted one) and mouth outline, in photo pixels, for the
//! measurement scripts. Ignored: needs `FACE_MODELS`, `FACE_PHOTOS`,
//! `FACE_OUT` and `ORT_DYLIB_PATH` (see real_photos.rs).
//!
//! cargo test -p rawtonic-faces --release --test eye_teeth_photos -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use rawtonic_faces::landmarks::{LEFT_IRIS, RIGHT_IRIS};
use rawtonic_faces::{FaceModels, analyze, eye_teeth};

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn mesh_iris(points: &[[f32; 2]], indices: &[usize; 5]) -> [f32; 3] {
    let c = points[indices[0]];
    let r = indices[1..]
        .iter()
        .map(|&i| (points[i][0] - c[0]).hypot(points[i][1] - c[1]))
        .sum::<f32>()
        / 4.0;
    [c[0], c[1], r]
}

#[test]
#[ignore]
fn writes_eye_teeth_geometry_of_real_photos() {
    let (Some(models), Some(photos), Some(out)) =
        (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
    else {
        eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let models = FaceModels::load(&models).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&photos)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
        .collect();
    files.sort();
    for file in files {
        let image = image::open(&file).unwrap().into_rgb8();
        let analysis = analyze(&image, &models).unwrap();
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let faces: Vec<serde_json::Value> = analysis
            .faces
            .iter()
            .map(|face| {
                let start = Instant::now();
                let found = eye_teeth::eye_teeth(&image, face);
                let took = start.elapsed();
                let mesh = face.landmarks.as_ref().map(|p| {
                    let mut both = [mesh_iris(p, &RIGHT_IRIS), mesh_iris(p, &LEFT_IRIS)];
                    both.sort_by(|a, b| a[0].total_cmp(&b[0]));
                    both
                });
                let eye_distance = face
                    .landmarks
                    .as_ref()
                    .map(|p| (p[263][0] - p[33][0]).hypot(p[263][1] - p[33][1]));
                serde_json::json!({
                    "box": face.detection.bbox,
                    "eyeDistance": eye_distance,
                    "ms": took.as_secs_f64() * 1000.0,
                    "meshIris": mesh,
                    "eyes": found.eyes.iter().map(|e| e.as_ref().map(|e| serde_json::json!({
                        "iris": e.iris, "lid": e.lid,
                    }))).collect::<Vec<_>>(),
                    "mouth": found.mouth,
                })
            })
            .collect();
        println!("{name}: {} face(s)", faces.len());
        std::fs::write(
            out.join(format!("{name}.json")),
            serde_json::to_string(&serde_json::json!({
                "width": image.width(), "height": image.height(), "faces": faces,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
