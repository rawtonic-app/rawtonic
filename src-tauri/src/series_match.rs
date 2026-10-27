//! Match a series (`rawtonic_engine::series_match`): decodes each photo,
//! finds the skin of its faces when the face models are installed, and
//! proposes Exposure and white balance against the reference photos. The
//! proposals are written by `file_management::set_recipe_values` (one undo
//! step in the frontend); previews render through `snapshot_thumbnail`.
use rawtonic_engine::series_match::{
    self, Capture, MatchOptions, PhotoStats, Proposal, SeriesPhoto, WhiteBalance,
};
use rawtonic_export::sidecars::parse_virtual_path;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// Long edge of the image faces are looked for in.
const FACE_SIZE: u32 = 1280;
const MAX_FACES: usize = 8;

/// Statistics of a file, kept while the app runs: matching again (other
/// references, after edits) does not decode again.
#[derive(Clone)]
struct Analysed {
    stamp: (u64, u128, bool),
    stats: Arc<PhotoStats>,
    capture: Capture,
}
static CACHE: Mutex<Option<HashMap<String, Analysed>>> = Mutex::new(None);
static RUN: AtomicUsize = AtomicUsize::new(0);

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeriesMatchResult {
    pub proposals: Vec<Proposal>,
    /// "used", "missing" (not installed and not to be downloaded) or "off".
    pub face_models: &'static str,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    done: usize,
    total: usize,
}

fn number(value: &str) -> Option<f64> {
    let value = value.trim().trim_end_matches('s').trim();
    if let Some((n, d)) = value.split_once('/') {
        let (n, d) = (number(n)?, number(d)?);
        return (d != 0.0).then(|| n / d);
    }
    value
        .trim_start_matches("f/")
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
}

/// Capture facts from the file's EXIF fields.
pub(crate) fn capture_from_exif(exif: &HashMap<String, String>) -> Capture {
    let get = |key: &str| {
        exif.get(key)
            .map(|v| v.trim().trim_matches('"').to_string())
    };
    let time = get("DateTimeOriginal")
        .or_else(|| get("DateTime"))
        .and_then(|v| rawtonic_export::exif_processing::parse_creation_datetime(&v))
        .map(|dt| {
            let subsec = get("SubSecTimeOriginal")
                .and_then(|s| format!("0.{}", s.trim()).parse::<f64>().ok())
                .unwrap_or(0.0);
            dt.and_utc().timestamp() as f64 + subsec
        });
    let shutter = get("ExposureTime")
        .and_then(|v| number(&v))
        .filter(|v| *v > 0.0);
    let aperture = get("FNumber").and_then(|v| number(&v)).filter(|v| *v > 0.0);
    let iso = ["PhotographicSensitivity", "ISOSpeed", "ISOSpeedRatings"]
        .iter()
        .find_map(|key| get(key).and_then(|v| number(&v)))
        .filter(|v| *v > 0.0);
    let exposure_value = match (shutter, aperture, iso) {
        (Some(t), Some(n), Some(iso)) => Some(((n * n / t).log2() - (iso / 100.0).log2()) as f32),
        _ => None,
    };
    let auto_bracket = get("ExposureMode")
        .is_some_and(|v| v.trim() == "2" || v.to_ascii_lowercase().contains("bracket"));
    Capture {
        time,
        exposure_value,
        bias: get("ExposureBiasValue")
            .and_then(|v| number(&v))
            .map(|v| v as f32),
        auto_bracket,
        camera: get("Model").unwrap_or_default(),
    }
}

fn stamp(path: &std::path::Path, faces: bool) -> (u64, u128, bool) {
    let meta = std::fs::metadata(path).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    (meta.map_or(0, |m| m.len()), modified, faces)
}

/// Decodes `source` and measures it (skin with `faces`); also the faces'
/// boxes (0..1 across the image) for the evaluation.
pub(crate) fn analyse_file(
    source: &std::path::Path,
    settings: &crate::app_settings::AppSettings,
    faces: Option<&rawtonic_faces::FaceModels>,
) -> Result<(PhotoStats, Capture, Vec<[f32; 4]>), String> {
    let path = source.to_string_lossy().to_string();
    // Mapped: EXIF reads a few pages, a proxy spares reading the rest.
    let bytes = crate::file_management::read_file_mapped(source).map_err(|e| e.to_string())?;
    let exif = crate::exif_processing::read_exif_data_from_bytes(&path, &bytes);
    let capture = capture_from_exif(&exif);
    // The Standard preview proxy (unedited linear data at about screen
    // size, made on import or when the photo was opened) is enough and
    // spares the decode.
    let image = match crate::preview_proxy::read(source, settings) {
        Some(proxy) => proxy.image,
        None => {
            crate::image_loader::load_base_image_from_bytes(&bytes, &path, true, settings, None)
                .map_err(|e| e.to_string())?
        }
    };
    drop(bytes);
    measure_image(&path, image, capture, faces)
}

/// `analyse_file` from a decoded (or proxy) image.
fn measure_image(
    path: &str,
    image: image::DynamicImage,
    capture: Capture,
    faces: Option<&rawtonic_faces::FaceModels>,
) -> Result<(PhotoStats, Capture, Vec<[f32; 4]>), String> {
    let path = path.to_string();
    let found = faces.and_then(|models| {
        let small = crate::image_processing::thumbnail(&image, FACE_SIZE, FACE_SIZE);
        let rgb = rawtonic_faces::recipe::display_rgb8(&small, true);
        let (w, h) = rgb.dimensions();
        match rawtonic_faces::face_skin(&rgb, models, MAX_FACES) {
            Ok(found) => Some((found, w as f32, h as f32)),
            Err(e) => {
                log::warn!("Series match: face detection failed for {path}: {e:#}");
                None
            }
        }
    });
    let skin_at = |x: f32, y: f32| -> f32 {
        let Some((found, w, h)) = &found else {
            return 0.0;
        };
        let (px, py) = (x * w, y * h);
        found
            .iter()
            .filter(|(d, _)| {
                let [bx, by, bw, bh] = d.bbox;
                px >= bx && px <= bx + bw && py >= by && py <= by + bh
            })
            .map(|(_, parse)| {
                parse
                    .sample(rawtonic_faces::parse::Class::FaceSkin, px, py)
                    .0
            })
            .fold(0.0, f32::max)
    };
    let face_count = found.as_ref().map_or(0, |(f, _, _)| f.len() as u32);
    let light = |rgb: [f32; 3]| {
        rawtonic_engine::camera_profile::white_balance_for_sample(&path, rgb)
            .map(|(temperature, tint)| WhiteBalance { temperature, tint })
    };
    let skin: Option<series_match::SkinMap> =
        (face_count > 0).then_some((&skin_at as &dyn Fn(f32, f32) -> f32, face_count));
    let stats = series_match::photo_stats(&image, skin, &light)
        .ok_or_else(|| "No usable pixels".to_string())?;
    let boxes = found.as_ref().map_or_else(Vec::new, |(found, w, h)| {
        found
            .iter()
            .map(|(d, _)| {
                let [x, y, bw, bh] = d.bbox;
                [x / w, y / h, (x + bw) / w, (y + bh) / h]
            })
            .collect()
    });
    Ok((stats, capture, boxes))
}

/// The recipe's Exposure and white balance (As Shot resolved) of `path`.
fn recipe_facts(path: &str) -> (f32, Option<WhiteBalance>) {
    let (source, sidecar) = parse_virtual_path(path);
    let recipe = crate::exif_processing::load_sidecar(&sidecar).adjustments;
    let exposure = recipe["exposure"].as_f64().unwrap_or(0.0) as f32;
    let source = source.to_string_lossy();
    let white_balance = crate::image_processing::recipe_white_balance(&recipe)
        .or_else(|| rawtonic_engine::camera_profile::as_shot_white_balance(&source))
        .map(|(temperature, tint)| WhiteBalance { temperature, tint });
    (exposure, white_balance)
}

/// Proposals for `paths` against `references` (a subset of them).
/// `download_face_models`: the user asked for faces to be used (PRIVACY.md:
/// models are only downloaded on request); without it, faces are used only
/// when the models are installed.
#[tauri::command]
pub async fn analyze_series_match(
    paths: Vec<String>,
    references: Vec<String>,
    options: MatchOptions,
    use_faces: bool,
    download_face_models: bool,
    app_handle: tauri::AppHandle,
) -> Result<SeriesMatchResult, String> {
    let started = std::time::Instant::now();
    let run = RUN.fetch_add(1, Ordering::SeqCst) + 1;
    let (models, face_models) = if use_faces {
        match crate::ai_faces::models(&app_handle, download_face_models).await {
            Ok(models) => (Some(models), "used"),
            Err(e) if e == crate::ai_faces::FACE_MODELS_MISSING => (None, "missing"),
            Err(e) => {
                log::warn!("Series match without faces: {e}");
                (None, "missing")
            }
        }
    } else {
        (None, "off")
    };
    let app = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::load_settings(app.clone()).unwrap_or_default();
        let total = paths.len();
        let done = AtomicUsize::new(0);
        let photos: Vec<Option<SeriesPhoto>> = paths
            .par_iter()
            .map(|path| {
                if RUN.load(Ordering::SeqCst) != run {
                    return None;
                }
                let (source, _) = parse_virtual_path(path);
                let key = source.to_string_lossy().to_string();
                let stamp = stamp(&source, models.is_some());
                let cached = CACHE
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|cache| cache.get(&key).cloned())
                    .filter(|a| a.stamp == stamp);
                let analysed = match cached {
                    Some(a) => Ok(a),
                    None => analyse_file(&source, &settings, models.as_deref()).map(
                        |(stats, capture, _)| {
                            let a = Analysed {
                                stamp,
                                stats: Arc::new(stats),
                                capture,
                            };
                            CACHE
                                .lock()
                                .unwrap()
                                .get_or_insert_with(HashMap::new)
                                .insert(key.clone(), a.clone());
                            a
                        },
                    ),
                };
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit("series-match-progress", Progress { done: n, total });
                let (exposure, white_balance) = recipe_facts(path);
                let baseline_ev = rawtonic_engine::baseline_exposure::base_look(&key).baseline_ev;
                let reference = references.contains(path);
                Some(match analysed {
                    Ok(a) => SeriesPhoto {
                        path: path.clone(),
                        stats: Some((*a.stats).clone()),
                        capture: a.capture,
                        baseline_ev,
                        exposure,
                        white_balance,
                        reference,
                        error: None,
                    },
                    Err(e) => {
                        log::warn!("Series match: {path}: {e}");
                        SeriesPhoto {
                            path: path.clone(),
                            stats: None,
                            capture: Capture::default(),
                            baseline_ev,
                            exposure,
                            white_balance,
                            reference,
                            error: Some(e),
                        }
                    }
                })
            })
            .collect();
        let cancelled = RUN.load(Ordering::SeqCst) != run || photos.iter().any(Option::is_none);
        let photos: Vec<SeriesPhoto> = photos.into_iter().flatten().collect();
        let proposals = if cancelled {
            Vec::new()
        } else {
            series_match::plan(&photos, options)
        };
        SeriesMatchResult {
            proposals,
            face_models,
            elapsed_ms: started.elapsed().as_millis() as u64,
            cancelled,
        }
    })
    .await
    .map_err(|e| format!("Series match failed: {e}"))
}

/// Stops a running analysis (its result comes back marked cancelled).
#[tauri::command]
pub fn cancel_series_match() {
    RUN.fetch_add(1, Ordering::SeqCst);
}

/// Forgets the statistics (memory) once the dialog closes.
#[tauri::command]
pub fn clear_series_match_cache() {
    *CACHE.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_facts_from_exif_fields() {
        let exif: HashMap<String, String> = [
            ("DateTimeOriginal", "2026:07:18 19:55:43"),
            ("ExposureTime", "1/125 s"),
            ("FNumber", "2.8"),
            ("PhotographicSensitivity", "200"),
            ("ExposureBiasValue", "-0.33"),
            ("ExposureMode", "2"),
            ("Model", "Camera"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let c = capture_from_exif(&exif);
        assert!(c.time.is_some());
        let ev = (2.8f64 * 2.8 * 125.0).log2() - 1.0;
        assert!((c.exposure_value.unwrap() as f64 - ev).abs() < 1e-4);
        assert_eq!(c.bias, Some(-0.33));
        assert!(c.auto_bracket);
        assert_eq!(c.camera, "Camera");
    }

    /// Evaluation (private series): RAWTONIC_SERIES lists one series per
    /// line, "name<TAB>reference path<TAB>path<TAB>path…"; prints per photo
    /// the proposal and the rendered median before/after, and the timing.
    /// RAWTONIC_SERIES_FACES=DIR uses the face models in DIR.
    #[test]
    #[ignore = "requires private RAW series"]
    fn private_series_match_evaluation() {
        let Ok(list) = std::env::var("RAWTONIC_SERIES") else {
            eprintln!("SKIP: RAWTONIC_SERIES not set");
            return;
        };
        let settings = crate::app_settings::AppSettings::default();
        let models = std::env::var("RAWTONIC_SERIES_FACES")
            .ok()
            .map(|dir| rawtonic_faces::FaceModels::load(std::path::Path::new(&dir)).unwrap());
        let out = std::env::var("RAWTONIC_SERIES_OUT").ok();
        let mut report = serde_json::Map::new();
        for line in std::fs::read_to_string(list).unwrap().lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                continue;
            }
            let (name, references) = (parts[0], parts[1]);
            let started = std::time::Instant::now();
            let photos: Vec<(SeriesPhoto, Vec<[f32; 4]>)> = parts[2..]
                .par_iter()
                .map(|path| {
                    let analysed =
                        analyse_file(std::path::Path::new(path), &settings, models.as_ref());
                    let wb = rawtonic_engine::camera_profile::as_shot_white_balance(path)
                        .map(|(temperature, tint)| WhiteBalance { temperature, tint });
                    let (stats, capture, boxes, error) = match analysed {
                        Ok((s, c, b)) => (Some(s), c, b, None),
                        Err(e) => (None, Capture::default(), Vec::new(), Some(e)),
                    };
                    (
                        SeriesPhoto {
                            path: path.to_string(),
                            stats,
                            capture,
                            baseline_ev: rawtonic_engine::baseline_exposure::base_look(path)
                                .baseline_ev,
                            exposure: 0.0,
                            white_balance: wb,
                            reference: references.split(';').any(|r| r == *path),
                            error,
                        },
                        boxes,
                    )
                })
                .collect();
            let (photos, boxes): (Vec<SeriesPhoto>, Vec<Vec<[f32; 4]>>) =
                photos.into_iter().unzip();
            let analysed = started.elapsed();
            // The same photos from Standard preview proxies (encoded here
            // as the app stores them): reading a proxy and measuring it.
            if std::env::var("RAWTONIC_SERIES_PROXY_BENCH").is_ok() {
                let proxies: Vec<(String, Vec<u8>, crate::preview_proxy::ProxyKey)> = parts[2..]
                    .par_iter()
                    .filter_map(|path| {
                        let bytes = std::fs::read(path).ok()?;
                        let image = crate::image_loader::load_base_image_from_bytes(
                            &bytes, path, true, &settings, None,
                        )
                        .ok()?;
                        let size = crate::preview_proxy::standard_size(0, &settings);
                        let small = crate::image_processing::thumbnail(&image, size, size);
                        let key = crate::preview_proxy::ProxyKey {
                            file_size: 0,
                            modified_ns: 0,
                            decoder: String::new(),
                            engine: 0,
                        };
                        let encoded = crate::preview_proxy::encode(
                            &small,
                            image.width(),
                            image.height(),
                            key.clone(),
                        );
                        Some((path.to_string(), encoded, key))
                    })
                    .collect();
                let started = std::time::Instant::now();
                let measured = proxies
                    .par_iter()
                    .filter(|(path, encoded, key)| {
                        let Some(proxy) = crate::preview_proxy::decode(encoded, key) else {
                            return false;
                        };
                        let bytes = std::fs::read(path).unwrap();
                        let capture = capture_from_exif(
                            &crate::exif_processing::read_exif_data_from_bytes(path, &bytes),
                        );
                        measure_image(path, proxy.image, capture, models.as_ref()).is_ok()
                    })
                    .count();
                let took = started.elapsed().as_secs_f64();
                println!(
                    "PROXIES {name}: {measured} photos from proxies in {took:.2} s ({:.0} ms/photo wall)",
                    took * 1000.0 / measured.max(1) as f64
                );
            }
            let both = MatchOptions {
                exposure: true,
                white_balance: true,
            };
            let proposals = series_match::plan(&photos, both);
            println!(
                "SERIES {name}: {} photos, analysis {:.2} s ({:.0} ms/photo wall)",
                photos.len(),
                analysed.as_secs_f64(),
                analysed.as_secs_f64() * 1000.0 / photos.len() as f64
            );
            let mut rows = Vec::new();
            for ((photo, proposal), faces) in photos.iter().zip(&proposals).zip(&boxes) {
                let before = photo.stats.as_ref().and_then(|s| {
                    series_match::rendered_median(s, photo.baseline_ev, photo.exposure)
                });
                let after = photo.stats.as_ref().and_then(|s| {
                    series_match::rendered_median(
                        s,
                        photo.baseline_ev,
                        proposal.exposure.unwrap_or(photo.exposure),
                    )
                });
                println!(
                    "  {} {:?} {:?} exp {:?} wb {:?} conf {:.2} skin {:?} median {:?} -> {:?}",
                    std::path::Path::new(&photo.path)
                        .file_name()
                        .unwrap()
                        .to_string_lossy(),
                    proposal.status,
                    proposal.reasons,
                    proposal.exposure,
                    proposal.white_balance,
                    proposal.confidence,
                    photo
                        .stats
                        .as_ref()
                        .and_then(|s| s.skin.as_ref())
                        .map(|s| (s.faces, s.pixels)),
                    before,
                    after
                );
                rows.push(serde_json::json!({
                    "photo": photo, "proposal": proposal, "faces": faces,
                    "medianBefore": before, "medianAfter": after,
                }));
            }
            report.insert(
                name.to_string(),
                serde_json::json!({ "analysisSeconds": analysed.as_secs_f64(), "rows": rows }),
            );
        }
        if let Some(out) = out {
            std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
        }
    }
}
