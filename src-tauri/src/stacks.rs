//! Stacks (G36): the Tauri commands over `rawtonic_catalog::stacks`. Photos
//! are named by their paths as the webview lists them (a virtual copy as
//! `<path>?vc=<id>`); every command works in the library that manages the
//! first of them and returns the `StackChange` the webview keeps as one
//! undo step (`utils/appUndo`), handed back to `stacks_restore`. No file or
//! sidecar is read or written.
use crate::file_management::parse_virtual_path;
use crate::ipc::CommandError;
use rawtonic_catalog::stacks::{AutoGroup, AutoPlan, AutoStackOptions, Member, StackChange};
use rawtonic_catalog::{CatalogError, Library};
use rawtonic_metadata::photo_ref::{self, PhotoRef};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// A stack as the webview lists it: member paths, the top first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebStack {
    pub id: String,
    pub collapsed: bool,
    pub members: Vec<String>,
}

/// What a stack command did, in which library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackResult {
    pub library: String,
    pub change: StackChange,
}

/// The webview's path of a member.
fn web_path(path: &Path, copy: &str) -> String {
    PhotoRef::of_path(path, (!copy.is_empty()).then_some(copy)).format()
}

/// The refusal when none of the paths lies in a library.
const NOT_IN_A_LIBRARY: &str = "These photos are not in a library";

/// The library of the first of `paths` and the members among `paths` it
/// holds (others, and photos of other libraries, are left out).
fn members(paths: &[String]) -> Result<(PathBuf, Library, Vec<Member>), CommandError> {
    let mut found: Option<(PathBuf, Library)> = None;
    let mut members = Vec::new();
    for path in paths {
        let copy = photo_ref::split(path).1.unwrap_or_default().to_string();
        let (source, _) = parse_virtual_path(path);
        let Some(directory) = crate::session_bridge::library_dir_for(&source) else {
            continue;
        };
        if found.is_none() {
            let lib = Library::open(&directory)?;
            found = Some((directory.clone(), lib));
        }
        let Some((owner, lib)) = found.as_ref() else {
            continue;
        };
        if *owner != directory {
            continue;
        }
        if let Some(asset) = lib.asset_at(&source)? {
            members.push(Member {
                asset_id: asset.id,
                copy,
            });
        }
    }
    let (directory, lib) = found.ok_or_else(|| CommandError::not_found(NOT_IN_A_LIBRARY))?;
    Ok((directory, lib, members))
}

fn run(
    paths: Vec<String>,
    command: impl FnOnce(&Library, &[Member]) -> Result<StackChange, CatalogError>,
) -> Result<StackResult, CommandError> {
    let (directory, lib, members) = members(&paths)?;
    let change = command(&lib, &members)?;
    Ok(StackResult {
        library: directory.to_string_lossy().into_owned(),
        change,
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CommandError> + Send + 'static,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| CommandError::internal(format!("Task failed: {e}")))?
}

fn open(library: &str) -> Result<Library, CommandError> {
    Ok(Library::open(Path::new(library))?)
}

/// Every stack of the library at `directory`.
#[tauri::command]
pub async fn stacks_list(directory: String) -> Result<Vec<WebStack>, CommandError> {
    blocking(move || {
        let lib = open(&directory)?;
        Ok(lib
            .listed_stacks()?
            .into_iter()
            .map(|stack| WebStack {
                id: stack.id,
                collapsed: stack.collapsed,
                members: stack
                    .members
                    .iter()
                    .map(|m| web_path(&m.path, &m.copy))
                    .collect(),
            })
            .collect())
    })
    .await
}

/// Group into Stack; the first path is the top.
#[tauri::command]
pub async fn stacks_group(paths: Vec<String>) -> Result<StackResult, CommandError> {
    blocking(move || run(paths, |lib, m| lib.group_stack(m))).await
}

#[tauri::command]
pub async fn stacks_unstack(paths: Vec<String>) -> Result<StackResult, CommandError> {
    blocking(move || run(paths, |lib, m| lib.unstack(m))).await
}

#[tauri::command]
pub async fn stacks_remove(paths: Vec<String>) -> Result<StackResult, CommandError> {
    blocking(move || run(paths, |lib, m| lib.remove_from_stack(m))).await
}

#[tauri::command]
pub async fn stacks_move_to_top(path: String) -> Result<StackResult, CommandError> {
    blocking(move || {
        run(vec![path], |lib, m| match m.first() {
            Some(member) => lib.move_to_stack_top(member),
            None => Ok(StackChange::default()),
        })
    })
    .await
}

/// Split Stack: the first path that is a member below its stack's top
/// splits its stack there; the change is one undo step (`done`, false when
/// no path applied).
#[tauri::command]
pub async fn stacks_split(paths: Vec<String>) -> Result<SplitResult, CommandError> {
    blocking(move || {
        let (directory, lib, members) = members(&paths)?;
        let mut done = false;
        let mut change = StackChange::default();
        for member in &members {
            if lib.can_split_stack(member)? {
                (change, done) = lib.split_stack(member)?;
                break;
            }
        }
        Ok(SplitResult {
            library: directory.to_string_lossy().into_owned(),
            change,
            done,
        })
    })
    .await
}

/// What Split Stack did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitResult {
    pub library: String,
    pub change: StackChange,
    pub done: bool,
}

/// Move Up in Stack (`up`) or Move Down in Stack: the first path that has a
/// neighbour on that side swaps with it; one undo step.
#[tauri::command]
pub async fn stacks_move(paths: Vec<String>, up: bool) -> Result<StackResult, CommandError> {
    blocking(move || {
        let (directory, lib, members) = members(&paths)?;
        let mut change = StackChange::default();
        for member in &members {
            if lib.can_move_in_stack(member, up)? {
                change = lib.move_in_stack(member, up)?;
                break;
            }
        }
        Ok(StackResult {
            library: directory.to_string_lossy().into_owned(),
            change,
        })
    })
    .await
}

/// Collapses (`true`), expands (`false`) or toggles (none) the stacks of
/// `paths`.
#[tauri::command]
pub async fn stacks_set_collapsed(
    paths: Vec<String>,
    collapsed: Option<bool>,
) -> Result<StackResult, CommandError> {
    blocking(move || run(paths, |lib, m| lib.set_stacks_collapsed(m, collapsed))).await
}

/// Undoes (`undo`) or redoes a stack command in `library`.
#[tauri::command]
pub async fn stacks_restore(
    library: String,
    change: StackChange,
    undo: bool,
) -> Result<(), CommandError> {
    blocking(move || Ok(open(&library)?.restore_stacks(&change, undo)?)).await
}

/// The photographs Auto-Stack would group among `paths` (copies count as
/// their photograph), and in which library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebAutoPlan {
    pub library: String,
    #[serde(flatten)]
    pub plan: AutoPlan,
}

#[tauri::command]
pub async fn stacks_auto_plan(
    paths: Vec<String>,
    options: AutoStackOptions,
) -> Result<WebAutoPlan, CommandError> {
    blocking(move || {
        let (directory, lib, members) = members(&paths)?;
        let assets: Vec<String> = members
            .into_iter()
            .filter(|m| m.copy.is_empty())
            .map(|m| m.asset_id)
            .collect();
        let plan = lib.auto_stack_plan(&assets, options)?;
        Ok(WebAutoPlan {
            library: directory.to_string_lossy().into_owned(),
            plan,
        })
    })
    .await
}

/// Makes the stacks of a plan's `groups` in `library`: one undo step.
#[tauri::command]
pub async fn stacks_auto_apply(
    library: String,
    groups: Vec<AutoGroup>,
) -> Result<StackResult, CommandError> {
    blocking(move || {
        let change = open(&library)?.auto_stack(&groups)?;
        Ok(StackResult { library, change })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;

    #[test]
    fn a_copy_is_listed_with_its_photographs_path() {
        assert_eq!(
            web_path(Path::new("/s/Capture/A.ARW"), ""),
            "/s/Capture/A.ARW"
        );
        assert_eq!(
            web_path(Path::new("/s/Capture/A.ARW"), "a1b2c3"),
            "/s/Capture/A.ARW?vc=a1b2c3"
        );
    }

    #[test]
    fn photos_outside_a_library_are_not_found() {
        let error = match members(&[]) {
            Err(error) => error,
            Ok(_) => panic!("no path lies in a library"),
        };
        assert_eq!(
            (error.code, error.message.as_str()),
            (crate::ipc::ErrorCode::NotFound, NOT_IN_A_LIBRARY)
        );
    }

    #[test]
    fn a_library_that_closed_fails_as_internal_with_the_text_it_had() {
        let temp = tempfile::tempdir().unwrap();
        let job = temp.path().join("Job");
        Library::create(&job, "Job", Mode::Session).unwrap();
        let opened = open(job.to_str().unwrap()).unwrap();
        assert_eq!(opened.manifest.name, "Job");
        std::fs::remove_file(job.join("session.json")).unwrap();
        let error = match open(job.to_str().unwrap()) {
            Err(error) => error,
            Ok(_) => panic!("the session closed"),
        };
        assert_eq!(error.code, crate::ipc::ErrorCode::Internal);
        assert!(
            error
                .message
                .contains("No Rawtonic session or catalog in this folder"),
            "{error}"
        );
    }
}
