//! The export's render input for reduced views, on the GPU.
//!
//! A reduced view (the Develop fit view) renders from a reduced copy of the
//! photograph, and the colour noise filter run on that copy leaves
//! 1.5–3 × the chroma noise the export shows at the same size (notes:
//! benchmark-results/tpx-thumb-parity-2026-11-12/REPORT.md, TPX3); its
//! edge-fringe pass at the view's scale also leaves out what the export's
//! full-resolution pass does to colour noise (up to 1.9 × the export's on
//! noisy photos, TPX7). The app therefore renders a final fit frame from
//! the export's own render input reduced to the view's size.
//!
//! Here that input is made as the export makes it: the source is uploaded
//! with the recipe's warp and edge-fringe pass as an export uploads it, its
//! chroma pyramid built, measured and rebuilt on the GPU as for an export
//! (`colorNoiseVersion` 11, [`prepare_color_noise_on_gpu`]), and a pass whose
//! colour noise code is the image-processing shader's own
//! (`shaders/color_noise_view.wgsl` plus that section of `shader.wgsl`)
//! writes the filtered pixel band by band (the input pixel at Color 0).
//! Each band is read back and fed to a streaming Lanczos3
//! ([`rawtonic_engine::lanczos::SignedRowStream::wide_gamut`], the values of
//! `lanczos::resize_wide_gamut`: colours beyond sRGB keep their negative
//! channels, ringing is cut at 0), so the full-resolution result never exists
//! whole on the CPU. Measured: notes:
//! benchmark-results/tpx5-gpu-chroma-2026-11-15/REPORT.md (the filter),
//! benchmark-results/tpx7-2026-11-17/REPORT.md (the render input).

use std::sync::{Arc, Mutex};

use image::DynamicImage;
use rawtonic_engine::lanczos::SignedRowStream;

use super::*;

/// Input pixels per band of the band pass: its output texture and
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
    /// [`OUTPUT_FILTERED`] or [`OUTPUT_INPUT`].
    output_mode: u32,
    _pad: f32,
    source_window: [u32; 4],
    tile_offset: [u32; 2],
    is_raw: u32,
    row_offset: u32,
}

/// The pass writes the filtered pixel (`ViewParams::output_mode`).
const OUTPUT_FILTERED: u32 = 1;
/// The pass writes the input pixel (no colour noise reduction: Color 0).
const OUTPUT_INPUT: u32 = 2;

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
/// there (so the filtered pixel is the export's).
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
    band: wgpu::ComputePipeline,
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
    let band = create_compute_pipeline(
        device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("color_noise_view_band"),
            layout: None,
            module: &module,
            entry_point: Some("color_noise_view_band"),
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
        band,
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

/// The CPU work of a build (the streaming reduction), run by the caller:
/// below the editor's priority, while the GPU calls stay on the calling
/// thread (a starved thread inside wgpu holds locks the editor's frames
/// wait for: G17/TPX6, drags stalled while a build took 7.9 s under load).
pub type CpuRunner<'a> = &'a (dyn Fn(&mut (dyn FnMut() + Send)) + Sync);

/// Uploads `source` as the export uploads its render input (export_processing
/// `prepare_export_render`): the recipe's warp and edge-fringe pass at full
/// resolution on the GPU (`upload_warped_render_source`), else the warp on
/// the CPU and the pass on the GPU (`upload_fringed_render_source`), else
/// both on the CPU. Uncropped: the crop follows the fit view's own base.
/// None beyond the GPU's texture size.
fn upload_render_input(
    context: &GpuContext,
    source: &Arc<DynamicImage>,
    recipe: &serde_json::Value,
    is_raw: bool,
) -> Result<Option<UploadedImage>, String> {
    let (width, height) = source.dimensions();
    let window = [0, 0, width, height];
    let source = match upload_warped_render_source(
        context,
        Arc::clone(source),
        recipe,
        is_raw,
        window,
        0,
        None,
    )? {
        WarpedUpload::Uploaded(uploaded) => return Ok(Some(uploaded)),
        WarpedUpload::NotOnGpu(source) => source,
    };
    let warped = rawtonic_engine::adjustment_utils::apply_warp(&*source, recipe, is_raw)?;
    if let Some(uploaded) = upload_fringed_render_source(context, &warped, recipe, window, 0, None)?
    {
        return Ok(Some(uploaded));
    }
    let fringed = rawtonic_engine::edge_fringe::apply_for_recipe(warped, recipe, 1.0)?;
    upload_render_source(context, &fringed, 0, None)
}

/// The export's render input of the RAW decode `source` with `recipe`
/// ([`upload_render_input`]: warp and edge-fringe pass at full resolution)
/// after its colour noise reduction (`global`'s, version 11; none at Color
/// 0), resampled to `width` x `height` (Lanczos3, signed, streamed: the
/// full-resolution result never exists whole on the CPU). A Develop fit
/// view renders from it at Color 0 instead of its own reduced, warped and
/// fringe-treated copy, whose fringe pass at the view's scale leaves up to
/// 1.9 × the export's colour noise on noisy photos and whose correction from
/// the bare decode missed the export's by 30–40 % of cells (notes:
/// benchmark-results/tpx7-2026-11-17/REPORT.md). None for a recipe whose
/// colour noise reduction is on but not version 11, beyond the texture
/// size, or when `stop` returned true between two steps (a newer frame
/// came: the caller wants again). It never runs beside a shader compile:
/// each step waits for a running one. `cpu` runs the CPU work.
pub fn reduced_render_input(
    context: &GpuContext,
    source: &Arc<DynamicImage>,
    recipe: &serde_json::Value,
    global: &crate::image_processing::GlobalAdjustments,
    (width, height): (u32, u32),
    stop: &(dyn Fn() -> bool + Sync),
    cpu: CpuRunner<'_>,
) -> Result<Option<image::Rgb32FImage>, String> {
    let mut global = *global;
    let versioned = color_noise_tuning(&global);
    let settings = color_noise_settings(&mut global);
    if (settings.is_some() && !versioned.1.gpu_pyramid) || width == 0 || height == 0 {
        return Ok(None);
    }
    let is_raw = global.is_raw_image != 0;
    let pipelines = pipelines(context)?;
    if !wait_for_compiles(stop) {
        return Ok(None);
    }
    let Some(uploaded) = upload_render_input(context, source, recipe, is_raw)? else {
        return Ok(None);
    };
    let (source_width, source_height) = (uploaded.0.width, uploaded.0.height);
    let input = &uploaded.0.texture_view;
    if !wait_for_compiles(stop) {
        return Ok(None);
    }
    let chroma = match settings {
        Some(settings) => {
            let Some(pyramid) = GpuPyramid::build(
                context,
                &pipelines.chroma,
                input,
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
                input,
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
            Some(chroma)
        }
        None => None,
    };
    let output_mode = if chroma.is_some() {
        OUTPUT_FILTERED
    } else {
        OUTPUT_INPUT
    };
    let params = |row_offset: u32| ViewParams {
        color_noise: global.color_noise,
        color_noise_window: global.color_noise_window,
        color_noise_aware2: global.color_noise_aware2,
        color_noise_aware_far: global.color_noise_aware_far,
        color_noise_guide: global.color_noise_guide,
        color_noise_guide_k2: global.color_noise_guide_k2,
        color_noise_speck: global.color_noise_speck,
        color_noise_reduction: global.color_noise_reduction,
        output_mode,
        _pad: 0.0,
        source_window: [0, 0, source_width, source_height],
        tile_offset: [0, 0],
        is_raw: u32::from(is_raw),
        row_offset,
    };
    let device = &context.device;
    let band_rows = (BAND_PIXELS / source_width.max(1)).clamp(16, source_height.max(16)) / 16 * 16;
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Colour noise view band"),
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
    let (packed, energy) = match &chroma {
        Some(chroma) => (&chroma.packed_view, &chroma.energy_view),
        // Never read without the filter.
        None => (&pipelines.blur, &pipelines.blur),
    };
    let mut stream = Some(SignedRowStream::wide_gamut(
        source_width,
        source_height,
        width,
        height,
    ));
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
            layout: &pipelines.band.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: texture_view(input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: texture_view(packed),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture_view(energy),
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
            pass.set_pipeline(&pipelines.band);
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
        if stop() {
            return Ok(None);
        }
        cpu(&mut || {
            if let Some(stream) = stream.as_mut() {
                stream.push_rows(&rgb);
            }
        });
        top += rows;
    }
    let mut reduced = None;
    cpu(&mut || reduced = stream.take().and_then(SignedRowStream::finish));
    Ok(reduced)
}
