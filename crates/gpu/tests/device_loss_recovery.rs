//! DVL1 (G74/G62): which single GPU submissions the driver's watchdog
//! takes the device away for, and that a new device works after it in the
//! same process (the step the app's recovery rests on).
//!
//! The probes make real losses: submissions that grow until the device is
//! lost or one runs for a minute. On Windows every window may flicker.
//! On the Radeon 680M (Vulkan, driver 25.10) two losses seconds apart
//! once froze Windows until a power cycle (no BugCheck, event 6008 only);
//! `DVL1_PROBES=B` runs one probe (A, B, C) per process.
//! Measured there (2026-11-23): kept 1.67 / 1.86 / 1.75 s, lost at
//! 2.15 / 2.18 / 2.41 s; a new device worked 0.24-0.26 s after the loss.
//! Run them alone, on purpose:
//!
//! `cargo test -p rawtonic-gpu --test device_loss_recovery -- --ignored --nocapture --test-threads 1`

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rawtonic_gpu::device_start::{self, LostDevice};

/// Words in the probe's buffer.
const WORDS: u32 = 1 << 16;

/// Each invocation runs STEPS steps of a recurrence through memory (a load
/// and a store per step, so the compiler cannot shorten it; naga's loop
/// bound is 2^64).
const PROBE: &str = r"
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let k = (id.x + id.y * 4096u) & 65535u;
    var i = 0u;
    loop {
        if (i >= STEPSu) { break; }
        let j = (data[k] + i) & 65535u;
        data[k] = data[j] * 1664525u + 1013904223u + k;
        i = i + 1u;
    }
}
";

/// A submission's shape: `dispatches` dispatches of `groups` × `rows`
/// workgroups of 64, each invocation `steps` steps long.
#[derive(Debug, Clone, Copy)]
struct Shape {
    steps: u64,
    groups: u32,
    rows: u32,
    dispatches: u32,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn start() -> Gpu {
    let started = device_start::start_device(|_| None, wgpu::Backends::empty(), |_| {}).unwrap();
    started
        .device
        .on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
            eprintln!("DVL1 uncaptured: {error}");
        }));
    Gpu {
        device: started.device,
        queue: started.queue,
    }
}

fn wait(device: &wgpu::Device) {
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(Duration::from_secs(90)),
    });
}

/// Runs one submission of `shape`: how long it took, and whether its result
/// came back and a small submission after it still works.
fn run(gpu: &Gpu, shape: Shape) -> (Duration, Result<(), String>) {
    let device = &gpu.device;
    let source = PROBE.replace("STEPS", &shape.steps.to_string());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("DVL1 probe"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("DVL1 probe"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let data = rawtonic_rawdecode::gpu_util::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("DVL1 data"),
            contents: bytemuck::cast_slice(&vec![1u32; WORDS as usize]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        },
    );
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: data.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    for _ in 0..shape.dispatches {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(shape.groups, shape.rows, 1);
    }
    // Compiled and created by now: the clock times the submission alone.
    wait(device);
    let submitted = Instant::now();
    gpu.queue.submit(Some(encoder.finish()));
    wait(device);
    let took = submitted.elapsed();
    (took, small(gpu))
}

/// A tiny submission read back: whether the device still works.
fn small(gpu: &Gpu) -> Result<(), String> {
    let device = &gpu.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("DVL1 small"),
        source: wgpu::ShaderSource::Wgsl(
            r"
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    data[id.x] = id.x * 3u + 1u;
}"
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("DVL1 small"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let words = 4096u32;
    let data = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("DVL1 small data"),
        size: u64::from(words) * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("DVL1 small read"),
        size: u64::from(words) * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: data.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(words / 64, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&data, 0, &read, 0, u64::from(words) * 4);
    gpu.queue.submit(Some(encoder.finish()));
    let (sender, receiver) = mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    wait(device);
    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            let right = {
                let view = read
                    .slice(..)
                    .get_mapped_range()
                    .map_err(|error| format!("{error:?}"))?;
                bytemuck::cast_slice::<u8, u32>(&view)
                    .iter()
                    .enumerate()
                    .all(|(i, v)| *v == i as u32 * 3 + 1)
            };
            read.unmap();
            if device_start::device_lost() {
                Err("the device is marked lost".into())
            } else if right {
                Ok(())
            } else {
                Err("wrong values".into())
            }
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the map never answered".into()),
    }
}

/// Grows `shape` with `grow` until a submission loses the device or runs a
/// minute; prints each and returns the longest that kept the device and
/// the first that lost it.
fn probe(
    name: &str,
    mut shape: Shape,
    grow: impl Fn(Shape) -> Shape,
    lost: &mpsc::Receiver<LostDevice>,
) -> (Option<Duration>, Option<Duration>) {
    let gpu = start();
    let mut kept = None;
    loop {
        let (took, outcome) = run(&gpu, shape);
        eprintln!("DVL1 {name}: {shape:?} took {took:.2?}: {outcome:?}");
        match outcome {
            Ok(()) => kept = Some(took),
            Err(_) => {
                let why = lost
                    .recv_timeout(Duration::from_secs(10))
                    .map(|lost| lost.message)
                    .unwrap_or_else(|_| "no lost callback".into());
                eprintln!("DVL1 {name}: device lost after a {took:.2?} submission: {why}");
                return (kept, Some(took));
            }
        }
        if took > Duration::from_secs(60) {
            return (kept, None);
        }
        shape = grow(shape);
    }
}

#[test]
#[ignore = "requires a native GPU; may reset the display driver (Windows TDR)"]
fn which_submissions_lose_the_device_and_a_new_device_works_after() {
    let (lost_sender, lost_receiver) = mpsc::channel::<LostDevice>();
    let lost_sender = std::sync::Mutex::new(lost_sender);
    device_start::on_device_lost(move |lost| {
        let _ = lost_sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(lost);
    });
    let grow_by = |factor: f64, value: u64| ((value as f64) * factor).ceil() as u64;
    // DVL1_PROBES=BC runs only those probes (default all).
    let probes = std::env::var("DVL1_PROBES").unwrap_or_else(|_| "ABC".into());
    let skipped = (None, None);

    // A: one dispatch whose invocations run long (few, long workgroups).
    let a = if !probes.contains('A') {
        skipped
    } else {
        probe(
            "long workgroups",
            Shape {
                steps: 1 << 16,
                groups: WORDS / 64,
                rows: 1,
                dispatches: 1,
            },
            |s| Shape {
                steps: grow_by(1.25, s.steps),
                ..s
            },
            &lost_receiver,
        )
    };
    // B: one dispatch of many short workgroups (a whole-photo pass).
    let b = if !probes.contains('B') {
        skipped
    } else {
        probe(
            "many short workgroups",
            Shape {
                steps: 512,
                groups: 4096,
                rows: 8,
                dispatches: 1,
            },
            |s| Shape {
                rows: grow_by(1.25, u64::from(s.rows)) as u32,
                ..s
            },
            &lost_receiver,
        )
    };
    // C: many short dispatches in one submission (a tile's passes).
    let c = if !probes.contains('C') {
        skipped
    } else {
        probe(
            "many short dispatches",
            Shape {
                steps: 512,
                groups: 4096,
                rows: 8,
                dispatches: 4,
            },
            |s| Shape {
                dispatches: grow_by(1.25, u64::from(s.dispatches)) as u32,
                ..s
            },
            &lost_receiver,
        )
    };
    eprintln!("DVL1 summary (longest kept, first lost): A {a:?}  B {b:?}  C {c:?}");

    // The recovery: after every loss a new device on the same adapter works.
    let restart = Instant::now();
    let gpu = start();
    small(&gpu).expect("a new device after the losses failed");
    eprintln!(
        "DVL1 a new device works, started in {:.2?}",
        restart.elapsed()
    );
}
