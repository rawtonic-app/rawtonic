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
/// A shot whose file has not come after this long is reported as failed
/// (it is never fired again; a file that still comes is downloaded).
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(60);

struct Open {
    camera: Box<dyn Camera>,
    staging: PathBuf,
    live_view: bool,
    /// App shots still waiting for their file, oldest first.
    waiting: VecDeque<Instant>,
    /// Files downloaded from this camera.
    downloaded: u64,
}

pub struct Server<W: Write> {
    driver: Box<dyn Driver>,
    out: W,
    open: Option<Open>,
    shot_timeout: Duration,
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
        let result = self.answer(request, &mut fired);
        if let Err(e) = &result
            && e.kind == ErrorKind::Disconnected
        {
            self.open = None;
        }
        self.send(&Message::Reply { id, result })?;
        if let Some((file, at)) = fired {
            self.download(file, Some(at))?;
        }
        Ok(!shutdown)
    }

    fn camera(&mut self) -> Result<&mut Open, TetherError> {
        self.open.as_mut().ok_or_else(not_connected)
    }

    fn answer(
        &mut self,
        request: Request,
        fired: &mut Option<(CameraFile, Instant)>,
    ) -> Result<Reply, TetherError> {
        match request {
            Request::Hello => Ok(Reply::Hello {
                protocol: PROTOCOL,
                library: self.driver.library(),
            }),
            Request::ListCameras => Ok(Reply::Cameras {
                cameras: self.driver.list()?,
            }),
            Request::Connect { port, staging } => {
                if !staging.is_absolute() {
                    return Err(TetherError::new(
                        ErrorKind::Protocol,
                        "The transfer folder must be an absolute path.",
                    ));
                }
                self.open = None;
                let mut camera = self.driver.open(port.as_deref())?;
                let status = camera.status();
                self.open = Some(Open {
                    camera,
                    staging,
                    live_view: false,
                    waiting: VecDeque::new(),
                    downloaded: 0,
                });
                Ok(Reply::Connected { status })
            }
            Request::Disconnect => {
                self.open = None;
                Ok(Reply::Ok)
            }
            Request::Settings => Ok(Reply::Settings {
                settings: self.camera()?.camera.settings()?,
            }),
            Request::SetSetting { key, value } => {
                self.camera()?.camera.set_setting(&key, &value)?;
                Ok(Reply::Ok)
            }
            Request::Capture => {
                let open = self.camera()?;
                let at = Instant::now();
                match open.camera.fire()? {
                    Fired::Pending => open.waiting.push_back(at),
                    Fired::File(file) => *fired = Some((file, at)),
                }
                Ok(Reply::Ok)
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
            Request::Shutdown => {
                self.open = None;
                Ok(Reply::Ok)
            }
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
                .is_some_and(|at| at.elapsed() > self.shot_timeout)
        {
            open.waiting.pop_front();
            self.event(Event::CaptureFailed {
                error: TetherError::new(
                    ErrorKind::Timeout,
                    "The camera did not deliver the photo. It was not fired again.",
                ),
            })?;
        }
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        match open.camera.wait_event(wait) {
            Ok(CameraEvent::FileAdded(file)) => {
                let started = open.waiting.pop_front();
                self.download(file, started)
            }
            Ok(CameraEvent::SettingsChanged) => self.event(Event::SettingsChanged),
            Ok(_) => Ok(()),
            Err(error) if error.kind == ErrorKind::Disconnected => self.lost(error),
            Err(error) => {
                log::warn!("camera event: {error}");
                Ok(())
            }
        }
    }

    fn lost(&mut self, error: TetherError) -> io::Result<()> {
        self.open = None;
        self.event(Event::Disconnected { error })
    }

    /// Downloads a file the camera reported into the transfer folder and
    /// tells the app. `requested`: when the app asked for the shot (else it
    /// was fired on the body).
    fn download(&mut self, file: CameraFile, requested: Option<Instant>) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let reported = Instant::now();
        let started = requested.unwrap_or(reported);
        let shot = open.downloaded + 1;
        let (data, size) = match open.camera.download(&file) {
            Ok(downloaded) => downloaded,
            Err(error) if error.kind == ErrorKind::Disconnected => return self.lost(error),
            Err(error) => {
                return self.event(Event::CaptureFailed {
                    error: TetherError::new(
                        error.kind,
                        format!("{} could not be downloaded: {}", file.name, error.message),
                    ),
                });
            }
        };
        open.downloaded = shot;
        let expected = size.unwrap_or(data.len() as u64);
        let mut partial = match Partial::create(&open.staging, shot, &file.name) {
            Ok(partial) => partial,
            Err(e) => return self.storage_failed(&file, e),
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
            Err(e) => return self.storage_failed(&file, e),
        };
        if open.camera.abort_at(AbortPoint::BeforeEvent, shot) {
            std::process::exit(70);
        }
        let captured = Captured {
            path,
            camera_folder: file.folder,
            camera_name: file.name,
            size: expected,
            requested: requested.is_some(),
            ms_to_event: requested.map(|_| reported.duration_since(started).as_millis() as u64),
            ms_to_downloaded: ms(started),
        };
        self.event(Event::Captured { captured })
    }

    fn storage_failed(&mut self, file: &CameraFile, e: io::Error) -> io::Result<()> {
        self.event(Event::CaptureFailed {
            error: TetherError::new(
                ErrorKind::Storage,
                format!(
                    "{} could not be saved: {e}. It is still on the camera if the camera keeps a copy.",
                    file.name
                ),
            ),
        })
    }
}
