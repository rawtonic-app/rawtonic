// SPDX-License-Identifier: AGPL-3.0-or-later
//! X-Trans demosaicing on the GPU (shaders/xtrans_demosaic.wgsl): the steps
//! of `demosaic::directional_tile` over bands of the padded mosaic, with the
//! CPU path as the fallback whenever no device is registered, a buffer
//! would exceed the device's limits, or anything fails.

use std::sync::{Arc, Mutex, PoisonError};

use rayon::prelude::*;
use wgpu::util::DeviceExt;

use crate::demosaic::{BORDER, Cancel, Cancelled, CfaPattern, DirectionalTables};

/// Output rows per band (plus `BORDER` rows of context above and below).
const BAND_ROWS: usize = 256;

struct Device {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

/// One demosaic at a time on the GPU; the band buffers are large.
static RUNNING: Mutex<Option<Pipelines>> = Mutex::new(None);

/// Makes the processing device available to the demosaic (called when the
/// app's GPU context is created).
pub fn register(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
    *DEVICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Device { device, queue });
}

struct Pipelines {
    device: Arc<wgpu::Device>,
    layout: wgpu::BindGroupLayout,
    green: wgpu::ComputePipeline,
    colour: wgpu::ComputePipeline,
    refine: wgpu::ComputePipeline,
    roughness: wgpu::ComputePipeline,
    select: wgpu::ComputePipeline,
    pad_layout: wgpu::BindGroupLayout,
    pad: wgpu::ComputePipeline,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PadParams {
    width: u32,
    height: u32,
    padded_width: u32,
    padded_height: u32,
    pattern_width: u32,
    pattern_height: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    padded_width: u32,
    band_top: u32,
    band_rows: u32,
    out_width: u32,
    out_rows: u32,
    pattern_width: u32,
    pattern_height: u32,
    pixels: u32,
}

impl Pipelines {
    fn new(device: &Arc<wgpu::Device>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("X-Trans demosaic"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/xtrans_demosaic.wgsl").into()),
        });
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("X-Trans demosaic"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, true),
                storage(6, false),
                storage(7, false),
                storage(8, false),
                storage(9, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("X-Trans demosaic"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pad_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("X-Trans demosaic padding"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/xtrans_pad.wgsl").into()),
        });
        let pad_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("X-Trans demosaic padding"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, false),
            ],
        });
        let pad_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("X-Trans demosaic padding"),
            bind_group_layouts: &[Some(&pad_layout)],
            immediate_size: 0,
        });
        let pad = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pad_pass"),
            layout: Some(&pad_pipeline_layout),
            module: &pad_module,
            entry_point: Some("pad_pass"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            device: device.clone(),
            pad_layout,
            pad,
            green: pipeline("green_pass"),
            colour: pipeline("colour_pass"),
            refine: pipeline("refine_pass"),
            roughness: pipeline("roughness_pass"),
            select: pipeline("select_pass"),
            layout,
        }
    }
}

/// Demosaics a non-Bayer mosaic on the GPU; `None` leaves it to the CPU.
/// `cancelled` is asked before each band.
pub fn xtrans(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    tables: &DirectionalTables,
    cancelled: Cancel,
) -> Option<Result<Vec<[f32; 3]>, Cancelled>> {
    if std::env::var_os("RAWTONIC_CPU_DEMOSAIC").is_some() || width < 64 || height < 64 {
        return None;
    }
    let (device, queue) = {
        let guard = DEVICE.lock().unwrap_or_else(PoisonError::into_inner);
        let d = guard.as_ref()?;
        (d.device.clone(), d.queue.clone())
    };
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if running
        .as_ref()
        .is_none_or(|p| !Arc::ptr_eq(&p.device, &device))
    {
        *running = Some(Pipelines::new(&device));
    }
    let pipelines = running.as_ref()?;
    match run(
        &device, &queue, pipelines, mosaic, width, height, cfa, tables, cancelled,
    ) {
        Ok(Some(rgb)) => Some(Ok(rgb)),
        Ok(None) => Some(Err(Cancelled)),
        Err(error) => {
            log::warn!("GPU demosaic unavailable, using the CPU: {error}");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &Pipelines,
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    tables: &DirectionalTables,
    cancelled: Cancel,
) -> Result<Option<Vec<[f32; 3]>>, String> {
    let padded_width = width + 2 * BORDER;
    let band_pixels = padded_width * (BAND_ROWS + 2 * BORDER);
    let limit = device.limits().max_storage_buffer_binding_size as usize;
    let padded_len = padded_width * (height + 2 * BORDER);
    if padded_len * 4 > limit || band_pixels * 12 * 4 > limit {
        return Err(format!("{width}x{height} exceeds the storage buffer limit"));
    }

    // Tables: pattern colours, green lines and colour taps per phase.
    let phases = tables.period_w * tables.period_h;
    let pattern: Vec<u32> = (0..phases)
        .map(|p| {
            cfa.color(
                (p / tables.period_w) as isize,
                (p % tables.period_w) as isize,
            ) as u32
        })
        .collect();
    let mut lines: Vec<[i32; 4]> = Vec::with_capacity(phases * 8);
    for phase in &tables.green {
        for line in phase {
            match line {
                Some(line) => {
                    let (s1, s2) = line.own.unwrap_or((0, 0));
                    lines.push([1, line.a as i32, line.b as i32, line.own.is_some() as i32]);
                    lines.push([s1 as i32, s2 as i32, 0, 0]);
                }
                None => {
                    lines.push([0; 4]);
                    lines.push([0; 4]);
                }
            }
        }
    }
    let phase_taps = crate::demosaic::phase_taps(tables);
    let mut tap_index: Vec<[u32; 2]> = Vec::new();
    let mut taps: Vec<[f32; 4]> = Vec::new();
    for by_direction in &phase_taps {
        for by_slot in by_direction {
            for slot in by_slot {
                tap_index.push([taps.len() as u32, slot.len() as u32]);
                taps.extend(
                    slot.iter()
                        .map(|&(oy, ox, w)| [oy as f32, ox as f32, w, 0.0]),
                );
            }
        }
    }
    if taps.is_empty() {
        taps.push([0.0; 4]);
    }

    let init = |label: &str, contents: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    };
    let read = wgpu::BufferUsages::STORAGE;
    // The raw mosaic goes up as it is; the GPU pads and encodes it.
    let raw_buffer = init("demosaic raw mosaic", bytemuck::cast_slice(mosaic), read);
    let mosaic_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("demosaic mosaic"),
        size: (padded_len * 4) as u64,
        usage: read,
        mapped_at_creation: false,
    });
    let pattern_buffer = init("demosaic pattern", bytemuck::cast_slice(&pattern), read);
    let lines_buffer = init("demosaic green lines", bytemuck::cast_slice(&lines), read);
    let index_buffer = init("demosaic tap index", bytemuck::cast_slice(&tap_index), read);
    let taps_buffer = init("demosaic taps", bytemuck::cast_slice(&taps), read);
    {
        let padded_height = height + 2 * BORDER;
        let params = PadParams {
            width: width as u32,
            height: height as u32,
            padded_width: padded_width as u32,
            padded_height: padded_height as u32,
            pattern_width: tables.period_w as u32,
            pattern_height: tables.period_h as u32,
            _pad: [0; 2],
        };
        let params_buffer = init(
            "demosaic pad params",
            bytemuck::bytes_of(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("X-Trans demosaic padding"),
            layout: &pipelines.pad_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: raw_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: pattern_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: mosaic_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipelines.pad);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                (padded_width as u32).div_ceil(16),
                (padded_height as u32).div_ceil(16),
                1,
            );
        }
        queue.submit(Some(encoder.finish()));
    }
    drop(raw_buffer);
    let scratch = |label: &str, floats: usize| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (floats * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    };
    let greens = scratch("demosaic greens", band_pixels * 4);
    let cand = scratch("demosaic candidates", band_pixels * 12);
    let rough = scratch("demosaic roughness", band_pixels * 4);
    let out_floats = BAND_ROWS * width * 3;
    let out_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("demosaic output"),
        size: (out_floats * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    // Two staging buffers: one band is read while the next renders.
    let staging: [wgpu::Buffer; 2] = std::array::from_fn(|_| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("demosaic readback"),
            size: (out_floats * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });

    let mut rgb = vec![[0.0f32; 3]; width * height];
    let bands = height.div_ceil(BAND_ROWS);
    // (staging buffer, first row, rows, submission)
    let mut pending: Option<(usize, usize, usize, wgpu::SubmissionIndex)> = None;
    let finish =
        |rgb: &mut Vec<[f32; 3]>,
         (index, first, rows, submission): (usize, usize, usize, wgpu::SubmissionIndex)| {
            let slice = staging[index].slice(..(rows * width * 3 * 4) as u64);
            let (sender, receiver) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
            // Only this band's work: the next band keeps the GPU busy meanwhile.
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            });
            receiver
                .recv()
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            {
                let data = slice.get_mapped_range();
                let floats: &[f32] = bytemuck::cast_slice(&data);
                rgb[first * width..(first + rows) * width]
                    .par_iter_mut()
                    .zip(floats.par_chunks_exact(3))
                    .for_each(|(dst, src)| *dst = [src[0], src[1], src[2]]);
            }
            staging[index].unmap();
            Ok::<(), String>(())
        };

    for band in 0..bands {
        // Work already submitted finishes on its own; its buffers are kept
        // alive until then.
        if cancelled() {
            return Ok(None);
        }
        let first = band * BAND_ROWS;
        let rows = BAND_ROWS.min(height - first);
        let band_rows = rows + 2 * BORDER;
        let params = Params {
            padded_width: padded_width as u32,
            band_top: first as u32,
            band_rows: band_rows as u32,
            out_width: width as u32,
            out_rows: rows as u32,
            pattern_width: tables.period_w as u32,
            pattern_height: tables.period_h as u32,
            pixels: (padded_width * band_rows) as u32,
        };
        let params_buffer = init(
            "demosaic params",
            bytemuck::bytes_of(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("X-Trans demosaic"),
            layout: &pipelines.layout,
            entries: &[
                params_buffer.as_entire_binding(),
                mosaic_buffer.as_entire_binding(),
                pattern_buffer.as_entire_binding(),
                lines_buffer.as_entire_binding(),
                index_buffer.as_entire_binding(),
                taps_buffer.as_entire_binding(),
                greens.as_entire_binding(),
                cand.as_entire_binding(),
                rough.as_entire_binding(),
                out_buffer.as_entire_binding(),
            ]
            .into_iter()
            .enumerate()
            .map(|(binding, resource)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource,
            })
            .collect::<Vec<_>>(),
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_bind_group(0, &bind_group, &[]);
            let groups = (
                (padded_width as u32).div_ceil(16),
                (band_rows as u32).div_ceil(16),
            );
            for pipeline in [
                &pipelines.green,
                &pipelines.colour,
                &pipelines.refine,
                &pipelines.colour,
                &pipelines.roughness,
            ] {
                pass.set_pipeline(pipeline);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            pass.set_pipeline(&pipelines.select);
            pass.dispatch_workgroups((width as u32).div_ceil(16), (rows as u32).div_ceil(16), 1);
        }
        let index = band % 2;
        encoder.copy_buffer_to_buffer(
            &out_buffer,
            0,
            &staging[index],
            0,
            (rows * width * 3 * 4) as u64,
        );
        let submission = queue.submit(Some(encoder.finish()));
        if let Some(previous) = pending.replace((index, first, rows, submission)) {
            finish(&mut rgb, previous)?;
        }
    }
    if let Some(previous) = pending.take() {
        finish(&mut rgb, previous)?;
    }
    Ok(Some(rgb))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::demosaic::{CfaPattern, DirectionalTables};

    fn xtrans() -> CfaPattern {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let colors = rows
            .iter()
            .flat_map(|row| {
                row.chars().map(|ch| match ch {
                    'R' => 0u8,
                    'G' => 1,
                    _ => 2,
                })
            })
            .collect();
        CfaPattern::new(6, 6, colors).unwrap()
    }

    /// The GPU kernel reproduces the CPU tiles (edges, texture, noise, the
    /// image border and several bands).
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_matches_the_cpu_kernel() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .unwrap();
        super::register(Arc::new(device), Arc::new(queue));

        let cfa = xtrans();
        let (width, height) = (1203, 701);
        let mut state = 7u32;
        let mut noise = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.02
        };
        let mut mosaic = vec![0.0f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let edge = if (x as f32) * 0.6 + (y as f32) > 600.0 {
                    0.6
                } else {
                    0.1
                };
                let rgb = [
                    edge * (0.5 + 0.5 * ((x as f32) * 0.05).sin()),
                    0.3 + 0.2 * ((y as f32) * 0.03).cos(),
                    0.05 + 0.4 * ((x + y) % 23) as f32 / 23.0,
                ];
                mosaic[y * width + x] = (rgb[cfa.color(y as isize, x as isize)] + noise()).max(0.0);
            }
        }
        let tables = DirectionalTables::new(&cfa);
        let cpu = crate::demosaic::xtrans_cpu(&mosaic, width, height, &cfa, &tables);
        let gpu = super::xtrans(&mosaic, width, height, &cfa, &tables, &|| false)
            .expect("GPU demosaic")
            .unwrap();
        // A cancel stops it before the next band.
        assert!(matches!(
            super::xtrans(&mosaic, width, height, &cfa, &tables, &|| true),
            Some(Err(crate::demosaic::Cancelled))
        ));
        let (mut max, mut sum) = (0.0f32, 0.0f64);
        for (a, b) in cpu.iter().flatten().zip(gpu.iter().flatten()) {
            let d = (a - b).abs();
            max = max.max(d);
            sum += d as f64;
        }
        let mean = sum / (cpu.len() * 3) as f64;
        let big: Vec<(usize, usize, f32)> = cpu
            .iter()
            .zip(&gpu)
            .enumerate()
            .filter_map(|(i, (a, b))| {
                let d = (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0f32, f32::max);
                (d > 1e-4).then_some((i / width, i % width, d))
            })
            .collect();
        println!("GPU vs CPU: max {max:e}, mean {mean:e}");
        // GPU rounding can flip the direction choice where two directions
        // are equally smooth (2 of 843,000 pixels here); everything else
        // agrees to float precision.
        assert!(
            big.len() * 10_000 < cpu.len(),
            "{} pixels differ",
            big.len()
        );
        assert!(mean < 1e-6, "mean difference {mean}");
    }
}
