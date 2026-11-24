//! Library thumbnails: cache, generation workers and queue.
//! Part of `file_management`, split by concern (ADR 0009).
use super::enqueue_metadata;
use super::is_cloud_placeholder;
use super::parse_virtual_path;
use super::read_file_mapped;
use crate::AppSettings;
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
use crate::mask_generation::ParametricMaskSource;
use crate::mask_generation::prepare_parametric_mask_source;
use crate::mask_generation::{MaskBitmap, MaskDefinition};
use crate::state::develop::DevelopSession;
use crate::state::lock::lock_recover;
use crate::state::thumbnails::{ThumbnailJob, ThumbnailQueue, ThumbnailState, ThumbnailWork};
use anyhow::Result;
use image::DynamicImage;
use image::GenericImageView;
use image::codecs::jpeg::JpegEncoder;
use rawtonic_recipe::effects;
use rawtonic_recipe::geometry;
use rawtonic_recipe::lens;
use rawtonic_recipe::masks;
use rawtonic_recipe::tone;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex};
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
/// Lens parameters saved while the photo could not be read are resolved here
/// once it can be, and Lightroom items imported before their photo's
/// orientation was recorded get it, so the render and its persistent cache
/// key both change.
fn thumbnail_metadata(source: &Path, sidecar: &Path) -> Option<ImageMetadata> {
    let mut metadata: ImageMetadata =
        if let Some(content) = rawtonic_export::sidecars::read_sidecar_text(sidecar) {
            serde_json::from_str(&content).ok()?
        } else {
            if source.exists() {
                return None;
            }
            crate::session_bridge::read_editor_sidecar(sidecar)
                .and_then(|state| serde_json::from_value(state).ok())?
        };
    let _ = rawtonic_export::exif_processing::resolve_unresolved_lens(
        &mut metadata.adjustments,
        source,
    );
    rawtonic_export::exif_processing::record_lightroom_orientation(
        &mut metadata.adjustments,
        source,
    );
    // A camera TIFF's edit made on its embedded thumbnail, on the full
    // frame as the editor and exports show it (`recipe_frame`); the moved
    // recipe is also what the cache hash reads, so its old tile is replaced.
    rawtonic_engine::recipe_frame::conform(&mut metadata.adjustments, source);
    Some(metadata)
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
        [tone::CONTRAST, tone::SHADOWS, tone::WHITES, tone::BLACKS]
            .iter()
            .any(|key| {
                values
                    .get(key.name())
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|v| v != 0.0)
            })
    };
    sets(adjustments)
        || adjustments
            .get(masks::MASKS.name())
            .and_then(serde_json::Value::as_array)
            .is_some_and(|masks| {
                masks
                    .iter()
                    .any(|mask| mask.get("adjustments").is_some_and(&sets))
            })
}

/// How the thumbnail cache names the photograph `path_str` (its source file
/// `source`): inside a session or catalog folder by that library's id and
/// its path in the folder, so a whole-session move, its undo and redo (and
/// another spelling of the same folder) find the thumbnails made at the old
/// place instead of developing every photograph again (SMV1, notes:
/// parity-gaps/G40-file-operations.md); elsewhere by its absolute path.
/// Two copies of one session folder share an id: their photographs at one
/// path are the same file then, and a changed one has another time stamp.
fn cache_label(path_str: &str, source: &Path) -> String {
    #[cfg(test)]
    LABEL_READS.with(|reads| reads.set(reads.get() + 1));
    #[derive(serde::Deserialize)]
    struct Id {
        id: String,
    }
    rawtonic_catalog::layout::LibraryLayout::holding(source)
        .and_then(|library| {
            let id = serde_json::from_slice::<Id>(&fs::read(library.manifest()).ok()?)
                .ok()?
                .id;
            let relative = Path::new(path_str).strip_prefix(library.directory()).ok()?;
            Some(format!("library\0{id}\0{}", relative.to_string_lossy()))
        })
        .unwrap_or_else(|| path_str.to_string())
}

#[cfg(test)]
thread_local! {
    /// How often this thread asked `cache_label` (it stats a library's
    /// manifests beside the photo).
    static LABEL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A photo's cache label and source time (`label_and_mtime`).
type LookupFacts = (String, Option<u64>);

thread_local! {
    /// Inside `with_lookup_facts`: each photo's cache label and source
    /// time, asked once for all the keys of one lookup.
    static LOOKUP_FACTS: std::cell::RefCell<Option<HashMap<String, LookupFacts>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with the cache label and source time of each photo read once:
/// a lookup computes up to seven keys of one photo, and each asked again
/// stats the original and looks for a library's manifests beside it (GRD2,
/// a never-edited RAW on a share: 8 stats of the original and 71 of
/// manifests per lookup, 2 and 46 with this; each a round trip when the
/// share's attribute cache is cold).
fn with_lookup_facts<R>(f: impl FnOnce() -> R) -> R {
    struct Scope(bool);
    impl Drop for Scope {
        fn drop(&mut self) {
            if self.0 {
                LOOKUP_FACTS.with(|facts| facts.borrow_mut().take());
            }
        }
    }
    // The outermost scope owns the facts and drops them at its end.
    let _scope = Scope(LOOKUP_FACTS.with(|facts| {
        let mut facts = facts.borrow_mut();
        let outermost = facts.is_none();
        if outermost {
            *facts = Some(HashMap::new());
        }
        outermost
    }));
    f()
}

/// `cache_label` and `source_mtime` of `path_str`, once per
/// `with_lookup_facts` scope.
fn label_and_mtime(path_str: &str, source: &Path) -> LookupFacts {
    let known = LOOKUP_FACTS.with(|facts| {
        facts
            .borrow()
            .as_ref()
            .and_then(|facts| facts.get(path_str).cloned())
    });
    if let Some(known) = known {
        return known;
    }
    let facts = (cache_label(path_str, source), source_mtime(source));
    LOOKUP_FACTS.with(|kept| {
        if let Some(kept) = kept.borrow_mut().as_mut() {
            kept.insert(path_str.to_string(), facts.clone());
        }
    });
    facts
}

/// Whether a tile of `source` is developed from a Standard proxy that still
/// holds the clamp (`preview_proxy::DISPLAY_COPY`): until the proxy is
/// renewed its tile keeps its old key. Online, a copy that develops another
/// decode variant or effective frame can renew its tile from the RAW.
fn display_copy_clamps(source: &Path, recipe: &serde_json::Value) -> bool {
    crate::preview_proxy::standard_display_copy_is_stale(source, recipe)
}

pub(crate) fn compute_thumbnail_cache_hash(
    path_str: &str,
    adjustments_bytes: &[u8],
) -> Option<String> {
    thumbnail_hash_for_engine(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
    )
}

fn thumbnail_hash_for_engine(
    path_str: &str,
    adjustments_bytes: &[u8],
    engine: u32,
) -> Option<String> {
    thumbnail_hash_with(path_str, adjustments_bytes, engine, true, None, true, true)
}

/// The key a tile was cached under before a photo's camera matched its
/// Develop defaults' entries by the camera's one name (2026-11-23): while
/// the original is unavailable, that tile is better than none.
fn thumbnail_hash_before_camera_identity(
    path_str: &str,
    adjustments_bytes: &[u8],
) -> Option<String> {
    thumbnail_hash_with(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
        true,
        None,
        false,
        false,
    )
}

/// The key a tile was cached under while its copy's preview proxy still held
/// the clamp but the other refreshes were done (a retouched copy of another
/// decode variant, before it was made again at once).
fn thumbnail_hash_with_display_copy_held(
    path_str: &str,
    adjustments_bytes: &[u8],
) -> Option<String> {
    thumbnail_hash_with(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
        true,
        Some(true),
        false,
        false,
    )
}

/// The key of a tile whose display copy was renewed (what the lookup asks
/// for once the master's proxy is renewed or the copy's variant is known to
/// differ), whatever the proxy says now: while offline and the master's
/// sidecar is evicted the lookup cannot tell, and the tile may be there.
fn thumbnail_hash_with_display_copy_renewed(
    path_str: &str,
    adjustments_bytes: &[u8],
) -> Option<String> {
    thumbnail_hash_with(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
        true,
        Some(false),
        false,
        false,
    )
}

/// The key a thumbnail was cached under before face retouch tiles and RAW
/// tiles reduced with a clamp were made again (2026-11-21): while the
/// original is unavailable, a tile cached then is better than none
/// (`ThumbnailCacheEntry::of`).
fn thumbnail_hash_before_refreshes(path_str: &str, adjustments_bytes: &[u8]) -> Option<String> {
    thumbnail_hash_with(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
        false,
        None,
        false,
        false,
    )
}

/// The key a never-edited ORF's tile was developed under before its maker
/// note's preview was read (2026-11-24): while the original is unavailable,
/// that tile is better than none.
fn thumbnail_hash_before_orf_preview(path_str: &str, adjustments_bytes: &[u8]) -> Option<String> {
    thumbnail_hash_with(
        path_str,
        adjustments_bytes,
        crate::preview_proxy::ENGINE_VERSION,
        true,
        None,
        true,
        false,
    )
}

fn thumbnail_hash_with(
    path_str: &str,
    adjustments_bytes: &[u8],
    engine: u32,
    refreshed: bool,
    display_copy: Option<bool>,
    camera_identity: bool,
    orf_preview: bool,
) -> Option<String> {
    let (source_path, _) = parse_virtual_path(path_str);

    let (label, img_mod_time) = label_and_mtime(path_str, &source_path);
    let img_mod_time = img_mod_time?;

    let mut hasher = blake3::Hasher::new();
    hasher.update(label.as_bytes());
    hasher.update(&img_mod_time.to_le_bytes());
    hasher.update(adjustments_bytes);
    let adjustments = serde_json::from_slice::<serde_json::Value>(adjustments_bytes).ok();
    if adjustments
        .as_ref()
        .is_some_and(|adjustments| !adjustments["relightDepth"].is_null())
    {
        hasher.update(b"\0relight-face-reflection-v7");
    }
    // A photograph without an edit renders with the user's develop defaults
    // (`rawtonic_engine::develop_defaults`): new ones make new thumbnails.
    if let Some(defaults) = rawtonic_engine::develop_defaults::fingerprint()
        && rawtonic_engine::develop_defaults::follows_defaults(
            adjustments.as_ref().unwrap_or(&serde_json::Value::Null),
        )
    {
        hasher.update(b"\0develop-defaults\0");
        hasher.update(&defaults);
        // The same defaults may give a photo another camera entry since its
        // camera is matched by its one name for all its files.
        if camera_identity {
            hasher.update(b"\0camera-identity-v1");
        }
    }
    // Absent light fields now render with a new light's values.
    if adjustments
        .as_ref()
        .and_then(|adjustments| adjustments["relightLights"].as_array())
        .is_some_and(|lights| !lights.is_empty())
    {
        hasher.update(b"\0relight-defaults-v1");
    }
    // A recipe whose LUT file did not load rendered black until 2026-11-22:
    // tiles of recipes with a LUT are made again once. Tiles made while the
    // LUT does not load are kept under a key of their own
    // (`ThumbnailCacheEntry::save`), so no file is probed here.
    if refreshed
        && adjustments
            .as_ref()
            .is_some_and(|adjustments| adjustments[effects::LUT_PATH.into_name()].is_string())
    {
        hasher.update(b"\0lut-renders-when-missing-v1\0");
    }
    if is_raw_file(&source_path) {
        // A never-edited Olympus / OM System RAW's tile was developed from
        // the sensor data until 2026-11-24; it now shows the camera's preview
        // from the maker note (`image_loader::orf_embedded_jpeg`) like other
        // cameras' RAWs: such tiles are made again once.
        if orf_preview
            && adjustments.as_ref().is_none_or(serde_json::Value::is_null)
            && source_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("orf"))
        {
            hasher.update(b"\0orf-maker-note-preview-v1");
        }
        // Old embedded RAW previews could omit camera orientation for non-TIFF
        // containers (notably RAF). Their pixels must not survive this fix.
        hasher.update(b"\0embedded-raw-orientation-v1");
        // Developed RAW pixels change with the decode engine (e.g. highlight
        // reconstruction), so thumbnails follow its version.
        hasher.update(format!("\0raw-engine-{engine}").as_bytes());
        // With a preview proxy the thumbnail is developed from it (not the
        // camera JPEG), so a new proxy makes a new thumbnail.
        if crate::preview_proxy::proxy_path(&source_path).is_some_and(|p| p.is_file()) {
            hasher.update(b"\0developed-from-proxy-v1");
        }
        // Reductions clamped colours beyond sRGB until 2026-11-21 (the
        // display copy, `preview_proxy::DISPLAY_COPY`): tiles are made again.
        // A tile developed from a preview proxy that still holds the clamp
        // shows it, so it keeps its key until the proxy is renewed.
        if refreshed
            && !display_copy.unwrap_or_else(|| {
                display_copy_clamps(
                    &source_path,
                    adjustments.as_ref().unwrap_or(&serde_json::Value::Null),
                )
            })
        {
            hasher.update(b"\0display-copy-keeps-beyond-srgb-v1\0");
        }
        // A face retouch drawn on a reduced base (a preview proxy or a fast
        // decode) was placed with full-size positions until 2026-11-21.
        if refreshed
            && adjustments
                .as_ref()
                .is_some_and(rawtonic_engine::face_retouch::is_active)
        {
            hasher.update(b"\0face-retouch-on-reduced-base-v1\0");
        }
        // A never-edited RAW renders with the defaults of its time
        // (`embedded_lens::apply_default`: lens profile, render version).
        if adjustments
            .as_ref()
            .is_none_or(|adjustments| adjustments.get(lens::CORRECTION_MODE.name()).is_none())
        {
            hasher.update(
                format!(
                    "\0never-edited-defaults-{}-{}",
                    rawtonic_engine::source_facts::VERSION,
                    rawtonic_engine::image_processing::RENDER_VERSION
                )
                .as_bytes(),
            );
        }
        // A new recipe (none, or a Reset's) renders with the tool responses
        // of the editor's new edit since 2026-11-05 (`normalize`: the
        // luminance noise reduction's speck removal).
        if adjustments
            .as_ref()
            .is_none_or(rawtonic_engine::image_processing::is_new_recipe)
        {
            hasher.update(b"\0new-edit-responses-v1");
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
    } else if serde_json::from_slice::<serde_json::Value>(adjustments_bytes)
        .ok()
        .is_some_and(|adjustments| {
            rawtonic_engine::image_processing::shades_lens_vignetting(
                &rawtonic_engine::image_processing::get_geometry_params_from_json(&adjustments),
            )
        })
    {
        // Lens vignetting on a photograph's encoded values applies in
        // linear light since 2026-10-28 (`GeometryParams::srgb_base`).
        hasher.update(b"\0encoded-vignetting-v1");
    }
    if serde_json::from_slice::<serde_json::Value>(adjustments_bytes)
        .ok()
        .is_some_and(|adjustments| {
            adjustments
                .get(masks::MASKS.name())
                .and_then(serde_json::Value::as_array)
                .is_some_and(|masks| !masks.is_empty())
        })
    {
        // Previously persisted masked previews may contain pixels from another
        // editor source or shifted layer adjustments. Preserve unmasked entries.
        hasher.update(b"\0masked-thumbnail-source-and-alignment-v1");
    }
    // A face retouch found before a later flip, turn, straightening or lens
    // change is placed through its stored frame; it was skipped before
    // (2026-11-21), so thumbnails cached then show it missing.
    if refreshed
        && adjustments
            .as_ref()
            .is_some_and(rawtonic_engine::face_retouch::follows_stored_frame)
    {
        hasher.update(b"\0face-retouch-by-stored-frame\0");
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// The decoded base with the recipe's base retouch, for a source at
/// `scale` (its pixels per full-resolution pixel; below 1 for a preview
/// proxy or a fast decode).
fn retouched_base<'a>(
    pristine_source: &'a DynamicImage,
    adjustments: &serde_json::Value,
    scale: f32,
) -> anyhow::Result<Cow<'a, DynamicImage>> {
    if scale == 1.0 {
        return image_loader::composite_patches_for_render(pristine_source, adjustments);
    }
    // A reduced source leaves Lightroom's spots and red eyes not yet placed
    // out, and places patches, spots and faces (in full-resolution pixels)
    // at its scale, as reduced copies do.
    if image_loader::has_base_retouch(adjustments) {
        image_loader::composite_patches_on_scaled_image(pristine_source, adjustments, scale)
            .map(Cow::Owned)
    } else {
        Ok(Cow::Borrowed(pristine_source))
    }
}

/// A RAW's embedded preview (`safe_embedded_preview_fallback_shared`) and
/// the mapping it was read from, for the steps after it
/// (`embedded_in_camera_crop`) to read the same file through it instead of
/// opening it again.
fn embedded_raw_preview_and_file(source_path: &Path) -> Option<(DynamicImage, MappedRaw)> {
    let (mmap, metadata) =
        rawtonic_engine::file_io::read_file_mapped_with_metadata(source_path).ok()?;
    // The mapped file's own size and time (`camera_crop::crops_in`).
    let stamp = metadata.modified().ok().map(|time| (metadata.len(), time));
    let file: image_loader::SharedBytes = Arc::new(mmap);
    let preview = image_loader::safe_embedded_preview_fallback_shared(
        file.clone(),
        &source_path.to_string_lossy(),
    )?;
    Some((preview, MappedRaw { file, stamp }))
}

/// A RAW's mapping and the `camera_crop::FileStamp` of the file mapped.
struct MappedRaw {
    file: image_loader::SharedBytes,
    stamp: Option<rawtonic_engine::camera_crop::FileStamp>,
}

pub fn generate_thumbnail_data(
    path_str: &str,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
) -> anyhow::Result<DynamicImage> {
    generate_thumbnail_with_source(
        path_str,
        gpu_context,
        preloaded_image,
        app_handle,
        &mut false,
    )
}

fn generate_thumbnail_with_source(
    path_str: &str,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
    embedded: &mut bool,
) -> anyhow::Result<DynamicImage> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);
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
    develop_thumbnail_with_source(
        path_str,
        path_str,
        metadata,
        gpu_context,
        preloaded_image,
        app_handle,
        embedded,
    )
}

/// The stored Standard proxy of the RAW `source` when it stands for a
/// photograph shown with `adjustments` in its own effective frame.
fn stored_proxy(
    source: &Path,
    adjustments: &serde_json::Value,
    settings: &crate::app_settings::AppSettings,
) -> Option<crate::preview_proxy::Proxy> {
    crate::preview_proxy::read_kind(
        source,
        crate::preview_proxy::ProxyKind::Standard,
        settings,
        Some(adjustments),
    )
}

/// A new recipe's plain decode (never edited, or a Reset's) cut to the
/// camera's crop of the frame the recipe asks for
/// (`camera_crop::initial_crop`, the rule of the GPU render), as its export
/// frames it. The crop is centred in the developed frame, so its share of
/// each side carries over to a reduced decode.
fn cut_to_camera_crop(
    image: DynamicImage,
    recipe: &serde_json::Value,
    source: &str,
) -> DynamicImage {
    let Some(crop) = rawtonic_engine::camera_crop::initial_crop(
        recipe,
        &crate::camera_crop::crops(Path::new(source)),
    ) else {
        return image;
    };
    cut_to_camera_frame(image, crop)
}

fn cut_to_camera_frame(
    image: DynamicImage,
    crop: rawtonic_engine::camera_crop::CameraCrop,
) -> DynamicImage {
    let share = |part: u32, margin: u32| part as f64 / (2 * margin + part) as f64;
    let (width, height) = (image.width(), image.height());
    let cut_width = ((width as f64 * share(crop.width, crop.x)).round() as u32).clamp(1, width);
    let cut_height = ((height as f64 * share(crop.height, crop.y)).round() as u32).clamp(1, height);
    image.crop_imm(
        (width - cut_width) / 2,
        (height - cut_height) / 2,
        cut_width,
        cut_height,
    )
}

/// A never-edited RAW's embedded JPEG framed as its export frames it: cut
/// to the camera's crop where the JPEG shows the whole frame
/// (`camera_crop::shows_crop`: a Fujifilm RAF's 3:2 JPEG of a 16:9 or 5:4
/// photo); Canon's and the EX2F's JPEGs are their crop already. The crops
/// are read through `file`, the mapping the JPEG came from, when given.
fn embedded_in_camera_crop(
    image: DynamicImage,
    source: &str,
    file: Option<&MappedRaw>,
) -> DynamicImage {
    let never_edited = serde_json::Value::Null;
    let crops = match file {
        Some(raw) => rawtonic_engine::camera_crop::crops_in(source, &raw.file, raw.stamp),
        None => crate::camera_crop::crops(Path::new(source)),
    };
    match rawtonic_engine::camera_crop::initial_crop(&never_edited, &crops) {
        Some(crop)
            if !rawtonic_engine::camera_crop::shows_crop(crop, image.width(), image.height()) =>
        {
            cut_to_camera_frame(image, crop)
        }
        _ => image,
    }
}

/// The thumbnail of `path_str` developed with `metadata`'s adjustments
/// (none: as a photograph never edited), as the Library shows it. Its
/// decoded base is cached under `cache_key` (a photograph's own thumbnails
/// use its path). A RAW is decoded for the decode variant of those
/// adjustments (a snapshot's or a virtual copy's may differ from the
/// photograph's), and its preview proxy only used when made for the same.
pub(crate) fn develop_thumbnail(
    path_str: &str,
    cache_key: &str,
    metadata: Option<ImageMetadata>,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
) -> anyhow::Result<DynamicImage> {
    develop_thumbnail_with_source(
        path_str,
        cache_key,
        metadata,
        gpu_context,
        preloaded_image,
        app_handle,
        &mut false,
    )
}

#[allow(clippy::too_many_arguments)]
fn develop_thumbnail_with_source(
    path_str: &str,
    cache_key: &str,
    metadata: Option<ImageMetadata>,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
    embedded: &mut bool,
) -> anyhow::Result<DynamicImage> {
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let (source_path, _) = parse_virtual_path(path_str);
    let source_path_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path_str);

    let adjustments = metadata
        .as_ref()
        .map_or(serde_json::Value::Null, |m| m.adjustments.clone());
    // Where the render below cannot crop (no GPU): the cut to the camera's
    // crop of a new recipe's frame.
    let new_recipe = (is_raw && crate::image_processing::is_new_recipe(&adjustments))
        .then(|| adjustments.clone());

    let settings = crate::state::settings::current(app_handle);
    let always_decode_raw = settings.always_decode_raw_thumbnails.unwrap_or(false);

    // A preview proxy (preview_proxy.rs) develops the thumbnail like the
    // editor does, also for never-edited RAWs (RAW defaults and the camera's
    // lens data), so the Library and the placeholder show the edited look.
    let proxy = (is_raw && preloaded_image.is_none() && gpu_context.is_some())
        .then(|| stored_proxy(&source_path, &adjustments, &settings))
        .flatten();
    let (metadata, adjustments) = match (&proxy, metadata) {
        // A new recipe renders as its export does: none (with a proxy to
        // develop or develop defaults to apply; without either the camera's
        // JPEG or a plain decode below), or a Reset's.
        (proxy, metadata)
            if crate::image_processing::is_new_recipe(&adjustments)
                && ((is_raw && (proxy.is_some() || !adjustments.is_null()))
                    // With the user's develop defaults, not the camera's JPEG
                    // (a JPEG or TIFF too when they are not for RAWs only).
                    || (rawtonic_engine::develop_defaults::follows_defaults(&adjustments)
                        && rawtonic_engine::develop_defaults::for_path(&source_path).is_some())) =>
        {
            let mut recipe = if adjustments.is_null() {
                serde_json::json!({})
            } else {
                adjustments.clone()
            };
            let _ = rawtonic_export::normalize::normalize(
                &mut recipe,
                &rawtonic_export::normalize::RecipeSource::file(&source_path),
                rawtonic_export::normalize::Bitmaps::None,
                rawtonic_export::normalize::Defaults::NewEdit(crate::camera_crop::apply_default),
            );
            let mut meta = metadata.unwrap_or_default();
            meta.adjustments = recipe.clone();
            (Some(meta), recipe)
        }
        (_, metadata) => (metadata, adjustments),
    };
    // The open photograph's render input (`color_noise_view`: the export's
    // warp, edge-fringe pass and colour noise reduction at full resolution,
    // reduced): its thumbnail renders from it at Color 0 in place of the
    // warped preview proxy, as the Develop fit view. Not with AI Denoise:
    // the input was made from the denoised decode, the proxy stands for the
    // photograph without it.
    let color_noise = (is_raw
        && gpu_context.is_some()
        && !adjustments.is_null()
        && image_loader::ai_denoise_settings(&adjustments).is_none())
    .then(|| {
        let tm = crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
        crate::color_noise_view::for_thumbnail(
            &app_handle.state::<DevelopSession>().color_noise_view,
            path_str,
            &get_all_adjustments_from_json(&adjustments, is_raw, tm),
            &adjustments,
            is_raw,
        )
    })
    .flatten();
    let (proxy, color_noise) = match (proxy, color_noise) {
        (None, Some(input)) => match stored_proxy(&source_path, &adjustments, &settings) {
            Some(proxy) => (Some(proxy), Some(input)),
            None => (None, None),
        },
        (proxy, input) => (proxy, input),
    };
    let preloaded_image = preloaded_image.filter(|_| color_noise.is_none());
    // Read from the proxy that supplies the pixels, not from the file again
    // (a build may replace it meanwhile).
    let proxy_clamps = proxy.as_ref().is_some_and(|proxy| proxy.clamped);
    if is_raw
        && adjustments.is_null()
        && preloaded_image.is_none()
        && !always_decode_raw
        && let Some((preview, file)) = embedded_raw_preview_and_file(&source_path)
    {
        *embedded = true;
        return Ok(embedded_in_camera_crop(
            preview,
            &source_path_str,
            Some(&file),
        ));
    }

    if let (Some(context), Some(meta)) = (gpu_context, metadata)
        && !meta.adjustments.is_null()
    {
        let luts = app_handle.state::<crate::state::luts::LutState>();
        let develop = app_handle.state::<DevelopSession>();
        let thumbnails = app_handle.state::<ThumbnailState>();
        let target_res = settings.medium_thumbnail_resolution.unwrap_or(1280);

        let base_cache_hash = crate::cache_utils::calculate_thumbnail_base_hash(&meta.adjustments);
        // The base a proxy that clamps made is not the one its renewal makes.
        let base_cache_hash = if proxy_clamps {
            let mut hasher = DefaultHasher::new();
            base_cache_hash.hash(&mut hasher);
            "display-copy-clamps".hash(&mut hasher);
            hasher.finish()
        } else {
            base_cache_hash
        };
        let base_cache_hash = match &color_noise {
            Some(input) => {
                let mut hasher = DefaultHasher::new();
                base_cache_hash.hash(&mut hasher);
                (Arc::as_ptr(input) as usize).hash(&mut hasher);
                hasher.finish()
            }
            None => base_cache_hash,
        };

        let crop_data: Option<Crop> =
            serde_json::from_value(meta.adjustments[geometry::CROP.name()].clone()).ok();

        let mask_definitions: Vec<MaskDefinition> = meta
            .adjustments
            .get(masks::MASKS.name())
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();
        let needs_mask_source = mask_definitions
            .iter()
            .any(MaskDefinition::requires_warped_image);

        #[allow(clippy::type_complexity)]
        let cached_base: Option<(
            Arc<DynamicImage>,
            f32,
            Option<ParametricMaskSource>,
            Option<(u32, u32)>,
        )> = {
            let cache = lock_recover(&thumbnails.geometry_cache);
            if let Some((cached_hash, img, scale, mask_source, decoded_size)) = cache.get(cache_key)
            {
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
                    Some((Arc::clone(img), *scale, mask_source.clone(), *decoded_size))
                } else {
                    None
                }
            } else {
                None
            }
        };

        let (processing_base_arc, total_scale, mask_source, decoded_size) = if let Some(cached) =
            cached_base
        {
            cached
        } else {
            let mut raw_scale_factor = 1.0f32;

            let pristine_source = if let Some(img) = preloaded_image {
                Cow::Borrowed(img)
            } else if let Some(proxy) = &proxy {
                raw_scale_factor = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
                Cow::Borrowed(&proxy.image)
            } else {
                let vec_guard;
                let mapped = rawtonic_engine::file_io::read_file_mapped_with_metadata(&source_path)
                    .map(|(mmap, metadata)| {
                        let bytes: image_loader::SharedBytes = Arc::new(mmap);
                        (bytes, rawtonic_fs::FileStamp::from_metadata(&metadata))
                    });

                let file_slice: &[u8] = match &mapped {
                    Ok((mmap, _)) => (**mmap).as_ref(),
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

                let decode = || {
                    image_loader::load_base_image_for_recipe(
                        file_slice,
                        &source_path_str,
                        true,
                        &settings,
                        None,
                        None,
                        &adjustments,
                    )
                };
                // The base look applied below reads the decode's mapping.
                let img = match &mapped {
                    Ok((mmap, stamp)) if is_raw => crate::baseline_exposure::decode_with_base_look(
                        &source_path_str,
                        mmap.clone(),
                        *stamp,
                        decode,
                    ),
                    _ => decode(),
                }?;

                if is_raw {
                    raw_scale_factor = crate::raw_processing::get_fast_demosaic_scale_factor(
                        file_slice,
                        img.width(),
                        img.height(),
                    );
                }
                Cow::Owned(img)
            };

            let composite_image = retouched_base(&pristine_source, &adjustments, raw_scale_factor)?;
            // A full decode's size places Lightroom masks still in their
            // imported form, as for exports (`lightroom_masks`).
            let decoded_size = (raw_scale_factor == 1.0).then(|| pristine_source.dimensions());

            // The scale of the warped image to the photograph.
            let mut warped_scale_factor = raw_scale_factor;
            let warped_image = match (&color_noise, &proxy) {
                (Some(input), Some(proxy)) => {
                    warped_scale_factor = input.width() as f32 / proxy.full_width.max(1) as f32;
                    Cow::Owned(DynamicImage::ImageRgb32F((**input).clone()))
                }
                _ => apply_geometry_warp(composite_image, &meta.adjustments, is_raw)
                    .map_err(anyhow::Error::msg)?,
            };

            let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, &meta.adjustments);

            let orientation_steps = meta.adjustments[geometry::ORIENTATION_STEPS.name()]
                .as_u64()
                .unwrap_or(0) as u8;
            let coarse_rotated_image = apply_coarse_rotation(blurred_image, orientation_steps);

            let (full_w, full_h) = coarse_rotated_image.dimensions();

            let mut processing_dim = target_res;
            if let Some(c) = &crop_data
                && c.width > 0.0
                && c.height > 0.0
            {
                let crop_max_dim_loaded = c.width.max(c.height) * warped_scale_factor as f64;
                let full_max_dim = full_w.max(full_h) as f64;
                if crop_max_dim_loaded > 0.0 {
                    processing_dim = ((target_res as f64 * full_max_dim / crop_max_dim_loaded)
                        .round() as u32)
                        .min(full_w.max(full_h));
                }
            }

            let (base, gpu_scale) = if full_w > processing_dim || full_h > processing_dim {
                let base = crate::image_processing::resample_f32_image(
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

            let total_scale = gpu_scale * warped_scale_factor;

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
                    let reduced = crate::image_processing::downscale_f32_image_signed(
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
            let mut cache = lock_recover(&thumbnails.geometry_cache);
            if cache.len() >= 8 {
                let key_to_remove = cache.keys().next().cloned();
                if let Some(key) = key_to_remove {
                    cache.remove(&key);
                }
            }

            let base_arc = Arc::new(base);
            cache.insert(
                cache_key.to_string(),
                (
                    base_cache_hash,
                    Arc::clone(&base_arc),
                    total_scale,
                    mask_source.clone(),
                    decoded_size,
                ),
            );

            (base_arc, total_scale, mask_source, decoded_size)
        };

        let rotation_degrees = meta.adjustments[geometry::ROTATION.name()]
            .as_f64()
            .unwrap_or(0.0) as f32;
        let flip_horizontal = meta.adjustments[geometry::FLIP_HORIZONTAL.name()]
            .as_bool()
            .unwrap_or(false);
        let flip_vertical = meta.adjustments[geometry::FLIP_VERTICAL.name()]
            .as_bool()
            .unwrap_or(false);

        let flipped_image = apply_flip(
            Cow::Borrowed(&*processing_base_arc),
            flip_horizontal,
            flip_vertical,
        );
        let rotated_image = apply_rotation(
            flipped_image,
            rotation_degrees,
            rawtonic_engine::recipe_versions::RenderVersion::of(&meta.adjustments).keeps_negative(),
        );

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
        let (base_w, base_h) = if meta.adjustments[geometry::ORIENTATION_STEPS.name()]
            .as_u64()
            .unwrap_or(0)
            % 2
            == 1
        {
            (base_h, base_w)
        } else {
            (base_w, base_h)
        };
        let mask_geometry =
            crate::mask_generation::PhotoGeometry::from_adjustments(&meta.adjustments);
        let mask_geometry = match decoded_size {
            Some((width, height)) => mask_geometry.with_decoded_size(width, height),
            None => mask_geometry.with_photo_size(
                base_w as f64 / total_scale as f64,
                base_h as f64 / total_scale as f64,
            ),
        };
        let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
            .iter()
            .filter_map(|def| {
                crate::mask_generation::get_cached_or_generate_mask(
                    &develop,
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
        // A preview: the measures of the decode it was made from; a fast
        // development: the last full decode's.
        crate::baseline_exposure::apply_base_look(
            &mut gpu_adjustments.global,
            &source_path_str,
            proxy.as_ref().and_then(|proxy| proxy.measures).as_ref(),
            crate::photo_library::library_of_str(&source_path_str).as_deref(),
        );
        gpu_adjustments.global.detail_scale = total_scale;
        if color_noise.is_some() {
            gpu_adjustments.global.color_noise_reduction = 0.0;
        }
        let lut_path = meta.adjustments[effects::LUT_PATH.into_name()].as_str();
        let lut = lut_path.and_then(|p| crate::lut_processing::get_or_load_lut(&luts, p).ok());

        let mut hasher = DefaultHasher::new();
        cache_key.hash(&mut hasher);
        meta.adjustments.to_string().hash(&mut hasher);
        let unique_hash = hasher.finish();

        // Thumbnails follow every edit and share the editor's GPU lock. The
        // display variants keep many-layer recipes fast; exports stay exact.
        if let Ok(processed_image) =
            gpu_processing::process_and_get_dynamic_image_with_display_shader(
                context,
                &gpu,
                &develop,
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
    if let Some(recipe) = new_recipe {
        final_image = cut_to_camera_crop(final_image, &recipe, &source_path_str);
    }

    let fallback_orientation_steps = adjustments[geometry::ORIENTATION_STEPS.name()]
        .as_u64()
        .unwrap_or(0) as u8;
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

impl ImportSource {
    /// The file as a buffer rawler reads in place
    /// (`RawSource::new_from_shared`): mapped, only the parts a step looks at
    /// are read from the disk or share, not a copy of every byte.
    fn into_shared(self) -> image_loader::SharedBytes {
        match self {
            Self::Mapped(map) => std::sync::Arc::new(map),
            Self::Read(bytes) => std::sync::Arc::new(bytes),
        }
    }
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

/// Reads the photograph at `path` ahead of a decode that reads all of it
/// (`file_io::prefetch`: over a share faster than the mapping's page
/// faults), except on a card, whose files are read into memory or mapped
/// as they are decoded. Stops once `cancel` (the decode's) is cancelled or
/// the returned guard is dropped (the decode is over, done or failed).
#[must_use = "the read-ahead stops when the guard is dropped"]
pub(crate) fn read_ahead_for_decode(
    path: &Path,
    cancel: Option<rawtonic_runtime::CancelToken>,
) -> ReadAhead {
    let over = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if !read_import_into_memory(path) {
        let done = over.clone();
        rawtonic_engine::file_io::prefetch_until(path, move || {
            done.load(Ordering::Relaxed)
                || cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled())
        });
    }
    ReadAhead(over)
}

/// Ends `read_ahead_for_decode`'s reads when dropped.
pub(crate) struct ReadAhead(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
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
        let file = read_import_source(path)?.into_shared();
        quick_source_preview(&file, path, size).or_else(|| {
            let path_str = path.to_string_lossy();
            // Developed: every byte is read, ahead of the mapping's faults,
            // and the decode's facts read the same buffer.
            let _read_ahead = read_ahead_for_decode(path, None);
            let mut image =
                rawtonic_rawdecode::capture_info::with_held_source(&path_str, file.clone(), || {
                    image_loader::load_and_composite(
                        (*file).as_ref(),
                        &path_str,
                        &serde_json::Value::Null,
                        true,
                        &AppSettings::default(),
                        None,
                    )
                })
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

/// How the import dialog's 100 % view of a photograph was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FullSizeSource {
    /// The camera's embedded JPEG: it has (nearly) the sensor's size.
    Embedded,
    /// The RAW rendered at its full size as the app shows it unedited
    /// (`render`): the camera's JPEG is smaller.
    Decoded,
    /// A JPEG or other image file: its own pixels.
    File,
}

/// An embedded JPEG counts as full size from this share of the sensor's
/// long side: below it a 100 % view would be an upscale, not a focus check.
pub(crate) const FULL_SIZE_SHARE: f64 = 0.95;

/// Whether an embedded JPEG `jpeg_long` pixels long is a full-size view of
/// a sensor `sensor_long` pixels long (`FULL_SIZE_SHARE`); with no sensor
/// size known it is not.
pub(crate) fn embedded_is_full_size(jpeg_long: u32, sensor_long: Option<u32>) -> bool {
    sensor_long.is_some_and(|sensor| f64::from(jpeg_long) >= f64::from(sensor) * FULL_SIZE_SHARE)
}

/// The import dialog's 100 % view of a photograph not in the library yet,
/// as a JPEG and how it was made: a RAW's largest embedded JPEG when it is
/// full size (`embedded_is_full_size`), else the RAW rendered at full size
/// by `render` (from the bytes read here; the app's render of an unedited
/// photograph); a JPEG file's own bytes; another image decoded. Reads the
/// file only; nothing is written or cached.
pub(crate) fn source_full_size_jpeg(
    path: &Path,
    render: &dyn Fn(&[u8]) -> Option<DynamicImage>,
) -> Option<(Vec<u8>, FullSizeSource)> {
    if !path.is_file() || is_cloud_placeholder(path) {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = read_import_source(path)?.into_shared();
        let bytes: &[u8] = (*file).as_ref();
        if !is_raw_file(path) {
            if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                return Some((bytes.to_vec(), FullSizeSource::File));
            }
            let image = quick_source_preview(&file, path, u32::MAX)?;
            return Some((encode_full_size(&image)?, FullSizeSource::File));
        }
        let source = rawler::rawsource::RawSource::new_from_shared(file.clone());
        let params = rawler::decoders::RawDecodeParams::default();
        let decoder = rawler::get_decoder(&source).ok();
        // The sensor's size and orientation, from the RAW's header only.
        let raw = decoder
            .as_ref()
            .and_then(|decoder| decoder.raw_image(&source, &params, true).ok());
        let sensor_long = raw.as_ref().map(|raw| {
            let (w, h) = raw
                .crop_area
                .map_or((raw.width, raw.height), |area| (area.d.w, area.d.h));
            u32::try_from(w.max(h)).unwrap_or(u32::MAX)
        });
        if let Some(jpeg) = image_loader::priority_embedded_jpeg(bytes)
            && let Ok((w, h)) =
                image::ImageReader::with_format(Cursor::new(&jpeg), image::ImageFormat::Jpeg)
                    .into_dimensions()
            && embedded_is_full_size(w.max(h), sensor_long)
        {
            return Some((jpeg, FullSizeSource::Embedded));
        }
        // A full-size JPEG the file format keeps apart (CR3's first track):
        // decoded, turned upright as the RAW and encoded again.
        if let (Some(decoder), Some(raw)) = (&decoder, &raw)
            && let Ok(Some(image)) = decoder.full_image(&source, &params)
            && embedded_is_full_size(image.width().max(image.height()), sensor_long)
        {
            let image = rawtonic_rawdecode::orientation::apply_orientation(image, raw.orientation);
            return Some((encode_full_size(&image)?, FullSizeSource::Embedded));
        }
        // The render reads all of the file: from a network share, ahead of
        // the mapping's page faults (`file_io::prefetch`). A card's file is
        // in memory already. The base look's by-path facts read the same
        // buffer instead of the file again.
        if !read_import_into_memory(path) {
            rawtonic_engine::file_io::prefetch(path);
        }
        let started = std::time::Instant::now();
        let held = path.to_string_lossy();
        let image =
            rawtonic_rawdecode::capture_info::with_held_source(&held, file.clone(), || {
                render(bytes)
            })?;
        let rendered = started.elapsed();
        let jpeg = encode_full_size(&image)?;
        log::info!(
            "Import 100 % view of {}: rendered in {} ms, encoded in {} ms",
            path.display(),
            rendered.as_millis(),
            (started.elapsed() - rendered).as_millis()
        );
        Some((jpeg, FullSizeSource::Decoded))
    }))
    .ok()
    .flatten()
}

/// The width and height of a photograph from its file (read in place), if
/// it tells.
pub(crate) type SizeOfBytes<'a> = dyn Fn(&image_loader::SharedBytes) -> Option<(u32, u32)> + 'a;

/// The size of a RAW's import 100 % view (`source_full_size_jpeg`), from
/// its header and embedded JPEG only, without decoding it: a full-size
/// embedded JPEG's size, else `render_size` (the size the RAW renders at,
/// `export_processing::unedited_size`; a CR3's full-size track has the
/// camera's framing too). What the large view enlarges its preview to while
/// those pixels are made, so they replace it in place. None for other files
/// (their 100 % view is their own pixels, shown at once) or unreadable ones.
pub(crate) fn source_full_size_dimensions(
    path: &Path,
    render_size: &SizeOfBytes,
) -> Option<(u32, u32)> {
    if !is_raw_file(path) || !path.is_file() || is_cloud_placeholder(path) {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = read_import_source(path)?.into_shared();
        let source = rawler::rawsource::RawSource::new_from_shared(file.clone());
        let params = rawler::decoders::RawDecodeParams::default();
        let sensor_long = rawler::get_decoder(&source)
            .ok()
            .and_then(|decoder| decoder.raw_image(&source, &params, true).ok())
            .map(|raw| {
                let (w, h) = raw
                    .crop_area
                    .map_or((raw.width, raw.height), |area| (area.d.w, area.d.h));
                u32::try_from(w.max(h)).unwrap_or(u32::MAX)
            });
        if let Some(jpeg) = image_loader::priority_embedded_jpeg((*file).as_ref())
            && let Ok((w, h)) =
                image::ImageReader::with_format(Cursor::new(&jpeg), image::ImageFormat::Jpeg)
                    .into_dimensions()
            && embedded_is_full_size(w.max(h), sensor_long)
        {
            return Some((w, h));
        }
        render_size(&file)
    }))
    .ok()
    .flatten()
}

/// A full-size view as a JPEG of high quality (a focus check must not show
/// compression blocks; full chroma, so colour edges stay as rendered).
/// libjpeg-turbo: about 2–2.5× faster than the `image` crate's encoder at
/// 24–81 MP (notes: benchmark-results/impd3-import-100-2026-11-08).
pub(crate) fn encode_full_size(image: &DynamicImage) -> Option<Vec<u8>> {
    let rgb = image.to_rgb8();
    let mut compressor = turbojpeg::Compressor::new().ok()?;
    compressor.set_quality(92).ok()?;
    compressor.set_subsamp(turbojpeg::Subsamp::None).ok()?;
    compressor
        .compress_to_vec(turbojpeg::Image {
            pixels: rgb.as_raw().as_slice(),
            width: rgb.width() as usize,
            pitch: rgb.width() as usize * 3,
            height: rgb.height() as usize,
            format: turbojpeg::PixelFormat::RGB,
        })
        .ok()
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
        let file = read_import_source(path)?.into_shared();
        let exif =
            crate::exif_processing::read_exif_data_from_shared(&path.to_string_lossy(), file);
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
fn quick_source_preview(
    file: &image_loader::SharedBytes,
    path: &Path,
    size: u32,
) -> Option<DynamicImage> {
    let bytes: &[u8] = (**file).as_ref();
    let orient = |image: DynamicImage, orientation: Option<u16>| match orientation {
        Some(o) if o > 1 => rawtonic_rawdecode::orientation::apply_orientation(
            image,
            rawler::Orientation::from_u16(o),
        ),
        _ => image,
    };
    let jpeg =
        |data: &[u8]| image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok();
    if rawtonic_engine::heif::is_heif(bytes) {
        // The OS decoder's embedded thumbnail or scaled decode, oriented.
        return rawtonic_engine::heif::preview(bytes, size);
    }
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
    if let Some(image) = image_loader::tiff_jpeg_previews(bytes)
        .and_then(|mut previews| previews.rfind(small_enough))
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
    image_loader::safe_embedded_preview_fallback_shared(file.clone(), &path.to_string_lossy())
}

pub(crate) fn encode_thumbnail(image: &DynamicImage, target_width: u32) -> Result<Vec<u8>> {
    encode_tile(&crate::image_processing::resample_display_image(
        image,
        target_width,
        target_width,
    ))
}

/// The grid's small and medium tiles of `image` as JPEGs (the small one
/// reduced from the medium one, `resample_display_pair`).
fn encode_tiles(image: &DynamicImage, settings: &AppSettings) -> Result<(Vec<u8>, Vec<u8>)> {
    let (medium, small) = crate::image_processing::resample_display_pair(
        image,
        settings.medium_thumbnail_resolution.unwrap_or(1280),
        settings.small_thumbnail_resolution.unwrap_or(480),
    );
    Ok((encode_tile(&small)?, encode_tile(&medium)?))
}

fn encode_tile(thumbnail: &DynamicImage) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 75);
    encoder.encode_image(&thumbnail.to_rgb8())?;
    Ok(buf.into_inner())
}

fn cache_thumbnail(path: &Path, bytes: &[u8]) -> Result<()> {
    rawtonic_fs::atomic_write(path, bytes)
}

/// Camera pixels depend on the source and orientation decoder, not the RAW
/// renderer. This key is written only after an embedded preview was decoded.
/// Bump `embedded-raw-orientation-v1` when preview extraction or orientation
/// changes: RAW ENGINE_VERSION intentionally does not invalidate these pixels.
fn embedded_thumbnail_hash(path_str: &str) -> Option<String> {
    let (source, _) = parse_virtual_path(path_str);
    if !is_raw_file(&source) {
        return None;
    }
    let (label, mtime) = label_and_mtime(path_str, &source);
    let mut hash = blake3::Hasher::new();
    hash.update(label.as_bytes());
    hash.update(&mtime?.to_le_bytes());
    hash.update(b"embedded-raw-orientation-v1");
    // Cut to the camera's crop where the JPEG shows the whole frame
    // (`embedded_in_camera_crop`; Fujifilm aspect ratios, 2026-11-19).
    hash.update(b"\0embedded-camera-crop-v1");
    Some(hash.finalize().to_hex().to_string())
}

fn preferred_thumbnail_hash<'a>(
    directory: &Path,
    developed: &'a str,
    embedded: Option<&'a str>,
    allowed: bool,
) -> &'a str {
    embedded
        .filter(|hash| {
            allowed
                && directory.join(format!("{hash}_small.jpg")).is_file()
                && directory.join(format!("{hash}_medium.jpg")).is_file()
        })
        .unwrap_or(developed)
}

/// The keys a tile of `path_str` may have been cached under, newest
/// first: before ORF maker-note previews, before camera identities, with its display copy renewed (the lookup cannot tell), while its proxy still clamped, then before any.
fn older_tile_keys(path_str: &str, adjustments_bytes: &[u8]) -> Vec<String> {
    [
        thumbnail_hash_before_orf_preview(path_str, adjustments_bytes),
        thumbnail_hash_before_camera_identity(path_str, adjustments_bytes),
        thumbnail_hash_with_display_copy_renewed(path_str, adjustments_bytes),
        thumbnail_hash_with_display_copy_held(path_str, adjustments_bytes),
        thumbnail_hash_before_refreshes(path_str, adjustments_bytes),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A face retouch tile is made again (a new key). A disconnected original
/// without a preview proxy cannot be developed again, so the tile cached
/// under `old` stays until the original is back (a blank grid is worse).
fn keep_tile_of_unavailable_original(
    directory: &Path,
    source: &Path,
    recipe: &serde_json::Value,
    new: String,
    old: impl FnOnce() -> Vec<String>,
) -> String {
    // The cached tiles first: they are local, and asking whether the
    // original is available opens it (GRD1/GRD2: 7–25 ms per tile on a
    // Wi-Fi share). Without an old tile to keep the answer is `new` either
    // way, so the original is only asked about when one is there.
    if thumbnail_files_exist(directory, &new) {
        return new;
    }
    let Some(old) = old()
        .into_iter()
        .find(|old| *old != new && thumbnail_files_exist(directory, old))
    else {
        return new;
    };
    // A proxy of another variant or frame cannot show this recipe, and none
    // is developed for an evicted cloud original (`get_or_generate`
    // develops nothing without downloading it): its old tile stays.
    if crate::preview_proxy::source_available(source)
        || (!is_cloud_placeholder(source)
            && !proxy_lacks_camera_defaults(&rawtonic_engine::develop_defaults::current(), recipe)
            && crate::preview_proxy::stands_for_by_recipe(source, recipe) == Some(true))
    {
        return new;
    }
    old
}

/// A never-edited ORF whose original is unavailable keeps the tile an
/// earlier version developed (`before`) until it can be made again from the
/// camera's preview: a Standard preview may stand for the recipe
/// (`keep_tile_of_unavailable_original`) yet be refused by the renderer when
/// it was built with other decode preferences, and a blank tile is worse.
fn orf_tile_while_offline(
    directory: &Path,
    source: &Path,
    new: String,
    before: impl FnOnce() -> Option<String>,
) -> String {
    // Looked at only for an ORF whose tile is missing (each lookup's cost),
    // and the original (an open) only when the earlier tile is there.
    let orf = source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("orf"));
    if !orf || thumbnail_files_exist(directory, &new) {
        return new;
    }
    before()
        .filter(|before| *before != new && thumbnail_files_exist(directory, before))
        .filter(|_| !crate::preview_proxy::source_available(source))
        .unwrap_or(new)
}

/// A tile developed from a proxy while the original is unavailable cannot
/// carry a never-edited photo's camera defaults (its camera is read from the
/// original), so it does not replace one cached while it was readable.
fn proxy_lacks_camera_defaults(
    defaults: &rawtonic_engine::develop_defaults::DevelopDefaults,
    recipe: &serde_json::Value,
) -> bool {
    !defaults.cameras.is_empty() && rawtonic_engine::develop_defaults::follows_defaults(recipe)
}

fn thumbnail_files_exist(directory: &Path, hash: &str) -> bool {
    directory.join(format!("{hash}_small.jpg")).is_file()
        && directory.join(format!("{hash}_medium.jpg")).is_file()
}

fn embedded_thumbnail_allowed(path_str: &str, adjustments: &[u8], settings: &AppSettings) -> bool {
    let (source, _) = parse_virtual_path(path_str);
    // The header check reads the master's recipe (a virtual copy's too).
    let (_, master_sidecar) = parse_virtual_path(&source.to_string_lossy());
    let header_current = || {
        crate::preview_proxy::standard_header_is_current(
            &source,
            settings,
            &serde_json::Value::Null,
        )
    };
    let defaults_apply = || rawtonic_engine::develop_defaults::for_path(&source).is_some();
    (adjustments.is_empty() || adjustments == b"null")
        && is_raw_file(&source)
        && !settings.always_decode_raw_thumbnails.unwrap_or(false)
        && if is_cloud_placeholder(&master_sidecar) {
            // The header check reads the master sidecar, which would download
            // an evicted one on the lookup worker: the camera first.
            !defaults_apply() && !header_current()
        } else {
            // The local proxy header first: a photo with a current proxy
            // never shows its embedded preview, so its camera is not read
            // (CRD6).
            !header_current() && !defaults_apply()
        }
}

/// Track one current developed recipe per photo/copy. Only strict cache file
/// names from our manifest are eligible; anchored eviction refuses links and
/// respects active readers. Originals and embedded previews are never eligible.
fn supersede_thumbnail(directory: &Path, path_str: &str, hash: &str) -> Result<()> {
    static UPDATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _update = lock_recover(&UPDATE);
    let (source, _) = parse_virtual_path(path_str);
    let owner = blake3::hash(cache_label(path_str, &source).as_bytes()).to_hex();
    let index = directory.join(format!("{owner}.latest"));
    let previous = fs::read_to_string(&index).ok();
    cache_thumbnail(&index, hash.as_bytes())?;
    if let Some(previous) = previous
        .filter(|old| old != hash && old.len() == 64 && old.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        let root = directory
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No thumbnail cache root"))?;
        let small = format!("{previous}_small.jpg");
        let medium = format!("{previous}_medium.jpg");
        for name in [small, medium] {
            crate::storage_fs::remove_cache_entry(root, directory, &name)?;
        }
    }
    Ok(())
}

/// Where `path_str`'s thumbnails are cached for its file and recipe as they
/// are now, with the rating and edit state read from its sidecar.
struct ThumbnailCacheEntry {
    developed_hash: String,
    embedded_hash: Option<String>,
    small: PathBuf,
    medium: PathBuf,
    rating: Option<u8>,
    is_edited: Option<bool>,
    /// The LUT the recipe names (`lutPath`).
    lut: Option<String>,
    /// The tile was developed while that LUT did not load: it is saved
    /// under `lut_missing_hash`, which no lookup asks for, so the next
    /// lookup develops it again (with the LUT once it loads).
    lut_missing: bool,
}

/// The key of a tile developed while its recipe's LUT did not load: a hash
/// like every tile key (the webview renews tiles by 64-hex names).
fn lut_missing_hash(developed: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(developed.as_bytes());
    hasher.update(b"\0lut-missing-v1");
    hasher.finalize().to_hex().to_string()
}

/// A photo's cached thumbnails: small path, medium path, rating, edited.
pub(crate) type CachedThumbnails = (String, String, Option<u8>, Option<bool>);

impl ThumbnailCacheEntry {
    fn of(
        path_str: &str,
        thumb_cache_dir: &Path,
        app_handle: &AppHandle,
        settings: &AppSettings,
    ) -> Option<Self> {
        Self::lookup(path_str, thumb_cache_dir, settings, |source, sidecar| {
            enqueue_metadata(app_handle, path_str.to_string(), source, sidecar);
        })
    }

    fn lookup(
        path_str: &str,
        thumb_cache_dir: &Path,
        settings: &AppSettings,
        queue_metadata: impl FnOnce(PathBuf, PathBuf),
    ) -> Option<Self> {
        with_lookup_facts(|| Self::lookup_keys(path_str, thumb_cache_dir, settings, queue_metadata))
    }

    fn lookup_keys(
        path_str: &str,
        thumb_cache_dir: &Path,
        settings: &AppSettings,
        queue_metadata: impl FnOnce(PathBuf, PathBuf),
    ) -> Option<Self> {
        let (source_path, sidecar_path) = parse_virtual_path(path_str);

        // Rating and edit state only when a sidecar was read: without one they
        // are unknown here (the listing has them), and an event saying 0 could
        // arrive after the photograph was registered with its rating.
        let mut lut = None;
        let (rating, is_edited, adjustments_bytes) = if is_cloud_placeholder(&sidecar_path) {
            queue_metadata(source_path.clone(), sidecar_path.clone());
            (None, None, Vec::new())
        } else if let Some(meta) = thumbnail_metadata(&source_path, &sidecar_path) {
            {
                lut = meta.adjustments[effects::LUT_PATH.into_name()]
                    .as_str()
                    .map(str::to_string);
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
        let cache_hash = keep_tile_of_unavailable_original(
            thumb_cache_dir,
            &source_path,
            &serde_json::from_slice(&adjustments_bytes).unwrap_or_default(),
            cache_hash,
            || older_tile_keys(path_str, &adjustments_bytes),
        );
        let cache_hash = orf_tile_while_offline(thumb_cache_dir, &source_path, cache_hash, || {
            thumbnail_hash_before_orf_preview(path_str, &adjustments_bytes)
        });
        let embedded_hash = embedded_thumbnail_hash(path_str);
        let selected = preferred_thumbnail_hash(
            thumb_cache_dir,
            &cache_hash,
            embedded_hash.as_deref(),
            embedded_thumbnail_allowed(path_str, &adjustments_bytes, settings),
        );
        Some(Self {
            developed_hash: cache_hash.clone(),
            embedded_hash: embedded_hash.clone(),
            small: thumb_cache_dir.join(format!("{}_small.jpg", selected)),
            medium: thumb_cache_dir.join(format!("{}_medium.jpg", selected)),
            rating,
            is_edited,
            lut,
            lut_missing: false,
        })
    }

    fn save(
        &mut self,
        path_str: &str,
        image: &DynamicImage,
        embedded: bool,
        settings: &AppSettings,
    ) -> Result<()> {
        let directory = self
            .small
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No cache directory"))?
            .to_path_buf();
        let lut_missing =
            (!embedded && self.lut_missing).then(|| lut_missing_hash(&self.developed_hash));
        let hash = if embedded {
            self.embedded_hash.as_ref().unwrap_or(&self.developed_hash)
        } else {
            lut_missing.as_ref().unwrap_or(&self.developed_hash)
        };
        self.small = directory.join(format!("{hash}_small.jpg"));
        self.medium = directory.join(format!("{hash}_medium.jpg"));
        let _small_use = crate::storage_fs::pin_file(&self.small);
        let _medium_use = crate::storage_fs::pin_file(&self.medium);
        let (small, medium) = encode_tiles(image, settings)?;
        cache_thumbnail(&self.small, &small)?;
        cache_thumbnail(&self.medium, &medium)?;
        // If a current-looking proxy could not actually be used (no GPU or
        // damaged payload), lookup may request the developed key. Backfill it
        // from the actual fallback pixels, so subsequent requests still hit.
        let backfill = embedded && !embedded_thumbnail_allowed(path_str, b"null", settings);
        if backfill {
            for (size, bytes) in [("small", &small), ("medium", &medium)] {
                cache_thumbnail(
                    &directory.join(format!("{}_{size}.jpg", self.developed_hash)),
                    bytes,
                )?;
            }
        }
        if (!embedded || backfill)
            && lut_missing.is_none()
            && let Err(error) = supersede_thumbnail(&directory, path_str, &self.developed_hash)
        {
            log::warn!("Superseded thumbnail of {path_str} not removed: {error:#}");
        }
        Ok(())
    }

    /// Both files, marked used, if the cache has them.
    fn cached(&self) -> Option<CachedThumbnails> {
        let _small_use = crate::storage_fs::pin_file(&self.small);
        let _medium_use = crate::storage_fs::pin_file(&self.medium);
        if !(self.small.exists() && self.medium.exists()) {
            return None;
        }
        crate::storage_fs::mark_used(&self.small);
        crate::storage_fs::mark_used(&self.medium);
        Some(self.paths())
    }

    fn paths(&self) -> CachedThumbnails {
        (
            self.small.to_string_lossy().into_owned(),
            self.medium.to_string_lossy().into_owned(),
            self.rating,
            self.is_edited,
        )
    }
}

/// `path_str`'s cached thumbnails for its file and recipe as they are now,
/// if both exist; nothing is made.
pub(crate) fn cached_thumbnails(
    path_str: &str,
    thumb_cache_dir: &Path,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<CachedThumbnails> {
    // One read of each sidecar for the whole lookup (CRD6).
    rawtonic_export::sidecars::with_sidecars_kept(|| {
        ThumbnailCacheEntry::of(path_str, thumb_cache_dir, app_handle, settings)?.cached()
    })
}

pub(crate) fn generate_single_thumbnail_and_cache(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<CachedThumbnails> {
    // One read of each sidecar for the lookup and the render (CRD6).
    rawtonic_export::sidecars::with_sidecars_kept(|| {
        cache_or_make_thumbnail(
            path_str,
            thumb_cache_dir,
            gpu_context,
            preloaded_image,
            force_regenerate,
            app_handle,
            settings,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn cache_or_make_thumbnail(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<CachedThumbnails> {
    // Developing again needs the file; an evicted original is not downloaded
    // for it (and the cache entry itself would open it for its camera).
    if force_regenerate && is_cloud_placeholder(&parse_virtual_path(path_str).0) {
        return None;
    }
    let mut entry = ThumbnailCacheEntry::of(path_str, thumb_cache_dir, app_handle, settings)?;

    if !force_regenerate && let Some(cached) = entry.cached() {
        return Some(cached);
    }

    let (source_path, _) = parse_virtual_path(path_str);
    if is_cloud_placeholder(&source_path) {
        return None;
    }

    let mut embedded = false;
    let made = generate_thumbnail_with_source(
        path_str,
        gpu_context,
        preloaded_image,
        app_handle,
        &mut embedded,
    );
    // The render took the recipe's LUT from the same cache.
    entry.lut_missing = entry.lut.as_deref().is_some_and(|lut| {
        crate::lut_processing::get_or_load_lut(
            &app_handle.state::<crate::state::luts::LutState>(),
            lut,
        )
        .is_err()
    });
    let made = made.and_then(|image| entry.save(path_str, &image, embedded, settings));
    if let Err(error) = made {
        log::warn!("Thumbnail of {path_str} not made: {error:#}");
        return None;
    }
    Some(entry.paths())
}

/// Makes `path`'s thumbnails again after its preview proxy was written (the
/// thumbnail is then developed from it) and tells the frontend.
pub fn refresh_thumbnail(app_handle: &AppHandle, path: &str) {
    develop_thumbnail_again(app_handle, path, false);
}

/// Develops `path`'s thumbnails again even where they are cached (the
/// open photograph's render input came in,
/// `color_noise_view::for_thumbnail`) and tells the frontend.
pub(crate) fn redevelop_thumbnail(app_handle: &AppHandle, path: &str) {
    develop_thumbnail_again(app_handle, path, true);
}

fn develop_thumbnail_again(app_handle: &AppHandle, path: &str, force: bool) {
    let gpu_context = crate::gpu_processing::get_or_init_gpu_context(
        &app_handle.state::<crate::state::gpu::GpuState>(),
        app_handle,
    )
    .ok();
    let settings = crate::state::settings::current(app_handle);
    let Ok(cache_dir) = get_thumb_cache_dir(app_handle) else {
        return;
    };
    if let Some((small, medium, rating, is_edited)) = generate_single_thumbnail_and_cache(
        path,
        &cache_dir,
        gpu_context.as_ref(),
        None,
        force,
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

/// The next job, or `None` (and the rest of the queue dropped) once
/// thumbnail generation was cancelled.
pub(crate) fn take_next_thumbnail_job(
    queue: &mut ThumbnailQueue,
    work: ThumbnailWork,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Option<ThumbnailJob> {
    if cancelled.load(Ordering::SeqCst) {
        queue.clear();
        return None;
    }
    queue.take(work)
}

pub fn start_thumbnail_workers(app_handle: tauri::AppHandle) {
    let manager = app_handle.state::<ThumbnailState>().manager.clone();
    let settings = crate::state::settings::current(&app_handle);
    let thread_count = settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 16);

    // One thread looks the viewports up in the cache, the others decode.
    let workers = std::iter::once(ThumbnailWork::Lookup).chain(std::iter::repeat_n(
        ThumbnailWork::Decode,
        thread_count as usize,
    ));
    for work in workers {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();

        rawtonic_runtime::spawn_render_thread("thumbnails", move || {
            loop {
                let job = {
                    let mut queue = lock_recover(&manager_clone.queue);
                    while !queue.has(work) {
                        queue = manager_clone
                            .cvar
                            .wait(queue)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    let thumbnails = app_clone.state::<ThumbnailState>();
                    let Some(job) =
                        take_next_thumbnail_job(&mut queue, work, &thumbnails.cancelled)
                    else {
                        // Cancelled: the queue is dropped, the progress is already reset.
                        continue;
                    };

                    if let ThumbnailJob::Decode(path) = &job {
                        let mut processing = lock_recover(&manager_clone.processing_now);
                        if processing.contains(path) {
                            increment_thumbnail_progress(&thumbnails, &app_clone);
                            continue;
                        }
                        processing.insert(path.clone());
                    }
                    job
                };
                // A GPU check of an AI model never times the CPU or the GPU
                // beside a thumbnail (rawtonic-onnx `background`).
                rawtonic_onnx::note_activity();

                let current_settings = crate::state::settings::current(&app_clone);
                let cache_dir = get_thumb_cache_dir(&app_clone);

                match job {
                    ThumbnailJob::Lookup { paths, generation } => {
                        let Ok(cache_dir) = cache_dir else { continue };
                        let mut found = Vec::new();
                        let mut misses = Vec::new();
                        for path in paths {
                            match cached_thumbnails(
                                &path,
                                &cache_dir,
                                &app_clone,
                                &current_settings,
                            ) {
                                Some(cached) => found.push((path, cached)),
                                None => misses.push(path),
                            }
                        }
                        let current =
                            lock_recover(&manager_clone.queue).queue_misses(generation, misses);
                        if current {
                            manager_clone.cvar.notify_all();
                        }
                        // An older viewport's thumbnails are shown too (they
                        // are made), but count only toward their own progress.
                        emit_thumbnails_generated(&app_clone, &found);
                        if current && !found.is_empty() {
                            complete_thumbnail_progress(
                                &app_clone.state::<ThumbnailState>(),
                                found.len(),
                                &app_clone,
                            );
                        }
                    }
                    ThumbnailJob::Decode(path_to_process) => {
                        if let Ok(cache_dir) = cache_dir {
                            let gpu_context = crate::gpu_processing::get_or_init_gpu_context(
                                &app_clone.state::<crate::state::gpu::GpuState>(),
                                &app_clone,
                            )
                            .ok();
                            // Only a photo the cache did not have is read
                            // ahead: a cached one costs no read of its file.
                            if manager_clone.rotational_disk.load(Ordering::Relaxed) {
                                let _io_permit = lock_recover(&manager_clone.io_gate);
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
                            increment_thumbnail_progress(
                                &app_clone.state::<ThumbnailState>(),
                                &app_clone,
                            );
                        }
                        lock_recover(&manager_clone.processing_now).remove(&path_to_process);
                    }
                }
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
    let thumbnails = app_handle.state::<ThumbnailState>();
    let manager = &thumbnails.manager;

    let mut queue = lock_recover(&manager.queue);

    if paths.is_empty() {
        queue.clear();
        thumbnails.reset_progress();

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        manager.cvar.notify_all();
        return Ok(());
    }

    // A new generation starts: an earlier cancel must not stop it.
    thumbnails.cancelled.store(false, Ordering::SeqCst);

    // This command receives a viewport snapshot, not an accumulating job list.
    // Do not spend CPU decoding images the user has already scrolled past.
    let processing = lock_recover(&manager.processing_now);
    let mut unique_paths = visible_thumbnail_paths(paths, &processing);
    drop(processing);
    if manager.rotational_disk.load(Ordering::Relaxed) {
        unique_paths.sort();
    }
    // Looked up in the cache first, then decoded in this (visible) order.
    queue.replace(unique_paths);

    let queue_len = queue.queued();
    drop(queue);

    let (current, total) = thumbnails.set_queued(queue_len);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );

    manager.cvar.notify_all();
    Ok(())
}

pub fn add_to_thumbnail_queue(thumbnails: &ThumbnailState, count: usize, app_handle: &AppHandle) {
    let (current, total) = thumbnails.add_to_total(count);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );
}

pub fn increment_thumbnail_progress(thumbnails: &ThumbnailState, app_handle: &AppHandle) {
    complete_thumbnail_progress(thumbnails, 1, app_handle);
}

/// Counts `count` finished photos with one progress event.
fn complete_thumbnail_progress(thumbnails: &ThumbnailState, count: usize, app_handle: &AppHandle) {
    if let Some((current, total)) = thumbnails.complete(count) {
        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": current, "total": total }),
        );
    } else {
        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        let _ = app_handle.emit("thumbnail-generation-complete", true);
    }
}

/// The medium thumbnail last announced to the webview for each photograph
/// (what its filmstrip tile shows), so the multi view can show it in a cell
/// without resolving the cache entry again (the recipe read that takes up to
/// ~30 ms per photograph).
static ANNOUNCED_MEDIUM: LazyLock<Mutex<HashMap<String, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Paths kept in [`ANNOUNCED_MEDIUM`] at most (about 2 MB); past it the map
/// starts again from the photographs announced next.
const ANNOUNCED_MEDIUM_LIMIT: usize = 10_000;

fn note_announced_medium(path: &str, medium: &str) {
    if medium.is_empty() {
        return;
    }
    let mut announced = ANNOUNCED_MEDIUM.lock().unwrap_or_else(|e| e.into_inner());
    if announced.len() >= ANNOUNCED_MEDIUM_LIMIT && !announced.contains_key(path) {
        announced.clear();
    }
    announced.insert(path.to_string(), PathBuf::from(medium));
}

/// The medium thumbnail the webview was last told about for `path_str`,
/// while that file is still in the cache: the image its filmstrip tile
/// shows now (it may predate a recipe change whose thumbnail is still being
/// made). Nothing is resolved or generated.
pub(crate) fn announced_medium_thumbnail(path_str: &str) -> Option<PathBuf> {
    let path = ANNOUNCED_MEDIUM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path_str)
        .cloned()?;
    let _file_use = crate::storage_fs::pin_file(&path);
    if !path.is_file() {
        return None;
    }
    crate::storage_fs::mark_used(&path);
    Some(path)
}

pub(crate) fn emit_thumbnail_generated(
    app_handle: &AppHandle,
    path: &str,
    small_thumbnail_path: &str,
    medium_thumbnail_path: &str,
    rating: Option<u8>,
    is_edited: Option<bool>,
) {
    note_announced_medium(path, medium_thumbnail_path);
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

/// A viewport's cached thumbnails as one event (`thumbnails-generated`, a
/// list of `thumbnail-generated` payloads).
fn emit_thumbnails_generated(app_handle: &AppHandle, found: &[(String, CachedThumbnails)]) {
    if found.is_empty() {
        return;
    }
    let batch: Vec<_> = found
        .iter()
        .map(|(path, (small, medium, rating, is_edited))| {
            note_announced_medium(path, medium);
            serde_json::json!({
                "path": path,
                "thumbnailPath": small,
                "previewPath": medium,
                "rating": rating,
                "is_edited": is_edited
            })
        })
        .collect();
    let _ = app_handle.emit("thumbnails-generated", batch);
}

// Recovery bypasses the viewport generation queue. Admit it before entering
// the blocking pool (`ThumbnailState::restoration_slot`) so a cache-miss
// burst cannot allocate many RAW decodes.
async fn restoration_job<R: Send + 'static>(
    slot: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    let permit = slot
        .acquire_owned()
        .await
        .map_err(|error| error.to_string())?;
    super::blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

/// Rebuilds a missing disk-cache entry without replacing the viewport queue.
#[tauri::command]
pub async fn restore_thumbnail(
    path: String,
    app_handle: AppHandle,
) -> Result<(String, String), String> {
    let slot = app_handle
        .state::<ThumbnailState>()
        .restoration_slot
        .clone();
    restoration_job(slot, move || {
        let gpu = crate::gpu_processing::get_or_init_gpu_context(
            &app_handle.state::<crate::state::gpu::GpuState>(),
            &app_handle,
        )
        .ok();
        let settings = crate::state::settings::current(&app_handle);
        let directory = get_thumb_cache_dir(&app_handle)?;
        generate_single_thumbnail_and_cache(
            &path,
            &directory,
            gpu.as_ref(),
            None,
            false,
            &app_handle,
            &settings,
        )
        .map(|(small, medium, _, _)| (small, medium))
        .ok_or_else(|| "Thumbnail source is unavailable".to_string())
    })
    .await?
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

/// The medium thumbnail cached for `path_str` as it is stored now (recipe
/// and file), if there is one; nothing is generated.
pub(crate) fn cached_medium_thumbnail(app_handle: &AppHandle, path_str: &str) -> Option<PathBuf> {
    let settings = crate::state::settings::current(app_handle);
    let directory = get_thumb_cache_dir(app_handle).ok()?;
    let path = ThumbnailCacheEntry::of(path_str, &directory, app_handle, &settings)?.medium;
    if path.is_file() {
        // The pin protects the timestamp update from eviction; the returned
        // path is subsequently protected by its recent-use lease.
        let _file_use = crate::storage_fs::pin_file(&path);
        crate::storage_fs::mark_used(&path);
        Some(path)
    } else {
        None
    }
}

pub fn get_cached_or_generate_thumbnail_image(
    path_str: &str,
    app_handle: &AppHandle,
    gpu_context: Option<&GpuContext>,
) -> Result<DynamicImage> {
    let thumb_cache_dir = get_thumb_cache_dir(app_handle).map_err(|e| anyhow::anyhow!(e))?;
    let settings = crate::state::settings::current(app_handle);
    let Some(mut entry) =
        ThumbnailCacheEntry::of(path_str, &thumb_cache_dir, app_handle, &settings)
    else {
        return generate_thumbnail_data(path_str, gpu_context, None, app_handle);
    };
    let _medium_use = crate::storage_fs::pin_file(&entry.medium);
    if let Ok(image) = image::open(&entry.medium) {
        crate::storage_fs::mark_used(&entry.medium);
        return Ok(image);
    }
    let mut embedded = false;
    let image =
        generate_thumbnail_with_source(path_str, gpu_context, None, app_handle, &mut embedded)?;
    if let Err(error) = entry.save(path_str, &image, embedded, &settings) {
        log::warn!("Thumbnail of {path_str} not cached: {error:#}");
    }
    let medium = settings.medium_thumbnail_resolution.unwrap_or(1280);
    Ok(as_cached(path_str, image, medium))
}

/// The thumbnail as its cached medium file gives it (`medium` pixels wide,
/// compressed; the same bytes `ThumbnailCacheEntry::save` writes), also when
/// that file could not be written: the photograph is read from the same
/// pixels the first time and every later time, so its content embedding and
/// culling scores do not depend on whether the pass or the grid made its
/// thumbnail (notes: benchmark-results/cul3-2026-11-21/REPORT.md). The
/// `generated` image only if it cannot be encoded.
fn as_cached(path_str: &str, generated: DynamicImage, medium: u32) -> DynamicImage {
    match encode_thumbnail(&generated, medium).and_then(|jpeg| {
        Ok(image::load_from_memory_with_format(
            &jpeg,
            image::ImageFormat::Jpeg,
        )?)
    }) {
        Ok(cached) => cached,
        Err(error) => {
            log::warn!("Thumbnail of {path_str} not read as cached: {error:#}");
            generated
        }
    }
}

#[cfg(test)]
mod kept_sidecar_tests {
    /// One tile job (lookup + render) reads its sidecar once: the tile's
    /// metadata and the proxy checks' master recipe (`load_sidecar`) share
    /// the first read (CRD6: ~22 ms per read on a LAN share).
    #[test]
    fn a_tile_job_reads_its_sidecar_once() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.ARW");
        std::fs::write(&source, b"raw").unwrap();
        let sidecar = temp.path().join("photo.ARW.rrdata");
        let written = crate::image_processing::ImageMetadata {
            rating: 4,
            adjustments: serde_json::json!({ "exposure": 0.5 }),
            ..Default::default()
        };
        std::fs::write(&sidecar, serde_json::to_string(&written).unwrap()).unwrap();
        rawtonic_export::sidecars::with_sidecars_kept(|| {
            let tile = super::thumbnail_metadata(&source, &sidecar).unwrap();
            assert_eq!(tile.rating, 4);
            // Gone after the first read: the job's later reads are that one.
            std::fs::remove_file(&sidecar).unwrap();
            assert_eq!(
                super::thumbnail_metadata(&source, &sidecar).unwrap().rating,
                4
            );
            let master = crate::exif_processing::load_sidecar(&sidecar);
            assert_eq!(master.rating, 4);
            assert_eq!(master.adjustments["exposure"], 0.5);
        });
        // Outside a job, the file as it is.
        assert!(super::thumbnail_metadata(&source, &sidecar).is_none());
    }
}

#[cfg(test)]
mod share_lookup_tests {

    fn set_mtime(path: &std::path::Path, time: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }
    /// A cached developed tile is found without opening its original (GRD1:
    /// an open is a server round trip on a share, 7–25 ms per tile on
    /// Wi-Fi); the folder listing reads the sidecar from its file and the
    /// lookup after it takes that read while the sidecar is unchanged; an
    /// edit made meanwhile is read and asks for a new tile.
    #[test]
    fn a_cached_tile_is_looked_up_without_opening_its_original_or_reading_its_sidecar_again() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.NEF");
        std::fs::write(&source, b"raw").unwrap();
        let sidecar = temp.path().join("photo.NEF.rrdata");
        std::fs::write(
            &sidecar,
            br#"{"version":1,"rating":3,"adjustments":{"exposure":0.5}}"#,
        )
        .unwrap();
        let listed = std::fs::metadata(&sidecar).unwrap().modified().unwrap();
        let path = source.to_str().unwrap();
        let cache = temp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let settings = crate::AppSettings::default();
        let lookup = || {
            rawtonic_export::sidecars::with_sidecars_kept(|| {
                super::ThumbnailCacheEntry::lookup(path, &cache, &settings, |_, _| {
                    panic!("no cloud sidecar")
                })
                .unwrap()
            })
        };
        let list = || {
            crate::file_management::listing::resolve_image_metadata(
                &source, &sidecar, false, &settings,
            )
            .rating
        };
        let rewrite_unchanged = |rating: u8| {
            std::fs::write(
                &sidecar,
                format!(r#"{{"version":1,"rating":{rating},"adjustments":{{"exposure":0.5}}}}"#),
            )
            .unwrap();
            set_mtime(&sidecar, listed);
        };
        let entry = lookup();
        assert_eq!(entry.rating, Some(3));
        for tile in [&entry.small, &entry.medium] {
            std::fs::write(tile, b"cached tile").unwrap();
        }

        // Same size, same file, time put back: nothing tells it changed.
        // Opening the folder again reads the file all the same.
        rewrite_unchanged(6);
        assert_eq!(list(), 6, "the listing took an earlier read");

        // The lookup after it takes the listing's read, and finds the
        // cached tile without opening the original.
        rewrite_unchanged(4);
        crate::preview_proxy::SOURCE_OPENS.with(|opens| opens.set(0));
        let cached = lookup();
        assert_eq!(cached.rating, Some(6), "the sidecar was read again");
        assert!(cached.cached().is_some());
        assert_eq!(
            crate::preview_proxy::SOURCE_OPENS.with(std::cell::Cell::get),
            0,
            "the original was opened"
        );

        // Edited meanwhile (a new time): read, and a new tile is asked for.
        std::fs::write(
            &sidecar,
            br#"{"version":1,"rating":4,"adjustments":{"exposure":0.7}}"#,
        )
        .unwrap();
        set_mtime(&sidecar, listed + std::time::Duration::from_secs(2));
        let edited = lookup();
        assert_eq!(edited.rating, Some(4));
        assert!(edited.cached().is_none());
    }

    /// A never-edited RAW's tile is its camera preview, cached under its
    /// own key: looking it up opens the original neither while the tile is
    /// still to be made nor once it is cached (GRD2: one open per lookup,
    /// two for an ORF, 18 ms each on a Wi-Fi share), as long as no older
    /// tile could stand in for an unavailable original; the keys it asks
    /// for share one look for a library beside the photo.
    #[test]
    fn a_never_edited_raw_is_looked_up_without_opening_its_original() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let settings = crate::AppSettings::default();
        for name in ["photo.NEF", "photo.ORF"] {
            let source = temp.path().join(name);
            std::fs::write(&source, b"raw").unwrap();
            let path = source.to_str().unwrap();
            let lookup = || {
                crate::preview_proxy::SOURCE_OPENS.with(|opens| opens.set(0));
                super::LABEL_READS.with(|reads| reads.set(0));
                let entry = rawtonic_export::sidecars::with_sidecars_kept(|| {
                    super::ThumbnailCacheEntry::lookup(path, &cache, &settings, |_, _| {
                        panic!("no cloud sidecar")
                    })
                    .unwrap()
                });
                // Its label (a library's manifests looked for beside it)
                // once for all the keys asked.
                assert_eq!(super::LABEL_READS.with(std::cell::Cell::get), 1);
                let opens = crate::preview_proxy::SOURCE_OPENS.with(std::cell::Cell::get);
                (entry, opens)
            };
            let (missing, opens) = lookup();
            assert!(missing.cached().is_none());
            assert_eq!(
                opens, 0,
                "{name}: the original was opened for a tile to make"
            );
            let embedded = super::embedded_thumbnail_hash(path).unwrap();
            for size in ["small", "medium"] {
                std::fs::write(cache.join(format!("{embedded}_{size}.jpg")), b"tile").unwrap();
            }
            let (cached, opens) = lookup();
            assert!(cached.cached().is_some());
            assert!(cached.small.ends_with(format!("{embedded}_small.jpg")));
            assert_eq!(
                opens, 0,
                "{name}: the original was opened for a cached tile"
            );
        }
    }
}

#[cfg(test)]
mod source_preview_tests {
    #[test]
    fn a_thumbnail_just_made_is_read_as_its_cached_file() {
        let temp = tempfile::tempdir().unwrap();
        let medium = temp.path().join("hash_medium.jpg");
        let generated =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1600, 1000, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
            }));
        // What a later reading opens: the cached file.
        std::fs::write(&medium, super::encode_thumbnail(&generated, 1280).unwrap()).unwrap();
        let later = image::open(&medium).unwrap();
        assert_ne!(later, generated);
        // The first reading, whether or not that file was written.
        assert_eq!(super::as_cached("photo.jpg", generated, 1280), later);
    }

    #[test]
    fn an_announced_medium_thumbnail_is_found_while_its_file_is_cached() {
        let temp = tempfile::tempdir().unwrap();
        let medium = temp.path().join("hash_medium.jpg");
        std::fs::write(&medium, b"jpeg").unwrap();
        let photo = temp.path().join("photo.ARW");
        let photo = photo.to_str().unwrap();
        assert_eq!(super::announced_medium_thumbnail(photo), None);
        super::note_announced_medium(photo, medium.to_str().unwrap());
        assert_eq!(
            super::announced_medium_thumbnail(photo),
            Some(medium.clone())
        );
        std::fs::remove_file(&medium).unwrap();
        assert_eq!(super::announced_medium_thumbnail(photo), None);
    }

    #[test]
    fn a_tile_developed_without_its_lut_is_kept_apart_and_made_again() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.jpg");
        std::fs::write(&source, b"source unchanged").unwrap();
        let path = source.to_str().unwrap();
        let lut = temp.path().join("look.cube");
        let recipe = serde_json::to_vec(&serde_json::json!({
            "lutPath": lut.to_str().unwrap(), "lutIntensity": 100
        }))
        .unwrap();
        let key = || super::thumbnail_hash_for_engine(path, &recipe, 1).unwrap();
        // The key probes no file: the same with the LUT there or not.
        let missing = key();
        std::fs::write(&lut, b"LUT_3D_SIZE 2\n").unwrap();
        assert_eq!(missing, key());
        // Tiles made before 2026-11-22 (black without their LUT) are made
        // again; an offline original still finds its older tile.
        let before = super::thumbnail_hash_before_refreshes(path, &recipe).unwrap();
        assert_ne!(before, key());
        let plain = b"{\"exposure\":1}";
        assert_eq!(
            super::thumbnail_hash_for_engine(path, plain, 1),
            super::thumbnail_hash_before_refreshes(path, plain)
        );

        let cache = temp.path().join("thumbnails");
        std::fs::create_dir(&cache).unwrap();
        let mut entry = super::ThumbnailCacheEntry {
            developed_hash: key(),
            embedded_hash: None,
            small: cache.join("unused"),
            medium: cache.join("unused"),
            rating: None,
            is_edited: None,
            lut: Some(lut.to_str().unwrap().to_string()),
            lut_missing: true,
        };
        let image = image::DynamicImage::new_rgb8(8, 8);
        let settings = crate::AppSettings::default();
        entry.save(path, &image, false, &settings).unwrap();
        // Shown now, but not found by the next lookup, which develops again.
        assert!(entry.small.is_file());
        assert!(super::thumbnail_files_exist(
            &cache,
            &super::lut_missing_hash(&key())
        ));
        assert!(!super::thumbnail_files_exist(&cache, &key()));
        entry.lut_missing = false;
        entry.save(path, &image, false, &settings).unwrap();
        assert!(super::thumbnail_files_exist(&cache, &key()));
    }

    #[test]
    fn embedded_pixels_survive_an_engine_upgrade_but_decoded_pixels_do_not() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.ARW");
        std::fs::write(&source, b"source unchanged").unwrap();
        let path = source.to_str().unwrap();
        let camera = super::embedded_thumbnail_hash(path).unwrap();
        let old = super::thumbnail_hash_for_engine(path, b"null", 1).unwrap();
        let new = super::thumbnail_hash_for_engine(path, b"null", 2).unwrap();
        assert_ne!(old, new);
        assert_ne!(camera, old);
        assert_ne!(camera, new);
        let cache = temp.path().join("thumbnails");
        std::fs::create_dir(&cache).unwrap();
        let mut entry = super::ThumbnailCacheEntry {
            developed_hash: old,
            embedded_hash: Some(camera.clone()),
            small: cache.join("unused"),
            medium: cache.join("unused"),
            rating: None,
            is_edited: None,
            lut: None,
            lut_missing: false,
        };
        let image = image::DynamicImage::new_rgb8(8, 8);
        let settings = crate::AppSettings::default();
        entry.save(path, &image, true, &settings).unwrap();
        let before = std::fs::read(&entry.medium).unwrap();
        assert_eq!(
            super::preferred_thumbnail_hash(&cache, &new, Some(&camera), true),
            camera
        );
        assert_eq!(
            super::preferred_thumbnail_hash(&cache, &new, Some(&camera), false),
            new
        );
        entry.developed_hash = new;
        // The same camera key still names the persisted pixels after upgrade.
        assert_eq!(super::embedded_thumbnail_hash(path).unwrap(), camera);
        assert_eq!(
            std::fs::read(cache.join(format!("{camera}_medium.jpg"))).unwrap(),
            before
        );
        // A decode fallback is saved under the new engine key, never camera's.
        entry.save(path, &image, false, &settings).unwrap();
        assert_ne!(entry.medium, cache.join(format!("{camera}_medium.jpg")));
        assert_eq!(
            std::fs::read(cache.join(format!("{camera}_medium.jpg"))).unwrap(),
            before
        );
        assert!(!super::embedded_thumbnail_allowed(
            path,
            br#"{"exposure":1}"#,
            &settings
        ));
        let mut decode = settings;
        decode.always_decode_raw_thumbnails = Some(true);
        assert!(!super::embedded_thumbnail_allowed(path, b"null", &decode));
    }

    #[test]
    fn unreadable_proxy_does_not_hide_a_cached_camera_pair() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("session.json"), br#"{"id":"stale-proxy"}"#).unwrap();
        let source = temp.path().join("Capture/photo.ARW");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"unchanged source").unwrap();
        let proxy = crate::preview_proxy::proxy_path(&source).unwrap();
        std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
        std::fs::write(&proxy, b"unreadable old proxy").unwrap();
        let path = source.to_str().unwrap();
        let settings = crate::AppSettings::default();
        assert!(super::embedded_thumbnail_allowed(path, b"null", &settings));
        let camera = super::embedded_thumbnail_hash(path).unwrap();
        let cache = temp.path().join("thumbnails");
        std::fs::create_dir(&cache).unwrap();
        let mut entry = super::ThumbnailCacheEntry {
            developed_hash: "a".repeat(64),
            embedded_hash: Some(camera.clone()),
            small: cache.join("unused"),
            medium: cache.join("unused"),
            rating: None,
            is_edited: None,
            lut: None,
            lut_missing: false,
        };
        entry
            .save(path, &image::DynamicImage::new_rgb8(8, 8), true, &settings)
            .unwrap();
        assert_eq!(
            super::preferred_thumbnail_hash(
                &cache,
                &entry.developed_hash,
                Some(&camera),
                super::embedded_thumbnail_allowed(path, b"null", &settings)
            ),
            camera
        );
        let image = image::DynamicImage::new_rgb8(8, 8);
        let variant = crate::image_loader::decode_variant(&serde_json::Value::Null, path);
        let mut key = crate::preview_proxy::current_key(&source, &settings, variant).unwrap();
        key.engine -= 1;
        std::fs::write(&proxy, crate::preview_proxy::encode(&image, 8, 8, key)).unwrap();
        assert!(
            super::embedded_thumbnail_allowed(path, b"null", &settings),
            "stale engine header must not hide camera cache"
        );
        let key = crate::preview_proxy::current_key(&source, &settings, variant).unwrap();
        let mut bytes = crate::preview_proxy::encode(&image, 8, 8, key);
        let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        bytes.truncate(12 + header_len);
        std::fs::write(&proxy, bytes).unwrap();
        assert!(!super::embedded_thumbnail_allowed(path, b"null", &settings));
        assert!(crate::preview_proxy::read(&source, &settings).is_none());
        // The generator falls back to camera pixels for this damaged payload.
        // Its normal-key backfill ensures the next lookup can hit as well.
        entry.save(path, &image, true, &settings).unwrap();
        let selected = super::preferred_thumbnail_hash(
            &cache,
            &entry.developed_hash,
            Some(&camera),
            super::embedded_thumbnail_allowed(path, b"null", &settings),
        );
        assert_eq!(selected, entry.developed_hash);
        assert!(cache.join(format!("{selected}_medium.jpg")).is_file());
    }

    #[test]
    fn saving_many_edits_keeps_one_pair_and_preserves_camera_and_other_copies() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("thumbnails");
        std::fs::create_dir(&cache).unwrap();
        let path = temp.path().join("photo.ARW").to_string_lossy().into_owned();
        let camera = "c".repeat(64);
        let copy = "d".repeat(64);
        for hash in [&camera, &copy] {
            for size in ["small", "medium"] {
                std::fs::write(cache.join(format!("{hash}_{size}.jpg")), b"keep").unwrap();
            }
        }
        super::supersede_thumbnail(&cache, &format!("{path}?vc=1"), &copy).unwrap();
        for edit in 0..20 {
            let hash = blake3::hash(&[edit]).to_hex().to_string();
            for size in ["small", "medium"] {
                std::fs::write(cache.join(format!("{hash}_{size}.jpg")), b"edit").unwrap();
            }
            super::supersede_thumbnail(&cache, &path, &hash).unwrap();
        }
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            8,
            "three pairs plus two per-copy indexes"
        );
        assert_eq!(
            std::fs::read(cache.join(format!("{camera}_small.jpg"))).unwrap(),
            b"keep"
        );
        assert_eq!(
            std::fs::read(cache.join(format!("{copy}_medium.jpg"))).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn superseded_cache_removal_respects_readers_and_refuses_escape() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("thumbnails");
        std::fs::create_dir(&cache).unwrap();
        let file = cache.join("cached.jpg");
        std::fs::write(&file, b"cache").unwrap();
        let pin = crate::storage_fs::pin_file(&file);
        assert!(!crate::storage_fs::remove_cache_entry(temp.path(), &cache, "cached.jpg").unwrap());
        drop(pin);
        assert!(crate::storage_fs::remove_cache_entry(temp.path(), &cache, "cached.jpg").unwrap());
        assert!(
            crate::storage_fs::remove_cache_entry(temp.path(), &cache, "../original.ARW").is_err()
        );
        #[cfg(unix)]
        {
            let original = temp.path().join("original.ARW");
            std::fs::write(&original, b"original").unwrap();
            std::os::unix::fs::symlink(&original, cache.join("link.jpg")).unwrap();
            assert!(
                crate::storage_fs::remove_cache_entry(temp.path(), &cache, "link.jpg").is_err()
            );
            assert_eq!(std::fs::read(original).unwrap(), b"original");
        }
    }

    #[tokio::test]
    async fn restoration_jobs_are_bounded_before_decoder_work() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let slot = crate::state::thumbnails::ThumbnailState::default().restoration_slot;
        let mut jobs = tokio::task::JoinSet::new();
        for index in 0..8 {
            let active = active.clone();
            let peak = peak.clone();
            let slot = slot.clone();
            jobs.spawn(async move {
                super::restoration_job(slot, move || {
                    let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(running, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                    index
                })
                .await
                .unwrap()
            });
        }
        let mut completed = Vec::new();
        while let Some(result) = jobs.join_next().await {
            completed.push(result.unwrap());
        }
        completed.sort();
        assert_eq!(completed, (0..8).collect::<Vec<_>>());
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "cache misses must not fan out unbounded decodes"
        );
    }

    #[test]
    fn cached_thumbnail_can_be_replaced_without_changing_its_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("small.jpg");
        super::cache_thumbnail(&path, b"first").unwrap();
        super::cache_thumbnail(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cached_thumbnail_never_follows_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.jpg");
        std::fs::write(&original, b"original").unwrap();
        let cache = directory.path().join("small.jpg");
        std::os::unix::fs::symlink(&original, &cache).unwrap();
        assert!(super::cache_thumbnail(&cache, b"thumbnail").is_err());
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
    }

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

    /// Without a GPU a new recipe's thumbnail is cut by the same rule as the
    /// GPU render's initial crop: never edited and after a Library Reset
    /// alike, at the frame the recipe asks for; an edit keeps its frame.
    #[test]
    fn the_cpu_thumbnail_of_a_new_recipe_is_cut_to_the_camera_crop() {
        use rawtonic_engine::source_facts::{SourceFacts, VERSION, register};
        let path = "/offline/cpu-thumbnail-crop.CR3";
        register(
            path,
            SourceFacts {
                version: VERSION,
                frames: Some(vec![0, 1]),
                camera_crop: Some(vec![0, 250, 4000, 2250]),
                ..Default::default()
            },
        );
        let sensor = || DynamicImage::new_rgb8(400, 300);
        let size = |recipe: serde_json::Value| {
            let cut = cut_to_camera_crop(sensor(), &recipe, path);
            (cut.width(), cut.height())
        };
        // 2250 of 2750 rows kept (the crop centred, its share of the height).
        assert_eq!(size(serde_json::Value::Null), (400, 245));
        assert_eq!(size(serde_json::json!({})), (400, 245));
        // A Reset keeps the frame it states: frame 1 is cut, frame 0 (whose
        // crop the facts do not know) and an edit are not.
        assert_eq!(
            size(serde_json::json!({"sourceFrameVersion": 1})),
            (400, 245)
        );
        assert_eq!(
            size(serde_json::json!({"sourceFrameVersion": 0})),
            (400, 300)
        );
        assert_eq!(size(serde_json::json!({"exposure": 1.0})), (400, 300));
    }

    /// A never-edited RAW's camera JPEG is cut to the camera's crop only
    /// where it shows the whole frame (a Fujifilm RAF's 3:2 JPEG of a 16:9
    /// photo), not where it is the crop already (Canon, EX2F).
    #[test]
    fn the_camera_jpeg_thumbnail_is_cut_to_the_crop_where_it_shows_the_whole_frame() {
        use rawtonic_engine::source_facts::{SourceFacts, VERSION, register};
        let path = "/offline/embedded-thumbnail-crop.RAF";
        register(
            path,
            SourceFacts {
                version: VERSION,
                frames: Some(vec![0; rawtonic_engine::source_frame::VERSION as usize + 1]),
                camera_crop: Some(vec![0, 312, 6000, 3375]),
                ..Default::default()
            },
        );
        let size = |width, height| {
            let cut = embedded_in_camera_crop(DynamicImage::new_rgb8(width, height), path, None);
            (cut.width(), cut.height())
        };
        assert_eq!(size(1920, 1280), (1920, 1080));
        assert_eq!(size(1920, 1080), (1920, 1080));
        // The crop turned upright: its shape either way up.
        assert_eq!(size(1080, 1920), (1080, 1920));
    }

    /// A never-edited RAW's camera JPEG keeps the camera's crop when the
    /// mapping it came from has no known file time (`MappedRaw::stamp`
    /// None): the crops are read from that mapping, only not cached.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn an_unstamped_mapping_keeps_the_camera_crop_of_the_thumbnail() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut cut = 0;
        for path in fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
        {
            let name = path.to_string_lossy().into_owned();
            if !is_raw_file(&name) {
                continue;
            }
            let Some((preview, stamped)) = embedded_raw_preview_and_file(&path) else {
                continue;
            };
            let unstamped = MappedRaw {
                file: stamped.file.clone(),
                stamp: None,
            };
            let size = |image: DynamicImage| (image.width(), image.height());
            let without = size(embedded_in_camera_crop(
                preview.clone(),
                &name,
                Some(&unstamped),
            ));
            let with = size(embedded_in_camera_crop(
                preview.clone(),
                &name,
                Some(&stamped),
            ));
            assert_eq!(without, with, "{name}");
            cut += usize::from(with != size(preview));
        }
        assert!(cut > 0, "no fixture whose camera JPEG is cut to its crop");
    }

    #[test]
    fn a_recipe_uses_the_preview_proxy_only_when_made_for_its_decode_variant() {
        let directory = tempfile::tempdir().unwrap();
        let photo = directory.path().join("A.RAF");
        fs::write(&photo, b"proxy fixture").unwrap();
        // The photograph (master) on the cross-bilateral filter, its
        // virtual copy on the multi-scale one.
        fs::write(
            directory.path().join("A.RAF.rrdata"),
            br#"{"version":1,"rating":0,"adjustments":{"colorNoiseVersion":1}}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("A.RAF.c0ffee.rrdata"),
            br#"{"version":1,"rating":0,"adjustments":{"colorNoiseVersion":9}}"#,
        )
        .unwrap();
        let copy = format!("{}?vc=c0ffee", photo.to_string_lossy());
        let (source, _) = parse_virtual_path(&copy);
        // The proxy (the master's) fits a snapshot of the master's filter,
        // never one of the copy's.
        let stands_for = |source: &Path, recipe: &serde_json::Value| {
            let wanted = crate::image_loader::decode_variant(recipe, &source.to_string_lossy());
            crate::preview_proxy::may_stand_for_variant(source, wanted)
        };
        assert!(stands_for(
            &source,
            &serde_json::json!({"colorNoiseVersion": 1})
        ));
        assert!(!stands_for(
            &source,
            &serde_json::json!({"colorNoiseVersion": 9})
        ));
        // Nor one of another dead-photosite test: a render version 2 copy
        // of a version 3 master and the other way round.
        assert!(!stands_for(
            &source,
            &serde_json::json!({"colorNoiseVersion": 1, "renderVersion": 3})
        ));
        fs::write(
            directory.path().join("A.RAF.rrdata"),
            br#"{"version":1,"rating":0,"adjustments":{"colorNoiseVersion":9,"renderVersion":3}}"#,
        )
        .unwrap();
        assert!(stands_for(
            &source,
            &serde_json::json!({"colorNoiseVersion": 9, "renderVersion": 3})
        ));
        assert!(!stands_for(
            &source,
            &serde_json::json!({"colorNoiseVersion": 9, "renderVersion": 2})
        ));
        // A never-edited photograph's proxies stand for itself.
        fs::remove_file(directory.path().join("A.RAF.rrdata")).unwrap();
        assert!(stands_for(&source, &serde_json::Value::Null));
    }

    /// Thumbnails never develop an edit (ADR 0020) on offline previews of
    /// another frame of the RAW.
    #[test]
    fn offline_thumbnails_use_previews_of_their_own_frame_only() {
        use crate::preview_proxy::frame_fixture::{frame_one_edit, offline_cr3};
        let settings = crate::app_settings::AppSettings::default();
        for (frame, shown) in [(0, false), (1, true)] {
            let directory = tempfile::tempdir().unwrap();
            let photo = offline_cr3(directory.path(), frame_one_edit(), frame);
            assert_eq!(
                stored_proxy(&photo, &frame_one_edit(), &settings).is_some(),
                shown
            );
        }
    }

    #[test]
    fn frame_copy_does_not_keep_the_masters_stale_proxy_key() {
        use crate::preview_proxy::frame_fixture::{frame_one_edit, offline_cr3};
        let directory = tempfile::tempdir().unwrap();
        let master = frame_one_edit();
        let photo = offline_cr3(directory.path(), master.clone(), 1);
        let mut copy = master.clone();
        copy["sourceFrameVersion"] = 0.into();
        fs::rename(directory.path().join("elsewhere.CR3"), &photo).unwrap();
        crate::preview_proxy::clear_display_copy_marker_for_test(&photo);
        assert!(display_copy_clamps(&photo, &master));
        assert!(!display_copy_clamps(&photo, &copy));
        let settings = AppSettings::default();
        assert!(stored_proxy(&photo, &master, &settings).is_some());
        assert!(stored_proxy(&photo, &copy, &settings).is_none());

        fs::rename(&photo, directory.path().join("elsewhere.CR3")).unwrap();
        for size in ["small", "medium"] {
            fs::write(directory.path().join(format!("old_{size}.jpg")), b"tile").unwrap();
        }
        assert_eq!(
            keep_tile_of_unavailable_original(
                directory.path(),
                &photo,
                &copy,
                "new".into(),
                || { vec!["old".into()] }
            ),
            "old"
        );
    }

    #[test]
    fn frame_copy_uses_its_own_preview_when_the_master_changed_frames_offline() {
        use crate::preview_proxy::frame_fixture::{frame_one_edit, offline_cr3};
        let directory = tempfile::tempdir().unwrap();
        let master = frame_one_edit();
        let photo = offline_cr3(directory.path(), master.clone(), 0);
        let mut copy = master.clone();
        copy["sourceFrameVersion"] = 0.into();
        let settings = AppSettings::default();
        assert!(stored_proxy(&photo, &master, &settings).is_none());
        assert!(stored_proxy(&photo, &copy, &settings).is_some());
    }

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
    fn a_moved_session_keeps_its_thumbnails_and_another_session_does_not_share_them() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("Job");
        fs::create_dir_all(session.join("Capture")).unwrap();
        fs::write(session.join("session.json"), br#"{"id":"one"}"#).unwrap();
        fs::write(session.join("Capture/A.RAF"), b"thumbnail-cache-fixture").unwrap();
        let key = |session: &Path, suffix: &str| {
            let photo = format!("{}{suffix}", session.join("Capture/A.RAF").display());
            compute_thumbnail_cache_hash(&photo, b"null").unwrap()
        };
        let before = [key(&session, ""), key(&session, "?vc=1")];
        assert_ne!(
            before[0], before[1],
            "a virtual copy has thumbnails of its own"
        );

        // The whole session moved (a rename keeps the file's time stamp).
        let moved = directory.path().join("Archive/Job");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::rename(&session, &moved).unwrap();
        assert_eq!([key(&moved, ""), key(&moved, "?vc=1")], before);

        // Another session (another id) at the same place does not take them.
        fs::write(moved.join("session.json"), br#"{"id":"two"}"#).unwrap();
        assert_ne!(key(&moved, ""), before[0]);

        // Outside a library a photograph is still named by its path.
        let loose = directory.path().join("A.RAF");
        assert_eq!(
            cache_label(&loose.to_string_lossy(), &loose),
            loose.to_string_lossy()
        );
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
            hasher.update(b"\0display-copy-keeps-beyond-srgb-v1\0");
            hasher.finalize().to_hex().to_string()
        };
        for adjustments in [
            br#"{"lensCorrectionMode":"manual","exposure":1}"#.as_slice(),
            br#"{"lensCorrectionMode":"manual","contrast":0,"shadows":0,"whites":0,"blacks":0,"highlights":-40}"#,
        ] {
            assert_eq!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments),
                "thumbnails without the tone sliders stay cached"
            );
        }
        // Never-edited RAWs (no lens mode) render with the current defaults
        // (lens profile, render version): made again when those change.
        for adjustments in [b"null".as_slice(), br#"{"exposure":1}"#] {
            assert_ne!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments)
            );
        }
        for adjustments in [
            br#"{"lensCorrectionMode":"manual","contrast":20}"#.as_slice(),
            br#"{"lensCorrectionMode":"manual","shadows":-30}"#,
            br#"{"lensCorrectionMode":"manual","whites":-100}"#,
            br#"{"lensCorrectionMode":"manual","blacks":5}"#,
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

    /// A new RAW recipe's thumbnail (none, or a Reset's) is made again since
    /// it renders with the new edit's tool responses; a saved edit's stays.
    #[test]
    fn raw_thumbnail_cache_renders_new_recipes_again() {
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
        // The key before 2026-11-05 of a recipe without a lens mode and tone sliders.
        let previous_hash = |adjustments: &[u8]| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(path_str.as_bytes());
            hasher.update(&modified.to_le_bytes());
            hasher.update(adjustments);
            hasher.update(b"\0embedded-raw-orientation-v1");
            hasher.update(
                format!("\0raw-engine-{}", crate::preview_proxy::ENGINE_VERSION).as_bytes(),
            );
            hasher.update(b"\0display-copy-keeps-beyond-srgb-v1\0");
            hasher.update(
                format!(
                    "\0never-edited-defaults-{}-{}",
                    rawtonic_engine::source_facts::VERSION,
                    rawtonic_engine::image_processing::RENDER_VERSION
                )
                .as_bytes(),
            );
            hasher.finalize().to_hex().to_string()
        };
        for adjustments in [b"null".as_slice(), b"{}", br#"{"sourceFrameVersion":3}"#] {
            assert_ne!(
                compute_thumbnail_cache_hash(path_str, adjustments).unwrap(),
                previous_hash(adjustments)
            );
        }
        let saved = br#"{"exposure":1}"#.as_slice();
        assert_eq!(
            compute_thumbnail_cache_hash(path_str, saved).unwrap(),
            previous_hash(saved)
        );
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

    fn names(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn a_cancelled_generation_stops_at_the_next_photo_and_a_new_one_runs() {
        use std::sync::atomic::AtomicBool;
        let cancelled = AtomicBool::new(false);
        let mut queue = ThumbnailQueue::default();
        queue.replace(names(&["a", "b", "c"]));
        let Some(ThumbnailJob::Lookup { paths, generation }) =
            take_next_thumbnail_job(&mut queue, ThumbnailWork::Lookup, &cancelled)
        else {
            panic!("a new viewport is looked up first");
        };
        assert!(queue.queue_misses(generation, paths));
        assert_eq!(
            take_next_thumbnail_job(&mut queue, ThumbnailWork::Decode, &cancelled),
            Some(ThumbnailJob::Decode("a".into()))
        );

        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(
            take_next_thumbnail_job(&mut queue, ThumbnailWork::Decode, &cancelled),
            None
        );
        assert!(
            queue.queued() == 0,
            "the rest of the cancelled queue is dropped"
        );

        // update_thumbnail_queue resets the flag when a new generation starts.
        cancelled.store(false, Ordering::SeqCst);
        queue.replace(names(&["d"]));
        assert!(matches!(
            take_next_thumbnail_job(&mut queue, ThumbnailWork::Lookup, &cancelled),
            Some(ThumbnailJob::Lookup { paths, .. }) if paths == names(&["d"])
        ));
    }

    #[test]
    fn a_viewport_is_looked_up_at_once_and_only_its_misses_are_decoded_in_visible_order() {
        use ThumbnailWork::{Decode, Lookup};
        let mut queue = ThumbnailQueue::default();
        queue.replace(names(&["first", "second", "third", "fourth"]));
        assert_eq!(queue.queued(), 4);
        assert!(!queue.has(Decode), "decoders wait for the lookup's misses");
        let Some(ThumbnailJob::Lookup { paths, generation }) = queue.take(Lookup) else {
            panic!("the whole viewport is one lookup");
        };
        assert_eq!(paths, names(&["first", "second", "third", "fourth"]));
        assert_eq!(queue.queued(), 0);

        // "second" and "fourth" were cached.
        assert!(queue.queue_misses(generation, names(&["first", "third"])));
        assert!(!queue.has(Lookup));
        assert_eq!(
            queue.take(Decode),
            Some(ThumbnailJob::Decode("first".into()))
        );
        assert_eq!(
            queue.take(Decode),
            Some(ThumbnailJob::Decode("third".into()))
        );
        assert_eq!(queue.take(Decode), None);
    }

    #[test]
    fn a_new_viewport_is_looked_up_while_the_decoders_are_busy() {
        use ThumbnailWork::{Decode, Lookup};
        let mut queue = ThumbnailQueue::default();
        queue.replace(names(&["passed"]));
        let Some(ThumbnailJob::Lookup { paths, generation }) = queue.take(Lookup) else {
            panic!("lookup expected");
        };
        assert!(queue.queue_misses(generation, paths));
        // Every decoder now works on a row the scroll passed; the rows it
        // stopped at are looked up without waiting for them.
        assert_eq!(
            queue.take(Decode),
            Some(ThumbnailJob::Decode("passed".into()))
        );
        queue.replace(names(&["shown"]));
        assert!(queue.has(Lookup));
        assert!(!queue.has(Decode));
    }

    #[test]
    fn a_lookup_overtaken_by_a_newer_viewport_queues_no_decodes() {
        use ThumbnailWork::{Decode, Lookup};
        let mut queue = ThumbnailQueue::default();
        queue.replace(names(&["passed-1", "passed-2"]));
        let Some(ThumbnailJob::Lookup { generation, .. }) = queue.take(Lookup) else {
            panic!("lookup expected");
        };
        // The user scrolled on while the cache was being looked up.
        queue.replace(names(&["shown"]));
        assert!(!queue.queue_misses(generation, names(&["passed-1", "passed-2"])));
        assert!(matches!(
            queue.take(Lookup),
            Some(ThumbnailJob::Lookup { paths, .. }) if paths == names(&["shown"])
        ));
        assert_eq!(
            queue.take(Decode),
            None,
            "the passed rows are never decoded"
        );

        // A replacement also drops decodes still waiting from the last one.
        queue.replace(names(&["a"]));
        let Some(ThumbnailJob::Lookup { paths, generation }) = queue.take(Lookup) else {
            panic!("lookup expected");
        };
        assert!(queue.queue_misses(generation, paths));
        queue.replace(names(&["b"]));
        assert_eq!(queue.take(Decode), None);
        assert!(queue.has(Lookup));
    }
}

#[cfg(test)]
mod face_retouch_thumbnail_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose};
    use serde_json::json;
    use std::io::Cursor;

    /// A 200 × 160 skin image with one dark spot at (60, 70), and a recipe
    /// that heals it (the face's 100 px crop covers the photo, found at
    /// full resolution).
    fn skin_and_recipe() -> (DynamicImage, serde_json::Value) {
        let image = image::Rgb32FImage::from_fn(200, 160, |x, y| {
            let t = (((x * 7 + y * 13) % 5) as f32 - 2.0) * 0.004;
            let d = ((x as f32 - 60.0).powi(2) + (y as f32 - 70.0).powi(2)).sqrt();
            let dark = if d < 5.0 { 0.07 } else { 0.0 };
            image::Rgb([0.24 + t - dark, 0.18 + t - dark, 0.15 + t - dark])
        });
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(image::GrayImage::from_pixel(100, 100, image::Luma([255])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let skin = format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(png.into_inner())
        );
        let recipe = json!({"faceRetouch": {
            "settings": {"blemishes": 100},
            "faces": [{
                "crop": [2.0, 0.0, 0.0, 0.0, 1.6, 0.0], "size": 100, "eyeDistance": 80.0,
                "skin": skin,
                "blemishes": [{"id": 1, "x": 60.0, "y": 70.0, "r": 5.0, "score": 0.4, "neck": false}],
                "kept": [],
            }],
        }});
        (DynamicImage::ImageRgb32F(image), recipe)
    }

    fn luma_at(image: &DynamicImage, x: u32, y: u32) -> f32 {
        image.to_rgb32f().get_pixel(x, y)[0]
    }

    #[test]
    fn a_reduced_proxy_places_the_face_retouch_where_the_full_render_does() {
        let (full, recipe) = skin_and_recipe();
        let on_full = retouched_base(&full, &recipe, 1.0).unwrap();
        assert!(
            luma_at(&on_full, 60, 70) > 0.2,
            "the full render heals the spot"
        );

        let proxy = full.resize_exact(100, 80, image::imageops::FilterType::Triangle);
        let spot = luma_at(&proxy, 30, 35);
        assert!(spot < 0.2, "the proxy shows the spot ({spot})");
        let on_proxy = retouched_base(&proxy, &recipe, 0.5).unwrap();
        let healed = luma_at(&on_proxy, 30, 35);
        assert!(healed > 0.2, "the proxy heals the same spot ({healed})");
        // Nothing is retouched where the full render leaves the skin alone.
        for (x, y) in [(10, 10), (80, 60), (40, 70)] {
            assert!((luma_at(&on_proxy, x, y) - luma_at(&proxy, x, y)).abs() < 0.03);
        }
    }

    #[test]
    fn a_retouch_whose_frame_is_stale_is_keyed_apart_from_the_unretouched_render() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.jpg");
        std::fs::write(&source, b"source unchanged").unwrap();
        let path = source.to_str().unwrap();
        let modified = source_mtime(&source).unwrap();
        // The key a thumbnail was cached under before such a retouch was
        // placed through its stored frame (it was skipped).
        let skipped = |bytes: &[u8]| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(cache_label(path, &source).as_bytes());
            hasher.update(&modified.to_le_bytes());
            hasher.update(bytes);
            hasher.finalize().to_hex().to_string()
        };
        let (_, mut recipe) = skin_and_recipe();
        recipe["faceRetouch"]["frame"] = json!(rawtonic_engine::face_retouch::frame_key(&recipe));
        let current = serde_json::to_vec(&recipe).unwrap();
        assert_eq!(
            compute_thumbnail_cache_hash(path, &current).unwrap(),
            skipped(&current),
            "faces found in the photo's geometry keep their thumbnails"
        );
        recipe["flipHorizontal"] = json!(true);
        let stale = serde_json::to_vec(&recipe).unwrap();
        assert_ne!(
            compute_thumbnail_cache_hash(path, &stale).unwrap(),
            skipped(&stale),
            "a thumbnail cached while the retouch was skipped is made again"
        );
    }

    #[test]
    fn never_edited_orf_tiles_are_made_again_once_with_the_old_tile_kept_offline() {
        let temp = tempfile::tempdir().unwrap();
        let orf = temp.path().join("P1010001.ORF");
        std::fs::write(&orf, b"source unchanged").unwrap();
        let path = orf.to_str().unwrap();
        let before = thumbnail_hash_before_orf_preview(path, b"null").unwrap();
        assert_ne!(compute_thumbnail_cache_hash(path, b"null").unwrap(), before);
        assert_ne!(compute_thumbnail_cache_hash(path, b"").unwrap(), before);
        assert!(older_tile_keys(path, b"null").contains(&before));
        // Offline with only the earlier tile cached, that tile is shown;
        // online, the new key (made again from the camera's preview).
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        for size in ["small", "medium"] {
            std::fs::write(cache.join(format!("{before}_{size}.jpg")), b"tile").unwrap();
        }
        let new = compute_thumbnail_cache_hash(path, b"null").unwrap();
        let gone = temp.path().join("offline/P1010001.ORF");
        assert_eq!(
            orf_tile_while_offline(&cache, &gone, new.clone(), || Some(before.clone())),
            before
        );
        assert_eq!(
            orf_tile_while_offline(&cache, &orf, new.clone(), || Some(before.clone())),
            new
        );
        // An edited ORF and other RAWs keep their keys.
        let edited = br#"{"exposure":0.5}"#;
        assert_eq!(
            compute_thumbnail_cache_hash(path, edited),
            thumbnail_hash_before_orf_preview(path, edited)
        );
        let cr2 = temp.path().join("IMG_0001.CR2");
        std::fs::write(&cr2, b"source unchanged").unwrap();
        let cr2 = cr2.to_str().unwrap();
        assert_eq!(
            compute_thumbnail_cache_hash(cr2, b"null"),
            thumbnail_hash_before_orf_preview(cr2, b"null")
        );
    }

    #[test]
    fn an_offline_orf_lookup_finds_both_legacy_tiles_even_with_a_matching_proxy() {
        let temp = tempfile::tempdir().unwrap();
        let lib = rawtonic_catalog::Library::create(
            temp.path(),
            "Olympus",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        let source = lib.directory.join("Capture/photo.ORF");
        std::fs::write(&source, b"ORF fixture").unwrap();
        std::fs::write(
            lib.directory.join("Capture/photo.ORF.rrdata"),
            br#"{"version":1,"rating":0,"adjustments":null}"#,
        )
        .unwrap();
        lib.add_files(std::slice::from_ref(&source)).unwrap();
        let path = source.to_str().unwrap();
        let cache = temp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let proxy = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_pixel(
            8,
            8,
            image::Rgb([0.2, 0.3, 0.4]),
        ));
        crate::preview_proxy::write_for_test(&source, &proxy, 8);
        let legacy = thumbnail_hash_before_orf_preview(path, b"null").unwrap();
        let small = cache.join(format!("{legacy}_small.jpg"));
        let medium = cache.join(format!("{legacy}_medium.jpg"));
        for tile in [&small, &medium] {
            std::fs::write(tile, b"cached tile").unwrap();
        }
        let settings = AppSettings::default();
        let lookup = || {
            ThumbnailCacheEntry::lookup(path, &cache, &settings, |_, _| {
                panic!("fixture has no cloud sidecar")
            })
            .unwrap()
            .cached()
        };
        assert!(lookup().is_none(), "online, renew the legacy tiles");

        std::fs::rename(&source, temp.path().join("disconnected.ORF")).unwrap();
        assert_eq!(
            crate::preview_proxy::stands_for_by_recipe(&source, &serde_json::Value::Null),
            Some(true)
        );
        let (found_small, found_medium, _, _) = lookup().expect("offline, keep the legacy tiles");
        assert_eq!(Path::new(&found_small), small);
        assert_eq!(Path::new(&found_medium), medium);
    }

    #[test]
    fn retouched_raw_tiles_on_a_reduced_base_are_made_again() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("session.json"),
            br#"{"id":"retouch-proxy"}"#,
        )
        .unwrap();
        let source = temp.path().join("Capture/photo.RAF");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"source unchanged").unwrap();
        let path = source.to_str().unwrap();
        let (_, mut recipe) = skin_and_recipe();
        recipe["lensCorrectionMode"] = json!("manual");
        recipe["faceRetouch"]["frame"] = json!(rawtonic_engine::face_retouch::frame_key(&recipe));
        let bytes = serde_json::to_vec(&recipe).unwrap();
        // A fast decode is a reduced base too: the tile is made again.
        assert_ne!(
            compute_thumbnail_cache_hash(path, &bytes),
            thumbnail_hash_before_refreshes(path, &bytes)
        );
        let proxy = crate::preview_proxy::proxy_path(&source).expect("proxy location");
        std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
        std::fs::write(&proxy, b"proxy").unwrap();
        assert_ne!(
            compute_thumbnail_cache_hash(path, &bytes),
            thumbnail_hash_before_refreshes(path, &bytes)
        );
        // A recipe without face retouch is made again only for its display
        // copy (`raw_tiles_that_clamped_colours_are_made_again…`).
        let plain = serde_json::to_vec(&json!({"lensCorrectionMode": "manual"})).unwrap();
        assert_ne!(
            compute_thumbnail_cache_hash(path, &plain),
            thumbnail_hash_before_refreshes(path, &plain)
        );
    }

    #[test]
    fn raw_tiles_that_clamped_colours_are_made_again_unless_their_proxy_still_clamps() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("session.json"),
            br#"{"id":"display-copy"}"#,
        )
        .unwrap();
        let source = temp.path().join("Capture/photo.RAF");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"source unchanged").unwrap();
        let path = source.to_str().unwrap();
        let bytes = serde_json::to_vec(&json!({"lensCorrectionMode": "manual"})).unwrap();
        // Developed from the RAW: a new key, so the tile is made again.
        assert_ne!(
            compute_thumbnail_cache_hash(path, &bytes),
            thumbnail_hash_before_refreshes(path, &bytes)
        );
        // A JPEG was never reduced with a clamp that mattered.
        let jpeg = temp.path().join("Capture/photo.jpg");
        std::fs::write(&jpeg, b"jpeg").unwrap();
        let jpeg_path = jpeg.to_str().unwrap();
        assert_eq!(
            compute_thumbnail_cache_hash(jpeg_path, &bytes),
            thumbnail_hash_before_refreshes(jpeg_path, &bytes)
        );
        // Developed from a proxy that clamps: the tile shows that, so it
        // keeps its key until the proxy is made again.
        let image = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_pixel(
            80,
            60,
            image::Rgb([0.2, 0.3, 0.4]),
        ));
        crate::preview_proxy::write_for_test(&source, &image, 40);
        assert_ne!(
            compute_thumbnail_cache_hash(path, &bytes),
            thumbnail_hash_before_refreshes(path, &bytes),
            "a renewed proxy holds the colours: its tile is made again"
        );
        crate::preview_proxy::clear_display_copy_marker_for_test(&source);
        let new_edit = b"null".as_slice();
        assert_ne!(
            compute_thumbnail_cache_hash(path, new_edit),
            thumbnail_hash_before_refreshes(path, new_edit),
            "without frame facts the stale frame-0 proxy cannot prove a match for a new RAF edit"
        );
        crate::preview_proxy::record_standard_frames_for_test(
            &source,
            vec![0; rawtonic_engine::source_frame::VERSION as usize + 1],
        );
        // These frame facts say every recipe version develops frame 0.
        assert_eq!(
            compute_thumbnail_cache_hash(path, new_edit),
            thumbnail_hash_before_refreshes(path, new_edit)
        );
        // The proxy is the master's: it holds the key of every recipe of the
        // same decode variant until it is renewed.
        std::fs::write(
            temp.path().join("Capture/photo.RAF.rrdata"),
            br#"{"version":1,"rating":0,"adjustments":{"lensCorrectionMode":"manual"}}"#,
        )
        .unwrap();
        assert_eq!(
            compute_thumbnail_cache_hash(path, &bytes),
            thumbnail_hash_before_refreshes(path, &bytes),
            "a stale proxy holds the key of every recipe of its variant until it is renewed"
        );
    }

    /// A session with a master (cross-bilateral colour noise filter) and a
    /// virtual copy (multi-scale), the master's proxy written and then stale.
    fn session_with_a_copy_of_another_variant(temp: &Path) -> (PathBuf, String, Vec<u8>, Vec<u8>) {
        std::fs::write(temp.join("session.json"), br#"{"id":"copy-variant"}"#).unwrap();
        let source = temp.join("Capture/photo.RAF");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"source unchanged").unwrap();
        let master = br#"{"colorNoiseVersion":1,"lensCorrectionMode":"manual"}"#;
        let copy = br#"{"colorNoiseVersion":9,"lensCorrectionMode":"manual"}"#;
        std::fs::write(
            temp.join("Capture/photo.RAF.rrdata"),
            format!(
                r#"{{"version":1,"rating":0,"adjustments":{}}}"#,
                String::from_utf8_lossy(master)
            ),
        )
        .unwrap();
        let image = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_pixel(
            80,
            60,
            image::Rgb([0.2, 0.3, 0.4]),
        ));
        crate::preview_proxy::write_for_test(&source, &image, 40);
        crate::preview_proxy::clear_display_copy_marker_for_test(&source);
        let copy_path = format!("{}?vc=c0ffee", source.to_string_lossy());
        (source, copy_path, master.to_vec(), copy.to_vec())
    }

    #[test]
    fn a_copy_of_another_decode_variant_is_made_again_while_the_masters_proxy_clamps() {
        let temp = tempfile::tempdir().unwrap();
        let (source, copy_path, master, copy) = session_with_a_copy_of_another_variant(temp.path());
        let master_path = source.to_str().unwrap();
        // The master's tile is developed from its stale proxy: old key.
        assert_eq!(
            compute_thumbnail_cache_hash(master_path, &master),
            thumbnail_hash_before_refreshes(master_path, &master)
        );
        // The copy's decode differs, so it renders from the RAW (never from
        // that proxy): its tile is made again now, not once the master's
        // proxy is renewed.
        assert_ne!(
            compute_thumbnail_cache_hash(&copy_path, &copy),
            thumbnail_hash_before_refreshes(&copy_path, &copy)
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_offline_copy_of_another_variant_keeps_its_old_tile_beside_the_masters_proxy() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (source, _, master, copy) = session_with_a_copy_of_another_variant(temp.path());
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let (old, new) = ("a".repeat(64), "b".repeat(64));
        for size in ["small", "medium"] {
            std::fs::write(cache.join(format!("{old}_{size}.jpg")), b"tile").unwrap();
        }
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&source).is_ok() {
            return;
        }
        let key = |recipe: &[u8]| {
            keep_tile_of_unavailable_original(
                &cache,
                &source,
                &serde_json::from_slice(recipe).unwrap(),
                new.clone(),
                || vec![old.clone()],
            )
        };
        // The master's proxy stands for the master: its tile is renewed.
        assert_eq!(key(&master), new);
        // It cannot show the copy, and the RAW cannot be read: the old tile
        // stays instead of a blank.
        assert_eq!(key(&copy), old);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// A never-edited photo with camera defaults set cannot get them from a
    /// proxy while its original is offline (CID1 review round 3): its old
    /// tile stays; an edited one, or without camera defaults, renews.
    #[test]
    fn a_proxy_does_not_replace_a_tile_with_camera_defaults_of_an_offline_original() {
        use rawtonic_engine::develop_defaults::{CameraDefaults, DevelopDefaults};
        let mut defaults = DevelopDefaults::default();
        let never_edited = serde_json::Value::Null;
        let edited = serde_json::json!({ "exposure": 0.5 });
        assert!(!proxy_lacks_camera_defaults(&defaults, &never_edited));
        defaults.cameras.push(CameraDefaults {
            make: "Canon".into(),
            model: "EOS R5".into(),
            serial: None,
            name: None,
            settings: Default::default(),
            inherit: false,
        });
        assert!(proxy_lacks_camera_defaults(&defaults, &never_edited));
        assert!(!proxy_lacks_camera_defaults(&defaults, &edited));
    }

    /// An evicted cloud original is not developed for a tile, so a preview
    /// that stands for its recipe does not renew the key: the old tile
    /// stays instead of a blank (PXV1 review).
    #[test]
    fn an_evicted_cloud_original_keeps_its_tile_though_its_preview_stands_for_it() {
        let temp = tempfile::tempdir().unwrap();
        let (source, _, master, _) = session_with_a_copy_of_another_variant(temp.path());
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let (old, new) = ("a".repeat(64), "b".repeat(64));
        for size in ["small", "medium"] {
            std::fs::write(cache.join(format!("{old}_{size}.jpg")), b"tile").unwrap();
        }
        crate::file_management::simulate_cloud_placeholder(&source);
        let master = serde_json::from_slice(&master).unwrap();
        assert_eq!(
            crate::preview_proxy::stands_for_by_recipe(&source, &master),
            Some(true)
        );
        let chosen =
            keep_tile_of_unavailable_original(&cache, &source, &master, new, || vec![old.clone()]);
        assert_eq!(chosen, old);
    }

    #[cfg(unix)]
    #[test]
    fn an_offline_copy_with_an_evicted_master_sidecar_finds_its_renewed_tile() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (source, copy_path, _, copy) = session_with_a_copy_of_another_variant(temp.path());
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        // Looked up with the master's sidecar evicted, as the grid does.
        crate::file_management::simulate_cloud_placeholder(
            &temp.path().join("Capture/photo.RAF.rrdata"),
        );
        let new = compute_thumbnail_cache_hash(&copy_path, &copy).unwrap();
        let renewed = thumbnail_hash_with_display_copy_renewed(&copy_path, &copy).unwrap();
        for hash in [&renewed] {
            for size in ["small", "medium"] {
                std::fs::write(cache.join(format!("{hash}_{size}.jpg")), b"tile").unwrap();
            }
        }
        crate::file_management::simulate_cloud_placeholder(
            &temp.path().join("Capture/photo.RAF.rrdata"),
        );
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        let offline = std::fs::File::open(&source).is_err();
        let chosen = keep_tile_of_unavailable_original(
            &cache,
            &source,
            &serde_json::from_slice(&copy).unwrap(),
            new,
            || older_tile_keys(&copy_path, &copy),
        );
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        if offline {
            // Whether the master's proxy fits is not known without the
            // download: a tile cached after the refresh is found; the
            // older one stays otherwise (tested above), never a blank.
            assert_eq!(chosen, renewed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_offline_copy_finds_the_tile_cached_while_its_masters_proxy_clamped() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (source, copy_path, _, _) = session_with_a_copy_of_another_variant(temp.path());
        // A recipe another refresh marker applies to (a LUT), beside the
        // display copy's: the tile cached before this fix has that marker
        // only.
        let copy = br#"{"colorNoiseVersion":9,"lensCorrectionMode":"manual","lutPath":"x.cube"}"#;
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let held = thumbnail_hash_with_display_copy_held(&copy_path, copy).unwrap();
        let new = compute_thumbnail_cache_hash(&copy_path, copy).unwrap();
        let before = thumbnail_hash_before_refreshes(&copy_path, copy).unwrap();
        assert!(held != new && held != before);
        for size in ["small", "medium"] {
            std::fs::write(cache.join(format!("{held}_{size}.jpg")), b"tile").unwrap();
        }
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        let offline = std::fs::File::open(&source).is_err();
        let chosen = keep_tile_of_unavailable_original(
            &cache,
            &source,
            &serde_json::from_slice(copy).unwrap(),
            new,
            || older_tile_keys(&copy_path, copy),
        );
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        if offline {
            assert_eq!(chosen, held);
        }
    }

    #[test]
    fn a_disconnected_original_keeps_the_tile_cached_before_the_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let (old, new) = ("a".repeat(64), "b".repeat(64));
        let put = |hash: &str| {
            for size in ["small", "medium"] {
                std::fs::write(cache.join(format!("{hash}_{size}.jpg")), b"tile").unwrap();
            }
        };
        let gone = temp.path().join("drive/photo.jpg");
        let present = temp.path().join("photo.jpg");
        std::fs::write(&present, b"photo").unwrap();
        let key = |source: &Path| {
            keep_tile_of_unavailable_original(
                &cache,
                source,
                &serde_json::Value::Null,
                new.clone(),
                || vec![old.clone()],
            )
        };
        assert_eq!(key(&gone), new, "no old tile: nothing to keep");
        put(&old);
        assert_eq!(key(&gone), old, "offline: the old tile stays");
        assert_eq!(key(&present), new, "online: made again");
        #[cfg(unix)]
        {
            // Present but not readable (a cloud placeholder, a locked file):
            // nothing to develop from either.
            use std::os::unix::fs::PermissionsExt;
            let locked = temp.path().join("locked.jpg");
            std::fs::write(&locked, b"photo").unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            if std::fs::File::open(&locked).is_err() {
                assert_eq!(
                    key(&locked),
                    old,
                    "an unreadable original keeps the old tile"
                );
            }
        }
        put(&new);
        assert_eq!(key(&gone), new, "the new tile wins once it exists");
    }
}
