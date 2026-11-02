//! SCRATCH ONLY (task GD, G43 ceiling; not product code).
//!
//! `cargo run --release -p rawtonic-rawdecode --example gd_ceiling -- OUT.txt NEF...`
//!
//! Per Bayer NEF: decode on the CPU (rawler), then
//!  * CPU reference: hot pixels, shadow-mean stats, wb mosaic, demosaic,
//!    restore — each timed on a 1-thread rayon pool (= CPU-seconds) and on
//!    the full pool (wall);
//!  * GPU: upload of the u16 mosaic, then the same stages as wgpu compute
//!    (gd_ceiling.wgsl), each pass timed with Metal timestamp queries,
//!    6 repeats (first is warm-up);
//!  * accuracy: GPU demosaic vs our CPU demosaic on the same mosaic.

use std::time::Instant;

use rawler::{
    decoders::RawDecodeParams,
    rawimage::{RawImageData, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use rawtonic_rawdecode::{black_bias::ShadowMeans, demosaic, hot_pixels};
use rayon::prelude::*;
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

const PASSES: [&str; 13] = [
    "unpack",
    "hot",
    "shadow_stats",
    "hl_hist",
    "hl_rebuild",
    "d1",
    "d2",
    "d3",
    "d4",
    "d5",
    "restore_sums",
    "apply",
    "_end",
];
const HL_BINS: u32 = 512;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: Vec<(String, wgpu::ComputePipeline)>,
    period: f32,
}

fn gpu() -> Gpu {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .unwrap();
    eprintln!("adapter {:?}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .unwrap();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gd"),
        source: wgpu::ShaderSource::Wgsl(include_str!("gd_ceiling.wgsl").into()),
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
    let pipelines = PASSES[..12]
        .iter()
        .map(|name| {
            (
                name.to_string(),
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
    let period = queue.get_timestamp_period();
    Gpu {
        device,
        queue,
        layout,
        pipelines,
        period,
    }
}

fn read_buffer(g: &Gpu, src: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let staging = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(src, 0, &staging, 0, size);
    g.queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    let v = slice.get_mapped_range().to_vec();
    staging.unmap();
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = &args[1];
    let files = &args[2..];
    let g = gpu();
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut report = String::new();
    report.push_str(&format!(
        "# gd_ceiling: rayon threads {}\n",
        rayon::current_num_threads()
    ));
    for path in files {
        let bytes = std::fs::read(path).unwrap();
        let t = Instant::now();
        let source = RawSource::new_from_slice(&bytes);
        let Ok(decoder) = rawler::get_decoder(&source) else {
            report.push_str(&format!("{path}: no decoder\n"));
            continue;
        };
        let raw = match decoder.raw_image(&source, &RawDecodeParams::default(), false) {
            Ok(raw) => raw,
            Err(e) => {
                report.push_str(&format!("{path}: decode failed: {e}\n"));
                continue;
            }
        };
        let decode_ms = ms(t);
        let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else {
            report.push_str(&format!("{path}: not CFA\n"));
            continue;
        };
        let RawImageData::Integer(data) = &raw.data else {
            report.push_str(&format!("{path}: not integer\n"));
            continue;
        };
        let roi = raw.active_area.unwrap_or_else(|| {
            rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), raw.dim())
        });
        let (rx, ry, w, h) = (roi.p.x, roi.p.y, roi.d.w, roi.d.h);
        let colors: Vec<u8> = (0..2)
            .flat_map(|r| (0..2).map(move |c| (r, c)))
            .map(|(r, c)| config.cfa.color_at(ry + r, rx + c) as u8)
            .collect();
        let pattern = demosaic::CfaPattern::new(2, 2, colors.clone()).unwrap();
        if !pattern.is_bayer() {
            report.push_str(&format!("{path}: not Bayer\n"));
            continue;
        }
        let black = raw.blacklevel.as_bayer_array();
        let white = raw.whitelevel.as_bayer_array();
        let scale: [f32; 4] = std::array::from_fn(|i| 1.0 / (white[i] - black[i]).max(1.0));
        let wb_raw = raw.wb_coeffs;
        let wb: [f32; 4] = if wb_raw[1] > 0.0 && wb_raw[0].is_finite() {
            [wb_raw[0] / wb_raw[1], 1.0, wb_raw[2] / wb_raw[1], 1.0]
        } else {
            [2.0, 1.0, 1.5, 1.0]
        };
        let cfa_roi = config.cfa.shift(rx, ry);
        let mp = (w * h) as f64 / 1e6;
        eprintln!("{path}: {w}x{h} ({mp:.1} MP), decode {decode_ms:.0} ms");

        // ---- CPU level plane (same arithmetic as the GPU unpack) ----
        let level: Vec<f32> = (0..h)
            .into_par_iter()
            .flat_map_iter(|r| {
                let (ay, raw_w) = (ry + r, raw.width);
                (0..w).map(move |c| {
                    let ax = rx + c;
                    let pos = (ay & 1) * 2 + (ax & 1);
                    (data[ay * raw_w + ax] as f32 - black[pos]) * scale[pos]
                })
            })
            .collect();

        // ---- CPU reference stages: 1 thread (CPU-s) and full pool (wall) ----
        let cpu_stages = |pool: Option<&rayon::ThreadPool>| {
            let run = |f: &mut (dyn FnMut() + Send)| {
                let t = Instant::now();
                match pool {
                    Some(p) => p.install(|| f()),
                    None => f(),
                }
                ms(t)
            };
            let mut lev = level.clone();
            let mut fixed = 0;
            let hot = run(&mut || {
                fixed = hot_pixels::suppress(
                    &mut lev,
                    w,
                    (0, 0, w, h),
                    &cfa_roi,
                    wb,
                    hot_pixels::DeadTest::Adjacent,
                    |v| v,
                    |v| v,
                );
            });
            let mut means = None;
            let stats = run(&mut || {
                means = Some(ShadowMeans::measure(
                    w,
                    h,
                    |r, c| lev[r * w + c],
                    |r, c| pattern.color(r as isize, c as isize),
                ));
            });
            let mut mosaic = Vec::new();
            let wbm = run(&mut || {
                mosaic = lev
                    .par_iter()
                    .enumerate()
                    .map(|(i, v)| v * wb[pattern.color((i / w) as isize, (i % w) as isize)])
                    .collect();
            });
            let mut rgb = Vec::new();
            let dem = run(&mut || {
                rgb = demosaic::demosaic(&mosaic, w, h, &pattern);
            });
            let mut rgb2 = rgb.clone();
            let restore = run(&mut || {
                means
                    .as_ref()
                    .unwrap()
                    .restore(&mut rgb2, w, [wb[0], wb[1], wb[2]]);
            });
            ((hot, stats, wbm, dem, restore), fixed, mosaic, rgb)
        };
        let ((h1, s1, w1, d1, r1), fixed_cpu, _, _) = cpu_stages(Some(&one));
        let ((hn, sn, wn, dn, rn), _, mosaic_cpu, rgb_cpu) = cpu_stages(None);
        let cpu1 = h1 + s1 + w1 + d1 + r1;
        report.push_str(&format!(
            "\n## {path}\n{w}x{h} = {mp:.1} MP, rawler decode {decode_ms:.0} ms (full pool)\n\
             CPU 1 thread (ms = CPU-ms): hot {h1:.0}, shadow stats {s1:.0}, wb mosaic {w1:.0}, demosaic {d1:.0}, restore {r1:.0}; sum {cpu1:.0}\n\
             CPU full pool wall (ms): hot {hn:.0}, shadow stats {sn:.0}, wb mosaic {wn:.0}, demosaic {dn:.0}, restore {rn:.0}; sum {:.0}\n\
             hot/dead fixed (CPU): {fixed_cpu}\n",
            hn + sn + wn + dn + rn
        ));

        // ---- GPU ----
        let n = w * h;
        let bw = w.div_ceil(12);
        let bh = h.div_ceil(12);
        let params = Params {
            w: w as u32,
            h: h as u32,
            raw_w: raw.width as u32,
            rx: rx as u32,
            ry: ry as u32,
            bw: bw as u32,
            bh: bh as u32,
            hl_bins: HL_BINS,
            cfa: std::array::from_fn(|i| colors[i] as u32),
            black,
            scale,
            wb,
            m0: [1.6, -0.5, -0.1, 0.0],
            m1: [-0.2, 1.5, -0.3, 0.0],
            m2: [0.0, -0.4, 1.4, 0.0],
            misc: [1.0, 0.98, 0.0, 0.0],
        };
        let dev = &g.device;
        let ub = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
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
        let mut packed: Vec<u16> = data.clone();
        if packed.len() % 2 == 1 {
            packed.push(0);
        }
        let raw_bytes: &[u8] = bytemuck::cast_slice(&packed);
        let rawb = storage(raw_bytes.len() as u64);
        let f = (n * 4) as u64;
        let lev_b = storage(f);
        let enc_b = storage(f);
        let dir_b = storage(f * 2);
        let pr = storage(f);
        let pg = storage(f);
        let pb = storage(f);
        let stats_b = storage((bw * bh * 15 * 4) as u64);
        let out_b = storage((n * 8) as u64);
        let counters = storage((16 + 3 * HL_BINS as u64) * 4);
        let gains: Vec<[f32; 4]> = (0..bw * bh)
            .map(|i| [1.0 + (i % 7) as f32 * 1e-3, 1.0, 1.0, 0.0])
            .collect();
        let gains_b = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&gains),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bufs = [
            &ub, &rawb, &lev_b, &enc_b, &dir_b, &pr, &pg, &pb, &stats_b, &out_b, &counters,
            &gains_b,
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
        let qs = dev.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 64,
        });
        let resolve = dev.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 8,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let groups2 = ((w as u32).div_ceil(16), (h as u32).div_ceil(16));
        let groups1 = ((bw * bh) as u32).div_ceil(64);
        let encode_passes = |order: &[usize], timed: bool| {
            let mut encoder = dev.create_command_encoder(&Default::default());
            encoder.clear_buffer(&counters, 0, None);
            for (k, &pi) in order.iter().enumerate() {
                let (name, pipe) = &g.pipelines[pi];
                let tw = timed.then(|| wgpu::ComputePassTimestampWrites {
                    query_set: &qs,
                    beginning_of_pass_write_index: Some(2 * k as u32),
                    end_of_pass_write_index: Some(2 * k as u32 + 1),
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(name),
                    timestamp_writes: tw,
                });
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, &bg, &[]);
                if name == "shadow_stats" || name == "restore_sums" {
                    pass.dispatch_workgroups(groups1, 1, 1);
                } else {
                    pass.dispatch_workgroups(groups2.0, groups2.1, 1);
                }
            }
            if timed {
                encoder.resolve_query_set(&qs, 0..2 * order.len() as u32, &resolve, 0);
            }
            encoder
        };

        // Upload (CPU wall incl. submit and wait; includes Vec copy into the queue's staging).
        let mut uploads = Vec::new();
        for _ in 0..4 {
            let t = Instant::now();
            g.queue.write_buffer(&rawb, 0, raw_bytes);
            g.queue.submit(None);
            let _ = dev.poll(wgpu::PollType::wait_indefinitely());
            uploads.push(ms(t));
        }

        // Accuracy run: product order without the highlight proxy.
        let acc_order = [0usize, 1, 2, 5, 6, 7, 8, 9];
        g.queue.submit(Some(encode_passes(&acc_order, false).finish()));
        let _ = dev.poll(wgpu::PollType::wait_indefinitely());
        let cnt = read_buffer(&g, &counters, 4);
        let fixed_gpu = u32::from_le_bytes(cnt[0..4].try_into().unwrap());
        let lev_gpu: Vec<f32> = bytemuck::cast_slice(&read_buffer(&g, &lev_b, f)).to_vec();
        let planes: Vec<Vec<f32>> = [&pr, &pg, &pb]
            .iter()
            .map(|b| bytemuck::cast_slice(&read_buffer(&g, b, f)).to_vec())
            .collect();
        // mosaic difference (hot pixel fix) and demosaic difference
        let mosaic_gpu: Vec<f32> = lev_gpu
            .iter()
            .enumerate()
            .map(|(i, v)| v * wb[pattern.color((i / w) as isize, (i % w) as isize)])
            .collect();
        let mosaic_diff = mosaic_gpu
            .iter()
            .zip(&mosaic_cpu)
            .filter(|(a, b)| a != b)
            .count();
        // our CPU demosaic of the GPU's mosaic (isolates the demosaic port)
        let rgb_ref = demosaic::demosaic(&mosaic_gpu, w, h, &pattern);
        let _ = rgb_cpu;
        let border = 12;
        let (mut maxrel, mut differ, mut differ16, mut total, mut sq, mut sig) =
            (0f64, 0usize, 0usize, 0usize, 0f64, 0f64);
        let (mut differ_border16, mut total_border) = (0usize, 0usize);
        let (mut maxcode, mut maxcode_at, mut over1) = (0i64, 0f64, 0usize);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let inner = y >= border && x >= border && y < h - border && x < w - border;
                for k in 0..3 {
                    let a = planes[k][i] as f64;
                    let b = rgb_ref[i][k] as f64;
                    let q = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as i64;
                    if inner {
                        total += 1;
                        if a != b {
                            differ += 1;
                        }
                        if q(a) != q(b) {
                            differ16 += 1;
                            let d = (q(a) - q(b)).abs();
                            if d > 1 {
                                over1 += 1;
                            }
                            if d > maxcode {
                                maxcode = d;
                                maxcode_at = b;
                            }
                        }
                        let rel = (a - b).abs() / b.abs().max(1e-4);
                        maxrel = maxrel.max(rel);
                        sq += (a - b) * (a - b);
                        sig += b * b;
                    } else {
                        total_border += 1;
                        if q(a) != q(b) {
                            differ_border16 += 1;
                        }
                    }
                }
            }
        }
        let snr_db = 10.0 * (sig / sq.max(1e-300)).log10();
        report.push_str(&format!(
            "hot/dead fixed (GPU): {fixed_gpu}; mosaic samples differing GPU vs CPU: {mosaic_diff}\n\
             demosaic GPU vs our CPU demosaic of the same mosaic (interior, {border} px border excluded): \
             {differ}/{total} values differ in f32 ({:.3} %), {differ16} differ as 16-bit codes ({:.5} %), \
             max rel diff {maxrel:.2e}, signal/difference {snr_db:.1} dB, max 16-bit code diff {maxcode} (at linear {maxcode_at:.4}), codes off by >1: {over1}; border: {differ_border16}/{total_border} 16-bit codes differ\n",
            100.0 * differ as f64 / total as f64,
            100.0 * differ16 as f64 / total as f64,
        ));

        // Timing: full order in product sequence, 6 repeats, first = warm-up.
        let order: Vec<usize> = (0..12).collect();
        let mut per_pass: Vec<Vec<f64>> = vec![Vec::new(); 12];
        let mut totals = Vec::new();
        for rep in 0..6 {
            let encoder = encode_passes(&order, true);
            let readb = dev.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 64 * 8,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = encoder;
            encoder.copy_buffer_to_buffer(&resolve, 0, &readb, 0, 64 * 8);
            g.queue.submit(Some(encoder.finish()));
            readb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = dev.poll(wgpu::PollType::wait_indefinitely());
            let ts: Vec<u64> = bytemuck::cast_slice(&readb.slice(..).get_mapped_range()).to_vec();
            let p = g.period as f64 / 1e6;
            if rep > 0 {
                for k in 0..12 {
                    per_pass[k].push((ts[2 * k + 1] - ts[2 * k]) as f64 * p);
                }
                totals.push((ts[23] - ts[0]) as f64 * p);
            }
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        report.push_str("GPU per pass (Metal timestamps, median of 5, ms):");
        let mut sum = 0.0;
        for (k, v) in per_pass.iter_mut().enumerate() {
            let m = median(v);
            sum += m;
            report.push_str(&format!(" {} {m:.2};", PASSES[k]));
        }
        let mut up = uploads.clone();
        report.push_str(&format!(
            "\nGPU sum of passes {sum:.1} ms, first start -> last end {:.1} ms; upload of the u16 mosaic ({:.0} MB) {:.1} ms CPU wall (median of 4)\n",
            median(&mut totals),
            raw_bytes.len() as f64 / 1e6,
            median(&mut up)
        ));
        eprint!("{report}");
        std::fs::write(out_path, &report).unwrap();
    }
}
