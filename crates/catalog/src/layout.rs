//! What makes a folder a Rawtonic library (a session or a catalog): the one
//! definition every crate asks instead of spelling the file names itself.
//!
//! A folder is a library when it holds a library manifest (`session.json`,
//! else `catalog.json`). The manifest alone decides, so a half-created or
//! damaged library — its database missing — is still recognised as one: the
//! user is told it is damaged ([`LibraryState::Damaged`], and
//! [`crate::Library::open`] refuses to replace the database), and its photos'
//! library resources (camera profiles, previews) keep resolving inside it,
//! instead of the folder silently turning into a plain folder.
use std::path::{Path, PathBuf};

use crate::Mode;

pub const SESSION_MANIFEST: &str = "session.json";
pub const CATALOG_MANIFEST: &str = "catalog.json";
pub const SESSION_DATABASE: &str = "session.sqlite";
pub const CATALOG_DATABASE: &str = "catalog.sqlite";
/// SQLite's companion files of a database in WAL mode.
const DATABASE_COMPANIONS: [&str; 2] = ["-wal", "-shm"];

/// The manifest file of a library of `mode`.
pub fn manifest_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Session => SESSION_MANIFEST,
        Mode::Catalog => CATALOG_MANIFEST,
    }
}

/// The database file of a library of `mode`.
pub fn database_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Session => SESSION_DATABASE,
        Mode::Catalog => CATALOG_DATABASE,
    }
}

/// Whether `name` is one of the files a library keeps its own state in
/// (manifest, database and the database's WAL companions) of either kind.
pub fn is_library_file(name: &str) -> bool {
    [Mode::Session, Mode::Catalog].into_iter().any(|mode| {
        name == manifest_name(mode)
            || name == database_name(mode)
            || DATABASE_COMPANIONS
                .iter()
                .any(|suffix| name.strip_prefix(database_name(mode)) == Some(suffix))
    })
}

/// Whether a library's files are all there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryState {
    Ready,
    /// The manifest is there but the database is not (a half-created or
    /// damaged library); it is never replaced by an empty one.
    Damaged,
}

/// A library folder and its kind, as its manifest names it on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLayout {
    directory: PathBuf,
    mode: Mode,
}

impl LibraryLayout {
    /// The library in `directory` itself, when it holds a manifest (a
    /// session's before a catalog's).
    pub fn at(directory: &Path) -> Option<Self> {
        [Mode::Session, Mode::Catalog]
            .into_iter()
            .find(|&mode| directory.join(manifest_name(mode)).is_file())
            .map(|mode| Self {
                directory: directory.to_path_buf(),
                mode,
            })
    }

    /// The library whose folder holds `path` (a folder above it, never
    /// `path` itself).
    pub fn holding(path: &Path) -> Option<Self> {
        path.parent()?.ancestors().find_map(Self::at)
    }

    /// The library in `path` itself or in a folder above it.
    pub fn at_or_above(path: &Path) -> Option<Self> {
        path.ancestors().find_map(Self::at)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn into_directory(self) -> PathBuf {
        self.directory
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn manifest(&self) -> PathBuf {
        self.directory.join(manifest_name(self.mode))
    }

    pub fn database(&self) -> PathBuf {
        self.directory.join(database_name(self.mode))
    }

    /// The database and its WAL companions, which belong together.
    pub fn database_files(&self) -> [PathBuf; 3] {
        let name = database_name(self.mode);
        [
            self.directory.join(name),
            self.directory
                .join(format!("{name}{}", DATABASE_COMPANIONS[0])),
            self.directory
                .join(format!("{name}{}", DATABASE_COMPANIONS[1])),
        ]
    }

    pub fn state(&self) -> LibraryState {
        if self.database().is_file() {
            LibraryState::Ready
        } else {
            LibraryState::Damaged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_half_created_library_is_a_damaged_library_not_a_plain_folder() {
        let dir = tempfile::tempdir().unwrap();
        let job = dir.path().join("Job");
        fs::create_dir_all(job.join("Capture/Day 1")).unwrap();
        let photo = job.join("Capture/Day 1/A.NEF");
        assert_eq!(LibraryLayout::holding(&photo), None);

        fs::write(job.join(SESSION_MANIFEST), "{}").unwrap();
        let layout = LibraryLayout::holding(&photo).unwrap();
        assert_eq!(layout.directory(), job);
        assert_eq!(layout.mode(), Mode::Session);
        assert_eq!(layout.state(), LibraryState::Damaged);

        fs::write(job.join(SESSION_DATABASE), "").unwrap();
        assert_eq!(layout.state(), LibraryState::Ready);
        assert_eq!(layout.database_files()[1], job.join("session.sqlite-wal"));
    }

    #[test]
    fn a_library_holds_what_is_below_it_and_is_found_at_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("Archive");
        fs::create_dir_all(catalog.join("Photos")).unwrap();
        fs::write(catalog.join(CATALOG_MANIFEST), "{}").unwrap();
        assert_eq!(LibraryLayout::holding(&catalog), None);
        assert_eq!(
            LibraryLayout::at_or_above(&catalog).map(LibraryLayout::into_directory),
            Some(catalog.clone())
        );
        let layout = LibraryLayout::holding(&catalog.join("Photos/a.jpg")).unwrap();
        assert_eq!(layout.mode(), Mode::Catalog);
        assert_eq!(layout.database(), catalog.join(CATALOG_DATABASE));
        // A session's manifest wins over a catalog's, as `Library::open` reads it.
        fs::write(catalog.join(SESSION_MANIFEST), "{}").unwrap();
        assert_eq!(LibraryLayout::at(&catalog).unwrap().mode(), Mode::Session);
    }

    #[test]
    fn library_files_are_the_manifests_databases_and_their_wal_files() {
        for name in [
            "session.json",
            "catalog.json",
            "session.sqlite",
            "catalog.sqlite-wal",
            "session.sqlite-shm",
        ] {
            assert!(is_library_file(name), "{name}");
        }
        for name in ["session.sqlite-journal", "Capture", "a.json", "session"] {
            assert!(!is_library_file(name), "{name}");
        }
    }
}
