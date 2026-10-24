//! Desktop glue for `rawtonic_gpu::gpu_processing` (ADR 0009): the window
//! surface, the processor and image cache kept in `AppState`, and the Tauri
//! commands stay here; the pipeline lives in the GPU crate.
pub use rawtonic_gpu::gpu_processing::*;

use std::sync::Arc;

use image::DynamicImage;
use tauri::Manager;

use crate::app_state::AppState;

pub fn get_or_init_gpu_context(
    state: &tauri::State<AppState>,
    _app_handle: &tauri::AppHandle,
) -> Result<GpuContext, String> {
    #[cfg(not(target_os = "linux"))]
    let app_handle = _app_handle;

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

    #[allow(unused_mut)]
    let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();

    #[cfg(target_os = "windows")]
    if std::env::var("WGPU_BACKEND").is_err() {
        instance_desc.backends = wgpu::Backends::PRIMARY;
    }

    let flag_path = state.gpu_crash_flag_path.lock().unwrap().clone();
    if let Some(p) = &flag_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // The owner's process id: another process starting meanwhile must
        // not take an initialisation in progress for a crash.
        let _ = std::fs::write(p, std::process::id().to_string());
    }

    let instance = wgpu::Instance::new(instance_desc);

    #[cfg(not(target_os = "linux"))]
    let surface_opt = {
        let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
        let use_wgpu_renderer = settings.use_wgpu_renderer.unwrap_or(true);

        if use_wgpu_renderer {
            if let Some(window) = app_handle.get_webview_window("main") {
                match instance.create_surface(window) {
                    Ok(surface) => Some(surface),
                    Err(e) => {
                        log::warn!(
                            "Failed to create surface, falling back to compute-only: {}",
                            e
                        );
                        if let Some(p) = &flag_path {
                            let _ = std::fs::remove_file(p);
                        }
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    #[cfg(target_os = "linux")]
    let surface_opt: Option<wgpu::Surface> = None;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface_opt.as_ref(),
        ..Default::default()
    }))
    .map_err(|e| {
        if let Some(p) = &flag_path {
            let _ = std::fs::remove_file(p);
        }
        format!("Failed to find a wgpu adapter: {}", e)
    })?;

    let limits = adapter.limits();

    let (device, queue) =
        pollster::block_on(adapter.request_device(&processing_device_descriptor(&adapter)))
            .map_err(|e| {
                if let Some(p) = &flag_path {
                    let _ = std::fs::remove_file(p);
                }
                e.to_string()
            })?;

    device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
        log::error!("[wgpu-error] {}", err);
    }));

    if let Some(p) = &flag_path {
        let _ = std::fs::remove_file(p);
    }

    #[cfg(not(target_os = "linux"))]
    let display_opt = if let Some(surface) = surface_opt {
        let window = app_handle
            .get_webview_window("main")
            .ok_or("Failed to get main window")?;

        let swapchain_caps = surface.get_capabilities(&adapter);
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
        surface.configure(&device, &config);
        #[cfg(target_os = "macos")]
        tag_display_layer_p3(&surface);

        Some(WgpuDisplay::new(&device, surface, config)?)
    } else {
        None
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
    *context_lock = Some(new_context.clone());
    Ok(new_context)
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
        Err(error) => log::warn!("GPU processor prewarm failed: {error}"),
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
