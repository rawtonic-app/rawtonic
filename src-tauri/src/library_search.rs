//! Library search and smart albums (G36/G37): Tauri glue over
//! `rawtonic_catalog::query`. Each command opens the library in a blocking
//! worker (`session_bridge::with_library`); nothing here reads photographs
//! except the capture-facts pass, which reads each one's header once.
use crate::session_bridge::with_library;
use rawtonic_catalog::{Collection, Field, Library, Rule, query::FacetValue};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

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

/// How long a pass waits before reading files (opening the library and its
/// first previews come first), and between two files.
const PASS_DELAY: Duration = Duration::from_secs(3);
const PASS_PAUSE: Duration = Duration::from_millis(2);

/// Starts reading the capture facts of every photograph of the library at
/// `directory` not read yet, in the background (the header of each file,
/// once; rawtonic_ingest::capture::capture_facts). A pass for another
/// library is stopped; one already running for this library goes on.
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
            let mut cursor = 0;
            let mut recorded = 0;
            while !cancel.load(Ordering::Acquire) {
                let Ok(lib) = Library::open(&directory) else {
                    break;
                };
                match lib.fill_facts(
                    &mut cursor,
                    256,
                    PASS_PAUSE,
                    &cancel,
                    &rawtonic_ingest::capture::capture_facts,
                ) {
                    Ok((0, _)) => break,
                    Ok((_, done)) => recorded += done,
                    Err(e) => {
                        log::warn!("Capture facts paused: {e:#}");
                        break;
                    }
                }
            }
            // Done (or stopped): a later start begins a new pass.
            cancel.store(true, Ordering::Release);
            if recorded > 0 {
                log::info!(
                    "Read the capture facts of {recorded} photographs in {:.1} s",
                    started.elapsed().as_secs_f64()
                );
            }
        });
    if let Err(e) = spawned {
        log::warn!("Capture facts not read: {e}");
    }
}

/// Starts the pass when photographs of `lib` were added without their
/// facts (by an edit, an import from another catalog, …) and none runs.
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

    #[test]
    fn search_facets_and_smart_albums_reach_the_catalog() {
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
}
