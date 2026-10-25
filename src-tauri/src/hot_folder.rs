//! Hot folder for sessions (G47; Capture One's hot folder, Lightroom's Auto
//! Import): photos that appear in a watched folder — written by tethering
//! software, a card reader or another app — are copied into the session's
//! Capture folder through the verified ingest once they are complete, and
//! get the next-capture adjustments and metadata (rating, keywords), stored
//! with the photograph in one transaction. While a hot folder runs, its
//! captures form an import batch (Previous Import). The watched files are
//! never moved or deleted; verified duplicates are skipped.
//!
//! The hot folder is kept in the library (`Library::hot_folder`): watching
//! resumes when the library is opened again (`hot_folder_resume`, also after
//! a restart), and the files it took are kept with it, so files that arrived
//! while the app was closed are taken once and the earlier ones are neither
//! imported nor hashed again. A watched folder that is gone (an unplugged
//! drive) is shown as such and watched again when it is back. Subfolders are
//! watched when asked (off by default, as Lightroom's Auto Import, which
//! never watches them); hidden files and folders are skipped and symbolic
//! links never followed.
use crate::formats::is_supported_image_file;
use rawtonic_catalog::{Contents, HotFolderConfig, ImportKind, Library};
use rawtonic_ingest::hot_folder::{DEFAULT_SETTLE, HotFolder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

const POLL: Duration = Duration::from_millis(700);

/// What a new capture starts with (Capture One's Next Capture Adjustments).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum NextCapture {
    /// The defaults, like an ordinary ingest.
    #[default]
    Defaults,
    /// The adjustments of the photo captured before, as it is edited now.
    CopyFromLast,
    /// A preset's adjustments (as they were when watching started).
    Preset { name: String, adjustments: Value },
}

/// The metadata every new capture gets (Capture One's Next Capture
/// Metadata, Lightroom's import metadata).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NextCaptureMetadata {
    /// 0 (none) to 5 stars.
    pub rating: u8,
    pub keywords: Vec<String>,
}

impl NextCaptureMetadata {
    fn is_empty(&self) -> bool {
        self.rating == 0 && self.keywords.is_empty()
    }

    /// Trimmed keywords without empty ones or repeats; a rating of 0–5.
    pub(crate) fn cleaned(self) -> Result<Self, String> {
        if self.rating > 5 {
            return Err("The rating must be 0–5 stars.".into());
        }
        let mut keywords: Vec<String> = Vec::new();
        for keyword in self.keywords {
            let keyword = keyword.trim();
            if !keyword.is_empty() && !keywords.iter().any(|k| k == keyword) {
                keywords.push(keyword.to_string());
            }
        }
        Ok(Self {
            rating: self.rating,
            keywords,
        })
    }
}

/// What a new capture is stored with, as kept in the library.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureSettings {
    pub next_capture: NextCapture,
    pub metadata: NextCaptureMetadata,
}

impl From<NextCapture> for CaptureSettings {
    fn from(next_capture: NextCapture) -> Self {
        Self {
            next_capture,
            metadata: NextCaptureMetadata::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotFolderStatus {
    pub folder: PathBuf,
    pub session: PathBuf,
    pub imported: usize,
    pub next_capture: NextCapture,
    pub metadata: NextCaptureMetadata,
    pub include_subfolders: bool,
    /// The watched folder cannot be read now (gone, say its drive was
    /// unplugged); watching goes on when it is back.
    pub missing: bool,
}

struct Watch {
    stop: Arc<AtomicBool>,
    imported: Arc<AtomicUsize>,
    missing: Arc<AtomicBool>,
    folder: PathBuf,
    session: PathBuf,
    settings: CaptureSettings,
    include_subfolders: bool,
    /// Its thread, joined when the watch stops.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    /// Stops the watch and waits for its thread: nothing of it is written
    /// after this (a watch replacing it must not have the old one record its
    /// files and last capture against the new folder). The thread checks the
    /// stop between files and while copying, so this is short.
    fn stop(mut self) -> Self {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::warn!("Hot folder: its watch ended with a panic");
        }
        self
    }

    fn status(&self) -> HotFolderStatus {
        HotFolderStatus {
            folder: self.folder.clone(),
            session: self.session.clone(),
            imported: self.imported.load(Ordering::Acquire),
            next_capture: self.settings.next_capture.clone(),
            metadata: self.settings.metadata.clone(),
            include_subfolders: self.include_subfolders,
            missing: self.missing.load(Ordering::Acquire),
        }
    }
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// One photo the hot folder handled, for the interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
    pub skipped: bool,
    pub error: Option<String>,
}

/// The adjustments a new capture gets, or `None` for the defaults.
fn next_state(lib: &Library, next: &NextCapture, previous: Option<&str>) -> Option<Value> {
    let adjustments = match next {
        NextCapture::Defaults => return None,
        NextCapture::Preset { adjustments, .. } => adjustments.clone(),
        NextCapture::CopyFromLast => lib
            .editor_state(previous?)
            .ok()??
            .get("adjustments")
            .cloned()?,
    };
    Some(for_new_capture(adjustments))
}

/// Lens settings applied to a new capture are looked up for it (its focal
/// length, aperture, camera data), not taken over with the last capture's
/// or a preset's parameters: marked unresolved, resolved by its first render
/// and save (`rawtonic_engine::lens_recipe`).
fn for_new_capture(mut adjustments: Value) -> Value {
    if let Some(layers) = adjustments.get_mut("masks") {
        rawtonic_engine::masks::prepare_layers_for_photo(layers, &mut || {
            uuid::Uuid::new_v4().to_string()
        });
    }
    if let Some(map) = adjustments.as_object_mut()
        && map.contains_key("lensCorrectionMode")
    {
        map.remove("lensDistortionParams");
        map.insert(
            rawtonic_engine::lens_recipe::LENS_PROFILE_FOR.to_string(),
            json!(rawtonic_engine::lens_recipe::LENS_UNRESOLVED),
        );
    }
    adjustments
}

/// `settings` with Copy from last capture resolved to the adjustments
/// `previous` has now (the defaults without one), so a capture can be
/// ingested again later with exactly what it was given (tethering keeps it
/// with the transfer file).
pub(crate) fn resolved(
    lib: &Library,
    settings: &CaptureSettings,
    previous: Option<&str>,
) -> CaptureSettings {
    let next_capture = match &settings.next_capture {
        NextCapture::CopyFromLast => match next_state(lib, &NextCapture::CopyFromLast, previous) {
            Some(adjustments) => NextCapture::Preset {
                name: String::new(),
                adjustments,
            },
            None => NextCapture::Defaults,
        },
        other => other.clone(),
    };
    CaptureSettings {
        next_capture,
        metadata: settings.metadata.clone(),
    }
}

/// What a new capture is stored with: its next-capture adjustments and
/// metadata.
fn next_contents(
    lib: &Library,
    settings: &CaptureSettings,
    previous: Option<&str>,
) -> Option<Contents> {
    let adjustments = next_state(lib, &settings.next_capture, previous);
    let meta = &settings.metadata;
    if adjustments.is_none() && meta.is_empty() {
        return None;
    }
    // The editor marks keywords the user typed with `user:`.
    let tags: Vec<String> = meta.keywords.iter().map(|k| format!("user:{k}")).collect();
    Some(Contents {
        editor_state: adjustments.map(|adjustments| {
            json!({
                "version": crate::image_processing::ImageMetadata::default().version,
                "rating": meta.rating,
                "tags": tags,
                "adjustments": adjustments,
            })
        }),
        metadata: (!meta.is_empty()).then(|| rawtonic_metadata::Metadata {
            rating: meta.rating,
            keywords: meta.keywords.clone(),
            ..Default::default()
        }),
        ..Contents::default()
    })
}

/// Ingests one complete file with its next-capture adjustments and
/// metadata (one transaction: a capture is never registered without them).
/// `earlier` is the copy an earlier try left unrecorded; it is recorded
/// instead of copying again when it still holds the file. Returns the asset
/// id of a new photo.
#[allow(clippy::too_many_arguments)]
pub fn ingest_capture(
    lib: &Library,
    source: &Path,
    template: &str,
    sequence: u64,
    settings: &CaptureSettings,
    previous: Option<&str>,
    earlier: Option<&Path>,
    cancel: &AtomicBool,
) -> (Imported, Option<String>) {
    let contents = next_contents(lib, settings, previous);
    let recorded = earlier.and_then(|copy| {
        rawtonic_ingest::record_earlier_copy(lib, source, copy, contents.as_ref()).transpose()
    });
    let result = match recorded.unwrap_or_else(|| {
        rawtonic_ingest::import_capture(lib, source, template, sequence, contents.as_ref(), cancel)
    }) {
        Ok(result) => result,
        Err(e) => {
            return (
                Imported {
                    source: source.into(),
                    destination: None,
                    skipped: false,
                    error: Some(format!("{e:#}")),
                },
                None,
            );
        }
    };
    let imported = Imported {
        source: source.into(),
        destination: result.destination.clone(),
        skipped: result.skipped,
        error: result.error.clone(),
    };
    if result.skipped || result.error.is_some() {
        return (imported, None);
    }
    let Some(asset) = result.asset_id else {
        return (imported, None);
    };
    if contents.is_some() {
        // The derived editor sidecar and XMP, as after an edit; the library
        // already holds the adjustments and metadata.
        crate::session_bridge::rederive(lib, &asset);
    }
    (imported, Some(asset))
}

/// Refuses folders the session itself writes into (they would feed back).
fn check_folder(session: &Path, folder: &Path) -> Result<(PathBuf, PathBuf), String> {
    let folder = folder
        .canonicalize()
        .map_err(|e| format!("The hot folder cannot be opened: {e}"))?;
    let session = session.canonicalize().map_err(|e| e.to_string())?;
    if !folder.is_dir() {
        return Err("The hot folder is not a folder.".into());
    }
    if folder.starts_with(&session) || session.starts_with(&folder) {
        return Err("Choose a hot folder outside the session.".into());
    }
    Ok((session, folder))
}

/// What one poll of a watch did.
#[derive(Debug, Default)]
struct Step {
    /// Whether the watched folder could not be read.
    missing: bool,
    /// The files handled, to show (a repeated failure is not shown again).
    shown: Vec<Imported>,
    /// How many of them are new photographs in the library.
    imported: usize,
}

/// A running hot folder: the poll state and what it needs between polls.
struct Watcher<'a> {
    lib: &'a Library,
    hot: HotFolder,
    template: String,
    settings: CaptureSettings,
    /// The last capture (Copy from last capture).
    previous: Option<String>,
    /// Copies made but not recorded, by source, for the retry.
    unrecorded: HashMap<PathBuf, PathBuf>,
}

impl<'a> Watcher<'a> {
    /// Starts from what the library keeps of its hot folder: the files it
    /// took before and the last capture.
    fn new(lib: &'a Library, folder: &Path, settle: Duration, config: &HotFolderConfig) -> Self {
        let mut hot = HotFolder::new(folder, settle).with_subfolders(config.include_subfolders);
        match lib.hot_folder_files() {
            Ok(files) => hot.restore_taken(files),
            Err(e) => log::warn!("Hot folder: the files taken before cannot be read: {e:#}"),
        }
        let settings = serde_json::from_value(config.settings.clone()).unwrap_or_default();
        Self {
            lib,
            hot,
            template: config.template.clone(),
            settings,
            previous: config.last_capture.clone(),
            unrecorded: HashMap::new(),
        }
    }

    fn poll(&mut self, now: Instant, stop: &AtomicBool) -> Step {
        let Ok(ready) = self.hot.poll(now, |p: &Path| is_supported_image_file(p)) else {
            return Step {
                missing: true,
                ..Step::default()
            };
        };
        let forgotten = self.hot.take_forgotten();
        if let Err(e) = self.lib.forget_hot_folder_files(&forgotten) {
            log::debug!("Hot folder: cannot forget files that left: {e:#}");
        }
        self.unrecorded.retain(|source, _| source.exists());
        let mut step = Step::default();
        for source in ready {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let sequence = self
                .lib
                .registered_asset_count()
                .map(|count| count as u64 + 1)
                .unwrap_or(1);
            let (result, asset) = ingest_capture(
                self.lib,
                &source,
                &self.template,
                sequence,
                &self.settings,
                self.previous.as_deref(),
                self.unrecorded.get(&source).map(PathBuf::as_path),
                stop,
            );
            match (&asset, &result.destination) {
                (None, Some(copy)) if result.error.is_some() && !result.skipped => {
                    self.unrecorded.insert(source.clone(), copy.clone());
                }
                _ => {
                    self.unrecorded.remove(&source);
                }
            }
            let recorded = asset.is_some();
            if recorded || result.skipped {
                // Taken for good: kept so a later watch does not take it again.
                if let Some(file) = self.hot.taken_file(&source)
                    && let Err(e) = self.lib.record_hot_folder_file(&file)
                {
                    log::debug!("Hot folder: {} not kept: {e:#}", source.display());
                }
            }
            if let Some(asset) = asset {
                step.imported += 1;
                if let Err(e) = self.lib.set_hot_folder_last_capture(&asset) {
                    log::debug!("Hot folder: last capture not kept: {e:#}");
                }
                self.previous = Some(asset);
            }
            // Not in the session (e.g. it was not writable, or the copy
            // could not be recorded): the file is tried again later (a copy
            // left unrecorded is recorded then, not copied again), and its
            // error shown once until it succeeds or changes.
            if result.error.is_some() && !recorded && !self.hot.failed(&source, Instant::now()) {
                log::debug!("Hot folder: {} failed again", source.display());
                continue;
            }
            if let Some(error) = &result.error {
                log::warn!("Hot folder: {}: {error}", source.display());
            }
            step.shown.push(result);
        }
        step
    }
}

/// Stops the running watch (the library keeps its hot folder).
fn suspend() -> Option<Watch> {
    let watch = WATCH.lock().ok()?.take()?;
    Some(watch.stop())
}

/// Starts watching the hot folder `session` keeps, in place of any other.
/// `imported`: the photographs it brought in since it was started
/// (`Library::hot_folder_imported`), so the count survives a restart.
fn spawn(
    session: PathBuf,
    config: HotFolderConfig,
    imported: usize,
    app: AppHandle,
) -> Result<HotFolderStatus, String> {
    suspend();
    let stop = Arc::new(AtomicBool::new(false));
    let imported = Arc::new(AtomicUsize::new(imported));
    let missing = Arc::new(AtomicBool::new(!config.folder.is_dir()));
    let settings: CaptureSettings =
        serde_json::from_value(config.settings.clone()).unwrap_or_default();
    let worker = {
        let (stop, imported, missing, session, config) = (
            stop.clone(),
            imported.clone(),
            missing.clone(),
            session.clone(),
            config.clone(),
        );
        std::thread::Builder::new()
            .name("hot-folder".into())
            .spawn(move || {
                // One connection, owned by this thread.
                let lib = match Library::open(&session) {
                    Ok(lib) => lib,
                    Err(e) => {
                        let _ = app.emit("hot-folder-error", format!("{e:#}"));
                        return;
                    }
                };
                // The captures of this run are one import batch.
                let _batch = lib.import_batch(ImportKind::HotFolder);
                let mut watcher = Watcher::new(&lib, &config.folder, DEFAULT_SETTLE, &config);
                while !stop.load(Ordering::Acquire) {
                    let step = watcher.poll(Instant::now(), &stop);
                    if missing.swap(step.missing, Ordering::AcqRel) != step.missing {
                        let _ = app.emit("hot-folder-state", json!({ "missing": step.missing }));
                    }
                    imported.fetch_add(step.imported, Ordering::AcqRel);
                    for result in &step.shown {
                        let _ = app.emit("hot-folder-imported", result);
                    }
                    std::thread::sleep(POLL);
                }
            })
            .map_err(|e| e.to_string())?
    };
    let watch = Watch {
        stop,
        imported,
        missing,
        folder: config.folder,
        session,
        settings,
        include_subfolders: config.include_subfolders,
        worker: Some(worker),
    };
    let status = watch.status();
    *WATCH.lock().map_err(|e| e.to_string())? = Some(watch);
    Ok(status)
}

/// Runs a change of the watch in a blocking worker, one at a time: stopping
/// a watch waits for its thread, which may be hashing a large file or
/// reading a slow drive, and must not hold up the app's main thread. The
/// workers do not keep the order the commands came in; the app sends each
/// one after the last has finished (`utils/hotFolderCommands.ts`).
async fn changing<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    static CHANGING: Mutex<()> = Mutex::new(());
    crate::file_management::blocking(move || {
        let _one = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
        work()
    })
    .await
}

/// Starts watching `folder` for the library in `directory` and keeps it
/// there, so watching resumes when the library is opened again.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn hot_folder_start(
    directory: String,
    folder: String,
    template: String,
    next_capture: NextCapture,
    metadata: Option<NextCaptureMetadata>,
    include_subfolders: Option<bool>,
    app: AppHandle,
) -> Result<HotFolderStatus, String> {
    changing(move || {
        start(
            directory,
            folder,
            template,
            next_capture,
            metadata,
            include_subfolders,
            app,
        )
    })
    .await?
}

fn start(
    directory: String,
    folder: String,
    template: String,
    next_capture: NextCapture,
    metadata: Option<NextCaptureMetadata>,
    include_subfolders: Option<bool>,
    app: AppHandle,
) -> Result<HotFolderStatus, String> {
    let (session, folder) = check_folder(Path::new(&directory), Path::new(&folder))?;
    rawtonic_naming::render(&template, &rawtonic_naming::Context::new("A.ARW", 1))
        .map_err(|e| format!("Filename template: {e}"))?;
    let settings = CaptureSettings {
        next_capture,
        metadata: metadata.unwrap_or_default().cleaned()?,
    };
    // The watch running now is replaced (and stops writing its files).
    suspend();
    let lib = Library::open(&session).map_err(|e| format!("{e:#}"))?;
    let config = HotFolderConfig {
        folder,
        template,
        include_subfolders: include_subfolders.unwrap_or(false),
        settings: serde_json::to_value(&settings).map_err(|e| e.to_string())?,
        last_capture: None,
    };
    lib.set_hot_folder(Some(&config))
        .map_err(|e| format!("{e:#}"))?;
    let imported = lib.hot_folder_imported().map_err(|e| format!("{e:#}"))?;
    drop(lib);
    spawn(session, config, imported, app)
}

/// Resumes the hot folder the library in `directory` keeps, if any (it is
/// already running for it: its status). A watched folder that is gone is
/// watched all the same and shown as missing.
#[tauri::command]
pub async fn hot_folder_resume(
    directory: String,
    app: AppHandle,
) -> Result<Option<HotFolderStatus>, String> {
    changing(move || resume(directory, app)).await?
}

/// Stops a watch that imports into another library than `session` (only
/// one library is open): a library without a hot folder must not leave the
/// last one's watch taking photos into a library the user has left.
fn leave_other_libraries(session: &Path) {
    let other = {
        let Ok(mut watch) = WATCH.lock() else { return };
        if watch.as_ref().is_some_and(|w| w.session != session) {
            watch.take()
        } else {
            None
        }
    };
    if let Some(other) = other {
        other.stop();
    }
}

fn resume(directory: String, app: AppHandle) -> Result<Option<HotFolderStatus>, String> {
    let session = Path::new(&directory)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    leave_other_libraries(&session);
    if let Ok(watch) = WATCH.lock()
        && let Some(watch) = watch.as_ref()
        && watch.session == session
    {
        return Ok(Some(watch.status()));
    }
    let lib = Library::open(&session).map_err(|e| format!("{e:#}"))?;
    let Some(config) = lib.hot_folder().map_err(|e| format!("{e:#}"))? else {
        return Ok(None);
    };
    // Not a reason to keep the hot folder from resuming.
    let imported = lib.hot_folder_imported().unwrap_or_else(|e| {
        log::warn!("Hot folder: its count of captures cannot be read: {e:#}");
        0
    });
    drop(lib);
    if config.folder.starts_with(&session) || session.starts_with(&config.folder) {
        return Err("Choose a hot folder outside the session.".into());
    }
    spawn(session, config, imported, app).map(Some)
}

/// Stops watching for now (another library is opened); the library keeps
/// its hot folder, which resumes when it is opened again. With `directory`,
/// only the watch of that library (not one resumed for another meanwhile).
#[tauri::command]
pub async fn hot_folder_suspend(directory: Option<String>) -> Result<(), String> {
    changing(move || suspend_for(directory)).await
}

fn suspend_for(directory: Option<String>) {
    let session = directory.map(|d| Path::new(&d).canonicalize());
    match session {
        Some(Ok(session)) => {
            stop_watch_of(Some(&session));
        }
        // A library that cannot be found has no running watch.
        Some(Err(_)) => {}
        None => {
            stop_watch_of(None);
        }
    }
}

/// Stops the running watch when it is `session`'s (any with `None`): a
/// request about one library never touches another's watch, also when it
/// runs late (after that library was opened).
fn stop_watch_of(session: Option<&Path>) -> Option<Watch> {
    let running = {
        let mut watch = WATCH.lock().ok()?;
        if watch
            .as_ref()
            .is_some_and(|running| session.is_none_or(|s| running.session == s))
        {
            watch.take()
        } else {
            None
        }
    };
    running.map(Watch::stop)
}

/// Stops watching, and the library forgets its hot folder: the library in
/// `directory` (its watch only), else the running watch's library.
#[tauri::command]
pub async fn hot_folder_stop(directory: Option<String>) -> Result<(), String> {
    changing(move || stop_and_forget(directory)).await?
}

fn stop_and_forget(directory: Option<String>) -> Result<(), String> {
    let asked = directory
        .map(|d| Path::new(&d).canonicalize().map_err(|e| e.to_string()))
        .transpose()?;
    let stopped = stop_watch_of(asked.as_deref());
    let session = match (asked, stopped) {
        (Some(session), _) => session,
        (None, Some(watch)) => watch.session,
        (None, None) => return Ok(()),
    };
    let lib = Library::open(&session).map_err(|e| format!("{e:#}"))?;
    lib.set_hot_folder(None).map_err(|e| format!("{e:#}"))
}

/// The running watch's state (the session commands report it).
pub fn hot_folder_status() -> Option<HotFolderStatus> {
    let watch = WATCH.lock().ok()?;
    Some(watch.as_ref()?.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use std::fs;

    fn session(root: &Path) -> Library {
        Library::create(&root.join("Job"), "Job", Mode::Session).unwrap()
    }

    /// Tests that put a watch into the app's one `WATCH` run one at a time.
    static ONE_WATCH: Mutex<()> = Mutex::new(());

    /// A watch of `session` whose thread runs until it is stopped; `done`
    /// is set when it has ended.
    fn fake_watch(session: &str) -> (Watch, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let worker = {
            let (stop, done) = (stop.clone(), done.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                done.store(true, Ordering::Release);
            })
        };
        let watch = Watch {
            stop,
            imported: Default::default(),
            missing: Default::default(),
            folder: PathBuf::from("/hot"),
            session: PathBuf::from(session),
            settings: CaptureSettings::default(),
            include_subfolders: false,
            worker: Some(worker),
        };
        (watch, done)
    }

    /// Library A's resume came late and started its watch; then library B,
    /// which has no hot folder, is opened: A's watch stops.
    #[test]
    fn opening_another_library_stops_the_last_ones_watch() {
        let _one = ONE_WATCH.lock().unwrap_or_else(|e| e.into_inner());
        let (a, a_done) = fake_watch("/libraries/A");
        *WATCH.lock().unwrap() = Some(a);
        leave_other_libraries(Path::new("/libraries/B"));
        assert!(WATCH.lock().unwrap().is_none());
        assert!(
            a_done.load(Ordering::Acquire),
            "A's watch stopped and joined"
        );
        // The library's own watch is kept.
        let (own, own_done) = fake_watch("/libraries/B");
        *WATCH.lock().unwrap() = Some(own);
        leave_other_libraries(Path::new("/libraries/B"));
        assert!(WATCH.lock().unwrap().is_some());
        assert!(!own_done.load(Ordering::Acquire));
        suspend();
        assert!(own_done.load(Ordering::Acquire));
    }

    /// A stop for library A that runs late, after library B resumed its
    /// watch, stops and forgets A's hot folder only.
    #[test]
    fn a_late_stop_for_one_library_leaves_anothers_watch_and_hot_folder() {
        let _one = ONE_WATCH.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let dir = tmp.path().join(name);
            let lib = Library::create(&dir, name, Mode::Session).unwrap();
            lib.set_hot_folder(Some(&HotFolderConfig {
                folder: tmp.path().join(format!("hot-{name}")),
                template: "{orig}".into(),
                include_subfolders: false,
                settings: json!({}),
                last_capture: None,
            }))
            .unwrap();
            dir.canonicalize().unwrap()
        };
        let (a, b) = (make("A"), make("B"));
        let (b_watch, b_done) = fake_watch(b.to_str().unwrap());
        *WATCH.lock().unwrap() = Some(b_watch);
        stop_and_forget(Some(a.to_string_lossy().into_owned())).unwrap();
        assert!(Library::open(&a).unwrap().hot_folder().unwrap().is_none());
        assert!(Library::open(&b).unwrap().hot_folder().unwrap().is_some());
        assert!(!b_done.load(Ordering::Acquire), "B's watch runs on");
        suspend_for(Some(a.to_string_lossy().into_owned()));
        assert!(!b_done.load(Ordering::Acquire));
        stop_and_forget(None).unwrap();
        assert!(b_done.load(Ordering::Acquire));
        assert!(Library::open(&b).unwrap().hot_folder().unwrap().is_none());
    }

    #[test]
    fn a_stopped_watch_has_finished_writing() {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let worker = {
            let (stop, done) = (stop.clone(), done.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // The file it was taking in when it was stopped.
                std::thread::sleep(Duration::from_millis(200));
                done.store(true, Ordering::Release);
            })
        };
        let watch = Watch {
            stop,
            imported: Default::default(),
            missing: Default::default(),
            folder: PathBuf::from("/hot"),
            session: PathBuf::from("/job"),
            settings: CaptureSettings::default(),
            include_subfolders: false,
            worker: Some(worker),
        };
        let stopped = watch.stop();
        assert!(done.load(Ordering::Acquire), "joined before returning");
        assert!(stopped.worker.is_none());
    }

    #[test]
    fn captures_are_copied_verified_and_take_the_last_adjustments() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        fs::write(hot.join("A.ARW"), b"first capture").unwrap();
        fs::write(hot.join("B.ARW"), b"second capture").unwrap();
        let cancel = AtomicBool::new(false);
        let next = NextCapture::CopyFromLast;
        let (first, a) = ingest_capture(
            &lib,
            &hot.join("A.ARW"),
            "{orig}",
            1,
            &next.clone().into(),
            None,
            None,
            &cancel,
        );
        assert!(first.error.is_none(), "{first:?}");
        let a = a.unwrap();
        // The photographer edits the first capture...
        lib.save_editor_state(
            &a,
            &json!({"version":1,"rating":2,"adjustments":{"exposure":0.7}}),
        )
        .unwrap();
        // ...and the next one starts from it.
        let (second, b) = ingest_capture(
            &lib,
            &hot.join("B.ARW"),
            "{orig}",
            2,
            &next.clone().into(),
            Some(&a),
            None,
            &cancel,
        );
        let b = b.unwrap();
        assert_eq!(
            lib.editor_state(&b).unwrap().unwrap()["adjustments"]["exposure"],
            0.7
        );
        let copy = second.destination.unwrap();
        assert!(copy.starts_with(lib.directory.join("Capture")));
        assert_eq!(fs::read(&copy).unwrap(), b"second capture");
        // The watched files stay.
        assert!(hot.join("A.ARW").is_file() && hot.join("B.ARW").is_file());
        // The same file again is a verified duplicate.
        let (again, none) = ingest_capture(
            &lib,
            &hot.join("B.ARW"),
            "{orig}",
            3,
            &next.clone().into(),
            Some(&b),
            None,
            &cancel,
        );
        assert!(again.skipped && none.is_none());
    }

    #[test]
    fn new_capture_preset_masks_wait_for_the_new_photo() {
        let own = json!({"masks": [{"subMasks": [{"id": "source", "type": "ai-sky",
            "parameters": {"maskDataBase64": "data:source"}}]}]});
        let first = for_new_capture(own.clone());
        let second = for_new_capture(own.clone());
        let sub = &first["masks"][0]["subMasks"][0];
        assert!(sub["parameters"]["maskDataBase64"].is_null());
        assert_eq!(sub["parameters"]["detectPending"], true);
        assert_ne!(sub["id"], second["masks"][0]["subMasks"][0]["id"]);
        assert_eq!(
            own["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:source"
        );
    }

    #[test]
    fn a_preset_applies_and_defaults_leave_no_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        fs::write(hot.join("A.ARW"), b"a").unwrap();
        fs::write(hot.join("B.ARW"), b"b").unwrap();
        let cancel = AtomicBool::new(false);
        let preset = NextCapture::Preset {
            name: "Studio".into(),
            adjustments: json!({"contrast": 12}),
        };
        let (_, a) = ingest_capture(
            &lib,
            &hot.join("A.ARW"),
            "{orig}",
            1,
            &preset.clone().into(),
            None,
            None,
            &cancel,
        );
        assert_eq!(
            lib.editor_state(&a.unwrap()).unwrap().unwrap()["adjustments"]["contrast"],
            12
        );
        let (_, b) = ingest_capture(
            &lib,
            &hot.join("B.ARW"),
            "{orig}",
            2,
            &NextCapture::Defaults.into(),
            None,
            None,
            &cancel,
        );
        assert!(lib.editor_state(&b.unwrap()).unwrap().is_none());
    }

    #[test]
    fn lens_settings_given_to_a_new_capture_are_looked_up_for_it() {
        use rawtonic_engine::lens_recipe::{LENS_PROFILE_FOR, LENS_UNRESOLVED};
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        for name in ["A.ARW", "B.ARW", "C.ARW"] {
            fs::write(hot.join(name), name.as_bytes()).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let next = NextCapture::CopyFromLast;
        let (_, a) = ingest_capture(
            &lib,
            &hot.join("A.ARW"),
            "{orig}",
            1,
            &next.clone().into(),
            None,
            None,
            &cancel,
        );
        let a = a.unwrap();
        // The last capture's lens was resolved for its own focal length.
        lib.save_editor_state(
            &a,
            &json!({"version":1,"adjustments":{
                "exposure":0.7,"lensCorrectionMode":"manual","lensMaker":"Canon",
                "lensModel":"Canon RF 24-70mm F2.8L IS USM",
                "lensDistortionParams":{"k1":0.03},"lensProfileFor":"stamp of A"}}),
        )
        .unwrap();
        let (_, b) = ingest_capture(
            &lib,
            &hot.join("B.ARW"),
            "{orig}",
            2,
            &next.clone().into(),
            Some(&a),
            None,
            &cancel,
        );
        let copied = &lib.editor_state(&b.unwrap()).unwrap().unwrap()["adjustments"];
        assert_eq!(copied["exposure"], 0.7);
        assert_eq!(copied["lensModel"], "Canon RF 24-70mm F2.8L IS USM");
        assert!(copied.get("lensDistortionParams").is_none());
        assert_eq!(copied[LENS_PROFILE_FOR], LENS_UNRESOLVED);
        // A preset's lens choice: looked up for the capture too.
        let preset = NextCapture::Preset {
            name: "Lens".into(),
            adjustments: json!({"lensCorrectionMode": "auto"}),
        };
        let (_, c) = ingest_capture(
            &lib,
            &hot.join("C.ARW"),
            "{orig}",
            3,
            &preset.into(),
            None,
            None,
            &cancel,
        );
        let applied = &lib.editor_state(&c.unwrap()).unwrap().unwrap()["adjustments"];
        assert_eq!(applied[LENS_PROFILE_FOR], LENS_UNRESOLVED);
        // Without a lens setting nothing is marked.
        assert!(
            for_new_capture(json!({"contrast": 12}))
                .get(LENS_PROFILE_FOR)
                .is_none()
        );
    }

    /// A capture copied but not recorded (the database was locked) is
    /// recorded on the retry, with its next-capture adjustments, even when
    /// the template's name moved on meanwhile (`{seq}`): no second copy.
    #[test]
    fn a_copy_left_unrecorded_is_recorded_on_the_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        let source = hot.join("A.ARW");
        fs::write(&source, b"capture").unwrap();
        let capture = lib.directory.join("Capture");
        fs::create_dir_all(&capture).unwrap();
        // What the failed try left behind under sequence 1.
        let earlier = capture.join("Job_0001.ARW");
        fs::write(&earlier, b"capture").unwrap();
        let preset = NextCapture::Preset {
            name: "Studio".into(),
            adjustments: json!({"contrast": 12}),
        };
        let (result, asset) = ingest_capture(
            &lib,
            &source,
            "{job}_{seq:4}.{ext}",
            2,
            &preset.clone().into(),
            None,
            Some(&earlier),
            &AtomicBool::new(false),
        );
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.destination.as_deref(), Some(earlier.as_path()));
        let asset = asset.unwrap();
        assert_eq!(lib.asset_at(&earlier).unwrap().unwrap().id, asset);
        assert_eq!(
            lib.editor_state(&asset).unwrap().unwrap()["adjustments"]["contrast"],
            12
        );
        let files: Vec<_> = fs::read_dir(&capture)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().ends_with(".ARW"))
            .collect();
        assert_eq!(files, ["Job_0001.ARW"]);

        // A hint that no longer holds the file imports as usual.
        let other = hot.join("B.ARW");
        fs::write(&other, b"another capture").unwrap();
        let (result, asset) = ingest_capture(
            &lib,
            &other,
            "{job}_{seq:4}.{ext}",
            2,
            &NextCapture::Defaults.into(),
            None,
            Some(&earlier),
            &AtomicBool::new(false),
        );
        assert!(asset.is_some(), "{result:?}");
        assert_eq!(
            fs::read(result.destination.unwrap()).unwrap(),
            b"another capture"
        );
        assert_eq!(fs::read(&earlier).unwrap(), b"capture");
    }

    /// The next-capture adjustments and metadata go into the library with
    /// the capture: when that write fails the capture is not registered at all (its
    /// verified copy stays for the retry), and the retry stores both, in
    /// the hot folder's import batch.
    #[test]
    fn a_capture_is_never_registered_without_its_adjustments() {
        for table in ["recipes", "metadata", "asset_imports"] {
            let tmp = tempfile::tempdir().unwrap();
            let lib = session(tmp.path());
            let _batch = lib.import_batch(ImportKind::HotFolder);
            let hot = tmp.path().join("Hot");
            fs::create_dir(&hot).unwrap();
            let source = hot.join("A.ARW");
            fs::write(&source, b"capture").unwrap();
            let settings = CaptureSettings {
                next_capture: NextCapture::Preset {
                    name: "Studio".into(),
                    adjustments: json!({"contrast": 12}),
                },
                metadata: NextCaptureMetadata {
                    rating: 3,
                    keywords: vec!["studio".into()],
                },
            };
            let cancel = AtomicBool::new(false);
            lib.inject_write_failure(table).unwrap();
            let (failed, none) =
                ingest_capture(&lib, &source, "{orig}", 1, &settings, None, None, &cancel);
            assert!(none.is_none(), "{table}: {failed:?}");
            assert!(
                failed.error.as_deref().unwrap().contains("injected"),
                "{table}"
            );
            let copy = failed.destination.unwrap();
            assert!(lib.asset_at(&copy).unwrap().is_none(), "{table}");
            assert_eq!(lib.previous_import().unwrap(), None, "{table}");
            lib.clear_write_failures().unwrap();
            let (retried, asset) = ingest_capture(
                &lib,
                &source,
                "{orig}",
                2,
                &settings,
                None,
                Some(&copy),
                &cancel,
            );
            assert!(retried.error.is_none(), "{table}: {retried:?}");
            assert_eq!(retried.destination.as_deref(), Some(copy.as_path()));
            let asset = asset.unwrap();
            assert_eq!(
                lib.editor_state(&asset).unwrap().unwrap()["adjustments"]["contrast"],
                12,
                "{table}"
            );
            // The derived editor sidecar follows the library.
            let sidecar: Value =
                serde_json::from_slice(&fs::read(copy.with_file_name("A.ARW.rrdata")).unwrap())
                    .unwrap();
            assert_eq!(sidecar["adjustments"]["contrast"], 12, "{table}");
            let meta = lib.metadata(&asset).unwrap();
            assert_eq!(
                (meta.rating, meta.keywords),
                (3, vec!["studio".to_string()])
            );
            assert_eq!(sidecar["rating"], 3, "{table}");
            let batch = lib.previous_import().unwrap().unwrap();
            assert_eq!((batch.kind, batch.count), (ImportKind::HotFolder, 1));
        }
    }

    #[test]
    fn captures_of_one_run_are_one_import_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        let cancel = AtomicBool::new(false);
        let _batch = lib.import_batch(ImportKind::HotFolder);
        for (i, name) in ["A.ARW", "B.ARW", "C.ARW"].into_iter().enumerate() {
            fs::write(hot.join(name), name).unwrap();
            let (result, asset) = ingest_capture(
                &lib,
                &hot.join(name),
                "{orig}",
                i as u64 + 1,
                &NextCapture::Defaults.into(),
                None,
                None,
                &cancel,
            );
            assert!(asset.is_some(), "{result:?}");
        }
        let batch = lib.previous_import().unwrap().unwrap();
        assert_eq!((batch.kind, batch.count), (ImportKind::HotFolder, 3));
        assert_eq!(lib.previous_import_assets().unwrap().len(), 3);
    }

    fn kept(
        lib: &Library,
        hot: &Path,
        subfolders: bool,
        settings: CaptureSettings,
    ) -> HotFolderConfig {
        let config = HotFolderConfig {
            folder: hot.canonicalize().unwrap(),
            template: "{orig}".into(),
            include_subfolders: subfolders,
            settings: serde_json::to_value(settings).unwrap(),
            last_capture: None,
        };
        lib.set_hot_folder(Some(&config)).unwrap();
        config
    }

    /// Polls a watch as its thread does, without the settle time.
    fn run(watcher: &mut Watcher) -> Vec<Imported> {
        let stop = AtomicBool::new(false);
        let mut shown = Vec::new();
        for _ in 0..3 {
            shown.extend(watcher.poll(Instant::now(), &stop).shown);
        }
        shown
    }

    fn reopen(lib: Library) -> (Library, HotFolderConfig) {
        let directory = lib.directory.clone();
        drop(lib);
        let lib = Library::open(&directory).unwrap();
        let config = lib.hot_folder().unwrap().expect("kept in the library");
        (lib, config)
    }

    #[test]
    fn capture_sequence_does_not_reuse_numbers_after_trash() {
        for system in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let lib = session(tmp.path());
            let first = lib.directory.join("Capture/1.ARW");
            for (path, bytes) in [
                (&first, "first"),
                (&lib.directory.join("Capture/2.ARW"), "second"),
            ] {
                fs::write(path, bytes).unwrap();
                lib.add_asset(path).unwrap();
            }
            let held = if system {
                tmp.path().join("held.ARW")
            } else {
                lib.directory.join("Trash/1.ARW")
            };
            fs::rename(&first, &held).unwrap();
            if system {
                lib.set_trashed_at(&first, true).unwrap();
            } else {
                lib.relocate(&first, &held).unwrap();
            }
            let hot = tmp.path().join("Hot");
            fs::create_dir(&hot).unwrap();
            fs::write(hot.join("new.ARW"), "third distinct photo").unwrap();
            let mut config = kept(&lib, &hot, false, CaptureSettings::default());
            config.template = "{seq}.{ext}".into();
            let mut watcher = Watcher::new(&lib, &hot, Duration::ZERO, &config);
            let shown = run(&mut watcher);
            assert_eq!(shown.len(), 1, "{shown:?}");
            assert_eq!(
                shown[0].destination.as_ref().unwrap().file_name().unwrap(),
                "3.ARW"
            );
        }
    }

    /// The app quits while watching; files that arrive meanwhile (here in a
    /// subfolder) are taken once when the library is opened again, the
    /// earlier ones are not taken again (not even as duplicates), and Copy
    /// from last capture goes on from the capture before the restart.
    #[test]
    fn watching_resumes_after_a_restart_and_takes_what_arrived_once() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir_all(hot.join("day 2")).unwrap();
        fs::write(hot.join("A.ARW"), b"first capture").unwrap();
        let config = kept(&lib, &hot, true, NextCapture::CopyFromLast.into());
        let (lib, config) = {
            // As the watch thread does: the run's captures are one batch.
            let _batch = lib.import_batch(ImportKind::HotFolder);
            let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
            let shown = run(&mut watcher);
            assert_eq!(shown.len(), 1, "{shown:?}");
            assert!(!shown[0].skipped && shown[0].error.is_none());
            let a = watcher.previous.clone().unwrap();
            watcher
                .lib
                .save_editor_state(
                    &a,
                    &json!({"version":1,"rating":0,"adjustments":{"exposure":0.4}}),
                )
                .unwrap();
            // The app quits.
            drop(watcher);
            drop(_batch);
            reopen(lib)
        };
        // The hot folder button's count goes on from before the restart.
        assert_eq!(lib.hot_folder_imported().unwrap(), 1);
        // While it was closed.
        fs::write(hot.join("day 2/B.ARW"), b"second capture").unwrap();
        let batch = lib.import_batch(ImportKind::HotFolder);
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        let shown = run(&mut watcher);
        assert_eq!(
            shown.len(),
            1,
            "only B, and A not even as a duplicate: {shown:?}"
        );
        assert!(shown[0].source.ends_with("day 2/B.ARW"));
        assert!(!shown[0].skipped && shown[0].error.is_none());
        let b = watcher.previous.clone().unwrap();
        assert_eq!(
            watcher.lib.editor_state(&b).unwrap().unwrap()["adjustments"]["exposure"],
            0.4
        );
        assert!(run(&mut watcher).is_empty(), "once");
        assert_eq!(watcher.lib.summary().unwrap().assets, 2);
        assert_eq!(watcher.lib.hot_folder_imported().unwrap(), 2);

        // Once more, nothing new: nothing at all.
        drop(watcher);
        drop(batch);
        let (lib, config) = reopen(lib);
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        assert!(run(&mut watcher).is_empty());
        // The watched files stay.
        assert!(hot.join("A.ARW").is_file() && hot.join("day 2/B.ARW").is_file());
    }

    /// Without the kept files (an older library, or the start after Stop)
    /// files already in the library are verified duplicates: no copies.
    #[test]
    fn a_fresh_start_skips_what_the_library_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        fs::write(hot.join("A.ARW"), b"capture").unwrap();
        let config = kept(&lib, &hot, false, CaptureSettings::default());
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        assert_eq!(run(&mut watcher).len(), 1);
        drop(watcher);
        let config = kept(&lib, &hot, false, CaptureSettings::default());
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        let shown = run(&mut watcher);
        assert!(shown.len() == 1 && shown[0].skipped, "{shown:?}");
        assert_eq!(watcher.lib.summary().unwrap().assets, 1);
    }

    #[test]
    fn subfolders_are_left_alone_unless_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir_all(hot.join("sub")).unwrap();
        fs::write(hot.join("sub/A.ARW"), b"capture").unwrap();
        let config = kept(&lib, &hot, false, CaptureSettings::default());
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        assert!(run(&mut watcher).is_empty());
    }

    /// An unplugged drive: shown as missing (no error per poll), and the
    /// watch goes on when it is back.
    #[test]
    fn a_missing_folder_is_a_state_and_watching_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        let config = kept(&lib, &hot, false, CaptureSettings::default());
        let away = tmp.path().join("Unplugged");
        fs::rename(&hot, &away).unwrap();
        let mut watcher = Watcher::new(&lib, &config.folder, Duration::ZERO, &config);
        let stop = AtomicBool::new(false);
        for _ in 0..3 {
            let step = watcher.poll(Instant::now(), &stop);
            assert!(step.missing && step.shown.is_empty());
        }
        fs::write(away.join("A.ARW"), b"capture").unwrap();
        fs::rename(&away, &hot).unwrap();
        assert!(!watcher.poll(Instant::now(), &stop).missing);
        let shown = run(&mut watcher);
        assert!(shown.len() == 1 && shown[0].error.is_none(), "{shown:?}");
    }

    #[test]
    fn next_capture_metadata_applies_with_or_without_adjustments() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        fs::write(hot.join("A.ARW"), b"a").unwrap();
        let metadata = NextCaptureMetadata {
            rating: 2,
            keywords: vec![" studio ".into(), "".into(), "studio".into(), "Acme".into()],
        }
        .cleaned()
        .unwrap();
        assert_eq!(metadata.keywords, ["studio", "Acme"]);
        let settings = CaptureSettings {
            next_capture: NextCapture::Defaults,
            metadata,
        };
        let (_, a) = ingest_capture(
            &lib,
            &hot.join("A.ARW"),
            "{orig}",
            1,
            &settings,
            None,
            None,
            &AtomicBool::new(false),
        );
        let a = a.unwrap();
        let meta = lib.metadata(&a).unwrap();
        assert_eq!((meta.rating, meta.keywords.len()), (2, 2));
        // The editor shows them; the photo still opens with the defaults.
        let state = lib.editor_state(&a).unwrap().unwrap();
        assert_eq!(state["rating"], 2);
        assert!(state["adjustments"].is_null());
        assert!(
            NextCaptureMetadata {
                rating: 6,
                keywords: vec![]
            }
            .cleaned()
            .is_err()
        );
    }
    #[test]
    fn folders_inside_the_session_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = session(tmp.path());
        assert!(check_folder(&lib.directory, &lib.directory.join("Capture")).is_err());
        assert!(
            check_folder(&lib.directory, tmp.path()).is_err(),
            "the session's parent"
        );
        let hot = tmp.path().join("Hot");
        fs::create_dir(&hot).unwrap();
        assert!(check_folder(&lib.directory, &hot).is_ok());
    }
}
