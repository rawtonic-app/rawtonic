// SPDX-License-Identifier: AGPL-3.0-or-later
//! The geometry warp of `renderVersion` 7 (`GeometryParams::gpu_warp`) on
//! the GPU (shaders/warp.wgsl): the f32 arithmetic of `renderVersion` 6
//! per output pixel. It is the one warp of such recipes: the display's
//! preview bases, full-resolution frame, regions and mask sources and the
//! exports all warp here, so they compute the same warped pixels (notes:
//! parity-gaps/G43, task GW). Metal compiles with fast math, so the result
//! differs from the CPU's f32 warp within float rounding; it does not
//! depend on how the output is cut into dispatches.
//!
//! Without a registered device, or for a frame larger than the GPU's
//! storage buffers, the warp runs on the CPU (`f32_warp`) for every use
//! alike ([`runs_for`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use wgpu::util::DeviceExt;

use crate::image_processing::{SourceRows, WarpUniforms};

/// Uniforms of shaders/warp.wgsl (`Params`).
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    origin: [f32; 4],
    step_x: [f32; 4],
    step_y: [f32; 4],
    width: u32,
    height: u32,
    flags: u32,
    table_len: u32,
    src_col0: u32,
    src_row0: u32,
    src_cols: u32,
    src_rows: u32,
    out_x0: u32,
    out_y0: u32,
    out_width: u32,
    out_step: u32,
    band_row0: u32,
    band_rows: u32,
    dst_row0: u32,
    _pad0: u32,
    cx: f32,
    cy: f32,
    hd: f32,
    max_radius_sq_inv: f32,
    auto_crop_scale: f32,
    k1: f32,
    k2: f32,
    k3: f32,
    lens_dist_amt: f32,
    k_distortion_c: f32,
    vr: f32,
    vb: f32,
    vr2: f32,
    vb2: f32,
    inv_hd2: f32,
    vk1: f32,
    vk2: f32,
    vk3: f32,
    lens_vig_amt: f32,
    table_step: f32,
    table_end_slope: f32,
    _pad1: [f32; 3],
}

const LENS: u32 = 1;
const PTLENS: u32 = 2;
const TABLE: u32 = 4;
const KNOTS: u32 = 8;
const MANUAL: u32 = 16;
const TCA: u32 = 32;
const VIGNETTING: u32 = 64;
const KEEP_NEGATIVE: u32 = 128;
#[cfg(any(test, feature = "test-support"))]
const POSITIONS: u32 = 256;

/// Output pixels per submission: display frames queued meanwhile wait for
/// one band, not the whole frame, and a band's buffers stay small.
const BAND_PIXELS: u32 = 4 << 20;

/// Which output pixels a warp computes: row `r` < `rows`, pixel `i` <
/// `width` is frame pixel (`x0 + i`, `y0 + r × step`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct OutputRows {
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub rows: u32,
    pub step: u32,
}

impl OutputRows {
    /// The whole `width × height` frame.
    pub fn whole(width: u32, height: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            width,
            rows: height,
            step: 1,
        }
    }
}

struct Pipeline {
    device: Arc<wgpu::Device>,
    layout: wgpu::BindGroupLayout,
    warp: wgpu::ComputePipeline,
}

static PIPELINE: Mutex<Option<Arc<Pipeline>>> = Mutex::new(None);

fn pipeline_for(device: &Arc<wgpu::Device>) -> Arc<Pipeline> {
    let mut slot = PIPELINE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(p) = slot.as_ref().filter(|p| Arc::ptr_eq(&p.device, device)) {
        return p.clone();
    }
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Geometry warp"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/warp.wgsl").into()),
    });
    let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    };
    let storage = |read_only| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Geometry warp"),
        entries: &[
            entry(
                0,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            entry(1, storage(true)),
            entry(2, storage(false)),
            entry(3, storage(true)),
            entry(4, storage(false)),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Geometry warp"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let warp = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("warp"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("warp"),
        compilation_options: Default::default(),
        cache: None,
    });
    let p = Arc::new(Pipeline {
        device: device.clone(),
        layout,
        warp,
    });
    *slot = Some(p.clone());
    p
}

/// The largest storage buffer the warp binds on `device`.
fn max_binding(device: &wgpu::Device) -> u64 {
    let limits = device.limits();
    limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size)
}

/// Whether the warp of a `width × height` frame runs on the GPU: a device
/// is registered and the whole frame fits one storage buffer. Every warp
/// of the frame (whole, regions, rows) asks this, so they all run on the
/// same processor.
pub fn runs_for(width: u32, height: u32) -> bool {
    let bytes = u64::from(width) * u64::from(height) * 12;
    width > 0
        && height > 0
        && crate::edge_fringe::gpu::device()
            .is_some_and(|(device, _)| bytes <= max_binding(&device))
}

/// Polls the device until `done` receives, sleeping between polls: a
/// blocking poll holds wgpu's fence lock and stalls other threads'
/// submissions (display frames, other export workers) meanwhile.
fn wait_for<T>(device: &wgpu::Device, done: &std::sync::mpsc::Receiver<T>) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|e| format!("Failed while polling the GPU: {e}"))?;
        match done.try_recv() {
            Ok(value) => return Ok(value),
            Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_micros(200));
            }
            Err(error) => return Err(format!("The GPU warp did not finish: {error}")),
        }
    }
}

/// The base uniforms of `u` warping `source` into `out`.
fn params(u: &WarpUniforms, source: &SourceRows, out: OutputRows) -> (Params, Vec<f32>) {
    let mut flags = 0;
    let mut table_data = vec![0.0f32];
    let (mut table_step, mut table_end_slope, mut table_len) = (0.0, 0.0, 0);
    if u.lens {
        flags |= LENS;
        if let Some(table) = &u.table {
            flags |= TABLE;
            table_len = table.values.len() as u32;
            table_step = table.step;
            table_end_slope = table.end_slope;
            table_data = table.values.clone();
            if table.radii.is_empty() {
                table_data.extend(&table.tangents);
            } else {
                flags |= KNOTS;
                table_data.extend(std::iter::repeat_n(0.0, table.values.len()));
                table_data.extend(&table.radii);
            }
        } else if u.ptlens {
            flags |= PTLENS;
        }
    }
    let manual = u.manual_c.inspect(|_| flags |= MANUAL).unwrap_or(0.0);
    let [vr, vb, vr2, vb2, inv_hd2] = u.tca.inspect(|_| flags |= TCA).unwrap_or_default();
    let [vk1, vk2, vk3, lens_vig_amt] = u
        .vignetting
        .inspect(|_| flags |= VIGNETTING)
        .unwrap_or_default();
    if u.keep_negative {
        flags |= KEEP_NEGATIVE;
    }
    let four = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
    let params = Params {
        origin: four(u.origin),
        step_x: four(u.step_x),
        step_y: four(u.step_y),
        width: source.width,
        height: source.height,
        flags,
        table_len,
        src_col0: source.col0,
        src_row0: source.row0,
        src_cols: source.cols,
        src_rows: source.rows(),
        out_x0: out.x0,
        out_y0: out.y0,
        out_width: out.width,
        out_step: out.step.max(1),
        cx: u.cx,
        cy: u.cy,
        hd: u.hd,
        max_radius_sq_inv: u.max_radius_sq_inv,
        auto_crop_scale: u.auto_crop_scale,
        k1: u.k[0],
        k2: u.k[1],
        k3: u.k[2],
        lens_dist_amt: u.lens_dist_amt,
        k_distortion_c: manual,
        vr,
        vb,
        vr2,
        vb2,
        inv_hd2,
        vk1,
        vk2,
        vk3,
        lens_vig_amt,
        table_step,
        table_end_slope,
        ..Default::default()
    };
    (params, table_data)
}

/// Output pixels `out` of the warp `u` of the frame `source` holds rows
/// and columns of, RGB rows of `out.width` pixels, on the registered
/// device. Fails when the GPU fails, or when a sample needs a pixel
/// `source` does not hold (a region planned too tight).
pub(crate) fn warp(
    u: &WarpUniforms,
    source: &SourceRows,
    out: OutputRows,
) -> Result<Vec<f32>, String> {
    warp_with(u, source, out, 0)
}

/// The source position (x, y, and z < 0 where the pixel is left black) of
/// output pixels `out` of the warp `u` of a `width × height` frame, as the
/// GPU computes them for [`warp`] (precision tests).
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn positions(
    u: &WarpUniforms,
    (width, height): (u32, u32),
    out: OutputRows,
) -> Result<Vec<f32>, String> {
    let none = [0.0f32; 3];
    let source = SourceRows {
        pixels: &none,
        width,
        height,
        row0: 0,
        col0: 0,
        cols: 1,
    };
    warp_with(u, &source, out, POSITIONS)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAILING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes this thread's GPU warps fail as a failing GPU would (`true`), or
/// run again (tests of the failure paths).
#[cfg(any(test, feature = "test-support"))]
pub fn fail_on_this_thread(failing: bool) {
    FAILING.with(|f| f.set(failing));
}

fn warp_with(
    u: &WarpUniforms,
    source: &SourceRows,
    out: OutputRows,
    flags: u32,
) -> Result<Vec<f32>, String> {
    #[cfg(any(test, feature = "test-support"))]
    if FAILING.with(|f| f.get()) {
        return Err("The warp failed on the GPU: failure injected by a test.".to_string());
    }
    let (device, queue) = crate::edge_fringe::gpu::device()
        .ok_or_else(|| "No graphics device is registered for the warp.".to_string())?;
    let mut result = vec![0.0f32; out.width as usize * out.rows as usize * 3];
    if result.is_empty() {
        return Ok(result);
    }
    scoped(&device, || {
        let warp = Prepared::new(&device, u, source, out, flags)?;
        warp.read_back(&queue, &mut result)?;
        warp.check(&queue)
    })?;
    Ok(result)
}

/// The warp of `adjustments` on `image` (its whole decode) into a new
/// buffer of the warped frame on `device` (RGB f32 rows of the same size,
/// `STORAGE | COPY_SRC`): the pixels [`warp`] gives for the whole frame,
/// left on the GPU. `None` where this warp does not run on the GPU (a
/// recipe before `renderVersion` 7 or without a warp, a frame the GPU
/// cannot hold: [`runs_for`]). Returns once the warp is done.
pub fn warp_into_buffer(
    device: &Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    image: &image::DynamicImage,
    adjustments: &serde_json::Value,
) -> Result<Option<wgpu::Buffer>, String> {
    let Some(source) = image.as_rgb32f() else {
        return Ok(None);
    };
    let (width, height) = source.dimensions();
    let Some(u) = crate::image_processing::gpu_warp_uniforms(source, adjustments) else {
        return Ok(None);
    };
    #[cfg(any(test, feature = "test-support"))]
    if FAILING.with(|f| f.get()) {
        return Err("The warp failed on the GPU: failure injected by a test.".to_string());
    }
    let source = SourceRows::whole(source.as_raw(), width, height);
    let out = OutputRows::whole(width, height);
    scoped(device, || {
        let warp = Prepared::new(device, &u, &source, out, 0)?;
        let frame = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Warped frame"),
            size: u64::from(width) * u64::from(height) * 12,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // A band before: done before the next is submitted, so two bands
        // at most wait in the queue ahead of a display frame.
        let mut in_flight: Option<std::sync::mpsc::Receiver<()>> = None;
        for (row0, rows) in warp.bands() {
            let mut encoder = device.create_command_encoder(&Default::default());
            warp.encode(&mut encoder, row0, rows, &frame, row0);
            queue.submit(Some(encoder.finish()));
            let (sender, done) = std::sync::mpsc::channel();
            queue.on_submitted_work_done(move || {
                let _ = sender.send(());
            });
            if let Some(previous) = in_flight.replace(done) {
                wait_for(device, &previous)?;
            }
        }
        if let Some(last) = in_flight {
            wait_for(device, &last)?;
        }
        warp.check(queue)?;
        Ok(Some(frame))
    })
}

/// Every fourth row (0, 4, 8, …) of a warped frame in `buffer` (RGB f32,
/// `width × height`, [`warp_into_buffer`]), read back.
pub fn read_every_fourth_row(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    (width, height): (u32, u32),
) -> Result<Vec<f32>, String> {
    let row_bytes = u64::from(width) * 12;
    let rows = height.div_ceil(4);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Warp noise rows"),
        size: u64::from(rows) * row_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    for k in 0..rows {
        encoder.copy_buffer_to_buffer(
            buffer,
            u64::from(4 * k) * row_bytes,
            &readback,
            u64::from(k) * row_bytes,
            row_bytes,
        );
    }
    queue.submit(Some(encoder.finish()));
    map(device, &readback, |bytes| {
        bytemuck::cast_slice(bytes).to_vec()
    })
}

/// `run` with the device's out-of-memory, validation and internal errors
/// (a failed submission among them) caught.
fn scoped<T>(device: &wgpu::Device, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let result = run();
    let internal = pollster::block_on(internal.pop());
    let validation = pollster::block_on(validation.pop());
    let memory = pollster::block_on(memory.pop());
    if let Some(error) = memory.or(validation).or(internal) {
        return Err(format!("The warp failed on the GPU: {error}"));
    }
    result
}

/// Maps `buffer` (`MAP_READ`, its copies submitted) and reads it with
/// `read`.
fn map<T>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    read: impl FnOnce(&[u8]) -> T,
) -> Result<T, String> {
    let (sender, mapped) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    wait_for(device, &mapped)?.map_err(|e| format!("Warp readback failed: {e}"))?;
    let value = read(&buffer.slice(..).get_mapped_range());
    buffer.unmap();
    Ok(value)
}

/// A warp with its source, table and uniforms on the GPU, dispatched in
/// bands of output rows.
struct Prepared<'a> {
    device: &'a wgpu::Device,
    pipeline: Arc<Pipeline>,
    base: Params,
    out: OutputRows,
    input: wgpu::Buffer,
    table: wgpu::Buffer,
    missed: wgpu::Buffer,
}

impl<'a> Prepared<'a> {
    fn new(
        device: &'a Arc<wgpu::Device>,
        u: &WarpUniforms,
        source: &SourceRows,
        out: OutputRows,
        flags: u32,
    ) -> Result<Self, String> {
        if std::mem::size_of_val(source.pixels) as u64 > max_binding(device) {
            return Err("The warp's source exceeds the GPU's storage buffers.".to_string());
        }
        let (mut base, table_data) = params(u, source, out);
        base.flags |= flags;
        let init = |label, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        Ok(Self {
            device,
            pipeline: pipeline_for(device),
            base,
            out,
            input: init(
                "Warp source",
                bytemuck::cast_slice(source.pixels),
                wgpu::BufferUsages::STORAGE,
            ),
            table: init(
                "Warp lens table",
                bytemuck::cast_slice(&table_data),
                wgpu::BufferUsages::STORAGE,
            ),
            missed: init(
                "Warp missed",
                bytemuck::bytes_of(&0u32),
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            ),
        })
    }

    /// Output rows per band.
    fn band_rows(&self) -> u32 {
        (BAND_PIXELS / self.out.width.max(1)).clamp(1, self.out.rows.max(1))
    }

    /// The bands: first output row and rows.
    fn bands(&self) -> impl Iterator<Item = (u32, u32)> + use<'_> {
        let (band, rows) = (self.band_rows(), self.out.rows);
        (0..rows)
            .step_by(band as usize)
            .map(move |row0| (row0, band.min(rows - row0)))
    }

    /// Encodes output rows `row0..row0 + rows` into `dst` from its row
    /// `dst_row0`.
    fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        row0: u32,
        rows: u32,
        dst: &wgpu::Buffer,
        dst_row0: u32,
    ) {
        let device = self.device;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Warp params"),
            contents: bytemuck::bytes_of(&Params {
                band_row0: row0,
                band_rows: rows,
                dst_row0,
                ..self.base
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Geometry warp"),
            layout: &self.pipeline.layout,
            entries: &[
                entry(0, &uniform),
                entry(1, &self.input),
                entry(2, dst),
                entry(3, &self.table),
                entry(4, &self.missed),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.pipeline.warp);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(self.out.width.div_ceil(16), rows.div_ceil(16), 1);
    }

    /// Warps band by band into `result` (RGB rows of `out.width`), two
    /// bands in flight: one computes while the other is copied out.
    fn read_back(&self, queue: &wgpu::Queue, result: &mut [f32]) -> Result<(), String> {
        let device = self.device;
        let row_bytes = u64::from(self.out.width) * 12;
        let band_bytes = u64::from(self.band_rows()) * row_bytes;
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: band_bytes,
                usage,
                mapped_at_creation: false,
            })
        };
        let mut slots: Vec<Slot> = (0..2)
            .map(|_| Slot {
                output: buffer(
                    "Warped rows",
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                ),
                readback: buffer(
                    "Warped rows read back",
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                ),
                pending: None,
            })
            .collect();
        let row_floats = self.out.width as usize * 3;
        let mut collect = |slot: &mut Slot| -> Result<(), String> {
            let Some((row0, rows, mapped)) = slot.pending.take() else {
                return Ok(());
            };
            wait_for(device, &mapped)?.map_err(|e| format!("Warp readback failed: {e}"))?;
            let bytes = u64::from(rows) * row_bytes;
            {
                let view = slot.readback.slice(..bytes).get_mapped_range();
                let start = row0 as usize * row_floats;
                result[start..start + rows as usize * row_floats]
                    .copy_from_slice(bytemuck::cast_slice(&view));
            }
            slot.readback.unmap();
            Ok(())
        };
        for (band, (row0, rows)) in self.bands().enumerate() {
            let slot = &mut slots[band % 2];
            collect(slot)?;
            let bytes = u64::from(rows) * row_bytes;
            let mut encoder = device.create_command_encoder(&Default::default());
            self.encode(&mut encoder, row0, rows, &slot.output, 0);
            encoder.copy_buffer_to_buffer(&slot.output, 0, &slot.readback, 0, bytes);
            queue.submit(Some(encoder.finish()));
            let (sender, mapped) = std::sync::mpsc::channel();
            slot.readback
                .slice(..bytes)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            slot.pending = Some((row0, rows, mapped));
        }
        for slot in &mut slots {
            collect(slot)?;
        }
        Ok(())
    }

    /// Fails when a sample needed a pixel the source does not hold.
    fn check(&self, queue: &wgpu::Queue) -> Result<(), String> {
        let device = self.device;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Warp missed read back"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.missed, 0, &readback, 0, 4);
        queue.submit(Some(encoder.finish()));
        let missed: u32 = map(device, &readback, |bytes| {
            bytemuck::pod_read_unaligned(&bytes[..4])
        })?;
        if missed != 0 {
            return Err("The warp's source rows miss pixels its samples need.".to_string());
        }
        Ok(())
    }
}

/// A band's mapping in flight: its first row, rows, and the mapping's
/// result.
type Pending = (
    u32,
    u32,
    std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
);

/// One band's buffers: the warped rows and the copy read back.
struct Slot {
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    pending: Option<Pending>,
}
