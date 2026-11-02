//! SCRATCH ONLY (task BX6): GPU time per compute pass from timestamp queries.
//! Enabled with RAWTONIC_GPU_TIMER=<file>; totals per label are written there.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Timer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    period: f32,
    pending: Mutex<Vec<(Instant, String, (usize, u32))>>,
    sets: Vec<wgpu::QuerySet>,
    cursor: std::sync::atomic::AtomicUsize,
    totals: Arc<Mutex<HashMap<String, (u64, f64)>>>,
    spans: Arc<Mutex<Vec<(u64, u64, String)>>>,
    path: String,
}

static TIMER: OnceLock<Option<Timer>> = OnceLock::new();

pub fn register(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
    let Ok(path) = std::env::var("RAWTONIC_GPU_TIMER") else {
        let _ = TIMER.set(None);
        return;
    };
    if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        eprintln!("gpu_timer: no TIMESTAMP_QUERY");
        let _ = TIMER.set(None);
        return;
    }
    let period = queue.get_timestamp_period();
    let sets = (0..4)
        .map(|_| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: 4096,
            })
        })
        .collect();
    let _ = TIMER.set(Some(Timer {
        sets,
        cursor: Default::default(),
        device,
        queue,
        period,
        pending: Mutex::new(Vec::new()),
        totals: Arc::new(Mutex::new(HashMap::new())),
        spans: Arc::new(Mutex::new(Vec::new())),
        path,
    }));
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            flush(Duration::from_millis(1500));
        }
    });
}

pub fn wanted() -> bool {
    std::env::var("RAWTONIC_GPU_TIMER").is_ok()
}

fn timer() -> Option<&'static Timer> {
    TIMER.get().and_then(|t| t.as_ref())
}

/// A compute pass descriptor that times the pass under `label` (+ caller line).
#[track_caller]
pub fn pass(label: &str) -> wgpu::ComputePassDescriptor<'static> {
    let loc = std::panic::Location::caller();
    let name = format!("{label}@{}:{}", loc.file().rsplit('/').next().unwrap_or(""), loc.line());
    let Some(t) = timer() else {
        return Default::default();
    };
    let slot = t.cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % (4 * 2048);
    let (set, index) = (slot / 2048, (slot % 2048) as u32 * 2);
    let qs: &'static wgpu::QuerySet = &t.sets[set];
    t.pending
        .lock()
        .unwrap()
        .push((Instant::now(), name, (set, index)));
    wgpu::ComputePassDescriptor {
        label: None,
        timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
            query_set: qs,
            beginning_of_pass_write_index: Some(index),
            end_of_pass_write_index: Some(index + 1),
        }),
    }
}

/// Resolves the passes encoded more than `age` ago.
pub fn flush(age: Duration) {
    let Some(t) = timer() else { return };
    let now = Instant::now();
    let ready: Vec<_> = {
        let mut pending = t.pending.lock().unwrap();
        let (ready, keep): (Vec<_>, Vec<_>) = pending
            .drain(..)
            .partition(|(at, _, _)| now.duration_since(*at) >= age);
        *pending = keep;
        ready
    };
    if ready.is_empty() {
        write(t);
        return;
    }
    let stride = 256u64;
    let size = stride * ready.len() as u64;
    let resolve = t.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = Arc::new(t.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    let mut encoder = t.device.create_command_encoder(&Default::default());
    for (i, (_, _, (set, index))) in ready.iter().enumerate() {
        encoder.resolve_query_set(&t.sets[*set], *index..*index + 2, &resolve, i as u64 * stride);
    }
    encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, size);
    t.queue.submit(Some(encoder.finish()));
    let names: Vec<String> = ready.into_iter().map(|(_, n, _)| n).collect();
    let totals = t.totals.clone();
    let spans = t.spans.clone();
    let period = t.period as f64;
    let buffer = read.clone();
    read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        if result.is_err() {
            return;
        }
        {
            let data = buffer.slice(..).get_mapped_range();
            let mut totals = totals.lock().unwrap();
            let mut spans = spans.lock().unwrap();
            for (i, name) in names.iter().enumerate() {
                let o = i * 256;
                let a = u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
                let b = u64::from_le_bytes(data[o + 8..o + 16].try_into().unwrap());
                if b > a && a > 0 {
                    let entry = totals.entry(name.clone()).or_insert((0, 0.0));
                    entry.0 += 1;
                    entry.1 += (b - a) as f64 * period / 1e6;
                    spans.push((a, b, name.clone()));
                }
            }
        }
        buffer.unmap();
    });
    write(t);
}

fn write(t: &Timer) {
    let period = t.period as f64 / 1e6;
    let totals = t.totals.lock().unwrap();
    let mut rows: Vec<_> = totals.iter().collect();
    rows.sort_by(|a, b| b.1.1.partial_cmp(&a.1.1).unwrap());
    let mut spans = t.spans.lock().unwrap().clone();
    spans.sort_by_key(|s| s.0);
    // Exclusive attribution: each instant's busy time split between the passes running then.
    let mut events: Vec<(u64, i32, usize)> = Vec::new();
    for (i, (a, b, _)) in spans.iter().enumerate() {
        events.push((*a, 1, i));
        events.push((*b, -1, i));
    }
    events.sort();
    let mut active: std::collections::HashSet<usize> = Default::default();
    let mut share: HashMap<String, f64> = HashMap::new();
    let mut gaps: HashMap<String, (u64, f64)> = HashMap::new();
    let mut last = 0u64;
    let mut last_ended: Option<usize> = None;
    let mut busy = 0f64;
    let mut idle = 0f64;
    for (time, kind, i) in events {
        if !active.is_empty() && time > last {
            let d = (time - last) as f64 * period;
            busy += d;
            for &j in &active {
                *share.entry(spans[j].2.clone()).or_default() += d / active.len() as f64;
            }
        } else if active.is_empty() && kind == 1 && last > 0 && time > last {
            let d = (time - last) as f64 * period;
            idle += d;
            let short = |n: &str| n.split('@').next().unwrap_or("").to_string();
            let key = format!("{} -> {}", last_ended.map(|j| short(&spans[j].2)).unwrap_or_default(), short(&spans[i].2));
            let e = gaps.entry(key).or_default();
            e.0 += 1;
            e.1 += d;
        }
        last = time;
        if kind == 1 {
            active.insert(i);
        } else {
            active.remove(&i);
            last_ended = Some(i);
        }
    }
    let mut out = format!("# busy_ms {busy:.1} idle_between_ms {idle:.1}\n# exclusive_ms   raw_ms   count  pass\n");
    for (name, (count, ms)) in rows {
        out.push_str(&format!("{:10.1} {ms:10.1} {count:7} {name}\n", share.get(name).copied().unwrap_or(0.0)));
    }
    let mut g: Vec<_> = gaps.into_iter().collect();
    g.sort_by(|a, b| b.1.1.partial_cmp(&a.1.1).unwrap());
    out.push_str("# idle gaps: ms count prev -> next\n");
    for (k, (n, ms)) in g.into_iter().take(15) {
        out.push_str(&format!("{ms:10.1} {n:7} {k}\n"));
    }
    let _ = std::fs::write(&t.path, out);
    let p = period * 1.0;
    let dump: String = spans.iter().map(|(a, b, n)| format!("{:.4} {:.4} {n}\n", *a as f64 * p, *b as f64 * p)).collect();
    let _ = std::fs::write(format!("{}.spans", t.path), dump);
}

/// Waits until every pass so far is resolved and written.
pub fn finish() {
    if timer().is_none() {
        return;
    }
    std::thread::sleep(Duration::from_millis(300));
    flush(Duration::ZERO);
    if let Some(t) = timer() {
        let _ = t.device.poll(wgpu::PollType::wait_indefinitely());
    }
    std::thread::sleep(Duration::from_millis(300));
    if let Some(t) = timer() {
        write(t);
    }
}
