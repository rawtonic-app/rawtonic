use std::sync::{Arc, Mutex};
use std::time::Instant;

use half::f16;
#[cfg(test)]
use image::Luma;
use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Rgba};
use std::num::NonZero;

use wgpu::util::{DeviceExt, TextureDataOrder};

use crate::image_processing::{AllAdjustments, MAX_MASKS, Point};
use crate::lut::Lut;
use rawtonic_engine::color_noise::{self, ChromaPyramid};
use rayon::prelude::*;

#[cfg(test)]
#[path = "gpu_processing_tests.rs"]
mod preview_tests;

#[path = "multi_view.rs"]
pub mod multi_view;
#[path = "preview_layers.rs"]
mod preview_layers;
pub use multi_view::{CellExtent, CellLayer, CellZoom, MultiView, MultiViewCell};
use multi_view::{DisplayBindings, DisplayPass, PrimaryDraw, encode_display_pass};
use preview_layers::{CURVE_LUT_SAMPLES, PreparedLayer, curve_lut, prepare_layers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The processing device; `display` is the window surface when the app has one.
#[derive(Clone)]
pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub limits: wgpu::Limits,
    pub display: Arc<std::sync::Mutex<Option<WgpuDisplay>>>,
    /// The colour space display frames are encoded in, as the window layer
    /// is tagged (Display P3 on macOS, sRGB elsewhere and headless).
    pub display_space: rawtonic_engine::output_color_space::OutputColorSpace,
}

/// The processing device requested from `adapter`: its own limits and, where
/// available, adapter-specific texture format features. Shared by the app's
/// window context and [`GpuContext::headless`].
pub fn processing_device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let mut required_features = wgpu::Features::empty();
    if adapter
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
    {
        required_features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    }
    wgpu::DeviceDescriptor {
        label: Some("Processing Device"),
        required_features,
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }
}

impl GpuContext {
    /// A processing context without a window surface, for exports and tools
    /// outside the app. Registers the device for the GPU X-Trans demosaic.
    pub fn headless() -> Result<Self, String> {
        #[allow(unused_mut)]
        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        #[cfg(target_os = "windows")]
        if std::env::var("WGPU_BACKEND").is_err() {
            instance_desc.backends = wgpu::Backends::PRIMARY;
        }
        let instance = wgpu::Instance::new(instance_desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| format!("Failed to find a wgpu adapter: {}", e))?;
        let limits = adapter.limits();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&processing_device_descriptor(&adapter)))
                .map_err(|e| e.to_string())?;
        device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
            log::error!("[wgpu-error] {}", err);
        }));
        let context = GpuContext {
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            display: Arc::new(Mutex::new(None)),
            display_space: Default::default(),
        };
        rawtonic_rawdecode::gpu_demosaic::register(context.device.clone(), context.queue.clone());
        Ok(context)
    }
}

/// An uploaded source texture, kept between renders of the same photo.
pub struct GpuImageCache {
    pub texture: wgpu::Texture,
    pub texture_view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub transform_hash: u64,
    /// The source's chroma pyramid for the multi-scale colour noise
    /// reduction, built on first use (rawtonic_engine::color_noise).
    pub chroma: Option<Arc<ChromaPyramid>>,
    /// The renderer's textures for `chroma` (level 1 and its reconstruction,
    /// the finest level's energy).
    chroma_gpu: Option<Box<ChromaGpu>>,
}

/// Thresholds and steepness a packed chroma texture was rebuilt with.
type ChromaKey = [u32; color_noise::LEVELS + 1];

/// Level-1 pixels up to which display frames rebuild level 1 on the GPU
/// (a 2304 px preview has 1.3 M); larger sources rebuild it on the CPU.
const GPU_RECONSTRUCTION_MAX_PIXELS: usize = 4 << 20;

/// The multi-scale colour noise reduction's textures for one input texture
/// (the whole photo, or one window of it).
pub struct ChromaGpu {
    /// The thresholds the packed reconstruction holds (None: not yet built).
    key: Option<ChromaKey>,
    /// Level-1 window: origin and size in level-1 samples.
    window: [u32; 4],
    /// Level 1 (xy) and its reconstruction (zw), Rgba16Float.
    packed: wgpu::Texture,
    packed_view: wgpu::TextureView,
    /// The finest level's local energy per input pixel, R32Float.
    energy_view: wgpu::TextureView,
    /// Display frames: the coarse levels, rebuilt on the GPU.
    reconstruction: Option<ChromaReconstruction>,
}

struct ChromaReconstruction {
    /// Prepared levels 1…4 (detail xy, local energy z, chroma-edge ratio w).
    levels: Vec<wgpu::TextureView>,
    /// The luminance-following part of each prepared level's detail (xy).
    follow: Vec<wgpu::TextureView>,
    /// Rebuilt levels 1…4.
    rebuilt: Vec<wgpu::TextureView>,
    /// Level 5 as it is (smoothed per threshold set above Amount 25).
    coarsest: wgpu::TextureView,
    /// Levels 2…5 unfiltered (chroma xy, luminance z), for the aware
    /// expansion of the finer level.
    originals: Vec<wgpu::TextureView>,
    /// Level 1 (xy).
    level1: wgpu::TextureView,
}

/// Compute pipelines of color_noise.wgsl.
pub struct ChromaPipelines {
    energy: wgpu::ComputePipeline,
    reconstruct: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
}

impl ChromaPipelines {
    fn new(device: &wgpu::Device) -> Result<Self, String> {
        let module = create_shader(
            device,
            "Colour Noise Shader",
            include_str!("shaders/color_noise.wgsl"),
        )?;
        let pipeline = |entry: &str| {
            create_compute_pipeline(
                device,
                &wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: None,
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                },
            )
        };
        Ok(Self {
            energy: pipeline("finest_energy")?,
            reconstruct: pipeline("reconstruct_level")?,
            pack: pipeline("pack_finest")?,
        })
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorNoiseParams {
    source: [u32; 4],
    level: [u32; 4],
    is_raw: u32,
    threshold2: f32,
    steepness: f32,
    /// Squared factor of the chroma-edge guard of the level being rebuilt
    /// (`Tuning::edge_scale2`).
    edge2: f32,
    /// Inverse squared range of the level's aware expansion
    /// (`Tuning::aware_inv2`); 0 = plain expansion.
    aware_inv2: f32,
    /// `Tuning::aware_far`.
    aware_far: f32,
    _pad: [f32; 2],
    /// The colour noise profile by brightness (`ChromaPyramid::profile`),
    /// for the finest level's energy and the aware expansion.
    profile: [f32; color_noise::PROFILE_BINS],
}

/// A pyramid level as an Rgba32Float texture: chroma (xy), luminance (z).
fn chroma_plane_texture(
    context: &GpuContext,
    plane: &color_noise::Plane,
    label: &str,
) -> wgpu::TextureView {
    let data: Vec<[f32; 4]> = plane.data.iter().map(|v| [v[0], v[1], v[2], 0.0]).collect();
    float_texture(
        context,
        label,
        plane.width,
        plane.height,
        wgpu::TextureFormat::Rgba32Float,
        Some(bytemuck::cast_slice(&data)),
    )
    .create_view(&Default::default())
}

fn float_texture(
    context: &GpuContext,
    label: &str,
    width: usize,
    height: usize,
    format: wgpu::TextureFormat,
    data: Option<&[u8]>,
) -> wgpu::Texture {
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    if let Some(data) = data {
        write_whole_texture(context, &texture, data);
    }
    texture
}

fn write_whole_texture(context: &GpuContext, texture: &wgpu::Texture, data: &[u8]) {
    let size = texture.size();
    let bytes_per_texel = data.len() as u32 / (size.width * size.height);
    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * bytes_per_texel),
            rows_per_image: Some(size.height),
        },
        size,
    );
}

/// The window of level 1 a photo window's finest level reads: one sample
/// beyond the expansion taps of its pixels on each side.
fn chroma_level_window(window: Roi, level_width: usize, level_height: usize) -> [u32; 4] {
    let span = |start: u32, size: u32, n: usize| {
        let first = (start / 2).saturating_sub(2);
        let end = ((start + size).saturating_sub(1) / 2 + 3).min(n as u32);
        (first, end - first)
    };
    let (x, w) = span(window.x, window.width, level_width);
    let (y, h) = span(window.y, window.height, level_height);
    [x, y, w, h]
}

/// Level 1 and `rebuilt` as the renderer reads them (Rgba16Float), for the
/// level-1 `window`.
fn pack_chroma(
    level1: &color_noise::Plane,
    rebuilt: &color_noise::Plane,
    window: [u32; 4],
) -> Vec<f16> {
    let [x0, y0, w, h] = window.map(|v| v as usize);
    let mut texels = vec![f16::ZERO; w * h * 4];
    texels
        .par_chunks_mut(4 * w)
        .enumerate()
        .for_each(|(row, out)| {
            let start = (y0 + row) * level1.width + x0;
            let level = &level1.data[start..start + w];
            let rebuilt = &rebuilt.data[start..start + w];
            for ((texel, g), r) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(level)
                .zip(rebuilt)
            {
                *texel = [g[0], g[1], r[0], r[1]].map(f16::from_f32);
            }
        });
    texels
}

/// The recipe's colour noise settings, or None (and the cross-bilateral
/// filter) when the multi-scale filter does not apply.
fn color_noise_settings(
    global: &mut crate::image_processing::GlobalAdjustments,
) -> Option<color_noise::Settings> {
    if global.color_noise[0] <= 0.0 {
        return None;
    }
    let settings = color_noise::Settings {
        amount: global.color_noise_reduction,
        detail: global.noise_detail[2],
        smoothness: global.noise_detail[3],
    };
    if settings.amount <= 0.0 {
        global.color_noise[0] = 0.0;
        return None;
    }
    Some(settings)
}

/// Whether the recipe's luminance noise reduction uses the photo's
/// luminance noise by brightness: recipes of the multi-scale colour noise
/// reduction (`colorNoiseVersion` 2, also at Color 0) with luminance noise
/// reduction; earlier recipes render as before.
fn luma_profile_wanted(global: &crate::image_processing::GlobalAdjustments) -> bool {
    global.color_noise[0] > 0.0 && global.luma_noise_reduction > 0.0
}

/// Sets the luminance noise profile (`ChromaPyramid::luma_profile`) in the
/// uniforms.
fn set_luma_profile(
    global: &mut crate::image_processing::GlobalAdjustments,
    pyramid: &ChromaPyramid,
) {
    let p = pyramid.luma_profile;
    global.luma_noise_profile = [[p[0], p[1], p[2], p[3]], [p[4], p[5], p[6], p[7]]];
}

/// The thresholds for `settings`, set in the uniforms, the coarsest
/// level's smoothing (`Tuning::base_sigma`) and their key.
fn color_noise_thresholds(
    global: &mut crate::image_processing::GlobalAdjustments,
    settings: color_noise::Settings,
    pyramid: &ChromaPyramid,
    tuning: &color_noise::Tuning,
) -> ([f32; color_noise::LEVELS], f32, ChromaKey) {
    let thresholds = tuning.thresholds(settings, &pyramid.noise);
    let base_sigma = tuning.base_sigma(settings);
    let mut key: ChromaKey = [0; color_noise::LEVELS + 1];
    for (k, t) in key.iter_mut().zip(thresholds) {
        *k = t.to_bits();
    }
    key[color_noise::LEVELS] = tuning.steepness.to_bits();
    global.color_noise[1] = thresholds[0];
    global.color_noise[2] = tuning.steepness;
    global.color_noise[3] = tuning.finest_edge2(&thresholds, &pyramid.noise);
    global.color_noise_aware2 = tuning.finest_aware2(&thresholds, &pyramid.noise);
    global.color_noise_aware_far = tuning.aware_far;
    (thresholds, base_sigma, key)
}

impl ChromaGpu {
    /// Textures for an input texture holding `source` of a `width` x
    /// `height` photo, with level 1 and `rebuilt` (or level 1 again) for
    /// the level-1 `window`; computes the finest level's energy.
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: &GpuContext,
        pipelines: &ChromaPipelines,
        input: &wgpu::TextureView,
        source: Roi,
        width: u32,
        height: u32,
        is_raw: bool,
        pyramid: &ChromaPyramid,
        rebuilt: Option<&color_noise::Plane>,
        window: [u32; 4],
    ) -> Self {
        let level1 = &pyramid.levels[0];
        let texels = pack_chroma(level1, rebuilt.unwrap_or(level1), window);
        let packed = float_texture(
            context,
            "Chroma level 1",
            window[2] as usize,
            window[3] as usize,
            wgpu::TextureFormat::Rgba16Float,
            Some(bytemuck::cast_slice(&texels)),
        );
        let packed_view = packed.create_view(&Default::default());
        let energy = float_texture(
            context,
            "Chroma finest energy",
            source.width as usize,
            source.height as usize,
            wgpu::TextureFormat::R32Float,
            None,
        );
        let energy_view = energy.create_view(&Default::default());
        let params = ColorNoiseParams {
            source: [source.x, source.y, width, height],
            level: [
                window[0],
                window[1],
                level1.width as u32,
                level1.height as u32,
            ],
            is_raw: u32::from(is_raw),
            threshold2: 0.0,
            steepness: 0.0,
            edge2: 0.0,
            aware_inv2: 0.0,
            aware_far: 0.0,
            _pad: [0.0; 2],
            profile: pyramid.profile,
        };
        let device = &context.device;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Colour noise params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Chroma energy BG0"),
            layout: &pipelines.energy.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&packed_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Chroma energy BG1"),
            layout: &pipelines.energy.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&energy_view),
            }],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipelines.energy);
            pass.set_bind_group(0, &group0, &[]);
            pass.set_bind_group(1, &group1, &[]);
            pass.dispatch_workgroups(source.width.div_ceil(16), source.height.div_ceil(16), 1);
        }
        context.queue.submit(Some(encoder.finish()));
        Self {
            key: None,
            window,
            packed,
            packed_view,
            energy_view,
            reconstruction: None,
        }
    }

    /// Rebuilds level 1 for `thresholds` on the CPU.
    fn rebuild_on_cpu(
        &mut self,
        context: &GpuContext,
        pyramid: &ChromaPyramid,
        thresholds: &[f32; color_noise::LEVELS],
        base_sigma: f32,
        tuning: &color_noise::Tuning,
    ) {
        let rebuilt = pyramid.reconstruct(thresholds, base_sigma, tuning);
        let texels = pack_chroma(&pyramid.levels[0], &rebuilt, self.window);
        write_whole_texture(context, &self.packed, bytemuck::cast_slice(&texels));
    }

    /// Rebuilds level 1 for `thresholds` on the GPU (whole photos only).
    fn rebuild_on_gpu(
        &mut self,
        context: &GpuContext,
        pipelines: &ChromaPipelines,
        pyramid: &ChromaPyramid,
        thresholds: &[f32; color_noise::LEVELS],
        base_sigma: f32,
        tuning: &color_noise::Tuning,
    ) {
        let device = &context.device;
        let reconstruction = self.reconstruction.get_or_insert_with(|| {
            let prepared = pyramid.prepared(tuning);
            let rgba = |data: Vec<[f32; 4]>, width: usize, height: usize, label: &str| {
                float_texture(
                    context,
                    label,
                    width,
                    height,
                    wgpu::TextureFormat::Rgba32Float,
                    Some(bytemuck::cast_slice(&data)),
                )
                .create_view(&Default::default())
            };
            let levels = prepared
                .levels
                .iter()
                .map(|level| {
                    let data = (0..level.width * level.height)
                        .map(|i| {
                            let d = level.detail[i];
                            [d[0], d[1], level.size2[i], level.edge[i]]
                        })
                        .collect();
                    rgba(data, level.width, level.height, "Chroma prepared level")
                })
                .collect();
            let follow = prepared
                .levels
                .iter()
                .map(|level| {
                    let data = level
                        .follow
                        .iter()
                        .map(|f| [f[0], f[1], 0.0, 0.0])
                        .collect();
                    rgba(
                        data,
                        level.width,
                        level.height,
                        "Chroma luminance-following detail",
                    )
                })
                .collect();
            let rebuilt = prepared
                .levels
                .iter()
                .map(|level| {
                    float_texture(
                        context,
                        "Chroma rebuilt level",
                        level.width,
                        level.height,
                        wgpu::TextureFormat::Rgba32Float,
                        None,
                    )
                    .create_view(&Default::default())
                })
                .collect();
            let originals = pyramid.levels[1..]
                .iter()
                .map(|level| chroma_plane_texture(context, level, "Chroma level (unfiltered)"))
                .collect();
            ChromaReconstruction {
                levels,
                follow,
                rebuilt,
                coarsest: chroma_plane_texture(
                    context,
                    &pyramid.levels[color_noise::LEVELS - 1],
                    "Chroma level 5",
                ),
                originals,
                level1: chroma_plane_texture(context, &pyramid.levels[0], "Chroma level 1 (f32)"),
            }
        });
        let smoothed = (base_sigma > 0.0).then(|| {
            chroma_plane_texture(
                context,
                &pyramid.coarsest(base_sigma, tuning),
                "Chroma level 5 (smoothed)",
            )
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for l in (1..color_noise::LEVELS).rev() {
            let params = ColorNoiseParams {
                source: [0; 4],
                level: [0; 4],
                is_raw: 0,
                threshold2: thresholds[l] * thresholds[l],
                steepness: tuning.steepness,
                edge2: tuning.edge_scale2(l, thresholds[l], &pyramid.noise),
                aware_inv2: tuning.aware_inv2(l, thresholds, &pyramid.noise),
                aware_far: tuning.aware_far,
                _pad: [0.0; 2],
                profile: pyramid.profile,
            };
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Colour noise level params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let coarser = if l == color_noise::LEVELS - 1 {
                smoothed.as_ref().unwrap_or(&reconstruction.coarsest)
            } else {
                &reconstruction.rebuilt[l]
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Chroma reconstruct BG"),
                layout: &pipelines.reconstruct.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&reconstruction.levels[l - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(coarser),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(
                            &reconstruction.rebuilt[l - 1],
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&reconstruction.follow[l - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(
                            &reconstruction.originals[l - 1],
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(if l == 1 {
                            &reconstruction.level1
                        } else {
                            &reconstruction.originals[l - 2]
                        }),
                    },
                ],
            });
            let size = pyramid.levels[l - 1].width.max(1) as u32;
            let rows = pyramid.levels[l - 1].height.max(1) as u32;
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipelines.reconstruct);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.div_ceil(16), rows.div_ceil(16), 1);
        }
        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Chroma pack BG0"),
            layout: &pipelines.pack.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&reconstruction.level1),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&reconstruction.rebuilt[0]),
                },
            ],
        });
        let packed_storage = self.packed.create_view(&Default::default());
        let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Chroma pack BG1"),
            layout: &pipelines.pack.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&packed_storage),
            }],
        });
        {
            let size = self.packed.size();
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipelines.pack);
            pass.set_bind_group(0, &group0, &[]);
            pass.set_bind_group(1, &group1, &[]);
            pass.dispatch_workgroups(size.width.div_ceil(16), size.height.div_ceil(16), 1);
        }
        context.queue.submit(Some(encoder.finish()));
    }
}

/// Readies the multi-scale colour noise reduction of a whole-photo render:
/// builds the source's chroma pyramid if needed (from `source`, the CPU
/// pixels of the uploaded texture), rebuilds level 1 for the recipe's
/// thresholds (on the GPU for display frames of sources up to
/// [`GPU_RECONSTRUCTION_MAX_PIXELS`], else on the CPU) and sets the finest
/// threshold in the uniforms. Without a pyramid, or at Color 0, the recipe
/// falls back to the cross-bilateral filter (masks with colour noise
/// reduction use it too).
fn prepare_color_noise(
    context: &GpuContext,
    processor: &GpuProcessor,
    cache: &mut GpuImageCache,
    source: Option<&DynamicImage>,
    adjustments: &mut crate::image_processing::PreparedAdjustments,
    interactive: bool,
) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
    let global = &mut adjustments.uniforms.global;
    let luma_profile = luma_profile_wanted(global);
    let settings = color_noise_settings(global);
    if settings.is_none() && !luma_profile {
        return None;
    }
    if cache.chroma.is_none() {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("color_noise_pyramid");
        cache.chroma = source
            .and_then(|image| ChromaPyramid::build(image, global.is_raw_image != 0))
            .map(Arc::new);
    }
    let Some(pyramid) = cache.chroma.clone() else {
        log::warn!("No chroma pyramid for the render source; colour noise reduction falls back");
        global.color_noise[0] = 0.0;
        return None;
    };
    if luma_profile {
        set_luma_profile(global, &pyramid);
    }
    let settings = settings?;
    let pipelines = processor.chroma_pipelines.as_ref()?;
    let tuning = color_noise::Tuning::default();
    let (thresholds, base_sigma, key) = color_noise_thresholds(global, settings, &pyramid, &tuning);
    let level1 = &pyramid.levels[0];
    let window = [0, 0, level1.width as u32, level1.height as u32];
    global.color_noise_window = window;
    let on_gpu = interactive && level1.data.len() <= GPU_RECONSTRUCTION_MAX_PIXELS;
    let chroma = cache.chroma_gpu.get_or_insert_with(|| {
        Box::new(ChromaGpu::new(
            context,
            pipelines,
            &cache.texture_view,
            Roi {
                x: 0,
                y: 0,
                width: cache.width,
                height: cache.height,
            },
            cache.width,
            cache.height,
            global.is_raw_image != 0,
            &pyramid,
            None,
            window,
        ))
    });
    if chroma.key != Some(key) {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("color_noise_reconstruct");
        if on_gpu {
            chroma.rebuild_on_gpu(
                context,
                pipelines,
                &pyramid,
                &thresholds,
                base_sigma,
                &tuning,
            );
        } else {
            chroma.rebuild_on_cpu(context, &pyramid, &thresholds, base_sigma, &tuning);
        }
        chroma.key = Some(key);
    }
    Some((chroma.packed_view.clone(), chroma.energy_view.clone()))
}

pub struct GpuProcessorState {
    pub processor: GpuProcessor,
    pub width: u32,
    pub height: u32,
}

pub struct AnalyticsJob {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
}

pub struct AnalyticsConfig {
    pub path: String,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub sender: LatestJobSender<AnalyticsJob>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderOutputPrecision {
    #[default]
    EightBit,
    SixteenBit,
}

/// Shader family for a CPU-readback render.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadbackShader {
    /// Exact export arithmetic.
    #[default]
    Export,
    /// The display variants (prepared layers, curve tables). Within one 8-bit
    /// level of exports in the tested cases, but much faster for many layers.
    /// For previews such as thumbnails, never for exports; 8-bit only.
    Display,
    /// Foreground browser previews: display arithmetic with retained mask,
    /// blur and detail intermediates. Background jobs must not evict these.
    Preview,
}

pub struct RenderRequest<'a> {
    pub adjustments: rawtonic_engine::image_processing::PreparedAdjustments,
    pub mask_bitmaps: &'a [Arc<GrayImage>],
    pub lut: Option<Arc<Lut>>,
    pub roi: Option<Roi>,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DisplayTransform {
    pub rect: [f32; 4],
    pub clip: [f32; 4],
    pub window: [f32; 2],
    pub image_size: [f32; 2],
    pub texture_size: [f32; 2],
    pub pixelated: f32,
    /// Clockwise rotation of the image about the rect's centre, in radians
    /// (crop editing shows the straighten angle without re-rendering).
    pub rotation: f32,
    pub bg_primary: [f32; 4],
    pub bg_secondary: [f32; 4],
    /// Mask overlay colour (rgb) and opacity (a), all 0–1.
    pub overlay_color: [f32; 4],
    /// x: 0 no overlay, 1 coloured, 2 grayscale mask. Rest padding.
    pub overlay_mode: [f32; 4],
    /// Screen area that may show the image (x, y, w, h); zero width = no
    /// limit beyond `rect`. Crop editing clips the rotated image to its box.
    pub box_rect: [f32; 4],
    /// Before/after split: x 1 while it shows, y the divider's screen x
    /// (device pixels, the unit of `rect`). Rest padding. Set by `render`.
    pub split: [f32; 4],
}

/// Where the webview places the image: `rect` for normal frames (crop and
/// rotation applied), `crop_rect` and `crop_rotation` for crop-editing
/// frames (uncropped, unrotated). Each frame says which one it needs, so
/// the switch happens with the frame that needs it.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct DisplayGeometry {
    pub rect: [f32; 4],
    pub crop_rect: [f32; 4],
    pub crop_rotation: f32,
}

impl DisplayGeometry {
    /// Rect, rotation and clip box for a frame (see `DisplayTransform`).
    pub fn placement(&self, uncropped: bool) -> ([f32; 4], f32, [f32; 4]) {
        if uncropped {
            (self.crop_rect, self.crop_rotation, self.crop_rect)
        } else {
            (self.rect, 0.0, [0.0; 4])
        }
    }
}

/// The active mask layer drawn over the developed image, as Capture One shows
/// a mask while it is edited. The mask is at the displayed preview's size, so
/// it follows zoom and pan with the image and never crosses IPC.
#[derive(Clone)]
pub struct DisplayMaskOverlay {
    pub mask: Arc<GrayImage>,
    pub style: crate::mask_generation::OverlayStyle,
}

struct OverlayTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    mask: Arc<GrayImage>,
}

/// The split view's unedited render (the "before" side), kept on the GPU.
/// It is drawn only beside frames of the same photograph and framing
/// (`frame_key`), so a crop or the next photograph never shows a stale or
/// stretched before.
struct SplitBefore {
    bind_group: wgpu::BindGroup,
    /// What was rendered (source, recipe, resolution); a request with the
    /// same key needs no new render.
    render_key: u64,
    frame_key: u64,
}

/// Whether the split shows: a divider is set, the frame on screen is not the
/// uncropped crop-editing frame and the before belongs to its framing.
pub fn split_shows(
    split_x: Option<f32>,
    frame_uncropped: bool,
    before_frame_key: Option<u64>,
    frame_key: u64,
) -> bool {
    split_x.is_some() && !frame_uncropped && before_frame_key == Some(frame_key)
}

/// The display's render pipeline (display.wgsl) for a `format` target and
/// its two bind group layouts: frame (transform, image, sampler, mask
/// overlay) and the split view's before.
fn display_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> Result<
    (
        wgpu::RenderPipeline,
        wgpu::BindGroupLayout,
        wgpu::BindGroupLayout,
    ),
    String,
> {
    let shader = create_shader(
        device,
        "Display Shader",
        include_str!("shaders/display.wgsl"),
    )?;

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Display BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            },
        ],
    });

    let split_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Display Split BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            }],
        });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Display Pipeline Layout"),
        bind_group_layouts: &[Some(&bind_group_layout), Some(&split_bind_group_layout)],
        immediate_size: 0,
    });

    let pipeline = validated(device, "pipeline 'Display Pipeline'", || {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Display Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: NonZero::new(0),
            cache: None,
        })
    })?;
    Ok((pipeline, bind_group_layout, split_bind_group_layout))
}

pub struct WgpuDisplay {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    pub transform_buffer: wgpu::Buffer,
    pub latest_transform: DisplayTransform,
    pub current_bind_group: Option<wgpu::BindGroup>,
    /// Set by each preview job before it presents; `None` hides the overlay.
    pub overlay: Option<DisplayMaskOverlay>,
    overlay_texture: Option<OverlayTexture>,
    /// Bound while there is no overlay (the layout always has a mask texture).
    empty_overlay_view: wgpu::TextureView,
    pub geometry: DisplayGeometry,
    /// While the next photograph opens, the old frame stays on screen with
    /// its placement; the new photograph's placement waits here and is taken
    /// with its first frame, so the viewer never shows a gap or a stretched
    /// frame between them.
    pub pending_geometry: Option<DisplayGeometry>,
    /// The viewer's area (device pixels) while it shows the photograph to
    /// fit, so a native activation can place the next one exactly as the
    /// webview does (contain, centred); None while zoomed or cropping.
    pub viewport: Option<[f32; 4]>,
    /// Set by each preview job before it renders: whether its frame is the
    /// uncropped crop-editing frame. Applied when that frame is presented.
    pub next_frame_uncropped: bool,
    frame_uncropped: bool,
    /// Counts frames bound for presentation (profiling joins a preview job's
    /// frame to the present that shows it).
    frame_serial: u64,
    /// The before/after divider's screen x (device pixels); `None` = no split.
    pub split_x: Option<f32>,
    /// Set by each preview job before it renders: the photograph and framing
    /// of its frame (see `SplitBefore::frame_key`). Applied when presented.
    pub next_frame_split_key: u64,
    frame_split_key: u64,
    split_before: Option<SplitBefore>,
    split_bind_group_layout: wgpu::BindGroupLayout,
    /// Bound while there is no before (the layout always has a texture).
    empty_split_bind_group: wgpu::BindGroup,
    /// The photograph of the bound frame and its texture (multi view: the
    /// live frame is drawn only in its own cell, and stays as that cell's
    /// frame when another photograph becomes the primary).
    frame_path: Option<String>,
    frame_texture: Option<(wgpu::Texture, u32, u32)>,
    /// Other photographs shown beside the live one (multi_view.rs).
    multi: MultiView,
}

/// Where a photograph of `width` × `height` (as shown: cropped and turned)
/// sits in the viewer's `viewport` at fit: contained and centred, as the
/// webview's `useImageRenderSize` places it.
pub fn fit_placement(viewport: [f32; 4], width: f32, height: f32) -> [f32; 4] {
    let [x, y, w, h] = viewport;
    let (fit_w, fit_h) = if width / height > w / h {
        (w, w * height / width)
    } else {
        (h * width / height, h)
    };
    [x + (w - fit_w) / 2.0, y + (h - fit_h) / 2.0, fit_w, fit_h]
}

/// The split divider's screen x once the photograph moved from `old` to
/// `new` (x, y, w, h): the same point of the photograph.
pub fn split_x_resized(x: f32, old: [f32; 4], new: [f32; 4]) -> f32 {
    if old == new || old[2] <= 0.0 {
        return x;
    }
    new[0] + (x - old[0]) * new[2] / old[2]
}

/// The display's placement in a window of `window` device pixels.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DisplayPlacement {
    pub window: [f32; 2],
    pub clip: [f32; 4],
    pub viewport: Option<[f32; 4]>,
    pub geometry: DisplayGeometry,
}

impl DisplayPlacement {
    /// The placement once the window is `width` × `height`, until the
    /// webview sends its own. The image keeps its size and place in device
    /// pixels (with the old window size it would be stretched over the new
    /// surface). A viewer filling the whole window at fit (the photo alone,
    /// F) grows with it and the image is fitted again, as the webview will
    /// place it, so entering or leaving full screen shows no frame of the
    /// wrong size.
    pub fn resized(self, width: u32, height: u32, uncropped: bool) -> Self {
        let (width, height) = (width.max(1) as f32, height.max(1) as f32);
        let [old_w, old_h] = self.window;
        let near = |a: f32, b: f32| (a - b).abs() <= 1.0;
        let fills_window = self.viewport.filter(|&[x, y, w, h]| {
            near(x, 0.0) && near(y, 0.0) && near(x + w, old_w) && near(y + h, old_h)
        });
        let [_, _, rect_w, rect_h] = self.geometry.rect;
        match fills_window {
            Some(_) if !uncropped && rect_w > 0.0 && rect_h > 0.0 => {
                let viewport = [0.0, 0.0, width, height];
                let rect = fit_placement(viewport, rect_w, rect_h);
                let [cx, cy, cw, ch] = self.clip;
                Self {
                    window: [width, height],
                    clip: [cx, cy, cw + width - old_w, ch + height - old_h],
                    viewport: Some(viewport),
                    geometry: DisplayGeometry {
                        rect,
                        crop_rect: rect,
                        crop_rotation: 0.0,
                    },
                }
            }
            _ => Self {
                window: [width, height],
                ..self
            },
        }
    }
}

impl WgpuDisplay {
    /// Builds the display pipeline for a configured window surface.
    pub fn new(
        device: &wgpu::Device,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    ) -> Result<Self, String> {
        let (pipeline, bind_group_layout, split_bind_group_layout) =
            display_pipeline(device, config.format)?;

        let transform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Transform Buffer"),
            size: std::mem::size_of::<DisplayTransform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Display Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let empty_split_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Display Empty Before"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let empty_split_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Display Empty Before"),
            layout: &split_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&empty_split_view),
            }],
        });

        Ok(WgpuDisplay {
            surface,
            config,
            pipeline,
            bind_group_layout,
            transform_buffer,
            latest_transform: DisplayTransform {
                rect: [0.0, 0.0, 100.0, 100.0],
                clip: [0.0, 0.0, 10000.0, 10000.0],
                window: [1280.0, 720.0],
                image_size: [100.0, 100.0],
                texture_size: [100.0, 100.0],
                pixelated: 0.0,
                rotation: 0.0,
                bg_primary: [24.0 / 255.0, 24.0 / 255.0, 24.0 / 255.0, 1.0],
                bg_secondary: [35.0 / 255.0, 35.0 / 255.0, 35.0 / 255.0, 1.0],
                overlay_color: [0.0; 4],
                overlay_mode: [0.0; 4],
                box_rect: [0.0; 4],
                split: [0.0; 4],
            },
            geometry: DisplayGeometry {
                rect: [0.0, 0.0, 100.0, 100.0],
                crop_rect: [0.0, 0.0, 100.0, 100.0],
                crop_rotation: 0.0,
            },
            pending_geometry: None,
            viewport: None,
            next_frame_uncropped: false,
            frame_uncropped: false,
            frame_serial: 0,
            split_x: None,
            next_frame_split_key: 0,
            frame_split_key: 0,
            split_before: None,
            split_bind_group_layout,
            empty_split_bind_group,
            frame_path: None,
            frame_texture: None,
            multi: MultiView::default(),
            sampler,
            current_bind_group: None,
            overlay: None,
            overlay_texture: None,
            empty_overlay_view: device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("Display Empty Overlay"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default()),
        })
    }

    /// Write the placement for the frame on screen into the uniforms.
    pub fn apply_geometry(&mut self) {
        let t = &mut self.latest_transform;
        (t.rect, t.rotation, t.box_rect) = self.geometry.placement(self.frame_uncropped);
    }

    /// Called when a new frame is bound for presentation.
    pub fn present_frame_geometry(&mut self) {
        if let Some(geometry) = self.pending_geometry.take() {
            self.geometry = geometry;
        }
        self.frame_uncropped = self.next_frame_uncropped;
        self.frame_split_key = self.next_frame_split_key;
        self.apply_geometry();
    }

    /// The render key of the before kept for the split, if any.
    pub fn split_before_key(&self) -> Option<u64> {
        self.split_before.as_ref().map(|before| before.render_key)
    }

    /// Keep `image` (display-encoded RGBA, the unedited render of the frame
    /// `frame_key`) as the split's before side.
    pub fn set_split_before(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &image::RgbaImage,
        render_key: u64,
        frame_key: u64,
    ) {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            self.split_before = None;
            return;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Display Before"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            image.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Display Before"),
            layout: &self.split_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        self.split_before = Some(SplitBefore {
            bind_group,
            render_key,
            frame_key,
        });
    }

    /// Drop the split's before (the split was turned off).
    pub fn clear_split_before(&mut self) {
        self.split_before = None;
    }

    /// The split is single-view only: the multi view never shows it.
    fn split_active(&self) -> bool {
        !self.multi.is_active()
            && split_shows(
                self.split_x,
                self.frame_uncropped,
                self.split_before.as_ref().map(|before| before.frame_key),
                self.frame_split_key,
            )
    }

    /// Upload the overlay mask for a frame of `width` × `height` and set its
    /// uniforms; returns the view to bind. Only rows that changed since the
    /// last upload are written, so a brush move costs a few rows. A mask of
    /// another size than the frame is not drawn.
    pub fn prepare_overlay(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        use crate::mask_generation::OverlayStyle;
        let overlay = self
            .overlay
            .as_ref()
            .filter(|overlay| overlay.mask.dimensions() == (width, height));
        let Some(overlay) = overlay else {
            self.latest_transform.overlay_mode = [0.0; 4];
            return self.empty_overlay_view.clone();
        };
        let (color, mode) = match overlay.style {
            OverlayStyle::Colored { rgb, opacity } => (
                [
                    rgb[0] as f32 / 255.0,
                    rgb[1] as f32 / 255.0,
                    rgb[2] as f32 / 255.0,
                    opacity.min(100) as f32 / 100.0,
                ],
                1.0,
            ),
            OverlayStyle::Grayscale => ([0.0; 4], 2.0),
        };
        self.latest_transform.overlay_color = color;
        self.latest_transform.overlay_mode = [mode, 0.0, 0.0, 0.0];

        let mask = &overlay.mask;
        let write_rows = |texture: &wgpu::Texture, top: u32, rows: u32| {
            let start = top as usize * width as usize;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: top, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &mask.as_raw()[start..start + rows as usize * width as usize],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
        };
        match &mut self.overlay_texture {
            Some(current) if current.mask.dimensions() == (width, height) => {
                if !Arc::ptr_eq(&current.mask, mask) {
                    if let Some((top, bottom)) = changed_rows(&current.mask, mask) {
                        write_rows(&current.texture, top, bottom - top);
                    }
                    current.mask = Arc::clone(mask);
                }
            }
            slot => {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Display Mask Overlay"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                write_rows(&texture, 0, height);
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                *slot = Some(OverlayTexture {
                    texture,
                    view,
                    mask: Arc::clone(mask),
                });
            }
        }
        self.overlay_texture.as_ref().unwrap().view.clone()
    }

    /// The window is now `width` × `height` device pixels: reconfigures the
    /// surface and presents the current frame placed for that size (see
    /// `DisplayPlacement::resized`) until the webview sends its placement.
    /// The single view at fit is fitted again (a split divider keeps its
    /// place on the photograph); the multi view keeps its cells where they
    /// are (nothing stretched) until the webview packs the grid for the new
    /// size and sends the cells and the primary's placement.
    pub fn resize(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(device, &self.config);
        let current = DisplayPlacement {
            window: self.latest_transform.window,
            clip: self.latest_transform.clip,
            viewport: self.viewport,
            geometry: self.geometry,
        };
        let placement = if self.multi.is_active() {
            DisplayPlacement {
                window: [self.config.width as f32, self.config.height as f32],
                ..current
            }
        } else {
            current.resized(width, height, self.frame_uncropped)
        };
        self.split_x = self
            .split_x
            .map(|x| split_x_resized(x, current.geometry.rect, placement.geometry.rect));
        self.latest_transform.window = placement.window;
        self.latest_transform.clip = placement.clip;
        self.viewport = placement.viewport;
        self.geometry = placement.geometry;
        self.apply_geometry();
        queue.write_buffer(
            &self.transform_buffer,
            0,
            bytemuck::bytes_of(&self.latest_transform),
        );
        self.render(device, queue);
    }

    /// Whether the bound frame is drawn: always in the single view; in the
    /// multi view only when it shows the primary photograph where the
    /// webview placed it (not the previous primary held while the next one's
    /// first frame is on its way, nor a frame the webview has hidden).
    fn live_frame_shown(&self) -> bool {
        if !self.multi.is_active() {
            return true;
        }
        self.frame_path
            .as_deref()
            .is_some_and(|path| Some(path) == self.multi.primary())
            && self.latest_transform.rect[0] > -1.0e5
    }

    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let multi = self.multi.is_active();
        if self.current_bind_group.is_none() && !multi {
            return;
        }
        #[cfg(feature = "performance-profiling")]
        let acquire_stage = crate::performance::PerformanceStage::new("display_acquire");
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(tex)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
                    _ => panic!("Failed to acquire surface texture"),
                }
            }
            _ => return,
        };
        #[cfg(feature = "performance-profiling")]
        drop(acquire_stage);
        #[cfg(feature = "performance-profiling")]
        let _present_stage = crate::performance::PerformanceStage::new("display_submit_present");
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // The multi view's gaps take the viewer's background, as the area
        // around a single photograph does.
        let clear = if multi {
            self.multi.prepare(
                queue,
                [self.config.width as f32, self.config.height as f32],
                self.latest_transform.bg_secondary,
            );
            self.latest_transform.bg_secondary
        } else {
            self.latest_transform.bg_primary
        };
        // In the multi view the live frame covers exactly the primary's cell,
        // whatever clip the webview last sent (it follows the editor's
        // container a moment later when the primary changes).
        let mut transform = self.latest_transform;
        if let Some(cell) = self.multi.primary().and_then(|path| self.multi.cell(path)) {
            transform.clip = cell.area;
        }
        let split_active = self.split_active();
        transform.split = [
            if split_active { 1.0 } else { 0.0 },
            self.split_x.unwrap_or(0.0),
            0.0,
            0.0,
        ];
        self.latest_transform.split = transform.split;
        queue.write_buffer(&self.transform_buffer, 0, bytemuck::bytes_of(&transform));
        let before = match (&self.split_before, split_active) {
            (Some(before), true) => &before.bind_group,
            _ => &self.empty_split_bind_group,
        };
        let primary = self
            .current_bind_group
            .as_ref()
            .filter(|_| self.live_frame_shown())
            .map(|bind_group| PrimaryDraw {
                bind_group,
                before,
                clip: transform.clip,
            });
        encode_display_pass(
            &mut encoder,
            &view,
            [self.config.width, self.config.height],
            DisplayPass {
                pipeline: &self.pipeline,
                empty_before: &self.empty_split_bind_group,
            },
            clear,
            multi.then_some(&self.multi),
            primary,
        );
        queue.submit(Some(encoder.finish()));
        output.present();
        // Handed to the window server, not yet on screen. Pointer
        // sessions in the webview log join these by wall-clock time.
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "display_present", "epoch_ms": crate::performance::epoch_ms(),
                "frame": self.frame_serial, "multi_view_cells": self.multi.cells().len()
            })
        );
    }

    fn bindings(&self) -> DisplayBindings<'_> {
        DisplayBindings {
            layout: &self.bind_group_layout,
            sampler: &self.sampler,
            empty_overlay: &self.empty_overlay_view,
        }
    }

    /// The multi view shown now (empty: the single view).
    pub fn multi_view(&self) -> &MultiView {
        &self.multi
    }

    /// Shows `cells` beside each other with `primary` as the live photograph
    /// (no cells: back to the single view, releasing every cell texture).
    /// When the primary changes, the old primary's live frame stays as its
    /// cell's frame until that cell is rendered.
    pub fn set_multi_view(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        primary: Option<String>,
        cells: Vec<MultiViewCell>,
    ) {
        if cells.is_empty() {
            self.multi.clear();
            return;
        }
        let old_primary = self.multi.primary().map(str::to_string);
        let mut multi = std::mem::take(&mut self.multi);
        multi.set_layout(primary.clone(), cells);
        if let (Some(old), Some(frame)) = (old_primary, self.frame_path.as_deref())
            && primary.as_deref() != Some(old.as_str())
            && frame == old
            && let Some((texture, width, height)) = &self.frame_texture
        {
            multi.adopt_live_frame(
                device,
                queue,
                &self.bindings(),
                &old,
                texture,
                *width,
                *height,
            );
        }
        self.multi = multi;
    }

    /// Keeps a secondary cell's developed frame (display-encoded RGBA8): its
    /// whole photograph or, zoomed in, its visible region (`layer`).
    #[allow(clippy::too_many_arguments)]
    pub fn upload_cell(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &str,
        key: u64,
        extent: CellExtent,
        layer: CellLayer,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        let mut multi = std::mem::take(&mut self.multi);
        multi.upload(
            device,
            queue,
            &self.bindings(),
            path,
            key,
            extent,
            layer,
            width,
            height,
            rgba,
        );
        self.multi = multi;
    }

    /// Zooms every cell (Lightroom's locked zoom; None: fit).
    pub fn set_multi_view_zoom(&mut self, zoom: Option<CellZoom>) {
        self.multi.set_zoom(zoom);
    }

    /// Without the zoom lock: each secondary in `cells` at its own zoom, the
    /// others to fit; `shared` zooms all of them alike (the lock).
    pub fn set_multi_view_zooms(
        &mut self,
        shared: Option<CellZoom>,
        cells: std::collections::HashMap<String, CellZoom>,
    ) {
        self.multi.set_zooms(shared, cells);
    }
}

pub fn read_texture_data_roi(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    origin: wgpu::Origin3d,
    size: wgpu::Extent3d,
    bytes_per_pixel: u32,
) -> Result<Vec<u8>, String> {
    let pending = begin_texture_readback(device, queue, texture, origin, size, bytes_per_pixel);
    finish_texture_readback(device, pending)
}

/// A texture region copied into a mappable buffer whose mapping has been
/// requested; `finish_texture_readback` waits for it.
struct PendingReadback {
    buffer: wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    mapped: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
    height: u32,
}

/// Submits the copy of a texture region and requests the mapping without
/// waiting, so the GPU can go on with the next submission meanwhile.
fn begin_texture_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    origin: wgpu::Origin3d,
    size: wgpu::Extent3d,
    bytes_per_pixel: u32,
) -> PendingReadback {
    let unpadded_bytes_per_row = bytes_per_pixel * size.width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) & !(align - 1);
    let output_buffer_size = (padded_bytes_per_row * size.height) as u64;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: output_buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );

    let submission = queue.submit(Some(encoder.finish()));
    let (tx, mapped) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    PendingReadback {
        buffer,
        submission,
        mapped,
        padded_bytes_per_row,
        unpadded_bytes_per_row,
        height: size.height,
    }
}

/// Waits for a readback started by `begin_texture_readback` and returns the
/// tightly packed rows.
fn finish_texture_readback(
    device: &wgpu::Device,
    pending: PendingReadback,
) -> Result<Vec<u8>, String> {
    let PendingReadback {
        buffer,
        submission,
        mapped: rx,
        padded_bytes_per_row,
        unpadded_bytes_per_row,
        height,
    } = pending;
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(60)),
        })
        .map_err(|e| format!("Failed while polling mapped GPU buffer: {}", e))?;
    let map_result = rx
        .recv()
        .map_err(|e| format!("Failed receiving GPU map result: {}", e))?;
    map_result.map_err(|e| e.to_string())?;

    let buffer_slice = buffer.slice(..);
    let mapped = buffer_slice.get_mapped_range();
    let pixels = if padded_bytes_per_row == unpadded_bytes_per_row {
        mapped.to_vec()
    } else {
        let mut pixels = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in mapped.chunks_exact(padded_bytes_per_row as usize) {
            pixels.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
        }
        pixels
    };
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

/// Where a rendered tile goes in the output, relative to the output origin.
#[derive(Clone, Copy)]
struct TilePlacement {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Copies a read-back tile (RGBA8 or RGBA32F rows) into the output pixels.
fn store_tile(final_pixels: &mut RenderedPixels, tile: TilePlacement, out_width: u32, data: &[u8]) {
    match final_pixels {
        RenderedPixels::U8(final_pixels) => {
            for row in 0..tile.height {
                let final_row_offset = ((tile.y + row) * out_width + tile.x) as usize * 4;
                let source_row_offset = (row * tile.width) as usize * 4;
                let copy_bytes = (tile.width * 4) as usize;
                final_pixels[final_row_offset..final_row_offset + copy_bytes]
                    .copy_from_slice(&data[source_row_offset..source_row_offset + copy_bytes]);
            }
        }
        RenderedPixels::U16(final_pixels) => {
            for row in 0..tile.height {
                let final_row_offset = ((tile.y + row) * out_width + tile.x) as usize * 4;
                let source_pixel_offset = (row * tile.width) as usize;
                for sample in 0..(tile.width as usize * 4) {
                    let source_byte_offset = (source_pixel_offset * 4 + sample) * 4;
                    let value = f32::from_ne_bytes([
                        data[source_byte_offset],
                        data[source_byte_offset + 1],
                        data[source_byte_offset + 2],
                        data[source_byte_offset + 3],
                    ]);
                    final_pixels[final_row_offset + sample] = if value.is_finite() {
                        (value.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16
                    } else {
                        0
                    };
                }
            }
        }
    }
}

/// Converts the pixels of row `y` from column `x` on into the Rgba16Float
/// upload layout (`destination` holds 4 texels per pixel).
fn rgba_f16_row(img: &DynamicImage, x: u32, y: u32, destination: &mut [f16]) {
    let width = img.width() as usize;
    let first = y as usize * width + x as usize;
    let pixels = destination.len() / 4;
    // RAWs already contain linear RGB floats. Expanding them to an owned RGBA
    // float image first costs another 16 bytes per pixel (800 MB at 50 MP).
    // Convert directly into the upload payload, preserving the same f16 values.
    if let Some(rgb) = img.as_rgb32f() {
        let source = &rgb.as_raw()[first * 3..(first + pixels) * 3];
        for (rgba, rgb) in destination
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(source.as_chunks::<3>().0)
        {
            rgba[0] = f16::from_f32(rgb[0]);
            rgba[1] = f16::from_f32(rgb[1]);
            rgba[2] = f16::from_f32(rgb[2]);
            rgba[3] = f16::ONE;
        }
    } else if let Some(rgba) = img.as_rgba32f() {
        let source = &rgba.as_raw()[first * 4..(first + pixels) * 4];
        for (texel, value) in destination.iter_mut().zip(source) {
            *texel = f16::from_f32(*value);
        }
    } else {
        // Conversion is per pixel, so converting a copy of the row is exact.
        let source = img.crop_imm(x, y, pixels as u32, 1).to_rgba32f();
        for (texel, value) in destination.iter_mut().zip(source.as_raw()) {
            *texel = f16::from_f32(*value);
        }
    }
}

#[cfg(test)]
fn to_rgba_f16(img: &DynamicImage) -> Vec<f16> {
    let row_texels = (img.width() as usize * 4).max(1);
    let mut output = vec![f16::ZERO; img.width() as usize * 4 * img.height() as usize];
    for (y, row) in output.chunks_exact_mut(row_texels).enumerate() {
        rgba_f16_row(img, 0, y as u32, row);
    }
    output
}

/// Rows per input upload band; 256 rows of a 50 MP frame are 16 MB as f16.
const INPUT_UPLOAD_BAND_ROWS: u32 = 256;

/// Threads converting input textures for callers outside rayon (the
/// preview, export and multi view workers), apart from the global pool.
fn upload_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("gpu-upload-{i}"))
            .build()
            .expect("GPU upload thread pool")
    })
}

/// Creates the Rgba16Float input texture of `image` band by band.
///
/// Uploading in one call needs a full f16 copy and an equally large staging
/// buffer at the same time (8 bytes per pixel each, 400 MB apiece at 50 MP).
/// Here one converted band and at most two staged bands exist (16 MB each at
/// 50 MP). The texels are identical.
fn create_input_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &DynamicImage,
    band_rows: u32,
) -> Result<wgpu::Texture, String> {
    let (width, height) = image.dimensions();
    create_window_texture(
        device,
        queue,
        image,
        Roi {
            x: 0,
            y: 0,
            width,
            height,
        },
        band_rows,
    )
}

/// [`create_input_texture`] of `window` of `image` only.
fn create_window_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &DynamicImage,
    window: Roi,
    band_rows: u32,
) -> Result<wgpu::Texture, String> {
    use rayon::prelude::*;

    let Roi {
        x: window_x,
        y: window_y,
        width,
        height,
    } = window;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Input Texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        // Tests read the texels back.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | if cfg!(test) {
                wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::empty()
            },
        view_formats: &[],
    });
    let row_texels = width as usize * 4;
    let mut band = vec![f16::ZERO; row_texels * band_rows.min(height) as usize];
    let mut in_flight = None;
    for y in (0..height).step_by(band_rows as usize) {
        let rows = band_rows.min(height - y);
        let texels = &mut band[..row_texels * rows as usize];
        let convert = |texels: &mut [f16]| {
            texels
                .par_chunks_mut(row_texels)
                .enumerate()
                .for_each(|(row, texels)| {
                    rgba_f16_row(image, window_x, window_y + y + row as u32, texels)
                });
        };
        if rayon::current_thread_index().is_some() {
            // A rayon worker (thumbnails, batch work) may hold the GPU
            // processor lock here: waiting for rayon jobs could run another
            // task of the pool on this thread that waits for the same lock.
            for (row, texels) in texels.chunks_mut(row_texels).enumerate() {
                rgba_f16_row(image, window_x, window_y + y + row as u32, texels);
            }
        } else {
            // Not the global pool: its threads may all be waiting for the
            // processor lock this caller holds (thumbnail batches), and the
            // conversion would never run.
            upload_pool().install(|| convert(texels));
        }
        // Copied into a staging buffer at once, so the band is reusable.
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 8),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        // A write stays staged until a submission executes it. Keep one band
        // in flight while the next one converts.
        let submitted = queue.submit(std::iter::empty());
        if let Some(previous) = in_flight.replace(submitted) {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(previous),
                    timeout: Some(std::time::Duration::from_secs(60)),
                })
                .map_err(|e| format!("Failed while uploading the input texture: {}", e))?;
        }
    }
    Ok(texture)
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    radius: u32,
    tile_offset_x: u32,
    tile_offset_y: u32,
    input_width: u32,
    input_height: u32,
    source_x: u32,
    source_y: u32,
    full_width: u32,
    full_height: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FlareParams {
    amount: f32,
    is_raw: u32,
    exposure: f32,
    brightness: f32,
    contrast: f32,
    whites: f32,
    aspect_ratio: f32,
    gathered: u32,
    source_width: u32,
    source_height: u32,
    _pad: [u32; 2],
}

const PREVIEW_PIPELINE_COUNT: usize = 32;
/// The measured noise level above which the luminance noise reduction's
/// edge-preserving pass fades in (shader.wgsl `LUMA_NR_EDGE_NOISE.x`).
/// Exports of quieter photos, and of recipes without luminance noise
/// reduction, use pipelines compiled without it (`EDGE_NR` false): with the
/// pass present but idle, Metal compiled neighbouring arithmetic differently
/// and changed a few deep-shadow values by one 16-bit step.
const LUMA_NR_EDGE_NOISE_START: f32 = 0.005;
/// Display variant bits (see `create_preview_pipeline`): sharpen 1,
/// highlights 2, curves 4, masks 8, noise reduction 16.
const ALL_PREVIEW_FEATURES: usize = PREVIEW_PIPELINE_COUNT - 1;
/// A RAW with the default detail settings (Sharpening 40, Color 25).
const RAW_DEFAULT_PREVIEW_FEATURES: usize = 1 | 16;

#[derive(Clone)]
pub struct DisplayFrameIdentity {
    pub path: String,
    pub source: std::sync::Weak<DynamicImage>,
    pub load_generation: usize,
    pub render_generation: usize,
}

impl DisplayFrameIdentity {
    pub fn matches(
        &self,
        path: &str,
        source: &Arc<DynamicImage>,
        load: usize,
        render: usize,
    ) -> bool {
        self.path == path
            && self.source.ptr_eq(&Arc::downgrade(source))
            && self.load_generation == load
            && self.render_generation == render
    }
}

struct DisplayFrame {
    identity: DisplayFrameIdentity,
    roi: Roi,
}

pub fn preview_sample_roi(width: u32, height: u32, x: f64, y: f64) -> Option<Roi> {
    if width == 0
        || height == 0
        || !x.is_finite()
        || !y.is_finite()
        || !(0.0..=1.0).contains(&x)
        || !(0.0..=1.0).contains(&y)
    {
        return None;
    }
    let center_x = (x * width as f64).floor() as u32;
    let center_y = (y * height as f64).floor() as u32;
    let left = center_x.saturating_sub(5);
    let top = center_y.saturating_sub(5);
    Some(Roi {
        x: left,
        y: top,
        width: (center_x + 6).min(width) - left,
        height: (center_y + 6).min(height) - top,
    })
}

pub fn mean_rgb8(pixels: &[u8]) -> Option<[f64; 3]> {
    let pixels = pixels.as_chunks::<4>().0;
    if pixels.is_empty() {
        return None;
    }
    let mut sums = [0u64; 3];
    for pixel in pixels {
        for channel in 0..3 {
            sums[channel] += pixel[channel] as u64;
        }
    }
    Some(sums.map(|sum| sum as f64 / pixels.len() as f64))
}

struct DisplayTargets {
    width: u32,
    height: u32,
    working_texture: wgpu::Texture,
    output_texture: wgpu::Texture,
    output_texture_view: wgpu::TextureView,
}

pub struct GpuProcessor {
    context: GpuContext,
    blur_bgl: wgpu::BindGroupLayout,
    h_blur_pipeline: wgpu::ComputePipeline,
    v_blur_pipeline: wgpu::ComputePipeline,
    blur_params_buffer: wgpu::Buffer,

    flare_bgl_0: wgpu::BindGroupLayout,
    flare_bgl_1: wgpu::BindGroupLayout,
    flare_threshold_pipeline: wgpu::ComputePipeline,
    flare_ghosts_pipeline: wgpu::ComputePipeline,
    flare_params_buffer: wgpu::Buffer,
    flare_threshold_view: wgpu::TextureView,
    flare_ghosts_view: wgpu::TextureView,
    flare_final_view: wgpu::TextureView,
    flare_sampler: wgpu::Sampler,
    relight_depth_cache: Mutex<Option<([u32; 4], RelightDepthViews)>>,
    relight_pattern_cache: Mutex<Option<([u32; 2], wgpu::TextureView)>>,

    main_bgl: wgpu::BindGroupLayout,
    main_pipeline: wgpu::ComputePipeline,
    /// The general pipelines (8-bit, high precision) with the luminance
    /// noise reduction's edge-preserving pass, compiled on first use.
    edge_pipelines: [std::sync::OnceLock<Option<wgpu::ComputePipeline>>; 2],
    /// General pipelines with the luminance noise profile (recipes of the
    /// multi-scale colour noise reduction), by [high precision][edge pass];
    /// the others are compiled without it (`LUMA_PROFILE` false) so earlier
    /// recipes' exports stay bit-exact.
    profile_pipelines: [[std::sync::OnceLock<Option<wgpu::ComputePipeline>>; 2]; 2],
    detail_bgl: wgpu::BindGroupLayout,
    detail_layout: wgpu::PipelineLayout,
    /// Compiled off the render worker on first use; `None` if it failed.
    detail_pipeline: Arc<std::sync::OnceLock<Option<wgpu::ComputePipeline>>>,
    detail_compilation: std::sync::OnceLock<()>,
    preview_pipelines: Arc<[std::sync::OnceLock<wgpu::ComputePipeline>; PREVIEW_PIPELINE_COUNT]>,
    preview_compilation: [std::sync::OnceLock<()>; PREVIEW_PIPELINE_COUNT],
    preview_shader: wgpu::ShaderModule,
    preview_layout: wgpu::PipelineLayout,
    /// `None` records a variant that failed validation; exports use the general pipeline.
    export_pipelines: [std::sync::OnceLock<Option<wgpu::ComputePipeline>>; PREVIEW_PIPELINE_COUNT],
    high_precision_export_pipelines:
        [std::sync::OnceLock<Option<wgpu::ComputePipeline>>; PREVIEW_PIPELINE_COUNT],
    high_precision_shader: wgpu::ShaderModule,
    high_precision_layout: wgpu::PipelineLayout,
    high_precision_bgl: wgpu::BindGroupLayout,
    high_precision_pipeline: wgpu::ComputePipeline,
    high_precision_tile: std::sync::OnceLock<HighPrecisionTile>,
    tile_output_size: wgpu::Extent3d,
    adjustments_buffer: wgpu::Buffer,
    /// Count, then the tile-local regions of the current dispatch.
    render_regions_buffer: wgpu::Buffer,
    prepared_layers_buffer: wgpu::Buffer,
    curve_luts_buffer: wgpu::Buffer,
    curve_lut_cache: Mutex<Vec<CurveLutEntry>>,
    /// The camera-profile tables currently in `curve_luts_buffer` (key).
    camera_profile_loaded: Mutex<Option<u32>>,
    dummy_blur_view: wgpu::TextureView,
    empty_mask_view: wgpu::TextureView,
    mask_cache: Mutex<Option<MaskTextureCache>>,
    blur_cache: Mutex<BlurCache>,
    /// The multi-scale colour noise reduction's passes (None if they failed
    /// to compile: the cross-bilateral filter runs).
    chroma_pipelines: Option<ChromaPipelines>,
    #[cfg(test)]
    force_mask_upload: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    mask_upload_layers: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    mask_upload_bytes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    mask_cache_budget: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    force_full_preparation: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_inline_detail: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_fallback_variant: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_exact_curves: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_general_pipeline: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_legacy_curves: std::sync::atomic::AtomicBool,
    /// Renders as if the luminance noise reduction had no edge-preserving pass.
    #[cfg(test)]
    force_plain_noise_reduction: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    force_unprepared_layers: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    unprepared_layer_pipelines:
        [std::sync::OnceLock<wgpu::ComputePipeline>; PREVIEW_PIPELINE_COUNT],
    #[cfg(test)]
    legacy_curve_pipelines: [std::sync::OnceLock<wgpu::ComputePipeline>; PREVIEW_PIPELINE_COUNT],
    dummy_lut_view: wgpu::TextureView,
    dummy_lut_sampler: wgpu::Sampler,
    ping_pong_view: wgpu::TextureView,

    pub tile_output_texture: wgpu::Texture,
    pub tile_output_texture_view: wgpu::TextureView,
    // Full-image presentation targets are unnecessary for CPU readback jobs.
    // Keep only the current display size instead of the largest image/export.
    display_targets: Mutex<Option<Arc<DisplayTargets>>>,
    analytics_readback: std::sync::OnceLock<LatestJobSender<AnalyticsReadbackJob>>,
    display_frame: Mutex<Option<DisplayFrame>>,
    display_render_state: Mutex<Option<DisplayRenderState>>,
    #[cfg(test)]
    force_full_display_render: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    last_display_render_pixels: std::sync::atomic::AtomicU64,
}

/// One active job and one replaceable latest pending job. The signal channel
/// carries no image buffers; dropping the sender stops the worker after draining.
pub struct LatestJobSender<T> {
    pending: Arc<Mutex<Option<T>>>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl<T> Clone for LatestJobSender<T> {
    fn clone(&self) -> Self {
        Self {
            pending: Arc::clone(&self.pending),
            wake: self.wake.clone(),
        }
    }
}

impl<T> LatestJobSender<T> {
    pub fn send(&self, job: T) {
        *self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(job);
        let _ = self.wake.try_send(());
    }
}

pub fn start_latest_job_worker<T: Send + 'static>(
    mut process: impl FnMut(T) + Send + 'static,
) -> LatestJobSender<T> {
    let pending = Arc::new(Mutex::new(None));
    let worker_pending = Arc::clone(&pending);
    let (wake, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        while receive.recv().is_ok() {
            let job = worker_pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(job) = job {
                process(job);
            }
        }
    });
    LatestJobSender { pending, wake }
}

struct AnalyticsReadbackJob {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    analytics: AnalyticsConfig,
}

fn complete_analytics_readback(device: &wgpu::Device, job: AnalyticsReadbackJob) {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("analytics_readback");
    let buffer_slice = job.buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    // A blocking wgpu poll holds the device fence lock until the GPU finishes,
    // which stalls every other thread's submit and present, including the
    // frame this readback belongs to. Poll briefly instead.
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let mapped = loop {
        if let Err(error) = device.poll(wgpu::PollType::Poll) {
            log::error!("Async analytics readback poll failed: {error}");
            return;
        }
        match rx.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
        }
    };
    if mapped.is_err() {
        return;
    }
    let mapped = buffer_slice.get_mapped_range();
    let unpadded_bytes_per_row = job.width as usize * 4;
    let pixels = if job.padded_bytes_per_row as usize == unpadded_bytes_per_row {
        mapped.to_vec()
    } else {
        let mut pixels = Vec::with_capacity(unpadded_bytes_per_row * job.height as usize);
        for row in mapped.chunks_exact(job.padded_bytes_per_row as usize) {
            pixels.extend_from_slice(&row[..unpadded_bytes_per_row]);
        }
        pixels
    };
    drop(mapped);
    job.buffer.unmap();
    if let Some(image) = ImageBuffer::<Rgba<u8>, _>::from_raw(job.width, job.height, pixels) {
        job.analytics.sender.send(AnalyticsJob {
            path: job.analytics.path,
            image: Arc::new(DynamicImage::ImageRgba8(image)),
            compute_waveform: job.analytics.compute_waveform,
            active_waveform_channel: job.analytics.active_waveform_channel,
        });
    }
}

/// Strong references keep mask pixels immutable while their uploaded layers are
/// retained. Arc::make_mut creates a new identity before modifying cached pixels.
/// A single entry bounds retention across images and preview resolutions.
struct MaskTextureCache {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    masks: Vec<Arc<GrayImage>>,
    bytes: usize,
}

#[derive(Clone)]
struct RelightDepthViews {
    legacy: wgpu::TextureView,
    camera: wgpu::TextureView,
    scene: wgpu::TextureView,
}

const MASK_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// How a frame's mask texture relates to the previous display frame's.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MaskTextureUpdate {
    /// Not retained, so the next frame cannot be compared with this one.
    Transient,
    /// The retained array was rebuilt; nothing is known about the previous frame.
    Rebuilt,
    /// The retained array was reused. Holds disjoint rectangles covering every
    /// changed mask pixel; empty when all layers are unchanged.
    Reused(Vec<Roi>),
}

/// Changed mask pixels are bounded per band of rows, so a long diagonal stroke
/// costs roughly its own area instead of its whole bounding box.
const DIRTY_BAND_ROWS: usize = 64;
/// Regions one dispatch can render; more are rendered as their bounding box.
const MAX_RENDER_REGIONS: usize = 255;
/// Width of the flat workgroup grid of a region dispatch (shader.wgsl reads
/// it from num_workgroups).
const RENDER_REGION_GROUPS_X: u32 = 256;

/// Chunk equality compiles to wide compares; only a differing chunk is
/// searched byte by byte.
const DIFFERENCE_CHUNK: usize = 32;

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    debug_assert_eq!(a.len(), b.len());
    let (index, (a, b)) = a
        .chunks(DIFFERENCE_CHUNK)
        .zip(b.chunks(DIFFERENCE_CHUNK))
        .enumerate()
        .find(|(_, (a, b))| a != b)?;
    let offset = a.iter().zip(b).position(|(a, b)| a != b)?;
    Some(index * DIFFERENCE_CHUNK + offset)
}

fn last_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    debug_assert_eq!(a.len(), b.len());
    let (index, (chunk_a, chunk_b)) = a
        .rchunks(DIFFERENCE_CHUNK)
        .zip(b.rchunks(DIFFERENCE_CHUNK))
        .enumerate()
        .find(|(_, (a, b))| a != b)?;
    let offset = chunk_a.iter().zip(chunk_b).rposition(|(a, b)| a != b)?;
    // rchunks align to the end, so only the first chunk may be shorter.
    Some(a.len().saturating_sub((index + 1) * DIFFERENCE_CHUNK) + offset)
}
/// The rows (top, bottom exclusive) in which two equally sized masks differ.
fn changed_rows(previous: &GrayImage, current: &GrayImage) -> Option<(u32, u32)> {
    debug_assert_eq!(previous.dimensions(), current.dimensions());
    let width = current.width() as usize;
    if width == 0 {
        return None;
    }
    let differs = |(old, new): (&[u8], &[u8])| old != new;
    let rows = || {
        previous
            .as_raw()
            .chunks_exact(width)
            .zip(current.as_raw().chunks_exact(width))
    };
    let top = rows().position(differs)?;
    let bottom = rows().rposition(differs)? + 1;
    Some((top as u32, bottom as u32))
}

/// Widen `bands` (one entry per `DIRTY_BAND_ROWS` rows) by the pixels that
/// differ between two equally sized masks. Returns the changed row range.
fn add_changed_mask_bands(
    previous: &GrayImage,
    current: &GrayImage,
    bands: &mut [Option<Roi>],
) -> Option<(u32, u32)> {
    debug_assert_eq!(previous.dimensions(), current.dimensions());
    let width = current.width() as usize;
    if width == 0 {
        return None;
    }
    let mut rows_changed: Option<(u32, u32)> = None;
    let rows = previous
        .as_raw()
        .chunks_exact(width)
        .zip(current.as_raw().chunks_exact(width));
    for (y, (old_row, new_row)) in rows.enumerate() {
        // Slice equality is a vectorized memcmp; only differing rows are searched.
        if old_row == new_row {
            continue;
        }
        let band = &mut bands[y / DIRTY_BAND_ROWS];
        let (left, right) = band.map_or((width, 0), |band| {
            (band.x as usize, (band.x + band.width) as usize)
        });
        // Only columns outside the band's current bounds can still widen them.
        let first = first_difference(&old_row[..left], &new_row[..left]).unwrap_or(left);
        let last = last_difference(&old_row[right..], &new_row[right..])
            .map_or(right, |offset| right + offset + 1);
        let row = Roi {
            x: first as u32,
            y: y as u32,
            width: (last - first) as u32,
            height: 1,
        };
        *band = Some(union_roi(*band, row));
        rows_changed =
            Some(rows_changed.map_or((y as u32, y as u32 + 1), |(top, _)| (top, y as u32 + 1)));
    }
    rows_changed
}

/// Merge consecutive bands while the combined rectangle stays close to the
/// bands' own area. Each remaining rectangle costs one small dispatch.
fn merge_dirty_bands(bands: &[Option<Roi>]) -> Vec<Roi> {
    let area = |rect: Roi| rect.width as u64 * rect.height as u64;
    let mut merged = Vec::new();
    let mut current: Option<(Roi, u64)> = None;
    for band in bands.iter().flatten() {
        current = Some(match current {
            None => (*band, area(*band)),
            Some((rect, covered)) => {
                let union = union_roi(Some(rect), *band);
                let covered = covered + area(*band);
                if area(union) * 4 <= covered * 5 {
                    (union, covered)
                } else {
                    merged.push(rect);
                    (*band, area(*band))
                }
            }
        });
    }
    merged.extend(current.map(|(rect, _)| rect));
    merged
}

fn union_roi(a: Option<Roi>, b: Roi) -> Roi {
    let Some(a) = a else {
        return b;
    };
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Roi {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

fn intersect_roi(a: Roi, b: Roi) -> Option<Roi> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (x < right && y < bottom).then(|| Roi {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Everything except mask pixels that determines the last complete display
/// frame in the working texture. The shader reads each mask texel only for its
/// own output pixel, so a matching frame may re-render just the changed pixels.
struct DisplayRenderState {
    input: wgpu::TextureView,
    targets: std::sync::Weak<DisplayTargets>,
    dimensions: (u32, u32),
    bounds: Roi,
    adjustments: AllAdjustments,
    lut: Option<std::sync::Weak<Lut>>,
    pipeline: wgpu::ComputePipeline,
    mask_layers: usize,
}

impl DisplayRenderState {
    fn matches(&self, other: &Self) -> bool {
        self.input == other.input
            && self.targets.ptr_eq(&other.targets)
            && self.dimensions == other.dimensions
            && self.bounds == other.bounds
            && bytemuck::bytes_of(&self.adjustments) == bytemuck::bytes_of(&other.adjustments)
            && match (&self.lut, &other.lut) {
                (None, None) => true,
                (Some(a), Some(b)) => a.ptr_eq(b),
                _ => false,
            }
            && self.pipeline == other.pipeline
            && self.mask_layers == other.mask_layers
    }
}

/// Keep immutable source-dependent intermediates across slider frames, including
/// previews wider than one tile. Allocate only used effects and bound retention.
#[derive(Default)]
struct BlurCache {
    input: Option<wgpu::TextureView>,
    tiles: std::collections::VecDeque<BlurTile>,
    bytes: usize,
}

/// With the luminance noise reduction's three blur levels and the detail
/// stage, the app's largest preview (2304 x 1728, two tiles) needs 177 MB
/// and a four-tile 2200 x 2100 frame 231 MB (192 MB before 2026-10-07).
const BLUR_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// Bytes the blur cache needs to keep every tile of a `width` x `height`
/// display frame with `blurs` blur levels and the detail stage (Rgba16Float
/// blurs and Rgba32Float detail over each tile's input, overlap included).
/// Detail caching needs all tiles to fit at once: otherwise each frame would
/// evict the tile the next one needs and rebuild it.
fn display_cache_bytes(width: u32, height: u32, tile: u32, overlap: u32, blurs: usize) -> usize {
    let span = |extent: u32| {
        (0..extent.div_ceil(tile)).map(move |index| {
            let start = (index * tile).saturating_sub(overlap);
            let end = ((index + 1) * tile + overlap).min(extent);
            (end - start) as usize
        })
    };
    span(width)
        .flat_map(|w| span(height).map(move |h| w * h * (blurs * 8 + 16)))
        .sum()
}

struct BlurTile {
    geometry: [u32; 6],
    views: [Option<wgpu::TextureView>; 5],
    /// The detail stage of this tile and the settings it was computed with.
    detail: Option<(DetailKey, wgpu::TextureView)>,
    bytes: usize,
}

/// Everything the cached detail stage depends on besides the input and the
/// tile: the global noise-reduction and sharpening settings, the preview
/// scale of the sharpening radius and the chromatic aberration correction.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DetailKey([u32; 26]);

impl DetailKey {
    fn of(global: &crate::image_processing::GlobalAdjustments) -> Self {
        Self([
            global.luma_noise_reduction.to_bits(),
            global.color_noise_reduction.to_bits(),
            global.sharpness.to_bits(),
            global.sharpness_threshold.to_bits(),
            global.detail_scale.to_bits(),
            global.sharpen_radius_offset.to_bits(),
            global.sharpen_detail_offset.to_bits(),
            global.sharpen_masking.to_bits(),
            global.chromatic_aberration_red_cyan.to_bits(),
            global.chromatic_aberration_blue_yellow.to_bits(),
            global.is_raw_image,
            global.noise_level.to_bits(),
            global.sharpen_falloff[0].to_bits(),
            global.sharpen_falloff[1].to_bits(),
            global.sharpen_falloff[2].to_bits(),
            global.moire[0].to_bits(),
            global.moire[1].to_bits(),
            global.noise_detail[0].to_bits(),
            global.noise_detail[1].to_bits(),
            global.noise_detail[2].to_bits(),
            global.noise_detail[3].to_bits(),
            global.output_space[0],
            global.color_noise[0].to_bits(),
            global.color_noise[1].to_bits(),
            global.color_noise[2].to_bits(),
            global.color_noise[3].to_bits(),
        ])
    }

    /// Whether there is a detail stage to cache at all.
    fn active(global: &crate::image_processing::GlobalAdjustments) -> bool {
        global.sharpness != 0.0
            || global.luma_noise_reduction != 0.0
            || global.color_noise_reduction != 0.0
            || global.sharpen_falloff[2] != 0.0
            || global.moire[0] != 0.0
    }
}

fn required_blurs(adjustments: &AllAdjustments) -> [bool; 5] {
    let g = &adjustments.global;
    // Structure uses the small blur (index 2) and Clarity the large one
    // (index 3), as their Capture One and Lightroom counterparts do.
    // Noise reduction samples the small blur too; colour noise reduction
    // also the tonal one.
    // Luminance noise reduction uses the 1 px (index 4) and tonal blurs as
    // its bands.
    let luma_nr = g.luma_noise_reduction != 0.0;
    let mut required = [
        g.sharpness != 0.0 || g.color_noise_reduction != 0.0 || luma_nr,
        g.sharpness != 0.0
            || g.shadows != 0.0
            || g.blacks != 0.0
            || g.color_noise_reduction != 0.0
            || luma_nr,
        g.structure != 0.0 || g.centré != 0.0 || g.halation_amount > 0.0,
        g.clarity != 0.0 || g.dehaze != 0.0 || g.glow_amount > 0.0,
        luma_nr,
    ];
    for m in adjustments
        .mask_adjustments
        .iter()
        .take(adjustments.mask_count as usize)
    {
        required[0] |=
            m.sharpness != 0.0 || m.color_noise_reduction != 0.0 || m.luma_noise_reduction != 0.0;
        required[1] |= m.sharpness != 0.0
            || m.shadows != 0.0
            || m.blacks != 0.0
            || m.color_noise_reduction != 0.0;
        required[2] |= m.structure != 0.0 || m.halation_amount > 0.0;
        required[3] |= m.clarity != 0.0 || m.dehaze != 0.0 || m.glow_amount > 0.0;
        if m.luma_noise_reduction != 0.0 {
            required[1] = true;
            required[4] = true;
        }
    }
    required
}

/// Missing/bypassed curves (fewer than two points) return their input in WGSL;
/// its final clamp matches the bypass path. Otherwise skip only exact two-point
/// identities, retaining interpolation for near-identity values and extra points.
fn curves_are_identity(curves: [(&[Point; 16], u32); 4]) -> bool {
    curves.iter().all(|(points, count)| {
        *count < 2
            || (*count == 2
                && points[0].x == 0.0
                && points[0].y == 0.0
                && points[1].x == 255.0
                && points[1].y == 255.0)
    })
}

/// Whether a render needs the luminance noise reduction's edge-preserving
/// pass: a measured noise level above its start and luminance noise
/// reduction anywhere in the recipe.
fn edge_noise_reduction(adjustments: &AllAdjustments) -> bool {
    let g = &adjustments.global;
    let luma = g.luma_noise_reduction != 0.0
        || adjustments
            .mask_adjustments
            .iter()
            .take(adjustments.mask_count as usize)
            .any(|m| m.luma_noise_reduction != 0.0);
    luma && g.noise_level > LUMA_NR_EDGE_NOISE_START
}

/// Bits: sharpening, highlights, curves, masks, noise reduction. An operation
/// must remain enabled if any uploaded local adjustment can contribute to it.
fn preview_features(adjustments: &AllAdjustments) -> usize {
    let g = &adjustments.global;
    let mut sharpen = g.sharpness != 0.0;
    let mut highlights = g.highlights != 0.0;
    let mut curves = !curves_are_identity([
        (&g.luma_curve, g.luma_curve_count),
        (&g.red_curve, g.red_curve_count),
        (&g.green_curve, g.green_curve_count),
        (&g.blue_curve, g.blue_curve_count),
    ]);
    let mut noise_reduction = g.luma_noise_reduction != 0.0 || g.color_noise_reduction != 0.0;
    for m in adjustments
        .mask_adjustments
        .iter()
        .take(adjustments.mask_count as usize)
    {
        sharpen |= m.sharpness != 0.0;
        highlights |= m.highlights != 0.0;
        curves |= !curves_are_identity([
            (&m.luma_curve, m.luma_curve_count),
            (&m.red_curve, m.red_curve_count),
            (&m.green_curve, m.green_curve_count),
            (&m.blue_curve, m.blue_curve_count),
        ]);
        noise_reduction |= m.luma_noise_reduction != 0.0 || m.color_noise_reduction != 0.0;
    }
    usize::from(sharpen)
        | (usize::from(highlights) << 1)
        | (usize::from(curves) << 2)
        | (usize::from(adjustments.mask_count > 0) << 3)
        | (usize::from(noise_reduction) << 4)
}

/// Per mask layer: the curve data its table was built from, and whether the
/// table passed the tolerance check. Rebuilt only when that layer changes.
type CurveLutEntry = Option<(Vec<u8>, bool)>;

struct HighPrecisionTile {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

enum RenderedPixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// 16-bit exports write 32-bit floats: a float16 tile (10-bit mantissa)
/// left them in steps of 32 codes near white.
fn high_precision_shader_source(source: &str) -> String {
    source.replace("rgba8unorm, write>", "rgba32float, write>")
}

/// Runs a shader-module or pipeline creation inside error scopes. wgpu otherwise
/// returns an invalid handle and only reports the failure to the uncaptured
/// error handler, after which every dispatch silently writes nothing.
/// Error scopes are per thread, so concurrent work elsewhere is not captured.
fn validated<T>(
    device: &wgpu::Device,
    what: &str,
    create: impl FnOnce() -> T,
) -> Result<T, String> {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = create();
    let validation = pollster::block_on(validation.pop());
    let internal = pollster::block_on(internal.pop());
    match validation.or(internal) {
        Some(error) => Err(format!("GPU {what} could not be created: {error}")),
        None => Ok(value),
    }
}

fn create_shader(
    device: &wgpu::Device,
    label: &str,
    source: &str,
) -> Result<wgpu::ShaderModule, String> {
    validated(device, &format!("shader '{label}'"), || {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        })
    })
}

fn create_compute_pipeline(
    device: &wgpu::Device,
    descriptor: &wgpu::ComputePipelineDescriptor,
) -> Result<wgpu::ComputePipeline, String> {
    let label = descriptor.label.unwrap_or("compute");
    validated(device, &format!("pipeline '{label}'"), || {
        device.create_compute_pipeline(descriptor)
    })
}

fn create_preview_pipeline(
    device: &wgpu::Device,
    shader_module: &wgpu::ShaderModule,
    pipeline_layout: &wgpu::PipelineLayout,
    features: usize,
    direct_curves: bool,
    prepared_layers: bool,
) -> Result<wgpu::ComputePipeline, String> {
    create_compute_pipeline(
        device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("Specialized image processing"),
            layout: Some(pipeline_layout),
            module: shader_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[
                    ("DIRECT_CURVES", u8::from(direct_curves) as f64),
                    ("PREPARED_LAYERS", u8::from(prepared_layers) as f64),
                    ("ENABLE_MASKS", ((features >> 3) & 1) as f64),
                    ("ENABLE_NOISE_REDUCTION", ((features >> 4) & 1) as f64),
                    ("ENABLE_SHARPEN", (features & 1) as f64),
                    ("ENABLE_HIGHLIGHTS", ((features >> 1) & 1) as f64),
                    ("ENABLE_CURVES", ((features >> 2) & 1) as f64),
                ],
                ..Default::default()
            },
            cache: None,
        },
    )
}

const FLARE_MAP_SIZE: u32 = 512;

/// Renders run in tiles of this size (photo pixels, aligned to the photo's
/// origin); each tile's blurs cover the tile and `TILE_OVERLAP` around it.
const TILE_SIZE: u32 = 2048;
const TILE_OVERLAP: u32 = 128;

impl GpuProcessor {
    pub fn new(context: GpuContext, max_width: u32, max_height: u32) -> Result<Self, String> {
        Self::with_shader_source(
            context,
            max_width,
            max_height,
            include_str!("shaders/shader.wgsl"),
        )
    }

    /// `new` with the image-processing WGSL supplied by the caller, so tests can
    /// prove that an invalid shader fails construction instead of rendering black.
    fn with_shader_source(
        context: GpuContext,
        max_width: u32,
        max_height: u32,
        shader_source: &str,
    ) -> Result<Self, String> {
        let device = &context.device;
        const MAX_MASK_BINDINGS: u32 = 1;

        let blur_shader_module =
            create_shader(device, "Blur Shader", include_str!("shaders/blur.wgsl"))?;

        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blur BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blur Pipeline Layout"),
            bind_group_layouts: &[Some(&blur_bgl)],
            immediate_size: 0,
        });

        let h_blur_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("Horizontal Blur Pipeline"),
                layout: Some(&blur_pipeline_layout),
                module: &blur_shader_module,
                entry_point: Some("horizontal_blur"),
                compilation_options: Default::default(),
                cache: None,
            },
        )?;

        let v_blur_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("Vertical Blur Pipeline"),
                layout: Some(&blur_pipeline_layout),
                module: &blur_shader_module,
                entry_point: Some("vertical_blur"),
                compilation_options: Default::default(),
                cache: None,
            },
        )?;

        let blur_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Blur Params Buffer"),
            size: std::mem::size_of::<BlurParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let flare_shader =
            create_shader(device, "Flare Shader", include_str!("shaders/flare.wgsl"))?;

        let flare_bgl_0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flare BGL 0"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let flare_bgl_1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flare BGL 1"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let flare_threshold_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Flare Threshold Layout"),
                bind_group_layouts: &[Some(&flare_bgl_0)],
                immediate_size: 0,
            });

        let flare_ghosts_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Flare Ghosts Layout"),
            bind_group_layouts: &[Some(&flare_bgl_0), Some(&flare_bgl_1)],
            immediate_size: 0,
        });

        let flare_threshold_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("Flare Threshold Pipeline"),
                layout: Some(&flare_threshold_layout),
                module: &flare_shader,
                entry_point: Some("threshold_main"),
                compilation_options: Default::default(),
                cache: None,
            },
        )?;

        let flare_ghosts_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("Flare Ghosts Pipeline"),
                layout: Some(&flare_ghosts_layout),
                module: &flare_shader,
                entry_point: Some("ghosts_main"),
                compilation_options: Default::default(),
                cache: None,
            },
        )?;

        let flare_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Flare Params Buffer"),
            size: std::mem::size_of::<FlareParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let flare_tex_desc = wgpu::TextureDescriptor {
            label: Some("Flare Tex"),
            size: wgpu::Extent3d {
                width: FLARE_MAP_SIZE,
                height: FLARE_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        };

        let flare_threshold_texture = device.create_texture(&flare_tex_desc);
        let flare_threshold_view = flare_threshold_texture.create_view(&Default::default());
        let flare_ghosts_texture = device.create_texture(&flare_tex_desc);
        let flare_ghosts_view = flare_ghosts_texture.create_view(&Default::default());
        let flare_final_texture = device.create_texture(&flare_tex_desc);
        let flare_final_view = flare_final_texture.create_view(&Default::default());

        let flare_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Flare Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let chroma_pipelines = ChromaPipelines::new(device)
            .inspect_err(|e| log::error!("Colour noise passes failed: {e}"))
            .ok();
        let shader_module = create_shader(device, "Image Processing Shader", shader_source)?;

        let mut bind_group_layout_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 4 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 5 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 6 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 7 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 8 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 17,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 18,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 19,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 20,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 21,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        for binding in [22, 23] {
            bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 9 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 10 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 12,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 13,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 16,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 14,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });

        let main_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Main BGL"),
            entries: &bind_group_layout_entries,
        });
        // The detail pass writes the cache that the main pass reads.
        let mut detail_entries = bind_group_layout_entries.clone();
        detail_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 15,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba32Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        });
        let detail_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Detail BGL"),
            entries: &detail_entries,
        });
        let detail_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Detail Pipeline Layout"),
            bind_group_layouts: &[Some(&detail_bgl)],
            immediate_size: 0,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pipeline Layout"),
            bind_group_layouts: &[Some(&main_bgl)],
            immediate_size: 0,
        });

        let main_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("Compute Pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader_module,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("EDGE_NR", 0.0), ("LUMA_PROFILE", 0.0)],
                    ..Default::default()
                },
                cache: None,
            },
        )?;

        // The common variant is ready with the first photograph. Other variants
        // compile on demand off the render worker; use the complete pipeline until
        // ready. Avoid compiling unused variants during thumbnail/export startup.
        let preview_pipelines: Arc<
            [std::sync::OnceLock<wgpu::ComputePipeline>; PREVIEW_PIPELINE_COUNT],
        > = Arc::new(std::array::from_fn(|_| std::sync::OnceLock::new()));
        let _ = preview_pipelines[0].set(create_preview_pipeline(
            device,
            &shader_module,
            &pipeline_layout,
            0,
            true,
            true,
        )?);

        bind_group_layout_entries[1].ty = wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        };
        let high_precision_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("High Precision Main BGL"),
                entries: &bind_group_layout_entries,
            });
        let high_precision_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("High Precision Pipeline Layout"),
                bind_group_layouts: &[Some(&high_precision_bgl)],
                immediate_size: 0,
            });
        let high_precision_shader = create_shader(
            device,
            "High Precision Image Processing Shader",
            &high_precision_shader_source(shader_source).replace(
                "override HIGH_PRECISION_OUTPUT: u32 = 0u;",
                "override HIGH_PRECISION_OUTPUT: u32 = 1u;",
            ),
        )?;
        let high_precision_pipeline = create_compute_pipeline(
            device,
            &wgpu::ComputePipelineDescriptor {
                label: Some("High Precision Compute Pipeline"),
                layout: Some(&high_precision_pipeline_layout),
                module: &high_precision_shader,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[
                        ("HIGH_PRECISION_OUTPUT", 1.0),
                        ("EDGE_NR", 0.0),
                        ("LUMA_PROFILE", 0.0),
                    ],
                    ..Default::default()
                },
                cache: None,
            },
        )?;

        let adjustments_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Adjustments Buffer"),
            size: std::mem::size_of::<AllAdjustments>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let render_regions_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Render Regions Buffer"),
            size: ((MAX_RENDER_REGIONS + 1) * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let prepared_layers_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Prepared preview layers"),
            size: (std::mem::size_of::<PreparedLayer>() * (MAX_MASKS + 1)) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let curve_luts_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Display curve tables"),
            // Mask layers' curve tables, then a camera profile's colour
            // tables.
            size: ((CURVE_LUT_SAMPLES * (MAX_MASKS + 1) + crate::camera_profile::TABLE_CAPACITY)
                * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dummy_texture_desc = wgpu::TextureDescriptor {
            label: Some("Dummy Texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let dummy_blur_texture = device.create_texture(&dummy_texture_desc);
        let dummy_blur_view = dummy_blur_texture.create_view(&Default::default());

        // No masks: bind a zero-initialized 1×1 array instead of allocating and
        // uploading two full-size empty layers for every slider frame.
        let empty_mask_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Empty mask array"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 2,
            },
            format: wgpu::TextureFormat::R8Unorm,
            ..dummy_texture_desc
        });
        let empty_mask_view = empty_mask_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let dummy_lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            dimension: wgpu::TextureDimension::D3,
            ..dummy_texture_desc
        });
        let dummy_lut_view = dummy_lut_texture.create_view(&Default::default());
        let dummy_lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

        let clamped_tile_width = max_width.min(TILE_SIZE + TILE_OVERLAP * 2);
        let clamped_tile_height = max_height.min(TILE_SIZE + TILE_OVERLAP * 2);

        let clamped_tile_size = wgpu::Extent3d {
            width: clamped_tile_width,
            height: clamped_tile_height,
            depth_or_array_layers: 1,
        };

        let reusable_texture_desc = wgpu::TextureDescriptor {
            label: None,
            size: clamped_tile_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        };

        let ping_pong_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Ping Pong Texture"),
            ..reusable_texture_desc
        });
        let ping_pong_view = ping_pong_texture.create_view(&Default::default());

        let tile_output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Tile Output Texture"),
            size: clamped_tile_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tile_output_texture_view = tile_output_texture.create_view(&Default::default());

        let processor = Self {
            context,
            blur_bgl,
            h_blur_pipeline,
            v_blur_pipeline,
            blur_params_buffer,
            flare_bgl_0,
            flare_bgl_1,
            flare_threshold_pipeline,
            flare_ghosts_pipeline,
            flare_params_buffer,
            flare_threshold_view,
            flare_ghosts_view,
            flare_final_view,
            flare_sampler,
            relight_depth_cache: Mutex::new(None),
            relight_pattern_cache: Mutex::new(None),
            main_bgl,
            main_pipeline,
            edge_pipelines: Default::default(),
            profile_pipelines: Default::default(),
            detail_bgl,
            detail_layout,
            detail_pipeline: Arc::new(std::sync::OnceLock::new()),
            detail_compilation: std::sync::OnceLock::new(),
            preview_pipelines,
            preview_compilation: std::array::from_fn(|_| std::sync::OnceLock::new()),
            preview_shader: shader_module,
            preview_layout: pipeline_layout,
            export_pipelines: std::array::from_fn(|_| std::sync::OnceLock::new()),
            high_precision_export_pipelines: std::array::from_fn(|_| std::sync::OnceLock::new()),
            high_precision_shader,
            high_precision_layout: high_precision_pipeline_layout,
            high_precision_bgl,
            high_precision_pipeline,
            high_precision_tile: std::sync::OnceLock::new(),
            tile_output_size: clamped_tile_size,
            adjustments_buffer,
            render_regions_buffer,
            prepared_layers_buffer,
            curve_luts_buffer,
            curve_lut_cache: Mutex::new(vec![None; MAX_MASKS + 1]),
            camera_profile_loaded: Mutex::new(None),
            dummy_blur_view,
            empty_mask_view,
            mask_cache: Mutex::new(None),
            blur_cache: Mutex::new(BlurCache::default()),
            chroma_pipelines,
            #[cfg(test)]
            force_mask_upload: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            mask_upload_layers: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            mask_upload_bytes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            mask_cache_budget: std::sync::atomic::AtomicUsize::new(MASK_CACHE_BYTES),
            #[cfg(test)]
            force_inline_detail: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_fallback_variant: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_full_preparation: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_exact_curves: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_general_pipeline: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_legacy_curves: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_plain_noise_reduction: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            force_unprepared_layers: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            unprepared_layer_pipelines: std::array::from_fn(|_| std::sync::OnceLock::new()),
            #[cfg(test)]
            legacy_curve_pipelines: std::array::from_fn(|_| std::sync::OnceLock::new()),
            dummy_lut_view,
            dummy_lut_sampler,
            ping_pong_view,
            tile_output_texture,
            tile_output_texture_view,
            display_targets: Mutex::new(None),
            analytics_readback: std::sync::OnceLock::new(),
            display_frame: Mutex::new(None),
            display_render_state: Mutex::new(None),
            #[cfg(test)]
            force_full_display_render: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            last_display_render_pixels: std::sync::atomic::AtomicU64::new(0),
        };
        // RAWs open with the default detail settings, and the all-enabled
        // variant renders any recipe until its own variant is ready.
        processor.compile_preview_variant(RAW_DEFAULT_PREVIEW_FEATURES);
        processor.compile_preview_variant(ALL_PREVIEW_FEATURES);
        Ok(processor)
    }

    fn relight_depth_view(
        &self,
        key: [u32; 4],
        retained: Option<&rawtonic_engine::relight::DepthSurface>,
    ) -> Result<RelightDepthViews, String> {
        let mut cache = self
            .relight_depth_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((cached, view)) = cache.as_ref()
            && *cached == key
        {
            return Ok(view.clone());
        }
        if key == [0; 4] {
            return Ok(RelightDepthViews {
                legacy: self.dummy_blur_view.clone(),
                camera: self.dummy_blur_view.clone(),
                scene: self.dummy_blur_view.clone(),
            });
        }
        let depth = retained.ok_or("Relight render has no retained depth surface")?;
        // Cache derivatives once per analysis. Camera-space projection amplifies
        // half-float inverse-depth errors, so keep its entire surface in f32.
        // A separate unfilterable binding needs no optional GPU format feature
        // and preserves legacy texture filtering exactly.
        let (surface, scene) = rawtonic_engine::relight::surface_textures(depth);
        let camera = depth.camera.is_some();
        let values: Vec<f16> = if camera {
            Vec::new()
        } else {
            surface.as_raw().iter().map(|v| f16::from_f32(*v)).collect()
        };
        let texture = self.context.device.create_texture_with_data(
            &self.context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Relight depth"),
                size: wgpu::Extent3d {
                    width: surface.width(),
                    height: surface.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: if camera {
                    wgpu::TextureFormat::Rgba32Float
                } else {
                    wgpu::TextureFormat::Rgba16Float
                },
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            if camera {
                bytemuck::cast_slice(surface.as_raw())
            } else {
                bytemuck::cast_slice(&values)
            },
        );
        let view = texture.create_view(&Default::default());
        let views = if camera {
            let scene = scene.map_or_else(
                || view.clone(),
                |scene| {
                    self.context
                        .device
                        .create_texture_with_data(
                            &self.context.queue,
                            &wgpu::TextureDescriptor {
                                label: Some("Relight scene surface"),
                                size: wgpu::Extent3d {
                                    width: scene.width(),
                                    height: scene.height(),
                                    depth_or_array_layers: 1,
                                },
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba32Float,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            },
                            wgpu::util::TextureDataOrder::MipMajor,
                            bytemuck::cast_slice(scene.as_raw()),
                        )
                        .create_view(&Default::default())
                },
            );
            RelightDepthViews {
                legacy: self.dummy_blur_view.clone(),
                camera: view,
                scene,
            }
        } else {
            RelightDepthViews {
                legacy: view,
                camera: self.dummy_blur_view.clone(),
                scene: self.dummy_blur_view.clone(),
            }
        };
        *cache = Some((key, views.clone()));
        Ok(views)
    }

    fn relight_pattern_view(
        &self,
        key: [u32; 2],
        retained: Option<&image::GrayImage>,
    ) -> Result<wgpu::TextureView, String> {
        let mut cache = self
            .relight_pattern_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((cached, view)) = cache.as_ref()
            && *cached == key
        {
            return Ok(view.clone());
        }
        if key == [0; 2] {
            return Ok(self.dummy_blur_view.clone());
        }
        let atlas = retained.ok_or("Relight render has no retained pattern atlas")?;
        let texture = self.context.device.create_texture_with_data(
            &self.context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Relight patterns"),
                size: wgpu::Extent3d {
                    width: atlas.width(),
                    height: atlas.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            atlas.as_raw(),
        );
        let view = texture.create_view(&Default::default());
        *cache = Some((key, view.clone()));
        Ok(view)
    }

    fn create_mask_texture(&self, width: u32, height: u32, layers: u32) -> wgpu::Texture {
        self.context
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Full Mask Texture Array"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // GrayImage uploads below contain one unorm byte per texel.
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
    }

    fn upload_mask_layer(&self, texture: &wgpu::Texture, layer: usize, mask: &GrayImage) {
        self.upload_mask_rows(texture, layer, mask, 0, mask.height());
    }

    fn upload_mask_rows(
        &self,
        texture: &wgpu::Texture,
        layer: usize,
        mask: &GrayImage,
        first_row: u32,
        rows: u32,
    ) {
        let width = mask.width();
        let start = first_row as usize * width as usize;
        self.context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: first_row,
                    z: layer as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &mask.as_raw()[start..start + rows as usize * width as usize],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        #[cfg(test)]
        {
            self.mask_upload_layers
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.mask_upload_bytes.fetch_add(
                width as usize * rows as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    #[cfg(test)]
    fn mask_cache_retained_bytes(&self) -> usize {
        self.mask_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |cache| cache.bytes)
    }

    fn prepare_mask_texture(
        &self,
        width: u32,
        height: u32,
        masks: &[Arc<GrayImage>],
        output_to_display: bool,
        force_full_preparation: bool,
    ) -> Result<(wgpu::TextureView, MaskTextureUpdate), String> {
        let masks = &masks[..masks.len().min(MAX_MASKS)];
        for (index, mask) in masks.iter().enumerate() {
            if mask.dimensions() != (width, height) {
                return Err(format!(
                    "Mask {index} dimensions ({}x{}) do not match render dimensions ({width}x{height}).",
                    mask.width(),
                    mask.height()
                ));
            }
        }

        #[cfg(test)]
        let force_mask_upload = self
            .force_mask_upload
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let force_mask_upload = false;

        if masks.is_empty() && !force_full_preparation {
            if output_to_display && !force_mask_upload {
                *self.mask_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            // The shared empty view never changes.
            return Ok((
                self.empty_mask_view.clone(),
                MaskTextureUpdate::Reused(Vec::new()),
            ));
        }

        let layer_count = masks.len().max(2);
        let layer_bytes = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "Mask texture dimensions exceed addressable memory.".to_string())?;
        let texture_bytes = layer_bytes
            .checked_mul(layer_count)
            .ok_or_else(|| "Mask texture array exceeds addressable memory.".to_string())?;
        let view_descriptor = wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        };

        // Preserve the old allocation/copy/upload sequence as an independent
        // pixel oracle and a benchmark baseline with identical shader work.
        #[cfg(test)]
        if force_full_preparation || force_mask_upload {
            let mut data = Vec::with_capacity(texture_bytes);
            for mask in masks {
                data.extend_from_slice(mask.as_raw());
            }
            data.resize(texture_bytes, 0);
            let texture = self.context.device.create_texture_with_data(
                &self.context.queue,
                &wgpu::TextureDescriptor {
                    label: Some("Uncached Full Mask Texture Array"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: layer_count as u32,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                TextureDataOrder::MipMajor,
                &data,
            );
            self.mask_upload_layers
                .fetch_add(layer_count, std::sync::atomic::Ordering::Relaxed);
            self.mask_upload_bytes
                .fetch_add(texture_bytes, std::sync::atomic::Ordering::Relaxed);
            return Ok((
                texture.create_view(&view_descriptor),
                MaskTextureUpdate::Transient,
            ));
        }

        #[cfg(test)]
        let budget = self
            .mask_cache_budget
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let budget = MASK_CACHE_BYTES;
        let retained_bytes = masks.iter().try_fold(texture_bytes, |bytes, mask| {
            bytes.checked_add(mask.as_raw().len())
        });

        // Readback jobs use transient textures and cannot evict the foreground
        // entry. Oversized display jobs release it before allocating their array.
        if !output_to_display || retained_bytes.is_none_or(|bytes| bytes > budget) {
            if output_to_display {
                *self.mask_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            let texture = self.create_mask_texture(width, height, layer_count as u32);
            for (index, mask) in masks.iter().enumerate() {
                self.upload_mask_layer(&texture, index, mask);
            }
            return Ok((
                texture.create_view(&view_descriptor),
                MaskTextureUpdate::Transient,
            ));
        }

        let retained_bytes = retained_bytes.unwrap();
        let mut cache = self.mask_cache.lock().unwrap_or_else(|e| e.into_inner());
        let matches_geometry = cache.as_ref().is_some_and(|cache| {
            cache.width == width && cache.height == height && cache.masks.len() == masks.len()
        });
        if !matches_geometry {
            // Actual mask count is part of the identity, including 2 -> 1 when
            // allocation depth stays at two: the unused layer must become zero.
            *cache = None;
            let texture = self.create_mask_texture(width, height, layer_count as u32);
            for (index, mask) in masks.iter().enumerate() {
                self.upload_mask_layer(&texture, index, mask);
            }
            let view = texture.create_view(&view_descriptor);
            *cache = Some(MaskTextureCache {
                texture,
                view,
                width,
                height,
                masks: masks.to_vec(),
                bytes: retained_bytes,
            });
            let cache = cache.as_ref().unwrap();
            debug_assert!(cache.bytes <= budget);
            return Ok((cache.view.clone(), MaskTextureUpdate::Rebuilt));
        }
        let cache = cache.as_mut().unwrap();
        let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
        for (index, mask) in masks.iter().enumerate() {
            if Arc::ptr_eq(&cache.masks[index], mask) {
                continue;
            }
            // A brush stroke usually changes a small part of one layer: upload
            // only its rows and report the pixels a display frame must redo.
            if let Some((top, bottom)) =
                add_changed_mask_bands(&cache.masks[index], mask, &mut bands)
            {
                self.upload_mask_rows(&cache.texture, index, mask, top, bottom - top);
            }
            cache.masks[index] = Arc::clone(mask);
        }
        cache.bytes = retained_bytes;
        debug_assert!(cache.bytes <= budget);
        Ok((
            cache.view.clone(),
            MaskTextureUpdate::Reused(merge_dirty_bands(&bands)),
        ))
    }

    fn display_targets(&self, width: u32, height: u32) -> Arc<DisplayTargets> {
        let mut cached = self
            .display_targets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(targets) = cached.as_ref()
            && targets.width == width
            && targets.height == height
        {
            return Arc::clone(targets);
        }
        let device = &self.context.device;
        let descriptor = wgpu::TextureDescriptor {
            label: Some("Current display working output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        let working_texture = device.create_texture(&descriptor);
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Current display output"),
            ..descriptor
        });
        let output_texture_view = output_texture.create_view(&Default::default());
        let targets = Arc::new(DisplayTargets {
            width,
            height,
            working_texture,
            output_texture,
            output_texture_view,
        });
        // An in-flight frame/bind group may still retain the prior output until
        // presentation completes; the processor itself drops its old pair here.
        *cached = Some(Arc::clone(&targets));
        targets
    }

    #[cfg(test)]
    fn test_working_texture(&self) -> wgpu::Texture {
        self.display_targets
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .working_texture
            .clone()
    }

    pub fn sample_display_color(
        &self,
        path: &str,
        source: &Arc<DynamicImage>,
        load: usize,
        render: usize,
        x: f64,
        y: f64,
    ) -> Result<Option<[f64; 3]>, String> {
        let frame = self
            .display_frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(frame) = frame
            .as_ref()
            .filter(|frame| frame.identity.matches(path, source, load, render))
        else {
            return Ok(None);
        };
        let targets = self
            .display_targets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(targets) = targets.as_ref() else {
            return Ok(None);
        };
        let Some(sample) = preview_sample_roi(targets.width, targets.height, x, y) else {
            return Ok(None);
        };
        if sample.x < frame.roi.x
            || sample.y < frame.roi.y
            || sample.x + sample.width > frame.roi.x + frame.roi.width
            || sample.y + sample.height > frame.roi.y + frame.roi.height
        {
            return Ok(None);
        }
        let pixels = read_texture_data_roi(
            &self.context.device,
            &self.context.queue,
            &targets.output_texture,
            wgpu::Origin3d {
                x: sample.x,
                y: sample.y,
                z: 0,
            },
            wgpu::Extent3d {
                width: sample.width,
                height: sample.height,
                depth_or_array_layers: 1,
            },
            4,
        )?;
        Ok(mean_rgb8(&pixels))
    }

    fn high_precision_tile(&self) -> &HighPrecisionTile {
        self.high_precision_tile.get_or_init(|| {
            let texture = self
                .context
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("High Precision Tile Output Texture"),
                    size: self.tile_output_size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
            let view = texture.create_view(&Default::default());
            HighPrecisionTile { texture, view }
        })
    }

    /// Uploads a camera profile's colour tables (display and export), once
    /// per profile and photo.
    fn update_camera_profile_tables(&self, global: &crate::image_processing::GlobalAdjustments) {
        let key = global.camera_profile[1];
        if key == 0 {
            return;
        }
        let mut loaded = self
            .camera_profile_loaded
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *loaded == Some(key) {
            return;
        }
        if let Some(table) = crate::camera_profile::tables(key)
            && !table.is_empty()
            && table.len() <= crate::camera_profile::TABLE_CAPACITY
        {
            self.context.queue.write_buffer(
                &self.curve_luts_buffer,
                (CURVE_LUT_SAMPLES * (MAX_MASKS + 1) * 16) as u64,
                bytemuck::cast_slice(&table),
            );
            *loaded = Some(key);
        }
    }

    /// Display only. Tables are rebuilt for layers whose curves changed; a layer
    /// whose table fails the tolerance check keeps the exact evaluator.
    fn update_curve_luts(&self, layers: &mut [PreparedLayer], mask_count: usize) {
        #[cfg(test)]
        let disabled = self
            .force_exact_curves
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let disabled = false;
        let mut cache = self
            .curve_lut_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (index, layer) in layers.iter_mut().enumerate().skip(1).take(mask_count) {
            if disabled || !layer.has_curves() {
                layer.set_curve_lut(false);
                continue;
            }
            let enabled = match &cache[index] {
                Some((key, enabled)) if key.as_slice() == layer.curve_key() => *enabled,
                _ => {
                    let table = curve_lut(layer);
                    if let Some(table) = &table {
                        self.context.queue.write_buffer(
                            &self.curve_luts_buffer,
                            (index * CURVE_LUT_SAMPLES * 16) as u64,
                            bytemuck::cast_slice(table),
                        );
                    }
                    cache[index] = Some((layer.curve_key().to_vec(), table.is_some()));
                    table.is_some()
                }
            };
            layer.set_curve_lut(enabled);
        }
    }

    fn preview_pipeline(&self, features: usize) -> &wgpu::ComputePipeline {
        #[cfg(test)]
        if self
            .force_legacy_curves
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return self.legacy_curve_pipelines[features]
                .get()
                .expect("prepare the matching legacy curve variant before comparing");
        }
        #[cfg(test)]
        if self
            .force_unprepared_layers
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return self.unprepared_layer_pipelines[features]
                .get()
                .expect("prepare the previous direct-curve variant before comparing");
        }
        #[cfg(test)]
        if self
            .force_fallback_variant
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return self.preview_pipelines[ALL_PREVIEW_FEATURES]
                .get()
                .expect("compile the all-features variant before comparing");
        }
        // Even the all-enabled variant uses direct curve reads for display.
        // Until this variant is compiled, the all-enabled one (a superset,
        // compiled from processor creation) renders the frame; the general
        // shader only before that, as it is far slower with many layers.
        if self.preview_pipelines[features].get().is_none() {
            self.compile_preview_variant(features);
        }
        self.preview_pipelines[features]
            .get()
            .or_else(|| self.preview_pipelines[ALL_PREVIEW_FEATURES].get())
            .unwrap_or(&self.main_pipeline)
    }

    /// Compile a display variant off the render worker (once).
    fn compile_preview_variant(&self, features: usize) {
        self.preview_compilation[features].get_or_init(|| {
            let pipelines = Arc::clone(&self.preview_pipelines);
            let device = Arc::clone(&self.context.device);
            let shader = self.preview_shader.clone();
            let layout = self.preview_layout.clone();
            std::thread::spawn(move || {
                // The general pipeline was validated at construction and
                // stays in use if this variant cannot be created.
                match create_preview_pipeline(&device, &shader, &layout, features, true, true) {
                    Ok(pipeline) => {
                        let _ = pipelines[features].set(pipeline);
                    }
                    Err(error) => log::error!(
                        "Specialized preview variant {features} unavailable; using the general shader: {error}"
                    ),
                }
            });
        });
    }

    /// The pass that fills the detail cache, or `None` while it compiles off
    /// the render worker (frames compute the detail stage inline until then)
    /// or if it failed validation.
    fn detail_pipeline(&self) -> Option<&wgpu::ComputePipeline> {
        if self.detail_pipeline.get().is_none() {
            self.detail_compilation.get_or_init(|| {
                let pipeline = Arc::clone(&self.detail_pipeline);
                let device = Arc::clone(&self.context.device);
                let shader = self.preview_shader.clone();
                let layout = self.detail_layout.clone();
                std::thread::spawn(move || {
                    let created = create_compute_pipeline(
                        &device,
                        &wgpu::ComputePipelineDescriptor {
                            label: Some("Detail cache"),
                            layout: Some(&layout),
                            module: &shader,
                            entry_point: Some("detail_main"),
                            compilation_options: Default::default(),
                            cache: None,
                        },
                    );
                    if let Err(error) = &created {
                        log::error!("Detail cache unavailable; computing it per frame: {error}");
                    }
                    let _ = pipeline.set(created.ok());
                });
            });
        }
        self.detail_pipeline.get().and_then(Option::as_ref)
    }

    /// CPU output preserves the general curve arithmetic, including identity
    /// interpolation rounding. Only absent effects with exact early returns are
    /// specialized away. Compile synchronously because exports need this result.
    fn export_pipeline(
        &self,
        adjustments: &AllAdjustments,
        high_precision: bool,
    ) -> &wgpu::ComputePipeline {
        let features = preview_features(adjustments) | (1 << 2);
        // Metal may reassociate active neighbourhood operations when unrelated
        // branches are removed. Their full general pipeline preserves exact
        // 8/16-bit exports; specialize only recipes without those operations.
        if features & ((1 << 0) | (1 << 1) | (1 << 4)) != 0 {
            return self.general_pipeline(adjustments, high_precision);
        }
        let (pipelines, shader, layout) = if high_precision {
            (
                &self.high_precision_export_pipelines,
                &self.high_precision_shader,
                &self.high_precision_layout,
            )
        } else {
            (
                &self.export_pipelines,
                &self.preview_shader,
                &self.preview_layout,
            )
        };
        let general = if high_precision {
            &self.high_precision_pipeline
        } else {
            &self.main_pipeline
        };
        pipelines[features]
            .get_or_init(|| {
                create_preview_pipeline(&self.context.device, shader, layout, features, true, false)
                    .inspect_err(|error| {
                        log::error!(
                            "Specialized export variant {features} unavailable; using the general shader: {error}"
                        )
                    })
                    .ok()
            })
            .as_ref()
            .unwrap_or(general)
    }

    /// The general pipeline for `adjustments`: with the luminance noise
    /// reduction's edge-preserving pass for noisy photos (see
    /// `LUMA_NR_EDGE_NOISE_START`), else the one compiled without it.
    fn general_pipeline(
        &self,
        adjustments: &AllAdjustments,
        high_precision: bool,
    ) -> &wgpu::ComputePipeline {
        let plain = if high_precision {
            &self.high_precision_pipeline
        } else {
            &self.main_pipeline
        };
        #[cfg(test)]
        if self
            .force_plain_noise_reduction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return plain;
        }
        let edge = edge_noise_reduction(adjustments);
        if adjustments.global.luma_noise_profile[0][0] > 0.0 {
            return self.profile_pipelines[usize::from(high_precision)][usize::from(edge)]
                .get_or_init(|| {
                    let (shader, layout) = if high_precision {
                        (&self.high_precision_shader, &self.high_precision_layout)
                    } else {
                        (&self.preview_shader, &self.preview_layout)
                    };
                    let edge_nr = if edge { 1.0 } else { 0.0 };
                    let constants: &[(&str, f64)] = if high_precision {
                        &[
                            ("HIGH_PRECISION_OUTPUT", 1.0),
                            ("EDGE_NR", edge_nr),
                            ("LUMA_PROFILE", 1.0),
                        ]
                    } else {
                        &[("EDGE_NR", edge_nr), ("LUMA_PROFILE", 1.0)]
                    };
                    create_compute_pipeline(
                        &self.context.device,
                        &wgpu::ComputePipelineDescriptor {
                            label: Some("Compute Pipeline (luminance noise profile)"),
                            layout: Some(layout),
                            module: shader,
                            entry_point: Some("main"),
                            compilation_options: wgpu::PipelineCompilationOptions {
                                constants,
                                ..Default::default()
                            },
                            cache: None,
                        },
                    )
                    .inspect_err(|error| {
                        log::error!("Luminance noise profile pipeline unavailable: {error}")
                    })
                    .ok()
                })
                .as_ref()
                .unwrap_or(plain);
        }
        if !edge {
            return plain;
        }
        self.edge_pipelines[usize::from(high_precision)]
            .get_or_init(|| {
                let (shader, layout) = if high_precision {
                    (&self.high_precision_shader, &self.high_precision_layout)
                } else {
                    (&self.preview_shader, &self.preview_layout)
                };
                let constants: &[(&str, f64)] = if high_precision {
                    &[
                        ("HIGH_PRECISION_OUTPUT", 1.0),
                        ("EDGE_NR", 1.0),
                        ("LUMA_PROFILE", 0.0),
                    ]
                } else {
                    &[("EDGE_NR", 1.0), ("LUMA_PROFILE", 0.0)]
                };
                create_compute_pipeline(
                    &self.context.device,
                    &wgpu::ComputePipelineDescriptor {
                        label: Some("Compute Pipeline (edge-preserving noise reduction)"),
                        layout: Some(layout),
                        module: shader,
                        entry_point: Some("main"),
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants,
                            ..Default::default()
                        },
                        cache: None,
                    },
                )
                .inspect_err(|error| {
                    log::error!("Edge-preserving noise reduction unavailable: {error}")
                })
                .ok()
            })
            .as_ref()
            .unwrap_or(plain)
    }

    /// Test shorthand for the exact export shader on readback jobs.
    #[cfg(test)]
    fn run(
        &self,
        input_texture_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        request: RenderRequest,
        output_to_display: bool,
        output_precision: RenderOutputPrecision,
    ) -> Result<(RenderedPixels, u32, u32, u32, u32), String> {
        self.run_with_shader(
            input_texture_view,
            width,
            height,
            request,
            output_to_display,
            output_precision,
            ReadbackShader::Export,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_with_shader(
        &self,
        input_texture_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        request: RenderRequest,
        output_to_display: bool,
        output_precision: RenderOutputPrecision,
        readback_shader: ReadbackShader,
        chroma: Option<(&wgpu::TextureView, &wgpu::TextureView)>,
    ) -> Result<(RenderedPixels, u32, u32, u32, u32), String> {
        self.run_in_window(
            input_texture_view,
            width,
            height,
            Roi {
                x: 0,
                y: 0,
                width,
                height,
            },
            request,
            output_to_display,
            output_precision,
            readback_shader,
            chroma,
            None,
        )
    }

    /// Renders a `width` x `height` photo whose input texture (and mask
    /// bitmaps) hold only `window` of it. The output is `request.roi`, which
    /// must lie in the window far enough from its edges that every read stays
    /// inside it (see [`plan_render_windows`]); the pixels then equal those of
    /// a render from the whole photo. Light flare needs the photo's
    /// `flare_samples` ([`create_flare_samples_texture`]) then.
    #[allow(clippy::too_many_arguments)]
    fn run_in_window(
        &self,
        input_texture_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        window: Roi,
        request: RenderRequest,
        output_to_display: bool,
        output_precision: RenderOutputPrecision,
        readback_shader: ReadbackShader,
        chroma: Option<(&wgpu::TextureView, &wgpu::TextureView)>,
        flare_samples: Option<&wgpu::TextureView>,
    ) -> Result<(RenderedPixels, u32, u32, u32, u32), String> {
        let mut request = request;
        if chroma.is_none() {
            // No chroma pyramid: the cross-bilateral colour noise reduction.
            request.adjustments.uniforms.global.color_noise[0] = 0.0;
        }
        let whole_photo = window
            == Roi {
                x: 0,
                y: 0,
                width,
                height,
            };
        if !whole_photo && (output_to_display || request.roi.is_none()) {
            return Err("A render from a photo window needs a CPU readback region.".to_string());
        }
        // The flare map reads the whole photo: windows bring its samples.
        let flare_source = if whole_photo {
            Some(input_texture_view)
        } else {
            flare_samples
        };
        if request.adjustments.global.flare_amount > 0.0 && flare_source.is_none() {
            return Err(FLARE_WINDOW_ERROR.to_string());
        }
        let source_window = [window.x, window.y, width, height];
        let display_shader = output_to_display
            || (matches!(
                readback_shader,
                ReadbackShader::Display | ReadbackShader::Preview
            ) && output_precision == RenderOutputPrecision::EightBit);
        let retain_intermediates = output_to_display
            || (readback_shader == ReadbackShader::Preview
                && output_precision == RenderOutputPrecision::EightBit);
        let device = &self.context.device;
        let queue = &self.context.queue;
        let display_targets = output_to_display.then(|| self.display_targets(width, height));
        let scale = (width.min(height) as f32) / 1080.0;
        const MAX_MASK_BINDINGS: u32 = 1;
        // Thumbnail/export work must not evict the source-dependent preview
        // intermediates immediately after the editor saves a released slider.
        let background_blur_cache = Mutex::new(BlurCache::default());
        let blur_cache = if retain_intermediates {
            &self.blur_cache
        } else {
            &background_blur_cache
        };

        #[cfg(test)]
        let force_full_preparation = self
            .force_full_preparation
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let force_full_preparation = false;
        #[cfg(test)]
        let force_general_pipeline = self
            .force_general_pipeline
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let force_general_pipeline = false;
        let main_pipeline = if display_shader && !force_full_preparation && !force_general_pipeline
        {
            self.preview_pipeline(preview_features(&request.adjustments))
        } else if !output_to_display
            && output_precision == RenderOutputPrecision::EightBit
            && !force_full_preparation
            && !force_general_pipeline
        {
            self.export_pipeline(&request.adjustments, false)
        } else {
            self.general_pipeline(&request.adjustments, false)
        };

        if output_precision == RenderOutputPrecision::SixteenBit && output_to_display {
            return Err(
                "High-precision GPU output is only supported for CPU-readback renders.".to_string(),
            );
        }

        let high_precision_tile = if output_precision == RenderOutputPrecision::SixteenBit {
            Some(self.high_precision_tile())
        } else {
            None
        };
        let (output_pipeline, output_bgl, output_tile_texture, output_tile_view, bytes_per_pixel) =
            if let Some(high_precision_tile) = high_precision_tile {
                (
                    if force_full_preparation || force_general_pipeline {
                        self.general_pipeline(&request.adjustments, true)
                    } else {
                        self.export_pipeline(&request.adjustments, true)
                    },
                    &self.high_precision_bgl,
                    &high_precision_tile.texture,
                    &high_precision_tile.view,
                    16,
                )
            } else {
                (
                    main_pipeline,
                    &self.main_bgl,
                    &self.tile_output_texture,
                    &self.tile_output_texture_view,
                    4,
                )
            };

        let bounds = request.roi.unwrap_or(Roi {
            x: 0,
            y: 0,
            width,
            height,
        });
        let out_width = bounds.width;
        let out_height = bounds.height;
        // Taken before any work so that a failed frame never leaves a state
        // describing pixels the working texture no longer holds.
        let previous_display_state = if output_to_display {
            self.display_render_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        } else {
            None
        };
        // Depth key in xy, imported-pattern atlas key in zw (relight.rs).
        let [depth_low, depth_high, pattern_low, pattern_high] =
            request.adjustments.global.relight_depth;
        let relight_depth_view = self.relight_depth_view(
            [depth_low, depth_high, 0, 0],
            request.adjustments.relight_surface.as_deref(),
        )?;
        let relight_pattern_view = self.relight_pattern_view(
            [pattern_low, pattern_high],
            request.adjustments.relight_patterns.as_deref(),
        )?;
        let (mask_texture_view, mask_update) = self.prepare_mask_texture(
            window.width,
            window.height,
            request.mask_bitmaps,
            retain_intermediates,
            force_full_preparation,
        )?;
        // Test oracles switch shaders, curves or uploads without changing the
        // request, so they always render and never seed a later partial frame.
        #[cfg(test)]
        let test_override = force_full_preparation
            || force_general_pipeline
            || [
                &self.force_mask_upload,
                &self.force_exact_curves,
                &self.force_legacy_curves,
                &self.force_unprepared_layers,
                &self.force_full_display_render,
                &self.force_plain_noise_reduction,
            ]
            .iter()
            .any(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
        #[cfg(not(test))]
        let test_override = false;
        let display_state =
            (output_to_display && !test_override && mask_update != MaskTextureUpdate::Transient)
                .then(|| DisplayRenderState {
                    input: input_texture_view.clone(),
                    targets: Arc::downgrade(display_targets.as_ref().unwrap()),
                    dimensions: (width, height),
                    bounds,
                    adjustments: request.adjustments.uniforms,
                    lut: request.lut.as_ref().map(Arc::downgrade),
                    pipeline: main_pipeline.clone(),
                    mask_layers: request.mask_bitmaps.len().min(MAX_MASKS),
                });
        // Restrict this frame to the changed mask pixels when everything else
        // matches the previous complete frame. `Some` without regions skips work.
        let dirty: Option<Vec<Roi>> = match (&previous_display_state, &display_state, &mask_update)
        {
            (Some(previous), Some(current), MaskTextureUpdate::Reused(changed))
                if previous.matches(current) =>
            {
                Some(
                    changed
                        .iter()
                        .filter_map(|changed| intersect_roi(*changed, bounds))
                        .collect(),
                )
            }
            _ => None,
        };
        #[cfg(any(test, feature = "performance-profiling"))]
        let rendered_pixels =
            dirty
                .as_ref()
                .map_or(bounds.width as u64 * bounds.height as u64, |regions| {
                    regions
                        .iter()
                        .map(|region| region.width as u64 * region.height as u64)
                        .sum()
                });
        #[cfg(test)]
        self.last_display_render_pixels
            .store(rendered_pixels, std::sync::atomic::Ordering::Relaxed);
        #[cfg(feature = "performance-profiling")]
        if output_to_display {
            log::info!(
                "[PERF] {}",
                serde_json::json!({
                    "stage": "display_region", "partial": dirty.is_some(),
                    "pixels": rendered_pixels, "of": bounds.width as u64 * bounds.height as u64
                })
            );
        }

        let (lut_texture_view, lut_sampler) = if let Some(lut_arc) = &request.lut {
            let lut_data = &lut_arc.data;
            let size = lut_arc.size;
            let mut rgba_lut_data_f16 = Vec::with_capacity(lut_data.len() / 3 * 4);
            for chunk in lut_data.as_chunks::<3>().0 {
                rgba_lut_data_f16.push(f16::from_f32(chunk[0]));
                rgba_lut_data_f16.push(f16::from_f32(chunk[1]));
                rgba_lut_data_f16.push(f16::from_f32(chunk[2]));
                rgba_lut_data_f16.push(f16::ONE);
            }
            let lut_texture = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("LUT 3D Texture"),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: size,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                TextureDataOrder::MipMajor,
                bytemuck::cast_slice(&rgba_lut_data_f16),
            );
            let view = lut_texture.create_view(&Default::default());
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });
            (view, sampler)
        } else {
            (self.dummy_lut_view.clone(), self.dummy_lut_sampler.clone())
        };

        let adjustments = request.adjustments.uniforms;
        self.update_camera_profile_tables(&adjustments.global);
        if display_shader {
            let mut layers = prepare_layers(&adjustments);
            self.update_curve_luts(&mut layers, adjustments.mask_count as usize);
            queue.write_buffer(
                &self.prepared_layers_buffer,
                0,
                bytemuck::cast_slice(&layers),
            );
        }
        let blur_requirements = if force_full_preparation {
            [true; 5]
        } else {
            required_blurs(&adjustments)
        };
        if let Some(flare_source) = flare_source.filter(|_| adjustments.global.flare_amount > 0.0) {
            let mut encoder = device.create_command_encoder(&Default::default());

            let aspect_ratio = if height > 0 {
                width as f32 / height as f32
            } else {
                1.0
            };
            let f_params = FlareParams {
                amount: adjustments.global.flare_amount,
                is_raw: adjustments.global.is_raw_image,
                exposure: adjustments.global.exposure,
                brightness: adjustments.global.brightness,
                contrast: adjustments.global.contrast,
                whites: adjustments.global.whites,
                aspect_ratio,
                gathered: u32::from(!whole_photo),
                source_width: width,
                source_height: height,
                _pad: [0; 2],
            };
            queue.write_buffer(&self.flare_params_buffer, 0, bytemuck::bytes_of(&f_params));

            let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG0"),
                layout: &self.flare_bgl_0,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(flare_source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_threshold_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.flare_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                    },
                ],
            });

            {
                let mut cpass = encoder.begin_compute_pass(&Default::default());
                cpass.set_pipeline(&self.flare_threshold_pipeline);
                cpass.set_bind_group(0, &bg0, &[]);
                cpass.dispatch_workgroups(FLARE_MAP_SIZE / 16, FLARE_MAP_SIZE / 16, 1);
            }

            let bg0_ghosts = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG0 Ghosts"),
                layout: &self.flare_bgl_0,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(flare_source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_final_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.flare_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                    },
                ],
            });

            let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG1"),
                layout: &self.flare_bgl_1,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.flare_threshold_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_ghosts_view),
                    },
                ],
            });

            {
                let mut cpass = encoder.begin_compute_pass(&Default::default());
                cpass.set_pipeline(&self.flare_ghosts_pipeline);
                cpass.set_bind_group(0, &bg0_ghosts, &[]);
                cpass.set_bind_group(1, &bg1, &[]);
                cpass.dispatch_workgroups(FLARE_MAP_SIZE / 16, FLARE_MAP_SIZE / 16, 1);
            }

            queue.submit(Some(encoder.finish()));
        }

        // Display frames cache the detail stage with the blurs when every tile
        // of the frame fits the cache budget together.
        let detail_fits = display_cache_bytes(
            width,
            height,
            TILE_SIZE,
            TILE_OVERLAP,
            blur_requirements
                .iter()
                .filter(|&&required| required)
                .count(),
        ) <= BLUR_CACHE_BYTES;

        let output_len = if output_to_display {
            0
        } else {
            (out_width * out_height * 4) as usize
        };
        let mut final_pixels = match output_precision {
            RenderOutputPrecision::EightBit => RenderedPixels::U8(vec![0u8; output_len]),
            RenderOutputPrecision::SixteenBit => RenderedPixels::U16(vec![0u16; output_len]),
        };

        let start_tile_x = bounds.x / TILE_SIZE;
        let start_tile_y = bounds.y / TILE_SIZE;
        let end_tile_x = (bounds.x + bounds.width).div_ceil(TILE_SIZE);
        let end_tile_y = (bounds.y + bounds.height).div_ceil(TILE_SIZE);

        let mut pending_readback: Option<(PendingReadback, TilePlacement)> = None;
        for tile_y in start_tile_y..end_tile_y {
            for tile_x in start_tile_x..end_tile_x {
                let x_start_unclamped = tile_x * TILE_SIZE;
                let y_start_unclamped = tile_y * TILE_SIZE;

                let x_start = x_start_unclamped.max(bounds.x);
                let y_start = y_start_unclamped.max(bounds.y);
                let x_end = (x_start_unclamped + TILE_SIZE)
                    .min(bounds.x + bounds.width)
                    .min(width);
                let y_end = (y_start_unclamped + TILE_SIZE)
                    .min(bounds.y + bounds.height)
                    .min(height);

                let tile_width = x_end - x_start;
                let tile_height = y_end - y_start;
                // Partial frames keep the full frame's tile and blur geometry, so
                // every re-rendered pixel matches the complete render exactly.
                let tile_rect = Roi {
                    x: x_start,
                    y: y_start,
                    width: tile_width,
                    height: tile_height,
                };
                let retained_regions: Vec<Roi> = match &dirty {
                    None => vec![tile_rect],
                    Some(regions) => regions
                        .iter()
                        .filter_map(|region| intersect_roi(*region, tile_rect))
                        .collect(),
                };
                if retained_regions.is_empty() {
                    continue;
                }

                let input_x_start = (x_start as i32 - TILE_OVERLAP as i32).max(0) as u32;
                let input_y_start = (y_start as i32 - TILE_OVERLAP as i32).max(0) as u32;
                let input_x_end = (x_end + TILE_OVERLAP).min(width);
                let input_y_end = (y_end + TILE_OVERLAP).min(height);
                let input_width = input_x_end - input_x_start;
                let input_height = input_y_end - input_y_start;

                let input_texture_size = wgpu::Extent3d {
                    width: input_width,
                    height: input_height,
                    depth_or_array_layers: 1,
                };

                let geometry = [
                    width,
                    height,
                    input_x_start,
                    input_y_start,
                    input_width,
                    input_height,
                ];
                let mut cache = blur_cache.lock().unwrap_or_else(|e| e.into_inner());
                if cache.input.as_ref() != Some(input_texture_view) {
                    *cache = BlurCache {
                        input: Some(input_texture_view.clone()),
                        ..Default::default()
                    };
                }
                let mut tile =
                    if let Some(index) = cache.tiles.iter().position(|t| t.geometry == geometry) {
                        let tile = cache.tiles.remove(index).unwrap();
                        cache.bytes -= tile.bytes;
                        tile
                    } else {
                        BlurTile {
                            geometry,
                            views: Default::default(),
                            detail: None,
                            bytes: 0,
                        }
                    };
                let mut run_blur = |index: usize, base_radius: f32| -> Option<wgpu::TextureView> {
                    if !blur_requirements[index] {
                        return None;
                    }
                    if !force_full_preparation && let Some(view) = &tile.views[index] {
                        return Some(view.clone());
                    }
                    let output_view = tile.views[index].get_or_insert_with(|| {
                        tile.bytes += input_width as usize * input_height as usize * 8;
                        device
                            .create_texture(&wgpu::TextureDescriptor {
                                label: Some("Cached blur tile"),
                                size: input_texture_size,
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba16Float,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::STORAGE_BINDING,
                                view_formats: &[],
                            })
                            .create_view(&Default::default())
                    });
                    let radius = (base_radius * scale).ceil().max(1.0) as u32;

                    let params = BlurParams {
                        radius,
                        tile_offset_x: input_x_start,
                        tile_offset_y: input_y_start,
                        input_width,
                        input_height,
                        source_x: window.x,
                        source_y: window.y,
                        full_width: width,
                        full_height: height,
                        _pad1: 0,
                        _pad2: 0,
                        _pad3: 0,
                    };
                    queue.write_buffer(&self.blur_params_buffer, 0, bytemuck::bytes_of(&params));

                    let mut blur_encoder = device.create_command_encoder(&Default::default());

                    let h_blur_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("H-Blur BG"),
                        layout: &self.blur_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(input_texture_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&self.ping_pong_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blur_params_buffer.as_entire_binding(),
                            },
                        ],
                    });

                    {
                        let mut cpass = blur_encoder.begin_compute_pass(&Default::default());
                        cpass.set_pipeline(&self.h_blur_pipeline);
                        cpass.set_bind_group(0, &h_blur_bg, &[]);
                        cpass.dispatch_workgroups(
                            input_width.div_ceil(16),
                            input_height.div_ceil(16),
                            1,
                        );
                    }

                    let v_blur_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("V-Blur BG"),
                        layout: &self.blur_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&self.ping_pong_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(output_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blur_params_buffer.as_entire_binding(),
                            },
                        ],
                    });

                    {
                        let mut cpass = blur_encoder.begin_compute_pass(&Default::default());
                        cpass.set_pipeline(&self.v_blur_pipeline);
                        cpass.set_bind_group(0, &v_blur_bg, &[]);
                        cpass.dispatch_workgroups(
                            input_width.div_ceil(16),
                            input_height.div_ceil(16),
                            1,
                        );
                    }

                    queue.submit(Some(blur_encoder.finish()));
                    Some(output_view.clone())
                };

                let sharpness_blur = run_blur(0, 1.0);
                let tonal_blur = run_blur(1, 3.5);
                let clarity_blur = run_blur(2, 8.0);
                let structure_blur = run_blur(3, 40.0);
                let nr_blur = run_blur(4, 2.0);
                // Display frames cache the detail stage with the blurs if all
                // their tiles fit the cache budget (`detail_fits`).
                // 32-bit floats: 16-bit rounding flipped strongly saturated
                // channels near zero by up to 220 levels after Vibrance.
                let detail_bytes = input_width as usize * input_height as usize * 16;
                let detail_key = DetailKey::of(&adjustments.global);
                #[cfg(test)]
                let force_inline_detail = self
                    .force_inline_detail
                    .load(std::sync::atomic::Ordering::Relaxed);
                #[cfg(not(test))]
                let force_inline_detail = false;
                let detail_pipeline = (retain_intermediates
                    && !force_full_preparation
                    && !force_inline_detail
                    && detail_fits
                    && DetailKey::active(&adjustments.global)
                    && tile.bytes
                        + if tile.detail.is_some() {
                            0
                        } else {
                            detail_bytes
                        }
                        <= BLUR_CACHE_BYTES)
                    .then(|| self.detail_pipeline())
                    .flatten();
                let mut build_detail = false;
                let detail_view = match (detail_pipeline, tile.detail.take()) {
                    (None, previous) => {
                        tile.detail = previous;
                        None
                    }
                    (Some(_), Some((key, view))) if key == detail_key => {
                        tile.detail = Some((key, view.clone()));
                        Some(view)
                    }
                    (Some(_), previous) => {
                        let view = previous.map(|(_, view)| view).unwrap_or_else(|| {
                            tile.bytes += detail_bytes;
                            device
                                .create_texture(&wgpu::TextureDescriptor {
                                    label: Some("Cached detail tile"),
                                    size: input_texture_size,
                                    mip_level_count: 1,
                                    sample_count: 1,
                                    dimension: wgpu::TextureDimension::D2,
                                    format: wgpu::TextureFormat::Rgba32Float,
                                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                                        | wgpu::TextureUsages::STORAGE_BINDING,
                                    view_formats: &[],
                                })
                                .create_view(&Default::default())
                        });
                        tile.detail = Some((detail_key, view.clone()));
                        build_detail = true;
                        Some(view)
                    }
                };
                while cache.bytes + tile.bytes > BLUR_CACHE_BYTES {
                    let Some(oldest) = cache.tiles.pop_back() else {
                        break;
                    };
                    cache.bytes -= oldest.bytes;
                }
                if retain_intermediates && tile.bytes > 0 && tile.bytes <= BLUR_CACHE_BYTES {
                    cache.bytes += tile.bytes;
                    cache.tiles.push_front(tile);
                }
                drop(cache);

                let mut bind_group_entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input_texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(output_tile_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.adjustments_buffer.as_entire_binding(),
                    },
                ];
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mask_texture_view),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 3 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(&lut_texture_view),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 4 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::Sampler(&lut_sampler),
                });

                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 5 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(
                        sharpness_blur.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 6 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(
                        tonal_blur.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 7 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(
                        clarity_blur.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 8 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(
                        structure_blur.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 17,
                    resource: wgpu::BindingResource::TextureView(
                        nr_blur.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });

                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 18,
                    resource: wgpu::BindingResource::TextureView(&relight_depth_view.legacy),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&relight_depth_view.camera),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 20,
                    resource: wgpu::BindingResource::TextureView(&relight_depth_view.scene),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 21,
                    resource: wgpu::BindingResource::TextureView(&relight_pattern_view),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 22,
                    resource: wgpu::BindingResource::TextureView(
                        chroma.map_or(&self.dummy_blur_view, |(packed, _)| packed),
                    ),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 23,
                    resource: wgpu::BindingResource::TextureView(
                        chroma.map_or(&self.dummy_blur_view, |(_, energy)| energy),
                    ),
                });
                let use_flare = adjustments.global.flare_amount > 0.0;
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 9 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if use_flare {
                        &self.flare_ghosts_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 10 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                });

                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 12,
                    resource: self.prepared_layers_buffer.as_entire_binding(),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 13,
                    resource: self.curve_luts_buffer.as_entire_binding(),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 16,
                    resource: self.render_regions_buffer.as_entire_binding(),
                });
                let crop_x_start = x_start - input_x_start;
                let crop_y_start = y_start - input_y_start;

                if build_detail {
                    // The whole tile, so later partial frames find every pixel.
                    let mut detail_adjustments = adjustments;
                    detail_adjustments.tile_offset_x = input_x_start;
                    detail_adjustments.tile_offset_y = input_y_start;
                    detail_adjustments.render_region =
                        [crop_x_start, crop_y_start, tile_width, tile_height];
                    detail_adjustments.detail_cache = [0; 4];
                    detail_adjustments.source_window = source_window;
                    queue.write_buffer(
                        &self.adjustments_buffer,
                        0,
                        bytemuck::bytes_of(&detail_adjustments),
                    );
                    let mut detail_entries = bind_group_entries.clone();
                    detail_entries.push(wgpu::BindGroupEntry {
                        binding: 14,
                        resource: wgpu::BindingResource::TextureView(&self.dummy_blur_view),
                    });
                    detail_entries.push(wgpu::BindGroupEntry {
                        binding: 15,
                        resource: wgpu::BindingResource::TextureView(detail_view.as_ref().unwrap()),
                    });
                    let detail_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Detail Bind Group"),
                        layout: &self.detail_bgl,
                        entries: &detail_entries,
                    });
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let mut pass = encoder.begin_compute_pass(&Default::default());
                        pass.set_pipeline(detail_pipeline.unwrap());
                        pass.set_bind_group(0, &detail_bind_group, &[]);
                        pass.dispatch_workgroups(
                            tile_width.div_ceil(8),
                            tile_height.div_ceil(8),
                            1,
                        );
                    }
                    queue.submit(Some(encoder.finish()));
                }

                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 14,
                    resource: wgpu::BindingResource::TextureView(
                        detail_view.as_ref().unwrap_or(&self.dummy_blur_view),
                    ),
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Tile Bind Group"),
                    layout: output_bgl,
                    entries: &bind_group_entries,
                });

                // Every retained region of the tile in one dispatch: the shader
                // maps each workgroup to its region (render_regions), so a
                // partial frame costs one submit instead of one per region.
                let mut regions: Vec<[u32; 4]> = retained_regions
                    .iter()
                    .map(|retained| {
                        [
                            retained.x - input_x_start,
                            retained.y - input_y_start,
                            retained.width,
                            retained.height,
                        ]
                    })
                    .collect();
                // Keep the former full-halo dispatch available as a pixel
                // reference for the existing GPU regression suite.
                #[cfg(test)]
                if force_full_preparation {
                    regions = vec![[0, 0, input_width, input_height]];
                }
                if regions.len() > MAX_RENDER_REGIONS {
                    let bounds = retained_regions
                        .iter()
                        .fold(None, |union, region| Some(union_roi(union, *region)))
                        .unwrap();
                    regions = vec![[
                        bounds.x - input_x_start,
                        bounds.y - input_y_start,
                        bounds.width,
                        bounds.height,
                    ]];
                }
                let workgroups: u32 = regions
                    .iter()
                    .map(|region| region[2].div_ceil(8) * region[3].div_ceil(8))
                    .sum();
                let mut region_data = vec![[regions.len() as u32, 0, 0, 0]];
                region_data.extend_from_slice(&regions);
                queue.write_buffer(
                    &self.render_regions_buffer,
                    0,
                    bytemuck::cast_slice(&region_data),
                );
                let mut tile_adjustments = adjustments;
                tile_adjustments.tile_offset_x = input_x_start;
                tile_adjustments.tile_offset_y = input_y_start;
                tile_adjustments.render_region = regions[0];
                tile_adjustments.detail_cache = [u32::from(detail_view.is_some()), 0, 0, 0];
                tile_adjustments.source_window = source_window;
                queue.write_buffer(
                    &self.adjustments_buffer,
                    0,
                    bytemuck::bytes_of(&tile_adjustments),
                );
                let mut main_encoder = device.create_command_encoder(&Default::default());
                {
                    let mut compute_pass = main_encoder.begin_compute_pass(&Default::default());
                    compute_pass.set_pipeline(output_pipeline);
                    compute_pass.set_bind_group(0, &bind_group, &[]);
                    compute_pass.dispatch_workgroups(
                        RENDER_REGION_GROUPS_X,
                        workgroups.div_ceil(RENDER_REGION_GROUPS_X),
                        1,
                    );
                }
                if output_to_display {
                    for retained in &retained_regions {
                        main_encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &self.tile_output_texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: retained.x - input_x_start,
                                    y: retained.y - input_y_start,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &display_targets.as_ref().unwrap().working_texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: retained.x,
                                    y: retained.y,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: retained.width,
                                height: retained.height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
                queue.submit(Some(main_encoder.finish()));

                if !output_to_display {
                    // The copy of this tile is read back while the GPU
                    // renders the next one (queue order keeps the shared
                    // output texture intact).
                    let next = begin_texture_readback(
                        device,
                        queue,
                        output_tile_texture,
                        wgpu::Origin3d {
                            x: crop_x_start,
                            y: crop_y_start,
                            z: 0,
                        },
                        wgpu::Extent3d {
                            width: tile_width,
                            height: tile_height,
                            depth_or_array_layers: 1,
                        },
                        bytes_per_pixel,
                    );
                    let placement = TilePlacement {
                        x: x_start - bounds.x,
                        y: y_start - bounds.y,
                        width: tile_width,
                        height: tile_height,
                    };
                    if let Some((previous, previous_placement)) =
                        pending_readback.replace((next, placement))
                    {
                        #[cfg(feature = "performance-profiling")]
                        let _readback_stage =
                            crate::performance::PerformanceStage::new("gpu_tile_readback");
                        let data = finish_texture_readback(device, previous)?;
                        store_tile(&mut final_pixels, previous_placement, out_width, &data);
                    }
                }
            }
        }

        if let Some((previous, placement)) = pending_readback.take() {
            #[cfg(feature = "performance-profiling")]
            let _readback_stage = crate::performance::PerformanceStage::new("gpu_tile_readback");
            let data = finish_texture_readback(device, previous)?;
            store_tile(&mut final_pixels, placement, out_width, &data);
        }

        if let Some(state) = display_state {
            *self
                .display_render_state
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(state);
        }
        Ok((final_pixels, out_width, out_height, bounds.x, bounds.y))
    }
}

/// An input texture uploaded before the GPU processor is taken, so that the
/// caller can release its CPU pixels before rendering.
pub struct UploadedImage(GpuImageCache);

/// Uploads a CPU-readback source for [`process_and_get_dynamic_image_inner`].
/// `None` means the image exceeds the GPU's texture limits; render it from
/// [`RenderSource::Image`] instead, which renders it in windows.
/// `color_noise` (Some(linear)) also builds the chroma pyramid the
/// multi-scale colour noise reduction needs, while the CPU pixels exist.
pub fn upload_render_source(
    context: &GpuContext,
    image: &DynamicImage,
    transform_hash: u64,
    color_noise: Option<bool>,
) -> Result<Option<UploadedImage>, String> {
    let (width, height) = image.dimensions();
    let max_dim = context.limits.max_texture_dimension_2d;
    if width > max_dim || height > max_dim {
        return Ok(None);
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("gpu_texture_upload");
    let texture = create_input_texture(
        &context.device,
        &context.queue,
        image,
        INPUT_UPLOAD_BAND_ROWS,
    )?;
    let texture_view = texture.create_view(&Default::default());
    let chroma = color_noise
        .and_then(|linear| ChromaPyramid::build(image, linear))
        .map(Arc::new);
    Ok(Some(UploadedImage(GpuImageCache {
        texture,
        texture_view,
        width,
        height,
        transform_hash,
        chroma,
        chroma_gpu: None,
    })))
}

/// The error of a render this GPU cannot do because the photo exceeds its
/// texture size limit. The app translates it (`src/utils/renderErrors.ts`
/// matches this wording).
pub fn texture_limit_error(width: u32, height: u32, max_dim: u32) -> String {
    format!(
        "The image ({width} × {height} px) is larger than this graphics card can render ({max_dim} px per side)."
    )
}

/// The two photo pixels and the second one's weight (in 1/(2 x `map_size`))
/// of map texel `i` along a photo side of `size`, as `flare_tap` in
/// flare.wgsl.
fn flare_tap(i: u32, map_size: u32, size: u32) -> (u32, u32) {
    let denominator = 2 * i64::from(map_size);
    let numerator = (2 * i64::from(i) + 1) * i64::from(size) - i64::from(map_size);
    let base = numerator.div_euclid(denominator);
    let last = i64::from(size) - 1;
    (base.clamp(0, last) as u32, (base + 1).clamp(0, last) as u32)
}

/// The photo pixels Light flare's threshold pass reads, gathered for a render
/// from windows: the 2 x 2 pixels of map texel (x, y) at (2x, 2y), with the
/// texels of the input texture (`rgba_f16_row`).
fn create_flare_samples_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &DynamicImage,
) -> wgpu::Texture {
    use rayon::prelude::*;

    let (width, height) = image.dimensions();
    let side = 2 * FLARE_MAP_SIZE;
    let columns: Vec<u32> = (0..FLARE_MAP_SIZE)
        .flat_map(|i| {
            let (first, second) = flare_tap(i, FLARE_MAP_SIZE, width);
            [first, second]
        })
        .collect();
    let mut texels = vec![f16::ZERO; side as usize * side as usize * 4];
    upload_pool().install(|| {
        texels
            .par_chunks_mut(side as usize * 8)
            .enumerate()
            .for_each(|(i, rows)| {
                let (first, second) = flare_tap(i as u32, FLARE_MAP_SIZE, height);
                for (row, y) in rows
                    .chunks_exact_mut(side as usize * 4)
                    .zip([first, second])
                {
                    for (texel, &x) in row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
                        rgba_f16_row(image, x, y, texel.as_mut_slice());
                    }
                }
            });
    });
    device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("Flare Samples"),
            size: wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&texels),
    )
}

const FLARE_WINDOW_ERROR: &str = "Light flare needs the whole photo on the GPU.";

/// Photo pixels around a rendered pixel that its render may read, for the
/// renders of photos larger than the GPU's texture limit.
///
/// Every input read of the shaders, by the photo's size and the recipe:
/// - blurs (`run_blur`): each tile's blur covers the tile and `TILE_OVERLAP`
///   around it, the horizontal pass reads its radius further (largest base
///   radius 40, Structure);
/// - Highlights' local contrast (`apply_highlights_adjustment`): 7.5 x scale;
/// - Defringe (`apply_defringe`): up to 18 detail-scaled pixels;
/// - Moiré (`apply_moire`): two pattern steps, at most 8 detail-scaled pixels
///   each (`moire_settings`), for the photo and every layer;
/// - chromatic aberration (`apply_ca_correction`): the shift, at most the
///   amount times the distance from the centre;
/// - sharpening (6), luminance noise reduction (4) and diffraction
///   correction (3); the cross-bilateral colour noise reduction reads the
///   blurs only, the multi-scale one (`colorNoiseVersion` 2) the finest
///   level's energy, which reads 1 pixel around each pixel (color_noise.wgsl
///   `finest_energy`), and level 1 of the chroma pyramid, which is built
///   from the whole photo and given to each window with the samples its
///   pixels expand from (`chroma_level_window`).
///
/// Masks are read at the rendered pixel itself. Light flare's map reads the
/// whole photo, from samples gathered before the windows
/// (`create_flare_samples_texture`).
fn render_window_margin(adjustments: &AllAdjustments, width: u32, height: u32) -> u32 {
    let global = &adjustments.global;
    let short_side = width.min(height) as f32;
    // As in `run_in_window` and the shader's `scale`.
    let blur_scale = short_side / 1080.0;
    let shader_scale = (short_side / 1080.0).max(0.1);
    let blur = TILE_OVERLAP + (40.0 * blur_scale).ceil().max(1.0) as u32;
    // Full-resolution renders have detail scale 0 (1), previews less.
    let detail = global.detail_scale.max(1.0);
    let highlights = (7.5 * shader_scale).round().max(2.0) as u32;
    let defringe = (18.0 * detail).ceil() as u32 + 1;
    let moire = (2.0 * 8.0 * detail).ceil() as u32 + 1;
    let aberration = global
        .chromatic_aberration_red_cyan
        .abs()
        .max(global.chromatic_aberration_blue_yellow.abs());
    let half_diagonal = (width as f32 / 2.0).hypot(height as f32 / 2.0);
    let aberration = (aberration * half_diagonal)
        .ceil()
        .min(u32::MAX as f32 / 4.0) as u32
        + 1;
    let kernels = 6;
    // Two more pixels for rounding.
    blur.max(highlights)
        .max(defringe)
        .max(moire)
        .max(aberration)
        .max(kernels)
        .saturating_add(2)
}

/// Splits `bounds` of a `width` x `height` photo into regions to render, each
/// with the window of the photo it reads: the region and `margin` around it,
/// at most `max_dim` pixels per side. Regions follow the render tiles
/// (multiples of [`TILE_SIZE`] from the photo's origin), so every tile keeps
/// the geometry of a render from the whole photo. `None` when a tile and its
/// margins do not fit `max_dim`.
fn plan_render_windows(
    width: u32,
    height: u32,
    bounds: Roi,
    max_dim: u32,
    margin: u32,
) -> Option<Vec<(Roi, Roi)>> {
    let step = max_dim.checked_sub(margin.checked_mul(2)?)? / TILE_SIZE * TILE_SIZE;
    if step == 0 {
        return None;
    }
    let mut plan = Vec::new();
    for y in (0..height).step_by(step as usize) {
        for x in (0..width).step_by(step as usize) {
            let cell = Roi {
                x,
                y,
                width: step.min(width - x),
                height: step.min(height - y),
            };
            let Some(region) = intersect_roi(cell, bounds) else {
                continue;
            };
            let left = region.x.saturating_sub(margin);
            let top = region.y.saturating_sub(margin);
            let right = (region.x + region.width).saturating_add(margin).min(width);
            let bottom = (region.y + region.height)
                .saturating_add(margin)
                .min(height);
            plan.push((
                region,
                Roi {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                },
            ));
        }
    }
    Some(plan)
}

/// Renders a photo larger than the GPU's texture limit for CPU readback,
/// region by region from windows of it (see [`plan_render_windows`]). The
/// pixels equal those of a render from the whole photo (compared byte for
/// byte by `renders_over_the_texture_limit_match_whole_renders`).
fn render_in_windows(
    context: &GpuContext,
    processor_slot: &Mutex<Option<GpuProcessorState>>,
    image: &DynamicImage,
    request: RenderRequest,
    caller_id: &str,
    output_precision: RenderOutputPrecision,
    readback_shader: ReadbackShader,
) -> Result<DynamicImage, String> {
    let start_time = Instant::now();
    let (width, height) = image.dimensions();
    let max_dim = context.limits.max_texture_dimension_2d;
    let bounds = request.roi.unwrap_or(Roi {
        x: 0,
        y: 0,
        width,
        height,
    });
    if bounds.width == 0
        || bounds.height == 0
        || bounds
            .x
            .checked_add(bounds.width)
            .is_none_or(|right| right > width)
        || bounds
            .y
            .checked_add(bounds.height)
            .is_none_or(|bottom| bottom > height)
    {
        return Err("Render region lies outside the image.".to_string());
    }
    let masks = &request.mask_bitmaps[..request.mask_bitmaps.len().min(MAX_MASKS)];
    if let Some(mask) = masks
        .iter()
        .find(|mask| mask.dimensions() != (width, height))
    {
        return Err(format!(
            "Mask dimensions ({}x{}) do not match render dimensions ({width}x{height}).",
            mask.width(),
            mask.height()
        ));
    }
    let margin = render_window_margin(&request.adjustments.uniforms, width, height);
    let plan = plan_render_windows(width, height, bounds, max_dim, margin)
        .ok_or_else(|| texture_limit_error(width, height, max_dim))?;

    let processor_lock = lock_processor(context, processor_slot, max_dim, max_dim)?;
    let processor = &processor_lock.as_ref().unwrap().processor;
    // The multi-scale colour noise reduction measures and rebuilds the whole
    // photo; each window gets its part of level 1 (rebuilt on the CPU, as a
    // whole render's export does).
    let mut request = request;
    let is_raw = request.adjustments.global.is_raw_image != 0;
    let tuning = color_noise::Tuning::default();
    let luma_profile = luma_profile_wanted(&request.adjustments.uniforms.global);
    let settings = color_noise_settings(&mut request.adjustments.uniforms.global);
    let pyramid = (settings.is_some() || luma_profile)
        .then(|| ChromaPyramid::build(image, is_raw))
        .flatten();
    if luma_profile && let Some(pyramid) = &pyramid {
        set_luma_profile(&mut request.adjustments.uniforms.global, pyramid);
    }
    let chroma = settings.zip(pyramid).and_then(|(settings, pyramid)| {
        let pipelines = processor.chroma_pipelines.as_ref()?;
        let (thresholds, base_sigma, _) = color_noise_thresholds(
            &mut request.adjustments.uniforms.global,
            settings,
            &pyramid,
            &tuning,
        );
        let rebuilt = pyramid.reconstruct(&thresholds, base_sigma, &tuning);
        let level1 = &pyramid.levels[0];
        request.adjustments.uniforms.global.color_noise_window =
            [0, 0, level1.width as u32, level1.height as u32];
        Some((pyramid, rebuilt, pipelines))
    });
    if chroma.is_none() {
        request.adjustments.uniforms.global.color_noise[0] = 0.0;
    }
    // Light flare's map is built from samples of the whole photo.
    let flare_samples = (request.adjustments.global.flare_amount > 0.0)
        .then(|| create_flare_samples_texture(&context.device, &context.queue, image))
        .map(|texture| texture.create_view(&Default::default()));
    let output_len = bounds.width as usize * bounds.height as usize * 4;
    let mut output = match output_precision {
        RenderOutputPrecision::EightBit => RenderedPixels::U8(vec![0; output_len]),
        RenderOutputPrecision::SixteenBit => RenderedPixels::U16(vec![0; output_len]),
    };
    for (region, window) in &plan {
        let texture = create_window_texture(
            &context.device,
            &context.queue,
            image,
            *window,
            INPUT_UPLOAD_BAND_ROWS,
        )?;
        let window_masks: Vec<Arc<GrayImage>> = masks
            .iter()
            .map(|mask| {
                Arc::new(
                    image::imageops::crop_imm(
                        mask.as_ref(),
                        window.x,
                        window.y,
                        window.width,
                        window.height,
                    )
                    .to_image(),
                )
            })
            .collect();
        let input_view = texture.create_view(&Default::default());
        let mut adjustments = request.adjustments.clone();
        let window_chroma = chroma.as_ref().map(|(pyramid, rebuilt, pipelines)| {
            let level1 = &pyramid.levels[0];
            let level_window = chroma_level_window(*window, level1.width, level1.height);
            let global = &mut adjustments.uniforms.global;
            global.color_noise_window[0] = level_window[0];
            global.color_noise_window[1] = level_window[1];
            ChromaGpu::new(
                context,
                pipelines,
                &input_view,
                *window,
                width,
                height,
                is_raw,
                pyramid,
                Some(rebuilt),
                level_window,
            )
        });
        let (pixels, region_width, region_height, x, y) = processor.run_in_window(
            &input_view,
            width,
            height,
            *window,
            RenderRequest {
                adjustments,
                mask_bitmaps: &window_masks,
                lut: request.lut.clone(),
                roi: Some(*region),
            },
            false,
            output_precision,
            readback_shader,
            window_chroma
                .as_ref()
                .map(|chroma| (&chroma.packed_view, &chroma.energy_view)),
            flare_samples.as_ref(),
        )?;
        place_region(
            &mut output,
            bounds.width,
            Roi {
                x: x - bounds.x,
                y: y - bounds.y,
                width: region_width,
                height: region_height,
            },
            &pixels,
        );
    }
    log::info!(
        "[{}] {}x{} processed (ROI: {}x{}) on GPU in {} windows of at most {} px in {:?}",
        caller_id,
        width,
        height,
        bounds.width,
        bounds.height,
        plan.len(),
        max_dim,
        start_time.elapsed()
    );
    rendered_image(output, bounds.width, bounds.height)
}

/// Copies a rendered region (RGBA rows) into the output at `placement`.
fn place_region(
    output: &mut RenderedPixels,
    output_width: u32,
    placement: Roi,
    region: &RenderedPixels,
) {
    fn copy<T: Copy>(output: &mut [T], output_width: u32, placement: Roi, region: &[T]) {
        let row_len = placement.width as usize * 4;
        for (row, source) in region.chunks_exact(row_len).enumerate() {
            let start =
                ((placement.y as usize + row) * output_width as usize + placement.x as usize) * 4;
            output[start..start + row_len].copy_from_slice(source);
        }
    }
    match (output, region) {
        (RenderedPixels::U8(output), RenderedPixels::U8(region)) => {
            copy(output, output_width, placement, region)
        }
        (RenderedPixels::U16(output), RenderedPixels::U16(region)) => {
            copy(output, output_width, placement, region)
        }
        _ => unreachable!("regions share the render's precision"),
    }
}

/// The caller's processor, created (or replaced by a larger one) for a
/// `width` x `height` input texture.
fn lock_processor<'a>(
    context: &GpuContext,
    processor_slot: &'a Mutex<Option<GpuProcessorState>>,
    width: u32,
    height: u32,
) -> Result<std::sync::MutexGuard<'a, Option<GpuProcessorState>>, String> {
    let max_dim = context.limits.max_texture_dimension_2d;
    let mut processor_lock = match processor_slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::warn!("GPU processor lock was poisoned. Resetting to self-heal.");
            let mut guard = poisoned.into_inner();
            *guard = None;
            guard
        }
    };
    let mut needs_new_processor = false;
    // Processing pipelines and scratch tiles do not depend on source size.
    // The constructor clamps scratch allocation to a 2304-pixel tile, while
    // presentation targets are allocated separately at their current size.
    // Avoid discarding compiled pipelines whenever a larger photo is selected.
    let new_width = max_dim;
    let new_height = max_dim;

    if let Some(p) = processor_lock.as_ref() {
        if p.width < width || p.height < height {
            needs_new_processor = true;
        }
    } else {
        needs_new_processor = true;
    }

    if needs_new_processor {
        log::info!(
            "Creating new GPU Processor for dimensions up to {}x{}",
            new_width,
            new_height
        );

        let old_processor = processor_lock.take();
        drop(old_processor);

        let _ = context.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_millis(500)),
        });

        #[cfg(feature = "performance-profiling")]
        let processor_init_stage = crate::performance::PerformanceStage::new("gpu_processor_init");
        let new_processor = GpuProcessor::new(context.clone(), new_width, new_height)?;
        #[cfg(feature = "performance-profiling")]
        drop(processor_init_stage);

        *processor_lock = Some(GpuProcessorState {
            processor: new_processor,
            width: new_width,
            height: new_height,
        });
    }

    Ok(processor_lock)
}

pub enum RenderSource<'a> {
    /// Uploaded unless the image cache already holds `transform_hash`.
    Image {
        image: &'a DynamicImage,
        transform_hash: u64,
    },
    Uploaded(UploadedImage),
}

/// Renders on the GPU with the caller's processor slot and uploaded-source
/// cache (the app keeps both in its state), creating or resizing the
/// processor as needed.
#[allow(clippy::too_many_arguments)]
pub fn process_and_get_dynamic_image_inner(
    context: &GpuContext,
    processor_slot: &Mutex<Option<GpuProcessorState>>,
    image_cache: &Mutex<Option<GpuImageCache>>,
    source: RenderSource,
    request: RenderRequest,
    caller_id: &str,
    output_precision: RenderOutputPrecision,
    output_to_display: bool,
    analytics_config: Option<AnalyticsConfig>,
    display_identity: Option<DisplayFrameIdentity>,
    readback_shader: ReadbackShader,
) -> Result<DynamicImage, String> {
    let mut request = request;
    if output_to_display {
        let space = context.display_space;
        request.adjustments.global.output_space[0] = space.shader_code();
        request.adjustments.global.output_matrix =
            crate::image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
    }
    let start_time = Instant::now();
    let (width, height) = match &source {
        RenderSource::Image { image, .. } => image.dimensions(),
        RenderSource::Uploaded(UploadedImage(input)) => (input.width, input.height),
    };
    let device = &context.device;
    let queue = &context.queue;

    let max_dim = context.limits.max_texture_dimension_2d;
    if width > max_dim || height > max_dim {
        return match source {
            RenderSource::Image { image, .. } if !output_to_display => render_in_windows(
                context,
                processor_slot,
                image,
                request,
                caller_id,
                output_precision,
                readback_shader,
            ),
            _ => Err(texture_limit_error(width, height, max_dim)),
        };
    }

    let processor_lock = lock_processor(context, processor_slot, width, height)?;
    let processor_state = processor_lock.as_ref().unwrap();
    let processor = &processor_state.processor;

    // CPU-readback jobs (thumbnails, exports) use a transient input. Otherwise
    // each saved thumbnail forces the next slider frame to re-upload its RAW
    // preview and rebuild every blur, even though the source did not change.
    let (base_image, transform_hash, uploaded) = match source {
        RenderSource::Image {
            image,
            transform_hash,
        } => (Some(image), transform_hash, None),
        RenderSource::Uploaded(UploadedImage(input)) => (None, input.transform_hash, Some(input)),
    };
    let preuploaded = uploaded.is_some();
    let background_image_cache = Mutex::new(uploaded);
    // The JPEG preview fallback (e.g. Linux) is also foreground work.
    let image_cache = if !preuploaded && (output_to_display || caller_id == "apply_adjustments") {
        image_cache
    } else {
        &background_image_cache
    };
    let mut cache_lock = match image_cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::warn!("GPU image cache lock was poisoned. Resetting to self-heal.");
            let mut guard = poisoned.into_inner();
            *guard = None;
            guard
        }
    };
    let mut needs_new_cache = false;

    if let Some(cache) = &*cache_lock {
        if !preuploaded
            && (cache.transform_hash != transform_hash
                || cache.width != width
                || cache.height != height)
        {
            needs_new_cache = true;
        }
    } else {
        needs_new_cache = true;
    }

    if needs_new_cache {
        let base_image = base_image.ok_or("Missing source image for GPU upload.")?;
        let old_cache = cache_lock.take();
        drop(old_cache);

        let _ = context.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_millis(500)),
        });

        #[cfg(feature = "performance-profiling")]
        let upload_stage = crate::performance::PerformanceStage::new("gpu_texture_upload");
        let texture = create_input_texture(device, queue, base_image, INPUT_UPLOAD_BAND_ROWS)?;
        let texture_view = texture.create_view(&Default::default());
        #[cfg(feature = "performance-profiling")]
        drop(upload_stage);

        *cache_lock = Some(GpuImageCache {
            texture,
            texture_view,
            width,
            height,
            transform_hash,
            chroma: None,
            chroma_gpu: None,
        });
    }

    let cache = cache_lock.as_mut().unwrap();
    let chroma = prepare_color_noise(
        context,
        processor,
        cache,
        base_image,
        &mut request.adjustments,
        output_to_display || readback_shader != ReadbackShader::Export,
    );

    let skip_readback = output_to_display;
    let display_targets = output_to_display.then(|| processor.display_targets(width, height));

    #[cfg(feature = "performance-profiling")]
    let run_stage = crate::performance::PerformanceStage::new("gpu_run");
    let (processed_pixels, out_w, out_h, out_x, out_y) = processor.run_with_shader(
        &cache.texture_view,
        cache.width,
        cache.height,
        request,
        output_to_display,
        output_precision,
        readback_shader,
        chroma.as_ref().map(|(packed, energy)| (packed, energy)),
    )?;
    #[cfg(feature = "performance-profiling")]
    drop(run_stage);

    let mut final_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Final Passes Encoder"),
    });
    let mut submit_final_encoder = false;

    let mut async_readback_buffer: Option<wgpu::Buffer> = None;
    let mut async_padded_bpr: u32 = 0;

    if analytics_config.is_some() && skip_readback {
        let unpadded_bytes_per_row = 4 * out_w;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) & !(align - 1);
        let output_buffer_size = (padded_bytes_per_row * out_h) as u64;

        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Async Analytics Readback Buffer"),
            size: output_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        final_encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &display_targets.as_ref().unwrap().working_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: out_x,
                    y: out_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(out_h),
                },
            },
            wgpu::Extent3d {
                width: out_w,
                height: out_h,
                depth_or_array_layers: 1,
            },
        );

        async_readback_buffer = Some(output_buffer);
        async_padded_bpr = padded_bytes_per_row;
        submit_final_encoder = true;
    }

    if output_to_display {
        final_encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &display_targets.as_ref().unwrap().working_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: out_x,
                    y: out_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &display_targets.as_ref().unwrap().output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: out_x,
                    y: out_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: out_w,
                height: out_h,
                depth_or_array_layers: 1,
            },
        );
        submit_final_encoder = true;
    }

    if submit_final_encoder {
        queue.submit(Some(final_encoder.finish()));
    }

    if let Some(analytics) = analytics_config {
        if let Some(buffer) = async_readback_buffer {
            let worker = processor.analytics_readback.get_or_init(|| {
                let device = Arc::clone(&context.device);
                start_latest_job_worker(move |job| complete_analytics_readback(&device, job))
            });
            worker.send(AnalyticsReadbackJob {
                buffer,
                width: out_w,
                height: out_h,
                padded_bytes_per_row: async_padded_bpr,
                analytics,
            });
        } else if let RenderedPixels::U8(pixels) = &processed_pixels {
            if let Some(image) = ImageBuffer::<Rgba<u8>, _>::from_raw(out_w, out_h, pixels.clone())
            {
                analytics.sender.send(AnalyticsJob {
                    path: analytics.path,
                    image: Arc::new(DynamicImage::ImageRgba8(image)),
                    compute_waveform: analytics.compute_waveform,
                    active_waveform_channel: analytics.active_waveform_channel,
                });
            }
        } else {
            log::warn!("Skipping analytics for a high-precision CPU readback");
        }
    }

    #[cfg(feature = "performance-profiling")]
    if output_to_display {
        let counters = device.get_internal_counters().hal;
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "gpu_resources", "textures": counters.textures.read(),
                "buffers": counters.buffers.read(), "texture_views": counters.texture_views.read(),
                "bind_groups": counters.bind_groups.read(),
                "compute_pipelines": counters.compute_pipelines.read(),
                "new_source": needs_new_cache, "width": width, "height": height
            })
        );
    }

    if output_to_display
        && let Some(display) = context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
    {
        display.latest_transform.image_size = [width as f32, height as f32];
        let targets = display_targets.as_ref().unwrap();
        display.latest_transform.texture_size = [targets.width as f32, targets.height as f32];
        let overlay_view = display.prepare_overlay(device, queue, width, height);
        display.present_frame_geometry();

        queue.write_buffer(
            &display.transform_buffer,
            0,
            bytemuck::bytes_of(&display.latest_transform),
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &display.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: display.transform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &display_targets.as_ref().unwrap().output_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&display.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&overlay_view),
                },
            ],
            label: None,
        });
        display.current_bind_group = Some(bind_group);
        display.frame_path = display_identity
            .as_ref()
            .map(|identity| identity.path.clone());
        display.frame_texture = Some((targets.output_texture.clone(), width, height));
        display.frame_serial += 1;
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "display_frame_bound", "frame": display.frame_serial,
                "epoch_ms": crate::performance::epoch_ms()
            })
        );
        display.render(device, queue);
        *processor
            .display_frame
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            display_identity.map(|identity| DisplayFrame {
                identity,
                roi: Roi {
                    x: out_x,
                    y: out_y,
                    width: out_w,
                    height: out_h,
                },
            });
    }

    if skip_readback {
        let duration = start_time.elapsed();
        let fps = 1.0 / duration.as_secs_f64();
        log::info!(
            "[{}] {}x{} native WGPU display updated in {:?} ({:.2} FPS)",
            caller_id,
            width,
            height,
            duration,
            fps
        );
        return Ok(DynamicImage::new_rgba8(0, 0));
    }

    let duration = start_time.elapsed();
    let fps = 1.0 / duration.as_secs_f64();
    log::info!(
        "[{}] {}x{} processed (ROI: {}x{}) on GPU in {:?} ({:.2} FPS)",
        caller_id,
        width,
        height,
        out_w,
        out_h,
        duration,
        fps
    );

    rendered_image(processed_pixels, out_w, out_h)
}

fn rendered_image(pixels: RenderedPixels, width: u32, height: u32) -> Result<DynamicImage, String> {
    match pixels {
        RenderedPixels::U8(pixels) => {
            let img_buf = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, pixels)
                .ok_or("Failed to create 8-bit image buffer from GPU data")?;
            Ok(DynamicImage::ImageRgba8(img_buf))
        }
        RenderedPixels::U16(pixels) => {
            let img_buf = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_raw(width, height, pixels)
                .ok_or("Failed to create 16-bit image buffer from GPU data")?;
            Ok(DynamicImage::ImageRgba16(img_buf))
        }
    }
}

#[cfg(test)]
mod display_geometry_tests {
    use super::{DisplayGeometry, split_shows};

    #[test]
    fn the_split_shows_only_beside_frames_of_its_framing() {
        assert!(split_shows(Some(400.0), false, Some(7), 7));
        // No divider, the crop-editing frame, no before or another framing.
        assert!(!split_shows(None, false, Some(7), 7));
        assert!(!split_shows(Some(400.0), true, Some(7), 7));
        assert!(!split_shows(Some(400.0), false, None, 7));
        assert!(!split_shows(Some(400.0), false, Some(7), 8));
    }

    #[test]
    fn crop_editing_frames_use_the_crop_box_and_angle() {
        let geometry = DisplayGeometry {
            rect: [10.0, 20.0, 300.0, 200.0],
            crop_rect: [0.0, 0.0, 400.0, 300.0],
            crop_rotation: 0.1,
        };
        assert_eq!(
            geometry.placement(false),
            ([10.0, 20.0, 300.0, 200.0], 0.0, [0.0; 4])
        );
        assert_eq!(
            geometry.placement(true),
            ([0.0, 0.0, 400.0, 300.0], 0.1, [0.0, 0.0, 400.0, 300.0])
        );
    }

    use super::{DisplayPlacement, fit_placement, split_x_resized};

    #[test]
    fn the_split_divider_stays_on_the_same_photo_point_when_refitted() {
        let old = [100.0, 0.0, 1800.0, 1200.0];
        let new = [42.0, 0.0, 2946.0, 1964.0];
        // A third across the photograph before and after.
        let x = split_x_resized(100.0 + 600.0, old, new);
        assert!((x - (42.0 + 982.0)).abs() < 1e-3);
        // Not refitted (panels beside, zoomed, multi view): unchanged.
        assert_eq!(split_x_resized(700.0, old, old), 700.0);
    }

    fn placement(viewport: Option<[f32; 4]>, rect: [f32; 4]) -> DisplayPlacement {
        DisplayPlacement {
            window: [2000.0, 1200.0],
            clip: [-4.0, -4.0, 2008.0, 1208.0],
            viewport,
            geometry: DisplayGeometry {
                rect,
                crop_rect: rect,
                crop_rotation: 0.0,
            },
        }
    }

    #[test]
    fn a_viewer_filling_the_window_refits_the_photo_on_resize() {
        // A 3:2 photograph fitted in a 2000 × 1200 window, then full screen.
        let before = placement(
            Some([0.0, 0.0, 2000.0, 1200.0]),
            [100.0, 0.0, 1800.0, 1200.0],
        );
        let after = before.resized(3024, 1964, false);
        assert_eq!(after.window, [3024.0, 1964.0]);
        assert_eq!(after.viewport, Some([0.0, 0.0, 3024.0, 1964.0]));
        assert_eq!(after.clip, [-4.0, -4.0, 3032.0, 1972.0]);
        let [x, y, w, h] = after.geometry.rect;
        assert!((w / h - 1.5).abs() < 1e-4, "keeps the aspect ratio");
        // The screen is wider than 3:2: full height, centred across.
        assert_eq!((y, h), (0.0, 1964.0));
        assert!((x - (3024.0 - w) / 2.0).abs() < 1e-3, "centred");
        assert_eq!(after.geometry.crop_rect, after.geometry.rect);
    }

    #[test]
    fn otherwise_a_resize_keeps_the_photos_pixels() {
        // A viewer beside panels, a zoomed view (no viewport) and a crop
        // editing frame: only the window size changes, nothing is stretched.
        let rect = [300.0, 80.0, 1200.0, 800.0];
        for (viewport, uncropped) in [
            (Some([280.0, 60.0, 1400.0, 1000.0]), false),
            (None, false),
            (Some([0.0, 0.0, 2000.0, 1200.0]), true),
        ] {
            let before = placement(viewport, rect);
            assert_eq!(
                before.resized(3024, 1964, uncropped),
                DisplayPlacement {
                    window: [3024.0, 1964.0],
                    ..before
                }
            );
        }
    }

    #[test]
    fn fit_contains_and_centres() {
        assert_eq!(
            fit_placement([10.0, 20.0, 400.0, 200.0], 300.0, 300.0),
            [110.0, 20.0, 200.0, 200.0]
        );
    }
}
