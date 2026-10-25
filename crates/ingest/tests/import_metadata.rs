use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::{
    batch::{BatchOptions, import_batch},
    metadata::ImportMetadata,
};
use rawtonic_metadata::Metadata;
use std::{fs, sync::atomic::AtomicBool};

#[test]
fn preset_is_written_after_publish_keeps_existing_values_and_never_changes_sources() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.NEF");
    fs::write(&source, b"original raw bytes").unwrap();
    let source_xmp = source.with_extension("xmp");
    let original = Metadata {
        creator: "Camera owner".into(),
        keywords: vec!["existing".into()],
        ..Metadata::default()
    };
    rawtonic_metadata::write_xmp(&source_xmp, &original, None, None).unwrap();
    let raw_hash = rawtonic_ingest::sha256(&source).unwrap();
    let xmp_hash = rawtonic_ingest::sha256(&source_xmp).unwrap();
    let lib = Library::create(&d.path().join("Library"), "Library", Mode::Session).unwrap();
    let mut preset = ImportMetadata {
        creator: "Preset creator".into(),
        copyright: "© {year} Photographer".into(),
        keywords: vec!["existing".into(), "new".into()],
        ..ImportMetadata::default()
    };
    preset.iptc.insert("usageTerms".into(), "Editorial".into());
    preset
        .iptc
        .insert("creatorEmail".into(), "private@example.org".into());
    let options = BatchOptions {
        template: "{orig}",
        sequence_start: 1,
        backup: None,
        folder: None,
        roots: &[],
        pair_raw: None,
        album: None,
        metadata: Some(&preset),
    };
    let job =
        rawtonic_ingest::queue::create_job(&lib, std::slice::from_ref(&source), &options).unwrap();
    assert!(!lib.directory.join("Capture/source.xmp").exists());
    assert_eq!(lib.summary().unwrap().assets, 0);
    let results = rawtonic_ingest::queue::run_job(&lib, &job, &AtomicBool::new(false), |done| {
        let r = done.result;
        assert!(r.error.is_none(), "{r:?}");
        let dest = r.destination.as_ref().unwrap();
        assert_eq!(rawtonic_ingest::sha256(dest).unwrap(), raw_hash);
        let meta = lib.metadata(r.asset_id.as_ref().unwrap()).unwrap();
        assert_eq!(meta.creator, "Camera owner");
        assert_eq!(meta.keywords, ["existing", "new"]);
        assert_eq!(meta.iptc.get("usageTerms").unwrap(), "Editorial");
        assert!(!meta.iptc.contains_key("creatorEmail"));
        assert!(!meta.copyright.contains("{year}"));
        let xmp = fs::read_to_string(dest.with_extension("xmp")).unwrap();
        assert_eq!(rawtonic_metadata::read_xmp(&xmp).unwrap().0, meta);
    })
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(rawtonic_ingest::sha256(&source).unwrap(), raw_hash);
    assert_eq!(rawtonic_ingest::sha256(&source_xmp).unwrap(), xmp_hash);
}

#[test]
fn cancelled_import_does_not_apply_preset_or_publish() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.NEF");
    fs::write(&source, b"raw").unwrap();
    let lib = Library::create(&d.path().join("Library"), "Library", Mode::Session).unwrap();
    let preset = ImportMetadata {
        creator: "Creator".into(),
        ..ImportMetadata::default()
    };
    let options = BatchOptions {
        template: "{orig}",
        sequence_start: 1,
        backup: None,
        folder: None,
        roots: &[],
        pair_raw: None,
        album: None,
        metadata: Some(&preset),
    };
    assert!(
        import_batch(
            &lib,
            &[source],
            &options,
            &AtomicBool::new(true),
            |_| panic!("not published")
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(lib.summary().unwrap().assets, 0);
    assert_eq!(
        fs::read_dir(lib.directory.join("Capture")).unwrap().count(),
        0
    );
}

fn embedded_jpeg(path: &std::path::Path, metadata: &Metadata) {
    let mut packet = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    packet.extend(
        rawtonic_metadata::export_xmp(metadata, "Test")
            .unwrap()
            .as_bytes(),
    );
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
    jpeg.extend(u16::try_from(packet.len() + 2).unwrap().to_be_bytes());
    jpeg.extend(packet);
    jpeg.extend([0xff, 0xd9]);
    fs::write(path, jpeg).unwrap();
}

#[test]
fn embedded_rights_are_preserved_and_overwrite_is_explicit() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("embedded.jpg");
    let mut embedded = Metadata {
        creator: "Embedded creator".into(),
        copyright: "Embedded rights".into(),
        keywords: vec!["embedded".into()],
        ..Metadata::default()
    };
    embedded.set_iptc("usageTerms", "Embedded terms");
    embedded.set_iptc("creatorEmail", "existing@example.org");
    embedded_jpeg(&source, &embedded);
    let hash = rawtonic_ingest::sha256(&source).unwrap();
    let mut preset = ImportMetadata {
        creator: "Preset creator".into(),
        copyright: "Preset rights".into(),
        ..ImportMetadata::default()
    };
    preset
        .iptc
        .insert("usageTerms".into(), "Preset terms".into());
    for overwrite in [false, true] {
        if overwrite {
            preset
                .overwrite
                .extend(["creator".into(), "usageTerms".into()]);
        }
        let lib = Library::create(
            &d.path().join(format!("Library-{overwrite}")),
            "Library",
            Mode::Session,
        )
        .unwrap();
        let options = BatchOptions {
            template: "{orig}",
            sequence_start: 1,
            backup: None,
            folder: None,
            roots: &[],
            pair_raw: None,
            album: None,
            metadata: Some(&preset),
        };
        let results = import_batch(
            &lib,
            std::slice::from_ref(&source),
            &options,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let meta = lib.metadata(results[0].asset_id.as_ref().unwrap()).unwrap();
        assert_eq!(
            meta.creator,
            if overwrite {
                "Preset creator"
            } else {
                "Embedded creator"
            }
        );
        assert_eq!(meta.copyright, "Embedded rights");
        assert_eq!(
            meta.iptc["usageTerms"],
            if overwrite {
                "Preset terms"
            } else {
                "Embedded terms"
            }
        );
        assert_eq!(meta.iptc["creatorEmail"], "existing@example.org");
        assert_eq!(meta.keywords, ["embedded"]);
    }
    assert_eq!(rawtonic_ingest::sha256(&source).unwrap(), hash);
}

#[test]
fn add_preset_sets_catalog_pending_without_writing_any_source_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.jpg");
    fs::write(&source, b"photo").unwrap();
    let hash = rawtonic_ingest::sha256(&source).unwrap();
    let lib = Library::create(&d.path().join("Catalog"), "Catalog", Mode::Catalog).unwrap();
    let preset = ImportMetadata {
        creator: "Creator".into(),
        ..ImportMetadata::default()
    };
    let results = rawtonic_ingest::batch::add_batch_with_metadata(
        &lib,
        std::slice::from_ref(&source),
        None,
        |_| rawtonic_ingest::batch::Medium::Disk,
        &AtomicBool::new(false),
        Some(&preset),
        |done| {
            assert_eq!(
                lib.metadata(done.result.asset_id.as_ref().unwrap())
                    .unwrap()
                    .creator,
                "Creator"
            );
            assert!(!source.with_extension("xmp").exists());
        },
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(lib.summary().unwrap().xmp_pending, 1);
    assert_eq!(rawtonic_ingest::sha256(&source).unwrap(), hash);
}

#[test]
fn add_keeps_sidecar_digest_for_the_normal_later_sync() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.jpg");
    fs::write(&source, b"photo").unwrap();
    let xmp = source.with_extension("xmp");
    let original = Metadata {
        creator: "Original creator".into(),
        ..Metadata::default()
    };
    rawtonic_metadata::write_xmp(&xmp, &original, None, None).unwrap();
    let hash = rawtonic_ingest::sha256(&xmp).unwrap();
    let lib = Library::create(&d.path().join("Catalog"), "Catalog", Mode::Catalog).unwrap();
    let preset = ImportMetadata {
        copyright: "Rights".into(),
        ..ImportMetadata::default()
    };
    let results = rawtonic_ingest::batch::add_batch_with_metadata(
        &lib,
        std::slice::from_ref(&source),
        None,
        |_| rawtonic_ingest::batch::Medium::Disk,
        &AtomicBool::new(false),
        Some(&preset),
        |_| {},
    )
    .unwrap();
    let id = results[0].asset_id.as_ref().unwrap();
    assert_eq!(lib.summary().unwrap().xmp_pending, 1);
    assert_eq!(rawtonic_ingest::sha256(&xmp).unwrap(), hash);
    lib.sync_xmp(id).unwrap();
    let read = rawtonic_metadata::read_xmp(&fs::read_to_string(xmp).unwrap())
        .unwrap()
        .0;
    assert_eq!(read.creator, "Original creator");
    assert_eq!(read.copyright, "Rights");
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
}

#[test]
fn independent_same_stem_photos_keep_metadata_pending_and_report_xmp_conflict() {
    let d = tempfile::tempdir().unwrap();
    let raw = d.path().join("source.NEF");
    let jpeg = d.path().join("source.JPG");
    fs::write(&raw, b"raw").unwrap();
    fs::write(&jpeg, b"jpeg").unwrap();
    let lib = Library::create(&d.path().join("Library"), "Library", Mode::Session).unwrap();
    let preset = ImportMetadata {
        creator: "Creator".into(),
        ..ImportMetadata::default()
    };
    let options = BatchOptions {
        template: "{orig}",
        sequence_start: 1,
        backup: None,
        folder: None,
        roots: &[],
        pair_raw: None,
        album: None,
        metadata: Some(&preset),
    };
    let results = import_batch(
        &lib,
        &[raw, jpeg],
        &options,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.error.is_none()));
    for result in &results {
        assert_eq!(
            lib.metadata(result.asset_id.as_ref().unwrap())
                .unwrap()
                .creator,
            "Creator"
        );
    }
    assert!(
        results[1]
            .warning
            .as_ref()
            .unwrap()
            .contains("pair resolution")
    );
    assert_eq!(lib.summary().unwrap().xmp_pending, 1);
}

#[test]
fn capture_year_comes_from_exif_not_the_import_year() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("dated.NEF");
    // Minimal little-endian TIFF with IFD0 DateTime (camera file date).
    let mut tiff = b"II\x2a\0\x08\0\0\0\x01\0\x32\x01\x02\0\x14\0\0\0\x1a\0\0\0\0\0\0\0".to_vec();
    tiff.extend(b"2003:05:06 07:08:09\0");
    fs::write(&source, tiff).unwrap();
    let lib = Library::create(&d.path().join("Library"), "Library", Mode::Session).unwrap();
    let preset = ImportMetadata {
        copyright: "© {year}".into(),
        ..ImportMetadata::default()
    };
    let options = BatchOptions {
        template: "{orig}",
        sequence_start: 1,
        backup: None,
        folder: None,
        roots: &[],
        pair_raw: None,
        album: None,
        metadata: Some(&preset),
    };
    let results = import_batch(&lib, &[source], &options, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(
        lib.metadata(results[0].asset_id.as_ref().unwrap())
            .unwrap()
            .copyright,
        "© 2003"
    );
}
