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
}

type DustCache = std::sync::Mutex<
    Option<(
        u64,
        std::sync::Arc<rawtonic_engine::dust::DustAnalysis>,
        bool,
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
    state: tauri::State<'_, crate::app_state::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<FoundDust, String> {
    let loaded = crate::image_loader::original_image(&state).await?;
    let (source, is_raw, path) = (loaded.image, loaded.is_raw, loaded.path);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (std::sync::Arc::as_ptr(&source) as usize).hash(&mut hasher);
        for retouch in ["aiPatches", "spots", "redEyes"] {
            js_adjustments[retouch].to_string().hash(&mut hasher);
        }
        hasher.finish()
    };
    let cached = dust_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(k, ..)| *k == key)
        .map(|(_, analysis, faces)| (analysis.clone(), *faces));
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
    let (analysis, faces_checked) = match (cached, &base) {
        (Some(found), _) => found,
        (None, None) => return Err("No photo analysis".into()),
        (None, Some(base)) => {
            // Faces as decoded: the patches do not move them.
            let faces = face_boxes(&app_handle, source.clone(), is_raw).await;
            let checked = faces.is_some();
            let base = base.clone();
            let analysis = tokio::task::spawn_blocking(move || {
                std::sync::Arc::new(rawtonic_engine::dust::analyse(
                    &base,
                    !is_raw,
                    faces.as_deref().unwrap_or_default(),
                ))
            })
            .await
            .map_err(|e| e.to_string())?;
            *dust_cache().lock().unwrap_or_else(|e| e.into_inner()) =
                Some((key, analysis.clone(), checked));
            (analysis, checked)
        }
    };
    tokio::task::spawn_blocking(move || {
        let found = rawtonic_engine::dust::placed(&analysis, sensitivity, &js_adjustments);
        let spots = if let Some(base) = base.filter(|_| apply) {
            let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(&path);
            let bytes = std::fs::read(&source_path).map_err(|e| e.to_string())?;
            let orientation =
                crate::image_loader::source_orientation(&bytes, &source_path.to_string_lossy());
            rawtonic_engine::dust::heal_spots(&base, &js_adjustments, &found, orientation)
        } else {
            found
                .iter()
                .map(|s| serde_json::to_value(s).unwrap_or_default())
                .collect()
        };
        Ok(FoundDust {
            spots,
            faces_checked,
        })
    })
    .await
    .map_err(|e| e.to_string())?
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
