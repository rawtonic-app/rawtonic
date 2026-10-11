use rawtonic_fileops::{Journal, State, Step, SystemTrash, Target, rename_no_clobber};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
};

/// A system Trash stand-in: a folder with numbered items.
struct FolderTrash {
    dir: PathBuf,
    next: Cell<u32>,
    fail_after: Option<u32>,
}

impl FolderTrash {
    fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir).unwrap();
        Self {
            dir: dir.to_path_buf(),
            next: Cell::new(0),
            fail_after: None,
        }
    }
}

impl SystemTrash for FolderTrash {
    fn trash(&self, path: &Path) -> anyhow::Result<String> {
        let n = self.next.get();
        if self.fail_after.is_some_and(|limit| n >= limit) {
            anyhow::bail!("Trash refused");
        }
        self.next.set(n + 1);
        let item = self.dir.join(n.to_string());
        rename_no_clobber(path, &item)?;
        Ok(item.to_string_lossy().into_owned())
    }
    fn restore(&self, item: &str, original: &Path) -> anyhow::Result<()> {
        Ok(rename_no_clobber(Path::new(item), original)?)
    }
    fn contains(&self, item: &str) -> bool {
        Path::new(item).exists()
    }
}

fn setup() -> (tempfile::TempDir, PathBuf, Journal) {
    let dir = tempfile::tempdir().unwrap();
    // Canonical, so that /var and /private/var do not differ on macOS.
    let root = dir.path().canonicalize().unwrap();
    let journal = Journal::open(&root.join("journal/file-operations.sqlite")).unwrap();
    (dir, root, journal)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn rename_with_sidecars_and_undo_restore_names_and_bytes() {
    let (_tmp, root, journal) = setup();
    let trash = FolderTrash::new(&root.join("system-trash"));
    write(&root.join("a.RAF"), "raw");
    write(&root.join("a.RAF.rrdata"), "edit");
    write(&root.join("a.xmp"), "xmp");
    let op = journal
        .execute(
            "rename",
            "Rename 1 photo",
            vec![
                Step::rename(root.join("a.RAF"), root.join("b.RAF")),
                Step::rename(root.join("a.RAF.rrdata"), root.join("b.RAF.rrdata")),
                Step::rename(root.join("a.xmp"), root.join("b.xmp")),
            ],
            &trash,
        )
        .unwrap();
    assert_eq!(op.state, State::Done);
    assert!(!op.synced);
    assert_eq!(fs::read_to_string(root.join("b.RAF")).unwrap(), "raw");
    assert!(!root.join("a.RAF").exists());
    assert_eq!(journal.last_undoable().unwrap().unwrap().id, op.id);

    let undone = journal.undo(&op.id, &trash).unwrap();
    assert_eq!(undone.state, State::Undone);
    assert_eq!(fs::read_to_string(root.join("a.RAF")).unwrap(), "raw");
    assert_eq!(
        fs::read_to_string(root.join("a.RAF.rrdata")).unwrap(),
        "edit"
    );
    assert_eq!(fs::read_to_string(root.join("a.xmp")).unwrap(), "xmp");
    assert!(!root.join("b.RAF").exists());
    assert!(journal.last_undoable().unwrap().is_none());
    // Undoing twice is refused.
    assert!(journal.undo(&op.id, &trash).is_err());
}

#[test]
fn an_existing_target_refuses_the_whole_plan() {
    let (_tmp, root, journal) = setup();
    write(&root.join("a.RAF"), "a");
    write(&root.join("c.RAF"), "c");
    write(&root.join("b.RAF"), "b");
    let error = journal
        .execute(
            "rename",
            "Rename",
            vec![
                Step::rename(root.join("a.RAF"), root.join("x.RAF")),
                Step::rename(root.join("c.RAF"), root.join("b.RAF")),
            ],
            &rawtonic_fileops::NoSystemTrash,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("already exists"), "{error:#}");
    assert_eq!(fs::read_to_string(root.join("a.RAF")).unwrap(), "a");
    assert_eq!(fs::read_to_string(root.join("b.RAF")).unwrap(), "b");
    assert!(!root.join("x.RAF").exists());
    // Refused before journaling.
    assert!(journal.history(10).unwrap().is_empty());
}

#[test]
fn chains_swaps_and_duplicate_targets_are_refused() {
    let (_tmp, root, journal) = setup();
    write(&root.join("a"), "a");
    write(&root.join("b"), "b");
    let none = &rawtonic_fileops::NoSystemTrash;
    // Swap: each target is the other's source.
    assert!(
        journal
            .execute(
                "rename",
                "Swap",
                vec![
                    Step::rename(root.join("a"), root.join("b")),
                    Step::rename(root.join("b"), root.join("a")),
                ],
                none,
            )
            .is_err()
    );
    // Two files onto one name.
    assert!(
        journal
            .execute(
                "rename",
                "Same name",
                vec![
                    Step::rename(root.join("a"), root.join("c")),
                    Step::rename(root.join("b"), root.join("c")),
                ],
                none,
            )
            .is_err()
    );
    // A folder into itself.
    fs::create_dir(root.join("dir")).unwrap();
    assert!(
        journal
            .execute(
                "move",
                "Into itself",
                vec![Step::rename(root.join("dir"), root.join("dir/inner"))],
                none,
            )
            .is_err()
    );
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
    assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
    assert!(!root.join("c").exists());
}

#[test]
fn a_failing_step_rolls_back_the_applied_ones() {
    let (_tmp, root, journal) = setup();
    let mut trash = FolderTrash::new(&root.join("system-trash"));
    trash.fail_after = Some(1);
    write(&root.join("a.RAF"), "a");
    write(&root.join("b.RAF"), "b");
    write(&root.join("c.RAF"), "c");
    let error = journal
        .execute(
            "trash",
            "Trash 3 photos",
            vec![
                Step::rename(root.join("a.RAF"), root.join("Trash/a.RAF")),
                Step::system_trash(root.join("b.RAF")),
                Step::system_trash(root.join("c.RAF")),
            ],
            &trash,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Trash refused"), "{error:#}");
    for name in ["a.RAF", "b.RAF", "c.RAF"] {
        assert!(root.join(name).is_file(), "{name} was not put back");
    }
    // The created Trash folder is gone again, the trash stand-in is empty.
    assert!(!root.join("Trash").exists());
    assert_eq!(fs::read_dir(root.join("system-trash")).unwrap().count(), 0);
    let history = journal.history(1).unwrap();
    assert_eq!(history[0].state, State::RolledBack);
    assert!(
        history[0]
            .note
            .as_deref()
            .unwrap()
            .contains("Trash refused")
    );
}

#[test]
fn system_trash_round_trip_records_the_item() {
    let (_tmp, root, journal) = setup();
    let trash = FolderTrash::new(&root.join("system-trash"));
    write(&root.join("a.RAF"), "a");
    let op = journal
        .execute(
            "trash",
            "Trash",
            vec![Step::system_trash(root.join("a.RAF"))],
            &trash,
        )
        .unwrap();
    let Target::SystemTrash { item: Some(item) } = &op.steps[0].to else {
        panic!("item not recorded: {:?}", op.steps[0]);
    };
    assert!(Path::new(item).is_file());
    assert_eq!(op.trashed(), vec![root.join("a.RAF")]);
    journal.undo(&op.id, &trash).unwrap();
    assert_eq!(fs::read_to_string(root.join("a.RAF")).unwrap(), "a");
}

#[test]
fn undo_is_refused_when_something_moved_on_and_changes_nothing() {
    let (_tmp, root, journal) = setup();
    let trash = FolderTrash::new(&root.join("system-trash"));
    write(&root.join("a"), "a");
    write(&root.join("b"), "b");
    let op = journal
        .execute(
            "move",
            "Move",
            vec![
                Step::rename(root.join("a"), root.join("sub/a")),
                Step::rename(root.join("b"), root.join("sub/b")),
            ],
            &trash,
        )
        .unwrap();
    // Someone created a new "a" where the original was.
    write(&root.join("a"), "new");
    let error = journal.undo(&op.id, &trash).unwrap_err();
    assert!(format!("{error:#}").contains("exists again"), "{error:#}");
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "new");
    assert_eq!(fs::read_to_string(root.join("sub/a")).unwrap(), "a");
    assert_eq!(fs::read_to_string(root.join("sub/b")).unwrap(), "b");
    assert_eq!(journal.operation(&op.id).unwrap().state, State::Done);

    // A moved-away target refuses too.
    fs::remove_file(root.join("a")).unwrap();
    fs::rename(root.join("sub/b"), root.join("elsewhere")).unwrap();
    assert!(journal.undo(&op.id, &trash).is_err());
    assert_eq!(fs::read_to_string(root.join("sub/a")).unwrap(), "a");
}

#[test]
fn undo_removes_folders_the_operation_created_when_empty() {
    let (_tmp, root, journal) = setup();
    let none = &rawtonic_fileops::NoSystemTrash;
    write(&root.join("a"), "a");
    let op = journal
        .execute(
            "trash",
            "Trash",
            vec![Step::rename(root.join("a"), root.join("Trash/Capture/a"))],
            none,
        )
        .unwrap();
    assert_eq!(
        op.created_dirs,
        vec![root.join("Trash"), root.join("Trash/Capture")]
    );
    journal.undo(&op.id, none).unwrap();
    assert!(!root.join("Trash").exists());
    assert!(root.join("a").is_file());
}

#[test]
fn folders_rename_and_undo() {
    let (_tmp, root, journal) = setup();
    let none = &rawtonic_fileops::NoSystemTrash;
    write(&root.join("Day 1/a.RAF"), "a");
    let op = journal
        .execute(
            "renameFolder",
            "Rename folder",
            vec![Step::rename(root.join("Day 1"), root.join("Day one"))],
            none,
        )
        .unwrap();
    assert!(root.join("Day one/a.RAF").is_file());
    journal.undo(&op.id, none).unwrap();
    assert!(root.join("Day 1/a.RAF").is_file());
    assert!(!root.join("Day one").exists());
}

#[test]
fn case_only_rename_works_on_case_insensitive_volumes() {
    let (_tmp, root, journal) = setup();
    let none = &rawtonic_fileops::NoSystemTrash;
    write(&root.join("dscf0001.raf"), "a");
    let op = journal
        .execute(
            "rename",
            "Rename",
            vec![Step::rename(
                root.join("dscf0001.raf"),
                root.join("DSCF0001.RAF"),
            )],
            none,
        )
        .unwrap();
    let names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".raf") || n.ends_with(".RAF"))
        .collect();
    assert_eq!(names, vec!["DSCF0001.RAF".to_string()]);
    journal.undo(&op.id, none).unwrap();
    assert!(
        fs::read_dir(&root)
            .unwrap()
            .any(|e| e.unwrap().file_name() == "dscf0001.raf")
    );
}

#[test]
fn rename_no_clobber_never_replaces() {
    let (_tmp, root, _journal) = setup();
    write(&root.join("a"), "a");
    write(&root.join("b"), "b");
    let error = rename_no_clobber(&root.join("a"), &root.join("b")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
}

#[test]
fn journal_survives_reopening_and_tracks_sync() {
    let (_tmp, root, journal) = setup();
    let none = &rawtonic_fileops::NoSystemTrash;
    write(&root.join("a"), "a");
    let op = journal
        .execute(
            "rename",
            "Rename",
            vec![Step::rename(root.join("a"), root.join("b"))],
            none,
        )
        .unwrap();
    drop(journal);
    let journal = Journal::open(&root.join("journal/file-operations.sqlite")).unwrap();
    // Not yet synced: recovery hands it back to the caller.
    let pending = journal.recover(none).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, op.id);
    assert_eq!(pending[0].renames(), vec![(root.join("a"), root.join("b"))]);
    // Until a sync succeeds it stays pending for the next try, too.
    assert_eq!(journal.unsynced().unwrap().len(), 1);
    journal.mark_synced(&op.id).unwrap();
    assert!(journal.recover(none).unwrap().is_empty());
    assert!(journal.unsynced().unwrap().is_empty());
    // Undo makes it unsynced again.
    journal.undo(&op.id, none).unwrap();
    assert_eq!(journal.recover(none).unwrap().len(), 1);
    assert_eq!(journal.unsynced().unwrap()[0].state, State::Undone);
}
