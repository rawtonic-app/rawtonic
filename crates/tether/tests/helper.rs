//! The real helper process with the scripted mock camera, driven through
//! the client as the app drives it.
use rawtonic_tether::client::{Client, Incoming};
use rawtonic_tether::mock::MockSpec;
use rawtonic_tether::protocol::{Captured, ErrorKind, Event, Reply, Request};
use rawtonic_tether::staging;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_rawtonic-tether");
const WAIT: Duration = Duration::from_secs(10);

struct Helper {
    client: Client,
    incoming: mpsc::Receiver<Incoming>,
    staging: PathBuf,
}

fn start_with(spec: &MockSpec, session: &Path, shot_timeout_ms: u64) -> Helper {
    let (tx, rx) = mpsc::channel();
    let envs: Vec<(OsString, OsString)> = vec![
        (
            "RAWTONIC_TETHER_MOCK".into(),
            serde_json::to_string(spec).unwrap().into(),
        ),
        (
            "RAWTONIC_TETHER_SHOT_TIMEOUT_MS".into(),
            shot_timeout_ms.to_string().into(),
        ),
    ];
    let (client, library) = Client::spawn(Path::new(HELPER), &["--mock".into()], &envs, move |i| {
        let _ = tx.send(i);
    })
    .unwrap();
    assert_eq!(library, None);
    Helper {
        client,
        incoming: rx,
        staging: staging::root(session),
    }
}

fn start(spec: &MockSpec, session: &Path) -> Helper {
    start_with(spec, session, 60_000)
}

impl Helper {
    fn connect(&self) -> Result<Reply, rawtonic_tether::protocol::TetherError> {
        self.client.request(
            Request::Connect {
                port: None,
                staging: self.staging.clone(),
                serial: None,
            },
            WAIT,
        )
    }

    fn capture(&self) {
        assert_eq!(
            self.client.request(Request::Capture, WAIT).unwrap(),
            Reply::Ok
        );
    }

    /// Events until `n` photos arrived (or `WAIT` passed, or the helper
    /// ended: then `other` ends with `None`).
    fn photos(&self, n: usize) -> (Vec<Captured>, Vec<Option<Event>>) {
        let mut photos = Vec::new();
        let mut other = Vec::new();
        let deadline = Instant::now() + WAIT;
        while photos.len() < n {
            match self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Incoming::Event(Event::Captured { captured })) => photos.push(captured),
                Ok(Incoming::Event(event)) => other.push(Some(event)),
                Ok(Incoming::LiveView(_)) => {}
                Ok(Incoming::Exited(_)) => {
                    other.push(None);
                    break;
                }
                Err(_) => break,
            }
        }
        (photos, other)
    }

    /// The next event that is not a photo or a frame.
    fn next_event(&self, within: Duration) -> Option<Event> {
        let deadline = Instant::now() + within;
        loop {
            match self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Incoming::Event(Event::Captured { .. })) | Ok(Incoming::LiveView(_)) => {}
                Ok(Incoming::Event(event)) => return Some(event),
                Ok(Incoming::Exited(_)) | Err(_) => return None,
            }
        }
    }

    fn exited(&self) -> Option<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            match self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Incoming::Exited(log)) => return Some(log),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

#[test]
fn every_shot_arrives_once_under_the_cameras_own_name_even_when_numbering_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        restart_numbering_every: Some(3),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    assert!(matches!(helper.connect().unwrap(), Reply::Connected { .. }));
    for _ in 0..7 {
        helper.capture();
    }
    let (photos, _) = helper.photos(7);
    assert_eq!(photos.len(), 7);
    let names: Vec<&str> = photos.iter().map(|p| p.camera_name.as_str()).collect();
    assert_eq!(
        names,
        [
            "DSC00001.ARW",
            "DSC00002.ARW",
            "DSC00003.ARW",
            "DSC00001.ARW",
            "DSC00002.ARW",
            "DSC00003.ARW",
            "DSC00001.ARW"
        ]
    );
    let paths: HashSet<&PathBuf> = photos.iter().map(|p| &p.path).collect();
    assert_eq!(paths.len(), 7, "each photo in its own file");
    let contents: HashSet<Vec<u8>> = photos
        .iter()
        .map(|p| std::fs::read(&p.path).unwrap())
        .collect();
    assert_eq!(contents.len(), 7, "no photo replaced another");
    for p in &photos {
        assert!(p.requested);
        assert_eq!(p.path.file_name().unwrap().to_str().unwrap(), p.camera_name);
        assert_eq!(std::fs::metadata(&p.path).unwrap().len(), p.size);
    }
}

#[test]
fn a_photo_the_camera_reports_twice_arrives_once() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        report_twice: true,
        restart_numbering_every: Some(2),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    // Back to back: a repeat must not take the next shot's place.
    for _ in 0..4 {
        helper.capture();
    }
    let (photos, _) = helper.photos(5);
    let names: Vec<&str> = photos.iter().map(|p| p.camera_name.as_str()).collect();
    // The restarted numbering still brings its shots; the repeats do not.
    assert_eq!(
        names,
        [
            "DSC00001.ARW",
            "DSC00002.ARW",
            "DSC00001.ARW",
            "DSC00002.ARW"
        ]
    );
    assert!(photos.iter().all(|p| p.requested));
}

#[test]
fn a_full_transfer_queue_fires_again_once_photos_are_released() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        queue_limit: Some(2),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    helper.capture();
    let (photos, _) = helper.photos(2);
    let full = helper.client.request(Request::Capture, WAIT).unwrap_err();
    assert_eq!(full.kind, ErrorKind::Other);
    let release = Request::Release {
        connection: photos[0].connection,
        download: photos[0].download,
        camera_folder: photos[0].camera_folder.clone(),
        camera_name: photos[0].camera_name.clone(),
    };
    assert_eq!(helper.client.request(release, WAIT).unwrap(), Reply::Ok);
    helper.capture();
    assert_eq!(helper.photos(1).0.len(), 1);
}

#[test]
fn a_shot_refused_as_busy_right_after_the_connect_is_taken_once_the_camera_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        busy_for_ms: 400,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    assert_eq!(helper.photos(1).0.len(), 1);
}

#[test]
fn a_camera_lost_during_a_request_is_reported_like_one_lost_between_them() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        unplug_after: Some(1),
        quiet_unplug: true,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    assert_eq!(helper.photos(1).0.len(), 1);
    let error = helper.client.request(Request::Settings, WAIT).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Disconnected);
    match helper.next_event(WAIT) {
        Some(Event::Disconnected { error }) => assert_eq!(error.kind, ErrorKind::Disconnected),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_release_from_an_earlier_connection_does_not_reach_the_camera_now_open() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        queue_limit: Some(1),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    let first = helper.photos(1).0.remove(0);
    // Connected again (another camera, or the same one replugged).
    helper.connect().unwrap();
    helper.capture();
    let second = helper.photos(1).0.remove(0);
    assert_ne!(first.connection, second.connection);
    let release = |p: &Captured| Request::Release {
        connection: p.connection,
        download: p.download,
        camera_folder: p.camera_folder.clone(),
        camera_name: p.camera_name.clone(),
    };
    // Same camera path, earlier connection: the queue stays full.
    assert_eq!(first.camera_name, second.camera_name);
    assert_eq!(
        helper.client.request(release(&first), WAIT).unwrap(),
        Reply::Ok
    );
    assert!(helper.client.request(Request::Capture, WAIT).is_err());
    assert_eq!(
        helper.client.request(release(&second), WAIT).unwrap(),
        Reply::Ok
    );
    helper.capture();
    assert_eq!(helper.photos(1).0.len(), 1);
}

#[test]
fn another_body_of_the_model_is_closed_before_anything_of_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        body_shots_ms: vec![0],
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    let error = helper
        .client
        .request(
            Request::Connect {
                port: None,
                staging: helper.staging.clone(),
                serial: Some("SOMEONE ELSE".into()),
            },
            WAIT,
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    std::thread::sleep(Duration::from_millis(300));
    let incoming = helper.staging.join(staging::INCOMING);
    assert!(
        !incoming.exists() || std::fs::read_dir(&incoming).unwrap().next().is_none(),
        "nothing of it downloaded"
    );
}

#[test]
fn back_to_back_photos_under_one_reused_name_all_arrive() {
    let dir = tempfile::tempdir().unwrap();
    // Every photo is called DSC00001.ARW.
    let spec = MockSpec {
        restart_numbering_every: Some(1),
        report_twice: true,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    for _ in 0..3 {
        helper.capture();
    }
    let (photos, _) = helper.photos(3);
    assert_eq!(photos.len(), 3, "new photos, not repeats");
    assert!(photos.iter().all(|p| p.camera_name == "DSC00001.ARW"));
    let contents: HashSet<Vec<u8>> = photos
        .iter()
        .map(|p| std::fs::read(&p.path).unwrap())
        .collect();
    assert_eq!(contents.len(), 3);
}

#[test]
fn a_late_release_never_frees_a_newer_photo_under_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        restart_numbering_every: Some(1),
        queue_limit: Some(2),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    helper.capture();
    let photos = helper.photos(2).0;
    assert_eq!(photos[0].camera_name, photos[1].camera_name);
    let release = |p: &Captured| Request::Release {
        connection: p.connection,
        download: p.download,
        camera_folder: p.camera_folder.clone(),
        camera_name: p.camera_name.clone(),
    };
    // The first one's release comes after the second photo took its name.
    assert_eq!(
        helper.client.request(release(&photos[0]), WAIT).unwrap(),
        Reply::Ok
    );
    assert!(
        helper.client.request(Request::Capture, WAIT).is_err(),
        "the newer photo was not freed"
    );
    assert_eq!(
        helper.client.request(release(&photos[1]), WAIT).unwrap(),
        Reply::Ok
    );
    helper.capture();
}

#[test]
fn shots_fired_on_the_body_are_downloaded_too() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        body_shots_ms: vec![50, 120],
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    let (photos, _) = helper.photos(2);
    assert_eq!(photos.len(), 2);
    assert!(
        photos
            .iter()
            .all(|p| !p.requested && p.ms_to_event.is_none())
    );
}

#[test]
fn a_camera_without_a_separate_trigger_is_captured_the_blocking_way() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        no_trigger: true,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    helper.capture();
    let (photos, _) = helper.photos(2);
    assert_eq!(photos.len(), 2);
    assert!(photos.iter().all(|p| p.requested));
}

#[test]
fn a_late_photo_is_not_fired_again_and_a_lost_one_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        lost_shots: vec![2],
        late_shots: vec![1],
        late_ms: 600,
        ..MockSpec::default()
    };
    let helper = start_with(&spec, dir.path(), 300);
    helper.connect().unwrap();
    helper.capture();
    helper.capture();
    // Shot 1 comes after the shot timeout: both requests are reported as
    // failed, the late file still arrives (as a body shot then), and
    // nothing is fired a second time: one photo for two requests, since
    // shot 2 was lost.
    let mut failed = 0;
    let mut photos = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match helper.incoming.recv_timeout(Duration::from_millis(100)) {
            Ok(Incoming::Event(Event::CaptureFailed { error })) => {
                assert_eq!(error.kind, ErrorKind::Timeout);
                failed += 1;
            }
            Ok(Incoming::Event(Event::Captured { captured })) => photos.push(captured),
            _ => {}
        }
    }
    assert_eq!(failed, 2);
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].camera_name, "DSC00001.ARW");
}

#[test]
fn a_camera_another_program_holds_is_reported_as_busy() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        busy: true,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    assert_eq!(helper.connect().unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(
        helper
            .client
            .request(Request::Capture, WAIT)
            .unwrap_err()
            .kind,
        ErrorKind::NotConnected
    );
}

#[test]
fn settings_fixed_on_the_body_are_shown_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        read_only: vec!["aperture".into()],
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    let Reply::Settings { settings } = helper.client.request(Request::Settings, WAIT).unwrap()
    else {
        panic!()
    };
    assert!(settings.iter().any(|s| s.key == "aperture" && s.read_only));
    let set = |key: &str, value: &str| {
        helper.client.request(
            Request::SetSetting {
                key: key.into(),
                value: value.into(),
            },
            WAIT,
        )
    };
    assert_eq!(
        set("aperture", "f/4").unwrap_err().kind,
        ErrorKind::ReadOnly
    );
    assert_eq!(set("iso", "12").unwrap_err().kind, ErrorKind::BadValue);
    assert_eq!(set("iso", "400").unwrap(), Reply::Ok);
}

#[test]
fn an_unplugged_camera_is_reported_and_closed() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        unplug_after: Some(1),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    let (photos, _) = helper.photos(1);
    assert_eq!(photos.len(), 1);
    match helper.next_event(WAIT) {
        Some(Event::Disconnected { error }) => assert_eq!(error.kind, ErrorKind::Disconnected),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        helper
            .client
            .request(Request::Capture, WAIT)
            .unwrap_err()
            .kind,
        ErrorKind::NotConnected
    );
}

#[test]
fn a_crash_mid_download_leaves_no_file_to_ingest_and_keeps_the_part() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        abort_mid_download: Some(2),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    helper.capture();
    let (photos, other) = helper.photos(2);
    assert_eq!(photos.len(), 1);
    assert_eq!(other.last(), Some(&None), "the helper ended");
    assert_eq!(
        helper
            .client
            .request(Request::Capture, WAIT)
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    let recovered = staging::recover(&helper.staging).unwrap();
    assert_eq!(recovered.complete, vec![photos[0].path.clone()]);
    assert_eq!(recovered.incomplete.len(), 1);
    assert!(recovered.incomplete[0].starts_with(helper.staging.join(staging::INCOMPLETE)));
}

#[test]
fn a_crash_before_the_app_heard_of_a_photo_leaves_it_to_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let spec = MockSpec {
        abort_before_event: Some(1),
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    helper.capture();
    let (photos, other) = helper.photos(1);
    assert!(photos.is_empty());
    assert_eq!(other.last(), Some(&None), "the helper ended");
    let recovered = staging::recover(&helper.staging).unwrap();
    assert_eq!(recovered.complete.len(), 1);
    assert_eq!(recovered.complete[0].file_name().unwrap(), "DSC00001.ARW");
    assert!(recovered.incomplete.is_empty());
}

#[test]
fn a_killed_helper_fails_waiting_requests_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let helper = start(&MockSpec::default(), dir.path());
    helper.connect().unwrap();
    helper.client.kill();
    assert!(helper.exited().is_some());
    let started = Instant::now();
    let error = helper.client.request(Request::Settings, WAIT).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Disconnected);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn live_view_frames_arrive_while_on() {
    let dir = tempfile::tempdir().unwrap();
    let helper = start(&MockSpec::default(), dir.path());
    helper.connect().unwrap();
    helper
        .client
        .request(Request::LiveView { on: true }, WAIT)
        .unwrap();
    let mut frames = 0;
    let deadline = Instant::now() + WAIT;
    while frames < 3 && Instant::now() < deadline {
        if let Ok(Incoming::LiveView(jpeg)) = helper.incoming.recv_timeout(WAIT) {
            assert_eq!(&jpeg[..2], [0xff, 0xd8]);
            frames += 1;
        }
    }
    assert_eq!(frames, 3);
    helper
        .client
        .request(Request::LiveView { on: false }, WAIT)
        .unwrap();
    // Without live view a camera that has none refuses it.
    let none = start(
        &MockSpec {
            preview: false,
            ..MockSpec::default()
        },
        dir.path(),
    );
    none.connect().unwrap();
    assert_eq!(
        none.client
            .request(Request::LiveView { on: true }, WAIT)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
}

#[test]
fn the_probe_never_writes_over_a_file_in_its_output_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mine = dir.path().join("liveview-first.jpg");
    std::fs::write(&mine, b"mine").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rawtonic-tether"))
        .args([
            "probe",
            "--mock",
            "--shots",
            "0",
            "--live-view-frames",
            "2",
            "--out",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["liveView"]["firstFrameFile"]
            .as_str()
            .unwrap()
            .starts_with("not saved"),
        "{report}"
    );
    assert_eq!(std::fs::read(&mine).unwrap(), b"mine");
}

#[test]
fn a_short_request_behind_a_long_capture_is_not_taken_for_a_hung_helper() {
    let dir = tempfile::tempdir().unwrap();
    // A camera that captures the blocking way, taking 1.5 s per photo.
    let spec = MockSpec {
        no_trigger: true,
        event_delay_ms: 1500,
        ..MockSpec::default()
    };
    let helper = start(&spec, dir.path());
    helper.connect().unwrap();
    let client = std::sync::Arc::new(helper.client);
    let capturing = client.clone();
    let capture = std::thread::spawn(move || capturing.request(Request::Capture, WAIT));
    std::thread::sleep(Duration::from_millis(100));
    // Its own answer takes milliseconds; it waits for the capture first.
    let settings = client.request(Request::Settings, Duration::from_millis(1000));
    assert!(
        matches!(settings, Ok(Reply::Settings { .. })),
        "{settings:?}"
    );
    assert_eq!(capture.join().unwrap().unwrap(), Reply::Ok);
    assert!(client.alive());
}
