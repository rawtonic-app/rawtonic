// SPDX-License-Identifier: AGPL-3.0-or-later
// A small, file-free diagnostic of the actual processing pipeline. This is
// deliberately not a performance benchmark or a replacement rendering path.
use std::sync::{Arc, Mutex};

use rawtonic_gpu::gpu_processing::{
    GpuContext, GpuProcessorSlot, ReadbackShader, RenderOutputPrecision, RenderRequest,
    RenderSource, process_and_get_dynamic_image_inner, processing_device_descriptor,
};

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
    // Match the larger stacks used by rendering workers. Windows' default main
    // stack can overflow inside the software driver's shader compilation.
    std::thread::Builder::new()
        .name("processing-probe".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(run)
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "processing probe panicked".to_string())?
}

fn run() -> Result<(), String> {
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
    let source = image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(
        size,
        size,
        image::Rgb([0.2, 0.4, 0.6]),
    ));
    let readback = match std::env::args().nth(2).as_deref() {
        None | Some("export") => ReadbackShader::Export,
        Some("display") => ReadbackShader::Display,
        _ => return Err("expected export or display after the size".into()),
    };
    let tone = std::env::args().nth(3).unwrap_or_else(|| "standard".into());
    if !matches!(tone.as_str(), "standard" | "basic" | "agx") {
        return Err("expected standard, basic or agx after the readback mode".into());
    }
    let adjustments = rawtonic_engine::image_processing::get_all_adjustments_from_json(
        &serde_json::json!({"renderVersion":7,"toneMapper":tone,"sharpness":0,"colorNoiseReduction":0,"lumaNoiseReduction":0}),
        false,
        None,
    );
    eprintln!("PROCESSING {size}x{size} {readback:?} tone={tone}");
    let output = process_and_get_dynamic_image_inner(
        &context,
        &GpuProcessorSlot::background(None),
        &Mutex::new(None),
        RenderSource::Image {
            image: &source,
            transform_hash: 1,
        },
        RenderRequest {
            adjustments,
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        "processing-probe",
        RenderOutputPrecision::EightBit,
        false,
        None,
        None,
        readback,
    )?;
    let pixels = output.to_rgba8().into_raw();
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
