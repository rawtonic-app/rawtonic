//! Folders › "Add Folder…" in a catalog (G39 AF): the folder's photographs
//! are planned (`folder_add::plan_folder_add`) and registered where they are
//! through the one Add path (`batch::add_batch`).

use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::batch::{Medium, add_batch};
use rawtonic_ingest::folder_add::{FolderPlan, plan_folder_add};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

fn raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["nef", "arw", "cr3", "dng"].contains(&e.to_ascii_lowercase().as_str()))
}

fn supported(path: &Path) -> bool {
    raw(path)
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["jpg", "jpeg"].contains(&e.to_ascii_lowercase().as_str()))
}

fn write(path: &Path, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

fn rated_xmp(rating: u8) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/></rdf:RDF></x:xmpmeta>"#
    )
}

/// Every file below `root` with its SHA-256.
fn hashes(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), rawtonic_ingest::sha256(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

/// 200 files in nested folders: 140 lone RAWs, 25 RAW+JPEG pairs (50
/// files), 10 lone JPEGs; every tenth RAW has a rated XMP; plus a text
/// file the app does not show.
fn shoot(root: &Path) -> usize {
    let mut n = 0usize;
    let mut file = |path: PathBuf| {
        n += 1;
        // Distinct content per file, so no file is a duplicate of another.
        let mut bytes = vec![(n % 251) as u8; 1_000 + n];
        bytes.extend_from_slice(&n.to_le_bytes());
        write(&path, &bytes);
    };
    for i in 0..140 {
        let folder = root.join(format!("2026/{:02}/day{}", 1 + i % 4, i % 3));
        let photo = folder.join(format!("DSC_{i:04}.NEF"));
        if i % 10 == 0 {
            write(&photo.with_extension("xmp"), rated_xmp(3).as_bytes());
        }
        file(photo);
    }
    for i in 0..25 {
        let folder = root.join(format!("2026/pairs/{}", i % 2));
        file(folder.join(format!("P_{i:04}.ARW")));
        file(folder.join(format!("P_{i:04}.JPG")));
    }
    for i in 0..10 {
        file(root.join(format!("jpegs/J_{i:04}.jpg")));
    }
    write(&root.join("notes.txt"), b"not a photograph");
    n
}

fn plan(lib: &Library, folder: &Path, subfolders: bool) -> FolderPlan {
    plan_folder_add(lib, folder, subfolders, supported, Some(raw), 20_000).unwrap()
}

fn add(lib: &Library, files: &[PathBuf], cancel: &AtomicBool, mut each: impl FnMut(usize)) {
    let results = add_batch(
        lib,
        files,
        Some(raw),
        |_| Medium::Disk,
        cancel,
        |done| each(done.index),
    )
    .unwrap();
    assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
}

#[test]
fn adding_a_folder_registers_its_photographs_in_place_and_again_skips_them() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let drive = d.path().join("Drive");
    assert_eq!(shoot(&drive), 200);
    let before = hashes(&drive);

    let first = plan(&lib, &drive, true);
    assert_eq!(first.files.len(), 200);
    assert_eq!(first.photographs, 175, "a pair is one photograph");
    assert_eq!((first.registered, first.truncated), (0, false));
    assert!(first.owned.is_empty());
    add(&lib, &first.files, &AtomicBool::new(false), |_| {});

    // Every photograph is in the library at once: the count, the pairs,
    // the ratings from the XMP sidecars.
    assert_eq!(lib.summary().unwrap().assets, 175);
    let pair = lib
        .asset_at(&drive.join("2026/pairs/0/P_0000.ARW"))
        .unwrap()
        .unwrap();
    assert!(lib.companion(&pair).unwrap().is_some());
    let rated = lib
        .asset_at(&drive.join("2026/01/day0/DSC_0000.NEF"))
        .unwrap()
        .unwrap();
    assert_eq!(rated.rating, 3, "sidecar XMP adopted");
    let batch = lib.previous_import().unwrap().unwrap();
    assert_eq!(
        (batch.kind, batch.count),
        (rawtonic_catalog::ImportKind::Add, 175)
    );

    // Adding the folder again: nothing left, all counted as registered.
    let again = plan(&lib, &drive, true);
    assert!(again.files.is_empty(), "{:?}", again.files);
    assert_eq!(again.registered, 200);
    // Nothing was written into the folder: names, bytes and SHA-256.
    assert_eq!(hashes(&drive), before);
}

#[test]
fn without_subfolders_only_the_folders_own_files_are_planned() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let drive = d.path().join("Drive");
    shoot(&drive);
    let top = drive.join("2026/pairs/0");
    let only = plan(&lib, &top, false);
    assert_eq!(only.files.len(), 26);
    assert_eq!(only.photographs, 13);
    assert!(plan(&lib, &drive, false).files.is_empty(), "notes.txt only");
}

#[test]
fn files_inside_another_library_are_refused_with_its_name() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let drive = d.path().join("Drive");
    write(&drive.join("loose/A.NEF"), b"loose raw");
    let session = Library::create(&drive.join("Wedding"), "Wedding Smith", Mode::Session).unwrap();
    write(&session.directory.join("Capture/B.NEF"), b"session raw 1");
    write(&session.directory.join("Capture/C.NEF"), b"session raw 2");
    // The catalog's own folder lists its photographs itself.
    write(&lib.directory.join("Capture/D.NEF"), b"own raw");

    let planned = plan(&lib, d.path(), true);
    assert_eq!(
        planned.files,
        vec![drive.join("loose/A.NEF").canonicalize().unwrap()]
    );
    assert_eq!(planned.owned.len(), 1);
    assert_eq!(planned.owned[0].name, "Wedding Smith");
    assert_eq!(planned.owned[0].count, 2);
}

#[test]
fn a_cancel_leaves_every_photograph_whole_and_the_rest_for_the_next_add() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let drive = d.path().join("Drive");
    shoot(&drive);
    let before = hashes(&drive);
    let first = plan(&lib, &drive, true);
    let cancel = AtomicBool::new(false);
    add(&lib, &first.files, &cancel, |index| {
        if index == 150 {
            cancel.store(true, Ordering::Release);
        }
    });
    let added = lib.summary().unwrap().assets;
    assert!(added > 0 && added < 175, "{added}");
    // No pair is half in: a registered RAW with a JPEG beside it has it as
    // its companion, never as a photograph of its own.
    for asset in lib.assets(None).unwrap() {
        let jpeg = asset.path.with_extension("JPG");
        if raw(&asset.path) && jpeg.is_file() {
            assert!(
                lib.companion(&asset).unwrap().is_some(),
                "{}",
                asset.path.display()
            );
            assert!(lib.asset_at(&jpeg).unwrap().is_none());
        }
    }
    // The next Add takes the rest and finishes the folder.
    let rest = plan(&lib, &drive, true);
    assert_eq!(rest.files.len() + rest.registered, 200);
    add(&lib, &rest.files, &AtomicBool::new(false), |_| {});
    assert_eq!(lib.summary().unwrap().assets, 175);
    assert_eq!(hashes(&drive), before);
}

#[test]
fn a_session_cannot_add_a_folder_in_place() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    write(&d.path().join("Drive/A.NEF"), b"raw");
    assert!(
        plan_folder_add(
            &lib,
            &d.path().join("Drive"),
            true,
            supported,
            Some(raw),
            10
        )
        .is_err()
    );
}

/// Time to plan and add 1,000 photographs in place (database work and file
/// reads of the Add path; no previews, no hash pass). `--ignored` to run.
#[test]
#[ignore]
fn time_adding_a_thousand_files() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let drive = d.path().join("Drive");
    for i in 0..1_000usize {
        let mut bytes = vec![(i % 251) as u8; 2_000_000];
        bytes.extend_from_slice(&i.to_le_bytes());
        write(
            &drive.join(format!("{:02}/DSC_{i:04}.NEF", i / 100)),
            &bytes,
        );
    }
    let started = std::time::Instant::now();
    let planned = plan(&lib, &drive, true);
    let planning = started.elapsed();
    add(&lib, &planned.files, &AtomicBool::new(false), |_| {});
    let total = started.elapsed();
    assert_eq!(lib.summary().unwrap().assets, 1_000);
    let started = std::time::Instant::now();
    let again = plan(&lib, &drive, true);
    eprintln!(
        "folder add 1000: plan {:.2} s, plan+add {:.2} s, re-plan (all registered) {:.2} s",
        planning.as_secs_f64(),
        total.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    assert_eq!(again.registered, 1_000);
}

/// Review r1: JPEGs put beside RAWs added earlier pair with them on the next
/// Add Folder… instead of becoming photographs of their own.
#[test]
fn a_jpeg_added_later_joins_its_registered_raw() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let shoot = d.path().join("Drive/Shoot");
    write(&shoot.join("A.NEF"), b"raw a");
    write(&shoot.join("B.NEF"), b"raw b");
    let first = plan(&lib, &shoot, true);
    add(&lib, &first.files, &AtomicBool::new(false), |_| {});
    write(&shoot.join("A.JPG"), b"jpeg a");
    write(&shoot.join("C.JPG"), b"jpeg c");
    let again = plan(&lib, &shoot, true);
    assert_eq!((again.photographs, again.companions), (1, 1), "{again:?}");
    add(&lib, &again.files, &AtomicBool::new(false), |_| {});
    assert_eq!(lib.summary().unwrap().assets, 3, "A (with its JPEG), B, C");
    let a = lib.asset_at(&shoot.join("A.NEF")).unwrap().unwrap();
    assert!(lib.companion(&a).unwrap().is_some());
    assert!(lib.asset_at(&shoot.join("A.JPG")).unwrap().is_none());
    assert!(plan(&lib, &shoot, true).files.is_empty());
}

/// Review r1: a copy of a photograph the catalog holds elsewhere counts as
/// held, so a scan cut at the limit by such copies still reaches new files.
#[test]
fn copies_of_held_photographs_do_not_fill_the_limit() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let originals = d.path().join("Drive/Originals");
    for i in 0..3 {
        write(
            &originals.join(format!("O_{i}.NEF")),
            format!("photo {i}").as_bytes(),
        );
    }
    add(
        &lib,
        &plan(&lib, &originals, true).files,
        &AtomicBool::new(false),
        |_| {},
    );
    let backup = d.path().join("Drive/Backup");
    for i in 0..3 {
        write(
            &backup.join(format!("A_{i}.NEF")),
            format!("photo {i}").as_bytes(),
        );
    }
    write(&backup.join("Z_new.NEF"), b"a new photo");
    let planned = plan_folder_add(&lib, &backup, true, supported, Some(raw), 2).unwrap();
    assert_eq!(
        planned.files,
        vec![backup.join("Z_new.NEF").canonicalize().unwrap()]
    );
    assert_eq!(planned.registered, 3);
}

/// Review r1: a folder that cannot be read is named, never passed over as
/// if it were empty; an unreadable chosen folder is an error.
#[cfg(unix)]
#[test]
fn unreadable_folders_are_reported() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
    let shoot = d.path().join("Drive/Shoot");
    write(&shoot.join("A.NEF"), b"raw a");
    let locked = shoot.join("locked");
    write(&locked.join("B.NEF"), b"raw b");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&locked).is_ok(); // running as root
    let planned = plan_folder_add(&lib, &shoot, true, supported, Some(raw), 100);
    let whole = plan_folder_add(&lib, &locked, true, supported, Some(raw), 100);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return;
    }
    let planned = planned.unwrap();
    assert_eq!(planned.files.len(), 1);
    assert_eq!(planned.unreadable, vec![locked.canonicalize().unwrap()]);
    assert!(whole.unwrap_err().to_string().contains("cannot be read"));
}
