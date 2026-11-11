//! The library database's schema: every migration in one table and the one
//! runner that applies them. A new library is migrated from schema 0, an
//! older one from its `user_version`, both by the same steps, so a new and
//! an upgraded library cannot drift apart (`tests/migration_fixtures.rs`).
//!
//! Adding a schema: write `migrations/NNNN.sql` (ending with
//! `PRAGMA user_version=NNNN;`) and add one `Migration` line to `MIGRATIONS`.
use crate::error::ensure;
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

/// One schema step: from `version - 1` to `version`.
pub(crate) struct Migration {
    pub version: u32,
    /// Columns added before the script, each only where it is missing, so
    /// the step can run again on a library a test took back to an older
    /// schema (`tests/undo_*.sql`).
    pub columns: Option<(&'static str, &'static [(&'static str, &'static str)])>,
    pub script: &'static str,
}

impl Migration {
    const fn sql(version: u32, script: &'static str) -> Self {
        Self {
            version,
            columns: None,
            script,
        }
    }

    const fn with_columns(
        version: u32,
        table: &'static str,
        columns: &'static [(&'static str, &'static str)],
        script: &'static str,
    ) -> Self {
        Self {
            version,
            columns: Some((table, columns)),
            script,
        }
    }
}

pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration::sql(1, include_str!("../migrations/0001.sql")),
    Migration::sql(2, include_str!("../migrations/0002.sql")),
    Migration::sql(3, include_str!("../migrations/0003.sql")),
    Migration::sql(4, include_str!("../migrations/0004.sql")),
    Migration::sql(5, include_str!("../migrations/0005.sql")),
    Migration::sql(6, include_str!("../migrations/0006.sql")),
    Migration::sql(7, include_str!("../migrations/0007.sql")),
    Migration::sql(8, include_str!("../migrations/0008.sql")),
    Migration::sql(9, include_str!("../migrations/0009.sql")),
    Migration::sql(10, include_str!("../migrations/0010.sql")),
    Migration::sql(11, include_str!("../migrations/0011.sql")),
    Migration::sql(12, include_str!("../migrations/0012.sql")),
    Migration::sql(13, include_str!("../migrations/0013.sql")),
    Migration::sql(14, include_str!("../migrations/0014.sql")),
    Migration::sql(15, include_str!("../migrations/0015.sql")),
    Migration::sql(16, include_str!("../migrations/0016.sql")),
    Migration::sql(17, include_str!("../migrations/0017.sql")),
    Migration::sql(18, include_str!("../migrations/0018.sql")),
    Migration::sql(19, include_str!("../migrations/0019.sql")),
    Migration::sql(20, include_str!("../migrations/0020.sql")),
    Migration::sql(21, include_str!("../migrations/0021.sql")),
    // Burst and bracket facts of `assets`.
    Migration::with_columns(
        22,
        "assets",
        &[
            ("exposure_mode", "INTEGER"),
            ("exposure_bias", "REAL"),
            ("sequence", "INTEGER"),
        ],
        include_str!("../migrations/0022.sql"),
    ),
    // The digest of the XMP sidecar being published, in `metadata`.
    Migration::with_columns(
        23,
        "metadata",
        &[("xmp_publishing", "TEXT")],
        include_str!("../migrations/0023.sql"),
    ),
    Migration::sql(24, include_str!("../migrations/0024.sql")),
    Migration::sql(25, include_str!("../migrations/0025.sql")),
    // The older-AI-Denoise-version fact of recipes saved before version 4.
    Migration::sql(26, include_str!("../migrations/0026.sql")),
];

/// The newest schema: the last migration's.
pub(crate) const LATEST: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

// The table lists schemas 1, 2, … in order, none left out.
const _: () = {
    let mut index = 0;
    while index < MIGRATIONS.len() {
        assert!(MIGRATIONS[index].version as usize == index + 1);
        index += 1;
    }
};

/// Builds the schema of a new library on an empty database.
pub(crate) fn create(conn: &Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = user_version(&tx)?;
    ensure!(version == 0, "The new library database is not empty");
    apply(&tx, version)?;
    tx.commit()?;
    Ok(())
}

/// Brings an older database up to `LATEST` in one immediate transaction
/// (another connection may be migrating at the same time). Newer databases
/// are refused rather than misread.
pub(crate) fn upgrade(conn: &Connection) -> Result<()> {
    let version = user_version(conn)?;
    ensure!(
        (1..=LATEST).contains(&version),
        "Unsupported database schema {version}; this Rawtonic reads up to {LATEST}"
    );
    if version == LATEST {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = user_version(&tx)?;
    apply(&tx, version)?;
    tx.commit()?;
    Ok(())
}

fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Every migration after schema `from`, in order.
fn apply(conn: &Connection, from: u32) -> Result<()> {
    for migration in MIGRATIONS.iter().filter(|m| m.version > from) {
        if let Some((table, columns)) = migration.columns {
            add_missing_columns(conn, table, columns)?;
        }
        conn.execute_batch(migration.script)?;
    }
    Ok(())
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
