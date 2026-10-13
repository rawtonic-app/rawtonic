#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use mimalloc::MiMalloc;

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod adjustment_utils;
mod ai_commands;
mod ai_connector;
mod ai_denoise;
mod ai_faces;
mod ai_lifecycle;
mod ai_processing;
mod app_quit;
mod app_settings;
mod app_state;
mod apple_raw;
mod auto_white_balance;
#[cfg(any(target_os = "linux", test))]
mod browser_preview_transport;
mod bw_auto;
mod cache_utils;
mod camera_profile;
mod camera_tethering;
mod collage;
mod costyle;
mod culling;
mod editor_preview;
mod embedded_lens;
#[cfg(test)]
mod engine_tools;
mod exif_processing;
mod export_processing;
mod export_queue;
mod file_management;
mod file_operations;
mod gpu_processing;
mod guided_perspective;
mod hot_folder;
mod identity_migration;
mod image_loader;
mod image_processing;
mod inpainting;
mod launch_request;
mod legal;
mod lens_correction;
mod library_import;
mod library_listing;
mod lut_processing;
mod mask_generation;
mod memory_pressure;
mod merges;
mod multi_view;
mod multi_view_prefetch;
mod negative_conversion;
#[cfg(feature = "performance-profiling")]
mod performance;
mod preset_converter;
mod preview_cache;
mod preview_proxy;
mod proxy_worker;
#[cfg(feature = "qa-control")]
mod qa_control;
mod session_bridge;
mod spot_removal;
mod startup;
mod tagging;
mod tagging_utils;
mod upright;
mod window_customizer;
mod window_state;

// Engine layers extracted from this crate (ADR 0009). Re-exported under
// their former module names so `crate::<module>` paths keep resolving.
use rawtonic_engine::{auto_adjust, baseline_exposure, formats, lens_blur};
use rawtonic_rawdecode::{gpu_demosaic, raw_processing};
use rawtonic_runtime::allocator;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::{self};

use std::sync::{Arc, Mutex};

use tauri::Manager;
use tokio::sync::Mutex as TokioMutex;

use crate::app_settings::{AppSettings, load_settings};
use crate::app_state::{
    AppState, MetadataManager, ThumbnailManager, ThumbnailProgressTracker, WindowState,
};
use crate::cache_utils::DecodedImageCache;
use crate::image_processing::get_or_init_gpu_context;
use crate::launch_request::{LaunchRequest, emit_launch_request, parse_launch_args};
use crate::window_customizer::PinchZoomDisablePlugin;

#[cfg(target_os = "macos")]
extern "C" fn force_exit(_signal: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

#[cfg(target_os = "macos")]
pub fn register_exit_handler() {
    unsafe {
        libc::signal(libc::SIGABRT, force_exit as *const () as libc::sighandler_t);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn register_exit_handler() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(feature = "performance-profiling")]
    performance::mark_application_start();
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(8 * 1024 * 1024)
        .build_global();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch_req = parse_launch_args(&args);
    if let LaunchRequest::InvalidHeadless(error) = &launch_req {
        eprintln!("Headless export failed: {}", error);
        std::process::exit(2);
    }
    let is_headless = matches!(launch_req, LaunchRequest::HeadlessExport(_));

    let mut builder = tauri::Builder::default();

    #[cfg(target_os = "linux")]
    {
        builder = builder.register_asynchronous_uri_scheme_protocol(
            "rawtonic-preview",
            |_context, request, responder| {
                browser_preview_transport::serve(request, responder);
            },
        );
        if !is_headless {
            builder = builder.plugin(tauri_plugin_wayland_nvidia_quirk::init());
        }
    }

    #[cfg(target_os = "macos")]
    if !is_headless {
        builder = builder
            .menu(app_quit::menu)
            .on_menu_event(app_quit::on_menu_event);
    }

    {
        if !is_headless {
            builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
                log::info!(
                    "New instance launched with args: {:?}. Focusing main window.",
                    argv
                );
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(e) = window.unminimize() {
                        log::error!("Failed to unminimize window: {}", e);
                    }
                    if let Err(e) = window.set_focus() {
                        log::error!("Failed to set focus on window: {}", e);
                    }
                }

                let forwarded_args = argv.get(1..).unwrap_or(&[]);
                emit_launch_request(app, parse_launch_args(forwarded_args));
            }));
        }
    }

    builder
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(PinchZoomDisablePlugin)
        .on_window_event(|window, event| {
            // Closing the main window quits once the editor saved (app_quit.rs).
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && app_quit::intercept_close(window.app_handle())
            {
                api.prevent_close();
                return;
            }
            #[cfg(all(feature = "performance-profiling", target_os = "macos"))]
            if matches!(event, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::ScaleFactorChanged { .. })
                && let Ok(ns_window) = window.ns_window()
            {
                performance::log_display_refresh(ns_window);
            }
            let tauri::WindowEvent::Resized(size) = event else { return };
            let state = window.state::<AppState>();
            if let Some(ctx) = state.gpu_context.lock().unwrap().as_ref()
                && let Ok(mut display_lock) = ctx.display.try_lock()
                    && let Some(display) = display_lock.as_mut() {
                        display.resize(&ctx.device, &ctx.queue, size.width, size.height);
                    }
        })
        .setup(move |app| {
            // Before anything reads settings or user content (see the module).
            let carried_over = identity_migration::carry_over(app.handle());
            #[cfg(feature = "tethering")]
            {
                std::thread::spawn(|| {
                    match gphoto2::Context::new() {
                        Ok(context) => {
                            log::info!("gphoto2 context initialized successfully.");
                            match gphoto2::Camera::autodetect(&context) {
                                Ok(cameras) => {
                                    log::info!("Found {} attached camera(s)", cameras.len());
                                }
                                Err(e) => log::warn!("Failed to autodetect cameras: {}", e),
                            }
                        }
                        Err(e) => log::error!("Failed to initialize gphoto2 context: {}", e),
                    }
                });
            }

            let state = app.state::<AppState>();

            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            {
                match launch_req.clone() {
                    LaunchRequest::EditSession(session) => {
                        log::info!("Initial launch with external edit session for: {}", session.source);
                        *state.pending_edit_session.lock().unwrap() = Some(session);
                    }
                    LaunchRequest::OpenFile(path) => {
                        log::info!("Initial open: Storing path {} for later.", path);
                        *state.initial_file_path.lock().unwrap() = Some(path);
                    }
                    _ => {}
                }
            }

            let app_handle = app.handle().clone();

            if let Ok(cache_dir) = app_handle.path().app_cache_dir() {
                preview_proxy::set_outside_sessions_dir(cache_dir.join("previews"));
                crate::exif_processing::initialize_cache_dir(cache_dir);
            }

            {
                let disks_app_handle = app_handle.clone();
                std::thread::spawn(move || {
                    let disks = sysinfo::Disks::new_with_refreshed_list();
                    let state = disks_app_handle.state::<AppState>();
                    *state.disks_cache.lock().unwrap() = Some(disks);
                });
            }

            let config_dir = app_handle.path().app_config_dir().expect("Failed to get config dir");
            let crash_flag_path = config_dir.join(".gpu_init_crash_flag");

            {
                let state = app.state::<AppState>();
                *state.gpu_crash_flag_path.lock().unwrap() = Some(crash_flag_path.clone());
            }

            let mut settings: AppSettings = load_settings(app_handle.clone()).unwrap_or_default();

            {
                let state = app.state::<AppState>();
                let cache_size = settings.image_cache_size.unwrap_or(5) as usize;
                state.decoded_image_cache.lock().unwrap().set_capacity(cache_size);
            }

            match startup::gpu_crash_check(&crash_flag_path, settings.processing_backend.as_deref()) {
                startup::GpuCrashCheck::Clean => {}
                startup::GpuCrashCheck::InProgressElsewhere => log::info!(
                    "Another Rawtonic process is initialising the GPU; not treated as a crash."
                ),
                startup::GpuCrashCheck::Crashed { fall_back_to_gl } => {
                    let _ = std::fs::remove_file(&crash_flag_path);
                    if fall_back_to_gl {
                        log::warn!(
                            "GPU Driver crash detected on last run! Falling back to OpenGL backend."
                        );
                        settings.processing_backend = Some("gl".to_string());
                        let _ = crate::app_settings::save_settings(settings.clone(), app_handle.clone());
                    } else {
                        log::warn!(
                            "GPU initialisation did not finish on the last run; OpenGL is not available in this build, keeping the backend."
                        );
                    }
                }
            }
            // Earlier builds switched to OpenGL after a supposed crash (also
            // when two processes started together); without OpenGL in the
            // build that left no GPU at all. Undo it.
            if !startup::GL_BACKEND_COMPILED && settings.processing_backend.as_deref() == Some("gl") {
                log::warn!("Processing backend 'gl' is not available in this build; using auto.");
                settings.processing_backend = Some("auto".to_string());
                let _ = crate::app_settings::save_settings(settings.clone(), app_handle.clone());
            }

            // Parsed off the start-up path; users of the database wait for it
            // (`global_lens_db`).
            lens_correction::mark_lens_db_loading();
            {
                let app_handle = app_handle.clone();
                std::thread::spawn(move || {
                    let lens_db = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        lens_correction::load_lensfun_db(&app_handle)
                    }))
                    .unwrap_or_else(|_| {
                        log::error!("Loading the Lensfun database failed; lens profiles are unavailable.");
                        lens_correction::LensDatabase { cameras: Vec::new(), lenses: Vec::new() }
                    });
                    let lens_db = Arc::new(lens_db);
                    lens_correction::set_global_lens_db(lens_db.clone());
                    *app_handle.state::<AppState>().lens_db.lock().unwrap() = Some(lens_db);
                });
            }

            unsafe {
                if let Some(backend) = &settings.processing_backend
                    && backend != "auto" {
                        std::env::set_var("WGPU_BACKEND", backend);
                    }

                #[cfg(target_os = "linux")]
                {
                    if settings.linux_gpu_optimization.unwrap_or(false) {
                        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
                        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
                        std::env::set_var("NODEVICE_SELECT", "1");
                    }
                }

                {
                    let resource_path = app_handle
                        .path()
                        .resolve("resources", tauri::path::BaseDirectory::Resource)
                        .expect("failed to resolve resource directory");

                    let ort_library_name = {
                        #[cfg(target_os = "windows")]
                        { "onnxruntime.dll" }
                        #[cfg(target_os = "linux")]
                        { "libonnxruntime.so" }
                        #[cfg(target_os = "macos")]
                        { "libonnxruntime.dylib" }
                        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
                        { "libonnxruntime.so" }
                    };
                    let ort_library_path = resource_path.join(ort_library_name);
                    std::env::set_var("ORT_DYLIB_PATH", &ort_library_path);
                    println!("Set ORT_DYLIB_PATH to: {}", ort_library_path.display());
                    ai_processing::configure_acceleration(
                        &app_handle,
                        &resource_path,
                        settings.ai_gpu.unwrap_or(true),
                    );
                }
            }

            startup::setup_logging(&app_handle);
            for message in &carried_over.messages {
                log::info!("{message}");
            }
            #[cfg(feature = "qa-control")]
            if !is_headless {
                qa_control::start(&app_handle);
            }
            #[cfg(target_os = "macos")]
            if !is_headless {
                // Dock Quit and log out wait for pending saves too.
                app_quit::hold_system_terminate(&app_handle);
            }
            if !is_headless {
                // Masking models: unloaded when idle or short of memory.
                ai_lifecycle::start(&app_handle);
                // Roll back file operations a crash interrupted.
                file_operations::recover(&app_handle);
                export_queue::open(&app_handle);
            }

            if let Some(backend) = &settings.processing_backend
                && backend != "auto" {
                    log::info!("Applied processing backend setting: {}", backend);
                }

            #[cfg(target_os = "linux")]
            if settings.linux_gpu_optimization.unwrap_or(false) {
                log::info!("Applied Linux Compatibility Mode (forced software compositing).");
            } else {
                log::info!(
                    "Wayland Nvidia quirk status: {:?}",
                    tauri_plugin_wayland_nvidia_quirk::status()
                );
            }

            match launch_req {
                LaunchRequest::HeadlessExport(session) => {
                    let app_handle_clone = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        match crate::export_processing::run_headless_export(session, app_handle_clone.clone()).await {
                            Ok(_) => {
                                println!("Headless export completed successfully.");
                                app_handle_clone.exit(0);
                            }
                            Err(e) => {
                                eprintln!("Headless export failed: {}", e);
                                app_handle_clone.exit(1);
                            }
                        }
                    });

                    return Ok(());
                }
                LaunchRequest::InvalidHeadless(_) => unreachable!("invalid headless arguments exit before app setup"),
                _ => {}
            }

            editor_preview::start_preview_worker(app_handle.clone());
            editor_preview::start_analytics_worker(app_handle.clone());
            file_management::start_thumbnail_workers(app_handle.clone());
            file_management::start_metadata_workers(app_handle.clone());
            session_bridge::start_mount_watch(app_handle.clone());
            jxl_oxide::integration::register_image_decoding_hook();

            let window_cfg = app.config().app.windows.first().unwrap().clone();
            let decorations = settings.decorations.unwrap_or(window_cfg.decorations);
            #[cfg(target_os = "macos")]
            let _ = decorations;

            let main_window_cfg = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .expect("Main window config not found")
                .clone();

            let mut window_builder =
                tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window_cfg)
                    .unwrap();

            #[cfg(not(target_os = "macos"))]
            {
                window_builder = window_builder.decorations(decorations).visible(false);
            }

            #[cfg(target_os = "macos")]
            {
                // AppKit owns the window controls; the 42px session bar fills the
                // transparent titlebar. Keep its 92px CSS inset in sync with these controls.
                window_builder = window_builder
                    .decorations(true)
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true)
                    .traffic_light_position(tauri::LogicalPosition::new(14.0, 21.0))
                    .visible(false);
            }

            let window = window_builder.build().expect("Failed to build window");
            #[cfg(all(feature = "performance-profiling", target_os = "macos"))]
            if let Ok(ns_window) = window.ns_window() {
                performance::log_display_refresh(ns_window);
            }

            {
                let app_state = app.state::<AppState>();
                if let Err(error) = get_or_init_gpu_context(&app_state, app.handle()) {
                    log::warn!(
                        "GPU pre-initialization failed (editing and thumbnails may be degraded): {}",
                        error
                    );
                }

                if let Ok(config_dir) = app.path().app_config_dir() {
                    let path = config_dir.join("window_state.json");
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        if let Ok(state) = serde_json::from_str::<WindowState>(&contents) {
                            let monitor_bounds = startup::available_monitor_bounds(&window);
                            if startup::saved_window_state_is_usable(&state, &monitor_bounds) {
                                #[cfg(target_os = "linux")]
                                let size = startup::linux_restored_window_size(
                                    state.width,
                                    state.height,
                                    state.scale_factor,
                                )
                                .unwrap_or_else(|| {
                                    // Neither GTK nor its monitor reports the final
                                    // scale before Wayland maps the window. Migrate
                                    // older files once using the configured size.
                                    tauri::Size::Logical(tauri::LogicalSize::new(
                                        main_window_cfg.width,
                                        main_window_cfg.height,
                                    ))
                                });
                                #[cfg(not(target_os = "linux"))]
                                let size = tauri::Size::Physical(tauri::PhysicalSize::new(
                                    state.width,
                                    state.height,
                                ));
                                let _ = window.set_size(size);
                                let _ = window.set_position(tauri::Position::Physical(
                                    tauri::PhysicalPosition::new(state.x, state.y),
                                ));
                            } else {
                                log::warn!(
                                    "Saved window state was unusable ({}x{} at {},{}), centering instead.",
                                    state.width,
                                    state.height,
                                    state.x,
                                    state.y
                                );
                                let _ = window.center();
                            }
                        } else {
                            let _ = window.center();
                        }
                    } else {
                        let _ = window.center();
                    }
                } else {
                    let _ = window.center();
                }

                let window_failsafe = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    if let Ok(false) = window_failsafe.is_visible() {
                        log::warn!(
                            "Frontend failed to report ready within timeout. Forcing window visibility."
                        );
                        let _ = window_failsafe.show();
                        let _ = window_failsafe.set_focus();
                    }
                });

                // Written while the window changes and on quitting (window_state.rs).
                let window_state_path = app
                    .path()
                    .app_config_dir()
                    .ok()
                    .map(|dir| dir.join("window_state.json"));
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(window_state::INTERVAL).await;
                        window_state::flush();
                    }
                });

                let window_for_handler = window.clone();

                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                        #[cfg(any(windows, target_os = "linux"))]
                        let maximized = window_for_handler.is_maximized().unwrap_or(false);
                        #[cfg(not(any(windows, target_os = "linux")))]
                        let maximized = false;

                        // Full screen is the photo-only view (F), which a
                        // start does not return to: the window keeps the
                        // size and place it had before.
                        if window_for_handler.is_minimized().unwrap_or(false)
                            || window_for_handler.is_fullscreen().unwrap_or(false)
                        {
                            return;
                        }

                        let mut state = WindowState {
                            width: 1280,
                            height: 720,
                            x: 0,
                            y: 0,
                            maximized,
                            scale_factor: window_for_handler.scale_factor().ok(),
                            fullscreen: false,
                        };

                        if let Ok(position) = window_for_handler.outer_position() {
                            state.x = position.x;
                            state.y = position.y;
                        }

                        if !maximized
                            && let Ok(size) = window_for_handler.outer_size()
                            && size.width >= 800
                            && size.height >= 600
                        {
                            state.width = size.width;
                            state.height = size.height;
                        }

                        if let Some(path) = &window_state_path {
                            window_state::note(path.clone(), state);
                        }
                    }
                    _ => {}
                });
            }

            crate::register_exit_handler();
            Ok(())
        })
        .manage(AppState {
            window_setup_complete: AtomicBool::new(false),
            gpu_crash_flag_path: Mutex::new(None),
            original_image: Mutex::new(None),
            proxy_image: Mutex::new(None),
            proxy_memory: Mutex::new(Default::default()),
            offline_source: Mutex::new(None),
            current_source: Mutex::new(String::new()),
            last_preview_dim: std::sync::atomic::AtomicU32::new(0),
            cached_preview: Mutex::new(None),
            reusable_previews: Mutex::new(Default::default()),
            gpu_context: Mutex::new(None),
            gpu_image_cache: Mutex::new(None),
            gpu_processor: Mutex::new(None),
            ai_state: Mutex::new(None),
            ai_init_lock: TokioMutex::new(()),
            ai_denoiser: Mutex::new(None),
            faces: Mutex::new(Default::default()),
            active_ai_tasks: Mutex::new(HashMap::new()),
            export_task_token: Arc::new(Mutex::new(None)),
            merge_result: Arc::new(Mutex::new(None)),
            indexing_task_handle: Mutex::new(None),
            lut_cache: Mutex::new(HashMap::new()),
            initial_file_path: Mutex::new(None),
            pending_edit_session: Mutex::new(None),
            thumbnail_cancellation_token: Arc::new(AtomicBool::new(false)),
            thumbnail_progress: Mutex::new(ThumbnailProgressTracker { total: 0, completed: 0 }),
            preview_worker_tx: Mutex::new(None),
            analytics_worker_tx: Mutex::new(None),
            mask_cache: Mutex::new(Default::default()),
            mask_thumbnail_cache: Mutex::new(Default::default()),
            patch_cache: Mutex::new(HashMap::new()),
            geometry_cache: Mutex::new(HashMap::new()),
            thumbnail_geometry_cache: Mutex::new(HashMap::new()),
            lens_db: Mutex::new(None),
            load_image_generation: Arc::new(AtomicUsize::new(0)),
            preview_generation: AtomicUsize::new(0),
            full_warped_cache: Mutex::new(None),
            patched_warped_cache: Mutex::new(None),
            full_transformed_cache: Mutex::new(None),
            decoded_image_cache: Mutex::new(DecodedImageCache::new(5)),
            thumbnail_manager: ThumbnailManager::new(),
            metadata_manager: MetadataManager::new(),
            disks_cache: Mutex::new(None),
            disks_cache_refreshing: AtomicBool::new(false),
            camera_session: Mutex::new(camera_tethering::CameraSession::new()),
        })
        .invoke_handler(tauri::generate_handler![
            session_bridge::session_create,
            session_bridge::library_copy,
            session_bridge::session_open,
            session_bridge::session_last,
            session_bridge::session_summary,
            session_bridge::session_ingest,
            session_bridge::session_ingest_scan,
            session_bridge::session_ingest_thumbnail,
            session_bridge::session_ingest_preview,
            session_bridge::session_ingest_capture_info,
            session_bridge::session_cancel_ingest,
            session_bridge::session_set_flag,
            session_bridge::session_last_directory,
            session_bridge::session_text_metadata,
            session_bridge::session_all_text_metadata,
            session_bridge::session_set_text_metadata,
            session_bridge::session_albums,
            session_bridge::session_create_album,
            session_bridge::session_move_album,
            session_bridge::session_rename_album,
            session_bridge::session_delete_album,
            session_bridge::session_album_add,
            session_bridge::session_album_remove,
            session_bridge::session_album_paths,
            session_bridge::session_offline,
            session_bridge::session_relink,
            session_bridge::session_relink_folder,
            session_bridge::catalog_folders,
            session_bridge::session_flags,
            editor_preview::apply_adjustments,
            gpu_processing::sample_preview_color,
            gpu_processing::sample_preview_white_balance,
            gpu_processing::sample_preview_grey_world,
            editor_preview::generate_preview_for_path,
            multi_view::render_browser_view,
            editor_preview::generate_uncropped_preview,
            startup::get_log_file_path,
            startup::frontend_log,
            collage::save_collage,
            merges::merge_photos,
            merges::save_merge,
            merges::refinish_merge,
            merges::discard_merge,
            app_quit::quit_saving,
            app_quit::finish_quit,
            app_quit::cancel_quit,
            lut_processing::load_and_parse_lut,
            camera_profile::import_camera_profile,
            camera_profile::list_camera_profiles,
            camera_profile::white_balance_info,
            camera_profile::white_balance_from_sample,
            bw_auto::auto_gray_mix,
            preset_converter::place_lightroom_masks,
            spot_removal::find_spot_source,
            spot_removal::visualize_spots,
            auto_white_balance::auto_white_balance_for,
            lut_processing::list_luts,
            legal::third_party_licenses,
            lut_processing::import_luts,
            lut_processing::remove_lut,
            startup::frontend_ready,
            editor_preview::cancel_thumbnail_generation,
            editor_preview::update_wgpu_transform,
            editor_preview::render_split_before,
            cache_utils::clear_image_caches,
            app_settings::load_settings,
            app_settings::save_settings,
            app_settings::is_tethering_supported,
            ai_commands::generate_ai_depth_mask,
            file_management::delete_files_from_disk,
            file_management::delete_files_with_associated,
            ai_commands::generate_ai_subject_mask,
            ai_commands::precompute_ai_subject_mask,
            ai_commands::precompute_ai_foreground_mask,
            ai_commands::precompute_ai_sky_mask,
            ai_commands::preload_ai_mask_models,
            ai_lifecycle::unload_ai_models,
            ai_lifecycle::note_ai_mask_use,
            ai_commands::generate_ai_foreground_mask,
            ai_commands::generate_ai_sky_mask,
            ai_commands::generate_full_image_depth_map,
            ai_commands::generate_relight_depth,
            ai_commands::load_relight_pattern,
            ai_commands::cancel_ai_task,
            ai_faces::detect_people,
            ai_faces::generate_ai_people_mask,
            ai_faces::analyze_face_retouch,
            ai_faces::apply_face_retouch_to_paths,
            inpainting::invoke_generative_replace_with_mask_def,
            image_loader::load_image,
            image_loader::apply_ai_denoise,
            image_loader::load_embedded_preview,
            export_processing::export_images,
            export_processing::cancel_export,
            export_processing::estimate_export_sizes,
            export_processing::list_font_families,
            image_processing::calculate_auto_adjustments,
            mask_generation::generate_mask_overlay,
            mask_generation::sample_luma_range,
            mask_generation::generate_mask_thumbnails,
            file_management::update_exif_fields,
            file_management::file_size,
            file_management::get_supported_file_types,
            file_management::read_exif_for_paths,
            library_listing::begin_image_listing,
            library_listing::next_image_listing_page,
            library_listing::cancel_image_listing,
            file_management::get_folder_tree,
            file_management::get_folder_children,
            file_management::get_pinned_folder_trees,
            file_management::update_thumbnail_queue,
            file_management::create_folder,
            file_management::delete_folder,
            file_operations::undo_file_operation,
            export_queue::export_queue_jobs,
            hot_folder::hot_folder_start,
            library_import::library_import_preview,
            library_import::library_import,
            hot_folder::hot_folder_stop,
            hot_folder::hot_folder_status,
            export_queue::export_queue_resume,
            export_queue::export_queue_remove,
            file_operations::last_file_operation,
            file_management::copy_files,
            file_management::move_files,
            file_management::rename_folder,
            file_management::rename_files,
            file_management::duplicate_file,
            file_management::show_in_finder,
            file_management::save_metadata_and_update_thumbnail,
            file_management::apply_adjustments_to_paths,
            file_management::load_metadata,
            file_management::load_presets,
            file_management::save_presets,
            file_management::reset_adjustments_for_paths,
            file_management::apply_auto_lens_correction_to_paths,
            file_management::apply_auto_adjustments_to_paths,
            file_management::handle_import_presets_from_file,
            file_management::handle_import_legacy_presets_from_file,
            file_management::handle_import_presets_from_files,
            file_management::handle_export_presets_to_file,
            file_management::clear_all_sidecars,
            file_management::clear_thumbnail_cache,
            file_management::set_color_label_for_paths,
            file_management::set_rating_for_paths,
            file_management::import_files,
            file_management::create_virtual_copy,
            file_management::get_albums,
            file_management::save_albums,
            file_management::add_to_album,
            file_management::get_album_images,
            tagging::start_background_indexing,
            tagging::clear_ai_tags,
            tagging::clear_all_tags,
            tagging::add_tag_for_paths,
            tagging::remove_tag_for_paths,
            culling::cull_images,
            lens_correction::get_lensfun_makers,
            embedded_lens::get_camera_lens_profile,
            upright::calculate_upright,
            image_loader::load_image_proxy,
            editor_preview::activate_photo,
            multi_view::set_multi_view,
            multi_view::set_multi_view_zoom,
            multi_view::set_multi_view_live,
            proxy_worker::prepare_preview_proxies,
            proxy_worker::build_previews,
            proxy_worker::stop_preview_build,
            proxy_worker::preview_build_progress,
            proxy_worker::discard_previews,
            proxy_worker::preview_cache_usage,
            proxy_worker::clear_preview_cache,
            proxy_worker::apply_preview_limits,
            lens_correction::get_lensfun_lenses_for_maker,
            lens_correction::autodetect_lens,
            lens_correction::get_lens_distortion_params,
            negative_conversion::preview_negative_conversion,
            negative_conversion::convert_negatives,
            camera_tethering::tether_list_cameras,
            camera_tethering::tether_connect,
            camera_tethering::tether_get_settings,
            camera_tethering::tether_set_setting,
            camera_tethering::tether_capture,
            camera_tethering::tether_get_preview,
            camera_tethering::tether_autofocus,
            guided_perspective::calculate_guided_perspective,
            guided_perspective::calculate_manual_transform,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(#[allow(unused_variables)] |app_handle, event| {
            match event {
                #[cfg(target_os = "macos")]
				tauri::RunEvent::Opened { urls } => {
				    if let Some(url) = urls.first()
				        && let Ok(path) = url.to_file_path()
				        && let Some(path_str) = path.to_str()
				    {
				        let state = app_handle.state::<AppState>();

				        if state
				            .window_setup_complete
				            .load(std::sync::atomic::Ordering::Relaxed)
				        {
				            if let Some(window) = app_handle.get_webview_window("main") {
				                let _ = window.unminimize();
				                let _ = window.show();
				                let _ = window.set_focus();
				            }

				            log::info!("macOS runtime open: Opening {}.", path_str);

				            emit_launch_request(
				                app_handle,
				                LaunchRequest::OpenFile(path_str.to_string()),
				            );
				        } else {
				            *state.initial_file_path.lock().unwrap() =
				                Some(path_str.to_string());

				            log::info!(
				                "macOS initial open: Stored path {} for later.",
				                path_str
				            );
				        }
				    }
				}
                // Closing and quitting reach this only after the editor
                // saved its pending edits (app_quit.rs).
                tauri::RunEvent::ExitRequested { api, code, .. } => {
                    api.prevent_exit();
                    let exit_code = code.unwrap_or(0);
                    window_state::flush();

                    // libc exit handlers can tear down the GPU driver while
                    // its resources are still being dropped on worker threads.
                    #[cfg(any(target_os = "macos", target_os = "linux"))]
                    unsafe { libc::_exit(exit_code); }

                    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                    std::process::exit(exit_code);
                }
                tauri::RunEvent::Exit => {
                    window_state::flush();
                    #[cfg(any(target_os = "macos", target_os = "linux"))]
                    unsafe { libc::_exit(0); }

                    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                    std::process::exit(0);
                }
                _ => {}
            }
        });
}
