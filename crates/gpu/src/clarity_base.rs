//! Clarity version 4's large-scale base on a reduced grid
//! (`shaders/clarity_base.wgsl`). Version 3 reads the mean log luminance of
//! the large blur (index 3), computed at full resolution for every tile: a
//! quarter of an export's GPU time (notes: benchmark-results/
//! exp2-2026-11-26). Version 4 computes that field once per render on
//! blocks of F × F pixels and interpolates it into each tile's large blur,
//! where the main shader reads it unchanged. Dehaze and Glow read the large
//! blur's colour, which stays the full-resolution blur.

use super::{CompileWeight, Roi, create_compute_pipeline, create_shader};
use rawtonic_rawdecode::{gpu_submit, gpu_util};

/// Block side for the large blur's radius (pixels): the largest power of
/// two that keeps at least 32 blocks of radius, at most 8 (4 on 24–60 MP
/// photos, 1 on small previews). Larger blocks cost no less time on the 37
/// test photos but doubled the largest pixel differences from version 3
/// (CLR4 report).
pub(super) fn block_step(radius: u32) -> u32 {
    match radius / 32 {
        0 | 1 => 1,
        2 | 3 => 2,
        4..=7 => 4,
        _ => 8,
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    step: u32,
    window_x: u32,
    window_y: u32,
    window_width: u32,
    window_height: u32,
    origin_x: u32,
    origin_y: u32,
    blocks_x: u32,
    blocks_y: u32,
    pixel_radius: u32,
    sigma: f32,
    radius: u32,
    flags: u32,
    tile_x: u32,
    tile_y: u32,
    tile_flags: u32,
}

/// One render's block field and the parameters it was made with.
#[derive(Clone)]
pub(super) struct Field {
    view: wgpu::TextureView,
    params: Params,
}

pub(super) struct ClarityBasePasses {
    field_bgl: wgpu::BindGroupLayout,
    upsample_bgl: wgpu::BindGroupLayout,
    reduce: wgpu::ComputePipeline,
    blur_h: wgpu::ComputePipeline,
    blur_v: wgpu::ComputePipeline,
    upsample: wgpu::ComputePipeline,
    upsample_params: wgpu::Buffer,
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

const UNIFORM_ENTRY: wgpu::BindGroupLayoutEntry = wgpu::BindGroupLayoutEntry {
    binding: 2,
    visibility: wgpu::ShaderStages::COMPUTE,
    ty: wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    },
    count: None,
};

impl ClarityBasePasses {
    pub(super) fn new(device: &wgpu::Device) -> Result<Self, String> {
        let module = create_shader(
            device,
            "Clarity Base Shader",
            include_str!("shaders/clarity_base.wgsl"),
        )?;
        let field_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Clarity Base Field BGL"),
            entries: &[
                texture_entry(0),
                storage_entry(1, wgpu::TextureFormat::R32Float),
                UNIFORM_ENTRY,
            ],
        });
        let upsample_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Clarity Base Upsample BGL"),
            entries: &[
                texture_entry(0),
                UNIFORM_ENTRY,
                texture_entry(3),
                storage_entry(4, wgpu::TextureFormat::Rgba16Float),
            ],
        });
        let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, entry: &str| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            create_compute_pipeline(
                device,
                &wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                },
                CompileWeight::Tiny,
            )
        };
        Ok(Self {
            reduce: pipeline("Clarity Base Reduce", &field_bgl, "reduce_log")?,
            blur_h: pipeline("Clarity Base Blur H", &field_bgl, "blur_horizontal")?,
            blur_v: pipeline("Clarity Base Blur V", &field_bgl, "blur_vertical")?,
            upsample: pipeline("Clarity Base Upsample", &upsample_bgl, "upsample")?,
            upsample_params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Clarity Base Upsample Params"),
                size: std::mem::size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            field_bgl,
            upsample_bgl,
        })
    }

    /// The parameters of the field for the input `window` of the photo
    /// (photo pixels) and the large blur's `radius`.
    fn field_params(window: Roi, radius: u32, srgb: bool) -> Params {
        let step = block_step(radius);
        let origin_x = window.x / step;
        let origin_y = window.y / step;
        let blocks_x = (window.x + window.width).div_ceil(step) - origin_x;
        let blocks_y = (window.y + window.height).div_ceil(step) - origin_y;
        // As blur.wgsl: sigma is half the radius. The blocks either side
        // that hold a tap: offsets up to the radius from the block centre.
        let sigma = radius as f32 / 2.0;
        let blocks = (2 * radius + step - 1) / (2 * step);
        Params {
            step,
            window_x: window.x,
            window_y: window.y,
            window_width: window.width,
            window_height: window.height,
            origin_x,
            origin_y,
            blocks_x,
            blocks_y,
            pixel_radius: radius,
            sigma,
            radius: blocks,
            flags: u32::from(srgb) << 1,
            ..Default::default()
        }
    }

    /// Whether `field` was made for these inputs.
    pub(super) fn matches(field: &Field, window: Roi, radius: u32, srgb: bool) -> bool {
        field.params == Self::field_params(window, radius, srgb)
    }

    /// The block field of `input` (the photo `window`).
    pub(super) fn field(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::TextureView,
        window: Roi,
        radius: u32,
        srgb: bool,
    ) -> Field {
        let params = Self::field_params(window, radius, srgb);
        let texture = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: params.blocks_x,
                        height: params.blocks_y,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let blocks = texture("Clarity Base Blocks");
        let across = texture("Clarity Base Across");
        let field = texture("Clarity Base Field");
        let buffer = gpu_util::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("Clarity Base Field Params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let passes = [
            (&self.reduce, input, &blocks),
            (&self.blur_h, &blocks, &across),
            (&self.blur_v, &across, &field),
        ];
        for (pipeline, source, target) in passes {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Clarity Base Field BG"),
                layout: &self.field_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(target),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(params.blocks_x.div_ceil(8), params.blocks_y.div_ceil(8), 1);
        }
        gpu_submit::submit(
            device,
            queue,
            "clarity base field",
            u64::from(params.blocks_x) * u64::from(params.blocks_y),
            Some(encoder.finish()),
        );
        Field {
            view: field,
            params,
        }
    }

    /// Fills `output` (a tile's large blur, `size` texels from photo pixel
    /// `tile`) with the field in alpha and `colour`'s colour, if given.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn upsample(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        field: &Field,
        tile: (u32, u32),
        size: (u32, u32),
        colour: Option<&wgpu::TextureView>,
        dummy: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        let params = Params {
            tile_x: tile.0,
            tile_y: tile.1,
            tile_flags: u32::from(colour.is_some()),
            ..field.params
        };
        queue.write_buffer(&self.upsample_params, 0, bytemuck::bytes_of(&params));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Clarity Base Upsample BG"),
            layout: &self.upsample_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&field.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.upsample_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(colour.unwrap_or(dummy)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(output),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.upsample);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(16), size.1.div_ceil(16), 1);
        }
        gpu_submit::submit(
            device,
            queue,
            "clarity base tile",
            u64::from(size.0) * u64::from(size.1),
            Some(encoder.finish()),
        );
    }
}
