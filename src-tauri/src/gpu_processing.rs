//! Desktop glue for `rawtonic_gpu::gpu_processing` (ADR 0009): the window
//! surface, the processor and image cache kept in `AppState`, and the Tauri
//! commands stay here; the pipeline lives in the GPU crate.
pub use rawtonic_gpu::gpu_processing::*;

use std::sync::Arc;

use image::DynamicImage;
use tauri::Manager;

use crate::app_state::AppState;

/// The processing context, created on first use. It is made without the
/// window (the device and the first display variant start before the window
/// exists, `start_early`); `attach_display` adds the window surface.
pub fn get_or_init_gpu_context(
    state: &tauri::State<AppState>,
    app_handle: &tauri::AppHandle,
) -> Result<GpuContext, String> {
    let mut context_lock = match state.gpu_context.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::warn!(
                "GPU context lock was poisoned (previous crash). Wiping state to self-heal."
            );
            let mut guard = poisoned.into_inner();
            *guard = None;
            guard
        }
    };
    if let Some(context) = &*context_lock {
        return Ok(context.clone());
    }
    let instance = new_instance();
    let (context, adapter) = create_context(state, app_handle, &instance, None)?;
    *state.gpu_adapter.lock().unwrap_or_else(|e| e.into_inner()) = Some((instance, adapter));
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
        .spawn(move || {
            let state = app_handle.state::<AppState>();
            match get_or_init_gpu_context(&state, &app_handle) {
                Ok(context) => prewarm_gpu_processor(&state.gpu_processor, &context),
                Err(error) => {
                    log::warn!(
                        "GPU pre-initialization failed (editing and thumbnails may be degraded): {}",
                        error
                    );
                    // Nothing to wait for.
                    first_display_variant_settled();
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("GPU start thread unavailable: {error}");
    }
}

/// Holds optional start-up work (background library passes, model
/// warm-ups) until the RAW default display variant is compiled, for at most
/// 30 s: a cold start's first Develop frame waits for that compile.
pub fn after_first_display_variant() {
    wait_for_first_display_variant(std::time::Duration::from_secs(30));
}

/// Presents onto the main window: its surface on the context's adapter, or,
/// where that adapter cannot present to it, a new context from an adapter
/// that can (the processor of the old one is dropped).
#[cfg(not(target_os = "linux"))]
pub fn attach_display(state: &tauri::State<AppState>, app_handle: &tauri::AppHandle) {
    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    if !settings.use_wgpu_renderer.unwrap_or(true) {
        return;
    }
    let Some(window) = app_handle.get_webview_window("main") else {
        return;
    };
    // Waits for a context the start thread is creating.
    let context = match get_or_init_gpu_context(state, app_handle) {
        Ok(context) => context,
        Err(error) => {
            log::warn!(
                "GPU pre-initialization failed (editing and thumbnails may be degraded): {error}"
            );
            return;
        }
    };
    let Some((instance, adapter)) = state
        .gpu_adapter
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
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
    let mut processor = state
        .gpu_processor
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut context_lock = state.gpu_context.lock().unwrap_or_else(|e| e.into_inner());
    *processor = None;
    match create_context(state, app_handle, &instance, Some((surface, &window))) {
        Ok((context, adapter)) => {
            *state.gpu_adapter.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((instance, adapter));
            *context_lock = Some(context);
        }
        Err(error) => {
            *context_lock = None;
            log::warn!("GPU initialization with the window failed: {error}");
        }
    }
}

fn new_instance() -> wgpu::Instance {
    #[allow(unused_mut)]
    let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();

    #[cfg(target_os = "windows")]
    if std::env::var("WGPU_BACKEND").is_err() {
        instance_desc.backends = wgpu::Backends::PRIMARY;
    }
    wgpu::Instance::new(instance_desc)
}

/// A device on the high-performance adapter (one that can present to
/// `window`'s surface when given), with that window's display.
fn create_context(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    instance: &wgpu::Instance,
    #[allow(unused_variables)] window: Option<(wgpu::Surface<'static>, &tauri::WebviewWindow)>,
) -> Result<(GpuContext, wgpu::Adapter), String> {
    let flag_path = state.gpu_crash_flag_path.lock().unwrap().clone();
    if let Some(p) = &flag_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // The owner's process id: another process starting meanwhile must
        // not take an initialisation in progress for a crash.
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    let remove_flag = || {
        if let Some(p) = &flag_path {
            let _ = std::fs::remove_file(p);
        }
    };

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: window.as_ref().map(|(surface, _)| surface),
        ..Default::default()
    }))
    .map_err(|e| {
        remove_flag();
        format!("Failed to find a wgpu adapter: {}", e)
    })?;

    let info = adapter.get_info();
    log_adapter(&info);
    crate::contribute::note_gpu(&info);
    let limits = adapter.limits();

    let (device, queue) =
        pollster::block_on(adapter.request_device(&processing_device_descriptor(&adapter)))
            .map_err(|e| {
                remove_flag();
                e.to_string()
            })?;

    device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
        log::error!("[wgpu-error] {}", err);
    }));

    remove_flag();

    if let Ok(cache_dir) = app_handle.path().app_cache_dir() {
        rawtonic_gpu::pipeline_cache::open(&device, &info, &cache_dir.join("gpu-pipelines"));
    }

    #[cfg(not(target_os = "linux"))]
    let display_opt = match window {
        Some((surface, window)) => Some(configure_display(&device, &adapter, surface, window)?),
        None => None,
    };

    #[cfg(target_os = "linux")]
    let display_opt = None;

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
    Ok((new_context, adapter))
}

#[cfg(not(target_os = "linux"))]
fn configure_display(
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        present_mode,
        alpha_mode,
        view_formats: vec![],
        // Three drawables. Two would save one window-sized buffer (about
        // 80 MB at 6K) but made live brush frames wait for a free one.
        desired_maximum_frame_latency: 2,
    };
    surface.configure(device, &config);
    #[cfg(target_os = "macos")]
    tag_display_layer_p3(&surface);

    WgpuDisplay::new(device, surface, config)
}

/// The window's newest size in device pixels (width << 32 | height), for a
/// resize that waits for the display.
static WINDOW_SIZE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fits the window surface to a window now `width` × `height` device pixels.
/// While a frame or placement holds the display, the resize waits for it on
/// a worker (the main thread must not block) and then takes the newest size:
/// the window's last resize event — the end of a zoom animation — is never
/// dropped, which left the photograph on a surface of an earlier size.
pub fn resize_display(context: GpuContext, width: u32, height: u32) {
    use std::sync::atomic::Ordering;
    WINDOW_SIZE.store(
        (u64::from(width) << 32) | u64::from(height),
        Ordering::SeqCst,
    );
    let apply = |display: &mut Option<WgpuDisplay>, context: &GpuContext| {
        let size = WINDOW_SIZE.load(Ordering::SeqCst);
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
    let mut processor_lock = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if processor_lock.is_some() {
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
    state: &tauri::State<AppState>,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &state.gpu_processor,
        &state.gpu_image_cache,
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
    state: &AppState,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &state.gpu_processor,
        &state.gpu_image_cache,
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
    state: &tauri::State<AppState>,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &state.gpu_processor,
        &state.gpu_image_cache,
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

pub fn process_and_get_dynamic_image_with_precision(
    context: &GpuContext,
    state: &tauri::State<AppState>,
    base_image: &DynamicImage,
    transform_hash: u64,
    request: RenderRequest,
    caller_id: &str,
    output_precision: RenderOutputPrecision,
) -> Result<DynamicImage, String> {
    process_and_get_dynamic_image_inner(
        context,
        &state.gpu_processor,
        &state.gpu_image_cache,
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
    state: &tauri::State<AppState>,
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
        &state.gpu_processor,
        &state.gpu_image_cache,
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
fn current_preview_base(state: &AppState, path: &str) -> Option<(Arc<DynamicImage>, bool)> {
    let (source, is_raw) = {
        let original = state
            .original_image
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let image = original.as_ref().filter(|image| image.path == path)?;
        (Arc::clone(&image.image), image.is_raw)
    };
    state
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
        Ok(
            current_preview_base(&state, &path).and_then(|(image, is_raw)| {
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
        Ok(current_preview_base(&state, &path)
            .and_then(|(image, is_raw)| grey_world_rgb(&image, is_raw)))
    })
    .await
    .map_err(|error| format!("Auto white balance sampling task failed: {error}"))?
}

#[tauri::command]
pub async fn sample_preview_color(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    app_handle: tauri::AppHandle,
) -> Result<Option<[f64; 3]>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::sync::atomic::Ordering;
        let state = app_handle.state::<AppState>();
        let load = state.load_image_generation.load(Ordering::SeqCst);
        let render = state.preview_generation.load(Ordering::SeqCst);
        let source = state
            .original_image
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|image| image.path == path)
            .map(|image| Arc::clone(&image.image));
        let Some(source) = source else {
            return Ok(None);
        };
        let processor = state
            .gpu_processor
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(processor) = processor.as_ref() else {
            return Ok(None);
        };
        let sample = processor.processor.sample_display_color(
            &path,
            &source,
            load,
            render,
            normalized_x,
            normalized_y,
        )?;
        if load != state.load_image_generation.load(Ordering::SeqCst)
            || render != state.preview_generation.load(Ordering::SeqCst)
        {
            return Ok(None);
        }
        let still_current = state
            .original_image
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|image| image.path == path && Arc::ptr_eq(&image.image, &source));
        Ok(if still_current { sample } else { None })
    })
    .await
    .map_err(|error| format!("Preview color sampling task failed: {error}"))?
}

/// Display frames are encoded in Display P3 (D65 primaries, sRGB curve), so
/// colours beyond sRGB reach a wide-gamut panel. Tagging the Metal layer as
/// Display P3 makes Core Animation convert the values to the display's
/// colour space (sRGB monitors included), as WebKit does for the interface.
/// macOS 26 assigns sRGB to a layer whose colour space was never set, and an
/// explicit `nil` turns matching off (measured: sRGB 200,100,50 appeared as
/// 215,93,31 on the P3 panel), so this is not left to system or wgpu
/// defaults.
#[cfg(target_os = "macos")]
fn tag_display_layer_p3(surface: &wgpu::Surface) {
    use objc2_core_graphics::{CGColorSpace, kCGColorSpaceDisplayP3};
    let Some(space) = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceDisplayP3 })) else {
        log::warn!("Display layer left untagged: no Display P3 colour space");
        return;
    };
    // SAFETY: the Metal backend's surface is only read for its layer, which
    // is retained by the surface for the duration of this call; setColorspace
    // copies the colour space reference.
    unsafe {
        let Some(hal_surface) = surface.as_hal::<wgpu::hal::api::Metal>() else {
            log::warn!("Display layer left untagged: surface is not Metal");
            return;
        };
        let layer = hal_surface.render_layer().lock();
        let space: *const CGColorSpace = &*space;
        let () = objc2::msg_send![&**layer, setColorspace: space];
    }
}
