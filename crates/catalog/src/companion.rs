//! A RAW photograph's companion: the JPEG (or HEIF) the camera wrote with
//! it, imported as part of the RAW's asset rather than as a photograph of
//! its own (ingest's RAW+JPEG pairs). It lives beside the RAW; the asset
//! records its file name (`assets.raw_jpeg_pair`) with its size and content
//! fingerprint, so the library never registers it again (`add_files`,
//! Synchronize Folder) and the app does not list it, and a file of that
//! name that is not it (the RAW relinked next to another JPEG) is never
//! taken for it. Nothing here touches files.
use crate::{Asset, Library, fingerprint};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// `name` as a companion file name: one plain path component.
pub(crate) fn validate_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && path.file_name().and_then(|n| n.to_str()) == Some(name)
            && !name.contains(['/', '\\']),
        "Invalid companion file name {name:?}"
    );
    Ok(())
}

/// A companion's identity: its size and content fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub size: u64,
    pub fingerprint: String,
}

impl Identity {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("No companion file {}", path.display()))?;
        ensure!(meta.is_file(), "{} is not a file", path.display());
        Ok(Self {
            size: meta.len(),
            fingerprint: fingerprint(path)?,
        })
    }
}

/// A recorded companion, whether its file is still there or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionRecord {
    /// The photograph's asset id.
    pub owner: String,
    /// Where it is recorded: beside the photograph's file.
    pub path: PathBuf,
    size: Option<u64>,
    fingerprint: Option<String>,
}

impl CompanionRecord {
    /// Whether the file at `path` has the recorded size.
    pub fn sized(&self) -> bool {
        fs::symlink_metadata(&self.path).is_ok_and(|m| m.is_file() && Some(m.len()) == self.size)
    }

    /// Whether `file` has the recorded companion's size and content
    /// fingerprint (the same JPEG, wherever it is).
    pub fn matches(&self, file: &Path) -> bool {
        fs::metadata(file).is_ok_and(|m| m.is_file() && Some(m.len()) == self.size)
            && fingerprint(file).ok() == self.fingerprint
    }

    /// Whether the file at `path` is the recorded companion (size and
    /// content fingerprint).
    pub fn present(&self) -> bool {
        self.sized() && fingerprint(&self.path).ok() == self.fingerprint
    }
}

impl Library {
    /// Every recorded companion, also those whose file is gone or is no
    /// longer the one recorded (names a new copy must not take).
    pub fn companion_records(&self) -> Result<Vec<CompanionRecord>> {
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external,a.raw_jpeg_pair,a.companion_size,a.companion_fingerprint
             FROM assets a JOIN roots r ON r.id=a.root_id WHERE a.raw_jpeg_pair IS NOT NULL",
        )?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (owner, relative, root, external, name, size, fingerprint) = row?;
            let photo = self.join_root(root, external, relative);
            if let Some(folder) = photo.parent() {
                records.push(CompanionRecord {
                    owner,
                    path: folder.join(name),
                    size: size.map(|s| s as u64),
                    fingerprint,
                });
            }
        }
        Ok(records)
    }

    /// The companion file of `asset` (beside its file), if it has one and
    /// that file is the one recorded (size and content fingerprint).
    pub fn companion(&self, asset: &Asset) -> Result<Option<PathBuf>> {
        Ok(self
            .companion_record(asset)?
            .filter(CompanionRecord::present)
            .map(|record| record.path))
    }

    /// `asset`'s companion record, whether its file is there or not.
    pub fn companion_record(&self, asset: &Asset) -> Result<Option<CompanionRecord>> {
        Ok(self
            .companion_records()?
            .into_iter()
            .find(|record| record.owner == asset.id))
    }

    /// Every companion file that is there as recorded (size and content
    /// fingerprint), with the asset it belongs to.
    pub fn companions(&self) -> Result<HashMap<PathBuf, String>> {
        self.companions_in(None)
    }

    /// `companions` at or below `folder` (only their files are read).
    pub fn companions_in(&self, folder: Option<&Path>) -> Result<HashMap<PathBuf, String>> {
        Ok(self
            .companion_records()?
            .into_iter()
            .filter(|record| folder.is_none_or(|folder| record.path.starts_with(folder)))
            .filter(CompanionRecord::present)
            .map(|record| (record.path, record.owner))
            .collect())
    }

    /// The asset whose companion `path` is (its recorded file).
    pub fn companion_owner(&self, path: &Path) -> Result<Option<Asset>> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.companion_records()?
            .into_iter()
            .find(|record| record.path == path && record.present())
            .map(|record| self.asset(&record.owner))
            .transpose()
    }

    /// Records `name`, a file beside the photograph `id`, as its companion
    /// (with that file's size and content fingerprint), or clears it.
    pub fn set_companion(&self, id: &str, name: Option<&str>) -> Result<()> {
        let identity = match name {
            Some(name) => {
                validate_name(name)?;
                let photo = self.asset(id)?.path;
                Some(Identity::read(
                    &photo.parent().context("No parent folder")?.join(name),
                )?)
            }
            None => None,
        };
        let changed = self.conn.execute(
            "UPDATE assets SET raw_jpeg_pair=?,companion_size=?,companion_fingerprint=? WHERE id=?",
            params![
                name,
                identity.as_ref().map(|i| i.size as i64),
                identity.as_ref().map(|i| i.fingerprint.as_str()),
                id
            ],
        )?;
        ensure!(changed == 1, "No photograph {id}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Library, Mode, folder_sync::SyncOptions};
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn a_companion_is_part_of_its_photograph_and_never_registered_again() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        fs::write(capture.join("A.ARW"), b"raw").unwrap();
        fs::write(capture.join("A.JPG"), b"jpeg").unwrap();
        let planned = lib
            .plan_add(&capture.join("A.ARW"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        let (id, _) = lib.insert_planned(vec![planned]).unwrap().pop().unwrap();
        let asset = lib.asset(&id).unwrap();
        assert_eq!(lib.companion(&asset).unwrap(), Some(capture.join("A.JPG")));
        assert_eq!(
            lib.companion_owner(&capture.join("A.JPG"))
                .unwrap()
                .unwrap()
                .id,
            id
        );

        // Adding the folder again registers nothing: the JPEG is the RAW's.
        let report = lib
            .add_files(&[capture.join("A.ARW"), capture.join("A.JPG")])
            .unwrap();
        assert!(report.added.is_empty());
        assert_eq!(report.known.len(), 2);
        assert!(report.known.iter().all(|a| a.id == id));
        assert_eq!(lib.summary().unwrap().assets, 1);

        // Synchronize Folder does not take it for a new photograph.
        let never = AtomicBool::new(false);
        let plan = lib
            .scan_files_sync(
                &[capture.join("A.JPG")],
                &SyncOptions {
                    accept: &|_| true,
                    cancel: &never,
                    progress: &|_| {},
                },
            )
            .unwrap();
        assert!(plan.new_files.is_empty(), "{plan:?}");

        // Cleared, it is a file of its own again.
        lib.set_companion(&id, None).unwrap();
        assert_eq!(lib.companion(&asset).unwrap(), None);
        assert_eq!(
            lib.add_files(&[capture.join("A.JPG")]).unwrap().added.len(),
            1
        );
    }

    #[test]
    fn a_companion_name_is_one_file_beside_the_photograph() {
        for bad in ["", "../A.JPG", "sub/A.JPG", "a\\b.jpg", ".."] {
            assert!(super::validate_name(bad).is_err(), "{bad}");
        }
        super::validate_name("A.JPG").unwrap();
    }

    /// The RAW relinked (moved outside the app) next to another JPEG of
    /// the companion's name: that JPEG is not taken for the companion, not
    /// even with the same size.
    #[test]
    fn a_companion_is_its_recorded_file_not_any_file_of_its_name() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let x = lib.directory.join("Capture/X");
        let y = lib.directory.join("Capture/Y");
        fs::create_dir_all(&x).unwrap();
        fs::create_dir_all(&y).unwrap();
        fs::write(x.join("A.ARW"), b"raw").unwrap();
        fs::write(x.join("A.JPG"), b"jpeg one").unwrap();
        fs::write(y.join("A.JPG"), b"jpeg two").unwrap(); // same size, other content
        let planned = lib
            .plan_add(&x.join("A.ARW"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        let (id, _) = lib.insert_planned(vec![planned]).unwrap().pop().unwrap();
        fs::rename(x.join("A.ARW"), y.join("A.ARW")).unwrap();
        lib.relink(&id, &y.join("A.ARW"), false).unwrap();
        let asset = lib.asset(&id).unwrap();
        assert_eq!(lib.companion(&asset).unwrap(), None);
        assert!(lib.companions().unwrap().is_empty());
        assert!(lib.companion_owner(&y.join("A.JPG")).unwrap().is_none());
        // Recorded still: a new copy must not take that name.
        assert_eq!(lib.companion_records().unwrap()[0].path, y.join("A.JPG"));
        // The other JPEG is a photograph of its own.
        let report = lib.add_files(&[y.join("A.JPG")]).unwrap();
        assert_eq!(report.added.len(), 1);
        // Moved back beside it, it is the companion again.
        fs::rename(y.join("A.ARW"), x.join("A.ARW")).unwrap();
        lib.relink(&id, &x.join("A.ARW"), false).unwrap();
        assert_eq!(
            lib.companion(&lib.asset(&id).unwrap()).unwrap(),
            Some(x.join("A.JPG"))
        );
    }
}
