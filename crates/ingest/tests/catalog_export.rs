//! Export as Catalog (G39): a new catalog with some photographs of a library
//! and everything that is theirs; the source is never changed and a cancel
//! or failure never leaves a half catalog.
use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::catalog_export::{Options, export_catalog};
use rawtonic_metadata::Metadata;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn go() -> AtomicBool {
    AtomicBool::new(false)
}

fn sha(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn options(include_originals: bool) -> Options {
    Options {
        name: "Job".into(),
        include_originals,
        leave_out_owned_elsewhere: false,
    }
}

fn no_stage(
    _: &Path,
    _: &[rawtonic_ingest::catalog_export::Exported],
    _: &AtomicBool,
) -> anyhow::Result<usize> {
    Ok(0)
}

fn leftovers(parent: &Path) -> Vec<String> {
    fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".rawtonic-export-"))
        .collect()
}

/// A catalog of three external photographs with edits, history, a virtual
/// copy, metadata and albums (one group holds the album of the third).
struct Source {
    lib: Library,
    ids: Vec<String>,
    files: Vec<PathBuf>,
    root: PathBuf,
}

fn source(root: &Path) -> Source {
    let lib = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let shoot = root.join("Shoot");
    fs::create_dir_all(&shoot).unwrap();
    let mut ids = Vec::new();
    let mut files = Vec::new();
    for (index, name) in ["wedding-a.arw", "wedding-b.arw", "private-qxjz.arw"]
        .iter()
        .enumerate()
    {
        let path = shoot.join(name);
        fs::write(&path, format!("original {index}")).unwrap();
        ids.push(lib.add_asset(&path).unwrap().id);
        files.push(path);
    }
    fs::write(files[0].with_extension("xmp"), b"foreign xmp").unwrap();
    let lut = shoot.join("look.cube");
    fs::write(&lut, b"LUT").unwrap();
    for (n, id) in ids.iter().enumerate() {
        let first = json!({"rating": 3, "adjustments": {"exposure": 0.5}});
        lib.save_editor_state(id, &first).unwrap();
        let later =
            json!({"rating": 3, "adjustments": {"exposure": 1.0 + n as f64, "lutPath": lut}});
        lib.save_editor_state(id, &later).unwrap();
        lib.set_metadata(
            id,
            &Metadata {
                rating: 4,
                label: "red".into(),
                flag: 1,
                keywords: vec!["Bride".into(), format!("n{n}")],
                title: "Ceremony".into(),
                ..Metadata::default()
            },
        )
        .unwrap();
    }
    lib.save_copy_state(&ids[0], "abcdef", &json!({"adjustments": {"exposure": -1}}))
        .unwrap();
    let album = lib.create_collection("Ceremony", None).unwrap();
    lib.add_to_collection(&album.id, &ids[..2]).unwrap();
    let group = lib
        .create_collection_of_kind("Private", None, rawtonic_catalog::GROUP)
        .unwrap();
    let hidden = lib
        .create_collection("Third only", Some(&group.id))
        .unwrap();
    lib.add_to_collection(&hidden.id, &ids[2..]).unwrap();
    Source {
        lib,
        ids,
        files,
        root: root.to_path_buf(),
    }
}

#[test]
fn exports_only_the_chosen_photographs_with_everything_that_is_theirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let src = source(&root);
    let originals: Vec<String> = src.files.iter().map(|f| sha(f)).collect();
    let source_db = sha(&src.lib.directory.join("catalog.sqlite"));
    let before = src.lib.collections().unwrap();
    let to = root.join("Job");
    let receipt = export_catalog(
        &src.lib.directory,
        &src.ids[..2],
        &to,
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(receipt.photographs, 2);
    assert_eq!(receipt.originals_copied, 0);

    let job = Library::open(&to).unwrap();
    assert_eq!(job.manifest.mode, Mode::Catalog);
    assert_eq!(job.manifest.name, "Job");
    assert_ne!(job.manifest.id, src.lib.manifest.id);
    let mut ids: Vec<String> = job
        .assets(None)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    ids.sort();
    let mut want = src.ids[..2].to_vec();
    want.sort();
    assert_eq!(ids, want, "the same asset ids, only the chosen ones");
    for (n, id) in src.ids[..2].iter().enumerate() {
        let asset = job.asset(id).unwrap();
        assert_eq!(
            asset.path, src.files[n],
            "by reference: the original where it is"
        );
        assert!(!asset.missing);
        let state = job.editor_state(id).unwrap().unwrap();
        assert_eq!(state["adjustments"]["exposure"], json!(1.0 + n as f64));
        // The LUT travels with the catalog.
        let lut = Path::new(state["adjustments"]["lutPath"].as_str().unwrap());
        assert!(
            lut.starts_with(job.directory.as_path()),
            "{}",
            lut.display()
        );
        assert_eq!(fs::read(lut).unwrap(), b"LUT");
        let metadata = job.metadata(id).unwrap();
        assert_eq!(metadata, src.lib.metadata(id).unwrap());
        assert_eq!(metadata.keywords[1], format!("n{n}"));
    }
    // History came along: undo returns to the first state.
    assert!(
        job.undo(&job.primary_version(&src.ids[0]).unwrap())
            .unwrap()
    );
    assert_eq!(
        job.editor_state(&src.ids[0]).unwrap().unwrap()["adjustments"]["exposure"],
        json!(0.5)
    );
    assert_eq!(
        job.copy_state(&src.ids[0], "abcdef").unwrap().unwrap()["adjustments"]["exposure"],
        json!(-1)
    );
    // Albums restricted to the exported photographs; the group that only
    // held the third photograph's album went with it.
    let albums = job.collections().unwrap();
    assert_eq!(albums.len(), 1, "{albums:?}");
    assert_eq!(albums[0].name, "Ceremony");
    assert_eq!(albums[0].count, 2);
    // Nothing of the third photograph is left in the database file.
    drop(job);
    let bytes = fs::read(to.join("catalog.sqlite")).unwrap();
    assert!(!bytes.windows(12).any(|w| w == b"private-qxjz"));
    // Nor in the search index: deleted full-text entries are only marked
    // until their index segments merge, so their trigrams ("qxj", "xjz")
    // would still be in the file.
    for trigram in [b"qxj", b"xjz"] {
        assert!(
            !bytes.windows(3).any(|w| w == trigram),
            "{}",
            String::from_utf8_lossy(trigram)
        );
    }
    assert!(!bytes.windows(10).any(|w| w == b"Third only"));
    // The source and the originals are unchanged.
    assert_eq!(src.lib.assets(None).unwrap().len(), 3);
    assert_eq!(src.lib.collections().unwrap(), before);
    assert_eq!(sha(&src.lib.directory.join("catalog.sqlite")), source_db);
    for (file, digest) in src.files.iter().zip(&originals) {
        assert_eq!(&sha(file), digest);
    }
    assert!(leftovers(&src.root).is_empty());
}

#[test]
fn originals_are_copied_verified_and_relative_so_the_catalog_folder_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // A session: the result is a catalog all the same.
    let session = Library::create(&root.join("Session"), "Session", Mode::Session).unwrap();
    let mut ids = Vec::new();
    for name in ["a.arw", "b.arw"] {
        let path = session.directory.join("Capture").join(name);
        fs::write(&path, format!("session {name}")).unwrap();
        let id = session.add_asset(&path).unwrap().id;
        session
            .save_editor_state(&id, &json!({"adjustments": {"contrast": 12}}))
            .unwrap();
        ids.push(id);
    }
    fs::write(session.directory.join("Capture/a.XMP"), b"a xmp").unwrap();
    let digests: Vec<String> = ids
        .iter()
        .map(|id| sha(&session.asset(id).unwrap().path))
        .collect();
    let seen = std::cell::RefCell::new(Vec::new());
    let to = root.join("Laptop job");
    let receipt = export_catalog(
        &session.directory,
        &ids,
        &to,
        &options(true),
        &go(),
        |p| seen.borrow_mut().push(p.phase),
        |stage, exported, _| {
            // The stage step sees each photograph's copy inside the stage.
            for e in exported {
                assert!(e.to.starts_with(stage) && e.to.is_file());
                assert!(e.from.starts_with(&session.directory));
            }
            Ok(exported.len())
        },
    )
    .unwrap();
    assert_eq!(receipt.originals_copied, 2);
    assert_eq!(receipt.previews, 2);
    assert!(seen.borrow().contains(&"originals"));
    // Another computer: the folder moved, the source gone.
    let moved = root.join("elsewhere");
    fs::rename(&to, &moved).unwrap();
    let source_dir = session.directory.clone();
    drop(session);
    fs::rename(&source_dir, root.join("gone")).unwrap();
    let job = Library::open(&moved).unwrap();
    assert_eq!(job.manifest.mode, Mode::Catalog);
    assert_eq!(job.verify_paths().unwrap(), 0);
    for (id, digest) in ids.iter().zip(&digests) {
        let asset = job.asset(id).unwrap();
        assert!(asset.path.starts_with(&moved), "{}", asset.path.display());
        assert!(asset.path.starts_with(moved.join("Photos/Capture")));
        assert_eq!(&sha(&asset.path), digest);
        assert_eq!(
            job.editor_state(id).unwrap().unwrap()["adjustments"]["contrast"],
            json!(12)
        );
    }
    assert_eq!(
        fs::read(moved.join("Photos/Capture/a.XMP")).unwrap(),
        b"a xmp"
    );
    // The originals in the (moved) session are untouched.
    for (name, digest) in ["a.arw", "b.arw"].iter().zip(&digests) {
        assert_eq!(&sha(&root.join("gone/Capture").join(name)), digest);
    }
}

#[test]
fn folders_of_the_same_name_stay_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let mut ids = Vec::new();
    for day in ["2026/Day 1", "2025/Day 1"] {
        let folder = root.join(day);
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join("same.arw");
        fs::write(&file, day).unwrap();
        ids.push(lib.add_asset(&file).unwrap().id);
    }
    let to = root.join("Out");
    export_catalog(
        &lib.directory,
        &ids,
        &to,
        &options(true),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    let job = Library::open(&to).unwrap();
    let mut places: Vec<String> = ids
        .iter()
        .map(|id| fs::read_to_string(job.asset(id).unwrap().path).unwrap())
        .collect();
    places.sort();
    assert_eq!(places, ["2025/Day 1", "2026/Day 1"]);
    assert!(to.join("Photos/Day 1/same.arw").is_file());
    assert!(to.join("Photos/Day 1 (2)/same.arw").is_file());
}

#[test]
fn cancel_or_failure_leaves_no_catalog_and_existing_folders_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let src = source(&root);
    let to = root.join("Job");
    // Cancel while copying originals.
    let cancel = go();
    let error = export_catalog(
        &src.lib.directory,
        &src.ids,
        &to,
        &options(true),
        &cancel,
        |_| cancel.store(true, Ordering::Release),
        no_stage,
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    assert!(!to.exists());
    assert!(leftovers(&root).is_empty());
    // A failure in the last step before publishing.
    assert!(
        export_catalog(
            &src.lib.directory,
            &src.ids,
            &to,
            &options(false),
            &go(),
            |_| {},
            |_, _, _| { anyhow::bail!("disk full") }
        )
        .is_err()
    );
    assert!(!to.exists());
    assert!(leftovers(&root).is_empty());
    // An unknown photograph: nothing is made.
    assert!(
        export_catalog(
            &src.lib.directory,
            &["nope".to_string()],
            &to,
            &options(false),
            &go(),
            |_| {},
            no_stage
        )
        .is_err()
    );
    assert!(!to.exists());
    // A folder with something in it is never used.
    fs::create_dir(&to).unwrap();
    fs::write(to.join("keep.txt"), b"mine").unwrap();
    assert!(
        export_catalog(
            &src.lib.directory,
            &src.ids,
            &to,
            &options(false),
            &go(),
            |_| {},
            no_stage
        )
        .unwrap_err()
        .to_string()
        .contains("not an empty folder")
    );
    assert_eq!(fs::read(to.join("keep.txt")).unwrap(), b"mine");
    // An empty one is.
    fs::remove_file(to.join("keep.txt")).unwrap();
    export_catalog(
        &src.lib.directory,
        &src.ids,
        &to,
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(Library::open(&to).unwrap().assets(None).unwrap().len(), 3);
    // Not inside the source library.
    assert!(
        export_catalog(
            &src.lib.directory,
            &src.ids,
            &src.lib.directory.join("Output/sub"),
            &options(false),
            &go(),
            |_| {},
            no_stage
        )
        .is_err()
    );
}

/// Killed while building (G66 app kill test): the stage it left beside the
/// destination holds copies only; the next export into that folder removes
/// it and says so, while a folder of that prefix without the marker, or one
/// a running export holds, stays.
#[test]
fn the_next_export_removes_what_a_killed_export_left() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let src = source(&root);
    let killed = root.join(".rawtonic-export-KiLLed");
    fs::create_dir_all(killed.join("Photos/Shoot")).unwrap();
    fs::write(
        killed.join(rawtonic_metadata::staging::MARKER),
        b"Export as Catalog",
    )
    .unwrap();
    fs::write(killed.join("Photos/Shoot/wedding-a.arw"), b"original 0").unwrap();
    let unmarked = root.join(".rawtonic-export-0ldVer");
    fs::create_dir_all(&unmarked).unwrap();
    let receipt = export_catalog(
        &src.lib.directory,
        &src.ids,
        &root.join("Job"),
        &options(true),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert!(!killed.exists());
    assert_eq!(receipt.abandoned.folders, 1);
    assert_eq!(
        receipt.abandoned.bytes,
        10 + "Export as Catalog".len() as u64
    );
    assert_eq!(receipt.abandoned.kept, vec![unmarked.clone()]);
    assert!(unmarked.exists());
    assert!(root.join("Job/catalog.sqlite").exists());
    assert!(
        !root
            .join("Job")
            .join(rawtonic_metadata::staging::MARKER)
            .exists(),
        "the published catalog carries no marker"
    );
}

#[test]
fn an_offline_original_is_exported_by_reference_but_not_copied() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let src = source(&root);
    fs::rename(&src.files[1], root.join("away.arw")).unwrap();
    let to = root.join("Copy");
    let error = export_catalog(
        &src.lib.directory,
        &src.ids,
        &to,
        &options(true),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("Originals not available (1)"), "{error}");
    assert!(!to.exists());
    let receipt = export_catalog(
        &src.lib.directory,
        &src.ids,
        &to,
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(receipt.offline, 1);
    let job = Library::open(&to).unwrap();
    assert!(job.asset(&src.ids[1]).unwrap().missing);
    assert!(!job.asset(&src.ids[0]).unwrap().missing);
}

#[test]
fn own_photographs_are_copied_and_another_librarys_never_exported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // A session's photographs belong to it: a catalog referring to them
    // would edit them in the session's name.
    let session = Library::create(&root.join("Session"), "S", Mode::Session).unwrap();
    let file = session.directory.join("Capture/a.arw");
    fs::write(&file, b"session original").unwrap();
    let id = session.add_asset(&file).unwrap().id;
    // A catalog's own copy and a photograph outside it.
    let main = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let own = main.directory.join("Photos/own.arw");
    fs::write(&own, b"catalog copy").unwrap();
    let outside = root.join("Shoot/out.arw");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::write(&outside, b"outside").unwrap();
    let own_id = main.add_asset(&own).unwrap().id;
    let out_id = main.add_asset(&outside).unwrap().id;
    // Added where it is from another session's Capture folder.
    let other = Library::create(&root.join("Other session"), "O", Mode::Session).unwrap();
    let theirs = other.directory.join("Capture/theirs.arw");
    fs::write(&theirs, b"another session's").unwrap();
    let theirs_id = main.add_asset(&theirs).unwrap().id;
    let receipt = export_catalog(
        &session.directory,
        std::slice::from_ref(&id),
        &root.join("A"),
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(receipt.originals_copied, 1);
    let a = Library::open(&root.join("A")).unwrap();
    assert!(a.asset(&id).unwrap().path.starts_with(&a.directory));
    // Its latest edit is in that session, not here: never exported. The
    // export is refused, or leaves it out when asked.
    let in_other = other.add_asset(&theirs).unwrap().id;
    other
        .save_editor_state(&in_other, &json!({"adjustments": {"exposure": 2}}))
        .unwrap();
    let chosen = [own_id.clone(), out_id.clone(), theirs_id.clone()];
    let owned = rawtonic_ingest::catalog_export::owned_elsewhere(&main, &chosen).unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].library, other.directory);
    // Named as the user knows it ("O"), not by its folder.
    assert_eq!(owned[0].name, "O");
    let error = export_catalog(
        &main.directory,
        &chosen,
        &root.join("B"),
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("belong to another project or catalog") && error.contains("(O)"),
        "{error}"
    );
    assert!(!root.join("B").exists());
    let only_theirs = std::slice::from_ref(&theirs_id);
    let mut leave_out = options(false);
    leave_out.leave_out_owned_elsewhere = true;
    assert!(
        export_catalog(
            &main.directory,
            only_theirs,
            &root.join("B"),
            &leave_out,
            &go(),
            |_| {},
            no_stage
        )
        .is_err()
    );
    let receipt = export_catalog(
        &main.directory,
        &chosen,
        &root.join("B"),
        &leave_out,
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(
        (
            receipt.photographs,
            receipt.left_out,
            receipt.originals_copied
        ),
        (2, 1, 1)
    );
    let b = Library::open(&root.join("B")).unwrap();
    assert!(b.asset(&own_id).unwrap().path.starts_with(&b.directory));
    assert!(b.asset(&theirs_id).is_err());
    assert_eq!(fs::read(&theirs).unwrap(), b"another session's");
    assert_eq!(b.asset(&out_id).unwrap().path, outside);
    assert_eq!(fs::read(&file).unwrap(), b"session original");
}

#[test]
fn an_exported_catalog_exports_again_with_its_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let src = source(&root);
    let first = root.join("First");
    export_catalog(
        &src.lib.directory,
        &src.ids[..1],
        &first,
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    // The LUT is the first catalog's own now (a relative reference).
    let second = root.join("Second");
    let receipt = export_catalog(
        &first,
        &src.ids[..1],
        &second,
        &options(false),
        &go(),
        |_| {},
        no_stage,
    )
    .unwrap();
    assert_eq!(receipt.missing_resources, 0);
    let job = Library::open(&second).unwrap();
    let state = job.editor_state(&src.ids[0]).unwrap().unwrap();
    let lut = Path::new(state["adjustments"]["lutPath"].as_str().unwrap());
    assert!(lut.starts_with(&second), "{}", lut.display());
    assert_eq!(fs::read(lut).unwrap(), b"LUT");
}

/// Nothing waits to be written right after an export: a photograph whose
/// only metadata is its camera's own rating gets no editor sidecar, which
/// opening the catalog would adopt as the library's metadata (and so mark
/// its XMP as not updated). Photographs with the library's own work keep
/// theirs, and their XMP state goes along.
#[test]
fn a_fresh_export_has_no_xmp_waiting() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let photos = main.directory.join("Photos");
    fs::create_dir_all(&photos).unwrap();
    let mut ids = Vec::new();
    for name in ["camera.jpg", "rated.jpg", "plain.jpg"] {
        let path = photos.join(name);
        fs::write(&path, name).unwrap();
        ids.push(main.add_asset(&path).unwrap().id);
    }
    // The camera's rating (facts pass) and one set in the library.
    main.record_facts(
        &ids[0],
        &rawtonic_catalog::CaptureFacts {
            camera_rating: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    main.set_metadata(
        &ids[1],
        &Metadata {
            rating: 4,
            ..Metadata::default()
        },
    )
    .unwrap();
    assert!(main.write_pending_xmp().unwrap().unsaved.is_empty());
    assert_eq!(main.summary().unwrap().xmp_pending, 0);
    for originals in [false, true] {
        let to = root.join(format!("Export {originals}"));
        export_catalog(
            &main.directory,
            &ids,
            &to,
            &options(originals),
            &go(),
            |_| {},
            no_stage,
        )
        .unwrap();
        let new = Library::open(&to).unwrap();
        // What opening it does: untouched photographs adopt their sidecars.
        for id in &ids {
            let path = new.asset(id).unwrap().path;
            let contents = rawtonic_catalog::Contents::read_sidecars(&path).unwrap();
            new.adopt_contents(id, &contents).unwrap();
        }
        assert_eq!(
            new.summary().unwrap().xmp_pending,
            0,
            "originals {originals}"
        );
        assert_eq!(new.metadata(&ids[0]).unwrap().rating, 3);
        assert_eq!(new.metadata(&ids[1]).unwrap().rating, 4);
        let camera = new.asset(&ids[0]).unwrap().path;
        assert!(!camera.with_file_name("camera.jpg.rrdata").exists());
        assert!(camera.with_file_name("rated.jpg.rrdata").exists());
    }
}
