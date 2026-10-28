//! Photographs whose XMP sidecar does not hold what the library holds yet
//! (`Summary::xmp_pending`): which ones, and writing them again on request.
//! The library's own edits are safe either way; only the sidecar lags.

use crate::Library;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        let mut retry = XmpRetry::default();
        for id in self.pending_xmp()? {
            let path = self.asset(&id)?.path;
            // Whether the file is there now, not the last check's flag: a
            // photograph moved away since gets no sidecar left behind, one
            // that is back gets its own.
            let problem = if !path.is_file() {
                Some(XmpProblem::Offline)
            } else if self.xmp_owners(&path)? > 1 {
                Some(XmpProblem::SharedName)
            } else if self.sidecar_changed_elsewhere(&id, &path)? {
                // `sync_xmp` refuses it too; it is not even read twice.
                Some(XmpProblem::ChangedElsewhere)
            } else {
                self.sync_xmp(&id)
                    .err()
                    .map(|e| XmpProblem::Failed(e.to_string()))
            };
            match problem {
                None => retry.written += 1,
                Some(problem) => retry.unsaved.push(UnsavedXmp { path, problem }),
            }
        }
        Ok(retry)
    }

    /// Whether a sidecar is there that the library did not write or read last
    /// (or something else that is not a file has its name).
    fn sidecar_changed_elsewhere(&self, id: &str, path: &std::path::Path) -> Result<bool> {
        let sidecar = path.with_extension("xmp");
        if sidecar.exists() && !sidecar.is_file() {
            return Ok(true);
        }
        let Ok(bytes) = std::fs::read(sidecar) else {
            return Ok(false);
        };
        let hash: Option<String> = self.conn.query_row(
            "SELECT xmp_hash FROM metadata WHERE asset_id=?",
            [id],
            |r| r.get(0),
        )?;
        Ok(hash.as_deref() != Some(rawtonic_metadata::digest(&bytes).as_str()))
    }
}
