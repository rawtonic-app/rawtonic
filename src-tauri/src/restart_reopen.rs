// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an app restart reopens when no library was open: the photo shown at
//! "Restart now" (the in-app update). A library reopens by itself
//! (`last-library.json`); a photo opened on its own (command line, "Open
//! with", Finder) had nothing to come back from, and the Windows setup's
//! relaunch (`/R`) starts Rawtonic without arguments. So the restart notes
//! the photo's path in the app's config folder, and the next start takes the
//! note once (read and removed) and opens the photo. Only the path is
//! written, never anything beside or inside the photo.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The note, in the app's config folder (beside `last-library.json`).
pub const FILE: &str = "reopen-after-restart.json";

/// Set by `app_quit::start_new_instance` on the instance a restart starts
/// (macOS, Linux): its arguments are the first launch's, so the note (what
/// was open at the restart) wins over a photo among them.
pub const RESTARTED_ENV: &str = "RAWTONIC_RESTARTED";

#[derive(Serialize, Deserialize)]
struct Note {
    photo: String,
}

/// Notes `photo` for the next start, or removes an earlier note when there
/// is nothing to reopen. Atomic: a start never reads half a note.
pub fn remember(config_dir: &Path, photo: Option<&str>) -> anyhow::Result<()> {
    let path = config_dir.join(FILE);
    match photo.filter(|photo| !photo.is_empty()) {
        Some(photo) => {
            std::fs::create_dir_all(config_dir)?;
            let note = serde_json::to_vec(&Note {
                photo: photo.to_owned(),
            })?;
            rawtonic_fs::atomic_write(&path, note)
        }
        None => forget(&path),
    }
}

fn forget(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Takes the note: removes it (a note is used by one start only) and returns
/// its photo when the photo's file is still there.
pub fn take(config_dir: &Path) -> Option<String> {
    let path = config_dir.join(FILE);
    let bytes = std::fs::read(&path).ok()?;
    if let Err(e) = forget(&path) {
        // Kept, it would reopen the photo at every start.
        log::warn!("Could not remove the restart note: {e}");
        return None;
    }
    let note: Note = serde_json::from_slice(&bytes).ok()?;
    let (source, _) = rawtonic_export::sidecars::parse_virtual_path(&note.photo);
    source.is_file().then_some(note.photo)
}

/// Whether this process was started by an app restart (`RESTARTED_ENV`).
pub fn restarted(var: impl Fn(&str) -> Option<OsString>) -> bool {
    var(RESTARTED_ENV).is_some_and(|value| value == "1")
}

/// The photo a start opens: one named at launch, or the restart's note. A
/// restart's own instance prefers the note (its arguments are stale); any
/// other start prefers what the photographer asked for now, and the note
/// serves only a start without one (the Windows setup's relaunch).
pub fn choose(launch: Option<String>, restarted: bool, noted: Option<String>) -> Option<String> {
    if restarted {
        noted.or(launch)
    } else {
        launch.or(noted)
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
    fn a_noted_photo_is_reopened_once() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let photo = photo_in(photos.path());
        remember(config.path(), Some(&photo)).unwrap();
        assert_eq!(take(config.path()), Some(photo.clone()));
        assert_eq!(take(config.path()), None, "a note serves one start");
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
        assert_eq!(take(config.path()), Some(copy));
    }

    #[test]
    fn nothing_open_removes_an_earlier_note() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        remember(config.path(), Some(&photo_in(photos.path()))).unwrap();
        remember(config.path(), None).unwrap();
        assert_eq!(take(config.path()), None);
        assert!(!config.path().join(FILE).exists());
        remember(config.path(), None).unwrap();
    }

    #[test]
    fn a_photo_gone_since_is_not_reopened_and_the_note_goes() {
        let config = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let photo = photo_in(photos.path());
        remember(config.path(), Some(&photo)).unwrap();
        std::fs::remove_file(&photo).unwrap();
        assert_eq!(take(config.path()), None);
        assert!(!config.path().join(FILE).exists());
    }

    #[test]
    fn a_damaged_note_is_dropped() {
        let config = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join(FILE), b"{not json").unwrap();
        assert_eq!(take(config.path()), None);
        assert!(!config.path().join(FILE).exists());
    }

    #[test]
    fn a_restart_prefers_the_note_other_starts_the_launch_photo() {
        let launch = || Some("first.ARW".to_owned());
        let noted = || Some("now.ARW".to_owned());
        // macOS/Linux restart: the arguments are the first launch's.
        assert_eq!(choose(launch(), true, noted()), noted());
        // Windows setup relaunch: no arguments.
        assert_eq!(choose(None, false, noted()), noted());
        // A photo opened by hand after a restart that never came back.
        assert_eq!(choose(launch(), false, noted()), launch());
        assert_eq!(choose(launch(), true, None), launch());
        assert_eq!(choose(None, true, None), None);
    }

    #[test]
    fn only_the_restart_flag_marks_a_restart() {
        let env = |value: Option<&str>| {
            move |name: &str| {
                (name == RESTARTED_ENV)
                    .then_some(value)
                    .flatten()
                    .map(OsString::from)
            }
        };
        assert!(restarted(env(Some("1"))));
        assert!(!restarted(env(Some("0"))));
        assert!(!restarted(env(None)));
    }
}
