//! Edit With renders: a file made from a photograph is registered as a
//! photograph of its own and stacked with the original.
use rawtonic_catalog::{Library, Mode};
use std::fs;

#[test]
fn an_edit_is_linked_to_its_original_in_a_session_and_a_catalog() {
    for mode in [Mode::Session, Mode::Catalog] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(&tmp.path().join("Lib"), "Lib", mode).unwrap();
        // A session keeps its photographs inside; a catalog references them.
        let folder = match mode {
            Mode::Session => tmp.path().join("Lib/Capture"),
            Mode::Catalog => tmp.path().join("Shoot"),
        };
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("A.NEF"), b"raw").unwrap();
        fs::write(folder.join("A_edit.tif"), b"tiff").unwrap();
        fs::write(folder.join("A_edit-2.tif"), b"tiff 2").unwrap();
        let original = lib.add_asset(&folder.join("A.NEF")).unwrap();
        let edit = lib.add_asset(&folder.join("A_edit.tif")).unwrap();
        let second = lib.add_asset(&folder.join("A_edit-2.tif")).unwrap();

        assert!(lib.derived_from(&edit.id).unwrap().is_none());
        lib.set_derived_from(&edit.id, &original.id).unwrap();
        // An edit of the edit joins the original's stack.
        lib.set_derived_from(&second.id, &edit.id).unwrap();

        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        assert_eq!(
            lib.derived_from(&second.id).unwrap().unwrap().id,
            original.id
        );
        assert!(lib.derived_from(&original.id).unwrap().is_none());
        let stack: Vec<_> = lib
            .derivatives(&original.id)
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(stack, [edit.id.clone(), second.id.clone()]);

        // The link lasts when the library is opened again.
        drop(lib);
        let lib = Library::open(&tmp.path().join("Lib")).unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
    }
}

#[test]
fn links_need_two_registered_photographs() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("Lib"), "Lib", Mode::Session).unwrap();
    let path = tmp.path().join("Lib/Capture/A.NEF");
    fs::write(&path, b"raw").unwrap();
    let original = lib.add_asset(&path).unwrap();
    assert!(lib.set_derived_from(&original.id, &original.id).is_err());
    assert!(lib.set_derived_from("no-such-asset", &original.id).is_err());
    assert!(lib.set_derived_from(&original.id, "no-such-asset").is_err());
    assert!(lib.derived_from(&original.id).unwrap().is_none());
}

#[test]
fn stacks_stay_one_level_and_never_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("Lib"), "Lib", Mode::Session).unwrap();
    let ids: Vec<_> = ["A.NEF", "A_edit.tif", "B.NEF"]
        .iter()
        .map(|name| {
            let path = tmp.path().join("Lib/Capture").join(name);
            fs::write(&path, name.as_bytes()).unwrap();
            lib.add_asset(&path).unwrap().id
        })
        .collect();
    let (a, edit, b) = (&ids[0], &ids[1], &ids[2]);
    lib.set_derived_from(edit, a).unwrap();
    // The original cannot become its own render's render.
    assert!(lib.set_derived_from(a, edit).is_err());
    // Linked to another photograph, A takes its render along.
    lib.set_derived_from(a, b).unwrap();
    assert_eq!(lib.derived_from(edit).unwrap().unwrap().id, *b);
    assert_eq!(lib.derived_from(a).unwrap().unwrap().id, *b);
    assert!(lib.derivatives(a).unwrap().is_empty());
}

#[test]
fn a_render_saved_by_another_program_is_known_by_its_new_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("Lib"), "Lib", Mode::Session).unwrap();
    let path = tmp.path().join("Lib/Capture/A_edit.tif");
    fs::write(&path, b"first render").unwrap();
    let render = lib.add_asset(&path).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.fill_all_checksums(&cancel).unwrap();
    assert!(lib.checksum(&render.id).unwrap().is_some());

    fs::write(&path, b"saved by the other program, longer").unwrap();
    lib.file_saved(&render.id).unwrap();
    let after = lib.asset(&render.id).unwrap();
    assert_eq!(after.size, 34);
    assert_eq!(
        after.fingerprint,
        rawtonic_catalog::fingerprint(&path).unwrap()
    );
    assert_ne!(after.fingerprint, render.fingerprint);
    // Hashed again later, from the new content.
    assert!(lib.checksum(&render.id).unwrap().is_none());
    lib.fill_all_checksums(&cancel).unwrap();
    assert!(lib.checksum(&render.id).unwrap().is_some());
}
