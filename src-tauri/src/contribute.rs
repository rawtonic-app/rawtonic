// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commands of the contribution channels (ADR 0017): prepare a body for the
//! "Review what will be sent" dialog, send it after the user clicked Send,
//! delete a submission by its id; the problem report's context and
//! screenshot; the crash records of channel 2 and their hooks; the usage
//! statistics of channel 4. No channel sends on its own — statistics, once
//! the user switched them on, send each finished week at a later start —
//! and a build without a relay URL sends nothing.

use rawtonic_contribute::client::{Receipt, RelayClient};
use rawtonic_contribute::crash::{self, AppInfo, CrashRecord, CrashStore, Gpu, ThisRun};
use rawtonic_contribute::payload::{self, Channel, Prepared};
use rawtonic_contribute::scrub::Scrubber;
use rawtonic_contribute::stats::{Sent, StatsStore};
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

/// Scrubs a channel's payload and returns the exact body to review.
#[tauri::command]
pub fn contribution_prepare(
    channel: Channel,
    payload: serde_json::Value,
) -> Result<Prepared, String> {
    payload::prepare(channel, payload, &SCRUBBER).map_err(|e| e.to_string())
}

/// Sends a reviewed body, once. Errors are the client's codes.
#[tauri::command]
pub async fn contribution_send(
    app_handle: AppHandle,
    prepared: Prepared,
) -> Result<Receipt, String> {
    let client = client(&app_handle)?;
    client.send(&prepared, &SCRUBBER).await.map_err(|e| {
        log::warn!("Contribution not sent: {e}");
        e.code().to_string()
    })
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
    if let Some(stats) = STATS.get() {
        stats.note_gpu_vendor(crash::now_secs(), &gpu.vendor);
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

// --- Usage statistics (channel 4) ------------------------------------------

static STATS: OnceLock<StatsStore> = OnceLock::new();

/// How long after the start the finished weeks are sent (the start itself
/// stays free of network work).
const STATS_SEND_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// At start: the store of weekly counters (on only when the user switched
/// statistics on), this run noted, and the finished weeks sent once.
pub(crate) fn start_usage_stats(app: &AppHandle, on: bool) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let store = StatsStore::new(dir.join("usage-stats.json"), on);
    store.note_run(crash::now_secs(), &app_info(app));
    // Weeks the relay no longer takes go at every start, also in a build
    // without a relay.
    store.due(crash::now_secs());
    if STATS.set(store).is_err() || !on {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STATS_SEND_DELAY).await;
        send_finished_weeks(&app).await;
    });
}

async fn send_finished_weeks(app: &AppHandle) {
    use rawtonic_contribute::client::SendError;
    let (Some(stats), Ok(client)) = (STATS.get(), client(app)) else {
        return;
    };
    // Switched off from here on (also off and on again): nothing of this
    // round is sent or kept again.
    let Some(ticket) = stats.ticket() else {
        return;
    };
    let info = app_info(app);
    for week in stats.due(crash::now_secs()) {
        if !stats.still(ticket) {
            return;
        }
        // At most once: from here the week is not kept, so a send whose
        // outcome is unknown is never repeated.
        let Some(kept) = stats.take(&week) else {
            continue;
        };
        let value = rawtonic_contribute::stats::payload(&week, &kept, &info);
        let prepared = match payload::prepare(Channel::Stats, value, &SCRUBBER) {
            Ok(prepared) => prepared,
            Err(e) => {
                log::warn!("Usage statistics of {week} not prepared: {e}");
                continue;
            }
        };
        // Asked again right before the request goes out: switched off
        // meanwhile, nothing is sent.
        match client
            .send_if(&prepared, &SCRUBBER, || stats.still(ticket))
            .await
        {
            Ok(_) => stats.sent(&week, prepared.body, ticket),
            // Certainly not arrived: the next start tries again while the
            // relay still takes the week (not if switched off).
            Err(e @ (SendError::Unreached(_) | SendError::RateLimited | SendError::Cancelled)) => {
                log::info!("Usage statistics of {week} not sent now: {e}");
                stats.put_back(&week, kept, ticket);
            }
            Err(e) => log::warn!("Usage statistics of {week} not confirmed, not sent again: {e}"),
        }
    }
}

/// A photo of this camera was opened (only counted while statistics are
/// on, and only a body of the camera library's list, by its name there).
pub(crate) fn note_camera(exif: &std::collections::HashMap<String, String>) {
    let Some(stats) = STATS.get().filter(|stats| stats.is_on()) else {
        return;
    };
    if let (Some(make), Some(model)) = (exif.get("Make"), exif.get("Model"))
        && let Some((make, model)) = rawtonic_rawdecode::fingerprint::known_camera(make, model)
    {
        stats.note_camera(crash::now_secs(), &make, &model);
    }
}

/// Turns statistics on or off and saves the choice. Off stops at once —
/// a send under way is dropped — and deletes what was kept, also when saving
/// the choice fails; on starts (from no kept weeks) only once the choice is
/// saved. An error says what did not work: the switch then shows the choice
/// as not made.
#[tauri::command]
pub fn usage_stats_set_enabled(app_handle: AppHandle, enabled: bool) -> Result<(), String> {
    let stats = STATS.get();
    if enabled {
        crate::app_settings::persist_usage_stats(&app_handle, true)?;
        if let Some(stats) = stats {
            if let Err(e) = stats.set_on(true) {
                // Not on after all: the saved choice goes back to off.
                let _ = crate::app_settings::persist_usage_stats(&app_handle, false);
                return Err(e);
            }
            stats.note_run(crash::now_secs(), &app_info(&app_handle));
        }
        Ok(())
    } else {
        let cleared = stats.map_or(Ok(()), |stats| stats.set_on(false));
        crate::app_settings::persist_usage_stats(&app_handle, false)?;
        cleared
    }
}

/// Tools used (keys from the fixed list), while statistics are on.
#[tauri::command]
pub fn usage_stats_note_tools(tools: Vec<String>) {
    if let Some(stats) = STATS.get() {
        stats.note_tools(crash::now_secs(), &tools);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingWeek {
    week: String,
    /// Exactly as it will be prepared for sending.
    body: String,
    /// This week so far (sent after it ends); else a finished week sent at
    /// the next start.
    current: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsPreview {
    /// Every body kept to be sent, in send order.
    pending: Vec<PendingWeek>,
    last_sent: Option<Sent>,
}

/// "Show what is sent": every body waiting to be sent, and the last one sent.
#[tauri::command]
pub fn usage_stats_preview(app_handle: AppHandle) -> UsageStatsPreview {
    let Some(stats) = STATS.get() else {
        return UsageStatsPreview {
            pending: Vec::new(),
            last_sent: None,
        };
    };
    let now = crash::now_secs();
    let current = rawtonic_contribute::stats::iso_week(now);
    let pending = if stats.is_on() {
        stats
            .pending(now, &app_info(&app_handle))
            .into_iter()
            .filter_map(|(week, value)| {
                let prepared = payload::prepare(Channel::Stats, value, &SCRUBBER).ok()?;
                Some(PendingWeek {
                    current: week == current,
                    week,
                    body: prepared.body,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    UsageStatsPreview {
        pending,
        last_sent: stats.last_sent(),
    }
}
