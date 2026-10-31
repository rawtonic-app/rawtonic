//! Stacks (G36): the Tauri commands over `rawtonic_catalog::stacks`. Photos
//! are named by their paths as the webview lists them (a virtual copy as
//! `<path>?vc=<id>`); every command works in the library that manages the
//! first of them and returns the `StackChange` the webview keeps as one
//! undo step (`utils/appUndo`), handed back to `stacks_restore`. No file or
//! sidecar is read or written.
use crate::file_management::parse_virtual_path;
use rawtonic_catalog::Library;
use rawtonic_catalog::stacks::{AutoGroup, AutoPlan, AutoStackOptions, Member, StackChange};
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
    let path = path.to_string_lossy();
    if copy.is_empty() {
        path.into_owned()
    } else {
        format!("{path}?vc={copy}")
    }
}

/// The library of the first of `paths` and the members among `paths` it
/// holds (others, and photos of other libraries, are left out).
fn members(paths: &[String]) -> Result<(PathBuf, Library, Vec<Member>), String> {
    let mut found: Option<(PathBuf, Library)> = None;
    let mut members = Vec::new();
    for path in paths {
        let copy = path
            .rsplit_once("?vc=")
            .map(|(_, id)| id.to_string())
            .unwrap_or_default();
        let (source, _) = parse_virtual_path(path);
        let Some(directory) = crate::session_bridge::library_dir_for(&source) else {
            continue;
        };
        if found.is_none() {
            let lib = Library::open(&directory).map_err(|e| e.to_string())?;
            found = Some((directory.clone(), lib));
        }
        let Some((owner, lib)) = found.as_ref() else {
            continue;
        };
        if *owner != directory {
            continue;
        }
        if let Some(asset) = lib.asset_at(&source).map_err(|e| e.to_string())? {
            members.push(Member {
                asset_id: asset.id,
                copy,
            });
        }
    }
    let (directory, lib) = found.ok_or("These photos are not in a library")?;
    Ok((directory, lib, members))
}

fn run(
    paths: Vec<String>,
    command: impl FnOnce(&Library, &[Member]) -> anyhow::Result<StackChange>,
) -> Result<StackResult, String> {
    let (directory, lib, members) = members(&paths)?;
    let change = command(&lib, &members).map_err(|e| e.to_string())?;
    Ok(StackResult {
        library: directory.to_string_lossy().into_owned(),
        change,
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn open(library: &str) -> Result<Library, String> {
    Library::open(Path::new(library)).map_err(|e| e.to_string())
}

/// Every stack of the library at `directory`.
#[tauri::command]
pub async fn stacks_list(directory: String) -> Result<Vec<WebStack>, String> {
    blocking(move || {
        let lib = open(&directory)?;
        Ok(lib
            .listed_stacks()
            .map_err(|e| e.to_string())?
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
pub async fn stacks_group(paths: Vec<String>) -> Result<StackResult, String> {
    blocking(move || run(paths, |lib, m| lib.group_stack(m))).await
}

#[tauri::command]
pub async fn stacks_unstack(paths: Vec<String>) -> Result<StackResult, String> {
    blocking(move || run(paths, |lib, m| lib.unstack(m))).await
}

#[tauri::command]
pub async fn stacks_remove(paths: Vec<String>) -> Result<StackResult, String> {
    blocking(move || run(paths, |lib, m| lib.remove_from_stack(m))).await
}

#[tauri::command]
pub async fn stacks_move_to_top(path: String) -> Result<StackResult, String> {
    blocking(move || {
        run(vec![path], |lib, m| match m.first() {
            Some(member) => lib.move_to_stack_top(member),
            None => Ok(StackChange::default()),
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
) -> Result<StackResult, String> {
    blocking(move || run(paths, |lib, m| lib.set_stacks_collapsed(m, collapsed))).await
}

/// Undoes (`undo`) or redoes a stack command in `library`.
#[tauri::command]
pub async fn stacks_restore(
    library: String,
    change: StackChange,
    undo: bool,
) -> Result<(), String> {
    blocking(move || {
        open(&library)?
            .restore_stacks(&change, undo)
            .map_err(|e| e.to_string())
    })
    .await
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
) -> Result<WebAutoPlan, String> {
    blocking(move || {
        let (directory, lib, members) = members(&paths)?;
        let assets: Vec<String> = members
            .into_iter()
            .filter(|m| m.copy.is_empty())
            .map(|m| m.asset_id)
            .collect();
        let plan = lib
            .auto_stack_plan(&assets, options)
            .map_err(|e| e.to_string())?;
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
) -> Result<StackResult, String> {
    blocking(move || {
        let change = open(&library)?
            .auto_stack(&groups)
            .map_err(|e| e.to_string())?;
        Ok(StackResult { library, change })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
