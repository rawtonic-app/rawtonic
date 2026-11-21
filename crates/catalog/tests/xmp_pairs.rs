//! RAW+JPEG (and ARW+HIF) pairs with one stem: the RAW owns `<stem>.xmp`,
//! the developed image beside it has `<name>.<ext>.xmp`, and neither reads
//! or writes the other's (`rawtonic_metadata::sidecars`).

use rawtonic_catalog::{Library, Mode, SidecarStamps, XmpProblem};
use std::fs;
use std::path::{Path, PathBuf};

/// A Lightroom-written RAW sidecar: rating 5, Red, develop settings.
const LR_RAW_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="5" xmp:Label="Red" crs:Exposure2012="+1.00"/></rdf:RDF></x:xmpmeta>"#;

/// A sidecar written for the JPEG itself (darktable's full-name form).
const JPEG_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2"/></rdf:RDF></x:xmpmeta>"#;

fn session(dir: &Path, files: &[(&str, &[u8])]) -> (Library, PathBuf) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    for (name, bytes) in files {
        fs::write(capture.join(name), bytes).unwrap();
    }
    (lib, capture)
}

fn add(lib: &Library, photo: &Path) -> String {
    let report = lib.add_files(&[photo.to_path_buf()]).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    lib.asset_at(photo).unwrap().unwrap().id
}

fn rate(lib: &Library, id: &str, rating: u8) {
    let mut meta = lib.metadata(id).unwrap();
    meta.rating = rating;
    lib.set_metadata(id, &meta).unwrap();
}

fn rating_in(sidecar: &Path) -> u8 {
    let text = fs::read_to_string(sidecar).unwrap();
    rawtonic_metadata::read_xmp(&text).unwrap().0.rating
}

#[test]
fn a_jpeg_added_alone_never_adopts_or_overwrites_its_raws_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[
            ("IMG_1234.CR3", b"raw"),
            ("IMG_1234.JPG", b"jpg"),
            ("IMG_1234.xmp", LR_RAW_XMP.as_bytes()),
        ],
    );
    let jpg = capture.join("IMG_1234.JPG");
    let id = add(&lib, &jpg);
    let meta = lib.metadata(&id).unwrap();
    assert_eq!((meta.rating, meta.label.as_str()), (0, ""), "not the RAW's");
    rate(&lib, &id, 1);
    lib.sync_xmp(&id).unwrap();
    assert_eq!(rating_in(&capture.join("IMG_1234.JPG.xmp")), 1);
    assert_eq!(
        fs::read_to_string(capture.join("IMG_1234.xmp")).unwrap(),
        LR_RAW_XMP
    );
    assert_eq!(fs::read(&jpg).unwrap(), b"jpg");
    assert_eq!(fs::read(capture.join("IMG_1234.CR3")).unwrap(), b"raw");
}

#[test]
fn an_imported_lightroom_pair_gives_each_photo_its_own_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[
            ("DSC0001.ARW", b"raw"),
            ("DSC0001.HIF", b"hif"),
            ("DSC0001.xmp", LR_RAW_XMP.as_bytes()),
        ],
    );
    let raw = capture.join("DSC0001.ARW");
    let hif = capture.join("DSC0001.HIF");
    let report = lib.add_files(&[raw.clone(), hif.clone()]).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let raw_id = lib.asset_at(&raw).unwrap().unwrap().id;
    let hif_id = lib.asset_at(&hif).unwrap().unwrap().id;
    assert_eq!(lib.metadata(&raw_id).unwrap().rating, 5);
    assert_eq!(lib.metadata(&hif_id).unwrap().rating, 0);
    rate(&lib, &raw_id, 4);
    rate(&lib, &hif_id, 3);
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!((retry.written, retry.unsaved.len()), (2, 0), "{retry:?}");
    let stem = fs::read_to_string(capture.join("DSC0001.xmp")).unwrap();
    assert_eq!(rawtonic_metadata::read_xmp(&stem).unwrap().0.rating, 4);
    assert!(
        stem.contains("crs:Exposure2012"),
        "Lightroom's develop kept"
    );
    assert_eq!(rating_in(&capture.join("DSC0001.HIF.xmp")), 3);
    // Neither is a change made elsewhere for the other.
    let check = lib
        .check_xmp_sidecars(None, &mut SidecarStamps::default())
        .unwrap();
    assert!(check.changed.is_empty(), "{check:?}");
}

#[test]
fn a_jpegs_own_full_name_sidecar_is_adopted_and_never_silently_replaced() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[
            ("A.NEF", b"raw"),
            ("A.JPG", b"jpg"),
            ("A.xmp", LR_RAW_XMP.as_bytes()),
            ("A.JPG.xmp", JPEG_XMP.as_bytes()),
        ],
    );
    let id = add(&lib, &capture.join("A.JPG"));
    assert_eq!(lib.metadata(&id).unwrap().rating, 2);
    // Another program changes it: a conflict to review, not replaced.
    let edited = JPEG_XMP.replace("xmp:Rating=\"2\"", "xmp:Rating=\"4\"");
    fs::write(capture.join("A.JPG.xmp"), &edited).unwrap();
    rate(&lib, &id, 1);
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.written, 0);
    assert_eq!(retry.unsaved[0].problem, XmpProblem::ChangedElsewhere);
    assert_eq!(
        fs::read_to_string(capture.join("A.JPG.xmp")).unwrap(),
        edited
    );
    assert_eq!(
        fs::read_to_string(capture.join("A.xmp")).unwrap(),
        LR_RAW_XMP
    );
}

#[test]
fn a_lone_jpeg_keeps_the_stem_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[("B.JPG", b"jpg"), ("B.xmp", JPEG_XMP.as_bytes())],
    );
    let id = add(&lib, &capture.join("B.JPG"));
    assert_eq!(lib.metadata(&id).unwrap().rating, 2);
    rate(&lib, &id, 5);
    lib.sync_xmp(&id).unwrap();
    assert_eq!(rating_in(&capture.join("B.xmp")), 5);
    assert!(!capture.join("B.JPG.xmp").exists());
}

#[test]
fn two_raws_of_one_stem_still_refuse_to_share_a_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[
            ("C.CR3", b"cr3"),
            ("C.DNG", b"dng"),
            ("C.xmp", LR_RAW_XMP.as_bytes()),
        ],
    );
    let ids: Vec<String> = ["C.CR3", "C.DNG"]
        .iter()
        .map(|name| add(&lib, &capture.join(name)))
        .collect();
    for id in &ids {
        rate(&lib, id, 1);
        assert!(lib.sync_xmp(id).is_err());
    }
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.written, 0);
    assert!(
        retry
            .unsaved
            .iter()
            .all(|u| u.problem == XmpProblem::SharedName),
        "{retry:?}"
    );
    assert_eq!(
        fs::read_to_string(capture.join("C.xmp")).unwrap(),
        LR_RAW_XMP
    );
}

/// Libraries before the pair rule read a JPEG's XMP from the stem sidecar;
/// here the JPEG was added alone and its RAW arrived afterwards. Opening the
/// library touches no file: the digest of the RAW's sidecar is not taken for
/// the JPEG's going missing, and the JPEG's next write goes to its own.
#[test]
fn a_jpeg_holding_its_raws_digest_from_before_the_pair_rule_moves_to_its_own() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[("D.JPG", b"jpg"), ("D.xmp", LR_RAW_XMP.as_bytes())],
    );
    let id = add(&lib, &capture.join("D.JPG"));
    assert_eq!(
        lib.metadata(&id).unwrap().rating,
        5,
        "a lone JPEG's sidecar"
    );
    // Its RAW arrives (copied in from a backup): `D.xmp` is the RAW's now.
    fs::write(capture.join("D.RAF"), b"raw").unwrap();
    let check = lib
        .check_xmp_sidecars(None, &mut SidecarStamps::default())
        .unwrap();
    assert!(check.changed.is_empty(), "{check:?}");
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
    // Its values stay; an edit of its own goes to `D.JPG.xmp`.
    rate(&lib, &id, 3);
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!((retry.written, retry.unsaved.len()), (1, 0), "{retry:?}");
    assert_eq!(rating_in(&capture.join("D.JPG.xmp")), 3);
    assert_eq!(
        fs::read_to_string(capture.join("D.xmp")).unwrap(),
        LR_RAW_XMP
    );
    // The RAW added now adopts its own sidecar untouched.
    let raw = add(&lib, &capture.join("D.RAF"));
    assert_eq!(lib.metadata(&raw).unwrap().rating, 5);
}

/// Both photos of a pair hold the stem sidecar's digest (a JPEG added
/// alone before the pair rule, its RAW added later); the RAW writes first.
/// The JPEG is not taken for having lost a sidecar, and writes its own.
#[test]
fn a_raw_writing_first_releases_the_digest_its_jpeg_held_from_before_the_pair_rule() {
    let d = tempfile::tempdir().unwrap();
    let (lib, capture) = session(
        d.path(),
        &[("E.JPG", b"jpg"), ("E.xmp", LR_RAW_XMP.as_bytes())],
    );
    let jpg = add(&lib, &capture.join("E.JPG"));
    fs::write(capture.join("E.ARW"), b"raw").unwrap();
    let raw = add(&lib, &capture.join("E.ARW"));
    rate(&lib, &raw, 2);
    lib.sync_xmp(&raw).unwrap();
    assert_eq!(rating_in(&capture.join("E.xmp")), 2);
    let check = lib
        .check_xmp_sidecars(None, &mut SidecarStamps::default())
        .unwrap();
    assert!(check.changed.is_empty(), "{check:?}");
    rate(&lib, &jpg, 4);
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!((retry.written, retry.unsaved.len()), (1, 0), "{retry:?}");
    assert_eq!(rating_in(&capture.join("E.JPG.xmp")), 4);
    assert_eq!(rating_in(&capture.join("E.xmp")), 2);
}
