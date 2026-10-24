//! Copies made in place (`Publish::InPlace`, ADR 0016): for volumes that
//! cannot rename exclusively (exFAT on macOS) each file is created
//! exclusively under its final name and copied and verified there. These
//! tests force that mode on whatever volumes they run on.
use super::*;

/// Every volume publishes in place while it lives.
struct InPlace;
impl InPlace {
    fn on() -> Self {
        volume::FORCE_IN_PLACE.set(true);
        Self
    }
}
impl Drop for InPlace {
    fn drop(&mut self) {
        volume::FORCE_IN_PLACE.set(false);
    }
}

impl World {
    /// After a crash, before recovery: no file is lost; a file under a
    /// final name holds the start of its source's bytes (it is visible
    /// while it is copied) or all of them.
    fn assert_only_starts_of_copies(&self) {
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
                assert!(bytes.starts_with(found), "{name}: not the start of a copy");
            }
            if let Some(found) = &at_source {
                assert!(bytes.starts_with(found), "{name}: the original changed");
            }
            let whole = |f: &Option<Vec<u8>>| f.as_ref() == Some(&bytes);
            assert!(
                whole(&at_source) || whole(&at_target),
                "{name}: no whole copy outside the Trash (Trash holds it: {})",
                trashed.contains(&bytes)
            );
        }
    }
}

/// A crash at every point of a move, an undo and a redo, in place: before
/// recovery only starts of copies are visible; recovery removes the ones
/// it created (or names an empty one it cannot tell for its own) and ends
/// in one of the two states.
#[test]
fn a_crash_at_any_step_of_an_in_place_move_undo_or_redo_loses_nothing() {
    let _in_place = InPlace::on();
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Phase {
        Move,
        Undo,
        Redo,
    }
    for phase in [Phase::Move, Phase::Undo, Phase::Redo] {
        let mut points = 0;
        let mut empty_left = 0;
        for n in 0.. {
            let world = World::new();
            let id = (phase != Phase::Move).then(|| {
                let journal = world.journal();
                let op = journal
                    .execute("move", "Move", world.steps(), &world.trash)
                    .unwrap();
                if phase == Phase::Redo {
                    journal.undo(&op.id, &world.trash).unwrap();
                    journal.mark_synced(&op.id).unwrap();
                }
                op.id
            });
            crash::arm(n);
            let run = catch_unwind(AssertUnwindSafe(|| {
                let journal = world.journal();
                match (phase, &id) {
                    (Phase::Move, _) => journal
                        .execute("move", "Move", world.steps(), &world.trash)
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
            world.assert_only_starts_of_copies();
            let journal = world.journal();
            journal.recover(&world.trash).unwrap();
            let op = journal.history(1).unwrap().remove(0);
            // Toward the side the operation is on now; the other side may
            // keep an empty file whose creation was not journaled.
            let (side, other) = match op.state {
                State::RolledBack | State::Undone => (&world.source, &world.target),
                State::Done => (&world.target, &world.source),
                other => panic!("{phase:?} crash point {n}: {other:?}"),
            };
            let note = op.note.clone().unwrap_or_default();
            for (name, len) in FILES {
                assert_eq!(
                    fs::read(side.join(name)).unwrap(),
                    content(name, len),
                    "{name}"
                );
                match fs::read(other.join(name)) {
                    Err(_) => {}
                    // A kept original (after the copies were verified).
                    Ok(kept) if kept == content(name, len) => {}
                    Ok(empty) if empty.is_empty() => {
                        assert!(note.contains("may be an empty file"), "{note}");
                        empty_left += 1;
                        fs::remove_file(other.join(name)).unwrap();
                    }
                    Ok(_) => panic!("{phase:?} crash point {n}: a part of {name} left"),
                }
            }
            journal.recover(&world.trash).unwrap();
            assert_eq!(journal.operation(&op.id).unwrap().state, op.state);
        }
        eprintln!("{phase:?}: {points} crash points, {empty_left} empty files named");
        assert!(points >= 10, "{phase:?}: only {points} crash points");
        assert!(
            empty_left >= 1,
            "{phase:?}: the unjournaled creation was not reached"
        );
    }
}

#[test]
fn an_in_place_move_and_its_undo_and_redo_go_through() {
    let _in_place = InPlace::on();
    let world = World::new();
    let journal = world.journal();
    let op = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap();
    assert_eq!(op.note, None);
    world.assert_at(&world.target, &world.source);
    journal.undo(&op.id, &world.trash).unwrap();
    world.assert_at(&world.source, &world.target);
    journal.redo(&op.id, &world.trash).unwrap();
    world.assert_at(&world.target, &world.source);
}

/// Stop while a file is being copied in place: its start under the final
/// name is taken back, nothing else changes.
#[test]
fn stop_while_copying_in_place_takes_the_start_of_the_copy_back() {
    struct StopAfterFirstBlock(Cell<bool>);
    impl CopyObserver for StopAfterFirstBlock {
        fn copied(&self, _: u64, _: u64) {
            self.0.set(true);
        }
        fn cancelled(&self) -> bool {
            self.0.get()
        }
    }
    let _in_place = InPlace::on();
    let world = World::new();
    let error = world
        .journal()
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
}

/// An original changed while it is copied in place stops the move, and the
/// copy (holding only what this move wrote, no longer the start of the
/// changed original) is taken back: nothing is left under its final name.
#[test]
fn a_source_changed_while_copying_in_place_leaves_nothing_behind() {
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
    let _in_place = InPlace::on();
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
    assert!(format!("{error:#}").contains("A.RAF"), "{error:#}");
    assert!(World::names(&world.target).is_empty());
    assert_eq!(
        fs::read(&raw).unwrap()[1..],
        content("A.RAF", FILES[0].1)[1..]
    );
}

/// An original removed while it is copied in place: the move fails and
/// its complete copy, now the only one, stays under its final name.
#[test]
fn a_copy_in_place_whose_source_vanished_stays() {
    struct Remove<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Remove<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                fs::remove_file(self.0).unwrap();
            }
        }
    }
    let _in_place = InPlace::on();
    let world = World::new();
    let raw = world.source.join("A.RAF");
    let result = world.journal().execute_with(
        "move",
        "Move",
        world.steps(),
        &world.trash,
        &Remove(&raw, Cell::new(false)),
    );
    assert!(result.is_err());
    assert!(!exists(&raw));
    assert_eq!(
        fs::read(world.target.join("A.RAF")).unwrap(),
        content("A.RAF", FILES[0].1)
    );
}

/// An attribute another program gives a copy while it is made in place
/// (a Finder comment): the copy fails its check and is kept with it, both
/// when the failed copy is taken back at once and after a crash.
#[test]
fn an_attribute_given_to_a_copy_in_place_meanwhile_keeps_it() {
    use super::attributes::{attribute, tag, tags, target_keeps};
    struct Comment<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Comment<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                assert!(tag(self.0, "comment"));
            }
        }
    }
    for crash in [false, true] {
        let _in_place = InPlace::on();
        let world = World::new();
        if !target_keeps(&world) {
            eprintln!("SKIP: the target volume keeps no extended attributes");
            return;
        }
        let copy = world.target.join("A.RAF");
        let journal = world.journal();
        if crash {
            crash::arm_at("copy block written");
        }
        let run = catch_unwind(AssertUnwindSafe(|| {
            journal.execute_with(
                "move",
                "Move",
                world.steps(),
                &world.trash,
                &Comment(&copy, Cell::new(false)),
            )
        }));
        if crash {
            assert!(!crash::disarm() && run.is_err());
            journal.recover(&world.trash).unwrap();
        } else {
            assert!(run.unwrap().is_err());
        }
        assert_eq!(tags(&copy), attribute("comment"), "crash: {crash}");
        assert_eq!(World::names(&world.target), ["A.RAF"]);
        let op = journal.history(1).unwrap().remove(0);
        assert_eq!(op.state, State::RolledBack);
        assert!(op.note.unwrap().contains("both were kept"));
        for (name, len) in FILES {
            assert_eq!(
                fs::read(world.source.join(name)).unwrap(),
                content(name, len)
            );
        }
    }
}

/// Journaling a creation fails after the file was created: the empty file
/// is taken back and the move rolls back.
#[test]
fn a_creation_its_journal_missed_is_taken_back() {
    let _in_place = InPlace::on();
    let world = World::new();
    let journal = world.journal();
    crash::fail_at("in place journaled");
    let error = journal
        .execute("move", "Move", world.steps(), &world.trash)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("simulated failure"),
        "{error:#}"
    );
    assert!(World::names(&world.target).is_empty());
    assert_eq!(
        journal.history(1).unwrap().remove(0).state,
        State::RolledBack
    );
    world.assert_at(&world.source, &world.target);
}

/// A name taken before its creation is never touched; what was created
/// is taken back.
#[test]
fn an_in_place_name_taken_meanwhile_is_never_replaced() {
    struct Squatter<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Squatter<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                fs::write(self.0, b"someone else's").unwrap();
            }
        }
    }
    let _in_place = InPlace::on();
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

/// Something else writing into a file being copied in place: the copy
/// fails its check and the file, no longer the start of the original, is
/// left with the original (both kept), and the note says so.
#[test]
fn an_in_place_copy_written_by_someone_else_is_left_with_its_original() {
    struct Scribbler<'a>(&'a Path, Cell<bool>);
    impl CopyObserver for Scribbler<'_> {
        fn copied(&self, _: u64, _: u64) {
            if !self.1.replace(true) {
                use std::io::Write;
                let mut file = fs::OpenOptions::new().write(true).open(self.0).unwrap();
                file.write_all(b"XXXX").unwrap();
            }
        }
    }
    let _in_place = InPlace::on();
    let world = World::new();
    let journal = world.journal();
    let raw = world.target.join("A.RAF");
    let error = journal
        .execute_with(
            "move",
            "Move",
            world.steps(),
            &world.trash,
            &Scribbler(&raw, Cell::new(false)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("differs"), "{error:#}");
    let op = journal.history(1).unwrap().remove(0);
    assert_eq!(op.state, State::RolledBack);
    assert!(op.note.unwrap().contains("both were kept"));
    assert!(fs::read(&raw).unwrap().starts_with(b"XXXX"));
    assert_eq!(World::names(&world.target), ["A.RAF"]);
    for (name, len) in FILES {
        assert_eq!(
            fs::read(world.source.join(name)).unwrap(),
            content(name, len)
        );
    }
}

/// A folder cannot be published in place as a whole: refused before
/// anything is copied, and the Library's probe says so too.
#[test]
fn a_folder_is_refused_where_copies_are_made_in_place() {
    let _in_place = InPlace::on();
    let world = World::new();
    let (from, to) = world.folder();
    let error = world
        .journal()
        .execute(
            "move",
            "Move folder",
            vec![world.folder_step()],
            &world.trash,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("as a whole"), "{error:#}");
    assert!(!exists(&to));
    assert!(World::names(&world.target).is_empty());
    assert!(exists(&from.join("Sub/B.NEF")));
    let refused = probe_folder(&world.target, &[from]).unwrap_err();
    assert!(format!("{refused:#}").contains("as a whole"), "{refused:#}");
    probe_folder(&world.target, &[world.source.join("A.RAF")]).unwrap();
}

/// Exclusive creation under contention, on a volume the environment names
/// (`RAWTONIC_TEST_EXCLUSIVE_CREATE_DIR`, e.g. an exFAT image on macOS):
/// threads creating the same 400 names never both win one, and on a
/// volume that ignores case a name differing only in case counts as taken.
#[test]
fn exclusive_creation_has_one_winner_per_name() {
    let Some(dir) = std::env::var_os("RAWTONIC_TEST_EXCLUSIVE_CREATE_DIR") else {
        eprintln!("SKIP: RAWTONIC_TEST_EXCLUSIVE_CREATE_DIR not set");
        return;
    };
    let dir = tempfile::tempdir_in(dir).unwrap();
    let wins: Vec<Vec<usize>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dir = dir.path();
                scope.spawn(move || {
                    (0..400)
                        .filter(|i| fs::File::create_new(dir.join(format!("{i}.RAF"))).is_ok())
                        .collect()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut count = vec![0; 400];
    for won in &wins {
        for &i in won {
            count[i] += 1;
        }
    }
    assert!(count.iter().all(|&c| c == 1), "{count:?}");
    fs::write(dir.path().join("Case.RAF"), b"x").unwrap();
    if exists(&dir.path().join("CASE.RAF")) {
        assert_eq!(
            fs::File::create_new(dir.path().join("CASE.RAF"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
