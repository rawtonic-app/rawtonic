//! Tethered shooting (G46; Capture One's Camera tool): the Tauri side of
//! `rawtonic_tether`. The camera library runs in the helper process
//! `rawtonic-tether` next to the app's executable; a crash or a hung USB
//! transfer there ends only the helper, and the next command starts a new
//! one.
//!
//! Every photo the helper downloads into the session's transfer folder
//! (`rawtonic_tether::staging`) goes through the hot folder's
//! `ingest_capture`: the verified no-clobber ingest into Capture with the
//! session's filename template, and the next-capture adjustments, rating
//! and keywords in the same transaction. What a photo is given is kept with
//! its transfer file before the ingest starts, so after a crash it is
//! ingested again with exactly that (and a copy the crash left unrecorded
//! is recorded, not copied twice). The transfer copy is removed only once
//! the photo is in the library. Nothing on the camera is deleted.
use crate::hot_folder::{self, CaptureSettings, NextCapture, NextCaptureMetadata};
use rawtonic_catalog::{ImportKind, Library};
use rawtonic_tether::client::{Client, Incoming};
use rawtonic_tether::protocol::{
    CameraInfo, CameraStatus, Captured, ErrorKind, Event, Reply, Request, Setting, TetherError,
};
use rawtonic_tether::staging;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager};

const QUICK: Duration = Duration::from_secs(15);
const SLOW: Duration = Duration::from_secs(45);
/// A capture request returns once the shutter fired; a camera that waits
/// for the photo itself (no separate trigger) takes the exposure time too.
const CAPTURE: Duration = Duration::from_secs(90);

/// What the next photo is ingested with.
#[derive(Debug, Clone)]
struct CaptureConfig {
    template: String,
    settings: CaptureSettings,
    /// Free each photo from the camera's transfer queue once it is in the
    /// library (on unless the user turned it off; a camera may keep no card
    /// copy, so never before the verified commit).
    free_camera_copies: bool,
}

/// What a photo was given, kept with its transfer file before its ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    sequence: u64,
    template: String,
    settings: CaptureSettings,
}

/// Where the helper's photos go: the connection's ingest, when there is one.
type IngestSender = Arc<Mutex<Option<mpsc::Sender<(Captured, Instant)>>>>;

/// The helper's path, arguments and environment.
type HelperCommand = (PathBuf, Vec<OsString>, Vec<(OsString, OsString)>);

struct Connection {
    session: PathBuf,
    status: CameraStatus,
    config: Arc<Mutex<CaptureConfig>>,
}

struct Helper {
    client: Arc<Client>,
    library: Option<String>,
    /// Where the helper's photos go (the connection's ingest), shared with
    /// the callback that hears them.
    ingest: IngestSender,
    connection: Option<Connection>,
    /// The connection whose camera went away, kept until it is connected
    /// again or the user connects or disconnects.
    lost: Option<Connection>,
    /// The connection's ingest thread (joined before another one starts:
    /// one ingest per transfer folder).
    worker: Option<std::thread::JoinHandle<()>>,
    /// A connect is waiting for the helper; `lost_while_connecting` notes a
    /// camera lost before its connection was installed.
    connecting: bool,
    lost_while_connecting: bool,
}

/// The camera went away: its connection is kept for connecting it again.
/// Whether a reconnect should start now (a connect still in flight starts
/// it itself once it has installed the connection).
fn camera_lost(state: &Mutex<Option<Helper>>) -> bool {
    let mut state = state.lock().unwrap();
    let Some(h) = state.as_mut() else {
        return false;
    };
    if let Some(connection) = h.connection.take() {
        h.lost = Some(connection);
        return true;
    }
    if h.connecting {
        h.lost_while_connecting = true;
    }
    false
}

/// The running helper. Never held while waiting for the helper: its reader
/// thread may need it meanwhile.
static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
/// The newest live-view frame.
static FRAME: Mutex<Option<Vec<u8>>> = Mutex::new(None);
/// Counts the user's connects and disconnects; a reconnect started before
/// one of them gives up.
static CHOICE: AtomicU64 = AtomicU64::new(0);
/// Held while a connection changes (the user's connect or disconnect, a
/// reconnect's look-and-connect), so they never interleave.
static TRANSITION: Mutex<()> = Mutex::new(());
/// Held while a helper is started (and an ended one's ingest joined), so
/// two callers never start two or skip the join.
static STARTING: Mutex<()> = Mutex::new(());
/// How often a camera that went away is looked for.
const RECONNECT_EVERY: Duration = Duration::from_secs(1);

fn text(e: TetherError) -> String {
    e.message
}

/// The helper's path, its arguments and environment. `RAWTONIC_TETHER_HELPER`
/// overrides the path; `RAWTONIC_TETHER_MOCK` (a mock script, for tests and
/// QA without a camera) starts the mock camera. Bundled camera drivers
/// (packaging) are passed in libgphoto2's variables.
fn helper_command(app: &AppHandle) -> Result<HelperCommand, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no executable folder")?;
    let path = std::env::var_os("RAWTONIC_TETHER_HELPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join(rawtonic_tether::HELPER));
    if !path.is_file() {
        return Err("Tethering is not available in this build (no tether helper).".into());
    }
    let args = if std::env::var_os("RAWTONIC_TETHER_MOCK").is_some() {
        vec!["--mock".into()]
    } else {
        vec![]
    };
    // The bundled camera and port drivers, never the system's (the
    // libraries are the bundle's: macOS frameworks, AppImage usr/lib).
    let mut envs: Vec<(OsString, OsString)> = Vec::new();
    if let Ok(libs) = app.path().resource_dir().map(|r| r.join("gphoto2"))
        && libs.join("camlibs").is_dir()
    {
        envs.push(("CAMLIBS".into(), libs.join("camlibs").into()));
        envs.push(("IOLIBS".into(), libs.join("iolibs").into()));
    }
    Ok((path, args, envs))
}

/// Emits the helper's events; photos go to the connection's ingest.
fn on_incoming(app: AppHandle, ingest: IngestSender) -> impl Fn(Incoming) + Send + Sync + 'static {
    move |incoming| match incoming {
        Incoming::Event(Event::Captured { captured }) => {
            let received = Instant::now();
            let sent = ingest
                .lock()
                .unwrap()
                .as_ref()
                .map(|tx| tx.send((captured.clone(), received)).is_ok());
            if sent != Some(true) {
                // Kept in the transfer folder; ingested on the next connect.
                log::warn!(
                    "Tether: {} arrived without a session; kept for the next connect",
                    captured.path.display()
                );
            }
        }
        Incoming::Event(Event::CaptureFailed { error }) => {
            let _ = app.emit("tether-capture-failed", error);
        }
        Incoming::Event(Event::SettingsChanged) => {
            let _ = app.emit("tether-settings-changed", ());
        }
        Incoming::Event(Event::Disconnected { error }) => {
            let _ = app.emit(
                "tether-state",
                json!({ "connected": false, "error": error }),
            );
            if error.kind == ErrorKind::Disconnected && camera_lost(&HELPER) {
                let app = app.clone();
                std::thread::spawn(move || reconnect_when_back(app));
            }
        }
        Incoming::Event(Event::LiveViewStopped { error }) => {
            let _ = app.emit("tether-live-view-stopped", error);
        }
        Incoming::LiveView(jpeg) => *FRAME.lock().unwrap() = Some(jpeg),
        Incoming::Exited(log) => {
            log::warn!("Tether helper ended. Last log lines:\n{log}");
            let error = TetherError::new(
                ErrorKind::Disconnected,
                "The tether helper stopped. Connect the camera again.",
            );
            let _ = app.emit(
                "tether-state",
                json!({ "connected": false, "error": error }),
            );
        }
    }
}

/// The running helper's client (started when there is none or the last one
/// ended).
fn client(app: &AppHandle) -> Result<Arc<Client>, String> {
    let _starting = STARTING.lock().unwrap();
    // A helper that ended: its ingest finishes the photos it has before a
    // new helper (and a new ingest) may touch the transfer folder.
    let ended = {
        let mut helper = HELPER.lock().unwrap();
        match helper.as_ref() {
            Some(h) if h.client.alive() => return Ok(h.client.clone()),
            Some(_) => {
                log::info!("Tether helper gone; starting a new one");
                helper.take()
            }
            None => None,
        }
    };
    if let Some(ended) = ended {
        ended.ingest.lock().unwrap().take();
        if let Some(worker) = ended.worker {
            let _ = worker.join();
        }
    }
    let mut helper = HELPER.lock().unwrap();
    if let Some(h) = helper.as_ref()
        && h.client.alive()
    {
        return Ok(h.client.clone());
    }
    let (path, args, envs) = helper_command(app)?;
    let ingest = Arc::new(Mutex::new(None));
    let (client, library) = Client::spawn(
        &path,
        &args,
        &envs,
        on_incoming(app.clone(), ingest.clone()),
    )
    .map_err(text)?;
    let client = Arc::new(client);
    *helper = Some(Helper {
        client: client.clone(),
        library,
        ingest,
        connection: None,
        lost: None,
        worker: None,
        connecting: false,
        lost_while_connecting: false,
    });
    Ok(client)
}

/// After the camera went away (cable, power): connects the same model again
/// when it is back, into the same session with the same capture settings
/// and the same ingest — unless the user connected or disconnected
/// meanwhile or the helper ended (Capture One reconnects on its own too).
fn reconnect_when_back(app: AppHandle) {
    let choice = CHOICE.load(Ordering::SeqCst);
    let (client, session, model, serial, config) = {
        let mut helper = HELPER.lock().unwrap();
        let Some(h) = helper.as_mut() else { return };
        // Not connected any more; kept for connecting it again.
        if let Some(connection) = h.connection.take() {
            h.lost = Some(connection);
        }
        let Some(c) = h.lost.as_ref() else {
            return;
        };
        (
            h.client.clone(),
            c.session.clone(),
            c.status.model.clone(),
            c.status.serial.clone(),
            c.config.clone(),
        )
    };
    let wanted = || CHOICE.load(Ordering::SeqCst) == choice;
    let Some((status, _turn)) = reconnect(
        &client,
        &model,
        serial.as_deref(),
        &session,
        RECONNECT_EVERY,
        &TRANSITION,
        wanted,
    ) else {
        return;
    };
    let mut helper = HELPER.lock().unwrap();
    let Some(h) = helper.as_mut() else { return };
    if !wanted() || !Arc::ptr_eq(&h.client, &client) {
        return;
    }
    h.lost = None;
    h.connection = Some(Connection {
        session: session.clone(),
        status: status.clone(),
        config,
    });
    log::info!("Tether: {model} is back, connected again");
    let status = TetherStatus {
        session,
        camera: status,
    };
    let _ = app.emit(
        "tether-state",
        json!({ "connected": true, "status": status }),
    );
}

/// Looks for `model` every `every` and connects it once it is listed again;
/// `None` when the helper ended or `wanted` turned false first.
fn reconnect<'a>(
    client: &Client,
    model: &str,
    serial: Option<&str>,
    session: &Path,
    every: Duration,
    transition: &'a Mutex<()>,
    wanted: impl Fn() -> bool,
) -> Option<(CameraStatus, std::sync::MutexGuard<'a, ()>)> {
    loop {
        std::thread::sleep(every);
        // One look-and-connect at a time with the user's connect and
        // disconnect: a choice made meanwhile is seen before connecting.
        let turn = transition.lock().unwrap();
        if !wanted() || !client.alive() {
            return None;
        }
        let Ok(Reply::Cameras { cameras }) = client.request(Request::ListCameras, QUICK) else {
            continue;
        };
        let candidates: Vec<_> = cameras.into_iter().filter(|c| c.model == model).collect();
        // Without a serial number the body is known only by its model:
        // then only when it is the one body of that model attached.
        if serial.is_none() && candidates.len() != 1 {
            continue;
        }
        for camera in candidates {
            if !wanted() {
                return None;
            }
            let connect = Request::Connect {
                port: Some(camera.port),
                staging: staging::root(session),
            };
            let Ok(Reply::Connected { status }) = client.request(connect, SLOW) else {
                continue;
            };
            if serial.is_some() && status.serial.as_deref() != serial {
                // Another body of the same model.
                let _ = client.request(Request::Disconnect, QUICK);
                continue;
            }
            return Some((status, turn));
        }
    }
}

/// Sends a request to the helper; one that hangs is ended (its next use
/// starts a new helper).
fn request(app: &AppHandle, request: Request, timeout: Duration) -> Result<Reply, String> {
    let client = client(app)?;
    match client.request(request, timeout) {
        Err(e) if e.kind == ErrorKind::Unresponsive => {
            client.kill();
            Err(format!(
                "{} The tether helper was stopped; connect the camera again.",
                e.message
            ))
        }
        Err(e) if e.kind == ErrorKind::Disconnected => {
            // The same transition as the helper's Disconnected event (which
            // follows this reply): whichever is first starts the reconnect.
            if camera_lost(&HELPER) {
                let app = app.clone();
                std::thread::spawn(move || reconnect_when_back(app));
            }
            Err(e.message)
        }
        other => other.map_err(text),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherCameras {
    /// libgphoto2's version (`None`: the mock camera).
    pub library: Option<String>,
    pub cameras: Vec<CameraInfo>,
}

/// Starts the helper if needed and lists the cameras on USB. Fails when
/// this build has no helper, or the helper does not answer as this app's.
#[tauri::command]
pub async fn tether_cameras(app: AppHandle) -> Result<TetherCameras, String> {
    blocking(move || {
        let Reply::Cameras { cameras } = request(&app, Request::ListCameras, SLOW)? else {
            return Err("Unexpected answer from the tether helper.".into());
        };
        let library = HELPER
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.library.clone());
        Ok(TetherCameras { library, cameras })
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherStatus {
    pub session: PathBuf,
    pub camera: CameraStatus,
}

/// Tells the interface (`AppHandle::emit` in the app).
type Notify = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;
/// Frees a committed photo from the camera's transfer queue (the helper's
/// `Release`; a no-op on cameras without one).
type Release = Arc<dyn Fn(&Captured) + Send + Sync>;

/// `Release` through the helper that delivered the photo: none once that
/// helper ended (a new one numbers its connections afresh).
fn release_via(client: std::sync::Weak<Client>) -> Release {
    Arc::new(move |captured: &Captured| {
        let Some(client) = client.upgrade() else {
            return;
        };
        let release = Request::Release {
            connection: captured.connection,
            camera_folder: captured.camera_folder.clone(),
            camera_name: captured.camera_name.clone(),
        };
        if let Err(e) = client.request(release, QUICK) {
            log::warn!(
                "Tether: {} not freed on the camera: {}",
                captured.camera_name,
                e.message
            );
        }
    })
}

fn notify_app(app: AppHandle) -> Notify {
    Arc::new(move |name: &str, value: serde_json::Value| {
        let _ = app.emit(name, value);
    })
}

/// A connection's ingest: one library connection, the captures of the
/// connection form one import batch. First ingests what an earlier run
/// left in the transfer folder.
fn spawn_ingest(
    notify: Notify,
    release: Release,
    recovered: staging::Recovered,
    session: PathBuf,
    config: Arc<Mutex<CaptureConfig>>,
    photos: mpsc::Receiver<(Captured, Instant)>,
) -> Result<std::thread::JoinHandle<()>, String> {
    let root = staging::root(&session);
    std::thread::Builder::new()
        .name("tether-ingest".into())
        .spawn(move || {
            let lib = match Library::open(&session) {
                Ok(lib) => lib,
                Err(e) => {
                    notify("tether-error", json!(format!("{e:#}")));
                    return;
                }
            };
            let _batch = lib.import_batch(ImportKind::Tether);
            let mut ingest = Ingest {
                notify: &*notify,
                release: &*release,
                lib: &lib,
                root: &root,
                config: &config,
                previous: None,
            };
            for path in recovered.incomplete {
                log::warn!("Tether: incomplete download kept at {}", path.display());
                notify("tether-incomplete", json!(path));
            }
            for path in recovered.complete {
                ingest.one(&path, None);
            }
            while let Ok((captured, received)) = photos.recv() {
                ingest.one(&captured.path.clone(), Some((captured, received)));
            }
        })
        .map_err(|e| e.to_string())
}

/// One photo the tether brought in, for the interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherCaptured {
    pub camera_name: String,
    pub destination: Option<PathBuf>,
    pub asset_id: Option<String>,
    pub skipped: bool,
    pub error: Option<String>,
    /// Fired by the app (else on the body, or left from an earlier run).
    pub requested: bool,
    /// From the app's request (or the camera's report) to: the camera's
    /// report, the complete download, the photo in the library.
    pub ms_to_event: Option<u64>,
    pub ms_to_downloaded: Option<u64>,
    pub ms_to_ingested: Option<u64>,
}

struct Ingest<'a> {
    notify: &'a (dyn Fn(&str, serde_json::Value) + Send + Sync),
    release: &'a (dyn Fn(&Captured) + Send + Sync),
    lib: &'a Library,
    root: &'a Path,
    config: &'a Mutex<CaptureConfig>,
    /// The last capture (Copy from last capture).
    previous: Option<String>,
}

impl Ingest<'_> {
    /// The record kept with a transfer file, written now if there is none.
    fn record(&self, path: &Path) -> Result<Record, String> {
        if let Ok(Some(value)) = staging::read_record(self.root, path)
            && let Ok(record) = serde_json::from_value(value)
        {
            return Ok(record);
        }
        let config = self.config.lock().unwrap().clone();
        let record = Record {
            sequence: self.lib.summary().map(|s| s.assets as u64 + 1).unwrap_or(1),
            template: config.template,
            settings: hot_folder::resolved(self.lib, &config.settings, self.previous.as_deref()),
        };
        let value = serde_json::to_value(&record).map_err(|e| e.to_string())?;
        staging::write_record(self.root, path, &value).map_err(|e| e.to_string())?;
        // A record written meanwhile (never, with one ingest per session)
        // would stand; read back what is kept.
        match staging::read_record(self.root, path) {
            Ok(Some(value)) => serde_json::from_value(value).map_err(|e| e.to_string()),
            _ => Ok(record),
        }
    }

    fn one(&mut self, path: &Path, captured: Option<(Captured, Instant)>) {
        let camera_name = captured
            .as_ref()
            .map(|(c, _)| c.camera_name.clone())
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let (result, asset) = match self.record(path) {
            Ok(record) => hot_folder::ingest_capture(
                self.lib,
                path,
                &record.template,
                record.sequence,
                &record.settings,
                None,
                None,
                &AtomicBool::new(false),
            ),
            Err(e) => (
                hot_folder::Imported {
                    source: path.into(),
                    destination: None,
                    skipped: false,
                    error: Some(format!("The capture could not be prepared: {e}")),
                },
                None,
            ),
        };
        if asset.is_some() || result.skipped {
            // In the library (or a verified duplicate of a photo there).
            if let Err(e) = staging::finish(self.root, path) {
                log::warn!("Tether: transfer copy {} not removed: {e}", path.display());
            }
        } else if let Some(error) = &result.error {
            log::warn!(
                "Tether: {} kept in the transfer folder, ingested on the next connect: {error}",
                path.display()
            );
        }
        if let Some(asset) = &asset {
            self.previous = Some(asset.clone());
        }
        let committed = asset.is_some() || result.skipped;
        let timing = captured.as_ref();
        let event = TetherCaptured {
            camera_name,
            destination: result.destination,
            asset_id: asset,
            skipped: result.skipped,
            error: result.error,
            requested: timing.is_some_and(|(c, _)| c.requested),
            ms_to_event: timing.and_then(|(c, _)| c.ms_to_event),
            ms_to_downloaded: timing.map(|(c, _)| c.ms_to_downloaded),
            ms_to_ingested: timing
                .map(|(c, received)| c.ms_to_downloaded + received.elapsed().as_millis() as u64),
        };
        (self.notify)("tether-captured", json!(event));
        // Only now that the photo is in the library (or a verified duplicate
        // of one there) may the camera let go of it.
        let free = self.config.lock().unwrap().free_camera_copies;
        if committed
            && free
            && let Some((captured, _)) = &captured
        {
            (self.release)(captured);
        }
    }
}

fn capture_settings(
    next_capture: NextCapture,
    metadata: Option<NextCaptureMetadata>,
) -> Result<CaptureSettings, String> {
    Ok(CaptureSettings {
        next_capture,
        metadata: metadata.unwrap_or_default().cleaned()?,
    })
}

fn check_template(template: &str) -> Result<(), String> {
    rawtonic_naming::render(template, &rawtonic_naming::Context::new("A.ARW", 1))
        .map(|_| ())
        .map_err(|e| format!("Filename template: {e}"))
}

/// Opens the camera on `port` (the first one when `None`) for the library
/// in `directory`: its photos are ingested into that session's Capture.
#[tauri::command]
pub async fn tether_connect(
    directory: String,
    port: Option<String>,
    template: String,
    next_capture: NextCapture,
    metadata: Option<NextCaptureMetadata>,
    free_camera_copies: Option<bool>,
    app: AppHandle,
) -> Result<TetherStatus, String> {
    CHOICE.fetch_add(1, Ordering::SeqCst);
    blocking(move || {
        check_template(&template)?;
        let settings = capture_settings(next_capture, metadata)?;
        let session = Path::new(&directory)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        drop(Library::open(&session).map_err(|e| format!("{e:#}"))?);
        let config = Arc::new(Mutex::new(CaptureConfig {
            template,
            settings,
            free_camera_copies: free_camera_copies.unwrap_or(true),
        }));
        let status = open_session(
            &HELPER,
            &TRANSITION,
            client(&app)?,
            notify_app(app.clone()),
            Opening {
                session: session.clone(),
                port,
                config,
            },
            || {
                let app = app.clone();
                std::thread::spawn(move || reconnect_when_back(app));
            },
        )?;
        Ok(TetherStatus {
            session,
            camera: status,
        })
    })
    .await
}

/// Forgets the connection here and waits for its ingest to finish the
/// photos it has (one ingest per transfer folder).
fn finish_ingest(state: &Mutex<Option<Helper>>) {
    let worker = state.lock().unwrap().as_mut().and_then(|h| {
        h.ingest.lock().unwrap().take();
        h.connection = None;
        h.lost = None;
        h.worker.take()
    });
    FRAME.lock().unwrap().take();
    if let Some(worker) = worker {
        let _ = worker.join();
    }
}

/// Opens a camera for `session` on the running helper: the earlier
/// connection's ingest finished, the transfer folder recovered while no
/// camera is open, the new ingest listening before the camera is (a body
/// shot right after the connect goes straight in).
/// What a connect opens: the camera on `port` (the first one when `None`)
/// for `session`, its photos ingested with `config`.
struct Opening {
    session: PathBuf,
    port: Option<String>,
    config: Arc<Mutex<CaptureConfig>>,
}

fn open_session(
    state: &Mutex<Option<Helper>>,
    transition: &Mutex<()>,
    client: Arc<Client>,
    notify: Notify,
    Opening {
        session,
        port,
        config,
    }: Opening,
    on_lost: impl FnOnce(),
) -> Result<CameraStatus, String> {
    let _turn = transition.lock().unwrap();
    finish_ingest(state);
    let _ = client.request(Request::Disconnect, QUICK);
    let recovered = staging::recover(&staging::root(&session)).unwrap_or_else(|e| {
        log::warn!("Tether: the transfer folder cannot be read: {e}");
        staging::Recovered::default()
    });
    let (tx, rx) = mpsc::channel();
    let worker = spawn_ingest(
        notify,
        release_via(Arc::downgrade(&client)),
        recovered,
        session.clone(),
        config.clone(),
        rx,
    )?;
    {
        let mut helper = state.lock().unwrap();
        let helper = helper.as_mut().ok_or("The tether helper stopped.")?;
        *helper.ingest.lock().unwrap() = Some(tx);
        helper.worker = Some(worker);
        helper.connecting = true;
        helper.lost_while_connecting = false;
    }
    let connect = Request::Connect {
        port,
        staging: staging::root(&session),
    };
    let status = match client.request(connect, SLOW) {
        Ok(Reply::Connected { status }) => status,
        other => {
            if other
                .as_ref()
                .is_err_and(|e| e.kind == ErrorKind::Unresponsive)
            {
                client.kill();
            }
            if let Some(h) = state.lock().unwrap().as_mut() {
                h.connecting = false;
            }
            finish_ingest(state);
            return Err(match other {
                Err(e) => text(e),
                Ok(_) => "Unexpected answer from the tether helper.".into(),
            });
        }
    };
    let mut helper = state.lock().unwrap();
    let helper = helper.as_mut().ok_or("The tether helper stopped.")?;
    helper.connecting = false;
    let connection = Connection {
        session,
        status: status.clone(),
        config,
    };
    if std::mem::take(&mut helper.lost_while_connecting) {
        // Gone again before it was installed: kept for the reconnect.
        helper.lost = Some(connection);
        on_lost();
        return Err(format!(
            "{} went away while connecting. It is connected again when it is back.",
            status.model
        ));
    }
    helper.connection = Some(connection);
    Ok(status)
}

#[tauri::command]
pub async fn tether_disconnect(app: AppHandle) -> Result<(), String> {
    CHOICE.fetch_add(1, Ordering::SeqCst);
    blocking(move || {
        let _turn = TRANSITION.lock().unwrap();
        let running = HELPER.lock().unwrap().is_some();
        if running {
            let _ = request(&app, Request::Disconnect, QUICK);
        }
        finish_ingest(&HELPER);
        Ok(())
    })
    .await
}

/// The connected camera, if any (for a window opened later).
#[tauri::command]
pub fn tether_status() -> Option<TetherStatus> {
    let helper = HELPER.lock().unwrap();
    let connection = helper.as_ref()?.connection.as_ref()?;
    Some(TetherStatus {
        session: connection.session.clone(),
        camera: connection.status.clone(),
    })
}

#[tauri::command]
pub async fn tether_settings(app: AppHandle) -> Result<Vec<Setting>, String> {
    blocking(move || match request(&app, Request::Settings, SLOW)? {
        Reply::Settings { settings } => Ok(settings),
        _ => Err("Unexpected answer from the tether helper.".into()),
    })
    .await
}

#[tauri::command]
pub async fn tether_set_setting(key: String, value: String, app: AppHandle) -> Result<(), String> {
    blocking(move || request(&app, Request::SetSetting { key, value }, QUICK).map(|_| ())).await
}

/// Fires the shutter; the photo arrives as `tether-captured`.
#[tauri::command]
pub async fn tether_capture(app: AppHandle) -> Result<(), String> {
    blocking(move || request(&app, Request::Capture, CAPTURE).map(|_| ())).await
}

#[tauri::command]
pub async fn tether_autofocus(app: AppHandle) -> Result<(), String> {
    blocking(move || request(&app, Request::Autofocus, QUICK).map(|_| ())).await
}

#[tauri::command]
pub async fn tether_live_view(on: bool, app: AppHandle) -> Result<(), String> {
    blocking(move || {
        if !on {
            FRAME.lock().unwrap().take();
        }
        request(&app, Request::LiveView { on }, QUICK).map(|_| ())
    })
    .await
}

/// The newest live-view frame (a camera JPEG), empty when there is none
/// yet. Each frame is handed out once.
#[tauri::command]
pub fn tether_live_view_frame() -> Response {
    Response::new(FRAME.lock().unwrap().take().unwrap_or_default())
}

/// Changes what the next photos are ingested with.
#[tauri::command]
pub fn tether_next_capture(
    template: String,
    next_capture: NextCapture,
    metadata: Option<NextCaptureMetadata>,
    free_camera_copies: Option<bool>,
) -> Result<(), String> {
    check_template(&template)?;
    let settings = capture_settings(next_capture, metadata)?;
    let helper = HELPER.lock().unwrap();
    let connection = helper
        .as_ref()
        .and_then(|h| h.connection.as_ref())
        .ok_or("No camera is connected.")?;
    *connection.config.lock().unwrap() = CaptureConfig {
        template,
        settings,
        free_camera_copies: free_camera_copies.unwrap_or(true),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use rawtonic_tether::mock::MockSpec;
    use std::collections::HashSet;
    use std::fs;

    /// The helper binary cargo built for the workspace (`--workspace`
    /// builds it for the tether crate's own tests).
    #[test]
    fn photos_are_freed_on_the_camera_after_their_commit_unless_turned_off() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let (tx, rx) = mpsc::channel::<(Captured, Instant)>();
        let freed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = freed.clone();
        let release: Release = Arc::new(move |_: &Captured| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "off"),
            free_camera_copies: false,
        }));
        let (notify, seen) = collector();
        spawn_ingest(
            notify,
            release,
            Default::default(),
            session.clone(),
            config.clone(),
            rx,
        )
        .unwrap();
        let staging = staging::root(&session);
        let send = |n: u64| {
            let path = staging.join(format!("incoming/{n:08}/DSCF{n:04}.RAF"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("photo {n}")).unwrap();
            let captured = Captured {
                path,
                connection: 1,
                camera_folder: "/store_10000001".into(),
                camera_name: format!("DSCF{n:04}.RAF"),
                size: 7,
                requested: true,
                ms_to_event: Some(1),
                ms_to_downloaded: 1,
            };
            tx.send((captured, Instant::now())).unwrap();
        };
        send(1);
        captured(&seen, 1);
        assert_eq!(freed.load(Ordering::SeqCst), 0, "turned off");
        config.lock().unwrap().free_camera_copies = true;
        send(2);
        captured(&seen, 1);
        // Freed right after the interface heard of the photo.
        let started = Instant::now();
        while freed.load(Ordering::SeqCst) == 0 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(freed.load(Ordering::SeqCst), 1, "freed after its commit");
    }

    #[test]
    fn an_unplugged_camera_is_connected_again_when_it_is_back() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let spec = MockSpec {
            file_size: 16 * 1024,
            unplug_after: Some(2),
            ..MockSpec::default()
        };
        let (events_tx, events_rx) = mpsc::channel::<Event>();
        let events_tx = Mutex::new(events_tx);
        let (client, _) = Client::spawn(
            &helper_binary(),
            &["--mock".into()],
            &[(
                "RAWTONIC_TETHER_MOCK".into(),
                serde_json::to_string(&spec).unwrap().into(),
            )],
            move |incoming| {
                if let Incoming::Event(event) = incoming {
                    let _ = events_tx.lock().unwrap().send(event);
                }
            },
        )
        .unwrap();
        let connect = Request::Connect {
            port: None,
            staging: staging::root(&session),
        };
        client.request(connect, SLOW).unwrap();
        let mut files = 0;
        let next = |files: &mut usize| loop {
            match events_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("an event")
            {
                Event::Captured { .. } => *files += 1,
                Event::Disconnected { error } => return Some(error),
                _ => {}
            }
            if *files == 3 {
                return None;
            }
        };
        client.request(Request::Capture, CAPTURE).unwrap();
        client.request(Request::Capture, CAPTURE).unwrap();
        let error = next(&mut files).expect("the camera went away");
        assert_eq!(error.kind, ErrorKind::Disconnected);
        assert_eq!(files, 2);
        let transition = Mutex::new(());
        // A user's disconnect meanwhile stops the reconnect.
        assert!(
            reconnect(
                &client,
                &spec.model,
                Some("MOCK0001"),
                &session,
                Duration::ZERO,
                &transition,
                || false
            )
            .is_none()
        );
        // One made while the camera is being looked for: no connect follows.
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let changed_during_the_look = || looks.fetch_add(1, Ordering::SeqCst) == 0;
        assert!(
            reconnect(
                &client,
                &spec.model,
                Some("MOCK0001"),
                &session,
                Duration::ZERO,
                &transition,
                changed_during_the_look
            )
            .is_none()
        );
        assert_eq!(
            client.request(Request::Settings, QUICK).unwrap_err().kind,
            ErrorKind::NotConnected,
            "not connected behind the user's back"
        );
        let (status, turn) = reconnect(
            &client,
            &spec.model,
            Some("MOCK0001"),
            &session,
            Duration::from_millis(50),
            &transition,
            || true,
        )
        .expect("connected again");
        assert!(
            transition.try_lock().is_err(),
            "held until the state is updated"
        );
        drop(turn);
        assert_eq!(status.model, spec.model);
        client.request(Request::Capture, CAPTURE).unwrap();
        assert!(next(&mut files).is_none());
        assert_eq!(files, 3, "the camera fires again after the reconnect");
    }

    /// A helper on the mock camera as the app runs it: its photos go to
    /// whichever ingest is installed.
    fn mock_helper(spec: &MockSpec) -> (Arc<Mutex<Option<Helper>>>, Arc<Client>) {
        mock_helper_on_lost(spec, || {})
    }

    /// `mock_helper` whose lost camera starts `on_lost` the way
    /// `on_incoming` starts a reconnect.
    fn mock_helper_on_lost(
        spec: &MockSpec,
        on_lost: impl Fn() + Send + Sync + 'static,
    ) -> (Arc<Mutex<Option<Helper>>>, Arc<Client>) {
        let ingest: IngestSender = Arc::new(Mutex::new(None));
        let route = ingest.clone();
        let state: Arc<Mutex<Option<Helper>>> = Arc::new(Mutex::new(None));
        let events = state.clone();
        let (client, library) = Client::spawn(
            &helper_binary(),
            &["--mock".into()],
            &[(
                "RAWTONIC_TETHER_MOCK".into(),
                serde_json::to_string(spec).unwrap().into(),
            )],
            move |incoming| match incoming {
                Incoming::Event(Event::Captured { captured }) => {
                    if let Some(tx) = route.lock().unwrap().as_ref() {
                        let _ = tx.send((captured, Instant::now()));
                    }
                }
                Incoming::Event(Event::Disconnected { .. }) if camera_lost(&events) => on_lost(),
                _ => {}
            },
        )
        .unwrap();
        let client = Arc::new(client);
        *state.lock().unwrap() = Some(Helper {
            client: client.clone(),
            library,
            ingest,
            connection: None,
            lost: None,
            worker: None,
            connecting: false,
            lost_while_connecting: false,
        });
        (state, client)
    }

    #[test]
    fn the_reply_and_the_event_of_one_loss_start_one_reconnect() {
        let (state, _client) = mock_helper(&MockSpec::default());
        state.lock().unwrap().as_mut().unwrap().connection = Some(Connection {
            session: PathBuf::from("/job"),
            status: CameraStatus::default(),
            config: Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "x"),
                free_camera_copies: true,
            })),
        });
        // Whichever of the failed request and the event comes first.
        assert!(camera_lost(&state));
        assert!(!camera_lost(&state));
        assert!(state.lock().unwrap().as_ref().unwrap().lost.is_some());
    }

    #[test]
    fn a_reconnect_takes_only_the_same_body() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let (_state, client) = mock_helper(&MockSpec::default());
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let transition = Mutex::new(());
        // Another body of the same model (serial MOCK0001) is attached.
        let found = reconnect(
            &client,
            "Mock Camera",
            Some("MOCK0002"),
            &session,
            Duration::ZERO,
            &transition,
            || looks.fetch_add(1, Ordering::SeqCst) < 6,
        );
        assert!(found.is_none());
        assert_eq!(
            client.request(Request::Settings, QUICK).unwrap_err().kind,
            ErrorKind::NotConnected,
            "the other body was let go"
        );
    }

    #[test]
    fn a_camera_lost_right_after_its_connect_is_connected_again_either_way() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        // A body shot at the connect, then the cable comes out.
        let spec = MockSpec {
            body_shots_ms: vec![0],
            unplug_after: Some(1),
            ..MockSpec::default()
        };
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let from_event = starts.clone();
        let (state, client) = mock_helper_on_lost(&spec, move || {
            from_event.fetch_add(1, Ordering::SeqCst);
        });
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "lost"),
            free_camera_copies: false,
        }));
        let (notify, seen) = collector();
        let from_connect = starts.clone();
        let _ = open_session(
            &state,
            &Mutex::new(()),
            client,
            notify,
            Opening {
                session: session.clone(),
                port: None,
                config,
            },
            move || {
                from_connect.fetch_add(1, Ordering::SeqCst);
            },
        );
        captured(&seen, 1);
        let started = Instant::now();
        while starts.load(Ordering::SeqCst) == 0 && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "one reconnect, whichever came first"
        );
        let helper = state.lock().unwrap();
        let helper = helper.as_ref().unwrap();
        assert!(helper.connection.is_none() && helper.lost.is_some());
        assert!(!helper.connecting);
    }

    #[test]
    fn a_new_connection_ingests_what_was_left_and_a_shot_right_after_it_once_each() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        // Left by an earlier run, complete.
        let root = staging::root(&session);
        let mut left = staging::Partial::create(&root, 1, "LEFT0001.ARW").unwrap();
        left.write(b"left from before").unwrap();
        left.publish(16).unwrap();
        // A body shot fired the moment the camera is open.
        let spec = MockSpec {
            file_size: 16 * 1024,
            body_shots_ms: vec![0],
            ..MockSpec::default()
        };
        let (state, client) = mock_helper(&spec);
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "connect"),
            free_camera_copies: false,
        }));
        let (notify, seen) = collector();
        let transition = Mutex::new(());
        open_session(
            &state,
            &transition,
            client.clone(),
            notify.clone(),
            Opening {
                session: session.clone(),
                port: None,
                config: config.clone(),
            },
            || {},
        )
        .unwrap();
        let seen_now = captured(&seen, 2);
        assert!(
            seen_now.iter().all(|c| c.error.is_none() && !c.skipped),
            "{seen_now:?}"
        );
        // Connecting again waits for that ingest before reading the folder
        // (the mock fires its body shot again on this connect).
        open_session(
            &state,
            &transition,
            client,
            notify,
            Opening {
                session: session.clone(),
                port: None,
                config,
            },
            || {},
        )
        .unwrap();
        // Byte-identical to the first one (the mock's numbering and content
        // restart per connection): recognised, not copied again.
        let again = captured(&seen, 1);
        assert!(again[0].skipped && again[0].error.is_none(), "{again:?}");
        finish_ingest(&state);
        assert_eq!(photos_in_capture(&session), 2, "each once");
        assert_eq!(incoming_left(&session), 0);
    }

    #[test]
    fn a_release_never_reaches_a_camera_of_a_later_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let spec = MockSpec {
            queue_limit: Some(1),
            ..MockSpec::default()
        };
        let staging = staging::root(&session);
        let first = {
            let (_state, client) = mock_helper(&spec);
            client
                .request(
                    Request::Connect {
                        port: None,
                        staging: staging.clone(),
                    },
                    SLOW,
                )
                .unwrap();
            let release = release_via(Arc::downgrade(&client));
            (
                release,
                Captured {
                    path: staging.join("x"),
                    connection: 1,
                    camera_folder: "/".into(),
                    camera_name: "DSC00001.ARW".into(),
                    size: 1,
                    requested: true,
                    ms_to_event: None,
                    ms_to_downloaded: 0,
                },
            )
        };
        // That helper ended; a new one numbers its first connection 1 too.
        let (_state, client) = mock_helper(&spec);
        client
            .request(
                Request::Connect {
                    port: None,
                    staging,
                },
                SLOW,
            )
            .unwrap();
        client.request(Request::Capture, CAPTURE).unwrap();
        let (release, captured) = first;
        release(&captured);
        assert!(
            client.request(Request::Capture, CAPTURE).is_err(),
            "the new camera's photo was not freed"
        );
    }

    fn helper_binary() -> PathBuf {
        let deps = std::env::current_exe().unwrap();
        let path = deps
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(rawtonic_tether::HELPER);
        assert!(
            path.is_file(),
            "{} is missing: run the tests with --workspace (it builds the tether helper)",
            path.display()
        );
        path
    }

    fn session(root: &Path) -> PathBuf {
        let dir = root.join("Job");
        Library::create(&dir, "Job", Mode::Session).unwrap();
        dir.canonicalize().unwrap()
    }

    fn settings(rating: u8, keyword: &str) -> CaptureSettings {
        CaptureSettings {
            next_capture: NextCapture::Defaults,
            metadata: NextCaptureMetadata {
                rating,
                keywords: vec![keyword.into()],
            },
        }
    }

    /// Collects what the ingest tells the interface.
    fn collector() -> (Notify, mpsc::Receiver<(String, serde_json::Value)>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let notify: Notify = Arc::new(move |name: &str, value| {
            let _ = tx.lock().unwrap().send((name.to_string(), value));
        });
        (notify, rx)
    }

    fn captured(
        rx: &mpsc::Receiver<(String, serde_json::Value)>,
        n: usize,
    ) -> Vec<TetherCapturedSeen> {
        let mut seen = Vec::new();
        while seen.len() < n {
            let (name, value) = rx.recv_timeout(Duration::from_secs(30)).expect("a capture");
            if name == "tether-captured" {
                seen.push(serde_json::from_value(value).unwrap());
            }
        }
        seen
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TetherCapturedSeen {
        asset_id: Option<String>,
        destination: Option<PathBuf>,
        error: Option<String>,
        skipped: bool,
        ms_to_ingested: Option<u64>,
    }

    /// The photographs in Capture (not their XMP and editor sidecars).
    fn photos_in_capture(session: &Path) -> usize {
        fs::read_dir(session.join("Capture"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.ends_with(".ARW") && !name.contains(".ARW.")
            })
            .count()
    }

    fn incoming_left(session: &Path) -> usize {
        fs::read_dir(staging::root(session).join(staging::INCOMING))
            .map(|d| d.count())
            .unwrap_or(0)
    }

    #[test]
    fn two_hundred_shots_arrive_once_each_with_their_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        // The camera holds at most 10 photos until they are released after
        // their commit: all 200 arrive only if every one is released.
        let spec = MockSpec {
            file_size: 16 * 1024,
            restart_numbering_every: Some(50),
            body_shots_ms: vec![300, 900],
            queue_limit: Some(10),
            ..MockSpec::default()
        };
        let (photos_tx, photos_rx) = mpsc::channel::<(Captured, Instant)>();
        let photos_tx = Mutex::new(photos_tx);
        let (client, _) = Client::spawn(
            &helper_binary(),
            &["--mock".into()],
            &[(
                "RAWTONIC_TETHER_MOCK".into(),
                serde_json::to_string(&spec).unwrap().into(),
            )],
            move |incoming| {
                if let Incoming::Event(Event::Captured { captured }) = incoming {
                    let _ = photos_tx.lock().unwrap().send((captured, Instant::now()));
                }
            },
        )
        .unwrap();
        client
            .request(
                Request::Connect {
                    port: None,
                    staging: staging::root(&session),
                },
                SLOW,
            )
            .unwrap();
        let (notify, seen) = collector();
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(3, "tether"),
            free_camera_copies: true,
        }));
        let client = Arc::new(client);
        // Weak: the ingest thread must not keep the helper alive.
        let releasing = Arc::downgrade(&client);
        let release: Release = Arc::new(move |captured: &Captured| {
            let release = Request::Release {
                connection: captured.connection,
                camera_folder: captured.camera_folder.clone(),
                camera_name: captured.camera_name.clone(),
            };
            if let Some(client) = releasing.upgrade() {
                client.request(release, QUICK).unwrap();
            }
        });
        spawn_ingest(
            notify,
            release,
            Default::default(),
            session.clone(),
            config,
            photos_rx,
        )
        .unwrap();
        for _ in 0..198 {
            // A full queue refuses until the ingest has released a photo.
            let started = Instant::now();
            while let Err(e) = client.request(Request::Capture, CAPTURE) {
                assert_eq!(e.kind, ErrorKind::Other, "{e:?}");
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "never released"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // 198 from the app and 2 fired on the body.
        let seen = captured(&seen, 200);
        assert!(
            seen.iter().all(|c| c.error.is_none() && !c.skipped),
            "{seen:?}"
        );
        assert!(seen.iter().all(|c| c.ms_to_ingested.is_some()));
        let destinations: HashSet<PathBuf> = seen
            .iter()
            .map(|c| c.destination.clone().unwrap())
            .collect();
        assert_eq!(destinations.len(), 200, "no photo replaced another");
        let lib = Library::open(&session).unwrap();
        let assets = lib.assets(None).unwrap();
        assert_eq!(assets.len(), 200);
        let ids: HashSet<String> = seen.iter().map(|c| c.asset_id.clone().unwrap()).collect();
        assert_eq!(ids.len(), 200);
        for asset in &assets {
            assert_eq!(asset.rating, 3, "{}", asset.rel_path);
            assert!(asset.rel_path.starts_with("Capture/"), "{}", asset.rel_path);
        }
        // The camera restarted its numbering every 50 shots: the later ones
        // got the collision suffix, never an earlier file's place.
        assert!(
            destinations
                .iter()
                .any(|d| d.ends_with("Capture/DSC00001_3.ARW"))
        );
        assert_eq!(photos_in_capture(&session), 200);
        assert_eq!(incoming_left(&session), 0, "every transfer copy removed");
    }

    #[test]
    fn a_crash_between_copy_and_commit_is_recorded_once_with_what_it_was_given() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let root = staging::root(&session);
        // A download the helper published, with the record the ingest
        // kept before it started...
        let mut partial = staging::Partial::create(&root, 1, "DSC00007.ARW").unwrap();
        partial.write(b"the shot that crashed").unwrap();
        let staged = partial.publish(21).unwrap();
        let record = Record {
            sequence: 1,
            template: "{seq}_{orig}".into(),
            settings: settings(4, "before"),
        };
        staging::write_record(&root, &staged, &serde_json::to_value(&record).unwrap()).unwrap();
        // ...and the verified copy that crash left in Capture, unrecorded.
        let orphan = session.join("Capture").join("1_DSC00007.ARW");
        fs::write(&orphan, b"the shot that crashed").unwrap();
        // Meanwhile another import advanced the sequence...
        let other = tmp.path().join("Other.ARW");
        fs::write(&other, b"another photo").unwrap();
        {
            let lib = Library::open(&session).unwrap();
            let (r, _) = hot_folder::ingest_capture(
                &lib,
                &other,
                "{orig}",
                1,
                &CaptureSettings::default(),
                None,
                None,
                &AtomicBool::new(false),
            );
            assert!(r.error.is_none());
        }
        // ...and the next-capture settings changed.
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}_{seq}".into(),
            settings: settings(1, "after"),
            free_camera_copies: false,
        }));
        let (notify, seen) = collector();
        let (_tx, rx) = mpsc::channel();
        spawn_ingest(
            notify,
            Arc::new(|_: &Captured| {}),
            staging::recover(&staging::root(&session)).unwrap(),
            session.clone(),
            config,
            rx,
        )
        .unwrap();
        let seen = captured(&seen, 1);
        assert!(seen[0].error.is_none(), "{seen:?}");
        assert_eq!(seen[0].destination.as_deref(), Some(orphan.as_path()));
        let lib = Library::open(&session).unwrap();
        let asset = lib.asset_at(&orphan).unwrap().expect("recorded");
        assert_eq!(asset.rating, 4, "with what it was given before the crash");
        assert_eq!(photos_in_capture(&session), 2, "no second copy");
        assert!(!staged.exists());
        assert_eq!(incoming_left(&session), 0);
    }

    #[test]
    fn a_partial_download_is_never_ingested() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let root = staging::root(&session);
        let mut partial = staging::Partial::create(&root, 1, "DSC00001.ARW").unwrap();
        partial.write(b"half").unwrap();
        partial.abandon();
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: CaptureSettings::default(),
            free_camera_copies: false,
        }));
        let (notify, seen) = collector();
        let (_tx, rx) = mpsc::channel();
        spawn_ingest(
            notify,
            Arc::new(|_: &Captured| {}),
            staging::recover(&staging::root(&session)).unwrap(),
            session.clone(),
            config,
            rx,
        )
        .unwrap();
        let (name, path) = seen.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(name, "tether-incomplete");
        let path = PathBuf::from(path.as_str().unwrap());
        assert!(path.starts_with(root.join(staging::INCOMPLETE)));
        assert_eq!(fs::read(&path).unwrap(), b"half");
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err());
        assert_eq!(photos_in_capture(&session), 0);
    }
}
