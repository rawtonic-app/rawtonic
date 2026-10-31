// SPDX-License-Identifier: AGPL-3.0-or-later
//! Eyes per face for culling (G35, notes:
//! benchmark-results/fqb-eyes-2026-10-31/): the background pass that reads
//! every photograph of the open library with the face models
//! (`rawtonic_faces::eyes`, kept in the catalog's `face_eyes`), its progress
//! for the activity centre, each photograph's faces for the Loupe, the
//! Library's per-photograph summary and the photographer's own call per
//! face. A suggestion only: nothing here rates, flags, rejects or removes a
//! photograph.
//!
//! The pass reads the photograph as shot (upright, uncropped) from its
//! largest preview that needs no develop (a JPEG file itself, a RAW's
//! embedded camera JPEG; `source_large_preview_jpeg`). It runs only after
//! the photographer turned it on (`eye_check`) and with the face models
//! installed; it never downloads them by itself (PRIVACY.md).

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose};
use image::{ImageDecoder, ImageFormat, RgbImage};
use rawtonic_catalog::Library;
use rawtonic_catalog::face_eyes::{EyeFace, PhotoEyes};
use rawtonic_faces::{FaceModels, eyes};
use serde::Serialize;
use tauri::Emitter;

use crate::session_bridge::with_library;

/// `{directory, done, total}` while the pass runs.
pub const EYES_PROGRESS: &str = "eye-check-progress";
/// The pass stopped: `{directory, read, failed, cancelled}`.
pub const EYES_DONE: &str = "eye-check-done";
/// Readings or calls changed in the library at the payload's directory.
pub const EYES_CHANGED: &str = "library-eyes-changed";

/// Longest side of a preview the pass develops itself (a file without a
/// large enough embedded JPEG).
const PREVIEW_MAX: u32 = 4096;
/// How long the pass waits before reading (the library's first previews
/// come first) and between two photographs.
const PASS_DELAY: Duration = Duration::from_secs(3);
const PASS_PAUSE: Duration = Duration::from_millis(5);
/// Photographs taken from the catalog at a time; readings are announced
/// after each batch.
const PASS_BATCH: usize = 16;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// The running pass: its library and its cancel flag.
static PASS: Mutex<Option<(PathBuf, Arc<AtomicBool>)>> = Mutex::new(None);

/// The pass reports to the app from now on.
pub fn announce_to(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        let _ = app.emit(event, payload);
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    directory: &'a Path,
    done: usize,
    total: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Done<'a> {
    directory: &'a Path,
    read: usize,
    failed: usize,
    cancelled: bool,
}

/// The photograph at `path` upright, as the pass reads it.
fn upright_preview(path: &Path) -> Result<RgbImage, String> {
    let jpeg = crate::file_management::source_large_preview_jpeg(path, PREVIEW_MAX)
        .ok_or("No preview of this photograph could be read")?;
    let mut decoder = image::ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg)
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    Ok(image.into_rgb8())
}

/// Each face's eyes in `image`, left to right, boxes as fractions of it.
pub fn read_image(image: &RgbImage, models: &FaceModels) -> anyhow::Result<Vec<EyeFace>> {
    let analysis = rawtonic_faces::analyze(image, models)?;
    let (w, h) = (analysis.width as f32, analysis.height as f32);
    Ok(analysis
        .faces
        .iter()
        .map(|face| {
            let reading = eyes::read(face);
            let [x, y, fw, fh] = face.detection.bbox;
            EyeFace {
                bbox: [x / w, y / h, fw / w, fh / h],
                state: reading.state.as_str().into(),
                ear: reading.ear,
                accessories: reading.accessories,
            }
        })
        .collect())
}

/// Reads every photograph of the library at `directory` the catalog has no
/// current reading of, newest first, until done or `cancel` is set,
/// counting in `counts` how many were read and how many failed (also when
/// it stops on an error).
fn run_pass(
    directory: &Path,
    models: &FaceModels,
    cancel: &AtomicBool,
    counts: &mut (usize, usize),
) -> anyhow::Result<()> {
    let total = Library::open(directory)?.eye_backlog_count(eyes::VERSION)?;
    let (read, failed) = counts;
    // Photographs whose file is not there now (a drive going away while the
    // pass runs): left unread, not taken again in this pass.
    let mut skipped = std::collections::HashSet::new();
    emit(
        EYES_PROGRESS,
        Progress {
            directory,
            done: 0,
            total,
        },
    );
    'pass: while !cancel.load(Ordering::Acquire) {
        let lib = Library::open(directory)?;
        let work: Vec<_> = lib
            .eye_backlog(eyes::VERSION, PASS_BATCH + skipped.len())?
            .into_iter()
            .filter(|w| !skipped.contains(&w.id))
            .take(PASS_BATCH)
            .collect();
        if work.is_empty() {
            break;
        }
        for item in work {
            if cancel.load(Ordering::Acquire) {
                break 'pass;
            }
            if !item.path.is_file() {
                skipped.insert(item.id);
                continue;
            }
            crate::ai_lifecycle::LIFECYCLE.touch();
            let reading = {
                let _running = crate::ai_lifecycle::LIFECYCLE.run();
                upright_preview(&item.path)
                    .and_then(|image| read_image(&image, models).map_err(|e| e.to_string()))
            };
            match &reading {
                Ok(faces) => {
                    lib.record_eyes(&item.id, eyes::VERSION, &item.fingerprint, Ok(faces))?;
                    *read += 1;
                }
                Err(error) => {
                    log::warn!("Eyes not read for {}: {error}", item.path.display());
                    lib.record_eyes(&item.id, eyes::VERSION, &item.fingerprint, Err(error))?;
                    *failed += 1;
                }
            }
            emit(
                EYES_PROGRESS,
                Progress {
                    directory,
                    done: (*read + *failed).min(total),
                    total,
                },
            );
            std::thread::sleep(PASS_PAUSE);
        }
        emit(EYES_CHANGED, directory);
    }
    Ok(())
}

/// Starts the pass for the library at `directory` with `models` in the
/// background. A pass for another library is stopped; one running for
/// this library goes on.
fn start_pass(directory: PathBuf, models: Arc<FaceModels>) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut running) = PASS.lock() else {
            return;
        };
        if let Some((library, previous)) = running.as_ref() {
            if *library == directory && !previous.load(Ordering::Acquire) {
                return;
            }
            previous.store(true, Ordering::Release);
        }
        *running = Some((directory.clone(), cancel.clone()));
    }
    let spawned = std::thread::Builder::new()
        .name("eye-check".into())
        .spawn(move || {
            std::thread::sleep(PASS_DELAY);
            let started = Instant::now();
            let mut counts = (0, 0);
            if let Err(e) = run_pass(&directory, &models, &cancel, &mut counts) {
                log::warn!("Eye check paused: {e:#}");
            }
            let cancelled = cancel.swap(true, Ordering::AcqRel);
            let (read, failed) = counts;
            if read + failed > 0 {
                // Readings recorded since the last batch's announcement (a
                // Cancel or an error mid-batch) reach badges and filters too.
                emit(EYES_CHANGED, &directory);
                log::info!(
                    "Read the eyes of {read} photographs ({failed} failed) in {:.1} s",
                    started.elapsed().as_secs_f64()
                );
            }
            emit(
                EYES_DONE,
                Done {
                    directory: &directory,
                    read,
                    failed,
                    cancelled,
                },
            );
        });
    if let Err(e) = spawned {
        log::warn!("Eye check not started: {e}");
    }
}

/// Starts reading eyes in the library at `directory` (when there is
/// anything to read). `download`: the photographer asked for it (Check
/// Eyes), so missing face models are downloaded and the setting is turned
/// on; otherwise it runs only with the setting on and the models
/// installed, and says `face-models-missing` without them.
#[tauri::command]
pub async fn start_eye_check(
    directory: String,
    download: bool,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if !download {
        let settings = crate::app_settings::load_settings(app_handle.clone())?;
        if !settings.eye_check.unwrap_or(false) {
            stop_eye_check(None);
            return Ok(());
        }
    }
    stop_other_pass(Path::new(&directory));
    let models = crate::ai_faces::models(&app_handle, download).await?;
    let backlog = with_library(directory.clone(), false, |lib| {
        lib.eye_backlog_count(eyes::VERSION)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if backlog > 0 {
        start_pass(PathBuf::from(directory), models);
    }
    Ok(())
}

/// Stops a running pass of another library than `directory`: a library
/// opened after it never sees its progress, and it reads no more files.
fn stop_other_pass(directory: &Path) {
    if let Ok(running) = PASS.lock()
        && let Some((library, cancel)) = running.as_ref()
        && library != directory
    {
        cancel.store(true, Ordering::Release);
    }
}

/// Stops the running pass: Cancel in the activity centre and the check
/// turned off (any library), a library closing or moving (`directory`: only
/// that library's, never a pass started meanwhile for the next one). The
/// next start goes on.
#[tauri::command]
pub fn stop_eye_check(directory: Option<String>) {
    if let Ok(running) = PASS.lock()
        && let Some((library, cancel)) = running.as_ref()
        && directory.is_none_or(|d| Path::new(&d) == library)
    {
        cancel.store(true, Ordering::Release);
    }
}

/// The catalog's photograph at `path` (a virtual copy's `?vc=` path is its
/// photograph's).
fn asset_id(lib: &Library, path: &str) -> Result<String, String> {
    let file = path.split("?vc=").next().unwrap_or(path);
    lib.asset_at(Path::new(file))
        .map_err(|e| e.to_string())?
        .map(|a| a.id)
        .ok_or_else(|| "This photograph is not in the library".to_string())
}

/// One photograph's faces and their eyes (`faces` null while not read).
#[tauri::command]
pub async fn photo_eyes(directory: String, path: String) -> Result<PhotoEyes, String> {
    with_library(directory, false, move |lib| {
        let id = asset_id(lib, &path)?;
        lib.photo_eyes(&id, eyes::VERSION)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the Library shows on a photograph: its faces and how many of
/// them have their eyes closed (or partly), hidden, or need a check, by
/// the photographer's call where made.
#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EyeSummary {
    pub faces: usize,
    pub closed: usize,
    pub not_visible: usize,
    pub uncertain: usize,
}

/// `EyeSummary` of each of `paths` that has a current reading (keyed by
/// the path as given).
#[tauri::command]
pub async fn photo_eye_summaries(
    directory: String,
    paths: Vec<String>,
) -> Result<HashMap<String, EyeSummary>, String> {
    with_library(directory, false, move |lib| {
        let mut ids = Vec::new();
        for path in &paths {
            if let Ok(id) = asset_id(lib, path) {
                ids.push((path, id));
            }
        }
        let read = lib
            .photos_eyes(
                &ids.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>(),
                eyes::VERSION,
            )
            .map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        for (path, id) in ids {
            let Some(faces) = read.get(&id).and_then(|p| p.faces.as_ref()) else {
                continue;
            };
            let count = |state: &str| faces.iter().filter(|f| f.effective() == state).count();
            out.insert(
                path.clone(),
                EyeSummary {
                    faces: faces.len(),
                    closed: count("closed"),
                    not_visible: count("notVisible"),
                    uncertain: count("uncertain"),
                },
            );
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Sets the photographer's call on face `face` of the photograph at
/// `path` (`open`, `closed`, `notVisible`), or with `None` gives the
/// reading back. Announces the change, so filters follow it.
#[tauri::command]
pub async fn set_face_eyes(
    directory: String,
    path: String,
    face: u32,
    state: Option<String>,
) -> Result<(), String> {
    let changed = PathBuf::from(&directory);
    with_library(directory, true, move |lib| {
        let id = asset_id(lib, &path)?;
        lib.set_eye_override(&id, face, state.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    emit(EYES_CHANGED, changed);
    Ok(())
}

/// A crop of each of `boxes` (fractions of the upright photograph) as a
/// small JPEG data URL, for the Loupe's faces: the eyes large enough to
/// check the call.
#[tauri::command]
pub async fn face_eye_crops(path: String, boxes: Vec<[f32; 4]>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let file = path.split("?vc=").next().unwrap_or(&path).to_string();
        let image = upright_preview(Path::new(&file))?;
        let (w, h) = (image.width() as f32, image.height() as f32);
        boxes
            .iter()
            .map(|&[x, y, bw, bh]| {
                // The face box widened a little: brows to chin, ear to ear.
                let (cx, cy) = ((x + bw / 2.0) * w, (y + bh / 2.0) * h);
                let side = (bw * w).max(bh * h) * 1.3;
                let left = (cx - side / 2.0).clamp(0.0, w - 1.0);
                let top = (cy - side / 2.0).clamp(0.0, h - 1.0);
                let width = side.min(w - left).max(1.0);
                let height = side.min(h - top).max(1.0);
                let crop = image::imageops::crop_imm(
                    &image,
                    left as u32,
                    top as u32,
                    width as u32,
                    height as u32,
                )
                .to_image();
                let size = 160;
                let crop = image::imageops::resize(
                    &crop,
                    size,
                    (size as f32 * height / width).round().max(1.0) as u32,
                    image::imageops::FilterType::Lanczos3,
                );
                let mut jpeg = Cursor::new(Vec::new());
                crop.write_to(&mut jpeg, ImageFormat::Jpeg)
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "data:image/jpeg;base64,{}",
                    general_purpose::STANDARD.encode(jpeg.get_ref())
                ))
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use tauri::async_runtime::block_on;

    #[test]
    fn summaries_and_calls_reach_the_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Eyes", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture").join("A.JPG");
        std::fs::write(&photo, b"synthetic photo").unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let face = |state: &str| EyeFace {
            bbox: [0.1, 0.1, 0.1, 0.1],
            state: state.into(),
            ear: Some(0.1),
            accessories: Some(0.0),
        };
        lib.record_eyes(
            &asset.id,
            eyes::VERSION,
            &asset.fingerprint,
            Ok(&[face("closed"), face("uncertain"), face("open")]),
        )
        .unwrap();
        let directory = lib.directory.to_string_lossy().into_owned();
        let path = photo.to_string_lossy().into_owned();
        let summaries =
            |paths: Vec<String>| block_on(photo_eye_summaries(directory.clone(), paths)).unwrap();
        let summary = EyeSummary {
            faces: 3,
            closed: 1,
            not_visible: 0,
            uncertain: 1,
        };
        assert_eq!(summaries(vec![path.clone()])[&path], summary);
        // A virtual copy is its photograph; an unknown file has no summary.
        let copy = format!("{path}?vc=abc");
        assert_eq!(summaries(vec![copy.clone()])[&copy], summary);
        assert!(summaries(vec!["/nowhere/B.JPG".into()]).is_empty());

        block_on(set_face_eyes(
            directory.clone(),
            path.clone(),
            1,
            Some("closed".into()),
        ))
        .unwrap();
        assert_eq!(summaries(vec![path.clone()])[&path].closed, 2);
        assert!(
            block_on(set_face_eyes(
                directory.clone(),
                path.clone(),
                1,
                Some("uncertain".into())
            ))
            .is_err()
        );
        let eyes = block_on(photo_eyes(directory, path)).unwrap();
        assert_eq!(
            eyes.faces.unwrap()[1].override_state.as_deref(),
            Some("closed")
        );
    }

    #[test]
    fn a_stop_names_the_library_it_stops() {
        let cancel = Arc::new(AtomicBool::new(false));
        *PASS.lock().unwrap() = Some((PathBuf::from("/A"), cancel.clone()));
        // Library B opening or closing leaves A's pass alone…
        stop_eye_check(Some("/B".into()));
        stop_other_pass(Path::new("/A"));
        assert!(!cancel.load(Ordering::Acquire));
        // …A closing, B starting, or Cancel stop it.
        stop_eye_check(Some("/A".into()));
        assert!(cancel.swap(false, Ordering::AcqRel));
        stop_other_pass(Path::new("/B"));
        assert!(cancel.swap(false, Ordering::AcqRel));
        stop_eye_check(None);
        assert!(cancel.load(Ordering::Acquire));
        *PASS.lock().unwrap() = None;
    }

    /// Reads the eyes of every photograph in `FACE_PHOTOS` (one level of
    /// subfolders) exactly as the pass does (preview, upright, analysis,
    /// call), writing one JSON line per photograph to `FACE_OUT` (a new file) with the
    /// time per step (notes: benchmark-results/fqb-eyes-2026-10-31/).
    /// Needs `FACE_MODELS` and `ORT_DYLIB_PATH`.
    ///
    /// cargo test -p rawtonic-app --release --lib reads_eyes_of_real_photos -- --ignored --nocapture
    #[test]
    #[ignore]
    fn reads_eyes_of_real_photos() {
        use std::io::Write;
        let env = |name| std::env::var_os(name).map(PathBuf::from);
        let (Some(models), Some(photos), Some(out)) =
            (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
        else {
            eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
            return;
        };
        let models = FaceModels::load(&models).unwrap();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&photos)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .flat_map(|dir| {
                std::fs::read_dir(dir)
                    .unwrap()
                    .filter_map(|e| e.ok().map(|e| e.path()))
            })
            .filter(|p| rawtonic_engine::formats::is_supported_image_file(p))
            .collect();
        files.sort();
        // Published whole at the end as a new file (never over an existing
        // one, such as a photograph; nothing partial after a failed run).
        assert!(
            std::fs::symlink_metadata(&out).is_err(),
            "FACE_OUT exists: {}",
            out.display()
        );
        let mut lines = tempfile::NamedTempFile::new_in(
            out.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .unwrap();
        for file in files {
            let start = Instant::now();
            let image = upright_preview(&file).unwrap();
            let preview_ms = start.elapsed().as_secs_f64() * 1e3;
            let start = Instant::now();
            let faces = read_image(&image, &models).unwrap();
            let read_ms = start.elapsed().as_secs_f64() * 1e3;
            let name = file.strip_prefix(&photos).unwrap().to_string_lossy();
            println!(
                "{name}: {} faces, preview {preview_ms:.0} ms, read {read_ms:.0} ms",
                faces.len()
            );
            let line = serde_json::json!({
                "file": name, "width": image.width(), "height": image.height(),
                "preview_ms": preview_ms, "read_ms": read_ms, "faces": faces,
            });
            writeln!(lines, "{line}").unwrap();
        }
        lines.as_file().sync_all().unwrap();
        rawtonic_metadata::no_clobber::persist(lines, &out).unwrap();
    }
}
