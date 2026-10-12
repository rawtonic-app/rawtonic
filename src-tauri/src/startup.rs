//! Start-up services: logging, window placement, the frontend-ready handshake
//! and the GPU crash check. Moved out of `lib.rs`.
use crate::app_state::AppState;
use crate::app_state::WindowState;
use crate::launch_request::LaunchPayload;
use std::fs;
use std::panic;
use tauri::Manager;

pub(crate) fn setup_logging(app_handle: &tauri::AppHandle) {
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

    let log_file_path = log_dir.join("app.log");

    let log_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&log_file_path)
        .ok();

    let var = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let level: log::LevelFilter = var.parse().unwrap_or(log::LevelFilter::Info);

    let mut dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(level)
        .chain(std::io::stderr());

    if let Some(file) = log_file {
        dispatch = dispatch.chain(file);
    } else {
        eprintln!(
            "Failed to open log file at {:?}. Logging to console only.",
            log_file_path
        );
    }

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct MonitorBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

pub(crate) fn saved_window_state_is_usable(
    state: &WindowState,
    monitors: &[MonitorBounds],
) -> bool {
    if state.width < 800 || state.height < 600 {
        return false;
    }

    if monitors.is_empty() {
        return true;
    }

    let window_left = state.x as i64;
    let window_top = state.y as i64;
    let window_right = window_left + state.width as i64;
    let window_bottom = window_top + state.height as i64;

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

#[tauri::command]
pub(crate) fn frontend_ready(
    app_handle: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<AppState>,
) -> Result<LaunchPayload, String> {
    let is_first_run = !state
        .window_setup_complete
        .swap(true, std::sync::atomic::Ordering::Relaxed);

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
        #[cfg(any(windows, target_os = "linux"))]
        if is_first_run {
            if should_maximize {
                let _ = window.maximize();
            }
        }
    }

    let open_with_file = state.initial_file_path.lock().unwrap().take();
    let edit_session = state.pending_edit_session.lock().unwrap().take();
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

#[derive(Debug, PartialEq)]
pub(crate) enum GpuCrashCheck {
    Clean,
    /// The flag belongs to a process that is still running (a concurrent
    /// launch or headless export), not to a crashed one.
    InProgressElsewhere,
    Crashed {
        fall_back_to_gl: bool,
    },
}

/// wgpu's OpenGL backend is not compiled in on Apple platforms (it would
/// need ANGLE), so falling back to it there leaves no GPU at all.
pub(crate) const GL_BACKEND_COMPILED: bool = !cfg!(any(target_os = "macos", target_os = "ios"));

/// Reads the GPU-initialisation flag left by an earlier run.
pub(crate) fn gpu_crash_check(flag: &std::path::Path, backend: Option<&str>) -> GpuCrashCheck {
    let Ok(contents) = std::fs::read_to_string(flag) else {
        return GpuCrashCheck::Clean;
    };
    let owner = contents.trim().parse::<u32>().ok();
    if owner.is_some_and(|pid| pid != std::process::id() && process_alive(pid)) {
        return GpuCrashCheck::InProgressElsewhere;
    }
    GpuCrashCheck::Crashed {
        fall_back_to_gl: GL_BACKEND_COMPILED && backend != Some("gl"),
    }
}

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

#[cfg(test)]
mod gpu_crash_flag_tests {
    use super::{GL_BACKEND_COMPILED, GpuCrashCheck, gpu_crash_check};

    #[test]
    fn a_flag_of_a_running_process_is_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("rawtonic-gpu-flag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flag = dir.join(".gpu_init_crash_flag");
        assert_eq!(gpu_crash_check(&flag, None), GpuCrashCheck::Clean);
        // PID 1 (launchd/init) is always running.
        std::fs::write(&flag, "1").unwrap();
        assert_eq!(
            gpu_crash_check(&flag, None),
            GpuCrashCheck::InProgressElsewhere
        );
        // A legacy flag or a dead owner is a crash; OpenGL only where built.
        std::fs::write(&flag, "initializing_gpu").unwrap();
        assert_eq!(
            gpu_crash_check(&flag, Some("auto")),
            GpuCrashCheck::Crashed {
                fall_back_to_gl: GL_BACKEND_COMPILED
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
