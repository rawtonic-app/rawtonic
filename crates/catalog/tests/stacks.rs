//! Stacks (schema 22): Group into Stack, Unstack, Remove from Stack, Move to
//! Top, Collapse/Expand, each one step undone and redone through
//! `restore_stacks`; members leaving the library; Auto-Stack by capture
//! time, brackets and bursts; and an older library taking stacks.

use rawtonic_catalog::stacks::{AutoKind, AutoStackOptions, Member, StackChange};
use rawtonic_catalog::{CaptureFacts, Library, Mode};
use serde_json::json;
use std::fs;
use std::path::Path;

fn session(dir: &Path, names: &[&str]) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let ids = names
        .iter()
        .map(|name| {
            let path = lib.directory.join("Capture").join(name);
            fs::write(&path, format!("raw {name}")).unwrap();
            lib.add_asset(&path).unwrap().id
        })
        .collect();
    (lib, ids)
}

fn photos(ids: &[String], which: &[usize]) -> Vec<Member> {
    which
        .iter()
        .map(|&i| Member::photo(ids[i].clone()))
        .collect()
}

/// The stack of photo `i` as indices into `ids` (top first) and its
/// collapsed flag.
fn stack(lib: &Library, ids: &[String], i: usize) -> Option<(Vec<usize>, bool)> {
    lib.stack_of(&Member::photo(ids[i].clone()))
        .unwrap()
        .map(|s| {
            (
                s.members
                    .iter()
                    .map(|m| ids.iter().position(|id| *id == m.asset_id).unwrap())
                    .collect(),
                s.collapsed,
            )
        })
}

/// Undo then redo `change`, checking the state before and after each.
fn undo_redo(lib: &Library, change: &StackChange, check_before: impl Fn(), check_after: impl Fn()) {
    lib.restore_stacks(change, true).unwrap();
    check_before();
    lib.restore_stacks(change, false).unwrap();
    check_after();
}

#[test]
fn group_unstack_remove_top_and_collapse_are_one_step_each() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "D.ARW"]);

    // Group: the first given (the active photo) is the top; collapsed.
    let grouped = lib.group_stack(&photos(&ids, &[1, 0, 2])).unwrap();
    assert_eq!(stack(&lib, &ids, 0), Some((vec![1, 0, 2], true)));
    assert!(grouped.before.is_empty());
    undo_redo(
        &lib,
        &grouped,
        || assert_eq!(stack(&lib, &ids, 0), None),
        || assert_eq!(stack(&lib, &ids, 2), Some((vec![1, 0, 2], true))),
    );
    // One photograph alone is no stack.
    assert!(lib.group_stack(&photos(&ids, &[3])).unwrap().is_empty());

    // Expand, toggle.
    let expanded = lib
        .set_stacks_collapsed(&photos(&ids, &[0]), Some(false))
        .unwrap();
    assert_eq!(stack(&lib, &ids, 0).unwrap().1, false);
    undo_redo(
        &lib,
        &expanded,
        || assert!(stack(&lib, &ids, 0).unwrap().1),
        || assert!(!stack(&lib, &ids, 0).unwrap().1),
    );
    lib.set_stacks_collapsed(&photos(&ids, &[2]), None).unwrap();
    assert!(stack(&lib, &ids, 0).unwrap().1);

    // Move to Top.
    let top = lib
        .move_to_stack_top(&Member::photo(ids[2].clone()))
        .unwrap();
    assert_eq!(stack(&lib, &ids, 0).unwrap().0, [2, 1, 0]);
    undo_redo(
        &lib,
        &top,
        || assert_eq!(stack(&lib, &ids, 0).unwrap().0, [1, 0, 2]),
        || assert_eq!(stack(&lib, &ids, 0).unwrap().0, [2, 1, 0]),
    );

    // Remove the top: the next one is the top.
    let removed = lib.remove_from_stack(&photos(&ids, &[2])).unwrap();
    assert_eq!(stack(&lib, &ids, 0).unwrap().0, [1, 0]);
    assert_eq!(stack(&lib, &ids, 2), None);
    undo_redo(
        &lib,
        &removed,
        || assert_eq!(stack(&lib, &ids, 0).unwrap().0, [2, 1, 0]),
        || assert_eq!(stack(&lib, &ids, 0).unwrap().0, [1, 0]),
    );
    // Removing one of the last two dissolves the stack.
    let dissolved = lib.remove_from_stack(&photos(&ids, &[0])).unwrap();
    assert_eq!(stack(&lib, &ids, 1), None);
    assert!(dissolved.after.is_empty());
    lib.restore_stacks(&dissolved, true).unwrap();
    assert_eq!(stack(&lib, &ids, 1).unwrap().0, [1, 0]);

    // Unstack.
    let unstacked = lib.unstack(&photos(&ids, &[0])).unwrap();
    assert_eq!(stack(&lib, &ids, 1), None);
    undo_redo(
        &lib,
        &unstacked,
        || assert_eq!(stack(&lib, &ids, 0).unwrap().0, [1, 0]),
        || assert_eq!(stack(&lib, &ids, 0), None),
    );
    // The change travels through the webview as JSON.
    let text = serde_json::to_string(&unstacked).unwrap();
    let back: StackChange = serde_json::from_str(&text).unwrap();
    assert_eq!(back, unstacked);
}

#[test]
fn grouping_members_of_stacks_merges_them_and_a_photo_is_in_one_stack() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "D.ARW", "E.ARW"]);
    lib.group_stack(&photos(&ids, &[0, 1])).unwrap();
    lib.group_stack(&photos(&ids, &[2, 3])).unwrap();
    let merged = lib.group_stack(&photos(&ids, &[4, 3])).unwrap();
    // E on top, then D's stack in its order; A's stack untouched.
    assert_eq!(stack(&lib, &ids, 4).unwrap().0, [4, 2, 3]);
    assert_eq!(stack(&lib, &ids, 0).unwrap().0, [0, 1]);
    assert_eq!(merged.before.len(), 1);
    lib.restore_stacks(&merged, true).unwrap();
    assert_eq!(stack(&lib, &ids, 2).unwrap().0, [2, 3]);
    assert_eq!(stack(&lib, &ids, 4), None);
    assert_eq!(lib.listed_stacks().unwrap().len(), 2);
}

#[test]
fn virtual_copies_stack_and_leave_when_forgotten() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW"]);
    lib.save_copy_state(&ids[0], "a1b2c3", &json!({"exposure": 1.0}))
        .unwrap();
    let copy = Member {
        asset_id: ids[0].clone(),
        copy: "a1b2c3".into(),
    };
    let members = vec![
        Member::photo(ids[0].clone()),
        copy.clone(),
        Member::photo(ids[1].clone()),
    ];
    lib.group_stack(&members).unwrap();
    let listed = lib.listed_stacks().unwrap();
    assert_eq!(listed[0].members.len(), 3);
    assert_eq!(listed[0].members[1].copy, "a1b2c3");
    assert!(listed[0].members[1].path.ends_with("Capture/A.ARW"));
    // An unknown copy is not stacked.
    let ghost = Member {
        asset_id: ids[1].clone(),
        copy: "ffffff".into(),
    };
    lib.group_stack(&[Member::photo(ids[1].clone()), ghost])
        .unwrap();
    assert_eq!(lib.stack_of(&copy).unwrap().unwrap().members.len(), 3);
    assert!(lib.remove_copy(&ids[0], "a1b2c3").unwrap());
    assert_eq!(
        lib.stack_of(&Member::photo(ids[0].clone()))
            .unwrap()
            .unwrap()
            .members
            .len(),
        2
    );
}

#[test]
fn a_trashed_top_stays_a_member_for_undo_and_a_forgotten_one_promotes_the_next() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW"]);
    lib.group_stack(&photos(&ids, &[0, 1, 2])).unwrap();
    // Move to Trash keeps the row (Undo puts it back where it was); the
    // listing leaves it out, so the next member shows as the top.
    let a = lib.asset(&ids[0]).unwrap().path;
    lib.set_trashed_at(&a, true).unwrap();
    assert_eq!(stack(&lib, &ids, 1).unwrap().0, [0, 1, 2]);
    // Emptied from the Trash: forgotten, the next is the top.
    assert_eq!(lib.forget_at(&a).unwrap(), 1);
    let left = stack(&lib, &ids[1..], 0).unwrap().0;
    assert_eq!(left, [0, 1]);
    // Forgetting one more dissolves it.
    lib.forget_at(&lib.asset(&ids[1]).unwrap().path).unwrap();
    assert!(lib.listed_stacks().unwrap().is_empty());
}

fn facts(time: &str, mode: Option<u8>, bias: Option<f64>, sequence: Option<u32>) -> CaptureFacts {
    CaptureFacts {
        captured: Some(time.into()),
        make: Some("Maker".into()),
        model: Some("Body".into()),
        exposure: Some(0.004),
        exposure_mode: mode,
        exposure_bias: bias,
        sequence,
        ..CaptureFacts::default()
    }
}

#[test]
fn auto_stack_groups_brackets_bursts_and_time_and_never_restacks() {
    let d = tempfile::tempdir().unwrap();
    let names = [
        "01.ARW", "02.ARW", "03.ARW", "04.ARW", "05.ARW", "06.ARW", "07.ARW", "08.ARW", "09.ARW",
    ];
    let (lib, ids) = session(d.path(), &names);
    let rows = [
        // A three-frame bracket, 0 / −2 / +2 EV.
        facts("2026-10-30 10:00:00", Some(2), Some(0.0), None),
        facts("2026-10-30 10:00:00", Some(2), Some(-2.0), None),
        facts("2026-10-30 10:00:01", Some(2), Some(2.0), None),
        // A burst, the maker's frames 1–3.
        facts("2026-10-30 10:00:02", Some(0), Some(0.0), Some(1)),
        facts("2026-10-30 10:00:02", Some(0), Some(0.0), Some(2)),
        facts("2026-10-30 10:00:03", Some(0), Some(0.0), Some(3)),
        // Two single frames 20 s apart, then one an hour later.
        facts("2026-10-30 10:05:00", Some(0), None, None),
        facts("2026-10-30 10:05:20", Some(0), None, None),
        facts("2026-10-30 11:05:20", Some(0), None, None),
    ];
    for (id, f) in ids.iter().zip(&rows) {
        assert!(lib.record_facts(id, f).unwrap());
    }
    let (read, _) = lib.facts(&ids[1]).unwrap().unwrap();
    assert_eq!(read.exposure_mode, Some(2));
    assert_eq!(read.exposure_bias, Some(-2.0));
    let options = AutoStackOptions {
        interval_secs: 30,
        use_camera_data: true,
    };
    let plan = lib.auto_stack_plan(&ids, options).unwrap();
    let kinds: Vec<_> = plan
        .groups
        .iter()
        .map(|g| (g.kind, g.assets.len()))
        .collect();
    assert_eq!(
        kinds,
        [
            (AutoKind::Bracket, 3),
            (AutoKind::Burst, 3),
            (AutoKind::Time, 2)
        ]
    );
    assert_eq!((plan.photos, plan.considered), (8, 9));
    // Without camera data, the first six are one stack by time.
    let by_time = lib
        .auto_stack_plan(
            &ids,
            AutoStackOptions {
                use_camera_data: false,
                ..options
            },
        )
        .unwrap();
    assert_eq!(
        by_time
            .groups
            .iter()
            .map(|g| g.assets.len())
            .collect::<Vec<_>>(),
        [6, 2]
    );
    // An hour's interval takes all nine.
    let hour = lib
        .auto_stack_plan(
            &ids,
            AutoStackOptions {
                interval_secs: 3600,
                use_camera_data: false,
            },
        )
        .unwrap();
    assert_eq!(hour.groups.len(), 1);

    // Photos stacked already are left out; the run is one undo step.
    lib.group_stack(&photos(&ids, &[6, 7])).unwrap();
    let plan = lib.auto_stack_plan(&ids, options).unwrap();
    assert_eq!(plan.groups.len(), 2);
    let change = lib.auto_stack(&plan.groups).unwrap();
    assert_eq!(change.after.len(), 2);
    assert_eq!(lib.listed_stacks().unwrap().len(), 3);
    assert_eq!(stack(&lib, &ids, 1).unwrap().0, [0, 1, 2]);
    lib.restore_stacks(&change, true).unwrap();
    assert_eq!(lib.listed_stacks().unwrap().len(), 1);
    lib.restore_stacks(&change, false).unwrap();
    assert_eq!(lib.listed_stacks().unwrap().len(), 3);
    assert!(
        lib.auto_stack_plan(&ids, options)
            .unwrap()
            .groups
            .is_empty()
    );
}

#[test]
fn a_library_of_schema_21_takes_stacks_and_reads_facts_again() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW"]);
    lib.record_facts(&ids[0], &facts("2026-10-30 10:00:00", None, None, None))
        .unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0022.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    // The bracket and burst facts of dated photographs are read again by
    // the background pass.
    let (_, read) = lib.facts(&ids[0]).unwrap().unwrap();
    assert!(!read);
    lib.group_stack(&photos(&ids, &[0, 1])).unwrap();
    assert_eq!(lib.listed_stacks().unwrap()[0].members.len(), 2);
    // Removing a photograph removes it from its stack (and the stack).
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    db.execute("DELETE FROM assets WHERE id=?", [&ids[0]])
        .unwrap();
    let left: i64 = db
        .query_row("SELECT count(*) FROM stacks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// Auto-Stack on 10,000 catalog rows: bursts of 5 frames 0–1 s apart every
/// 30 s, every tenth burst a bracket. Prints plan and apply times
/// (`cargo test -p rawtonic-catalog --release --test stacks -- --ignored
/// --nocapture`).
#[test]
#[ignore = "measurement"]
fn auto_stack_on_ten_thousand_rows() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let paths: Vec<_> = (0..10_000)
        .map(|i| {
            let path = lib.directory.join("Capture").join(format!("{i:05}.ARW"));
            fs::write(&path, format!("raw {i}")).unwrap();
            path
        })
        .collect();
    lib.add_files(&paths).unwrap();
    let ids: Vec<String> = paths
        .iter()
        .map(|p| lib.asset_at(p).unwrap().unwrap().id)
        .collect();
    let start = chrono::NaiveDate::from_ymd_opt(2026, 10, 30)
        .unwrap()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    for (i, id) in ids.iter().enumerate() {
        let (burst, frame) = (i / 5, i % 5);
        let time = start + chrono::Duration::seconds((burst * 30 + frame / 3) as i64);
        let bracket = burst % 10 == 0;
        let f = facts(
            &time.format("%Y-%m-%d %H:%M:%S").to_string(),
            Some(if bracket { 2 } else { 0 }),
            Some(if bracket {
                [0.0, -1.0, 1.0, -2.0, 2.0][frame]
            } else {
                0.0
            }),
            (!bracket).then_some(frame as u32 + 1),
        );
        lib.record_facts(id, &f).unwrap();
    }
    let options = AutoStackOptions {
        interval_secs: 5,
        use_camera_data: true,
    };
    let mut plans = Vec::new();
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let plan = lib.auto_stack_plan(&ids, options).unwrap();
        plans.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(plan.groups.len(), 2000);
        assert_eq!(plan.photos, 10_000);
    }
    let plan = lib.auto_stack_plan(&ids, options).unwrap();
    let t = std::time::Instant::now();
    let change = lib.auto_stack(&plan.groups).unwrap();
    let apply = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(change.after.len(), 2000);
    let t = std::time::Instant::now();
    let listed = lib.listed_stacks().unwrap();
    let list = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(listed.len(), 2000);
    let t = std::time::Instant::now();
    lib.restore_stacks(&change, true).unwrap();
    let undo = t.elapsed().as_secs_f64() * 1000.0;
    assert!(lib.listed_stacks().unwrap().is_empty());
    plans.sort_by(f64::total_cmp);
    println!(
        "auto-stack 10000 rows: plan median {:.1} ms (min {:.1}, max {:.1}); apply 2000 stacks {:.1} ms; list {:.1} ms; undo {:.1} ms",
        plans[2], plans[0], plans[4], apply, list, undo
    );
}
