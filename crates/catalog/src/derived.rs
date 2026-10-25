//! Files rendered from a photograph for another program (Edit With): each
//! is a photograph of its own, stacked with the one it was made from. The
//! derived asset's `assets.stack` holds the id of that original; the
//! original keeps `stack` empty, so a stack is the original plus every
//! asset naming it. Nothing here touches files.
use crate::{Asset, Library};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

impl Library {
    /// Records that `derived` was rendered from `original`. A derived file's
    /// own derivatives join its original's stack, so stacks stay one level.
    pub fn set_derived_from(&self, derived: &str, original: &str) -> Result<()> {
        ensure!(
            derived != original,
            "A photograph cannot derive from itself"
        );
        let top = self
            .derived_from(original)?
            .map_or_else(|| original.to_string(), |a| a.id);
        ensure!(
            top != derived,
            "A photograph cannot derive from its own render"
        );
        // Both must be registered; `asset` fails for an unknown id.
        self.asset(derived)?;
        self.asset(&top)?;
        // Renders of `derived` move with it into `top`'s stack.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE assets SET stack=? WHERE id=? OR stack=?",
            params![top, derived, derived],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The photograph `id` was rendered from, while it is still registered.
    pub fn derived_from(&self, id: &str) -> Result<Option<Asset>> {
        let stack: Option<String> = self
            .conn
            .query_row("SELECT stack FROM assets WHERE id=?", [id], |r| r.get(0))
            .optional()?
            .flatten();
        match stack {
            Some(original) if original != id => match self.asset(&original) {
                Ok(asset) => Ok(Some(asset)),
                Err(_) => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// The files rendered from `id`, oldest first.
    pub fn derivatives(&self, id: &str) -> Result<Vec<Asset>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM assets WHERE stack=? AND id<>? ORDER BY rowid")?;
        let ids = statement
            .query_map(params![id, id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|derived| self.asset(derived)).collect()
    }

    /// After another program saved asset `id`'s file in place (an Edit With
    /// render): records its new size, time and fingerprint, and forgets its
    /// content hash (the background pass hashes it again).
    pub fn file_saved(&self, id: &str) -> Result<()> {
        let asset = self.asset(id)?;
        let (size, mtime) = crate::checksum::stamp(&asset.path)
            .with_context(|| format!("{} is not there", asset.path.display()))?;
        let fingerprint = crate::fingerprint(&asset.path)?;
        self.conn.execute(
            "UPDATE assets SET size=?,mtime=?,fingerprint=?,sha256=NULL WHERE id=?",
            params![i64::try_from(size)?, mtime, fingerprint, id],
        )?;
        Ok(())
    }
}
