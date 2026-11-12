//! XMP sidecars the library has not brought up to date, and their review:
//! *pending* ones (the library is newer and its XMP is not written yet — the
//! photograph is offline, the sidecar read-only, a write failed) and
//! *conflicts* (the sidecar changed outside Rawtonic since the library last
//! wrote or read it). A conflict is resolved either way: the library's
//! metadata replaces the sidecar's (`keep_library_xmp`) or the sidecar's is
//! read into the library (`use_xmp`, undone with `undo_use_xmp`). Only the
//! sidecar is ever written, through `rawtonic_metadata::write_xmp` (atomic,
//! unowned XMP kept); the photograph itself is never touched.

use crate::CatalogError;
use crate::error::{bail, ensure};
use crate::{Library, XmpProblem, search_text};
use anyhow::Result;
use rawtonic_metadata::{Metadata, MetadataError};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A photograph whose XMP sidecar does not hold what the library holds.
#[derive(Debug, Clone, Serialize)]
pub struct XmpEntry {
    pub id: String,
    pub path: PathBuf,
    /// Why it is not written (`Library::xmp_problem`); None: it is pending
    /// and the next write (Try again) brings it up to date.
    pub problem: Option<XmpProblem>,
    /// The disconnected drive an offline photograph is on, when known.
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
    /// The corrected capture time before and after (the sidecar's
    /// `exif:DateTimeOriginal` may set or remove it), and the value of the
    /// library's the sidecar held before (`metadata.xmp_capture`).
    #[serde(default)]
    pub capture_before: Option<crate::capture_time::CaptureTime>,
    #[serde(default)]
    pub capture_after: Option<crate::capture_time::CaptureTime>,
    #[serde(default)]
    pub xmp_capture: Option<String>,
}

impl Library {
    /// The photographs `Summary::xmp_pending` counts, in path order, with
    /// why each is not written (the same check as `write_pending_xmp`).
    pub fn xmp_unsynced(&self) -> Result<Vec<XmpEntry>, CatalogError> {
        let rows = self.offline_rows(
            &format!(
                "a.id IN (SELECT asset_id FROM metadata WHERE xmp_status!='synced') AND {}",
                self.visible_assets()
            ),
            &[],
        )?;
        let mut entries = rows
            .into_iter()
            .map(|row| {
                let problem = self.xmp_problem(&row.id, &row.path)?;
                Ok(XmpEntry {
                    volume: row.volume.filter(|_| problem == Some(XmpProblem::Offline)),
                    id: row.id,
                    path: row.path,
                    problem,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// Resolves a conflict for the library: its metadata and recipe replace
    /// the sidecar's owned properties. IPTC keys absent from the library
    /// stay in the sidecar; only explicit empty values clear them. Refused
    /// while the photograph is offline; a sidecar changed again meanwhile
    /// is a conflict again, not overwritten.
    pub fn keep_library_xmp(&self, asset: &str) -> Result<(), CatalogError> {
        let path = self.writable_sidecar(asset)?;
        // The edit the photograph shows is what the sidecar gets.
        self.adopt_editor_sidecar(asset)?;
        let current = match fs::read(&path) {
            Ok(bytes) => Some(rawtonic_fs::sha256_hex(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(self.write_sidecar(asset, &path, current.as_deref())?)
    }

    /// Resolves a conflict for the sidecar: the metadata Rawtonic reads
    /// from XMP (rating, label, flag, keywords, title, caption, creator,
    /// copyright, IPTC) replaces the library's; the edit (recipe) stays the
    /// library's, which never reads it back. A sidecar without a flag keeps
    /// the library's flag. The photograph is then pending: `sync_xmp`
    /// writes the library's recipe into the sidecar it has just read (a
    /// sidecar changed again meanwhile is a conflict again).
    pub fn use_xmp(&self, asset: &str) -> Result<XmpUndo, CatalogError> {
        let path = self.writable_sidecar(asset)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("The XMP sidecar is gone; keep the library's metadata to write it again")
            }
            Err(e) => return Err(e.into()),
        };
        let (mut metadata, _) = rawtonic_metadata::read_xmp(&text)?;
        let stated = rawtonic_metadata::read_capture_time(&text)?;
        let carries_flag = rawtonic_metadata::carries_flag(&text)?;
        // Only a packet the sidecar writer can take back (an XMP packet with
        // an RDF root) replaces the library's metadata: well-formed XML that
        // is no XMP would read as "everything empty".
        rawtonic_metadata::merge_xmp(Some(&text), &metadata, None)?;
        // Metadata never changes the look: an edit only in the editor
        // sidecar is the library's before the sidecar's metadata replaces
        // its own (the undo then goes back to what the photograph showed).
        self.adopt_editor_sidecar(asset)?;
        // Read and replaced under one write lock: an edit another
        // connection commits meanwhile is either before (and kept in the
        // undo) or after, never overwritten from a stale read.
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let (before, xmp_hash, xmp_status, from_camera, xmp_capture): (
            String,
            Option<String>,
            String,
            bool,
            Option<String>,
        ) = tx.query_row(
            "SELECT data,xmp_hash,xmp_status,from_camera,xmp_capture FROM metadata WHERE asset_id=?",
            [asset],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let capture_before = crate::capture_time::override_of(&tx, asset)?;
        if !carries_flag {
            metadata.flag = serde_json::from_str::<Metadata>(&before)?.flag;
        }
        metadata.validate()?;
        crate::write_metadata(&tx, asset, &metadata)?;
        tx.execute(
            "UPDATE metadata SET xmp_hash=?,xmp_publishing=NULL WHERE asset_id=?",
            params![rawtonic_fs::sha256_hex(text.as_bytes()), asset],
        )?;
        // The sidecar's capture time too: with the existing rule (the
        // photographer chose the sidecar), a different one replaces the
        // library's correction.
        crate::capture_time::adopt_sidecar(&tx, asset, stated.as_ref())?;
        let capture_after = crate::capture_time::override_of(&tx, asset)?;
        tx.commit()?;
        Ok(XmpUndo {
            capture_before,
            capture_after,
            xmp_capture,
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
    pub fn undo_use_xmp(&self, undo: &XmpUndo) -> Result<(), CatalogError> {
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
        // The correction as it was, unless it was changed since.
        if undo.capture_before != undo.capture_after
            && crate::capture_time::override_of(&tx, &undo.asset)? == undo.capture_after
        {
            match &undo.capture_before {
                Some(time) => tx.execute(
                    "INSERT INTO capture_overrides(asset_id,captured,subsec,offset) VALUES(?,?,?,?)
                     ON CONFLICT(asset_id) DO UPDATE SET captured=excluded.captured,
                        subsec=excluded.subsec,offset=excluded.offset",
                    params![undo.asset, time.local, time.subsec, time.offset],
                )?,
                None => tx.execute(
                    "DELETE FROM capture_overrides WHERE asset_id=?",
                    [&undo.asset],
                )?,
            };
            tx.execute(
                "UPDATE metadata SET xmp_capture=? WHERE asset_id=?",
                params![undo.xmp_capture, undo.asset],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The sidecar path of a photograph that is there and owns it alone.
    pub(crate) fn writable_sidecar(&self, asset: &str) -> Result<PathBuf> {
        let a = self.asset(asset)?;
        // Never write beside a missing file: the XMP follows when it is back.
        if !a.path.is_file() {
            return Err(crate::CatalogError::XmpOffline.into());
        }
        // RAW+JPEG siblings would share a stem; refuse ambiguity rather than clobber.
        ensure!(
            self.xmp_owners(&a.path)? == 1,
            "RAW/JPEG sidecar ownership needs pair resolution"
        );
        Ok(rawtonic_metadata::sidecar_path(&a.path))
    }

    /// Writes the library's metadata and recipe over a sidecar whose digest
    /// is `expected` (None: there is none). Absent IPTC keys are preserved;
    /// stored values and explicit clears win. A changed sidecar marks a
    /// conflict; other failures (read-only, a full disk) leave the state as
    /// it was, so the photograph stays pending.
    pub(crate) fn write_sidecar(
        &self,
        asset: &str,
        path: &Path,
        expected: Option<&str>,
    ) -> Result<()> {
        let recipe = self.recipe(&self.primary_version(asset)?)?;
        let data: String =
            self.conn
                .query_row("SELECT data FROM metadata WHERE asset_id=?", [asset], |r| {
                    r.get(0)
                })?;
        let mut metadata: Metadata = serde_json::from_str(&data)?;
        let (capture, owned) = crate::capture_time::capture_write(&self.conn, asset)?;
        metadata.capture = capture;
        // The digest to be published is committed first (`xmp_publishing`,
        // its own statement outside any transaction), so a crash between
        // the publish and the commit of `xmp_hash` leaves a sidecar the next
        // check recognises as this write, not as another program's change.
        debug_assert!(
            self.conn.is_autocommit(),
            "the announced digest must be durable"
        );
        let written = rawtonic_metadata::write_xmp_announced(
            path,
            &metadata,
            recipe.as_ref(),
            expected,
            |hash| {
                self.conn.execute(
                    "UPDATE metadata SET xmp_publishing=? WHERE asset_id=?",
                    params![hash, asset],
                )?;
                Ok(())
            },
        );
        #[cfg(any(test, feature = "test-support"))]
        if written.is_ok()
            && self
                .crash_after_publish
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            bail!("test: stopped between the XMP publish and its digest's commit");
        }
        match written {
            Ok(hash) => {
                // In sync only with the metadata written: an edit committed
                // meanwhile (another connection) keeps the photograph pending.
                self.conn.execute(
                    "UPDATE metadata SET xmp_hash=?,xmp_capture=?,
                     xmp_publishing=CASE WHEN xmp_publishing=? THEN NULL ELSE xmp_publishing END,
                     xmp_status=CASE WHEN data=? THEN 'synced' ELSE xmp_status END
                     WHERE asset_id=?",
                    params![hash, owned, hash, data, asset],
                )?;
                Ok(())
            }
            Err(e) => {
                if matches!(e, MetadataError::XmpConflict) {
                    self.mark_xmp_conflict(asset)?;
                }
                Err(e.into())
            }
        }
    }

    /// Takes over a publish a crash cut off: when the sidecar on disk is
    /// the one `write_sidecar` announced (`xmp_publishing`) but never
    /// committed, its digest becomes the library's (`xmp_hash`). Any other
    /// sidecar is left to the usual check (a change elsewhere stays one).
    pub(crate) fn settle_xmp_publishing(&self, asset: &str, sidecar: &Path) -> Result<()> {
        let publishing: Option<String> = self.conn.query_row(
            "SELECT xmp_publishing FROM metadata WHERE asset_id=?",
            [asset],
            |r| r.get(0),
        )?;
        let Some(publishing) = publishing else {
            return Ok(());
        };
        // Unreadable or missing: nothing to take over.
        let Ok(bytes) = fs::read(sidecar) else {
            return Ok(());
        };
        if rawtonic_fs::sha256_hex(&bytes) == publishing {
            self.conn.execute(
                "UPDATE metadata SET xmp_hash=xmp_publishing, xmp_publishing=NULL
                 WHERE asset_id=? AND xmp_publishing=?",
                params![asset, publishing],
            )?;
        }
        Ok(())
    }

    pub(crate) fn mark_xmp_conflict(&self, asset: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE metadata SET xmp_status='conflict' WHERE asset_id=?",
            [asset],
        )?;
        Ok(())
    }
}
