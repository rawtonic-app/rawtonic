//! Journaled, undoable file operations: rename, move and trash.
//!
//! Every operation is planned completely, validated, written to a SQLite
//! journal and only then carried out, one no-clobber rename at a time. A
//! failure rolls the applied steps back; an interrupted operation (crash,
//! power loss) is rolled back by [`Journal::recover`] on the next start. A
//! finished operation can be undone as long as nothing has taken its place.
//!
//! Nothing here deletes a file. "Trash" is either a rename into a folder the
//! caller chooses (a session's own `Trash/`) or the system Trash through a
//! [`SystemTrash`] the caller provides, which must be able to put items back.
//! Moving between volumes would need a copy and a delete and is refused.
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
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Applying => "applying",
            State::Done => "done",
            State::Undoing => "undoing",
            State::Undone => "undone",
            State::RolledBack => "rolledBack",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "applying" => State::Applying,
            "done" => State::Done,
            "undoing" => State::Undoing,
            "undone" => State::Undone,
            "rolledBack" => State::RolledBack,
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

const JOURNAL_VERSION: u32 = 1;

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
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             PRAGMA user_version=1;",
        )?;
        Ok(Self { conn })
    }

    /// Validates the whole plan, journals it and carries it out. On failure
    /// the applied steps are reversed and the error is returned; the journal
    /// keeps the operation as rolled back.
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
            note: None,
            context: None,
        };
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO operations(id,kind,label,state,payload) VALUES(?,?,?,?,?)",
            params![
                id,
                kind,
                label,
                State::Applying.as_str(),
                serde_json::to_string(&payload)?
            ],
        )?;
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
        self.store(&id, State::Done, &payload)?;
        self.operation(&id)
    }

    /// Undoes a finished operation. Refuses, changing nothing, when any file
    /// has moved on since or something now occupies an original location.
    pub fn undo(&self, id: &str, trash: &dyn SystemTrash) -> Result<Operation> {
        let op = self.operation(id)?;
        ensure!(
            op.state == State::Done,
            "Only finished operations can be undone"
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
            note: None,
            context: op.context.clone(),
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
        self.store(id, State::Undone, &payload)?;
        self.operation(id)
    }

    /// The newest finished operation, the one Undo applies to.
    pub fn last_undoable(&self) -> Result<Option<Operation>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM operations WHERE state='done' ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.operation(&id)).transpose()
    }

    /// Rolls back operations a crash interrupted and returns every operation
    /// whose caller records still need to follow it (including those).
    pub fn recover(&self, trash: &dyn SystemTrash) -> Result<Vec<Operation>> {
        let ids: Vec<String> = {
            let mut q = self.conn.prepare(
                "SELECT id FROM operations WHERE state IN ('applying','undoing') ORDER BY seq",
            )?;
            q.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in ids {
            let op = self.operation(&id)?;
            let mut payload = Payload {
                steps: op.steps.clone(),
                created_dirs: op.created_dirs.clone(),
                note: None,
                context: op.context.clone(),
            };
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
            let state = if op.state == State::Undoing {
                State::Undone
            } else {
                State::RolledBack
            };
            self.store(&id, state, &payload)?;
            // An interrupted undo may have reversed steps the caller's
            // records still show as done.
            self.set_synced(&id, op.state != State::Undoing)?;
        }
        self.unsynced()
    }

    /// Finished or undone operations the caller's records do not follow yet
    /// (their sync failed or never ran), oldest first: sync them in order.
    pub fn unsynced(&self) -> Result<Vec<Operation>> {
        let ids: Vec<String> = {
            let mut q = self.conn.prepare(
                "SELECT id FROM operations WHERE synced=0 AND state IN ('done','undone') ORDER BY seq",
            )?;
            q.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.operation(id)).collect()
    }

    /// Stores the caller's data with an operation (kept through undo).
    pub fn set_context(&self, id: &str, context: serde_json::Value) -> Result<()> {
        let payload: String =
            self.conn
                .query_row("SELECT payload FROM operations WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
        let mut payload: Payload = serde_json::from_str(&payload)?;
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
            synced,
            created_at,
        })
    }

    fn store(&self, id: &str, state: State, payload: &Payload) -> Result<()> {
        // A changed state invalidates what the caller synced.
        self.conn.execute(
            "UPDATE operations SET state=?, payload=?, synced=0 WHERE id=?",
            params![state.as_str(), serde_json::to_string(payload)?, id],
        )?;
        Ok(())
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
            created_dirs: vec![],
            note: None,
            context: None,
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
}
