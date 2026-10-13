//! Full-content SHA-256 per asset. The sampled `fingerprint` (size and the
//! first and last 64 KiB) finds candidates; only the full hash proves that a
//! file found elsewhere is a missing photograph byte for byte. A missing
//! file cannot be read, so the hash has to be on record from before it went
//! missing: it is recorded for the file as registered (same size and
//! modification time) by verified ingest, which has it anyway, and by a
//! throttled background pass (`fill_checksums`) over the rest.
use crate::Library;
use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, UNIX_EPOCH},
};

/// The SHA-256 of a file's content, read in 1 MiB chunks; `None` when
/// `cancel` was set meanwhile.
pub fn sha256_file(path: &Path, cancel: &AtomicBool) -> Result<Option<String>> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(Some(
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
    ))
}

/// A regular file's size and modification time (seconds), as assets record
/// them.
pub fn stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Some((meta.len(), mtime))
}

impl Library {
    /// Records `sha256` as the content hash of asset `id` when its file is
    /// still the one registered (same size and modification time). Returns
    /// whether it was recorded.
    pub fn record_checksum(&self, id: &str, sha256: &str) -> Result<bool> {
        let asset = self.asset(id)?;
        let Some((size, mtime)) = stamp(&asset.path) else {
            return Ok(false);
        };
        if size != asset.size || mtime != asset.mtime {
            return Ok(false);
        }
        Ok(self.conn.execute(
            "UPDATE assets SET sha256=? WHERE id=? AND size=? AND mtime=?",
            params![sha256, id, i64::try_from(size)?, mtime],
        )? == 1)
    }

    /// The recorded content hash of asset `id`, if any.
    pub fn checksum(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT sha256 FROM assets WHERE id=?", [id], |r| r.get(0))?)
    }

    /// How many present assets have no content hash yet.
    pub fn checksum_backlog(&self) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM assets WHERE sha256 IS NULL AND missing=0",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// Hashes up to `limit` present assets without a content hash that come
    /// after `cursor` (in registration order; the cursor moves past every
    /// one looked at, so files that cannot be hashed are not tried again in
    /// the same pass), one file at a time with `pause` between files so the
    /// background pass leaves the disk to the user's work, until `cancel` is
    /// set. Files changed since they were registered, or not there, are
    /// skipped. Returns how many were looked at and how many recorded.
    pub fn fill_checksums(
        &self,
        cursor: &mut i64,
        limit: usize,
        pause: Duration,
        cancel: &AtomicBool,
    ) -> Result<(usize, usize)> {
        let rows: Vec<(i64, String, String, String, bool)> = self
            .conn
            .prepare(
                "SELECT a.rowid,a.id,a.rel_path,r.path,r.external FROM assets a JOIN roots r ON r.id=a.root_id
                 WHERE a.sha256 IS NULL AND a.missing=0 AND a.rowid>? ORDER BY a.rowid LIMIT ?",
            )?
            .query_map(params![*cursor, i64::try_from(limit)?], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let (mut looked, mut recorded) = (0, 0);
        for (rowid, id, relative, root, external) in rows {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            *cursor = rowid;
            looked += 1;
            let path: PathBuf = self.join_root(root, external, relative);
            let Ok(Some(hash)) = sha256_file(&path, cancel) else {
                continue;
            };
            if self.record_checksum(&id, &hash)? {
                recorded += 1;
            }
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
        Ok((looked, recorded))
    }

    /// Hashes every present asset without a content hash, at once (tests,
    /// `rawtonic checksums`). Returns how many were recorded.
    pub fn fill_all_checksums(&self, cancel: &AtomicBool) -> Result<usize> {
        let mut cursor = 0;
        let mut recorded = 0;
        loop {
            let (looked, done) = self.fill_checksums(&mut cursor, 256, Duration::ZERO, cancel)?;
            recorded += done;
            if looked == 0 || cancel.load(Ordering::Relaxed) {
                return Ok(recorded);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

    #[test]
    fn hashes_are_recorded_only_for_the_file_as_registered() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let a = lib.directory.join("Capture/A.ARW");
        let b = lib.directory.join("Capture/B.ARW");
        fs::write(&a, b"alpha").unwrap();
        fs::write(&b, b"bravo").unwrap();
        let (ia, ib) = (lib.add_asset(&a).unwrap().id, lib.add_asset(&b).unwrap().id);
        assert_eq!(lib.checksum_backlog().unwrap(), 2);
        // B changes after it was registered: its hash would describe
        // another file, so it is not recorded.
        fs::write(&b, b"bravo, changed size").unwrap();
        let none = AtomicBool::new(false);
        let mut cursor = 0;
        assert_eq!(
            lib.fill_checksums(&mut cursor, 10, Duration::ZERO, &none)
                .unwrap(),
            (2, 1)
        );
        // The pass moved past both; a new pass tries B again.
        assert_eq!(
            lib.fill_checksums(&mut cursor, 10, Duration::ZERO, &none)
                .unwrap(),
            (0, 0)
        );
        assert_eq!(
            lib.checksum(&ia).unwrap().as_deref(),
            sha256_file(&a, &none).unwrap().as_deref()
        );
        assert_eq!(lib.checksum(&ib).unwrap(), None);
        // A cancelled pass records nothing.
        assert_eq!(lib.fill_all_checksums(&AtomicBool::new(true)).unwrap(), 0);
        assert_eq!(fs::read(&a).unwrap(), b"alpha");
    }
}
