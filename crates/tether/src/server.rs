//! The helper's loop: one thread owns the camera. Between requests it
//! waits briefly for camera events, so photos fired on the body are
//! downloaded like the ones the app asks for, and sends live-view frames
//! while they are wanted.
use crate::camera::{AbortPoint, Camera, CameraEvent, CameraFile, Driver, Fired};
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
const ONE_AT_A_TIME_WAIT: Duration = Duration::from_secs(10);
/// An exposure of a file name (`Open::exposures_by_stem`).
struct Exposure {
    /// The file kinds (extensions) it gave so far.
    kinds: Vec<String>,
    /// Its app request, if any.
    request: Option<Asked>,
    /// `Captured::late_request`.
    late: Option<u64>,
    /// `Captured::exposure`.
    number: u64,
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

/// A shot whose file has not come after this long is reported as failed
/// (it is never fired again; a file that still comes is downloaded).
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// How many shots reported as not delivered are remembered for a photo
/// that still comes (`Open::timed_out`).
const TIMED_OUT_KEPT: usize = 16;

struct Open {
    camera: Box<dyn Camera>,
    staging: PathBuf,
    live_view: bool,
    /// App shots still waiting for their file, oldest first.
    waiting: VecDeque<Asked>,
    /// App shots reported as not delivered (`SHOT_TIMEOUT`) whose file
    /// has not come, oldest first (`Captured::late_request`).
    timed_out: VecDeque<u64>,
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
    /// This connection's number: a release names the connection its photo
    /// came from, so a late one never reaches another camera's photo.
    connection: u64,
    /// `Request::Hold`: the camera's events are not read (nothing more is
    /// downloaded) until the connection ends.
    held: bool,
}

pub struct Server<W: Write> {
    driver: Box<dyn Driver>,
    out: W,
    open: Option<Open>,
    shot_timeout: Duration,
    /// Connections opened so far.
    connections: u64,
    /// App shot requests so far (`Asked::id`).
    asked: u64,
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
            connections: 0,
            asked: 0,
        }
    }

    pub fn with_shot_timeout(mut self, timeout: Duration) -> Self {
        self.shot_timeout = timeout;
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
        if request == Request::Capture {
            for _ in 0..64 {
                if !self.read_events(Duration::ZERO)? {
                    break;
                }
            }
            self.wait_for_the_last_shot()?;
        }
        let result = match request {
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
                // A connection never starts with photos kept only in the
                // camera's memory: Save to goes to the card where the camera
                // offers that; else the camera is not opened.
                if camera.memory_only()? && (!camera.prefer_card() || camera.memory_only()?) {
                    return Err(TetherError::new(ErrorKind::Unsupported, MEMORY_ONLY));
                }
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
                    waiting: VecDeque::new(),
                    timed_out: VecDeque::new(),
                    downloaded: 0,
                    published: Default::default(),
                    downloads: Default::default(),
                    exposures_by_stem: Default::default(),
                    exposures: 0,
                    memory_only: false,
                    connection: self.connections,
                    held: false,
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
    /// shots fired before have their photo, for `ONE_AT_A_TIME_WAIT` at
    /// most (a shot that never comes is still reported by its timeout).
    fn wait_for_the_last_shot(&mut self) -> io::Result<()> {
        let started = Instant::now();
        let mut said = false;
        while let Some(open) = self.open.as_ref()
            && open.camera.one_shot_at_a_time()
            && !open.held
            && let Some(last) = open.waiting.back().map(|asked| asked.id)
        {
            if started.elapsed() >= ONE_AT_A_TIME_WAIT {
                log::warn!(
                    "request {last} still has no photo after {} ms; firing the next shot anyway",
                    ms(started)
                );
                break;
            }
            if !said {
                log::info!("the next shot waits for the photo of request {last}");
                said = true;
            }
            self.read_events(EVENT_POLL)?;
        }
        Ok(())
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
        loop {
            let Some(open) = self.open.as_mut() else {
                return Ok(Err(not_connected()));
            };
            let now = match value_of(&mut open.camera) {
                Ok(now) => now,
                Err(e) => return Ok(Err(e)),
            };
            if now.as_deref() == Some(value) || started.elapsed() >= READBACK {
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
        if open.live_view {
            wait = Duration::from_millis(1);
            match open.camera.preview() {
                Ok(jpeg) => frame::write(&mut self.out, frame::LIVE_VIEW, &jpeg)?,
                Err(error) => {
                    open.live_view = false;
                    if error.kind == ErrorKind::Disconnected {
                        return self.lost(error);
                    }
                    self.event(Event::LiveViewStopped { error })?;
                }
            }
        }
        while let Some(open) = self.open.as_mut()
            && open
                .waiting
                .front()
                .is_some_and(|asked| asked.at.elapsed() > self.shot_timeout)
        {
            let asked = open.waiting.pop_front();
            if let Some(asked) = asked {
                open.timed_out.push_back(asked.id);
                if open.timed_out.len() > TIMED_OUT_KEPT {
                    open.timed_out.pop_front();
                }
                log::warn!(
                    "request {}: no photo {} s after the shot; reported as not delivered, not fired again",
                    asked.id,
                    self.shot_timeout.as_secs()
                );
            }
            self.event(Event::CaptureFailed {
                error: TetherError::new(
                    ErrorKind::Timeout,
                    "No photo came within a minute of the shot: the camera may not have fired (with \
                     autofocus, a shot that finds no focus is not taken) or did not hand it over. It \
                     was not fired again.",
                ),
                request: asked.map(|asked| asked.id),
                exposure: None,
                ambiguous: Vec::new(),
            })?;
        }
        self.read_events(wait).map(|_| ())
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
            Ok(CameraEvent::FileAdded(file)) => self.download(file, Shot::Waiting).map(|()| true),
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
        log::info!(
            "camera reported {} ({} shot(s) waiting)",
            file.name,
            open.waiting.len()
        );
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
        let ambiguous: Vec<u64> = match (&shot_of, open.exposures_by_stem.get(&stem)) {
            (Shot::Waiting, Some(exposures)) => {
                let candidates: Vec<u64> = exposures
                    .iter()
                    .filter(|e| !e.kinds.contains(&extension))
                    .map(|e| e.number)
                    .collect();
                // Another exposure of the name could have taken it (more
                // than one open), or it may be a new exposure (the one open
                // is not the newest: a newer one already has this kind).
                let newest_has_it = exposures
                    .back()
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
            Shot::Waiting => open.exposures_by_stem.get_mut(&stem).and_then(|exposures| {
                exposures
                    .iter_mut()
                    .rev()
                    .find(|e| !e.kinds.contains(&extension))
            }),
            Shot::At(_) => None,
        };
        let (requested, late, exposure) = match companion {
            Some(e) => {
                e.kinds.push(extension);
                (e.request, e.late, e.number)
            }
            // A new exposure.
            None => {
                let request = match shot_of {
                    Shot::Waiting => open.waiting.pop_front(),
                    Shot::At(at) => Some(at),
                };
                // No shot waiting, but one reported as not delivered: this
                // may be its photo, come after all, or a shot on the body
                // (which cannot be told).
                let late = match request {
                    None => open.timed_out.pop_front(),
                    Some(_) => None,
                };
                if let Some(late) = late {
                    log::warn!(
                        "{}: no shot waiting; may be the late photo of request {late} (reported as not delivered) or a shot on the body",
                        file.name
                    );
                }
                open.exposures += 1;
                let exposure = Exposure {
                    kinds: vec![extension],
                    request,
                    late,
                    number: open.exposures,
                };
                open.exposures_by_stem
                    .entry(stem)
                    .or_default()
                    .push_back(exposure);
                (request, late, open.exposures)
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
        if open.camera.abort_at(AbortPoint::BeforeEvent, shot) {
            std::process::exit(70);
        }
        log::info!(
            "{}: {} bytes in {} ms; {}, exposure {exposure}{}, {} ms after the {}",
            file.name,
            expected,
            ms(reported),
            requested.map_or("fired on the body".to_string(), |asked| format!(
                "request {}",
                asked.id
            )),
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
            late_request: late,
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
