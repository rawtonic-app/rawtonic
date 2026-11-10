// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deliberate crashes for testing the crash records and the GPU start guard
//! (task WSF), only in QA builds (`qa-control`; never in release bundles):
//! `RAWTONIC_TEST_CRASH=<kind>@<point>` ends the app at that point of its
//! start. Kinds: `access-violation`, `stack-overflow`, `abort`, `panic`.
//! Points: `start` (before Tauri builds the app, the crash records armed
//! early), `setup` (before the log exists), `gpu-adapter`, `gpu-device`,
//! `gpu-compile` (the GPU start's steps, its flag written).
//! `RAWTONIC_TEST_GPU_OOM=<n>` fails export renders out of graphics memory
//! (`gpu_out_of_memory`).

/// Crashes here when `RAWTONIC_TEST_CRASH` names this point.
pub(crate) fn at(point: &str) {
    let Ok(request) = std::env::var("RAWTONIC_TEST_CRASH") else {
        return;
    };
    let Some((kind, wanted)) = request.split_once('@') else {
        return;
    };
    if wanted != point {
        return;
    }
    eprintln!("RAWTONIC_TEST_CRASH: {kind} at {point}");
    crash(kind);
}

fn crash(kind: &str) {
    match kind {
        "access-violation" => {
            // SAFETY: none; a write to the first page faults on purpose
            // (address 16: a null pointer is a checked precondition).
            unsafe { std::ptr::write_volatile(std::ptr::without_provenance_mut::<u64>(16), 1) };
        }
        "stack-overflow" => {
            #[allow(unconditional_recursion)]
            fn deeper(n: u64) -> u64 {
                let pad = [n; 512];
                std::hint::black_box(&pad);
                deeper(n + 1) + pad[7]
            }
            std::hint::black_box(deeper(0));
        }
        "abort" => std::process::abort(),
        "panic" => panic!("RAWTONIC_TEST_CRASH panic"),
        _ => {}
    }
}

/// `RAWTONIC_TEST_GPU_OOM=<n>`: the first `n` export renders of the process
/// fail as a graphics card out of memory does, after their files are
/// planned (tests of the export's retry: a retried photo must come out as
/// it does without the fault, under the same name).
pub(crate) fn gpu_out_of_memory() -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static FAILED: AtomicU64 = AtomicU64::new(0);
    let Some(wanted) = std::env::var("RAWTONIC_TEST_GPU_OOM")
        .ok()
        .and_then(|n| n.parse::<u64>().ok())
    else {
        return Ok(());
    };
    if FAILED.fetch_add(1, Ordering::SeqCst) < wanted {
        eprintln!("RAWTONIC_TEST_GPU_OOM: an export render fails out of memory");
        return Err(rawtonic_runtime::gpu_memory::gpu_out_of_memory(
            "failure injected by a test",
        ));
    }
    Ok(())
}
