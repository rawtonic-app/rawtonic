//! How many photos a batch export renders at once, for the app's export
//! queue and the CLI alike, and the thread pools they run in.

/// Export workers at most. The GPU renders one photo at a time and is the
/// slowest stage; four keep it busy while they load, upload and encode the
/// other photos. Measured on 20 RAWs with two recipes (M5 Pro): 4 workers
/// 23.5 s, 6 23.7 s, 8 26.1 s; with one render for both recipes 4 workers
/// 16.4 s, 6 18.6 s, 8 19.1 s (notes: parity-gaps/G43, F10).
pub const MAX_EXPORT_WORKERS: usize = 4;
/// Memory an export worker takes, measured: four workers on 24–45 MP RAWs
/// with two recipes peaked 7.8 GB above the app's footprint before the
/// export.
pub const EXPORT_WORKER_BYTES: u64 = 2 << 30;

/// The workers for `photos` photos on `cores` cores with `available` bytes
/// of free memory: one for a single photo, else as many as the cores and
/// the memory allow, at least one and at most `MAX_EXPORT_WORKERS`.
pub fn export_workers(photos: usize, cores: usize, available: u64) -> usize {
    if photos <= 1 {
        return 1;
    }
    let by_memory = usize::try_from(available / EXPORT_WORKER_BYTES).unwrap_or(usize::MAX);
    cores.min(by_memory).clamp(1, MAX_EXPORT_WORKERS)
}

/// The available cores, 1 when unknown.
pub fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// One thread pool per worker, never one shared: a pool thread holding the
/// GPU processor that waits for its parallel work may meanwhile run other
/// jobs of its pool, which would be another photo waiting for that
/// processor. With a pool of its own no job a photo waits for waits for it.
/// Threads have the render stack (`RENDER_STACK_SIZE`) and are marked as a
/// private pool's (`mark_private_pool_thread`), so their renders keep their
/// parallel work in it. `start` runs on each new thread after that (the app
/// lowers their priority there).
pub fn lane_pools(
    workers: usize,
    threads: usize,
    name: &'static str,
    start: impl Fn() + Send + Sync + Clone + 'static,
) -> Vec<rayon::ThreadPool> {
    (0..workers)
        .map(|lane| {
            let start = start.clone();
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .stack_size(rawtonic_runtime::RENDER_STACK_SIZE)
                .thread_name(move |i| format!("{name}-{lane}-{i}"))
                .start_handler(move |_| {
                    rawtonic_gpu::gpu_processing::mark_private_pool_thread();
                    start();
                })
                .build()
                .expect("export thread pool")
        })
        .collect()
}

/// `run(i)` for every `i` in `0..jobs`, `workers` at a time, each in a
/// lane pool of its own (`lane_pools`); the results in job order.
pub fn run_in_lanes<R: Send>(
    pools: &[rayon::ThreadPool],
    jobs: usize,
    run: impl Fn(usize) -> R + Sync,
) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<R>>> =
        (0..jobs).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for pool in pools {
            let (next, results, run) = (&next, &results, &run);
            std::thread::Builder::new()
                .stack_size(rawtonic_runtime::RENDER_STACK_SIZE)
                .spawn_scoped(scope, move || {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= jobs {
                            break;
                        }
                        let result = pool.install(|| run(i));
                        *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                    }
                })
                .expect("start an export worker");
        }
    });
    results
        .into_iter()
        .map(|r| {
            r.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .expect("every job ran")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_follow_photos_cores_and_memory() {
        let gb = 1u64 << 30;
        assert_eq!(export_workers(1, 12, 64 * gb), 1);
        assert_eq!(export_workers(20, 12, 64 * gb), 4);
        assert_eq!(export_workers(20, 2, 64 * gb), 2);
        assert_eq!(export_workers(20, 12, 5 * gb), 2);
        assert_eq!(export_workers(20, 12, gb), 1);
    }

    #[test]
    fn every_job_runs_once_and_results_keep_their_order() {
        let pools = lane_pools(3, 2, "test-lane", || {});
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let results = run_in_lanes(&pools, 50, |i| {
            ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Parallel work inside a job stays in its lane's pool.
            use rayon::prelude::*;
            (0..100).into_par_iter().map(|k| k * i).sum::<usize>()
        });
        assert_eq!(ran.into_inner(), 50);
        assert_eq!(results, (0..50).map(|i| 4950 * i).collect::<Vec<_>>());
        assert!(run_in_lanes(&pools, 0, |i| i).is_empty());
    }
}
