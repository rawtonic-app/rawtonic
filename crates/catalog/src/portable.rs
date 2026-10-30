//! Consistent database snapshots and remapping for complete library copies.
//! File copying/verification is owned by ingest; these methods affect only
//! the destination database, never the live source.
use crate::{
    Library, Manifest, SCHEMA_VERSION, db_name, manifest_name, portable_relative, validate_relative,
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Where a resource reference that `Library::rewrite_resources` passes on is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceUse<'a> {
    /// The photograph whose edit holds it (recipes, versions, masks).
    pub owner: Option<&'a Path>,
    /// Used by what a photograph shows now: a version's current recipe (its
    /// head), a version's state or a mask. Not: undo history, presets,
    /// styles, export recipes.
    pub current: bool,
}

impl Library {
    pub fn snapshot_from(source: &Path, folder: &Path) -> Result<Library> {
        let manifest_path = source.join(if source.join("session.json").exists() {
            "session.json"
        } else {
            "catalog.json"
        });
        let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
        ensure!(
            manifest.schema_version == SCHEMA_VERSION,
            "Unsupported manifest version"
        );
        validate_relative(Path::new(&manifest.capture))?;
        validate_relative(Path::new(&manifest.output))?;
        let source_db = source.join(db_name(manifest.mode));
        ensure!(
            !fs::symlink_metadata(&source_db)?.file_type().is_symlink(),
            "Library database cannot be a symlink"
        );
        // Do not migrate, configure or checkpoint the source database.
        let conn = rusqlite::Connection::open_with_flags(
            source_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let database = folder.join(db_name(manifest.mode));
        ensure!(!database.exists(), "Snapshot destination already exists");
        // VACUUM INTO takes a consistent SQLite read snapshot, including WAL.
        conn.execute("VACUUM INTO ?", [database.to_string_lossy().as_ref()])?;
        fs::write(
            folder.join(manifest_name(manifest.mode)),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Library::open(folder)
    }

    pub fn check_integrity(&self) -> Result<()> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        ensure!(result == "ok", "Library integrity check failed: {result}");
        let mut statement = self.conn.prepare("PRAGMA foreign_key_check")?;
        ensure!(
            statement.query([])?.next()?.is_none(),
            "Library contains broken database references"
        );
        Ok(())
    }

    /// All recipe versions, including undo history, virtual copies and presets.
    pub fn rewrite_resources(
        &self,
        mut rewrite: impl FnMut(&str, ResourceUse<'_>) -> Result<String>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (table, key, column, current) in [
            (
                "recipes",
                "id",
                "payload",
                "EXISTS(SELECT 1 FROM versions v WHERE v.head=recipes.id)",
            ),
            ("versions", "id", "legacy_state", "1"),
            ("presets", "id", "recipe_json", "0"),
            ("styles", "id", "recipe_json", "0"),
            ("export_recipes", "id", "recipe_json", "0"),
            ("masks", "id", "vector_json", "1"),
            ("snapshots", "key", "adjustments", "0"),
        ] {
            let values: Vec<(String, String, bool)> = tx
                .prepare(&format!(
                    "SELECT CAST({key} AS TEXT), {column}, {current} FROM {table}
                     WHERE {column} IS NOT NULL"
                ))?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, text, current) in values {
                let owner_query = match table {
                    "recipes" => Some(
                        "SELECT v.asset_id FROM recipes r JOIN versions v ON r.version_id=v.id WHERE r.id=?",
                    ),
                    "versions" => Some("SELECT asset_id FROM versions WHERE id=?"),
                    "snapshots" => Some(
                        "SELECT v.asset_id FROM snapshots s JOIN versions v ON s.version_id=v.id WHERE s.key=?",
                    ),
                    "masks" => Some(
                        "SELECT v.asset_id FROM masks m JOIN versions v ON m.version_id=v.id WHERE m.id=?",
                    ),
                    _ => None,
                };
                let owner = owner_query
                    .map(|sql| -> Result<PathBuf> {
                        let asset: String = tx.query_row(sql, [&id], |r| r.get(0))?;
                        Ok(self.asset(&asset)?.path)
                    })
                    .transpose()?;
                let mut value: Value = serde_json::from_str(&text)?;
                let mut error = None;
                rawtonic_metadata::resources::visit(&mut value, &mut |path| {
                    if path.is_empty() || error.is_some() {
                        return;
                    }
                    let used = ResourceUse {
                        owner: owner.as_deref(),
                        current,
                    };
                    match rewrite(path, used) {
                        Ok(next) => *path = next,
                        Err(e) => error = Some(e),
                    }
                });
                if let Some(e) = error {
                    return Err(e);
                }
                tx.execute(
                    &format!("UPDATE {table} SET {column}=? WHERE {key}=?"),
                    params![serde_json::to_string(&value)?, id],
                )?;
            }
        }
        let rasters: Vec<(String, String)> = tx
            .prepare("SELECT id,raster_path FROM masks WHERE raster_path IS NOT NULL")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, path) in rasters {
            tx.execute(
                "UPDATE masks SET raster_path=? WHERE id=?",
                params![
                    rewrite(
                        &path,
                        ResourceUse {
                            owner: None,
                            current: true
                        }
                    )?,
                    id
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn internalize_assets(&self, paths: &[(String, PathBuf)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (id, relative) in paths {
            // `relative` is a native path (the copied file's place below the
            // library folder): it becomes the stored form once, here, and
            // that form is what the row holds.
            let stored = portable_relative(relative)?;
            ensure!(
                self.directory.join(relative).is_file(),
                "Copied original missing: {}",
                relative.display()
            );
            tx.execute(
                "UPDATE assets SET root_id='session',rel_path=?,missing=0 WHERE id=?",
                params![stored, id],
            )?;
        }
        tx.execute("DELETE FROM roots WHERE external=1", [])?;
        // These are queued jobs/undoable filesystem operations of the source,
        // not edit history. Replaying them on another computer is invalid.
        tx.execute("DELETE FROM tasks", [])?;
        tx.execute("DELETE FROM file_ops_log", [])?;
        // A copy does not watch the source's hot folder: two libraries
        // would take the same captures.
        tx.execute("DELETE FROM hot_folder", [])?;
        tx.execute("DELETE FROM hot_folder_files", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Emit the snapshot's authoritative states, including offline copies.
    pub fn write_portable_sidecars(&self) -> Result<()> {
        for asset in self.assets(None)? {
            let name = asset.path.file_name().unwrap().to_string_lossy();
            if let Some(mut state) = self.editor_state(&asset.id)? {
                rawtonic_metadata::resources::localize(&mut state, &self.directory);
                fs::write(
                    asset.path.with_file_name(format!("{name}.rrdata")),
                    serde_json::to_vec(&state)?,
                )?;
            }
            for copy in self.copies(&asset.id)? {
                if let Some(mut state) = self.copy_state(&asset.id, &copy)? {
                    rawtonic_metadata::resources::localize(&mut state, &self.directory);
                    fs::write(
                        asset.path.with_file_name(format!("{name}.{copy}.rrdata")),
                        serde_json::to_vec(&state)?,
                    )?;
                }
            }
        }
        Ok(())
    }
}
