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
use rawtonic_tether::camera::Driver;
use rawtonic_tether::client::{Client, Incoming};
use rawtonic_tether::mock::{MockDriver, MockSpec};
use rawtonic_tether::protocol::{Event, Reply, Request};
use rawtonic_tether::server::Server;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct StderrLog(log::Level);

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.0
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {} {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

fn driver(mock: bool) -> Result<Box<dyn Driver>, String> {
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
            .map(|d| Box::new(d) as Box<dyn Driver>)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux"))))]
    Ok(Box::new(NoCameras))
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
    // RAWTONIC_TETHER_LOG=debug also logs libgphoto2's own messages (what a
    // body answers, step by step) — for diagnosing a camera.
    let debug = std::env::var("RAWTONIC_TETHER_LOG").is_ok_and(|v| v == "debug");
    let level = if debug {
        log::Level::Debug
    } else {
        log::Level::Info
    };
    let logger: &'static StderrLog = Box::leak(Box::new(StderrLog(level)));
    let _ = log::set_logger(logger).map(|()| log::set_max_level(level.to_level_filter()));
    #[cfg(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux")))]
    let _gphoto_log = debug
        .then(|| gphoto2::log_bridge::forward_to_log(gphoto2::log_bridge::LogLevel::Debug).ok())
        .flatten();
    let args: Vec<String> = std::env::args().skip(1).collect();
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

/// The command line's work; the process's exit code.
fn run(args: &[String]) -> i32 {
    let mock = args.iter().any(|a| a == "--mock");
    let result = match args.first().map(String::as_str) {
        Some("list") => driver(mock).and_then(|mut d| {
            let cameras = d.list().map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&cameras).unwrap());
            Ok(())
        }),
        Some("probe") => probe(args, mock),
        Some("version") => version(),
        _ => driver(mock).and_then(|d| {
            // Tests shorten how long a shot may take.
            let timeout = std::env::var("RAWTONIC_TETHER_SHOT_TIMEOUT_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map(Duration::from_millis)
                .unwrap_or(rawtonic_tether::server::SHOT_TIMEOUT);
            Server::new(d, std::io::stdout().lock())
                .with_shot_timeout(timeout)
                .run(std::io::stdin())
                .map_err(|e| e.to_string())
        }),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rawtonic-tether: {e}");
            1
        }
    }
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
        let deadline = Instant::now() + Duration::from_secs(20);
        while got < frames && Instant::now() < deadline {
            if let Ok((at, Incoming::LiveView(jpeg))) = rx.recv_timeout(Duration::from_secs(2)) {
                first.get_or_insert(at);
                got += 1;
                sizes.push(jpeg.len());
                if got == 1 {
                    first_frame = Some(save_new(&out.join("liveview-first.jpg"), &jpeg));
                }
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
        });
    }
    let _ = client.request(Request::Disconnect, long);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}
