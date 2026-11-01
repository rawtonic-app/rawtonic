//! "Move contents" of a session folder whose place changed (G38): every
//! entry of the old folder moves into the new one through the journal,
//! undoable, all or nothing.
use super::*;

/// Every file below `dir` with its bytes, by path relative to `dir`.
fn listing(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((
                    path.strip_prefix(dir).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn contents_move_on_one_volume_as_one_undoable_operation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (from, to) = (root.join("Capture"), root.join("Day 1"));
    fs::create_dir_all(from.join("Sub")).unwrap();
    fs::create_dir(&to).unwrap();
    for (name, len) in FILES {
        fs::write(from.join(name), content(name, len)).unwrap();
    }
    fs::write(from.join("Sub/B.NEF"), content("B", 99)).unwrap();
    let before = listing(&from);

    let operations = plan_move_contents(&from, &to).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].len(), FILES.len() + 1);
    let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
    let op = journal
        .execute(
            "move",
            "Move contents",
            operations[0].clone(),
            &NoSystemTrash,
        )
        .unwrap();
    assert_eq!(listing(&to), before);
    assert!(listing(&from).is_empty() && from.is_dir());

    journal.undo(&op.id, &NoSystemTrash).unwrap();
    assert_eq!(listing(&from), before);
    assert!(listing(&to).is_empty());
}

#[test]
fn contents_move_to_another_volume_verified_and_undoable() {
    let world = World::new();
    if world.publishes_in_place() {
        return;
    }
    fs::create_dir_all(world.source.join("Day 2")).unwrap();
    fs::write(world.source.join("Day 2/C.RAF"), content("C", 4096)).unwrap();
    let before = listing(&world.source);

    let operations = plan_move_contents(&world.source, &world.target).unwrap();
    // A folder going to another volume is an operation of its own.
    assert_eq!(operations.len(), if world.two_volumes { 2 } else { 1 });
    let journal = world.journal();
    let mut done = Vec::new();
    for steps in operations {
        let op = journal
            .execute("move", "Move contents", steps, &world.trash)
            .unwrap();
        done.push(op.id);
    }
    assert_eq!(listing(&world.target), before);
    assert!(listing(&world.source).is_empty());

    for id in done.iter().rev() {
        journal.undo(id, &world.trash).unwrap();
    }
    assert_eq!(listing(&world.source), before);
    assert!(listing(&world.target).is_empty());
}

#[test]
fn contents_move_nothing_when_a_name_is_taken_or_the_target_is_inside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (from, to) = (root.join("Capture"), root.join("New"));
    fs::create_dir_all(&from).unwrap();
    fs::create_dir_all(&to).unwrap();
    fs::write(from.join("A.RAF"), b"a").unwrap();
    fs::write(from.join("B.RAF"), b"b").unwrap();
    fs::write(to.join("B.RAF"), b"other").unwrap();
    let error = plan_move_contents(&from, &to).unwrap_err().to_string();
    assert!(error.contains("B.RAF"), "{error}");

    let inside = from.join("Day 2");
    fs::create_dir(&inside).unwrap();
    assert!(plan_move_contents(&from, &inside).is_err());
    assert!(plan_move_contents(&from, &from).is_err());
    assert!(plan_move_contents(&from, &root.join("missing")).is_err());

    fs::remove_file(to.join("B.RAF")).unwrap();
    if symlink_or_skip(&from.join("A.RAF"), &from.join("link.RAF")) {
        assert!(plan_move_contents(&from, &to).is_err());
    }
    // Nothing moved by any of these.
    assert!(from.join("A.RAF").is_file() && from.join("B.RAF").is_file());
    assert_eq!(fs::read_dir(&to).unwrap().count(), 0);
}

#[test]
fn an_empty_folder_plans_no_operation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    assert!(
        plan_move_contents(&root.join("a"), &root.join("b"))
            .unwrap()
            .is_empty()
    );
}
