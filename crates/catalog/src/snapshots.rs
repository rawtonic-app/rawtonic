//! Named snapshots of a version's development (G26, schema 12): the
//! adjustments of one moment under a name, kept until deleted, whatever the
//! version's history does meanwhile. The catalog keeps them in their own
//! table, never in the recipe history (a snapshot is no edit), and gives
//! them to the editor state it hands out (`Library::editor_state`,
//! `copy_state`), so the editor sidecar derived from it carries them
//! (`SIDECAR_KEY`); a library adopting that sidecar takes them over.
//!
//! In a sidecar each long string of the snapshots (a mask bitmap, an
//! analysis) is written once, under `STRINGS_KEY`, and referred to by its
//! SHA-256: twenty snapshots of a photograph with AI masks wrote 18.5 MB
//! per save with a copy each, 1.8 MB like this (`snapshot_timing`).
use crate::{BLOB_MIN_LEN, Library, load_blobs, prune_blobs, store_blobs, visit_strings};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

/// The editor sidecar key snapshots travel under.
pub const SIDECAR_KEY: &str = "snapshots";
/// The editor sidecar key their long strings are kept under, by SHA-256.
pub const STRINGS_KEY: &str = "snapshotStrings";
const STRING_PREFIX: &str = "rawtonic-snapshot-string:sha256:";
/// Longest snapshot name, in characters.
pub const MAX_NAME: usize = 200;
const MAX_ID: usize = 64;
const MAX_ADJUSTMENTS: usize = 64 * 1024 * 1024;

/// One snapshot: its id (kept wherever it travels), name, when it was taken
/// (RFC 3339, UTC) and the adjustments it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub adjustments: Value,
}

impl Snapshot {
    /// A new snapshot of `adjustments`, taken now.
    pub fn new(name: &str, adjustments: Value) -> Result<Self> {
        let snapshot = Self {
            id: Uuid::new_v4().to_string(),
            name: clean_name(name)?,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            adjustments,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty() && self.id.len() <= MAX_ID,
            "Not a snapshot id"
        );
        ensure!(clean_name(&self.name)? == self.name, "Not a snapshot name");
        ensure!(
            chrono::DateTime::parse_from_rfc3339(&self.created_at).is_ok(),
            "Not a snapshot time"
        );
        ensure!(self.adjustments.is_object(), "A snapshot holds adjustments");
        Ok(())
    }
}

/// `name` as a snapshot is named: trimmed, not empty, at most `MAX_NAME`
/// characters.
pub fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(!name.is_empty(), "A snapshot needs a name");
    ensure!(
        name.chars().count() <= MAX_NAME,
        "Snapshot names are at most {MAX_NAME} characters"
    );
    Ok(name.to_string())
}

/// The snapshots an editor state (a sidecar's) carries, in its order. An
/// entry that is not a valid snapshot, repeats an id or refers to a string
/// the sidecar lacks, is left out (and logged): it must not keep the
/// photograph's edit from being adopted.
pub fn in_state(state: &Value) -> Vec<Snapshot> {
    let mut seen = HashSet::new();
    let strings = state.get(STRINGS_KEY).and_then(Value::as_object);
    state
        .get(SIDECAR_KEY)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let mut entry = entry.clone();
            let mut missing = None;
            visit_strings(&mut entry, &mut |text| {
                if let Some(hash) = text.strip_prefix(STRING_PREFIX) {
                    match strings.and_then(|s| s.get(hash)).and_then(Value::as_str) {
                        Some(found) => *text = found.to_string(),
                        None => missing = Some(hash.to_string()),
                    }
                }
            });
            match missing
                .map_or(Ok(()), |hash| {
                    Err(anyhow::anyhow!("its string {hash} is missing"))
                })
                .and_then(|()| Ok(serde_json::from_value::<Snapshot>(entry)?))
                .and_then(|s| s.validate().map(|()| s))
            {
                Ok(snapshot) if seen.insert(snapshot.id.clone()) => Some(snapshot),
                Ok(snapshot) => {
                    log::warn!("Snapshot {} appears twice; kept once", snapshot.id);
                    None
                }
                Err(e) => {
                    log::warn!("Snapshot left out: {e:#}");
                    None
                }
            }
        })
        .collect()
}

/// `snapshots` as a sidecar holds them: the list (`SIDECAR_KEY`) and its
/// long strings, each once (`STRINGS_KEY`). None for no snapshots.
pub fn for_sidecar(snapshots: &[Snapshot]) -> Result<Option<(Value, Value)>> {
    if snapshots.is_empty() {
        return Ok(None);
    }
    let mut list = serde_json::to_value(snapshots)?;
    let mut strings = serde_json::Map::new();
    visit_strings(&mut list, &mut |text| {
        if text.len() >= BLOB_MIN_LEN {
            let hash: String = <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let full = std::mem::replace(text, format!("{STRING_PREFIX}{hash}"));
            strings.entry(hash).or_insert(Value::String(full));
        }
    });
    Ok(Some((list, Value::Object(strings))))
}

/// Gives an editor state `snapshots` as a sidecar holds them, in place of
/// any it carries.
pub fn set_in_state(state: &mut Value, snapshots: &[Snapshot]) -> Result<()> {
    let packed = for_sidecar(snapshots)?;
    if let Some(map) = state.as_object_mut() {
        map.remove(SIDECAR_KEY);
        map.remove(STRINGS_KEY);
        if let Some((list, strings)) = packed {
            map.insert(SIDECAR_KEY.into(), list);
            if strings.as_object().is_some_and(|s| !s.is_empty()) {
                map.insert(STRINGS_KEY.into(), strings);
            }
        }
    }
    Ok(())
}

/// Takes the snapshots (and their strings) out of an editor state.
pub(crate) fn strip(state: &mut Value) {
    if let Some(map) = state.as_object_mut() {
        map.remove(SIDECAR_KEY);
        map.remove(STRINGS_KEY);
    }
}

/// The snapshots of `version` in the order they were taken.
pub(crate) fn read(
    conn: &Connection,
    directory: &std::path::Path,
    version: &str,
) -> Result<Vec<Snapshot>> {
    let rows: Vec<(String, String, String, String)> = conn
        .prepare(
            "SELECT id,name,created_at,adjustments FROM snapshots WHERE version_id=? ORDER BY key",
        )?
        .query_map([version], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(id, name, created_at, text)| {
            let mut adjustments: Value = serde_json::from_str(&text)?;
            load_blobs(conn, &mut adjustments)?;
            rawtonic_metadata::resources::resolve(&mut adjustments, directory);
            Ok(Snapshot {
                id,
                name,
                created_at,
                adjustments,
            })
        })
        .collect()
}

/// Stores `snapshots` for `version` after those it has; one with an id the
/// version has already is left out.
pub(crate) fn insert(
    tx: &Connection,
    directory: &std::path::Path,
    version: &str,
    snapshots: &[Snapshot],
) -> Result<()> {
    for snapshot in snapshots {
        snapshot.validate()?;
        let mut adjustments = snapshot.adjustments.clone();
        rawtonic_metadata::resources::localize(&mut adjustments, directory);
        let hashes = store_blobs(tx, &mut adjustments)?;
        let text = serde_json::to_string(&adjustments)?;
        ensure!(text.len() <= MAX_ADJUSTMENTS, "Snapshot too large");
        if tx.execute(
            "INSERT OR IGNORE INTO snapshots(id,version_id,name,created_at,adjustments) VALUES(?,?,?,?,?)",
            params![snapshot.id, version, snapshot.name, snapshot.created_at, text],
        )? == 0
        {
            continue;
        }
        let key = tx.last_insert_rowid();
        for hash in hashes {
            tx.execute(
                "INSERT OR IGNORE INTO snapshot_blob_refs(snapshot_key,hash) VALUES(?,?)",
                params![key, hash],
            )?;
        }
    }
    Ok(())
}

/// Whether `version` has any snapshot.
pub(crate) fn any(tx: &Connection, version: &str) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM snapshots WHERE version_id=? LIMIT 1",
            [version],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Takes a snapshot into `list`, the snapshots an editor sidecar keeps (a
/// photograph in no library), as `Library::add_snapshot` does.
pub fn add_to(list: &mut Vec<Snapshot>, name: &str, adjustments: Value) -> Result<Snapshot> {
    let snapshot = Snapshot::new(name, adjustments)?;
    list.push(snapshot.clone());
    Ok(snapshot)
}

/// Renames the snapshot `id` of `list` (`Library::rename_snapshot`).
pub fn rename_in(list: &mut [Snapshot], id: &str, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    let snapshot = list
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow::anyhow!("Unknown snapshot"))?;
    snapshot.name = name;
    Ok(())
}

/// Gives the snapshot `id` of `list` new adjustments (`Library::update_snapshot`).
pub fn update_in(list: &mut [Snapshot], id: &str, adjustments: Value) -> Result<()> {
    let snapshot = list
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow::anyhow!("Unknown snapshot"))?;
    let updated = Snapshot {
        adjustments,
        ..snapshot.clone()
    };
    updated.validate()?;
    *snapshot = updated;
    Ok(())
}

/// Deletes the snapshot `id` of `list` (`Library::delete_snapshot`).
pub fn delete_from(list: &mut Vec<Snapshot>, id: &str) -> Result<()> {
    let before = list.len();
    list.retain(|s| s.id != id);
    ensure!(list.len() < before, "Unknown snapshot");
    Ok(())
}

impl Library {
    /// The version a photograph's edit is: its primary one, or the virtual
    /// copy `copy` (None when that copy is not recorded).
    pub fn version_for(&self, asset: &str, copy: Option<&str>) -> Result<Option<String>> {
        match copy {
            None => self.primary_version(asset).map(Some),
            Some(copy) => Ok(self
                .conn
                .query_row(
                    "SELECT id FROM versions WHERE asset_id=? AND name=? AND is_primary=0",
                    params![asset, copy],
                    |r| r.get(0),
                )
                .optional()?),
        }
    }

    /// The snapshots of `version`, in the order they were taken.
    pub fn snapshots(&self, version: &str) -> Result<Vec<Snapshot>> {
        read(&self.conn, &self.directory, version)
    }

    /// Takes a snapshot of `adjustments` for `version` under `name`.
    pub fn add_snapshot(&self, version: &str, name: &str, adjustments: Value) -> Result<Snapshot> {
        let snapshot = Snapshot::new(name, adjustments)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        ensure!(
            tx.query_row("SELECT count(*) FROM versions WHERE id=?", [version], |r| r
                .get::<_, i64>(0))?
                == 1,
            "Unknown version"
        );
        insert(
            &tx,
            &self.directory,
            version,
            std::slice::from_ref(&snapshot),
        )?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Takes over the `snapshots` a sidecar carries when `version` has none
    /// (as adopting a sidecar does); returns whether it did.
    pub fn adopt_snapshots(&self, version: &str, snapshots: &[Snapshot]) -> Result<bool> {
        if snapshots.is_empty() {
            return Ok(false);
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if any(&tx, version)? {
            return Ok(false);
        }
        insert(&tx, &self.directory, version, snapshots)?;
        tx.commit()?;
        Ok(true)
    }

    /// Renames the snapshot `id` of `version`.
    pub fn rename_snapshot(&self, version: &str, id: &str, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        ensure!(
            self.conn.execute(
                "UPDATE snapshots SET name=? WHERE version_id=? AND id=?",
                params![name, version, id],
            )? == 1,
            "Unknown snapshot"
        );
        Ok(())
    }

    /// Gives the snapshot `id` of `version` new adjustments (Lightroom's
    /// Update with Current Settings); its name, id and time stay.
    pub fn update_snapshot(&self, version: &str, id: &str, adjustments: Value) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut snapshot = read(&tx, &self.directory, version)?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("Unknown snapshot"))?;
        snapshot.adjustments = adjustments;
        snapshot.validate()?;
        let mut stored = snapshot.adjustments.clone();
        rawtonic_metadata::resources::localize(&mut stored, &self.directory);
        let hashes = store_blobs(&tx, &mut stored)?;
        let text = serde_json::to_string(&stored)?;
        ensure!(text.len() <= MAX_ADJUSTMENTS, "Snapshot too large");
        let key: i64 = tx.query_row(
            "SELECT key FROM snapshots WHERE version_id=? AND id=?",
            params![version, id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE snapshots SET adjustments=? WHERE key=?",
            params![text, key],
        )?;
        tx.execute("DELETE FROM snapshot_blob_refs WHERE snapshot_key=?", [key])?;
        for hash in hashes {
            tx.execute(
                "INSERT OR IGNORE INTO snapshot_blob_refs(snapshot_key,hash) VALUES(?,?)",
                params![key, hash],
            )?;
        }
        prune_blobs(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes the snapshot `id` of `version` (and the stored strings only
    /// it used).
    pub fn delete_snapshot(&self, version: &str, id: &str) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        ensure!(
            tx.execute(
                "DELETE FROM snapshots WHERE version_id=? AND id=?",
                params![version, id],
            )? == 1,
            "Unknown snapshot"
        );
        prune_blobs(&tx)?;
        tx.commit()?;
        Ok(())
    }
}
