//! One folder, one record; one file, one photograph.
//!
//! A catalog finds a photograph outside its folder by its folder's record
//! (`roots`) and its name there. Two records for one folder gave one file
//! two rows, and path lookups picked between them: a rating saved to one
//! row was read back from the other and written over (notes:
//! parity-gaps/G39-catalogs.md, RATE). Windows catalogs got such pairs
//! when their verbatim folder spellings (`\\?\C:\…`) became plain ones.
//!
//! Every place that registers a folder now asks [`root_for`] for the record
//! the folder has, and a catalog that holds such pairs merges them once,
//! when it opens ([`Library::merge_duplicate_roots`]): a backup of the
//! database first, then one transaction. Rows only: no file is touched.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use crate::{Library, external_root_key, write_metadata, write_recipe};

/// The name of the snapshot that keeps a merged row's edit.
pub const REPAIR_SNAPSHOT: &str = "Before duplicate repair";

/// What the last repair merged, until the app has said so (`.cache`).
const REPORT: &str = "duplicate-repair.json";

/// The SQL function that gives a folder path's [`folder_key`].
pub(crate) const FOLDER: &str = "rawtonic_folder";

/// What makes two spellings of a folder path one folder: no trailing
/// separator; on Windows either separator, and letters of any case (its
/// file systems ignore case).
pub(crate) fn folder_key(path: &str) -> String {
    let mut key = if cfg!(windows) {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_string()
    };
    let separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    // A drive's or the file system's own root keeps its separator.
    while key.len() > 1 && key.ends_with(separator) && !key[..key.len() - 1].ends_with(':') {
        key.pop();
    }
    key
}

/// The record of `folder` (an external folder of the library): the one it
/// has (by [`folder_key`], whatever its id: a record keeps its id when its
/// folder is relinked or the catalog moved), else the one its spelling
/// names. Of several, the one whose id its own path gives, else the oldest,
/// as the repair keeps.
pub(crate) fn root_for(conn: &Connection, folder: &Path) -> Result<String> {
    let path = folder.to_string_lossy();
    let mut found = conn.prepare_cached(&format!(
        "SELECT id,path FROM roots WHERE external=1 AND {FOLDER}(path)={FOLDER}(?1) ORDER BY rowid"
    ))?;
    let records: Vec<(String, String)> = found
        .query_map([path.as_ref()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(keeper(&records).map_or_else(|| external_root_key(folder), |(id, _)| id))
}

/// Of the records `(id, path)` of one folder (oldest first), the one kept:
/// the one whose id its path gives (registering found it: the latest
/// ratings, labels and edits were saved to its photographs; `true`), else
/// the oldest (`false`).
fn keeper(records: &[(String, String)]) -> Option<(String, bool)> {
    records
        .iter()
        .find(|(id, path)| *id == external_root_key(Path::new(path)))
        .map(|(id, _)| (id.clone(), true))
        .or_else(|| records.first().map(|(id, _)| (id.clone(), false)))
}

/// One photograph the repair merged: the row kept and the rows merged into
/// it, what was taken from them, and the snapshots that keep their edits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPhoto {
    pub path: PathBuf,
    pub kept: String,
    pub merged: Vec<String>,
    /// Metadata fields taken from a merged row (the kept row had none), as
    /// `field <- row`.
    pub taken: Vec<String>,
    /// Snapshots added to the kept row (and its copies) for merged edits.
    pub snapshots: usize,
}

/// What a repair merged, and where the database was saved before it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateRepair {
    pub photos: Vec<MergedPhoto>,
    pub backup: Option<PathBuf>,
}

impl Library {
    /// Whether two records name one folder (the repair has work).
    fn has_duplicate_roots(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT 1 FROM roots WHERE external=1 GROUP BY {FOLDER}(path) HAVING count(*)>1 LIMIT 1"
                ),
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Merges the records of each folder registered more than once into
    /// one, and each file's rows into one photograph (see the module). The
    /// row kept is the one under the record the folder's spelling names
    /// (where ratings, labels and edits were saved); a metadata field it
    /// has no value for takes a merged row's; a merged row's edit that
    /// differs becomes a snapshot of the kept one (its edit, when the kept
    /// row has none); albums, virtual copies, snapshots, stacks and other
    /// records come along where the kept row has none. Saves the database
    /// beside itself first; does nothing when there is nothing to merge.
    pub(crate) fn merge_duplicate_roots(&self) -> Result<()> {
        if !self.has_duplicate_roots()? {
            return Ok(());
        }
        let backup = self.backup_database("before-duplicate-repair")?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut report = DuplicateRepair {
            photos: Vec::new(),
            backup: Some(backup.clone()),
        };
        let mut folders: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for record in tx
            .prepare("SELECT id,path FROM roots WHERE external=1 ORDER BY rowid")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (id, path) = record?;
            folders
                .entry(folder_key(&path))
                .or_default()
                .push((id, path));
        }
        for records in folders.into_values().filter(|r| r.len() > 1) {
            let (keeper, hashed) = keeper(&records).context("A record")?;
            let path = records
                .iter()
                .find(|(id, _)| *id == keeper)
                .map(|(_, path)| path.clone())
                .context("A record")?;
            // The records the path lookup searched before: `asset_at`
            // compared the exact path, and of those holding the file the
            // oldest came first.
            let exact: Vec<String> = records
                .iter()
                .filter(|(_, p)| *p == path)
                .map(|(id, _)| id.clone())
                .collect();
            // Each file's rows, oldest record first.
            let mut files: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
            for (record, _) in &records {
                let rows: Vec<(String, String)> = tx
                    .prepare("SELECT id,rel_path FROM assets WHERE root_id=? ORDER BY rowid")?
                    .query_map([record], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (asset, rel) in rows {
                    files.entry(rel).or_default().push((record.clone(), asset));
                }
            }
            for (rel, rows) in files {
                let in_keeper = rows.iter().find(|(record, _)| *record == keeper);
                // Who held each field's latest value (see `Authority`).
                let authority = Authority {
                    registering: hashed && in_keeper.is_some(),
                    looked_up: rows
                        .iter()
                        .find(|(record, _)| exact.contains(record))
                        .map(|(_, asset)| asset.clone()),
                };
                let kept = in_keeper.unwrap_or(&rows[0]).1.clone();
                tx.execute(
                    "UPDATE assets SET root_id=? WHERE id=?",
                    params![keeper, kept],
                )?;
                // The looked-up row first: its fields count, the others only fill.
                let mut others: Vec<String> = rows
                    .iter()
                    .map(|(_, asset)| asset.clone())
                    .filter(|asset| *asset != kept)
                    .collect();
                others.sort_by_key(|asset| Some(asset) != authority.looked_up.as_ref());
                if others.is_empty() {
                    continue;
                }
                let mut photo = MergedPhoto {
                    path: crate::native_join(Path::new(&path), &rel),
                    kept: kept.clone(),
                    merged: Vec::new(),
                    taken: Vec::new(),
                    snapshots: 0,
                };
                for asset in others {
                    let merged = self.merge_asset(&tx, &kept, &asset, &authority)?;
                    photo.merged.push(asset);
                    photo.taken.extend(merged.taken);
                    photo.snapshots += merged.snapshots;
                }
                report.photos.push(photo);
            }
            for (other, _) in records.iter().filter(|(id, _)| *id != keeper) {
                // The drive the merged record knew, when the kept one knows none.
                tx.execute(
                    "UPDATE roots SET
                       volume_id=coalesce(volume_id,(SELECT volume_id FROM roots WHERE id=?1)),
                       volume_name=coalesce(volume_name,(SELECT volume_name FROM roots WHERE id=?1)),
                       volume_path=coalesce(volume_path,(SELECT volume_path FROM roots WHERE id=?1)),
                       volume_kind=coalesce(volume_kind,(SELECT volume_kind FROM roots WHERE id=?1))
                     WHERE id=?2",
                    params![other, keeper],
                )?;
                tx.execute("DELETE FROM roots WHERE id=?", [other])?;
            }
        }
        crate::prune_blobs(&tx)?;
        tx.commit()?;
        for photo in &report.photos {
            log::info!(
                "Duplicate repair: {} kept row {}, merged {:?}, fields taken {:?}, {} snapshot(s) \"{REPAIR_SNAPSHOT}\"",
                photo.path.display(),
                photo.kept,
                photo.merged,
                photo.taken,
                photo.snapshots
            );
        }
        log::info!(
            "Duplicate repair: {} photograph(s) merged; database saved before at {}",
            report.photos.len(),
            backup.display()
        );
        if !report.photos.is_empty() {
            let cache = self.directory.join(".cache");
            std::fs::create_dir_all(&cache)?;
            rawtonic_fs::atomic_write(&cache.join(REPORT), serde_json::to_vec_pretty(&report)?)?;
        }
        Ok(())
    }

    /// What the last repair merged, once: the app shows it, and it is not
    /// shown again.
    pub fn take_duplicate_repair(&self) -> Result<Option<DuplicateRepair>, crate::CatalogError> {
        let path = self.directory.join(".cache").join(REPORT);
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(None);
        };
        // Our own note, not a user file.
        std::fs::remove_file(&path)?;
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// A consistent copy of the database beside it, named
    /// `<database>.<label>-<date>[-n]`; never over another file.
    fn backup_database(&self, label: &str) -> Result<PathBuf> {
        let database = self
            .directory
            .join(crate::layout::database_name(self.manifest.mode));
        let date = chrono::Local::now().format("%Y%m%d");
        let name = database
            .file_name()
            .context("No database name")?
            .to_string_lossy()
            .into_owned();
        let temp = rawtonic_fs::temp::file_for(&database)?;
        let temp_path = temp.path().to_path_buf();
        // VACUUM INTO writes a new file only.
        drop(temp);
        std::fs::remove_file(&temp_path).ok();
        self.conn
            .execute("VACUUM INTO ?", [temp_path.to_string_lossy().as_ref()])?;
        for n in 1.. {
            let target = self.directory.join(if n == 1 {
                format!("{name}.{label}-{date}")
            } else {
                format!("{name}.{label}-{date}-{n}")
            });
            match rawtonic_fs::rename_no_clobber(&temp_path, &target) {
                Ok(_) => return Ok(target),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    std::fs::remove_file(&temp_path).ok();
                    return Err(e.into());
                }
            }
        }
        unreachable!()
    }

    /// Merges the row `from` into `into` (two rows of one file) and deletes
    /// `from`'s row.
    fn merge_asset(
        &self,
        tx: &Connection,
        into: &str,
        from: &str,
        authority: &Authority,
    ) -> Result<Merged> {
        let mut merged = Merged::default();
        let mut meta = self.metadata(into)?;
        let theirs = self.metadata(from)?;
        // Each field from the row that held its latest value (`Authority`);
        // where none is known, the kept row's, filled from `from` when it
        // has none. Every field taken is named in the report.
        let decide = |written_by: Option<&str>, empty: bool, other_empty: bool| match written_by {
            Some(row) => row == from,
            None => empty && !other_empty,
        };
        let registering = authority.registering.then_some(into);
        let looked_up = authority.looked_up.as_deref();
        let mut take = |field: &str, taken: bool, differs: bool| {
            let taken = taken && differs;
            if taken {
                merged.taken.push(format!("{field} <- {from}"));
            }
            taken
        };
        if take(
            "rating",
            decide(registering, meta.rating == 0, theirs.rating == 0),
            meta.rating != theirs.rating,
        ) {
            meta.rating = theirs.rating;
        }
        if take(
            "label",
            decide(registering, meta.label.is_empty(), theirs.label.is_empty()),
            meta.label != theirs.label,
        ) {
            meta.label = theirs.label.clone();
        }
        if take(
            "keywords",
            decide(
                registering,
                meta.keywords.is_empty(),
                theirs.keywords.is_empty(),
            ),
            meta.keywords != theirs.keywords,
        ) {
            meta.keywords = theirs.keywords.clone();
        }
        if take(
            "flag",
            decide(looked_up, meta.flag == 0, theirs.flag == 0),
            meta.flag != theirs.flag,
        ) {
            meta.flag = theirs.flag;
        }
        for (field, ours, other) in [
            ("title", &mut meta.title, &theirs.title),
            ("description", &mut meta.description, &theirs.description),
            ("creator", &mut meta.creator, &theirs.creator),
            ("copyright", &mut meta.copyright, &theirs.copyright),
        ] {
            if take(
                field,
                decide(looked_up, ours.is_empty(), other.is_empty()),
                ours != other,
            ) {
                *ours = other.clone();
            }
        }
        match looked_up {
            Some(row) => {
                if take("iptc", row == from, meta.iptc != theirs.iptc) {
                    meta.iptc = theirs.iptc.clone();
                }
            }
            None => {
                for (key, value) in &theirs.iptc {
                    let ours = meta.iptc.get(key).is_none_or(String::is_empty);
                    if take(&format!("iptc:{key}"), ours && !value.is_empty(), true) {
                        meta.iptc.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        if !merged.taken.is_empty() {
            write_metadata(tx, into, &meta)?;
        }
        // Versions: the primary and copies of the same name pair up; the
        // others' copies move.
        let versions = |asset: &str| -> Result<BTreeMap<Option<String>, String>> {
            Ok(tx
                .prepare("SELECT id,is_primary,name FROM versions WHERE asset_id=?")?
                .query_map([asset], |r| {
                    let primary: bool = r.get(1)?;
                    let name: String = r.get(2)?;
                    Ok(((!primary).then_some(name), r.get(0)?))
                })?
                .collect::<rusqlite::Result<_>>()?)
        };
        let ours = versions(into)?;
        for (name, version) in versions(from)? {
            match ours.get(&name) {
                None => {
                    tx.execute(
                        "UPDATE versions SET asset_id=? WHERE id=?",
                        params![into, version],
                    )?;
                }
                Some(kept) => {
                    merged.snapshots += self.merge_version(tx, kept, &version)?;
                }
            }
        }
        // Albums: both.
        tx.execute(
            "INSERT OR IGNORE INTO collection_assets(collection_id,asset_id)
             SELECT collection_id,? FROM collection_assets WHERE asset_id=?",
            params![into, from],
        )?;
        // An Edit With render's link.
        tx.execute(
            "UPDATE assets SET stack=? WHERE stack=?",
            params![into, from],
        )?;
        // Every other record of a photograph: `from`'s that the kept row's
        // own do not take the place of (by the record's key, where the
        // photograph is part of one: a copy's stack membership beside the
        // photograph's own); in a table without such a key, `from`'s only
        // when the kept row has none there.
        for (table, column, set_null, keyed) in asset_references(tx)? {
            if [
                "versions",
                "metadata",
                "collection_assets",
                "asset_keywords",
            ]
            .contains(&table.as_str())
            {
                continue;
            }
            if set_null {
                tx.execute(
                    &format!("UPDATE \"{table}\" SET \"{column}\"=?1 WHERE \"{column}\"=?2"),
                    params![into, from],
                )?;
            } else if keyed {
                tx.execute(
                    &format!(
                        "UPDATE OR IGNORE \"{table}\" SET \"{column}\"=?1 WHERE \"{column}\"=?2"
                    ),
                    params![into, from],
                )?;
            } else {
                tx.execute(
                    &format!(
                        "UPDATE \"{table}\" SET \"{column}\"=?1 WHERE \"{column}\"=?2
                         AND NOT EXISTS (SELECT 1 FROM \"{table}\" WHERE \"{column}\"=?1)"
                    ),
                    params![into, from],
                )?;
            }
        }
        tx.execute("DELETE FROM assets WHERE id=?", [from])?;
        Ok(merged)
    }

    /// Merges version `from` into `into` (the same version of one file):
    /// `from`'s snapshots move; its edit becomes `into`'s when that has
    /// none, else a snapshot of it when it differs. A version's edit is its
    /// recipe's editor state, a virtual copy's its stored state
    /// (`legacy_state`, `save_copy_state`) when it has no recipe. Returns
    /// the snapshots added for edits.
    fn merge_version(&self, tx: &Connection, into: &str, from: &str) -> Result<usize> {
        tx.execute(
            "UPDATE OR IGNORE snapshots SET version_id=? WHERE version_id=?",
            params![into, from],
        )?;
        let Some(theirs) = self.version_edit(tx, from)? else {
            return Ok(0);
        };
        match self.version_edit(tx, into)? {
            None => {
                match self.recipe(from)? {
                    Some(recipe) => write_recipe(tx, into, &serde_json::to_string(&recipe)?)?,
                    None => {
                        tx.execute(
                            "UPDATE versions SET legacy_state=(SELECT legacy_state FROM versions WHERE id=?1) WHERE id=?2",
                            params![from, into],
                        )?;
                    }
                }
                Ok(0)
            }
            Some(ours) if ours == theirs => Ok(0),
            Some(_) => {
                crate::snapshots::insert(
                    tx,
                    &self.directory,
                    into,
                    &[crate::Snapshot::new(REPAIR_SNAPSHOT, theirs)?],
                )?;
                Ok(1)
            }
        }
    }

    /// The adjustments of `version`'s edit, if it has any.
    fn version_edit(&self, tx: &Connection, version: &str) -> Result<Option<Value>> {
        let state = match self.recipe(version)? {
            Some(recipe) => recipe.get("editor_state").cloned(),
            None => tx
                .query_row(
                    "SELECT legacy_state FROM versions WHERE id=?",
                    [version],
                    |r| r.get::<_, Option<String>>(0),
                )?
                .map(|text| serde_json::from_str::<Value>(&text))
                .transpose()?,
        };
        Ok(state
            .and_then(|s| s.get("adjustments").cloned())
            .filter(|a| a.as_object().is_some_and(|a| !a.is_empty())))
    }
}

/// Which row of one file held the latest value of each field. Before
/// `root_for`, registering (`add_asset`: the record whose id its path
/// gives) and the path lookup (`asset_at`: the oldest record with the exact
/// path) picked different rows of a file held twice. Every save through the
/// editor state went to the registering row: ratings, colour labels,
/// keywords (tag edits) and Develop edits, read from the looked-up row and
/// written over the registering one. Flags, title, caption, creator,
/// copyright and IPTC were written through the path lookup. So each field
/// comes from the row its writes went to, a deliberate clear too
/// (coordinator decision, RATE); where that row is not known (rows this did
/// not make), the kept row's value, filled from another row when it has
/// none.
struct Authority {
    /// The kept row is the one registering found.
    registering: bool,
    /// The row the path lookup found, if any.
    looked_up: Option<String>,
}

#[derive(Default)]
struct Merged {
    taken: Vec<String>,
    snapshots: usize,
}

/// Every column that refers to a photograph (`assets.id`): its table, the
/// column, whether deleting the photograph clears it (rather than deleting
/// its row), and whether the column is part of a unique key of the table.
fn asset_references(tx: &Connection) -> Result<Vec<(String, String, bool, bool)>> {
    let tables: Vec<String> = tx
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for table in tables {
        let references: Vec<(String, String, Option<String>, String)> = tx
            .prepare(&format!("PRAGMA foreign_key_list(\"{table}\")"))?
            .query_map([], |r| Ok((r.get(2)?, r.get(3)?, r.get(4)?, r.get(6)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (parent, column, to, on_delete) in references {
            if parent != "assets" || to.as_deref().is_some_and(|to| to != "id") {
                continue;
            }
            let unique: Vec<String> = tx
                .prepare(&format!("PRAGMA index_list(\"{table}\")"))?
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))?
                .filter_map(|row| match row {
                    Ok((name, true)) => Some(Ok(name)),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<rusqlite::Result<_>>()?;
            let mut keyed = false;
            for index in unique {
                let columns: Vec<String> = tx
                    .prepare(&format!("PRAGMA index_info(\"{index}\")"))?
                    .query_map([], |r| r.get(2))?
                    .collect::<rusqlite::Result<_>>()?;
                keyed |= columns.contains(&column);
            }
            out.push((table.clone(), column, on_delete == "SET NULL", keyed));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::folder_key;

    #[test]
    fn spellings_of_one_folder_have_one_key() {
        assert_eq!(folder_key("/Users/a/Shoot/"), folder_key("/Users/a/Shoot"));
        assert_eq!(folder_key("/"), "/");
        assert_ne!(folder_key("/Users/a/Shoot"), folder_key("/Users/a/Shoot2"));
        if cfg!(windows) {
            assert_eq!(folder_key(r"C:\Users\A\R\"), folder_key(r"c:\users\a\r"));
            assert_eq!(folder_key("C:/Users/A/R"), folder_key(r"C:\Users\A\R"));
            assert_eq!(folder_key(r"C:\"), r"c:\");
        } else {
            // Case matters on the file systems these spellings come from.
            assert_ne!(folder_key("/Users/a/Shoot"), folder_key("/Users/a/shoot"));
        }
    }
}
