//! Adding photographs to a library in three steps: the caller finds the
//! files (a folder walk, an ingest's copies, an explicit list), everything
//! each new photograph brings from its own files is read and validated
//! (`plan_add`), then all of them are inserted with it in one transaction
//! (`insert_planned`). A failure keeps none of them, so a retry adds them
//! complete; nothing is ever written to or beside the files.
use crate::{
    Asset, Library, Mode, external_root_key, fingerprint, insert_root, search_text,
    validate_relative, write_metadata, write_recipe,
};
use anyhow::{Context, Result, ensure};
use rawtonic_metadata::Metadata;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

/// What brought a batch of photographs into the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportKind {
    /// Copied in (verified ingest), or added where they are by an import.
    Ingest,
    /// Found in a folder the library added or opened.
    Add,
    /// New files a Synchronize Folder found.
    Sync,
}

impl ImportKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Add => "add",
            Self::Sync => "sync",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ingest" => Self::Ingest,
            "add" => Self::Add,
            "sync" => Self::Sync,
            _ => return None,
        })
    }
}

/// The newest import batch (Lightroom's Previous Import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportBatchInfo {
    pub id: i64,
    pub kind: ImportKind,
    /// When it started (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub created_at: String,
    /// Its photographs still in the library.
    pub count: usize,
}

/// What a photograph brings from its own files besides itself: the editor
/// state a `.rrdata` sidecar holds, metadata (from its XMP sidecar or
/// another program's catalog), and the digest of the XMP the metadata was
/// read from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contents {
    pub editor_state: Option<Value>,
    /// Takes precedence over the rating, keywords and label the editor
    /// state carries.
    pub metadata: Option<Metadata>,
    /// The XMP sidecar `metadata` was read from, by digest: the library
    /// then knows it holds exactly that sidecar's metadata.
    pub xmp_digest: Option<String>,
}

impl Contents {
    /// Reads `photo`'s sidecars: the editor sidecar (`<name>.rrdata`) when
    /// there is one, else its XMP sidecar (`<stem>.xmp`), whose rating,
    /// label and keywords also seed an editor state. Reads only; an
    /// unreadable sidecar is an error, so the photograph is not added
    /// without what it holds.
    pub fn read_sidecars(photo: &Path) -> Result<Self> {
        let name = photo.file_name().context("No filename")?;
        let editor = photo.with_file_name(format!("{}.rrdata", name.to_string_lossy()));
        if editor.is_file() {
            let bytes =
                fs::read(&editor).with_context(|| format!("Cannot read {}", editor.display()))?;
            let state: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a valid editor sidecar", editor.display()))?;
            return Ok(Self {
                editor_state: Some(state),
                ..Self::default()
            });
        }
        let xmp = photo.with_extension("xmp");
        if xmp.is_file() {
            let text = fs::read_to_string(&xmp)
                .with_context(|| format!("Cannot read {}", xmp.display()))?;
            let (metadata, _) = rawtonic_metadata::read_xmp(&text)
                .with_context(|| format!("{} is not a readable XMP sidecar", xmp.display()))?;
            let mut tags = metadata.keywords.clone();
            if !metadata.label.is_empty() {
                tags.push(format!("color:{}", metadata.label.to_lowercase()));
            }
            return Ok(Self {
                editor_state: Some(
                    json!({"version":1,"rating":metadata.rating,"tags":tags,"adjustments":{}}),
                ),
                xmp_digest: Some(rawtonic_metadata::digest(text.as_bytes())),
                metadata: Some(metadata),
            });
        }
        Ok(Self::default())
    }

    fn is_empty(&self) -> bool {
        self.editor_state.is_none() && self.metadata.is_none()
    }
}

/// A new photograph read and validated, ready for `insert_planned`.
#[derive(Debug, Clone)]
pub struct PlannedAsset {
    /// Canonical path of the file.
    pub path: PathBuf,
    pub(crate) root: String,
    pub(crate) rel: String,
    root_path: String,
    pub size: u64,
    pub mtime: i64,
    pub fingerprint: String,
    kind: String,
    recipe: Option<String>,
    metadata: Option<Metadata>,
    xmp_digest: Option<String>,
}

/// What `add_files` did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AddReport {
    /// Photographs added, with what their sidecars hold.
    pub added: Vec<Asset>,
    /// Files that were registered already (left as they are).
    pub known: Vec<Asset>,
    /// Files not added and why (not a file, outside a session, an
    /// unreadable sidecar): nothing of them is in the library.
    pub failed: Vec<(PathBuf, String)>,
}

/// While it lives, photographs the library adds join one import batch
/// (`Library::import_batch`).
pub struct ImportBatch<'a> {
    library: &'a Library,
}

impl Drop for ImportBatch<'_> {
    fn drop(&mut self) {
        self.library.import.set(None);
    }
}

/// The canonical path, root, path in the root and root folder of a file.
struct Place {
    path: PathBuf,
    root: String,
    rel: String,
    root_path: String,
}

impl Library {
    /// Photographs added until the returned guard is dropped form one
    /// import batch of `kind`, recorded with the first of them (a batch
    /// that adds nothing leaves no trace). The newest batch is Previous
    /// Import.
    pub fn import_batch(&self, kind: ImportKind) -> ImportBatch<'_> {
        self.import.set(Some((kind, None)));
        ImportBatch { library: self }
    }

    fn place(&self, path: &Path) -> Result<Place> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot read {}", path.display()))?;
        ensure!(path.is_file(), "{} is not a file", path.display());
        let (root, relative, root_path) = if let Ok(relative) = path.strip_prefix(&self.directory) {
            (
                "session".to_string(),
                relative.to_path_buf(),
                ".".to_string(),
            )
        } else {
            ensure!(
                self.manifest.mode == Mode::Catalog,
                "Session assets must be inside the session; use verified ingest to copy them"
            );
            let parent = path.parent().context("No parent")?;
            (
                external_root_key(parent),
                PathBuf::from(path.file_name().context("No filename")?),
                parent.to_string_lossy().into_owned(),
            )
        };
        validate_relative(&relative)?;
        let rel = relative
            .to_str()
            .context("Paths must be valid UTF-8")?
            .to_string();
        Ok(Place {
            path,
            root,
            rel,
            root_path,
        })
    }

    /// Reads `path` (size, modification time, content fingerprint) and
    /// validates what it brings (`contents`) as it will be stored. Writes
    /// nothing.
    pub fn plan_add(&self, path: &Path, contents: Contents) -> Result<PlannedAsset> {
        let place = self.place(path)?;
        let meta = fs::metadata(&place.path)?;
        let fingerprint = fingerprint(&place.path)?;
        let mtime = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let (recipe, metadata) = self.resolve_contents(&contents, Metadata::default())?;
        Ok(PlannedAsset {
            kind: place
                .path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
            path: place.path,
            root: place.root,
            rel: place.rel,
            root_path: place.root_path,
            size: meta.len(),
            mtime,
            fingerprint,
            recipe,
            metadata,
            xmp_digest: contents.xmp_digest,
        })
    }

    /// The recipe payload and metadata `contents` are stored as; `base` is
    /// the metadata the editor state applies to when `contents` has none.
    fn resolve_contents(
        &self,
        contents: &Contents,
        base: Metadata,
    ) -> Result<(Option<String>, Option<Metadata>)> {
        let base = contents.metadata.clone().unwrap_or(base);
        let (recipe, from_state) = match &contents.editor_state {
            Some(state) => {
                let (recipe, metadata) = self.editor_recipe(state, base)?;
                (Some(recipe), Some(metadata))
            }
            None => (None, None),
        };
        let metadata = contents.metadata.clone().or(from_state);
        if let Some(metadata) = &metadata {
            metadata.validate()?;
        }
        Ok((recipe, metadata))
    }

    pub(crate) fn registered_id(&self, root: &str, rel: &str) -> Result<Option<String>> {
        registered_id(&self.conn, root, rel)
    }

    /// Inserts every planned photograph with its recipe and metadata in one
    /// transaction (into the current import batch, if any): all of them or,
    /// on any failure, none. A file registered meanwhile is left as it is.
    /// Returns each one's asset id and whether it was added.
    pub fn insert_planned(&self, planned: Vec<PlannedAsset>) -> Result<Vec<(String, bool)>> {
        let mut batch = self.import.get();
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut result = Vec::with_capacity(planned.len());
        for asset in planned {
            insert_root(&tx, &asset.root, &asset.root_path)?;
            if let Some(id) = registered_id(&tx, &asset.root, &asset.rel)? {
                result.push((id, false));
                continue;
            }
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,kind,search_text)
                 VALUES(?,?,?,?,?,?,?,?)",
                params![
                    id,
                    asset.root,
                    asset.rel,
                    i64::try_from(asset.size)?,
                    asset.mtime,
                    asset.fingerprint,
                    asset.kind,
                    asset.metadata.as_ref().map(search_text).unwrap_or_default()
                ],
            )?;
            match &asset.metadata {
                Some(metadata) => tx.execute(
                    "INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status)
                     VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        metadata.rating,
                        metadata.label,
                        metadata.flag,
                        serde_json::to_string(metadata)?,
                        asset.xmp_digest,
                        if asset.xmp_digest.is_some() {
                            "synced"
                        } else {
                            "pending"
                        }
                    ],
                )?,
                None => tx.execute("INSERT INTO metadata(asset_id) VALUES(?)", [&id])?,
            };
            let version = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO versions(id,asset_id,name,is_primary) VALUES(?,?,'Original',1)",
                params![version, id],
            )?;
            if let Some(recipe) = &asset.recipe {
                write_recipe(&tx, &version, recipe)?;
            }
            if let Some((kind, batch_id)) = &mut batch {
                let batch_id = match batch_id {
                    Some(batch_id) => *batch_id,
                    None => {
                        tx.execute(
                            "INSERT INTO import_batches(kind) VALUES(?)",
                            [kind.as_str()],
                        )?;
                        *batch_id.insert(tx.last_insert_rowid())
                    }
                };
                tx.execute(
                    "INSERT INTO asset_imports(asset_id,batch_id) VALUES(?,?)",
                    params![id, batch_id],
                )?;
            }
            result.push((id, true));
        }
        tx.commit()?;
        if self.import.get().is_some() {
            self.import.set(batch);
        }
        Ok(result)
    }

    /// Adds an explicit list of files, each with what its sidecars hold
    /// (`Contents::read_sidecars`): first every file is read, then all new
    /// ones are inserted in one transaction. Files already registered are
    /// left alone; files that cannot be added are reported, not added.
    pub fn add_files(&self, paths: &[PathBuf]) -> Result<AddReport> {
        let mut report = AddReport::default();
        let mut planned = Vec::new();
        let mut seen = HashSet::new();
        for path in paths {
            let plan = (|| -> Result<Option<PlannedAsset>> {
                let place = self.place(path)?;
                if !seen.insert(place.path.clone()) {
                    return Ok(None);
                }
                if let Some(id) = self.registered_id(&place.root, &place.rel)? {
                    report.known.push(self.asset(&id)?);
                    return Ok(None);
                }
                let contents = Contents::read_sidecars(&place.path)?;
                Ok(Some(self.plan_add(&place.path, contents)?))
            })();
            match plan {
                Ok(Some(plan)) => planned.push(plan),
                Ok(None) => {}
                Err(e) => report.failed.push((path.clone(), format!("{e:#}"))),
            }
        }
        for (id, added) in self.insert_planned(planned)? {
            let asset = self.asset(&id)?;
            if added {
                report.added.push(asset);
            } else {
                report.known.push(asset);
            }
        }
        Ok(report)
    }

    /// Gives a registered photograph that was never edited nor had metadata
    /// adopted (`AssetIndexEntry::needs_metadata_adoption`) what its
    /// sidecars hold, recipe and metadata in one transaction. Checked again
    /// inside it, so an edit made meanwhile is never replaced. Returns
    /// whether anything was adopted.
    pub fn adopt_contents(&self, id: &str, contents: &Contents) -> Result<bool> {
        if contents.is_empty() {
            return Ok(false);
        }
        self.write_contents(id, contents, true)
    }

    /// Stores `contents` for a registered photograph (an import from another
    /// program's catalog), recipe and metadata in one transaction.
    pub fn apply_contents(&self, id: &str, contents: &Contents) -> Result<()> {
        self.write_contents(id, contents, false).map(|_| ())
    }

    fn write_contents(&self, id: &str, contents: &Contents, only_new: bool) -> Result<bool> {
        let (recipe, metadata) = self.resolve_contents(contents, self.metadata(id)?)?;
        let version = self.primary_version(id)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if only_new {
            let untouched: bool = tx.query_row(
                "SELECT m.data='{}' AND (p.payload IS NULL OR json_type(p.payload,'$.editor_state') IS NULL)
                 FROM metadata m JOIN versions v ON v.asset_id=m.asset_id AND v.is_primary=1
                 LEFT JOIN recipes p ON p.id=v.head WHERE m.asset_id=?",
                [id],
                |r| r.get(0),
            )?;
            if !untouched {
                return Ok(false);
            }
        }
        if let Some(recipe) = &recipe {
            write_recipe(&tx, &version, recipe)?;
        }
        if let Some(metadata) = &metadata {
            write_metadata(&tx, id, metadata)?;
            if let Some(digest) = &contents.xmp_digest {
                tx.execute(
                    "UPDATE metadata SET xmp_hash=?,xmp_status='synced' WHERE asset_id=?",
                    params![digest, id],
                )?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// The newest import batch, if any photographs were ever added in one.
    pub fn previous_import(&self) -> Result<Option<ImportBatchInfo>> {
        let row: Option<(i64, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT b.id,b.kind,b.created_at,
                        (SELECT count(*) FROM asset_imports ai WHERE ai.batch_id=b.id)
                 FROM import_batches b ORDER BY b.id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        row.map(|(id, kind, created_at, count)| {
            Ok(ImportBatchInfo {
                id,
                kind: ImportKind::parse(&kind).context("Unknown import kind")?,
                created_at,
                count: count as usize,
            })
        })
        .transpose()
    }

    /// The photographs of the newest import batch still in the library, in
    /// path order.
    pub fn previous_import_assets(&self) -> Result<Vec<Asset>> {
        let ids: Vec<String> = self
            .conn
            .prepare(
                "SELECT a.id FROM asset_imports ai JOIN assets a ON a.id=ai.asset_id
                 WHERE ai.batch_id=(SELECT max(id) FROM import_batches) ORDER BY a.rel_path",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.iter().map(|id| self.asset(id)).collect()
    }

    /// Makes every later insert into `table` on this connection fail, as a
    /// full disk or a broken database would (tests of all-or-nothing
    /// writes). `clear_write_failures` ends it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject_write_failure(&self, table: &str) -> Result<()> {
        ensure!(
            !table.is_empty()
                && table
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "Not a table name"
        );
        self.conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS rawtonic_fail_{table} BEFORE INSERT ON {table}
             BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;"
        ))?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_write_failures(&self) -> Result<()> {
        let names: Vec<String> = self
            .conn
            .prepare("SELECT name FROM sqlite_temp_master WHERE type='trigger' AND name LIKE 'rawtonic_fail_%'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for name in names {
            self.conn
                .execute_batch(&format!("DROP TRIGGER temp.{name}"))?;
        }
        Ok(())
    }
}

fn registered_id(conn: &Connection, root: &str, rel: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM assets WHERE root_id=? AND rel_path=?",
            params![root, rel],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(dir: &Path) -> Library {
        Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap()
    }

    /// Three photographs: one with an editor sidecar, one with an XMP, one bare.
    fn photos(lib: &Library) -> Vec<PathBuf> {
        let capture = lib.directory.join("Capture");
        let names = ["A.ARW", "B.ARW", "C.ARW"];
        for name in names {
            fs::write(capture.join(name), format!("fixture {name}")).unwrap();
        }
        fs::write(
            capture.join("A.ARW.rrdata"),
            br#"{"version":1,"rating":4,"tags":["user:harbour","color:red"],"adjustments":{"exposure":1.5}}"#,
        )
        .unwrap();
        fs::write(capture.join("B.xmp"), r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Label="Blue" xmp:Rating="2"/></rdf:RDF></x:xmpmeta>"#).unwrap();
        names.iter().map(|n| capture.join(n)).collect()
    }

    fn assert_complete(lib: &Library, paths: &[PathBuf]) {
        let a = lib.asset_at(&paths[0]).unwrap().unwrap();
        assert_eq!((a.rating, a.label.as_str()), (4, "Red"));
        assert_eq!(lib.metadata(&a.id).unwrap().keywords, ["harbour"]);
        assert_eq!(
            lib.editor_state(&a.id).unwrap().unwrap()["adjustments"]["exposure"],
            1.5
        );
        let b = lib.asset_at(&paths[1]).unwrap().unwrap();
        assert_eq!((b.rating, b.label.as_str()), (2, "Blue"));
        assert_eq!(
            lib.editor_state(&b.id).unwrap().unwrap()["tags"],
            json!(["color:blue"])
        );
        let c = lib.asset_at(&paths[2]).unwrap().unwrap();
        assert!(!lib.metadata_initialized(&c.id).unwrap());
        assert_eq!(lib.summary().unwrap().assets, 3);
        // B's XMP is the metadata the library holds: nothing to write back.
        assert_eq!(lib.summary().unwrap().xmp_pending, 2);
        assert!(lib.assets(Some("harbour")).unwrap().len() == 1);
    }

    #[test]
    fn a_folder_is_added_with_its_sidecars_in_one_transaction() {
        let d = tempfile::tempdir().unwrap();
        let lib = session(d.path());
        let paths = photos(&lib);
        let report = {
            let _batch = lib.import_batch(ImportKind::Add);
            lib.add_files(&paths).unwrap()
        };
        assert_eq!((report.added.len(), report.failed.len()), (3, 0));
        assert_complete(&lib, &paths);
        let batch = lib.previous_import().unwrap().unwrap();
        assert_eq!((batch.kind, batch.count), (ImportKind::Add, 3));
        // Adding again finds them known; no new batch.
        let again = lib.add_files(&paths).unwrap();
        assert_eq!((again.added.len(), again.known.len()), (0, 3));
        assert_eq!(lib.previous_import().unwrap().unwrap().id, batch.id);
        assert_eq!(fs::read(&paths[0]).unwrap(), b"fixture A.ARW");
    }

    #[test]
    fn a_failure_mid_insert_keeps_no_photograph_and_a_retry_adds_them_complete() {
        for table in ["recipes", "versions", "metadata", "asset_imports"] {
            let d = tempfile::tempdir().unwrap();
            let lib = session(d.path());
            let paths = photos(&lib);
            lib.inject_write_failure(table).unwrap();
            {
                let _batch = lib.import_batch(ImportKind::Add);
                let error = lib.add_files(&paths).unwrap_err();
                assert!(
                    format!("{error:#}").contains("injected"),
                    "{table}: {error:#}"
                );
            }
            assert_eq!(
                lib.summary().unwrap().assets,
                0,
                "{table}: nothing half added"
            );
            assert!(lib.assets(None).unwrap().is_empty());
            assert_eq!(lib.previous_import().unwrap(), None, "{table}");
            lib.clear_write_failures().unwrap();
            let _batch = lib.import_batch(ImportKind::Add);
            let report = lib.add_files(&paths).unwrap();
            assert_eq!(report.added.len(), 3, "{table}");
            assert_complete(&lib, &paths);
            assert_eq!(lib.previous_import().unwrap().unwrap().count, 3);
        }
    }

    #[test]
    fn an_unreadable_sidecar_keeps_only_its_photograph_out() {
        let d = tempfile::tempdir().unwrap();
        let lib = session(d.path());
        let paths = photos(&lib);
        fs::write(lib.directory.join("Capture/C.ARW.rrdata"), b"{ not json").unwrap();
        let report = lib.add_files(&paths).unwrap();
        assert_eq!(report.added.len(), 2);
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].1.contains("C.ARW.rrdata"),
            "{:?}",
            report.failed
        );
        assert!(lib.asset_at(&paths[2]).unwrap().is_none());
        // Outside the session: refused, nothing registered.
        let outside = d.path().join("outside.ARW");
        fs::write(&outside, b"x").unwrap();
        assert_eq!(lib.add_files(&[outside]).unwrap().failed.len(), 1);
    }

    #[test]
    fn adoption_of_a_registered_photograph_is_all_or_nothing_and_never_replaces_an_edit() {
        let d = tempfile::tempdir().unwrap();
        let lib = session(d.path());
        let paths = photos(&lib);
        let a = lib.add_asset(&paths[0]).unwrap();
        let contents = Contents::read_sidecars(&paths[0]).unwrap();
        lib.inject_write_failure("recipes").unwrap();
        assert!(lib.adopt_contents(&a.id, &contents).is_err());
        assert!(
            !lib.metadata_initialized(&a.id).unwrap(),
            "metadata rolled back"
        );
        lib.clear_write_failures().unwrap();
        assert!(lib.adopt_contents(&a.id, &contents).unwrap());
        assert_eq!(lib.asset(&a.id).unwrap().rating, 4);
        lib.save_editor_state(&a.id, &json!({"rating":1,"tags":[],"adjustments":{}}))
            .unwrap();
        assert!(!lib.adopt_contents(&a.id, &contents).unwrap());
        assert_eq!(lib.asset(&a.id).unwrap().rating, 1);
    }
}
