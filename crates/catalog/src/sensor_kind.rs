//! The sensor layout of each RAW photograph as read from its file's own
//! bytes (`sensor_kinds`, schema 38), so Develop shows the controls that fit
//! the sensor while the file is offline. The catalog reads no file for it:
//! the app passes the kind its sensor read verified, with the fingerprint of
//! the file it read; each read replaces the one before, and a change of the
//! registered file drops it. A read that could not tell the layout is not
//! passed, so nothing stored is a guess; one of another file than the
//! recorded read forgets it. Renders and exports never use the stored kind;
//! they read the sensor from the file.
use crate::{CatalogError, Library};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A sensor layout read from the file (never "unknown": that is not stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SensorKind {
    Bayer,
    XTrans,
}

impl SensorKind {
    fn name(self) -> &'static str {
        match self {
            SensorKind::Bayer => "bayer",
            SensorKind::XTrans => "xTrans",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bayer" => Some(SensorKind::Bayer),
            "xTrans" => Some(SensorKind::XTrans),
            _ => None,
        }
    }
}

impl Library {
    /// Records `kind` for asset `id`, read from its file when that file's
    /// fingerprint (`crate::fingerprint`) was `fingerprint`: the latest
    /// read replaces any earlier one, also when the file was changed in
    /// place without the library registering it yet. Returns whether asset
    /// `id` exists.
    pub fn record_sensor_kind(
        &self,
        id: &str,
        kind: SensorKind,
        fingerprint: &str,
    ) -> Result<bool, CatalogError> {
        Ok(self.conn.execute(
            "INSERT INTO sensor_kinds(asset_id,kind,fingerprint)
             SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1)
             ON CONFLICT(asset_id) DO UPDATE SET kind=excluded.kind,fingerprint=excluded.fingerprint",
            params![id, kind.name(), fingerprint],
        )? == 1)
    }

    /// Asset `id`'s latest recorded sensor layout (none since its
    /// registered file changed), with the fingerprint of the file it was
    /// read from.
    pub fn sensor_read(&self, id: &str) -> Result<Option<(SensorKind, String)>, CatalogError> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT kind,fingerprint FROM sensor_kinds WHERE asset_id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(kind, fingerprint)| Some((SensorKind::from_name(&kind)?, fingerprint))))
    }

    /// Forgets asset `id`'s recorded layout: its file is another one now,
    /// whose read could not tell the layout.
    pub fn forget_sensor_kind(&self, id: &str) -> Result<(), CatalogError> {
        self.conn
            .execute("DELETE FROM sensor_kinds WHERE asset_id=?", [id])?;
        Ok(())
    }

    /// Asset `id`'s latest recorded sensor layout ([`Library::sensor_read`]).
    pub fn sensor_kind(&self, id: &str) -> Result<Option<SensorKind>, CatalogError> {
        Ok(self.sensor_read(id)?.map(|(kind, _)| kind))
    }
}

/// After `ids` were relinked to files found by their registered fingerprint:
/// drops their layouts read from another file (one that had taken their
/// place), which the files now linked are not.
pub(crate) fn forget_reads_of_other_files(
    tx: &Connection,
    ids: impl IntoIterator<Item = impl AsRef<str>>,
) -> rusqlite::Result<()> {
    let mut forget = tx.prepare(
        "DELETE FROM sensor_kinds WHERE asset_id=?1
         AND fingerprint IS NOT (SELECT fingerprint FROM assets WHERE id=?1)",
    )?;
    for id in ids {
        forget.execute([id.as_ref()])?;
    }
    Ok(())
}
