//! Hot folder for sessions (G47; Capture One's hot folder, Lightroom's Auto
//! Import): photos that appear in a watched folder — written by tethering
//! software, a card reader or another app — are copied into the session's
//! Capture folder through the verified ingest once they are complete, and
//! get the next-capture adjustments, stored with the photograph in one
//! transaction. While a hot folder runs, its captures form an import batch
//! (Previous Import). The watched files are never moved or deleted;
//! verified duplicates are skipped.
use crate::formats::is_supported_image_file;
use rawtonic_catalog::{Contents, ImportKind, Library};
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum NextCapture {
    /// The defaults, like an ordinary ingest.
    Defaults,
    /// The adjustments of the photo captured before, as it is edited now.
    CopyFromLast,
    /// A preset's adjustments.
    Preset { name: String, adjustments: Value },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotFolderStatus {
    pub folder: PathBuf,
    pub session: PathBuf,
    pub imported: usize,
    pub next_capture: NextCapture,
}

struct Watch {
    stop: Arc<AtomicBool>,
    imported: Arc<AtomicUsize>,
    folder: PathBuf,
    session: PathBuf,
    next_capture: NextCapture,
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

/// The editor state a new capture gets, or `None` for the defaults.
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
    Some(adjustments)
}

/// What a new capture is stored with: its next-capture adjustments.
fn next_contents(lib: &Library, next: &NextCapture, previous: Option<&str>) -> Option<Contents> {
    let adjustments = next_state(lib, next, previous)?;
    Some(Contents {
        editor_state: Some(json!({
            "version": crate::image_processing::ImageMetadata::default().version,
            "rating": 0,
            "adjustments": adjustments,
        })),
        ..Contents::default()
    })
}

/// Ingests one complete file with its next-capture adjustments (one
/// transaction: a capture is never registered without them). `earlier` is
/// the copy an earlier try left unrecorded; it is recorded instead of
/// copying again when it still holds the file. Returns the asset id of a
/// new photo.
#[allow(clippy::too_many_arguments)]
pub fn ingest_capture(
    lib: &Library,
    source: &Path,
    template: &str,
    sequence: u64,
    next: &NextCapture,
    previous: Option<&str>,
    earlier: Option<&Path>,
    cancel: &AtomicBool,
) -> (Imported, Option<String>) {
    let contents = next_contents(lib, next, previous);
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
        // already holds the adjustments.
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

#[tauri::command]
pub fn hot_folder_start(
    directory: String,
    folder: String,
    template: String,
    next_capture: NextCapture,
    app: AppHandle,
) -> Result<HotFolderStatus, String> {
    let (session, folder) = check_folder(Path::new(&directory), Path::new(&folder))?;
    rawtonic_naming::render(&template, &rawtonic_naming::Context::new("A.ARW", 1))
        .map_err(|e| format!("Filename template: {e}"))?;
    hot_folder_stop();
    let stop = Arc::new(AtomicBool::new(false));
    let imported = Arc::new(AtomicUsize::new(0));
    {
        let (stop, imported, folder, session, next) = (
            stop.clone(),
            imported.clone(),
            folder.clone(),
            session.clone(),
            next_capture.clone(),
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
                let mut hot = HotFolder::new(&folder, DEFAULT_SETTLE);
                let mut previous: Option<String> = None;
                // Copies made but not recorded, by source, for the retry.
                let mut unrecorded: HashMap<PathBuf, PathBuf> = HashMap::new();
                while !stop.load(Ordering::Acquire) {
                    unrecorded.retain(|source, _| source.exists());
                    for source in hot.poll(Instant::now(), |p: &Path| is_supported_image_file(p)) {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let sequence = lib.summary().map(|s| s.assets as u64 + 1).unwrap_or(1);
                        let (result, asset) = ingest_capture(
                            &lib,
                            &source,
                            &template,
                            sequence,
                            &next,
                            previous.as_deref(),
                            unrecorded.get(&source).map(PathBuf::as_path),
                            &stop,
                        );
                        match (&asset, &result.destination) {
                            (None, Some(copy)) if result.error.is_some() && !result.skipped => {
                                unrecorded.insert(source.clone(), copy.clone());
                            }
                            _ => {
                                unrecorded.remove(&source);
                            }
                        }
                        let recorded = asset.is_some();
                        if recorded {
                            previous = asset;
                            imported.fetch_add(1, Ordering::AcqRel);
                        }
                        // Not in the session (e.g. it was not writable, or
                        // the copy could not be recorded): the file is tried
                        // again later (a copy left unrecorded is recorded
                        // then, not copied again), and its error shown once
                        // until it succeeds or changes.
                        if result.error.is_some()
                            && !recorded
                            && !hot.failed(&source, Instant::now())
                        {
                            log::debug!("Hot folder: {} failed again", source.display());
                            continue;
                        }
                        if let Some(error) = &result.error {
                            log::warn!("Hot folder: {}: {error}", source.display());
                        }
                        let _ = app.emit("hot-folder-imported", &result);
                    }
                    std::thread::sleep(POLL);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    let status = HotFolderStatus {
        folder: folder.clone(),
        session: session.clone(),
        imported: 0,
        next_capture: next_capture.clone(),
    };
    *WATCH.lock().map_err(|e| e.to_string())? = Some(Watch {
        stop,
        imported,
        folder,
        session,
        next_capture,
    });
    Ok(status)
}

#[tauri::command]
pub fn hot_folder_stop() {
    if let Ok(mut watch) = WATCH.lock()
        && let Some(watch) = watch.take()
    {
        watch.stop.store(true, Ordering::Release);
    }
}

#[tauri::command]
pub fn hot_folder_status() -> Option<HotFolderStatus> {
    let watch = WATCH.lock().ok()?;
    let watch = watch.as_ref()?;
    Some(HotFolderStatus {
        folder: watch.folder.clone(),
        session: watch.session.clone(),
        imported: watch.imported.load(Ordering::Acquire),
        next_capture: watch.next_capture.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use std::fs;

    fn session(root: &Path) -> Library {
        Library::create(&root.join("Job"), "Job", Mode::Session).unwrap()
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
            &next,
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
            &next,
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
            &next,
            Some(&b),
            None,
            &cancel,
        );
        assert!(again.skipped && none.is_none());
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
            &preset,
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
            &NextCapture::Defaults,
            None,
            None,
            &cancel,
        );
        assert!(lib.editor_state(&b.unwrap()).unwrap().is_none());
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
            &preset,
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
            &NextCapture::Defaults,
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

    /// The next-capture adjustments go into the library with the capture:
    /// when that write fails the capture is not registered at all (its
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
            let preset = NextCapture::Preset {
                name: "Studio".into(),
                adjustments: json!({"contrast": 12}),
            };
            let cancel = AtomicBool::new(false);
            lib.inject_write_failure(table).unwrap();
            let (failed, none) =
                ingest_capture(&lib, &source, "{orig}", 1, &preset, None, None, &cancel);
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
                &preset,
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
                &NextCapture::Defaults,
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
