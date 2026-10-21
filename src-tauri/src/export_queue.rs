//! The export queue of the desktop app (G44/G66): every export is a job in
//! `export-queue.sqlite` (`rawtonic_export::queue`), run one after another.
//! An export started while another runs waits behind it; a job the app quit
//! or crashed in is offered for resuming; failed photos can be retried.
//! Cancel in the Export dialog stops the running export and holds the queue
//! until the next export or resume; the queue panel pauses, cancels and
//! resumes single jobs and follows each photo (`export-item-progress`).
use crate::app_state::AppState;
use crate::export_processing::{
    ExportAdjustmentsMode, ExportSettings, ExportTarget, export_images_impl,
};
use rawtonic_export::queue::{ExportQueue, Item, ItemState, Job, JobState, RunEnd};
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

/// What a photo in progress is doing, for the queue panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// Reading and developing the RAW.
    Loading,
    /// AI Denoise (reports how many of its tiles are done).
    Denoising,
    /// Rendering, encoding and writing the file.
    Rendering,
    /// Writing the photo's separate mask files.
    Masks,
}

/// One photo's change, sent as `export-item-progress`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ItemProgress<'a> {
    job: &'a str,
    item: Item,
    stage: Option<Stage>,
    /// How far the stage is (0 … 1), when it can tell.
    fraction: Option<f32>,
}

/// The queue for the interface: its jobs and whether it is held.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub jobs: Vec<Job>,
    /// Waiting jobs do not start (after Cancel in the Export dialog or
    /// Hold queue) until an export, a resume or Start queue.
    pub held: bool,
}

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
    /// Set when the job was cancelled (on its own, or with its run).
    pub cancelled: Arc<AtomicBool>,
    /// Set by Pause: no further photo starts, those in progress finish.
    pub paused: Arc<AtomicBool>,
    app: AppHandle,
}

impl Tracking {
    pub fn wants(&self, index: usize) -> bool {
        self.pending.contains(&index)
    }
    /// Whether further photos are exported for this job (not paused or
    /// cancelled on its own while other jobs of its run go on).
    pub fn goes_on(&self) -> bool {
        !self.paused.load(Ordering::SeqCst) && !self.cancelled.load(Ordering::SeqCst)
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    /// A photo's attempt begins: its source is read next.
    pub fn began(&self, index: usize) {
        with_queue(|q| q.item_began(&self.job, index));
        self.report(index, Some(Stage::Loading), None);
    }
    pub fn started(&self, index: usize, output: &Path) {
        with_queue(|q| q.item_started(&self.job, index, output));
        self.report(index, Some(Stage::Rendering), None);
    }
    /// A photo in progress moved on to `stage`.
    pub fn stage(&self, index: usize, stage: Stage, fraction: Option<f32>) {
        self.report(index, Some(stage), fraction);
    }
    pub fn finished(&self, index: usize, result: &Result<(), String>) {
        match result {
            Ok(()) => with_queue(|q| q.item_done(&self.job, index)),
            Err(e) => with_queue(|q| q.item_failed(&self.job, index, e)),
        };
        self.report(index, None, None);
    }
    fn report(&self, index: usize, stage: Option<Stage>, fraction: Option<f32>) {
        if let Some(item) = with_queue(|q| q.item(&self.job, index)) {
            let _ = self.app.emit(
                "export-item-progress",
                ItemProgress {
                    job: &self.job,
                    item,
                    stage,
                    fraction,
                },
            );
        }
    }
}

/// AI Denoise progress of photo `index` for every job exporting it, sent
/// when the whole percent changes (a photo has a few hundred tiles at most).
pub fn denoise_progress(
    trackings: Vec<Tracking>,
    index: usize,
) -> rawtonic_rawdecode::mosaic_denoise::DenoiseProgress {
    let last = Mutex::new(None);
    Arc::new(move |done, total| {
        let percent = done * 100 / total.max(1);
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        if *last != Some(percent) {
            *last = Some(percent);
            for tracking in &trackings {
                tracking.stage(
                    index,
                    Stage::Denoising,
                    Some(done as f32 / total.max(1) as f32),
                );
            }
        }
    })
}

/// A job of the running export and its switches.
struct RunningJob {
    id: String,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

/// The jobs of the running export. The runner sets them before their export
/// task starts and clears them after it ended, both under this lock, so
/// while it names a job the active export task is that job's.
type Running = Mutex<Vec<RunningJob>>;
static CURRENT: Running = Mutex::new(Vec::new());

/// Runs `act` with `job`'s switches and whether it runs alone, if it is
/// running, holding `running` throughout so the runner cannot move on.
fn with_running<T>(
    running: &Running,
    job: &str,
    act: impl FnOnce(&RunningJob, bool) -> T,
) -> Option<T> {
    let current = running.lock().unwrap_or_else(|e| e.into_inner());
    let alone = current.len() == 1;
    current.iter().find(|j| j.id == job).map(|j| act(j, alone))
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
            // Exports left waiting do not start by themselves after a
            // restart: the queue comes up held and offers Start.
            if queue
                .jobs()
                .is_ok_and(|jobs| jobs.iter().any(|j| j.state == JobState::Queued))
            {
                hold();
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
    enqueue_all(app, vec![request]).map(|mut ids| ids.remove(0))
}

/// Adds exports to the queue together (a photo set's ticked recipes, which
/// then run as one: each photo is developed once for all of them) and
/// starts the queue if it is idle.
pub fn enqueue_all(app: &AppHandle, requests: Vec<ExportRequest>) -> Result<Vec<String>, String> {
    let requests = requests
        .iter()
        .map(|request| Ok((label_for(request), serde_json::to_value(request)?, request)))
        .collect::<Result<Vec<_>, serde_json::Error>>()
        .map_err(|e| e.to_string())?;
    // Under one lock, so the runner sees all of them or none.
    let ids = with_queue(|q| {
        requests
            .iter()
            .map(|(label, value, request)| q.enqueue(label, value, &request.paths))
            .collect::<anyhow::Result<Vec<_>>>()
    })
    .ok_or("The export queue is not available.")?;
    HOLD.store(false, Ordering::SeqCst);
    notify(app);
    kick(app);
    Ok(ids)
}

/// Whether two queued exports render the same photos with the same recipes
/// (develop settings) and so can share each photo's develop.
fn same_photos(a: &Value, b: &Value) -> bool {
    ["paths", "currentEditPath", "currentEditAdjustments"]
        .iter()
        .all(|key| a.get(key) == b.get(key))
}

/// Requeues what a failed, cancelled or interrupted job did not export; a
/// paused job continues in its place. A waiting job held by a cancel just
/// starts the queue again.
pub fn resume(app: &AppHandle, job: &str) -> Result<usize, String> {
    let state = with_queue(|q| q.job(job)).map(|j| j.state);
    let count = match state {
        Some(JobState::Queued) => 0,
        Some(JobState::Paused) => {
            with_queue(|q| q.unpause(job)).ok_or("This export cannot be continued.")?;
            0
        }
        _ => with_queue(|q| q.retry(job)).ok_or("This export cannot be resumed.")?,
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

/// Pauses a job: a running one after its photos in progress (the queue
/// then goes on), a waiting one at once.
pub fn pause(app: &AppHandle, job: &str) -> Result<(), String> {
    // Told under the lock: the job's last queue snapshot cannot come first.
    let pausing = with_running(&CURRENT, job, |running, _| {
        running.paused.store(true, Ordering::SeqCst);
        let _ = app.emit("export-job-pausing", job);
    });
    if pausing.is_some() {
        return Ok(());
    }
    with_queue(|q| q.pause(job)).ok_or("This export cannot be paused.")?;
    notify(app);
    Ok(())
}

/// Cancels one job and lets the queue go on. A running job that runs alone
/// stops at the next checkpoint; one sharing its photos' develop with other
/// recipes starts no further photo. A waiting or paused job is cancelled at
/// once. Its photos that did not export stay for a resume.
pub fn cancel(app: &AppHandle, job: &str) -> Result<(), String> {
    // Under the lock: the runner cannot move on to another export
    // meanwhile, which the cancel would otherwise stop.
    let cancelled = with_running(&CURRENT, job, |running, alone| {
        if alone {
            crate::export_processing::cancel_running_export(app)
        } else {
            running.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }
    });
    if let Some(result) = cancelled {
        return result;
    }
    with_queue(|q| q.cancel(job)).ok_or("This export cannot be cancelled.")?;
    notify(app);
    Ok(())
}

/// Called by Cancel: the queue waits until the next export or resume.
pub fn hold() {
    HOLD.store(true, Ordering::SeqCst);
}

/// Hold queue / Start queue: whether waiting jobs start; running ones are
/// left alone.
pub fn set_held(app: &AppHandle, held: bool) {
    HOLD.store(held, Ordering::SeqCst);
    notify(app);
    if !held {
        kick(app);
    }
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        jobs: with_queue(|q| q.jobs()).unwrap_or_default(),
        held: HOLD.load(Ordering::SeqCst),
    }
}

fn notify(app: &AppHandle) {
    let _ = app.emit("export-queue-changed", snapshot());
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
            let Some(jobs) =
                with_queue(|q| q.start_next_with(|a, b| same_photos(&a.request, &b.request)))
                    .filter(|jobs| !jobs.is_empty())
            else {
                break;
            };
            run_jobs(&app, &jobs).await;
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

/// Runs queued jobs over the same photos as one export (`same_photos`).
async fn run_jobs(app: &AppHandle, jobs: &[Job]) {
    let mut targets = Vec::with_capacity(jobs.len());
    let mut trackings: Vec<(&str, Tracking)> = Vec::with_capacity(jobs.len());
    let mut first: Option<ExportRequest> = None;
    for job in jobs {
        let request: ExportRequest = match serde_json::from_value(job.request.clone()) {
            Ok(request) => request,
            Err(e) => {
                log::error!("Export '{}' has an unreadable request: {e}", job.label);
                for index in 0..job.total {
                    with_queue(|q| q.item_failed(&job.id, index, "Unreadable export request"));
                }
                with_queue(|q| q.finish(&job.id, RunEnd::Completed));
                continue;
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
            paused: Arc::new(AtomicBool::new(false)),
            app: app.clone(),
        };
        trackings.push((&job.label, tracking.clone()));
        targets.push(ExportTarget {
            output_folder_or_file: request.output_folder_or_file.clone(),
            is_explicit_file_path: request.is_explicit_file_path,
            base_origin_folders: request.base_origin_folders.clone(),
            export_settings: request.export_settings.clone(),
            output_format: request.output_format.clone(),
            tracking: Some(tracking),
        });
        first.get_or_insert(request);
    }
    let Some(request) = first else {
        return;
    };
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = trackings
        .iter()
        .map(|(_, t)| RunningJob {
            id: t.job.clone(),
            paused: Arc::clone(&t.paused),
            cancelled: Arc::clone(&t.cancelled),
        })
        .collect();
    let _ = app.emit(
        "export-job-started",
        serde_json::json!({
            "id": jobs[0].id,
            "ids": trackings.iter().map(|(_, t)| &t.job).collect::<Vec<_>>(),
            "label": jobs[0].label,
            "current": trackings.iter().map(|(_, t)| t.already_done).sum::<usize>(),
            "total": request.paths.len() * trackings.len(),
        }),
    );
    notify(app);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let state = app.state::<AppState>();
    let started = export_images_impl(
        request.paths,
        targets,
        ExportAdjustmentsMode::UseSidecars {
            active_path: request.current_edit_path,
            active_adjustments: request.current_edit_adjustments,
        },
        state,
        app.clone(),
        Some(tx),
        None,
    )
    .await;
    if let Err(e) = started {
        let _ = app.emit("export-error", e.clone());
        for (label, tracking) in &trackings {
            log::error!("Export '{label}' could not start: {e}");
            for index in tracking.pending.iter() {
                with_queue(|q| q.item_failed(&tracking.job, *index, &e));
            }
        }
    } else {
        let _ = rx.await;
    }
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clear();
    // (Cancel in the Export dialog holds the queue itself; a job cancelled
    // in the queue panel lets it go on.)
    for (_, tracking) in &trackings {
        let end = if tracking.cancelled.load(Ordering::SeqCst) {
            RunEnd::Cancelled
        } else if tracking.is_paused() {
            RunEnd::Paused
        } else {
            RunEnd::Completed
        };
        with_queue(|q| q.finish(&tracking.job, end));
    }
}

#[tauri::command]
pub fn export_queue_jobs() -> Snapshot {
    snapshot()
}

/// The photos of a job with their state, output, error and timing.
#[tauri::command]
pub fn export_queue_items(job: String) -> Result<Vec<Item>, String> {
    with_queue(|q| q.items(&job)).ok_or_else(|| "This export is no longer in the queue.".into())
}

#[tauri::command]
pub fn export_queue_pause(job: String, app: AppHandle) -> Result<(), String> {
    pause(&app, &job)
}

#[tauri::command]
pub fn export_queue_cancel(job: String, app: AppHandle) -> Result<(), String> {
    cancel(&app, &job)
}

#[tauri::command]
pub fn export_queue_hold(held: bool, app: AppHandle) {
    set_held(&app, held);
}

#[tauri::command]
pub fn export_queue_resume(job: String, app: AppHandle) -> Result<usize, String> {
    resume(&app, &job)
}

#[tauri::command]
pub fn export_queue_remove(job: String, app: AppHandle) -> Result<(), String> {
    remove(&app, &job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{sync::mpsc, thread, time::Duration};

    fn running(jobs: &[&str]) -> Arc<Running> {
        Arc::new(Mutex::new(
            jobs.iter()
                .map(|id| RunningJob {
                    id: id.to_string(),
                    paused: Arc::default(),
                    cancelled: Arc::default(),
                })
                .collect(),
        ))
    }

    #[test]
    fn only_running_jobs_are_acted_on_and_know_whether_they_run_alone() {
        let current = running(&["a"]);
        assert_eq!(with_running(&current, "b", |_, alone| alone), None);
        assert_eq!(with_running(&current, "a", |_, alone| alone), Some(true));
        let group = running(&["a", "b"]);
        assert_eq!(with_running(&group, "b", |_, alone| alone), Some(false));
        current.lock().unwrap().clear();
        assert_eq!(with_running(&current, "a", |_, alone| alone), None);
    }

    #[test]
    fn the_runner_cannot_switch_jobs_while_a_cancel_acts() {
        let current = running(&["a"]);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (acting, started) = mpsc::channel();
        let canceller = {
            let (current, order) = (Arc::clone(&current), Arc::clone(&order));
            thread::spawn(move || {
                with_running(&current, "a", |_, _| {
                    acting.send(()).unwrap();
                    thread::sleep(Duration::from_millis(100));
                    order.lock().unwrap().push("cancel a");
                })
            })
        };
        started.recv().unwrap();
        // The runner finishing job a and starting job b.
        *current.lock().unwrap() = running(&["b"]).lock().unwrap().drain(..).collect();
        order.lock().unwrap().push("switch to b");
        assert!(canceller.join().unwrap().is_some());
        assert_eq!(*order.lock().unwrap(), ["cancel a", "switch to b"]);
    }

    #[test]
    fn only_exports_of_the_same_photos_and_edits_share_a_develop() {
        let web = json!({"paths": ["/a.NEF", "/b.NEF"], "outputFormat": "jpg",
            "currentEditPath": "/a.NEF", "currentEditAdjustments": {"exposure": 1}});
        let mut print = web.clone();
        print["outputFormat"] = json!("tiff");
        assert!(same_photos(&web, &print));
        let mut fewer = print.clone();
        fewer["paths"] = json!(["/a.NEF"]);
        assert!(!same_photos(&web, &fewer));
        let mut edited = print.clone();
        edited["currentEditAdjustments"] = json!({"exposure": 2});
        assert!(
            !same_photos(&web, &edited),
            "the open photo's unsaved edit differs"
        );
    }
}
