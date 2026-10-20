//! A RAW photograph's companion: the JPEG (or HEIF) the camera wrote with
//! it, imported as part of the RAW's asset rather than as a photograph of
//! its own (ingest's RAW+JPEG pairs). It lives beside the RAW; the asset
//! records its file name (`assets.raw_jpeg_pair`), so the library never
//! registers it again (`add_files`, Synchronize Folder) and the app does
//! not list it. Nothing here touches files.
use crate::{Asset, Library};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::{
    collections::HashMap,
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

impl Library {
    /// The companion file of `asset` (beside its file), if it has one.
    pub fn companion(&self, asset: &Asset) -> Result<Option<PathBuf>> {
        let name: Option<String> = self.conn.query_row(
            "SELECT raw_jpeg_pair FROM assets WHERE id=?",
            [&asset.id],
            |r| r.get(0),
        )?;
        name.map(|name| Ok(asset.path.parent().context("No parent folder")?.join(name)))
            .transpose()
    }

    /// Every recorded companion file, with the asset it belongs to.
    pub fn companions(&self) -> Result<HashMap<PathBuf, String>> {
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external,a.raw_jpeg_pair
             FROM assets a JOIN roots r ON r.id=a.root_id WHERE a.raw_jpeg_pair IS NOT NULL",
        )?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut companions = HashMap::new();
        for row in rows {
            let (id, relative, root, external, name) = row?;
            let photo = self.join_root(root, external, relative);
            if let Some(folder) = photo.parent() {
                companions.insert(folder.join(name), id);
            }
        }
        Ok(companions)
    }

    /// The asset whose companion `path` is.
    pub fn companion_owner(&self, path: &Path) -> Result<Option<Asset>> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.companions()?
            .remove(&path)
            .map(|id| self.asset(&id))
            .transpose()
    }

    /// Records `name` (a file beside the photograph) as `id`'s companion,
    /// or clears it.
    pub fn set_companion(&self, id: &str, name: Option<&str>) -> Result<()> {
        if let Some(name) = name {
            validate_name(name)?;
        }
        let changed = self.conn.execute(
            "UPDATE assets SET raw_jpeg_pair=? WHERE id=?",
            params![name, id],
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
}
