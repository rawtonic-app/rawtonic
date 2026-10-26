//! XMP sidecars the library has not brought up to date, and their review:
//! *pending* ones (the library is newer and its XMP is not written yet — the
//! photograph is offline, the sidecar read-only, a write failed) and
//! *conflicts* (the sidecar changed outside Rawtonic since the library last
//! wrote or read it). A conflict is resolved either way: the library's
//! metadata replaces the sidecar's (`keep_library_xmp`) or the sidecar's is
//! read into the library (`use_xmp`, undone with `undo_use_xmp`). Only the
//! sidecar is ever written, through `rawtonic_metadata::write_xmp` (atomic,
//! unowned XMP kept); the photograph itself is never touched.

use crate::{Library, search_text};
use anyhow::{Result, bail, ensure};
use rawtonic_metadata::{Metadata, XmpConflict};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum XmpState {
    /// The library is newer than the sidecar; the next write brings it up to date.
    Pending,
    /// The sidecar changed outside Rawtonic; it is not replaced until reviewed.
    Conflict,
}

/// A photograph whose XMP sidecar is not in sync with the library.
#[derive(Debug, Clone, Serialize)]
pub struct XmpEntry {
    pub id: String,
    pub path: PathBuf,
    pub state: XmpState,
    /// The photograph's file is not there (missing, drive disconnected):
    /// nothing is written beside it until it is back.
    pub offline: bool,
    /// The disconnected drive it is on, when known.
    pub volume: Option<String>,
}

/// What `use_xmp` changed, so `undo_use_xmp` can put it back: the library's
/// metadata row as it was (and the metadata it set, to refuse an undo over
/// later edits).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct XmpUndo {
    pub asset: String,
    pub before: String,
    pub after: String,
    pub xmp_hash: Option<String>,
    pub xmp_status: String,
    pub from_camera: bool,
}

/// The error of a write refused because the photograph is offline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmpOffline;

impl std::fmt::Display for XmpOffline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The photograph is offline; its XMP is written when it is back")
    }
}

impl std::error::Error for XmpOffline {}

impl Library {
    /// The visible photographs whose XMP is pending or in conflict, in path order.
    pub fn xmp_unsynced(&self) -> Result<Vec<XmpEntry>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id,m.xmp_status FROM assets a JOIN metadata m ON m.asset_id=a.id
             WHERE m.xmp_status!='synced' AND {}",
            self.visible_assets()
        ))?;
        let states: HashMap<String, XmpState> = query
            .query_map([], |row| {
                let status: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    if status == "conflict" {
                        XmpState::Conflict
                    } else {
                        XmpState::Pending
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.offline_rows(
            &format!(
                "a.id IN (SELECT asset_id FROM metadata WHERE xmp_status!='synced') AND {}",
                self.visible_assets()
            ),
            &[],
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let state = *states.get(&row.id)?;
                let offline = !row.path.is_file();
                Some(XmpEntry {
                    id: row.id,
                    offline,
                    volume: row.volume.filter(|_| offline),
                    path: row.path,
                    state,
                })
            })
            .collect())
    }

    /// Resolves a conflict for the library: its metadata and recipe replace
    /// the sidecar's owned properties (everything else in the sidecar is
    /// kept). Refused while the photograph is offline; a sidecar changed
    /// again meanwhile is a conflict again, not overwritten.
    pub fn keep_library_xmp(&self, asset: &str) -> Result<()> {
        let path = self.writable_sidecar(asset)?;
        let current = match fs::read(&path) {
            Ok(bytes) => Some(rawtonic_metadata::digest(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        self.write_sidecar(asset, &path, current.as_deref(), true)
    }

    /// Resolves a conflict for the sidecar: the metadata Rawtonic reads
    /// from XMP (rating, label, flag, keywords, title, caption, creator,
    /// copyright, IPTC) replaces the library's; the edit (recipe) stays the
    /// library's, which never reads it back. A sidecar without a flag keeps
    /// the library's flag. The photograph is then pending: `sync_xmp`
    /// writes the library's recipe into the sidecar it has just read (a
    /// sidecar changed again meanwhile is a conflict again).
    pub fn use_xmp(&self, asset: &str) -> Result<XmpUndo> {
        let path = self.writable_sidecar(asset)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("The XMP sidecar is gone; keep the library's metadata to write it again")
            }
            Err(e) => return Err(e.into()),
        };
        let (mut metadata, _) = rawtonic_metadata::read_xmp(&text)?;
        let carries_flag = rawtonic_metadata::carries_flag(&text)?;
        // Read and replaced under one write lock: an edit another
        // connection commits meanwhile is either before (and kept in the
        // undo) or after, never overwritten from a stale read.
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let (before, xmp_hash, xmp_status, from_camera): (String, Option<String>, String, bool) =
            tx.query_row(
                "SELECT data,xmp_hash,xmp_status,from_camera FROM metadata WHERE asset_id=?",
                [asset],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        if !carries_flag {
            metadata.flag = serde_json::from_str::<Metadata>(&before)?.flag;
        }
        metadata.validate()?;
        crate::write_metadata(&tx, asset, &metadata)?;
        tx.execute(
            "UPDATE metadata SET xmp_hash=? WHERE asset_id=?",
            params![rawtonic_metadata::digest(text.as_bytes()), asset],
        )?;
        tx.commit()?;
        Ok(XmpUndo {
            asset: asset.to_string(),
            before,
            after: serde_json::to_string(&metadata)?,
            xmp_hash,
            xmp_status,
            from_camera,
        })
    }

    /// Puts back the library's metadata as it was before `use_xmp`, with
    /// its XMP state (a conflict is a conflict again). Refused when the
    /// metadata was changed since.
    pub fn undo_use_xmp(&self, undo: &XmpUndo) -> Result<()> {
        ensure!(
            ["pending", "conflict", "synced"].contains(&undo.xmp_status.as_str()),
            "Unknown XMP state"
        );
        let before: Metadata = serde_json::from_str(&undo.before)?;
        before.validate()?;
        // Only over the metadata `use_xmp` wrote (`write_metadata` stores
        // exactly this serialization): checked and restored in one
        // statement, so an edit committed meanwhile by another connection
        // is never overwritten.
        let tx = self.conn.unchecked_transaction()?;
        let restored = tx.execute(
            "UPDATE metadata SET rating=?,label=?,flag=?,data=?,xmp_hash=?,xmp_status=?,from_camera=?
             WHERE asset_id=? AND data=?",
            params![
                before.rating,
                before.label,
                before.flag,
                undo.before,
                undo.xmp_hash,
                undo.xmp_status,
                undo.from_camera,
                undo.asset,
                undo.after
            ],
        )?;
        ensure!(
            restored == 1,
            "The photograph's metadata was changed since; it is left as it is"
        );
        tx.execute(
            "UPDATE assets SET search_text=? WHERE id=?",
            params![search_text(&before), undo.asset],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The sidecar path of a photograph that is there and owns it alone.
    pub(crate) fn writable_sidecar(&self, asset: &str) -> Result<PathBuf> {
        let a = self.asset(asset)?;
        // Never write beside a missing file: the XMP follows when it is back.
        if !a.path.is_file() {
            return Err(XmpOffline.into());
        }
        // RAW+JPEG siblings would share a stem; refuse ambiguity rather than clobber.
        ensure!(
            self.xmp_owners(&a.path)? == 1,
            "RAW/JPEG sidecar ownership needs pair resolution"
        );
        Ok(a.path.with_extension("xmp"))
    }

    /// Writes the library's metadata and recipe over a sidecar whose digest
    /// is `expected` (None: there is none); `replace_iptc` as
    /// `rawtonic_metadata::write_xmp_replacing_iptc`. A changed sidecar marks a
    /// conflict; other failures (read-only, a full disk) leave the state as
    /// it was, so the photograph stays pending.
    pub(crate) fn write_sidecar(
        &self,
        asset: &str,
        path: &std::path::Path,
        expected: Option<&str>,
        replace_iptc: bool,
    ) -> Result<()> {
        let recipe = self.recipe(&self.primary_version(asset)?)?;
        let data: String =
            self.conn
                .query_row("SELECT data FROM metadata WHERE asset_id=?", [asset], |r| {
                    r.get(0)
                })?;
        let metadata: Metadata = serde_json::from_str(&data)?;
        let written = if replace_iptc {
            rawtonic_metadata::write_xmp_replacing_iptc(path, &metadata, recipe.as_ref(), expected)
        } else {
            rawtonic_metadata::write_xmp(path, &metadata, recipe.as_ref(), expected)
        };
        match written {
            Ok(hash) => {
                // In sync only with the metadata written: an edit committed
                // meanwhile (another connection) keeps the photograph pending.
                self.conn.execute(
                    "UPDATE metadata SET xmp_hash=?,
                     xmp_status=CASE WHEN data=? THEN 'synced' ELSE xmp_status END
                     WHERE asset_id=?",
                    params![hash, data, asset],
                )?;
                Ok(())
            }
            Err(e) => {
                if e.is::<XmpConflict>() {
                    self.mark_xmp_conflict(asset)?;
                }
                Err(e)
            }
        }
    }

    pub(crate) fn mark_xmp_conflict(&self, asset: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE metadata SET xmp_status='conflict' WHERE asset_id=?",
            [asset],
        )?;
        Ok(())
    }
}
