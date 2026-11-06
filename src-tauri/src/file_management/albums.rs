//! Albums (inherited app-wide collections).
//! Part of `file_management`, split by concern (ADR 0009).
use super::ImageFile;
use super::ImageFileMetadata;
use super::assign_group_ids;
use super::enqueue_metadata;
use super::is_cloud_placeholder;
use super::parse_virtual_path;
use super::resolve_image_metadata;
use super::resolve_xmp_path;
use rawtonic_metadata::photo_ref::{self, PhotoRef};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::state::library_files::{AlbumsStore, LibraryFile, LibraryQueue, run_change};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AlbumItem {
    Album {
        id: String,
        name: String,
        icon: Option<String>,
        images: Vec<String>,
    },
    Group {
        id: String,
        name: String,
        icon: Option<String>,
        children: Vec<AlbumItem>,
    },
}

/// Where the album list lives (`albums/albums.json` in the app's data
/// folder), made when it is not there.
pub(crate) fn get_albums_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let albums_dir = data_dir.join("albums");
    if !albums_dir.exists() {
        fs::create_dir_all(&albums_dir).map_err(|e| e.to_string())?;
    }
    Ok(albums_dir.join("albums.json"))
}

pub fn sort_album_tree(items: &mut [AlbumItem]) {
    items.sort_by(|a, b| {
        let get_sort_key = |item: &AlbumItem| match item {
            AlbumItem::Group { name, .. } => (0, name.to_lowercase()),
            AlbumItem::Album { name, .. } => (1, name.to_lowercase()),
        };

        let key_a = get_sort_key(a);
        let key_b = get_sort_key(b);

        key_a.cmp(&key_b)
    });

    for item in items.iter_mut() {
        if let AlbumItem::Group { children, .. } = item {
            sort_album_tree(children);
        }
    }
}

/// The app's album list, read from `albums.json` on the first call of the
/// run (`state::library_files::AlbumsStore`); sorted as the file keeps it.
pub fn get_albums_blocking(app_handle: AppHandle) -> Result<Vec<AlbumItem>, String> {
    app_handle
        .state::<AlbumsStore>()
        .get(&app_handle)
        .map(|items| items.as_ref().clone())
}

/// `get_albums`, off the main thread.
#[tauri::command]
pub async fn get_albums(app_handle: AppHandle) -> Result<Vec<AlbumItem>, String> {
    super::blocking(move || get_albums_blocking(app_handle)).await?
}

/// Replaces the app's album list (sorted as it is saved), in the library's
/// change order.
#[tauri::command]
pub async fn save_albums(
    tree: Vec<AlbumItem>,
    app_handle: AppHandle,
) -> Result<Vec<AlbumItem>, String> {
    super::blocking(move || {
        let queue = app_handle.state::<LibraryQueue>();
        run_change(
            &queue,
            LibraryFile::Albums,
            "Album list could not be saved",
            || save_albums_blocking(tree, &app_handle),
        )
    })
    .await?
}

/// Adds photographs to an album, in the library's change order;
/// the album must still be there.
#[tauri::command]
pub async fn add_to_album(
    album_id: String,
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    super::blocking(move || {
        let queue = app_handle.state::<LibraryQueue>();
        run_change(
            &queue,
            LibraryFile::Albums,
            "Album or preset change could not be saved",
            || add_to_album_blocking(&album_id, &paths, &app_handle),
        )
    })
    .await?
}

/// A single edit, applied under AlbumsStore's write lock to its current tree.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AlbumChange {
    AddToAlbum {
        album_id: String,
        paths: Vec<String>,
    },
    RemovePhotos {
        album_id: String,
        paths: Vec<String>,
    },
    Rename {
        id: String,
        name: String,
    },
    Delete {
        id: String,
    },
    Move {
        id: String,
        parent: Option<String>,
        index: usize,
    },
    Create {
        item: AlbumItem,
        parent: Option<String>,
    },
}

fn item_id(item: &AlbumItem) -> &str {
    match item {
        AlbumItem::Album { id, .. } | AlbumItem::Group { id, .. } => id,
    }
}

fn find_item<'a>(tree: &'a [AlbumItem], id: &str) -> Option<&'a AlbumItem> {
    for item in tree {
        if item_id(item) == id {
            return Some(item);
        }
        if let AlbumItem::Group { children, .. } = item
            && let Some(found) = find_item(children, id)
        {
            return Some(found);
        }
    }
    None
}

fn find_item_mut<'a>(tree: &'a mut [AlbumItem], id: &str) -> Option<&'a mut AlbumItem> {
    for item in tree {
        if item_id(item) == id {
            return Some(item);
        }
        if let AlbumItem::Group { children, .. } = item
            && let Some(found) = find_item_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}

fn child_list<'a>(
    tree: &'a mut Vec<AlbumItem>,
    parent: Option<&str>,
) -> Option<&'a mut Vec<AlbumItem>> {
    match parent {
        None => Some(tree),
        Some(id) => match find_item_mut(tree, id) {
            Some(AlbumItem::Group { children, .. }) => Some(children),
            _ => None,
        },
    }
}

fn extract(tree: &mut Vec<AlbumItem>, id: &str) -> Option<AlbumItem> {
    if let Some(index) = tree.iter().position(|item| item_id(item) == id) {
        return Some(tree.remove(index));
    }
    for item in tree {
        if let AlbumItem::Group { children, .. } = item
            && let Some(found) = extract(children, id)
        {
            return Some(found);
        }
    }
    None
}

pub(crate) fn apply_album_change(
    tree: &mut Vec<AlbumItem>,
    change: &AlbumChange,
) -> Result<bool, String> {
    match change {
        AlbumChange::AddToAlbum { album_id, paths } => {
            if !matches!(find_item(tree, album_id), Some(AlbumItem::Album { .. })) {
                return Err(format!("Album {album_id} is not there any more"));
            }
            Ok(add_paths(tree, album_id, paths))
        }
        AlbumChange::RemovePhotos { album_id, paths } => {
            let Some(AlbumItem::Album { images, .. }) = find_item_mut(tree, album_id) else {
                return Ok(false);
            };
            let before = images.len();
            images.retain(|path| !paths.contains(path));
            Ok(images.len() != before)
        }
        AlbumChange::Rename { id, name } => {
            let Some(item) = find_item_mut(tree, id) else {
                return Ok(false);
            };
            let (AlbumItem::Album { name: current, .. } | AlbumItem::Group { name: current, .. }) =
                item;
            if current == name {
                return Ok(false);
            }
            *current = name.clone();
            Ok(true)
        }
        AlbumChange::Delete { id } => Ok(extract(tree, id).is_some()),
        AlbumChange::Move { id, parent, index } => {
            let Some(item) = find_item(tree, id) else {
                return Ok(false);
            };
            if let Some(parent) = parent {
                if parent == id
                    || matches!(item, AlbumItem::Group { children, .. } if find_item(children, parent).is_some())
                {
                    return Ok(false);
                }
                if !matches!(find_item(tree, parent), Some(AlbumItem::Group { .. })) {
                    return Ok(false);
                }
            }
            let item = extract(tree, id).expect("album found above");
            let list = child_list(tree, parent.as_deref()).expect("parent checked above");
            list.insert((*index).min(list.len()), item);
            Ok(true)
        }
        AlbumChange::Create { item, parent } => {
            if find_item(tree, item_id(item)).is_some() {
                return Ok(false);
            }
            let Some(list) = child_list(tree, parent.as_deref()) else {
                return Ok(false);
            };
            let mut item = item.clone();
            if let AlbumItem::Album { images, .. } = &mut item {
                let mut seen = HashSet::new();
                images.retain(|path| seen.insert(path.clone()));
            }
            list.push(item);
            Ok(true)
        }
    }
}

/// Applies one edit under the store's writer lock; a failed or no-op edit
/// writes nothing.
fn change_albums_blocking(change: AlbumChange, app_handle: &AppHandle) -> Result<(), String> {
    let store = app_handle.state::<AlbumsStore>();
    let mut outcome = Ok(false);
    store.update(app_handle, |stored| {
        outcome = apply_album_change(stored, &change);
        matches!(outcome, Ok(true)).then_some(())
    })?;
    outcome.map(|_| ())
}

#[cfg(test)]
mod followup_regressions {
    use super::*;

    #[test]
    fn adding_to_a_gone_album_reports_the_same_error_as_add_to_album() {
        let change: AlbumChange = serde_json::from_value(serde_json::json!({
            "kind": "addToAlbum", "albumId": "gone", "paths": ["photo.dng"]
        }))
        .unwrap();
        assert_eq!(
            apply_album_change(&mut Vec::new(), &change),
            Err("Album gone is not there any more".into())
        );
    }

    #[test]
    fn moving_a_group_never_drops_it_into_itself_or_a_non_group() {
        let tree: Vec<AlbumItem> = serde_json::from_value(serde_json::json!([
            {"type":"group", "id":"group", "name":"Group", "icon":"folder", "children":[
                {"type":"group", "id":"child", "name":"Child", "children":[]}
            ]},
            {"type":"album", "id":"album", "name":"Album", "images":["photo.dng"]}
        ]))
        .unwrap();
        for parent in ["group", "child", "album", "gone"] {
            let mut current = tree.clone();
            assert!(
                !apply_album_change(
                    &mut current,
                    &AlbumChange::Move {
                        id: "group".into(),
                        parent: Some(parent.into()),
                        index: 0
                    }
                )
                .unwrap()
            );
            assert_eq!(current, tree);
        }
    }

    #[test]
    fn adding_existing_members_is_successful_without_a_write() {
        let mut tree: Vec<AlbumItem> = serde_json::from_value(serde_json::json!([
            {"type":"album", "id":"album", "name":"Album", "images":["photo.dng"]}
        ]))
        .unwrap();
        assert_eq!(
            apply_album_change(
                &mut tree,
                &AlbumChange::AddToAlbum {
                    album_id: "album".into(),
                    paths: vec!["photo.dng".into()]
                }
            ),
            Ok(false)
        );
    }

    #[test]
    fn album_edits_accept_frontend_json_and_keep_file_operation_updates() {
        for change in [
            serde_json::json!({"kind":"rename", "id":"trip", "name":"Renamed"}),
            serde_json::json!({"kind":"delete", "id":"other"}),
            serde_json::json!({"kind":"move", "id":"trip", "parent":"group", "index":0}),
            serde_json::json!({"kind":"removePhotos", "albumId":"trip", "paths":["remove.dng"]}),
        ] {
            let mut tree = vec![
                AlbumItem::Album {
                    id: "trip".into(),
                    name: "Trip".into(),
                    icon: None,
                    images: vec!["renamed.dng".into(), "remove.dng".into()],
                },
                AlbumItem::Album {
                    id: "other".into(),
                    name: "Other".into(),
                    icon: None,
                    images: vec![],
                },
                AlbumItem::Group {
                    id: "group".into(),
                    name: "Group".into(),
                    icon: None,
                    children: vec![],
                },
            ];
            let change: AlbumChange = serde_json::from_value(change).unwrap();
            assert!(apply_album_change(&mut tree, &change).unwrap());
            let trip = serde_json::to_value(&tree).unwrap().to_string();
            assert!(trip.contains("renamed.dng"));
            assert!(!trip.contains("old.dng"));
        }
    }
}

/// Applies `change` to the stored album list (one read-modify-write under the
/// store's lock), in the library's change order, and answers the
/// list the file now holds.
#[tauri::command]
pub async fn library_change(
    change: AlbumChange,
    app_handle: AppHandle,
) -> Result<Vec<AlbumItem>, String> {
    super::blocking(move || {
        let queue = app_handle.state::<LibraryQueue>();
        run_change(
            &queue,
            LibraryFile::Albums,
            "Album list could not be saved",
            || {
                change_albums_blocking(change, &app_handle)?;
                app_handle
                    .state::<AlbumsStore>()
                    .get(&app_handle)
                    .map(|items| items.as_ref().clone())
            },
        )
    })
    .await?
}

/// Replaces the app's album list (sorted as it is saved); nothing is
/// written when it is the list already stored.
fn save_albums_blocking(
    tree: Vec<AlbumItem>,
    app_handle: &AppHandle,
) -> Result<Vec<AlbumItem>, String> {
    app_handle
        .state::<AlbumsStore>()
        .update(app_handle, |stored| {
            if stored == &tree {
                return None;
            }
            *stored = tree;
            Some(())
        })?;
    app_handle
        .state::<AlbumsStore>()
        .get(app_handle)
        .map(|items| items.as_ref().clone())
}

/// Adds photographs to an album; nothing is written when it already holds
/// them all.
pub(crate) fn add_to_album_blocking(
    album_id: &str,
    paths: &[String],
    app_handle: &AppHandle,
) -> Result<(), String> {
    change_albums_blocking(
        AlbumChange::AddToAlbum {
            album_id: album_id.to_owned(),
            paths: paths.to_vec(),
        },
        app_handle,
    )
}

/// Brings the album list in line with a file operation (renames, deletions,
/// a folder rename) in one read-modify-write of the store, so an album change
/// made meanwhile is never written over.
pub(crate) fn sync_album_path_changes(
    app_handle: &AppHandle,
    renames: Option<&HashMap<String, String>>,
    deletions: Option<&HashSet<String>>,
    folder_rename: Option<(&str, &str)>,
) {
    let _ = app_handle
        .state::<AlbumsStore>()
        .update(app_handle, |tree| {
            let mut changed = false;
            process_album_paths(tree, renames, deletions, folder_rename, &mut changed);
            changed.then_some(())
        });
}

pub(crate) fn add_paths(items: &mut [AlbumItem], target_id: &str, paths_to_add: &[String]) -> bool {
    for item in items.iter_mut() {
        #[allow(clippy::collapsible_match)]
        match item {
            AlbumItem::Album { id, images, .. } if id == target_id => {
                let before = images.len();
                for p in paths_to_add {
                    if !images.contains(p) {
                        images.push(p.clone());
                    }
                }
                return images.len() != before;
            }
            AlbumItem::Group { children, .. } => {
                if add_paths(children, target_id, paths_to_add) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn process_album_paths(
    nodes: &mut [AlbumItem],
    renames: Option<&HashMap<String, String>>,
    deletions: Option<&HashSet<String>>,
    folder_rename: Option<(&str, &str)>,
    changed: &mut bool,
) {
    for node in nodes.iter_mut() {
        match node {
            AlbumItem::Album { images, .. } => {
                let mut new_images = Vec::new();

                for img in images.drain(..) {
                    let mut current_img = img;

                    if let Some((old_folder, new_folder)) = folder_rename {
                        let img_path = Path::new(&current_img);
                        let old_path = Path::new(old_folder);
                        if let Ok(stripped) = img_path.strip_prefix(old_path) {
                            let new_img_path = Path::new(new_folder).join(stripped);
                            current_img = new_img_path.to_string_lossy().into_owned();
                            *changed = true;
                        }
                    }

                    if let Some(r) = renames {
                        if let Some(new_path) = r.get(&current_img) {
                            current_img = new_path.clone();
                            *changed = true;
                        } else if photo_ref::is_copy(&current_img)
                            && let Some(new_base) = r.get(photo_ref::source_of(&current_img))
                        {
                            current_img = PhotoRef::parse(&current_img)
                                .with_source(new_base.as_str())
                                .format();
                            *changed = true;
                        }
                    }

                    let mut is_deleted = false;
                    if let Some(d) = deletions {
                        if d.contains(&current_img) {
                            is_deleted = true;
                        } else {
                            let img_path = Path::new(&current_img);
                            for del_path_str in d {
                                let del_path = Path::new(del_path_str);
                                if img_path.starts_with(del_path) {
                                    is_deleted = true;
                                    break;
                                }

                                if photo_ref::is_copy(&current_img)
                                    && photo_ref::source_of(&current_img) == del_path_str
                                {
                                    is_deleted = true;
                                    break;
                                }
                            }
                        }
                    }

                    if !is_deleted {
                        new_images.push(current_img);
                    } else {
                        *changed = true;
                    }
                }
                *images = new_images;
            }
            AlbumItem::Group { children, .. } => {
                process_album_paths(children, renames, deletions, folder_rename, changed);
            }
        }
    }
}

/// Off the main thread: it stats every photograph and reads its sidecar.
#[tauri::command]
pub async fn get_album_images(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    super::blocking(move || album_images(paths, app_handle)).await?
}

fn album_images(paths: Vec<String>, app_handle: AppHandle) -> Result<Vec<ImageFile>, String> {
    let settings = crate::state::settings::current(&app_handle);
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    // Photographs a library holds whose file is offline stay in their
    // albums (and make up the Missing Photographs view).
    // Virtual copies of offline photographs stay too (listed from the
    // library with their photograph).
    let mut gone: Vec<PathBuf> = paths
        .iter()
        .map(|path| parse_virtual_path(path).0)
        .filter(|source| !source.exists())
        .collect();
    gone.sort();
    gone.dedup();
    let offline: HashMap<String, ImageFile> = if gone.is_empty() {
        HashMap::new()
    } else {
        let entries = crate::session_bridge::offline_status(&gone)
            .into_values()
            .collect();
        super::offline_image_files(entries, &settings)
            .into_iter()
            .map(|file| (file.path.clone(), file))
            .collect()
    };

    let mut result_list: Vec<ImageFile> = paths
        .into_par_iter()
        .filter_map(|virtual_path| {
            let (source_path, sidecar_path) = parse_virtual_path(&virtual_path);
            if !source_path.exists() {
                return offline.get(&virtual_path).cloned();
            }

            let modified = fs::metadata(&source_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_virtual_copy = photo_ref::is_copy(&virtual_path);
            let is_cloud_placeholder = is_cloud_placeholder(&source_path);

            let xmp_is_placeholder = enable_xmp_sync
                && resolve_xmp_path(&source_path)
                    .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

            let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                || xmp_is_placeholder
            {
                enqueue_metadata(
                    &app_handle,
                    virtual_path.clone(),
                    source_path.clone(),
                    sidecar_path.clone(),
                );
                ImageFileMetadata {
                    is_edited: false,
                    tags: None,
                    rating: 0,
                    is_raw: crate::formats::is_raw_file(&source_path),
                }
            } else {
                resolve_image_metadata(&source_path, &sidecar_path, enable_xmp_sync, &settings)
            };

            Some(ImageFile {
                path: virtual_path.clone(),
                modified,
                is_edited: metadata.is_edited,
                tags: metadata.tags,
                exif: None,
                is_virtual_copy,
                is_raw: metadata.is_raw,
                group_id: None,
                rating: metadata.rating,
                is_cloud_placeholder,
                is_offline: false,
                offline_volume: None,
            })
        })
        .collect();

    assign_group_ids(&mut result_list, &settings);
    Ok(result_list)
}
