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
        let op =
            crate::file_operations::run(&app, "move", &format!("Move contents to {name}"), steps)?;
        Ok(Some((&op).into()))
    })
    .await?
}
