//! Assisted culling (schema 30, G35): misfire and document scores from the
//! content embeddings, scored once per file, model and prompt set; the
//! panel's suggestions, the photographer's dismissals, the search rules,
//! and Auto-Stack's similarity with capture-time fractions.
use rawtonic_catalog::content::DIM;
use rawtonic_catalog::cull::{DOCUMENT_THRESHOLD, MISFIRE_THRESHOLD, PromptSet};
use rawtonic_catalog::face_eyes::EyeFace;
use rawtonic_catalog::facts::CaptureFacts;
use rawtonic_catalog::query::Rule;
use rawtonic_catalog::stacks::{AutoStackOptions, Member};
use rawtonic_catalog::{Library, Mode};
use serde_json::json;
use std::{fs, path::Path, path::PathBuf};

const MODEL: &str = "vision-a";
const SCALE: f32 = 112.6689;
const BIAS: f32 = -16.771725;

/// A unit vector with `cos` to axis 0 and the rest along `axis`.
fn toward(cos: f32, axis: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[0] = cos;
    v[axis] = (1.0 - cos * cos).sqrt();
    v
}

fn axis(n: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[n] = 1.0;
    v
}

/// Misfire prompt along axis 0, document prompt along axis 1.
fn prompts(id: &str) -> PromptSet {
    PromptSet {
        id: id.into(),
        misfire: vec![axis(0), axis(5)],
        document: vec![axis(1)],
        logit_scale: SCALE,
        logit_bias: BIAS,
    }
}

/// The cosine whose tag score is `score`.
fn cos_of(score: f32) -> f32 {
    ((score / (1.0 - score)).ln() - BIAS) / SCALE
}

fn library(dir: &Path, names: &[&str]) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    let ids = names
        .iter()
        .map(|name| {
            fs::write(capture.join(name), name).unwrap();
            lib.add_asset(&capture.join(name)).unwrap().id
        })
        .collect();
    (lib, ids)
}

fn embed(lib: &Library, id: &str, values: &[f32]) {
    let fingerprint = lib.asset(id).unwrap().fingerprint;
    lib.record_content(id, MODEL, &fingerprint, Ok(values))
        .unwrap();
}

fn rule(value: serde_json::Value) -> Rule {
    serde_json::from_value(value).unwrap()
}

fn names(found: Vec<PathBuf>) -> Vec<String> {
    let mut names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_new_library_has_the_culling_tables() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path(), &[]);
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 30);
    for table in ["cull_scores", "cull_dismissed"] {
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table}");
    }
}

#[test]
fn an_older_library_takes_the_tables_and_reads_its_photographs_for_scores() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["a.jpg"]);
    embed(&lib, &ids[0], &toward(0.5, 3));
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0030.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    // Its embedding is kept, but scores come only from a fresh reading.
    assert_eq!(lib.content_backlog_count(MODEL).unwrap(), 0);
    assert_eq!(lib.cull_backlog_count(MODEL, "p1").unwrap(), 1);
    assert_eq!(lib.cull_backlog(MODEL, "p1", 10).unwrap()[0].id, ids[0]);
}

/// What the content pass does with a photograph it read: its embedding and
/// its scores from the same values.
fn read(lib: &Library, id: &str, set: &PromptSet, values: &[f32]) {
    embed(lib, id, values);
    let fingerprint = lib.asset(id).unwrap().fingerprint;
    lib.record_cull(id, MODEL, set, &fingerprint, values)
        .unwrap();
}

#[test]
fn scores_are_made_from_each_reading_once_per_file_model_and_prompt_set() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(
        d.path(),
        &["cap.jpg", "page.jpg", "people.jpg", "unreadable.jpg"],
    );
    let p1 = prompts("p1");
    // Every present photograph waits for a reading, newest first.
    assert_eq!(lib.cull_backlog_count(MODEL, "p1").unwrap(), 4);
    assert_eq!(lib.cull_backlog(MODEL, "p1", 2).unwrap()[0].id, ids[3]);
    let misfire = cos_of(MISFIRE_THRESHOLD * 2.0);
    let document = cos_of(DOCUMENT_THRESHOLD * 2.0);
    read(&lib, &ids[0], &p1, &toward(misfire, 7));
    let mut page = vec![0.0; DIM];
    page[1] = document;
    page[8] = (1.0 - document * document).sqrt();
    read(&lib, &ids[1], &p1, &page);
    read(&lib, &ids[2], &p1, &axis(9));
    // A photograph the model could not read is not asked for again.
    let fp = lib.asset(&ids[3]).unwrap().fingerprint;
    lib.record_content(&ids[3], MODEL, &fp, Err("unreadable"))
        .unwrap();
    assert_eq!(lib.cull_backlog_count(MODEL, "p1").unwrap(), 0);
    assert!(lib.has_cull(&ids[0], MODEL, "p1").unwrap());
    assert!(lib.has_cull(&ids[3], MODEL, "p1").unwrap());

    // Scores come from the values as read (exact), not the stored bytes.
    let (m, doc) = lib.cull_scores(&ids[0]).unwrap().unwrap();
    assert!((m / (MISFIRE_THRESHOLD * 2.0) - 1.0).abs() < 1e-3, "{m}");
    assert!(doc < DOCUMENT_THRESHOLD);
    let (m, doc) = lib.cull_scores(&ids[1]).unwrap().unwrap();
    assert!(
        m < MISFIRE_THRESHOLD && (doc / (DOCUMENT_THRESHOLD * 2.0) - 1.0).abs() < 1e-3,
        "{m} {doc}"
    );
    assert!(lib.cull_scores(&ids[3]).unwrap().is_none());

    // Another prompt set, or another model, reads all but the failed one
    // again.
    assert_eq!(lib.cull_backlog_count(MODEL, "p2").unwrap(), 3);
    assert_eq!(lib.cull_backlog_count("vision-b", "p1").unwrap(), 4);
    // A changed file has no current score until it is read again.
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute(
        "UPDATE assets SET fingerprint='xxh3:changed' WHERE id=?",
        [&ids[0]],
    )
    .unwrap();
    assert!(lib.cull_scores(&ids[0]).unwrap().is_none());
    assert_eq!(lib.cull_backlog(MODEL, "p1", 10).unwrap()[0].id, ids[0]);
    assert!(!lib.has_cull(&ids[0], MODEL, "p1").unwrap());

    // A photograph whose embedding is kept but could not be read again for
    // its scores keeps the embedding and is not asked for again.
    db.execute(
        "UPDATE assets SET fingerprint='xxh3:changed2' WHERE id=?",
        [&ids[2]],
    )
    .unwrap();
    embed(&lib, &ids[2], &axis(9));
    assert!(
        lib.cull_backlog(MODEL, "p1", 10)
            .unwrap()
            .iter()
            .any(|w| w.id == ids[2])
    );
    assert!(lib.has_embedding(&ids[2], MODEL).unwrap());
    lib.record_cull_failure(&ids[2], MODEL, "p1", "xxh3:changed2", "decode failed")
        .unwrap();
    assert!(lib.has_embedding(&ids[2], MODEL).unwrap());
    assert!(lib.has_cull(&ids[2], MODEL, "p1").unwrap());
    assert!(lib.cull_scores(&ids[2]).unwrap().is_none());
    assert!(!lib.cull_photos(&ids[2..3]).unwrap()[&ids[2]].scored);
    assert!(
        lib.cull_backlog(MODEL, "p1", 10)
            .unwrap()
            .iter()
            .all(|w| w.id != ids[2])
    );
    // A later good reading replaces the failure.
    lib.record_cull(&ids[2], MODEL, &p1, "xxh3:changed2", &axis(9))
        .unwrap();
    assert!(lib.cull_scores(&ids[2]).unwrap().is_some());
    assert!(!lib.has_embedding(&ids[3], MODEL).unwrap());

    // A bad prompt set or embedding is refused, nothing written.
    let mut bad = prompts("p3");
    bad.document = vec![vec![1.0; 3]];
    assert!(
        lib.record_cull(&ids[2], MODEL, &bad, &fp, &axis(9))
            .is_err()
    );
    assert!(
        lib.record_cull(&ids[2], MODEL, &p1, &fp, &[0.5; 3])
            .is_err()
    );
}

#[test]
fn suggestions_dismissals_and_search_rules() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(
        d.path(),
        &["cap.jpg", "page.jpg", "people.jpg", "blink.jpg"],
    );
    let p1 = prompts("p1");
    read(&lib, &ids[0], &p1, &toward(cos_of(0.05), 7));
    read(&lib, &ids[1], &p1, &{
        let mut v = toward(cos_of(0.05), 1);
        // Also very much a document.
        v[1] = 0.6;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect::<Vec<_>>()
    });
    read(&lib, &ids[2], &p1, &axis(9));
    // Closed eyes on blink.jpg, read by the eye check.
    let fp = lib.asset(&ids[3]).unwrap().fingerprint;
    lib.record_eyes(
        &ids[3],
        1,
        &fp,
        Ok(&[EyeFace {
            bbox: [0.4, 0.2, 0.1, 0.15],
            state: "closed".into(),
            ear: Some(0.1),
            accessories: Some(0.0),
        }]),
    )
    .unwrap();

    let photos = lib.cull_photos(&ids).unwrap();
    assert_eq!(photos[&ids[0]].suggested, ["misfire"]);
    assert_eq!(photos[&ids[1]].suggested, ["misfire", "document"]);
    assert!(photos[&ids[2]].suggested.is_empty() && photos[&ids[2]].scored);
    assert_eq!(photos[&ids[3]].suggested, ["eyesClosed"]);
    assert!(!photos[&ids[3]].scored);

    let suggestion = |value: serde_json::Value| {
        names(
            lib.search(&rule(
                json!({"field": "suggestion", "op": "is", "value": value}),
            ))
            .unwrap(),
        )
    };
    assert_eq!(suggestion(json!("misfire")), ["cap.jpg", "page.jpg"]);
    assert_eq!(suggestion(json!("document")), ["page.jpg"]);
    assert_eq!(
        suggestion(json!(["document", "eyesClosed"])),
        ["blink.jpg", "page.jpg"]
    );
    assert!(
        lib.search(&rule(
            json!({"field": "suggestion", "op": "is", "value": "exposure"})
        ))
        .is_err()
    );

    // Dismissed stays dismissed, also when the photograph is scored again.
    let changed = lib
        .set_cull_dismissed(&[ids[0].clone(), ids[2].clone()], "misfire", true)
        .unwrap();
    assert_eq!(changed, [ids[0].clone(), ids[2].clone()]);
    assert!(
        lib.set_cull_dismissed(&[ids[0].clone()], "misfire", true)
            .unwrap()
            .is_empty()
    );
    let p2 = prompts("p2");
    for id in &ids[..3] {
        let fingerprint = lib.asset(id).unwrap().fingerprint;
        lib.record_cull(id, MODEL, &p2, &fingerprint, &toward(cos_of(0.05), 7))
            .unwrap();
    }
    let photos = lib.cull_photos(&ids).unwrap();
    assert!(photos[&ids[0]].suggested.is_empty());
    assert_eq!(photos[&ids[0]].dismissed, ["misfire"]);
    assert_eq!(suggestion(json!("misfire")), ["page.jpg"]);
    assert_eq!(
        names(
            lib.search(&rule(
                json!({"field": "suggestion", "op": "isNot", "value": "misfire"})
            ))
            .unwrap()
        ),
        ["blink.jpg", "cap.jpg", "people.jpg"]
    );
    // Brought back.
    lib.set_cull_dismissed(&[ids[0].clone()], "misfire", false)
        .unwrap();
    assert_eq!(suggestion(json!("misfire")), ["cap.jpg", "page.jpg"]);
    assert!(
        lib.set_cull_dismissed(&[ids[0].clone()], "focus", true)
            .is_err()
    );
    // A suggestion never flags anything.
    assert!(
        lib.search(&rule(
            json!({"field": "flag", "op": "is", "value": "rejected"})
        ))
        .unwrap()
        .is_empty()
    );

    // Stacked.
    lib.group_stack(&[Member::photo(ids[0].clone()), Member::photo(ids[1].clone())])
        .unwrap();
    let stacked = |value: bool| {
        names(
            lib.search(&rule(
                json!({"field": "stacked", "op": "is", "value": value}),
            ))
            .unwrap(),
        )
    };
    assert_eq!(stacked(true), ["cap.jpg", "page.jpg"]);
    assert_eq!(stacked(false), ["blink.jpg", "people.jpg"]);
}

fn at(time: &str, subsec: Option<&str>) -> CaptureFacts {
    CaptureFacts {
        captured: Some(time.into()),
        captured_subsec: subsec.map(str::to_string),
        make: Some("Maker".into()),
        model: Some("Body".into()),
        exposure: Some(0.004),
        ..CaptureFacts::default()
    }
}

#[test]
fn auto_stack_compares_embeddings_and_fractions_of_a_second() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(
        d.path(),
        &["01.ARW", "02.ARW", "03.ARW", "04.ARW", "05.ARW", "06.ARW"],
    );
    let rows = [
        at("2026-11-21 10:00:00", Some("100")),
        at("2026-11-21 10:00:01", Some("500")),
        // Another subject 0.3 s later.
        at("2026-11-21 10:00:01", Some("800")),
        at("2026-11-21 10:00:03", Some("000")),
        // 2.1 s after the one before: beyond a 2 s interval.
        at("2026-11-21 10:00:05", Some("100")),
        at("2026-11-21 10:00:06", None),
    ];
    for (id, facts) in ids.iter().zip(&rows) {
        assert!(lib.record_facts(id, facts).unwrap());
    }
    // A corrected capture time keeps its own fraction: photo 6 moved to
    // 2.8 s after photo 5 is beyond the 2 s interval (its file's whole
    // seconds would read 2.0).
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute(
        "INSERT INTO capture_overrides(asset_id,captured,subsec,offset) VALUES(?1,?2,?3,NULL)",
        rusqlite::params![&ids[5], "2026-11-21 10:00:07", "900"],
    )
    .unwrap();
    let looks = [toward(0.99, 3), toward(0.98, 3), axis(4), toward(0.1, 4)];
    for (id, look) in ids.iter().zip(&looks) {
        embed(&lib, id, look);
    }
    let options = AutoStackOptions {
        interval_secs: 2,
        use_camera_data: false,
        similarity: Some(0.78),
    };
    let runs = |plan: rawtonic_catalog::stacks::AutoPlan| -> Vec<Vec<usize>> {
        plan.groups
            .iter()
            .map(|g| {
                g.assets
                    .iter()
                    .map(|a| ids.iter().position(|i| i == a).unwrap())
                    .collect()
            })
            .collect()
    };
    let plan = lib
        .auto_stack_plan_with(&ids, options, Some(MODEL))
        .unwrap();
    assert_eq!(plan.unread, 2);
    assert_eq!(runs(plan), [vec![0, 1], vec![2, 3]]);
    // Without a model (or similarity) time alone: 0–3 are one.
    let plan = lib.auto_stack_plan_with(&ids, options, None).unwrap();
    assert_eq!(plan.unread, 0);
    assert_eq!(runs(plan), [vec![0, 1, 2, 3]]);
    let plan = lib
        .auto_stack_plan_with(
            &ids,
            AutoStackOptions {
                similarity: None,
                ..options
            },
            Some(MODEL),
        )
        .unwrap();
    assert_eq!(runs(plan), [vec![0, 1, 2, 3]]);
    // Without the correction its file's 10:00:06 joins photo 5.
    db.execute("DELETE FROM capture_overrides", []).unwrap();
    let plan = lib.auto_stack_plan_with(&ids, options, None).unwrap();
    assert_eq!(runs(plan), [vec![0, 1, 2, 3], vec![4, 5]]);
}

/// Not a check: prints Auto-Stack's plan of every photograph of the library
/// at `CUL2_LIBRARY` as the app makes it from the Culling panel's defaults
/// (2 s, look-alike 0.78, embeddings of model `CUL2_MODEL`), as JSON, for
/// the CUL2 hold-out re-check (notes: scripts/reference/cul2/).
#[test]
#[ignore]
fn dump_auto_stack_plan() {
    let (Some(dir), Some(model)) = (
        std::env::var_os("CUL2_LIBRARY"),
        std::env::var("CUL2_MODEL").ok(),
    ) else {
        return;
    };
    let lib = Library::open(Path::new(&dir)).unwrap();
    let ids: Vec<String> = lib
        .assets(None)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    let options = AutoStackOptions {
        interval_secs: 2,
        // `CUL2_CAMERA=0`: without the camera's burst and bracket data.
        use_camera_data: std::env::var("CUL2_CAMERA").as_deref() != Ok("0"),
        similarity: (std::env::var("CUL2_LOOK").as_deref() != Ok("0"))
            .then_some(rawtonic_catalog::cull::STACK_SIMILARITY),
    };
    let plan = lib
        .auto_stack_plan_with(&ids, options, Some(&model))
        .unwrap();
    println!("CUL2PLAN {}", serde_json::to_string(&plan).unwrap());
}
