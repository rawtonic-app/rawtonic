//! Desktop glue for `rawtonic_engine::spot_removal` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::spot_removal::*;

use serde_json::Value;

/// The automatic source for a spot about to be placed on the open photo:
/// mask-frame centre and radius in, mask-frame source centre out (the
/// recipe's existing patches and spots already applied). With `points`
/// (two or more), a brush stroke along them: where its first point copies
/// from.
#[tauri::command]
pub async fn find_spot_source(
    x: f64,
    y: f64,
    radius: f64,
    points: Option<Vec<StrokePoint>>,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(f64, f64), String> {
    let (source, _) = crate::editor_preview::original_source(&state).await?;
    tokio::task::spawn_blocking(move || {
        let base = crate::image_loader::composite_patches_on_image(&source, &js_adjustments)
            .map_err(|e| e.to_string())?;
        match points.filter(|p| p.len() >= 2) {
            Some(points) => find_stroke_source(&base, &js_adjustments, &points, radius),
            None => find_source(&base, &js_adjustments, x, y, radius),
        }
        .ok_or_else(|| "No source area fits around this spot".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Visualize Spots for the open photo: a JPEG of the band-pass view of the
/// displayed (transformed, cropped) photo with its spots applied, at most
/// `max_edge` pixels long. The band-pass is cached per photo state, so the
/// sensitivity slider only re-thresholds.
#[tauri::command]
pub async fn visualize_spots(
    sensitivity: f64,
    max_edge: u32,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<tauri::ipc::Response, String> {
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (std::sync::Arc::as_ptr(&source) as usize).hash(&mut hasher);
        crate::cache_utils::calculate_transform_hash(&js_adjustments).hash(&mut hasher);
        crate::cache_utils::calculate_patched_warped_hash(&js_adjustments).hash(&mut hasher);
        max_edge.hash(&mut hasher);
        hasher.finish()
    };
    let cached = band_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(k, _)| *k == key)
        .map(|(_, band)| band.clone());
    let band = match cached {
        Some(band) => band,
        None => {
            let warped = crate::editor_preview::get_cached_full_warped_image(
                &state,
                &source,
                is_raw,
                &js_adjustments,
            )?;
            let adjustments = js_adjustments.clone();
            let band = tokio::task::spawn_blocking(move || {
                let (transformed, _) = crate::adjustment_utils::apply_spatial_transformations(
                    warped.as_ref(),
                    &adjustments,
                );
                std::sync::Arc::new(spot_band_pass(&transformed, max_edge))
            })
            .await
            .map_err(|e| e.to_string())?;
            *band_cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((key, band.clone()));
            band
        }
    };
    let view = visualize(&band.0, band.1, sensitivity);
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(view)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(jpeg.into_inner()))
}

/// What Find Dust found on the open photo (`find_dust`).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundDust {
    /// With `apply`: heal spots for `adjustments.spots` (the editor gives
    /// each its id and settings); otherwise the spots to preview (mask-frame
    /// centre and radius).
    pub spots: Vec<Value>,
    /// Whether faces were left out (the face models are installed).
    pub faces_checked: bool,
    /// How many other photos of the camera the spots were checked against
    /// (the multi-photo cue, `rawtonic_engine::dust::SeriesTarget`).
    pub series_photos: usize,
}

/// The open photo's analysis (with the series' evidence), whether faces
/// were checked, and how many other photos gave evidence.
type DustCache = std::sync::Mutex<
    Option<(
        u64,
        std::sync::Arc<rawtonic_engine::dust::DustAnalysis>,
        bool,
        usize,
    )>,
>;

fn dust_cache() -> &'static DustCache {
    static CACHE: std::sync::OnceLock<DustCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The face boxes of `base` (base pixels, x0, y0, x1, y1) when the face
/// models are installed (Find Dust never downloads them), else None.
async fn face_boxes(
    app_handle: &tauri::AppHandle,
    base: std::sync::Arc<image::DynamicImage>,
    is_raw: bool,
) -> Option<Vec<[f64; 4]>> {
    let models = crate::ai_faces::models(app_handle, false).await.ok()?;
    tokio::task::spawn_blocking(move || {
        let long = base.width().max(base.height());
        let scale = (1600.0 / f64::from(long)).min(1.0);
        let small = if scale < 1.0 {
            base.resize(
                (f64::from(base.width()) * scale).round() as u32,
                (f64::from(base.height()) * scale).round() as u32,
                image::imageops::FilterType::Triangle,
            )
        } else {
            (*base).clone()
        };
        let image = rawtonic_faces::recipe::display_rgb8(&small, is_raw);
        let found = models.detect(&image).ok()?;
        Some(
            found
                .iter()
                .map(|d| {
                    let [x, y, w, h] = d.bbox.map(|v| f64::from(v) / scale);
                    [x, y, x + w, y + h]
                })
                .collect(),
        )
    })
    .await
    .ok()
    .flatten()
}

/// Find Dust (Lightroom's Dust > Apply) on the open photo: the dust spots
/// at `sensitivity` where the photo shows them, not on spots it has
/// (`rawtonic_engine::dust`). The analysis is cached per photo state, so
/// moving the sensitivity only filters. With `apply`, the spots get their
/// automatic sources and their sensor position (`dust`) for syncing.
#[tauri::command]
pub async fn find_dust(
    sensitivity: f64,
    apply: bool,
    js_adjustments: Value,
    series: Option<Vec<String>>,
    state: tauri::State<'_, crate::app_state::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<FoundDust, String> {
    let loaded = crate::image_loader::original_image(&state).await?;
    let (source, is_raw, path) = (loaded.image, loaded.is_raw, loaded.path);
    // The other selected photos of the same camera and frame that can
    // witness the dust (`series_of`).
    let series = {
        let (open, adjustments) = (path.clone(), js_adjustments.clone());
        let paths = series.unwrap_or_default();
        tokio::task::spawn_blocking(move || series_of(&open, &adjustments, paths))
            .await
            .map_err(|e| e.to_string())?
    };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (std::sync::Arc::as_ptr(&source) as usize).hash(&mut hasher);
        for retouch in ["aiPatches", "spots", "redEyes"] {
            js_adjustments[retouch].to_string().hash(&mut hasher);
        }
        series.hash(&mut hasher);
        hasher.finish()
    };
    let cached = dust_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(k, ..)| *k == key)
        .map(|(_, analysis, faces, photos)| (analysis.clone(), *faces, *photos));
    // The photo with its patches and spots: what the analysis and the
    // sources look at (healed dust is not found again).
    let base = if cached.is_none() || apply {
        let (source, adjustments) = (source.clone(), js_adjustments.clone());
        let base = tokio::task::spawn_blocking(move || {
            crate::image_loader::composite_patches_on_image(&source, &adjustments)
                .map(std::sync::Arc::new)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        Some(base)
    } else {
        None
    };
    let (analysis, faces_checked, series_photos) = match (cached, &base) {
        (Some(found), _) => found,
        (None, None) => return Err("No photo analysis".into()),
        (None, Some(base)) => {
            // Faces as decoded: the patches do not move them.
            let faces = face_boxes(&app_handle, source.clone(), is_raw).await;
            let checked = faces.is_some();
            let (base, open) = (base.clone(), path.clone());
            let (analysis, photos) = tokio::task::spawn_blocking(move || {
                let faces = faces.unwrap_or_default();
                let work = rawtonic_engine::dust::work_copy(&base, !is_raw);
                let size = (base.width(), base.height());
                let analysis = rawtonic_engine::dust::analyse_work(&work, size, &faces);
                let (analysis, photos) = with_series(&open, work, size, analysis, &series, &faces);
                (std::sync::Arc::new(analysis), photos)
            })
            .await
            .map_err(|e| e.to_string())?;
            *dust_cache().lock().unwrap_or_else(|e| e.into_inner()) =
                Some((key, analysis.clone(), checked, photos));
            (analysis, checked, photos)
        }
    };
    tokio::task::spawn_blocking(move || {
        let found = rawtonic_engine::dust::placed(&analysis, sensitivity, &js_adjustments);
        let spots = if let Some(base) = base.filter(|_| apply) {
            let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(&path);
            let bytes = std::fs::read(&source_path).map_err(|e| e.to_string())?;
            let orientation =
                crate::image_loader::source_orientation(&bytes, &source_path.to_string_lossy());
            rawtonic_engine::dust::heal_spots(
                &base,
                &js_adjustments,
                &analysis,
                &found,
                orientation,
            )
        } else {
            found
                .iter()
                .map(|s| serde_json::to_value(s).unwrap_or_default())
                .collect()
        };
        Ok(FoundDust {
            spots,
            faces_checked,
            series_photos,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// At most this many other photos witness the dust (the nearest to the
/// open photo in the selection's order): enough evidence, and the time and
/// memory stay bounded.
const SERIES_MAX: usize = 8;

/// What the multi-photo cue keeps of a photo of the series (its work copy
/// and analysis, decoded once while the app runs).
#[derive(Clone)]
struct SeriesEntry {
    stamp: (u64, u128),
    work: std::sync::Arc<rawtonic_engine::dust::WorkCopy>,
    analysis: std::sync::Arc<rawtonic_engine::dust::DustAnalysis>,
    size: (u32, u32),
    orientation: u16,
    f_number: Option<f32>,
}

/// The series photos last used, by path (at most `SERIES_MAX`).
static SERIES_CACHE: std::sync::Mutex<Vec<(String, SeriesEntry)>> =
    std::sync::Mutex::new(Vec::new());

fn file_stamp(path: &std::path::Path) -> (u64, u128) {
    let meta = std::fs::metadata(path).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    (meta.map_or(0, |m| m.len()), modified)
}

/// A photo's f-number and body serial number from its metadata.
fn aperture_and_body(path: &str) -> (Option<f32>, Option<String>) {
    let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(path);
    let meta = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path);
    let Some(exif) = meta.exif else {
        return (None, None);
    };
    let field = |k: &str| {
        exif.get(k)
            .map(|v| v.trim().trim_matches('"').trim().to_string())
            .filter(|v| !v.is_empty() && v != "0")
    };
    let f_number = field("FNumber").and_then(|v| {
        let v = v.trim_start_matches("f/");
        match v.split_once('/') {
            Some((n, d)) => Some(n.trim().parse::<f32>().ok()? / d.trim().parse::<f32>().ok()?),
            None => v.parse::<f32>().ok(),
        }
        .filter(|f| f.is_finite() && *f > 0.0)
    });
    let body = field("BodySerialNumber").or_else(|| field("SerialNumber"));
    (f_number, body)
}

/// The photos of `paths` that can witness the open photo's dust: the same
/// camera body (make, model and, where both record it, serial number) and
/// frame, at most `SERIES_MAX`, the nearest to the open photo in `paths`'
/// order first.
fn series_of(open: &str, adjustments: &Value, paths: Vec<String>) -> Vec<String> {
    if paths.iter().all(|p| p == open) {
        return Vec::new();
    }
    let at = paths.iter().position(|p| p == open).unwrap_or(0);
    let mut ordered: Vec<(usize, String)> = paths
        .into_iter()
        .enumerate()
        .filter(|(_, p)| p != open)
        .map(|(k, p)| (k.abs_diff(at), p))
        .collect();
    ordered.sort_by_key(|(d, _)| *d);
    let camera = camera_of(open);
    let frame = frame_of(open, adjustments);
    let body = aperture_and_body(open).1;
    if camera.is_none() || frame.is_none() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (_, path) in ordered {
        if found.len() >= SERIES_MAX {
            break;
        }
        if camera_of(&path) != camera {
            continue;
        }
        let other_body = aperture_and_body(&path).1;
        if body.is_some() && other_body.is_some() && other_body != body {
            continue;
        }
        let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(&path);
        let mut recipe = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
        started_recipe(&mut recipe, &source_path);
        if frame_of(&path, &recipe) == frame {
            found.push(path);
        }
    }
    found
}

/// A series photo's work copy and analysis, from the cache or decoded.
fn series_entry(path: &str) -> Option<SeriesEntry> {
    let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(path);
    let stamp = file_stamp(&source_path);
    let cached = SERIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(p, e)| p == path && e.stamp == stamp)
        .map(|(_, e)| e.clone());
    if cached.is_some() {
        return cached;
    }
    let bytes = crate::file_management::read_file_mapped(&source_path).ok()?;
    let name = source_path.to_string_lossy().to_string();
    let orientation = crate::image_loader::source_orientation(&bytes, &name);
    let settings = crate::app_settings::AppSettings::default();
    let image =
        crate::image_loader::load_base_image_from_bytes(&bytes, &name, false, &settings, None)
            .ok()?;
    drop(bytes);
    let encoded = !rawtonic_engine::formats::is_raw_file(&source_path);
    let work = rawtonic_engine::dust::work_copy(&image, encoded);
    let size = (image.width(), image.height());
    drop(image);
    let analysis = rawtonic_engine::dust::analyse_work(&work, size, &[]);
    let entry = SeriesEntry {
        stamp,
        work: std::sync::Arc::new(work),
        analysis: std::sync::Arc::new(analysis),
        size,
        orientation,
        f_number: aperture_and_body(path).0,
    };
    let mut cache = SERIES_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(p, _)| p != path);
    cache.push((path.to_string(), entry.clone()));
    if cache.len() > SERIES_MAX {
        cache.remove(0);
    }
    Some(entry)
}

/// The open photo's `analysis` with the evidence of the `series` photos
/// (`rawtonic_engine::dust::SeriesTarget`), and how many gave it.
fn with_series(
    open: &str,
    work: rawtonic_engine::dust::WorkCopy,
    size: (u32, u32),
    analysis: rawtonic_engine::dust::DustAnalysis,
    series: &[String],
    faces: &[[f64; 4]],
) -> (rawtonic_engine::dust::DustAnalysis, usize) {
    let others: Vec<SeriesEntry> = series.iter().filter_map(|p| series_entry(p)).collect();
    if others.is_empty() {
        return (analysis, 0);
    }
    let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(open);
    let orientation = std::fs::read(&source_path)
        .map(|bytes| {
            crate::image_loader::source_orientation(&bytes, &source_path.to_string_lossy())
        })
        .unwrap_or(1);
    let photo = rawtonic_engine::dust::SeriesPhoto {
        work: &work,
        analysis: &analysis,
        size,
        orientation,
        f_number: aperture_and_body(open).0,
    };
    let target = rawtonic_engine::dust::SeriesTarget::new(&photo);
    let mut evidence = target.evidence();
    for other in &others {
        target.gather(
            &rawtonic_engine::dust::SeriesPhoto {
                work: &other.work,
                analysis: &other.analysis,
                size: other.size,
                orientation: other.orientation,
                f_number: other.f_number,
            },
            &mut evidence,
        );
    }
    (target.apply(&evidence, faces), others.len())
}

/// A photo `sync_dust_spots` left alone, and why.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedPhoto {
    pub path: String,
    /// "otherCamera" (another camera or file kind), "unknownCamera"
    /// (no camera in its metadata), "otherFrame" (the same camera with
    /// another frame: a crop mode or aspect ratio).
    pub reason: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DustSync {
    #[serde(flatten)]
    pub batch: crate::file_management::RecipeBatch,
    pub skipped: Vec<SkippedPhoto>,
}

/// The unrotated size of the frame a photo develops with `adjustments`
/// (its frame version): the RAW's developed frame read without decoding
/// it, or another format's stored image size. Photos of one camera with another
/// frame (a crop mode, an in-camera aspect ratio) put the same sensor spot
/// elsewhere, so dust is synced only between equal frames.
fn frame_of(path: &str, adjustments: &Value) -> Option<(u32, u32)> {
    let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(path);
    let unrotated = |(w, h): (u32, u32), orientation: u16| {
        if (5..=8).contains(&orientation) {
            (h, w)
        } else {
            (w, h)
        }
    };
    if rawtonic_engine::formats::is_raw_file(&source_path) {
        let version = rawtonic_engine::source_frame::requested(adjustments);
        let frame = std::panic::catch_unwind(|| {
            rawtonic_rawdecode::raw_processing::developed_frame_at(&source_path, version)
        })
        .ok()
        .flatten()?;
        Some(unrotated(frame.size?, frame.orientation))
    } else {
        // The stored size, before the EXIF orientation: already unrotated.
        image::image_dimensions(&source_path).ok()
    }
}

/// A photo's recipe as an edit of it starts: a never-edited photo gets
/// the camera's crop and the current render versions first, as every other
/// batch edit gives it (`set_recipe_values`), so adding spots changes
/// nothing else about how it renders.
fn started_recipe(adjustments: &mut Value, source_path: &std::path::Path) {
    if !adjustments.is_object() {
        *adjustments = serde_json::json!({});
    }
    rawtonic_engine::camera_crop::apply_default(adjustments, source_path);
    crate::image_processing::apply_render_default(adjustments);
}

/// The camera of a photo from its metadata: make and model, and whether it
/// is a RAW file (a camera's JPEGs may have another frame).
fn camera_of(path: &str) -> Option<(String, String, bool)> {
    let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(path);
    let meta = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path);
    let exif = meta.exif?;
    let field = |k: &str| {
        exif.get(k)
            .map(|v| v.trim().trim_matches('"').trim().to_lowercase())
            .filter(|v| !v.is_empty())
    };
    Some((
        field("Make")?,
        field("Model")?,
        rawtonic_engine::formats::is_raw_file(&source_path),
    ))
}

/// "Sync dust to selected photos": the open photo's Find Dust spots
/// (`dust`), as they stand now, added to `paths` taken with the same
/// camera, at the same sensor place (each photo's own orientation, turns,
/// crop and lens geometry place them, also in exports before it is opened:
/// `lightroom_retouch`). Dust a photo already has is not added twice.
/// Photos of another or an unknown camera are skipped and named. One
/// recorded batch (one undo step).
#[tauri::command]
pub async fn sync_dust_spots(
    source: String,
    paths: Vec<String>,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<DustSync, String> {
    let loaded = crate::image_loader::original_image(&state).await?;
    // The spots' photo must still be the one loaded: its size and
    // orientation place them (a sync queued behind other writes may start
    // after the user went on to another photo).
    if loaded.path != source {
        return Err("The photo the dust was found on is no longer open".into());
    }
    let (width, height, open_path) = (loaded.image.width(), loaded.image.height(), loaded.path);
    let (targets, skipped, items) = tokio::task::spawn_blocking(move || {
        let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(&open_path);
        let bytes = std::fs::read(&source_path).map_err(|e| e.to_string())?;
        let orientation =
            crate::image_loader::source_orientation(&bytes, &source_path.to_string_lossy());
        let items = rawtonic_engine::dust::sync_items(&js_adjustments, width, height, orientation);
        let camera = camera_of(&open_path);
        let frame = frame_of(&open_path, &js_adjustments);
        let mut targets = Vec::new();
        let mut skipped = Vec::new();
        for path in paths.into_iter().filter(|p| *p != open_path) {
            match (&camera, camera_of(&path)) {
                (Some(here), Some(there)) if *here == there => {
                    let (source_path, sidecar_path) =
                        rawtonic_export::sidecars::parse_virtual_path(&path);
                    let mut recipe =
                        crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
                    started_recipe(&mut recipe, &source_path);
                    if frame.is_some() && frame_of(&path, &recipe) == frame {
                        targets.push(path);
                    } else {
                        skipped.push(SkippedPhoto {
                            path,
                            reason: "otherFrame",
                        });
                    }
                }
                (Some(_), Some(_)) => skipped.push(SkippedPhoto {
                    path,
                    reason: "otherCamera",
                }),
                _ => skipped.push(SkippedPhoto {
                    path,
                    reason: "unknownCamera",
                }),
            }
        }
        Ok::<_, String>((targets, skipped, items))
    })
    .await
    .map_err(|e| e.to_string())??;
    if items.is_empty() || targets.is_empty() {
        return Ok(DustSync {
            batch: crate::file_management::RecipeBatch {
                changes: Vec::new(),
                failure: None,
            },
            skipped,
        });
    }
    let batch =
        crate::file_management::recorded_batch(app_handle, targets, move |app_handle, paths| {
            crate::file_management::edit_sidecars_recorded(
                paths,
                crate::file_management::xmp_sync(app_handle),
                |_, source_path, sidecar_path| {
                    let mut metadata =
                        crate::exif_processing::load_sidecar_with_exif(sidecar_path, source_path);
                    started_recipe(&mut metadata.adjustments, source_path);
                    let bytes = std::fs::read(source_path).map_err(|e| e.to_string())?;
                    let orientation = crate::image_loader::source_orientation(
                        &bytes,
                        &source_path.to_string_lossy(),
                    );
                    rawtonic_engine::dust::add_synced(
                        &mut metadata.adjustments,
                        &items,
                        orientation,
                        &mut || uuid::Uuid::new_v4().to_string(),
                    );
                    Ok(metadata)
                },
            )
        })
        .await;
    Ok(DustSync { batch, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_dust_starts_a_never_edited_photos_edit_as_other_batch_edits_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        image::RgbImage::new(60, 40).save(&path).unwrap();
        let mut recipe = Value::Null;
        started_recipe(&mut recipe, &path);
        let mut expected = Value::Null;
        rawtonic_engine::camera_crop::apply_default(&mut expected, &path);
        crate::image_processing::apply_render_default(&mut expected);
        // A new edit's render versions, so the spots are the only change.
        assert_eq!(recipe, expected);
        assert!(recipe["renderVersion"].is_u64(), "{recipe}");
        let items = vec![
            serde_json::json!({"mode": "heal", "visible": true, "dust": {"x": 0.5, "y": 0.5},
            "lrNormalized": {"x": 0.5, "y": 0.5, "radius": 0.01, "sourceX": 0.6, "sourceY": 0.5}}),
        ];
        assert_eq!(
            rawtonic_engine::dust::add_synced(&mut recipe, &items, 1, &mut || "id".into()),
            1
        );
        assert_eq!(recipe["renderVersion"], expected["renderVersion"]);
        assert_eq!(recipe["spots"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn dust_frame_uses_stored_size_for_both_landscape_and_portrait_exif() {
        let dir = tempfile::tempdir().unwrap();
        let recipe = serde_json::json!({});
        let mut frames = Vec::new();
        for orientation in [1, 6] {
            let path = dir.path().join(format!("orientation-{orientation}.jpg"));
            let mut jpeg = Vec::new();
            image::DynamicImage::new_rgb8(60, 40)
                .write_to(
                    &mut std::io::Cursor::new(&mut jpeg),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
            let mut metadata = little_exif::metadata::Metadata::new();
            metadata.set_tag(little_exif::exif_tag::ExifTag::Orientation(vec![
                orientation,
            ]));
            metadata
                .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
                .unwrap();
            assert_eq!(
                crate::image_loader::source_orientation(&jpeg, "photo.jpg"),
                orientation
            );
            std::fs::write(&path, jpeg).unwrap();
            frames.push(frame_of(&path.to_string_lossy(), &recipe));
        }
        assert_eq!(frames, [Some((60, 40)), Some((60, 40))]);
    }

    #[test]
    fn photos_with_another_frame_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a.jpg"),
            dir.path().join("b.jpg"),
            dir.path().join("c.jpg"),
        );
        image::RgbImage::new(60, 40).save(&a).unwrap();
        image::RgbImage::new(60, 40).save(&b).unwrap();
        // A crop mode or another aspect ratio of the same camera.
        image::RgbImage::new(40, 40).save(&c).unwrap();
        let recipe = serde_json::json!({});
        let frame = |p: &std::path::Path| frame_of(&p.to_string_lossy(), &recipe);
        assert_eq!(frame(&a), Some((60, 40)));
        assert_eq!(frame(&a), frame(&b));
        assert_ne!(frame(&a), frame(&c));
    }
}
