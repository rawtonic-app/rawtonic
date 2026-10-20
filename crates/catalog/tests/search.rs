//! Library search and smart albums (G36/G37): every field, nested groups,
//! facets, smart albums kept current, the capture-facts pass, migration.
use rawtonic_catalog::{CaptureFacts, Field, Library, Mode, Rule, SMART};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn rule(value: Value) -> Rule {
    serde_json::from_value(value).unwrap()
}

/// File names, sorted (search results come in path order).
fn names(paths: Vec<PathBuf>) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[allow(clippy::too_many_arguments)] // one fixture photograph per call, field by field
fn facts(
    make: &str,
    model: &str,
    lens: &str,
    iso: u32,
    focal: f64,
    aperture: f64,
    exposure: f64,
    captured: &str,
) -> CaptureFacts {
    CaptureFacts {
        captured: Some(captured.into()),
        make: Some(make.into()),
        model: Some(model.into()),
        lens: Some(lens.into()),
        focal_length: Some(focal),
        aperture: Some(aperture),
        iso: Some(iso),
        exposure: Some(exposure),
    }
}

/// Four photographs with capture facts, ratings, labels, flags, keywords.
fn library(dir: &Path) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    fs::create_dir_all(capture.join("Day 2")).unwrap();
    let files = [
        (
            "DSC00123.ARW",
            facts(
                "SONY",
                "ILCE-7RM4",
                "FE 24-70mm F2.8 GM",
                100,
                24.0,
                2.8,
                1.0 / 250.0,
                "2026-10-01 10:00:00",
            ),
        ),
        (
            "DSC00124.ARW",
            facts(
                "SONY",
                "ILCE-7RM4",
                "FE 85mm F1.4 GM",
                3200,
                85.0,
                1.4,
                1.0 / 60.0,
                "2026-10-01 18:30:00",
            ),
        ),
        (
            "IMG_0001.CR3",
            facts(
                "Canon",
                "Canon EOS R5",
                "RF24-105mm F4 L IS USM",
                800,
                105.0,
                4.0,
                0.5,
                "2026-09-15 12:00:00",
            ),
        ),
        ("Day 2/scan.jpg", CaptureFacts::default()),
    ];
    let mut ids = Vec::new();
    for (name, facts) in files {
        let path = capture.join(name);
        fs::write(&path, name.as_bytes()).unwrap();
        let id = lib.add_asset(&path).unwrap().id;
        lib.record_facts(&id, &facts).unwrap();
        ids.push(id);
    }
    let states = [
        json!({"rating":5,"tags":["user:wedding","user:Bride","color:red"],"adjustments":{}}),
        json!({"rating":3,"tags":["user:wedding","user:dance floor"],"adjustments":{}}),
        json!({"rating":0,"tags":["user:landscape","color:blue"],"adjustments":{}}),
        json!({"rating":1,"tags":[],"adjustments":{}}),
    ];
    for (id, state) in ids.iter().zip(states) {
        lib.save_editor_state(id, &state).unwrap();
    }
    lib.set_flags(&ids[..1], 1).unwrap();
    lib.set_flags(&ids[2..3], -1).unwrap();
    (lib, ids)
}

fn found(lib: &Library, value: Value) -> Vec<String> {
    names(lib.search(&rule(value)).unwrap())
}

#[test]
fn text_finds_part_of_any_word_in_any_searchable_field() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    // Part of a file name (not a word start), like Lightroom's "contains".
    assert_eq!(
        found(&lib, json!({"field":"text","op":"contains","value":"0123"})),
        ["DSC00123.ARW"]
    );
    // Keywords, camera, lens and folder names are searchable; case is ignored.
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"BRIDE"})
        ),
        ["DSC00123.ARW"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"eos r5"})
        ),
        ["IMG_0001.CR3"]
    );
    assert_eq!(
        found(&lib, json!({"field":"text","op":"contains","value":"85mm"})),
        ["DSC00124.ARW"]
    );
    assert_eq!(
        found(&lib, json!({"field":"text","op":"contains","value":"day"})),
        ["scan.jpg"]
    );
    // Every word must be there; words shorter than the index's trigrams too.
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"wedding 4"})
        ),
        ["DSC00123.ARW", "DSC00124.ARW"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"wedding 85"})
        ),
        ["DSC00124.ARW"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"containsAny","value":"bride landscape"})
        ),
        ["DSC00123.ARW", "IMG_0001.CR3"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"notContains","value":"wedding"})
        ),
        ["IMG_0001.CR3", "scan.jpg"]
    );
    // Search syntax is text: quotes and FTS operators find nothing odd.
    assert!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"\"OR\" * NEAR("})
        )
        .is_empty()
    );
    // A word most photographs hold is checked row by row instead of
    // through the index: the same result.
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"APTUR 0123"})
        ),
        ["DSC00123.ARW"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"notContains","value":"apture"})
        )
        .len(),
        0
    );
    // The CLI's search is the same search.
    assert_eq!(lib.assets(Some("0123")).unwrap().len(), 1);
}

#[test]
fn every_field_compares_as_documented() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    let cases: Vec<(Value, Vec<&str>)> = vec![
        (
            json!({"field":"camera","op":"is","value":"ilce-7rm4"}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"make","op":"is","value":"Canon"}),
            vec!["IMG_0001.CR3"],
        ),
        (json!({"field":"camera","op":"empty"}), vec!["scan.jpg"]),
        (
            json!({"field":"lens","op":"contains","value":"GM 85"}),
            vec!["DSC00124.ARW"],
        ),
        (
            json!({"field":"lens","op":"startsWith","value":"rf"}),
            vec!["IMG_0001.CR3"],
        ),
        (
            json!({"field":"lens","op":"endsWith","value":"usm"}),
            vec!["IMG_0001.CR3"],
        ),
        (
            json!({"field":"filename","op":"startsWith","value":"dsc"}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"filename","op":"is","value":"scan.jpg"}),
            vec!["scan.jpg"],
        ),
        (
            json!({"field":"keyword","op":"is","value":"WEDDING"}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"keyword","op":"isNot","value":"wedding"}),
            vec!["IMG_0001.CR3", "scan.jpg"],
        ),
        (
            json!({"field":"keyword","op":"contains","value":"floor"}),
            vec!["DSC00124.ARW"],
        ),
        (json!({"field":"keyword","op":"empty"}), vec!["scan.jpg"]),
        (
            json!({"field":"fileType","op":"is","value":"raw"}),
            vec!["DSC00123.ARW", "DSC00124.ARW", "IMG_0001.CR3"],
        ),
        (
            json!({"field":"fileType","op":"is","value":["jpeg","tiff"]}),
            vec!["scan.jpg"],
        ),
        (
            json!({"field":"fileType","op":"isNot","value":"jpeg"}),
            vec!["DSC00123.ARW", "DSC00124.ARW", "IMG_0001.CR3"],
        ),
        (
            json!({"field":"rating","op":"ge","value":3}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"rating","op":"is","value":5}),
            vec!["DSC00123.ARW"],
        ),
        (json!({"field":"rating","op":"empty"}), vec!["IMG_0001.CR3"]),
        (
            json!({"field":"rating","op":"between","value":[1,3]}),
            vec!["DSC00124.ARW", "scan.jpg"],
        ),
        (
            json!({"field":"flag","op":"is","value":"picked"}),
            vec!["DSC00123.ARW"],
        ),
        (
            json!({"field":"flag","op":"is","value":["unflagged","rejected"]}),
            vec!["DSC00124.ARW", "IMG_0001.CR3", "scan.jpg"],
        ),
        (
            json!({"field":"label","op":"is","value":["red","none"]}),
            vec!["DSC00123.ARW", "DSC00124.ARW", "scan.jpg"],
        ),
        (
            json!({"field":"label","op":"notEmpty"}),
            vec!["DSC00123.ARW", "IMG_0001.CR3"],
        ),
        (
            json!({"field":"iso","op":"ge","value":800}),
            vec!["DSC00124.ARW", "IMG_0001.CR3"],
        ),
        (json!({"field":"iso","op":"empty"}), vec!["scan.jpg"]),
        (
            json!({"field":"focalLength","op":"between","value":[20,50]}),
            vec!["DSC00123.ARW"],
        ),
        (
            json!({"field":"aperture","op":"le","value":2.8}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"aperture","op":"is","value":4}),
            vec!["IMG_0001.CR3"],
        ),
        // 1/250 s is 0.004 s; cameras write it as 1/250 or 10/2500.
        (
            json!({"field":"exposure","op":"is","value":0.004}),
            vec!["DSC00123.ARW"],
        ),
        (
            json!({"field":"exposure","op":"gt","value":0.004}),
            vec!["DSC00124.ARW", "IMG_0001.CR3"],
        ),
        (
            json!({"field":"captureDate","op":"is","value":"2026-10-01"}),
            vec!["DSC00123.ARW", "DSC00124.ARW"],
        ),
        (
            json!({"field":"captureDate","op":"lt","value":"2026-10-01"}),
            vec!["IMG_0001.CR3"],
        ),
        (
            json!({"field":"captureDate","op":"between","value":["2026-09-01","2026-09-30"]}),
            vec!["IMG_0001.CR3"],
        ),
        (
            json!({"field":"captureDate","op":"empty"}),
            vec!["scan.jpg"],
        ),
        (
            json!({"field":"offline","op":"is","value":false}),
            vec!["DSC00123.ARW", "DSC00124.ARW", "IMG_0001.CR3", "scan.jpg"],
        ),
        (
            json!({"field":"previousImport","op":"is","value":true}),
            vec![],
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(found(&lib, value.clone()), expected, "{value}");
    }
}

#[test]
fn groups_nest_with_all_any_and_none() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    // Sony at 3 stars or more, or anything rejected — but no "Bride".
    let nested = json!({"combine":"all","rules":[
        {"combine":"any","rules":[
            {"combine":"all","rules":[
                {"field":"make","op":"is","value":"sony"},
                {"field":"rating","op":"ge","value":3}
            ]},
            {"field":"flag","op":"is","value":"rejected"}
        ]},
        {"combine":"none","rules":[{"field":"keyword","op":"is","value":"bride"}]}
    ]});
    assert_eq!(found(&lib, nested), ["DSC00124.ARW", "IMG_0001.CR3"]);
    // Empty groups: all/none match everything, any matches nothing.
    assert_eq!(found(&lib, json!({"combine":"all","rules":[]})).len(), 4);
    assert_eq!(found(&lib, json!({"combine":"none","rules":[]})).len(), 4);
    assert!(found(&lib, json!({"combine":"any","rules":[]})).is_empty());
}

#[test]
fn rules_that_cannot_be_evaluated_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    for bad in [
        json!({"field":"rating","op":"ge","value":7}),
        json!({"field":"rating","op":"contains","value":"x"}),
        json!({"field":"flag","op":"is","value":"maybe"}),
        json!({"field":"label","op":"is","value":"orange"}),
        json!({"field":"fileType","op":"is","value":"mp4"}),
        json!({"field":"captureDate","op":"is","value":"yesterday"}),
        json!({"field":"captureDate","op":"inLast","value":{"amount":0,"unit":"days"}}),
        json!({"field":"captureDate","op":"inLast","value":{"amount":2,"unit":"fortnights"}}),
        json!({"field":"iso","op":"between","value":[100]}),
        json!({"field":"folder","op":"contains","value":"../outside"}),
    ] {
        assert!(lib.search(&rule(bad.clone())).is_err(), "{bad}");
    }
    // Too deep.
    let mut deep = json!({"field":"rating","op":"ge","value":1});
    for _ in 0..20 {
        deep = json!({"combine":"all","rules":[deep]});
    }
    assert!(lib.search(&rule(deep)).is_err());
    // Unknown fields and operators are not even read.
    assert!(serde_json::from_value::<Rule>(json!({"field":"gps","op":"is","value":1})).is_err());
}

#[test]
fn relative_dates_count_back_from_today() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let today = chrono::Local::now().date_naive();
    let day = |n: u64| format!("{} 09:00:00", today - chrono::Days::new(n));
    for (id, captured) in ids.iter().zip([day(0), day(6), day(7), day(400)]) {
        lib.record_facts(
            id,
            &CaptureFacts {
                captured: Some(captured),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let last = |amount: u32, unit: &str| {
        found(
            &lib,
            json!({"field":"captureDate","op":"inLast","value":{"amount":amount,"unit":unit}}),
        )
    };
    assert_eq!(last(1, "days"), ["DSC00123.ARW"]);
    assert_eq!(last(7, "days"), ["DSC00123.ARW", "DSC00124.ARW"]);
    assert_eq!(last(1, "weeks"), ["DSC00123.ARW", "DSC00124.ARW"]);
    assert_eq!(last(1, "months").len(), 3);
    assert_eq!(last(2, "years").len(), 4);
    assert_eq!(
        found(
            &lib,
            json!({"field":"captureDate","op":"notInLast","value":{"amount":1,"unit":"years"}})
        ),
        ["scan.jpg"]
    );
}

#[test]
fn folders_albums_and_offline_photographs_scope_a_search() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let capture = lib.directory.join("Capture");
    let inside = |op: &str| {
        found(
            &lib,
            json!({"field":"folder","op":op,"value":capture.to_string_lossy()}),
        )
    };
    assert_eq!(inside("contains").len(), 4);
    assert_eq!(
        inside("is"),
        ["DSC00123.ARW", "DSC00124.ARW", "IMG_0001.CR3"]
    );
    assert_eq!(inside("isNot"), ["scan.jpg"]);
    let day2 = capture.join("Day 2");
    assert_eq!(
        found(
            &lib,
            json!({"field":"folder","op":"is","value":day2.to_string_lossy()})
        ),
        ["scan.jpg"]
    );
    // A folder outside the library holds none of its photographs.
    assert!(found(&lib, json!({"field":"folder","op":"contains","value":d.path().join("elsewhere").to_string_lossy()})).is_empty());

    let album = lib.create_collection("Picks", None).unwrap();
    lib.add_to_collection(&album.id, &ids[1..3]).unwrap();
    let group = lib
        .create_collection_of_kind("Clients", None, rawtonic_catalog::GROUP)
        .unwrap();
    let inner = lib.create_collection("Inner", Some(&group.id)).unwrap();
    lib.add_to_collection(&inner.id, &ids[3..]).unwrap();
    assert_eq!(
        found(&lib, json!({"field":"album","op":"is","value":album.id})),
        ["DSC00124.ARW", "IMG_0001.CR3"]
    );
    assert_eq!(
        found(&lib, json!({"field":"album","op":"isNot","value":album.id})),
        ["DSC00123.ARW", "scan.jpg"]
    );
    assert_eq!(
        found(&lib, json!({"field":"album","op":"is","value":group.id})),
        ["scan.jpg"]
    );
    assert!(found(&lib, json!({"field":"album","op":"is","value":"gone"})).is_empty());

    fs::remove_file(capture.join("IMG_0001.CR3")).unwrap();
    lib.verify_paths().unwrap();
    assert_eq!(
        found(&lib, json!({"field":"offline","op":"is","value":true})),
        ["IMG_0001.CR3"]
    );
}

#[test]
fn smart_albums_stay_current_and_live_among_the_albums() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let group = lib
        .create_collection_of_kind("Weddings", None, rawtonic_catalog::GROUP)
        .unwrap();
    let rules = rule(json!({"combine":"all","rules":[
        {"field":"keyword","op":"is","value":"wedding"},
        {"field":"rating","op":"ge","value":4}
    ]}));
    let smart = lib
        .create_smart_collection("Best of the wedding", Some(&group.id), &rules)
        .unwrap();
    assert_eq!((smart.kind.as_str(), smart.count), (SMART, 1));
    assert_eq!(
        names(lib.collection_paths(&smart.id).unwrap()),
        ["DSC00123.ARW"]
    );

    // A rating or keyword change shows at once: nothing to refresh.
    let mut meta = lib.metadata(&ids[1]).unwrap();
    meta.rating = 4;
    lib.set_metadata(&ids[1], &meta).unwrap();
    assert_eq!(
        names(lib.collection_paths(&smart.id).unwrap()),
        ["DSC00123.ARW", "DSC00124.ARW"]
    );
    lib.save_editor_state(
        &ids[0],
        &json!({"rating":5,"tags":["user:Bride"],"adjustments":{}}),
    )
    .unwrap();
    assert_eq!(
        names(lib.collection_paths(&smart.id).unwrap()),
        ["DSC00124.ARW"]
    );

    let listed = lib.collections().unwrap();
    let entry = listed.iter().find(|c| c.id == smart.id).unwrap();
    assert_eq!(
        (entry.count, entry.parent.as_deref(), entry.rules.as_ref()),
        (1, Some(group.id.as_str()), Some(&rules))
    );
    // A group holds smart albums too; a search of the group finds theirs.
    assert_eq!(
        found(&lib, json!({"field":"album","op":"is","value":group.id})),
        ["DSC00124.ARW"]
    );

    // Rules can be changed; photographs cannot be put into a smart album.
    lib.set_collection_rules(
        &smart.id,
        &rule(json!({"field":"make","op":"is","value":"canon"})),
    )
    .unwrap();
    assert_eq!(
        names(lib.collection_paths(&smart.id).unwrap()),
        ["IMG_0001.CR3"]
    );
    assert!(lib.add_to_collection(&smart.id, &ids[..1]).is_err());
    assert!(lib.set_collection_rules(&group.id, &rules).is_err());
    assert!(lib.create_collection_of_kind("x", None, SMART).is_err());

    // Smart albums refer to albums and to each other; a loop matches nothing
    // there instead of running forever.
    let other = lib
        .create_smart_collection("Loop", None, &Rule::album(&smart.id))
        .unwrap();
    lib.set_collection_rules(
        &smart.id,
        &rule(json!({"combine":"any","rules":[
            {"field":"album","op":"is","value":other.id},
            {"field":"rating","op":"is","value":1}
        ]})),
    )
    .unwrap();
    assert_eq!(
        names(lib.collection_paths(&other.id).unwrap()),
        ["scan.jpg"]
    );

    lib.rename_collection(&smart.id, "Renamed").unwrap();
    lib.move_collection(&smart.id, None, 0).unwrap();
    drop(lib);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    let reopened = lib.collections().unwrap();
    assert_eq!(reopened[0].name, "Renamed");
    lib.delete_collection(&group.id).unwrap();
    assert_eq!(lib.assets(None).unwrap().len(), 4);
}

#[test]
fn facets_list_values_with_their_counts_within_a_search() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    let all = lib
        .facets(&Rule::everything(), rawtonic_catalog::query::FACETS)
        .unwrap();
    let pairs = |field: Field| -> Vec<(String, usize)> {
        all[&field]
            .iter()
            .map(|v| (v.value.clone(), v.count))
            .collect()
    };
    let p = |v: &str, n: usize| (v.to_string(), n);
    assert_eq!(
        pairs(Field::Camera),
        [p("ILCE-7RM4", 2), p("Canon EOS R5", 1), p("", 1)]
    );
    assert_eq!(pairs(Field::FileType), [p("raw", 3), p("jpeg", 1)]);
    assert_eq!(
        pairs(Field::Keyword),
        [
            p("wedding", 2),
            p("Bride", 1),
            p("dance floor", 1),
            p("landscape", 1),
            p("", 1)
        ]
    );
    assert_eq!(
        pairs(Field::Iso),
        [p("100", 1), p("800", 1), p("3200", 1), p("", 1)]
    );
    assert_eq!(
        pairs(Field::Aperture),
        [p("1.4", 1), p("2.8", 1), p("4", 1), p("", 1)]
    );
    assert_eq!(
        pairs(Field::Flag),
        [p("unflagged", 2), p("picked", 1), p("rejected", 1)]
    );
    assert_eq!(
        pairs(Field::Rating),
        [p("0", 1), p("1", 1), p("3", 1), p("5", 1)]
    );
    assert_eq!(
        pairs(Field::CaptureDate),
        [p("2026-10", 2), p("2026-09", 1), p("", 1)]
    );
    // Within a search: the values of what it finds.
    let sony = lib
        .facets(
            &rule(json!({"field":"make","op":"is","value":"sony"})),
            &[Field::Lens],
        )
        .unwrap();
    assert_eq!(sony[&Field::Lens].len(), 2);
}

#[test]
fn the_facts_pass_reads_each_photograph_once_and_keeps_newer_records() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    for name in ["A.ARW", "B.ARW", "C.ARW"] {
        fs::write(capture.join(name), name).unwrap();
        lib.add_asset(&capture.join(name)).unwrap();
    }
    assert_eq!(lib.facts_backlog().unwrap(), 3);
    let reads = std::cell::Cell::new(0);
    // B cannot be read now: it is tried again by a later pass.
    let read = |path: &Path| {
        reads.set(reads.get() + 1);
        (!path.ends_with("B.ARW")).then(|| CaptureFacts {
            model: Some(format!(
                "Model of {}",
                path.file_name().unwrap().to_string_lossy()
            )),
            iso: Some(400),
            ..Default::default()
        })
    };
    assert_eq!(
        lib.fill_all_facts(&AtomicBool::new(false), &read).unwrap(),
        2
    );
    assert_eq!(reads.get(), 3);
    assert_eq!(lib.facts_backlog().unwrap(), 1);
    let a = lib.asset_at(&capture.join("A.ARW")).unwrap().unwrap();
    let (facts, read_once) = lib.facts(&a.id).unwrap().unwrap();
    assert!(read_once);
    assert_eq!(
        (facts.model.as_deref(), facts.iso),
        (Some("Model of A.ARW"), Some(400))
    );
    assert_eq!(
        found(
            &lib,
            json!({"field":"camera","op":"is","value":"model of a.arw"})
        ),
        ["A.ARW"]
    );
    // A second pass reads only B; a cancelled pass reads nothing.
    assert_eq!(
        lib.fill_all_facts(&AtomicBool::new(true), &read).unwrap(),
        0
    );
    assert_eq!(
        lib.fill_all_facts(&AtomicBool::new(false), &|_: &Path| Some(
            CaptureFacts::default()
        ))
        .unwrap(),
        1
    );
    assert_eq!(lib.facts_backlog().unwrap(), 0);
    // Odd values a damaged file gives are dropped.
    lib.record_facts(
        &a.id,
        &CaptureFacts {
            captured: Some("2026-13-45 99:00:00".into()),
            iso: Some(0),
            aperture: Some(f64::NAN),
            exposure: Some(-1.0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        lib.facts(&a.id).unwrap().unwrap().0,
        CaptureFacts::default()
    );
}

#[test]
fn a_schema_10_library_gets_its_keywords_search_and_albums_migrated() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let album = lib.create_collection("Kept", None).unwrap();
    lib.add_to_collection(&album.id, &ids[..2]).unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0011.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    // Keywords and search text come along; capture facts are read again.
    assert_eq!(
        found(&lib, json!({"field":"keyword","op":"is","value":"wedding"})).len(),
        2
    );
    assert_eq!(
        found(&lib, json!({"field":"text","op":"contains","value":"0123"})),
        ["DSC00123.ARW"]
    );
    assert_eq!(lib.facts_backlog().unwrap(), 4);
    assert_eq!(
        names(lib.collection_paths(&album.id).unwrap()),
        ["DSC00123.ARW", "DSC00124.ARW"]
    );
    assert_eq!(lib.collections().unwrap()[0].count, 2);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    let broken: Vec<String> = db
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(broken.is_empty(), "{broken:?}");
}

#[test]
fn a_none_group_keeps_photographs_without_the_value() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    // scan.jpg has no ISO, no capture date and no lens: none of these rules
    // holds for it.
    assert_eq!(
        found(
            &lib,
            json!({"combine":"none","rules":[{"field":"iso","op":"ge","value":800}]})
        ),
        ["DSC00123.ARW", "scan.jpg"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"combine":"none","rules":[{"field":"captureDate","op":"lt","value":"2026-10-01"}]})
        ),
        ["DSC00123.ARW", "DSC00124.ARW", "scan.jpg"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"combine":"none","rules":[{"field":"lens","op":"startsWith","value":"FE"}]})
        ),
        ["IMG_0001.CR3", "scan.jpg"]
    );
}

#[test]
fn case_is_folded_beyond_ascii_whichever_way_a_word_is_found() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    for name in [
        "École 1.jpg",
        "École 2.jpg",
        "Hafen.jpg",
        "ΟΣΟΣ 1.jpg",
        "οσος 2.jpg",
    ] {
        fs::write(capture.join(name), name).unwrap();
        lib.add_asset(&capture.join(name)).unwrap();
    }
    let search = |lib: &Library, text: &str| {
        found(lib, json!({"field":"text","op":"contains","value":text}))
    };
    // "éco": two of five photographs (through the index) …
    assert_eq!(search(&lib, "éco"), ["École 1.jpg", "École 2.jpg"]);
    // Greek: the final sigma folds the same in both ways a word is found.
    assert_eq!(search(&lib, "οσος"), ["ΟΣΟΣ 1.jpg", "οσος 2.jpg"]);
    for gone in ["Hafen.jpg", "École 1.jpg"] {
        lib.forget_at(&capture.join(gone)).unwrap();
    }
    // … and of three, most hold "οσος": checked row by row, same result.
    assert_eq!(search(&lib, "ΟΣΟΣ"), ["ΟΣΟΣ 1.jpg", "οσος 2.jpg"]);
    assert_eq!(search(&lib, "οσοσ"), Vec::<String>::new());
    fs::write(capture.join("Hafen.jpg"), "Hafen.jpg").unwrap();
    lib.add_asset(&capture.join("Hafen.jpg")).unwrap();
    fs::write(capture.join("École 1.jpg"), "École 1.jpg").unwrap();
    lib.add_asset(&capture.join("École 1.jpg")).unwrap();
    // "éco": two of five again; with the Greek ones gone, two of three.
    for gone in ["ΟΣΟΣ 1.jpg", "οσος 2.jpg"] {
        lib.forget_at(&capture.join(gone)).unwrap();
    }
    assert_eq!(search(&lib, "éco"), ["École 1.jpg", "École 2.jpg"]);
    assert_eq!(search(&lib, "ÉC"), ["École 1.jpg", "École 2.jpg"]);
    assert_eq!(
        found(
            &lib,
            json!({"field":"filename","op":"startsWith","value":"écol"})
        )
        .len(),
        2
    );
    // … and the same once they are few (through the index).
    for i in 0..6 {
        let name = format!("Other {i}.jpg");
        fs::write(capture.join(&name), &name).unwrap();
        lib.add_asset(&capture.join(&name)).unwrap();
    }
    assert_eq!(search(&lib, "éco"), ["École 1.jpg", "École 2.jpg"]);
    let id = lib
        .asset_at(&capture.join("Hafen.jpg"))
        .unwrap()
        .unwrap()
        .id;
    let mut meta = lib.metadata(&id).unwrap();
    meta.keywords = vec!["Ärger".into()];
    lib.set_metadata(&id, &meta).unwrap();
    assert_eq!(
        found(&lib, json!({"field":"keyword","op":"is","value":"ärger"})),
        ["Hafen.jpg"]
    );
}

#[test]
fn a_smart_album_of_a_folder_moves_with_its_session() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    let day2 = lib.directory.join("Capture/Day 2");
    let rules = rule(json!({"field":"folder","op":"is","value":day2.to_string_lossy()}));
    let smart = lib.create_smart_collection("Day 2", None, &rules).unwrap();
    assert_eq!(smart.count, 1);
    // Kept relative to the library; a folder outside it stays absolute.
    let kept = lib
        .collections()
        .unwrap()
        .into_iter()
        .find(|c| c.id == smart.id)
        .unwrap();
    assert_eq!(
        kept.rules,
        Some(rule(
            json!({"field":"folder","op":"is","value":"Capture/Day 2"})
        ))
    );
    let outside = d.path().join("elsewhere").to_string_lossy().into_owned();
    lib.set_collection_rules(
        &smart.id,
        &rule(json!({"combine":"any","rules":[
            {"field":"folder","op":"contains","value":outside},
            {"field":"folder","op":"is","value":day2.to_string_lossy()}
        ]})),
    )
    .unwrap();
    drop(lib);
    let moved = d.path().join("Moved");
    fs::rename(d.path().join("Job"), &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(
        names(lib.collection_paths(&smart.id).unwrap()),
        ["scan.jpg"]
    );
    let kept = lib
        .collections()
        .unwrap()
        .into_iter()
        .find(|c| c.id == smart.id)
        .unwrap();
    assert_eq!(
        kept.rules,
        Some(rule(json!({"combine":"any","rules":[
            {"field":"folder","op":"contains","value":outside},
            {"field":"folder","op":"is","value":"Capture/Day 2"}
        ]})))
    );
}

#[test]
fn a_folder_above_the_library_holds_its_photographs_with_subfolders() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    let above = d
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        found(
            &lib,
            json!({"field":"folder","op":"contains","value":above})
        )
        .len(),
        4
    );
    assert!(
        found(
            &lib,
            json!({"field":"folder","op":"notContains","value":above})
        )
        .is_empty()
    );
    // Directly in it: none (they are in the library's folders).
    assert!(found(&lib, json!({"field":"folder","op":"is","value":above})).is_empty());
}

#[test]
fn the_library_folder_itself_is_a_folder_rule_that_moves_with_it() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let root = lib.directory.to_string_lossy().into_owned();
    let rules = rule(json!({"field":"folder","op":"contains","value":root}));
    let smart = lib
        .create_smart_collection("Everything", None, &rules)
        .unwrap();
    assert_eq!(smart.count, 4);
    assert_eq!(
        smart.rules,
        Some(rule(json!({"field":"folder","op":"contains","value":"."})))
    );
    // Photographs are not taken out of a smart album by hand.
    assert!(lib.remove_from_collection(&smart.id, &ids[..1]).is_err());
    drop(lib);
    fs::rename(d.path().join("Job"), d.path().join("Moved")).unwrap();
    let lib = Library::open(&d.path().join("Moved")).unwrap();
    assert_eq!(lib.collection_paths(&smart.id).unwrap().len(), 4);
}

#[test]
fn a_keyword_in_two_spellings_counts_its_photograph_once() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let mut meta = lib.metadata(&ids[2]).unwrap();
    meta.keywords = vec!["Ärger".into(), "ärger".into()];
    lib.set_metadata(&ids[2], &meta).unwrap();
    let facets = lib.facets(&Rule::everything(), &[Field::Keyword]).unwrap();
    let arger: Vec<_> = facets[&Field::Keyword]
        .iter()
        .filter(|v| v.value.to_lowercase() == "ärger")
        .map(|v| v.count)
        .collect();
    assert_eq!(arger, [1]);
}

#[test]
fn camera_spellings_differing_in_case_are_one_value() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    lib.record_facts(
        &ids[3],
        &CaptureFacts {
            model: Some("ilce-7rm4".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let facets = lib.facets(&Rule::everything(), &[Field::Camera]).unwrap();
    let cameras: Vec<_> = facets[&Field::Camera]
        .iter()
        .map(|v| (v.value.as_str(), v.count))
        .collect();
    // As many as choosing it finds.
    assert_eq!(cameras, [("ILCE-7RM4", 3), ("Canon EOS R5", 1)]);
    assert_eq!(
        found(
            &lib,
            json!({"field":"camera","op":"is","value":"ILCE-7RM4"})
        )
        .len(),
        3
    );
}

#[test]
fn a_listed_focal_length_or_aperture_selects_what_it_counts() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    for (id, focal, aperture) in [(&ids[0], 50.0, 2.85), (&ids[1], 50.5, 2.75)] {
        lib.record_facts(
            id,
            &CaptureFacts {
                focal_length: Some(focal),
                aperture: Some(aperture),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let facets = lib
        .facets(&Rule::everything(), &[Field::FocalLength, Field::Aperture])
        .unwrap();
    for field in [Field::FocalLength, Field::Aperture] {
        for listed in &facets[&field] {
            if listed.value.is_empty() {
                continue;
            }
            let name = if field == Field::FocalLength {
                "focalLength"
            } else {
                "aperture"
            };
            let value: f64 = listed.value.parse().unwrap();
            assert_eq!(
                found(&lib, json!({"field":name,"op":"is","value":value})).len(),
                listed.count,
                "{field:?} {}",
                listed.value
            );
        }
    }
    assert_eq!(
        found(&lib, json!({"field":"focalLength","op":"is","value":50})),
        ["DSC00123.ARW"]
    );
    // Typed values compare with the stored ones.
    assert_eq!(
        found(&lib, json!({"field":"focalLength","op":"is","value":50.5})),
        ["DSC00124.ARW"]
    );
    assert!(
        !found(&lib, json!({"field":"focalLength","op":"gt","value":50.5}))
            .contains(&"DSC00124.ARW".to_string())
    );
    assert_eq!(
        found(&lib, json!({"field":"aperture","op":"is","value":2.85})),
        ["DSC00123.ARW"]
    );
    assert!(
        !found(&lib, json!({"field":"aperture","op":"isNot","value":2.85}))
            .contains(&"DSC00123.ARW".to_string())
    );
}

#[test]
fn details_are_the_searchable_text_without_keywords() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.title = "Ceremony".into();
    meta.iptc.insert("city".into(), "Ascona".into());
    lib.set_metadata(&ids[0], &meta).unwrap();
    let details = |text: &str| {
        found(
            &lib,
            json!({"field":"details","op":"contains","value":text}),
        )
    };
    assert_eq!(details("ceremony ascona"), ["DSC00123.ARW"]);
    assert_eq!(details("sony 0123"), ["DSC00123.ARW"]);
    // Its keyword "wedding" is no detail (the text search finds it).
    assert!(details("wedding").is_empty());
    assert_eq!(
        found(
            &lib,
            json!({"field":"text","op":"contains","value":"wedding ceremony"})
        ),
        ["DSC00123.ARW"]
    );
}

#[test]
fn smart_albums_naming_their_own_group_stay_small() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _) = library(d.path());
    let group = lib
        .create_collection_of_kind("G", None, rawtonic_catalog::GROUP)
        .unwrap();
    let rules = rule(json!({"field":"album","op":"is","value":group.id}));
    for i in 0..8 {
        lib.create_smart_collection(&format!("S{i}"), Some(&group.id), &rules)
            .unwrap();
    }
    let started = std::time::Instant::now();
    let albums = lib.collections().unwrap();
    assert!(albums.iter().all(|a| a.count == 0));
    assert!(found(&lib, json!({"field":"album","op":"is","value":group.id})).is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn keywords_with_spaces_around_them_are_listed_as_they_select() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path());
    let mut meta = lib.metadata(&ids[3]).unwrap();
    meta.keywords = vec!["\t harbour\u{a0}".into()];
    lib.set_metadata(&ids[3], &meta).unwrap();
    let facets = lib.facets(&Rule::everything(), &[Field::Keyword]).unwrap();
    let listed = facets[&Field::Keyword]
        .iter()
        .find(|v| v.value.contains("harbour"))
        .unwrap();
    assert_eq!((listed.value.as_str(), listed.count), ("harbour", 1));
    assert_eq!(
        found(
            &lib,
            json!({"field":"keyword","op":"is","value":listed.value})
        ),
        ["scan.jpg"]
    );
}
