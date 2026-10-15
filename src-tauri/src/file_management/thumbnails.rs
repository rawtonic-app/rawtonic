//! Library thumbnails: cache, generation workers and queue.
//! Part of `file_management`, split by concern (ADR 0009).
use super::enqueue_metadata;
use super::is_cloud_placeholder;
use super::parse_virtual_path;
use super::read_file_mapped;
use crate::AppSettings;
use crate::app_state::AppState;
use crate::formats::is_raw_file;
use crate::gpu_processing;
use crate::image_loader;
use crate::image_processing::Crop;
use crate::image_processing::GpuContext;
use crate::image_processing::ImageMetadata;
use crate::image_processing::apply_coarse_rotation;
use crate::image_processing::apply_cpu_default_raw_processing;
use crate::image_processing::apply_crop;
use crate::image_processing::apply_flip;
use crate::image_processing::apply_geometry_warp;
use crate::image_processing::apply_rotation;
use crate::image_processing::get_all_adjustments_from_json;
use crate::load_settings;
use crate::mask_generation::MaskDefinition;
use crate::mask_generation::ParametricMaskSource;
use crate::mask_generation::prepare_parametric_mask_source;
use anyhow::Result;
use image::DynamicImage;
use image::GenericImageView;
use image::codecs::jpeg::JpegEncoder;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::AppHandle;
use tauri::{Emitter, Manager};

pub(crate) fn resolve_thumbnail_cache_dir(
    app_handle: &AppHandle,
) -> std::result::Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

pub(crate) fn emit_thumbnail_cache_setup_error(app_handle: &AppHandle, path: &str, reason: &str) {
    let _ = app_handle.emit(
        "thumbnail-generation-error",
        serde_json::json!({ "path": path, "reason": reason }),
    );
}

/// The editor state for `source`'s thumbnail: its sidecar file, or for an
/// offline photograph (its file and maybe its sidecar are gone) the
/// library's.
fn thumbnail_metadata(source: &Path, sidecar: &Path) -> Option<ImageMetadata> {
    if let Ok(content) = fs::read_to_string(sidecar) {
        return serde_json::from_str(&content).ok();
    }
    if source.exists() {
        return None;
    }
    crate::session_bridge::read_editor_sidecar(sidecar)
        .and_then(|state| serde_json::from_value(state).ok())
}

/// The modification time of `source` in seconds; for an offline photograph
/// the one the library registered (its cached thumbnails stay valid).
fn source_mtime(source: &Path) -> Option<u64> {
    match fs::metadata(source) {
        Ok(meta) => meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs()),
        Err(_) => crate::session_bridge::registered_mtime(source),
    }
}

/// Whether the recipe or one of its mask layers sets Contrast, Shadows,
/// Whites or Blacks.
fn uses_tone_sliders(adjustments: &serde_json::Value) -> bool {
    let sets = |values: &serde_json::Value| {
        ["contrast", "shadows", "whites", "blacks"]
            .iter()
            .any(|key| {
                values
                    .get(key)
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|v| v != 0.0)
            })
    };
    sets(adjustments)
        || adjustments
            .get("masks")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|masks| {
                masks
                    .iter()
                    .any(|mask| mask.get("adjustments").is_some_and(&sets))
            })
}

pub(crate) fn compute_thumbnail_cache_hash(
    path_str: &str,
    adjustments_bytes: &[u8],
) -> Option<String> {
    let (source_path, _) = parse_virtual_path(path_str);

    let img_mod_time = source_mtime(&source_path)?;

    let mut hasher = blake3::Hasher::new();
    hasher.update(path_str.as_bytes());
    hasher.update(&img_mod_time.to_le_bytes());
    hasher.update(adjustments_bytes);
    let adjustments = serde_json::from_slice::<serde_json::Value>(adjustments_bytes).ok();
    if adjustments
        .as_ref()
        .is_some_and(|adjustments| !adjustments["relightDepth"].is_null())
    {
        hasher.update(b"\0relight-face-reflection-v7");
    }
    // Absent light fields now render with a new light's values.
    if adjustments
        .as_ref()
        .and_then(|adjustments| adjustments["relightLights"].as_array())
        .is_some_and(|lights| !lights.is_empty())
    {
        hasher.update(b"\0relight-defaults-v1");
    }
    if is_raw_file(&source_path) {
        // Old embedded RAW previews could omit camera orientation for non-TIFF
        // containers (notably RAF). Their pixels must not survive this fix.
        hasher.update(b"\0embedded-raw-orientation-v1");
        // Developed RAW pixels change with the decode engine (e.g. highlight
        // reconstruction), so thumbnails follow its version.
        hasher.update(format!("\0raw-engine-{}", crate::preview_proxy::ENGINE_VERSION).as_bytes());
        // With a preview proxy the thumbnail is developed from it (not the
        // camera JPEG), so a new proxy makes a new thumbnail.
        if crate::preview_proxy::proxy_path(&source_path).is_some_and(|p| p.is_file()) {
            hasher.update(b"\0developed-from-proxy-v1");
        }
        // Contrast, Shadows, Whites and Blacks keep tones in order since
        // 2026-10-12 (the RAW look's slider gains reversed near black and
        // white). Only thumbnails that use them are rendered again; the
        // decode (preview proxies, ENGINE_VERSION) is unchanged.
        if serde_json::from_slice::<serde_json::Value>(adjustments_bytes)
            .ok()
            .is_some_and(|adjustments| uses_tone_sliders(&adjustments))
        {
            hasher.update(b"\0tone-order-v1");
        }
    }
    if serde_json::from_slice::<serde_json::Value>(adjustments_bytes)
        .ok()
        .is_some_and(|adjustments| {
            adjustments
                .get("masks")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|masks| !masks.is_empty())
        })
    {
        // Previously persisted masked previews may contain pixels from another
        // editor source or shifted layer adjustments. Preserve unmasked entries.
        hasher.update(b"\0masked-thumbnail-source-and-alignment-v1");
    }
    Some(hasher.finalize().to_hex().to_string())
}

pub(crate) fn try_load_embedded_raw_preview(source_path: &Path) -> Option<DynamicImage> {
    let mmap = read_file_mapped(source_path).ok()?;
    image_loader::safe_embedded_preview_fallback(&mmap, &source_path.to_string_lossy())
}

pub fn generate_thumbnail_data(
    path_str: &str,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
) -> anyhow::Result<DynamicImage> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);
    let source_path_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path_str);

    let metadata: Option<ImageMetadata> = if is_cloud_placeholder(&sidecar_path) {
        enqueue_metadata(
            app_handle,
            path_str.to_string(),
            source_path.clone(),
            sidecar_path.clone(),
        );
        None
    } else {
        thumbnail_metadata(&source_path, &sidecar_path)
    };

    let adjustments = metadata
        .as_ref()
        .map_or(serde_json::Value::Null, |m| m.adjustments.clone());

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let always_decode_raw = settings.always_decode_raw_thumbnails.unwrap_or(false);

    // A preview proxy (preview_proxy.rs) develops the thumbnail like the
    // editor does, also for never-edited RAWs (RAW defaults and the camera's
    // lens data), so the Library and the placeholder show the edited look.
    let proxy = (is_raw && preloaded_image.is_none() && gpu_context.is_some())
        .then(|| crate::preview_proxy::read(&source_path, &settings))
        .flatten();
    let (metadata, adjustments) = match (&proxy, metadata) {
        (Some(_), metadata) if adjustments.is_null() => {
            let mut recipe = serde_json::json!({});
            crate::embedded_lens::apply_default(&mut recipe, &source_path);
            let mut meta = metadata.unwrap_or_default();
            meta.adjustments = recipe.clone();
            (Some(meta), recipe)
        }
        (_, metadata) => (metadata, adjustments),
    };

    if is_raw
        && adjustments.is_null()
        && preloaded_image.is_none()
        && !always_decode_raw
        && let Some(preview) = try_load_embedded_raw_preview(&source_path)
    {
        return Ok(preview);
    }

    if let (Some(context), Some(meta)) = (gpu_context, metadata)
        && !meta.adjustments.is_null()
    {
        let state = app_handle.state::<AppState>();
        let target_res = settings.medium_thumbnail_resolution.unwrap_or(1280);

        let base_cache_hash = crate::cache_utils::calculate_thumbnail_base_hash(&meta.adjustments);

        let crop_data: Option<Crop> = serde_json::from_value(meta.adjustments["crop"].clone()).ok();

        let mask_definitions: Vec<MaskDefinition> = meta
            .adjustments
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();
        let needs_mask_source = mask_definitions
            .iter()
            .any(MaskDefinition::requires_warped_image);

        let cached_base: Option<(Arc<DynamicImage>, f32, Option<ParametricMaskSource>)> = {
            let cache = state.thumbnail_geometry_cache.lock().unwrap();
            if let Some((cached_hash, img, scale, mask_source)) = cache.get(path_str) {
                let mut sufficient_resolution = true;
                if let Some(c) = &crop_data
                    && c.width > 0.0
                    && c.height > 0.0
                {
                    let final_crop_max_dim =
                        (c.width as f32 * *scale).max(c.height as f32 * *scale);
                    if final_crop_max_dim < (target_res as f32 * 0.95) {
                        sufficient_resolution = false;
                    }
                }

                if *cached_hash == base_cache_hash
                    && sufficient_resolution
                    && (!needs_mask_source || mask_source.is_some())
                {
                    Some((Arc::clone(img), *scale, mask_source.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        };

        let (processing_base_arc, total_scale, mask_source) = if let Some(cached) = cached_base {
            cached
        } else {
            let mut raw_scale_factor = 1.0f32;

            let pristine_source = if let Some(img) = preloaded_image {
                Cow::Borrowed(img)
            } else if let Some(proxy) = &proxy {
                raw_scale_factor = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
                Cow::Borrowed(&proxy.image)
            } else {
                let mmap_guard;
                let vec_guard;

                let file_slice: &[u8] = match read_file_mapped(&source_path) {
                    Ok(mmap) => {
                        mmap_guard = Some(mmap);
                        mmap_guard.as_ref().unwrap()
                    }
                    Err(e) => {
                        if preloaded_image.is_none() {
                            log::warn!("Fallback read for {}: {}", source_path_str, e);
                        }
                        let bytes = fs::read(&source_path).map_err(|io_err| {
                            anyhow::anyhow!(
                                "Fallback read failed for {}: {}",
                                source_path_str,
                                io_err
                            )
                        })?;
                        vec_guard = Some(bytes);
                        vec_guard.as_ref().unwrap()
                    }
                };

                let img = image_loader::load_base_image_from_bytes(
                    file_slice,
                    &source_path_str,
                    true,
                    &settings,
                    None,
                )?;

                if is_raw {
                    raw_scale_factor = crate::raw_processing::get_fast_demosaic_scale_factor(
                        file_slice,
                        img.width(),
                        img.height(),
                    );
                }
                Cow::Owned(img)
            };

            let composite_image =
                image_loader::composite_patches_for_render(&pristine_source, &adjustments)?;

            let warped_image = apply_geometry_warp(composite_image, &meta.adjustments);

            let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, &meta.adjustments);

            let orientation_steps =
                meta.adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
            let coarse_rotated_image = apply_coarse_rotation(blurred_image, orientation_steps);

            let (full_w, full_h) = coarse_rotated_image.dimensions();

            let mut processing_dim = target_res;
            if let Some(c) = &crop_data
                && c.width > 0.0
                && c.height > 0.0
            {
                let crop_max_dim_loaded = c.width.max(c.height) * raw_scale_factor as f64;
                let full_max_dim = full_w.max(full_h) as f64;
                if crop_max_dim_loaded > 0.0 {
                    processing_dim = ((target_res as f64 * full_max_dim / crop_max_dim_loaded)
                        .round() as u32)
                        .min(full_w.max(full_h));
                }
            }

            let (base, gpu_scale) = if full_w > processing_dim || full_h > processing_dim {
                let base = crate::image_processing::downscale_f32_image(
                    &coarse_rotated_image,
                    processing_dim,
                    processing_dim,
                );
                let scale = if full_w > 0 {
                    base.width() as f32 / full_w as f32
                } else {
                    1.0
                };
                (base, scale)
            } else {
                (coarse_rotated_image.into_owned(), 1.0)
            };

            let total_scale = gpu_scale * raw_scale_factor;

            // Keep a separate sampling source before orientation/flip/crop and
            // lens blur. Its scale maps saved full-resolution picker coordinates
            // into this target file's reduced pixels, never the editor image.
            let mask_source = if needs_mask_source {
                let mut source = prepare_parametric_mask_source(
                    &pristine_source,
                    is_raw,
                    raw_scale_factor,
                    &meta.adjustments,
                )
                .map_err(anyhow::Error::msg)?;
                if source.image.width().max(source.image.height()) > target_res {
                    let reduced = crate::image_processing::downscale_f32_image(
                        &source.image,
                        target_res,
                        target_res,
                    );
                    source = source.with_resized_image(reduced);
                }
                Some(source)
            } else {
                None
            };
            let mut cache = state.thumbnail_geometry_cache.lock().unwrap();
            if cache.len() >= 8 {
                let key_to_remove = cache.keys().next().cloned();
                if let Some(key) = key_to_remove {
                    cache.remove(&key);
                }
            }

            let base_arc = Arc::new(base);
            cache.insert(
                path_str.to_string(),
                (
                    base_cache_hash,
                    Arc::clone(&base_arc),
                    total_scale,
                    mask_source.clone(),
                ),
            );

            (base_arc, total_scale, mask_source)
        };

        let rotation_degrees = meta.adjustments["rotation"].as_f64().unwrap_or(0.0) as f32;
        let flip_horizontal = meta.adjustments["flipHorizontal"]
            .as_bool()
            .unwrap_or(false);
        let flip_vertical = meta.adjustments["flipVertical"].as_bool().unwrap_or(false);

        let flipped_image = apply_flip(
            Cow::Borrowed(&*processing_base_arc),
            flip_horizontal,
            flip_vertical,
        );
        let rotated_image = apply_rotation(flipped_image, rotation_degrees);

        let scaled_crop_json = if let Some(c) = &crop_data {
            serde_json::to_value(Crop {
                x: c.x * total_scale as f64,
                y: c.y * total_scale as f64,
                width: c.width * total_scale as f64,
                height: c.height * total_scale as f64,
            })
            .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };

        let cropped_preview = apply_crop(rotated_image, &scaled_crop_json);
        let (preview_w, preview_h) = cropped_preview.dimensions();
        let unscaled_crop_offset = crop_data.map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));

        // The photo's own size, from the oriented base at its scale.
        let (base_w, base_h) = processing_base_arc.dimensions();
        let (base_w, base_h) =
            if meta.adjustments["orientationSteps"].as_u64().unwrap_or(0) % 2 == 1 {
                (base_h, base_w)
            } else {
                (base_w, base_h)
            };
        let mask_geometry =
            crate::mask_generation::PhotoGeometry::from_adjustments(&meta.adjustments)
                .with_photo_size(
                    base_w as f64 / total_scale as f64,
                    base_h as f64 / total_scale as f64,
                );
        let mask_bitmaps: Vec<Arc<image::GrayImage>> = mask_definitions
            .iter()
            .filter_map(|def| {
                crate::mask_generation::get_cached_or_generate_mask(
                    &state,
                    def,
                    preview_w,
                    preview_h,
                    total_scale,
                    (
                        unscaled_crop_offset.0 * total_scale,
                        unscaled_crop_offset.1 * total_scale,
                    ),
                    &mask_geometry,
                    mask_source.as_ref(),
                )
                .transpose()
            })
            .collect::<std::result::Result<_, _>>()
            .map_err(anyhow::Error::msg)?;

        let tm_override = crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
        let mut gpu_adjustments =
            get_all_adjustments_from_json(&meta.adjustments, is_raw, tm_override);
        crate::baseline_exposure::apply_base_look(&mut gpu_adjustments.global, &source_path_str);
        gpu_adjustments.global.detail_scale = total_scale;
        let lut_path = meta.adjustments["lutPath"].as_str();
        let lut = lut_path.and_then(|p| {
            let mut cache = state.lut_cache.lock().unwrap();
            if let Some(cached_lut) = cache.get(p) {
                return Some(cached_lut.clone());
            }
            if let Ok(loaded_lut) = crate::lut_processing::parse_lut_file(p) {
                let arc_lut = Arc::new(loaded_lut);
                cache.insert(p.to_string(), arc_lut.clone());
                return Some(arc_lut);
            }
            None
        });

        let mut hasher = DefaultHasher::new();
        path_str.hash(&mut hasher);
        meta.adjustments.to_string().hash(&mut hasher);
        let unique_hash = hasher.finish();

        // Thumbnails follow every edit and share the editor's GPU lock. The
        // display variants keep many-layer recipes fast; exports stay exact.
        if let Ok(processed_image) =
            gpu_processing::process_and_get_dynamic_image_with_display_shader(
                context,
                &state,
                cropped_preview.as_ref(),
                unique_hash,
                gpu_processing::RenderRequest {
                    adjustments: gpu_adjustments,
                    mask_bitmaps: &mask_bitmaps,
                    lut,
                    roi: None,
                },
                "generate_thumbnail_data",
            )
        {
            return Ok(processed_image);
        } else {
            return Ok(cropped_preview.into_owned());
        }
    }

    let mut final_image = if let Some(img) = preloaded_image {
        image_loader::composite_patches_on_image(img, &adjustments)?
    } else {
        match read_file_mapped(&source_path) {
            Ok(mmap) => image_loader::load_and_composite(
                &mmap,
                &source_path_str,
                &adjustments,
                true,
                &settings,
                None,
            )?,
            Err(e) => {
                log::warn!("Fallback read for {}: {}", source_path_str, e);
                let bytes = fs::read(&source_path)?;
                image_loader::load_and_composite(
                    &bytes,
                    &source_path_str,
                    &adjustments,
                    true,
                    &settings,
                    None,
                )?
            }
        }
    };

    if adjustments.is_null() {
        let tm_override = crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
        let use_agx = tm_override == Some(1);

        if use_agx {
            if !is_raw {
                final_image = crate::image_processing::apply_srgb_to_linear(final_image);
            }
            crate::image_processing::apply_cpu_agx_tonemap(&mut final_image);
        } else if is_raw {
            apply_cpu_default_raw_processing(&mut final_image);
        }
    }

    let fallback_orientation_steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
    Ok(apply_coarse_rotation(Cow::Owned(final_image), fallback_orientation_steps).into_owned())
}

/// A small JPEG (at most `size` pixels on its long side) of a photograph that
/// is not in the library yet, for the import dialog's previews: a RAW's
/// smallest embedded camera preview that is at least `size / 2` pixels on its
/// short side, else its small TIFF thumbnail (no RAW decode); other images
/// decoded and scaled. Only a file without any of these is developed (fast
/// mode, as the library's thumbnails). Reads the file only (memory-mapped,
/// read into memory on a card: `read_import_source`); never writes or
/// caches anything. `None` when nothing could be read, and
/// for cloud placeholders (no download).
pub(crate) fn source_preview_jpeg(path: &Path, size: u32) -> Option<Vec<u8>> {
    encode_thumbnail(&source_preview_image(path, size)?, size).ok()
}

/// A photograph to import, as the import dialog's previews read it.
enum ImportSource {
    /// Memory-mapped: only the parts a preview needs are read.
    Mapped(memmap2::Mmap),
    /// Read into memory.
    Read(Vec<u8>),
}

impl std::ops::Deref for ImportSource {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Mapped(map) => map,
            Self::Read(bytes) => bytes,
        }
    }
}

/// Whether a photograph to import is read into memory rather than mapped:
/// on a memory card or other removable medium (`Medium::Card`) a card
/// pulled while a memory map of it is read raises SIGBUS and kills the app,
/// where a read only fails. Costs a full read of the file per preview there.
fn read_import_into_memory(path: &Path) -> bool {
    rawtonic_ingest::batch::source_medium(path) == rawtonic_ingest::batch::Medium::Card
}

/// A photograph to import for `source_preview_jpeg`, `source_large_preview_jpeg`
/// and `source_capture_info`: read into memory on a card, mapped elsewhere
/// (as `read_file_mapped`: refusing missing, empty or locked files).
fn read_import_source(path: &Path) -> Option<ImportSource> {
    if !read_import_into_memory(path) {
        return read_file_mapped(path).ok().map(ImportSource::Mapped);
    }
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() || file.try_lock_shared().is_err() {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &file, &mut bytes).ok()?;
    (!bytes.is_empty()).then_some(ImportSource::Read(bytes))
}

/// `source_preview_jpeg`'s image before scaling.
fn source_preview_image(path: &Path, size: u32) -> Option<DynamicImage> {
    if !path.is_file() || is_cloud_placeholder(path) {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bytes = read_import_source(path)?;
        quick_source_preview(&bytes, path, size).or_else(|| {
            let path_str = path.to_string_lossy();
            let mut image = image_loader::load_and_composite(
                &bytes,
                &path_str,
                &serde_json::Value::Null,
                true,
                &AppSettings::default(),
                None,
            )
            .ok()?;
            if is_raw_file(path) {
                apply_cpu_default_raw_processing(&mut image);
            }
            Some(image)
        })
    }))
    .ok()
    .flatten()
}

/// The import dialog's large view of a photograph that is not in the library
/// yet: a RAW's largest embedded camera JPEG passed through as it is (no
/// decode or re-encode; its orientation written into the copy's EXIF, which
/// the webview applies), a JPEG file's own bytes, and for anything else (a
/// RAW whose embedded previews are all small, CR3, TIFF, HEIC, PNG…) the
/// largest available preview scaled to at most `max_size` pixels on its long
/// side, as `source_preview_jpeg` makes it. Reads the file only.
pub(crate) fn source_large_preview_jpeg(path: &Path, max_size: u32) -> Option<Vec<u8>> {
    if !path.is_file() || is_cloud_placeholder(path) {
        return None;
    }
    // Below this an embedded JPEG is a thumbnail, not a viewable preview.
    let viewable = max_size.min(1280);
    let passthrough = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bytes = read_import_source(path)?;
        let jpeg = if is_raw_file(path) {
            image_loader::priority_embedded_jpeg(&bytes)?
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            bytes.to_vec()
        } else {
            return None;
        };
        let (width, height) =
            image::ImageReader::with_format(Cursor::new(&jpeg), image::ImageFormat::Jpeg)
                .into_dimensions()
                .ok()?;
        (width.max(height) >= viewable).then_some(jpeg)
    }))
    .ok()
    .flatten();
    passthrough.or_else(|| {
        let image = source_preview_image(path, max_size)?;
        // Scaled down to the view, never up.
        encode_thumbnail(&image, max_size.min(image.width().max(image.height()))).ok()
    })
}

/// What the import dialog's large view shows under a photograph: ISO,
/// shutter speed, aperture and focal length as the camera wrote them. Read
/// from the file only (nothing is cached beside it); empty fields when the
/// file has no such metadata.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCaptureInfo {
    pub iso: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<String>,
    pub focal_length: Option<String>,
}

pub(crate) fn source_capture_info(path: &Path) -> SourceCaptureInfo {
    if !path.is_file() || is_cloud_placeholder(path) {
        return SourceCaptureInfo::default();
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bytes = read_import_source(path)?;
        let exif =
            crate::exif_processing::read_exif_data_from_bytes(&path.to_string_lossy(), &bytes);
        let field = |keys: &[&str]| {
            keys.iter()
                .filter_map(|key| exif.get(*key))
                .map(|value| value.trim().trim_matches('"').trim().to_string())
                .find(|value| !value.is_empty() && value != "0")
        };
        let number = |value: String| {
            value
                .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .find(|part| !part.is_empty())
                .and_then(|part| part.parse::<f64>().ok())
                .filter(|v| *v > 0.0)
        };
        let trim = |v: f64| {
            let text = format!("{:.1}", v);
            text.trim_end_matches(".0").to_string()
        };
        Some(SourceCaptureInfo {
            iso: field(&["PhotographicSensitivity", "ISOSpeed", "ISOSpeedRatings"])
                .and_then(number)
                .map(|v| format!("ISO {}", v.round())),
            exposure: field(&["ExposureTime"]).map(|value| {
                let value = value.trim_end_matches(" s").trim_end_matches("s").trim();
                format!("{value} s")
            }),
            aperture: field(&["FNumber", "ApertureValue"])
                .and_then(number)
                .map(|v| format!("f/{}", trim(v))),
            focal_length: field(&["FocalLength"])
                .and_then(number)
                .map(|v| format!("{} mm", trim(v))),
        })
    }))
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// `source_preview_jpeg`'s previews that need no develop.
fn quick_source_preview(bytes: &[u8], path: &Path, size: u32) -> Option<DynamicImage> {
    let orient = |image: DynamicImage, orientation: Option<u16>| match orientation {
        Some(o) if o > 1 => rawtonic_rawdecode::orientation::apply_orientation(
            image,
            rawler::Orientation::from_u16(o),
        ),
        _ => image,
    };
    let jpeg =
        |data: &[u8]| image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok();
    if !is_raw_file(path) {
        let image = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?;
        return Some(orient(image, image_loader::exif_orientation(bytes)));
    }
    let small_enough = |data: &&[u8]| {
        image::ImageReader::with_format(Cursor::new(*data), image::ImageFormat::Jpeg)
            .into_dimensions()
            .is_ok_and(|(w, h)| w.min(h) >= size / 2)
    };
    // Candidates come largest first: take the smallest that suffices.
    if let Some(image) = image_loader::tiff_jpeg_candidates(bytes)
        .and_then(|candidates| candidates.into_iter().rev().find(small_enough))
        .and_then(jpeg)
    {
        return Some(orient(image, image_loader::exif_orientation(bytes)));
    }
    if let Some(data) = image_loader::raf_embedded_jpeg(bytes)
        && let Some(image) = jpeg(data)
    {
        return Some(orient(image, image_loader::exif_orientation(data)));
    }
    // A DNG without a JPEG preview (DNG Converter's "None") still has its
    // small uncompressed thumbnail as the first image.
    let small_tiff = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Tiff)
        .into_dimensions()
        .is_ok_and(|(w, h)| w.max(h) <= 1024);
    if small_tiff
        && let Ok(image) = image::load_from_memory_with_format(bytes, image::ImageFormat::Tiff)
    {
        return Some(orient(image, image_loader::exif_orientation(bytes)));
    }
    image_loader::safe_embedded_preview_fallback(bytes, &path.to_string_lossy())
}

pub(crate) fn encode_thumbnail(image: &DynamicImage, target_width: u32) -> Result<Vec<u8>> {
    let thumbnail = crate::image_processing::thumbnail(image, target_width, target_width);
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 75);
    encoder.encode_image(&thumbnail.to_rgb8())?;
    Ok(buf.into_inner())
}

pub(crate) fn generate_single_thumbnail_and_cache(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<(String, String, Option<u8>, Option<bool>)> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);

    // Rating and edit state only when a sidecar was read: without one they
    // are unknown here (the listing has them), and an event saying 0 could
    // arrive after the photograph was registered with its rating.
    let (rating, is_edited, adjustments_bytes) = if is_cloud_placeholder(&sidecar_path) {
        enqueue_metadata(
            app_handle,
            path_str.to_string(),
            source_path.clone(),
            sidecar_path.clone(),
        );
        (None, None, Vec::new())
    } else if let Some(meta) = thumbnail_metadata(&source_path, &sidecar_path) {
        {
            let is_raw = crate::formats::is_raw_file(path_str);
            let tm = crate::image_processing::resolve_tonemapper_override(settings, is_raw);
            (
                Some(meta.rating),
                Some(crate::image_processing::is_image_edited(
                    &meta.adjustments,
                    is_raw,
                    tm,
                )),
                serde_json::to_vec(&meta.adjustments).unwrap_or_default(),
            )
        }
    } else {
        (None, None, Vec::new())
    };

    let cache_hash = compute_thumbnail_cache_hash(path_str, &adjustments_bytes)?;

    let small_path = thumb_cache_dir.join(format!("{}_small.jpg", cache_hash));
    let medium_path = thumb_cache_dir.join(format!("{}_medium.jpg", cache_hash));

    if !force_regenerate && small_path.exists() && medium_path.exists() {
        return Some((
            small_path.to_string_lossy().into_owned(),
            medium_path.to_string_lossy().into_owned(),
            rating,
            is_edited,
        ));
    }

    if is_cloud_placeholder(&source_path) {
        return None;
    }

    let target_width_small = settings.small_thumbnail_resolution.unwrap_or(480);
    let target_width_medium = settings.medium_thumbnail_resolution.unwrap_or(1280);

    if let Ok(thumb_image) =
        generate_thumbnail_data(path_str, gpu_context, preloaded_image, app_handle)
        && let (Ok(small_data), Ok(medium_data)) = (
            encode_thumbnail(&thumb_image, target_width_small),
            encode_thumbnail(&thumb_image, target_width_medium),
        )
    {
        let _ = fs::write(&small_path, &small_data);
        let _ = fs::write(&medium_path, &medium_data);
        return Some((
            small_path.to_string_lossy().into_owned(),
            medium_path.to_string_lossy().into_owned(),
            rating,
            is_edited,
        ));
    }
    None
}

/// Makes `path`'s thumbnails again after its preview proxy was written (the
/// thumbnail is then developed from it) and tells the frontend.
pub fn refresh_thumbnail(app_handle: &AppHandle, path: &str) {
    let state = app_handle.state::<crate::app_state::AppState>();
    let gpu_context = crate::gpu_processing::get_or_init_gpu_context(&state, app_handle).ok();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let Ok(cache_dir) = get_thumb_cache_dir(app_handle) else {
        return;
    };
    if let Some((small, medium, rating, is_edited)) = generate_single_thumbnail_and_cache(
        path,
        &cache_dir,
        gpu_context.as_ref(),
        None,
        false,
        app_handle,
        &settings,
    ) {
        emit_thumbnail_generated(app_handle, path, &small, &medium, rating, is_edited);
    }
}

pub(crate) fn prefetch_source_file(path_str: &str) {
    let (source_path, _) = parse_virtual_path(path_str);
    if let Ok(mut file) = std::fs::File::open(&source_path) {
        let _ = std::io::copy(&mut file, &mut std::io::sink());
    }
}

pub fn start_thumbnail_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::app_state::AppState>();
    let manager = state.thumbnail_manager.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let thread_count = settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 16);

    for _ in 0..thread_count {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();

        std::thread::spawn(move || {
            loop {
                let path_to_process: String = {
                    let mut queue = manager_clone.queue.lock().unwrap();
                    while queue.is_empty() {
                        queue = manager_clone.cvar.wait(queue).unwrap();
                    }
                    let path = queue.pop_back().unwrap();

                    let mut processing = manager_clone.processing_now.lock().unwrap();
                    if processing.contains(&path) {
                        let state = app_clone.state::<crate::app_state::AppState>();
                        increment_thumbnail_progress(&state, &app_clone);
                        continue;
                    }
                    processing.insert(path.clone());
                    path
                };

                let state = app_clone.state::<crate::app_state::AppState>();
                let gpu_context =
                    crate::gpu_processing::get_or_init_gpu_context(&state, &app_clone).ok();

                let current_settings = load_settings(app_clone.clone()).unwrap_or_default();

                if let Ok(cache_dir) = get_thumb_cache_dir(&app_clone) {
                    if manager_clone.rotational_disk.load(Ordering::Relaxed) {
                        let _io_permit = manager_clone.io_gate.lock().unwrap();
                        prefetch_source_file(&path_to_process);
                    }

                    let result = generate_single_thumbnail_and_cache(
                        &path_to_process,
                        &cache_dir,
                        gpu_context.as_ref(),
                        None,
                        false,
                        &app_clone,
                        &current_settings,
                    );

                    if let Some((small_path, medium_path, rating, is_edited)) = result {
                        emit_thumbnail_generated(
                            &app_clone,
                            &path_to_process,
                            &small_path,
                            &medium_path,
                            rating,
                            is_edited,
                        );
                    }
                    increment_thumbnail_progress(&state, &app_clone);
                }
                manager_clone
                    .processing_now
                    .lock()
                    .unwrap()
                    .remove(&path_to_process);
            }
        });
    }
}

pub(crate) fn visible_thumbnail_paths(
    paths: Vec<String>,
    processing: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|path| !processing.contains(path) && seen.insert(path.clone()))
        .take(500)
        .collect()
}

#[tauri::command]
pub fn update_thumbnail_queue(
    paths: Vec<String>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<crate::app_state::AppState>();

    let mut queue = state.thumbnail_manager.queue.lock().unwrap();

    if paths.is_empty() {
        queue.clear();
        let mut tracker = state.thumbnail_progress.lock().unwrap();
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        state.thumbnail_manager.cvar.notify_all();
        return Ok(());
    }

    // This command receives a viewport snapshot, not an accumulating job list.
    // Do not spend CPU decoding images the user has already scrolled past.
    let processing = state.thumbnail_manager.processing_now.lock().unwrap();
    let mut unique_paths = visible_thumbnail_paths(paths, &processing);
    drop(processing);
    queue.clear();
    if state
        .thumbnail_manager
        .rotational_disk
        .load(Ordering::Relaxed)
    {
        unique_paths.sort();
    }
    // Workers pop from the back; preserve visible order and bound oversized input.
    queue.extend(unique_paths.into_iter().rev());

    let queue_len = queue.len();
    drop(queue);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = tracker.completed + queue_len;

    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );

    state.thumbnail_manager.cvar.notify_all();
    Ok(())
}

pub fn add_to_thumbnail_queue(state: &AppState, count: usize, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total += count;
    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );
}

pub fn increment_thumbnail_progress(state: &AppState, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.completed += 1;
    let current = tracker.completed;
    let total = tracker.total;

    if current >= total {
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        let _ = app_handle.emit("thumbnail-generation-complete", true);
    } else {
        drop(tracker);
        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": current, "total": total }),
        );
    }
}

pub(crate) fn emit_thumbnail_generated(
    app_handle: &AppHandle,
    path: &str,
    small_thumbnail_path: &str,
    medium_thumbnail_path: &str,
    rating: Option<u8>,
    is_edited: Option<bool>,
) {
    let _ = app_handle.emit(
        "thumbnail-generated",
        serde_json::json!({
            "path": path,
            "thumbnailPath": small_thumbnail_path,
            "previewPath": medium_thumbnail_path,
            "rating": rating,
            "is_edited": is_edited
        }),
    );
}

#[tauri::command]
pub async fn clear_thumbnail_cache(app_handle: AppHandle) -> Result<(), String> {
    super::blocking(move || clear_thumbnail_cache_blocking(app_handle)).await?
}

fn clear_thumbnail_cache_blocking(app_handle: AppHandle) -> Result<(), String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");

    if thumb_cache_dir.exists() {
        fs::remove_dir_all(&thumb_cache_dir)
            .map_err(|e| format!("Failed to remove thumbnail cache: {}", e))?;
    }

    fs::create_dir_all(&thumb_cache_dir)
        .map_err(|e| format!("Failed to recreate thumbnail cache directory: {}", e))?;

    Ok(())
}

pub fn get_thumb_cache_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

pub fn get_cache_key_hash(path_str: &str) -> Option<String> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);

    let adjustments_bytes = thumbnail_metadata(&source_path, &sidecar_path)
        .map(|meta| serde_json::to_vec(&meta.adjustments).unwrap_or_default())
        .unwrap_or_default();

    compute_thumbnail_cache_hash(path_str, &adjustments_bytes)
}

pub fn get_cached_or_generate_thumbnail_image(
    path_str: &str,
    app_handle: &AppHandle,
    gpu_context: Option<&GpuContext>,
) -> Result<DynamicImage> {
    let thumb_cache_dir = get_thumb_cache_dir(app_handle).map_err(|e| anyhow::anyhow!(e))?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let target_width_small = settings.small_thumbnail_resolution.unwrap_or(480);
    let target_width_medium = settings.medium_thumbnail_resolution.unwrap_or(1280);

    if let Some(cache_hash) = get_cache_key_hash(path_str) {
        let cache_path = thumb_cache_dir.join(format!("{}_medium.jpg", cache_hash));

        if cache_path.exists() {
            if let Ok(image) = image::open(&cache_path) {
                return Ok(image);
            }
            eprintln!(
                "Could not open cached thumbnail, regenerating: {:?}",
                cache_path
            );
        }

        let thumb_image = generate_thumbnail_data(path_str, gpu_context, None, app_handle)?;
        if let (Ok(small_data), Ok(medium_data)) = (
            encode_thumbnail(&thumb_image, target_width_small),
            encode_thumbnail(&thumb_image, target_width_medium),
        ) {
            let _ = fs::write(
                thumb_cache_dir.join(format!("{}_small.jpg", cache_hash)),
                &small_data,
            );
            let _ = fs::write(
                thumb_cache_dir.join(format!("{}_medium.jpg", cache_hash)),
                &medium_data,
            );
        }

        Ok(thumb_image)
    } else {
        generate_thumbnail_data(path_str, gpu_context, None, app_handle)
    }
}

#[cfg(test)]
mod source_preview_tests {
    use super::*;

    /// The import dialog's preview of a photograph not yet in the library:
    /// small, upright, and the file is only read.
    #[test]
    fn previews_a_source_photograph_small_without_touching_it() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("card.png");
        image::RgbImage::from_pixel(1200, 800, image::Rgb([200, 120, 40]))
            .save(&source)
            .unwrap();
        let before = fs::read(&source).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let jpeg = source_preview_jpeg(&source, 320).expect("a preview");
        let preview = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((preview.width(), preview.height()), (320, 213));
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "nothing written"
        );
        assert!(source_preview_jpeg(&directory.path().join("missing.ARW"), 320).is_none());
        let broken = directory.path().join("broken.ARW");
        fs::write(&broken, b"not a raw file").unwrap();
        assert!(source_preview_jpeg(&broken, 320).is_none());
    }

    fn jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        })
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
        out.into_inner()
    }

    /// A TIFF-based RAW container (as DNG, NEF, ARW…) whose only image is an
    /// embedded camera JPEG, with the camera's orientation in IFD0.
    fn raw_with_embedded_jpeg(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let entries: [(u16, u16, u32); 4] = [
            (259, 3, 7),
            (273, 4, 8 + 2 + 4 * 12 + 4),
            (274, 3, orientation as u32),
            (279, 4, jpeg.len() as u32),
        ];
        let mut file = b"II*\0".to_vec();
        file.extend(8u32.to_le_bytes());
        file.extend((entries.len() as u16).to_le_bytes());
        for (tag, kind, value) in entries {
            file.extend(tag.to_le_bytes());
            file.extend(kind.to_le_bytes());
            file.extend(1u32.to_le_bytes());
            file.extend(value.to_le_bytes());
        }
        file.extend(0u32.to_le_bytes());
        file.extend(jpeg);
        file
    }

    fn untouched(directory: &Path, files: &[(&Path, Vec<u8>, std::time::SystemTime)]) {
        for (path, bytes, modified) in files {
            assert_eq!(&fs::read(path).unwrap(), bytes);
            assert_eq!(&fs::metadata(path).unwrap().modified().unwrap(), modified);
        }
        assert_eq!(
            fs::read_dir(directory).unwrap().count(),
            files.len(),
            "nothing written"
        );
    }

    /// On a memory card (a camera's DCIM folder here) the import dialog
    /// reads photographs into memory instead of mapping them, so a card
    /// pulled meanwhile fails a read instead of killing the app (SIGBUS);
    /// the previews are the same.
    #[test]
    fn photographs_on_a_card_are_read_not_memory_mapped_for_import_previews() {
        let directory = tempfile::tempdir().unwrap();
        let card = directory.path().join("DCIM/100MSDCF");
        fs::create_dir_all(&card).unwrap();
        let raw = card.join("DSC00001.DNG");
        let bytes = raw_with_embedded_jpeg(&jpeg_bytes(2400, 1600), 1);
        fs::write(&raw, &bytes).unwrap();
        let disk = directory.path().join("DSC00001.DNG");
        fs::write(&disk, &bytes).unwrap();
        assert!(matches!(
            read_import_source(&raw),
            Some(ImportSource::Read(read)) if read == bytes
        ));
        assert!(matches!(
            read_import_source(&disk),
            Some(ImportSource::Mapped(_))
        ));
        assert_eq!(
            source_large_preview_jpeg(&raw, 2560),
            source_large_preview_jpeg(&disk, 2560)
        );
        assert!(source_preview_jpeg(&raw, 320).is_some());
        assert_eq!(source_capture_info(&raw), source_capture_info(&disk));
        // Empty or gone: nothing, as with a mapped file.
        let empty = card.join("EMPTY.DNG");
        fs::write(&empty, b"").unwrap();
        assert!(read_import_source(&empty).is_none());
        assert!(read_import_source(&card.join("GONE.DNG")).is_none());
    }

    /// The import dialog's large view: a RAW's embedded camera JPEG at its
    /// full size (bigger than the grid's preview, upright), a JPEG file's own
    /// bytes, other images scaled to the requested size; sources only read.
    #[test]
    fn large_source_preview_is_the_full_embedded_jpeg_and_leaves_sources_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("DSC00001.DNG");
        fs::write(&raw, raw_with_embedded_jpeg(&jpeg_bytes(2400, 1600), 6)).unwrap();
        let photo = directory.path().join("IMG_0001.JPG");
        fs::write(&photo, jpeg_bytes(1800, 1200)).unwrap();
        let png = directory.path().join("scan.png");
        image::RgbImage::from_pixel(3000, 2000, image::Rgb([10, 20, 30]))
            .save(&png)
            .unwrap();
        let before: Vec<_> = [&raw, &photo, &png]
            .into_iter()
            .map(|path| {
                (
                    path.as_path(),
                    fs::read(path).unwrap(),
                    fs::metadata(path).unwrap().modified().unwrap(),
                )
            })
            .collect();

        let thumbnail = image::load_from_memory(&source_preview_jpeg(&raw, 320).unwrap()).unwrap();
        let large_jpeg = source_large_preview_jpeg(&raw, 2560).expect("a large preview");
        let large = image::load_from_memory(&large_jpeg).unwrap();
        assert!(large.width().max(large.height()) > thumbnail.width().max(thumbnail.height()));
        assert_eq!((large.width(), large.height()), (2400, 1600));
        assert_eq!(
            image_loader::exif_orientation(&large_jpeg),
            Some(6),
            "the camera's orientation travels with the passed-through JPEG"
        );

        assert_eq!(
            source_large_preview_jpeg(&photo, 2560).unwrap(),
            before[1].1,
            "a JPEG is shown as it is"
        );
        let scaled =
            image::load_from_memory(&source_large_preview_jpeg(&png, 2560).unwrap()).unwrap();
        assert_eq!((scaled.width(), scaled.height()), (2560, 1707));

        // A RAW whose only embedded JPEG is a thumbnail falls back to the
        // largest preview there is rather than showing a postage stamp.
        let small = directory.path().join("small.DNG");
        fs::write(&small, raw_with_embedded_jpeg(&jpeg_bytes(400, 300), 1)).unwrap();
        let small_bytes = fs::read(&small).unwrap();
        let small_modified = fs::metadata(&small).unwrap().modified().unwrap();
        let fallback =
            image::load_from_memory(&source_large_preview_jpeg(&small, 2560).unwrap()).unwrap();
        assert_eq!((fallback.width(), fallback.height()), (400, 300));

        assert!(source_large_preview_jpeg(&directory.path().join("missing.ARW"), 2560).is_none());
        let mut all = before;
        all.push((small.as_path(), small_bytes, small_modified));
        untouched(directory.path(), &all);
    }

    #[test]
    fn capture_info_reads_iso_shutter_aperture_and_focal_length() {
        use little_exif::{
            exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata, rational::uR64,
        };
        let directory = tempfile::tempdir().unwrap();
        let photo = directory.path().join("IMG_0002.JPG");
        let mut jpeg = jpeg_bytes(64, 48);
        let mut metadata = Metadata::new();
        metadata.set_tag(ExifTag::ISO(vec![800]));
        metadata.set_tag(ExifTag::ExposureTime(vec![uR64 {
            nominator: 1,
            denominator: 250,
        }]));
        metadata.set_tag(ExifTag::FNumber(vec![uR64 {
            nominator: 28,
            denominator: 10,
        }]));
        metadata.set_tag(ExifTag::FocalLength(vec![uR64 {
            nominator: 35,
            denominator: 1,
        }]));
        metadata
            .write_to_vec(&mut jpeg, FileExtension::JPEG)
            .unwrap();
        fs::write(&photo, &jpeg).unwrap();
        let modified = fs::metadata(&photo).unwrap().modified().unwrap();

        let info = source_capture_info(&photo);
        assert_eq!(
            info,
            SourceCaptureInfo {
                iso: Some("ISO 800".into()),
                exposure: Some("1/250 s".into()),
                aperture: Some("f/2.8".into()),
                focal_length: Some("35 mm".into()),
            }
        );
        untouched(directory.path(), &[(&photo, jpeg, modified)]);
        assert_eq!(
            source_capture_info(&directory.path().join("missing.NEF")),
            SourceCaptureInfo::default()
        );
    }

    /// Times the import dialog's large previews on the photographs in
    /// `INGEST_PREVIEW_DIR` (use copies): `cargo test -p rawtonic-app
    /// --release --lib times_large_source_previews -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs INGEST_PREVIEW_DIR"]
    fn times_large_source_previews() {
        let directory = std::env::var("INGEST_PREVIEW_DIR").expect("INGEST_PREVIEW_DIR");
        let mut times = Vec::new();
        for entry in fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if !crate::formats::is_supported_image_file(&path) {
                continue;
            }
            let started = std::time::Instant::now();
            let jpeg = source_large_preview_jpeg(&path, 2560);
            let preview_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let info = source_capture_info(&path);
            let info_ms = started.elapsed().as_secs_f64() * 1000.0;
            let size = jpeg
                .as_deref()
                .and_then(|j| {
                    image::ImageReader::new(Cursor::new(j))
                        .with_guessed_format()
                        .ok()
                })
                .and_then(|r| r.into_dimensions().ok());
            println!(
                "{:>8.1} ms preview {:?} {} KB, {:>6.1} ms info {:?} {}",
                preview_ms,
                size,
                jpeg.as_ref().map_or(0, |j| j.len() / 1024),
                info_ms,
                info,
                path.display()
            );
            times.push(preview_ms);
        }
        times.sort_by(f64::total_cmp);
        if !times.is_empty() {
            println!(
                "{} files, median {:.1} ms, max {:.1} ms",
                times.len(),
                times[times.len() / 2],
                times[times.len() - 1]
            );
        }
    }

    /// Times the import dialog's previews on the photographs in
    /// `INGEST_PREVIEW_DIR` (use copies): `cargo test -p rawtonic-app
    /// --release --lib times_source_previews -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs INGEST_PREVIEW_DIR"]
    fn times_source_previews() {
        let directory = std::env::var("INGEST_PREVIEW_DIR").expect("INGEST_PREVIEW_DIR");
        let mut times = Vec::new();
        for entry in fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if !crate::formats::is_supported_image_file(&path) {
                continue;
            }
            let started = std::time::Instant::now();
            let jpeg = source_preview_jpeg(&path, 320);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let size = jpeg
                .as_deref()
                .and_then(|j| image::load_from_memory(j).ok())
                .map(|i| (i.width(), i.height()));
            println!("{:>8.1} ms {:?} {}", ms, size, path.display());
            times.push(ms);
        }
        times.sort_by(f64::total_cmp);
        if !times.is_empty() {
            println!(
                "{} files, median {:.1} ms, max {:.1} ms",
                times.len(),
                times[times.len() / 2],
                times[times.len() - 1]
            );
        }
    }
}

#[cfg(test)]
mod thumbnail_queue_tests {
    use super::*;

    #[test]
    fn raw_thumbnail_cache_invalidates_pre_orientation_pixels_for_originals_and_virtual_copies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.RAF");
        fs::write(&path, b"thumbnail-cache-fixture").unwrap();
        let modified = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for path_str in [
            path.to_string_lossy().to_string(),
            format!("{}?vc=1", path.display()),
        ] {
            for adjustments in [b"null".as_slice(), br#"{"orientationSteps":1}"#] {
                let mut previous = blake3::Hasher::new();
                previous.update(path_str.as_bytes());
                previous.update(&modified.to_le_bytes());
                previous.update(adjustments);
                assert_ne!(
                    compute_thumbnail_cache_hash(&path_str, adjustments).unwrap(),
                    previous.finalize().to_hex().to_string(),
                    "old orientation must not survive the on-disk cache"
                );
            }
        }
    }

    #[test]
    fn raw_thumbnail_cache_renders_tone_slider_recipes_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.ARW");
        fs::write(&path, b"thumbnail-cache-fixture").unwrap();
        let path_str = path.to_str().unwrap();
        let modified = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let previous_hash = |adjustments: &[u8]| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(path_str.as_bytes());
            hasher.update(&modified.to_le_bytes());
            hasher.update(adjustments);
            hasher.update(b"\0embedded-raw-orientation-v1");
            hasher.update(
                format!("\0raw-engine-{}", crate::preview_proxy::ENGINE_VERSION).as_bytes(),
            );
            hasher.finalize().to_hex().to_string()
        };
        for adjustments in [
            b"null".as_slice(),
            br#"{"exposure":1}"#,
            br#"{"contrast":0,"shadows":0,"whites":0,"blacks":0,"highlights":-40}"#,
        ] {
            assert_eq!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments),
                "thumbnails without the tone sliders stay cached"
            );
        }
        for adjustments in [
            br#"{"contrast":20}"#.as_slice(),
            br#"{"shadows":-30}"#,
            br#"{"whites":-100}"#,
            br#"{"blacks":5}"#,
        ] {
            assert_ne!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments),
                "tone-slider thumbnails are rendered again"
            );
        }
        assert!(uses_tone_sliders(&serde_json::json!({
            "masks": [{ "adjustments": { "exposure": 1 } }, { "adjustments": { "whites": -20 } }]
        })));
        assert!(!uses_tone_sliders(&serde_json::json!({
            "masks": [{ "adjustments": { "exposure": 1, "contrast": 0 } }]
        })));
    }

    #[test]
    fn masked_thumbnail_cache_invalidates_previous_renderer_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.jpg");
        fs::write(&path, b"thumbnail-cache-fixture").unwrap();
        let path_str = path.to_str().unwrap();
        let modified = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let previous_hash = |adjustments: &[u8]| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(path_str.as_bytes());
            hasher.update(&modified.to_le_bytes());
            hasher.update(adjustments);
            hasher.finalize().to_hex().to_string()
        };
        for adjustments in [
            b"".as_slice(),
            b"null",
            br#"{"exposure":1}"#,
            br#"{"masks":[]}"#,
            br#"{"masks":null}"#,
            br#"{"masks":{}}"#,
        ] {
            assert_eq!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments),
                "unmasked caches remain reusable"
            );
        }
        // The epoch also applies to empty/hidden/malformed definitions in a
        // nonempty array, since skipped-layer handling is part of this change.
        for adjustments in [
            br#"{"masks":[{}]}"#.as_slice(),
            br#"{"masks":[{"visible":false,"subMasks":[]}]}"#,
            br#"{"masks":[{"visible":true,"subMasks":[]}]}"#,
        ] {
            assert_ne!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments),
                "old masked pixels must be regenerated"
            );
        }
    }

    #[test]
    fn viewport_queue_is_bounded_ordered_and_excludes_inflight_duplicates() {
        let mut paths = vec!["busy".to_string(), "first".to_string(), "first".to_string()];
        paths.extend((0..600).map(|i| i.to_string()));
        let processing = ["busy".to_string()].into_iter().collect();
        let queue = visible_thumbnail_paths(paths, &processing);
        assert_eq!(queue.len(), 500);
        assert_eq!(queue[0], "first");
        assert_eq!(queue[499], "498");
        assert!(visible_thumbnail_paths(Vec::new(), &processing).is_empty());
    }
}
