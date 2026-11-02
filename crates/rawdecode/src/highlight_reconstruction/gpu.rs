// SPDX-License-Identifier: AGPL-3.0-or-later
//! `reconstruct_clipped_mosaic` (`HighlightRebuild::Ratios`) and the clip
//! levels' maxima and onset histograms on the GPU (shaders/highlights.wgsl).
//!
//! The per-sample steps run as compute passes; the grid steps between them
//! (fill and relax of a few thousand cells) run on the CPU with the same
//! functions as the CPU path. Not wired into the develop yet (task GDB:
//! the GPU develop's cost measurement); the CPU path is the product.

use std::collections::HashMap;
use std::sync::Arc;

use rawler::CFA;
use wgpu::util::DeviceExt;

use super::{
    Area, CELL, ClipLevels, MIN_CELL_SAMPLES, ONSET_BINS, ONSET_RANGE, Pattern, Reconstruction,
    SHARE_BLOCK, fill_grid, relax_grid,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    aw: u32,
    ah: u32,
    words: u32,
    grid_cols: u32,
    grid_rows: u32,
    share_cols: u32,
    share_rows: u32,
    ratios: u32,
    thresholds: [f32; 4],
    wb: [f32; 4],
    clip_wb: [f32; 4],
    misc: [f32; 4],
    ratio_measured: [u32; 4],
}

const ENTRIES: [&str; 10] = [
    "hl_max",
    "hl_onset",
    "hl_mask",
    "hl_near_h",
    "hl_near_v",
    "hl_cand",
    "hl_blocks",
    "hl_share",
    "hl_level",
    "hl_apply",
];
const GRID_STRIDE: usize = 8;
const C_CLIPPED: usize = 0;
const C_RAISED: usize = 1;
const C_MAX: usize = 4;
const C_HIST: usize = 8;
const COUNTERS: usize = C_HIST + 3 * ONSET_BINS;
/// Larger than any clip level in data units, for colours that do not clip.
const NO_CLIP: f32 = 3.0e38;

/// GPU time per pass (ms), when the device has timestamp queries.
#[derive(Debug, Default, Clone)]
pub struct PassTimes {
    pub ms: Vec<(&'static str, f64)>,
    /// Per submission: first pass start to last pass end (ms).
    pub spans: Vec<f64>,
}

impl PassTimes {
    pub fn total(&self) -> f64 {
        self.ms.iter().map(|(_, v)| v).sum()
    }
}

pub struct HighlightGpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    layout: wgpu::BindGroupLayout,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    timestamps: bool,
    /// The mask buffers of the largest mosaic so far, reused: a new buffer
    /// is zero-filled on the GPU at its first use.
    scratch: std::sync::Mutex<Option<Scratch>>,
    /// Held for a whole reconstruction (several submissions on `scratch`).
    running: std::sync::Mutex<()>,
}

struct Scratch {
    bytes: u64,
    mask: wgpu::Buffer,
    hnear: wgpu::Buffer,
    near: wgpu::Buffer,
}

/// One submission's passes, with optional timestamps around each.
struct Passes<'a> {
    gpu: &'a HighlightGpu,
    encoder: wgpu::CommandEncoder,
    query: Option<wgpu::QuerySet>,
    names: Vec<&'static str>,
}

impl<'a> Passes<'a> {
    fn new(gpu: &'a HighlightGpu) -> Self {
        let query = gpu.timestamps.then(|| {
            gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("highlights"),
                ty: wgpu::QueryType::Timestamp,
                count: 32,
            })
        });
        Self {
            gpu,
            encoder: gpu.device.create_command_encoder(&Default::default()),
            query,
            names: Vec::new(),
        }
    }

    fn run(&mut self, name: &'static str, bind: &wgpu::BindGroup, groups: (u32, u32)) {
        let index = self.names.len() as u32;
        self.names.push(name);
        let writes = self
            .query
            .as_ref()
            .map(|q| wgpu::ComputePassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(2 * index),
                end_of_pass_write_index: Some(2 * index + 1),
            });
        let mut pass = self
            .encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(name),
                timestamp_writes: writes,
            });
        pass.set_pipeline(&self.gpu.pipelines[name]);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(groups.0.max(1), groups.1.max(1), 1);
    }

    /// Submits, copying `reads` (buffer, bytes) back, and returns them.
    fn finish(
        mut self,
        reads: &[(&wgpu::Buffer, u64)],
        times: Option<&mut PassTimes>,
    ) -> Vec<Vec<u8>> {
        let device = &self.gpu.device;
        let mut staging = Vec::new();
        for (buffer, size) in reads {
            let s = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: *size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.encoder.copy_buffer_to_buffer(buffer, 0, &s, 0, *size);
            staging.push((s, *size));
        }
        let n = self.names.len() as u64;
        let ts = self.query.as_ref().filter(|_| n > 0).map(|q| {
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16 * n,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            self.encoder
                .resolve_query_set(q, 0..2 * n as u32, &resolve, 0);
            let s = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16 * n,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.encoder
                .copy_buffer_to_buffer(&resolve, 0, &s, 0, 16 * n);
            s
        });
        self.gpu.queue.submit(Some(self.encoder.finish()));
        let map = |s: &wgpu::Buffer| -> Vec<u8> {
            let slice = s.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let v = slice.get_mapped_range().to_vec();
            s.unmap();
            v
        };
        let out = staging.iter().map(|(s, _)| map(s)).collect();
        if let (Some(ts), Some(times)) = (ts, times) {
            let raw = map(&ts);
            let ticks: &[u64] = bytemuck::cast_slice(&raw);
            let period = self.gpu.queue.get_timestamp_period() as f64;
            for (i, name) in self.names.iter().enumerate() {
                let d = ticks[2 * i + 1].saturating_sub(ticks[2 * i]) as f64 * period / 1e6;
                times.ms.push((name, d));
            }
            // First start to last end of the submission (passes back to back).
            let first = ticks
                .iter()
                .step_by(2)
                .copied()
                .filter(|t| *t > 0)
                .min()
                .unwrap_or(0);
            let last = ticks.iter().skip(1).step_by(2).copied().max().unwrap_or(0);
            times
                .spans
                .push(last.saturating_sub(first) as f64 * period / 1e6);
        }
        out
    }
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytemuck::cast_slice(bytes).to_vec()
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytemuck::cast_slice(bytes).to_vec()
}

impl HighlightGpu {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("highlights"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/highlights.wgsl").into()),
        });
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let write = wgpu::BufferBindingType::Storage { read_only: false };
        let entries = [
            entry(0, wgpu::BufferBindingType::Uniform),
            entry(1, read),
            entry(2, write),
            entry(3, write),
            entry(4, write),
            entry(5, write),
            entry(6, write),
            entry(7, write),
            entry(8, read),
            entry(9, write),
            entry(10, read),
            entry(11, write),
        ];
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("highlights"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("highlights"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = ENTRIES
            .iter()
            .map(|name| {
                (
                    *name,
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(name),
                        layout: Some(&pipeline_layout),
                        module: &module,
                        entry_point: Some(name),
                        compilation_options: Default::default(),
                        cache: None,
                    }),
                )
            })
            .collect();
        let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        Self {
            device,
            queue,
            layout,
            pipelines,
            timestamps,
            scratch: std::sync::Mutex::new(None),
            running: std::sync::Mutex::new(()),
        }
    }

    fn storage(&self, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes.max(16),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// `detect_clip_levels` with the maxima and onset histograms on the GPU.
    /// `samples` are the raw sample values. A colour missed by the onset
    /// test falls back to the CPU's robust retry.
    pub fn detect_clip_levels(
        &self,
        samples: &[f32],
        width: usize,
        active: Area,
        cfa: &CFA,
        nominal_white: f32,
        times: Option<&mut PassTimes>,
    ) -> Option<ClipLevels> {
        let (_, top, aw, ah) = active;
        if width == 0 || aw == 0 || ah == 0 || samples.len() < width * (top + ah) {
            return None;
        }
        let data = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(samples),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let height = samples.len() / width;
        self.detect_clip_levels_on(
            &data,
            width,
            height,
            active,
            cfa,
            nominal_white,
            Some(samples),
            times,
        )
    }

    /// `detect_clip_levels` on a mosaic already on the GPU (f32 samples,
    /// `width` × `height`). `samples` (the same values on the CPU) serve the
    /// rare robust retry; without them a colour the onset test misses stays
    /// unclipped.
    #[allow(clippy::too_many_arguments)]
    pub fn detect_clip_levels_on(
        &self,
        data: &wgpu::Buffer,
        width: usize,
        height: usize,
        active: Area,
        cfa: &CFA,
        nominal_white: f32,
        samples: Option<&[f32]>,
        mut times: Option<&mut PassTimes>,
    ) -> Option<ClipLevels> {
        let (left, top, aw, ah) = active;
        let pattern = Pattern::new(cfa);
        let params = Params {
            width: width as u32,
            height: height as u32,
            left: left as u32,
            top: top as u32,
            aw: aw as u32,
            ah: ah as u32,
            misc: [0.0, ONSET_RANGE, ONSET_BINS as f32, 0.0],
            ..bytemuck::Zeroable::zeroed()
        };
        let set = self.bind(&params, data, &pattern, width, height, None, None);
        let chunks = (aw.div_ceil(32) * ah) as u32;
        let mut passes = Passes::new(self);
        passes.run("hl_max", &set.bind, (chunks.div_ceil(256), 1));
        passes.run("hl_onset", &set.bind, (chunks.div_ceil(256), 1));
        let out = passes.finish(
            &[(&set.counters, (COUNTERS * 4) as u64)],
            times.as_deref_mut(),
        );
        let counters = words(&out[0]);
        let maxima: [f32; 3] = std::array::from_fn(|c| f32::from_bits(counters[C_MAX + c]));
        let histograms: [[usize; ONSET_BINS]; 3] = std::array::from_fn(|c| {
            std::array::from_fn(|b| counters[C_HIST + c * ONSET_BINS + b] as usize)
        });
        let mut thresholds = super::thresholds_from_onset(&histograms, maxima, nominal_white);
        if let Some(samples) = samples.filter(|_| thresholds.iter().any(Option::is_none)) {
            // Rare: stray samples above the clip; the CPU's retry.
            let robust = super::robust_maxima(samples, width, active, &pattern, maxima);
            if (0..3).any(|c| thresholds[c].is_none() && robust[c] < maxima[c]) {
                let retry = super::onset_thresholds(
                    samples,
                    width,
                    active,
                    &pattern,
                    robust,
                    nominal_white,
                );
                for c in 0..3 {
                    if thresholds[c].is_none() && robust[c] < maxima[c] {
                        thresholds[c] = retry[c];
                    }
                }
            }
        }
        if thresholds.iter().all(Option::is_none) {
            return None;
        }
        Some(ClipLevels { thresholds })
    }

    fn bind(
        &self,
        params: &Params,
        data: &wgpu::Buffer,
        pattern: &Pattern,
        width: usize,
        height: usize,
        grids: Option<(usize, usize)>,
        out: Option<&wgpu::Buffer>,
    ) -> Bindings {
        let words = width.div_ceil(32);
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::bytes_of(params),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let table: Vec<u32> = pattern
            .table
            .iter()
            .flat_map(|line| line.iter().map(|v| *v as u32))
            .collect();
        let pattern = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&table),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let (cells_n, share_n) = grids.unwrap_or((1, 1));
        let full = grids.is_some();
        let mask_bytes = if full {
            (words * height * 4) as u64
        } else {
            16
        };
        let (mask, hnear, near) = {
            let mut scratch = self
                .scratch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if scratch.as_ref().is_none_or(|s| s.bytes < mask_bytes) {
                *scratch = Some(Scratch {
                    bytes: mask_bytes,
                    mask: self.storage(mask_bytes),
                    hnear: self.storage(mask_bytes),
                    near: self.storage(mask_bytes),
                });
            }
            let s = scratch.as_ref().expect("set above");
            (s.mask.clone(), s.hnear.clone(), s.near.clone())
        };
        let cells = self.storage((cells_n * 12 * 4) as u64);
        let blocks = self.storage((share_n * 6 * 4) as u64);
        let share = self.storage((share_n * 3 * 4) as u64);
        let grid = self.storage((cells_n * GRID_STRIDE * 4) as u64);
        let out = match out {
            Some(out) => out.clone(),
            None => self.storage(16),
        };
        let counters = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&vec![0u32; COUNTERS]),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            });
        let buffers = [
            &uniform, data, &mask, &hnear, &near, &cells, &blocks, &share, &grid, &out, &pattern,
            &counters,
        ];
        let entries: Vec<wgpu::BindGroupEntry> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("highlights"),
            layout: &self.layout,
            entries: &entries,
        });
        Bindings {
            bind,
            uniform,
            cells,
            grid,
            counters,
        }
    }

    /// `reconstruct_clipped_mosaic` with `HighlightRebuild::Ratios` on the
    /// GPU: same arguments, `data` changed in place.
    #[allow(clippy::too_many_arguments)]
    pub fn reconstruct(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        active: Area,
        cfa: &CFA,
        wb: [f32; 4],
        levels: &ClipLevels,
        mut times: Option<&mut PassTimes>,
    ) -> Option<Reconstruction> {
        if data.len() < width * height {
            return None;
        }
        let input = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&data[..width * height]),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
        let out = self.storage((width * height * 4) as u64);
        let result = self.reconstruct_on(
            &input,
            &out,
            width,
            height,
            active,
            cfa,
            wb,
            levels,
            times.as_deref_mut(),
        )?;
        let read = Passes::new(self).finish(&[(&out, (width * height * 4) as u64)], None);
        data[..width * height].copy_from_slice(bytemuck::cast_slice(&read[0]));
        Some(result)
    }

    /// `reconstruct` on a mosaic already on the GPU: the rebuilt mosaic
    /// is written to `output` (at least as large; `input` unchanged). Only
    /// one call runs at a time (the scratch buffers are shared).
    #[allow(clippy::too_many_arguments)]
    pub fn reconstruct_on(
        &self,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: usize,
        height: usize,
        active: Area,
        cfa: &CFA,
        wb: [f32; 4],
        levels: &ClipLevels,
        mut times: Option<&mut PassTimes>,
    ) -> Option<Reconstruction> {
        let (left, top, aw, ah) = active;
        if width == 0 || aw == 0 || ah == 0 || left + aw > width || top + ah > height {
            return None;
        }
        let _running = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wb = [0, 1, 2].map(|c| if wb[c] > 0.0 { wb[c] } else { 1.0 });
        let pattern = Pattern::new(cfa);
        let thresholds = [0, 1, 2].map(|c| levels.thresholds[c].unwrap_or(f32::INFINITY));
        let clip_wb = [0, 1, 2].map(|c| thresholds[c] * wb[c]);
        let grid_cols = width.div_ceil(CELL);
        let grid_rows = height.div_ceil(CELL);
        let share_cols = aw.div_ceil(SHARE_BLOCK);
        let share_rows = ah.div_ceil(SHARE_BLOCK);
        let finite = |v: f32| if v.is_finite() { v } else { NO_CLIP };
        let neutral_level = clip_wb
            .iter()
            .copied()
            .filter(|level| level.is_finite())
            .fold(0.0f32, f32::max);
        let mut params = Params {
            width: width as u32,
            height: height as u32,
            left: left as u32,
            top: top as u32,
            aw: aw as u32,
            ah: ah as u32,
            words: width.div_ceil(32) as u32,
            grid_cols: grid_cols as u32,
            grid_rows: grid_rows as u32,
            share_cols: share_cols as u32,
            share_rows: share_rows as u32,
            ratios: 1,
            thresholds: [
                finite(thresholds[0]),
                finite(thresholds[1]),
                finite(thresholds[2]),
                0.0,
            ],
            wb: [wb[0], wb[1], wb[2], 1.0],
            clip_wb: [
                finite(clip_wb[0]),
                finite(clip_wb[1]),
                finite(clip_wb[2]),
                0.0,
            ],
            misc: [neutral_level, 0.0, 0.0, 0.0],
            ratio_measured: [0; 4],
        };
        let cells_n = grid_cols * grid_rows;
        let set = self.bind(
            &params,
            input,
            &pattern,
            width,
            height,
            Some((cells_n, share_cols * share_rows)),
            Some(output),
        );
        let words = params.words;
        let wg = |n: u32, size: u32| n.div_ceil(size);

        // 1-2: mask, near-clip map, candidates per cell, the share map.
        let mut passes = Passes::new(self);
        passes.run("hl_mask", &set.bind, (wg(words, 64), wg(height as u32, 4)));
        passes.run(
            "hl_near_h",
            &set.bind,
            (wg(words, 64), wg(height as u32, 4)),
        );
        passes.run(
            "hl_near_v",
            &set.bind,
            (wg(words, 64), wg(height as u32, 4)),
        );
        passes.run("hl_cand", &set.bind, (grid_cols as u32, grid_rows as u32));
        passes.run(
            "hl_blocks",
            &set.bind,
            (wg(share_cols as u32, 16), wg(share_rows as u32, 16)),
        );
        passes.run(
            "hl_share",
            &set.bind,
            (wg(share_cols as u32, 16), wg(share_rows as u32, 16)),
        );
        let out = passes.finish(
            &[
                (&set.cells, (cells_n * 12 * 4) as u64),
                (&set.counters, (COUNTERS * 4) as u64),
            ],
            times.as_deref_mut(),
        );
        let clipped_total = words_at(&out[1], C_CLIPPED) as usize;
        if clipped_total == 0 {
            return None;
        }
        let cells = floats(&out[0]);
        let pick = |base: usize| -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
            let sums = (0..cells_n)
                .map(|i| std::array::from_fn(|k| cells[i * 12 + base + k]))
                .collect();
            let counts = (0..cells_n)
                .map(|i| std::array::from_fn(|k| cells[i * 12 + base + 3 + k] as u32))
                .collect();
            (sums, counts)
        };
        let (grid_sum, grid_count) = pick(0);
        let (ratio_sum, ratio_count) = pick(6);
        // The CPU path's grid step (`filled` there).
        let filled = |sums: &[[f32; 3]], counts: &[[u32; 3]], k: usize, relax: bool| {
            let total: f32 = sums.iter().map(|s| s[k]).sum();
            let n: u32 = counts.iter().map(|c| c[k]).sum();
            let mean = if n > 0 { total / n as f32 } else { 0.0 };
            let mut sum: Vec<f32> = sums.iter().map(|s| s[k]).collect();
            let mut count: Vec<u32> = counts.iter().map(|c| c[k]).collect();
            let known: Vec<bool> = count
                .iter()
                .map(|c| *c as usize >= MIN_CELL_SAMPLES)
                .collect();
            let measured = known.iter().any(|k| *k);
            fill_grid(&mut sum, &mut count, grid_cols, grid_rows, mean);
            if relax {
                relax_grid(&mut sum, &known, grid_cols, grid_rows);
            }
            (mean, sum, measured)
        };
        use rayon::prelude::*;
        let grids: Vec<(f32, Vec<f32>, bool)> = (0..6)
            .into_par_iter()
            .map(|k| {
                if k < 3 {
                    filled(&grid_sum, &grid_count, k, true)
                } else {
                    filled(&ratio_sum, &ratio_count, k - 3, true)
                }
            })
            .collect();
        let global_chrominance = [grids[0].0, grids[1].0, grids[2].0];
        let mut grid = vec![0.0f32; cells_n * GRID_STRIDE];
        for i in 0..cells_n {
            for k in 0..6 {
                grid[i * GRID_STRIDE + k] = grids[k].1[i];
            }
        }
        params.ratio_measured = [grids[3].2 as u32, grids[4].2 as u32, grids[5].2 as u32, 0];
        self.queue
            .write_buffer(&set.uniform, 0, bytemuck::bytes_of(&params));
        self.queue
            .write_buffer(&set.grid, 0, bytemuck::cast_slice(&grid));

        // 4: the neutral fill level per cell.
        let mut passes = Passes::new(self);
        passes.run("hl_level", &set.bind, (grid_cols as u32, grid_rows as u32));
        let out = passes.finish(
            &[(&set.cells, (cells_n * 12 * 4) as u64)],
            times.as_deref_mut(),
        );
        let cells = floats(&out[0]);
        let mut level_sum = vec![0.0f32; cells_n];
        let mut level_count = vec![0u32; cells_n];
        for i in 0..cells_n {
            let s: [f32; 3] = std::array::from_fn(|k| cells[i * 12 + k]);
            let c: [u32; 3] = std::array::from_fn(|k| cells[i * 12 + 3 + k] as u32);
            if c.iter().all(|n| *n as usize >= MIN_CELL_SAMPLES) {
                let mean = |k: usize| s[k] / c[k] as f32;
                level_sum[i] = (mean(0) + 2.0 * mean(1) + mean(2)) / 4.0;
                level_count[i] = MIN_CELL_SAMPLES as u32;
            }
        }
        level_sum
            .iter_mut()
            .zip(level_count.iter_mut())
            .for_each(|(s, c)| *s *= *c as f32);
        fill_grid(
            &mut level_sum,
            &mut level_count,
            grid_cols,
            grid_rows,
            neutral_level,
        );
        for i in 0..cells_n {
            grid[i * GRID_STRIDE + 6] = level_sum[i].max(neutral_level);
        }
        self.queue
            .write_buffer(&set.grid, 0, bytemuck::cast_slice(&grid));

        // 5: raise the clipped samples.
        let mut passes = Passes::new(self);
        passes
            .encoder
            .copy_buffer_to_buffer(input, 0, output, 0, (width * height * 4) as u64);
        passes.run("hl_apply", &set.bind, (wg(aw as u32, 64), wg(ah as u32, 4)));
        let out = passes.finish(&[(&set.counters, (COUNTERS * 4) as u64)], times);
        let raised = words_at(&out[0], C_RAISED) as usize;
        Some(Reconstruction {
            levels: levels.clone(),
            clipped_fraction: clipped_total as f32 / (aw * ah) as f32,
            raised,
            global_chrominance,
        })
    }
}

fn words_at(bytes: &[u8], index: usize) -> u32 {
    words(bytes)[index]
}

struct Bindings {
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    cells: wgpu::Buffer,
    grid: wgpu::Buffer,
    counters: wgpu::Buffer,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight_reconstruction::{HighlightRebuild, reconstruct_clipped_mosaic};

    /// A sky brightening to the right with a cloud, clipped in green first,
    /// then in all colours, plus noise and an edge: the GPU rebuild matches
    /// the CPU's within f32 rounding, sample for sample.
    #[test]
    #[ignore = "needs a GPU"]
    fn gpu_rebuild_matches_the_cpu() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .unwrap();
        let gpu = HighlightGpu::new(Arc::new(device), Arc::new(queue));
        let cfa = CFA::new("RGGB");
        let (width, height) = (701, 467);
        let wb = [2.1, 1.0, 1.6, f32::NAN];
        let mut state = 11u32;
        let mut noise = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.01
        };
        let mut data = vec![0.0f32; width * height];
        for row in 0..height {
            for col in 0..width {
                let t = col as f32 / width as f32;
                let (dx, dy) = (col as f32 - 450.0, row as f32 - 200.0);
                let cloud = (-(dx * dx + dy * dy) / 9000.0).exp();
                let dark = if (col as f32) < 120.0 + row as f32 * 0.3 {
                    0.25
                } else {
                    1.0
                };
                let rgb = [
                    (0.45 + 0.6 * t + 0.8 * cloud) * dark,
                    (0.7 + 0.7 * t + 0.8 * cloud) * dark,
                    (0.9 + 0.5 * t + 0.6 * cloud) * dark,
                ];
                let c = cfa.color_at(row, col);
                data[row * width + col] = (rgb[c] / wb[c] + noise()).min(1.0);
            }
        }
        let active = (6, 4, width - 12, height - 8);
        let levels = ClipLevels {
            thresholds: [Some(0.996), Some(0.996), Some(0.996)],
        };
        let mut cpu = data.clone();
        let cpu_result = reconstruct_clipped_mosaic(
            &mut cpu,
            width,
            height,
            active,
            &cfa,
            wb,
            &levels,
            HighlightRebuild::Ratios,
        )
        .expect("clips");
        let mut gpu_data = data.clone();
        let gpu_result = gpu
            .reconstruct(
                &mut gpu_data,
                width,
                height,
                active,
                &cfa,
                wb,
                &levels,
                None,
            )
            .expect("clips");
        assert!(cpu_result.raised > 1000, "{}", cpu_result.raised);
        assert_eq!(cpu_result.raised, gpu_result.raised);
        let worst = cpu
            .iter()
            .zip(&gpu_data)
            .map(|(a, b)| (a - b).abs() / a.abs().max(1e-6))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "largest relative difference {worst}");

        // The clip levels from the GPU's maxima and onset histograms.
        let raw: Vec<f32> = data.iter().map(|v| (v * 4000.0).round()).collect();
        let cpu_levels = super::super::detect_clip_levels(&raw, width, active, &cfa, 4000.0);
        let gpu_levels = gpu.detect_clip_levels(&raw, width, active, &cfa, 4000.0, None);
        assert!(cpu_levels.is_some());
        assert_eq!(cpu_levels, gpu_levels);
    }
}
