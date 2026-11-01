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
    /// The moves of the old folder's contents ("Move contents"), each one
    /// undo step: one for the files, one per subfolder going to another drive.
    pub operations: Vec<OperationSummary>,
    /// Why the contents were not (all) moved; the folder is changed anyway.
    pub move_error: Option<String>,
}

/// Makes `path` the session's folder with `role`. With `move_contents` the
/// old folder's entries then move into it with the journaled file
/// operations (verified copies to another drive, undoable); without, no
/// file moves. The move is planned before the folder changes, so a name
/// clash refuses both.
#[tauri::command]
pub async fn session_set_folder(
    directory: String,
    role: FolderRole,
    path: String,
    move_contents: bool,
    app: AppHandle,
) -> Result<FolderChange, String> {
    crate::file_management::blocking(move || {
        set_folder(
            &app,
            Path::new(&directory),
            role,
            Path::new(&path),
            move_contents,
        )
    })
    .await?
}

fn set_folder(
    app: &AppHandle,
    directory: &Path,
    role: FolderRole,
    path: &Path,
    move_contents: bool,
) -> Result<FolderChange, String> {
    let (folder, plan) = {
        let _turn = crate::session_bridge::write_turn();
        let mut lib = Library::open(directory).map_err(|e| format!("{e:#}"))?;
        let old = lib.session_folder(role);
        let plan = if move_contents && old.available {
            rawtonic_fileops::plan_move_contents(&old.path, path).map_err(|e| format!("{e:#}"))?
        } else {
            Vec::new()
        };
        let folder = lib
            .set_session_folder(role, path)
            .map_err(|e| format!("{e:#}"))?;
        (folder, plan)
    };
    if crate::session_bridge::active_library_directory().as_deref() == Some(directory) {
        crate::session_bridge::register_outside_folders(directory);
    }
    let name = folder
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut operations = Vec::new();
    let mut move_error = None;
    for steps in plan {
        let label = format!("Move contents to {name}");
        match crate::file_operations::run(app, "move", &label, steps) {
            Ok(op) => operations.push((&op).into()),
            Err(e) => {
                move_error = Some(e);
                break;
            }
        }
    }
    Ok(FolderChange {
        folder,
        operations,
        move_error,
    })
}
