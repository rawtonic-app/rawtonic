// SPDX-License-Identifier: AGPL-3.0-or-later
//! People masks and face retouch (rawtonic-faces): the three face models
//! from the model pack, one analysis of the open photo kept in memory (per
//! geometry), the commands of the masks panel's People entry, and the face
//! data the retouch tool stores in the recipe (`faceRetouch`).

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose};
use image::{ImageFormat, RgbImage};
use rawtonic_faces::{FaceAnalysis, FaceModels, Part};
use serde::Serialize;
use tauri::Manager;

use crate::ai_processing::AiPeopleMaskParameters;
use crate::app_state::{AiTaskGuard, AppState};
use crate::editor_preview::get_cached_full_warped_image;

/// Files of the model pack (docs/MODELS.md) and their SHA-256.
const MODEL_FILES: [(&str, &str); 3] = [
    (
        rawtonic_faces::DETECTOR_FILE,
        "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    ),
    (
        rawtonic_faces::MESH_FILE,
        "5e82081f46411caa2b8a6734bd977c0120293f0debe29cf76cbc0c5703d48be3",
    ),
    (
        rawtonic_faces::PARSER_FILE,
        "c949f945f549e3f865a19b67753694b928e061e40217ac38c0ad39daa8c4b194",
    ),
];

/// The analysis of the open photo, for one geometry.
pub struct CachedFaces {
    key: String,
    image: Arc<RgbImage>,
    analysis: Arc<FaceAnalysis>,
}

#[derive(Default)]
pub struct FaceState {
    models: Option<Arc<FaceModels>>,
    cached: Option<CachedFaces>,
}

impl FaceState {
    pub fn has_models(&self) -> bool {
        self.models.is_some()
    }

    /// Drops the models and the analysis (`ai_lifecycle`); `true` if there
    /// was something to drop.
    pub fn unload(&mut self) -> bool {
        let had = self.models.is_some() || self.cached.is_some();
        self.models = None;
        self.cached = None;
        had
    }
}

pub type SharedFaceState = Mutex<FaceState>;

/// One detected person for the masks panel.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonSummary {
    pub index: usize,
    /// x, y, width, height of the face as fractions of the photo (after
    /// geometry).
    pub face: [f32; 4],
    /// A small JPEG of the face as a data URL.
    pub thumbnail: String,
    /// Eyes, brows, lips and teeth can be selected (not in profile views).
    pub has_features: bool,
}

/// The error of a face tool whose models are not installed and that was not
/// asked to download them (the frontend offers the download).
pub(crate) const FACE_MODELS_MISSING: &str = "face-models-missing";

/// The face models, loaded once. `download`: fetch missing files from the
/// model pack (an explicit request, PRIVACY.md); otherwise a missing file
/// gives `FACE_MODELS_MISSING`.
pub(crate) async fn models(
    app_handle: &tauri::AppHandle,
    download: bool,
) -> Result<Arc<FaceModels>, String> {
    let state = app_handle.state::<AppState>();
    if let Some(models) = state.faces.lock().unwrap().models.clone() {
        return Ok(models);
    }
    let _guard = state.ai_init_lock.lock().await;
    if let Some(models) = state.faces.lock().unwrap().models.clone() {
        return Ok(models);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    crate::ai_processing::ensure_runtime().map_err(|e| e.to_string())?;
    let dir = crate::ai_processing::models_dir(app_handle).map_err(|e| e.to_string())?;
    for (file, sha) in MODEL_FILES {
        if !download {
            if crate::ai_processing::verify_sha256(&dir.join(file), sha)
                .map_err(|e| e.to_string())?
            {
                continue;
            }
            return Err(FACE_MODELS_MISSING.to_string());
        }
        crate::ai_processing::download_verified(app_handle, &dir, file, sha, "Face models")
            .await
            .map_err(|e| e.to_string())?;
    }
    let models = tokio::task::spawn_blocking(move || FaceModels::load(&dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    crate::register_exit_handler();
    let models = Arc::new(models);
    let mut faces = state.faces.lock().unwrap();
    if crate::ai_lifecycle::LIFECYCLE.keep(generation) {
        faces.models = Some(models.clone());
    }
    Ok(models)
}

/// The analysis of the open photo with `js_adjustments`' geometry, made on
/// first use.
async fn analysis(
    state: &tauri::State<'_, AppState>,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
) -> Result<(Arc<RgbImage>, Arc<FaceAnalysis>), String> {
    // Keyed by the photo actually loaded, taken before the models load: a
    // photo switch meanwhile cannot file one photo's faces under another.
    let loaded = crate::image_loader::original_image(state).await?;
    let key = crate::ai_commands::geometry_key(&loaded, js_adjustments);
    if let Some(c) = state.faces.lock().unwrap().cached.as_ref()
        && c.key == key
    {
        return Ok((c.image.clone(), c.analysis.clone()));
    }
    let models = models(app_handle, true).await?;
    let is_raw = loaded.is_raw;
    let (app, adjustments) = (app_handle.clone(), js_adjustments.clone());
    let (image, analysis) = tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let warped = get_cached_full_warped_image(&state, &loaded.image, is_raw, &adjustments)?;
        let image = rawtonic_faces::recipe::display_rgb8(&warped, is_raw);
        rawtonic_faces::analyze(&image, &models)
            .map(|a| (Arc::new(image), Arc::new(a)))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    state.faces.lock().unwrap().cached = Some(CachedFaces {
        key,
        image: image.clone(),
        analysis: analysis.clone(),
    });
    Ok((image, analysis))
}

fn data_url(bytes: &[u8], mime: &str) -> String {
    format!(
        "data:{mime};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    )
}

/// The people in the open photo, left to right.
#[tauri::command]
pub async fn detect_people(
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<PersonSummary>, String> {
    crate::ai_lifecycle::LIFECYCLE.touch();
    let _running = crate::ai_lifecycle::LIFECYCLE.run();
    let (image, analysis) = analysis(&state, &app_handle, &js_adjustments).await?;
    tokio::task::spawn_blocking(move || {
        let (w, h) = (analysis.width as f32, analysis.height as f32);
        analysis
            .faces
            .iter()
            .enumerate()
            .map(|(index, face)| {
                let thumb = rawtonic_faces::thumbnail(&image, face, 96);
                let mut jpeg = Cursor::new(Vec::new());
                thumb
                    .write_to(&mut jpeg, ImageFormat::Jpeg)
                    .map_err(|e| e.to_string())?;
                let [x, y, fw, fh] = face.detection.bbox;
                Ok(PersonSummary {
                    index,
                    face: [x / w, y / h, fw / w, fh / h],
                    thumbnail: data_url(jpeg.get_ref(), "image/jpeg"),
                    has_features: face.landmarks.is_some(),
                })
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A People mask of `parts` (see `rawtonic_faces::Part::name`) of everyone
/// or of `person`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_people_mask(
    js_adjustments: serde_json::Value,
    parts: Vec<String>,
    person: Option<usize>,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiPeopleMaskParameters, String> {
    let guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancelled = || guard.as_ref().is_some_and(|g| g.token.is_cancelled());
    crate::ai_lifecycle::LIFECYCLE.touch();
    let _running = crate::ai_lifecycle::LIFECYCLE.run();
    let selected: Vec<Part> = parts.iter().filter_map(|p| Part::from_name(p)).collect();
    if selected.is_empty() {
        return Err("No person parts selected".to_string());
    }
    let (image, analysis) = analysis(&state, &app_handle, &js_adjustments).await?;
    if cancelled() {
        return Err("Task cancelled".to_string());
    }
    if let Some(i) = person
        && i >= analysis.faces.len()
    {
        return Err(format!("There is no person {} in this photo", i + 1));
    }
    let png = tokio::task::spawn_blocking(move || {
        let mask = rawtonic_faces::part_mask(&image, &analysis, &selected, person);
        let mut png = Cursor::new(Vec::new());
        mask.write_to(&mut png, ImageFormat::Png)
            .map(|_| png.into_inner())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if cancelled() {
        return Err("Task cancelled".to_string());
    }
    Ok(AiPeopleMaskParameters {
        parts,
        person,
        mask_data_base64: Some(data_url(&png, "image/png")),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
    })
}

/// The faces of the open photo for the face retouch tool (see
/// `retouch_faces`), found in the mask frame of `js_adjustments`' geometry
/// (transform adjustments with rotation, flips and orientation steps).
/// Returns `{ frame, faces }`: the frame key the faces belong to
/// (rawtonic-engine `face_retouch::frame_key`) and the faces.
#[tauri::command]
pub async fn analyze_face_retouch(
    js_adjustments: serde_json::Value,
    download: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    // Opening the Retouch tab analyses with installed models only; the
    // download needs the user's click (`download`).
    let models = models(&app_handle, download.unwrap_or(false)).await?;
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let warped = get_cached_full_warped_image(&state, &source, is_raw, &js_adjustments)?;
        // The mask frame: the spatial transforms without the crop.
        let mut uncropped = js_adjustments.clone();
        uncropped["crop"] = serde_json::Value::Null;
        let (framed, _) =
            crate::adjustment_utils::apply_spatial_transformations(warped.as_ref(), &uncropped);
        let image = rawtonic_faces::recipe::display_rgb8(&framed, is_raw);
        let analysis = rawtonic_faces::analyze(&image, &models).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "frame": rawtonic_engine::face_retouch::frame_key(&js_adjustments),
            "faces": rawtonic_faces::recipe::retouch_faces(&image, &analysis)?,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A face's own values in the photo they come from: its box centre as a
/// share of the photo's width and height.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceSettingsAt {
    pub center: [f64; 2],
    pub settings: serde_json::Value,
}

/// The face retouch of `face_settings` given to the faces found in a photo:
/// each takes the values of the nearest source face within an eighth of the
/// photo (Capture One maps faces by order, which breaks when people move).
fn carry_face_settings(
    faces: &mut [serde_json::Value],
    face_settings: &[FaceSettingsAt],
    (width, height): (f64, f64),
) {
    for face in faces.iter_mut() {
        let Some(b) = face["box"].as_array().map(|b| {
            b.iter()
                .map(|v| v.as_f64().unwrap_or(0.0))
                .collect::<Vec<_>>()
        }) else {
            continue;
        };
        if b.len() < 4 {
            continue;
        }
        let c = [(b[0] + b[2] / 2.0) / width, (b[1] + b[3] / 2.0) / height];
        let nearest = face_settings
            .iter()
            .map(|f| (f, (f.center[0] - c[0]).hypot(f.center[1] - c[1])))
            .filter(|(_, d)| *d < 0.125)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((f, _)) = nearest {
            face["settings"] = f.settings.clone();
        }
    }
}

/// Copy/apply of the face retouch: for each photo the faces are found as
/// the Retouch tab finds them (in its mask frame, its own geometry), take
/// `settings` for all faces and, by position, the per-face values, and the
/// result is written like any applied adjustment (sidecar, XMP, thumbnail).
#[tauri::command]
pub async fn apply_face_retouch_to_paths(
    paths: Vec<String>,
    settings: serde_json::Value,
    face_settings: Vec<FaceSettingsAt>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    let models = models(&app_handle, true).await?;
    let app_settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    let face_settings = Arc::new(face_settings);
    let mut done = 0;
    for path in paths {
        let face_settings = face_settings.clone();
        let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(&path);
        let models = models.clone();
        let app_settings = app_settings.clone();
        let settings = settings.clone();
        let face_retouch =
            tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
                let mut recipe =
                    crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path)
                        .adjustments;
                if !recipe.is_object() {
                    recipe = serde_json::json!({});
                }
                rawtonic_export::exif_processing::prepare_recipe_lens(&mut recipe, &source_path)?;
                let bytes = std::fs::read(&source_path).map_err(|e| e.to_string())?;
                let name = source_path.to_string_lossy();
                let is_raw = rawtonic_engine::formats::is_raw_file(&source_path);
                let base = crate::image_loader::load_base_image_for_recipe(
                    &bytes,
                    &name,
                    false,
                    &app_settings,
                    None,
                    None,
                    &recipe,
                )
                .map_err(|e| e.to_string())?;
                let warped = rawtonic_engine::mask_generation::prepare_shared_warped_mask_image(
                    &Arc::new(base),
                    is_raw,
                    &recipe,
                )?;
                let mut uncropped = recipe.clone();
                uncropped["crop"] = serde_json::Value::Null;
                let (framed, _) = crate::adjustment_utils::apply_spatial_transformations(
                    warped.as_ref(),
                    &uncropped,
                );
                let image = rawtonic_faces::recipe::display_rgb8(&framed, is_raw);
                let analysis =
                    rawtonic_faces::analyze(&image, &models).map_err(|e| e.to_string())?;
                let mut faces = rawtonic_faces::recipe::retouch_faces(&image, &analysis)?;
                carry_face_settings(
                    &mut faces,
                    &face_settings,
                    (image.width() as f64, image.height() as f64),
                );
                let frame = rawtonic_engine::face_retouch::frame_key(&recipe);
                // The photo's own protection brush strokes stay (in the
                // geometry they were painted in).
                let protect = recipe
                    .get("faceRetouch")
                    .filter(|r| {
                        r.get("frame")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|stored| {
                                rawtonic_engine::face_retouch::frame_matches(stored, &recipe)
                            })
                    })
                    .and_then(|r| r.get("protect"))
                    .cloned();
                let mut value = serde_json::json!({
                    "settings": settings,
                    "faces": faces,
                    "frame": frame,
                    // The panel compares its own key and finds the faces again
                    // when it opens the photo (settings carried over).
                    "geometry": "",
                });
                if let Some(protect) = protect {
                    value["protect"] = protect;
                }
                Ok(value)
            })
            .await
            .map_err(|e| e.to_string())?;
        match face_retouch {
            Ok(value) => {
                // A recipe not saved does not count as done.
                match crate::file_management::apply_adjustments_to_paths(
                    vec![path.clone()],
                    serde_json::json!({ "faceRetouch": value }),
                    None,
                    None,
                    app_handle.clone(),
                )
                .await
                {
                    Ok(()) => done += 1,
                    Err(failure) => log::warn!("Face retouch for {path}: {}", failure.message),
                }
            }
            Err(error) => log::warn!("Face retouch for {path}: {error}"),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;

    #[test]
    fn face_settings_go_to_the_face_at_the_same_place() {
        let mut faces = vec![
            serde_json::json!({"box": [100.0, 100.0, 100.0, 100.0], "settings": {}}),
            serde_json::json!({"box": [700.0, 100.0, 100.0, 100.0], "settings": {}}),
        ];
        let own = vec![FaceSettingsAt {
            center: [0.76, 0.16],
            settings: serde_json::json!({"evenSkin": 80}),
        }];
        carry_face_settings(&mut faces, &own, (1000.0, 1000.0));
        assert_eq!(faces[0]["settings"], serde_json::json!({}));
        assert_eq!(faces[1]["settings"], serde_json::json!({"evenSkin": 80}));
    }

    /// Renders the face retouch of real photos: analysis as the app makes
    /// it, then the engine's render on the photo as linear float (no
    /// geometry, so the mask frame is the photo). Writes `<name>_before.jpg`
    /// and `<name>_after.jpg` crops of the first face. Ignored: needs
    /// `FACE_MODELS`, `FACE_PHOTOS` (JPEGs), `FACE_OUT`, `ORT_DYLIB_PATH`;
    /// `FACE_SETTINGS` (JSON, e.g. {"blemishes":50,"evenSkin":50}).
    #[test]
    #[ignore]
    fn renders_face_retouch_of_real_photos() {
        let env = |n: &str| std::env::var_os(n).map(std::path::PathBuf::from);
        let (Some(models), Some(photos), Some(out)) =
            (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
        else {
            eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
            return;
        };
        let settings: serde_json::Value = std::env::var("FACE_SETTINGS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({"blemishes": 50, "evenSkin": 50}));
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
            let analysis = rawtonic_faces::analyze(&image, &models).unwrap();
            let faces = rawtonic_faces::recipe::retouch_faces(&image, &analysis).unwrap();
            if faces.is_empty() {
                continue;
            }
            let adjustments =
                serde_json::json!({"faceRetouch": {"settings": settings, "faces": faces}});
            let name = file.file_stem().unwrap().to_string_lossy().to_string();
            // With each face's detection and landmarks, for prototypes.
            let points: Vec<serde_json::Value> = analysis
                .faces
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "box": f.detection.bbox,
                        "keypoints": f.detection.keypoints,
                        "landmarks": f.landmarks,
                    })
                })
                .collect();
            std::fs::write(
                out.join(format!("{name}.json")),
                serde_json::to_string(
                    &serde_json::json!({"faceRetouch": adjustments["faceRetouch"], "analysis": points}),
                )
                .unwrap(),
            )
            .unwrap();
            let mut linear = DynamicImage::ImageRgb8(image.clone()).into_rgb32f();
            for p in linear.pixels_mut() {
                for c in p.0.iter_mut() {
                    *c = if *c <= 0.04045 {
                        *c / 12.92
                    } else {
                        ((*c + 0.055) / 1.055).powf(2.4)
                    };
                }
            }
            // A fit-view preview: a quarter-size copy retouched with the
            // scaled recipe, against the full render reduced afterwards.
            let quarter = |img: &image::Rgb32FImage| {
                image::imageops::resize(
                    img,
                    img.width() / 4,
                    img.height() / 4,
                    image::imageops::FilterType::Triangle,
                )
            };
            let mut preview = quarter(&linear);
            let preview_before = preview.clone();
            let start = std::time::Instant::now();
            rawtonic_engine::face_retouch::apply_face_retouch(
                &mut preview,
                &rawtonic_engine::face_retouch::scaled_recipe(&adjustments, 0.25),
            );
            let preview_took = start.elapsed();
            let start = std::time::Instant::now();
            rawtonic_engine::face_retouch::apply_face_retouch(&mut linear, &adjustments);
            let took = start.elapsed();
            let expected = quarter(&linear);
            let (mut diff, mut change) = (0.0f64, 0.0f64);
            for ((a, b), o) in preview
                .pixels()
                .zip(expected.pixels())
                .zip(preview_before.pixels())
            {
                for c in 0..3 {
                    diff += f64::from((a[c] - b[c]).abs());
                    change += f64::from((b[c] - o[c]).abs());
                }
            }
            for p in linear.pixels_mut() {
                for c in p.0.iter_mut() {
                    let v = c.clamp(0.0, 1.0);
                    *c = if v <= 0.003_130_8 {
                        12.92 * v
                    } else {
                        1.055 * v.powf(1.0 / 2.4) - 0.055
                    };
                }
            }
            let after = DynamicImage::ImageRgb32F(linear).into_rgb8();
            let b = faces[0]["box"].as_array().unwrap();
            let [x, y, w, h] = [0, 1, 2, 3].map(|i| b[i].as_f64().unwrap() as i64);
            let pad = w.max(h) / 4;
            let (x0, y0) = ((x - pad).max(0) as u32, (y - pad).max(0) as u32);
            let cw = ((w + 2 * pad) as u32).min(image.width() - x0);
            // Down to the neck too (for "include neck").
            let ch = ((2 * h + 2 * pad) as u32).min(image.height() - y0);
            image::imageops::crop_imm(&image, x0, y0, cw, ch)
                .to_image()
                .save(out.join(format!("{name}_before.jpg")))
                .unwrap();
            image::imageops::crop_imm(&after, x0, y0, cw, ch)
                .to_image()
                .save(out.join(format!("{name}_after.jpg")))
                .unwrap();
            let neck = faces
                .iter()
                .map(|f| {
                    let marked = f["blemishes"]
                        .as_array()
                        .map_or(0, |b| b.iter().filter(|b| b["neck"] == true).count());
                    format!(
                        "{}/{marked}",
                        if f["neck"].is_string() { "neck" } else { "-" }
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{name}: {} faces ({neck} neck blemishes), render {:.0} ms; quarter-size preview {:.0} ms, differs by {:.2} of the change",
                faces.len(),
                took.as_secs_f64() * 1e3,
                preview_took.as_secs_f64() * 1e3,
                diff / change.max(1e-9)
            );
        }
        std::mem::forget(models);
    }

    /// The protection brush on real photos: per face a stroke over the
    /// viewer-left cheek (as a user protects freckles there), rendered with
    /// and without it. Prints, per photo: values in the stroke's solid core
    /// that differ from the unretouched photo (must be 0), values outside
    /// the stroke that differ from the unprotected render (must be 0), the
    /// retouch's change kept on the rest of the face (mean |Δ|), healed
    /// blemishes inside the stroke, the protection's cost, and the
    /// quarter-size preview against the full render. Writes
    /// `<name>_{before,unprotected,protected}.jpg` face crops. Ignored: the
    /// same environment as `renders_face_retouch_of_real_photos`.
    #[test]
    #[ignore]
    fn protects_painted_areas_of_real_photos() {
        use rawtonic_engine::face_retouch::{apply_face_retouch, scaled_recipe};
        let env = |n: &str| std::env::var_os(n).map(std::path::PathBuf::from);
        let (Some(models), Some(photos), Some(out)) =
            (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
        else {
            eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
            return;
        };
        let settings: serde_json::Value = std::env::var("FACE_SETTINGS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({"blemishes": 70, "evenSkin": 35}));
        std::fs::create_dir_all(&out).unwrap();
        let models = FaceModels::load(&models).unwrap();
        let mut files: Vec<_> = std::fs::read_dir(&photos)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
            .collect();
        files.sort();
        let to_linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let to_srgb8 = |img: &image::Rgb32FImage| {
            let mut img = img.clone();
            for p in img.pixels_mut() {
                for c in p.0.iter_mut() {
                    let v = c.clamp(0.0, 1.0);
                    *c = if v <= 0.003_130_8 {
                        12.92 * v
                    } else {
                        1.055 * v.powf(1.0 / 2.4) - 0.055
                    };
                }
            }
            DynamicImage::ImageRgb32F(img).into_rgb8()
        };
        let mut measured = 0;
        for file in files {
            let image = image::open(&file).unwrap().into_rgb8();
            let analysis = rawtonic_faces::analyze(&image, &models).unwrap();
            let faces = rawtonic_faces::recipe::retouch_faces(&image, &analysis).unwrap();
            if faces.is_empty() {
                continue;
            }
            measured += 1;
            let name = file.file_stem().unwrap().to_string_lossy().to_string();
            // One stroke per face across the viewer-left cheek, a fifth of
            // the face wide, feathered over its outer half.
            let boxes: Vec<[f64; 4]> = faces
                .iter()
                .map(|f| [0, 1, 2, 3].map(|i| f["box"][i].as_f64().unwrap()))
                .collect();
            let strokes: Vec<(f64, f64, f64, f64)> = boxes
                .iter()
                .map(|[x, y, w, h]| (x + 0.18 * w, x + 0.38 * w, y + 0.62 * h, 0.1 * w))
                .collect();
            let protect: Vec<serde_json::Value> = strokes
                .iter()
                .map(|&(x0, x1, y, r)| {
                    let points: Vec<_> = (0..=10)
                        .map(|i| serde_json::json!({"x": x0 + (x1 - x0) * f64::from(i) / 10.0, "y": y}))
                        .collect();
                    serde_json::json!({"points": points, "radius": r, "feather": 50})
                })
                .collect();
            let plain = serde_json::json!({"faceRetouch": {"settings": settings, "faces": faces}});
            let mut protected_recipe = plain.clone();
            protected_recipe["faceRetouch"]["protect"] = serde_json::Value::Array(protect);
            let mut original = DynamicImage::ImageRgb8(image.clone()).into_rgb32f();
            for p in original.pixels_mut() {
                for c in p.0.iter_mut() {
                    *c = to_linear(*c);
                }
            }
            let mut unprotected = original.clone();
            let start = std::time::Instant::now();
            apply_face_retouch(&mut unprotected, &plain);
            let plain_took = start.elapsed();
            let mut protected = original.clone();
            let start = std::time::Instant::now();
            apply_face_retouch(&mut protected, &protected_recipe);
            let protected_took = start.elapsed();
            // Distance to the nearest stroke, relative to its radius.
            let reach = |x: f64, y: f64| {
                strokes
                    .iter()
                    .map(|&(x0, x1, sy, r)| (x - x.clamp(x0, x1)).hypot(y - sy) / r)
                    .fold(f64::INFINITY, f64::min)
            };
            let (mut core, mut core_changed, mut outside_changed) = (0u64, 0u64, 0u64);
            let (mut rest_change, mut rest_n) = (0.0f64, 0u64);
            let (mut core_change_unprotected, mut feather_n) = (0.0f64, 0u64);
            for (x, y, p) in protected.enumerate_pixels() {
                let d = reach(f64::from(x), f64::from(y));
                let (o, u) = (original.get_pixel(x, y), unprotected.get_pixel(x, y));
                if d <= 0.5 {
                    core += 1;
                    core_changed += (0..3).filter(|&c| p[c] != o[c]).count() as u64;
                    core_change_unprotected +=
                        (0..3).map(|c| f64::from((u[c] - o[c]).abs())).sum::<f64>();
                } else if d < 1.0 {
                    feather_n += 1;
                } else {
                    outside_changed += (0..3).filter(|&c| p[c] != u[c]).count() as u64;
                    let in_face = boxes.iter().any(|[bx, by, bw, bh]| {
                        (*bx..bx + bw).contains(&f64::from(x))
                            && (*by..by + bh).contains(&f64::from(y))
                    });
                    if in_face {
                        rest_change += (0..3).map(|c| f64::from((p[c] - o[c]).abs())).sum::<f64>();
                        rest_n += 1;
                    }
                }
            }
            // Blemishes the settings heal whose centre the solid core covers.
            let amount = settings["blemishes"].as_f64().unwrap_or(0.0) as f32 / 100.0;
            let threshold = rawtonic_engine::face_retouch::blemish_threshold(amount);
            let (mut healed, mut kept) = (0, 0);
            for f in &faces {
                for b in f["blemishes"].as_array().into_iter().flatten() {
                    if b["neck"] == true || (b["score"].as_f64().unwrap() as f32) < threshold {
                        continue;
                    }
                    healed += 1;
                    if reach(b["x"].as_f64().unwrap(), b["y"].as_f64().unwrap()) <= 0.5 {
                        kept += 1;
                    }
                }
            }
            // The fit-view preview of the protected recipe against the full
            // render reduced.
            let quarter = |img: &image::Rgb32FImage| {
                image::imageops::resize(
                    img,
                    img.width() / 4,
                    img.height() / 4,
                    image::imageops::FilterType::Triangle,
                )
            };
            let mut preview = quarter(&original);
            let preview_before = preview.clone();
            apply_face_retouch(&mut preview, &scaled_recipe(&protected_recipe, 0.25));
            let expected = quarter(&protected);
            let (mut diff, mut change) = (0.0f64, 0.0f64);
            for ((a, b), o) in preview
                .pixels()
                .zip(expected.pixels())
                .zip(preview_before.pixels())
            {
                for c in 0..3 {
                    diff += f64::from((a[c] - b[c]).abs());
                    change += f64::from((b[c] - o[c]).abs());
                }
            }
            let [x, y, w, h] = boxes[0];
            let pad = w.max(h) / 4.0;
            let (x0, y0) = ((x - pad).max(0.0) as u32, (y - pad).max(0.0) as u32);
            let cw = ((w + 2.0 * pad) as u32).min(image.width() - x0);
            let ch = ((h + 2.0 * pad) as u32).min(image.height() - y0);
            for (suffix, img) in [
                ("before", &original),
                ("unprotected", &unprotected),
                ("protected", &protected),
            ] {
                let crop = image::imageops::crop_imm(&to_srgb8(img), x0, y0, cw, ch).to_image();
                let mut bytes = std::io::Cursor::new(Vec::new());
                crop.write_to(&mut bytes, image::ImageFormat::Jpeg).unwrap();
                rawtonic_export::output::write_new_output(
                    out.join(format!("{name}_{suffix}.jpg")),
                    bytes.into_inner(),
                )
                .unwrap();
            }
            assert!(core > 0, "{name}: empty protection core");
            assert_eq!(core_changed, 0, "{name}: protected pixels changed");
            assert_eq!(
                outside_changed, 0,
                "{name}: protection leaked outside strokes"
            );
            assert!(
                rest_change > 0.0,
                "{name}: unprotected face was not retouched"
            );
            println!(
                "{name}: {} faces; core {core} px: {core_changed} values changed vs unretouched (retouch there without protection: mean |d| {:.5}); \
                 outside: {outside_changed} values differ from the unprotected render; rest of the faces: mean |d| {:.5} over {rest_n} px; \
                 feather {feather_n} px; healed blemishes {healed}, {kept} of them in the core kept; render {:.0} ms vs {:.0} ms without; \
                 quarter-size preview differs by {:.3} of the change",
                faces.len(),
                core_change_unprotected / (3.0 * core.max(1) as f64),
                rest_change / (3.0 * rest_n.max(1) as f64),
                protected_took.as_secs_f64() * 1e3,
                plain_took.as_secs_f64() * 1e3,
                diff / change.max(1e-9),
            );
        }
        assert!(measured > 0, "no photos with detected faces were measured");
        std::mem::forget(models);
    }
}
