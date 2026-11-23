// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an app restart reopens when no library was open: the photo shown at
//! "Restart now" (the in-app update). A library reopens by itself
//! (`last-library.json`); a photo opened on its own (command line, "Open
//! with", Finder) had nothing to come back from, and the Windows setup's
//! relaunch (`/R`) starts Rawtonic without arguments. So the restart notes
//! what to reopen in the app's config folder — the photo, or nothing (a
//! library was open, or no photo) — and the next start follows the note. It
//! is removed once a window has asked for its launch file, so a start that
//! failed before that leaves it for the next one. Only the path is written,
//! never anything beside or inside the photo.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The note, in the app's config folder (beside `last-library.json`).
pub const FILE: &str = "reopen-after-restart.json";

/// Set by `app_quit::start_new_instance` on the instance a restart starts
/// (macOS, Linux): its arguments are the first launch's, so the note (what
/// was open at the restart) decides instead of them.
pub const RESTARTED_ENV: &str = "RAWTONIC_RESTARTED";

#[derive(Serialize, Deserialize)]
struct Note {
    /// None: nothing to reopen beside what restores itself.
    photo: Option<String>,
}

/// Notes what the next start reopens: `photo`, or nothing. Atomic: a start
/// never reads half a note.
pub fn remember(config_dir: &Path, photo: Option<&str>) -> anyhow::Result<()> {
    let note = Note {
        photo: photo.filter(|photo| !photo.is_empty()).map(str::to_owned),
    };
    std::fs::create_dir_all(config_dir)?;
    rawtonic_fs::atomic_write(&config_dir.join(FILE), serde_json::to_vec(&note)?)
}

/// Removes the note (used once, or the restart did not happen).
pub fn forget(config_dir: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(config_dir.join(FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Reads the note without removing it: None without a (readable) note,
/// `Some(None)` when it names nothing or a photo whose file is gone.
pub fn read(config_dir: &Path) -> Option<Option<String>> {
    let bytes = std::fs::read(config_dir.join(FILE)).ok()?;
    let note: Note = serde_json::from_slice(&bytes).ok()?;
    Some(note.photo.filter(|photo| {
        let (source, _) = rawtonic_export::sidecars::parse_virtual_path(photo);
        source.is_file()
    }))
}

/// Whether this process was started by an app restart (`RESTARTED_ENV`).
pub fn restarted(var: impl Fn(&str) -> Option<OsString>) -> bool {
    var(RESTARTED_ENV).is_some_and(|value| value == "1")
}

/// The file a start opens: one named at launch, or the restart's note. A
/// restart's own instance follows its note, also when it names nothing (its
/// arguments are the first launch's: a library switched away from since
/// must not come back); any other start prefers what the photographer asked
/// for now, and the note serves only a start without one (the Windows
/// setup's relaunch).
pub fn choose(
    launch: Option<String>,
    restarted: bool,
    noted: Option<Option<String>>,
) -> Option<String> {
    match (restarted, noted) {
        (true, Some(noted)) => noted,
        (_, noted) => launch.or(noted.flatten()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo_in(dir: &Path) -> String {
        let photo = dir.join("photo one.ARW");
        std::fs::write(&photo, b"raw").unwrap();
        photo.to_string_lossy().into_owned()
    }

    #[test]
    fn a_noted_photo_is_read_until_forgotten() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let photo = photo_in(photos.path());
        remember(config.path(), Some(&photo)).unwrap();
        assert_eq!(read(config.path()), Some(Some(photo.clone())));
        // A start that failed before its window asked keeps it.
        assert_eq!(read(config.path()), Some(Some(photo.clone())));
        forget(config.path()).unwrap();
        assert_eq!(read(config.path()), None);
        forget(config.path()).unwrap();
        assert_eq!(
            std::fs::read(&photo).unwrap(),
            b"raw",
            "the photo is never touched"
        );
    }

    #[test]
    fn a_virtual_copy_is_reopened_by_its_address() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let photo = photo_in(photos.path());
        let copy = rawtonic_export::sidecars::virtual_path(Path::new(&photo), Some("ab12cd"));
        remember(config.path(), Some(&copy)).unwrap();
        assert_eq!(read(config.path()), Some(Some(copy)));
    }

    #[test]
    fn nothing_open_is_noted_as_nothing() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        remember(config.path(), Some(&photo_in(photos.path()))).unwrap();
        remember(config.path(), None).unwrap();
        assert_eq!(read(config.path()), Some(None));
        remember(config.path(), Some("")).unwrap();
        assert_eq!(read(config.path()), Some(None));
    }

    #[test]
    fn a_photo_gone_since_is_not_reopened() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let photo = photo_in(photos.path());
        remember(config.path(), Some(&photo)).unwrap();
        std::fs::remove_file(&photo).unwrap();
        assert_eq!(read(config.path()), Some(None));
    }

    #[test]
    fn a_damaged_note_counts_as_none() {
        let config = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join(FILE), b"{not json").unwrap();
        assert_eq!(read(config.path()), None);
    }

    #[test]
    fn a_restart_follows_its_note_other_starts_the_launch_file() {
        let launch = || Some("first.ARW".to_owned());
        let now = || Some(Some("now.ARW".to_owned()));
        // macOS/Linux restart: the arguments are the first launch's.
        assert_eq!(choose(launch(), true, now()), Some("now.ARW".to_owned()));
        // Restart with a library open (or nothing): the first launch's
        // library or photo must not come back (UPD3 review r1).
        assert_eq!(choose(launch(), true, Some(None)), None);
        // Windows setup relaunch: no arguments.
        assert_eq!(choose(None, false, now()), Some("now.ARW".to_owned()));
        // A file opened by hand after a restart that never came back.
        assert_eq!(choose(launch(), false, now()), launch());
        assert_eq!(choose(launch(), false, Some(None)), launch());
        // A restart without a note (it could not be written): as before.
        assert_eq!(choose(launch(), true, None), launch());
        assert_eq!(choose(None, true, None), None);
    }

    #[test]
    fn only_the_restart_flag_marks_a_restart() {
        fn env(value: Option<&'static str>) -> impl Fn(&str) -> Option<OsString> {
            move |name| {
                (name == RESTARTED_ENV)
                    .then_some(value)
                    .flatten()
                    .map(OsString::from)
            }
        }
        assert!(restarted(env(Some("1"))));
        assert!(!restarted(env(Some("0"))));
        assert!(!restarted(env(None)));
    }
}
