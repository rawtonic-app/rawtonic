//! Task GDB (G43): the GPU cost of the real highlight reconstruction
//! (`HighlightRebuild::Ratios`) and of the clip-level detection, against the
//! CPU path on the same mosaic, per RAW file.
//!
//! `cargo run --release -p rawtonic-rawdecode --example gdb_highlights -- OUT.txt RAW...`
//!
//! The mosaic is the decoder's (rawler), normalised to black 0 / white 1;
//! the product's hot-pixel and black-bias steps before it are left out, as
//! both paths see the same input.

use std::sync::Arc;
use std::time::Instant;

use rawler::{
    decoders::RawDecodeParams,
    rawimage::{RawImageData, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use rawtonic_rawdecode::highlight_reconstruction::{
    self as hl, HighlightRebuild,
    gpu::{HighlightGpu, PassTimes},
};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = &args[1];
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .unwrap();
    let gpu = HighlightGpu::new(Arc::new(device), Arc::new(queue));
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut report = format!(
        "# gdb_highlights: {} | rayon threads {}\n# file MP clipped% | detect: cpu1 ms, gpu ms, levels equal | rebuild: cpu1 ms, cpu-pool ms, gpu ms (median of 5), gpu wall ms | raised cpu/gpu | samples differing >1e-6 rel, max rel diff | passes\n",
        adapter.get_info().name,
        rayon::current_num_threads()
    );
    let mut sum_span = 0.0;
    let (mut sum_cpu1, mut sum_pool, mut sum_gpu, mut sum_det_gpu, mut sum_det_cpu) =
        (0.0, 0.0, 0.0, 0.0, 0.0);
    for path in &args[2..] {
        let bytes = std::fs::read(path).unwrap();
        let source = RawSource::new_from_slice(&bytes);
        let Ok(decoder) = rawler::get_decoder(&source) else {
            report.push_str(&format!("{path}: no decoder\n"));
            continue;
        };
        let Ok(raw) = decoder.raw_image(&source, &RawDecodeParams::default(), false) else {
            report.push_str(&format!("{path}: decode failed\n"));
            continue;
        };
        let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else {
            report.push_str(&format!("{path}: not CFA\n"));
            continue;
        };
        if !config.cfa.is_rgb() || raw.cpp != 1 {
            report.push_str(&format!("{path}: not an RGB mosaic\n"));
            continue;
        }
        let RawImageData::Integer(samples) = &raw.data else {
            report.push_str(&format!("{path}: not integer\n"));
            continue;
        };
        let (width, height) = (raw.width, raw.height);
        let active = raw
            .active_area
            .map(|a| (a.p.x, a.p.y, a.d.w, a.d.h))
            .unwrap_or((0, 0, width, height));
        let white = raw.whitelevel.0.first().cloned().unwrap_or(u16::MAX as u32) as f32;
        let black = raw
            .blacklevel
            .levels
            .first()
            .map(|r| r.as_f32())
            .unwrap_or(0.0);
        let cfa = &config.cfa;
        let raw_f: Vec<f32> = samples.iter().map(|v| *v as f32).collect();

        let t = Instant::now();
        let cpu_levels = one.install(|| hl::detect_clip_levels(samples, width, active, cfa, white));
        let det_cpu = t.elapsed().as_secs_f64() * 1000.0;
        let mut det_times = Vec::new();
        let mut gpu_levels = None;
        for _ in 0..6 {
            let mut times = PassTimes::default();
            gpu_levels =
                gpu.detect_clip_levels(&raw_f, width, active, cfa, white, Some(&mut times));
            det_times.push(times.total());
        }
        let det_gpu = median(det_times[1..].to_vec());
        sum_det_cpu += det_cpu;
        sum_det_gpu += det_gpu;
        let name = std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let mp = (active.2 * active.3) as f64 / 1e6;
        let Some(levels) = cpu_levels.clone() else {
            report.push_str(&format!(
                "{name} {mp:.1} 0 | detect {det_cpu:.1} {det_gpu:.2} {} | no clipping\n",
                gpu_levels == cpu_levels
            ));
            continue;
        };
        let scale = 1.0 / (white - black).max(1.0);
        let data: Vec<f32> = raw_f.iter().map(|v| (v - black) * scale).collect();
        let scaled = hl::ClipLevels {
            thresholds: levels.thresholds.map(|l| l.map(|v| (v - black) * scale)),
        };
        let wb = raw.wb_coeffs;
        let rebuild = |d: &mut Vec<f32>| {
            hl::reconstruct_clipped_mosaic(
                d,
                width,
                height,
                active,
                cfa,
                wb,
                &scaled,
                HighlightRebuild::Ratios,
            )
        };
        let mut cpu = data.clone();
        let t = Instant::now();
        let cpu_result = one.install(|| rebuild(&mut cpu));
        let cpu1 = t.elapsed().as_secs_f64() * 1000.0;
        let mut pool_runs = Vec::new();
        for _ in 0..3 {
            let mut d = data.clone();
            let t = Instant::now();
            rebuild(&mut d);
            pool_runs.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let pool = median(pool_runs);
        let mut gpu_runs = Vec::new();
        let mut span_runs = Vec::new();
        let mut walls = Vec::new();
        let mut last = PassTimes::default();
        let mut g = data.clone();
        let mut gpu_result = None;
        for _ in 0..6 {
            g = data.clone();
            let mut times = PassTimes::default();
            let t = Instant::now();
            gpu_result = gpu.reconstruct(
                &mut g,
                width,
                height,
                active,
                cfa,
                wb,
                &scaled,
                Some(&mut times),
            );
            walls.push(t.elapsed().as_secs_f64() * 1000.0);
            gpu_runs.push(times.total());
            span_runs.push(times.spans.iter().sum::<f64>());
            last = times;
        }
        let gpu_ms = median(gpu_runs[1..].to_vec());
        let wall = median(walls[1..].to_vec());
        let span = median(span_runs[1..].to_vec());
        sum_span += span;
        sum_cpu1 += cpu1;
        sum_pool += pool;
        sum_gpu += gpu_ms;
        let (mut differ, mut max_rel) = (0usize, 0.0f64);
        for (a, b) in cpu.iter().zip(&g) {
            let rel = ((a - b).abs() / a.abs().max(1e-6)) as f64;
            if rel > 1e-6 {
                differ += 1;
            }
            max_rel = max_rel.max(rel);
        }
        let passes: Vec<String> = last
            .ms
            .iter()
            .map(|(n, v)| format!("{}={v:.2}", n.trim_start_matches("hl_")))
            .collect();
        report.push_str(&format!(
            "{name} {mp:.1} {:.2} | detect {det_cpu:.1} {det_gpu:.2} {} | rebuild {cpu1:.0} {pool:.0} {gpu_ms:.2} span {span:.2} {wall:.0} | raised {}/{} | differ {differ} max_rel {max_rel:.2e} | {}\n",
            cpu_result.as_ref().map_or(0.0, |r| r.clipped_fraction * 100.0),
            gpu_levels == cpu_levels,
            cpu_result.as_ref().map_or(0, |r| r.raised),
            gpu_result.as_ref().map_or(0, |r| r.raised),
            passes.join(" ")
        ));
        eprint!(".");
    }
    report.push_str(&format!(
        "# totals: detect cpu1 {:.0} ms gpu {:.1} ms | rebuild cpu1 {:.0} ms pool {:.0} ms gpu {:.1} ms (spans {:.1} ms)\n",
        sum_det_cpu, sum_det_gpu, sum_cpu1, sum_pool, sum_gpu, sum_span
    ));
    std::fs::write(out_path, &report).unwrap();
    print!("{report}");
}
