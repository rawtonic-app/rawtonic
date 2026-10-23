//! Moves to another volume (ADR 0016): copy, verify, publish, trash; undo
//! and redo; a simulated crash at every journal step.
//!
//! The target lives on a second file system where the machine has one at
//! `/dev/shm` (Linux, CI included) on another device than the temporary
//! folder; elsewhere both are on one volume and the copy move is asked for
//! explicitly (`Step::other_volume`), which runs the same code.
use super::*;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The system Trash for tests: a `.trash` folder beside the file's folder
/// (so on its volume). `refuse` plays a volume without a Trash; `on_trash`
/// runs before each trashing.
type TrashHook = Box<dyn FnMut(&Path)>;

struct FakeTrash {
    refuse: Cell<bool>,
    on_trash: RefCell<Option<TrashHook>>,
    dirs: RefCell<Vec<PathBuf>>,
}

impl FakeTrash {
    fn new() -> Self {
        Self {
            refuse: Cell::new(false),
            on_trash: RefCell::new(None),
            dirs: RefCell::new(Vec::new()),
        }
    }
    fn items(&self) -> Vec<PathBuf> {
        let mut items: Vec<_> = self
            .dirs
            .borrow()
            .iter()
            .flat_map(|dir| fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()))
            .collect();
        items.sort();
        items
    }
}

impl SystemTrash for FakeTrash {
    fn trash(&self, path: &Path) -> Result<String> {
        if let Some(hook) = self.on_trash.borrow_mut().as_mut() {
            hook(path);
        }
        ensure!(!self.refuse.get(), "this volume has no Trash");
        let dir = path.parent().unwrap().parent().unwrap().join(".trash");
        fs::create_dir_all(&dir)?;
        if !self.dirs.borrow().contains(&dir) {
            self.dirs.borrow_mut().push(dir.clone());
        }
        let item = dir.join(format!(
            "{}-{}",
            uuid::Uuid::new_v4(),
            path.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(path, &item)?;
        Ok(item.to_string_lossy().into_owned())
    }
    fn restore(&self, item: &str, original: &Path) -> Result<()> {
        Ok(rename_no_clobber(Path::new(item), original)?)
    }
    fn contains(&self, item: &str) -> bool {
        exists(Path::new(item))
    }
}

/// A source folder, a target folder on another volume where possible, a
/// trash on the source volume and a journal.
struct World {
    _source_dir: tempfile::TempDir,
    _target_dir: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
    journal_path: PathBuf,
    trash: FakeTrash,
    two_volumes: bool,
}

/// Bytes that differ per file and per position, longer than one copy block
/// for the photos (so a crash can come in the middle of a copy).
fn content(name: &str, len: usize) -> Vec<u8> {
    let seed = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    (0..len)
        .map(|i| (seed.wrapping_add(i as u32).wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

const FILES: [(&str, usize); 3] = [
    ("A.RAF", 2 * volume::BLOCK + 123),
    ("A.RAF.rrdata", 300),
    ("A.xmp", 50),
];

impl World {
    fn new() -> Self {
        let source_dir = tempfile::tempdir().unwrap();
        let shm = Path::new("/dev/shm");
        let target_dir = if same_volume(source_dir.path(), &shm.join("x")).unwrap_or(true) {
            tempfile::tempdir().unwrap()
        } else {
            tempfile::tempdir_in(shm).unwrap()
        };
        let source = source_dir.path().canonicalize().unwrap().join("Capture");
        let target = target_dir.path().canonicalize().unwrap().join("Archive");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        for (name, len) in FILES {
            fs::write(source.join(name), content(name, len)).unwrap();
        }
        let two_volumes = !same_volume(&source, &target.join("x")).unwrap();
        let trash = FakeTrash::new();
        let journal_path = source_dir.path().join("journal.sqlite");
        Self {
            _source_dir: source_dir,
            _target_dir: target_dir,
            source,
            target,
            journal_path,
            trash,
            two_volumes,
        }
    }
    fn journal(&self) -> Journal {
        Journal::open(&self.journal_path).unwrap()
    }
    fn steps(&self) -> Vec<Step> {
        FILES
            .iter()
            .map(|(name, _)| {
                let (from, to) = (self.source.join(name), self.target.join(name));
                if self.two_volumes {
                    let step = Step::moving(from, to).unwrap();
                    assert!(matches!(step.to, Target::OtherVolume { .. }));
                    step
                } else {
                    Step::other_volume(from, to)
                }
            })
            .collect()
    }
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
    /// Every file is where the operation's state says, with its bytes; the
    /// other side holds nothing of it; no staging folder is left.
    fn assert_at(&self, side: &Path, other: &Path) {
        for (name, len) in FILES {
            assert_eq!(
                fs::read(side.join(name)).unwrap(),
                content(name, len),
                "{name} at {}",
                side.display()
            );
            assert!(
                !exists(&other.join(name)),
                "{name} also at {}",
                other.display()
            );
        }
        for dir in [side, other] {
            assert!(
                Self::names(dir).iter().all(|n| !n.starts_with('.')),
                "left in {}: {:?}",
                dir.display(),
                Self::names(dir)
            );
        }
    }
    /// After a crash, before recovery: no file is lost, and a file under
    /// a final name in the target is complete.
    fn assert_nothing_lost(&self) {
        let trashed: Vec<Vec<u8>> = self
            .trash
            .items()
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect();
        for (name, len) in FILES {
            let bytes = content(name, len);
            let at_source = fs::read(self.source.join(name)).ok();
            let at_target = fs::read(self.target.join(name)).ok();
            if let Some(found) = &at_target {
                assert_eq!(found, &bytes, "{name}: a half file under its final name");
            }
            if let Some(found) = &at_source {
                assert_eq!(found, &bytes, "{name}: the original changed");
            }
            assert!(
                at_source.is_some() || at_target.is_some() || trashed.contains(&bytes),
                "{name} is lost"
            );
            assert!(
                at_source.is_some() || at_target.is_some(),
                "{name}: only the Trash holds it"
            );
        }
    }
}

#[test]
fn a_move_to_another_volume_copies_verifies_and_trashes_the_originals() {
    let world = World::new();
    eprintln!("two volumes: {}", world.two_volumes);
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    assert_eq!(op.state, State::Done);
    assert_eq!(op.note, None);
    world.assert_at(&world.target, &world.source);
    let trashed: Vec<Vec<u8>> = world
        .trash
        .items()
        .iter()
        .map(|p| fs::read(p).unwrap())
        .collect();
    for (name, len) in FILES {
        assert!(
            trashed.contains(&content(name, len)),
            "{name} not in the Trash"
        );
    }
    assert_eq!(op.renames().len(), FILES.len());
    assert!(
        op.trashed().is_empty(),
        "the records relocate; nothing is missing"
    );
    let mtime = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
    let original = world
        .trash
        .items()
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with("A.RAF"))
        .unwrap();
    assert_eq!(
        mtime(&world.target.join("A.RAF")),
        mtime(&original),
        "the time travels"
    );
}

#[test]
fn a_move_picks_a_rename_on_one_volume_and_a_copy_across() {
    let world = World::new();
    let step = Step::moving(world.source.join("A.RAF"), world.source.join("B.RAF")).unwrap();
    assert!(matches!(step.to, Target::Path { .. }));
    if !world.two_volumes {
        eprintln!("SKIP: no second volume here");
        return;
    }
    let step = Step::moving(world.source.join("A.RAF"), world.target.join("A.RAF")).unwrap();
    assert!(matches!(step.to, Target::OtherVolume { .. }));
    // A rename across volumes is still refused, never a silent copy.
    let error = world
        .journal()
        .execute(
            "move",
            "Move",
            vec![Step::rename(
                world.source.join("A.RAF"),
                world.target.join("A.RAF"),
            )],
            &world.trash,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("another volume"), "{error:#}");
}

#[test]
fn undo_copies_back_what_is_there_now_and_redo_moves_again() {
    let world = World::new();
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    // An edit made after the move travels back.
    fs::write(world.target.join("A.RAF.rrdata"), b"edited there").unwrap();
    let undone = journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(undone.state, State::Undone);
    assert_eq!(
        fs::read(world.source.join("A.RAF.rrdata")).unwrap(),
        b"edited there"
    );
    assert_eq!(
        fs::read(world.source.join("A.RAF")).unwrap(),
        content("A.RAF", FILES[0].1)
    );
    assert!(
        World::names(&world.target).is_empty(),
        "{:?}",
        World::names(&world.target)
    );
    assert!(
        World::names(&world.source)
            .iter()
            .all(|n| !n.starts_with('.'))
    );
    // The originals from the move and the copies from the undo are in the Trash.
    assert_eq!(world.trash.items().len(), 2 * FILES.len());
    assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);

    let redone = journal.redo(&op.id, &world.trash).unwrap();
    assert_eq!(redone.state, State::Done);
    assert_eq!(
        fs::read(world.target.join("A.RAF.rrdata")).unwrap(),
        b"edited there"
    );
    assert!(!exists(&world.source.join("A.RAF")));
    assert!(journal.last_redoable().unwrap().is_none());
    assert_eq!(journal.last_undoable().unwrap().unwrap().id, op.id);
}

#[test]
fn undo_refuses_when_an_original_place_is_taken_again() {
    let world = World::new();
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    fs::write(world.source.join("A.xmp"), b"a new file").unwrap();
    let error = journal.undo(&op.id, &world.trash).unwrap_err();
    assert!(format!("{error:#}").contains("exists again"), "{error:#}");
    assert_eq!(fs::read(world.source.join("A.xmp")).unwrap(), b"a new file");
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Done);
    assert!(exists(&world.target.join("A.RAF")));
}

/// The bar of ADR 0016: nothing under a final name until its verified copy
/// is complete, the originals untouched until then.
#[test]
fn the_originals_stay_untouched_until_the_verified_copies_exist() {
    struct Watch<'a>(&'a World, Cell<u32>);
    impl CopyObserver for Watch<'_> {
        fn copied(&self, _done: u64, _total: u64) {
            self.1.set(self.1.get() + 1);
            for (name, len) in FILES {
                assert_eq!(
                    fs::read(self.0.source.join(name)).unwrap(),
                    content(name, len)
                );
                assert!(
                    !exists(&self.0.target.join(name)),
                    "{name} published while copying"
                );
            }
        }
    }
    let world = World::new();
    let watch = Watch(&world, Cell::new(0));
    world
        .journal()
        .execute_with("move", "Move", world.steps(), &world.trash, &watch)
        .unwrap();
    assert!(watch.1.get() >= 4, "observed {} blocks", watch.1.get());
    world.assert_at(&world.target, &world.source);
}

/// A crash at every point of a move: recovery rolls back before the
/// sources go to the Trash and finishes after; nothing is ever lost.
#[test]
fn a_crash_at_any_step_of_a_move_loses_nothing() {
    let mut points = 0;
    for n in 0.. {
        let world = World::new();
        crash::arm(n);
        let run = catch_unwind(AssertUnwindSafe(|| {
            world
                .journal()
                .execute("move", "Move", world.steps(), &world.trash)
        }));
        if crash::disarm() {
            run.expect("no crash").unwrap();
            break;
        }
        assert!(run.is_err(), "crash point {n}");
        points += 1;
        world.assert_nothing_lost();
        let journal = world.journal();
        let pending = journal.recover(&world.trash).unwrap();
        let op = journal.history(1).unwrap().remove(0);
        match op.state {
            State::RolledBack => {
                world.assert_at(&world.source, &world.target);
                assert!(world.trash.items().is_empty());
                assert!(pending.is_empty());
            }
            State::Done => {
                world.assert_at(&world.target, &world.source);
                assert_eq!(pending.len(), 1, "the records follow");
            }
            other => panic!("crash point {n}: {other:?}"),
        }
        // Recovering again changes nothing.
        journal.recover(&world.trash).unwrap();
        assert_eq!(journal.operation(&op.id).unwrap().state, op.state);
    }
    assert!(points >= 12, "only {points} crash points");
}

/// A crash at every point of an undo: before its copies back are all in
/// place the move stays done; after, the undo finishes.
#[test]
fn a_crash_at_any_step_of_an_undo_loses_nothing() {
    let mut points = 0;
    for n in 0.. {
        let world = World::new();
        let op = world
            .journal()
            .execute("move", "Move", world.steps(), &world.trash)
            .unwrap();
        let trashed_by_move = world.trash.items().len();
        crash::arm(n);
        let run = catch_unwind(AssertUnwindSafe(|| {
            world.journal().undo(&op.id, &world.trash)
        }));
        if crash::disarm() {
            run.expect("no crash").unwrap();
            break;
        }
        assert!(run.is_err(), "crash point {n}");
        points += 1;
        world.assert_nothing_lost();
        let journal = world.journal();
        journal.recover(&world.trash).unwrap();
        let now = journal.operation(&op.id).unwrap();
        match now.state {
            State::Done => {
                world.assert_at(&world.target, &world.source);
                assert_eq!(world.trash.items().len(), trashed_by_move);
                // And the undo works after all.
                journal.undo(&op.id, &world.trash).unwrap();
                world.assert_at(&world.source, &world.target);
            }
            State::Undone => {
                world.assert_at(&world.source, &world.target);
                assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);
            }
            other => panic!("crash point {n}: {other:?}"),
        }
    }
    assert!(points >= 10, "only {points} crash points");
}

/// A crash at every point of a redo: undone again before the sources go,
/// done after.
#[test]
fn a_crash_at_any_step_of_a_redo_loses_nothing() {
    let mut points = 0;
    for n in 0.. {
        let world = World::new();
        let journal = world.journal();
        let op = journal
            .execute("move", "Move", world.steps(), &world.trash)
            .unwrap();
        journal.undo(&op.id, &world.trash).unwrap();
        journal.mark_synced(&op.id).unwrap();
        drop(journal);
        crash::arm(n);
        let run = catch_unwind(AssertUnwindSafe(|| {
            world.journal().redo(&op.id, &world.trash)
        }));
        if crash::disarm() {
            run.expect("no crash").unwrap();
            break;
        }
        assert!(run.is_err(), "crash point {n}");
        points += 1;
        world.assert_nothing_lost();
        let journal = world.journal();
        let pending = journal.recover(&world.trash).unwrap();
        match journal.operation(&op.id).unwrap().state {
            State::Undone => {
                world.assert_at(&world.source, &world.target);
                assert!(pending.is_empty(), "records already follow undone");
                assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);
            }
            State::Done => {
                world.assert_at(&world.target, &world.source);
                assert!(journal.last_redoable().unwrap().is_none());
                assert_eq!(pending.len(), 1);
            }
            other => panic!("crash point {n}: {other:?}"),
        }
    }
    assert!(points >= 12, "only {points} crash points");
}

#[test]
fn stop_while_copying_rolls_back() {
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
    let journal = world.journal();
    let error = journal
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &StopAfterFirstBlock(Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Stopped"), "{error:#}");
    world.assert_at(&world.source, &world.target);
    assert_eq!(journal.history(1).unwrap()[0].state, State::RolledBack);
}

#[test]
fn a_source_changed_while_copying_stops_the_move() {
    struct Edit<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Edit<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                let mut bytes = fs::read(self.0).unwrap();
                bytes[0] ^= 1;
                fs::write(self.0, bytes).unwrap();
            }
        }
    }
    let world = World::new();
    let raw = world.source.join("A.RAF");
    let error = world
        .journal()
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &Edit(&raw, Cell::new(false)),
        )
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("differs") || text.contains("changed"),
        "{text}"
    );
    assert!(World::names(&world.target).is_empty());
    assert!(exists(&raw));
}

/// An original that changes after its copy was verified, or that cannot
/// go to the Trash, stays where it is and the note says so; undo then
/// only trashes the copy, and only while both are identical.
#[test]
fn an_original_that_changed_or_cannot_be_trashed_is_kept() {
    let world = World::new();
    let xmp = world.source.join("A.xmp");
    let edited = xmp.clone();
    *world.trash.on_trash.borrow_mut() = Some(Box::new(move |path: &Path| {
        if path.ends_with("A.RAF") {
            fs::write(&edited, b"written by another app").unwrap();
        }
    }));
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    assert_eq!(op.state, State::Done);
    let note = op.note.clone().unwrap();
    assert!(note.contains("A.xmp changed while it was moved"), "{note}");
    assert_eq!(fs::read(&xmp).unwrap(), b"written by another app");
    assert_eq!(
        fs::read(world.target.join("A.xmp")).unwrap(),
        content("A.xmp", FILES[2].1)
    );
    *world.trash.on_trash.borrow_mut() = None;
    // The kept one differs from its copy: undo refuses.
    let error = journal.undo(&op.id, &world.trash).unwrap_err();
    assert!(format!("{error:#}").contains("differ now"), "{error:#}");

    // A volume without a Trash keeps the originals.
    let world = World::new();
    world.trash.refuse.set(true);
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    assert!(
        op.note
            .as_deref()
            .unwrap()
            .contains("cannot go to the Trash")
    );
    for (name, len) in FILES {
        assert_eq!(
            fs::read(world.source.join(name)).unwrap(),
            content(name, len)
        );
        assert_eq!(
            fs::read(world.target.join(name)).unwrap(),
            content(name, len)
        );
    }
    // Undo trashes only the copies (the Trash of the target volume works).
    world.trash.refuse.set(false);
    journal.undo(&op.id, &world.trash).unwrap();
    world.assert_at(&world.source, &world.target);
}

#[test]
fn a_name_taken_while_copying_is_never_replaced() {
    struct Squatter<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Squatter<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                fs::write(self.0, b"someone else's").unwrap();
            }
        }
    }
    let world = World::new();
    let taken = world.target.join("A.xmp");
    let error = world
        .journal()
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &Squatter(&taken, Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("A.xmp"), "{error:#}");
    assert_eq!(fs::read(&taken).unwrap(), b"someone else's");
    assert_eq!(World::names(&world.target), ["A.xmp"]);
    for (name, len) in FILES {
        assert_eq!(
            fs::read(world.source.join(name)).unwrap(),
            content(name, len)
        );
    }
}

#[test]
fn folders_links_existing_targets_and_full_volumes_are_refused_before_copying() {
    let world = World::new();
    let journal = world.journal();
    let refused = |steps: Vec<Step>, expected: &str| {
        let error = journal
            .execute("move", "Move", steps, &world.trash)
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert!(World::names(&world.target).iter().all(|n| n == "B.RAF"));
    };
    fs::create_dir(world.source.join("Folder")).unwrap();
    refused(
        vec![Step::other_volume(
            world.source.join("Folder"),
            world.target.join("Folder"),
        )],
        "moving folders to another drive is not supported yet",
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(world.source.join("A.RAF"), world.source.join("L.RAF")).unwrap();
        refused(
            vec![Step::other_volume(
                world.source.join("L.RAF"),
                world.target.join("L.RAF"),
            )],
            "link",
        );
    }
    fs::write(world.target.join("B.RAF"), b"there").unwrap();
    refused(
        vec![Step::other_volume(
            world.source.join("A.RAF"),
            world.target.join("B.RAF"),
        )],
        "already exists",
    );
    // A sparse file larger than any free space here.
    let huge = world.source.join("HUGE.RAF");
    fs::File::create(&huge).unwrap().set_len(1 << 50).unwrap();
    refused(
        vec![Step::other_volume(&huge, world.target.join("HUGE.RAF"))],
        "do not fit",
    );
    assert_eq!(fs::read(world.target.join("B.RAF")).unwrap(), b"there");
}

/// A damaged journal naming paths outside a staging folder deletes nothing.
#[test]
fn staging_cleanup_never_removes_anything_outside_its_folder() {
    let world = World::new();
    let precious = world.target.join("A.RAF");
    fs::write(&precious, b"original").unwrap();
    let mut step = Step::other_volume(world.source.join("A.RAF"), &precious);
    if let Target::OtherVolume { copy, .. } = &mut step.to {
        copy.staged = Some(precious.clone());
    }
    let payload = Payload {
        steps: vec![step],
        staging_dirs: vec![
            world.target.clone(),
            world.target.join(format!("{MOVE_STAGING_PREFIX}x")),
        ],
        ..Payload::default()
    };
    let _ = remove_staging(&payload);
    assert_eq!(fs::read(&precious).unwrap(), b"original");
}

/// A real exFAT volume (Linux exfat driver: exclusive rename works), when
/// the environment names a folder on one: `RAWTONIC_TEST_EXFAT_DIR`.
#[test]
fn a_move_to_an_exfat_volume_when_one_is_given() {
    let Some(dir) = std::env::var_os("RAWTONIC_TEST_EXFAT_DIR") else {
        eprintln!("SKIP: RAWTONIC_TEST_EXFAT_DIR not set");
        return;
    };
    let target = tempfile::tempdir_in(dir).unwrap();
    let info = volume_info(target.path()).unwrap();
    assert_eq!(info.file_system, "exfat");
    let world = World::new();
    let to = target.path().join("A.RAF");
    let op = world
        .journal()
        .execute(
            "move",
            "Move",
            vec![Step::moving(world.source.join("A.RAF"), &to).unwrap()],
            &world.trash,
        )
        .unwrap();
    assert_eq!(op.state, State::Done);
    assert_eq!(fs::read(&to).unwrap(), content("A.RAF", FILES[0].1));
    assert!(!exists(&world.source.join("A.RAF")));
    world.journal().undo(&op.id, &world.trash).unwrap();
    assert_eq!(
        fs::read(world.source.join("A.RAF")).unwrap(),
        content("A.RAF", FILES[0].1)
    );
    assert!(!exists(&to));
}

/// Throughput of a move to another volume, for the measurement in G40:
/// `RAWTONIC_MOVE_BENCH_FROM` and `RAWTONIC_MOVE_BENCH_TO` name two
/// folders on different volumes (it works in new folders inside them),
/// `RAWTONIC_MOVE_BENCH_MB` the size of each of 20 files (default 50).
/// Moves them, undoes the move, prints both.
#[test]
#[ignore = "measurement; needs two volumes named by the environment"]
fn copy_move_timing() {
    let (Some(from), Some(to)) = (
        std::env::var_os("RAWTONIC_MOVE_BENCH_FROM"),
        std::env::var_os("RAWTONIC_MOVE_BENCH_TO"),
    ) else {
        panic!("set RAWTONIC_MOVE_BENCH_FROM and RAWTONIC_MOVE_BENCH_TO");
    };
    let mb: usize = std::env::var("RAWTONIC_MOVE_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    // Folders of its own, files created new: nothing there is replaced.
    let run = format!("rawtonic-move-bench-{}", uuid::Uuid::new_v4());
    let (from, to) = (PathBuf::from(from).join(&run), PathBuf::from(to).join(&run));
    fs::create_dir(&from).unwrap();
    fs::create_dir(&to).unwrap();
    assert!(
        !same_volume(&from, &to.join("x")).unwrap(),
        "two volumes needed"
    );
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(&journal_dir.path().join("journal.sqlite")).unwrap();
    let trash = FakeTrash::new();
    let source = from.join("Capture");
    fs::create_dir(&source).unwrap();
    let mut steps = Vec::new();
    for i in 0..20 {
        let name = format!("IMG_{i:04}.RAF");
        use std::io::Write;
        fs::File::create_new(source.join(&name))
            .unwrap()
            .write_all(&content(&name, mb << 20))
            .unwrap();
        steps.push(Step::moving(source.join(&name), to.join(&name)).unwrap());
    }
    assert!(
        steps
            .iter()
            .all(|s| matches!(s.to, Target::OtherVolume { .. }))
    );
    let total = 20.0 * mb as f64 * 1.048576;
    let start = std::time::Instant::now();
    let op = journal.execute("move", "Move", steps, &trash).unwrap();
    let moved = start.elapsed().as_secs_f64();
    let start = std::time::Instant::now();
    journal.undo(&op.id, &trash).unwrap();
    let undone = start.elapsed().as_secs_f64();
    eprintln!(
        "move {total:.0} MB: {moved:.2} s ({:.0} MB/s); undo {undone:.2} s ({:.0} MB/s)",
        total / moved,
        total / undone
    );
}

/// A crash after the copies were published, then the target drive changed
/// before the next start (a copy gone, another replaced): recovery trashes
/// only originals whose copy is still there with the same bytes.
#[test]
fn recovery_keeps_an_original_whose_copy_is_gone_or_changed() {
    let world = World::new();
    crash::arm_at("finishing");
    let run = catch_unwind(AssertUnwindSafe(|| {
        world
            .journal()
            .execute("move", "Move", world.steps(), &world.trash)
    }));
    assert!(!crash::disarm() && run.is_err());
    fs::remove_file(world.target.join("A.RAF")).unwrap();
    let xmp = world.target.join("A.xmp");
    let mut bytes = fs::read(&xmp).unwrap();
    bytes[0] ^= 1;
    fs::write(&xmp, bytes).unwrap();
    let journal = world.journal();
    journal.recover(&world.trash).unwrap();
    let op = journal.history(1).unwrap().remove(0);
    assert_eq!(op.state, State::Done);
    for (name, len) in [FILES[0], FILES[2]] {
        assert_eq!(
            fs::read(world.source.join(name)).unwrap(),
            content(name, len),
            "{name} kept"
        );
        assert!(op.note.as_deref().unwrap().contains(name), "{:?}", op.note);
    }
    assert!(
        !exists(&world.source.join("A.RAF.rrdata")),
        "an intact copy's original goes"
    );
    assert_eq!(world.trash.items().len(), 1);
}

/// The same for an undo: a copy back gone before recovery keeps the copy
/// at the target.
#[test]
fn recovery_of_an_undo_keeps_a_copy_whose_copy_back_is_gone() {
    let world = World::new();
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    drop(journal);
    crash::arm_at("finishing");
    let run = catch_unwind(AssertUnwindSafe(|| {
        world.journal().undo(&op.id, &world.trash)
    }));
    assert!(!crash::disarm() && run.is_err());
    fs::remove_file(world.source.join("A.RAF")).unwrap();
    let journal = world.journal();
    journal.recover(&world.trash).unwrap();
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Undone);
    assert_eq!(
        fs::read(world.target.join("A.RAF")).unwrap(),
        content("A.RAF", FILES[0].1),
        "the only copy left stays"
    );
    assert!(!exists(&world.target.join("A.xmp")));
}

/// A publish whose rename worked but whose folder could not be synced (a
/// folder that can be written but not read) is rolled back with the rest.
#[cfg(unix)]
#[test]
fn a_publish_that_fails_after_its_rename_is_rolled_back() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    fs::set_permissions(&world.target, fs::Permissions::from_mode(0o300)).unwrap();
    if fs::File::open(&world.target).is_ok() {
        fs::set_permissions(&world.target, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("SKIP: permissions do not apply (running as root)");
        return;
    }
    let journal = world.journal();
    let result = journal.execute("move", "Move", world.steps(), &world.trash);
    fs::set_permissions(&world.target, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(journal.history(1).unwrap()[0].state, State::RolledBack);
    world.assert_at(&world.source, &world.target);
}

/// Stop pressed while the last copy is being verified still stops the move.
#[test]
fn stop_during_the_last_verification_rolls_back() {
    /// Stop arrives after the copy loop's last look (the one that finds
    /// the end of the last file), i.e. while verifying.
    struct StopWhenCopied(Cell<bool>, Cell<u32>);
    impl CopyObserver for StopWhenCopied {
        fn copied(&self, done: u64, total: u64) {
            self.0.set(done == total);
        }
        fn cancelled(&self) -> bool {
            if self.0.get() {
                self.1.set(self.1.get() + 1);
            }
            self.1.get() >= 2
        }
    }
    let world = World::new();
    let journal = world.journal();
    let error = journal
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &StopWhenCopied(Cell::new(false), Cell::new(0)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Stopped"), "{error:#}");
    world.assert_at(&world.source, &world.target);
}
