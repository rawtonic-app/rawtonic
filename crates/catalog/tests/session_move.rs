//! A whole session moved to another folder (G40, option (c)): planned by
//! `session_move`, run as one journaled `rawtonic_fileops` step, reopened at
//! its new place with every photograph, rating and edit; refused moves leave
//! the session where and as it was.
use rawtonic_catalog::{Library, Mode, session_move::plan_session_move};
use rawtonic_fileops::{Journal, Step, SystemTrash, Target, rename_no_clobber};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
};

/// A system Trash of folders, one per drive as a real one has (the
/// originals of a copy move go there): `dir` on the temporary folder's
/// drive, `other` on the second one.
struct FolderTrash {
    dir: PathBuf,
    other: PathBuf,
    next: Cell<usize>,
}

impl SystemTrash for FolderTrash {
    fn trash(&self, path: &Path) -> anyhow::Result<String> {
        let parent = self.other.parent().unwrap();
        let dir = if path.starts_with(parent) {
            &self.other
        } else {
            &self.dir
        };
        let item = dir.join(self.next.get().to_string());
        self.next.set(self.next.get() + 1);
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

struct World {
    _dirs: Vec<tempfile::TempDir>,
    /// The folder the session is in.
    jobs: PathBuf,
    /// A folder to move it to, on another volume when one is at hand.
    archive: PathBuf,
    journal: Journal,
    trash: FolderTrash,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        // `/dev/shm` is another file system than the temporary folder on
        // Linux (as on CI); elsewhere the copy move is forced below.
        let shm = Path::new("/dev/shm");
        let other = if shm.is_dir() {
            tempfile::tempdir_in(shm).unwrap()
        } else {
            tempfile::tempdir().unwrap()
        };
        let jobs = root.join("Jobs");
        let archive = other.path().canonicalize().unwrap().join("Archive");
        fs::create_dir_all(&jobs).unwrap();
        fs::create_dir_all(&archive).unwrap();
        let trash = FolderTrash {
            dir: root.join("Trash"),
            other: archive.with_file_name("Trash"),
            next: Cell::new(0),
        };
        fs::create_dir_all(&trash.dir).unwrap();
        fs::create_dir_all(&trash.other).unwrap();
        let journal = Journal::open(&root.join("journal/file-operations.sqlite")).unwrap();
        Self {
            _dirs: vec![home, other],
            jobs,
            archive,
            journal,
            trash,
        }
    }

    /// The session `S7Job` with two rated, edited photographs (one in
    /// Selects) and a file the catalog does not know; returns its folder.
    fn session(&self) -> PathBuf {
        let dir = self.jobs.join("S7Job");
        let lib = Library::create(&dir, "S7Job", Mode::Session).unwrap();
        fs::create_dir_all(dir.join("Selects")).unwrap();
        for (seed, name) in [(1u8, "Capture/A.NEF"), (2, "Selects/B.NEF")] {
            let path = dir.join(name);
            fs::write(&path, photo(seed)).unwrap();
            let asset = lib.add_asset(&path).unwrap();
            let mut meta = lib.metadata(&asset.id).unwrap();
            meta.rating = seed + 2;
            lib.set_metadata(&asset.id, &meta).unwrap();
            lib.save_editor_state(
                &asset.id,
                &serde_json::json!({"adjustments": {"exposure": seed}}),
            )
            .unwrap();
        }
        fs::write(dir.join("Output/notes.txt"), b"client notes").unwrap();
        dir
    }

    fn run(&self, step: Step) -> anyhow::Result<rawtonic_fileops::Operation> {
        self.journal
            .execute("moveSession", "Move session S7Job", vec![step], &self.trash)
    }
}

/// Larger than the fingerprint's two 64 KiB samples.
fn photo(seed: u8) -> Vec<u8> {
    (0..200_000)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

/// Every file below `dir` with its bytes, except the database's WAL files
/// (which come and go with connections).
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), out);
            } else {
                out.push(entry.path());
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    let mut files: Vec<_> = files
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            !name.ends_with("-wal") && !name.ends_with("-shm")
        })
        .map(|p| {
            (
                p.strip_prefix(dir).unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

/// The session at `dir` opens with both photographs present, rated and
/// edited, at their places below `dir`.
fn assert_opens_whole(dir: &Path) {
    let lib = Library::open(dir).unwrap();
    assert_eq!(lib.manifest.name, "S7Job");
    let assets = lib.assets(None).unwrap();
    assert_eq!(assets.len(), 2);
    for (seed, name) in [(1u8, "Capture/A.NEF"), (2, "Selects/B.NEF")] {
        let path = dir.join(name);
        let asset = lib.asset_at(&path).unwrap().expect("registered");
        assert!(!asset.missing, "{name} is missing");
        assert_eq!(asset.path, path);
        assert_eq!(fs::read(&path).unwrap(), photo(seed));
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, seed + 2);
        let state = lib.editor_state(&asset.id).unwrap().expect("edited");
        assert_eq!(state["adjustments"]["exposure"], seed);
    }
    assert_eq!(
        fs::read(dir.join("Output/notes.txt")).unwrap(),
        b"client notes"
    );
    assert_eq!(lib.summary().unwrap().missing, 0);
}

#[test]
fn a_session_moved_on_its_drive_reopens_whole_at_its_new_place_and_undo_brings_it_back() {
    let world = World::new();
    let session = world.session();
    let other = world.jobs.join("Done");
    fs::create_dir(&other).unwrap();
    let plan = plan_session_move(&session, &other).unwrap();
    assert_eq!(plan.to, other.join("S7Job"));
    assert_eq!(plan.name, "S7Job");
    let step = Step::moving(&plan.from, &plan.to).unwrap();
    assert!(matches!(step.to, Target::Path { .. }), "{:?}", step.to);
    let op = world.run(step).unwrap();
    assert!(!session.exists());
    assert_opens_whole(&plan.to);

    world.journal.undo(&op.id, &world.trash).unwrap();
    assert!(!plan.to.exists());
    assert_opens_whole(&session);
}

/// A catalog that added a session's photographs (absolute places) follows
/// the session's move as the app's catalog sync does for the open catalog
/// (`Library::relocate` of the session folder): same assets, metadata and
/// albums at the new place; undo brings the records back too.
#[test]
fn a_catalog_holding_a_sessions_photos_follows_its_move_and_undo() {
    let world = World::new();
    let session = world.session();
    let catalog_dir = world.jobs.join("Catalog");
    let catalog = Library::create(&catalog_dir, "Catalog", Mode::Catalog).unwrap();
    let photos = [session.join("Capture/A.NEF"), session.join("Selects/B.NEF")];
    catalog.add_files(&photos).unwrap();
    let ids: Vec<String> = photos
        .iter()
        .map(|p| catalog.asset_at(p).unwrap().expect("added").id)
        .collect();
    let album = catalog.create_collection("Client picks", None).unwrap();
    catalog.add_to_collection(&album.id, &ids).unwrap();
    let mut meta = catalog.metadata(&ids[0]).unwrap();
    meta.rating = 5;
    catalog.set_metadata(&ids[0], &meta).unwrap();

    let other = world.jobs.join("Done");
    fs::create_dir(&other).unwrap();
    let plan = plan_session_move(&session, &other).unwrap();
    let op = world
        .run(Step::moving(&plan.from, &plan.to).unwrap())
        .unwrap();
    assert_eq!(catalog.relocate(&session, &plan.to).unwrap(), 2);
    let at = |dir: &Path| {
        let mut places: Vec<(String, PathBuf, bool)> = catalog
            .collection_assets(&album.id)
            .unwrap()
            .into_iter()
            .map(|a| {
                (
                    a.id,
                    a.path.strip_prefix(dir).unwrap().to_path_buf(),
                    a.missing,
                )
            })
            .collect();
        places.sort();
        places
    };
    let mut expected: Vec<(String, PathBuf, bool)> = ids
        .iter()
        .zip(["Capture/A.NEF", "Selects/B.NEF"])
        .map(|(id, name)| (id.clone(), PathBuf::from(name), false))
        .collect();
    expected.sort();
    assert_eq!(at(&plan.to), expected);
    assert_eq!(catalog.metadata(&ids[0]).unwrap().rating, 5);
    assert_eq!(catalog.summary().unwrap().missing, 0);

    world.journal.undo(&op.id, &world.trash).unwrap();
    assert_eq!(catalog.relocate(&plan.to, &session).unwrap(), 2);
    assert_eq!(at(&session), expected);
    assert_opens_whole(&session);
}

#[test]
fn a_session_moved_to_another_drive_is_copied_verified_and_its_original_trashed() {
    let world = World::new();
    let session = world.session();
    let before = snapshot(&session);
    let plan = plan_session_move(&session, &world.archive).unwrap();
    // A real second volume where there is one; the copy move otherwise.
    let step = match Step::moving(&plan.from, &plan.to).unwrap() {
        step @ Step {
            to: Target::OtherVolume { .. },
            ..
        } => step,
        _ => Step::other_volume(&plan.from, &plan.to),
    };
    let op = world.run(step).unwrap();
    assert!(!session.exists());
    // The checked copy holds exactly what the session held.
    assert_eq!(snapshot(&plan.to), before);
    assert_opens_whole(&plan.to);
    // The original went to the Trash as one folder, whole.
    let trashed = world.trash.dir.join("0");
    assert_eq!(snapshot(&trashed), before);

    let undone = world.journal.undo(&op.id, &world.trash).unwrap();
    assert!(!plan.to.exists(), "{:?}", undone.note);
    assert_opens_whole(&session);
}

#[test]
fn refused_session_moves_leave_the_session_where_and_as_it_was() {
    let world = World::new();
    let session = world.session();
    let before = snapshot(&session);
    let refused = |destination: &Path, expected: &str| {
        let error = plan_session_move(&session, destination).unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert_eq!(snapshot(&session), before);
    };

    // Into itself.
    refused(&session.join("Capture"), "into its own folder");
    // Into a session or catalog: libraries do not nest.
    let other = world.jobs.join("Other");
    Library::create(&other, "Other", Mode::Session).unwrap();
    refused(&other.join("Capture"), "inside another session or catalog");
    // A name clash: something of the session's name is there.
    let clash = world.archive.join("S7Job");
    fs::create_dir(&clash).unwrap();
    fs::write(clash.join("keep.txt"), b"theirs").unwrap();
    refused(&world.archive, "already holds something named S7Job");
    assert_eq!(fs::read(clash.join("keep.txt")).unwrap(), b"theirs");
    fs::remove_file(clash.join("keep.txt")).unwrap();
    fs::remove_dir(&clash).unwrap();
    // Open in another program: a connection elsewhere holds the database.
    let open = Library::open(&session).unwrap();
    refused(&world.archive, "open in another program");
    drop(open);
    // A catalog is not moved as a folder.
    let catalog = world.jobs.join("Main.catalog");
    Library::create(&catalog, "Main", Mode::Catalog).unwrap();
    let error = plan_session_move(&catalog, &world.archive).unwrap_err();
    assert!(
        format!("{error:#}").contains("holds no Rawtonic session"),
        "{error:#}"
    );

    // Once nothing holds it, the same move is planned.
    assert!(plan_session_move(&session, &world.archive).is_ok());
}

#[test]
fn a_session_that_does_not_fit_on_the_other_drive_is_not_moved() {
    let world = World::new();
    let session = world.session();
    // A sparse file far larger than any disk: the move refuses before
    // copying anything.
    let huge = fs::File::create(session.join("Capture/HUGE.NEF")).unwrap();
    assert!(
        [50, 46, 43, 40]
            .into_iter()
            .any(|shift| huge.set_len(1 << shift).is_ok()),
        "no sparse size was accepted"
    );
    drop(huge);
    let before = snapshot_names(&session);
    let plan = plan_session_move(&session, &world.archive).unwrap();
    let error = world
        .run(Step::other_volume(&plan.from, &plan.to))
        .unwrap_err();
    assert!(format!("{error:#}").contains("do not fit"), "{error:#}");
    assert!(!plan.to.exists());
    assert_eq!(
        fs::read_dir(&world.archive).unwrap().count(),
        0,
        "nothing is left on the other drive"
    );
    assert_eq!(snapshot_names(&session), before);
    fs::remove_file(session.join("Capture/HUGE.NEF")).unwrap();
    assert_opens_whole(&session);
}

/// The names below `dir` (a sparse file is not read).
fn snapshot_names(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in fs::read_dir(folder).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.ends_with("-wal") || name.ends_with("-shm") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                stack.push(path.clone());
            }
            names.push(path.strip_prefix(dir).unwrap().to_path_buf());
        }
    }
    names.sort();
    names
}
