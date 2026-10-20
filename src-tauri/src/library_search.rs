//! Library search and smart albums (G36/G37): Tauri glue over
//! `rawtonic_catalog::query`. Each command opens the library in a blocking
//! worker (`session_bridge::with_library`); nothing here reads photographs
//! except the capture-facts pass, which reads each one's header (at most
//! its first 16 MiB) once.
use crate::session_bridge::with_library;
use rawtonic_catalog::{Collection, Field, Library, Rule, query::FacetValue};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Emitter;

/// Paths of the photographs `rule` matches, in path order.
#[tauri::command]
pub async fn session_search(directory: String, rule: Rule) -> Result<Vec<PathBuf>, String> {
    with_library(directory, false, move |lib| {
        start_facts_pass_if_needed(lib);
        lib.search(&rule).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One value list the Library filter shows: the values of `field` among
/// the photographs `rule` matches.
#[derive(Debug, Deserialize)]
pub struct FacetRequest {
    field: Field,
    rule: Rule,
}

/// The value lists asked for, each with its own rule (a column's counts
/// leave out its own selection, so choosing another value of it shows what
/// that adds).
#[tauri::command]
pub async fn session_facets(
    directory: String,
    requests: Vec<FacetRequest>,
) -> Result<BTreeMap<Field, Vec<FacetValue>>, String> {
    with_library(directory, false, move |lib| {
        start_facts_pass_if_needed(lib);
        // Lists asked with the same rule (all of them while nothing is
        // chosen) are counted in one scan.
        let mut by_rule: Vec<(Rule, Vec<Field>)> = Vec::new();
        for request in requests {
            match by_rule.iter_mut().find(|(rule, _)| *rule == request.rule) {
                Some((_, fields)) => fields.push(request.field),
                None => by_rule.push((request.rule, vec![request.field])),
            }
        }
        let mut result = BTreeMap::new();
        for (rule, fields) in by_rule {
            result.extend(lib.facets(&rule, &fields).map_err(|e| e.to_string())?);
        }
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A new smart album with `rules` in `parent` (a group) or at the top level.
#[tauri::command]
pub async fn session_create_smart_album(
    directory: String,
    name: String,
    parent: Option<String>,
    rules: Rule,
) -> Result<Collection, String> {
    with_library(directory, true, move |lib| {
        lib.create_smart_collection(&name, parent.as_deref(), &rules)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Replaces a smart album's rules.
#[tauri::command]
pub async fn session_set_smart_album_rules(
    directory: String,
    id: String,
    rules: Rule,
) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        lib.set_collection_rules(&id, &rules)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The running capture-facts pass: its library and its stop switch.
static FACTS_PASS: Mutex<Option<(PathBuf, Arc<AtomicBool>)>> = Mutex::new(None);

/// Where the pass announces facts it recorded (the app: `FACTS_RECORDED`).
static FACTS_ANNOUNCED: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Emitted with the library's directory after the pass recorded facts, so
/// the Library searches, counts its lists and smart albums again.
pub const FACTS_RECORDED: &str = "library-facts-recorded";

/// The app hears of facts the background pass records from now on.
pub fn announce_facts_to(app: tauri::AppHandle) {
    let _ = FACTS_ANNOUNCED.set(app);
}

/// How long a pass waits before reading files (opening the library and its
/// first previews come first), and between two files.
const PASS_DELAY: Duration = Duration::from_secs(3);
const PASS_PAUSE: Duration = Duration::from_millis(2);
/// Photographs read between two announcements.
const PASS_BATCH: usize = 256;

/// Reads the capture facts of every photograph of the library at
/// `directory` not read yet (`rawtonic_ingest::capture::capture_facts`, the
/// header of each file, once), `PASS_BATCH` at a time, calling `recorded`
/// after each batch that recorded any, until done or `cancel` is set.
/// Returns how many were recorded.
fn read_facts(
    directory: &Path,
    pause: Duration,
    cancel: &AtomicBool,
    recorded: &dyn Fn(usize),
) -> anyhow::Result<usize> {
    let mut cursor = 0;
    let mut total = 0;
    while !cancel.load(Ordering::Acquire) {
        let lib = Library::open(directory)?;
        let (looked, done) = lib.fill_facts(
            &mut cursor,
            PASS_BATCH,
            pause,
            cancel,
            &rawtonic_ingest::capture::capture_facts,
        )?;
        if done > 0 {
            total += done;
            recorded(done);
        }
        if looked == 0 {
            break;
        }
    }
    Ok(total)
}

/// Starts `read_facts` for the library at `directory` in the background,
/// after a delay. A pass for another library is stopped; one already
/// running for this library goes on.
pub fn start_facts_pass(directory: PathBuf) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut running) = FACTS_PASS.lock() else {
            return;
        };
        if let Some((library, previous)) = running.as_ref() {
            if *library == directory && !previous.load(Ordering::Acquire) {
                return;
            }
            previous.store(true, Ordering::Release);
        }
        *running = Some((directory.clone(), cancel.clone()));
    }
    let spawned = std::thread::Builder::new()
        .name("capture-facts".into())
        .spawn(move || {
            let started = Instant::now();
            std::thread::sleep(PASS_DELAY);
            let announce = |_: usize| {
                if let Some(app) = FACTS_ANNOUNCED.get() {
                    let _ = app.emit(FACTS_RECORDED, &directory);
                }
            };
            match read_facts(&directory, PASS_PAUSE, &cancel, &announce) {
                Ok(0) => {}
                Ok(recorded) => log::info!(
                    "Read the capture facts of {recorded} photographs in {:.1} s",
                    started.elapsed().as_secs_f64()
                ),
                Err(e) => log::warn!("Capture facts paused: {e:#}"),
            }
            // Done (or stopped): a later start begins a new pass.
            cancel.store(true, Ordering::Release);
        });
    if let Err(e) = spawned {
        log::warn!("Capture facts not read: {e}");
    }
}

/// Starts the pass when photographs of `lib` were added without their
/// facts (by an edit, an import from another catalog, …) and none runs:
/// asked by every search and value list, so whatever registered them, the
/// Library's next look fills them in.
fn start_facts_pass_if_needed(lib: &Library) {
    if lib.facts_backlog().is_ok_and(|n| n > 0) {
        start_facts_pass(lib.directory.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use serde_json::json;
    use tauri::async_runtime::block_on;

    /// Tests that start the (one, process-wide) facts pass run one at a time.
    static PASS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn search_facets_and_smart_albums_reach_the_catalog() {
        let _one = PASS.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Search", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        for (name, rating) in [("A.ARW", 5), ("B.ARW", 2), ("C.JPG", 0)] {
            std::fs::write(capture.join(name), name).unwrap();
            let asset = lib.add_asset(&capture.join(name)).unwrap();
            let mut meta = lib.metadata(&asset.id).unwrap();
            meta.rating = rating;
            lib.set_metadata(&asset.id, &meta).unwrap();
        }
        let directory = lib.directory.to_string_lossy().into_owned();
        let rule: Rule =
            serde_json::from_value(json!({"field":"rating","op":"ge","value":2})).unwrap();
        let found = block_on(session_search(directory.clone(), rule.clone())).unwrap();
        assert_eq!(found, [capture.join("A.ARW"), capture.join("B.ARW")]);
        let facets = block_on(session_facets(
            directory.clone(),
            vec![
                FacetRequest {
                    field: Field::FileType,
                    rule: Rule::everything(),
                },
                FacetRequest {
                    field: Field::Rating,
                    rule: Rule::everything(),
                },
                FacetRequest {
                    field: Field::Camera,
                    rule: rule.clone(),
                },
            ],
        ))
        .unwrap();
        let types: Vec<_> = facets[&Field::FileType]
            .iter()
            .map(|v| (v.value.as_str(), v.count))
            .collect();
        assert_eq!(types, [("raw", 2), ("jpeg", 1)]);
        assert_eq!(facets[&Field::Rating].len(), 3);
        // Within its own rule: the two photographs rated 2 or more.
        assert_eq!(facets[&Field::Camera][0].count, 2);
        let smart = block_on(session_create_smart_album(
            directory.clone(),
            "Good".into(),
            None,
            rule,
        ))
        .unwrap();
        assert_eq!(smart.count, 2);
        let five: Rule =
            serde_json::from_value(json!({"field":"rating","op":"is","value":5})).unwrap();
        block_on(session_set_smart_album_rules(
            directory.clone(),
            smart.id.clone(),
            five,
        ))
        .unwrap();
        let paths = block_on(crate::session_bridge::session_album_paths(
            directory.clone(),
            smart.id,
        ))
        .unwrap();
        assert_eq!(paths, [capture.join("A.ARW")]);
        // Bad rules are refused with a reason, not run.
        let bad: Rule =
            serde_json::from_value(json!({"field":"rating","op":"ge","value":9})).unwrap();
        assert!(
            block_on(session_search(directory, bad))
                .unwrap_err()
                .contains("0–5")
        );
    }

    /// The pass reads every photograph not read yet and says so after each
    /// batch it recorded, so the Library searches again.
    #[test]
    fn the_facts_pass_announces_what_it_recorded() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Facts", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        for i in 0..(PASS_BATCH + 3) {
            let path = capture.join(format!("{i}.ARW"));
            std::fs::write(&path, b"no exif").unwrap();
            lib.add_asset(&path).unwrap();
        }
        let announced = std::sync::Mutex::new(Vec::new());
        let recorded = read_facts(
            &lib.directory,
            Duration::ZERO,
            &AtomicBool::new(false),
            &|n| announced.lock().unwrap().push(n),
        )
        .unwrap();
        assert_eq!(recorded, PASS_BATCH + 3);
        assert_eq!(*announced.lock().unwrap(), [PASS_BATCH, 3]);
        assert_eq!(lib.facts_backlog().unwrap(), 0);
        // Nothing left: nothing announced.
        read_facts(
            &lib.directory,
            Duration::ZERO,
            &AtomicBool::new(false),
            &|_| panic!("nothing to say"),
        )
        .unwrap();
    }

    /// A value list asked of a library with photographs not read yet starts
    /// the pass for it (an import after the startup pass finished).
    #[test]
    fn value_lists_start_the_facts_pass_for_unread_photographs() {
        let _one = PASS.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Later", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        lib.add_asset(&photo).unwrap();
        let directory = lib.directory.clone();
        block_on(session_facets(
            directory.to_string_lossy().into_owned(),
            vec![FacetRequest {
                field: Field::Camera,
                rule: Rule::everything(),
            }],
        ))
        .unwrap();
        let running = FACTS_PASS
            .lock()
            .unwrap()
            .as_ref()
            .map(|(dir, _)| dir.clone());
        assert_eq!(running, Some(directory));
    }

    /// The catalog's file types say RAW exactly where the grid does.
    #[test]
    fn file_types_agree_with_the_formats_the_app_reads() {
        use rawtonic_catalog::query::file_type;
        for (extension, _) in rawtonic_engine::formats::RAW_EXTENSIONS {
            assert!(matches!(file_type(extension), "raw" | "dng"), "{extension}");
        }
        for extension in rawtonic_engine::formats::NON_RAW_EXTENSIONS {
            assert!(
                !matches!(file_type(extension), "raw" | "dng"),
                "{extension}"
            );
        }
    }
}
