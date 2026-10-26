// SPDX-License-Identifier: AGPL-3.0-or-later
//! Version 5: version 4's pass on the GPU (shaders/edge_fringe.wgsl). The
//! same stages and arithmetic run per pixel over square tiles, each tile
//! uploaded with the pixels its stages read around it ([`super::reach`])
//! and only its own pixels read back, so the result does not depend on the
//! tile layout. Version 4's pass was about a quarter of a batch export's
//! CPU time (notes: benchmark-results/bx3-export-throughput-2026-10-25).
//!
//! There is no CPU fallback: without a registered device, or when the GPU
//! fails, the pass returns an error and the render fails.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use wgpu::util::DeviceExt;

use super::{Plan, axial};

struct Device {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

static DEVICE: Mutex<Option<Device>> = Mutex::new(None);
static PIPELINES: Mutex<Option<Arc<Pipelines>>> = Mutex::new(None);

/// Makes the processing device available to the pass (called when the
/// app's or a headless GPU context is created).
pub fn register(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
    *DEVICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Device { device, queue });
}

/// Side of a tile's own pixels. With its halo (about 30 pixels at full
/// resolution) each of the seven buffers holds 16 bytes per pixel, about
/// 130 MB in all; tiles also keep each submission short, so display frames
/// queued meanwhile wait for one tile, not the whole photo.
const TILE: usize = 1024;
/// The widest window radius of the luminance and colour-line stages (their
/// models are kept for that many virtual rows beyond the tile).
const MAX_RADIUS: usize = 3;

struct Pipelines {
    device: Arc<wgpu::Device>,
    layout: wgpu::BindGroupLayout,
    unpack: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
    pack_half: wgpu::ComputePipeline,
    axial_across: wgpu::ComputePipeline,
    axial_down: wgpu::ComputePipeline,
    axial_moves: wgpu::ComputePipeline,
    axial_apply: wgpu::ComputePipeline,
    suppress_edges: wgpu::ComputePipeline,
    suppress_models: wgpu::ComputePipeline,
    suppress_apply: wgpu::ComputePipeline,
    line_weights: wgpu::ComputePipeline,
    line_maps: wgpu::ComputePipeline,
    line_apply: wgpu::ComputePipeline,
}

impl Pipelines {
    fn new(device: &Arc<wgpu::Device>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Edge fringes"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/edge_fringe.wgsl").into()),
        });
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        // The tile buffers, then the band's upload (read only).
        entries.extend(
            (1..=BUFFERS as u32 + 1).map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: binding > BUFFERS as u32,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }),
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Edge fringes"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Edge fringes"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // Every pass writes the workgroup values it reads first (the row
        // sums of the rows its windows span, the group flags), so naga's
        // zero fill of them (one invocation writes all 1008 row sums while
        // the group waits) is left out: 16 % of the pass's GPU time.
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    zero_initialize_workgroup_memory: false,
                    ..Default::default()
                },
                cache: None,
            })
        };
        Self {
            device: device.clone(),
            unpack: pipeline("unpack"),
            pack: pipeline("pack"),
            pack_half: pipeline("pack_half"),
            axial_across: pipeline("axial_across"),
            axial_down: pipeline("axial_down"),
            axial_moves: pipeline("axial_moves"),
            axial_apply: pipeline("axial_apply"),
            suppress_edges: pipeline("suppress_edges"),
            suppress_models: pipeline("suppress_models"),
            suppress_apply: pipeline("suppress_apply"),
            line_weights: pipeline("line_weights"),
            line_maps: pipeline("line_maps"),
            line_apply: pipeline("line_apply"),
            layout,
        }
    }
}

/// Storage buffers of a tile: the stage's input and output (which swap from
/// stage to stage) and five for intermediate planes.
const BUFFERS: usize = 7;

/// Uniforms of shaders/edge_fringe.wgsl (`Params`).
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    rows: u32,
    radius: u32,
    search: u32,
    direction_radius: u32,
    side_radius: u32,
    profile_radius: u32,
    blocks_at: u32,
    out_x: u32,
    out_y: u32,
    out_width: u32,
    out_rows: u32,
    band_x: u32,
    band_width: u32,
    blocks_width: u32,
    half_stride: u32,
    noise: f32,
    push: f32,
    inv_area: f32,
    _pad1: f32,
    direction: [[f32; 4]; 3],
    side: [[f32; 4]; 3],
    profile: [[f32; 4]; 3],
}

/// Gaussian weights (at most 12) packed for the uniform block.
fn packed(weights: &[f32]) -> Result<[[f32; 4]; 3], String> {
    if weights.len() > 12 {
        return Err(format!(
            "{} Gaussian taps exceed the shader's 12",
            weights.len()
        ));
    }
    let mut out = [[0.0f32; 4]; 3];
    for (k, &w) in weights.iter().enumerate() {
        out[k / 4][k % 4] = w;
    }
    Ok(out)
}

fn registered() -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), String> {
    let guard = DEVICE.lock().unwrap_or_else(PoisonError::into_inner);
    let d = guard.as_ref().ok_or_else(|| {
        "The edge-fringe correction of this edit needs the graphics card, which is not available."
            .to_string()
    })?;
    Ok((d.device.clone(), d.queue.clone()))
}

/// The pipelines for `device`, made on first use.
fn pipelines_for(device: &Arc<wgpu::Device>) -> Arc<Pipelines> {
    let mut pipelines = PIPELINES.lock().unwrap_or_else(PoisonError::into_inner);
    if pipelines
        .as_ref()
        .is_none_or(|p| !Arc::ptr_eq(&p.device, device))
    {
        *pipelines = Some(Arc::new(Pipelines::new(device)));
    }
    pipelines.clone().expect("pipelines were just made")
}

/// Polls the device until `done` receives, sleeping between polls: a
/// blocking poll holds the device's fence lock until the GPU is done and
/// stalls other threads' submissions (display frames during an export).
fn wait_for<T>(device: &wgpu::Device, done: &std::sync::mpsc::Receiver<T>) -> Result<T, String> {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("fringe_gpu_wait");
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
            Err(error) => return Err(format!("The edge-fringe pass did not finish: {error}")),
        }
    }
}

/// Runs `plan` (a version 5 plan) on `w × h` pixels of `stride` floats (RGB
/// first) in place, as [`super::apply`] runs version 4's on the CPU.
pub(super) fn apply(
    data: &mut [f32],
    w: usize,
    h: usize,
    stride: usize,
    plan: Plan,
) -> Result<(), String> {
    apply_in_tiles(data, w, h, stride, plan, TILE)
}

/// [`apply`] in tiles of `tile × tile` pixels (the tile layout's tests).
pub(super) fn apply_in_tiles(
    data: &mut [f32],
    w: usize,
    h: usize,
    stride: usize,
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Ok(());
    }
    let (device, queue) = registered()?;
    apply_on_device(&device, &queue, data, (w, h, stride), plan, tile)
}

/// [`apply_in_tiles`] on `device` instead of the registered one (tests of
/// devices with other limits).
pub(super) fn apply_on_device(
    device: &Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    data: &mut [f32],
    (w, h, stride): (usize, usize, usize),
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    run_scoped(
        device,
        queue,
        Target::Data(data),
        (w, h, stride),
        plan,
        tile,
    )
}

/// [`apply`] on `w × h` RGB pixels with the result written into `texture`
/// instead of `data` (see [`Target::Texture`]).
pub(super) fn apply_into_texture(
    device: &Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    data: &[f32],
    (w, h): (usize, usize),
    plan: Plan,
    window: [usize; 4],
    texture: &wgpu::Texture,
) -> Result<(), String> {
    run_scoped(
        device,
        queue,
        Target::Texture {
            data,
            window,
            texture,
        },
        (w, h, 3),
        plan,
        TILE,
    )
}

/// Where a run's results go.
enum Target<'a> {
    /// Back into the pixels, in place.
    Data(&'a mut [f32]),
    /// Into an `Rgba16Float` texture (with `COPY_DST`) the size of
    /// `window` (x, y, width, height in the image): its pixels at the
    /// texture's origin, as f16 RGBA with alpha 1 exactly as `f16::from_f32`
    /// converts them on the CPU, so the texture holds what uploading the
    /// pass's result cropped to `window` would. `data` is not changed, and
    /// nothing is read back or waited for beyond the tile before.
    Texture {
        data: &'a [f32],
        window: [usize; 4],
        texture: &'a wgpu::Texture,
    },
}

impl Target<'_> {
    fn data(&self) -> &[f32] {
        match self {
            Target::Data(data) => data,
            Target::Texture { data, .. } => data,
        }
    }
}

fn run_scoped(
    device: &Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    target: Target,
    (w, h, stride): (usize, usize, usize),
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Ok(());
    }
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipelines = pipelines_for(device);
    let result = run(
        device,
        queue,
        &pipelines,
        target,
        (w, h, stride),
        plan,
        tile.max(1),
    );
    let validation = pollster::block_on(validation.pop());
    let memory = pollster::block_on(memory.pop());
    if let Some(error) = memory.or(validation) {
        return Err(format!("The edge-fringe pass failed on the GPU: {error}"));
    }
    result
}

fn run(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &Pipelines,
    mut target: Target,
    (w, h, stride): (usize, usize, usize),
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    let r = plan.radius;
    if r > MAX_RADIUS {
        return Err(format!(
            "Window radius {r} exceeds the GPU pass's {MAX_RADIUS}"
        ));
    }
    // Version 4 leaves images under 3 × 3 pixels to the other stages.
    let axial = plan.axial.filter(|_| w >= 3 && h >= 3);
    let distances = axial.map(|a| axial::distances(a.scale()));
    let halo = super::reach(plan);
    let (tile_w, tile_h) = (tile.min(w), tile.min(h));
    let (cap_w, cap_h) = ((tile_w + 2 * halo).min(w), (tile_h + 2 * halo).min(h));
    // Each buffer holds a tile and its virtual rows; the last also the
    // records of its 8 × 8 blocks after them.
    let planes = cap_w * (cap_h + 2 * MAX_RADIUS);
    let blocks_width = cap_w.div_ceil(8);
    let blocks = blocks_width * (cap_h + 2 * MAX_RADIUS).div_ceil(8);
    let buffer_bytes = ((planes + blocks) * 16) as u64;
    let limits = device.limits();
    let max_binding = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size);
    if buffer_bytes > max_binding {
        return Err(format!(
            "Tiles of {tile} px exceed the GPU's buffer size for the edge-fringe pass"
        ));
    }
    let buffers: Vec<wgpu::Buffer> = (0..BUFFERS)
        .map(|k| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Edge fringe tile"),
                size: buffer_bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | if k < 2 {
                        wgpu::BufferUsages::COPY_SRC
                    } else {
                        wgpu::BufferUsages::empty()
                    },
                mapped_at_creation: false,
            })
        })
        .collect();
    let readback_bytes = (tile_w * tile_h * 12) as u64;
    let readbacks: [wgpu::Buffer; 2] = std::array::from_fn(|_| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Edge fringe readback"),
            size: readback_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });
    let mut base = Params {
        blocks_at: planes as u32,
        blocks_width: blocks_width as u32,
        radius: r as u32,
        inv_area: 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32,
        ..Default::default()
    };
    if let (Some(axial), Some(d)) = (axial, &distances) {
        base.noise = axial.noise();
        base.search = d.search as u32;
        base.push = d.push;
        base.direction_radius = (d.direction.len() / 2) as u32;
        base.side_radius = (d.side.len() / 2) as u32;
        base.profile_radius = (d.profile.len() / 2) as u32;
        base.direction = packed(&d.direction)?;
        base.side = packed(&d.side)?;
        base.profile = packed(&d.profile)?;
    }
    // The stages' passes in order: (pipeline, workgroup side, rows beyond
    // the tile it covers on each side, whether it writes the next stage's
    // input).
    let mut passes: Vec<(&wgpu::ComputePipeline, u32, usize, bool)> = Vec::new();
    if axial.is_some() {
        passes.extend([
            (&pipelines.axial_across, 16, 0, false),
            (&pipelines.axial_down, 8, 0, false),
            (&pipelines.axial_moves, 16, 0, false),
            (&pipelines.axial_apply, 16, 0, true),
        ]);
    }
    passes.extend([
        (&pipelines.suppress_edges, 8, r, false),
        (&pipelines.suppress_models, 8, r, false),
        (&pipelines.suppress_apply, 16, 0, true),
    ]);
    if plan.colour_lines {
        passes.extend([
            (&pipelines.line_weights, 8, r, false),
            (&pipelines.line_maps, 8, r, false),
            (&pipelines.line_apply, 16, 0, true),
        ]);
    }

    // Each band of tiles goes up once, packed RGB, with the rows its tiles
    // read above and below it. Tiles write their results into `data` as
    // they finish, so the rows above the band (the band before's) come from
    // `carry`, the original rows [y0 - halo, y0). A band wider than one
    // storage binding goes up in column groups of whole tiles, each with
    // the columns its tiles read beside it, all before the band's first
    // tile so that every group reads original pixels.
    let tiles_across = w.div_ceil(tile_w);
    let group_columns = |tiles: usize| (tiles * tile_w + 2 * halo).min(w);
    let per_group = (1..=tiles_across)
        .rev()
        .find(|&tiles| (cap_h * group_columns(tiles) * 12) as u64 <= max_binding)
        .ok_or_else(|| {
            format!("Tiles of {tile} px exceed the GPU's buffer size for the edge-fringe pass")
        })?;
    // (first tile column, end of its tile columns, uploaded columns c0..c1)
    let groups: Vec<(usize, usize, usize, usize)> = (0..tiles_across)
        .step_by(per_group)
        .map(|t| {
            let (gx0, gx1) = (t * tile_w, ((t + per_group) * tile_w).min(w));
            (gx0, gx1, gx0.saturating_sub(halo), (gx1 + halo).min(w))
        })
        .collect();
    let band_buffers: Vec<wgpu::Buffer> = groups
        .iter()
        .map(|&(_, _, c0, c1)| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Edge fringe band"),
                size: (cap_h * (c1 - c0) * 12) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
        .collect();
    // Columns c0..c1 of `rows` of `source` (rows of `w` pixels of
    // `source_stride` floats), packed RGB.
    let packed = |source: &[f32],
                  source_stride: usize,
                  rows: std::ops::Range<usize>,
                  (c0, c1): (usize, usize)|
     -> Vec<f32> {
        let row_len = w * source_stride;
        let mut out = vec![0.0f32; rows.len() * (c1 - c0) * 3];
        out.par_chunks_mut((c1 - c0) * 3)
            .zip(source[rows.start * row_len..rows.end * row_len].par_chunks(row_len))
            .for_each(|(out, row)| {
                let row = &row[c0 * source_stride..c1 * source_stride];
                for (o, px) in out
                    .as_chunks_mut::<3>()
                    .0
                    .iter_mut()
                    .zip(row.chunks_exact(source_stride))
                {
                    o.copy_from_slice(&px[..3]);
                }
            });
        out
    };
    // Texels go to the texture's window only (x0, y0, x1, y1 in the image);
    // the pixels stay as they are, so bands read them all, no carry.
    let window = match &target {
        Target::Data(_) => None,
        Target::Texture { window, .. } => {
            let [x, y, width, height] = *window;
            Some([x, y, x + width, y + height])
        }
    };
    let mut carry: Vec<f32> = Vec::new();
    // (readback, tile's own rectangle)
    let mut pending: Option<(usize, [usize; 4])> = None;
    // A texture's tile before: done before the next is submitted, so a
    // worker keeps two tiles in the queue at most, as with readbacks.
    let mut in_flight: Option<std::sync::mpsc::Receiver<()>> = None;
    let mut count = 0;
    for y0 in (0..h).step_by(tile_h) {
        let y1 = (y0 + tile_h).min(h);
        let (top, bottom) = (y0.saturating_sub(halo), (y1 + halo).min(h));
        if let Some([_, wy0, _, wy1]) = window {
            if y1 <= wy0 || y0 >= wy1 {
                continue;
            }
            let data = target.data();
            for (band_buffer, &(_, _, c0, c1)) in band_buffers.iter().zip(&groups) {
                let rows = if c0 == 0 && c1 == w {
                    std::borrow::Cow::Borrowed(&data[top * w * 3..bottom * w * 3])
                } else {
                    std::borrow::Cow::Owned(packed(data, 3, top..bottom, (c0, c1)))
                };
                queue.write_buffer(band_buffer, 0, bytemuck::cast_slice(&rows));
            }
        } else {
            let data = target.data();
            // The next band's carry, while rows y0.. are still original.
            let next_top = y1.saturating_sub(halo);
            let mut next_carry = Vec::with_capacity((y1 - next_top) * w * 3);
            if next_top < y0 {
                next_carry.extend_from_slice(&carry[(next_top - top) * w * 3..]);
            }
            if stride == 3 {
                next_carry.extend_from_slice(&data[next_top.max(y0) * w * 3..y1 * w * 3]);
            } else {
                next_carry.extend(packed(data, stride, next_top.max(y0)..y1, (0, w)));
            }
            for (band_buffer, &(_, _, c0, c1)) in band_buffers.iter().zip(&groups) {
                let all_columns = c0 == 0 && c1 == w;
                if !carry.is_empty() {
                    if all_columns {
                        queue.write_buffer(band_buffer, 0, bytemuck::cast_slice(&carry));
                    } else {
                        let rows = packed(&carry, 3, 0..y0 - top, (c0, c1));
                        queue.write_buffer(band_buffer, 0, bytemuck::cast_slice(&rows));
                    }
                }
                let below = ((y0 - top) * (c1 - c0) * 12) as u64;
                if all_columns && stride == 3 {
                    let rows = &data[y0 * w * 3..bottom * w * 3];
                    queue.write_buffer(band_buffer, below, bytemuck::cast_slice(rows));
                } else {
                    let rows = packed(data, stride, y0..bottom, (c0, c1));
                    queue.write_buffer(band_buffer, below, bytemuck::cast_slice(&rows));
                }
            }
            carry = next_carry;
        }
        let tiles = groups
            .iter()
            .zip(&band_buffers)
            .flat_map(|(&(gx0, gx1, c0, c1), band)| {
                (gx0..gx1).step_by(tile_w).map(move |x0| (x0, c0, c1, band))
            });
        for (x0, c0, c1, band_buffer) in tiles {
            let x1 = (x0 + tile_w).min(w);
            // The pixels of the tile that are kept: its own, within the
            // texture's window.
            let [ox0, oy0, ox1, oy1] = match window {
                None => [x0, y0, x1, y1],
                Some([wx0, wy0, wx1, wy1]) => [x0.max(wx0), y0.max(wy0), x1.min(wx1), y1.min(wy1)],
            };
            if ox0 >= ox1 || oy0 >= oy1 {
                continue;
            }
            let (left, right) = (x0.saturating_sub(halo), (x1 + halo).min(w));
            let (in_w, in_h) = (right - left, bottom - top);
            // Rows of texels padded to the copy's row alignment.
            let half_stride =
                ((ox1 - ox0) * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize) / 4;
            let params = Params {
                width: in_w as u32,
                rows: in_h as u32,
                band_x: (left - c0) as u32,
                band_width: (c1 - c0) as u32,
                out_x: (ox0 - left) as u32,
                out_y: (oy0 - top) as u32,
                out_width: (ox1 - ox0) as u32,
                out_rows: (oy1 - oy0) as u32,
                half_stride: half_stride as u32,
                ..base
            };
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Edge fringe params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = |input: usize| {
                let order = [input, 1 - input, 2, 3, 4, 5, 6];
                let mut entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: BUFFERS as u32 + 1,
                        resource: band_buffer.as_entire_binding(),
                    },
                ];
                entries.extend(
                    order
                        .iter()
                        .enumerate()
                        .map(|(k, &b)| wgpu::BindGroupEntry {
                            binding: k as u32 + 1,
                            resource: buffers[b].as_entire_binding(),
                        }),
                );
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Edge fringe tile"),
                    layout: &pipelines.layout,
                    entries: &entries,
                })
            };
            let groups = [bind_group(0), bind_group(1)];
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut input = 0;
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                let dispatch =
                    |pass: &mut wgpu::ComputePass, group: u32, width: usize, rows: usize| {
                        pass.dispatch_workgroups(
                            (width as u32).div_ceil(group),
                            (rows as u32).div_ceil(group),
                            1,
                        );
                    };
                pass.set_pipeline(&pipelines.unpack);
                pass.set_bind_group(0, &groups[0], &[]);
                dispatch(&mut pass, 16, in_w, in_h);
                for &(pipeline, group, beyond, writes) in &passes {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &groups[input], &[]);
                    dispatch(&mut pass, group, in_w, in_h + 2 * beyond);
                    if writes {
                        input = 1 - input;
                    }
                }
                // The tile's own pixels, packed, into the other buffer.
                let pack = match window {
                    None => &pipelines.pack,
                    Some(_) => &pipelines.pack_half,
                };
                pass.set_pipeline(pack);
                pass.set_bind_group(0, &groups[input], &[]);
                dispatch(&mut pass, 16, ox1 - ox0, oy1 - oy0);
            }
            if let Target::Texture {
                texture,
                window: [wx, wy, _, _],
                ..
            } = &target
            {
                encoder.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffers[1 - input],
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(half_stride as u32 * 4),
                            rows_per_image: Some((oy1 - oy0) as u32),
                        },
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: (ox0 - wx) as u32,
                            y: (oy0 - wy) as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: (ox1 - ox0) as u32,
                        height: (oy1 - oy0) as u32,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit(Some(encoder.finish()));
                let (sender, done) = std::sync::mpsc::channel();
                queue.on_submitted_work_done(move || {
                    let _ = sender.send(());
                });
                if let Some(previous) = in_flight.replace(done) {
                    wait_for(device, &previous)?;
                }
                continue;
            }
            let slot = count % 2;
            count += 1;
            encoder.copy_buffer_to_buffer(
                &buffers[1 - input],
                0,
                &readbacks[slot],
                0,
                ((x1 - x0) * (y1 - y0) * 12) as u64,
            );
            queue.submit(Some(encoder.finish()));
            if let Some(previous) = pending.replace((slot, [x0, y0, x1, y1]))
                && let Target::Data(data) = &mut target
            {
                read_back(device, &readbacks, previous, data, w, stride)?;
            }
        }
    }
    if let Some(previous) = pending.take()
        && let Target::Data(data) = &mut target
    {
        read_back(device, &readbacks, previous, data, w, stride)?;
    }
    Ok(())
}

/// Copies a tile's own pixels from readback buffer `slot` into `data` once
/// the GPU has written them.
fn read_back(
    device: &wgpu::Device,
    readbacks: &[wgpu::Buffer; 2],
    (slot, [x0, y0, x1, y1]): (usize, [usize; 4]),
    data: &mut [f32],
    w: usize,
    stride: usize,
) -> Result<(), String> {
    let tile_w = x1 - x0;
    let slice = readbacks[slot].slice(..(tile_w * (y1 - y0) * 12) as u64);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    wait_for(device, &receiver)?.map_err(|e| format!("Edge fringe readback failed: {e}"))?;
    {
        let mapped = slice.get_mapped_range();
        let pixels: &[f32] = bytemuck::cast_slice(&mapped);
        data[y0 * w * stride..y1 * w * stride]
            .par_chunks_mut(w * stride)
            .zip(pixels.par_chunks(tile_w * 3))
            .for_each(|(row, tile_row)| {
                let row = &mut row[x0 * stride..x1 * stride];
                if stride == 3 {
                    row.copy_from_slice(tile_row);
                } else {
                    for (px, t) in row
                        .chunks_exact_mut(stride)
                        .zip(tile_row.as_chunks::<3>().0)
                    {
                        px[..3].copy_from_slice(t);
                    }
                }
            });
    }
    readbacks[slot].unmap();
    Ok(())
}
