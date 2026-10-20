//! The helper's transfer folder in a session (`<session>/.tether`, never
//! through a symbolic link): a download is written to a temporary `….part`
//! file in its own new slot `incoming/<n>/`, synced, checked against the
//! size the camera reported and only then published under the camera's
//! name, no-clobber — a complete file only ever appears that way. One slot
//! per file keeps the camera's own name (the naming template uses it) even
//! when the camera restarts its numbering.
//!
//! The app ingests complete files into Capture and then removes the
//! transfer copy (`finish`). After a crash, `recover` hands back the
//! complete files to ingest and moves `.part` files to `incomplete/` —
//! never ingested, never deleted.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const INCOMING: &str = "incoming";
pub const INCOMPLETE: &str = "incomplete";
const PART: &str = "part";

/// The session's transfer folder.
pub fn root(session: &Path) -> PathBuf {
    session.join(".tether")
}

/// A file name from the camera that is safe as one path component; else
/// `fallback`.
fn safe_name(name: &str, fallback: &str) -> String {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.ends_with(&format!(".{PART}"))
        && !name.contains(['/', '\\', '\0', ':']);
    if ok { name.into() } else { fallback.into() }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// `dir` as a real folder (created when missing), never a symbolic link.
fn real_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::other(format!(
            "{} is a symbolic link; tethering will not write through it",
            dir.display()
        ))),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other(format!(
            "{} is not a folder",
            dir.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(dir),
        Err(e) => Err(e),
    }
}

/// `dir` exists as a real folder, not a symbolic link (recovery and clean-up
/// never read or delete through one).
fn existing_real_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a real folder; tethering will not read or delete through it",
            dir.display()
        )));
    }
    Ok(())
}

/// A new transfer folder for one file (`incoming/<n>`, n from `serial`
/// on): created exclusively, never one that exists.
fn new_slot(root: &Path, serial: u64) -> io::Result<PathBuf> {
    real_dir(root)?;
    let incoming = root.join(INCOMING);
    real_dir(&incoming)?;
    for n in serial.. {
        let slot = incoming.join(format!("{n:08}"));
        match fs::create_dir(&slot) {
            Ok(()) => return Ok(slot),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Where a download is written before it is complete: a temporary file
/// (`….part`) in its slot.
pub struct Partial {
    path: PathBuf,
    temp: tempfile::NamedTempFile,
}

impl Partial {
    pub fn create(root: &Path, serial: u64, name: &str) -> io::Result<Self> {
        let slot = new_slot(root, serial)?;
        let path = slot.join(safe_name(name, &format!("capture-{serial}")));
        let temp = tempfile::Builder::new()
            .prefix(".")
            .suffix(&format!(".{PART}"))
            .tempfile_in(&slot)?;
        Ok(Self { path, temp })
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.temp.write_all(bytes)
    }

    /// Test hook: leaves the `.part` file behind as a crash would.
    pub fn abandon(self) {
        let _ = self.temp.keep();
    }

    /// Syncs, checks the length and publishes the file under its name
    /// (no-clobber: it fails rather than replace anything).
    pub fn publish(self, expected: u64) -> io::Result<PathBuf> {
        self.temp.as_file().sync_all()?;
        let written = self.temp.as_file().metadata()?.len();
        if written != expected {
            return Err(io::Error::other(format!(
                "the download has {written} of {expected} bytes"
            )));
        }
        // The slot is this download's own (`new_slot`), so a volume without
        // an exclusive rename still gets an atomic one: a complete file only.
        rawtonic_metadata::no_clobber::persist_in_own_folder(self.temp, &self.path)?;
        sync_dir(self.path.parent().unwrap())?;
        Ok(self.path)
    }
}

/// What a crashed run left in the transfer folder.
#[derive(Debug, Default, PartialEq)]
pub struct Recovered {
    /// Complete downloads still to ingest, oldest first.
    pub complete: Vec<PathBuf>,
    /// Partial downloads moved to `incomplete/` (kept, never ingested).
    pub incomplete: Vec<PathBuf>,
}

pub fn recover(root: &Path) -> io::Result<Recovered> {
    let mut recovered = Recovered::default();
    let incoming = root.join(INCOMING);
    for dir in [root, incoming.as_path()] {
        match existing_real_dir(dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(recovered),
            other => other?,
        }
    }
    // Entries' own types: a slot or file that is a symbolic link is skipped.
    let mut slots: Vec<PathBuf> = match fs::read_dir(&incoming) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(recovered),
        Err(e) => return Err(e),
    };
    slots.sort();
    for slot in slots {
        let mut files: Vec<PathBuf> = fs::read_dir(&slot)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .collect();
        files.sort();
        for file in files {
            if file.extension().is_some_and(|e| e == PART) {
                let target = root
                    .join(INCOMPLETE)
                    .join(slot.file_name().unwrap())
                    .join(file.file_name().unwrap());
                real_dir(&root.join(INCOMPLETE))?;
                real_dir(target.parent().unwrap())?;
                if target.exists() {
                    continue;
                }
                fs::rename(&file, &target)?;
                recovered.incomplete.push(target);
            } else if !file
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                recovered.complete.push(file);
            }
        }
        // Only an empty slot goes.
        let _ = fs::remove_dir(&slot);
    }
    Ok(recovered)
}

/// The slot of a transfer file inside `root`'s incoming folder.
fn slot_of<'a>(root: &Path, path: &'a Path) -> io::Result<&'a Path> {
    let incoming = root.join(INCOMING);
    path.parent()
        .filter(|slot| slot.parent() == Some(incoming.as_path()))
        .ok_or_else(|| io::Error::other("not a tether transfer file"))
}

const RECORD: &str = ".ingest.json";

/// Keeps what the app decided for a transfer file before it ingests it
/// (sequence, template, next-capture contents), in its slot, written
/// atomically and no-clobber: a recovery ingests with exactly that. A
/// record that exists is kept (the first decision stands).
pub fn write_record(root: &Path, path: &Path, record: &serde_json::Value) -> io::Result<()> {
    let slot = slot_of(root, path)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".record")
        .tempfile_in(slot)?;
    temp.write_all(&serde_json::to_vec(record).map_err(io::Error::other)?)?;
    temp.as_file().sync_all()?;
    // The slot's only writer is the session's one ingest.
    match rawtonic_metadata::no_clobber::persist_in_own_folder(temp, &slot.join(RECORD)) {
        Ok(_) => sync_dir(slot),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// The record `write_record` kept for a transfer file, if any.
pub fn read_record(root: &Path, path: &Path) -> io::Result<Option<serde_json::Value>> {
    let slot = slot_of(root, path)?;
    match fs::read(slot.join(RECORD)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Removes a transfer copy (and its record) once its photo is in the
/// session; its slot goes with it when empty. Only files inside `root`'s
/// incoming folder.
pub fn finish(root: &Path, path: &Path) -> io::Result<()> {
    let slot = slot_of(root, path)?;
    for dir in [root, &root.join(INCOMING), slot] {
        existing_real_dir(dir)?;
    }
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::other("not a tether transfer file"));
    }
    fs::remove_file(path)?;
    match fs::remove_file(slot.join(RECORD)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let _ = fs::remove_dir(slot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_appears_only_complete_and_keeps_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        let mut a = Partial::create(&root, 1, "DSC00001.ARW").unwrap();
        a.write(b"abc").unwrap();
        let mut b = Partial::create(&root, 1, "DSC00001.ARW").unwrap();
        b.write(b"second").unwrap();
        let a = a.publish(3).unwrap();
        let b = b.publish(6).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap(), "DSC00001.ARW");
        assert_eq!(b.file_name().unwrap(), "DSC00001.ARW");
        assert_eq!(fs::read(&a).unwrap(), b"abc");
        assert_eq!(fs::read(&b).unwrap(), b"second");
    }

    #[test]
    fn existing_slots_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        fs::create_dir_all(root.join(INCOMING).join("00000001")).unwrap();
        fs::write(root.join(INCOMING).join("00000001").join("A.ARW"), b"old").unwrap();
        let mut a = Partial::create(&root, 1, "A.ARW").unwrap();
        a.write(b"new").unwrap();
        let a = a.publish(3).unwrap();
        assert_eq!(a, root.join(INCOMING).join("00000002").join("A.ARW"));
        assert_eq!(
            fs::read(root.join(INCOMING).join("00000001").join("A.ARW")).unwrap(),
            b"old"
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_and_clean_up_never_go_through_a_symbolic_link() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        let photo = elsewhere
            .join(INCOMING)
            .join("00000001")
            .join("ORIGINAL.ARW");
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, b"someone's original").unwrap();
        let session = dir.path().join("session");
        fs::create_dir(&session).unwrap();
        // The whole transfer folder is a link.
        std::os::unix::fs::symlink(&elsewhere, root(&session)).unwrap();
        assert!(recover(&root(&session)).is_err());
        let through = root(&session)
            .join(INCOMING)
            .join("00000001")
            .join("ORIGINAL.ARW");
        assert!(finish(&root(&session), &through).is_err());
        // Only its incoming folder is a link.
        fs::remove_file(root(&session)).unwrap();
        fs::create_dir(root(&session)).unwrap();
        std::os::unix::fs::symlink(elsewhere.join(INCOMING), root(&session).join(INCOMING))
            .unwrap();
        assert!(recover(&root(&session)).is_err());
        assert!(finish(&root(&session), &through).is_err());
        assert_eq!(fs::read(&photo).unwrap(), b"someone's original");
    }

    #[test]
    fn a_symbolic_link_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let session = dir.path().join("session");
        fs::create_dir(&session).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root(&session)).unwrap();
        assert!(Partial::create(&root(&session), 1, "A.ARW").is_err());
        fs::remove_file(root(&session)).unwrap();
        fs::create_dir(root(&session)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root(&session).join(INCOMING)).unwrap();
        assert!(Partial::create(&root(&session), 1, "A.ARW").is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_short_download_is_not_published() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        let mut a = Partial::create(&root, 1, "DSC00001.ARW").unwrap();
        a.write(b"ab").unwrap();
        assert!(a.publish(3).is_err());
        // The failed temporary file is gone; nothing to ingest.
        assert_eq!(recover(&root).unwrap(), Recovered::default());
    }

    #[test]
    fn recovery_ingests_complete_files_and_keeps_partial_ones_aside() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        let mut done = Partial::create(&root, 1, "A.ARW").unwrap();
        done.write(b"done").unwrap();
        let done = done.publish(4).unwrap();
        let mut partial = Partial::create(&root, 2, "B.ARW").unwrap();
        partial.write(b"hal").unwrap();
        partial.abandon();
        let recovered = recover(&root).unwrap();
        assert_eq!(recovered.complete, vec![done.clone()]);
        assert_eq!(recovered.incomplete.len(), 1);
        assert_eq!(fs::read(&recovered.incomplete[0]).unwrap(), b"hal");
        // Again: nothing moves twice, the complete file is still there.
        let again = recover(&root).unwrap();
        assert_eq!(again.complete, vec![done.clone()]);
        assert!(again.incomplete.is_empty());
        let record = serde_json::json!({"sequence": 7});
        write_record(&root, &done, &record).unwrap();
        write_record(&root, &done, &serde_json::json!({"sequence": 8})).unwrap();
        assert_eq!(read_record(&root, &done).unwrap(), Some(record));
        assert_eq!(recover(&root).unwrap().complete, vec![done.clone()]);
        finish(&root, &done).unwrap();
        assert!(!done.exists());
        assert!(!done.parent().unwrap().exists());
        assert_eq!(recover(&root).unwrap(), Recovered::default());
    }

    #[test]
    fn only_transfer_files_are_finished_and_odd_names_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        let other = dir.path().join("photo.ARW");
        fs::write(&other, b"x").unwrap();
        assert!(finish(&root, &other).is_err());
        assert!(other.exists());
        for hostile in ["", ".hidden", "a/b.ARW", "..", "x.part", "c:d"] {
            let mut p = Partial::create(&root, 9, hostile).unwrap();
            p.write(b"x").unwrap();
            let p = p.publish(1).unwrap();
            assert!(
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("capture-"),
                "{hostile}"
            );
            assert_eq!(p.parent().unwrap().parent().unwrap(), root.join(INCOMING));
        }
        let mut odd = Partial::create(&root, 5, "../x.ARW").unwrap();
        odd.write(b"x").unwrap();
        let odd = odd.publish(1).unwrap();
        assert_eq!(odd.file_name().unwrap(), "capture-5");
    }
}
