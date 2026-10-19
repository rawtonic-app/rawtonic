//! The app's side: starts the helper, sends requests and waits for their
//! replies (with a timeout each), and hands events and live-view frames to
//! a callback. When the helper ends — a crash in the camera library, a
//! killed process — every waiting request fails and the callback hears
//! `Exited` with the end of the helper's log.
use crate::frame;
use crate::protocol::{Envelope, ErrorKind, Event, Message, PROTOCOL, Reply, Request, TetherError};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// What the helper sends without being asked.
#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    LiveView(Vec<u8>),
    /// The helper ended; its last log lines.
    Exited(String),
}

type Waiting = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Reply, TetherError>>>>>;

pub struct Client {
    child: Arc<Mutex<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    /// Held while a request is out: the helper answers one at a time, so a
    /// request's timeout starts when it is its turn (a short request queued
    /// behind a long capture is not "unresponsive").
    dispatch: Mutex<()>,
    waiting: Waiting,
    next: AtomicU64,
    ended: Arc<AtomicBool>,
}

const LOG_TAIL: usize = 20;

fn helper_gone() -> TetherError {
    TetherError::new(
        ErrorKind::Disconnected,
        "The tether helper stopped. Connect the camera again.",
    )
}

impl Client {
    /// Starts `program` with `args` and checks that it speaks this app's
    /// protocol. Returns the client and the helper's camera library version
    /// (`None` for the mock).
    pub fn spawn(
        program: &Path,
        args: &[OsString],
        envs: &[(OsString, OsString)],
        on_incoming: impl Fn(Incoming) + Send + Sync + 'static,
    ) -> Result<(Self, Option<String>), TetherError> {
        let mut child = Command::new(program)
            .args(args)
            .envs(envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                TetherError::new(
                    ErrorKind::Unsupported,
                    format!("The tether helper could not start: {e}"),
                )
            })?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        {
            let tail = tail.clone();
            let _ = std::thread::Builder::new()
                .name("tether-log".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        log::info!("tether helper: {line}");
                        let mut tail = tail.lock().unwrap();
                        if tail.len() == LOG_TAIL {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                });
        }
        let waiting: Waiting = Arc::default();
        let on_incoming = Arc::new(on_incoming);
        let ended = Arc::new(AtomicBool::new(false));
        // The helper's end, heard once: from the end of its output or from
        // its exit (another process started meanwhile can hold a copy of
        // its pipe, so the output may never end).
        let end = {
            let (waiting, on_incoming, ended) =
                (waiting.clone(), on_incoming.clone(), ended.clone());
            move || {
                if ended.swap(true, Ordering::AcqRel) {
                    return;
                }
                // Every request still waiting fails now.
                for (_, tx) in waiting.lock().unwrap().drain() {
                    let _ = tx.send(Err(helper_gone()));
                }
                // Its last words reach the log thread shortly after.
                std::thread::sleep(Duration::from_millis(100));
                let tail: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
                on_incoming(Incoming::Exited(tail.join("\n")));
            }
        };
        let end = Arc::new(end);
        let child = Arc::new(Mutex::new(child));
        {
            // Runs until the helper is reaped, also when the end of its
            // output was heard first (else it stays a zombie).
            let (child, end) = (child.clone(), end.clone());
            std::thread::Builder::new()
                .name("tether-wait".into())
                .spawn(move || {
                    loop {
                        if !matches!(child.lock().unwrap().try_wait(), Ok(None)) {
                            end();
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
                .map_err(|e| TetherError::new(ErrorKind::Other, e.to_string()))?;
        }
        {
            let waiting = waiting.clone();
            std::thread::Builder::new()
                .name("tether-read".into())
                .spawn(move || {
                    loop {
                        let frame = match frame::read(&mut stdout) {
                            Ok(Some(frame)) => frame,
                            Ok(None) => break,
                            Err(e) => {
                                log::error!("tether helper: {e}");
                                break;
                            }
                        };
                        if frame.kind == frame::LIVE_VIEW {
                            on_incoming(Incoming::LiveView(frame.payload));
                            continue;
                        }
                        match serde_json::from_slice::<Message>(&frame.payload) {
                            Ok(Message::Reply { id, result }) => {
                                if let Some(tx) = waiting.lock().unwrap().remove(&id) {
                                    let _ = tx.send(result);
                                }
                            }
                            Ok(Message::Event { event }) => on_incoming(Incoming::Event(event)),
                            Err(e) => {
                                log::error!("tether helper sent a malformed message: {e}");
                                break;
                            }
                        }
                    }
                    end();
                })
                .map_err(|e| TetherError::new(ErrorKind::Other, e.to_string()))?;
        }
        let client = Self {
            child,
            stdin: Mutex::new(stdin),
            dispatch: Mutex::new(()),
            waiting,
            next: AtomicU64::new(1),
            ended,
        };
        match client.request(Request::Hello, Duration::from_secs(10))? {
            Reply::Hello { protocol, library } if protocol == PROTOCOL => Ok((client, library)),
            Reply::Hello { protocol, .. } => Err(TetherError::new(
                ErrorKind::Protocol,
                format!("The tether helper speaks protocol {protocol}, this app {PROTOCOL}."),
            )),
            other => Err(TetherError::new(
                ErrorKind::Protocol,
                format!("Unexpected answer from the tether helper: {other:?}"),
            )),
        }
    }

    /// Sends `request` and waits up to `timeout` for its reply.
    pub fn request(&self, request: Request, timeout: Duration) -> Result<Reply, TetherError> {
        let _turn = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        // The helper's end marks `ended` before it fails the waiting
        // requests: one registered after that would wait for its timeout.
        if self.ended.load(Ordering::Acquire) {
            self.waiting.lock().unwrap().remove(&id);
            return Err(helper_gone());
        }
        let sent = {
            let mut stdin = self.stdin.lock().unwrap();
            match stdin.as_mut() {
                Some(stdin) => frame::write_json(stdin, &Envelope { id, request }).is_ok(),
                None => false,
            }
        };
        if !sent {
            self.waiting.lock().unwrap().remove(&id);
            return Err(helper_gone());
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.waiting.lock().unwrap().remove(&id);
                Err(TetherError::new(
                    ErrorKind::Unresponsive,
                    "The camera did not answer in time.",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(helper_gone()),
        }
    }

    /// Whether the helper still runs.
    pub fn alive(&self) -> bool {
        !self.ended.load(Ordering::Acquire)
    }

    /// Ends the helper at once (it hangs in the camera library, say).
    pub fn kill(&self) {
        let _ = self.child.lock().unwrap().kill();
    }

    /// The helper's process id.
    pub fn pid(&self) -> u32 {
        self.child.lock().unwrap().id()
    }
}

impl Drop for Client {
    /// Asks the helper to close the camera and end; kills it if it does not
    /// within two seconds.
    fn drop(&mut self) {
        let _ = self.request(Request::Shutdown, Duration::from_secs(2));
        self.stdin.lock().unwrap().take();
        let mut child = self.child.lock().unwrap();
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
