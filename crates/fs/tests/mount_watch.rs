//! Mount-change notifications ([`rawtonic_fs::volume::watch_mounts`]).
use rawtonic_fs::volume;
use std::fs;

#[test]
fn the_mount_list_is_watched_by_system_notifications_where_there_are_any() {
    let key = volume::mounts_key();
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(!key.is_empty(), "the system disk is mounted");
    }
    let kind = volume::watch_mounts(|| {});
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert_eq!(kind, volume::WatchKind::Notifications);
    } else {
        assert_eq!(kind, volume::WatchKind::Polling);
    }
}

/// Mounts a small file system and unmounts it again, measuring how soon the
/// watcher reports each. Needs privileges (`hdiutil` on macOS, root for
/// `mount` on Linux): `cargo test -p rawtonic-fs --test mount_watch -- --ignored --nocapture`.
#[test]
#[ignore = "mounts a file system"]
fn mounting_and_ejecting_is_reported_at_once() {
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let (tx, rx) = mpsc::channel::<Instant>();
    volume::watch_mounts(move || {
        let _ = tx.send(Instant::now());
    });
    let tmp = tempfile::tempdir().unwrap();
    let point = tmp.path().join("point");
    fs::create_dir_all(&point).unwrap();
    let image = tmp.path().join("watch.dmg");
    let (mount, unmount): (Vec<String>, Vec<String>) = if cfg!(target_os = "macos") {
        let status = Command::new("hdiutil")
            .args([
                "create",
                "-size",
                "8m",
                "-fs",
                "APFS",
                "-volname",
                "WatchTest",
            ])
            .arg(&image)
            .status()
            .unwrap();
        assert!(status.success());
        (
            vec![
                "hdiutil".into(),
                "attach".into(),
                "-mountpoint".into(),
                point.display().to_string(),
                image.display().to_string(),
            ],
            vec![
                "hdiutil".into(),
                "detach".into(),
                point.display().to_string(),
            ],
        )
    } else {
        // A drive (a loop device), not a virtual file system: those are
        // left out of the mount list on purpose.
        fs::write(&image, vec![0u8; 16 << 20]).unwrap();
        let status = Command::new("mkfs.ext4")
            .arg("-q")
            .arg(&image)
            .status()
            .unwrap();
        assert!(status.success());
        (
            vec![
                "mount".into(),
                "-o".into(),
                "loop".into(),
                image.display().to_string(),
                point.display().to_string(),
            ],
            vec!["umount".into(), point.display().to_string()],
        )
    };
    for (what, command) in [("mount", &mount), ("unmount", &unmount)] {
        while rx.try_recv().is_ok() {}
        let started = Instant::now();
        let status = Command::new(&command[0])
            .args(&command[1..])
            .status()
            .unwrap();
        let returned = Instant::now();
        assert!(status.success(), "{what} failed");
        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no change seen");
        let after_start = seen.duration_since(started).as_secs_f64() * 1000.0;
        let command_ms = returned.duration_since(started).as_secs_f64() * 1000.0;
        println!(
            "{what}: command took {command_ms:.0} ms; reported {after_start:.0} ms after it started \
             ({:+.0} ms relative to its return)",
            after_start - command_ms
        );
    }
}
