//! Photo merges (HDR, panorama, focus stack). The merges
//! live in rawtonic-merge; this module runs them for the merge dialog and
//! writes the result as a linear DNG beside the reference photo.
use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use base64::Engine as _;
use base64::engine::general_purpose;
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_merge::dng::{DngMeta, encode_linear_dng};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

/// A merge waiting in memory for `save_merge`.
pub struct MergeOutcome {
    image: image::Rgb32FImage,
    reference: PathBuf,
    color: Option<rawtonic_rawdecode::raw_processing::ImageColor>,
    metadata: Option<rawler::decoders::RawMetadata>,
    /// Lens corrections already applied (panorama, focus stack).
    lens_applied: bool,
    /// File name suffix: HDR, Pano, Stack.
    suffix: &'static str,
    /// Panoramas: `image` is the stitched canvas; its covered pixels and the
    /// outline settings (boundary warp, crop) apply when it is shown or
    /// saved, so they change without stitching again.
    outline: Option<(Vec<bool>, rawtonic_merge::panorama::PanoramaConfig)>,
    /// The dialog's job id of the run that made it (`merge_photos`).
    job: u64,
}

/// The merge job the dialog waits for: the id of the last run started, 0
/// once the dialog closed. Results and events of other runs are stale: the
/// result is dropped, the dialog ignores their events.
static CURRENT_JOB: AtomicU64 = AtomicU64::new(0);
/// One merge computes at a time: each holds all its frames in memory, and
/// the merges cannot be interrupted, so a new run waits for an older one.
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn is_current(job: u64) -> bool {
    CURRENT_JOB.load(Ordering::SeqCst) == job
}

impl MergeOutcome {
    /// A preview (at most `max_dim` pixels) and the full output size.
    fn preview(&self, max_dim: u32) -> (image::RgbImage, (u32, u32)) {
        match &self.outline {
            Some((covered, config)) => {
                let (small, size) =
                    rawtonic_merge::panorama::finish_preview(&self.image, covered, config, max_dim);
                (rawtonic_merge::hdr::preview(&small, max_dim), size)
            }
            None => (
                rawtonic_merge::hdr::preview(&self.image, max_dim),
                self.image.dimensions(),
            ),
        }
    }

    /// The image to save.
    /// The image to save and, for panoramas, the pixels it covers.
    fn finished(&self) -> (std::borrow::Cow<'_, image::Rgb32FImage>, Option<Vec<bool>>) {
        match &self.outline {
            Some((covered, config)) => {
                let (image, covered) =
                    rawtonic_merge::panorama::finish(&self.image, covered, config);
                (std::borrow::Cow::Owned(image), Some(covered))
            }
            None => (std::borrow::Cow::Borrowed(&self.image), None),
        }
    }
}

fn base_options(app_handle: &tauri::AppHandle) -> BaseImageOptions {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    BaseImageOptions::from_preferences(
        settings.raw_highlight_compression,
        settings.linear_raw_mode.clone(),
        settings.raw_preprocessing_color_nr,
        settings.raw_preprocessing_sharpening,
        settings.apply_preprocessing_to_non_raws,
    )
}

fn png_data_url(image: &image::DynamicImage) -> Result<String, String> {
    let mut buf = Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode the preview: {e}"))?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(buf.get_ref())
    ))
}

/// A grey overlay (ghosts, depth) as a translucent PNG.
fn overlay_data_url(
    plane: &rawtonic_merge::planes::Plane,
    tint: [u8; 3],
) -> Result<String, String> {
    let mut img = image::RgbaImage::new(plane.w as u32, plane.h as u32);
    for (p, v) in img.pixels_mut().zip(&plane.data) {
        let a = (v.clamp(0.0, 1.0) * 200.0) as u8;
        *p = image::Rgba([tint[0], tint[1], tint[2], a]);
    }
    png_data_url(&image::DynamicImage::ImageRgba8(img))
}

fn depth_data_url(
    result: &rawtonic_merge::focus_stack::StackResult,
    frames: usize,
) -> Result<String, String> {
    let plane = rawtonic_merge::focus_stack::depth_preview_plane(result, frames);
    let mut img = image::RgbImage::new(plane.w as u32, plane.h as u32);
    for (p, v) in img.pixels_mut().zip(&plane.data) {
        let g = (v.clamp(0.0, 1.0) * 255.0) as u8;
        *p = image::Rgb([g, g, g]);
    }
    png_data_url(&image::DynamicImage::ImageRgb8(img))
}

/// Runs a merge (`kind` hdr, panorama or focusStack with its `options`) and
/// keeps the result for `save_merge`. Progress, the preview and errors
/// arrive as merge-progress, merge-complete and merge-error events, each
/// with the dialog's `job` id; a newer run or `discard_merge` makes this
/// one stale (its result is dropped).
#[tauri::command]
pub(crate) async fn merge_photos(
    paths: Vec<String>,
    kind: String,
    options: serde_json::Value,
    job: u64,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if paths.len() < 2 {
        return Err("Select at least two photos to merge.".to_string());
    }
    let sources: Vec<PathBuf> = paths.iter().map(|p| parse_virtual_path(p).0).collect();
    let slot = state.merge_result.clone();
    CURRENT_JOB.store(job, Ordering::SeqCst);
    // An older result is no longer wanted: free its memory now.
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let _running = match RUNNING.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let _ = app_handle.emit(
                "merge-progress",
                serde_json::json!({ "job": job, "message": "Waiting for the previous merge" }),
            );
            RUNNING.lock().await
        }
    };
    if !is_current(job) {
        // Superseded or closed while waiting.
        return Ok(());
    }
    let base = base_options(&app_handle);
    let task = tokio::task::spawn_blocking(move || {
        let result = run_merge(&sources, &kind, &options, &base, &slot, job, &app_handle);
        if let Err(e) = &result {
            let _ = app_handle.emit("merge-error", serde_json::json!({ "job": job, "error": e }));
        }
        result
    });
    match task.await {
        Ok(r) => r,
        Err(e) => Err(format!("Merge failed: {e}")),
    }
}

/// The merge dialog closed: drops `job`'s result (or, still computing, makes
/// it stale), unless a newer run took over.
#[tauri::command]
pub(crate) fn discard_merge(job: u64, state: tauri::State<'_, AppState>) {
    let mut slot = state.merge_result.lock().unwrap_or_else(|e| e.into_inner());
    if CURRENT_JOB
        .compare_exchange(job, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
        || slot.as_ref().is_some_and(|outcome| outcome.job == job)
    {
        *slot = None;
    }
}

fn run_merge(
    sources: &[PathBuf],
    kind: &str,
    options: &serde_json::Value,
    base: &BaseImageOptions,
    slot: &std::sync::Arc<std::sync::Mutex<Option<MergeOutcome>>>,
    job: u64,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    {
        let (sources, base) = (sources.to_vec(), base.clone());
        let progress = |msg: &str| {
            let _ = app_handle.emit(
                "merge-progress",
                serde_json::json!({ "job": job, "message": msg }),
            );
        };
        let opts = |v: &serde_json::Value| v.clone();
        let (outcome, overlay, report) = match kind {
            "hdr" => {
                let config: rawtonic_merge::hdr::HdrConfig =
                    serde_json::from_value(opts(options)).map_err(|e| e.to_string())?;
                let r = rawtonic_merge::hdr::merge_hdr(&sources, &base, &config, &progress)
                    .map_err(|e| e.to_string())?;
                let overlay = r
                    .ghost_mask
                    .as_ref()
                    .map(|g| overlay_data_url(g, [255, 64, 64]))
                    .transpose()?;
                let report = serde_json::json!({ "frames": r.frames, "reference": r.reference });
                (
                    MergeOutcome {
                        image: r.image,
                        reference: r.reference_path,
                        color: r.color,
                        metadata: r.metadata,
                        lens_applied: false,
                        suffix: "HDR",
                        outline: None,
                        job,
                    },
                    overlay,
                    report,
                )
            }
            "panorama" => {
                let config: rawtonic_merge::panorama::PanoramaConfig =
                    serde_json::from_value(opts(options)).map_err(|e| e.to_string())?;
                let r = rawtonic_merge::panorama::stitch(&sources, &base, &config, &progress)
                    .map_err(|e| e.to_string())?;
                let report = serde_json::json!({
                    "frames": r.frames,
                    "projection": r.projection,
                    "rmsPx": r.rms_px,
                    "fovDeg": r.fov_deg,
                });
                (
                    MergeOutcome {
                        image: r.image,
                        reference: r.reference_path,
                        color: r.color,
                        metadata: r.metadata,
                        lens_applied: true,
                        suffix: "Pano",
                        outline: Some((r.covered, config)),
                        job,
                    },
                    None,
                    report,
                )
            }
            "hdrPanorama" => {
                #[derive(serde::Deserialize)]
                struct Options {
                    hdr: rawtonic_merge::hdr::HdrConfig,
                    panorama: rawtonic_merge::panorama::PanoramaConfig,
                }
                let o: Options =
                    serde_json::from_value(opts(options)).map_err(|e| e.to_string())?;
                let r = rawtonic_merge::hdr_panorama::hdr_panorama(
                    &sources,
                    &base,
                    &o.hdr,
                    &o.panorama,
                    &progress,
                )
                .map_err(|e| e.to_string())?;
                let report = serde_json::json!({
                    "frames": r.frames,
                    "projection": r.projection,
                    "rmsPx": r.rms_px,
                    "fovDeg": r.fov_deg,
                });
                (
                    MergeOutcome {
                        image: r.image,
                        reference: r.reference_path,
                        color: r.color,
                        metadata: r.metadata,
                        lens_applied: true,
                        suffix: "HDR-Pano",
                        outline: Some((r.covered, o.panorama)),
                        job,
                    },
                    None,
                    report,
                )
            }
            "focusStack" => {
                let config: rawtonic_merge::focus_stack::FocusConfig =
                    serde_json::from_value(opts(options)).map_err(|e| e.to_string())?;
                let r = rawtonic_merge::focus_stack::stack(&sources, &base, &config, &progress)?;
                let overlay = Some(depth_data_url(&r.stack, sources.len())?);
                let report = serde_json::json!({ "reference": r.stack.reference });
                (
                    MergeOutcome {
                        image: r.image,
                        reference: r.reference_path,
                        color: r.color,
                        metadata: r.metadata,
                        lens_applied: true,
                        suffix: "Stack",
                        outline: None,
                        job,
                    },
                    overlay,
                    report,
                )
            }
            other => return Err(format!("Unknown merge {other}")),
        };
        progress("Creating preview");
        let (preview, size) = outcome.preview(1600);
        let base64 = png_data_url(&image::DynamicImage::ImageRgb8(preview))?;
        {
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            if !is_current(job) {
                // Nobody waits for this result any more.
                return Ok(());
            }
            *slot = Some(outcome);
        }
        let _ = app_handle.emit(
            "merge-complete",
            serde_json::json!({
                "job": job,
                "base64": base64,
                "overlay": overlay,
                "report": report,
                "width": size.0,
                "height": size.1,
            }),
        );
        Ok(())
    }
}

/// New outline settings (boundary warp, auto crop) for the panorama in
/// memory: returns the preview and output size without stitching again.
#[tauri::command]
pub(crate) async fn refinish_merge(
    options: serde_json::Value,
    job: u64,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let config: rawtonic_merge::panorama::PanoramaConfig =
        serde_json::from_value(options).map_err(|e| e.to_string())?;
    let slot = state.merge_result.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = guard
            .as_mut()
            .filter(|outcome| outcome.job == job)
            .ok_or_else(|| "There is no merge in memory.".to_string())?;
        let Some((_, current)) = outcome.outline.as_mut() else {
            return Err("This merge has no outline settings.".to_string());
        };
        if current.projection != config.projection {
            return Err("A new projection needs a new stitch.".to_string());
        }
        *current = config;
        let (preview, size) = outcome.preview(1600);
        Ok(serde_json::json!({
            "base64": png_data_url(&image::DynamicImage::ImageRgb8(preview))?,
            "width": size.0,
            "height": size.1,
        }))
    })
    .await
    .map_err(|e| format!("Refinish failed: {e}"))?
}

/// `<stem>-<suffix>.dng` beside `reference`, numbered when taken.
fn output_path(reference: &Path, suffix: &str, attempt: usize) -> Result<PathBuf, String> {
    let parent = reference
        .parent()
        .ok_or_else(|| "The photo has no folder".to_string())?;
    let stem = reference
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Merge");
    Ok(if attempt == 0 {
        parent.join(format!("{stem}-{suffix}.dng"))
    } else {
        parent.join(format!("{stem}-{suffix}-{}.dng", attempt + 1))
    })
}

/// Writes `job`'s merge in memory as a DNG (never over an existing file)
/// with its starting recipe; returns its path.
#[tauri::command]
pub(crate) async fn save_merge(
    auto_settings: Option<bool>,
    fill_edges: Option<bool>,
    job: u64,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let outcome = {
        let mut slot = state.merge_result.lock().unwrap_or_else(|e| e.into_inner());
        match slot.take() {
            Some(outcome) if outcome.job == job => outcome,
            other => {
                *slot = other;
                return Err("There is no merge to save.".to_string());
            }
        }
    };
    let slot = state.merge_result.clone();
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let (finished, covered) = outcome.finished();
        let fill_patches = match &covered {
            Some(covered) if fill_edges.unwrap_or(false) && covered.iter().any(|c| !c) => {
                crate::inpainting::fill_edge_patches(&finished, covered)
                    .map_err(|e| format!("Failed to fill the edges: {e}"))?
            }
            _ => Vec::new(),
        };
        let preview = rawtonic_merge::hdr::preview(&finished, 1024);
        let baseline = rawtonic_engine::baseline_exposure::dng_baseline_equivalent(
            &outcome.reference.to_string_lossy(),
        ) as f64;
        let bytes = encode_linear_dng(
            &finished,
            &DngMeta {
                color: outcome.color.as_ref(),
                metadata: outcome.metadata.as_ref(),
                keep_lens: !outcome.lens_applied,
                baseline_exposure: Some(baseline),
                preview: Some(&preview),
                software: "Rawtonic",
            },
        )
        .map_err(|e| format!("Failed to encode the DNG: {e}"))?;
        for attempt in 0..100 {
            let path = output_path(&outcome.reference, outcome.suffix, attempt)?;
            match crate::session_bridge::write_new_output(&path, &bytes) {
                Ok(()) => {
                    let mut adjustments = rawtonic_merge::initial_adjustments(
                        &outcome.reference,
                        outcome.lens_applied,
                    );
                    if auto_settings.unwrap_or(false) {
                        rawtonic_merge::apply_auto_settings(&mut adjustments, &finished, &path);
                    }
                    if !fill_patches.is_empty() {
                        adjustments["aiPatches"] = serde_json::Value::Array(fill_patches.clone());
                    }
                    let sidecar =
                        rawtonic_merge::sidecar_json(adjustments).map_err(|e| e.to_string())?;
                    let mut sidecar_path = path.clone().into_os_string();
                    sidecar_path.push(".rrdata");
                    crate::session_bridge::write_new_output(Path::new(&sidecar_path), &sidecar)
                        .map_err(|e| format!("Failed to save the recipe: {e}"))?;
                    return Ok(path.to_string_lossy().into_owned());
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    // Keep the merge for another attempt (unless a newer
                    // one took its place).
                    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.is_none() && is_current(outcome.job) {
                        *slot = Some(outcome);
                    }
                    return Err(format!("Failed to save the merge: {e}"));
                }
            }
        }
        Err("Too many merges with this name".to_string())
    })
    .await
    .map_err(|e| format!("Saving failed: {e}"))?
}

#[cfg(test)]
#[cfg(feature = "performance-profiling")]
mod memory {
    /// Footprint of an HDR merge and its DNG encoding as the app runs them
    /// (mimalloc): `RAWTONIC_MERGE_HDR` = comma-separated brackets. Prints
    /// the current and peak footprint after each step.
    #[test]
    #[ignore]
    fn hdr_merge_footprint() {
        let Some(list) = std::env::var_os("RAWTONIC_MERGE_HDR") else {
            return;
        };
        let paths: Vec<std::path::PathBuf> = list
            .to_string_lossy()
            .split(',')
            .map(std::path::PathBuf::from)
            .collect();
        let started = std::time::Instant::now();
        let report = |step: &str| {
            let (now, peak) = rawtonic_runtime::performance::footprint_bytes().unwrap_or((0, 0));
            println!(
                "{:6.2} s  {:5} MB  peak {:5} MB  {step}",
                started.elapsed().as_secs_f64(),
                now / 1_000_000,
                peak / 1_000_000
            );
        };
        let config = rawtonic_merge::hdr::HdrConfig {
            deghost: rawtonic_merge::hdr::Deghost::Medium,
            ..Default::default()
        };
        let r =
            rawtonic_merge::hdr::merge_hdr(&paths, &Default::default(), &config, &report).unwrap();
        report("merged");
        let preview = rawtonic_merge::hdr::preview(&r.image, 1024);
        let bytes = super::encode_linear_dng(
            &r.image,
            &super::DngMeta {
                color: r.color.as_ref(),
                metadata: r.metadata.as_ref(),
                keep_lens: true,
                baseline_exposure: Some(0.0),
                preview: Some(&preview),
                software: "Rawtonic",
            },
        )
        .unwrap();
        report(&format!("encoded {} MB", bytes.len() / 1_000_000));
        crate::register_exit_handler();
    }

    /// Footprint of a panorama stitch and finish as the app runs them:
    /// `RAWTONIC_MERGE_PANO` = comma-separated frames.
    #[test]
    #[ignore]
    fn pano_footprint() {
        let Some(list) = std::env::var_os("RAWTONIC_MERGE_PANO") else {
            return;
        };
        let paths: Vec<std::path::PathBuf> = list
            .to_string_lossy()
            .split(',')
            .map(std::path::PathBuf::from)
            .collect();
        let started = std::time::Instant::now();
        let report = |step: &str| {
            let (now, peak) = rawtonic_runtime::performance::footprint_bytes().unwrap_or((0, 0));
            println!(
                "{:6.2} s  {:5} MB  peak {:5} MB  {step}",
                started.elapsed().as_secs_f64(),
                now / 1_000_000,
                peak / 1_000_000
            );
        };
        let config = rawtonic_merge::panorama::PanoramaConfig::default();
        let r = rawtonic_merge::panorama::stitch(&paths, &Default::default(), &config, &report)
            .unwrap();
        report(&format!(
            "stitched {}x{}",
            r.image.width(),
            r.image.height()
        ));
        let (finished, _) = rawtonic_merge::panorama::finish(&r.image, &r.covered, &config);
        report(&format!(
            "finished {}x{}",
            finished.width(),
            finished.height()
        ));
        crate::register_exit_handler();
    }

    /// Footprint of the decode steps of one RAW (`RAWTONIC_DECODE_ONE`).
    #[test]
    #[ignore]
    fn decode_footprint() {
        let Some(path) = std::env::var_os("RAWTONIC_DECODE_ONE") else {
            return;
        };
        let report = |step: &str| {
            let (now, peak) = rawtonic_runtime::performance::footprint_bytes().unwrap_or((0, 0));
            println!(
                "{:5} MB  peak {:5} MB  {step}",
                now / 1_000_000,
                peak / 1_000_000
            );
        };
        report("start");
        let bytes = std::fs::read(&path).unwrap();
        report("read");
        let source = rawler::rawsource::RawSource::new_from_slice(&bytes);
        report("source");
        let decoder = rawler::get_decoder(&source).unwrap();
        let raw = decoder
            .raw_image(
                &source,
                &rawler::decoders::RawDecodeParams::default(),
                false,
            )
            .unwrap();
        report("raw_image");
        drop(raw);
        drop(source);
        rawtonic_runtime::allocator::release_freed_memory();
        report("dropped");
        let frame = rawtonic_merge::frame::load_frame_from_bytes(
            std::path::Path::new(&path),
            &bytes,
            &Default::default(),
        )
        .unwrap();
        report("load_frame");
        drop(frame);
        crate::register_exit_handler();
    }
}
