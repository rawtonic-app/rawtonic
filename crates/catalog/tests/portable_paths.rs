//! The boundary between native paths and the portable paths a library
//! stores: a stored path is relative, uses `/` on every platform, and is
//! the only form the catalogue ever queries or writes.
use anyhow::Result;
use rawtonic_catalog::{Library, Mode, Rule, portable_relative};
use serde_json::json;
use std::{fs, path::Path};
use tempfile::tempdir;

/// A rule as the Library filter sends it.
fn rule(field: &str, op: &str, value: impl Into<String>) -> Rule {
    serde_json::from_value(json!({ "field": field, "op": op, "value": value.into() })).unwrap()
}

/// The file names a folder rule selects.
fn names(lib: &Library, op: &str, folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = lib
        .search(&rule("folder", op, folder.to_string_lossy()))
        .unwrap()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn stored(lib: &Library) -> Vec<String> {
    let mut paths: Vec<String> = lib
        .assets(None)
        .unwrap()
        .into_iter()
        .map(|asset| asset.rel_path)
        .collect();
    paths.sort();
    paths
}

/// A session with `relative` photographs (`Capture/Day 2/A.RAF`), each
/// written through `Path::join` as a native path reaches a folder.
fn session(relative: &[&str]) -> (tempfile::TempDir, Library) {
    let d = tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    for relative in relative {
        let photo = relative
            .split('/')
            .fold(lib.directory.clone(), |path, part| path.join(part));
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, b"x").unwrap();
        lib.add_asset(&photo).unwrap();
    }
    (d, lib)
}

#[test]
fn a_native_relative_path_is_stored_with_forward_slashes() {
    assert_eq!(
        portable_relative(Path::new("Capture").join("Day 2").as_path()).unwrap(),
        "Capture/Day 2"
    );
    assert_eq!(portable_relative(Path::new("A.RAF")).unwrap(), "A.RAF");
    // Nothing a library cannot store.
    for refused in ["", "..", "Capture/..", "/absolute", "Capture\\A.RAF"] {
        assert!(
            portable_relative(Path::new(refused)).is_err(),
            "{refused:?} is not a relative portable path"
        );
    }
    #[cfg(unix)]
    assert!(
        portable_relative(Path::new("Cam\\A.RAF")).is_err(),
        "a Unix name with a backslash would be two folders on Windows"
    );
}

#[test]
fn nested_photographs_are_stored_and_found_after_reopening() {
    let (d, lib) = session(&["Capture/A.RAF", "Capture/Day 2/B.RAF"]);
    assert_eq!(stored(&lib), ["Capture/A.RAF", "Capture/Day 2/B.RAF"]);
    // The folder rule takes the native path of the folder.
    let capture = lib.directory.canonicalize().unwrap().join("Capture");
    assert_eq!(names(&lib, "is", &capture), ["A.RAF"]);
    assert_eq!(names(&lib, "contains", &capture).len(), 2);
    assert_eq!(names(&lib, "contains", &capture.join("Day 2")), ["B.RAF"]);
    // A library moved to another folder keeps reading its photographs.
    drop(lib);
    let moved = d.path().join("Moved");
    fs::rename(d.path().join("Job"), &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(stored(&lib), ["Capture/A.RAF", "Capture/Day 2/B.RAF"]);
    let day = moved.canonicalize().unwrap().join("Capture").join("Day 2");
    assert_eq!(names(&lib, "contains", &day), ["B.RAF"]);
}

#[test]
fn a_photograph_is_found_by_its_native_path() {
    let (_d, lib) = session(&["Capture/Day 2/A.RAF"]);
    let photo = lib
        .directory
        .canonicalize()
        .unwrap()
        .join("Capture")
        .join("Day 2")
        .join("A.RAF");
    let asset = lib.asset_at(&photo).unwrap().expect("registered");
    assert_eq!(asset.rel_path, "Capture/Day 2/A.RAF");
    assert_eq!(asset.path, photo);
    // A sibling that was never added is not registered.
    assert!(
        lib.asset_at(&photo.with_file_name("B.RAF"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn forgetting_and_offline_lookups_use_the_stored_form() {
    let (_d, lib) = session(&["Capture/Day 2/A.RAF", "Capture/B.RAF"]);
    let day = lib
        .directory
        .canonicalize()
        .unwrap()
        .join("Capture")
        .join("Day 2");
    let offline = |folder: &Path| {
        lib.offline_in(folder, false)
            .unwrap()
            .into_iter()
            .map(|asset| {
                asset
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>()
    };
    assert!(offline(&day).is_empty());
    // Gone from disk and marked so: listed by its folder.
    fs::remove_file(day.join("A.RAF")).unwrap();
    lib.set_missing_at(&day.join("A.RAF"), true).unwrap();
    assert_eq!(offline(&day), ["A.RAF"]);
    lib.forget_at(&day).unwrap();
    assert_eq!(stored(&lib), ["Capture/B.RAF"]);
}

/// On Unix a backslash is an ordinary character of a name: it must not be
/// read as a separator and must not be stored as one.
#[cfg(unix)]
#[test]
fn a_unix_name_with_a_backslash_is_refused_not_rewritten() {
    let (d, lib) = session(&[]);
    let folder = lib.directory.join("Capture");
    fs::create_dir_all(&folder).unwrap();
    let odd = folder.join("Cam\\A.RAF");
    fs::write(&odd, b"x").unwrap();
    let error = lib.add_asset(&odd).unwrap_err().to_string();
    assert!(error.contains("portable"), "{error}");
    assert_eq!(stored(&lib), Vec::<String>::new());
    // And it never reads as a nested folder of the library.
    assert!(lib.asset_at(&odd).unwrap().is_none());
    assert_eq!(
        lib.search(&rule("folder", "contains", folder.to_string_lossy()))
            .unwrap()
            .len(),
        0
    );
    drop(d);
}

#[test]
fn a_library_folder_outside_the_session_holds_no_session_photographs() {
    let (d, lib) = session(&["Capture/A.RAF"]);
    let outside = d.path().join("Elsewhere");
    fs::create_dir_all(&outside).unwrap();
    for op in ["is", "contains"] {
        assert!(
            lib.search(&rule("folder", op, outside.to_string_lossy()))
                .unwrap()
                .is_empty(),
            "{op} outside the library selects nothing"
        );
    }
    // Nor does the library itself: no photograph is registered as it.
    for op in ["is", "contains"] {
        let found = lib
            .search(&rule("folder", op, lib.directory.to_string_lossy()))
            .unwrap();
        assert_eq!(found.len(), usize::from(op == "contains"));
    }
}

#[test]
fn move_rename_and_trash_keep_stored_paths_portable() -> Result<()> {
    let (_d, lib) = session(&["Capture/Day 2/A.RAF"]);
    let photo = lib
        .directory
        .canonicalize()?
        .join("Capture")
        .join("Day 2")
        .join("A.RAF");
    let target = lib
        .directory
        .canonicalize()?
        .join("Capture")
        .join("Day 3")
        .join("A.RAF");
    fs::create_dir_all(target.parent().unwrap())?;
    fs::rename(&photo, &target)?;
    lib.relocate(&photo, &target)?;
    assert_eq!(stored(&lib), ["Capture/Day 3/A.RAF"]);
    let moved = lib.asset_at(&target)?.expect("registered at its new place");
    assert_eq!(moved.rel_path, "Capture/Day 3/A.RAF");
    // Trashed: the place stays as it is (portable) and the photograph is
    // hidden from every folder rule.
    assert_eq!(lib.set_trashed_at(&target, true)?, 1);
    assert_eq!(stored(&lib), ["Capture/Day 3/A.RAF"]);
    assert!(
        lib.search(&rule(
            "folder",
            "contains",
            target.parent().unwrap().to_string_lossy()
        ))?
        .is_empty(),
        "a trashed photograph is not in its folder"
    );
    Ok(())
}
