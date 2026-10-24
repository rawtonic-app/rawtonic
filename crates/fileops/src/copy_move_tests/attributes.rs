//! Extended attributes travel with a copy move (files and folders, there
//! and back), are verified on the copy and checked again before the
//! original goes to the Trash; a target that cannot keep them refuses the
//! move before anything is copied. Where the temporary folder's volume
//! keeps no attributes, the tests say SKIP.
use super::*;
use crate::xattr::{self, Attribute};

#[cfg(target_vendor = "apple")]
const NAME: &str = "cc.rawtonic.test";
#[cfg(not(target_vendor = "apple"))]
const NAME: &str = "user.rawtonic.test";

fn attribute(value: &str) -> Vec<Attribute> {
    vec![(NAME.as_bytes().to_vec(), value.as_bytes().to_vec())]
}

/// Gives `path` the test attribute; false where its volume cannot.
fn tag(path: &Path, value: &str) -> bool {
    xattr::write(&fs::File::open(path).unwrap(), &attribute(value)).is_ok()
}

fn tags(path: &Path) -> Vec<Attribute> {
    xattr::read_path(path).unwrap()
}

/// Whether the target volume keeps attributes (else the move is refused).
fn target_keeps(world: &World) -> bool {
    let probe = world.target.join("probe");
    fs::create_dir(&probe).unwrap();
    let keeps = xattr::probe(&probe).is_ok();
    fs::remove_dir(&probe).unwrap();
    keeps
}

#[test]
fn attributes_travel_with_a_moved_file_and_back() {
    let world = World::new();
    if !tag(&world.source.join("A.RAF"), "red") {
        eprintln!("SKIP: the temporary folder keeps no extended attributes");
        return;
    }
    let journal = world.journal();
    let result = journal.execute("move", "Move", world.steps(), &world.trash);
    if !target_keeps(&world) {
        let error = result.unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("cannot keep extended attributes"), "{text}");
        assert!(text.contains("A.RAF has") && text.contains(NAME), "{text}");
        world.assert_at(&world.source, &world.target);
        assert_eq!(tags(&world.source.join("A.RAF")), attribute("red"));
        assert!(world.trash.items().is_empty());
        let sources = vec![world.source.join("A.RAF")];
        let refused = probe_folder(&world.target, &sources).unwrap_err();
        assert!(format!("{refused:#}").contains("cannot keep"), "{refused:#}");
        probe_folder(&world.target, &[world.source.join("A.xmp")]).unwrap();
        return;
    }
    let op = result.unwrap();
    assert_eq!(op.note, None);
    assert_eq!(tags(&world.target.join("A.RAF")), attribute("red"));
    assert!(tags(&world.target.join("A.xmp")).is_empty());
    // A tag changed on the copy travels back with it.
    assert!(tag(&world.target.join("A.RAF"), "green"));
    journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(tags(&world.source.join("A.RAF")), attribute("green"));
    journal.redo(&op.id, &world.trash).unwrap();
    assert_eq!(tags(&world.target.join("A.RAF")), attribute("green"));
}

#[test]
fn attributes_of_a_folder_and_of_what_is_in_it_travel_with_it() {
    let world = World::new();
    let (from, to) = world.folder();
    if !(tag(&from, "folder") && tag(&from.join("Sub/B.NEF"), "nef") && tag(&from.join("Sub"), "sub"))
    {
        eprintln!("SKIP: the temporary folder keeps no extended attributes");
        return;
    }
    let journal = world.journal();
    let result = journal.execute("move", "Move folder", vec![world.folder_step()], &world.trash);
    if !target_keeps(&world) {
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains("cannot keep"), "{error:#}");
        assert!(!exists(&to));
        assert_eq!(tags(&from.join("Sub/B.NEF")), attribute("nef"));
        return;
    }
    let op = result.unwrap();
    assert_eq!(op.note, None);
    assert_eq!(tags(&to), attribute("folder"));
    assert_eq!(tags(&to.join("Sub")), attribute("sub"));
    assert_eq!(tags(&to.join("Sub/B.NEF")), attribute("nef"));
    assert!(tags(&to.join("A.RAF")).is_empty());
    journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(tags(&from), attribute("folder"));
    assert_eq!(tags(&from.join("Sub/B.NEF")), attribute("nef"));
}

/// An attribute given to the original after its copy was verified (a
/// Finder tag added meanwhile): the original is kept, and undo refuses
/// while the two differ.
#[test]
fn an_original_whose_attributes_changed_after_its_copy_is_kept() {
    struct TagOncePublished<'a>(&'a Path, &'a Path, Cell<bool>);
    impl CopyObserver for TagOncePublished<'_> {
        fn cancelled(&self) -> bool {
            if exists(self.1) && !self.2.replace(true) {
                assert!(tag(self.0, "added meanwhile"));
            }
            false
        }
    }
    let world = World::new();
    let raw = world.source.join("A.RAF");
    if !tag(&raw, "red") || !target_keeps(&world) {
        eprintln!("SKIP: no extended attributes on both volumes");
        return;
    }
    let journal = world.journal();
    let op = journal
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &TagOncePublished(&raw, &world.target.join("A.RAF"), Cell::new(false)),
        )
        .unwrap();
    let note = op.note.clone().unwrap();
    assert!(note.contains("A.RAF changed while it was moved"), "{note}");
    assert_eq!(tags(&raw), attribute("added meanwhile"));
    assert_eq!(tags(&world.target.join("A.RAF")), attribute("red"));
    let error = journal.undo(&op.id, &world.trash).unwrap_err();
    assert!(format!("{error:#}").contains("differ now"), "{error:#}");
}

/// An attribute changed while the file is copied stops the move.
#[test]
fn attributes_changed_while_copying_stop_the_move() {
    struct TagWhileCopying<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for TagWhileCopying<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                assert!(tag(self.0, "while copying"));
            }
        }
    }
    let world = World::new();
    let raw = world.source.join("A.RAF");
    if !tag(&raw, "red") || !target_keeps(&world) {
        eprintln!("SKIP: no extended attributes on both volumes");
        return;
    }
    let journal = world.journal();
    let error = journal
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &TagWhileCopying(&raw, Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed while it was copied"), "{error:#}");
    assert_eq!(journal.history(1).unwrap()[0].state, State::RolledBack);
    world.assert_at(&world.source, &world.target);
}

#[test]
fn only_attributes_a_program_may_set_are_kept() {
    #[cfg(target_os = "linux")]
    {
        assert!(xattr::kept(b"user.xdg.tags"));
        for name in ["security.selinux", "system.posix_acl_access", "trusted.overlay.opaque"] {
            assert!(!xattr::kept(name.as_bytes()), "{name}");
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        for name in [
            "com.apple.metadata:_kMDItemUserTags",
            "com.apple.FinderInfo",
            "com.apple.ResourceFork",
            "com.apple.quarantine",
        ] {
            assert!(xattr::kept(name.as_bytes()), "{name}");
        }
        for name in ["com.apple.provenance", "com.apple.rootless", "com.apple.system.Security"] {
            assert!(!xattr::kept(name.as_bytes()), "{name}");
        }
    }
}
