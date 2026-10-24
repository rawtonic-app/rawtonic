// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commands of the contribution channels (ADR 0017): prepare a body for the
//! "Review what will be sent" dialog, send it after the user clicked Send,
//! delete a submission by its id; the problem report's context and
//! screenshot; the crash records of channel 2 and their hooks. No channel
//! sends on its own, and a build without a relay URL sends nothing.

use rawtonic_contribute::client::{Receipt, RelayClient};
use rawtonic_contribute::crash::{self, AppInfo, CrashRecord, CrashStore, Gpu, ThisRun};
use rawtonic_contribute::payload::{self, Channel, Prepared};
use rawtonic_contribute::scrub::Scrubber;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use tauri::{AppHandle, Manager};

static SCRUBBER: LazyLock<Scrubber> = LazyLock::new(Scrubber::for_this_machine);

fn client(app: &AppHandle) -> Result<RelayClient, String> {
    RelayClient::from_build(&app.package_info().version.to_string())
        .ok_or_else(|| "not-configured".to_string())
}

/// Whether this build can send contributions at all.
#[tauri::command]
pub fn contribution_available(app_handle: AppHandle) -> bool {
    client(&app_handle).is_ok()
}

/// Scrubs a channel's payload and returns the exact body to review. A
/// report's screenshot (`screenshot.data`, base64) and photo file
/// (`photo.path`) become attachments the body announces by size and
/// SHA-256; hashing a photo's file runs off the main thread.
#[tauri::command]
pub async fn contribution_prepare(
    channel: Channel,
    payload: serde_json::Value,
) -> Result<Prepared, String> {
    tauri::async_runtime::spawn_blocking(move || payload::prepare(channel, payload, &SCRUBBER))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Sends a reviewed body, once. Errors are the client's codes.
#[tauri::command]
pub async fn contribution_send(
    app_handle: AppHandle,
    prepared: Prepared,
) -> Result<Receipt, String> {
    let client = client(&app_handle)?;
    let receipt = client.send(&prepared, &SCRUBBER).await.map_err(|e| {
        log::warn!("Contribution not sent: {e}");
        e.code().to_string()
    })?;
    if !receipt.not_uploaded.is_empty() {
        log::warn!("Contribution sent without {:?}", receipt.not_uploaded);
    }
    Ok(receipt)
}

/// The `app` block every payload carries (version, OS and its major.minor
/// version, architecture), for channels the frontend fills itself (the
/// camera test).
#[tauri::command]
pub fn contribution_app_info(app_handle: AppHandle) -> AppInfo {
    app_info(&app_handle)
}

/// Deletes a submission by the id its receipt gave.
#[tauri::command]
pub async fn contribution_delete(app_handle: AppHandle, id: String) -> Result<(), String> {
    let client = client(&app_handle)?;
    client.delete(&id).await.map_err(|e| {
        log::warn!("Contribution not deleted: {e}");
        e.code().to_string()
    })
}

// --- Problem report (channel 1) -------------------------------------------

/// What the problem report adds besides the user's text: the `app` block,
/// the GPU (if known) and the last lines of this run's log, scrubbed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportContext {
    app: AppInfo,
    gpu: Option<Gpu>,
    log_tail: Option<String>,
    /// Whether a screenshot of the window can be taken here.
    screenshot_available: bool,
}

fn app_info(app: &AppHandle) -> AppInfo {
    AppInfo::new(
        &app.package_info().version.to_string(),
        sysinfo::System::os_version().as_deref(),
    )
}

fn log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_log_dir().ok().map(|dir| dir.join("app.log"))
}

#[tauri::command]
pub async fn problem_report_context(app_handle: AppHandle) -> Result<ReportContext, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::startup::flush_logs();
        let log_tail = log_path(&app_handle)
            .and_then(|path| crash::read_log_end(&path, crash::LOG_READ_BYTES))
            .and_then(|log| crash::log_tail(&log, &SCRUBBER));
        ReportContext {
            app: app_info(&app_handle),
            gpu: GPU.get().cloned(),
            log_tail,
            screenshot_available: crate::window_capture::AVAILABLE,
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// The relay's screenshot cap (decoded bytes).
const MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Serialize)]
pub struct Screenshot {
    mime: &'static str,
    data: String,
    width: u32,
    height: u32,
}

/// A JPEG of `image` within the relay's cap: at most 1600 pixels wide,
/// smaller until it fits.
fn screenshot_jpeg(image: &image::RgbaImage) -> Result<(Vec<u8>, u32, u32), String> {
    let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
    let mut max_width = 1600u32;
    loop {
        let scaled = if rgb.width() > max_width {
            let height = (u64::from(rgb.height()) * u64::from(max_width) / u64::from(rgb.width()))
                .max(1) as u32;
            image::imageops::resize(
                &rgb,
                max_width,
                height,
                image::imageops::FilterType::Triangle,
            )
        } else {
            rgb.clone()
        };
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&scaled)
            .map_err(|e| e.to_string())?;
        if jpeg.len() <= MAX_SCREENSHOT_BYTES || max_width <= 400 {
            return Ok((jpeg, scaled.width(), scaled.height()));
        }
        max_width = (max_width * 3 / 4).max(400);
    }
}

/// A screenshot of the main window as it is now, kept in memory only and
/// sent only if the user includes it in the report. `None` where window
/// capture is not available.
#[tauri::command]
pub async fn problem_report_screenshot(
    app_handle: AppHandle,
) -> Result<Option<Screenshot>, String> {
    use base64::Engine as _;
    if !crate::window_capture::AVAILABLE {
        return Ok(None);
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let handle = app_handle.clone();
    app_handle
        .run_on_main_thread(move || {
            let result = handle
                .get_webview_window("main")
                .ok_or_else(|| "no main window".to_string())
                .and_then(|window| crate::window_capture::capture_rgba(&window, false, false));
            let _ = sender.send(result);
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let image = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "the main thread did not answer".to_string())??;
        let (jpeg, width, height) = screenshot_jpeg(&image)?;
        Ok(Some(Screenshot {
            mime: "image/jpeg",
            data: base64::engine::general_purpose::STANDARD.encode(jpeg),
            width,
            height,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- Crash report on the next start (channel 2) ---------------------------

static CRASHES: OnceLock<CrashStore> = OnceLock::new();
static GPU: OnceLock<Gpu> = OnceLock::new();

/// At start, before the log is replaced: turns a crash of the previous run
/// into a record (unless the user chose Never), marks this run and arms the
/// panic and signal hooks. Not for headless runs.
pub(crate) fn start_crash_records(app: &AppHandle, keep: bool) -> Option<CrashRecord> {
    let dir = app.path().app_config_dir().ok()?.join("crash-reports");
    let store = CrashStore::new(dir);
    let run = ThisRun {
        pid: std::process::id(),
        app: app_info(app),
        now: crash::now_secs(),
    };
    let previous_log =
        || log_path(app).and_then(|path| crash::read_log_end(&path, crash::LOG_READ_BYTES));
    let record = store
        .start(
            &run,
            crate::startup::process_alive,
            previous_log,
            &SCRUBBER,
            keep,
        )
        .unwrap_or_else(|e| {
            eprintln!("Crash records unavailable: {e}");
            None
        });
    if !keep {
        store.remove_all();
    }
    crash::arm(&store);
    crash::set_recording(&store, keep);
    let _ = CRASHES.set(store);
    record
}

/// The graphics adapter, once known: for reports and this run's marker.
pub(crate) fn note_gpu(info: &wgpu::AdapterInfo) {
    let gpu = Gpu::new(info.vendor, &info.name);
    if let Some(store) = CRASHES.get()
        && let Err(e) = store.note_gpu(std::process::id(), gpu.clone())
    {
        log::warn!("Could not note the GPU for crash records: {e}");
    }
    let _ = GPU.set(gpu);
}

/// A clean exit: the next start has no crash to ask about.
pub(crate) fn clean_exit() {
    crash::exiting();
    if let Some(store) = CRASHES.get() {
        store.clean_exit(std::process::id());
    }
}

fn crashes() -> Result<&'static CrashStore, String> {
    CRASHES
        .get()
        .ok_or_else(|| "crash records unavailable".to_string())
}

/// Crash records not yet sent, newest first (older than 30 days go).
#[tauri::command]
pub fn crash_reports_pending() -> Vec<CrashRecord> {
    crashes()
        .map(|store| store.pending(crash::now_secs()))
        .unwrap_or_default()
}

/// The user was asked about this crash (Not now): it is not asked again.
#[tauri::command]
pub fn crash_report_mark_asked(id: String) -> Result<(), String> {
    crashes()?.mark_asked(&id).map_err(|e| e.to_string())
}

/// Deletes a crash record (sent, or not wanted).
#[tauri::command]
pub fn crash_report_remove(id: String) -> Result<(), String> {
    crashes()?.remove(&id);
    Ok(())
}

/// Turns crash records on or off for this run too (the preference, or
/// Never); off deletes every record and this run's notes.
#[tauri::command]
pub fn crash_reports_set_enabled(enabled: bool) -> Result<(), String> {
    let store = crashes()?;
    crash::set_recording(store, enabled);
    if !enabled {
        store.remove_all();
    }
    Ok(())
}
