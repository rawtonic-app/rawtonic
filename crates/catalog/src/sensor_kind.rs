//! The sensor layout of each RAW photograph as read from its file's own
//! bytes (`sensor_kinds`, schema 38), so Develop shows the controls that fit
//! the sensor while the file is offline. The catalog reads no file for it:
//! the app passes the kind its sensor read verified, with the fingerprint of
//! the file it read. A read that could not tell the layout is not passed, so
//! nothing stored is a guess. Renders and exports never use the stored kind;
//! they read the sensor from the file.
use crate::{CatalogError, Library};
use rusqlite::{OptionalExtension, params};
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
    /// Records `kind` for asset `id`, read from a file whose fingerprint
    /// (`crate::fingerprint`) was `fingerprint`, when that is the file the
    /// library has registered for it. Returns whether it was recorded.
    pub fn record_sensor_kind(
        &self,
        id: &str,
        kind: SensorKind,
        fingerprint: &str,
    ) -> Result<bool, CatalogError> {
        Ok(self.conn.execute(
            "INSERT INTO sensor_kinds(asset_id,kind,fingerprint)
             SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1 AND fingerprint=?3)
             ON CONFLICT(asset_id) DO UPDATE SET kind=excluded.kind,fingerprint=excluded.fingerprint",
            params![id, kind.name(), fingerprint],
        )? == 1)
    }

    /// Asset `id`'s recorded sensor layout, while its registered file is
    /// still the one that was read.
    pub fn sensor_kind(&self, id: &str) -> Result<Option<SensorKind>, CatalogError> {
        let name: Option<String> = self
            .conn
            .query_row(
                "SELECT s.kind FROM sensor_kinds s JOIN assets a ON a.id=s.asset_id
                 WHERE s.asset_id=? AND s.fingerprint=a.fingerprint",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(name.as_deref().and_then(SensorKind::from_name))
    }
}
