//! The Export as Catalog round trip on real RAW files (G51 use test, task
//! B): a catalog of six photographs with masks and a virtual copy goes to
//! another computer with its originals, comes back edits only and is
//! imported. Prints the package sizes and the review; asserts that the
//! return holds no original and that each side is told right.
//!
//! Needs `RAWTONIC_RAW_FIXTURES` (a folder of at least six RAW files); the
//! files are copied into a temporary folder, the originals never touched.
//! `cargo test -p rawtonic-ingest --test catalog_round_trip -- --ignored --nocapture`
use rawtonic_catalog::{Library, Mode, catalog_import::Side};
use rawtonic_fs::Canonical;
use rawtonic_ingest::{
    catalog_export::{self, export_catalog},
    catalog_import::{NewPhotos, Options, import, open_source, review},
};
use serde_json::json;
use std::{collections::HashMap, fs, path::Path, sync::atomic::AtomicBool};

fn size(dir: &Path) -> (u64, usize) {
    let (mut bytes, mut raws) = (0, 0);
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            let (b, r) = size(&entry.path());
            bytes += b;
            raws += r;
        } else {
            bytes += entry.metadata().unwrap().len();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            raws += usize::from(
                [
                    ".arw", ".cr3", ".nef", ".raf", ".dng", ".cr2", ".orf", ".rw2",
                ]
                .iter()
                .any(|e| name.ends_with(e)),
            );
        }
    }
    (bytes, raws)
}

#[test]
#[ignore = "needs RAWTONIC_RAW_FIXTURES"]
fn edits_only_round_trip_on_raw_files() {
    let Some(fixtures) = std::env::var_os("RAWTONIC_RAW_FIXTURES") else {
        eprintln!("SKIP: RAWTONIC_RAW_FIXTURES not set");
        return;
    };
    let mut raws: Vec<_> = fs::read_dir(&fixtures)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| {
                    ["arw", "cr3", "nef", "raf", "dng"]
                        .contains(&e.to_string_lossy().to_lowercase().as_str())
                })
        })
        .collect();
    raws.sort();
    assert!(raws.len() >= 6, "need six RAW files");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonical().unwrap();
    let shoot = root.join("Shoot");
    fs::create_dir_all(&shoot).unwrap();
    let main = Library::create(&root.join("Studio"), "Studio", Mode::Catalog).unwrap();
    let mut ids = Vec::new();
    for raw in &raws[..6] {
        let copy = shoot.join(raw.file_name().unwrap());
        fs::copy(raw, &copy).unwrap();
        ids.push(main.add_asset(&copy).unwrap().id);
    }
    let mask = |exposure: f64| {
        json!({"version": 1, "adjustments": {"exposure": exposure, "masks": [
            {"id": "l1", "name": "Radial", "visible": true, "invert": false, "opacity": 100.0, "maskVersion": 4,
             "adjustments": {"exposure": 0.8},
             "subMasks": [{"id": "r", "type": "radial", "visible": true, "invert": false, "opacity": 100.0, "mode": "additive",
                           "parameters": {"centerX": 300.0, "centerY": 200.0, "radiusX": 150.0, "radiusY": 100.0, "rotation": 0.0, "feather": 0.5}}]},
            {"id": "l2", "name": "Sky", "visible": true, "invert": false, "opacity": 100.0, "maskVersion": 4,
             "adjustments": {"exposure": -0.3},
             "subMasks": [{"id": "s", "type": "ai-sky", "visible": true, "invert": false, "opacity": 100.0, "mode": "additive",
                           "parameters": {"maskDataBase64": format!("data:image/png;base64,{}", "QUJD".repeat(20_000))}}]}]}})
    };
    for (n, id) in ids.iter().enumerate() {
        main.save_editor_state(id, &mask(0.1 * n as f64)).unwrap();
        let mut meta = main.metadata(id).unwrap();
        meta.rating = 1 + n as u8 % 5;
        meta.keywords.push("uxp-test".into());
        main.set_metadata(id, &meta).unwrap();
    }
    main.save_copy_state(&ids[0], "abcdef", &json!({"adjustments": {"exposure": -1}}))
        .unwrap();
    let go = AtomicBool::new(false);
    let export = |from: &Path, ids: &[String], to: &str, originals: bool| {
        export_catalog(
            from,
            ids,
            &root.join(to),
            &catalog_export::Options {
                name: to.into(),
                include_originals: originals,
                leave_out_owned_elsewhere: false,
            },
            &go,
            |_| {},
            |_, _, _| Ok(0),
        )
        .unwrap()
    };
    let handed = export(&main.directory, &ids, "Laptop", true);
    let (handed_bytes, handed_raws) = size(&root.join("Laptop"));
    let laptop = Library::open(&root.join("Laptop")).unwrap();
    // On the laptop: an edit on 0, ratings on 1 and 3; here: 3 and 4.
    laptop.save_editor_state(&ids[0], &mask(1.2)).unwrap();
    for (lib, id, rating) in [
        (&laptop, &ids[1], 5),
        (&laptop, &ids[3], 5),
        (&main, &ids[3], 3),
        (&main, &ids[4], 2),
    ] {
        let mut meta = lib.metadata(id).unwrap();
        meta.rating = rating;
        lib.set_metadata(id, &meta).unwrap();
    }
    let back = export(&laptop.directory, &ids[..4], "Back", false);
    let (back_bytes, back_raws) = size(&root.join("Back"));
    let source = open_source(&main, &root.join("Back")).unwrap();
    let seen = review(&main, &source).unwrap();
    drop(source);
    let receipt = import(
        &main,
        &root.join("Back"),
        &Options {
            new_photos: NewPhotos::Reference,
            changed: None,
            overrides: HashMap::new(),
            include_new: true,
            before_snapshot: "Before import".into(),
            kept_snapshot: "Not imported".into(),
        },
        &go,
        |_| {},
    )
    .unwrap();
    let names: HashMap<&String, String> = ids
        .iter()
        .map(|id| {
            (
                id,
                main.asset(id)
                    .unwrap()
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "handOver": {"bytes": handed_bytes, "raws": handed_raws, "originalsCopied": handed.originals_copied},
            "editsOnlyReturn": {"bytes": back_bytes, "raws": back_raws, "photographs": back.photographs, "editsOnly": back.edits_only},
            "review": seen.photos.iter().map(|p| json!({"photo": names[&p.id], "changes": p.changes, "side": p.side})).collect::<Vec<_>>(),
            "counts": {"new": seen.new, "changed": seen.changed, "unchanged": seen.unchanged},
            "import": {"replaced": receipt.report.replaced, "kept": receipt.report.kept, "snapshotsKept": receipt.report.snapshots_kept,
                        "setAside": receipt.report.set_aside.iter().map(|s| json!({"photo": s.name, "kept": s.kept, "here": s.here.rating, "there": s.there.rating})).collect::<Vec<_>>()},
        }))
        .unwrap()
    );
    assert_eq!(back_raws, 0);
    let side = |n: usize| seen.photos.iter().find(|p| p.id == ids[n]).map(|p| p.side);
    assert_eq!(side(0), Some(Some(Side::There)));
    assert_eq!(side(1), Some(Some(Side::There)));
    assert_eq!(side(3), Some(Some(Side::Both)));
    assert_eq!(side(2), None, "unchanged");
    assert_eq!(main.metadata(&ids[1]).unwrap().rating, 5);
    assert_eq!(main.metadata(&ids[3]).unwrap().rating, 3);
    for (raw, id) in raws.iter().zip(&ids) {
        assert_eq!(
            fs::read(raw).unwrap(),
            fs::read(main.asset(id).unwrap().path).unwrap()
        );
    }
}
