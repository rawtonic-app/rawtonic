//! Start-up services: logging, window placement, the frontend-ready handshake
//! and the GPU crash check. Moved out of `lib.rs`.
use crate::app_state::WindowState;
use crate::launch_request::LaunchPayload;
use crate::state::lock::lock_recover;
use crate::state::startup::StartupState;
use std::fs;
use std::io::Write;
use std::panic;
use std::sync::{OnceLock, mpsc};
use tauri::Manager;

enum LogMessage {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

static LOG_WRITER: OnceLock<mpsc::Sender<LogMessage>> = OnceLock::new();

/// Log lines are written to stderr and the log file on a thread of their
/// own. A write can block: on the Linux PC's btrfs one waited ~0.9 s for
/// metadata space right after preview proxies were written, and fern writes
/// every output under a lock all logging threads share, so the preview
/// command, the preview worker and the main thread (which presents the
/// webview) stood still with it — the first slider drag after opening a
/// photo showed no frame for ~0.9 s (notes: parity-gaps/G55-performance.md).
fn start_log_writer(file: Option<fs::File>) -> mpsc::Sender<LogMessage> {
    let (sender, receiver) = mpsc::channel::<LogMessage>();
    let _ = LOG_WRITER.set(sender.clone());
    let spawned = std::thread::Builder::new()
        .name("log-writer".into())
        .spawn(move || {
            let mut stderr = std::io::stderr();
            let mut file = file.map(std::io::BufWriter::new);
            while let Ok(message) = receiver.recv() {
                // Write what is waiting, then flush once.
                let mut next = Some(message);
                while let Some(message) = next {
                    match message {
                        LogMessage::Line(line) => {
                            let _ = stderr.write_all(line.as_bytes());
                            if let Some(file) = file.as_mut() {
                                let _ = file.write_all(line.as_bytes());
                            }
                        }
                        LogMessage::Flush(done) => {
                            if let Some(file) = file.as_mut() {
                                let _ = file.flush();
                            }
                            let _ = done.send(());
                        }
                    }
                    next = receiver.try_recv().ok();
                }
                if let Some(file) = file.as_mut() {
                    let _ = file.flush();
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("Failed to start the log writer: {}", e);
    }
    sender
}

/// Waits (at most 2 s) until the lines logged so far are written: before the
/// process ends.
pub(crate) fn flush_logs() {
    if let Some(sender) = LOG_WRITER.get() {
        let (done, written) = mpsc::sync_channel(1);
        if sender.send(LogMessage::Flush(done)).is_ok() {
            let _ = written.recv_timeout(std::time::Duration::from_secs(2));
        }
    }
}

/// The panic hook until the logger's (`setup_logging`) replaces it: a panic
/// before the log exists goes to stderr and, once the crash records are
/// armed, leaves a note for the next start's crash report.
pub(crate) fn install_early_panic_hook() {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        rawtonic_contribute::crash::note_panic(info);
        default(info);
    }));
}

/// Logs to `file_name` in the app's log folder (replaced on each start).
pub(crate) fn setup_logging(app_handle: &tauri::AppHandle, file_name: &str) {
    let log_dir = match app_handle.path().app_log_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to get app log directory: {}", e);
            return;
        }
    };

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory at {:?}: {}", log_dir, e);
    }

    let log_file_path = log_dir.join(file_name);

    let log_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&log_file_path)
        .ok();

    let var = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let level: log::LevelFilter = var.parse().unwrap_or(log::LevelFilter::Info);

    let dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(level);
    if log_file.is_none() {
        eprintln!(
            "Failed to open log file at {:?}. Logging to console only.",
            log_file_path
        );
    }
    let sender = start_log_writer(log_file);
    let dispatch = dispatch.chain(fern::Output::call(move |record| {
        let _ = sender.send(LogMessage::Line(format!("{}\n", record.args())));
    }));

    if let Err(e) = dispatch.apply() {
        eprintln!("Failed to apply logger configuration: {}", e);
    }

    panic::set_hook(Box::new(|info| {
        let message = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            format!("{:?}", info.payload())
        };
        let location = info.location().map_or_else(
            || "at an unknown location".to_string(),
            |loc| format!("at {}:{}:{}", loc.file(), loc.line(), loc.column()),
        );
        log::error!("PANIC! {} - {}", location, message.trim());
        // For the crash report on the next start, should the app end now.
        rawtonic_contribute::crash::note_panic(info);
        flush_logs();
    }));

    log::info!(
        "Logger initialized successfully. Log file at: {:?}",
        log_file_path
    );
}

#[tauri::command]
pub(crate) fn get_log_file_path(app_handle: tauri::AppHandle) -> Result<String, String> {
    let log_dir = app_handle.path().app_log_dir().map_err(|e| e.to_string())?;
    let log_file_path = log_dir.join("app.log");
    Ok(log_file_path.to_string_lossy().to_string())
}

#[tauri::command]
pub(crate) fn frontend_log(level: String, message: String) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    #[cfg(feature = "performance-profiling")]
    if let Some(json) = trimmed.strip_prefix("[PERF] ")
        && let Ok(mut record) = serde_json::from_str::<serde_json::Value>(json)
        && record["stage"] == "library_first_grid_frame"
        && let Some(elapsed) = crate::performance::application_elapsed_ms()
    {
        record["native_boot_ms"] = serde_json::json!(elapsed);
        log::info!("[PERF] {record}");
        return Ok(());
    }

    let log_line = |line: &str| match level.to_lowercase().as_str() {
        "error" => log::error!("[frontend] {}", line),
        "warn" => log::warn!("[frontend] {}", line),
        "debug" => log::debug!("[frontend] {}", line),
        "trace" => log::trace!("[frontend] {}", line),
        _ => log::info!("[frontend] {}", line),
    };

    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        log_line(line);
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonitorBounds {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) fn saved_window_state_is_usable(
    state: &WindowState,
    monitors: &[MonitorBounds],
) -> bool {
    if state.width < 800 || state.height < 600 {
        return false;
    }
    overlaps_a_monitor(state.x, state.y, state.width, state.height, monitors)
}

/// Whether a window at `x`, `y` of `width` × `height` (physical px) shows
/// at least 100 × 100 px on one of `monitors` (none known: assumed so).
pub(crate) fn overlaps_a_monitor(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    monitors: &[MonitorBounds],
) -> bool {
    if monitors.is_empty() {
        return true;
    }

    let window_left = x as i64;
    let window_top = y as i64;
    let window_right = window_left + width as i64;
    let window_bottom = window_top + height as i64;

    monitors.iter().any(|monitor| {
        let monitor_left = monitor.x as i64;
        let monitor_top = monitor.y as i64;
        let monitor_right = monitor_left + monitor.width as i64;
        let monitor_bottom = monitor_top + monitor.height as i64;

        let overlap_width = window_right.min(monitor_right) - window_left.max(monitor_left);
        let overlap_height = window_bottom.min(monitor_bottom) - window_top.max(monitor_top);

        overlap_width >= 100 && overlap_height >= 100
    })
}

pub(crate) fn available_monitor_bounds(window: &tauri::WebviewWindow) -> Vec<MonitorBounds> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .into_iter()
                .map(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    MonitorBounds {
                        x: position.x,
                        y: position.y,
                        width: size.width,
                        height: size.height,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Restores window geometry using the platform's stored-size convention.
/// Windows stores outer bounds, but Tauri's setter takes an inner size.
/// Move first so the frame measurements use the destination monitor's DPI.
pub(crate) fn restore_window_bounds(
    window: &tauri::WebviewWindow,
    state: &WindowState,
    _configured_size: tauri::LogicalSize<f64>,
) {
    let position = tauri::Position::Physical(tauri::PhysicalPosition::new(state.x, state.y));
    #[cfg(windows)]
    let _ = window.set_position(position);

    #[cfg(windows)]
    let size = match (window.outer_size(), window.inner_size()) {
        (Ok(outer), Ok(inner)) => inner_size_for_saved_outer(
            tauri::PhysicalSize::new(state.width, state.height),
            outer,
            inner,
        ),
        _ => None,
    }
    .map(tauri::Size::Physical)
    .unwrap_or_else(|| {
        log::warn!("Could not measure the window frame; using the configured window size.");
        tauri::Size::Logical(_configured_size)
    });
    #[cfg(target_os = "linux")]
    let size = linux_restored_window_size(state.width, state.height, state.scale_factor)
        .unwrap_or(tauri::Size::Logical(_configured_size));
    #[cfg(not(any(windows, target_os = "linux")))]
    let size = tauri::Size::Physical(tauri::PhysicalSize::new(state.width, state.height));

    let _ = window.set_size(size);
    #[cfg(not(windows))]
    let _ = window.set_position(position);
}

#[cfg(any(windows, test))]
fn inner_size_for_saved_outer(
    saved_outer: tauri::PhysicalSize<u32>,
    current_outer: tauri::PhysicalSize<u32>,
    current_inner: tauri::PhysicalSize<u32>,
) -> Option<tauri::PhysicalSize<u32>> {
    let frame_width = current_outer.width.checked_sub(current_inner.width)?;
    let frame_height = current_outer.height.checked_sub(current_inner.height)?;
    let width = saved_outer.width.checked_sub(frame_width)?;
    let height = saved_outer.height.checked_sub(frame_height)?;
    (width > 0 && height > 0).then_some(tauri::PhysicalSize::new(width, height))
}

/// Saved sizes are physical pixels. A hidden GTK window and its monitor have
/// not yet adopted Wayland's scale, so use the scale stored with those pixels.
/// Older files migrate to the configured logical size once.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linux_restored_window_size(
    width: u32,
    height: u32,
    scale: Option<f64>,
) -> Option<tauri::Size> {
    let scale = scale.filter(|scale| scale.is_finite() && *scale > 0.0)?;
    Some(tauri::Size::Logical(tauri::LogicalSize::new(
        width as f64 / scale,
        height as f64 / scale,
    )))
}

/// What the start does with a window that does not fit on its screen.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WorkAreaFit {
    /// Fully inside a work area.
    Keep,
    /// Move (and shrink) to these outer bounds.
    Place(MonitorBounds),
    /// Not even the minimum size fits: maximize on this work area.
    Maximize(MonitorBounds),
}

/// Fits a window's outer bounds into the work area (the screen without the
/// taskbar) of the monitor it overlaps most, or the first one when it is on
/// none. A window that only sticks out is moved in at its size; one larger
/// than the work area shrinks to it and is centred; when even
/// `min_outer` does not fit, it is maximized. Physical pixels throughout.
#[cfg(any(windows, test))]
pub(crate) fn fit_into_work_area(
    window: MonitorBounds,
    min_outer: (u32, u32),
    work_areas: &[MonitorBounds],
) -> WorkAreaFit {
    let overlap = |area: &MonitorBounds| {
        let width = (window.x as i64 + window.width as i64).min(area.x as i64 + area.width as i64)
            - (window.x as i64).max(area.x as i64);
        let height = (window.y as i64 + window.height as i64)
            .min(area.y as i64 + area.height as i64)
            - (window.y as i64).max(area.y as i64);
        width.max(0) * height.max(0)
    };
    let Some(area) = work_areas
        .iter()
        .copied()
        .filter(|area| area.width > 0 && area.height > 0)
        .max_by_key(|area| overlap(area))
        .filter(|area| overlap(area) > 0)
        .or_else(|| work_areas.first().copied())
    else {
        return WorkAreaFit::Keep;
    };
    let inside = window.x >= area.x
        && window.y >= area.y
        && window.x as i64 + window.width as i64 <= area.x as i64 + area.width as i64
        && window.y as i64 + window.height as i64 <= area.y as i64 + area.height as i64;
    if inside {
        return WorkAreaFit::Keep;
    }
    if min_outer.0 > area.width || min_outer.1 > area.height {
        return WorkAreaFit::Maximize(area);
    }
    let width = window.width.min(area.width);
    let height = window.height.min(area.height);
    let (x, y) = if width < window.width || height < window.height {
        (
            area.x + ((area.width - width) / 2) as i32,
            area.y + ((area.height - height) / 2) as i32,
        )
    } else {
        let clamp = |pos: i32, size: u32, start: i32, extent: u32| {
            pos.clamp(start, start + (extent - size) as i32)
        };
        (
            clamp(window.x, width, area.x, area.width),
            clamp(window.y, height, area.y, area.height),
        )
    };
    WorkAreaFit::Place(MonitorBounds {
        x,
        y,
        width,
        height,
    })
}

/// Set when the start found no room for the window's minimum size; the
/// window is then maximized when it is first shown (`frontend_ready`).
#[cfg(windows)]
static MAXIMIZE_AT_SHOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keeps the hidden main window on screen at the start (Windows): the
/// configured 1440 × 900 at 125 % display scale is taller than a 1080 px
/// screen, and a saved geometry may come from another monitor or scale.
/// Runs after the restore or centring. Moving to a monitor of another scale
/// lets Windows rescale the window, so it measures again after a move.
#[cfg(windows)]
pub(crate) fn keep_window_on_screen(window: &tauri::WebviewWindow, min_inner: (f64, f64)) {
    for _ in 0..2 {
        let (Ok(position), Ok(outer), Ok(inner), Ok(monitors)) = (
            window.outer_position(),
            window.outer_size(),
            window.inner_size(),
            window.available_monitors(),
        ) else {
            return;
        };
        let current = MonitorBounds {
            x: position.x,
            y: position.y,
            width: outer.width,
            height: outer.height,
        };
        let frame = (
            outer.width.saturating_sub(inner.width),
            outer.height.saturating_sub(inner.height),
        );
        let work_areas: Vec<MonitorBounds> = monitors
            .iter()
            .map(|monitor| {
                let area = monitor.work_area();
                MonitorBounds {
                    x: area.position.x,
                    y: area.position.y,
                    width: area.size.width,
                    height: area.size.height,
                }
            })
            .collect();
        // The minimum size at the target monitor's scale: take the largest
        // scale on offer, so the check never lets a too-small area pass.
        let scale = monitors
            .iter()
            .map(|monitor| monitor.scale_factor())
            .fold(1.0_f64, f64::max);
        let min_outer = (
            (min_inner.0 * scale).ceil() as u32 + frame.0,
            (min_inner.1 * scale).ceil() as u32 + frame.1,
        );
        match fit_into_work_area(current, min_outer, &work_areas) {
            WorkAreaFit::Keep => return,
            WorkAreaFit::Maximize(area) => {
                log::warn!(
                    "Window {current:?} does not fit the work area {area:?} even at its minimum size; maximizing."
                );
                let _ = window.set_position(tauri::PhysicalPosition::new(area.x, area.y));
                MAXIMIZE_AT_SHOW.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            WorkAreaFit::Place(bounds) => {
                log::info!("Window {current:?} moved into the work area as {bounds:?}.");
                let _ = window.set_position(tauri::PhysicalPosition::new(bounds.x, bounds.y));
                let _ = window.set_size(tauri::PhysicalSize::new(
                    bounds.width.saturating_sub(frame.0).max(1),
                    bounds.height.saturating_sub(frame.1).max(1),
                ));
            }
        }
    }
}

#[tauri::command]
pub(crate) fn frontend_ready(
    app_handle: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<StartupState>,
) -> Result<LaunchPayload, String> {
    let is_first_run = !state
        .window_setup_complete
        .swap(true, std::sync::atomic::Ordering::Relaxed);
    if is_first_run {
        crate::contribute::note_phase("running");
        crate::start_failure::mark_started();
    }

    {
        #[cfg(any(windows, target_os = "linux"))]
        let mut should_maximize = false;
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = (&app_handle, is_first_run);

        #[cfg(any(windows, target_os = "linux"))]
        if is_first_run && let Ok(config_dir) = app_handle.path().app_config_dir() {
            let path = config_dir.join("window_state.json");

            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(saved_state) = serde_json::from_str::<WindowState>(&contents)
            {
                // `fullscreen` in older files is not restored: full screen is
                // the photo-only view (F), which a start does not return to.
                should_maximize = saved_state.maximized;

                if should_maximize
                    && let Some(monitor) = window
                        .current_monitor()
                        .ok()
                        .flatten()
                        .or_else(|| window.primary_monitor().ok().flatten())
                        .or_else(|| {
                            window
                                .available_monitors()
                                .ok()
                                .and_then(|m| m.into_iter().next())
                        })
                {
                    let monitor_size = monitor.size();
                    let monitor_pos = monitor.position();
                    let default_width = 1280i32;
                    let default_height = 720i32;
                    let center_x = monitor_pos.x + (monitor_size.width as i32 - default_width) / 2;
                    let center_y =
                        monitor_pos.y + (monitor_size.height as i32 - default_height) / 2;

                    let _ = window.set_size(tauri::PhysicalSize::new(
                        default_width as u32,
                        default_height as u32,
                    ));
                    let _ = window.set_position(tauri::PhysicalPosition::new(center_x, center_y));
                }
            }
        }

        if let Err(e) = window.show() {
            log::error!("Failed to show window: {}", e);
        }
        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus window: {}", e);
        }
        #[cfg(windows)]
        if is_first_run && MAXIMIZE_AT_SHOW.load(std::sync::atomic::Ordering::Relaxed) {
            should_maximize = true;
        }
        #[cfg(any(windows, target_os = "linux"))]
        if is_first_run && should_maximize {
            let _ = window.maximize();
        }
    }

    let open_with_file = lock_recover(&state.initial_file_path).take();
    let edit_session = lock_recover(&state.pending_edit_session).take();
    if let Some(path) = &open_with_file {
        log::info!("Frontend is ready, returning initial path: {}", path);
    }
    if let Some(session) = &edit_session {
        log::info!(
            "Frontend is ready, returning external edit session for: {}",
            session.source
        );
    }
    Ok(LaunchPayload {
        open_with_file,
        edit_session,
    })
}

/// The processing backend chosen in Preferences (other than "auto") for
/// wgpu (`WGPU_BACKEND`), before the GPU starts; the fallback of
/// `rawtonic_gpu::device_start` tries the others after it.
pub(crate) fn apply_processing_backend(backend: Option<&str>) {
    if let Some(backend) = backend
        && backend != "auto"
    {
        // SAFETY: set in the app's setup before the GPU starts and reads it,
        // where the app has always set it.
        unsafe { std::env::set_var("WGPU_BACKEND", backend) };
    }
}

/// Whether a device may start on OpenGL: wgpu's OpenGL backend is not
/// compiled in on Apple platforms (it would need ANGLE), and on Windows it
/// is never used (`rawtonic_gpu::device_start::usable_backends`).
pub(crate) const GL_BACKEND_USABLE: bool = !cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
));

pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 only checks that the process exists; EPERM means it
        // exists but belongs to someone else.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let mut system = sysinfo::System::new();
        let pid = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).is_some()
    }
}

/// Whether GTK runs on Wayland (else X11 or XWayland): it takes the first
/// backend it can open from GDK_BACKEND.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn gtk_uses_wayland(env: impl Fn(&str) -> Option<String>) -> bool {
    match env("GDK_BACKEND").and_then(|list| {
        list.split(',')
            .map(str::trim)
            .find(|b| !b.is_empty() && *b != "*")
            .map(str::to_owned)
    }) {
        Some(backend) => backend == "wayland",
        None => env("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()),
    }
}

/// Whether to set `WEBKIT_FORCE_DMABUF_RENDERER=1` before the webview exists.
///
/// WebKitGTK up to at least 2.50 (the AppImage's) turns its DMA-BUF renderer
/// off when it finds `/dev/nvidia*`, and on Wayland that leaves it without
/// accelerated compositing: every frame of the page is rasterised on the CPU
/// (the photo rescaled each time), which held slider drags to ~30 webview
/// frames per second on an RTX 3080 (notes: benchmark-results/
/// linux-slider-drag-2026-10-17/). Its reason, the explicit-sync protocol
/// error on Wayland, is handled by `tauri_plugin_wayland_nvidia_quirk`
/// (driver 555 and newer). Any WebKit renderer variable the user set wins,
/// and so does Compatibility Mode, which turns the renderer off.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn force_webkit_dmabuf_renderer(
    env: impl Fn(&str) -> Option<String>,
    nvidia_driver: Option<&str>,
    compatibility_mode: bool,
) -> bool {
    const USER_CHOICES: [&str; 3] = [
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        "WEBKIT_FORCE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
    ];
    if compatibility_mode || USER_CHOICES.iter().any(|name| env(name).is_some()) {
        return false;
    }
    let wayland = gtk_uses_wayland(&env);
    let explicit_sync = nvidia_driver
        .and_then(|version| version.trim().split('.').next()?.parse::<u32>().ok())
        .is_some_and(|major| major >= 555);
    wayland && explicit_sync
}

#[cfg(test)]
mod window_scale_tests {
    use super::linux_restored_window_size;

    #[test]
    fn saved_outer_bounds_do_not_grow_across_restarts_or_frame_styles() {
        use super::inner_size_for_saved_outer;
        use tauri::PhysicalSize;
        let saved = PhysicalSize::new(1280, 900);
        for frame in [(0, 0), (16, 39), (24, 59), (32, 78)] {
            let mut inner = PhysicalSize::new(900, 650);
            for _ in 0..10 {
                let outer = PhysicalSize::new(inner.width + frame.0, inner.height + frame.1);
                inner = inner_size_for_saved_outer(saved, outer, inner).unwrap();
                assert_eq!(
                    PhysicalSize::new(inner.width + frame.0, inner.height + frame.1),
                    saved,
                    "restoring an outer size as an inner size would add the frame each time"
                );
            }
        }
    }

    #[test]
    fn inconsistent_or_oversized_frames_request_the_configured_size() {
        use super::inner_size_for_saved_outer;
        use tauri::PhysicalSize;
        let saved = PhysicalSize::new(1280, 900);
        assert_eq!(
            inner_size_for_saved_outer(
                saved,
                PhysicalSize::new(900, 700),
                PhysicalSize::new(901, 650)
            ),
            None
        );
        assert_eq!(
            inner_size_for_saved_outer(
                saved,
                PhysicalSize::new(900, 700),
                PhysicalSize::new(850, 701)
            ),
            None
        );
        for saved in [PhysicalSize::new(16, 900), PhysicalSize::new(1280, 38)] {
            assert_eq!(
                inner_size_for_saved_outer(
                    saved,
                    PhysicalSize::new(916, 689),
                    PhysicalSize::new(900, 650)
                ),
                None
            );
        }
    }

    #[test]
    fn hidpi_physical_sizes_restore_without_growing_on_restart() {
        for scale in [1.0, 1.25, 2.0] {
            let size = linux_restored_window_size(2400, 1600, Some(scale)).unwrap();
            assert_eq!(
                size.to_physical::<u32>(scale),
                tauri::PhysicalSize::new(2400, 1600)
            );
        }
        assert_eq!(
            linux_restored_window_size(2400, 1600, Some(2.0)),
            Some(tauri::Size::Logical(tauri::LogicalSize::new(1200.0, 800.0)))
        );
    }

    #[test]
    fn legacy_or_invalid_scales_request_the_configured_size() {
        assert_eq!(linux_restored_window_size(1200, 800, None), None);
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(linux_restored_window_size(1200, 800, Some(scale)), None);
        }
    }
}

#[cfg(test)]
mod gpu_crash_flag_tests {
    use super::process_alive;
    use crate::gpu_start_guard::{FlagCheck, StartCrash, check_flag};

    #[test]
    fn gpu_flag_child_waits_for_stdin() {
        if std::env::var_os("RAWTONIC_GPU_FLAG_TEST_CHILD").is_some() {
            use std::io::Read;
            let _ = std::io::stdin().read(&mut [0]);
        }
    }

    struct TestChild(std::process::Child);
    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_flag_of_a_running_process_is_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("rawtonic-gpu-flag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flag = dir.join(".gpu_init_crash_flag");
        assert_eq!(check_flag(&flag, process_alive), FlagCheck::Clean);
        // Keep an actual child alive until stdin closes: Windows has no
        // launchd/init PID 1, and a short-lived --list child could exit before
        // the liveness query. The guard also reaps it if an assertion fails.
        let mut child = TestChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "startup::gpu_crash_flag_tests::gpu_flag_child_waits_for_stdin",
                ])
                .env("RAWTONIC_GPU_FLAG_TEST_CHILD", "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        assert!(process_alive(pid));
        std::fs::write(
            &flag,
            format!(r#"{{"pid":{pid},"backends":"vulkan","stage":"device"}}"#),
        )
        .unwrap();
        assert_eq!(
            check_flag(&flag, process_alive),
            FlagCheck::InProgressElsewhere
        );
        drop(child.0.stdin.take());
        child.0.wait().unwrap();
        assert!(!process_alive(pid));
        assert_eq!(
            check_flag(&flag, process_alive),
            FlagCheck::Crashed(StartCrash {
                backends: wgpu::Backends::VULKAN,
                stage: "device".into()
            })
        );
        // A legacy flag is a crash whose backends are not known.
        std::fs::write(&flag, "initializing_gpu").unwrap();
        assert_eq!(
            check_flag(&flag, process_alive),
            FlagCheck::Crashed(StartCrash {
                backends: wgpu::Backends::empty(),
                stage: "unknown".into()
            })
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod webkit_renderer_tests {
    use super::force_webkit_dmabuf_renderer;

    fn env(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn nvidia_on_wayland_gets_the_accelerated_renderer() {
        let wayland = env(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert!(force_webkit_dmabuf_renderer(
            &wayland,
            Some("615.78.08\n"),
            false
        ));
        assert!(force_webkit_dmabuf_renderer(
            &wayland,
            Some("555.42.02"),
            false
        ));
        // Before explicit sync the quirk plugin cannot prevent the protocol error.
        assert!(!force_webkit_dmabuf_renderer(
            &wayland,
            Some("550.120"),
            false
        ));
        // Not NVIDIA's driver: WebKit already uses its renderer.
        assert!(!force_webkit_dmabuf_renderer(&wayland, None, false));
        assert!(!force_webkit_dmabuf_renderer(
            &wayland,
            Some("garbage"),
            false
        ));
    }

    #[test]
    fn user_choices_and_x11_are_left_alone() {
        let nvidia = Some("615.78.08");
        assert!(!force_webkit_dmabuf_renderer(
            env(&[("WAYLAND_DISPLAY", "wayland-0")]),
            nvidia,
            true
        ));
        for name in [
            "WEBKIT_DISABLE_DMABUF_RENDERER",
            "WEBKIT_FORCE_DMABUF_RENDERER",
            "WEBKIT_DISABLE_COMPOSITING_MODE",
        ] {
            let vars: &'static [(&'static str, &'static str)] = match name {
                "WEBKIT_DISABLE_DMABUF_RENDERER" => &[
                    ("WAYLAND_DISPLAY", "w"),
                    ("WEBKIT_DISABLE_DMABUF_RENDERER", "0"),
                ],
                "WEBKIT_FORCE_DMABUF_RENDERER" => &[
                    ("WAYLAND_DISPLAY", "w"),
                    ("WEBKIT_FORCE_DMABUF_RENDERER", "0"),
                ],
                _ => &[
                    ("WAYLAND_DISPLAY", "w"),
                    ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
                ],
            };
            assert!(
                !force_webkit_dmabuf_renderer(env(vars), nvidia, false),
                "{name}"
            );
        }
        assert!(!force_webkit_dmabuf_renderer(env(&[]), nvidia, false));
        assert!(!force_webkit_dmabuf_renderer(
            env(&[("WAYLAND_DISPLAY", "w"), ("GDK_BACKEND", "x11")]),
            nvidia,
            false
        ));
        assert!(force_webkit_dmabuf_renderer(
            env(&[("GDK_BACKEND", "wayland,x11")]),
            nvidia,
            false
        ));
    }
}

#[cfg(test)]
mod work_area_tests {
    use super::{MonitorBounds, WorkAreaFit, fit_into_work_area};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> MonitorBounds {
        MonitorBounds {
            x,
            y,
            width,
            height,
        }
    }

    /// 1920 × 1080 at 125 % with a 48 px taskbar; the minimum 780 × 560 at 125 %.
    const LAPTOP: MonitorBounds = MonitorBounds {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032,
    };
    const MIN: (u32, u32) = (975, 700);

    #[test]
    fn a_window_inside_the_work_area_stays_as_it_is() {
        assert_eq!(
            fit_into_work_area(rect(100, 50, 1200, 800), MIN, &[LAPTOP]),
            WorkAreaFit::Keep
        );
        assert_eq!(
            fit_into_work_area(LAPTOP, MIN, &[LAPTOP]),
            WorkAreaFit::Keep
        );
    }

    #[test]
    fn the_configured_size_at_125_percent_shrinks_to_the_work_area_and_is_centred() {
        // 1440 × 900 logical = 1800 × 1125 physical, centred on the full screen: top above it.
        assert_eq!(
            fit_into_work_area(rect(60, -22, 1800, 1125), MIN, &[LAPTOP]),
            WorkAreaFit::Place(rect(60, 0, 1800, 1032))
        );
    }

    #[test]
    fn a_window_that_only_sticks_out_moves_in_at_its_size() {
        assert_eq!(
            fit_into_work_area(rect(1500, -62, 1000, 800), MIN, &[LAPTOP]),
            WorkAreaFit::Place(rect(920, 0, 1000, 800))
        );
        assert_eq!(
            fit_into_work_area(rect(-300, 600, 1000, 800), MIN, &[LAPTOP]),
            WorkAreaFit::Place(rect(0, 232, 1000, 800))
        );
    }

    #[test]
    fn a_geometry_from_a_monitor_that_is_gone_comes_back_on_the_first_one() {
        assert_eq!(
            fit_into_work_area(rect(2600, 100, 1200, 800), MIN, &[LAPTOP]),
            WorkAreaFit::Place(rect(720, 100, 1200, 800))
        );
    }

    #[test]
    fn the_monitor_the_window_overlaps_most_is_the_one_it_is_fitted_to() {
        let left = rect(-2560, 0, 2560, 1400);
        assert_eq!(
            fit_into_work_area(rect(-1500, 900, 1400, 800), MIN, &[LAPTOP, left]),
            WorkAreaFit::Place(rect(-1500, 600, 1400, 800))
        );
    }

    #[test]
    fn a_work_area_smaller_than_the_minimum_size_maximizes() {
        let small = rect(0, 0, 1280, 672);
        assert_eq!(
            fit_into_work_area(rect(0, 0, 1800, 1125), MIN, &[small]),
            WorkAreaFit::Maximize(small)
        );
    }

    #[test]
    fn no_monitor_known_changes_nothing() {
        assert_eq!(
            fit_into_work_area(rect(0, -62, 1800, 1125), MIN, &[]),
            WorkAreaFit::Keep
        );
    }
}
