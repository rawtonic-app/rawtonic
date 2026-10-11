//! The export queue of the desktop app (G44/G66): every export is a job in
//! `export-queue.sqlite` (`rawtonic_export::queue`), run one after another.
//! An export started while another runs waits behind it; a job the app quit
//! or crashed in is offered for resuming; failed photos can be retried.
//! Cancelling stops the running job and holds the queue until the next
//! export or resume.
use crate::app_state::AppState;
use crate::export_processing::{ExportAdjustmentsMode, ExportSettings, export_images_impl};
use rawtonic_export::queue::{ExportQueue, ItemState, Job, JobState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Emitter, Manager};

/// Finished jobs kept for the queue list.
const KEEP_DONE: usize = 20;

/// What `export_images` was asked, stored with the job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub paths: Vec<String>,
    pub output_folder_or_file: String,
    pub is_explicit_file_path: bool,
    pub base_origin_folders: Vec<String>,
    pub export_settings: ExportSettings,
    pub output_format: String,
    pub current_edit_path: Option<String>,
    pub current_edit_adjustments: Option<Value>,
}

/// Which photos of a queued job to render and where their state goes.
#[derive(Clone)]
pub struct Tracking {
    pub job: String,
    /// Indices still to export; the others finished in an earlier run.
    pub pending: Arc<HashSet<usize>>,
    pub already_done: usize,
    /// Set when the run ended by cancellation.
    pub cancelled: Arc<AtomicBool>,
}

impl Tracking {
    pub fn wants(&self, index: usize) -> bool {
        self.pending.contains(&index)
    }
    pub fn started(&self, index: usize, output: &Path) {
        with_queue(|q| q.item_started(&self.job, index, output));
    }
    pub fn finished(&self, index: usize, result: &Result<(), String>) {
        match result {
            Ok(()) => with_queue(|q| q.item_done(&self.job, index)),
            Err(e) => with_queue(|q| q.item_failed(&self.job, index, e)),
        };
    }
}

static QUEUE: Mutex<Option<ExportQueue>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static HOLD: AtomicBool = AtomicBool::new(false);

/// Runs `f` on the open queue; logs and returns `None` on errors.
fn with_queue<T>(f: impl FnOnce(&ExportQueue) -> anyhow::Result<T>) -> Option<T> {
    let guard = QUEUE.lock().ok()?;
    let queue = guard.as_ref()?;
    match f(queue) {
        Ok(value) => Some(value),
        Err(e) => {
            log::warn!("Export queue: {e:#}");
            None
        }
    }
}

/// Opens the queue at start-up and marks jobs a quit or crash cut short as
/// interrupted (photos already written count as finished).
pub fn open(app: &AppHandle) {
    let path = match app.path().app_data_dir() {
        Ok(dir) => dir.join("export-queue.sqlite"),
        Err(e) => {
            log::error!("Export queue: no app data folder: {e}");
            return;
        }
    };
    match ExportQueue::open(&path) {
        Ok(queue) => {
            match queue.recover() {
                Ok(jobs) => {
                    for job in jobs {
                        log::warn!(
                            "Export '{}' was interrupted: {} of {} photos exported",
                            job.label,
                            job.done,
                            job.total
                        );
                    }
                }
                Err(e) => log::error!("Export queue recovery failed: {e:#}"),
            }
            if let Ok(mut guard) = QUEUE.lock() {
                *guard = Some(queue);
            }
        }
        Err(e) => log::error!("Export queue unavailable: {e:#}"),
    }
}

fn label_for(request: &ExportRequest) -> String {
    let n = request.paths.len();
    let format = request.output_format.to_uppercase();
    if n == 1 {
        let name = request.paths[0]
            .split("?vc=")
            .next()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{name} as {format}")
    } else {
        format!("{n} photos as {format}")
    }
}

/// Adds an export to the queue and starts the queue if it is idle.
pub fn enqueue(app: &AppHandle, request: ExportRequest) -> Result<String, String> {
    let value = serde_json::to_value(&request).map_err(|e| e.to_string())?;
    let label = label_for(&request);
    let id = with_queue(|q| q.enqueue(&label, &value, &request.paths))
        .ok_or("The export queue is not available.")?;
    HOLD.store(false, Ordering::SeqCst);
    notify(app);
    kick(app);
    Ok(id)
}

/// Requeues what a failed, cancelled or interrupted job did not export.
/// A waiting job held by a cancel just starts the queue again.
pub fn resume(app: &AppHandle, job: &str) -> Result<usize, String> {
    let waiting = with_queue(|q| q.job(job)).is_some_and(|j| j.state == JobState::Queued);
    let count = if waiting {
        0
    } else {
        with_queue(|q| q.retry(job)).ok_or("This export cannot be resumed.")?
    };
    HOLD.store(false, Ordering::SeqCst);
    notify(app);
    kick(app);
    Ok(count)
}

pub fn remove(app: &AppHandle, job: &str) -> Result<(), String> {
    with_queue(|q| q.remove(job)).ok_or("This export cannot be removed.")?;
    notify(app);
    Ok(())
}

/// Called by Cancel: the queue waits until the next export or resume.
pub fn hold() {
    HOLD.store(true, Ordering::SeqCst);
}

pub fn jobs() -> Vec<Job> {
    with_queue(|q| q.jobs()).unwrap_or_default()
}

fn notify(app: &AppHandle) {
    let _ = app.emit("export-queue-changed", jobs());
}

/// Starts the runner unless it is already working through the queue.
fn kick(app: &AppHandle) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if HOLD.load(Ordering::SeqCst) {
                break;
            }
            let Some(Some(job)) = with_queue(|q| q.start_next()) else {
                break;
            };
            run_job(&app, &job).await;
            with_queue(|q| q.prune_done(KEEP_DONE));
            notify(&app);
        }
        RUNNING.store(false, Ordering::SeqCst);
        // An export queued while the runner was finishing.
        let queued = with_queue(|q| q.jobs())
            .unwrap_or_default()
            .iter()
            .any(|j| j.state == JobState::Queued);
        if queued && !HOLD.load(Ordering::SeqCst) {
            kick(&app);
        }
    });
}

async fn run_job(app: &AppHandle, job: &Job) {
    let request: ExportRequest = match serde_json::from_value(job.request.clone()) {
        Ok(request) => request,
        Err(e) => {
            log::error!("Export '{}' has an unreadable request: {e}", job.label);
            for index in 0..job.total {
                with_queue(|q| q.item_failed(&job.id, index, "Unreadable export request"));
            }
            with_queue(|q| q.finish(&job.id, false));
            return;
        }
    };
    let items = with_queue(|q| q.items(&job.id)).unwrap_or_default();
    let pending: HashSet<usize> = items
        .iter()
        .filter(|i| i.state != ItemState::Done)
        .map(|i| i.index)
        .collect();
    let tracking = Tracking {
        job: job.id.clone(),
        already_done: items.len() - pending.len(),
        pending: Arc::new(pending),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let _ = app.emit(
        "export-job-started",
        serde_json::json!({
            "id": job.id,
            "label": job.label,
            "current": tracking.already_done,
            "total": job.total,
        }),
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    let state = app.state::<AppState>();
    let started = export_images_impl(
        request.paths,
        request.output_folder_or_file,
        request.is_explicit_file_path,
        request.base_origin_folders,
        request.export_settings,
        request.output_format,
        ExportAdjustmentsMode::UseSidecars {
            active_path: request.current_edit_path,
            active_adjustments: request.current_edit_adjustments,
        },
        state,
        app.clone(),
        Some(tx),
        None,
        Some(tracking.clone()),
    )
    .await;
    if let Err(e) = started {
        log::error!("Export '{}' could not start: {e}", job.label);
        let _ = app.emit("export-error", e.clone());
        for index in tracking.pending.iter() {
            with_queue(|q| q.item_failed(&job.id, *index, &e));
        }
    } else {
        let _ = rx.await;
    }
    let cancelled = tracking.cancelled.load(Ordering::SeqCst);
    if cancelled {
        hold();
    }
    with_queue(|q| q.finish(&job.id, cancelled));
}

#[tauri::command]
pub fn export_queue_jobs() -> Vec<Job> {
    jobs()
}

#[tauri::command]
pub fn export_queue_resume(job: String, app: AppHandle) -> Result<usize, String> {
    resume(&app, &job)
}

#[tauri::command]
pub fn export_queue_remove(job: String, app: AppHandle) -> Result<(), String> {
    remove(&app, &job)
}
