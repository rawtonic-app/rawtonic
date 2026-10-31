//! Moving a whole session folder (G40, option (c)): a session stays a
//! self-contained job folder whose asset paths are relative, so it moves as
//! one piece — never photo by photo out of it. This module says whether a
//! session can go into a folder and where it lands; the move itself is one
//! journaled, no-clobber, undoable `rawtonic_fileops` step (a rename on the
//! same drive, a verified copy with the original trashed afterwards on
//! another), which the caller runs with the session closed.
use crate::{Manifest, Mode, db_name};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path, path::PathBuf};

/// Where a session folder goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMove {
    /// The session folder now (its real place).
    pub from: PathBuf,
    /// The session folder afterwards: the destination folder joined with
    /// the session folder's name.
    pub to: PathBuf,
    /// The session's name (its manifest's).
    pub name: String,
}

/// Plans moving the session folder `session` into `destination`. Refuses,
/// changing nothing, when `session` is no session (a catalog keeps its
/// photographs anywhere and is not moved this way), when `destination` is
/// the folder it is in, inside the session itself or inside another session
/// or catalog, when a file or folder of the session folder's name is there
/// already, or when another program (another Rawtonic, the `rawtonic`
/// command) has the session's database open. Free space is checked by the
/// move itself (`rawtonic_fileops`), which refuses before copying.
pub fn plan_session_move(session: &Path, destination: &Path) -> Result<SessionMove> {
    let planned = check_session_move(session, destination)?;
    ensure!(
        !in_use_elsewhere(&planned.from)?,
        "The session {} is open in another program (another Rawtonic window or the rawtonic command); close it there and try again. Nothing was moved.",
        planned.name
    );
    Ok(planned)
}

/// [`plan_session_move`] without asking whether the database is open
/// elsewhere: for offering the move, while this program may still have it
/// open itself.
pub fn check_session_move(session: &Path, destination: &Path) -> Result<SessionMove> {
    let from = session
        .canonicalize()
        .with_context(|| format!("{} is not available", session.display()))?;
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(from.join("session.json"))
            .with_context(|| format!("{} holds no Rawtonic session", from.display()))?,
    )
    .context("The session's session.json cannot be read")?;
    ensure!(
        manifest.mode == Mode::Session,
        "{} is a catalog; a catalog keeps its photographs anywhere and is not moved as a folder",
        from.display()
    );
    ensure!(
        destination.is_dir(),
        "Destination is not a folder: {}",
        destination.display()
    );
    let into = destination
        .canonicalize()
        .with_context(|| format!("{} is not available", destination.display()))?;
    ensure!(
        !into.starts_with(&from),
        "The session {} cannot be moved into its own folder; nothing was moved.",
        manifest.name
    );
    ensure!(
        from.parent() != Some(into.as_path()),
        "The session {} is already in {}.",
        manifest.name,
        into.display()
    );
    if let Some(library) = into
        .ancestors()
        .find(|p| p.join("session.json").is_file() || p.join("catalog.json").is_file())
    {
        bail!(
            "A session cannot go inside another session or catalog ({}); nothing was moved.",
            library.display()
        );
    }
    let name = from
        .file_name()
        .with_context(|| format!("{} has no name", from.display()))?;
    let to = into.join(name);
    ensure!(
        fs::symlink_metadata(&to).is_err(),
        "{} already holds something named {}; nothing was moved. Rename or move it first.",
        into.display(),
        name.to_string_lossy()
    );
    Ok(SessionMove {
        from,
        to,
        name: manifest.name,
    })
}

/// Whether another connection — of another program, or one still open in
/// this one — has the session's database open. A connection in WAL mode
/// keeps a shared lock for as long as it is open, so taking the database's
/// exclusive lock, without waiting, fails exactly then. On success the
/// probe's close checkpoints the WAL into the database file.
pub fn in_use_elsewhere(session: &Path) -> Result<bool> {
    let db = session.join(db_name(Mode::Session));
    ensure!(
        db.is_file(),
        "The session's database is missing: {}",
        db.display()
    );
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::ZERO)?;
    let probe = conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;");
    match probe {
        Ok(()) => Ok(false),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            Ok(true)
        }
        Err(e) => Err(e).context("The session's database cannot be checked"),
    }
}
