// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool: times Core ML model loads in one process, step by step, to see
//! which part of a load is paid once per process (macOS).
//!
//! `cargo run --release -p rawtonic-denoise --example coreml_load_times -- STEP…`
//! Steps, in order:
//! - `load:PACKAGE` loads a `.mlpackage` (its compiled `.mlmodelc` beside it)
//!   on the CPU and GPU and keeps it; `drop` drops every loaded model;
//! - `prefetch:PACKAGE` starts reading the package's program in Core ML's
//!   device cache (`prefetch_device_cache`) and goes on; `join` waits for it;
//! - `metal` creates the system's Metal device;
//! - `qos:interactive|initiated|default|utility|background` sets this
//!   thread's quality of service;
//! - `thread:STEP` runs one step on a new thread (as the app's blocking pool);
//! - `sleep:MS`.

#[cfg(target_os = "macos")]
fn main() {
    use rawtonic_denoise::coreml::{ComputeUnits, CoreMlModel};
    use std::path::PathBuf;
    use std::time::Instant;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> *mut std::ffi::c_void;
    }
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
    }

    type Prefetch = Option<std::thread::JoinHandle<(u64, f64)>>;
    static PREFETCH: std::sync::Mutex<Prefetch> = std::sync::Mutex::new(None);

    fn step(arg: &str, models: &mut Vec<CoreMlModel>, origin: Instant) {
        let start = Instant::now();
        if let Some(path) = arg.strip_prefix("prefetch:") {
            *PREFETCH.lock().unwrap() =
                rawtonic_denoise::coreml::prefetch_device_cache(&PathBuf::from(path));
        } else if arg == "join" {
            if let Some(handle) = PREFETCH.lock().unwrap().take() {
                let (bytes, seconds) = handle.join().unwrap();
                println!(
                    "prefetched {:.0} MB in {:.0} ms",
                    bytes as f64 / 1e6,
                    seconds * 1e3
                );
            } else {
                println!("nothing prefetched");
            }
        } else if let Some(path) = arg.strip_prefix("load:") {
            models.push(CoreMlModel::load(&PathBuf::from(path), ComputeUnits::CpuAndGpu).unwrap());
        } else if arg == "drop" {
            models.clear();
        } else if arg == "metal" {
            assert!(!unsafe { MTLCreateSystemDefaultDevice() }.is_null());
        } else if let Some(qos) = arg.strip_prefix("qos:") {
            let class = match qos {
                "interactive" => 0x21,
                "initiated" => 0x19,
                "default" => 0x15,
                "utility" => 0x11,
                "background" => 0x09,
                _ => panic!("qos {qos}"),
            };
            assert_eq!(unsafe { pthread_set_qos_class_self_np(class, 0) }, 0);
        } else if let Some(inner) = arg.strip_prefix("thread:") {
            let inner = inner.to_string();
            let mut moved = std::mem::take(models);
            moved = std::thread::spawn(move || {
                step(&inner, &mut moved, origin);
                moved
            })
            .join()
            .unwrap();
            *models = moved;
            return;
        } else if let Some(ms) = arg.strip_prefix("sleep:") {
            std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap()));
        } else {
            panic!("unknown step {arg}");
        }
        println!(
            "{:>7.1} ms  {arg}   (at {:.0} ms)",
            start.elapsed().as_secs_f64() * 1e3,
            origin.elapsed().as_secs_f64() * 1e3
        );
    }

    let origin = Instant::now();
    let mut models = Vec::new();
    for arg in std::env::args().skip(1) {
        step(&arg, &mut models, origin);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Core ML runs on macOS only");
}
