//! Redo of undone operations and emptying a folder (a session's Trash).
use rawtonic_fileops::{
    Journal, NoSystemTrash, State, Step, SystemTrash, Target, plan_purge, rename_no_clobber,
};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
};

/// A system Trash stand-in: a folder with numbered items.
struct FolderTrash {
    dir: PathBuf,
    next: Cell<u32>,
}

impl FolderTrash {
    fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir).unwrap();
        Self {
            dir: dir.to_path_buf(),
            next: Cell::new(0),
        }
    }
}

impl SystemTrash for FolderTrash {
    fn trash(&self, path: &Path) -> anyhow::Result<String> {
        let n = self.next.get();
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
    let root = dir.path().canonicalize().unwrap();
    let journal = Journal::open(&root.join("journal/file-operations.sqlite")).unwrap();
    (dir, root, journal)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn redo_after_undo_moves_everything_again_and_undo_works_after_it() {
    let (_tmp, root, journal) = setup();
    let none = &NoSystemTrash;
    write(&root.join("Capture/a.RAF"), "raw");
    write(&root.join("Capture/a.RAF.rrdata"), "edit");
    let op = journal
        .execute(
            "move",
            "Move 1 photo",
            vec![
                Step::rename(root.join("Capture/a.RAF"), root.join("Selects/a.RAF")),
                Step::rename(
                    root.join("Capture/a.RAF.rrdata"),
                    root.join("Selects/a.RAF.rrdata"),
                ),
            ],
            none,
        )
        .unwrap();
    assert!(journal.last_redoable().unwrap().is_none(), "nothing undone");
    journal.undo(&op.id, none).unwrap();
    assert!(
        !root.join("Selects").exists(),
        "created folder went on undo"
    );
    assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);

    let redone = journal.redo(&op.id, none).unwrap();
    assert_eq!(redone.state, State::Done);
    assert!(!redone.synced, "the records must follow the redo");
    assert_eq!(read(&root.join("Selects/a.RAF")), "raw");
    assert_eq!(read(&root.join("Selects/a.RAF.rrdata")), "edit");
    assert!(!root.join("Capture/a.RAF").exists());
    // The folder is made again and recorded, so the next undo removes it.
    assert_eq!(redone.created_dirs, vec![root.join("Selects")]);
    assert!(journal.last_redoable().unwrap().is_none());
    assert_eq!(journal.last_undoable().unwrap().unwrap().id, op.id);
    // Redoing twice is refused.
    assert!(journal.redo(&op.id, none).is_err());

    journal.undo(&op.id, none).unwrap();
    assert_eq!(read(&root.join("Capture/a.RAF")), "raw");
    assert!(!root.join("Selects").exists());
}

#[test]
fn redo_follows_the_undo_order_and_a_new_operation_clears_it() {
    let (_tmp, root, journal) = setup();
    let none = &NoSystemTrash;
    write(&root.join("a"), "a");
    write(&root.join("x"), "x");
    let first = journal
        .execute(
            "rename",
            "a→b",
            vec![Step::rename(root.join("a"), root.join("b"))],
            none,
        )
        .unwrap();
    let second = journal
        .execute(
            "rename",
            "b→c",
            vec![Step::rename(root.join("b"), root.join("c"))],
            none,
        )
        .unwrap();
    journal
        .undo(&journal.last_undoable().unwrap().unwrap().id, none)
        .unwrap();
    journal
        .undo(&journal.last_undoable().unwrap().unwrap().id, none)
        .unwrap();
    assert!(root.join("a").is_file());
    // Undone last (the first) is redone first.
    let next = journal.last_redoable().unwrap().unwrap();
    assert_eq!(next.id, first.id);
    journal.redo(&next.id, none).unwrap();
    assert!(root.join("b").is_file());
    let next = journal.last_redoable().unwrap().unwrap();
    assert_eq!(next.id, second.id);

    // A new operation ends the redo stack.
    journal
        .execute(
            "rename",
            "x→y",
            vec![Step::rename(root.join("x"), root.join("y"))],
            none,
        )
        .unwrap();
    assert!(journal.last_redoable().unwrap().is_none());
    let error = journal.redo(&second.id, none).unwrap_err();
    assert!(
        format!("{error:#}").contains("another file operation"),
        "{error:#}"
    );
    assert!(root.join("b").is_file() && !root.join("c").exists());
}

#[test]
fn redo_is_refused_when_a_target_was_taken_and_changes_nothing() {
    let (_tmp, root, journal) = setup();
    let none = &NoSystemTrash;
    write(&root.join("a.RAF"), "a");
    write(&root.join("c.RAF"), "c");
    let op = journal
        .execute(
            "rename",
            "Rename 2 photos",
            vec![
                Step::rename(root.join("a.RAF"), root.join("b.RAF")),
                Step::rename(root.join("c.RAF"), root.join("d.RAF")),
            ],
            none,
        )
        .unwrap();
    journal.undo(&op.id, none).unwrap();
    // Something else now has the second target's name.
    write(&root.join("d.RAF"), "someone else's");
    let error = journal.redo(&op.id, none).unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("already exists") && text.contains("Nothing was redone"),
        "{text}"
    );
    assert_eq!(read(&root.join("a.RAF")), "a");
    assert_eq!(read(&root.join("c.RAF")), "c");
    assert_eq!(read(&root.join("d.RAF")), "someone else's");
    assert!(!root.join("b.RAF").exists());
    // Still undone and on the redo stack: it can be redone once the way is free.
    let op_now = journal.operation(&op.id).unwrap();
    assert_eq!(op_now.state, State::Undone);
    assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);

    // A source gone since the undo refuses too.
    fs::remove_file(root.join("d.RAF")).unwrap();
    fs::rename(root.join("c.RAF"), root.join("moved.RAF")).unwrap();
    let error = journal.redo(&op.id, none).unwrap_err();
    assert!(format!("{error:#}").contains("was not found"), "{error:#}");
    assert!(root.join("a.RAF").is_file() && !root.join("b.RAF").exists());

    fs::rename(root.join("moved.RAF"), root.join("c.RAF")).unwrap();
    journal.redo(&op.id, none).unwrap();
    assert_eq!(read(&root.join("b.RAF")), "a");
    assert_eq!(read(&root.join("d.RAF")), "c");
}

#[test]
fn redo_of_a_system_trash_step_records_the_new_item() {
    let (_tmp, root, journal) = setup();
    let trash = FolderTrash::new(&root.join("system-trash"));
    write(&root.join("a.RAF"), "a");
    let op = journal
        .execute(
            "trash",
            "Delete 1 photo",
            vec![Step::system_trash(root.join("a.RAF"))],
            &trash,
        )
        .unwrap();
    journal.undo(&op.id, &trash).unwrap();
    let redone = journal.redo(&op.id, &trash).unwrap();
    let Target::SystemTrash { item: Some(item) } = &redone.steps[0].to else {
        panic!("item not recorded: {:?}", redone.steps[0]);
    };
    assert_eq!(Path::new(item), root.join("system-trash").join("1"));
    assert_eq!(read(Path::new(item)), "a");
    journal.undo(&op.id, &trash).unwrap();
    assert_eq!(read(&root.join("a.RAF")), "a");
}

#[test]
fn a_failing_redo_step_rolls_back_and_stays_redoable() {
    let (_tmp, root, journal) = setup();
    let none = &NoSystemTrash;
    write(&root.join("a"), "a");
    write(&root.join("b"), "b");
    let op = journal
        .execute(
            "trash",
            "Delete 2",
            vec![
                Step::rename(root.join("a"), root.join("Trash/a")),
                Step::system_trash(root.join("b")),
            ],
            &FolderTrash::new(&root.join("system-trash")),
        )
        .unwrap();
    journal
        .undo(&op.id, &FolderTrash::new(&root.join("system-trash")))
        .unwrap();
    // Redo without a system Trash: the second step fails, the first goes back.
    let error = journal.redo(&op.id, none).unwrap_err();
    assert!(
        format!("{error:#}").contains("Nothing was redone"),
        "{error:#}"
    );
    assert!(root.join("a").is_file() && root.join("b").is_file());
    assert!(!root.join("Trash").exists());
    let now = journal.operation(&op.id).unwrap();
    assert_eq!(now.state, State::Undone);
    assert!(now.note.unwrap().contains("Redo failed"));
    assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);
}

fn session_trash(root: &Path) -> PathBuf {
    let trash = root.join("Job/Trash");
    write(&trash.join("Capture/A.RAF"), "a");
    write(&trash.join("Capture/A.RAF.rrdata"), "edit");
    write(&trash.join("Capture/1/A.RAF"), "older a");
    write(&trash.join("B.ARW"), "b");
    trash
}

#[test]
fn emptying_a_folder_deletes_only_what_is_in_it_and_is_not_undoable() {
    let (_tmp, root, journal) = setup();
    let none = &NoSystemTrash;
    write(&root.join("Job/Capture/keep.RAF"), "keep");
    // An earlier change and one undone (on the redo stack).
    let earlier = journal
        .execute(
            "rename",
            "Rename",
            vec![Step::rename(
                root.join("Job/Capture/keep.RAF"),
                root.join("Job/Capture/kept.RAF"),
            )],
            none,
        )
        .unwrap();
    write(&root.join("Job/Capture/x"), "x");
    let undone = journal
        .execute(
            "rename",
            "x→y",
            vec![Step::rename(
                root.join("Job/Capture/x"),
                root.join("Job/Capture/y"),
            )],
            none,
        )
        .unwrap();
    journal.undo(&undone.id, none).unwrap();
    let trash = session_trash(&root);

    let plan = plan_purge(&trash).unwrap();
    assert_eq!(
        plan.entries,
        vec![trash.join("B.ARW"), trash.join("Capture")]
    );
    assert_eq!(
        plan.files,
        vec![
            trash.join("B.ARW"),
            trash.join("Capture/1/A.RAF"),
            trash.join("Capture/A.RAF"),
            trash.join("Capture/A.RAF.rrdata"),
        ]
    );
    let op = journal.purge("emptyTrash", "Empty Trash", &plan).unwrap();
    assert_eq!(op.state, State::Purged);
    assert!(!op.synced, "the caller's records follow it");
    assert_eq!(op.removed, plan.files);
    assert_eq!(op.purged(), plan.entries);
    assert!(trash.is_dir(), "the Trash folder itself stays");
    assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
    assert_eq!(read(&root.join("Job/Capture/kept.RAF")), "keep");
    assert!(root.join("Job/Capture/x").is_file());

    // Not undoable, and the end of Undo and Redo for what came before.
    assert!(journal.last_undoable().unwrap().is_none());
    assert!(journal.last_redoable().unwrap().is_none());
    let error = journal.undo(&earlier.id, none).unwrap_err();
    assert!(
        format!("{error:#}").contains("Trash was emptied"),
        "{error:#}"
    );
    assert!(journal.undo(&op.id, none).is_err());
    assert!(journal.redo(&undone.id, none).is_err());
    assert_eq!(journal.unsynced().unwrap().last().unwrap().id, op.id);

    // A new change after it is undoable as usual.
    let later = journal
        .execute(
            "rename",
            "Rename",
            vec![Step::rename(
                root.join("Job/Capture/kept.RAF"),
                root.join("Job/Capture/k.RAF"),
            )],
            none,
        )
        .unwrap();
    assert_eq!(journal.last_undoable().unwrap().unwrap().id, later.id);
}

#[test]
fn a_folder_that_changed_since_it_was_listed_is_not_emptied() {
    let (_tmp, root, journal) = setup();
    let trash = session_trash(&root);
    let plan = plan_purge(&trash).unwrap();
    // A photo deleted into the Trash after the count was shown.
    write(&trash.join("Capture/C.RAF"), "c");
    let error = journal
        .purge("emptyTrash", "Empty Trash", &plan)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("changed since it was listed"),
        "{error:#}"
    );
    assert!(trash.join("Capture/A.RAF").is_file() && trash.join("Capture/C.RAF").is_file());
    assert!(
        journal.history(1).unwrap().is_empty(),
        "refused before journaling"
    );

    // An empty folder has nothing to empty.
    let empty = root.join("Empty");
    fs::create_dir(&empty).unwrap();
    assert!(
        journal
            .purge("emptyTrash", "Empty", &plan_purge(&empty).unwrap())
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn links_are_deleted_never_followed() {
    use std::os::unix::fs::symlink;
    let (_tmp, root, journal) = setup();
    let trash = root.join("Job/Trash");
    fs::create_dir_all(&trash).unwrap();
    // Outside the Trash: must survive.
    write(&root.join("Originals/A.RAF"), "original");
    symlink(root.join("Originals"), trash.join("linked folder")).unwrap();
    symlink(root.join("Originals/A.RAF"), trash.join("linked.RAF")).unwrap();
    write(&trash.join("Capture/B.RAF"), "b");
    symlink(root.join("Originals"), trash.join("Capture/inner link")).unwrap();

    let plan = plan_purge(&trash).unwrap();
    assert!(
        plan.files.iter().all(|f| f.starts_with(&trash)),
        "nothing outside is listed: {:?}",
        plan.files
    );
    assert!(plan.files.contains(&trash.join("linked folder")));
    assert!(
        !plan
            .files
            .iter()
            .any(|f| f.ends_with("linked folder/A.RAF"))
    );
    journal.purge("emptyTrash", "Empty Trash", &plan).unwrap();
    assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
    assert_eq!(read(&root.join("Originals/A.RAF")), "original");
}

#[cfg(unix)]
#[test]
fn only_a_real_folder_by_its_real_path_can_be_emptied() {
    use std::os::unix::fs::symlink;
    let (_tmp, root, _journal) = setup();
    write(&root.join("Originals/A.RAF"), "original");
    fs::create_dir_all(root.join("Job")).unwrap();
    // The Trash itself a link to the originals.
    symlink(root.join("Originals"), root.join("Job/Trash")).unwrap();
    let error = plan_purge(&root.join("Job/Trash")).unwrap_err();
    assert!(format!("{error:#}").contains("is a link"), "{error:#}");
    // A link on the way to it.
    symlink(root.join("Originals"), root.join("Linked")).unwrap();
    fs::create_dir_all(root.join("Originals/Trash")).unwrap();
    let error = plan_purge(&root.join("Linked/Trash")).unwrap_err();
    assert!(format!("{error:#}").contains("real path"), "{error:#}");
    // Traversal.
    fs::create_dir_all(root.join("Job2/Trash")).unwrap();
    let error = plan_purge(&root.join("Job2/Trash/../../Originals")).unwrap_err();
    assert!(format!("{error:#}").contains("real path"), "{error:#}");
    assert!(plan_purge(Path::new("Job2/Trash")).is_err(), "relative");
    assert_eq!(read(&root.join("Originals/A.RAF")), "original");
}

#[test]
fn a_plan_naming_anything_outside_the_folder_deletes_nothing() {
    let (_tmp, root, journal) = setup();
    let trash = session_trash(&root);
    write(&root.join("Originals/A.RAF"), "original");
    let mut plan = plan_purge(&trash).unwrap();
    // A plan edited after listing: refused because it no longer matches.
    plan.entries.push(root.join("Originals"));
    plan.entries.push(trash.join("../../Originals"));
    assert!(journal.purge("emptyTrash", "Empty Trash", &plan).is_err());
    assert_eq!(read(&root.join("Originals/A.RAF")), "original");
    assert!(trash.join("B.ARW").is_file());
}
