//! Journaled, undoable file operations: rename, move and trash.
//!
//! Every operation is planned completely, validated, written to a SQLite
//! journal and only then carried out, one no-clobber rename at a time. A
//! failure rolls the applied steps back; an interrupted operation (crash,
//! power loss) is rolled back by [`Journal::recover`] on the next start. A
//! finished operation can be undone as long as nothing has taken its place,
//! and an undone one redone ([`Journal::redo`]) until a new operation runs:
//! redo validates the plan again and runs it like the original.
//!
//! Only [`Journal::purge`] deletes, and only what is inside one folder the
//! caller names (a session's own `Trash/`), listed by [`plan_purge`] without
//! following links. Everything else is renames: "Trash" is either a rename
//! into such a folder or the system Trash through a [`SystemTrash`] the
//! caller provides, which must be able to put items back. Moving between
//! volumes would need a copy and a delete and is refused.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

/// Where a step puts its file or folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Target {
    /// A rename on the same volume.
    Path { path: PathBuf },
    /// The system Trash; `item` identifies the trashed file once it is there.
    SystemTrash { item: Option<String> },
}

/// One file or folder of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub from: PathBuf,
    pub to: Target,
}

impl Step {
    pub fn rename(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: Target::Path { path: to.into() },
        }
    }
    pub fn system_trash(from: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: Target::SystemTrash { item: None },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    /// Written before the first step; seen after a crash only.
    Applying,
    Done,
    /// Written before undoing; seen after a crash only.
    Undoing,
    Undone,
    /// A failed or interrupted operation whose applied steps were reversed.
    RolledBack,
    /// Written before redoing an undone operation; seen after a crash only
    /// (recovery puts it back to undone).
    Redoing,
    /// Emptying a folder: its entries are being moved aside. Seen after a
    /// crash only (recovery puts them back).
    Purging,
    /// Every entry was moved aside: from here on they are deleted, and a
    /// crash finishes the deletion at the next start.
    PurgeCommitted,
    /// Emptied for good. Not undoable, and the end of Undo for every
    /// operation before it.
    Purged,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Applying => "applying",
            State::Done => "done",
            State::Undoing => "undoing",
            State::Undone => "undone",
            State::RolledBack => "rolledBack",
            State::Redoing => "redoing",
            State::Purging => "purging",
            State::PurgeCommitted => "purgeCommitted",
            State::Purged => "purged",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "applying" => State::Applying,
            "done" => State::Done,
            "undoing" => State::Undoing,
            "undone" => State::Undone,
            "rolledBack" => State::RolledBack,
            "redoing" => State::Redoing,
            "purging" => State::Purging,
            "purgeCommitted" => State::PurgeCommitted,
            "purged" => State::Purged,
            other => bail!("Unknown file operation state {other}"),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    steps: Vec<Step>,
    /// Folders the operation created for its targets, outermost first.
    #[serde(default)]
    created_dirs: Vec<PathBuf>,
    #[serde(default)]
    note: Option<String>,
    /// The caller's own data, e.g. album memberships to restore on undo.
    #[serde(default)]
    context: Option<serde_json::Value>,
    /// A purge: the folder it empties.
    #[serde(default)]
    purge_folder: Option<PathBuf>,
    /// A purge: every file (and link) it deletes, at its place before.
    #[serde(default)]
    removed: Vec<PathBuf>,
}

/// A journaled operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    /// Caller-defined kind, e.g. `rename`, `move`, `trash`.
    pub kind: String,
    /// What the user sees, e.g. "Rename 3 photos".
    pub label: String,
    pub state: State,
    pub steps: Vec<Step>,
    pub created_dirs: Vec<PathBuf>,
    /// Why an operation was rolled back, or what recovery could not restore.
    pub note: Option<String>,
    pub context: Option<serde_json::Value>,
    /// A purge: every file (and link) it deletes, at its place in the
    /// emptied folder; folders are not listed.
    pub removed: Vec<PathBuf>,
    /// Whether the caller has brought its own records (catalog, albums) in
    /// line with the operation's current state.
    pub synced: bool,
    pub created_at: String,
}

impl Operation {
    /// Pairs of (old, new) location for the operation's current state:
    /// after it was done (`from` → target) or undone/rolled back (none).
    pub fn renames(&self) -> Vec<(PathBuf, PathBuf)> {
        self.steps
            .iter()
            .filter_map(|step| match &step.to {
                Target::Path { path } => Some((step.from.clone(), path.clone())),
                Target::SystemTrash { .. } => None,
            })
            .collect()
    }
    /// A purge's top-level entries of the emptied folder, at their place
    /// there (what the caller's records forget, with everything below).
    pub fn purged(&self) -> Vec<PathBuf> {
        if !matches!(
            self.state,
            State::Purging | State::PurgeCommitted | State::Purged
        ) {
            return Vec::new();
        }
        self.steps.iter().map(|step| step.from.clone()).collect()
    }
    /// Sources the operation sent to the system Trash.
    pub fn trashed(&self) -> Vec<PathBuf> {
        self.steps
            .iter()
            .filter(|step| matches!(step.to, Target::SystemTrash { .. }))
            .map(|step| step.from.clone())
            .collect()
    }
}

/// The system Trash, provided by the platform layer. It must be able to put
/// an item back, otherwise trashing through it would not be undoable.
pub trait SystemTrash {
    /// Moves `path` to the Trash and returns an identifier to find it there.
    fn trash(&self, path: &Path) -> Result<String>;
    /// Puts a trashed item back at `original` without replacing anything.
    fn restore(&self, item: &str, original: &Path) -> Result<()>;
    /// Whether the item is still in the Trash.
    fn contains(&self, item: &str) -> bool;
}

/// A trash that refuses: for callers that only rename.
pub struct NoSystemTrash;

impl SystemTrash for NoSystemTrash {
    fn trash(&self, _path: &Path) -> Result<String> {
        bail!("The system Trash is not available here; nothing was moved")
    }
    fn restore(&self, _item: &str, _original: &Path) -> Result<()> {
        bail!("The system Trash is not available here")
    }
    fn contains(&self, _item: &str) -> bool {
        false
    }
}

const JOURNAL_VERSION: u32 = 2;

/// The hidden folder inside an emptied folder that its entries are moved
/// into before they are deleted (followed by the operation's id).
const PURGE_STAGING_PREFIX: &str = ".rawtonic-emptying-";

/// The sequence number of the last emptied folder: Undo and Redo end there.
const BARRIER_SQL: &str =
    "SELECT COALESCE(MAX(seq),0) FROM operations WHERE state IN ('purgeCommitted','purged')";

/// The operation journal, one SQLite file (e.g. in the app's data folder).
pub struct Journal {
    conn: Connection,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| {
            format!("Cannot open the file operation journal {}", path.display())
        })?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= JOURNAL_VERSION,
            "The file operation journal is from a newer Rawtonic (version {version})"
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS operations(
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL UNIQUE,
               kind TEXT NOT NULL,
               label TEXT NOT NULL,
               state TEXT NOT NULL,
               synced INTEGER NOT NULL DEFAULT 0,
               payload TEXT NOT NULL,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
        )?;
        if version < 2 {
            // Version 2: the redo stack. Operations undone before it are
            // not redoable (nothing offered them).
            let tx = conn.unchecked_transaction()?;
            let has_column: bool = tx.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('operations') WHERE name='redoable'",
                [],
                |r| r.get(0),
            )?;
            if !has_column {
                tx.execute_batch(
                    "ALTER TABLE operations ADD COLUMN redoable INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            tx.execute_batch("PRAGMA user_version=2;")?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Validates the whole plan, journals it and carries it out. On failure
    /// the applied steps are reversed and the error is returned; the journal
    /// keeps the operation as rolled back. A finished operation clears the
    /// redo stack.
    pub fn execute(
        &self,
        kind: &str,
        label: &str,
        steps: Vec<Step>,
        trash: &dyn SystemTrash,
    ) -> Result<Operation> {
        ensure!(!steps.is_empty(), "Nothing to do");
        validate(&steps)?;
        let created_dirs = create_target_parents(&steps)?;
        let mut payload = Payload {
            steps,
            created_dirs,
            ..Payload::default()
        };
        let id = uuid::Uuid::new_v4().to_string();
        self.insert(&id, kind, label, State::Applying, &payload)?;
        for index in 0..payload.steps.len() {
            let result = apply(&mut payload.steps[index], trash);
            if let Err(error) = result {
                let reversed = reverse_steps(&payload.steps[..index], trash);
                remove_created_dirs(&payload.created_dirs);
                payload.note = Some(match reversed {
                    Ok(()) => format!("{error:#}"),
                    Err(e) => format!("{error:#}; could not reverse every step: {e:#}"),
                });
                self.store(&id, State::RolledBack, &payload)?;
                return Err(error.context("Nothing was changed"));
            }
            if matches!(payload.steps[index].to, Target::SystemTrash { .. }) {
                // The only state the file system cannot tell later.
                self.store(&id, State::Applying, &payload)?;
            }
        }
        self.store_clearing_redo(&id, State::Done, &payload)?;
        self.operation(&id)
    }

    /// Undoes a finished operation. Refuses, changing nothing, when any file
    /// has moved on since or something now occupies an original location.
    /// An undone operation goes onto the redo stack.
    pub fn undo(&self, id: &str, trash: &dyn SystemTrash) -> Result<Operation> {
        let op = self.operation(id)?;
        ensure!(
            op.state == State::Done,
            "Only finished operations can be undone"
        );
        ensure!(
            self.seq(id)? > self.barrier()?,
            "The Trash was emptied after this change; it can no longer be undone"
        );
        for step in &op.steps {
            match &step.to {
                Target::Path { path } => {
                    ensure!(
                        exists(path),
                        "{} is no longer there; nothing was undone",
                        path.display()
                    );
                    ensure!(
                        !exists(&step.from) || same_file(&step.from, path),
                        "{} exists again; nothing was undone",
                        step.from.display()
                    );
                }
                Target::SystemTrash { item } => {
                    let item = item.as_deref().context("Trashed item was not recorded")?;
                    ensure!(
                        trash.contains(item),
                        "{} is no longer in the Trash; nothing was undone",
                        step.from.display()
                    );
                    ensure!(
                        !exists(&step.from),
                        "{} exists again; nothing was undone",
                        step.from.display()
                    );
                }
            }
        }
        let mut payload = Payload {
            steps: op.steps.clone(),
            created_dirs: op.created_dirs.clone(),
            context: op.context.clone(),
            ..Payload::default()
        };
        self.store(id, State::Undoing, &payload)?;
        let total = payload.steps.len();
        for undone in 0..total {
            let step = &payload.steps[total - 1 - undone];
            if let Err(error) = reverse(step, trash) {
                // Put back what was already undone: the operation stays done.
                let redo = payload.steps[total - undone..]
                    .iter_mut()
                    .try_for_each(|step| apply(step, trash));
                payload.note = Some(match redo {
                    Ok(()) => format!("Undo failed: {error:#}"),
                    Err(e) => format!("Undo failed: {error:#}; could not restore: {e:#}"),
                });
                self.store(id, State::Done, &payload)?;
                return Err(error.context("Nothing was undone"));
            }
        }
        remove_created_dirs(&payload.created_dirs);
        let tx = self.conn.unchecked_transaction()?;
        self.store(id, State::Undone, &payload)?;
        self.set_redoable(id, true)?;
        tx.commit()?;
        self.operation(id)
    }

    /// Redoes an undone operation on the redo stack: its plan is validated
    /// again as if new (every source back in place, no target taken, same
    /// volume), journaled and carried out one no-clobber rename at a time.
    /// Refuses, changing nothing, when the world changed since the undo; a
    /// failing step rolls the applied ones back and the operation stays
    /// undone (and redoable); a crash is rolled back by [`Journal::recover`].
    pub fn redo(&self, id: &str, trash: &dyn SystemTrash) -> Result<Operation> {
        let op = self.operation(id)?;
        ensure!(
            op.state == State::Undone && self.redoable(id)?,
            "Only an undone operation can be redone, and only until another file operation runs"
        );
        let mut steps = op.steps.clone();
        for step in &mut steps {
            // The item of the first trashing was put back by the undo.
            if let Target::SystemTrash { item } = &mut step.to {
                *item = None;
            }
        }
        validate(&steps).context("Nothing was redone")?;
        let created_dirs = create_target_parents(&steps).context("Nothing was redone")?;
        let mut payload = Payload {
            steps,
            created_dirs,
            context: op.context.clone(),
            ..Payload::default()
        };
        // The caller's records still follow the undone state until it is done.
        self.store_keeping_sync(id, State::Redoing, &payload)?;
        for index in 0..payload.steps.len() {
            if let Err(error) = apply(&mut payload.steps[index], trash) {
                let reversed = reverse_steps(&payload.steps[..index], trash);
                remove_created_dirs(&payload.created_dirs);
                payload.note = Some(match reversed {
                    Ok(()) => format!("Redo failed: {error:#}"),
                    Err(e) => {
                        format!("Redo failed: {error:#}; could not reverse every step: {e:#}")
                    }
                });
                self.store_keeping_sync(id, State::Undone, &payload)?;
                return Err(error.context("Nothing was redone"));
            }
            if matches!(payload.steps[index].to, Target::SystemTrash { .. }) {
                self.store_keeping_sync(id, State::Redoing, &payload)?;
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        self.store(id, State::Done, &payload)?;
        self.set_redoable(id, false)?;
        tx.commit()?;
        self.operation(id)
    }

    /// The newest finished operation, the one Undo applies to (none before
    /// the last emptied folder).
    pub fn last_undoable(&self) -> Result<Option<Operation>> {
        let id: Option<String> = self
            .conn
            .query_row(
                &format!(
                    "SELECT id FROM operations WHERE state='done' AND seq > ({BARRIER_SQL})
                     ORDER BY seq DESC LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.operation(&id)).transpose()
    }

    /// The operation Redo applies to: the one undone last. (Undo always
    /// takes the newest finished operation and a new operation clears the
    /// stack, so that is the redoable one with the lowest sequence.)
    pub fn last_redoable(&self) -> Result<Option<Operation>> {
        let id: Option<String> = self
            .conn
            .query_row(
                &format!(
                    "SELECT id FROM operations WHERE state='undone' AND redoable=1
                     AND seq > ({BARRIER_SQL}) ORDER BY seq ASC LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.operation(&id)).transpose()
    }

    fn barrier(&self) -> Result<i64> {
        Ok(self.conn.query_row(BARRIER_SQL, [], |r| r.get(0))?)
    }

    fn seq(&self, id: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT seq FROM operations WHERE id=?", [id], |r| r.get(0))?)
    }

    fn redoable(&self, id: &str) -> Result<bool> {
        let (redoable, seq): (bool, i64) = self.conn.query_row(
            "SELECT redoable, seq FROM operations WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(redoable && seq > self.barrier()?)
    }

    fn set_redoable(&self, id: &str, redoable: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE operations SET redoable=? WHERE id=?",
            params![redoable, id],
        )?;
        Ok(())
    }

    /// Permanently deletes what `plan` lists in its folder (see
    /// [`plan_purge`]). Refuses, deleting nothing, when the folder's
    /// contents differ from the plan now. The entries are first moved, one
    /// no-clobber rename each, into a hidden folder inside it (so what is
    /// added to the folder meanwhile is never deleted); a failure or crash
    /// during that puts them back. Once all are aside the operation is
    /// committed: they are deleted (never following a link), and a crash
    /// finishes the deletion at the next [`Journal::recover`]. Not undoable;
    /// it ends Undo and Redo for every earlier operation, and its caller
    /// syncs its records like any operation's ([`Journal::unsynced`]).
    /// When some files could not be deleted the result stays committed
    /// with a note; the next recovery tries again.
    pub fn purge(&self, kind: &str, label: &str, plan: &PurgePlan) -> Result<Operation> {
        ensure!(!plan.entries.is_empty(), "The folder is empty");
        let now = plan_purge(&plan.folder).context("Nothing was deleted")?;
        ensure!(
            now == *plan,
            "{} changed since it was listed; nothing was deleted",
            plan.folder.display()
        );
        let id = uuid::Uuid::new_v4().to_string();
        let staging = plan.folder.join(format!("{PURGE_STAGING_PREFIX}{id}"));
        let mut steps = Vec::with_capacity(plan.entries.len());
        for entry in &plan.entries {
            ensure!(
                directly_in(entry, &plan.folder),
                "{} is not in {}; nothing was deleted",
                entry.display(),
                plan.folder.display()
            );
            let name = entry.file_name().context("An entry has no name")?;
            steps.push(Step::rename(entry, staging.join(name)));
        }
        validate(&steps).context("Nothing was deleted")?;
        let created_dirs = create_target_parents(&steps).context("Nothing was deleted")?;
        let mut payload = Payload {
            steps,
            created_dirs,
            purge_folder: Some(plan.folder.clone()),
            removed: plan.files.clone(),
            ..Payload::default()
        };
        self.insert(&id, kind, label, State::Purging, &payload)?;
        for index in 0..payload.steps.len() {
            if let Err(error) = apply(&mut payload.steps[index], &NoSystemTrash) {
                let reversed = reverse_steps(&payload.steps[..index], &NoSystemTrash);
                remove_created_dirs(&payload.created_dirs);
                payload.note = Some(match reversed {
                    Ok(()) => format!("{error:#}"),
                    Err(e) => format!("{error:#}; could not reverse every step: {e:#}"),
                });
                self.store(&id, State::RolledBack, &payload)?;
                return Err(error.context("Nothing was deleted"));
            }
        }
        self.store_clearing_redo(&id, State::PurgeCommitted, &payload)?;
        self.finish_purge(&id, payload)?;
        self.operation(&id)
    }

    /// Deletes a committed purge's entries (already aside) and records it
    /// as purged; on failure it stays committed with a note.
    fn finish_purge(&self, id: &str, mut payload: Payload) -> Result<()> {
        match delete_staged(&payload) {
            Ok(()) => {
                remove_created_dirs(&payload.created_dirs);
                payload.note = None;
                self.store(id, State::Purged, &payload)
            }
            Err(error) => {
                payload.note = Some(format!(
                    "Not everything could be deleted yet (tried again at the next start): {error:#}"
                ));
                self.store(id, State::PurgeCommitted, &payload)
            }
        }
    }

    /// Rolls back operations a crash interrupted (an interrupted redo goes
    /// back to undone), finishes the deletion of a committed purge and
    /// returns every operation whose caller records still need to follow it
    /// (including those).
    pub fn recover(&self, trash: &dyn SystemTrash) -> Result<Vec<Operation>> {
        let ids: Vec<String> = {
            let mut q = self.conn.prepare(
                "SELECT id FROM operations
                 WHERE state IN ('applying','undoing','redoing','purging','purgeCommitted')
                 ORDER BY seq",
            )?;
            q.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in ids {
            let state = self.operation(&id)?.state;
            let mut payload = self.payload(&id)?;
            if state == State::PurgeCommitted {
                self.finish_purge(&id, payload)?;
                continue;
            }
            payload.note = None;
            let mut problems = Vec::new();
            for step in payload.steps.iter().rev() {
                if let Err(e) = restore_if_moved(step, trash) {
                    problems.push(format!("{e:#}"));
                }
            }
            remove_created_dirs(&payload.created_dirs);
            if !problems.is_empty() {
                payload.note = Some(format!("Recovery: {}", problems.join("; ")));
            }
            match state {
                State::Undoing => {
                    self.store(&id, State::Undone, &payload)?;
                    self.set_redoable(&id, true)?;
                    // An interrupted undo may have reversed steps the
                    // caller's records still show as done.
                    self.set_synced(&id, false)?;
                }
                State::Redoing => {
                    // Back to undone: the records follow it as far as they
                    // did before the redo began.
                    self.store_keeping_sync(&id, State::Undone, &payload)?;
                }
                _ => {
                    // Applying or purging: nothing the records follow changed.
                    self.store(&id, State::RolledBack, &payload)?;
                    self.set_synced(&id, true)?;
                }
            }
        }
        self.unsynced()
    }

    /// Finished, undone or purged operations the caller's records do not
    /// follow yet (their sync failed or never ran), oldest first: sync them
    /// in order.
    pub fn unsynced(&self) -> Result<Vec<Operation>> {
        let ids: Vec<String> = {
            let mut q = self.conn.prepare(
                "SELECT id FROM operations WHERE synced=0
                 AND state IN ('done','undone','purgeCommitted','purged') ORDER BY seq",
            )?;
            q.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.operation(id)).collect()
    }

    /// Stores the caller's data with an operation (kept through undo).
    pub fn set_context(&self, id: &str, context: serde_json::Value) -> Result<()> {
        let mut payload = self.payload(id)?;
        payload.context = Some(context);
        self.conn.execute(
            "UPDATE operations SET payload=? WHERE id=?",
            params![serde_json::to_string(&payload)?, id],
        )?;
        Ok(())
    }

    /// Records that the caller's own records follow the operation's state.
    pub fn mark_synced(&self, id: &str) -> Result<()> {
        self.set_synced(id, true)
    }

    fn set_synced(&self, id: &str, synced: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE operations SET synced=? WHERE id=?",
            params![synced, id],
        )?;
        Ok(())
    }

    /// The newest operations, newest first.
    pub fn history(&self, limit: usize) -> Result<Vec<Operation>> {
        let ids: Vec<String> = {
            let mut q = self
                .conn
                .prepare("SELECT id FROM operations ORDER BY seq DESC LIMIT ?")?;
            q.query_map([limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.operation(id)).collect()
    }

    pub fn operation(&self, id: &str) -> Result<Operation> {
        let (kind, label, state, synced, payload, created_at): (
            String,
            String,
            String,
            bool,
            String,
            String,
        ) = self
            .conn
            .query_row(
                "SELECT kind,label,state,synced,payload,created_at FROM operations WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("No file operation {id}"))?;
        let payload: Payload = serde_json::from_str(&payload)?;
        Ok(Operation {
            id: id.to_string(),
            kind,
            label,
            state: State::parse(&state)?,
            steps: payload.steps,
            created_dirs: payload.created_dirs,
            note: payload.note,
            context: payload.context,
            removed: payload.removed,
            synced,
            created_at,
        })
    }

    fn payload(&self, id: &str) -> Result<Payload> {
        let payload: String =
            self.conn
                .query_row("SELECT payload FROM operations WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&payload)?)
    }

    fn insert(
        &self,
        id: &str,
        kind: &str,
        label: &str,
        state: State,
        payload: &Payload,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operations(id,kind,label,state,payload) VALUES(?,?,?,?,?)",
            params![
                id,
                kind,
                label,
                state.as_str(),
                serde_json::to_string(payload)?
            ],
        )?;
        Ok(())
    }

    fn store(&self, id: &str, state: State, payload: &Payload) -> Result<()> {
        // A changed state invalidates what the caller synced.
        self.conn.execute(
            "UPDATE operations SET state=?, payload=?, synced=0 WHERE id=?",
            params![state.as_str(), serde_json::to_string(payload)?, id],
        )?;
        Ok(())
    }

    /// For states the caller's records do not see change (a redo before it
    /// is done, and its rollback to undone).
    fn store_keeping_sync(&self, id: &str, state: State, payload: &Payload) -> Result<()> {
        self.conn.execute(
            "UPDATE operations SET state=?, payload=? WHERE id=?",
            params![state.as_str(), serde_json::to_string(payload)?, id],
        )?;
        Ok(())
    }

    /// A new operation is done (or a purge committed): nothing undone
    /// before can be redone any more. One transaction.
    fn store_clearing_redo(&self, id: &str, state: State, payload: &Payload) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.store(id, state, payload)?;
        self.conn
            .execute("UPDATE operations SET redoable=0 WHERE redoable=1", [])?;
        tx.commit()?;
        Ok(())
    }
}

/// What emptying a folder deletes: its entries and every file and link
/// below them, listed without following links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgePlan {
    /// The folder emptied (it stays), by its real path.
    pub folder: PathBuf,
    /// Its entries, sorted.
    pub entries: Vec<PathBuf>,
    /// Every file and link at or below the entries, sorted; links are
    /// deleted themselves, never what they point to.
    pub files: Vec<PathBuf>,
}

/// Lists what [`Journal::purge`] would delete in `folder`. The folder must
/// be given by its real path (absolute, no `..`, no link in it or on the
/// way to it) and be a folder, not a link; nothing below it is followed.
pub fn plan_purge(folder: &Path) -> Result<PurgePlan> {
    ensure!(
        folder.is_absolute(),
        "{} is not an absolute path",
        folder.display()
    );
    let meta = fs::symlink_metadata(folder)
        .with_context(|| format!("{} was not found", folder.display()))?;
    ensure!(
        !meta.file_type().is_symlink(),
        "{} is a link; only a real folder can be emptied",
        folder.display()
    );
    ensure!(meta.is_dir(), "{} is not a folder", folder.display());
    let real = folder
        .canonicalize()
        .with_context(|| format!("Cannot resolve {}", folder.display()))?;
    ensure!(
        real == folder,
        "{} must be given by its real path ({})",
        folder.display(),
        real.display()
    );
    let mut entries: Vec<PathBuf> = fs::read_dir(folder)?
        .map(|entry| entry.map(|e| folder.join(e.file_name())))
        .collect::<io::Result<_>>()?;
    entries.sort();
    let mut files = Vec::new();
    for entry in &entries {
        list_files(entry, &mut files)?;
    }
    files.sort();
    Ok(PurgePlan {
        folder: folder.to_path_buf(),
        entries,
        files,
    })
}

fn list_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    // Not following links: a link to a folder is listed as a file.
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            list_files(&path.join(entry?.file_name()), files)?;
        }
    } else {
        files.push(path.to_path_buf());
    }
    Ok(())
}

/// Whether `entry` is a single named entry of `folder`.
fn directly_in(entry: &Path, folder: &Path) -> bool {
    use std::path::Component;
    let Ok(below) = entry.strip_prefix(folder) else {
        return false;
    };
    let mut parts = below.components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Deletes a committed purge's entries from its hidden folder. Checks the
/// journal's paths first (the hidden folder directly in the emptied one,
/// each entry directly in that), so that nothing else is ever deleted.
fn delete_staged(payload: &Payload) -> Result<()> {
    let folder = payload
        .purge_folder
        .as_deref()
        .context("The purge has no folder")?;
    let mut first_error = None;
    for step in &payload.steps {
        let Target::Path { path } = &step.to else {
            bail!("A purge step leads to the system Trash");
        };
        let staging = path.parent().context("A purge entry has no folder")?;
        ensure!(
            directly_in(staging, folder)
                && staging
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(PURGE_STAGING_PREFIX))
                && directly_in(path, staging)
                && directly_in(&step.from, folder),
            "{} is not an entry set aside in {}; it was not deleted",
            path.display(),
            folder.display()
        );
        if let Err(e) = remove_tree(path) {
            first_error.get_or_insert(
                anyhow::Error::new(e).context(format!("Cannot delete {}", path.display())),
            );
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Deletes a file, a link (not what it points to) or a folder with
/// everything in it (links inside are deleted, never followed). Already
/// gone is fine.
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        // A link to a folder is a directory entry on Windows.
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::remove_file(path).or_else(|e| fs::remove_dir(path).map_err(|_| e))
        }
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn validate(steps: &[Step]) -> Result<()> {
    let mut sources = HashSet::new();
    let mut targets = HashSet::new();
    for step in steps {
        ensure!(
            step.from.is_absolute(),
            "{} is not an absolute path",
            step.from.display()
        );
        ensure!(
            sources.insert(step.from.clone()),
            "{} appears twice",
            step.from.display()
        );
        ensure!(exists(&step.from), "{} was not found", step.from.display());
        if let Target::Path { path } = &step.to {
            ensure!(
                path.is_absolute(),
                "{} is not an absolute path",
                path.display()
            );
            ensure!(
                targets.insert(path.clone()),
                "Two files would be named {}",
                path.display()
            );
            ensure!(path != &step.from, "{} would not move", path.display());
            ensure!(
                !path.starts_with(&step.from),
                "{} cannot be moved into itself",
                step.from.display()
            );
            ensure!(
                !exists(path) || same_file(&step.from, path),
                "{} already exists",
                path.display()
            );
            ensure!(
                same_volume(&step.from, path)?,
                "{} is on another volume than {}; moving between volumes is not supported yet",
                path.display(),
                step.from.display()
            );
        }
    }
    // A target that is another step's source would depend on step order.
    for target in &targets {
        ensure!(
            !sources.contains(target),
            "{} is both moved and replaced in one operation",
            target.display()
        );
    }
    Ok(())
}

/// Creates missing parent folders of the targets; returns them outermost first.
fn create_target_parents(steps: &[Step]) -> Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    for step in steps {
        let Target::Path { path } = &step.to else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        let mut missing: Vec<&Path> = parent.ancestors().take_while(|p| !exists(p)).collect();
        missing.reverse();
        for dir in missing {
            fs::create_dir(dir)
                .with_context(|| format!("Cannot create the folder {}", dir.display()))?;
            created.push(dir.to_path_buf());
        }
    }
    Ok(created)
}

fn remove_created_dirs(dirs: &[PathBuf]) {
    for dir in dirs.iter().rev() {
        // Only empty folders go; anything placed there since stays.
        let _ = fs::remove_dir(dir);
    }
}

fn apply(step: &mut Step, trash: &dyn SystemTrash) -> Result<()> {
    match &mut step.to {
        Target::Path { path } => rename_no_clobber(&step.from, path)
            .with_context(|| format!("Cannot move {} to {}", step.from.display(), path.display())),
        Target::SystemTrash { item } => {
            *item =
                Some(trash.trash(&step.from).with_context(|| {
                    format!("Cannot move {} to the Trash", step.from.display())
                })?);
            Ok(())
        }
    }
}

fn reverse(step: &Step, trash: &dyn SystemTrash) -> Result<()> {
    match &step.to {
        Target::Path { path } => rename_no_clobber(path, &step.from).with_context(|| {
            format!(
                "Cannot move {} back to {}",
                path.display(),
                step.from.display()
            )
        }),
        Target::SystemTrash { item } => {
            let item = item.as_deref().context("Trashed item was not recorded")?;
            if let Some(parent) = step.from.parent() {
                fs::create_dir_all(parent)?;
            }
            trash
                .restore(item, &step.from)
                .with_context(|| format!("Cannot put {} back from the Trash", step.from.display()))
        }
    }
}

fn reverse_steps(steps: &[Step], trash: &dyn SystemTrash) -> Result<()> {
    let mut first_error = None;
    for step in steps.iter().rev() {
        if let Err(e) = reverse(step, trash) {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Recovery: reverses a step only where the file system shows it applied.
fn restore_if_moved(step: &Step, trash: &dyn SystemTrash) -> Result<()> {
    if exists(&step.from) {
        return Ok(());
    }
    match &step.to {
        Target::Path { path } if exists(path) => reverse(step, trash),
        Target::Path { path } => bail!(
            "neither {} nor {} exists",
            step.from.display(),
            path.display()
        ),
        Target::SystemTrash { item: Some(item) } if trash.contains(item) => reverse(step, trash),
        Target::SystemTrash { .. } => bail!(
            "{} may be in the system Trash; put it back from there",
            step.from.display()
        ),
    }
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    // Case-only renames on case-insensitive volumes.
    a.parent() == b.parent()
        && a.file_name().map(|n| n.to_string_lossy().to_lowercase())
            == b.file_name().map(|n| n.to_string_lossy().to_lowercase())
        && exists(a)
}

/// The volume of `path`'s nearest existing ancestor.
#[cfg(unix)]
fn same_volume(from: &Path, to: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let source = fs::symlink_metadata(from)?.dev();
    let existing = to
        .ancestors()
        .skip(1)
        .find(|p| exists(p))
        .context("Target has no existing folder")?;
    Ok(fs::metadata(existing)?.dev() == source)
}

#[cfg(not(unix))]
fn same_volume(from: &Path, to: &Path) -> Result<bool> {
    use std::path::Component;
    let prefix = |p: &Path| match p.components().next() {
        Some(Component::Prefix(prefix)) => Some(prefix.as_os_str().to_ascii_lowercase()),
        _ => None,
    };
    Ok(prefix(from) == prefix(to))
}

/// Renames `from` to `to`, failing if `to` exists (except for a case-only
/// rename of the same file). Atomic on macOS and Linux.
pub fn rename_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
    if same_file(from, to) {
        return fs::rename(from, to);
    }
    #[cfg(target_vendor = "apple")]
    {
        let (f, t) = (c_path(from)?, c_path(to)?);
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOTSUP) {
            return Err(error);
        }
    }
    #[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
    {
        let (f, t) = (c_path(from)?, c_path(to)?);
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                f.as_ptr(),
                libc::AT_FDCWD,
                t.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(error);
        }
    }
    // Volumes without an exclusive rename: check, then rename.
    if exists(to) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    fs::rename(from, to)
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains a NUL byte"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_with(root: &Path, state: State, steps: Vec<Step>) -> (Journal, String) {
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        let payload = Payload {
            steps,
            ..Payload::default()
        };
        let id = "interrupted".to_string();
        journal
            .conn
            .execute(
                "INSERT INTO operations(id,kind,label,state,payload) VALUES(?,?,?,?,?)",
                params![
                    id,
                    "move",
                    "Move",
                    state.as_str(),
                    serde_json::to_string(&payload).unwrap()
                ],
            )
            .unwrap();
        (journal, id)
    }

    #[test]
    fn recovery_rolls_back_an_operation_a_crash_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for name in ["a", "b"] {
            fs::write(root.join(name), name).unwrap();
        }
        fs::create_dir(root.join("sub")).unwrap();
        let (journal, id) = journal_with(
            &root,
            State::Applying,
            vec![
                Step::rename(root.join("a"), root.join("sub/a")),
                Step::rename(root.join("b"), root.join("sub/b")),
            ],
        );
        // The crash came after the first step.
        fs::rename(root.join("a"), root.join("sub/a")).unwrap();
        let pending = journal.recover(&NoSystemTrash).unwrap();
        assert!(pending.is_empty(), "rolled back ops need no sync");
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
        let op = journal.operation(&id).unwrap();
        assert_eq!(op.state, State::RolledBack);
        assert!(op.synced);
    }

    #[test]
    fn recovery_finishes_an_interrupted_undo_and_asks_for_a_sync() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        // Undo had moved "b" back (it runs in reverse) but not "a".
        fs::write(root.join("sub/a"), "a").unwrap();
        fs::write(root.join("b"), "b").unwrap();
        let (journal, id) = journal_with(
            &root,
            State::Undoing,
            vec![
                Step::rename(root.join("a"), root.join("sub/a")),
                Step::rename(root.join("b"), root.join("sub/b")),
            ],
        );
        let pending = journal.recover(&NoSystemTrash).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].state, State::Undone);
        assert!(root.join("a").is_file() && root.join("b").is_file());
    }

    #[test]
    fn recovery_reports_an_unrecorded_trash_step() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // The file went to the system Trash, the crash came before its item
        // was journaled.
        let (journal, id) = journal_with(
            &root,
            State::Applying,
            vec![Step::system_trash(root.join("gone.RAF"))],
        );
        journal.recover(&NoSystemTrash).unwrap();
        let op = journal.operation(&id).unwrap();
        assert_eq!(op.state, State::RolledBack);
        assert!(op.note.unwrap().contains("put it back from there"));
    }

    /// A crash during a redo: the steps it applied go back, the operation
    /// is undone again (and still redoable), the records need no sync.
    #[test]
    fn recovery_puts_an_interrupted_redo_back_to_undone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for name in ["a", "b"] {
            fs::write(root.join(name), name).unwrap();
        }
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        let steps = vec![
            Step::rename(root.join("a"), root.join("sub/a")),
            Step::rename(root.join("b"), root.join("sub/b")),
        ];
        let op = journal
            .execute("move", "Move", steps, &NoSystemTrash)
            .unwrap();
        journal.undo(&op.id, &NoSystemTrash).unwrap();
        journal.mark_synced(&op.id).unwrap();
        // The redo journaled its plan, made the folder, moved "a", then
        // the power went.
        let mut payload = journal.payload(&op.id).unwrap();
        payload.created_dirs = create_target_parents(&payload.steps).unwrap();
        journal
            .store_keeping_sync(&op.id, State::Redoing, &payload)
            .unwrap();
        fs::rename(root.join("a"), root.join("sub/a")).unwrap();
        drop(journal);

        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        let pending = journal.recover(&NoSystemTrash).unwrap();
        assert!(pending.is_empty(), "records already follow undone");
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
        assert!(!root.join("sub").exists());
        let now = journal.operation(&op.id).unwrap();
        assert_eq!(now.state, State::Undone);
        assert_eq!(journal.last_redoable().unwrap().unwrap().id, op.id);
        // And it can be redone after all.
        journal.redo(&op.id, &NoSystemTrash).unwrap();
        assert!(root.join("sub/a").is_file() && root.join("sub/b").is_file());
    }

    /// A crash while the entries are being set aside: they go back, nothing
    /// is deleted, Undo of earlier operations stays.
    #[test]
    fn recovery_puts_back_an_interrupted_purge_before_its_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let trash = root.join("Trash");
        fs::create_dir_all(trash.join("Capture")).unwrap();
        fs::write(trash.join("Capture/A.RAF"), "a").unwrap();
        fs::write(trash.join("B.RAF"), "b").unwrap();
        fs::write(root.join("x"), "x").unwrap();
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        let earlier = journal
            .execute(
                "rename",
                "x→y",
                vec![Step::rename(root.join("x"), root.join("y"))],
                &NoSystemTrash,
            )
            .unwrap();
        let plan = plan_purge(&trash).unwrap();
        let staging = trash.join(format!("{PURGE_STAGING_PREFIX}crash"));
        let steps: Vec<Step> = plan
            .entries
            .iter()
            .map(|e| Step::rename(e, staging.join(e.file_name().unwrap())))
            .collect();
        let payload = Payload {
            created_dirs: create_target_parents(&steps).unwrap(),
            steps,
            purge_folder: Some(trash.clone()),
            removed: plan.files.clone(),
            ..Payload::default()
        };
        journal
            .insert(
                "crash",
                "emptyTrash",
                "Empty Trash",
                State::Purging,
                &payload,
            )
            .unwrap();
        fs::rename(trash.join("B.RAF"), staging.join("B.RAF")).unwrap();

        let pending = journal.recover(&NoSystemTrash).unwrap();
        assert!(
            pending.iter().all(|op| op.id != "crash"),
            "nothing to forget"
        );
        assert_eq!(fs::read_to_string(trash.join("B.RAF")).unwrap(), "b");
        assert!(trash.join("Capture/A.RAF").is_file());
        assert!(!staging.exists());
        assert_eq!(journal.operation("crash").unwrap().state, State::RolledBack);
        assert_eq!(journal.last_undoable().unwrap().unwrap().id, earlier.id);
    }

    /// A crash after the commit, with some entries deleted: recovery
    /// deletes the rest and hands the purge to the caller to forget its
    /// records; Undo before it is over.
    #[test]
    fn recovery_finishes_a_committed_purge_a_crash_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let trash = root.join("Trash");
        fs::create_dir_all(trash.join("Capture")).unwrap();
        fs::write(trash.join("Capture/A.RAF"), "a").unwrap();
        fs::write(trash.join("B.RAF"), "b").unwrap();
        fs::write(root.join("keep.RAF"), "keep").unwrap();
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        let plan = plan_purge(&trash).unwrap();
        let staging = trash.join(format!("{PURGE_STAGING_PREFIX}crash"));
        let steps: Vec<Step> = plan
            .entries
            .iter()
            .map(|e| Step::rename(e, staging.join(e.file_name().unwrap())))
            .collect();
        let created_dirs = create_target_parents(&steps).unwrap();
        for step in &steps {
            let Target::Path { path } = &step.to else {
                unreachable!()
            };
            fs::rename(&step.from, path).unwrap();
        }
        let payload = Payload {
            steps,
            created_dirs,
            purge_folder: Some(trash.clone()),
            removed: plan.files.clone(),
            ..Payload::default()
        };
        journal
            .insert(
                "crash",
                "emptyTrash",
                "Empty Trash",
                State::PurgeCommitted,
                &payload,
            )
            .unwrap();
        // One entry was deleted before the crash.
        fs::remove_file(staging.join("B.RAF")).unwrap();

        let pending = journal.recover(&NoSystemTrash).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, State::Purged);
        assert_eq!(pending[0].removed, plan.files);
        assert!(!staging.exists());
        assert!(trash.is_dir());
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(root.join("keep.RAF")).unwrap(), "keep");
        // Recovering again is harmless.
        journal.recover(&NoSystemTrash).unwrap();
        assert_eq!(journal.operation("crash").unwrap().state, State::Purged);
    }

    /// A journal whose purge row names paths outside its folder (damaged
    /// or edited) deletes nothing.
    #[test]
    fn a_committed_purge_never_deletes_outside_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let trash = root.join("Trash");
        fs::create_dir_all(&trash).unwrap();
        fs::write(root.join("A.RAF"), "original").unwrap();
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        for target in [
            root.join("A.RAF"),
            trash.join("not-staging/A.RAF"),
            trash.join(format!("{PURGE_STAGING_PREFIX}x/../../A.RAF")),
        ] {
            let payload = Payload {
                steps: vec![Step::rename(trash.join("A.RAF"), &target)],
                purge_folder: Some(trash.clone()),
                ..Payload::default()
            };
            let id = uuid::Uuid::new_v4().to_string();
            journal
                .insert(&id, "emptyTrash", "Empty", State::PurgeCommitted, &payload)
                .unwrap();
            journal.recover(&NoSystemTrash).unwrap();
            let op = journal.operation(&id).unwrap();
            assert_eq!(op.state, State::PurgeCommitted, "{target:?}");
            assert!(op.note.unwrap().contains("not deleted"));
        }
        assert_eq!(fs::read_to_string(root.join("A.RAF")).unwrap(), "original");
    }

    /// A version 1 journal gains the redo stack; its undone operations are
    /// not redoable (nothing offered them).
    #[test]
    fn a_version_1_journal_is_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE operations(
                   seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                   kind TEXT NOT NULL, label TEXT NOT NULL, state TEXT NOT NULL,
                   synced INTEGER NOT NULL DEFAULT 0, payload TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO operations(id,kind,label,state,synced,payload)
                   VALUES('old','rename','Rename','undone',1,'{\"steps\":[]}');
                 PRAGMA user_version=1;",
            )
            .unwrap();
        }
        let journal = Journal::open(&path).unwrap();
        let version: u32 = journal
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        assert_eq!(journal.operation("old").unwrap().state, State::Undone);
        assert!(journal.last_redoable().unwrap().is_none());
        drop(journal);
        Journal::open(&path).unwrap();
    }
}
