//! Start-up services: logging, window placement, the frontend-ready handshake
//! and the GPU crash check. Moved out of `lib.rs`.
use crate::app_state::AppState;
use crate::app_state::WindowState;
use crate::launch_request::LaunchPayload;
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
        if is_first_run && should_maximize {
            let _ = window.maximize();
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
    // GTK takes the first backend it can open from GDK_BACKEND.
    let wayland = match env("GDK_BACKEND").and_then(|list| {
        list.split(',')
            .map(str::trim)
            .find(|b| !b.is_empty() && *b != "*")
            .map(str::to_owned)
    }) {
        Some(backend) => backend == "wayland",
        None => env("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()),
    };
    let explicit_sync = nvidia_driver
        .and_then(|version| version.trim().split('.').next()?.parse::<u32>().ok())
        .is_some_and(|major| major >= 555);
    wayland && explicit_sync
}

#[cfg(test)]
mod window_scale_tests {
    use super::linux_restored_window_size;

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
    use super::{GL_BACKEND_COMPILED, GpuCrashCheck, gpu_crash_check, process_alive};

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
        assert_eq!(gpu_crash_check(&flag, None), GpuCrashCheck::Clean);
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
        std::fs::write(&flag, pid.to_string()).unwrap();
        assert_eq!(
            gpu_crash_check(&flag, None),
            GpuCrashCheck::InProgressElsewhere
        );
        drop(child.0.stdin.take());
        child.0.wait().unwrap();
        assert!(!process_alive(pid));
        assert_eq!(
            gpu_crash_check(&flag, None),
            GpuCrashCheck::Crashed {
                fall_back_to_gl: GL_BACKEND_COMPILED,
            }
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
