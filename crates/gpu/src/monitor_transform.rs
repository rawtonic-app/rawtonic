//! The monitor's display transform in the present pass (display.wgsl,
//! group 2): the window's frame, background and multi-view cells are
//! converted into the monitor profile as they are drawn, on the GPU, so no
//! pixel crosses to the CPU (rawtonic-display-profile builds the table).
//! Without a transform the shader passes every value through unchanged.

use rawtonic_display_profile::DisplayLut;
use wgpu::util::{DeviceExt, TextureDataOrder};

/// The uniforms of display.wgsl's `Monitor`.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MonitorUniform {
    enabled: f32,
    size: f32,
    curve_size: f32,
    _pad: f32,
}

/// The bind group layout of display.wgsl's group 2.
pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let texture = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        count: None,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension,
            multisampled: false,
        },
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Display Monitor BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            },
            texture(1, wgpu::TextureViewDimension::D3),
            texture(2, wgpu::TextureViewDimension::D2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                count: None,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            },
        ],
    })
}

fn half_texels(values: &[[f32; 3]]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 1.0])
        .flat_map(|v| half::f16::from_f32(v).to_le_bytes())
        .collect()
}

/// The transform bound with every display draw.
pub struct MonitorBinding {
    pub(crate) bind_group: wgpu::BindGroup,
    lut: Option<DisplayLut>,
}

impl MonitorBinding {
    /// No transform: every value passes through.
    pub fn none(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        Self::bind(device, None, layout, None)
    }

    /// Binds `lut`, or no transform for `None`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        lut: Option<&DisplayLut>,
    ) -> Self {
        Self::bind(device, Some(queue), layout, lut)
    }

    fn bind(
        device: &wgpu::Device,
        queue: Option<&wgpu::Queue>,
        layout: &wgpu::BindGroupLayout,
        lut: Option<&DisplayLut>,
    ) -> Self {
        let lut = lut.filter(|_| queue.is_some());
        let (size, levels, curve_size, curve) = match lut {
            Some(lut) => (
                lut.size(),
                half_texels(lut.levels()),
                lut.curve().len() as u32,
                half_texels(lut.curve()),
            ),
            None => (1, Vec::new(), 1, Vec::new()),
        };
        let texture = |label, dimension, width, height, depth, data: &[u8]| {
            let descriptor = wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: depth,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            };
            match queue.filter(|_| !data.is_empty()) {
                Some(queue) => device.create_texture_with_data(
                    queue,
                    &descriptor,
                    TextureDataOrder::LayerMajor,
                    data,
                ),
                // Unused while disabled; wgpu clears new textures.
                None => device.create_texture(&descriptor),
            }
        };
        let levels = texture(
            "Display Monitor Levels",
            wgpu::TextureDimension::D3,
            size,
            size,
            size,
            &levels,
        );
        let curve = texture(
            "Display Monitor Curve",
            wgpu::TextureDimension::D2,
            curve_size,
            1,
            1,
            &curve,
        );
        let uniform = MonitorUniform {
            enabled: if lut.is_some() { 1.0 } else { 0.0 },
            size: size as f32,
            curve_size: curve_size as f32,
            _pad: 0.0,
        };
        let uniforms = rawtonic_rawdecode::gpu_util::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("Display Monitor Uniforms"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Display Monitor Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Display Monitor"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &levels.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        &curve.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            bind_group,
            lut: lut.cloned(),
        }
    }

    /// Whether a transform is bound.
    pub fn converts(&self) -> bool {
        self.lut.is_some()
    }

    /// A clear colour (display-encoded, premultiplied) as the shader
    /// converts drawn colours, so the area around the photograph matches.
    pub fn convert_clear(&self, color: [f32; 4]) -> [f32; 4] {
        let Some(lut) = &self.lut else {
            return color;
        };
        let alpha = color[3];
        if alpha <= 0.0 {
            return color;
        }
        let rgb = lut.apply([color[0], color[1], color[2]].map(|v| v / alpha));
        [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha]
    }
}
