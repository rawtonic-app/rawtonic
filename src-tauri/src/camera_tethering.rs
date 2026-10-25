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

/// A photo the helper delivered, with when it arrived and the next-capture
/// settings of that moment: a preset, rating or keywords changed while
/// earlier photos still wait for their ingest apply to the photos after.
struct Arrival {
    captured: Captured,
    received: Instant,
    config: CaptureConfig,
}

impl Arrival {
    fn now(captured: Captured, config: &Mutex<CaptureConfig>) -> Self {
        Self {
            captured,
            received: Instant::now(),
            config: config.lock().unwrap().clone(),
        }
    }
}

/// The connection's ingest as the helper's photos reach it.
struct Inlet {
    tx: mpsc::Sender<Arrival>,
    config: Arc<Mutex<CaptureConfig>>,
}

impl Inlet {
    /// False when the ingest has ended.
    fn send(&self, captured: Captured) -> bool {
        self.tx.send(Arrival::now(captured, &self.config)).is_ok()
    }
}

/// Where the helper's photos go: the connection's ingest, when there is one.
type IngestSender = Arc<Mutex<Option<Inlet>>>;

/// The helper's path, arguments and environment.
type HelperCommand = (PathBuf, Vec<OsString>, Vec<(OsString, OsString)>);

struct Connection {
    /// This connection, distinct from every other one of this app run (a
    /// late failure of an earlier one never touches it).
    id: u64,
    session: PathBuf,
    status: CameraStatus,
    config: Arc<Mutex<CaptureConfig>>,
}

impl Connection {
    fn new(session: PathBuf, status: CameraStatus, config: Arc<Mutex<CaptureConfig>>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::SeqCst),
            session,
            status,
            config,
        }
    }
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
    /// This helper process among all started in this app run.
    generation: u64,
    /// The connection's ingest thread (joined before another one starts:
    /// one ingest per transfer folder).
    worker: Option<std::thread::JoinHandle<()>>,
    /// A connect is waiting for the helper; `lost_while_connecting` notes a
    /// camera lost before its connection was installed.
    connecting: Option<u64>,
    lost_while_connecting: bool,
}

/// A connect attempt's own mark for `Helper::connecting`: only the attempt
/// that set it clears it (a cancelled reconnect never clears a newer
/// connect's).
fn connect_attempt() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Clears `connecting` if `attempt` set it; whether the camera was lost
/// meanwhile.
fn end_attempt(h: &mut Helper, attempt: u64) -> bool {
    if h.connecting != Some(attempt) {
        return false;
    }
    h.connecting = None;
    std::mem::take(&mut h.lost_while_connecting)
}

/// The camera went away: its connection is kept for connecting it again.
/// Whether a reconnect should start now (a connect still in flight starts
/// it itself once it has installed the connection).
fn camera_lost(state: &Mutex<Option<Helper>>, only: Option<u64>) -> bool {
    let mut state = state.lock().unwrap();
    let Some(h) = state.as_mut() else {
        return false;
    };
    // A request's late failure concerns only the connection it was sent on.
    if let Some(id) = only
        && h.connection.as_ref().is_none_or(|c| c.id != id)
    {
        return false;
    }
    if let Some(connection) = h.connection.take() {
        h.lost = Some(connection);
        return true;
    }
    if h.connecting.is_some() {
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

/// Whether `generation` is the running helper (a late message of one that
/// was replaced concerns no camera of now).
fn current_helper(state: &Mutex<Option<Helper>>, generation: u64) -> bool {
    state
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|h| h.generation == generation)
}

/// Emits the helper's events; photos go to the connection's ingest.
fn on_incoming(
    app: AppHandle,
    ingest: IngestSender,
    generation: u64,
) -> impl Fn(Incoming) + Send + Sync + 'static {
    move |incoming| match incoming {
        Incoming::Event(Event::Captured { captured }) => {
            let sent = ingest
                .lock()
                .unwrap()
                .as_ref()
                .map(|inlet| inlet.send(captured.clone()));
            if sent != Some(true) {
                // Kept in the transfer folder; ingested on the next connect.
                log::warn!(
                    "Tether: {} arrived without a session; kept for the next connect",
                    captured.camera_name
                );
            }
        }
        Incoming::Event(Event::CaptureFailed {
            error,
            request,
            exposure,
            ambiguous,
        }) => {
            log::warn!(
                "Tether: shot not delivered (request {request:?}, exposure {exposure:?}): {:?}: {}",
                error.kind,
                error.message
            );
            let _ = app.emit(
                "tether-capture-failed",
                json!({
                    "kind": error.kind,
                    "message": error.message,
                    "request": request,
                    "connection": exposure.map(|(connection, _)| connection),
                    "exposure": exposure.map(|(_, exposure)| exposure),
                    "ambiguous": ambiguous,
                }),
            );
        }
        Incoming::Event(Event::SettingsChanged) => {
            let _ = app.emit("tether-settings-changed", ());
        }
        Incoming::Event(Event::Disconnected { error }) => {
            if !current_helper(&HELPER, generation) {
                return;
            }
            let _ = app.emit(
                "tether-state",
                json!({ "connected": false, "error": error }),
            );
            if error.kind == ErrorKind::Disconnected && camera_lost(&HELPER, None) {
                let app = app.clone();
                let choice = CHOICE.load(Ordering::SeqCst);
                std::thread::spawn(move || reconnect_when_back(app, choice));
            }
        }
        Incoming::Event(Event::LiveViewStopped { error }) => {
            let _ = app.emit("tether-live-view-stopped", error);
        }
        Incoming::LiveView(jpeg) => *FRAME.lock().unwrap() = Some(jpeg),
        Incoming::Exited(log) => {
            log::warn!("Tether helper ended. Last log lines:\n{log}");
            if !current_helper(&HELPER, generation) {
                return;
            }
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
    static NEXT_HELPER: AtomicU64 = AtomicU64::new(1);
    let generation = NEXT_HELPER.fetch_add(1, Ordering::SeqCst);
    let (path, args, envs) = helper_command(app)?;
    let ingest = Arc::new(Mutex::new(None));
    let (client, library) = Client::spawn(
        &path,
        &args,
        &envs,
        on_incoming(app.clone(), ingest.clone(), generation),
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
        connecting: None,
        lost_while_connecting: false,
        generation,
    });
    Ok(client)
}

/// After the camera went away (cable, power): connects the same model again
/// when it is back, into the same session with the same capture settings
/// and the same ingest — unless the user connected or disconnected
/// meanwhile or the helper ended (Capture One reconnects on its own too).
/// `choice`: the user's connects and disconnects counted when the camera
/// was lost; a later connect or disconnect cancels this reconnect.
fn reconnect_when_back(app: AppHandle, choice: u64) {
    reconnect_lost(
        &HELPER,
        &TRANSITION,
        RECONNECT_EVERY,
        || CHOICE.load(Ordering::SeqCst) == choice,
        |status| {
            let _ = app.emit(
                "tether-state",
                json!({ "connected": true, "status": status }),
            );
        },
    );
}

/// `reconnect_when_back` over `state`: looks for the lost connection's
/// body until it is connected again (`connected` hears it) or no longer
/// `wanted`. A body lost again right after its Connected reply, before it
/// was installed, stays lost and is looked for again.
fn reconnect_lost(
    state: &Mutex<Option<Helper>>,
    transition: &Mutex<()>,
    every: Duration,
    wanted: impl Fn() -> bool,
    connected: impl FnOnce(TetherStatus),
) {
    // The lost connection this reconnect is for (set at its first look).
    let mut for_lost: Option<u64> = None;
    loop {
        let attempt = connect_attempt();
        // Checked under the transition lock before touching the state: a
        // connect or disconnect of the user's since the loss wins.
        let turn = transition.lock().unwrap();
        if !wanted() {
            return;
        }
        let (client, session, model, serial, config) = {
            let mut helper = state.lock().unwrap();
            let Some(h) = helper.as_mut() else { return };
            // Only a lost connection, and only the one this reconnect is
            // for: a connection installed meanwhile is never taken.
            if h.connection.is_some() {
                return;
            }
            let Some(c) = h.lost.as_ref() else {
                return;
            };
            if for_lost.is_some_and(|id| id != c.id) {
                return;
            }
            for_lost = Some(c.id);
            h.connecting = Some(attempt);
            h.lost_while_connecting = false;
            (
                h.client.clone(),
                c.session.clone(),
                c.status.model.clone(),
                c.status.serial.clone(),
                c.config.clone(),
            )
        };
        drop(turn);
        let found = reconnect(
            &client,
            &model,
            serial.as_deref(),
            &session,
            every,
            QUICK,
            transition,
            &wanted,
        );
        let mut helper = state.lock().unwrap();
        let Some(h) = helper.as_mut() else { return };
        let lost_again = end_attempt(h, attempt);
        let Some((status, _turn)) = found else {
            return;
        };
        if !wanted() || !Arc::ptr_eq(&h.client, &client) {
            return;
        }
        if lost_again {
            // Gone again before it was installed: still lost, look again.
            continue;
        }
        h.lost = None;
        h.connection = Some(Connection::new(session.clone(), status.clone(), config));
        log::info!("Tether: {model} is back, connected again");
        let library = h.library.clone();
        drop(helper);
        connected(TetherStatus {
            session,
            camera: status,
            connected: true,
            transport: transport(library.as_deref()),
            library,
        });
        return;
    }
}

/// Looks for `model` every `every` and connects it once it is listed again;
/// `None` when the helper ended or `wanted` turned false first. A helper
/// that does not answer a look within `look` (or a connect) is ended: a
/// hung one is not asked again every few seconds forever.
#[allow(clippy::too_many_arguments)]
fn reconnect<'a>(
    client: &Client,
    model: &str,
    serial: Option<&str>,
    session: &Path,
    every: Duration,
    look: Duration,
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
        let cameras = match client.request(Request::ListCameras, look) {
            Ok(Reply::Cameras { cameras }) => cameras,
            Err(e) if e.kind == ErrorKind::Unresponsive => {
                log::warn!(
                    "Tether: the helper does not answer; it is ended ({})",
                    e.message
                );
                client.kill();
                return None;
            }
            _ => continue,
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
            // The helper closes another body of the same model again
            // before reading or downloading anything of it.
            let connect = Request::Connect {
                port: Some(camera.port),
                staging: staging::root(session),
                serial: serial.map(String::from),
            };
            match client.request(connect, SLOW) {
                Ok(Reply::Connected { status }) => return Some((status, turn)),
                Err(e) if e.kind == ErrorKind::Unresponsive => {
                    client.kill();
                    return None;
                }
                _ => continue,
            }
        }
    }
}

/// A request to the connected camera: sent only while no connection change
/// runs, and only to the connection that was current when it was asked for
/// (a command never reaches a camera connected meanwhile).
fn camera_request(app: &AppHandle, request: Request, timeout: Duration) -> Result<Reply, String> {
    let current = || {
        HELPER
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.connection.as_ref().map(|c| c.id))
    };
    let Some(asked_for) = current() else {
        return Err("No camera is connected.".into());
    };
    let _turn = TRANSITION.lock().unwrap();
    if current() != Some(asked_for) {
        return Err("The camera was connected again meanwhile; try once more.".into());
    }
    self::request(app, request, timeout)
}

/// Sends a request to the helper; one that hangs is ended (its next use
/// starts a new helper).
fn request(app: &AppHandle, request: Request, timeout: Duration) -> Result<Reply, String> {
    let client = client(app)?;
    // The connection (and the user's choices) this request is sent on.
    let on = HELPER
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|h| h.connection.as_ref().map(|c| c.id));
    let choice = CHOICE.load(Ordering::SeqCst);
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
            // follows this reply): whichever is first starts the reconnect —
            // for this request's connection only.
            if on.is_some() && camera_lost(&HELPER, on) {
                let app = app.clone();
                std::thread::spawn(move || reconnect_when_back(app, choice));
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
    /// False while the camera is away and waits to be connected again.
    pub connected: bool,
    /// libgphoto2's version (`None`: the mock camera).
    pub library: Option<String>,
    /// How the helper reaches the camera: `imageCaptureCore` (macOS's
    /// camera service, our libgphoto2 port driver), `usb` (libusb) or
    /// `mock`.
    pub transport: &'static str,
}

/// `TetherStatus::transport` for a helper with this camera library.
fn transport(library: Option<&str>) -> &'static str {
    match library {
        None => "mock",
        Some(_) if cfg!(target_os = "macos") => "imageCaptureCore",
        Some(_) => "usb",
    }
}

/// Tells the interface (`AppHandle::emit` in the app).
type Notify = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;
/// Frees a committed photo from the camera's transfer queue (the helper's
/// `Release`; a no-op on cameras without one): whether it was freed.
type Release = Arc<dyn Fn(&Captured) -> Result<bool, String> + Send + Sync>;

/// `Release` through the helper that delivered the photo: none once that
/// helper ended (a new one numbers its connections afresh).
fn release_via(client: std::sync::Weak<Client>) -> Release {
    Arc::new(move |captured: &Captured| {
        let Some(client) = client.upgrade() else {
            return Err("The tether helper ended.".into());
        };
        let release = Request::Release {
            connection: captured.connection,
            download: captured.download,
            camera_folder: captured.camera_folder.clone(),
            camera_name: captured.camera_name.clone(),
        };
        match client.request(release, QUICK) {
            Ok(Reply::Released { freed }) => Ok(freed),
            Ok(_) => Err("Unexpected answer from the tether helper.".into()),
            Err(e) => {
                log::warn!(
                    "Tether: {} not freed on the camera: {}",
                    captured.camera_name,
                    e.message
                );
                Err(e.message)
            }
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
    photos: mpsc::Receiver<Arrival>,
) -> Result<std::thread::JoinHandle<()>, String> {
    let root = staging::root(&session);
    // The worker says whether it could open the library before the camera
    // is opened: no photo is ever downloaded with nobody to take it in.
    let (started_tx, started_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let worker = std::thread::Builder::new()
        .name("tether-ingest".into())
        .spawn(move || {
            let lib = match Library::open(&session) {
                Ok(lib) => {
                    let _ = started_tx.send(Ok(()));
                    lib
                }
                Err(e) => {
                    let _ = started_tx.send(Err(format!(
                        "The library cannot take tethered photos: {e:#}"
                    )));
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
                log::warn!("Tether: incomplete download {} kept", file_name(&path));
                notify("tether-incomplete", json!(path));
            }
            for path in recovered.complete {
                ingest.one(&path, None);
            }
            while let Ok(arrival) = photos.recv() {
                ingest.one(&arrival.captured.path.clone(), Some(arrival));
            }
        })
        .map_err(|e| e.to_string())?;
    match started_rx.recv() {
        Ok(Ok(())) => Ok(worker),
        Ok(Err(e)) => {
            let _ = worker.join();
            Err(e)
        }
        Err(_) => Err("The tether ingest did not start.".into()),
    }
}

/// A path's file name, for the log (never the whole path: it names the
/// user's folders).
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One photo the tether brought in, for the interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherCaptured {
    /// The library it went into.
    pub session: PathBuf,
    pub camera_name: String,
    pub destination: Option<PathBuf>,
    pub asset_id: Option<String>,
    pub skipped: bool,
    pub error: Option<String>,
    /// Fired by the app (else on the body, or left from an earlier run).
    pub requested: bool,
    /// The helper's connection and exposure numbers (the RAW and JPEG of
    /// one exposure share them); none for a photo left from an earlier run.
    pub connection: Option<u64>,
    pub exposure: Option<u64>,
    /// The exposures it may belong to when that cannot be told (a reused
    /// camera file name); empty when certain.
    pub ambiguous: Vec<u64>,
    /// The app request it answers (`tether_capture`'s number), if any.
    pub request: Option<u64>,
    /// From the app's request (or the camera's report) to: the camera's
    /// report, the complete download, the photo in the library.
    pub ms_to_event: Option<u64>,
    pub ms_to_downloaded: Option<u64>,
    pub ms_to_ingested: Option<u64>,
}

/// A committed photo's release on the camera (`Request::Release`), told
/// after its `TetherCaptured`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherReleased {
    pub camera_name: String,
    /// Its photo's helper connection and exposure (as in `TetherCaptured`),
    /// so the release can be told apart from another file of that name.
    pub connection: u64,
    pub exposure: u64,
    /// Freed from the camera's transfer queue (false: it keeps none).
    pub freed: bool,
    pub error: Option<String>,
}

struct Ingest<'a> {
    notify: &'a (dyn Fn(&str, serde_json::Value) + Send + Sync),
    release: &'a (dyn Fn(&Captured) -> Result<bool, String> + Send + Sync),
    lib: &'a Library,
    root: &'a Path,
    config: &'a Mutex<CaptureConfig>,
    /// The last capture (Copy from last capture).
    previous: Option<String>,
}

impl Ingest<'_> {
    /// The record kept with a transfer file, written now if there is none:
    /// with the settings the photo arrived with (`arrived`), else (a photo
    /// left from an earlier run) the settings of now.
    fn record(&self, path: &Path, arrived: Option<&CaptureConfig>) -> Result<Record, String> {
        if let Ok(Some(value)) = staging::read_record(self.root, path)
            && let Ok(record) = serde_json::from_value(value)
        {
            return Ok(record);
        }
        let config = match arrived {
            Some(config) => config.clone(),
            None => self.config.lock().unwrap().clone(),
        };
        let record = Record {
            sequence: self
                .lib
                .registered_asset_count()
                .map(|count| count as u64 + 1)
                .unwrap_or(1),
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

    fn one(&mut self, path: &Path, arrival: Option<Arrival>) {
        let (captured, arrived) = match arrival {
            Some(Arrival {
                captured,
                received,
                config,
            }) => (Some((captured, received)), Some(config)),
            None => (None, None),
        };
        let camera_name = captured
            .as_ref()
            .map(|(c, _)| c.camera_name.clone())
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let (result, asset) = match self.record(path, arrived.as_ref()) {
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
                log::warn!("Tether: transfer copy {camera_name} not removed: {e}");
            }
        } else if let Some(error) = &result.error {
            log::warn!(
                "Tether: {camera_name} kept in the transfer folder, ingested on the next connect: {error}"
            );
        }
        if let Some(asset) = &asset {
            self.previous = Some(asset.clone());
        }
        let committed = asset.is_some() || result.skipped;
        // Decided with the commit, before the interface hears of the photo:
        // a setting changed after that applies to the next photo.
        let free = self.config.lock().unwrap().free_camera_copies;
        let timing = captured.as_ref();
        let event = TetherCaptured {
            session: self
                .root
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            camera_name,
            destination: result.destination,
            asset_id: asset,
            skipped: result.skipped,
            error: result.error,
            requested: timing.is_some_and(|(c, _)| c.requested),
            connection: timing.map(|(c, _)| c.connection),
            exposure: timing.map(|(c, _)| c.exposure),
            ambiguous: timing.map(|(c, _)| c.ambiguous.clone()).unwrap_or_default(),
            request: timing.and_then(|(c, _)| c.request),
            ms_to_event: timing.and_then(|(c, _)| c.ms_to_event),
            ms_to_downloaded: timing.map(|(c, _)| c.ms_to_downloaded),
            ms_to_ingested: timing
                .map(|(c, received)| c.ms_to_downloaded + received.elapsed().as_millis() as u64),
        };
        log::info!(
            "Tether: {} {} ({}, exposure {:?}/{:?}): camera report {:?} ms, downloaded {:?} ms, in library {:?} ms",
            event.camera_name,
            if committed {
                if event.skipped {
                    "already in the library"
                } else {
                    "in the library"
                }
            } else {
                "not added"
            },
            event
                .request
                .map_or("not requested".to_string(), |request| format!(
                    "request {request}"
                )),
            event.connection,
            event.exposure,
            event.ms_to_event,
            event.ms_to_downloaded,
            event.ms_to_ingested
        );
        (self.notify)("tether-captured", json!(event));
        // Only now that the photo is in the library (or a verified duplicate
        // of one there) may the camera let go of it.
        if committed
            && free
            && let Some((captured, _)) = &captured
        {
            let released = (self.release)(captured);
            // For the camera test: the release came after the commit.
            (self.notify)(
                "tether-released",
                json!(TetherReleased {
                    camera_name: captured.camera_name.clone(),
                    connection: captured.connection,
                    exposure: captured.exposure,
                    freed: released.as_ref().is_ok_and(|freed| *freed),
                    error: released.err(),
                }),
            );
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
                let choice = CHOICE.load(Ordering::SeqCst);
                std::thread::spawn(move || reconnect_when_back(app, choice));
            },
        )?;
        let library = HELPER
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.library.clone());
        Ok(TetherStatus {
            session,
            camera: status,
            connected: true,
            transport: transport(library.as_deref()),
            library,
        })
    })
    .await
}

/// Asks the helper to take no more photos from the camera but keep it open
/// (`Request::Hold`), before the connection's ingest ends: every photo it
/// downloaded is then delivered to that ingest, and the ingest's releases
/// still reach the camera. Err only when the helper does not answer (no
/// camera open: nothing to hold).
fn hold(client: &Client) -> Result<(), rawtonic_tether::protocol::TetherError> {
    match client.request(Request::Hold, QUICK) {
        Err(e) if e.kind == ErrorKind::Unresponsive => Err(e),
        _ => Ok(()),
    }
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

/// What a connect opens: the camera on `port` (the first one when `None`)
/// for `session`, its photos ingested with `config`.
struct Opening {
    session: PathBuf,
    port: Option<String>,
    config: Arc<Mutex<CaptureConfig>>,
}

/// Opens a camera for `session` on the running helper: the earlier
/// connection's ingest finished, the transfer folder recovered while no
/// camera is open, the new ingest listening before the camera is (a body
/// shot right after the connect goes straight in).
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
    let attempt = connect_attempt();
    // The earlier camera delivers what it downloaded to its own library and
    // takes no more; its photos are freed on it before it is closed.
    let held = hold(&client);
    finish_ingest(state);
    // The transfer folder is read only once the helper has confirmed that
    // no camera is open (a download in progress would still write there);
    // a helper that does not confirm is ended first.
    if let Err(e) = held.and_then(|()| client.request(Request::Disconnect, QUICK).map(drop)) {
        client.kill();
        let started = Instant::now();
        while client.alive() && started.elapsed() < QUICK {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Err(format!(
            "{} The tether helper was restarted; connect the camera again.",
            e.message
        ));
    }
    // A transfer folder that cannot be used (e.g. a symbolic link) stops the
    // connect: the camera's photos would have nowhere safe to go.
    let recovered = staging::recover(&staging::root(&session))
        .map_err(|e| format!("The tether transfer folder cannot be used: {e}"))?;
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
        *helper.ingest.lock().unwrap() = Some(Inlet {
            tx,
            config: config.clone(),
        });
        helper.worker = Some(worker);
        helper.connecting = Some(attempt);
        helper.lost_while_connecting = false;
    }
    let connect = Request::Connect {
        port,
        staging: staging::root(&session),
        serial: None,
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
                end_attempt(h, attempt);
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
    let lost_meanwhile = end_attempt(helper, attempt);
    let connection = Connection::new(session, status.clone(), config);
    if lost_meanwhile {
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
        // Its last photos go in and are freed on the camera before it closes.
        if running {
            let _ = request(&app, Request::Hold, QUICK);
        }
        finish_ingest(&HELPER);
        if running {
            let _ = request(&app, Request::Disconnect, QUICK);
        }
        // Said like any other end of a connection (the shortcut follows it
        // also when no panel is mounted).
        let _ = app.emit("tether-state", json!({ "connected": false }));
        Ok(())
    })
    .await
}

/// The connected camera, if any (for a window opened later).
#[tauri::command]
pub fn tether_status() -> Option<TetherStatus> {
    status_of(&HELPER)
}

fn status_of(state: &Mutex<Option<Helper>>) -> Option<TetherStatus> {
    let helper = state.lock().unwrap();
    // A helper that ended has no camera.
    let h = helper.as_ref().filter(|h| h.client.alive())?;
    let (connection, connected) = match (&h.connection, &h.lost) {
        (Some(c), _) => (c, true),
        (None, Some(c)) => (c, false),
        (None, None) => return None,
    };
    Some(TetherStatus {
        session: connection.session.clone(),
        camera: connection.status.clone(),
        connected,
        library: h.library.clone(),
        transport: transport(h.library.as_deref()),
    })
}

#[tauri::command]
pub async fn tether_settings(app: AppHandle) -> Result<Vec<Setting>, String> {
    blocking(
        move || match camera_request(&app, Request::Settings, SLOW)? {
            Reply::Settings { settings } => Ok(settings),
            _ => Err("Unexpected answer from the tether helper.".into()),
        },
    )
    .await
}

/// Sets a camera setting; the value the camera reports afterwards (it may
/// keep its value or choose another), if it could be read.
#[tauri::command]
pub async fn tether_set_setting(
    key: String,
    value: String,
    app: AppHandle,
) -> Result<Option<String>, String> {
    blocking(
        move || match camera_request(&app, Request::SetSetting { key, value }, QUICK)? {
            Reply::SettingSet { value } => Ok(value),
            _ => Err("Unexpected answer from the tether helper.".into()),
        },
    )
    .await
}

/// Fires the shutter; the photo arrives as `tether-captured`.
#[tauri::command]
pub async fn tether_capture(app: AppHandle) -> Result<Option<u64>, String> {
    blocking(move || {
        camera_request(&app, Request::Capture, CAPTURE).map(|reply| match reply {
            // The request's number, named again by its photo or failure.
            Reply::Fired { request } => Some(request),
            _ => None,
        })
    })
    .await
}

#[tauri::command]
pub async fn tether_autofocus(app: AppHandle) -> Result<(), String> {
    blocking(move || camera_request(&app, Request::Autofocus, QUICK).map(|_| ())).await
}

#[tauri::command]
pub async fn tether_live_view(on: bool, app: AppHandle) -> Result<(), String> {
    blocking(move || {
        if !on {
            FRAME.lock().unwrap().take();
        }
        camera_request(&app, Request::LiveView { on }, QUICK).map(|_| ())
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

    #[test]
    fn photos_are_freed_on_the_camera_after_their_commit_unless_turned_off() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let (tx, rx) = mpsc::channel::<Arrival>();
        let (freed_tx, freed) = mpsc::channel::<String>();
        let freed_tx = Mutex::new(freed_tx);
        let release: Release = Arc::new(move |captured: &Captured| {
            let _ = freed_tx.lock().unwrap().send(captured.camera_name.clone());
            Ok(true)
        });
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "off"),
            free_camera_copies: false,
        }));
        let (collect, seen) = collector();
        // Turned on the moment the interface hears of the first photo: that
        // photo was committed with it off.
        let switch = config.clone();
        let notify: Notify = Arc::new(move |name: &str, value| {
            if name == "tether-captured" {
                switch.lock().unwrap().free_camera_copies = true;
            }
            collect(name, value);
        });
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
                download: n,
                exposure: n,
                ambiguous: Vec::new(),
                request: Some(n),
                camera_folder: "/store_10000001".into(),
                camera_name: format!("DSCF{n:04}.RAF"),
                size: 7,
                requested: true,
                ms_to_event: Some(1),
                ms_to_downloaded: 1,
            };
            tx.send(Arrival::now(captured, &config)).unwrap();
        };
        send(1);
        captured(&seen, 1);
        send(2);
        captured(&seen, 1);
        // Freed right after the interface heard of the photo; the first one
        // never (a release of it would come before this one).
        let first = freed
            .recv_timeout(Duration::from_secs(30))
            .expect("freed after its commit");
        assert_eq!(first, "DSCF0002.RAF", "turned off for the first photo");
        // The interface hears of the release after the photo's commit.
        let (name, value) = seen.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(name, "tether-released");
        let released: TetherReleased = serde_json::from_value(value).unwrap();
        assert_eq!(released.camera_name, "DSCF0002.RAF");
        assert_eq!((released.connection, released.exposure), (1, 2));
        assert!(released.freed && released.error.is_none());
        drop(tx);
        assert!(freed.recv_timeout(Duration::from_secs(30)).is_err());
    }

    #[test]
    fn photos_waiting_for_their_ingest_keep_the_settings_they_arrived_with() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(1, "before"),
            free_camera_copies: false,
        }));
        let (tx, rx) = mpsc::channel::<Arrival>();
        let staging = staging::root(&session);
        let arrive = |n: u64| {
            let path = staging.join(format!("incoming/{n:08}/DSC{n:05}.ARW"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("photo {n}")).unwrap();
            let captured = Captured {
                path,
                connection: 1,
                download: n,
                exposure: n,
                ambiguous: Vec::new(),
                request: Some(n),
                camera_folder: "/store_00010001".into(),
                camera_name: format!("DSC{n:05}.ARW"),
                size: 7,
                requested: true,
                ms_to_event: Some(1),
                ms_to_downloaded: 1,
            };
            tx.send(Arrival::now(captured, &config)).unwrap();
        };
        // A backlog: two photos arrive before the ingest takes any in, then
        // the photographer changes the rating for the next ones.
        arrive(1);
        arrive(2);
        config.lock().unwrap().settings = settings(5, "after");
        arrive(3);
        drop(tx);
        let (notify, seen) = collector();
        let worker = spawn_ingest(
            notify,
            Arc::new(|_: &Captured| Ok(false)),
            Default::default(),
            session.clone(),
            config,
            rx,
        )
        .unwrap();
        worker.join().unwrap();
        let seen = captured(&seen, 3);
        assert!(seen.iter().all(|c| c.error.is_none()), "{seen:?}");
        let lib = Library::open(&session).unwrap();
        let mut ratings: Vec<_> = lib
            .assets(None)
            .unwrap()
            .into_iter()
            .map(|a| (a.rel_path, a.rating))
            .collect();
        ratings.sort();
        assert_eq!(
            ratings,
            [
                ("Capture/DSC00001.ARW".to_string(), 1),
                ("Capture/DSC00002.ARW".to_string(), 1),
                ("Capture/DSC00003.ARW".to_string(), 5),
            ]
        );
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
            serial: None,
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
                QUICK,
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
                QUICK,
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
            QUICK,
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
                    if let Some(inlet) = route.lock().unwrap().as_ref() {
                        inlet.send(captured);
                    }
                }
                Incoming::Event(Event::Disconnected { .. }) if camera_lost(&events, None) => {
                    on_lost()
                }
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
            connecting: None,
            lost_while_connecting: false,
            generation: 7,
        });
        (state, client)
    }

    #[test]
    fn the_reply_and_the_event_of_one_loss_start_one_reconnect() {
        let (state, _client) = mock_helper(&MockSpec::default());
        state.lock().unwrap().as_mut().unwrap().connection = Some(Connection {
            id: 0,
            session: PathBuf::from("/job"),
            status: CameraStatus::default(),
            config: Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "x"),
                free_camera_copies: true,
            })),
        });
        // Whichever of the failed request and the event comes first.
        assert!(camera_lost(&state, None));
        assert!(!camera_lost(&state, None));
        assert!(state.lock().unwrap().as_ref().unwrap().lost.is_some());
    }

    #[test]
    fn a_late_failure_of_an_earlier_connection_leaves_the_newer_one_alone() {
        let (state, client) = mock_helper(&MockSpec::default());
        let config = || {
            Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "x"),
                free_camera_copies: true,
            }))
        };
        let earlier = Connection::new("/a".into(), CameraStatus::default(), config()).id;
        let newer = Connection::new("/b".into(), CameraStatus::default(), config());
        state.lock().unwrap().as_mut().unwrap().connection = Some(newer);
        assert!(!camera_lost(&state, Some(earlier)));
        let status = status_of(&state).expect("still connected");
        assert!(status.connected && status.session == Path::new("/b"));
        // Its own failure, or the helper's event: away, waiting.
        assert!(camera_lost(&state, None));
        let waiting = status_of(&state).expect("its library is still known");
        assert!(!waiting.connected && waiting.session == Path::new("/b"));
        // A helper that ended has no camera.
        client.kill();
        let started = Instant::now();
        while client.alive() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(status_of(&state).is_none());
    }

    #[test]
    fn a_replaced_helpers_late_messages_concern_no_camera_of_now() {
        let (state, _client) = mock_helper(&MockSpec::default());
        // The running helper is generation 7 (mock_helper).
        assert!(current_helper(&state, 7));
        assert!(!current_helper(&state, 6), "the one it replaced");
        assert!(!current_helper(&Mutex::new(None), 7));
    }

    #[test]
    fn a_cancelled_reconnect_leaves_a_newer_connects_mark_alone() {
        let (state, _client) = mock_helper(&MockSpec {
            serial: Some("ELSEWHERE".into()),
            ..MockSpec::default()
        });
        state.lock().unwrap().as_mut().unwrap().lost = Some(Connection {
            id: 0,
            session: PathBuf::from("/job"),
            status: CameraStatus {
                model: "Mock Camera".into(),
                serial: Some("MOCK0001".into()),
                ..CameraStatus::default()
            },
            config: Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "x"),
                free_camera_copies: true,
            })),
        });
        let checks = std::sync::atomic::AtomicUsize::new(0);
        reconnect_lost(
            &state,
            &Mutex::new(()),
            Duration::ZERO,
            || {
                // The user connects while this reconnect is looking.
                if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                    state.lock().unwrap().as_mut().unwrap().connecting = Some(u64::MAX);
                }
                checks.load(Ordering::SeqCst) < 3
            },
            |_| panic!("connected"),
        );
        assert_eq!(
            state.lock().unwrap().as_ref().unwrap().connecting,
            Some(u64::MAX),
            "the newer connect's mark stays"
        );
    }

    #[test]
    fn a_reconnect_started_after_a_newer_connect_leaves_that_connection_alone() {
        let (state, _client) = mock_helper(&MockSpec::default());
        // The user's connect finished first; the old camera's loss started
        // this reconnect with the new choice count (wanted stays true).
        let newer = Connection::new(
            "/b".into(),
            CameraStatus::default(),
            Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "x"),
                free_camera_copies: true,
            })),
        );
        let id = newer.id;
        state.lock().unwrap().as_mut().unwrap().connection = Some(newer);
        reconnect_lost(
            &state,
            &Mutex::new(()),
            Duration::ZERO,
            || true,
            |_| panic!("connected"),
        );
        let helper = state.lock().unwrap();
        let helper = helper.as_ref().unwrap();
        assert_eq!(helper.connection.as_ref().map(|c| c.id), Some(id));
        assert!(helper.lost.is_none() && helper.connecting.is_none());
    }

    #[test]
    fn a_late_reconnect_never_touches_a_newer_connection() {
        let (state, _client) = mock_helper(&MockSpec::default());
        state.lock().unwrap().as_mut().unwrap().connection = Some(Connection {
            id: 0,
            session: PathBuf::from("/job"),
            status: CameraStatus::default(),
            config: Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "new"),
                free_camera_copies: true,
            })),
        });
        // Started for a loss before the user's newer connect.
        reconnect_lost(
            &state,
            &Mutex::new(()),
            Duration::ZERO,
            || false,
            |_| panic!("connected"),
        );
        let helper = state.lock().unwrap();
        let helper = helper.as_ref().unwrap();
        assert!(helper.connection.is_some() && helper.lost.is_none());
    }

    #[test]
    fn a_reconnect_ends_a_helper_that_does_not_answer_instead_of_asking_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let spec = MockSpec {
            list_delay_ms: 30_000,
            ..MockSpec::default()
        };
        let (_state, client) = mock_helper(&spec);
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let transition = Mutex::new(());
        let found = reconnect(
            &client,
            "Mock Camera",
            Some("MOCK0001"),
            &session,
            Duration::ZERO,
            Duration::from_millis(500),
            &transition,
            || looks.fetch_add(1, Ordering::SeqCst) < 100,
        );
        assert!(found.is_none());
        assert_eq!(looks.load(Ordering::SeqCst), 1, "asked once, not again");
        let started = Instant::now();
        while client.alive() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!client.alive(), "the hung helper was ended");
    }

    #[test]
    fn a_reconnect_takes_only_the_same_body() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        // Another body of the same model (serial MOCK0001) is attached, and
        // a shot is fired on it the moment it is opened.
        let spec = MockSpec {
            body_shots_ms: vec![0],
            ..MockSpec::default()
        };
        let (_state, client) = mock_helper(&spec);
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let transition = Mutex::new(());
        let found = reconnect(
            &client,
            "Mock Camera",
            Some("MOCK0002"),
            &session,
            Duration::ZERO,
            QUICK,
            &transition,
            || looks.fetch_add(1, Ordering::SeqCst) < 6,
        );
        assert!(found.is_none());
        assert_eq!(
            client.request(Request::Settings, QUICK).unwrap_err().kind,
            ErrorKind::NotConnected,
            "the other body was let go"
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(incoming_left(&session), 0, "nothing of it downloaded");
    }

    #[test]
    fn a_body_lost_again_right_after_its_reconnect_is_never_left_connected() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        // Every time it is opened: a shot, then the cable comes out again.
        let spec = MockSpec {
            body_shots_ms: vec![0],
            unplug_after: Some(1),
            ..MockSpec::default()
        };
        let restarts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = restarts.clone();
        let (state, client) = mock_helper_on_lost(&spec, move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        state.lock().unwrap().as_mut().unwrap().lost = Some(Connection {
            id: 0,
            session: session.clone(),
            status: CameraStatus {
                model: spec.model.clone(),
                serial: spec.serial.clone(),
                ..CameraStatus::default()
            },
            config: Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "again"),
                free_camera_copies: false,
            })),
        });
        let installs = std::sync::atomic::AtomicUsize::new(0);
        let looks = std::sync::atomic::AtomicUsize::new(0);
        reconnect_lost(
            &state,
            &Mutex::new(()),
            Duration::from_millis(20),
            || looks.fetch_add(1, Ordering::SeqCst) < 40,
            |_| {
                installs.fetch_add(1, Ordering::SeqCst);
            },
        );
        drop(client);
        std::thread::sleep(Duration::from_millis(300));
        let helper = state.lock().unwrap();
        let helper = helper.as_ref().unwrap();
        // Never left connected to a body that is gone: either it stayed lost
        // (and was looked for again) or its loss started a reconnect.
        if helper.connection.is_some() {
            assert_eq!(restarts.load(Ordering::SeqCst), 0);
        } else {
            assert!(helper.lost.is_some());
            assert_eq!(
                installs.load(Ordering::SeqCst),
                restarts.load(Ordering::SeqCst),
                "every install whose body went away started a reconnect"
            );
        }
        assert!(helper.connecting.is_none());
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
        assert!(helper.connecting.is_none());
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
    fn a_library_switch_during_a_burst_takes_every_delivered_photo_in_and_frees_it() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a")).unwrap();
        fs::create_dir_all(tmp.path().join("b")).unwrap();
        let (a, b) = (
            session(&tmp.path().join("a")),
            session(&tmp.path().join("b")),
        );
        // A burst fired on the body, one shot every 3 ms for 0.6 s.
        let spec = MockSpec {
            file_size: 4 * 1024,
            event_delay_ms: 0,
            body_shots_ms: (0..600).step_by(3).collect(),
            queue_limit: Some(1000),
            ..MockSpec::default()
        };
        let (state, client) = mock_helper(&spec);
        let config = || {
            Arc::new(Mutex::new(CaptureConfig {
                template: "{orig}".into(),
                settings: settings(0, "switch"),
                free_camera_copies: true,
            }))
        };
        let (notify, seen) = collector();
        let transition = Mutex::new(());
        let open = |session: &Path| {
            open_session(
                &state,
                &transition,
                client.clone(),
                notify.clone(),
                Opening {
                    session: session.to_path_buf(),
                    port: None,
                    config: config(),
                },
                || {},
            )
            .unwrap()
        };
        open(&a);
        // Some of the burst is in, some is still coming.
        let first = captured(&seen, 20);
        assert!(first.iter().all(|c| c.error.is_none()), "{first:?}");
        open(&b);
        // Disconnected as `tether_disconnect` does.
        hold(&client).unwrap();
        finish_ingest(&state);
        // Nothing of A's camera connection is left behind in A's transfer
        // folder for a later connect: what was delivered went in.
        assert_eq!(incoming_left(&a), 0, "photos stranded in the old library");
        let in_a = photos_in_capture(&a);
        assert!(in_a >= 20, "{in_a}");
        assert_eq!(incoming_left(&b), 0);
    }

    #[test]
    fn the_transfer_folder_is_not_touched_without_a_confirmed_disconnect() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let root = staging::root(&session);
        // Being written by a helper that does not answer.
        let writing = staging::Partial::create(&root, 1, "DSC00001.ARW").unwrap();
        let (state, client) = mock_helper(&MockSpec::default());
        client.kill();
        // A kill takes effect a little later: until then the helper can
        // still confirm the disconnect (and the folder may be read).
        let started = Instant::now();
        while client.alive() {
            assert!(started.elapsed() < Duration::from_secs(30), "not ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "x"),
            free_camera_copies: false,
        }));
        let (notify, _seen) = collector();
        let opened = open_session(
            &state,
            &Mutex::new(()),
            client,
            notify,
            Opening {
                session: session.clone(),
                port: None,
                config,
            },
            || {},
        );
        assert!(opened.is_err());
        assert!(
            !root.join(staging::INCOMPLETE).exists(),
            "the partial download was not moved aside"
        );
        drop(writing);
    }

    #[test]
    fn a_transfer_folder_that_cannot_be_used_stops_the_connect() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(tmp.path());
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, staging::root(&session)).unwrap();
        let (state, client) = mock_helper(&MockSpec::default());
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "x"),
            free_camera_copies: false,
        }));
        let (notify, _seen) = collector();
        let opened = open_session(
            &state,
            &Mutex::new(()),
            client.clone(),
            notify,
            Opening {
                session,
                port: None,
                config,
            },
            || {},
        );
        assert!(opened.is_err());
        assert_eq!(
            client.request(Request::Settings, QUICK).unwrap_err().kind,
            ErrorKind::NotConnected,
            "no camera opened"
        );
    }

    #[test]
    fn no_camera_is_opened_for_a_library_its_photos_cannot_go_into() {
        let tmp = tempfile::tempdir().unwrap();
        // Not a library.
        let folder = tmp.path().join("not-a-library");
        fs::create_dir(&folder).unwrap();
        let (state, client) = mock_helper(&MockSpec::default());
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(0, "x"),
            free_camera_copies: false,
        }));
        let (notify, _seen) = collector();
        let opened = open_session(
            &state,
            &Mutex::new(()),
            client.clone(),
            notify,
            Opening {
                session: folder,
                port: None,
                config,
            },
            || {},
        );
        assert!(opened.is_err(), "{opened:?}");
        assert_eq!(
            client.request(Request::Settings, QUICK).unwrap_err().kind,
            ErrorKind::NotConnected
        );
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
                        serial: None,
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
                    download: 1,
                    exposure: 1,
                    ambiguous: Vec::new(),
                    request: Some(1),
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
                    serial: None,
                },
                SLOW,
            )
            .unwrap();
        client.request(Request::Capture, CAPTURE).unwrap();
        let (release, captured) = first;
        // Through the helper that delivered it (or none once it ended).
        assert!(!matches!(release(&captured), Ok(true)));
        assert!(
            client.request(Request::Capture, CAPTURE).is_err(),
            "the new camera's photo was not freed"
        );
    }

    /// The helper binary cargo built for the workspace (`--workspace`
    /// builds it for the tether crate's own tests).
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
        let (photos_tx, photos_rx) = mpsc::channel::<Arrival>();
        let photos_tx = Mutex::new(photos_tx);
        let config = Arc::new(Mutex::new(CaptureConfig {
            template: "{orig}".into(),
            settings: settings(3, "tether"),
            free_camera_copies: true,
        }));
        let arriving = config.clone();
        let (client, _) = Client::spawn(
            &helper_binary(),
            &["--mock".into()],
            &[(
                "RAWTONIC_TETHER_MOCK".into(),
                serde_json::to_string(&spec).unwrap().into(),
            )],
            move |incoming| {
                if let Incoming::Event(Event::Captured { captured }) = incoming {
                    let _ = photos_tx
                        .lock()
                        .unwrap()
                        .send(Arrival::now(captured, &arriving));
                }
            },
        )
        .unwrap();
        client
            .request(
                Request::Connect {
                    port: None,
                    staging: staging::root(&session),
                    serial: None,
                },
                SLOW,
            )
            .unwrap();
        let (notify, seen) = collector();
        let client = Arc::new(client);
        // Weak: the ingest thread must not keep the helper alive.
        let releasing = Arc::downgrade(&client);
        let release: Release = Arc::new(move |captured: &Captured| {
            let release = Request::Release {
                connection: captured.connection,
                download: captured.download,
                camera_folder: captured.camera_folder.clone(),
                camera_name: captured.camera_name.clone(),
            };
            let client = releasing.upgrade().ok_or("helper ended")?;
            let reply = client.request(release, QUICK).unwrap();
            Ok(reply == Reply::Released { freed: true })
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
    fn capture_record_sequence_does_not_reuse_numbers_after_trash() {
        for system in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let directory = session(tmp.path());
            let lib = Library::open(&directory).unwrap();
            let first = directory.join("Capture/1.ARW");
            for (path, bytes) in [
                (&first, "first"),
                (&directory.join("Capture/2.ARW"), "second"),
            ] {
                fs::write(path, bytes).unwrap();
                lib.add_asset(path).unwrap();
            }
            let held = if system {
                tmp.path().join("held.ARW")
            } else {
                directory.join("Trash/1.ARW")
            };
            fs::rename(&first, &held).unwrap();
            if system {
                lib.set_trashed_at(&first, true).unwrap();
            } else {
                lib.relocate(&first, &held).unwrap();
            }
            let root = staging::root(&directory);
            let mut incoming = staging::Partial::create(&root, 1, "new.ARW").unwrap();
            let bytes = b"third distinct photo";
            incoming.write(bytes).unwrap();
            let staged = incoming.publish(bytes.len() as u64).unwrap();
            let config = Mutex::new(CaptureConfig {
                template: "{seq}.{ext}".into(),
                settings: settings(0, "kept"),
                free_camera_copies: false,
            });
            let notify = |_: &str, _: serde_json::Value| {};
            let release = |_: &Captured| -> Result<bool, String> { Ok(false) };
            let ingest = Ingest {
                notify: &notify,
                release: &release,
                lib: &lib,
                root: &root,
                config: &config,
                previous: None,
            };
            assert_eq!(ingest.record(&staged, None).unwrap().sequence, 3);
        }
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
            Arc::new(|_: &Captured| Ok(false)),
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
            Arc::new(|_: &Captured| Ok(false)),
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
