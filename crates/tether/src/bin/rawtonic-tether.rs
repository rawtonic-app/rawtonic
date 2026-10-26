//! The tether helper. Started by the app, it serves requests on stdin and
//! answers on stdout (`rawtonic_tether::server`); its log goes to stderr.
//!
//!   rawtonic-tether [--mock]          serve (the mock camera with --mock;
//!                                     its script in RAWTONIC_TETHER_MOCK)
//!   rawtonic-tether list              the cameras on USB
//!   rawtonic-tether version           libgphoto2's version and how many
//!                                     camera models its drivers know
//!   rawtonic-tether probe --out DIR [--port P] [--shots N] [--interval-ms MS]
//!                         [--body-seconds S] [--live-view-frames N] [--mock]
//!       what a body does, through the same helper process the app uses:
//!       settings, app shots with their timings, shots fired on the body,
//!       autofocus, live view. One JSON report on stdout; downloads go to DIR.
//!   rawtonic-tether liveview --out DIR [--port P] [--seconds S] [--shots N] [--mock]
//!       live view the way a session uses it, phase by phase: before any
//!       shot, through N shots fired while it runs, and started again after
//!       them — frame rate, gaps, why it stopped. One JSON report on stdout.
//!
//! RAWTONIC_TETHER_TRACE=FILE writes a trace for a session with a body:
//! every line of the helper's log and every libgphoto2 debug line (each PTP
//! request with its parameters, failed responses with their codes; no data
//! dumps), unthrottled, on the helper's clock. Never over an existing file:
//! a second helper writes FILE.2, and so on. It holds the camera's serial
//! number and file names: keep it on the machine. RAWTONIC_TETHER_LV_EXPERIMENT
//! names live-view experiments (`rawtonic_tether::experiment`).
use rawtonic_tether::camera::Driver;
use rawtonic_tether::client::{Client, Incoming};
use rawtonic_tether::experiment::LiveViewExperiment;
use rawtonic_tether::mock::{MockDriver, MockSpec};
use rawtonic_tether::protocol::{Event, Reply, Request};
use rawtonic_tether::server::Server;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The helper's log on stderr (the app writes it into its own log): each
/// line with the helper's clock in ms (the app's log has whole seconds),
/// and a line repeated at once only once, with how often it came.
/// libgphoto2's own messages come at most once a minute each: some come on
/// every event poll (a read that timed out, a Sony property type), 50 times
/// a second (an a7 III test wrote 4 MB in six minutes).
struct StderrLog {
    level: log::Level,
    started: Instant,
    /// The last line and how often it came again since.
    last: std::sync::Mutex<(String, u64)>,
    /// Each libgphoto2 message: when it was last written, and how often it
    /// came since.
    library: std::sync::Mutex<std::collections::HashMap<String, (Instant, u64)>>,
    /// `RAWTONIC_TETHER_TRACE`: every line, unthrottled, down to debug.
    trace: Option<std::sync::Mutex<std::io::LineWriter<std::fs::File>>>,
}

/// How often one libgphoto2 message is written at most.
const LIBRARY_MESSAGE_EVERY: Duration = Duration::from_secs(60);

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
            || (self.trace.is_some() && metadata.level() <= log::Level::Debug)
    }

    fn log(&self, record: &log::Record) {
        if let Some(trace) = &self.trace
            && record.level() <= log::Level::Debug
        {
            use std::io::Write as _;
            let mut trace = trace.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(
                trace,
                "+{:.1}ms {} {} {}",
                self.started.elapsed().as_secs_f64() * 1000.0,
                record.level(),
                record.target(),
                record.args()
            );
        }
        if record.level() <= self.level {
            // Never a panic when the app's end of the pipe is gone (eprintln!
            // would panic): the helper must still be able to end itself.
            use std::io::Write as _;
            let mut line = format!("{} {} {}", record.level(), record.target(), record.args());
            if record.target() == "gphoto2" {
                let mut seen = self.library.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                match seen.get_mut(&line) {
                    Some((at, more)) if now.duration_since(*at) < LIBRARY_MESSAGE_EVERY => {
                        *more += 1;
                        return;
                    }
                    Some((at, more)) => {
                        if *more > 0 {
                            line = format!("{line} (and {more} times in the minute before)");
                        }
                        *at = now;
                        *more = 0;
                    }
                    None => {
                        // Bounded: messages naming objects or values differ.
                        if seen.len() > 1000 {
                            seen.clear();
                        }
                        seen.insert(line.clone(), (now, 0));
                    }
                }
            }
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if last.0 == line {
                last.1 += 1;
                return;
            }
            let mut err = std::io::stderr().lock();
            if last.1 > 0 {
                let _ = writeln!(err, "(the line before came {} more times)", last.1);
            }
            let _ = writeln!(err, "+{}ms {line}", self.started.elapsed().as_millis());
            *last = (line, 0);
        }
    }

    fn flush(&self) {}
}

/// Opens the trace file `RAWTONIC_TETHER_TRACE` names (or FILE.2, FILE.3, …
/// when it exists); `None` without the variable or when no new file could be
/// made (said on stderr).
fn open_trace() -> Option<std::fs::File> {
    let path = PathBuf::from(std::env::var_os("RAWTONIC_TETHER_TRACE")?);
    for n in 1..100 {
        let candidate = if n == 1 {
            path.clone()
        } else {
            let mut name = path.clone().into_os_string();
            name.push(format!(".{n}"));
            PathBuf::from(name)
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Some(file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "rawtonic-tether: no trace ({}): {e}",
                    candidate.display()
                );
                return None;
            }
        }
    }
    None
}

fn driver(mock: bool, experiment: LiveViewExperiment) -> Result<Box<dyn Driver>, String> {
    if mock {
        let spec = match std::env::var("RAWTONIC_TETHER_MOCK") {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("mock script: {e}"))?,
            Err(_) => MockSpec::default(),
        };
        return Ok(Box::new(MockDriver { spec }));
    }
    #[cfg(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux")))]
    {
        rawtonic_tether::gphoto::GphotoDriver::new()
            .map(|d| Box::new(d.with_live_view_experiment(experiment)) as Box<dyn Driver>)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
    {
        let _ = experiment;
        Ok(Box::new(NoCameras))
    }
}

/// A helper built without camera support (feature `gphoto2`): it answers,
/// and says so when asked for a camera.
#[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
struct NoCameras;

#[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
impl NoCameras {
    fn unsupported() -> rawtonic_tether::protocol::TetherError {
        rawtonic_tether::protocol::TetherError::new(
            rawtonic_tether::protocol::ErrorKind::Unsupported,
            "This build's tether helper has no camera support (built without libgphoto2).",
        )
    }
}

#[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
impl Driver for NoCameras {
    fn library(&self) -> Option<String> {
        None
    }

    fn list(
        &mut self,
    ) -> Result<Vec<rawtonic_tether::protocol::CameraInfo>, rawtonic_tether::protocol::TetherError>
    {
        Err(Self::unsupported())
    }

    fn open(
        &mut self,
        _port: Option<&str>,
    ) -> Result<Box<dyn rawtonic_tether::camera::Camera>, rawtonic_tether::protocol::TetherError>
    {
        Err(Self::unsupported())
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // RAWTONIC_TETHER_LOG=debug also logs libgphoto2's own messages (what a
    // body answers, step by step) — for diagnosing a camera.
    let debug = std::env::var("RAWTONIC_TETHER_LOG").is_ok_and(|v| v == "debug");
    let level = if debug {
        log::Level::Debug
    } else {
        log::Level::Info
    };
    // The probes start a helper of their own, which writes the trace.
    let trace = if matches!(args.first().map(String::as_str), Some("probe" | "liveview")) {
        None
    } else {
        open_trace()
    };
    let traced = trace.is_some();
    let logger: &'static StderrLog = Box::leak(Box::new(StderrLog {
        level,
        started: Instant::now(),
        last: Default::default(),
        library: Default::default(),
        trace: trace.map(|file| std::sync::Mutex::new(std::io::LineWriter::new(file))),
    }));
    let max = if traced {
        log::Level::Debug.max(level)
    } else {
        level
    };
    let _ = log::set_logger(logger).map(|()| log::set_max_level(max.to_level_filter()));
    #[cfg(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux")))]
    // libgphoto2's errors always (a camera that did not answer, an object
    // not found); everything with RAWTONIC_TETHER_LOG=debug or a trace.
    let _gphoto_log = gphoto2::log_bridge::forward_to_log(if debug || traced {
        gphoto2::log_bridge::LogLevel::Debug
    } else {
        gphoto2::log_bridge::LogLevel::Error
    })
    .ok();
    // macOS: libgphoto2 reaches cameras through ImageCaptureCore (the `icc`
    // port driver), which answers on the main thread's event loop. The work
    // runs on its own thread; the main thread only runs that loop.
    #[cfg(target_os = "macos")]
    {
        std::thread::spawn(move || {
            let code = run(&args);
            std::process::exit(code);
        });
        main_run_loop();
    }
    #[cfg(not(target_os = "macos"))]
    std::process::exit(run(&args));
}

/// Runs the application event loop on the main thread. ImageCaptureCore
/// hands its cameras only to a running NSApplication (a bare CFRunLoop
/// gets none); the "prohibited" activation policy keeps it without a Dock
/// icon or menu bar.
#[cfg(target_os = "macos")]
fn main_run_loop() -> ! {
    use std::ffi::{c_char, c_void};
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    type Send = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type SendPolicy = unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> i8;
    const ACTIVATION_POLICY_PROHIBITED: isize = 2;
    // SAFETY: objc_msgSend called through the signatures of the methods
    // sent (+sharedApplication, -setActivationPolicy:, -run), on the main
    // thread as AppKit requires.
    unsafe {
        let send: Send = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_policy: SendPolicy = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let app = send(
            objc_getClass(c"NSApplication".as_ptr()),
            sel_registerName(c"sharedApplication".as_ptr()),
        );
        send_policy(
            app,
            sel_registerName(c"setActivationPolicy:".as_ptr()),
            ACTIVATION_POLICY_PROHIBITED,
        );
        send(app, sel_registerName(c"run".as_ptr()));
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// How long the helper waits after the app is gone before it ends itself.
#[cfg(unix)]
const PARENT_GRACE: Duration = Duration::from_secs(10);

/// Serving the app: when the app is gone (quit or crashed; the helper is
/// handed to another parent) its requests end and the helper closes the
/// camera. A helper stuck inside a camera call never reads that end, so
/// after `grace` it ends itself instead of outliving the app. Nothing is
/// lost: an unfinished download was never released on the camera, and its
/// partial transfer file is kept as incomplete.
#[cfg(unix)]
fn end_with_the_app(grace: Duration) {
    let parent = std::os::unix::process::parent_id();
    std::thread::spawn(move || {
        while std::os::unix::process::parent_id() == parent {
            std::thread::sleep(Duration::from_millis(250));
        }
        std::thread::sleep(grace);
        // Nothing that could fail before the end (the app's stderr pipe is
        // usually closed by now).
        // SAFETY: _exit ends the process at once, without exit handlers that
        // could wait for the camera call still running on another thread.
        unsafe { libc::_exit(3) };
    });
}

/// The command line's work; the process's exit code.
fn run(args: &[String]) -> i32 {
    let mock = args.iter().any(|a| a == "--mock");
    let result = match args.first().map(String::as_str) {
        Some("list") => driver(mock, LiveViewExperiment::default()).and_then(|mut d| {
            let cameras = d.list().map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&cameras).unwrap());
            Ok(())
        }),
        Some("probe") => probe(args, mock),
        Some("liveview") => live_view_sequence(args, mock),
        Some("version") => version(),
        _ => {
            let experiment = LiveViewExperiment::from_env();
            serve(mock, experiment)
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "rawtonic-tether: {e}");
            1
        }
    }
}

/// Serves the app's requests (the helper's normal work).
fn serve(mock: bool, experiment: LiveViewExperiment) -> Result<(), String> {
    driver(mock, experiment.clone()).and_then(|d| {
        #[cfg(unix)]
        end_with_the_app(
            std::env::var("RAWTONIC_TETHER_PARENT_GRACE_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map(Duration::from_millis)
                .unwrap_or(PARENT_GRACE),
        );
        // Tests shorten how long a shot may take.
        let timeout = std::env::var("RAWTONIC_TETHER_SHOT_TIMEOUT_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(rawtonic_tether::server::SHOT_TIMEOUT);
        let one_at_a_time = std::env::var("RAWTONIC_TETHER_ONE_AT_A_TIME_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(rawtonic_tether::server::ONE_AT_A_TIME_WAIT);
        Server::new(d, std::io::stdout().lock())
            .with_live_view_experiment(experiment)
            .with_shot_timeout(timeout)
            .with_one_at_a_time_wait(one_at_a_time)
            .run(std::io::stdin())
            .map_err(|e| e.to_string())
    })
}

fn version() -> Result<(), String> {
    #[cfg(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux")))]
    {
        let models = rawtonic_tether::gphoto::known_models().map_err(|e| e.to_string())?;
        println!(
            "{}",
            json!({
                "protocol": rawtonic_tether::protocol::PROTOCOL,
                "libgphoto2": rawtonic_tether::gphoto::library_version(),
                "cameraModels": models,
            })
        );
        if models == 0 {
            return Err("libgphoto2 found no camera drivers (CAMLIBS)".into());
        }
        Ok(())
    }
    #[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
    {
        println!(
            "{}",
            json!({ "protocol": rawtonic_tether::protocol::PROTOCOL, "libgphoto2": null })
        );
        Ok(())
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e4).round() / 10.0
}

/// Writes `bytes` to a new file at `path` (never over an existing file or
/// through a link); what happened, for the report.
fn save_new(path: &Path, bytes: &[u8]) -> String {
    use std::io::Write;
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()));
    match written {
        Ok(()) => path.display().to_string(),
        Err(e) => format!("not saved: {e}"),
    }
}

/// Runs the helper as a child process (exactly as the app does) and asks a
/// body what it can do.
fn probe(args: &[String], mock: bool) -> Result<(), String> {
    let out = PathBuf::from(arg(args, "--out").ok_or("probe needs --out DIR")?);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;
    let shots: u32 = arg(args, "--shots")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let interval = Duration::from_millis(
        arg(args, "--interval-ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(3000),
    );
    let body_seconds: u64 = arg(args, "--body-seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let frames: u32 = arg(args, "--live-view-frames")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let (tx, rx) = mpsc::channel::<(Instant, Incoming)>();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper_args: Vec<OsString> = if mock { vec!["--mock".into()] } else { vec![] };
    let (client, library) = Client::spawn(&exe, &helper_args, &[], move |incoming| {
        let _ = tx.send((Instant::now(), incoming));
    })
    .map_err(|e| e.to_string())?;
    let long = Duration::from_secs(30);
    let mut report = json!({ "library": library });
    let started = Instant::now();
    let cameras = client.request(Request::ListCameras, long);
    report["listMs"] = json!(ms(started.elapsed()));
    report["cameras"] = json!(format!("{cameras:?}"));
    let started = Instant::now();
    let connected = client.request(
        Request::Connect {
            port: arg(args, "--port"),
            staging: out.clone(),
            serial: None,
        },
        long,
    );
    report["connectMs"] = json!(ms(started.elapsed()));
    let status = match connected {
        Ok(Reply::Connected { status }) => status,
        other => {
            report["connect"] = json!(format!("{other:?}"));
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            return Err("could not connect".into());
        }
    };
    report["status"] = serde_json::to_value(&status).unwrap();
    let started = Instant::now();
    let settings = client.request(Request::Settings, long);
    report["settingsMs"] = json!(ms(started.elapsed()));
    report["settings"] = match &settings {
        Ok(Reply::Settings { settings }) => serde_json::to_value(settings).unwrap(),
        other => json!(format!("{other:?}")),
    };
    // Setting a value: ISO to a neighbouring choice and back.
    if let Ok(Reply::Settings { settings }) = &settings
        && let Some(iso) = settings.iter().find(|s| s.key == "iso" && !s.read_only)
        && let Some(i) = iso.choices.iter().position(|c| *c == iso.value)
    {
        let other = iso
            .choices
            .get(i + 1)
            .or(iso.choices.get(i.wrapping_sub(1)));
        if let Some(other) = other {
            let set = |value: &str| {
                let t = Instant::now();
                let r = client.request(
                    Request::SetSetting {
                        key: "iso".into(),
                        value: value.into(),
                    },
                    long,
                );
                json!({ "value": value, "ms": ms(t.elapsed()), "result": format!("{r:?}") })
            };
            let there = set(other);
            let read = client.request(Request::Settings, long);
            let read_value = match read {
                Ok(Reply::Settings { settings }) => settings
                    .into_iter()
                    .find(|s| s.key == "iso")
                    .map(|s| s.value),
                _ => None,
            };
            let back = set(&iso.value.clone());
            report["setIso"] = json!({ "to": there, "readBack": read_value, "back": back });
        }
    }
    let drain = |until: Instant, events: &mut Vec<Value>| {
        while let Ok((at, incoming)) =
            rx.recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            match incoming {
                Incoming::Event(Event::Captured { captured }) => {
                    let mut v = serde_json::to_value(&captured).unwrap();
                    v["msSinceProbe"] = json!(ms(at.duration_since(started)));
                    events.push(v);
                }
                Incoming::Event(e) => events.push(json!(format!("{e:?}"))),
                Incoming::LiveView(_) => {}
                Incoming::Exited(log) => events.push(json!({ "exited": log })),
            }
            if Instant::now() >= until {
                break;
            }
        }
    };
    let mut events = Vec::new();
    let mut fires = Vec::new();
    for _ in 0..shots {
        let t = Instant::now();
        let r = client.request(Request::Capture, long);
        fires.push(json!({ "ms": ms(t.elapsed()), "result": format!("{r:?}") }));
        drain(t + interval, &mut events);
    }
    report["fires"] = json!(fires);
    if body_seconds > 0 {
        eprintln!("probe: press the shutter on the camera now ({body_seconds} s)");
        drain(
            Instant::now() + Duration::from_secs(body_seconds),
            &mut events,
        );
    }
    report["captured"] = json!(events);
    let t = Instant::now();
    let af = client.request(Request::Autofocus, long);
    report["autofocus"] = json!({ "ms": ms(t.elapsed()), "result": format!("{af:?}") });
    if status.can_preview && frames > 0 {
        let t = Instant::now();
        let on = client.request(Request::LiveView { on: true }, long);
        let mut got = 0u32;
        let mut sizes = Vec::new();
        let mut first = None;
        let mut first_frame = None;
        let mut stopped = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        while got < frames && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok((at, Incoming::LiveView(jpeg))) => {
                    first.get_or_insert(at);
                    got += 1;
                    sizes.push(jpeg.len());
                    if got == 1 {
                        first_frame = Some(save_new(&out.join("liveview-first.jpg"), &jpeg));
                    }
                }
                Ok((_, Incoming::Event(Event::LiveViewStopped { error }))) => {
                    stopped = Some(serde_json::to_value(&error).unwrap());
                    break;
                }
                _ => {}
            }
        }
        let off = client.request(Request::LiveView { on: false }, long);
        let span = first.map(|f| f.elapsed()).unwrap_or_default();
        sizes.sort();
        let median_bytes = sizes.get(sizes.len() / 2).copied();
        let fps = if got > 1 {
            (got - 1) as f64 / span.as_secs_f64()
        } else {
            0.0
        };
        report["liveView"] = json!({
            "firstFrameFile": first_frame,
            "on": format!("{on:?}"),
            "off": format!("{off:?}"),
            "frames": got,
            "firstFrameMs": first.map(|f| ms(f.duration_since(t))),
            "fps": fps,
            "medianBytes": median_bytes,
            "stopped": stopped,
        });
    }
    let _ = client.request(Request::Disconnect, long);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

/// One phase of `liveview`: the frames that came, why live view stopped,
/// the shot fired in it.
#[derive(Default)]
struct Phase {
    frames: Vec<Instant>,
    sizes: Vec<usize>,
    stopped: Vec<Value>,
    /// The request number of the shot fired in this phase.
    request: Option<u64>,
    /// When the shot was asked for, when the helper answered (it had fired:
    /// a frame that comes after the answer was taken after the shot).
    fire: Option<(Instant, Instant, Value)>,
    restart: Option<Value>,
    first_frame_file: Option<String>,
}

impl Phase {
    fn report(&self, name: &str, started: Instant) -> Value {
        let mut gaps: Vec<f64> = self
            .frames
            .windows(2)
            .map(|w| ms(w[1].duration_since(w[0])))
            .collect();
        gaps.sort_by(f64::total_cmp);
        let at = |q: f64| gaps.get(((gaps.len() as f64 - 1.0) * q).round() as usize);
        let fps = match (self.frames.first(), self.frames.last()) {
            (Some(first), Some(last)) if self.frames.len() > 1 => {
                (self.frames.len() - 1) as f64 / last.duration_since(*first).as_secs_f64()
            }
            _ => 0.0,
        };
        let fired = self.fire.as_ref().map(|(_, replied, _)| *replied);
        let mut sizes = self.sizes.clone();
        sizes.sort();
        let median_bytes = sizes.get(sizes.len() / 2).copied();
        json!({
            "phase": name,
            "frames": self.frames.len(),
            "fps": (fps * 10.0).round() / 10.0,
            "firstFrameMs": self.frames.first().map(|f| ms(f.duration_since(started))),
            "medianGapMs": at(0.5),
            "p95GapMs": at(0.95),
            "maxGapMs": gaps.last(),
            "medianBytes": median_bytes,
            "fire": self.fire.as_ref().map(|(_, _, fire)| fire.clone()),
            "framesAfterShot": fired.map(|f| self.frames.iter().filter(|t| **t > f).count()),
            "firstFrameAfterShotMs": fired
                .and_then(|f| self.frames.iter().find(|t| **t > f).map(|t| ms(t.duration_since(f)))),
            "request": self.request,
            "stopped": self.stopped,
            "turnedOnAgain": self.restart,
            "firstFrameFile": self.first_frame_file,
        })
    }
}

/// A photo of the `liveview` sequence, with the shot it answers (by its
/// request number) and when it came. When the helper could not tell which
/// exposure a file belongs to (a reused name: `Captured::ambiguous`), the
/// shot is a guess: `certain` is false and no time after the shot is given.
fn photo(
    photos: &mut Vec<Value>,
    fired: &std::collections::HashMap<u64, (String, Instant)>,
    captured: &rawtonic_tether::protocol::Captured,
    at: Instant,
    arrived_in: &str,
) {
    let shot = captured.request.and_then(|r| fired.get(&r));
    let certain = captured.ambiguous.is_empty();
    photos.push(json!({
        "file": captured.camera_name,
        "request": captured.request,
        "exposure": captured.exposure,
        "ambiguous": captured.ambiguous,
        "certain": certain,
        "shot": shot.map(|(phase, _)| phase),
        "msAfterFire": shot.filter(|_| certain).map(|(_, t)| ms(at.duration_since(*t))),
        "arrivedIn": arrived_in,
    }));
}

/// `liveview`: live view the way a session uses it. Phase "before shots";
/// then one phase per shot, fired a quarter into the phase while live view
/// runs (when live view stops, it is turned on again once, a second later,
/// as a user would); then "started again" (off, then on). Photos are
/// downloaded into DIR like the probe's, never deleted on the camera.
fn live_view_sequence(args: &[String], mock: bool) -> Result<(), String> {
    let out = PathBuf::from(arg(args, "--out").ok_or("liveview needs --out DIR")?);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;
    let span = Duration::from_secs_f64(
        arg(args, "--seconds")
            .and_then(|s| s.parse().ok())
            .unwrap_or(8.0),
    );
    let shots: u32 = arg(args, "--shots")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let (tx, rx) = mpsc::channel::<(Instant, Incoming)>();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper_args: Vec<OsString> = if mock { vec!["--mock".into()] } else { vec![] };
    let (client, library) = Client::spawn(&exe, &helper_args, &[], move |incoming| {
        let _ = tx.send((Instant::now(), incoming));
    })
    .map_err(|e| e.to_string())?;
    let long = Duration::from_secs(30);
    let mut report = json!({
        "library": library,
        "experiment": std::env::var(rawtonic_tether::experiment::ENV).ok(),
    });
    let connected = client.request(
        Request::Connect {
            port: arg(args, "--port"),
            staging: out.clone(),
            serial: None,
        },
        long,
    );
    match connected {
        Ok(Reply::Connected { status }) if status.can_preview => {
            report["model"] = json!(status.model);
            report["firmware"] = json!(status.firmware);
        }
        other => {
            report["connect"] = json!(format!("{other:?}"));
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            return Err("no camera with live view connected".into());
        }
    }
    let mut phases = Vec::new();
    // Each shot's request number → its phase and when it was asked for:
    // a photo belongs to the shot it answers, whenever it comes.
    let mut fired: std::collections::HashMap<u64, (String, Instant)> = Default::default();
    let mut photos = Vec::new();
    let count = shots + 2;
    for n in 0..count {
        let name = match n {
            0 => "before shots".to_string(),
            n if n == count - 1 => "started again".to_string(),
            n => format!("shot {n}"),
        };
        let mut phase = Phase::default();
        if n == count - 1 {
            let off = client.request(Request::LiveView { on: false }, long);
            phase
                .stopped
                .push(json!({ "turnedOff": format!("{off:?}") }));
            let pause = Instant::now() + Duration::from_secs(1);
            while let Ok((at, incoming)) =
                rx.recv_timeout(pause.saturating_duration_since(Instant::now()))
            {
                if let Incoming::Event(Event::Captured { captured }) = incoming {
                    photo(&mut photos, &fired, &captured, at, "between phases");
                }
            }
        }
        let started = Instant::now();
        let on = client.request(Request::LiveView { on: true }, long);
        if on.is_err() {
            phase.stopped.push(json!({ "turnOn": format!("{on:?}") }));
        }
        let fire_at = (n > 0 && n < count - 1).then(|| started + span / 4);
        let mut stopped_at = None;
        let end = started + span;
        while Instant::now() < end {
            if let Some(at) = fire_at
                && phase.fire.is_none()
                && Instant::now() >= at
            {
                let t = Instant::now();
                let r = client.request(Request::Capture, long);
                if let Ok(Reply::Fired { request }) = &r {
                    phase.request = Some(*request);
                    fired.insert(*request, (name.clone(), t));
                }
                phase.fire = Some((
                    t,
                    Instant::now(),
                    json!({ "ms": ms(t.elapsed()), "result": format!("{r:?}") }),
                ));
            }
            if let Some(at) = stopped_at
                && phase.restart.is_none()
                && at + Duration::from_secs(1) <= Instant::now()
            {
                let r = client.request(Request::LiveView { on: true }, long);
                phase.restart = Some(json!({
                    "msAfterStop": ms(Instant::now().duration_since(at)),
                    "result": format!("{r:?}"),
                }));
            }
            let next = fire_at
                .filter(|_| phase.fire.is_none())
                .into_iter()
                .chain(
                    stopped_at
                        .filter(|_| phase.restart.is_none())
                        .map(|at| at + Duration::from_secs(1)),
                )
                .chain([end])
                .min()
                .unwrap_or(end);
            let wait = next
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100));
            match rx.recv_timeout(wait) {
                Ok((at, Incoming::LiveView(jpeg))) => {
                    if phase.frames.is_empty() {
                        phase.first_frame_file =
                            Some(save_new(&out.join(format!("liveview-{n}.jpg")), &jpeg));
                    }
                    phase.frames.push(at);
                    phase.sizes.push(jpeg.len());
                }
                Ok((at, Incoming::Event(Event::LiveViewStopped { error }))) => {
                    stopped_at.get_or_insert(at);
                    phase.stopped.push(json!({
                        "msSincePhase": ms(at.duration_since(started)),
                        "error": serde_json::to_value(&error).unwrap(),
                    }));
                }
                Ok((at, Incoming::Event(Event::Captured { captured }))) => {
                    photo(&mut photos, &fired, &captured, at, &name);
                }
                Ok((_, Incoming::Event(e))) => {
                    phase.stopped.push(json!({ "event": format!("{e:?}") }))
                }
                Ok((_, Incoming::Exited(log))) => {
                    phase.stopped.push(json!({ "helperExited": log }));
                    break;
                }
                Err(_) => {}
            }
        }
        phases.push(phase.report(&name, started));
    }
    let _ = client.request(Request::LiveView { on: false }, long);
    // Photos still on their way: waited for until every shot has one (up
    // to 10 s), then 2 s more for their companions (the JPEG of a
    // RAW+JPEG); never dropped.
    let late = Instant::now() + Duration::from_secs(10);
    let mut companions = None;
    loop {
        let answered = |photos: &[Value]| {
            fired
                .keys()
                .all(|r| photos.iter().any(|p| p["request"].as_u64() == Some(*r)))
        };
        if companions.is_none() && answered(&photos) {
            companions = Some(Instant::now() + Duration::from_secs(2));
        }
        let until = companions.unwrap_or(late).min(late);
        if Instant::now() >= until {
            break;
        }
        if let Ok((at, Incoming::Event(Event::Captured { captured }))) =
            rx.recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            photo(&mut photos, &fired, &captured, at, "after the phases");
        }
    }
    let _ = client.request(Request::Disconnect, long);
    report["phases"] = json!(phases);
    report["photos"] = json!(photos);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}
