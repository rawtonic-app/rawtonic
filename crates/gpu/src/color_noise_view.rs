//! The export's colour noise reduction for reduced views, on the GPU.
//!
//! A reduced view (the Develop fit view) renders from a reduced copy of the
//! photograph, and the colour noise filter run on that copy leaves
//! 1.5–3 × the chroma noise the export shows at the same size (notes:
//! benchmark-results/tpx-thumb-parity-2026-11-12/REPORT.md, TPX3). The app
//! therefore adds what the full-resolution filter changes, reduced to the
//! view's size, to the view's reduced copy (`color_noise::
//! reduced_correction` on the CPU, TPX4). On a Radeon 680M laptop that CPU
//! build took 2.1–2.9 s with +1.3–1.85 GB of private memory (G17/WTPX4).
//!
//! Here the same correction comes from the export's own GPU filter: the
//! source is uploaded as an export uploads it, its chroma pyramid built,
//! measured and rebuilt on the GPU as for an export (`colorNoiseVersion`
//! 11, [`prepare_color_noise_on_gpu`]), and a pass whose colour noise code
//! is the image-processing shader's own (`shaders/color_noise_view.wgsl`
//! plus that section of `shader.wgsl`) writes the filtered pixel minus the
//! input pixel band by band. Each band is read back and fed to a streaming
//! Lanczos3 ([`rawtonic_engine::lanczos::SignedRowStream`], the values of
//! `lanczos::resize_signed`), so the full-resolution difference never
//! exists whole. Measured: notes: benchmark-results/tpx5-gpu-chroma-2026-11-15/REPORT.md.

use std::sync::{Arc, Mutex};

use image::DynamicImage;
use rawtonic_engine::lanczos::SignedRowStream;

use super::*;

/// Input pixels per band of the difference pass: its output texture and
/// readback buffer are 16 bytes per pixel each (32 MB apiece).
const BAND_PIXELS: u32 = 2 << 20;

/// Uniforms of color_noise_view.wgsl (`ViewParams`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewParams {
    color_noise: [f32; 4],
    color_noise_window: [u32; 4],
    color_noise_aware2: f32,
    color_noise_aware_far: f32,
    color_noise_guide: f32,
    color_noise_guide_k2: f32,
    color_noise_speck: f32,
    color_noise_reduction: f32,
    _pad: [f32; 2],
    source_window: [u32; 4],
    tile_offset: [u32; 2],
    is_raw: u32,
    row_offset: u32,
}

/// The functions of `shader.wgsl` the colour noise section calls.
const HELPERS: [&str; 9] = [
    "load_input",
    "input_dims",
    "get_luma",
    "srgb_to_linear",
    "nr_encode",
    "nr_decode",
    "srgb_to_prophoto",
    "prophoto_to_srgb",
    "gamut_floor",
];

/// `fn name(` … the first closing brace at the start of a line, of `source`.
fn function_of<'a>(source: &'a str, name: &str) -> Result<&'a str, String> {
    let start = source
        .find(&format!("\nfn {name}("))
        .ok_or_else(|| format!("shader.wgsl has no fn {name}"))?
        + 1;
    let end = source[start..]
        .find("\n}\n")
        .ok_or_else(|| format!("fn {name} has no end"))?;
    Ok(&source[start..start + end + 3])
}

/// The pass's source: its bindings and entry point, the image-processing
/// shader's colour noise section and the helpers it calls, as they stand
/// there (so the difference is the export's filter).
pub(super) fn shader_source() -> Result<String, String> {
    const MAIN: &str = include_str!("shaders/shader.wgsl");
    const BEGIN: &str = "// --- Multi-scale colour noise reduction";
    const END: &str = "\nfn apply_noise_reduction_default(";
    let begin = MAIN
        .find(BEGIN)
        .ok_or("shader.wgsl has no colour noise section")?;
    let end = begin
        + MAIN[begin..]
            .find(END)
            .ok_or("shader.wgsl's colour noise section has no end")?;
    let luma = MAIN
        .lines()
        .find(|line| line.starts_with("const LUMA_COEFF"))
        .ok_or("shader.wgsl has no LUMA_COEFF")?;
    let mut source = String::from(include_str!("shaders/color_noise_view.wgsl"));
    source.push('\n');
    source.push_str(luma);
    source.push('\n');
    for name in HELPERS {
        source.push('\n');
        source.push_str(function_of(MAIN, name)?);
    }
    source.push('\n');
    source.push_str(&MAIN[begin..end]);
    Ok(source)
}

struct ViewPipelines {
    device: Arc<wgpu::Device>,
    chroma: ChromaPipelines,
    delta: wgpu::ComputePipeline,
    /// Stands in for the luminance noise reduction's blur (never read).
    blur: wgpu::TextureView,
}

/// The pass's pipelines, made on first use for the context's device.
fn pipelines(context: &GpuContext) -> Result<Arc<ViewPipelines>, String> {
    static PIPELINES: Mutex<Option<Arc<ViewPipelines>>> = Mutex::new(None);
    let mut slot = PIPELINES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = slot.as_ref()
        && Arc::ptr_eq(&p.device, &context.device)
    {
        return Ok(Arc::clone(p));
    }
    let device = &context.device;
    let module = create_shader(device, "Colour Noise View Shader", &shader_source()?)?;
    let delta = create_compute_pipeline(
        device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("color_noise_delta"),
            layout: None,
            module: &module,
            entry_point: Some("color_noise_delta"),
            compilation_options: Default::default(),
            cache: None,
        },
        CompileWeight::Tiny,
    )?;
    let blur = float_texture(
        context,
        "Colour noise view blur stand-in",
        1,
        1,
        wgpu::TextureFormat::Rgba16Float,
        None,
    )
    .create_view(&Default::default());
    let made = Arc::new(ViewPipelines {
        device: Arc::clone(device),
        chroma: ChromaPipelines::new(device)?,
        delta,
        blur,
    });
    *slot = Some(Arc::clone(&made));
    Ok(made)
}

/// Makes the pass's pipelines for `context` ahead (their compile is not
/// part of a build's time).
pub fn ready(context: &GpuContext) -> Result<(), String> {
    pipelines(context).map(drop)
}

/// Waits while a shader compile runs (on a Radeon 680M one commits 4–6 GB
/// and holds the GPU); false when `stop` said so meanwhile.
fn wait_for_compiles(stop: &(dyn Fn() -> bool + Sync)) -> bool {
    while compiling_now() {
        if stop() {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    !stop()
}

/// What the export's colour noise reduction (`global`'s, version 11)
/// changes in `image` (the render source, linear for a RAW), resampled to
/// `width` x `height` (Lanczos3, signed): the GPU counterpart of
/// `color_noise::reduced_correction`. None when the recipe has no
/// multi-scale colour noise reduction of version 11, the image is too
/// small to filter, or `stop` returned true between two steps (a newer
/// frame came: the caller wants again). It never runs beside a shader
/// compile: each step waits for a running one.
pub fn reduced_correction(
    context: &GpuContext,
    image: &DynamicImage,
    global: &crate::image_processing::GlobalAdjustments,
    (width, height): (u32, u32),
    stop: &(dyn Fn() -> bool + Sync),
) -> Result<Option<image::Rgb32FImage>, String> {
    let mut global = *global;
    let versioned = color_noise_tuning(&global);
    if !versioned.1.gpu_pyramid {
        return Ok(None);
    }
    let Some(settings) = color_noise_settings(&mut global) else {
        return Ok(None);
    };
    let (source_width, source_height) = image.dimensions();
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let is_raw = global.is_raw_image != 0;
    let pipelines = pipelines(context)?;
    if !wait_for_compiles(stop) {
        return Ok(None);
    }
    let device = &context.device;
    let texture = create_input_texture(device, &context.queue, image, INPUT_UPLOAD_BAND_ROWS)?;
    let input = texture.create_view(&Default::default());
    if !wait_for_compiles(stop) {
        return Ok(None);
    }
    let Some(pyramid) = GpuPyramid::build(
        context,
        &pipelines.chroma,
        &input,
        source_width,
        source_height,
        is_raw,
    )?
    else {
        return Ok(None);
    };
    if !wait_for_compiles(stop) {
        return Ok(None);
    }
    let (thresholds, base_sigma, _) =
        color_noise_thresholds(&mut global, settings, &pyramid.measured.noise, &versioned);
    let [lw, lh] = pyramid.sizes[0];
    global.color_noise_window = [0, 0, lw, lh];
    let mut chroma = ChromaGpu::from_gpu_pyramid(
        context,
        &pipelines.chroma,
        &input,
        source_width,
        source_height,
        is_raw,
        &pyramid,
    )?;
    chroma.rebuild_on_gpu(
        context,
        &pipelines.chroma,
        PyramidRef::Gpu(&pyramid),
        &thresholds,
        base_sigma,
        &versioned.1,
    )?;
    // As an export: only level 1 and its reconstruction stay.
    chroma.reconstruction = None;
    drop(pyramid);

    let params = |row_offset: u32| ViewParams {
        color_noise: global.color_noise,
        color_noise_window: global.color_noise_window,
        color_noise_aware2: global.color_noise_aware2,
        color_noise_aware_far: global.color_noise_aware_far,
        color_noise_guide: global.color_noise_guide,
        color_noise_guide_k2: global.color_noise_guide_k2,
        color_noise_speck: global.color_noise_speck,
        color_noise_reduction: global.color_noise_reduction,
        _pad: [0.0; 2],
        source_window: [0, 0, source_width, source_height],
        tile_offset: [0, 0],
        is_raw: u32::from(is_raw),
        row_offset,
    };
    let band_rows = (BAND_PIXELS / source_width.max(1)).clamp(16, source_height.max(16)) / 16 * 16;
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Colour noise view difference"),
        size: wgpu::Extent3d {
            width: source_width,
            height: band_rows,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&Default::default());
    let mut stream = SignedRowStream::new(source_width, source_height, width, height);
    let mut rgb = Vec::new();
    let mut top = 0;
    while top < source_height {
        if !wait_for_compiles(stop) {
            return Ok(None);
        }
        let rows = band_rows.min(source_height - top);
        let buffer = create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("Colour noise view params"),
                contents: bytemuck::bytes_of(&params(top)),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Colour noise view BG"),
            layout: &pipelines.delta.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: texture_view(&input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: texture_view(&chroma.packed_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture_view(&chroma.energy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: texture_view(&pipelines.blur),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: texture_view(&output_view),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipelines.delta);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(source_width.div_ceil(16), rows.div_ceil(16), 1);
        }
        context.queue.submit(Some(encoder.finish()));
        let pending = begin_texture_readback(
            device,
            &context.queue,
            &output,
            wgpu::Origin3d::ZERO,
            wgpu::Extent3d {
                width: source_width,
                height: rows,
                depth_or_array_layers: 1,
            },
            16,
            None,
        );
        finish_texture_readback_with(device, pending, None, |mapped, padded, unpadded| {
            rgb.clear();
            rgb.reserve(source_width as usize * rows as usize * 3);
            for row in mapped.chunks_exact(padded) {
                let texels: &[f32] = bytemuck::cast_slice(&row[..unpadded]);
                for texel in texels.as_chunks::<4>().0 {
                    rgb.extend_from_slice(&texel[..3]);
                }
            }
        })?;
        stream.push_rows(&rgb);
        top += rows;
    }
    Ok(stream.finish())
}
