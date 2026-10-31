//! Import from Catalog (G39): the work done in another Rawtonic catalog —
//! typically one made by Export as Catalog and edited on another computer —
//! brought into this catalog.
//!
//! The other library is read from a snapshot of its database
//! (`subset::snapshot_database`), never opened for writing. Each of its
//! photographs is matched to one here by its asset id (an exported catalog
//! keeps them), else by the SHA-256 of its original — recorded on both
//! sides, or read from both files when their size and content fingerprint
//! already agree. Never by file name.
//!
//! `review` sorts them into new, changed and unchanged; `apply` brings in
//! the new ones with everything that is theirs and, for changed ones,
//! what the photographer chose, in one transaction. Replacing an edit adds
//! the incoming state as a new step of the photograph's history, so Undo
//! and the History panel return to the state it replaced.
use crate::{
    Library, Metadata, external_root_key, insert_root, portable_relative, prune_blobs,
    valid_copy_id, write_metadata, write_recipe,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// How an incoming photograph relates to this catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    New,
    Changed,
    Unchanged,
}

/// What to do with a changed photograph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Choice {
    /// Its edits and metadata replace this catalog's; the replaced edit
    /// stays in the photograph's history.
    Replace,
    /// Nothing changes here.
    Keep,
    /// Its edit becomes a new virtual copy of the photograph here.
    VirtualCopy,
}

/// One photograph of the other catalog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    /// Its asset id in the other catalog.
    pub id: String,
    /// The photograph it is here, if any.
    pub target: Option<String>,
    /// "id" or "sha256".
    pub matched_by: Option<&'static str>,
    pub status: Status,
    /// For a changed photograph, what differs: "edit", "copies",
    /// "metadata", "captureTime", "eyes", "snapshots", "albums".
    pub changes: Vec<&'static str>,
    /// Only this catalog changed since the other was made from it: the
    /// other's edit is an earlier step of this photograph's history and the
    /// rest is the same. Keep current is the safe default for those.
    pub changed_here_only: bool,
    /// Where the other catalog has the original.
    pub path: PathBuf,
    /// Whether that file is there now.
    pub available: bool,
}

/// What `apply` does with each photograph (by its id in the other catalog).
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// New photographs and where their original is for this catalog (the
    /// file the other catalog refers to, or a verified copy of it).
    pub new: Vec<(String, PathBuf)>,
    /// Changed photographs and what to do with each.
    pub changed: Vec<Changed>,
    /// The import batch kind of the new ones: "add" or "ingest" (copied).
    pub batch_kind: &'static str,
    /// The name of the snapshot that keeps an edit Replace replaces.
    pub before: String,
}

/// A changed photograph of a `Plan`.
#[derive(Debug, Clone)]
pub struct Changed {
    /// Its id in the other catalog.
    pub incoming: String,
    /// The photograph here.
    pub target: String,
    pub choice: Choice,
    /// What differs (`Incoming::changes`): Replace takes only those.
    pub changes: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub added: usize,
    pub replaced: usize,
    pub virtual_copies: usize,
    pub kept: usize,
    pub albums_created: usize,
    /// The photographs here whose edits or copies changed (their derived
    /// editor sidecars are to be written again).
    pub touched: Vec<String>,
}

/// What a photograph carries that an import compares, with resources
/// named by their content (a LUT copied into an exported catalog is the
/// same LUT).
#[derive(Debug, PartialEq)]
struct Facts {
    edit: Option<Value>,
    copies: BTreeMap<String, Value>,
    metadata: Metadata,
    capture: Option<Value>,
    eyes: Vec<(i64, Option<String>)>,
    snapshots: BTreeSet<(String, String, String)>,
}

/// Resource references replaced by the SHA-256 of the file they name, read
/// once per file. `root` resolves relative references.
struct Digests {
    cache: HashMap<PathBuf, Option<String>>,
}

impl Digests {
    fn normalize(&mut self, value: &mut Value, root: &Path) {
        rawtonic_metadata::resources::visit(value, &mut |path| {
            let file = match rawtonic_metadata::resources::relative(path) {
                Some(relative) => root.join(relative),
                None => PathBuf::from(path.as_str()),
            };
            let digest = self
                .cache
                .entry(file.clone())
                .or_insert_with(|| {
                    file.is_file()
                        .then(|| {
                            crate::checksum::sha256_file(&file, &AtomicBool::new(false))
                                .ok()
                                .flatten()
                        })
                        .flatten()
                })
                .clone();
            if let Some(digest) = digest {
                *path = format!("sha256:{digest}");
            }
        });
    }
}

impl Library {
    /// The primary version's current edit as stored (blobs read back), its
    /// rating and keywords left out (the metadata holds them).
    fn stored_edit(&self, asset: &str) -> Result<Option<Value>> {
        let version = self.primary_version(asset)?;
        Ok(self.recipe(&version)?.and_then(|recipe| {
            let mut state = recipe.get("editor_state").cloned()?;
            // An edit back to the defaults (`UNEDITED`) is no edit.
            if state.get("adjustments").is_none_or(Value::is_null) {
                return None;
            }
            if let Some(map) = state.as_object_mut() {
                map.remove("rating");
                map.remove("tags");
            }
            Some(state)
        }))
    }

    fn import_facts(&self, asset: &str, root: &Path, digests: &mut Digests) -> Result<Facts> {
        let mut edit = self.stored_edit(asset)?;
        if let Some(edit) = edit.as_mut() {
            digests.normalize(edit, root);
        }
        let mut copies = BTreeMap::new();
        let rows: Vec<(String, Option<String>)> = self
            .conn
            .prepare("SELECT name,legacy_state FROM versions WHERE asset_id=? AND is_primary=0")?
            .query_map([asset], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (name, text) in rows {
            let mut value: Value = text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or(Value::Null);
            digests.normalize(&mut value, root);
            copies.insert(name, value);
        }
        let eyes = self
            .conn
            .prepare("SELECT face,override_state FROM face_eyes WHERE asset_id=? ORDER BY face")?
            .query_map([asset], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        // Of the photograph and of its virtual copies (by the copy's id).
        let snapshots = self
            .conn
            .prepare(
                "SELECT CASE WHEN v.is_primary=1 THEN '' ELSE v.name END || '/' || s.id,s.name,s.adjustments
                 FROM snapshots s JOIN versions v ON v.id=s.version_id WHERE v.asset_id=?",
            )?
            .query_map([asset], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Facts {
            edit,
            copies,
            metadata: self.metadata(asset)?,
            capture: self
                .capture_override(asset)?
                .map(serde_json::to_value)
                .transpose()?,
            eyes,
            snapshots,
        })
    }

    /// The albums holding `asset`, by id, with their name and parent.
    fn album_memberships(&self, asset: &str) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare(
                "SELECT c.id FROM collection_assets ca JOIN collections c ON c.id=ca.collection_id
                 WHERE ca.asset_id=? AND c.kind='album' ORDER BY c.id",
            )?
            .query_map([asset], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The album here that stands for album `id` of `other`: the same id,
    /// else an album of the same name in the group that stands for its
    /// group (top level for top level). None: it is to be made.
    fn matching_album(&self, other: &Library, id: &str) -> Result<Option<String>> {
        if self
            .conn
            .query_row("SELECT 1 FROM collections WHERE id=?", [id], |_| Ok(()))
            .optional()?
            .is_some()
        {
            return Ok(Some(id.to_string()));
        }
        let (name, kind, parent): (String, String, Option<String>) = other.conn.query_row(
            "SELECT name,kind,parent FROM collections WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let parent = match parent {
            Some(parent) => match self.matching_album(other, &parent)? {
                Some(here) => Some(here),
                None => return Ok(None),
            },
            None => None,
        };
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM collections WHERE name=? AND kind=? AND parent IS ?",
                params![name, kind, parent],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The photograph here that `incoming` (of `other`) is: same asset id,
    /// else the same original by SHA-256.
    fn match_incoming(
        &self,
        other: &Library,
        incoming: &crate::Asset,
        claimed: &std::collections::HashSet<String>,
    ) -> Result<Option<(String, &'static str)>> {
        if self
            .conn
            .query_row(
                "SELECT 1 FROM assets WHERE id=?",
                [&incoming.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Ok(Some((incoming.id.clone(), "id")));
        }
        let stored: Option<String> = other.conn.query_row(
            "SELECT sha256 FROM assets WHERE id=?",
            [&incoming.id],
            |r| r.get(0),
        )?;
        if let Some(sha) = &stored {
            let found: Vec<String> = self
                .conn
                .prepare("SELECT id FROM assets WHERE sha256=? ORDER BY id")?
                .query_map([sha], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if let Some(found) = found.into_iter().find(|id| !claimed.contains(id)) {
                return Ok(Some((found, "sha256")));
            }
        }
        // Not recorded on one side: the same size and content fingerprint
        // are a candidate, proven by both files' SHA-256.
        let candidates: Vec<(String, Option<String>)> = self
            .conn
            .prepare("SELECT id,sha256 FROM assets WHERE size=? AND fingerprint=?")?
            .query_map(params![incoming.size as i64, incoming.fingerprint], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let never = AtomicBool::new(false);
        let incoming_sha = match stored {
            Some(sha) => Some(sha),
            None if incoming.path.is_file() => {
                crate::checksum::sha256_file(&incoming.path, &never)?
            }
            None => None,
        };
        let Some(incoming_sha) = incoming_sha else {
            return Ok(None);
        };
        for (id, sha) in candidates {
            if claimed.contains(&id) {
                continue;
            }
            let sha = match sha {
                Some(sha) => Some(sha),
                None => {
                    let path = self.asset(&id)?.path;
                    if path.is_file() {
                        crate::checksum::sha256_file(&path, &never)?
                    } else {
                        None
                    }
                }
            };
            if sha.as_deref() == Some(incoming_sha.as_str()) {
                return Ok(Some((id, "sha256")));
            }
        }
        Ok(None)
    }

    /// Sorts every photograph of `other` (a snapshot of the catalog at
    /// `other_dir`) into new, changed and unchanged for this catalog.
    /// Reads only.
    pub fn review_import(&self, other: &Library, other_dir: &Path) -> Result<Vec<Incoming>> {
        let mut digests = Digests {
            cache: HashMap::new(),
        };
        let mut out = Vec::new();
        // In the order the other library registered them, so that of two
        // copies of one file there the earlier keeps its match here, import
        // after import.
        let mut assets = other.assets(None)?;
        let order: HashMap<String, i64> = other
            .conn
            .prepare("SELECT id, rowid FROM assets")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        assets.sort_by_key(|asset| order.get(&asset.id).copied().unwrap_or(i64::MAX));
        // A photograph here is matched by at most one incoming photograph:
        // those with its asset id first, then copies of its file by
        // content (another copy of the same file there is a photograph of
        // its own: new here).
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for asset in &assets {
            if self
                .conn
                .query_row("SELECT 1 FROM assets WHERE id=?", [&asset.id], |_| Ok(()))
                .optional()?
                .is_some()
            {
                claimed.insert(asset.id.clone());
            }
        }
        for asset in assets {
            // The snapshot sees the other catalog's own photographs inside
            // its (temporary) folder: they are in the other catalog's.
            let path = match asset.path.strip_prefix(&other.directory) {
                Ok(relative) => other_dir.join(relative),
                Err(_) => asset.path.clone(),
            };
            let available = path.is_file();
            let found = self.match_incoming(
                other,
                &crate::Asset {
                    path: path.clone(),
                    ..asset.clone()
                },
                &claimed,
            )?;
            if let Some((target, "sha256")) = &found {
                claimed.insert(target.clone());
            }
            let Some((target, by)) = found else {
                out.push(Incoming {
                    id: asset.id,
                    target: None,
                    matched_by: None,
                    status: Status::New,
                    changes: vec![],
                    changed_here_only: false,
                    path,
                    available,
                });
                continue;
            };
            let theirs = other.import_facts(&asset.id, other_dir, &mut digests)?;
            let ours = self.import_facts(&target, &self.directory, &mut digests)?;
            let mut changes = Vec::new();
            if theirs.edit != ours.edit {
                changes.push("edit");
            }
            if theirs
                .copies
                .iter()
                .any(|(name, state)| ours.copies.get(name) != Some(state))
            {
                changes.push("copies");
            }
            if theirs.metadata != ours.metadata {
                changes.push("metadata");
            }
            if theirs.capture != ours.capture {
                changes.push("captureTime");
            }
            if theirs.eyes != ours.eyes {
                changes.push("eyes");
            }
            if !theirs.snapshots.is_subset(&ours.snapshots) {
                changes.push("snapshots");
            }
            let ours_albums = self.album_memberships(&target)?;
            for album in other.album_memberships(&asset.id)? {
                match self.matching_album(other, &album)? {
                    Some(here) if ours_albums.contains(&here) => {}
                    _ => {
                        changes.push("albums");
                        break;
                    }
                }
            }
            // Edited here since: theirs is an earlier step of our history,
            // or they never edited it and we did.
            let changed_here_only = changes == ["edit"]
                && match &theirs.edit {
                    Some(edit) => self.earlier_edits(&target, &mut digests)?.contains(edit),
                    None => true,
                };
            out.push(Incoming {
                id: asset.id,
                target: Some(target),
                matched_by: Some(by),
                status: if changes.is_empty() {
                    Status::Unchanged
                } else {
                    Status::Changed
                },
                changes,
                changed_here_only,
                path,
                available,
            });
        }
        Ok(out)
    }

    /// The edits of the primary version's history before its current one,
    /// normalized like `facts`.
    fn earlier_edits(&self, asset: &str, digests: &mut Digests) -> Result<Vec<Value>> {
        let version = self.primary_version(asset)?;
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT r.id FROM recipes r JOIN versions v ON v.id=r.version_id
                 WHERE r.version_id=? AND r.id != coalesce(v.head,-1)",
            )?
            .query_map([&version], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for id in ids {
            let text: String =
                self.conn
                    .query_row("SELECT payload FROM recipes WHERE id=?", [id], |r| r.get(0))?;
            let mut value: Value = serde_json::from_str(&text)?;
            crate::load_blobs(&self.conn, &mut value)?;
            if let Some(mut state) = value.get("editor_state").cloned() {
                if let Some(map) = state.as_object_mut() {
                    map.remove("rating");
                    map.remove("tags");
                }
                digests.normalize(&mut state, &self.directory);
                out.push(state);
            }
        }
        Ok(out)
    }

    /// Applies `plan` with the photographs of `other` (a snapshot whose
    /// edit resources already name files of this catalog) in one
    /// transaction: nothing is applied when anything fails.
    pub fn apply_import(&self, other: &Library, plan: &Plan) -> Result<Report> {
        ensure!(
            self.manifest.mode == crate::Mode::Catalog,
            "Import from Catalog brings photographs into a catalog; sessions stay self-contained"
        );
        let database = other.directory.join("catalog.sqlite");
        ensure!(database.is_file(), "No snapshot database");
        self.conn.execute(
            "ATTACH DATABASE ? AS other",
            [database.to_string_lossy().as_ref()],
        )?;
        let result = self.apply_attached(other, plan);
        let detached = self.conn.execute_batch("DETACH DATABASE other");
        let report = result?;
        detached?;
        Ok(report)
    }

    fn apply_attached(&self, other: &Library, plan: &Plan) -> Result<Report> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut report = Report::default();
        // Asset ids of the other catalog → photographs here.
        let mut here: Vec<(String, String)> = Vec::new();
        if !plan.new.is_empty() {
            add_new(&tx, self, plan)?;
            report.added = plan.new.len();
            report
                .touched
                .extend(plan.new.iter().map(|(id, _)| id.clone()));
            here.extend(plan.new.iter().map(|(id, _)| (id.clone(), id.clone())));
        }
        for changed in &plan.changed {
            let (incoming, target) = (&changed.incoming, &changed.target);
            match changed.choice {
                Choice::Keep => report.kept += 1,
                Choice::Replace => {
                    replace(
                        &tx,
                        self,
                        other,
                        incoming,
                        target,
                        &changed.changes,
                        &plan.before,
                    )?;
                    here.push((incoming.clone(), target.clone()));
                    report.touched.push(target.clone());
                    report.replaced += 1;
                }
                Choice::VirtualCopy => {
                    as_virtual_copy(&tx, other, incoming, target)?;
                    report.touched.push(target.clone());
                    report.virtual_copies += 1;
                }
            }
        }
        for (incoming, target) in &here {
            for album in other.album_memberships(incoming)? {
                let (id, created) = ensure_album(&tx, self, other, &album)?;
                report.albums_created += created;
                tx.execute(
                    "INSERT OR IGNORE INTO collection_assets(collection_id,asset_id) VALUES(?,?)",
                    params![id, target],
                )?;
            }
        }
        prune_blobs(&tx)?;
        tx.commit()?;
        Ok(report)
    }
}

/// The columns of `table` here (the snapshot was migrated to the same
/// schema), in order.
fn columns(tx: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(tx
        .prepare(&format!("PRAGMA main.table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Copies the rows of `other.<table>` (aliased `o`, joined with `join`)
/// into `main.<table>`, each column as it is unless `set` gives an
/// expression for it.
fn copy_rows(tx: &Connection, table: &str, join: &str, set: &[(&str, &str)]) -> Result<usize> {
    let cols = columns(tx, table)?;
    let values: Vec<String> = cols
        .iter()
        .map(|col| {
            set.iter()
                .find(|(name, _)| name == col)
                .map(|(_, expr)| expr.to_string())
                .unwrap_or_else(|| format!("o.{col}"))
        })
        .collect();
    Ok(tx.execute(
        &format!(
            "INSERT INTO main.{table}({}) SELECT {} FROM other.{table} o {join}",
            cols.join(","),
            values.join(",")
        ),
        [],
    )?)
}

/// New photographs with all their records: versions and history, virtual
/// copies, snapshots, masks, metadata and keywords, capture-time
/// corrections, eye reads and calls; their asset ids stay.
fn add_new(tx: &Connection, lib: &Library, plan: &Plan) -> Result<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS import_new(id TEXT PRIMARY KEY, root TEXT NOT NULL, rel TEXT NOT NULL, missing INTEGER NOT NULL);
         DELETE FROM temp.import_new;",
    )?;
    for (id, path) in &plan.new {
        let (root, rel) = match path.strip_prefix(&lib.directory) {
            Ok(relative) => ("session".to_string(), portable_relative(relative)?),
            Err(_) => {
                let parent = path.parent().context("No parent")?;
                let root = external_root_key(parent);
                insert_root(tx, &root, &parent.to_string_lossy())?;
                (
                    root,
                    portable_relative(Path::new(path.file_name().context("No filename")?))?,
                )
            }
        };
        tx.execute(
            "INSERT INTO temp.import_new(id,root,rel,missing) VALUES(?,?,?,?)",
            params![id, root, rel, !path.is_file()],
        )?;
    }
    let taken: Option<String> = tx
        .query_row(
            "SELECT a.rel_path FROM temp.import_new n JOIN main.assets a ON a.root_id=n.root AND a.rel_path=n.rel",
            [],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        taken.is_none(),
        "Another photograph of this catalog is registered at {}",
        taken.unwrap_or_default()
    );
    let recipe_offset: i64 =
        tx.query_row("SELECT coalesce(max(id),0) FROM main.recipes", [], |r| {
            r.get(0)
        })?;
    let snapshot_offset: i64 =
        tx.query_row("SELECT coalesce(max(key),0) FROM main.snapshots", [], |r| {
            r.get(0)
        })?;
    let ro = recipe_offset.to_string();
    let so = snapshot_offset.to_string();
    copy_rows(
        tx,
        "assets",
        "JOIN temp.import_new n ON n.id=o.id",
        &[
            ("root_id", "n.root"),
            ("rel_path", "n.rel"),
            ("missing", "n.missing"),
            ("trashed", "0"),
        ],
    )?;
    let new_asset = "WHERE o.asset_id IN (SELECT id FROM temp.import_new)";
    copy_rows(tx, "metadata", new_asset, &[("xmp_status", "'pending'")])?;
    copy_rows(tx, "versions", new_asset, &[("head", "NULL")])?;
    let new_version = "WHERE o.version_id IN (SELECT id FROM other.versions WHERE asset_id IN (SELECT id FROM temp.import_new))";
    let recipe_id = format!("o.id+{ro}");
    let recipe_parent = format!("o.parent+{ro}");
    // Parents before their children (the history's own references).
    let in_order = format!("{new_version} ORDER BY o.id");
    copy_rows(
        tx,
        "recipes",
        &in_order,
        &[("id", &recipe_id), ("parent", &recipe_parent)],
    )?;
    let snapshot_key = format!("o.key+{so}");
    copy_rows(tx, "snapshots", new_version, &[("key", &snapshot_key)])?;
    tx.execute_batch(&format!(
        "INSERT OR IGNORE INTO main.recipe_blobs(hash,data)
           SELECT b.hash,b.data FROM other.recipe_blobs b WHERE b.hash IN
             (SELECT hash FROM other.recipe_blob_refs WHERE recipe_id IN (SELECT o.id FROM other.recipes o {new_version})
              UNION SELECT hash FROM other.snapshot_blob_refs WHERE snapshot_key IN (SELECT o.key FROM other.snapshots o {new_version}));
         INSERT INTO main.recipe_blob_refs(recipe_id,hash)
           SELECT r.recipe_id+{ro},r.hash FROM other.recipe_blob_refs r WHERE r.recipe_id IN (SELECT o.id FROM other.recipes o {new_version});
         INSERT INTO main.snapshot_blob_refs(snapshot_key,hash)
           SELECT s.snapshot_key+{so},s.hash FROM other.snapshot_blob_refs s WHERE s.snapshot_key IN (SELECT o.key FROM other.snapshots o {new_version});
         UPDATE main.versions SET head=(SELECT o.head+{ro} FROM other.versions o WHERE o.id=main.versions.id)
           WHERE id IN (SELECT id FROM other.versions WHERE asset_id IN (SELECT id FROM temp.import_new));"
    ))?;
    copy_rows(tx, "masks", new_version, &[])?;
    for table in [
        "capture_overrides",
        "eye_reads",
        "face_eyes",
        "image_hashes",
    ] {
        copy_rows(tx, table, new_asset, &[])?;
    }
    tx.execute(
        "INSERT INTO import_batches(kind) VALUES(?)",
        [plan.batch_kind],
    )?;
    let batch = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO asset_imports(asset_id,batch_id) SELECT id,? FROM temp.import_new",
        [batch],
    )?;
    tx.execute_batch("DELETE FROM temp.import_new")?;
    Ok(())
}

/// The edit an unedited photograph stores when an import replaces an edit
/// with none: it opens with the defaults of a never-edited photograph (as
/// `with_library_metadata` shows one), and has an edit of its own, so an
/// editor sidecar beside it is never adopted over it.
const UNEDITED: &str = r#"{"schema_version":1,"process_version":"rapidraw-1.6.4","adjustments":null,"editor_state":{"version":1,"adjustments":null},"layers":[]}"#;

/// The incoming photograph's edit, virtual copies, metadata, capture-time
/// correction, eye calls and snapshots replace the photograph's here (only
/// what differs). The edit is a new step of its history, and the edit it
/// replaces is kept as a snapshot named `before` (the app's Snapshots
/// panel brings it back); copies only here stay.
fn replace(
    tx: &Connection,
    lib: &Library,
    other: &Library,
    incoming: &str,
    target: &str,
    changes: &[&str],
    before: &str,
) -> Result<()> {
    let version: String = tx.query_row(
        "SELECT id FROM versions WHERE asset_id=? AND is_primary=1",
        [target],
        |r| r.get(0),
    )?;
    let theirs = other.primary_version(incoming)?;
    if changes.contains(&"edit") {
        if let Some(adjustments) = lib
            .recipe(&version)?
            .and_then(|recipe| recipe.pointer("/editor_state/adjustments").cloned())
            .filter(Value::is_object)
        {
            crate::snapshots::insert(
                tx,
                &lib.directory,
                &version,
                &[crate::Snapshot::new(before, adjustments)?],
            )?;
        }
        let payload = match other.recipe(&theirs)? {
            Some(recipe) => serde_json::to_string(&recipe)?,
            None => UNEDITED.to_string(),
        };
        write_recipe(tx, &version, &payload)?;
    }
    if changes.contains(&"metadata") {
        write_metadata(tx, target, &other.metadata(incoming)?)?;
    }
    // Their virtual copies (state and snapshots); copies only here stay.
    let copies: Vec<(String, String, Option<String>)> = other
        .conn
        .prepare("SELECT id,name,legacy_state FROM versions WHERE asset_id=? AND is_primary=0")?
        .query_map([incoming], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if changes.contains(&"copies") || changes.contains(&"snapshots") {
        for (their_copy, name, state) in copies {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM versions WHERE asset_id=? AND name=? AND is_primary=0",
                    params![target, name],
                    |r| r.get(0),
                )
                .optional()?;
            let here = match existing {
                Some(here) => {
                    tx.execute(
                        "UPDATE versions SET legacy_state=? WHERE id=?",
                        params![state, here],
                    )?;
                    here
                }
                None => {
                    let here = uuid::Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO versions(id,asset_id,name,is_primary,legacy_state) VALUES(?,?,?,0,?)",
                        params![here, target, name, state],
                    )?;
                    here
                }
            };
            let snapshots = crate::snapshots::read(&other.conn, &lib.directory, &their_copy)?;
            crate::snapshots::upsert(tx, &lib.directory, &here, &snapshots)?;
        }
    }
    if changes.contains(&"snapshots") {
        let snapshots = crate::snapshots::read(&other.conn, &lib.directory, &theirs)?;
        crate::snapshots::upsert(tx, &lib.directory, &version, &snapshots)?;
    }
    let set_target = format!("'{}'", target.replace('\'', "''"));
    let theirs_only = format!("WHERE o.asset_id='{}'", incoming.replace('\'', "''"));
    if changes.contains(&"captureTime") {
        tx.execute("DELETE FROM capture_overrides WHERE asset_id=?", [target])?;
        copy_rows(
            tx,
            "capture_overrides",
            &theirs_only,
            &[("asset_id", &set_target)],
        )?;
    }
    if changes.contains(&"eyes") {
        tx.execute("DELETE FROM face_eyes WHERE asset_id=?", [target])?;
        tx.execute("DELETE FROM eye_reads WHERE asset_id=?", [target])?;
        copy_rows(tx, "eye_reads", &theirs_only, &[("asset_id", &set_target)])?;
        copy_rows(tx, "face_eyes", &theirs_only, &[("asset_id", &set_target)])?;
    }
    Ok(())
}

/// The incoming photograph's edit as a new virtual copy here; the
/// photograph's own edit and metadata stay.
fn as_virtual_copy(tx: &Connection, other: &Library, incoming: &str, target: &str) -> Result<()> {
    let theirs = other.primary_version(incoming)?;
    let state = other
        .recipe(&theirs)?
        .and_then(|recipe| recipe.get("editor_state").cloned())
        .unwrap_or_else(|| serde_json::json!({"version": 1, "adjustments": null}));
    let taken: BTreeSet<String> = tx
        .prepare("SELECT name FROM versions WHERE asset_id=? AND is_primary=0")?
        .query_map([target], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let name = loop {
        let candidate = uuid::Uuid::new_v4().simple().to_string()[..6].to_string();
        if valid_copy_id(&candidate) && !taken.contains(&candidate) {
            break candidate;
        }
    };
    tx.execute(
        "INSERT INTO versions(id,asset_id,name,is_primary,legacy_state) VALUES(?,?,?,0,?)",
        params![
            uuid::Uuid::new_v4().to_string(),
            target,
            name,
            serde_json::to_string(&state)?
        ],
    )?;
    Ok(())
}

/// The album here standing for album `id` of `other`, made (with the
/// groups above it) when there is none. Returns its id and how many albums
/// and groups were made.
fn ensure_album(
    tx: &Connection,
    lib: &Library,
    other: &Library,
    id: &str,
) -> Result<(String, usize)> {
    if let Some(found) = lib.matching_album(other, id)? {
        return Ok((found, 0));
    }
    let (name, kind, parent, position): (String, String, Option<String>, i64) =
        other.conn.query_row(
            "SELECT name,kind,parent,position FROM collections WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let (parent, made) = match parent {
        Some(parent) => {
            let (here, made) = ensure_album(tx, lib, other, &parent)?;
            (Some(here), made)
        }
        None => (None, 0),
    };
    // Its own id when free (a later import finds it by id), else a new one.
    let free = tx
        .query_row("SELECT 1 FROM collections WHERE id=?", [id], |_| Ok(()))
        .optional()?
        .is_none();
    let new_id = if free {
        id.to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    tx.execute(
        "INSERT INTO collections(id,name,parent,kind,position) VALUES(?,?,?,?,?)",
        params![new_id, name, parent, kind, position],
    )?;
    Ok((new_id, made + 1))
}
