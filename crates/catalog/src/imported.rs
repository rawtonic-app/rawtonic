// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an import from another editor (Lightroom catalog, Capture One
//! session or catalog) last wrote to a version (schema 31), so that a
//! repeated import never replaces an edit the photographer made here since
//! (decision C1RI1 2026-11-23). Digests only; the recipes themselves are in
//! the version's history.
use crate::CatalogError;
use crate::Library;
use rusqlite::{OptionalExtension, params};

/// The digests an import recorded for one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRecord {
    /// The other editor's settings as read.
    pub source: String,
    /// The recipe the import made of them.
    pub translated: String,
    /// The recipe as the library held it right after the import.
    pub stored: String,
}

impl Library {
    /// What the last import from another editor wrote to `version`, if one
    /// did.
    pub fn imported_recipe(&self, version: &str) -> Result<Option<ImportRecord>, CatalogError> {
        Ok(self
            .conn
            .query_row(
                "SELECT source,translated,stored FROM imported_recipes WHERE version_id=?",
                [version],
                |r| {
                    Ok(ImportRecord {
                        source: r.get(0)?,
                        translated: r.get(1)?,
                        stored: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Records what an import from another editor wrote to `version`.
    pub fn record_imported_recipe(
        &self,
        version: &str,
        record: &ImportRecord,
    ) -> Result<(), CatalogError> {
        self.conn.execute(
            "INSERT INTO imported_recipes(version_id,source,translated,stored) VALUES(?,?,?,?)
             ON CONFLICT(version_id) DO UPDATE SET
               source=excluded.source,translated=excluded.translated,stored=excluded.stored",
            params![version, record.source, record.translated, record.stored],
        )?;
        Ok(())
    }
}
