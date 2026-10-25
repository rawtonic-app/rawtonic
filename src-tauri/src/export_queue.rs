//! The export queue of the desktop app (G44/G66): every export is a job in
//! `export-queue.sqlite` (`rawtonic_export::queue`), run one after another.
//! An export started while another runs waits behind it; a job the app quit
//! or crashed in is offered for resuming; failed photos can be retried.
//! Cancel in the Export dialog stops the running export and holds the queue
//! until the next export or resume; the queue panel pauses, cancels and
//! resumes single jobs and follows each photo (`export-item-progress`).
use crate::app_state::AppState;
use crate::export_processing::{
    ExportPhotos, ExportSettings, ExportTarget, PinnedPhoto, denoise_models, export_images_impl,
    pin_photos,
};
use rawtonic_export::queue::{ExportQueue, Item, ItemState, Job, JobState, RunEnd};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
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
    #[serde(default)]
    pub display_label: Option<String>,
    pub output_folder_or_file: String,
    pub is_explicit_file_path: bool,
    pub base_origin_folders: Vec<String>,
    pub export_settings: ExportSettings,
    pub output_format: String,
    pub current_edit_path: Option<String>,
    pub current_edit_adjustments: Option<Value>,
    /// The session or catalog the export was submitted from; it owns the
    /// photos outside any session (`culling_library_for`).
    #[serde(default)]
    pub library: Option<String>,
    /// Set once each photo's recipe and metadata were read (`pin`); they
    /// are kept with the job's photos in the queue (`item_snapshot`), and
    /// the job renders only them, whichever library is open when it runs.
    /// None only before `pin`, and in jobs an older build queued.
    #[serde(default)]
    pub pinned: Option<Pinned>,
}

/// What a request's pinned photos amount to, kept in the request itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pinned {
    /// Of all the photos' snapshots: jobs with equal ones share a develop.
    pub digest: String,
    /// The AI Denoise models the recipes use, loaded before the job runs.
    pub denoise_models: BTreeSet<usize>,
}

impl ExportRequest {
    /// Reads what each photo is exported with, once, through the library
    /// the export was submitted from (`pin_photos`), and returns it.
    fn pin(&mut self) -> Result<Vec<PinnedPhoto>, String> {
        let photos = pin_photos(
            &self.paths,
            self.current_edit_path.as_deref(),
            self.current_edit_adjustments.as_ref(),
            self.library.as_deref().map(Path::new),
        );
        let mut digest = blake3::Hasher::new();
        for photo in &photos {
            digest.update(&serde_json::to_vec(photo).map_err(|e| e.to_string())?);
        }
        self.pinned = Some(Pinned {
            digest: digest.finalize().to_hex().to_string(),
            denoise_models: denoise_models(photos.iter().map(|photo| &photo.adjustments)),
        });
        Ok(photos)
    }

    /// Whether `other` reads its photos as this one does (`pin`).
    fn pins_like(&self, other: &ExportRequest) -> bool {
        self.paths == other.paths
            && self.current_edit_path == other.current_edit_path
            && self.current_edit_adjustments == other.current_edit_adjustments
            && self.library == other.library
    }
}

/// Pins `requests` (`ExportRequest::pin`) on a blocking worker: each
/// photo's recipe and metadata are read from its library there, once for
/// requests that read their photos alike (a photo set's recipes). Returns
/// each request with its photos' snapshots (JSON).
async fn pin_all(
    requests: Vec<ExportRequest>,
) -> Result<Vec<(ExportRequest, Arc<Vec<String>>)>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut pinned: Vec<(ExportRequest, Arc<Vec<String>>)> = Vec::new();
        for mut request in requests {
            if let Some((earlier, snapshots)) = pinned
                .iter()
                .find(|(earlier, _)| earlier.pins_like(&request))
            {
                request.pinned.clone_from(&earlier.pinned);
                let snapshots = Arc::clone(snapshots);
                pinned.push((request, snapshots));
                continue;
            }
            let snapshots = request
                .pin()?
                .iter()
                .map(|photo| serde_json::to_string(photo).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            pinned.push((request, Arc::new(snapshots)));
        }
        Ok(pinned)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The photos of a run of `jobs` (a group sharing its develop): each read
/// from the snapshot it was queued with (`read`: a job's photo's snapshot,
/// None once exported there), of the first job still to export it.
fn queued_photos(
    request: &ExportRequest,
    jobs: Vec<String>,
    read: impl Fn(&str, usize) -> Option<String> + Send + Sync + 'static,
) -> ExportPhotos {
    ExportPhotos {
        open_path: request.current_edit_path.clone(),
        denoise_models: request
            .pinned
            .as_ref()
            .map(|pinned| pinned.denoise_models.clone())
            .unwrap_or_default(),
        read: Arc::new(move |index| {
            let snapshot = jobs
                .iter()
                .find_map(|job| read(job, index))
                .ok_or("This photo's recipe was not kept with the export")?;
            serde_json::from_str(&snapshot).map_err(|e| e.to_string())
        }),
    }
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
    /// Its photos in progress, so a job stopped on its own settles once
    /// they are done while the other jobs of its run go on.
    run: Arc<RunCount>,
    app: AppHandle,
}

/// Photos of a job in progress, and whether the job was settled. One lock
/// for both, so a photo is admitted only into a job that has not settled,
/// and a job never settles while a photo admitted into it is still to come.
#[derive(Default)]
struct RunCount(Mutex<RunState>);

#[derive(Default)]
struct RunState {
    in_flight: usize,
    settled: bool,
}

impl RunCount {
    fn state(&self) -> std::sync::MutexGuard<'_, RunState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Reserves a photo for a job that goes on and has not settled.
    fn admit(&self, paused: &AtomicBool, cancelled: &AtomicBool) -> bool {
        let mut state = self.state();
        if state.settled || paused.load(Ordering::SeqCst) || cancelled.load(Ordering::SeqCst) {
            return false;
        }
        state.in_flight += 1;
        true
    }
    /// An admitted photo is done.
    fn release(&self) {
        let mut state = self.state();
        state.in_flight = state.in_flight.saturating_sub(1);
    }
    /// Marks the job settled; false if it was already.
    fn claim_settlement(&self) -> bool {
        !std::mem::replace(&mut self.state().settled, true)
    }
}

/// How a job stopped on its own ends, once its last photo in progress is
/// done; `None` while it goes on, has photos in progress or was settled.
fn settle_now(paused: &AtomicBool, cancelled: &AtomicBool, run: &RunCount) -> Option<RunEnd> {
    let mut state = run.state();
    let cancelled = cancelled.load(Ordering::SeqCst);
    if !(cancelled || paused.load(Ordering::SeqCst)) || state.in_flight != 0 || state.settled {
        return None;
    }
    state.settled = true;
    Some(if cancelled {
        RunEnd::Cancelled
    } else {
        RunEnd::Paused
    })
}

/// Ends a job stopped on its own (see `settle_now`): it leaves the running
/// jobs and can be resumed while the others of its run go on.
fn settle(app: &AppHandle, job: &str, paused: &AtomicBool, cancelled: &AtomicBool, run: &RunCount) {
    if let Some(end) = settle_now(paused, cancelled, run) {
        with_queue(|q| q.finish(job, end));
        CURRENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|running| running.id != job);
        notify(app);
    }
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
    /// Reserves photo export for this job: false once it is paused,
    /// cancelled on its own or settled. An admitted photo must end with
    /// `finished` (or with the run being cancelled as a whole).
    pub fn admit(&self) -> bool {
        self.run.admit(&self.paused, &self.cancelled)
    }
    /// An admitted photo's attempt begins: its source is read next.
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
        self.run.release();
        settle(
            &self.app,
            &self.job,
            &self.paused,
            &self.cancelled,
            &self.run,
        );
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
    run: Arc<RunCount>,
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
    let source = if n == 1 {
        let name = request.paths[0]
            .split("?vc=")
            .next()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{name} as {format}")
    } else {
        format!("{n} photos as {format}")
    };
    match &request.display_label {
        Some(label) if !label.is_empty() => label.clone(),
        _ => source,
    }
}

/// Adds an export to the queue and starts the queue if it is idle.
pub async fn enqueue(app: &AppHandle, request: ExportRequest) -> Result<String, String> {
    enqueue_all(app, vec![request])
        .await
        .map(|mut ids| ids.remove(0))
}

/// Adds exports to the queue together (a photo set's ticked recipes, which
/// then run as one: each photo is developed once for all of them) and
/// starts the queue if it is idle. Each is pinned first: from here on the
/// export depends on no library being open.
pub async fn enqueue_all(
    app: &AppHandle,
    requests: Vec<ExportRequest>,
) -> Result<Vec<String>, String> {
    let requests = pin_all(requests).await?;
    let requests = requests
        .iter()
        .map(|(request, snapshots)| {
            Ok((
                label_for(request),
                serde_json::to_value(request)?,
                request,
                snapshots,
            ))
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()
        .map_err(|e| e.to_string())?;
    // Under one lock and in one transaction: the runner sees all of them
    // or none, and a failure queues none.
    let jobs: Vec<_> = requests
        .iter()
        .map(|(label, value, request, snapshots)| {
            (
                label.as_str(),
                value,
                request.paths.as_slice(),
                snapshots.as_slice(),
            )
        })
        .collect();
    let ids =
        with_queue(|q| q.enqueue_all_with(&jobs)).ok_or("The export queue is not available.")?;
    HOLD.store(false, Ordering::SeqCst);
    notify(app);
    kick(app);
    Ok(ids)
}

/// Whether two queued exports render the same photos with the same recipes
/// (develop settings) and so can share each photo's develop.
fn same_photos(a: &Value, b: &Value) -> bool {
    [
        "paths",
        "currentEditPath",
        "currentEditAdjustments",
        "library",
        "pinned",
    ]
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
        (
            Arc::clone(&running.paused),
            Arc::clone(&running.cancelled),
            Arc::clone(&running.run),
        )
    });
    if let Some((paused, cancelled, run)) = pausing {
        // No photo of it in progress: it ends now (outside the lock).
        settle(app, job, &paused, &cancelled, &run);
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
        // Recorded first: also a run that stops before its first photo
        // ends the job as cancelled.
        running.cancelled.store(true, Ordering::SeqCst);
        let switches = (
            Arc::clone(&running.paused),
            Arc::clone(&running.cancelled),
            Arc::clone(&running.run),
        );
        if alone {
            crate::export_processing::cancel_running_export(app).map(|()| None)
        } else {
            Ok(Some(switches))
        }
    });
    if let Some(result) = cancelled {
        if let Some((paused, cancelled, run)) = result? {
            settle(app, job, &paused, &cancelled, &run);
        }
        return Ok(());
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
            run: Arc::default(),
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
    let photos = if request.pinned.is_some() {
        let jobs = trackings.iter().map(|(_, t)| t.job.clone()).collect();
        queued_photos(&request, jobs, |job, index| {
            with_queue(|q| q.item_snapshot(job, index)).flatten()
        })
    } else {
        // A job an older build queued was not pinned: it is, once, now,
        // with the library open now, as that build read it.
        let mut legacy = request.clone();
        legacy.library = crate::session_bridge::active_library_directory()
            .map(|dir| dir.to_string_lossy().into_owned());
        match pin_all(vec![legacy]).await {
            Ok(mut pinned) => {
                let (legacy, snapshots) = pinned.remove(0);
                queued_photos(&legacy, vec![String::new()], move |_, index| {
                    snapshots.get(index).cloned()
                })
            }
            Err(e) => {
                for (_, tracking) in &trackings {
                    for index in tracking.pending.iter() {
                        with_queue(|q| q.item_failed(&tracking.job, *index, &e));
                    }
                    with_queue(|q| q.finish(&tracking.job, RunEnd::Completed));
                }
                return;
            }
        }
    };
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = trackings
        .iter()
        .map(|(_, t)| RunningJob {
            id: t.job.clone(),
            paused: Arc::clone(&t.paused),
            cancelled: Arc::clone(&t.cancelled),
            run: Arc::clone(&t.run),
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
        request.paths.clone(),
        targets,
        photos,
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
        // Settled already: stopped on its own while the others went on.
        if !tracking.run.claim_settlement() {
            continue;
        }
        let end = if tracking.cancelled.load(Ordering::SeqCst) {
            RunEnd::Cancelled
        } else if tracking.is_paused() {
            RunEnd::Paused
        } else {
            RunEnd::Completed
        };
        with_queue(|q| q.finish(&tracking.job, end));
    }
    // Report only outputs written in this run, including a partially successful
    // run; resumed jobs must not announce their earlier outputs again.
    let outputs: Vec<_> = trackings
        .iter()
        .flat_map(|(_, tracking)| {
            new_outputs(
                with_queue(|q| q.items(&tracking.job)).unwrap_or_default(),
                &tracking.pending,
            )
        })
        .collect();
    if !outputs.is_empty() {
        let _ = app.emit("export-outputs-written", outputs);
    }
}

/// Only successful items admitted in this run are newly written outputs.
fn new_outputs(items: Vec<Item>, pending: &HashSet<usize>) -> Vec<PathBuf> {
    items
        .into_iter()
        .filter(|item| pending.contains(&item.index) && item.state == ItemState::Done)
        .filter_map(|item| item.output)
        .collect()
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
                    run: Arc::default(),
                })
                .collect(),
        ))
    }

    /// Two catalogs that both hold `Photos/A.jpg`, each with its own
    /// recipe and rating, and an export request for `paths`.
    struct TwoCatalogs {
        _tmp: tempfile::TempDir,
        a: rawtonic_catalog::Library,
        b: rawtonic_catalog::Library,
        source: std::path::PathBuf,
    }

    fn two_catalogs() -> TwoCatalogs {
        use rawtonic_catalog::{Library, Mode};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("Photos")).unwrap();
        let source = root.join("Photos/A.jpg");
        std::fs::write(&source, b"jpeg").unwrap();
        let catalog = |name: &str, exposure: f64, rating: u8| {
            let lib = Library::create(&root.join(name), name, Mode::Catalog).unwrap();
            let asset = lib.add_asset(&source).unwrap();
            lib.save_editor_state(&asset.id, &recipe(exposure, rating, &["color:red"]))
                .unwrap();
            lib
        };
        TwoCatalogs {
            a: catalog("A", 1.0, 4),
            b: catalog("B", 2.0, 1),
            _tmp: tmp,
            source,
        }
    }

    fn recipe(exposure: f64, rating: u8, tags: &[&str]) -> Value {
        json!({"version": 1, "rating": rating, "tags": tags, "adjustments": {"exposure": exposure}})
    }

    fn request(paths: &[String], library: Option<&std::path::Path>) -> ExportRequest {
        serde_json::from_value(json!({
            "paths": paths, "outputFolderOrFile": "/out", "isExplicitFilePath": false,
            "baseOriginFolders": [], "exportSettings": {"jpegQuality": 85, "keepMetadata": true, "stripGps": false},
            "outputFormat": "jpeg", "currentEditPath": null, "currentEditAdjustments": null,
            "library": library,
        }))
        .unwrap()
    }

    /// A request pinned (`pin`) and queued as `enqueue_all` queues it.
    struct Queued {
        _tmp: tempfile::TempDir,
        queue: Arc<Mutex<ExportQueue>>,
        job: String,
        request: ExportRequest,
    }

    fn queued(mut request: ExportRequest) -> Queued {
        let tmp = tempfile::tempdir().unwrap();
        let queue = ExportQueue::open(&tmp.path().join("export-queue.sqlite")).unwrap();
        let snapshots: Vec<String> = request
            .pin()
            .unwrap()
            .iter()
            .map(|photo| serde_json::to_string(photo).unwrap())
            .collect();
        let value = serde_json::to_value(&request).unwrap();
        let job = queue
            .enqueue_all_with(&[("Job", &value, &request.paths, &snapshots)])
            .unwrap()
            .remove(0);
        let request = serde_json::from_value(queue.job(&job).unwrap().request).unwrap();
        Queued {
            _tmp: tmp,
            queue: Arc::new(Mutex::new(queue)),
            job,
            request,
        }
    }

    impl Queued {
        /// What the runner reads for each photo of the job.
        fn as_run(&self) -> Vec<PinnedPhoto> {
            let queue = Arc::clone(&self.queue);
            let photos = queued_photos(&self.request, vec![self.job.clone()], move |job, i| {
                queue.lock().unwrap().item_snapshot(job, i).unwrap()
            });
            (0..self.request.paths.len())
                .map(|i| (photos.read)(i).unwrap())
                .collect()
        }
    }

    #[test]
    fn a_queued_export_renders_and_embeds_what_the_library_it_was_submitted_from_held() {
        let libs = two_catalogs();
        let path = libs.source.to_string_lossy().into_owned();
        let from_a = queued(request(
            std::slice::from_ref(&path),
            Some(libs.a.directory.as_path()),
        ));
        // The user switches to catalog B and edits there, and A changes too,
        // before the job runs.
        let asset = |lib: &rawtonic_catalog::Library| lib.asset_at(&libs.source).unwrap().unwrap();
        libs.b
            .save_editor_state(&asset(&libs.b).id, &recipe(3.0, 2, &[]))
            .unwrap();
        libs.a
            .save_editor_state(&asset(&libs.a).id, &recipe(5.0, 5, &[]))
            .unwrap();
        let run = from_a.as_run();
        assert_eq!(run.len(), 1);
        assert_eq!(run[0].adjustments["exposure"], json!(1.0));
        let metadata = run[0].metadata.as_ref().expect("catalog A's metadata");
        assert_eq!((metadata.rating, metadata.label.as_str()), (4, "Red"));

        // The same photo submitted from B takes B's.
        let from_b = queued(request(&[path], Some(libs.b.directory.as_path())));
        let run = from_b.as_run();
        assert_eq!(run[0].adjustments["exposure"], json!(3.0));
        assert_eq!(run[0].metadata.as_ref().map(|m| m.rating), Some(2));
        assert!(
            !same_photos(
                &serde_json::to_value(&from_a.request).unwrap(),
                &serde_json::to_value(&from_b.request).unwrap()
            ),
            "jobs of different libraries never share a develop"
        );
    }

    #[test]
    fn a_queued_photo_keeps_its_recipe_until_it_is_exported() {
        let libs = two_catalogs();
        let path = libs.source.to_string_lossy().into_owned();
        let mut ai = recipe(1.0, 4, &[]);
        ai["adjustments"]["aiDenoise"] = json!(60);
        let asset = libs.a.asset_at(&libs.source).unwrap().unwrap();
        libs.a.save_editor_state(&asset.id, &ai).unwrap();
        let job = queued(request(&[path], Some(libs.a.directory.as_path())));
        let pinned = job.request.pinned.clone().expect("pinned when queued");
        assert_eq!(pinned.denoise_models.len(), 1, "loaded before the job runs");
        assert_eq!(
            pinned.denoise_models,
            crate::export_processing::denoise_models([&ai["adjustments"]])
        );
        assert!(
            !serde_json::to_string(&job.request)
                .unwrap()
                .contains("exposure"),
            "the recipes are kept with the photos, not in the job's request"
        );
        job.queue.lock().unwrap().item_done(&job.job, 0).unwrap();
        let queue = Arc::clone(&job.queue);
        let photos = queued_photos(&job.request, vec![job.job.clone()], move |job, i| {
            queue.lock().unwrap().item_snapshot(job, i).unwrap()
        });
        assert!((photos.read)(0).is_err(), "an exported photo is not again");
    }

    #[test]
    fn a_virtual_copy_exports_its_own_recipe_rating_label_and_keywords() {
        let libs = two_catalogs();
        let source = libs.source.to_string_lossy().into_owned();
        let copy = format!("{source}?vc=9b8208");
        std::fs::write(
            libs.source.with_file_name("A.jpg.9b8208.rrdata"),
            serde_json::to_vec(&recipe(-1.0, 5, &["color:green", "user:print", "boat"])).unwrap(),
        )
        .unwrap();
        let run = queued(request(&[source, copy], Some(libs.a.directory.as_path()))).as_run();
        let (photo, copy) = (&run[0], &run[1]);
        assert_eq!(photo.adjustments["exposure"], json!(1.0));
        assert_eq!(copy.adjustments["exposure"], json!(-1.0));
        let photo = photo.metadata.as_ref().unwrap();
        let copy = copy.metadata.as_ref().unwrap();
        assert_eq!((photo.rating, photo.label.as_str()), (4, "Red"));
        assert_eq!(
            (copy.rating, copy.label.as_str(), copy.keywords.clone()),
            (5, "Green", vec!["print".to_string(), "boat".into()])
        );
        // And that is what the copy's file carries.
        let embedded = rawtonic_export::exif_processing::export_metadata_with(
            &libs.source.to_string_lossy(),
            "tiff",
            true,
            false,
            false,
            8,
            8,
            false,
            &|| None,
            &|| Some(copy.clone()),
        );
        let xmp = embedded.and_then(|m| m.xmp).unwrap_or_default();
        assert!(xmp.contains("Green") && xmp.contains("print"), "{xmp}");
    }

    #[test]
    fn a_photo_gone_before_its_export_runs_fails_alone() {
        let libs = two_catalogs();
        let gone = libs.source.with_file_name("Gone.jpg");
        let paths = [
            gone.to_string_lossy().into_owned(),
            libs.source.to_string_lossy().into_owned(),
        ];
        let job = queued(request(&paths, Some(libs.a.directory.as_path())));
        assert_eq!(job.as_run().len(), 2, "the job keeps both photos");
        let error = crate::export_processing::missing_source(&gone).expect("gone");
        assert!(
            error.contains("Gone.jpg") && error.contains("cannot be found"),
            "{error}"
        );
        assert_eq!(crate::export_processing::missing_source(&libs.source), None);
    }

    #[test]
    fn output_notification_excludes_earlier_and_failed_items() {
        let item = |index, state, output: Option<&str>| Item {
            index,
            path: format!("{index}.raw"),
            state,
            output: output.map(PathBuf::from),
            error: None,
            attempts: 1,
            started_ms: None,
            finished_ms: None,
        };
        let items = vec![
            item(0, ItemState::Done, Some("old.jpg")),
            item(1, ItemState::Done, Some("new.jpg")),
            item(2, ItemState::Failed, Some("failed.jpg")),
        ];
        assert_eq!(
            new_outputs(items, &HashSet::from([1, 2])),
            vec![PathBuf::from("new.jpg")]
        );
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

    #[test]
    fn a_job_stopped_on_its_own_settles_once_its_photos_in_progress_are_done() {
        let (paused, cancelled, run) = (
            AtomicBool::new(false),
            AtomicBool::new(false),
            RunCount::default(),
        );
        assert!(run.admit(&paused, &cancelled));
        assert_eq!(settle_now(&paused, &cancelled, &run), None, "it goes on");
        paused.store(true, Ordering::SeqCst);
        assert_eq!(
            settle_now(&paused, &cancelled, &run),
            None,
            "a photo in progress"
        );
        run.release();
        assert_eq!(settle_now(&paused, &cancelled, &run), Some(RunEnd::Paused));
        assert_eq!(settle_now(&paused, &cancelled, &run), None, "settled once");
        let run = RunCount::default();
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(
            settle_now(&paused, &cancelled, &run),
            Some(RunEnd::Cancelled),
            "cancel wins"
        );
    }

    #[test]
    fn a_job_settles_not_between_a_photos_admission_and_its_start() {
        let (paused, cancelled, run) = (
            AtomicBool::new(false),
            AtomicBool::new(false),
            RunCount::default(),
        );
        // A worker is admitted, then held up before its photo begins; the
        // job is paused meanwhile.
        assert!(run.admit(&paused, &cancelled));
        paused.store(true, Ordering::SeqCst);
        assert_eq!(
            settle_now(&paused, &cancelled, &run),
            None,
            "the admitted photo is still to come"
        );
        run.release();
        assert_eq!(settle_now(&paused, &cancelled, &run), Some(RunEnd::Paused));
        assert!(
            !run.admit(&paused, &cancelled),
            "a settled job admits no photo"
        );
    }
}
