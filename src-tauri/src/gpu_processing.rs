//! Desktop glue for `rawtonic_gpu::gpu_processing` (ADR 0009): the window
//! surface, the processors and the context kept in `GpuState`, and the Tauri
//! commands stay here; the pipeline lives in the GPU crate.
pub use rawtonic_gpu::gpu_processing::*;

use std::sync::Arc;

use image::DynamicImage;
use tauri::Manager;

use crate::app_state::AppState;
use crate::state::develop::DevelopSession;
use crate::state::gpu::GpuState;
use crate::state::lock::lock_recover;

/// The processing context, created on first use. It is made without the
/// window (the device and the first display variant start before the window
/// exists, `start_early`); `attach_display` adds the window surface.
pub fn get_or_init_gpu_context(
    gpu: &GpuState,
    app_handle: &tauri::AppHandle,
) -> Result<GpuContext, String> {
    let mut context_lock = gpu.context_slot();
    if let Some(context) = &*context_lock {
        return Ok(context.clone());
    }
    let (context, instance, adapter) = create_context(gpu, app_handle, None)?;
    *lock_recover(&gpu.adapter) = Some((instance, adapter));
    *context_lock = Some(context.clone());
    Ok(context)
}

/// Starts the device, the processor and with it the RAW default display
/// variant on their own thread, before the window and its webview exist:
/// on a cold driver shader cache the first Develop frame waits for that
/// compile (11.7 s on NVIDIA/Windows; notes: benchmark-results/
/// windows-gpu-vfio-2026-10-30/COLD-START-OPTIONS.md, B2).
pub fn start_early(app_handle: &tauri::AppHandle) {
    let app_handle = app_handle.clone();
    let spawned = std::thread::Builder::new()
        .name("gpu-start".into())
        .stack_size(rawtonic_runtime::RENDER_STACK_SIZE)
        .spawn(move || {
            let gpu = app_handle.state::<GpuState>();
            match get_or_init_gpu_context(&gpu, &app_handle) {
                // A window whose adapter cannot present replaces the context
                // while it holds the processor slot (`attach_display`): no
                // processor for the replaced device.
                Ok(context) => {
                    // The first processor's compile is part of the start: a
                    // driver that crashes compiling it is left out next time.
                    let flag = gpu.crash_flag_path();
                    let backend = lock_recover(&gpu.adapter)
                        .as_ref()
                        .map(|(_, adapter)| wgpu::Backends::from(adapter.get_info().backend));
                    if let (Some(flag), Some(backend)) = (&flag, backend) {
                        crate::gpu_start_guard::write_flag(flag, backend, "compile");
                        #[cfg(feature = "qa-control")]
                        crate::qa_crash::at("gpu-compile");
                    }
                    prewarm_gpu_processor_if(&gpu.processor, &context, || is_current(&gpu, &context));
                    // Up to the first display variant (at most 30 s).
                    after_first_display_variant();
                    if let Some(flag) = &flag {
                        crate::gpu_start_guard::clear_flag(flag);
                    }
                }
                Err(error) => {
                    log::error!(
                        "GPU pre-initialization failed (editing and thumbnails may be degraded): {}",
                        error
                    );
                    record_start_error(&gpu, &app_handle, error);
                    // Nothing to wait for.
                    first_display_variant_settled();
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("GPU start thread unavailable: {error}");
    }
}

/// Records why the GPU did not start, for the window (`gpu_start_error`):
/// without it no photo can be developed.
fn record_start_error(gpu: &GpuState, app_handle: &tauri::AppHandle, error: String) {
    use tauri::Emitter;
    // Stored first: a window that asks after missing the event finds it.
    gpu.set_start_error(error.clone());
    let _ = app_handle.emit("gpu-start-failed", &error);
}

/// The reason the GPU did not start, if it did not: the window shows it
/// (it may load after the `gpu-start-failed` event).
#[tauri::command]
pub fn gpu_start_error(gpu: tauri::State<'_, GpuState>) -> Option<String> {
    gpu.start_error()
}

/// The processing backend as the window shows it: the one in use, and
/// what the start guard left out after a crash (`gpu_start_guard`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuBackendStatus {
    /// The backend the GPU runs on ("vulkan"), once it started.
    in_use: Option<String>,
    adapter: Option<String>,
    /// "clean", "fellBack", "retrying" or "kept".
    step: crate::gpu_start_guard::Step,
    /// Left out after a crash (comma list), and the newest crash.
    avoided: String,
    crashed: String,
    crashed_on: Option<String>,
    /// What this start tells about it, once.
    notice: Option<crate::gpu_start_guard::Notice>,
}

fn gpu_backend_status_blocking(app_handle: &tauri::AppHandle) -> Result<GpuBackendStatus, String> {
    let gpu = app_handle.state::<GpuState>();
    // Waits (at most 20 s) for the start thread's device or its failure;
    // never starts one itself.
    let waited = std::time::Instant::now();
    while gpu.context().is_none()
        && gpu.start_error().is_none()
        && waited.elapsed() < std::time::Duration::from_secs(20)
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let info = lock_recover(&gpu.adapter)
        .as_ref()
        .map(|(_, adapter)| adapter.get_info());
    let config_dir = app_handle
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    let state =
        crate::gpu_start_guard::read_state(&crate::gpu_start_guard::state_path(&config_dir));
    Ok(GpuBackendStatus {
        in_use: info.as_ref().map(|info| info.backend.to_str().to_string()),
        adapter: info.map(|info| info.name),
        step: state.step,
        avoided: state.avoid,
        crashed: state.crashed,
        crashed_on: state.crashed_on,
        notice: gpu.start_notice(),
    })
}

#[tauri::command]
pub async fn gpu_backend_status(app_handle: tauri::AppHandle) -> Result<GpuBackendStatus, String> {
    tauri::async_runtime::spawn_blocking(move || gpu_backend_status_blocking(&app_handle))
        .await
        .map_err(|e| e.to_string())?
}

/// "Use automatic again": nothing is left out from the next start on (the
/// window sets the preference to Auto and offers the relaunch).
#[tauri::command]
pub async fn gpu_backend_use_automatic(app_handle: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config_dir = app_handle
            .path()
            .app_config_dir()
            .map_err(|e| e.to_string())?;
        let path = crate::gpu_start_guard::state_path(&config_dir);
        let state = crate::gpu_start_guard::GuardState {
            gl_checked: true,
            ..Default::default()
        };
        crate::gpu_start_guard::write_state(&path, &state)?;
        log::info!("GPU backends left out after a crash: cleared (Use automatic again).");
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Resolves once the start's RAW default display variant has compiled (or
/// the GPU did not start): the window shows "Preparing graphics" in the
/// activity centre while it waits, the visible form of the first start's
/// shader compile (G76: no installer compiles ahead any more). `false`
/// when the GPU did not start, or after 15 minutes without the variant.
#[tauri::command]
pub async fn gpu_first_display_ready(app_handle: tauri::AppHandle) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wait_for_first_display_variant(std::time::Duration::from_secs(15 * 60))
            && app_handle.state::<GpuState>().start_error().is_none()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Holds optional start-up work (background library passes, model
/// warm-ups) until the RAW default display variant is compiled, for at most
/// 30 s: a cold start's first Develop frame waits for that compile.
pub fn after_first_display_variant() {
    wait_for_first_display_variant(std::time::Duration::from_secs(30));
}

/// A sharpening view asked for while its variant compiled showed the photo:
/// the editor renders it again once the variant is ready (every preview
/// path, native display or readback).
pub fn notify_sharpen_view_ready(app_handle: &tauri::AppHandle) {
    let app_handle = app_handle.clone();
    rawtonic_gpu::gpu_processing::on_sharpen_view_ready(move || {
        use tauri::Emitter;
        let _ = app_handle.emit("sharpen-view-ready", ());
    });
}

/// Presents onto the main window: its surface on the context's adapter, or,
/// where that adapter cannot present to it, a new context from an adapter
/// that can (the processor of the old one is dropped).
#[cfg(not(target_os = "linux"))]
pub fn attach_display(gpu: &GpuState, app_handle: &tauri::AppHandle) {
    let settings = crate::state::settings::current(app_handle);
    if !settings.use_wgpu_renderer.unwrap_or(true) {
        return;
    }
    let Some(window) = app_handle.get_webview_window("main") else {
        return;
    };
    // Waits for a context the start thread is creating.
    let context = match get_or_init_gpu_context(gpu, app_handle) {
        Ok(context) => context,
        Err(error) => {
            log::warn!(
                "GPU pre-initialization failed (editing and thumbnails may be degraded): {error}"
            );
            return;
        }
    };
    let Some((instance, adapter)) = lock_recover(&gpu.adapter).clone() else {
        return;
    };
    let surface = match instance.create_surface(window.clone()) {
        Ok(surface) => surface,
        Err(error) => {
            log::warn!("Failed to create surface, falling back to compute-only: {error}");
            return;
        }
    };
    if adapter.is_surface_supported(&surface) {
        match configure_display(&context.device, &adapter, surface, &window) {
            Ok(display) => {
                *context.display.lock().unwrap_or_else(|e| e.into_inner()) = Some(display)
            }
            Err(error) => log::warn!("Window display unavailable, compute-only: {error}"),
        }
        return;
    }
    log::warn!("The GPU adapter cannot present to the window; choosing one that can.");
    // No render may use the old processor with the new context.
    let mut processor = gpu.processor.lock().unwrap_or_else(|e| e.into_inner());
    let mut context_lock = gpu.context_lock();
    *processor = None;
    drop(surface);
    match create_context(gpu, app_handle, Some(&window)) {
        Ok((context, instance, adapter)) => {
            *lock_recover(&gpu.adapter) = Some((instance, adapter));
            *context_lock = Some(context.clone());
            let app_handle = app_handle.clone();
            rawtonic_runtime::spawn_render_thread("gpu-prewarm", move || {
                let gpu = app_handle.state::<GpuState>();
                prewarm_gpu_processor_if(&gpu.processor, &context, || is_current(&gpu, &context));
            });
        }
        Err(error) => {
            *context_lock = None;
            log::warn!("GPU initialization with the window failed: {error}");
        }
    }
}

/// Whether `context` is the app's processing context (not one replaced by
/// `attach_display`).
fn is_current(gpu: &GpuState, context: &GpuContext) -> bool {
    gpu.context()
        .is_some_and(|current| Arc::ptr_eq(&current.device, &context.device))
}

/// A device on the high-performance adapter (one that can present to
/// `window` when given), with that window's display. The backend falls back
/// when the device cannot be created (`rawtonic_gpu::device_start`).
fn create_context(
    gpu: &GpuState,
    app_handle: &tauri::AppHandle,
    #[allow(unused_variables)] window: Option<&tauri::WebviewWindow>,
) -> Result<(GpuContext, wgpu::Instance, wgpu::Adapter), String> {
    // The flag names each step and its backends with this process's id: a
    // crash leaves it for the next start (`gpu_start_guard`), and another
    // process starting meanwhile does not take it for one.
    let flag_path = gpu.crash_flag_path();
    let remove_flag = || {
        if let Some(p) = &flag_path {
            crate::gpu_start_guard::clear_flag(p);
        }
    };
    let on_step = |step: rawtonic_gpu::device_start::StartStep| {
        use rawtonic_gpu::device_start::StartStep;
        let Some(p) = &flag_path else { return };
        let (backends, stage) = match step {
            StartStep::Adapter(backends) => (backends, "adapter"),
            StartStep::Device(backend) => (wgpu::Backends::from(backend), "device"),
        };
        crate::gpu_start_guard::write_flag(p, backends, stage);
        #[cfg(feature = "qa-control")]
        crate::qa_crash::at(&format!("gpu-{stage}"));
    };

    let started = rawtonic_gpu::device_start::start_device(
        |instance| {
            #[cfg(not(target_os = "linux"))]
            if let Some(window) = window {
                return instance.create_surface(window.clone()).ok();
            }
            #[cfg(target_os = "linux")]
            let _ = instance;
            None
        },
        gpu.avoid_backends(),
        on_step,
    )
    .inspect_err(|_| remove_flag())?;
    let rawtonic_gpu::device_start::StartedDevice {
        instance,
        adapter,
        surface,
        device,
        queue,
    } = started;

    let info = adapter.get_info();
    log_adapter(&info);
    crate::contribute::note_gpu(&info);
    let limits = adapter.limits();

    device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
        log::error!("[wgpu-error] {}", err);
    }));

    remove_flag();

    if let Ok(cache_dir) = app_handle.path().app_cache_dir() {
        rawtonic_gpu::pipeline_cache::open(
            &device,
            &info,
            &cache_dir.join("gpu-pipelines"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    #[cfg(not(target_os = "linux"))]
    let display_opt = match (surface, window) {
        (Some(surface), Some(window)) => {
            Some(configure_display(&device, &adapter, surface, window)?)
        }
        (None, Some(_)) => {
            log::warn!("Failed to create surface, falling back to compute-only");
            None
        }
        _ => None,
    };

    #[cfg(target_os = "linux")]
    let display_opt = {
        drop(surface);
        None
    };

    // Display frames are encoded in the space the window layer is tagged with.
    let display_space = if cfg!(target_os = "macos") {
        rawtonic_engine::output_color_space::OutputColorSpace::DisplayP3
    } else {
        rawtonic_engine::output_color_space::OutputColorSpace::Srgb
    };
    let new_context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(display_opt)),
        display_space,
    };
    crate::gpu_demosaic::register(new_context.device.clone(), new_context.queue.clone());
    rawtonic_engine::edge_fringe::gpu::register(
        new_context.device.clone(),
        new_context.queue.clone(),
    );
    Ok((new_context, instance, adapter))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn configure_display(
    device: &wgpu::Device,
    adapter: &wgpu::Adapter,
    surface: wgpu::Surface<'static>,
    window: &tauri::WebviewWindow,
) -> Result<WgpuDisplay, String> {
    let swapchain_caps = surface.get_capabilities(adapter);
    let swapchain_format = swapchain_caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(swapchain_caps.formats[0]);

    let alpha_mode = if cfg!(target_os = "windows")
        && swapchain_caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Opaque)
    {
        wgpu::CompositeAlphaMode::Opaque
    } else if swapchain_caps
        .alpha_modes
        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
    {
        wgpu::CompositeAlphaMode::PreMultiplied
    } else if swapchain_caps
        .alpha_modes
        .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
    {
        wgpu::CompositeAlphaMode::PostMultiplied
    } else {
        swapchain_caps.alpha_modes[0]
    };

    // macOS: present without waiting for vsync. Frames are presented once
    // per preview job, not in a loop, and the window server still
    // composites whole frames. With FIFO about one live frame in eleven
    // waited ~26 ms for a free drawable while the previous ones queued for
    // vsync. Elsewhere immediate presentation can tear, so FIFO stays.
    let present_mode = if cfg!(target_os = "macos")
        && swapchain_caps
            .present_modes
            .contains(&wgpu::PresentMode::Immediate)
    {
        wgpu::PresentMode::Immediate
    } else {
        wgpu::PresentMode::Fifo
    };

    let size = window
        .inner_size()
        .unwrap_or(tauri::PhysicalSize::new(1280, 720));
    let config = wgpu::SurfaceConfiguration {
        width: size.width.max(1),
        height: size.height.max(1),
        format: swapchain_format,
        color_space: display_surface_color_space(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        present_mode,
        alpha_mode,
        view_formats: vec![],
        // Three drawables. Two would save one window-sized buffer (about
        // 80 MB at 6K) but made live brush frames wait for a free one.
        desired_maximum_frame_latency: 2,
    };
    surface.configure(device, &config);

    WgpuDisplay::new(device, surface, config)
}

/// Fits the window surface to a window now `width` × `height` device pixels.
/// While a frame or placement holds the display, the resize waits for it on
/// a worker (the main thread must not block) and then takes the newest size:
/// the window's last resize event — the end of a zoom animation — is never
/// dropped, which left the photograph on a surface of an earlier size.
pub fn resize_display(gpu: &GpuState, context: GpuContext, width: u32, height: u32) {
    use std::sync::atomic::Ordering;
    let window_size = Arc::clone(&gpu.window_size);
    window_size.store(
        (u64::from(width) << 32) | u64::from(height),
        Ordering::SeqCst,
    );
    let apply = move |display: &mut Option<WgpuDisplay>, context: &GpuContext| {
        let size = window_size.load(Ordering::SeqCst);
        let (width, height) = ((size >> 32) as u32, size as u32);
        if let Some(display) = display.as_mut()
            && (display.config.width, display.config.height) != (width.max(1), height.max(1))
        {
            display.resize(&context.device, &context.queue, width, height);
        }
    };
    let busy = match context.display.try_lock() {
        Ok(mut display) => {
            apply(&mut display, &context);
            false
        }
        Err(std::sync::TryLockError::Poisoned(e)) => {
            apply(&mut e.into_inner(), &context);
            false
        }
        Err(std::sync::TryLockError::WouldBlock) => true,
    };
    if busy {
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({ "stage": "display_resize_deferred", "size": [width, height] })
        );
        tauri::async_runtime::spawn_blocking(move || {
            let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
            apply(&mut display, &context);
        });
    }
}

/// Builds the processing pipelines ahead of the first render (shader
/// compilation: seconds on a cold driver cache), so an export overlaps it
/// with decoding. A processor that already exists
/// is kept; the render path creates one itself if this has not finished.
pub fn prewarm_gpu_processor(slot: &GpuProcessorSlot, context: &GpuContext) {
    prewarm_gpu_processor_if(slot, context, || true);
}

/// [`prewarm_gpu_processor`] when `current` still holds once the slot is
/// taken.
fn prewarm_gpu_processor_if(
    slot: &GpuProcessorSlot,
    context: &GpuContext,
    current: impl FnOnce() -> bool,
) {
    let mut processor_lock = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if processor_lock.is_some() || !current() {
        return;
    }
    let max_dim = context.limits.max_texture_dimension_2d;
    match create_processor(slot, context, max_dim, max_dim) {
        Ok(processor) => {
            *processor_lock = Some(crate::app_state::GpuProcessorState {
                processor,
                width: max_dim,
                height: max_dim,
            });
        }
        Err(error) => {
            log::warn!("GPU processor prewarm failed: {error}");
            first_display_variant_settled();
        }
    }
}

pub fn process_and_get_dynamic_image(
    context: &GpuContext,
    gpu: &GpuState,
    develop: &DevelopSession,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &gpu.processor,
        &develop.gpu_image_cache,
        RenderSource::Image {
            image: base_image,
            transform_hash,
        },
        request,
        caller_id,
        RenderOutputPrecision::EightBit,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
}

/// CPU readback with the display shader variants; see [`ReadbackShader::Display`].
pub fn process_and_get_dynamic_image_with_display_shader(
    context: &GpuContext,
    gpu: &GpuState,
    develop: &DevelopSession,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &gpu.processor,
        &develop.gpu_image_cache,
        RenderSource::Image {
            image: base_image,
            transform_hash,
        },
        request,
        caller_id,
        RenderOutputPrecision::EightBit,
        false,
        None,
        None,
        ReadbackShader::Display,
    )
}

/// CPU readback with the display arithmetic and the foreground preview's
/// retained intermediates (blurs, masks); see [`ReadbackShader::Preview`].
/// For small samples of the shown preview's base, never for exports.
pub fn process_and_get_dynamic_image_with_preview_shader(
    context: &GpuContext,
    gpu: &GpuState,
    develop: &DevelopSession,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &gpu.processor,
        &develop.gpu_image_cache,
        RenderSource::Image {
            image: base_image,
            transform_hash,
        },
        request,
        caller_id,
        RenderOutputPrecision::EightBit,
        false,
        None,
        None,
        ReadbackShader::Preview,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn process_and_get_dynamic_image_with_precision(
    context: &GpuContext,
    gpu: &GpuState,
    develop: &DevelopSession,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
    output_precision: RenderOutputPrecision,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &gpu.processor,
        &develop.gpu_image_cache,
        RenderSource::Image {
            image: base_image,
            transform_hash,
        },
        request,
        caller_id,
        output_precision,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn process_and_get_dynamic_image_with_analytics(
    context: &GpuContext,
    gpu: &GpuState,
    develop: &DevelopSession,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
    output_to_display: bool,
    analytics_config: Option<crate::app_state::AnalyticsConfig>,
    display_identity: Option<DisplayFrameIdentity>,
) -> Result<DynamicImage, String> {
    let readback_shader = if cfg!(target_os = "linux") && caller_id == "apply_adjustments" {
        ReadbackShader::Preview
    } else {
        ReadbackShader::Export
    };
    process_and_get_dynamic_image_inner(
        context,
        &gpu.processor,
        &develop.gpu_image_cache,
        RenderSource::Image {
            image: base_image,
            transform_hash,
        },
        request,
        caller_id,
        RenderOutputPrecision::EightBit,
        output_to_display,
        analytics_config,
        display_identity,
        readback_shader,
    )
}

/// Mean linear RGB of the preview's input around a point, before any
/// adjustment (exposure, white balance, tone): what the shader's white balance
/// multiplies. Values are scene-linear for RAW and linearised sRGB otherwise.
fn mean_linear_rgb(image: &DynamicImage, is_raw: bool, x: f64, y: f64) -> Option<[f64; 3]> {
    let roi = preview_sample_roi(image.width(), image.height(), x, y)?;
    linear_mean(&image.crop_imm(roi.x, roi.y, roi.width, roi.height), is_raw)
}

/// Mean linear RGB of the whole preview input (grey world): what Auto white
/// balance neutralises for files without a camera calibration.
fn grey_world_rgb(image: &DynamicImage, is_raw: bool) -> Option<[f64; 3]> {
    linear_mean(&crate::image_processing::thumbnail(image, 512, 512), is_raw)
}

fn linear_mean(image: &DynamicImage, is_raw: bool) -> Option<[f64; 3]> {
    let patch = image.to_rgb32f();
    let count = patch.pixels().len();
    if count == 0 {
        return None;
    }
    let linear = |value: f32| -> f64 {
        let value = value as f64;
        if is_raw {
            value
        } else if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut sums = [0f64; 3];
    for pixel in patch.pixels() {
        for channel in 0..3 {
            sums[channel] += linear(pixel[channel]);
        }
    }
    Some(sums.map(|sum| sum / count as f64))
}

/// The cached preview base of `path` (and whether it is a RAW), or `None`
/// when no current preview exists.
fn current_preview_base(
    state: &AppState,
    develop: &DevelopSession,
    path: &str,
) -> Option<(Arc<DynamicImage>, bool)> {
    let (source, is_raw) = {
        let original = state
            .source
            .original_image
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let image = original.as_ref().filter(|image| image.path == path)?;
        (Arc::clone(&image.image), image.is_raw)
    };
    develop
        .cached_preview
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|preview| preview.source.ptr_eq(&Arc::downgrade(&source)))
        .map(|preview| (Arc::clone(&preview.image), is_raw))
}

/// The white balance picker's sample: [`mean_linear_rgb`] of the cached
/// preview base of `path`, or `None` when no current preview exists.
#[tauri::command]
pub async fn sample_preview_white_balance(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    app_handle: tauri::AppHandle,
) -> Result<Option<[f64; 3]>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let develop = app_handle.state::<DevelopSession>();
        Ok(
            current_preview_base(&state, &develop, &path).and_then(|(image, is_raw)| {
                mean_linear_rgb(&image, is_raw, normalized_x, normalized_y)
            }),
        )
    })
    .await
    .map_err(|error| format!("White balance sampling task failed: {error}"))?
}

/// Auto white balance's sample for files without a camera calibration:
/// [`grey_world_rgb`] of the cached preview base of `path`.
#[tauri::command]
pub async fn sample_preview_grey_world(
    path: String,
    app_handle: tauri::AppHandle,
) -> Result<Option<[f64; 3]>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let develop = app_handle.state::<DevelopSession>();
        Ok(current_preview_base(&state, &develop, &path)
            .and_then(|(image, is_raw)| grey_world_rgb(&image, is_raw)))
    })
    .await
    .map_err(|error| format!("Auto white balance sampling task failed: {error}"))?
}

/// `sample` on the GPU processor for the shown frame of `path`, or `None`
/// when the photograph, its load or its render changed meanwhile.
fn sample_current_frame<T>(
    state: &AppState,
    gpu: &GpuState,
    path: &str,
    sample: impl FnOnce(&GpuProcessor, &Arc<DynamicImage>, usize, usize) -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    use std::sync::atomic::Ordering;
    let load = state.source.load_image_generation.load(Ordering::SeqCst);
    let render = state.preview.preview_generation.load(Ordering::SeqCst);
    let source = state
        .source
        .original_image
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|image| image.path == path)
        .map(|image| Arc::clone(&image.image));
    let Some(source) = source else {
        return Ok(None);
    };
    let sampled = {
        let processor = gpu
            .processor
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(processor) = processor.as_ref() else {
            return Ok(None);
        };
        sample(&processor.processor, &source, load, render)?
    };
    if load != state.source.load_image_generation.load(Ordering::SeqCst)
        || render != state.preview.preview_generation.load(Ordering::SeqCst)
    {
        return Ok(None);
    }
    let still_current = state
        .source
        .original_image
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|image| image.path == path && Arc::ptr_eq(&image.image, &source));
    Ok(if still_current { sampled } else { None })
}

#[tauri::command]
pub async fn sample_preview_color(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    app_handle: tauri::AppHandle,
) -> Result<Option<[f64; 3]>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        sample_current_frame(
            &state,
            &app_handle.state::<GpuState>(),
            &path,
            |processor, source, load, render| {
                processor.sample_display_color(
                    &path,
                    source,
                    load,
                    render,
                    normalized_x,
                    normalized_y,
                )
            },
        )
    })
    .await
    .map_err(|error| format!("Preview color sampling task failed: {error}"))?
}

/// [`DisplayPatch`] for the webview.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPatch {
    column: u32,
    row: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    center: [u32; 2],
    mean: Option<[f64; 3]>,
    frame: [u32; 2],
}

/// The picker loupe's and the pointer readout's sample: the shown frame's
/// 11 × 11 display pixels around a point ([`GpuProcessor::sample_display_patch`]);
/// `mean` is exactly what [`sample_preview_color`] returns there.
#[tauri::command]
pub async fn sample_preview_patch(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    app_handle: tauri::AppHandle,
) -> Result<Option<PreviewPatch>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(feature = "performance-profiling")]
        let started = std::time::Instant::now();
        let state = app_handle.state::<AppState>();
        let patch = sample_current_frame(
            &state,
            &app_handle.state::<GpuState>(),
            &path,
            |processor, source, load, render| {
                processor.sample_display_patch(
                    &path,
                    source,
                    load,
                    render,
                    normalized_x,
                    normalized_y,
                )
            },
        )?;
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "preview_patch_sample",
                "micros": started.elapsed().as_micros() as u64,
                "sampled": patch.is_some(),
                "frame": patch.as_ref().map(|patch| patch.frame),
            })
        );
        Ok(patch.map(|patch| PreviewPatch {
            column: patch.column,
            row: patch.row,
            width: patch.width,
            height: patch.height,
            pixels: patch.pixels,
            center: patch.center,
            mean: patch.mean,
            frame: patch.frame,
        }))
    })
    .await
    .map_err(|error| format!("Preview patch sampling task failed: {error}"))?
}

/// Display frames are encoded in Display P3 (D65 primaries, sRGB curve) on
/// macOS, so colours beyond sRGB reach a wide-gamut panel. Tagging the Metal
/// layer as Display P3 makes Core Animation convert the values to the
/// display's colour space (sRGB monitors included), as WebKit does for the
/// interface. wgpu sets the tag on every `configure` (resizes included);
/// `Srgb` would set an explicit `nil`, which turns matching off on macOS 26
/// (measured: sRGB 200,100,50 appeared as 215,93,31 on the P3 panel), while
/// a layer whose colour space was never set is matched as sRGB. Elsewhere
/// frames are sRGB, as `GpuContext::display_space` says.
#[cfg(not(target_os = "linux"))]
fn display_surface_color_space() -> wgpu::SurfaceColorSpace {
    if cfg!(target_os = "macos") {
        wgpu::SurfaceColorSpace::DisplayP3
    } else {
        wgpu::SurfaceColorSpace::Srgb
    }
}
