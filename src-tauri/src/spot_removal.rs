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
    /// (no camera in its metadata).
    pub reason: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DustSync {
    #[serde(flatten)]
    pub batch: crate::file_management::RecipeBatch,
    pub skipped: Vec<SkippedPhoto>,
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
    paths: Vec<String>,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<DustSync, String> {
    let loaded = crate::image_loader::original_image(&state).await?;
    let (width, height, open_path) = (loaded.image.width(), loaded.image.height(), loaded.path);
    let (targets, skipped, items) = tokio::task::spawn_blocking(move || {
        let (source_path, _) = rawtonic_export::sidecars::parse_virtual_path(&open_path);
        let bytes = std::fs::read(&source_path).map_err(|e| e.to_string())?;
        let orientation =
            crate::image_loader::source_orientation(&bytes, &source_path.to_string_lossy());
        let items = rawtonic_engine::dust::sync_items(&js_adjustments, width, height, orientation);
        let camera = camera_of(&open_path);
        let mut targets = Vec::new();
        let mut skipped = Vec::new();
        for path in paths.into_iter().filter(|p| *p != open_path) {
            match (&camera, camera_of(&path)) {
                (Some(here), Some(there)) if *here == there => targets.push(path),
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
