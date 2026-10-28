//! The helper's loop: one thread owns the camera. Between requests it
//! waits briefly for camera events, so photos fired on the body are
//! downloaded like the ones the app asks for, and sends live-view frames
//! while they are wanted.
use crate::camera::{AbortPoint, Camera, CameraEvent, CameraFile, Driver, Fired};
use crate::experiment::LiveViewExperiment;
use crate::frame;
use crate::protocol::{
    Captured, Envelope, ErrorKind, Event, Message, PROTOCOL, Reply, Request, TetherError,
};
use crate::staging::Partial;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one wait for camera events lasts while nothing else is due.
const EVENT_POLL: Duration = Duration::from_millis(20);
/// How long a set setting is read back for (`Server::set_setting`), and
/// how often.
const READBACK: Duration = Duration::from_secs(3);
const READBACK_EVERY: Duration = Duration::from_millis(250);
/// How long a shot waits for the last one's photo on a camera that takes
/// one at a time (`Server::wait_for_the_last_shot`).
pub const ONE_AT_A_TIME_WAIT: Duration = Duration::from_secs(10);
/// An exposure of a file name (`Open::exposures_by_stem`).
struct Exposure {
    /// The file kinds (extensions) it gave so far.
    kinds: Vec<String>,
    /// Its app request, if any.
    request: Option<Asked>,
    /// `Captured::exposure`.
    number: u64,
    /// It was in the camera's transfer queue at the connect: its files pair
    /// only with each other, never with a new shot's under the same name.
    queued: bool,
}

/// An app shot request: when it fired, and its number (`Reply::Fired`).
#[derive(Debug, Clone, Copy)]
struct Asked {
    at: Instant,
    id: u64,
}

/// Which app shot a downloaded file belongs to.
enum Shot {
    /// The oldest waiting one, if any — unless the file turns out to be a
    /// repeated report of the last photo.
    Waiting,
    /// The blocking capture fired at this time returned it.
    At(Asked),
    /// None: it was in the camera's transfer queue when the camera was
    /// connected (`Open::queue`).
    Queued,
}

fn content_hash(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}
const MEMORY_ONLY: &str = "The camera keeps photos only in its memory now (no card, or Save to \
     without the card), where a photo could be lost on its way: Rawtonic no longer fires it or \
     takes photos from it. Put a card in, set Save to with the card, and connect again.";

const EXPERIMENTAL_OFF: &str = "Canon cameras have not been tested with Rawtonic yet, so they are \
     not connected by default. To help test one, turn on Preferences > Tethering > Experimental: \
     Canon cameras, keep a memory card in the camera and connect again.";

const EXPERIMENTAL_SAVE_TO: &str = "Rawtonic could not set the camera to save each photo on its \
     memory card, so it is not connected: with this camera a photo's report can get lost on its \
     way, and the card copy is what keeps it. Set Save to (or Capture target) to the memory card \
     on the camera and connect again.";

const EXPERIMENTAL_NO_CARD: &str = "Rawtonic could not confirm that the camera saves each photo \
     on a memory card (no card in, or it still saves to its own memory), so it is not connected: \
     with this camera a photo's report can get lost on its way, and the card copy is what keeps \
     it. Put a memory card in, set Save to on the camera to the card, and connect again.";

/// A shot whose file has not come after this long is reported as failed
/// (it is never fired again; a file that still comes is downloaded).
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(60);

struct Open {
    camera: Box<dyn Camera>,
    staging: PathBuf,
    live_view: bool,
    /// Live-view frames sent on this connection (for the log).
    frames: u64,
    /// Failed live-view frames in a row (`LiveViewExperiment::tolerate`).
    frame_failures: u32,
    /// When the camera's events were last read during live view
    /// (`LiveViewExperiment::poll_every`).
    events_read_at: Option<Instant>,
    /// App shots still waiting for their file, oldest first.
    waiting: VecDeque<Asked>,
    /// Files downloaded from this camera.
    downloaded: u64,
    /// Each camera file's last published download: length and content hash.
    published: std::collections::HashMap<CameraFile, (usize, u64)>,
    /// The download each camera file stands for now.
    downloads: std::collections::HashMap<CameraFile, u64>,
    /// The exposures of each file name (without the extension) in this
    /// connection, oldest first (a few bytes each): RAW and JPEG of one exposure share the
    /// name and answer one request, and may come interleaved with other
    /// exposures' files, under other names or the same one (A.RAW, B.RAW,
    /// A.JPG; A.RAW, A.RAW, A.JPG, A.JPG with reused names).
    exposures_by_stem: std::collections::HashMap<String, VecDeque<Exposure>>,
    /// Exposures so far (`Captured::exposure`).
    exposures: u64,
    /// The camera came to keep photos only in its memory ("Save to" without
    /// the card, or no card): not polled or fired for the rest of this
    /// connection (photos it took meanwhile have no card copy).
    memory_only: bool,
    /// On a driver path no body was tested on (`CameraStatus::experimental`):
    /// nothing is freed on the camera, and a shot without its photo is
    /// reported as kept on the card.
    experimental: bool,
    /// This connection's number: a release names the connection its photo
    /// came from, so a late one never reaches another camera's photo.
    connection: u64,
    /// `Request::Hold`: the camera's events are not read (nothing more is
    /// downloaded) until the connection ends.
    held: bool,
    /// On a camera that takes one shot at a time: a shot was fired, or a
    /// photo came (also one shot on the body), and the camera has not said
    /// its capture is complete — libgphoto2's Sony path reports that after a
    /// shot's last file, so a RAW+JPEG's JPEG may still be on its way. A
    /// shot reported as not delivered ends it too.
    transferring: bool,
    /// The photos that were in the camera's USB transfer queue when it was
    /// connected and are still to be downloaded, one per turn of the loop
    /// (requests and new photos are served in between). They were taken
    /// while no computer took them, and a camera may keep no other copy:
    /// they go in like any photo and are freed only after their commit
    /// (`Request::Release`). One that cannot be downloaded stays on the
    /// camera and is reported.
    queue: VecDeque<CameraFile>,
    /// The photos the transfer queue held at the connect that are not safely
    /// downloaded yet: one the camera reports as new (also after a failed
    /// download) is fetched as a queued photo, never taken for an app
    /// shot's. Once one is in the transfer folder, a later file of its name
    /// is a repeated report (same bytes, `published`) or a new photo.
    from_queue: std::collections::HashSet<CameraFile>,
    /// Every photo the transfer queue held at the connect. A file of one of
    /// these names that cannot be downloaded may be that photo reported
    /// again or a new one: it takes no app shot's request.
    listed: std::collections::HashSet<CameraFile>,
    /// Why the transfer queue could not be listed, told once after the
    /// connect (its photos stay on the camera).
    queue_unlisted: Option<TetherError>,
    /// The transfer queue could not be listed: a file the camera reports
    /// could be an old photo of it, so no shot is fired on this connection
    /// (its photo could not be told from them).
    queue_unknown: bool,
}

const QUEUE_UNKNOWN: &str = "The photos already waiting on the camera could not be listed, so a \
     new shot could not be told from them: shots are not fired on this connection. Connect the \
     camera again.";

pub struct Server<W: Write> {
    driver: Box<dyn Driver>,
    out: W,
    open: Option<Open>,
    shot_timeout: Duration,
    /// `ONE_AT_A_TIME_WAIT` (shorter in tests).
    one_at_a_time_wait: Duration,
    /// Connections opened so far.
    connections: u64,
    /// App shot requests so far (`Asked::id`).
    asked: u64,
    experiment: LiveViewExperiment,
}

/// Reads requests on their own thread, so the loop can wait for the camera
/// meanwhile. A malformed request ends the helper (the app and the helper
/// do not match).
fn reader(mut input: impl Read + Send + 'static) -> mpsc::Receiver<Envelope> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match frame::read(&mut input) {
                Ok(Some(f)) if f.kind == frame::JSON => {
                    match serde_json::from_slice::<Envelope>(&f.payload) {
                        Ok(envelope) => {
                            if tx.send(envelope).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            log::error!("malformed request: {e}");
                            return;
                        }
                    }
                }
                Ok(Some(f)) => {
                    log::error!("unexpected frame kind {}", f.kind);
                    return;
                }
                Ok(None) => return,
                Err(e) => {
                    log::error!("reading requests: {e}");
                    return;
                }
            }
        }
    });
    rx
}

fn not_connected() -> TetherError {
    TetherError::new(ErrorKind::NotConnected, "No camera is connected.")
}

fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

impl<W: Write> Server<W> {
    pub fn new(driver: Box<dyn Driver>, out: W) -> Self {
        Self {
            driver,
            out,
            open: None,
            shot_timeout: SHOT_TIMEOUT,
            one_at_a_time_wait: ONE_AT_A_TIME_WAIT,
            connections: 0,
            asked: 0,
            experiment: LiveViewExperiment::default(),
        }
    }

    /// Live-view experiments for a session with a body (`experiment`).
    pub fn with_live_view_experiment(mut self, experiment: LiveViewExperiment) -> Self {
        self.experiment = experiment;
        self
    }

    pub fn with_shot_timeout(mut self, timeout: Duration) -> Self {
        self.shot_timeout = timeout;
        self
    }

    pub fn with_one_at_a_time_wait(mut self, wait: Duration) -> Self {
        self.one_at_a_time_wait = wait;
        self
    }

    /// Serves requests from `input` until it ends or asks to shut down.
    pub fn run(mut self, input: impl Read + Send + 'static) -> io::Result<()> {
        let requests = reader(input);
        loop {
            let next = if self.open.is_some() {
                match requests.try_recv() {
                    Ok(r) => Some(r),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            } else {
                match requests.recv() {
                    Ok(r) => Some(r),
                    Err(_) => break,
                }
            };
            match next {
                Some(envelope) => {
                    if !self.handle(envelope)? {
                        break;
                    }
                }
                None => self.tick()?,
            }
        }
        self.open = None;
        Ok(())
    }

    fn send(&mut self, message: &Message) -> io::Result<()> {
        frame::write_json(&mut self.out, message)
    }

    fn event(&mut self, event: Event) -> io::Result<()> {
        self.send(&Message::Event { event })
    }

    /// Answers one request; `false` ends the helper.
    fn handle(&mut self, Envelope { id, request }: Envelope) -> io::Result<bool> {
        let shutdown = request == Request::Shutdown;
        let mut fired = None;
        // A shot refused as busy is not tried again here: busy does not
        // prove that nothing was exposed (the camera may refuse a step after
        // the shutter); a photo that still comes is downloaded as it comes.
        // What the camera reported before this shot is taken in first: a
        // driver may drop events it collected when it fires (libgphoto2's
        // Canon EOS trigger clears its event list).
        let mut ready = true;
        if request == Request::Capture {
            for _ in 0..64 {
                if !self.read_events(Duration::ZERO)? {
                    break;
                }
            }
            ready = self.wait_for_the_last_shot()?;
        }
        let result = match request {
            // Not fired: the camera would take no shot now and say nothing.
            Request::Capture if !ready => Err(TetherError::new(
                ErrorKind::Busy,
                "The camera has not delivered the last shot yet, so this one was not fired. Try \
                 again in a moment.",
            )),
            Request::SetSetting { key, value } => self.set_setting(&key, &value)?,
            request => self.answer(request, &mut fired),
        };
        // A camera lost during a request is lost like one lost between
        // requests: the app hears it (and connects it again when it is back).
        let lost = match &result {
            Err(e) if e.kind == ErrorKind::Disconnected && self.open.is_some() => Some(e.clone()),
            _ => None,
        };
        if lost.is_some() {
            self.open = None;
        }
        self.send(&Message::Reply { id, result })?;
        if let Some(error) = lost {
            self.event(Event::Disconnected { error })?;
        }
        if let Some((file, at)) = fired {
            self.download(file, Shot::At(at))?;
        }
        Ok(!shutdown)
    }

    fn camera(&mut self) -> Result<&mut Open, TetherError> {
        self.open.as_mut().ok_or_else(not_connected)
    }

    fn answer(
        &mut self,
        request: Request,
        fired: &mut Option<(CameraFile, Asked)>,
    ) -> Result<Reply, TetherError> {
        match request {
            Request::Hello => Ok(Reply::Hello {
                protocol: PROTOCOL,
                library: self.driver.library(),
            }),
            Request::ListCameras => Ok(Reply::Cameras {
                cameras: self.driver.list()?,
            }),
            Request::Connect {
                port,
                staging,
                serial,
                experimental,
            } => {
                if !staging.is_absolute() {
                    return Err(TetherError::new(
                        ErrorKind::Protocol,
                        "The transfer folder must be an absolute path.",
                    ));
                }
                self.open = None;
                let mut camera = self.driver.open(port.as_deref())?;
                let status = camera.status();
                if serial.is_some() && status.serial != serial {
                    // Closed (dropped) before any event or download.
                    return Err(TetherError::new(
                        ErrorKind::NotFound,
                        "Another body of that model is on this port.",
                    ));
                }
                // A path no body was tested on (libgphoto2's Canon EOS
                // path) only with the user's opt-in, and only with every
                // photo kept on the memory card: its trigger can discard a
                // photo's report, and the card copy is then the only one.
                // notes: reference/tether.md
                if status.experimental {
                    if !experimental {
                        return Err(TetherError::new(ErrorKind::Unsupported, EXPERIMENTAL_OFF));
                    }
                    if !camera.prefer_card() {
                        return Err(TetherError::new(
                            ErrorKind::Unsupported,
                            EXPERIMENTAL_SAVE_TO,
                        ));
                    }
                    if camera.memory_only()? {
                        return Err(TetherError::new(
                            ErrorKind::Unsupported,
                            EXPERIMENTAL_NO_CARD,
                        ));
                    }
                    log::info!(
                        "{} on an experimental driver path: Save to on the card, nothing freed",
                        status.model
                    );
                }
                // A connection never starts with photos kept only in the
                // camera's memory: Save to goes to the card where the camera
                // offers that; else the camera is not opened.
                if camera.memory_only()? && (!camera.prefer_card() || camera.memory_only()?) {
                    return Err(TetherError::new(ErrorKind::Unsupported, MEMORY_ONLY));
                }
                // Listed before the reply; downloaded afterwards, one per
                // turn (`tick`).
                let (queue, queue_unlisted): (VecDeque<CameraFile>, _) = match camera.queued() {
                    Ok(queued) => {
                        if !queued.is_empty() {
                            log::info!(
                                "{} photo(s) of {} MB in the camera's transfer queue; fetching them",
                                queued.len(),
                                queued.iter().filter_map(|(_, size)| *size).sum::<u64>() >> 20
                            );
                        }
                        (queued.into_iter().map(|(file, _)| file).collect(), None)
                    }
                    Err(e) if e.kind == ErrorKind::Disconnected => return Err(e),
                    Err(e) => {
                        log::warn!("transfer queue not listed: {e}");
                        (VecDeque::new(), Some(e))
                    }
                };
                // Save to may have changed since the first look.
                let status = camera.status();
                self.connections += 1;
                log::info!(
                    "connection {}: {} (firmware {})",
                    self.connections,
                    status.model,
                    status.firmware.as_deref().unwrap_or("not reported")
                );
                self.open = Some(Open {
                    camera,
                    staging,
                    live_view: false,
                    frames: 0,
                    frame_failures: 0,
                    events_read_at: None,
                    waiting: VecDeque::new(),
                    downloaded: 0,
                    published: Default::default(),
                    downloads: Default::default(),
                    exposures_by_stem: Default::default(),
                    exposures: 0,
                    memory_only: false,
                    experimental: status.experimental,
                    connection: self.connections,
                    held: false,
                    transferring: false,
                    from_queue: queue.iter().cloned().collect(),
                    listed: queue.iter().cloned().collect(),
                    queue,
                    queue_unknown: queue_unlisted.is_some(),
                    queue_unlisted,
                });
                Ok(Reply::Connected { status })
            }
            Request::Disconnect => {
                if self.open.take().is_some() {
                    log::info!("disconnected");
                }
                Ok(Reply::Ok)
            }
            Request::Hold => {
                log::info!("hold: no more downloads on this connection");
                let open = self.camera()?;
                open.held = true;
                open.live_view = false;
                Ok(Reply::Ok)
            }
            Request::Settings => Ok(Reply::Settings {
                settings: self.camera()?.camera.settings()?,
            }),
            // Answered by `set_setting` (it reads the camera's events while
            // it reads the value back).
            Request::SetSetting { .. } => Err(TetherError::new(
                ErrorKind::Protocol,
                "SetSetting is answered by set_setting.",
            )),
            Request::Capture => {
                let open = self.camera()?;
                if open.held {
                    return Err(TetherError::new(
                        ErrorKind::NotConnected,
                        "The camera is being disconnected.",
                    ));
                }
                if open.memory_only || open.camera.memory_only()? {
                    open.memory_only = true;
                    return Err(TetherError::new(ErrorKind::Unsupported, MEMORY_ONLY));
                }
                if open.queue_unknown {
                    return Err(TetherError::new(ErrorKind::Other, QUEUE_UNKNOWN));
                }
                let at = Instant::now();
                let shot = open.camera.fire().inspect_err(|e| {
                    log::info!("capture refused after {} ms ({:?}: {e})", ms(at), e.kind)
                })?;
                self.asked += 1;
                let asked = Asked { at, id: self.asked };
                match shot {
                    Fired::Pending => {}
                    Fired::File(file) => {
                        log::info!(
                            "request {}: fired, {} came with the shot after {} ms",
                            asked.id,
                            file.name,
                            ms(at)
                        );
                        *fired = Some((file, asked));
                        return Ok(Reply::Fired { request: asked.id });
                    }
                }
                if let Some(open) = self.open.as_mut() {
                    open.waiting.push_back(asked);
                    if open.camera.one_shot_at_a_time() {
                        open.transferring = true;
                    }
                    log::info!(
                        "request {}: fired in {} ms; {} shot(s) waiting for their photo",
                        asked.id,
                        ms(at),
                        open.waiting.len()
                    );
                }
                Ok(Reply::Fired { request: asked.id })
            }
            Request::Autofocus => {
                self.camera()?.camera.autofocus()?;
                Ok(Reply::Ok)
            }
            Request::LiveView { on } => {
                let open = self.camera()?;
                if on && !open.camera.status().can_preview {
                    return Err(TetherError::new(
                        ErrorKind::Unsupported,
                        "This camera offers no live view over USB.",
                    ));
                }
                open.live_view = on;
                Ok(Reply::Ok)
            }
            Request::Release {
                connection,
                download,
                camera_folder,
                camera_name,
            } => {
                let open = self.camera()?;
                let file = CameraFile {
                    folder: camera_folder,
                    name: camera_name,
                };
                // A photo of an earlier connection stays on its camera (the
                // camera now open may be another one with a file of that
                // name), and so does one whose name stands for a newer photo.
                // Nothing is freed on an experimental path: there the card
                // copy is what keeps a photo whose report got lost.
                if open.experimental {
                    log::info!("{}: kept on the camera (experimental path)", file.name);
                    return Ok(Reply::Released { freed: false });
                }
                if open.connection != connection || open.downloads.get(&file) != Some(&download) {
                    log::info!("{} is not the photo to release any more; kept", file.name);
                    return Ok(Reply::Released { freed: false });
                }
                let freed = open.camera.release(&file)?;
                log::info!(
                    "{}: {}",
                    file.name,
                    if freed {
                        "freed from the transfer queue"
                    } else {
                        "no transfer queue to free"
                    }
                );
                open.downloads.remove(&file);
                Ok(Reply::Released { freed })
            }
            Request::Shutdown => {
                self.open = None;
                Ok(Reply::Ok)
            }
        }
    }

    /// On a camera that takes one shot at a time
    /// (`Camera::one_shot_at_a_time`): waits, reading its events, until the
    /// shots before have their photo and the camera said their capture is
    /// complete (`Open::transferring`), for `one_at_a_time_wait` at most.
    /// Whether the next may be fired: not while the camera is still busy
    /// (it would take no shot and say nothing); a shot that never comes is
    /// reported by its timeout, and then the camera can be fired.
    fn wait_for_the_last_shot(&mut self) -> io::Result<bool> {
        let started = Instant::now();
        let mut said = false;
        while let Some(open) = self.open.as_ref()
            && open.camera.one_shot_at_a_time()
            && !open.held
        {
            let last = open.waiting.back().map(|asked| asked.id);
            if last.is_none() && !open.transferring {
                break;
            }
            let what = last.map_or("the last photo's transfer".to_string(), |last| {
                format!("the photo of request {last}")
            });
            if started.elapsed() >= self.one_at_a_time_wait {
                log::warn!(
                    "still waiting for {what} after {} ms; the next shot is not fired",
                    ms(started)
                );
                return Ok(false);
            }
            if !said {
                log::info!("the next shot waits for {what}");
                said = true;
            }
            self.read_events(EVENT_POLL)?;
        }
        Ok(true)
    }

    /// Sets a setting, then reads it back until the camera reports the
    /// value asked for or `READBACK` passed: libgphoto2 answers a property
    /// from its last reading for up to 2 s (ptp2's `cachetime`), so a read
    /// right after the set can still show the old value, and a Sony body
    /// steps to a value over ~0.7 s. Photos keep coming meanwhile.
    fn set_setting(&mut self, key: &str, value: &str) -> io::Result<Result<Reply, TetherError>> {
        let started = Instant::now();
        let open = match self.camera() {
            Ok(open) => open,
            Err(e) => return Ok(Err(e)),
        };
        if key == "capturetarget" && crate::camera::memory_only(Some(value)) {
            return Ok(Err(TetherError::new(
                ErrorKind::Unsupported,
                "Rawtonic keeps a card copy of every tethered photo: Save to stays with the card.",
            )));
        }
        let value_of = |camera: &mut Box<dyn Camera>| {
            camera
                .settings()
                .map(|settings| settings.into_iter().find(|s| s.key == key).map(|s| s.value))
        };
        let before = value_of(&mut open.camera).ok().flatten();
        if let Err(e) = open.camera.set_setting(key, value) {
            log::info!(
                "set {key} {} -> {value}: refused after {} ms ({:?}: {e})",
                before.as_deref().unwrap_or("?"),
                ms(started),
                e.kind
            );
            return Ok(Err(e));
        }
        let set_ms = ms(started);
        // From the set's end: a Sony body's set itself steps for seconds.
        let set = Instant::now();
        loop {
            let Some(open) = self.open.as_mut() else {
                return Ok(Err(not_connected()));
            };
            let now = match value_of(&mut open.camera) {
                Ok(now) => now,
                Err(e) => return Ok(Err(e)),
            };
            if now.as_deref() == Some(value) || set.elapsed() >= READBACK {
                log::info!(
                    "set {key} {} -> {value}: set in {set_ms} ms, camera reports {} after {} ms",
                    before.as_deref().unwrap_or("?"),
                    now.as_deref().unwrap_or("nothing"),
                    ms(started)
                );
                return Ok(Ok(Reply::SettingSet { value: now }));
            }
            self.read_events(READBACK_EVERY)?;
        }
    }

    /// One turn while a camera is open: a live-view frame when wanted, then
    /// the camera's events.
    fn tick(&mut self) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let mut wait = EVENT_POLL;
        let mut read = true;
        let live_view = open.live_view;
        if live_view {
            wait = Duration::from_millis(1);
            // Every frame's timing goes to the debug log (a body session's
            // trace: how long a frame takes, how long the event reads).
            let started = Instant::now();
            match open.camera.preview() {
                Ok(jpeg) => {
                    open.frames += 1;
                    open.frame_failures = 0;
                    log::debug!(
                        "live view frame {}: {} bytes in {} ms",
                        open.frames,
                        jpeg.len(),
                        ms(started)
                    );
                    frame::write(&mut self.out, frame::LIVE_VIEW, &jpeg)?
                }
                Err(error) if error.kind == ErrorKind::Disconnected => {
                    open.live_view = false;
                    return self.lost(error);
                }
                Err(error) if open.frame_failures < self.experiment.tolerate => {
                    open.frame_failures += 1;
                    log::info!(
                        "live view frame failed after {} ms ({} in a row, tolerated): {error}",
                        ms(started),
                        open.frame_failures
                    );
                }
                Err(error) => {
                    open.live_view = false;
                    log::info!(
                        "live view stopped after {} frames: frame failed after {} ms ({:?}: {error})",
                        open.frames,
                        ms(started),
                        error.kind
                    );
                    self.event(Event::LiveViewStopped { error })?;
                }
            }
            if let Some(open) = self.open.as_ref()
                && let Some(every) = self.experiment.poll_every
            {
                read = open.events_read_at.is_none_or(|at| at.elapsed() >= every);
            }
        }
        while let Some(open) = self.open.as_mut()
            && open
                .waiting
                .front()
                .is_some_and(|asked| asked.at.elapsed() > self.shot_timeout)
        {
            let asked = open.waiting.pop_front();
            if open.waiting.is_empty() {
                open.transferring = false;
            }
            let experimental = open.experimental;
            if let Some(asked) = asked {
                log::warn!(
                    "request {}: no photo {} s after the shot; reported as not delivered, not fired again",
                    asked.id,
                    self.shot_timeout.as_secs()
                );
            }
            let message = if experimental {
                // The known risk of this path (libgphoto2's EOS trigger can
                // discard a photo's report): said every time, never silent.
                format!(
                    "Shot {} was not received within {} s. If the camera took it, it is on the \
                     memory card: with this camera (experimental support) a photo's report can get \
                     lost on its way. It was not fired again.",
                    asked.map_or(0, |asked| asked.id),
                    self.shot_timeout.as_secs()
                )
            } else {
                "No photo came within a minute of the shot: the camera may not have fired (with \
                 autofocus, a shot that finds no focus is not taken) or did not hand it over. It \
                 was not fired again."
                    .to_string()
            };
            self.event(Event::CaptureFailed {
                error: TetherError::new(ErrorKind::Timeout, message),
                request: asked.map(|asked| asked.id),
                exposure: None,
                ambiguous: Vec::new(),
            })?;
        }
        if let Some(open) = self.open.as_mut()
            && let Some(error) = open.queue_unlisted.take()
        {
            self.event(Event::CaptureFailed {
                error: TetherError::new(
                    error.kind,
                    format!(
                        "The photos left in the camera's transfer queue could not be listed, so \
                         they were not fetched; they stay on the camera. Connect it again to fetch \
                         them. ({})",
                        error.message
                    ),
                ),
                request: None,
                exposure: None,
                ambiguous: Vec::new(),
            })?;
        }
        // Not while held (nothing more is downloaded) or while the camera
        // keeps photos only in its memory (nothing is read then).
        if let Some(open) = self.open.as_mut()
            && !open.held
            && !open.memory_only
            && let Some(file) = open.queue.pop_front()
        {
            self.download(file, Shot::Queued)?;
        }
        if !read {
            return Ok(());
        }
        let started = Instant::now();
        let handled = self.read_events(wait)?;
        if live_view && let Some(open) = self.open.as_mut() {
            open.events_read_at = Some(Instant::now());
            log::debug!(
                "live view: camera events read in {} ms{}",
                ms(started),
                if handled { " (one handled)" } else { "" }
            );
        }
        Ok(())
    }

    /// Reads the camera's events for up to `wait` (after checking that a
    /// photo it would hand over keeps a card copy). Whether one was handled.
    fn read_events(&mut self, wait: Duration) -> io::Result<bool> {
        let Some(open) = self.open.as_mut() else {
            return Ok(false);
        };
        if open.held {
            std::thread::sleep(wait);
            return Ok(false);
        }
        // Checked before every read of the camera's events: reading them may
        // already take a photo out of the camera's memory.
        if !open.memory_only {
            match open.camera.memory_only() {
                Ok(false) => {}
                Ok(true) => {
                    open.memory_only = true;
                    self.event(Event::CaptureFailed {
                        error: TetherError::new(ErrorKind::Unsupported, MEMORY_ONLY),
                        request: None,
                        exposure: None,
                        ambiguous: Vec::new(),
                    })?;
                    return Ok(false);
                }
                Err(error) if error.kind == ErrorKind::Disconnected => {
                    self.lost(error)?;
                    return Ok(false);
                }
                Err(error) => {
                    // Not read while it cannot be told; tried again next turn.
                    log::warn!("camera memory check: {error}");
                    std::thread::sleep(wait);
                    return Ok(false);
                }
            }
        }
        let Some(open) = self.open.as_mut() else {
            return Ok(false);
        };
        if open.memory_only {
            // Not polled: a transfer it would start could not be undone.
            std::thread::sleep(wait);
            return Ok(false);
        }
        match open.camera.wait_event(wait) {
            Ok(CameraEvent::FileAdded(file)) => {
                // A photo of the transfer queue reported as new (libgphoto2's
                // Fujifilm event wait reports every object it has not listed
                // itself): fetched now, never taken for an app shot's photo.
                if open.from_queue.contains(&file) {
                    open.queue.retain(|queued| *queued != file);
                    return self.download(file, Shot::Queued).map(|()| true);
                }
                if open.camera.one_shot_at_a_time() {
                    open.transferring = true;
                }
                self.download(file, Shot::Waiting).map(|()| true)
            }
            Ok(CameraEvent::CaptureComplete) => {
                if open.waiting.is_empty() {
                    open.transferring = false;
                }
                Ok(true)
            }
            Ok(CameraEvent::SettingsChanged) => self.event(Event::SettingsChanged).map(|()| true),
            Ok(CameraEvent::Timeout) => Ok(false),
            Ok(_) => Ok(true),
            Err(error) if error.kind == ErrorKind::Disconnected => self.lost(error).map(|()| false),
            Err(error) => {
                log::warn!("camera event: {error}");
                Ok(false)
            }
        }
    }

    fn lost(&mut self, error: TetherError) -> io::Result<()> {
        log::warn!("camera lost: {error}");
        self.open = None;
        self.event(Event::Disconnected { error })
    }

    /// Downloads a file the camera reported into the transfer folder and
    /// tells the app. `shot_of`: which app shot it answers, if any (else it
    /// was fired on the body).
    fn download(&mut self, file: CameraFile, shot_of: Shot) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let reported = Instant::now();
        let queued = matches!(shot_of, Shot::Queued);
        if queued {
            log::info!(
                "{} from the camera's transfer queue ({} more to fetch)",
                file.name,
                open.queue.len()
            );
        } else {
            log::info!(
                "camera reported {} ({} shot(s) waiting)",
                file.name,
                open.waiting.len()
            );
        }
        let shot = open.downloaded + 1;
        let downloaded = open.camera.download(&file);
        // The same photo reported twice (libgphoto2's Fujifilm event polling
        // finds it in the object list and the camera's ObjectAdded names it
        // again; RAW and JPEG may be repeated in turn): same file and same
        // bytes as its last download. A new photo under a reused name has
        // other bytes and is kept.
        if let Ok((data, _)) = &downloaded
            && open
                .published
                .get(&file)
                .is_some_and(|(len, hash)| *len == data.len() && *hash == content_hash(data))
        {
            log::info!("{} reported again; already downloaded", file.name);
            return Ok(());
        }
        // A file under the name of a photo the queue held at the connect
        // that cannot be downloaded: it may be that photo reported again or
        // a new one, so it answers no request (the request stays waiting).
        if !queued
            && open.listed.contains(&file)
            && let Err(error) = &downloaded
        {
            if error.kind == ErrorKind::Disconnected {
                return self.lost(error.clone());
            }
            log::warn!(
                "{} not downloaded after {} ms ({:?}: {}); its name was in the transfer queue, \
                 so it answers no request",
                file.name,
                ms(reported),
                error.kind,
                error.message
            );
            return self.event(Event::CaptureFailed {
                error: TetherError::new(
                    error.kind,
                    format!("{} could not be downloaded: {}", file.name, error.message),
                ),
                request: None,
                exposure: None,
                ambiguous: Vec::new(),
            });
        }
        let (stem, extension) = match file.name.rsplit_once('.') {
            Some((stem, extension)) => (stem.to_string(), extension.to_lowercase()),
            None => (file.name.clone(), String::new()),
        };
        // Another file of an exposure of the name (the newest that has not
        // given this kind yet): the same request, not the next one, however
        // late it comes. Otherwise a new exposure. With a reused name (the
        // camera's numbering wrapped or was reset) this can be ambiguous:
        // libgphoto2 gives no exposure identity, and A.RAW, A.RAW, A.JPG,
        // A.JPG may be two RAW+JPEG exposures or two RAW-only and two
        // JPEG-only ones (likewise A.RAW; A.RAW+A.JPG; A.JPG). Such a file
        // names the exposures it may belong to (`ambiguous`) (the camera test then reports its step as
        // inconclusive). notes: reference/tether.md
        // A queued photo's file and a new shot's file under one name belong
        // to different exposures (the queued one was taken before the
        // connect): each looks only among its own kind.
        let ambiguous: Vec<u64> = match (&shot_of, open.exposures_by_stem.get(&stem)) {
            (Shot::Waiting | Shot::Queued, Some(exposures)) => {
                let candidates: Vec<u64> = exposures
                    .iter()
                    .filter(|e| e.queued == queued && !e.kinds.contains(&extension))
                    .map(|e| e.number)
                    .collect();
                // Another exposure of the name could have taken it (more
                // than one open), or it may be a new exposure (the one open
                // is not the newest: a newer one already has this kind).
                let newest_has_it = exposures
                    .iter()
                    .rev()
                    .find(|e| e.queued == queued)
                    .is_some_and(|e| e.kinds.contains(&extension));
                if candidates.len() > 1 || (candidates.len() == 1 && newest_has_it) {
                    candidates
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        let companion = match shot_of {
            Shot::Waiting | Shot::Queued => {
                open.exposures_by_stem.get_mut(&stem).and_then(|exposures| {
                    exposures
                        .iter_mut()
                        .rev()
                        .find(|e| e.queued == queued && !e.kinds.contains(&extension))
                })
            }
            Shot::At(_) => None,
        };
        let (requested, exposure) = match companion {
            Some(e) => {
                e.kinds.push(extension);
                (e.request, e.number)
            }
            // A new exposure.
            None => {
                let request = match shot_of {
                    Shot::Waiting => open.waiting.pop_front(),
                    Shot::At(at) => Some(at),
                    Shot::Queued => None,
                };
                open.exposures += 1;
                let exposure = Exposure {
                    kinds: vec![extension],
                    request,
                    number: open.exposures,
                    queued,
                };
                open.exposures_by_stem
                    .entry(stem)
                    .or_default()
                    .push_back(exposure);
                (request, open.exposures)
            }
        };
        let started = requested.map_or(reported, |asked| asked.at);
        let connection = open.connection;
        let (data, size) = match downloaded {
            Ok(downloaded) => downloaded,
            Err(error) if error.kind == ErrorKind::Disconnected => return self.lost(error),
            Err(error) => {
                log::warn!(
                    "{} not downloaded after {} ms ({:?}: {}); request {:?}, exposure {exposure}",
                    file.name,
                    ms(reported),
                    error.kind,
                    error.message,
                    requested.map(|asked| asked.id)
                );
                return self.event(Event::CaptureFailed {
                    error: TetherError::new(
                        error.kind,
                        format!("{} could not be downloaded: {}", file.name, error.message),
                    ),
                    request: requested.map(|asked| asked.id),
                    exposure: Some((connection, exposure)),
                    ambiguous,
                });
            }
        };
        open.downloaded = shot;
        let identity = (data.len(), content_hash(&data));
        let expected = size.unwrap_or(data.len() as u64);
        let mut partial = match Partial::create(&open.staging, shot, &file.name) {
            Ok(partial) => partial,
            Err(e) => {
                return self.storage_failed(
                    &file,
                    e,
                    requested.map(|asked| asked.id),
                    (connection, exposure),
                    ambiguous,
                );
            }
        };
        if open.camera.abort_at(AbortPoint::MidDownload, shot) {
            let _ = partial.write(&data[..data.len() / 2]);
            partial.abandon();
            std::process::exit(70);
        }
        let published = partial
            .write(&data)
            .and_then(|()| partial.publish(expected));
        let path = match published {
            Ok(path) => path,
            Err(e) => {
                return self.storage_failed(
                    &file,
                    e,
                    requested.map(|asked| asked.id),
                    (connection, exposure),
                    ambiguous,
                );
            }
        };
        // Only a photo that is safely in the transfer folder counts as
        // downloaded (a repeat of a failed one is taken again).
        open.published.insert(file.clone(), identity);
        open.downloads.insert(file.clone(), shot);
        if queued {
            open.from_queue.remove(&file);
        }
        if open.camera.abort_at(AbortPoint::BeforeEvent, shot) {
            std::process::exit(70);
        }
        log::info!(
            "{}: {} bytes in {} ms; {}, exposure {exposure}{}, {} ms after the {}",
            file.name,
            expected,
            ms(reported),
            match requested {
                Some(asked) => format!("request {}", asked.id),
                None if queued => "left in the camera's transfer queue".to_string(),
                None => "fired on the body".to_string(),
            },
            if ambiguous.is_empty() {
                String::new()
            } else {
                format!(" (ambiguous: {ambiguous:?})")
            },
            ms(started),
            if requested.is_some() {
                "request"
            } else {
                "camera's report"
            }
        );
        let captured = Captured {
            path,
            connection: open.connection,
            download: shot,
            exposure,
            ambiguous,
            camera_folder: file.folder,
            camera_name: file.name,
            size: expected,
            requested: requested.is_some(),
            request: requested.map(|asked| asked.id),
            ms_to_event: requested.map(|_| reported.duration_since(started).as_millis() as u64),
            ms_to_downloaded: ms(started),
        };
        self.event(Event::Captured { captured })
    }

    fn storage_failed(
        &mut self,
        file: &CameraFile,
        e: io::Error,
        request: Option<u64>,
        exposure: (u64, u64),
        ambiguous: Vec<u64>,
    ) -> io::Result<()> {
        log::warn!("{} not saved to the transfer folder: {e}", file.name);
        self.event(Event::CaptureFailed {
            error: TetherError::new(
                ErrorKind::Storage,
                format!(
                    "{} could not be saved: {e}. It is still on the camera if the camera keeps a copy.",
                    file.name
                ),
            ),
            request,
            exposure: Some(exposure),
            ambiguous,
        })
    }
}
