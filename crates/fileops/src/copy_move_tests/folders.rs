//! Whole folders moved to another volume: the tree is copied and verified,
//! published with one exclusive rename and the original folder goes to the
//! Trash as one item, only while it holds exactly what was copied.
use super::*;

/// The files of the moved folder `Shoot` (below it), with their sizes, and
/// its folders; `Empty` has nothing in it. Files the catalog does not know
/// (`notes.txt`, `.hidden`) travel too.
const TREE: [(&str, usize); 6] = [
    ("A.RAF", 2 * volume::BLOCK + 123),
    ("A.RAF.rrdata", 300),
    ("notes.txt", 12),
    (".hidden", 3),
    ("Sub/B.NEF", volume::BLOCK + 7),
    ("Sub/Deeper/B.xmp", 40),
];
const FOLDERS: [&str; 3] = ["Empty", "Sub", "Sub/Deeper"];

impl World {
    pub(super) fn folder(&self) -> (PathBuf, PathBuf) {
        let (from, to) = (self.source.join("Shoot"), self.target.join("Shoot"));
        if !exists(&from) && !exists(&to) {
            for dir in FOLDERS {
                fs::create_dir_all(from.join(dir)).unwrap();
            }
            for (name, len) in TREE {
                fs::write(from.join(name), content(name, len)).unwrap();
            }
        }
        (from, to)
    }
    pub(super) fn folder_step(&self) -> Step {
        let (from, to) = self.folder();
        if self.two_volumes {
            let step = Step::moving(from, to).unwrap();
            assert!(matches!(step.to, Target::OtherVolume { .. }));
            step
        } else {
            Step::other_volume(from, to)
        }
    }
    /// The whole folder is at `side` with every file's bytes and nothing
    /// else; at `other` it is gone or (kept) whole; no staging folder is
    /// left on either volume.
    fn assert_folder_at(&self, side: &Path, other: &Path, kept_allowed: bool) {
        assert_whole(&side.join("Shoot"));
        if exists(&other.join("Shoot")) {
            assert!(kept_allowed, "the folder is also at {}", other.display());
            assert_whole(&other.join("Shoot"));
        }
        for dir in [&self.source, &self.target] {
            assert!(
                World::names(dir)
                    .iter()
                    .all(|n| !n.starts_with(".rawtonic")),
                "left in {}: {:?}",
                dir.display(),
                World::names(dir)
            );
        }
    }
    /// After a crash, before recovery: every file is complete at the
    /// source or in a complete folder at the target; a folder under its
    /// final name at the target is never partial.
    fn assert_folder_nothing_lost(&self) {
        let (from, to) = (self.source.join("Shoot"), self.target.join("Shoot"));
        if exists(&to) {
            assert_whole(&to);
        }
        if exists(&from) {
            assert_whole(&from);
        }
        assert!(exists(&from) || exists(&to), "the folder is lost");
    }
}

/// `assert_whole` for a folder that also holds the file `extra`.
fn assert_whole_but(root: &Path, extra: &str) {
    let bytes = fs::read(root.join(extra)).unwrap();
    fs::remove_file(root.join(extra)).unwrap();
    assert_whole(root);
    fs::write(root.join(extra), bytes).unwrap();
}

/// Every file of `TREE` with its bytes, every folder, nothing else.
fn assert_whole(root: &Path) {
    let entries = tree::list(root).unwrap();
    let mut expected: Vec<PathBuf> = TREE
        .iter()
        .map(|(n, _)| PathBuf::from(n))
        .chain(FOLDERS.iter().map(PathBuf::from))
        .collect();
    expected.sort();
    let mut found: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    found.sort();
    assert_eq!(found, expected, "{}", root.display());
    for (name, len) in TREE {
        assert_eq!(
            fs::read(root.join(name)).unwrap(),
            content(name, len),
            "{name} in {}",
            root.display()
        );
    }
}

#[test]
fn a_folder_moves_whole_and_goes_to_the_trash_as_one_item() {
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(from.join("Sub/B.NEF"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let op = world
        .journal()
        .execute("move", "Move folder", vec![step], &world.trash)
        .unwrap();
    assert_eq!(op.state, State::Done);
    assert_eq!(op.note, None);
    world.assert_folder_at(&world.target, &world.source, false);
    assert!(!exists(&from));
    let items = world.trash.items();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_whole(&items[0]);
    assert_eq!(op.renames(), vec![(from, to.clone())]);
    assert_eq!(
        fs::metadata(to.join("Sub/B.NEF"))
            .unwrap()
            .modified()
            .unwrap(),
        old
    );
    // The photos beside it were not touched.
    for (name, len) in FILES {
        assert_eq!(
            fs::read(world.source.join(name)).unwrap(),
            content(name, len)
        );
    }
}

#[test]
fn undo_copies_the_folder_back_as_it_is_now_and_redo_moves_it_again() {
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let journal = world.journal();
    let (from, to) = world.folder();
    let op = journal
        .execute(
            "move",
            "Move folder",
            vec![world.folder_step()],
            &world.trash,
        )
        .unwrap();
    // Edited and added after the move: both travel back.
    fs::write(to.join("A.RAF.rrdata"), b"edited there").unwrap();
    fs::write(to.join("Sub/C.xmp"), b"new").unwrap();
    let undone = journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(undone.state, State::Undone);
    assert!(!exists(&to));
    assert_eq!(
        fs::read(from.join("A.RAF.rrdata")).unwrap(),
        b"edited there"
    );
    assert_eq!(fs::read(from.join("Sub/C.xmp")).unwrap(), b"new");
    assert_eq!(
        fs::read(from.join("Sub/B.NEF")).unwrap(),
        content("Sub/B.NEF", TREE[4].1)
    );
    assert!(from.join("Empty").is_dir());
    // The original from the move and the copy from the undo.
    assert_eq!(world.trash.items().len(), 2);
    let redone = journal.redo(&op.id, &world.trash).unwrap();
    assert_eq!(redone.state, State::Done);
    assert!(!exists(&from));
    assert_eq!(fs::read(to.join("Sub/C.xmp")).unwrap(), b"new");
    assert_eq!(fs::read(to.join("A.RAF.rrdata")).unwrap(), b"edited there");
}

#[test]
fn undo_refuses_when_the_folders_place_is_taken_again() {
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let journal = world.journal();
    let (from, to) = world.folder();
    let op = journal
        .execute(
            "move",
            "Move folder",
            vec![world.folder_step()],
            &world.trash,
        )
        .unwrap();
    fs::create_dir(&from).unwrap();
    let error = journal.undo(&op.id, &world.trash).unwrap_err();
    assert!(format!("{error:#}").contains("exists again"), "{error:#}");
    assert_whole(&to);
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Done);
}

/// A crash at every point of a folder's move, its undo and its redo:
/// nothing is lost, no partial folder appears under the final name, and
/// recovery ends the operation in one of its two states.
#[test]
fn a_crash_at_any_step_of_a_folder_move_undo_or_redo_loses_nothing() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Phase {
        Move,
        Undo,
        Redo,
    }
    for phase in [Phase::Move, Phase::Undo, Phase::Redo] {
        let mut points = 0;
        for n in 0.. {
            let world = World::new();
            if world.publishes_in_place() {
                return;
            }
            let step = world.folder_step();
            let id = match phase {
                Phase::Move => None,
                _ => {
                    let journal = world.journal();
                    let op = journal
                        .execute("move", "Move folder", vec![step.clone()], &world.trash)
                        .unwrap();
                    if let Phase::Redo = phase {
                        journal.undo(&op.id, &world.trash).unwrap();
                        journal.mark_synced(&op.id).unwrap();
                    }
                    Some(op.id)
                }
            };
            crash::arm(n);
            let run = catch_unwind(AssertUnwindSafe(|| {
                let journal = world.journal();
                match (phase, &id) {
                    (Phase::Move, _) => journal
                        .execute("move", "Move folder", vec![step.clone()], &world.trash)
                        .map(drop),
                    (Phase::Undo, Some(id)) => journal.undo(id, &world.trash).map(drop),
                    (_, Some(id)) => journal.redo(id, &world.trash).map(drop),
                    _ => unreachable!(),
                }
            }));
            if crash::disarm() {
                run.expect("no crash").unwrap();
                break;
            }
            assert!(run.is_err(), "{phase:?} crash point {n}");
            points += 1;
            world.assert_folder_nothing_lost();
            let journal = world.journal();
            journal.recover(&world.trash).unwrap();
            let op = journal.history(1).unwrap().remove(0);
            // A published folder's copy is never taken back: rolled back
            // after it appeared, both are kept and the note says so.
            let (side, other) = match op.state {
                State::RolledBack | State::Undone => (&world.source, &world.target),
                State::Done => (&world.target, &world.source),
                other => panic!("{phase:?} crash point {n}: {other:?}"),
            };
            world.assert_folder_at(side, other, true);
            let both = exists(&other.join("Shoot"));
            if both {
                let note = op.note.as_deref().unwrap_or("");
                assert!(
                    note.contains("stays at") || note.contains("was kept"),
                    "{note}"
                );
            }
            // Recovering again changes nothing, and the operation goes on.
            journal.recover(&world.trash).unwrap();
            assert_eq!(journal.operation(&op.id).unwrap().state, op.state);
            match op.state {
                _ if both && op.state != State::Done => {}
                State::Done => {
                    journal.undo(&op.id, &world.trash).unwrap();
                    world.assert_folder_at(&world.source, &world.target, false);
                }
                State::Undone => {
                    journal.redo(&op.id, &world.trash).unwrap();
                    world.assert_folder_at(&world.target, &world.source, false);
                }
                _ => {}
            }
        }
        assert!(points >= 8, "{phase:?}: only {points} crash points");
    }
}

/// While the tree is copied the original folder stays as it is and
/// nothing appears under the folder's final name.
#[test]
fn nothing_appears_under_the_folders_name_until_its_whole_copy_is_verified() {
    struct Watch<'a>(&'a Path, &'a Path, Cell<u32>);
    impl CopyObserver for Watch<'_> {
        fn copied(&self, _done: u64, _total: u64) {
            self.2.set(self.2.get() + 1);
            assert_whole(self.0);
            assert!(!exists(self.1), "published while copying");
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let watch = Watch(&from, &to, Cell::new(0));
    world
        .journal()
        .execute_with("move", "Move folder", vec![step], &world.trash, &watch)
        .unwrap();
    assert!(watch.2.get() >= 5, "observed {} blocks", watch.2.get());
    assert_whole(&to);
}

/// A file added to, or changed in, the original folder after its copy was
/// verified: the folder is kept (both exist) and the note says so; undo
/// then refuses, since the two differ.
#[test]
fn an_original_folder_that_changed_after_its_copy_is_kept() {
    struct AddOncePublished<'a>(&'a Path, &'a Path, Cell<bool>);
    impl CopyObserver for AddOncePublished<'_> {
        fn cancelled(&self) -> bool {
            if exists(self.1) && !self.2.replace(true) {
                fs::write(self.0.join("Sub/late.jpg"), b"written meanwhile").unwrap();
            }
            false
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let journal = world.journal();
    let op = journal
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &AddOncePublished(&from, &to, Cell::new(false)),
        )
        .unwrap();
    assert_eq!(op.state, State::Done);
    let note = op.note.clone().unwrap();
    assert!(note.contains("changed while it was moved"), "{note}");
    assert_eq!(
        fs::read(from.join("Sub/late.jpg")).unwrap(),
        b"written meanwhile"
    );
    assert_whole(&to);
    assert!(world.trash.items().is_empty());
    let error = journal.undo(&op.id, &world.trash).unwrap_err();
    assert!(format!("{error:#}").contains("differ now"), "{error:#}");
    // Once the two hold the same again, undo trashes only the copy.
    fs::remove_file(from.join("Sub/late.jpg")).unwrap();
    journal.undo(&op.id, &world.trash).unwrap();
    world.assert_folder_at(&world.source, &world.target, false);
}

/// A file changed or added in the original folder while it is copied stops
/// the move: nothing changes.
#[test]
fn a_folder_changed_while_copying_stops_the_move() {
    struct AddWhileCopying<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for AddWhileCopying<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                fs::write(self.0.join("Empty/new.txt"), b"x").unwrap();
            }
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let journal = world.journal();
    let error = journal
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &AddWhileCopying(&from, Cell::new(false)),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("changed while it was copied"),
        "{error:#}"
    );
    assert_eq!(journal.history(1).unwrap()[0].state, State::RolledBack);
    assert!(!exists(&to));
    fs::remove_file(from.join("Empty/new.txt")).unwrap();
    world.assert_folder_at(&world.source, &world.target, false);
}

#[test]
fn stop_while_copying_a_folder_rolls_back() {
    struct StopAfterFirstBlock(Cell<bool>);
    impl CopyObserver for StopAfterFirstBlock {
        fn copied(&self, _: u64, _: u64) {
            self.0.set(true);
        }
        fn cancelled(&self) -> bool {
            self.0.get()
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let error = world
        .journal()
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &StopAfterFirstBlock(Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Stopped"), "{error:#}");
    world.assert_folder_at(&world.source, &world.target, false);
    assert!(world.trash.items().is_empty());
}

#[test]
fn a_folder_name_taken_while_copying_is_never_replaced() {
    struct Squatter<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Squatter<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                fs::create_dir(self.0).unwrap();
                fs::write(self.0.join("theirs.txt"), b"someone else's").unwrap();
            }
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let error = world
        .journal()
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &Squatter(&to, Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Shoot"), "{error:#}");
    assert_eq!(World::names(&to), ["theirs.txt"]);
    assert_whole(&from);
}

/// A volume without a Trash keeps the original folder; undo then trashes
/// only the copy, while both are the same.
#[test]
fn a_folder_that_cannot_go_to_the_trash_is_kept() {
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    world.trash.refuse.set(true);
    let journal = world.journal();
    let op = journal
        .execute(
            "move",
            "Move folder",
            vec![world.folder_step()],
            &world.trash,
        )
        .unwrap();
    assert!(
        op.note
            .as_deref()
            .unwrap()
            .contains("cannot go to the Trash")
    );
    world.assert_folder_at(&world.target, &world.source, true);
    assert!(exists(&world.source.join("Shoot")));
    world.trash.refuse.set(false);
    journal.undo(&op.id, &world.trash).unwrap();
    world.assert_folder_at(&world.source, &world.target, false);
    assert_eq!(world.trash.items().len(), 1);
}

#[cfg(unix)]
#[test]
fn a_folder_with_a_link_in_it_is_refused_before_copying() {
    let world = World::new();
    let step = world.folder_step();
    let (from, to) = world.folder();
    std::os::unix::fs::symlink(world.source.join("A.RAF"), from.join("Sub/link.RAF")).unwrap();
    let error = world
        .journal()
        .execute("move", "Move folder", vec![step], &world.trash)
        .unwrap_err();
    assert!(format!("{error:#}").contains("link"), "{error:#}");
    assert!(!exists(&to));
    assert!(World::names(&world.target).is_empty());
    fs::remove_file(from.join("Sub/link.RAF")).unwrap();
    assert_whole(&from);
}

#[test]
fn a_folder_moves_to_another_drive_on_its_own() {
    let world = World::new();
    let step = world.folder_step();
    let (from, _) = world.folder();
    let inner = Step::other_volume(from.join("A.RAF"), world.target.join("A.RAF"));
    let beside = Step::rename(world.source.join("A.xmp"), world.source.join("B.xmp"));
    for other in [inner, beside] {
        let error = world
            .journal()
            .execute("move", "Move", vec![step.clone(), other], &world.trash)
            .unwrap_err();
        assert!(format!("{error:#}").contains("of its own"), "{error:#}");
    }
    assert_whole(&from);
    assert!(exists(&world.source.join("A.xmp")));
}

/// The space a folder move needs is the space it reports it will copy
/// (`copy_needs`: its files, not its folders) on the volume it goes to;
/// the free space there is the system's own figure (a real query, asked
/// for a path that does not exist yet through its nearest existing
/// folder), never an invented one. The move of that folder then fits.
#[test]
fn a_folder_move_needs_its_bytes_on_the_target_volume() {
    if std::env::var_os("RAWTONIC_TEST_OTHER_VOLUME").is_some() {
        World::new();
        eprintln!("SKIP: the target is the one volume given, asked separately");
        return;
    }
    let world = World::new();
    let (from, to) = world.folder();
    let bytes: u64 = TREE.iter().map(|(_, len)| *len as u64).sum();
    let needs = copy_needs(&[(from.clone(), to.clone())]).unwrap();
    assert_eq!(needs.len(), 1, "{needs:?}");
    let need = &needs[0];
    assert_eq!((need.bytes, need.files), (bytes, TREE.len()));
    // The target volume, before the folder exists there.
    assert_eq!(
        need.volume,
        volume_info(&world.target).unwrap(),
        "asked for {to:?}"
    );
    assert!(need.volume.mount_point.is_absolute(), "{need:?}");
    assert!(
        need.volume.free_bytes < u64::MAX,
        "free space comes from the system: {need:?}"
    );
    if world.publishes_in_place() {
        return;
    }
    let op = world
        .journal()
        .execute(
            "move",
            "Move folder",
            vec![Step::other_volume(&from, &to)],
            &world.trash,
        )
        .unwrap();
    assert_eq!(op.state, State::Done);
    assert!(!exists(&from));
}

/// Permission bits travel with the folder and back (owner access kept),
/// on volumes that keep them.
#[cfg(unix)]
#[test]
fn a_folders_permissions_travel_with_it() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    let step = world.folder_step();
    let (from, to) = world.folder();
    if volume::unix_mode(&world.target).unwrap().is_none() {
        eprintln!("SKIP: the target volume has no Unix modes");
        return;
    }
    let set =
        |p: &Path, mode: u32| fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    set(&from.join("Sub"), 0o750);
    set(&from.join("Sub/B.NEF"), 0o600);
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let journal = world.journal();
    let op = journal
        .execute("move", "Move folder", vec![step], &world.trash)
        .unwrap();
    assert_eq!(mode(&to.join("Sub")), 0o750);
    assert_eq!(mode(&to.join("Sub/B.NEF")), 0o600);
    journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(mode(&from.join("Sub")), 0o750);
    assert_eq!(mode(&from.join("Sub/B.NEF")), 0o600);
}

/// A file gone from the published copy before the original folder goes to
/// the Trash: the original is kept, whole.
#[test]
fn an_original_folder_whose_copy_lost_a_file_is_kept() {
    struct RemoveOncePublished<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for RemoveOncePublished<'_> {
        fn cancelled(&self) -> bool {
            if exists(self.0) && !self.1.replace(true) {
                fs::remove_file(self.0.join("Sub/Deeper/B.xmp")).unwrap();
            }
            false
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let op = world
        .journal()
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &RemoveOncePublished(&to, Cell::new(false)),
        )
        .unwrap();
    let note = op.note.unwrap();
    assert!(note.contains("is not there unchanged"), "{note}");
    assert_whole(&from);
    assert!(world.trash.items().is_empty());
}

/// A published folder is never taken back: Stop after it appeared keeps
/// it (with what was added to it) and the original, and says so; staging
/// cleanup removes only paths the moved folder has.
#[test]
fn rolling_a_published_folder_back_never_deletes_what_was_added_to_it() {
    struct AddAndStopOncePublished<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for AddAndStopOncePublished<'_> {
        fn cancelled(&self) -> bool {
            if exists(self.0) && !self.1.replace(true) {
                fs::write(self.0.join("Sub/added.jpg"), b"added there").unwrap();
            }
            self.1.get()
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let step = world.folder_step();
    let (from, to) = world.folder();
    let journal = world.journal();
    let error = journal
        .execute_with(
            "move",
            "Move folder",
            vec![step],
            &world.trash,
            &AddAndStopOncePublished(&to, Cell::new(false)),
        )
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("Stopped") && text.contains("Not everything was put back"),
        "{text}"
    );
    let op = journal.history(1).unwrap().remove(0);
    assert_eq!(op.state, State::RolledBack);
    assert!(op.note.unwrap().contains("stays at"));
    assert_whole_but(&to, "Sub/added.jpg");
    assert_eq!(fs::read(to.join("Sub/added.jpg")).unwrap(), b"added there");
    assert_whole(&from);

    // A file that reached the staged copy after its comparison stays.
    let world = World::new();
    let (from, _) = world.folder();
    let staging = world.target.join(format!("{MOVE_STAGING_PREFIX}x"));
    let staged = staging.join("0");
    fs::create_dir_all(staged.join("Sub")).unwrap();
    fs::write(staged.join("A.RAF"), b"a copy").unwrap();
    fs::write(staged.join("Sub/late.jpg"), b"not a copy").unwrap();
    let mut step = Step::other_volume(&from, world.target.join("Shoot"));
    if let Target::OtherVolume { copy, .. } = &mut step.to {
        copy.folder = true;
        copy.staged = Some(staged.clone());
    }
    let payload = Payload {
        steps: vec![step],
        staging_dirs: vec![staging],
        ..Payload::default()
    };
    let error = remove_staging(&payload).unwrap_err();
    assert!(format!("{error:#}").contains("not copies"), "{error:#}");
    assert!(!exists(&staged.join("A.RAF")));
    assert_eq!(
        fs::read(staged.join("Sub/late.jpg")).unwrap(),
        b"not a copy"
    );
    assert_whole(&from);
}

/// Undo or redo stopped after it published a folder: the error names the
/// folder that stays, and a retry takes it over.
#[test]
fn undo_and_redo_stopped_after_publishing_a_folder_say_so_and_can_be_retried() {
    struct StopWhen<'a>(&'a Path);
    impl CopyObserver for StopWhen<'_> {
        fn cancelled(&self) -> bool {
            exists(self.0)
        }
    }
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    let journal = world.journal();
    let (from, to) = world.folder();
    let op = journal
        .execute(
            "move",
            "Move folder",
            vec![world.folder_step()],
            &world.trash,
        )
        .unwrap();
    let error = journal
        .undo_with(&op.id, &world.trash, &StopWhen(&from))
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("Not everything was put back") && text.contains("stays at"),
        "{text}"
    );
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Done);
    assert_whole(&from);
    assert_whole(&to);
    journal.undo(&op.id, &world.trash).unwrap();
    world.assert_folder_at(&world.source, &world.target, false);

    let error = journal
        .redo_with(&op.id, &world.trash, &StopWhen(&to))
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("Not everything was put back") && text.contains("stays at"),
        "{text}"
    );
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Undone);
    journal.redo(&op.id, &world.trash).unwrap();
    world.assert_folder_at(&world.target, &world.source, false);
}
