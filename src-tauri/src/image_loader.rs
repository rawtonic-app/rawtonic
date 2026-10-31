//! Desktop glue for `rawtonic_engine::image_loader` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::image_loader::*;

use crate::app_settings::{AppSettings, load_settings};
use crate::app_state::{AppState, LoadedImage, SourceSignal};
use crate::exif_processing;
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::image_processing::ImageMetadata;
use anyhow::{Result, anyhow};
use image::{DynamicImage, GenericImageView};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::panic;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(feature = "performance-profiling")]
use std::time::Instant;

#[derive(serde::Serialize)]
pub struct LoadImageResult {
    pub width: u32,
    pub height: u32,
    pub metadata: ImageMetadata,
    pub exif: HashMap<String, String>,
    pub is_raw: bool,
    /// The AI Denoise amount the source was developed with (0 … 100).
    pub ai_denoise: f32,
    /// Its `aiDenoiseVersion` (1 when off).
    pub ai_denoise_version: u8,
    /// Why the recipe's AI Denoise could not be applied, if it could not.
    pub ai_denoise_error: Option<String>,
    /// The RAW is offline and the photograph opened on its preview proxy:
    /// "full" (its 1:1 preview, full resolution) or "standard".
    pub offline: Option<&'static str>,
    /// The source was developed for the multi-scale colour noise filter
    /// (without the decode-time chroma clean-up); None for preview data.
    pub color_noise_multiscale: Option<bool>,
    /// The source was developed with `DeadTest::Adjacent` (`renderVersion`
    /// 3); None for preview data.
    pub dead_pixel_adjacent: Option<bool>,
    /// The `sourceFrameVersion` a first edit made on what is shown carries
    /// when it is not the newest (`new_edit_frame`); None otherwise.
    pub new_edit_frame: Option<u32>,
    /// The RAW data could not be developed and the camera's embedded preview
    /// is shown instead (an unsupported camera or RAW mode, a damaged file).
    pub developed_from_preview: bool,
    /// ... and a camera sample could help (Develop offers camera help): not
    /// for a format no open decoder reads.
    pub camera_help: bool,
}

pub fn load_and_composite(
    base_image: &[u8],
    path: &str,
    adjustments: &Value,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let base_image = load_base_image_for_recipe(
        base_image,
        path,
        use_fast_raw_dev,
        settings,
        cancel_token,
        None,
        adjustments,
    )?;
    match composite_patches_for_render(&base_image, adjustments)? {
        Cow::Borrowed(_) => Ok(base_image),
        Cow::Owned(composited) => Ok(composited),
    }
}

pub fn load_base_image_from_bytes(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    load_base_image_from_bytes_with(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        None,
    )
}

/// `load_base_image_from_bytes` with AI Denoise (ai_denoise.rs) for full
/// RAW developments.
pub fn load_base_image_from_bytes_with(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
) -> Result<DynamicImage> {
    load_base_image_from_bytes_with_draft(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        denoise,
        None,
    )
}

/// `load_base_image_from_bytes_with`; a full RAW development hands `draft`
/// an early reduced development first (raw_processing::DraftSink).
pub fn load_base_image_from_bytes_with_draft(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    draft: Option<Arc<rawtonic_rawdecode::raw_processing::DraftSink>>,
) -> Result<DynamicImage> {
    load_base_image_from_bytes_for(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        denoise,
        draft,
        None,
    )
}

/// `load_base_image_from_bytes_with` for the recipe rendered,
/// `adjustments`: its colour noise filter decides the decode-time chroma
/// clean-up (`load_base_image_from_bytes_for`).
pub fn load_base_image_for_recipe(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    adjustments: &Value,
) -> Result<DynamicImage> {
    load_base_image_for_recipe_measured(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        denoise,
        adjustments,
    )
    .map(|(image, _)| image)
}

/// `load_base_image_for_recipe` with the measures of a full RAW decode
/// (`DecodeMeasures`).
pub fn load_base_image_for_recipe_measured(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    adjustments: &Value,
) -> Result<(DynamicImage, Option<DecodeMeasures>)> {
    let variant = decode_variant(adjustments, path_for_ext_check);
    load_base_image_measured_for(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        denoise,
        None,
        Some(variant),
    )
}

/// What the stored recipe of the photograph at `path` (a source or a
/// virtual copy's path) asks of its decode (`decode_variant`): a
/// never-edited RAW renders as a new edit. A source path decides by its
/// master recipe.
pub fn stored_decode_variant(path: &str) -> DecodeVariant {
    let (source, sidecar) = parse_virtual_path(path);
    let metadata = exif_processing::load_sidecar(&sidecar);
    decode_variant(&metadata.adjustments, &source.to_string_lossy())
}

/// The base image options of the app's preferences for a photograph whose
/// recipe asks for `variant` (`BaseImageOptions::for_variant`).
pub fn base_options(settings: &AppSettings, variant: DecodeVariant) -> BaseImageOptions {
    BaseImageOptions::from_preferences(
        settings.raw_highlight_compression,
        settings.linear_raw_mode.clone(),
        settings.raw_preprocessing_color_nr,
        settings.raw_preprocessing_sharpening,
        settings.apply_preprocessing_to_non_raws,
    )
    .for_variant(variant)
}

/// `load_base_image_from_bytes_with_draft` for a recipe whose colour noise
/// filter and dead-photosite test are known: `variant` (`decode_variant`
/// of the recipe rendered); None decides by the photograph's stored master
/// recipe (`stored_decode_variant`). Fast developments never run the
/// clean-up; they keep the recipe's dead-photosite test (it runs before
/// their demosaicing too).
#[allow(clippy::too_many_arguments)]
pub fn load_base_image_from_bytes_for(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    draft: Option<Arc<rawtonic_rawdecode::raw_processing::DraftSink>>,
    variant: Option<DecodeVariant>,
) -> Result<DynamicImage> {
    load_base_image_measured_for(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        settings,
        cancel_token,
        denoise,
        draft,
        variant,
    )
    .map(|(image, _)| image)
}

/// `load_base_image_from_bytes_for` with the measures of a full RAW decode
/// (`DecodeMeasures`; None for fast developments, Apple's RAW engine,
/// embedded previews and other formats), which travel with its pixels.
#[allow(clippy::too_many_arguments)]
pub fn load_base_image_measured_for(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    draft: Option<Arc<rawtonic_rawdecode::raw_processing::DraftSink>>,
    variant: Option<DecodeVariant>,
) -> Result<(DynamicImage, Option<DecodeMeasures>)> {
    let variant = variant.unwrap_or_else(|| stored_decode_variant(path_for_ext_check));
    let variant = DecodeVariant {
        multiscale: variant.multiscale && !use_fast_raw_dev,
        ..variant
    };
    let mut options = base_options(settings, variant);
    options.mosaic_denoise = denoise;
    options.draft = draft;

    {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("source_exif_cache");
        crate::exif_processing::persist_exif_if_missing(
            Path::new(path_for_ext_check),
            path_for_ext_check,
            bytes,
        );
    }

    // AI Denoise works on Rawtonic's own mosaic, so it develops with rawler.
    if is_raw_file(path_for_ext_check)
        && !use_fast_raw_dev
        && settings.use_apple_raw9.unwrap_or(false)
        && options.mosaic_denoise.is_none()
    {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }

        match crate::apple_raw::develop_raw9(
            bytes,
            path_for_ext_check,
            &crate::apple_raw::Raw9Options::for_loading(),
        ) {
            Ok(image) => return Ok((image, None)),
            Err(e) => log::warn!(
                "Apple RAW 9 unavailable for '{}', falling back to rawler: {}",
                path_for_ext_check,
                e
            ),
        }
    }

    crate::image_loader::load_base_image_measured(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        &options,
        cancel_token,
    )
}

#[tauri::command]
pub async fn load_embedded_preview(path: String) -> Result<tauri::ipc::Response, String> {
    tokio::task::spawn_blocking(move || {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("selected_embedded_preview");
        let (source_path, _) = parse_virtual_path(&path);
        if !is_raw_file(&source_path) || crate::file_management::is_cloud_placeholder(&source_path)
        {
            return Ok(tauri::ipc::Response::new(Vec::new()));
        }
        let bytes = read_file_mapped(&source_path).map_err(|error| error.to_string())?;
        let jpeg = panic::catch_unwind(panic::AssertUnwindSafe(|| priority_embedded_jpeg(&bytes)))
            .ok()
            .flatten()
            .unwrap_or_default();
        Ok(tauri::ipc::Response::new(jpeg))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Why a tool that needs full-resolution pixels cannot run on an offline
/// photograph shown from its Standard preview.
pub const OFFLINE_NEEDS_ORIGINAL: &str = "This photograph is offline and shown from its Standard preview: this tool needs the RAW file or a 1:1 preview. Reconnect the RAW file, or build 1:1 previews while it is available.";

/// The open photograph's full decode. While it is still running behind its
/// preview proxy (the editor already shows and edits the photograph), waits
/// for it (briefly, a decode takes well under a second) instead of failing
/// the command that needs full resolution. An offline photograph on its
/// Standard preview gets no decode: `OFFLINE_NEEDS_ORIGINAL` at once (on its
/// 1:1 preview, that is the source and is returned).
pub fn wait_for_original_image(state: &AppState) -> Result<LoadedImage, String> {
    state
        .source_signal
        .wait_for(DECODE_WAIT, || original_image_settled(state))
        .unwrap_or_else(|| Err(no_original(state)))
}

/// `wait_for_original_image` for async commands: waits without holding
/// the async runtime's worker thread.
pub async fn original_image(state: &AppState) -> Result<LoadedImage, String> {
    state
        .source_signal
        .wait_for_async(DECODE_WAIT, || original_image_settled(state))
        .await
        .unwrap_or_else(|| Err(no_original(state)))
}

const DECODE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const NO_PHOTOGRAPH: &str = "No photograph is loaded";

/// The answer for `wait_for_original_image` once there is one: the full
/// decode, or why there will be none; `None` while it is still decoding.
fn original_image_settled(state: &AppState) -> Option<Result<LoadedImage, String>> {
    let original = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let offline = state
        .offline_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    let proxy = state
        .proxy_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    settled(original, offline, proxy)
}

/// `original_image_settled` from the open photo's source slots: its full
/// decode (`original`), whether it is offline, and whether a preview proxy
/// stands in while something decodes.
fn settled(
    original: Option<LoadedImage>,
    offline: bool,
    proxy: bool,
) -> Option<Result<LoadedImage, String>> {
    match (original, offline, proxy) {
        (Some(loaded), _, _) => Some(Ok(loaded)),
        // On its Standard preview: no decode will come.
        (None, true, true) => Some(Err(OFFLINE_NEEDS_ORIGINAL.to_string())),
        // Its 1:1 preview is being made the source (`open_offline`).
        (None, true, false) => None,
        (None, false, true) => None,
        (None, false, false) => Some(Err(NO_PHOTOGRAPH.to_string())),
    }
}

/// Why a wait for the full decode ended without one.
fn no_original(state: &AppState) -> String {
    let offline = state
        .offline_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    if offline {
        OFFLINE_NEEDS_ORIGINAL
    } else {
        NO_PHOTOGRAPH
    }
    .to_string()
}

fn publish_loaded_image(
    original_image: &std::sync::Mutex<Option<LoadedImage>>,
    signal: &SourceSignal,
    generation: &AtomicUsize,
    expected_generation: usize,
    loaded: LoadedImage,
) -> Result<(), String> {
    let mut original = original_image.lock().unwrap_or_else(|e| e.into_inner());
    // A newer cache hit may finish while this request is waiting for the
    // source lock. Check after acquiring it so an old decode cannot replace it.
    if generation.load(Ordering::SeqCst) != expected_generation {
        return Err("Load cancelled".to_string());
    }
    *original = Some(loaded);
    drop(original);
    signal.changed();
    Ok(())
}

/// Full size of the photograph whose preview proxy `load_image_proxy` made
/// current.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInfo {
    pub width: u32,
    pub height: u32,
    pub is_raw: bool,
    /// See `LoadImageResult::new_edit_frame`.
    pub new_edit_frame: Option<u32>,
}

/// Makes the photograph's stored preview proxy (preview_proxy.rs) the
/// preview source until `load_image` has decoded it, so Develop shows the
/// developed photograph at once. None without a stored proxy (`load_image`
/// then hands over a draft, `DRAFT_EVENT`) or when the full decode is
/// already in memory (then `load_image` is quick anyway): decoded before,
/// or kept by the prefetch for this photograph (`proxy_worker`).
#[tauri::command]
pub async fn load_image_proxy(
    path: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Option<ProxyInfo>, String> {
    #[cfg(feature = "performance-profiling")]
    let performance_start = Instant::now();
    let (source_path, _) = parse_virtual_path(&path);
    let source_path_str = source_path.to_string_lossy().to_string();
    if !is_raw_file(&source_path_str) {
        return Ok(None);
    }
    open_source(&state, &path);
    crate::editor_preview::release_still_for(&state, &path);
    let settings = load_settings(app_handle).unwrap_or_default();
    let variant = stored_decode_variant(&path);
    let decoded_key = decoded_cache_key(&source_path_str, None, variant);
    if state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&decoded_key)
        .is_some()
        || crate::proxy_worker::keeps_decode_of(
            &decoded_key,
            &crate::preview_proxy::current_key(&source_path, &settings, variant),
        )
    {
        return Ok(None);
    }
    // Only previews made for this photograph's decode variant (a virtual
    // copy's may differ from its master's) stand in until the decode.
    let wanted = stored_decode_variant(&path);
    if wanted != stored_decode_variant(&source_path_str) {
        return Ok(None);
    }
    let remembered = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path, wanted);
    let loaded = if let Some(loaded) = remembered {
        Some(loaded)
    } else {
        let read_path = path.clone();
        let proxy = tokio::task::spawn_blocking(move || {
            crate::preview_proxy::read(&source_path, &settings)
        })
        .await
        .map_err(|e| e.to_string())?;
        proxy.map(|proxy| {
            state
                .proxy_memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remember(&read_path, proxy)
        })
    };
    let Some(loaded) = loaded else {
        return Ok(None);
    };
    // Another photograph was asked for meanwhile, or the decode finished.
    let current = state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let decoded = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|o| o.path == path);
    if current != path || decoded {
        return Ok(None);
    }
    let info = ProxyInfo {
        width: loaded.full_width,
        height: loaded.full_height,
        is_raw: true,
        new_edit_frame: new_edit_frame(
            &source_path_str,
            loaded.made_for.map_or(0, |made_for| made_for.frame),
        ),
    };
    // The preview job renders the proxy, not the previous photograph's
    // decode (`load_image`, which clears it too, may start later).
    if state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|o| o.path != path)
    {
        clear_source_caches(&state);
    }
    *state.proxy_image.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "load_image_proxy", "ms": performance_start.elapsed().as_secs_f64() * 1000.0,
            "width": info.width, "height": info.height,
            "start_epoch_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
                - performance_start.elapsed().as_secs_f64() * 1000.0
        })
    );
    Ok(Some(info))
}

/// How long the preview proxy written after a decode waits for the decode's
/// first frame (none comes when Develop is left meanwhile).
const DECODED_FRAME_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The decode to write a preview proxy from, once a frame of it was
/// rendered: written beside that frame, the proxy and its new thumbnail
/// (developed again, with their own lateral CA estimate) took the cores the
/// frame needed, 0.8 s later for a 26 MP RAF on a Ryzen 5 3600X. None when
/// the photograph was left before its frame (`still_open`) or the decode is
/// gone from memory: the proxy is then written by a later decode of it.
fn decode_for_proxy(
    rendered: &crate::app_state::RenderedSource,
    decode: &std::sync::Weak<DynamicImage>,
    still_open: impl Fn() -> bool,
    timeout: std::time::Duration,
) -> Option<Arc<DynamicImage>> {
    if !rendered.wait(decode, timeout) && !still_open() {
        return None;
    }
    decode.upgrade()
}

/// An offline virtual copy whose recipe develops the RAW differently from
/// the photograph's previews (`preview_proxy::stands_for`).
const OTHER_VARIANT_OFFLINE: &str = "This copy develops the RAW differently from the photograph's stored previews (another colour noise filter or render version); reconnect the RAW to open it";

/// The `sourceFrameVersion` a first edit of the RAW at `source` carries when
/// it is made on a source of frame `shown` (`DecodeVariant::frame`): None
/// where that is the newest version's frame, else `shown` (a version that
/// develops it). A never-edited photograph shown offline on previews of an
/// older frame (no decode can follow) is edited in the frame it shows
/// (src/hooks/useImageLoader.ts), so its edit never lands on another frame.
pub fn new_edit_frame(source: &str, shown: u32) -> Option<u32> {
    let newest =
        rawtonic_engine::source_frame::effective(source, rawtonic_engine::source_frame::VERSION);
    (shown != newest).then_some(shown)
}

const NOT_BAYER: &str = "AI Denoise works on Bayer sensors only; this photograph's sensor (X-Trans, Foveon or a linear DNG) is not supported";

/// The decoded-image cache key of a full decode of the source at `path`:
/// the path, plus the AI Denoise amount and marks for a base developed for
/// the multi-scale colour noise filter (no decode-time chroma clean-up) and
/// for the second dead-photosite test, and the frame (`source_frame`), so
/// bases of different variants never stand for each other. A base of the default variant without AI Denoise
/// keeps the plain path.
pub fn decoded_cache_key(
    path: &str,
    denoise: Option<&rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    variant: DecodeVariant,
) -> String {
    let mut key = path.to_string();
    if let Some(d) = denoise {
        key.push_str(&format!("\u{1}ai-denoise {:.3}", d.amount));
        // Versions differ in the noise scale and the model.
        if d.noise_scale != 1.0 {
            key.push_str(&format!(" ×{:.4}", d.noise_scale));
        }
        key.push_str(&format!(" {:p}", Arc::as_ptr(&d.denoiser) as *const ()));
    }
    if variant.multiscale {
        key.push_str("\u{1}cnr-ms");
    }
    if variant.dead_test == DeadTest::Adjacent {
        key.push_str("\u{1}dead-adjacent");
    }
    if variant.frame > 0 {
        key.push_str(&format!("\u{1}frame {}", variant.frame));
    }
    key
}

/// Drops the open photograph's source and everything derived from it.
fn clear_source_caches(state: &AppState) {
    *state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    clear_derived_caches(state);
}

/// Makes `path` the open photograph (`current_source`). The bitmaps the
/// editor sent for it (`patch_cache`: AI patches, AI mask and relight data,
/// in the photograph's own frame) are dropped only for another photograph:
/// they do not depend on its decode, and the editor sends each just once
/// per photograph it opens (`patchesSentToBackend`), also when a stored
/// proxy's frame carried them before `load_image` decodes it.
pub(crate) fn open_source(state: &AppState, path: &str) {
    let mut current = state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if *current != path {
        state
            .patch_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *current = path.to_string();
    }
}

/// Drops what was computed from the open photograph's source (previews,
/// warps, masks, tool results).
fn clear_derived_caches(state: &AppState) {
    *state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *state
        .gpu_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *state
        .full_warped_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *state
        .full_transformed_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *state
        .patched_warped_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    state
        .mask_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    state
        .mask_thumbnail_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    state
        .geometry_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    *state.merge_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Develops the open photograph again with AI Denoise at `amount` (0 … 100)
/// of `version` (`aiDenoiseVersion`, None: 1), for the colour noise filter
/// of its recipe (`multiscale`, see `load_base_image_from_bytes_for`; None:
/// its stored recipe's) and for its dead-photosite test
/// (`dead_pixel_adjacent`: `DeadTest::Adjacent`; None: its stored
/// recipe's), and makes
/// that the source; returns the AI Denoise amount it was developed with.
/// The editor calls it when the recipe's AI Denoise amount or colour noise
/// filter differs from the source's (src/utils/aiDenoise.ts). The frame
/// stays the open source's: a source of another frame (another size) is
/// opened again instead (`load_image`), and a develop that would change the
/// size fails rather than replace the source under the editor's overlays.
#[tauri::command]
pub async fn apply_ai_denoise(
    path: String,
    amount: f32,
    version: Option<u8>,
    multiscale: Option<bool>,
    dead_pixel_adjacent: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<f32, String> {
    let (source_path, _) = parse_virtual_path(&path);
    let source = source_path.to_string_lossy().to_string();
    if !is_raw_file(&source) {
        return Err("AI Denoise works on RAW files only".to_string());
    }
    let stored = stored_decode_variant(&path);
    let open = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|o| o.path == path)
        .map(|o| (o.decode.map(|d| d.frame), o.image.dimensions()));
    let variant = DecodeVariant {
        multiscale: multiscale.unwrap_or(stored.multiscale),
        dead_test: match dead_pixel_adjacent {
            Some(true) => DeadTest::Adjacent,
            Some(false) => DeadTest::SameColour,
            None => stored.dead_test,
        },
        frame: open.and_then(|(frame, _)| frame).unwrap_or(stored.frame),
    };
    let recipe =
        serde_json::json!({ "aiDenoise": amount, "aiDenoiseVersion": version.unwrap_or(1) });
    let denoise = crate::ai_denoise::option_for(&app_handle, &recipe).await?;
    let applied = if denoise.is_some() {
        amount.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let my_generation = state.load_image_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let tracker = state.load_image_generation.clone();
    let key = decoded_cache_key(&source, denoise.as_ref(), variant);
    let cached = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_measured(&key);
    let (image, measures) = match cached {
        Some((image, _, measures)) => (image, measures),
        None => {
            let settings = load_settings(app_handle.clone()).unwrap_or_default();
            let cancel_token = Some((tracker, my_generation));
            let decode_source = source.clone();
            let ((image, measures), exif) = tokio::task::spawn_blocking(move || {
                let bytes = fs::read(&decode_source).map_err(|e| e.to_string())?;
                let image = load_base_image_measured_for(
                    &bytes,
                    &decode_source,
                    false,
                    &settings,
                    cancel_token,
                    denoise.clone(),
                    None,
                    Some(variant),
                )
                .map_err(|e| e.to_string())?;
                if denoise.as_ref().is_some_and(|d| !d.was_applied()) {
                    return Err(NOT_BAYER.to_string());
                }
                let exif = exif_processing::read_exif_data(&decode_source, &bytes);
                Ok::<_, String>((image, exif))
            })
            .await
            .map_err(|e| e.to_string())??;
            let image = Arc::new(image);
            state
                .decoded_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, image.clone(), exif, measures);
            (image, measures)
        }
    };
    if *state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        != path
    {
        return Err("Load cancelled".to_string());
    }
    // Developed again in the same frame: the same size as the source it
    // replaces.
    if let Some((_, size)) = open
        && image.dimensions() != size
    {
        return Err(format!(
            "Developed again at {:?} instead of the open {:?}: the photograph has to be opened again",
            image.dimensions(),
            size
        ));
    }
    publish_loaded_image(
        &state.original_image,
        &state.source_signal,
        &state.load_image_generation,
        my_generation,
        LoadedImage {
            path,
            image,
            is_raw: true,
            decode: Some(variant),
            measures,
        },
    )?;
    clear_derived_caches(&state);
    Ok(applied)
}

/// Event with a never-opened photograph's draft (`DraftInfo`), made by its
/// full decode before demosaicing and shown until the decode is in.
pub const DRAFT_EVENT: &str = "develop-draft-ready";

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DraftInfo {
    path: String,
    width: u32,
    height: u32,
    is_raw: bool,
}

/// How long the full development waits for its draft's first frame (none
/// comes while Develop is not shown).
const DRAFT_FRAME_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// The draft sink of the full decode of `path` (load request `generation`):
/// the draft becomes the preview source while that photograph is still the
/// one asked for and shows nothing else, and the editor is told. Never
/// stored as a preview proxy. Returns once a frame of the draft is rendered
/// (at most `DRAFT_FRAME_WAIT`): the full development starts then. Beside
/// it, the frame's CPU work (colour noise pyramid) took three times as long:
/// a 24 MP CR3's first frame came 0.5 s after the draft on a Ryzen 5 3600X.
fn draft_sink(
    app_handle: &tauri::AppHandle,
    path: &str,
    generation: usize,
) -> Arc<rawtonic_rawdecode::raw_processing::DraftSink> {
    use tauri::{Emitter, Manager};
    let app = app_handle.clone();
    let path = path.to_string();
    Arc::new(rawtonic_rawdecode::raw_processing::DraftSink(Box::new(
        move |image, (full_width, full_height)| {
            let state = app.state::<AppState>();
            if state.load_image_generation.load(Ordering::SeqCst) != generation
                || *state
                    .current_source
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    != path
            {
                return;
            }
            let image = Arc::new(image);
            let draft = Arc::downgrade(&image);
            {
                let mut proxy = state.proxy_image.lock().unwrap_or_else(|e| e.into_inner());
                if proxy.as_ref().is_some_and(|p| p.path == path) {
                    return;
                }
                *proxy = Some(crate::app_state::LoadedProxy {
                    path: path.clone(),
                    image,
                    full_width,
                    full_height,
                    is_raw: true,
                    made_for: None,
                    measures: None,
                });
            }
            #[cfg(feature = "performance-profiling")]
            log::info!(
                "[PERF] {}",
                serde_json::json!({"stage": "develop_draft", "width": full_width, "height": full_height,
                    "epoch_ms": crate::performance::epoch_ms()})
            );
            let _ = app.emit(
                DRAFT_EVENT,
                DraftInfo {
                    path: path.clone(),
                    width: full_width,
                    height: full_height,
                    is_raw: true,
                },
            );
            #[cfg(feature = "performance-profiling")]
            let waited = Instant::now();
            let _rendered = state.rendered_source.wait(&draft, DRAFT_FRAME_WAIT);
            #[cfg(feature = "performance-profiling")]
            log::info!(
                "[PERF] {}",
                serde_json::json!({"stage": "draft_frame_wait", "ms": waited.elapsed().as_secs_f64() * 1000.0,
                    "rendered": _rendered})
            );
        },
    )))
}

#[tauri::command]
pub async fn load_image(
    path: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<LoadImageResult, String> {
    #[cfg(feature = "performance-profiling")]
    let performance_start = Instant::now();
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({"stage": "load_image_received", "epoch_ms": crate::performance::epoch_ms()})
    );
    let my_generation = state.load_image_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let generation_tracker = state.load_image_generation.clone();
    let cancel_token = Some((generation_tracker.clone(), my_generation));
    open_source(&state, &path);
    crate::editor_preview::release_still_for(&state, &path);
    *state
        .offline_source
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    {
        // Another photograph's proxy is of no use any more.
        let mut proxy = state.proxy_image.lock().unwrap_or_else(|e| e.into_inner());
        if proxy.as_ref().is_some_and(|p| p.path != path) {
            *proxy = None;
        }
    }
    state.source_signal.changed();

    clear_source_caches(&state);

    let (source_path, sidecar_path) = parse_virtual_path(&path);
    let source_path_str = source_path.to_string_lossy().to_string();

    let metadata: ImageMetadata = crate::exif_processing::load_sidecar(&sidecar_path);

    // An offline RAW renders with what its previews keep of the file (base
    // look, colour, lens defaults); one that can be read with its file.
    let stored_exif = if is_raw_file(&source_path_str) {
        crate::preview_proxy::prepare_offline(&source_path)
    } else {
        None
    };

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let proxy_settings = settings.clone();

    let path_clone = source_path_str.clone();

    // AI Denoise from the recipe; if its model cannot be had, the photograph
    // still opens and the frontend reports why.
    let (denoise, mut ai_denoise_error) = if is_raw_file(&source_path_str) {
        match crate::ai_denoise::option_for(&app_handle, &metadata.adjustments).await {
            Ok(denoise) => (denoise, None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    let mut ai_denoise = if denoise.is_some() {
        rawtonic_engine::image_loader::ai_denoise_amount(&metadata.adjustments) * 100.0
    } else {
        0.0
    };
    let ai_denoise_version =
        rawtonic_engine::image_loader::ai_denoise_settings(&metadata.adjustments)
            .filter(|_| denoise.is_some())
            .map_or(1, |ai| ai.version);
    // The opened recipe decides the decode-time chroma clean-up, the
    // dead-photosite test and the frame (a virtual copy's own recipe).
    let variant = decode_variant(&metadata.adjustments, &source_path_str);
    // A photograph shown on nothing developed yet (no stored proxy) gets a
    // draft from its decode.
    let draft = (is_raw_file(&source_path_str)
        && state
            .proxy_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|p| p.path != path))
    .then(|| draft_sink(&app_handle, &path, my_generation));
    let mut cache_key = decoded_cache_key(&source_path_str, denoise.as_ref(), variant);
    let decode_denoise = denoise.clone();

    let cached_data = {
        let mut cache = state
            .decoded_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let cached = cache.get_measured(&cache_key);
        if cached.is_none() {
            cache.make_room_for_next();
        }
        cached
    };

    #[cfg(feature = "performance-profiling")]
    let was_cached = cached_data.is_some();
    let (pristine_arc, exif_data, measures) = if let Some((cached_img, cached_exif, measures)) =
        cached_data
    {
        (cached_img, cached_exif, measures)
    } else {
        // Offline (missing, unreadable or not downloaded): the photograph
        // opens on its preview proxies, as Lightroom's smart previews and
        // Capture One's previews of offline images.
        if is_raw_file(&source_path_str)
            && !crate::preview_proxy::source_available(&source_path)
            && let Some(result) = open_offline(
                &state,
                &path,
                &source_path,
                my_generation,
                metadata.clone(),
                stored_exif,
                &settings,
            )
            .await?
        {
            return Ok(result);
        }
        if let Some(message) = offline_without_preview(&source_path) {
            return Err(message);
        }
        if crate::file_management::is_cloud_placeholder(&source_path) {
            return Err(format!(
                "'{}' is stored in iCloud and hasn't been downloaded yet. Download it in Finder, then try again.",
                source_path_str
            ));
        }

        let held_key = cache_key.clone();
        let ((pristine_img, measures), exif_data_loaded) = tokio::task::spawn_blocking(move || {
            if generation_tracker.load(Ordering::SeqCst) != my_generation {
                return Err("Load cancelled".to_string());
            }
            #[cfg(feature = "performance-profiling")]
            log::info!(
                "[PERF] {}",
                serde_json::json!({"stage": "load_image_decode_start", "epoch_ms": crate::performance::epoch_ms()})
            );
            // The multi view's background work holds the decodes of other
            // cells for their 1:1 previews: a cell made the primary opens
            // on its decode (finished first, when under way) instead of
            // decoding the RAW again.
            // Stepping through photographs: the prefetch decoding this one
            // ahead is finished and taken over (one decoding another is
            // cancelled for this decode).
            let held = crate::multi_view_prefetch::held_or_pending_decode(
                &held_key,
                &settings,
                std::time::Duration::from_secs(3),
            )
            .or_else(|| {
                crate::proxy_worker::decode_for_open(
                    &held_key,
                    crate::preview_proxy::current_key(Path::new(&path_clone), &settings, variant),
                    std::time::Duration::from_secs(3),
                )
            });
            // Its decode goes first (the background work waits).
            let _decoding = held.is_none().then(crate::multi_view_prefetch::PrimaryDecode::start);

            #[allow(clippy::type_complexity)]
            let result: Result<((Arc<DynamicImage>, Option<DecodeMeasures>), HashMap<String, String>), String> =
                (|| match read_file_mapped(Path::new(&path_clone)) {
                    Ok(mmap) => {
                        if generation_tracker.load(Ordering::SeqCst) != my_generation {
                            return Err("Load cancelled".to_string());
                        }
                        if let Some(held) = held {
                            let exif = exif_processing::read_exif_data(&path_clone, &mmap);
                            return Ok((held, exif));
                        }

                        let (img, measures) = load_base_image_measured_for(
                            &mmap,
                            &path_clone,
                            false,
                            &settings,
                            cancel_token.clone(),
                            decode_denoise.clone(),
                            draft.clone(),
                            Some(variant),
                        )
                        .map_err(|e| e.to_string())?;
                        let exif = {
                            #[cfg(feature = "performance-profiling")]
                            let _stage =
                                crate::performance::PerformanceStage::new("source_exif_read");
                            exif_processing::read_exif_data(&path_clone, &mmap)
                        };
                        Ok(((Arc::new(img), measures), exif))
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to memory-map file '{}': {}. Falling back to standard read.",
                            path_clone,
                            e
                        );
                        let bytes = fs::read(&path_clone).map_err(|io_err| {
                            format!("Fallback read failed for {}: {}", path_clone, io_err)
                        })?;

                        if generation_tracker.load(Ordering::SeqCst) != my_generation {
                            return Err("Load cancelled".to_string());
                        }

                        let (img, measures) = load_base_image_measured_for(
                            &bytes,
                            &path_clone,
                            false,
                            &settings,
                            cancel_token.clone(),
                            decode_denoise.clone(),
                            draft.clone(),
                            Some(variant),
                        )
                        .map_err(|e| e.to_string())?;
                        let exif = {
                            #[cfg(feature = "performance-profiling")]
                            let _stage =
                                crate::performance::PerformanceStage::new("source_exif_read");
                            exif_processing::read_exif_data(&path_clone, &bytes)
                        };
                        Ok(((Arc::new(img), measures), exif))
                    }
                })();
            result
        })
        .await
        .map_err(|e| e.to_string())??;

        let arc_img = pristine_img;
        // A sensor the denoiser skips develops as without AI Denoise.
        if denoise.as_ref().is_some_and(|d| !d.was_applied()) {
            ai_denoise = 0.0;
            ai_denoise_error = Some(NOT_BAYER.to_string());
            cache_key = decoded_cache_key(&source_path_str, None, variant);
        }

        state
            .decoded_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                cache_key.clone(),
                arc_img.clone(),
                exif_data_loaded.clone(),
                measures,
            );

        (arc_img, exif_data_loaded, measures)
    };

    if state.load_image_generation.load(Ordering::SeqCst) != my_generation {
        return Err("Load cancelled".to_string());
    }

    let is_raw = is_raw_file(&source_path_str);

    let (orig_width, orig_height) = pristine_arc.dimensions();

    // Weak: a decode left for another photograph stays only as long as the
    // decoded-image cache keeps it.
    let proxy_source = Arc::downgrade(&pristine_arc);
    publish_loaded_image(
        &state.original_image,
        &state.source_signal,
        &state.load_image_generation,
        my_generation,
        LoadedImage {
            path: path.clone(),
            image: pristine_arc,
            is_raw,
            decode: Some(variant),
            measures,
        },
    )?;
    // The full decode replaces the proxy; keep (or make) the proxy on disk
    // for the next time, at the size the viewer needs (from an undenoised
    // decode only: the proxy stands for the photograph without AI Denoise).
    *state.proxy_image.lock().unwrap_or_else(|e| e.into_inner()) = None;
    state.source_signal.changed();
    // Proxies stand for the master recipe's decode variant: a virtual copy
    // developed for another one leaves them as they are.
    if is_raw && denoise.is_none() && variant == stored_decode_variant(&source_path_str) {
        let settings = proxy_settings;
        let size = crate::preview_proxy::standard_size(
            state.last_preview_dim.load(Ordering::Relaxed),
            &settings,
        );
        let source = source_path.clone();
        let app = app_handle.clone();
        let thumbnail_path = path.clone();
        std::thread::spawn(move || {
            if crate::preview_proxy::proxy_path(&source).is_none()
                || crate::preview_proxy::stored_size(&source, &settings).is_some_and(|s| s >= size)
            {
                return;
            }
            use tauri::Manager;
            let state = app.state::<AppState>();
            let still_open = || {
                *state
                    .current_source
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    == thumbnail_path
            };
            let Some(proxy_source) = decode_for_proxy(
                &state.rendered_source,
                &proxy_source,
                still_open,
                DECODED_FRAME_WAIT,
            ) else {
                return;
            };
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("preview_proxy_write");
            match crate::preview_proxy::write(
                &source,
                &proxy_source,
                size,
                &settings,
                variant,
                measures,
            ) {
                Ok(proxy) => {
                    app.state::<AppState>()
                        .proxy_memory
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remember(&thumbnail_path, proxy);
                    crate::file_management::refresh_thumbnail(&app, &thumbnail_path);
                    crate::proxy_worker::apply_limits_near(&app, &thumbnail_path);
                }
                Err(e) => log::warn!("Preview proxy for {} not written: {}", source.display(), e),
            }
        });
    }

    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "load_image", "ms": performance_start.elapsed().as_secs_f64() * 1000.0,
            "cached": was_cached, "width": orig_width, "height": orig_height
        })
    );
    // The decode keeps the file's EXIF (also in its cache): the capture
    // time the library corrected since then is shown over it.
    let exif_data = crate::session_bridge::with_capture_override(&source_path, exif_data);
    crate::contribute::note_camera(&exif_data);
    let preview_fallback = is_raw
        .then(|| preview_fallback_reason(&source_path_str))
        .flatten();
    Ok(LoadImageResult {
        width: orig_width,
        height: orig_height,
        metadata,
        exif: exif_data,
        is_raw,
        ai_denoise,
        ai_denoise_version,
        ai_denoise_error,
        offline: None,
        color_noise_multiscale: Some(variant.multiscale),
        dead_pixel_adjacent: Some(variant.dead_test == DeadTest::Adjacent),
        new_edit_frame: is_raw
            .then(|| new_edit_frame(&source_path_str, variant.frame))
            .flatten(),
        developed_from_preview: preview_fallback.is_some(),
        camera_help: preview_fallback
            .as_deref()
            .is_some_and(|reason| !reason.contains(NO_OPEN_DECODER)),
    })
}

/// The error for a photograph whose file is gone and that has no preview to
/// open on (it was never shown before it went offline), instead of a failed
/// file read. None when the file is there.
fn offline_without_preview(source: &Path) -> Option<String> {
    if source.exists() {
        return None;
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string());
    Some(format!(
        "{name} is offline and has no preview yet: connect its drive, or right-click it › Locate Photo…"
    ))
}

/// Opens an offline photograph (its RAW cannot be read) on its preview
/// proxies: its 1:1 proxy becomes the source at full resolution (zoom,
/// masks and tools work as with the decode, from the stored data); else its
/// Standard proxy stays the source and no decode follows. None without
/// either.
async fn open_offline(
    state: &tauri::State<'_, AppState>,
    path: &str,
    source: &Path,
    generation: usize,
    metadata: ImageMetadata,
    stored_exif: Option<HashMap<String, String>>,
    settings: &AppSettings,
) -> Result<Option<LoadImageResult>, String> {
    // The previews stand for the master recipe's decode variant: a virtual
    // copy of another one cannot be shown from them.
    let wanted = stored_decode_variant(path);
    if wanted != stored_decode_variant(&source.to_string_lossy()) {
        return Err(OTHER_VARIANT_OFFLINE.to_string());
    }
    let exif = crate::session_bridge::with_capture_override(
        source,
        crate::exif_processing::read_rrexif_sidecar(source)
            .or(stored_exif)
            .unwrap_or_default(),
    );
    let read_source = source.to_path_buf();
    let read_settings = settings.clone();
    let full = tokio::task::spawn_blocking(move || {
        crate::preview_proxy::read_kind(
            &read_source,
            crate::preview_proxy::ProxyKind::Full,
            &read_settings,
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(full) = full {
        let proxy_frame = crate::preview_proxy::variant_of_key(&full.key.decoder).frame;
        let (width, height) = full.image.dimensions();
        *state.proxy_image.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // Marks the source as preview data (exports never reuse it).
        *state
            .offline_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(path.to_string());
        // Waiters wake when the 1:1 preview is published (or this fails).
        publish_loaded_image(
            &state.original_image,
            &state.source_signal,
            &state.load_image_generation,
            generation,
            LoadedImage {
                path: path.to_string(),
                image: Arc::new(full.image),
                is_raw: true,
                decode: None,
                measures: full.measures,
            },
        )
        .inspect_err(|_| state.source_signal.changed())?;
        return Ok(Some(LoadImageResult {
            width,
            height,
            metadata,
            exif,
            is_raw: true,
            ai_denoise: 0.0,
            ai_denoise_version: 1,
            ai_denoise_error: None,
            offline: Some("full"),
            color_noise_multiscale: None,
            dead_pixel_adjacent: None,
            new_edit_frame: new_edit_frame(&source.to_string_lossy(), proxy_frame),
            developed_from_preview: false,
            camera_help: false,
        }));
    }
    let remembered = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path, wanted);
    let standard = match remembered {
        Some(proxy) => Some(proxy),
        None => {
            let read_source = source.to_path_buf();
            let read_settings = settings.clone();
            tokio::task::spawn_blocking(move || {
                crate::preview_proxy::read(&read_source, &read_settings)
            })
            .await
            .map_err(|e| e.to_string())?
            .map(|proxy| {
                state
                    .proxy_memory
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remember(path, proxy)
            })
        }
    };
    let Some(standard) = standard else {
        return Ok(None);
    };
    let proxy_frame = standard.made_for.map_or(0, |made_for| made_for.frame);
    if state.load_image_generation.load(Ordering::SeqCst) != generation {
        return Err("Load cancelled".to_string());
    }
    let (width, height) = (standard.full_width, standard.full_height);
    *state.proxy_image.lock().unwrap_or_else(|e| e.into_inner()) = Some(standard);
    *state
        .offline_source
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(path.to_string());
    state.source_signal.changed();
    Ok(Some(LoadImageResult {
        width,
        height,
        metadata,
        exif,
        is_raw: true,
        ai_denoise: 0.0,
        ai_denoise_version: 1,
        ai_denoise_error: None,
        offline: Some("standard"),
        color_noise_multiscale: None,
        dead_pixel_adjacent: None,
        new_edit_frame: new_edit_frame(&source.to_string_lossy(), proxy_frame),
        developed_from_preview: false,
        camera_help: false,
    }))
}

#[cfg(test)]
mod activation_tests {
    use super::*;

    #[test]
    fn a_wait_during_the_offline_preview_activation_keeps_waiting() {
        let loaded = LoadedImage {
            path: "a.raf".into(),
            image: Arc::new(DynamicImage::new_rgb8(2, 1)),
            is_raw: true,
            decode: None,
            measures: None,
        };
        // Marked offline, its 1:1 preview not published yet.
        assert!(settled(None, true, false).is_none());
        assert!(matches!(settled(Some(loaded), true, false), Some(Ok(_))));
        assert_eq!(
            settled(None, true, true).unwrap().err().as_deref(),
            Some(OFFLINE_NEEDS_ORIGINAL)
        );
        // Decoding behind its proxy; nothing open.
        assert!(settled(None, false, true).is_none());
        assert_eq!(
            settled(None, false, false).unwrap().err().as_deref(),
            Some(NO_PHOTOGRAPH)
        );
    }

    #[test]
    fn publishing_a_source_recovers_a_poisoned_slot() {
        let original = std::sync::Mutex::new(None);
        let _ = std::panic::catch_unwind(|| {
            let _guard = original.lock().unwrap();
            panic!("poison source slot");
        });
        let loaded = LoadedImage {
            path: "a.raf".into(),
            image: Arc::new(DynamicImage::new_rgb8(2, 1)),
            is_raw: true,
            decode: None,
            measures: None,
        };
        publish_loaded_image(
            &original,
            &SourceSignal::default(),
            &AtomicUsize::new(1),
            1,
            loaded,
        )
        .unwrap();
        assert_eq!(
            original
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .unwrap()
                .path,
            "a.raf"
        );
    }

    #[test]
    fn obsolete_load_cannot_replace_a_newer_activated_source() {
        let original = std::sync::Mutex::new(None);
        let signal = SourceSignal::default();
        let generation = AtomicUsize::new(1);
        let loaded = |path: &str| LoadedImage {
            path: path.to_owned(),
            image: Arc::new(DynamicImage::new_rgb8(2, 1)),
            is_raw: true,
            decode: None,
            measures: None,
        };
        publish_loaded_image(&original, &signal, &generation, 1, loaded("first.raf")).unwrap();
        generation.store(2, Ordering::SeqCst);
        publish_loaded_image(&original, &signal, &generation, 2, loaded("cached.raf")).unwrap();
        let active_pixels = original.lock().unwrap().as_ref().unwrap().image.clone();
        assert!(
            publish_loaded_image(&original, &signal, &generation, 1, loaded("first.raf")).is_err()
        );
        let active = original.lock().unwrap();
        let active = active.as_ref().unwrap();
        assert_eq!(active.path, "cached.raf");
        assert!(Arc::ptr_eq(&active.image, &active_pixels));
    }

    #[test]
    fn bases_of_different_decode_variants_never_share_a_cache_entry() {
        let ms = |multiscale, dead_test| DecodeVariant {
            multiscale,
            dead_test,
            frame: 0,
        };
        let plain = decoded_cache_key("/p/A.NEF", None, DecodeVariant::default());
        let multiscale = decoded_cache_key("/p/A.NEF", None, ms(true, DeadTest::SameColour));
        let adjacent = decoded_cache_key("/p/A.NEF", None, ms(true, DeadTest::Adjacent));
        // The default variant's base keeps the key it always had, the
        // multi-scale one too.
        assert_eq!(plain, "/p/A.NEF");
        assert_eq!(multiscale, "/p/A.NEF\u{1}cnr-ms");
        assert_ne!(adjacent, multiscale);
        assert_ne!(
            decoded_cache_key("/p/A.NEF", None, ms(false, DeadTest::Adjacent)),
            plain
        );
        assert!(adjacent.starts_with("/p/A.NEF\u{1}"));
        assert_ne!(
            decoded_cache_key("/p/B.NEF", None, ms(true, DeadTest::SameColour)),
            multiscale
        );
        // Another frame of the RAW is another base.
        let framed = decoded_cache_key(
            "/p/A.CR3",
            None,
            DecodeVariant {
                frame: 1,
                ..ms(true, DeadTest::Adjacent)
            },
        );
        assert_ne!(
            framed,
            decoded_cache_key("/p/A.CR3", None, ms(true, DeadTest::Adjacent))
        );
    }

    #[test]
    fn the_stored_recipe_decides_the_decode_time_chroma_clean_up() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("A.NEF");
        std::fs::write(&raw, b"raw").unwrap();
        let raw_str = raw.to_string_lossy().to_string();
        // Never edited: the multi-scale filter, no clean-up, and the
        // second dead test (a new edit's render version).
        assert_eq!(
            stored_decode_variant(&raw_str),
            DecodeVariant {
                multiscale: true,
                dead_test: DeadTest::Adjacent,
                frame: 0,
            }
        );
        let settings = AppSettings::default();
        let today = BaseImageOptions::from_preferences(
            settings.raw_highlight_compression,
            settings.linear_raw_mode.clone(),
            settings.raw_preprocessing_color_nr,
            settings.raw_preprocessing_sharpening,
            settings.apply_preprocessing_to_non_raws,
        );
        assert!(today.color_nr_amount > 0.0);
        let variant = |multiscale| DecodeVariant {
            multiscale,
            dead_test: DeadTest::SameColour,
            frame: 0,
        };
        assert_eq!(base_options(&settings, variant(true)).color_nr_amount, 0.0);
        assert_eq!(
            base_options(&settings, variant(true)).sharpening_amount,
            today.sharpening_amount
        );
        assert_eq!(
            base_options(&settings, variant(false)).color_nr_amount,
            today.color_nr_amount
        );
        let write = |path: &std::path::Path, adjustments: serde_json::Value| {
            let metadata =
                serde_json::json!({ "version": 1, "rating": 0, "adjustments": adjustments });
            std::fs::write(path, metadata.to_string()).unwrap();
        };
        // A legacy edit (old filter) keeps today's base; its v2 virtual
        // copy decides by its own recipe.
        write(
            &dir.path().join("A.NEF.rrdata"),
            serde_json::json!({ "colorNoiseReduction": 25 }),
        );
        write(
            &dir.path().join("A.NEF.c1.rrdata"),
            serde_json::json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 2 }),
        );
        assert!(!stored_decode_variant(&raw_str).multiscale);
        assert!(stored_decode_variant(&format!("{raw_str}?vc=c1")).multiscale);
        // Saved edits keep the first dead test until their render version
        // is 3.
        assert_eq!(
            stored_decode_variant(&raw_str).dead_test,
            DeadTest::SameColour
        );
        write(
            &dir.path().join("A.NEF.c2.rrdata"),
            serde_json::json!({ "colorNoiseReduction": 25, "renderVersion": 3 }),
        );
        assert_eq!(
            stored_decode_variant(&format!("{raw_str}?vc=c2")).dead_test,
            DeadTest::Adjacent
        );
    }

    #[test]
    fn an_offline_photograph_without_preview_says_so_instead_of_a_failed_read() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("A.NEF");
        std::fs::write(&present, b"raw").unwrap();
        assert_eq!(offline_without_preview(&present), None);
        let message = offline_without_preview(&dir.path().join("Gone/B.NEF")).unwrap();
        assert!(message.starts_with("B.NEF is offline"), "{message}");
        assert!(message.contains("Locate Photo"), "{message}");
    }
}

#[cfg(test)]
mod decode_for_proxy_tests {
    use super::decode_for_proxy;
    use crate::app_state::RenderedSource;
    use image::DynamicImage;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_proxy_waits_for_its_frame_and_never_keeps_a_left_decode() {
        let rendered = RenderedSource::default();
        let wait = Duration::from_millis(20);
        let decode = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let weak = Arc::downgrade(&decode);
        // Left before a frame of it: nothing is written.
        assert!(decode_for_proxy(&rendered, &weak, || false, wait).is_none());
        // Still open (Develop rendered nothing): written after the wait.
        assert!(decode_for_proxy(&rendered, &weak, || true, wait).is_some());
        // Rendered: written, also when the photograph was left since.
        rendered.mark(&decode);
        assert!(decode_for_proxy(&rendered, &weak, || false, Duration::ZERO).is_some());
        // Evicted from memory meanwhile: the waiting writer held no copy.
        drop(decode);
        assert!(decode_for_proxy(&rendered, &weak, || true, Duration::ZERO).is_none());
    }
}

#[cfg(test)]
mod patch_cache_tests {
    use super::*;

    /// The editor sends each AI mask bitmap once per photograph it opens
    /// (`patchesSentToBackend`); a stored proxy's frame carries them before
    /// `load_image` decodes the same photograph.
    #[test]
    fn bitmaps_sent_with_the_proxy_frame_outlive_the_decode() {
        let state = AppState::default();
        let sent = |state: &AppState| state.patch_cache.lock().unwrap().contains_key("subject");
        // load_image_proxy, then the proxy frame carrying the bitmap.
        open_source(&state, "a.ARW");
        state.patch_cache.lock().unwrap().insert(
            "subject".into(),
            serde_json::json!("data:image/png;base64,AA"),
        );
        // What load_image (and apply_ai_denoise) do on arrival.
        open_source(&state, "a.ARW");
        clear_source_caches(&state);
        assert!(sent(&state));
        // Another photograph: its masks may share ids (pasted layers).
        open_source(&state, "b.ARW");
        assert!(!sent(&state));
        assert_eq!(*state.current_source.lock().unwrap(), "b.ARW");
    }
}
