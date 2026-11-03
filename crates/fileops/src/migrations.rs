//! The operation journal's schema: every migration in one table and the one
//! runner that applies them, in the shape of the catalog's
//! (`rawtonic_catalog`'s `migrations.rs`). A new journal is migrated from
//! version 0, an older one from its `user_version`.
//!
//! Adding a version: add one `Migration` line to `MIGRATIONS` whose script
//! ends with `PRAGMA user_version=N;`.
use anyhow::{Result, ensure};
use rusqlite::{Connection, TransactionBehavior};

/// One schema step: from `version - 1` to `version`.
pub(crate) struct Migration {
    pub version: u32,
    /// Columns added before the script, each only where it is missing.
    pub columns: Option<(&'static str, &'static [(&'static str, &'static str)])>,
    pub script: &'static str,
}

pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        columns: None,
        script: "CREATE TABLE IF NOT EXISTS operations(
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL UNIQUE,
               kind TEXT NOT NULL,
               label TEXT NOT NULL,
               state TEXT NOT NULL,
               synced INTEGER NOT NULL DEFAULT 0,
               payload TEXT NOT NULL,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             PRAGMA user_version=1;",
    },
    // The redo stack. Operations undone before it are not redoable
    // (nothing offered them).
    Migration {
        version: 2,
        columns: Some(("operations", &[("redoable", "INTEGER NOT NULL DEFAULT 0")])),
        script: "PRAGMA user_version=2;",
    },
];

/// The newest journal version: the last migration's.
pub(crate) const LATEST: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

// The table lists versions 1, 2, … in order, none left out.
const _: () = {
    let mut index = 0;
    while index < MIGRATIONS.len() {
        assert!(MIGRATIONS[index].version as usize == index + 1);
        index += 1;
    }
};

/// Brings a new (version 0) or older journal up to `LATEST` in one
/// immediate transaction; a newer one is refused rather than misread.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let version = user_version(conn)?;
    ensure!(
        version <= LATEST,
        "The file operation journal is from a newer Rawtonic (version {version})"
    );
    if version == LATEST {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = user_version(&tx)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version > version) {
        if let Some((table, columns)) = migration.columns {
            add_missing_columns(&tx, table, columns)?;
        }
        tx.execute_batch(migration.script)?;
    }
    tx.commit()?;
    Ok(())
}

fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// A migration's new columns, each only where it is missing.
fn add_missing_columns(conn: &Connection, table: &str, add: &[(&str, &str)]) -> Result<()> {
    let columns: Vec<String> = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, kind) in add {
        if !columns.iter().any(|c| c == name) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {kind}"))?;
        }
    }
    Ok(())
}
