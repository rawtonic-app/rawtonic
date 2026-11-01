//! Photographs whose XMP sidecar does not hold what the library holds yet
//! (`Summary::xmp_pending`): which ones, and writing them again on request.
//! The library's own edits are safe either way; only the sidecar lags.

use crate::Library;
use anyhow::Result;
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Why a photograph's XMP sidecar was not written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum XmpProblem {
    /// The file is missing, moved away or on a disconnected drive; written
    /// when it is back.
    Offline,
    /// Another photograph of the same name (a RAW and its JPEG) would share
    /// the sidecar.
    SharedName,
    /// The sidecar was changed by another program, or was there before and
    /// never read: it is not replaced.
    ChangedElsewhere,
    /// Writing failed (permissions, a full or read-only drive).
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsavedXmp {
    pub path: PathBuf,
    pub problem: XmpProblem,
}

/// What `Library::write_pending_xmp` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmpRetry {
    pub written: usize,
    pub unsaved: Vec<UnsavedXmp>,
}

impl Library {
    fn pending_xmp(&self) -> Result<Vec<String>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id FROM assets a JOIN metadata m ON m.asset_id=a.id
             WHERE m.xmp_status!='synced' AND {} ORDER BY a.rel_path",
            self.visible_assets()
        ))?;
        let rows = query.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The photographs `Summary::xmp_pending` counts.
    pub fn pending_xmp_paths(&self) -> Result<Vec<PathBuf>> {
        self.pending_xmp()?
            .into_iter()
            .map(|id| Ok(self.asset(&id)?.path))
            .collect()
    }

    /// Writes the XMP sidecar of every photograph `Summary::xmp_pending`
    /// counts, as an edit would (`sync_xmp`): a sidecar changed elsewhere is
    /// never replaced and an offline photograph is not tried. Only sidecars
    /// are written, never the photographs.
    pub fn write_pending_xmp(&self) -> Result<XmpRetry> {
        self.write_pending_xmp_of(None)
    }

    /// `write_pending_xmp` for those of `only` still pending (all of them
    /// with None): the open-time write of the XMP a quit or crash left
    /// behind goes in parts, each after the photographs' earlier writes.
    pub fn write_pending_xmp_of(&self, only: Option<&[PathBuf]>) -> Result<XmpRetry> {
        let pending = self.pending_xmp()?;
        // A part: its own photographs, looked up directly (not every pending
        // one loaded for each part).
        let ids = match only {
            None => pending,
            Some(paths) => {
                let pending: HashSet<String> = pending.into_iter().collect();
                let mut ids = Vec::new();
                for path in paths {
                    if let Some(asset) = self.asset_at(path)?
                        && pending.contains(&asset.id)
                        && !ids.contains(&asset.id)
                    {
                        ids.push(asset.id);
                    }
                }
                ids
            }
        };
        let mut retry = XmpRetry::default();
        for id in ids {
            let path = self.asset(&id)?.path;
            let problem = match self.xmp_problem(&id, &path)? {
                // `sync_xmp` refuses a changed sidecar too; it is not even read twice.
                Some(problem) => Some(problem),
                None => self
                    .sync_xmp(&id)
                    .err()
                    .map(|e| XmpProblem::Failed(e.to_string())),
            };
            match problem {
                None => retry.written += 1,
                Some(problem) => retry.unsaved.push(UnsavedXmp { path, problem }),
            }
        }
        Ok(retry)
    }

    /// Why the pending XMP of a photograph cannot be written now (None: it
    /// can be tried). Whether the file is there now, not the last check's
    /// flag: a photograph moved away since gets no sidecar left behind, one
    /// that is back gets its own.
    pub(crate) fn xmp_problem(&self, id: &str, path: &Path) -> Result<Option<XmpProblem>> {
        Ok(if !path.is_file() {
            Some(XmpProblem::Offline)
        } else if self.xmp_owners(path)? > 1 {
            Some(XmpProblem::SharedName)
        } else if self.sidecar_changed_elsewhere(id, path)? {
            Some(XmpProblem::ChangedElsewhere)
        } else {
            None
        })
    }

    /// Whether a sidecar is there that the library did not write or read last
    /// (or something else that is not a file has its name), or the one it
    /// wrote or read is gone: `sync_xmp` replaces neither (Keep library does).
    fn sidecar_changed_elsewhere(&self, id: &str, path: &Path) -> Result<bool> {
        let sidecar = rawtonic_metadata::sidecar_path(path);
        if sidecar.exists() && !sidecar.is_file() {
            return Ok(true);
        }
        let (hash, publishing): (Option<String>, Option<String>) = self.conn.query_row(
            "SELECT xmp_hash,xmp_publishing FROM metadata WHERE asset_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        match std::fs::read(sidecar) {
            // The one it wrote last, or the one it was publishing when a
            // crash cut the write short (taken over by `sync_xmp`).
            Ok(bytes) => {
                let here = Some(rawtonic_metadata::digest(&bytes));
                Ok(here != hash && here != publishing)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(hash.is_some()),
            // Unreadable: writing it is tried and fails with why.
            Err(_) => Ok(false),
        }
    }
}

/// How each photograph's sidecar looked when `Library::check_xmp_sidecars`
/// last found it holding what the library wrote or read (size, modification
/// time and that digest), by asset id: a sidecar that still looks so is not
/// read again. Kept by the caller between checks; empty, every sidecar is
/// read once.
#[derive(Debug, Default)]
pub struct SidecarStamps(HashMap<String, Stamp>);

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    digest: String,
}

/// What `Library::check_xmp_sidecars` found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct XmpCheck {
    /// Photographs in sync whose sidecar was looked at.
    pub checked: usize,
    /// Of those, the sidecars read (new or changed since the last check).
    pub read: usize,
    /// In sync until now, but their sidecar was changed, deleted or written
    /// by another program (`XmpProblem::ChangedElsewhere`), by asset id.
    pub changed: Vec<String>,
}

impl Library {
    /// Looks for sidecars another program changed since the library wrote
    /// or read them, among the photographs whose XMP is in sync (the others
    /// are listed already), in `folder` and below or everywhere. Reads only:
    /// what it finds is marked by `mark_changed_elsewhere`, so the check can
    /// run without holding up the library's writes. A sidecar is read only
    /// when it is new or its size or modification time changed (`seen`).
    pub fn check_xmp_sidecars(
        &self,
        folder: Option<&Path>,
        seen: &mut SidecarStamps,
    ) -> Result<XmpCheck> {
        let folder = folder.map(|f| f.canonicalize().unwrap_or_else(|_| f.to_path_buf()));
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id,r.path,r.external,a.rel_path,m.xmp_hash,m.xmp_publishing
             FROM assets a JOIN roots r ON r.id=a.root_id JOIN metadata m ON m.asset_id=a.id
             WHERE m.xmp_status='synced' AND {} ORDER BY a.rel_path",
            self.visible_assets()
        ))?;
        let rows = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    self.join_root(row.get(1)?, row.get(2)?, row.get(3)?),
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut check = XmpCheck::default();
        for (id, path, hash, publishing) in rows {
            if folder.as_ref().is_some_and(|f| !path.starts_with(f)) {
                continue;
            }
            check.checked += 1;
            let sidecar = rawtonic_metadata::sidecar_path(&path);
            let changed = match fs::metadata(&sidecar) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.is_some(),
                // Unreadable (permissions, a drive going away): not a change.
                Err(_) => false,
                Ok(meta) if !meta.is_file() => true,
                Ok(meta) => {
                    let stamp = |digest: String| Stamp {
                        len: meta.len(),
                        modified: meta.modified().ok(),
                        digest,
                    };
                    let known = hash.clone().map(stamp);
                    if known.is_some() && seen.0.get(&id) == known.as_ref() {
                        false
                    } else {
                        check.read += 1;
                        match fs::read(&sidecar) {
                            Ok(bytes) => {
                                let digest = rawtonic_metadata::digest(&bytes);
                                if hash.as_deref() == Some(digest.as_str()) {
                                    seen.0.insert(id.clone(), stamp(digest));
                                    false
                                } else {
                                    // A publish a crash cut off is the
                                    // library's own write, not a change.
                                    publishing.as_deref() != Some(digest.as_str())
                                }
                            }
                            Err(_) => false,
                        }
                    }
                }
            };
            // A RAW and its JPEG share a sidecar the library never writes.
            if changed && self.xmp_owners(&path)? <= 1 {
                seen.0.remove(&id);
                check.changed.push(id);
            }
        }
        Ok(check)
    }

    /// Runs inside `mark_changed_elsewhere`'s transaction right after a
    /// mark, before the next photograph is looked at: nothing to fail in the
    /// app, nothing unless a test armed a step (`Library::fail_when_marking`).
    #[cfg(any(test, feature = "test-support"))]
    fn step_marked(&self) -> Result<()> {
        let step = self.markings.lock().unwrap_or_else(|e| e.into_inner());
        match *step {
            None => Ok(()),
            Some(ref step) => step(),
        }
    }

    /// Marks the photographs `check_xmp_sidecars` found as changed elsewhere,
    /// so the session bar counts them and its panel offers Keep library and
    /// Use XMP. Each is looked at again first (`xmp_problem`, as the panel
    /// lists it): one whose sidecar is back as the library left it, that went
    /// offline, or that an edit made pending meanwhile is left as it is.
    /// Writes only the catalog, in one transaction: a failure part-way keeps
    /// none of the marks, so the session bar never shows half of what one
    /// check found. Returns how many were marked.
    pub fn mark_changed_elsewhere(&self, ids: &[String]) -> Result<usize> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut marked = 0;
        for id in ids {
            // The photograph as the transaction sees it: the check's reads
            // are not in it, and one edited by a concurrent write lands as a
            // whole row (or not at all), one removed meanwhile is left out.
            let Some(path): Option<PathBuf> = tx
                .query_row(
                    "SELECT r.path,r.external,a.rel_path FROM assets a JOIN roots r ON r.id=a.root_id WHERE a.id=?",
                    [id],
                    |r| Ok(self.join_root(r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
            else {
                continue;
            };
            if self.xmp_problem(id, &path)? == Some(XmpProblem::ChangedElsewhere) {
                let updated = tx.execute(
                    "UPDATE metadata SET xmp_status='conflict' WHERE asset_id=? AND xmp_status='synced'",
                    [id],
                )?;
                marked += updated;
                // Only a photograph still in sync is marked: a test's step
                // lands after a real mark, not after a skipped one.
                #[cfg(any(test, feature = "test-support"))]
                if updated > 0 {
                    self.step_marked()?;
                }
            }
        }
        tx.commit()?;
        Ok(marked)
    }
}
