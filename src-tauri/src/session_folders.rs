//! A session's Capture, Selects and Output folders (G38): the settings live
//! in `rawtonic_catalog::session_folders`; this is the glue for the session
//! settings, "Move contents" through the journaled file operations, and the
//! folders' use by import, "Move to Selects" and export.
use std::path::{Path, PathBuf};

use rawtonic_catalog::Library;
use rawtonic_catalog::session_folders::{FolderRole, SessionFolder};
use serde::Serialize;
use tauri::AppHandle;

use crate::file_operations::OperationSummary;

fn blocking_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The session's three folders, where they are and whether they are there.
#[tauri::command]
pub async fn session_folders(directory: String) -> Result<Vec<SessionFolder>, String> {
    crate::session_bridge::with_library(directory, false, |lib| Ok(lib.session_folders()))
        .await
        .map_err(blocking_err)?
}

/// The folder with `role`, to put files in now: made again inside the
/// session when it was removed; a folder outside that is not there fails
/// with a message naming it (nothing is made in its place).
#[tauri::command]
pub async fn session_usable_folder(directory: String, role: FolderRole) -> Result<PathBuf, String> {
    crate::session_bridge::with_library(directory, false, move |lib| {
        lib.usable_folder(role).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(blocking_err)?
}

/// What choosing a folder did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderChange {
    pub folder: SessionFolder,
    /// The folder it was before (whose contents "Move contents" moves).
    pub previous: SessionFolder,
}

/// Makes `path` the session's folder with `role`; no file moves. With
/// `move_contents` the move of the old folder's contents is planned first,
/// so a name clash (or a link in it) refuses the change before anything
/// happens; the webview then runs the moves one undoable operation at a
/// time (`session_move_folder_contents`).
#[tauri::command]
pub async fn session_set_folder(
    directory: String,
    role: FolderRole,
    path: String,
    move_contents: bool,
    app: AppHandle,
) -> Result<FolderChange, String> {
    crate::file_management::blocking(move || {
        let directory = Path::new(&directory);
        let change = {
            let _turn = crate::session_bridge::write_turn();
            let mut lib = Library::open(directory).map_err(|e| format!("{e:#}"))?;
            let previous = lib.session_folder(role);
            if move_contents && previous.available {
                rawtonic_fileops::plan_move_contents(&previous.path, Path::new(&path))
                    .map_err(|e| format!("{e:#}"))?;
            }
            let folder = lib
                .set_session_folder(role, Path::new(&path))
                .map_err(|e| format!("{e:#}"))?;
            FolderChange { folder, previous }
        };
        // A folder chosen now is the user's: no earlier Move contents
        // gives the role elsewhere on its redo.
        crate::file_operations::with_journal_pub(&app, |journal| {
            chosen_by_hand(journal, directory, role);
            Ok(())
        })?;
        if crate::session_bridge::active_library_directory().as_deref() == Some(directory) {
            crate::session_bridge::register_outside_folders(directory);
        }
        Ok(change)
    })
    .await?
}

/// Moves the next part of `from`'s contents into `to` as one journaled,
/// undoable operation: its files and the folders on `to`'s drive, else the
/// next folder going to another drive (an operation of its own). `None`
/// when `from` is empty. Nothing moves when a name in `from` is taken in
/// `to`.
#[tauri::command]
pub async fn session_move_folder_contents(
    from: String,
    to: String,
    app: AppHandle,
) -> Result<Option<OperationSummary>, String> {
    crate::file_management::blocking(move || {
        let plan = rawtonic_fileops::plan_move_contents(Path::new(&from), Path::new(&to))
            .map_err(|e| format!("{e:#}"))?;
        let Some(steps) = plan.into_iter().next() else {
            return Ok(None);
        };
        let name = Path::new(&to)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let context = crate::session_bridge::active_library_directory()
            .and_then(|session| move_context(&session, Path::new(&from), Path::new(&to)));
        let op = crate::file_operations::run_with_context(
            &app,
            "move",
            &format!("Move contents to {name}"),
            steps,
            context,
        )?;
        Ok(Some((&op).into()))
    })
    .await?
}

/// The key of a Move contents operation's journal context: the folder role
/// that changed with it (`move_context`).
pub const CONTEXT_KEY: &str = "sessionFolder";

/// The journal context of a part of "Move contents" from `from` to `to`:
/// the session's role whose folder `to` now is (chosen just before the
/// move, `session_set_folder`), so that undoing the move gives the role
/// back to `from` and redoing it gives it to `to` again (`follow`). None
/// when no role of the session at `session` has `to`.
pub fn move_context(session: &Path, from: &Path, to: &Path) -> Option<serde_json::Value> {
    let lib = Library::open(session).ok()?;
    let to = canonical(to);
    let role = FolderRole::ALL
        .into_iter()
        .find(|role| canonical(&lib.session_folder(*role).path) == to)?;
    Some(serde_json::json!({
        "session": lib.directory,
        "role": role,
        "from": canonical(from),
        "to": to,
    }))
}

fn canonical(path: &Path) -> PathBuf {
    rawtonic_fs::Canonical::canonical(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A folder chosen for `role` by the user in the session at `session`:
/// the Move contents operations whose undo gave that role back no longer
/// give it away on their redo (`follow`), whatever folder it is now.
pub fn chosen_by_hand(journal: &rawtonic_fileops::Journal, session: &Path, role: FolderRole) {
    let session = canonical(session);
    let Ok(history) = journal.history(1000) else {
        return;
    };
    for op in history {
        let Some(mut context) = op.context else {
            continue;
        };
        let Some(change) = context
            .get(CONTEXT_KEY)
            .and_then(|c| serde_json::from_value::<Change>(c.clone()).ok())
        else {
            continue;
        };
        if change.restored && change.role == role && canonical(&change.session) == session {
            context[CONTEXT_KEY]["restored"] = serde_json::json!(false);
            if let Err(e) = journal.set_context(&op.id, context) {
                log::warn!("Move contents: a folder choice was not recorded: {e}");
            }
        }
    }
}

/// What `follow` did to a session's folder role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The operation changed no role, or the role is not where it left it.
    Unchanged,
    /// The role followed the undo or redo.
    Changed { session: PathBuf },
    /// The role could not follow (e.g. the old folder has another role
    /// now): the files moved all the same; the role stays as it is.
    Refused { session: PathBuf, error: String },
}

#[derive(serde::Deserialize)]
struct Change {
    session: PathBuf,
    role: FolderRole,
    from: PathBuf,
    to: PathBuf,
    /// Whether the operation's latest undo gave the role back (only then
    /// does its redo give it to `to` again).
    #[serde(default)]
    restored: bool,
}

/// The role a "Move contents" part went with follows its undo (back to the
/// folder the contents came from) and its redo (to where they went again),
/// in the session's settings. An undo changes the role only while it is
/// still on the new folder, and a redo only when that undo changed it and
/// the role is still on the old folder: a folder chosen in between keeps
/// the role. A role that cannot follow is refused (named for the user), not
/// a failure of the operation's records. Running it again changes nothing
/// more (the catalog's sync may retry).
pub fn follow(
    journal: &rawtonic_fileops::Journal,
    op: &rawtonic_fileops::Operation,
    forward: bool,
) -> Followed {
    // The newest context: an earlier run of this may have recorded `restored`.
    let context = journal
        .operation(&op.id)
        .ok()
        .and_then(|op| op.context)
        .or_else(|| op.context.clone());
    let Some(mut context) = context else {
        return Followed::Unchanged;
    };
    let Some(change) = context
        .get(CONTEXT_KEY)
        .and_then(|c| serde_json::from_value::<Change>(c.clone()).ok())
    else {
        return Followed::Unchanged;
    };
    let _turn = crate::session_bridge::write_turn();
    let Ok(mut lib) = Library::open(&change.session) else {
        return Followed::Unchanged;
    };
    let now = canonical(&lib.session_folder(change.role).path);
    let (wanted, restored) = match forward {
        true if change.restored && now == change.from => (Some(&change.to), false),
        true => (None, false),
        false if now == change.to => (Some(&change.from), true),
        // Given back already by an earlier run of this undo.
        false if now == change.from => (None, change.restored),
        false => (None, false),
    };
    let result = match wanted {
        Some(folder) => lib
            .set_session_folder(change.role, folder)
            .map(|_| ())
            .map_err(|e| format!("{e:#}")),
        None => Ok(()),
    };
    drop(lib);
    let restored = restored && result.is_ok();
    if restored != change.restored {
        context[CONTEXT_KEY]["restored"] = serde_json::json!(restored);
        if let Err(e) = journal.set_context(&op.id, context) {
            log::warn!("Move contents: the folder role's undo state was not kept: {e}");
        }
    }
    match (wanted, result) {
        (None, _) => Followed::Unchanged,
        (Some(_), Ok(())) => {
            if crate::session_bridge::active_library_directory().as_deref() == Some(&change.session)
            {
                crate::session_bridge::register_outside_folders(&change.session);
            }
            Followed::Changed {
                session: change.session,
            }
        }
        (Some(_), Err(error)) => Followed::Refused {
            session: change.session,
            error,
        },
    }
}
