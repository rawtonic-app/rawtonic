// SPDX-License-Identifier: AGPL-3.0-or-later
// A small, file-free diagnostic of the actual processing pipeline. This is
// deliberately not a performance benchmark or a replacement rendering path.
use std::sync::{Arc, Mutex};

use half::f16;
use rawtonic_gpu::gpu_processing::{
    GpuContext, GpuProcessor, RenderOutputPrecision, RenderRequest, RenderedPixels,
    processing_device_descriptor,
};
use wgpu::util::{DeviceExt, TextureDataOrder};

struct Logger;
impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }
    fn flush(&self) {}
}
static LOGGER: Logger = Logger;

fn main() -> Result<(), String> {
    log::set_logger(&LOGGER).map_err(|error| error.to_string())?;
    log::set_max_level(log::LevelFilter::Info);
    let size: u32 = std::env::args()
        .nth(1)
        .map(|value| value.parse().map_err(|_| "expected a size from 1 to 256"))
        .transpose()?
        .unwrap_or(4);
    if !(1..=256).contains(&size) {
        return Err("expected a size from 1 to 256".into());
    }
    #[allow(unused_mut)]
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    #[cfg(target_os = "windows")]
    if std::env::var("WGPU_BACKEND").is_err() {
        descriptor.backends = wgpu::Backends::PRIMARY;
    }
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|error| error.to_string())?;
    eprintln!("ADAPTER {:?}", adapter.get_info());
    let limits = adapter.limits();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&processing_device_descriptor(&adapter)))
            .map_err(|error| error.to_string())?;
    device.set_device_lost_callback(|reason, message| {
        eprintln!("DEVICE_LOST {reason:?}: {message}");
    });
    device.on_uncaptured_error(Arc::new(|error| {
        eprintln!("UNCAPTURED_GPU_ERROR: {error}");
    }));
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
        display_space: Default::default(),
    };
    let pixels: Vec<f16> = (0..size * size)
        .flat_map(|_| [0.2, 0.4, 0.6, 1.0].map(f16::from_f32))
        .collect();
    let texture = context.device.create_texture_with_data(
        &context.queue,
        &wgpu::TextureDescriptor {
            label: Some("processing probe input"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        TextureDataOrder::MipMajor,
        bytemuck::cast_slice(&pixels),
    );
    let processor = GpuProcessor::new(context.clone(), size, size)?;
    let adjustments = rawtonic_engine::image_processing::get_all_adjustments_from_json(
        &serde_json::json!({"sharpness":0,"colorNoiseReduction":0,"lumaNoiseReduction":0}),
        false,
        Some(3),
    );
    eprintln!("PROCESSING {size}x{size}");
    let (pixels, ..) = processor.run(
        &texture.create_view(&Default::default()),
        size,
        size,
        RenderRequest {
            adjustments,
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        false,
        RenderOutputPrecision::EightBit,
    )?;
    let RenderedPixels::U8(pixels) = pixels else {
        return Err("unexpected output precision".into());
    };
    let rgb_max = pixels
        .chunks_exact(4)
        .flat_map(|pixel| pixel[..3].iter().copied())
        .max()
        .unwrap_or(0);
    eprintln!("RESULT first_rgba={:?} rgb_max={rgb_max}", &pixels[..4]);
    if rgb_max == 0 {
        return Err("positive input rendered entirely black".into());
    }
    Ok(())
}
