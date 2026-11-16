//! SCRATCH ONLY (task GDB probe): `GD_PROBE=3` = `GD_PROBE=1` plus the GPU
//! work the GPU develop would add, run per photo on the app's device and
//! discarded: upload of the u16 mosaic, the GD kernels (unpack, hot pixels,
//! shadow stats, demosaic d1-d5, restore sums, apply), the real highlight
//! reconstruction (clip levels + Ratios rebuild, with its CPU grid steps),
//! and a readback of an Rgb32F frame.

use std::sync::{Arc, Mutex, OnceLock};

use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    w: u32,
    h: u32,
    raw_w: u32,
    rx: u32,
    ry: u32,
    bw: u32,
    bh: u32,
    hl_bins: u32,
    cfa: [u32; 4],
    black: [f32; 4],
    scale: [f32; 4],
    wb: [f32; 4],
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
    misc: [f32; 4],
}

const PASSES: [&str; 10] = [
    "unpack",
    "hot",
    "shadow_stats",
    "d1",
    "d2",
    "d3",
    "d4",
    "d5",
    "restore_sums",
    "apply",
];

struct Load {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    layout: wgpu::BindGroupLayout,
    pipelines: Vec<(&'static str, wgpu::ComputePipeline)>,
    highlights: crate::highlight_reconstruction::gpu::HighlightGpu,
}

static LOAD: OnceLock<Option<Load>> = OnceLock::new();
/// Running photos at once as the export workers would: no lock beyond the
/// device's own queue.
static STATS: Mutex<(u32, f64)> = Mutex::new((0, 0.0));

fn load() -> Option<&'static Load> {
    LOAD.get_or_init(|| {
        let (device, queue) = crate::gpu_demosaic::device()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gd_load"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/gd_ceiling.wgsl").into()),
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
        let mut entries = vec![entry(0, wgpu::BufferBindingType::Uniform)];
        entries.push(entry(1, wgpu::BufferBindingType::Storage { read_only: true }));
        for b in 2..=10 {
            entries.push(entry(b, wgpu::BufferBindingType::Storage { read_only: false }));
        }
        entries.push(entry(11, wgpu::BufferBindingType::Storage { read_only: true }));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &entries,
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = PASSES
            .iter()
            .map(|name| {
                (
                    *name,
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(name),
                        layout: Some(&pl),
                        module: &module,
                        entry_point: Some(name),
                        compilation_options: Default::default(),
                        cache: None,
                    }),
                )
            })
            .collect();
        let highlights =
            crate::highlight_reconstruction::gpu::HighlightGpu::new(device.clone(), queue.clone());
        Some(Load {
            device,
            queue,
            layout,
            pipelines,
            highlights,
        })
    })
    .as_ref()
}

struct Set {
    n: usize,
    rawb: wgpu::Buffer,
    lev: wgpu::Buffer,
    enc: wgpu::Buffer,
    dir: wgpu::Buffer,
    pr: wgpu::Buffer,
    pg: wgpu::Buffer,
    pb: wgpu::Buffer,
    stats: wgpu::Buffer,
    out: wgpu::Buffer,
    counters: wgpu::Buffer,
    gains: wgpu::Buffer,
    staging: wgpu::Buffer,
}

/// One photo's GPU develop at a time, on buffers of the largest photo so
/// far (as gpu_demosaic: the frame buffers are large; new ones are
/// zero-filled on the GPU).
static SET: Mutex<Option<Set>> = Mutex::new(None);

/// The GPU work for one photo's mosaic (`w` × `h`, values about 0-1),
/// colours of the 2 × 2 pattern in `colors` (row-major).
pub fn run(mosaic: &[f32], w: usize, h: usize, colors: [u32; 4]) {
    let Some(g) = load() else {
        log::warn!("gd_load: no device");
        return;
    };
    let started = std::time::Instant::now();
    let dev = &g.device;
    let n = w * h;
    use rayon::prelude::*;
    let mut packed: Vec<u16> = mosaic
        .par_iter()
        .map(|v| (v.clamp(0.0, 1.0) * 16383.0) as u16)
        .collect();
    if packed.len() % 2 == 1 {
        packed.push(0);
    }
    let prep_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut guard = SET.lock().unwrap_or_else(|e| e.into_inner());
    let waited_ms = started.elapsed().as_secs_f64() * 1000.0 - prep_ms;
    let t_gpu = std::time::Instant::now();
    let bw = w.div_ceil(12);
    let bh = h.div_ceil(12);
    let storage = |size: u64| {
        dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    if guard.as_ref().is_none_or(|s| s.n < n) {
        let f = (n * 4) as u64;
        let cap_b = (n.div_ceil(12 * 12) + 2 * (w + h)) as u64;
        *guard = Some(Set {
            n,
            rawb: storage((n as u64 + 2) / 2 * 4),
            lev: storage(f),
            enc: storage(f),
            dir: storage(f * 2),
            pr: storage(f),
            pg: storage(f),
            pb: storage(f),
            stats: storage(cap_b * 15 * 4),
            out: storage(f * 2),
            counters: storage((16 + 3 * 512) * 4),
            gains: storage(cap_b * 16),
            staging: dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: f * 3,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        });
    }
    let set = guard.as_ref().expect("set above");
    let params = Params {
        w: w as u32,
        h: h as u32,
        raw_w: w as u32,
        rx: 0,
        ry: 0,
        bw: bw as u32,
        bh: bh as u32,
        hl_bins: 512,
        cfa: colors,
        black: [0.0; 4],
        scale: [1.0 / 16383.0; 4],
        wb: [2.0, 1.0, 1.5, 1.0],
        m0: [1.6, -0.5, -0.1, 0.0],
        m1: [-0.2, 1.5, -0.3, 0.0],
        m2: [0.0, -0.4, 1.4, 0.0],
        misc: [1.0, 0.98, 0.0, 0.0],
    };
    let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let raw_bytes: &[u8] = bytemuck::cast_slice(&packed);
    g.queue.write_buffer(&set.rawb, 0, raw_bytes);
    let f = (n * 4) as u64;
    let bufs = [
        &ub, &set.rawb, &set.lev, &set.enc, &set.dir, &set.pr, &set.pg, &set.pb, &set.stats,
        &set.out, &set.counters, &set.gains,
    ];
    let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.layout,
        entries: &bufs
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect::<Vec<_>>(),
    });
    let groups2 = ((w as u32).div_ceil(16), (h as u32).div_ceil(16));
    let groups1 = ((bw * bh) as u32).div_ceil(64);
    let pass = |encoder: &mut wgpu::CommandEncoder, name: &str| {
        let (_, pipe) = g.pipelines.iter().find(|(n, _)| *n == name).unwrap();
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &bg, &[]);
        if name == "shadow_stats" || name == "restore_sums" {
            pass.dispatch_workgroups(groups1, 1, 1);
        } else {
            pass.dispatch_workgroups(groups2.0, groups2.1, 1);
        }
    };
    // Unpack, then the highlights on the GPU-resident mosaic (one upload,
    // the rebuilt mosaic stays on the GPU).
    let mut encoder = dev.create_command_encoder(&Default::default());
    encoder.clear_buffer(&set.counters, 0, None);
    pass(&mut encoder, "unpack");
    g.queue.submit(Some(encoder.finish()));
    let cfa = rawler::CFA::new(
        &colors
            .iter()
            .map(|c| ['R', 'G', 'B', 'E'][*c as usize])
            .collect::<String>(),
    );
    let active = (0, 0, w, h);
    let mut encoder = dev.create_command_encoder(&Default::default());
    if let Some(levels) =
        g.highlights
            .detect_clip_levels_on(&set.lev, w, h, active, &cfa, 0.0, None, None)
        && g.highlights
            .reconstruct_on(
                &set.lev,
                &set.enc,
                w,
                h,
                active,
                &cfa,
                [2.0, 1.0, 1.5, 1.0],
                &levels,
                None,
            )
            .is_some()
    {
        encoder.copy_buffer_to_buffer(&set.enc, 0, &set.lev, 0, f);
    }
    let hl_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;
    for name in &PASSES[1..] {
        pass(&mut encoder, name);
    }
    // EXT7 `noreadback`: a GPU-resident design keeps the frame on the GPU;
    // wait for the work only.
    if std::env::var("EXT6_SKIP").is_ok_and(|v| v.split(',').any(|t| t == "noreadback")) {
        g.queue.submit(Some(encoder.finish()));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        g.queue.on_submitted_work_done(move || flag.store(true, std::sync::atomic::Ordering::Release));
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            let _ = dev.poll(wgpu::PollType::Poll);
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
        drop(guard);
        log::info!("gd_load: {w}x{h} noreadback {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
        return;
    }
    // The developed frame back as Rgb32F (12 bytes per pixel).
    encoder.copy_buffer_to_buffer(&set.pr, 0, &set.staging, 0, f);
    encoder.copy_buffer_to_buffer(&set.pg, 0, &set.staging, f, f);
    encoder.copy_buffer_to_buffer(&set.pb, 0, &set.staging, 2 * f, f);
    let index = g.queue.submit(Some(encoder.finish()));
    let slice = set.staging.slice(..3 * f);
    // EXT7: non-blocking wait (a blocking poll holds the device's fence
    // lock against other workers' submits, BX7).
    let _ = index;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    slice.map_async(wgpu::MapMode::Read, move |_| flag.store(true, std::sync::atomic::Ordering::Release));
    while !done.load(std::sync::atomic::Ordering::Acquire) {
        let _ = dev.poll(wgpu::PollType::Poll);
        std::thread::sleep(std::time::Duration::from_micros(500));
    }
    let gpu_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;
    {
        let mapped = slice.get_mapped_range().expect("mapped");
        let mut copy = vec![0f32; n * 3];
        copy.copy_from_slice(bytemuck::cast_slice(&mapped));
        std::hint::black_box(&copy);
    }
    set.staging.unmap();
    drop(guard);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut s = STATS.lock().unwrap_or_else(|e| e.into_inner());
    s.0 += 1;
    s.1 += ms;
    log::info!(
        "gd_load: {w}x{h} {ms:.0} ms (prep {prep_ms:.0}, wait {waited_ms:.0}, highlights {hl_ms:.0}, gpu done {gpu_ms:.0}; photos {}, sum {:.0} ms)",
        s.0,
        s.1
    );
}
