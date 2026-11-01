//! Export as Catalog (G39): a new catalog database holding only some
//! photographs of a session or catalog, with everything that is theirs —
//! versions and recipe history, virtual copies, snapshots, metadata,
//! keywords, capture-time corrections, eye calls, image hashes, masks — and
//! the albums that hold them. The source database is only read (a
//! `VACUUM INTO` snapshot, as for a complete library copy); files are
//! copied by `rawtonic_ingest::catalog_export`, never here.
//!
//! Asset ids stay the same, so Import from Catalog finds each photograph in
//! the catalog it came from by its id.
use crate::{
    Library, Manifest, Mode, SCHEMA_VERSION, db_name, external_root_key, insert_root,
    manifest_name, prune_blobs,
};
use anyhow::{Context, Result, ensure};
use rawtonic_metadata::atomic_write;
use rusqlite::{TransactionBehavior, params};
use std::{fs, path::Path};
use uuid::Uuid;

/// Reads the manifest of the library (session or catalog) at `source`.
pub fn read_manifest(source: &Path) -> Result<Manifest> {
    let path = source.join(if source.join("session.json").is_file() {
        "session.json"
    } else {
        "catalog.json"
    });
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(&path).context("No Rawtonic session or catalog in this folder")?,
    )?;
    ensure!(
        manifest.schema_version == SCHEMA_VERSION,
        "Unsupported manifest version {}",
        manifest.schema_version
    );
    crate::session_folders::validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Copies the database of the library at `source` into `folder` as it is
/// now (a consistent read snapshot, WAL included), without migrating,
/// configuring or checkpointing the source, and returns the copy's path.
/// `folder` must not hold a database yet.
pub fn snapshot_database(
    source: &Path,
    manifest: &Manifest,
    folder: &Path,
) -> Result<std::path::PathBuf> {
    let source_db = source.join(db_name(manifest.mode));
    ensure!(
        source_db.is_file() && !fs::symlink_metadata(&source_db)?.file_type().is_symlink(),
        "Library database is missing or a symbolic link"
    );
    let conn = rusqlite::Connection::open_with_flags(
        &source_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let database = folder.join(db_name(Mode::Catalog));
    ensure!(!database.exists(), "Snapshot destination already exists");
    conn.execute("VACUUM INTO ?", [database.to_string_lossy().as_ref()])?;
    Ok(database)
}

impl Library {
    /// A new catalog in the empty folder `folder` holding the photographs
    /// `keep` of the library at `source` (session or catalog) with all
    /// their records, under a new catalog id and `name`. Their paths are
    /// still the source's (relative ones now relative to `folder`):
    /// `internalize_assets` / `reference_assets` place them.
    pub fn subset_from(
        source: &Path,
        folder: &Path,
        name: &str,
        keep: &[String],
    ) -> Result<Library> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 255,
            "Library name must be 1–255 bytes"
        );
        let manifest = read_manifest(source)?;
        snapshot_database(source, &manifest, folder)?;
        let subset = Manifest {
            schema_version: SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            mode: Mode::Catalog,
            capture: "Photos".into(),
            output: "Output".into(),
            selects: crate::session_folders::default_selects(),
        };
        atomic_write(
            &folder.join(manifest_name(Mode::Catalog)),
            serde_json::to_vec_pretty(&subset)?,
        )?;
        let library = Library::open(folder)?;
        library.retain_only(keep)?;
        Ok(library)
    }

    /// Removes every photograph but `keep` with all its records, the albums
    /// left without photographs (and groups left without albums), the
    /// catalog folders nothing is in any more, and what belongs to the
    /// source library rather than to its photographs: queued jobs, the file
    /// operation journal, the watched folder and the folder drive cache.
    /// One transaction. Every id in `keep` must be a photograph here.
    pub fn retain_only(&self, keep: &[String]) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TEMP TABLE keep_assets(id TEXT PRIMARY KEY)")?;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO temp.keep_assets(id) VALUES(?)")?;
            for id in keep {
                insert.execute([id])?;
            }
        }
        let (wanted, found): (i64, i64) = tx.query_row(
            "SELECT count(*), (SELECT count(*) FROM assets WHERE id IN (SELECT id FROM temp.keep_assets))
             FROM temp.keep_assets",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(wanted > 0, "Choose at least one photograph");
        ensure!(
            wanted == found,
            "{} of the chosen photographs are not in this library any more",
            wanted - found
        );
        tx.execute_batch(
            "DELETE FROM masks WHERE version_id IN
               (SELECT id FROM versions WHERE asset_id NOT IN (SELECT id FROM temp.keep_assets));
             DELETE FROM assets WHERE id NOT IN (SELECT id FROM temp.keep_assets);
             DELETE FROM collections WHERE kind='album'
               AND NOT EXISTS (SELECT 1 FROM collection_assets ca WHERE ca.collection_id=collections.id);",
        )?;
        // Groups emptied by the albums that went, innermost first.
        while tx.execute(
            "DELETE FROM collections WHERE kind='group'
               AND NOT EXISTS (SELECT 1 FROM collections c WHERE c.parent=collections.id)",
            [],
        )? > 0
        {}
        tx.execute_batch(
            "DELETE FROM roots WHERE external=1 AND id NOT IN (SELECT root_id FROM assets);
             DELETE FROM import_batches WHERE id NOT IN (SELECT batch_id FROM asset_imports);
             DELETE FROM tasks;
             DELETE FROM file_ops_log;
             DELETE FROM hot_folder;
             DELETE FROM hot_folder_files;
             DELETE FROM folder_root_volumes;
             DROP TABLE temp.keep_assets;",
        )?;
        prune_blobs(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Records each photograph at the file `path` outside this catalog's
    /// folder, as a catalog folder registers it (a photograph exported by
    /// reference from a session: its file stays in the session). Nothing
    /// on disk changes; a file that is not there is marked missing.
    pub fn reference_assets(&self, places: &[(String, std::path::PathBuf)]) -> Result<()> {
        ensure!(
            self.manifest.mode == Mode::Catalog,
            "Only a catalog refers to photographs outside its folder"
        );
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for (id, path) in places {
            ensure!(
                path.is_absolute(),
                "Not an absolute path: {}",
                path.display()
            );
            ensure!(
                !path.starts_with(&self.directory),
                "{} is inside the catalog",
                path.display()
            );
            let parent = path.parent().context("No parent")?;
            let name = path.file_name().context("No filename")?;
            let root = external_root_key(parent);
            insert_root(&tx, &root, &parent.to_string_lossy())?;
            ensure!(
                tx.execute(
                    "UPDATE assets SET root_id=?,rel_path=?,missing=? WHERE id=?",
                    params![
                        root,
                        crate::portable_relative(Path::new(name))?,
                        !path.is_file(),
                        id
                    ],
                )? == 1,
                "Unknown asset {id}"
            );
        }
        tx.execute(
            "DELETE FROM roots WHERE external=1 AND id NOT IN (SELECT root_id FROM assets)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rebuilds the search index and the database file so nothing of the
    /// removed photographs stays behind (an exported catalog is handed on):
    /// full-text deletions only mark entries until their index segments
    /// merge, and `VACUUM` alone keeps live segments as they are. The index
    /// is made again from its remaining rows with its own definition.
    pub fn compact(&self) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let ddl: String = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='asset_search'",
            [],
            |r| r.get(0),
        )?;
        tx.execute_batch(
            "CREATE TEMP TABLE kept_search AS SELECT asset_id, content FROM asset_search;
             DROP TABLE asset_search;",
        )?;
        tx.execute_batch(&ddl)?;
        tx.execute_batch(
            "INSERT INTO asset_search(asset_id, content) SELECT asset_id, content FROM temp.kept_search;
             DROP TABLE temp.kept_search;",
        )?;
        tx.commit()?;
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }
}
